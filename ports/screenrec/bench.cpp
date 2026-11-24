// Host benchmark of the recorder's H.264 settings (23.11): synthetic
// desktop-like 4:2:0 pictures (a static "text" background, a window moving
// across it, a ticking clock area), encoded with `fh264_*` from h264.cpp.
//
//   bench <width> <height> <frames> <threads> <bitrate_kbit> [out.h264] [screen] [cabac] [complexity]
//
// Prints ms per frame and the bitrate it produced; the raw Annex B stream
// goes to `out.h264` for ffprobe/ffplay. Built by `xtask thirdparty --only
// screenrec-media` for the host (see ROADMAP «Запись экрана»).

#include <chrono>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <vector>

extern "C" {
struct FH264Config {
    int32_t width, height, fps, bitrate, max_bitrate, threads, keyint, screen, cabac, complexity;
};
void* fh264_open(const FH264Config*);
int32_t fh264_encode(void*, const uint8_t*, const uint8_t*, const uint8_t*, int32_t, int32_t, int64_t, uint8_t*, int32_t, int32_t*);
int32_t fh264_headers(void*, uint8_t*, int32_t);
void fh264_close(void*);
}

static uint32_t rng = 12345;
static uint32_t next() {
    rng ^= rng << 13;
    rng ^= rng >> 17;
    rng ^= rng << 5;
    return rng;
}

int main(int argc, char** argv) {
    int w = argc > 1 ? atoi(argv[1]) : 2560;
    int h = argc > 2 ? atoi(argv[2]) : 1440;
    int frames = argc > 3 ? atoi(argv[3]) : 120;
    int threads = argc > 4 ? atoi(argv[4]) : 8;
    int kbit = argc > 5 ? atoi(argv[5]) : 12000;
    const char* path = argc > 6 ? argv[6] : nullptr;
    FH264Config c = {w, h, 30, kbit * 1000, 0, threads, 300, argc > 7 ? atoi(argv[7]) : 0, argc > 8 ? atoi(argv[8]) : 1,
                     argc > 9 ? atoi(argv[9]) : 0};
    void* e = fh264_open(&c);
    if (!e) {
        printf("bench: FAILED fh264_open\n");
        return 1;
    }
    std::vector<uint8_t> bg((size_t)w * h), y((size_t)w * h), u((size_t)w * h / 4), v((size_t)w * h / 4);
    // Background: light grey with dark "glyphs" in rows (like text).
    for (int r = 0; r < h; r++) {
        for (int x = 0; x < w; x++) {
            uint8_t p = 220;
            if ((r % 24) > 6 && (r % 24) < 18 && (x % 9) < 6 && ((next() & 7) < 3)) p = 40;
            bg[(size_t)r * w + x] = p;
        }
    }
    std::vector<uint8_t> out(8 << 20);
    FILE* f = path ? fopen(path, "wb") : nullptr;
    long long bytes = 0;
    int keys = 0;
    auto t0 = std::chrono::steady_clock::now();
    for (int i = 0; i < frames; i++) {
        memcpy(y.data(), bg.data(), y.size());
        memset(u.data(), 128, u.size());
        memset(v.data(), 128, v.size());
        // A window 800x500 moving right and down, with a gradient inside.
        int wx = (i * 12) % (w - 800), wy = (i * 5) % (h - 500);
        for (int r = 0; r < 500; r++)
            for (int x = 0; x < 800; x++) y[(size_t)(wy + r) * w + wx + x] = (uint8_t)(60 + (x + r + i) % 140);
        for (int r = 0; r < 250; r++)
            for (int x = 0; x < 400; x++) {
                u[(size_t)(wy / 2 + r) * (w / 2) + wx / 2 + x] = 90;
                v[(size_t)(wy / 2 + r) * (w / 2) + wx / 2 + x] = 160;
            }
        // A clock that changes every frame.
        for (int r = 0; r < 20; r++)
            for (int x = 0; x < 120; x++) y[(size_t)(h - 30 + r) * w + w - 140 + x] = (uint8_t)((x * 7 + i * 31 + r) & 0xff);
        int32_t key = 0;
        int32_t n = fh264_encode(e, y.data(), u.data(), v.data(), w, w / 2, (int64_t)i * 1000 / 30, out.data(), (int32_t)out.size(), &key);
        if (n < 0) {
            printf("bench: FAILED encode %d -> %d\n", i, n);
            return 1;
        }
        keys += key;
        bytes += n;
        if (f) fwrite(out.data(), 1, (size_t)n, f);
    }
    auto t1 = std::chrono::steady_clock::now();
    double ms = std::chrono::duration<double, std::milli>(t1 - t0).count();
    if (f) fclose(f);
    fh264_close(e);
    printf("bench: %dx%d %d frames, %d thread(s), screen=%d cabac=%d complexity=%d: %.2f ms/frame (%.1f fps), %lld bytes = %.2f Mbit/s at 30 fps, %d IDR\n",
           w, h, frames, threads, c.screen, c.cabac, c.complexity, ms / frames, frames * 1000.0 / ms, bytes,
           bytes * 8.0 * 30 / frames / 1e6, keys);
    return 0;
}
