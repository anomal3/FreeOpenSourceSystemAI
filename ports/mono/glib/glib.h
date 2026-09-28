/* glib для libgdiplus во FreeOS — это eglib самой Mono (фаза 61b).
 *
 * libgdiplus берёт из glib малость: хеш-таблицу, `GString`, UTF-8, `g_new`,
 * `g_warning` и два мьютекса. Всё это, кроме мьютексов, есть в eglib — урезанной
 * glib, которую Mono носит в себе и с которой уже скомпонована. Портировать
 * glib целиком (своя сборка на meson, libffi, pcre, gettext) ради этого было бы
 * несоразмерно, а две копии хеш-таблицы в одной программе — бессмысленно.
 *
 * Имена eglib переименованы (`g_hash_table_new` → `monoeg_g_hash_table_new`,
 * `eglib-remap.h`), поэтому libgdiplus после компиляции ссылается ровно на те
 * функции, что уже лежат в `mono`, и компонуется с ней без второй копии.
 *
 * Досказать приходится одно: eglib называет себя glib 2.4, а libgdiplus для
 * glib старше 2.32 берёт `GStaticMutex` — тип, которого в eglib нет (самой Mono
 * он не нужен). Он здесь, поверх мьютекса POSIX; статический инициализатор у
 * него есть, как и требует `G_STATIC_MUTEX_INIT`.
 *
 * Заголовок кладёт `ports/mono/gdiplus.sh` рядом с копией заголовков eglib
 * этой архитектуры: `include/glib.h` (этот) и `include/eglib/…`. */
#ifndef FREEOS_GLIB_SHIM_H
#define FREEOS_GLIB_SHIM_H

#include "eglib/glib.h"

/* Настоящая glib подключает `<float.h>` сама (`gtypes.h`, ради `G_MAXFLOAT`), и
 * libgdiplus на это полагается: `matrix.c` берёт `FLT_MAX`, ничего не
 * подключая. eglib этого не делает. */
#include <float.h>
#include <pthread.h>

typedef struct {
    pthread_mutex_t mutex;
} GStaticMutex;

#define G_STATIC_MUTEX_INIT {PTHREAD_MUTEX_INITIALIZER}
#define g_static_mutex_lock(m) pthread_mutex_lock(&(m)->mutex)
#define g_static_mutex_unlock(m) pthread_mutex_unlock(&(m)->mutex)

#endif
