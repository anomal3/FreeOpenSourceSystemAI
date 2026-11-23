//! Потоки программ Linux (фаза 71c): `clone`, `clone3`, выход потока и
//! процесса, `prctl`.

use super::e;
use crate::mm::PageFlags;
use crate::sched::{self, TaskId};
use crate::user::space;

const CLONE_VM: usize = 0x100;
const CLONE_THREAD: usize = 0x10000;
const CLONE_SETTLS: usize = 0x80000;
const CLONE_PARENT_SETTID: usize = 0x100000;
const CLONE_CHILD_CLEARTID: usize = 0x200000;
const CLONE_CHILD_SETTID: usize = 0x1000000;

const CLONE_VFORK: usize = 0x4000;

/// `clone(flags, stack, parent_tid, child_tid, tls)` — порядок аргументов
/// x86-64. Поток — `CLONE_VM | CLONE_THREAD`; без `CLONE_THREAD` — новый
/// процесс (фаза 71d): `fork`, `vfork`, `posix_spawn`.
pub fn clone(frame: &crate::arch::TrapFrame, flags: usize, stack: usize, parent_tid: usize, child_tid: usize, tls: usize) -> i64 {
    if flags & CLONE_THREAD == 0 {
        return clone_process(frame, flags, stack, parent_tid, child_tid, tls);
    }
    if flags & CLONE_VM == 0 {
        return -e::EINVAL;
    }
    let mut regs = *frame;
    regs.rax = 0;
    if stack != 0 {
        regs.rsp = stack as u64;
    }
    let tls = if flags & CLONE_SETTLS != 0 { tls as u64 } else { crate::sched::current_bases().0 };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 };
    // Номер пишется до того, как поток войдёт в программу (см.
    // `spawn_linux_thread`): иначе быстрый поток успел бы выйти и обнулить
    // `pd->tid` раньше этой записи.
    let set_tid = [
        if flags & CLONE_PARENT_SETTID != 0 { parent_tid } else { 0 },
        if flags & CLONE_CHILD_SETTID != 0 { child_tid } else { 0 },
    ];
    let id = match crate::user::spawn_linux_thread(regs, tls, clear, set_tid) {
        Ok(id) => id,
        Err(crate::user::ThreadError::TooManyTasks) => return -e::EAGAIN,
        Err(_) => return -e::ENOMEM,
    };
    i64::from(id.as_u32())
}

/// Новый процесс — копия текущего (фаза 71d). `CLONE_VM` без `CLONE_THREAD`
/// (`vfork`, `posix_spawn` glibc) общей памяти не даёт — ребёнок получает
/// копию, — а `CLONE_VFORK` держит родителя, пока ребёнок не сделает
/// `execve` или не кончится: этого порядка `posix_spawn` и ждёт. Сигнал
/// конца (младший байт флагов) — всегда `SIGCHLD`.
fn clone_process(frame: &crate::arch::TrapFrame, flags: usize, stack: usize, parent_tid: usize, child_tid: usize, tls: usize) -> i64 {
    let mut regs = *frame;
    regs.rax = 0;
    if stack != 0 {
        regs.rsp = stack as u64;
    }
    let tls = if flags & CLONE_SETTLS != 0 { tls as u64 } else { crate::sched::current_bases().0 };
    let set = if flags & CLONE_CHILD_SETTID != 0 { child_tid } else { 0 };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 };
    let id = match crate::user::spawn_linux_process(regs, tls, set, clear, flags & CLONE_VM != 0) {
        Ok(id) => id,
        Err(crate::user::ThreadError::TooManyTasks) => return -e::EAGAIN,
        Err(_) => return -e::ENOMEM,
    };
    let pid = id.as_u32();
    if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 && space::user_can(parent_tid, 4, PageFlags::WRITE) {
        // SAFETY: адрес проверен на запись в пространстве родителя.
        unsafe { core::ptr::write_volatile(parent_tid as *mut u32, pid) };
    }
    if flags & CLONE_VFORK != 0 {
        super::process::wait_vfork(pid);
    }
    i64::from(pid)
}

/// `clone3(args, size)`: то же, что `clone`, аргументы — структурой.
pub fn clone3(frame: &crate::arch::TrapFrame, args: usize, size: usize) -> i64 {
    const MIN_SIZE: usize = 64;
    if size < MIN_SIZE || !space::user_can(args, MIN_SIZE, PageFlags::READ) {
        return -e::EINVAL;
    }
    // SAFETY: первые 64 байта `struct clone_args` проверены на чтение.
    let field = |index: usize| unsafe { core::ptr::read_unaligned((args + index * 8) as *const u64) } as usize;
    let (flags, child_tid, parent_tid, stack, stack_size, tls) = (field(0), field(2), field(3), field(5), field(6), field(7));
    let top = if stack != 0 { stack + stack_size } else { 0 };
    clone(frame, flags, top, parent_tid, child_tid, tls)
}

/// Главный ли это поток процесса.
fn is_main() -> bool {
    crate::user::with_current(|program| program.linux_pid) == Some(sched::current().as_u32())
}

/// `exit`: у потока — конец потока (ядро обнулит его `clear_tid`), у главного
/// — конец процесса.
pub fn exit(code: usize) -> i64 {
    // SAFETY: из системного вызова третьего кольца — кадр входа цел.
    unsafe { crate::arch::return_to_kernel(code as i64) }
}

/// `exit_group`: конец процесса из любого потока. Из потока — процессу
/// заказывается конец с этим кодом, главный поток будится и уходит с ним на
/// ближайшем возврате.
pub fn exit_group(code: usize) -> i64 {
    if !is_main() {
        let main = crate::user::with_current(|program| {
            program.linux_exit = Some(code as i64);
            program.kill_requested = true;
            program.linux_pid
        });
        if let Some(main) = main {
            sched::wake(TaskId::new(main));
        }
    }
    // SAFETY: из системного вызова третьего кольца — кадр входа цел.
    unsafe { crate::arch::return_to_kernel(code as i64) }
}

/// `prctl`: имя потока и флаги, которых у ядра нет, — принимаются.
pub fn prctl(option: usize, arg: usize) -> i64 {
    const PR_SET_PDEATHSIG: usize = 1;
    const PR_GET_DUMPABLE: usize = 3;
    const PR_SET_DUMPABLE: usize = 4;
    const PR_SET_NAME: usize = 15;
    const PR_GET_NAME: usize = 16;
    const PR_CAPBSET_READ: usize = 23;
    const PR_SET_NO_NEW_PRIVS: usize = 38;
    const PR_GET_NO_NEW_PRIVS: usize = 39;
    match option {
        PR_SET_NAME => {
            // hw-log15: kept per thread - perf-diag names the thread that
            // spins (`pthread_setname_np` of glibc for the calling thread).
            // Up to 16 bytes, stopping at the NUL or an unreadable byte (a
            // short name may end right before an unmapped page).
            let mut name = [0u8; 16];
            for (index, byte) in name.iter_mut().enumerate().take(15) {
                if !space::user_can(arg + index, 1, PageFlags::READ) {
                    if index == 0 {
                        return -e::EFAULT;
                    }
                    break;
                }
                // SAFETY: checked by the line above.
                *byte = unsafe { core::ptr::read_volatile((arg + index) as *const u8) };
                if *byte == 0 {
                    break;
                }
            }
            set_thread_name(&name);
            0
        }
        PR_SET_PDEATHSIG | PR_SET_DUMPABLE | PR_SET_NO_NEW_PRIVS => 0,
        PR_GET_DUMPABLE | PR_GET_NO_NEW_PRIVS => i64::from(option == PR_GET_DUMPABLE),
        PR_CAPBSET_READ => 0,
        PR_GET_NAME => {
            if !space::user_can(arg, 16, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            let slot = sched::current_slot();
            let mut name = [0u8; 16];
            match thread_name(slot) {
                Some(own) => {
                    let len = own.len().min(15);
                    name[..len].copy_from_slice(&own.as_bytes()[..len]);
                }
                None => name[..5].copy_from_slice(b"linux"),
            }
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), arg as *mut u8, name.len()) };
            0
        }
        _ => -e::EINVAL,
    }
}

/// Thread names (`PR_SET_NAME`) by task slot: 16 bytes, and the task id
/// they belong to (a reused slot has no name until it sets one).
static NAMES: [[core::sync::atomic::AtomicU64; 2]; sched::MAX_TASKS] =
    [const { [const { core::sync::atomic::AtomicU64::new(0) }; 2] }; sched::MAX_TASKS];
static NAME_IDS: [core::sync::atomic::AtomicU32; sched::MAX_TASKS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; sched::MAX_TASKS];

fn set_thread_name(name: &[u8; 16]) {
    use core::sync::atomic::Ordering::Relaxed;
    let slot = sched::current_slot();
    let id = sched::current();
    NAME_IDS[slot].store(0, Relaxed);
    NAMES[slot][0].store(u64::from_le_bytes(name[..8].try_into().unwrap_or([0; 8])), Relaxed);
    NAMES[slot][1].store(u64::from_le_bytes(name[8..].try_into().unwrap_or([0; 8])), Relaxed);
    NAME_IDS[slot].store(id.as_u32(), Relaxed);
}

/// Name a Linux thread gave itself (`PR_SET_NAME`), if the task in `slot`
/// did.
#[must_use]
pub fn thread_name(slot: usize) -> Option<alloc::string::String> {
    use core::sync::atomic::Ordering::Relaxed;
    let id = NAME_IDS.get(slot)?.load(Relaxed);
    if id == 0 || sched::slot_task_id(slot) != Some(id) {
        return None;
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&NAMES[slot][0].load(Relaxed).to_le_bytes());
    bytes[8..].copy_from_slice(&NAMES[slot][1].load(Relaxed).to_le_bytes());
    let len = bytes.iter().position(|&b| b == 0).unwrap_or(16);
    Some(alloc::string::String::from_utf8_lossy(&bytes[..len]).into_owned())
}

/// `yieldnap N` (hw-log15): a thread that calls `sched_yield` in a tight
/// loop (64 yields in a row, each within 200 us of the last) sleeps `N` us
/// before each further yield; 0 - off (default: yield stays advisory and
/// immediate, as in Linux; turn on to test on hardware whether D2R's
/// getrusage+yield spinner gives its CPU back without losing frames).
pub static YIELD_NAP_US: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static YIELD_STREAK: [core::sync::atomic::AtomicU32; sched::MAX_TASKS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; sched::MAX_TASKS];
static YIELD_LAST_NS: [core::sync::atomic::AtomicU64; sched::MAX_TASKS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; sched::MAX_TASKS];
/// Naps taken (perf-diag / `yieldnap`).
pub static YIELD_NAPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Before a `sched_yield` of a Linux thread: nap if it spins (see above).
pub fn yield_nap() {
    use core::sync::atomic::Ordering::Relaxed;
    let nap = YIELD_NAP_US.load(Relaxed);
    if nap == 0 {
        return;
    }
    let slot = sched::current_slot();
    let now = crate::time::uptime_ns();
    let last = YIELD_LAST_NS[slot].swap(now, Relaxed);
    let streak = if now.saturating_sub(last) < 200_000 + nap * 1000 {
        YIELD_STREAK[slot].fetch_add(1, Relaxed) + 1
    } else {
        YIELD_STREAK[slot].store(0, Relaxed);
        0
    };
    if streak >= 64 && sched::hrsleep::enabled() {
        YIELD_NAPS.fetch_add(1, Relaxed);
        sched::hrsleep::sleep_until_ns(now + nap * 1000, 0, 0, || false);
        YIELD_LAST_NS[slot].store(crate::time::uptime_ns(), Relaxed);
    }
}
