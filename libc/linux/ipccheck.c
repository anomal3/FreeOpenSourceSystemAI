/* Межпроцессное программы Linux на glibc (фаза 71e): сокеты AF_UNIX — пара,
 * пакеты, имя, передача дескриптора (SCM_RIGHTS); eventfd, timerfd, epoll;
 * memfd, MAP_SHARED между процессами, shm_open.
 *
 * Собирается, как linuxcheck, clang с sysroot Debian и едет в пакете
 * `debian-12.fpk` как `/usr/libexec/fossai/ipccheck`. Строки — `ipccheck: ok
 * <что>` или `ipccheck: FAILED <что>`; итог — `ipccheck: N failed`, код
 * выхода — то же N.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/timerfd.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(int good, const char *what) {
    printf("ipccheck: %s %s\n", good ? "ok" : "FAILED", what);
    fflush(stdout);
    if (!good) failed++;
}

static long long now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000LL + t.tv_nsec / 1000000;
}

static int exited_with(pid_t pid, int code) {
    int status = -1;
    return pid > 0 && waitpid(pid, &status, 0) == pid && WIFEXITED(status) && WEXITSTATUS(status) == code;
}

/* Передать дескриптор по сокету. */
static int send_fd(int sock, int fd) {
    char byte = 'F';
    struct iovec iov = {&byte, 1};
    char control[CMSG_SPACE(sizeof(int))];
    memset(control, 0, sizeof control);
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof control;
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    cmsg->cmsg_level = SOL_SOCKET;
    cmsg->cmsg_type = SCM_RIGHTS;
    cmsg->cmsg_len = CMSG_LEN(sizeof(int));
    memcpy(CMSG_DATA(cmsg), &fd, sizeof fd);
    return sendmsg(sock, &msg, 0) == 1 ? 0 : -1;
}

static int recv_fd(int sock) {
    char byte = 0;
    struct iovec iov = {&byte, 1};
    char control[CMSG_SPACE(sizeof(int))];
    struct msghdr msg = {0};
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control;
    msg.msg_controllen = sizeof control;
    if (recvmsg(sock, &msg, 0) != 1 || byte != 'F') return -1;
    struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
    if (!cmsg || cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) return -1;
    int fd;
    memcpy(&fd, CMSG_DATA(cmsg), sizeof fd);
    return fd;
}

int main(void) {
    printf("ipccheck: start\n");

    /* Пара потоковых сокетов в обе стороны, между процессами. */
    int pair[2];
    char text[64] = {0};
    int paired = socketpair(AF_UNIX, SOCK_STREAM, 0, pair) == 0;
    pid_t child = paired ? fork() : -1;
    if (child == 0) {
        close(pair[0]);
        char got[8] = {0};
        int good = read(pair[1], got, 4) == 4 && strcmp(got, "ping") == 0 && write(pair[1], "pong", 4) == 4;
        _exit(good ? 0 : 1);
    }
    if (paired) close(pair[1]);
    int talked = paired && write(pair[0], "ping", 4) == 4 && read(pair[0], text, 4) == 4 && strcmp(text, "pong") == 0;
    char after[4];
    int eof = paired && exited_with(child, 0) && read(pair[0], after, sizeof after) == 0;
    if (paired) close(pair[0]);
    check(talked && eof, "socketpair between processes, EOF when the peer ends");

    /* Пакеты: границы сохраняются. */
    int packets[2];
    int seq = socketpair(AF_UNIX, SOCK_SEQPACKET, 0, packets) == 0 && write(packets[0], "a", 1) == 1 &&
              write(packets[0], "bcd", 3) == 3;
    char one[8] = {0}, two[8] = {0};
    seq = seq && read(packets[1], one, sizeof one) == 1 && read(packets[1], two, sizeof two) == 3 &&
          strcmp(one, "a") == 0 && strcmp(two, "bcd") == 0;
    check(seq, "SOCK_SEQPACKET keeps message boundaries");

    /* Дескриптор через сокет: ребёнок получает конец канала и пишет в него. */
    int pipe_fds[2];
    int carried = pipe(pipe_fds) == 0;
    pid_t carrier = carried ? fork() : -1;
    if (carrier == 0) {
        close(packets[0]);
        int fd = recv_fd(packets[1]);
        _exit(fd >= 0 && write(fd, "via fd", 6) == 6 ? 0 : 1);
    }
    carried = carried && send_fd(packets[0], pipe_fds[1]) == 0;
    close(pipe_fds[1]);
    memset(text, 0, sizeof text);
    carried = carried && exited_with(carrier, 0) && read(pipe_fds[0], text, sizeof text - 1) == 6 &&
              strcmp(text, "via fd") == 0;
    close(pipe_fds[0]);
    close(packets[0]);
    close(packets[1]);
    check(carried, "SCM_RIGHTS passes a pipe to another process");

    /* Сокет по имени (абстрактному): слушает родитель, подключается ребёнок. */
    struct sockaddr_un addr;
    memset(&addr, 0, sizeof addr);
    addr.sun_family = AF_UNIX;
    const char name[] = "\0fossai-71e";
    memcpy(addr.sun_path, name, sizeof name - 1);
    socklen_t addr_len = offsetof(struct sockaddr_un, sun_path) + sizeof name - 1;
    int server = socket(AF_UNIX, SOCK_STREAM, 0);
    int named = server >= 0 && bind(server, (struct sockaddr *)&addr, addr_len) == 0 && listen(server, 4) == 0;
    pid_t client = named ? fork() : -1;
    if (client == 0) {
        int sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
        int good = sock >= 0 && connect(sock, (struct sockaddr *)&addr, addr_len) == 0 && write(sock, "hello", 5) == 5;
        _exit(good ? 0 : 1);
    }
    int conn = named ? accept(server, NULL, NULL) : -1;
    memset(text, 0, sizeof text);
    named = conn >= 0 && read(conn, text, sizeof text - 1) == 5 && strcmp(text, "hello") == 0 && exited_with(client, 0);
    struct ucred cred;
    socklen_t cred_len = sizeof cred;
    int peer = conn >= 0 && getsockopt(conn, SOL_SOCKET, SO_PEERCRED, &cred, &cred_len) == 0 && cred.pid == client;
    if (conn >= 0) close(conn);
    if (server >= 0) close(server);
    check(named, "bind, listen, accept and connect by an abstract name");
    check(peer, "SO_PEERCRED names the connecting process");

    /* eventfd. */
    int event = eventfd(0, EFD_NONBLOCK);
    uint64_t value = 3, sum = 0;
    int counted = event >= 0 && write(event, &value, 8) == 8;
    value = 4;
    counted = counted && write(event, &value, 8) == 8 && read(event, &sum, 8) == 8 && sum == 7;
    int empty = event >= 0 && read(event, &sum, 8) == -1 && errno == EAGAIN;
    check(counted && empty, "eventfd adds and resets; empty is EAGAIN");

    /* timerfd: однократный — чтение ждёт срока; повторяющийся считает. */
    int timer = timerfd_create(CLOCK_MONOTONIC, 0);
    struct itimerspec once = {{0, 0}, {0, 30000000}};
    long long before = now_ms();
    uint64_t ticks = 0;
    int timed = timer >= 0 && timerfd_settime(timer, 0, &once, NULL) == 0 && read(timer, &ticks, 8) == 8 &&
                ticks == 1 && now_ms() - before >= 30;
    struct itimerspec every = {{0, 10000000}, {0, 10000000}};
    timed = timed && timerfd_settime(timer, 0, &every, NULL) == 0;
    struct timespec nap = {0, 55000000};
    nanosleep(&nap, NULL);
    timed = timed && read(timer, &ticks, 8) == 8 && ticks >= 4;
    check(timed, "timerfd one-shot and interval");

    /* epoll: eventfd, сокет и таймер в одном наборе. */
    int ep = epoll_create1(EPOLL_CLOEXEC);
    int sockets[2];
    socketpair(AF_UNIX, SOCK_STREAM, 0, sockets);
    struct epoll_event watch = {0}, got[4];
    watch.events = EPOLLIN;
    watch.data.u64 = 111;
    int set = ep >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, event, &watch) == 0;
    watch.data.u64 = 222;
    set = set && epoll_ctl(ep, EPOLL_CTL_ADD, sockets[0], &watch) == 0;
    int quiet = set && epoll_wait(ep, got, 4, 0) == 0;
    write(sockets[1], "x", 1);
    int woke = epoll_wait(ep, got, 4, 1000) == 1 && got[0].data.u64 == 222 && (got[0].events & EPOLLIN);
    struct itimerspec soon = {{0, 0}, {0, 20000000}};
    timerfd_settime(timer, 0, &soon, NULL);
    watch.data.u64 = 333;
    epoll_ctl(ep, EPOLL_CTL_ADD, timer, &watch);
    char drop;
    read(sockets[0], &drop, 1);
    before = now_ms();
    int by_timer = epoll_wait(ep, got, 4, 2000) == 1 && got[0].data.u64 == 333 && now_ms() - before >= 15;
    check(quiet && woke && by_timer, "epoll waits for a socket and a timerfd");

    /* memfd и MAP_SHARED: ребёнок пишет в отображение, родитель видит. */
    int mem = memfd_create("fossai", MFD_CLOEXEC);
    int sized = mem >= 0 && ftruncate(mem, 8192) == 0;
    volatile int *shared = sized ? mmap(NULL, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, mem, 0) : MAP_FAILED;
    int mapped = shared != MAP_FAILED;
    pid_t writer = mapped ? fork() : -1;
    if (writer == 0) {
        shared[0] = 42;
        shared[1024] = 43;
        _exit(0);
    }
    int seen = mapped && exited_with(writer, 0) && shared[0] == 42 && shared[1024] == 43;
    int through = 0;
    struct stat info;
    if (seen && pread(mem, &through, sizeof through, 0) == sizeof through) seen = through == 42;
    seen = seen && fstat(mem, &info) == 0 && info.st_size == 8192;
    check(seen, "memfd mapped MAP_SHARED is shared with a child");

    volatile int *anon = mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    pid_t anon_writer = anon != MAP_FAILED ? fork() : -1;
    if (anon_writer == 0) {
        anon[7] = 77;
        _exit(0);
    }
    check(anon != MAP_FAILED && exited_with(anon_writer, 0) && anon[7] == 77, "MAP_SHARED|MAP_ANONYMOUS survives fork");

    /* shm_open: имя в /dev/shm видно второму открытию. */
    int first = shm_open("/fossai-71e", O_CREAT | O_RDWR, 0600);
    int named_shm = first >= 0 && ftruncate(first, 4096) == 0;
    int *one_map = named_shm ? mmap(NULL, 4096, PROT_READ | PROT_WRITE, MAP_SHARED, first, 0) : MAP_FAILED;
    if (one_map != MAP_FAILED) one_map[0] = 5;
    int second = shm_open("/fossai-71e", O_RDWR, 0600);
    int *two_map = second >= 0 ? mmap(NULL, 4096, PROT_READ, MAP_SHARED, second, 0) : MAP_FAILED;
    named_shm = named_shm && two_map != MAP_FAILED && two_map[0] == 5 && shm_unlink("/fossai-71e") == 0 &&
                shm_open("/fossai-71e", O_RDWR, 0600) == -1 && errno == ENOENT;
    check(named_shm, "shm_open, a second mapping and shm_unlink");

    printf("ipccheck: %d failed\n", failed);
    return failed;
}
