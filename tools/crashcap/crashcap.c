// crashcap: crash capture for bc-rust's binaries (al-test, bc, bc-nst) without root and without a core file.
//
//   gcc -O2 -fPIC -shared -o crashcap.so crashcap.c
//   LD_PRELOAD=/path/crashcap.so CRASHCAP_DIR=/some/dir al-test ...
//
// On SIGABRT, SIGSEGV, SIGBUS, SIGILL or SIGFPE the process runs gdb on itself before it dies: gdb attaches, prints
// `info threads` and the backtrace of every thread (the crashing one shows the frames below `<signal handler
// called>`), and detaches. The output goes to CRASHCAP_DIR/crash-<pid>-<epoch>.txt, or to stderr when CRASHCAP_DIR is
// unset; one header line always goes to stderr. Then the signal kills the process as it would have without the
// library: same signal, same exit status, and a core where RLIMIT_CORE allows one. Nothing runs before a crash except
// the handler installation at load time.
//
// Settings: CRASHCAP_DIR (output directory), CRASHCAP_GDB (default /usr/bin/gdb), CRASHCAP_TIMEOUT (seconds gdb may
// take, default 180; then it is killed and the process dies anyway), CRASHCAP_OFF=1 (install nothing). The library
// removes LD_PRELOAD from the environment at load time, so child processes run without it.
//
// Notes:
// - Yama ptrace_scope 1 lets only a declared tracer attach; the handler declares any process of the same user
//   (PR_SET_PTRACER_ANY) right before it starts gdb.
// - The handler uses only async-signal-safe calls (vfork + execve, no malloc), since the heap may be what is broken.
// - Rust's runtime installs its own SIGSEGV/SIGBUS handler (the "has overflowed its stack" message) only when none is
//   installed. With this library loaded first, a stack overflow on a thread without an alternate signal stack is
//   killed by the kernel with SIGSEGV directly (no message, no backtrace; a core where RLIMIT_CORE allows one).
//   al-test's stack guard stops deep AL recursion before that point.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef PR_SET_PTRACER
#define PR_SET_PTRACER 0x59616d61
#endif
#ifndef PR_SET_PTRACER_ANY
#define PR_SET_PTRACER_ANY ((unsigned long)-1)
#endif

static char cc_dir[512];
static char cc_gdb[256] = "/usr/bin/gdb";
static long cc_timeout = 180;
static atomic_int cc_busy;
static const int cc_sigs[] = {SIGABRT, SIGSEGV, SIGBUS, SIGILL, SIGFPE};

static size_t cc_len(const char *s) {
    size_t n = 0;
    while (s[n]) n++;
    return n;
}
static char *cc_put(char *p, const char *end, const char *s) {
    while (*s && p < end - 1) *p++ = *s++;
    *p = 0;
    return p;
}
static char *cc_putu(char *p, const char *end, unsigned long v) {
    char t[24];
    int n = 0;
    do t[n++] = (char)('0' + v % 10), v /= 10;
    while (v);
    while (n && p < end - 1) *p++ = t[--n];
    *p = 0;
    return p;
}
static char *cc_putx(char *p, const char *end, unsigned long v) {
    char t[24];
    int n = 0;
    do t[n++] = "0123456789abcdef"[v & 15], v >>= 4;
    while (v);
    p = cc_put(p, end, "0x");
    while (n && p < end - 1) *p++ = t[--n];
    *p = 0;
    return p;
}
static void cc_write(int fd, const char *s) {
    size_t n = cc_len(s);
    while (n) {
        ssize_t w = write(fd, s, n);
        if (w < 0 && errno == EINTR) continue;
        if (w <= 0) return;
        s += w;
        n -= (size_t)w;
    }
}
static const char *cc_signame(int s) {
    switch (s) {
    case SIGABRT: return "SIGABRT";
    case SIGSEGV: return "SIGSEGV";
    case SIGBUS: return "SIGBUS";
    case SIGILL: return "SIGILL";
    case SIGFPE: return "SIGFPE";
    }
    return "signal";
}

static void cc_capture(int sig, siginfo_t *si) {
    pid_t pid = getpid();
    long tid = syscall(SYS_gettid);
    struct timespec ts;
    clock_gettime(CLOCK_REALTIME, &ts);

    char path[700], *pe = path + sizeof path, *p = path;
    path[0] = 0;
    if (cc_dir[0]) {
        p = cc_put(p, pe, cc_dir);
        p = cc_put(p, pe, "/crash-");
        p = cc_putu(p, pe, (unsigned long)pid);
        p = cc_put(p, pe, "-");
        p = cc_putu(p, pe, (unsigned long)ts.tv_sec);
        p = cc_put(p, pe, ".txt");
    }
    char hdr[1024], *he = hdr + sizeof hdr, *h = hdr;
    h = cc_put(h, he, "crashcap: ");
    h = cc_put(h, he, cc_signame(sig));
    h = cc_put(h, he, " (signal ");
    h = cc_putu(h, he, (unsigned long)sig);
    h = cc_put(h, he, ", si_code ");
    if (si && si->si_code < 0) h = cc_put(h, he, "-"), h = cc_putu(h, he, (unsigned long)-si->si_code);
    else h = cc_putu(h, he, si ? (unsigned long)si->si_code : 0);
    if (si && (sig == SIGSEGV || sig == SIGBUS)) h = cc_put(h, he, ", address "), h = cc_putx(h, he, (unsigned long)si->si_addr);
    h = cc_put(h, he, ") in pid ");
    h = cc_putu(h, he, (unsigned long)pid);
    h = cc_put(h, he, " thread ");
    h = cc_putu(h, he, (unsigned long)tid);
    h = cc_put(h, he, " at unix time ");
    h = cc_putu(h, he, (unsigned long)ts.tv_sec);
    h = cc_put(h, he, "; gdb backtraces of every thread -> ");
    h = cc_put(h, he, path[0] ? path : "stderr");
    h = cc_put(h, he, "\n");
    cc_write(2, hdr);

    int fd = 2;
    if (path[0]) {
        fd = open(path, O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, 0644);
        if (fd < 0) {
            cc_write(2, "crashcap: cannot create the output file, writing to stderr\n");
            fd = 2;
        } else {
            cc_write(fd, hdr);
        }
    }

    char pidarg[24];
    cc_putu(pidarg, pidarg + sizeof pidarg, (unsigned long)pid);
    char *argv[] = {cc_gdb, "-q", "-nx", "-batch", "-p", pidarg,
                    "-ex", "set pagination off",
                    "-ex", "set width 0",
                    "-ex", "set print frame-arguments none",
                    "-ex", "info threads",
                    "-ex", "thread apply all bt 80",
                    "-ex", "detach",
                    NULL};
    char *envp[] = {"PATH=/usr/bin:/bin", "LC_ALL=C", NULL};

    prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY, 0, 0, 0);
    pid_t c = vfork();
    if (c == 0) {
        if (fd != 1) dup2(fd, 1);
        if (fd != 2) dup2(fd, 2);
        execve(cc_gdb, argv, envp);
        _exit(127);
    }
    if (c > 0) {
        struct timespec step = {0, 100 * 1000 * 1000};
        long waited_ms = 0;
        for (;;) {
            int st;
            pid_t r = waitpid(c, &st, WNOHANG);
            if (r == c || (r < 0 && errno != EINTR)) break;
            if (waited_ms >= cc_timeout * 1000) {
                kill(c, SIGKILL);
                waitpid(c, &st, 0);
                cc_write(2, "crashcap: gdb timed out, killed\n");
                break;
            }
            nanosleep(&step, NULL);
            waited_ms += 100;
        }
    } else {
        cc_write(2, "crashcap: vfork failed\n");
    }
    if (fd != 2) close(fd);
    cc_write(2, "crashcap: capture done, letting the signal kill the process\n");
}

static void cc_handler(int sig, siginfo_t *si, void *uc) {
    (void)uc;
    int saved = errno;
    if (atomic_exchange(&cc_busy, 1) == 0) cc_capture(sig, si);
    // die as without the handler: default action, then deliver the signal again. A fault re-executes the faulting
    // instruction on return; a sent or raised signal (si_code <= 0, abort's included) is raised again and stays
    // pending until the handler returns (it is blocked while the handler runs).
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_handler = SIG_DFL;
    sigemptyset(&sa.sa_mask);
    sigaction(sig, &sa, NULL);
    if (!si || si->si_code <= 0 || sig == SIGABRT) raise(sig);
    errno = saved;
}

__attribute__((constructor)) static void cc_init(void) {
    const char *off = getenv("CRASHCAP_OFF");
    if (off && off[0] == '1') return;
    const char *d = getenv("CRASHCAP_DIR");
    if (d && cc_len(d) < sizeof cc_dir) cc_put(cc_dir, cc_dir + sizeof cc_dir, d);
    const char *g = getenv("CRASHCAP_GDB");
    if (g && cc_len(g) < sizeof cc_gdb) cc_put(cc_gdb, cc_gdb + sizeof cc_gdb, g);
    const char *t = getenv("CRASHCAP_TIMEOUT");
    if (t && atol(t) > 0) cc_timeout = atol(t);
    // child processes don't inherit the library (a .NET or JVM child installs its own SIGSEGV handling); this runs
    // before main, while the process has one thread
    unsetenv("LD_PRELOAD");

    // an alternate stack for the main thread, so its stack overflows are captured too
    size_t sz = 256 * 1024;
    void *alt = mmap(NULL, sz, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (alt != MAP_FAILED) {
        stack_t ss = {.ss_sp = alt, .ss_size = sz, .ss_flags = 0};
        sigaltstack(&ss, NULL);
    }
    for (size_t i = 0; i < sizeof cc_sigs / sizeof cc_sigs[0]; i++) {
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_sigaction = cc_handler;
        sa.sa_flags = SA_SIGINFO | SA_ONSTACK;
        sigemptyset(&sa.sa_mask);
        sigaction(cc_sigs[i], &sa, NULL);
    }
}
