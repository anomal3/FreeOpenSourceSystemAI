//! Gamepads for programs Linux (24.11.2026): `/dev/input/eventN` and the
//! sysfs that libudev reads about it - so that Proton's `winebus.sys` gives
//! games an XInput and DirectInput pad.
//!
//! # Which way into Wine
//!
//! `winebus.sys` of GE-Proton11 has three ways to a pad (`dlls/winebus.sys`):
//! SDL (`libSDL2-2.0.so.0`, not in our Proton package), hidraw (only for
//! DualShock/DualSense and a list of flight sticks unless
//! `PROTON_ENABLE_HIDRAW`) and evdev - where every device is "deferred to a
//! different backend" (SDL) **except the Steam Input virtual pad**: vendor
//! 28de, product 11ff, named "Microsoft X-Box 360 pad N". That one becomes an
//! XInput gamepad (`is_gamepad`) with the `xpad` layout. Steam itself gives
//! games exactly this device, so it is the best-trodden path of Proton: every
//! pad of the kernel (`usb::gamepad`) is shown here as such a virtual pad.
//! `fossai-proton` sets `PROTON_SPOOF_STEAMINPUT_VIDPID=1`, and Windows sees
//! it as an Xbox 360 pad (045e:028e).
//!
//! # What winebus reads (in a "container", i.e. without udevd)
//!
//! 1. `opendir("/dev/input")`, every `eventN`; `inotify` on `/dev/input` for
//!    hotplug (`IN_CREATE`/`IN_DELETE`);
//! 2. `open(O_RDWR)`, `realpath("/sys/class/input/eventN")` ->
//!    `/sys/devices/virtual/input/inputN/eventN`;
//! 3. libudev `udev_device_new_from_syspath`: the directories, their `uevent`
//!    (`DEVNAME`, `MAJOR`/`MINOR`, properties) and `subsystem` links; the
//!    parent `inputN` with `PRODUCT=3/28de/11ff/1` and `ID_INPUT_JOYSTICK=1`;
//! 4. ioctls `EVIOCGID`, `EVIOCGNAME`, `EVIOCGUNIQ`, `EVIOCGBIT`, `EVIOCGABS`;
//! 5. `poll` and `read` of `struct input_event`, one at a time.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};

use usb_hid::PadState;

use super::e;
use crate::sync::SpinLock;
use crate::usb::gamepad;
use crate::user::files::Object;
use crate::vfs::NodeKind;

/// The directory of the device nodes.
pub const DIR: &str = "/dev/input";
/// `INPUT_MAJOR` and the first minor of `evdev`.
const MAJOR: u64 = 13;
const MINOR_BASE: u64 = 64;
/// Ids of the Steam Input virtual pad.
const BUS_USB: u16 = 3;
const VENDOR: u16 = 0x28de;
const PRODUCT: u16 = 0x11ff;
const VERSION: u16 = 1;

// Event types and codes (`linux/input-event-codes.h`).
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_Z: u16 = 2;
const ABS_RX: u16 = 3;
const ABS_RY: u16 = 4;
const ABS_RZ: u16 = 5;
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;
/// `BTN_A`.. `BTN_THUMBR` in the bit order of [`PadState::buttons`].
const KEYS: [u16; 11] = [0x130, 0x131, 0x133, 0x134, 0x136, 0x137, 0x13a, 0x13b, 0x13c, 0x13d, 0x13e];
/// The axes: code, min, max, fuzz, flat (as `xpad`).
const AXES: [(u16, i32, i32, i32, i32); 8] = [
    (ABS_X, -32768, 32767, 16, 128),
    (ABS_Y, -32768, 32767, 16, 128),
    (ABS_Z, 0, 255, 0, 0),
    (ABS_RX, -32768, 32767, 16, 128),
    (ABS_RY, -32768, 32767, 16, 128),
    (ABS_RZ, 0, 255, 0, 0),
    (ABS_HAT0X, -1, 1, 0, 0),
    (ABS_HAT0Y, -1, 1, 0, 0),
];
/// Size of `struct input_event` on x86-64: `timeval` and type, code, value.
const EVENT_LEN: usize = 24;
/// Most events waiting for one reader; beyond - `SYN_DROPPED`.
const QUEUE_MAX: usize = 1024;

/// Value of axis `code` in `state`.
fn axis(state: &PadState, code: u16) -> i32 {
    match code {
        ABS_X => i32::from(state.lx),
        ABS_Y => i32::from(state.ly),
        ABS_Z => i32::from(state.lt),
        ABS_RX => i32::from(state.rx),
        ABS_RY => i32::from(state.ry),
        ABS_RZ => i32::from(state.rt),
        ABS_HAT0X => i32::from(state.hat_x),
        ABS_HAT0Y => i32::from(state.hat_y),
        _ => 0,
    }
}

/// Name of pad `index`: the one Steam gives its virtual pads (Wine takes the
/// number after "pad" for the XInput slot).
fn name(index: usize) -> String {
    format!("Microsoft X-Box 360 pad {index}")
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

/// One open `/dev/input/eventN`.
struct Client {
    index: usize,
    serial: u32,
    queue: SpinLock<VecDeque<[u8; EVENT_LEN]>>,
    /// The pad went away: reads fail with `ENODEV`, as on Linux.
    gone: AtomicBool,
}

static CLIENTS: SpinLock<Vec<Weak<Client>>> = SpinLock::new(Vec::new());

fn event(kind: u16, code: u16, value: i32) -> [u8; EVENT_LEN] {
    let micros = crate::time::uptime_ns() / 1000;
    let mut out = [0u8; EVENT_LEN];
    out[..8].copy_from_slice(&((micros / 1_000_000) as i64).to_ne_bytes());
    out[8..16].copy_from_slice(&((micros % 1_000_000) as i64).to_ne_bytes());
    out[16..18].copy_from_slice(&kind.to_ne_bytes());
    out[18..20].copy_from_slice(&code.to_ne_bytes());
    out[20..24].copy_from_slice(&value.to_ne_bytes());
    out
}

fn clients_of(index: usize) -> Vec<Arc<Client>> {
    let mut all = CLIENTS.lock();
    all.retain(|weak| weak.strong_count() > 0);
    all.iter().filter_map(Weak::upgrade).filter(|client| client.index == index).collect()
}

/// A pad appeared (`usb::gamepad::attach`): `IN_CREATE` in `/dev/input`.
pub fn attached(index: usize) {
    crate::fs::notify::emit(&format!("{DIR}/event{index}"), crate::fs::notify::CREATE);
    crate::io_events::notify();
}

/// A pad went away: its readers get `ENODEV`, watchers `IN_DELETE`.
pub fn detached(index: usize) {
    for client in clients_of(index) {
        client.gone.store(true, Ordering::Relaxed);
    }
    crate::fs::notify::emit(&format!("{DIR}/event{index}"), crate::fs::notify::DELETE);
    crate::io_events::notify();
}

/// The state of pad `index` changed: events to every reader.
pub fn changed(index: usize, serial: u32, old: &PadState, new: &PadState) {
    let clients = clients_of(index);
    if clients.is_empty() {
        return;
    }
    let mut events: Vec<[u8; EVENT_LEN]> = Vec::new();
    for (code, ..) in AXES {
        let value = axis(new, code);
        if value != axis(old, code) {
            events.push(event(EV_ABS, code, value));
        }
    }
    for (bit, code) in KEYS.iter().enumerate() {
        let mask = 1u16 << bit;
        if (old.buttons ^ new.buttons) & mask != 0 {
            events.push(event(EV_KEY, *code, i32::from(new.buttons & mask != 0)));
        }
    }
    if events.is_empty() {
        return;
    }
    events.push(event(EV_SYN, SYN_REPORT, 0));
    for client in clients.iter().filter(|client| client.serial == serial) {
        let mut queue = client.queue.lock();
        if queue.len() + events.len() > QUEUE_MAX {
            // Like Linux: the reader lost events and must resync by ioctls.
            queue.clear();
            queue.push_back(event(EV_SYN, SYN_DROPPED, 0));
            continue;
        }
        queue.extend(events.iter().copied());
    }
    crate::io_events::notify();
}

struct EvdevFile(Arc<Client>);

impl Object for EvdevFile {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        if buf.len() < EVENT_LEN {
            return Err(-e::EINVAL);
        }
        let mut queue = self.0.queue.lock();
        let mut done = 0;
        while done + EVENT_LEN <= buf.len() {
            let Some(event) = queue.pop_front() else { break };
            buf[done..done + EVENT_LEN].copy_from_slice(&event);
            done += EVENT_LEN;
        }
        if done > 0 {
            return Ok(done);
        }
        if self.0.gone.load(Ordering::Relaxed) {
            return Err(-e::ENODEV);
        }
        Err(-e::EAGAIN)
    }
    /// `EV_FF` (rumble) and `EV_LED` writes are taken and not played yet.
    fn write(&self, data: &[u8]) -> Result<usize, i64> {
        if self.0.gone.load(Ordering::Relaxed) {
            return Err(-e::ENODEV);
        }
        Ok(data.len())
    }
    fn readiness(&self) -> (bool, bool, bool) {
        let gone = self.0.gone.load(Ordering::Relaxed);
        (!self.0.queue.lock().is_empty() || gone, !gone, gone)
    }
    fn mode(&self) -> u32 {
        0o020_000
    }
    fn pending(&self) -> Option<usize> {
        Some(self.0.queue.lock().len() * EVENT_LEN)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Pad index of `/dev/input/eventN`, if it is there.
fn node_index(linux: &str) -> Option<usize> {
    let index: usize = linux.strip_prefix("/dev/input/event")?.parse().ok()?;
    let pad = gamepad::pad(index)?;
    Some(pad.index)
}

/// Open `/dev/input/eventN`; `None` - not ours.
pub fn open(linux: &str, flags: usize) -> Option<i64> {
    const O_NONBLOCK: usize = 0o4000;
    const O_CLOEXEC: usize = 0o2_000_000;
    if !linux.starts_with("/dev/input/event") {
        return None;
    }
    let Some(index) = node_index(linux) else {
        return Some(-e::ENOENT);
    };
    let serial = gamepad::pad(index).map_or(0, |pad| pad.serial);
    let client = Arc::new(Client { index, serial, queue: SpinLock::new(VecDeque::new()), gone: AtomicBool::new(false) });
    CLIENTS.lock().push(Arc::downgrade(&client));
    let object: Arc<dyn Object> = Arc::new(EvdevFile(client));
    let pid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
    crate::kprintln!("  gamepad     : {linux} opened by pid {pid}");
    Some(match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, flags & O_NONBLOCK != 0)?;
        program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    })
}

/// `st_rdev` of `/dev/input/eventN` (`makedev(13, 64 + N)`).
#[must_use]
pub fn rdev(linux: &str) -> Option<u64> {
    let index = node_index(linux)?;
    Some((MAJOR << 8) | (MINOR_BASE + index as u64))
}

// ---------------------------------------------------------------------------
// ioctl
// ---------------------------------------------------------------------------

/// Bitmap of `bits` set, `size` bytes long.
fn bitmap(size: usize, bits: &[u16]) -> Vec<u8> {
    let mut out = alloc::vec![0u8; size];
    for &bit in bits {
        if let Some(byte) = out.get_mut(usize::from(bit) / 8) {
            *byte |= 1 << (bit % 8);
        }
    }
    out
}

/// Copy at most `len` bytes of `data` to the program; the count copied.
fn give(arg: usize, len: usize, data: &[u8]) -> i64 {
    let count = len.min(data.len());
    match super::ipc::write_user(arg, &data[..count]) {
        Ok(()) => count as i64,
        Err(err) => err,
    }
}

/// ioctl on `/dev/input/eventN`; `None` - the descriptor is not ours.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    let (object, _) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    let file = object.as_any().downcast_ref::<EvdevFile>()?;
    let request = request as u32;
    let size = ((request >> 16) & 0x3fff) as usize;
    let nr = request & 0xff;
    let read = request >> 30 == 2;
    if (request >> 8) & 0xff != u32::from(b'E') {
        // `FIONREAD`, `FIONBIO` ... - the common handlers.
        return None;
    }
    let index = file.0.index;
    let state = gamepad::pad(index).filter(|pad| pad.serial == file.0.serial).map(|pad| pad.state);
    // Fixed-size answers return 0 on Linux, the variable ones the length.
    let fixed = |data: &[u8]| match super::ipc::write_user(arg, data) {
        Ok(()) => 0,
        Err(err) => err,
    };
    let int = |value: i32| fixed(&value.to_ne_bytes());
    Some(match nr {
        // EVIOCGVERSION
        0x01 => int(0x01_0001),
        // EVIOCGID: bustype, vendor, product, version.
        0x02 => {
            let mut id = [0u8; 8];
            for (at, value) in [BUS_USB, VENDOR, PRODUCT, VERSION].iter().enumerate() {
                id[at * 2..at * 2 + 2].copy_from_slice(&value.to_ne_bytes());
            }
            fixed(&id)
        }
        // EVIOCGREP: no autorepeat.
        0x03 => -e::EINVAL,
        // EVIOCGNAME
        0x06 if read => {
            let mut text = name(index).into_bytes();
            text.push(0);
            give(arg, size, &text)
        }
        // EVIOCGPHYS, EVIOCGUNIQ: none, like a virtual device.
        0x07 | 0x08 => -e::ENOENT,
        // EVIOCGPROP: no properties.
        0x09 => give(arg, size, &[0u8; 4]),
        // EVIOCGKEY: buttons now.
        0x18 => {
            let buttons = state.map_or(0, |state| state.buttons);
            let held: Vec<u16> = KEYS.iter().enumerate().filter(|(bit, _)| buttons & (1 << bit) != 0).map(|(_, code)| *code).collect();
            give(arg, size, &bitmap(96, &held))
        }
        // EVIOCGLED, EVIOCGSND, EVIOCGSW: nothing on.
        0x19..=0x1b => give(arg, size, &[0u8; 8]),
        // EVIOCGBIT(type)
        0x20..=0x3f => match nr - 0x20 {
            0 => give(arg, size, &bitmap(4, &[EV_SYN, EV_KEY, EV_ABS])),
            1 => give(arg, size, &bitmap(96, &KEYS)),
            3 => {
                let codes: Vec<u16> = AXES.iter().map(|axis| axis.0).collect();
                give(arg, size, &bitmap(8, &codes))
            }
            // REL, MSC, SW, LED, SND, REP, FF: nothing.
            2 | 4 | 5 | 0x11 | 0x12 | 0x14 | 0x15 => give(arg, size, &[0u8; 16]),
            _ => -e::EINVAL,
        },
        // EVIOCGABS(axis): value, minimum, maximum, fuzz, flat, resolution.
        0x40..=0x7f if read => match AXES.iter().find(|axis| u32::from(axis.0) == nr - 0x40) {
            Some(&(code, min, max, fuzz, flat)) => {
                let value = state.map_or(0, |state| axis(&state, code));
                let mut info = [0u8; 24];
                for (at, field) in [value, min, max, fuzz, flat, 0].iter().enumerate() {
                    info[at * 4..at * 4 + 4].copy_from_slice(&field.to_ne_bytes());
                }
                fixed(&info)
            }
            None => -e::EINVAL,
        },
        // EVIOCSABS: accepted, ranges stay.
        0x40..=0x7f => 0,
        // EVIOCSFF, EVIOCRMFF: no force feedback (yet).
        0x80 | 0x81 => -e::EINVAL,
        // EVIOCGEFFECTS: none at once.
        0x84 => int(0),
        // EVIOCGRAB, EVIOCREVOKE, EVIOCSMASK/GMASK, EVIOCSCLOCKID.
        0x90..=0x93 | 0xa0 => 0,
        _ => -e::EINVAL,
    })
}

// ---------------------------------------------------------------------------
// sysfs
// ---------------------------------------------------------------------------

const CLASS: &str = "/sys/class/input";
const DEVICES: &str = "/sys/devices/virtual/input";

/// Indexes of the pads now.
fn present() -> Vec<usize> {
    gamepad::pads().iter().map(|pad| pad.index).collect()
}

/// `/sys/devices/virtual/input/inputN[/rest]` -> (N, rest) of a present pad.
fn in_device(path: &str) -> Option<(usize, &str)> {
    let rest = path.strip_prefix(DEVICES)?.strip_prefix("/input")?;
    let (number, rest) = rest.split_once('/').unwrap_or((rest, ""));
    let index: usize = number.parse().ok()?;
    gamepad::pad(index)?;
    Some((index, rest))
}

/// Directory listing; `None` - not ours (or no pads: then nothing in sysfs
/// changes for other programs).
#[must_use]
pub fn directory(path: &str) -> Option<Vec<(String, NodeKind)>> {
    let file = |name: String| (name, NodeKind::File);
    let dir = |name: &str| (String::from(name), NodeKind::Directory);
    if path == DIR {
        return Some(present().into_iter().map(|index| file(format!("event{index}"))).collect());
    }
    if !path.starts_with("/sys") {
        return None;
    }
    let pads = present();
    if pads.is_empty() {
        return None;
    }
    match path {
        "/sys" => Some(alloc::vec![dir("class"), dir("devices"), dir("dev")]),
        "/sys/devices" => Some(alloc::vec![dir("virtual")]),
        "/sys/devices/virtual" => Some(alloc::vec![dir("input")]),
        DEVICES => Some(pads.iter().map(|index| (format!("input{index}"), NodeKind::Directory)).collect()),
        CLASS => {
            let mut out = Vec::new();
            for index in pads {
                out.push(file(format!("input{index}")));
                out.push(file(format!("event{index}")));
            }
            Some(out)
        }
        _ => {
            let (index, rest) = in_device(path)?;
            let names: &[&str] = match rest {
                "" => &["uevent", "name", "phys", "uniq", "properties", "subsystem", "id", "capabilities"],
                "id" => &["bustype", "vendor", "product", "version"],
                "capabilities" => &["ev", "key", "abs", "rel", "msc", "ff", "led", "snd", "sw"],
                other if other == format!("event{index}") => &["uevent", "dev", "subsystem", "device"],
                _ => return None,
            };
            let mut out: Vec<(String, NodeKind)> = names
                .iter()
                .map(|name| (String::from(*name), if matches!(*name, "id" | "capabilities") { NodeKind::Directory } else { NodeKind::File }))
                .collect();
            if rest.is_empty() {
                out.push((format!("event{index}"), NodeKind::Directory));
            }
            Some(out)
        }
    }
}

/// `KEY=` bitmap the way sysfs prints it: 64-bit words, highest first.
const KEY_WORDS: &str = "7cdb000000000000 0 0 0 0";
/// `ABS=`: X, Y, Z, Rx, Ry, Rz, Hat0X, Hat0Y.
const ABS_WORD: &str = "3003f";

/// Text of a sysfs file; `None` - not ours.
#[must_use]
pub fn text(path: &str) -> Option<String> {
    let (index, rest) = in_device(path)?;
    let event = format!("event{index}");
    let minor = MINOR_BASE + index as u64;
    Some(match rest {
        "uevent" => format!(
            "PRODUCT={BUS_USB:x}/{VENDOR:x}/{PRODUCT:x}/{VERSION:x}\nNAME=\"{}\"\nPHYS=\"\"\nUNIQ=\"\"\nPROP=0\nEV=b\n\
             KEY={KEY_WORDS}\nABS={ABS_WORD}\nMODALIAS=input:b{BUS_USB:04X}v{VENDOR:04X}p{PRODUCT:04X}e{VERSION:04X}-e0,1,3,k130,131,133,134,136,137,13A,13B,13C,13D,13E,ra0,1,2,3,4,5,10,11,mlsfw\n\
             ID_INPUT=1\nID_INPUT_JOYSTICK=1\n",
            name(index)
        ),
        "name" => format!("{}\n", name(index)),
        "phys" | "uniq" => String::from("\n"),
        "properties" => String::from("0\n"),
        "id/bustype" => format!("{BUS_USB:04x}\n"),
        "id/vendor" => format!("{VENDOR:04x}\n"),
        "id/product" => format!("{PRODUCT:04x}\n"),
        "id/version" => format!("{VERSION:04x}\n"),
        "capabilities/ev" => String::from("b\n"),
        "capabilities/key" => format!("{KEY_WORDS}\n"),
        "capabilities/abs" => format!("{ABS_WORD}\n"),
        "capabilities/rel" | "capabilities/msc" | "capabilities/ff" | "capabilities/led" | "capabilities/snd" | "capabilities/sw" => {
            String::from("0\n")
        }
        other => {
            let file = other.strip_prefix(event.as_str())?.strip_prefix('/')?;
            match file {
                "uevent" => format!("MAJOR={MAJOR}\nMINOR={minor}\nDEVNAME=input/{event}\nID_INPUT=1\nID_INPUT_JOYSTICK=1\n"),
                "dev" => format!("{MAJOR}:{minor}\n"),
                _ => return None,
            }
        }
    })
}

/// Target of a sysfs link; `None` - not a link of ours.
#[must_use]
pub fn link(path: &str) -> Option<String> {
    if let Some(name) = path.strip_prefix(CLASS).and_then(|rest| rest.strip_prefix('/')) {
        let (index, event) = if let Some(number) = name.strip_prefix("event") {
            (number.parse::<usize>().ok()?, true)
        } else {
            (name.strip_prefix("input")?.parse::<usize>().ok()?, false)
        };
        gamepad::pad(index)?;
        return Some(if event {
            format!("../../devices/virtual/input/input{index}/event{index}")
        } else {
            format!("../../devices/virtual/input/input{index}")
        });
    }
    let (index, rest) = in_device(path)?;
    let event = format!("event{index}");
    match rest {
        "subsystem" => Some(String::from("../../../../class/input")),
        other => match other.strip_prefix(event.as_str())? {
            "/subsystem" => Some(String::from("../../../../../class/input")),
            "/device" => Some(format!("../../input{index}")),
            _ => None,
        },
    }
}

/// A path through our links (`/sys/class/input/eventN`, `inputN`,
/// `…/eventN/device`) to the real directory: `open` and `stat` go through
/// them. `None` - no link of ours on the way.
#[must_use]
pub fn resolve(path: &str) -> Option<String> {
    let mut path = String::from(path);
    let mut changed = false;
    if let Some(rest) = path.strip_prefix(CLASS).and_then(|rest| rest.strip_prefix('/')) {
        let (name, tail) = rest.split_once('/').map_or((rest, ""), |(name, tail)| (name, tail));
        let target = link(&format!("{CLASS}/{name}"))?;
        let base = target.strip_prefix("../../")?;
        path = if tail.is_empty() { format!("/sys/{base}") } else { format!("/sys/{base}/{tail}") };
        changed = true;
    }
    // `…/inputN/eventN/device` is `…/inputN`.
    if let Some((index, rest)) = in_device(&path) {
        let device = format!("event{index}/device");
        if let Some(tail) = rest.strip_prefix(device.as_str()) {
            path = format!("{DEVICES}/input{index}{tail}");
            changed = true;
        }
    }
    changed.then_some(path)
}

/// Is `path` one of ours (a node, a sysfs file, link or directory)?
#[must_use]
pub fn exists(path: &str) -> bool {
    node_index(path).is_some() || text(path).is_some() || link(path).is_some()
}

/// `readlink` of our sysfs links; `None` - not ours.
pub fn readlink(path: &str, buf: usize, size: usize) -> Option<i64> {
    let target = link(path)?;
    let len = target.len().min(size);
    Some(match super::ipc::write_user(buf, &target.as_bytes()[..len]) {
        Ok(()) => len as i64,
        Err(err) => err,
    })
}

/// Paths `inotify` must watch as they are, not placed on a volume: events
/// of `/dev/input` come from here, not from a file system.
#[must_use]
pub fn virtual_dir(path: &str) -> bool {
    path == DIR
}
