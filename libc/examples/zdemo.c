/* Чужая библиотека, собранная нашим набором и работающая в FreeOS.
 *
 * # Что она доказывает и чего не доказывает `zlib собрался`
 *
 * Сборка чужого проекта доказывает, что набор выглядит как набор: `configure`
 * нашёл инструменты, компилятор проглотил исходники, архиватор собрал `libz.a`.
 * Она **не** доказывает, что получившийся код работает: неверная модель кода,
 * разъехавшаяся раскладка структуры, `memcpy`, которого нет, — всё это
 * собирается молча и падает при запуске.
 *
 * Поэтому фаза кончается не сборкой, а этой программой. Она сжимает данные,
 * распаковывает обратно и сверяет побайтно; сверх того считает CRC-32 —
 * чужой реализацией, — и сверяет с числом, посчитанным здесь же вручную.
 *
 * # Почему данные именно такие
 *
 * Не «hello» и не нули. Строка повторяется, чтобы сжатию было что сжимать:
 * несжимаемый вход zlib оборачивает в блок «как есть», и проверка прошла бы,
 * даже если бы дефлятор не работал вовсе. Размер — больше окна вывода
 * (`uncompress` за один вызов), но заведомо влезающий в кучу.
 *
 * # Ни строки «под нашу систему»
 *
 * Ровно как в `cdemo`: если бы программу пришлось писать с оглядкой на FreeOS,
 * она доказывала бы существование FreeOS, а не переносимость набора.
 */

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zlib.h>

static int failures;

static void ok(const char *what) { printf("zdemo: ok %s\n", what); }

static void fail(const char *what, int code) {
    printf("zdemo: FAILED %s (code %d)\n", what, code);
    failures++;
}

/* Образец: узнаваемая строка, повторённая до размера, который стоит сжимать. */
static const char UNIT[] = "the quick brown fox jumps over the lazy dog; ";
#define REPEATS 400

static unsigned char *make_sample(size_t *length) {
    const size_t unit = sizeof(UNIT) - 1;
    const size_t total = unit * REPEATS;
    unsigned char *data = malloc(total);
    if (data == NULL) {
        return NULL;
    }
    for (size_t i = 0; i < REPEATS; i++) {
        memcpy(data + i * unit, UNIT, unit);
    }
    *length = total;
    return data;
}

/* CRC-32 своей рукой, побитно и без таблицы.
 *
 * Медленно и намеренно: смысл в том, чтобы число было посчитано **не** тем
 * кодом, который проверяется. Совпадение двух независимых реализаций — это
 * проверка; совпадение библиотеки с самой собой — тавтология. */
static unsigned long slow_crc32(const unsigned char *data, size_t length) {
    unsigned long crc = 0xffffffffUL;
    for (size_t i = 0; i < length; i++) {
        crc ^= data[i];
        for (int bit = 0; bit < 8; bit++) {
            crc = (crc & 1) ? ((crc >> 1) ^ 0xedb88320UL) : (crc >> 1);
        }
    }
    return (crc ^ 0xffffffffUL) & 0xffffffffUL;
}

int main(void) {
    printf("zdemo: starting, zlib %s\n", zlibVersion());

    size_t length = 0;
    unsigned char *sample = make_sample(&length);
    if (sample == NULL) {
        fail("malloc", 0);
        printf("zdemo: done, %d check(s) failed\n", failures);
        return 1;
    }
    printf("zdemo: sample is %lu bytes\n", (unsigned long) length);

    /* Контрольная сумма: чужая реализация против своей. */
    unsigned long theirs = crc32(0UL, sample, (unsigned) length);
    unsigned long ours = slow_crc32(sample, length);
    if (theirs == ours) {
        ok("crc32");
    } else {
        printf("zdemo: FAILED crc32 (their %08lx, our %08lx)\n", theirs, ours);
        failures++;
    }

    uLongf packed_len = compressBound(length);
    unsigned char *packed = malloc(packed_len);
    if (packed == NULL) {
        fail("malloc for the compressed copy", 0);
        free(sample);
        printf("zdemo: done, %d check(s) failed\n", failures);
        return 1;
    }

    int status = compress2(packed, &packed_len, sample, (uLong) length, 9);
    if (status != Z_OK) {
        fail("compress2", status);
    } else if (packed_len >= length) {
        /* Сжатие, не уменьшившее повторяющуюся строку в разы, — это сжатие,
         * которое не работает, а не «так вышло». */
        printf("zdemo: FAILED compress2 did not shrink (%lu -> %lu)\n",
               (unsigned long) length, (unsigned long) packed_len);
        failures++;
    } else {
        printf("zdemo: ok compress2, %lu -> %lu bytes\n", (unsigned long) length,
               (unsigned long) packed_len);
    }

    uLongf back_len = length;
    unsigned char *back = malloc(back_len);
    if (back == NULL) {
        fail("malloc for the round trip", 0);
    } else {
        status = uncompress(back, &back_len, packed, packed_len);
        if (status != Z_OK) {
            fail("uncompress", status);
        } else if (back_len != length) {
            printf("zdemo: FAILED uncompress gave %lu bytes instead of %lu\n",
                   (unsigned long) back_len, (unsigned long) length);
            failures++;
        } else if (memcmp(back, sample, length) != 0) {
            /* Отдельной проверкой, а не вместе с длиной: «столько же байт» и
             * «те же байты» — разные утверждения, и первое проходит при
             * испорченном содержимом. */
            printf("zdemo: FAILED round trip differs\n");
            failures++;
        } else {
            ok("round trip");
        }
        free(back);
    }

    free(packed);
    free(sample);
    printf("zdemo: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
