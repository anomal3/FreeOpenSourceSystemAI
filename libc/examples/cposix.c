/* Слой POSIX под чужую среду исполнения (фаза 58b).
 *
 * # Что она доказывает
 *
 * Что функции, которых не хватило компоновке Mono, не просто есть, а делают
 * то, что обещает POSIX, на настоящих вызовах ядра: текущий каталог и
 * относительные пути, каталоги с перечислением, канал с ожиданием, пределы,
 * таблица обработчиков сигналов. Каждая проверка устроена так, чтобы заглушка
 * с ответом «успех» её **не** прошла: перечисление обязано найти созданные
 * имена с их видом, канал — вернуть записанное, `rmdir` на файле — отказать.
 *
 * Аргумент — каталог, в котором можно писать (на загрузке с носителя корень
 * только на чтение). Итог — одна строка с числом проваленных.
 */

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/filio.h>
#include <sys/ioctl.h>
#include <sys/resource.h>
#include <sys/select.h>
#include <sys/stat.h>
#include <unistd.h>

static int failures;

static void ok(const char *what) { printf("cposix: ok %s\n", what); }

static void fail(const char *what) {
    printf("cposix: FAILED %s (errno %d: %s)\n", what, errno, strerror(errno));
    failures++;
}

static void check(int passed, const char *what) {
    if (passed) {
        ok(what);
    } else {
        fail(what);
    }
}

/* ── Текущий каталог и относительные пути ────────────────────────────────── */

static void check_cwd(const char *dir) {
    char here[256];
    int started_at_root = getcwd(here, sizeof(here)) != NULL && strcmp(here, "/") == 0;
    int moved = chdir(dir) == 0 && getcwd(here, sizeof(here)) != NULL && strcmp(here, dir) == 0;
    check(started_at_root && moved, "getcwd starts at / and follows chdir");

    /* Файл по относительному имени обязан оказаться в текущем каталоге —
     * проверяется по абсолютному пути, а не тем же относительным. */
    FILE *file = fopen("cposix.txt", "w");
    int wrote = file != NULL && fputs("relative\n", file) >= 0 && fclose(file) == 0;
    char full[300];
    snprintf(full, sizeof(full), "%s/cposix.txt", dir);
    struct stat info;
    check(wrote && stat(full, &info) == 0 && info.st_size == 9, "a relative path lands in the current directory");

    errno = 0;
    check(chdir("cposix.txt") == -1 && errno == ENOTDIR, "chdir into a file is ENOTDIR");
}

/* ── Каталоги ────────────────────────────────────────────────────────────── */

static void check_dirs(const char *dir) {
    int made = mkdir("sub", 0755) == 0;
    int saw_file = 0;
    int saw_dir = 0;
    DIR *list = opendir(".");
    if (list != NULL) {
        struct dirent *entry;
        while ((entry = readdir(list)) != NULL) {
            if (strcmp(entry->d_name, "cposix.txt") == 0 && entry->d_type == DT_REG) {
                saw_file = 1;
            }
            if (strcmp(entry->d_name, "sub") == 0 && entry->d_type == DT_DIR && entry->d_ino != 0) {
                saw_dir = 1;
            }
        }
        closedir(list);
    }
    check(made && saw_file && saw_dir, "mkdir, then readdir lists both names with their kinds");

    errno = 0;
    int refused = rmdir("cposix.txt") == -1 && errno == ENOTDIR;
    struct stat info;
    int removed = rmdir("sub") == 0 && stat("sub", &info) == -1 && errno == ENOENT;
    check(refused && removed, "rmdir refuses a file and removes a directory");

    /* `..` сворачивается и в `chdir`, и в `getcwd`. */
    char here[256];
    char parent[256];
    snprintf(parent, sizeof(parent), "%s", dir);
    char *slash = strrchr(parent, '/');
    if (slash != NULL) {
        *(slash == parent ? slash + 1 : slash) = '\0';
    }
    int up = chdir("..") == 0 && getcwd(here, sizeof(here)) != NULL && strcmp(here, parent) == 0;
    check(up && chdir(dir) == 0, "chdir .. folds the path");

    errno = 0;
    char link[16];
    int no_link = readlink("cposix.txt", link, sizeof(link)) == -1 && errno == EINVAL;
    check(no_link && access("cposix.txt", R_OK | W_OK) == 0 && access("nothing-here", F_OK) == -1,
          "access and readlink answer by stat");
    unlink("cposix.txt");
}

/* ── Канал, ожидание, дескрипторы ────────────────────────────────────────── */

static void check_pipe(void) {
    int ends[2];
    if (pipe(ends) != 0) {
        fail("pipe carries bytes and poll sees them");
        return;
    }
    struct pollfd one = {ends[0], POLLIN, 0};
    int empty = poll(&one, 1, 0) == 0;
    int wrote = write(ends[1], "ping", 4) == 4;
    int ready = poll(&one, 1, 1000) == 1 && (one.revents & POLLIN) != 0;
    int pending = -1;
    int told = ioctl(ends[0], FIONREAD, &pending) == 0 && pending > 0;
    fd_set readable;
    FD_ZERO(&readable);
    FD_SET(ends[0], &readable);
    struct timeval now = {0, 0};
    int selected = select(ends[0] + 1, &readable, NULL, NULL, &now) == 1 && FD_ISSET(ends[0], &readable);
    char got[8] = {0};
    int copy = dup(ends[0]);
    int read_back = copy >= 0 && read(copy, got, sizeof(got)) == 4 && memcmp(got, "ping", 4) == 0;
    check(empty && wrote && ready && told && selected && read_back, "pipe carries bytes and poll sees them");
    check(fcntl(ends[0], F_GETFD) == FD_CLOEXEC, "fcntl says close-on-exec");
    close(copy);
    close(ends[0]);
    close(ends[1]);
}

/* Стандартные потоки открыты, и `fstat` это видит (фаза 60): за ними терминал,
 * то есть символьное устройство. Mono проверяет каждый из трёх потоков
 * `fcntl(fd, F_GETFL)` и без ответа не открывает `Console`. */
static void check_standard(void) {
    struct stat in;
    struct stat out;
    int terminal = fstat(0, &in) == 0 && S_ISCHR(in.st_mode) && fstat(1, &out) == 0 && S_ISCHR(out.st_mode);
    int open_all = fcntl(0, F_GETFL) != -1 && fcntl(1, F_GETFL) != -1 && fcntl(2, F_GETFL) != -1;
    check(terminal && open_all, "fstat and fcntl see the standard streams as a terminal");
}

/* ── Пределы, личность, сигналы ──────────────────────────────────────────── */

static void on_signal(int number) { (void)number; }

static void check_process(void) {
    struct rlimit stack;
    struct rlimit files;
    int limits = getrlimit(RLIMIT_STACK, &stack) == 0 && stack.rlim_cur >= 1024 * 1024 &&
                 getrlimit(RLIMIT_NOFILE, &files) == 0 && files.rlim_cur == 11;
    printf("cposix: stack limit %lu, open files %lu\n", (unsigned long)stack.rlim_cur,
           (unsigned long)files.rlim_cur);
    check(limits, "getrlimit tells the real stack and file limits");

    struct sigaction want;
    struct sigaction was;
    memset(&want, 0, sizeof(want));
    want.sa_handler = on_signal;
    errno = 0;
    int kept = sigaction(SIGUSR1, &want, NULL) == 0 && sigaction(SIGUSR1, NULL, &was) == 0 &&
               was.sa_handler == on_signal;
    int refused = sigaction(SIGKILL, &want, NULL) == -1 && errno == EINVAL;
    check(kept && refused, "sigaction keeps a handler and refuses SIGKILL");

    check(getuid() == geteuid() && getgid() == getegid(), "getuid matches geteuid");
}

int main(int argc, char **argv) {
    const char *dir = argc > 1 ? argv[1] : "/home/roman";
    printf("cposix: starting in %s\n", dir);
    check_cwd(dir);
    check_dirs(dir);
    check_pipe();
    check_standard();
    check_process();
    printf("cposix: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
