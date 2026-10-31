//! `poll`, `ppoll` и `pipe` программ Linux (фаза 71c).

use super::e;
use crate::mm::PageFlags;
use crate::user::space;

const POLLIN: u16 = 0x1;
const POLLOUT: u16 = 0x4;
const POLLERR: u16 = 0x8;
const POLLHUP: u16 = 0x10;
const POLLNVAL: u16 = 0x20;

/// `poll(fds, n, timeout_ms)`; `struct pollfd` — `int fd; short events; short
/// revents`. Готовность каждого дескриптора спрашивается у того же, что
/// отвечает родному `poll` (`readiness_of`).
pub fn poll(fds: usize, count: usize, timeout_ms: i64) -> i64 {
    let deadline = if timeout_ms < 0 { None } else { Some(crate::time::uptime_ms() + timeout_ms as u64) };
    wait(fds, count, deadline)
}

/// `ppoll(fds, n, tmo, sigmask)`: срок — `timespec`, `NULL` — без срока.
pub fn ppoll(fds: usize, count: usize, timeout: usize) -> i64 {
    let deadline = if timeout == 0 {
        None
    } else {
        if !space::user_can(timeout, 16, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let [seconds, nanos] = unsafe { core::ptr::read_unaligned(timeout as *const [u64; 2]) };
        Some(crate::time::uptime_ms() + seconds.saturating_mul(1000) + nanos.div_ceil(1_000_000))
    };
    wait(fds, count, deadline)
}

fn wait(fds: usize, count: usize, deadline: Option<u64>) -> i64 {
    if count > 4096 || !space::user_can(fds, count * 8, PageFlags::WRITE) {
        return -e::EINVAL;
    }
    loop {
        let seen = crate::io_events::generation();
        // Заново на каждой попытке (фаза 71h): пока мы спали, другой поток
        // мог отдать или защитить эту память, а пишет ядро своими правами.
        if !space::user_can(fds, count * 8, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        let mut ready = 0;
        for index in 0..count {
            let at = fds + index * 8;
            // SAFETY: массив проверен на запись выше.
            let (fd, events) = unsafe { (core::ptr::read_unaligned(at as *const i32), core::ptr::read_unaligned((at + 4) as *const u16)) };
            let revents = if fd < 0 { 0 } else { revents_of(fd as usize, events) };
            // SAFETY: тот же проверенный массив.
            unsafe { core::ptr::write_unaligned((at + 6) as *mut u16, revents) };
            if revents != 0 {
                ready += 1;
                let pid = u64::from(crate::user::with_current(|program| program.linux_pid).unwrap_or(0));
                super::profile::POLL_LAST.store(
                    pid << 48 | (fd as u64 & 0xffff) << 32 | u64::from(events) << 16 | u64::from(revents),
                    core::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        if ready > 0 {
            return ready;
        }
        if deadline.is_some_and(|deadline| crate::time::uptime_ms() >= deadline) {
            return 0;
        }
        if crate::user::kill_pending() || super::signal::pending() {
            return -e::EINTR;
        }
        let mut until = crate::time::uptime_ms() + 100;
        if let Some(deadline) = deadline {
            until = until.min(deadline);
        }
        // `timerfd` становится готов сам, по времени (фаза 71e).
        for index in 0..count {
            // SAFETY: массив проверен на запись выше.
            let fd = unsafe { core::ptr::read_unaligned((fds + index * 8) as *const i32) };
            if fd >= 0 {
                if let Some(at) = crate::user::with_current(|program| program.files.deadline(fd as usize)).flatten() {
                    until = until.min(at.max(crate::time::uptime_ms() + 1));
                }
            }
        }
        crate::io_events::wait(seen, Some(until));
    }
}

/// Что ответить `poll` о дескрипторе `fd`, у которого спрашивают `events`.
fn revents_of(fd: usize, events: u16) -> u16 {
    let mut wanted = 0;
    if events & POLLIN != 0 {
        wanted |= user_abi::POLL_IN;
    }
    if events & POLLOUT != 0 {
        wanted |= user_abi::POLL_OUT;
    }
    let got = crate::user::syscall::readiness_of(fd, wanted);
    // Как у Linux: `POLLIN`/`POLLOUT` — только если о них просили;
    // `POLLHUP`/`POLLERR`/`POLLNVAL` — всегда. И если ждут записи, а
    // собеседника нет (канал без читателя), — `POLLERR`: иначе программа,
    // ждущая только `POLLOUT`, получала `POLLIN | POLLHUP`, не видела ни того,
    // ни ошибки и опрашивала снова — миллионы раз в секунду (.NET Rider,
    // фаза 74).
    let mut revents = 0;
    // Пустой канал без писателей у Linux — только `POLLHUP`, без `POLLIN`
    // (31.10.2026). Родной `poll` отвечает «читай» и в конце файла, а поток
    // `.NET SynchManager` на `POLLIN | POLLHUP` не читал и не засыпал:
    // миллионы опросов за запуск Rider, процессор занят с первой секунды.
    let empty_hangup = got & user_abi::POLL_HUP != 0
        && crate::user::with_current(|program| program.files.pipe_empty(fd)).flatten() == Some(true);
    if got & user_abi::POLL_IN != 0 && events & POLLIN != 0 && !empty_hangup {
        revents |= POLLIN;
    }
    if got & user_abi::POLL_OUT != 0 && events & POLLOUT != 0 {
        revents |= POLLOUT;
    }
    if got & user_abi::POLL_HUP != 0 {
        revents |= POLLHUP;
        if events & POLLOUT != 0 && events & POLLIN == 0 {
            revents |= POLLERR;
        }
    }
    if got & user_abi::POLL_BAD != 0 {
        revents |= POLLNVAL;
    }
    revents
}

/// `select(n, readfds, writefds, exceptfds, timeval)` (31.10.2026: `fsnotifier`
/// Rider ждёт им ввод и `inotify`). Срок `timeval` — секунды и микросекунды,
/// `NULL` — без срока. Остаток срока обратно не пишется: на это не полагается
/// никто из тех, кого мы запускаем.
pub fn select(count: usize, read: usize, write: usize, except: usize, timeout: usize) -> i64 {
    let deadline = if timeout == 0 {
        None
    } else {
        if !space::user_can(timeout, 16, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let [seconds, micros] = unsafe { core::ptr::read_unaligned(timeout as *const [u64; 2]) };
        Some(crate::time::uptime_ms() + seconds.saturating_mul(1000) + micros.div_ceil(1000))
    };
    select_until(count, read, write, except, deadline)
}

/// `pselect6(n, readfds, writefds, exceptfds, timespec, sigmask)`.
pub fn pselect(count: usize, read: usize, write: usize, except: usize, timeout: usize) -> i64 {
    let deadline = if timeout == 0 {
        None
    } else {
        if !space::user_can(timeout, 16, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        let [seconds, nanos] = unsafe { core::ptr::read_unaligned(timeout as *const [u64; 2]) };
        Some(crate::time::uptime_ms() + seconds.saturating_mul(1000) + nanos.div_ceil(1_000_000))
    };
    select_until(count, read, write, except, deadline)
}

/// Наборы `fd_set` — битовые карты по 64 дескриптора в слове.
fn select_until(count: usize, read: usize, write: usize, except: usize, deadline: Option<u64>) -> i64 {
    const FD_SETSIZE: usize = 1024;
    if count > FD_SETSIZE {
        return -e::EINVAL;
    }
    let words = count.div_ceil(64);
    let load = |set: usize| -> Result<[u64; FD_SETSIZE / 64], i64> {
        let mut out = [0u64; FD_SETSIZE / 64];
        if set != 0 && words > 0 {
            if !space::user_can(set, words * 8, PageFlags::WRITE) {
                return Err(-e::EFAULT);
            }
            for (index, word) in out.iter_mut().take(words).enumerate() {
                // SAFETY: проверено выше.
                *word = unsafe { core::ptr::read_unaligned((set + index * 8) as *const u64) };
            }
        }
        Ok(out)
    };
    let (want_read, want_write) = match (load(read), load(write)) {
        (Ok(read), Ok(write)) => (read, write),
        (Err(err), _) | (_, Err(err)) => return err,
    };
    if except != 0 && load(except).is_err() {
        return -e::EFAULT;
    }
    loop {
        let seen = crate::io_events::generation();
        let mut got_read = [0u64; FD_SETSIZE / 64];
        let mut got_write = [0u64; FD_SETSIZE / 64];
        let mut ready = 0i64;
        for fd in 0..count {
            let (word, bit) = (fd / 64, 1u64 << (fd % 64));
            let r = want_read[word] & bit != 0;
            let w = want_write[word] & bit != 0;
            if !r && !w {
                continue;
            }
            let events = if r { POLLIN } else { 0 } | if w { POLLOUT } else { 0 };
            let revents = revents_of(fd, events);
            if revents & POLLNVAL != 0 {
                return -e::EBADF;
            }
            if r && revents & (POLLIN | POLLHUP | POLLERR) != 0 {
                got_read[word] |= bit;
                ready += 1;
            }
            if w && revents & (POLLOUT | POLLERR) != 0 {
                got_write[word] |= bit;
                ready += 1;
            }
        }
        let timed_out = deadline.is_some_and(|deadline| crate::time::uptime_ms() >= deadline);
        if ready > 0 || timed_out {
            for (set, got) in [(read, &got_read), (write, &got_write), (except, &[0u64; FD_SETSIZE / 64])] {
                if set == 0 {
                    continue;
                }
                if !space::user_can(set, words * 8, PageFlags::WRITE) {
                    return -e::EFAULT;
                }
                for (index, word) in got.iter().take(words).enumerate() {
                    // SAFETY: проверено строкой выше.
                    unsafe { core::ptr::write_unaligned((set + index * 8) as *mut u64, *word) };
                }
            }
            return ready;
        }
        if crate::user::kill_pending() || super::signal::pending() {
            return -e::EINTR;
        }
        let mut until = crate::time::uptime_ms() + 100;
        if let Some(deadline) = deadline {
            until = until.min(deadline);
        }
        crate::io_events::wait(seen, Some(until));
    }
}

/// `pipe2(fds, flags)`: канал Linux — ждущий, если не просили `O_NONBLOCK`
/// (родной рождается неждущим).
pub fn pipe(fds: usize, flags: usize) -> i64 {
    const O_NONBLOCK: usize = 0o4000;
    const O_DIRECT: usize = 0o40_000;
    const O_CLOEXEC_FLAG: usize = 0o2_000_000;
    let flags = flags as u32 as usize;
    if flags & !(O_NONBLOCK | O_DIRECT | O_CLOEXEC_FLAG) != 0 {
        return -e::EINVAL;
    }
    if !space::user_can(fds, 8, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // SAFETY: из системного вызова третьего кольца.
    let packed = unsafe { crate::user::syscall::handle(user_abi::SYS_PIPE, 0, 0, 0) };
    if packed < 0 {
        return super::errno(packed);
    }
    let (read_fd, write_fd) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
    const O_CLOEXEC: usize = 0o2_000_000;
    let nonblock = flags & O_NONBLOCK != 0;
    crate::user::with_current(|program| {
        let _ = program.files.set_nonblock(read_fd, nonblock);
        let _ = program.files.set_nonblock(write_fd, nonblock);
        program.files.set_cloexec(read_fd, flags & O_CLOEXEC != 0);
        program.files.set_cloexec(write_fd, flags & O_CLOEXEC != 0);
    });
    let pair: [i32; 2] = [read_fd as i32, write_fd as i32];
    // SAFETY: проверено выше.
    unsafe { core::ptr::write_unaligned(fds as *mut [i32; 2], pair) };
    0
}
