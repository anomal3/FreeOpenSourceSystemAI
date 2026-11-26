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
    ID_OUTPUT,
    /* G5 (21.11): popups on the second monitor, minimize, confinement. */
    ID_POS, ID_PSURF, ID_PXDG, ID_POPUP, ID_PPOOL, ID_PBUF, ID_TOKEN2, ID_CONFINE2, ID_SYNC4,
    /* G5 (22.11): one wl_output per monitor, xdg_output, enter/leave,
     * xdg_popup.reposition, outputs switched off and on. */
    ID_XOMGR, ID_OUT0, ID_OUT1, ID_XOUT0, ID_XOUT1, ID_SYNC5, ID_POS2, ID_PPOOL2, ID_PBUF2, ID_OUT2, ID_SYNC6,
    /* Clipboard (24.11): wl_data_device_manager, the device, our source. */
    ID_DDM, ID_DDEV, ID_SRC, ID_SYNC7,
    /* ext_data_control_manager_v1 (Wine's clipboard sync) and its device. */
    ID_DCM, ID_DCDEV,
    /* 26.11: own cursor image (wl_pointer.set_cursor with a surface). */
    ID_CSURF, ID_CPOOL, ID_CBUF, ID_SYNC8
};
static int synced8;
static uint32_t global_dcm, dc_offer;
static int dc_text, dc_gnome, dc_sel, dc_files;
/* Clipboard: the current offer and what it offers, the selection offer,
 * keyboard enter serial, sends our source was asked for, cancelled. */
static uint32_t global_ddm, cur_offer, sel_offer;
static int cur_text, cur_utf8, cur_uri, cur_gnome, sel_text, sel_utf8, sel_uri, sel_gnome, sel_events, kb_serial;
static int src_sends, src_cancelled, synced7;
static const char *clip_text = "from-wayland-42";
/* G5 outputs: registry names, what each bound one said (index 0/1 = ID_OUT0/1,
 * 2 = ID_OUT2 bound after the hotplug). */
#define MAX_OUT 4
static uint32_t out_names[MAX_OUT];
static int n_out;
static uint32_t global_xomgr, removed_name, added_name;
static int out_removed, out_added;
struct out_info {
    int x, y, mm_w, mm_h, w, h, mhz, done, lx, ly, lw, lh, xdone, entered;
    char make[32], model[64], name[32], desc[96];
};
static struct out_info outs[3];
static int enter_events, leave_events, repositioned_token;
static uint32_t left_out = ID_OUT0, right_out = ID_OUT1;
static int on_right_only, on_left_only, synced5, synced6, key_r;
/* G5: `wm_capabilities`, popup configure, pointer buttons/leave, focus. */
static int caps_seen, caps_min, caps_full, caps_before_configure;
static int popup_x = -99999, popup_y = -99999, popup_conf, pxdg_serial, pxdg_conf, popup_done;
static int button_serial, button_down, pointer_left, keyboard_left, key_p, key_q, key_c;
static int confined2, confine2_on, confine2_moves, confine2_min_x = 1 << 30, leave_confined, token2_done, synced4;
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
        if (!strcmp(interface, "wl_output")) {
            if (!global_output) global_output = name;
            if (n_out < MAX_OUT) out_names[n_out++] = name;
            if (removed_name) {
                added_name = name;
                out_added = 1;
            }
        }
        if (!strcmp(interface, "zxdg_output_manager_v1")) global_xomgr = name;
        if (!strcmp(interface, "wl_data_device_manager")) global_ddm = name;
        if (!strcmp(interface, "ext_data_control_manager_v1")) global_dcm = name;
    } else if (id == ID_REGISTRY && opcode == 1) {
        /* global_remove(name) */
        for (int i = 0; i < n_out; i++)
            if (out_names[i] == args[0]) {
                removed_name = args[0];
                out_removed = 1;
            }
    } else if ((id == ID_OUT0 || id == ID_OUT1 || id == ID_OUT2) && opcode <= 5) {
        struct out_info *o = &outs[id == ID_OUT0 ? 0 : id == ID_OUT1 ? 1 : 2];
        if (opcode == 0) {
            /* geometry(x, y, mm_w, mm_h, subpixel, make, model, transform) */
            o->x = (int32_t)args[0];
            o->y = (int32_t)args[1];
            o->mm_w = (int32_t)args[2];
            o->mm_h = (int32_t)args[3];
            uint32_t len = args[5];
            snprintf(o->make, sizeof o->make, "%s", (char *)&args[6]);
            snprintf(o->model, sizeof o->model, "%s", (char *)&args[6 + (len + 3) / 4 + 1]);
        } else if (opcode == 1) {
            o->w = (int)args[1];
            o->h = (int)args[2];
            o->mhz = (int)args[3];
        } else if (opcode == 2) {
            o->done++;
        } else if (opcode == 4) {
            snprintf(o->name, sizeof o->name, "%s", (char *)&args[1]);
        } else if (opcode == 5) {
            snprintf(o->desc, sizeof o->desc, "%s", (char *)&args[1]);
        }
    } else if ((id == ID_XOUT0 || id == ID_XOUT1) && opcode <= 1) {
        struct out_info *o = &outs[id == ID_XOUT0 ? 0 : 1];
        if (opcode == 0) {
            o->lx = (int32_t)args[0];
            o->ly = (int32_t)args[1];
        } else {
            o->lw = (int32_t)args[0];
            o->lh = (int32_t)args[1];
        }
    } else if (id == ID_SURFACE && (opcode == 0 || opcode == 1)) {
        /* enter(output) / leave(output) */
        uint32_t out = args[0];
        int k = out == ID_OUT0 ? 0 : out == ID_OUT1 ? 1 : out == ID_OUT2 ? 2 : -1;
        if (k >= 0) outs[k].entered = opcode == 0;
        if (opcode == 0) enter_events++;
        else leave_events++;
        int l = left_out == ID_OUT0 ? 0 : 1, r = 1 - l;
        on_right_only = outs[r].entered && !outs[l].entered;
        on_left_only = outs[l].entered && !outs[r].entered;
    } else if (id == ID_DCDEV && opcode == 0) {
        dc_offer = args[0];
        dc_text = dc_gnome = 0;
    } else if (dc_offer && id == dc_offer && opcode == 0) {
        if (!strcmp((const char *)&args[1], "text/plain;charset=utf-8")) dc_text = 1;
        if (!strcmp((const char *)&args[1], "x-special/gnome-copied-files")) dc_gnome = 1;
    } else if (id == ID_DCDEV && opcode == 1) {
        dc_sel = args[0] && args[0] == dc_offer && dc_text;
        dc_files = args[0] && args[0] == dc_offer && dc_gnome;
    } else if (id == ID_DDEV && opcode == 0) {
        /* data_offer(id) - its mime types follow */
        cur_offer = args[0];
        cur_text = cur_utf8 = cur_uri = cur_gnome = 0;
    } else if (cur_offer && id == cur_offer && opcode == 0) {
        const char *mime = (const char *)&args[1];
        if (!strcmp(mime, "text/plain;charset=utf-8")) cur_text = 1;
        if (!strcmp(mime, "UTF8_STRING")) cur_utf8 = 1;
        if (!strcmp(mime, "text/uri-list")) cur_uri = 1;
        if (!strcmp(mime, "x-special/gnome-copied-files")) cur_gnome = 1;
    } else if (id == ID_DDEV && opcode == 5) {
        /* selection(offer or null) */
        sel_offer = args[0];
        sel_text = sel_offer && sel_offer == cur_offer && cur_text;
        sel_utf8 = sel_offer && sel_offer == cur_offer && cur_utf8;
        sel_uri = sel_offer && sel_offer == cur_offer && cur_uri;
        sel_gnome = sel_offer && sel_offer == cur_offer && cur_gnome;
        sel_events++;
    } else if (id == ID_SRC && opcode == 1) {
        /* send(mime, fd): write our selection and close */
        const char *mime = (const char *)&args[1];
        int fd = nfds > 0 ? fds[0] : -1;
        if (nfds > 0) memmove(fds, fds + 1, sizeof(int) * (size_t)--nfds);
        if (fd >= 0) {
            const char *data = !strcmp(mime, "text/uri-list") ? "file:///tmp/clip%20test.txt\r\n" : clip_text;
            ssize_t w = write(fd, data, strlen(data));
            (void)w;
            close(fd);
        }
        src_sends++;
    } else if (id == ID_SRC && opcode == 2) {
        src_cancelled = 1;
    } else if (id == ID_SYNC7 && opcode == 0) {
        synced7 = 1;
    } else if (id == ID_SYNC8 && opcode == 0) {
        synced8 = 1;
    } else if (id == ID_POPUP && opcode == 2) {
        repositioned_token = (int)args[0];
    } else if (id == ID_SYNC5 && opcode == 0) {
        synced5 = 1;
    } else if (id == ID_SYNC6 && opcode == 0) {
        synced6 = 1;
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
    } else if (id == ID_TOPLEVEL && opcode == 3) {
        /* wm_capabilities(array): 2 maximize, 3 fullscreen, 4 minimize. */
        caps_seen = 1;
        caps_before_configure = !configured;
        for (uint32_t i = 0; i < args[0] / 4; i++) {
            if (args[1 + i] == 3) caps_full = 1;
            if (args[1 + i] == 4) caps_min = 1;
        }
    } else if (id == ID_POPUP && opcode == 0) {
        popup_x = (int32_t)args[0];
        popup_y = (int32_t)args[1];
        popup_conf = 1;
    } else if (id == ID_POPUP && opcode == 1) {
        popup_done = 1;
    } else if (id == ID_PXDG && opcode == 0) {
        pxdg_serial = (int)args[0];
        pxdg_conf = 1;
    } else if (id == ID_TOKEN2 && opcode == 0) {
        token2_done = 1;
    } else if (id == ID_SYNC4 && opcode == 0) {
        synced4 = 1;
    } else if (id == ID_CONFINE2 && opcode == 0) {
        confined2 = 1;
        confine2_on = 1;
    } else if (id == ID_CONFINE2 && opcode == 1) {
        confine2_on = 0;
    } else if (id == ID_POINTER && opcode == 1) {
        pointer_left = 1;
        if (confine2_on) leave_confined = 1;
    } else if (id == ID_POINTER && opcode == 3) {
        /* button(serial, time, button, state) */
        if (args[3] == 1) {
            button_serial = (int)args[0];
            button_down = 1;
        }
    } else if (id == ID_KEYBOARD && opcode == 2) {
        keyboard_left = 1;
    } else if (id == ID_POINTER && opcode == 2) {
        /* motion(time, x, y) */
        int x = (int32_t)args[1] / 256, y = (int32_t)args[2] / 256;
        if (confine2_on) {
            confine2_moves++;
            if (x < confine2_min_x) confine2_min_x = x;
        }
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
        kb_serial = (int)args[0];
    } else if (id == ID_KEYBOARD && opcode == 3) {
        if (args[2] == 30 && args[3] == 1) key_down = 1;
        if (args[2] == 30 && args[3] == 0 && key_down) key_up = 1;
        if (args[2] == 48 && args[3] == 0) key_b = 1;
        if (args[2] == 25 && args[3] == 0) key_p = 1;
        if (args[2] == 16 && args[3] == 0) key_q = 1;
        if (args[2] == 46 && args[3] == 0) key_c = 1;
        if (args[2] == 19 && args[3] == 0) key_r = 1;
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

/* A memfd pool with one `w`x`h` ARGB buffer filled with `color` (white
 * 3-pixel border). */
static void shm_buffer(uint32_t pool, uint32_t buffer, int w, int h, uint32_t color) {
    size_t bytes = (size_t)w * (size_t)h * 4;
    int fd = memfd_create("waylandcheck-g5", MFD_CLOEXEC);
    uint32_t *px = fd >= 0 && ftruncate(fd, (off_t)bytes) == 0 ? mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0) : MAP_FAILED;
    if (px != MAP_FAILED) {
        for (int y = 0; y < h; y++)
            for (int x = 0; x < w; x++)
                px[(size_t)y * w + x] = (x < 3 || y < 3 || x >= w - 3 || y >= h - 3) ? 0xffffffffu : color;
        munmap(px, bytes);
    }
    begin(ID_SHM, 0); /* create_pool */
    put(pool);
    put((uint32_t)bytes);
    end_fd(fd);
    if (fd >= 0) close(fd);
    begin(pool, 0); /* create_buffer */
    put(buffer);
    put(0);
    put((uint32_t)w);
    put((uint32_t)h);
    put((uint32_t)w * 4);
    put(0);
    end();
}

static void show(uint32_t surface, uint32_t buffer, int w, int h) {
    begin(surface, 1); /* attach */
    put(buffer);
    put(0);
    put(0);
    end();
    begin(surface, 2); /* damage */
    put(0);
    put(0);
    put((uint32_t)w);
    put((uint32_t)h);
    end();
    begin(surface, 6); /* commit */
    end();
}

static void say(const char *text) {
    printf("waylandcheck: %s\n", text);
    fflush(stdout);
}

/* `waylandcheck g5` (21.11, hw: two monitors, the primary on the left):
 * wm_capabilities before the first configure (GE-Proton keeps minimized
 * windows mapped without it), an xdg_popup of a window dragged to the right
 * monitor opens at its anchor (it was pushed onto the primary monitor),
 * set_minimized and back with xdg_activation, a confinement asked while the
 * pointer is off the window starts at once and holds the pointer in. The
 * scenario `wayland-multihead` drives it. */
static int g5(void) {
    begin(ID_DISPLAY, 1);
    put(ID_REGISTRY);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC);
    end();
    wait_for(&synced, 5000);
    check(synced && global_compositor && global_shm && global_wm && global_seat && global_constraints && global_activation,
          "g5 registry lists the compositor, shm, xdg_wm_base, seat, pointer constraints and xdg_activation");
    bind_global(global_compositor, "wl_compositor", 4, ID_COMPOSITOR);
    bind_global(global_shm, "wl_shm", 1, ID_SHM);
    bind_global(global_wm, "xdg_wm_base", 5, ID_WM);
    bind_global(global_seat, "wl_seat", 5, ID_SEAT);
    bind_global(global_constraints, "zwp_pointer_constraints_v1", 1, ID_CONSTR);
    bind_global(global_activation, "xdg_activation_v1", 1, ID_ACT);
    begin(ID_SEAT, 0);
    put(ID_POINTER);
    end();
    begin(ID_SEAT, 1);
    put(ID_KEYBOARD);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC2);
    end();
    wait_for(&synced2, 5000);

    /* G5 (22.11): Wine builds EnumDisplayMonitors from one wl_output per
     * monitor and their xdg_output logical positions (the primary at 0,0). */
    printf("waylandcheck: %d wl_output global(s), xdg_output manager %s\n", n_out, global_xomgr ? "yes" : "no");
    check(n_out == 2 && global_xomgr, "registry lists one wl_output per monitor (2) and zxdg_output_manager_v1");
    if (n_out >= 2 && global_xomgr) {
        bind_global(out_names[0], "wl_output", 4, ID_OUT0);
        bind_global(out_names[1], "wl_output", 4, ID_OUT1);
        bind_global(global_xomgr, "zxdg_output_manager_v1", 3, ID_XOMGR);
        begin(ID_XOMGR, 1); /* get_xdg_output(id, output) */
        put(ID_XOUT0);
        put(ID_OUT0);
        end();
        begin(ID_XOMGR, 1);
        put(ID_XOUT1);
        put(ID_OUT1);
        end();
        begin(ID_DISPLAY, 0);
        put(ID_SYNC5);
        end();
        wait_for(&synced5, 5000);
        for (int i = 0; i < 2; i++)
            printf("waylandcheck: output %d '%s' '%s' %s %s %dx%d@%d mm %dx%d at %d,%d logical %d,%d %dx%d done %d\n", i, outs[i].name,
                   outs[i].desc, outs[i].make, outs[i].model, outs[i].w, outs[i].h, outs[i].mhz, outs[i].mm_w, outs[i].mm_h, outs[i].x,
                   outs[i].y, outs[i].lx, outs[i].ly, outs[i].lw, outs[i].lh, outs[i].done);
        int l = outs[0].x <= outs[1].x ? 0 : 1, r = 1 - l;
        left_out = l == 0 ? ID_OUT0 : ID_OUT1;
        right_out = r == 0 ? ID_OUT0 : ID_OUT1;
        check(outs[l].x == 0 && outs[l].y == 0 && outs[r].x == 1280 && outs[r].y == 0 && outs[l].w == 1280 && outs[l].h == 800 &&
                  outs[r].w == 1280 && outs[r].h == 800 && outs[0].mhz > 0 && outs[1].mhz > 0,
              "wl_output geometry and mode: the primary at 0,0 and the second at 1280,0, both 1280x800");
        check(outs[0].done >= 2 && outs[1].done >= 2 && outs[0].name[0] && outs[1].name[0] && strcmp(outs[0].name, outs[1].name) &&
                  outs[0].desc[0] && outs[0].make[0] && outs[0].model[0] && outs[0].mm_w > 0,
              "wl_output v4 sends distinct names, descriptions, make/model and done");
        check(outs[l].lx == 0 && outs[l].ly == 0 && outs[r].lx == 1280 && outs[r].ly == 0 && outs[0].lw == 1280 && outs[0].lh == 800 &&
                  outs[1].lw == 1280 && outs[1].lh == 800,
              "zxdg_output_v1 logical positions and sizes match the layout");
    }

    const int width = 320, height = 200;
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
    put_str("waylandcheck");
    end();
    begin(ID_SURFACE, 6);
    end();
    wait_for(&configured, 5000);
    check(caps_seen && caps_before_configure && caps_min && caps_full,
          "xdg_toplevel gets wm_capabilities with fullscreen and minimize before the first configure");
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    shm_buffer(ID_POOL, ID_BUFFER, width, height, 0xff305090u);
    frame_done = 0;
    begin(ID_SURFACE, 3);
    put(ID_FRAME);
    end();
    show(ID_SURFACE, ID_BUFFER, width, height);
    wait_for(&frame_done, 5000);
    wait_for(&on_left_only, 3000);
    check(on_left_only, "the new window gets wl_surface.enter for the primary (left) monitor only");

    /* Drag to the right monitor: a press on the window asks the desktop to
     * move it (`xdg_toplevel.move`, like Wine's caption). */
    say("g5 window shown, drag it to the right monitor");
    wait_for(&button_down, 60000);
    begin(ID_TOPLEVEL, 5); /* move(seat, serial) */
    put(ID_SEAT);
    put((uint32_t)button_serial);
    end();
    wait_for(&key_p, 60000);
    wait_for(&on_right_only, 3000);
    printf("waylandcheck: enter %d, leave %d\n", enter_events, leave_events);
    check(on_right_only && leave_events > 0, "dragged to the right monitor, the window enters its output and leaves the left one");

    /* A 200x150 menu below-right of the point (300,150) of the window, with
     * flip and slide allowed: it fits on the right monitor, so it opens
     * exactly there. */
    begin(ID_WM, 1); /* create_positioner */
    put(ID_POS);
    end();
    begin(ID_POS, 1); /* set_size */
    put(200);
    put(150);
    end();
    begin(ID_POS, 2); /* set_anchor_rect */
    put(300);
    put(150);
    put(1);
    put(1);
    end();
    begin(ID_POS, 3); /* set_anchor: top_left (5) */
    put(5);
    end();
    begin(ID_POS, 4); /* set_gravity: bottom_right (8) */
    put(8);
    end();
    begin(ID_POS, 5); /* set_constraint_adjustment: slide x/y, flip x/y */
    put(1 | 2 | 4 | 8);
    end();
    begin(ID_COMPOSITOR, 0);
    put(ID_PSURF);
    end();
    begin(ID_WM, 2);
    put(ID_PXDG);
    put(ID_PSURF);
    end();
    begin(ID_PXDG, 2); /* get_popup(id, parent, positioner) */
    put(ID_POPUP);
    put(ID_XDG);
    put(ID_POS);
    end();
    begin(ID_PSURF, 6);
    end();
    wait_for(&pxdg_conf, 5000);
    printf("waylandcheck: popup configure at %d,%d\n", popup_x, popup_y);
    check(popup_conf && popup_x == 300 && popup_y == 150,
          "an xdg_popup of a window on the right monitor opens at its anchor, not pushed onto the primary monitor");
    begin(ID_PXDG, 4);
    put((uint32_t)pxdg_serial);
    end();
    shm_buffer(ID_PPOOL, ID_PBUF, 200, 150, 0xffc06020u);
    show(ID_PSURF, ID_PBUF, 200, 150);
    say("g5 popup shown");
    wait_for(&key_r, 60000);

    /* xdg_popup.reposition (v3): a 160x100 menu at the point (20,30) - the
     * reply is repositioned(token), then configure at the new place. */
    begin(ID_WM, 1);
    put(ID_POS2);
    end();
    begin(ID_POS2, 1);
    put(160);
    put(100);
    end();
    begin(ID_POS2, 2);
    put(20);
    put(30);
    put(1);
    put(1);
    end();
    begin(ID_POS2, 3);
    put(5);
    end();
    begin(ID_POS2, 4);
    put(8);
    end();
    begin(ID_POS2, 5);
    put(1 | 2 | 4 | 8);
    end();
    popup_conf = 0;
    pxdg_conf = 0;
    begin(ID_POPUP, 2); /* reposition(positioner, token) */
    put(ID_POS2);
    put(77);
    end();
    wait_for(&pxdg_conf, 5000);
    printf("waylandcheck: repositioned token %d, configure at %d,%d\n", repositioned_token, popup_x, popup_y);
    check(repositioned_token == 77 && popup_conf && popup_x == 20 && popup_y == 30,
          "xdg_popup.reposition answers repositioned(token) and a configure at the new place");
    begin(ID_PXDG, 4);
    put((uint32_t)pxdg_serial);
    end();
    shm_buffer(ID_PPOOL2, ID_PBUF2, 160, 100, 0xff20a040u);
    show(ID_PSURF, ID_PBUF2, 160, 100);
    say("g5 popup repositioned");
    wait_for(&key_q, 60000);
    begin(ID_POPUP, 0);
    end();
    begin(ID_PXDG, 0);
    end();
    begin(ID_PSURF, 0);
    end();

    /* Minimize (Wine: a minimized game) and back through xdg_activation. */
    keyboard_left = 0;
    begin(ID_TOPLEVEL, 13); /* set_minimized */
    end();
    wait_for(&keyboard_left, 5000);
    check(keyboard_left, "set_minimized minimizes the window (it loses the keyboard)");
    say("g5 minimized");
    struct timespec pause = {1, 0};
    nanosleep(&pause, NULL);
    begin(ID_ACT, 1);
    put(ID_TOKEN2);
    end();
    begin(ID_TOKEN2, 3);
    end();
    wait_for(&token2_done, 3000);
    keyboard_in = 0;
    configured = 0;
    begin(ID_ACT, 2);
    put_str("fossai-check");
    put(ID_SURFACE);
    end();
    wait_for(&keyboard_in, 5000);
    wait_for(&configured, 2000);
    check(keyboard_in && configured, "activation restores the minimized window with a configure (Wine restores on it)");
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    show(ID_SURFACE, ID_BUFFER, width, height);

    /* Confinement asked while the pointer is off the window (on the other
     * monitor): it starts at once, the pointer is brought in and stays. */
    say("g5 move the pointer off the window");
    wait_for(&key_c, 60000);
    begin(ID_CONSTR, 2); /* confine_pointer(id, surface, pointer, region = whole, persistent) */
    put(ID_CONFINE2);
    put(ID_SURFACE);
    put(ID_POINTER);
    put(0);
    put(2);
    end();
    wait_for(&confined2, 5000);
    check(confined2, "a confinement asked while the pointer is off the window starts at once");
    say("g5 confined, push the pointer away");
    struct timespec pause2 = {3, 0};
    nanosleep(&pause2, NULL);
    begin(ID_DISPLAY, 0);
    put(ID_SYNC4);
    end();
    wait_for(&synced4, 3000);
    wait_for(&never, 1500);
    printf("waylandcheck: confined motions %d, leftmost x %d, left the window %d\n", confine2_moves, confine2_min_x, leave_confined);
    check(confine2_on && !leave_confined && confine2_moves > 0 && confine2_min_x == 0,
          "the confined pointer stops at the window's edge and does not leave it");
    say("g5 shot");
    wait_for(&never, 2500);
    begin(ID_CONFINE2, 0);
    end();

    /* Settings -> Display: the right monitor switched off and on again -
     * its global goes (the window comes to the left monitor) and a new one
     * comes with the same place. */
    if (n_out >= 2) {
        uint32_t right_name = right_out == ID_OUT0 ? out_names[0] : out_names[1];
        /* The scenario's command lines go to the focused window: minimized,
         * the terminal gets them. */
        keyboard_left = 0;
        begin(ID_TOPLEVEL, 13); /* set_minimized */
        end();
        wait_for(&keyboard_left, 5000);
        say("g5 switch the right monitor off");
        wait_for(&out_removed, 30000);
        printf("waylandcheck: global_remove %u (right output %u)\n", removed_name, right_name);
        check(out_removed && removed_name == right_name, "switching the right monitor off removes its wl_output global");
        /* Back from the panel (activation): it is on the left monitor now. */
        keyboard_in = 0;
        begin(ID_ACT, 2);
        put_str("fossai-check");
        put(ID_SURFACE);
        end();
        wait_for(&on_left_only, 5000);
        check(on_left_only, "the window moved off the switched-off monitor enters the left output");
        begin(right_out, 0); /* release */
        end();
        /* Minimized again once restored (its restore flight is over). */
        wait_for(&keyboard_in, 5000);
        wait_for(&never, 1000);
        keyboard_left = 0;
        begin(ID_TOPLEVEL, 13);
        end();
        wait_for(&keyboard_left, 5000);
        say("g5 switch the right monitor on");
        wait_for(&out_added, 30000);
        check(out_added && added_name != right_name, "switching it on again announces a new wl_output global");
        if (out_added) {
            bind_global(added_name, "wl_output", 4, ID_OUT2);
            begin(ID_DISPLAY, 0);
            put(ID_SYNC6);
            end();
            wait_for(&synced6, 5000);
            printf("waylandcheck: new output '%s' %dx%d at %d,%d\n", outs[2].name, outs[2].w, outs[2].h, outs[2].x, outs[2].y);
            check(outs[2].x == 1280 && outs[2].y == 0 && outs[2].w == 1280 && outs[2].h == 800 && outs[2].done > 0,
                  "the new wl_output has the right monitor's place and mode");
        }
    }
    begin(ID_TOPLEVEL, 0);
    end();
    begin(ID_XDG, 0);
    end();
    begin(ID_SURFACE, 0);
    end();
    wait_for(&never, 500);
    check(!protocol_error, "no protocol errors");
    close(sock);
    printf("waylandcheck: %d failed\n", failed);
    return failed;
}

/* Receive the selection offer as `mime` through a pipe: what the owner wrote
 * (NUL-terminated in `out`), or -1. The server writes asynchronously, so the
 * pipe is read with a deadline. */
static int clip_receive_from(uint32_t offer, uint32_t opcode, const char *mime, char *out, int room) {
    int p[2];
    if (!offer || pipe2(p, O_CLOEXEC) != 0) return -1;
    begin(offer, opcode); /* receive(mime, fd) */
    put_str(mime);
    end_fd(p[1]);
    close(p[1]);
    int got = 0;
    long until = now_ms() + 5000;
    while (got < room - 1) {
        struct pollfd pfd = {p[0], POLLIN, 0};
        long left = until - now_ms();
        if (left <= 0 || poll(&pfd, 1, (int)left) <= 0) break;
        ssize_t n = read(p[0], out + got, (size_t)(room - 1 - got));
        if (n <= 0) break;
        got += (int)n;
    }
    close(p[0]);
    out[got] = 0;
    return got;
}

static int clip_receive(const char *mime, char *out, int room) {
    return clip_receive_from(sel_offer, 1, mime, out, room);
}

/* `waylandcheck clip TEXT` (24.11): the one clipboard with native programs.
 * The scenario `wayland-clipboard` puts TEXT on the system clipboard
 * (`clip set`), clicks the window; the client must get it offered as
 * text/plain;charset=utf-8 and UTF8_STRING and read it. Then the client sets
 * its own selection (text + text/uri-list) - the server reads it onto the
 * system clipboard (the scenario checks `clip`); then the scenario puts files
 * on the system clipboard (`clip files`) - the client gets text/uri-list and
 * its own source is cancelled. */
static int clip(const char *expect) {
    begin(ID_DISPLAY, 1);
    put(ID_REGISTRY);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC);
    end();
    wait_for(&synced, 5000);
    check(synced && global_compositor && global_shm && global_wm && global_seat && global_ddm,
          "clip registry lists wl_data_device_manager");
    check(global_dcm != 0, "clip registry lists ext_data_control_manager_v1");
    bind_global(global_compositor, "wl_compositor", 4, ID_COMPOSITOR);
    bind_global(global_shm, "wl_shm", 1, ID_SHM);
    bind_global(global_wm, "xdg_wm_base", 2, ID_WM);
    bind_global(global_seat, "wl_seat", 5, ID_SEAT);
    bind_global(global_ddm, "wl_data_device_manager", 3, ID_DDM);
    begin(ID_SEAT, 0);
    put(ID_POINTER);
    end();
    begin(ID_SEAT, 1);
    put(ID_KEYBOARD);
    end();
    begin(ID_DDM, 1); /* get_data_device(id, seat) */
    put(ID_DDEV);
    put(ID_SEAT);
    end();
    /* Data control: the selection comes at once, without keyboard focus. */
    if (global_dcm) {
        bind_global(global_dcm, "ext_data_control_manager_v1", 1, ID_DCM);
        begin(ID_DCM, 1); /* get_data_device(id, seat) */
        put(ID_DCDEV);
        put(ID_SEAT);
        end();
        wait_for(&dc_sel, 3000);
        check(dc_sel, "a data-control device gets the native selection without keyboard focus");
        char dcbuf[256];
        int n = clip_receive_from(dc_offer, 0, "text/plain;charset=utf-8", dcbuf, sizeof dcbuf);
        check(n >= 0 && !strcmp(dcbuf, expect), "native clipboard text reaches the client through data control");
    }
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
    put_str("waylandcheck");
    end();
    begin(ID_SURFACE, 6);
    end();
    wait_for(&configured, 5000);
    begin(ID_XDG, 4);
    put((uint32_t)configure_serial);
    end();
    shm_buffer(ID_POOL, ID_BUFFER, 320, 200, 0xff3060a0u);
    show(ID_SURFACE, ID_BUFFER, 320, 200);
    say("clip waiting for the keyboard");
    wait_for(&keyboard_in, 60000);
    check(keyboard_in, "clip the window gets the keyboard");
    /* The selection comes right before wl_keyboard.enter. */
    wait_for(&sel_text, 3000);
    check(sel_text && sel_utf8, "native clipboard is offered as text/plain;charset=utf-8 and UTF8_STRING");
    char buf[4096];
    int got = clip_receive("text/plain;charset=utf-8", buf, sizeof buf);
    printf("waylandcheck: clip received '%s'\n", got >= 0 ? buf : "");
    check(got >= 0 && !strcmp(buf, expect), "native clipboard text reaches the client");
    got = clip_receive("UTF8_STRING", buf, sizeof buf);
    check(got >= 0 && !strcmp(buf, expect), "native clipboard text reaches the client as UTF8_STRING");

    /* Our selection onto the system clipboard. */
    begin(ID_DDM, 0); /* create_data_source */
    put(ID_SRC);
    end();
    const char *mimes[] = {"text/plain;charset=utf-8", "UTF8_STRING", "text/uri-list"};
    for (int i = 0; i < 3; i++) {
        begin(ID_SRC, 0); /* offer */
        put_str(mimes[i]);
        end();
    }
    begin(ID_DDEV, 1); /* set_selection(source, serial) */
    put(ID_SRC);
    put((uint32_t)kb_serial);
    end();
    int two = 0;
    long until = now_ms() + 5000;
    while (src_sends < 2 && now_ms() < until) {
        begin(ID_DISPLAY, 0);
        put(ID_SYNC7);
        end();
        synced7 = 0;
        wait_for(&synced7, 500);
    }
    two = src_sends >= 2;
    check(two, "the server reads the client's selection (text and text/uri-list) right when it is set");
    say("clip selection set");
    /* Give the keyboard back (the scenario types `clip` into the shell):
     * the window goes away, the connection stays; the files come through
     * data control, which needs no window and no focus. */
    begin(ID_TOPLEVEL, 0); /* destroy */
    end();
    begin(ID_XDG, 0); /* destroy */
    end();
    begin(ID_SURFACE, 0); /* destroy */
    end();
    say("clip window closed");

    /* Files from native programs come as text/uri-list. */
    /* Only the system clipboard offers x-special/gnome-copied-files - our
     * own selection (echoed back to us) does not. The scenario takes the
     * keyboard away to type `clip files` into the shell, so the files come
     * through the data-control device (no focus needed). */
    wait_for(&dc_files, 120000);
    check(dc_files, "native files are offered as text/uri-list");
    got = clip_receive_from(dc_offer, 0, "text/uri-list", buf, sizeof buf);
    printf("waylandcheck: clip uri-list '%s'\n", got >= 0 ? buf : "");
    check(got > 0 && strstr(buf, "file:///etc/hostname\r\n") != NULL, "native files reach the client as file:// URIs");
    check(src_cancelled, "the client's source is cancelled when native programs copy");
    check(!protocol_error, "no protocol errors");
    close(sock);
    printf("waylandcheck: %d failed\n", failed);
    return failed;
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
    if (argc > 1 && !strcmp(argv[1], "clip")) {
        check(connected, "connect to the compositor");
        return connected ? clip(argc > 2 ? argv[2] : "") : 1;
    }
    if (argc > 1 && !strcmp(argv[1], "g5")) {
        check(connected, "connect to the compositor");
        return connected ? g5() : 1;
    }
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

    /* 26.11 (Uot: no cursor over the fullscreen game): a program's own
     * cursor image - a 24x24 magenta square with a white border, hotspot
     * in the middle - over the window (Wine: SetCursor with a color
     * cursor). The scenario takes a screenshot of it. */
    begin(ID_COMPOSITOR, 0); /* create_surface */
    put(ID_CSURF);
    end();
    shm_buffer(ID_CPOOL, ID_CBUF, 24, 24, 0xffff00ffu);
    show(ID_CSURF, ID_CBUF, 24, 24);
    begin(ID_POINTER, 0); /* set_cursor(serial, surface, hx, hy) */
    put(0);
    put(ID_CSURF);
    put(12);
    put(12);
    end();
    begin(ID_DISPLAY, 0);
    put(ID_SYNC8);
    end();
    wait_for(&synced8, 5000);
    printf("waylandcheck: own cursor shown\n");
    fflush(stdout);
    wait_for(&never, 3000);
    check(synced8 && !protocol_error, "wl_pointer.set_cursor with a surface is taken");

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
