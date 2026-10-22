/* Адреса IPv4 и IPv6 (фаза 58). Числа и раскладки — как у Linux.
 * `inet_*` объявлены здесь же; `arpa/inet.h` (свой с фазы 65c) подключает
 * этот заголовок, так что они видны через любой из двух. */

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

/* Параметры групповой рассылки (фаза 66a) — числа Linux. Самой рассылки у
 * ядра нет, и `setsockopt` отвечает на них `ENOPROTOOPT`; объявлены они
 * потому, что чужой код (Mono, `w32socket.c`) таблицей переводит свои имена в
 * эти и без них не собирается. */
#define IP_MULTICAST_IF 32
#define IP_MULTICAST_TTL 33
#define IP_MULTICAST_LOOP 34
#define IP_ADD_MEMBERSHIP 35
#define IP_DROP_MEMBERSHIP 36
#define IPV6_UNICAST_HOPS 16
#define IPV6_MULTICAST_IF 17
#define IPV6_MULTICAST_HOPS 18
#define IPV6_MULTICAST_LOOP 19
#define IPV6_JOIN_GROUP 20
#define IPV6_LEAVE_GROUP 21
/* Фаза 70d: cpp-httplib (его тянет llama.cpp) ставит `IPV6_V6ONLY` всякому
 * слушающему сокету и разбирает адреса макросами `IN6_IS_ADDR_*`. IPv6 в
 * системе нет: `setsockopt` с этим параметром отвечает отказом, а сокета
 * `AF_INET6` не создать вовсе, — но собираться код обязан. Число — Linux. */
#define IPV6_V6ONLY 26

#define IN6_IS_ADDR_UNSPECIFIED(a) \
    ((a)->__s6_addr32[0] == 0 && (a)->__s6_addr32[1] == 0 && \
     (a)->__s6_addr32[2] == 0 && (a)->__s6_addr32[3] == 0)
#define IN6_IS_ADDR_LOOPBACK(a) \
    ((a)->__s6_addr32[0] == 0 && (a)->__s6_addr32[1] == 0 && \
     (a)->__s6_addr32[2] == 0 && (a)->s6_addr[12] == 0 && (a)->s6_addr[13] == 0 && \
     (a)->s6_addr[14] == 0 && (a)->s6_addr[15] == 1)
#define IN6_IS_ADDR_MULTICAST(a) ((a)->s6_addr[0] == 0xff)
#define IN6_IS_ADDR_LINKLOCAL(a) ((a)->s6_addr[0] == 0xfe && ((a)->s6_addr[1] & 0xc0) == 0x80)
#define IN6_IS_ADDR_SITELOCAL(a) ((a)->s6_addr[0] == 0xfe && ((a)->s6_addr[1] & 0xc0) == 0xc0)
#define IN6_IS_ADDR_V4MAPPED(a) \
    ((a)->__s6_addr32[0] == 0 && (a)->__s6_addr32[1] == 0 && \
     (a)->s6_addr[8] == 0 && (a)->s6_addr[9] == 0 && \
     (a)->s6_addr[10] == 0xff && (a)->s6_addr[11] == 0xff)
#define IN6_IS_ADDR_V4COMPAT(a) \
    ((a)->__s6_addr32[0] == 0 && (a)->__s6_addr32[1] == 0 && \
     (a)->__s6_addr32[2] == 0 && !IN6_IS_ADDR_UNSPECIFIED(a) && !IN6_IS_ADDR_LOOPBACK(a))

struct ip_mreq {
    struct in_addr imr_multiaddr;
    struct in_addr imr_interface;
};

struct ipv6_mreq {
    struct in6_addr ipv6mr_multiaddr;
    unsigned int ipv6mr_interface;
};

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
