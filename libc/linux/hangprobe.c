/* Программа Linux x86-64 без libc (18.11.2026): «игра, которая не
 * закрывается» — P4 плана Proton.
 *
 * Открывает окно Wayland на голом протоколе (как `waylandcheck.c`, но без
 * libc: `socket`, `sendmsg` с SCM_RIGHTS, `memfd`), а потом перестаёт
 * разбирать очередь совсем: ни `pong` на `xdg_wm_base.ping`, ни ответа на
 * `xdg_toplevel.close`. Главный поток спит в `futex_waitv` без срока (так
 * спит поток fsync Wine), второй поток — в `FUTEX_WAIT` без срока, а
 * дочерний процесс того же запуска («wineserver») — тоже в `futex_waitv`.
 *
 * Стенд нажимает Alt+F4, ждёт «не отвечает», выбирает «Завершить
 * программу» — и ядро обязано снять оба процесса со всеми потоками.
 *
 * `hangprobe: ok <что>` / `hangprobe: FAILED <что>`.
 */

typedef unsigned long u64;
typedef unsigned int u32;
typedef unsigned short u16;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0, SYS_write = 1, SYS_mmap = 9, SYS_nanosleep = 35, SYS_getpid = 39, SYS_socket = 41,
    SYS_connect = 42, SYS_sendmsg = 46, SYS_fork = 57, SYS_exit = 60, SYS_ftruncate = 77, SYS_futex = 202,
    SYS_exit_group = 231, SYS_memfd_create = 319, SYS_futex_waitv = 449,
};
enum { PROT_RW = 3, MAP_SHARED = 1 };
enum { FUTEX_WAIT_PRIVATE = 128, FUTEX_32 = 2, FUTEX_PRIVATE = 128 };

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
static void out(const char *s) { sys3(SYS_write, 1, s, strlen_(s)); }
static void check(int good, const char *what) {
    out(good ? "hangprobe: ok " : "hangprobe: FAILED ");
    out(what);
    out("\n");
    if (!good) sys3(SYS_exit_group, 1, 0, 0);
}

/* Спать без срока: `futex_waitv` по слову, которое никто не меняет. */
static volatile u32 never;
/* Поля — присваиваниями: инициализатор с адресом компилятор кладёт в
 * константу с перемещением, а перемещения статической PIE здесь никто не
 * применяет (адрес выходил нулём). */
static void sleep_forever_waitv(void) {
    volatile struct { u64 val, uaddr; u32 flags, reserved; } w;
    w.val = 0;
    w.uaddr = (u64)&never;
    w.flags = FUTEX_32 | FUTEX_PRIVATE;
    w.reserved = 0;
    for (;;) sys6(SYS_futex_waitv, (i64)&w, 1, 0, 0, 1, 0);
}

/* ---- провод Wayland ---- */
static int sock;
static u32 msg[64];
static int msg_len;
static void begin(u32 id, u32 opcode) {
    msg[0] = id;
    msg[1] = opcode;
    msg_len = 2;
}
static void put(u32 v) { msg[msg_len++] = v; }
static void put_str(const char *s) {
    u32 len = (u32)strlen_(s) + 1;
    put(len);
    char *at = (char *)&msg[msg_len];
    memset(at, 0, (len + 3) & ~3u);
    memcpy(at, s, len);
    msg_len += (int)((len + 3) / 4);
}
static void send_msg(int fd) {
    msg[1] |= (u32)(msg_len * 4) << 16;
    struct { void *base; u64 len; } iov = {msg, (u64)msg_len * 4};
    struct { u64 len; int level, type; int fd; int pad; } cmsg = {20, 1, 1, fd, 0};
    struct {
        void *name; u32 namelen; u32 pad0; void *iov; u64 iovlen; void *control; u64 controllen; int flags; int pad1;
    } hdr = {0, 0, 0, &iov, 1, fd >= 0 ? &cmsg : 0, fd >= 0 ? 24 : 0, 0, 0};
    sys3(SYS_sendmsg, sock, &hdr, 0);
}
static void end(void) { send_msg(-1); }

/* Читать события, пока не придёт `xdg_surface.configure` (номер — в
 * `*serial`) или `wl_callback.done` объекта `callback`. */
static unsigned char in[8192];
static int in_len;
static int pump(u32 xdg, u32 *serial, u32 callback) {
    for (int round = 0; round < 200; round++) {
        i64 got = sys3(SYS_read, sock, in + in_len, sizeof in - (u64)in_len);
        if (got <= 0) return 0;
        in_len += (int)got;
        int at = 0;
        while (in_len - at >= 8) {
            u32 id = *(u32 *)(in + at);
            u32 word = *(u32 *)(in + at + 4);
            int size = (int)(word >> 16);
            u32 opcode = word & 0xffff;
            if (size < 8 || in_len - at < size) break;
            if (serial && id == xdg && opcode == 0) {
                *serial = *(u32 *)(in + at + 8);
                in_len = 0;
                return 1;
            }
            if (callback && id == callback && opcode == 0) {
                in_len = 0;
                return 1;
            }
            at += size;
        }
        memcpy(in, in + at, (u64)(in_len - at));
        in_len -= at;
    }
    return 0;
}

long raw_thread(u64 flags, void *stack_top, void (*fn)(void));
__asm__(".text\n"
        ".globl raw_thread\n"
        "raw_thread:\n"
        "  mov %rdx, %r9\n"
        "  xor %edx, %edx\n"
        "  xor %r10d, %r10d\n"
        "  xor %r8d, %r8d\n"
        "  mov $56, %eax\n"
        "  syscall\n"
        "  test %rax, %rax\n"
        "  jnz 1f\n"
        "  and $-16, %rsp\n"
        "  call *%r9\n"
        "  mov $60, %eax\n"
        "  xor %edi, %edi\n"
        "  syscall\n"
        "  hlt\n"
        "1:\n"
        "  ret\n");
static char stack[32 * 1024] __attribute__((aligned(16)));
void sleeper_main(void) {
    for (;;) sys6(SYS_futex, (i64)&never, FUTEX_WAIT_PRIVATE, 0, 0, 0, 0);
}

void start_c(u64 *sp) {
    (void)sp;
    out("hangprobe: start\n");

    /* Помощник того же запуска — как wineserver у игры. */
    i64 child = sys3(SYS_fork, 0, 0, 0);
    if (child == 0) {
        out("hangprobe: helper sleeps in futex_waitv\n");
        sleep_forever_waitv();
    }
    check(child > 0, "fork starts the helper process");

    struct { u16 family; char path[108]; } addr;
    memset(&addr, 0, sizeof addr);
    addr.family = 1;
    const char *path = "/run/user/0/wayland-0";
    memcpy(addr.path, path, strlen_(path));
    sock = (int)sys3(SYS_socket, 1, 1, 0);
    check(sock >= 0 && sys3(SYS_connect, sock, &addr, 2 + strlen_(path) + 1) == 0, "connect to the compositor");

    begin(1, 1); /* get_registry */
    put(2);
    end();
    begin(2, 0); /* bind wl_compositor */
    put(1);
    put_str("wl_compositor");
    put(4);
    put(4);
    end();
    begin(2, 0); /* bind wl_shm */
    put(3);
    put_str("wl_shm");
    put(1);
    put(5);
    end();
    begin(2, 0); /* bind xdg_wm_base */
    put(4);
    put_str("xdg_wm_base");
    put(2);
    put(6);
    end();
    begin(4, 0); /* create_surface */
    put(7);
    end();
    begin(6, 2); /* get_xdg_surface */
    put(8);
    put(7);
    end();
    begin(8, 1); /* get_toplevel */
    put(9);
    end();
    begin(9, 2); /* set_title */
    put_str("hangprobe");
    end();
    begin(7, 6); /* commit */
    end();
    u32 serial = 0;
    check(pump(8, &serial, 0), "xdg_surface gets configure");
    begin(8, 4); /* ack_configure */
    put(serial);
    end();

    const u32 width = 240, height = 160, stride = width * 4;
    int fd = (int)sys3(SYS_memfd_create, "hangprobe", 0, 0);
    check(fd >= 0 && sys3(SYS_ftruncate, fd, stride * height, 0) == 0, "memfd for the buffer");
    u32 *pixels = (u32 *)sys6(SYS_mmap, 0, stride * height, PROT_RW, MAP_SHARED, fd, 0);
    for (u32 i = 0; i < width * height; i++) pixels[i] = 0xff803020u + (i % width);
    begin(5, 0); /* create_pool */
    put(10);
    put(stride * height);
    send_msg(fd);
    begin(10, 0); /* create_buffer XRGB8888 */
    put(11);
    put(0);
    put(width);
    put(height);
    put(stride);
    put(1);
    end();
    begin(7, 1); /* attach */
    put(11);
    put(0);
    put(0);
    end();
    begin(7, 2); /* damage */
    put(0);
    put(0);
    put(width);
    put(height);
    end();
    begin(7, 6); /* commit */
    end();
    begin(1, 0); /* sync */
    put(12);
    end();
    check(pump(0, 0, 12), "the window is shown");

    check(raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, stack + sizeof stack, sleeper_main) > 0,
          "a second thread sleeps in FUTEX_WAIT");
    out("hangprobe: ignoring the compositor now\n");
    sleep_forever_waitv();
}

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
