/* Сетевые интерфейсы (фаза 66a): флаги и длина имени — числа Linux.
 *
 * Интерфейс у машины один — карта, которую поднял `net` ядра, — и его видно
 * через `getifaddrs` (`<ifaddrs.h>`). Петли `127.0.0.1` у ядра нет, и
 * интерфейса `lo` здесь не бывает. */

#ifndef FREEOS_NET_IF_H
#define FREEOS_NET_IF_H

#define IF_NAMESIZE 16
#define IFNAMSIZ IF_NAMESIZE

#define IFF_UP 0x1
#define IFF_BROADCAST 0x2
#define IFF_LOOPBACK 0x8
#define IFF_RUNNING 0x40
#define IFF_MULTICAST 0x1000

#ifdef __cplusplus
extern "C" {
#endif

unsigned int if_nametoindex(const char *name);
char *if_indextoname(unsigned int index, char *name);

#ifdef __cplusplus
}
#endif

#endif
