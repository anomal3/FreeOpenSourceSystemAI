/* Программа Linux на glibc (фаза 71c): потоки, сигналы, /proc, /dev, каналы.
 *
 * Собирается clang с sysroot из пакетов Debian 12 (`libc6-dev`,
 * `linux-libc-dev`) — обычная программа для Linux, ни строчки FossAi — и едет
 * в пакете `debian-12.fpk` как `/usr/libexec/fossai/linuxcheck`. Каждая
 * проверка — то, на чём стоят большие программы: `pthread_create` и
 * `pthread_join` (clone3 и CLONE_CHILD_CLEARTID), замки и условные переменные
 * (futex с WAIT_BITSET и абсолютным сроком), `__thread` (FS у каждого потока
 * свой), сигналы с обработчиком и `rt_sigreturn`, `sysconf` по `/sys`,
 * границы стека главного потока по `/proc/self/maps`.
 *
 * Строки — `linuxcheck: ok <что>` или `linuxcheck: FAILED <что>`; итог —
 * `linuxcheck: N failed`, код выхода — то же N.
 */

#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <gnu/libc-version.h>
#include <poll.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

static int failed;

static void check(int good, const char *what) {
    printf("linuxcheck: %s %s\n", good ? "ok" : "FAILED", what);
    fflush(stdout);
    if (!good) failed++;
}

static long long now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1000000000LL + t.tv_nsec;
}

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static long counter;
static __thread int own;

static void *count_up(void *arg) {
    own = (int)(long)arg;
    for (int i = 0; i < 100000; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    struct timespec nap = {0, 2000000};
    nanosleep(&nap, NULL);
    return (void *)(long)(own * 10);
}

static volatile sig_atomic_t got_signal;
static volatile int got_code;
static __thread volatile sig_atomic_t thread_signal;

static void on_signal(int sig, siginfo_t *info, void *context) {
    (void)context;
    got_signal = sig;
    got_code = info->si_signo;
    thread_signal = sig;
}

static volatile int waiter_ready;
static void *wait_for_signal(void *arg) {
    (void)arg;
    waiter_ready = 1;
    for (int i = 0; i < 500 && !thread_signal; i++) {
        struct timespec nap = {0, 2000000};
        nanosleep(&nap, NULL);
    }
    return (void *)(long)thread_signal;
}

int main(int argc, char **argv) {
    (void)argc;
    printf("linuxcheck: %s, glibc %s\n", argv[0], gnu_get_libc_version());

    /* Потоки и замок: неатомарный счётчик сходится только под замком. */
    pthread_t threads[4];
    int created = 1;
    for (long i = 0; i < 4; i++) {
        created &= pthread_create(&threads[i], NULL, count_up, (void *)(i + 1)) == 0;
    }
    long sum = 0;
    for (int i = 0; i < 4; i++) {
        void *result = NULL;
        if (created) pthread_join(threads[i], &result);
        sum += (long)result;
    }
    check(created && counter == 400000, "four threads and a mutex");
    check(sum == 100 && own == 0, "__thread is per thread, join returns the value");

    /* Условная переменная со сроком по монотонным часам. */
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_t cond;
    pthread_cond_init(&cond, &attr);
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_nsec += 50000000;
    if (deadline.tv_nsec >= 1000000000) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000;
    }
    long long before = now_ns();
    pthread_mutex_lock(&lock);
    int waited = pthread_cond_timedwait(&cond, &lock, &deadline);
    pthread_mutex_unlock(&lock);
    check(waited == ETIMEDOUT && now_ns() - before >= 50000000, "timed wait ends at its deadline");

    long cpus = sysconf(_SC_NPROCESSORS_ONLN);
    printf("linuxcheck: %ld processor(s)\n", cpus);
    check(cpus >= 1, "sysconf counts processors");

    pthread_attr_t self_attr;
    void *stack_addr = NULL;
    size_t stack_size = 0;
    int got_attr = pthread_getattr_np(pthread_self(), &self_attr) == 0 &&
                   pthread_attr_getstack(&self_attr, &stack_addr, &stack_size) == 0;
    char *here = (char *)&self_attr;
    check(got_attr && here > (char *)stack_addr && here < (char *)stack_addr + stack_size,
          "main thread stack bounds from /proc/self/maps");

    /* Сигналы: себе, заблокированный и снятый, соседнему потоку. */
    struct sigaction action;
    memset(&action, 0, sizeof action);
    action.sa_sigaction = on_signal;
    action.sa_flags = SA_SIGINFO;
    sigaction(SIGUSR1, &action, NULL);
    sigaction(SIGUSR2, &action, NULL);
    raise(SIGUSR1);
    check(got_signal == SIGUSR1 && got_code == SIGUSR1, "raise runs the handler");

    sigset_t block;
    sigemptyset(&block);
    sigaddset(&block, SIGUSR2);
    got_signal = 0;
    sigprocmask(SIG_BLOCK, &block, NULL);
    raise(SIGUSR2);
    int held = got_signal == 0;
    sigprocmask(SIG_UNBLOCK, &block, NULL);
    check(held && got_signal == SIGUSR2, "a blocked signal waits for the unblock");

    pthread_t waiter;
    pthread_create(&waiter, NULL, wait_for_signal, NULL);
    while (!waiter_ready) sched_yield();
    pthread_kill(waiter, SIGUSR1);
    void *seen = NULL;
    pthread_join(waiter, &seen);
    check((long)seen == SIGUSR1, "pthread_kill reaches the other thread");

    /* Устройства. */
    int null_fd = open("/dev/null", O_WRONLY);
    int zero_fd = open("/dev/zero", O_RDONLY);
    int random_fd = open("/dev/urandom", O_RDONLY);
    char zeros[16] = {1}, noise[16] = {0};
    int devices = null_fd >= 0 && write(null_fd, "x", 1) == 1 && zero_fd >= 0 && read(zero_fd, zeros, 16) == 16 &&
                  random_fd >= 0 && read(random_fd, noise, 16) == 16;
    int all_zero = 1, any_noise = 0;
    for (int i = 0; i < 16; i++) {
        all_zero &= zeros[i] == 0;
        any_noise |= noise[i];
    }
    check(devices && all_zero && any_noise, "/dev/null, /dev/zero, /dev/urandom");

    /* /proc. */
    FILE *maps = fopen("/proc/self/maps", "r");
    char line[256];
    int stack_line = 0;
    while (maps && fgets(line, sizeof line, maps)) stack_line |= strstr(line, "[stack]") != NULL;
    if (maps) fclose(maps);
    FILE *cpuinfo = fopen("/proc/cpuinfo", "r");
    int processor = 0;
    while (cpuinfo && fgets(line, sizeof line, cpuinfo)) processor |= strncmp(line, "processor", 9) == 0;
    if (cpuinfo) fclose(cpuinfo);
    check(stack_line && processor, "/proc/self/maps and /proc/cpuinfo");

    /* Канал и poll. */
    int pipe_fds[2];
    int piped = pipe(pipe_fds) == 0 && write(pipe_fds[1], "abc", 3) == 3;
    struct pollfd watch = {pipe_fds[0], POLLIN, 0};
    char got[4] = {0};
    int polled = piped && poll(&watch, 1, 1000) == 1 && (watch.revents & POLLIN) && read(pipe_fds[0], got, 3) == 3;
    check(polled && strcmp(got, "abc") == 0, "pipe and poll");

    /* Каталоги: перечисление, путь от дескриптора, текущий каталог. */
    /* Debian 12 без usrmerge: libc.so.6 — в /lib, не в /usr/lib. */
    DIR *dir = opendir("/lib");
    int entries = 0;
    struct dirent *entry;
    while (dir && (entry = readdir(dir)) != NULL) entries++;
    int at_fd = dir ? openat(dirfd(dir), "x86_64-linux-gnu", O_RDONLY | O_DIRECTORY) : -1;
    struct stat info;
    int at_stat = at_fd >= 0 && fstatat(at_fd, "libc.so.6", &info, 0) == 0 && S_ISREG(info.st_mode);
    if (dir) closedir(dir);
    check(entries > 0 && at_stat, "readdir, openat and fstatat from a directory descriptor");
    char cwd[64] = {0};
    int moved = chdir("/lib") == 0 && getcwd(cwd, sizeof cwd) != NULL && strcmp(cwd, "/lib") == 0 &&
                access("x86_64-linux-gnu/libc.so.6", R_OK) == 0;
    check(moved, "chdir, getcwd and a relative path");

    /* Большой блок — mmap glibc. */
    char *big = malloc(64 << 20);
    int big_ok = big != NULL;
    if (big_ok) {
        big[0] = 1;
        big[(64 << 20) - 1] = 2;
        free(big);
    }
    check(big_ok, "malloc of 64 MiB");

    printf("linuxcheck: %d failed\n", failed);
    return failed;
}
