/* Слой POSIX, которого просит чужая среда исполнения (фаза 58).
 *
 * Список — не из головы, а из первой пробной сборки Mono 6.14.1 нашим
 * набором: семафоры, динамическая загрузка (отказом), журнал, сокеты
 * (объявления и отказ), разбор адресов IPv4. Правило то же, что у всего слоя
 * ОС: **нереализованное возвращает ошибку, а не притворяется успехом.** */

#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <limits.h>
#include <netdb.h>
#include <netinet/in.h>
#include <semaphore.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/uio.h>
#include <sys/utime.h>
#include <syslog.h>
#include <time.h>
#include <unistd.h>

#include "freeos-syscall.h"

/* ── Семафоры ────────────────────────────────────────────────────────────── */

/* Число свободных единиц лежит в слове, и на этом же слове спят. Взять — это
 * уменьшить ненулевое сравнением-с-обменом; нуль — уснуть, пока он нуль. Кто
 * кладёт единицу, будит одного — если кто-то ждёт (`waiters`), иначе ядро не
 * зовётся вовсе. */

int sem_init(sem_t *sem, int shared, unsigned int value) {
    if (shared != 0) {
        /* Общих между процессами семафоров нет: процессы не делят памяти. */
        errno = ENOSYS;
        return -1;
    }
    if (value > (unsigned int)INT_MAX) {
        errno = EINVAL;
        return -1;
    }
    sem->value = value;
    sem->waiters = 0;
    return 0;
}

int sem_destroy(sem_t *sem) {
    (void)sem;
    return 0;
}

int sem_trywait(sem_t *sem) {
    unsigned int seen = __atomic_load_n(&sem->value, __ATOMIC_ACQUIRE);
    while (seen != 0) {
        if (__atomic_compare_exchange_n(&sem->value, &seen, seen - 1, 1, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
            return 0;
        }
    }
    errno = EAGAIN;
    return -1;
}

/* Сколько миллисекунд до срока по часам реального времени (так велит POSIX для
 * `sem_timedwait`), округлённо вверх; ноль — срок прошёл. */
static unsigned long millis_left(const struct timespec *deadline) {
    struct timespec now;
    clock_gettime(CLOCK_REALTIME, &now);
    long long left = (long long)(deadline->tv_sec - now.tv_sec) * 1000000000LL +
                     (long long)(deadline->tv_nsec - now.tv_nsec);
    return left <= 0 ? 0 : (unsigned long)((left + 999999) / 1000000);
}

static int sem_wait_until(sem_t *sem, const struct timespec *deadline) {
    for (;;) {
        if (sem_trywait(sem) == 0) {
            return 0;
        }
        unsigned long timeout = 0;
        if (deadline != NULL) {
            timeout = millis_left(deadline);
            if (timeout == 0) {
                errno = ETIMEDOUT;
                return -1;
            }
        }
        __atomic_fetch_add(&sem->waiters, 1, __ATOMIC_ACQ_REL);
        freeos_syscall(SYS_FUTEX_WAIT, (long)&sem->value, 0, (long)timeout);
        __atomic_fetch_sub(&sem->waiters, 1, __ATOMIC_ACQ_REL);
    }
}

int sem_wait(sem_t *sem) { return sem_wait_until(sem, NULL); }

int sem_timedwait(sem_t *sem, const struct timespec *deadline) {
    if (deadline->tv_nsec < 0 || deadline->tv_nsec >= 1000000000L) {
        errno = EINVAL;
        return -1;
    }
    return sem_wait_until(sem, deadline);
}

int sem_post(sem_t *sem) {
    unsigned int before = __atomic_fetch_add(&sem->value, 1, __ATOMIC_RELEASE);
    if (before == (unsigned int)INT_MAX) {
        __atomic_fetch_sub(&sem->value, 1, __ATOMIC_RELAXED);
        errno = EOVERFLOW;
        return -1;
    }
    if (__atomic_load_n(&sem->waiters, __ATOMIC_ACQUIRE) != 0) {
        freeos_syscall(SYS_FUTEX_WAKE, (long)&sem->value, 1, 0);
    }
    return 0;
}

int sem_getvalue(sem_t *sem, int *value) {
    *value = (int)__atomic_load_n(&sem->value, __ATOMIC_ACQUIRE);
    return 0;
}

sem_t *sem_open(const char *name, int flags, ...) {
    (void)name;
    (void)flags;
    errno = ENOSYS;
    return SEM_FAILED;
}

int sem_close(sem_t *sem) {
    (void)sem;
    errno = ENOSYS;
    return -1;
}

int sem_unlink(const char *name) {
    (void)name;
    errno = ENOSYS;
    return -1;
}

/* ── Динамическая загрузка: отказ ────────────────────────────────────────── */

static const char NO_LOADER[] = "FreeOS links programs statically; there is no dynamic loader";
static const char *dl_error;

void *dlopen(const char *path, int flags) {
    (void)path;
    (void)flags;
    dl_error = NO_LOADER;
    return NULL;
}

void *dlsym(void *handle, const char *name) {
    (void)handle;
    (void)name;
    dl_error = NO_LOADER;
    return NULL;
}

int dlclose(void *handle) {
    (void)handle;
    dl_error = NO_LOADER;
    return -1;
}

/* Сообщение отдаётся один раз и сбрасывается — так требует POSIX. */
char *dlerror(void) {
    const char *error = dl_error;
    dl_error = NULL;
    return (char *)error;
}

/* ── Журнал ──────────────────────────────────────────────────────────────── */

static const char *log_ident;
static int log_mask = 0xff;

void openlog(const char *ident, int option, int facility) {
    (void)option;
    (void)facility;
    log_ident = ident;
}

void closelog(void) { log_ident = NULL; }

int setlogmask(int mask) {
    int previous = log_mask;
    if (mask != 0) {
        log_mask = mask;
    }
    return previous;
}

void vsyslog(int priority, const char *format, va_list args) {
    if ((log_mask & LOG_MASK(priority & 7)) == 0) {
        return;
    }
    fprintf(stderr, "%s: <%d> ", log_ident != NULL ? log_ident : "syslog", priority & 7);
    vfprintf(stderr, format, args);
    fputc('\n', stderr);
}

void syslog(int priority, const char *format, ...) {
    va_list args;
    va_start(args, format);
    vsyslog(priority, format, args);
    va_end(args);
}

/* ── Сокеты: пока отказ ──────────────────────────────────────────────────── */

/* Все до одного — `ENOSYS`, пока не привязаны к `SYS_SOCKET` и соседям. Не
 * `EAFNOSUPPORT` и не нуль: программа должна понять, что сети у неё нет
 * совсем, а не что ей попался не тот адрес. */
static int no_sockets(void) {
    errno = ENOSYS;
    return -1;
}

int socket(int domain, int type, int protocol) {
    (void)domain, (void)type, (void)protocol;
    return no_sockets();
}
int bind(int fd, const struct sockaddr *address, socklen_t length) {
    (void)fd, (void)address, (void)length;
    return no_sockets();
}
int connect(int fd, const struct sockaddr *address, socklen_t length) {
    (void)fd, (void)address, (void)length;
    return no_sockets();
}
int listen(int fd, int backlog) {
    (void)fd, (void)backlog;
    return no_sockets();
}
int accept(int fd, struct sockaddr *address, socklen_t *length) {
    (void)fd, (void)address, (void)length;
    return no_sockets();
}
ssize_t send(int fd, const void *buf, size_t len, int flags) {
    (void)fd, (void)buf, (void)len, (void)flags;
    return no_sockets();
}
ssize_t recv(int fd, void *buf, size_t len, int flags) {
    (void)fd, (void)buf, (void)len, (void)flags;
    return no_sockets();
}
ssize_t sendto(int fd, const void *buf, size_t len, int flags, const struct sockaddr *to, socklen_t length) {
    (void)fd, (void)buf, (void)len, (void)flags, (void)to, (void)length;
    return no_sockets();
}
ssize_t recvfrom(int fd, void *buf, size_t len, int flags, struct sockaddr *from, socklen_t *length) {
    (void)fd, (void)buf, (void)len, (void)flags, (void)from, (void)length;
    return no_sockets();
}
ssize_t sendmsg(int fd, const struct msghdr *message, int flags) {
    (void)fd, (void)message, (void)flags;
    return no_sockets();
}
ssize_t recvmsg(int fd, struct msghdr *message, int flags) {
    (void)fd, (void)message, (void)flags;
    return no_sockets();
}
int getsockopt(int fd, int level, int name, void *value, socklen_t *length) {
    (void)fd, (void)level, (void)name, (void)value, (void)length;
    return no_sockets();
}
int setsockopt(int fd, int level, int name, const void *value, socklen_t length) {
    (void)fd, (void)level, (void)name, (void)value, (void)length;
    return no_sockets();
}
int getsockname(int fd, struct sockaddr *address, socklen_t *length) {
    (void)fd, (void)address, (void)length;
    return no_sockets();
}
int getpeername(int fd, struct sockaddr *address, socklen_t *length) {
    (void)fd, (void)address, (void)length;
    return no_sockets();
}
int shutdown(int fd, int how) {
    (void)fd, (void)how;
    return no_sockets();
}
int socketpair(int domain, int type, int protocol, int fds[2]) {
    (void)domain, (void)type, (void)protocol, (void)fds;
    return no_sockets();
}

/* ── Ввод-вывод кусками ──────────────────────────────────────────────────── */

/* Кусок за куском; короткий ответ на куске кончает пачку — дальше читать или
 * писать значило бы оставить дыру там, где программа ждёт сплошные данные. */
ssize_t readv(int fd, const struct iovec *pieces, int count) {
    ssize_t total = 0;
    for (int index = 0; index < count; index++) {
        ssize_t done = read(fd, pieces[index].iov_base, pieces[index].iov_len);
        if (done < 0) {
            return total > 0 ? total : -1;
        }
        total += done;
        if ((size_t)done < pieces[index].iov_len) {
            break;
        }
    }
    return total;
}

ssize_t writev(int fd, const struct iovec *pieces, int count) {
    ssize_t total = 0;
    for (int index = 0; index < count; index++) {
        ssize_t done = write(fd, pieces[index].iov_base, pieces[index].iov_len);
        if (done < 0) {
            return total > 0 ? total : -1;
        }
        total += done;
        if ((size_t)done < pieces[index].iov_len) {
            break;
        }
    }
    return total;
}

/* ── Адреса IPv4 ─────────────────────────────────────────────────────────── */

/* Четыре десятичных числа через точку, каждое до 255, — только эта запись.
 * Старые формы `inet_aton` (восьмеричные, «a.b» с дробным хвостом) не
 * принимаются: их не пишет никто, а неверно понятый адрес хуже отказа. */
int inet_aton(const char *text, struct in_addr *out) {
    uint32_t value = 0;
    for (int part = 0; part < 4; part++) {
        if (*text < '0' || *text > '9') {
            return 0;
        }
        unsigned int number = 0;
        int digits = 0;
        while (*text >= '0' && *text <= '9') {
            number = number * 10 + (unsigned int)(*text++ - '0');
            if (++digits > 3 || number > 255) {
                return 0;
            }
        }
        value = (value << 8) | number;
        if (part < 3 && *text++ != '.') {
            return 0;
        }
    }
    if (*text != '\0') {
        return 0;
    }
    if (out != NULL) {
        out->s_addr = htonl(value);
    }
    return 1;
}

in_addr_t inet_addr(const char *text) {
    struct in_addr address;
    return inet_aton(text, &address) ? address.s_addr : INADDR_NONE;
}

char *inet_ntoa(struct in_addr address) {
    static char text[INET_ADDRSTRLEN];
    uint32_t value = ntohl(address.s_addr);
    snprintf(text, sizeof(text), "%u.%u.%u.%u", (unsigned)(value >> 24), (unsigned)((value >> 16) & 0xff),
             (unsigned)((value >> 8) & 0xff), (unsigned)(value & 0xff));
    return text;
}

int inet_pton(int family, const char *text, void *out) {
    if (family != AF_INET) {
        /* IPv6 у стека FreeOS нет — и разбирать его адреса незачем. */
        errno = EAFNOSUPPORT;
        return -1;
    }
    return inet_aton(text, (struct in_addr *)out);
}

const char *inet_ntop(int family, const void *address, char *out, socklen_t length) {
    if (family != AF_INET) {
        errno = EAFNOSUPPORT;
        return NULL;
    }
    const char *text = inet_ntoa(*(const struct in_addr *)address);
    if (strlen(text) + 1 > length) {
        errno = ENOSPC;
        return NULL;
    }
    strcpy(out, text);
    return out;
}

/* ── Время файла ─────────────────────────────────────────────────────────── */

/* Ядро времён файлу не ставит — ни доступа, ни изменения. `ENOSYS`, а не
 * нуль: программа, проверяющая время после `utime`, иначе нашла бы там не то,
 * что поставила, и не поняла бы почему. */
int utime(const char *path, const struct utimbuf *times) {
    (void)path;
    (void)times;
    errno = ENOSYS;
    return -1;
}

/* ── Имена: протоколы, хосты, адреса ─────────────────────────────────────── */

int h_errno;

static struct protoent PROTOCOLS[] = {
    {"ip", NULL, IPPROTO_IP},
    {"icmp", NULL, IPPROTO_ICMP},
    {"tcp", NULL, IPPROTO_TCP},
    {"udp", NULL, IPPROTO_UDP},
};

struct protoent *getprotobyname(const char *name) {
    for (size_t index = 0; index < sizeof(PROTOCOLS) / sizeof(PROTOCOLS[0]); index++) {
        if (strcmp(PROTOCOLS[index].p_name, name) == 0) {
            return &PROTOCOLS[index];
        }
    }
    return NULL;
}

struct protoent *getprotobynumber(int proto) {
    for (size_t index = 0; index < sizeof(PROTOCOLS) / sizeof(PROTOCOLS[0]); index++) {
        if (PROTOCOLS[index].p_proto == proto) {
            return &PROTOCOLS[index];
        }
    }
    return NULL;
}

struct servent *getservbyname(const char *name, const char *proto) {
    (void)name;
    (void)proto;
    return NULL;
}

/* Хост — только готовый адрес: разрешения имён через сеть здесь пока нет. */
struct hostent *gethostbyname(const char *name) {
    static struct in_addr address;
    static char *list[2] = {(char *)&address, NULL};
    static char *aliases[1] = {NULL};
    static struct hostent host;
    if (!inet_aton(name, &address)) {
        h_errno = HOST_NOT_FOUND;
        return NULL;
    }
    host.h_name = (char *)name;
    host.h_aliases = aliases;
    host.h_addrtype = AF_INET;
    host.h_length = sizeof(address);
    host.h_addr_list = list;
    return &host;
}

struct hostent *gethostbyaddr(const void *address, socklen_t length, int family) {
    (void)address;
    (void)length;
    (void)family;
    h_errno = HOST_NOT_FOUND;
    return NULL;
}

int getaddrinfo(const char *node, const char *service, const struct addrinfo *hints, struct addrinfo **result) {
    if (hints != NULL && hints->ai_family != AF_UNSPEC && hints->ai_family != AF_INET) {
        return EAI_FAMILY;
    }
    struct in_addr address = {INADDR_ANY};
    if (node != NULL && !inet_aton(node, &address)) {
        return EAI_NONAME;
    }
    int port = 0;
    if (service != NULL) {
        char *end;
        long value = strtol(service, &end, 10);
        if (*end != '\0' || value < 0 || value > 65535) {
            return EAI_SERVICE;
        }
        port = (int)value;
    }
    struct addrinfo *info = calloc(1, sizeof(*info) + sizeof(struct sockaddr_in));
    if (info == NULL) {
        return EAI_MEMORY;
    }
    struct sockaddr_in *socket_address = (struct sockaddr_in *)(info + 1);
    socket_address->sin_family = AF_INET;
    socket_address->sin_port = htons((uint16_t)port);
    socket_address->sin_addr = address;
    info->ai_family = AF_INET;
    info->ai_socktype = hints != NULL ? hints->ai_socktype : 0;
    info->ai_protocol = hints != NULL ? hints->ai_protocol : 0;
    info->ai_addrlen = sizeof(*socket_address);
    info->ai_addr = (struct sockaddr *)socket_address;
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
        return "name does not resolve (only numeric IPv4 addresses are understood)";
    case EAI_FAMILY:
        return "address family not supported";
    case EAI_SERVICE:
        return "service not supported";
    case EAI_MEMORY:
        return "out of memory";
    default:
        return "address lookup failed";
    }
}

int getnameinfo(const struct sockaddr *address, socklen_t length, char *host, socklen_t host_length, char *service,
                socklen_t service_length, int flags) {
    (void)flags;
    if (address->sa_family != AF_INET || length < sizeof(struct sockaddr_in)) {
        return EAI_FAMILY;
    }
    const struct sockaddr_in *in = (const struct sockaddr_in *)address;
    if (host != NULL && inet_ntop(AF_INET, &in->sin_addr, host, host_length) == NULL) {
        return EAI_SYSTEM;
    }
    if (service != NULL && snprintf(service, service_length, "%u", ntohs(in->sin_port)) >= (int)service_length) {
        return EAI_SYSTEM;
    }
    return 0;
}
