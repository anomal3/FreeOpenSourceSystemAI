//! Desktop frame gaps (24.11.2026, hw-log17 item 6: "the whole UI freezes for
//! a moment when a game starts", and nothing in the log measured it).
//!
//! Every piece of desktop work goes through `ui::with_desktop`, which takes
//! the compositor out of its slot and puts it back. Two things are logged:
//!
//! * a **hold** longer than [`GAP_MS`]: who held the desktop (call site of
//!   `with_desktop`, task) - the frame could not be composed meanwhile;
//! * a **gap** longer than [`GAP_MS`] between two holds while the desktop
//!   wanted frames (an animation runs - the launch splash, a zoom - or an
//!   input event waits): nobody took the desktop, so the task that drives it
//!   was busy elsewhere or did not get a CPU. The line names the last
//!   [`phase`] that task announced (work done outside the desktop, like the
//!   launch icons) and the run queue at that moment.
//!
//! At most [`BUDGET`] lines per 10 s, so a stuck desktop does not flood the
//! log. `framegap off` (shell) - silent.

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

/// Longer than this is a visible stutter (three frames at 60 Hz).
pub const GAP_MS: u64 = 50;
/// Lines per 10 s window.
const BUDGET: u64 = 12;

static ON: AtomicBool = AtomicBool::new(true);
static LAST_RETURN_NS: AtomicU64 = AtomicU64::new(0);
static WINDOW_START_MS: AtomicU64 = AtomicU64::new(0);
static WINDOW_LINES: AtomicU64 = AtomicU64::new(0);
/// Gaps and long holds since boot (perf-diag style counters for `framegap`).
static GAPS: AtomicU64 = AtomicU64::new(0);
static HOLDS: AtomicU64 = AtomicU64::new(0);
static WORST_GAP_NS: AtomicU64 = AtomicU64::new(0);

/// What the desktop-driving task does outside the desktop right now, and
/// since when (ns). A `&'static str` behind an atomic pointer.
static PHASE: AtomicPtr<&'static str> = AtomicPtr::new(core::ptr::null_mut());
static PHASE_SINCE_NS: AtomicU64 = AtomicU64::new(0);
/// The longest phase since the last gap: name and ns.
static PHASE_WORST: AtomicPtr<&'static str> = AtomicPtr::new(core::ptr::null_mut());
static PHASE_WORST_NS: AtomicU64 = AtomicU64::new(0);

/// The slowest named step inside the current desktop hold (see [`step`]).
static STEP_WORST: AtomicPtr<&'static str> = AtomicPtr::new(core::ptr::null_mut());
static STEP_WORST_NS: AtomicU64 = AtomicU64::new(0);

/// Run one named step of desktop work and remember it if it is the slowest
/// of this hold: a long hold names it (`ui::tick` wraps its steps).
pub fn step<R>(name: &'static &'static str, work: impl FnOnce() -> R) -> R {
    let started = crate::time::uptime_ns();
    let result = work();
    let spent = crate::time::uptime_ns().saturating_sub(started);
    if STEP_WORST_NS.fetch_max(spent, Ordering::Relaxed) < spent {
        STEP_WORST.store(core::ptr::from_ref(name).cast_mut(), Ordering::Relaxed);
    }
    result
}

/// `framegap on|off`.
pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
}

#[must_use]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// `(gaps, long holds, worst gap ns)` since boot.
#[must_use]
pub fn stats() -> (u64, u64, u64) {
    (GAPS.load(Ordering::Relaxed), HOLDS.load(Ordering::Relaxed), WORST_GAP_NS.load(Ordering::Relaxed))
}

/// The driving task starts work outside the desktop (`name` must be a
/// `static` string: `phase(&"launch icons")`); [`phase_end`] closes it.
pub fn phase(name: &'static &'static str) {
    PHASE_SINCE_NS.store(crate::time::uptime_ns(), Ordering::Relaxed);
    PHASE.store(core::ptr::from_ref(name).cast_mut(), Ordering::Relaxed);
}

/// The work of the last [`phase`] is done.
pub fn phase_end() {
    let name = PHASE.swap(core::ptr::null_mut(), Ordering::Relaxed);
    if name.is_null() {
        return;
    }
    let spent = crate::time::uptime_ns().saturating_sub(PHASE_SINCE_NS.load(Ordering::Relaxed));
    if PHASE_WORST_NS.fetch_max(spent, Ordering::Relaxed) < spent {
        PHASE_WORST.store(name, Ordering::Relaxed);
    }
}

fn phase_name(ptr: *mut &'static str) -> &'static str {
    if ptr.is_null() {
        "-"
    } else {
        // SAFETY: only `&'static &'static str` are stored (see `phase`).
        unsafe { *ptr }
    }
}

/// May one more line go to the log in this 10 s window?
fn budget() -> bool {
    let now = crate::time::uptime_ms();
    let start = WINDOW_START_MS.load(Ordering::Relaxed);
    if now.saturating_sub(start) >= 10_000 {
        WINDOW_START_MS.store(now, Ordering::Relaxed);
        WINDOW_LINES.store(1, Ordering::Relaxed);
        return true;
    }
    WINDOW_LINES.fetch_add(1, Ordering::Relaxed) < BUDGET
}

/// `ui::with_desktop` gives the desktop back: it was taken at `taken_ns` by
/// the call at `at`.
pub fn desktop_returned(taken_ns: u64, now_ns: u64, at: &'static core::panic::Location<'static>) {
    let prev = LAST_RETURN_NS.swap(now_ns, Ordering::Relaxed);
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    // The log line itself may draw into the terminal window through the
    // desktop and come back here: not twice.
    if IN_LOG.swap(true, Ordering::Acquire) {
        return;
    }
    check(prev, taken_ns, now_ns, at);
    IN_LOG.store(false, Ordering::Release);
}

static IN_LOG: AtomicBool = AtomicBool::new(false);

fn check(prev: u64, taken_ns: u64, now_ns: u64, at: &'static core::panic::Location<'static>) {
    let limit = GAP_MS * 1_000_000;
    let held = now_ns.saturating_sub(taken_ns);
    let step = phase_name(STEP_WORST.swap(core::ptr::null_mut(), Ordering::Relaxed));
    let step_ns = STEP_WORST_NS.swap(0, Ordering::Relaxed);
    if held > limit {
        HOLDS.fetch_add(1, Ordering::Relaxed);
        if budget() {
            crate::kprintln!(
                "  desktop     : held {} ms by '{}' at {}:{} (no frame meanwhile), slowest step '{step}' {} ms",
                held / 1_000_000,
                crate::sched::current_name(),
                at.file(),
                at.line(),
                step_ns / 1_000_000
            );
        }
    }
    // A gap: between the previous return and this take nobody had the
    // desktop. Only a gap when frames were wanted.
    let idle = taken_ns.saturating_sub(prev);
    if prev == 0 || idle <= limit || !(super::animating() || super::input_waiting()) {
        return;
    }
    GAPS.fetch_add(1, Ordering::Relaxed);
    WORST_GAP_NS.fetch_max(idle, Ordering::Relaxed);
    if !budget() {
        return;
    }
    let now_phase = phase_name(PHASE.load(Ordering::Relaxed));
    let worst = phase_name(PHASE_WORST.swap(core::ptr::null_mut(), Ordering::Relaxed));
    let worst_ns = PHASE_WORST_NS.swap(0, Ordering::Relaxed);
    let (ready, running) = crate::sched::queue_facts();
    crate::kprintln!(
        "  desktop     : frame gap {} ms ({}; taken again by '{}' at {}:{}); driver busy with '{now_phase}', longest outside work '{worst}' {} ms; run queue {ready} ready, {running} running",
        idle / 1_000_000,
        if super::input_waiting() { "input waiting" } else { "animating" },
        crate::sched::current_name(),
        at.file(),
        at.line(),
        worst_ns / 1_000_000
    );
}
