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
#include <fcntl.h>
#include <sys/ioctl.h>
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
    ID_SURFACE, ID_XDG, ID_TOPLEVEL, ID_POOL, ID_BUFFER, ID_FRAME, ID_POINTER, ID_SYNC2, ID_KEYBOARD, ID_BUFFER2,
    ID_POOL_FULL, ID_BUFFER_FULL, ID_FRAME2, ID_BUFFER_MON, ID_VIEWPORTER, ID_VIEWPORT, ID_FRAME3,
    ID_DMABUF, ID_PARAMS1, ID_DBUF1, ID_PARAMS2, ID_DBUF2, ID_FRAME4,
    /* G3 (21.11): mouse for games, activation, fractional scale. */
    ID_RELMGR, ID_REL, ID_CONSTR, ID_LOCK, ID_REGION, ID_CONFINE, ID_ACT, ID_TOKEN, ID_FRACMGR, ID_FRAC, ID_SYNC3,
    ID_OUTPUT
};
/* `wl_output.mode` (bench): the monitor size. */
static uint32_t global_output;
static int out_w, out_h;

/* G3: globals and what they said. */
static uint32_t global_relative, global_constraints, global_activation, global_fractional;
static int frac_scale, token_done, synced3;
static int locked, unlocked, lock_on, confined, confine_on, rel_seen, motion_locked;
static int rel_dx, rel_dy, confine_max_x = -1, confine_max_y = -1, confine_moved;

static uint32_t global_compositor, global_shm, global_wm, global_seat, global_viewporter;
/* `done` кадра с буфером через `wp_viewport` (D4, 20.11). */
static int frame3_done;
/* dma-buf во весь монитор (D4): `zwp_linux_dmabuf_v1` и возврат буфера. */
static uint32_t global_dmabuf;
static int dbuf1_released, frame4_done;
static int synced, synced2, formats_argb, configured, configure_serial, released, frame_done, pointer_in;
static int protocol_error;
static int keymap_ok, keyboard_in, key_down, key_up, key_b;
/* Последний `xdg_toplevel.configure` (20.11): размер и есть ли FULLSCREEN. */
static int conf_w, conf_h, full_seen, full_clear, never;
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
        if (!strcmp(interface, "wp_viewporter")) global_viewporter = name;
        if (!strcmp(interface, "zwp_linux_dmabuf_v1")) global_dmabuf = name;
        if (!strcmp(interface, "zwp_relative_pointer_manager_v1")) global_relative = name;
        if (!strcmp(interface, "zwp_pointer_constraints_v1")) global_constraints = name;
        if (!strcmp(interface, "xdg_activation_v1")) global_activation = name;
        if (!strcmp(interface, "wp_fractional_scale_manager_v1")) global_fractional = name;
        if (!strcmp(interface, "wl_output")) global_output = name;
    } else if (id == ID_OUTPUT && opcode == 1) {
        out_w = (int)args[1];
        out_h = (int)args[2];
    } else if (id == ID_SYNC3 && opcode == 0) {
        synced3 = 1;
    } else if (id == ID_FRAC && opcode == 0) {
        frac_scale = (int)args[0];
    } else if (id == ID_TOKEN && opcode == 0) {
        token_done = 1;
    } else if (id == ID_LOCK && opcode == 0) {
        locked = 1;
        lock_on = 1;
    } else if (id == ID_LOCK && opcode == 1) {
        unlocked = 1;
        lock_on = 0;
    } else if (id == ID_CONFINE && opcode == 0) {
        confined = 1;
        confine_on = 1;
    } else if (id == ID_CONFINE && opcode == 1) {
        confine_on = 0;
    } else if (id == ID_REL && opcode == 0) {
        /* relative_motion(utime_hi, utime_lo, dx, dy, dx_unaccel, dy_unaccel) */
        rel_seen = 1;
        rel_dx += (int32_t)args[4] / 256;
        rel_dy += (int32_t)args[5] / 256;
    } else if (id == ID_POINTER && opcode == 2) {
        /* motion(time, x, y) */
        int x = (int32_t)args[1] / 256, y = (int32_t)args[2] / 256;
        if (lock_on) motion_locked++;
        if (confine_on) {
            if (x > confine_max_x) confine_max_x = x;
            if (y > confine_max_y) confine_max_y = y;
            if (x > 0 || y > 0) confine_moved = 1;
        }
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
    } else if (id == ID_DBUF1 && opcode == 0) {
        dbuf1_released = 1;
    } else if (id == ID_FRAME4 && opcode == 0) {
        frame4_done = 1;
    } else if (id == ID_FRAME3 && opcode == 0) {
        frame3_done = 1;
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
        if (args[2] == 48 && args[3] == 0) key_b = 1;
    } else if (id == ID_TOPLEVEL && opcode == 0) {
        int full = 0;
        conf_w = (int)args[0];
        conf_h = (int)args[1];
        for (uint32_t i = 0; i < args[2] / 4; i++)
            if (args[3 + i] == 2) full = 1; /* XDG_TOPLEVEL_STATE_FULLSCREEN */
        if (full) full_seen = 1;
        else full_clear = 1;
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

static long now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

/* `waylandcheck bench [frames]` (G3, 21.11): a fullscreen window shows
 * monitor-sized dma-bufs of the DRM node in turn (like a game's swapchain),
 * one per frame callback, no interaction - for perf-diag's present cost with
 * `wldirect on` / `off`. Needs `nvk soft on` (or the GPU) in QEMU. */
static int bench(int frames) {
    begin(ID_DISPLAY, 1);
    put(ID_REGISTRY);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC);
    end();
    wait_for(&synced, 5000);
    int drm = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (!synced || !global_compositor || !global_wm || !global_dmabuf || !global_output || drm < 0) {
        printf("waylandcheck: bench FAILED - no compositor, output, dma-buf or /dev/dri/renderD128\n");
        return 1;
    }
    bind_global(global_compositor, "wl_compositor", 4, ID_COMPOSITOR);
    bind_global(global_wm, "xdg_wm_base", 2, ID_WM);
    bind_global(global_dmabuf, "zwp_linux_dmabuf_v1", 3, ID_DMABUF);
    bind_global(global_output, "wl_output", 2, ID_OUTPUT);
    begin(ID_DISPLAY, 0);
    put(ID_SYNC2);
    end();
    wait_for(&synced2, 5000);
    begin(ID_COMPOSITOR, 0);
    put(ID_SURFACE);
    end();
    begin(ID_WM, 2);
    put(ID_XDG);
    put(ID_SURFACE);
    end();
    begin(ID_XDG, 1);
    put(ID_TOPLEVEL);
    end();
    begin(ID_TOPLEVEL, 2);
    put_str("waylandcheck bench");
    end();
    begin(ID_TOPLEVEL, 11); /* set_fullscreen before the first buffer */
    put(0);
    end();
    begin(ID_SURFACE, 6);
    end();
    wait_for(&configured, 5000);
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    /* Monitor-sized frameless window: the desktop makes it fullscreen. */
    int w = out_w, h = out_h;
    if (w <= 0 || h <= 0 || w > 8192 || h > 8192) {
        printf("waylandcheck: bench FAILED - output mode %dx%d\n", w, h);
        return 1;
    }
    size_t bytes = (size_t)w * (size_t)h * 4;
    uint32_t bufs[2] = {ID_DBUF1, ID_DBUF2}, params[2] = {ID_PARAMS1, ID_PARAMS2};
    for (int b = 0; b < 2; b++) {
        struct { uint32_t handle, domain; uint64_t size, offset, map_handle; uint32_t tile_mode, tile_flags, channel_hint, align; } gem = {0};
        gem.domain = 4 | 8;
        gem.size = bytes;
        struct { uint32_t handle, flags; int32_t fd; } prime = {0};
        if (ioctl(drm, 0xC0306480, &gem) != 0) return 1;
        uint32_t *px = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (off_t)gem.map_handle);
        if (px == MAP_FAILED) return 1;
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++)
                px[(size_t)y * w + x] = b ? 0x00204060u + (uint32_t)(x * 128 / w) : 0x00402060u + (uint32_t)((y * 128 / h) << 8);
        munmap(px, bytes);
        prime.handle = gem.handle;
        prime.flags = 02000000;
        if (ioctl(drm, 0xC00C642D, &prime) != 0) return 1;
        begin(ID_DMABUF, 1);
        put(params[b]);
        end();
        begin(params[b], 1);
        put(0);
        put(0);
        put((uint32_t)w * 4);
        put(0);
        put(0);
        end_fd(prime.fd);
        close(prime.fd);
        begin(params[b], 3);
        put(bufs[b]);
        put((uint32_t)w);
        put((uint32_t)h);
        put(0x34325258);
        put(0);
        end();
    }
    printf("waylandcheck: bench start, %d frames of %dx%d\n", frames, w, h);
    fflush(stdout);
    long started = now_ms();
    int shown = 0;
    for (int i = 0; i < frames && !protocol_error; i++) {
        begin(ID_SURFACE, 1);
        put(bufs[i & 1]);
        put(0);
        put(0);
        end();
        begin(ID_SURFACE, 2);
        put(0);
        put(0);
        put((uint32_t)w);
        put((uint32_t)h);
        end();
        frame4_done = 0;
        begin(ID_SURFACE, 3);
        put(ID_FRAME4);
        end();
        begin(ID_SURFACE, 6);
        end();
        if (wait_for(&frame4_done, 2000)) shown++;
    }
    long spent = now_ms() - started;
    printf("waylandcheck: bench %d of %d frames in %ld ms (%ld fps)\n", shown, frames, spent, spent > 0 ? shown * 1000L / spent : 0);
    begin(ID_TOPLEVEL, 0);
    end();
    begin(ID_XDG, 0);
    end();
    begin(ID_SURFACE, 0);
    end();
    close(sock);
    return shown == frames ? 0 : 1;
}

int main(int argc, char **argv) {
    const char *dir = getenv("XDG_RUNTIME_DIR");
    const char *display = getenv("WAYLAND_DISPLAY");
    struct sockaddr_un addr = {0};
    addr.sun_family = AF_UNIX;
    snprintf(addr.sun_path, sizeof addr.sun_path, "%s/%s", dir ? dir : "/run/user/0", display ? display : "wayland-0");
    sock = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    int connected = sock >= 0 && connect(sock, (struct sockaddr *)&addr, sizeof addr) == 0;
    if (argc > 1 && !strcmp(argv[1], "bench"))
        return connected ? bench(argc > 2 ? atoi(argv[2]) : 300) : 1;
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

    /* Во весь экран (20.11): `set_fullscreen` даёт `configure` с состоянием
     * FULLSCREEN и размером монитора; `unset_fullscreen` — прежний размер
     * без него, и стол не разворачивает окно обратно сам. Окно, которое
     * само стало размером с монитор (игры Wine), становится полноэкранным. */
    full_seen = 0;
    configured = 0;
    begin(ID_TOPLEVEL, 11); /* set_fullscreen(output = null) */
    put(0);
    end();
    wait_for(&full_seen, 5000);
    wait_for(&configured, 2000);
    int mon_w = conf_w, mon_h = conf_h;
    printf("waylandcheck: fullscreen configure %dx%d\n", mon_w, mon_h);
    check(full_seen && mon_w > width && mon_h > height, "set_fullscreen gets configure with the fullscreen state and the monitor size");
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    int big = mon_w > 0 && mon_h > 0 && mon_w <= 8192 && mon_h <= 8192;
    size_t full_bytes = big ? (size_t)mon_w * (size_t)mon_h * 4 : 4;
    int full_fd = memfd_create("waylandcheck-full", MFD_CLOEXEC);
    uint32_t *full_px = full_fd >= 0 && ftruncate(full_fd, (off_t)full_bytes) == 0
        ? mmap(NULL, full_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, full_fd, 0) : MAP_FAILED;
    if (full_px != MAP_FAILED && big)
        for (int y = 0; y < mon_h; y++)
            for (int x = 0; x < mon_w; x++) {
                int edge = x < 8 || y < 8 || x >= mon_w - 8 || y >= mon_h - 8;
                full_px[(size_t)y * mon_w + x] = edge ? 0xffff4040u : 0xff203050u + (uint32_t)((x * 96 / mon_w) << 8);
            }
    begin(ID_SHM, 0);
    put(ID_POOL_FULL);
    put((uint32_t)full_bytes);
    end_fd(full_fd);
    begin(ID_POOL_FULL, 0);
    put(ID_BUFFER_FULL);
    put(0);
    put((uint32_t)mon_w);
    put((uint32_t)mon_h);
    put((uint32_t)mon_w * 4);
    put(0);
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER_FULL);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 2);
    put(0);
    put(0);
    put((uint32_t)mon_w);
    put((uint32_t)mon_h);
    end();
    frame_done = 0;
    begin(ID_SURFACE, 3);
    put(ID_FRAME2);
    end();
    begin(ID_SURFACE, 6);
    end();
    printf("waylandcheck: fullscreen shown\n");
    fflush(stdout);
    wait_for(&key_b, 60000);

    full_clear = 0;
    configured = 0;
    begin(ID_TOPLEVEL, 12); /* unset_fullscreen */
    end();
    wait_for(&full_clear, 5000);
    wait_for(&configured, 2000);
    printf("waylandcheck: unset_fullscreen configure %dx%d\n", conf_w, conf_h);
    check(full_clear && conf_w == width && conf_h == height, "unset_fullscreen gives back the window size without the fullscreen state");
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 2);
    put(0);
    put(0);
    put(width);
    put(height);
    end();
    begin(ID_SURFACE, 6);
    end();
    full_seen = 0;
    wait_for(&never, 2000);
    check(!full_seen, "no fullscreen ping-pong after unset_fullscreen");

    /* Сам стал размером с монитор — полноэкранный. */
    begin(ID_POOL_FULL, 0);
    put(ID_BUFFER_MON);
    put(0);
    put((uint32_t)mon_w);
    put((uint32_t)mon_h);
    put((uint32_t)mon_w * 4);
    put(0);
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER_MON);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 2);
    put(0);
    put(0);
    put((uint32_t)mon_w);
    put((uint32_t)mon_h);
    end();
    begin(ID_SURFACE, 6);
    end();
    wait_for(&full_seen, 5000);
    check(full_seen && conf_w == mon_w && conf_h == mon_h, "a monitor-sized window becomes fullscreen");

    /* wp_viewporter (D4, 20.11): Wine изображает режим игры ниже монитора
     * буфером поменьше и `set_destination` в размер монитора. Буфер 320x200
     * с назначением во весь монитор — окно остаётся во весь экран, кадр
     * растянут (раньше: 1:1 в углу и выход из полного экрана). */
    check(global_viewporter != 0, "registry lists wp_viewporter");
    bind_global(global_viewporter, "wp_viewporter", 1, ID_VIEWPORTER);
    begin(ID_VIEWPORTER, 1); /* get_viewport */
    put(ID_VIEWPORT);
    put(ID_SURFACE);
    end();
    begin(ID_VIEWPORT, 2); /* set_destination */
    put((uint32_t)mon_w);
    put((uint32_t)mon_h);
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 9); /* damage_buffer */
    put(0);
    put(0);
    put(width);
    put(height);
    end();
    begin(ID_SURFACE, 3);
    put(ID_FRAME3);
    end();
    full_clear = 0;
    begin(ID_SURFACE, 6);
    end();
    wait_for(&frame3_done, 5000);
    printf("waylandcheck: viewport shown\n");
    fflush(stdout);
    wait_for(&never, 2500);
    check(frame3_done && !full_clear, "a 320x200 buffer with set_destination to the monitor stays fullscreen");

    /* Из полного экрана — туда и тем размером, что было до того, как окно
     * само выросло до монитора (D4: раньше оставалось огромным). */
    full_clear = 0;
    configured = 0;
    begin(ID_TOPLEVEL, 12); /* unset_fullscreen */
    end();
    wait_for(&full_clear, 5000);
    wait_for(&configured, 2000);
    printf("waylandcheck: unset_fullscreen after growing configure %dx%d\n", conf_w, conf_h);
    check(full_clear && conf_w == width && conf_h == height, "unset_fullscreen after growing to the monitor gives back the size before it");
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    begin(ID_VIEWPORT, 0); /* destroy: снова в размер буфера */
    end();
    begin(ID_SURFACE, 1);
    put(ID_BUFFER);
    put(0);
    put(0);
    end();
    begin(ID_SURFACE, 6);
    end();
    full_seen = 0;
    wait_for(&never, 2000);
    check(!full_seen, "the restored window stays windowed");

    /* dma-buf во весь монитор (D4, 20.11): буфер GEM узла DRM (`nvk soft on`
     * в QEMU) — окно показывает его без копии, а прежний буфер возвращает,
     * когда показан следующий. Нет узла — пропуск. */
    int drm = open("/dev/dri/renderD128", O_RDWR | O_CLOEXEC);
    if (drm >= 0 && global_dmabuf && big) {
        bind_global(global_dmabuf, "zwp_linux_dmabuf_v1", 3, ID_DMABUF);
        uint32_t bufs[2] = {ID_DBUF1, ID_DBUF2}, params[2] = {ID_PARAMS1, ID_PARAMS2};
        int made = 0;
        for (int b = 0; b < 2; b++) {
            /* drm_nouveau_gem_new: info {handle, domain, size, offset,
             * map_handle, tile_mode, tile_flags}, channel_hint, align. */
            struct { uint32_t handle, domain; uint64_t size, offset, map_handle; uint32_t tile_mode, tile_flags, channel_hint, align; } gem = {0};
            gem.domain = 4 | 8; /* GART | MAPPABLE */
            gem.size = full_bytes;
            struct { uint32_t handle, flags; int32_t fd; } prime = {0};
            if (ioctl(drm, 0xC0306480, &gem) != 0) break;
            uint32_t *px = mmap(NULL, full_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, drm, (off_t)gem.map_handle);
            if (px == MAP_FAILED) break;
            for (int y = 0; y < mon_h; y++)
                for (int x = 0; x < mon_w; x++)
                    px[(size_t)y * mon_w + x] = b ? 0x00305030u + (uint32_t)(x * 128 / mon_w) : 0x00503030u + (uint32_t)((y * 128 / mon_h) << 8);
            munmap(px, full_bytes);
            prime.handle = gem.handle;
            prime.flags = 02000000; /* O_CLOEXEC */
            if (ioctl(drm, 0xC00C642D, &prime) != 0) break;
            begin(ID_DMABUF, 1); /* create_params */
            put(params[b]);
            end();
            begin(params[b], 1); /* add(fd, plane, offset, stride, mod_hi, mod_lo) */
            put(0);
            put(0);
            put((uint32_t)mon_w * 4);
            put(0);
            put(0);
            end_fd(prime.fd);
            close(prime.fd);
            begin(params[b], 3); /* create_immed(id, w, h, XRGB8888, flags) */
            put(bufs[b]);
            put((uint32_t)mon_w);
            put((uint32_t)mon_h);
            put(0x34325258);
            put(0);
            end();
            made++;
        }
        check(made == 2, "two monitor-sized GEM buffers are shared as dma-buf");
        for (int b = 0; b < 2 && made == 2; b++) {
            begin(ID_SURFACE, 1);
            put(bufs[b]);
            put(0);
            put(0);
            end();
            begin(ID_SURFACE, 2);
            put(0);
            put(0);
            put((uint32_t)mon_w);
            put((uint32_t)mon_h);
            end();
            frame4_done = 0;
            begin(ID_SURFACE, 3);
            put(ID_FRAME4);
            end();
            begin(ID_SURFACE, 6);
            end();
            wait_for(&frame4_done, 5000);
            if (b == 0) {
                printf("waylandcheck: dma-buf shown\n");
                fflush(stdout);
                struct timespec pause = {1, 500000000};
                nanosleep(&pause, NULL);
            }
        }
        wait_for(&dbuf1_released, 3000);
        check(made == 2 && frame4_done && dbuf1_released, "a monitor-sized dma-buf is shown and released once the next one is shown");
    } else {
        printf("waylandcheck: no /dev/dri/renderD128 - dma-buf check skipped\n");
    }

    /* G3 (21.11): mouse for games. Wine asks for these four globals (hw-log13:
     * "relative motion won't work", "pointer locking/confining won't work",
     * xdg_activation_v1, wp_fractional_scale_manager_v1). */
    check(global_relative && global_constraints && global_activation && global_fractional,
          "registry lists relative pointer, pointer constraints, xdg_activation and fractional scale");
    if (global_fractional) {
        bind_global(global_fractional, "wp_fractional_scale_manager_v1", 1, ID_FRACMGR);
        begin(ID_FRACMGR, 1); /* get_fractional_scale(id, surface) */
        put(ID_FRAC);
        put(ID_SURFACE);
        end();
    }
    if (global_activation) {
        bind_global(global_activation, "xdg_activation_v1", 1, ID_ACT);
        begin(ID_ACT, 1); /* get_activation_token */
        put(ID_TOKEN);
        end();
        begin(ID_TOKEN, 2); /* set_surface */
        put(ID_SURFACE);
        end();
        begin(ID_TOKEN, 3); /* commit -> done(token) */
        end();
        wait_for(&token_done, 3000);
        begin(ID_ACT, 2); /* activate(token, surface) */
        put_str("fossai-check");
        put(ID_SURFACE);
        end();
    }
    begin(ID_DISPLAY, 0);
    put(ID_SYNC3);
    end();
    wait_for(&synced3, 5000);
    printf("waylandcheck: preferred scale %d\n", frac_scale);
    check(frac_scale == 120, "wp_fractional_scale_v1 announces the preferred scale 1.0 (120)");
    check(token_done && !protocol_error, "xdg_activation_v1 hands out a token and activates the window");

    if (global_relative && global_constraints) {
        bind_global(global_relative, "zwp_relative_pointer_manager_v1", 1, ID_RELMGR);
        begin(ID_RELMGR, 1); /* get_relative_pointer(id, pointer) */
        put(ID_REL);
        put(ID_POINTER);
        end();
        bind_global(global_constraints, "zwp_pointer_constraints_v1", 1, ID_CONSTR);
        begin(ID_CONSTR, 1); /* lock_pointer(id, surface, pointer, region, lifetime) */
        put(ID_LOCK);
        put(ID_SURFACE);
        put(ID_POINTER);
        put(0);
        put(2); /* persistent */
        end();
        wait_for(&locked, 5000);
        check(locked, "lock_pointer gets locked while the window has the focus and the pointer");
        printf("waylandcheck: waiting for relative motion\n");
        fflush(stdout);
        rel_seen = 0;
        rel_dx = rel_dy = 0;
        motion_locked = 0;
        wait_for(&rel_seen, 60000);
        wait_for(&never, 1000);
        printf("waylandcheck: relative motion %d,%d, %d absolute motions while locked\n", rel_dx, rel_dy, motion_locked);
        check(rel_seen && (rel_dx != 0 || rel_dy != 0), "zwp_relative_pointer_v1 gets the mouse motion while the pointer is locked");
        check(motion_locked == 0, "no wl_pointer.motion while the pointer is locked");
        /* Win (the Start menu) releases the lock; closing it brings the
         * persistent lock back. */
        printf("waylandcheck: waiting for the unlock\n");
        fflush(stdout);
        wait_for(&unlocked, 60000);
        check(unlocked, "the Start menu (Win) unlocks the pointer");
        locked = 0;
        printf("waylandcheck: waiting for the lock to come back\n");
        fflush(stdout);
        wait_for(&locked, 60000);
        check(locked, "the persistent lock comes back when the window is in front again");
        begin(ID_LOCK, 0); /* destroy */
        end();

        /* Confine to a 64x64 region of the window, oneshot. */
        begin(ID_COMPOSITOR, 1); /* create_region */
        put(ID_REGION);
        end();
        begin(ID_REGION, 1); /* add(x, y, w, h) */
        put(0);
        put(0);
        put(64);
        put(64);
        end();
        begin(ID_CONSTR, 2); /* confine_pointer(id, surface, pointer, region, lifetime) */
        put(ID_CONFINE);
        put(ID_SURFACE);
        put(ID_POINTER);
        put(ID_REGION);
        put(1); /* oneshot */
        end();
        begin(ID_REGION, 0); /* destroy: the constraint keeps a copy */
        end();
        printf("waylandcheck: waiting for the confinement\n");
        fflush(stdout);
        wait_for(&confined, 60000);
        check(confined, "confine_pointer with a region gets confined when the pointer enters it");
        printf("waylandcheck: waiting for confined motion\n");
        fflush(stdout);
        /* The pointer is pushed far right and down: it must stop at the
         * region's edge (63, 63). */
        wait_for(&confine_moved, 60000);
        wait_for(&never, 1500);
        printf("waylandcheck: confined pointer reached %d,%d\n", confine_max_x, confine_max_y);
        check(confine_moved && confine_max_x > 0 && confine_max_x < 64 && confine_max_y < 64, "the confined pointer stays inside the region");
        begin(ID_CONFINE, 0); /* destroy */
        end();
    }

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
