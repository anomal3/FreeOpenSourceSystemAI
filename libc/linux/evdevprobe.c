/* Программа Linux x86-64 без libc (24.11.2026): геймпад так, как его видит
 * `winebus.sys` GE-Proton11 без udevd — `/dev/input/event0`, ioctl evdev,
 * sysfs для libudev (`realpath`, `uevent`, ссылки `subsystem`, проход
 * `openat(O_PATH|O_NOFOLLOW)` как у `chase_symlinks` systemd), события
 * `struct input_event` через `poll`/`read`, `inotify` на `/dev/input`.
 *
 * Перед запуском ядру нужен пробный геймпад: `gamepad fake`. Программа
 * печатает `evdevprobe: waiting for input` и ждёт нажатия A (`gamepad fake
 * press a`), затем `evdevprobe: waiting for unplug` и ждёт `IN_DELETE`
 * (`gamepad fake off`). Каждая проверка — `evdevprobe: ok …` или
 * `evdevprobe: FAILED …`, итог — `evdevprobe: N failed`.
 */

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef unsigned char u8;
typedef long i64;
typedef int i32;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0, SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_stat = 4, SYS_fstat = 5, SYS_poll = 7,
    SYS_ioctl = 16, SYS_readlink = 89, SYS_getdents64 = 217, SYS_exit_group = 231, SYS_inotify_add_watch = 254,
    SYS_openat = 257, SYS_faccessat = 269, SYS_inotify_init1 = 294, SYS_fstatfs = 138,
};
enum { ENOENT = 2, EAGAIN = 11, ENODEV = 19, EINVAL = 22 };
enum { O_RDONLY = 0, O_RDWR = 2, O_NONBLOCK = 04000, O_DIRECTORY = 0200000, O_NOFOLLOW = 0400000, O_PATH = 010000000 };
enum { POLLIN = 1, IN_CREATE = 0x100, IN_DELETE = 0x200 };
enum { S_IFMT = 0170000, S_IFDIR = 0040000, S_IFCHR = 0020000, S_IFLNK = 0120000 };

static i64 sys6(i64 n, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
    register i64 r10 __asm__("r10") = d;
    register i64 r8 __asm__("r8") = e;
    register i64 r9 __asm__("r9") = f;
    i64 ret;
    __asm__ volatile("syscall"
                     : "=a"(ret)
                     : "a"(n), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                     : "rcx", "r11", "memory");
    return ret;
}
#define sys3(n, a, b, c) sys6(n, (i64)(a), (i64)(b), (i64)(c), 0, 0, 0)
#define sys4(n, a, b, c, d) sys6(n, (i64)(a), (i64)(b), (i64)(c), (i64)(d), 0, 0)

void *memset(void *d, int c, u64 n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}
void *memcpy(void *d, const void *s, u64 n) {
    unsigned char *p = d;
    const unsigned char *q = s;
    while (n--) *p++ = *q++;
    return d;
}
static u64 strlen_(const char *s) {
    u64 n = 0;
    while (s[n]) n++;
    return n;
}
static int streq(const char *a, const char *b) {
    while (*a && *a == *b) a++, b++;
    return *a == *b;
}
static int contains(const char *hay, u64 len, const char *needle) {
    u64 n = strlen_(needle);
    for (u64 i = 0; i + n <= len; i++) {
        u64 j = 0;
        while (j < n && hay[i + j] == needle[j]) j++;
        if (j == n) return 1;
    }
    return 0;
}

static char line[256];
static u64 line_len;
static void put_s(const char *s) {
    while (*s && line_len < sizeof line - 2) line[line_len++] = *s++;
}
static void put_i(i64 v) {
    char tmp[24];
    int n = 0;
    if (v < 0) put_s("-"), v = -v;
    do tmp[n++] = (char)('0' + v % 10), v /= 10;
    while (v);
    while (n) {
        if (line_len < sizeof line - 2) line[line_len++] = tmp[--n];
        else n--;
    }
}
static void put_hex(u64 v) {
    const char *digits = "0123456789abcdef";
    char tmp[17];
    int n = 0;
    do tmp[n++] = digits[v & 15], v >>= 4;
    while (v);
    while (n) {
        if (line_len < sizeof line - 2) line[line_len++] = tmp[--n];
        else n--;
    }
}
static void flush(void) {
    line[line_len++] = '\n';
    sys3(SYS_write, 1, line, line_len);
    line_len = 0;
}

static int failed;
static void check(int good, const char *what) {
    put_s(good ? "evdevprobe: ok " : "evdevprobe: FAILED ");
    put_s(what);
    flush();
    if (!good) failed++;
}

static char file_buf[4096];
static i64 read_file(const char *path) {
    i64 fd = sys3(SYS_open, path, O_RDONLY, 0);
    if (fd < 0) return fd;
    i64 total = 0;
    for (;;) {
        i64 got = sys3(SYS_read, fd, file_buf + total, sizeof file_buf - 1 - total);
        if (got <= 0) break;
        total += got;
    }
    sys3(SYS_close, fd, 0, 0);
    file_buf[total] = 0;
    return total;
}

static char link_buf[256];
static i64 read_link(const char *path) {
    i64 n = sys3(SYS_readlink, path, link_buf, sizeof link_buf - 1);
    link_buf[n > 0 ? n : 0] = 0;
    return n;
}

struct stat_ { u64 dev, ino, nlink; u32 mode, uid, gid, pad; u64 rdev; i64 size, blksize, blocks; i64 times[6]; i64 reserved[3]; };
struct input_id { u16 bustype, vendor, product, version; };
struct absinfo { i32 value, minimum, maximum, fuzz, flat, resolution; };
struct input_event { i64 sec, usec; u16 type, code; i32 value; };
struct pollfd { i32 fd; short events, revents; };

#define IOC(dir, nr, size) (((u64)(dir) << 30) | ((u64)(size) << 16) | ((u64)'E' << 8) | (u64)(nr))
#define EVIOCGID IOC(2, 0x02, 8)
#define EVIOCGNAME(len) IOC(2, 0x06, len)
#define EVIOCGBIT(ev, len) IOC(2, 0x20 + (ev), len)
#define EVIOCGABS(abs) IOC(2, 0x40 + (abs), 24)
#define EVIOCGUNIQ(len) IOC(2, 0x08, len)

static int bit(const u8 *map, int n) { return (map[n / 8] >> (n % 8)) & 1; }

/* Ждать события из `fd`; `-1` — срок вышел. */
static int wait_in(int fd, int ms) {
    struct pollfd p = {fd, POLLIN, 0};
    i64 got = sys3(SYS_poll, &p, 1, ms);
    return got > 0 && (p.revents & POLLIN) ? 0 : -1;
}

void start_c(u64 *sp) {
    (void)sp;
    put_s("evdevprobe: start");
    flush();

    /* inotify первым: как `create_inotify` winebus. */
    i64 in = sys3(SYS_inotify_init1, O_NONBLOCK, 0, 0);
    check(in >= 0 && sys3(SYS_inotify_add_watch, in, "/dev/input", IN_CREATE | IN_DELETE) > 0, "inotify watches /dev/input");

    /* /dev/input: каталог, в нём event0. */
    struct stat_ st;
    check(sys3(SYS_stat, "/dev/input", &st, 0) == 0 && (st.mode & S_IFMT) == S_IFDIR, "/dev/input is a directory");
    i64 dir = sys3(SYS_open, "/dev/input", O_RDONLY | O_DIRECTORY, 0);
    i64 got = dir >= 0 ? sys3(SYS_getdents64, dir, file_buf, sizeof file_buf) : -1;
    int listed = 0;
    for (i64 at = 0; got > 0 && at < got;) {
        u16 reclen = *(u16 *)(file_buf + at + 16);
        if (streq(file_buf + at + 19, "event0")) listed = 1;
        at += reclen ? reclen : got;
    }
    if (dir >= 0) sys3(SYS_close, dir, 0, 0);
    check(listed, "/dev/input lists event0");
    check(sys3(SYS_stat, "/dev/input/event0", &st, 0) == 0 && (st.mode & S_IFMT) == S_IFCHR && st.rdev == ((13 << 8) | 64),
          "/dev/input/event0 is char device 13:64");

    i64 fd = sys3(SYS_open, "/dev/input/event0", O_RDWR | O_NONBLOCK, 0);
    check(fd >= 0, "open /dev/input/event0 O_RDWR");
    if (fd < 0) goto done;

    struct input_id id;
    memset(&id, 0, sizeof id);
    check(sys3(SYS_ioctl, fd, EVIOCGID, &id) == 0, "EVIOCGID answers 0");
    put_s("evdevprobe: id ");
    put_hex(id.bustype), put_s("/"), put_hex(id.vendor), put_s("/"), put_hex(id.product), put_s("/"), put_hex(id.version);
    flush();
    check(id.bustype == 3 && id.vendor == 0x28de && id.product == 0x11ff, "EVIOCGID is the Steam virtual pad 3/28de/11ff");
    char name[128];
    memset(name, 0, sizeof name);
    i64 n = sys3(SYS_ioctl, fd, EVIOCGNAME(sizeof name - 1), name);
    put_s("evdevprobe: name '"), put_s(name), put_s("'");
    flush();
    check(n > 0 && streq(name, "Microsoft X-Box 360 pad 0"), "EVIOCGNAME is 'Microsoft X-Box 360 pad 0'");
    check(sys3(SYS_ioctl, fd, EVIOCGUNIQ(64), name) < 0, "EVIOCGUNIQ fails like a virtual device");
    u8 evbits[4] = {0}, keys[96] = {0}, abs[8] = {0};
    sys3(SYS_ioctl, fd, EVIOCGBIT(0, sizeof evbits), evbits);
    sys3(SYS_ioctl, fd, EVIOCGBIT(1, sizeof keys), keys);
    sys3(SYS_ioctl, fd, EVIOCGBIT(3, sizeof abs), abs);
    check(bit(evbits, 1) && bit(evbits, 3), "EVIOCGBIT(0) has EV_KEY and EV_ABS");
    check(bit(keys, 0x130) && bit(keys, 0x13c) && bit(keys, 0x13e) && !bit(keys, 0x110), "EVIOCGBIT(EV_KEY) has BTN_A, BTN_MODE, BTN_THUMBR, no BTN_LEFT");
    check(bit(abs, 0) && bit(abs, 5) && bit(abs, 0x10) && bit(abs, 0x11) && !bit(abs, 6), "EVIOCGBIT(EV_ABS) has X..RZ and HAT0");
    struct absinfo ai;
    memset(&ai, 0, sizeof ai);
    check(sys3(SYS_ioctl, fd, EVIOCGABS(0), &ai) == 0 && ai.minimum == -32768 && ai.maximum == 32767, "EVIOCGABS(ABS_X) is -32768..32767");
    check(sys3(SYS_ioctl, fd, EVIOCGABS(2), &ai) >= 0 && ai.minimum == 0 && ai.maximum == 255, "EVIOCGABS(ABS_Z) is 0..255");
    check(sys3(SYS_ioctl, fd, EVIOCGABS(6), &ai) == -EINVAL, "EVIOCGABS(ABS_THROTTLE) is EINVAL");

    /* sysfs как у libudev. */
    check(read_link("/sys/class/input/event0") > 0 && streq(link_buf, "../../devices/virtual/input/input0/event0"),
          "readlink /sys/class/input/event0");
    check(read_link("/sys") == -EINVAL && read_link("/sys/class/input") == -EINVAL && read_link("/sys/devices/virtual/input/input0/event0") == -EINVAL,
          "readlink of the directories is EINVAL (realpath)");
    check(sys3(SYS_stat, "/sys/class/input/event0", &st, 0) == 0 && (st.mode & S_IFMT) == S_IFDIR, "stat follows the class link to a directory");
    i64 len = read_file("/sys/devices/virtual/input/input0/event0/uevent");
    check(len > 0 && contains(file_buf, (u64)len, "DEVNAME=input/event0") && contains(file_buf, (u64)len, "MAJOR=13") &&
              contains(file_buf, (u64)len, "MINOR=64"),
          "event0/uevent names input/event0 13:64");
    len = read_file("/sys/class/input/event0/device/uevent");
    check(len > 0 && contains(file_buf, (u64)len, "PRODUCT=3/28de/11ff/1") && contains(file_buf, (u64)len, "ID_INPUT_JOYSTICK=1"),
          "input0/uevent has PRODUCT and ID_INPUT_JOYSTICK (through the links)");
    check(read_link("/sys/devices/virtual/input/input0/event0/subsystem") > 0 && contains(link_buf, strlen_(link_buf), "/class/input"),
          "event0/subsystem links to class/input");
    check(read_link("/sys/devices/virtual/input/input0/subsystem") > 0 && contains(link_buf, strlen_(link_buf), "/class/input"),
          "input0/subsystem links to class/input");
    /* chase_symlinks: каталог за каталогом от корня. */
    {
        const char *parts[] = {"sys", "devices", "virtual", "input", "input0", "event0"};
        i64 at = sys3(SYS_open, "/", O_PATH | O_DIRECTORY, 0);
        int good = at >= 0;
        for (int i = 0; good && i < 6; i++) {
            i64 child = sys4(SYS_openat, at, parts[i], O_PATH | O_NOFOLLOW, 0);
            good = child >= 0 && sys3(SYS_fstat, child, &st, 0) == 0 && (st.mode & S_IFMT) == S_IFDIR;
            sys3(SYS_close, at, 0, 0);
            at = child;
        }
        check(good, "openat O_PATH|O_NOFOLLOW walks /sys/devices/virtual/input/input0/event0");
        check(good && sys4(SYS_faccessat, at, "uevent", 0, 0) == 0, "faccessat(event0 dir, uevent)");
        /* `fd_is_fs_type(fd, SYSFS_MAGIC)`: libudev refuses directories off sysfs. */
        u64 fs[15];
        memset(fs, 0, sizeof fs);
        check(good && sys3(SYS_fstatfs, at, fs, 0) == 0 && fs[0] == 0x62656572, "fstatfs of the event0 dir is SYSFS_MAGIC");
        if (at >= 0) sys3(SYS_close, at, 0, 0);
        i64 up = sys3(SYS_open, "/sys/devices/virtual/input", O_PATH | O_DIRECTORY, 0);
        check(up >= 0 && sys4(SYS_faccessat, up, "uevent", 0, 0) == -ENOENT, "faccessat(virtual/input, uevent) is ENOENT (not a device)");
        if (up >= 0) sys3(SYS_close, up, 0, 0);
    }

    /* События. */
    struct input_event ev;
    check(sys3(SYS_read, fd, &ev, sizeof ev) == -EAGAIN, "read with nothing pending is EAGAIN");
    put_s("evdevprobe: waiting for input");
    flush();
    int pressed = 0, synced = 0;
    for (int tries = 0; tries < 64 && !(pressed && synced); tries++) {
        if (wait_in((int)fd, 60000) < 0) break;
        while (sys3(SYS_read, fd, &ev, sizeof ev) == (i64)sizeof ev) {
            put_s("evdevprobe: event type "), put_i(ev.type), put_s(" code "), put_hex(ev.code), put_s(" value "), put_i(ev.value);
            flush();
            if (ev.type == 1 && ev.code == 0x130 && ev.value == 1) pressed = 1;
            if (pressed && ev.type == 0 && ev.code == 0) synced = 1;
        }
    }
    check(pressed && synced, "EV_KEY BTN_A 1 then SYN_REPORT");

    put_s("evdevprobe: waiting for unplug");
    flush();
    int deleted = 0;
    for (int tries = 0; tries < 16 && !deleted; tries++) {
        if (wait_in((int)in, 60000) < 0) break;
        got = sys3(SYS_read, in, file_buf, sizeof file_buf);
        for (i64 at = 0; got > 0 && at < got;) {
            u32 mask = *(u32 *)(file_buf + at + 4);
            u32 nlen = *(u32 *)(file_buf + at + 12);
            if ((mask & IN_DELETE) && nlen && streq(file_buf + at + 16, "event0")) deleted = 1;
            at += 16 + nlen;
        }
    }
    check(deleted, "IN_DELETE event0 on unplug");
    check(sys3(SYS_read, fd, &ev, sizeof ev) == -ENODEV, "read after unplug is ENODEV");
    sys3(SYS_close, fd, 0, 0);
    check(sys3(SYS_open, "/dev/input/event0", O_RDWR, 0) == -ENOENT, "event0 is gone");

done:
    put_s("evdevprobe: ");
    put_i(failed);
    put_s(" failed");
    flush();
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
