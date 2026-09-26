/* Ввод-вывод кусками (фаза 58). У picolibc этого заголовка нет, а
 * `sys/socket.h` без `struct iovec` не описать. `readv` и `writev` — по
 * куску за раз поверх `read` и `write`: атомарности всей пачки ядро не
 * обещает, и это сказано здесь, а не обнаружено потом. */

#ifndef FREEOS_SYS_UIO_H
#define FREEOS_SYS_UIO_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

struct iovec {
    void *iov_base;
    size_t iov_len;
};

#define IOV_MAX 1024

ssize_t readv(int fd, const struct iovec *pieces, int count);
ssize_t writev(int fd, const struct iovec *pieces, int count);

#ifdef __cplusplus
}
#endif

#endif
