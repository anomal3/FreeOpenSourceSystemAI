//! H.264 (openh264) and Opus (libopus) encoders through their C faces in
//! `ports/screenrec` (23.11).
//!
//! The library (`libscreenrec-media.a`) is not linked by this crate: the
//! program that uses these wrappers links it (`user-progs/build.rs` for
//! `/bin/screenrec`, `build.rs` here for the host tests). Every function here
//! is `#[inline]`, so a program that never calls them carries no reference to
//! the C symbols and links without the library.
//!
//! The encoders allocate through the C runtime (`malloc`, threads) — in the
//! recorder that is its own `crt` module, not the Rust heap of the program.

use core::ffi::c_void;

/// Settings of [`H264::open`] (layout of `FH264Config` in `h264.cpp`).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct H264Config {
    pub width: i32,
    pub height: i32,
    pub fps: i32,
    /// Bits per second.
    pub bitrate: i32,
    /// Bits per second, 0 — unlimited.
    pub max_bitrate: i32,
    /// Encoder threads (= slices of a picture), 1..=16.
    pub threads: i32,
    /// Frames between IDR pictures.
    pub keyint: i32,
    /// 1 — openh264's screen-content mode (slower, single-threaded).
    pub screen: i32,
    /// 1 — Main profile with CABAC, 0 — Baseline with CAVLC.
    pub cabac: i32,
    /// 0 — fastest, 1 — medium, 2 — best.
    pub complexity: i32,
}

unsafe extern "C" {
    fn fh264_open(config: *const H264Config) -> *mut c_void;
    fn fh264_encode(handle: *mut c_void, y: *const u8, u: *const u8, v: *const u8, stride_y: i32, stride_uv: i32, time_ms: i64, out: *mut u8, cap: i32, key: *mut i32) -> i32;
    fn fh264_headers(handle: *mut c_void, out: *mut u8, cap: i32) -> i32;
    fn fh264_force_key(handle: *mut c_void);
    fn fh264_close(handle: *mut c_void);
    fn fopus_open(bitrate: i32, complexity: i32, lookahead: *mut i32) -> *mut c_void;
    fn fopus_encode(handle: *mut c_void, pcm: *const i16, frames: i32, out: *mut u8, cap: i32) -> i32;
    fn fopus_close(handle: *mut c_void);
}

/// An openh264 encoder.
pub struct H264 {
    handle: *mut c_void,
    width: u32,
    height: u32,
}

// SAFETY: the encoder is used by one thread at a time (moved, not shared).
unsafe impl Send for H264 {}

/// What [`H264::encode`] made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoded {
    /// Bytes of Annex B NAL units and whether it is an IDR picture.
    Frame(usize, bool),
    /// The encoder made nothing for this picture.
    Skipped,
}

impl H264 {
    /// `None` — openh264 refused the settings (odd size, no memory…).
    #[inline]
    #[must_use]
    pub fn open(config: &H264Config) -> Option<Self> {
        // SAFETY: the configuration is a live `repr(C)` structure.
        let handle = unsafe { fh264_open(config) };
        (!handle.is_null()).then_some(Self { handle, width: config.width as u32, height: config.height as u32 })
    }

    /// Encode an I420 picture (`frame` = Y, U, V planes back to back, as
    /// `yuv::Converter` writes them) into `out`. `Err` — the encoder failed or
    /// `out` was too small.
    #[inline]
    pub fn encode(&mut self, frame: &[u8], time_ms: i64, out: &mut [u8]) -> Result<Encoded, i32> {
        let luma = self.width as usize * self.height as usize;
        if frame.len() < luma + luma / 2 {
            return Err(-3);
        }
        let mut key = 0i32;
        // SAFETY: the planes lie inside `frame` (checked above), `out` is
        // writable for its length.
        let n = unsafe {
            fh264_encode(
                self.handle,
                frame.as_ptr(),
                frame.as_ptr().add(luma),
                frame.as_ptr().add(luma + luma / 4),
                self.width as i32,
                self.width as i32 / 2,
                time_ms,
                out.as_mut_ptr(),
                out.len().min(i32::MAX as usize) as i32,
                &mut key,
            )
        };
        match n {
            0 => Ok(Encoded::Skipped),
            n if n > 0 => Ok(Encoded::Frame(n as usize, key != 0)),
            n => Err(n),
        }
    }

    /// SPS and PPS (Annex B) into `out`; bytes written.
    #[inline]
    pub fn headers(&mut self, out: &mut [u8]) -> Option<usize> {
        // SAFETY: `out` is writable for its length.
        let n = unsafe { fh264_headers(self.handle, out.as_mut_ptr(), out.len().min(i32::MAX as usize) as i32) };
        (n > 0).then_some(n as usize)
    }

    /// Make the next picture an IDR.
    #[inline]
    pub fn force_key(&mut self) {
        // SAFETY: a live handle.
        unsafe { fh264_force_key(self.handle) }
    }
}

impl Drop for H264 {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: a live handle, dropped once.
        unsafe { fh264_close(self.handle) }
    }
}

/// Samples per channel in one Opus packet (20 ms at 48 kHz).
pub const OPUS_FRAME: usize = 960;
/// Bytes of the largest packet asked for.
pub const OPUS_MAX_PACKET: usize = 1500;

/// A libopus encoder: 48 kHz stereo.
pub struct Opus {
    handle: *mut c_void,
    /// Look-ahead of the encoder: samples the decoder drops at the start.
    pub pre_skip: u16,
}

// SAFETY: as `H264`.
unsafe impl Send for Opus {}

impl Opus {
    /// `bitrate` — bits per second; `complexity` 0..=10.
    #[inline]
    #[must_use]
    pub fn open(bitrate: i32, complexity: i32) -> Option<Self> {
        let mut lookahead = 0i32;
        // SAFETY: `lookahead` is a live integer.
        let handle = unsafe { fopus_open(bitrate, complexity, &mut lookahead) };
        (!handle.is_null()).then_some(Self { handle, pre_skip: lookahead.clamp(0, i32::from(u16::MAX)) as u16 })
    }

    /// Encode [`OPUS_FRAME`] interleaved stereo samples; packet bytes.
    #[inline]
    pub fn encode(&mut self, pcm: &[i16], out: &mut [u8]) -> Result<usize, i32> {
        if pcm.len() < OPUS_FRAME * 2 {
            return Err(-1);
        }
        // SAFETY: `pcm` holds a whole frame, `out` is writable for its length.
        let n = unsafe { fopus_encode(self.handle, pcm.as_ptr(), OPUS_FRAME as i32, out.as_mut_ptr(), out.len().min(i32::MAX as usize) as i32) };
        if n > 0 { Ok(n as usize) } else { Err(n) }
    }
}

impl Drop for Opus {
    #[inline]
    fn drop(&mut self) {
        // SAFETY: a live handle, dropped once.
        unsafe { fopus_close(self.handle) }
    }
}
