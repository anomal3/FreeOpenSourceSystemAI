//! Recording into MP4: H.264 video and Opus system sound (23.11).
//!
//! # Pipeline
//!
//! * The main thread grabs frames from the kernel mirror at the frame rate
//!   (as the AVI path) into slots, numbering them; an unchanged screen, a
//!   late frame or no free slot makes **no** sample — the previous one simply
//!   lasts longer in the file (variable frame rate on capture timestamps).
//! * Converter threads turn slots into I420 (BT.709) in parallel, frames
//!   independent ([`screencast::yuv`]).
//! * One encoder thread feeds openh264 in capture order; openh264 encodes the
//!   slices of each picture on its own threads (`threads` slices). An IDR
//!   every 5 s of wall time (and every `5 × fps` frames), so a long static
//!   screen still has key frames to seek to.
//! * The sound thread reads the system loopback of the kernel
//!   (`SOUND_LOOP_READ`) from the frame of the first picture on, so both
//!   tracks start at 0, encodes 20 ms Opus packets and passes them through a
//!   ring; a gap in what it could read (it was late by more than the ring of
//!   the kernel, ~2 s) becomes silence, the timeline stays.
//! * The main thread writes encoded pictures in order and sound packets as
//!   they come into the MP4 ([`screencast::mp4`]): sample tables in memory,
//!   `moov` at the end.
//!
//! Threads other than the main one never touch the Rust heap (it is
//! single-threaded): all buffers are made before they start. The encoders
//! allocate through their own C runtime (`crt.rs`).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use screencast::jpeg::Source;
use screencast::media::{Encoded, H264, H264Config, OPUS_FRAME, OPUS_MAX_PACKET, Opus};
use screencast::mp4::{Mp4, OpusTrack};
use screencast::yuv::Converter;
use user_abi::{CAP_GRAB, CAP_POINTER, CAP_START, CAP_STOP, CapSpec, SoundLoop};
use user_progs::{close, monotonic_ms, open_write, println, screencap, sleep_ms, stat};

use super::{FileSink, Settings, area_rect, cap_info, cap_status, file_name, in_memory, last_path_file, make_dirs, map_buffer, now_ns, out_size, write_text};

const FREE: u32 = 0;
const FILLED: u32 = 1;
const CONVERTING: u32 = 2;
const CONVERTED: u32 = 3;
const ENCODING: u32 = 4;
const ENCODED: u32 = 5;

/// Most converter threads (a conversion is a few ms; more only cost memory).
const MAX_CONVERTERS: usize = 4;
/// Most openh264 threads (more slices stop paying off beyond eight).
const MAX_ENCODER_THREADS: usize = 8;
/// Stack of converter threads (Rust code, small frames).
const CONVERTER_STACK: usize = 128 * 1024;
/// Stack of the encoder and sound threads: openh264 and libopus (`VAR_ARRAYS`)
/// keep their scratch there.
const CODEC_STACK: usize = 2 << 20;
/// A key frame at least this often, µs of the recording.
const KEY_EVERY_US: u64 = 5_000_000;
/// Opus packets the ring holds (20 s).
const PACKETS: usize = 1024;

/// Sound bitrate of the quality presets (video: `super::mp4_bitrate` —
/// 0.15 / 0.08 / 0.04 bits per pixel and frame, 2560×1440 at 30 fps:
/// 16.6 / 8.8 / 4.4 Mbit/s).
const SOUND_BITRATE: [i32; 3] = [160_000, 128_000, 96_000];

struct Slot {
    state: AtomicU32,
    /// Capture order (0, 1, 2…), set with FILLED.
    order: AtomicU64,
    time_us: AtomicU64,
    raw: *mut u32,
    yuv: *mut u8,
    bits: *mut u8,
    cap: usize,
    len: AtomicUsize,
    key: AtomicBool,
}

struct Audio {
    bitrate: i32,
    /// Loopback frame of the first picture; `u64::MAX` — not yet.
    start: AtomicU64,
    /// Loopback frame where the recording ended; `u64::MAX` — still going.
    end: AtomicU64,
    /// 0 starting, 1 running, 2 failed, 3 finished.
    state: AtomicU32,
    pre_skip: AtomicU32,
    ring: *mut u8,
    lens: Vec<AtomicU32>,
    head: AtomicU64,
    tail: AtomicU64,
    /// Frames of silence put in for what could not be read.
    lost: AtomicU64,
    /// Largest sample magnitude seen (is anything playing at all?).
    peak: AtomicU32,
    dropped: AtomicU64,
}

struct Shared {
    slots: Vec<Slot>,
    scratch: Vec<UnsafeCell<Vec<u32>>>,
    conv: Converter,
    src_w: u32,
    src_h: u32,
    bgr: bool,
    config: H264Config,
    /// 0 starting, 1 running, 2 failed.
    h264_state: AtomicU32,
    jobs: AtomicU32,
    done: AtomicU32,
    quit: AtomicBool,
    alive: AtomicU32,
    convert_ns: AtomicU64,
    converted: AtomicU64,
    encode_ns: AtomicU64,
    encoded: AtomicU64,
    failed_frames: AtomicU64,
    audio: Option<Audio>,
}

// SAFETY: slot buffers are touched by one thread at a time, as `state` says;
// a scratch belongs to one converter by index; the sound ring is written by
// the sound thread below `head` and read by the main thread below it.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

static SHARED: AtomicUsize = AtomicUsize::new(0);

fn shared() -> &'static Shared {
    // SAFETY: set before any thread starts and leaked — lives to the end.
    unsafe { &*(SHARED.load(Ordering::Acquire) as *const Shared) }
}

fn bump(word: &AtomicU32) {
    word.fetch_add(1, Ordering::AcqRel);
    user_progs::futex_wake(word, 0);
}

extern "C" fn converter(index: usize) -> ! {
    let shared = shared();
    // SAFETY: scratch `index` belongs to this converter only.
    let scratch = unsafe { &mut *shared.scratch[index].get() };
    loop {
        let seen = shared.jobs.load(Ordering::Acquire);
        let mut did = false;
        for slot in &shared.slots {
            if slot.state.compare_exchange(FILLED, CONVERTING, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                continue;
            }
            did = true;
            let started = now_ns();
            let points = shared.src_w as usize * shared.src_h as usize;
            // SAFETY: the slot is ours until CONVERTED; sizes as allocated.
            let (pixels, yuv) = unsafe { (core::slice::from_raw_parts(slot.raw, points), core::slice::from_raw_parts_mut(slot.yuv, shared.conv.frame_bytes())) };
            let src = Source { pixels, width: shared.src_w, height: shared.src_h, stride: shared.src_w, bgr: shared.bgr };
            shared.conv.convert_all(&src, yuv, scratch);
            shared.convert_ns.fetch_add(now_ns().saturating_sub(started), Ordering::Relaxed);
            shared.converted.fetch_add(1, Ordering::Relaxed);
            slot.state.store(CONVERTED, Ordering::Release);
            bump(&shared.done);
        }
        if !did {
            if shared.quit.load(Ordering::Acquire) {
                break;
            }
            user_progs::futex_wait_for(&shared.jobs, seen, 100);
        }
    }
    shared.alive.fetch_sub(1, Ordering::AcqRel);
    user_progs::thread_exit(0)
}

extern "C" fn encoder(_: usize) -> ! {
    let shared = shared();
    let Some(mut h264) = H264::open(&shared.config) else {
        shared.h264_state.store(2, Ordering::Release);
        shared.alive.fetch_sub(1, Ordering::AcqRel);
        user_progs::thread_exit(1)
    };
    shared.h264_state.store(1, Ordering::Release);
    let mut next = 0u64;
    let mut last_key = 0u64;
    loop {
        let seen = shared.done.load(Ordering::Acquire);
        let ready = shared.slots.iter().find(|slot| slot.order.load(Ordering::Acquire) == next && slot.state.load(Ordering::Acquire) == CONVERTED);
        let Some(slot) = ready else {
            if shared.quit.load(Ordering::Acquire) {
                break;
            }
            user_progs::futex_wait_for(&shared.done, seen, 50);
            continue;
        };
        slot.state.store(ENCODING, Ordering::Release);
        let time = slot.time_us.load(Ordering::Acquire);
        if next > 0 && time.saturating_sub(last_key) >= KEY_EVERY_US {
            h264.force_key();
        }
        let started = now_ns();
        // SAFETY: the slot is ours until ENCODED; sizes as allocated.
        let (yuv, bits) = unsafe { (core::slice::from_raw_parts(slot.yuv, shared.conv.frame_bytes()), core::slice::from_raw_parts_mut(slot.bits, slot.cap)) };
        match h264.encode(yuv, (time / 1000) as i64, bits) {
            Ok(Encoded::Frame(n, key)) => {
                slot.len.store(n, Ordering::Relaxed);
                slot.key.store(key, Ordering::Relaxed);
                if key {
                    last_key = time;
                }
            }
            Ok(Encoded::Skipped) => slot.len.store(0, Ordering::Relaxed),
            Err(_) => {
                slot.len.store(0, Ordering::Relaxed);
                shared.failed_frames.fetch_add(1, Ordering::Relaxed);
            }
        }
        shared.encode_ns.fetch_add(now_ns().saturating_sub(started), Ordering::Relaxed);
        shared.encoded.fetch_add(1, Ordering::Relaxed);
        slot.state.store(ENCODED, Ordering::Release);
        next += 1;
    }
    drop(h264);
    shared.alive.fetch_sub(1, Ordering::AcqRel);
    user_progs::thread_exit(0)
}

extern "C" fn sound(_: usize) -> ! {
    let shared = shared();
    let Some(audio) = shared.audio.as_ref() else { user_progs::thread_exit(0) };
    let finish = |state: u32| -> ! {
        audio.state.store(state, Ordering::Release);
        shared.alive.fetch_sub(1, Ordering::AcqRel);
        user_progs::thread_exit(0)
    };
    let Some(mut opus) = Opus::open(audio.bitrate, 5) else { finish(2) };
    audio.pre_skip.store(u32::from(opus.pre_skip), Ordering::Release);
    audio.state.store(1, Ordering::Release);
    // Wait for the first picture: the sound starts with it.
    while audio.start.load(Ordering::Acquire) == u64::MAX {
        if shared.quit.load(Ordering::Acquire) || audio.end.load(Ordering::Acquire) != u64::MAX {
            finish(3);
        }
        sleep_ms(5);
    }
    let mut next = audio.start.load(Ordering::Acquire);
    let mut pcm = [0i16; OPUS_FRAME * 2];
    let mut filled = 0usize;
    let mut bytes = [0u8; OPUS_FRAME * 4];
    let mut packet = [0u8; OPUS_MAX_PACKET];
    let mut push = |pcm: &[i16; OPUS_FRAME * 2], opus: &mut Opus| {
        let Ok(n) = opus.encode(pcm, &mut packet) else { return };
        let head = audio.head.load(Ordering::Acquire);
        if head - audio.tail.load(Ordering::Acquire) >= PACKETS as u64 {
            audio.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let at = (head % PACKETS as u64) as usize;
        // SAFETY: ring slot `at` is free (below `tail + PACKETS`), only this
        // thread writes it.
        unsafe { core::ptr::copy_nonoverlapping(packet.as_ptr(), audio.ring.add(at * OPUS_MAX_PACKET), n) };
        audio.lens[at].store(n as u32, Ordering::Release);
        audio.head.store(head + 1, Ordering::Release);
    };
    loop {
        let end = audio.end.load(Ordering::Acquire);
        if next >= end {
            if filled > 0 {
                pcm[filled * 2..].fill(0);
                push(&pcm, &mut opus);
            }
            break;
        }
        let want = (OPUS_FRAME - filled).min((end - next) as usize);
        let mut request = SoundLoop { from: next, ..SoundLoop::default() };
        let got = user_progs::sound_loop_read(&mut request, &mut bytes[..want * 4]);
        if got < 0 {
            finish(2);
        }
        if request.from > next {
            // Lost (read too late): silence in their place, at most a packet
            // at a time.
            let gap = ((request.from - next) as usize).min(OPUS_FRAME - filled);
            pcm[filled * 2..(filled + gap) * 2].fill(0);
            filled += gap;
            next += gap as u64;
            audio.lost.fetch_add(gap as u64, Ordering::Relaxed);
            if filled == OPUS_FRAME {
                push(&pcm, &mut opus);
                filled = 0;
            }
            continue;
        }
        let got = got as usize;
        for i in 0..got {
            let left = i16::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1]]);
            let right = i16::from_le_bytes([bytes[i * 4 + 2], bytes[i * 4 + 3]]);
            pcm[(filled + i) * 2] = left;
            pcm[(filled + i) * 2 + 1] = right;
            let magnitude = u32::from(left.unsigned_abs().max(right.unsigned_abs()));
            if magnitude > audio.peak.load(Ordering::Relaxed) {
                audio.peak.store(magnitude, Ordering::Relaxed);
            }
        }
        filled += got;
        next += got as u64;
        if filled == OPUS_FRAME {
            push(&pcm, &mut opus);
            filled = 0;
        } else if got == 0 {
            sleep_ms(10);
        }
    }
    drop(opus);
    finish(3)
}

/// Write encoded pictures that are ready, in order.
fn write_video(mp4: &mut Mp4<FileSink>, queue: &mut VecDeque<usize>, written: &mut u64, all: bool) {
    let shared = shared();
    while let Some(&index) = queue.front() {
        let slot = &shared.slots[index];
        if slot.state.load(Ordering::Acquire) != ENCODED {
            if !all || shared.h264_state.load(Ordering::Acquire) == 2 {
                break;
            }
            sleep_ms(1);
            continue;
        }
        let len = slot.len.load(Ordering::Relaxed);
        if len > 0 {
            // SAFETY: ENCODED — the main thread owns the slot.
            let data = unsafe { core::slice::from_raw_parts(slot.bits, len) };
            if mp4.video(data, slot.time_us.load(Ordering::Relaxed), slot.key.load(Ordering::Relaxed)) {
                *written += 1;
            }
        }
        slot.state.store(FREE, Ordering::Release);
        queue.pop_front();
    }
}

/// Write sound packets that are ready.
fn write_audio(mp4: &mut Mp4<FileSink>) {
    let Some(audio) = shared().audio.as_ref() else { return };
    let head = audio.head.load(Ordering::Acquire);
    let mut tail = audio.tail.load(Ordering::Acquire);
    while tail < head {
        let at = (tail % PACKETS as u64) as usize;
        let len = audio.lens[at].load(Ordering::Acquire) as usize;
        // SAFETY: below `head` the sound thread is done with the slot.
        let packet = unsafe { core::slice::from_raw_parts(audio.ring.add(at * OPUS_MAX_PACKET), len) };
        mp4.audio(packet, OPUS_FRAME as u32);
        tail += 1;
        audio.tail.store(tail, Ordering::Release);
    }
}

/// Record into MP4 until asked to stop or `seconds` pass. Exit code.
pub fn record(settings: &Settings, seconds: Option<u64>, out: Option<String>) -> i64 {
    let Some(info) = cap_info() else {
        println("screenrec: FAILED no desktop to capture");
        return 1;
    };
    let (x, y, w, h) = area_rect(settings.area, &info);
    let (out_w, out_h) = out_size(w, h, settings.size);
    let fps = if settings.fps >= 60 { 60 } else { 30 };
    let cpus = user_progs::sysinfo().map_or(2, |i| i.cpus.max(1)) as usize;
    let converters = cpus.saturating_sub(1).clamp(1, MAX_CONVERTERS);
    let threads = cpus.saturating_sub(1).clamp(1, MAX_ENCODER_THREADS);
    let nslots = converters + 4;
    let preset = settings.preset.min(SOUND_BITRATE.len() - 1);
    let bitrate = super::mp4_bitrate(preset, out_w, out_h, fps);

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

    let path = match out {
        Some(path) => path,
        None => {
            make_dirs(&settings.dir);
            let base = format!("{}/{}", settings.dir.trim_end_matches('/'), file_name("mp4"));
            let mut path = base.clone();
            for n in 2..100 {
                let mut info = user_abi::Stat::default();
                if stat(&path, &mut info) < 0 {
                    break;
                }
                path = format!("{}_{n}.mp4", base.trim_end_matches(".mp4"));
            }
            path
        }
    };
    let fd = open_write(&path, true, true);
    if fd < 0 {
        return give_up(format!("cannot create {path}: {fd}"));
    }
    if let Some(volume) = in_memory(&settings.dir) {
        println(&format!("screenrec: note: {} is in memory ({volume}) and is lost on reboot", settings.dir));
    }

    // Buffers before the threads (the heap is single-threaded).
    let conv = Converter::new(w, h, out_w, out_h);
    let frame_bytes = conv.frame_bytes();
    let cap = frame_bytes.min(16 << 20) + 65536;
    let mut slots = Vec::with_capacity(nslots);
    for _ in 0..nslots {
        let (Some(raw), Some(yuv), Some(bits)) = (map_buffer(w as usize * h as usize * 4), map_buffer(frame_bytes), map_buffer(cap)) else {
            close(fd);
            return give_up(String::from("out of memory for frame buffers"));
        };
        slots.push(Slot {
            state: AtomicU32::new(FREE),
            order: AtomicU64::new(u64::MAX),
            time_us: AtomicU64::new(0),
            raw: raw.cast::<u32>(),
            yuv,
            bits,
            cap,
            len: AtomicUsize::new(0),
            key: AtomicBool::new(false),
        });
    }
    let audio = if settings.sound {
        let Some(ring) = map_buffer(PACKETS * OPUS_MAX_PACKET) else {
            close(fd);
            return give_up(String::from("out of memory for sound"));
        };
        Some(Audio {
            bitrate: SOUND_BITRATE[preset],
            start: AtomicU64::new(u64::MAX),
            end: AtomicU64::new(u64::MAX),
            state: AtomicU32::new(0),
            pre_skip: AtomicU32::new(0),
            ring,
            lens: (0..PACKETS).map(|_| AtomicU32::new(0)).collect(),
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            lost: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            dropped: AtomicU64::new(0),
        })
    } else {
        None
    };
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        slots,
        scratch: (0..converters).map(|_| UnsafeCell::new(alloc::vec![0u32; 2 * out_w as usize])).collect(),
        conv,
        src_w: w,
        src_h: h,
        bgr: info.format == user_abi::PIXEL_BGR,
        config: H264Config {
            width: out_w as i32,
            height: out_h as i32,
            fps: fps as i32,
            bitrate,
            max_bitrate: 0,
            threads: threads as i32,
            keyint: (fps * 5) as i32,
            screen: 0,
            cabac: 1,
            complexity: 0,
        },
        h264_state: AtomicU32::new(0),
        jobs: AtomicU32::new(0),
        done: AtomicU32::new(0),
        quit: AtomicBool::new(false),
        alive: AtomicU32::new(0),
        convert_ns: AtomicU64::new(0),
        converted: AtomicU64::new(0),
        encode_ns: AtomicU64::new(0),
        encoded: AtomicU64::new(0),
        failed_frames: AtomicU64::new(0),
        audio,
    }));
    SHARED.store(core::ptr::from_ref(shared) as usize, Ordering::Release);
    let start = |entry: extern "C" fn(usize) -> !, arg: usize, stack: usize| {
        let ok = user_progs::thread_create(entry, arg, stack).is_some();
        if ok {
            shared.alive.fetch_add(1, Ordering::AcqRel);
        }
        ok
    };
    let encoder_ok = start(encoder, 0, CODEC_STACK);
    for index in 0..converters {
        start(converter, index, CONVERTER_STACK);
    }
    let sound_ok = shared.audio.is_some() && start(sound, 0, CODEC_STACK);
    let stop_all = || {
        shared.quit.store(true, Ordering::Release);
        bump(&shared.jobs);
        bump(&shared.done);
        let deadline = monotonic_ms() + 3000;
        while shared.alive.load(Ordering::Acquire) > 0 && monotonic_ms() < deadline {
            sleep_ms(5);
        }
    };
    // The encoders start on their threads: wait for both to say yes or no.
    let deadline = monotonic_ms() + 10_000;
    while (shared.h264_state.load(Ordering::Acquire) == 0 || shared.audio.as_ref().is_some_and(|a| sound_ok && a.state.load(Ordering::Acquire) == 0)) && monotonic_ms() < deadline {
        sleep_ms(5);
    }
    if !encoder_ok || shared.h264_state.load(Ordering::Acquire) != 1 {
        stop_all();
        close(fd);
        return give_up(format!("the H.264 encoder did not start ({out_w}x{out_h}, {bitrate} bit/s)"));
    }
    let audio_track = match shared.audio.as_ref() {
        Some(a) if sound_ok && a.state.load(Ordering::Acquire) == 1 => Some(OpusTrack { pre_skip: a.pre_skip.load(Ordering::Acquire) as u16, channels: 2 }),
        Some(_) => {
            println("screenrec: note: the Opus encoder did not start, recording without sound");
            None
        }
        None => None,
    };
    let Some(mut mp4) = Mp4::new(FileSink(fd), out_w, out_h, audio_track) else {
        stop_all();
        close(fd);
        return give_up(format!("cannot write {path}"));
    };
    println(&format!(
        "screenrec: recording {w}x{h} at {x},{y} -> {out_w}x{out_h} {fps} fps, MP4 H.264 {} kbit/s ({}), {threads} encoder thread(s), {converters} converter(s), {nslots} slots, sound {}, into {path}",
        bitrate / 1000,
        super::PRESET_NAMES[preset],
        if audio_track.is_some() { format!("Opus {} kbit/s", SOUND_BITRATE[preset] / 1000) } else { String::from("off") }
    ));

    // The mirror fills at the next desktop frame.
    let fill_deadline = monotonic_ms() + 700;
    while cap_status().seq <= 1 && monotonic_ms() < fill_deadline {
        sleep_ms(10);
    }

    let mut queue: VecDeque<usize> = VecDeque::new();
    let (mut unique, mut same, mut late, mut busy, mut written) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut have_seq = 0u64;
    let mut grab_ns = 0u64;
    let mut t0_ns: Option<u64> = None;
    let t0 = monotonic_ms();
    let mut k = 0u64;
    let mut next_status = t0;
    let mut stop_reason = "asked";
    let mut order = 0u64;
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
        if mp4.failed() {
            stop_reason = "write failed (disk full?)";
            break;
        }
        if shared.h264_state.load(Ordering::Acquire) == 2 {
            stop_reason = "the encoder failed";
            break;
        }
        let due = t0 + k * 1000 / u64::from(fps);
        if now < due {
            write_video(&mut mp4, &mut queue, &mut written, false);
            write_audio(&mut mp4);
            let wait = (due - monotonic_ms().min(due)).min(4);
            if wait > 0 {
                user_progs::nanosleep(0, wait as u32 * 1_000_000);
            }
            continue;
        }
        k += 1;
        if now >= t0 + k * 1000 / u64::from(fps) && k > 1 {
            late += 1;
            continue;
        }
        let Some(index) = shared.slots.iter().position(|slot| slot.state.load(Ordering::Acquire) == FREE) else {
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
            same += 1;
            continue;
        }
        have_seq = seq as u64;
        let base = *t0_ns.get_or_insert_with(|| {
            if let Some(audio) = shared.audio.as_ref() {
                audio.start.store((u128::from(g0) * 48_000 / 1_000_000_000) as u64, Ordering::Release);
            }
            g0
        });
        slot.time_us.store(g0.saturating_sub(base) / 1000, Ordering::Release);
        slot.order.store(order, Ordering::Release);
        order += 1;
        slot.state.store(FILLED, Ordering::Release);
        bump(&shared.jobs);
        queue.push_back(index);
        unique += 1;
    }
    let end_ns = now_ns();
    let elapsed_ms = monotonic_ms().saturating_sub(t0).max(1);
    let end_us = t0_ns.map_or(0, |base| end_ns.saturating_sub(base) / 1000);
    // The rest of the pictures, then the sound up to the same moment.
    write_video(&mut mp4, &mut queue, &mut written, true);
    if let Some(audio) = shared.audio.as_ref() {
        let start = audio.start.load(Ordering::Acquire);
        let end = if start == u64::MAX { 0 } else { start + end_us * 48 / 1000 };
        audio.end.store(end, Ordering::Release);
        let deadline = monotonic_ms() + 5000;
        while sound_ok && audio.state.load(Ordering::Acquire) == 1 && monotonic_ms() < deadline {
            write_audio(&mut mp4);
            sleep_ms(5);
        }
    }
    write_audio(&mut mp4);
    stop_all();
    let samples = mp4.video_samples();
    let packets = mp4.audio_packets();
    let (sink, ok) = mp4.finish(end_us);
    let fd = sink.0;
    let bytes = user_progs::file_size(fd).max(0) as u64;
    close(fd);
    let per = |ns: &AtomicU64, n: &AtomicU64| ns.load(Ordering::Relaxed) / n.load(Ordering::Relaxed).max(1) / 100_000;
    let encode_x10 = per(&shared.encode_ns, &shared.encoded);
    let convert_x10 = per(&shared.convert_ns, &shared.converted);
    println(&format!(
        "screenrec: stopped ({stop_reason}) after {}.{} s: {samples} pictures = {}.{} fps of new frames, {unique} new, {same} unchanged, {late} late, {busy} encoder busy, {} failed",
        elapsed_ms / 1000,
        elapsed_ms % 1000 / 100,
        samples as u64 * 1000 / elapsed_ms,
        samples as u64 * 10_000 / elapsed_ms % 10,
        shared.failed_frames.load(Ordering::Relaxed),
    ));
    println(&format!(
        "screenrec: encode {}.{} ms/frame H.264 on {threads} thread(s), convert {}.{} ms/frame, grab {} us/frame, {} kbit/s video+sound, {bytes} bytes",
        encode_x10 / 10,
        encode_x10 % 10,
        convert_x10 / 10,
        convert_x10 % 10,
        grab_ns / unique.max(1) / 1000,
        bytes * 8 / elapsed_ms,
    ));
    if let Some(audio) = shared.audio.as_ref() {
        println(&format!(
            "screenrec: sound {packets} Opus packets ({} ms), peak {}, {} frames lost, {} packets dropped",
            packets as u64 * 20,
            audio.peak.load(Ordering::Relaxed),
            audio.lost.load(Ordering::Relaxed),
            audio.dropped.load(Ordering::Relaxed)
        ));
    }
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
    0
}

/// Check a recorded MP4 (`--probe`): boxes, tables, samples inside `mdat`,
/// NAL lengths, an IDR first, Opus packets of 20 ms.
pub fn probe_fd(fd: i64) -> Result<String, String> {
    let size = user_progs::file_size(fd).max(0) as u64;
    let read_at = |offset: u64, buf: &mut [u8]| super::read_exact_at(fd, offset, buf);
    let mut at = 0u64;
    let (mut moov, mut mdat, mut ftyp) = (None, None, false);
    let mut head = [0u8; 16];
    while at + 8 <= size {
        if !read_at(at, &mut head[..8]) {
            return Err(format!("short read at {at}"));
        }
        let mut len = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let tag = [head[4], head[5], head[6], head[7]];
        let mut header = 8;
        if len == 1 {
            if !read_at(at + 8, &mut head[8..16]) {
                return Err(String::from("short largesize"));
            }
            len = u64::from_be_bytes([head[8], head[9], head[10], head[11], head[12], head[13], head[14], head[15]]);
            header = 16;
        }
        if len < header || at + len > size {
            return Err(format!("box {:?} at {at} runs past the end ({len} bytes, file {size})", core::str::from_utf8(&tag).unwrap_or("?")));
        }
        match &tag {
            b"ftyp" => ftyp = true,
            b"mdat" => mdat = Some((at + header, at + len)),
            b"moov" => {
                let mut body = alloc::vec![0u8; (len - header) as usize];
                if !read_at(at + header, &mut body) {
                    return Err(String::from("cannot read moov"));
                }
                moov = Some(body);
            }
            _ => {}
        }
        at += len;
    }
    if !ftyp {
        return Err(String::from("no ftyp"));
    }
    let (Some(moov), Some(mdat)) = (moov, mdat) else {
        return Err(String::from("no moov or mdat (recording not finished?)"));
    };
    let p = screencast::mp4::probe(&moov, mdat, read_at).map_err(String::from)?;
    let sound = if p.audio_packets > 0 {
        format!(", Opus {} packets = {}.{} s, {} B/packet", p.audio_packets, p.audio_ms / 1000, p.audio_ms % 1000 / 100, p.audio_bytes / u64::from(p.audio_packets))
    } else {
        String::from(", no sound")
    };
    Ok(format!(
        "mp4 h264 {}x{} profile {} level {}, {} pictures ({} key) = {}.{} s{sound}, {size} bytes",
        p.width,
        p.height,
        p.profile,
        p.level,
        p.video_samples,
        p.video_keys,
        p.video_ms / 1000,
        p.video_ms % 1000 / 100
    ))
}

/// `--loopback-test N`: listen to the system sound for N seconds and say
/// what was heard (level and the pitch by zero crossings of the left
/// channel) — checks the kernel tap without a file.
pub fn loopback_test(seconds: u64) -> i64 {
    let mut request = SoundLoop { from: u64::MAX, ..SoundLoop::default() };
    let mut buf = alloc::vec![0u8; 48_000 * 4 / 10];
    if user_progs::sound_loop_read(&mut request, &mut buf[..0]) < 0 {
        println("screenrec: FAILED loopback read");
        return 1;
    }
    let mut next = request.now;
    let end = next + seconds * 48_000;
    let (mut frames, mut sum, mut crossings, mut loud_frames) = (0u64, 0f64, 0u64, 0u64);
    let mut last = 0i16;
    // First and last loud frame: the tone lasts between them (quiet samples
    // near each zero crossing are part of it too).
    let (mut first_loud, mut last_loud) = (None, 0u64);
    while next < end {
        let want = ((end - next) as usize).min(buf.len() / 4);
        request.from = next;
        let got = user_progs::sound_loop_read(&mut request, &mut buf[..want * 4]);
        if got < 0 {
            println("screenrec: FAILED loopback read");
            return 1;
        }
        next = request.from + got as u64;
        for i in 0..got as usize {
            let s = i16::from_le_bytes([buf[i * 4], buf[i * 4 + 1]]);
            sum += f64::from(s) * f64::from(s);
            if s.unsigned_abs() > 300 {
                loud_frames += 1;
                first_loud.get_or_insert(frames + i as u64);
                last_loud = frames + i as u64;
                if (last < 0) != (s < 0) {
                    crossings += 1;
                }
                last = s;
            }
        }
        frames += got as u64;
        if got == 0 {
            sleep_ms(20);
        }
    }
    let rms = if frames > 0 { libm::sqrt(sum / frames as f64) } else { 0.0 };
    // Pitch rounded to 10 Hz: zero crossings of a short tone are off by one
    // or two.
    let span = first_loud.map_or(0, |first| last_loud - first + 1);
    let hz = if span > 0 { (crossings * 48_000 / 2 / span + 5) / 10 * 10 } else { 0 };
    println(&format!("screenrec: loopback heard {frames} frames, {loud_frames} loud, rms {}, ~{hz} Hz", rms as u64));
    0
}
