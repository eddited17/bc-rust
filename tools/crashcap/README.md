# crashcap: backtraces of every thread when a bc-rust binary crashes

`crashcap.so` is a small preload library for `al-test`, `bc` and `bc-nst` on Linux. When the process gets SIGABRT
(a glibc heap check, a Rust abort), SIGSEGV, SIGBUS, SIGILL or SIGFPE, the library runs gdb on the process before it
dies. gdb writes `info threads` and the backtrace of every thread to a file. Then the signal kills the process as it
would have without the library, with the same exit status, and a core is still written where `RLIMIT_CORE` allows one.
It needs gdb but no root, and no core file on the root disk. A capture takes about a second for al-test.

It was written for the heap corruption that aborted one full al-test run on 2026-11-09 (glibc
`malloc_consolidate(): unaligned fastbin chunk detected`), which left nothing but that message. Ubuntu's apport only
keeps a core of an unpackaged binary when the soft core limit is above 0, and the cores land in
`/var/lib/apport/coredump`, which only root can clean up.

```sh
gcc -O2 -fPIC -shared -o crashcap.so tools/crashcap/crashcap.c
LD_PRELOAD=$PWD/crashcap.so CRASHCAP_DIR=/some/dir al-test ...
tools/crashcap/test.sh        # builds both files in a temporary directory and crashes a test program three ways
```

- `CRASHCAP_DIR`: where `crash-<pid>-<unix time>.txt` goes. Without it, gdb's output goes to stderr.
  One `crashcap: SIGABRT ... in pid ... thread ...` line always goes to stderr.
- `CRASHCAP_GDB` (default `/usr/bin/gdb`), `CRASHCAP_TIMEOUT` (seconds gdb may take, default 180; then gdb is killed
  and the process dies anyway), `CRASHCAP_OFF=1` (install nothing).
- The library removes `LD_PRELOAD` from the environment when it loads, so child processes don't inherit it.
- A missing `.so` is not fatal: the dynamic loader prints a warning and the program runs without it.

For a systemd unit, a drop-in such as `~/.config/systemd/user/bc-rust.service.d/crashcap.conf`:

```ini
[Service]
Environment=LD_PRELOAD=%h/.local/lib/bc-rust/crashcap.so
Environment=CRASHCAP_DIR=%h/bc-rust-crash
```

Limits:

- Rust's runtime installs its own SIGSEGV handler (the "has overflowed its stack" message) only when there is none.
  With the library loaded, a native stack overflow on a thread without an alternate signal stack is killed by the
  kernel with SIGSEGV directly, with no message and no capture. Threads that run AL register their stacks with
  `al_runtime::stackguard`, which stops deep recursion with "Maximum call depth exceeded" before that point.
- Yama's `ptrace_scope` 1 allows only a declared tracer to attach. The handler declares any process of the same user
  (`PR_SET_PTRACER_ANY`) just before it starts gdb, so only after the crash.
- The handler uses only async-signal-safe calls (`vfork`, `execve`, `waitpid`, no `malloc`), because the heap may be
  what is broken. Other threads keep running until gdb has attached.
