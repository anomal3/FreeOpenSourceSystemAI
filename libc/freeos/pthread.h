/* Потоки POSIX для FreeOS (фаза 57).
 *
 * picolibc своего `pthread.h` не поставляет: это библиотека для систем без
 * потоков, и всё, что она о них знает, — восемь функций блокировки, которые
 * обязана дать система (`sys/lock.h`). Потоки дают ядро (`SYS_THREAD_CREATE`,
 * `SYS_SET_TLS`, `SYS_FUTEX_WAIT` со сроком) и этот слой
 * (`libc/freeos/threads.c`).
 *
 * # Что есть и чего нет
 *
 * Есть то, на чём стоит обычная многопоточная программа на C и чужая среда
 * исполнения: потоки с ожиданием и отсоединением, мьютексы (обычные,
 * рекурсивные, с проверкой ошибок), условные переменные с часами на выбор,
 * блокировки чтения-записи, ключи с деструкторами, `pthread_once`.
 *
 * Нет — и ответ `ENOSYS`, а не молчаливый успех: отмены потоков
 * (`pthread_cancel`), приоритетов и политик планирования, привязки к
 * процессору, общих между процессами объектов (`PTHREAD_PROCESS_SHARED`),
 * сигналов потоку (`pthread_kill`). Причина у всех одна: под ними нет
 * механизма в ядре, и притворяться, что он есть, значило бы получить
 * программу, которая думает, что отменила поток, пока тот работает дальше.
 *
 * Раскладки структур — наши и публичны только ради статических
 * инициализаторов. Программа, лезущая в поля, завязывается на эту libc.
 */

#ifndef FREEOS_PTHREAD_H
#define FREEOS_PTHREAD_H

#include <sched.h>
#include <stddef.h>
#include <sys/_sigset.h>
#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct freeos_thread *pthread_t;

typedef struct {
    size_t stacksize;
    int detachstate;
} pthread_attr_t;

/* Слово `state` — то, на чём спят: 0 свободен, 1 занят, 2 занят и кто-то ждёт. */
typedef struct {
    unsigned int state;
    int type;
    pthread_t owner;
    unsigned int count;
} pthread_mutex_t;

typedef struct {
    int type;
} pthread_mutexattr_t;

/* `seq` растёт на каждом сигнале; ждущий спит, пока оно то, что он видел. */
typedef struct {
    unsigned int seq;
    int clock;
} pthread_cond_t;

typedef struct {
    int clock;
} pthread_condattr_t;

typedef struct {
    pthread_mutex_t lock;
    pthread_cond_t readable;
    pthread_cond_t writable;
    int readers;
    int writer;
    int waiting_writers;
} pthread_rwlock_t;

typedef struct {
    int unused;
} pthread_rwlockattr_t;

typedef unsigned int pthread_key_t;

/* 0 — не звали, 1 — идёт, 2 — сделано. */
typedef struct {
    unsigned int state;
} pthread_once_t;

#define PTHREAD_CREATE_JOINABLE 0
#define PTHREAD_CREATE_DETACHED 1

#define PTHREAD_MUTEX_NORMAL 0
#define PTHREAD_MUTEX_RECURSIVE 1
#define PTHREAD_MUTEX_ERRORCHECK 2
#define PTHREAD_MUTEX_DEFAULT PTHREAD_MUTEX_NORMAL

#define PTHREAD_PROCESS_PRIVATE 0
#define PTHREAD_PROCESS_SHARED 1

#define PTHREAD_MUTEX_INITIALIZER {0, PTHREAD_MUTEX_NORMAL, 0, 0}
#define PTHREAD_RECURSIVE_MUTEX_INITIALIZER_NP {0, PTHREAD_MUTEX_RECURSIVE, 0, 0}
#define PTHREAD_COND_INITIALIZER {0, CLOCK_REALTIME}
#define PTHREAD_RWLOCK_INITIALIZER \
    {PTHREAD_MUTEX_INITIALIZER, PTHREAD_COND_INITIALIZER, PTHREAD_COND_INITIALIZER, 0, 0, 0}
#define PTHREAD_ONCE_INIT {0}

/* Сколько ключей может завести программа. */
#define PTHREAD_KEYS_MAX 128
#define PTHREAD_DESTRUCTOR_ITERATIONS 4
/* Меньше стек потока не бывает: страница под сторожем ядра — не в счёт. */
#define PTHREAD_STACK_MIN 16384

int pthread_create(pthread_t *thread, const pthread_attr_t *attr, void *(*start)(void *), void *arg);
int pthread_join(pthread_t thread, void **result);
int pthread_detach(pthread_t thread);
pthread_t pthread_self(void);
int pthread_equal(pthread_t a, pthread_t b);
void pthread_exit(void *result) __attribute__((noreturn));

int pthread_attr_init(pthread_attr_t *attr);
int pthread_attr_destroy(pthread_attr_t *attr);
int pthread_attr_setstacksize(pthread_attr_t *attr, size_t size);
int pthread_attr_getstacksize(const pthread_attr_t *attr, size_t *size);
int pthread_attr_setdetachstate(pthread_attr_t *attr, int state);
int pthread_attr_getdetachstate(const pthread_attr_t *attr, int *state);

int pthread_mutex_init(pthread_mutex_t *mutex, const pthread_mutexattr_t *attr);
int pthread_mutex_destroy(pthread_mutex_t *mutex);
int pthread_mutex_lock(pthread_mutex_t *mutex);
int pthread_mutex_trylock(pthread_mutex_t *mutex);
int pthread_mutex_unlock(pthread_mutex_t *mutex);
int pthread_mutexattr_init(pthread_mutexattr_t *attr);
int pthread_mutexattr_destroy(pthread_mutexattr_t *attr);
int pthread_mutexattr_settype(pthread_mutexattr_t *attr, int type);
int pthread_mutexattr_gettype(const pthread_mutexattr_t *attr, int *type);
int pthread_mutexattr_setpshared(pthread_mutexattr_t *attr, int shared);

int pthread_cond_init(pthread_cond_t *cond, const pthread_condattr_t *attr);
int pthread_cond_destroy(pthread_cond_t *cond);
int pthread_cond_wait(pthread_cond_t *cond, pthread_mutex_t *mutex);
int pthread_cond_timedwait(pthread_cond_t *cond, pthread_mutex_t *mutex, const struct timespec *deadline);
int pthread_cond_signal(pthread_cond_t *cond);
int pthread_cond_broadcast(pthread_cond_t *cond);
int pthread_condattr_init(pthread_condattr_t *attr);
int pthread_condattr_destroy(pthread_condattr_t *attr);
int pthread_condattr_setclock(pthread_condattr_t *attr, clockid_t clock);
int pthread_condattr_getclock(const pthread_condattr_t *attr, clockid_t *clock);
int pthread_condattr_setpshared(pthread_condattr_t *attr, int shared);

int pthread_rwlock_init(pthread_rwlock_t *lock, const pthread_rwlockattr_t *attr);
int pthread_rwlock_destroy(pthread_rwlock_t *lock);
int pthread_rwlock_rdlock(pthread_rwlock_t *lock);
int pthread_rwlock_tryrdlock(pthread_rwlock_t *lock);
int pthread_rwlock_wrlock(pthread_rwlock_t *lock);
int pthread_rwlock_trywrlock(pthread_rwlock_t *lock);
int pthread_rwlock_unlock(pthread_rwlock_t *lock);

int pthread_key_create(pthread_key_t *key, void (*destructor)(void *));
int pthread_key_delete(pthread_key_t key);
void *pthread_getspecific(pthread_key_t key);
int pthread_setspecific(pthread_key_t key, const void *value);

int pthread_once(pthread_once_t *once, void (*init)(void));

/* Нет механизма в ядре — `ENOSYS` (см. заголовок файла). */
int pthread_cancel(pthread_t thread);
int pthread_kill(pthread_t thread, int signal);
int pthread_sigmask(int how, const __sigset_t *set, __sigset_t *old);

#ifdef __cplusplus
}
#endif

#endif
