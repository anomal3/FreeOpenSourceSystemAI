//! Wait queues by address (G2, 21.11.2026): futex words of programs and
//! kernel sleeping locks (`sync::Mutex`, pipes).
//!
//! # Why
//!
//! Until now every futex wait and wake went through the one scheduler lock,
//! and every wake walked all task slots under it (hw-log13, D2R on 16 CPUs:
//! 4-9 M waits for that lock per 30 s, `block_on_futex` and `wake_lock` the
//! hottest sites). Like Linux `futex_hash`, waiters now sit in hashed
//! buckets, each with its own small lock; a wake takes only the bucket of its
//! key and touches the scheduler lock once, to make the found tasks ready.
//!
//! # The protocol (no lost wake-ups)
//!
//! Waiter: (1) clear its [`WOKEN`] cell, (2) put an entry per key into the
//! bucket (under the bucket lock), (3) check the condition (`ready`) with no
//! lock held, (4) block in the scheduler unless [`WOKEN`] or the task's wake
//! sequence already changed (checked under the scheduler lock), (5) take its
//! remaining entries out and read [`WOKEN`].
//!
//! Waker: changes the word first, then takes the bucket lock, removes the
//! matching entries and stores "woken by key N" into their [`WOKEN`] cells,
//! drops the bucket lock and makes those tasks ready under the scheduler
//! lock. Either the waker's bucket section comes after the waiter's (2) and
//! finds the entry, or it came before - then the word was changed before the
//! waiter's (3) and the waiter does not sleep. A waker that found the entry
//! before the waiter reached (4) leaves [`WOKEN`] set, and (4) does not block.
//!
//! A waker can be slow between "removed the entry" and "made the task
//! ready": by then the waiter may be in its next wait and get a spurious
//! wake-up. Every wait here is a loop that re-checks its condition, so that
//! costs one round (the same as `sched::wake`, which wakes any wait).

use core::sync::atomic::{AtomicU32, Ordering};

use alloc::vec::Vec;

use super::MAX_TASKS;
use crate::sync::SpinLock;

/// What a key means: futex keys and kernel lock addresses live in
/// different spaces and must never match each other.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Kind {
    Futex = 0,
    Lock = 1,
}

#[derive(Clone, Copy)]
struct Entry {
    key: usize,
    slot: u16,
    /// Index of the key in a `futex_waitv` set; 0 for single waits.
    index: u16,
    kind: u8,
}

/// Buckets; a power of two (the hash takes the top bits).
const BUCKETS: usize = 256;

static TABLE: [SpinLock<Vec<Entry>>; BUCKETS] = [const { SpinLock::new(Vec::new()) }; BUCKETS];

/// "Woken by key N" per task slot: N + 1, 0 = not woken by a key.
static WOKEN: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];

/// Wake-ups that went through the buckets (`perf-diag`).
pub(super) static WAKES: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn bucket(kind: Kind, key: usize) -> &'static SpinLock<Vec<Entry>> {
    let mixed = (key as u64 ^ ((kind as u64) << 63)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    &TABLE[(mixed >> (64 - BUCKETS.trailing_zeros())) as usize]
}

/// Step (1): forget an old "woken" mark.
pub(super) fn begin(slot: usize) {
    WOKEN[slot].store(0, Ordering::SeqCst);
}

/// Step (2): queue the waiter. `false` - no memory for the entry.
pub(super) fn enqueue(kind: Kind, key: usize, slot: usize, index: usize) -> bool {
    let mut entries = bucket(kind, key).lock();
    if entries.try_reserve(1).is_err() {
        return false;
    }
    entries.push(Entry { key, slot: slot as u16, index: index as u16, kind: kind as u8 });
    true
}

/// Step (5): take the waiter's entry for `key` out (if a waker did not).
pub(super) fn dequeue(kind: Kind, key: usize, slot: usize) {
    let mut entries = bucket(kind, key).lock();
    entries.retain(|entry| !(usize::from(entry.slot) == slot && entry.kind == kind as u8 && entry.key == key));
}

/// Index of the key that woke the waiter in `slot`, if a key did.
pub(super) fn woken(slot: usize) -> Option<usize> {
    match WOKEN[slot].load(Ordering::SeqCst) {
        0 => None,
        n => Some(n as usize - 1),
    }
}

/// Take up to `count` waiters on `key` (0 = all) out of their bucket, mark
/// them woken and hand their slots to `ready` in batches (outside the bucket
/// lock). Returns how many were taken. Oldest waiters first.
pub(super) fn wake(kind: Kind, key: usize, count: usize, mut ready: impl FnMut(&[u16])) -> usize {
    let mut total = 0usize;
    loop {
        let mut batch = [0u16; 32];
        let mut taken = 0usize;
        let mut more = false;
        {
            let mut entries = bucket(kind, key).lock();
            let mut at = 0;
            while at < entries.len() {
                let entry = entries[at];
                if entry.key != key || entry.kind != kind as u8 {
                    at += 1;
                    continue;
                }
                if count != 0 && total + taken >= count {
                    break;
                }
                if taken == batch.len() {
                    more = true;
                    break;
                }
                entries.remove(at);
                let _ = WOKEN[usize::from(entry.slot)].compare_exchange(
                    0,
                    u32::from(entry.index) + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                );
                batch[taken] = entry.slot;
                taken += 1;
            }
        }
        total += taken;
        if taken > 0 {
            ready(&batch[..taken]);
        }
        if !more {
            break;
        }
    }
    if total > 0 {
        WAKES.fetch_add(total as u64, Ordering::Relaxed);
    }
    total
}
