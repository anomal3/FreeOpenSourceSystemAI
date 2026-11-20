/* Программа Linux x86-64 без libc: ядро пишет в память процесса, пока соседний
 * поток делает `fork` (20.11.2026, сбой TrackMania под Proton: `CPU FAULT` в
 * `linux::poll::wait` — ядро проверило массив `pollfd` на запись, соседний
 * поток сделал `fork`, страница стала разделённой «только на чтение», и запись
 * `revents` ядром пришла отказом страницы в кольце 0).
 *
 * Потоки (CLONE_THREAD) без остановки зовут вызовы, в которых **ядро первым**
 * пишет в страницы процесса: `poll` по 1000 записям `pollfd` (8000 байт —
 * через границы страниц, в памяти `mmap` и в `.bss`), `read` из канала в буфер,
 * `clock_gettime`. Сама программа эти страницы между вызовами не трогает —
 * значит, после каждого `fork` первым их пишет ядро и разрывает копию при
 * записи оно же. Главный поток тем временем делает `fork` раз за разом
 * (ребёнок пишет в общую страницу и выходит), `wait4` — его статус тоже пишет
 * ядро. Проверки: ни одного `EFAULT`/неверного ответа, ни одного отказа ядра,
 * запись ребёнка не видна родителю.
 *
 * `forkpoll: ok <что>` / `forkpoll: FAILED <что>`; итог `forkpoll: N failed`.
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
    SYS_read = 0, SYS_write = 1, SYS_poll = 7, SYS_mmap = 9, SYS_rt_sigaction = 13, SYS_rt_sigreturn = 15,
    SYS_pipe = 22, SYS_nanosleep = 35, SYS_clone = 56, SYS_exit = 60, SYS_wait4 = 61,
    SYS_clock_gettime = 228, SYS_exit_group = 231,
};
enum { PROT_RW = 3, MAP_PRIVATE = 2, MAP_ANON = 0x20 };
enum { POLLIN = 1, SIGSEGV = 11, SIGCHLD = 17, SA_SIGINFO = 4, SA_RESTORER = 0x04000000 };
#define PAGE 4096UL

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
static void num(const char *label, u64 v) {
    char line[96], digits[24];
    u64 len = 0, d = 0;
    for (const char *s = label; *s && len < 60; s++) line[len++] = *s;
    do { digits[d++] = (char)('0' + v % 10); v /= 10; } while (v);
    while (d) line[len++] = digits[--d];
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
}

static int failed;
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "forkpoll: ok " : "forkpoll: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

static void on_segv(int sig, void *info, void *ctx) {
    (void)sig; (void)ctx;
    out("forkpoll: FAILED SIGSEGV\n");
    num("forkpoll: fault address ", *(u64 *)((char *)info + 16));
    sys3(SYS_exit_group, 99, 0, 0);
}
void restorer(void);
__asm__(".text\n.globl restorer\nrestorer:\n  mov $15, %eax\n  syscall\n");

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

static void nap_ms(i64 ms) {
    i64 t[2] = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, t, 0, 0);
}

struct pollfd { int fd; short events; short revents; };

#define POLLERS 2
#define ENTRIES 1000
#define READ_BYTES 3000
#define FORKS 300
#define THREADS (POLLERS + 1)

static char stacks[THREADS][32 * 1024] __attribute__((aligned(16)));
/* Массив второго опрашивающего — в `.bss`, со сдвигом через границу страниц. */
static struct pollfd bss_fds[ENTRIES + PAGE / 8] __attribute__((aligned(4096)));
static volatile int started, finished, stop, next_id;
static volatile u64 rounds[THREADS];
static volatile int bad[THREADS];
static volatile i64 bad_answer[THREADS];
static int pipes[THREADS][2];
static struct pollfd *poll_fds[POLLERS];
static unsigned char *read_buf;
static volatile unsigned char shared_page[PAGE] __attribute__((aligned(4096)));

/* Опрашивающий: каждый раз все ENTRIES записей готовы (в канале байт), ответ —
 * ровно ENTRIES. `revents` проверяется чтением — запись в массив только у
 * ядра. */
static void poller(int id) {
    struct pollfd *fds = poll_fds[id];
    while (!stop) {
        i64 ready = sys3(SYS_poll, fds, ENTRIES, 0);
        if (ready != ENTRIES) {
            bad[id] = 1;
            bad_answer[id] = ready;
            break;
        }
        if (fds[ENTRIES - 1].revents != POLLIN || fds[0].revents != POLLIN) {
            bad[id] = 2;
            break;
        }
        rounds[id]++;
    }
}

/* Читающий: пишет в свой канал READ_BYTES байт из неизменного образца и
 * читает их в буфер `mmap` через границы страниц; потом `clock_gettime` в
 * тот же буфер. */
static void reader(int id) {
    static const unsigned char pattern[64] = "forkpoll pattern: the kernel copies this into a forked page...";
    static unsigned char source[READ_BYTES];
    for (u64 i = 0; i < READ_BYTES; i++) source[i] = pattern[i % 64];
    int r = pipes[id][0], w = pipes[id][1];
    while (!stop) {
        if (sys3(SYS_write, w, source, READ_BYTES) != READ_BYTES) { bad[id] = 3; break; }
        u64 got = 0;
        while (got < READ_BYTES) {
            i64 n = sys3(SYS_read, r, read_buf + got, READ_BYTES - got);
            if (n <= 0) { bad[id] = 4; bad_answer[id] = n; break; }
            got += (u64)n;
        }
        if (bad[id]) break;
        for (u64 i = 0; i < READ_BYTES; i += 97)
            if (read_buf[i] != source[i]) { bad[id] = 5; break; }
        if (bad[id]) break;
        /* `timespec` — выровненный, за прочитанным, на той же странице. */
        i64 ret = sys3(SYS_clock_gettime, 1, ((u64)read_buf + READ_BYTES + 15) & ~15UL, 0);
        if (ret != 0) { bad[id] = 6; bad_answer[id] = ret; break; }
        rounds[id]++;
    }
}

void worker(void) {
    int id = __atomic_fetch_add(&next_id, 1, __ATOMIC_SEQ_CST);
    __atomic_fetch_add(&started, 1, __ATOMIC_SEQ_CST);
    if (id < POLLERS) poller(id);
    else reader(id);
    __atomic_fetch_add(&finished, 1, __ATOMIC_SEQ_CST);
}

void start_c(u64 *sp) {
    (void)sp;
    out("forkpoll: start\n");
    u64 act[4] = {(u64)on_segv, SA_SIGINFO | SA_RESTORER, 0, 0};
    __asm__("lea restorer(%%rip), %0" : "=r"(act[2]));
    sys6(SYS_rt_sigaction, SIGSEGV, (i64)act, 0, 8, 0, 0);

    int pipes_ok = 1;
    for (int i = 0; i < THREADS; i++)
        if (sys3(SYS_pipe, pipes[i], 0, 0) != 0) pipes_ok = 0;
    check(pipes_ok, "pipe for every thread");
    /* Байт в каналах опрашивающих — их записи всегда готовы на чтение. */
    for (int i = 0; i < POLLERS; i++) sys3(SYS_write, pipes[i][1], "x", 1);

    char *heap = (char *)sys6(SYS_mmap, 0, 16 * PAGE, PROT_RW, MAP_PRIVATE | MAP_ANON, -1, 0);
    check((u64)heap < (u64)-4096, "anonymous mmap for the buffers");
    /* Первый массив — в `mmap`, с середины страницы: 8000 байт на трёх. */
    poll_fds[0] = (struct pollfd *)(heap + PAGE / 2);
    poll_fds[1] = bss_fds + (PAGE - 24) / 8;
    read_buf = (unsigned char *)(heap + 6 * PAGE + 100);
    for (int p = 0; p < POLLERS; p++)
        for (int i = 0; i < ENTRIES; i++) {
            poll_fds[p][i].fd = pipes[p][0];
            poll_fds[p][i].events = POLLIN;
            poll_fds[p][i].revents = 0;
        }
    shared_page[0] = 1;

    for (int i = 0; i < THREADS; i++)
        raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, stacks[i] + sizeof stacks[i], worker);
    while (started < THREADS) nap_ms(1);
    /* Пусть все войдут в цикл. */
    while (rounds[0] == 0 || rounds[1] == 0 || rounds[POLLERS] == 0) {
        if (bad[0] || bad[1] || bad[POLLERS]) break;
        nap_ms(1);
    }

    int forks = 0, fork_errors = 0, wait_errors = 0;
    for (int i = 0; i < FORKS; i++) {
        i64 pid = sys6(SYS_clone, SIGCHLD, 0, 0, 0, 0, 0);
        if (pid == 0) {
            /* Ребёнок: запись в общую страницу — его копия, не родителя. */
            shared_page[0] = 2;
            sys3(SYS_exit_group, shared_page[0] == 2 ? 0 : 1, 0, 0);
        }
        if (pid < 0) { fork_errors++; continue; }
        forks++;
        int status = -1;
        if (sys6(SYS_wait4, pid, (i64)&status, 0, 0, 0, 0) != pid || status != 0) wait_errors++;
        if (bad[0] || bad[1] || bad[POLLERS]) break;
    }
    stop = 1;
    for (int i = 0; i < 3000 && finished < THREADS; i++) nap_ms(10);

    num("forkpoll: forks ", (u64)forks);
    num("forkpoll: poll rounds ", rounds[0] + rounds[1]);
    num("forkpoll: read rounds ", rounds[POLLERS]);
    check(fork_errors == 0 && forks == FORKS, "300 forks while three threads poll/read into the forked pages");
    check(wait_errors == 0, "wait4 got every child with status 0");
    check(finished == THREADS, "all threads finished");
    for (int i = 0; i < POLLERS; i++)
        if (bad[i]) num("forkpoll: poller answer ", (u64)bad_answer[i]);
    check(!bad[0] && !bad[1], "poll answered every entry ready every time (no EFAULT)");
    if (bad[POLLERS]) num("forkpoll: reader stage ", (u64)bad[POLLERS]);
    check(!bad[POLLERS], "read and clock_gettime into forked pages never failed");
    check(shared_page[0] == 1, "the children's writes did not reach the parent (copy on write)");

    out(failed ? "forkpoll: some failed\n" : "forkpoll: 0 failed\n");
    sys3(SYS_exit_group, failed, 0, 0);
    for (;;) { }
}

__asm__(".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
