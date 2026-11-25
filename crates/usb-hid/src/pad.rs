//! Gamepads (24.11.2026): one state for every pad the kernel reads, and the
//! reports of the Xbox pads, which are not HID at all.
//!
//! # One state, the layout of the Xbox 360 pad
//!
//! Games under Proton see a pad through Wine's `winebus.sys`, and the pad
//! Wine knows best is the one Steam gives it: a Linux evdev device named
//! "Microsoft X-Box 360 pad N" in the layout of the `xpad` driver. So every
//! pad - Xbox 360, Xbox One/Series, DualShock 4, DualSense, a generic HID
//! pad - is brought to that layout here, and the kernel shows all of them the
//! same way: two sticks, two analog triggers, a d-pad and eleven buttons.
//!
//! # The Xbox reports
//!
//! Xbox pads declare a vendor interface (class 0xFF) and speak their own
//! protocols, decoded here byte for byte as Linux `drivers/input/joystick/xpad.c`
//! does:
//!
//! * Xbox 360 wired (subclass 0x5D, protocol 0x01): a 20-byte report, type 0;
//! * Xbox 360 wireless receiver (0x5D/0x81): the same report after a 4-byte
//!   header, plus "pad connected / gone" messages;
//! * Xbox One / Series (GIP, 0x47/0xD0): the pad is silent until the host
//!   sends "power on", input comes in command 0x20, the Guide button in its
//!   own command 0x07 which some pads want acknowledged.

/// Buttons of [`PadState::buttons`], in the order of the evdev codes
/// `BTN_A`.. `BTN_THUMBR` of the `xpad` driver.
pub const BTN_A: u16 = 1 << 0;
pub const BTN_B: u16 = 1 << 1;
pub const BTN_X: u16 = 1 << 2;
pub const BTN_Y: u16 = 1 << 3;
pub const BTN_LB: u16 = 1 << 4;
pub const BTN_RB: u16 = 1 << 5;
pub const BTN_BACK: u16 = 1 << 6;
pub const BTN_START: u16 = 1 << 7;
pub const BTN_GUIDE: u16 = 1 << 8;
pub const BTN_LS: u16 = 1 << 9;
pub const BTN_RS: u16 = 1 << 10;

/// Names of the buttons, bit by bit (for the shell and the log).
pub const BUTTON_NAMES: [&str; 11] = ["A", "B", "X", "Y", "LB", "RB", "Back", "Start", "Guide", "LS", "RS"];

/// Hat direction (0 up, clockwise) as (x, y), y down positive.
pub const HAT_VECTORS: [(i8, i8); 8] = [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)];

/// State of a pad.
///
/// Sticks -32768..32767 with Y **down** positive (the Linux convention; the
/// Xbox reports say up and are turned over here, as `xpad` does), triggers
/// 0..255, the d-pad -1..1 per axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PadState {
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
    pub lt: u8,
    pub rt: u8,
    pub hat_x: i8,
    pub hat_y: i8,
    pub buttons: u16,
}

impl PadState {
    /// Names of the pressed buttons, joined by spaces (`-` for none).
    #[must_use]
    pub fn pressed(&self) -> PressedNames {
        PressedNames(self.buttons)
    }
}

/// Display of the pressed buttons.
pub struct PressedNames(u16);

impl core::fmt::Display for PressedNames {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut any = false;
        for (bit, name) in BUTTON_NAMES.iter().enumerate() {
            if self.0 & (1 << bit) != 0 {
                if any {
                    f.write_str(" ")?;
                }
                f.write_str(name)?;
                any = true;
            }
        }
        if !any {
            f.write_str("-")?;
        }
        Ok(())
    }
}

/// Which Xbox protocol an interface speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XboxKind {
    /// Xbox 360 wired pad (and the many "XInput" pads: 8BitDo, Logitech in X
    /// mode, PowerA ...).
    X360,
    /// Xbox 360 wireless receiver: up to four pads, one interface each.
    X360Wireless,
    /// Xbox One / Series (GIP).
    XboxOne,
}

impl XboxKind {
    /// By class, subclass and protocol of an interface.
    #[must_use]
    pub const fn of_interface(class: u8, subclass: u8, protocol: u8) -> Option<Self> {
        match (class, subclass, protocol) {
            (0xFF, 0x5D, 0x01) => Some(Self::X360),
            (0xFF, 0x5D, 0x81) => Some(Self::X360Wireless),
            (0xFF, 0x47, 0xD0) => Some(Self::XboxOne),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::X360 => "Xbox 360",
            Self::X360Wireless => "Xbox 360 wireless",
            Self::XboxOne => "Xbox One",
        }
    }
}

/// A little-endian `i16` at `at`.
fn le_i16(data: &[u8], at: usize) -> i16 {
    i16::from_le_bytes([data[at], data[at + 1]])
}

/// Y of an Xbox stick (up positive) in the Linux sense: `~value`, exactly as
/// `xpad` (-32768 becomes 32767 and back, no overflow).
const fn flip(value: i16) -> i16 {
    !value
}

/// The Xbox 360 report (wired, or the wireless one after its header): type 0,
/// 20 bytes. `None` - another message (LED state, rumble ack ...).
#[must_use]
pub fn decode_x360(data: &[u8]) -> Option<PadState> {
    if data.len() < 14 || data[0] != 0x00 {
        return None;
    }
    let mut pad = PadState {
        lx: le_i16(data, 6),
        ly: flip(le_i16(data, 8)),
        rx: le_i16(data, 10),
        ry: flip(le_i16(data, 12)),
        lt: data[4],
        rt: data[5],
        hat_x: i8::from(data[2] & 0x08 != 0) - i8::from(data[2] & 0x04 != 0),
        hat_y: i8::from(data[2] & 0x02 != 0) - i8::from(data[2] & 0x01 != 0),
        buttons: 0,
    };
    for (byte, bit, button) in [
        (2, 4, BTN_START),
        (2, 5, BTN_BACK),
        (2, 6, BTN_LS),
        (2, 7, BTN_RS),
        (3, 0, BTN_LB),
        (3, 1, BTN_RB),
        (3, 2, BTN_GUIDE),
        (3, 4, BTN_A),
        (3, 5, BTN_B),
        (3, 6, BTN_X),
        (3, 7, BTN_Y),
    ] {
        if data[byte] & (1 << bit) != 0 {
            pad.buttons |= button;
        }
    }
    Some(pad)
}

/// A message of the Xbox 360 wireless receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wireless {
    /// A pad connected (`true`) or went away.
    Presence(bool),
    Input(PadState),
    Other,
}

/// Decode a report of the wireless receiver (`xpad360w_process_packet`).
#[must_use]
pub fn decode_x360_wireless(data: &[u8]) -> Wireless {
    if data.len() < 2 {
        return Wireless::Other;
    }
    if data[0] & 0x08 != 0 {
        return Wireless::Presence(data[1] & 0x80 != 0);
    }
    if data[1] != 0x01 || data.len() < 4 + 14 {
        return Wireless::Other;
    }
    decode_x360(&data[4..]).map_or(Wireless::Other, Wireless::Input)
}

/// Ask the wireless receiver whether a pad is there (`xpad_inquiry_pad_presence`).
pub const X360W_INQUIRY: [u8; 12] = [0x08, 0x00, 0x0F, 0xC0, 0, 0, 0, 0, 0, 0, 0, 0];

/// Player LED of a wired Xbox 360 pad: "flash, then on" for player
/// `index % 4 + 1` (`xpad_send_led_command`).
#[must_use]
pub const fn x360_led(index: u8) -> [u8; 3] {
    [0x01, 0x03, 0x02 + index % 4]
}

/// Player LED of a pad behind the wireless receiver.
#[must_use]
pub const fn x360w_led(index: u8) -> [u8; 12] {
    [0x00, 0x00, 0x08, 0x40 + (0x02 + index % 4), 0, 0, 0, 0, 0, 0, 0, 0]
}

/// GIP commands.
const GIP_ACK: u8 = 0x01;
const GIP_ANNOUNCE: u8 = 0x02;
const GIP_VIRTUAL_KEY: u8 = 0x07;
const GIP_INPUT: u8 = 0x20;
const GIP_OPT_ACK: u8 = 0x10;
const GIP_OPT_INTERNAL: u8 = 0x20;

/// A message of an Xbox One / Series pad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gip {
    /// Input. The Guide button is not in it: keep it from [`Gip::Guide`].
    Input(PadState),
    /// The Guide button; `ack` - the sequence number to acknowledge
    /// ([`gip_ack`]), when the pad asks for it.
    Guide { pressed: bool, ack: Option<u8> },
    /// The pad (re)announced itself: it waits for "power on" again.
    Announce,
    Other,
}

/// Decode a GIP message (`xpadone_process_packet`).
#[must_use]
pub fn decode_gip(data: &[u8]) -> Gip {
    if data.len() < 5 {
        return Gip::Other;
    }
    match data[0] {
        GIP_ANNOUNCE => Gip::Announce,
        GIP_VIRTUAL_KEY => Gip::Guide {
            pressed: data[4] & 0x03 != 0,
            ack: (data[1] == GIP_OPT_ACK | GIP_OPT_INTERNAL).then_some(data[2]),
        },
        GIP_INPUT if data.len() >= 18 => {
            let mut pad = PadState {
                lt: (u16::from_le_bytes([data[6], data[7]]).min(1023) >> 2) as u8,
                rt: (u16::from_le_bytes([data[8], data[9]]).min(1023) >> 2) as u8,
                lx: le_i16(data, 10),
                ly: flip(le_i16(data, 12)),
                rx: le_i16(data, 14),
                ry: flip(le_i16(data, 16)),
                hat_x: i8::from(data[5] & 0x08 != 0) - i8::from(data[5] & 0x04 != 0),
                hat_y: i8::from(data[5] & 0x02 != 0) - i8::from(data[5] & 0x01 != 0),
                buttons: 0,
            };
            for (byte, bit, button) in [
                (4, 2, BTN_START),
                (4, 3, BTN_BACK),
                (4, 4, BTN_A),
                (4, 5, BTN_B),
                (4, 6, BTN_X),
                (4, 7, BTN_Y),
                (5, 4, BTN_LB),
                (5, 5, BTN_RB),
                (5, 6, BTN_LS),
                (5, 7, BTN_RS),
            ] {
                if data[byte] & (1 << bit) != 0 {
                    pad.buttons |= button;
                }
            }
            Gip::Input(pad)
        }
        _ => Gip::Other,
    }
}

/// Acknowledge a Guide message (`xpadone_ack_mode_report`): without it the
/// Xbox One S pad repeats the message forever and reports no more presses.
#[must_use]
pub const fn gip_ack(seq: u8) -> [u8; 13] {
    [GIP_ACK, GIP_OPT_INTERNAL, seq, 0x09, 0x00, GIP_VIRTUAL_KEY, GIP_OPT_INTERNAL, 0x02, 0, 0, 0, 0, 0]
}

/// Init packets of an Xbox One / Series pad (`xboxone_init_packets` of
/// `xpad`): vendor, product (0 - any) and the packet; byte 2 is the sequence
/// number and is set by the sender.
const GIP_INIT: [(u16, u16, &[u8]); 15] = [
    // Hori: acknowledge its identification first.
    (0x0e6f, 0x0165, &[0x01, 0x20, 0x00, 0x09, 0x00, 0x04, 0x20, 0x3a, 0x00, 0x00, 0x00, 0x80, 0x00]),
    (0x0f0d, 0x0067, &[0x01, 0x20, 0x00, 0x09, 0x00, 0x04, 0x20, 0x3a, 0x00, 0x00, 0x00, 0x80, 0x00]),
    // Every pad: power on.
    (0, 0, &[0x05, 0x20, 0x00, 0x01, 0x00]),
    // Xbox One S and Elite 2: the "S init".
    (0x045e, 0x02ea, &[0x05, 0x20, 0x00, 0x0f, 0x06]),
    (0x045e, 0x0b00, &[0x05, 0x20, 0x00, 0x0f, 0x06]),
    (0x045e, 0x0b00, &[0x4d, 0x10, 0x00, 0x02, 0x07, 0x00]),
    // PDP: LED on and "authenticated".
    (0x0e6f, 0, &[0x0a, 0x20, 0x00, 0x03, 0x00, 0x01, 0x14]),
    (0x20d6, 0xa01a, &[0x0a, 0x20, 0x00, 0x03, 0x00, 0x01, 0x14]),
    (0x0e6f, 0, &[0x06, 0x20, 0x00, 0x02, 0x01, 0x00]),
    (0x20d6, 0xa01a, &[0x06, 0x20, 0x00, 0x02, 0x01, 0x00]),
    // PowerA: a short rumble starts the input.
    (0x24c6, 0x541a, &[0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, 0x1d, 0x1d, 0xff, 0x00, 0x00]),
    (0x24c6, 0x542a, &[0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, 0x1d, 0x1d, 0xff, 0x00, 0x00]),
    (0x24c6, 0x543a, &[0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, 0x1d, 0x1d, 0xff, 0x00, 0x00]),
    (0x24c6, 0x541a, &[0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
    (0x24c6, 0x542a, &[0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
];

/// The init packets for a pad `vendor:product`, in order.
pub fn gip_init(vendor: u16, product: u16) -> impl Iterator<Item = &'static [u8]> {
    GIP_INIT
        .iter()
        .filter(move |(v, p, _)| (*v == 0 || *v == vendor) && (*p == 0 || *p == product))
        .map(|(_, _, packet)| *packet)
}

/// Rumble of an Xbox 360 pad: big (left) and small (right) motor 0..255.
#[must_use]
pub const fn x360_rumble(big: u8, small: u8) -> [u8; 8] {
    [0x00, 0x08, 0x00, big, small, 0x00, 0x00, 0x00]
}

/// Rumble of an Xbox One pad (byte 2 - sequence): motors 0..100.
#[must_use]
pub const fn gip_rumble(big: u8, small: u8) -> [u8; 13] {
    [0x09, 0x00, 0x00, 0x09, 0x00, 0x0f, 0x00, 0x00, big, small, 0xff, 0x00, 0xeb]
}
