//! Screen capture for Linux programs (23.11.2026): `/dev/fossai-capture`.
//!
//! The kernel capture (`ui/capture.rs`, `SYS_SCREENCAP`) is a native system
//! call; a Linux process (OBS Studio with the «Захват экрана FossAi» module,
//! `libc/linux/obs-fossai.c`) enters the kernel through the Linux layer and
//! cannot make it. The device hands the same actions over ioctls on the open
//! node, with the same structures (`user_abi::CapInfo`, `CapSpec`,
//! `CapStatus`):
//!
//! | request                     | argument                                 | answer      |
//! |-----------------------------|------------------------------------------|-------------|
//! | `FOSSAI_CAP_INFO` (`F`, 1)  | `CapInfo` out                            | 0           |
//! | `FOSSAI_CAP_START` (`F`, 2) | `CapSpec` in                             | 0 / `EBUSY` |
//! | `FOSSAI_CAP_GRAB` (`F`, 3)  | `{u64 buf; u64 have}` in                 | frame seq   |
//! | `FOSSAI_CAP_STOP` (`F`, 4)  | —                                        | 0           |
//! | `FOSSAI_CAP_STATUS` (`F`,5) | `CapStatus` out                          | 0           |
//! | `FOSSAI_CAP_WINDOWS` (`F`,6)| `{u64 buf; u64 len}` in — window lines   | bytes       |
//! | `FOSSAI_CAP_IDENTIFY` (`F`,7)| the argument itself: ms (0 — 3 s)       | 0           |
//!
//! `read` (24.11) hands the monitors as text, one line per monitor, the
//! primary first: `x\ty\tw\th\tprimary\tname\tdescription\tnumber` (desktop
//! coordinates, the `wl_output` connector name, the number of «Параметры →
//! Дисплей» and of the «Определить» plates) — for OBS's monitor list and
//! for the launcher (`fossai-obs` sizes the canvas by the primary monitor);
//! the text is taken at the first `read` of an open, then read to the end.
//!
//! Only the request type `'F'` and number are looked at (the size bits of
//! `_IOWR` are not). A capture started through a descriptor ends when the
//! last copy of it is closed: OBS removing the source, or the process dying,
//! must not leave the red indicator on the panel until the orphan timeout.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use super::e;
use super::ipc::get;
use crate::sync::Mutex;
use crate::user::files::Object;

/// Not in `super::e`: «another capture runs», «no capture».
const EBUSY: i64 = 16;
const ENODATA: i64 = 61;

/// The node.
pub const PATH: &str = "/dev/fossai-capture";

const INFO: usize = 1;
const START: usize = 2;
const GRAB: usize = 3;
const STOP: usize = 4;
const STATUS: usize = 5;
const WINDOWS: usize = 6;
/// 24.11: «Определить» — number plates on the monitors.
const IDENTIFY: usize = 7;

/// One open of the node.
struct Device {
    /// This open started the running capture.
    started: AtomicBool,
    /// `read`: the monitor text taken at the first read, and how far it is read.
    text: Mutex<Option<(Vec<u8>, usize)>>,
}

impl Object for Device {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        let mut text = self.text.lock();
        let (data, at) = text.get_or_insert_with(|| (super::wayland::monitor_lines().into_bytes(), 0));
        let n = buf.len().min(data.len() - *at);
        buf[..n].copy_from_slice(&data[*at..*at + n]);
        *at += n;
        Ok(n)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (true, false, false)
    }
    fn mode(&self) -> u32 {
        0o020_000 // S_IFCHR
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        if self.started.load(Ordering::Relaxed) && crate::ui::capture::active() {
            crate::ui::capture::syscall(user_abi::CAP_STOP, 0, 0);
            crate::kprintln!("  capture     : /dev/fossai-capture closed, capture ended");
        }
    }
}

/// Is `linux` the node (for `stat`/`access`).
#[must_use]
pub fn exists(linux: &str) -> bool {
    linux == PATH
}

/// Open the node; `None` — not this path.
pub fn open(linux: &str, flags: usize) -> Option<i64> {
    const O_CLOEXEC: usize = 0o2_000_000;
    if linux != PATH {
        return None;
    }
    let object: Arc<dyn Object> = Arc::new(Device { started: AtomicBool::new(false), text: Mutex::new(None) });
    Some(match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, false)?;
        program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    })
}

/// Native error of `SYS_SCREENCAP` as a Linux one.
fn errno(answer: i64) -> i64 {
    match answer {
        n if n >= 0 => n,
        user_abi::ERR_EXISTS => -EBUSY,
        user_abi::ERR_AGAIN => -e::EAGAIN,
        user_abi::ERR_NOT_FOUND => -ENODATA,
        user_abi::ERR_BAD_ADDRESS => -e::EFAULT,
        user_abi::ERR_LIMIT => -e::ENOMEM,
        _ => -e::EINVAL,
    }
}

/// ioctl on a descriptor; `None` — not ours.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    let (object, _) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    let device = object.as_any().downcast_ref::<Device>()?;
    if (request >> 8) & 0xff != usize::from(b'F') {
        return Some(-e::ENOTTY);
    }
    let capture = crate::ui::capture::source_syscall;
    Some(match request & 0xff {
        INFO => errno(capture(user_abi::CAP_INFO, arg, 0)),
        START => {
            let answer = errno(capture(user_abi::CAP_START, arg, 0));
            if answer == 0 {
                device.started.store(true, Ordering::Relaxed);
                // OBS's source captures for its preview whether it records
                // or not: no red «recording» indicator for it (24.11).
                crate::ui::capture::mark_shared();
            }
            answer
        }
        GRAB => match get::<[u64; 2]>(arg) {
            Ok([buf, have]) => errno(capture(user_abi::CAP_GRAB, buf as usize, have as usize)),
            Err(err) => err,
        },
        STOP => {
            device.started.store(false, Ordering::Relaxed);
            errno(capture(user_abi::CAP_STOP, 0, 0))
        }
        STATUS => errno(capture(user_abi::CAP_STATUS, arg, 0)),
        WINDOWS => match get::<[u64; 2]>(arg) {
            Ok([buf, len]) => errno(capture(user_abi::CAP_WINDOWS, buf as usize, len as usize)),
            Err(err) => err,
        },
        IDENTIFY => errno(capture(user_abi::CAP_IDENTIFY, arg, 0)),
        _ => -e::ENOTTY,
    })
}
