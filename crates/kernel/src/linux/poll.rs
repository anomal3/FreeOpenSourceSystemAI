//! `poll`, `ppoll` и `pipe` программ Linux (фаза 71c).

use super::e;
use crate::mm::PageFlags;
use crate::user::space;

const POLLIN: u16 = 0x1;
const POLLOUT: u16 = 0x4;
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
        let mut ready = 0;
        for index in 0..count {
            let at = fds + index * 8;
            // SAFETY: массив проверен на запись выше.
            let (fd, events) = unsafe { (core::ptr::read_unaligned(at as *const i32), core::ptr::read_unaligned((at + 4) as *const u16)) };
            let revents = if fd < 0 {
                0
            } else {
                let mut wanted = 0;
                if events & POLLIN != 0 {
                    wanted |= user_abi::POLL_IN;
                }
                if events & POLLOUT != 0 {
                    wanted |= user_abi::POLL_OUT;
                }
                let got = crate::user::syscall::readiness_of(fd as usize, wanted);
                let mut revents = 0;
                if got & user_abi::POLL_IN != 0 {
                    revents |= POLLIN;
                }
                if got & user_abi::POLL_OUT != 0 {
                    revents |= POLLOUT;
                }
                if got & user_abi::POLL_HUP != 0 {
                    revents |= POLLHUP;
                }
                if got & user_abi::POLL_BAD != 0 {
                    revents |= POLLNVAL;
                }
                revents
            };
            // SAFETY: тот же проверенный массив.
            unsafe { core::ptr::write_unaligned((at + 6) as *mut u16, revents) };
            if revents != 0 {
                ready += 1;
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
        crate::io_events::wait(seen, Some(until));
    }
}

/// `pipe2(fds, flags)`: канал Linux — ждущий, если не просили `O_NONBLOCK`
/// (родной рождается неждущим).
pub fn pipe(fds: usize, flags: usize) -> i64 {
    const O_NONBLOCK: usize = 0o4000;
    if !space::user_can(fds, 8, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // SAFETY: из системного вызова третьего кольца.
    let packed = unsafe { crate::user::syscall::handle(user_abi::SYS_PIPE, 0, 0, 0) };
    if packed < 0 {
        return super::errno(packed);
    }
    let (read_fd, write_fd) = ((packed >> 32) as usize, (packed & 0xFFFF_FFFF) as usize);
    let nonblock = flags & O_NONBLOCK != 0;
    crate::user::with_current(|program| {
        let _ = program.files.set_nonblock(read_fd, nonblock);
        let _ = program.files.set_nonblock(write_fd, nonblock);
    });
    let pair: [i32; 2] = [read_fd as i32, write_fd as i32];
    // SAFETY: проверено выше.
    unsafe { core::ptr::write_unaligned(fds as *mut [i32; 2], pair) };
    0
}
