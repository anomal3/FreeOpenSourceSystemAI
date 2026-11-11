/* Программа Linux x86-64 без libc (12.11.2026): сигнал потоку, который
 * крутится в своём коде и в ядро не входит. Так Wine приостанавливает потоки
 * (`SuspendThread`: `SIGUSR1` через `tgkill`, обработчик пишет контекст), а
 * .NET и Java останавливают их для сборки мусора.
 *
 * Поток заводится сырым `clone` и ждёт флага в пустом цикле. Главный шлёт ему
 * `SIGUSR1`; обработчик (на том потоке) отмечает, что пришёл, проверяет, что
 * прерван был именно цикл (`rip` в `ucontext`), и правит в контексте `r12` —
 * после `rt_sigreturn` цикл видит новое значение и выходит.
 *
 * Строки — `sigasync: ok <что>` / `sigasync: FAILED <что>`, итог —
 * `sigasync: N failed`.
 */

typedef unsigned long u64;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

enum {
    SYS_write = 1, SYS_rt_sigaction = 13, SYS_rt_sigreturn = 15, SYS_nanosleep = 35, SYS_getpid = 39,
    SYS_tgkill = 234, SYS_exit_group = 231,
};
enum { SIGUSR1 = 10, SA_SIGINFO = 4, SA_RESTORER = 0x04000000 };

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
    const char *head = good ? "sigasync: ok " : "sigasync: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

struct timespec_ { i64 sec, nsec; };
static void nap_ms(i64 ms) {
    struct timespec_ t = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, &t, 0, 0);
}

static volatile int spinning, handled, rip_in_loop, child_done;

/* Пустой цикл без вызовов: ждёт, пока обработчик не впишет единицу в `r12`.
 * Границы цикла — метки, по ним обработчик узнаёт прерванное место. */
void spin_until_r12(void);
extern char spin_begin[], spin_end[];
__asm__(".text\n"
        ".globl spin_until_r12\n"
        "spin_until_r12:\n"
        "  push %r12\n"
        "  xor %r12d, %r12d\n"
        ".globl spin_begin\n"
        "spin_begin:\n"
        "  test %r12, %r12\n"
        "  jz spin_begin\n"
        ".globl spin_end\n"
        "spin_end:\n"
        "  pop %r12\n"
        "  ret\n");

/* `ucontext`: `uc_mcontext.gregs` с 40-го байта; r12 — 4-й, rip — 16-й. */
void on_usr1(int sig, void *info, void *uc) {
    (void)info;
    u64 *gregs = (u64 *)((char *)uc + 40);
    u64 rip = gregs[16];
    u64 begin, end;
    __asm__("lea spin_begin(%%rip), %0" : "=r"(begin));
    __asm__("lea spin_end(%%rip), %0" : "=r"(end));
    rip_in_loop = sig == SIGUSR1 && rip >= begin && rip <= end;
    gregs[4] = 1;
    handled++;
}

void restorer(void);
__asm__(".text\n"
        ".globl restorer\n"
        "restorer:\n"
        "  mov $15, %eax\n"
        "  syscall\n"
        "  hlt\n");

void child_main(void) {
    spinning = 1;
    spin_until_r12();
    child_done = 1;
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

static char child_stack[64 * 1024] __attribute__((aligned(16)));

void start_c(u64 *sp) {
    (void)sp;
    out("sigasync: start\n");
    /* Адреса — через `lea` по `rip`: инициализатор массива указателями
     * компилятор кладёт в данные с перемещениями, а применять их некому. */
    u64 handler, restorer_at;
    __asm__("lea on_usr1(%%rip), %0" : "=r"(handler));
    __asm__("lea restorer(%%rip), %0" : "=r"(restorer_at));
    u64 act[4];
    act[0] = handler;
    act[1] = SA_SIGINFO | SA_RESTORER;
    act[2] = restorer_at;
    act[3] = 0;
    check(sys6(SYS_rt_sigaction, SIGUSR1, (i64)act, 0, 8, 0, 0) == 0, "rt_sigaction SIGUSR1");

    long tid = raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, child_stack + sizeof child_stack, child_main);
    check(tid > 0, "clone starts a spinning thread");
    for (int i = 0; i < 400 && !spinning; i++) nap_ms(5);
    /* Дать ему уйти в цикл. */
    nap_ms(50);
    i64 pid = sys3(SYS_getpid, 0, 0, 0);
    check(sys3(SYS_tgkill, pid, tid, SIGUSR1) == 0, "tgkill SIGUSR1 to the spinning thread");
    for (int i = 0; i < 400 && !child_done; i++) nap_ms(5);
    check(handled == 1, "the handler ran on a thread that never enters the kernel");
    check(rip_in_loop, "the interrupted rip is inside the spin loop");
    check(child_done, "a register changed in ucontext is seen after rt_sigreturn");

    out(failed ? "sigasync: some failed\n" : "sigasync: 0 failed\n");
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
