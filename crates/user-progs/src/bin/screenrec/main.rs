//! «Запись экрана» (22.11) — native screen recorder.
//!
//! Three ways to run:
//!
//! * `screenrec` — the window: source (monitor, whole desktop, a window's
//!   area), 30/60 frames per second, size (as is / 1080p / 720p), pointer,
//!   format (MP4 H.264 + Opus or AVI MJPEG «Совместимость»), quality, system
//!   sound, folder; «Начать запись» / «Остановить».
//! * `screenrec --record [--seconds N] [--dir D] [--fps F] [--size H]
//!   [--format mp4|avi] [--quality high|medium|low] [--sound on|off]
//!   [--out FILE]` — the recording itself, without a window (Win+Alt+R
//!   starts it; the window too). Stops when asked through the kernel
//!   (`CAP_REQUEST_STOP`: Win+Alt+R, the red tray indicator, the window) or
//!   after N seconds.
//! * `screenrec --probe FILE` — check a recording: MP4 (boxes, tables, NAL
//!   units, Opus packets — `mp4rec::probe_fd`) or AVI (chunk structure,
//!   indexes, every JPEG frame complete, sample frames decoded).
//! * `screenrec --loopback-test N` — listen to the system sound loopback for
//!   N seconds and print the level and pitch.
//!
//! MP4 (23.11, default) is in `mp4rec.rs`: H.264 by openh264 and Opus by
//! libopus, C libraries linked in by `build.rs` with their C runtime in
//! `crt.rs`. Without the library (no SDK when built) the recorder writes AVI
//! only.
//!
//! # Pipeline
//!
//! The kernel keeps a mirror of the captured rectangle, updated at present
//! time ([`user_abi::SYS_SCREENCAP`]). The main thread grabs it at the frame
//! rate into one of a few frame slots (a grab of an unchanged screen copies
//! nothing), worker threads encode slots to JPEG in parallel (MJPEG frames are
//! independent), and the main thread writes them in order into an AVI
//! (`screencast::avi`, OpenDML, any length). Unchanged frames, frames due
//! while the encoders are busy and frames the main thread was late for repeat
//! the previous picture, so the file keeps real time at a constant rate.
//!
//! Worker threads never allocate: the program heap is single-threaded
//! (`user_progs::ProgramHeap`); frames, JPEG buffers and scratch are prepared
//! by the main thread before the workers start.

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(screenrec_media)]
mod crt;
#[cfg(screenrec_media)]
mod mp4rec;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use screencast::avi::{Avi, Sink};
use screencast::jpeg::{Encoder, Scratch, Source};
use user_abi::{CAP_GRAB, CAP_INFO, CAP_POINTER, CAP_REQUEST_STOP, CAP_START, CAP_STATUS, CAP_STOP, CAP_WINDOWS, CapInfo, CapSpec, CapStatus};
use user_progs::app::{self, App, Spec};
use user_progs::filedlg::{Dialog, Options, Outcome};
use user_progs::{Args, SEEK_END, SEEK_SET, close, exit, mkdir, mmap, monotonic_ms, open, open_write, println, read, screencap, seek, sleep_ms, spawn, stat, write};

const SPEC: Spec = Spec { name: "screenrec", title: "Screen Recorder", period_ms: 500 };

/// Most encoder threads (beyond that the writer and the disk are the limit).
const MAX_WORKERS: usize = 8;
/// Stack of an encoder thread.
const WORKER_STACK: usize = 256 * 1024;
/// JPEG quality (libjpeg scale): text of the desktop stays readable.
const QUALITY: u32 = 80;
/// H.264 and Opus are linked in (see `build.rs`).
const MEDIA: bool = cfg!(screenrec_media);
/// Names of the quality presets of MP4 (`mp4rec::PRESETS`).
const PRESET_NAMES: [&str; 3] = ["Высокое", "Среднее", "Экономное"];
const PRESET_KEYS: [&str; 3] = ["high", "medium", "low"];

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// What to record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Area {
    /// All monitors (their bounding rectangle).
    Desktop,
    /// One monitor by number (0 — primary).
    Monitor(usize),
    /// A rectangle of the desktop (a window where it stood).
    Rect(i32, i32, u32, u32),
}

#[derive(Clone, Debug)]
struct Settings {
    dir: String,
    fps: u32,
    /// Output height: 0 — as captured, else 1080 / 720 (never upscaled).
    size: u32,
    area: Area,
    pointer: bool,
    quality: u32,
    /// MP4 (H.264 + Opus); `false` — AVI (MJPEG), «Совместимость».
    mp4: bool,
    /// MP4 quality preset: 0 high, 1 medium, 2 economical.
    preset: usize,
    /// Record the system sound (MP4 only).
    sound: bool,
}

impl Settings {
    fn defaults() -> Self {
        Self {
            dir: format!("{}/Videos", user_progs::filedlg::home_dir()),
            fps: 30,
            size: 0,
            area: Area::Monitor(0),
            pointer: true,
            quality: QUALITY,
            mp4: MEDIA,
            preset: 1,
            sound: true,
        }
    }

    fn path() -> String {
        format!("{}/.config/screenrec.cfg", user_progs::filedlg::home_dir())
    }

    fn load() -> Self {
        let mut settings = Self::defaults();
        let Some(text) = read_text(&Self::path(), 16 * 1024) else {
            return settings;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "dir" if !value.is_empty() => settings.dir = value.to_string(),
                "fps" => settings.fps = if value == "60" { 60 } else { 30 },
                "size" => settings.size = value.parse().ok().filter(|v| matches!(v, 0 | 720 | 1080)).unwrap_or(0),
                "pointer" => settings.pointer = value != "0",
                "quality" => settings.quality = value.parse().unwrap_or(QUALITY).clamp(30, 95),
                "format" => settings.mp4 = MEDIA && value != "avi",
                "preset" => settings.preset = PRESET_KEYS.iter().position(|k| *k == value).unwrap_or(1),
                "sound" => settings.sound = value != "0",
                "area" => {
                    settings.area = if value == "desktop" {
                        Area::Desktop
                    } else if let Some(n) = value.strip_prefix("monitor:") {
                        Area::Monitor(n.parse().unwrap_or(0))
                    } else if let Some(r) = value.strip_prefix("rect:") {
                        let v: Vec<i64> = r.split(',').filter_map(|p| p.trim().parse().ok()).collect();
                        if v.len() == 4 && v[2] > 0 && v[3] > 0 { Area::Rect(v[0] as i32, v[1] as i32, v[2] as u32, v[3] as u32) } else { Area::Monitor(0) }
                    } else {
                        Area::Monitor(0)
                    };
                }
                _ => {}
            }
        }
        settings
    }

    fn save(&self) {
        let area = match self.area {
            Area::Desktop => String::from("desktop"),
            Area::Monitor(n) => format!("monitor:{n}"),
            Area::Rect(x, y, w, h) => format!("rect:{x},{y},{w},{h}"),
        };
        let text = format!(
            "# Запись экрана (screenrec)\ndir={}\nfps={}\nsize={}\narea={area}\npointer={}\nquality={}\nformat={}\npreset={}\nsound={}\n",
            self.dir,
            self.fps,
            self.size,
            u8::from(self.pointer),
            self.quality,
            if self.mp4 { "mp4" } else { "avi" },
            PRESET_KEYS[self.preset.min(2)],
            u8::from(self.sound)
        );
        let config = format!("{}/.config", user_progs::filedlg::home_dir());
        let _ = mkdir(&config, 0o755);
        write_text(&Self::path(), &text);
    }
}

fn read_text(path: &str, limit: usize) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    while bytes.len() < limit {
        let got = read(fd, &mut chunk);
        if got <= 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..got as usize]);
    }
    close(fd);
    String::from_utf8(bytes).ok()
}

fn write_text(path: &str, text: &str) -> bool {
    let fd = open_write(path, true, true);
    if fd < 0 {
        return false;
    }
    let ok = write_all(fd, text.as_bytes());
    close(fd);
    ok
}

fn write_all(fd: i64, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let n = write(fd, data);
        if n <= 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

/// 27.11 (hw-log20, «цвет капец при записи»): the colour bars of the
/// recording check, left to right. `fossai-obs --check-colors` keeps the same
/// table: white and black (range), mid grey (the transfer curve: an sRGB
/// value taken as linear and encoded again comes out ~188), the primaries
/// (matrix), yellow and the desktop's blue.
const BARS: [(u8, u8, u8); 8] = [(255, 255, 255), (128, 128, 128), (0, 0, 0), (255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 0), (40, 90, 170)];

/// `--bars FILE [W H]`: the bars as a 24-bit BMP (shown full screen by
/// «Фотографии» while a recording runs).
fn write_bars(path: &str, w: u32, h: u32) -> i64 {
    let (w, h) = (w.clamp(64, 8192), h.clamp(64, 8192));
    let row = (w as usize * 3).next_multiple_of(4);
    let size = 54 + row * h as usize;
    let mut out = Vec::with_capacity(size);
    let le32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    out.extend_from_slice(b"BM");
    le32(&mut out, size as u32);
    le32(&mut out, 0);
    le32(&mut out, 54);
    le32(&mut out, 40);
    le32(&mut out, w);
    le32(&mut out, h);
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    for v in [0, (row * h as usize) as u32, 2835, 2835, 0, 0] {
        le32(&mut out, v);
    }
    let mut line = Vec::with_capacity(row);
    for x in 0..w {
        let (r, g, b) = BARS[(x as usize * BARS.len()) / w as usize];
        line.extend_from_slice(&[b, g, r]);
    }
    line.resize(row, 0);
    // Bottom-up rows, all the same.
    for _ in 0..h {
        out.extend_from_slice(&line);
    }
    let fd = open_write(path, true, true);
    let ok = fd >= 0 && write_all(fd, &out);
    if fd >= 0 {
        close(fd);
    }
    let table: Vec<String> = BARS.iter().map(|(r, g, b)| format!("{r},{g},{b}")).collect();
    println(&format!("screenrec: bars {w}x{h} {} {path}: {}", if ok { "written to" } else { "FAILED to write" }, table.join(" ")));
    i64::from(!ok)
}

/// `--bars-show`: [`BARS`] over the whole monitor (Esc closes).
struct BarsWindow;

impl App for BarsWindow {
    fn draw(&self, s: &mut Surface, area: Rect, _ctx: Ctx) {
        let n = BARS.len() as u32;
        for (i, &(r, g, b)) in BARS.iter().enumerate() {
            let x0 = area.x + (area.w * i as u32 / n) as i32;
            let x1 = area.x + (area.w * (i as u32 + 1) / n) as i32;
            s.fill(Rect::new(x0, area.y, (x1 - x0) as u32, area.h), mini_ui::Color::rgb(r, g, b));
        }
    }
    fn key(&mut self, code: u32) -> bool {
        if code == 0x1B {
            println("screenrec: bars closed");
            exit(0);
        }
        false
    }
    fn fullscreen(&self) -> bool {
        true
    }
    fn title(&self) -> Option<String> {
        Some(String::from("Полосы цвета — Запись экрана"))
    }
}

/// Where the last recording went (the window shows it and opens its folder).
fn last_path_file() -> String {
    format!("{}/.config/screenrec.last", user_progs::filedlg::home_dir())
}

/// `mkdir -p`.
fn make_dirs(path: &str) {
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        let mut info = user_abi::Stat::default();
        if stat(&at, &mut info) < 0 {
            let _ = mkdir(&at, 0o755);
        }
    }
}

/// Is `path` on a volume in memory (the Live stick keeps `/root` in RAM)?
/// Answer: the mount point and its kind when it is.
fn in_memory(path: &str) -> Option<String> {
    let mut buffer = alloc::vec![0u8; 8192];
    let n = user_progs::mounts(&mut buffer);
    if n <= 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer[..n as usize]).to_string();
    let mut best: Option<(&str, &str, &str)> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let (Some(point), Some(kind)) = (fields.first(), fields.get(1)) else {
            continue;
        };
        let label = fields.get(4).copied().unwrap_or("");
        let inside = *point == "/" || path == *point || path.starts_with(&format!("{point}/"));
        if inside && best.is_none_or(|(p, _, _)| point.len() > p.len()) {
            best = Some((point, kind, label));
        }
    }
    let (point, kind, label) = best?;
    let lower = format!("{kind} {label}").to_lowercase();
    (lower.contains("ram") || lower.contains("tmp") || lower.contains("scratch") || lower.contains("memory") || lower.contains("initrd")).then(|| format!("{point} ({kind})"))
}

// ---------------------------------------------------------------------------
// Kernel capture
// ---------------------------------------------------------------------------

fn cap_info() -> Option<CapInfo> {
    let mut info = CapInfo::default();
    for _ in 0..50 {
        // SAFETY: the answer structure lives in this program.
        let r = unsafe { screencap(CAP_INFO, core::ptr::from_mut(&mut info) as usize, 0) };
        if r == 0 {
            return Some(info);
        }
        if r != user_abi::ERR_AGAIN {
            return None;
        }
        sleep_ms(20);
    }
    None
}

fn cap_status() -> CapStatus {
    let mut status = CapStatus::default();
    // SAFETY: the answer structure lives in this program.
    unsafe { screencap(CAP_STATUS, core::ptr::from_mut(&mut status) as usize, 0) };
    status
}

fn cap_request_stop() -> bool {
    // SAFETY: no pointers.
    unsafe { screencap(CAP_REQUEST_STOP, 0, 0) == 0 }
}

/// Windows on the desktop, top first: rectangle and name.
fn cap_windows() -> Vec<((i32, i32, u32, u32), String)> {
    let mut buffer = alloc::vec![0u8; 16 * 1024];
    let mut n = -1;
    for _ in 0..50 {
        // SAFETY: the buffer lives in this program, its length is its own.
        n = unsafe { screencap(CAP_WINDOWS, buffer.as_mut_ptr() as usize, buffer.len()) };
        if n != user_abi::ERR_AGAIN {
            break;
        }
        sleep_ms(20);
    }
    if n <= 0 {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buffer[..n as usize]).to_string();
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(5, '\t').collect();
            if f.len() < 5 {
                return None;
            }
            Some(((f[0].parse().ok()?, f[1].parse().ok()?, f[2].parse().ok()?, f[3].parse().ok()?), f[4].to_string()))
        })
        .collect()
}

/// The rectangle of `area` on this desktop (clipped to it).
fn area_rect(area: Area, info: &CapInfo) -> (i32, i32, u32, u32) {
    let count = (info.count as usize).clamp(1, 8);
    let desk = (0, 0, info.desk_w.max(1), info.desk_h.max(1));
    let rect = match area {
        Area::Desktop => desk,
        Area::Monitor(n) => {
            let o = info.outputs[n.min(count - 1)];
            (o[0], o[1], o[2].max(1) as u32, o[3].max(1) as u32)
        }
        Area::Rect(x, y, w, h) => (x, y, w, h),
    };
    // Clip to the desktop; even sizes (players are happier with them).
    let (dw, dh) = ((desk.2 as i32).max(2), (desk.3 as i32).max(2));
    let x0 = rect.0.clamp(0, dw - 2);
    let y0 = rect.1.clamp(0, dh - 2);
    let x1 = (rect.0 + rect.2 as i32).min(dw).max(x0 + 2);
    let y1 = (rect.1 + rect.3 as i32).min(dh).max(y0 + 2);
    (x0, y0, ((x1 - x0) as u32) & !1, ((y1 - y0) as u32) & !1)
}

/// Bitrate of an MP4 preset, bits per second (as `mp4rec::video_bitrate`,
/// also for builds without it — the window shows it).
fn mp4_bitrate(preset: usize, w: u32, h: u32, fps: u32) -> i32 {
    const BPP: [f32; 3] = [0.15, 0.08, 0.04];
    ((w as f32) * (h as f32) * (fps as f32) * BPP[preset.min(2)]).clamp(1_500_000.0, 40_000_000.0) as i32
}

/// Output size for a captured `w`×`h` and the size setting.
fn out_size(w: u32, h: u32, size: u32) -> (u32, u32) {
    if size == 0 || h <= size {
        return (w, h);
    }
    let ow = ((u64::from(w) * u64::from(size) / u64::from(h)) as u32 + 1) & !1;
    (ow.max(2), size)
}

// ---------------------------------------------------------------------------
// Encoder threads
// ---------------------------------------------------------------------------

const FREE: u32 = 0;
const FILLED: u32 = 1;
const BUSY: u32 = 2;
const DONE: u32 = 3;

/// A frame on its way: captured points and its JPEG.
struct Slot {
    state: AtomicU32,
    raw: *mut u32,
    jpeg: *mut u8,
    cap: usize,
    /// Bytes of JPEG (0 — did not fit).
    len: AtomicUsize,
}

/// What the workers share with the main thread. Built before they start and
/// never resized.
struct Shared {
    slots: Vec<Slot>,
    scratch: Vec<UnsafeCell<Scratch>>,
    encoder: Encoder,
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    bgr: bool,
    /// Bumped on every new job; workers sleep on it.
    jobs: AtomicU32,
    quit: AtomicBool,
    alive: AtomicU32,
    encode_ns: AtomicU64,
    encoded: AtomicU64,
}

// SAFETY: the raw buffers of a slot are touched by one thread at a time, as
// its `state` says (FILLED/BUSY — the worker that took it, FREE/DONE — the
// main thread); a scratch belongs to one worker by index.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

static SHARED: AtomicUsize = AtomicUsize::new(0);

fn now_ns() -> u64 {
    user_progs::clock(user_abi::CLOCK_MONOTONIC).map_or(0, |t| t.seconds * 1_000_000_000 + u64::from(t.nanos))
}

extern "C" fn worker(index: usize) -> ! {
    // SAFETY: set before any worker starts and leaked — lives to the end.
    let shared = unsafe { &*(SHARED.load(Ordering::Acquire) as *const Shared) };
    // SAFETY: scratch `index` belongs to this worker only.
    let scratch = unsafe { &mut *shared.scratch[index].get() };
    loop {
        let seen = shared.jobs.load(Ordering::Acquire);
        let mut did = false;
        for slot in &shared.slots {
            if slot.state.compare_exchange(FILLED, BUSY, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                continue;
            }
            did = true;
            let started = now_ns();
            let points = shared.src_w as usize * shared.src_h as usize;
            // SAFETY: the slot is BUSY — ours until DONE; sizes are those the
            // main thread allocated.
            let (pixels, out) = unsafe { (core::slice::from_raw_parts(slot.raw, points), core::slice::from_raw_parts_mut(slot.jpeg, slot.cap)) };
            let src = Source { pixels, width: shared.src_w, height: shared.src_h, stride: shared.src_w, bgr: shared.bgr };
            let len = shared.encoder.encode(&src, shared.out_w, shared.out_h, scratch, out).unwrap_or(0);
            slot.len.store(len, Ordering::Relaxed);
            shared.encode_ns.fetch_add(now_ns().saturating_sub(started), Ordering::Relaxed);
            shared.encoded.fetch_add(1, Ordering::Relaxed);
            slot.state.store(DONE, Ordering::Release);
        }
        if !did {
            if shared.quit.load(Ordering::Acquire) {
                break;
            }
            user_progs::futex_wait(&shared.jobs, seen);
        }
    }
    shared.alive.fetch_sub(1, Ordering::AcqRel);
    user_progs::thread_exit(0)
}

/// Bytes of the AVI into a file.
struct FileSink(i64);

impl Sink for FileSink {
    fn write(&mut self, data: &[u8]) -> bool {
        write_all(self.0, data)
    }
    fn write_at(&mut self, offset: u64, data: &[u8]) -> bool {
        seek(self.0, offset as i64, SEEK_SET) >= 0 && write_all(self.0, data) && seek(self.0, 0, SEEK_END) >= 0
    }
}

/// A frame of the timeline: a slot being encoded, or the previous picture
/// again (with why).
enum Entry {
    Slot(usize),
    Same,
}

fn map_buffer(bytes: usize) -> Option<*mut u8> {
    let at = mmap(bytes.div_ceil(4096) * 4096, 0);
    (at > 0).then_some(at as usize as *mut u8)
}

/// File name from the local time: `Screen_2026-11-22_14-05-33.<ext>`.
fn file_name(ext: &str) -> String {
    let offset = local_offset();
    let now = user_progs::time_now() as i64 + i64::from(offset) * 60;
    let days = now.div_euclid(86_400);
    let secs = now.rem_euclid(86_400);
    let (year, month, day) = calendar::civil_from_days(days);
    format!("Screen_{year:04}-{month:02}-{day:02}_{:02}-{:02}-{:02}.{ext}", secs / 3600, secs / 60 % 60, secs % 60)
}

/// Local time offset, minutes (`/etc/system.cfg`, as the kernel reads it).
fn local_offset() -> i32 {
    let Some(text) = read_text("/etc/system.cfg", 8192) else {
        return 0;
    };
    sysconf::value(&text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

/// The recording: until asked to stop or `seconds` pass. Exit code.
fn record(settings: &Settings, seconds: Option<u64>, out: Option<String>) -> i64 {
    let Some(info) = cap_info() else {
        println("screenrec: FAILED no desktop to capture");
        return 1;
    };
    let (x, y, w, h) = area_rect(settings.area, &info);
    let (out_w, out_h) = out_size(w, h, settings.size);
    let fps = if settings.fps >= 60 { 60 } else { 30 };
    let cpus = user_progs::sysinfo().map_or(2, |i| i.cpus.max(1)) as usize;
    let workers = cpus.saturating_sub(1).clamp(1, MAX_WORKERS);
    let nslots = workers + 3;

    // The capture first: a second recorder is refused before it touches a file.
    let spec = CapSpec { x, y, w, h, flags: if settings.pointer { CAP_POINTER } else { 0 }, fps };
    // SAFETY: the spec lives in this program.
    let started = unsafe { screencap(CAP_START, core::ptr::from_ref(&spec) as usize, 0) };
    if started != 0 {
        println(&format!(
            "screenrec: FAILED capture refused: {}",
            if started == user_abi::ERR_EXISTS { String::from("another recording runs") } else { format!("{started}") }
        ));
        return 1;
    }
    let give_up = |why: String| -> i64 {
        println(&format!("screenrec: FAILED {why}"));
        // SAFETY: no path — no notification.
        unsafe { screencap(CAP_STOP, 0, 0) };
        1
    };

    make_dirs(&settings.dir);
    let base = format!("{}/{}", settings.dir.trim_end_matches('/'), file_name("avi"));
    let mut path = base.clone();
    for n in 2..100 {
        let mut info = user_abi::Stat::default();
        if stat(&path, &mut info) < 0 {
            break;
        }
        path = format!("{}_{n}.avi", base.trim_end_matches(".avi"));
    }
    let path = out.unwrap_or(path);
    let fd = open_write(&path, true, true);
    if fd < 0 {
        return give_up(format!("cannot create {path}: {fd}"));
    }
    if let Some(volume) = in_memory(&settings.dir) {
        println(&format!("screenrec: note: {} is in memory ({volume}) and is lost on reboot", settings.dir));
    }

    // Buffers before the threads (the heap is single-threaded).
    let points = w as usize * h as usize;
    let cap = (out_w as usize * out_h as usize * 2).min(screencast::jpeg::max_size(out_w, out_h)) + 4096;
    let mut slots = Vec::with_capacity(nslots);
    for _ in 0..nslots {
        let (Some(raw), Some(jpeg)) = (map_buffer(points * 4), map_buffer(cap)) else {
            close(fd);
            return give_up(String::from("out of memory for frame buffers"));
        };
        slots.push(Slot { state: AtomicU32::new(FREE), raw: raw.cast::<u32>(), jpeg, cap, len: AtomicUsize::new(0) });
    }
    let Some(last) = map_buffer(cap) else {
        close(fd);
        return give_up(String::from("out of memory"));
    };
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        slots,
        scratch: (0..workers).map(|_| UnsafeCell::new(Scratch::new(out_w))).collect(),
        encoder: Encoder::new(settings.quality),
        src_w: w,
        src_h: h,
        out_w,
        out_h,
        bgr: info.format == user_abi::PIXEL_BGR,
        jobs: AtomicU32::new(0),
        quit: AtomicBool::new(false),
        alive: AtomicU32::new(0),
        encode_ns: AtomicU64::new(0),
        encoded: AtomicU64::new(0),
    }));
    SHARED.store(core::ptr::from_ref(shared) as usize, Ordering::Release);
    let Some(mut avi) = Avi::new(FileSink(fd), out_w, out_h, fps) else {
        close(fd);
        return give_up(format!("cannot write {path}"));
    };
    for index in 0..workers {
        if user_progs::thread_create(worker, index, WORKER_STACK).is_some() {
            shared.alive.fetch_add(1, Ordering::AcqRel);
        }
    }
    let threads = shared.alive.load(Ordering::Acquire);
    println(&format!(
        "screenrec: recording {w}x{h} at {x},{y} -> {out_w}x{out_h} {fps} fps, {threads} encoder thread(s), {nslots} slots, into {path}"
    ));
    if threads == 0 {
        println("screenrec: FAILED no encoder threads");
        // SAFETY: no path — no notification.
        unsafe { screencap(CAP_STOP, 0, 0) };
        close(fd);
        return 1;
    }

    // The mirror fills at the next desktop frame: wait for it a little so the
    // first frame is not black.
    let fill_deadline = monotonic_ms() + 700;
    while cap_status().seq <= 1 && monotonic_ms() < fill_deadline {
        sleep_ms(10);
    }

    let mut queue: VecDeque<Entry> = VecDeque::new();
    let mut last_len = 0usize;
    let mut have_seq = 0u64;
    let (mut unique, mut same, mut late, mut busy, mut written) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut grab_ns = 0u64;
    let t0 = monotonic_ms();
    let mut k = 0u64;
    let mut next_status = t0;
    let mut stop_reason = "asked";
    let write_frame = |avi: &mut Avi<FileSink>, queue: &mut VecDeque<Entry>, last_len: &mut usize, written: &mut u64, all: bool| {
        while let Some(entry) = queue.front() {
            match *entry {
                Entry::Slot(index) => {
                    let slot = &shared.slots[index];
                    if slot.state.load(Ordering::Acquire) != DONE {
                        if !all {
                            break;
                        }
                        sleep_ms(1);
                        continue;
                    }
                    let len = slot.len.load(Ordering::Relaxed);
                    if len > 0 {
                        // SAFETY: DONE — the main thread owns the slot; both
                        // buffers are `cap` bytes.
                        unsafe { core::ptr::copy_nonoverlapping(slot.jpeg, last, len) };
                        *last_len = len;
                    }
                    slot.state.store(FREE, Ordering::Release);
                }
                Entry::Same => {}
            }
            queue.pop_front();
            if *last_len > 0 {
                // SAFETY: `last` holds `last_len` bytes written above.
                let jpeg = unsafe { core::slice::from_raw_parts(last, *last_len) };
                avi.frame(jpeg);
                *written += 1;
            }
        }
    };
    loop {
        let now = monotonic_ms();
        if now >= next_status {
            next_status = now + 200;
            let status = cap_status();
            if status.active == 0 {
                stop_reason = "the capture ended";
                break;
            }
            if status.stop_requested != 0 {
                break;
            }
        }
        if seconds.is_some_and(|limit| now.saturating_sub(t0) >= limit * 1000) {
            stop_reason = "time is up";
            break;
        }
        if avi.failed() {
            stop_reason = "write failed (disk full?)";
            break;
        }
        let due = t0 + k * 1000 / u64::from(fps);
        if now < due {
            write_frame(&mut avi, &mut queue, &mut last_len, &mut written, false);
            // Short sleeps end at their deadline (`hrsleep`), not at the
            // next 10 ms tick.
            let wait = (due - monotonic_ms().min(due)).min(4);
            if wait > 0 {
                user_progs::nanosleep(0, wait as u32 * 1_000_000);
            }
            continue;
        }
        k += 1;
        // Behind by more than a frame: this one repeats the previous picture.
        if now >= t0 + k * 1000 / u64::from(fps) && k > 1 {
            queue.push_back(Entry::Same);
            late += 1;
            continue;
        }
        let Some(index) = shared.slots.iter().position(|slot| slot.state.load(Ordering::Acquire) == FREE) else {
            queue.push_back(Entry::Same);
            busy += 1;
            continue;
        };
        let slot = &shared.slots[index];
        let g0 = now_ns();
        // SAFETY: the slot is FREE — the main thread owns its buffer of `w*h` points.
        let seq = unsafe { screencap(CAP_GRAB, slot.raw as usize, have_seq as usize) };
        grab_ns += now_ns().saturating_sub(g0);
        if seq <= 0 {
            stop_reason = "the capture ended";
            break;
        }
        if seq as u64 == have_seq {
            queue.push_back(Entry::Same);
            same += 1;
        } else {
            have_seq = seq as u64;
            slot.state.store(FILLED, Ordering::Release);
            shared.jobs.fetch_add(1, Ordering::AcqRel);
            user_progs::futex_wake(&shared.jobs, 1);
            queue.push_back(Entry::Slot(index));
            unique += 1;
        }
    }
    let elapsed_ms = monotonic_ms().saturating_sub(t0).max(1);
    write_frame(&mut avi, &mut queue, &mut last_len, &mut written, true);
    shared.quit.store(true, Ordering::Release);
    shared.jobs.fetch_add(1, Ordering::AcqRel);
    user_progs::futex_wake(&shared.jobs, 0);
    let frames = avi.frames();
    let (_, ok) = avi.finish();
    let bytes = user_progs::file_size(fd).max(0) as u64;
    close(fd);
    let encoded = shared.encoded.load(Ordering::Relaxed).max(1);
    let encode_ms_x10 = shared.encode_ns.load(Ordering::Relaxed) / encoded / 100_000;
    let grabbed = unique.max(1);
    println(&format!(
        "screenrec: stopped ({stop_reason}) after {}.{} s: {frames} frames = {}.{} fps in the file, {unique} new, {same} unchanged, {late} late, {busy} encoder busy",
        elapsed_ms / 1000,
        elapsed_ms % 1000 / 100,
        frames as u64 * 1000 / elapsed_ms,
        frames as u64 * 10_000 / elapsed_ms % 10,
    ));
    println(&format!(
        "screenrec: encode {}.{} ms/frame on {threads} thread(s) (capacity ~{} fps), grab {} us/frame, {} KB/frame, {bytes} bytes",
        encode_ms_x10 / 10,
        encode_ms_x10 % 10,
        if encode_ms_x10 == 0 { 9999 } else { u64::from(threads) * 10_000 / encode_ms_x10 },
        grab_ns / grabbed / 1000,
        bytes / u64::from(frames.max(1)) / 1024,
    ));
    let _ = written;
    if !ok {
        println(&format!("screenrec: FAILED writing {path}"));
        // SAFETY: no path — no notification.
        unsafe { screencap(CAP_STOP, 0, 0) };
        return 1;
    }
    make_dirs(&format!("{}/.config", user_progs::filedlg::home_dir()));
    write_text(&last_path_file(), &path);
    // SAFETY: the path lives in this program.
    unsafe { screencap(CAP_STOP, path.as_ptr() as usize, path.len()) };
    println(&format!("screenrec: saved {path}"));
    // Workers leave on `quit`; give them a moment before the process goes.
    let deadline = monotonic_ms() + 1000;
    while shared.alive.load(Ordering::Acquire) > 0 && monotonic_ms() < deadline {
        sleep_ms(5);
    }
    0
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Check a recorded AVI. Exit code 0 — fine.
fn probe(path: &str) -> i64 {
    match probe_file(path) {
        Ok(text) => {
            println(&format!("screenrec: probe OK {text}"));
            0
        }
        Err(why) => {
            println(&format!("screenrec: probe FAILED {path}: {why}"));
            1
        }
    }
}

fn read_exact_at(fd: i64, offset: u64, buf: &mut [u8]) -> bool {
    let mut got = 0;
    while got < buf.len() {
        let n = user_progs::read_at(fd, offset + got as u64, &mut buf[got..]);
        if n <= 0 {
            return false;
        }
        got += n as usize;
    }
    true
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn probe_file(path: &str) -> Result<String, String> {
    let fd = open(path);
    if fd < 0 {
        return Err(format!("cannot open: {fd}"));
    }
    let result = probe_fd(fd);
    close(fd);
    result
}

fn probe_fd(fd: i64) -> Result<String, String> {
    let mut magic = [0u8; 8];
    if read_exact_at(fd, 0, &mut magic) && &magic[4..8] == b"ftyp" {
        #[cfg(screenrec_media)]
        return mp4rec::probe_fd(fd);
        #[cfg(not(screenrec_media))]
        return Err(String::from("MP4, but this build has no MP4 support"));
    }
    let size = user_progs::file_size(fd).max(0) as u64;
    let mut head = [0u8; 12];
    let mut at = 0u64;
    let (mut width, mut height, mut fps, mut header_frames, mut odml_frames) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let (mut frames, mut segments, mut idx1, mut ix_entries, mut decoded) = (0u32, 0u32, 0u32, 0u32, 0u32);
    let mut frame = Vec::new();
    while at + 12 <= size {
        if !read_exact_at(fd, at, &mut head) {
            return Err(format!("short read at {at}"));
        }
        if &head[..4] != b"RIFF" {
            return Err(format!("no RIFF at {at}"));
        }
        let riff_end = at + 8 + u64::from(le32(&head, 4));
        if riff_end > size {
            return Err(format!("RIFF at {at} runs past the end ({riff_end} > {size})"));
        }
        let first = &head[8..12] == b"AVI ";
        if !first && &head[8..12] != b"AVIX" {
            return Err(format!("RIFF {:?} at {at}", &head[8..12]));
        }
        segments += 1;
        let mut c = at + 12;
        while c + 8 <= riff_end {
            let mut ch = [0u8; 12];
            if !read_exact_at(fd, c, &mut ch[..8]) {
                return Err(format!("short chunk at {c}"));
            }
            let len = u64::from(le32(&ch, 4));
            let tag = [ch[0], ch[1], ch[2], ch[3]];
            if &tag == b"LIST" {
                read_exact_at(fd, c + 8, &mut ch[8..12]);
                let kind = [ch[8], ch[9], ch[10], ch[11]];
                if &kind == b"hdrl" {
                    let mut hdrl = alloc::vec![0u8; len as usize];
                    read_exact_at(fd, c + 8, &mut hdrl);
                    if let Some(p) = hdrl.windows(4).position(|w| w == b"avih") {
                        let body = p + 8;
                        fps = 1_000_000 / le32(&hdrl, body).max(1);
                        header_frames = le32(&hdrl, body + 16);
                        width = le32(&hdrl, body + 32);
                        height = le32(&hdrl, body + 36);
                    }
                    if let Some(p) = hdrl.windows(4).position(|w| w == b"dmlh") {
                        odml_frames = le32(&hdrl, p + 8);
                    }
                } else if &kind == b"movi" {
                    let mut m = c + 12;
                    let end = c + 8 + len;
                    while m + 8 <= end {
                        let mut sub = [0u8; 8];
                        read_exact_at(fd, m, &mut sub);
                        let sl = le32(&sub, 4) as usize;
                        if &sub[..4] == b"00dc" {
                            frames += 1;
                            frame.resize(sl, 0);
                            if !read_exact_at(fd, m + 8, &mut frame) {
                                return Err(format!("frame {frames} cut"));
                            }
                            if sl < 4 || frame[..2] != [0xFF, 0xD8] || frame[sl - 2..] != [0xFF, 0xD9] {
                                return Err(format!("frame {frames} is not a whole JPEG"));
                            }
                            if frames == 1 || frames % 60 == 0 {
                                let (dw, dh) = decode(&frame).map_err(|why| format!("frame {frames}: {why}"))?;
                                if dw != width as usize || dh != height as usize {
                                    return Err(format!("frame {frames} is {dw}x{dh}, the header says {width}x{height}"));
                                }
                                decoded += 1;
                            }
                        } else if &sub[..4] == b"ix00" {
                            let mut n = [0u8; 8];
                            read_exact_at(fd, m + 8, &mut n);
                            ix_entries += le32(&n, 4);
                        }
                        m += 8 + sl as u64 + (sl as u64 & 1);
                    }
                }
            } else if &tag == b"idx1" {
                idx1 = (len / 16) as u32;
            }
            c += 8 + len + (len & 1);
        }
        at = riff_end;
    }
    if frames == 0 {
        return Err(String::from("no frames"));
    }
    if odml_frames != frames || ix_entries != frames {
        return Err(format!("{frames} frames, but dmlh says {odml_frames} and ix00 lists {ix_entries}"));
    }
    if segments == 1 && (idx1 != frames || header_frames != frames) {
        return Err(format!("{frames} frames, but idx1 lists {idx1} and avih says {header_frames}"));
    }
    let tenths = u64::from(frames) * 10 / u64::from(fps.max(1));
    Ok(format!(
        "{frames} frames {width}x{height} @{fps} fps = {}.{} s, {segments} RIFF, {decoded} decoded, {size} bytes",
        tenths / 10,
        tenths % 10
    ))
}

fn decode(data: &[u8]) -> Result<(usize, usize), &'static str> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB).set_max_width(16_384).set_max_height(16_384).set_strict_mode(true);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(data, options);
    let pixels = decoder.decode().map_err(|_| "JPEG does not decode")?;
    let (w, h) = decoder.dimensions().ok_or("no JPEG size")?;
    if pixels.len() < w * h * 3 {
        return Err("short JPEG data");
    }
    Ok((w, h))
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Area(usize),
    NextWindow,
    Fps(u32),
    Size(u32),
    Pointer(bool),
    /// MP4 (`true`) or AVI.
    Format(bool),
    Preset(usize),
    Sound(bool),
    Folder,
    Start,
    Stop,
    OpenFolder,
}

/// One button of the window.
struct Control {
    action: Action,
    rect: Rect,
    label: String,
    on: bool,
}

struct Recorder {
    settings: Settings,
    info: CapInfo,
    /// Window choice: its rectangle and name.
    window: Option<((i32, i32, u32, u32), String)>,
    /// Which window of the list the next «Окно» click takes.
    window_turn: usize,
    status: CapStatus,
    next_poll: u64,
    last: Option<String>,
    note: Option<String>,
    dialog: Option<Dialog>,
    started_at: u64,
    /// Scale of the desktop: the window size is laid out in it (24.11).
    scale: mini_ui::Scale,
    /// Rows closer (a 720p screen leaves the window ~520 points).
    compact: bool,
}

/// Width of the recorder window, layout points (24.11: the window has one
/// size, like the Snipping Tool bar — its rows are laid out for it).
const WINDOW_W: u32 = 800;

impl Recorder {
    fn new(scale: mini_ui::Scale, compact: bool) -> Self {
        let settings = Settings::load();
        let info = cap_info().unwrap_or_default();
        let last = read_text(&last_path_file(), 4096).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
        let mut recorder =
            Self { settings, info, window: None, window_turn: 0, status: cap_status(), next_poll: 0, last, note: None, dialog: None, started_at: 0, scale, compact };
        if let Area::Rect(x, y, w, h) = recorder.settings.area {
            recorder.window = Some(((x, y, w, h), String::from("область")));
        }
        println(&format!(
            "screenrec: ready, {} monitor(s), desktop {}x{}, folder {}",
            recorder.info.count, recorder.info.desk_w, recorder.info.desk_h, recorder.settings.dir
        ));
        recorder
    }

    /// Area choices: label and value.
    fn areas(&self) -> Vec<(String, Area)> {
        let count = self.info.count as usize;
        let mut out = Vec::new();
        if count > 1 {
            out.push((format!("Весь стол {}×{}", self.info.desk_w, self.info.desk_h), Area::Desktop));
        }
        for n in 0..count.min(8) {
            let o = self.info.outputs[n];
            let name = if count == 1 { String::from("Весь экран") } else { format!("Монитор {}", n + 1) };
            out.push((format!("{name} {}×{}", o[2], o[3]), Area::Monitor(n)));
        }
        out
    }

    fn recording(&self) -> bool {
        self.status.active != 0
    }

    /// Height the rows need at the desktop scale, layout points (24.11): the
    /// window is exactly that high — MP4 (two more rows) makes it taller.
    fn natural_h(&self) -> u32 {
        let ctx = Ctx::scaled(self.scale);
        let (controls, _, folder_row) = self.controls(ctx, Rect::new(0, 0, ctx.px(WINDOW_W), ctx.px(2000)));
        let mut bottom = controls.iter().map(|c| c.rect.bottom()).max().unwrap_or(0);
        // Room for the warning under «Папка» (the folder is in memory)
        // always: the window would jump when it comes and goes.
        bottom = bottom.max(folder_row.bottom() + ctx.px(26) as i32);
        let px = bottom.max(0) as u32 + ctx.px(if self.compact { 16 } else { 24 }) + ctx.px(theme::STATUS_H);
        (px * 100).div_ceil(self.scale.pct().max(1))
    }

    fn controls(&self, ctx: Ctx, area: Rect) -> (Vec<Control>, Vec<(String, i32)>, Rect) {
        let pad = ctx.px(24) as i32;
        let label_w = ctx.px(130) as i32;
        let row_h = ctx.px(34);
        let gap = ctx.px(8) as i32;
        let face = ctx.face(Role::Body);
        let mut controls = Vec::new();
        let mut labels = Vec::new();
        // A 720p screen leaves the window ~520 points: rows closer there.
        let compact = self.compact;
        let mut y = area.y + ctx.px(if compact { 82 } else { 96 }) as i32;
        let row_gap = ctx.px(if compact { 6 } else { 14 }) as i32;
        let right = area.right() - pad;
        let row = |label: &str, items: Vec<(Action, String, bool)>, y: &mut i32, controls: &mut Vec<Control>, labels: &mut Vec<(String, i32)>| {
            labels.push((label.to_string(), *y));
            let mut x = area.x + pad + label_w;
            for (action, text, on) in items {
                let w = face.width(&text) + ctx.px(28);
                if x + w as i32 > right && x > area.x + pad + label_w {
                    x = area.x + pad + label_w;
                    *y += row_h as i32 + gap;
                }
                controls.push(Control { action, rect: Rect::new(x, *y, w, row_h), label: text, on });
                x += w as i32 + gap;
            }
            *y += row_h as i32 + row_gap;
        };
        let mut items: Vec<(Action, String, bool)> = self.areas().into_iter().enumerate().map(|(i, (text, value))| (Action::Area(i), text, self.settings.area == value)).collect();
        let window_label = match &self.window {
            Some((_, name)) if matches!(self.settings.area, Area::Rect(..)) => format!("Окно: {}", short(name, 24)),
            _ => String::from("Окно…"),
        };
        items.push((Action::NextWindow, window_label, matches!(self.settings.area, Area::Rect(..))));
        row("Что записывать", items, &mut y, &mut controls, &mut labels);
        let fps = self.settings.fps;
        row("Частота", alloc::vec![(Action::Fps(30), String::from("30 кадров/с"), fps != 60), (Action::Fps(60), String::from("60 кадров/с"), fps == 60)], &mut y, &mut controls, &mut labels);
        let (w, h) = {
            let r = area_rect(self.settings.area, &self.info);
            (r.2, r.3)
        };
        let size = self.settings.size;
        row(
            "Размер",
            alloc::vec![
                (Action::Size(0), format!("Исходный {w}×{h}"), size == 0),
                (Action::Size(1080), String::from("1080p"), size == 1080),
                (Action::Size(720), String::from("720p"), size == 720),
            ],
            &mut y,
            &mut controls,
            &mut labels,
        );
        let pointer = self.settings.pointer;
        row("Указатель", alloc::vec![(Action::Pointer(true), String::from("Показывать"), pointer), (Action::Pointer(false), String::from("Скрыть"), !pointer)], &mut y, &mut controls, &mut labels);
        let mp4 = self.settings.mp4;
        row(
            "Формат",
            alloc::vec![(Action::Format(true), String::from("MP4 (H.264 + Opus)"), mp4), (Action::Format(false), String::from("AVI — совместимость"), !mp4)],
            &mut y,
            &mut controls,
            &mut labels,
        );
        if mp4 {
            let (w, h) = out_size(w, h, size);
            let presets = (0..PRESET_NAMES.len())
                .map(|i| {
                    let mbit = mp4_bitrate(i, w, h, self.settings.fps) / 100_000;
                    (Action::Preset(i), format!("{} · {}.{} Мбит/с", PRESET_NAMES[i], mbit / 10, mbit % 10), self.settings.preset == i)
                })
                .collect();
            row("Качество", presets, &mut y, &mut controls, &mut labels);
            let sound = self.settings.sound;
            row(
                "Звук системы",
                alloc::vec![(Action::Sound(true), String::from("Записывать"), sound), (Action::Sound(false), String::from("Без звука"), !sound)],
                &mut y,
                &mut controls,
                &mut labels,
            );
        }
        row("Папка", alloc::vec![(Action::Folder, String::from("Изменить…"), false)], &mut y, &mut controls, &mut labels);
        // The folder path goes on the row of «Папка» after the button.
        let folder_row = controls.last().map_or(Rect::EMPTY, |c| Rect::new(c.rect.right() + gap, c.rect.y, (right - c.rect.right() - gap).max(0) as u32, c.rect.h));
        // The big button.
        let big_w = ctx.px(260);
        let big = Rect::new(area.x + pad + label_w, y + ctx.px(if compact { 28 } else { 36 }) as i32, big_w, ctx.px(44));
        if self.recording() {
            controls.push(Control { action: Action::Stop, rect: big, label: String::from("Остановить запись"), on: true });
        } else {
            controls.push(Control { action: Action::Start, rect: big, label: String::from("Начать запись"), on: true });
        }
        if self.last.is_some() && !self.recording() {
            let w = face.width("Открыть папку") + ctx.px(28);
            controls.push(Control { action: Action::OpenFolder, rect: Rect::new(big.right() + gap * 2, big.y + (big.h - row_h) as i32 / 2, w, row_h), label: String::from("Открыть папку"), on: false });
        }
        (controls, labels, folder_row)
    }

    fn act(&mut self, action: Action) {
        self.note = None;
        match action {
            Action::Area(i) => {
                if let Some((_, value)) = self.areas().get(i) {
                    self.settings.area = *value;
                }
            }
            Action::NextWindow => {
                let windows: Vec<_> = cap_windows().into_iter().filter(|(_, name)| !name.contains("Screen Recorder") && !name.contains("Запись экрана")).collect();
                if windows.is_empty() {
                    self.note = Some(String::from("Нет открытых окон"));
                } else {
                    let pick = &windows[self.window_turn % windows.len()];
                    self.window_turn += 1;
                    let (x, y, w, h) = pick.0;
                    self.settings.area = Area::Rect(x, y, w, h);
                    self.window = Some(pick.clone());
                    println(&format!("screenrec: area of window '{}' {w}x{h} at {x},{y}", pick.1));
                }
            }
            Action::Fps(fps) => self.settings.fps = fps,
            Action::Size(size) => self.settings.size = size,
            Action::Pointer(on) => self.settings.pointer = on,
            Action::Format(mp4) => {
                if mp4 && !MEDIA {
                    self.note = Some(String::from("MP4 в этой сборке нет (собрана без набора C) — пишется AVI"));
                } else {
                    self.settings.mp4 = mp4;
                }
            }
            Action::Preset(preset) => self.settings.preset = preset,
            Action::Sound(on) => self.settings.sound = on,
            Action::Folder => {
                let options = Options::folder().title("Папка для записей").dir(&self.settings.dir);
                match Dialog::start_owned(&options, app::window_id(), "screenrec") {
                    Ok(dialog) => self.dialog = Some(dialog),
                    Err(why) => self.note = Some(format!("Окно выбора папки не открылось: {why}")),
                }
            }
            Action::Start => {
                self.settings.save();
                let id = spawn("/bin/screenrec --record");
                println(&format!("screenrec: recording started from the window ({id})"));
                self.started_at = monotonic_ms();
                self.next_poll = 0;
            }
            Action::Stop => {
                cap_request_stop();
                println("screenrec: stop asked from the window");
            }
            Action::OpenFolder => {
                if let Some(last) = &self.last {
                    spawn(&format!("/bin/files {last}"));
                }
            }
        }
        if !matches!(action, Action::Start | Action::Stop | Action::OpenFolder | Action::Folder) {
            self.settings.save();
        }
    }
}

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

fn clock_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

impl App for Recorder {
    fn title(&self) -> Option<String> {
        Some(String::from("Screen Recorder\x1fЗапись экрана"))
    }

    fn step(&mut self) -> bool {
        let mut dirty = false;
        if let Some(dialog) = self.dialog.as_mut() {
            match dialog.poll() {
                Some(Outcome::Chosen(dir)) => {
                    println(&format!("screenrec: folder {dir}"));
                    self.settings.dir = dir;
                    self.settings.save();
                    self.dialog = None;
                    dirty = true;
                }
                Some(Outcome::Cancelled) => {
                    self.dialog = None;
                }
                _ => {}
            }
        }
        let now = monotonic_ms();
        if now >= self.next_poll {
            self.next_poll = now + 250;
            let was = self.recording();
            self.status = cap_status();
            if was && !self.recording() {
                self.last = read_text(&last_path_file(), 4096).map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
                if let Some(last) = &self.last {
                    self.note = Some(format!("Сохранено: {last}"));
                }
            }
            dirty |= was != self.recording() || self.recording();
        }
        dirty
    }

    /// 24.11: one size, laid out for it (Roman: stretched, the rows stayed
    /// where they were) — no edges, «Развернуть» greyed, no snapping.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::fixed(WINDOW_W, self.natural_h())
    }

    fn tick(&mut self, info: &user_progs::SysInfo) -> bool {
        // The desktop scale may change on the fly (the size follows it).
        self.scale = app::ui_scale(info);
        // Monitors may come and go.
        if let Some(info) = cap_info() {
            let changed = info.count != self.info.count || info.desk_w != self.info.desk_w || info.desk_h != self.info.desk_h;
            self.info = info;
            return changed;
        }
        false
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let (controls, _, _) = self.controls(ctx, area);
        let Some(action) = controls.iter().find(|c| c.rect.contains(x, y)).map(|c| c.action) else {
            return false;
        };
        if self.recording() && !matches!(action, Action::Stop) {
            self.note = Some(String::from("Идёт запись — остановите её, чтобы менять настройки"));
            return true;
        }
        self.act(action);
        true
    }

    fn key(&mut self, code: u32) -> bool {
        if code == '\n' as u32 {
            self.act(if self.recording() { Action::Stop } else { Action::Start });
            return true;
        }
        false
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let status_h = ctx.px(theme::STATUS_H);
        let status = Rect::new(area.x, area.bottom() - status_h as i32, area.w, status_h);
        let pad = ctx.px(24) as i32;
        // Header: red camera tile, title, hint.
        let tile = Rect::new(area.x + pad, area.y + ctx.px(22) as i32, ctx.px(48), ctx.px(48));
        draw::rounded(s, tile, ctx.px(10), p.bad, 255);
        let g = ctx.px(26);
        glyphicon::draw(s, Icon::Camera, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, mini_ui::Color::rgb(0xFF, 0xFF, 0xFF), 255);
        let tx = tile.right() + ctx.px(16) as i32;
        let room = (area.right() - pad - tx).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Heading, tx, tile.y, room, "Запись экрана", p.ink);
        let sub = if self.recording() {
            format!("Идёт запись · {} · {}×{}", clock_text(self.status.elapsed_ms), self.status.w, self.status.h)
        } else {
            String::from(if self.settings.mp4 { "Видео экрана в MP4 (H.264, звук Opus) · Win+Alt+R — начать и остановить" } else { "Видео экрана в AVI (MJPEG) · Win+Alt+R — начать и остановить" })
        };
        paint::text_clipped(ctx, s, Role::Body, tx, tile.y + i32::from(ctx.face(Role::Heading).line) + ctx.px(4) as i32, room, &sub, if self.recording() { p.bad } else { p.ink3 });

        let (controls, labels, folder_row) = self.controls(ctx, area);
        for (label, y) in &labels {
            let row = Rect::new(area.x + pad, *y, ctx.px(130), ctx.px(34));
            paint::text(ctx, s, Role::Body, row.x, paint::baseline(ctx, Role::Body, row), label, p.ink2);
        }
        for control in &controls {
            let weight = match control.action {
                Action::Start | Action::Stop => Weight::Danger,
                _ if control.on => Weight::Primary,
                _ => Weight::Normal,
            };
            if matches!(control.action, Action::Start | Action::Stop) && !theme::is_classic() {
                // The big red button, like the record button of the Game Bar.
                draw::rounded(s, control.rect, ctx.px(theme::R_ROW), p.bad, 255);
                paint::text_centered(ctx.on(p.bad), s, Role::Label, control.rect, &control.label, mini_ui::Color::rgb(0xFF, 0xFF, 0xFF));
            } else {
                paint::button(ctx, s, control.rect, weight, &control.label, false);
            }
            if control.action == Action::Stop || control.action == Action::Start {
                // A white dot (record) or square (stop) at the left edge.
                let side = ctx.px(12);
                let mark = Rect::new(control.rect.x + ctx.px(14) as i32, control.rect.y + (control.rect.h - side) as i32 / 2, side, side);
                let radius = if control.action == Action::Start { side / 2 } else { ctx.px(2) };
                draw::rounded(s, mark, radius, mini_ui::Color::rgb(0xFF, 0xFF, 0xFF), 255);
            }
        }
        let folder_text = self.settings.dir.clone();
        paint::text_clipped(ctx, s, Role::Body, folder_row.x, paint::baseline(ctx, Role::Body, folder_row), folder_row.w, &folder_text, p.ink);
        // Live stick: the folder is in memory.
        if let Some(volume) = in_memory(&self.settings.dir) {
            let warn = Rect::new(area.x + pad, folder_row.bottom() + ctx.px(4) as i32, area.w.saturating_sub(pad as u32 * 2), ctx.px(20));
            let text = format!("Папка в оперативной памяти ({volume}): после перезагрузки запись пропадёт — выберите папку на флешке или диске");
            paint::text_clipped(ctx, s, Role::Caption, warn.x, warn.y, warn.w, &text, p.warn);
        }
        let line = self.note.clone().unwrap_or_else(|| match &self.last {
            Some(last) => format!("Последняя запись: {last}"),
            None => String::from("Enter — начать или остановить запись · Win+Alt+R работает и без этого окна"),
        });
        kit::status_bar(ctx, s, status, &line);
    }

    fn quits_on_q(&self) -> bool {
        false
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: values from the kernel, as `Args` expects.
    let args = unsafe { Args::new(argc, argv) };
    let flag = |name: &str| (1..args.len()).any(|i| args.get(i) == Some(name));
    let value = |name: &str| (1..args.len()).find(|&i| args.get(i) == Some(name)).and_then(|i| args.get(i + 1)).map(String::from);
    user_progs::heap_size(16 * 1024 * 1024);
    if let Some(path) = value("--bars") {
        let at = (1..args.len()).find(|&i| args.get(i) == Some("--bars")).unwrap_or(0);
        let num = |i: usize| args.get(i).and_then(|v| v.parse::<u32>().ok());
        exit(write_bars(&path, num(at + 2).unwrap_or(1280), num(at + 3).unwrap_or(800)));
    }
    if let Some(path) = value("--probe") {
        // `last` — the file of the last recording.
        let path = if path == "last" { read_text(&last_path_file(), 4096).map(|t| t.trim().to_string()).unwrap_or(path) } else { path };
        exit(probe(&path));
    }
    // Every `.avi` of a folder.
    if let Some(dir) = value("--probe-dir") {
        let fd = open(&dir);
        let mut names = Vec::new();
        if fd >= 0 {
            let mut entry = user_progs::Dirent::default();
            while user_progs::readdir(fd, &mut entry) {
                let len = (entry.name_len as usize).min(entry.name.len());
                let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
                if name.ends_with(".avi") || name.ends_with(".mp4") {
                    names.push(name);
                }
            }
            close(fd);
        }
        names.sort();
        let mut failed = names.is_empty();
        for name in &names {
            failed |= probe(&format!("{}/{name}", dir.trim_end_matches('/'))) != 0;
        }
        println(&format!("screenrec: probed {} file(s) in {dir}{}", names.len(), if failed { ", FAILED" } else { ", all fine" }));
        exit(i64::from(failed));
    }
    if flag("--stop") {
        exit(if cap_request_stop() { 0 } else { 1 });
    }
    if flag("--record") {
        let mut settings = Settings::load();
        if let Some(dir) = value("--dir") {
            settings.dir = dir;
        }
        if let Some(fps) = value("--fps").and_then(|v| v.parse().ok()) {
            settings.fps = fps;
        }
        if let Some(size) = value("--size").and_then(|v| v.parse().ok()) {
            settings.size = size;
        }
        if let Some(area) = value("--area") {
            settings.area = if area == "desktop" { Area::Desktop } else { Area::Monitor(area.trim_start_matches("monitor:").parse().unwrap_or(0)) };
        }
        if let Some(format) = value("--format") {
            settings.mp4 = format != "avi";
        }
        if let Some(quality) = value("--quality") {
            settings.preset = PRESET_KEYS.iter().position(|k| *k == quality).unwrap_or(1);
        }
        if let Some(sound) = value("--sound") {
            settings.sound = sound != "off" && sound != "0";
        }
        let seconds = value("--seconds").and_then(|v| v.parse().ok());
        let out = value("--out");
        if settings.mp4 {
            #[cfg(screenrec_media)]
            exit(mp4rec::record(&settings, seconds, out));
            #[cfg(not(screenrec_media))]
            println("screenrec: note: this build has no H.264 (SDK was missing), recording AVI");
        }
        exit(record(&settings, seconds, out));
    }
    if let Some(seconds) = value("--loopback-test").and_then(|v| v.parse().ok()) {
        #[cfg(screenrec_media)]
        exit(mp4rec::loopback_test(seconds));
        #[cfg(not(screenrec_media))]
        {
            let _: u64 = seconds;
            println("screenrec: FAILED this build has no sound support");
            exit(1);
        }
    }
    let info = app::start(&SPEC);
    if flag("--bars-show") {
        // 27.11: the bars full screen, for the colour check of recordings.
        println("screenrec: bars shown full screen");
        app::run(&SPEC, &info, (640, 400), |_, _| BarsWindow)
    }
    let scale = app::ui_scale(&info);
    // A screen that leaves less than 640 points under the window title
    // (720p) gets the closer rows; the window is then as high as they are.
    let work_pts = info.screen_h.saturating_sub((theme::PANEL_H + theme::TITLE_H) * scale) * 100 / scale.pct().max(1);
    let compact = work_pts < 660;
    let height: u32 = if compact { 560 } else { 640 };
    let size = (WINDOW_W * scale, height * scale);
    app::run(&SPEC, &info, size, move |_, _| Recorder::new(scale, compact))
}
