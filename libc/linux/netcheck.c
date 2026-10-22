/* Сеть программы Linux на glibc (фаза 71f): TCP-клиент к эхо-серверу хоста,
 * неждущее соединение с poll и SO_ERROR, epoll над сокетом, отказ в
 * соединении, имя через резолвер glibc (`/etc/resolv.conf`, UDP), сервер с
 * accept.
 *
 *   netcheck <адрес> <порт>   — клиент эхо-сервера и имя через DNS;
 *   netcheck serve <порт>     — принять одного клиента и вернуть ему эхо.
 *
 * Едет в пакете `debian-12.fpk` как `/usr/libexec/fossai/netcheck`. Строки —
 * `netcheck: ok <что>` или `netcheck: FAILED <что>`; итог — `netcheck: N
 * failed`, код выхода — то же N.
 */

#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netdb.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/socket.h>
#include <unistd.h>

static int failed;

static void check(int good, const char *what) {
    printf("netcheck: %s %s\n", good ? "ok" : "FAILED", what);
    fflush(stdout);
    if (!good) failed++;
}

/* Прочитать до конца файла или до `size` байт. */
static size_t drain(int fd, char *out, size_t size) {
    size_t have = 0;
    ssize_t got;
    while (have < size && (got = read(fd, out + have, size - have)) > 0) have += (size_t)got;
    return have;
}

static int serve(const char *port) {
    int server = socket(AF_INET, SOCK_STREAM | SOCK_CLOEXEC, 0);
    int one = 1;
    setsockopt(server, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons((unsigned short)atoi(port));
    addr.sin_addr.s_addr = htonl(INADDR_ANY);
    if (server < 0 || bind(server, (struct sockaddr *)&addr, sizeof addr) != 0 || listen(server, 4) != 0) {
        printf("netcheck: FAILED listen on port %s\n", port);
        return 1;
    }
    printf("netcheck: listening on port %s\n", port);
    fflush(stdout);
    struct sockaddr_in peer;
    socklen_t peer_len = sizeof peer;
    int client = accept(server, (struct sockaddr *)&peer, &peer_len);
    if (client < 0) {
        printf("netcheck: FAILED accept (errno %d)\n", errno);
        return 1;
    }
    printf("netcheck: accepted a client from %s\n", inet_ntoa(peer.sin_addr));
    fflush(stdout);
    char buffer[256];
    ssize_t got = read(client, buffer, sizeof buffer);
    int echoed = got > 0 && write(client, buffer, (size_t)got) == got;
    printf("netcheck: served %zd bytes\n", got);
    close(client);
    close(server);
    return echoed ? 0 : 1;
}

int main(int argc, char **argv) {
    if (argc == 3 && strcmp(argv[1], "serve") == 0) return serve(argv[2]);
    if (argc != 3) {
        printf("usage: netcheck <address> <port> | netcheck serve <port>\n");
        return 2;
    }
    const char *host = argv[1], *port = argv[2];

    /* Блокирующий клиент: адрес числом через getaddrinfo. */
    struct addrinfo hints = {0}, *found = NULL;
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    hints.ai_flags = AI_NUMERICHOST;
    int resolved = getaddrinfo(host, port, &hints, &found) == 0 && found;
    int sock = resolved ? socket(found->ai_family, found->ai_socktype, found->ai_protocol) : -1;
    int connected = sock >= 0 && connect(sock, found->ai_addr, found->ai_addrlen) == 0;
    const char hello[] = "linux-tcp-hello";
    char echo[64] = {0};
    int one = 1;
    setsockopt(sock, IPPROTO_TCP, TCP_NODELAY, &one, sizeof one);
    int sent = connected && write(sock, hello, sizeof hello - 1) == (ssize_t)(sizeof hello - 1);
    int echoed = sent && drain(sock, echo, sizeof hello - 1) == sizeof hello - 1 && strcmp(echo, hello) == 0;
    check(echoed, "connect, write and read back from the host echo server");

    struct sockaddr_in local, remote;
    socklen_t local_len = sizeof local, remote_len = sizeof remote;
    int named = connected && getsockname(sock, (struct sockaddr *)&local, &local_len) == 0 &&
                getpeername(sock, (struct sockaddr *)&remote, &remote_len) == 0 && local.sin_port != 0 &&
                remote.sin_port == htons((unsigned short)atoi(port)) && strcmp(inet_ntoa(remote.sin_addr), host) == 0;
    check(named, "getsockname and getpeername");
    shutdown(sock, SHUT_WR);
    char rest[8];
    check(connected && read(sock, rest, sizeof rest) == 0, "shutdown, then EOF from the server");
    if (sock >= 0) close(sock);

    /* Неждущее соединение: EINPROGRESS, poll на запись, SO_ERROR. */
    int quick = resolved ? socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0) : -1;
    int started = quick >= 0 && connect(quick, found->ai_addr, found->ai_addrlen) == -1 && errno == EINPROGRESS;
    struct pollfd watch = {quick, POLLOUT, 0};
    int error = -1;
    socklen_t error_len = sizeof error;
    int finished = started && poll(&watch, 1, 10000) == 1 && (watch.revents & POLLOUT) &&
                   getsockopt(quick, SOL_SOCKET, SO_ERROR, &error, &error_len) == 0 && error == 0;
    check(finished, "non-blocking connect, poll for POLLOUT, SO_ERROR");

    /* epoll над сокетом: эхо приходит, сокет становится готов на чтение. */
    int ep = epoll_create1(0);
    struct epoll_event event = {0}, got;
    event.events = EPOLLIN;
    event.data.fd = quick;
    int watched = finished && ep >= 0 && epoll_ctl(ep, EPOLL_CTL_ADD, quick, &event) == 0 && write(quick, "epoll", 5) == 5;
    memset(echo, 0, sizeof echo);
    int ready = watched && epoll_wait(ep, &got, 1, 10000) == 1 && got.data.fd == quick;
    ssize_t part = ready ? read(quick, echo, sizeof echo) : -1;
    check(ready && part > 0 && strncmp(echo, "epoll", (size_t)part) == 0, "epoll reports the echo");
    if (quick >= 0) close(quick);

    /* Отказ: порт, где никто не слушает. */
    struct sockaddr_in closed = {0};
    closed.sin_family = AF_INET;
    closed.sin_port = htons(1);
    inet_pton(AF_INET, host, &closed.sin_addr);
    int refused_sock = socket(AF_INET, SOCK_STREAM, 0);
    int refused = refused_sock >= 0 && connect(refused_sock, (struct sockaddr *)&closed, sizeof closed) == -1 &&
                  errno == ECONNREFUSED;
    check(refused, "connect to a closed port is ECONNREFUSED");
    if (refused_sock >= 0) close(refused_sock);

    /* Имя через DNS: резолвер glibc читает /etc/resolv.conf и спрашивает
     * сервер имён по UDP. Отвечает резолвер хоста — нужен выход в интернет. */
    FILE *conf = fopen("/etc/resolv.conf", "r");
    char line[128] = {0};
    int has_server = conf && fgets(line, sizeof line, conf) && strncmp(line, "nameserver ", 11) == 0;
    if (conf) fclose(conf);
    check(has_server, "/etc/resolv.conf names the DHCP name server");
    struct addrinfo name_hints = {0}, *names = NULL;
    name_hints.ai_family = AF_INET;
    name_hints.ai_socktype = SOCK_STREAM;
    int status = getaddrinfo("example.com", "80", &name_hints, &names);
    char text[INET_ADDRSTRLEN] = "?";
    if (status == 0 && names) inet_ntop(AF_INET, &((struct sockaddr_in *)names->ai_addr)->sin_addr, text, sizeof text);
    printf("netcheck: example.com is %s (status %d)\n", text, status);
    check(status == 0 && names, "getaddrinfo resolves a name through DNS");

    if (found) freeaddrinfo(found);
    if (names) freeaddrinfo(names);
    printf("netcheck: %d failed\n", failed);
    return failed;
}
