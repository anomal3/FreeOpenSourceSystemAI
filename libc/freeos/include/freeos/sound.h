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

/* Microphone (28.11): open the HDA input (the one the Linux layer calls
 * hw:0,1) with a cursor of its own - other opens and programs do not take
 * frames from it. Answer - a handle; -1 and errno ENOSYS - no microphone or
 * line input. The end of the program closes its opens. */
int freeos_mic_open(void);

/* Whole frames (same format) of the open `handle` into `buf`. Answer - bytes
 * read, 0 - nothing within ~20 ms. */
ssize_t freeos_mic_read(int handle, void *buf, size_t bytes);

/* Close the open `handle`. */
int freeos_mic_close(int handle);

/* Name of the input ("Mic in, rear pink jack") into `buf` (not terminated
 * when cut); answer - its whole length, -1 - no input. */
ssize_t freeos_mic_name(char *buf, size_t size);

#ifdef __cplusplus
}
#endif

#endif
