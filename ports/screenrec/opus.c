/* Opus for the screen recorder (23.11): a small C face of libopus for the
 * Rust side (`crates/screencast::media`).
 *
 * 48 kHz stereo, 20 ms frames (960 samples per channel), the "audio"
 * application (music and system sounds, not speech tuning), constrained VBR.
 * libopus is built fixed-point (`FIXED_POINT`): the input is 16-bit PCM
 * already, and the fixed-point build needs no libm beyond a handful of
 * functions.
 *
 * BSD-3-Clause like libopus itself.
 */

#include <stdint.h>

#include "opus.h"

/* Create an encoder; null — failure. `*lookahead` — pre-skip samples for the
 * MP4 `dOps` box and its edit list. */
void *fopus_open(int32_t bitrate, int32_t complexity, int32_t *lookahead) {
    int err = 0;
    OpusEncoder *enc = opus_encoder_create(48000, 2, OPUS_APPLICATION_AUDIO, &err);
    if (enc == 0 || err != OPUS_OK) {
        return 0;
    }
    opus_encoder_ctl(enc, OPUS_SET_BITRATE(bitrate > 0 ? bitrate : 128000));
    opus_encoder_ctl(enc, OPUS_SET_COMPLEXITY(complexity < 0 ? 0 : (complexity > 10 ? 10 : complexity)));
    opus_encoder_ctl(enc, OPUS_SET_VBR(1));
    opus_encoder_ctl(enc, OPUS_SET_VBR_CONSTRAINT(1));
    opus_encoder_ctl(enc, OPUS_SET_SIGNAL(OPUS_SIGNAL_MUSIC));
    opus_int32 skip = 0;
    opus_encoder_ctl(enc, OPUS_GET_LOOKAHEAD(&skip));
    *lookahead = skip;
    return enc;
}

/* Encode `frames` (120, 240, 480, 960, 1920 or 2880) interleaved stereo
 * samples. Answer — bytes of the packet, negative — an Opus error. */
int32_t fopus_encode(void *handle, const int16_t *pcm, int32_t frames, uint8_t *out, int32_t cap) {
    return opus_encode((OpusEncoder *)handle, pcm, frames, out, cap);
}

void fopus_close(void *handle) {
    if (handle != 0) {
        opus_encoder_destroy((OpusEncoder *)handle);
    }
}
