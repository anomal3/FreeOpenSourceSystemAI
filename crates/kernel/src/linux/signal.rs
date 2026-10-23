//! Сигналы программ Linux (фаза 71c): обработчики, маски, доставка на выходе
//! из системного вызова и `rt_sigreturn`; ожидание сигнала — `rt_sigsuspend`,
//! `pause`, `rt_sigtimedwait`.
//!
//! # Когда сигнал доставляется
//!
//! Только на выходе из системного вызова потока — `raise`, `kill` себе,
//! `sigprocmask`, снявший блокировку, и вызов, прерванный `tgkill` соседнего
//! потока. Асинхронной доставки посреди кода нет, и поэтому в кадр сигнала не
//! кладётся состояние FPU: обработчик вызывается с границы системного вызова, а
//! через неё (обычный вызов функции в C) векторные регистры не живут.
//! Отказы страниц и прочие исключения программу снимают, как и прежде.

use core::sync::atomic::{AtomicU64, Ordering};

use super::e;
use crate::mm::PageFlags;
use crate::sched;
use crate::user::{LINUX_ALTSTACK, LINUX_PENDING, LINUX_SIGMASK, LinuxAction, space};

const SIGKILL: usize = 9;
const SIGSTOP: usize = 19;
const SA_ONSTACK: usize = 0x0800_0000;
const SA_NODEFER: usize = 0x4000_0000;
const SA_RESETHAND: usize = 0x8000_0000;
const SIG_IGN: usize = 1;

fn bit(sig: usize) -> u64 {
    1u64 << (sig - 1)
}

/// Что не блокируется никогда.
const UNBLOCKABLE: u64 = (1 << (SIGKILL - 1)) | (1 << (SIGSTOP - 1));

/// `rt_sigaction(sig, act, oact, sigsetsize)`: `struct kernel_sigaction`
/// x86-64 — обработчик, флаги, `sa_restorer`, маска.
pub fn sigaction(sig: usize, act: usize, old: usize, _size: usize) -> i64 {
    if sig == 0 || sig > 64 {
        return -e::EINVAL;
    }
    if old != 0 {
        if !space::user_can(old, 32, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        let now = crate::user::with_current(|program| program.linux_actions[sig]).unwrap_or_default();
        // SAFETY: проверено строкой выше.
        unsafe {
            core::ptr::write_unaligned(old as *mut [u64; 4], [now.handler as u64, now.flags as u64, now.restorer as u64, now.mask]);
        }
    }
    if act != 0 {
        if sig == SIGKILL || sig == SIGSTOP {
            return -e::EINVAL;
        }
        if !space::user_can(act, 32, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let [handler, flags, restorer, mask] = unsafe { core::ptr::read_unaligned(act as *const [u64; 4]) };
        let action = LinuxAction { handler: handler as usize, flags: flags as usize, restorer: restorer as usize, mask };
        crate::user::with_current(|program| program.linux_actions[sig] = action);
    }
    0
}

/// `rt_sigprocmask(how, set, oset, size)` — маска потока.
pub fn sigprocmask(how: usize, set: usize, old: usize, _size: usize) -> i64 {
    const SIG_BLOCK: usize = 0;
    const SIG_UNBLOCK: usize = 1;
    const SIG_SETMASK: usize = 2;
    let slot = sched::current_slot();
    let now = LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    if old != 0 {
        if !space::user_can(old, 8, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::write_unaligned(old as *mut u64, now) };
    }
    if set != 0 {
        if !space::user_can(set, 8, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let value = unsafe { core::ptr::read_unaligned(set as *const u64) };
        let next = match how {
            SIG_BLOCK => now | value,
            SIG_UNBLOCK => now & !value,
            SIG_SETMASK => value,
            _ => return -e::EINVAL,
        };
        LINUX_SIGMASK[slot].store(next & !UNBLOCKABLE, Ordering::Relaxed);
    }
    0
}

/// `sigaltstack(ss, oss)`: `stack_t` — адрес, флаги, размер.
pub fn sigaltstack(new: usize, old: usize) -> i64 {
    const SS_DISABLE: u32 = 2;
    let slot = sched::current_slot();
    if old != 0 {
        if !space::user_can(old, 24, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        let (sp, size) = (LINUX_ALTSTACK[slot].0.load(Ordering::Relaxed), LINUX_ALTSTACK[slot].1.load(Ordering::Relaxed));
        let flags = if size == 0 { SS_DISABLE } else { 0 };
        // SAFETY: проверено строкой выше.
        unsafe {
            core::ptr::write_unaligned(old as *mut u64, sp as u64);
            core::ptr::write_unaligned((old + 8) as *mut u64, u64::from(flags));
            core::ptr::write_unaligned((old + 16) as *mut u64, size as u64);
        }
    }
    if new != 0 {
        if !space::user_can(new, 24, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let (sp, flags, size) = unsafe {
            (
                core::ptr::read_unaligned(new as *const u64),
                core::ptr::read_unaligned((new + 8) as *const u32),
                core::ptr::read_unaligned((new + 16) as *const u64),
            )
        };
        let (sp, size) = if flags & SS_DISABLE != 0 { (0, 0) } else { (sp as usize, size as usize) };
        LINUX_ALTSTACK[slot].0.store(sp, Ordering::Relaxed);
        LINUX_ALTSTACK[slot].1.store(size, Ordering::Relaxed);
    }
    0
}

/// `kill(pid, sig)`. Себе — как `raise`; другому процессу (фаза 71d) — по
/// его обработчику: есть — сигнал ждёт его главный поток на выходе из
/// вызова, нет — процесс снимается (или сигнал пропускается, если его
/// действие по умолчанию — ничего). Группы процессов (`pid` 0 и
/// отрицательные) — как сам процесс: группа у каждого своя.
pub fn kill(pid: usize, sig: usize) -> i64 {
    let own = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
    let pid = pid as i32;
    let target = if pid == 0 || pid == -1 { own } else { pid.unsigned_abs() };
    if target == own {
        return raise_on(sched::current_slot(), sig);
    }
    if sig > 64 {
        return -e::EINVAL;
    }
    let me = crate::user::credentials();
    // Ребёнок, только что рождённый `fork`, может ещё не дойти до своей
    // задачи — в таблице программ его пока нет. Записан — значит будет.
    let mut found = find_process(target);
    for _ in 0..1000 {
        if found.is_some() || !super::process::is_starting(target) {
            break;
        }
        sched::sleep_ms(1);
        found = find_process(target);
    }
    match found {
        None => -e::ESRCH,
        Some((_, process)) => {
            let owner = process.lock().credentials();
            if !me.is_root() && owner.uid != me.uid {
                return -e::EPERM;
            }
            drop(process);
            if sig != 0 {
                send_to_process(target, sig, false);
            }
            0
        }
    }
}

/// Главный поток процесса Linux `pid`: слот и процесс.
fn find_process(pid: u32) -> Option<(usize, alloc::sync::Arc<crate::sync::Mutex<crate::user::Program>>)> {
    let (slot, state) = sched::lookup(sched::TaskId::new(pid))?;
    if state == sched::TaskState::Finished {
        return None;
    }
    let process = crate::user::process_at(slot)?;
    let same = process.lock().linux_pid == pid;
    same.then_some((slot, process))
}

/// Послать сигнал процессу `pid` (не себе). `only_handled` — только если у
/// него есть обработчик (`SIGCHLD` родителю: без обработчика он ничего не
/// значит и не должен прерывать его `poll`).
pub fn send_to_process(pid: u32, sig: usize, only_handled: bool) {
    let Some((slot, process)) = find_process(pid) else {
        return;
    };
    let id = sched::TaskId::new(pid);
    let handler = process.lock().linux_actions[sig].handler;
    match handler {
        SIG_IGN if sig != SIGKILL => {}
        0 if only_handled => {}
        // Остановки нет — нечем и останавливать; продолжение — тем более.
        0 if ignored_by_default(sig) || (SIGSTOP..=22).contains(&sig) => {}
        0 => {
            {
                let mut program = process.lock();
                program.linux_signal = Some(sig as u8);
                program.kill_requested = true;
            }
            drop(process);
            crate::kprintln!("  linux       : process #{pid} killed by signal {sig}");
            sched::wake_input();
            sched::wake(id);
        }
        _ => {
            drop(process);
            LINUX_PENDING[slot].fetch_or(bit(sig), Ordering::Relaxed);
            sched::wake(id);
        }
    }
}

/// `tgkill(tgid, tid, sig)` / `tkill(tid, sig)`.
pub fn tgkill(_tgid: usize, tid: usize, sig: usize) -> i64 {
    let id = sched::TaskId::new(tid as u32);
    if id == sched::current() {
        return raise_on(sched::current_slot(), sig);
    }
    // Поток того же процесса — пометить сигнал и разбудить: он увидит его на
    // выходе из вызова, в котором ждёт.
    let Some((slot, _)) = sched::lookup(id) else {
        return -e::ESRCH;
    };
    let same = crate::user::same_process(slot);
    if !same {
        return -e::ESRCH;
    }
    let result = raise_on(slot, sig);
    sched::wake(id);
    result
}

fn raise_on(slot: usize, sig: usize) -> i64 {
    if sig == 0 {
        return 0;
    }
    if sig > 64 {
        return -e::EINVAL;
    }
    LINUX_PENDING[slot].fetch_or(bit(sig), Ordering::Relaxed);
    0
}

/// Ждёт ли поток незаблокированный сигнал — для `poll` и долгих ожиданий.
pub fn pending() -> bool {
    let slot = sched::current_slot();
    LINUX_PENDING[slot].load(Ordering::Relaxed) & !LINUX_SIGMASK[slot].load(Ordering::Relaxed) != 0
}

/// Действие по умолчанию — пропустить.
fn ignored_by_default(sig: usize) -> bool {
    matches!(sig, 17 | 18 | 23 | 28)
}

// ---- ожидание сигнала ----------------------------------------------------------

/// Маска, которую вернёт `rt_sigreturn` после обработчика сигнала, прервавшего
/// `rt_sigsuspend`: пока поток ждал, действовала временная. [`NO_MASK`] — нет
/// такой (маска без `SIGKILL` и `SIGSTOP` никогда не бывает полной).
static SUSPENDED_MASK: [AtomicU64; sched::MAX_TASKS] = [const { AtomicU64::new(NO_MASK) }; sched::MAX_TASKS];
const NO_MASK: u64 = u64::MAX;

/// Выбросить пришедшие незаблокированные сигналы, которые ничего не делают
/// (`SIG_IGN` и пропускаемые по умолчанию): ожидание сигнала их не замечает.
fn drop_ignored(slot: usize) {
    let ready = LINUX_PENDING[slot].load(Ordering::Relaxed) & !LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    if ready == 0 {
        return;
    }
    let ignored = crate::user::with_current(|program| {
        (1..=64)
            .filter(|&sig| ready & bit(sig) != 0)
            .filter(|&sig| match program.linux_actions[sig].handler {
                SIG_IGN => true,
                0 => ignored_by_default(sig),
                _ => false,
            })
            .fold(0, |set, sig| set | bit(sig))
    })
    .unwrap_or(0);
    LINUX_PENDING[slot].fetch_and(!ignored, Ordering::Relaxed);
}

/// Ждать, пока `ready` не ответит `Some` или не настанет `until`; между
/// проверками — сон до пробуждения (его шлёт `kill`), не дольше 100 мс.
/// Снятие процесса — `None`.
fn wait_for<T>(until: Option<u64>, mut ready: impl FnMut() -> Option<T>) -> Option<T> {
    loop {
        let seen = crate::io_events::generation();
        if let Some(found) = ready() {
            return Some(found);
        }
        let now = crate::time::uptime_ms();
        if crate::user::kill_pending() || until.is_some_and(|at| now >= at) {
            return None;
        }
        let nap = until.map_or(now + 100, |at| at.min(now + 100));
        crate::io_events::wait(seen, Some(nap));
    }
}

/// Прочитать `sigset_t` ядра (64 бита) из памяти программы.
fn read_set(set: usize, size: usize) -> Result<u64, i64> {
    if size != 8 {
        return Err(-e::EINVAL);
    }
    if !space::user_can(set, 8, PageFlags::READ) {
        return Err(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше.
    Ok(unsafe { core::ptr::read_unaligned(set as *const u64) })
}

/// `rt_sigsuspend(mask, size)`: заменить маску на `mask` и спать, пока не
/// придёт сигнал с обработчиком (или снимающий процесс). Ответ всегда
/// `-EINTR`; обработчик вызывается на выходе из этого же вызова, а прежнюю
/// маску ему в `ucontext` кладёт [`deliver`] — после обработчика действует она.
pub fn sigsuspend(mask: usize, size: usize) -> i64 {
    match read_set(mask, size) {
        Ok(set) => suspend(set),
        Err(err) => err,
    }
}

/// `pause()` — `rt_sigsuspend` с той же маской.
pub fn pause() -> i64 {
    suspend(LINUX_SIGMASK[sched::current_slot()].load(Ordering::Relaxed))
}

fn suspend(set: u64) -> i64 {
    let slot = sched::current_slot();
    let old = LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    LINUX_SIGMASK[slot].store(set & !UNBLOCKABLE, Ordering::Relaxed);
    let woke = wait_for(None, || {
        drop_ignored(slot);
        pending().then_some(())
    });
    if woke.is_some() {
        SUSPENDED_MASK[slot].store(old, Ordering::Relaxed);
    } else {
        LINUX_SIGMASK[slot].store(old, Ordering::Relaxed);
    }
    -e::EINTR
}

/// `rt_sigpending(set, size)`: пришедшие, но заблокированные сигналы.
pub fn sigpending(set: usize, size: usize) -> i64 {
    if size != 8 {
        return -e::EINVAL;
    }
    if !space::user_can(set, 8, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let slot = sched::current_slot();
    let waiting = LINUX_PENDING[slot].load(Ordering::Relaxed) & LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::write_unaligned(set as *mut u64, waiting) };
    0
}

/// `rt_sigtimedwait(set, info, timeout, size)`: забрать сигнал из `set`, не
/// вызывая обработчик, — ответ его номер. Не пришёл до `timeout` (`NULL` —
/// ждать сколько угодно) — `-EAGAIN`; пришёл другой, с обработчиком, —
/// `-EINTR`. Ждать так можно только заблокированные сигналы: незаблокированный
/// ушёл бы обработчику раньше, — это забота программы, как и у Linux.
pub fn sigtimedwait(set: usize, info: usize, timeout: usize, size: usize) -> i64 {
    let wanted = match read_set(set, size) {
        Ok(wanted) => wanted & !UNBLOCKABLE,
        Err(err) => return err,
    };
    if info != 0 && !space::user_can(info, SIGINFO, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let until = if timeout == 0 {
        None
    } else {
        if !space::user_can(timeout, 16, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let [seconds, nanos] = unsafe { core::ptr::read_unaligned(timeout as *const [i64; 2]) };
        if seconds < 0 || !(0..1_000_000_000).contains(&nanos) {
            return -e::EINVAL;
        }
        let ms = (seconds as u64).saturating_mul(1000).saturating_add((nanos as u64).div_ceil(1_000_000));
        Some(crate::time::uptime_ms().saturating_add(ms))
    };
    let slot = sched::current_slot();
    enum Woke {
        Taken(usize),
        Other,
    }
    let woke = wait_for(until, || {
        let taken = LINUX_PENDING[slot].load(Ordering::Relaxed) & wanted;
        if taken != 0 {
            let sig = taken.trailing_zeros() as usize + 1;
            LINUX_PENDING[slot].fetch_and(!bit(sig), Ordering::Relaxed);
            return Some(Woke::Taken(sig));
        }
        drop_ignored(slot);
        pending().then_some(Woke::Other)
    });
    match woke {
        Some(Woke::Taken(sig)) => {
            if info != 0 {
                let pid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
                // SAFETY: проверено в начале вызова.
                unsafe {
                    core::ptr::write_bytes(info as *mut u8, 0, SIGINFO);
                    core::ptr::write_unaligned(info as *mut i32, sig as i32);
                    core::ptr::write_unaligned((info + 8) as *mut i32, -6);
                    core::ptr::write_unaligned((info + 16) as *mut u32, pid);
                }
            }
            sig as i64
        }
        Some(Woke::Other) => -e::EINTR,
        None if crate::user::kill_pending() => -e::EINTR,
        None => -e::EAGAIN,
    }
}

/// Размеры кадра сигнала: `struct ucontext` ядра x86-64 и `siginfo_t`.
const UCONTEXT: usize = 304;
const SIGINFO: usize = 128;
/// Смещение `uc_mcontext` в `ucontext` и `uc_sigmask` за ним.
const MCONTEXT_AT: usize = 40;
const SIGMASK_AT: usize = 296;

/// Доставить пришедший незаблокированный сигнал: кадр на стеке программы и
/// вход в обработчик.
pub fn deliver(frame: &mut crate::arch::TrapFrame) {
    let slot = sched::current_slot();
    let mask = LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    // После `rt_sigsuspend` сигнал выбирается по временной маске, а
    // обработчику достаётся прежняя.
    let suspended = SUSPENDED_MASK[slot].swap(NO_MASK, Ordering::Relaxed);
    let ready = LINUX_PENDING[slot].load(Ordering::Relaxed) & !mask;
    if ready == 0 {
        if suspended != NO_MASK {
            LINUX_SIGMASK[slot].store(suspended, Ordering::Relaxed);
        }
        return;
    }
    let sig = ready.trailing_zeros() as usize + 1;
    LINUX_PENDING[slot].fetch_and(!bit(sig), Ordering::Relaxed);
    let action = crate::user::with_current(|program| program.linux_actions[sig]).unwrap_or_default();
    let (mask, handled_mask) = if suspended == NO_MASK { (mask, mask) } else { (suspended, mask) };
    if action.handler == 0 || action.handler == SIG_IGN {
        LINUX_SIGMASK[slot].store(mask, Ordering::Relaxed);
    }
    match action.handler {
        0 if ignored_by_default(sig) => {}
        0 => {
            crate::kprintln!("  linux       : killed by signal {sig}");
            crate::user::with_current(|program| program.linux_signal = Some(sig as u8));
            // SAFETY: из системного вызова третьего кольца.
            unsafe { crate::arch::return_to_kernel(128 + sig as i64) }
        }
        SIG_IGN => {}
        handler => enter_handler(frame, slot, sig, action, handler, mask, handled_mask),
    }
}

/// `mask` — что вернёт `rt_sigreturn`, `during` — к чему на время обработчика
/// добавятся его `sa_mask` и сам сигнал (у `rt_sigsuspend` они разные).
fn enter_handler(
    frame: &mut crate::arch::TrapFrame,
    slot: usize,
    sig: usize,
    action: LinuxAction,
    handler: usize,
    mask: u64,
    during: u64,
) {
    let (alt_sp, alt_size) = (LINUX_ALTSTACK[slot].0.load(Ordering::Relaxed), LINUX_ALTSTACK[slot].1.load(Ordering::Relaxed));
    let user_sp = frame.rsp as usize;
    let on_alt = alt_size != 0 && user_sp > alt_sp && user_sp <= alt_sp + alt_size;
    let base = if action.flags & SA_ONSTACK != 0 && alt_size != 0 && !on_alt { alt_sp + alt_size } else { user_sp - 128 };
    let total = 8 + UCONTEXT + SIGINFO;
    // После «call» обработчика `rsp + 8` кратен 16.
    let sp = ((base - total) & !15) - 8;
    if !space::user_can(sp, total, PageFlags::WRITE) {
        crate::kprintln!("  linux       : no room for a signal frame, killed by signal {sig}");
        // SAFETY: из системного вызова третьего кольца.
        unsafe { crate::arch::return_to_kernel(128 + sig as i64) }
    }
    let uc = sp + 8;
    let info = uc + UCONTEXT;
    let f = frame;
    let gregs: [u64; 18] = [
        f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx, f.rsp,
        f.rip, f.rflags,
    ];
    let pid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
    // SAFETY: весь кадр проверен на запись выше.
    unsafe {
        core::ptr::write_bytes(sp as *mut u8, 0, total);
        core::ptr::write_unaligned(sp as *mut u64, action.restorer as u64);
        // uc_stack: ss_sp, ss_flags, ss_size.
        core::ptr::write_unaligned((uc + 16) as *mut u64, alt_sp as u64);
        core::ptr::write_unaligned((uc + 32) as *mut u64, alt_size as u64);
        core::ptr::write_unaligned((uc + MCONTEXT_AT) as *mut [u64; 18], gregs);
        // cs, gs, fs, ss — по 16 бит.
        let csgsfs = (f.cs & 0xFFFF) | ((f.ss & 0xFFFF) << 48);
        core::ptr::write_unaligned((uc + MCONTEXT_AT + 144) as *mut u64, csgsfs);
        core::ptr::write_unaligned((uc + SIGMASK_AT) as *mut u64, mask);
        // siginfo: si_signo, si_errno, si_code (SI_TKILL = -6), pid, uid.
        core::ptr::write_unaligned(info as *mut i32, sig as i32);
        core::ptr::write_unaligned((info + 8) as *mut i32, -6);
        core::ptr::write_unaligned((info + 16) as *mut u32, pid);
    }
    let mut next = during | action.mask;
    if action.flags & SA_NODEFER == 0 {
        next |= bit(sig);
    }
    LINUX_SIGMASK[slot].store(next & !UNBLOCKABLE, Ordering::Relaxed);
    if action.flags & SA_RESETHAND != 0 {
        crate::user::with_current(|program| program.linux_actions[sig] = LinuxAction::default());
    }
    f.rip = handler as u64;
    f.rsp = sp as u64;
    f.rdi = sig as u64;
    f.rsi = info as u64;
    f.rdx = uc as u64;
    f.rax = 0;
    // Флаг направления — сброшен, как требует System V на входе в функцию.
    f.rflags &= !0x400;
}

/// `rt_sigreturn`: прерванные регистры и маска — из `ucontext` на стеке.
pub fn sigreturn(frame: &mut crate::arch::TrapFrame) {
    // `ret` обработчика снял адрес `sa_restorer`: `rsp` указывает на ucontext.
    let uc = frame.rsp as usize;
    if !space::user_can(uc, UCONTEXT, PageFlags::READ) {
        crate::kprintln!("  linux       : rt_sigreturn with a bad frame");
        // SAFETY: из системного вызова третьего кольца.
        unsafe { crate::arch::return_to_kernel(128 + 11) }
    }
    // SAFETY: кадр проверен на чтение выше.
    let (gregs, mask) = unsafe {
        (
            core::ptr::read_unaligned((uc + MCONTEXT_AT) as *const [u64; 18]),
            core::ptr::read_unaligned((uc + SIGMASK_AT) as *const u64),
        )
    };
    let f = frame;
    [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx, f.rsp, f.rip] =
        [gregs[0], gregs[1], gregs[2], gregs[3], gregs[4], gregs[5], gregs[6], gregs[7], gregs[8], gregs[9], gregs[10], gregs[11], gregs[12], gregs[13], gregs[14], gregs[15], gregs[16]];
    // Флаги — только те, что программе можно менять; прерывания разрешены.
    const USER_FLAGS: u64 = 0xCD5;
    f.rflags = (gregs[17] & USER_FLAGS) | 0x202;
    LINUX_SIGMASK[sched::current_slot()].store(mask & !UNBLOCKABLE, Ordering::Relaxed);
    deliver(f);
}
