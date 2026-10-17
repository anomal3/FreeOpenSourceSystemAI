/* Видео SDL2 на окнах стола FreeOS (фаза 68b).
 *
 * Окно SDL — окно стола (`<freeos/window.h>`). Рисует программный
 * рисовальщик SDL, а кадр окна — это **сама поверхность окна**: SDL пишет
 * точки прямо в память, которую видит стол, и копировать нечего;
 * `SDL_UpdateWindowSurface` сообщает столу изменённые прямоугольники.
 *
 * Ввод — события окна. Договор отдаёт символ клавиши и её букву в раскладке
 * US, а не код клавиши, поэтому код SDL (`SDL_Scancode`) собирается из
 * буквы, имени клавиши или управляющего символа. С фазы 68c именами приходят
 * и F1–F12, Insert и сами Shift, Ctrl и Alt; маска модификаторов при каждой
 * клавише сверяется с тем, что SDL уже видел, — на случай модификатора,
 * зажатого до того, как окно стало активным.
 */
#include "../../SDL_internal.h"

#ifdef SDL_VIDEO_DRIVER_FREEOS

#include <freeos/window.h>

#include "SDL_video.h"
#include "SDL_hints.h"
#include "SDL_mouse.h"
#include "../SDL_sysvideo.h"
#include "../SDL_pixels_c.h"
#include "../../events/SDL_events_c.h"
#include "../../events/SDL_keyboard_c.h"
#include "../../events/SDL_mouse_c.h"
#include "../../events/SDL_windowevents_c.h"

#define FREEOS_DRIVER_NAME "freeos"

typedef struct
{
    int64_t id;
    void *surface;
    int width;
    int height;
    /* Маска кнопок, которую SDL уже видел. */
    Uint32 buttons;
} FREEOS_Window;

/* Формат точки окна — один на машину. */
static Uint32 window_format = SDL_PIXELFORMAT_XRGB8888;
/* Модификаторы, которые SDL уже видел нажатыми. */
static int held_mods;

static int FREEOS_VideoInit(_THIS)
{
    uint32_t width = 0, height = 0, pixel = 0;
    SDL_DisplayMode mode;

    if (freeos_screen(&width, &height, &pixel) != 0 || width == 0) {
        return SDL_SetError("freeos: no graphics on this machine");
    }
    /* RGB — красный в младшем байте: 0xXXBBGGRR. BGR — 0xXXRRGGBB. */
    window_format = pixel == FREEOS_PIXEL_RGB ? SDL_PIXELFORMAT_XBGR8888 : SDL_PIXELFORMAT_XRGB8888;

    /* Рисовальщик один — программный. Программа, которая просит ускоренный
     * (`SDL_RENDERER_ACCELERATED`, так делает doomgeneric), без подсказки
     * получила бы отказ; по имени SDL выбирает, не глядя на флаги (фаза 68d).
     * Переменная окружения `SDL_RENDER_DRIVER` по-прежнему сильнее. */
    SDL_SetHintWithPriority(SDL_HINT_RENDER_DRIVER, "software", SDL_HINT_DEFAULT);

    SDL_zero(mode);
    mode.format = window_format;
    mode.w = (int)width;
    mode.h = (int)height;
    mode.refresh_rate = 60;
    if (SDL_AddBasicVideoDisplay(&mode) < 0) {
        return -1;
    }
    SDL_AddDisplayMode(&_this->displays[0], &mode);
    return 0;
}

static void FREEOS_VideoQuit(_THIS)
{
}

/* Окно стола открывается не в `CreateSDLWindow`, а при первом показе или
 * первой поверхности: SDL создаёт окно **до** того, как задаёт ему заголовок,
 * и стол открыл бы его под чужим именем. Скрытое окно SDL до показа на стол
 * не попадает вовсе. */
static int FREEOS_Open(SDL_Window *window)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    const char *title = (window->title && *window->title) ? window->title : "SDL";

    if (data->id > 0) {
        return 0;
    }
    data->id = freeos_window_open(title, (uint32_t)window->w, (uint32_t)window->h, &data->surface);
    if (data->id < 0) {
        data->id = 0;
        return SDL_SetError("freeos: the desktop refused a %dx%d window", window->w, window->h);
    }
    data->width = window->w;
    data->height = window->h;
    if (window->flags & SDL_WINDOW_RESIZABLE) {
        freeos_window_style(data->id, FREEOS_WIN_STYLE_RESIZABLE);
    }
    /* Новое окно стол делает активным. */
    SDL_SetKeyboardFocus(window);
    return 0;
}

static int FREEOS_CreateWindow(_THIS, SDL_Window *window)
{
    FREEOS_Window *data = (FREEOS_Window *)SDL_calloc(1, sizeof(*data));
    if (!data) {
        return SDL_OutOfMemory();
    }
    window->driverdata = data;
    return 0;
}

static void FREEOS_ShowWindow(_THIS, SDL_Window *window)
{
    FREEOS_Open(window);
}

static void FREEOS_SetWindowTitle(_THIS, SDL_Window *window)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    if (data && data->id > 0 && window->title && *window->title) {
        freeos_window_title(data->id, window->title);
    }
}

/* Поверхность того размера, что у окна SDL: ответ — удалось ли. */
static int FREEOS_Fit(FREEOS_Window *data, int w, int h)
{
    void *surface;
    if (data->width == w && data->height == h) {
        return 0;
    }
    surface = freeos_window_resize(data->id, (uint32_t)w, (uint32_t)h);
    if (!surface) {
        return SDL_SetError("freeos: the desktop refused size %dx%d", w, h);
    }
    data->surface = surface;
    data->width = w;
    data->height = h;
    return 0;
}

static void FREEOS_SetWindowSize(_THIS, SDL_Window *window)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    if (data && data->id > 0) {
        FREEOS_Fit(data, window->w, window->h);
    }
}

static void FREEOS_SetWindowResizable(_THIS, SDL_Window *window, SDL_bool resizable)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    if (data && data->id > 0) {
        freeos_window_style(data->id, resizable ? FREEOS_WIN_STYLE_RESIZABLE : 0);
    }
}

static void FREEOS_DestroyWindow(_THIS, SDL_Window *window)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    if (data) {
        if (data->id > 0) {
            freeos_window_close(data->id);
        }
        SDL_free(data);
        window->driverdata = NULL;
    }
}

static int FREEOS_CreateWindowFramebuffer(_THIS, SDL_Window *window, Uint32 *format, void **pixels, int *pitch)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    int w, h;

    if (!data) {
        return SDL_SetError("freeos: no window");
    }
    if (FREEOS_Open(window) < 0) {
        return -1;
    }
    SDL_GetWindowSizeInPixels(window, &w, &h);
    if (FREEOS_Fit(data, w, h) < 0) {
        return -1;
    }
    *format = window_format;
    *pixels = data->surface;
    *pitch = data->width * 4;
    return 0;
}

static int FREEOS_UpdateWindowFramebuffer(_THIS, SDL_Window *window, const SDL_Rect *rects, int numrects)
{
    FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
    int i;

    if (!data || data->id <= 0) {
        return SDL_SetError("freeos: no window");
    }
    if (numrects <= 0) {
        return freeos_window_commit(data->id, 0, 0, 0, 0);
    }
    for (i = 0; i < numrects; ++i) {
        if (rects[i].w > 0 && rects[i].h > 0) {
            freeos_window_commit(data->id, rects[i].x, rects[i].y, rects[i].w, rects[i].h);
        }
    }
    return 0;
}

static void FREEOS_DestroyWindowFramebuffer(_THIS, SDL_Window *window)
{
    /* Поверхность принадлежит окну и уходит вместе с ним. */
}

/* --- ввод --------------------------------------------------------------- */

/* Буква клавиши в раскладке US — код SDL. */
static SDL_Scancode FREEOS_LatinScancode(int latin)
{
    if (latin >= 'a' && latin <= 'z') {
        return (SDL_Scancode)(SDL_SCANCODE_A + (latin - 'a'));
    }
    if (latin >= '1' && latin <= '9') {
        return (SDL_Scancode)(SDL_SCANCODE_1 + (latin - '1'));
    }
    switch (latin) {
    case '0': return SDL_SCANCODE_0;
    case '-': return SDL_SCANCODE_MINUS;
    case '=': return SDL_SCANCODE_EQUALS;
    case '[': return SDL_SCANCODE_LEFTBRACKET;
    case ']': return SDL_SCANCODE_RIGHTBRACKET;
    case '\\': return SDL_SCANCODE_BACKSLASH;
    case ';': return SDL_SCANCODE_SEMICOLON;
    case '\'': return SDL_SCANCODE_APOSTROPHE;
    case '`': return SDL_SCANCODE_GRAVE;
    case ',': return SDL_SCANCODE_COMMA;
    case '.': return SDL_SCANCODE_PERIOD;
    case '/': return SDL_SCANCODE_SLASH;
    case ' ': return SDL_SCANCODE_SPACE;
    default: return SDL_SCANCODE_UNKNOWN;
    }
}

static SDL_Scancode FREEOS_Scancode(const struct freeos_win_event *event)
{
    switch (event->code) {
    case FREEOS_WIN_KEY_LEFT: return SDL_SCANCODE_LEFT;
    case FREEOS_WIN_KEY_RIGHT: return SDL_SCANCODE_RIGHT;
    case FREEOS_WIN_KEY_UP: return SDL_SCANCODE_UP;
    case FREEOS_WIN_KEY_DOWN: return SDL_SCANCODE_DOWN;
    case FREEOS_WIN_KEY_HOME: return SDL_SCANCODE_HOME;
    case FREEOS_WIN_KEY_END: return SDL_SCANCODE_END;
    case FREEOS_WIN_KEY_PAGE_UP: return SDL_SCANCODE_PAGEUP;
    case FREEOS_WIN_KEY_PAGE_DOWN: return SDL_SCANCODE_PAGEDOWN;
    case FREEOS_WIN_KEY_DELETE: return SDL_SCANCODE_DELETE;
    case FREEOS_WIN_KEY_MENU: return SDL_SCANCODE_APPLICATION;
    case FREEOS_WIN_KEY_INSERT: return SDL_SCANCODE_INSERT;
    case FREEOS_WIN_KEY_SHIFT: return SDL_SCANCODE_LSHIFT;
    case FREEOS_WIN_KEY_CTRL: return SDL_SCANCODE_LCTRL;
    case FREEOS_WIN_KEY_ALT: return SDL_SCANCODE_LALT;
    case 0x08: return SDL_SCANCODE_BACKSPACE;
    case 0x1B: return SDL_SCANCODE_ESCAPE;
    case '\n':
    case '\r': return SDL_SCANCODE_RETURN;
    case '\t': return SDL_SCANCODE_TAB;
    case ' ': return SDL_SCANCODE_SPACE;
    default: break;
    }
    if (event->code >= FREEOS_WIN_KEY_F1 && event->code <= FREEOS_WIN_KEY_F12) {
        return (SDL_Scancode)(SDL_SCANCODE_F1 + (event->code - FREEOS_WIN_KEY_F1));
    }
    if (event->code >= FREEOS_WIN_KEY_NAMED) {
        /* Имя из будущего договора — не буква. */
        return SDL_SCANCODE_UNKNOWN;
    }
    /* Буква раскладки US — и для Ctrl+H, и для «щ» на месте «o». */
    if (event->y != 0) {
        return FREEOS_LatinScancode(event->y);
    }
    if (event->code < 0x80) {
        return FREEOS_LatinScancode((int)event->code);
    }
    return SDL_SCANCODE_UNKNOWN;
}

/* Сказать SDL о модификаторах, если маска изменилась. */
static void FREEOS_SyncMods(int mods)
{
    static const struct
    {
        int bit;
        SDL_Scancode code;
    } keys[] = {
        { FREEOS_WIN_MOD_SHIFT, SDL_SCANCODE_LSHIFT },
        { FREEOS_WIN_MOD_CTRL, SDL_SCANCODE_LCTRL },
        { FREEOS_WIN_MOD_ALT, SDL_SCANCODE_LALT },
    };
    size_t i;
    for (i = 0; i < SDL_arraysize(keys); ++i) {
        int now = mods & keys[i].bit;
        if (now != (held_mods & keys[i].bit)) {
            SDL_SendKeyboardKey(now ? SDL_PRESSED : SDL_RELEASED, keys[i].code);
        }
    }
    held_mods = mods;
}

/* Символ клавиши — текст, если это печатный символ без Ctrl и Alt. */
static void FREEOS_SendText(Uint32 code, int mods)
{
    char text[5];
    if (code < 0x20 || code == 0x7F || code >= FREEOS_WIN_KEY_NAMED || (code >= 0xD800 && code < 0xE000) ||
        code > 0x10FFFF || (mods & (FREEOS_WIN_MOD_CTRL | FREEOS_WIN_MOD_ALT))) {
        return;
    }
    if (code < 0x80) {
        text[0] = (char)code;
        text[1] = 0;
    } else if (code < 0x800) {
        text[0] = (char)(0xC0 | (code >> 6));
        text[1] = (char)(0x80 | (code & 0x3F));
        text[2] = 0;
    } else if (code < 0x10000) {
        text[0] = (char)(0xE0 | (code >> 12));
        text[1] = (char)(0x80 | ((code >> 6) & 0x3F));
        text[2] = (char)(0x80 | (code & 0x3F));
        text[3] = 0;
    } else {
        text[0] = (char)(0xF0 | (code >> 18));
        text[1] = (char)(0x80 | ((code >> 12) & 0x3F));
        text[2] = (char)(0x80 | ((code >> 6) & 0x3F));
        text[3] = (char)(0x80 | (code & 0x3F));
        text[4] = 0;
    }
    SDL_SendKeyboardText(text);
}

/* Кнопки стола (бит 0 — левая, бит 1 — правая) — кнопки SDL. */
static void FREEOS_SyncButtons(SDL_Window *window, FREEOS_Window *data, Uint32 buttons)
{
    static const struct
    {
        Uint32 bit;
        Uint8 button;
    } map[] = { { 1, SDL_BUTTON_LEFT }, { 2, SDL_BUTTON_RIGHT }, { 4, SDL_BUTTON_MIDDLE } };
    size_t i;
    for (i = 0; i < SDL_arraysize(map); ++i) {
        Uint32 now = buttons & map[i].bit;
        if (now != (data->buttons & map[i].bit)) {
            SDL_SendMouseButton(window, 0, now ? SDL_PRESSED : SDL_RELEASED, map[i].button);
        }
    }
    data->buttons = buttons;
}

static void FREEOS_HandleEvent(SDL_Window *window, FREEOS_Window *data, const struct freeos_win_event *event)
{
    switch (event->kind) {
    case FREEOS_WIN_KEY:
    case FREEOS_WIN_KEYUP:
    {
        SDL_Scancode code = FREEOS_Scancode(event);
        int bit = event->code == FREEOS_WIN_KEY_SHIFT  ? FREEOS_WIN_MOD_SHIFT
                  : event->code == FREEOS_WIN_KEY_CTRL ? FREEOS_WIN_MOD_CTRL
                  : event->code == FREEOS_WIN_KEY_ALT  ? FREEOS_WIN_MOD_ALT
                                                       : 0;
        if (bit) {
            /* Сам модификатор (фаза 68c): его нажатие и есть смена маски. */
            if (event->kind == FREEOS_WIN_KEY) {
                held_mods |= bit;
            } else {
                held_mods &= ~bit;
            }
        } else {
            FREEOS_SyncMods(event->x);
        }
        if (code != SDL_SCANCODE_UNKNOWN) {
            SDL_SendKeyboardKey(event->kind == FREEOS_WIN_KEY ? SDL_PRESSED : SDL_RELEASED, code);
        }
        if (event->kind == FREEOS_WIN_KEY) {
            FREEOS_SendText(event->code, event->x);
        }
        break;
    }
    case FREEOS_WIN_POINTER:
        SDL_SetMouseFocus(window);
        SDL_SendMouseMotion(window, 0, 0, event->x, event->y);
        FREEOS_SyncButtons(window, data, data->buttons | event->code);
        break;
    case FREEOS_WIN_RELEASE:
        SDL_SendMouseMotion(window, 0, 0, event->x, event->y);
        FREEOS_SyncButtons(window, data, data->buttons & ~event->code);
        break;
    case FREEOS_WIN_MOVE:
        SDL_SetMouseFocus(window);
        SDL_SendMouseMotion(window, 0, 0, event->x, event->y);
        break;
    case FREEOS_WIN_WHEEL:
        SDL_SendMouseWheel(window, 0, 0.0f, (float)event->x, SDL_MOUSEWHEEL_NORMAL);
        break;
    case FREEOS_WIN_LEAVE:
        SDL_SetMouseFocus(NULL);
        break;
    case FREEOS_WIN_RESIZE:
        SDL_SendWindowEvent(window, SDL_WINDOWEVENT_RESIZED, event->x, event->y);
        break;
    case FREEOS_WIN_FOCUS:
        SDL_SetKeyboardFocus(event->code ? window : NULL);
        break;
    case FREEOS_WIN_CLOSE:
        SDL_SendWindowEvent(window, SDL_WINDOWEVENT_CLOSE, 0, 0);
        break;
    default:
        /* Новые виды договор добавляет — незнакомый пропускается. */
        break;
    }
}

static void FREEOS_PumpEvents(_THIS)
{
    SDL_Window *window;
    for (window = _this->windows; window; window = window->next) {
        FREEOS_Window *data = (FREEOS_Window *)window->driverdata;
        struct freeos_win_event event;
        int budget = 256;
        if (!data || data->id <= 0) {
            continue;
        }
        while (budget-- > 0 && freeos_window_event(data->id, &event) == 1) {
            FREEOS_HandleEvent(window, data, &event);
        }
    }
}

/* --- устройство ---------------------------------------------------------- */

static void FREEOS_DeleteDevice(SDL_VideoDevice *device)
{
    SDL_free(device);
}

static SDL_VideoDevice *FREEOS_CreateDevice(void)
{
    SDL_VideoDevice *device = (SDL_VideoDevice *)SDL_calloc(1, sizeof(SDL_VideoDevice));
    if (!device) {
        SDL_OutOfMemory();
        return NULL;
    }
    device->VideoInit = FREEOS_VideoInit;
    device->VideoQuit = FREEOS_VideoQuit;
    device->PumpEvents = FREEOS_PumpEvents;
    device->CreateSDLWindow = FREEOS_CreateWindow;
    device->ShowWindow = FREEOS_ShowWindow;
    device->SetWindowTitle = FREEOS_SetWindowTitle;
    device->SetWindowSize = FREEOS_SetWindowSize;
    device->SetWindowResizable = FREEOS_SetWindowResizable;
    device->DestroyWindow = FREEOS_DestroyWindow;
    device->CreateWindowFramebuffer = FREEOS_CreateWindowFramebuffer;
    device->UpdateWindowFramebuffer = FREEOS_UpdateWindowFramebuffer;
    device->DestroyWindowFramebuffer = FREEOS_DestroyWindowFramebuffer;
    device->free = FREEOS_DeleteDevice;
    return device;
}

VideoBootStrap FREEOS_bootstrap = {
    FREEOS_DRIVER_NAME, "SDL video on FreeOS desktop windows",
    FREEOS_CreateDevice,
    NULL /* no ShowMessageBox implementation */
};

#endif /* SDL_VIDEO_DRIVER_FREEOS */
