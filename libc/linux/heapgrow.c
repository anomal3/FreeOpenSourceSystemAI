/* Программа Linux x86-64 без libc: шаблон «куча потока glibc» — то, на чём
 * 17.11.2026 упала Trackmania под Proton (#PF, запись, страницы нет, область
 * безымянная, 0x91d8021000..0x91dc000000).
 *
 * glibc (`new_heap`/`grow_heap`/`shrink_heap`) делает так:
 *   1. mmap(NULL, 2*64MiB, PROT_NONE, MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE);
 *   2. munmap головы до границы 64 МиБ и хвоста за ней;
 *   3. mprotect(начало, 0x21000, RW);
 *   4. рост: mprotect(кусок за краем, RW) раз за разом, запись в новое;
 *   5. усадка: madvise(MADV_DONTNEED) хвоста либо mmap(MAP_FIXED, PROT_NONE).
 * Здесь это повторено один в один, потом — 8 потоков, у каждого своя куча, в
 * разнобой (SMP, общий замок программы). Фаза F — гонка подсказок `mmap`
 * (17.11.2026, найдена разбором этого сбоя): ядро проверяло «свободно ли
 * место под подсказку» одним заходом под замок, а отображало следующим, уже
 * как MAP_FIXED, — и два потока с одной подсказкой оба получали её адрес,
 * второй молча снимал отображение первого (так куча glibc легла поверх
 * стека нового потока llvmpipe). Устроена как `futexv.c`: статическая
 * PIE, только `syscall`. Ошибка страницы ловится SIGSEGV и печатает адрес и
 * фазу — программа не падает молча.
 *
 * `heapgrow: ok <что>` / `heapgrow: FAILED <что>`; итог `heapgrow: N failed`.
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
    SYS_write = 1, SYS_mmap = 9, SYS_mprotect = 10, SYS_munmap = 11, SYS_rt_sigaction = 13,
    SYS_rt_sigreturn = 15, SYS_madvise = 28, SYS_nanosleep = 35, SYS_exit_group = 231,
    SYS_clone = 56, SYS_exit = 60,
};
enum { PROT_NONE = 0, PROT_RW = 3 };
enum { MAP_PRIVATE = 2, MAP_FIXED = 0x10, MAP_ANON = 0x20, MAP_NORESERVE = 0x4000 };
enum { MADV_DONTNEED = 4, SIGSEGV = 11, SA_SIGINFO = 4, SA_RESTORER = 0x04000000 };
#define HEAP_MAX (64UL << 20)
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
static void hex(const char *label, u64 v) {
    char line[96];
    u64 len = 0;
    for (const char *s = label; *s && len < 60; s++) line[len++] = *s;
    line[len++] = '0';
    line[len++] = 'x';
    for (int sh = 60; sh >= 0; sh -= 4) line[len++] = "0123456789abcdef"[(v >> sh) & 15];
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
}

static int failed;
static void check(int good, const char *what) {
    char line[160];
    const char *head = good ? "heapgrow: ok " : "heapgrow: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!good) failed++;
}

/* SIGSEGV: печатаем адрес и фазу, выходим с 99 (без обработчика программа
 * молча снималась бы). */
static const char *phase = "start";
static void on_segv(int sig, void *info, void *ctx) {
    (void)sig; (void)ctx;
    out("heapgrow: FAILED SIGSEGV in phase: ");
    out(phase);
    out("\n");
    hex("heapgrow: fault address ", *(u64 *)((char *)info + 16));
    sys3(SYS_exit_group, 99, 0, 0);
}
void restorer(void);
__asm__(".text\n.globl restorer\nrestorer:\n  mov $15, %eax\n  syscall\n");

/* ---- куча glibc ---------------------------------------------------------- */
struct heap { char *base; u64 mprotect_size; };

/* new_heap: резерв 128 МиБ, выровнять по 64 МиБ, обрезать, открыть `first` байт. */
static int new_heap(struct heap *h, u64 first, int hinted, char *hint) {
    char *p1 = (char *)sys6(SYS_mmap, (i64)(hinted ? hint : 0), 2 * HEAP_MAX, PROT_NONE,
                            MAP_PRIVATE | MAP_ANON | MAP_NORESERVE, -1, 0);
    if ((u64)p1 > (u64)-4096) return -1;
    char *p2 = (char *)(((u64)p1 + HEAP_MAX - 1) & ~(HEAP_MAX - 1));
    if (p2 != p1) sys3(SYS_munmap, p1, p2 - p1, 0);
    u64 tail_at = (u64)p2 + HEAP_MAX, res_end = (u64)p1 + 2 * HEAP_MAX;
    if (res_end > tail_at) sys3(SYS_munmap, tail_at, res_end - tail_at, 0);
    if (sys3(SYS_mprotect, p2, first, PROT_RW) != 0) return -2;
    h->base = p2;
    h->mprotect_size = first;
    return 0;
}
/* grow_heap: открыть ещё `diff` и сразу писать в новый кусок — первый и
 * последний байт и смещение, не кратное странице (как заголовок чанка). */
static int grow_heap(struct heap *h, u64 diff) {
    if (h->mprotect_size + diff > HEAP_MAX) return -1;
    if (sys3(SYS_mprotect, h->base + h->mprotect_size, diff, PROT_RW) != 0) return -2;
    char *n = h->base + h->mprotect_size;
    h->mprotect_size += diff;
    n[0] = 1;
    n[0xd58 % diff] = 2;
    n[diff - 1] = 3;
    return 0;
}
/* shrink_heap, обе ветки glibc. */
static void shrink_heap(struct heap *h, u64 new_size, int mmap_branch) {
    char *end = h->base + new_size;
    u64 diff = h->mprotect_size - new_size;
    if (mmap_branch) {
        sys6(SYS_mmap, (i64)end, diff, PROT_NONE, MAP_PRIVATE | MAP_ANON | MAP_NORESERVE | MAP_FIXED, -1, 0);
        h->mprotect_size = new_size;
    } else {
        sys3(SYS_madvise, end, diff, MADV_DONTNEED);
    }
}

static int grow_to(struct heap *h, u64 target) {
    while (h->mprotect_size < target)
        if (grow_heap(h, 0x21000) != 0) return -1;
    return 0;
}

/* ---- потоки -------------------------------------------------------------- */
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

#define THREADS 8
static char stacks[THREADS][64 * 1024] __attribute__((aligned(16)));
static volatile int started, finished, go, next_id;
static volatile int thread_bad[THREADS];

static void nap_ms(i64 ms) {
    i64 t[2] = {ms / 1000, (ms % 1000) * 1000000L};
    sys3(SYS_nanosleep, t, 0, 0);
}

/* Каждый поток: своя куча, рост до 9 МиБ (через 8 МиБ — граница из лога),
 * усадка до 1 МиБ madvise-ом, снова рост, усадка mmap-ом, снова рост. */
void worker(void) {
    int id = __atomic_fetch_add(&next_id, 1, __ATOMIC_SEQ_CST);
    struct heap h;
    __atomic_fetch_add(&started, 1, __ATOMIC_SEQ_CST);
    while (!go) { }
    if (new_heap(&h, 0x21000, 0, 0) != 0) { thread_bad[id] = 1; goto done; }
    for (int round = 0; round < 6; round++) {
        if (grow_to(&h, 9UL << 20) != 0) { thread_bad[id] = 2; break; }
        shrink_heap(&h, 1UL << 20, round & 1);
        if (round & 1) { /* mmap-ветка уже сбросила mprotect_size */ }
        else h.mprotect_size = 1UL << 20;   /* glibc: h->size, mprotect_size остаётся; для пробы
                                              откроем заново — mprotect по открытому безвреден */
    }
done:
    __atomic_fetch_add(&finished, 1, __ATOMIC_SEQ_CST);
}

/* ---- фаза F: два потока, одна подсказка ------------------------------------
 * Каждый поток раз за разом: mmap(подсказка P, 64 КиБ, RW) без MAP_FIXED,
 * пишет свой номер во все 16 страниц, немного ждёт, проверяет, что номер на
 * месте, снимает. Правильное ядро отдаёт P только одному из тех, кто спросил
 * одновременно, остальным — другое место. С гонкой двое получают P, и
 * второй снимает страницы первого: тот читает ноль или чужой номер. */
#define RACERS 4
#define RACE_ROUNDS 3000
#define RACE_BYTES (64UL * 1024)
static char race_stacks[RACERS][64 * 1024] __attribute__((aligned(16)));
static volatile u64 race_hint;
static volatile int race_started, race_finished, race_go, race_next;
static volatile int race_bad[RACERS];
static volatile u64 race_at[RACERS];

void racer(void) {
    int id = __atomic_fetch_add(&race_next, 1, __ATOMIC_SEQ_CST);
    __atomic_fetch_add(&race_started, 1, __ATOMIC_SEQ_CST);
    while (!race_go) { }
    for (int round = 0; round < RACE_ROUNDS && !race_bad[id]; round++) {
        volatile char *m = (volatile char *)sys6(SYS_mmap, (i64)race_hint, RACE_BYTES, PROT_RW,
                                                 MAP_PRIVATE | MAP_ANON, -1, 0);
        if ((u64)m > (u64)-4096) { race_bad[id] = 1; break; }
        for (u64 off = 0; off < RACE_BYTES; off += PAGE) m[off] = (char)(id + 1);
        for (volatile int spin = 0; spin < 200; spin++) { }
        for (u64 off = 0; off < RACE_BYTES; off += PAGE)
            if (m[off] != (char)(id + 1)) { race_bad[id] = 2; race_at[id] = (u64)m + off; }
        sys3(SYS_munmap, m, RACE_BYTES, 0);
    }
    __atomic_fetch_add(&race_finished, 1, __ATOMIC_SEQ_CST);
}

void start_c(u64 *sp) {
    (void)sp;
    out("heapgrow: start\n");
    u64 act[4] = {(u64)on_segv, SA_SIGINFO | SA_RESTORER, 0, 0};
    __asm__("lea restorer(%%rip), %0" : "=r"(act[2]));
    sys6(SYS_rt_sigaction, SIGSEGV, (i64)act, 0, 8, 0, 0);

    struct heap h;
    phase = "A: new_heap + grow to 9 MiB (single thread)";
    check(new_heap(&h, 0x21000, 0, 0) == 0, "new_heap: reserve 128 MiB, trim to 64 MiB, open 0x21000");
    check(((u64)h.base & (HEAP_MAX - 1)) == 0, "heap base is 64 MiB aligned");
    check(grow_to(&h, 9UL << 20) == 0, "grow 0x21000 at a time to 9 MiB, writing into each new piece");
    /* Именно адрес из лога: смещение 0x800d58 от начала кучи. */
    h.base[0x800d58] = 7;
    check(h.base[0x800d58] == 7, "write at heap+0x800d58 (the faulting offset)");

    phase = "B: hinted second heap right after the first";
    struct heap h2;
    check(new_heap(&h2, 0x21000, 1, h.base + HEAP_MAX) == 0, "new_heap with the aligned_heap_area hint");
    check(grow_to(&h2, 9UL << 20) == 0, "grow the hinted heap to 9 MiB");

    phase = "C: madvise(DONTNEED) shrink, regrow, content is zero";
    h.base[0x400000] = 5;
    shrink_heap(&h, 1UL << 20, 0);
    check(h.base[0x400000] == 0, "after MADV_DONTNEED the page reads zero");
    h.base[0x800d58] = 9;
    check(h.base[0x800d58] == 9, "still writable after MADV_DONTNEED (mprotect_size unchanged)");

    phase = "D: MAP_FIXED PROT_NONE shrink, then mprotect again";
    shrink_heap(&h, 1UL << 20, 1);
    check(grow_to(&h, 9UL << 20) == 0, "regrow after the mmap-branch shrink");

    phase = "E: 8 threads, each with its own heap, grow/shrink in parallel";
    for (int i = 0; i < THREADS; i++)
        raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, stacks[i] + sizeof stacks[i], worker);
    while (started < THREADS) nap_ms(1);
    go = 1;
    for (int i = 0; i < 3000 && finished < THREADS; i++) nap_ms(10);
    check(finished == THREADS, "all threads finished");
    int bad = 0;
    for (int i = 0; i < THREADS; i++) bad += thread_bad[i];
    check(bad == 0, "no thread saw a failed mprotect/new_heap");

    phase = "F: hinted mmap race (several threads, one hint)";
    {
        /* Свободное место: взять и отдать. */
        char *probe = (char *)sys6(SYS_mmap, 0, 16 * RACE_BYTES, PROT_NONE, MAP_PRIVATE | MAP_ANON, -1, 0);
        check((u64)probe < (u64)-4096, "reserve and release a place for the hint");
        sys3(SYS_munmap, probe, 16 * RACE_BYTES, 0);
        race_hint = (u64)probe;
        for (int i = 0; i < RACERS; i++)
            raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000,
                       race_stacks[i] + sizeof race_stacks[i], racer);
        while (race_started < RACERS) nap_ms(1);
        race_go = 1;
        for (int i = 0; i < 6000 && race_finished < RACERS; i++) nap_ms(10);
        check(race_finished == RACERS, "all racers finished");
        int lost = 0, refused = 0;
        for (int i = 0; i < RACERS; i++) {
            if (race_bad[i] == 1) refused++;
            if (race_bad[i] == 2) { lost++; hex("heapgrow: page lost at ", race_at[i]); }
        }
        check(refused == 0, "every hinted mmap succeeded");
        check(lost == 0, "no thread lost its pages to another's hinted mmap");
    }

    out(failed ? "heapgrow: some failed\n" : "heapgrow: 0 failed\n");
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
