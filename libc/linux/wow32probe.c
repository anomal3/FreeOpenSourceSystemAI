/* Программа Linux x86-64 без libc (12.11.2026, шаг 4 плана Proton): 32-битный
 * код в 64-битном процессе — как Wine в режиме WoW64.
 *
 * `set_thread_area` через `int 0x80` (номер i386 243, `user_desc` ниже 4 ГиБ)
 * заводит запись TLS с базой на 32-битные данные; дальний возврат уходит в
 * 32-битный код (CS `0x23`), тот грузит селектор в `FS` и долго читает
 * `fs:[0]`, пока три потока крутятся в 64-битном коде (процессора на всех не
 * хватает — переключения задач идут посреди 32-битного кода). Назад —
 * дальним возвратом в 64-битный код (`0x33`).
 *
 * Строки — `wow32: ok <что>` / `wow32: FAILED <что>`, итог — `wow32: N failed`.
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

enum { SYS_write = 1, SYS_mmap = 9, SYS_mprotect = 10, SYS_nanosleep = 35, SYS_exit_group = 231 };

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

/* Системный вызов i386: номер в eax, аргумент в ebx. */
static i64 int80(u32 number, u32 arg) {
    i64 ret;
    __asm__ volatile("push %%rbx\n\t"
                     "mov %2, %%ebx\n\t"
                     "int $0x80\n\t"
                     "pop %%rbx"
                     : "=a"(ret)
                     : "a"((u64)number), "r"(arg)
                     : "memory");
    return (i64)(int)ret;
}

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
    const char *head = good ? "wow32: ok " : "wow32: FAILED ";
    u64 len = 0;
    for (const char *s = head; *s && len < sizeof line - 2; s++) line[len++] = *s;
    for (const char *s = what; *s && len < sizeof line - 2; s++) line[len++] = *s;
    line[len++] = '\n';
    sys3(SYS_write, 1, line, len);
    if (!failed && !good) failed = 1;
    else if (!good) failed++;
}

/* Низкая память: 0x30000000 — описатель, сохранённые rsp и адрес возврата,
 * итог, стек 32-битного кода; +0x1000 — данные под FS; +0x2000 — код. */
#define LOW 0x30000000UL

/* 32-битный код (с `LOW + 0x2000`): грузит FS, 0x04000000 раз читает
 * fs:[0] и сверяет с 0x12345678, пишет итог (1 — сошлось, 2 — нет) в
 * LOW+0x110, уходит в 64-битный код по LOW+0x2100. Байт 2–3 — селектор. */
static const unsigned char code32[] = {
    0x66, 0xB8, 0x00, 0x00,             /* mov ax, SEL            */
    0x8E, 0xE0,                         /* mov fs, ax             */
    0xB9, 0x00, 0x00, 0x00, 0x04,       /* mov ecx, 0x04000000    */
    0x64, 0xA1, 0x00, 0x00, 0x00, 0x00, /* L: mov eax, fs:[0]     */
    0x3D, 0x78, 0x56, 0x34, 0x12,       /* cmp eax, 0x12345678    */
    0x75, 0x0F,                         /* jne bad                */
    0x49,                               /* dec ecx                */
    0x75, 0xF0,                         /* jnz L                  */
    0xC7, 0x05, 0x10, 0x01, 0x00, 0x30, 0x01, 0x00, 0x00, 0x00, /* mov [LOW+0x110], 1 */
    0xEB, 0x0A,                         /* jmp out                */
    0xC7, 0x05, 0x10, 0x01, 0x00, 0x30, 0x02, 0x00, 0x00, 0x00, /* bad: mov [LOW+0x110], 2 */
    0x6A, 0x33,                         /* out: push 0x33         */
    0x68, 0x00, 0x21, 0x00, 0x30,       /* push LOW+0x2100        */
    0xCB,                               /* retf                   */
};

/* 64-битный код (с `LOW + 0x2100`): вернуть rsp и уйти по сохранённому
 * адресу. */
static const unsigned char back64[] = {
    0x48, 0x8B, 0x24, 0x25, 0x00, 0x01, 0x00, 0x30, /* mov rsp, [LOW+0x100] */
    0xFF, 0x24, 0x25, 0x08, 0x01, 0x00, 0x30,       /* jmp [LOW+0x108]      */
};

/* Уйти в 32-битный код и вернуться: сохраняемые регистры — на 64-битный
 * стек, rsp и адрес возврата — в низкую память. */
void run32(void);
__asm__(".text\n"
        ".globl run32\n"
        "run32:\n"
        "  push %rbx\n"
        "  push %rbp\n"
        "  push %r12\n"
        "  push %r13\n"
        "  push %r14\n"
        "  push %r15\n"
        "  movq %rsp, 0x30000100\n"
        "  lea 1f(%rip), %rax\n"
        "  movq %rax, 0x30000108\n"
        "  movq $0x30000f00, %rsp\n"
        "  pushq $0x23\n"
        "  pushq $0x30002000\n"
        "  lretq\n"
        "1:\n"
        "  pop %r15\n"
        "  pop %r14\n"
        "  pop %r13\n"
        "  pop %r12\n"
        "  pop %rbp\n"
        "  pop %rbx\n"
        "  ret\n");

static volatile int stop_spinners;
void spinner(void) {
    while (!stop_spinners) {
    }
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

static char stacks[3][32 * 1024] __attribute__((aligned(16)));

void start_c(u64 *sp) {
    (void)sp;
    out("wow32: start\n");
    i64 m = sys6(SYS_mmap, LOW, 0x3000, 3, 0x32 /* PRIVATE|ANONYMOUS|FIXED */, -1, 0);
    check(m == (i64)LOW, "low memory at 0x30000000");
    if (m != (i64)LOW) goto done;
    volatile u32 *low = (volatile u32 *)LOW;
    *(volatile u32 *)(LOW + 0x1000) = 0x12345678;

    /* user_desc: номер −1 (выбрать), база, предел, seg_32bit | useable. */
    low[0] = 0xFFFFFFFF;
    low[1] = (u32)(LOW + 0x1000);
    low[2] = 0xFFF;
    low[3] = 0x41;
    i64 r = int80(243, (u32)LOW);
    u32 entry = low[0];
    check(r == 0 && entry >= 12 && entry <= 14, "set_thread_area through int 0x80 picks a TLS entry");
    if (r != 0) goto done;

    low[0] = entry;
    low[1] = low[2] = low[3] = 0;
    r = int80(244, (u32)LOW);
    check(r == 0 && low[1] == (u32)(LOW + 0x1000) && (low[3] & 1), "get_thread_area returns the base back");

    unsigned char code[sizeof code32];
    memcpy(code, code32, sizeof code);
    u32 selector = entry * 8 + 3;
    code[2] = (unsigned char)selector;
    code[3] = (unsigned char)(selector >> 8);
    memcpy((void *)(LOW + 0x2000), code, sizeof code);
    memcpy((void *)(LOW + 0x2100), back64, sizeof back64);
    check(sys3(SYS_mprotect, LOW + 0x2000, 0x1000, 5 /* READ|EXEC */) == 0, "the code page made executable");

    /* Три потока в 64-битном коде: переключения посреди 32-битного. */
    int spinners = 0;
    for (int i = 0; i < 3; i++) {
        if (raw_thread(0x100 | 0x200 | 0x400 | 0x800 | 0x10000 | 0x40000, stacks[i] + sizeof stacks[i], spinner) > 0) spinners++;
    }
    check(spinners == 3, "three 64-bit spinner threads");

    *(volatile u32 *)(LOW + 0x110) = 0;
    run32();
    stop_spinners = 1;
    u32 result = *(volatile u32 *)(LOW + 0x110);
    check(result == 1, "32-bit code read fs:[0] 64M times across task switches");
    check(result != 2, "fs:[0] never read a wrong base");
    out("wow32: back in 64-bit code\n");

done:
    out(failed ? "wow32: some failed\n" : "wow32: 0 failed\n");
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
