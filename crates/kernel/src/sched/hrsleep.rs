//! Sub-tick sleeps (hw-log15, 22.11.2026): sleep until a deadline in
//! nanoseconds, or until an event source fires, whichever comes first.
//!
//! # Why
//!
//! Every timed wait of the scheduler is in milliseconds and ends at the next
//! timer tick after its deadline: a 1 ms sleep lasts 1-2 ms. hw-log15 (D2R on
//! an RTX 3060): DRM fence waits that outlived their 200 us poll slept 1.85 ms
//! each, 5-10 thousand times per 30 s; a fence that needed 0.3 ms of GPU time
//! cost two ticks. `nanosleep(1 ms)` of Wine (`Sleep(1)`) lasted 2-3 ms.
//!
//! # How
//!
//! A sleeper writes its deadline into its slot's cell ([`FINE_AT`]), arms a
//! one-shot of the local APIC timer for that moment (`arch::arm_short_timer`:
//! the periodic tick is restarted with a shorter count and put back by the
//! shot's interrupt) and blocks on a key nobody else uses. Whoever sees the
//! deadline pass - the shot's interrupt, any CPU's regular tick, an idle CPU
//! in its poll loop - takes the cell (compare-and-swap to 0) and wakes the
//! task like `sched::wake` does (the wake sequence first, so a sleeper that
//! is still on its way into the scheduler does not block).
//!
//! Event sources ([`SRC_XHCI`]): an interrupt handler calls [`signal`], which
//! moves the source's generation and wakes every fine sleeper that waits for
//! that source. A sleeper reads the generation before it checks its
//! condition and does not sleep if it moved (see [`generation`]).
//!
//! Without a one-shot timer (AArch64, or the APIC timer is off) the regular
//! tick still expires the cells: the wait then ends within a tick of its
//! deadline instead of one to two ticks after it. The millisecond deadline
//! given to the scheduler (two ticks after the fine one) is the backstop if
//! everything else is missed.
//!
//! `hrsleep off` (kernel shell) turns the whole thing off: [`enabled`] callers
//! go back to their millisecond sleeps.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::{
    MAX_TASKS, SCHED, SLOT_ID, SlotBits, TaskId, TaskState, WAKE_SEQ, Wait, defer_wake, kick_woken, waitq, wq_block,
};
use crate::smp::MAX_CPUS;

/// The xHCI controllers raised an interrupt (USB disk completions).
pub const SRC_XHCI: u32 = 1 << 0;
const SOURCES: usize = 1;

/// Fine sleeps on (`hrsleep on|off`).
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Deadline of the fine sleep of a slot, ns of uptime; 0 - not sleeping.
static FINE_AT: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// Task id that owns the cell (a reused slot must not be woken by mistake).
static FINE_ID: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
/// Event sources the sleeper also wakes on.
static FINE_SRC: [AtomicU32; MAX_TASKS] = [const { AtomicU32::new(0) }; MAX_TASKS];
/// Slots with a live cell (a superset).
static FINE: SlotBits = SlotBits::new();
/// Earliest deadline among the cells (a hint for the idle poll; may be
/// stale low, never stale high for long: every expiry pass rewrites it).
static NEXT_FINE: AtomicU64 = AtomicU64::new(u64::MAX);
/// Generations of the event sources.
static GENERATION: [AtomicU64; SOURCES] = [const { AtomicU64::new(0) }; SOURCES];
/// When the regular tick of each CPU last ran its work, ns (a short shot
/// that comes a tick or more after it runs the tick's work too).
static LAST_TICK_NS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

/// Counters for perf-diag: fine sleeps, ended by their deadline (the waker
/// expired them), ended by an event, one-shots armed, their interrupts, and
/// the sum of lateness (wake ns - deadline ns) of deadline wakes.
static SLEEPS: AtomicU64 = AtomicU64::new(0);
static BY_DEADLINE: AtomicU64 = AtomicU64::new(0);
static BY_EVENT: AtomicU64 = AtomicU64::new(0);
static SHOTS: AtomicU64 = AtomicU64::new(0);
static SHOT_IRQS: AtomicU64 = AtomicU64::new(0);
static LATE_NS: AtomicU64 = AtomicU64::new(0);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Are fine sleeps on? Off - callers use their millisecond sleeps.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed) && super::RUNNING.load(Ordering::Relaxed)
}

/// `(sleeps, ended by deadline, ended by event, one-shots armed, one-shot
/// interrupts, average lateness of deadline wakes in ns)` since boot.
#[must_use]
pub fn stats() -> (u64, u64, u64, u64, u64, u64) {
    let by_deadline = BY_DEADLINE.load(Ordering::Relaxed);
    (
        SLEEPS.load(Ordering::Relaxed),
        by_deadline,
        BY_EVENT.load(Ordering::Relaxed),
        SHOTS.load(Ordering::Relaxed),
        SHOT_IRQS.load(Ordering::Relaxed),
        LATE_NS.load(Ordering::Relaxed) / by_deadline.max(1),
    )
}

/// Generation of the sources in `sources` (sum): read it before checking
/// the condition, give it to [`sleep_until_ns`].
#[must_use]
pub fn generation(sources: u32) -> u64 {
    (0..SOURCES)
        .filter(|bit| sources & (1 << bit) != 0)
        .map(|bit| GENERATION[bit].load(Ordering::Acquire))
        .fold(0u64, u64::wrapping_add)
}

/// Sleep until `deadline_ns` (uptime), until one of `sources` fires after
/// `seen` (its [`generation`] read before the caller checked its condition),
/// or until any wake-up. `ready` runs after the sleeper is queued, with no
/// lock held (see `waitq`): `true` - do not sleep. Returns at once if fine
/// sleeps are off (the caller then sleeps its old way).
pub fn sleep_until_ns(deadline_ns: u64, sources: u32, seen: u64, ready: impl FnOnce() -> bool) {
    if !enabled() {
        return;
    }
    let slot = super::current_slot();
    let id = SLOT_ID[slot].load(Ordering::Relaxed);
    if id == 0 || super::is_idle_slot(slot) {
        return;
    }
    let now = crate::time::uptime_ns();
    if deadline_ns <= now {
        return;
    }
    FINE_ID[slot].store(id, Ordering::Relaxed);
    FINE_SRC[slot].store(sources, Ordering::Relaxed);
    FINE_AT[slot].store(deadline_ns, Ordering::SeqCst);
    FINE.set(slot);
    NEXT_FINE.fetch_min(deadline_ns, Ordering::SeqCst);
    // The local one-shot; another CPU's tick or idle poll may come first.
    if crate::arch::interrupts::without_interrupts(|| crate::arch::arm_short_timer(deadline_ns - now)) {
        SHOTS.fetch_add(1, Ordering::Relaxed);
    }
    SLEEPS.fetch_add(1, Ordering::Relaxed);
    let key = core::ptr::from_ref(&FINE_AT[slot]).addr();
    // Backstop in scheduler terms: two ticks after the fine deadline.
    let until_ms = deadline_ns / 1_000_000 + 2;
    let _ = wq_block(waitq::Kind::Futex, &[key], Wait::FutexUntil(key, until_ms), Some(until_ms), || {
        FINE_AT[slot].load(Ordering::SeqCst) == 0
            || crate::time::uptime_ns() >= deadline_ns
            || (sources != 0 && generation(sources) != seen)
            || ready()
    });
    // Whoever did not take the cell: take it back.
    FINE_AT[slot].store(0, Ordering::SeqCst);
    FINE.clear(slot);
}

/// `sched::block_on_futex` with a deadline in nanoseconds (hw-log15: Wine's
/// timed waits through ntsync and futex rounded up to whole ticks). Sleeps
/// on `key` - a wake on it ends the wait - until `deadline_ns`. `ready` as
/// in `block_on_futex` (no lock held, after queueing). Returns `true` when a
/// wake on `key` ended it. Fine sleeps off: the old millisecond way.
pub fn block_on_futex_ns(key: usize, deadline_ns: u64, ready: impl FnOnce() -> bool) -> bool {
    let until_ms = deadline_ns.div_ceil(1_000_000);
    let slot = super::current_slot();
    let id = SLOT_ID[slot].load(Ordering::Relaxed);
    let now = crate::time::uptime_ns();
    if !enabled() || id == 0 || super::is_idle_slot(slot) || deadline_ns <= now {
        return super::block_on_futex(key, Some(until_ms.max(crate::time::uptime_ms() + 1)), ready);
    }
    FINE_ID[slot].store(id, Ordering::Relaxed);
    FINE_SRC[slot].store(0, Ordering::Relaxed);
    FINE_AT[slot].store(deadline_ns, Ordering::SeqCst);
    FINE.set(slot);
    NEXT_FINE.fetch_min(deadline_ns, Ordering::SeqCst);
    if crate::arch::interrupts::without_interrupts(|| crate::arch::arm_short_timer(deadline_ns - now)) {
        SHOTS.fetch_add(1, Ordering::Relaxed);
    }
    SLEEPS.fetch_add(1, Ordering::Relaxed);
    let until_ms = until_ms + 2;
    let end = wq_block(waitq::Kind::Futex, &[key], Wait::FutexUntil(key, until_ms), Some(until_ms), || {
        FINE_AT[slot].load(Ordering::SeqCst) == 0 || crate::time::uptime_ns() >= deadline_ns || ready()
    });
    FINE_AT[slot].store(0, Ordering::SeqCst);
    FINE.clear(slot);
    // A waker may have taken our queue entry just before `ready` said
    // "do not sleep" (the deadline passed): that wake is ours - reporting a
    // timeout would lose it, and a thread waiting without a timeout behind us
    // would never be woken (QEMU proton-hw14 22.11: Wine stood still).
    matches!(end, super::WqEnd::Woke(Some(_))) || waitq::woken(slot).is_some()
}

/// An event source fired (interrupt handler): wake its fine sleepers.
pub fn signal(source: u32) {
    for bit in 0..SOURCES {
        if source & (1 << bit) != 0 {
            GENERATION[bit].fetch_add(1, Ordering::AcqRel);
        }
    }
    if !FINE.any() {
        return;
    }
    let high = super::HIGH.load(Ordering::Acquire).min(MAX_TASKS);
    FINE.scan(0, high, &mut |slot| {
        if FINE_SRC[slot].load(Ordering::Relaxed) & source != 0 {
            let at = FINE_AT[slot].load(Ordering::SeqCst);
            if at != 0 && FINE_AT[slot].compare_exchange(at, 0, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                BY_EVENT.fetch_add(1, Ordering::Relaxed);
                wake_slot(slot);
            }
        }
        false
    });
}

/// Wake the sleepers whose deadline passed; rewrite [`NEXT_FINE`]. Returns
/// the earliest deadline still pending (`u64::MAX` - none). Safe from an
/// interrupt handler.
pub fn expire() -> u64 {
    if !FINE.any() {
        NEXT_FINE.store(u64::MAX, Ordering::Relaxed);
        return u64::MAX;
    }
    let now = crate::time::uptime_ns();
    let mut next = u64::MAX;
    let high = super::HIGH.load(Ordering::Acquire).min(MAX_TASKS);
    FINE.scan(0, high, &mut |slot| {
        let at = FINE_AT[slot].load(Ordering::SeqCst);
        if at == 0 {
            return false;
        }
        if at <= now {
            if FINE_AT[slot].compare_exchange(at, 0, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                BY_DEADLINE.fetch_add(1, Ordering::Relaxed);
                LATE_NS.fetch_add(now - at, Ordering::Relaxed);
                wake_slot(slot);
            }
        } else {
            next = next.min(at);
        }
        false
    });
    NEXT_FINE.store(next, Ordering::Relaxed);
    // A sleeper that registered during the scan lowered the hint before our
    // store: give it back.
    if FINE.any() {
        let high = super::HIGH.load(Ordering::Acquire).min(MAX_TASKS);
        FINE.scan(0, high, &mut |slot| {
            let at = FINE_AT[slot].load(Ordering::SeqCst);
            if at != 0 && at < next {
                next = at;
            }
            false
        });
        NEXT_FINE.fetch_min(next, Ordering::Relaxed);
    }
    next
}

/// Is a fine deadline due (cheap; for the idle loop)?
#[inline]
#[must_use]
pub fn due() -> bool {
    let next = NEXT_FINE.load(Ordering::Relaxed);
    next != u64::MAX && crate::time::uptime_ns() >= next
}

/// The timer interrupt of this CPU. `short` - it was a one-shot armed by a
/// sleeper (the arch layer has already put the periodic count back).
/// Returns whether the regular tick's work (`irq::on_timer_tick`) is due.
pub fn on_timer_interrupt(short: bool) -> bool {
    let cpu = crate::smp::cpu();
    let now = crate::time::uptime_ns();
    let tick = if short {
        SHOT_IRQS.fetch_add(1, Ordering::Relaxed);
        // A tick's work is owed once a millisecond even if one-shots keep
        // restarting the period.
        now.saturating_sub(LAST_TICK_NS[cpu].load(Ordering::Relaxed)) >= 1_000_000
    } else {
        true
    };
    if tick {
        LAST_TICK_NS[cpu].store(now, Ordering::Relaxed);
    }
    let next = expire();
    // Chain the one-shot to the next fine deadline if it comes before the
    // next tick (only after a one-shot: regular ticks of 16 CPUs must not
    // all arm one for the same sleeper).
    if short && next != u64::MAX {
        let now = crate::time::uptime_ns();
        if next > now && next - now < 1_000_000 && crate::arch::arm_short_timer(next - now) {
            SHOTS.fetch_add(1, Ordering::Relaxed);
        }
    }
    tick
}

/// `sched::wake` for a slot whose task id is in its cell.
fn wake_slot(slot: usize) {
    let id = FINE_ID[slot].load(Ordering::Relaxed);
    if id == 0 || SLOT_ID[slot].load(Ordering::Relaxed) != id {
        return;
    }
    WAKE_SEQ[slot].fetch_add(1, Ordering::SeqCst);
    // Never wait for the scheduler lock here (often an interrupt handler):
    // a busy lock - the wake goes to the deferred queue, the next switch
    // anywhere makes the task ready.
    let Some(mut sched) = SCHED.try_lock_once() else {
        defer_wake(TaskId::new(id));
        // An idle CPU takes the deferred wake at once, not at its next tick.
        super::kick_idle(1);
        return;
    };
    let woke = sched.tasks[slot]
        .as_ref()
        .is_some_and(|task| task.id.as_u32() == id && matches!(task.state, TaskState::Blocked(_)));
    if woke {
        sched.set_ready(slot);
    }
    drop(sched);
    if woke {
        kick_woken(1);
    }
}

/// `hrbench` (kernel shell): `count` sleeps of `us` microseconds - fine
/// (as `nanosleep` sleeps now) or the tick way (`nanosleep` before 22.11:
/// rounded up to ms plus a tick). Returns `(average, worst)` length, us.
pub fn sleep_bench(us: u64, count: u32, fine: bool) -> (u64, u64) {
    let (mut sum, mut worst) = (0u64, 0u64);
    for _ in 0..count {
        let started = crate::time::uptime_ns();
        let deadline = started + us * 1000;
        if fine {
            while crate::time::uptime_ns() < deadline && enabled() {
                sleep_until_ns(deadline, 0, 0, || false);
            }
        } else {
            super::sleep_ms(us.div_ceil(1000) + 1);
        }
        let took = crate::time::uptime_ns() - started;
        sum += took;
        worst = worst.max(took);
    }
    (sum / u64::from(count.max(1)) / 1000, worst / 1000)
}
