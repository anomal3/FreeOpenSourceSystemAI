//! virtio-input: клавиатура, мышь и планшет virtio (фаза 67g).
//!
//! # Чем это проще USB
//!
//! USB HID — это перечисление устройства, дескрипторы, отчёты в формате,
//! который описывает сам отчёт, и опрос конечной точки. virtio-input отдаёт
//! готовые события в виде evdev Linux: `(вид, код, значение)` по восемь байт,
//! в очередь, буферы которой драйвер подкладывает заранее, — и сигналит
//! прерыванием. Разбирать нечего: клавиша — это код evdev, движение — `REL_X`
//! или `ABS_X`, конец отчёта — `SYN_REPORT`.
//!
//! # Что куда
//!
//! Клавиши — в ту же очередь, что от PS/2 и USB (`input::post`), по таблице
//! кодов evdev ниже. Движение и кнопки копятся до `SYN_REPORT` и уходят одним
//! отчётом: мышь — приращением (`post_pointer`), планшет — долями экрана
//! (`post_pointer_at`), диапазон оси у устройства свой, и он спрашивается в
//! конфигурации (`ABS_INFO`).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use super::{DESC_F_WRITE, FEATURE_VERSION_1, Queue, Transport, VirtioError, map_bar};
use crate::input::{self, Buttons, KeyCode};
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer};
use crate::pci::{self, Device, MSIX_ENTRY_SIZE};

/// Идентификатор устройства virtio-input в современном виде (`0x1040 + 18`).
pub const DEVICE_VIRTIO_INPUT: u16 = 0x1052;

// Конфигурация: выбор, подвыбор, размер ответа, данные с 8-го байта.
const CFG_SELECT: usize = 0;
const CFG_SUBSEL: usize = 1;
const CFG_SIZE: usize = 2;
const CFG_DATA: usize = 8;
const CFG_ID_NAME: u8 = 0x01;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

// Виды и коды событий evdev.
const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_WHEEL: u16 = 8;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_TOUCH: u16 = 0x14A;

/// Буферов событий в очереди — по восемь байт.
const EVENTS: usize = 16;

/// Все поднятые устройства. Спин-лок: читает их обработчик прерывания.
static INPUTS: crate::sync::SpinLock<Vec<VirtioInput>> = crate::sync::SpinLock::new(Vec::new());

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Keyboard,
    Mouse,
    Tablet,
}

struct VirtioInput {
    _transport: Transport,
    events: Queue,
    buffers: DmaBuffer,
    kind: Kind,
    name: String,
    /// Диапазон осей планшета: `(min, max)` по X и по Y.
    range: [(i32, i32); 2],
    /// Отчёт, который копится до `SYN_REPORT`.
    dx: i32,
    dy: i32,
    wheel: i32,
    x: i32,
    y: i32,
    buttons: Buttons,
    moved: bool,
    /// Первое событие уже названо в журнале.
    heard: bool,
}

/// Найти все устройства virtio-input, поднять их и дописать источники ввода.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц, прерывания —
/// уметь доставлять MSI.
pub unsafe fn probe_all(root: &pci::Root) {
    let mut found = Vec::new();
    // SAFETY: контракт функции.
    unsafe {
        pci::for_each(root, |device| {
            if device.vendor == pci::VENDOR_VIRTIO && device.device == DEVICE_VIRTIO_INPUT {
                found.push(*device);
            }
            true
        });
    }
    let (mut keyboard, mut pointer) = (false, false);
    for device in found {
        // SAFETY: контракт функции.
        match unsafe { VirtioInput::attach(&device) } {
            Ok(input) => {
                crate::devices::claim(device.address, "virtio-input");
                kprintln!(
                    "  virtio-input: at {}, '{}' as a {}",
                    device.address,
                    input.name,
                    match input.kind {
                        Kind::Keyboard => "keyboard",
                        Kind::Mouse => "mouse",
                        Kind::Tablet => "tablet",
                    }
                );
                keyboard |= input.kind == Kind::Keyboard;
                pointer |= input.kind != Kind::Keyboard;
                INPUTS.lock().push(input);
            }
            Err(err) => kprintln!("  virtio-input: at {} unusable: {err}", device.address),
        }
    }
    if keyboard || pointer {
        let sources = input::sources();
        input::set_sources(input::Sources {
            keyboard: sources.keyboard || keyboard,
            mouse: sources.mouse || pointer,
            ..sources
        });
    }
}

/// Прерывание любого из устройств: разобрать всё, что пришло, у всех.
fn on_interrupt() {
    let mut inputs = INPUTS.lock();
    for input in inputs.iter_mut() {
        input.drain();
    }
}

impl VirtioInput {
    /// # Safety
    ///
    /// См. [`probe_all`].
    unsafe fn attach(device: &Device) -> Result<Self, VirtioError> {
        // SAFETY: устройство не тронет память до `DRIVER_OK`.
        unsafe { device.enable_bus_master() };
        // SAFETY: контракт функции.
        let transport = unsafe { Transport::open(device) }?;
        transport.negotiate(FEATURE_VERSION_1)?;
        let events = Queue::new(&transport, 0)?;
        let buffers = dma::alloc(EVENTS * 8).map_err(VirtioError::NoMemory)?;

        let name = config_text(&transport, CFG_ID_NAME, 0);
        let kind = if config_size(&transport, CFG_EV_BITS, EV_ABS as u8) > 0 {
            Kind::Tablet
        } else if config_size(&transport, CFG_EV_BITS, EV_REL as u8) > 0 {
            Kind::Mouse
        } else {
            Kind::Keyboard
        };
        let range = [abs_range(&transport, ABS_X as u8), abs_range(&transport, ABS_Y as u8)];

        let mut input = Self {
            _transport: transport,
            events,
            buffers,
            kind,
            name,
            range,
            dx: 0,
            dy: 0,
            wheel: 0,
            x: 0,
            y: 0,
            buttons: Buttons::NONE,
            moved: false,
            heard: false,
        };
        // Все буферы — устройству сразу: событие, пришедшее, когда класть
        // некуда, устройство держит у себя, а не теряет, но ждать его незачем.
        let phys = input.buffers.phys().as_u64();
        for index in 0..EVENTS {
            input.events.set_descriptor(index as u16, phys + (index * 8) as u64, 8, DESC_F_WRITE, 0);
            input.events.offer(index as u16);
        }
        input._transport.set_driver_ok();
        input.events.kick();

        // SAFETY: контракт функции.
        if !unsafe { input.enable_interrupts(device) } {
            return Err(VirtioError::NoCapabilities);
        }
        Ok(input)
    }

    /// Прерывание по очереди событий — без него устройство бесполезно:
    /// опрашивать его некому, а ввод должен будить систему.
    ///
    /// # Safety
    ///
    /// См. [`probe_all`].
    unsafe fn enable_interrupts(&mut self, device: &Device) -> bool {
        let Some(msix) = device.msix() else {
            return false;
        };
        let Some((address, data)) = crate::arch::interrupts::alloc_msi(on_interrupt) else {
            return false;
        };
        let span = u64::from(msix.table_offset) + (MSIX_ENTRY_SIZE * usize::from(msix.vectors)) as u64;
        // SAFETY: контракт функции; окно — регистры устройства.
        let Ok(base) = (unsafe { map_bar(device, msix.bir as u8, span) }) else {
            return false;
        };
        let table = base.as_usize() + msix.table_offset as usize;
        // SAFETY: таблица отображена, строка 0 существует всегда.
        unsafe { device.set_msix_vector(&msix, table, 0, address, data) };
        // SAFETY: MSI-X включён записью выше.
        unsafe { Queue::silence_config_changes(&self._transport) };
        // SAFETY: см. выше.
        unsafe { self.events.want_interrupts(&self._transport, 0) }
    }

    /// Разобрать пришедшие события и вернуть буферы устройству.
    fn drain(&mut self) {
        let mut any = false;
        while let Some((index, _)) = self.events.take_used() {
            any = true;
            fence(Ordering::SeqCst);
            // SAFETY: номер проверен очередью; буфер — восемь байт на событие.
            let (kind, code, value) = unsafe {
                let at = self.buffers.as_ptr::<u8>().add(usize::from(index) * 8);
                (
                    at.cast::<u16>().read_volatile(),
                    at.add(2).cast::<u16>().read_volatile(),
                    at.add(4).cast::<i32>().read_volatile(),
                )
            };
            self.event(kind, code, value);
            self.events.offer(index);
        }
        if any {
            self.events.kick();
        }
    }

    fn event(&mut self, kind: u16, code: u16, value: i32) {
        if !self.heard && kind != EV_SYN {
            self.heard = true;
            kprintln!("  virtio-input: first event from '{}'", self.name);
        }
        match kind {
            EV_KEY => {
                let button = match code {
                    BTN_LEFT | BTN_TOUCH => Some(Buttons::LEFT),
                    BTN_RIGHT => Some(Buttons::RIGHT),
                    BTN_MIDDLE => Some(Buttons::MIDDLE),
                    _ => None,
                };
                if let Some(button) = button {
                    let bits = if value != 0 { self.buttons.bits() | button.bits() } else { self.buttons.bits() & !button.bits() };
                    self.buttons = Buttons::from_bits(bits);
                    self.moved = true;
                } else if value != 2 {
                    // Двойка — автоповтор устройства; повторяет клавиши ядро само.
                    input::post(key_of(code), value != 0);
                }
            }
            EV_REL => {
                match code {
                    REL_X => self.dx += value,
                    REL_Y => self.dy += value,
                    REL_WHEEL => self.wheel += value,
                    _ => return,
                }
                self.moved = true;
            }
            EV_ABS => {
                match code {
                    ABS_X => self.x = value,
                    ABS_Y => self.y = value,
                    _ => return,
                }
                self.moved = true;
            }
            EV_SYN if self.moved => {
                self.moved = false;
                if self.kind == Kind::Tablet {
                    let fraction = |value: i32, (min, max): (i32, i32)| {
                        let span = i64::from(max - min).max(1);
                        ((i64::from(value - min).clamp(0, span) * 65_535) / span) as u16
                    };
                    let (x, y) = (fraction(self.x, self.range[0]), fraction(self.y, self.range[1]));
                    input::post_pointer_at(x, y, self.wheel, self.buttons);
                } else {
                    // Колесо — как у USB HID: плюс — от себя, как и `REL_WHEEL` evdev.
                    input::post_pointer(self.dx, self.dy, self.wheel, self.buttons);
                }
                self.dx = 0;
                self.dy = 0;
                self.wheel = 0;
            }
            _ => {}
        }
    }
}

/// Выбрать в конфигурации раздел и подраздел и узнать размер ответа.
fn config_size(transport: &Transport, select: u8, subsel: u8) -> u8 {
    // SAFETY: поля конфигурации virtio-input — в окне устройства.
    unsafe {
        transport.device_config_write8(CFG_SELECT, select);
        transport.device_config_write8(CFG_SUBSEL, subsel);
        transport.device_config8(CFG_SIZE)
    }
}

/// Строка из конфигурации (имя устройства).
fn config_text(transport: &Transport, select: u8, subsel: u8) -> String {
    let size = usize::from(config_size(transport, select, subsel)).min(128);
    (0..size)
        // SAFETY: см. `config_size`; ответ не длиннее 128 байт.
        .map(|at| char::from(unsafe { transport.device_config8(CFG_DATA + at) }))
        .filter(|ch| !ch.is_control())
        .collect()
}

/// Диапазон оси планшета: `ABS_INFO` — `min`, `max` первыми полями.
fn abs_range(transport: &Transport, axis: u8) -> (i32, i32) {
    if config_size(transport, CFG_ABS_INFO, axis) < 8 {
        return (0, 32_767);
    }
    let field = |offset: usize| {
        // SAFETY: см. `config_size`; поле — четыре байта внутри ответа.
        let bytes: [u8; 4] = core::array::from_fn(|at| unsafe { transport.device_config8(CFG_DATA + offset + at) });
        i32::from_le_bytes(bytes)
    };
    (field(0), field(4))
}

/// Код evdev — в код клавиши. В основном блоке коды evdev совпадают со
/// скан-кодами PS/2 набора 1; дальше — свои номера Linux.
const fn key_of(code: u16) -> KeyCode {
    use KeyCode as K;
    match code {
        1 => K::Escape,
        2 => K::Digit1, 3 => K::Digit2, 4 => K::Digit3, 5 => K::Digit4, 6 => K::Digit5,
        7 => K::Digit6, 8 => K::Digit7, 9 => K::Digit8, 10 => K::Digit9, 11 => K::Digit0,
        12 => K::Minus, 13 => K::Equal, 14 => K::Backspace, 15 => K::Tab,
        16 => K::Q, 17 => K::W, 18 => K::E, 19 => K::R, 20 => K::T, 21 => K::Y, 22 => K::U,
        23 => K::I, 24 => K::O, 25 => K::P, 26 => K::LeftBracket, 27 => K::RightBracket,
        28 => K::Enter, 29 => K::LeftCtrl,
        30 => K::A, 31 => K::S, 32 => K::D, 33 => K::F, 34 => K::G, 35 => K::H, 36 => K::J,
        37 => K::K, 38 => K::L, 39 => K::Semicolon, 40 => K::Apostrophe, 41 => K::Grave,
        42 => K::LeftShift, 43 => K::Backslash,
        44 => K::Z, 45 => K::X, 46 => K::C, 47 => K::V, 48 => K::B, 49 => K::N, 50 => K::M,
        51 => K::Comma, 52 => K::Period, 53 => K::Slash, 54 => K::RightShift,
        55 => K::KeypadAsterisk, 56 => K::LeftAlt, 57 => K::Space, 58 => K::CapsLock,
        59 => K::F1, 60 => K::F2, 61 => K::F3, 62 => K::F4, 63 => K::F5,
        64 => K::F6, 65 => K::F7, 66 => K::F8, 67 => K::F9, 68 => K::F10,
        69 => K::NumLock, 70 => K::ScrollLock,
        71 => K::Keypad7, 72 => K::Keypad8, 73 => K::Keypad9, 74 => K::KeypadMinus,
        75 => K::Keypad4, 76 => K::Keypad5, 77 => K::Keypad6, 78 => K::KeypadPlus,
        79 => K::Keypad1, 80 => K::Keypad2, 81 => K::Keypad3, 82 => K::Keypad0,
        83 => K::KeypadPeriod, 87 => K::F11, 88 => K::F12,
        96 => K::KeypadEnter, 97 => K::RightCtrl, 98 => K::KeypadSlash, 99 => K::PrintScreen,
        100 => K::RightAlt, 102 => K::Home, 103 => K::Up, 104 => K::PageUp, 105 => K::Left,
        106 => K::Right, 107 => K::End, 108 => K::Down, 109 => K::PageDown, 110 => K::Insert,
        111 => K::Delete, 119 => K::Pause, 125 => K::LeftMeta, 126 => K::RightMeta,
        127 => K::Menu,
        _ => K::Unknown,
    }
}
