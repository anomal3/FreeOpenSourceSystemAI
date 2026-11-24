//! Gamepads (24.11.2026, Roman: "no gamepad in any Wine game").
//!
//! # What is here
//!
//! * the list of connected pads ([`attach`], [`PadHandle`]) - the state of
//!   each in one layout, the one of the Xbox 360 pad ([`usb_hid::PadState`]);
//! * [`Gamepad`] - the reader of a pad's reports for the USB drivers: Xbox 360
//!   (wired and the wireless receiver), Xbox One / Series (GIP) and every HID
//!   gamepad or joystick (DualShock 4, DualSense, DirectInput pads) by its
//!   report descriptor;
//! * [`find_xbox`] - the vendor interface of an Xbox pad in a configuration;
//! * the shell command `gamepad` ([`shell`]): the pads and their live state, a
//!   log of every change (`gamepad log on`) and a test pad without hardware
//!   (`gamepad fake ...`) for the stand, where QEMU has no USB gamepad.
//!
//! Programs see the pads through the Linux layer: `/dev/input/eventN` as the
//! "Microsoft X-Box 360 pad N" of Steam (`linux/evdev.rs`) - exactly what
//! Proton's `winebus.sys` turns into an XInput and DirectInput device.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use usb_hid::pad::{self, Gip, Wireless, XboxKind};
use usb_hid::{GamepadMap, PadState};

use crate::sync::SpinLock;

/// Most pads at once (Wine's XInput has four slots; DirectInput more).
pub const MAX_PADS: usize = 8;

/// A connected pad.
#[derive(Clone)]
pub struct PadInfo {
    pub index: usize,
    /// Changes with every attach: a slot reused by the next pad must not be
    /// taken for the previous one (an open `/dev/input/event0`).
    pub serial: u32,
    pub name: String,
    /// What reads it: "Xbox 360", "Xbox One", "HID", "test".
    pub kind: &'static str,
    pub vendor: u16,
    pub product: u16,
    pub state: PadState,
    /// State changes so far.
    pub changes: u64,
}

static PADS: SpinLock<Vec<Option<PadInfo>>> = SpinLock::new(Vec::new());
static SERIALS: AtomicU32 = AtomicU32::new(1);
/// `gamepad log on`: every change of every pad into the journal.
static LOG: AtomicBool = AtomicBool::new(false);

/// A pad in the list; dropping it removes the pad (the device was unplugged,
/// the wireless pad went away, the test pad was switched off).
pub struct PadHandle {
    index: usize,
    serial: u32,
}

/// Add a pad. `None` - all [`MAX_PADS`] slots are taken.
pub fn attach(name: &str, kind: &'static str, vendor: u16, product: u16) -> Option<PadHandle> {
    let serial = SERIALS.fetch_add(1, Ordering::Relaxed);
    let index = {
        let mut pads = PADS.lock();
        if pads.len() < MAX_PADS {
            pads.resize(MAX_PADS, None);
        }
        let index = pads.iter().position(Option::is_none)?;
        pads[index] = Some(PadInfo {
            index,
            serial,
            name: String::from(name),
            kind,
            vendor,
            product,
            state: PadState::default(),
            changes: 0,
        });
        index
    };
    crate::kprintln!("  gamepad     : pad {index} connected: {name} ({vendor:04x}:{product:04x}, {kind}) -> /dev/input/event{index}");
    #[cfg(target_arch = "x86_64")]
    crate::linux::evdev::attached(index);
    Some(PadHandle { index, serial })
}

impl PadHandle {
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// New state; the same as before is not a change.
    pub fn update(&self, state: PadState) {
        let old = {
            let mut pads = PADS.lock();
            let Some(Some(pad)) = pads.get_mut(self.index) else { return };
            if pad.serial != self.serial || pad.state == state {
                return;
            }
            pad.changes += 1;
            core::mem::replace(&mut pad.state, state)
        };
        if LOG.load(Ordering::Relaxed) {
            crate::kprintln!("  gamepad     : pad {} {}", self.index, describe(&state));
        }
        #[cfg(target_arch = "x86_64")]
        crate::linux::evdev::changed(self.index, self.serial, &old, &state);
        #[cfg(not(target_arch = "x86_64"))]
        let _ = old;
    }

    /// Name and ids, known only after the pad was described (HID pads).
    pub fn rename(&self, name: &str, vendor: u16, product: u16) {
        let mut pads = PADS.lock();
        if let Some(Some(pad)) = pads.get_mut(self.index) {
            if pad.serial == self.serial {
                pad.name = String::from(name);
                pad.vendor = vendor;
                pad.product = product;
            }
        }
    }
}

impl Drop for PadHandle {
    fn drop(&mut self) {
        let gone = {
            let mut pads = PADS.lock();
            match pads.get_mut(self.index) {
                Some(slot) if slot.as_ref().is_some_and(|pad| pad.serial == self.serial) => slot.take(),
                _ => None,
            }
        };
        if let Some(pad) = gone {
            crate::kprintln!("  gamepad     : pad {} disconnected: {} after {} change(s)", self.index, pad.name, pad.changes);
            #[cfg(target_arch = "x86_64")]
            crate::linux::evdev::detached(self.index);
        }
    }
}

/// The pads now.
#[must_use]
pub fn pads() -> Vec<PadInfo> {
    PADS.lock().iter().flatten().cloned().collect()
}

/// One pad by its slot (the Linux layer's `/dev/input/eventN`).
#[must_use]
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub fn pad(index: usize) -> Option<PadInfo> {
    PADS.lock().get(index).cloned().flatten()
}

/// The state in one line: buttons, sticks, triggers, d-pad.
#[must_use]
pub fn describe(state: &PadState) -> String {
    alloc::format!(
        "buttons {} lx {} ly {} rx {} ry {} lt {} rt {} dpad {},{}",
        state.pressed(),
        state.lx,
        state.ly,
        state.rx,
        state.ry,
        state.lt,
        state.rt,
        state.hat_x,
        state.hat_y
    )
}

// ---------------------------------------------------------------------------
// The reader of a USB pad
// ---------------------------------------------------------------------------

enum Source {
    /// HID gamepad or joystick, by its report descriptor.
    Hid(GamepadMap),
    Xbox(XboxKind),
}

/// Reports of one pad interface -> [`PadHandle::update`]; packets for the
/// pad's OUT endpoint (Xbox init, acks, LEDs) wait in [`Gamepad::take_output`].
pub struct Gamepad {
    source: Source,
    identity: (u16, u16),
    pad: Option<PadHandle>,
    /// Guide of an Xbox One pad: its own message, kept between inputs.
    guide: bool,
    last: PadState,
    reports: u64,
    /// GIP sequence number of the next packet.
    seq: u8,
    out: VecDeque<Vec<u8>>,
}

impl Gamepad {
    /// A HID pad described by `map`. Its ids come later ([`Gamepad::identify`]).
    #[must_use]
    pub fn hid(map: GamepadMap) -> Self {
        let pad = attach(&hid_name(&map, (0, 0)), "HID", 0, 0);
        Self {
            source: Source::Hid(map),
            identity: (0, 0),
            pad,
            guide: false,
            last: PadState::default(),
            reports: 0,
            seq: 0,
            out: VecDeque::new(),
        }
    }

    /// An Xbox pad on its vendor interface. The init packets wait for the OUT
    /// endpoint at once.
    #[must_use]
    pub fn xbox(kind: XboxKind, vendor: u16, product: u16) -> Self {
        let mut pad = Self {
            source: Source::Xbox(kind),
            identity: (vendor, product),
            pad: None,
            guide: false,
            last: PadState::default(),
            reports: 0,
            seq: 0,
            out: VecDeque::new(),
        };
        match kind {
            XboxKind::X360 => {
                pad.connect();
                let index = pad.pad.as_ref().map_or(0, PadHandle::index) as u8;
                pad.out.push_back(Vec::from(pad::x360_led(index)));
            }
            // A pad behind the receiver appears when it says so.
            XboxKind::X360Wireless => pad.out.push_back(Vec::from(pad::X360W_INQUIRY)),
            XboxKind::XboxOne => {
                pad.connect();
                pad.gip_init();
            }
        }
        pad
    }

    fn connect(&mut self) {
        if self.pad.is_none() {
            let (vendor, product) = self.identity;
            let name = match self.source {
                Source::Xbox(kind) => alloc::format!("{} pad", kind.name()),
                Source::Hid(ref map) => hid_name(map, self.identity),
            };
            self.pad = attach(&name, self.kind(), vendor, product);
        }
    }

    fn kind(&self) -> &'static str {
        match self.source {
            Source::Hid(_) => "HID",
            Source::Xbox(kind) => kind.name(),
        }
    }

    fn gip_init(&mut self) {
        let (vendor, product) = self.identity;
        for packet in pad::gip_init(vendor, product) {
            let mut packet = Vec::from(packet);
            packet[2] = self.seq;
            self.seq = self.seq.wrapping_add(1);
            self.out.push_back(packet);
        }
    }

    /// Vendor and product of a HID pad, once the device descriptor is read.
    pub fn identify(&mut self, vendor: u16, product: u16) {
        self.identity = (vendor, product);
        if let (Some(handle), Source::Hid(map)) = (&self.pad, &self.source) {
            handle.rename(&hid_name(map, self.identity), vendor, product);
        }
    }

    #[must_use]
    pub const fn reports(&self) -> u64 {
        self.reports
    }

    /// The next packet for the OUT endpoint.
    pub fn take_output(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    pub fn handle_report(&mut self, report: &[u8]) {
        self.reports += 1;
        let state = match self.source {
            Source::Hid(map) => map.to_pad(report),
            Source::Xbox(XboxKind::X360) => pad::decode_x360(report),
            Source::Xbox(XboxKind::X360Wireless) => match pad::decode_x360_wireless(report) {
                Wireless::Presence(true) => {
                    self.connect();
                    let index = self.pad.as_ref().map_or(0, PadHandle::index) as u8;
                    self.out.push_back(Vec::from(pad::x360w_led(index)));
                    None
                }
                Wireless::Presence(false) => {
                    self.pad = None;
                    self.last = PadState::default();
                    None
                }
                Wireless::Input(state) => {
                    self.connect();
                    Some(state)
                }
                Wireless::Other => None,
            },
            Source::Xbox(XboxKind::XboxOne) => match pad::decode_gip(report) {
                Gip::Input(mut state) => {
                    if self.guide {
                        state.buttons |= pad::BTN_GUIDE;
                    }
                    Some(state)
                }
                Gip::Guide { pressed, ack } => {
                    if let Some(seq) = ack {
                        self.out.push_back(Vec::from(pad::gip_ack(seq)));
                    }
                    self.guide = pressed;
                    let mut state = self.last;
                    state.buttons = (state.buttons & !pad::BTN_GUIDE) | if pressed { pad::BTN_GUIDE } else { 0 };
                    Some(state)
                }
                Gip::Announce => {
                    crate::kprintln!("  gamepad     : the Xbox One pad announced itself, powering it on");
                    self.gip_init();
                    None
                }
                Gip::Other => None,
            },
        };
        let Some(state) = state else { return };
        self.last = state;
        if let Some(handle) = &self.pad {
            handle.update(state);
        }
    }
}

/// Name of a HID pad: known families by their ids, else "HID gamepad".
fn hid_name(map: &GamepadMap, (vendor, product): (u16, u16)) -> String {
    let known = match (vendor, product) {
        (0x054c, 0x05c4 | 0x09cc | 0x0ba0) => Some("DualShock 4"),
        (0x054c, 0x0ce6) => Some("DualSense"),
        (0x054c, 0x0df2) => Some("DualSense Edge"),
        (0x057e, 0x2009) => Some("Switch Pro Controller"),
        (0x045e, _) => Some("Xbox pad (HID)"),
        _ => None,
    };
    match known {
        Some(name) => String::from(name),
        None if map.usage == 4 => String::from("HID joystick"),
        None => String::from("HID gamepad"),
    }
}

// ---------------------------------------------------------------------------
// Xbox vendor interfaces
// ---------------------------------------------------------------------------

/// The vendor interface of an Xbox pad and its interrupt endpoints.
#[derive(Clone, Copy, Debug)]
pub struct XboxInterface {
    pub kind: XboxKind,
    pub configuration: u8,
    pub interface: u8,
    /// IN endpoint: number (without the direction bit), packet, `bInterval`.
    pub input: (u8, u16, u8),
    /// OUT endpoint, if any: number, packet, `bInterval`.
    pub output: Option<(u8, u16, u8)>,
}

/// Find the first Xbox interface (alternate setting 0) in a configuration.
#[must_use]
pub fn find_xbox(bytes: &[u8]) -> Option<XboxInterface> {
    let configuration = *bytes.get(5)?;
    let mut at = 0;
    let mut found: Option<XboxInterface> = None;
    let mut inside = false;
    while at + 2 <= bytes.len() {
        let length = usize::from(bytes[at]);
        if length < 2 || at + length > bytes.len() {
            break;
        }
        match bytes[at + 1] {
            super::DESC_INTERFACE if length >= 9 => {
                if found.is_some_and(|xbox| xbox.input.0 != 0) {
                    break;
                }
                let kind = XboxKind::of_interface(bytes[at + 5], bytes[at + 6], bytes[at + 7]);
                inside = kind.is_some() && bytes[at + 3] == 0;
                if let (true, Some(kind)) = (inside, kind) {
                    found = Some(XboxInterface { kind, configuration, interface: bytes[at + 2], input: (0, 0, 0), output: None });
                }
            }
            super::DESC_ENDPOINT if length >= 7 && inside => {
                let (address, attributes) = (bytes[at + 2], bytes[at + 3]);
                let packet = u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]) & 0x7FF;
                let endpoint = (address & 0x0F, packet, bytes[at + 6]);
                if let Some(xbox) = found.as_mut() {
                    if attributes & 0b11 == 0b11 {
                        if address & 0x80 != 0 {
                            if xbox.input.0 == 0 {
                                xbox.input = endpoint;
                            }
                        } else if xbox.output.is_none() {
                            xbox.output = Some(endpoint);
                        }
                    }
                }
            }
            _ => {}
        }
        at += length;
    }
    found.filter(|xbox| xbox.input.0 != 0)
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// The test pad of `gamepad fake`.
static FAKE: SpinLock<Option<(PadHandle, PadState)>> = SpinLock::new(None);

/// Button by name (`a`, `lb`, `start`, `guide` ...).
fn button(name: &str) -> Option<u16> {
    pad::BUTTON_NAMES.iter().position(|known| known.eq_ignore_ascii_case(name)).map(|bit| 1 << bit)
}

/// `gamepad [log on|off | fake [off | press B | release B | axis lx|ly|rx|ry|lt|rt V | dpad X Y]]`.
/// Returns the lines to print.
pub fn shell(argument: &str) -> Vec<String> {
    let mut out = Vec::new();
    let words: Vec<&str> = argument.split_whitespace().collect();
    match words.as_slice() {
        [] => {}
        ["log", on @ ("on" | "off")] => LOG.store(*on == "on", Ordering::Relaxed),
        // What programs ask of `/dev/input` and its sysfs (libudev of Wine).
        #[cfg(target_arch = "x86_64")]
        ["trace", on @ ("on" | "off")] => crate::linux::evdev::set_trace(*on == "on"),
        ["fake"] | ["fake", "on"] => {
            let mut fake = FAKE.lock();
            if fake.is_none() {
                // The ids of the Xbox 360 pad: what the game sees.
                *fake = attach("FossAi test pad", "test", 0x045e, 0x028e).map(|handle| (handle, PadState::default()));
            }
        }
        ["fake", "off"] => {
            let gone = FAKE.lock().take();
            drop(gone);
        }
        ["fake", rest @ ..] => {
            let mut fake = FAKE.lock();
            let Some((handle, state)) = fake.as_mut() else {
                out.push(String::from("  gamepad: no test pad, `gamepad fake` first"));
                return out;
            };
            let number = |text: &str| text.parse::<i32>().ok();
            let ok = match rest {
                ["press" | "release", name] => match button(name) {
                    Some(bit) => {
                        if rest[0] == "press" {
                            state.buttons |= bit;
                        } else {
                            state.buttons &= !bit;
                        }
                        true
                    }
                    None => false,
                },
                ["axis", axis, value] => match number(value) {
                    Some(value) => {
                        let stick = value.clamp(-32768, 32767) as i16;
                        let trigger = value.clamp(0, 255) as u8;
                        match *axis {
                            "lx" => state.lx = stick,
                            "ly" => state.ly = stick,
                            "rx" => state.rx = stick,
                            "ry" => state.ry = stick,
                            "lt" => state.lt = trigger,
                            "rt" => state.rt = trigger,
                            _ => {}
                        }
                        matches!(*axis, "lx" | "ly" | "rx" | "ry" | "lt" | "rt")
                    }
                    None => false,
                },
                ["dpad", x, y] => match (number(x), number(y)) {
                    (Some(x), Some(y)) => {
                        state.hat_x = x.signum() as i8;
                        state.hat_y = y.signum() as i8;
                        true
                    }
                    _ => false,
                },
                ["reset"] => {
                    *state = PadState::default();
                    true
                }
                _ => false,
            };
            if ok {
                handle.update(*state);
            } else {
                out.push(String::from(
                    "  gamepad: `gamepad fake [on|off|reset|press B|release B|axis lx|ly|rx|ry|lt|rt V|dpad X Y]`",
                ));
            }
        }
        _ => out.push(String::from("  gamepad: usage `gamepad [log on|off] [fake ...]`")),
    }
    let list = pads();
    if list.is_empty() {
        out.push(String::from("  gamepad: no gamepads"));
    }
    for pad in list {
        out.push(alloc::format!(
            "  gamepad {}: {} ({:04x}:{:04x}, {}), /dev/input/event{}, {} change(s)",
            pad.index,
            pad.name,
            pad.vendor,
            pad.product,
            pad.kind,
            pad.index,
            pad.changes
        ));
        out.push(alloc::format!("    {}", describe(&pad.state)));
    }
    out.push(alloc::format!("  gamepad: log {}", if LOG.load(Ordering::Relaxed) { "on" } else { "off" }));
    out
}
