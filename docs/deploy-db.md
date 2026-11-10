# Database address for the live services (applied 2026-11-08)

Status: **applied on 2026-11-08 at 05:14 UTC** (the proposal was P12, 2026-11-08). bc-rust.service and bc-nst.service now connect through the Unix socket `~/.local/state/bc-rust-pg/sock` (`~/.config/bc-rust/pg.env` plus a `db.conf` drop-in per unit), and the port 127.0.0.1:5442 is still published. Downtime was about 4 s. Live posting median went from 39.2 to 31.6 ms. Evidence: data/capture/redeploy-2026-11-08-socket/notes.md. The kit with dumps, switch.sh and rollback.sh is kept outside the repository.

Differences from the plan below:
- The old container also had `POSTGRES_PASSWORD` set, and the new one gets it too (from the environment, not on the command line).
- The backup was `pg_dump -Fc` of `bc` and `bc_nst` plus `pg_dumpall --globals-only`, not a full `pg_dumpall`; the other databases are scratch databases.
- Rollback doesn't delete anything: kit `rollback.sh address|container` moves the drop-ins away, and in `container` mode renames the new container `bc-rust-postgres-socket` instead of running `docker rm`.
- `bc-rust-postgres-old` (stopped, restart=no) is still there, for the cleanup in step 6.

The "Today" section below describes the setup before the change.

## Today

| | |
|---|---|
| container | `bc-rust-postgres`, image `postgres:17.11-alpine`, restart `unless-stopped`, default `bridge` network |
| command | `-c shared_buffers=2GB -c synchronous_commit=on -c max_connections=200 -c work_mem=32MB` |
| environment | `POSTGRES_USER=bc`, `POSTGRES_DB=bc`, `POSTGRES_INITDB_ARGS=--locale-provider=icu --icu-locale=und` |
| data | named volume `bc-rust-pg` → `/var/lib/postgresql/data` |
| address | published port `127.0.0.1:5442 → 5432`, which goes through docker-proxy |
| clients | `bc-rust.service` (`bc serve --pg`: `BC_PG`, else `host=127.0.0.1 port=5442 dbname=bc` with data/pg/password), `bc-nst.service` (`BC_NST_PG`, else `dbname=bc_nst`), al-test, tools and other agents (scratch databases via 127.0.0.1:5442) |

A sales-order post through pages 42/46 makes about 181 round trips (data/capture/standard-apps-trial-2026-11-07/round6). Each one passes docker-proxy, a userland TCP relay, twice.

## Measured (scratch container, served path)

Setup: container `bc-p12-pg`, same image and settings except shared_buffers=1GB. It was reachable four ways at once: published port 127.0.0.1:5443 (docker-proxy), fixed IP 172.31.250.10 on a user-defined network, its IP on the default bridge, and its socket directory bind-mounted to the host. Four `bc serve --pg --apps standard-prod` servers ran from the same binary, each on its own database in that container. Each was warmed up with 8 posts, then measured over 24 rounds in random order per block (verify-bc.py: create order, add line, post). Steps were 13,267 per post on every path.

| path | post median, block 1 | block 2 | faster than proxy |
|---|---:|---:|---|
| docker-proxy (127.0.0.1:port) | 36.9 ms | 38.0 ms | |
| fixed IP, user-defined network | 33.6 ms | 28.8 ms | 19/24, 23/24 |
| container IP on the default bridge | 29.6 ms | 31.6 ms | 22/24, 17/24 |
| Unix socket (bind-mounted directory) | **27.2 ms** | **25.9 ms** | 20/24, 21/24 |

Header create and line updates moved the same way: proxy 6.0 / 6.8 ms, socket 4.9 / 5.6 ms in block 1.

## Proposal: Unix socket, port kept

Bind-mount the container's socket directory to the host and point the two units at it. The published port stays, so al-test, tools, other agents and `docker exec ... psql` keep working unchanged, and TCP is the fallback.

- Socket directory: `~/.local/state/bc-rust-pg/sock`, on the root file system (sockets need a Linux file system; the NTFS volume may not support them). The image's entrypoint runs as root and resets the mounted directory to owner uid 70 (postgres), mode 03775, whatever the host set, and the socket file itself is 0777. So the protection on the host has to come from the parent directories: the home directory is 0750, so other host users can't reach the socket. A world-traversable place like /tmp would expose it. Tested on the scratch container: with /tmp, another uid could connect.
- Authentication: the image's `pg_hba.conf` trusts all local (socket) connections without a password (`local all all trust`). Keep it. `docker exec bc-rust-postgres psql -U bc ...`, which the redeploy scripts, notes and other agents use, connects through the same socket inside the container and would otherwise need a password. Through the bind mount only users who can traverse the service user's home directory reach the socket, and the one user who can (the service user) already has docker access, which is root-equivalent. If other host users ever join that user's group, switch to `local all all scram-sha-256`. Tested on the scratch container: the connection is then refused without a password ("fe_sendauth: no password supplied") and accepted with it, and `bc serve` runs over the socket. That breaks `docker exec ... psql` without `-h 127.0.0.1`/password, so update those scripts first.
- The connection strings carry the password anyway, so they keep working if `scram-sha-256` is turned on later.
- Connection strings (in a 0600 environment file, not in the unit file, because they hold the password): `~/.config/bc-rust/pg.env`

  ```
  BC_PG=host=/home/<user>/.local/state/bc-rust-pg/sock port=5432 user=bc dbname=bc password=<data/pg/password>
  BC_NST_PG=host=/home/<user>/.local/state/bc-rust-pg/sock port=5432 user=bc dbname=bc_nst password=<data/pg/password>
  ```

  `port=5432` names the socket file (`.s.PGSQL.5432`); the host has no TCP listener on 5432.
- Units: a drop-in per unit, `~/.config/systemd/user/bc-rust.service.d/db.conf` and `.../bc-nst.service.d/db.conf`, with `EnvironmentFile=%h/.config/bc-rust/pg.env`. ExecStart lines stay as they are (including `--apps standard-prod` after I5).

### Migration (downtime about 1 minute)

1. **Prepare (no downtime).** `mkdir -p ~/.local/state/bc-rust-pg/sock && chmod 0700 ~/.local/state/bc-rust-pg`, write `pg.env` (mode 0600) and the two drop-ins (don't daemon-reload yet). Back up: `docker exec bc-rust-postgres pg_dumpall -U bc > <backup dir>/pg-before-socket-<time>.sql` (all databases; the volume itself is not touched).
2. **Stop the services:** `systemctl --user stop bc-rust.service bc-nst.service`. msweb-bcnst-8193 can keep running; it reconnects.
3. **Recreate the container with the extra mount, keeping the old one stopped for rollback.** Never start both containers: they share the volume.

   ```
   docker stop bc-rust-postgres
   docker rename bc-rust-postgres bc-rust-postgres-old
   docker update --restart=no bc-rust-postgres-old
   docker run -d --name bc-rust-postgres --restart unless-stopped \
     -p 127.0.0.1:5442:5432 \
     -v bc-rust-pg:/var/lib/postgresql/data \
     -v ~/.local/state/bc-rust-pg/sock:/var/run/postgresql \
     -e POSTGRES_USER=bc -e POSTGRES_DB=bc -e "POSTGRES_INITDB_ARGS=--locale-provider=icu --icu-locale=und" \
     postgres:17.11-alpine -c shared_buffers=2GB -c synchronous_commit=on -c max_connections=200 -c work_mem=32MB
   ```

   The volume is already initialised, so the image's entrypoint skips initdb and needs no POSTGRES_PASSWORD.
4. **Checks.** `docker exec bc-rust-postgres pg_isready -U bc`, the socket file `.s.PGSQL.5432` exists in the host directory, and a socket connection as the host user works (the host has no psql: `docker run --rm --user 1000:1000 -v ~/.local/state/bc-rust-pg/sock:/s postgres:17.11-alpine psql "host=/s port=5432 user=bc dbname=bc" -Atc 'select 1'`).
5. **Start the services:** `systemctl --user daemon-reload`, `systemctl --user start bc-rust.service`, wait for "listening", then `systemctl --user start bc-nst.service` and `systemctl --user restart msweb-bcnst-8193.service`. Check that `/api/stats` reports `database: PostgreSQL (host=/home/<user>/.local/state/bc-rust-pg/sock ...)`, then run verify-bc.py (create and post an order) and sign in through https://bc-msclient.rockitpro.ai/BC/.
6. After a day without problems, remove the old container: `docker rm bc-rust-postgres-old`.

### Rollback

- **Only the address:** delete the two drop-ins, `daemon-reload`, restart the units. They use 127.0.0.1:5442 again, which the new container still publishes. Seconds of downtime.
- **The container as well:** stop the units, `docker stop bc-rust-postgres && docker rm bc-rust-postgres && docker rename bc-rust-postgres-old bc-rust-postgres && docker update --restart=unless-stopped bc-rust-postgres && docker start bc-rust-postgres`, delete the drop-ins, `daemon-reload`, start the units. The data is the same volume throughout.
- The `pg_dumpall` from step 1 is only for the unexpected case that the volume is damaged.

## Alternatives

- **Fixed IP on a user-defined network** (`docker network create --subnet 172.31.250.0/24 bc-net`, `docker run --network bc-net --ip 172.31.250.10 ...`): 9–24% faster than the proxy, no authentication change, also usable from other machines' tooling if routed. It needs the same container recreation; the address must not collide with other Docker networks.
- **Container IP looked up at unit start** (`ExecStartPre` writing `BC_PG=host=$(docker inspect ...)`): no container change, 17–20% faster. But the IP on the default bridge can change whenever the container restarts (the IPs are reassigned in start order), and the units would then lose the database until their next restart. Not recommended.
- **Leave as is:** docker-proxy costs roughly 9–12 ms of a 26–38 ms post on this host.

Windows and macOS deployments are unaffected: they keep TCP connection strings (Unix sockets are a Linux host option here).

## Plan cache: generic plans (2026-11-08, P12)

Session connections run `SET plan_cache_mode = force_generic_plan` when they are opened (`connect_client` in crates/bc-store/src/pg.rs). With PostgreSQL's default `auto`, a prepared statement gets a new custom plan on each of its first five executions per connection, and the sixth builds the generic plan that is reused from then on. Planning a statement on a wide BC table costs 0.2–0.4 ms (Sales Line has 223 columns and 17 indexes), so the first executions of every statement on every pooled connection were 3–6 times slower than later ones. A server keeps up to 64 pooled connections, each with its own prepared statements, and standard-prod issues thousands of distinct statement texts, so the live services paid this after every restart, on every new connection and for every rarely run statement.

Served posting on a scratch database (bc_bench_p12, standard-prod, Unix socket; 8 interleaved rounds per A/B, medians of per-round ratios, 90% bootstrap intervals; data/capture/standard-apps-trial-2026-11-07/round19/notes.md):

| | posts 2–6 after a start | posts 7 and later | all timed posts |
|---|---|---|---|
| generic plans (connection option) vs `auto`, same binary | 43.8 → 29.2 ms, −29% (−35..−27) | 30.2 → 27.1 ms, −7% (−16..−5) | −10% (−24..−5) |
| generic plans (set in code, `connect_client`) vs the 12:52 build | 42.9 → 29.0 ms, −29% (−35..−25) | 29.5 → 27.4 ms, −3% (−11..+6) | −13% (−15..−3) |

Both measured binaries also had a SQL-text cache that is not landed (it measured −7% and +1% on its own, not significant).

A generic plan is built from average statistics instead of the first parameter values. For these statements (equality on key prefixes, ORDER BY a key, LIMIT) it is the plan `auto` settles on anyway. Like SQL Server's plan reuse for BC's parameterized statements, it doesn't replan per execution. A statement on a very skewed column could get a worse plan than a custom plan would give. `BC_PG_PLAN_CACHE=auto` restores PostgreSQL's default; `force_custom_plan` is accepted too.

## Locking contract and lock timeout (2026-11-08, I12)

Every PostgreSQL backend (bc serve, bc-nst, the bc tools) now runs with the locking contract of docs/job-queue.md: a locked FindLast takes an advisory lock on the end of its key range, `ReadIsolation(UpdLock)` reads lock their rows, and AutoIncrement comes from a sequence. Without it, two API sessions posting at the same time got duplicate Item Ledger Entry and posted document numbers (about 30% of postings failed with two clients). `BC_PG_TABLE_LOCKS=0` in the unit's environment turns it off.

Session connections also run `SET lock_timeout` (BC's default, 10 s; `BC_PG_LOCK_TIMEOUT_MS`, 0 waits without limit). A statement that waits longer for a lock fails with a lock error naming the table, and a deadlock with a deadlock error naming the table (texts: docs/sources.md row X22)

Cost on one client (scratch databases, 2026-11-08, `bc sales` POST=30, in process): posting runs 140 SQL statements instead of 138 (+2: range-end locks) and order entry 65 instead of 64, with identical AL work (19,749 statements). That is about 0.05 ms per posting. Wall times on the shared host (load 27 to 121 during the runs) moved within the noise in both directions: served single-client posting medians, contract on against off, were +1.5% and +5% in the two pairs measured under the same load. With 2 and 4 concurrent clients, posting went from about 30% failures to none, at 28 and 41 postings/s. Functional check: an order created while psql held the S-ORD No. Series Line row failed after 10.01 s with the lock timeout error on table 'No. Series Line', and the server went on posting.

Get and Find('=') now raise such database errors as AL errors. Before, they returned "not found", while PostgreSQL had already aborted the transaction. bc-server's PostgreSQL tests (`BC_PG_TEST`) and flows pass with the contract on.

## Shared memory: `--shm-size=1g` (2026-11-10)

Status: **applied on 2026-11-10 at 01:04 UTC**; the units were down for 17 s.

Docker gives a container a 64 MB `/dev/shm` unless told otherwise, and PostgreSQL (`dynamic_shared_memory_type = posix`) puts its dynamic shared memory there: parallel query and index builds, and the growing part of the cumulative statistics area. On 2026-11-09 at 20:17 UTC a parallel index build during a scratch `pg_restore` failed with "could not resize shared memory segment ... No space left on device", a backend segfaulted, and every database, the live ones included, was down for 39 s. Since 20:42 that day `max_parallel_maintenance_workers = 0` (in the volume's `postgresql.auto.conf`) has kept index builds serial. That setting stays. Before the change, `/dev/shm` still held 61 of 64 MB: ten segments of 1 to 16 MB, sizes that double the way a DSA area grows, with 93 databases on the server. That points at the statistics area, but the cause was not verified. The next segment would have been 32 MB.

The change recreated the container with the same image (`postgres:17.11-alpine`, same image id), command, environment, volume, socket bind mount, port and restart policy, plus `--shm-size=1g`. `docker inspect` before and after differs only in ShmSize, Hostname and Created. Afterwards `/dev/shm` is 1.0 GB, with 9 MB used after the restart.

```
systemctl --user stop msweb-bcnst-8193 bc-nst bc-rust
docker stop -t 120 bc-rust-postgres
docker rename bc-rust-postgres bc-rust-postgres-old-shm64
docker update --restart=no bc-rust-postgres-old-shm64
export POSTGRES_PASSWORD=...   # copied from the old container's environment, never on the command line
docker run -d --name bc-rust-postgres --shm-size=1g --restart unless-stopped \
  -p 127.0.0.1:5442:5432 \
  -v bc-rust-pg:/var/lib/postgresql/data \
  -v ~/.local/state/bc-rust-pg/sock:/var/run/postgresql \
  -e POSTGRES_DB=bc -e POSTGRES_USER=bc -e POSTGRES_PASSWORD \
  -e "POSTGRES_INITDB_ARGS=--locale-provider=icu --icu-locale=und" \
  postgres:17.11-alpine -c shared_buffers=2GB -c synchronous_commit=on -c max_connections=200 -c work_mem=32MB
# checks: df -h /dev/shm = 1.0G, pg_isready, bc and bc_nst answer, socket and 127.0.0.1:5442 open
systemctl --user start bc-rust   # then bc-nst, then msweb-bcnst-8193, each after the previous one answers
```

Rollback: stop the units, stop the new container and rename it `bc-rust-postgres-new-shm1g` (restart=no), rename `bc-rust-postgres-old-shm64` back to `bc-rust-postgres`, set its restart policy to unless-stopped, start it, then start the units in order. Both containers use the same volume, so never run both at once. The kit (`phaseC.sh`, `rollbackC.sh`, log `PHASEC.log`) is kept outside the repository. `bc-rust-postgres-old-shm64` and the older `bc-rust-postgres-old` stay stopped until they are removed by hand.

A second issue showed up while preparing this, and it is not fixed: after the 20:17 restart, bc-nst's connection pool still handed out connections to the dead server once each. The first web-client sign-in afterwards (23:56 UTC) failed with "Could not open the company ... database: error communicating with the server". Restarting the units clears the dead connections; the pool should check or recycle a connection after the server restarts.
