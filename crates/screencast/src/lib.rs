//! Screen recording building blocks (22.11): a baseline JPEG encoder and an
//! AVI (MJPEG, OpenDML) writer; since 23.11 also the H.264 + Opus path —
//! colour conversion to I420 ([`yuv`]), the encoders through C ([`media`]:
//! openh264 and libopus, built by `xtask` into `libscreenrec-media.a`) and
//! an MP4 writer ([`mp4`]).
//!
//! MP4 with H.264 and Opus is what the recorder writes by default: it plays
//! everywhere, browsers and phones included. MJPEG in AVI stays as the
//! «Совместимость» format: every frame independent, no C code, plays in
//! Windows, VLC, mpv and ffmpeg.
//!
//! Hot paths are allocation-free: [`jpeg::Encoder::encode`] and
//! [`yuv::Converter::convert`] write into caller buffers with caller scratch,
//! so worker threads of a program whose heap is single-threaded never touch
//! the heap.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod avi;
pub mod jpeg;
pub mod media;
pub mod mp4;
pub mod yuv;

#[cfg(all(test, screencast_media))]
mod media_tests;
