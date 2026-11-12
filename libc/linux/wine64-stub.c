/* Переходник `lib/wine/i386-unix/wine64` пакета Proton (12.11.2026).
 *
 * ntdll GE-Proton запускает новый 64-битный процесс (`wineboot`, программу из
 * программы) загрузчиком `i386-unix/wine64`; 32-битной части Linux в пакете
 * нет. Эта программа отдаёт запуск 64-битному загрузчику с теми же
 * аргументами и окружением. Без libc и без загрузчика: ей не нужна ни одна
 * glibc, а `LD_LIBRARY_PATH` с glibc 2.41 уронил бы любую программу Debian 12.
 */

typedef unsigned long u64;
typedef long i64;

__asm__(".section .note.ABI-tag, \"a\", @note\n"
        ".balign 4\n"
        ".long 4, 16, 1\n"
        ".asciz \"GNU\"\n"
        ".long 0, 3, 2, 0\n"
        ".text\n");

static i64 sys3(i64 n, i64 a, i64 b, i64 c) {
    i64 ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(n), "D"(a), "S"(b), "d"(c) : "rcx", "r11", "memory");
    return ret;
}

void start_c(u64 *sp) {
    u64 argc = sp[0];
    char **argv = (char **)(sp + 1);
    char **envp = argv + argc + 1;
    const char *loader;
    __asm__("lea loader_path(%%rip), %0" : "=r"(loader));
    argv[0] = (char *)loader;
    sys3(59, (i64)loader, (i64)argv, (i64)envp);
    static const char fail[] = "wine64: cannot start /opt/proton/lib/wine/x86_64-unix/wine\n";
    const char *text;
    __asm__("lea %1, %0" : "=r"(text) : "m"(fail));
    sys3(1, 2, (i64)text, sizeof fail - 1);
    sys3(231, 127, 0, 0);
    for (;;) {
    }
}

__asm__(".section .rodata\n"
        "loader_path: .asciz \"/opt/proton/lib/wine/x86_64-unix/wine\"\n"
        ".text\n"
        ".globl _start\n"
        "_start:\n"
        "  xor %rbp, %rbp\n"
        "  mov %rsp, %rdi\n"
        "  and $-16, %rsp\n"
        "  call start_c\n"
        "  hlt\n");
