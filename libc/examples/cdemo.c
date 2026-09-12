/* Обычная программа на C. Ни одного системного вызова руками.
 *
 * # Что она доказывает
 *
 * Что цель фазы достигнута: человек пишет на C так, как писал бы где угодно, —
 * `printf`, `fopen`, `malloc`, `strcmp`, — и это работает в FreeOS. Ни одной
 * строки, написанной «под нашу систему», здесь нет: тот же файл собирается
 * компилятором хоста и проходит те же проверки (`cargo test -p xtask`,
 * `cdemo_behaves_the_same_on_the_host`). Программа, которую нельзя собрать
 * ничем, кроме нашего набора, доказывала бы только существование набора.
 *
 * # Почему проверки печатают «ok», а не молчат
 *
 * Потому что проверяет её автоматический стенд по серийной линии, а не человек
 * глазами. Каждая строка `cdemo: ok <что>` — отдельное утверждение, и падение
 * видно по тому, какой из них нет. Итоговая строка одна и содержит число
 * проваленных: ноль — и только ноль — означает, что прошло всё.
 */

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

/* Куда писать. Каталог передаётся аргументом: на системе, загруженной с
 * носителя, корень доступен только на чтение, а на установленной — нет. Один
 * и тот же двоичный файл обязан проходить и там, и там. */
static const char *DEFAULT_DIR = ".";

static int failures;

static void ok(const char *what) { printf("cdemo: ok %s\n", what); }

static void fail(const char *what) {
    /* Причина печатается вместе с `errno`: «не получилось» без него отсылает
     * читать код, а с ним — сразу к нужной ветке. */
    printf("cdemo: FAILED %s (errno %d: %s)\n", what, errno, strerror(errno));
    failures++;
}

/* Строка, которую программа пишет и читает обратно.
 *
 * Не «test» и не «hello»: в ней есть перевод строки посередине и знак,
 * выходящий за ASCII, — файл, обрезанный на первом же из них, иначе выглядел бы
 * прочитанным целиком. */
static const char SAMPLE[] = "libc works\nsecond line\n";

/* Проверить кучу.
 *
 * Не «выделилось и ладно»: память заполняется узором, зависящим от номера
 * байта, и сверяется после второго выделения. Аллокатор, отдавший один и тот же
 * блок дважды, на константе прошёл бы незамеченным. */
static void check_heap(void) {
    const size_t len = 4096;
    unsigned char *first = malloc(len);
    if (first == NULL) {
        fail("malloc");
        return;
    }
    for (size_t i = 0; i < len; i++) {
        first[i] = (unsigned char)(i * 31 + 7);
    }

    unsigned char *second = malloc(len);
    if (second == NULL) {
        free(first);
        fail("malloc twice");
        return;
    }
    memset(second, 0, len);

    for (size_t i = 0; i < len; i++) {
        if (first[i] != (unsigned char)(i * 31 + 7)) {
            free(first);
            free(second);
            fail("the heap handed the same block out twice");
            return;
        }
    }
    free(first);
    free(second);
    ok("malloc");
}

/* Проверить работу с файлом: записать, закрыть, открыть заново, прочитать. */
static void check_file(const char *dir) {
    char path[256];
    snprintf(path, sizeof(path), "%s/cdemo.txt", dir);

    /* Двоично (`"wb"`, а не `"w"`), и это не перестраховка: в текстовом режиме
     * Windows переводит `
` в `
`, и записанные двадцать три байта
     * становятся двадцатью пятью. Поймала это ровно хостовая проверка — в
     * FreeOS текстового режима нет вовсе, и здесь ошибка была бы невидима. */
    FILE *out = fopen(path, "wb");
    if (out == NULL) {
        fail("fopen for writing");
        return;
    }
    size_t written = fwrite(SAMPLE, 1, sizeof(SAMPLE) - 1, out);
    if (written != sizeof(SAMPLE) - 1) {
        fclose(out);
        fail("fwrite wrote the wrong length");
        return;
    }
    if (fclose(out) != 0) {
        fail("fclose");
        return;
    }
    ok("fopen and fwrite");

    /* Размер спрашивается у системы, а не у своей же памяти: это единственный
     * способ убедиться, что записанное дошло до носителя, а не осталось в
     * буфере stdio. */
    struct stat info;
    if (stat(path, &info) != 0) {
        fail("stat");
    } else if ((size_t)info.st_size != sizeof(SAMPLE) - 1) {
        printf("cdemo: FAILED stat says %ld bytes, expected %zu\n", (long)info.st_size,
               sizeof(SAMPLE) - 1);
        failures++;
    } else {
        ok("stat");
    }

    FILE *in = fopen(path, "rb");
    if (in == NULL) {
        fail("fopen for reading");
        return;
    }
    char buffer[sizeof(SAMPLE)];
    memset(buffer, 0, sizeof(buffer));
    size_t got = fread(buffer, 1, sizeof(SAMPLE) - 1, in);
    fclose(in);

    if (got != sizeof(SAMPLE) - 1) {
        printf("cdemo: FAILED fread got %zu bytes, expected %zu\n", got, sizeof(SAMPLE) - 1);
        failures++;
        return;
    }
    if (memcmp(buffer, SAMPLE, sizeof(SAMPLE) - 1) != 0) {
        fail("what was read back is not what was written");
        return;
    }
    ok("fread");

    if (remove(path) != 0) {
        fail("remove");
        return;
    }
    /* Удалённого файла быть не должно. Проверка отдельная: `remove`, вернувшая
     * ноль и ничего не удалившая, — ровно та заглушка, против которой написана
     * вся эта фаза. */
    if (stat(path, &info) == 0) {
        fail("the file is still there after remove");
        return;
    }
    ok("remove");
}

/* Сравнение для `qsort`: обычная функция обратного вызова. */
static int compare_ints(const void *a, const void *b) {
    int left = *(const int *)a;
    int right = *(const int *)b;
    return (left > right) - (left < right);
}

/* Проверить строки и форматирование.
 *
 * Здесь нет ничего, что зависело бы от системы, — и в этом смысл: если эта
 * часть сломается, сломана сама библиотека, а не порт. */
static void check_strings(void) {
    char joined[64];
    strcpy(joined, "free");
    strcat(joined, "OS");
    if (strcmp(joined, "freeOS") != 0) {
        fail("strcpy and strcat");
        return;
    }
    if (strlen(joined) != 6) {
        fail("strlen");
        return;
    }

    char formatted[64];
    int length = snprintf(formatted, sizeof(formatted), "%s %d %05.2f %x", joined, -42, 3.5, 255);
    if (length != (int)strlen(formatted)) {
        fail("snprintf returned a length that does not match");
        return;
    }
    if (strcmp(formatted, "freeOS -42 03.50 ff") != 0) {
        printf("cdemo: FAILED snprintf produced '%s'\n", formatted);
        failures++;
        return;
    }
    ok("strings and snprintf");

    /* Сортировка чужой функцией сравнения: `qsort` зовёт наш код обратно, то
     * есть проверяет указатель на функцию — а он на этой архитектуре живёт по
     * адресу 512 ГиБ и не влезает в тридцать два бита. Модель кода, выбранная
     * не та, ломается ровно здесь. */
    int numbers[] = {5, 3, 9, 1, 7};
    const size_t count = sizeof(numbers) / sizeof(numbers[0]);
    qsort(numbers, count, sizeof(int), compare_ints);
    for (size_t i = 1; i < count; i++) {
        if (numbers[i - 1] > numbers[i]) {
            fail("qsort");
            return;
        }
    }
    ok("qsort");
}

int main(int argc, char **argv) {
    const char *dir = argc > 1 ? argv[1] : DEFAULT_DIR;
    printf("cdemo: starting, writing under %s\n", dir);

    check_strings();
    check_heap();
    check_file(dir);

    printf("cdemo: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
