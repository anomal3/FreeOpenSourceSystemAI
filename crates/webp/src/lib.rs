//! WebP decoder for FossAi (`no_std` + `alloc`).
//!
//! This crate is a `no_std` port of the decoder half of **image-webp 0.2.4**
//! (<https://github.com/image-rs/image-webp>, Copyright the image-rs authors,
//! licensed MIT OR Apache-2.0; see `LICENSE-MIT` / `LICENSE-APACHE` next to
//! this crate's `Cargo.toml`).
//!
//! Changes against upstream:
//! - `#![no_std]` + `alloc`; `std::*` -> `core::*` / `alloc::*`.
//! - The encoder (`encoder.rs`) and the nightly benchmark modules were dropped.
//! - `std::io` replaced by a tiny private slice-backed shim (`io.rs`: `Read`,
//!   `BufRead`, `Seek`, `Cursor`, `Take`, `Error`); `byteorder-lite` replaced by
//!   a private `ReadBytesExt` in the same file; `quick-error` replaced by a
//!   hand-written `DecodingError` + `core::fmt::Display`.
//! - `HashMap` -> `BTreeMap` (chunk index).
//! - The upstream generic `WebPDecoder<R>` is kept internal; this crate exposes
//!   the small slice-based API below ([`decode`], [`dimensions`],
//!   [`decode_frames`]) with RGBA8 output and a pixel-count guard.
//!
//! The decoding logic itself is unchanged.

#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[allow(dead_code)]
mod alpha_blending;
#[allow(dead_code)]
mod decoder;
#[allow(dead_code)]
mod extended;
#[allow(dead_code)]
mod huffman;
mod io;
#[allow(dead_code)]
mod loop_filter;
#[allow(dead_code)]
mod lossless;
#[allow(dead_code)]
mod lossless_transform;
#[allow(dead_code)]
mod transform;
#[allow(dead_code)]
mod vp8;
#[allow(dead_code)]
mod vp8_arithmetic_decoder;
#[allow(dead_code)]
mod yuv;

mod prelude {
    pub(crate) use alloc::borrow::ToOwned;
    pub(crate) use alloc::boxed::Box;
    pub(crate) use alloc::string::String;
    pub(crate) use alloc::vec;
    pub(crate) use alloc::vec::Vec;
}

pub use decoder::DecodingError;
pub use io::{Error as IoError, ErrorKind as IoErrorKind};

use alloc::vec::Vec;
use decoder::WebPDecoder;
use io::Cursor;

/// Largest accepted canvas, in pixels (width * height).
pub const MAX_PIXELS: u64 = 16384 * 16384;

/// A decoded picture in RGBA8 (row-major, 4 bytes per pixel, no padding).
#[derive(Debug, Clone)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of RGBA8.
    pub rgba: Vec<u8>,
    /// Number of animation frames (1 for a still image). For animations
    /// `rgba` holds the first frame composed on the canvas.
    pub frames: u32,
}

fn open(data: &[u8]) -> Result<WebPDecoder<Cursor<&[u8]>>, DecodingError> {
    WebPDecoder::new(Cursor::new(data))
}

fn checked_pixels(w: u32, h: u32, max_pixels: u64) -> Result<usize, DecodingError> {
    let px = u64::from(w) * u64::from(h);
    if w == 0 || h == 0 || px > MAX_PIXELS.min(max_pixels) {
        return Err(DecodingError::ImageTooLarge);
    }
    usize::try_from(px).map_err(|_| DecodingError::ImageTooLarge)
}

fn zeroed(len: usize) -> Result<Vec<u8>, DecodingError> {
    let mut v = Vec::new();
    v.try_reserve_exact(len)
        .map_err(|_| DecodingError::MemoryLimitExceeded)?;
    v.resize(len, 0);
    Ok(v)
}

/// Expands tightly packed RGB8 to RGBA8 (alpha 255).
fn rgb_to_rgba(rgb: &[u8], pixels: usize) -> Result<Vec<u8>, DecodingError> {
    let mut out = Vec::new();
    out.try_reserve_exact(pixels * 4)
        .map_err(|_| DecodingError::MemoryLimitExceeded)?;
    for p in rgb.chunks_exact(3) {
        out.extend_from_slice(&[p[0], p[1], p[2], 255]);
    }
    Ok(out)
}

/// Reads only the headers and returns `(width, height)` of the canvas.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), DecodingError> {
    Ok(open(data)?.dimensions())
}

/// Decodes a lossy, lossless or extended (alpha / animated) WebP to RGBA8.
///
/// For animations only the first frame is decoded (composed on the canvas);
/// [`Image::frames`] reports the total frame count.
pub fn decode(data: &[u8]) -> Result<Image, DecodingError> {
    let mut dec = open(data)?;
    let (width, height) = dec.dimensions();
    let pixels = checked_pixels(width, height, MAX_PIXELS)?;
    let size = dec
        .output_buffer_size()
        .ok_or(DecodingError::ImageTooLarge)?;
    let mut buf = zeroed(size)?;
    dec.read_image(&mut buf)?;
    let rgba = if dec.has_alpha() {
        buf
    } else {
        rgb_to_rgba(&buf, pixels)?
    };
    let frames = if dec.is_animated() {
        dec.num_frames()
    } else {
        1
    };
    Ok(Image {
        width,
        height,
        rgba,
        frames,
    })
}

/// Decoded animation: `(width, height, frames)`, each frame being the full
/// RGBA8 canvas after composing it, plus its delay in milliseconds.
pub type Frames = (u32, u32, Vec<(Vec<u8>, u32)>);

/// Decodes every frame of an animated WebP (a still image yields one frame
/// with delay 0). `max_pixels` bounds the total number of decoded pixels,
/// `width * height * frame_count`; larger files fail with
/// [`DecodingError::MemoryLimitExceeded`] before anything is decoded.
pub fn decode_frames(data: &[u8], max_pixels: u64) -> Result<Frames, DecodingError> {
    let mut dec = open(data)?;
    let (width, height) = dec.dimensions();
    let pixels = checked_pixels(width, height, MAX_PIXELS)?;
    if !dec.is_animated() {
        if pixels as u64 > max_pixels {
            return Err(DecodingError::MemoryLimitExceeded);
        }
        let img = decode(data)?;
        let mut out = Vec::new();
        out.push((img.rgba, 0));
        return Ok((width, height, out));
    }
    let count = dec.num_frames();
    if (pixels as u64).saturating_mul(u64::from(count)) > max_pixels {
        return Err(DecodingError::MemoryLimitExceeded);
    }
    let size = dec
        .output_buffer_size()
        .ok_or(DecodingError::ImageTooLarge)?;
    let alpha = dec.has_alpha();
    let mut buf = zeroed(size)?;
    let mut out = Vec::new();
    out.try_reserve_exact(count as usize)
        .map_err(|_| DecodingError::MemoryLimitExceeded)?;
    loop {
        let delay = match dec.read_frame(&mut buf) {
            Ok(d) => d,
            Err(DecodingError::NoMoreFrames) => break,
            Err(e) => return Err(e),
        };
        let rgba = if alpha {
            let mut f = Vec::new();
            f.try_reserve_exact(buf.len())
                .map_err(|_| DecodingError::MemoryLimitExceeded)?;
            f.extend_from_slice(&buf);
            f
        } else {
            rgb_to_rgba(&buf, pixels)?
        };
        out.push((rgba, delay));
    }
    Ok((width, height, out))
}
