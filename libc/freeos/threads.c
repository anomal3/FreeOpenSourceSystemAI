/* Потоки для программ на C (фаза 57).
 *
 * Что здесь: хранилище потока (TLS) по шаблону, который ядро кладёт в
 * страницу процесса; потоки POSIX поверх `SYS_THREAD_CREATE`; мьютексы,
 * условные переменные, блокировки чтения-записи и `pthread_once` поверх
 * ожидания на адресе (`SYS_FUTEX_WAIT` со сроком); ключи с деструкторами;
 * и восемь функций блокировки, которые picolibc, собранная многопоточной,
 * требует от системы (`sys/lock.h`) — вместе с её статическим замком.
 *
 * # Почему picolibc не обходится без этого файла
 *
 * До фазы 57 она была собрана однопоточной **намеренно** (фаза 45): без
 * потоков в системе блокировки в `stdio` и `malloc` были бы пустой тратой, а
 * `errno` в TLS ронял программу на первой же ошибке, потому что блок TLS
 * никто не заводил. Теперь потоки есть, и однопоточная сборка стала неправдой:
 * два потока, одновременно зовущие `malloc`, портили бы кучу, а `errno` был бы
 * один на всех. Поэтому picolibc собирается с TLS и блокировками, а этот файл
 * даёт то, на что она при этом рассчитывает.
 *
 * # Замок — три состояния слова
 *
 * 0 — свободен, 1 — занят и никто не ждёт, 2 — занят и кто-то, может быть,
 * ждёт. Отпускающий зовёт ядро только из двойки. Это замок Дреппера («Futexes
 * are tricky», mutex3), тот же, что у программ на Rust (`user_progs::Lock`).
 */

#include <errno.h>
#include <limits.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/lock.h>
#include <time.h>

#include "freeos-syscall.h"

/* ── Ожидание на адресе ──────────────────────────────────────────────────── */

/* Уснуть, пока по адресу лежит `expected`, но не дольше `timeout_ms` (ноль —
 * без срока). Ответ ядра: 0 — разбудили, `FREEOS_FUTEX_CHANGED` — слово уже
 * другое, `FREEOS_FUTEX_TIMED_OUT` — срок вышел. */
static long futex_wait(unsigned int *word, unsigned int expected, unsigned long timeout_ms) {
    return freeos_syscall(SYS_FUTEX_WAIT, (long)word, (long)expected, (long)timeout_ms);
}

/* Разбудить `count` ждущих; ноль — всех. */
static void futex_wake(unsigned int *word, long count) {
    freeos_syscall(SYS_FUTEX_WAKE, (long)word, count, 0);
}

static unsigned int load(const unsigned int *word) { return __atomic_load_n(word, __ATOMIC_ACQUIRE); }

static void store(unsigned int *word, unsigned int value) { __atomic_store_n(word, value, __ATOMIC_RELEASE); }

/* ── Хранилище потока ────────────────────────────────────────────────────── */

/* Шаблон `PT_TLS` программы — его кладёт ядро (`user_abi::TlsTemplate`). */
struct freeos_tls_template {
    uint64_t addr;
    uint64_t filesz;
    uint64_t memsz;
    uint64_t align;
};

#define TEMPLATE ((const struct freeos_tls_template *)(FREEOS_PROCESS_PAGE + FREEOS_PROCESS_PAGE_TLS))

/* Сколько места отвести под блок управления потоком (TCB).
 *
 * На x86-64 он лежит **за** переменными, и первое его слово обязано указывать
 * на сам TCB: так компилятор узнаёт адрес базы (`mov %fs:0`). На AArch64 он
 * лежит **перед** переменными, шестнадцать байт, и не читается никем. Шесть
 * десятков байт — с запасом на оба случая. */
#define TCB_BYTES 64

static uintptr_t align_up(uintptr_t value, uintptr_t align) { return (value + align - 1) & ~(align - 1); }

/* Выравнивание блока: то, что просит шаблон, но не меньше шестнадцати — TCB и
 * векторные переменные рассчитывают на них. Смещения переменных при этом
 * считаются по выравниванию **шаблона**: так их посчитал компоновщик. */
static uintptr_t block_align(void) {
    uintptr_t align = (uintptr_t)TEMPLATE->align;
    return align < 16 ? 16 : align;
}

static uintptr_t template_align(void) {
    uintptr_t align = (uintptr_t)TEMPLATE->align;
    return align == 0 ? 1 : align;
}

/* Сколько байт нужно под блок TLS одного потока. */
static size_t tls_block_bytes(void) {
    uintptr_t memsz = (uintptr_t)TEMPLATE->memsz;
#if defined(__x86_64__)
    return align_up(memsz, template_align()) + block_align() + TCB_BYTES;
#else
    return block_align() + align_up(16, template_align()) + memsz;
#endif
}

/* Разложить шаблон в блок и вернуть то, что станет базой (`FS` / `TPIDR_EL0`).
 *
 * Раскладка — та, по которой компоновщик посчитал смещения переменных:
 *
 * - x86-64 (вариант 2 по Дрепперу): переменные лежат **под** базой, начиная с
 *   `база − выровненный memsz`; по самой базе — TCB с указателем на себя.
 * - AArch64 (вариант 1): по базе — шестнадцать байт TCB, переменные — с
 *   `база + выровненные 16`.
 *
 * Перепутать варианты значило бы получить переменные, лежащие в чужой памяти,
 * — и заметить это не сразу, а по чужим данным. */
static uintptr_t tls_lay_out(void *block) {
    const struct freeos_tls_template *tls = TEMPLATE;
    char *data;
    uintptr_t base;
#if defined(__x86_64__)
    uintptr_t span = align_up((uintptr_t)tls->memsz, template_align());
    base = align_up((uintptr_t)block + span, block_align());
    data = (char *)(base - span);
    memset((void *)base, 0, TCB_BYTES);
    *(uintptr_t *)base = base;
    memset(data, 0, span);
#else
    base = align_up((uintptr_t)block, block_align());
    data = (char *)(base + align_up(16, template_align()));
    memset((void *)base, 0, (size_t)(data - (char *)base));
    memset(data, 0, (size_t)tls->memsz);
#endif
    if (tls->filesz != 0) {
        memcpy(data, (const void *)(uintptr_t)tls->addr, (size_t)tls->filesz);
    }
    return base;
}

/* ── Потоки ──────────────────────────────────────────────────────────────── */

/* Стек потока, если программа не попросила другого. Ядро выдаёт его жадно,
 * поэтому не мегабайты; чужая среда, которой надо больше, просит через
 * `pthread_attr_setstacksize`. */
#define DEFAULT_STACK (256 * 1024)
/* Больше не даст ядро (`THREAD_STACK_MAX`). */
#define MAX_STACK (8 * 1024 * 1024)

enum { JOINABLE = 0, DETACHED = 1, EXITED = 2 };

struct freeos_thread {
    void *(*start)(void *);
    void *arg;
    void *result;
    /* Слово, на котором ждёт `pthread_join`: 1 — поток больше не трогает эту
     * структуру и свой блок TLS. */
    unsigned int done;
    /* JOINABLE, DETACHED или EXITED — кто из двоих освобождает структуру. */
    unsigned int state;
    void *tls_block;
    /* Номер задачи и стек `[stack_low, stack_high)` — у ядра, спрашивает их
     * сам поток при старте (`learn_self`): тот, кто его завёл, стека не
     * выбирал и не знает. */
    long id;
    uintptr_t stack_low;
    uintptr_t stack_high;
    const void *specific[PTHREAD_KEYS_MAX];
    /* Отсоединённые завершившиеся ждут освобождения в этом списке. */
    struct freeos_thread *next_zombie;
};

static __thread struct freeos_thread *current;
static struct freeos_thread main_thread;

/* Отсоединённые потоки, которые завершились, но освободить себя сами не могут:
 * их блок TLS — их же регистр базы до последней инструкции. Освобождает их
 * следующий `pthread_create`. */
static struct freeos_thread *zombies;

static void reap_zombies(void) {
    struct freeos_thread *list = __atomic_exchange_n(&zombies, NULL, __ATOMIC_ACQ_REL);
    while (list != NULL) {
        struct freeos_thread *next = list->next_zombie;
        if (load(&list->done) == 0) {
            /* Ещё не дошёл до конца — вернуть в список до следующего раза. */
            struct freeos_thread *head;
            do {
                head = __atomic_load_n(&zombies, __ATOMIC_ACQUIRE);
                list->next_zombie = head;
            } while (!__atomic_compare_exchange_n(&zombies, &head, list, 0, __ATOMIC_ACQ_REL,
                                                  __ATOMIC_ACQUIRE));
        } else {
            free(list->tls_block);
            free(list);
        }
        list = next;
    }
}

/* Поставить базу хранилища и записать в него, кто мы.
 *
 * Запись — в **отдельной** функции, которую нельзя встраивать, и это не
 * вкусовщина. Компилятор считает базу потока неизменной внутри функции: на
 * AArch64 он прочитал `TPIDR_EL0` ещё **до** системного вызова, который её
 * ставит, и записал `current` по старой базе — нулю; программа падала
 * обращением по адресу 0x10, не напечатав ни строки (27.09.2026). На x86-64
 * то же место работало: там обращение идёт через сегмент `FS` в сам миг
 * записи. Вызов функции — граница, через которую прочитанную базу не
 * пронести. */
__attribute__((noinline)) static void adopt(struct freeos_thread *self) { current = self; }

static void set_base(uintptr_t base) { freeos_syscall(SYS_SET_TLS, (long)base, 0, 0); }

/* Спросить у ядра свой номер и свой стек (фаза 58b). Отказ оставляет нули:
 * `pthread_getattr_np` тогда отвечает ошибкой, а не выдумывает границы. */
static void learn_self(struct freeos_thread *self) {
    struct freeos_thread_info info;
    if (freeos_syscall(SYS_THREAD_INFO, (long)&info, 0, 0) == 0) {
        self->id = (long)info.id;
        self->stack_low = (uintptr_t)info.stack_low;
        self->stack_high = (uintptr_t)info.stack_high;
    }
}

/* Первое, что делает программа на C, — до конструкторов и до `main`
 * (`crt0.c`): завести хранилище главному потоку. Без него первая же ошибка
 * системного вызова писала бы `errno` по базе ноль — так ронялась программа до
 * фазы 45, пока TLS не выключили вовсе.
 *
 * Память — `SYS_MMAP`, а не `malloc`: куча ещё не готова, а её замок сам
 * живёт в TLS-зависимом коде. Отказ здесь — конец программы: жить без `errno`
 * она не может. */
void freeos_threads_start(void) {
    long block = freeos_syscall(SYS_MMAP, (long)tls_block_bytes(), 0, 0);
    if (block < 0) {
        static const char message[] = "threads: no memory for the main thread storage\n";
        freeos_syscall(SYS_WRITE, 2, (long)message, sizeof(message) - 1);
        freeos_syscall(SYS_EXIT, 127, 0, 0);
    }
    set_base(tls_lay_out((void *)block));
    main_thread.state = JOINABLE;
    learn_self(&main_thread);
    adopt(&main_thread);
}

static void run_destructors(struct freeos_thread *self);

/* Уйти: деструкторы ключей, результат — ждущему, и выход из ядра. После
 * `done = 1` структуру и блок TLS вправе освободить другой поток, поэтому
 * дальше — только системные вызовы, ни одного обращения к ним. */
__attribute__((noreturn)) static void finish(struct freeos_thread *self, void *result) {
    run_destructors(self);
    self->result = result;
    unsigned int was = __atomic_exchange_n(&self->state, EXITED, __ATOMIC_ACQ_REL);
    if (was == DETACHED && self != &main_thread) {
        struct freeos_thread *head;
        do {
            head = __atomic_load_n(&zombies, __ATOMIC_ACQUIRE);
            self->next_zombie = head;
        } while (!__atomic_compare_exchange_n(&zombies, &head, self, 0, __ATOMIC_ACQ_REL,
                                              __ATOMIC_ACQUIRE));
    }
    unsigned int *done = &self->done;
    store(done, 1);
    futex_wake(done, 0);
    freeos_syscall(SYS_THREAD_EXIT, 0, 0, 0);
    for (;;) {
    }
}

/* Точка входа потока. Ядро зовёт её как функцию одного аргумента со стеком,
 * выровненным так, как ждёт компилятор. */
__attribute__((noreturn)) static void thread_entry(struct freeos_thread *self) {
    set_base(tls_lay_out(self->tls_block));
    learn_self(self);
    adopt(self);
    finish(self, self->start(self->arg));
}

int pthread_create(pthread_t *thread, const pthread_attr_t *attr, void *(*start)(void *), void *arg) {
    size_t stack = attr != NULL && attr->stacksize != 0 ? attr->stacksize : DEFAULT_STACK;
    if (stack < PTHREAD_STACK_MIN || stack > MAX_STACK) {
        return EINVAL;
    }
    reap_zombies();
    struct freeos_thread *self = calloc(1, sizeof(*self));
    void *block = malloc(tls_block_bytes());
    if (self == NULL || block == NULL) {
        free(self);
        free(block);
        return EAGAIN;
    }
    self->start = start;
    self->arg = arg;
    self->tls_block = block;
    self->state = attr != NULL && attr->detachstate == PTHREAD_CREATE_DETACHED ? DETACHED : JOINABLE;

    long id = freeos_syscall(SYS_THREAD_CREATE, (long)thread_entry, (long)self, (long)stack);
    if (id < 0) {
        free(block);
        free(self);
        return EAGAIN;
    }
    *thread = self;
    return 0;
}

int pthread_join(pthread_t thread, void **result) {
    if (thread == current) {
        return EDEADLK;
    }
    if (load(&thread->state) == DETACHED) {
        return EINVAL;
    }
    while (load(&thread->done) == 0) {
        futex_wait(&thread->done, 0, 0);
    }
    if (result != NULL) {
        *result = thread->result;
    }
    if (thread != &main_thread) {
        free(thread->tls_block);
        free(thread);
    }
    return 0;
}

int pthread_detach(pthread_t thread) {
    unsigned int expected = JOINABLE;
    if (__atomic_compare_exchange_n(&thread->state, &expected, DETACHED, 0, __ATOMIC_ACQ_REL,
                                    __ATOMIC_ACQUIRE)) {
        return 0;
    }
    if (expected == DETACHED) {
        return EINVAL;
    }
    /* Уже завершился — освобождать больше некому, кроме нас. */
    return pthread_join(thread, NULL);
}

pthread_t pthread_self(void) { return current; }

int pthread_equal(pthread_t a, pthread_t b) { return a == b; }

void pthread_exit(void *result) { finish(current, result); }

int pthread_cancel(pthread_t thread) {
    (void)thread;
    return ENOSYS;
}

int pthread_kill(pthread_t thread, int signal) {
    (void)thread;
    (void)signal;
    return ENOSYS;
}

/* Сигналов в системе нет (см. `sigprocmask` в `syscalls.c`). Зовёт её
 * `sigsetjmp` picolibc, как только система объявила потоки. */
int pthread_sigmask(int how, const sigset_t *set, sigset_t *old) {
    (void)how;
    (void)set;
    (void)old;
    return ENOSYS;
}

/* ── Атрибуты потока ─────────────────────────────────────────────────────── */

int pthread_attr_init(pthread_attr_t *attr) {
    attr->stacksize = DEFAULT_STACK;
    attr->detachstate = PTHREAD_CREATE_JOINABLE;
    attr->stackaddr = NULL;
    return 0;
}

int pthread_attr_destroy(pthread_attr_t *attr) {
    (void)attr;
    return 0;
}

int pthread_attr_setstacksize(pthread_attr_t *attr, size_t size) {
    if (size < PTHREAD_STACK_MIN || size > MAX_STACK) {
        return EINVAL;
    }
    attr->stacksize = size;
    return 0;
}

int pthread_attr_getstacksize(const pthread_attr_t *attr, size_t *size) {
    *size = attr->stacksize;
    return 0;
}

int pthread_attr_setdetachstate(pthread_attr_t *attr, int state) {
    if (state != PTHREAD_CREATE_JOINABLE && state != PTHREAD_CREATE_DETACHED) {
        return EINVAL;
    }
    attr->detachstate = state;
    return 0;
}

int pthread_attr_getdetachstate(const pthread_attr_t *attr, int *state) {
    *state = attr->detachstate;
    return 0;
}

/* Атрибуты уже идущего потока — главное в них стек, который выделило ядро.
 * Поток узнал его сам при старте (`learn_self`), поэтому ответ есть и про
 * чужой поток, а не только про себя. */
int pthread_getattr_np(pthread_t thread, pthread_attr_t *attr) {
    if (thread == NULL || thread->stack_high == 0) {
        return EINVAL;
    }
    attr->stacksize = (size_t)(thread->stack_high - thread->stack_low);
    attr->stackaddr = (void *)thread->stack_low;
    attr->detachstate =
        load(&thread->state) == DETACHED ? PTHREAD_CREATE_DETACHED : PTHREAD_CREATE_JOINABLE;
    return 0;
}

/* Низ стека и его размер — как у POSIX: адрес — **нижний**, а не вершина. */
int pthread_attr_getstack(const pthread_attr_t *attr, void **addr, size_t *size) {
    if (attr->stackaddr == NULL) {
        return EINVAL;
    }
    *addr = attr->stackaddr;
    *size = attr->stacksize;
    return 0;
}

int pthread_getthreadid_np(void) { return current != NULL ? (int)current->id : -1; }

/* ── Мьютексы ────────────────────────────────────────────────────────────── */

/* Захватить слово замка — без учёта типа и владельца. */
static void word_lock(unsigned int *state) {
    unsigned int seen = 0;
    if (__atomic_compare_exchange_n(state, &seen, 1, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
        return;
    }
    if (seen != 2) {
        seen = __atomic_exchange_n(state, 2, __ATOMIC_ACQUIRE);
    }
    while (seen != 0) {
        futex_wait(state, 2, 0);
        seen = __atomic_exchange_n(state, 2, __ATOMIC_ACQUIRE);
    }
}

static void word_unlock(unsigned int *state) {
    if (__atomic_fetch_sub(state, 1, __ATOMIC_RELEASE) != 1) {
        store(state, 0);
        futex_wake(state, 1);
    }
}

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attr) {
    mutex->state = 0;
    mutex->type = attr != NULL ? attr->type : PTHREAD_MUTEX_DEFAULT;
    mutex->owner = NULL;
    mutex->count = 0;
    return 0;
}

int pthread_mutex_destroy(pthread_mutex_t *mutex) { return load(&mutex->state) == 0 ? 0 : EBUSY; }

int pthread_mutex_lock(pthread_mutex_t *mutex) {
    pthread_t self = current;
    if (__atomic_load_n(&mutex->owner, __ATOMIC_RELAXED) == self && self != NULL) {
        if (mutex->type == PTHREAD_MUTEX_RECURSIVE) {
            mutex->count++;
            return 0;
        }
        if (mutex->type == PTHREAD_MUTEX_ERRORCHECK) {
            return EDEADLK;
        }
    }
    word_lock(&mutex->state);
    __atomic_store_n(&mutex->owner, self, __ATOMIC_RELAXED);
    mutex->count = 1;
    return 0;
}

int pthread_mutex_trylock(pthread_mutex_t *mutex) {
    pthread_t self = current;
    if (mutex->type == PTHREAD_MUTEX_RECURSIVE && self != NULL &&
        __atomic_load_n(&mutex->owner, __ATOMIC_RELAXED) == self) {
        mutex->count++;
        return 0;
    }
    unsigned int seen = 0;
    if (!__atomic_compare_exchange_n(&mutex->state, &seen, 1, 0, __ATOMIC_ACQUIRE, __ATOMIC_RELAXED)) {
        return EBUSY;
    }
    __atomic_store_n(&mutex->owner, self, __ATOMIC_RELAXED);
    mutex->count = 1;
    return 0;
}

int pthread_mutex_unlock(pthread_mutex_t *mutex) {
    pthread_t self = current;
    if (mutex->type != PTHREAD_MUTEX_NORMAL && __atomic_load_n(&mutex->owner, __ATOMIC_RELAXED) != self) {
        return EPERM;
    }
    if (mutex->type == PTHREAD_MUTEX_RECURSIVE && --mutex->count > 0) {
        return 0;
    }
    __atomic_store_n(&mutex->owner, NULL, __ATOMIC_RELAXED);
    mutex->count = 0;
    word_unlock(&mutex->state);
    return 0;
}

int pthread_mutexattr_init(pthread_mutexattr_t *attr) {
    attr->type = PTHREAD_MUTEX_DEFAULT;
    return 0;
}

int pthread_mutexattr_destroy(pthread_mutexattr_t *attr) {
    (void)attr;
    return 0;
}

int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int type) {
    if (type != PTHREAD_MUTEX_NORMAL && type != PTHREAD_MUTEX_RECURSIVE && type != PTHREAD_MUTEX_ERRORCHECK) {
        return EINVAL;
    }
    attr->type = type;
    return 0;
}

int pthread_mutexattr_gettype(const pthread_mutexattr_t *attr, int *type) {
    *type = attr->type;
    return 0;
}

/* Общие между процессами объекты — отказ: процессы не делят памяти. */
int pthread_mutexattr_setpshared(pthread_mutexattr_t *attr, int shared) {
    (void)attr;
    return shared == PTHREAD_PROCESS_PRIVATE ? 0 : ENOSYS;
}

/* ── Условные переменные ─────────────────────────────────────────────────── */

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attr) {
    cond->seq = 0;
    cond->clock = attr != NULL ? attr->clock : CLOCK_REALTIME;
    return 0;
}

int pthread_cond_destroy(pthread_cond_t *cond) {
    (void)cond;
    return 0;
}

/* Сколько миллисекунд до `deadline` по часам `clock`, округлённо вверх; ноль —
 * срок уже прошёл. */
static unsigned long millis_until(clockid_t clock, const struct timespec *deadline) {
    struct timespec now;
    if (clock_gettime(clock, &now) != 0) {
        return 1;
    }
    long long left = (long long)(deadline->tv_sec - now.tv_sec) * 1000000000LL +
                     (long long)(deadline->tv_nsec - now.tv_nsec);
    if (left <= 0) {
        return 0;
    }
    return (unsigned long)((left + 999999) / 1000000);
}

/* Отпустить мьютекс, уснуть на счётчике сигналов и взять мьютекс обратно.
 * Рекурсивный мьютекс отпускается целиком и возвращается с тем же счётом. */
static int cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex, const struct timespec *deadline) {
    unsigned int seen = load(&cond->seq);
    unsigned int depth = mutex->count;
    unsigned long timeout = 0;
    if (deadline != NULL) {
        timeout = millis_until(cond->clock, deadline);
        if (timeout == 0) {
            return ETIMEDOUT;
        }
    }
    __atomic_store_n(&mutex->owner, NULL, __ATOMIC_RELAXED);
    mutex->count = 0;
    word_unlock(&mutex->state);

    long answer = futex_wait(&cond->seq, seen, timeout);

    word_lock(&mutex->state);
    __atomic_store_n(&mutex->owner, current, __ATOMIC_RELAXED);
    mutex->count = depth;
    if (deadline != NULL && (answer == FREEOS_FUTEX_TIMED_OUT || millis_until(cond->clock, deadline) == 0)) {
        return ETIMEDOUT;
    }
    return 0;
}

int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex) { return cond_wait(cond, mutex, NULL); }

int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex, const struct timespec *deadline) {
    if (deadline->tv_nsec < 0 || deadline->tv_nsec >= 1000000000L) {
        return EINVAL;
    }
    return cond_wait(cond, mutex, deadline);
}

int pthread_cond_signal(pthread_cond_t *cond) {
    __atomic_fetch_add(&cond->seq, 1, __ATOMIC_RELEASE);
    futex_wake(&cond->seq, 1);
    return 0;
}

int pthread_cond_broadcast(pthread_cond_t *cond) {
    __atomic_fetch_add(&cond->seq, 1, __ATOMIC_RELEASE);
    futex_wake(&cond->seq, 0);
    return 0;
}

int pthread_condattr_init(pthread_condattr_t *attr) {
    attr->clock = CLOCK_REALTIME;
    return 0;
}

int pthread_condattr_destroy(pthread_condattr_t *attr) {
    (void)attr;
    return 0;
}

int pthread_condattr_setclock(pthread_condattr_t *attr, clockid_t clock) {
    if (clock != CLOCK_REALTIME && clock != CLOCK_MONOTONIC) {
        return EINVAL;
    }
    attr->clock = clock;
    return 0;
}

int pthread_condattr_getclock(const pthread_condattr_t *attr, clockid_t *clock) {
    *clock = attr->clock;
    return 0;
}

int pthread_condattr_setpshared(pthread_condattr_t *attr, int shared) {
    (void)attr;
    return shared == PTHREAD_PROCESS_PRIVATE ? 0 : ENOSYS;
}

/* ── Блокировки чтения-записи ────────────────────────────────────────────── */

/* Писатель в приоритете: читатель, пришедший, пока писатель ждёт, встаёт за
 * ним. Иначе поток читателей, сменяющих друг друга, не дал бы писателю войти
 * никогда. */

int pthread_rwlock_init(pthread_rwlock_t *lock, const pthread_rwlockattr_t *attr) {
    (void)attr;
    pthread_mutex_init(&lock->lock, NULL);
    pthread_cond_init(&lock->readable, NULL);
    pthread_cond_init(&lock->writable, NULL);
    lock->readers = 0;
    lock->writer = 0;
    lock->waiting_writers = 0;
    return 0;
}

int pthread_rwlock_destroy(pthread_rwlock_t *lock) {
    return lock->readers != 0 || lock->writer != 0 ? EBUSY : 0;
}

int pthread_rwlock_rdlock(pthread_rwlock_t *lock) {
    pthread_mutex_lock(&lock->lock);
    while (lock->writer || lock->waiting_writers > 0) {
        pthread_cond_wait(&lock->readable, &lock->lock);
    }
    lock->readers++;
    pthread_mutex_unlock(&lock->lock);
    return 0;
}

int pthread_rwlock_tryrdlock(pthread_rwlock_t *lock) {
    int answer = 0;
    pthread_mutex_lock(&lock->lock);
    if (lock->writer || lock->waiting_writers > 0) {
        answer = EBUSY;
    } else {
        lock->readers++;
    }
    pthread_mutex_unlock(&lock->lock);
    return answer;
}

int pthread_rwlock_wrlock(pthread_rwlock_t *lock) {
    pthread_mutex_lock(&lock->lock);
    lock->waiting_writers++;
    while (lock->writer || lock->readers > 0) {
        pthread_cond_wait(&lock->writable, &lock->lock);
    }
    lock->waiting_writers--;
    lock->writer = 1;
    pthread_mutex_unlock(&lock->lock);
    return 0;
}

int pthread_rwlock_trywrlock(pthread_rwlock_t *lock) {
    int answer = 0;
    pthread_mutex_lock(&lock->lock);
    if (lock->writer || lock->readers > 0) {
        answer = EBUSY;
    } else {
        lock->writer = 1;
    }
    pthread_mutex_unlock(&lock->lock);
    return answer;
}

int pthread_rwlock_unlock(pthread_rwlock_t *lock) {
    pthread_mutex_lock(&lock->lock);
    if (lock->writer) {
        lock->writer = 0;
    } else if (lock->readers > 0) {
        lock->readers--;
    }
    if (lock->waiting_writers > 0) {
        if (lock->readers == 0) {
            pthread_cond_signal(&lock->writable);
        }
    } else {
        pthread_cond_broadcast(&lock->readable);
    }
    pthread_mutex_unlock(&lock->lock);
    return 0;
}

/* ── Ключи ───────────────────────────────────────────────────────────────── */

static pthread_mutex_t keys_lock = PTHREAD_MUTEX_INITIALIZER;
static unsigned char key_used[PTHREAD_KEYS_MAX];
static void (*key_destructor[PTHREAD_KEYS_MAX])(void *);

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *)) {
    pthread_mutex_lock(&keys_lock);
    for (unsigned int index = 0; index < PTHREAD_KEYS_MAX; index++) {
        if (!key_used[index]) {
            key_used[index] = 1;
            key_destructor[index] = destructor;
            pthread_mutex_unlock(&keys_lock);
            *key = index;
            return 0;
        }
    }
    pthread_mutex_unlock(&keys_lock);
    return EAGAIN;
}

int pthread_key_delete(pthread_key_t key) {
    if (key >= PTHREAD_KEYS_MAX) {
        return EINVAL;
    }
    pthread_mutex_lock(&keys_lock);
    key_used[key] = 0;
    key_destructor[key] = NULL;
    pthread_mutex_unlock(&keys_lock);
    return 0;
}

void *pthread_getspecific(pthread_key_t key) {
    return key < PTHREAD_KEYS_MAX ? (void *)current->specific[key] : NULL;
}

int pthread_setspecific(pthread_key_t key, const void *value) {
    if (key >= PTHREAD_KEYS_MAX) {
        return EINVAL;
    }
    current->specific[key] = value;
    return 0;
}

/* Деструкторы ключей: по кругу, пока значения появляются заново, но не
 * больше `PTHREAD_DESTRUCTOR_ITERATIONS` раз — как требует POSIX. */
static void run_destructors(struct freeos_thread *self) {
    for (int round = 0; round < PTHREAD_DESTRUCTOR_ITERATIONS; round++) {
        int called = 0;
        for (unsigned int key = 0; key < PTHREAD_KEYS_MAX; key++) {
            void *value = (void *)self->specific[key];
            void (*destructor)(void *) = key_destructor[key];
            if (value != NULL && destructor != NULL) {
                self->specific[key] = NULL;
                destructor(value);
                called = 1;
            }
        }
        if (!called) {
            return;
        }
    }
}

/* ── Однократный вызов ───────────────────────────────────────────────────── */

int pthread_once(pthread_once_t *once, void (*init)(void)) {
    unsigned int seen = 0;
    if (__atomic_compare_exchange_n(&once->state, &seen, 1, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE)) {
        init();
        store(&once->state, 2);
        futex_wake(&once->state, 0);
        return 0;
    }
    while (load(&once->state) != 2) {
        futex_wait(&once->state, 1, 0);
    }
    return 0;
}

/* ── Блокировки picolibc ─────────────────────────────────────────────────── */

/* `sys/lock.h` объявляет тип замка неполным и оставляет его системе. */
struct __lock {
    pthread_mutex_t mutex;
};

/* Единственный статический замок этой picolibc: им она закрывает и `malloc`, и
 * список открытых `FILE`, и `atexit`, и окружение. Документация в её
 * `lock.c` перечисляет семь раздельных — от newlib; в коде их больше нет.
 * Недостающий символ компоновщик взял бы из её пустой заглушки, и та
 * принесла бы с собой вторые определения всех функций ниже — ошибка
 * компоновки, а не тихая подмена. */
struct __lock __lock___libc_recursive_mutex = {PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP};

/* Замки, которые picolibc заводит сама (например, у каждого `FILE`). Память —
 * `malloc`: его собственный замок статический, выше, и в эту функцию не
 * приходит. Не нашлось памяти — замка не будет, и захват пустого замка
 * ничего не делает: хуже порчи он не станет, а отказать здесь picolibc
 * нечем. */
static void lock_init(_LOCK_T *lock, int type) {
    struct __lock *made = malloc(sizeof(*made));
    if (made != NULL) {
        pthread_mutexattr_t attr = {type};
        pthread_mutex_init(&made->mutex, &attr);
    }
    *lock = made;
}

void __retarget_lock_init(_LOCK_T *lock) { lock_init(lock, PTHREAD_MUTEX_NORMAL); }

void __retarget_lock_init_recursive(_LOCK_T *lock) { lock_init(lock, PTHREAD_MUTEX_RECURSIVE); }

void __retarget_lock_close(_LOCK_T lock) { free(lock); }

void __retarget_lock_close_recursive(_LOCK_T lock) { free(lock); }

void __retarget_lock_acquire(_LOCK_T lock) {
    if (lock != NULL) {
        pthread_mutex_lock(&lock->mutex);
    }
}

void __retarget_lock_acquire_recursive(_LOCK_T lock) { __retarget_lock_acquire(lock); }

void __retarget_lock_release(_LOCK_T lock) {
    if (lock != NULL) {
        pthread_mutex_unlock(&lock->mutex);
    }
}

void __retarget_lock_release_recursive(_LOCK_T lock) { __retarget_lock_release(lock); }

/* ── Планирование (фаза 58b) ─────────────────────────────────────────────── */

int pthread_getschedparam(pthread_t thread, int *policy, struct sched_param *param) {
    (void)thread;
    *policy = SCHED_OTHER;
    param->sched_priority = 0;
    return 0;
}

int pthread_setschedparam(pthread_t thread, int policy, const struct sched_param *param) {
    (void)thread;
    if (policy == SCHED_OTHER && param->sched_priority == 0) {
        return 0;
    }
    return ENOTSUP;
}

int sched_get_priority_max(int policy) {
    if (policy == SCHED_OTHER) {
        return 0;
    }
    errno = EINVAL;
    return -1;
}

int sched_get_priority_min(int policy) { return sched_get_priority_max(policy); }
