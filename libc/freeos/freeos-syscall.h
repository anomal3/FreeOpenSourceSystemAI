/* Системные вызовы FreeOS для программ на C.
 *
 * Единственное место, где номера вызовов выписаны для стороны C. Дублируют они
 * `crates/user-abi/src/lib.rs`, и это дублирование опасное: разъехавшись, две
 * таблицы не дадут ни ошибки сборки, ни отказа во время работы — программа
 * просто позовёт не тот вызов. Поэтому они сверяются: `cargo xtask check`
 * читает этот файл, вынимает каждый `#define SYS_*` и сравнивает с константой
 * того же имени из `user-abi` (тест `c_header_matches_the_abi` в `xtask`).
 * Добавил номер в договор — добавь сюда, иначе проверка покажет, чего не
 * хватает, ещё до первого запуска.
 *
 * Здесь ровно то, что нужно слою ОС под picolibc, и ни номером больше: вызов,
 * выписанный «на будущее», — это обещание, которое никто не проверял.
 */

#ifndef FREEOS_SYSCALL_H
#define FREEOS_SYSCALL_H

#include <stdint.h>

/* ── Номера. Сверяются с `user-abi` тестом, см. заголовок файла ───────────── */

#define SYS_WRITE 1
#define SYS_EXIT 2
#define SYS_YIELD 3
#define SYS_UPTIME 4
#define SYS_OPEN 5
#define SYS_READ 6
#define SYS_CLOSE 7
#define SYS_STAT 8
#define SYS_GETUID 9
#define SYS_GETGID 10
#define SYS_GETPID 11
#define SYS_MKDIR 13
#define SYS_REMOVE 14
#define SYS_SEEK 15
#define SYS_TIME 16
#define SYS_READDIR 17
#define SYS_TTYMODE 19
#define SYS_RENAME 20
#define SYS_SPAWN 21
#define SYS_WAIT 22
#define SYS_CREATE 23
#define SYS_RANDOM 38
#define SYS_PIPE 39
#define SYS_MMAP 42
#define SYS_MUNMAP 43
#define SYS_MMAP_FILE 44
#define SYS_DUP 46
#define SYS_FSTAT 47
#define SYS_ISATTY 48
#define SYS_CLOCK 49
#define SYS_NANOSLEEP 50
#define SYS_POLL 51
#define SYS_TIMES 52
#define SYS_SYSINFO 57
#define SYS_THREAD_CREATE 63
#define SYS_SET_TLS 64
#define SYS_THREAD_EXIT 65
#define SYS_FUTEX_WAIT 66
#define SYS_FUTEX_WAKE 67
#define SYS_MPROTECT 72
#define SYS_THREAD_INFO 73

/* ── Значения, которые эти вызовы принимают и возвращают ──────────────────── */

#define FREEOS_FD_STDIN 0
#define FREEOS_FD_STDOUT 1
#define FREEOS_FD_STDERR 2

#define FREEOS_O_WRITE 1
#define FREEOS_O_CREATE 2
#define FREEOS_O_TRUNC 4

#define FREEOS_SEEK_SET 0
#define FREEOS_SEEK_CUR 1
#define FREEOS_SEEK_END 2

#define FREEOS_KIND_FILE 1
#define FREEOS_KIND_DIRECTORY 2
#define FREEOS_KIND_PIPE 3

/* `кто` у `SYS_SPAWN`: запустить с теми же правами, что у запускающего.
 * Минус единица, а не ноль: ноль — это root, то есть совсем другая просьба. */
#define FREEOS_SPAWN_INHERIT (-1)

/* `куда` у `SYS_DUP`: самый маленький свободный номер. */
#define FREEOS_DUP_ANY (-1)

/* `SYS_POLL`: чего ждём и что готово; срок «сколько угодно». */
#define FREEOS_POLL_IN 1
#define FREEOS_POLL_OUT 2
#define FREEOS_POLL_HUP 4
#define FREEOS_POLL_BAD 8
#define FREEOS_POLL_FOREVER (-1)

/* `SYS_TTYMODE`: строка с эхом или каждое нажатие без эха. */
#define FREEOS_TTY_LINE 0
#define FREEOS_TTY_RAW 1

/* Сколько файлов открыто у программы одновременно, самое большее. */
#define FREEOS_MAX_OPEN_FILES 8

/* Длина имени в записи каталога (`Dirent`), без завершающего нуля. */
#define FREEOS_MAX_NAME 255

/* Флаг `SYS_MMAP`: кадры по первому касанию, а не сразу (фаза 56). */
#define FREEOS_MAP_LAZY 2

/* Ответы `SYS_FUTEX_WAIT`, кроме нуля («разбудили»). */
#define FREEOS_FUTEX_CHANGED 1
#define FREEOS_FUTEX_TIMED_OUT 2

/* Страница процесса и шаблон TLS в ней (фаза 57, `user_abi::TlsTemplate`). */
#define FREEOS_PROCESS_PAGE 0x0000008060c00000UL
#define FREEOS_PROCESS_PAGE_TLS 64

#define FREEOS_CLOCK_REALTIME 0
#define FREEOS_CLOCK_MONOTONIC 1

/* Ошибки. Отрицательные, как их возвращает ядро; переводит их в `errno`
 * `freeos_errno` в `stubs.c`.
 *
 * Нумерация здесь **не** по порядку и не по смыслу: номер, однажды выданный,
 * не меняется, а выдавались они по мере надобности. Выписывать их на память
 * нельзя — я так и сделал, ошибся в четырёх из пяти, и поймала это сверка с
 * `user-abi`, а не сборка. */
#define FREEOS_ERR_NO_SYSCALL (-2)
#define FREEOS_ERR_BAD_ADDRESS (-3)
#define FREEOS_ERR_NOT_FOUND (-4)
#define FREEOS_ERR_PERMISSION (-5)
#define FREEOS_ERR_BAD_FD (-6)
#define FREEOS_ERR_TOO_MANY_FILES (-7)
#define FREEOS_ERR_IO (-8)
#define FREEOS_ERR_UNSUPPORTED (-9)
#define FREEOS_ERR_NO_FILESYSTEM (-10)
#define FREEOS_ERR_BAD_PATH (-11)
#define FREEOS_ERR_NO_PROGRAM (-12)
#define FREEOS_ERR_EXISTS (-14)
#define FREEOS_ERR_NOT_EMPTY (-15)
#define FREEOS_ERR_NO_SPACE (-16)
#define FREEOS_ERR_AGAIN (-17)
#define FREEOS_ERR_BROKEN_PIPE (-22)
#define FREEOS_ERR_LIMIT (-24)

/* Раскладка `Stat` — ответа `SYS_STAT` и `SYS_FSTAT`.
 *
 * Поля и их порядок обязаны совпадать с `user_abi::Stat` до байта; размер и
 * смещения проверяет тот же тест, что и номера. */
struct freeos_stat {
    uint64_t size;
    uint32_t mode;
    uint32_t uid;
    uint32_t gid;
    uint32_t kind;
};

/* Раскладка `Dirent` — ответа `SYS_READDIR` (фаза 58b в libc). */
struct freeos_dirent {
    uint64_t size;
    uint32_t mtime;
    uint32_t mode;
    uint32_t uid;
    uint32_t gid;
    uint32_t kind;
    uint32_t name_len;
    char name[FREEOS_MAX_NAME];
};

/* Раскладка `PollFd` — элемента массива `SYS_POLL`. */
struct freeos_pollfd {
    int64_t fd;
    uint32_t wanted;
    uint32_t ready;
};

/* Раскладка `Timespec` — ответа `SYS_CLOCK` и аргумента `SYS_NANOSLEEP`. */
struct freeos_timespec {
    uint64_t seconds;
    uint32_t nanos;
    uint32_t _pad;
};

/* Раскладка `ThreadInfo` — ответа `SYS_THREAD_INFO` (фаза 58b): номер задачи
 * потока и границы его стека `[stack_low, stack_high)`, сторож не в счёт. */
struct freeos_thread_info {
    uint64_t id;
    uint64_t stack_low;
    uint64_t stack_high;
};

/* ── Ловушка ──────────────────────────────────────────────────────────────── */

/* Три аргумента, а не шесть: столько же, сколько у стороны Rust, и столько же
 * принимает ядро. Вызов, которому нужно больше, кладёт структуру в память и
 * передаёт указатель — так устроены `SYS_LAUNCH`, `SYS_POLL` и окна. */
static inline long freeos_syscall(long number, long a0, long a1, long a2) {
#if defined(__x86_64__)
    long result;
    /* `int 0x80` — ловушка с DPL 3, ядро её ждёт. `rcx`/`r11` в список
     * испорченных не входят: их портит инструкция `syscall`, а не `int`. */
    __asm__ volatile("int $0x80"
                     : "=a"(result)
                     : "a"(number), "D"(a0), "S"(a1), "d"(a2)
                     : "memory", "cc");
    return result;
#elif defined(__aarch64__)
    register long x8 __asm__("x8") = number;
    register long x0 __asm__("x0") = a0;
    register long x1 __asm__("x1") = a1;
    register long x2 __asm__("x2") = a2;
    /* `svc #0` — единственный способ попасть из EL0 в EL1. */
    __asm__ volatile("svc #0" : "+r"(x0) : "r"(x8), "r"(x1), "r"(x2) : "memory", "cc");
    return x0;
#else
#error "FreeOS собирается только под x86-64 и AArch64"
#endif
}

#endif /* FREEOS_SYSCALL_H */
