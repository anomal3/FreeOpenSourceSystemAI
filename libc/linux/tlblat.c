/* Linux x86-64 program without libc (21.11.2026, hw-log14): how long a TLB
 * shootdown waits for a CPU that is inside a long system call.
 *
 * Diablo II Resurrected on 16 CPUs stalled 7-16 ms in shootdowns, and the
 * late CPU was never in a spin lock: Linux calls ran with interrupts off
 * (the `syscall` entry masks IF), so a CPU in a long call could not take the
 * flush IPI. Here:
 *   A. three threads spin in user code; the main thread maps, touches and
 *      unmaps one page 300 times (each munmap flushes the other CPUs) -
 *      average and worst munmap;
 *   B. the same while the three threads read 16 MiB from /dev/zero again and
 *      again (a long call that never sleeps) - average and worst munmap, and
 *      the average read.
 * Check: in B the worst munmap is well below one read (the flush does not
 * wait for the call to end). Also getrusage: the CPU time of the process and
 * of a busy thread goes up (it used to be all zeros, and Wine/D2R spun on it).
 *
 * `tlblat: ok <what>` / `tlblat: FAILED <what>`; total `tlblat: N failed`.
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
    SYS_pread64 = 17,
    SYS_write = 1,
    SYS_close = 3,
    SYS_mmap = 9,
    SYS_munmap = 11,
    SYS_sched_yield = 24,
    SYS_nanosleep = 35,
    SYS_getrusage = 98,
    SYS_clock_gettime = 228,
    SYS_exit_group = 231,
    SYS_openat = 257,
};
#define BUSY 3
#define ROUNDS 300
#define READ_BYTES (16L << 20)

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
    const char *head = good ? "tlblat: ok " : "tlblat: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}
/* `label` + number + `tail` in one line. */
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
static u32 load(volatile u32 *word) { return __atomic_load_n(word, __ATOMIC_ACQUIRE); }

/* struct rusage: ru_utime, ru_stime (timeval), then 14 longs. */
struct rusage_ { i64 utime_s, utime_us, stime_s, stime_us; i64 rest[14]; };
static i64 cpu_us(int who) {
    struct rusage_ r;
    memset(&r, 0xff, sizeof r);
    if (sys3(SYS_getrusage, who, &r, 0) != 0) return -1;
    return (r.utime_s + r.stime_s) * 1000000L + r.utime_us + r.stime_us;
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
#define THREAD_FLAGS (0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000)

static char stacks[BUSY][32 * 1024] __attribute__((aligned(16)));

/* 0 - spin in user code, 1 - read /dev/zero, 2 - leave. */
static volatile u32 mode, alive;
static int zero_fd;
static volatile i64 reads, read_ns, thread_cpu_us;

void busy_main(void) {
    char *buf = (char *)sys6(SYS_mmap, 0, READ_BYTES, 3, 0x22, -1, 0);
    if ((i64)buf < 0) buf = 0;
    else memset(buf, 1, READ_BYTES);
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    for (;;) {
        u32 m = load(&mode);
        if (m == 2) break;
        if (m == 1 && buf) {
            i64 t0 = ns();
            /* pread64: our read() copies under the process lock, so munmap
             * would wait for that lock, not for the flush; pread64 fills a
             * kernel chunk buffer and copies without it. */
            sys6(SYS_pread64, zero_fd, (i64)buf, READ_BYTES, 0, 0, 0);
            __atomic_add_fetch(&read_ns, ns() - t0, __ATOMIC_ACQ_REL);
            __atomic_add_fetch(&reads, 1, __ATOMIC_ACQ_REL);
        } else {
            for (int i = 0; i < 1000; i++) __asm__ volatile("pause");
        }
    }
    /* RUSAGE_THREAD: this thread did real work, its time is not zero. */
    i64 mine = cpu_us(1);
    i64 seen = thread_cpu_us;
    if (mine > seen) thread_cpu_us = mine;
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

/* ROUNDS of map/touch/unmap one page: average and worst munmap, ns. */
static void unmap_rounds(i64 *avg, i64 *worst) {
    i64 sum = 0, top = 0;
    for (int i = 0; i < ROUNDS; i++) {
        volatile char *page = (volatile char *)sys6(SYS_mmap, 0, 4096, 3, 0x22, -1, 0);
        if ((i64)page < 0) {
            check(0, "mmap of one page");
            return;
        }
        page[0] = 1;
        i64 t0 = ns();
        sys3(SYS_munmap, page, 4096, 0);
        i64 took = ns() - t0;
        sum += took;
        if (took > top) top = took;
        /* Let the busy threads get back into their calls. */
        if (i % 16 == 0) sys3(SYS_sched_yield, 0, 0, 0);
    }
    *avg = sum / ROUNDS;
    *worst = top;
}

void start_c(u64 *sp) {
    /* `tlblat baseline`: numbers only, no munmap check (the kernel runs
     * Linux calls with interrupts off after `lxirq off`). */
    const char *arg = sp[0] > 1 ? (const char *)sp[2] : "";
    int baseline = arg[0] == 'b';
    out(baseline ? "tlblat: start (baseline)\n" : "tlblat: start\n");
    zero_fd = (int)sys6(SYS_openat, -100, (i64)"/dev/zero", 0, 0, 0, 0);
    check(zero_fd >= 0, "/dev/zero opens");

    i64 before = cpu_us(0);
    check(before >= 0, "getrusage(RUSAGE_SELF) answers");
    for (int t = 0; t < BUSY; t++) {
        if (raw_thread(THREAD_FLAGS, stacks[t] + sizeof stacks[t], busy_main) <= 0) check(0, "clone starts a busy thread");
    }
    for (int i = 0; i < 400 && load(&alive) < BUSY; i++) nap_ms(5);
    check(load(&alive) == BUSY, "3 busy threads are up");

    i64 avg_a = 0, worst_a = 0, avg_b = 0, worst_b = 0;
    unmap_rounds(&avg_a, &worst_a);
    number("tlblat: A spinning threads: munmap average ", avg_a / 1000, " us");
    number("tlblat: A spinning threads: munmap worst ", worst_a / 1000, " us");

    __atomic_store_n(&mode, 1, __ATOMIC_RELEASE);
    nap_ms(50);
    unmap_rounds(&avg_b, &worst_b);
    __atomic_store_n(&mode, 2, __ATOMIC_RELEASE);
    for (int i = 0; i < 2000 && load(&alive) > 0; i++) nap_ms(5);
    check(load(&alive) == 0, "busy threads exit");
    i64 n = reads ? reads : 1;
    i64 read_avg = read_ns / n;
    number("tlblat: B threads in /dev/zero reads: munmap average ", avg_b / 1000, " us");
    number("tlblat: B threads in /dev/zero reads: munmap worst ", worst_b / 1000, " us");
    number("tlblat: B one 16 MiB read takes ", read_avg / 1000, " us on average");
    number("tlblat: B reads done ", reads, "");
    if (baseline) {
        out("tlblat: baseline, munmap not checked\n");
    } else if (read_avg < 2000000L) {
        check(1, "munmap vs long call: inconclusive, the read is shorter than 2 ms");
    } else {
        check(worst_b < read_avg / 2, "munmap does not wait for a whole read on another cpu");
    }

    i64 after = cpu_us(0);
    number("tlblat: getrusage self cpu ms ", (after - before) / 1000, "");
    number("tlblat: getrusage busy thread cpu ms ", thread_cpu_us / 1000, "");
    check(after > before, "getrusage(RUSAGE_SELF) time goes up");
    check(thread_cpu_us > 0, "getrusage(RUSAGE_THREAD) of a busy thread is not zero");

    out(failed ? "tlblat: some failed\n" : "tlblat: 0 failed\n");
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
