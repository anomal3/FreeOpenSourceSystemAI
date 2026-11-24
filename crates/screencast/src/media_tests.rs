//! Host tests of the whole MP4 path with the real encoders (only when the
//! host library is built — cfg `screencast_media`, see `build.rs`).
//!
//! `SCREENCAST_OUT=dir cargo test -p screencast -- --nocapture` keeps the
//! files; `ffprobe` (if on PATH) is asked about them.

use alloc::vec;
use alloc::vec::Vec;
use std::io::{Seek, SeekFrom, Write};

use crate::jpeg::Source;
use crate::media::{Encoded, H264, H264Config, OPUS_FRAME, OPUS_MAX_PACKET, Opus};
use crate::mp4::{Mp4, OpusTrack, Sink, probe};
use crate::yuv::Converter;

struct FileSink(std::fs::File);

impl Sink for FileSink {
    fn write(&mut self, data: &[u8]) -> bool {
        self.0.write_all(data).is_ok()
    }
    fn write_at(&mut self, offset: u64, data: &[u8]) -> bool {
        let Ok(end) = self.0.stream_position() else { return false };
        self.0.seek(SeekFrom::Start(offset)).is_ok() && self.0.write_all(data).is_ok() && self.0.seek(SeekFrom::Start(end)).is_ok()
    }
}

/// A desktop-like picture: gradient, a moving red square, a static band.
fn picture(w: u32, h: u32, frame: u32, pixels: &mut [u32]) {
    for y in 0..h {
        for x in 0..w {
            let inside = (x as i32 - (100 + frame as i32 * 15) % w as i32).abs() < 60 && (y as i32 - h as i32 / 2).abs() < 60;
            let (r, g, b) = if inside { (230, 30, 30) } else if y < 40 { (32, 32, 48) } else { (x * 255 / w, y * 255 / h, 128) };
            pixels[(y * w + x) as usize] = 0xFF00_0000 | r | (g << 8) | (b << 16);
        }
    }
}

fn record(name: &str, src_size: (u32, u32), out: (u32, u32), seconds: u32, with_audio: bool) -> std::path::PathBuf {
    let dir = std::env::var("SCREENCAST_OUT").map_or_else(|_| std::env::temp_dir(), std::path::PathBuf::from);
    let path = dir.join(name);
    let (w, h) = src_size;
    let conv = Converter::new(w, h, out.0, out.1);
    let mut h264 = H264::open(&H264Config { width: out.0 as i32, height: out.1 as i32, fps: 30, bitrate: 4_000_000, max_bitrate: 0, threads: 4, keyint: 150, screen: 0, cabac: 1, complexity: 0 }).expect("openh264");
    let mut opus = if with_audio { Some(Opus::open(128_000, 8).expect("opus")) } else { None };
    let audio = opus.as_ref().map(|o| OpusTrack { pre_skip: o.pre_skip, channels: 2 });
    let file = std::fs::File::create(&path).expect("create");
    let mut mp4 = Mp4::new(FileSink(file), out.0, out.1, audio).expect("mp4");
    let mut headers = vec![0u8; 1024];
    let n = h264.headers(&mut headers).expect("SPS/PPS");
    mp4.video_headers(&headers[..n]);
    let mut pixels = vec![0u32; (w * h) as usize];
    let mut yuv = vec![0u8; conv.frame_bytes()];
    let mut scratch = vec![0u32; 2 * out.0 as usize];
    let mut bits = vec![0u8; 4 << 20];
    let mut phase = 0f32;
    let mut audio_frames = 0u64;
    let mut packet = vec![0u8; OPUS_MAX_PACKET];
    let mut pcm: Vec<i16> = vec![0; OPUS_FRAME * 2];
    for frame in 0..seconds * 30 {
        // Every third second the screen stands still: no samples, the last
        // one lasts longer (variable frame rate, as the recorder writes).
        let still = (frame / 30) % 3 == 2 && frame % 30 != 0;
        if !still {
            picture(w, h, frame, &mut pixels);
            let src = Source { pixels: &pixels, width: w, height: h, stride: w, bgr: false };
            conv.convert_all(&src, &mut yuv, &mut scratch);
            let time_us = u64::from(frame) * 1_000_000 / 30;
            match h264.encode(&yuv, (time_us / 1000) as i64, &mut bits).expect("encode") {
                Encoded::Frame(n, key) => assert!(mp4.video(&bits[..n], time_us, key)),
                Encoded::Skipped => {}
            }
        }
        // Audio up to the end of this frame: a 440 Hz tone, 20 ms packets.
        if let Some(opus) = opus.as_mut() {
            let until = (u64::from(frame) + 1) * 48_000 / 30;
            while audio_frames + OPUS_FRAME as u64 <= until {
                for i in 0..OPUS_FRAME {
                    let s = (libm_sin(phase) * 8000.0) as i16;
                    phase += 2.0 * core::f32::consts::PI * 440.0 / 48_000.0;
                    if phase > 2.0 * core::f32::consts::PI {
                        phase -= 2.0 * core::f32::consts::PI;
                    }
                    pcm[2 * i] = s;
                    pcm[2 * i + 1] = s;
                }
                let n = opus.encode(&pcm, &mut packet).expect("opus encode");
                assert!(mp4.audio(&packet[..n], OPUS_FRAME as u32));
                audio_frames += OPUS_FRAME as u64;
            }
        }
    }
    let (_, ok) = mp4.finish(u64::from(seconds) * 1_000_000);
    assert!(ok);
    std::println!("screencast test file: {}", path.display());
    path
}

fn libm_sin(x: f32) -> f32 {
    // std is there in tests.
    std::primitive::f32::sin(x)
}

/// Run our own probe over the file (as `screenrec --probe` does).
fn own_probe(path: &std::path::Path) -> crate::mp4::Probe {
    let bytes = std::fs::read(path).expect("read");
    let mut at = 0usize;
    let (mut moov, mut mdat) = (None, (0u64, 0u64));
    while at + 8 <= bytes.len() {
        let size32 = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
        let tag = &bytes[at + 4..at + 8];
        let (size, head) = if size32 == 1 { (u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap()), 16) } else { (size32, 8) };
        if tag == b"moov" {
            moov = Some(&bytes[at + 8..at + size as usize]);
        }
        if tag == b"mdat" {
            mdat = ((at + head) as u64, at as u64 + size);
        }
        at += size as usize;
    }
    probe(moov.expect("moov"), mdat, |offset, buf| {
        let o = offset as usize;
        if o + buf.len() > bytes.len() {
            return false;
        }
        buf.copy_from_slice(&bytes[o..o + buf.len()]);
        true
    })
    .expect("probe")
}

fn ffprobe(path: &std::path::Path) -> Option<std::string::String> {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=codec_name,profile,width,height,nb_read_frames,duration,color_space", "-count_frames", "-of", "compact"])
        .arg(path)
        .output()
        .ok()?;
    assert!(out.status.success(), "ffprobe failed: {}", std::string::String::from_utf8_lossy(&out.stderr));
    assert!(out.stderr.is_empty(), "ffprobe complained: {}", std::string::String::from_utf8_lossy(&out.stderr));
    Some(std::string::String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn h264_opus_mp4() {
    let path = record("screencast-test.mp4", (1280, 720), (1280, 720), 6, true);
    let p = own_probe(&path);
    assert_eq!((p.width, p.height), (1280, 720));
    assert_eq!(p.profile, 77, "Main profile");
    assert!(p.video_samples > 100 && p.video_samples < 180, "{} samples", p.video_samples);
    assert!((5900..=6100).contains(&p.video_ms), "video {} ms", p.video_ms);
    assert!(p.audio_packets >= 295, "{} packets", p.audio_packets);
    if let Some(text) = ffprobe(&path) {
        std::println!("ffprobe: {text}");
        assert!(text.contains("codec_name=h264") && text.contains("codec_name=opus"), "{text}");
        assert!(text.contains("color_space=bt709"), "{text}");
    }
}

#[test]
fn scaled_video_only() {
    let path = record("screencast-test-720p.mp4", (1920, 1080), (1280, 720), 2, false);
    let p = own_probe(&path);
    assert_eq!((p.width, p.height, p.audio_packets), (1280, 720, 0));
    if let Some(text) = ffprobe(&path) {
        std::println!("ffprobe: {text}");
        assert!(text.contains("width=1280") && !text.contains("opus"), "{text}");
    }
}
