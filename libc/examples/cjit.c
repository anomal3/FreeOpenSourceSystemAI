/* Память кода: программа пишет себе функцию и исполняет её (фаза 59).
 *
 * # Что она доказывает
 *
 * Что чужая среда исполнения, которая пишет машинный код в память и тут же
 * его вызывает (Mono: трамплины, мост «интерпретатор → машинный код»), может
 * это делать — **и что W^X при этом остаётся правилом системы**: страница
 * памяти кода бывает то записываемой, то исполняемой, но не той и другой
 * разом, а переключает её ядро по обращению. Каждая проверка устроена так,
 * чтобы на сломанной системе она **не могла** пройти случайно:
 *
 * - функция переписывается и вызывается тысячу раз подряд, и каждый вызов
 *   обязан вернуть **новое** число: на AArch64 без чистки кешей процессор
 *   исполнил бы прежние инструкции, и число отстало бы;
 * - второй поток всё это время вызывает ту же функцию, а первый её
 *   переписывает: числа, которые видит второй, обязаны лежать в диапазоне
 *   записанных и не убывать, а после конца записи — совпасть с последним;
 * - «писать и исполнять» без `MAP_JIT` — отказ, как и был, а права памяти
 *   кода `mprotect` не меняет: у них уже есть хозяин.
 *
 * Итог — одна строка с числом проваленных; ноль и только ноль — всё прошло.
 */

#include <errno.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

static int failures;

static void ok(const char *what) { printf("cjit: ok %s\n", what); }

static void fail(const char *what) {
    printf("cjit: FAILED %s\n", what);
    failures++;
}

static void nap(long ms) {
    struct timespec pause = {ms / 1000, (ms % 1000) * 1000000L};
    nanosleep(&pause, NULL);
}

/* ── Функция `int f(void) { return value; }` одним словом ───────────────── */

/* Восемь байт кода, записываемые **одной** выровненной записью: поток,
 * исполняющий функцию, пока её переписывают, обязан увидеть либо старую
 * функцию целиком, либо новую — половины инструкции ему не достанется. */
static uint64_t encode(uint32_t value) {
#if defined(__x86_64__)
    /* mov eax, imm32; ret; nop; nop */
    return 0xB8ULL | ((uint64_t)value << 8) | (0xC3ULL << 40) | (0x90ULL << 48) | (0x90ULL << 56);
#elif defined(__aarch64__)
    /* movz w0, #imm16; ret */
    uint64_t movz = 0x52800000ULL | ((uint64_t)(value & 0xFFFF) << 5);
    return movz | (0xD65F03C0ULL << 32);
#else
#error "cjit знает только x86-64 и AArch64"
#endif
}

typedef int (*function)(void);

static volatile uint64_t *code;

static void write_function(uint32_t value) { *code = encode(value); }

static int call_function(void) {
    /* Указатель берётся заново на каждом вызове: компилятору нельзя ни
     * запомнить результат, ни вынести вызов из цикла. */
    function f = (function)(uintptr_t)code;
    return f();
}

/* ── Запреты ─────────────────────────────────────────────────────────────── */

static void check_refusals(size_t page) {
    void *both = mmap(NULL, page, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (both == MAP_FAILED && errno == EACCES) {
        ok("write and execute without MAP_JIT is refused");
    } else {
        fail("write and execute without MAP_JIT is refused");
    }
    if (mprotect((void *)code, page, PROT_READ) != 0) {
        ok("mprotect leaves code memory to the kernel");
    } else {
        fail("mprotect leaves code memory to the kernel");
    }
}

/* ── Один поток: записал — вызвал ────────────────────────────────────────── */

#define REWRITES 1000

static void check_single(void) {
    write_function(42);
    int first = call_function();
    write_function(43);
    int second = call_function();
    if (first == 42 && second == 43) {
        ok("a written function runs and a rewritten one runs anew");
    } else {
        printf("cjit: got %d and %d\n", first, second);
        fail("a written function runs and a rewritten one runs anew");
    }

    int stale = 0;
    for (uint32_t i = 0; i < REWRITES; i++) {
        write_function(1000 + i);
        if (call_function() != (int)(1000 + i)) {
            stale++;
        }
    }
    if (stale == 0) {
        printf("cjit: ok %d rewrites, each seen at once\n", REWRITES);
    } else {
        printf("cjit: %d of %d calls ran a stale function\n", stale, REWRITES);
        fail("rewrites are seen at once");
    }
}

/* ── Два потока: один пишет, другой исполняет ────────────────────────────── */

#define BASE 5000
#define STEPS 400

static volatile int writing_done;

struct seen {
    long calls;
    int low;
    int high;
    int backwards;
    int last;
};

static void *executor(void *arg) {
    struct seen *seen = arg;
    int previous = 0;
    for (;;) {
        /* Флаг читается **до** вызова: последний вызов после него обязан
         * увидеть последнюю запись. */
        int done = writing_done;
        int value = call_function();
        seen->calls++;
        if (value < seen->low) {
            seen->low = value;
        }
        if (value > seen->high) {
            seen->high = value;
        }
        if (value < previous) {
            seen->backwards++;
        }
        previous = value;
        if (done) {
            seen->last = value;
            return NULL;
        }
    }
}

static void check_threads(void) {
    write_function(BASE);
    struct seen seen = {0, BASE, BASE, 0, 0};
    pthread_t thread;
    if (pthread_create(&thread, NULL, executor, &seen) != 0) {
        fail("a second thread runs the function while it is rewritten");
        return;
    }
    for (int i = 1; i <= STEPS; i++) {
        write_function(BASE + i);
        /* Пауза длиннее окна, в которое ядро держит исполняющего: иначе
         * исполняющий дождался бы только конца всех записей. */
        if (i % 20 == 0) {
            nap(3);
        }
    }
    writing_done = 1;
    pthread_join(thread, NULL);

    int in_range = seen.low >= BASE && seen.high <= BASE + STEPS;
    if (in_range && seen.backwards == 0 && seen.last == BASE + STEPS && seen.calls > 1) {
        printf("cjit: ok a second thread ran the function %ld times while it was rewritten\n", seen.calls);
    } else {
        printf("cjit: calls %ld, saw %d..%d, %d backwards, last %d\n", seen.calls, seen.low, seen.high,
               seen.backwards, seen.last);
        fail("a second thread runs the function while it is rewritten");
    }
}

/* ── Кусок памяти кода возвращается, остальное живёт ─────────────────────── */

static void check_unmap(uint8_t *memory, size_t page) {
    if (munmap(memory + page, page) != 0) {
        fail("half of code memory is given back");
        return;
    }
    write_function(7);
    if (call_function() == 7) {
        ok("half of code memory is given back, the other half still runs");
    } else {
        fail("half of code memory is given back, the other half still runs");
    }
}

int main(void) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    uint8_t *memory =
        mmap(NULL, 2 * page, PROT_READ | PROT_WRITE | PROT_EXEC, MAP_PRIVATE | MAP_ANONYMOUS | MAP_JIT, -1, 0);
    if (memory == MAP_FAILED) {
        printf("cjit: MAP_JIT refused, errno %d\n", errno);
        return 1;
    }
    code = (volatile uint64_t *)memory;
    printf("cjit: code memory at %p\n", (void *)memory);

    check_refusals(page);
    check_single();
    check_threads();
    check_unmap(memory, page);
    munmap(memory, page);
    printf("cjit: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
