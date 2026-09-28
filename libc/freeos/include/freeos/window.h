/* Окна рабочего стола для программ на C (фаза 62).
 *
 * Тонкая обёртка над `SYS_WINOPEN` и соседями — тот же договор, которым окна
 * открывают программы на Rust и своя среда .NET (`crates/user-abi`). Первый
 * её пользователь — драйвер окон WinForms Mono (`XplatUIFreeOS`), который зовёт
 * эти функции через P/Invoke по имени библиотеки `freeos`.
 *
 * # Как устроено окно
 *
 * Окно — поверхность: `width * height` точек по четыре байта, строка за
 * строкой, без промежутков, отображённых в память программы. Программа пишет в
 * неё как в обычную память, а что изменилось, сообщает `freeos_window_commit`:
 * до этого стол о правке не знает. Заголовок, рамку и кнопки рисует стол, а
 * поверхность — только содержимое под заголовком. Порядок байтов точки зависит
 * от машины: `freeos_screen` отвечает, какой он (`FREEOS_PIXEL_RGB` или
 * `FREEOS_PIXEL_BGR`).
 *
 * События не ждут: `freeos_window_event` отвечает сразу, и программа, которой
 * нечем заняться, спит сама.
 *
 * Ошибка — -1 и `errno`, как у любой функции POSIX. */
#ifndef FREEOS_WINDOW_H
#define FREEOS_WINDOW_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Точки поверхности: красный — в младшем байте. */
#define FREEOS_PIXEL_RGB 1
/* Точки поверхности: синий — в младшем байте (как у GDI+ и у Windows). */
#define FREEOS_PIXEL_BGR 2

/* Виды событий — `user_abi::WIN_*`. */
#define FREEOS_WIN_KEY 1
#define FREEOS_WIN_POINTER 2
#define FREEOS_WIN_CLOSE 3
#define FREEOS_WIN_MOVE 4
#define FREEOS_WIN_LEAVE 5

/* Модификаторы в `x` события клавиши. */
#define FREEOS_WIN_MOD_SHIFT 1
#define FREEOS_WIN_MOD_CTRL 2
#define FREEOS_WIN_MOD_ALT 4

/* `code` не меньше этого — имя клавиши без символа, а не символ Unicode. */
#define FREEOS_WIN_KEY_NAMED 0x01000000u
#define FREEOS_WIN_KEY_LEFT (FREEOS_WIN_KEY_NAMED + 1)
#define FREEOS_WIN_KEY_RIGHT (FREEOS_WIN_KEY_NAMED + 2)
#define FREEOS_WIN_KEY_UP (FREEOS_WIN_KEY_NAMED + 3)
#define FREEOS_WIN_KEY_DOWN (FREEOS_WIN_KEY_NAMED + 4)
#define FREEOS_WIN_KEY_HOME (FREEOS_WIN_KEY_NAMED + 5)
#define FREEOS_WIN_KEY_END (FREEOS_WIN_KEY_NAMED + 6)
#define FREEOS_WIN_KEY_PAGE_UP (FREEOS_WIN_KEY_NAMED + 7)
#define FREEOS_WIN_KEY_PAGE_DOWN (FREEOS_WIN_KEY_NAMED + 8)
#define FREEOS_WIN_KEY_DELETE (FREEOS_WIN_KEY_NAMED + 9)
#define FREEOS_WIN_KEY_MENU (FREEOS_WIN_KEY_NAMED + 10)

/* Событие окна.
 *
 * - `FREEOS_WIN_KEY`: `code` — символ клавиши с учётом раскладки и
 *   модификаторов (или имя клавиши), `x` — маска модификаторов, `y` — буква
 *   клавиши в раскладке US без Shift (ноль — у клавиши её нет).
 * - `FREEOS_WIN_POINTER`: щелчок; `code` — 1 левой кнопкой, 2 правой; `x`,
 *   `y` — точка внутри поверхности. Отпускания кнопки договор не сообщает.
 * - `FREEOS_WIN_MOVE`: указатель над содержимым; `code` — маска кнопок.
 * - `FREEOS_WIN_LEAVE`: указатель ушёл с содержимого окна.
 * - `FREEOS_WIN_CLOSE`: окно просят закрыть; закрывает его программа. */
struct freeos_win_event {
    uint32_t kind;
    uint32_t code;
    int32_t x;
    int32_t y;
};

/* Открыть окно с содержимым `width` на `height` точек. Номер окна — ответ,
 * адрес поверхности — в `*surface`. Заголовок — UTF-8. */
int64_t freeos_window_open(const char *title, uint32_t width, uint32_t height, void **surface);

/* Сказать, какая часть поверхности изменилась; нулевая ширина или высота —
 * всё окно. */
int freeos_window_commit(int64_t id, int32_t x, int32_t y, int32_t width, int32_t height);

/* Забрать событие: 1 — записано в `*event`, 0 — очередь пуста. */
int freeos_window_event(int64_t id, struct freeos_win_event *event);

/* Сменить размер содержимого. Прежняя поверхность недействительна; ответ —
 * адрес новой, `NULL` — отказ (окно осталось прежним). */
void *freeos_window_resize(int64_t id, uint32_t width, uint32_t height);

/* Закрыть окно. Поверхность после этого недействительна. */
int freeos_window_close(int64_t id);

/* Размер экрана и порядок байтов точки. Нули — графики в системе нет. */
int freeos_screen(uint32_t *width, uint32_t *height, uint32_t *pixel_format);

#ifdef __cplusplus
}
#endif

#endif
