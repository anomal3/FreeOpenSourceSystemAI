//! USB: то, что не зависит ни от контроллера, ни от архитектуры.
//!
//! Здесь живут константы протокола, разбор дескрипторов и перевод отчётов HID в
//! события ввода. Контроллер (пока единственный — [`xhci`]) знает, как доставить
//! пакет, и ничего не знает о том, что в нём.
//!
//! # Область: только xHCI и только boot protocol
//!
//! Сужение сознательное и записано в плане проекта. Legacy-контроллеры (UHCI,
//! OHCI, EHCI) пропущены целиком: всё железо, на котором эта система должна
//! работать, — Raspberry Pi 4 (VL805) и современный x86 — предоставляет xHCI, а
//! QEMU эмулирует его же. Из HID берётся только **boot protocol**: у клавиатуры
//! это отчёт фиксированного вида (модификаторы плюс до шести нажатых клавиш),
//! который не требует разбора HID Report Descriptor — а полный его разбор
//! является отдельной подсистемой размером с этот модуль.
//!
//! Цена: клавиатуры, не поддерживающие boot protocol, работать не будут. Таких
//! почти нет — boot protocol обязателен для любой клавиатуры, которая должна
//! работать в BIOS.

pub mod hid;
pub mod xhci;

use crate::input::KeyCode;

// ---------------------------------------------------------------------------
// Стандартные запросы (USB 2.0, глава 9)
// ---------------------------------------------------------------------------

/// `bmRequestType`: направление IN (устройство → хост).
pub const REQ_DIR_IN: u8 = 0x80;
/// Тип запроса: класс устройства (а не стандартный).
pub const REQ_TYPE_CLASS: u8 = 0x20;
/// Получатель: интерфейс (а не устройство).
pub const REQ_RECIPIENT_INTERFACE: u8 = 0x01;

pub const REQ_GET_DESCRIPTOR: u8 = 6;
pub const REQ_SET_CONFIGURATION: u8 = 9;

/// HID-специфичные запросы (HID 1.11, 7.2).
pub const REQ_HID_SET_IDLE: u8 = 0x0A;
pub const REQ_HID_SET_PROTOCOL: u8 = 0x0B;

/// Значение `wValue` для boot protocol в `SET_PROTOCOL`.
pub const HID_PROTOCOL_BOOT: u16 = 0;

// ---------------------------------------------------------------------------
// Дескрипторы
// ---------------------------------------------------------------------------

pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIGURATION: u8 = 2;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;

/// Класс интерфейса HID.
pub const CLASS_HID: u8 = 3;
/// Подкласс «boot interface»: интерфейс обязан понимать boot protocol.
pub const SUBCLASS_BOOT: u8 = 1;
/// Протокол «клавиатура».
pub const PROTOCOL_KEYBOARD: u8 = 1;

/// Длина дескриптора устройства.
pub const DEVICE_DESC_LEN: usize = 18;
/// Длина дескриптора конфигурации (без вложенных).
pub const CONFIG_DESC_LEN: usize = 9;

/// Дескриптор устройства — только те поля, которые ядру нужны.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeviceDescriptor {
    /// Версия USB в BCD.
    pub usb_version: u16,
    /// Размер пакета для конечной точки 0. Значение важно знать до того, как
    /// читать что-то длиннее восьми байт: контроллеру надо сообщить его в
    /// контексте конечной точки, а до чтения дескриптора оно неизвестно.
    pub max_packet_size0: u8,
    pub vendor: u16,
    pub product: u16,
    pub configurations: u8,
}

impl DeviceDescriptor {
    /// Разобрать первые байты дескриптора устройства.
    ///
    /// Работает и на восьми байтах: именно столько читается первым запросом,
    /// чтобы узнать `max_packet_size0`.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 8 || bytes[1] != DESC_DEVICE {
            return None;
        }
        let mut desc = Self {
            usb_version: u16::from_le_bytes([bytes[2], bytes[3]]),
            max_packet_size0: bytes[7],
            ..Self::default()
        };
        if bytes.len() >= DEVICE_DESC_LEN {
            desc.vendor = u16::from_le_bytes([bytes[8], bytes[9]]);
            desc.product = u16::from_le_bytes([bytes[10], bytes[11]]);
            desc.configurations = bytes[17];
        }
        Some(desc)
    }
}

/// Найденный интерфейс HID-клавиатуры и её конечная точка прерываний.
#[derive(Clone, Copy, Debug)]
pub struct KeyboardInterface {
    /// Значение `bConfigurationValue`, которое надо выставить `SET_CONFIGURATION`.
    pub configuration: u8,
    /// Номер интерфейса — он же `wIndex` в запросах класса HID.
    pub interface: u8,
    /// Номер конечной точки (без бита направления).
    pub endpoint: u8,
    /// Размер пакета этой конечной точки. У boot-клавиатуры это 8, но брать
    /// значение из дескриптора, а не из ожиданий, — единственный способ не
    /// сломаться на устройстве, которое сообщает больше.
    pub max_packet_size: u16,
    /// `bInterval` из дескриптора: как часто хост должен опрашивать точку.
    pub interval: u8,
}

/// Найти в дескрипторе конфигурации интерфейс boot-клавиатуры.
///
/// `bytes` — конфигурация целиком, вместе с вложенными дескрипторами
/// интерфейсов и конечных точек: устройство отдаёт их одним куском, и разбирать
/// их надо тоже одним проходом.
#[must_use]
pub fn find_keyboard(bytes: &[u8]) -> Option<KeyboardInterface> {
    if bytes.len() < CONFIG_DESC_LEN || bytes[1] != DESC_CONFIGURATION {
        return None;
    }
    let configuration = bytes[5];

    let mut offset = usize::from(bytes[0]);
    let mut current: Option<KeyboardInterface> = None;

    while offset + 2 <= bytes.len() {
        let length = usize::from(bytes[offset]);
        let kind = bytes[offset + 1];
        // Нулевая длина — это бесконечный цикл, а не пустой дескриптор.
        if length < 2 || offset + length > bytes.len() {
            break;
        }

        match kind {
            DESC_INTERFACE if length >= 9 => {
                let class = bytes[offset + 5];
                let subclass = bytes[offset + 6];
                let protocol = bytes[offset + 7];
                current = if class == CLASS_HID
                    && subclass == SUBCLASS_BOOT
                    && protocol == PROTOCOL_KEYBOARD
                {
                    Some(KeyboardInterface {
                        configuration,
                        interface: bytes[offset + 2],
                        endpoint: 0,
                        max_packet_size: 0,
                        interval: 0,
                    })
                } else {
                    // Не наш интерфейс: его конечные точки нас не касаются, и
                    // забыть про него надо здесь, а не проверять потом.
                    None
                };
            }
            DESC_ENDPOINT if length >= 7 => {
                if let Some(mut found) = current {
                    let address = bytes[offset + 2];
                    let attributes = bytes[offset + 3];
                    // Нужна точка типа Interrupt (биты 1:0 = 11) и направления
                    // IN (бит 7 адреса). Клавиатура может объявлять и OUT-точку
                    // — для светодиодов, — и перепутать их значит ждать отчёты
                    // оттуда, откуда они не приходят.
                    if attributes & 0b11 == 0b11 && address & REQ_DIR_IN != 0 {
                        found.endpoint = address & 0x0F;
                        found.max_packet_size =
                            u16::from_le_bytes([bytes[offset + 4], bytes[offset + 5]]) & 0x07FF;
                        found.interval = bytes[offset + 6];
                        return Some(found);
                    }
                }
            }
            _ => {}
        }

        offset += length;
    }
    None
}

// ---------------------------------------------------------------------------
// Usage ID → код клавиши
// ---------------------------------------------------------------------------

/// Перевести HID usage ID (страница 0x07) в код клавиши ядра.
///
/// Таблица получается почти тождественной [`KeyCode`], и это не совпадение:
/// имена вариантов там выбраны по usage-именам HID именно затем, чтобы этот
/// перевод не превратился в словарь наименований.
#[must_use]
pub const fn key_for_usage(usage: u8) -> Option<KeyCode> {
    let key = match usage {
        0x04 => KeyCode::A,
        0x05 => KeyCode::B,
        0x06 => KeyCode::C,
        0x07 => KeyCode::D,
        0x08 => KeyCode::E,
        0x09 => KeyCode::F,
        0x0A => KeyCode::G,
        0x0B => KeyCode::H,
        0x0C => KeyCode::I,
        0x0D => KeyCode::J,
        0x0E => KeyCode::K,
        0x0F => KeyCode::L,
        0x10 => KeyCode::M,
        0x11 => KeyCode::N,
        0x12 => KeyCode::O,
        0x13 => KeyCode::P,
        0x14 => KeyCode::Q,
        0x15 => KeyCode::R,
        0x16 => KeyCode::S,
        0x17 => KeyCode::T,
        0x18 => KeyCode::U,
        0x19 => KeyCode::V,
        0x1A => KeyCode::W,
        0x1B => KeyCode::X,
        0x1C => KeyCode::Y,
        0x1D => KeyCode::Z,
        0x1E => KeyCode::Digit1,
        0x1F => KeyCode::Digit2,
        0x20 => KeyCode::Digit3,
        0x21 => KeyCode::Digit4,
        0x22 => KeyCode::Digit5,
        0x23 => KeyCode::Digit6,
        0x24 => KeyCode::Digit7,
        0x25 => KeyCode::Digit8,
        0x26 => KeyCode::Digit9,
        0x27 => KeyCode::Digit0,
        0x28 => KeyCode::Enter,
        0x29 => KeyCode::Escape,
        0x2A => KeyCode::Backspace,
        0x2B => KeyCode::Tab,
        0x2C => KeyCode::Space,
        0x2D => KeyCode::Minus,
        0x2E => KeyCode::Equal,
        0x2F => KeyCode::LeftBracket,
        0x30 => KeyCode::RightBracket,
        // 0x31 — обратная косая, 0x32 — «non-US #», которая на клавиатурах с
        // другой раскладкой стоит на её месте. Разного кода у них у нас нет, и
        // выдумывать его незачем.
        0x31 | 0x32 => KeyCode::Backslash,
        0x33 => KeyCode::Semicolon,
        0x34 => KeyCode::Apostrophe,
        0x35 => KeyCode::Grave,
        0x36 => KeyCode::Comma,
        0x37 => KeyCode::Period,
        0x38 => KeyCode::Slash,
        0x39 => KeyCode::CapsLock,
        0x3A => KeyCode::F1,
        0x3B => KeyCode::F2,
        0x3C => KeyCode::F3,
        0x3D => KeyCode::F4,
        0x3E => KeyCode::F5,
        0x3F => KeyCode::F6,
        0x40 => KeyCode::F7,
        0x41 => KeyCode::F8,
        0x42 => KeyCode::F9,
        0x43 => KeyCode::F10,
        0x44 => KeyCode::F11,
        0x45 => KeyCode::F12,
        0x46 => KeyCode::PrintScreen,
        0x47 => KeyCode::ScrollLock,
        0x48 => KeyCode::Pause,
        0x49 => KeyCode::Insert,
        0x4A => KeyCode::Home,
        0x4B => KeyCode::PageUp,
        0x4C => KeyCode::Delete,
        0x4D => KeyCode::End,
        0x4E => KeyCode::PageDown,
        0x4F => KeyCode::Right,
        0x50 => KeyCode::Left,
        0x51 => KeyCode::Down,
        0x52 => KeyCode::Up,
        0x53 => KeyCode::NumLock,
        0x54 => KeyCode::KeypadSlash,
        0x55 => KeyCode::KeypadAsterisk,
        0x56 => KeyCode::KeypadMinus,
        0x57 => KeyCode::KeypadPlus,
        0x58 => KeyCode::KeypadEnter,
        0x59 => KeyCode::Keypad1,
        0x5A => KeyCode::Keypad2,
        0x5B => KeyCode::Keypad3,
        0x5C => KeyCode::Keypad4,
        0x5D => KeyCode::Keypad5,
        0x5E => KeyCode::Keypad6,
        0x5F => KeyCode::Keypad7,
        0x60 => KeyCode::Keypad8,
        0x61 => KeyCode::Keypad9,
        0x62 => KeyCode::Keypad0,
        0x63 => KeyCode::KeypadPeriod,
        0x65 => KeyCode::Menu,
        0xE0 => KeyCode::LeftCtrl,
        0xE1 => KeyCode::LeftShift,
        0xE2 => KeyCode::LeftAlt,
        0xE3 => KeyCode::LeftMeta,
        0xE4 => KeyCode::RightCtrl,
        0xE5 => KeyCode::RightShift,
        0xE6 => KeyCode::RightAlt,
        0xE7 => KeyCode::RightMeta,
        // 0x00 — «клавиш не нажато», 0x01 — переполнение матрицы (нажато больше,
        // чем клавиатура умеет сообщить), 0x02/0x03 — ошибки POST. Ни одно из
        // этого не клавиша.
        _ => return None,
    };
    Some(key)
}
