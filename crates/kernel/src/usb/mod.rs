//! USB: то, что не зависит ни от контроллера, ни от архитектуры.
//!
//! Здесь живут константы протокола, разбор дескрипторов и перевод отчётов HID в
//! события ввода. Контроллер ([`xhci`] или [`ohci`]) знает, как доставить пакет,
//! и ничего не знает о том, что в нём.
//!
//! # Область: xHCI и OHCI
//!
//! Сначала был только xHCI, и сужение было сознательным: всё железо, на котором
//! эта система должна работать, — Raspberry Pi 4 (VL805) и современный x86 —
//! предоставляет его, и QEMU эмулирует его же.
//!
//! Цену этого сужения назвала первая же чужая машина. У VirtualBox по умолчанию
//! включён **OHCI**, и другого контроллера там нет вовсе: система, поставленная
//! читателем в самый доступный гипервизор, не слушалась ни клавиатуры, ни мыши.
//! Отсюда второй драйвер — [`ohci`], написанный по переписи с настоящей машины
//! ([`survey`]), а не по догадке о том, что там может стоять.
//!
//! UHCI и EHCI по-прежнему пропущены, и это тоже названо вслух: EHCI сам по
//! себе не разговаривает с низко- и полноскоростными устройствами — клавиатура
//! и мышь достаются его спутнику, то есть тому же OHCI.
//!
//! # Boot protocol больше не единственный путь
//!
//! Сначала из HID брался только он: отчёт фиксированного вида, не требующий
//! разбора HID Report Descriptor. Цена такого сужения выяснилась на первой же
//! чужой машине — VirtualBox предлагает вместо мыши **планшет**, а планшет
//! boot-протокола не объявляет вовсе, и указателя в системе не было ни одного.
//!
//! Теперь дескриптор отчётов читается и разбирается ([`usb_hid`]), а
//! boot-протокол остался запасным путём — для устройства, чей дескриптор
//! разобрать не удалось. Порядок именно такой: boot-протокол это упрощение,
//! придуманное ради BIOS, а дескриптор описывает то, что устройство шлёт на
//! самом деле. Оставить проверенный путь основным и ходить по новому только на
//! чужих машинах значило бы, что новый путь проверяется там, где его некому
//! чинить.

pub mod hid;
pub mod ohci;
pub mod survey;
pub mod xhci;

use crate::input::KeyCode;
use crate::time;

// ---------------------------------------------------------------------------
// Общее для драйверов контроллеров
// ---------------------------------------------------------------------------
//
// Всё, что ниже, до появления OHCI жило внутри драйвера xHCI. Переезд сюда — не
// уборка ради уборки: шаги перечисления, ожидание с двумя пределами и запись о
// поднятом устройстве у двух контроллеров одни и те же, а две копии одного
// правила расходятся молча. Ровно этого проект и избегает: разошедшийся разбор
// отчёта выглядит как неисправная мышь, а не как ошибка в коде.

/// Предел витков холостого ожидания.
///
/// Страховка на случай остановившегося таймера: без неё отказ таймера превратил
/// бы любое ожидание в вечное. Значение подобрано так, чтобы на любой мыслимой
/// частоте оно исчерпывалось позже, чем истекает время.
const SPIN_LIMIT: u32 = 200_000_000;

/// Через сколько витков спрашивать часы. См. [`Timeout::expired`].
const CLOCK_EVERY: u32 = 64;

/// Ожидание с двумя независимыми пределами.
pub struct Timeout {
    started_ms: u64,
    until_ms: u64,
    spins: u32,
}

impl Timeout {
    #[must_use]
    pub fn new(ms: u64) -> Self {
        Self {
            started_ms: time::uptime_ms(),
            until_ms: time::uptime_ms().saturating_add(ms),
            spins: 0,
        }
    }

    /// Сколько ждали и упёрлись ли в предел витков вместо часов.
    ///
    /// Различать это обязательно. «Часы отсчитали полсекунды» — значит
    /// устройство молчит; «кончились витки» — значит часы стоят, и настоящая
    /// неисправность совсем в другом месте. На машине без журнала эти два
    /// случая выглядят одинаково: «a control transfer never completed».
    #[must_use]
    pub fn report(&self) -> (u64, bool) {
        (
            time::uptime_ms().saturating_sub(self.started_ms),
            self.spins >= SPIN_LIMIT,
        )
    }

    /// `true`, если ждать больше нельзя.
    ///
    /// # Почему часы читаются не на каждом витке
    ///
    /// Потому что чтение часов не везде стоит одинаково. На железе `CNTPCT_EL0`
    /// — это несколько тактов; под гипервизором доступ к нему с EL1 может быть
    /// перехвачен, и тогда каждое чтение стоит выхода в монитор. VirtualBox на
    /// Apple Silicon именно таков: `CNTHCTL_EL2.EL1PCTEN` у него сброшен, и
    /// цикл, спрашивавший время на каждом витке, состоял из выходов в
    /// гипервизор целиком — измерено отладчиком, счётчик команд гостя стоял на
    /// инструкции `mrs cntpct_el0` во всех выборках подряд.
    ///
    /// Раз в [`CLOCK_EVERY`] витков достаточно: ожидания здесь измеряются
    /// миллисекундами, а витков в миллисекунде тысячи. Точность предела от
    /// этого не страдает, а цена ожидания падает на два порядка.
    pub fn expired(&mut self) -> bool {
        self.spins = self.spins.saturating_add(1);
        core::hint::spin_loop();
        if self.spins >= SPIN_LIMIT {
            return true;
        }
        if self.spins % CLOCK_EVERY != 0 {
            return false;
        }
        time::uptime_ms() >= self.until_ms
    }
}

/// Подождать `ms` миллисекунд.
pub fn sleep_ms(ms: u64) {
    let mut timeout = Timeout::new(ms);
    while !timeout.expired() {}
}

/// На каком шаге перечисления остановилось устройство.
///
/// Нужен ровно там, где нет журнала: на машине без последовательного порта
/// «устройство не поднялось» — это всё, что видит человек, а шагов между
/// «порт занят» и «устройство работает» шесть, и лечатся они по-разному.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Сброс порта.
    Reset,
    /// Выделение слота и выдача адреса.
    Address,
    /// Чтение дескрипторов устройства и конфигурации.
    Describe,
    /// Настройка точки прерываний у контроллера.
    Configure,
    /// Выбор конфигурации, чтение дескриптора отчётов, протокол.
    Enable,
}

impl core::fmt::Display for Stage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Reset => "resetting the port",
            Self::Address => "addressing the device",
            Self::Describe => "reading its descriptors",
            Self::Configure => "configuring the interrupt endpoint",
            Self::Enable => "enabling reports",
        })
    }
}

/// Одно поднятое устройство в сводке.
#[derive(Clone, Copy, Debug, Default)]
pub struct Attached {
    /// Корневой порт; ноль означает пустую запись.
    pub port: u8,
    /// Изготовитель и модель из дескриптора устройства.
    pub vendor: u16,
    pub product: u16,
    /// Чем ядро его сочло: `"keyboard"` или `"mouse"`.
    pub kind: &'static str,
    /// Длина дескриптора отчётов; ноль — устройство его не объявило.
    pub descriptor: u16,
    /// Номер интерфейса, который драйвер поднял.
    pub interface: u8,
    /// Сколько интерфейсов HID у устройства всего.
    pub interfaces: u8,
}

/// Сколько записей об устройствах помещается в сводку.
///
/// Столько же слотов драйвер xHCI просит у контроллера, и столько же портов
/// перечисляет OHCI: массив в сводке существует потому, что её спрашивают в том
/// числе оттуда, где нельзя выделять память.
pub const ATTACHED_MAX: usize = 4;

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
/// Выдача адреса. Нужен драйверу OHCI и не нужен драйверу xHCI: там адрес
/// назначает сам контроллер командой `Address Device`, а здесь это обычный
/// запрос по шине, который делает драйвер.
pub const REQ_SET_ADDRESS: u8 = 5;
pub const REQ_SET_CONFIGURATION: u8 = 9;

/// HID-специфичные запросы (HID 1.11, 7.2).
pub const REQ_HID_SET_IDLE: u8 = 0x0A;
pub const REQ_HID_SET_PROTOCOL: u8 = 0x0B;

/// Значение `wValue` для boot protocol в `SET_PROTOCOL`.
pub const HID_PROTOCOL_BOOT: u16 = 0;
/// Значение `wValue` для report protocol — того, в котором формат отчёта задаёт
/// сам дескриптор. Устройство просыпается именно в нём: boot-протокол включается
/// только явным запросом.
pub const HID_PROTOCOL_REPORT: u16 = 1;

// ---------------------------------------------------------------------------
// Дескрипторы
// ---------------------------------------------------------------------------

pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIGURATION: u8 = 2;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;
/// Дескриптор HID (HID 1.11, 6.2.1): лежит внутри конфигурации, между
/// интерфейсом и его конечными точками, и сообщает длину дескриптора отчётов.
pub const DESC_HID: u8 = 0x21;
/// Дескриптор отчётов — тот самый документ, которым устройство себя описывает.
/// Запрашивается отдельно и **у интерфейса**, а не у устройства.
pub const DESC_REPORT: u8 = 0x22;

/// Класс интерфейса HID.
pub const CLASS_HID: u8 = 3;
/// Подкласс «boot interface»: интерфейс обязан понимать boot protocol.
pub const SUBCLASS_BOOT: u8 = 1;
/// Протокол «клавиатура».
pub const PROTOCOL_KEYBOARD: u8 = 1;
/// Протокол «мышь».
pub const PROTOCOL_MOUSE: u8 = 2;

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

/// Найденный интерфейс HID и его конечная точка прерываний.
#[derive(Clone, Copy, Debug)]
pub struct HidInterface {
    /// [`PROTOCOL_KEYBOARD`], [`PROTOCOL_MOUSE`] или ноль — «устройство не
    /// сказало, что оно такое».
    ///
    /// Ноль законен и встречается чаще, чем кажется: его объявляет всякий
    /// интерфейс без boot-подкласса, в том числе планшет. Что это за устройство,
    /// в таком случае говорит только дескриптор отчётов.
    pub protocol: u8,
    /// Объявлен ли boot-подкласс. Означает ровно одно: у устройства есть
    /// запасной формат отчёта на случай, если дескриптор разобрать не удалось.
    pub boot: bool,
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
    /// Длина дескриптора отчётов из дескриптора HID; ноль — устройство его не
    /// объявило, и читать нечего.
    pub report_len: u16,
    /// Сколько всего интерфейсов класса HID у этой конфигурации.
    ///
    /// Больше одного означает составное устройство — «мышь», внутри которой
    /// живёт ещё и клавиатура. Драйвер поднимает только первый, и это число
    /// существует затем, чтобы такое устройство было **видно**: иначе половина
    /// его молчит, а понять, что она вообще есть, нечем.
    pub interfaces: u8,
}

/// Найти в дескрипторе конфигурации интерфейс HID.
///
/// Берётся первый интерфейс класса HID, у которого есть конечная точка
/// прерываний. Подкласс и протокол при этом не требуются: интерфейс без
/// boot-подкласса — это не «чужое устройство», а устройство, о котором придётся
/// прочитать его дескриптор отчётов. Именно такие интерфейсы объявляют планшеты.
///
/// Составные устройства (одна «мышь» с клавиатурным интерфейсом внутри)
/// обслуживаются только первым из интерфейсов: второй потребовал бы второго
/// кольца на том же слоте, а проверить это на QEMU нечем — там клавиатура и мышь
/// всегда отдельные устройства. Ограничение названо вслух, а не обойдено молча.
///
/// `bytes` — конфигурация целиком, вместе с вложенными дескрипторами
/// интерфейсов и конечных точек: устройство отдаёт их одним куском, и разбирать
/// их надо тоже одним проходом.
#[must_use]
pub fn find_hid(bytes: &[u8]) -> Option<HidInterface> {
    if bytes.len() < CONFIG_DESC_LEN || bytes[1] != DESC_CONFIGURATION {
        return None;
    }
    let configuration = bytes[5];

    // Сколько всего интерфейсов HID — считается отдельным проходом, до поиска:
    // ответ нужен уже в той записи, которую вернёт первый же найденный.
    let interfaces = hid_interfaces(bytes);

    let mut offset = usize::from(bytes[0]);
    let mut current: Option<HidInterface> = None;

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
                current = if class == CLASS_HID {
                    Some(HidInterface {
                        protocol,
                        boot: subclass == SUBCLASS_BOOT,
                        configuration,
                        interface: bytes[offset + 2],
                        endpoint: 0,
                        max_packet_size: 0,
                        interval: 0,
                        report_len: 0,
                        interfaces,
                    })
                } else {
                    // Не наш интерфейс: его конечные точки нас не касаются, и
                    // забыть про него надо здесь, а не проверять потом.
                    None
                };
            }
            // Дескриптор HID идёт после своего интерфейса и до его конечных
            // точек. Длина дескриптора отчётов лежит именно здесь, и другого
            // способа её узнать нет: запросить отчётов «сколько есть» нельзя,
            // wLength в запросе обязателен.
            DESC_HID if length >= 9 => {
                if let Some(found) = current.as_mut() {
                    found.report_len = report_descriptor_len(&bytes[offset..offset + length]);
                }
            }
            DESC_ENDPOINT if length >= 7 => {
                if let Some(mut found) = current {
                    let address = bytes[offset + 2];
                    let attributes = bytes[offset + 3];
                    // Нужна точка типа Interrupt (биты 1:0 = 11) и направления
                    // IN (бит 7 адреса). Устройство может объявлять и OUT-точку
                    // — у клавиатуры это светодиоды, — и перепутать их значит
                    // ждать отчёты оттуда, откуда они не приходят.
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

/// Сколько интерфейсов класса HID объявляет конфигурация.
///
/// Считаются именно интерфейсы, а не альтернативные их настройки: у HID
/// альтернативных настроек не бывает, поэтому различать их незачем.
#[must_use]
pub fn hid_interfaces(bytes: &[u8]) -> u8 {
    let mut found = 0u8;
    let mut offset = usize::from(bytes[0]);
    while offset + 2 <= bytes.len() {
        let length = usize::from(bytes[offset]);
        if length < 2 || offset + length > bytes.len() {
            break;
        }
        if bytes[offset + 1] == DESC_INTERFACE && length >= 9 && bytes[offset + 5] == CLASS_HID {
            found = found.saturating_add(1);
        }
        offset += length;
    }
    found
}

/// Длина дескриптора отчётов из дескриптора HID.
///
/// Устройство перечисляет свои подчинённые дескрипторы парами «тип, длина»,
/// начиная со смещения 6, и число пар лежит в байте 5. Нужен тип
/// [`DESC_REPORT`]: рядом с ним встречается физический дескриптор
/// (расположение кнопок на руке), который ядру не нужен и который нельзя
/// перепутать с отчётами — иначе будет прочитано не то и не той длины.
fn report_descriptor_len(hid: &[u8]) -> u16 {
    let count = usize::from(hid[5]);
    for index in 0..count {
        let at = 6 + index * 3;
        if at + 3 > hid.len() {
            break;
        }
        if hid[at] == DESC_REPORT {
            return u16::from_le_bytes([hid[at + 1], hid[at + 2]]);
        }
    }
    0
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
