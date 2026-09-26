/* Динамическая загрузка — которой в FreeOS нет (фаза 58).
 *
 * Программы компонуются статически, и загрузчика разделяемых библиотек в
 * системе нет. Заголовок есть затем, чтобы чужой код, спрашивающий `dlopen`,
 * собирался и получал **отказ** — `NULL` и объяснение в `dlerror`, — а не
 * падал на сборке. Mono в статической сборке ищет свои функции так же через
 * `dlsym` по «себе» и на отказ отвечает своей таблицей. */

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

void *dlopen(const char *path, int flags);
void *dlsym(void *handle, const char *name);
int dlclose(void *handle);
char *dlerror(void);

#ifdef __cplusplus
}
#endif

#endif
