//! MP4 (ISO BMFF) writer for one H.264 video track and an optional Opus
//! audio track (23.11).
//!
//! Layout: `ftyp`, `mdat` (64-bit size, patched at the end), `moov` at the
//! end. Every sample is its own chunk (`co64` lists them all): the recorder
//! writes video and audio as they come, interleaved by time, and the tables
//! stay trivial. Plays in browsers (Chrome, Firefox, Safari — files opened
//! locally or served with range requests), Windows Films & TV / Media
//! Player, VLC, mpv, ffmpeg, phones.
//!
//! "Faststart" (28.11, an option: [`Mp4::with_room`]): `moov` in front of the
//! data, so a browser can start a file served without range requests (or a
//! site that re-encodes only files with `moov` first) after the first
//! kilobytes. Moving `moov` in front at the end would copy the whole file —
//! minutes on a USB stick — so the room is kept from the start instead: a
//! `free` box right after `ftyp`, and at the end `moov` is written into it
//! and the rest stays `free` (players skip it). `mdat` never moves, so the
//! `co64` offsets stay right. A recording that outgrows the room gets `moov`
//! at the end as before ([`Mp4::moov_in_front`] says which).
//!
//! Times come from the capture: a video sample lasts until the next one
//! (unchanged screens and dropped frames make longer samples — variable frame
//! rate, which every player above handles), the last one until the end of
//! the recording. Video timescale 90 kHz, audio — 48 kHz (one Opus packet =
//! 960 samples = 20 ms). The Opus encoder's look-ahead (`pre_skip`) goes into
//! `dOps` and the audio edit list, as ffmpeg writes it.
//!
//! Samples are given in Annex B (start codes, as encoders write them);
//! SPS/PPS go into `avcC` once and are dropped from the samples, the rest is
//! written with 4-byte lengths.

use alloc::vec::Vec;

pub use crate::avi::Sink;

/// Video timescale.
pub const VIDEO_TIMESCALE: u32 = 90_000;
/// Audio timescale (Opus always decodes at 48 kHz).
pub const AUDIO_TIMESCALE: u32 = 48_000;
/// Movie timescale (`mvhd`, edit lists).
const MOVIE_TIMESCALE: u32 = 1000;

#[derive(Default)]
struct Track {
    sizes: Vec<u32>,
    offsets: Vec<u64>,
    /// Decode time of each sample, track timescale.
    times: Vec<u64>,
    /// Sync samples, numbered from 1 (video only).
    keys: Vec<u32>,
}

impl Track {
    fn push(&mut self, offset: u64, size: u32, time: u64, key: bool) {
        self.offsets.push(offset);
        self.sizes.push(size);
        self.times.push(time);
        if key {
            self.keys.push(self.sizes.len() as u32);
        }
    }
}

/// Opus track settings.
#[derive(Clone, Copy, Debug)]
pub struct OpusTrack {
    /// Samples (48 kHz) the decoder drops at the start.
    pub pre_skip: u16,
    pub channels: u8,
}

/// The MP4 being written.
pub struct Mp4<S: Sink> {
    sink: S,
    pos: u64,
    mdat: u64,
    width: u32,
    height: u32,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    video: Track,
    audio: Option<(OpusTrack, Track, u64)>,
    scratch: Vec<u8>,
    failed: bool,
    /// Faststart: where the reserved `free` box starts and its size (0 - no
    /// room kept); whether `finish` put `moov` there.
    room_at: u64,
    room: u64,
    moov_in_front: bool,
}

/// Bytes of `moov` per second of recording, a bound for [`room_for`]:
/// video `stsz` 4 + `co64` 8 + `stts` up to 8 and `stss`, audio (50 packets
/// a second) `stsz` 4 + `co64` 8.
fn moov_bytes_per_second(fps: u32, audio: bool) -> u64 {
    u64::from(fps.max(1)) * 24 + if audio { 50 * 12 } else { 0 }
}

/// Room to keep in front for `moov` of a recording of up to `seconds` at
/// `fps` (with sound or not): the bound above plus 16 KiB of headers and a
/// tenth more. Without a limit - room for 30 minutes (60 fps with sound:
/// 3.7 MiB; past it `moov` goes to the end as without the option).
#[must_use]
pub fn room_for(seconds: Option<u64>, fps: u32, audio: bool) -> u64 {
    let seconds = seconds.unwrap_or(30 * 60).max(1);
    let bytes = 16 * 1024 + seconds * moov_bytes_per_second(fps, audio);
    (bytes + bytes / 10).next_multiple_of(4096)
}

/// NAL units of an Annex B stream (without start codes).
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut at = 0usize;
    core::iter::from_fn(move || {
        // Find the next start code (00 00 01, the 4-byte form ends the same).
        let start = loop {
            if at + 3 > data.len() {
                return None;
            }
            if data[at] == 0 && data[at + 1] == 0 && data[at + 2] == 1 {
                break at + 3;
            }
            at += 1;
        };
        let mut end = start;
        while end + 3 <= data.len() && !(data[end] == 0 && data[end + 1] == 0 && (data[end + 2] == 1 || (data[end + 2] == 0 && end + 3 < data.len() && data[end + 3] == 1))) {
            end += 1;
        }
        let end = if end + 3 > data.len() { data.len() } else { end };
        at = end;
        Some(&data[start..end])
    })
    .filter(|nal| !nal.is_empty())
}

struct Buf(Vec<u8>);

impl Buf {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn zeros(&mut self, n: usize) {
        self.0.resize(self.0.len() + n, 0);
    }
    fn bytes(&mut self, b: &[u8]) {
        self.0.extend_from_slice(b);
    }
    /// Open a box; returns where it starts.
    fn open(&mut self, tag: &[u8; 4]) -> usize {
        let at = self.0.len();
        self.u32(0);
        self.bytes(tag);
        at
    }
    /// Open a full box (version 0).
    fn full(&mut self, tag: &[u8; 4], flags: u32) -> usize {
        let at = self.open(tag);
        self.u32(flags & 0x00FF_FFFF);
        at
    }
    fn close(&mut self, at: usize) {
        let size = (self.0.len() - at) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_be_bytes());
    }
    fn matrix(&mut self) {
        for v in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
            self.u32(v);
        }
    }
}

/// Run-length `stts` entries from decode times and the end time.
fn deltas(times: &[u64], end: u64) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &t) in times.iter().enumerate() {
        let next = times.get(i + 1).copied().unwrap_or(end.max(t + 1));
        let delta = (next.saturating_sub(t)).clamp(1, u64::from(u32::MAX)) as u32;
        match out.last_mut() {
            Some((count, d)) if *d == delta => *count += 1,
            _ => out.push((1, delta)),
        }
    }
    out
}

fn scale(value: u64, from: u32, to: u32) -> u64 {
    (u128::from(value) * u128::from(to) / u128::from(from.max(1))) as u64
}

impl<S: Sink> Mp4<S> {
    /// Start the file: `ftyp` and the `mdat` header. `audio` — an Opus track.
    pub fn new(sink: S, width: u32, height: u32, audio: Option<OpusTrack>) -> Option<Self> {
        Self::with_room(sink, width, height, audio, 0)
    }

    /// [`Self::new`] with `room` bytes kept after `ftyp` for `moov`
    /// (faststart; [`room_for`] sizes it). 0 - `moov` at the end.
    pub fn with_room(mut sink: S, width: u32, height: u32, audio: Option<OpusTrack>, room: u64) -> Option<Self> {
        let mut b = Buf(Vec::new());
        let ftyp = b.open(b"ftyp");
        b.bytes(b"isom");
        b.u32(0x200);
        for brand in [b"isom", b"iso2", b"avc1", b"mp41"] {
            b.bytes(brand);
        }
        b.close(ftyp);
        let room = if room == 0 { 0 } else { room.clamp(64, u64::from(u32::MAX)) };
        let room_at = b.0.len() as u64;
        if room > 0 {
            // A `free` box over the whole room (written in pieces: it may be
            // megabytes).
            b.u32(room as u32);
            b.bytes(b"free");
            if !sink.write(&b.0) {
                return None;
            }
            let zeros = alloc::vec![0u8; 64 * 1024];
            let mut left = room - 8;
            while left > 0 {
                let n = left.min(zeros.len() as u64) as usize;
                if !sink.write(&zeros[..n]) {
                    return None;
                }
                left -= n as u64;
            }
            b.0.clear();
        }
        // `b` holds `ftyp` when no room was kept, nothing after the room.
        let mdat = room_at + room;
        b.u32(1);
        b.bytes(b"mdat");
        b.u64(0);
        if !sink.write(&b.0) {
            return None;
        }
        Some(Self {
            sink,
            pos: mdat + 16,
            mdat,
            room_at,
            room,
            moov_in_front: false,
            width,
            height,
            sps: None,
            pps: None,
            video: Track::default(),
            audio: audio.map(|o| (o, Track::default(), 0)),
            scratch: Vec::new(),
            failed: false,
        })
    }

    /// Writing failed (disk full?); the file is not usable.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed
    }

    /// Video samples written.
    #[must_use]
    pub fn video_samples(&self) -> usize {
        self.video.sizes.len()
    }

    /// Audio packets written.
    #[must_use]
    pub fn audio_packets(&self) -> usize {
        self.audio.as_ref().map_or(0, |(_, t, _)| t.sizes.len())
    }

    /// Bytes so far.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.pos
    }

    fn take_parameter_sets(&mut self, annexb: &[u8]) {
        for nal in nal_units(annexb) {
            match nal[0] & 0x1F {
                7 if self.sps.is_none() => self.sps = Some(nal.to_vec()),
                8 if self.pps.is_none() => self.pps = Some(nal.to_vec()),
                _ => {}
            }
        }
    }

    /// SPS/PPS before the first frame (optional: an IDR frame brings them too).
    pub fn video_headers(&mut self, annexb: &[u8]) {
        self.take_parameter_sets(annexb);
    }

    /// One H.264 access unit (Annex B) presented at `time_us` from the start
    /// of the recording. The first one should be at 0 (MP4 decode times start
    /// there; the audio track starts there too).
    pub fn video(&mut self, annexb: &[u8], time_us: u64, key: bool) -> bool {
        if self.failed {
            return false;
        }
        self.take_parameter_sets(annexb);
        let mut sample = core::mem::take(&mut self.scratch);
        sample.clear();
        for nal in nal_units(annexb) {
            // Parameter sets live in `avcC`; access unit delimiters are noise.
            if matches!(nal[0] & 0x1F, 7 | 8 | 9) {
                continue;
            }
            sample.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            sample.extend_from_slice(nal);
        }
        let ok = if sample.is_empty() {
            true
        } else {
            let mut time = scale(time_us, 1_000_000, VIDEO_TIMESCALE);
            if let Some(&last) = self.video.times.last() {
                time = time.max(last + 1);
            }
            let offset = self.pos;
            let ok = self.sink.write(&sample);
            if ok {
                self.pos += sample.len() as u64;
                self.video.push(offset, sample.len() as u32, time, key);
            }
            ok
        };
        self.scratch = sample;
        self.failed |= !ok;
        ok
    }

    /// One Opus packet of `frames` samples (48 kHz).
    pub fn audio(&mut self, packet: &[u8], frames: u32) -> bool {
        if self.failed {
            return false;
        }
        let Some((_, track, end)) = self.audio.as_mut() else {
            return false;
        };
        let offset = self.pos;
        if !self.sink.write(packet) {
            self.failed = true;
            return false;
        }
        self.pos += packet.len() as u64;
        track.push(offset, packet.len() as u32, *end, false);
        *end += u64::from(frames);
        true
    }

    /// Close the file: `video_end_us` — when the recording ended (the last
    /// frame lasts until then). Answer: the sink and whether all went well.
    pub fn finish(self, video_end_us: u64) -> (S, bool) {
        let (sink, ok, _) = self.finish_report(video_end_us);
        (sink, ok)
    }

    /// [`Self::finish`] that also tells where `moov` went (faststart):
    /// `(sink, ok, Some((moov in front, moov bytes, room kept)))` - `None`
    /// when no room was kept.
    pub fn finish_report(mut self, video_end_us: u64) -> (S, bool, Option<(bool, u64, u64)>) {
        if self.failed || self.video.sizes.is_empty() || self.sps.is_none() || self.pps.is_none() {
            return (self.sink, false, None);
        }
        let size = self.pos - self.mdat;
        let mut ok = self.sink.write_at(self.mdat + 8, &size.to_be_bytes());
        let video_end = scale(video_end_us, 1_000_000, VIDEO_TIMESCALE).max(self.video.times.last().copied().unwrap_or(0) + 1);
        let moov = self.moov(video_end);
        let len = moov.len() as u64;
        // Faststart: `moov` into the room, the rest stays a `free` box (an
        // exact fit needs no `free` after it; less than its 8-byte header
        // left - does not fit).
        if self.room > 0 && (len == self.room || len + 8 <= self.room) {
            ok &= self.sink.write_at(self.room_at, &moov);
            if len < self.room {
                let mut free = Vec::with_capacity(8);
                free.extend_from_slice(&((self.room - len) as u32).to_be_bytes());
                free.extend_from_slice(b"free");
                ok &= self.sink.write_at(self.room_at + len, &free);
            }
            self.moov_in_front = ok;
        } else {
            ok &= self.sink.write(&moov);
        }
        let report = (self.room > 0).then_some((self.moov_in_front, len, self.room));
        (self.sink, ok, report)
    }

    fn moov(&self, video_end: u64) -> Vec<u8> {
        let mut b = Buf(Vec::new());
        let video_ms = scale(video_end, VIDEO_TIMESCALE, MOVIE_TIMESCALE);
        let audio_ms = self.audio.as_ref().map_or(0, |(o, _, end)| scale(end.saturating_sub(u64::from(o.pre_skip)), AUDIO_TIMESCALE, MOVIE_TIMESCALE));
        let moov = b.open(b"moov");
        let mvhd = b.full(b"mvhd", 0);
        b.u32(0);
        b.u32(0);
        b.u32(MOVIE_TIMESCALE);
        b.u32(video_ms.max(audio_ms) as u32);
        b.u32(0x0001_0000);
        b.u16(0x0100);
        b.zeros(10);
        b.matrix();
        b.zeros(24);
        b.u32(if self.audio.is_some() { 3 } else { 2 });
        b.close(mvhd);
        self.video_trak(&mut b, video_end, video_ms);
        if let Some((opus, track, end)) = &self.audio {
            if !track.sizes.is_empty() {
                self.audio_trak(&mut b, *opus, track, *end, audio_ms);
            }
        }
        b.close(moov);
        b.0
    }

    fn tkhd(b: &mut Buf, id: u32, duration_ms: u64, audio: bool, width: u32, height: u32) {
        let tkhd = b.full(b"tkhd", 3);
        b.u32(0);
        b.u32(0);
        b.u32(id);
        b.u32(0);
        b.u32(duration_ms as u32);
        b.zeros(8);
        b.u16(0); // layer
        b.u16(u16::from(audio)); // alternate group
        b.u16(if audio { 0x0100 } else { 0 });
        b.u16(0);
        b.matrix();
        b.u32(width << 16);
        b.u32(height << 16);
        b.close(tkhd);
    }

    fn mdhd_hdlr(b: &mut Buf, timescale: u32, duration: u64, handler: &[u8; 4], name: &[u8]) {
        let mdhd = b.full(b"mdhd", 0);
        b.u32(0);
        b.u32(0);
        b.u32(timescale);
        b.u32(duration.min(u64::from(u32::MAX)) as u32);
        b.u16(0x55C4); // "und"
        b.u16(0);
        b.close(mdhd);
        let hdlr = b.full(b"hdlr", 0);
        b.u32(0);
        b.bytes(handler);
        b.zeros(12);
        b.bytes(name);
        b.u8(0);
        b.close(hdlr);
    }

    fn dinf(b: &mut Buf) {
        let dinf = b.open(b"dinf");
        let dref = b.full(b"dref", 0);
        b.u32(1);
        let url = b.full(b"url ", 1);
        b.close(url);
        b.close(dref);
        b.close(dinf);
    }

    /// `stts`, `stsc`, `stsz`, `co64` (+ `stss` when `keys`).
    fn tables(b: &mut Buf, track: &Track, end: u64, keys: bool) {
        let stts = b.full(b"stts", 0);
        let runs = deltas(&track.times, end);
        b.u32(runs.len() as u32);
        for (count, delta) in runs {
            b.u32(count);
            b.u32(delta);
        }
        b.close(stts);
        if keys {
            let stss = b.full(b"stss", 0);
            b.u32(track.keys.len() as u32);
            for &k in &track.keys {
                b.u32(k);
            }
            b.close(stss);
        }
        let stsc = b.full(b"stsc", 0);
        b.u32(1);
        b.u32(1);
        b.u32(1);
        b.u32(1);
        b.close(stsc);
        let stsz = b.full(b"stsz", 0);
        b.u32(0);
        b.u32(track.sizes.len() as u32);
        for &s in &track.sizes {
            b.u32(s);
        }
        b.close(stsz);
        let co64 = b.full(b"co64", 0);
        b.u32(track.offsets.len() as u32);
        for &o in &track.offsets {
            b.u64(o);
        }
        b.close(co64);
    }

    fn video_trak(&self, b: &mut Buf, end: u64, duration_ms: u64) {
        let (Some(sps), Some(pps)) = (&self.sps, &self.pps) else { return };
        let trak = b.open(b"trak");
        Self::tkhd(b, 1, duration_ms, false, self.width, self.height);
        let mdia = b.open(b"mdia");
        let first = self.video.times.first().copied().unwrap_or(0);
        Self::mdhd_hdlr(b, VIDEO_TIMESCALE, end.saturating_sub(first), b"vide", b"FossAi video");
        let minf = b.open(b"minf");
        let vmhd = b.full(b"vmhd", 1);
        b.zeros(8);
        b.close(vmhd);
        Self::dinf(b);
        let stbl = b.open(b"stbl");
        let stsd = b.full(b"stsd", 0);
        b.u32(1);
        let avc1 = b.open(b"avc1");
        b.zeros(6);
        b.u16(1); // data reference
        b.zeros(16);
        b.u16(self.width as u16);
        b.u16(self.height as u16);
        b.u32(0x0048_0000);
        b.u32(0x0048_0000);
        b.u32(0);
        b.u16(1); // frames per sample
        let mut name = [0u8; 32];
        let label = b"FossAi H.264";
        name[0] = label.len() as u8;
        name[1..=label.len()].copy_from_slice(label);
        b.bytes(&name);
        b.u16(0x0018);
        b.u16(0xFFFF);
        let avcc = b.open(b"avcC");
        b.u8(1);
        b.u8(sps.get(1).copied().unwrap_or(77));
        b.u8(sps.get(2).copied().unwrap_or(0));
        b.u8(sps.get(3).copied().unwrap_or(40));
        b.u8(0xFF); // 4-byte lengths
        b.u8(0xE1); // one SPS
        b.u16(sps.len() as u16);
        b.bytes(sps);
        b.u8(1);
        b.u16(pps.len() as u16);
        b.bytes(pps);
        b.close(avcc);
        // BT.709, limited range — the same as the VUI of the stream.
        let colr = b.open(b"colr");
        b.bytes(b"nclx");
        b.u16(1);
        b.u16(1);
        b.u16(1);
        b.u8(0);
        b.close(colr);
        let pasp = b.open(b"pasp");
        b.u32(1);
        b.u32(1);
        b.close(pasp);
        b.close(avc1);
        b.close(stsd);
        Self::tables(b, &self.video, end, true);
        b.close(stbl);
        b.close(minf);
        b.close(mdia);
        b.close(trak);
    }

    fn audio_trak(&self, b: &mut Buf, opus: OpusTrack, track: &Track, end: u64, duration_ms: u64) {
        let trak = b.open(b"trak");
        Self::tkhd(b, 2, duration_ms, true, 0, 0);
        // Edit list: play from `pre_skip` on (the encoder's look-ahead).
        let edts = b.open(b"edts");
        let elst = b.full(b"elst", 0);
        b.u32(1);
        b.u32(duration_ms as u32);
        b.u32(u32::from(opus.pre_skip));
        b.u16(1);
        b.u16(0);
        b.close(elst);
        b.close(edts);
        let mdia = b.open(b"mdia");
        Self::mdhd_hdlr(b, AUDIO_TIMESCALE, end, b"soun", b"FossAi sound");
        let minf = b.open(b"minf");
        let smhd = b.full(b"smhd", 0);
        b.u32(0);
        b.close(smhd);
        Self::dinf(b);
        let stbl = b.open(b"stbl");
        let stsd = b.full(b"stsd", 0);
        b.u32(1);
        let entry = b.open(b"Opus");
        b.zeros(6);
        b.u16(1);
        b.zeros(8);
        b.u16(u16::from(opus.channels));
        b.u16(16);
        b.u32(0);
        b.u32(AUDIO_TIMESCALE << 16);
        let dops = b.open(b"dOps");
        b.u8(0);
        b.u8(opus.channels);
        b.u16(opus.pre_skip);
        b.u32(AUDIO_TIMESCALE);
        b.u16(0);
        b.u8(0);
        b.close(dops);
        b.close(entry);
        b.close(stsd);
        Self::tables(b, track, end, false);
        b.close(stbl);
        b.close(minf);
        b.close(mdia);
        b.close(trak);
    }
}

/// What [`probe`] found.
#[derive(Debug, Default, Clone)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub video_samples: u32,
    pub video_keys: u32,
    pub video_ms: u64,
    pub audio_packets: u32,
    pub audio_ms: u64,
    /// Bytes of all sound packets (silence encodes to a few bytes a packet).
    pub audio_bytes: u64,
    pub profile: u8,
    pub level: u8,
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Boxes inside `data`: (tag, body).
fn boxes(data: &[u8]) -> impl Iterator<Item = ([u8; 4], &[u8])> {
    let mut at = 0usize;
    core::iter::from_fn(move || {
        if at + 8 > data.len() {
            return None;
        }
        let size = be32(data, at) as usize;
        let tag = [data[at + 4], data[at + 5], data[at + 6], data[at + 7]];
        if size < 8 || at + size > data.len() {
            return None;
        }
        let body = &data[at + 8..at + size];
        at += size;
        Some((tag, body))
    })
}

fn child<'a>(data: &'a [u8], tag: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(data).find(|(t, _)| t == tag).map(|(_, b)| b)
}

/// Check a whole `moov` (and, through `read_at`, the samples it points at):
/// the tables agree, every sample lies inside `mdat`, video samples are
/// length-prefixed NAL units with an IDR first, audio packets have a valid
/// Opus TOC. `read_at(offset, buf)` reads the file.
pub fn probe(moov: &[u8], mdat: (u64, u64), mut read_at: impl FnMut(u64, &mut [u8]) -> bool) -> Result<Probe, &'static str> {
    let mut out = Probe::default();
    let mut found_video = false;
    for (tag, trak) in boxes(moov) {
        if &tag != b"trak" {
            continue;
        }
        let mdia = child(trak, b"mdia").ok_or("trak without mdia")?;
        let hdlr = child(mdia, b"hdlr").ok_or("no hdlr")?;
        let mdhd = child(mdia, b"mdhd").ok_or("no mdhd")?;
        let (timescale, duration) = (be32(mdhd, 12), be32(mdhd, 16));
        let stbl = child(child(mdia, b"minf").ok_or("no minf")?, b"stbl").ok_or("no stbl")?;
        let stsz = child(stbl, b"stsz").ok_or("no stsz")?;
        let co64 = child(stbl, b"co64").ok_or("no co64")?;
        let stts = child(stbl, b"stts").ok_or("no stts")?;
        let count = be32(stsz, 8);
        if be32(co64, 4) != count {
            return Err("co64 and stsz disagree");
        }
        let mut timed = 0u64;
        let mut ticks = 0u64;
        for i in 0..be32(stts, 4) as usize {
            let (n, d) = (be32(stts, 8 + i * 8), be32(stts, 12 + i * 8));
            timed += u64::from(n);
            ticks += u64::from(n) * u64::from(d);
        }
        if timed != u64::from(count) || ticks != u64::from(duration) {
            return Err("stts does not cover the samples or the duration");
        }
        let video = &hdlr[8..12] == b"vide";
        let mut head = [0u8; 8];
        for i in 0..count as usize {
            let size = be32(stsz, 12 + i * 4) as u64;
            let offset = u64::from_be_bytes(co64[8 + i * 8..16 + i * 8].try_into().map_err(|_| "short co64")?);
            if offset < mdat.0 || offset + size > mdat.1 || size == 0 {
                return Err("a sample lies outside mdat");
            }
            if video && (i == 0 || i % 30 == 0) {
                // Walk the NAL lengths of the sample.
                let mut at = 0u64;
                let mut first_type = 0u8;
                while at < size {
                    if !read_at(offset + at, &mut head[..5]) {
                        return Err("cannot read a sample");
                    }
                    let len = u64::from(be32(&head, 0));
                    if len == 0 || at + 4 + len > size {
                        return Err("bad NAL length in a video sample");
                    }
                    if at == 0 {
                        first_type = head[4] & 0x1F;
                    }
                    at += 4 + len;
                }
                if i == 0 && first_type != 5 {
                    return Err("the first video sample is not an IDR picture");
                }
            }
            if !video && (i == 0 || i % 50 == 0) {
                if !read_at(offset, &mut head[..1]) {
                    return Err("cannot read a packet");
                }
                // TOC: configuration 0..31, stereo flag, frame count code.
                // 20 ms CELT/hybrid/SILK configurations are 1, 5, 9, 13, 15,
                // 19, 23, 27, 31 mod 4 == 3 or ... any config is legal; check
                // the frame size is 20 ms.
                let config = head[0] >> 3;
                let ms20 = matches!(config, 1 | 5 | 9 | 13 | 15 | 19 | 23 | 27 | 31) || (config >= 16 && config % 4 == 3);
                if !ms20 {
                    return Err("an Opus packet is not a 20 ms frame");
                }
            }
        }
        if video {
            found_video = true;
            let stsd = child(stbl, b"stsd").ok_or("no stsd")?;
            let avc1 = child(&stsd[8..], b"avc1").ok_or("no avc1")?;
            out.width = u32::from(u16::from_be_bytes([avc1[24], avc1[25]]));
            out.height = u32::from(u16::from_be_bytes([avc1[26], avc1[27]]));
            let avcc = child(&avc1[78..], b"avcC").ok_or("no avcC")?;
            out.profile = avcc[1];
            out.level = avcc[3];
            out.video_samples = count;
            out.video_keys = child(stbl, b"stss").map_or(0, |s| be32(s, 4));
            out.video_ms = u64::from(duration) * 1000 / u64::from(timescale.max(1));
        } else {
            out.audio_packets = count;
            out.audio_bytes = (0..count as usize).map(|i| u64::from(be32(stsz, 12 + i * 4))).sum();
            out.audio_ms = u64::from(duration) * 1000 / u64::from(timescale.max(1));
        }
    }
    if !found_video {
        return Err("no video track");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn splits_annex_b() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0];
        let nals: Vec<&[u8]> = nal_units(&data).collect();
        assert_eq!(nals, vec![&[0x67u8, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9, 0][..]]);
    }

    #[test]
    fn run_lengths() {
        assert_eq!(deltas(&[0, 3000, 6000, 12000], 15000), vec![(2, 3000), (1, 6000), (1, 3000)]);
    }

    struct VecSink(Vec<u8>);

    impl Sink for VecSink {
        fn write(&mut self, data: &[u8]) -> bool {
            self.0.extend_from_slice(data);
            true
        }
        fn write_at(&mut self, offset: u64, data: &[u8]) -> bool {
            let at = offset as usize;
            if at + data.len() > self.0.len() {
                return false;
            }
            self.0[at..at + data.len()].copy_from_slice(data);
            true
        }
    }

    /// A tiny recording of `frames` pictures with `room` kept for `moov`.
    fn record(room: u64, frames: u32) -> (Vec<u8>, Option<(bool, u64, u64)>) {
        let mut mp4 = Mp4::with_room(VecSink(Vec::new()), 64, 48, None, room).unwrap();
        let idr = [0, 0, 0, 1, 0x67, 0x4d, 0, 0x1f, 0, 0, 0, 1, 0x68, 0xee, 0x3c, 0x80, 0, 0, 0, 1, 0x65, 0x88, 0x84, 0x00];
        let p = [0, 0, 0, 1, 0x41, 0x9a, 0x00];
        for i in 0..frames {
            assert!(mp4.video(if i == 0 { &idr } else { &p }, u64::from(i) * 33_333, i == 0));
        }
        let (sink, ok, report) = mp4.finish_report(u64::from(frames) * 33_333);
        assert!(ok);
        (sink.0, report)
    }

    /// Top-level boxes: (tag, start, size), 64-bit sizes too.
    fn top(file: &[u8]) -> Vec<([u8; 4], usize, usize)> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 8 <= file.len() {
            let mut size = be32(file, at) as usize;
            if size == 1 {
                size = u64::from_be_bytes(file[at + 8..at + 16].try_into().unwrap()) as usize;
            }
            out.push(([file[at + 4], file[at + 5], file[at + 6], file[at + 7]], at, size));
            assert!(size >= 8 && at + size <= file.len(), "box runs past the end");
            at += size;
        }
        assert_eq!(at, file.len());
        out
    }

    fn probe_file(file: &[u8]) -> Probe {
        let boxes = top(file);
        let (_, moov_at, moov_size) = *boxes.iter().find(|b| &b.0 == b"moov").unwrap();
        let (_, mdat_at, mdat_size) = *boxes.iter().find(|b| &b.0 == b"mdat").unwrap();
        let read = |offset: u64, buf: &mut [u8]| {
            let at = offset as usize;
            buf.copy_from_slice(&file[at..at + buf.len()]);
            true
        };
        probe(&file[moov_at + 8..moov_at + moov_size], ((mdat_at + 16) as u64, (mdat_at + mdat_size) as u64), read).unwrap()
    }

    #[test]
    fn faststart_puts_moov_in_the_room() {
        let (file, report) = record(64 * 1024, 30);
        let (front, len, room) = report.unwrap();
        assert!(front && len < room && room == 64 * 1024);
        let tags: Vec<[u8; 4]> = top(&file).iter().map(|b| b.0).collect();
        assert_eq!(tags, vec![*b"ftyp", *b"moov", *b"free", *b"mdat"]);
        assert_eq!(probe_file(&file).video_samples, 30);
    }

    #[test]
    fn faststart_room_too_small_keeps_moov_at_the_end() {
        let (file, report) = record(256, 30);
        assert_eq!(report.map(|r| r.0), Some(false));
        let tags: Vec<[u8; 4]> = top(&file).iter().map(|b| b.0).collect();
        assert_eq!(tags, vec![*b"ftyp", *b"free", *b"mdat", *b"moov"]);
        assert_eq!(probe_file(&file).video_samples, 30);
    }

    #[test]
    fn no_room_is_the_old_layout() {
        let (file, report) = record(0, 5);
        assert!(report.is_none());
        let tags: Vec<[u8; 4]> = top(&file).iter().map(|b| b.0).collect();
        assert_eq!(tags, vec![*b"ftyp", *b"mdat", *b"moov"]);
        assert_eq!(probe_file(&file).video_samples, 5);
    }

    #[test]
    fn room_covers_a_long_recording() {
        // An hour at 60 fps with sound: per second 60 * 24 + 600 bytes.
        let room = room_for(Some(3600), 60, true);
        assert!(room >= 3600 * (60 * 24 + 600));
        assert_eq!(room % 4096, 0);
        // No limit - half an hour.
        assert!(room_for(None, 30, true) < room);
    }
}
