
/* ── FreeOS (фаза 65c) ────────────────────────────────────────────────────
 *
 * Дописывается в конец копии `include/mbedtls/mbedtls_config.h` при сборке
 * (`cargo xtask thirdparty --only mbedtls`): так его видят и сама библиотека,
 * и всякий, кто собирается с её заголовками из набора, — libcurl в том числе.
 * Исходники в архиве не правятся.
 *
 * Случайность — своя: стандартный источник mbedTLS знает только Unix и
 * Windows (`entropy_poll.c`), а у нас есть `getentropy` поверх `SYS_RANDOM`.
 * Её даёт `mbedtls_hardware_poll` из `ports/mbedtls/freeos.c`, вшитая в
 * `libmbedcrypto.a`. */
#define MBEDTLS_NO_PLATFORM_ENTROPY
#define MBEDTLS_ENTROPY_HARDWARE_ALT

/* Часы в миллисекундах — оттуда же: mbedTLS ищет `clock_gettime` по
 * `_POSIX_VERSION`, а наш `unistd.h` его не объявляет. */
#define MBEDTLS_PLATFORM_MS_TIME_ALT

/* Сокеты и часы самого mbedTLS — только для Unix и Windows, и не нужны:
 * libcurl и наши программы кладут байты в сокет сами, через свои функции
 * ввода-вывода (`mbedtls_ssl_set_bio`). */
#undef MBEDTLS_NET_C
#undef MBEDTLS_TIMING_C

/* Постоянное хранилище ключей PSA пишет файлы в текущий каталог — программе,
 * которая просто идёт по HTTPS, это не нужно. */
#undef MBEDTLS_PSA_CRYPTO_STORAGE_C
#undef MBEDTLS_PSA_ITS_FILE_C
