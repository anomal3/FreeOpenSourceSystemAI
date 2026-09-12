/* Слой операционной системы под picolibc.
 *
 * picolibc устроена как «библиотека плюс тонкий слой заглушек под чужую
 * систему» — та же модель, что у newlib, и ровно та, которую выбрал ROADMAP.
 * Этот файл и есть тот слой: пятнадцать функций, каждая переводит просьбу
 * стандартной библиотеки в системный вызов FreeOS.
 *
 * # Главное правило этого файла
 *
 * **Нереализованное возвращает ошибку, а не притворяется успехом.** Заглушка,
 * отвечающая нулём, — самый дешёвый способ получить систему, где всё
 * собирается и ничего не работает: программа думает, что записала файл, а файла
 * нет; думает, что узнала время, и печатает первое января. Поэтому там, где под
 * нами нет соответствующего вызова, стоит `errno = ENOSYS` и `-1`, и это
 * написано в комментарии рядом.
 *
 * # Почему ошибки переводятся, а не пробрасываются
 *
 * Ядро отвечает своими отрицательными кодами ([`freeos-syscall.h`]), а C ждёт
 * `-1` и `errno`. Перевод — таблица в [`set_errno`]; её неполнота названа там же
 * вслух: код, которого в таблице нет, становится `EIO`, а не молча нулём.
 */

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/times.h>
#include <unistd.h>

#include "freeos-syscall.h"

/* ── Ошибки ──────────────────────────────────────────────────────────────── */

/* Перевести код ядра в `errno` и вернуть -1.
 *
 * Таблица неполная намеренно: в ней те коды, которые программа на C способна
 * осмысленно различить. Всё прочее — `EIO`, то есть «устройство отказало». Это
 * честнее, чем выдумывать соответствие: `EIO` человек пойдёт проверять, а
 * выдуманный `EINVAL` уведёт его искать ошибку в своих аргументах. */
static int set_errno(long code) {
    switch (code) {
    case FREEOS_ERR_NOT_FOUND:
        errno = ENOENT;
        break;
    case FREEOS_ERR_PERMISSION:
        errno = EACCES;
        break;
    case FREEOS_ERR_BAD_FD:
        errno = EBADF;
        break;
    case FREEOS_ERR_TOO_MANY_FILES:
        errno = EMFILE;
        break;
    case FREEOS_ERR_EXISTS:
        errno = EEXIST;
        break;
    case FREEOS_ERR_NOT_EMPTY:
        errno = ENOTEMPTY;
        break;
    case FREEOS_ERR_NO_SPACE:
        errno = ENOSPC;
        break;
    case FREEOS_ERR_LIMIT:
        errno = ENOMEM;
        break;
    case FREEOS_ERR_AGAIN:
        errno = EAGAIN;
        break;
    case FREEOS_ERR_BROKEN_PIPE:
        errno = EPIPE;
        break;
    case FREEOS_ERR_BAD_PATH:
        errno = ENAMETOOLONG;
        break;
    case FREEOS_ERR_BAD_ADDRESS:
        errno = EFAULT;
        break;
    case FREEOS_ERR_NO_SYSCALL:
    case FREEOS_ERR_UNSUPPORTED:
        errno = ENOSYS;
        break;
    case FREEOS_ERR_NO_FILESYSTEM:
        errno = ENODEV;
        break;
    default:
        errno = EIO;
        break;
    }
    return -1;
}

/* ── Ввод и вывод ────────────────────────────────────────────────────────── */

ssize_t write(int fd, const void *buf, size_t count) {
    long done = freeos_syscall(SYS_WRITE, fd, (long)buf, (long)count);
    return done < 0 ? set_errno(done) : (ssize_t)done;
}

ssize_t read(int fd, void *buf, size_t count) {
    long done = freeos_syscall(SYS_READ, fd, (long)buf, (long)count);
    return done < 0 ? set_errno(done) : (ssize_t)done;
}

int close(int fd) {
    long code = freeos_syscall(SYS_CLOSE, fd, 0, 0);
    return code < 0 ? set_errno(code) : 0;
}

/* Открыть файл.
 *
 * Флаги переводятся, а не передаются как есть: числа `O_*` у C свои, у договора
 * свои, и совпадение двух наборов на трёх младших битах было бы совпадением, на
 * которое нельзя опираться.
 *
 * Чего этот перевод **не** умеет, и это названо здесь, а не спрятано:
 * `O_APPEND`, `O_EXCL` и `O_NONBLOCK` договором не предусмотрены вовсе. Просьба
 * с ними — `ENOSYS`, а не тихое открытие без них: файл, открытый «на дозапись»,
 * которая не дозапись, теряет данные молча. */
int open(const char *path, int flags, ...) {
    if (flags & (O_APPEND | O_EXCL | O_NONBLOCK)) {
        errno = ENOSYS;
        return -1;
    }

    long os_flags = 0;
    if ((flags & O_ACCMODE) == O_WRONLY || (flags & O_ACCMODE) == O_RDWR) {
        os_flags |= FREEOS_O_WRITE;
    }
    if (flags & O_CREAT) {
        os_flags |= FREEOS_O_CREATE;
    }
    if (flags & O_TRUNC) {
        os_flags |= FREEOS_O_TRUNC;
    }

    long fd = freeos_syscall(SYS_OPEN, (long)path, (long)strlen(path), os_flags);
    return fd < 0 ? set_errno(fd) : (int)fd;
}

off_t lseek(int fd, off_t offset, int whence) {
    long where;
    switch (whence) {
    case SEEK_SET:
        where = FREEOS_SEEK_SET;
        break;
    case SEEK_CUR:
        where = FREEOS_SEEK_CUR;
        break;
    case SEEK_END:
        where = FREEOS_SEEK_END;
        break;
    default:
        errno = EINVAL;
        return -1;
    }
    long position = freeos_syscall(SYS_SEEK, fd, (long)offset, where);
    return position < 0 ? set_errno(position) : (off_t)position;
}

int isatty(int fd) {
    long answer = freeos_syscall(SYS_ISATTY, fd, 0, 0);
    if (answer < 0) {
        set_errno(answer);
        return 0;
    }
    if (answer == 0) {
        /* Так требует POSIX: не терминал — это `0` **и** `ENOTTY`. Без второго
         * `isatty` неотличима от отказа, и stdio выбирает буферизацию наугад. */
        errno = ENOTTY;
    }
    return (int)answer;
}

/* ── Сведения о файле ────────────────────────────────────────────────────── */

/* Переложить ответ ядра в `struct stat`.
 *
 * Заполняются только те поля, которые ядро действительно знает. Остальные
 * остаются нулями — и это не лень: `st_nlink`, `st_dev` и `st_ino` в этой
 * системе пока не имеют смысла, а выдуманная единица в `st_nlink` заставила бы
 * чужую программу поверить, что жёсткие ссылки здесь есть. */
static void fill_stat(struct stat *out, const struct freeos_stat *info) {
    memset(out, 0, sizeof(*out));
    out->st_size = (off_t)info->size;
    out->st_uid = info->uid;
    out->st_gid = info->gid;
    out->st_mode = info->mode & 07777;
    switch (info->kind) {
    case FREEOS_KIND_DIRECTORY:
        out->st_mode |= S_IFDIR;
        break;
    case FREEOS_KIND_PIPE:
        out->st_mode |= S_IFIFO;
        break;
    default:
        out->st_mode |= S_IFREG;
        break;
    }
    /* Размер блока — то, по чему stdio выбирает свой буфер. Страница: ровно
     * столько ядро читает за раз, и просить у него меньше значит платить
     * системным вызовом за каждую строку. */
    out->st_blksize = 4096;
    out->st_blocks = (blkcnt_t)((info->size + 511) / 512);
}

int fstat(int fd, struct stat *out) {
    struct freeos_stat info;
    long code = freeos_syscall(SYS_FSTAT, fd, (long)&info, 0);
    if (code < 0) {
        return set_errno(code);
    }
    fill_stat(out, &info);
    return 0;
}

int stat(const char *path, struct stat *out) {
    struct freeos_stat info;
    long code = freeos_syscall(SYS_STAT, (long)path, (long)strlen(path), (long)&info);
    if (code < 0) {
        return set_errno(code);
    }
    fill_stat(out, &info);
    return 0;
}

int unlink(const char *path) {
    long code = freeos_syscall(SYS_REMOVE, (long)path, (long)strlen(path), 0);
    return code < 0 ? set_errno(code) : 0;
}

/* ── Время ───────────────────────────────────────────────────────────────── */

int gettimeofday(struct timeval *now, void *timezone) {
    (void)timezone; /* Часовых поясов у этого вызова не бывает с 4.3BSD. */
    struct freeos_timespec stamp;
    long code = freeos_syscall(SYS_CLOCK, FREEOS_CLOCK_REALTIME, (long)&stamp, 0);
    if (code < 0) {
        return set_errno(code);
    }
    now->tv_sec = (time_t)stamp.seconds;
    now->tv_usec = (suseconds_t)(stamp.nanos / 1000);
    return 0;
}

/* Время, потраченное процессом.
 *
 * `tms_cutime`/`tms_cstime` — время потомков, и они остаются нулями: ядро их не
 * считает вовсе. Ноль здесь правдив, а не выдуман: потомков, чьё время учтено,
 * у программы в этой системе действительно нет. */
clock_t times(struct tms *out) {
    struct {
        uint64_t cpu_ms;
        uint64_t uptime_ms;
    } info;
    long code = freeos_syscall(SYS_TIMES, (long)&info, 0, 0);
    if (code < 0) {
        return (clock_t)set_errno(code);
    }
    memset(out, 0, sizeof(*out));
    out->tms_utime = (clock_t)info.cpu_ms;
    return (clock_t)info.uptime_ms;
}

/* ── Случайность ─────────────────────────────────────────────────────────── */

int getentropy(void *buf, size_t len) {
    if (len > 256) {
        /* Предел из самого определения `getentropy`, а не наш. */
        errno = EIO;
        return -1;
    }
    long got = freeos_syscall(SYS_RANDOM, (long)buf, (long)len, 0);
    if (got < 0) {
        return set_errno(got);
    }
    if ((size_t)got != len) {
        /* `getentropy` обязана отдать **всё** запрошенное или не отдать
         * ничего. Частичный ответ — отказ, а не «сколько получилось»: половина
         * ключа, дополненная нулями, выглядит как ключ. */
        errno = EIO;
        return -1;
    }
    return 0;
}

/* ── Куча ────────────────────────────────────────────────────────────────── */

/* Сколько памяти просить у ядра за один раз.
 *
 * Четверть мегабайта: заметно больше, чем нужно печати и паре файлов, и заметно
 * меньше предела, который ядро даёт одной задаче. */
#define HEAP_CHUNK (256 * 1024)

static char *heap_start;   /* начало последней взятой области */
static char *heap_current; /* докуда роздано */
static char *heap_end;     /* докуда взято у ядра */

/* Раздвинуть кучу.
 *
 * # Почему это не настоящий `sbrk`
 *
 * Настоящий обещает непрерывность: память, выданная вторым вызовом, лежит сразу
 * за первой. Наш `SYS_MMAP` адреса не обещает — он возвращает тот, который
 * выбрал сам, — поэтому непрерывность здесь **проверяется**, а не
 * предполагается: если очередная область легла не вплотную к прежней, мы
 * отвечаем отказом, а не отдаём разорванный кусок под видом целого.
 *
 * Цена названа честно: куча программы на C ограничена тем, сколько ядро выдало
 * подряд. Снять этот предел можно только вызовом, который умеет расширять
 * область на месте, — а такого в договоре нет, и заводить его ради malloc'а
 * значит заводить его вслепую.
 */
void *sbrk(ptrdiff_t increment) {
    if (increment < 0) {
        /* Уменьшение кучи не поддерживается: вернуть ядру можно только область
         * целиком (`SYS_MUNMAP` принимает точное совпадение), а из середины
         * кучи возвращать нечего. Отвечаем отказом, а не молча «получилось»:
         * malloc, поверивший в возврат, посчитал бы память свободной дважды. */
        errno = ENOSYS;
        return (void *)-1;
    }

    if (heap_current == NULL) {
        long got = freeos_syscall(SYS_MMAP, HEAP_CHUNK, 0, 0);
        if (got < 0) {
            set_errno(got);
            return (void *)-1;
        }
        heap_start = (char *)got;
        heap_current = heap_start;
        heap_end = heap_start + HEAP_CHUNK;
    }

    while (heap_current + increment > heap_end) {
        long got = freeos_syscall(SYS_MMAP, HEAP_CHUNK, 0, 0);
        if (got < 0) {
            set_errno(got);
            return (void *)-1;
        }
        if ((char *)got != heap_end) {
            /* Не вплотную. Взятое не возвращаем: `SYS_MUNMAP` принял бы его, но
             * отказ уже произошёл, и лишний вызов на пути отказа — это второе
             * место, где что-то может пойти не так. Область останется занятой
             * до конца программы, то есть до ближайшего мига. */
            errno = ENOMEM;
            return (void *)-1;
        }
        heap_end += HEAP_CHUNK;
    }

    char *previous = heap_current;
    heap_current += increment;
    return previous;
}

/* ── Конец программы ─────────────────────────────────────────────────────── */

void _exit(int status) {
    freeos_syscall(SYS_EXIT, status, 0, 0);
    /* Сюда не возвращаются: `SYS_EXIT` снимает задачу. Цикл стоит затем, что
     * `_exit` объявлена не возвращающей, и компилятор вправе рассчитывать на
     * это буквально. */
    for (;;) {
    }
}

/* ── То, чего в этой системе нет ─────────────────────────────────────────── */

/* Сигналов у FreeOS нет вовсе. picolibc зовёт `sigprocmask` из `abort`, и
 * ответ «нет такого вызова» её устраивает: она просто идёт дальше, к `_exit`.
 * Ответить нулём было бы хуже — это означало бы «маска установлена», и
 * программа, которая на неё рассчитывает, узнала бы правду не здесь. */
int sigprocmask(int how, const sigset_t *set, sigset_t *old) {
    (void)how;
    (void)set;
    (void)old;
    errno = ENOSYS;
    return -1;
}
