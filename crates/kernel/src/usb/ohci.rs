//! Драйвер контроллера OHCI: клавиатура и мышь там, где xHCI нет вовсе.
//!
//! # Зачем он появился
//!
//! Не «для полноты». Релиз `v0.1.57`, поставленный в VirtualBox, не слушался ни
//! клавиатуры, ни мыши, и перепись контроллеров ([`super::survey`]) назвала
//! причину с настоящей машины, а не по догадке:
//!
//! ```text
//!   usb : 0000:00:06.0 ohci (prog-if 0x10) vendor 0x106b device 0x003f -- no driver here
//!   usb : 1 controller(s), none of them driven by this kernel
//! ```
//!
//! У VirtualBox по умолчанию включён **один** контроллер — OHCI (эмулируется
//! Apple KeyLargo), и к нему же приезжают клавиатура и планшет. В режиме
//! «USB 2.0» рядом появляется EHCI, но низко- и полноскоростные устройства
//! остаются на OHCI: EHCI сам по себе с ними не разговаривает, они достаются
//! его спутнику. Отсюда и решение фазы: пишется OHCI, а не EHCI, — на машине
//! читателя ввод висит именно на нём.
//!
//! # Чем OHCI устроен иначе, чем xHCI
//!
//! Разница не в мелочах, а в том, кто кем командует. У xHCI драйвер кладёт
//! команду в кольцо и звонит в дверной звонок; у OHCI **вся работа описана
//! списками в памяти**, которые контроллер обходит сам каждый кадр:
//!
//! * **ED** (endpoint descriptor) — конечная точка: адрес устройства, номер
//!   точки, направление, размер пакета. У ED есть свой список передач.
//! * **TD** (transfer descriptor) — одна передача: откуда брать байты, сколько
//!   их и что с ними делать.
//! * **HCCA** — 256 байт, которые контроллер правит сам: таблица из 32 указателей
//!   на периодические ED (по одному на кадр) и голова очереди законченных
//!   передач.
//!
//! Контроллер идёт по спискам сам, а драйвер узнаёт о завершении тем, что
//! `HeadP` у ED догнал `TailP`. Отсюда приём, без которого этот драйвер был бы
//! гонкой: в конце списка всегда стоит **пустой TD**. Контроллер до него не
//! доходит, а драйвер, добавляя работу, заполняет именно его и переносит
//! `TailP` дальше — то есть никогда не правит поле, которое в этот момент может
//! менять контроллер.
//!
//! # Опрос, а не прерывания
//!
//! Сказано вслух, потому что у xHCI сделано иначе. У OHCI одна линия INTx, а не
//! MSI-X: чтобы узнать, в какой вход контроллера прерываний она приходит, нужно
//! разобрать `_PRT` из ACPI, то есть иметь интерпретатор AML. Его в ядре нет и
//! в этой фазе не будет. Поэтому задача обслуживания просыпается по часам —
//! ровно так же, как задача xHCI на машине без MSI-X. Цена видна в `usb`:
//! счётчик `wakeups` растёт сто раз в секунду. Клавиатуре это незаметно, а
//! честнее так, чем делать вид, что прерывания настроены.

#![allow(clippy::too_many_arguments)]

use alloc::vec::Vec;

use crate::acpi::AcpiError;
use crate::input;
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{MapError, PAGE_SIZE, PhysAddr};
use crate::pci;
use crate::usb::hid::{Reader, choose_reader};
use crate::usb::{self, ATTACHED_MAX, Attached, HidInterface, Stage, Timeout, sleep_ms};

// ---------------------------------------------------------------------------
// Регистры операционного блока (OHCI 1.0a, глава 7)
// ---------------------------------------------------------------------------

const HC_REVISION: usize = 0x00;
const HC_CONTROL: usize = 0x04;
const HC_COMMAND_STATUS: usize = 0x08;
const HC_INTERRUPT_STATUS: usize = 0x0C;
const HC_INTERRUPT_DISABLE: usize = 0x14;
const HC_HCCA: usize = 0x18;
const HC_CONTROL_HEAD_ED: usize = 0x20;
const HC_CONTROL_CURRENT_ED: usize = 0x24;
const HC_BULK_HEAD_ED: usize = 0x28;
const HC_BULK_CURRENT_ED: usize = 0x2C;
const HC_FM_INTERVAL: usize = 0x34;
const HC_PERIODIC_START: usize = 0x40;
const HC_RH_DESCRIPTOR_A: usize = 0x48;
const HC_RH_STATUS: usize = 0x50;
const HC_RH_PORT_STATUS: usize = 0x54;

/// `HcControl`: разрешение периодического списка.
const CONTROL_PLE: u32 = 1 << 2;
/// `HcControl`: разрешение списка управляющих передач.
const CONTROL_CLE: u32 = 1 << 4;
/// `HcControl`: маска поля состояния (`HCFS`).
const CONTROL_HCFS_MASK: u32 = 0b11 << 6;
/// `HcControl`: состояние `UsbOperational`.
const CONTROL_HCFS_OPERATIONAL: u32 = 0b10 << 6;
/// `HcControl`: `InterruptRouting` — прерывания забирает SMM.
const CONTROL_IR: u32 = 1 << 8;

/// `HcCommandStatus`: сброс контроллера.
const STATUS_HCR: u32 = 1 << 0;
/// `HcCommandStatus`: в списке управляющих передач есть работа.
const STATUS_CLF: u32 = 1 << 1;
/// `HcCommandStatus`: просьба к SMM отдать контроллер.
const STATUS_OCR: u32 = 1 << 3;

/// `HcInterruptStatus`: очередь законченных передач записана в HCCA.
const INTR_WDH: u32 = 1 << 1;
/// `HcInterruptStatus`: неисправимая ошибка контроллера.
const INTR_UE: u32 = 1 << 4;
/// Все биты `HcInterruptStatus`, которые бывают взведены.
const INTR_ALL: u32 = 0xC000_007F;

/// `HcRhDescriptorA`: число портов корневого хаба.
const RH_A_NDP_MASK: u32 = 0xFF;
/// `HcRhDescriptorA`: питание портов не выключается вовсе.
const RH_A_NPS: u32 = 1 << 9;
/// `HcRhDescriptorA`: время выхода портов на питание, в единицах по 2 мс.
const RH_A_POTPGT_SHIFT: u32 = 24;

/// `HcRhStatus`: включить питание на всех портах.
const RH_STATUS_LPSC: u32 = 1 << 16;

/// `HcRhPortStatus`: устройство подключено.
const PORT_CCS: u32 = 1 << 0;
/// `HcRhPortStatus`: порт разрешён.
const PORT_PES: u32 = 1 << 1;
/// `HcRhPortStatus`: сброс порта (запись — начать, чтение — идёт).
const PORT_PRS: u32 = 1 << 4;
/// `HcRhPortStatus`: питание порта.
const PORT_PPS: u32 = 1 << 8;
/// `HcRhPortStatus`: подключено низкоскоростное устройство.
const PORT_LSDA: u32 = 1 << 9;
/// `HcRhPortStatus`: изменилось подключение.
const PORT_CSC: u32 = 1 << 16;
/// `HcRhPortStatus`: изменилось разрешение порта.
const PORT_PESC: u32 = 1 << 17;
/// `HcRhPortStatus`: сброс порта закончился.
const PORT_PRSC: u32 = 1 << 20;
/// Все биты «изменилось», которые сбрасываются записью единицы.
const PORT_CHANGE_BITS: u32 = PORT_CSC | PORT_PESC | (1 << 18) | (1 << 19) | PORT_PRSC;

/// Значение `HcFmInterval` по умолчанию: 11999 тактов на кадр, то есть 1 мс.
///
/// Нужно на случай, когда прошивка контроллер не трогала и оставила регистр
/// нулевым: с нулевым интервалом контроллер не выдаёт ни одного кадра, и это
/// выглядит как «устройство молчит».
const DEFAULT_FRAME_INTERVAL: u32 = 11_999;

// ---------------------------------------------------------------------------
// Сроки
// ---------------------------------------------------------------------------

/// Сколько ждать, пока SMM отдаст контроллер.
const OWNERSHIP_TIMEOUT_MS: u64 = 1000;
/// Сколько ждать окончания сброса контроллера.
///
/// Спецификация обещает 10 микросекунд; миллисекунда — это запас на
/// гипервизор, который считает время по-своему.
const RESET_TIMEOUT_MS: u64 = 100;
/// Сколько ждать завершения передачи.
const TRANSFER_TIMEOUT_MS: u64 = 500;
/// Сколько ждать окончания сброса порта.
const PORT_RESET_TIMEOUT_MS: u64 = 500;
/// Пауза после сброса порта: спецификация USB требует дать устройству время на
/// восстановление, прежде чем обращаться к нему.
const PORT_RECOVERY_MS: u64 = 20;
/// Пауза после `SET_ADDRESS`: устройству дано 2 мс на то, чтобы начать отвечать
/// по новому адресу.
const SET_ADDRESS_SETTLE_MS: u64 = 10;

/// Сколько устройств драйвер поднимает.
///
/// Столько же, сколько слотов просит драйвер xHCI, и по той же причине:
/// клавиатура и мышь занимают по одному, остальные — запас, чтобы третье
/// устройство не требовало правки кода.
const DEVICES_MAX: usize = ATTACHED_MAX;

/// Сколько портов драйвер готов перечислить.
///
/// Больше 15 корневых портов у OHCI не бывает (поле `NDP` четырёхбитное по
/// смыслу, хотя занимает байт), а маска подключений — `u32`.
const PORTS_MAX: usize = 15;

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

/// Почему контроллер или устройство не заработали.
#[derive(Clone, Copy, Debug)]
pub enum OhciError {
    /// Таблицы ACPI не разобрались — шину PCI не найти.
    Acpi(AcpiError),
    /// Контроллера OHCI на шине нет.
    Absent,
    /// У контроллера нет BAR памяти: регистры недоступны.
    NoBar,
    /// Не удалось отобразить окно регистров.
    Map(MapError),
    /// Не удалось выделить память под дескрипторы.
    Dma(DmaError),
    /// SMM не отдал контроллер.
    Ownership,
    /// Контроллер не вышел из сброса.
    ResetTimeout,
    /// Порт не закончил сброс.
    PortResetTimeout,
    /// Порт после сброса остался запрещённым — устройства на нём нет или оно
    /// отвалилось между проверкой и сбросом.
    PortNotEnabled,
    /// Передача не завершилась: сколько ждали и кончились ли витки вместо часов.
    TransferTimeout { waited_ms: u64, spun_out: bool },
    /// Передача завершилась с ошибкой; код состояния — из TD.
    Transfer(u8),
    /// Дескриптор пришёл короче, чем в нём же объявлено.
    ShortDescriptor,
    /// У устройства нет интерфейса HID с точкой прерываний.
    NoHid,
    /// Интерфейс HID есть, а понять его формат отчётов нечем.
    UnknownHid,
    /// Устройств больше, чем драйвер поднимает.
    TooMany,
}

impl core::fmt::Display for OhciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Acpi(err) => write!(f, "ACPI: {err}"),
            Self::Absent => f.write_str("no OHCI controller on the PCI bus"),
            Self::NoBar => f.write_str("the controller has no memory BAR"),
            Self::Map(err) => write!(f, "the register window could not be mapped: {err}"),
            Self::Dma(err) => write!(f, "no memory for the descriptors: {err}"),
            Self::Ownership => f.write_str("the firmware did not hand the controller over"),
            Self::ResetTimeout => f.write_str("the controller never left reset"),
            Self::PortResetTimeout => f.write_str("the port reset never finished"),
            Self::PortNotEnabled => f.write_str("the port stayed disabled after the reset"),
            Self::TransferTimeout { waited_ms, spun_out } => {
                if *spun_out {
                    write!(f, "a transfer never completed ({waited_ms} ms; the spin limit ran out first, so the clock is suspect)")
                } else {
                    write!(f, "a transfer never completed ({waited_ms} ms)")
                }
            }
            Self::Transfer(code) => write!(f, "transfer failed: {}", condition_name(*code)),
            Self::ShortDescriptor => f.write_str("the descriptor came back shorter than it claims"),
            Self::NoHid => f.write_str("no HID interface with an interrupt endpoint"),
            Self::UnknownHid => f.write_str("the HID interface speaks neither boot protocol nor a descriptor we understand"),
            Self::TooMany => f.write_str("more devices than the driver brings up"),
        }
    }
}

impl From<DmaError> for OhciError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

/// Имя кода завершения из TD (OHCI 1.0a, 4.3.3).
///
/// Имена, а не числа: «код 4» и «устройство ответило STALL» — это одно и то же
/// для машины и совсем разное для человека, которому придётся чинить.
const fn condition_name(code: u8) -> &'static str {
    match code {
        0 => "no error",
        1 => "CRC error",
        2 => "bit stuffing",
        3 => "data toggle mismatch",
        4 => "STALL from the device",
        5 => "device not responding",
        6 => "PID check failure",
        7 => "unexpected PID",
        8 => "data overrun",
        9 => "data underrun",
        12 => "buffer overrun",
        13 => "buffer underrun",
        _ => "not accessed by the controller",
    }
}

// ---------------------------------------------------------------------------
// Регистры и дескрипторы
// ---------------------------------------------------------------------------

/// Окно регистров контроллера.
struct Regs {
    base: usize,
}

impl Regs {
    /// Прочитать регистр.
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: окно отображено при создании структуры, смещение — константа
        // из спецификации, лежащая внутри первой страницы. `volatile` потому,
        // что регистр меняет контроллер: обычное чтение компилятор вправе
        // вынести из цикла ожидания и превратить его в вечный.
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }

    /// Записать регистр.
    fn write(&self, offset: usize, value: u32) {
        // SAFETY: см. [`Regs::read`].
        unsafe { ((self.base + offset) as *mut u32).write_volatile(value) }
    }

    /// Регистр состояния порта. Порты нумеруются с нуля.
    fn port(&self, index: usize) -> u32 {
        self.read(HC_RH_PORT_STATUS + index * 4)
    }

    /// Записать регистр состояния порта.
    ///
    /// Единица в поле «изменилось» **сбрасывает** его, а не взводит: писать сюда
    /// прочитанное значение целиком означало бы стереть все отметки об
    /// изменениях разом.
    fn set_port(&self, index: usize, value: u32) {
        self.write(HC_RH_PORT_STATUS + index * 4, value);
    }
}

/// Страница под дескрипторы: ED-ы в начале, TD-ы следом.
///
/// Раскладка задана здесь, а не рассчитывается: и ED, и TD обязаны быть
/// выровнены на 16 байт, а страница от [`dma::alloc`] выровнена на 4096 — то
/// есть достаточно не ошибиться в смещениях.
struct Descriptors {
    buffer: DmaBuffer,
}

/// Смещение первого TD внутри страницы дескрипторов.
const TD_AREA: usize = 0x40;
/// Размер дескриптора — и ED, и TD занимают по 16 байт.
const DESC_LEN: usize = 16;

impl Descriptors {
    fn new() -> Result<Self, OhciError> {
        let buffer = dma::alloc(PAGE_SIZE)?;
        buffer.zero();
        Ok(Self { buffer })
    }

    /// Физический адрес ED с номером `index`.
    fn ed_phys(&self, index: usize) -> u32 {
        (self.buffer.phys().as_u64() + (index * DESC_LEN) as u64) as u32
    }

    /// Физический адрес TD с номером `index`.
    fn td_phys(&self, index: usize) -> u32 {
        (self.buffer.phys().as_u64() + (TD_AREA + index * DESC_LEN) as u64) as u32
    }

    /// Прочитать слово ED.
    fn ed_read(&self, index: usize, word: usize) -> u32 {
        // SAFETY: смещение внутри выделенной страницы; `volatile` — поля правит
        // контроллер.
        unsafe { self.word_ptr(index * DESC_LEN + word * 4).read_volatile() }
    }

    /// Записать слово ED.
    fn ed_write(&self, index: usize, word: usize, value: u32) {
        // SAFETY: см. [`Descriptors::ed_read`].
        unsafe { self.word_ptr(index * DESC_LEN + word * 4).write_volatile(value) }
    }

    /// Прочитать слово TD.
    fn td_read(&self, index: usize, word: usize) -> u32 {
        // SAFETY: см. [`Descriptors::ed_read`].
        unsafe { self.word_ptr(TD_AREA + index * DESC_LEN + word * 4).read_volatile() }
    }

    /// Записать слово TD.
    fn td_write(&self, index: usize, word: usize, value: u32) {
        // SAFETY: см. [`Descriptors::ed_read`].
        unsafe { self.word_ptr(TD_AREA + index * DESC_LEN + word * 4).write_volatile(value) }
    }

    /// Заполнить TD целиком.
    fn td_fill(&self, index: usize, control: u32, buffer: u32, next: u32, end: u32) {
        self.td_write(index, 0, control);
        self.td_write(index, 1, buffer);
        self.td_write(index, 2, next);
        self.td_write(index, 3, end);
    }

    /// Указатель на слово внутри страницы.
    ///
    /// # Safety
    ///
    /// `offset` обязан лежать внутри страницы и быть выровнен на четыре байта.
    unsafe fn word_ptr(&self, offset: usize) -> *mut u32 {
        // SAFETY: контракт функции.
        unsafe { self.buffer.as_ptr::<u8>().add(offset).cast::<u32>() }
    }
}

/// Слова ED: `[0]` — параметры точки, `[1]` — `TailP`, `[2]` — `HeadP`,
/// `[3]` — следующий ED.
const ED_CONTROL: usize = 0;
const ED_TAIL: usize = 1;
const ED_HEAD: usize = 2;
const ED_NEXT: usize = 3;

/// `HeadP`: контроллер остановил точку из-за ошибки.
const ED_HEAD_HALTED: u32 = 1 << 0;
/// Маска адреса в указателях: младшие четыре бита заняты флагами.
const POINTER_MASK: u32 = !0xF;

/// ED: направление задаётся в TD (так делаются управляющие передачи).
const ED_DIR_FROM_TD: u32 = 0b00 << 11;
/// ED: направление IN.
const ED_DIR_IN: u32 = 0b10 << 11;
/// ED: устройство низкоскоростное.
const ED_SPEED_LOW: u32 = 1 << 13;

/// TD: короткий пакет — не ошибка.
///
/// Взводится у всех чтений. Без него устройство, ответившее меньше, чем у него
/// просили (а так отвечает любой дескриптор, длину которого мы ещё не знаем),
/// останавливает точку кодом `data underrun`.
const TD_ROUNDING: u32 = 1 << 18;
/// TD: пакет SETUP.
const TD_DP_SETUP: u32 = 0b00 << 19;
/// TD: передача наружу.
const TD_DP_OUT: u32 = 0b01 << 19;
/// TD: передача внутрь.
const TD_DP_IN: u32 = 0b10 << 19;
/// TD: не просить прерывания по завершении (у нас опрос).
const TD_NO_INTERRUPT: u32 = 0b111 << 21;
/// TD: начать с `DATA0`, не считаясь с переносом у ED.
const TD_TOGGLE_DATA0: u32 = 0b10 << 24;
/// TD: начать с `DATA1`.
const TD_TOGGLE_DATA1: u32 = 0b11 << 24;
/// TD: код завершения «контроллер сюда ещё не дошёл».
const TD_CC_NOT_ACCESSED: u32 = 0xE << 28;
/// Сдвиг поля кода завершения.
const TD_CC_SHIFT: u32 = 28;

// ---------------------------------------------------------------------------
// Устройство
// ---------------------------------------------------------------------------

/// Подключённое устройство.
struct Device {
    /// Корневой порт, считая с единицы, — так же, как их видит человек.
    port: u8,
    /// Адрес на шине.
    address: u8,
    /// Низкоскоростное ли оно.
    low_speed: bool,
    /// Страница с ED точки прерываний и её TD.
    ring: Descriptors,
    /// Буфер, в который контроллер складывает отчёт.
    report: DmaBuffer,
    /// Сколько байт запрашивается у точки прерываний.
    report_len: u16,
    /// Какой из двух TD сейчас пустой — тот, который драйвер заполнит следующим.
    ///
    /// Пустой TD в конце списка — не запас, а способ не гоняться с контроллером
    /// за одно и то же поле: драйвер правит только тот дескриптор, до которого
    /// контроллер по определению ещё не дошёл.
    tail_td: usize,
    /// Разбор отчётов в события.
    reader: Option<Reader>,
    /// Кто изготовитель и что за модель — из дескриптора устройства.
    identity: (u16, u16),
    /// Длина дескриптора отчётов, по которому устройство разобрано; ноль —
    /// разбирали по boot-протоколу.
    described_by: u16,
    /// Номер поднятого интерфейса и сколько их всего у устройства.
    interface: (u8, u8),
}

impl Device {
    /// Поставить в очередь запрос отчёта, заполнив пустой TD.
    ///
    /// Заполняется именно тот дескриптор, на который смотрит `TailP`: до него
    /// контроллер не доходит по определению, поэтому гонки за поля нет. Новым
    /// пустым становится соседний, и `TailP` переносится на него — одной
    /// записью, последней.
    fn queue_report(&self, tail: usize) {
        let next = 1 - tail;
        let buffer = self.report.phys().as_u64() as u32;
        self.ring.td_fill(
            tail,
            TD_CC_NOT_ACCESSED | TD_DP_IN | TD_ROUNDING | TD_NO_INTERRUPT,
            buffer,
            self.ring.td_phys(next),
            buffer + u32::from(self.report_len) - 1,
        );
        self.ring.td_fill(next, 0, 0, 0, 0);
        self.ring.ed_write(0, ED_TAIL, self.ring.td_phys(next));
    }
}

// ---------------------------------------------------------------------------
// Контроллер
// ---------------------------------------------------------------------------

/// Контроллер и подключённые к нему устройства.
pub struct Controller {
    regs: Regs,
    /// Область, которую правит сам контроллер: таблица периодических точек и
    /// голова очереди законченных передач.
    hcca: DmaBuffer,
    /// Страница управляющей точки: один ED на весь контроллер и четыре TD.
    ///
    /// Один — потому что управляющие передачи драйвер делает по очереди и
    /// только при перечислении. Второй ED здесь не ускорил бы ничего, а
    /// перепутать их местами было бы легко.
    control: Descriptors,
    /// Буфер под данные управляющих передач: пакет SETUP и то, что придёт в
    /// ответ.
    transfer: DmaBuffer,
    /// Устройства в порядке подключения.
    devices: Vec<Device>,
    /// Сколько портов у корневого хаба.
    ports: usize,
    /// Сколько портов заняты. Больше, чем устройств, — значит устройство есть, а
    /// поднять его не удалось.
    occupied: usize,
    /// Маска занятых портов на момент последней сверки.
    connected: u32,
    /// Сколько передач завершилось ошибкой.
    errors: u64,
    /// Сколько раз задача просыпалась обслуживать контроллер.
    services: u64,
    /// Чем закончилась последняя неудачная попытка поднять устройство.
    last_error: Option<(u8, Stage, OhciError)>,
    /// Взводил ли контроллер бит неисправимой ошибки.
    ///
    /// Отдельно от счётчика ошибок передач: `UE` означает, что контроллер
    /// остановился сам, и это совсем другая неисправность, чем «устройство
    /// ответило STALL».
    unrecoverable: bool,
}

impl Controller {
    /// Найти контроллер, поднять его и перечислить порты.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, прерывания —
    /// быть разрешены (ожидания опираются на таймер), а память с таблицами ACPI
    /// — оставаться нетронутой. Вызывать не удерживая ни одного
    /// [`crate::sync::SpinLock`].
    pub unsafe fn init(rsdp: u64) -> Result<Self, OhciError> {
        // SAFETY: контракт функции.
        let root = unsafe { pci::Root::discover(rsdp) }.map_err(OhciError::Acpi)?;
        // SAFETY: контракт функции.
        let device = unsafe {
            pci::find_by_class(&root, pci::CLASS_SERIAL_BUS, pci::SUBCLASS_USB, PROG_IF_OHCI)
        }
        .ok_or(OhciError::Absent)?;

        let bar = device.memory_bar(0).ok_or(OhciError::NoBar)?;
        // SAFETY: контракт функции.
        let base = unsafe { map_bar(bar) }.map_err(OhciError::Map)?;
        let regs = Regs { base };

        // Bus master — до того, как контроллеру сообщён хоть один адрес: без
        // него он не сможет прочитать ни HCCA, ни дескрипторы, и это выглядит
        // как «контроллер работает, а передачи не идут». Заодно включается
        // Memory Space: прошивка вправе оставить его выключенным, и тогда все
        // регистры читаются как 0xFFFFFFFF.
        //
        // SAFETY: списки ещё не построены, но и работу контроллер пока не
        // ведёт — он остановлен и будет сброшен ниже.
        unsafe { device.enable_bus_master() };

        let revision = regs.read(HC_REVISION) & 0xFF;
        kprintln!(
            "  ohci        : {} at {:#x}, revision {:x}.{:x}",
            device.address,
            bar.as_u64(),
            revision >> 4,
            revision & 0xF
        );

        let hcca = dma::alloc(PAGE_SIZE)?;
        hcca.zero();
        let control = Descriptors::new()?;
        let transfer = dma::alloc(PAGE_SIZE)?;
        transfer.zero();

        let mut controller = Self {
            regs,
            hcca,
            control,
            transfer,
            devices: Vec::new(),
            ports: 0,
            occupied: 0,
            connected: 0,
            errors: 0,
            services: 0,
            last_error: None,
            unrecoverable: false,
        };

        // SAFETY: окно регистров отображено, буферы выделены и обнулены.
        unsafe { controller.take_over()? };
        // SAFETY: контроллер сброшен и переведён в рабочее состояние.
        unsafe { controller.start_root_hub() };
        Ok(controller)
    }

    /// Забрать контроллер у прошивки и перевести его в рабочее состояние.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено, а HCCA — выделена и обнулена.
    unsafe fn take_over(&mut self) -> Result<(), OhciError> {
        // Шаг первый: спросить, не занят ли контроллер системным управлением.
        // `IR` означает, что прерывания контроллера забирает SMM — то есть у
        // машины есть код, который прямо сейчас обслуживает эту клавиатуру,
        // изображая PS/2 для BIOS. Отобрать контроллер силой значит уронить
        // тот код вместе с машиной; спецификация описывает вежливый способ, и
        // он один: попросить и подождать.
        let control = self.regs.read(HC_CONTROL);
        if control & CONTROL_IR != 0 {
            self.regs.write(HC_COMMAND_STATUS, STATUS_OCR);
            let mut timeout = Timeout::new(OWNERSHIP_TIMEOUT_MS);
            while self.regs.read(HC_CONTROL) & CONTROL_IR != 0 {
                if timeout.expired() {
                    return Err(OhciError::Ownership);
                }
            }
            kprintln!("  ohci        : taken over from system management mode");
        }

        // Значение `HcFmInterval` настраивает прошивка под свой кварц, и после
        // сброса оно теряется. Сохранить и вернуть — дешевле, чем считать
        // самому; ноль означает, что прошивка регистр не трогала.
        let saved_interval = self.regs.read(HC_FM_INTERVAL);

        self.regs.write(HC_COMMAND_STATUS, STATUS_HCR);
        let mut timeout = Timeout::new(RESET_TIMEOUT_MS);
        while self.regs.read(HC_COMMAND_STATUS) & STATUS_HCR != 0 {
            if timeout.expired() {
                return Err(OhciError::ResetTimeout);
            }
        }

        // С этого места и до записи `UsbOperational` у драйвера **2 мс**: после
        // сброса контроллер стоит в `UsbSuspend`, и если его не запустить, он
        // уходит в `UsbResume` сам. Поэтому ниже нет ни одной печати и ни
        // одного ожидания — только записи регистров.
        let interval = if saved_interval & 0x3FFF == 0 {
            DEFAULT_FRAME_INTERVAL
        } else {
            saved_interval & 0x3FFF
        };
        // `FSMPS` — сколько байт контроллер разрешает себе начать передавать под
        // конец кадра. Формула из спецификации (7.3.1); без неё контроллер
        // берётся за пакет, который в кадр уже не влезает, и рвёт его.
        let fsmps = ((interval - 210) * 6 / 7) << 16;
        self.regs.write(HC_FM_INTERVAL, fsmps | interval);
        // Периодический список начинают обслуживать, когда до конца кадра
        // осталось 10 % времени.
        self.regs.write(HC_PERIODIC_START, interval * 9 / 10);

        self.regs.write(HC_HCCA, self.hcca.phys().as_u64() as u32);
        self.regs.write(HC_CONTROL_HEAD_ED, 0);
        self.regs.write(HC_CONTROL_CURRENT_ED, 0);
        self.regs.write(HC_BULK_HEAD_ED, 0);
        self.regs.write(HC_BULK_CURRENT_ED, 0);

        // Прерывания запрещены все до одного: драйвер работает опросом, а линия
        // INTx этого контроллера ведёт туда, где у ядра нет обработчика.
        // Разрешённое, но не обслуживаемое прерывание уровня повесило бы машину
        // намертво — оно взводится и не снимается никем.
        self.regs.write(HC_INTERRUPT_DISABLE, INTR_ALL);
        self.regs.write(HC_INTERRUPT_STATUS, INTR_ALL);

        let control = self.regs.read(HC_CONTROL) & !CONTROL_HCFS_MASK;
        self.regs
            .write(HC_CONTROL, control | CONTROL_HCFS_OPERATIONAL | CONTROL_PLE | CONTROL_CLE);
        Ok(())
    }

    /// Включить питание портов и запомнить, сколько их.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии.
    unsafe fn start_root_hub(&mut self) {
        let descriptor = self.regs.read(HC_RH_DESCRIPTOR_A);
        self.ports = ((descriptor & RH_A_NDP_MASK) as usize).min(PORTS_MAX);

        // Питание включается всегда, даже когда контроллер объявил, что
        // выключать его не умеет (`NPS`): лишняя запись стоит ничего, а
        // пропущенная означает порт, на котором устройство не появится никогда.
        self.regs.write(HC_RH_STATUS, RH_STATUS_LPSC);
        for index in 0..self.ports {
            self.regs.set_port(index, PORT_PPS);
        }

        // `POTPGT` — сколько ждать выхода портов на питание, в единицах по 2 мс.
        // Ноль означает «прошивка не сказала»; берём 20 мс — столько же требует
        // спецификация USB от любого хаба.
        let potpgt = (descriptor >> RH_A_POTPGT_SHIFT) & 0xFF;
        let settle = if potpgt == 0 { 20 } else { u64::from(potpgt) * 2 };
        if descriptor & RH_A_NPS == 0 {
            sleep_ms(settle);
        }

        kprintln!(
            "  ohci        : root hub with {} port(s), power settles in {} ms",
            self.ports,
            settle
        );
    }

    /// Перечислить порты и поднять всё, что на них нашлось.
    ///
    /// Возвращает «нашлась ли клавиатура» и «нашёлся ли указатель».
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии, вызов — идти не из
    /// обработчика прерывания.
    pub unsafe fn attach_devices(&mut self) -> (bool, bool) {
        self.occupied = 0;
        for index in 0..self.ports {
            let status = self.regs.port(index);
            if status & PORT_CCS == 0 {
                continue;
            }
            self.occupied += 1;
            let port = (index + 1) as u8;
            // SAFETY: контракт функции.
            match unsafe { self.attach_port(index) } {
                Ok(()) => {}
                Err((stage, err)) => {
                    kprintln!("  ohci        : port {port} stopped while {stage}: {err}");
                    self.last_error = Some((port, stage, err));
                }
            }
        }
        // SAFETY: контракт функции.
        self.connected = unsafe { self.connected_mask() };

        let mut keyboard = false;
        let mut mouse = false;
        for device in &self.devices {
            match device.reader {
                Some(Reader::Keyboard(_)) => keyboard = true,
                Some(Reader::Mouse(_)) => mouse = true,
                None => {}
            }
        }
        (keyboard, mouse)
    }

    /// Поднять устройство на одном порту.
    ///
    /// # Safety
    ///
    /// См. [`Controller::attach_devices`].
    unsafe fn attach_port(&mut self, index: usize) -> Result<(), (Stage, OhciError)> {
        let port = (index + 1) as u8;
        if self.devices.len() >= DEVICES_MAX {
            return Err((Stage::Reset, OhciError::TooMany));
        }

        // SAFETY: контракт функции.
        let low_speed = unsafe { self.reset_port(index) }.map_err(|err| (Stage::Reset, err))?;

        let address = (self.devices.len() + 1) as u8;
        let ring = Descriptors::new().map_err(|err| (Stage::Address, err))?;
        let report = dma::alloc(PAGE_SIZE).map_err(|err| (Stage::Address, err.into()))?;
        report.zero();

        let mut device = Device {
            port,
            address: 0,
            low_speed,
            ring,
            report,
            report_len: 0,
            tail_td: 1,
            reader: None,
            identity: (0, 0),
            described_by: 0,
            interface: (0, 0),
        };

        // Первые восемь байт дескриптора устройства — единственное, что можно
        // прочитать, не зная размера пакета управляющей точки: он лежит как раз
        // в восьмом байте. Просить больше значит просить контроллер разбить
        // ответ на пакеты неизвестного размера.
        // SAFETY: контракт функции; устройство отвечает по адресу 0.
        let read = unsafe { self.get_descriptor(&device, usb::DESC_DEVICE, 0, 8, 8) }
            .map_err(|err| (Stage::Address, err))?;
        if read < 8 {
            return Err((Stage::Address, OhciError::ShortDescriptor));
        }
        // SAFETY: буфер выделен на страницу, прочитано не больше восьми байт.
        let first = unsafe { core::slice::from_raw_parts(self.transfer_data(), read) };
        let max_packet = usb::DeviceDescriptor::parse(first)
            .map(|desc| u16::from(desc.max_packet_size0))
            .filter(|size| *size >= 8)
            .unwrap_or(8);

        // SET_ADDRESS: до него на шине может быть ровно одно неадресованное
        // устройство, поэтому порты и перечисляются по очереди, а не разом.
        // SAFETY: см. выше.
        unsafe {
            self.control_transfer(
                &device,
                max_packet,
                [0, usb::REQ_SET_ADDRESS, address, 0, 0, 0, 0, 0],
                0,
                false,
            )
        }
        .map_err(|err| (Stage::Address, err))?;
        // Устройству дано время принять новый адрес; обращение раньше срока
        // законно остаётся без ответа.
        sleep_ms(SET_ADDRESS_SETTLE_MS);
        device.address = address;

        // SAFETY: устройство адресовано.
        let found = unsafe { self.describe(&mut device, max_packet) }
            .map_err(|err| (Stage::Describe, err))?;

        // SAFETY: устройство адресовано и описано.
        let reader = unsafe { self.enable_reports(&mut device, max_packet, &found) }
            .map_err(|err| (Stage::Enable, err))?;

        device.report_len = found.max_packet_size.clamp(1, 64);
        device.interface = (found.interface, found.interfaces);
        device.reader = Some(reader);

        // SAFETY: устройство сконфигурировано, буфер отчётов свободен.
        unsafe { self.open_interrupt_endpoint(&device, &found) };

        kprintln!(
            "  ohci        : port {port} {}: {:04x}:{:04x} {}, {} byte reports every {} ms",
            if low_speed { "low speed" } else { "full speed" },
            device.identity.0,
            device.identity.1,
            device.reader.as_ref().map_or("unknown", Reader::name),
            device.report_len,
            found.interval.max(1),
        );

        self.devices.push(device);
        // Периодический список перестраивается целиком: устройств не больше
        // четырёх, а частичная правка цепочки на ходу — это как раз тот случай,
        // где контроллер читает поле, которое драйвер в этот момент меняет.
        // SAFETY: все ED существуют.
        unsafe { self.relink_periodic() };
        Ok(())
    }

    /// Сбросить порт и дождаться, пока он разрешится. Возвращает «низкая ли
    /// скорость у устройства».
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии.
    unsafe fn reset_port(&mut self, index: usize) -> Result<bool, OhciError> {
        // Отметки об изменениях снимаются до сброса: иначе `PRSC` от прошлого
        // сброса (его делала прошивка) выглядел бы как окончание этого.
        self.regs.set_port(index, PORT_CHANGE_BITS);
        self.regs.set_port(index, PORT_PRS);

        let mut timeout = Timeout::new(PORT_RESET_TIMEOUT_MS);
        loop {
            let status = self.regs.port(index);
            if status & PORT_PRSC != 0 {
                break;
            }
            if timeout.expired() {
                return Err(OhciError::PortResetTimeout);
            }
        }
        self.regs.set_port(index, PORT_PRSC);

        // Устройству дано время прийти в себя; спецификация USB требует этого
        // до первого обращения, и пропуск паузы выглядит как «устройство не
        // отвечает» ровно на первом же запросе.
        sleep_ms(PORT_RECOVERY_MS);

        let status = self.regs.port(index);
        if status & PORT_PES == 0 {
            return Err(OhciError::PortNotEnabled);
        }
        Ok(status & PORT_LSDA != 0)
    }

    /// Прочитать дескрипторы устройства и найти в них интерфейс HID.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn describe(
        &mut self,
        device: &mut Device,
        max_packet: u16,
    ) -> Result<HidInterface, OhciError> {
        // Полный дескриптор устройства — ради одних только идентификаторов. На
        // машине без журнала это единственный способ отличить два разных
        // устройства от одного, увиденного дважды. Отказ здесь не смертелен.
        // SAFETY: контракт функции.
        if let Ok(read) = unsafe { self.get_descriptor(device, usb::DESC_DEVICE, 0, 18, max_packet) }
        {
            // SAFETY: буфер выделен на страницу.
            let bytes = unsafe { core::slice::from_raw_parts(self.transfer_data(), read) };
            if let Some(full) = usb::DeviceDescriptor::parse(bytes) {
                device.identity = (full.vendor, full.product);
            }
        }

        // Дескриптор конфигурации: сначала девять байт, чтобы узнать полную
        // длину, потом всё целиком. Читать сразу «побольше» нельзя — устройство
        // вправе ответить ошибкой на запрос длиннее того, что у него есть.
        // SAFETY: контракт функции.
        let read = unsafe {
            self.get_descriptor(device, usb::DESC_CONFIGURATION, 0, 9, max_packet)
        }?;
        if read < 9 {
            return Err(OhciError::ShortDescriptor);
        }
        // SAFETY: см. выше.
        let total = {
            let bytes = unsafe { core::slice::from_raw_parts(self.transfer_data(), read) };
            u16::from_le_bytes([bytes[2], bytes[3]])
        };
        let total = total.min(TRANSFER_DATA_MAX as u16);

        // SAFETY: см. выше.
        let read =
            unsafe { self.get_descriptor(device, usb::DESC_CONFIGURATION, 0, total, max_packet) }?;
        // SAFETY: см. выше.
        let bytes = unsafe { core::slice::from_raw_parts(self.transfer_data(), read) };
        usb::find_hid(bytes).ok_or(OhciError::NoHid)
    }

    /// Выбрать конфигурацию, прочитать дескриптор отчётов и договориться о
    /// протоколе.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn enable_reports(
        &mut self,
        device: &mut Device,
        max_packet: u16,
        found: &HidInterface,
    ) -> Result<Reader, OhciError> {
        // SET_CONFIGURATION: до него устройство не отвечает ни на одном
        // интерфейсе, кроме нулевой точки. Дескриптор отчётов адресован именно
        // интерфейсу, поэтому читать его раньше этого запроса нельзя.
        // SAFETY: контракт функции.
        unsafe {
            self.control_transfer(
                device,
                max_packet,
                [0, usb::REQ_SET_CONFIGURATION, found.configuration, 0, 0, 0, 0, 0],
                0,
                false,
            )
        }?;

        // SAFETY: устройство сконфигурировано.
        let described = unsafe { self.read_report_descriptor(device, max_packet, found) };
        device.described_by = if described.keyboard.is_some() || described.pointer.is_some() {
            found.report_len
        } else {
            0
        };
        let (reader, boot) = choose_reader(found, &described).ok_or(OhciError::UnknownHid)?;

        let request = usb::REQ_TYPE_CLASS | usb::REQ_RECIPIENT_INTERFACE;
        if found.boot {
            let wanted = if boot { usb::HID_PROTOCOL_BOOT } else { usb::HID_PROTOCOL_REPORT };
            // SAFETY: см. выше.
            let protocol = unsafe {
                self.control_transfer(
                    device,
                    max_packet,
                    [request, usb::REQ_HID_SET_PROTOCOL, wanted as u8, 0, found.interface, 0, 0, 0],
                    0,
                    false,
                )
            };
            if protocol.is_err() {
                // Отказ не смертелен: устройство могло не поддерживать запрос,
                // оставаясь при этом в нужном протоколе. Молчать нельзя: если
                // отчёты потом окажутся бессмыслицей, причина будет здесь.
                kprintln!(
                    "  ohci        : SET_PROTOCOL({}) refused; assuming the device is in it anyway",
                    if boot { "boot" } else { "report" }
                );
            }
        }

        // SET_IDLE с нулевой длительностью означает «сообщать только об
        // изменениях». Без него клавиатура повторяет отчёт каждые несколько
        // миллисекунд, и ядро разбирает одно и то же.
        // SAFETY: см. выше.
        let idle = unsafe {
            self.control_transfer(
                device,
                max_packet,
                [request, usb::REQ_HID_SET_IDLE, 0, 0, found.interface, 0, 0, 0],
                0,
                false,
            )
        };
        if idle.is_err() {
            kprintln!("  ohci        : SET_IDLE refused; reports may repeat");
        }
        Ok(reader)
    }

    /// Прочитать и разобрать дескриптор отчётов.
    ///
    /// Неудача здесь отказом не является: у устройства с boot-подклассом
    /// остаётся запасной формат.
    ///
    /// # Safety
    ///
    /// Устройство должно быть сконфигурировано.
    unsafe fn read_report_descriptor(
        &mut self,
        device: &Device,
        max_packet: u16,
        found: &HidInterface,
    ) -> usb_hid::Descriptor {
        if found.report_len == 0 {
            kprintln!("  ohci        : the interface declares no report descriptor");
            return usb_hid::Descriptor::default();
        }
        let length = found.report_len.min(TRANSFER_DATA_MAX as u16);
        // Получатель — **интерфейс**, а не устройство: дескриптор отчётов
        // принадлежит интерфейсу, и запрос к устройству вернёт отказ.
        let setup = [
            usb::REQ_DIR_IN | usb::REQ_RECIPIENT_INTERFACE,
            usb::REQ_GET_DESCRIPTOR,
            0,
            usb::DESC_REPORT,
            found.interface,
            0,
            length as u8,
            (length >> 8) as u8,
        ];
        // SAFETY: контракт функции.
        let read = match unsafe { self.control_transfer(device, max_packet, setup, length, true) } {
            Ok(read) => read,
            Err(err) => {
                kprintln!("  ohci        : the report descriptor could not be read: {err}");
                return usb_hid::Descriptor::default();
            }
        };
        // SAFETY: буфер выделен на страницу, читается ровно столько, сколько
        // сообщил контроллер.
        let bytes = unsafe { core::slice::from_raw_parts(self.transfer_data(), read) };
        let parsed = usb_hid::parse(bytes);

        // Разобранное печатается целиком: дескриптор приходит от чужого
        // устройства, а ошибка разбора выглядит как «курсор ездит наискось», то
        // есть как неисправная мышь.
        match parsed.pointer {
            Some(map) if map.is_absolute() => {
                let (min, max) = map.range();
                kprintln!(
                    "  ohci        : report descriptor {read} bytes: pointer, absolute {min}..{max}, {} buttons{}",
                    map.button_count(),
                    if map.has_wheel() { ", wheel" } else { "" }
                );
            }
            Some(map) => kprintln!(
                "  ohci        : report descriptor {read} bytes: pointer, relative, {} buttons{}",
                map.button_count(),
                if map.has_wheel() { ", wheel" } else { "" }
            ),
            None => {}
        }
        if let Some(map) = parsed.keyboard {
            kprintln!(
                "  ohci        : report descriptor {read} bytes: keyboard, {}, {}-key array",
                if map.has_modifiers() { "modifiers" } else { "no modifiers" },
                map.key_slots()
            );
        }
        if parsed.pointer.is_none() && parsed.keyboard.is_none() {
            kprintln!("  ohci        : report descriptor {read} bytes: nothing the kernel can use");
        }
        parsed
    }

    /// Прочитать дескриптор. Возвращает число полученных байт; данные лежат в
    /// буфере управляющих передач.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано (или отвечать по адресу 0).
    unsafe fn get_descriptor(
        &mut self,
        device: &Device,
        kind: u8,
        index: u8,
        length: u16,
        max_packet: u16,
    ) -> Result<usize, OhciError> {
        let setup = [
            usb::REQ_DIR_IN,
            usb::REQ_GET_DESCRIPTOR,
            index,
            kind,
            0,
            0,
            length as u8,
            (length >> 8) as u8,
        ];
        // SAFETY: контракт функции.
        unsafe { self.control_transfer(device, max_packet, setup, length, true) }
    }

    /// Выполнить передачу по управляющей точке.
    ///
    /// Возвращает число полученных байт. Данные всегда идут через общий буфер
    /// [`Controller::transfer`]: отчёты в это время ещё не запрашиваются, а один
    /// буфер делает невозможной ошибку «прочитали туда, где лежит чужое».
    ///
    /// # Safety
    ///
    /// Устройство должно отвечать по адресу `device.address`.
    unsafe fn control_transfer(
        &mut self,
        device: &Device,
        max_packet: u16,
        setup: [u8; 8],
        length: u16,
        is_in: bool,
    ) -> Result<usize, OhciError> {
        let length = length.min(TRANSFER_DATA_MAX as u16);
        // SAFETY: буфер выделен на страницу, пакет SETUP лежит в её начале.
        unsafe {
            let ptr = self.transfer.as_ptr::<u8>();
            for (offset, byte) in setup.iter().enumerate() {
                ptr.add(offset).write_volatile(*byte);
            }
            if !is_in {
                // Данных наружу у нас не бывает: все передачи с данными — чтения.
                // Обнуление на всякий случай стоит ничего и исключает отправку
                // остатков прошлого ответа.
                for offset in 0..usize::from(length) {
                    ptr.add(TRANSFER_DATA + offset).write_volatile(0);
                }
            }
        }

        let setup_phys = self.transfer.phys().as_u64() as u32;
        let data_phys = setup_phys + TRANSFER_DATA as u32;

        // Три дескриптора: SETUP, данные (если они есть) и состояние. Четвёртый
        // — тот самый пустой TD, до которого контроллер не доходит и по
        // которому драйвер узнаёт, что список кончился.
        const TD_SETUP: usize = 0;
        const TD_DATA: usize = 1;
        const TD_STATUS: usize = 2;
        const TD_TAIL: usize = 3;

        let status_direction = if is_in && length > 0 { TD_DP_OUT } else { TD_DP_IN };
        let after_setup =
            if length > 0 { self.control.td_phys(TD_DATA) } else { self.control.td_phys(TD_STATUS) };

        self.control.td_fill(
            TD_SETUP,
            TD_CC_NOT_ACCESSED | TD_TOGGLE_DATA0 | TD_DP_SETUP | TD_NO_INTERRUPT,
            setup_phys,
            after_setup,
            setup_phys + 7,
        );
        if length > 0 {
            let direction = if is_in { TD_DP_IN } else { TD_DP_OUT };
            self.control.td_fill(
                TD_DATA,
                TD_CC_NOT_ACCESSED | TD_TOGGLE_DATA1 | direction | TD_ROUNDING | TD_NO_INTERRUPT,
                data_phys,
                self.control.td_phys(TD_STATUS),
                data_phys + u32::from(length) - 1,
            );
        }
        // Пакет состояния идёт в обратную сторону и без данных: адреса буфера у
        // него нулевые, и это не забытое поле, а требование спецификации.
        self.control.td_fill(
            TD_STATUS,
            TD_CC_NOT_ACCESSED | TD_TOGGLE_DATA1 | status_direction | TD_ROUNDING | TD_NO_INTERRUPT,
            0,
            self.control.td_phys(TD_TAIL),
            0,
        );
        self.control.td_fill(TD_TAIL, 0, 0, 0, 0);

        let mut ed = u32::from(device.address) | ED_DIR_FROM_TD | (u32::from(max_packet) << 16);
        if device.low_speed {
            ed |= ED_SPEED_LOW;
        }
        self.control.ed_write(0, ED_CONTROL, ed);
        self.control.ed_write(0, ED_TAIL, self.control.td_phys(TD_TAIL));
        self.control.ed_write(0, ED_HEAD, self.control.td_phys(TD_SETUP));
        self.control.ed_write(0, ED_NEXT, 0);

        // Список сообщается контроллеру и тут же объявляется непустым. Порядок
        // важен: `CLF` — это «в списке появилась работа», и взведённый до того,
        // как список сообщён, он относится к прежнему списку.
        self.regs.write(HC_CONTROL_CURRENT_ED, 0);
        self.regs.write(HC_CONTROL_HEAD_ED, self.control.ed_phys(0));
        self.regs.write(HC_COMMAND_STATUS, STATUS_CLF);

        let mut timeout = Timeout::new(TRANSFER_TIMEOUT_MS);
        let result = loop {
            let head = self.control.ed_read(0, ED_HEAD);
            let tail = self.control.ed_read(0, ED_TAIL);
            if head & ED_HEAD_HALTED != 0 {
                // Контроллер остановил точку. Код лежит в том TD, на который
                // указывает `HeadP`, — но искать его по адресу дороже, чем
                // перебрать четыре своих.
                break Err(OhciError::Transfer(self.first_failed_code(TD_TAIL)));
            }
            if head & POINTER_MASK == tail & POINTER_MASK {
                break Ok(());
            }
            if timeout.expired() {
                let (waited_ms, spun_out) = timeout.report();
                break Err(OhciError::TransferTimeout { waited_ms, spun_out });
            }
        };

        // Список отцепляется в любом случае: оставленный ED контроллер будет
        // обходить каждый кадр, а его TD к этому времени уже переиспользованы
        // под следующую передачу.
        self.regs.write(HC_CONTROL_HEAD_ED, 0);
        self.regs.write(HC_CONTROL_CURRENT_ED, 0);
        // Очередь законченных передач нам не нужна — завершение видно по ED, —
        // но признак `WDH` снять надо: пока он взведён, контроллер не пишет в
        // HCCA новую очередь и копит её у себя.
        self.regs.write(HC_INTERRUPT_STATUS, INTR_WDH);

        result?;

        if length == 0 {
            return Ok(0);
        }
        // Сколько байт пришло на самом деле: контроллер двигает `CBP` по мере
        // записи и обнуляет его, когда буфер заполнен целиком. Ноль здесь
        // означает «пришло всё», а не «не пришло ничего», и перепутать это
        // значит потерять самый длинный дескриптор из всех.
        let cbp = self.control.td_read(TD_DATA, 1);
        let received = if cbp == 0 { u32::from(length) } else { cbp.saturating_sub(data_phys) };
        Ok(received as usize)
    }

    /// Код завершения первого TD, который его выставил.
    fn first_failed_code(&self, tds: usize) -> u8 {
        for index in 0..=tds {
            let code = (self.control.td_read(index, 0) >> TD_CC_SHIFT) as u8;
            if code != 0 && code < 0xE {
                return code;
            }
        }
        0xE
    }

    /// Открыть точку прерываний устройства и запросить первый отчёт.
    ///
    /// # Safety
    ///
    /// Устройство должно быть сконфигурировано.
    unsafe fn open_interrupt_endpoint(&self, device: &Device, found: &HidInterface) {
        let mut ed = u32::from(device.address)
            | (u32::from(found.endpoint) << 7)
            | ED_DIR_IN
            | (u32::from(device.report_len) << 16);
        if device.low_speed {
            ed |= ED_SPEED_LOW;
        }
        device.ring.ed_write(0, ED_CONTROL, ed);
        device.ring.ed_write(0, ED_TAIL, device.ring.td_phys(0));
        device.ring.ed_write(0, ED_HEAD, device.ring.td_phys(0));
        device.ring.ed_write(0, ED_NEXT, 0);
        device.queue_report(0);
    }

    /// Пересобрать периодический список: все 32 кадра ведут на цепочку из ED
    /// поднятых устройств.
    ///
    /// Тридцать два одинаковых указателя означают опрос каждый кадр, то есть
    /// раз в миллисекунду. Реже незачем: устройство с `SET_IDLE(0)` отвечает
    /// `NAK`, пока ему нечего сказать, а `NAK` контроллер обрабатывает сам, не
    /// беспокоя ни драйвер, ни процессор.
    ///
    /// # Safety
    ///
    /// Все ED устройств должны существовать.
    unsafe fn relink_periodic(&self) {
        for index in 0..self.devices.len() {
            let next = self
                .devices
                .get(index + 1)
                .map_or(0, |device| device.ring.ed_phys(0));
            self.devices[index].ring.ed_write(0, ED_NEXT, next);
        }
        let head = self.devices.first().map_or(0, |device| device.ring.ed_phys(0));
        // SAFETY: HCCA выделена на страницу; таблица занимает её первые 128 байт.
        unsafe {
            let table = self.hcca.as_ptr::<u32>();
            for frame in 0..32 {
                table.add(frame).write_volatile(head);
            }
        }
    }

    /// Забрать пришедшие отчёты.
    fn service(&mut self) {
        self.services += 1;

        // Неисправимая ошибка контроллера — единственное, ради чего здесь
        // читается регистр состояния: она означает, что контроллер остановился
        // сам, и все дальнейшие «устройство молчит» будут её следствием.
        let status = self.regs.read(HC_INTERRUPT_STATUS);
        if status & INTR_UE != 0 {
            if !self.unrecoverable {
                kprintln!("  ohci        : the controller reported an unrecoverable error");
            }
            self.unrecoverable = true;
        }
        if status & INTR_WDH != 0 {
            self.regs.write(HC_INTERRUPT_STATUS, INTR_WDH);
        }

        for device in &mut self.devices {
            let head = device.ring.ed_read(0, ED_HEAD);
            let tail = device.ring.ed_read(0, ED_TAIL);

            if head & ED_HEAD_HALTED != 0 {
                // Точка остановлена ошибкой. Лечится тем же, чем лечит её любая
                // операционная система: снять признак, сбросить переключение
                // и запросить отчёт заново. Молча терять клавиатуру из-за одного
                // испорченного пакета — хуже.
                self.errors += 1;
                let tail_phys = device.ring.ed_read(0, ED_TAIL);
                device.ring.ed_write(0, ED_HEAD, tail_phys & POINTER_MASK);
                device.tail_td = 1 - device.tail_td;
                continue;
            }
            if head & POINTER_MASK != tail & POINTER_MASK {
                // Контроллер до дескриптора ещё не дошёл: устройству нечего
                // сказать, и это самое частое состояние.
                continue;
            }

            // Заполненный дескриптор — тот, который был пустым в прошлый раз.
            let done = device.tail_td;
            let code = (device.ring.td_read(done, 0) >> TD_CC_SHIFT) as u8;
            let cbp = device.ring.td_read(done, 1);
            let buffer = device.report.phys().as_u64() as u32;
            let length = if cbp == 0 {
                u32::from(device.report_len)
            } else {
                cbp.saturating_sub(buffer)
            };

            if code == 0 && length > 0 {
                // SAFETY: буфер выделен на страницу, длина — не больше
                // запрошенной.
                let report = unsafe {
                    core::slice::from_raw_parts(device.report.as_ptr::<u8>(), length as usize)
                };
                if let Some(reader) = device.reader.as_mut() {
                    reader.handle_report(report);
                }
            } else if code != 0 {
                self.errors += 1;
            }

            device.tail_td = 1 - done;
            device.queue_report(done);
        }
    }

    /// Маска занятых портов — по биту на порт.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии.
    unsafe fn connected_mask(&self) -> u32 {
        let mut mask = 0;
        for index in 0..self.ports {
            if self.regs.port(index) & PORT_CCS != 0 {
                mask |= 1 << index;
            }
        }
        mask
    }

    /// Изменился ли состав портов.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии.
    unsafe fn ports_differ(&self) -> bool {
        // SAFETY: контракт функции.
        self.connected != unsafe { self.connected_mask() }
    }

    /// Перечислить порты заново — то, что делает «воткнули на ходу» работающим.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть в рабочем состоянии, вызов — идти из задачи.
    unsafe fn rescan(&mut self) -> bool {
        // SAFETY: контракт функции.
        let mask = unsafe { self.connected_mask() };
        if mask == self.connected {
            return false;
        }

        // Устройства, чьи порты опустели, забываются: их ED уходит из
        // периодического списка, память возвращается окну DMA. Оставить их
        // значило бы опрашивать порт, на котором никого нет.
        //
        // Порядок здесь — не стилистика. Страницы освобождаются **после** того,
        // как список пересобран без них: пока ED остаётся в цепочке, контроллер
        // читает его каждый кадр, и освобождённая страница, отданная под чужие
        // данные, превратилась бы для него в дескриптор с произвольными
        // адресами. Такую неисправность не воспроизвести и не понять.
        let mut dropped = Vec::new();
        let mut index = 0;
        while index < self.devices.len() {
            let port = usize::from(self.devices[index].port) - 1;
            if mask & (1 << port) == 0 {
                let device = self.devices.remove(index);
                kprintln!("  ohci        : port {} is empty now", device.port);
                dropped.push(device);
            } else {
                index += 1;
            }
        }
        if !dropped.is_empty() {
            // SAFETY: оставшиеся ED существуют.
            unsafe { self.relink_periodic() };
            for device in &dropped {
                // SAFETY: ED этого устройства больше не в списке, по которому
                // ходит контроллер, — цепочка пересобрана строкой выше.
                unsafe {
                    dma::free(&device.ring.buffer);
                    dma::free(&device.report);
                }
            }
        }

        // Новые порты поднимаются тем же путём, что при загрузке.
        for port_index in 0..self.ports {
            if mask & (1 << port_index) == 0 {
                continue;
            }
            let port = (port_index + 1) as u8;
            if self.devices.iter().any(|device| device.port == port) {
                continue;
            }
            // SAFETY: контракт функции.
            match unsafe { self.attach_port(port_index) } {
                Ok(()) => {}
                Err((stage, err)) => {
                    kprintln!("  ohci        : port {port} stopped while {stage}: {err}");
                    self.last_error = Some((port, stage, err));
                }
            }
        }

        self.connected = mask;
        self.occupied = mask.count_ones() as usize;

        let sources = input::sources();
        input::set_sources(input::Sources {
            keyboard: sources.keyboard || self.has(usb::PROTOCOL_KEYBOARD),
            mouse: sources.mouse || self.has(usb::PROTOCOL_MOUSE),
            ..sources
        });
        true
    }

    /// Есть ли устройство, которое разбирается как указанный протокол.
    fn has(&self, protocol: u8) -> bool {
        self.devices
            .iter()
            .any(|device| device.reader.as_ref().is_some_and(|r| r.protocol() == protocol))
    }

    /// Адрес данных в буфере управляющих передач.
    fn transfer_data(&self) -> *const u8 {
        // SAFETY: смещение внутри страницы.
        unsafe { self.transfer.as_ptr::<u8>().add(TRANSFER_DATA) }
    }

    /// Сводка для диагностики.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let mut attached = [Attached::default(); ATTACHED_MAX];
        for (slot, device) in attached.iter_mut().zip(self.devices.iter()) {
            *slot = Attached {
                port: device.port,
                vendor: device.identity.0,
                product: device.identity.1,
                kind: device.reader.as_ref().map_or("unknown", Reader::name),
                descriptor: device.described_by,
                interface: device.interface.0,
                interfaces: device.interface.1,
            };
        }
        let reports = self
            .devices
            .iter()
            .map(|device| device.reader.as_ref().map_or(0, Reader::reports))
            .sum();
        Summary {
            devices: self.devices.len(),
            keyboards: self
                .devices
                .iter()
                .filter(|device| matches!(device.reader, Some(Reader::Keyboard(_))))
                .count(),
            mice: self
                .devices
                .iter()
                .filter(|device| matches!(device.reader, Some(Reader::Mouse(_))))
                .count(),
            ports: self.ports,
            occupied: self.occupied,
            attached,
            reports,
            errors: self.errors,
            services: self.services,
            last_error: self.last_error,
            unrecoverable: self.unrecoverable,
        }
    }
}

/// Смещение данных внутри буфера управляющих передач.
///
/// Пакет SETUP лежит в начале страницы, данные — с этого смещения: один буфер
/// на двоих означал бы, что ответ устройства затирает запрос, который
/// контроллер в этот момент ещё читает.
const TRANSFER_DATA: usize = 0x100;

/// Сколько байт данных помещается в буфер управляющих передач.
const TRANSFER_DATA_MAX: usize = PAGE_SIZE - TRANSFER_DATA;

/// Program Interface OHCI — то же число, по которому его называет перепись.
const PROG_IF_OHCI: u8 = 0x10;

/// Отобразить окно регистров контроллера.
///
/// Регистров у OHCI 256 байт, но отображается страница целиком: меньше нельзя,
/// а больше незачем — расширенных блоков, как у xHCI, здесь не бывает.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_bar(bar: PhysAddr) -> Result<usize, MapError> {
    let page = bar.page_align_down();
    let virt = page.to_direct_map();
    let flags =
        crate::mm::PageFlags::READ | crate::mm::PageFlags::WRITE | crate::mm::PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему; прямое отображение взаимно
    // однозначно, поэтому эти адреса не могут пересечься с кодом или стеком.
    unsafe { crate::arch::map_active(virt, page, PAGE_SIZE, flags) }?;
    Ok(virt.as_usize() + (bar.as_u64() - page.as_u64()) as usize)
}

// ---------------------------------------------------------------------------
// Сводка и точки входа
// ---------------------------------------------------------------------------

/// Что драйвер сообщает о себе наружу.
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    /// Сколько устройств поднято.
    pub devices: usize,
    /// Сколько из них разбираются как клавиатуры.
    pub keyboards: usize,
    /// Сколько из них разбираются как указатели.
    pub mice: usize,
    /// Сколько портов у корневого хаба.
    pub ports: usize,
    /// Сколько портов заняты.
    pub occupied: usize,
    /// По записи на поднятое устройство.
    pub attached: [Attached; ATTACHED_MAX],
    /// Сколько отчётов разобрано суммарно.
    pub reports: u64,
    /// Сколько передач завершилось ошибкой.
    pub errors: u64,
    /// Сколько раз задача просыпалась. У этого драйвера пробуждения по часам —
    /// других здесь нет, см. шапку модуля.
    pub services: u64,
    /// Чем закончилась последняя неудачная попытка: порт, шаг и ошибка.
    pub last_error: Option<(u8, Stage, OhciError)>,
    /// Сообщал ли контроллер о неисправимой ошибке.
    pub unrecoverable: bool,
}

/// Единственный контроллер, которым распоряжается этот драйвер.
static CONTROLLER: crate::sync::SpinLock<Option<Controller>> = crate::sync::SpinLock::new(None);

/// Поднять контроллер и всё, что на нём висит. `true`, если ввод появился.
///
/// # Safety
///
/// См. [`Controller::init`].
pub unsafe fn init(rsdp: u64) -> bool {
    // SAFETY: контракт функции.
    let mut controller = match unsafe { Controller::init(rsdp) } {
        Ok(controller) => controller,
        Err(OhciError::Absent) => {
            // Молчание здесь намеренное: перепись уже перечислила всё, что есть
            // на шине, и строка «OHCI нет» после неё была бы вторым ответом на
            // тот же вопрос.
            return false;
        }
        Err(err) => {
            kprintln!("  ohci        : unavailable: {err}");
            return false;
        }
    };

    // SAFETY: контроллер работает.
    let (keyboard, mouse) = unsafe { controller.attach_devices() };
    let summary = controller.summary();
    kprintln!(
        "  ohci        : {} of {} port(s) brought up, {} keyboard(s), {} pointer(s)",
        summary.devices,
        summary.occupied,
        summary.keyboards,
        summary.mice
    );

    let sources = input::sources();
    input::set_sources(input::Sources {
        keyboard: sources.keyboard || keyboard,
        mouse: sources.mouse || mouse,
        ..sources
    });
    *CONTROLLER.lock() = Some(controller);
    keyboard || mouse
}

/// Поднялся ли контроллер.
#[must_use]
pub fn is_present() -> bool {
    CONTROLLER.lock().is_some()
}

/// Как часто забирать отчёты, когда устройства есть.
///
/// Десять миллисекунд — период опроса точки прерываний у обоих загрузочных
/// устройств: чаще бессмысленно, реже заметно пальцам.
const POLL_PERIOD_MS: u64 = 10;

/// Как часто сверять состав портов, когда устройств нет.
///
/// Полсекунды: человеку, воткнувшему клавиатуру, такая задержка незаметна, а
/// контроллеру, на котором никого нет, чаще нечего сказать.
const IDLE_PERIOD_MS: u64 = 500;

/// Как часто сверять состав портов при работающих устройствах.
const PORT_CHECK_PERIOD_MS: u64 = 500;

/// Тело задачи, обслуживающей контроллер.
pub fn service_task() {
    let mut next_port_check = 0u64;
    loop {
        let (devices, now) = {
            let mut guard = CONTROLLER.lock();
            let Some(controller) = guard.as_mut() else {
                return;
            };
            controller.service();
            (controller.devices.len(), crate::time::uptime_ms())
        };

        if now >= next_port_check {
            next_port_check = now.saturating_add(PORT_CHECK_PERIOD_MS);
            if ports_changed() {
                poll_hotplug();
            }
        }

        crate::sched::sleep_ms(if devices == 0 { IDLE_PERIOD_MS } else { POLL_PERIOD_MS });
    }
}

/// Появилось ли на портах что-то новое (или исчезло старое).
fn ports_changed() -> bool {
    match CONTROLLER.lock().as_ref() {
        // SAFETY: контроллер существует, значит окно его регистров отображено.
        Some(controller) => unsafe { controller.ports_differ() },
        None => false,
    }
}

/// Перечислить порты заново.
///
/// Контроллер забирается из глобала целиком — по той же причине, что и у
/// xHCI: перечисление длится сотни миллисекунд, а [`crate::sync::SpinLock`]
/// держится с запрещёнными прерываниями, то есть под ним остановились бы те
/// самые часы, по которым перечисление отсчитывает свои ожидания.
fn poll_hotplug() {
    let taken = CONTROLLER.lock().take();
    let Some(mut controller) = taken else {
        return;
    };
    // SAFETY: контроллер работает, вызов идёт из задачи.
    let changed = unsafe { controller.rescan() };
    if changed {
        let summary = controller.summary();
        kprintln!(
            "  ohci        : now {} device(s), {} keyboard(s), {} pointer(s)",
            summary.devices,
            summary.keyboards,
            summary.mice
        );
    }
    *CONTROLLER.lock() = Some(controller);
}

/// Сводка для диагностики.
#[must_use]
pub fn summary() -> Option<Summary> {
    CONTROLLER.lock().as_ref().map(Controller::summary)
}
