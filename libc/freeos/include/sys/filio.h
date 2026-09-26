/* FIONREAD (фаза 58): сколько байт можно прочитать без ожидания. Число —
 * как у Linux. Сам ответ `ioctl` на него даёт picolibc — отказом, пока ядро
 * не умеет отвечать на такой вопрос. */
#ifndef FREEOS_SYS_FILIO_H
#define FREEOS_SYS_FILIO_H
#include <sys/ioctl.h>
#define FIONREAD 0x541B
#define FIONBIO 0x5421
#endif
