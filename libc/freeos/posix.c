/* Слой POSIX, которого просит чужая среда исполнения (фаза 58).
 *
 * Список — не из головы, а из первой пробной сборки Mono 6.14.1 нашим
 * набором: семафоры, динамическая загрузка (отказом), журнал, сокеты
 * (объявления и отказ), разбор адресов IPv4. Правило то же, что у всего слоя
 * ОС: **нереализованное возвращает ошибку, а не притворяется успехом.** */

#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
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
#include <sys/utsname.h>
#include <sys/utime.h>
#include <syslog.h>
#include <time.h>
#include <unistd.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <sys/filio.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <termios.h>

#include "freeos-internal.h"
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

/* ── Динамическая загрузка: по таблице экспортов (фаза 60) ───────────────── */

static const char NO_LIBRARY[] = "FreeOS links programs statically; no such library is built in";
static const char NO_SYMBOL[] = "FreeOS links programs statically; no such symbol is built in";
static const char *dl_error;

/* «Библиотека» — сама программа: поиск во всей таблице. Отдельный объект, а
 * не `NULL`: `NULL` из `dlopen` значит отказ. */
static const struct freeos_export dl_self = {NULL, NULL, NULL};

/* Имя библиотеки без каталога, приставки `lib` и окончания. */
static void dl_short_name(const char *path, char *out, size_t size) {
    const char *base = strrchr(path, '/');
    base = base != NULL ? base + 1 : path;
    if (strncmp(base, "lib", 3) == 0 && base[3] != '\0') {
        base += 3;
    }
    snprintf(out, size, "%s", base);
    static const char *const suffixes[] = {".so", ".dll", ".dylib"};
    size_t length = strlen(out);
    for (size_t i = 0; i < sizeof(suffixes) / sizeof(suffixes[0]); i++) {
        size_t cut = strlen(suffixes[i]);
        if (length > cut && strcmp(out + length - cut, suffixes[i]) == 0) {
            out[length - cut] = '\0';
            break;
        }
    }
}

/* Ручка библиотеки — её первая запись в таблице: по ней `dlsym` знает имя. */
void *dlopen(const char *path, int flags) {
    (void)flags;
    if (path == NULL) {
        return (void *)&dl_self;
    }
    char name[256];
    dl_short_name(path, name, sizeof(name));
    for (const struct freeos_export *entry = freeos_exports; entry != NULL && entry->name != NULL; entry++) {
        if (strcmp(entry->library, name) == 0) {
            return (void *)entry;
        }
    }
    dl_error = NO_LIBRARY;
    return NULL;
}

void *dlsym(void *handle, const char *name) {
    const struct freeos_export *library = handle;
    int everywhere = library == NULL || library == &dl_self;
    for (const struct freeos_export *entry = freeos_exports; entry != NULL && entry->name != NULL; entry++) {
        if ((everywhere || strcmp(entry->library, library->library) == 0) && strcmp(entry->name, name) == 0) {
            return entry->address;
        }
    }
    dl_error = NO_SYMBOL;
    return NULL;
}

/* Закрывать нечего: «библиотека» — часть программы и живёт вместе с ней. */
int dlclose(void *handle) {
    (void)handle;
    return 0;
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


/* ── Личность (фаза 58b) ─────────────────────────────────────────────────── */

/* Действующая личность у нас совпадает с настоящей: программы с `setuid` в
 * системе нет, и права запуска — свойство программы, а не бит файла. */
uid_t getuid(void) { return (uid_t)freeos_syscall(SYS_GETUID, 0, 0, 0); }
uid_t geteuid(void) { return getuid(); }
gid_t getgid(void) { return (gid_t)freeos_syscall(SYS_GETGID, 0, 0, 0); }
gid_t getegid(void) { return getgid(); }

/* ── Пределы ─────────────────────────────────────────────────────────────── */

/* Три предела у нас настоящие и отвечают правдой: стек — тот, что выдало ядро
 * этому потоку (мегабайт у главного, фаза 58b), открытые файлы — стандартные
 * три и восемь мест таблицы. Остальные ядро не ставит, и ответ «без предела» —
 * тоже правда. Поменять предел нечем: `setrlimit` — `ENOSYS`. */
int getrlimit(int resource, struct rlimit *limit) {
    rlim_t value = RLIM_INFINITY;
    switch (resource) {
    case RLIMIT_STACK: {
        pthread_attr_t attr;
        void *low;
        size_t size;
        if (pthread_getattr_np(pthread_self(), &attr) != 0 || pthread_attr_getstack(&attr, &low, &size) != 0) {
            errno = EINVAL;
            return -1;
        }
        value = (rlim_t)size;
        break;
    }
    case RLIMIT_NOFILE:
        value = 3 + FREEOS_MAX_OPEN_FILES;
        break;
    case RLIMIT_CPU:
    case RLIMIT_FSIZE:
    case RLIMIT_DATA:
    case RLIMIT_CORE:
    case RLIMIT_AS:
        break;
    default:
        errno = EINVAL;
        return -1;
    }
    limit->rlim_cur = value;
    limit->rlim_max = value;
    return 0;
}

int setrlimit(int resource, const struct rlimit *limit) {
    (void)resource;
    (void)limit;
    errno = ENOSYS;
    return -1;
}

/* ── Сигналы ─────────────────────────────────────────────────────────────── */

/* Ядро сигналов не посылает вовсе: отказ страницы снимает программу так, как
 * это сделал бы обработчик по умолчанию, а остановить поток сборщику мусора
 * Mono не нужно — он собран с кооперативной остановкой.
 *
 * Поэтому `sigaction` ведёт таблицу обработчиков честно — записать, прочитать
 * прежний, отказать на `SIGKILL` и `SIGSTOP`, — но позван обработчик не будет
 * никогда. Отказать здесь `ENOSYS` было бы хуже, а не честнее: Mono ставит
 * обработчики на старте под `g_assert` и без таблицы не запускается, хотя ни
 * одного сигнала в её режиме не ждёт. */
static struct sigaction handlers[NSIG];

int sigaction(int signal, const struct sigaction *action, struct sigaction *old) {
    if (signal <= 0 || signal >= NSIG || ((signal == SIGKILL || signal == SIGSTOP) && action != NULL)) {
        errno = EINVAL;
        return -1;
    }
    if (old != NULL) {
        *old = handlers[signal];
    }
    if (action != NULL) {
        handlers[signal] = *action;
    }
    return 0;
}

/* Ждать сигнала, который никто не пошлёт, — значит спать вечно. Ответ вместо
 * вечного сна: вызывающему лучше узнать, что ждать нечего. */
int sigsuspend(const sigset_t *mask) {
    (void)mask;
    errno = ENOSYS;
    return -1;
}

/* ── Ожидание дескрипторов ───────────────────────────────────────────────── */

/* `poll` поверх `SYS_POLL`. Сокетов ядро здесь не принимает (у них своё
 * пространство номеров, см. договор) — так же, как и сокеты в этой libc пока
 * отвечают `ENOSYS`. */
int poll(struct pollfd *fds, nfds_t count, int timeout) {
    enum { ON_STACK = 16 };
    struct freeos_pollfd local[ON_STACK];
    struct freeos_pollfd *table = local;
    if (count > ON_STACK) {
        table = malloc(count * sizeof(*table));
        if (table == NULL) {
            errno = ENOMEM;
            return -1;
        }
    }
    for (nfds_t at = 0; at < count; at++) {
        table[at].fd = fds[at].fd;
        table[at].wanted = ((fds[at].events & POLLIN) ? FREEOS_POLL_IN : 0) |
                           ((fds[at].events & POLLOUT) ? FREEOS_POLL_OUT : 0);
        table[at].ready = 0;
    }
    long wait = timeout < 0 ? FREEOS_POLL_FOREVER : timeout;
    long ready = freeos_syscall(SYS_POLL, (long)table, (long)count, wait);
    if (ready >= 0) {
        for (nfds_t at = 0; at < count; at++) {
            uint32_t got = table[at].ready;
            fds[at].revents = (short)(((got & FREEOS_POLL_IN) ? POLLIN : 0) | ((got & FREEOS_POLL_OUT) ? POLLOUT : 0) |
                                      ((got & FREEOS_POLL_HUP) ? POLLHUP : 0) |
                                      ((got & FREEOS_POLL_BAD) ? POLLNVAL : 0));
        }
    }
    if (table != local) {
        free(table);
    }
    return ready < 0 ? freeos_set_errno(ready) : (int)ready;
}

/* `select` — тот же `poll`, разложенный по трём наборам. Исключительных
 * состояний у наших дескрипторов нет, третий набор всегда пуст на выходе. */
int select(int count, fd_set *readable, fd_set *writable, fd_set *exceptional, struct timeval *timeout) {
    if (count < 0 || count > FD_SETSIZE) {
        errno = EINVAL;
        return -1;
    }
    struct pollfd table[FD_SETSIZE];
    nfds_t used = 0;
    for (int fd = 0; fd < count; fd++) {
        short events = (short)(((readable != NULL && FD_ISSET(fd, readable)) ? POLLIN : 0) |
                               ((writable != NULL && FD_ISSET(fd, writable)) ? POLLOUT : 0));
        if (events != 0) {
            table[used].fd = fd;
            table[used].events = events;
            table[used].revents = 0;
            used++;
        }
    }
    int wait = -1;
    if (timeout != NULL) {
        long long ms = (long long)timeout->tv_sec * 1000 + timeout->tv_usec / 1000;
        wait = ms > INT_MAX ? INT_MAX : (int)ms;
    }
    int answer = poll(table, used, wait);
    if (answer < 0) {
        return -1;
    }
    if (readable != NULL) {
        FD_ZERO(readable);
    }
    if (writable != NULL) {
        FD_ZERO(writable);
    }
    if (exceptional != NULL) {
        FD_ZERO(exceptional);
    }
    int marked = 0;
    for (nfds_t at = 0; at < used; at++) {
        if ((table[at].revents & POLLNVAL) != 0) {
            errno = EBADF;
            return -1;
        }
        if (readable != NULL && (table[at].revents & (POLLIN | POLLHUP)) != 0) {
            FD_SET(table[at].fd, readable);
            marked++;
        }
        if (writable != NULL && (table[at].revents & POLLOUT) != 0) {
            FD_SET(table[at].fd, writable);
            marked++;
        }
    }
    return marked;
}

/* ── Терминал ────────────────────────────────────────────────────────────── */

/* У терминала два режима (`SYS_TTYMODE`): строка с эхом и нажатие без эха.
 * `termios` описывает больше, и то, что в два режима не ложится, не
 * выдумывается: режим выбирается по `ICANON`, а `tcgetattr` после этого
 * отвечает тем, что стоит на самом деле. Режим принадлежит программе — ядро
 * вернёт строчный само, когда она закончится. */
static int raw_mode;

static int require_tty(int fd) {
    if (!isatty(fd)) {
        errno = ENOTTY;
        return -1;
    }
    return 0;
}

int tcgetattr(int fd, struct termios *out) {
    if (require_tty(fd) != 0) {
        return -1;
    }
    memset(out, 0, sizeof(*out));
    out->c_lflag = raw_mode ? 0 : (ICANON | ECHO);
    return 0;
}

int tcsetattr(int fd, int when, const struct termios *in) {
    (void)when;
    if (require_tty(fd) != 0) {
        return -1;
    }
    int raw = (in->c_lflag & ICANON) == 0;
    long code = freeos_syscall(SYS_TTYMODE, raw ? FREEOS_TTY_RAW : FREEOS_TTY_LINE, 0, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    raw_mode = raw;
    return 0;
}

/* Сбросить непрочитанный ввод: вычитать всё, что готово прямо сейчас. Буфера
 * вывода у терминала нет — он рисуется сразу, сбрасывать там нечего. */
int tcflush(int fd, int queue) {
    if (require_tty(fd) != 0) {
        return -1;
    }
    if (queue == TCIFLUSH || queue == TCIOFLUSH) {
        struct pollfd one = {fd, POLLIN, 0};
        char scratch[64];
        while (poll(&one, 1, 0) > 0 && (one.revents & POLLIN) != 0) {
            if (read(fd, scratch, sizeof(scratch)) <= 0) {
                break;
            }
        }
    }
    return 0;
}

/* `ioctl` — только `FIONREAD`, и отвечает он «хоть один байт» или «ни
 * одного»: сколько именно лежит в канале или в терминале, ядро наружу не
 * говорит, а `poll` отвечает на главный вопрос — будет ли `read` ждать.
 * Преуменьшить здесь безопасно (читающий прочтёт не всё за раз), преувеличить —
 * нет. */
int ioctl(int fd, unsigned long op, void *param) {
    if (op == FIONREAD) {
        struct pollfd one = {fd, POLLIN, 0};
        if (poll(&one, 1, 0) < 0) {
            return -1;
        }
        if ((one.revents & POLLNVAL) != 0) {
            errno = EBADF;
            return -1;
        }
        *(int *)param = (one.revents & POLLIN) != 0 ? 1 : 0;
        return 0;
    }
    errno = ENOTTY;
    return -1;
}

/* ── Советы о памяти ─────────────────────────────────────────────────────── */

/* Совет по POSIX — совет: его вправе не исполнить, содержимое страниц он не
 * меняет. Проверяются только аргументы. */
int posix_madvise(void *addr, size_t len, int advice) {
    (void)len;
    if (((uintptr_t)addr & 4095) != 0) {
        return EINVAL;
    }
    if (advice < POSIX_MADV_NORMAL || advice > POSIX_MADV_DONTNEED) {
        return EINVAL;
    }
    return 0;
}

/* `sysconf` (фаза 60): память и процессоры — из `SYS_SYSINFO`, остальное — как
 * у picolibc (у неё `sysconf` — слабое имя её `__fallback_sysconf`). Кадр ядра
 * и страница — одно и то же, 4 КиБ. */
long __fallback_sysconf(int name);

long sysconf(int name) {
    switch (name) {
    case _SC_NPROCESSORS_CONF:
    case _SC_NPROCESSORS_ONLN:
    case _SC_PHYS_PAGES:
    case _SC_AVPHYS_PAGES: {
        struct freeos_sysinfo info;
        long code = freeos_syscall(SYS_SYSINFO, (long)&info, 0, 0);
        if (code < 0) {
            return freeos_set_errno(code);
        }
        if (name == _SC_PHYS_PAGES) {
            return (long)info.frames_total;
        }
        if (name == _SC_AVPHYS_PAGES) {
            return (long)info.frames_free;
        }
        return info.cpus > 0 ? (long)info.cpus : 1;
    }
    default:
        return __fallback_sysconf(name);
    }
}

/* `uname` (фаза 61b). Имя системы и процессор известны при сборке; версия —
 * из `/os-release`, который пишет установщик и образ обновления (`version=…`).
 * У «живой» системы с носителя этого файла нет, и версия там — `unknown`:
 * выдумать число значило бы соврать тому, кто по нему что-то решает. Имени
 * машины у системы пока нет вовсе — отвечаем её же именем, как Linux без
 * настроенного имени. */
static void uname_release(char *out, size_t size) {
    snprintf(out, size, "unknown");
    int fd = open("/os-release", O_RDONLY);
    if (fd < 0) {
        return;
    }
    char text[256];
    ssize_t got = read(fd, text, sizeof(text) - 1);
    close(fd);
    if (got <= 0) {
        return;
    }
    text[got] = '\0';
    for (char *line = text; line != NULL && *line != '\0';) {
        char *next = strchr(line, '\n');
        if (next != NULL) {
            *next++ = '\0';
        }
        if (strncmp(line, "version=", 8) == 0 && line[8] != '\0') {
            snprintf(out, size, "%s", line + 8);
            return;
        }
        line = next;
    }
}

int uname(struct utsname *name) {
    if (name == NULL) {
        errno = EFAULT;
        return -1;
    }
    memset(name, 0, sizeof(*name));
    snprintf(name->sysname, sizeof(name->sysname), "FreeOS");
    snprintf(name->nodename, sizeof(name->nodename), "freeos");
    uname_release(name->release, sizeof(name->release));
    snprintf(name->version, sizeof(name->version), "%s", name->release);
#if defined(__x86_64__)
    snprintf(name->machine, sizeof(name->machine), "x86_64");
#elif defined(__aarch64__)
    snprintf(name->machine, sizeof(name->machine), "aarch64");
#else
#error "uname: unknown architecture"
#endif
    return 0;
}

/* `madvise` — тот же совет, но с ответом через `errno` (фаза 60). */
int madvise(void *addr, size_t len, int advice) {
    int error = posix_madvise(addr, len, advice);
    if (error != 0) {
        errno = error;
        return -1;
    }
    return 0;
}

/* `msync`: отображения файлов у нас только на чтение, а у безымянной памяти
 * файла нет, — записывать назад нечего ни в одном случае. Проверяется только
 * выравнивание, как требует POSIX. */
int msync(void *addr, size_t len, int flags) {
    (void)len;
    (void)flags;
    if (((uintptr_t)addr & 4095) != 0) {
        errno = EINVAL;
        return -1;
    }
    return 0;
}

/* Закрепить страницы в памяти ядро не умеет: страницы отображённых файлов оно
 * вправе выбросить. Отказ, а не успех: обещание «не уйдёт на диск» давать
 * нечем. */
int mlock(const void *addr, size_t len) {
    (void)addr;
    (void)len;
    errno = ENOSYS;
    return -1;
}

int munlock(const void *addr, size_t len) {
    (void)addr;
    (void)len;
    errno = ENOSYS;
    return -1;
}
