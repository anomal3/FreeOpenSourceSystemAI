/* Звук из программы на C (фаза 67f).
 *
 * Синус через `sinf` из libm — так, как его считала бы любая программа, — в
 * кусках разного размера: поток между вызовами не прерывается, и мелкие
 * порции не должны превращаться в тишину с щелчками. Частоту проверяет стенд
 * по записи QEMU (`Step::Tone`).
 *
 * `csound <Гц> <мс>` — по умолчанию 660 Гц и полторы секунды. */

#include <errno.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <freeos/sound.h>

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IOLBF, 0);
    int hz = argc > 1 ? atoi(argv[1]) : 660;
    int ms = argc > 2 ? atoi(argv[2]) : 1500;
    long frames = (long)FREEOS_SOUND_RATE * ms / 1000;

    /* Порции — 100, 1000 и 3000 кадров по кругу: меньше периода устройства,
     * больше него и не кратно ему. */
    static int16_t chunk[3000 * FREEOS_SOUND_CHANNELS];
    const long sizes[] = {100, 1000, 3000};
    long written = 0;
    float phase = 0.0f;
    const float step = 2.0f * 3.14159265f * (float)hz / (float)FREEOS_SOUND_RATE;
    for (int turn = 0; written < frames; turn++) {
        long take = sizes[turn % 3];
        if (take > frames - written) {
            take = frames - written;
        }
        for (long frame = 0; frame < take; frame++) {
            int16_t value = (int16_t)(8000.0f * sinf(phase));
            chunk[frame * 2] = value;
            chunk[frame * 2 + 1] = value;
            phase += step;
            if (phase > 2.0f * 3.14159265f) {
                phase -= 2.0f * 3.14159265f;
            }
        }
        ssize_t accepted = freeos_sound_write(chunk, (size_t)take * 4);
        if (accepted != take * 4) {
            printf("csound: FAILED write (%ld of %ld bytes, errno %d: %s)\n", (long)accepted, take * 4, errno,
                   strerror(errno));
            return 1;
        }
        written += take;
    }
    if (freeos_sound_drain() != 0) {
        printf("csound: FAILED drain (errno %d: %s)\n", errno, strerror(errno));
        return 1;
    }
    printf("csound: played %ld frames of %d Hz\n", written, hz);
    return 0;
}
