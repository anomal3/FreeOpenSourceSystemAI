/* Что mbedTLS берёт у FreeOS (фаза 65c): случайность и часы.
 *
 * Своих версий для нас у mbedTLS нет — он узнаёт Unix по `__unix__`, а
 * компилятор FreeOS его не объявляет. `freeos-config.h` велит взять эти две
 * функции отсюда; вшиваются они в `libmbedcrypto.a` при сборке. */

#include <stddef.h>
#include <stdint.h>
#include <time.h>
#include <unistd.h>

/* Случайные байты (`MBEDTLS_ENTROPY_HARDWARE_ALT`) — из `getentropy`, то есть
 * из `SYS_RANDOM` ядра. */

int mbedtls_hardware_poll(void *data, unsigned char *output, size_t len, size_t *olen);

int mbedtls_hardware_poll(void *data, unsigned char *output, size_t len, size_t *olen) {
    (void)data;
    size_t done = 0;
    while (done < len) {
        /* `getentropy` отдаёт не больше 256 байт за раз — так она определена. */
        size_t chunk = len - done > 256 ? 256 : len - done;
        if (getentropy(output + done, chunk) != 0) {
            *olen = done;
            return -1;
        }
        done += chunk;
    }
    *olen = done;
    return 0;
}

/* Миллисекунды для сроков сеансов TLS 1.3 (`MBEDTLS_PLATFORM_MS_TIME_ALT`) —
 * монотонные часы, их не переводят назад. Тип — `int64_t`, как
 * `mbedtls_ms_time_t` в `platform_time.h`. */
int64_t mbedtls_ms_time(void);

int64_t mbedtls_ms_time(void) {
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
        return (int64_t)time(NULL) * 1000;
    }
    return (int64_t)now.tv_sec * 1000 + now.tv_nsec / 1000000;
}
