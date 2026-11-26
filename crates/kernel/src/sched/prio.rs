//! Task priorities (25.11.2026): nice -20..19 with the CPU weights of Linux
//! CFS, scheduling policies, the foreground boost.
//!
//! # Why
//!
//! hw-log19: OBS (Mesa llvmpipe and x264 threads, started under `nice -n 5`)
//! took the CPUs from the game it recorded - every ready task got the same
//! 10 ms round-robin turn, and nice was accepted and ignored. Now a game must
//! win the CPU over OBS.
//!
//! # How
//!
//! Every task has a *virtual run time*: the CPU time it got, divided by its
//! weight (`user_abi::weighted_ns`; nice 0 = 1024, each nice step ~1.25x).
//! Among the ready tasks of a queue the scheduler takes the one with the
//! least of it, and at the end of a slice the running task stays while it
//! still has the least (`pick_weighted` in `mod.rs`): two busy tasks at nice
//! 0 and 10 share a CPU ~90/10, at nice 0 and 5 ~75/25, equal ones 50/50 as
//! before. The order of the priority classes in front of everybody (boosted
//! by an interrupt or input, drivers, the desktop while input waits) is not
//! touched, and `sched_yield` still gives the CPU to anybody ready.
//!
//! Virtual run time is one scale for the whole machine, with a floor
//! (`MIN_VR`, the least virtual time of the tasks picked, never going back).
//! A task waking from sleep is put a little before the floor
//! (`SLEEP_CREDIT`): an interactive task runs first, a sleeper cannot bank
//! credit for the whole time it slept.
//!
//! Between CPUs (25.11, prio scenario: per-CPU scales gave only 75/25 on 4
//! CPUs - two nice 10 threads on one CPU, two nice 0 on another): at the end
//! of a slice a CPU also looks at the ready tasks of the other queues; one
//! that is behind by more than [`MIGRATE_COST`] is taken over its own. A
//! task queued behind heavier ones falls behind in virtual time and moves to
//! the CPU running lighter work - the weighted share holds across CPUs, and
//! a game thread waiting behind OBS on one CPU does not wait while another
//! CPU runs OBS.
//!
//! `prio off` (shell) - every weight is 1024 (an even share, still by
//! virtual time); `prio rr` - the round-robin walks of before.
//! `prio fg off` - no foreground boost.
//!
//! # Foreground boost
//!
//! The threads of the program that owns the focused window (its address
//! space - a Proton game is one Wine process) run as if their nice were
//! [`FG_BONUS`] lower: ~2x the weight, small enough not to starve its own
//! helpers (wineserver, the compositor), big enough to win against
//! background work at nice 0..5.

use core::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, AtomicU64, AtomicU8, Ordering};

use user_abi::{nice_weight, weighted_ns, IDLE_POLICY_WEIGHT, NICE_0_WEIGHT, PRIO_POLICY_FIFO, PRIO_POLICY_IDLE, PRIO_POLICY_RR};

use super::{is_idle_slot, MAX_TASKS, RUN_START_NS, SLOT_SPACE};

/// Priorities are honoured (`prio on|off`).
static ON: AtomicBool = AtomicBool::new(true);
/// The ready task is chosen by virtual run time (`prio on|off`); `prio rr` -
/// the round-robin walks of before 25.11 (nice ignored).
static PICKER: AtomicBool = AtomicBool::new(true);
/// The foreground boost is on (`prio fg on|off`).
static FG_ON: AtomicBool = AtomicBool::new(true);
/// How many nice steps the focused program is lifted.
pub const FG_BONUS: i32 = 3;

/// A woken task is put this much (virtual ns) before the floor.
const SLEEP_CREDIT: u64 = 5_000_000;
/// The lead a waking task keeps, at most: one slice of a nice 19 task is
/// ~680 ms of virtual time; more is a sign of a stale value.
const MAX_LEAD: u64 = 2_000_000_000;
/// A ready task of another CPU's queue must be behind by this much (virtual
/// ns) to be taken over the own queue's best: moving costs its caches.
pub const MIGRATE_COST: i64 = 3_000_000;

static NICE: [AtomicI8; MAX_TASKS] = [const { AtomicI8::new(0) }; MAX_TASKS];
static POLICY: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(0) }; MAX_TASKS];
/// `sched_priority` of `SCHED_FIFO`/`SCHED_RR` (1..99), else 0.
static RT_PRIO: [AtomicU8; MAX_TASKS] = [const { AtomicU8::new(0) }; MAX_TASKS];
/// Virtual run time, ns.
static VRUNTIME: [AtomicU64; MAX_TASKS] = [const { AtomicU64::new(0) }; MAX_TASKS];
/// The floor: the virtual time of the tasks picked, never going back.
static MIN_VR: AtomicU64 = AtomicU64::new(0);

/// Address space of the program with the focused window (0 - none) and the
/// task that owns that window.
static FG_SPACE: AtomicU64 = AtomicU64::new(0);
static FG_TASK: AtomicU32 = AtomicU32::new(0);

/// CPU time by nice (index nice + 20), of `SCHED_IDLE` tasks (40) and of
/// real-time ones (41), and of the foreground program - for `perf-diag`
/// and `prio`. Since the last [`take_shares`].
const BUCKETS: usize = 42;
static NICE_NS: [AtomicU64; BUCKETS] = [const { AtomicU64::new(0) }; BUCKETS];
static FG_NS: AtomicU64 = AtomicU64::new(0);
/// Slice ends that kept the running task (it still had the least virtual
/// time), and that took a task from another CPU's queue - since boot.
pub(super) static STAYS: AtomicU64 = AtomicU64::new(0);
pub(super) static PULLS: AtomicU64 = AtomicU64::new(0);

/// Are priorities honoured.
#[must_use]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// `prio on|off`.
pub fn set_enabled(on: bool) {
    ON.store(on, Ordering::Relaxed);
    PICKER.store(true, Ordering::Relaxed);
}

/// Is the ready task chosen by virtual run time (not `prio rr`).
#[must_use]
pub fn picker() -> bool {
    PICKER.load(Ordering::Relaxed)
}

/// `prio rr`: the round-robin of before (nice ignored).
pub fn set_round_robin() {
    ON.store(false, Ordering::Relaxed);
    PICKER.store(false, Ordering::Relaxed);
}

/// `on`, `off` or `rr`.
#[must_use]
pub fn mode_name() -> &'static str {
    if !picker() {
        "rr"
    } else if enabled() {
        "on"
    } else {
        "off"
    }
}

#[must_use]
pub fn fg_enabled() -> bool {
    FG_ON.load(Ordering::Relaxed)
}

/// `prio fg on|off`.
pub fn set_fg_enabled(on: bool) {
    FG_ON.store(on, Ordering::Relaxed);
}

/// Nice of the task in a slot.
#[must_use]
pub fn nice_of(slot: usize) -> i32 {
    NICE.get(slot).map_or(0, |n| i32::from(n.load(Ordering::Relaxed)))
}

/// Policy (`SCHED_*` of Linux) and its real-time priority.
#[must_use]
pub fn policy_of(slot: usize) -> (u32, u32) {
    match (POLICY.get(slot), RT_PRIO.get(slot)) {
        (Some(policy), Some(rt)) => (u32::from(policy.load(Ordering::Relaxed)), u32::from(rt.load(Ordering::Relaxed))),
        _ => (0, 0),
    }
}

/// Set the nice of one task (clamped to -20..19). Permission is the
/// caller's business.
pub fn set_nice(slot: usize, nice: i32) {
    if let Some(cell) = NICE.get(slot) {
        cell.store(nice.clamp(user_abi::NICE_MIN, user_abi::NICE_MAX) as i8, Ordering::Relaxed);
    }
}

/// Set the policy of one task.
pub fn set_policy(slot: usize, policy: u32, rt_prio: u32) {
    if let (Some(cell), Some(rt)) = (POLICY.get(slot), RT_PRIO.get(slot)) {
        cell.store(policy as u8, Ordering::Relaxed);
        rt.store(rt_prio.min(99) as u8, Ordering::Relaxed);
    }
}

/// Is the task in `slot` part of the focused program.
fn foreground(slot: usize) -> bool {
    let fg = FG_SPACE.load(Ordering::Relaxed);
    fg != 0 && FG_ON.load(Ordering::Relaxed) && SLOT_SPACE[slot].load(Ordering::Relaxed) == fg
}

/// The nice the scheduler uses: own nice, lifted for the focused program.
#[must_use]
pub fn effective_nice(slot: usize) -> i32 {
    let nice = nice_of(slot);
    if foreground(slot) { nice - FG_BONUS } else { nice }
}

/// CPU weight of the task in `slot` (1024 with `prio off`).
#[must_use]
pub fn weight(slot: usize) -> u64 {
    if !ON.load(Ordering::Relaxed) || slot >= MAX_TASKS {
        return u64::from(NICE_0_WEIGHT);
    }
    let (policy, _) = policy_of(slot);
    let weight = match policy {
        PRIO_POLICY_IDLE => IDLE_POLICY_WEIGHT,
        // Real-time is not a separate class here: the heaviest weight.
        PRIO_POLICY_FIFO | PRIO_POLICY_RR => nice_weight(user_abi::NICE_MIN),
        _ => nice_weight(effective_nice(slot)),
    };
    u64::from(weight)
}

/// A new task in `slot`, made by the task in `parent`: nice and policy are
/// inherited (fork, clone, spawn), its virtual time starts at the floor.
pub(super) fn on_spawn(slot: usize, parent: usize) {
    let (nice, policy, rt) = if is_idle_slot(parent) || parent >= MAX_TASKS {
        (0, 0, 0)
    } else {
        (NICE[parent].load(Ordering::Relaxed), POLICY[parent].load(Ordering::Relaxed), RT_PRIO[parent].load(Ordering::Relaxed))
    };
    NICE[slot].store(nice, Ordering::Relaxed);
    POLICY[slot].store(policy, Ordering::Relaxed);
    RT_PRIO[slot].store(rt, Ordering::Relaxed);
    VRUNTIME[slot].store(MIN_VR.load(Ordering::Relaxed), Ordering::Relaxed);
}

/// The task in `slot` ran `ns` of CPU time: its virtual time and the share
/// counters. From the switch, for the leaving task.
pub(super) fn charge(slot: usize, ns: u64) {
    if is_idle_slot(slot) || slot >= MAX_TASKS || ns == 0 {
        return;
    }
    let weight = weight(slot) as u32;
    VRUNTIME[slot].fetch_add(weighted_ns(ns, weight), Ordering::Relaxed);
    let (policy, _) = policy_of(slot);
    let bucket = match policy {
        PRIO_POLICY_IDLE => 40,
        PRIO_POLICY_FIFO | PRIO_POLICY_RR => 41,
        _ => (nice_of(slot) - user_abi::NICE_MIN) as usize,
    };
    NICE_NS[bucket.min(BUCKETS - 1)].fetch_add(ns, Ordering::Relaxed);
    if foreground(slot) {
        FG_NS.fetch_add(ns, Ordering::Relaxed);
    }
}

/// Virtual time of the task, the stretch it is running now included.
fn live_vr(slot: usize, now_ns: u64) -> u64 {
    let done = VRUNTIME[slot].load(Ordering::Relaxed);
    let start = RUN_START_NS[slot].load(Ordering::Relaxed);
    if start == 0 {
        return done;
    }
    done.saturating_add(weighted_ns(now_ns.saturating_sub(start), weight(slot) as u32))
}

/// How far the task is past the floor (negative - behind it).
#[must_use]
pub(super) fn lag(slot: usize) -> i64 {
    VRUNTIME[slot].load(Ordering::Relaxed).wrapping_sub(MIN_VR.load(Ordering::Relaxed)) as i64
}

/// [`lag`] of the running task, its current stretch included.
#[must_use]
pub(super) fn live_lag(slot: usize, now_ns: u64) -> i64 {
    live_vr(slot, now_ns).wrapping_sub(MIN_VR.load(Ordering::Relaxed)) as i64
}

/// A task woke (or was made ready): at most [`SLEEP_CREDIT`] before the
/// floor, at most [`MAX_LEAD`] past it.
pub(super) fn on_wake(slot: usize, _cpu: usize) {
    if !is_idle_slot(slot) && slot < MAX_TASKS {
        let lag = lag(slot).clamp(-(SLEEP_CREDIT as i64), MAX_LEAD as i64);
        VRUNTIME[slot].store(MIN_VR.load(Ordering::Relaxed).wrapping_add(lag as u64), Ordering::Relaxed);
    }
}

/// Virtual time of the task each CPU runs (`u64::MAX` - it idles), for the
/// floor: it must not pass a running task, or a task waking at the floor
/// would take the CPU from it for as long as the gap.
static CPU_VR: [AtomicU64; crate::smp::MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; crate::smp::MAX_CPUS];

/// The floor follows the least virtual time among the running tasks.
fn advance_floor() {
    let online = crate::smp::online().clamp(1, crate::smp::MAX_CPUS);
    let least = CPU_VR[..online].iter().map(|vr| vr.load(Ordering::Relaxed)).min().unwrap_or(u64::MAX);
    if least != u64::MAX {
        MIN_VR.fetch_max(least, Ordering::Relaxed);
    }
}

/// CPU `cpu` switched to the task in `slot` (an idle slot - it idles now).
pub(super) fn on_pick(slot: usize, cpu: usize) {
    let Some(cell) = CPU_VR.get(cpu) else { return };
    if is_idle_slot(slot) || slot >= MAX_TASKS {
        cell.store(u64::MAX, Ordering::Relaxed);
        return;
    }
    cell.store(VRUNTIME[slot].load(Ordering::Relaxed), Ordering::Relaxed);
    advance_floor();
}

/// The running task stays on `cpu`.
pub(super) fn on_stay(slot: usize, cpu: usize, now_ns: u64) {
    if !is_idle_slot(slot) && slot < MAX_TASKS {
        if let Some(cell) = CPU_VR.get(cpu) {
            cell.store(live_vr(slot, now_ns), Ordering::Relaxed);
        }
        advance_floor();
    }
}

/// The desktop's focused window belongs to task `task` (0 - a window of the
/// kernel or none). Called with every pass through the desktop; cheap when
/// nothing changed.
pub fn note_focus(task: u32) {
    let was = FG_TASK.swap(task, Ordering::Relaxed);
    if was == task && (task == 0 || FG_SPACE.load(Ordering::Relaxed) != 0) {
        return;
    }
    let space = super::slot_of(super::TaskId::new(task)).map_or(0, |slot| SLOT_SPACE[slot].load(Ordering::Relaxed));
    FG_SPACE.store(space, Ordering::Relaxed);
}

/// The task whose program is in the foreground (0 - none).
#[must_use]
pub fn foreground_task() -> u32 {
    if FG_SPACE.load(Ordering::Relaxed) == 0 { 0 } else { FG_TASK.load(Ordering::Relaxed) }
}

// --- Who may change what (setpriority, sched_setscheduler, SYS_PRIORITY) ---

/// Why a priority was not changed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrioError {
    /// No such task (or it has finished).
    NotFound,
    /// Another user's program, or a kernel task, and the caller is not root.
    Permission,
    /// A higher priority (lower nice) without root: `RLIMIT_NICE` is 0, as
    /// on a default Linux (`EACCES`).
    Access,
    /// Out of range.
    Invalid,
}

impl core::fmt::Display for PrioError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            PrioError::NotFound => "no such task",
            PrioError::Permission => "not your program",
            PrioError::Access => "a higher priority needs root",
            PrioError::Invalid => "out of range",
        })
    }
}

/// Slot of a live task by id (0 - the caller).
#[must_use]
pub fn slot_of_task(id: u32) -> Option<usize> {
    if id == 0 {
        let slot = super::current_slot();
        return (!is_idle_slot(slot)).then_some(slot);
    }
    let (slot, state) = super::lookup(super::TaskId::new(id))?;
    (state != super::TaskState::Finished).then_some(slot)
}

/// Slots of every live thread of the program running in `slot` (threads
/// share the address space); a kernel task - just itself.
#[must_use]
pub fn process_slots(slot: usize) -> alloc::vec::Vec<usize> {
    let space = SLOT_SPACE[slot].load(Ordering::Relaxed);
    if space == 0 || crate::user::program_facts(slot).is_none() {
        return alloc::vec![slot];
    }
    let high = super::HIGH.load(Ordering::Acquire).min(MAX_TASKS);
    (crate::smp::MAX_CPUS..high)
        .filter(|&other| {
            SLOT_SPACE[other].load(Ordering::Relaxed) == space
                && matches!(super::RUNSTATE[other].load(Ordering::Relaxed), super::RS_READY | super::RS_RUNNING | super::RS_BLOCKED)
        })
        .collect()
}

/// Owner of the program in `slot` (`None` - a kernel task).
#[must_use]
pub fn owner_of(slot: usize) -> Option<u32> {
    crate::user::program_facts(slot).map(|(_, uid)| uid)
}

/// May the caller set nice `nice` for the task in `slot`.
pub fn may_set_nice(slot: usize, nice: i32) -> Result<(), PrioError> {
    if !(user_abi::NICE_MIN..=user_abi::NICE_MAX).contains(&nice) {
        return Err(PrioError::Invalid);
    }
    let me = crate::user::credentials();
    if me.is_root() {
        return Ok(());
    }
    match owner_of(slot) {
        Some(uid) if uid == me.uid => {}
        _ => return Err(PrioError::Permission),
    }
    if nice < nice_of(slot) { Err(PrioError::Access) } else { Ok(()) }
}

/// May the caller set a policy for the task in `slot`: real-time only for
/// root; `SCHED_OTHER`/`BATCH`/`IDLE` for the owner.
pub fn may_set_policy(slot: usize, policy: u32) -> Result<(), PrioError> {
    let me = crate::user::credentials();
    if me.is_root() {
        return Ok(());
    }
    match owner_of(slot) {
        Some(uid) if uid == me.uid => {}
        _ => return Err(PrioError::Permission),
    }
    if matches!(policy, PRIO_POLICY_FIFO | PRIO_POLICY_RR) { Err(PrioError::Permission) } else { Ok(()) }
}

/// Nice for every thread of the program of task `id` (0 - the caller's),
/// with the permission check. `Ok(threads changed)`.
pub fn set_process_nice_checked(id: u32, nice: i32) -> Result<usize, PrioError> {
    let slot = slot_of_task(id).ok_or(PrioError::NotFound)?;
    let slots = process_slots(slot);
    for &each in &slots {
        may_set_nice(each, nice)?;
    }
    for &each in &slots {
        set_nice(each, nice);
    }
    Ok(slots.len())
}

/// The same from the kernel shell (it is root): no check.
pub fn set_process_nice(id: u32, nice: i32) -> Result<usize, PrioError> {
    if !(user_abi::NICE_MIN..=user_abi::NICE_MAX).contains(&nice) {
        return Err(PrioError::Invalid);
    }
    let slot = slot_of_task(id).ok_or(PrioError::NotFound)?;
    let slots = process_slots(slot);
    for &each in &slots {
        set_nice(each, nice);
    }
    Ok(slots.len())
}

/// CPU time by priority since the last call, ns: `(nice or policy name,
/// ns)` of the buckets that ran, and of the foreground program. Resets the
/// counters.
pub fn take_shares() -> (alloc::vec::Vec<(alloc::string::String, u64)>, u64) {
    let mut rows = alloc::vec::Vec::new();
    for (index, cell) in NICE_NS.iter().enumerate() {
        let ns = cell.swap(0, Ordering::Relaxed);
        if ns == 0 {
            continue;
        }
        let name = match index {
            40 => alloc::string::String::from("idle-policy"),
            41 => alloc::string::String::from("realtime"),
            _ => alloc::format!("nice {}", index as i32 + user_abi::NICE_MIN),
        };
        rows.push((name, ns));
    }
    (rows, FG_NS.swap(0, Ordering::Relaxed))
}

/// One line about priorities: mode, foreground, CPU share by nice since the
/// last call (resets it).
pub fn diag_line() -> alloc::string::String {
    use core::fmt::Write;
    let (rows, fg) = take_shares();
    let total: u64 = rows.iter().map(|(_, ns)| ns).sum::<u64>().max(1);
    let mut line = alloc::format!(
        "prio {} (foreground boost {}, task #{}); cpu share by nice:",
        mode_name(),
        if fg_enabled() { "on" } else { "off" },
        foreground_task()
    );
    for (name, ns) in &rows {
        let _ = write!(line, " {name} {}.{}% ({} ms)", ns * 100 / total, ns * 1000 / total % 10, ns / 1_000_000);
    }
    let _ = write!(
        line,
        "; foreground {}.{}%; stays {}, pulls {} since boot",
        fg * 100 / total,
        fg * 1000 / total % 10,
        STAYS.load(Ordering::Relaxed),
        PULLS.load(Ordering::Relaxed)
    );
    line
}
