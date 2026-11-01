/*
 * vDSO FossAi для программ Linux (x86-64, 01.11.2026): время без входа в ядро.
 *
 * Java и .NET спрашивают часы миллионы раз (Rider при запуске — 1,7 млн
 * `clock_gettime`), и системный вызов на каждый — секунды процессора. Linux
 * отвечает из памяти программы: ядро кладёт рядом с этим кодом страницу с
 * основанием счётчика тактов (`vvar_page`), а код читает `rdtsc` и считает
 * время сам. glibc находит функции по `AT_SYSINFO_EHDR` и версии LINUX_2.6.
 *
 * Страница данных пишется ядром под счётчиком-замком (`seq`): нечётный —
 * идёт запись, другой после чтения — читать заново. `valid` ноль — счётчик
 * тактов не годится, и ответ даёт системный вызов, как без vDSO.
 */

typedef unsigned long u64;
typedef long i64;
typedef unsigned int u32;

struct vvar {
    volatile u64 seq;
    u64 valid;
    u64 tsc_base;
    u64 mono_base_ns;
    u64 mult;
    u64 shift;
    i64 real_base_ns;
    u64 real_valid;
};

/* Страница перед кодом vDSO (см. `vdso.lds`): адрес — относительно кода.
 * `volatile` целиком: страницу меняет ядро, и без этого компилятор вправе
 * переставить чтение полей за вторую проверку `seq` (так и было у `const`). */
extern volatile const struct vvar vvar_page __attribute__((visibility("hidden")));

struct timespec {
    i64 tv_sec;
    i64 tv_nsec;
};

struct timeval {
    i64 tv_sec;
    i64 tv_usec;
};

#define CLOCK_REALTIME 0
#define CLOCK_MONOTONIC 1
#define CLOCK_MONOTONIC_RAW 4
#define CLOCK_REALTIME_COARSE 5
#define CLOCK_MONOTONIC_COARSE 6
#define CLOCK_BOOTTIME 7

static inline u64 rdtsc_ordered(void) {
    u32 lo, hi;
    __asm__ volatile("lfence\n\trdtsc" : "=a"(lo), "=d"(hi) : : "memory");
    return ((u64)hi << 32) | lo;
}

static inline long syscall2(long nr, long a, long b) {
    long ret;
    __asm__ volatile("syscall" : "=a"(ret) : "a"(nr), "D"(a), "S"(b) : "rcx", "r11", "memory");
    return ret;
}

/* Монотонное время и время суток в наносекундах. 0 — не из vDSO. */
static int read_time(u64 *mono, i64 *real, int *real_ok) {
    for (;;) {
        u64 seq = vvar_page.seq;
        if (seq & 1) {
            continue;
        }
        __asm__ volatile("" ::: "memory");
        if (!vvar_page.valid) {
            return 0;
        }
        u64 delta = rdtsc_ordered() - vvar_page.tsc_base;
        u64 ns = vvar_page.mono_base_ns + (u64)(((unsigned __int128)delta * vvar_page.mult) >> vvar_page.shift);
        i64 base = vvar_page.real_base_ns;
        int ok = vvar_page.real_valid != 0;
        __asm__ volatile("" ::: "memory");
        if (vvar_page.seq == seq) {
            *mono = ns;
            *real = base + (i64)ns;
            *real_ok = ok;
            return 1;
        }
    }
}

int __vdso_clock_gettime(long clock, struct timespec *ts) {
    u64 mono;
    i64 real;
    int real_ok;
    switch (clock) {
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_MONOTONIC_COARSE:
    case CLOCK_BOOTTIME:
        if (read_time(&mono, &real, &real_ok)) {
            ts->tv_sec = (i64)(mono / 1000000000UL);
            ts->tv_nsec = (i64)(mono % 1000000000UL);
            return 0;
        }
        break;
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_COARSE:
        if (read_time(&mono, &real, &real_ok) && real_ok) {
            ts->tv_sec = real / 1000000000L;
            ts->tv_nsec = real % 1000000000L;
            return 0;
        }
        break;
    default:
        break;
    }
    return (int)syscall2(228, clock, (long)ts);
}

int __vdso_gettimeofday(struct timeval *tv, void *tz) {
    u64 mono;
    i64 real;
    int real_ok;
    if (tz == 0 && read_time(&mono, &real, &real_ok) && real_ok) {
        if (tv) {
            tv->tv_sec = real / 1000000000L;
            tv->tv_usec = (real % 1000000000L) / 1000;
        }
        return 0;
    }
    return (int)syscall2(96, (long)tv, (long)tz);
}

long __vdso_time(long *t) {
    u64 mono;
    i64 real;
    int real_ok;
    long seconds;
    if (read_time(&mono, &real, &real_ok) && real_ok) {
        seconds = real / 1000000000L;
    } else {
        seconds = syscall2(201, 0, 0);
    }
    if (t) {
        *t = seconds;
    }
    return seconds;
}

int clock_gettime(long, struct timespec *) __attribute__((weak, alias("__vdso_clock_gettime")));
int gettimeofday(struct timeval *, void *) __attribute__((weak, alias("__vdso_gettimeofday")));
long time(long *) __attribute__((weak, alias("__vdso_time")));
