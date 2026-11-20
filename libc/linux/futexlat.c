/* Программа Linux x86-64 без libc (18.11.2026): цена пробуждения через
 * futex и потерянные пробуждения при 16 потоках — P9 плана Proton.
 *
 * Лавапайп (DXVK на ЦП) раздаёт каждую порцию кадра шестнадцати потокам
 * через `futex` и ждёт их так же; wineserver и клиенты Wine передают друг
 * другу работу тем же способом. Здесь:
 *   A. пинг-понг двух потоков: 2000 передач туда и обратно — среднее время
 *      круга;
 *   B. веер: главный поток будит 16 рабочих одним FUTEX_WAKE, последний из
 *      них будит главного — 300 кругов; ожидание главного со сроком 2 с, и
 *      истёкший срок — потерянное пробуждение.
 * Числа — для сравнения до и после правок планировщика; проверки — только
 * «ничего не потерялось».
 *
 * `futexlat: ok <что>` / `futexlat: FAILED <что>`; итог `futexlat: N failed`.
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

enum { SYS_write = 1, SYS_nanosleep = 35, SYS_futex = 202, SYS_clock_gettime = 228, SYS_exit_group = 231 };
enum { FUTEX_WAIT_PRIVATE = 0 | 128, FUTEX_WAKE_PRIVATE = 1 | 128 };
enum { ETIMEDOUT = 110 };
#define WORKERS 16
#define ROUNDS 300
#define PINGS 2000

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
    const char *head = good ? "futexlat: ok " : "futexlat: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}
/* `label` + число + `tail` одной строкой. */
static void number(const char *label, i64 v, const char *tail) {
    char line[160];
    u64 len = 0;
    for (const char *s = label; *s && len < 80; s++) line[len++] = *s;
    char digits[24];
    int n = 0;
    do {
        digits[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && n < 20);
    while (n) line[len++] = digits[--n];
    for (const char *s = tail; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
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
static i64 wait_word(volatile u32 *word, u32 expected, i64 timeout_ms) {
    struct timespec_ t = {timeout_ms / 1000, (timeout_ms % 1000) * 1000000L};
    return sys6(SYS_futex, (i64)word, FUTEX_WAIT_PRIVATE, expected, timeout_ms ? (i64)&t : 0, 0, 0);
}
static void wake_word(volatile u32 *word, int count) { sys3(SYS_futex, word, FUTEX_WAKE_PRIVATE, count); }
static u32 load(volatile u32 *word) { return __atomic_load_n(word, __ATOMIC_ACQUIRE); }

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
#define THREAD_FLAGS (0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000)

static char stacks[WORKERS + 1][32 * 1024] __attribute__((aligned(16)));

/* ---- A. пинг-понг ---- */
static volatile u32 ping, pong, ping_exit;
void pong_main(void) {
    for (u32 i = 1; i <= PINGS; i++) {
        while (load(&ping) < i) wait_word(&ping, i - 1, 1000);
        __atomic_store_n(&pong, i, __ATOMIC_RELEASE);
        wake_word(&pong, 1);
    }
    __atomic_store_n(&ping_exit, 1, __ATOMIC_RELEASE);
}

/* ---- B. веер ---- */
static volatile u32 generation, done, stop, alive;
void worker_main(void) {
    u32 seen = 0;
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    for (;;) {
        u32 now = load(&generation);
        while (now == seen && !load(&stop)) {
            wait_word(&generation, seen, 1000);
            now = load(&generation);
        }
        if (load(&stop)) break;
        seen = now;
        if (__atomic_add_fetch(&done, 1, __ATOMIC_ACQ_REL) == WORKERS) wake_word(&done, 1);
    }
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

void start_c(u64 *sp) {
    (void)sp;
    out("futexlat: start\n");

    long tid = raw_thread(THREAD_FLAGS, stacks[0] + sizeof stacks[0], pong_main);
    check(tid > 0, "clone starts the pong thread");
    i64 lost = 0;
    i64 t0 = ns();
    for (u32 i = 1; i <= PINGS; i++) {
        __atomic_store_n(&ping, i, __ATOMIC_RELEASE);
        wake_word(&ping, 1);
        i64 started = ns();
        while (load(&pong) < i) {
            if (wait_word(&pong, i - 1, 2000) == -ETIMEDOUT && load(&pong) < i && ns() - started > 2000000000L) {
                lost++;
                break;
            }
        }
    }
    i64 spent = ns() - t0;
    number("futexlat: ping-pong ", PINGS, " round trips");
    number("futexlat: ping-pong average ", spent / PINGS / 1000, " us per round trip");
    check(lost == 0, "ping-pong lost no wakeup");
    for (int i = 0; i < 400 && !load(&ping_exit); i++) nap_ms(5);

    for (int w = 0; w < WORKERS; w++) {
        if (raw_thread(THREAD_FLAGS, stacks[w + 1] + sizeof stacks[w + 1], worker_main) <= 0) {
            check(0, "clone starts a worker");
        }
    }
    for (int i = 0; i < 400 && load(&alive) < WORKERS; i++) nap_ms(5);
    check(load(&alive) == WORKERS, "16 workers are up");
    lost = 0;
    i64 worst = 0;
    t0 = ns();
    for (int round = 0; round < ROUNDS; round++) {
        __atomic_store_n(&done, 0, __ATOMIC_RELEASE);
        i64 started = ns();
        __atomic_add_fetch(&generation, 1, __ATOMIC_ACQ_REL);
        wake_word(&generation, 0x7fffffff);
        for (;;) {
            u32 d = load(&done);
            if (d >= WORKERS) break;
            if (wait_word(&done, d, 2000) == -ETIMEDOUT && load(&done) < WORKERS) {
                lost++;
                break;
            }
        }
        i64 took = ns() - started;
        if (took > worst) worst = took;
    }
    spent = ns() - t0;
    number("futexlat: fan-out to 16 threads, ", ROUNDS, " rounds");
    number("futexlat: fan-out average ", spent / ROUNDS / 1000, " us per round");
    number("futexlat: fan-out worst ", worst / 1000, " us");
    check(lost == 0, "fan-out lost no wakeup in 300 rounds of 16 threads");
    __atomic_store_n(&stop, 1, __ATOMIC_RELEASE);
    wake_word(&generation, 0x7fffffff);
    for (int i = 0; i < 400 && load(&alive) > 0; i++) nap_ms(5);
    check(load(&alive) == 0, "all workers exit");

    out(failed ? "futexlat: some failed\n" : "futexlat: 0 failed\n");
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
