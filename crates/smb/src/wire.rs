//! Little-endian reading and writing, UTF-16 strings.

use alloc::string::String;
use alloc::vec::Vec;

/// Append-only little-endian writer.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    #[must_use]
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }

    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    pub fn bytes(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }

    pub fn zeros(&mut self, n: usize) -> &mut Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Pad with zeros to a multiple of `n` from the start of the buffer.
    pub fn align(&mut self, n: usize) -> &mut Self {
        while self.buf.len() % n != 0 {
            self.buf.push(0);
        }
        self
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn put_u16(&mut self, at: usize, v: u16) {
        self.buf[at..at + 2].copy_from_slice(&v.to_le_bytes());
    }

    pub fn put_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Bounds-checked little-endian reads; `None` past the end.
#[must_use]
pub fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(buf.get(at..at + 2)?.try_into().ok()?))
}

#[must_use]
pub fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(buf.get(at..at + 4)?.try_into().ok()?))
}

#[must_use]
pub fn u64_at(buf: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(buf.get(at..at + 8)?.try_into().ok()?))
}

/// UTF-16LE bytes of `text`.
#[must_use]
pub fn utf16(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// Text of UTF-16LE bytes (an odd trailing byte is ignored, bad surrogates
/// become U+FFFD).
#[must_use]
pub fn from_utf16(bytes: &[u8]) -> String {
    let units = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
    char::decode_utf16(units).map(|ch| ch.unwrap_or('\u{FFFD}')).collect()
}

/// Windows FILETIME (100 ns since 1601) to Unix seconds; 0 stays 0.
#[must_use]
pub fn filetime_to_unix(filetime: u64) -> u64 {
    const EPOCH_DIFF: u64 = 11_644_473_600;
    if filetime == 0 {
        return 0;
    }
    (filetime / 10_000_000).saturating_sub(EPOCH_DIFF)
}

/// Unix seconds to FILETIME.
#[must_use]
pub fn unix_to_filetime(seconds: u64) -> u64 {
    (seconds + 11_644_473_600) * 10_000_000
}
