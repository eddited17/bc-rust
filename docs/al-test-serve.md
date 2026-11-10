# `al-test --serve`: one process, shared catalogs and companies

Status: MVP landed as opt-in on 2026-11-08; standalone `al-test` stays the default. The MVP covers the spool protocol, the catalog and company caches, one request at a time, per-request tickets with the server's resident memory granted at once, the request's standard output in `done/<id>.log`, and the restart after 50 requests or more than 3 catalogs. Not built yet: concurrent requests with one server ticket, streaming progress to the client, and the restart on abandoned threads or RSS.

```
# server (a systemd user unit, so that no shell timeout kills it)
systemd-run --user --unit=al-test-serve --collect target/release/al-test --serve /path/to/spool
# clients: the usual arguments after --client DIR; output and exit code as a standalone run
target/release/al-test --client /path/to/spool --suite baseapp:Prepayment --jobs 2 --out data/parity/x
```

Identity check (2026-11-08): `--suite baseapp:Prepayment --codeunit "ERM Prepayment II,ERM Prepayment III" --jobs 2` (w1demo, 104 tests). A standalone run and the server's first request had the same outcome for every test, and their messages were identical after masking GUIDs and timestamps (`tools/parity/diff-runs.py`: gained 0, lost 0, message-only 0). The summaries differed only in the company creation time.

## Why

Every queued al-test run loads its own catalog (about 2.5 GB with the test apps) and its own demo company (1 GB from the cache, about 3.5 GB while it is created). With five runs on the box, that is 15–25 GB of identical data. Admission has to reserve all of it, so small runs wait 35–40 minutes. One long-lived process that keeps the catalogs and companies loaded and runs the queued suites in-process shares all of that.

The catalog is plain owned data: Strings, HashMaps and AST trees, built with rayon. Sharing it between processes would mean either zero-copy archived types (rkyv, mmapped), a rewrite of every AST and catalog reader in al-runtime, or a fork zygote, which is Linux-only and doesn't mix with rayon's global pool. Sharing it inside one process needs only a cache, because `run_phase` already runs a suite from an `Opts`.

## Protocol: a spool directory

It works the same on Linux, macOS and Windows, with no sockets.

- `al-test --serve DIR` starts the server. It holds `DIR/server.lock` (an exclusive file lock) while it runs, and writes its pid and start time into it.
- `al-test --client DIR <normal al-test arguments>` parses the arguments locally first (bad arguments fail in the client, never in the server). It then writes `DIR/requests/<unix-nanos>-<pid>.json`: `{ "argv": [...], "cwd": "...", "client_pid": N }`. Request names sort by arrival, so the server takes them in order.
- The server writes the summary and results files into the request's `--out`, as a standalone run would. The request's standard output (the summary, or the `--list` lines) goes to `DIR/done/<id>.log`, and finally the server writes `DIR/done/<id>.json`: `{ "exit": 0, "summary": "<path>", "results": "<path>", "log": "<path>" }`. Progress (admission, catalog, company, slow tests) goes to the server's stderr; the MVP doesn't stream it to the client yet.
- The client prints `done/<id>.log` as its own standard output, deletes both files, and exits with the run's code.
- If the server isn't running (no live `server.lock`), or dies while the client waits, the client runs the request standalone itself. A crashed server never loses a run.

## Caches

- **Catalog:** keyed by the exact list of app directories (default apps, test framework, the selected suites' libraries and test apps, the dataset's demo tool, and the missing dependencies P7 adds). A request with the same key reuses the `&'static Catalog`, so results are identical to a standalone run of the same arguments. A later step could load the full Base App catalog once and filter areas inside it. That changes which test apps are loaded, so it would need its own A/B first, because test apps' automatic subscribers could interact.
- **Demo company (master):** keyed by (catalog, dataset, `--no-install`). It holds the master store, the number sequences and the work date in an `Arc`. Workers get copy-on-write views as today, and the master is never written.
- Catalogs are leaked (the runtime needs `'static`), so they can't be evicted. That is one reason for the restart policy below.

## Running requests

- MVP: one request at a time, in arrival order. That already saves the catalog load (about 2 s, and more importantly the 2.5 GB that every run on the box would otherwise hold) and the company load or build for repeated suites.
- Next: several requests at once. Each runs `run_phase` on its own threads with its own workers. Thread-local state (Random seed, panic message, runner flags) is already per worker thread.

## Admission

- MVP: each request takes its own ticket from the server's pid, as a standalone run does. Since one request runs at a time, the ticket's RSS check (`/proc/<pid>/statm`) sees the server's real usage. What the server already holds (cached catalogs and companies) is resident. It is granted to the request's ticket at once (`Admission::adjust` to the server's RSS, which changes nothing for other runs, because a grant equal to RSS has nothing unused). A cached catalog or company is not reserved again, so a repeated request waits only for its workers. Without this, the first MVP build made the second request wait in the queue for 2.4 GB for a catalog it already had.
- With concurrent requests, per-request tickets from one pid would all see the whole server's RSS and undercount what is unused. Instead, the server keeps one ticket for itself. Its grant is the cached catalogs and companies plus the sum of the running requests' worker reservations, raised (waiting in the queue) before a request starts and lowered with `Admission::adjust` when it ends.

## Crashes and hangs

- A panicking test is already caught per test. A stack overflow or `abort` kills the server and every request in flight. Clients see the dead lock and rerun their request standalone. A supervisor (a systemd user unit, or the next client) restarts the server.
- A hung test is abandoned by the watchdog (`--test-timeout`), but its thread stays in the server, with its CPU and memory. The server counts abandoned threads.
- **Periodic restart:** after N requests (default 50), when abandoned threads exceed K (default 2), when RSS exceeds a limit, or when more than C distinct catalogs are cached (default 3), the server stops taking requests, finishes the running ones and exits. The supervisor starts a fresh one. The MVP implements the request and catalog limits; the abandoned-thread and RSS limits are still to come.

## Results identical to standalone

The test outcomes (results JSONL) must match a standalone run with the same arguments, test for test. Runs are deterministic, so the comparison is exact. The summary header may differ, for example "company loaded from cache" instead of "created in 80 s".
