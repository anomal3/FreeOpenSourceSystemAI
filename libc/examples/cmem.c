/* Память так, как её просит чужая среда исполнения (фаза 56).
 *
 * # Что она доказывает
 *
 * Что `mmap`, `munmap` и `mprotect` ведут себя так, как на них рассчитывает
 * код, написанный не под нас. Каждая проверка — сцена из настоящего
 * потребителя, сборщика мусора Mono (`mono/utils/mono-mmap.c`, `sgen`):
 *
 * - резерв адресов, который ничего не стоит, пока его не коснулись;
 * - выровненный кусок, вырезанный из области с запасом: голова и хвост
 *   отдаются обратно (`mono_valloc_aligned`);
 * - дыра посередине (освобождённые блоки кучи);
 * - код, записанный программой и исполненный ею же: `RW`, записать, `RX`,
 *   вызвать — страницы на запись и исполнение сразу система не даёт;
 * - «никак» и обратно — содержимое не теряется;
 * - куча, которая растёт, хотя между `malloc` программа сама зовёт `mmap`.
 *
 * # Нарушения — отдельными запусками
 *
 * Запись в страницу только на чтение, исполнение страницы на запись,
 * обращение к «никак» — всё это снимает программу: сигналов в системе нет, и
 * поймать отказ самой себе ей нечем. Поэтому каждое нарушение — свой запуск
 * со своим словом (`cmem write-ro` и т.д.), а стенд ждёт строку ядра о снятии
 * и **не** ждёт строки программы, которая стояла бы после нарушения.
 *
 * Проверки печатают `cmem: ok <что>`; итог — одна строка с числом проваленных.
 */

#include <errno.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

#include "freeos-syscall.h"

#define PAGE 4096UL

static int failures;

static void ok(const char *what) { printf("cmem: ok %s\n", what); }

static void fail(const char *what) {
    printf("cmem: FAILED %s (errno %d: %s)\n", what, errno, strerror(errno));
    failures++;
}

/* Свободных кадров в машине, в страницах.
 *
 * Буфер с запасом, а не структура: из ответа `SYS_SYSINFO` нужно одно поле
 * (четвёртое — свободная память в байтах), а раскладку целиком заголовок не
 * повторяет. Ядро пишет всю структуру, поэтому буфер заведомо больше её. */
static long free_pages(void) {
    uint64_t info[64];
    long done = freeos_syscall(SYS_SYSINFO, (long)info, 0, 0);
    if (done < 0) {
        return -1;
    }
    return (long)(info[3] / PAGE);
}

/* Резерв ничего не стоит, касание — стоит.
 *
 * Сравнение с порогами, а не точное: в машине живут и другие задачи, и счёт
 * свободных кадров между двумя вопросами может сдвинуться на единицы. Пороги
 * отстоят от «правильно» и «неправильно» на порядки: жадная выдача 64 МиБ —
 * это шестнадцать тысяч кадров, а ленивая — ноль. */
static void check_lazy(void) {
    const size_t len = 64UL * 1024 * 1024;
    long before = free_pages();
    unsigned char *area = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) {
        fail("lazy mmap");
        return;
    }
    long reserved = free_pages();
    if (before - reserved > 64) {
        printf("cmem: reserving 64 MiB took %ld pages\n", before - reserved);
        errno = 0;
        fail("lazy reserve is free");
    } else {
        ok("lazy reserve is free");
    }

    /* Каждая шестнадцатая страница, 256 штук: разбросаны по резерву, чтобы
     * промежуточные таблицы тоже пришлось завести. */
    for (size_t page = 0; page < 256; page++) {
        area[page * 16 * PAGE + page] = (unsigned char)page;
    }
    long touched = free_pages();
    if (reserved - touched < 256) {
        printf("cmem: touching 256 pages took %ld pages\n", reserved - touched);
        errno = 0;
        fail("touching takes frames");
    } else {
        ok("touching takes frames");
    }
    for (size_t page = 0; page < 256; page++) {
        if (area[page * 16 * PAGE + page] != (unsigned char)page) {
            errno = 0;
            fail("lazy pages hold what was written");
            break;
        }
    }

    /* Замер — вплотную к `munmap`, без печати между: счётчик свободных кадров
     * общий на машину, и первый прогон видел 229 вместо 256, потому что
     * между замерами стояли `printf` и чужие задачи. */
    long before_unmap = free_pages();
    if (munmap(area, len) != 0) {
        fail("munmap of the lazy area");
        return;
    }
    long after = free_pages();
    /* Вернуться обязаны все 256 кадров данных; таблицы остаются до выхода
     * программы (так устроено снятие в ядре). Допуск в шестнадцать кадров —
     * на чужие задачи в ту же миллисекунду; поломка снятия вернула бы ноль,
     * а не двести сорок. */
    printf("cmem: munmap gave back %ld pages\n", after - before_unmap);
    if (after - before_unmap < 256 - 16) {
        errno = 0;
        fail("munmap returns lazy frames");
    } else {
        ok("munmap returns lazy frames");
    }
}

/* Выровненный кусок из области с запасом — ровно как `mono_valloc_aligned`. */
static void check_aligned_carve(void) {
    const size_t size = 256 * 1024;
    const size_t align = 1024 * 1024;
    char *mem = mmap(NULL, size + align, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mem == MAP_FAILED) {
        fail("aligned carve: mmap");
        return;
    }
    char *aligned = (char *)(((uintptr_t)mem + align - 1) & ~(uintptr_t)(align - 1));
    if (aligned > mem && munmap(mem, (size_t)(aligned - mem)) != 0) {
        fail("aligned carve: munmap head");
        return;
    }
    char *tail = aligned + size;
    size_t tail_len = (size_t)(mem + size + align - tail);
    if (tail_len > 0 && munmap(tail, tail_len) != 0) {
        fail("aligned carve: munmap tail");
        return;
    }
    memset(aligned, 0x5a, size);
    for (size_t at = 0; at < size; at += PAGE) {
        if (aligned[at] != 0x5a) {
            errno = 0;
            fail("aligned carve: the middle survived");
            return;
        }
    }
    if (munmap(aligned, size) != 0) {
        fail("aligned carve: munmap the rest");
        return;
    }
    ok("aligned carve");
}

/* Дыра посередине: соседи по обе стороны живы и помнят своё. */
static void check_hole(void) {
    const size_t pages = 16;
    unsigned char *area = mmap(NULL, pages * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) {
        fail("hole: mmap");
        return;
    }
    for (size_t page = 0; page < pages; page++) {
        area[page * PAGE] = (unsigned char)(0x40 + page);
    }
    if (munmap(area + 4 * PAGE, 4 * PAGE) != 0) {
        fail("hole: munmap the middle");
        return;
    }
    for (size_t page = 0; page < pages; page++) {
        if (page >= 4 && page < 8) {
            continue;
        }
        if (area[page * PAGE] != (unsigned char)(0x40 + page)) {
            errno = 0;
            fail("hole: the neighbours kept their pages");
            return;
        }
    }
    /* Весь исходный диапазон разом — с дырой внутри, как разрешает POSIX. */
    if (munmap(area, pages * PAGE) != 0) {
        fail("hole: munmap across the hole");
        return;
    }
    ok("hole in the middle");
}

/* Две функции в машинных кодах: одна возвращает 7, другая 42. Длина у них
 * одна, и вторая пишется поверх первой. */
#if defined(__x86_64__)
static const unsigned char RETURN_7[] = {0xb8, 0x07, 0x00, 0x00, 0x00, 0xc3};  /* mov eax, 7; ret */
static const unsigned char RETURN_42[] = {0xb8, 0x2a, 0x00, 0x00, 0x00, 0xc3}; /* mov eax, 42; ret */
#elif defined(__aarch64__)
static const unsigned char RETURN_7[] = {
    0xe0, 0x00, 0x80, 0x52, /* mov w0, #7 */
    0xc0, 0x03, 0x5f, 0xd6, /* ret */
};
static const unsigned char RETURN_42[] = {
    0x40, 0x05, 0x80, 0x52, /* mov w0, #42 */
    0xc0, 0x03, 0x5f, 0xd6, /* ret */
};
#endif

/* Код, который программа пишет себе сама. */
static void check_code(void) {
    unsigned char *page = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        fail("code: mmap");
        return;
    }
    if (mprotect(page, PAGE, PROT_READ | PROT_WRITE | PROT_EXEC) == 0) {
        errno = 0;
        fail("code: a writable and executable page was refused");
    } else if (errno != EACCES) {
        fail("code: W^X answers EACCES");
    } else {
        ok("W^X holds");
    }

    /* Сперва другая функция — исполненная, потом переписанная. Так проверяется не только «исполняется», но и «исполняется
     * то, что записано последним»: на AArch64 без синхронизации кешей процессор
     * взял бы прежние инструкции. */
    memcpy(page, RETURN_7, sizeof(RETURN_7));
    if (mprotect(page, PAGE, PROT_READ | PROT_EXEC) != 0) {
        fail("code: mprotect to RX");
        return;
    }
    int (*function)(void) = (int (*)(void))(void *)page;
    int first = function();
    if (mprotect(page, PAGE, PROT_READ | PROT_WRITE) != 0) {
        fail("code: mprotect back to RW");
        return;
    }
    memcpy(page, RETURN_42, sizeof(RETURN_42));
    if (mprotect(page, PAGE, PROT_READ | PROT_EXEC) != 0) {
        fail("code: mprotect to RX again");
        return;
    }
    int second = function();
    if (first != 7 || second != 42) {
        printf("cmem: the written code returned %d, then %d\n", first, second);
        errno = 0;
        fail("code written and run");
    } else {
        ok("code written and run");
    }
    munmap(page, PAGE);
}

/* «Никак» и обратно: права меняются, память — нет. */
static void check_none(void) {
    unsigned char *area = mmap(NULL, 4 * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (area == MAP_FAILED) {
        fail("none: mmap");
        return;
    }
    area[PAGE] = 0x77;
    /* Середина: две страницы из четырёх, чтобы область пришлось разрезать. */
    if (mprotect(area + PAGE, 2 * PAGE, PROT_NONE) != 0) {
        fail("none: mprotect to PROT_NONE");
        return;
    }
    area[0] = 1;
    area[3 * PAGE] = 3;
    if (mprotect(area + PAGE, 2 * PAGE, PROT_READ) != 0) {
        fail("none: mprotect to PROT_READ");
        return;
    }
    if (area[PAGE] != 0x77 || area[2 * PAGE] != 0) {
        errno = 0;
        fail("none: the page kept its contents");
        return;
    }
    if (mprotect(area, 4 * PAGE, PROT_READ | PROT_WRITE) != 0) {
        fail("none: mprotect the whole range back");
        return;
    }
    area[PAGE] = 0x78;
    munmap(area, 4 * PAGE);
    ok("PROT_NONE and back");

    /* Диапазон с дырой менять нечему — POSIX отвечает `ENOMEM`, мы — отказом. */
    unsigned char *pair = mmap(NULL, 3 * PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (pair == MAP_FAILED) {
        fail("none: mmap a pair");
        return;
    }
    munmap(pair + PAGE, PAGE);
    if (mprotect(pair, 3 * PAGE, PROT_READ) == 0) {
        errno = 0;
        fail("a hole in the range is refused");
    } else {
        ok("a hole in the range is refused");
    }
    munmap(pair, 3 * PAGE);
}

/* Куча растёт, хотя между `malloc` программа сама берёт области. До фазы 56
 * первая же чужая область посреди кучи обрывала её рост. */
static void check_heap(void) {
    void *areas[8];
    size_t total = 0;
    for (int round = 0; round < 8; round++) {
        areas[round] = mmap(NULL, 64 * 1024, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        unsigned char *block = malloc(3 * 1024 * 1024);
        if (block == NULL || areas[round] == MAP_FAILED) {
            fail("heap grows between mmaps");
            return;
        }
        memset(block, round, 3 * 1024 * 1024);
        total += 3;
    }
    for (int round = 0; round < 8; round++) {
        munmap(areas[round], 64 * 1024);
    }
    printf("cmem: the heap grew by %zu MiB between eight mmaps\n", total);
    ok("heap grows between mmaps");
}

/* Нарушения. Каждое снимает программу, и строка после него не печатается. */
static int violate(const char *how) {
    volatile unsigned char *page = mmap(NULL, PAGE, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (page == MAP_FAILED) {
        fail("violation: mmap");
        return 1;
    }
    page[0] = 1;
    if (strcmp(how, "write-ro") == 0) {
        mprotect((void *)page, PAGE, PROT_READ);
        printf("cmem: writing to a read-only page\n");
        page[0] = 2;
    } else if (strcmp(how, "lazy-ro") == 0) {
        /* Страница, которой ещё нет: отказ приходит не нарушением прав, а
         * отсутствием, и отказать обязан обработчик ленивой памяти. */
        volatile unsigned char *lazy =
            mmap(NULL, PAGE, PROT_READ, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
        printf("cmem: writing to an untouched read-only page\n");
        lazy[0] = 2;
    } else if (strcmp(how, "exec-rw") == 0) {
        memcpy((void *)page, RETURN_42, sizeof(RETURN_42));
        printf("cmem: calling into a writable page\n");
        ((int (*)(void))(void *)page)();
    } else if (strcmp(how, "touch-none") == 0) {
        mprotect((void *)page, PAGE, PROT_NONE);
        printf("cmem: reading a PROT_NONE page\n");
        (void)page[0];
    } else {
        printf("cmem: unknown violation '%s'\n", how);
        return 2;
    }
    printf("cmem: FAILED the violation '%s' went through\n", how);
    return 1;
}

int main(int argc, char **argv) {
    if (argc > 1) {
        return violate(argv[1]);
    }
    printf("cmem: starting\n");
    check_lazy();
    check_aligned_carve();
    check_hole();
    check_code();
    check_none();
    check_heap();
    printf("cmem: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
