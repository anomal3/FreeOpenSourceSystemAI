/* Программа Linux x86-64 без libc (12.11.2026): `futex_waitv` и `FUTEX_WAKE_OP`
 * — то, на чём стоит fsync Wine/Proton (`WaitForMultipleObjects` по словам
 * памяти).
 *
 * Устроена как `hello.c`: статическая PIE без перемещений, только `syscall`.
 * Второй поток заводится сырым `clone` (`CLONE_VM | CLONE_THREAD`) на своём
 * стеке и TLS не трогает.
 *
 * Каждая проверка печатает `futexv: ok <что>` или `futexv: FAILED <что>`;
 * итог — `futexv: N failed`, код выхода — число проваленных.
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
    SYS_write = 1, SYS_nanosleep = 35, SYS_futex = 202, SYS_clock_gettime = 228, SYS_exit_group = 231,
    SYS_futex_waitv = 449,
};
enum { EAGAIN = 11, EINVAL = 22, ETIMEDOUT = 110 };
enum { FUTEX_WAKE_PRIVATE = 1 | 128, FUTEX_WAKE_OP_PRIVATE = 5 | 128, FUTEX_32 = 2, FUTEX_PRIVATE = 128 };

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
    const char *head = good ? "futexv: ok " : "futexv: FAILED ";
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
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}

struct waitv { u64 val; u64 uaddr; u32 flags; u32 reserved; };

static volatile u32 word_a, word_b, word_c;
static volatile int child_done;

static i64 waitv2(u32 expect_a, u32 expect_b, struct timespec_ *deadline, u32 flags) {
    struct waitv w[2] = {
        {expect_a, (u64)&word_a, flags, 0},
        {expect_b, (u64)&word_b, flags, 0},
    };
    return sys6(SYS_futex_waitv, (i64)w, 2, 0, (i64)deadline, 1, 0);
}

/* Второй поток: подождать, сменить `word_b` и разбудить по нему. */
void child_main(void) {
    nap_ms(100);
    word_b = 3;
    sys3(SYS_futex, &word_b, FUTEX_WAKE_PRIVATE, 1);
    child_done = 1;
}

/* `raw_thread(flags, stack_top, fn)`: `clone`, в ребёнке — `fn()` на новом
 * стеке и `exit` потока. Указатель на функцию — в `r9`: `clone` копирует
 * регистры родителя ребёнку. */
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

static char child_stack[64 * 1024] __attribute__((aligned(16)));

void start_c(u64 *sp) {
    (void)sp;
    out("futexv: start\n");
    const u32 flags = FUTEX_32 | FUTEX_PRIVATE;

    word_a = 1;
    word_b = 2;
    check(waitv2(1, 5, 0, flags) == -EAGAIN, "a changed word is EAGAIN");

    check(waitv2(1, 2, 0, 0) == -EINVAL, "an entry without FUTEX_32 is EINVAL");
    check(sys6(SYS_futex_waitv, 0, 0, 0, 0, 1, 0) == -EINVAL, "zero waiters is EINVAL");

    i64 t0 = ns();
    struct timespec_ deadline = {(t0 + 50000000L) / 1000000000L, (t0 + 50000000L) % 1000000000L};
    i64 r = waitv2(1, 2, &deadline, flags);
    i64 waited = ns() - t0;
    check(r == -ETIMEDOUT && waited >= 50000000L, "an absolute monotonic deadline is ETIMEDOUT after 50 ms");

    /* CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM */
    long tid = raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, child_stack + sizeof child_stack, child_main);
    check(tid > 0, "clone starts a thread");
    t0 = ns();
    struct timespec_ late = {(t0 + 5000000000L) / 1000000000L, (t0 + 5000000000L) % 1000000000L};
    r = waitv2(1, 2, &late, flags);
    check(r == 1 && word_b == 3, "a wake on the second word returns index 1");
    for (int i = 0; i < 200 && !child_done; i++) nap_ms(5);

    word_c = 5;
    /* op ADD 3, cmp EQ 5: c = 5 + 3, сравнение со старым значением сходится. */
    u32 op = (1u << 28) | (0u << 24) | (3u << 12) | 5u;
    r = sys6(SYS_futex, (i64)&word_a, FUTEX_WAKE_OP_PRIVATE, 1, 1, (i64)&word_c, op);
    check(r == 0 && word_c == 8, "FUTEX_WAKE_OP applies the operation");
    /* op OR 1<<4 через сдвиг (бит 31): 8 | 16 = 24. */
    op = (8u << 28) | (2u << 28) | (1u << 24) | (4u << 12) | 0u;
    r = sys6(SYS_futex, (i64)&word_a, FUTEX_WAKE_OP_PRIVATE, 1, 1, (i64)&word_c, op);
    check(r == 0 && word_c == 24, "FUTEX_WAKE_OP with a shifted argument");

    out(failed ? "futexv: some failed\n" : "futexv: 0 failed\n");
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
