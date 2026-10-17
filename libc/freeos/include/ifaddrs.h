/* Адреса интерфейсов (фаза 66a) — раскладка как у Linux и BSD.
 *
 * Подключает `<net/if.h>`: тот, кто перебирает интерфейсы, почти всегда смотрит
 * и на их флаги (`IFF_UP`, `IFF_LOOPBACK`), — Mono, например, делает это, не
 * подключив его сам. */

#ifndef FREEOS_IFADDRS_H
#define FREEOS_IFADDRS_H

#include <net/if.h>
#include <sys/socket.h>

#ifdef __cplusplus
extern "C" {
#endif

struct ifaddrs {
    struct ifaddrs *ifa_next;
    char *ifa_name;
    unsigned int ifa_flags;
    struct sockaddr *ifa_addr;
    struct sockaddr *ifa_netmask;
    union {
        struct sockaddr *ifu_broadaddr;
        struct sockaddr *ifu_dstaddr;
    } ifa_ifu;
    void *ifa_data;
};

#define ifa_broadaddr ifa_ifu.ifu_broadaddr
#define ifa_dstaddr ifa_ifu.ifu_dstaddr

int getifaddrs(struct ifaddrs **list);
void freeifaddrs(struct ifaddrs *list);

#ifdef __cplusplus
}
#endif

#endif
