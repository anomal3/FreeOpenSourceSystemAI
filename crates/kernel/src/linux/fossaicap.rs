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
//! | `FOSSAI_CAP_DMABUF` (`F`,8) | `CapDmabuf` out — the dma-buf ring       | 0 / `ENODEV`|
//! | `FOSSAI_CAP_FRAME` (`F`,9)  | `CapFrame` in/out — the newest frame     | 0           |
//! | `FOSSAI_CAP_CURSOR` (`F`,10)| `CapCursor` in/out — the pointer         | 0           |
//! | `FOSSAI_CAP_PICK` (`F`,11)  | `CapPick` in/out — show the share picker | 0 / `EBUSY` |
//! | `FOSSAI_CAP_PICK_POLL` (12) | `CapPick` out — its answer (`state`)     | 0 / `ENODATA`|
//! | `FOSSAI_CAP_PICK_CANCEL`(13)| — take the picker away                   | 0           |
//!
//! # «Демонстрация экрана» (28.11)
//!
//! `PICK` asks the desktop to show its picker of a monitor or a window
//! (`ui::sharepick`; the ScreenCast portal on `Start`); the answer is polled
//! with `PICK_POLL` (state 0 waiting, 1 chosen — rectangle, monitor, name,
//! pointer switch — 2 cancelled). One request per open; closing the open or
//! `PICK_CANCEL` takes an unanswered picker away.
//!
//! # One capture per open, per rectangle (26.11)
//!
//! Every open may start one capture (`START`; a second one on the same open
//! is `EBUSY`). The kernel keeps one session per captured rectangle
//! (`ui::capture`): OBS's sources on monitor 1 and monitor 2 are two opens
//! and two sessions at the monitors' own sizes, a second source of the same
//! monitor (or `/bin/screenrec` recording it) joins the session and costs no
//! second copy. Before, one capture ran on the whole system and OBS captured
//! the box around all its sources (5120x1440 for two monitors).
//!
//! # The pointer (26.11)
//!
//! `CURSOR` answers the pointer over the open's capture: its picture
//! (`0xAARRGGBB`, copied only when `have_serial` is old), hotspot, place in
//! the captured rectangle and whether the compositor drew it into the
//! composed frames itself (`CAP_CURSOR_IN_FRAME`). OBS starts its capture
//! without `CAP_POINTER` and draws the pointer as its own quad: over a ring
//! frame, and over a fullscreen game's buffer, which never has the pointer
//! (the display draws it as a hardware cursor). A pointer move costs no frame
//! copy any more.
//!
//! # Zero copy for OBS on the GPU (25.11)
//!
//! `GRAB` copies the whole frame into the program, OBS copies it again into
//! its frame queue and once more into a texture (three copies of 5120x1440
//! per frame on Roman's two monitors, hw-log19). With a render node
//! (`/dev/dri/renderD128`: NVIDIA, or `nvk soft on` in QEMU) the program can
//! take frames as dma-bufs instead:
//!
//! * `DMABUF` makes a ring of [`RING`] linear buffers in system memory the
//!   GPU reads (`drm::capture_buffer`, stride a multiple of 256 bytes) and
//!   answers their descriptors, the DRM fourcc (`XR24` for the BGR screen
//!   format, `XB24` for RGB) and the modifier (`DRM_FORMAT_MOD_LINEAR`). OBS
//!   imports each once as a texture (`gs_texture_create_from_dmabuf`: EGL
//!   `EGL_EXT_image_dma_buf_import` - zink imports it into NVK).
//! * `FRAME` hands the newest frame: when the mirror changed, it is copied
//!   into the next slot (the only CPU copy left, done in the caller's thread,
//!   the pointer patched in) and the slot number is answered. When the
//!   captured rectangle is a monitor shown straight from a fullscreen game's
//!   buffer (direct scanout), the answer is **that buffer** (`buffer` = its
//!   first physical frame, a new descriptor in `fd` unless the caller lists
//!   the buffer in `known`) - nothing is copied at all, and the desktop stops
//!   copying the game into the mirror (`capture::set_client`).
//! * The frame fence: a slot is complete when `FRAME` returns (the copy is
//!   synchronous); a game buffer is shown by the desktop only after its frame
//!   fence is done (`drm::frame_pending`). `fence_fd` is therefore always -1
//!   ("ready"); the ring of three keeps a slot the GPU may still sample two
//!   frames away from the next copy.
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
/// 25.11: the dma-buf ring and the newest frame (zero copy for OBS).
const DMABUF: usize = 8;
const FRAME: usize = 9;
/// 26.11: the pointer over the capture.
const CURSOR: usize = 10;
/// 28.11: the share picker (`ui::sharepick`).
const PICK: usize = 11;
const PICK_POLL: usize = 12;
const PICK_CANCEL: usize = 13;

/// Buffers in the dma-buf ring.
const RING: usize = 3;
/// Row alignment of ring buffers (NVK's linear images want 128 bytes).
const STRIDE_ALIGN: u32 = 256;
/// `fourcc_code('X','R','2','4')`, `fourcc_code('X','B','2','4')`.
const DRM_FORMAT_XRGB8888: u32 = 0x3432_5258;
const DRM_FORMAT_XBGR8888: u32 = 0x3432_4258;
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

/// `FOSSAI_CAP_DMABUF` answer (`struct cap_dmabuf` of obs-fossai.c).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapDmabuf {
    count: u32,
    width: u32,
    height: u32,
    stride: u32,
    fourcc: u32,
    flags: u32,
    modifier: u64,
    fds: [i32; 4],
}

/// `FOSSAI_CAP_FRAME` (`struct cap_frame` of obs-fossai.c).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct CapFrame {
    /// In: the frame number the caller has.
    have: u64,
    /// In: game buffers (`buffer`) the caller has imported already.
    known: [u64; 4],
    /// Out: the frame number.
    seq: u64,
    /// Out: a game's buffer (its first physical frame) or 0 - a ring slot.
    buffer: u64,
    /// Out: the ring slot (`-1` with a game buffer).
    slot: i32,
    /// Out: a new descriptor of `buffer` when it is not in `known`, else -1.
    fd: i32,
    width: u32,
    height: u32,
    stride: u32,
    fourcc: u32,
    modifier: u64,
    /// Out: always -1 - the frame is complete on return (see the module).
    fence_fd: i32,
    flags: u32,
}

/// The dma-buf ring of one open.
struct Ring {
    bos: Vec<Arc<crate::linux::drm::Bo>>,
    width: u32,
    height: u32,
    stride: u32,
    fourcc: u32,
    /// The slot answered last and the frame number copied into it.
    shown: usize,
    copied: u64,
}

/// The capture of one open (26.11): its kernel session.
#[derive(Clone, Copy)]
struct Use {
    session: u32,
    /// The pointer is patched into the frames (`CAP_POINTER`).
    pointer: bool,
    /// The open samples a fullscreen game's buffer now (`FRAME`).
    client: bool,
    started_ms: u64,
}

/// One open of the node.
struct Device {
    /// The capture this open started.
    session: Mutex<Option<Use>>,
    /// `read`: the monitor text taken at the first read, and how far it is read.
    text: Mutex<Option<(Vec<u8>, usize)>>,
    /// 25.11: the dma-buf ring (`FOSSAI_CAP_DMABUF`).
    ring: Mutex<Option<Ring>>,
    /// 28.11: the share picker this open asked for (`FOSSAI_CAP_PICK`).
    pick: Mutex<Option<u32>>,
}

/// A ring slot as a grab target: rows at `stride`, through the kernel's
/// direct map of the buffer's frames (one slice when they are contiguous).
struct BoSink<'a> {
    frames: &'a [u64],
    stride: usize,
}

impl BoSink<'_> {
    fn at(&self, offset: usize) -> *mut u8 {
        let frame = self.frames[offset / 4096];
        crate::mm::PhysAddr(frame + (offset % 4096) as u64).to_direct_map().as_mut_ptr::<u8>()
    }

    /// Copy `bytes` to `offset` of the buffer, page by page.
    fn write(&self, mut offset: usize, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let room = 4096 - offset % 4096;
            let n = room.min(bytes.len());
            // SAFETY: `offset + n` is inside the buffer (rows of the ring
            // buffer's size, checked when it was made) and inside one frame;
            // the frames belong to the buffer, which lives (the ring holds it).
            unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.at(offset), n) };
            offset += n;
            bytes = &bytes[n..];
        }
    }
}

impl crate::ui::capture::FrameSink for BoSink<'_> {
    fn put_rows(&mut self, first: u32, width: u32, points: &[u32]) {
        let row_bytes = width as usize * 4;
        // SAFETY: plain `u32` points viewed as their bytes.
        let bytes = unsafe { core::slice::from_raw_parts(points.as_ptr().cast::<u8>(), points.len() * 4) };
        for (index, row) in bytes.chunks(row_bytes).enumerate() {
            self.write((first as usize + index) * self.stride, row);
        }
    }
    fn point(&mut self, x: u32, y: u32) -> &mut u32 {
        // A point never crosses a page (4-byte aligned offsets).
        // SAFETY: inside the buffer (x < width, y < height), aligned, and
        // only this grab writes the slot now.
        unsafe { &mut *self.at(y as usize * self.stride + x as usize * 4).cast::<u32>() }
    }
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
        if let Some(id) = self.pick.lock().take() {
            crate::ui::sharepick::cancel(id);
        }
        if self.session.lock().is_some() {
            stop(self, "/dev/fossai-capture closed");
        }
    }
}

/// End the open's capture (`STOP`, close).
fn stop(device: &Device, why: &str) -> i64 {
    let Some(used) = device.session.lock().take() else {
        return -ENODATA;
    };
    *device.ring.lock() = None;
    crate::ui::capture::hold_game(key_of(device), None);
    match crate::ui::capture::close_session(used.session, used.pointer, used.client) {
        Some(grabs) => crate::kprintln!("  capture     : stopped after {grabs} grab(s) (session {}, {why})", used.session),
        None => crate::kprintln!("  capture     : one consumer of session {} left ({why})", used.session),
    }
    0
}

/// The open as a key of its held game buffer (`capture::hold_game`).
fn key_of(device: &Device) -> usize {
    core::ptr::from_ref(device) as usize
}

/// The open's capture.
fn used(device: &Device) -> Option<Use> {
    *device.session.lock()
}

/// The open samples the game's buffer (`on`) or composed frames.
fn set_client(device: &Device, on: bool) {
    let mut session = device.session.lock();
    if let Some(used) = session.as_mut().filter(|used| used.client != on) {
        used.client = on;
        crate::ui::capture::set_client(used.session, on);
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
    let object: Arc<dyn Object> = Arc::new(Device { session: Mutex::new(None), text: Mutex::new(None), ring: Mutex::new(None), pick: Mutex::new(None) });
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
    let capture = crate::ui::capture::syscall;
    Some(match request & 0xff {
        INFO => errno(capture(user_abi::CAP_INFO, arg, 0)),
        START => start(device, arg),
        GRAB => match (get::<[u64; 2]>(arg), used(device)) {
            (Ok([buf, have]), Some(used)) => {
                // 25.11: a source back on the CPU copy needs the mirror kept
                // up to date (see `FRAME`).
                set_client(device, false);
                crate::ui::capture::hold_game(key_of(device), None);
                errno(crate::ui::capture::grab(used.session, buf as usize, have, used.pointer))
            }
            (Err(err), _) => err,
            (_, None) => -ENODATA,
        },
        STOP => stop(device, "stopped by the program"),
        STATUS => match used(device) {
            Some(used) => errno(crate::ui::capture::session_status(used.session, used.started_ms, arg)),
            None => errno(write_idle(arg)),
        },
        WINDOWS => match get::<[u64; 2]>(arg) {
            Ok([buf, len]) => errno(capture(user_abi::CAP_WINDOWS, buf as usize, len as usize)),
            Err(err) => err,
        },
        IDENTIFY => errno(capture(user_abi::CAP_IDENTIFY, arg, 0)),
        DMABUF => dmabuf(device, arg),
        FRAME => frame(device, arg),
        CURSOR => cursor(device, arg),
        PICK => pick(device, arg),
        PICK_POLL => match *device.pick.lock() {
            Some(id) => match crate::ui::sharepick::poll(id) {
                Some(answer) => {
                    if super::ipc::put(arg, answer).is_err() {
                        -e::EFAULT
                    } else {
                        0
                    }
                }
                None => -ENODATA,
            },
            None => -ENODATA,
        },
        PICK_CANCEL => {
            if let Some(id) = device.pick.lock().take() {
                crate::ui::sharepick::cancel(id);
            }
            0
        }
        _ => -e::ENOTTY,
    })
}

/// `FOSSAI_CAP_PICK`: show the share picker (one request per open; a new one
/// replaces an answered one).
fn pick(device: &Device, arg: usize) -> i64 {
    let Ok(mut request) = get::<crate::ui::sharepick::CapPick>(arg) else { return -e::EFAULT };
    let mut held = device.pick.lock();
    if let Some(old) = held.take() {
        crate::ui::sharepick::cancel(old);
    }
    let end = request.title.iter().position(|&b| b == 0).unwrap_or(request.title.len());
    let title = core::str::from_utf8(&request.title[..end]).unwrap_or("");
    let pointer = request.flags & crate::ui::sharepick::PICK_POINTER != 0;
    match crate::ui::sharepick::ask(request.types, pointer, title) {
        Ok(id) => {
            crate::kprintln!("  share       : '{title}' asks to choose (types {}, pointer {}), request {id}", request.types, if pointer { "on" } else { "off" });
            *held = Some(id);
            request.id = id;
            request.state = crate::ui::sharepick::STATE_WAITING;
            if super::ipc::put(arg, request).is_err() { -e::EFAULT } else { 0 }
        }
        Err(err) => errno(err),
    }
}

/// `FOSSAI_CAP_START`: the open's capture (one per open).
fn start(device: &Device, arg: usize) -> i64 {
    let Ok(spec) = get::<user_abi::CapSpec>(arg) else { return -e::EFAULT };
    let mut session = device.session.lock();
    if session.is_some() {
        return -EBUSY;
    }
    match crate::ui::capture::open_session(&spec) {
        Ok(id) => {
            *session = Some(Use { session: id, pointer: spec.flags & user_abi::CAP_POINTER != 0, client: false, started_ms: crate::time::uptime_ms() });
            0
        }
        Err(err) => errno(err),
    }
}

/// `FOSSAI_CAP_STATUS` without a capture on the open: «none».
fn write_idle(arg: usize) -> i64 {
    if super::ipc::put(arg, user_abi::CapStatus::default()).is_err() { user_abi::ERR_BAD_ADDRESS } else { 0 }
}

/// `FOSSAI_CAP_CURSOR`: the pointer over the open's capture.
fn cursor(device: &Device, arg: usize) -> i64 {
    let Some(used) = used(device) else { return -ENODATA };
    let Ok(mut args) = get::<user_abi::CapCursor>(arg) else { return -e::EFAULT };
    let answer = crate::ui::capture::cursor(used.session, &mut args);
    if answer < 0 {
        return errno(answer);
    }
    if super::ipc::put(arg, args).is_err() { -e::EFAULT } else { 0 }
}

/// `FOSSAI_CAP_DMABUF`: a ring for the running capture of this open.
fn dmabuf(device: &Device, arg: usize) -> i64 {
    let Some(used) = used(device) else {
        return -ENODATA;
    };
    let Some((width, height)) = crate::ui::capture::size(used.session) else {
        return -ENODATA;
    };
    let stride = (width * 4).next_multiple_of(STRIDE_ALIGN);
    let size = u64::from(stride) * u64::from(height);
    let mut bos = Vec::new();
    for _ in 0..RING {
        match crate::linux::drm::capture_buffer(size) {
            Ok(bo) => bos.push(bo),
            Err(err) => {
                crate::kprintln!("  capture     : no dma-buf ring {width}x{height}: error {err} (the source copies frames)");
                return err;
            }
        }
    }
    let fourcc = if mini_ui::format_code() == 1 { DRM_FORMAT_XBGR8888 } else { DRM_FORMAT_XRGB8888 };
    let mut answer = CapDmabuf { count: RING as u32, width, height, stride, fourcc, flags: 0, modifier: DRM_FORMAT_MOD_LINEAR, fds: [-1; 4] };
    for (index, bo) in bos.iter().enumerate() {
        let fd = crate::linux::drm::install_dmabuf(Arc::clone(bo));
        if fd < 0 {
            close_fds(&answer.fds);
            return fd;
        }
        answer.fds[index] = fd as i32;
    }
    if super::ipc::put(arg, answer).is_err() {
        close_fds(&answer.fds);
        return -e::EFAULT;
    }
    crate::kprintln!("  capture     : dma-buf ring of {RING} for {width}x{height} (stride {stride}, linear) - frames go to the GPU without a copy in the program");
    *device.ring.lock() = Some(Ring { bos, width, height, stride, fourcc, shown: RING - 1, copied: 0 });
    0
}

fn close_fds(fds: &[i32]) {
    for &fd in fds.iter().filter(|&&fd| fd >= 0) {
        let _ = crate::user::with_current(|program| program.files.close(fd as usize));
    }
}

/// `FOSSAI_CAP_FRAME`: the newest frame - a game's buffer or a ring slot.
fn frame(device: &Device, arg: usize) -> i64 {
    let Ok(mut args) = get::<CapFrame>(arg) else { return -e::EFAULT };
    let Some(used) = used(device) else { return -ENODATA };
    let id = used.session;
    let mut ring = device.ring.lock();
    let Some(ring) = ring.as_mut() else { return -ENODATA };
    if crate::ui::capture::size(id) != Some((ring.width, ring.height)) {
        // The capture was restarted on another size: a new ring is needed.
        return -ENODATA;
    }
    args.fence_fd = -1;
    args.fd = -1;
    args.flags = 0;
    // A fullscreen game scanned out from its own buffer: that buffer.
    let game = crate::ui::capture::direct_source(id).and_then(|(phys, pitch)| {
        let bo = crate::linux::drm::exported_by_frame(phys)?;
        (pitch >= ring.width * 4 && u64::from(pitch) * u64::from(ring.height) <= bo.size).then_some((phys, pitch, bo))
    });
    set_client(device, game.is_some());
    // 27.11: the buffer stays away from the game until the next `FRAME`.
    crate::ui::capture::hold_game(key_of(device), game.as_ref().map(|(phys, pitch, _)| (*phys, u64::from(*pitch) * u64::from(ring.height))));
    if let Some((phys, pitch, bo)) = game {
        crate::ui::capture::touch(id);
        args.buffer = phys;
        args.slot = -1;
        args.width = ring.width;
        args.height = ring.height;
        args.stride = pitch;
        args.fourcc = ring.fourcc;
        args.modifier = DRM_FORMAT_MOD_LINEAR;
        args.seq = args.have.wrapping_add(1);
        if !args.known.contains(&phys) {
            let fd = crate::linux::drm::install_dmabuf(bo);
            if fd < 0 {
                return fd;
            }
            args.fd = fd as i32;
        }
        if super::ipc::put(arg, args).is_err() {
            close_fds(&[args.fd]);
            return -e::EFAULT;
        }
        return 0;
    }
    // The composed frame: copied into the next slot when it changed.
    let next = (ring.shown + 1) % ring.bos.len();
    let stride = ring.stride as usize;
    let answer = {
        let Some(frames) = ring.bos[next].frames() else { return -e::EINVAL };
        crate::ui::capture::grab_with(id, ring.copied, used.pointer, |w, h| {
            if (w, h) != (ring.width, ring.height) || frames.len() * 4096 < stride * h as usize {
                return Err(user_abi::ERR_NOT_FOUND);
            }
            Ok(BoSink { frames, stride })
        })
    };
    if answer < 0 {
        return errno(answer);
    }
    let seq = answer as u64;
    if seq != ring.copied {
        ring.copied = seq;
        ring.shown = next;
    }
    args.seq = ring.copied;
    args.buffer = 0;
    args.slot = ring.shown as i32;
    args.width = ring.width;
    args.height = ring.height;
    args.stride = ring.stride;
    args.fourcc = ring.fourcc;
    args.modifier = DRM_FORMAT_MOD_LINEAR;
    if super::ipc::put(arg, args).is_err() { -e::EFAULT } else { 0 }
}
