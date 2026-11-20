/* Программа Linux x86-64 без libc (18.11.2026): память «на запись и
 * исполнение» поверх файла и разделяемой памяти — и копия файла ядром.
 *
 * P5 плана Proton: в журнале TrackMania 64 отказа `mprotect … prot 0x7 ->
 * WriteExecute` — Wine делает RWX вид секции PE (`VirtualProtect`,
 * `NtMapViewOfSection` с PAGE_EXECUTE_READWRITE). Проверяется то же самое:
 *   1. частное отображение файла R -> mprotect RWX -> запись кода -> вызов,
 *      файл при этом не меняется;
 *   2. `memfd` под MAP_SHARED (секция Wine без файла) -> mprotect RWX ->
 *      код -> вызов;
 *   3. mmap памяти memfd сразу RWX.
 * Нужно право `jit` (`lxjit on` в консоли ядра): у `/compat/linux/bin` нет
 * пакета, который назвал бы его.
 *
 * P10: `copy_file_range` (326) целиком и кусками со смещениями, флаги не
 * ноль — EINVAL, `sendfile` по-прежнему копирует.
 *
 * `rwxprobe: ok <что>` / `rwxprobe: FAILED <что>`; итог `rwxprobe: N failed`.
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
    SYS_read = 0, SYS_write = 1, SYS_open = 2, SYS_close = 3, SYS_mmap = 9, SYS_mprotect = 10,
    SYS_munmap = 11, SYS_pread64 = 17, SYS_pwrite64 = 18, SYS_sendfile = 40, SYS_ftruncate = 77,
    SYS_unlink = 87, SYS_exit_group = 231, SYS_memfd_create = 319, SYS_copy_file_range = 326,
};
enum { PROT_READ = 1, PROT_WRITE = 2, PROT_EXEC = 4 };
enum { MAP_SHARED = 1, MAP_PRIVATE = 2, MAP_ANON = 0x20 };
enum { O_RDWR = 2, O_CREAT = 0x40, O_TRUNC = 0x200 };
enum { EINVAL = 22 };

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
    const char *head = good ? "rwxprobe: ok " : "rwxprobe: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}
static void number(const char *label, i64 v) {
    char line[96];
    u64 len = 0;
    for (const char *s = label; *s && len < 60; s++) line[len++] = *s;
    if (v < 0) {
        line[len++] = '-';
        v = -v;
    }
    char digits[24];
    int n = 0;
    do {
        digits[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && n < 20);
    while (n) line[len++] = digits[--n];
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
}

/* `mov eax, imm32; ret` */
static void put_code(unsigned char *at, u32 value) {
    at[0] = 0xb8;
    at[1] = (unsigned char)value;
    at[2] = (unsigned char)(value >> 8);
    at[3] = (unsigned char)(value >> 16);
    at[4] = (unsigned char)(value >> 24);
    at[5] = 0xc3;
}
static u32 call(void *code) { return ((u32(*)(void))code)(); }
static int bad(i64 r) { return (u64)r > (u64)-4096; }

static unsigned char block[300000];
static unsigned char back[300000];

void start_c(u64 *sp) {
    (void)sp;
    out("rwxprobe: start\n");

    /* 0. Право `jit` есть? Безымянная RWX — то, что у программы с правом
     * разрешено всегда. */
    i64 anon = sys6(SYS_mmap, 0, 4096, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANON, -1, 0);
    if (bad(anon)) {
        check(0, "anonymous RWX mmap (no jit right: type `lxjit on` first)");
        out("rwxprobe: some failed\n");
        sys3(SYS_exit_group, 1, 0, 0);
    }
    put_code((unsigned char *)anon, 11);
    check(call((void *)anon) == 11, "anonymous RWX code runs");

    /* 1. Частное отображение файла — самой пробы (корень живой системы
     * только на чтение: создать свой файл негде). */
    const char *self = "/compat/linux/bin/rwxprobe";
    i64 fd = sys3(SYS_open, self, 0, 0);
    check(fd >= 0, "open the probe's own file read-only");
    i64 map = sys6(SYS_mmap, 0, 8192, PROT_READ, MAP_PRIVATE, fd, 0);
    check(!bad(map), "private read-only file mapping");
    if (!bad(map)) {
        check(((unsigned char *)map)[0] == 0x7f && ((unsigned char *)map)[1] == 'E', "the mapping shows the ELF header");
        i64 r = sys3(SYS_mprotect, map, 8192, PROT_READ | PROT_WRITE | PROT_EXEC);
        number("rwxprobe: mprotect private file RWX = ", r);
        check(r == 0, "mprotect RWX on a private file mapping");
        if (r == 0) {
            put_code((unsigned char *)map + 4096, 7);
            check(call((char *)map + 4096) == 7, "code written into the RWX file mapping runs");
            unsigned char first[2] = {0, 0};
            sys6(SYS_pread64, fd, (i64)first, 2, 0, 0, 0);
            check(first[0] == 0x7f && first[1] == 'E', "the file itself is unchanged (MAP_PRIVATE)");
        }
        sys3(SYS_munmap, map, 8192, 0);
    }

    /* 2. Разделяемая память: секция Wine без файла. */
    i64 mfd = sys3(SYS_memfd_create, "section", 0, 0);
    check(mfd >= 0, "memfd_create");
    if (mfd >= 0) {
        check(sys3(SYS_ftruncate, mfd, 8192, 0) == 0, "ftruncate memfd 8 KiB");
        i64 shared = sys6(SYS_mmap, 0, 8192, PROT_READ | PROT_WRITE, MAP_SHARED, mfd, 0);
        check(!bad(shared), "MAP_SHARED read-write view of the memfd");
        if (!bad(shared)) {
            put_code((unsigned char *)shared + 4096, 99);
            i64 r = sys3(SYS_mprotect, shared, 8192, PROT_READ | PROT_WRITE | PROT_EXEC);
            number("rwxprobe: mprotect shared memfd RWX = ", r);
            check(r == 0, "mprotect RWX on a MAP_SHARED memfd view");
            if (r == 0) {
                check(call((char *)shared + 4096) == 99, "code in the shared RWX view runs");
            }
            /* Второй вид той же памяти сразу RWX — видит записанное. */
            i64 second = sys6(SYS_mmap, 0, 8192, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_SHARED, mfd, 0);
            check(!bad(second), "mmap of the memfd directly RWX");
            if (!bad(second)) {
                put_code((unsigned char *)second, 123);
                check(call((void *)second) == 123, "code in the second RWX view runs");
                check(((unsigned char *)shared)[1] == 123, "both views share the page");
                sys3(SYS_munmap, second, 8192, 0);
            }
            sys3(SYS_munmap, shared, 8192, 0);
        }
        sys3(SYS_close, mfd, 0, 0);
    }

    /* 3. copy_file_range и sendfile. Проверки отказов — на своём файле;
     * копия целиком — если есть куда писать (/tmp). */
    check(sys6(SYS_copy_file_range, fd, 0, fd, 0, 10, 1) == -EINVAL, "copy_file_range with flags is EINVAL");
    i64 off_in = 0, off_out = 100;
    check(sys6(SYS_copy_file_range, fd, (i64)&off_in, fd, (i64)&off_out, 1000, 0) == -EINVAL,
          "an overlapping copy within one file is EINVAL");
    i64 src = sys3(SYS_open, "/tmp/rwxprobe.src", O_RDWR | O_CREAT | O_TRUNC, 0644);
    i64 dst = sys3(SYS_open, "/tmp/rwxprobe.dst", O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (src < 0 || dst < 0) {
        out("rwxprobe: no writable /tmp here, the whole-file copy is skipped\n");
    } else {
        for (u64 i = 0; i < sizeof block; i++) block[i] = (unsigned char)(i * 7 + i / 251);
        check(sys6(SYS_pwrite64, src, (i64)block, sizeof block, 0, 0, 0) == (i64)sizeof block, "write 300000 bytes");
        i64 total = 0;
        for (int i = 0; i < 100; i++) {
            i64 r = sys6(SYS_copy_file_range, src, 0, dst, 0, 1L << 30, 0);
            if (r <= 0) {
                if (r < 0) number("rwxprobe: copy_file_range = ", r);
                break;
            }
            total += r;
        }
        number("rwxprobe: copy_file_range copied ", total);
        check(total == (i64)sizeof block, "copy_file_range copies the whole file by file positions");
        memset(back, 0, sizeof back);
        sys6(SYS_pread64, dst, (i64)back, sizeof back, 0, 0, 0);
        int same = 1;
        for (u64 i = 0; i < sizeof block; i++) {
            if (back[i] != block[i]) {
                same = 0;
                break;
            }
        }
        check(same, "the copy matches byte for byte");
        off_in = 1000;
        off_out = 0;
        i64 r = sys6(SYS_copy_file_range, src, (i64)&off_in, dst, (i64)&off_out, 5000, 0);
        check(r == 5000 && off_in == 6000 && off_out == 5000, "copy_file_range with offsets advances both");
        i64 sent_from = 0;
        r = sys6(SYS_sendfile, dst, src, (i64)&sent_from, 4096, 0, 0);
        check(r == 4096 && sent_from == 4096, "sendfile still copies between files");
        sys3(SYS_close, src, 0, 0);
        sys3(SYS_close, dst, 0, 0);
        sys3(SYS_unlink, "/tmp/rwxprobe.src", 0, 0);
        sys3(SYS_unlink, "/tmp/rwxprobe.dst", 0, 0);
    }
    sys3(SYS_close, fd, 0, 0);

    out(failed ? "rwxprobe: some failed\n" : "rwxprobe: 0 failed\n");
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
