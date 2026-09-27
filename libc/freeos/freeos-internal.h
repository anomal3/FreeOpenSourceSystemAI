/* Общее у файлов слоя ОС (`syscalls.c`, `files.c`, `posix.c`) — не для программ.
 *
 * В sysroot не копируется: программа, подключившая его, завязалась бы на
 * внутренности этой libc.
 */

#ifndef FREEOS_INTERNAL_H
#define FREEOS_INTERNAL_H

/* Самый длинный путь, который принимает ядро (`MAX_PATH` в `user/syscall.rs`):
 * длиннее оно отвергает само, и собирать такой путь незачем. */
#define FREEOS_PATH_LIMIT 255

/* Перевести отрицательный код ядра в `errno` и вернуть -1 (`syscalls.c`). */
int freeos_set_errno(long code);

/* Путь, который можно отдать ядру (фаза 58b, `files.c`): абсолютный — как есть,
 * относительный — приклеенный к текущему каталогу в `buf` (не короче
 * `FREEOS_PATH_LIMIT + 1`), со свёрнутыми `.` и `..`. `NULL` и `errno` — если
 * путь пуст или вышел длиннее предела. */
const char *freeos_path(const char *path, char *buf);

#endif
