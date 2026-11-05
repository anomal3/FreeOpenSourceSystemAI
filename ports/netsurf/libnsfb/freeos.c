/* Поверхность libnsfb на окне стола FreeOS (NetSurf, начато 03.11).
 *
 * Кадр nsfb — это сама поверхность окна (`<freeos/window.h>`): рисовальщики
 * libnsfb пишут точки прямо в память, которую видит стол, и копировать нечего;
 * `update` только сообщает столу изменённый прямоугольник. Своя поверхность, а
 * не SDL: поверхность SDL у libnsfb — на SDL 1.2, которого у нас нет, а SDL2
 * между NetSurf и окном добавил бы мегабайт ради того же `memcpy`.
 *
 * Указатель рисует стол, поэтому свой (программный) указатель nsfb не
 * рисуется вовсе: иначе их было бы два.
 *
 * Ввод: договор окна отдаёт символ клавиши и её букву в раскладке US, а nsfb
 * ждёт коды клавиш в духе SDL 1.2 (буква — её ASCII) и сам строит символ из
 * кода и Shift. Поэтому код nsfb — буква US, а Shift, Ctrl и Alt приходят
 * отдельными нажатиями. Одно событие окна даёт иногда два события nsfb
 * (сдвиг и нажатие кнопки мыши) — для них маленькая очередь. */
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <freeos/window.h>

#include "libnsfb.h"
#include "libnsfb_event.h"
#include "libnsfb_plot.h"

#include "nsfb.h"
#include "plot.h"
#include "surface.h"

struct freeos_priv {
    int64_t id;
    /* Модификаторы, о которых nsfb уже сказано. */
    int mods;
    /* Очередь событий nsfb, порождённых одним событием окна. */
    nsfb_event_t queue[8];
    int head;
    int count;
    /* Палец (05.11): экран стоит портретом — это телефон, и нажатие левой
     * кнопки откладывается до отпускания. Палец поехал по вертикали — это
     * прокрутка колесом, а не щелчок и не выделение текста. */
    bool touch;
    bool held;
    bool scrolling;
    /* Палец от левого края вправо — «назад» (05.11), как в браузерах
     * телефонов: такой жест не прокручивает и не щёлкает. */
    bool swiping;
    int press_x, press_y, last_x, last_y;
};

/* Сколько точек пальцем — один щелчок колеса и с какого сдвига нажатие
 * становится прокруткой. */
#define SCROLL_STEP 28
#define SCROLL_SLOP 14

/* Жест «назад»: начало не дальше `BACK_EDGE` точек от левого края, путь
 * вправо не короче `BACK_SWIPE`. NetSurf ловит управляющий код
 * `FREEOS_CONTROL_BACK` в своём цикле событий (`ports/netsurf/freeos.patch`,
 * `frontends/framebuffer/gui.c`) — кнопка «назад» его окна, но без касания
 * панели и независимо от того, где фокус ввода. */
#define BACK_EDGE 24
#define BACK_SWIPE 100
#define FREEOS_CONTROL_BACK 0x4241

/* Формат точки окна — один на машину; `freeos_screen` говорит какой. */
static enum nsfb_format_e window_format(void)
{
    uint32_t width = 0, height = 0, pixel = 0;
    if (freeos_screen(&width, &height, &pixel) != 0 || width == 0) {
        return NSFB_FMT_XRGB8888;
    }
    /* RGB — красный в младшем байте: 0xXXBBGGRR. BGR — 0xXXRRGGBB. */
    return pixel == FREEOS_PIXEL_RGB ? NSFB_FMT_XBGR8888 : NSFB_FMT_XRGB8888;
}

static int freeos_defaults(nsfb_t *nsfb)
{
    nsfb->width = 800;
    nsfb->height = 600;
    nsfb->format = window_format();
    select_plotters(nsfb);
    return 0;
}

static int freeos_initialise(nsfb_t *nsfb)
{
    struct freeos_priv *priv;
    void *surface = NULL;

    if (nsfb->surface_priv != NULL) {
        return -1;
    }
    priv = calloc(1, sizeof(*priv));
    if (priv == NULL) {
        return -1;
    }
    /* Формат просит программа (NetSurf — XRGB8888 для 32 бит), а точки окна
     * такие, какие у машины: рисовальщики выбираются под окно. */
    nsfb->format = window_format();
    select_plotters(nsfb);

    priv->id = freeos_window_open("NetSurf", (uint32_t)nsfb->width, (uint32_t)nsfb->height, &surface);
    if (priv->id <= 0 || surface == NULL) {
        free(priv);
        return -1;
    }
    freeos_window_style(priv->id, FREEOS_WIN_STYLE_RESIZABLE);
    {
        /* Портретный экран — телефон: касания пальцем (см. `struct freeos_priv`). */
        uint32_t width = 0, height = 0, pixel = 0;
        priv->touch = freeos_screen(&width, &height, &pixel) == 0 && height > width;
    }
    nsfb->surface_priv = priv;
    nsfb->ptr = surface;
    nsfb->linelen = nsfb->width * 4;
    return 0;
}

static int freeos_set_geometry(nsfb_t *nsfb, int width, int height, enum nsfb_format_e format)
{
    struct freeos_priv *priv = nsfb->surface_priv;
    (void)format;

    if (width <= 0) {
        width = nsfb->width;
    }
    if (height <= 0) {
        height = nsfb->height;
    }
    if (priv != NULL && (width != nsfb->width || height != nsfb->height)) {
        void *surface = freeos_window_resize(priv->id, (uint32_t)width, (uint32_t)height);
        if (surface == NULL) {
            return -1;
        }
        nsfb->ptr = surface;
    }
    nsfb->width = width;
    nsfb->height = height;
    nsfb->format = window_format();
    select_plotters(nsfb);
    nsfb->linelen = nsfb->width * 4;
    return 0;
}

static int freeos_finalise(nsfb_t *nsfb)
{
    struct freeos_priv *priv = nsfb->surface_priv;
    if (priv != NULL) {
        freeos_window_close(priv->id);
        free(priv);
        nsfb->surface_priv = NULL;
        nsfb->ptr = NULL;
    }
    return 0;
}

static void push(struct freeos_priv *priv, const nsfb_event_t *event)
{
    if (priv->count < (int)(sizeof(priv->queue) / sizeof(priv->queue[0]))) {
        priv->queue[(priv->head + priv->count) % 8] = *event;
        priv->count++;
    }
}

static void push_key(struct freeos_priv *priv, enum nsfb_event_type_e type, enum nsfb_key_code_e code)
{
    nsfb_event_t event;
    memset(&event, 0, sizeof(event));
    event.type = type;
    event.value.keycode = code;
    push(priv, &event);
}

/* Имя клавиши или буква US — код nsfb. */
static enum nsfb_key_code_e keycode(const struct freeos_win_event *event)
{
    uint32_t code = event->code;

    switch (code) {
    case FREEOS_WIN_KEY_LEFT: return NSFB_KEY_LEFT;
    case FREEOS_WIN_KEY_RIGHT: return NSFB_KEY_RIGHT;
    case FREEOS_WIN_KEY_UP: return NSFB_KEY_UP;
    case FREEOS_WIN_KEY_DOWN: return NSFB_KEY_DOWN;
    case FREEOS_WIN_KEY_HOME: return NSFB_KEY_HOME;
    case FREEOS_WIN_KEY_END: return NSFB_KEY_END;
    case FREEOS_WIN_KEY_PAGE_UP: return NSFB_KEY_PAGEUP;
    case FREEOS_WIN_KEY_PAGE_DOWN: return NSFB_KEY_PAGEDOWN;
    case FREEOS_WIN_KEY_DELETE: return NSFB_KEY_DELETE;
    case FREEOS_WIN_KEY_MENU: return NSFB_KEY_MENU;
    case FREEOS_WIN_KEY_INSERT: return NSFB_KEY_INSERT;
    case FREEOS_WIN_KEY_SHIFT: return NSFB_KEY_LSHIFT;
    case FREEOS_WIN_KEY_CTRL: return NSFB_KEY_LCTRL;
    case FREEOS_WIN_KEY_ALT: return NSFB_KEY_LALT;
    case 0x08: return NSFB_KEY_BACKSPACE;
    case 0x1B: return NSFB_KEY_ESCAPE;
    case '\n':
    case '\r': return NSFB_KEY_RETURN;
    case '\t': return NSFB_KEY_TAB;
    default: break;
    }
    if (code >= FREEOS_WIN_KEY_F1 && code <= FREEOS_WIN_KEY_F12) {
        return (enum nsfb_key_code_e)(NSFB_KEY_F1 + (code - FREEOS_WIN_KEY_F1));
    }
    if (code >= FREEOS_WIN_KEY_NAMED) {
        return NSFB_KEY_UNKNOWN;
    }
    /* Буква раскладки US без Shift: символ из неё и Shift nsfb соберёт сам
     * (`fbtk_keycode_to_ucs4`). Кириллицы nsfb не знает вовсе. */
    if (event->y > 0 && event->y < 0x80) {
        return (enum nsfb_key_code_e)event->y;
    }
    if (code < 0x80) {
        if (code >= 'A' && code <= 'Z') {
            return (enum nsfb_key_code_e)(code - 'A' + 'a');
        }
        return (enum nsfb_key_code_e)code;
    }
    return NSFB_KEY_UNKNOWN;
}

/* Сказать nsfb о модификаторах, если маска окна разошлась с тем, что он
 * видел (модификатор, зажатый до того, как окно стало активным). */
static void sync_mods(struct freeos_priv *priv, int mods)
{
    static const struct {
        int bit;
        enum nsfb_key_code_e code;
    } keys[] = {
        { FREEOS_WIN_MOD_SHIFT, NSFB_KEY_LSHIFT },
        { FREEOS_WIN_MOD_CTRL, NSFB_KEY_LCTRL },
        { FREEOS_WIN_MOD_ALT, NSFB_KEY_LALT },
    };
    size_t i;
    for (i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) {
        int now = mods & keys[i].bit;
        if (now != (priv->mods & keys[i].bit)) {
            push_key(priv, now ? NSFB_EVENT_KEY_DOWN : NSFB_EVENT_KEY_UP, keys[i].code);
        }
    }
    priv->mods = mods;
}

static enum nsfb_key_code_e mouse_button(uint32_t code)
{
    switch (code) {
    case 2: return NSFB_KEY_MOUSE_3;
    case 4: return NSFB_KEY_MOUSE_2;
    default: return NSFB_KEY_MOUSE_1;
    }
}

static void move_to(struct freeos_priv *priv, int x, int y)
{
    nsfb_event_t event;
    memset(&event, 0, sizeof(event));
    event.type = NSFB_EVENT_MOVE_ABSOLUTE;
    event.value.vector.x = x;
    event.value.vector.y = y;
    push(priv, &event);
}

/* Событие окна — события nsfb в очередь. */
static void translate(struct freeos_priv *priv, const struct freeos_win_event *event)
{
    nsfb_event_t out;
    memset(&out, 0, sizeof(out));

    switch (event->kind) {
    case FREEOS_WIN_KEY:
    case FREEOS_WIN_KEYUP: {
        enum nsfb_key_code_e code = keycode(event);
        int bit = event->code == FREEOS_WIN_KEY_SHIFT  ? FREEOS_WIN_MOD_SHIFT
                  : event->code == FREEOS_WIN_KEY_CTRL ? FREEOS_WIN_MOD_CTRL
                  : event->code == FREEOS_WIN_KEY_ALT  ? FREEOS_WIN_MOD_ALT
                                                       : 0;
        if (bit) {
            /* Сам модификатор: его нажатие и есть смена маски. */
            if (event->kind == FREEOS_WIN_KEY) {
                priv->mods |= bit;
            } else {
                priv->mods &= ~bit;
            }
        } else {
            sync_mods(priv, event->x);
        }
        if (code != NSFB_KEY_UNKNOWN) {
            push_key(priv, event->kind == FREEOS_WIN_KEY ? NSFB_EVENT_KEY_DOWN : NSFB_EVENT_KEY_UP, code);
        }
        break;
    }
    case FREEOS_WIN_POINTER:
        if (priv->touch && mouse_button(event->code) == NSFB_KEY_MOUSE_1) {
            priv->held = true;
            priv->scrolling = false;
            priv->swiping = false;
            priv->press_x = event->x;
            priv->press_y = event->y;
            priv->last_x = event->x;
            priv->last_y = event->y;
            break;
        }
        move_to(priv, event->x, event->y);
        push_key(priv, NSFB_EVENT_KEY_DOWN, mouse_button(event->code));
        break;
    case FREEOS_WIN_RELEASE:
        if (priv->touch && priv->held && mouse_button(event->code) == NSFB_KEY_MOUSE_1) {
            priv->held = false;
            if (priv->swiping) {
                if (event->x - priv->press_x >= BACK_SWIPE || priv->last_x - priv->press_x >= BACK_SWIPE) {
                    out.type = NSFB_EVENT_CONTROL;
                    out.value.controlcode = (enum nsfb_control_e)FREEOS_CONTROL_BACK;
                    push(priv, &out);
                }
                break;
            }
            if (!priv->scrolling) {
                /* Тап: нажатие и отпускание в точке касания. */
                move_to(priv, priv->press_x, priv->press_y);
                push_key(priv, NSFB_EVENT_KEY_DOWN, NSFB_KEY_MOUSE_1);
                push_key(priv, NSFB_EVENT_KEY_UP, NSFB_KEY_MOUSE_1);
            }
            break;
        }
        move_to(priv, event->x, event->y);
        push_key(priv, NSFB_EVENT_KEY_UP, mouse_button(event->code));
        break;
    case FREEOS_WIN_MOVE:
        if (priv->touch && priv->held) {
            int dx = event->x - priv->press_x;
            int dy = event->y - priv->press_y;
            int ady = dy < 0 ? -dy : dy;
            priv->last_x = event->x;
            if (priv->swiping) {
                break;
            }
            /* У левого края палец, ушедший вправо больше, чем вбок по
             * вертикали, — жест «назад», а не прокрутка. */
            if (!priv->scrolling && priv->press_x < BACK_EDGE && dx > SCROLL_SLOP && dx > ady) {
                priv->swiping = true;
                break;
            }
            if (!priv->scrolling && ady > SCROLL_SLOP && (priv->press_x >= BACK_EDGE || ady >= dx)) {
                priv->scrolling = true;
            }
            if (priv->scrolling) {
                /* Палец вниз — страница к началу (колесо «от себя»). */
                int clicks = 0;
                while (event->y - priv->last_y >= SCROLL_STEP && clicks < 3) {
                    push_key(priv, NSFB_EVENT_KEY_DOWN, NSFB_KEY_MOUSE_4);
                    push_key(priv, NSFB_EVENT_KEY_UP, NSFB_KEY_MOUSE_4);
                    priv->last_y += SCROLL_STEP;
                    clicks++;
                }
                while (priv->last_y - event->y >= SCROLL_STEP && clicks < 3) {
                    push_key(priv, NSFB_EVENT_KEY_DOWN, NSFB_KEY_MOUSE_5);
                    push_key(priv, NSFB_EVENT_KEY_UP, NSFB_KEY_MOUSE_5);
                    priv->last_y -= SCROLL_STEP;
                    clicks++;
                }
            }
            break;
        }
        move_to(priv, event->x, event->y);
        break;
    case FREEOS_WIN_WHEEL: {
        /* Колесо у nsfb — кнопки 4 (от себя) и 5, нажатие и отпускание на
         * каждый щелчок. */
        int clicks = event->x > 0 ? event->x : -event->x;
        enum nsfb_key_code_e code = event->x > 0 ? NSFB_KEY_MOUSE_4 : NSFB_KEY_MOUSE_5;
        if (clicks > 3) {
            clicks = 3;
        }
        while (clicks-- > 0) {
            push_key(priv, NSFB_EVENT_KEY_DOWN, code);
            push_key(priv, NSFB_EVENT_KEY_UP, code);
        }
        break;
    }
    case FREEOS_WIN_RESIZE:
        out.type = NSFB_EVENT_RESIZE;
        out.value.resize.w = event->x;
        out.value.resize.h = event->y;
        push(priv, &out);
        break;
    case FREEOS_WIN_CLOSE:
        out.type = NSFB_EVENT_CONTROL;
        out.value.controlcode = NSFB_CONTROL_QUIT;
        push(priv, &out);
        break;
    default:
        /* Новые виды договор добавляет — незнакомый пропускается. */
        break;
    }
}

static uint64_t now_ms(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000u + (uint64_t)ts.tv_nsec / 1000000u;
}

/* События окна не ждут: ждёт программа, засыпая по чуть-чуть, пока не
 * придёт событие или не выйдет срок (`timeout` < 0 — без срока). Шаг 10 мс —
 * между отзывчивостью ввода и холостыми пробуждениями. */
static bool freeos_input(nsfb_t *nsfb, nsfb_event_t *event, int timeout)
{
    struct freeos_priv *priv = nsfb->surface_priv;
    uint64_t deadline = timeout > 0 ? now_ms() + (uint64_t)timeout : 0;

    if (priv == NULL) {
        return false;
    }
    for (;;) {
        struct freeos_win_event raw;
        int budget = 64;
        while (priv->count == 0 && budget-- > 0 && freeos_window_event(priv->id, &raw) == 1) {
            translate(priv, &raw);
        }
        if (priv->count > 0) {
            *event = priv->queue[priv->head];
            priv->head = (priv->head + 1) % 8;
            priv->count--;
            return true;
        }
        if (timeout == 0 || (timeout > 0 && now_ms() >= deadline)) {
            event->type = NSFB_EVENT_CONTROL;
            event->value.controlcode = NSFB_CONTROL_TIMEOUT;
            return true;
        }
        {
            struct timespec step = { 0, 10 * 1000 * 1000 };
            nanosleep(&step, NULL);
        }
    }
}

static int freeos_update(nsfb_t *nsfb, nsfb_bbox_t *box)
{
    struct freeos_priv *priv = nsfb->surface_priv;
    if (priv == NULL) {
        return -1;
    }
    freeos_window_commit(priv->id, box->x0, box->y0, box->x1 - box->x0, box->y1 - box->y0);
    return 0;
}

static const nsfb_surface_rtns_t freeos_rtns = {
    .defaults = freeos_defaults,
    .initialise = freeos_initialise,
    .finalise = freeos_finalise,
    .input = freeos_input,
    .update = freeos_update,
    .geometry = freeos_set_geometry,
};

NSFB_SURFACE_DEF(freeos, NSFB_SURFACE_FREEOS, &freeos_rtns)
