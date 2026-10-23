/* Клиент Wayland без libwayland (фаза 71i): проверка сервера в ядре.
 *
 * Говорит на проводе сам — так проверяется сервер, а не библиотека клиента:
 * реестр и привязка глобальных объектов, форматы `wl_shm`, окно
 * `xdg_toplevel` с `configure`, буфер из `memfd` (дескриптор через
 * `SCM_RIGHTS`), `wl_callback.done` кадра, `wl_buffer.release` прежнего
 * буфера, когда показан следующий, вход указателя в окно, раскладка XKB
 * дескриптором и клавиша `a`. Окно — градиент 320x200 с рамкой.
 *
 * Едет в пакете `debian-12.fpk` как `/usr/libexec/fossai/waylandcheck`.
 * Строки — `waylandcheck: ok <что>` или `waylandcheck: FAILED <что>`; итог —
 * `waylandcheck: N failed`, код выхода — то же N.
 */

#define _GNU_SOURCE
#include <errno.h>
#include <poll.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

static int failed;
static int sock;

static void check(int good, const char *what) {
    printf("waylandcheck: %s %s\n", good ? "ok" : "FAILED", what);
    fflush(stdout);
    if (!good) failed++;
}

/* ---- провод ---- */

static uint32_t msg[256];
static int msg_len;

static void begin(uint32_t id, uint32_t opcode) {
    msg[0] = id;
    msg[1] = opcode;
    msg_len = 2;
}
static void put(uint32_t value) { msg[msg_len++] = value; }
static void put_str(const char *text) {
    uint32_t len = (uint32_t)strlen(text) + 1;
    put(len);
    memset(&msg[msg_len], 0, (len + 3) & ~3u);
    memcpy(&msg[msg_len], text, len);
    msg_len += (int)((len + 3) / 4);
}
static void end_fd(int fd) {
    msg[1] |= (uint32_t)(msg_len * 4) << 16;
    struct iovec iov = {msg, (size_t)msg_len * 4};
    union {
        char buf[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } control;
    struct msghdr header = {0};
    header.msg_iov = &iov;
    header.msg_iovlen = 1;
    if (fd >= 0) {
        header.msg_control = control.buf;
        header.msg_controllen = sizeof control.buf;
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&header);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cmsg), &fd, sizeof fd);
    }
    if (sendmsg(sock, &header, 0) < 0) perror("waylandcheck: sendmsg");
}
static void end(void) { end_fd(-1); }

/* ---- состояние ---- */

enum {
    ID_DISPLAY = 1, ID_REGISTRY, ID_SYNC, ID_COMPOSITOR, ID_SHM, ID_WM, ID_SEAT,
    ID_SURFACE, ID_XDG, ID_TOPLEVEL, ID_POOL, ID_BUFFER, ID_FRAME, ID_POINTER, ID_SYNC2, ID_KEYBOARD, ID_BUFFER2
};

static uint32_t global_compositor, global_shm, global_wm, global_seat;
static int synced, synced2, formats_argb, configured, configure_serial, released, frame_done, pointer_in;
static int protocol_error;
static int keymap_ok, keyboard_in, key_down, key_up;
static int fds[16], nfds;

static void handle(uint32_t id, uint32_t opcode, uint32_t *args, int words) {
    (void)words;
    if (id == ID_DISPLAY && opcode == 0) {
        printf("waylandcheck: protocol error %u on %u: %s\n", args[1], args[0], (char *)&args[3]);
        protocol_error = 1;
    } else if (id == ID_REGISTRY && opcode == 0) {
        uint32_t name = args[0];
        const char *interface = (const char *)&args[2];
        if (!strcmp(interface, "wl_compositor")) global_compositor = name;
        if (!strcmp(interface, "wl_shm")) global_shm = name;
        if (!strcmp(interface, "xdg_wm_base")) global_wm = name;
        if (!strcmp(interface, "wl_seat")) global_seat = name;
    } else if (id == ID_SYNC && opcode == 0) {
        synced = 1;
    } else if (id == ID_SYNC2 && opcode == 0) {
        synced2 = 1;
    } else if (id == ID_SHM && opcode == 0) {
        if (args[0] == 0) formats_argb = 1;
    } else if (id == ID_XDG && opcode == 0) {
        configured = 1;
        configure_serial = (int)args[0];
    } else if (id == ID_BUFFER && opcode == 0) {
        released = 1;
    } else if (id == ID_FRAME && opcode == 0) {
        frame_done = 1;
    } else if (id == ID_POINTER && opcode == 0) {
        pointer_in = 1;
    } else if (id == ID_KEYBOARD && opcode == 0) {
        int fd = nfds > 0 ? fds[0] : -1;
        if (nfds > 0) memmove(fds, fds + 1, sizeof(int) * (size_t)--nfds);
        char *text = fd >= 0 ? mmap(NULL, args[1], PROT_READ, MAP_PRIVATE, fd, 0) : MAP_FAILED;
        keymap_ok = args[0] == 1 && text != MAP_FAILED && strstr(text, "xkb_keymap") != NULL;
        if (fd >= 0) close(fd);
    } else if (id == ID_KEYBOARD && opcode == 1) {
        keyboard_in = 1;
    } else if (id == ID_KEYBOARD && opcode == 3) {
        if (args[2] == 30 && args[3] == 1) key_down = 1;
        if (args[2] == 30 && args[3] == 0 && key_down) key_up = 1;
    } else if (id == ID_WM && opcode == 0) {
        begin(ID_WM, 3); /* pong */
        put(args[0]);
        end();
    }
}

static uint8_t inbuf[65536];
static size_t inlen;

/* Читать и разбирать события, пока `*flag` не станет ненулевым или не
 * выйдет срок. */
static int wait_for(int *flag, int ms) {
    struct timespec start;
    clock_gettime(CLOCK_MONOTONIC, &start);
    while (!*flag && !protocol_error) {
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        long spent = (now.tv_sec - start.tv_sec) * 1000 + (now.tv_nsec - start.tv_nsec) / 1000000;
        if (spent >= ms) return 0;
        struct pollfd pfd = {sock, POLLIN, 0};
        if (poll(&pfd, 1, (int)(ms - spent)) <= 0) continue;
        struct iovec iov = {inbuf + inlen, sizeof inbuf - inlen};
        union {
            char buf[CMSG_SPACE(sizeof(int) * 8)];
            struct cmsghdr align;
        } control;
        struct msghdr header = {0};
        header.msg_iov = &iov;
        header.msg_iovlen = 1;
        header.msg_control = control.buf;
        header.msg_controllen = sizeof control.buf;
        ssize_t got = recvmsg(sock, &header, MSG_CMSG_CLOEXEC);
        if (got <= 0) return 0;
        for (struct cmsghdr *cmsg = CMSG_FIRSTHDR(&header); cmsg; cmsg = CMSG_NXTHDR(&header, cmsg)) {
            if (cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) continue;
            int count = (int)((cmsg->cmsg_len - CMSG_LEN(0)) / sizeof(int));
            for (int i = 0; i < count && nfds < 16; i++) memcpy(&fds[nfds++], CMSG_DATA(cmsg) + i * sizeof(int), sizeof(int));
        }
        inlen += (size_t)got;
        while (inlen >= 8) {
            uint32_t header[2];
            memcpy(header, inbuf, 8);
            uint32_t size = header[1] >> 16;
            if (size < 8 || inlen < size) break;
            uint32_t args[16384];
            memcpy(args, inbuf + 8, size - 8);
            handle(header[0], header[1] & 0xffff, args, (int)(size - 8) / 4);
            memmove(inbuf, inbuf + size, inlen - size);
            inlen -= size;
        }
    }
    return *flag;
}

static void bind_global(uint32_t name, const char *interface, uint32_t version, uint32_t id) {
    begin(ID_REGISTRY, 0);
    put(name);
    put_str(interface);
    put(version);
    put(id);
    end();
}

int main(void) {
    const char *dir = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    struct sockaddr_un addr = {0};
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof addr.sun_path, "%s/%s", dir ? dir : "/run/user/0", display ? display : "wayland-0");
    sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    int connected = sock >= 0 && connect(sock, (struct sockaddr *)&addr, sizeof addr) == 0;
    check(connected, "connect to the compositor");
    if (!connected) {
        printf("waylandcheck: %d failed\n", failed);
        return failed;
    }

    begin(ID_DISPLAY, 1); /* get_registry */
    put(ID_REGISTRY);
    end();
    begin(ID_DISPLAY, 0); /* sync */
    put(ID_SYNC);
    end();
    wait_for(&synced, 5000);
    check(synced && global_compositor && global_shm && global_wm && global_seat,
          "registry lists wl_compositor, wl_shm, xdg_wm_base and wl_seat");

    bind_global(global_compositor, "wl_compositor", 4, ID_COMPOSITOR);
    bind_global(global_shm, "wl_shm", 1, ID_SHM);
    bind_global(global_wm, "xdg_wm_base", 2, ID_WM);
    bind_global(global_seat, "wl_seat", 5, ID_SEAT);
    begin(ID_SEAT, 0); /* get_pointer */
    put(ID_POINTER);
    end();
    begin(ID_SEAT, 1); /* get_keyboard */
    put(ID_KEYBOARD);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC2);
    end();
    wait_for(&synced2, 5000);
    check(formats_argb, "wl_shm offers ARGB8888");
    check(keymap_ok, "wl_keyboard sends an XKB keymap through a descriptor");

    begin(ID_COMPOSITOR, 0); /* create_surface */
    put(ID_SURFACE);
    end();
    begin(ID_WM, 2); /* get_xdg_surface */
    put(ID_XDG);
    put(ID_SURFACE);
    end();
    begin(ID_XDG, 1); /* get_toplevel */
    put(ID_TOPLEVEL);
    end();
    begin(ID_TOPLEVEL, 2); /* set_title */
    put_str("waylandcheck");
    end();
    begin(ID_SURFACE, 6); /* commit */
    end();
    wait_for(&configured, 5000);
    check(configured, "xdg_toplevel gets configure");
    begin(ID_XDG, 4); /* ack_configure */
    put((uint32_t)configure_serial);
    end();

    const int width = 320, height = 200, stride = width * 4;
    int fd = memfd_create("waylandcheck", MFD_CLOEXEC);
    /* Два буфера в одном пуле: сервер держит показанный, пока его не сменит
     * следующий, и только тогда возвращает (`release`). */
    int sized = fd >= 0 && ftruncate(fd, 2 * stride * height) == 0;
    uint32_t *pixels = sized ? mmap(NULL, (size_t)2 * stride * height, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) : MAP_FAILED;
    if (pixels != MAP_FAILED) {
        for (int y = 0; y < height; y++)
            for (int x = 0; x < width; x++) {
                int border = x < 4 || y < 4 || x >= width - 4 || y >= height - 4;
                uint32_t r = (uint32_t)(x * 255 / width), g = (uint32_t)(y * 255 / height), b = 160;
                pixels[y * width + x] = border ? 0xffffffffu : (0xffu << 24) | (r << 16) | (g << 8) | b;
            }
        memcpy(pixels + width * height, pixels, (size_t)stride * height);
    }
    begin(ID_SHM, 0); /* create_pool */
    put(ID_POOL);
    put((uint32_t)(2 * stride * height));
    end_fd(fd);
    begin(ID_POOL, 0); /* create_buffer */
    put(ID_BUFFER);
    put(0);
    put(width);
    put(height);
    put(stride);
    put(0); /* ARGB8888 */
    end();
    begin(ID_SURFACE, 1); /* attach */
    put(ID_BUFFER);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 2); /* damage */
    put(0);
    put(0);
    put(width);
    put(height);
    end();
    begin(ID_SURFACE, 3); /* frame */
    put(ID_FRAME);
    end();
    begin(ID_SURFACE, 6); /* commit */
    end();
    wait_for(&frame_done, 5000);
    check(pixels != MAP_FAILED && frame_done, "a memfd buffer is shown and the frame is done");

    begin(ID_POOL, 0); /* create_buffer: вторая половина пула */
    put(ID_BUFFER2);
    put((uint32_t)(stride * height));
    put(width);
    put(height);
    put(stride);
    put(0);
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER2);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 6);
    end();
    wait_for(&released, 5000);
    check(released, "the first buffer is released once the second is shown");

    printf("waylandcheck: waiting for the pointer\n");
    fflush(stdout);
    wait_for(&pointer_in, 60000);
    check(pointer_in, "the pointer enters the window");

    printf("waylandcheck: waiting for a key\n");
    fflush(stdout);
    wait_for(&key_up, 60000);
    check(keyboard_in && key_down && key_up, "the window has the keyboard; key a goes down and up");

    struct timespec nap = {2, 0};
    nanosleep(&nap, NULL);
    begin(ID_TOPLEVEL, 0); /* destroy */
    end();
    begin(ID_XDG, 0);
    end();
    begin(ID_SURFACE, 0);
    end();
    check(!protocol_error, "no protocol errors");
    close(sock);
    printf("waylandcheck: %d failed\n", failed);
    return failed;
}
