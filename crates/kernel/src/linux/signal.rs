//! Сигналы программ Linux (фаза 71c): обработчики, маски, доставка на выходе
//! из системного вызова и `rt_sigreturn`; ожидание сигнала — `rt_sigsuspend`,
//! `pause`, `rt_sigtimedwait`.
//!
//! # Когда сигнал доставляется
//!
//! На выходе из системного вызова потока — `raise`, `kill` себе,
//! `sigprocmask`, снявший блокировку, и вызов, прерванный `tgkill` соседнего
//! потока, — и (12.11) на возврате в программу из прерывания
//! ([`deliver_async`]): поток, крутящийся в своём коде, получает сигнал не
//! позже следующего тика. В кадр кладётся состояние x87/SSE (`FXSAVE`).
//! Отказы процессора — сигналами по обработчикам программы ([`deliver_fault`]).

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

/// Селекторы третьего кольца — как у Linux x86-64 (`__USER_CS`,
/// `__USER32_CS`, `__USER_DS`).
const USER_CS64: u16 = 0x33;
const USER_CS32: u16 = 0x23;
const USER_SS: u16 = 0x2b;

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
            // Как у Linux: сигнал процессу получает поток, который его не
            // заблокировал (фаза 74). .NET держит `SIGCHLD` заблокированным
            // не во всех потоках; отдать его главному с маской — значит, что
            // обработчик не запустится никогда, и `Process.WaitForExit` ждёт
            // вечно (`dotnet new` вставал после запуска `chmod`).
            let threads = crate::user::slots_of(&process);
            drop(process);
            let open = |slot: usize| LINUX_SIGMASK[slot].load(Ordering::Relaxed) & bit(sig) == 0;
            let target = if open(slot) { slot } else { threads.iter().copied().find(|&other| open(other)).unwrap_or(slot) };
            LINUX_PENDING[target].fetch_or(bit(sig), Ordering::Relaxed);
            match sched::id_of_slot(target) {
                Some(thread) if target != slot => sched::wake(thread),
                _ => sched::wake(id),
            }
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
            crate::kprintln!(
                "  linux       : {} killed by signal {sig} at rip {:#x}",
                sched::current(),
                frame.rip
            );
            crate::user::with_current(|program| program.linux_signal = Some(sig as u8));
            // SAFETY: из системного вызова третьего кольца.
            unsafe { crate::arch::return_to_kernel(128 + sig as i64) }
        }
        SIG_IGN => {}
        handler => enter_handler(frame, slot, sig, action, handler, mask, handled_mask, None),
    }
}

/// Доставка посреди кода (12.11): на возврате в третье кольцо из
/// **прерывания** — тик таймера застаёт поток, крутящийся в своём коде, не
/// позже чем через миллисекунду. Так Wine приостанавливает чужие потоки
/// (`SuspendThread`, `GetThreadContext`: `SIGUSR1` через `tgkill`, обработчик
/// пишет контекст и ждёт), а .NET и Java останавливают потоки для сборки.
///
/// Векторные регистры в процессоре — программы: ядро SSE не трогает, а
/// возврат из [`crate::irq::on_trap_return`] — уже на этом потоке, после
/// возможного переключения, — их вернул. Обработчик получает их в кадре, как
/// и при доставке из вызова.
pub fn deliver_async(frame: &mut crate::arch::TrapFrame) {
    let slot = sched::current_slot();
    if LINUX_PENDING[slot].load(Ordering::Relaxed) & !LINUX_SIGMASK[slot].load(Ordering::Relaxed) == 0 {
        return;
    }
    // Только у программы Linux: у родной своих сигналов нет.
    if !crate::user::with_current(|program| program.linux_pid != 0).unwrap_or(false) {
        return;
    }
    deliver(frame);
}

/// Отказ процессора — для кадра сигнала: `si_code`, `si_addr` и то, что Linux
/// кладёт в `mcontext` (`err`, `trapno`, `cr2`).
#[derive(Clone, Copy)]
struct Fault {
    code: i32,
    addr: u64,
    trapno: u64,
    err: u64,
}

/// Отказ процессора в программе Linux — сигналом, как у Linux: `SIGSEGV` на
/// отказ страницы и общую защиту, `SIGFPE` на деление, `SIGILL` на
/// недопустимую инструкцию, `SIGTRAP` на точку останова, `SIGBUS` на
/// выравнивание. `true` — кадр построен, и возврат из исключения войдёт в
/// обработчик; `false` — обработчика нет или сигнал заблокирован, и программу
/// снимают, как раньше.
///
/// # Зачем
///
/// Java (HotSpot) держится на этом: проверки на `null` в скомпилированном
/// коде — это чтение по нулевому адресу и `SIGSEGV`, деление на ноль — `SIGFPE`,
/// остановка потоков — чтение защищённой страницы. Первое, что она делает при
/// запуске, — нарочно читает нулевой адрес и ждёт свой обработчик.
pub fn deliver_fault(frame: &mut crate::arch::TrapFrame, vector: u8, addr: usize) -> bool {
    const SIGILL: usize = 4;
    const SIGTRAP: usize = 5;
    const SIGBUS: usize = 7;
    const SIGFPE: usize = 8;
    const SIGSEGV: usize = 11;
    /// Бит «страница присутствует» в коде ошибки #PF.
    const PF_PRESENT: u64 = 1;
    let (sig, code, at) = match vector {
        // FPE_INTDIV.
        0 => (SIGFPE, 1, frame.rip),
        // TRAP_BRKPT.
        3 => (SIGTRAP, 1, 0),
        // ILL_ILLOPN.
        6 => (SIGILL, 2, frame.rip),
        // SI_KERNEL: у общей защиты адреса нет.
        13 => (SIGSEGV, 0x80, 0),
        // SEGV_ACCERR, если страница есть, иначе SEGV_MAPERR.
        14 => (SIGSEGV, if frame.error & PF_PRESENT != 0 { 2 } else { 1 }, addr as u64),
        // FPE_FLTINV — x87 и SSE.
        16 | 19 => (SIGFPE, 7, frame.rip),
        // BUS_ADRALN.
        17 => (SIGBUS, 1, addr as u64),
        _ => return false,
    };
    // Программа Linux — по своему `rt_sigaction`; родная — по
    // `SYS_SIGACTION` (09.11.2026, JIT SpiderMonkey). Таблица одна.
    let Some(action) = crate::user::with_current(|program| program.linux_actions[sig]) else {
        return false;
    };
    let slot = sched::current_slot();
    let mask = LINUX_SIGMASK[slot].load(Ordering::Relaxed);
    if action.handler == 0 || action.handler == SIG_IGN || mask & bit(sig) != 0 {
        return false;
    }
    // Переход по нулевому адресу (фаза 71j: Wine молча глотает такие сбои в
    // своём коде Unix) — в журнал с адресом возврата с вершины стека: по нему
    // видно, кто позвал пустой указатель. Первые двадцать на систему.
    if sig == SIGSEGV && frame.rip < 0x1_0000 {
        static SHOWN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if SHOWN.fetch_add(1, Ordering::Relaxed) < 20 {
            let rsp = frame.rsp as usize;
            let back = if crate::user::space::user_can(rsp, 8, crate::mm::PageFlags::READ) {
                // SAFETY: проверено строкой выше; пространство — текущей программы.
                unsafe { core::ptr::read_unaligned(rsp as *const u64) }
            } else {
                0
            };
            let place = crate::user::with_current(|program| program.image_at(back as usize)).flatten();
            crate::kprintln!(
                "  linux       : {} jumped to {:#x} (fault at {:#x}), return address {back:#x} (file offset, size: {place:x?}), rdi {:#x}",
                sched::current(),
                frame.rip,
                at,
                frame.rdi
            );
        }
    }
    // Отказ страницы с адресом в программе (фаза 71j) — что за область там.
    if vector == 14 && frame.rip >= 0x1_0000 {
        static SHOWN_PF: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        if SHOWN_PF.fetch_add(1, Ordering::Relaxed) < 20 {
            let what = crate::user::with_current(|program| program.describe_region(addr)).unwrap_or_default();
            crate::kprintln!(
                "  linux       : {} page fault at {addr:#x} (error {:#x}) from {:#x}: {what}",
                sched::current(),
                frame.error,
                frame.rip
            );
        }
    }
    let fault = Fault { code, addr: at, trapno: u64::from(vector), err: frame.error };
    enter_handler(frame, slot, sig, action, action.handler, mask, mask, Some(fault));
    true
}

/// Область `FXSAVE` в кадре сигнала — `uc_mcontext.fpregs` у Linux.
const FPSTATE: usize = 512;
/// Смещение `fpregs` в `mcontext`: за 23 регистрами.
const FPREGS_AT: usize = MCONTEXT_AT + 23 * 8;

/// Сохранить x87 и SSE программы в её память.
///
/// Регистры процессора сейчас — её: ядро SSE не трогает, а планировщик
/// сохраняет и возвращает их на каждом переключении (`arch::fpu`).
///
/// # Safety
///
/// `area` выровнен на 16 и проверен на запись.
unsafe fn save_fpu(area: usize) {
    if let Some((mask, size)) = signal_xsave() {
        // Полное состояние (AVX и выше) — `XSAVE`, как Linux (12.11): без
        // верхних половин `ymm` обработчик, тронувший AVX, портил прерванный
        // `memcpy` glibc — куча wineserver: «corrupted double-linked list».
        // SAFETY: контракт функции; область выровнена на 64 и длиной
        // `size + 4` (см. [`fp_size`]).
        unsafe {
            core::arch::asm!(
                "xsave64 [{}]",
                in(reg) area,
                in("eax") mask as u32,
                in("edx") (mask >> 32) as u32,
                options(nostack, preserves_flags),
            );
            // `fx_sw_reserved` Linux (байты 464..): MAGIC1, extended_size,
            // xfeatures, xstate_size; за областью — MAGIC2.
            core::ptr::write_unaligned((area + 464) as *mut u32, FP_XSTATE_MAGIC1);
            core::ptr::write_unaligned((area + 468) as *mut u32, (size + 4) as u32);
            core::ptr::write_unaligned((area + 472) as *mut u64, mask);
            core::ptr::write_unaligned((area + 480) as *mut u32, size as u32);
            core::ptr::write_unaligned((area + size) as *mut u32, FP_XSTATE_MAGIC2);
        }
        return;
    }
    // SAFETY: контракт функции.
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) area, options(nostack, preserves_flags)) };
}

/// Полный `XSAVE` в кадре сигнала (13.11): проверка — не он ли ломает Wine
/// (`mov rax,[rax+0x2f0]` с нулём сразу после обработчика).
const SIGNAL_XSAVE: bool = false;

fn signal_xsave() -> Option<(u64, usize)> {
    if SIGNAL_XSAVE { crate::arch::fpu::signal_xsave() } else { None }
}

const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;

/// Место под состояние в кадре сигнала: `XSAVE` и MAGIC2 — или `FXSAVE`.
fn fp_size() -> usize {
    signal_xsave().map_or(FPSTATE, |(_, size)| size + 4)
}

/// Вернуть полное состояние (`XRSTOR`) из кадра, если он наш (`MAGIC1/2`);
/// `false` — не вышло, тогда хватит `FXRSTOR`.
fn restore_xstate(area: usize) -> bool {
    let Some((mask, size)) = signal_xsave() else { return false };
    if area % 64 != 0 || size > 4096 || !space::user_can(area, size + 4, PageFlags::READ) {
        return false;
    }
    // SAFETY: проверено строкой выше.
    let (magic1, magic2) = unsafe {
        (core::ptr::read_unaligned((area + 464) as *const u32), core::ptr::read_unaligned((area + size) as *const u32))
    };
    if magic1 != FP_XSTATE_MAGIC1 || magic2 != FP_XSTATE_MAGIC2 {
        return false;
    }
    let mut buffer = [0u8; 4096 + 64];
    let skew = buffer.as_ptr().align_offset(64);
    let copy = &mut buffer[skew..skew + size];
    // SAFETY: проверено выше.
    unsafe { core::ptr::copy_nonoverlapping(area as *const u8, copy.as_mut_ptr(), size) };
    let field = |copy: &[u8], at: usize| u32::from_le_bytes(copy[at..at + 4].try_into().unwrap_or([0; 4]));
    let allowed = match field(copy, 28) {
        0 => 0xFFBF,
        m => m,
    };
    let mxcsr = field(copy, 24) & allowed;
    copy[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    // Заголовок XSAVE: только включённые компоненты, стандартный формат,
    // зарезервированное — нули (иначе `XRSTOR` — общая защита в ядре).
    let bv = u64::from_le_bytes(copy[512..520].try_into().unwrap_or([0; 8])) & mask;
    copy[512..520].copy_from_slice(&bv.to_le_bytes());
    copy[520..576].fill(0);
    // SAFETY: своя область, выровнена на 64, заголовок проверен.
    unsafe {
        core::arch::asm!(
            "xrstor64 [{}]",
            in(reg) copy.as_ptr(),
            in("eax") mask as u32,
            in("edx") (mask >> 32) as u32,
            options(nostack, preserves_flags),
        );
    }
    true
}

/// Вернуть x87 и SSE из кадра сигнала — через копию в ядре, с проверенным
/// `MXCSR`: зарезервированный бит в нём — общая защита в самом ядре.
fn restore_fpu(area: usize) {
    if area == 0 || area % 16 != 0 || !space::user_can(area, FPSTATE, PageFlags::READ) {
        return;
    }
    if restore_xstate(area) {
        return;
    }
    // Выравнивание — вручную, внутри буфера с запасом. `align(16)` у
    // переменной на стеке здесь не работает: стек ядра в ловушке выровнен на
    // восемь, а компилятор считает его выровненным на шестнадцать — и
    // `fxrstor64` по адресу `…e8` был общей защитой в нулевом кольце.
    let mut buffer = [0u8; FPSTATE + 16];
    let skew = buffer.as_ptr().align_offset(16);
    let copy = &mut buffer[skew..skew + FPSTATE];
    // SAFETY: проверено выше.
    unsafe { core::ptr::copy_nonoverlapping(area as *const u8, copy.as_mut_ptr(), FPSTATE) };
    let field = |copy: &[u8], at: usize| u32::from_le_bytes(copy[at..at + 4].try_into().unwrap_or([0; 4]));
    // `MXCSR_MASK` (смещение 28) из `fxsave` этого же процессора; ноль значит
    // маску по умолчанию.
    let allowed = match field(copy, 28) {
        0 => 0xFFBF,
        mask => mask,
    };
    let mxcsr = field(copy, 24) & allowed;
    copy[24..28].copy_from_slice(&mxcsr.to_le_bytes());
    // SAFETY: область своя, выровнена на 16, `MXCSR` без зарезервированных битов.
    unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) copy.as_ptr(), options(nostack, preserves_flags)) };
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
    fault: Option<Fault>,
) {
    let (alt_sp, alt_size) = (LINUX_ALTSTACK[slot].0.load(Ordering::Relaxed), LINUX_ALTSTACK[slot].1.load(Ordering::Relaxed));
    let user_sp = frame.rsp as usize;
    let on_alt = alt_size != 0 && user_sp > alt_sp && user_sp <= alt_sp + alt_size;
    let base = if action.flags & SA_ONSTACK != 0 && alt_size != 0 && !on_alt { alt_sp + alt_size } else { user_sp - 128 };
    // Над кадром — область `FXSAVE`, выровненная на 16: обработчик вправе
    // испортить регистры SSE, а прерванный код (скомпилированная Java) их
    // ждёт обратно.
    // С `XSAVE` — полное состояние, выровненное на 64 (12.11).
    let fp_bytes = fp_size();
    let fpstate = (base - fp_bytes) & !63;
    let total = 8 + UCONTEXT + SIGINFO;
    // После «call» обработчика `rsp + 8` кратен 16.
    let sp = ((fpstate - total) & !15) - 8;
    if !space::user_can(sp, fpstate + fp_bytes - sp, PageFlags::WRITE) {
        crate::kprintln!("  linux       : no room for a signal frame, killed by signal {sig}");
        // SAFETY: из системного вызова или исключения третьего кольца.
        unsafe { crate::arch::return_to_kernel(128 + sig as i64) }
    }
    // SAFETY: выровнено и проверено на запись строкой выше.
    unsafe { save_fpu(fpstate) };
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
        // uc_flags: UC_FP_XSTATE — в fpregs полное состояние XSAVE.
        if signal_xsave().is_some() {
            core::ptr::write_unaligned(uc as *mut u64, 1);
        }
        // uc_stack: ss_sp, ss_flags, ss_size.
        core::ptr::write_unaligned((uc + 16) as *mut u64, alt_sp as u64);
        core::ptr::write_unaligned((uc + 32) as *mut u64, alt_size as u64);
        core::ptr::write_unaligned((uc + MCONTEXT_AT) as *mut [u64; 18], gregs);
        // cs, gs, fs, ss — по 16 бит.
        let csgsfs = (f.cs & 0xFFFF) | ((f.ss & 0xFFFF) << 48);
        core::ptr::write_unaligned((uc + MCONTEXT_AT + 144) as *mut u64, csgsfs);
        core::ptr::write_unaligned((uc + FPREGS_AT) as *mut u64, fpstate as u64);
        core::ptr::write_unaligned((uc + SIGMASK_AT) as *mut u64, mask);
        core::ptr::write_unaligned(info as *mut i32, sig as i32);
        match fault {
            // Отказ: `si_code` и `si_addr`; в `mcontext` — `err`, `trapno`,
            // `cr2` (регистры 19, 20 и 22).
            Some(fault) => {
                core::ptr::write_unaligned((info + 8) as *mut i32, fault.code);
                core::ptr::write_unaligned((info + 16) as *mut u64, fault.addr);
                core::ptr::write_unaligned((uc + MCONTEXT_AT + 19 * 8) as *mut u64, fault.err);
                core::ptr::write_unaligned((uc + MCONTEXT_AT + 20 * 8) as *mut u64, fault.trapno);
                core::ptr::write_unaligned((uc + MCONTEXT_AT + 22 * 8) as *mut u64, fault.addr);
            }
            // Сигнал от процесса: si_code SI_TKILL (-6), pid.
            None => {
                core::ptr::write_unaligned((info + 8) as *mut i32, -6);
                core::ptr::write_unaligned((info + 16) as *mut u32, pid);
            }
        }
    }
    let mut next = during | action.mask;
    if action.flags & SA_NODEFER == 0 {
        next |= bit(sig);
    }
    LINUX_SIGMASK[slot].store(next & !UNBLOCKABLE, Ordering::Relaxed);
    if action.flags & SA_RESETHAND != 0 {
        crate::user::with_current(|program| program.linux_actions[sig] = LinuxAction::default());
    }
    // Обработчик — 64-битный код, даже если сигнал застал поток в 32-битном
    // (Wine WoW64, CS `0x23`): прерванный CS лежит в `uc_mcontext`, и
    // `rt_sigreturn` вернёт его (12.11).
    f.cs = u64::from(USER_CS64);
    f.ss = u64::from(USER_SS);
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
    let (gregs, mask, fpregs, csgsfs) = unsafe {
        (
            core::ptr::read_unaligned((uc + MCONTEXT_AT) as *const [u64; 18]),
            core::ptr::read_unaligned((uc + SIGMASK_AT) as *const u64),
            core::ptr::read_unaligned((uc + FPREGS_AT) as *const u64),
            core::ptr::read_unaligned((uc + MCONTEXT_AT + 144) as *const u64),
        )
    };
    // CS — 64-битный или 32-битный пользовательский (WoW64), иначе 64-битный:
    // другого программе не положено.
    let cs = (csgsfs & 0xFFFF) as u16;
    frame.cs = u64::from(if cs == USER_CS32 { USER_CS32 } else { USER_CS64 });
    frame.ss = u64::from(USER_SS);
    restore_fpu(fpregs as usize);
    let f = frame;
    [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx, f.rsp, f.rip] =
        [gregs[0], gregs[1], gregs[2], gregs[3], gregs[4], gregs[5], gregs[6], gregs[7], gregs[8], gregs[9], gregs[10], gregs[11], gregs[12], gregs[13], gregs[14], gregs[15], gregs[16]];
    // Флаги — только те, что программе можно менять; прерывания разрешены.
    const USER_FLAGS: u64 = 0xCD5;
    f.rflags = (gregs[17] & USER_FLAGS) | 0x202;
    LINUX_SIGMASK[sched::current_slot()].store(mask & !UNBLOCKABLE, Ordering::Relaxed);
    deliver(f);
}
