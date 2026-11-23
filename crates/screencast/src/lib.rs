//! Screen recording building blocks (22.11): a baseline JPEG encoder and an
//! AVI (MJPEG, OpenDML) writer.
//!
//! Why MJPEG in AVI first: every frame is independent, so frames are encoded
//! in parallel by worker threads with no shared state, the encoder is small
//! enough to own (no C++ port), and the file plays in Windows (Films & TV,
//! Media Player), VLC, mpv and ffmpeg. Browsers do not play AVI — H.264 in MP4
//! is the next milestone (see ROADMAP, «Запись экрана»).
//!
//! Both parts are allocation-free on the hot path: [`jpeg::Encoder::encode`]
//! writes into a caller buffer with a caller scratch, so worker threads of a
//! program whose heap is single-threaded never touch the heap.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod avi;
pub mod jpeg;
