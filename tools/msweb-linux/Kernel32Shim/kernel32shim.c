/*
 * Linux implementations of the two kernel32 functions that Microsoft.Dynamics.Framework.UI.Web
 * P/Invokes (Microsoft.Dynamics.Framework.UI.Web.Internal.NativeMethods):
 *
 *   CreateFileW(name, access, share, sa, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, 0)
 *   GetFinalPathNameByHandleW(handle, buf, len, 0)
 *
 * FileHelper.GetFileHash() uses them on every existing file it hashes (the "?h=<sha256>"
 * cache-busting suffix on CSS/JS/image URLs) to resolve symlinks before starting a
 * FileSystemWatcher. Without them every such request fails with DllNotFoundException('kernel32.dll').
 *
 * The library is NOT installed under a name the runtime probes for "kernel32.dll". The hosting
 * startup (../LinuxHostingStartup) maps "kernel32.dll" to this file only for that one assembly
 * through NativeLibrary.SetDllImportResolver, so other kernel32 P/Invokes in the app behave as before.
 *
 * Handles are plain file descriptors (opened O_PATH). The managed side wraps the return value in a
 * SafeFileHandle, whose Unix ReleaseHandle close()s it. Errors are reported through errno, which the
 * runtime exposes as Marshal.GetLastWin32Error() for SetLastError=true imports.
 *
 * Build: gcc -shared -fPIC -O2 -Wall -o libbclinux-kernel32.so kernel32shim.c
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

/* UTF-16 (NUL-terminated) -> newly allocated UTF-8. */
static char *utf16_to_utf8(const uint16_t *s)
{
    size_t n = 0;
    while (s[n]) n++;
    char *out = malloc(n * 3 + 1), *p = out;
    if (!out) return NULL;
    for (size_t i = 0; i < n; i++) {
        uint32_t c = s[i];
        if (c >= 0xD800 && c <= 0xDBFF && i + 1 < n && s[i + 1] >= 0xDC00 && s[i + 1] <= 0xDFFF)
            c = 0x10000 + ((c - 0xD800) << 10) + (s[++i] - 0xDC00);
        if (c < 0x80) *p++ = (char)c;
        else if (c < 0x800) { *p++ = (char)(0xC0 | c >> 6); *p++ = (char)(0x80 | (c & 0x3F)); }
        else if (c < 0x10000) { *p++ = (char)(0xE0 | c >> 12); *p++ = (char)(0x80 | (c >> 6 & 0x3F)); *p++ = (char)(0x80 | (c & 0x3F)); }
        else { *p++ = (char)(0xF0 | c >> 18); *p++ = (char)(0x80 | (c >> 12 & 0x3F)); *p++ = (char)(0x80 | (c >> 6 & 0x3F)); *p++ = (char)(0x80 | (c & 0x3F)); }
    }
    *p = 0;
    return out;
}

/* UTF-8 -> UTF-16 into out (capacity cap units, may be NULL to count). Returns units needed, no NUL. */
static size_t utf8_to_utf16(const unsigned char *s, uint16_t *out, size_t cap)
{
    size_t k = 0;
    while (*s) {
        uint32_t c = *s++;
        if (c >= 0xF0 && s[0] && s[1] && s[2]) { c = (c & 0x07) << 18 | (s[0] & 0x3F) << 12 | (s[1] & 0x3F) << 6 | (s[2] & 0x3F); s += 3; }
        else if (c >= 0xE0 && s[0] && s[1]) { c = (c & 0x0F) << 12 | (s[0] & 0x3F) << 6 | (s[1] & 0x3F); s += 2; }
        else if (c >= 0xC0 && s[0]) { c = (c & 0x1F) << 6 | (s[0] & 0x3F); s += 1; }
        if (c >= 0x10000) {
            if (out && k + 1 < cap) { out[k] = (uint16_t)(0xD800 + ((c - 0x10000) >> 10)); out[k + 1] = (uint16_t)(0xDC00 + ((c - 0x10000) & 0x3FF)); }
            k += 2;
        } else {
            if (out && k < cap) out[k] = (uint16_t)c;
            k += 1;
        }
    }
    return k;
}

intptr_t CreateFileW(const uint16_t *name, int32_t access, int32_t share, void *security,
                     int32_t disposition, int32_t flags, void *template_file)
{
    (void)access; (void)share; (void)security; (void)disposition; (void)flags; (void)template_file;
    if (!name) { errno = EINVAL; return -1; }
    char *path = utf16_to_utf8(name);
    if (!path) { errno = ENOMEM; return -1; }
    int fd = open(path, O_PATH | O_CLOEXEC); /* follows symlinks, like the Windows call without FILE_FLAG_OPEN_REPARSE_POINT */
    free(path);
    return fd < 0 ? -1 : fd;
}

/* Windows contract: on success the length without the NUL; if buf is too small the required size
 * including the NUL; 0 on failure. */
int32_t GetFinalPathNameByHandleW(intptr_t handle, uint16_t *buf, int32_t len, uint32_t flags)
{
    (void)flags;
    char link[64], target[PATH_MAX];
    snprintf(link, sizeof link, "/proc/self/fd/%d", (int)handle);
    ssize_t n = readlink(link, target, sizeof target - 1);
    if (n < 0) return 0;
    target[n] = 0;
    size_t need = utf8_to_utf16((const unsigned char *)target, NULL, 0);
    if (!buf || len < 0 || need + 1 > (size_t)len) return (int32_t)(need + 1);
    utf8_to_utf16((const unsigned char *)target, buf, (size_t)len);
    buf[need] = 0;
    return (int32_t)need;
}
