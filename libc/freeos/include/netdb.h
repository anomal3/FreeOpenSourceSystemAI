/* Имена хостов, служб и протоколов (фаза 58).
 *
 * Раскладки и коды `EAI_*` — как у Linux. Протоколы отвечают по короткой
 * таблице (`ip`, `icmp`, `tcp`, `udp`); имена хостов и `getaddrinfo` пока
 * разбирают только готовый адрес «a.b.c.d» и на имя отвечают
 * `EAI_NONAME`/`HOST_NOT_FOUND` — к `SYS_RESOLVE` их привяжет та же часть
 * фазы, что и сокеты. */

#ifndef FREEOS_NETDB_H
#define FREEOS_NETDB_H

#include <netinet/in.h>
#include <sys/socket.h>

#ifdef __cplusplus
extern "C" {
#endif

struct hostent {
    char *h_name;
    char **h_aliases;
    int h_addrtype;
    int h_length;
    char **h_addr_list;
};
#define h_addr h_addr_list[0]

struct protoent {
    char *p_name;
    char **p_aliases;
    int p_proto;
};

struct servent {
    char *s_name;
    char **s_aliases;
    int s_port;
    char *s_proto;
};

struct addrinfo {
    int ai_flags;
    int ai_family;
    int ai_socktype;
    int ai_protocol;
    socklen_t ai_addrlen;
    struct sockaddr *ai_addr;
    char *ai_canonname;
    struct addrinfo *ai_next;
};

#define AI_PASSIVE 0x01
#define AI_CANONNAME 0x02
#define AI_NUMERICHOST 0x04
#define AI_NUMERICSERV 0x400
#define AI_ADDRCONFIG 0x20

#define NI_NUMERICHOST 1
#define NI_NUMERICSERV 2
#define NI_MAXHOST 1025
#define NI_MAXSERV 32

#define EAI_BADFLAGS -1
#define EAI_NONAME -2
#define EAI_AGAIN -3
#define EAI_FAIL -4
#define EAI_FAMILY -6
#define EAI_SOCKTYPE -7
#define EAI_SERVICE -8
#define EAI_MEMORY -10
#define EAI_SYSTEM -11

#define HOST_NOT_FOUND 1
#define TRY_AGAIN 2
#define NO_RECOVERY 3
#define NO_DATA 4

extern int h_errno;

struct hostent *gethostbyname(const char *name);
struct hostent *gethostbyaddr(const void *address, socklen_t length, int family);
struct protoent *getprotobyname(const char *name);
struct protoent *getprotobynumber(int proto);
struct servent *getservbyname(const char *name, const char *proto);
int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **result);
void freeaddrinfo(struct addrinfo *list);
const char *gai_strerror(int code);
int getnameinfo(const struct sockaddr *address, socklen_t length, char *host, socklen_t host_length, char *service,
                socklen_t service_length, int flags);

#ifdef __cplusplus
}
#endif

#endif
