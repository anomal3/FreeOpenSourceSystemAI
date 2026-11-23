//! System sound loopback (23.11): what the card plays, for recording.
//!
//! There is no mixer: every program's samples go to the card one chunk after
//! another (`sound::feed`), the card plays them in that order. The tap sits
//! at the same place and keeps a copy on one timeline of the system clock —
//! frame `F` is the sample played at `F / 48 000` seconds of uptime (the
//! `CLOCK_MONOTONIC` of programs), so a recorder lines sound up with picture
//! frames by their timestamps alone.
//!
//! A chunk is placed at `max(now, end of the previous chunk)`: the first
//! chunk after silence starts now, the following ones queue behind it, as
//! they queue in the card. Frames nobody wrote are silence. Readers (the
//! native `SOUND_LOOP_READ` for `/bin/screenrec` and the ALSA capture device
//! `pcmC0D0c` — «Стерео микшер» — for Linux programs like OBS) each keep
//! their own position and may only read up to now: later frames can still
//! change.
//!
//! The tap costs nothing while nobody records: it is armed by a read and
//! disarms itself five seconds after the last one. The ring (2.7 s, 512 KiB)
//! is allocated on the first read and kept.
//!
//! Samples are taken before the volume of the tray: the recording does not
//! get quieter when the speakers do (Windows' loopback works the same way).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::sync::Mutex;

/// Frames in the ring: a power of two, 2.73 s at 48 kHz.
pub const RING_FRAMES: u64 = 1 << 17;
/// Frames per second.
pub const RATE: u64 = super::SAMPLE_RATE as u64;
/// The tap stays armed this long after the last read.
const ARMED_MS: u64 = 5_000;

struct Ring {
    /// Stereo frames, both 16-bit samples in one word (little-endian bytes).
    frames: Vec<u32>,
    /// One past the last frame written (absolute frame number).
    end: u64,
}

static RING: Mutex<Option<Ring>> = Mutex::new(None);
/// Uptime in ms until which the tap is armed (0 — never armed).
static ARMED_UNTIL: AtomicU64 = AtomicU64::new(0);

/// The frame playing now.
#[must_use]
pub fn now_frame() -> u64 {
    (u128::from(crate::time::uptime_ns()) * u128::from(RATE) / 1_000_000_000) as u64
}

/// Is somebody recording?
#[must_use]
pub fn armed() -> bool {
    crate::time::uptime_ms() < ARMED_UNTIL.load(Ordering::Relaxed)
}

/// Keep a copy of samples handed to the card (16-bit stereo, any length; a
/// torn frame at the end is ignored). Cheap when nobody records.
pub fn tap(bytes: &[u8]) {
    if !armed() {
        return;
    }
    let count = (bytes.len() / 4) as u64;
    if count == 0 {
        return;
    }
    let now = now_frame();
    let mut guard = RING.lock();
    let Some(ring) = guard.as_mut() else { return };
    let start = now.max(ring.end);
    if start > ring.end {
        // Silence between the previous chunk and this one (at most a ring).
        let from = ring.end.max(start.saturating_sub(RING_FRAMES));
        for frame in from..start {
            ring.frames[(frame % RING_FRAMES) as usize] = 0;
        }
    }
    for (i, pair) in bytes.chunks_exact(4).enumerate() {
        let frame = start + i as u64;
        ring.frames[(frame % RING_FRAMES) as usize] = u32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
    }
    ring.end = start + count;
}

/// Arm the tap (allocating the ring the first time). `false` — no memory.
pub fn arm() -> bool {
    ARMED_UNTIL.store(crate::time::uptime_ms() + ARMED_MS, Ordering::Relaxed);
    let mut guard = RING.lock();
    if guard.is_none() {
        let mut frames = Vec::new();
        if frames.try_reserve_exact(RING_FRAMES as usize).is_err() {
            return false;
        }
        frames.resize(RING_FRAMES as usize, 0);
        *guard = Some(Ring { frames, end: now_frame() });
    }
    true
}

/// Read frames from `from` on into `out` (4 bytes a frame), never past now.
///
/// A position older than the ring moves up to its oldest frame. Answer —
/// the first frame actually read and how many were read; `now` — the frame
/// playing now. Arms the tap.
pub fn read(from: u64, out: &mut [u8]) -> (u64, usize) {
    if !arm() {
        return (from, 0);
    }
    let now = now_frame();
    // Keep a quarter of the ring away from the writer's wrap.
    let oldest = now.saturating_sub(RING_FRAMES * 3 / 4);
    let first = from.max(oldest).min(now);
    let count = ((now - first) as usize).min(out.len() / 4);
    let guard = RING.lock();
    let Some(ring) = guard.as_ref() else { return (first, 0) };
    // Frames at or after `end` were not written (yet): silence. Frames more
    // than a ring before `end` were overwritten: silence too (cannot happen
    // with `oldest` above, but costs nothing to say).
    for (i, slot) in out.chunks_exact_mut(4).take(count).enumerate() {
        let frame = first + i as u64;
        let value = if frame < ring.end && frame + RING_FRAMES > ring.end { ring.frames[(frame % RING_FRAMES) as usize] } else { 0 };
        slot.copy_from_slice(&value.to_le_bytes());
    }
    (first, count)
}
