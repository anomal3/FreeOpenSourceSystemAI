/* Куча программ на C: dlmalloc Дуга Ли вместо malloc picolibc (фаза 70e).
 *
 * # Зачем своя куча
 *
 * malloc picolibc обнуляет **каждый** выданный блок (`memset` в конце
 * `malloc`, без настройки, которая бы это выключила). Для мелких блоков это
 * цена, для больших — приговор ленивой памяти: `malloc` на гигабайт касается
 * всех его страниц, и программа, попросившая памяти больше, чем есть в
 * машине, снимается ядром ещё внутри `malloc`, не начав работать. Так падал
 * `cmem` с гибибайтом на машине в 512 МиБ — а llama.cpp без `mmap` просит
 * под модель гигабайты. И блоки picolibc не возвращаются системе никогда.
 *
 * dlmalloc — классический аллокатор (на нём вырос ptmalloc в glibc):
 * большие блоки (от 256 КиБ) он берёт отдельным ленивым `mmap` и отдаёт
 * назад `munmap` при `free`; знает, что такая память уже нулевая
 * (`MMAP_CLEARS`), и `calloc` её не обнуляет; мелкие режет из кучи `sbrk`.
 *
 * # Как подключён
 *
 * Исходник — `dlmalloc/malloc.c`, версия 2.8.6, без единой правки
 * (https://gee.cs.oswego.edu/pub/misc/malloc.c, SHA-256
 * be7be56b40b4b31417e74e77687f9b96127ea6cae534cf236aee40a4c739508d,
 * лицензия автора — разрешительная, в шапке файла). Здесь — только его
 * настройки. Имена — обычные (`malloc`, `free`, …): `libfreeos.a` стоит на
 * компоновке перед libc, и ld.lld берёт первое определение, поэтому
 * malloc picolibc не попадает в программу вовсе; `crt0.c` ссылается на
 * `malloc`, чтобы так было и у программы, которая сама его не зовёт.
 */

/* Блокировка — спин-замок на атомарных встроенных функциях clang со
 * `sched_yield`: от потоков (`threads.c`) он не зависит и работает уже до
 * их запуска. */
#define USE_LOCKS 1
/* Мелкое — из кучи `sbrk` (`syscalls.c`): резерв адресов в 256 ГиБ. Сжать её
 * `sbrk` умеет, но память при этом остаётся за программой (см. там) —
 * просить незачем. */
#define MORECORE_CANNOT_TRIM 1
/* Большое — ленивым `mmap`: нули без касания, `munmap` возвращает кадры. */
#define HAVE_MMAP 1
#define MMAP_CLEARS 1
#define HAVE_MREMAP 0
#define malloc_getpagesize 4096
/* `struct mallinfo` — та, что объявляет `<malloc.h>` picolibc; раскладка у
 * них совпадает поле в поле. */
#define STRUCT_MALLINFO_DECLARED 1
#define MALLINFO_FIELD_TYPE size_t
/* `sys/param.h` у picolibc есть, но страницу сказано выше. */
#define LACKS_SYS_PARAM_H 1

#include <malloc.h>

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Weverything"
#include "dlmalloc/malloc.c"
#pragma clang diagnostic pop

/* Чего в dlmalloc 2.8.6 нет, а у picolibc (и в C11) есть. Без них программа,
 * позвавшая одну из этих функций, вытянула бы из libc файл с её собственным
 * `memalign` — и два определения столкнулись бы на компоновке. */
void *aligned_alloc(size_t align, size_t size) { return memalign(align, size); }

void cfree(void *ptr) { free(ptr); }
