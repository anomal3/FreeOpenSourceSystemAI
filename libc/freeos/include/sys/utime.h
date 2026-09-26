/* Время доступа и изменения файла (фаза 58). Заменяет «пустой» `sys/utime.h`
 * picolibc, который по её же словам система должна переопределить. Ядро
 * времён файлу не ставит, и `utime` отвечает `ENOSYS`. */
#ifndef FREEOS_SYS_UTIME_H
#define FREEOS_SYS_UTIME_H
#include <sys/types.h>
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
