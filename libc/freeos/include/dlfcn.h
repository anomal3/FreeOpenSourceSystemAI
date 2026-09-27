/* Динамическая загрузка — без загрузчика (фазы 58, 60).
 *
 * Программы компонуются статически, и загрузчика разделяемых библиотек в
 * системе нет. Но чужой код ищет функции по имени — Mono так находит каждую
 * функцию, которую сборка C# зовёт через P/Invoke, — и для него у `dlsym` есть
 * **таблица экспортов**, которую программа несёт в себе: массив
 * `freeos_exports`, кончающийся записью с `name == NULL`. Библиотека,
 * «открытая» `dlopen`, — это имя из таблицы; сама программа — `dlopen(NULL)`,
 * и поиск по ней идёт во всей таблице. Нет таблицы или нет имени — отказ,
 * `NULL` и объяснение в `dlerror`, как и раньше.
 *
 * Имя библиотеки сравнивается без каталога, без приставки `lib` и без
 * окончания `.so`/`.dll`/`.dylib`: `libSystem.Native.so` и
 * `/usr/lib/System.Native.dll` — это `System.Native`. */

#ifndef FREEOS_DLFCN_H
#define FREEOS_DLFCN_H

#ifdef __cplusplus
extern "C" {
#endif

#define RTLD_LAZY 1
#define RTLD_NOW 2
#define RTLD_GLOBAL 0x100
#define RTLD_LOCAL 0
#define RTLD_DEFAULT ((void *)0)

/* Запись таблицы экспортов: чья функция, как зовётся и где лежит. */
struct freeos_export {
    const char *library;
    const char *name;
    void *address;
};

/* Таблица экспортов программы. Слабая ссылка: у программы без неё это `NULL`. */
extern const struct freeos_export freeos_exports[] __attribute__((weak));

void *dlopen(const char *path, int flags);
void *dlsym(void *handle, const char *name);
int dlclose(void *handle);
char *dlerror(void);

#ifdef __cplusplus
}
#endif

#endif
