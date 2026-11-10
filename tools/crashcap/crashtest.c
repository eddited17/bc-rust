// crashtest: the test program of tools/crashcap/test.sh. It starts three idle threads, then crashes the way its
// argument says: heap (corrupts a chunk header; glibc aborts), segv (null pointer write) or abort.
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
static void *spin(void *a) { (void)a; for (;;) pause(); return 0; }
int main(int argc, char **argv) {
    pthread_t t[3];
    for (int i = 0; i < 3; i++) pthread_create(&t[i], 0, spin, 0);
    const char *m = argc > 1 ? argv[1] : "heap";
    if (!strcmp(m, "segv")) { volatile int *p = 0; *p = 1; }
    if (!strcmp(m, "abort")) abort();
    // heap: overwrite the size field of the next chunk (8 bytes past a 24-byte chunk), then free that chunk:
    // glibc detects it ("free(): invalid size") and aborts, as in the 2026-11-09 al-test crash
    char *a = malloc(24), *b = malloc(24);
    ((size_t *)a)[3] = 0x9; // size 8 (below the minimum), PREV_INUSE set
    free(b);
    free(a);
    printf("no crash\n");
    return 0;
}
