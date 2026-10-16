/* Порядок байтов и `inet_*` (фаза 65c). Заменяет одноимённый заголовок
 * picolibc: тот несёт только порядок байтов, а POSIX велит объявлять здесь и
 * `inet_pton`/`inet_ntop`. Чужой код этому верит — mbedTLS, например, видит
 * `AF_INET6` и зовёт `inet_pton`, подключив только этот заголовок.
 *
 * Сами объявления живут в `netinet/in.h` (фаза 58); он подключается здесь
 * после макросов порядка байтов, которые нужны ему самому. */

#ifndef FREEOS_ARPA_INET_H
#define FREEOS_ARPA_INET_H

#include <endian.h>

/* Порядок байтов — как у picolibc, слово в слово. */
#ifndef __machine_host_to_from_network_defined
#if _BYTE_ORDER == _LITTLE_ENDIAN
#define __htonl(_x) __bswap32(_x)
#define __htons(_x) __bswap16(_x)
#define __ntohl(_x) __bswap32(_x)
#define __ntohs(_x) __bswap16(_x)
#else
#define __htonl(_x) ((__uint32_t)(_x))
#define __htons(_x) ((__uint16_t)(_x))
#define __ntohl(_x) ((__uint32_t)(_x))
#define __ntohs(_x) ((__uint16_t)(_x))
#endif
#define htonl(_x) __htonl(_x)
#define htons(_x) __htons(_x)
#define ntohl(_x) __ntohl(_x)
#define ntohs(_x) __ntohs(_x)
#endif /* __machine_host_to_from_network_defined */

#include <netinet/in.h>

#endif /* FREEOS_ARPA_INET_H */
