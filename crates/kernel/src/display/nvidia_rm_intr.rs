//! GPU fence interrupt (hw-log17, 24.11.2026): a channel's fence segment
//! ends with `NON_STALL_INTERRUPT` (`gpfifo::fence_release`), so every
//! finished submission raises the engine's *nonstall* interrupt. Here it
//! wakes the DRM fence waiters (`hrsleep` source [`SRC_GPU`]) instead of
//! them polling in 50-200 us steps.
//!
//! # How (as nouveau on GSP-RM: `r535_gsp_intr_get_table`, `tu102_vfn.c`,
//! `ga100_fifo_nonstall_ctor`, `gp100_pci_msi_rearm`)
//!
//! * GSP-RM tells which interrupt vectors the CPU owns:
//!   `NV2080_CTRL_CMD_INTERNAL_INTR_GET_KERNEL_TABLE` (0x20800a5c) - per
//!   engine `{engineIdx u16, pmcIntrMask, vectorStall, vectorNonStall}`. We
//!   take every *nonstall* vector (all are "something finished"; a wake-up
//!   too many is harmless: the waiter checks its fence and sleeps again).
//! * The CPU interrupt tree of Turing/Ampere at BAR0 `0xb80000` (VFN priv):
//!   `LEAF(i)` 0x1000 (write 1 to clear), `LEAF_EN_SET(i)` 0x1200,
//!   `LEAF_EN_CLEAR(i)` 0x1400, `TOP` 0x1600, `TOP_EN_SET` 0x1608,
//!   `TOP_EN_CLEAR` 0x1610. Vector `v` = leaf `v / 32` bit `v % 32`; a TOP
//!   bit covers two leaves.
//! * Delivery: PCI MSI (one vector; after each interrupt the MSI is re-armed
//!   by writing 0 to `0x088704`), else MSI-X (the first entries of the table
//!   all point at our vector).
//! * The handler: TOP off, clear our pending bits, TOP on, re-arm MSI,
//!   `hrsleep::signal(SRC_GPU)`.
//!
//! # Not tested on hardware - it watches itself
//!
//! [`after_wait`] is told how every sleeping fence wait ended. The state goes
//! `Trial` -> `Live` once interrupts wake waiters; it falls back to the
//! polling way (`fenceirq: fallback ...` in the log) if no interrupt comes
//! within [`TRIAL_MS`] from the first sleeping wait (and 256 sleeps), if more than a quarter of the
//! waits end by the backstop timer although the fence was done (missed
//! interrupts), or on an interrupt storm. `fenceirq off` (kernel shell)
//! turns it off by hand, `fenceirq on` tries again.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::nvidia::Bar0;
use super::nvidia_gsp::Log;
use super::nvidia_rm_info::{say, with_rm};
use crate::sched::hrsleep;
use crate::sync::SpinLock;

pub use crate::sched::hrsleep::SRC_GPU;

const VFN: usize = 0x00b8_0000;
const LEAF: usize = VFN + 0x1000;
const LEAF_EN_SET: usize = VFN + 0x1200;
const LEAF_EN_CLEAR: usize = VFN + 0x1400;
const TOP: usize = VFN + 0x1600;
const TOP_EN_SET: usize = VFN + 0x1608;
const TOP_EN_CLEAR: usize = VFN + 0x1610;
/// `gp100_pci_msi_rearm`: NV_PCFG mirror (0x88000) + 0x704.
const MSI_REARM: usize = 0x0008_8704;
const LEAVES: usize = 8;

/// No interrupt within this long while fence waiters slept - fall back.
pub const TRIAL_MS: u64 = 3000;
/// More interrupts than this in 100 ms - a storm (something we do not clear).
const STORM_PER_100MS: u64 = 50_000;

/// `fenceirq on|off` (kernel shell).
static SWITCH: AtomicBool = AtomicBool::new(true);

/// How fence waits use the interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Mode {
    /// Not set up, switched off or fell back: poll as before.
    Off = 0,
    /// Armed, not proven yet: wait as before but also wake on it.
    Trial = 1,
    /// Interrupts wake the waiters: long backstop sleeps.
    Live = 2,
}

static MODE: AtomicU8 = AtomicU8::new(Mode::Off as u8);
/// BAR0 for the handler (0 - nothing armed: the handler only counts).
static BAR: AtomicUsize = AtomicUsize::new(0);
/// Our nonstall bits per leaf, and the TOP bits of their subtrees.
static MASKS: [AtomicU32; LEAVES] = [const { AtomicU32::new(0) }; LEAVES];
static TOP_MASK: AtomicU32 = AtomicU32::new(0);
/// 1 - MSI (re-arm after each interrupt), 2 - MSI-X.
static DELIVERY: AtomicU8 = AtomicU8::new(0);
/// MSI-X: the table's address and how many entries point at us (masked on
/// disarm and on a storm - MSI-X needs no re-arm, so a storm would not stop
/// by itself).
static MSIX_TABLE: AtomicUsize = AtomicUsize::new(0);
static MSIX_USED: AtomicUsize = AtomicUsize::new(0);

fn mask_msix() {
    let table = MSIX_TABLE.load(Ordering::Relaxed);
    if table == 0 {
        return;
    }
    for index in 0..MSIX_USED.load(Ordering::Relaxed) {
        // SAFETY: the table is inside the mapped BAR0 (set in `deliver`),
        // `index` below the entries we programmed.
        unsafe { crate::pci::Device::mask_msix_vector(table, index) };
    }
}

/// The MSI target from `alloc_msi` (a handler cannot be given back, so it is
/// taken once per boot and reused when GSP-RM comes up again).
static MSI_TARGET: SpinLock<Option<(u64, u32)>> = SpinLock::new(None);
/// Nonstall vectors from the last table (kept for `fenceirq on`).
static VECTORS: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());
/// The state line for `fenceirq` and the fallback reason.
static STATUS: SpinLock<String> = SpinLock::new(String::new());

// Counters (since boot).
static IRQS: AtomicU64 = AtomicU64::new(0);
/// Interrupts that found one of our nonstall bits pending.
static HITS: AtomicU64 = AtomicU64::new(0);
/// Pending bits that were not ours (seen, cleared only if ours).
static FOREIGN: AtomicU64 = AtomicU64::new(0);
static ARMED_AT_MS: AtomicU64 = AtomicU64::new(0);
/// The first sleeping fence wait after arming (the trial clock starts here:
/// the card may be armed at boot and a game start minutes later).
static TRIAL_FROM_MS: AtomicU64 = AtomicU64::new(0);
static FIRST_IRQ_MS: AtomicU64 = AtomicU64::new(0);
static STORM_WINDOW: AtomicU64 = AtomicU64::new(0);
static STORM_COUNT: AtomicU64 = AtomicU64::new(0);
static STORM: AtomicBool = AtomicBool::new(false);
/// Sleeps of fence waits, indexed by [`End`], in the current judging window
/// (`Live`: `[0]` and `[1]` start again every 256 finished waits - hw-log18
/// read "by irq 233 / by timer 697547" as "the interrupt rarely wakes
/// anyone", while 233 was a window count and the timer one a total).
static ENDS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// The same since boot, never reset (perf-diag, `fenceirq`).
static TOTAL_ENDS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// When the handler last found a fence bit (uptime ns): the wake latency of
/// a wait that ends by the interrupt is measured from it.
static LAST_HIT_NS: AtomicU64 = AtomicU64::new(0);
/// Interrupt -> waiter back with its fence done: sum, count, worst (ns).
static WAKE_NS: AtomicU64 = AtomicU64::new(0);
static WAKES: AtomicU64 = AtomicU64::new(0);
static WAKE_WORST_NS: AtomicU64 = AtomicU64::new(0);
/// Sleeping waits in `Trial` (to judge "no interrupt").
static TRIAL_SLEEPS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        1 => Mode::Trial,
        2 => Mode::Live,
        _ => Mode::Off,
    }
}

fn set_status(line: String) {
    crate::kprintln!("  {line}");
    *STATUS.lock() = line;
}

/// The MSI handler.
fn on_interrupt() {
    IRQS.fetch_add(1, Ordering::Relaxed);
    // Storm guard first (also while nothing is armed): interrupts we cannot
    // clear would come back at once.
    let now_ms = crate::time::uptime_ms();
    let window = now_ms / 100;
    if STORM_WINDOW.swap(window, Ordering::Relaxed) != window {
        STORM_COUNT.store(0, Ordering::Relaxed);
    }
    if STORM_COUNT.fetch_add(1, Ordering::Relaxed) > STORM_PER_100MS {
        // Leave everything off (TOP stays cleared, no MSI re-arm, MSI-X
        // masked); the wait path logs and falls back.
        STORM.store(true, Ordering::Relaxed);
        if let Some(base) = core::num::NonZeroUsize::new(BAR.swap(0, Ordering::AcqRel)) {
            Bar0(base.get()).wr(TOP_EN_CLEAR, TOP_MASK.load(Ordering::Relaxed));
        }
        mask_msix();
        return;
    }
    let base = BAR.load(Ordering::Acquire);
    if base == 0 {
        return;
    }
    let bar = Bar0(base);
    let top = TOP_MASK.load(Ordering::Relaxed);
    bar.wr(TOP_EN_CLEAR, top);
    let mut ours = false;
    for (leaf, mask) in MASKS.iter().enumerate() {
        let mask = mask.load(Ordering::Relaxed);
        if mask == 0 {
            continue;
        }
        let pending = bar.rd(LEAF + leaf * 4);
        if pending == u32::MAX {
            continue; // the card fell off the bus
        }
        if pending & mask != 0 {
            bar.wr(LEAF + leaf * 4, pending & mask);
            ours = true;
        }
        if pending & !mask != 0 {
            FOREIGN.fetch_add(1, Ordering::Relaxed);
        }
    }
    bar.wr(TOP_EN_SET, top);
    if DELIVERY.load(Ordering::Relaxed) == 1 {
        bar.wr(MSI_REARM, 0);
    }
    if ours {
        HITS.fetch_add(1, Ordering::Relaxed);
        LAST_HIT_NS.store(crate::time::uptime_ns(), Ordering::Relaxed);
        let _ = FIRST_IRQ_MS.compare_exchange(0, now_ms.max(1), Ordering::Relaxed, Ordering::Relaxed);
        hrsleep::signal(SRC_GPU);
    }
}

/// `NV2080_CTRL_CMD_INTERNAL_INTR_GET_KERNEL_TABLE`: nonstall vectors the
/// CPU owns. The parameter size differs between RM versions (with or without
/// the subtree map) - the first size RM accepts wins.
fn kernel_table(log: &mut Log) -> Result<Vec<u32>, String> {
    let r = with_rm(log, |rm, internal| {
        let mut last = String::new();
        for size in [0x814usize, 0x810, 0x804, 0x808, 0x820] {
            match rm.control(internal.0, internal.1, 0x2080_0a5c, &alloc::vec![0u8; size]) {
                Ok(r) => return Ok((size, r)),
                Err(why) => last = why,
            }
        }
        Err(last)
    })?;
    let (size, r) = r?;
    let le = |at: usize| r.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let len = (le(0) as usize).min(128);
    let mut vectors = Vec::new();
    let mut line = format!("fenceirq: RM interrupt table ({len} entries, params {size:#x}):");
    for i in 0..len {
        let at = 4 + i * 16;
        let engine = le(at) & 0xffff;
        let (stall, nonstall) = (le(at + 8), le(at + 12));
        line.push_str(&format!(" [{engine} s{stall:#x} n{nonstall:#x}]"));
        if (nonstall as usize) < LEAVES * 32 && !vectors.contains(&nonstall) {
            vectors.push(nonstall);
        }
    }
    say(log, line);
    Ok(vectors)
}

/// Point the device's MSI (or MSI-X) at our handler. `Some(kind)`.
fn deliver(device: &crate::pci::Device, bar: Bar0, log: &mut Log) -> Option<u8> {
    let target = {
        let mut t = MSI_TARGET.lock();
        if t.is_none() {
            *t = crate::arch::interrupts::alloc_msi(on_interrupt);
        }
        *t
    };
    let Some((address, data)) = target else {
        say(log, "fenceirq: no free MSI vector".into());
        return None;
    };
    if let Some(msi) = device.msi() {
        // SAFETY: the capability is this device's, the handler is installed.
        unsafe { device.set_msi_vector(&msi, address, data) };
        say(log, format!("fenceirq: MSI -> {address:#x} data {data:#x} (64-bit {}, maskable {})", msi.wide, msi.maskable));
        return Some(1);
    }
    if let Some(msix) = device.msix() {
        if msix.bir != 0 {
            say(log, format!("fenceirq: MSI-X table in BAR{} - not mapped here", msix.bir));
            return None;
        }
        let table = bar.0 + msix.table_offset as usize;
        // Which entry the CPU tree uses is not known here: all of the first
        // eight point at us.
        let used = usize::from(msix.vectors).min(8);
        for index in 0..used {
            // SAFETY: the table is inside the mapped BAR0, `index` < vectors.
            unsafe { device.set_msix_vector(&msix, table, index, address, data) };
        }
        MSIX_TABLE.store(table, Ordering::Relaxed);
        MSIX_USED.store(used, Ordering::Relaxed);
        say(log, format!("fenceirq: MSI-X ({} vectors, table at BAR0+{:#x}) -> {address:#x} data {data:#x}", msix.vectors, msix.table_offset));
        return Some(2);
    }
    say(log, "fenceirq: the card offers neither MSI nor MSI-X".into());
    None
}

/// Set the interrupt up (backend attach, `fenceirq on`). Never fails the
/// caller: a problem is logged and fence waits stay on polling.
pub(crate) fn arm(log: &mut Log) {
    disarm("re-arm");
    if !SWITCH.load(Ordering::Relaxed) {
        set_status("fenceirq: off ('fenceirq on' to enable)".into());
        return;
    }
    let Ok((device, bar)) = super::nvidia_rm_info::card() else { return };
    let vectors = match kernel_table(log) {
        Ok(v) if !v.is_empty() => {
            *VECTORS.lock() = v.clone();
            v
        }
        Ok(_) => {
            set_status("fenceirq: fallback - RM lists no nonstall vector; fence waits poll".into());
            return;
        }
        Err(why) => {
            let cached = VECTORS.lock().clone();
            if cached.is_empty() {
                set_status(format!("fenceirq: fallback - interrupt table: {why}; fence waits poll"));
                return;
            }
            cached
        }
    };
    let mut masks = [0u32; LEAVES];
    for v in &vectors {
        masks[*v as usize / 32] |= 1 << (v % 32);
    }
    let top = masks.iter().enumerate().filter(|(_, m)| **m != 0).fold(0u32, |t, (leaf, _)| t | 1 << (leaf / 2));
    // What was on before us (for the next log reading).
    let before: Vec<u32> = (0..LEAVES).map(|leaf| bar.rd(LEAF_EN_SET + leaf * 4)).collect();
    say(
        log,
        format!(
            "fenceirq: before: top {:#x} top_en {:#x} leaf_en {before:x?}; ours: vectors {vectors:?} leaf masks {masks:x?} top {top:#x}",
            bar.rd(TOP),
            bar.rd(TOP_EN_SET)
        ),
    );
    let Some(kind) = deliver(&device, bar, log) else {
        set_status("fenceirq: fallback - no MSI delivery; fence waits poll".into());
        return;
    };
    for (leaf, mask) in masks.iter().enumerate() {
        MASKS[leaf].store(*mask, Ordering::Relaxed);
        if *mask != 0 {
            bar.wr(LEAF + leaf * 4, *mask); // stale pending bits
            bar.wr(LEAF_EN_SET + leaf * 4, *mask);
        }
    }
    TOP_MASK.store(top, Ordering::Relaxed);
    DELIVERY.store(kind, Ordering::Relaxed);
    for end in &ENDS {
        end.store(0, Ordering::Relaxed);
    }
    TRIAL_SLEEPS.store(0, Ordering::Relaxed);
    TRIAL_FROM_MS.store(0, Ordering::Relaxed);
    FIRST_IRQ_MS.store(0, Ordering::Relaxed);
    STORM.store(false, Ordering::Relaxed);
    ARMED_AT_MS.store(crate::time::uptime_ms(), Ordering::Relaxed);
    BAR.store(bar.0, Ordering::Release);
    bar.wr(TOP_EN_SET, top);
    if kind == 1 {
        bar.wr(MSI_REARM, 0);
    }
    MODE.store(Mode::Trial as u8, Ordering::Release);
    set_status(format!(
        "fenceirq: enabled (trial) - {} nonstall vector(s), {}; falls back by itself if no interrupt comes within {TRIAL_MS} ms of sleeping fence waits",
        vectors.len(),
        if kind == 1 { "MSI" } else { "MSI-X" }
    ));
}

/// Stop: our leaves and TOP bits off, waits poll. `why` goes to the log if
/// it was on.
pub(crate) fn disarm(why: &str) {
    let was = mode();
    MODE.store(Mode::Off as u8, Ordering::Release);
    let base = BAR.swap(0, Ordering::AcqRel);
    mask_msix();
    if base != 0 {
        let bar = Bar0(base);
        bar.wr(TOP_EN_CLEAR, TOP_MASK.load(Ordering::Relaxed));
        for (leaf, mask) in MASKS.iter().enumerate() {
            let mask = mask.swap(0, Ordering::Relaxed);
            if mask != 0 {
                bar.wr(LEAF_EN_CLEAR + leaf * 4, mask);
            }
        }
    }
    // Waiters asleep for a long backstop: let them poll again.
    hrsleep::signal(SRC_GPU);
    if was != Mode::Off {
        crate::kprintln!("  fenceirq: disarmed ({why}); {}", counters());
    }
}

/// GSP-RM goes away (unload): the registers are no longer ours.
pub(crate) fn shutdown() {
    disarm("GSP-RM unloaded");
}

fn fallback(reason: String) {
    disarm("fallback");
    set_status(format!("fenceirq: fallback - {reason}; fence waits poll as before ('fenceirq on' tries again)"));
}

fn counters() -> String {
    let total = |end: End| TOTAL_ENDS[end as usize].load(Ordering::Relaxed);
    let wakes = WAKES.load(Ordering::Relaxed);
    format!(
        "{} irqs, {} with fence bits, {} with other bits; since boot, sleeps of fence waits: {} ended by the interrupt with the fence done \
         (interrupt to waiter {} us avg, worst {} us), {} found the fence done only at the backstop timer (missed or late interrupt), \
         {} woke by an interrupt for another fence, {} slept to the backstop with the fence pending; judging window: irq {}, late {}",
        IRQS.load(Ordering::Relaxed),
        HITS.load(Ordering::Relaxed),
        FOREIGN.load(Ordering::Relaxed),
        total(End::Irq),
        WAKE_NS.load(Ordering::Relaxed) / wakes.max(1) / 1000,
        WAKE_WORST_NS.load(Ordering::Relaxed) / 1000,
        total(End::TimerDone),
        total(End::IrqOther),
        total(End::Timer),
        ENDS[End::Irq as usize].load(Ordering::Relaxed),
        ENDS[End::TimerDone as usize].load(Ordering::Relaxed),
    )
}


/// How a sleep of a fence wait ended.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum End {
    /// The interrupt source moved during the sleep and the fence is done.
    Irq,
    /// The backstop timer, and the fence was done (an interrupt was missed
    /// or came late).
    TimerDone,
    /// The backstop timer, fence still pending (normal for long waits).
    Timer,
    /// Woken by the interrupt, but the fence is still pending: another
    /// channel or an earlier submission of this one finished (every nonstall
    /// interrupt wakes every fence waiter).
    IrqOther,
}

/// How one sleep of a fence wait ended: `by_irq` - the interrupt source
/// moved during it, `done` - the fence is signalled now.
#[must_use]
pub fn classify(by_irq: bool, done: bool) -> End {
    match (by_irq, done) {
        (true, true) => End::Irq,
        (false, true) => End::TimerDone,
        (true, false) => End::IrqOther,
        (false, false) => End::Timer,
    }
}

/// The wait path reports every sleep (task context, may log). Moves the
/// state machine.
pub fn after_wait(end: End) {
    let m = mode();
    if m == Mode::Off {
        return;
    }
    ENDS[end as usize].fetch_add(1, Ordering::Relaxed);
    TOTAL_ENDS[end as usize].fetch_add(1, Ordering::Relaxed);
    if end == End::Irq {
        let hit = LAST_HIT_NS.load(Ordering::Relaxed);
        if hit != 0 {
            let took = crate::time::uptime_ns().saturating_sub(hit);
            WAKE_NS.fetch_add(took, Ordering::Relaxed);
            WAKES.fetch_add(1, Ordering::Relaxed);
            WAKE_WORST_NS.fetch_max(took, Ordering::Relaxed);
        }
    }
    if STORM.load(Ordering::Relaxed) {
        fallback(format!("interrupt storm (> {STORM_PER_100MS} in 100 ms; pending bits we do not own?); {}", counters()));
        return;
    }
    let now = crate::time::uptime_ms();
    match m {
        Mode::Trial => {
            let _ = TRIAL_FROM_MS.compare_exchange(0, now.max(1), Ordering::Relaxed, Ordering::Relaxed);
            let sleeps = TRIAL_SLEEPS.fetch_add(1, Ordering::Relaxed) + 1;
            let hits = HITS.load(Ordering::Relaxed);
            let by_irq = ENDS[0].load(Ordering::Relaxed);
            if hits >= 32 && by_irq >= 16 {
                MODE.store(Mode::Live as u8, Ordering::Release);
                for end in &ENDS {
                    end.store(0, Ordering::Relaxed);
                }
                set_status(format!(
                    "fenceirq: live - first interrupt {} ms after arming ({} ms after the first sleeping wait), {hits} so far; fence waits sleep until the interrupt",
                    FIRST_IRQ_MS.load(Ordering::Relaxed).saturating_sub(ARMED_AT_MS.load(Ordering::Relaxed)),
                    now.saturating_sub(TRIAL_FROM_MS.load(Ordering::Relaxed))
                ));
            } else if now.saturating_sub(TRIAL_FROM_MS.load(Ordering::Relaxed)) > TRIAL_MS && sleeps >= 256 {
                let why = if hits == 0 {
                    format!("no interrupt within {TRIAL_MS} ms ({sleeps} sleeping fence waits)")
                } else {
                    format!("only {hits} interrupt(s) in {TRIAL_MS} ms ({sleeps} sleeping waits; MSI not re-armed?)")
                };
                fallback(format!("{why}; {}", counters()));
            }
        }
        Mode::Live => {
            let irq = ENDS[0].load(Ordering::Relaxed);
            let late = ENDS[1].load(Ordering::Relaxed);
            let total = irq + late;
            if total >= 256 {
                if late * 4 > total {
                    fallback(format!("{late} of {total} sleeping waits found their fence done only at the backstop timer (missed interrupts); {}", counters()));
                } else {
                    // A new window.
                    ENDS[0].store(0, Ordering::Relaxed);
                    ENDS[1].store(0, Ordering::Relaxed);
                }
            }
        }
        Mode::Off => {}
    }
}

/// `fenceirq [on|off]` (kernel shell).
pub fn command(argument: &str) -> String {
    match argument.trim() {
        "on" => {
            SWITCH.store(true, Ordering::Relaxed);
            if super::nvidia_rm_chan::backend().is_some() {
                let mut log = Log::new();
                arm(&mut log);
            } else {
                set_status("fenceirq: on (armed when the GPU backend comes up)".into());
            }
        }
        "off" => {
            SWITCH.store(false, Ordering::Relaxed);
            disarm("switched off");
            set_status("fenceirq: off ('fenceirq on' to enable)".into());
        }
        "" => {}
        other if other.starts_with("test") => {
            let rounds = other["test".len()..].trim().parse::<u64>().unwrap_or(200).clamp(1, 100_000);
            return self_test(rounds);
        }
        _ => return "fenceirq: usage `fenceirq [on|off|test N]`".into(),
    }
    status()
}

/// State and counters (shell, perf-diag).
#[must_use]
pub fn status() -> String {
    let line = STATUS.lock().clone();
    let head = if line.is_empty() { String::from("fenceirq: not set up (no GPU backend yet)") } else { line };
    format!("{head}; mode {:?}; {}", mode(), counters())
}

// ---- self-test without a card (QEMU) -------------------------------------------

/// `fenceirq test N`: the fence wait path (`drm::sync::wait_until`) in `Live`
/// mode against a simulated interrupt - a kernel task "finishes" a fence at a
/// random moment 0.1-3 ms after the wait began and signals [`SRC_GPU`] the
/// way [`on_interrupt`] does; half the waits also get an interrupt for
/// "another fence" first. Checks what hw-log18 could not tell: that waits end
/// by the interrupt (not the backstop timer), how long the wake-up takes,
/// and that foreign interrupts are counted apart. Only while no card is
/// armed (QEMU); the state is put back afterwards.
fn self_test(rounds: u64) -> String {
    if mode() != Mode::Off || BAR.load(Ordering::Acquire) != 0 {
        return String::from("fenceirq test: the interrupt is in use (a card is armed); nothing done");
    }
    if !hrsleep::enabled() {
        return String::from("fenceirq test: fine sleeps (hrsleep) are off; nothing done");
    }
    let before: [u64; 4] = core::array::from_fn(|i| TOTAL_ENDS[i].load(Ordering::Relaxed));
    let wakes_before = (WAKES.load(Ordering::Relaxed), WAKE_NS.load(Ordering::Relaxed));
    TEST_DUE_NS.store(0, Ordering::SeqCst);
    TEST_FOREIGN_NS.store(0, Ordering::SeqCst);
    TEST_DONE.store(0, Ordering::SeqCst);
    TEST_ROUND.store(0, Ordering::SeqCst);
    TEST_RUN.store(true, Ordering::SeqCst);
    MODE.store(Mode::Live as u8, Ordering::Release);
    if crate::sched::spawn("fenceirq-test", test_interrupts).is_err() {
        MODE.store(Mode::Off as u8, Ordering::Release);
        TEST_RUN.store(false, Ordering::SeqCst);
        return String::from("fenceirq test: no task for the simulated interrupt");
    }
    let (mut sum, mut worst, mut errors) = (0u64, 0u64, 0u64);
    let mut seed = crate::time::uptime_ns() | 1;
    for round in 1..=rounds {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let start = crate::time::uptime_ns();
        let delay = 100_000 + seed % 2_900_000;
        if round % 2 == 0 {
            TEST_FOREIGN_NS.store(start + delay / 2, Ordering::SeqCst);
        }
        TEST_ROUND.store(round, Ordering::SeqCst);
        TEST_DUE_NS.store(start + delay, Ordering::SeqCst);
        // The Linux DRM layer (and its wait) exists on x86-64 only.
        #[cfg(target_arch = "x86_64")]
        let result = crate::linux::drm::fence_wait(Some(start + 50_000_000), || TEST_DONE.load(Ordering::SeqCst) >= round);
        #[cfg(not(target_arch = "x86_64"))]
        let result: Result<(), i64> = Err(0);
        let end = crate::time::uptime_ns();
        if result.is_err() {
            errors += 1;
            TEST_DONE.store(round, Ordering::SeqCst);
        }
        // From the moment the "card" finished it (the simulated interrupt
        // task itself may run late under QEMU).
        let done_at = TEST_DONE_AT_NS.load(Ordering::SeqCst);
        let late = end.saturating_sub(if done_at > start { done_at } else { start + delay });
        sum += late;
        worst = worst.max(late);
    }
    TEST_RUN.store(false, Ordering::SeqCst);
    MODE.store(Mode::Off as u8, Ordering::Release);
    for end in &ENDS {
        end.store(0, Ordering::Relaxed);
    }
    hrsleep::signal(SRC_GPU);
    let delta: [u64; 4] = core::array::from_fn(|i| TOTAL_ENDS[i].load(Ordering::Relaxed) - before[i]);
    let wakes = WAKES.load(Ordering::Relaxed) - wakes_before.0;
    let wake_ns = WAKE_NS.load(Ordering::Relaxed) - wakes_before.1;
    format!(
        "fenceirq test: {rounds} waits, {errors} timed out; fence seen {} us after it was done on average, worst {} us; sleeps ended: \
         {} by the interrupt (interrupt to waiter {} us avg), {} done only at the backstop, {} by an interrupt for another fence, {} backstop with the fence pending",
        sum / rounds / 1000,
        worst / 1000,
        delta[End::Irq as usize],
        wake_ns / wakes.max(1) / 1000,
        delta[End::TimerDone as usize],
        delta[End::IrqOther as usize],
        delta[End::Timer as usize],
    )
}

static TEST_RUN: AtomicBool = AtomicBool::new(false);
/// When the simulated fence is due / when an interrupt for "another fence"
/// comes (uptime ns, 0 - none).
static TEST_DUE_NS: AtomicU64 = AtomicU64::new(0);
static TEST_FOREIGN_NS: AtomicU64 = AtomicU64::new(0);
/// The round being waited for, and the last round whose fence is done.
static TEST_ROUND: AtomicU64 = AtomicU64::new(0);
/// When the last simulated fence was finished (uptime ns).
static TEST_DONE_AT_NS: AtomicU64 = AtomicU64::new(0);
static TEST_DONE: AtomicU64 = AtomicU64::new(0);

/// The simulated interrupt: a task that watches the clock to the next due
/// moment and signals [`SRC_GPU`] like [`on_interrupt`].
fn test_interrupts() {
    while TEST_RUN.load(Ordering::SeqCst) {
        let foreign = TEST_FOREIGN_NS.load(Ordering::SeqCst);
        let due = TEST_DUE_NS.load(Ordering::SeqCst);
        let now = crate::time::uptime_ns();
        if foreign != 0 && foreign <= now {
            TEST_FOREIGN_NS.store(0, Ordering::SeqCst);
            hrsleep::signal(SRC_GPU);
            continue;
        }
        if due != 0 && due <= now {
            TEST_DUE_NS.store(0, Ordering::SeqCst);
            TEST_DONE_AT_NS.store(crate::time::uptime_ns(), Ordering::SeqCst);
            TEST_DONE.store(TEST_ROUND.load(Ordering::SeqCst), Ordering::SeqCst);
            LAST_HIT_NS.store(crate::time::uptime_ns(), Ordering::Relaxed);
            hrsleep::signal(SRC_GPU);
            continue;
        }
        // Polls (with yields) rather than fine sleeps: a fine sleep of this
        // task would end in the same timer pass as the waiter's backstop
        // and turn the test into a race of two timer wake-ups (QEMU).
        if due == 0 && foreign == 0 {
            crate::sched::sleep_ms(1);
        } else {
            crate::sched::yield_now();
        }
    }
}
