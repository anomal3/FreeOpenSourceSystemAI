/* Linux x86-64 program without libc (25.11.2026): task priorities.
 *
 * hw-log19: OBS under `nice -n 5` (llvmpipe, x264 threads) starved the game
 * it recorded - nice was accepted and ignored. Here:
 *   A. the calls: getpriority/setpriority (thread-level, as Linux), the nice
 *      inherited by fork, sched_setscheduler/sched_getscheduler (SCHED_IDLE
 *      and back), sched_getattr/sched_setattr, sched_get_priority_max,
 *      RLIMIT_NICE;
 *   B. the share: N threads at nice 0 and N at nice <B> (default 10) burn the
 *      CPU for <ms> (default 4000); each counts its rounds; the nice 0 group
 *      must do >= 80 % of the work (`prioprobe N ms B even`: 40..60 %, for
 *      `prio off`).
 *
 * `prioprobe [N [ms [B [even]]]]`.
 * `prioprobe: ok <what>` / `prioprobe: FAILED <what>`; total
 * `prioprobe: N failed`.
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
    SYS_write = 1,
    SYS_mmap = 9,
    SYS_nanosleep = 35,
    SYS_fork = 57,
    SYS_exit = 60,
    SYS_wait4 = 61,
    SYS_getuid = 102,
    SYS_getpriority = 140,
    SYS_setpriority = 141,
    SYS_sched_setscheduler = 144,
    SYS_sched_getscheduler = 145,
    SYS_sched_get_priority_max = 146,
    SYS_gettid = 186,
    SYS_clock_gettime = 228,
    SYS_exit_group = 231,
    SYS_prlimit64 = 302,
    SYS_sched_setattr = 314,
    SYS_sched_getattr = 315,
};
#define MAX_GROUP 16

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
static void out(const char *s) { sys3(SYS_write, 1, s, strlen_(s)); }

static int failed;
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "prioprobe: ok " : "prioprobe: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

/* A line of text pieces and numbers: `put` appends, `flush` writes it. */
static char text[256];
static u64 text_len;
static void put(const char *s) {
    for (; *s && text_len < sizeof text - 2; s++) text[text_len++] = *s;
}
static void put_num(i64 v) {
    if (v < 0) {
        put("-");
        v = -v;
    }
    char digits[24];
    int n = 0;
    do {
        digits[n++] = (char)('0' + v % 10);
        v /= 10;
    } while (v && n < 20);
    while (n && text_len < sizeof text - 2) text[text_len++] = digits[--n];
}
static void flush(void) {
    text[text_len++] = '\n';
    sys3(SYS_write, 1, text, text_len);
    text_len = 0;
}

static i64 parse(const char *s, i64 fallback) {
    if (!s || !*s) return fallback;
    i64 v = 0;
    for (; *s; s++) {
        if (*s < '0' || *s > '9') return fallback;
        v = v * 10 + (*s - '0');
    }
    return v;
}

struct timespec_ { i64 sec, nsec; };
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}
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

static char stacks[2 * MAX_GROUP][16 * 1024] __attribute__((aligned(16)));
/* One counter per cache line: the threads must not slow each other. */
static struct { volatile u64 rounds; char pad[56]; } work[2 * MAX_GROUP] __attribute__((aligned(64)));
static volatile u32 next_index, alive, stop, group, low_nice, nice_set;

void burn_main(void) {
    u32 me = __atomic_fetch_add(&next_index, 1, __ATOMIC_ACQ_REL);
    if (me >= group) {
        /* The second group: this thread only, as Linux PRIO_PROCESS on a tid. */
        i64 tid = sys3(SYS_gettid, 0, 0, 0);
        if (sys3(SYS_setpriority, 0, tid, low_nice) == 0 && sys3(SYS_getpriority, 0, tid, 0) == 20 - (i64)low_nice)
            __atomic_add_fetch(&nice_set, 1, __ATOMIC_ACQ_REL);
    }
    __atomic_add_fetch(&alive, 1, __ATOMIC_ACQ_REL);
    while (!load(&stop)) {
        for (volatile int i = 0; i < 20000; i++) {
        }
        work[me].rounds++;
    }
    __atomic_sub_fetch(&alive, 1, __ATOMIC_ACQ_REL);
}

/* struct sched_attr, VER0. */
struct sched_attr_ {
    u32 size, policy;
    u64 flags;
    int nice;
    u32 priority;
    u64 runtime, deadline, period;
};

static void calls(void) {
    i64 uid = sys3(SYS_getuid, 0, 0, 0);
    check(sys3(SYS_getpriority, 0, 0, 0) == 20, "getpriority of a fresh program is 20 - 0");
    check(sys3(SYS_setpriority, 0, 0, 3) == 0, "setpriority(PRIO_PROCESS, 0, 3)");
    check(sys3(SYS_getpriority, 0, 0, 0) == 17, "getpriority answers 20 - 3");
    i64 tid = sys3(SYS_gettid, 0, 0, 0);
    check(sys3(SYS_getpriority, 0, tid, 0) == 17, "getpriority by tid");
    check(sys3(SYS_getpriority, 1, 0, 0) == 17, "getpriority(PRIO_PGRP, 0)");
    check(sys3(SYS_getpriority, 0, 999999, 0) == -3, "getpriority of no task: ESRCH");
    check(sys3(SYS_getpriority, 7, 0, 0) == -22, "getpriority with a bad which: EINVAL");

    /* The nice goes to a child of fork. */
    i64 pid = sys3(SYS_fork, 0, 0, 0);
    if (pid == 0) {
        i64 code = sys3(SYS_getpriority, 0, 0, 0) == 17 ? 0 : 1;
        sys3(SYS_exit_group, code, 0, 0);
    }
    int status = -1;
    if (pid > 0) sys4(SYS_wait4, pid, &status, 0, 0);
    check(pid > 0 && status == 0, "fork: the child has nice 3 too");

    /* A lower nice: root only (RLIMIT_NICE is 0). */
    u64 limit[2] = {7, 7};
    check(sys4(SYS_prlimit64, 0, 13, 0, limit) == 0 && limit[0] == 0 && limit[1] == 0, "RLIMIT_NICE is 0");
    i64 back = sys3(SYS_setpriority, 0, 0, 0);
    if (uid == 0) check(back == 0 && sys3(SYS_getpriority, 0, 0, 0) == 20, "root sets nice 0 back");
    else check(back == -13, "a user cannot lower nice: EACCES");

    check(sys3(SYS_sched_getscheduler, 0, 0, 0) == 0, "sched_getscheduler: SCHED_OTHER");
    int param = 0;
    check(sys3(SYS_sched_setscheduler, 0, 5, &param) == 0, "sched_setscheduler(SCHED_IDLE)");
    check(sys3(SYS_sched_getscheduler, 0, 0, 0) == 5, "sched_getscheduler: SCHED_IDLE");
    check(sys3(SYS_sched_setscheduler, 0, 0, &param) == 0, "sched_setscheduler(SCHED_OTHER)");
    param = 10;
    i64 fifo = sys3(SYS_sched_setscheduler, 0, 1, &param);
    if (uid == 0) {
        check(fifo == 0 && sys3(SYS_sched_getscheduler, 0, 0, 0) == 1, "root: SCHED_FIFO 10");
        param = 0;
        sys3(SYS_sched_setscheduler, 0, 0, &param);
    } else {
        check(fifo == -1, "a user cannot take SCHED_FIFO: EPERM");
    }
    check(sys3(SYS_sched_get_priority_max, 1, 0, 0) == 99, "sched_get_priority_max(SCHED_FIFO) = 99");
    check(sys3(SYS_sched_get_priority_max, 0, 0, 0) == 0, "sched_get_priority_max(SCHED_OTHER) = 0");

    struct sched_attr_ attr;
    memset(&attr, 0, sizeof attr);
    check(sys4(SYS_sched_getattr, 0, &attr, sizeof attr, 0) == 0 && attr.size == 48 && attr.policy == 0, "sched_getattr");
    memset(&attr, 0, sizeof attr);
    attr.size = sizeof attr;
    attr.nice = 6;
    check(sys4(SYS_sched_setattr, 0, &attr, 0, 0) == 0, "sched_setattr(nice 6)");
    memset(&attr, 0, sizeof attr);
    check(sys4(SYS_sched_getattr, 0, &attr, sizeof attr, 0) == 0 && attr.nice == 6, "sched_getattr: nice 6");
    check(sys3(SYS_getpriority, 0, 0, 0) == 14, "getpriority after sched_setattr");
    if (uid == 0) sys3(SYS_setpriority, 0, 0, 0);
}

static void share(i64 per_group, i64 ms, i64 nice_b, int even) {
    group = (u32)per_group;
    low_nice = (u32)nice_b;
    i64 own = 20 - sys3(SYS_getpriority, 0, 0, 0);
    /* A user cannot go back to nice 0 after the calls: keep the distance. */
    if (own > 0) {
        nice_b = own + nice_b > 19 ? 19 : own + nice_b;
        low_nice = (u32)nice_b;
    }
    for (int t = 0; t < 2 * per_group; t++) {
        if (raw_thread(THREAD_FLAGS, stacks[t] + sizeof stacks[t], burn_main) <= 0) check(0, "clone starts a busy thread");
    }
    for (int i = 0; i < 400 && load(&alive) < 2 * per_group; i++) nap_ms(5);
    check(load(&alive) == 2 * per_group, "busy threads are up");
    check(load(&nice_set) == per_group, "each thread of the second group set its own nice");
    nap_ms(300);
    u64 start[2 * MAX_GROUP];
    for (int t = 0; t < 2 * per_group; t++) start[t] = work[t].rounds;
    nap_ms(ms);
    u64 a = 0, b = 0;
    for (int t = 0; t < 2 * per_group; t++) {
        u64 done = work[t].rounds - start[t];
        if (t < per_group) a += done;
        else b += done;
    }
    __atomic_store_n(&stop, 1, __ATOMIC_RELEASE);
    for (int i = 0; i < 2000 && load(&alive) > 0; i++) nap_ms(5);
    check(load(&alive) == 0, "busy threads exit");
    u64 total = a + b ? a + b : 1;
    i64 permille = (i64)(a * 1000 / total);
    put("prioprobe: ");
    put_num(per_group);
    put("+");
    put_num(per_group);
    put(" threads, ");
    put_num(ms);
    put(" ms: nice ");
    put_num(own);
    put(" got ");
    put_num(permille / 10);
    put(".");
    put_num(permille % 10);
    put("% of the work, nice ");
    put_num(nice_b);
    put(" got ");
    put_num((1000 - permille) / 10);
    put(".");
    put_num((1000 - permille) % 10);
    put("%");
    flush();
    if (even) check(permille >= 400 && permille <= 600, "priorities off: an even share");
    else check(permille >= 800, "the nice 0 group did at least 80 % of the work");
}

void start_c(u64 *sp) {
    u64 argc = sp[0];
    const char *arg1 = argc > 1 ? (const char *)sp[2] : 0;
    const char *arg2 = argc > 2 ? (const char *)sp[3] : 0;
    const char *arg3 = argc > 3 ? (const char *)sp[4] : 0;
    const char *arg4 = argc > 4 ? (const char *)sp[5] : 0;
    i64 per_group = parse(arg1, 1);
    if (per_group < 1) per_group = 1;
    if (per_group > MAX_GROUP) per_group = MAX_GROUP;
    i64 ms = parse(arg2, 4000);
    i64 nice_b = parse(arg3, 10);
    if (nice_b > 19) nice_b = 19;
    int even = arg4 && arg4[0] == 'e';
    out("prioprobe: start\n");
    calls();
    share(per_group, ms, nice_b, even);
    out(failed ? "prioprobe: some failed\n" : "prioprobe: 0 failed\n");
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
