/* Каталоги, текущий каталог и дескрипторы для программ на C (фаза 58b).
 *
 * Собрано по списку неразрешённых имён, который дала компоновка Mono нашим
 * набором, а не по памяти о POSIX. Каждая функция здесь — либо перевод на
 * вызов ядра, либо честный отказ с объяснением; правило то же, что в
 * `syscalls.c`: нереализованное возвращает ошибку, а не притворяется успехом.
 *
 * # Текущий каталог живёт здесь, а не в ядре
 *
 * У ядра текущего каталога нет: каждый вызов с путём получает путь целиком.
 * Программе на C он нужен — `getcwd`, `chdir` и пути без ведущей косой черты.
 * Поэтому он хранится в libc, и **каждая** функция с путём проводит путь через
 * `freeos_path`: относительный приклеивается к текущему каталогу, `.` и `..`
 * сворачиваются. Абсолютный уходит в ядро как есть — так, как ходил до этой
 * фазы. Начальный каталог — корень: запускающий свой каталог не передаёт, у
 * оболочки его тоже нет.
 *
 * `..` сворачивается по тексту пути, а не по диску: `a/../b` — это `b`, даже
 * если `a` нет. POSIX проходит путь по элементам и на несуществующем `a`
 * ответил бы `ENOENT`. Разница видна только на таком пути: символьных ссылок,
 * из-за которых текстовое сворачивание уводило бы не туда, в системе нет.
 */

#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdarg.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <sys/time.h>
#include <unistd.h>

#include "freeos-internal.h"
#include "freeos-syscall.h"

/* ── Текущий каталог ─────────────────────────────────────────────────────── */

static char cwd[FREEOS_PATH_LIMIT + 1] = "/";

/* Дописать к `out` (длина `*len`) один элемент пути, свернув `.` и `..`. */
static int push_part(char *out, size_t *len, const char *part, size_t part_len) {
    if (part_len == 0 || (part_len == 1 && part[0] == '.')) {
        return 0;
    }
    if (part_len == 2 && part[0] == '.' && part[1] == '.') {
        /* Выше корня не подняться — как и везде: `/..` это `/`. */
        while (*len > 1 && out[*len - 1] != '/') {
            (*len)--;
        }
        if (*len > 1) {
            (*len)--;
        }
        out[*len] = '\0';
        return 0;
    }
    size_t need = *len + (*len > 1 ? 1 : 0) + part_len;
    if (need > FREEOS_PATH_LIMIT) {
        errno = ENAMETOOLONG;
        return -1;
    }
    if (*len > 1) {
        out[(*len)++] = '/';
    }
    memcpy(out + *len, part, part_len);
    *len += part_len;
    out[*len] = '\0';
    return 0;
}

/* Разложить `path` по элементам и дописать их к `out`. */
static int push_path(char *out, size_t *len, const char *path) {
    while (*path != '\0') {
        while (*path == '/') {
            path++;
        }
        const char *end = path;
        while (*end != '\0' && *end != '/') {
            end++;
        }
        if (push_part(out, len, path, (size_t)(end - path)) != 0) {
            return -1;
        }
        path = end;
    }
    return 0;
}

const char *freeos_path(const char *path, char *buf) {
    if (path == NULL) {
        errno = EFAULT;
        return NULL;
    }
    if (path[0] == '\0') {
        /* POSIX: пустой путь — `ENOENT`, а не текущий каталог. */
        errno = ENOENT;
        return NULL;
    }
    if (path[0] == '/') {
        return path;
    }
    size_t len = strlen(cwd);
    memcpy(buf, cwd, len + 1);
    return push_path(buf, &len, path) == 0 ? buf : NULL;
}

char *getcwd(char *buf, size_t size) {
    size_t len = strlen(cwd);
    if (buf == NULL) {
        /* Расширение glibc и BSD: память выделяет сама `getcwd`. Mono и
         * многие другие зовут её именно так. */
        size = size == 0 ? len + 1 : size;
        buf = malloc(size);
        if (buf == NULL) {
            errno = ENOMEM;
            return NULL;
        }
    }
    if (size == 0) {
        errno = EINVAL;
        return NULL;
    }
    if (len + 1 > size) {
        errno = ERANGE;
        return NULL;
    }
    memcpy(buf, cwd, len + 1);
    return buf;
}

int chdir(const char *path) {
    char buf[FREEOS_PATH_LIMIT + 1];
    char next[FREEOS_PATH_LIMIT + 1] = "/";
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    /* Абсолютный путь тоже сворачивается: текущий каталог хранится в одном
     * виде, иначе `getcwd` вернула бы `/a/./b/..`. */
    size_t len = 1;
    if (push_path(next, &len, full) != 0) {
        return -1;
    }
    struct stat info;
    if (stat(next, &info) != 0) {
        return -1;
    }
    if (!S_ISDIR(info.st_mode)) {
        errno = ENOTDIR;
        return -1;
    }
    memcpy(cwd, next, len + 1);
    return 0;
}

/* ── Каталоги ────────────────────────────────────────────────────────────── */

int mkdir(const char *path, mode_t mode) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    long code = freeos_syscall(SYS_MKDIR, (long)full, (long)strlen(full), (long)(mode & 07777));
    return code < 0 ? freeos_set_errno(code) : 0;
}

/* У ядра одно удаление на файл и пустой каталог (`SYS_REMOVE`); у POSIX их
 * два, и `rmdir` на файле обязана отказать. Проверка — до вызова. */
int rmdir(const char *path) {
    struct stat info;
    if (stat(path, &info) != 0) {
        return -1;
    }
    if (!S_ISDIR(info.st_mode)) {
        errno = ENOTDIR;
        return -1;
    }
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    long code = freeos_syscall(SYS_REMOVE, (long)full, (long)strlen(full), 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

static void dirfd_remember(int fd, const char *path);

/* Каталог — дескриптор, открытый `SYS_OPEN`, из которого `SYS_READDIR` отдаёт
 * по записи. Список фиксируется при открытии (так обещает ядро), поэтому
 * `rewinddir` — это повторное открытие, а не сдвиг позиции. */
DIR *opendir(const char *path) {
    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        return NULL;
    }
    struct stat info;
    if (fstat(fd, &info) != 0 || !S_ISDIR(info.st_mode)) {
        close(fd);
        errno = ENOTDIR;
        return NULL;
    }
    DIR *dir = calloc(1, sizeof(*dir));
    if (dir == NULL) {
        close(fd);
        errno = ENOMEM;
        return NULL;
    }
    dir->fd = fd;
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full != NULL) {
        dirfd_remember(fd, full);
    }
    return dir;
}

/* `d_ino` у нас номер записи по порядку, начиная с единицы, а не номер узла:
 * ядро номеров узлов не отдаёт, а ноль в `d_ino` старый код BSD читает как
 * «пустая запись» и пропускает. Сравнивать `d_ino` двух каталогов бессмысленно
 * — это и названо здесь. */
struct dirent *readdir(DIR *dir) {
    struct freeos_dirent entry;
    long code = freeos_syscall(SYS_READDIR, dir->fd, (long)&entry, (long)sizeof(entry));
    if (code < 0) {
        freeos_set_errno(code);
        return NULL;
    }
    if (code == 0) {
        /* Конец каталога — `NULL` без смены `errno`, так отличают его от
         * ошибки. */
        return NULL;
    }
    size_t len = entry.name_len < FREEOS_MAX_NAME ? entry.name_len : FREEOS_MAX_NAME;
    memcpy(dir->dirent.d_name, entry.name, len);
    dir->dirent.d_name[len] = '\0';
    dir->dirent.d_type = entry.kind == FREEOS_KIND_DIRECTORY ? DT_DIR
                         : entry.kind == FREEOS_KIND_PIPE    ? DT_FIFO
                                                             : DT_REG;
    dir->count++;
    dir->dirent.d_ino = (ino_t)dir->count;
    return &dir->dirent;
}

int closedir(DIR *dir) {
    int code = close(dir->fd);
    free(dir);
    return code;
}

int dirfd(DIR *dir) { return dir->fd; }

/* ── Пути от каталога: `openat` и соседи (фаза 70d) ─────────────────────────
 *
 * Их зовёт `std::filesystem` из libc++ (`remove_all` идёт по дереву через
 * `openat`, `fdopendir` и `unlinkat`). У ядра нет пути «от дескриптора» —
 * каждый вызов получает путь целиком. Поэтому libc помнит, какой путь открыт
 * под каким дескриптором каталога: `openat` и `opendir` записывают, `close`
 * стирает. Дескриптор, которого в таблице нет (каталог открыт голым `open`,
 * или таблица полна), — `EBADF`: честнее, чем угадывать путь. */

#define DIRFD_SLOTS 64

static struct {
    int fd;
    int used; /* таблица в .bss: ноль — свободно */
    char path[FREEOS_PATH_LIMIT + 1];
} dirfds[DIRFD_SLOTS];

static void dirfd_remember(int fd, const char *path) {
    for (int index = 0; index < DIRFD_SLOTS; index++) {
        if (!dirfds[index].used) {
            dirfds[index].used = 1;
            dirfds[index].fd = fd;
            strncpy(dirfds[index].path, path, FREEOS_PATH_LIMIT);
            dirfds[index].path[FREEOS_PATH_LIMIT] = '\0';
            return;
        }
    }
}

void freeos_dirfd_forget(int fd) {
    for (int index = 0; index < DIRFD_SLOTS; index++) {
        if (dirfds[index].used && dirfds[index].fd == fd) {
            dirfds[index].used = 0;
        }
    }
}

/* Путь `path` от каталога `dir`: абсолютный и `AT_FDCWD` — как у всех
 * функций с путём, иначе — к пути, под которым `dir` был открыт. */
static const char *at_path(int dir, const char *path, char *buf) {
    if (path == NULL || path[0] == '\0' || path[0] == '/' || dir == AT_FDCWD) {
        return freeos_path(path, buf);
    }
    for (int index = 0; index < DIRFD_SLOTS; index++) {
        if (dirfds[index].used && dirfds[index].fd == dir) {
            size_t len = strlen(dirfds[index].path);
            memcpy(buf, dirfds[index].path, len + 1);
            return push_path(buf, &len, path) == 0 ? buf : NULL;
        }
    }
    errno = EBADF;
    return NULL;
}

int openat(int dir, const char *path, int flags, ...) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = at_path(dir, path, buf);
    if (full == NULL) {
        return -1;
    }
    int fd = open(full, flags & ~O_DIRECTORY);
    if (fd < 0) {
        return -1;
    }
    struct stat info;
    if (fstat(fd, &info) == 0 && S_ISDIR(info.st_mode)) {
        dirfd_remember(fd, full);
    } else if (flags & O_DIRECTORY) {
        close(fd);
        errno = ENOTDIR;
        return -1;
    }
    return fd;
}

DIR *fdopendir(int fd) {
    struct stat info;
    if (fstat(fd, &info) != 0) {
        return NULL;
    }
    if (!S_ISDIR(info.st_mode)) {
        errno = ENOTDIR;
        return NULL;
    }
    DIR *dir = calloc(1, sizeof(*dir));
    if (dir == NULL) {
        errno = ENOMEM;
        return NULL;
    }
    dir->fd = fd;
    return dir;
}

int unlinkat(int dir, const char *path, int flags) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = at_path(dir, path, buf);
    if (full == NULL) {
        return -1;
    }
    return (flags & AT_REMOVEDIR) ? rmdir(full) : unlink(full);
}

int fchmodat(int dir, const char *path, mode_t mode, int flags) {
    (void)flags;
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = at_path(dir, path, buf);
    return full == NULL ? -1 : chmod(full, mode);
}

/* Обрезать файл ядро умеет только до нуля — открытием с `O_TRUNC`; любая
 * другая длина — `ENOSYS`, как у `ftruncate`. */
int truncate(const char *path, off_t length) {
    if (length != 0) {
        errno = ENOSYS;
        return -1;
    }
    int fd = open(path, O_WRONLY | O_TRUNC);
    if (fd < 0) {
        return -1;
    }
    return close(fd);
}

/* Сведений о свободном месте на томе ядро не отдаёт: вызова нет. */
int statvfs(const char *path, struct statvfs *out) {
    (void)path;
    (void)out;
    errno = ENOSYS;
    return -1;
}

int fstatvfs(int fd, struct statvfs *out) {
    (void)fd;
    (void)out;
    errno = ENOSYS;
    return -1;
}

/* ── Пути, которых у нас нет ─────────────────────────────────────────────── */

/* Проверка прав по `stat`: владелец, группа, остальные — как у ядра. root
 * проходит чтение и запись всегда, исполнение — если хоть у кого-то есть бит
 * `x`; так же отвечает Linux. */
int access(const char *path, int mode) {
    struct stat info;
    if (stat(path, &info) != 0) {
        return -1;
    }
    if (mode == F_OK) {
        return 0;
    }
    uid_t uid = getuid();
    unsigned int bits;
    if (uid == 0) {
        bits = 06 | ((info.st_mode & 0111) != 0 ? 01 : 0);
    } else if (info.st_uid == uid) {
        bits = (info.st_mode >> 6) & 07;
    } else if (info.st_gid == getgid()) {
        bits = (info.st_mode >> 3) & 07;
    } else {
        bits = info.st_mode & 07;
    }
    unsigned int want = ((mode & R_OK) ? 04 : 0) | ((mode & W_OK) ? 02 : 0) | ((mode & X_OK) ? 01 : 0);
    if ((bits & want) != want) {
        errno = EACCES;
        return -1;
    }
    return 0;
}

/* Символьных ссылок в системе нет: ни одна из трёх файловых систем их не
 * создаёт. Значит, любой существующий путь — «не ссылка», и POSIX велит
 * ответить на это `EINVAL`. */
ssize_t readlink(const char *path, char *buf, size_t size) {
    (void)buf;
    (void)size;
    struct stat info;
    if (stat(path, &info) != 0) {
        return -1;
    }
    errno = EINVAL;
    return -1;
}

int symlink(const char *target, const char *path) {
    (void)target;
    (void)path;
    errno = ENOSYS;
    return -1;
}

int lstat(const char *path, struct stat *out) {
    /* Ссылок нет — `lstat` и `stat` видят одно и то же. */
    return stat(path, out);
}

/* Обрезать открытый файл ядро не умеет: укоротить можно только открытием с
 * `O_TRUNC`, и только до нуля. */
int ftruncate(int fd, off_t length) {
    (void)fd;
    (void)length;
    errno = ENOSYS;
    return -1;
}

/* Кеша блоков у ядра нет — запись уходит на носитель до возврата из `write`.
 * Поэтому `fsync` проверяет дескриптор и отвечает успехом: сбрасывать нечего. */
int fsync(int fd) {
    struct stat info;
    return fstat(fd, &info);
}

int fdatasync(int fd) { return fsync(fd); }

/* По той же причине `sync` — пустое действие: всё записанное уже на носителе. */
void sync(void) {}

/* `realpath` — полный путь без `.` и `..` (фаза 60). Свёртка лексическая, та
 * же, что у `chdir`: символьных ссылок в системе нет, и путь по тексту — это и
 * есть путь по диску. Как требует POSIX, файл обязан существовать. */
char *realpath(const char *path, char *resolved) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return NULL;
    }
    struct stat info;
    if (stat(full, &info) != 0) {
        return NULL;
    }
    if (resolved == NULL) {
        resolved = malloc(PATH_MAX);
        if (resolved == NULL) {
            errno = ENOMEM;
            return NULL;
        }
    }
    strncpy(resolved, full, PATH_MAX - 1);
    resolved[PATH_MAX - 1] = '\0';
    return resolved;
}

/* Права, время и жёсткие ссылки файла ядро пока менять не умеет: у него нет
 * таких вызовов. Отказ `ENOSYS`, а не успех, — программа, поставившая права
 * «только владельцу», обязана узнать, что они остались прежними. */
int chmod(const char *path, mode_t mode) {
    (void)path;
    (void)mode;
    errno = ENOSYS;
    return -1;
}

int fchmod(int fd, mode_t mode) {
    (void)fd;
    (void)mode;
    errno = ENOSYS;
    return -1;
}

int utimes(const char *path, const struct timeval times[2]) {
    (void)path;
    (void)times;
    errno = ENOSYS;
    return -1;
}

int link(const char *target, const char *path) {
    (void)target;
    (void)path;
    errno = ENOSYS;
    return -1;
}

/* Блокировок файлов у ядра нет — как и у `fcntl` (`F_SETLK`). */
int flock(int fd, int operation) {
    (void)fd;
    (void)operation;
    errno = ENOSYS;
    return -1;
}

/* ── Дескрипторы ─────────────────────────────────────────────────────────── */

int dup(int fd) {
    long copy = freeos_syscall(SYS_DUP, fd, FREEOS_DUP_ANY, 0);
    return copy < 0 ? freeos_set_errno(copy) : (int)copy;
}

int dup2(int fd, int to) {
    if (fd == to) {
        struct stat info;
        return fstat(fd, &info) == 0 ? to : -1;
    }
    long copy = freeos_syscall(SYS_DUP, fd, to, 0);
    return copy < 0 ? freeos_set_errno(copy) : (int)copy;
}

/* Канал ядра рождается неблокирующим (так `SYS_PIPE` работает для программ
 * на Rust); канал Unix — ждущим. Режим ставит `SYS_FDCTL` (фаза 66b): ждёт
 * ядро вне замка программы, будят его изменения канала. */
static int pipe_with_mode(int fds[2], int nonblock) {
    long packed = freeos_syscall(SYS_PIPE, 0, 0, 0);
    if (packed < 0) {
        return freeos_set_errno(packed);
    }
    fds[0] = (int)(packed >> 32);
    fds[1] = (int)(packed & 0xffffffff);
    if (!nonblock) {
        freeos_syscall(SYS_FDCTL, fds[0], FREEOS_FDCTL_NONBLOCK, 0);
        freeos_syscall(SYS_FDCTL, fds[1], FREEOS_FDCTL_NONBLOCK, 0);
    }
    return 0;
}

int pipe(int fds[2]) {
    return pipe_with_mode(fds, 0);
}

/* `pipe2`: `O_CLOEXEC` у нас и так всегда (см. `fcntl` ниже), `O_NONBLOCK`
 * — настоящий с фазы 66b. */
int pipe2(int fds[2], int flags) {
    if ((flags & ~(O_CLOEXEC | O_NONBLOCK)) != 0) {
        errno = EINVAL;
        return -1;
    }
    return pipe_with_mode(fds, (flags & O_NONBLOCK) != 0);
}

/* `fcntl` — только то, что имеет смысл при нашей модели:
 *
 * - `F_DUPFD` — копия. Нижнюю границу, которую просит вызывающий, ядро не
 *   принимает: если самый маленький свободный номер ниже неё, это `EINVAL`, а
 *   не тихо другой номер;
 * - `F_GETFD` — всегда `FD_CLOEXEC`, и это правда, а не заглушка: запущенная
 *   программа не наследует дескрипторов вовсе (`SYS_LAUNCH` называет её потоки
 *   поимённо). Поставить флаг — можно, снять (попросить наследования) — нельзя,
 *   `ENOSYS`;
 * - `F_GETFL` — `O_RDWR`, у неблокирующего сокета и канала ещё `O_NONBLOCK`
 *   (фазы 65b, 66b); `O_APPEND` у нас не бывает. `F_SETFL` включает и
 *   снимает `O_NONBLOCK` у сокета и канала; у файла он ничего не меняет —
 *   файл не ждёт и так;
 * - блокировки (`F_GETLK`, `F_SETLK`, `F_SETLKW`) — `ENOSYS`: у ядра их нет. */
int fcntl(int fd, int cmd, ...) {
    va_list args;
    va_start(args, cmd);
    long arg = va_arg(args, long);
    va_end(args);

    struct stat info;
    if (fstat(fd, &info) != 0) {
        return -1;
    }
    switch (cmd) {
    case F_DUPFD:
    case F_DUPFD_CLOEXEC: {
        int copy = dup(fd);
        if (copy >= 0 && copy < arg) {
            close(copy);
            errno = EINVAL;
            return -1;
        }
        return copy;
    }
    case F_GETFD:
        return FD_CLOEXEC;
    case F_SETFD:
        if ((arg & FD_CLOEXEC) == 0) {
            errno = ENOSYS;
            return -1;
        }
        return 0;
    case F_GETFL:
        if (S_ISFIFO(info.st_mode)) {
            long flags = freeos_syscall(SYS_FDCTL, fd, FREEOS_FDCTL_FLAGS, 0);
            if (flags < 0) {
                return freeos_set_errno(flags);
            }
            return O_RDWR | ((flags & FREEOS_FD_NONBLOCK) ? O_NONBLOCK : 0);
        }
        return O_RDWR | (freeos_socket_is_nonblock(fd) ? O_NONBLOCK : 0);
    case F_SETFL:
        if (arg & O_APPEND) {
            errno = ENOSYS;
            return -1;
        }
        /* `O_NONBLOCK` — у сокета (фаза 65b) и канала (фаза 66b). */
        if (S_ISSOCK(info.st_mode)) {
            return freeos_socket_set_nonblock(fd, (arg & O_NONBLOCK) != 0);
        }
        if (S_ISFIFO(info.st_mode)) {
            long code = freeos_syscall(SYS_FDCTL, fd, FREEOS_FDCTL_NONBLOCK, (arg & O_NONBLOCK) ? 1 : 0);
            return code < 0 ? freeos_set_errno(code) : 0;
        }
        return 0;
    default:
        errno = ENOSYS;
        return -1;
    }
}

/* Окружение программы (фаза 68f).
 *
 * Ядро переменных окружения программам не передаёт вовсе, и `getenv("HOME")`
 * был пуст всегда. Чужой код ищет через него, куда класть настройки:
 * `SDL_GetPrefPath` отвечал отказом, и SDL Sopwith не мог сохранить ни
 * настроек, ни рекордов. Поэтому стартовый код заводит самое нужное сам:
 *
 * - `HOME`, `USER`, `LOGNAME` — из `/etc/system.cfg` (`user=`, `home=`): это
 *   общедоступная копия того, что лежит в закрытом `/etc/passwd`, ровно для
 *   программ (см. `config_text` установщика). Только если программа запущена
 *   от этого человека; у root — `/root`, если такой каталог есть, иначе `/`.
 * - `PATH` — `/bin`.
 *
 * Уже заданное не трогается. */
static void environ_line(char *line, char *user, size_t user_len, char *home, size_t home_len) {
    if (strncmp(line, "user=", 5) == 0) {
        strncpy(user, line + 5, user_len - 1);
        user[user_len - 1] = '\0';
    } else if (strncmp(line, "home=", 5) == 0) {
        strncpy(home, line + 5, home_len - 1);
        home[home_len - 1] = '\0';
    }
}

void freeos_environ_start(void) {
    char user[64] = "";
    char home[256] = "";
    char text[2048];
    struct stat info;

    setenv("PATH", "/bin", 0);
    if (getenv("HOME") != NULL) {
        return;
    }
    int fd = open("/etc/system.cfg", O_RDONLY);
    if (fd >= 0) {
        ssize_t got = read(fd, text, sizeof(text) - 1);
        close(fd);
        if (got > 0) {
            text[got] = '\0';
            char *line = text;
            while (line != NULL && *line != '\0') {
                char *next = strchr(line, '\n');
                if (next != NULL) {
                    *next++ = '\0';
                }
                environ_line(line, user, sizeof(user), home, sizeof(home));
                line = next;
            }
        }
    }
    if (getuid() != 0 && home[0] != '\0') {
        setenv("HOME", home, 0);
        if (user[0] != '\0') {
            setenv("USER", user, 0);
            setenv("LOGNAME", user, 0);
        }
    } else if (getuid() == 0) {
        setenv("HOME", stat("/root", &info) == 0 ? "/root" : "/", 0);
        setenv("USER", "root", 0);
        setenv("LOGNAME", "root", 0);
    }
}
