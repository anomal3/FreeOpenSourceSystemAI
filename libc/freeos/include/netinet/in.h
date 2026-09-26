/* Адреса IPv4 и IPv6 (фаза 58). Числа и раскладки — как у Linux.
 * `inet_*` объявлены здесь же: у picolibc `arpa/inet.h` несёт только порядок
 * байтов, а чужой код подключает оба заголовка. */

#ifndef FREEOS_NETINET_IN_H
#define FREEOS_NETINET_IN_H

#include <arpa/inet.h>
#include <stdint.h>
#include <sys/socket.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef uint32_t in_addr_t;
typedef uint16_t in_port_t;

struct in_addr {
    in_addr_t s_addr;
};

struct sockaddr_in {
    sa_family_t sin_family;
    in_port_t sin_port;
    struct in_addr sin_addr;
    unsigned char sin_zero[8];
};

struct in6_addr {
    union {
        uint8_t s6_addr[16];
        uint16_t __s6_addr16[8];
        uint32_t __s6_addr32[4];
    };
};

struct sockaddr_in6 {
    sa_family_t sin6_family;
    in_port_t sin6_port;
    uint32_t sin6_flowinfo;
    struct in6_addr sin6_addr;
    uint32_t sin6_scope_id;
};

#define INADDR_ANY ((in_addr_t)0x00000000)
#define INADDR_BROADCAST ((in_addr_t)0xffffffff)
#define INADDR_NONE ((in_addr_t)0xffffffff)
#define INADDR_LOOPBACK ((in_addr_t)0x7f000001)

#define IPPROTO_IP 0
#define IPPROTO_ICMP 1
#define IPPROTO_TCP 6
#define IPPROTO_UDP 17
#define IPPROTO_IPV6 41
#define IPPROTO_RAW 255

#define INET_ADDRSTRLEN 16
#define INET6_ADDRSTRLEN 46

in_addr_t inet_addr(const char *text);
int inet_aton(const char *text, struct in_addr *out);
char *inet_ntoa(struct in_addr address);
int inet_pton(int family, const char *text, void *out);
const char *inet_ntop(int family, const void *address, char *out, socklen_t length);

#ifdef __cplusplus
}
#endif

#endif
