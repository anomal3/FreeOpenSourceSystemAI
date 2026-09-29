/* Окна рабочего стола для программ на C (фаза 62). Договор — в
 * `<freeos/window.h>`, здесь только перевод в системные вызовы: ни состояния,
 * ни очередей своих. */

#include <errno.h>
#include <stddef.h>
#include <string.h>

#include <freeos/window.h>

#include "freeos-internal.h"
#include "freeos-syscall.h"

/* Сколько раз и с какой паузой переспрашивать занятый стол: сорок раз по 50 мс,
 * две секунды — как у обвязки программ на Rust (`insist` в `user-progs`). */
#define BUSY_TRIES 40
#define BUSY_PAUSE_NS 50000000L

/* Вызов окна, повторённый, пока стол занят сборкой кадра.
 *
 * `FREEOS_ERR_AGAIN` от вызовов окна значит «ещё раз», а не «не вышло»: стол
 * вынимают из-под замка на время перерисовки, и попасть в этот промежуток —
 * обычное дело. Ждать его — свойство договора, одинаковое для всех, поэтому
 * повтор здесь, а не у каждой программы. До фазы 62b его здесь не было: вызов,
 * пришедшийся на анимацию открытия окна, отказывал сразу, и первый же
 * `freeos_window_style` у формы WinForms ушёл в отказ. */
static long insist(long number, long a0, long a1, long a2) {
    long code = FREEOS_ERR_AGAIN;
    for (int i = 0; i < BUSY_TRIES; i++) {
        code = freeos_syscall(number, a0, a1, a2);
        if (code != FREEOS_ERR_AGAIN) {
            break;
        }
        freeos_syscall(SYS_NANOSLEEP, 0, BUSY_PAUSE_NS, 0);
    }
    return code;
}

int64_t freeos_window_open(const char *title, uint32_t width, uint32_t height, void **surface) {
    if (title == NULL || surface == NULL) {
        errno = EFAULT;
        return -1;
    }
    struct freeos_window_spec spec = {
        .title = (uint64_t)(uintptr_t)title,
        .title_len = strlen(title),
        .width = width,
        .height = height,
    };
    long code = insist(SYS_WINOPEN, (long)&spec, 0, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    *surface = (void *)(uintptr_t)spec.surface;
    return spec.id;
}

/* Координаты упакованы по две в аргумент — так их принимает ядро: вызов зовут
 * на каждый кадр, и структура в памяти добавила бы ему проверку указателя. */
static long pack(int32_t high, int32_t low) {
    return (long)(((uint64_t)(uint32_t)high << 32) | (uint32_t)low);
}

int freeos_window_commit(int64_t id, int32_t x, int32_t y, int32_t width, int32_t height) {
    long code = insist(SYS_WINCOMMIT, (long)id, pack(x, y), pack(width, height));
    return code < 0 ? freeos_set_errno(code) : 0;
}

int freeos_window_event(int64_t id, struct freeos_win_event *event) {
    if (event == NULL) {
        errno = EFAULT;
        return -1;
    }
    struct freeos_win_event_raw raw;
    long code = freeos_syscall(SYS_WINEVENT, (long)id, (long)&raw, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    if (code == 0) {
        return 0;
    }
    event->kind = raw.kind;
    event->code = raw.code;
    event->x = raw.x;
    event->y = raw.y;
    return 1;
}

void *freeos_window_resize(int64_t id, uint32_t width, uint32_t height) {
    long code = insist(SYS_WINRESIZE, (long)id, pack((int32_t)width, (int32_t)height), 0);
    if (code < 0) {
        freeos_set_errno(code);
        return NULL;
    }
    return (void *)(uintptr_t)code;
}

int freeos_window_style(int64_t id, uint32_t flags) {
    long code = insist(SYS_WINSTYLE, (long)id, (long)flags, 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

int freeos_window_close(int64_t id) {
    long code = insist(SYS_WINCLOSE, (long)id, 0, 0);
    return code < 0 ? freeos_set_errno(code) : 0;
}

int freeos_screen(uint32_t *width, uint32_t *height, uint32_t *pixel_format) {
    struct freeos_sysinfo info;
    long code = freeos_syscall(SYS_SYSINFO, (long)&info, 0, 0);
    if (code < 0) {
        return freeos_set_errno(code);
    }
    if (width != NULL) {
        *width = info.screen_w;
    }
    if (height != NULL) {
        *height = info.screen_h;
    }
    if (pixel_format != NULL) {
        *pixel_format = info.pixel_format;
    }
    return 0;
}
