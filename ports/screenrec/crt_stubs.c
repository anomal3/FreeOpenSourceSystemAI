/* The bits of the C library openh264 and libopus reach for that the screen
 * recorder does not have (23.11). `/bin/screenrec` is a `no_std` Rust
 * program without picolibc: memory, threads, semaphores, clocks and libm come
 * from its `crt` module (Rust); here is only what is better said in C and
 * needs no system at all — openh264's debug dumps and trace output, which the
 * recorder never turns on.
 *
 * Files never open (`fopen` — null), so the rest is never reached with a
 * real stream; trace text is dropped.
 */

#include <stdarg.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

FILE *const stderr = 0;

FILE *fopen(const char *path, const char *mode) {
    (void)path;
    (void)mode;
    return 0;
}

int fclose(FILE *f) {
    (void)f;
    return 0;
}

int fflush(FILE *f) {
    (void)f;
    return 0;
}

int fseek(FILE *f, long offset, int whence) {
    (void)f;
    (void)offset;
    (void)whence;
    return -1;
}

size_t fwrite(const void *data, size_t size, size_t count, FILE *f) {
    (void)data;
    (void)size;
    (void)f;
    return count;
}

int fprintf(FILE *f, const char *format, ...) {
    (void)f;
    (void)format;
    return 0;
}

int vsnprintf(char *out, size_t size, const char *format, va_list args) {
    (void)format;
    (void)args;
    if (size > 0) {
        out[0] = 0;
    }
    return 0;
}

size_t strftime(char *out, size_t size, const char *format, const struct tm *tm) {
    (void)format;
    (void)tm;
    if (size > 0) {
        out[0] = 0;
    }
    return 0;
}

struct tm *localtime(const time_t *t) {
    static struct tm zero;
    (void)t;
    return &zero;
}

char *strncpy(char *dst, const char *src, size_t n) {
    size_t i = 0;
    for (; i < n && src[i]; i++) {
        dst[i] = src[i];
    }
    for (; i < n; i++) {
        dst[i] = 0;
    }
    return dst;
}

int abs(int v) {
    return v < 0 ? -v : v;
}
