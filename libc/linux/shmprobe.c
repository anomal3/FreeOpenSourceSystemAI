/* Программа Linux x86-64 без libc (12.11.2026): память сессии wineserver так,
 * как её видит Wine. Сервер: memfd (`MFD_EXEC`), `ftruncate`, `MAP_SHARED`
 * на запись, рост файла (`pwrite` байта в конец + `ftruncate`) и отображение
 * хвоста со смещением. Клиент: дескриптор через `SCM_RIGHTS`, отображение
 * `MAP_PRIVATE` с чтением и записью (как `map_file_into_view` Wine) — и он
 * обязан видеть записи сервера, сделанные и до, и после отображения.
 *
 * Строки — `shmprobe: ok <что>` / `shmprobe: FAILED <что>`, итог —
 * `shmprobe: N failed`.
 */

typedef unsigned long u64;
typedef unsigned int u32;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_write = 1, SYS_mmap = 9, SYS_pwrite64 = 18, SYS_socketpair = 53, SYS_sendmsg = 46, SYS_recvmsg = 47,
    SYS_ftruncate = 77, SYS_exit_group = 231, SYS_memfd_create = 319,
};

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

static int failed;
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "shmprobe: ok " : "shmprobe: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

enum { PROT_RW = 3, MAP_SHARED = 1, MAP_PRIVATE = 2, BLOCK = 0x10000 };

static volatile unsigned char *map(i64 len, i64 flags, i64 fd, i64 off) {
    i64 r = sys6(SYS_mmap, 0, len, PROT_RW, flags, fd, off);
    return r < 0 ? 0 : (volatile unsigned char *)r;
}

/* Передать `fd` через пару сокетов (SCM_RIGHTS) и вернуть принятый. */
static i64 pass_fd(i64 fd) {
    int pair[2];
    if (sys6(SYS_socketpair, 1, 1, 0, (i64)pair, 0, 0) != 0) return -1;
    char byte = 'x';
    struct { u64 base, len; } iov = {(u64)&byte, 1};
    struct { u64 len; int level, type; int fd; int pad; } cmsg = {20, 1, 1, (int)fd, 0};
    struct { u64 name; u32 namelen, pad0; u64 iov, iovlen, control, controllen; int flags, pad1; } msg = {
        0, 0, 0, (u64)&iov, 1, (u64)&cmsg, 24, 0, 0};
    if (sys3(SYS_sendmsg, pair[0], &msg, 0) != 1) return -2;
    struct { u64 len; int level, type; int fd; int pad; } got = {0, 0, 0, -1, 0};
    char in = 0;
    struct { u64 base, len; } iov2 = {(u64)&in, 1};
    struct { u64 name; u32 namelen, pad0; u64 iov, iovlen, control, controllen; int flags, pad1; } msg2 = {
        0, 0, 0, (u64)&iov2, 1, (u64)&got, 24, 0, 0};
    if (sys3(SYS_recvmsg, pair[1], &msg2, 0) != 1) return -3;
    return got.fd;
}

void start_c(u64 *sp) {
    (void)sp;
    out("shmprobe: start\n");
    const char *name;
    __asm__("lea probe_name(%%rip), %0" : "=r"(name));
    i64 fd = sys3(SYS_memfd_create, name, 0x10 /* MFD_EXEC */, 0);
    check(fd >= 0, "memfd_create with MFD_EXEC");
    check(sys3(SYS_ftruncate, fd, BLOCK, 0) == 0, "ftruncate to 64 KiB");
    volatile unsigned char *server = map(BLOCK, MAP_SHARED, fd, 0);
    check(server != 0, "server MAP_SHARED");
    if (!server) goto done;
    server[0] = 1;

    i64 client_fd = pass_fd(fd);
    check(client_fd >= 0, "fd passed through SCM_RIGHTS");
    if (client_fd < 0) client_fd = fd;
    volatile unsigned char *client = map(BLOCK, MAP_PRIVATE, client_fd, 0);
    check(client != 0, "client MAP_PRIVATE read-write");
    if (!client) goto done;
    check(client[0] == 1, "client sees a write made before it mapped");
    (void)client[200];
    server[200] = 7;
    check(client[200] == 7, "client sees a write made after it touched the page");
    server[40000] = 5;
    check(client[40000] == 5, "client sees a write on a page it never touched");

    /* Рост, как `grow_file` + `grow_session_mapping`. */
    char zero = 0;
    check(sys6(SYS_pwrite64, fd, (i64)&zero, 1, 3 * BLOCK - 1, 0, 0) == 1, "pwrite one byte at the new end");
    check(sys3(SYS_ftruncate, fd, 3 * BLOCK, 0) == 0, "ftruncate to 192 KiB");
    volatile unsigned char *tail = map(2 * BLOCK, MAP_SHARED, fd, BLOCK);
    check(tail != 0, "server maps the new tail at offset 64 KiB");
    if (!tail) goto done;
    tail[16] = 9;
    volatile unsigned char *whole = map(3 * BLOCK, MAP_PRIVATE, client_fd, 0);
    check(whole != 0, "client maps the grown section");
    if (!whole) goto done;
    check(whole[BLOCK + 16] == 9, "client sees a write in the grown tail");
    check(whole[200] == 7, "the grown client view sees the first block too");
    tail[100] = 11;
    check(whole[BLOCK + 100] == 11, "client sees a later write in the tail");

    /* Как Wine: сначала резерв адресов (безымянная PROT_NONE), потом файл
     * поверх него с MAP_FIXED. */
    i64 reserve = sys6(SYS_mmap, 0, 3 * BLOCK, 0, 0x22 /* MAP_PRIVATE|MAP_ANONYMOUS */, -1, 0);
    check(reserve > 0, "anonymous PROT_NONE reserve");
    if (reserve <= 0) goto done;
    i64 fixed = sys6(SYS_mmap, reserve, 3 * BLOCK, PROT_RW, MAP_PRIVATE | 0x10 /* MAP_FIXED */, client_fd, 0);
    check(fixed == reserve, "client MAP_FIXED|MAP_PRIVATE over the reserve");
    if (fixed != reserve) goto done;
    volatile unsigned char *view = (volatile unsigned char *)fixed;
    check(view[200] == 7 && view[BLOCK + 16] == 9, "the fixed view sees earlier writes");
    (void)view[300];
    server[300] = 13;
    tail[200] = 17;
    check(view[300] == 13 && view[BLOCK + 200] == 17, "the fixed view sees later writes");

done:
    out(failed ? "shmprobe: some failed\n" : "shmprobe: 0 failed\n");
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

__asm__(".section .rodata\n"
        "probe_name: .asciz \"wine-mapping\"\n"
        ".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
