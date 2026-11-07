/* Типы потоков POSIX для FreeOS — отдельно от функций (N4, 08.11.2026).
 *
 * POSIX требует, чтобы `pthread_t`, `pthread_mutex_t`, `pthread_rwlock_t` и
 * остальные типы были видны уже из `<sys/types.h>`, а не только из
 * `<pthread.h>`. Чужой код на это опирается: aws-lc (криптография TLS в
 * Servo) объявляет `typedef pthread_rwlock_t CRYPTO_MUTEX` в публичном
 * заголовке, не подключая `<pthread.h>` (так на musl, BSD и Android).
 * Поэтому типы здесь, а подключают их и `<pthread.h>`, и `<sys/types.h>`
 * (через `<machine/types.h>` — крючок picolibc для типов цели).
 *
 * Раскладки — наши (`libc/freeos/threads.c`), публичны только ради
 * статических инициализаторов в `<pthread.h>`.
 */

#ifndef FREEOS_SYS_PTHREADTYPES_H
#define FREEOS_SYS_PTHREADTYPES_H

typedef struct freeos_thread *pthread_t;

/* `stackaddr` — низ стека; заполняет только `pthread_getattr_np` (стеки
 * выделяет ядро, свой стек потоку не передать). */
typedef struct {
    __SIZE_TYPE__ stacksize;
    int detachstate;
    void *stackaddr;
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

#endif
