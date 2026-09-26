/* Семафоры POSIX для FreeOS (фаза 58).
 *
 * Безымянные — `sem_init` на памяти программы, поверх ожидания на адресе
 * (`SYS_FUTEX_WAIT`). Именованных (`sem_open`) нет — ответ `ENOSYS`: им нужно
 * общее между процессами имя, а процессы в этой системе памяти не делят.
 *
 * Без этого заголовка Mono уходит в ветку Win32 (`mono-os-semaphore.h`) и
 * даёт 2600 ошибок сборки из 2671 — так их и нашли (27.09.2026). */

#ifndef FREEOS_SEMAPHORE_H
#define FREEOS_SEMAPHORE_H

#include <time.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Слово `value` — число свободных единиц; на нём и спят. */
typedef struct {
    unsigned int value;
    unsigned int waiters;
} sem_t;

#define SEM_FAILED ((sem_t *)0)

int sem_init(sem_t *sem, int shared, unsigned int value);
int sem_destroy(sem_t *sem);
int sem_wait(sem_t *sem);
int sem_trywait(sem_t *sem);
int sem_timedwait(sem_t *sem, const struct timespec *deadline);
int sem_post(sem_t *sem);
int sem_getvalue(sem_t *sem, int *value);
sem_t *sem_open(const char *name, int flags, ...);
int sem_close(sem_t *sem);
int sem_unlink(const char *name);

#ifdef __cplusplus
}
#endif

#endif
