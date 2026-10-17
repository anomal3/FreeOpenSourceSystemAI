/* Звук SDL2 через `SYS_SOUND` (фаза 68b).
 *
 * Устройство одно, формат один — 16 бит со знаком, 48 кГц, два канала
 * (`<freeos/sound.h>`); всё остальное, что попросит программа, SDL
 * пересчитывает сам. Темп задаёт звуковая карта: `freeos_sound_write` ждёт
 * места, поэтому `WaitDevice` не нужен — поток SDL спит в записи.
 */
#include "../../SDL_internal.h"

#ifdef SDL_AUDIO_DRIVER_FREEOS

#include <errno.h>
#include <freeos/sound.h>

#include "SDL_audio.h"
#include "../SDL_audio_c.h"
#include "../SDL_sysaudio.h"

#define _THIS SDL_AudioDevice *_this

struct SDL_PrivateAudioData
{
    Uint8 *mixbuf;
    Uint32 mixlen;
};

static int FREEOSAUDIO_OpenDevice(_THIS, const char *devname)
{
    struct SDL_PrivateAudioData *hidden;

    if (_this->iscapture) {
        return SDL_SetError("freeos: no sound input");
    }
    hidden = (struct SDL_PrivateAudioData *)SDL_calloc(1, sizeof(*hidden));
    if (!hidden) {
        return SDL_OutOfMemory();
    }
    _this->hidden = hidden;

    _this->spec.freq = FREEOS_SOUND_RATE;
    _this->spec.format = AUDIO_S16LSB;
    _this->spec.channels = FREEOS_SOUND_CHANNELS;
    if (_this->spec.samples == 0) {
        _this->spec.samples = 1024;
    }
    SDL_CalculateAudioSpec(&_this->spec);

    hidden->mixlen = _this->spec.size;
    hidden->mixbuf = (Uint8 *)SDL_malloc(hidden->mixlen);
    if (!hidden->mixbuf) {
        return SDL_OutOfMemory();
    }
    SDL_memset(hidden->mixbuf, _this->spec.silence, hidden->mixlen);
    return 0;
}

static void FREEOSAUDIO_PlayDevice(_THIS)
{
    const Uint8 *at = _this->hidden->mixbuf;
    size_t left = _this->hidden->mixlen;
    while (left > 0) {
        ssize_t taken = freeos_sound_write(at, left);
        if (taken <= 0) {
            SDL_OpenedAudioDeviceDisconnected(_this);
            return;
        }
        at += taken;
        left -= (size_t)taken;
    }
}

static Uint8 *FREEOSAUDIO_GetDeviceBuf(_THIS)
{
    return _this->hidden->mixbuf;
}

static void FREEOSAUDIO_CloseDevice(_THIS)
{
    freeos_sound_drain();
    SDL_free(_this->hidden->mixbuf);
    SDL_free(_this->hidden);
}

static SDL_bool FREEOSAUDIO_Init(SDL_AudioDriverImpl *impl)
{
    /* Пустая запись ничего не играет, но отвечает `ENOSYS`, если карты нет. */
    if (freeos_sound_write(NULL, 0) < 0) {
        return SDL_FALSE;
    }
    impl->OpenDevice = FREEOSAUDIO_OpenDevice;
    impl->PlayDevice = FREEOSAUDIO_PlayDevice;
    impl->GetDeviceBuf = FREEOSAUDIO_GetDeviceBuf;
    impl->CloseDevice = FREEOSAUDIO_CloseDevice;
    impl->OnlyHasDefaultOutputDevice = SDL_TRUE;
    impl->SupportsNonPow2Samples = SDL_TRUE;
    return SDL_TRUE;
}

AudioBootStrap FREEOSAUDIO_bootstrap = {
    "freeos", "SDL audio through SYS_SOUND", FREEOSAUDIO_Init, SDL_FALSE
};

#endif /* SDL_AUDIO_DRIVER_FREEOS */
