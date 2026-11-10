# Platforms

bc-rust has to run on Linux, Windows and macOS. This page records how to build it on each one, what was tested and what is still open. Status as of 2026-11-07.

| | Linux x86_64 | Windows 11 x86_64 (MSVC) | macOS (x86_64, arm64) |
|---|---|---|---|
| Builds | yes | yes | type-checks, C code compiles; not linked |
| Workspace tests (95) | pass | pass, LF and CRLF checkouts | not run |
| `bc serve`, `bc-nst`, `al-test` | run | run | not run |
| PostgreSQL backend | tested | tested (host PostgreSQL over the VM network) | not run |

## Linux

```sh
cargo build --release
cargo test --release -p al-syntax -p al-model -p bc-store -p al-runtime -p bc-server -p bc-nst
```

You need Rust (1.94.0 here, `File::try_lock` needs 1.89 or later) and a C compiler for ring, the crypto library under rustls. gcc or clang both work. Nothing else: no OpenSSL, no libpq.

## Windows

Tested on a Windows 11 (build 26200) VM with 12 vCPUs and 20 GB, the `bc-sidebyside` VM that also hosts the real BC container. That VM runs Hyper-V nested inside KVM, with Defender real-time protection on.

### Toolchain

Use the MSVC toolchain. ring compiles C in its build script, so the GNU toolchain would also need a full mingw-w64 gcc or clang (MSYS2). Only MSVC was tested.

1. Visual Studio 2022 Build Tools with the C++ workload. This installs unattended in about 4 minutes and needs no reboot:

   ```powershell
   curl.exe -sSfL -o vs_BuildTools.exe https://aka.ms/vs/17/release/vs_BuildTools.exe
   .\vs_BuildTools.exe --quiet --wait --norestart --nocache --installPath C:\dev\BuildTools `
     --add Microsoft.VisualStudio.Workload.VCTools `
     --add Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
     --add Microsoft.VisualStudio.Component.Windows11SDK.26100
   ```

   That gave MSVC 14.44.35207 and Windows SDK 10.0.26100. rustc and the `cc` crate find it through the Visual Studio installer, so no developer prompt or `vcvars` is needed.

2. rustup, here kept under `C:\dev\rust` with the PATH left alone:

   ```powershell
   $env:RUSTUP_HOME = 'C:\dev\rust\rustup'; $env:CARGO_HOME = 'C:\dev\rust\cargo'
   .\rustup-init.exe -y --no-modify-path --profile minimal --default-toolchain 1.94.0 --default-host x86_64-pc-windows-msvc
   $env:Path = 'C:\dev\rust\cargo\bin;' + $env:Path
   ```

3. Build and test from the repository root exactly as on Linux. Run the binaries from the repository root too (`.\target\release\bc.exe serve`), since they load `platform\` and `vendor\bcapps\` relative to the current directory.

The release binaries depend on `VCRUNTIME140.dll` and the Universal CRT (`api-ms-win-crt-*`). A machine without Visual Studio needs the VC++ 2015-2022 redistributable. Building with `RUSTFLAGS="-C target-feature=+crt-static"` should give self-contained executables, but I have not tried it.

### Checkout

- **Line endings.** BCApps' `.gitattributes` is `* text=auto`, and Git for Windows defaults to `core.autocrlf=true`, so a Windows checkout has CRLF AL files. bc-rust's own repository has no `.gitattributes`, so its files arrive as CRLF too. The lexer treats `\r` as whitespace and reads CRLF inside a multi-line string literal as LF (`crates/al-syntax/src/lexer.rs`). `crates/al-syntax/tests/crlf.rs` checks that LF and CRLF give the same tokens, positions and syntax trees, for a synthetic file and for three Base App objects. In the VM, the whole tree was converted to CRLF the way Git does it. After that, alstats printed the same catalog as before (9,823 files, 9,916 objects, 3,428 event subscriptions, 0 warnings), alcheck parsed all 11,910 files, and all 95 tests passed.
- **Path length.** Windows still defaults to `LongPathsEnabled=0`. The longest file in the app directories bc-rust loads is 134 characters below `vendor\bcapps\src\`, about 167 characters under `C:\dev\bc-rust`. A full BCApps clone goes up to 186 characters below `src\` (Power BI report files). Clone into a short path or set `git config --global core.longpaths true`. bc-rust itself is not affected: Rust's standard library adds the `\\?\` prefix to long paths on its own.
- **Case.** NTFS is case-insensitive. The AL resource lookup already matches path components case-insensitively (`crates/al-runtime/src/resources.rs`) and lists resources with `/` separators on every platform.

### Results

Same source snapshot on both sides. Linux is the 24-core host that also runs the VM, with builds limited to `-j 12` and rayon to 12 threads to match the VM's 12 vCPUs. The host was shared with other agents' test runs, so expect a few percent of noise.

| | Linux | Windows VM |
|---|---|---|
| Clean `cargo build --release` | 47 s | 201 s |
| Test binaries on top of that (`cargo test --no-run`) | 41 s | 90 s |
| Test run, 95 tests incl. doctests | 7 s | 44 s |
| flows test (Customer insert + sales order posting through codeunit 80, embedded store) | 1.38 s | 1.77 s |
| bc-nst unit tests (61) | 3.4 s | 9.1 to 11.3 s |
| Catalog load, parse + build (alstats, warm) | 0.18 + 1.00 s | 0.50 + 1.21 s |
| `bc serve` boot (catalog + store) | 1.23 to 1.29 s | 1.71 to 1.76 s |
| alcheck, 11,910 files, 4.78 M lines | 0.25 s | 0.51 s |
| Sales posting, embedded (`POST=20 bc sales`, median of 3 runs) | 17.3 ms | 17.1 ms |
| Order entry, embedded | 7.2 ms | 6.6 ms |
| Sales posting on PostgreSQL (110 SQL statements) | 32 to 38 ms | 65 to 67 ms |
| al-test `--suite foundation --jobs 4`, wall time incl. creating the Contoso company | 106 s, peak RSS 5.3 GB | 125 s, peak working set 3.9 GB |
| Creating the Contoso company (48,766 rows in 382 tables) inside that run | 102.8 s | 120.2 s |
| al-test results (91 tests) | 52 pass, 38 fail, 1 error | identical, test by test |

What this says:

- Windows runs Microsoft's AL the same way Linux does. All 91 foundation tests have the same outcome and the same error text on both, apart from LibraryRandom's random codes.
- The interpreter is as fast on Windows as on Linux. Posting and order entry take the same time. Creating the Contoso company, about 150 million AL statements, takes 17% longer in the VM.
- Building is about 4 times slower in the VM. I did not split that up. Likely causes are Defender scanning `target\` (C:\dev is not excluded), nested virtualization and the MSVC linker writing PDBs. A Dev Drive or a Defender exclusion for the target directory is worth trying on a real Windows machine.
- Parsing reads 9,823 files, and opening files costs more on Windows. The single-threaded catalog build is 20% slower, which is roughly the VM's overhead.
- The PostgreSQL gap is the VM's network, not Windows. The VM reaches the host's PostgreSQL through QEMU's user-mode NAT: a TCP connect takes 0.75 ms from the VM and 0.1 ms on the host, and every posting makes 110 round trips.

Also checked on Windows:

- `bc serve --memory`, `bc serve --db FILE` on an empty file (seeds the company, then `compact()` renames the snapshot over the open WAL) and a restart on that WAL (replays 169 records). /api/stats answered every time. The WAL came out at 153,266 bytes, the same size as on Linux, and no `.compact` file was left behind. `/api/page/21` (68 KB) answers in 1.8 ms.
- `bc-nst --memory` serves the client metadata the web server asks for (pages 21 and 22, table 18, navigation), and `tests/ws_session.rs` runs the JSON-RPC WebSocket session.
- bc-store's PostgreSQL tests (`BC_PG_TEST`) and `bc sales --pg` against a scratch database.
- `al-test`: the parity lock and the memory-based `--jobs` default. The VM showed 11.4 GB available, enough for 10 Contoso workers.

### Windows notes

- **stderr redirection.** Don't start bc-nst with PowerShell's `Start-Process -RedirectStandardError FILE`. bc-nst logs one line per metadata request, and behind that redirection a request for the navigation document took 36 ms (median) instead of 2 ms. With `cmd /c ... 2> FILE`, `NUL`, a console or a service log, bc-nst answers in about 2 ms, the same as on Linux. A bare axum server and an `HttpListener` in the same VM both serve 47 KB in 0.4 ms, so the network stack is fine. `TCP_NODELAY` made no measurable difference.
- **Paths in messages.** `canonicalize` returns `\\?\C:\...` on Windows. `al-test` and `demo-data` strip that prefix from the repository root (`bc_server::app::canonical`), so their messages show `C:\dev\bc-rust\data/parity\...`. The mixed separators come from `join("data/parity")`, and Windows accepts them.
- **Ctrl+C.** No binary installs a signal handler. Ctrl+C ends the process on Windows as SIGINT does on Linux. The WAL store syncs every commit (`FlushFileBuffers` on Windows), so nothing committed is lost.
- **al-test admission.** Runs queue by memory in `/tmp/bc-rust-al-test.d` on Linux and macOS and `%TEMP%\bc-rust-al-test.d` on Windows (`bc_server::admission`). Admitted runs hold `bc-rust-al-test.lock` next to it shared, and older binaries lock it exclusively. `%TEMP%` is per user, so on Windows the queue only covers one user's runs. Without MemAvailable (macOS) one run is admitted at a time. Outside Linux there is no per-process RSS (`/proc/<pid>/statm`), so a run's whole reservation counts as still to come.

## macOS

No Mac was available. On Linux, both targets type-check, tests and binaries included:

```sh
rustup target add x86_64-apple-darwin aarch64-apple-darwin
ZIG=/path/to/zig tools/macos-check.sh        # cargo check --workspace --all-targets for both targets
```

`cargo check` still runs build scripts, and ring's build script compiles C for the target. `tools/macos-check.sh` uses `zig cc -target aarch64-macos` (and x86_64) as that C compiler. zig 0.14.1 ships the macOS libc headers. The script wraps it to drop the `-arch` and `--target=` flags cc-rs adds. The resulting `libring_core` objects are Mach-O x86_64 and arm64. Both targets check without errors, with the same 2 warnings as Linux (unreachable patterns in `al-runtime/src/interp.rs`). A check takes 20 to 30 s from scratch.

Not covered: linking (needs the macOS SDK), running anything, and APFS behavior. On a Mac, Xcode's command line tools should be all that is needed (`xcode-select --install`, then `cargo build --release`). `al-test` has no available-memory reading on macOS, so `--jobs` defaults to the CPU count there. Pass `--jobs` on machines with little memory.

## Dependencies

Everything bc-rust links is pure Rust except ring's C and assembly and, for the `bc` binary on Linux and Windows, mimalloc's C. At run time nothing is needed beyond the OS and its C runtime (on Windows the VC++ redistributable, see above):

- `postgres` 0.19 / `tokio-postgres`: pure Rust, used with `NoTls`, so no OpenSSL. It pulls in `whoami`, which links the SystemConfiguration framework on macOS (part of the OS) and Win32 APIs on Windows.
- `ureq` 2 with `rustls` 0.23, `ring` 0.17 and `webpki-roots` (for AL's HttpClient emulation): ring needs a C compiler at build time (MSVC on Windows, Xcode's clang on macOS, gcc or clang on Linux). The root certificates come from `webpki-roots` (Mozilla's list compiled in), not from the OS trust store.
- `flate2` on `zlib-rs`, `chrono`, `chrono-tz` (time zone database compiled in), `rust_decimal`, `quick-xml`, `fancy-regex`, `encoding_rs`, `tokio`, `axum`: pure Rust.
- `mimalloc` 0.1 (`libmimalloc-sys` with feature `v2`, i.e. mimalloc 2.3.2, C): the global allocator of the `bc` and `al-test` binaries on Linux and Windows only (bc since 2026-11-07, P12: posting about 5% faster and RSS 1.9 → 1.25 GB on the served path; al-test since 2026-11-08). libmimalloc-sys builds mimalloc v3 (3.3.2) unless `v2` is set. On 2026-11-08 three al-test runs died with SIGSEGV inside v3's malloc and free (`mi_page_malloc_zero`, `mi_free` → `_mi_ptr_page`), and the same 2,461-test slice crashed on v3 and completed on v2, so both binaries used v2 from 02:27 UTC. At 04:26 UTC a v2 run crashed the same way (`_mi_page_malloc_zero`, P12), so the corruption is not v3's. Since 05:22 UTC mimalloc is the opt-in cargo feature `mimalloc` of bc-server (`cargo build --release -p bc-server --features mimalloc`; Linux and Windows only, a no-op elsewhere), and `bc` and `al-test` use the system allocator by default until the corruption is found. It needs a C compiler at build time (MSVC on Windows: checked on the VM, mimalloc 3.3.2 initialises in `bc.exe`). On macOS its sources include `<CommonCrypto/CommonCryptoError.h>` from the SDK, which `tools/macos-check.sh`'s zig cc doesn't have, so the dependency and the `#[global_allocator]` are behind `cfg(any(target_os = "linux", target_os = "windows"))` and macOS uses the system allocator. bc-nst uses the system allocator everywhere.

## Microsoft's web server

Microsoft's BC web server (`Prod.Client.WebCoreApp`, .NET 10) is a Windows application. It runs natively on Windows, as it does inside the BC container. On Linux it runs with the shims in `tools/msweb-linux/`. macOS was not tried. The scripts in `tools/` (`msweb-linux/run.sh`, `nst-proxy/run-webserver.sh`) are bash and assume a Linux host with systemd. The Node and Python tools should run anywhere but were only used on Linux.

## Platform-specific code

Keep this list short. Everything else goes through `std` and behaves the same on all three systems.

- `crates/bc-server/src/admission.rs`: the queue directory (`/tmp` on Unix, `%TEMP%` on Windows), `mem_available_mb` (`/proc/meminfo` on Linux, `GlobalMemoryStatusEx` on Windows, none on macOS) and `rss_mb` (`/proc/<pid>/statm`, Linux only). The locks are `std::fs::File::lock`/`try_lock`/`try_lock_shared`, which is `flock` on Unix and `LockFileEx` on Windows.
- `crates/bc-server/src/app.rs`: `canonical` strips `\\?\` on Windows.
- `crates/bc-server/src/main.rs` and `crates/bc-server/Cargo.toml`: mimalloc as the global allocator with the cargo feature `mimalloc`, Linux and Windows only (see Dependencies).
- `crates/al-syntax/src/lexer.rs`: CRLF handling. It is not behind a `cfg`, since a Linux machine can have a CRLF checkout too.
- `crates/al-runtime/src/files.rs`: AL's server files (`File`, `FILE.Exists` and so on) live in memory and follow Windows path rules on every platform: `/` and `\` are the same, case doesn't matter. `crates/al-runtime/src/resources.rs` matches app resource names case-insensitively.

## Known gaps

- macOS has never been linked or run.
- Windows numbers come from a nested VM. Real Windows hardware should build faster, and I have not measured it.
- `+crt-static` Windows builds are untested.
