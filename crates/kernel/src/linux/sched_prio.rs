//! Priorities of Linux programs (25.11.2026): `getpriority`/`setpriority`,
//! `sched_setscheduler` and friends, `sched_setattr`/`sched_getattr`.
//!
//! The scheduler side is `sched::prio`: nice -20..19 weights the CPU share
//! as in Linux CFS. As in Linux, `PRIO_PROCESS` names a *thread* (`who` is a
//! tid; `nice -n 5 obs` sets the main thread before `execve`, and every
//! thread made later inherits it), `PRIO_PGRP` a process (each process is
//! its own group here) and `PRIO_USER` every program of a user.
//!
//! Permission as on a default Linux: the owner may raise nice, a lower nice
//! needs root (`RLIMIT_NICE` is 0 - so wineserver does not try to give
//! Windows thread priorities to unix threads: its `nice_limit` stays 20),
//! other users' tasks need root. `SCHED_FIFO`/`SCHED_RR` - root only; they
//! run with the heaviest weight (nice -20), not as a separate class:
//! a spinning real-time thread cannot freeze the machine.

use alloc::vec::Vec;

use user_abi::{PRIO_POLICY_BATCH, PRIO_POLICY_FIFO, PRIO_POLICY_IDLE, PRIO_POLICY_OTHER, PRIO_POLICY_RR};

use super::{e, ipc};
use crate::sched::prio::{self, PrioError};

const PRIO_PROCESS: usize = 0;
const PRIO_PGRP: usize = 1;
const PRIO_USER: usize = 2;
const SCHED_DEADLINE: u32 = 6;
const SCHED_RESET_ON_FORK: u32 = 0x4000_0000;
/// `sched_attr.sched_flags`: keep the policy / keep the parameters.
const SCHED_FLAG_KEEP_POLICY: u64 = 0x08;
const SCHED_FLAG_KEEP_PARAMS: u64 = 0x10;
/// `SCHED_ATTR_SIZE_VER0`.
const ATTR_SIZE: u32 = 48;
const E2BIG: i64 = 7;

fn errno(err: PrioError) -> i64 {
    match err {
        PrioError::NotFound => -e::ESRCH,
        PrioError::Permission => -e::EPERM,
        PrioError::Access => -e::EACCES,
        PrioError::Invalid => -e::EINVAL,
    }
}

/// The tasks `which`/`who` name.
fn targets(which: usize, who: usize) -> Result<Vec<usize>, i64> {
    let who = u32::try_from(who).map_err(|_| -e::ESRCH)?;
    match which {
        PRIO_PROCESS => prio::slot_of_task(who).map(|slot| alloc::vec![slot]).ok_or(-e::ESRCH),
        PRIO_PGRP => {
            let pid = if who == 0 { crate::user::with_current(|program| program.linux_pid).unwrap_or(0) } else { who };
            prio::slot_of_task(pid).map(prio::process_slots).ok_or(-e::ESRCH)
        }
        PRIO_USER => {
            let uid = if who == 0 { crate::user::credentials().uid } else { who };
            let slots: Vec<usize> = crate::sched::snapshot()
                .into_iter()
                .filter(|task| task.state != crate::sched::TaskState::Finished)
                .filter(|task| prio::owner_of(task.slot) == Some(uid))
                .map(|task| task.slot)
                .collect();
            if slots.is_empty() { Err(-e::ESRCH) } else { Ok(slots) }
        }
        _ => Err(-e::EINVAL),
    }
}

/// `getpriority(which, who)`: `20 - nice` of the highest priority among
/// them (the raw answer of the Linux call; libc turns it into nice).
pub fn getpriority(which: usize, who: usize) -> i64 {
    match targets(which, who) {
        Ok(slots) => {
            let nice = slots.iter().map(|&slot| prio::nice_of(slot)).min().unwrap_or(0);
            i64::from(20 - nice)
        }
        Err(err) => err,
    }
}

/// `setpriority(which, who, nice)`: nice is clamped to -20..19 as in Linux.
pub fn setpriority(which: usize, who: usize, nice: usize) -> i64 {
    let nice = (nice as i32).clamp(user_abi::NICE_MIN, user_abi::NICE_MAX);
    let slots = match targets(which, who) {
        Ok(slots) => slots,
        Err(err) => return err,
    };
    for &slot in &slots {
        if let Err(err) = prio::may_set_nice(slot, nice) {
            return errno(err);
        }
    }
    for &slot in &slots {
        prio::set_nice(slot, nice);
    }
    0
}

fn policy_valid(policy: u32, rt_prio: u32) -> Result<(), i64> {
    match policy {
        PRIO_POLICY_OTHER | PRIO_POLICY_BATCH | PRIO_POLICY_IDLE if rt_prio == 0 => Ok(()),
        PRIO_POLICY_FIFO | PRIO_POLICY_RR if (1..=99).contains(&rt_prio) => Ok(()),
        // Deadline scheduling is not there.
        SCHED_DEADLINE => Err(-e::EPERM),
        _ => Err(-e::EINVAL),
    }
}

/// One thread by tid (0 - the caller), as the `sched_*` calls name it.
fn thread(pid: usize) -> Result<usize, i64> {
    if (pid as i64) < 0 {
        return Err(-e::EINVAL);
    }
    u32::try_from(pid).ok().and_then(prio::slot_of_task).ok_or(-e::ESRCH)
}

fn set_policy_of(slot: usize, policy: u32, rt_prio: u32) -> i64 {
    if let Err(err) = policy_valid(policy, rt_prio) {
        return err;
    }
    if let Err(err) = prio::may_set_policy(slot, policy) {
        return errno(err);
    }
    prio::set_policy(slot, policy, rt_prio);
    0
}

/// `sched_setscheduler(pid, policy, param)`.
pub fn sched_setscheduler(pid: usize, policy: usize, param: usize) -> i64 {
    let slot = match thread(pid) {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    let rt_prio = match ipc::get::<i32>(param) {
        Ok(value) if value >= 0 => value as u32,
        Ok(_) => return -e::EINVAL,
        Err(err) => return err,
    };
    set_policy_of(slot, policy as u32 & !SCHED_RESET_ON_FORK, rt_prio)
}

/// `sched_getscheduler(pid)`.
pub fn sched_getscheduler(pid: usize) -> i64 {
    match thread(pid) {
        Ok(slot) => i64::from(prio::policy_of(slot).0),
        Err(err) => err,
    }
}

/// `sched_setparam(pid, param)`: the real-time priority of the policy it has.
pub fn sched_setparam(pid: usize, param: usize) -> i64 {
    let slot = match thread(pid) {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    let rt_prio = match ipc::get::<i32>(param) {
        Ok(value) if value >= 0 => value as u32,
        Ok(_) => return -e::EINVAL,
        Err(err) => return err,
    };
    set_policy_of(slot, prio::policy_of(slot).0, rt_prio)
}

/// `sched_getparam(pid, param)`.
pub fn sched_getparam(pid: usize, param: usize) -> i64 {
    match thread(pid) {
        Ok(slot) => match ipc::put(param, prio::policy_of(slot).1 as i32) {
            Ok(()) => 0,
            Err(err) => err,
        },
        Err(err) => err,
    }
}

/// `sched_get_priority_max/min(policy)`.
pub fn priority_range(policy: usize, max: bool) -> i64 {
    match policy as u32 {
        PRIO_POLICY_FIFO | PRIO_POLICY_RR => {
            if max {
                99
            } else {
                1
            }
        }
        PRIO_POLICY_OTHER | PRIO_POLICY_BATCH | PRIO_POLICY_IDLE => 0,
        _ => -e::EINVAL,
    }
}

/// `struct sched_attr` (VER0, 48 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SchedAttr {
    size: u32,
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
}

/// `sched_setattr(pid, attr, flags)`.
pub fn sched_setattr(pid: usize, attr: usize, flags: usize) -> i64 {
    if flags != 0 {
        return -e::EINVAL;
    }
    let slot = match thread(pid) {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    // A size of 0 means VER0; extra fields of a bigger struct are not read.
    match ipc::get::<u32>(attr) {
        Ok(size) if size != 0 && (size < ATTR_SIZE || size > 4096) => {
            // As Linux: the size the kernel knows goes back, E2BIG.
            let _ = ipc::put(attr, ATTR_SIZE);
            return -E2BIG;
        }
        Ok(_) => {}
        Err(err) => return err,
    }
    let value = match ipc::get::<SchedAttr>(attr) {
        Ok(value) => value,
        Err(err) => return err,
    };
    let policy = if value.flags & SCHED_FLAG_KEEP_POLICY != 0 { prio::policy_of(slot).0 } else { value.policy & !SCHED_RESET_ON_FORK };
    let rt_prio = if value.flags & SCHED_FLAG_KEEP_PARAMS != 0 { prio::policy_of(slot).1 } else { value.priority };
    if let Err(err) = policy_valid(policy, rt_prio) {
        return err;
    }
    if let Err(err) = prio::may_set_policy(slot, policy) {
        return errno(err);
    }
    let nice = value.nice.clamp(user_abi::NICE_MIN, user_abi::NICE_MAX);
    let set_nice = matches!(policy, PRIO_POLICY_OTHER | PRIO_POLICY_BATCH) && value.flags & SCHED_FLAG_KEEP_PARAMS == 0;
    if set_nice {
        if let Err(err) = prio::may_set_nice(slot, nice) {
            return errno(err);
        }
        prio::set_nice(slot, nice);
    }
    prio::set_policy(slot, policy, rt_prio);
    0
}

/// `sched_getattr(pid, attr, size, flags)`.
pub fn sched_getattr(pid: usize, attr: usize, size: usize, flags: usize) -> i64 {
    if flags != 0 || size < ATTR_SIZE as usize || size > 4096 {
        return -e::EINVAL;
    }
    let slot = match thread(pid) {
        Ok(slot) => slot,
        Err(err) => return err,
    };
    let (policy, priority) = prio::policy_of(slot);
    let value = SchedAttr { size: ATTR_SIZE, policy, nice: prio::nice_of(slot), priority, ..SchedAttr::default() };
    match ipc::put(attr, value) {
        Ok(()) => 0,
        Err(err) => err,
    }
}
