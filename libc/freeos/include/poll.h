/* <poll.h> FossAi (фаза 70d): то же, что у picolibc, но с `extern "C"`.
 *
 * У picolibc в этом заголовке нет `_BEGIN_STD_C`, и программа на C++ искала
 * `poll` под искажённым именем — `poll(pollfd*, unsigned long, int)`; так
 * компоновка llama.cpp (cpp-httplib) отказывала. Числа и раскладка — её же:
 * `poll` в `posix.c` собран против неё. Заменяет её файл целиком, потому что
 * `xtask sdk` кладёт наши заголовки поверх.
 */
#ifndef _POLL_H_
#define _POLL_H_

#include <sys/cdefs.h>

_BEGIN_STD_C

struct pollfd {
    int fd;
    short events;
    short revents;
};

typedef __size_t nfds_t;

#define POLLIN   0x0001
#define POLLPRI  0x0002
#define POLLOUT  0x0004

#define POLLERR  0x0008
#define POLLHUP  0x0010
#define POLLNVAL 0x0020

#if _XOPEN_SOURCE
#define POLLRDNORM 0x0040
#define POLLRDBAND 0x0080
#define POLLWRNORM 0x0100
#define POLLWRBAND 0x0200
#endif

#if __GNU_VISIBLE
#define POLLMSG    0x0400
#define POLLREMOVE 0x1000
#define POLLRDHUP  0x2000
#endif

int poll(struct pollfd[], nfds_t, int);

_END_STD_C

#endif /* _POLL_H_ */
