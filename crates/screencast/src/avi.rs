//! AVI writer for one MJPEG video stream with OpenDML (AVI 2.0) indexes.
//!
//! The file is a chain of RIFF segments of at most [`SEGMENT_LIMIT`] bytes:
//! `RIFF AVI ` (headers, `movi`, `ix00`, legacy `idx1`), then `RIFF AVIX`
//! (`movi`, `ix00`) as many as needed. The super index `indx` in the stream
//! header points at every `ix00`, so players seek in files of any size; the
//! legacy `idx1` covers the first segment for old players.
//!
//! Headers are written with zero counters at [`Avi::new`] and rewritten with
//! the totals at [`Avi::finish`] (same size, so the data does not move).

use alloc::vec::Vec;

/// Where the bytes go.
pub trait Sink {
    /// Append at the end.
    fn write(&mut self, data: &[u8]) -> bool;
    /// Overwrite at `offset` (inside what is already written), keeping the
    /// append position.
    fn write_at(&mut self, offset: u64, data: &[u8]) -> bool;
}

/// Largest RIFF segment (ffmpeg uses the same 1 GiB).
pub const SEGMENT_LIMIT: u64 = 1 << 30;

/// Room for super index entries: 256 segments of 1 GiB.
const SUPER_MAX: usize = 256;

const AVIF_HASINDEX: u32 = 0x10;
const AVIF_ISINTERLEAVED: u32 = 0x100;
const AVIIF_KEYFRAME: u32 = 0x10;

/// The AVI being written.
pub struct Avi<S: Sink> {
    sink: S,
    width: u32,
    height: u32,
    fps: u32,
    /// Append position.
    pos: u64,
    /// Start of the current RIFF chunk and of its `LIST movi`.
    riff: u64,
    movi: u64,
    first_segment: bool,
    frames_first: u32,
    frames_total: u32,
    /// Legacy index of the first segment: offset from the `movi` fourcc and
    /// size.
    idx1: Vec<(u32, u32)>,
    /// Standard index of the current segment: data offset from `movi` and size.
    ix: Vec<(u32, u32)>,
    /// Super index: offset and size of each `ix00`, frames in it.
    supers: Vec<(u64, u32, u32)>,
    max_frame: u32,
    failed: bool,
    segment_limit: u64,
}

fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

struct Buf(Vec<u8>);

impl Buf {
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn tag(&mut self, t: &[u8; 4]) {
        self.0.extend_from_slice(t);
    }
    /// Start a chunk; returns where its size goes.
    fn open(&mut self, t: &[u8; 4]) -> usize {
        self.tag(t);
        let at = self.0.len();
        self.u32(0);
        at
    }
    fn close(&mut self, at: usize) {
        let size = (self.0.len() - at - 4) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_le_bytes());
    }
}

impl<S: Sink> Avi<S> {
    /// Start the file: headers with zero counters and the first `movi`.
    pub fn new(sink: S, width: u32, height: u32, fps: u32) -> Option<Self> {
        let mut avi = Self {
            sink,
            width,
            height,
            fps: fps.max(1),
            pos: 0,
            riff: 0,
            movi: 0,
            first_segment: true,
            frames_first: 0,
            frames_total: 0,
            idx1: Vec::new(),
            ix: Vec::new(),
            supers: Vec::new(),
            max_frame: 0,
            failed: false,
            segment_limit: SEGMENT_LIMIT,
        };
        let header = avi.header();
        if !avi.sink.write(&header) {
            return None;
        }
        avi.pos = header.len() as u64;
        avi.movi = avi.pos;
        // `LIST <size> movi`.
        avi.put(b"LIST", 0);
        avi.raw(b"movi");
        (!avi.failed).then_some(avi)
    }

    /// Frames written so far.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames_total
    }

    /// Bytes written so far.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.pos
    }

    /// Smaller RIFF segments (tests of the OpenDML chain).
    #[doc(hidden)]
    pub fn set_segment_limit(&mut self, bytes: u64) {
        self.segment_limit = bytes.min(SEGMENT_LIMIT);
    }

    /// Did a write fail (disk full)?
    #[must_use]
    pub const fn failed(&self) -> bool {
        self.failed
    }

    fn raw(&mut self, data: &[u8]) {
        if self.failed || !self.sink.write(data) {
            self.failed = true;
            return;
        }
        self.pos += data.len() as u64;
    }

    fn put(&mut self, tag: &[u8; 4], size: u32) {
        let mut head = [0u8; 8];
        head[..4].copy_from_slice(tag);
        head[4..].copy_from_slice(&size.to_le_bytes());
        self.raw(&head);
    }

    /// Append one JPEG frame.
    pub fn frame(&mut self, jpeg: &[u8]) -> bool {
        let size = jpeg.len() as u32;
        let need = 8 + u64::from(size) + 1 + 24 + 8 * (self.ix.len() as u64 + 1) + if self.first_segment { 8 + 16 * (self.idx1.len() as u64 + 1) } else { 0 };
        if self.pos - self.riff + need > self.segment_limit && !self.ix.is_empty() {
            self.close_segment();
            self.open_segment();
        }
        let chunk = self.pos;
        self.put(b"00dc", size);
        self.raw(jpeg);
        if size % 2 == 1 {
            self.raw(&[0]);
        }
        if self.first_segment {
            self.idx1.push(((chunk - (self.movi + 8)) as u32, size));
            self.frames_first += 1;
        }
        self.ix.push(((chunk + 8 - self.movi) as u32, size));
        self.frames_total += 1;
        self.max_frame = self.max_frame.max(size);
        !self.failed
    }

    /// `ix00` at the end of the current `movi`, sizes of `movi` and RIFF
    /// (and `idx1` after the first `movi`).
    fn close_segment(&mut self) {
        let ix_at = self.pos;
        let mut ix = Buf(Vec::with_capacity(32 + self.ix.len() * 8));
        let size_at = ix.open(b"ix00");
        ix.u16(2);
        ix.0.push(0);
        ix.0.push(1);
        ix.u32(self.ix.len() as u32);
        ix.tag(b"00dc");
        ix.u64(self.movi);
        ix.u32(0);
        for (offset, size) in &self.ix {
            ix.u32(*offset);
            ix.u32(*size);
        }
        ix.close(size_at);
        let ix_size = ix.0.len() as u32;
        self.raw(&ix.0);
        if self.supers.len() < SUPER_MAX {
            self.supers.push((ix_at, ix_size, self.ix.len() as u32));
        }
        self.ix.clear();
        let movi_size = (self.pos - self.movi - 8) as u32;
        self.patch(self.movi + 4, &movi_size.to_le_bytes());
        if self.first_segment {
            let mut idx = Buf(Vec::with_capacity(8 + self.idx1.len() * 16));
            let at = idx.open(b"idx1");
            for (offset, size) in &self.idx1 {
                idx.tag(b"00dc");
                idx.u32(AVIIF_KEYFRAME);
                idx.u32(*offset);
                idx.u32(*size);
            }
            idx.close(at);
            self.raw(&idx.0);
            self.idx1 = Vec::new();
        }
        let riff_size = (self.pos - self.riff - 8) as u32;
        self.patch(self.riff + 4, &riff_size.to_le_bytes());
        self.first_segment = false;
    }

    fn open_segment(&mut self) {
        self.riff = self.pos;
        self.put(b"RIFF", 0);
        self.raw(b"AVIX");
        self.movi = self.pos;
        self.put(b"LIST", 0);
        self.raw(b"movi");
    }

    fn patch(&mut self, offset: u64, data: &[u8]) {
        if !self.failed && !self.sink.write_at(offset, data) {
            self.failed = true;
        }
    }

    /// Indexes, sizes and the headers with the totals. Gives the sink back.
    pub fn finish(mut self) -> (S, bool) {
        self.close_segment();
        let header = self.header();
        self.patch(0, &header);
        let ok = !self.failed;
        (self.sink, ok)
    }

    /// `RIFF AVI ` + `hdrl` (avih, strl: strh, strf, indx) + `odml`. Always
    /// the same length; the RIFF size is patched by [`Self::close_segment`].
    fn header(&self) -> Vec<u8> {
        let mut b = Buf(Vec::with_capacity(4600));
        let riff_size = if self.supers.is_empty() { 0 } else { self.riff_first_size() };
        b.tag(b"RIFF");
        b.u32(riff_size);
        b.tag(b"AVI ");
        let hdrl = b.open(b"LIST");
        b.tag(b"hdrl");
        let suggested = (self.max_frame + 8 + 1023) & !1023;
        // avih.
        let avih = b.open(b"avih");
        b.u32(1_000_000 / self.fps);
        b.u32(self.max_frame.saturating_mul(self.fps));
        b.u32(0);
        b.u32(AVIF_HASINDEX | AVIF_ISINTERLEAVED);
        b.u32(self.frames_first);
        b.u32(0);
        b.u32(1);
        b.u32(suggested);
        b.u32(self.width);
        b.u32(self.height);
        for _ in 0..4 {
            b.u32(0);
        }
        b.close(avih);
        // strl.
        let strl = b.open(b"LIST");
        b.tag(b"strl");
        let strh = b.open(b"strh");
        b.tag(b"vids");
        b.tag(b"MJPG");
        b.u32(0);
        b.u16(0);
        b.u16(0);
        b.u32(0);
        b.u32(1);
        b.u32(self.fps);
        b.u32(0);
        b.u32(self.frames_total);
        b.u32(suggested);
        b.u32(u32::MAX);
        b.u32(0);
        b.u16(0);
        b.u16(0);
        b.u16(self.width as u16);
        b.u16(self.height as u16);
        b.close(strh);
        let strf = b.open(b"strf");
        b.u32(40);
        b.u32(self.width);
        b.u32(self.height);
        b.u16(1);
        b.u16(24);
        b.u32(fourcc(b"MJPG"));
        b.u32(self.width.saturating_mul(self.height).saturating_mul(3));
        for _ in 0..4 {
            b.u32(0);
        }
        b.close(strf);
        let indx = b.open(b"indx");
        b.u16(4);
        b.0.push(0);
        b.0.push(0);
        b.u32(self.supers.len() as u32);
        b.tag(b"00dc");
        for _ in 0..3 {
            b.u32(0);
        }
        for index in 0..SUPER_MAX {
            let (offset, size, duration) = self.supers.get(index).copied().unwrap_or((0, 0, 0));
            b.u64(offset);
            b.u32(size);
            b.u32(duration);
        }
        b.close(indx);
        b.close(strl);
        // odml.
        let odml = b.open(b"LIST");
        b.tag(b"odml");
        let dmlh = b.open(b"dmlh");
        b.u32(self.frames_total);
        b.0.resize(b.0.len() + 244, 0);
        b.close(dmlh);
        b.close(odml);
        b.close(hdrl);
        b.0
    }

    /// Size field of the first RIFF: up to the second RIFF or the end.
    fn riff_first_size(&self) -> u32 {
        let end = if self.supers.len() > 1 { self.first_riff_end() } else { self.pos };
        (end - 8) as u32
    }

    /// Where the second RIFF starts: right after the first `idx1`. The first
    /// `ix00` is followed by `idx1` (16 bytes per frame of the first segment).
    fn first_riff_end(&self) -> u64 {
        let (ix_at, ix_size, _) = self.supers[0];
        ix_at + u64::from(ix_size) + 8 + 16 * u64::from(self.frames_first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jpeg::{Encoder, Scratch, Source, max_size};
    use std::io::{Seek, SeekFrom, Write};

    struct FileSink(std::fs::File);

    impl Sink for FileSink {
        fn write(&mut self, data: &[u8]) -> bool {
            self.0.write_all(data).is_ok()
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> bool {
            let Ok(end) = self.0.stream_position() else {
                return false;
            };
            self.0.seek(SeekFrom::Start(offset)).is_ok() && self.0.write_all(data).is_ok() && self.0.seek(SeekFrom::Start(end)).is_ok()
        }
    }

    /// A moving square over a gradient, `frames` frames, scaled from 1280×720
    /// to `out`. The path is printed so the file can be checked with ffprobe:
    /// `SCREENCAST_OUT=dir cargo test -p screencast -- --nocapture`.
    fn record(name: &str, out: (u32, u32), frames: u32, segment: u64) -> std::path::PathBuf {
        let (w, h) = (1280u32, 720u32);
        let dir = std::env::var("SCREENCAST_OUT").map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from);
        let path = dir.join(name);
        let file = std::fs::File::create(&path).expect("create");
        let mut avi = Avi::new(FileSink(file), out.0, out.1, 30).expect("avi");
        avi.set_segment_limit(segment);
        let encoder = Encoder::new(80);
        let mut scratch = Scratch::new(out.0);
        let mut jpeg = std::vec![0u8; max_size(out.0, out.1)];
        let mut pixels = std::vec![0u32; (w * h) as usize];
        for frame in 0..frames {
            for y in 0..h {
                for x in 0..w {
                    let inside = (x as i32 - (100 + frame as i32 * 20)).abs() < 80 && (y as i32 - 360).abs() < 80;
                    let (r, g, b) = if inside { (255, 40, 40) } else { (x * 255 / w, y * 255 / h, 128) };
                    pixels[(y * w + x) as usize] = 0xFF00_0000 | r | (g << 8) | (b << 16);
                }
            }
            let src = Source { pixels: &pixels, width: w, height: h, stride: w, bgr: false };
            let n = encoder.encode(&src, out.0, out.1, &mut scratch, &mut jpeg).expect("encode");
            assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
            assert_eq!(&jpeg[n - 2..n], &[0xFF, 0xD9]);
            assert!(avi.frame(&jpeg[..n]));
        }
        let (_, ok) = avi.finish();
        assert!(ok);
        std::println!("screencast test file: {}", path.display());
        path
    }

    #[test]
    fn writes_a_playable_avi() {
        let path = record("screencast-test.avi", (1280, 720), 30, SEGMENT_LIMIT);
        let bytes = std::fs::read(&path).expect("read");
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"AVI ");
        let riff = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff + 8, bytes.len());
    }

    #[test]
    fn scales_down() {
        let path = record("screencast-test-scaled.avi", (854, 480), 10, SEGMENT_LIMIT);
        assert!(std::fs::metadata(&path).expect("file").len() > 10_000);
    }

    /// RIFF AVIX segments (OpenDML) with 200 KB segments: 30 frames of ~34 KB
    /// make five of them.
    #[test]
    fn chains_segments() {
        let path = record("screencast-test-odml.avi", (1280, 720), 30, 200_000);
        let bytes = std::fs::read(&path).expect("read");
        let avix = bytes.windows(4).filter(|w| *w == b"AVIX").count();
        assert!(avix >= 3, "{avix} AVIX segments");
    }
}
