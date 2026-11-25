/* Время доступа и изменения файла (фаза 58). Заменяет «пустой» `sys/utime.h`
 * picolibc, который по её же словам система должна переопределить. С 24.11
 * `utime` ставит времена (`SYS_UTIMES`). */
#ifndef FREEOS_SYS_UTIME_H
#define FREEOS_SYS_UTIME_H
#include <sys/types.h>
/* `UTIME_NOW`/`UTIME_OMIT` for `utimensat`/`futimens` (24.11): picolibc has
 * none; the values are Linux ones, `files.c` turns them into `SYS_UTIMES`.
 * Here and not in `sys/stat.h`: the SDK copies this folder over picolibc. */
#ifndef UTIME_NOW
#define UTIME_NOW ((1l << 30) - 1l)
#endif
#ifndef UTIME_OMIT
#define UTIME_OMIT ((1l << 30) - 2l)
#endif
#ifdef __cplusplus
extern "C" {
#endif
struct utimbuf {
    time_t actime;
    time_t modtime;
};
int utime(const char *path, const struct utimbuf *times);
#ifdef __cplusplus
}
#endif
#endif
