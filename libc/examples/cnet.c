/* Сеть из C через сокеты BSD (фаза 65b).
 *
 * # Что она доказывает
 *
 * Что программа, написанная под POSIX и ничего не знающая о FreeOS, ходит в
 * сеть теми же вызовами, что под Linux: `getaddrinfo`, `socket`, `connect`,
 * `send`/`recv`, `select`, `shutdown`, `getsockname`/`getpeername`,
 * неблокирующее соединение через `fcntl` с исходом в `SO_ERROR`, и сервер —
 * `bind`, `listen`, `accept`. Каждая проверка устроена так, чтобы заглушка её
 * не прошла: эхо обязано вернуться тем же текстом, конец потока — нулём,
 * адреса — настоящими числами.
 *
 * `cnet <адрес> <порт>` — клиент эхо-сервера; `cnet serve <порт>` — сервер на
 * одного клиента; `cnet resolve <имя>` — имя в адрес через `getaddrinfo`.
 * Итог клиента — строка с числом проваленных.
 */

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
#include <sys/select.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;

static void check(int ok, const char *what) {
    if (ok) {
        printf("cnet: ok %s\n", what);
    } else {
        failures++;
        printf("cnet: FAILED %s (errno %d: %s)\n", what, errno, strerror(errno));
    }
}

/* Адрес и порт строкой — числами, без имени. */
static void describe(const char *what, const struct sockaddr_in *address) {
    char text[INET_ADDRSTRLEN];
    inet_ntop(AF_INET, &address->sin_addr, text, sizeof(text));
    printf("cnet: %s %s:%u\n", what, text, ntohs(address->sin_port));
}

static int client(const char *host, const char *port) {
    struct addrinfo hints;
    memset(&hints, 0, sizeof(hints));
    hints.ai_family = AF_INET;
    hints.ai_socktype = SOCK_STREAM;
    struct addrinfo *target = NULL;
    int code = getaddrinfo(host, port, &hints, &target);
    check(code == 0 && target != NULL && target->ai_family == AF_INET, "getaddrinfo gives the address");
    if (code != 0) {
        printf("cnet: getaddrinfo: %s\n", gai_strerror(code));
        return 1;
    }

    /* Блокирующее соединение и эхо. */
    int fd = socket(target->ai_family, target->ai_socktype, target->ai_protocol);
    check(fd >= 0, "socket opens a descriptor");
    check(connect(fd, target->ai_addr, target->ai_addrlen) == 0, "connect waits for the server");

    int nodelay = 1;
    check(setsockopt(fd, IPPROTO_TCP, TCP_NODELAY, &nodelay, sizeof(nodelay)) == 0, "setsockopt TCP_NODELAY");

    const char *text = "c-sockets-hello";
    size_t length = strlen(text);
    check(send(fd, text, length, 0) == (ssize_t)length, "send hands over the text");

    /* Ответ ждётся `select` — тем, чем ждут программы постарше `poll`. */
    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(fd, &readable);
    struct timeval limit = {10, 0};
    check(select(fd + 1, &readable, NULL, NULL, &limit) == 1 && FD_ISSET(fd, &readable), "select sees the reply");

    char buffer[64];
    size_t got = 0;
    while (got < length) {
        ssize_t read_now = recv(fd, buffer + got, sizeof(buffer) - got - 1, 0);
        if (read_now <= 0) {
            break;
        }
        got += (size_t)read_now;
    }
    buffer[got] = '\0';
    check(got == length && strcmp(buffer, text) == 0, "recv gets the echo back");
    printf("cnet: echoed '%s'\n", buffer);

    struct sockaddr_in mine;
    struct sockaddr_in theirs;
    socklen_t mine_length = sizeof(mine);
    socklen_t theirs_length = sizeof(theirs);
    check(getsockname(fd, (struct sockaddr *)&mine, &mine_length) == 0 && ntohs(mine.sin_port) != 0 &&
              mine.sin_addr.s_addr != htonl(INADDR_ANY),
          "getsockname names the local address and port");
    describe("local", &mine);
    check(getpeername(fd, (struct sockaddr *)&theirs, &theirs_length) == 0 &&
              theirs.sin_addr.s_addr == ((struct sockaddr_in *)target->ai_addr)->sin_addr.s_addr,
          "getpeername names the server");
    describe("peer", &theirs);

    struct stat info;
    check(fstat(fd, &info) == 0 && S_ISSOCK(info.st_mode), "fstat says S_IFSOCK");

    /* Своя половина закрыта — сервер закроет свою, и `recv` вернёт ноль. */
    check(shutdown(fd, SHUT_WR) == 0, "shutdown closes the writing half");
    check(recv(fd, buffer, sizeof(buffer), 0) == 0, "recv reads the end of stream as zero");
    check(close(fd) == 0, "close");

    /* Неблокирующее соединение: `EINPROGRESS`, ожидание `poll` на запись,
     * исход — `SO_ERROR`. Так соединяются libcurl и браузеры. */
    fd = socket(AF_INET, SOCK_STREAM, 0);
    check(fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_NONBLOCK) == 0 && (fcntl(fd, F_GETFL) & O_NONBLOCK) != 0,
          "fcntl turns O_NONBLOCK on");
    int started = connect(fd, target->ai_addr, target->ai_addrlen);
    check(started == -1 && errno == EINPROGRESS, "a nonblocking connect says EINPROGRESS");
    struct pollfd entry = {fd, POLLOUT, 0};
    check(poll(&entry, 1, 10000) == 1 && (entry.revents & POLLOUT) != 0, "poll waits until it can write");
    int error = -1;
    socklen_t error_length = sizeof(error);
    check(getsockopt(fd, SOL_SOCKET, SO_ERROR, &error, &error_length) == 0 && error == 0,
          "SO_ERROR says the connection is up");
    check(recv(fd, buffer, sizeof(buffer), 0) == -1 && errno == EAGAIN, "a nonblocking recv says EAGAIN");
    close(fd);

    /* Служба по имени — без сети: таблица в libc. */
    struct addrinfo *named = NULL;
    check(getaddrinfo(host, "http", &hints, &named) == 0 &&
              ntohs(((struct sockaddr_in *)named->ai_addr)->sin_port) == 80,
          "getaddrinfo knows the http service");
    if (named != NULL) {
        freeaddrinfo(named);
    }

    freeaddrinfo(target);
    printf("cnet: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}

static int serve(const char *port_text) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    int reuse = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
    struct sockaddr_in address;
    memset(&address, 0, sizeof(address));
    address.sin_family = AF_INET;
    address.sin_port = htons((unsigned short)atoi(port_text));
    address.sin_addr.s_addr = htonl(INADDR_ANY);
    if (bind(fd, (struct sockaddr *)&address, sizeof(address)) != 0 || listen(fd, SOMAXCONN) != 0) {
        printf("cnet: FAILED bind and listen (errno %d: %s)\n", errno, strerror(errno));
        return 1;
    }
    printf("cnet: listening on port %s\n", port_text);
    fflush(stdout);

    struct sockaddr_in client_address;
    socklen_t client_length = sizeof(client_address);
    int client = accept(fd, (struct sockaddr *)&client_address, &client_length);
    if (client < 0) {
        printf("cnet: FAILED accept (errno %d: %s)\n", errno, strerror(errno));
        return 1;
    }
    describe("accepted a client from", &client_address);

    char buffer[512];
    long total = 0;
    for (;;) {
        ssize_t got = recv(client, buffer, sizeof(buffer), 0);
        if (got <= 0) {
            break;
        }
        send(client, buffer, (size_t)got, 0);
        total += got;
    }
    printf("cnet: served %ld bytes\n", total);
    close(client);
    close(fd);
    return 0;
}

static int lookup(const char *name) {
    struct addrinfo *found = NULL;
    int code = getaddrinfo(name, NULL, NULL, &found);
    if (code != 0) {
        printf("cnet: %s does not resolve: %s\n", name, gai_strerror(code));
        return 1;
    }
    char text[INET_ADDRSTRLEN];
    inet_ntop(AF_INET, &((struct sockaddr_in *)found->ai_addr)->sin_addr, text, sizeof(text));
    printf("cnet: %s is %s\n", name, text);
    freeaddrinfo(found);
    return 0;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    if (argc == 3 && strcmp(argv[1], "serve") == 0) {
        return serve(argv[2]);
    }
    if (argc == 3 && strcmp(argv[1], "resolve") == 0) {
        return lookup(argv[2]);
    }
    if (argc == 3) {
        return client(argv[1], argv[2]);
    }
    fprintf(stderr, "usage: cnet <address> <port> | cnet serve <port> | cnet resolve <name>\n");
    return 2;
}
