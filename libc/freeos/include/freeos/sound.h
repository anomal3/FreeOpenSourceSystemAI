/* Звук для программ на C (фаза 67f).
 *
 * Тонкая обёртка над `SYS_SOUND` — тот же договор, что у программ на Rust
 * (`crates/user-abi`). Формат один: 16 бит со знаком, little-endian, два
 * канала вперемежку, `FREEOS_SOUND_RATE` кадров в секунду, — он же у
 * звуковой карты, и ядро ничего не пересчитывает.
 *
 * `freeos_sound_write` ждёт, пока для отсчётов найдётся место у звуковой
 * карты, — темп задаёт она, и программа, пишущая быстрее, чем звук играется,
 * просто спит. Порции могут быть любыми: поток между вызовами не прерывается.
 * `freeos_sound_drain` доигрывает отданное и останавливает поток.
 *
 * Ошибка — -1 и `errno`: `ENOSYS`, если звуковой карты нет. */
#ifndef FREEOS_SOUND_H
#define FREEOS_SOUND_H

#include <stddef.h>
#include <sys/types.h>

#ifdef __cplusplus
extern "C" {
#endif

#define FREEOS_SOUND_RATE 48000
#define FREEOS_SOUND_CHANNELS 2

/* Отдать `bytes` байт отсчётов. Ответ — сколько принято. */
ssize_t freeos_sound_write(const void *samples, size_t bytes);

/* Доиграть всё и остановить поток. */
int freeos_sound_drain(void);

#ifdef __cplusplus
}
#endif

#endif
