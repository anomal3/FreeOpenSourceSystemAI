/* Сокеты BSD поверх сокетов-дескрипторов ядра (фаза 65b).
 *
 * Сокет у ядра — обычный дескриптор (фаза 65a, `SYS_SOCKFD`): `read`,
 * `write`, `close`, `poll` и `select` работают на нём сами, без этого файла.
 * Здесь — то, чего у файла нет: адрес и порт (`bind`, `connect`, `accept`,
 * `getsockname`), параметры, имена хостов и служб. Всё — IPv4 и только он: у
 * ядра нет ни IPv6, ни петли 127.0.0.1 (запись в ROADMAP, веха v0.10).
 *
 * Сокет по умолчанию блокирующий, как в Unix: ждёт ядро, а не этот файл.
 * Неблокирующий режим включает только libc — она его и помнит
 * (`nonblocking` ниже): `fcntl(F_GETFL)` обязан ответить, не спрашивая ядро.
 */

#define _GNU_SOURCE

#include <arpa/inet.h>
#include <errno.h>
#include <ifaddrs.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <unistd.h>

#include "freeos-internal.h"
#include "freeos-syscall.h"

/* ── Неблокирующий режим ─────────────────────────────────────────────────── */

/* По номеру дескриптора: включён ли неблокирующий режим. Номер дескриптора
 * не бывает больше трёх стандартных и `FREEOS_MAX_OPEN_FILES`. */
static unsigned char nonblocking[3 + FREEOS_MAX_OPEN_FILES];

static int remembered(int fd) {
    return fd >= 0 && (size_t)fd < sizeof(nonblocking);
}

int freeos_socket_set_nonblock(int fd, int on) {
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_NONBLOCK, on ? 1 : 0);
    if (code < 0) {
        /* Ядро отвечает «нет такого сокета» кодом плохого дескриптора: для
         * `ioctl` и `fcntl` это «не сокет». */
        if (code == FREEOS_ERR_BAD_FD) {
            errno = ENOTSOCK;
            return -1;
        }
        return freeos_set_errno(code);
    }
    if (remembered(fd)) {
        nonblocking[fd] = on ? 1 : 0;
    }
    return 0;
}

int freeos_socket_is_nonblock(int fd) {
    return remembered(fd) && nonblocking[fd];
}

void freeos_socket_forget(int fd) {
    if (remembered(fd)) {
        nonblocking[fd] = 0;
    }
}

/* ── Сведения о сокете ───────────────────────────────────────────────────── */

static int info_of(int fd, struct freeos_sockinfo *info) {
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_INFO, (long)info);
    if (code < 0) {
        if (code == FREEOS_ERR_BAD_FD) {
            errno = ENOTSOCK;
            return -1;
        }
        return freeos_set_errno(code);
    }
    return 0;
}

/* Адрес в `struct sockaddr_in` — с обрезкой по длине, которую дал вызывающий,
 * и с настоящей длиной в ответ: так велит POSIX. */
static void put_address(struct sockaddr *address, socklen_t *length, uint32_t ip, uint16_t port) {
    if (address == NULL || length == NULL) {
        return;
    }
    struct sockaddr_in in;
    memset(&in, 0, sizeof(in));
    in.sin_family = AF_INET;
    in.sin_port = htons(port);
    in.sin_addr.s_addr = htonl(ip);
    size_t take = *length < sizeof(in) ? *length : sizeof(in);
    memcpy(address, &in, take);
    *length = sizeof(in);
}

/* Адрес из `struct sockaddr` — только IPv4. */
static int take_address(const struct sockaddr *address, socklen_t length, uint32_t *ip, uint16_t *port) {
    if (address == NULL || length < sizeof(struct sockaddr_in)) {
        errno = EINVAL;
        return -1;
    }
    if (address->sa_family != AF_INET) {
        errno = EAFNOSUPPORT;
        return -1;
    }
    const struct sockaddr_in *in = (const struct sockaddr_in *)address;
    *ip = ntohl(in->sin_addr.s_addr);
    *port = ntohs(in->sin_port);
    return 0;
}

/* ── Сокет ───────────────────────────────────────────────────────────────── */

int socket(int domain, int type, int protocol) {
    if (domain != AF_INET) {
        errno = EAFNOSUPPORT;
        return -1;
    }
    int nonblock = (type & SOCK_NONBLOCK) != 0;
    /* `SOCK_CLOEXEC` у нас и так всегда: запущенная программа дескрипторов не
     * наследует (см. `fcntl` в `files.c`). */
    int base = type & ~(SOCK_NONBLOCK | SOCK_CLOEXEC);
    long kind;
    if (base == SOCK_STREAM && (protocol == 0 || protocol == IPPROTO_TCP)) {
        kind = FREEOS_SOCK_TCP;
    } else if (base == SOCK_DGRAM && (protocol == 0 || protocol == IPPROTO_UDP)) {
        kind = FREEOS_SOCK_UDP;
    } else {
        errno = EPROTONOSUPPORT;
        return -1;
    }
    long fd = freeos_syscall(SYS_SOCKFD, kind, nonblock ? FREEOS_SOCK_NONBLOCK : 0, 0);
    if (fd < 0) {
        return freeos_set_errno(fd);
    }
    if (remembered((int)fd)) {
        nonblocking[fd] = (unsigned char)nonblock;
    }
    return (int)fd;
}

/* Адрес в `bind` у нас всегда «любой»: адрес у машины один. Другой — не наш,
 * и честный ответ на него — `EADDRNOTAVAIL`, а не молча «любой». */
int bind(int fd, const struct sockaddr *address, socklen_t length) {
    uint32_t ip;
    uint16_t port;
    if (take_address(address, length, &ip, &port) != 0) {
        return -1;
    }
    if (ip != INADDR_ANY) {
        struct freeos_netinfo net;
        long code = freeos_syscall(SYS_NETINFO, (long)&net, sizeof(net), 0);
        if (code < 0 || net.address != ip) {
            errno = EADDRNOTAVAIL;
            return -1;
        }
    }
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_BIND, port);
    if (code < 0) {
        if (code == FREEOS_ERR_EXISTS) {
            errno = EADDRINUSE;
            return -1;
        }
        return freeos_set_errno(code);
    }
    return 0;
}

int connect(int fd, const struct sockaddr *address, socklen_t length) {
    uint32_t ip;
    uint16_t port;
    if (take_address(address, length, &ip, &port) != 0) {
        return -1;
    }
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_CONNECT, (long)(((uint64_t)ip << 16) | port));
    return code < 0 ? freeos_set_errno(code) : 0;
}

/* Очередь ожидающих соединений у ядра своя (четыре места); `backlog` она не
 * слушает — меньше просили или больше, ответ один. */
int listen(int fd, int backlog) {
    (void)backlog;
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_LISTEN, 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

int accept4(int fd, struct sockaddr *address, socklen_t *length, int flags) {
    if ((flags & ~(SOCK_NONBLOCK | SOCK_CLOEXEC)) != 0) {
        errno = EINVAL;
        return -1;
    }
    long client = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_ACCEPT, 0);
    if (client < 0) {
        return freeos_set_errno(client);
    }
    if ((flags & SOCK_NONBLOCK) != 0 && freeos_socket_set_nonblock((int)client, 1) != 0) {
        close((int)client);
        return -1;
    }
    if (address != NULL) {
        struct freeos_sockinfo info;
        if (info_of((int)client, &info) == 0) {
            put_address(address, length, info.remote, info.remote_port);
        }
    }
    return (int)client;
}

int accept(int fd, struct sockaddr *address, socklen_t *length) {
    return accept4(fd, address, length, 0);
}

/* `MSG_DONTWAIT` — неблокирующий на один вызов: режим включается и
 * возвращается обратно. Окно между ними видно другому потоку той же
 * программы, и у Linux его нет; у нас — цена того, что режим живёт у сокета. */
static ssize_t once(int fd, int flags, ssize_t (*op)(int, void *, size_t), void *buf, size_t len) {
    int dontwait = (flags & MSG_DONTWAIT) != 0 && !freeos_socket_is_nonblock(fd);
    if (dontwait && freeos_socket_set_nonblock(fd, 1) != 0) {
        return -1;
    }
    ssize_t done = op(fd, buf, len);
    if (dontwait) {
        int saved = errno;
        freeos_socket_set_nonblock(fd, 0);
        errno = saved;
    }
    return done;
}

static ssize_t write_op(int fd, void *buf, size_t len) {
    return write(fd, buf, len);
}

/* `MSG_NOSIGNAL` у нас и так: сигнала `SIGPIPE` ядро не шлёт, оборванное
 * соединение — это ответ `ECONNRESET`. */
ssize_t send(int fd, const void *buf, size_t len, int flags) {
    if ((flags & (MSG_OOB | MSG_PEEK)) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    return once(fd, flags, write_op, (void *)buf, len);
}

/* `MSG_PEEK` (подсмотреть, не забирая) ядро не умеет — `EOPNOTSUPP`, а не
 * обычное чтение: программа, подсмотревшая данные, прочтёт их ещё раз. */
ssize_t recv(int fd, void *buf, size_t len, int flags) {
    if ((flags & (MSG_OOB | MSG_PEEK)) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    return once(fd, flags, read, buf, len);
}

/* Датаграмме новому получателю ядро называет его через «соединение»: UDP
 * запоминает адрес, и следующая запись идёт туда. */
ssize_t sendto(int fd, const void *buf, size_t len, int flags, const struct sockaddr *to, socklen_t length) {
    if (to != NULL) {
        struct freeos_sockinfo info;
        if (info_of(fd, &info) != 0) {
            return -1;
        }
        /* У потока адрес уже есть; `to` у него POSIX велит не замечать. */
        if (!info.tcp && connect(fd, to, length) != 0) {
            return -1;
        }
    }
    return send(fd, buf, len, flags);
}

ssize_t recvfrom(int fd, void *buf, size_t len, int flags, struct sockaddr *from, socklen_t *length) {
    ssize_t done = recv(fd, buf, len, flags);
    if (done >= 0 && from != NULL) {
        struct freeos_sockinfo info;
        if (info_of(fd, &info) == 0) {
            if (info.tcp) {
                put_address(from, length, info.remote, info.remote_port);
            } else {
                put_address(from, length, info.from, info.from_port);
            }
        }
    }
    return done;
}

ssize_t sendmsg(int fd, const struct msghdr *message, int flags) {
    if (message->msg_controllen != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    if (message->msg_name != NULL) {
        struct freeos_sockinfo info;
        if (info_of(fd, &info) != 0) {
            return -1;
        }
        if (!info.tcp && connect(fd, message->msg_name, message->msg_namelen) != 0) {
            return -1;
        }
    }
    if ((flags & (MSG_OOB | MSG_PEEK)) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    return writev(fd, message->msg_iov, (int)message->msg_iovlen);
}

ssize_t recvmsg(int fd, struct msghdr *message, int flags) {
    if ((flags & (MSG_OOB | MSG_PEEK)) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    ssize_t done = readv(fd, message->msg_iov, (int)message->msg_iovlen);
    if (done >= 0) {
        message->msg_controllen = 0;
        message->msg_flags = 0;
        if (message->msg_name != NULL) {
            struct freeos_sockinfo info;
            if (info_of(fd, &info) == 0) {
                socklen_t length = message->msg_namelen;
                put_address(message->msg_name, &length, info.tcp ? info.remote : info.from,
                            info.tcp ? info.remote_port : info.from_port);
                message->msg_namelen = length;
            }
        }
    }
    return done;
}

static int put_int(void *value, socklen_t *length, int number) {
    if (value == NULL || length == NULL || *length < sizeof(int)) {
        errno = EINVAL;
        return -1;
    }
    memcpy(value, &number, sizeof(int));
    *length = sizeof(int);
    return 0;
}

/* Параметры, которые ядро знает или которые однозначны: `SO_ERROR` — исход
 * неблокирующего соединения (поток оборван — `ECONNREFUSED`), `SO_TYPE`,
 * размеры буферов (8 КиБ у TCP в ядре), `TCP_NODELAY` — включён всегда: ядро
 * не придерживает мелкие отправки. */
int getsockopt(int fd, int level, int name, void *value, socklen_t *length) {
    struct freeos_sockinfo info;
    if (info_of(fd, &info) != 0) {
        return -1;
    }
    if (level == SOL_SOCKET) {
        switch (name) {
        case SO_ERROR:
            return put_int(value, length, info.tcp && info.reset && !info.open ? ECONNREFUSED : 0);
        case SO_TYPE:
            return put_int(value, length, info.tcp ? SOCK_STREAM : SOCK_DGRAM);
        case SO_ACCEPTCONN:
            return put_int(value, length, info.listening);
        case SO_SNDBUF:
        case SO_RCVBUF:
            return put_int(value, length, 8192);
        case SO_REUSEADDR:
        case SO_KEEPALIVE:
        case SO_BROADCAST:
        case SO_OOBINLINE:
            return put_int(value, length, 0);
        default:
            break;
        }
    } else if (level == IPPROTO_TCP && name == TCP_NODELAY) {
        return put_int(value, length, 1);
    }
    errno = ENOPROTOOPT;
    return -1;
}

/* Устанавливать нечего: ядро не держит параметров сокета. Известные
 * принимаются молча — их поведение у нас и так такое, о каком просят
 * (`TCP_NODELAY`), или безвредно не меняется (буферы, `SO_REUSEADDR`,
 * сроки). Незнакомые — `ENOPROTOOPT`, как и положено. */
int setsockopt(int fd, int level, int name, const void *value, socklen_t length) {
    (void)value;
    (void)length;
    struct freeos_sockinfo info;
    if (info_of(fd, &info) != 0) {
        return -1;
    }
    if (level == SOL_SOCKET) {
        switch (name) {
        case SO_REUSEADDR:
        case SO_REUSEPORT:
        case SO_KEEPALIVE:
        case SO_SNDBUF:
        case SO_RCVBUF:
        case SO_LINGER:
        case SO_RCVTIMEO:
        case SO_SNDTIMEO:
        case SO_BROADCAST:
        case SO_OOBINLINE:
            return 0;
        default:
            break;
        }
    } else if (level == IPPROTO_TCP && name == TCP_NODELAY) {
        return 0;
    }
    errno = ENOPROTOOPT;
    return -1;
}

/* Свой адрес — адрес машины (`SYS_NETINFO`), порт — у ядра. */
int getsockname(int fd, struct sockaddr *address, socklen_t *length) {
    struct freeos_sockinfo info;
    if (info_of(fd, &info) != 0) {
        return -1;
    }
    struct freeos_netinfo net;
    uint32_t ip = 0;
    if (freeos_syscall(SYS_NETINFO, (long)&net, sizeof(net), 0) >= 0) {
        ip = net.address;
    }
    put_address(address, length, ip, info.local_port);
    return 0;
}

int getpeername(int fd, struct sockaddr *address, socklen_t *length) {
    struct freeos_sockinfo info;
    if (info_of(fd, &info) != 0) {
        return -1;
    }
    if (!info.open) {
        errno = ENOTCONN;
        return -1;
    }
    put_address(address, length, info.remote, info.remote_port);
    return 0;
}

/* Закрыть на чтение ядро не умеет; на запись — `SOCKCTL_SHUTDOWN`. */
int shutdown(int fd, int how) {
    if (how == SHUT_RD) {
        struct freeos_sockinfo info;
        return info_of(fd, &info);
    }
    if (how != SHUT_WR && how != SHUT_RDWR) {
        errno = EINVAL;
        return -1;
    }
    long code = freeos_syscall(SYS_SOCKCTL, fd, FREEOS_SOCKCTL_SHUTDOWN, 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

/* Пары сокетов (`AF_UNIX`) у ядра нет; когда её попросит конкретная
 * программа, она станет фазой (запись в ROADMAP, веха v0.10). */
int socketpair(int domain, int type, int protocol, int fds[2]) {
    (void)domain, (void)type, (void)protocol, (void)fds;
    errno = EAFNOSUPPORT;
    return -1;
}

/* ── Имена: хосты и службы ───────────────────────────────────────────────── */

extern int h_errno;

/* Службы по имени — те, что нужны программам сети. Своего `/etc/services` у
 * системы нет, и таблица здесь честнее файла, которого никто не читает. */
static const struct {
    const char *name;
    int port;
} SERVICES[] = {
    {"echo", 7},     {"ftp", 21},     {"ssh", 22},     {"telnet", 23}, {"smtp", 25},
    {"domain", 53},  {"http", 80},    {"pop3", 110},   {"ntp", 123},   {"imap", 143},
    {"https", 443},  {"imaps", 993},  {"pop3s", 995},  {"http-alt", 8080},
};

static int service_port(const char *name) {
    for (size_t index = 0; index < sizeof(SERVICES) / sizeof(SERVICES[0]); index++) {
        if (strcmp(SERVICES[index].name, name) == 0) {
            return SERVICES[index].port;
        }
    }
    return -1;
}

struct servent *getservbyname(const char *name, const char *proto) {
    static struct servent entry;
    static char *aliases[1] = {NULL};
    int port = service_port(name);
    if (port < 0) {
        return NULL;
    }
    entry.s_name = (char *)name;
    entry.s_aliases = aliases;
    entry.s_port = htons((uint16_t)port);
    entry.s_proto = (char *)(proto != NULL ? proto : "tcp");
    return &entry;
}

/* Имя в адрес. Готовый адрес — как есть; `localhost` — 127.0.0.1 (адрес
 * правильный, хотя петли у ядра нет); прочее — `SYS_RESOLVE` (DNS, запись A).
 * Ответ — ноль либо код `EAI_*`. */
static int resolve(const char *name, int numeric_only, struct in_addr *out) {
    if (inet_aton(name, out)) {
        return 0;
    }
    if (numeric_only) {
        return EAI_NONAME;
    }
    if (strcmp(name, "localhost") == 0) {
        out->s_addr = htonl(INADDR_LOOPBACK);
        return 0;
    }
    unsigned char bytes[4];
    long code = freeos_syscall(SYS_RESOLVE, (long)name, (long)strlen(name), (long)bytes);
    if (code < 0) {
        switch (code) {
        case FREEOS_ERR_NOT_FOUND:
            return EAI_NONAME;
        case FREEOS_ERR_AGAIN:
            return EAI_AGAIN;
        default:
            return EAI_FAIL;
        }
    }
    memcpy(&out->s_addr, bytes, sizeof(bytes));
    return 0;
}

struct hostent *gethostbyname(const char *name) {
    static struct in_addr address;
    static char *list[2] = {(char *)&address, NULL};
    static char *aliases[1] = {NULL};
    static struct hostent host;
    int code = resolve(name, 0, &address);
    if (code != 0) {
        h_errno = code == EAI_AGAIN ? TRY_AGAIN : HOST_NOT_FOUND;
        return NULL;
    }
    host.h_name = (char *)name;
    host.h_aliases = aliases;
    host.h_addrtype = AF_INET;
    host.h_length = sizeof(address);
    host.h_addr_list = list;
    return &host;
}

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **result) {
    int family = hints != NULL ? hints->ai_family : AF_UNSPEC;
    int flags = hints != NULL ? hints->ai_flags : 0;
    int socktype = hints != NULL ? hints->ai_socktype : 0;
    int protocol = hints != NULL ? hints->ai_protocol : 0;
    if (family != AF_UNSPEC && family != AF_INET) {
        return EAI_FAMILY;
    }
    if (node == NULL && service == NULL) {
        return EAI_NONAME;
    }
    if (socktype != 0 && socktype != SOCK_STREAM && socktype != SOCK_DGRAM) {
        return EAI_SOCKTYPE;
    }
    if (socktype == 0) {
        socktype = protocol == IPPROTO_UDP ? SOCK_DGRAM : SOCK_STREAM;
    }
    if (protocol == 0) {
        protocol = socktype == SOCK_DGRAM ? IPPROTO_UDP : IPPROTO_TCP;
    }

    struct in_addr address;
    if (node == NULL) {
        /* Без имени: «любой» для того, кто слушает, «себя» — для того, кто
         * соединяется. */
        address.s_addr = htonl((flags & AI_PASSIVE) != 0 ? INADDR_ANY : INADDR_LOOPBACK);
    } else {
        int code = resolve(node, (flags & AI_NUMERICHOST) != 0, &address);
        if (code != 0) {
            return code;
        }
    }

    int port = 0;
    if (service != NULL) {
        char *end;
        long value = strtol(service, &end, 10);
        if (*service != '\0' && *end == '\0') {
            if (value < 0 || value > 65535) {
                return EAI_SERVICE;
            }
            port = (int)value;
        } else if ((flags & AI_NUMERICSERV) != 0 || (port = service_port(service)) < 0) {
            return EAI_SERVICE;
        }
    }

    /* Одно место под всё: запись, адрес и каноническое имя — освобождает их
     * `freeaddrinfo` одним `free`. */
    size_t name_length = (flags & AI_CANONNAME) != 0 && node != NULL ? strlen(node) + 1 : 0;
    struct addrinfo *info = calloc(1, sizeof(*info) + sizeof(struct sockaddr_in) + name_length);
    if (info == NULL) {
        return EAI_MEMORY;
    }
    struct sockaddr_in *socket_address = (struct sockaddr_in *)(info + 1);
    socket_address->sin_family = AF_INET;
    socket_address->sin_port = htons((uint16_t)port);
    socket_address->sin_addr = address;
    info->ai_family = AF_INET;
    info->ai_socktype = socktype;
    info->ai_protocol = protocol;
    info->ai_addrlen = sizeof(*socket_address);
    info->ai_addr = (struct sockaddr *)socket_address;
    if (name_length != 0) {
        info->ai_canonname = (char *)(socket_address + 1);
        memcpy(info->ai_canonname, node, name_length);
    }
    *result = info;
    return 0;
}

void freeaddrinfo(struct addrinfo *list) {
    while (list != NULL) {
        struct addrinfo *next = list->ai_next;
        free(list);
        list = next;
    }
}

const char *gai_strerror(int code) {
    switch (code) {
    case EAI_NONAME:
        return "name does not resolve";
    case EAI_AGAIN:
        return "name server did not answer, try again";
    case EAI_FAIL:
        return "name lookup failed";
    case EAI_FAMILY:
        return "address family not supported (IPv4 only)";
    case EAI_SOCKTYPE:
        return "socket type not supported";
    case EAI_SERVICE:
        return "service not supported";
    case EAI_MEMORY:
        return "out of memory";
    default:
        return "address lookup failed";
    }
}

/* ── Интерфейсы (фаза 66a) ─────────────────────────────────────────────── */

/* Один интерфейс — карта, которую поднял `net` ядра, с адресом из
 * `SYS_NETINFO`. Сети нет или адреса ещё нет (DHCP не ответил) — список
 * пуст, и это успех: интерфейсов с адресом действительно нет. Всё лежит
 * одним куском, и `freeifaddrs` отдаёт его одним `free`. */
struct one_interface {
    struct ifaddrs entry;
    struct sockaddr_in address;
    struct sockaddr_in netmask;
    struct sockaddr_in broadcast;
    char name[IF_NAMESIZE];
};

int getifaddrs(struct ifaddrs **list) {
    if (list == NULL) {
        errno = EINVAL;
        return -1;
    }
    *list = NULL;
    struct freeos_netinfo net;
    long code = freeos_syscall(SYS_NETINFO, (long)&net, sizeof(net), 0);
    if (code < 0 || !net.present || net.address == 0) {
        return 0;
    }
    struct one_interface *one = calloc(1, sizeof(*one));
    if (one == NULL) {
        errno = ENOMEM;
        return -1;
    }
    strcpy(one->name, "eth0");
    one->address.sin_family = AF_INET;
    one->address.sin_addr.s_addr = htonl(net.address);
    one->netmask.sin_family = AF_INET;
    one->netmask.sin_addr.s_addr = htonl(net.netmask);
    one->broadcast.sin_family = AF_INET;
    one->broadcast.sin_addr.s_addr = htonl(net.address | ~net.netmask);
    one->entry.ifa_name = one->name;
    one->entry.ifa_flags = IFF_UP | IFF_RUNNING | IFF_BROADCAST;
    one->entry.ifa_addr = (struct sockaddr *)&one->address;
    one->entry.ifa_netmask = (struct sockaddr *)&one->netmask;
    one->entry.ifa_broadaddr = (struct sockaddr *)&one->broadcast;
    *list = &one->entry;
    return 0;
}

void freeifaddrs(struct ifaddrs *list) {
    free(list);
}

unsigned int if_nametoindex(const char *name) {
    return name != NULL && strcmp(name, "eth0") == 0 ? 1 : 0;
}

char *if_indextoname(unsigned int index, char *name) {
    if (index != 1) {
        errno = ENXIO;
        return NULL;
    }
    strcpy(name, "eth0");
    return name;
}
