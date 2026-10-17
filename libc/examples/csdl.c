/* SDL2 на FreeOS (фаза 68b): окно, рисование, таймер, звук и ввод — тем
 * путём, каким ими пользуется всякая игра на SDL.
 *
 * # Что она доказывает
 *
 * - Окно стола открывается через `SDL_CreateWindow`, программный рисовальщик
 *   SDL рисует в его поверхность: прямоугольники читаются обратно
 *   (`SDL_RenderReadPixels`) ровно своего цвета.
 * - Таймер SDL срабатывает в своём потоке (потоки POSIX, часы Unix).
 * - Звук: программа просит 44 100 Гц, float, моно — SDL сам пересчитывает
 *   в 48 кГц, 16 бит, стерео, а драйвер отдаёт это в `SYS_SOUND`. Тон
 *   слышит стенд — в WAV у QEMU.
 * - Ввод: щелчок, набранный текст, клавиша и крестик окна приходят
 *   событиями SDL. Щелчок оставляет в окне жёлтый квадрат.
 *
 * Аргументы: частота тона и его длина в миллисекундах (440 и 1500). Итог —
 * одна строка с числом проваленных проверок.
 */

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <SDL.h>

#define W 320
#define H 200

static int failures;

static void check(int passed, const char *what) {
    printf("csdl: %s %s\n", passed ? "ok" : "FAILED", what);
    if (!passed) {
        failures++;
    }
}

/* --- звук ----------------------------------------------------------------- */

static double tone_hz = 440.0;
static double phase;
static int callbacks;

static void play(void *userdata, Uint8 *stream, int len) {
    float *out = (float *)stream;
    int frames = len / (int)sizeof(float);
    (void)userdata;
    for (int i = 0; i < frames; i++) {
        out[i] = 0.25f * (float)sin(phase);
        phase += 2.0 * M_PI * tone_hz / 44100.0;
        if (phase > 2.0 * M_PI) {
            phase -= 2.0 * M_PI;
        }
    }
    callbacks++;
}

/* --- таймер --------------------------------------------------------------- */

static SDL_atomic_t fired;

static Uint32 tick(Uint32 interval, void *param) {
    (void)interval;
    (void)param;
    SDL_AtomicSet(&fired, 1);
    return 0;
}

/* --- рисование ------------------------------------------------------------ */

static Uint32 read_pixel(SDL_Renderer *renderer, int x, int y) {
    Uint32 pixel = 0;
    SDL_Rect rect = {x, y, 1, 1};
    SDL_RenderReadPixels(renderer, &rect, SDL_PIXELFORMAT_ARGB8888, &pixel, 4);
    return pixel & 0x00ffffffu;
}

static void draw(SDL_Renderer *renderer) {
    SDL_Rect red = {20, 20, 100, 60};
    SDL_Rect green = {200, 120, 100, 60};
    SDL_SetRenderDrawColor(renderer, 20, 30, 60, 255);
    SDL_RenderClear(renderer);
    SDL_SetRenderDrawColor(renderer, 255, 0, 0, 255);
    SDL_RenderFillRect(renderer, &red);
    SDL_SetRenderDrawColor(renderer, 0, 255, 0, 255);
    SDL_RenderFillRect(renderer, &green);
}

int main(int argc, char **argv) {
    int ms = argc > 2 ? atoi(argv[2]) : 1500;
    tone_hz = argc > 1 ? atof(argv[1]) : 440.0;
    /* Строка — одной записью: без буфера `printf` пишет кусками, и строку
     * разрывало сообщение ядра посреди (так было на aarch64). */
    static char line_buffer[512];
    setvbuf(stdout, line_buffer, _IOLBF, sizeof(line_buffer));

    SDL_version linked;
    SDL_GetVersion(&linked);
    if (SDL_Init(SDL_INIT_VIDEO | SDL_INIT_AUDIO | SDL_INIT_TIMER) != 0) {
        printf("csdl: FAILED SDL_Init: %s\n", SDL_GetError());
        return 1;
    }
    printf("csdl: SDL %d.%d.%d, video %s, audio %s\n", linked.major, linked.minor, linked.patch,
           SDL_GetCurrentVideoDriver(), SDL_GetCurrentAudioDriver() ? SDL_GetCurrentAudioDriver() : "none");

    SDL_Window *window = SDL_CreateWindow("SDL Demo", SDL_WINDOWPOS_UNDEFINED, SDL_WINDOWPOS_UNDEFINED, W, H, 0);
    SDL_Renderer *renderer = window ? SDL_CreateRenderer(window, -1, SDL_RENDERER_SOFTWARE) : NULL;
    if (!renderer) {
        printf("csdl: FAILED window or renderer: %s\n", SDL_GetError());
        return 1;
    }
    SDL_RendererInfo info;
    SDL_GetRendererInfo(renderer, &info);
    draw(renderer);
    Uint32 inside_red = read_pixel(renderer, 60, 50);
    Uint32 inside_green = read_pixel(renderer, 250, 150);
    Uint32 between = read_pixel(renderer, 160, 100);
    SDL_RenderPresent(renderer);
    printf("csdl: shown %dx%d, renderer %s, pixels %06x %06x %06x\n", W, H, info.name, (unsigned)inside_red,
           (unsigned)inside_green, (unsigned)between);
    check(inside_red == 0xff0000u && inside_green == 0x00ff00u && between == 0x141e3cu,
          "rectangles are drawn in their colours");

    /* Таймер. */
    SDL_TimerID timer = SDL_AddTimer(50, tick, NULL);
    Uint32 waited = SDL_GetTicks();
    while (!SDL_AtomicGet(&fired) && SDL_GetTicks() - waited < 2000) {
        SDL_Delay(10);
    }
    check(timer != 0 && SDL_AtomicGet(&fired), "a timer fires on its own thread");

    /* Звук: 44,1 кГц float моно — SDL пересчитывает сам. */
    SDL_AudioSpec want, have;
    SDL_zero(want);
    want.freq = 44100;
    want.format = AUDIO_F32SYS;
    want.channels = 1;
    want.samples = 1024;
    want.callback = play;
    SDL_AudioDeviceID device = SDL_OpenAudioDevice(NULL, 0, &want, &have, 0);
    if (device == 0) {
        printf("csdl: FAILED audio: %s\n", SDL_GetError());
        failures++;
    } else {
        Uint32 started = SDL_GetTicks();
        SDL_PauseAudioDevice(device, 0);
        while (SDL_GetTicks() - started < (Uint32)ms) {
            SDL_PumpEvents();
            SDL_Delay(20);
        }
        SDL_PauseAudioDevice(device, 1);
        SDL_CloseAudioDevice(device);
        printf("csdl: played %d ms of %d Hz, %d callbacks\n", ms, (int)tone_hz, callbacks);
        check(callbacks > 0, "the audio callback runs");
    }

    /* Ввод: щелчок, текст, клавиши, крестик. */
    SDL_StartTextInput();
    printf("csdl: waiting for input\n");
    char text[64] = "";
    int clicks = 0;
    int running = 1;
    Uint32 deadline = SDL_GetTicks() + 300000;
    while (running && SDL_GetTicks() < deadline) {
        SDL_Event event;
        if (!SDL_WaitEventTimeout(&event, 100)) {
            continue;
        }
        switch (event.type) {
        case SDL_MOUSEBUTTONDOWN: {
            clicks++;
            printf("csdl: click %d button %d at %d,%d\n", clicks, event.button.button, event.button.x,
                   event.button.y);
            SDL_Rect mark = {event.button.x - 5, event.button.y - 5, 10, 10};
            SDL_SetRenderDrawColor(renderer, 255, 255, 0, 255);
            SDL_RenderFillRect(renderer, &mark);
            SDL_RenderPresent(renderer);
            break;
        }
        case SDL_TEXTINPUT:
            if (strlen(text) + strlen(event.text.text) < sizeof(text)) {
                strcat(text, event.text.text);
            }
            printf("csdl: text '%s'\n", text);
            break;
        case SDL_MOUSEWHEEL:
            printf("csdl: wheel %d,%d\n", event.wheel.x, event.wheel.y);
            break;
        case SDL_KEYDOWN:
            printf("csdl: key %s\n", SDL_GetScancodeName(event.key.keysym.scancode));
            break;
        case SDL_QUIT:
            printf("csdl: quit\n");
            running = 0;
            break;
        default:
            break;
        }
    }

    SDL_DestroyRenderer(renderer);
    SDL_DestroyWindow(window);
    SDL_Quit();
    printf("csdl: done, %d check(s) failed\n", failures);
    return failures == 0 ? 0 : 1;
}
