// H.264 for the screen recorder (23.11): a plain C face of openh264's C++
// encoder, so the Rust side (`crates/screencast::media`) needs no vtables.
//
// One call per frame: I420 in, Annex B NAL units (start codes) out, all
// layers of the frame back to back. The settings are chosen for a desktop
// recording that has to play everywhere:
//
// * one spatial and one temporal layer, no SVC extensions (plain AVC);
// * Main profile with CABAC (≈10–15 % smaller than CAVLC at the same
//   quality; every hardware decoder since 2008 has it), no B-frames —
//   openh264 never makes them, so decode order equals presentation order
//   and the MP4 needs no `ctts`;
// * a fixed number of slices, one per encoder thread: openh264 encodes the
//   slices of a picture in parallel;
// * bitrate rate control without frame skipping (the recorder decides what
//   to drop; a skipped frame would leave a hole in the timeline);
// * BT.709 limited range signalled in the VUI, the matrix the Rust colour
//   conversion uses.
//
// BSD-2-Clause like openh264 itself.

#include <stdint.h>
#include <string.h>

#include "codec_api.h"

namespace {

struct Encoder {
    ISVCEncoder* enc;
    int width;
    int height;
};

}  // namespace

extern "C" {

// Settings of `fh264_open`. Same layout as `screencast::media::H264Config`.
struct FH264Config {
    int32_t width;
    int32_t height;
    int32_t fps;
    int32_t bitrate;       // bits per second
    int32_t max_bitrate;   // bits per second, 0 — 2× bitrate
    int32_t threads;       // encoder threads = slices, 1..=16
    int32_t keyint;        // frames between IDR pictures
    int32_t screen;        // 1 — openh264's screen-content mode
    int32_t cabac;         // 1 — Main profile with CABAC, 0 — Baseline CAVLC
    int32_t complexity;    // 0 low (fastest), 1 medium, 2 high
};

// Create an encoder; null — openh264 refused the settings.
void* fh264_open(const FH264Config* config) {
    ISVCEncoder* enc = nullptr;
    if (WelsCreateSVCEncoder(&enc) != 0 || enc == nullptr) {
        return nullptr;
    }
    int quiet = WELS_LOG_QUIET;
    enc->SetOption(ENCODER_OPTION_TRACE_LEVEL, &quiet);

    SEncParamExt p;
    memset(&p, 0, sizeof p);
    enc->GetDefaultParams(&p);
    int threads = config->threads < 1 ? 1 : (config->threads > 16 ? 16 : config->threads);
    int bitrate = config->bitrate > 0 ? config->bitrate : 8000000;
    p.iUsageType = config->screen ? SCREEN_CONTENT_REAL_TIME : CAMERA_VIDEO_REAL_TIME;
    p.iPicWidth = config->width;
    p.iPicHeight = config->height;
    p.iTargetBitrate = bitrate;
    p.iMaxBitrate = config->max_bitrate > 0 ? config->max_bitrate : UNSPECIFIED_BIT_RATE;
    p.iRCMode = RC_BITRATE_MODE;
    p.fMaxFrameRate = (float)(config->fps > 0 ? config->fps : 30);
    p.iTemporalLayerNum = 1;
    p.iSpatialLayerNum = 1;
    p.iComplexityMode = (ECOMPLEXITY_MODE)(config->complexity < 0 ? 0 : (config->complexity > 2 ? 2 : config->complexity));
    p.uiIntraPeriod = config->keyint > 0 ? (unsigned)config->keyint : 0;
    p.iNumRefFrame = 1;
    p.eSpsPpsIdStrategy = CONSTANT_ID;
    p.bPrefixNalAddingCtrl = false;
    p.bEnableSSEI = false;
    p.bSimulcastAVC = false;
    p.iPaddingFlag = 0;
    p.iEntropyCodingModeFlag = config->cabac ? 1 : 0;
    p.bEnableFrameSkip = false;
    p.iMultipleThreadIdc = (unsigned short)threads;
    p.bUseLoadBalancing = true;
    p.iLoopFilterDisableIdc = 0;
    p.bEnableDenoise = false;
    p.bEnableBackgroundDetection = true;
    p.bEnableAdaptiveQuant = true;
    p.bEnableFrameCroppingFlag = true;
    p.bEnableSceneChangeDetect = true;
    p.bEnableLongTermReference = false;
    p.iMaxQp = 51;
    p.iMinQp = 0;

    SSpatialLayerConfig* layer = &p.sSpatialLayers[0];
    layer->iVideoWidth = config->width;
    layer->iVideoHeight = config->height;
    layer->fFrameRate = p.fMaxFrameRate;
    layer->iSpatialBitrate = bitrate;
    layer->iMaxSpatialBitrate = p.iMaxBitrate;
    layer->uiProfileIdc = config->cabac ? PRO_MAIN : PRO_BASELINE;
    layer->uiLevelIdc = LEVEL_UNKNOWN;  // openh264 derives it from size and rate
    layer->sSliceArgument.uiSliceMode = threads > 1 ? SM_FIXEDSLCNUM_SLICE : SM_SINGLE_SLICE;
    layer->sSliceArgument.uiSliceNum = (unsigned)threads;
    layer->bVideoSignalTypePresent = true;
    layer->uiVideoFormat = VF_UNDEF;
    layer->bFullRange = false;
    layer->bColorDescriptionPresent = true;
    layer->uiColorPrimaries = CP_BT709;
    layer->uiTransferCharacteristics = TRC_BT709;
    layer->uiColorMatrix = CM_BT709;

    if (enc->InitializeExt(&p) != 0) {
        WelsDestroySVCEncoder(enc);
        return nullptr;
    }
    int format = videoFormatI420;
    enc->SetOption(ENCODER_OPTION_DATAFORMAT, &format);
    Encoder* e = new Encoder;
    e->enc = enc;
    e->width = config->width;
    e->height = config->height;
    return e;
}

// Encode one I420 picture. Writes Annex B NAL units into `out` and returns
// their bytes: 0 — the encoder made nothing (skipped), -1 — error, -2 —
// `out` too small (the frame is lost). `*key` — 1 for an IDR picture.
int32_t fh264_encode(void* handle, const uint8_t* y, const uint8_t* u, const uint8_t* v, int32_t stride_y, int32_t stride_uv,
                     int64_t time_ms, uint8_t* out, int32_t cap, int32_t* key) {
    Encoder* e = (Encoder*)handle;
    SSourcePicture pic;
    memset(&pic, 0, sizeof pic);
    pic.iColorFormat = videoFormatI420;
    pic.iPicWidth = e->width;
    pic.iPicHeight = e->height;
    pic.iStride[0] = stride_y;
    pic.iStride[1] = stride_uv;
    pic.iStride[2] = stride_uv;
    pic.pData[0] = (unsigned char*)y;
    pic.pData[1] = (unsigned char*)u;
    pic.pData[2] = (unsigned char*)v;
    pic.uiTimeStamp = time_ms;
    SFrameBSInfo info;
    memset(&info, 0, sizeof info);
    if (e->enc->EncodeFrame(&pic, &info) != cmResultSuccess) {
        return -1;
    }
    *key = info.eFrameType == videoFrameTypeIDR ? 1 : 0;
    if (info.eFrameType == videoFrameTypeSkip || info.eFrameType == videoFrameTypeInvalid) {
        return 0;
    }
    int32_t total = 0;
    for (int l = 0; l < info.iLayerNum; l++) {
        const SLayerBSInfo* layer = &info.sLayerInfo[l];
        int32_t bytes = 0;
        for (int n = 0; n < layer->iNalCount; n++) {
            bytes += layer->pNalLengthInByte[n];
        }
        if (total + bytes > cap) {
            return -2;
        }
        memcpy(out + total, layer->pBsBuf, (size_t)bytes);
        total += bytes;
    }
    return total;
}

// SPS and PPS alone (Annex B), for the MP4 header before the first frame.
int32_t fh264_headers(void* handle, uint8_t* out, int32_t cap) {
    Encoder* e = (Encoder*)handle;
    SFrameBSInfo info;
    memset(&info, 0, sizeof info);
    if (e->enc->EncodeParameterSets(&info) != cmResultSuccess) {
        return -1;
    }
    int32_t total = 0;
    for (int l = 0; l < info.iLayerNum; l++) {
        const SLayerBSInfo* layer = &info.sLayerInfo[l];
        for (int n = 0; n < layer->iNalCount; n++) {
            int32_t bytes = layer->pNalLengthInByte[n];
            if (total + bytes > cap) {
                return -2;
            }
            total += bytes;
        }
    }
    total = 0;
    for (int l = 0; l < info.iLayerNum; l++) {
        const SLayerBSInfo* layer = &info.sLayerInfo[l];
        int32_t bytes = 0;
        for (int n = 0; n < layer->iNalCount; n++) {
            bytes += layer->pNalLengthInByte[n];
        }
        memcpy(out + total, layer->pBsBuf, (size_t)bytes);
        total += bytes;
    }
    return total;
}

// The next picture is an IDR (the recorder asks for one every few seconds of
// wall time: unchanged frames are not encoded at all).
void fh264_force_key(void* handle) {
    Encoder* e = (Encoder*)handle;
    e->enc->ForceIntraFrame(true);
}

// New target bitrate, bits per second.
void fh264_set_bitrate(void* handle, int32_t bitrate) {
    Encoder* e = (Encoder*)handle;
    SBitrateInfo info;
    info.iLayer = SPATIAL_LAYER_ALL;
    info.iBitrate = bitrate;
    e->enc->SetOption(ENCODER_OPTION_BITRATE, &info);
}

void fh264_close(void* handle) {
    Encoder* e = (Encoder*)handle;
    if (e == nullptr) {
        return;
    }
    e->enc->Uninitialize();
    WelsDestroySVCEncoder(e->enc);
    delete e;
}

}  // extern "C"
