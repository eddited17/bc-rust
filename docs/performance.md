# Performance measurements

bc-rust against real Business Central 29.1, Microsoft's web client on both service tiers, the Windows build, and posting on PostgreSQL and on the embedded engine. The README's status table quotes the headline rows.

This text moved here unchanged from the README on 2026-11-09. Phrases such as "see below", "Measured performance" and "What doesn't work yet" refer to sections of that README, which are now in [performance.md](performance.md) and [features.md](features.md). Paths under `data/` are local evidence files: `data/` is not in the repository.

## Side by side with real BC 29.1

### Both systems in the same Windows VM (2026-11-10, b41a391)

Real Business Central 29.1.57326.57497 and bc-rust b41a391 ran in the same Windows 11 VM, one system at a time, with identical REST API v2.0 calls. The VM runs on KVM with 12 vCPUs and 20 GiB.

- Real BC is a W1 sandbox container with Hyper-V isolation. Its service tier and SQL Server Express get 2 logical processors and 12 GiB, Docker's defaults.
- bc-rust ran as `bc serve --pg --apps standard-prod`, with PostgreSQL 17.11 as a native Windows process using the live service's settings. Both were pinned to the same 2 logical processors.
- The clients ran on the servers' own machines: inside real BC's container, and in the VM on bc-rust's 2 processors.

bc-rust ran in two configurations:

- defaults: no sign-in and no permission checks;
- `BC_AUTH=NavUserPassword` and `BC_PERMISSIONS=enforce`: Basic credentials checked on every request, and the user's permission sets checked on every record operation.

Each side had a SUPER user and a user with D365 BUS FULL ACCESS. On real BC that user also has a set with Execute on all objects, which BC online grants through the licence. Ratios divide real BC by bc-rust, against the same kind of user. All figures are median / p95 ms.

Small data is each side's starting CRONUS company, with 5 customers. 30 iterations of `tools/sidebyside/api-bench.mjs`:

| Operation | Real BC, SUPER | bc-rust, defaults | bc-rust, checks on, SUPER | Real BC, D365 BUS FULL ACCESS | bc-rust, checks on, D365 BUS FULL ACCESS | Ratio, defaults | Ratio, checks on, D365 BUS FULL ACCESS |
|---|---:|---:|---:|---:|---:|---:|---:|
| list 20 customers | 17.1 / 22.2 | 2.15 / 2.84 | 2.68 / 3.13 | 17.5 / 19.6 | 3.04 / 3.88 | 7.9x | 5.7x |
| filter on the number | 15.7 / 17.8 | 1.02 / 1.52 | 1.63 / 2.45 | 15.6 / 17.4 | 2.00 / 3.01 | 15.4x | 7.8x |
| get by id | 15.5 / 16.5 | 0.97 / 1.41 | 1.57 / 1.97 | 15.3 / 16.7 | 1.94 / 2.58 | 16.0x | 7.9x |
| create | 31.1 / 36.2 | 6.89 / 9.31 | 7.30 / 8.00 | 29.9 / 41.2 | 7.70 / 10.4 | 4.5x | 3.9x |
| update | 53.8 / 63.1 | 30.4 / 39.6 | 12.7 / 14.9 | 58.7 / 66.1 | 17.8 / 21.8 | 1.8x | 3.3x |
| delete | 43.8 / 47.5 | 6.46 / 7.84 | 6.72 / 7.93 | 44.6 / 50.3 | 6.93 / 8.78 | 6.8x | 6.4x |

Big data is 99,467 real company records from the GLEIF golden copy, which is CC0. 13% of the names are non-ASCII. Both sides loaded them through the API. bc-rust rejects 2 of them, because it upper-cases "ß" to "SS" and the Search Name gets too long; real BC keeps "ß". The number filter, the name filter (one match) and the city filter (about 1%) use indexed keys on both sides. 30 iterations:

| Operation | Real BC, SUPER | bc-rust, defaults | bc-rust, checks on, SUPER | Real BC, D365 BUS FULL ACCESS | bc-rust, checks on, D365 BUS FULL ACCESS | Ratio, defaults | Ratio, checks on, D365 BUS FULL ACCESS |
|---|---:|---:|---:|---:|---:|---:|---:|
| list 20 customers | 18.1 / 21.2 | 4.09 / 4.79 | 4.90 / 6.77 | 18.6 / 21.2 | 5.14 / 5.36 | 4.4x | 3.6x |
| filter on the number | 16.8 / 21.6 | 0.91 / 1.29 | 1.58 / 2.56 | 16.7 / 19.2 | 1.95 / 2.47 | 18.4x | 8.5x |
| filter on the name | 16.9 / 18.8 | 0.85 / 1.58 | 1.47 / 2.13 | 16.6 / 18.5 | 1.83 / 2.51 | 19.8x | 9.1x |
| filter on a city | 55.4 / 60.2 | 93.7 / 107 | 95.9 / 113 | 55.3 / 66.1 | 94.6 / 101 | 0.6x | 0.6x |
| get by id | 18.5 / 29.2 | 3.12 / 7.78 | 4.40 / 5.73 | 18.1 / 26.0 | 4.82 / 6.72 | 5.9x | 3.7x |
| create | 32.8 / 40.9 | 8.68 / 16.2 | 9.18 / 17.3 | 32.8 / 40.8 | 9.59 / 11.7 | 3.8x | 3.4x |
| update | 58.8 / 69.5 | 12.9 / 14.1 | 14.7 / 18.5 | 64.4 / 74.8 | 19.1 / 21.1 | 4.6x | 3.4x |
| delete | 44.8 / 53.8 | 7.48 / 8.68 | 8.91 / 10.6 | 46.1 / 54.8 | 9.31 / 9.81 | 6.0x | 5.0x |

The city filter returns 813 rows on real BC, whose database collation is case-sensitive, and 960 on bc-rust, which compares case-insensitively. Real BC is faster on that query.

Posting a sales order on the big data means creating the order, adding the lines and calling the bound action `Microsoft.NAV.shipAndInvoice`, the same on both sides. Total time over 20 iterations after warm-up, from `tools/sidebyside/post-bench.mjs`:

| | Real BC, SUPER | bc-rust, defaults | bc-rust, checks on, SUPER | Real BC, D365 BUS FULL ACCESS | bc-rust, checks on, D365 BUS FULL ACCESS | Ratio, defaults | Ratio, checks on, D365 BUS FULL ACCESS |
|---|---:|---:|---:|---:|---:|---:|---:|
| 1 line | 227 / 304 | 86.5 / 96.4 | 90.1 / 106 | 246 / 265 | 93.8 / 141 | 2.6x | 2.6x |
| 10 lines | 848 / 987 | 345 / 657 | 357 / 682 | 909 / 1,110 | 357 / 427 | 2.5x | 2.5x |

For write throughput, 7 users with 5 requests in flight each created customers for 2 minutes. BC's licence in the container allows 10 users, so 20 users weren't possible.

- Real BC: 82.9 per second, p50 406 ms.
- bc-rust, defaults: 184 per second.
- bc-rust, checks on: 164 per second.

During the load of the 99,467 records, real BC's first attempt failed on SQL deadlocks on the Contact table. The second attempt saw none.

Caveats:

- Real BC's container is a Hyper-V utility VM inside the Windows VM. That is one more virtualization layer than bc-rust has. bc-rust's PostgreSQL ran as a Windows process, not in a container.
- The clients ran on the servers' own machines, because responses above about 1.3 KB stalled on the way out of BC's container.
- bc-rust doesn't check object Execute permissions; real BC does.
- The starting companies differ: real BC has the legacy W1 CRONUS, bc-rust its Contoso CRONUS.
- Each block ran once. Between two runs minutes apart, real BC's update and delete medians moved by up to 50%.
- The live bc-rust units on the host stayed up and used about 10% of one core during the blocks.

Raw data, setup details and incidents: data/capture/sidebyside-2026-11-10/notes.md.

### bc-rust on the Linux host (2026-11-08, fc41a44)

This earlier measurement favoured bc-rust. bc-rust ran on the Linux host with all its cores and with sign-in and permission checks off, while real BC ran in the VM. The measurement above replaces it as the comparison.

Real Business Central 29.1.57326.57497 (W1 sandbox container in a Windows VM on the same host) against bc-rust on PostgreSQL (`bc serve --pg --apps standard-prod`, the build of 2026-11-08 12:45, a scratch database over the Unix socket), measured on 2026-11-08 with identical REST API v2.0 calls. Median ms.

Posting a sales order: create the order, add the lines, post. Real BC posts with the bound action `Microsoft.NAV.shipAndInvoice`. The measured bc-rust build had no bound actions yet, so it posted through the Sales Order page's Post action, which runs the same posting codeunits. 20 iterations after warm-up (`tools/sidebyside/post-bench.mjs`):

| | Real BC 29.1, 1 line | bc-rust, 1 line | Real BC 29.1, 10 lines | bc-rust, 10 lines |
|---|---:|---:|---:|---:|
| create the order | 35.2 | 6.3 | 32.3 | 8.5 |
| add the lines | 46.7 | 13.2 | 417.2 | 170.1 |
| post | 131.7 | 29.2 | 431.5 | 104.1 |
| **total** | **212.1** | **47.8** | **870.3** | **281.5** |

Customers through the API (`tools/sidebyside/api-bench.mjs`, 30 iterations, median / p95 ms):

| Operation | Real BC 29.1 | bc-rust |
|---|---:|---:|
| list 20 customers | 24.9 / 34.0 | 1.34 / 2.28 |
| filter | 23.7 / 27.9 | 0.83 / 1.57 |
| get by id | 23.2 / 29.4 | 0.59 / 0.95 |
| create | 40.0 / 51.1 | 1.79 / 3.15 |
| update | 51.8 / 110.8 | 1.36 / 2.12 |
| delete | 44.9 / 79.6 | 2.21 / 6.85 |

| | Real BC 29.1 | bc-rust |
|---|---|---|
| Memory | 3.75 GiB idle, 3.82 to 3.90 GiB while posting (the container: service tier and SQL Server) | 1.89 GB idle and while posting, plus about 0.7 GB for the PostgreSQL container, which also serves the live databases |
| Cold start to the first API answer | about 14 s (measured on 2026-11-07) | 1.60 to 1.75 s |

Caveats: real BC runs in a VM with Hyper-V nested in KVM, which costs it time that bc-rust doesn't pay. The host was busy during the runs (load average 3 to 8 from other test runs). Real BC wasn't restarted on 2026-11-08, so its cold start is the figure from 2026-11-07. Raw data and the tools are in data/capture/sidebyside-2026-11-08/notes.md.

## Microsoft's web client

Microsoft's BC 29.1 web server on Linux and headless Chromium, on the published stack (web server on :8193, bc-nst on :7185). Median ms of 5 warm runs (`tools/capture/capture.mjs`). The right column uses the same Linux-hosted web server against the real BC 29.1 service tier:

| Step | on bc-nst | on the real service tier |
|---|---|---|
| sign in + role center | 910 | 1,296 |
| open page 22 (Customers) | 548 | 681 |
| open page 21 (Customer Card) | 593 | 640 |
| open page 42 (Sales Order) | 702 | 831 |

Most of this is browser and web server time. bc-nst answers a metadata request in about 2 ms. The demo data differs between the two sides. Details are in data/capture/bc-nst-webclient/e5/notes.md.

## Windows

Same source on a 24-core Linux host and a Windows 11 VM with 12 vCPUs. The VM runs Hyper-V nested in KVM with Defender on. Builds were limited to 12 jobs on both:

| | Linux | Windows VM |
|---|---|---|
| Clean `cargo build --release` | 47 s | 201 s |
| Sales posting, embedded engine | 17.3 ms | 17.1 ms |
| Order entry, embedded engine | 7.2 ms | 6.6 ms |
| Creating the Contoso company | 102.8 s | 120.2 s |

The interpreter runs at the same speed on both. The slower build is most likely nested virtualization and Defender scanning `target\`. See docs/platforms.md. Platform check of the build of 2026-11-08 03:15, on a fresh CRLF copy in the VM with a new target directory: the clean release build took 207 s, and all 130 tests of bc-server, bc-store, al-model, al-runtime and bc-nst pass, the same as on Linux. The new compiled engine and the BENCH timing code, which uses Unix-only libc calls, build there too, and mimalloc 2.3.2 is the allocator of `bc` and `al-test` on Windows as on Linux. `tools/macos-check.sh` type-checks both macOS targets.

## Posting

Sales order entry and posting (`POST=12 bc sales --pg`, single-line orders, steady state after the first order):

| Storage | Order entry (header, customer, line) | Posting through codeunit 80 |
|---|---|---|
| PostgreSQL 17 (Docker, TCP), caches and prepared-statement reuse | ~10 ms | ~22 ms |
| Embedded engine | 6.5 ms | 12.7 ms |

A single-line posting issues 107 SQL statements (399 at first, 184 with the per-session `Get` cache). With one session per request, as the web API runs, posting dropped from 65 to 31 ms. The remaining single-session time is mostly AL interpretation (about 12 ms), so compiling AL instead of walking the AST is the next big step. The compiled engine, the default since 2026-11-08, resolves variables, calls, record fields and loops once per procedure instead of on every execution. On the posting loop it needs 7–8% less CPU per AL statement than the tree-walker and runs the same statements. `BC_ENGINE=tree` selects the tree-walker (docs/engine.md). Since the build of 2026-11-08 09:30, record variables share their rows copy-on-write (copies, xRec, the prefetch buffer), and the compiled engine reads variables by slot. On the in-memory posting loop (`tools/bench/cpu-ab.py`, CPU per AL statement, paired rounds against the 08:00 build, the same statements) that is 25.5% less CPU per statement on the compiled engine (90% interval 25.1% to 25.8%; copy-on-write + slot access), 20.6% less on the tree-walker (20.4% to 21.1%; copy-on-write alone), and 27.8% less with the standard app set (27.2% to 28.0%). The live server posts a single-line sales order through pages 42/46 in a warm median of 26.8 ms (orders 2 to 10 of 10, 14,332 statements each, measured in a quiet window at load average 2.4; the 08:00 build took 44 to 46 ms at load 17 to 18, which is not a paired comparison). The build of 12:45 adds three things (docs/engine.md, rounds 17 and 18): compiled call arguments read their variable slots directly, the instances of a session share label slots, and there are fewer string compares. Paired against the 09:30 build on the compiled engine, that is 16.8% less CPU per statement with the default apps (90% interval 13.3% to 20.8%) and 9.1% less with the standard app set (5.0% to 15.2%). These pairs ran on a busy host with no quiet window (load average 17 to 27). The live server's warm median was 37.5 ms at load average 26, with 14,337 statements per posting. The build of 16:30 plans PostgreSQL statements once per connection (generic plans) and turns the locking contract on. Paired against the 12:45 build on the compiled engine, the in-memory posting loop needs 3.7% more CPU per statement with the default apps (90% interval 0.3% less to 6.9% more, so not significant) and 2.5% more with the standard app set (0.6% to 3.9% more), running the same statements (load average 18 to 34). Served posting on PostgreSQL (a scratch database, standard app set, 8 interleaved rounds of 20 postings per build, each build started fresh): the first posts after a start (posts 2 to 6) take 24% less time than on the 12:45 build (90% interval 6% to 29%), because PostgreSQL no longer plans each statement anew on its first five runs per connection. The same build with `BC_PG_PLAN_CACHE=auto` loses that gain. Later posts and the overall median don't differ measurably on this busy host (load average about 24), and turning the locking contract off made no measurable difference either. The live server's warm median is now 27.0 ms at load average 18, with the same 14,337 statements. The build of 19:30 needs 6.3% less CPU per statement than the 16:30 build on the in-memory posting loop with the default apps (90% interval 5.8% to 7.0%) and 6.4% less with the standard app set (6.0% to 7.0%), running the same statements (load average 3 to 8). The likely sources, not measured one by one: a background session's stop check now rides on the step limit instead of running per statement, and a nested Validate copies the record only when an error trap is open. Served posting on PostgreSQL on a quiet host (6 interleaved rounds of 20 postings per build, scratch database) takes 23.4 ms against 23.7 ms for the 16:30 build (-1.0%, 90% interval -2.0% to +0.5%). With `BC_PG_PLAN_CACHE=auto` posts 2 to 6 after a start take 35% longer, and turning the locking contract off saves about 1% (-1.9% against -1.0%). The live server's warm median is 19.5 ms with the background pool on and 20.5 ms with it off (load average 2.6, 14,337 statements).

Embedded engine micro-benchmarks, `bc bench` (WAL with fdatasync per commit):

| Operation | Result |
|---|---|
| Customer `Insert(true)` + `Validate(Name)` + `Modify(true)` + commit with fdatasync, through Base App triggers | 1,743/s, 574 µs each, 250 AL statements each |
| Same, without fdatasync (`--nosync`, interpreter cost only) | 7,076/s, 141 µs each, about 0.56 µs per AL statement |
| Bulk ledger insert, one transaction | 128,000 rows/s |
| `CalcFields("Balance (LCY)")`, a FlowField summing 500k ledger entries | 47 µs per customer |
| `Get` by primary key | 1.9 µs |
| Restart: replay 540k WAL records | 3.3 s |

## The live services' database connection

On this machine the server runs as the systemd user unit `bc-rust.service` (127.0.0.1:8740, PostgreSQL database `bc`, `--apps standard-prod`) behind the rockit-proxy route `bc`. Microsoft's web client runs on `bc-nst.service` (127.0.0.1:7185, database `bc_nst`, `--apps standard-prod`) and `msweb-bcnst-8193.service` (`tools/msweb-linux/run.sh` on 127.0.0.1:8193), published as https://bc-msclient.rockitpro.ai/BC/. Both units reach PostgreSQL over its Unix socket instead of TCP since 2026-11-08 (docs/deploy-db.md), which cut the posting median from 39 to 32 ms.
