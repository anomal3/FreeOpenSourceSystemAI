/* Потоки POSIX в программе на C (фаза 57).
 *
 * # Что она доказывает
 *
 * Что обычная многопоточная программа на C — та, что написана под Linux и
 * собирается нашим набором без правок, — работает. Каждая проверка ловит свою
 * поломку, и каждая устроена так, чтобы на сломанной системе она **не могла**
 * пройти случайно:
 *
 * - счётчик под мьютексом — **неатомарный**: атомарный сошёлся бы и без
 *   мьютекса;
 * - `errno` и `__thread` — у каждого свой: поток кладёт своё, засыпает, чтобы
 *   планировщик успел поставить на процессор соседа, и читает обратно;
 * - `malloc` из четырёх потоков разом с проверкой узора: однопоточная куча
 *   раздала бы один блок двоим, и узор бы разошёлся;
 * - условная переменная со сроком обязана вернуть `ETIMEDOUT` не раньше
 *   срока — по монотонным часам;
 * - `pthread_once` при четырёх одновременных — ровно один вызов;
 * - деструкторы ключей зовутся на выходе потока;
 * - отсоединённый поток доживает сам и память за собой не держит.
 *
 * Итог — одна строка с числом проваленных; ноль и только ноль — всё прошло.
 */

#include <errno.h>
#include <pthread.h>
#include <semaphore.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define THREADS 4

static int failures;

static void ok(const char *what) { printf("cthreads: ok %s\n", what); }

static void fail(const char *what) {
    printf("cthreads: FAILED %s\n", what);
    failures++;
}

static void nap(long ms) {
    struct timespec pause = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&pause, NULL);
}

static long long now_ms(clockid_t clock) {
    struct timespec now;
    clock_gettime(clock, &now);
    return (long long)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}

/* ── Мьютекс над неатомарным счётчиком ───────────────────────────────────── */

#define STEPS 20000

static pthread_mutex_t counter_lock = PTHREAD_MUTEX_INITIALIZER;
static volatile long counter;

static void *count_up(void *arg) {
    (void)arg;
    for (int step = 0; step < STEPS; step++) {
        pthread_mutex_lock(&counter_lock);
        long seen = counter;
        /* Уступка под замком делает встречу потоков неизбежной, а не
         * вероятной: сосед обязан упереться в занятый замок. */
        if (step % 1000 == 0) {
            sched_yield();
        }
        counter = seen + 1;
        pthread_mutex_unlock(&counter_lock);
    }
    return NULL;
}

static void check_mutex(void) {
    pthread_t threads[THREADS];
    for (int index = 0; index < THREADS; index++) {
        if (pthread_create(&threads[index], NULL, count_up, NULL) != 0) {
            fail("pthread_create");
            return;
        }
    }
    for (int index = 0; index < THREADS; index++) {
        pthread_join(threads[index], NULL);
    }
    printf("cthreads: counter is %ld of %d\n", counter, THREADS * STEPS);
    if (counter == THREADS * STEPS) {
        ok("mutex keeps a plain counter exact");
    } else {
        fail("mutex keeps a plain counter exact");
    }
}

/* ── errno и __thread ────────────────────────────────────────────────────── */

static __thread int own_mark = 7;
static int storage_wrong;
static pthread_mutex_t storage_lock = PTHREAD_MUTEX_INITIALIZER;

static void *keep_own(void *arg) {
    int index = (int)(intptr_t)arg;
    /* Начальное значение из `.tdata` — у каждого своё, скопированное. */
    int initial = own_mark;
    own_mark = 100 + index;
    errno = 1000 + index;
    nap(50);
    int wrong = initial != 7 || own_mark != 100 + index || errno != 1000 + index;
    pthread_mutex_lock(&storage_lock);
    storage_wrong += wrong;
    pthread_mutex_unlock(&storage_lock);
    return NULL;
}

static void check_storage(void) {
    pthread_t threads[THREADS];
    for (int index = 0; index < THREADS; index++) {
        pthread_create(&threads[index], NULL, keep_own, (void *)(intptr_t)index);
    }
    for (int index = 0; index < THREADS; index++) {
        pthread_join(threads[index], NULL);
    }
    if (storage_wrong == 0 && own_mark == 7) {
        ok("errno and __thread are per thread");
    } else {
        printf("cthreads: %d thread(s) saw someone else's storage\n", storage_wrong);
        fail("errno and __thread are per thread");
    }
}

/* ── Куча из четырёх потоков ─────────────────────────────────────────────── */

static int heap_wrong;

static void *churn(void *arg) {
    unsigned int seed = (unsigned int)(uintptr_t)arg * 2654435761u;
    unsigned char *blocks[32] = {0};
    size_t sizes[32] = {0};
    int wrong = 0;
    for (int round = 0; round < 3000; round++) {
        seed = seed * 1103515245u + 12345u;
        int slot = (int)(seed >> 16) % 32;
        if (blocks[slot] != NULL) {
            unsigned char pattern = (unsigned char)(slot * 7 + (int)(uintptr_t)arg);
            for (size_t at = 0; at < sizes[slot]; at++) {
                if (blocks[slot][at] != pattern) {
                    wrong = 1;
                    break;
                }
            }
            free(blocks[slot]);
            blocks[slot] = NULL;
        } else {
            sizes[slot] = 16 + (seed >> 8) % 4000;
            blocks[slot] = malloc(sizes[slot]);
            if (blocks[slot] == NULL) {
                wrong = 1;
                continue;
            }
            memset(blocks[slot], slot * 7 + (int)(uintptr_t)arg, sizes[slot]);
        }
    }
    for (int slot = 0; slot < 32; slot++) {
        free(blocks[slot]);
    }
    __atomic_fetch_add(&heap_wrong, wrong, __ATOMIC_RELAXED);
    return NULL;
}

static void check_heap(void) {
    pthread_t threads[THREADS];
    for (int index = 0; index < THREADS; index++) {
        pthread_create(&threads[index], NULL, churn, (void *)(intptr_t)(index + 1));
    }
    for (int index = 0; index < THREADS; index++) {
        pthread_join(threads[index], NULL);
    }
    if (heap_wrong == 0) {
        ok("malloc from four threads at once");
    } else {
        fail("malloc from four threads at once");
    }
}

/* ── Условная переменная: очередь и срок ─────────────────────────────────── */

static pthread_mutex_t queue_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t queue_ready = PTHREAD_COND_INITIALIZER;
static int queue[8];
static int queue_len;
static long consumed_sum;

#define ITEMS 500

static void *consume(void *arg) {
    (void)arg;
    for (int taken = 0; taken < ITEMS; taken++) {
        pthread_mutex_lock(&queue_lock);
        while (queue_len == 0) {
            pthread_cond_wait(&queue_ready, &queue_lock);
        }
        consumed_sum += queue[--queue_len];
        pthread_cond_broadcast(&queue_ready);
        pthread_mutex_unlock(&queue_lock);
    }
    return NULL;
}

static void check_cond(void) {
    pthread_t consumer;
    pthread_create(&consumer, NULL, consume, NULL);
    for (int item = 1; item <= ITEMS; item++) {
        pthread_mutex_lock(&queue_lock);
        while (queue_len == 8) {
            pthread_cond_wait(&queue_ready, &queue_lock);
        }
        queue[queue_len++] = item;
        pthread_cond_broadcast(&queue_ready);
        pthread_mutex_unlock(&queue_lock);
    }
    pthread_join(consumer, NULL);
    if (consumed_sum == (long)ITEMS * (ITEMS + 1) / 2) {
        ok("condition variable carries a queue");
    } else {
        printf("cthreads: consumed %ld\n", consumed_sum);
        fail("condition variable carries a queue");
    }

    /* Срок: никто не сигналит, ответ обязан быть ETIMEDOUT и не раньше. */
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_t silent;
    pthread_cond_init(&silent, &attr);
    struct timespec deadline;
    clock_gettime(CLOCK_MONOTONIC, &deadline);
    deadline.tv_nsec += 200 * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000L;
    }
    long long started = now_ms(CLOCK_MONOTONIC);
    pthread_mutex_lock(&queue_lock);
    int answer = 0;
    while (answer == 0) {
        answer = pthread_cond_timedwait(&silent, &queue_lock, &deadline);
    }
    pthread_mutex_unlock(&queue_lock);
    long long waited = now_ms(CLOCK_MONOTONIC) - started;
    printf("cthreads: the timed wait took %lld ms\n", waited);
    if (answer == ETIMEDOUT && waited >= 190) {
        ok("timed wait ends at its deadline");
    } else {
        fail("timed wait ends at its deadline");
    }
}

/* ── pthread_once, ключи, отсоединённый поток ────────────────────────────── */

static pthread_once_t once = PTHREAD_ONCE_INIT;
static int once_calls;

static void init_once(void) {
    nap(20);
    __atomic_fetch_add(&once_calls, 1, __ATOMIC_RELAXED);
}

static pthread_key_t key;
static int destructed;

static void destroy(void *value) {
    free(value);
    __atomic_fetch_add(&destructed, 1, __ATOMIC_RELAXED);
}

static void *race_once(void *arg) {
    (void)arg;
    pthread_once(&once, init_once);
    pthread_setspecific(key, malloc(16));
    return NULL;
}

static int detached_ran;

static void *detached(void *arg) {
    (void)arg;
    __atomic_store_n(&detached_ran, 1, __ATOMIC_RELEASE);
    return NULL;
}

static void check_once_and_keys(void) {
    if (pthread_key_create(&key, destroy) != 0) {
        fail("pthread_key_create");
        return;
    }
    pthread_t threads[THREADS];
    for (int index = 0; index < THREADS; index++) {
        pthread_create(&threads[index], NULL, race_once, NULL);
    }
    for (int index = 0; index < THREADS; index++) {
        pthread_join(threads[index], NULL);
    }
    if (once_calls == 1) {
        ok("pthread_once runs once");
    } else {
        printf("cthreads: init ran %d time(s)\n", once_calls);
        fail("pthread_once runs once");
    }
    if (destructed == THREADS) {
        ok("key destructors run at thread exit");
    } else {
        printf("cthreads: %d destructor call(s)\n", destructed);
        fail("key destructors run at thread exit");
    }

    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setdetachstate(&attr, PTHREAD_CREATE_DETACHED);
    pthread_t loose;
    if (pthread_create(&loose, &attr, detached, NULL) != 0) {
        fail("detached thread");
        return;
    }
    for (int tries = 0; tries < 200 && !__atomic_load_n(&detached_ran, __ATOMIC_ACQUIRE); tries++) {
        nap(10);
    }
    if (detached_ran) {
        ok("a detached thread runs on its own");
    } else {
        fail("a detached thread runs on its own");
    }
}

/* ── Семафор (фаза 58) ───────────────────────────────────────────────────── */

/* Производитель кладёт сто единиц по одной, потребитель в другом потоке
 * забирает их, засыпая на нуле. Сумма забранного обязана сойтись, а
 * `sem_timedwait` на пустом — вернуть ETIMEDOUT не раньше срока. */
static sem_t items;
static volatile int taken_items;

static void *take_items(void *arg) {
    (void)arg;
    for (int item = 0; item < 100; item++) {
        sem_wait(&items);
        taken_items++;
    }
    return NULL;
}

static void check_semaphore(void) {
    sem_init(&items, 0, 0);
    pthread_t consumer;
    pthread_create(&consumer, NULL, take_items, NULL);
    for (int item = 0; item < 100; item++) {
        sem_post(&items);
        if (item % 10 == 0) {
            nap(1);
        }
    }
    pthread_join(consumer, NULL);
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_nsec += 100 * 1000000L;
    if (deadline.tv_nsec >= 1000000000L) {
        deadline.tv_sec++;
        deadline.tv_nsec -= 1000000000L;
    }
    long long started = now_ms(CLOCK_MONOTONIC);
    int answer = sem_timedwait(&items, &deadline);
    long long waited = now_ms(CLOCK_MONOTONIC) - started;
    if (taken_items == 100 && answer == -1 && errno == ETIMEDOUT && waited >= 90) {
        ok("semaphore hands over items and times out");
    } else {
        printf("cthreads: taken %d, answer %d, errno %d, waited %lld ms\n", taken_items, answer, errno, waited);
        fail("semaphore hands over items and times out");
    }
}

int main(void) {
    printf("cthreads: starting %d threads at a time\n", THREADS);
    check_mutex();
    check_storage();
    check_heap();
    check_cond();
    check_once_and_keys();
    check_semaphore();
    printf("cthreads: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
