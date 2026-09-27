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
 * `-1` и `errno`. Перевод — таблица в [`freeos_set_errno`]; её неполнота названа там же
 * вслух: код, которого в таблице нет, становится `EIO`, а не молча нулём.
 */

/* Слой ОС отвечает за все имена, которые picolibc знает, — в том числе за
 * «грубые» и «сырые» часы, видимые только под `_GNU_SOURCE`. */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <sys/times.h>
#include <sched.h>
#include <time.h>
#include <unistd.h>

#include "freeos-internal.h"
#include "freeos-syscall.h"

/* ── Ошибки ──────────────────────────────────────────────────────────────── */

/* Перевести код ядра в `errno` и вернуть -1.
 *
 * Таблица неполная намеренно: в ней те коды, которые программа на C способна
 * осмысленно различить. Всё прочее — `EIO`, то есть «устройство отказало». Это
 * честнее, чем выдумывать соответствие: `EIO` человек пойдёт проверять, а
 * выдуманный `EINVAL` уведёт его искать ошибку в своих аргументах. */
int freeos_set_errno(long code) {
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
    return done < 0 ? freeos_set_errno(done) : (ssize_t)done;
}

ssize_t read(int fd, void *buf, size_t count) {
    long done = freeos_syscall(SYS_READ, fd, (long)buf, (long)count);
    return done < 0 ? freeos_set_errno(done) : (ssize_t)done;
}

int close(int fd) {
    long code = freeos_syscall(SYS_CLOSE, fd, 0, 0);
    return code < 0 ? freeos_set_errno(code) : 0;
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

    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    long fd = freeos_syscall(SYS_OPEN, (long)full, (long)strlen(full), os_flags);
    return fd < 0 ? freeos_set_errno(fd) : (int)fd;
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
    return position < 0 ? freeos_set_errno(position) : (off_t)position;
}

int isatty(int fd) {
    long answer = freeos_syscall(SYS_ISATTY, fd, 0, 0);
    if (answer < 0) {
        freeos_set_errno(answer);
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
        return freeos_set_errno(code);
    }
    fill_stat(out, &info);
    return 0;
}

int stat(const char *path, struct stat *out) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    struct freeos_stat info;
    long code = freeos_syscall(SYS_STAT, (long)full, (long)strlen(full), (long)&info);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    fill_stat(out, &info);
    return 0;
}

int unlink(const char *path) {
    char buf[FREEOS_PATH_LIMIT + 1];
    const char *full = freeos_path(path, buf);
    if (full == NULL) {
        return -1;
    }
    long code = freeos_syscall(SYS_REMOVE, (long)full, (long)strlen(full), 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

int rename(const char *from, const char *to) {
    char from_buf[FREEOS_PATH_LIMIT + 1];
    char to_buf[FREEOS_PATH_LIMIT + 1];
    from = freeos_path(from, from_buf);
    to = from != NULL ? freeos_path(to, to_buf) : NULL;
    if (to == NULL) {
        return -1;
    }
    /* Оба пути уезжают одним буфером: у вызова три аргумента, а значений нужно
     * четыре — два адреса и две длины. Склейку разбирает ядро по первой длине;
     * тот же приём и в обёртке для Rust, и расходиться им нельзя. */
    size_t from_len = strlen(from);
    size_t to_len = strlen(to);
    char joined[2 * FREEOS_PATH_LIMIT];
    if (from_len == 0 || to_len == 0 || from_len + to_len > sizeof(joined)) {
        errno = ENAMETOOLONG;
        return -1;
    }
    memcpy(joined, from, from_len);
    memcpy(joined + from_len, to, to_len);
    long code = freeos_syscall(SYS_RENAME, (long)joined, (long)from_len,
                               (long)(from_len + to_len));
    return code < 0 ? freeos_set_errno(code) : 0;
}

/* ── Запуск другой программы ─────────────────────────────────────────────── */

/* `system` есть, а оболочки нет — и это не одно и то же.
 *
 * picolibc приносит свою `system`, написанную через `fork`+`execve`+`waitpid`.
 * Ни одного из этих трёх вызовов у нас нет и не будет в таком виде: `fork`
 * означает копию адресного пространства, которую наша модель памяти не делает
 * вовсе. Поэтому `system` здесь своя, и компоновщик берёт именно её — `-lfreeos`
 * стоит перед `-lc`.
 *
 * Делает она ровно то, что умеет система: отдаёт строку тому же разборщику
 * команд, которым пользуется оболочка (`SYS_SPAWN`), и дожидается конца
 * (`SYS_WAIT`). Этого хватает для `os.execute("ls /bin")` и не хватает ни для
 * чего из того, что делает настоящий `sh`.
 *
 * Отсюда два решения, и оба названы вслух:
 *
 * 1. **`system(NULL)` отвечает единицей.** Командный исполнитель есть — просто
 *    он не `sh`. Ответить нулём значило бы сказать программе «запускать нечего»,
 *    и она не стала бы и пробовать то, что у нас прекрасно работает.
 * 2. **Строка со знаками оболочки отвергается.** `|`, `>`, `<`, `&`, `;`, `$`,
 *    кавычка с обратным наклоном — всё это мы исполнить не можем, а выполнить
 *    команду, выбросив половину строки, — худший из возможных ответов: тихий и
 *    неверный. Такой вызов возвращает -1 и `ENOTSUP`.
 *
 * Код возврата переводится так же, как это делает настоящий `system`: значение
 * годится для `WEXITSTATUS`, то есть код программы уезжает в старший байт. */
int system(const char *command) {
    if (command == NULL) {
        return 1;
    }
    for (const char *at = command; *at != '\0'; at++) {
        if (strchr("|<>&;$`", *at) != NULL) {
            errno = ENOTSUP;
            return -1;
        }
    }
    long task = freeos_syscall(SYS_SPAWN, (long)command, (long)strlen(command),
                               FREEOS_SPAWN_INHERIT);
    if (task < 0) {
        return freeos_set_errno(task);
    }
    long code = freeos_syscall(SYS_WAIT, task, 0, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    /* Как у POSIX: младший байт — сигнал, которого у нас не бывает, старший —
     * код возврата. Программа на C разбирает это `WEXITSTATUS`, и выдумывать
     * своё соглашение значило бы сломать её разбор. */
    return (int)((code & 0xff) << 8);
}

/* ── Время ───────────────────────────────────────────────────────────────── */

int gettimeofday(struct timeval *now, void *timezone) {
    (void)timezone; /* Часовых поясов у этого вызова не бывает с 4.3BSD. */
    struct freeos_timespec stamp;
    long code = freeos_syscall(SYS_CLOCK, FREEOS_CLOCK_REALTIME, (long)&stamp, 0);
    if (code < 0) {
        return freeos_set_errno(code);
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
        return (clock_t)freeos_set_errno(code);
    }
    memset(out, 0, sizeof(*out));
    out->tms_utime = (clock_t)info.cpu_ms;
    return (clock_t)info.uptime_ms;
}

/* ── Часы и сон (фаза 57) ────────────────────────────────────────────────── */

/* Какие часы ядра отвечают на эти часы POSIX. «Грубые» и «сырые» варианты —
 * те же часы: других у ядра нет, и честнее ответить ими, чем отказом, — а
 * часы процессорного времени (`CLOCK_PROCESS_CPUTIME_ID`) это другое число, и
 * им отказ. */
static int kernel_clock(clockid_t clock) {
    switch (clock) {
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_COARSE:
        return FREEOS_CLOCK_REALTIME;
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_MONOTONIC_COARSE:
        return FREEOS_CLOCK_MONOTONIC;
    default:
        return -1;
    }
}

int clock_gettime(clockid_t clock, struct timespec *now) {
    int which = kernel_clock(clock);
    if (which < 0) {
        errno = EINVAL;
        return -1;
    }
    struct freeos_timespec stamp;
    long code = freeos_syscall(SYS_CLOCK, which, (long)&stamp, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    now->tv_sec = (time_t)stamp.seconds;
    now->tv_nsec = (long)stamp.nanos;
    return 0;
}

/* Разрешение — миллисекунда: планировщик и сон идут ею (`SYS_FUTEX_WAIT`,
 * `SYS_NANOSLEEP` с точностью до тика), и обещать мельче значило бы обещать
 * то, чего сроки в системе не держат. */
int clock_getres(clockid_t clock, struct timespec *resolution) {
    if (kernel_clock(clock) < 0) {
        errno = EINVAL;
        return -1;
    }
    if (resolution != NULL) {
        resolution->tv_sec = 0;
        resolution->tv_nsec = 1000000;
    }
    return 0;
}

/* Сон не прерывается ничем — сигналов нет, — поэтому остаток всегда ноль. */
int nanosleep(const struct timespec *request, struct timespec *remaining) {
    if (request->tv_sec < 0 || request->tv_nsec < 0 || request->tv_nsec >= 1000000000L) {
        errno = EINVAL;
        return -1;
    }
    long code = freeos_syscall(SYS_NANOSLEEP, (long)request->tv_sec, request->tv_nsec, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    if (remaining != NULL) {
        remaining->tv_sec = 0;
        remaining->tv_nsec = 0;
    }
    return 0;
}

int sched_yield(void) {
    freeos_syscall(SYS_YIELD, 0, 0, 0);
    return 0;
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
        return freeos_set_errno(got);
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

/* Сколько адресов куча резервирует разом.
 *
 * Сто двадцать восемь мегабайт — адреса, а не память: резерв ленивый
 * (`FREEOS_MAP_LAZY`, фаза 56), и кадр приходит в страницу только тогда, когда
 * `malloc` её коснётся. Четверть от того, что ядро даёт одной задаче по
 * запросу (512 МиБ), — остальное остаётся `mmap`, которым пользуются большие
 * программы в обход кучи. */
#define HEAP_RESERVE (128UL * 1024 * 1024)

static char *heap_start;   /* начало резерва */
static char *heap_current; /* докуда роздано */
static char *heap_end;     /* конец резерва */

/* Раздвинуть кучу.
 *
 * # Почему резерв, а не «ещё кусок вплотную»
 *
 * До фазы 56 куча росла кусками по четверти мегабайта, и каждый кусок обязан
 * был лечь вплотную к прежнему — `SYS_MMAP` адреса не обещает, и непрерывность
 * проверялась, а не предполагалась. Это работало, пока между двумя `malloc`
 * никто не звал `mmap` сам. Чужая среда исполнения (Mono) зовёт его на каждом
 * шагу, и первая же её область, легшая посреди кучи, обрывала кучу навсегда.
 *
 * Резерв снимает вопрос целиком: адреса заняты один раз, сразу на весь рост, и
 * никакой `mmap` в них не попадёт. Цена — потолок в `HEAP_RESERVE`, названный
 * вслух; упёршийся в него `malloc` получает `ENOMEM`, а не чужую память.
 *
 * Уменьшение кучи только двигает границу: память остаётся за программой. Снять
 * страницы можно было бы (частичный `munmap` с фазы 56 есть), но следующий
 * рост пришёл бы в снятые адреса — и программу сняли бы за обращение к ним.
 */
void *sbrk(ptrdiff_t increment) {
    if (heap_start == NULL) {
        long got = freeos_syscall(SYS_MMAP, (long)HEAP_RESERVE, FREEOS_MAP_LAZY, 0);
        if (got < 0) {
            freeos_set_errno(got);
            return (void *)-1;
        }
        heap_start = (char *)got;
        heap_current = heap_start;
        heap_end = heap_start + HEAP_RESERVE;
    }

    if (increment > heap_end - heap_current || increment < heap_start - heap_current) {
        errno = ENOMEM;
        return (void *)-1;
    }
    char *previous = heap_current;
    heap_current += increment;
    return previous;
}

/* ── Отображения памяти (фаза 56) ────────────────────────────────────────── */

/* `mmap`: безымянная память — всегда ленивая, как у любой системы с
 * отложенным выделением; кусок файла — только на чтение.
 *
 * Чего нет, и почему это отказ, а не «сделаем похоже»:
 * - `MAP_FIXED` — ядро выбирает адрес само и чужой не принимает. Отказ
 *   `EINVAL`: программа, которой нужен именно этот адрес, обязана узнать, что
 *   его не будет, — положить память в другое место молча значило бы отдать не
 *   то, что просили. Адрес **без** `MAP_FIXED` — подсказка, и ею можно
 *   пренебречь, как разрешает POSIX.
 * - запись в отображение файла — ядро отображает файл только на чтение;
 *   `EACCES`, как у POSIX при файле, открытом без права записи.
 * - `MAP_SHARED` у безымянной памяти равен `MAP_PRIVATE`: делить её не с кем,
 *   `fork` в системе нет.
 *
 * `MAP_JIT` (фаза 59) — память для кода, который программа пишет себе сама,
 * как у Apple: только безымянная, и `prot` у неё не спрашивается — страница
 * бывает то «читать и писать», то «читать и исполнять», и переключает её ядро
 * по обращению. Без этого флага «писать и исполнять» — отказ, как и был. */
void *mmap(void *addr, size_t len, int prot, int flags, int fd, off_t offset) {
    (void)addr;
    if (len == 0 || (flags & MAP_FIXED) != 0) {
        errno = EINVAL;
        return MAP_FAILED;
    }
    if ((flags & MAP_JIT) != 0) {
        if ((flags & MAP_ANONYMOUS) == 0) {
            errno = EINVAL;
            return MAP_FAILED;
        }
        long got = freeos_syscall(SYS_MMAP, (long)len, FREEOS_MAP_JIT, 0);
        if (got < 0) {
            freeos_set_errno(got);
            return MAP_FAILED;
        }
        return (void *)got;
    }
    if ((prot & PROT_WRITE) != 0 && (prot & PROT_EXEC) != 0) {
        /* W^X — правило системы. `EACCES` — ответ POSIX на права, которых не
         * дадут. */
        errno = EACCES;
        return MAP_FAILED;
    }

    long got;
    if ((flags & MAP_ANONYMOUS) != 0) {
        got = freeos_syscall(SYS_MMAP, (long)len, FREEOS_MAP_LAZY, 0);
        if (got < 0) {
            freeos_set_errno(got);
            return MAP_FAILED;
        }
        /* Ядро выдаёт «читать и писать»; остальное — второй просьбой. */
        if (prot != (PROT_READ | PROT_WRITE)) {
            long done = freeos_syscall(SYS_MPROTECT, got, (long)len, prot);
            if (done < 0) {
                freeos_syscall(SYS_MUNMAP, got, (long)len, 0);
                freeos_set_errno(done);
                return MAP_FAILED;
            }
        }
        return (void *)got;
    }

    if ((prot & PROT_WRITE) != 0) {
        errno = EACCES;
        return MAP_FAILED;
    }
    got = freeos_syscall(SYS_MMAP_FILE, fd, (long)offset, (long)len);
    if (got < 0) {
        freeos_set_errno(got);
        return MAP_FAILED;
    }
    return (void *)got;
}

int munmap(void *addr, size_t len) {
    long done = freeos_syscall(SYS_MUNMAP, (long)addr, (long)len, 0);
    return done < 0 ? freeos_set_errno(done) : 0;
}

int mprotect(void *addr, size_t len, int prot) {
    long done = freeos_syscall(SYS_MPROTECT, (long)addr, (long)len, prot);
    return done < 0 ? freeos_set_errno(done) : 0;
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
