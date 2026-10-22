/* Программа Linux x86-64 без libc (фаза 71a): проверка слоя совместимости.
 *
 * Собирается clang под `x86_64-unknown-linux-gnu` статической PIE, без единой
 * строчки FossAi: только номера системных вызовов Linux, инструкция `syscall`
 * и стек, разложенный по правилам `execve`. Метку ABI GNU (`.note.ABI-tag`)
 * кладёт сама программа — по ней ядро и узнаёт программу Linux, как узнаёт
 * их FreeBSD.
 *
 * Перемещений в программе нет нарочно: у статической PIE без libc их некому
 * применить, поэтому ни таблиц указателей, ни глобальных адресов — только
 * строки, до которых код дотягивается относительно себя.
 *
 * Каждая проверка печатает `linux: ok <что>` или `linux: FAILED <что>`; итог —
 * `linux: N failed`, и код выхода — число проваленных.
 */

typedef unsigned long u64;
typedef long i64;

/* Метка ABI GNU: имя "GNU", тип 1, ОС 0 (Linux), версия 3.2.0. */
__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_read = 0, SYS_write = 1, SYS_close = 3, SYS_fstat = 5, SYS_mmap = 9, SYS_munmap = 11,
    SYS_brk = 12, SYS_nanosleep = 35, SYS_getpid = 39, SYS_uname = 63, SYS_arch_prctl = 158,
    SYS_clock_gettime = 228, SYS_exit_group = 231, SYS_openat = 257, SYS_getrandom = 318,
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

/* Компилятор вправе заменить цикл вызовом `memset`/`memcpy` даже без libc. */
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
static int prefix(const char *s, const char *p) {
    while (*p) {
        if (*s++ != *p++) return 0;
    }
    return 1;
}
static void out(const char *s) { sys3(SYS_write, 1, s, strlen_(s)); }
static void out_num(u64 v) {
    char buf[24];
    int i = 23;
    buf[i] = 0;
    do {
        buf[--i] = (char)('0' + v % 10);
        v /= 10;
    } while (v);
    out(buf + i);
}

static int failed;
/* Строка проверки уходит одним `write`: по кускам её разрывали строки других
 * задач (`init: ...`), и стенд не находил её целиком. */
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "linux: ok " : "linux: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

struct timespec_ { i64 sec, nsec; };
static i64 ns(void) {
    struct timespec_ t;
    sys3(SYS_clock_gettime, 1, &t, 0);
    return t.sec * 1000000000L + t.nsec;
}

static char tls_block[64];

void start_c(u64 *sp) {
    u64 argc = sp[0];
    char **argv = (char **)(sp + 1);
    char **envp = argv + argc + 1;
    u64 *auxv = (u64 *)envp;
    while (*auxv) auxv++;
    auxv++;

    out("linux: hello from a Linux program, argc ");
    out_num(argc);
    out("\n");
    check(argc >= 1 && argv[argc] == 0, "argv ends with a null pointer");

    int path = 0;
    for (char **e = envp; *e; e++) path |= prefix(*e, "PATH=");
    check(path, "the environment has PATH");

    u64 pagesz = 0, random = 0, entry = 0, phdr = 0;
    for (u64 *a = auxv; a[0]; a += 2) {
        if (a[0] == 6) pagesz = a[1];
        if (a[0] == 25) random = a[1];
        if (a[0] == 9) entry = a[1];
        if (a[0] == 3) phdr = a[1];
    }
    check(pagesz == 4096 && random != 0 && entry != 0 && phdr != 0, "auxv: page size, AT_RANDOM, entry, phdr");
    check(phdr && (((unsigned *)phdr)[0] == 6 || ((unsigned *)phdr)[0] == 1), "AT_PHDR points at the program headers");

    char uts[65 * 6];
    i64 r = sys3(SYS_uname, uts, 0, 0);
    check(r == 0 && prefix(uts, "Linux"), "uname says Linux");
    out("linux: release ");
    out(uts + 65 * 2);
    out("\n");

    check(sys3(SYS_getpid, 0, 0, 0) > 0, "getpid");

    u64 base = (u64)sys3(SYS_brk, 0, 0, 0);
    u64 grown = (u64)sys3(SYS_brk, base + (1 << 20), 0, 0);
    int brk_ok = base != 0 && grown == base + (1 << 20);
    if (brk_ok) {
        ((volatile char *)base)[0] = 1;
        ((volatile char *)base)[(1 << 20) - 1] = 2;
    }
    check(brk_ok, "brk grows by a megabyte");

    char *m = (char *)sys6(SYS_mmap, 0, 1 << 20, 3, 0x22, -1, 0);
    int mmap_ok = (i64)m > 0;
    if (mmap_ok) {
        m[0] = 7;
        m[(1 << 20) - 1] = 9;
        mmap_ok = m[0] == 7 && m[4096] == 0 && sys3(SYS_munmap, m, 1 << 20, 0) == 0;
    }
    check(mmap_ok, "anonymous mmap and munmap");

    tls_block[8] = 42;
    r = sys3(SYS_arch_prctl, 0x1002, tls_block, 0);
    unsigned char via_fs = 0;
    __asm__ volatile("movb %%fs:8, %0" : "=r"(via_fs));
    u64 got = 0;
    sys3(SYS_arch_prctl, 0x1003, &got, 0);
    check(r == 0 && via_fs == 42 && got == (u64)tls_block, "arch_prctl sets FS for TLS");

    i64 before = ns();
    struct timespec_ nap = {0, 20000000};
    sys3(SYS_nanosleep, &nap, 0, 0);
    i64 slept = ns() - before;
    check(slept >= 20000000, "nanosleep 20 ms by the monotonic clock");

    unsigned char rnd[16] = {0};
    r = sys3(SYS_getrandom, rnd, 16, 0);
    int nonzero = 0;
    for (int i = 0; i < 16; i++) nonzero |= rnd[i];
    check(r == 16 && nonzero, "getrandom");

    /* Свой же файл — по `AT_EXECFN`: он заведомо есть, и начало у него
     * известно — заголовок ELF. */
    const char *self = 0;
    for (u64 *a = auxv; a[0]; a += 2) {
        if (a[0] == 31) self = (const char *)a[1];
    }
    i64 fd = self ? sys6(SYS_openat, -100, (i64)self, 0, 0, 0, 0) : -1;
    char buf[64];
    i64 n = fd >= 0 ? sys3(SYS_read, fd, buf, sizeof buf) : -1;
    u64 st[18];
    i64 s = fd >= 0 ? sys3(SYS_fstat, fd, st, 0) : -1;
    check(fd >= 0 && n == 64 && prefix(buf + 1, "ELF") && s == 0 && (st[3] & 0170000) == 0100000 && st[6] > 64,
          "openat, read and fstat of the program itself");
    if (fd >= 0) sys3(SYS_close, fd, 0, 0);
    check(sys6(SYS_openat, -100, (i64) "/no/such/file", 0, 0, 0, 0) == -2, "a missing file is ENOENT");

    out("linux: ");
    out_num((u64)failed);
    out(" failed\n");
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) {
    }
}

/* Вход: `rsp` указывает на `argc`. Выравнивание перед вызовом — то, что ждёт
 * System V: `rsp` кратен 16 на `call`. */
__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
