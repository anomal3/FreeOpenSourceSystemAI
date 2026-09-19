//! Драйвер контроллера EHCI: USB 2.0 и хабы за ним.
//!
//! # Зачем он появился
//!
//! Ноутбук ASUS K53SD (Intel Cougar Point) дошёл до рабочего стола и не
//! слушался ни клавиатуры, ни мыши. Перепись контроллеров назвала причину:
//!
//! ```text
//!   usb : 0000:00:1a.0 ehci (prog-if 0x20) vendor 0x8086 device 0x1c2d -- no driver here
//!   usb : 0000:00:1d.0 ehci (prog-if 0x20) vendor 0x8086 device 0x1c26 -- no driver here
//! ```
//!
//! Раньше EHCI пропускался сознательно: сам по себе он не разговаривает с низко-
//! и полноскоростными устройствами, и клавиатура с мышью доставались его спутнику
//! — OHCI, который ядро поднимает. У Intel начиная с Cougar Point спутников нет
//! вовсе: за каждым корневым портом EHCI стоит **встроенный хаб** (rate matching
//! hub), и клавиатура с мышью подключены к нему. Поэтому здесь сразу и драйвер
//! хаба — без него EHCI на этих машинах бесполезен.
//!
//! # Как EHCI устроен
//!
//! Работа описана структурами в памяти, как у OHCI, но их две разновидности:
//!
//! * **QH** (queue head) — конечная точка: адрес устройства, номер точки,
//!   скорость, размер пакета и «окно» (overlay) — копия передачи, которую
//!   контроллер исполняет прямо сейчас;
//! * **qTD** — одна передача: направление, длина, буфер.
//!
//! Управляющие передачи идут по **асинхронному** списку — кольцу QH, которое
//! контроллер обходит без остановки. Точки прерываний висят в **периодическом**
//! списке — таблице на 1024 кадра, каждый элемент которой указывает на цепочку
//! QH, опрашиваемых в этом кадре.
//!
//! У каждого устройства свой QH управляющей точки, и он стоит в кольце всё время,
//! пока устройство подключено. Передача начинается одной записью — адреса
//! первого qTD в окно простаивающего QH, — и это единственное поле, которое
//! драйвер правит у QH, стоящего в списке.
//!
//! # Медленные устройства за хабом: транслятор
//!
//! Клавиатура за встроенным хабом Intel — низко- или полноскоростная, а шина до
//! хаба — высокоскоростная. Разницу берёт на себя **транслятор** хаба (TT):
//! контроллер разбивает каждую передачу на «начать» (start split) и «забрать»
//! (complete split). Драйверу для этого достаточно сказать в QH адрес хаба и
//! номер его порта, а для точек прерываний — в каких микрокадрах слать то и
//! другое.
//!
//! # Что проверено где — сказано вслух
//!
//! Стенд: QEMU `usb-ehci` с высокоскоростными клавиатурой и мышью. Это проверяет
//! контроллер, оба списка, управляющие передачи и отчёты HID. **Хаба и
//! транслятора QEMU не эмулирует**: медленное устройство на `usb-ehci` он не
//! подключает вовсе. Этот путь проверяется только на настоящей машине, поэтому
//! каждый его шаг печатается — журнала там нет, есть фотография экрана.
//!
//! # Спутники
//!
//! На машинах со спутниками (AMD, старые Intel, VirtualBox в режиме «USB 2.0»)
//! запись `CONFIGFLAG` отбирает все порты у спутника. Медленное устройство на
//! таком порту драйвер тут же отдаёт обратно (`Port Owner`), и поэтому EHCI
//! поднимается **раньше** OHCI: к моменту перечисления OHCI устройство уже у него.
//!
//! # Опрос, а не прерывания
//!
//! По той же причине, что у OHCI: линия INTx требует разбора `_PRT` из ACPI, а
//! интерпретатора AML в ядре нет. Задача обслуживания просыпается по часам.

#![allow(clippy::too_many_arguments)]

use alloc::vec::Vec;

use crate::input;
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{DMA_SIZE, MapError, PAGE_SIZE, PhysAddr};
use crate::pci;
use crate::usb::hid::{Reader, choose_reader};
use crate::usb::{self, ATTACHED_MAX, Attached, HidInterface, Stage, Timeout, sleep_ms};

// ---------------------------------------------------------------------------
// Регистры (EHCI 1.0, глава 2)
// ---------------------------------------------------------------------------

/// Первое двойное слово: младший байт — длина блока возможностей, старшее слово
/// — версия интерфейса.
const CAP_LENGTH_VERSION: usize = 0x00;
const CAP_HCSPARAMS: usize = 0x04;
const CAP_HCCPARAMS: usize = 0x08;

/// `HCSPARAMS`: число корневых портов.
const HCS_PORTS_MASK: u32 = 0xF;
/// `HCSPARAMS`: питание портов управляется программно.
const HCS_PPC: u32 = 1 << 4;
/// `HCSPARAMS`: сколько у контроллера спутников.
const HCS_COMPANIONS_SHIFT: u32 = 12;

/// `HCCPARAMS`: 64-битные структуры (приложение B спецификации).
const HCC_AC64: u32 = 1 << 0;
/// `HCCPARAMS`: где в конфигурационном пространстве PCI лежат расширенные
/// возможности.
const HCC_EECP_SHIFT: u32 = 8;

const OP_USBCMD: usize = 0x00;
const OP_USBSTS: usize = 0x04;
const OP_USBINTR: usize = 0x08;
const OP_CTRLDSSEGMENT: usize = 0x10;
const OP_PERIODICLISTBASE: usize = 0x14;
const OP_ASYNCLISTADDR: usize = 0x18;
const OP_CONFIGFLAG: usize = 0x40;
const OP_PORTSC: usize = 0x44;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_PERIODIC: u32 = 1 << 4;
const CMD_ASYNC: u32 = 1 << 5;
/// «Позвонить» при изъятии QH из асинхронного списка.
const CMD_ASYNC_DOORBELL: u32 = 1 << 6;
/// Порог прерываний по умолчанию: 8 микрокадров.
const CMD_ITC_DEFAULT: u32 = 0x08 << 16;

const STS_HOST_ERROR: u32 = 1 << 4;
const STS_ASYNC_ADVANCE: u32 = 1 << 5;
const STS_HALTED: u32 = 1 << 12;
const STS_PERIODIC_ON: u32 = 1 << 14;
const STS_ASYNC_ON: u32 = 1 << 15;
/// Все признаки, сбрасываемые записью единицы.
const STS_ACK_ALL: u32 = 0x3F;
/// Закончился дескриптор, просивший прерывания.
const STS_INTERRUPT: u32 = 1 << 0;
/// То же, но с ошибкой.
const STS_ERROR_INTERRUPT: u32 = 1 << 1;

/// Что разрешаем и что снимаем в обработчике.
///
/// Ровно два признака, и остальные не случайно. Бит `Interrupt on Async
/// Advance` драйвер **ждёт сам** — им подтверждается звонок при перестройке
/// асинхронного кольца, — и обработчик, снявший его первым, оставил бы то
/// ожидание вечным. Бит «системная ошибка хоста» читает `service`, и снимать
/// его за его спиной значило бы прятать неисправность.
const INTERRUPT_CAUSES: u32 = STS_INTERRUPT | STS_ERROR_INTERRUPT;

/// Ярлык, по которому планировщик будит задачу контроллеров.
const IRQ_SOURCE: u32 = crate::irq::source::EHCI;

/// Сколько контроллеров может прислать прерывание.
///
/// Контроллеров у EHCI бывает несколько — чипсеты Intel ставят по два, каждый
/// со своими портами, — поэтому здесь массив, а не одно поле. Четырёх хватает
/// на любую из машин, которые мы видели; пятый останется на опросе и скажет об
/// этом.
const MAX_CONTROLLERS: usize = 4;

/// Окна рабочих регистров — для обработчика прерывания.
static OPERATIONAL: [core::sync::atomic::AtomicUsize; MAX_CONTROLLERS] =
    [const { core::sync::atomic::AtomicUsize::new(0) }; MAX_CONTROLLERS];

/// Просит ли драйвер прерывания по завершении передачи.
static WANT_INTERRUPTS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// Просить ли прерывание у дескриптора запроса отчёта.
///
/// Решается на каждый запрос, а не один раз: бит лежит в самом дескрипторе, и
/// до включения прерываний он обязан быть сброшен — разрешённое, но не
/// обслуживаемое прерывание уровня повесило бы машину намертво.
fn report_interrupt() -> u32 {
    if WANT_INTERRUPTS.load(core::sync::atomic::Ordering::Relaxed) { TOKEN_IOC } else { 0 }
}

/// Прерывание от контроллера.
///
/// Линия разделяемая, и контроллеров на ней может быть несколько, поэтому
/// обходятся все: у каждого спрашивается его собственный регистр состояния.
/// Признаки снимаются записью единиц; не снять их означало бы оставить
/// уровневую линию поднятой — то есть получить то же прерывание снова и снова,
/// пока машина не встанет от занятости.
pub fn on_interrupt() {
    let mut ours = false;
    for slot in &OPERATIONAL {
        let op = slot.load(core::sync::atomic::Ordering::Relaxed);
        if op == 0 {
            continue;
        }
        // SAFETY: адрес положен сюда только после отображения окна, и окно
        // живёт всё время работы ядра.
        let status = unsafe { ((op + OP_USBSTS) as *const u32).read_volatile() } & INTERRUPT_CAUSES;
        if status == 0 {
            continue;
        }
        // SAFETY: см. выше.
        unsafe { ((op + OP_USBSTS) as *mut u32).write_volatile(status) };
        ours = true;
    }
    if ours {
        crate::sched::wake_irq(IRQ_SOURCE);
    }
}

const PORT_CCS: u32 = 1 << 0;
const PORT_CSC: u32 = 1 << 1;
const PORT_PED: u32 = 1 << 2;
const PORT_PEDC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_PR: u32 = 1 << 8;
const PORT_LINE_MASK: u32 = 0b11 << 10;
/// Состояние линии K до сброса — низкоскоростное устройство.
const PORT_LINE_K: u32 = 0b01 << 10;
const PORT_PP: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
/// Биты «изменилось»: единица их сбрасывает, поэтому при записи порта их надо
/// убирать из прочитанного значения.
const PORT_RW1C: u32 = PORT_CSC | PORT_PEDC | PORT_OCC;

/// Расширенная возможность USB Legacy Support (EHCI 1.0, 5.1).
const LEGSUP_ID: u8 = 1;
const LEGSUP_BIOS_OWNED: u32 = 1 << 16;
/// Байт, в котором лежит признак `BIOS Owned`.
const LEGSUP_BIOS_BYTE: usize = 2;
/// Байт, в котором лежит признак `OS Owned`.
const LEGSUP_OS_BYTE: usize = 3;
/// `USBLEGCTLSTS`: разрешения SMI.
const LEGCTLSTS: usize = 4;

// ---------------------------------------------------------------------------
// QH и qTD (EHCI 1.0, 3.5–3.6; 64-битные — приложение B)
// ---------------------------------------------------------------------------

/// Указатель «дальше ничего».
const LINK_TERMINATE: u32 = 1;
/// Тип элемента в указателе: QH.
const LINK_QH: u32 = 0b01 << 1;

const QH_NEXT: usize = 0x00;
const QH_INFO1: usize = 0x04;
const QH_INFO2: usize = 0x08;
const QH_CURRENT: usize = 0x0C;
const QH_OVERLAY_NEXT: usize = 0x10;
const QH_OVERLAY_ALT: usize = 0x14;
const QH_OVERLAY_TOKEN: usize = 0x18;

const INFO1_SPEED_FULL: u32 = 0b00 << 12;
const INFO1_SPEED_LOW: u32 = 0b01 << 12;
const INFO1_SPEED_HIGH: u32 = 0b10 << 12;
/// Переключение DATA0/DATA1 берётся из qTD, а не из окна QH — для управляющих
/// передач, где стадии задают его сами.
const INFO1_TOGGLE_FROM_TD: u32 = 1 << 14;
/// Голова асинхронного кольца.
const INFO1_HEAD: u32 = 1 << 15;
/// Управляющая точка медленного устройства: контроллер обязан знать это ради
/// транслятора.
const INFO1_CONTROL_EP: u32 = 1 << 27;
/// Счётчик NAK для высокоскоростных управляющих точек — как у Linux.
const INFO1_NAK_RELOAD_HS: u32 = 4 << 28;
/// Одна транзакция за микрокадр.
const INFO2_MULT_ONE: u32 = 1 << 30;
/// Опрос в нулевом микрокадре каждого кадра.
const INFO2_START_MASK: u32 = 0x01;
/// Для медленных устройств за транслятором: «забрать» во втором–четвёртом
/// микрокадрах. Та же раскладка у SeaBIOS и coreboot.
const INFO2_COMPLETE_MASK_TT: u32 = 0x1C << 8;

const TD_NEXT: usize = 0x00;
const TD_ALT: usize = 0x04;
const TD_TOKEN: usize = 0x08;
const TD_BUFFER: usize = 0x0C;
/// Старшая половина адреса буфера — у 64-битных структур.
const TD_BUFFER_HIGH: usize = 0x20;

const TOKEN_ACTIVE: u32 = 1 << 7;
const TOKEN_HALTED: u32 = 1 << 6;
const TOKEN_BUFFER_ERROR: u32 = 1 << 5;
const TOKEN_BABBLE: u32 = 1 << 4;
const TOKEN_XACT: u32 = 1 << 3;
const TOKEN_MISSED: u32 = 1 << 2;
const TOKEN_PID_OUT: u32 = 0b00 << 8;
const TOKEN_PID_IN: u32 = 0b01 << 8;
const TOKEN_PID_SETUP: u32 = 0b10 << 8;
/// Три повтора при ошибке транзакции.
const TOKEN_RETRIES: u32 = 0b11 << 10;
const TOKEN_TOGGLE: u32 = 1 << 31;
/// Прервать процессор, когда этот дескриптор закончится.
const TOKEN_IOC: u32 = 1 << 15;
const TOKEN_LENGTH_SHIFT: u32 = 16;
const TOKEN_LENGTH_MASK: u32 = 0x7FFF;

// Раскладка страницы устройства. Смещения кратны 32: младшие пять бит адреса
// заняты признаками в указателях, и структура не на границе — это указатель на
// соседнюю. Места под каждую — с запасом на 64-битный вариант.
const PAGE_CONTROL_QH: usize = 0x000;
const PAGE_INTERRUPT_QH: usize = 0x080;
const PAGE_TD_SETUP: usize = 0x100;
const PAGE_TD_DATA: usize = 0x140;
const PAGE_TD_STATUS: usize = 0x180;
const PAGE_TD_REPORT: usize = 0x1C0;
const PAGE_REPORT: usize = 0x800;
/// Сколько байт отчёта помещается в буфер.
const REPORT_MAX: u16 = 64;

/// Смещение данных внутри буфера управляющих передач: пакет SETUP лежит в
/// начале страницы.
const TRANSFER_DATA: usize = 0x100;
const TRANSFER_DATA_MAX: usize = PAGE_SIZE - TRANSFER_DATA;

// ---------------------------------------------------------------------------
// Запросы хаба (USB 2.0, 11.24)
// ---------------------------------------------------------------------------

const CLASS_HUB: u8 = 9;
const DESC_HUB: u8 = 0x29;
const HUB_REQ_GET_STATUS: u8 = 0;
const HUB_REQ_CLEAR_FEATURE: u8 = 1;
const HUB_REQ_SET_FEATURE: u8 = 3;
/// Запрос класса к порту хаба.
const HUB_TO_PORT: u8 = 0x23;
const HUB_FROM_PORT: u8 = 0xA3;
const HUB_FROM_HUB: u8 = 0xA0;

const FEATURE_PORT_RESET: u8 = 4;
const FEATURE_PORT_POWER: u8 = 8;
const FEATURE_C_PORT_CONNECTION: u8 = 16;
const FEATURE_C_PORT_RESET: u8 = 20;

const HUB_PORT_CONNECTION: u16 = 1 << 0;
const HUB_PORT_ENABLE: u16 = 1 << 1;
const HUB_PORT_RESET: u16 = 1 << 4;
const HUB_PORT_LOW_SPEED: u16 = 1 << 9;
const HUB_PORT_HIGH_SPEED: u16 = 1 << 10;
const HUB_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_CHANGE_RESET: u16 = 1 << 4;

// ---------------------------------------------------------------------------
// Сроки и пределы
// ---------------------------------------------------------------------------

/// Сколько ждать, пока прошивка отпустит контроллер. Столько же ждёт Linux.
const HANDOFF_TIMEOUT_MS: u64 = 1000;
/// Остановка контроллера: спецификация обещает 16 мс.
const HALT_TIMEOUT_MS: u64 = 100;
/// Сброс контроллера.
const RESET_TIMEOUT_MS: u64 = 250;
/// Включение списков.
const SCHEDULE_TIMEOUT_MS: u64 = 100;
/// Передача. Секунда, а не полсекунды, как у OHCI: за транслятором каждая
/// передача — это несколько раундов «начать» и «забрать».
const TRANSFER_TIMEOUT_MS: u64 = 1000;
/// Сколько держать сброс корневого порта: спецификация USB требует 50 мс.
const ROOT_RESET_MS: u64 = 50;
/// Сколько ждать, пока порт хаба закончит сброс.
const PORT_RESET_TIMEOUT_MS: u64 = 500;
/// Пауза после сброса порта.
const PORT_RECOVERY_MS: u64 = 20;
/// Пауза после подключения, прежде чем сбрасывать порт (USB 2.0, 7.1.7.3).
const DEBOUNCE_MS: u64 = 100;
/// Пауза после включения питания портов, если хаб не назвал свою.
const POWER_SETTLE_MS: u64 = 100;
/// Пауза после `SET_ADDRESS`.
const SET_ADDRESS_SETTLE_MS: u64 = 10;

/// Сколько устройств, считая хабы, драйвер поднимает на одном контроллере.
const DEVICES_MAX: usize = 12;
/// Портов у корневого хаба EHCI не больше 15: поле четырёхбитное.
const PORTS_MAX: usize = 15;
/// Сколько хабов подряд драйвер готов пройти. Встроенный хаб Intel — первый.
const HUB_DEPTH_MAX: u8 = 3;
/// Сколько элементов в периодической таблице.
const FRAMES: usize = 1024;
/// Предел обхода расширенных возможностей в конфигурационном пространстве.
const EXT_CAP_LIMIT: usize = 16;

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

/// Почему контроллер или устройство не заработали.
#[derive(Clone, Copy, Debug)]
pub enum EhciError {
    NoBar,
    Map(MapError),
    Dma(DmaError),
    /// Контроллер без 64-битной адресации, а окно DMA — выше 4 ГиБ.
    AddressWidth(u64),
    HaltTimeout,
    ResetTimeout,
    StartTimeout,
    ScheduleTimeout,
    /// Порт хаба не закончил сброс.
    PortResetTimeout,
    /// Порт после сброса остался запрещённым.
    PortNotEnabled,
    TransferTimeout { waited_ms: u64, spun_out: bool },
    /// Передача остановлена; внутри — слово состояния qTD.
    Transfer(u32),
    ShortDescriptor,
    /// Хаб не отдал свой дескриптор.
    HubDescriptor,
    NoHid,
    UnknownHid,
    TooMany,
    /// Хабов подряд больше, чем драйвер проходит.
    TooDeep,
}

impl core::fmt::Display for EhciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoBar => f.write_str("the controller has no memory BAR"),
            Self::Map(err) => write!(f, "the register window could not be mapped: {err}"),
            Self::Dma(err) => write!(f, "no memory for the schedules: {err}"),
            Self::AddressWidth(phys) => write!(
                f,
                "the controller addresses only 32 bits, but its structures lie at {phys:#x}"
            ),
            Self::HaltTimeout => f.write_str("the controller did not stop"),
            Self::ResetTimeout => f.write_str("the controller never left reset"),
            Self::StartTimeout => f.write_str("the controller did not start"),
            Self::ScheduleTimeout => f.write_str("the schedules did not switch on"),
            Self::PortResetTimeout => f.write_str("the port reset never finished"),
            Self::PortNotEnabled => f.write_str("the port stayed disabled after the reset"),
            Self::TransferTimeout { waited_ms, spun_out } => {
                if *spun_out {
                    write!(f, "a transfer never completed ({waited_ms} ms; the spin limit ran out first, so the clock is suspect)")
                } else {
                    write!(f, "a transfer never completed ({waited_ms} ms)")
                }
            }
            Self::Transfer(token) => write!(f, "transfer failed: {}", token_name(*token)),
            Self::ShortDescriptor => f.write_str("the descriptor came back shorter than it claims"),
            Self::HubDescriptor => f.write_str("the hub did not return its descriptor"),
            Self::NoHid => f.write_str(
                "not a keyboard or a pointer: this kernel drives no other USB device yet",
            ),
            Self::UnknownHid => f.write_str("the HID interface speaks neither boot protocol nor a descriptor we understand"),
            Self::TooMany => f.write_str("more devices than the driver brings up"),
            Self::TooDeep => f.write_str("hubs are nested deeper than the driver goes"),
        }
    }
}

impl From<DmaError> for EhciError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

/// Что означает слово состояния остановленного qTD.
///
/// Имена, а не биты: на фотографии экрана «0x48» не скажет ничего, а «ошибка
/// транзакции» сразу отделяет молчащее устройство от отказавшего.
const fn token_name(token: u32) -> &'static str {
    if token & TOKEN_BABBLE != 0 {
        "babble: the device sent more than asked"
    } else if token & TOKEN_BUFFER_ERROR != 0 {
        "data buffer error"
    } else if token & TOKEN_XACT != 0 {
        "transaction error: no response, CRC or timeout"
    } else if token & TOKEN_MISSED != 0 {
        "missed micro-frame: the translator lost the complete split"
    } else {
        "STALL from the device"
    }
}

// ---------------------------------------------------------------------------
// Скорость и место устройства
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Speed {
    Low,
    Full,
    High,
}

impl Speed {
    const fn name(self) -> &'static str {
        match self {
            Self::Low => "low speed",
            Self::Full => "full speed",
            Self::High => "high speed",
        }
    }
}

/// Где устройство: на корневом порту или на порту хаба. Для журнала.
#[derive(Clone, Copy)]
struct Place {
    root_port: u8,
    parent: u8,
    hub_port: u8,
}

impl core::fmt::Display for Place {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.parent == 0 {
            write!(f, "root port {}", self.root_port)
        } else {
            write!(f, "hub {} port {}", self.parent, self.hub_port)
        }
    }
}

// ---------------------------------------------------------------------------
// Страница устройства
// ---------------------------------------------------------------------------

/// Всё, что нужно для передачи устройству: где его QH и qTD и как к нему
/// обращаться. Копируется — чтобы передача не держала заимствование устройства,
/// пока драйвер меняет свой список.
#[derive(Clone, Copy)]
struct Pipe {
    /// Виртуальный и физический адреса страницы устройства.
    virt: usize,
    phys: u32,
    address: u8,
    max_packet: u16,
    speed: Speed,
    /// Транслятор: адрес высокоскоростного хаба и номер его порта.
    tt: Option<(u8, u8)>,
}

impl Pipe {
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: страница устройства выделена целиком, смещения — константы
        // раскладки внутри неё. `volatile` — поля правит контроллер.
        unsafe { ((self.virt + offset) as *const u32).read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: см. [`Pipe::read`].
        unsafe { ((self.virt + offset) as *mut u32).write_volatile(value) }
    }

    /// Физический адрес структуры внутри страницы.
    const fn at(&self, offset: usize) -> u32 {
        self.phys + offset as u32
    }

    /// Поля QH управляющей точки.
    fn control_info(&self) -> (u32, u32) {
        let mut info1 =
            u32::from(self.address) | (u32::from(self.max_packet) << 16) | INFO1_TOGGLE_FROM_TD;
        let mut info2 = INFO2_MULT_ONE;
        match self.speed {
            Speed::High => info1 |= INFO1_SPEED_HIGH | INFO1_NAK_RELOAD_HS,
            Speed::Full | Speed::Low => {
                info1 |= INFO1_CONTROL_EP
                    | if self.speed == Speed::Low { INFO1_SPEED_LOW } else { INFO1_SPEED_FULL };
                info2 |= self.tt_fields();
            }
        }
        (info1, info2)
    }

    /// Поля QH точки прерываний.
    fn interrupt_info(&self, endpoint: u8, max_packet: u16) -> (u32, u32) {
        let mut info1 =
            u32::from(self.address) | (u32::from(endpoint) << 8) | (u32::from(max_packet) << 16);
        let mut info2 = INFO2_MULT_ONE | INFO2_START_MASK;
        match self.speed {
            Speed::High => info1 |= INFO1_SPEED_HIGH,
            Speed::Full | Speed::Low => {
                info1 |= if self.speed == Speed::Low { INFO1_SPEED_LOW } else { INFO1_SPEED_FULL };
                info2 |= INFO2_COMPLETE_MASK_TT | self.tt_fields();
            }
        }
        (info1, info2)
    }

    /// Адрес хаба и номер порта транслятора в раскладке `info2`.
    fn tt_fields(&self) -> u32 {
        self.tt.map_or(0, |(hub, port)| (u32::from(hub) << 16) | (u32::from(port) << 23))
    }
}

/// Подключённое устройство.
struct Device {
    place: Place,
    address: u8,
    speed: Speed,
    tt: Option<(u8, u8)>,
    /// Страница с QH, qTD и буфером отчёта.
    page: DmaBuffer,
    max_packet: u16,
    /// Сколько хабов над устройством.
    depth: u8,
    /// Портов у хаба; ноль — устройство не хаб.
    hub_ports: u8,
    /// Какие порты хаба заняты — по биту на порт, считая с первого.
    hub_connected: u32,
    reader: Option<Reader>,
    report_len: u16,
    identity: (u16, u16),
    described_by: u16,
    interface: (u8, u8),
    /// Это второй (третий, …) интерфейс HID того же физического устройства.
    ///
    /// Такая запись живёт на **чужом** адресе: управляющая точка у устройства
    /// одна, и кольцо для неё заводит первая запись. Спутник несёт только свою
    /// точку прерываний — то есть только своё кольцо в периодическом списке.
    /// Вписать его QH в асинхронное кольцо значило бы два разных QH на одну и ту
    /// же точку 0 одного и того же адреса, чего спецификация не разрешает и что
    /// контроллер обычно переживает как молчание устройства.
    secondary: bool,
    /// Сколько передач отчёта подряд кончились остановкой точки.
    halts: u32,
}

impl Device {
    fn pipe(&self) -> Pipe {
        Pipe {
            // SAFETY: страница выделена `dma::alloc` и живёт, пока живёт
            // устройство; адрес только запоминается.
            virt: unsafe { self.page.as_ptr::<u8>() } as usize,
            phys: self.page.phys().as_u64() as u32,
            address: self.address,
            max_packet: self.max_packet,
            speed: self.speed,
            tt: self.tt,
        }
    }
}

// ---------------------------------------------------------------------------
// Контроллер
// ---------------------------------------------------------------------------

/// Контроллер и всё, что к нему подключено.
pub struct Controller {
    pci: pci::Address,
    op: usize,
    ports: usize,
    companions: u8,
    /// Управляет ли контроллер питанием портов сам (`PPC`).
    power_control: bool,
    ac64: bool,
    /// Старшая половина адресов окна DMA — `CTRLDSSEGMENT` у 64-битных структур.
    segment: u32,
    frame_list: DmaBuffer,
    /// Голова асинхронного кольца.
    head: DmaBuffer,
    transfer: DmaBuffer,
    devices: Vec<Device>,
    /// Какие корневые порты заняты устройствами, оставшимися у этого контроллера.
    connected: u32,
    errors: u64,
    services: u64,
    last_error: Option<(u8, Stage, EhciError)>,
    unrecoverable: bool,
    /// Адреса устройств, которые надо поднять заново: их точка прерываний
    /// останавливается раз за разом (см. [`RECOVER_AFTER`]). Поднимает сверка
    /// портов — в задаче и без замка, потому что это сброс порта с паузами.
    recover: Vec<u8>,
}

impl Controller {
    fn read(&self, offset: usize) -> u32 {
        // SAFETY: окно отображено в `init`, смещения — константы спецификации
        // внутри первой страницы.
        unsafe { ((self.op + offset) as *const u32).read_volatile() }
    }

    fn write(&self, offset: usize, value: u32) {
        // SAFETY: см. [`Controller::read`].
        unsafe { ((self.op + offset) as *mut u32).write_volatile(value) }
    }

    fn port(&self, index: usize) -> u32 {
        self.read(OP_PORTSC + index * 4)
    }

    /// Записать порт, не сбросив попутно признаков «изменилось».
    fn set_port(&self, index: usize, value: u32) {
        self.write(OP_PORTSC + index * 4, value & !PORT_RW1C);
    }

    fn head_pipe(&self) -> Pipe {
        Pipe {
            // SAFETY: страница головы выделена в `init` и живёт, пока живёт
            // контроллер.
            virt: unsafe { self.head.as_ptr::<u8>() } as usize,
            phys: self.head.phys().as_u64() as u32,
            address: 0,
            max_packet: 0,
            speed: Speed::High,
            tt: None,
        }
    }

    /// Поднять контроллер: отобрать у прошивки, сбросить, завести оба списка.
    ///
    /// # Safety
    ///
    /// Ядро на собственных таблицах, прерывания разрешены, ни одного
    /// [`crate::sync::SpinLock`] не удерживается.
    unsafe fn init(device: &pci::Device) -> Result<Self, EhciError> {
        // SAFETY: списки ещё не заведены, но контроллер и не работает — ниже он
        // останавливается и сбрасывается.
        unsafe { device.enable_bus_master() };
        let bar = device.memory_bar(0).ok_or(EhciError::NoBar)?;
        // SAFETY: контракт функции.
        let base = unsafe { map_bar(bar) }.map_err(EhciError::Map)?;

        // SAFETY: окно отображено строкой выше.
        let (length_version, hcs, hcc) = unsafe {
            (
                ((base + CAP_LENGTH_VERSION) as *const u32).read_volatile(),
                ((base + CAP_HCSPARAMS) as *const u32).read_volatile(),
                ((base + CAP_HCCPARAMS) as *const u32).read_volatile(),
            )
        };
        let op = base + (length_version & 0xFF) as usize;
        let version = length_version >> 16;
        let ports = ((hcs & HCS_PORTS_MASK) as usize).min(PORTS_MAX);
        let companions = ((hcs >> HCS_COMPANIONS_SHIFT) & 0xF) as u8;
        let power_control = hcs & HCS_PPC != 0;
        let ac64 = hcc & HCC_AC64 != 0;
        kprintln!(
            "  ehci        : {} vendor {:#06x} device {:#06x}, version {}.{}, {ports} port(s), {companions} companion(s), {}-bit structures",
            device.address,
            device.vendor,
            device.device,
            version >> 8,
            (version & 0xFF) >> 4,
            if ac64 { 64 } else { 32 }
        );

        // Контроллер забирается у прошивки раньше всего остального — см.
        // [`take_from_firmware`].
        // SAFETY: конфигурационное пространство отображено перебором шины.
        unsafe { take_from_firmware(device, ((hcc >> HCC_EECP_SHIFT) & 0xFF) as usize) };

        let frame_list = dma::alloc(PAGE_SIZE)?;
        let head = dma::alloc(PAGE_SIZE)?;
        let transfer = dma::alloc(PAGE_SIZE)?;

        // Все структуры лежат в одном окне DMA размером `DMA_SIZE`. Контроллеру
        // без 64-битной адресации нужно, чтобы оно целиком было ниже 4 ГиБ;
        // 64-битному — чтобы оно не пересекало границу четырёх гигабайт: старшая
        // половина адресов QH и qTD у него одна на всех (`CTRLDSSEGMENT`).
        let lowest = frame_list.phys().as_u64().min(head.phys().as_u64()).min(transfer.phys().as_u64());
        let highest = lowest + DMA_SIZE as u64 - 1;
        if (!ac64 && highest >> 32 != 0) || (lowest >> 32) != (highest >> 32) {
            return Err(EhciError::AddressWidth(lowest));
        }

        let controller = Self {
            pci: device.address,
            op,
            ports,
            companions,
            power_control,
            ac64,
            segment: (lowest >> 32) as u32,
            frame_list,
            head,
            transfer,
            devices: Vec::new(),
            connected: 0,
            errors: 0,
            services: 0,
            last_error: None,
            unrecoverable: false,
            recover: Vec::new(),
        };
        // SAFETY: окно регистров отображено, буферы выделены и обнулены.
        unsafe { controller.start() }?;
        Ok(controller)
    }

    /// Остановить, сбросить и запустить контроллер с пустыми списками.
    ///
    /// # Safety
    ///
    /// Окно регистров отображено, буферы выделены.
    unsafe fn start(&self) -> Result<(), EhciError> {
        let command = self.read(OP_USBCMD);
        self.write(OP_USBCMD, command & !CMD_RUN);
        let mut timeout = Timeout::new(HALT_TIMEOUT_MS);
        while self.read(OP_USBSTS) & STS_HALTED == 0 {
            if timeout.expired() {
                return Err(EhciError::HaltTimeout);
            }
        }

        self.write(OP_USBCMD, CMD_RESET);
        let mut timeout = Timeout::new(RESET_TIMEOUT_MS);
        while self.read(OP_USBCMD) & CMD_RESET != 0 {
            if timeout.expired() {
                return Err(EhciError::ResetTimeout);
            }
        }

        if self.ac64 {
            self.write(OP_CTRLDSSEGMENT, self.segment);
        }
        // Прерывания запрещены все: драйвер работает опросом, и разрешённое, но
        // не обслуживаемое прерывание уровня повесило бы машину.
        self.write(OP_USBINTR, 0);
        self.write(OP_USBSTS, STS_ACK_ALL);

        // Периодическая таблица: все 1024 кадра пусты.
        // SAFETY: страница выделена под таблицу; 1024 указателя по 4 байта.
        unsafe {
            let table = self.frame_list.as_ptr::<u32>();
            for frame in 0..FRAMES {
                table.add(frame).write_volatile(LINK_TERMINATE);
            }
        }
        self.write(OP_PERIODICLISTBASE, self.frame_list.phys().as_u64() as u32);

        // Голова асинхронного кольца: указывает сама на себя и никогда ничего не
        // исполняет — окно у неё остановлено.
        let head = self.head_pipe();
        head.write(QH_NEXT, head.phys | LINK_QH);
        head.write(QH_INFO1, INFO1_HEAD | INFO1_SPEED_HIGH);
        head.write(QH_INFO2, INFO2_MULT_ONE);
        head.write(QH_CURRENT, 0);
        head.write(QH_OVERLAY_NEXT, LINK_TERMINATE);
        head.write(QH_OVERLAY_ALT, LINK_TERMINATE);
        head.write(QH_OVERLAY_TOKEN, TOKEN_HALTED);
        self.write(OP_ASYNCLISTADDR, head.phys);

        self.write(OP_USBCMD, CMD_ITC_DEFAULT | CMD_RUN);
        let mut timeout = Timeout::new(HALT_TIMEOUT_MS);
        while self.read(OP_USBSTS) & STS_HALTED != 0 {
            if timeout.expired() {
                return Err(EhciError::StartTimeout);
            }
        }

        // Все порты — этому контроллеру. Медленные устройства он отдаст спутнику
        // сам, при перечислении.
        self.write(OP_CONFIGFLAG, 1);
        sleep_ms(5);

        self.write(OP_USBCMD, self.read(OP_USBCMD) | CMD_ASYNC | CMD_PERIODIC);
        let mut timeout = Timeout::new(SCHEDULE_TIMEOUT_MS);
        loop {
            let status = self.read(OP_USBSTS);
            if status & (STS_ASYNC_ON | STS_PERIODIC_ON) == STS_ASYNC_ON | STS_PERIODIC_ON {
                break;
            }
            if timeout.expired() {
                return Err(EhciError::ScheduleTimeout);
            }
        }
        Ok(())
    }

    /// Включить питание корневых портов и поднять всё, что на них висит.
    ///
    /// # Safety
    ///
    /// Контроллер работает; вызов из задачи или при загрузке, не из прерывания.
    unsafe fn attach_devices(&mut self) {
        // Питание портов включается, только если контроллер им управляет:
        // без `PPC` бит `PP` только для чтения и всегда стоит.
        if self.power_control {
            for index in 0..self.ports {
                let status = self.port(index);
                if status & PORT_PP == 0 {
                    self.set_port(index, status | PORT_PP);
                }
            }
        }
        // После сброса контроллера порты переподключаются заново; устройству
        // нужно время, чтобы контроллер его увидел.
        sleep_ms(POWER_SETTLE_MS);

        for index in 0..self.ports {
            if self.port(index) & PORT_CCS != 0 {
                // SAFETY: контракт функции.
                unsafe { self.attach_root_port(index) };
            }
        }
        self.connected = self.connected_mask();
    }

    /// Поднять устройство на корневом порту.
    ///
    /// # Safety
    ///
    /// См. [`Controller::attach_devices`].
    unsafe fn attach_root_port(&mut self, index: usize) {
        let root_port = (index + 1) as u8;
        let place = Place { root_port, parent: 0, hub_port: 0 };
        let status = self.port(index);
        // SET_PORT_CSC снимается сразу: иначе следующая сверка приняла бы старое
        // подключение за новое.
        self.write(OP_PORTSC + index * 4, (status & !PORT_RW1C) | PORT_CSC);

        // Низкоскоростное устройство видно ещё до сброса — по состоянию линии.
        // Сбрасывать его этим контроллером бессмысленно: говорить с ним он не
        // умеет.
        let high = if status & PORT_LINE_MASK == PORT_LINE_K {
            false
        } else {
            // SAFETY: контракт функции.
            match unsafe { self.reset_root_port(index) } {
                Ok(high) => high,
                Err(err) => {
                    kprintln!("  ehci        : {place} stopped while {}: {err}", Stage::Reset);
                    self.last_error = Some((root_port, Stage::Reset, err));
                    return;
                }
            }
        };
        if !high {
            if self.companions > 0 {
                self.set_port(index, self.port(index) | PORT_OWNER);
                kprintln!("  ehci        : {place}: a full- or low-speed device, handed to the companion controller");
            } else {
                kprintln!("  ehci        : {place}: a full- or low-speed device and no companion controller to hand it to");
            }
            return;
        }

        // SAFETY: порт сброшен и разрешён.
        if let Err((stage, err)) = unsafe { self.attach(place, Speed::High, None, 0) } {
            kprintln!("  ehci        : {place} stopped while {stage}: {err}");
            self.last_error = Some((root_port, stage, err));
        }
    }

    /// Сбросить корневой порт. `true` — на нём высокоскоростное устройство.
    ///
    /// # Safety
    ///
    /// Контроллер работает.
    unsafe fn reset_root_port(&mut self, index: usize) -> Result<bool, EhciError> {
        let status = self.port(index);
        self.set_port(index, (status & !PORT_PED) | PORT_PR);
        sleep_ms(ROOT_RESET_MS);
        self.set_port(index, self.port(index) & !PORT_PR);
        let mut timeout = Timeout::new(PORT_RESET_TIMEOUT_MS);
        while self.port(index) & PORT_PR != 0 {
            if timeout.expired() {
                return Err(EhciError::PortResetTimeout);
            }
        }
        sleep_ms(PORT_RECOVERY_MS);
        let status = self.port(index);
        if status & PORT_CCS == 0 {
            return Err(EhciError::PortNotEnabled);
        }
        // Разрешённый после сброса порт — высокоскоростное устройство. Не
        // разрешённый при живом подключении — полноскоростное: его EHCI отдаёт.
        Ok(status & PORT_PED != 0)
    }

    /// Свободный адрес на шине этого контроллера.
    fn free_address(&self) -> Option<u8> {
        (1..=127u8).find(|address| self.devices.iter().all(|device| device.address != *address))
    }

    /// Поднять устройство, уже сброшенное на своём порту.
    ///
    /// # Safety
    ///
    /// Контроллер работает, порт сброшен и разрешён, других устройств по адресу
    /// 0 на шине нет.
    unsafe fn attach(
        &mut self,
        place: Place,
        speed: Speed,
        tt: Option<(u8, u8)>,
        depth: u8,
    ) -> Result<(), (Stage, EhciError)> {
        if self.devices.len() >= DEVICES_MAX {
            return Err((Stage::Reset, EhciError::TooMany));
        }
        let page = dma::alloc(PAGE_SIZE).map_err(|err| (Stage::Address, err.into()))?;
        page.zero();
        let mut device = Device {
            place,
            address: 0,
            speed,
            tt,
            page,
            // До первого дескриптора размер пакета неизвестен. Восемь — то, что
            // обязано уметь любое медленное устройство; у высокоскоростного
            // управляющая точка всегда 64.
            max_packet: if speed == Speed::High { 64 } else { 8 },
            depth,
            hub_ports: 0,
            hub_connected: 0,
            reader: None,
            report_len: 0,
            identity: (0, 0),
            described_by: 0,
            interface: (0, 0),
            secondary: false,
            halts: 0,
        };

        // SAFETY: страница выделена и обнулена.
        unsafe { self.open_control(&device) };
        // SAFETY: QH в кольце, устройство отвечает по адресу 0.
        match unsafe { self.bring_up(&mut device) } {
            Ok(extra) => {
                let is_hub = device.hub_ports > 0;
                let address = device.address;
                self.devices.push(device);
                // Спутники — сразу следом: они уже настроены, и до общей
                // пересборки периодического списка их кольца никуда не ведут.
                self.devices.extend(extra);
                if is_hub {
                    // SAFETY: хаб поднят и стоит в списке устройств.
                    unsafe { self.scan_hub(address) };
                } else {
                    // SAFETY: точка прерываний устройства заведена.
                    unsafe { self.relink_periodic() };
                }
                Ok(())
            }
            Err(failure) => {
                // SAFETY: устройство не в списке — кольцо пересобирается без
                // него, и только после этого его страница освобождается.
                unsafe { self.retire(alloc::vec![device], false) };
                Err(failure)
            }
        }
    }

    /// Прочитать дескрипторы, выдать адрес и поднять устройство как хаб или HID.
    ///
    /// # Safety
    ///
    /// См. [`Controller::attach`]; QH управляющей точки уже в кольце.
    unsafe fn bring_up(&mut self, device: &mut Device) -> Result<Vec<Device>, (Stage, EhciError)> {
        let place = device.place;

        // SAFETY: контракт функции.
        let read = unsafe { self.get_descriptor(device.pipe(), usb::DESC_DEVICE, 0, 8) }
            .map_err(|err| (Stage::Address, err))?;
        if read < 8 {
            return Err((Stage::Address, EhciError::ShortDescriptor));
        }
        let first = self.transfer_bytes(read);
        if let Some(desc) = usb::DeviceDescriptor::parse(first) {
            if desc.max_packet_size0 >= 8 {
                device.max_packet = u16::from(desc.max_packet_size0);
            }
        }

        let address = self.free_address().ok_or((Stage::Address, EhciError::TooMany))?;
        // SAFETY: см. выше.
        unsafe {
            self.control_transfer(
                device.pipe(),
                [0, usb::REQ_SET_ADDRESS, address, 0, 0, 0, 0, 0],
                0,
                false,
            )
        }
        .map_err(|err| (Stage::Address, err))?;
        sleep_ms(SET_ADDRESS_SETTLE_MS);
        device.address = address;

        // Полный дескриптор устройства: идентификаторы и класс.
        // SAFETY: устройство адресовано.
        let read = unsafe { self.get_descriptor(device.pipe(), usb::DESC_DEVICE, 0, 18) }
            .map_err(|err| (Stage::Describe, err))?;
        let bytes = self.transfer_bytes(read);
        let class = if read >= 5 { bytes[4] } else { 0 };
        if let Some(full) = usb::DeviceDescriptor::parse(bytes) {
            device.identity = (full.vendor, full.product);
        }

        // SAFETY: устройство адресовано.
        let read = unsafe { self.get_descriptor(device.pipe(), usb::DESC_CONFIGURATION, 0, 9) }
            .map_err(|err| (Stage::Describe, err))?;
        if read < 9 {
            return Err((Stage::Describe, EhciError::ShortDescriptor));
        }
        let head = self.transfer_bytes(read);
        let total = u16::from_le_bytes([head[2], head[3]]).min(TRANSFER_DATA_MAX as u16);
        let configuration = head[5];
        // SAFETY: см. выше.
        let read = unsafe { self.get_descriptor(device.pipe(), usb::DESC_CONFIGURATION, 0, total) }
            .map_err(|err| (Stage::Describe, err))?;
        let bytes = self.transfer_bytes(read);
        let interface_class = if bytes.len() >= 18 && bytes[10] == usb::DESC_INTERFACE { bytes[14] } else { 0 };

        if class == CLASS_HUB || interface_class == CLASS_HUB {
            if device.depth >= HUB_DEPTH_MAX {
                return Err((Stage::Configure, EhciError::TooDeep));
            }
            // SAFETY: устройство адресовано и описано.
            unsafe { self.configure_hub(device, configuration) }
                .map_err(|err| (Stage::Configure, err))?;
            kprintln!(
                "  ehci        : {place}: hub {:04x}:{:04x} at address {}, {}, {} port(s)",
                device.identity.0,
                device.identity.1,
                device.address,
                device.speed.name(),
                device.hub_ports
            );
            return Ok(Vec::new());
        }

        let found = usb::find_hid(bytes).ok_or((Stage::Describe, EhciError::NoHid))?;
        // Остальные интерфейсы HID выписываются **сейчас**, пока конфигурация
        // под рукой, и это не порядок ради порядка: `bytes` — срез буфера
        // передач самого контроллера, то есть заём `self`, а поднять по нему
        // устройство нельзя — подъём требует `&mut self`. Выписка отпускает
        // буфер, и заём кончается здесь же.
        let others: Vec<HidInterface> = (1..found.interfaces)
            .filter_map(|index| usb::find_hid_nth(bytes, index))
            .collect();

        // SAFETY: устройство адресовано и описано.
        let reader = unsafe { self.enable_reports(device, &found) }
            .map_err(|err| (Stage::Enable, err))?;
        device.report_len = found.max_packet_size.clamp(1, REPORT_MAX);
        device.interface = (found.interface, found.interfaces);
        device.reader = Some(reader);
        // SAFETY: устройство сконфигурировано.
        unsafe { self.open_interrupt(device, &found) };

        kprintln!(
            "  ehci        : {place}: {} {:04x}:{:04x} {}, {} byte reports{}",
            device.speed.name(),
            device.identity.0,
            device.identity.1,
            device.reader.as_ref().map_or("unknown", Reader::name),
            device.report_len,
            match device.tt {
                Some((hub, port)) => alloc::format!(", through the translator of hub {hub} port {port}"),
                None => alloc::string::String::new(),
            }
        );

        // Остальные интерфейсы HID того же устройства — по записи на каждый.
        // Это ровно те приёмники беспроводных мышей, у которых нулевой
        // интерфейс — клавиатура, а мышь висит на первом: до этой фазы драйвер
        // поднимал нулевой, сообщал о работающей клавиатуре, которая никогда
        // ничего не пришлёт, и указателя в системе не появлялось.
        let mut extra = Vec::new();
        for next in others {
            if self.devices.len() + extra.len() + 1 >= DEVICES_MAX {
                break;
            }
            // SAFETY: устройство адресовано и сконфигурировано.
            match unsafe { self.open_satellite(device, &next, found.interfaces) } {
                Ok(satellite) => extra.push(satellite),
                Err(err) => kprintln!(
                    "  ehci        : {place}: interface {} stopped while {}: {err}",
                    next.interface,
                    Stage::Enable
                ),
            }
        }
        Ok(extra)
    }

    /// Поднять ещё один интерфейс HID того же физического устройства.
    ///
    /// Своей управляющей точки у такой записи нет: она одна на устройство и
    /// принадлежит первой записи (см. [`Device::secondary`]). Своя у спутника
    /// только точка прерываний — и своя страница под неё.
    ///
    /// # Safety
    ///
    /// `owner` адресован и сконфигурирован, его QH управляющей точки в кольце.
    unsafe fn open_satellite(
        &mut self,
        owner: &Device,
        found: &HidInterface,
        interfaces: u8,
    ) -> Result<Device, EhciError> {
        let page = dma::alloc(PAGE_SIZE)?;
        page.zero();
        let mut satellite = Device {
            place: owner.place,
            address: owner.address,
            speed: owner.speed,
            tt: owner.tt,
            page,
            max_packet: owner.max_packet,
            depth: owner.depth,
            hub_ports: 0,
            hub_connected: 0,
            reader: None,
            report_len: found.max_packet_size.clamp(1, REPORT_MAX),
            identity: owner.identity,
            described_by: 0,
            interface: (found.interface, interfaces),
            secondary: true,
            halts: 0,
        };

        // SAFETY: контракт функции; управляющая точка — первой записи.
        let (reader, described_by) = unsafe { self.enable_reports_on(owner.pipe(), owner.place, found) }?;
        satellite.described_by = described_by;
        satellite.reader = Some(reader);
        // SAFETY: страница спутника выделена и обнулена.
        unsafe { self.open_interrupt(&satellite, found) };

        kprintln!(
            "  ehci        : {}: interface {} of the same device is a {}, {} byte reports",
            satellite.place,
            found.interface,
            satellite.reader.as_ref().map_or("unknown", Reader::name),
            satellite.report_len
        );
        Ok(satellite)
    }

    /// Выбрать конфигурацию хаба, прочитать его дескриптор, включить питание
    /// портов.
    ///
    /// # Safety
    ///
    /// Устройство адресовано.
    unsafe fn configure_hub(&mut self, device: &mut Device, configuration: u8) -> Result<(), EhciError> {
        let pipe = device.pipe();
        // SAFETY: контракт функции.
        unsafe {
            self.control_transfer(pipe, [0, usb::REQ_SET_CONFIGURATION, configuration, 0, 0, 0, 0, 0], 0, false)
        }?;
        // SAFETY: хаб сконфигурирован.
        let read = unsafe {
            self.control_transfer(pipe, [HUB_FROM_HUB, usb::REQ_GET_DESCRIPTOR, 0, DESC_HUB, 0, 0, 9, 0], 9, true)
        }?;
        if read < 7 {
            return Err(EhciError::HubDescriptor);
        }
        let bytes = self.transfer_bytes(read);
        let ports = bytes[2].min(31);
        let power_good = u64::from(bytes[5]) * 2;

        for port in 1..=ports {
            // SAFETY: см. выше.
            let _ = unsafe {
                self.control_transfer(pipe, [HUB_TO_PORT, HUB_REQ_SET_FEATURE, FEATURE_PORT_POWER, 0, port, 0, 0, 0], 0, false)
            };
        }
        sleep_ms(power_good.max(POWER_SETTLE_MS));
        device.hub_ports = ports;
        Ok(())
    }

    /// Состояние порта хаба: `(status, change)`.
    ///
    /// # Safety
    ///
    /// Хаб сконфигурирован.
    unsafe fn hub_port_status(&mut self, hub: Pipe, port: u8) -> Result<(u16, u16), EhciError> {
        // SAFETY: контракт функции.
        let read = unsafe {
            self.control_transfer(hub, [HUB_FROM_PORT, HUB_REQ_GET_STATUS, 0, 0, port, 0, 4, 0], 4, true)
        }?;
        if read < 4 {
            return Err(EhciError::ShortDescriptor);
        }
        let bytes = self.transfer_bytes(4);
        Ok((u16::from_le_bytes([bytes[0], bytes[1]]), u16::from_le_bytes([bytes[2], bytes[3]])))
    }

    /// Запрос `CLEAR_FEATURE` или `SET_FEATURE` к порту хаба.
    ///
    /// # Safety
    ///
    /// Хаб сконфигурирован.
    unsafe fn hub_port_feature(&mut self, hub: Pipe, port: u8, set: bool, feature: u8) -> Result<(), EhciError> {
        let request = if set { HUB_REQ_SET_FEATURE } else { HUB_REQ_CLEAR_FEATURE };
        // SAFETY: контракт функции.
        unsafe { self.control_transfer(hub, [HUB_TO_PORT, request, feature, 0, port, 0, 0, 0], 0, false) }
            .map(|_| ())
    }

    /// Пройти порты хаба и поднять всё, что на них нашлось.
    ///
    /// # Safety
    ///
    /// Хаб с адресом `address` поднят и стоит в списке устройств.
    unsafe fn scan_hub(&mut self, address: u8) {
        let Some(ports) = self.devices.iter().find(|d| d.address == address).map(|d| d.hub_ports) else {
            return;
        };
        for port in 1..=ports {
            // SAFETY: контракт функции.
            unsafe { self.check_hub_port(address, port) };
        }
    }

    /// Сверить один порт хаба с тем, что о нём известно, и поднять или забыть
    /// устройство. `true` — что-то изменилось.
    ///
    /// # Safety
    ///
    /// Хаб с адресом `address` поднят и стоит в списке устройств.
    unsafe fn check_hub_port(&mut self, address: u8, port: u8) -> bool {
        let Some(hub) = self.devices.iter().find(|d| d.address == address) else {
            return false;
        };
        let pipe = hub.pipe();
        let place = Place { root_port: hub.place.root_port, parent: address, hub_port: port };
        let (hub_speed, hub_tt, depth) = (hub.speed, hub.tt, hub.depth);
        let known = hub.hub_connected & (1 << port) != 0;

        // SAFETY: контракт функции.
        let (status, change) = match unsafe { self.hub_port_status(pipe, port) } {
            Ok(state) => state,
            Err(err) => {
                // Только при первой сверке: хаб, переставший отвечать, иначе
                // печатал бы строку на каждый порт раз в полсекунды.
                if self.devices.iter().find(|d| d.address == address).is_some_and(|hub| hub.hub_connected & 1 == 0) {
                    kprintln!("  ehci        : {place}: the port status could not be read: {err}");
                    self.set_hub_bit(address, 0, true);
                }
                return false;
            }
        };
        if change & HUB_CHANGE_CONNECTION != 0 {
            // SAFETY: см. выше.
            let _ = unsafe { self.hub_port_feature(pipe, port, false, FEATURE_C_PORT_CONNECTION) };
        }
        let connected = status & HUB_PORT_CONNECTION != 0;
        // Переподключение между двумя сверками: признак изменения стоит, а
        // устройство, которое мы знали, на порту уже другое.
        let replugged = known && connected && change & HUB_CHANGE_CONNECTION != 0;

        if known && (!connected || replugged) {
            let victims: Vec<u8> = self
                .devices
                .iter()
                .filter(|d| d.place.parent == address && d.place.hub_port == port)
                .map(|d| d.address)
                .collect();
            let taken = self.take_subtree(victims);
            // SAFETY: устройства изъяты из списка.
            unsafe { self.retire(taken, true) };
            self.set_hub_bit(address, port, false);
            if !replugged {
                return true;
            }
        }
        if !connected || (known && !replugged) {
            return false;
        }

        self.set_hub_bit(address, port, true);
        kprintln!("  ehci        : {place}: connected, resetting");
        sleep_ms(DEBOUNCE_MS);
        // SAFETY: см. выше.
        let speed = match unsafe { self.reset_hub_port(pipe, port) } {
            Ok(speed) => speed,
            Err(err) => {
                kprintln!("  ehci        : {place} stopped while {}: {err}", Stage::Reset);
                self.last_error = Some((place.root_port, Stage::Reset, err));
                return true;
            }
        };
        // Транслятор — у ближайшего высокоскоростного хаба на пути. Медленное
        // устройство за высокоскоростным хабом пользуется его транслятором;
        // устройство за медленным хабом — тем же, что и сам этот хаб.
        let tt = match (speed, hub_speed) {
            (Speed::High, _) => None,
            (_, Speed::High) => Some((address, port)),
            _ => hub_tt,
        };
        // SAFETY: порт сброшен и разрешён.
        if let Err((stage, err)) = unsafe { self.attach(place, speed, tt, depth + 1) } {
            kprintln!("  ehci        : {place} stopped while {stage}: {err}");
            self.last_error = Some((place.root_port, stage, err));
        }
        true
    }

    fn set_hub_bit(&mut self, address: u8, port: u8, on: bool) {
        if let Some(hub) = self.devices.iter_mut().find(|d| d.address == address) {
            if on {
                hub.hub_connected |= 1 << port;
            } else {
                hub.hub_connected &= !(1 << port);
            }
        }
    }

    /// Сбросить порт хаба и узнать скорость устройства на нём.
    ///
    /// # Safety
    ///
    /// Хаб сконфигурирован.
    unsafe fn reset_hub_port(&mut self, hub: Pipe, port: u8) -> Result<Speed, EhciError> {
        // SAFETY: контракт функции.
        unsafe { self.hub_port_feature(hub, port, true, FEATURE_PORT_RESET) }?;
        let mut timeout = Timeout::new(PORT_RESET_TIMEOUT_MS);
        loop {
            sleep_ms(10);
            // SAFETY: см. выше.
            let (status, change) = unsafe { self.hub_port_status(hub, port) }?;
            if change & HUB_CHANGE_RESET != 0 || (status & HUB_PORT_RESET == 0 && status & HUB_PORT_ENABLE != 0) {
                break;
            }
            if timeout.expired() {
                return Err(EhciError::PortResetTimeout);
            }
        }
        // SAFETY: см. выше.
        let _ = unsafe { self.hub_port_feature(hub, port, false, FEATURE_C_PORT_RESET) };
        sleep_ms(PORT_RECOVERY_MS);
        // SAFETY: см. выше.
        let (status, _) = unsafe { self.hub_port_status(hub, port) }?;
        if status & HUB_PORT_ENABLE == 0 {
            return Err(EhciError::PortNotEnabled);
        }
        Ok(if status & HUB_PORT_LOW_SPEED != 0 {
            Speed::Low
        } else if status & HUB_PORT_HIGH_SPEED != 0 {
            Speed::High
        } else {
            Speed::Full
        })
    }

    /// Выбрать конфигурацию, прочитать дескриптор отчётов и договориться о
    /// протоколе. Тот же порядок, что у OHCI и xHCI.
    ///
    /// # Safety
    ///
    /// Устройство адресовано.
    unsafe fn enable_reports(&mut self, device: &mut Device, found: &HidInterface) -> Result<Reader, EhciError> {
        let pipe = device.pipe();
        // SAFETY: контракт функции.
        let (reader, described_by) = unsafe { self.enable_reports_on(pipe, device.place, found) }?;
        device.described_by = described_by;
        Ok(reader)
    }

    /// То же, но по чужой управляющей точке и без записи в устройство.
    ///
    /// Нужно составным устройствам: интерфейсов HID у них несколько, а
    /// управляющая точка одна. Второй и следующие интерфейсы настраиваются
    /// через кольцо первого — своего у них нет и быть не может (см.
    /// [`Device::secondary`]).
    ///
    /// Возвращает разборщик и длину дескриптора отчётов, которой он разобран
    /// (ноль — разобрать не удалось и работает boot-протокол).
    ///
    /// # Safety
    ///
    /// Устройство адресовано, `pipe` — его управляющая точка.
    unsafe fn enable_reports_on(
        &mut self,
        pipe: Pipe,
        place: Place,
        found: &HidInterface,
    ) -> Result<(Reader, u16), EhciError> {
        // SAFETY: контракт функции.
        unsafe {
            self.control_transfer(pipe, [0, usb::REQ_SET_CONFIGURATION, found.configuration, 0, 0, 0, 0, 0], 0, false)
        }?;

        // SAFETY: устройство сконфигурировано.
        let described = unsafe { self.read_report_descriptor(pipe, place, found) };
        let described_by = if described.keyboard.is_some() || described.pointer.is_some() {
            found.report_len
        } else {
            0
        };
        let (reader, boot) = choose_reader(found, &described).ok_or(EhciError::UnknownHid)?;

        let request = usb::REQ_TYPE_CLASS | usb::REQ_RECIPIENT_INTERFACE;
        if found.boot {
            let wanted = if boot { usb::HID_PROTOCOL_BOOT } else { usb::HID_PROTOCOL_REPORT };
            // SAFETY: см. выше.
            let protocol = unsafe {
                self.control_transfer(
                    pipe,
                    [request, usb::REQ_HID_SET_PROTOCOL, wanted as u8, 0, found.interface, 0, 0, 0],
                    0,
                    false,
                )
            };
            if protocol.is_err() {
                kprintln!(
                    "  ehci        : SET_PROTOCOL({}) refused; assuming the device is in it anyway",
                    if boot { "boot" } else { "report" }
                );
            }
        }
        // SAFETY: см. выше.
        let idle = unsafe {
            self.control_transfer(pipe, [request, usb::REQ_HID_SET_IDLE, 0, 0, found.interface, 0, 0, 0], 0, false)
        };
        if idle.is_err() {
            kprintln!("  ehci        : SET_IDLE refused; reports may repeat");
        }
        Ok((reader, described_by))
    }

    /// Прочитать и разобрать дескриптор отчётов. Неудача — не отказ: у
    /// boot-устройства остаётся запасной формат.
    ///
    /// # Safety
    ///
    /// Устройство сконфигурировано.
    unsafe fn read_report_descriptor(&mut self, pipe: Pipe, place: Place, found: &HidInterface) -> usb_hid::Descriptor {
        if found.report_len == 0 {
            kprintln!("  ehci        : {place}: the interface declares no report descriptor");
            return usb_hid::Descriptor::default();
        }
        let length = found.report_len.min(TRANSFER_DATA_MAX as u16);
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
        let read = match unsafe { self.control_transfer(pipe, setup, length, true) } {
            Ok(read) => read,
            Err(err) => {
                kprintln!("  ehci        : {place}: the report descriptor could not be read: {err}");
                return usb_hid::Descriptor::default();
            }
        };
        let parsed = usb_hid::parse(self.transfer_bytes(read));
        match parsed.pointer {
            Some(map) if map.is_absolute() => {
                let (min, max) = map.range();
                kprintln!(
                    "  ehci        : {place}: report descriptor {read} bytes: pointer, absolute {min}..{max}, {} buttons{}",
                    map.button_count(),
                    if map.has_wheel() { ", wheel" } else { "" }
                );
            }
            Some(map) => kprintln!(
                "  ehci        : {place}: report descriptor {read} bytes: pointer, relative, {} buttons{}",
                map.button_count(),
                if map.has_wheel() { ", wheel" } else { "" }
            ),
            None => {}
        }
        if let Some(map) = parsed.keyboard {
            kprintln!(
                "  ehci        : {place}: report descriptor {read} bytes: keyboard, {}, {}-key array",
                if map.has_modifiers() { "modifiers" } else { "no modifiers" },
                map.key_slots()
            );
        }
        if parsed.pointer.is_none() && parsed.keyboard.is_none() {
            kprintln!("  ehci        : {place}: report descriptor {read} bytes: nothing the kernel can use");
        }
        parsed
    }

    /// Прочитать стандартный дескриптор устройства.
    ///
    /// # Safety
    ///
    /// QH устройства в кольце.
    unsafe fn get_descriptor(&mut self, pipe: Pipe, kind: u8, index: u8, length: u16) -> Result<usize, EhciError> {
        let setup = [usb::REQ_DIR_IN, usb::REQ_GET_DESCRIPTOR, index, kind, 0, 0, length as u8, (length >> 8) as u8];
        // SAFETY: контракт функции.
        unsafe { self.control_transfer(pipe, setup, length, true) }
    }

    /// Первые `length` байт данных буфера управляющих передач.
    fn transfer_bytes(&self, length: usize) -> &[u8] {
        let length = length.min(TRANSFER_DATA_MAX);
        // SAFETY: буфер выделен на страницу, данные лежат с `TRANSFER_DATA`.
        unsafe { core::slice::from_raw_parts(self.transfer.as_ptr::<u8>().add(TRANSFER_DATA), length) }
    }

    /// Заполнить qTD.
    fn fill_td(&self, pipe: Pipe, at: usize, next: u32, alt: u32, token: u32, buffer: u32) {
        pipe.write(at + TD_NEXT, next);
        pipe.write(at + TD_ALT, alt);
        pipe.write(at + TD_BUFFER, buffer);
        for page in 1..5 {
            pipe.write(at + TD_BUFFER + page * 4, 0);
        }
        pipe.write(at + TD_BUFFER_HIGH, if buffer == 0 { 0 } else { self.segment });
        // Слово состояния — последним: пока оно не активно, контроллер qTD не
        // исполняет, и порядок остальных записей неважен.
        pipe.write(at + TD_TOKEN, token);
    }

    /// Выполнить передачу по управляющей точке. Возвращает число полученных
    /// байт; данные лежат в буфере управляющих передач.
    ///
    /// # Safety
    ///
    /// QH устройства стоит в асинхронном кольце и простаивает.
    unsafe fn control_transfer(&mut self, pipe: Pipe, setup: [u8; 8], length: u16, is_in: bool) -> Result<usize, EhciError> {
        let length = length.min(TRANSFER_DATA_MAX as u16);
        // SAFETY: буфер выделен на страницу.
        unsafe {
            let ptr = self.transfer.as_ptr::<u8>();
            for (offset, byte) in setup.iter().enumerate() {
                ptr.add(offset).write_volatile(*byte);
            }
            if !is_in {
                for offset in 0..usize::from(length) {
                    ptr.add(TRANSFER_DATA + offset).write_volatile(0);
                }
            }
        }
        let setup_phys = self.transfer.phys().as_u64() as u32;
        let data_phys = setup_phys + TRANSFER_DATA as u32;

        let status_pid = if is_in && length > 0 { TOKEN_PID_OUT } else { TOKEN_PID_IN };
        let status_td = pipe.at(PAGE_TD_STATUS);
        let after_setup = if length > 0 { pipe.at(PAGE_TD_DATA) } else { status_td };

        self.fill_td(
            pipe,
            PAGE_TD_SETUP,
            after_setup,
            LINK_TERMINATE,
            TOKEN_ACTIVE | TOKEN_PID_SETUP | TOKEN_RETRIES | (8 << TOKEN_LENGTH_SHIFT),
            setup_phys,
        );
        if length > 0 {
            let direction = if is_in { TOKEN_PID_IN } else { TOKEN_PID_OUT };
            // Короткий ответ на чтение — не ошибка: контроллер уходит по
            // альтернативному указателю прямо к стадии состояния.
            let alt = if is_in { status_td } else { LINK_TERMINATE };
            self.fill_td(
                pipe,
                PAGE_TD_DATA,
                status_td,
                alt,
                TOKEN_ACTIVE | direction | TOKEN_RETRIES | TOKEN_TOGGLE | (u32::from(length) << TOKEN_LENGTH_SHIFT),
                data_phys,
            );
        }
        self.fill_td(
            pipe,
            PAGE_TD_STATUS,
            LINK_TERMINATE,
            LINK_TERMINATE,
            TOKEN_ACTIVE | status_pid | TOKEN_RETRIES | TOKEN_TOGGLE,
            0,
        );

        // QH простаивает: окно не активно. Адрес устройства и размер пакета
        // могли измениться после `SET_ADDRESS` и первого дескриптора — Linux
        // правит их у простаивающего QH так же. Передача начинается последней
        // записью — адресом первого qTD в окне.
        let (info1, info2) = pipe.control_info();
        pipe.write(PAGE_CONTROL_QH + QH_INFO1, info1);
        pipe.write(PAGE_CONTROL_QH + QH_INFO2, info2);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_TOKEN, 0);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_ALT, LINK_TERMINATE);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_NEXT, pipe.at(PAGE_TD_SETUP));

        let mut timeout = Timeout::new(TRANSFER_TIMEOUT_MS);
        let result = loop {
            let tokens = [
                pipe.read(PAGE_TD_SETUP + TD_TOKEN),
                if length > 0 { pipe.read(PAGE_TD_DATA + TD_TOKEN) } else { 0 },
                pipe.read(PAGE_TD_STATUS + TD_TOKEN),
            ];
            if let Some(halted) = tokens.iter().find(|token| *token & TOKEN_HALTED != 0) {
                break Err(EhciError::Transfer(*halted));
            }
            if tokens[2] & TOKEN_ACTIVE == 0 {
                break Ok(());
            }
            if timeout.expired() {
                let (waited_ms, spun_out) = timeout.report();
                break Err(EhciError::TransferTimeout { waited_ms, spun_out });
            }
        };

        if result.is_err() {
            self.errors += 1;
            // Остановить окно: иначе контроллер продолжит несостоявшуюся
            // передачу, а её qTD уже будут переписаны под следующую.
            pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_TOKEN, TOKEN_HALTED);
            for at in [PAGE_TD_SETUP, PAGE_TD_DATA, PAGE_TD_STATUS] {
                pipe.write(at + TD_TOKEN, 0);
            }
            // Кадр, чтобы контроллер точно ушёл с этого QH.
            sleep_ms(2);
        }
        result?;

        if length == 0 {
            return Ok(0);
        }
        let left = (pipe.read(PAGE_TD_DATA + TD_TOKEN) >> TOKEN_LENGTH_SHIFT) & TOKEN_LENGTH_MASK;
        Ok(usize::from(length).saturating_sub(left as usize))
    }

    /// Поставить QH управляющей точки нового устройства в асинхронное кольцо,
    /// сразу за головой.
    ///
    /// # Safety
    ///
    /// Страница устройства выделена и обнулена.
    unsafe fn open_control(&self, device: &Device) {
        let pipe = device.pipe();
        let (info1, info2) = pipe.control_info();
        pipe.write(PAGE_CONTROL_QH + QH_INFO1, info1);
        pipe.write(PAGE_CONTROL_QH + QH_INFO2, info2);
        pipe.write(PAGE_CONTROL_QH + QH_CURRENT, 0);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_NEXT, LINK_TERMINATE);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_ALT, LINK_TERMINATE);
        pipe.write(PAGE_CONTROL_QH + QH_OVERLAY_TOKEN, 0);
        let head = self.head_pipe();
        // Сначала свой указатель «дальше», потом — голова на нас: в каждый миг
        // кольцо замкнуто.
        pipe.write(PAGE_CONTROL_QH + QH_NEXT, head.read(QH_NEXT));
        head.write(QH_NEXT, pipe.at(PAGE_CONTROL_QH) | LINK_QH);
    }

    /// Завести точку прерываний устройства и запросить первый отчёт.
    ///
    /// # Safety
    ///
    /// Устройство сконфигурировано.
    unsafe fn open_interrupt(&self, device: &Device, found: &HidInterface) {
        let pipe = device.pipe();
        let (info1, info2) = pipe.interrupt_info(found.endpoint, device.report_len);
        pipe.write(PAGE_INTERRUPT_QH + QH_NEXT, LINK_TERMINATE);
        pipe.write(PAGE_INTERRUPT_QH + QH_INFO1, info1);
        pipe.write(PAGE_INTERRUPT_QH + QH_INFO2, info2);
        pipe.write(PAGE_INTERRUPT_QH + QH_CURRENT, 0);
        pipe.write(PAGE_INTERRUPT_QH + QH_OVERLAY_ALT, LINK_TERMINATE);
        pipe.write(PAGE_INTERRUPT_QH + QH_OVERLAY_TOKEN, 0);
        self.queue_report(device);
    }

    /// Запросить отчёт: заполнить qTD и вписать его в окно простаивающего QH.
    fn queue_report(&self, device: &Device) {
        let pipe = device.pipe();
        self.fill_td(
            pipe,
            PAGE_TD_REPORT,
            LINK_TERMINATE,
            LINK_TERMINATE,
            TOKEN_ACTIVE
                | TOKEN_PID_IN
                | TOKEN_RETRIES
                | report_interrupt()
                | (u32::from(device.report_len) << TOKEN_LENGTH_SHIFT),
            pipe.at(PAGE_REPORT),
        );
        pipe.write(PAGE_INTERRUPT_QH + QH_OVERLAY_NEXT, pipe.at(PAGE_TD_REPORT));
    }

    /// Пересобрать периодическую цепочку: все кадры ведут на QH точек
    /// прерываний поднятых устройств.
    ///
    /// # Safety
    ///
    /// Страницы всех устройств в списке существуют.
    unsafe fn relink_periodic(&self) {
        let listed: Vec<Pipe> = self.devices.iter().filter(|d| d.reader.is_some()).map(Device::pipe).collect();
        for (index, pipe) in listed.iter().enumerate() {
            let next = listed.get(index + 1).map_or(LINK_TERMINATE, |next| next.at(PAGE_INTERRUPT_QH) | LINK_QH);
            pipe.write(PAGE_INTERRUPT_QH + QH_NEXT, next);
        }
        let first = listed.first().map_or(LINK_TERMINATE, |pipe| pipe.at(PAGE_INTERRUPT_QH) | LINK_QH);
        // SAFETY: страница таблицы выделена под 1024 указателя.
        unsafe {
            let table = self.frame_list.as_ptr::<u32>();
            for frame in 0..FRAMES {
                table.add(frame).write_volatile(first);
            }
        }
    }

    /// Пересобрать асинхронное кольцо из QH устройств в списке.
    fn relink_async(&self) {
        let head = self.head_pipe();
        let mut next = head.phys | LINK_QH;
        // Спутники пропускаются: управляющая точка у устройства одна, и кольцо
        // для неё держит первая запись (см. [`Device::secondary`]).
        for device in self.devices.iter().rev().filter(|device| !device.secondary) {
            let pipe = device.pipe();
            pipe.write(PAGE_CONTROL_QH + QH_NEXT, next);
            next = pipe.at(PAGE_CONTROL_QH) | LINK_QH;
        }
        head.write(QH_NEXT, next);
    }

    /// Дождаться, пока контроллер отпустит изъятые из асинхронного кольца QH.
    fn async_advance(&self) {
        self.write(OP_USBSTS, STS_ASYNC_ADVANCE);
        self.write(OP_USBCMD, self.read(OP_USBCMD) | CMD_ASYNC_DOORBELL);
        let mut timeout = Timeout::new(SCHEDULE_TIMEOUT_MS);
        while self.read(OP_USBSTS) & STS_ASYNC_ADVANCE == 0 {
            if timeout.expired() {
                // Без ответа ждать дольше нечего: пауза в несколько кадров
                // покрывает то же самое на исправном контроллере.
                sleep_ms(20);
                return;
            }
        }
        self.write(OP_USBSTS, STS_ASYNC_ADVANCE);
    }

    /// Изъять из списка устройства с этими адресами и всё, что висит на них.
    fn take_subtree(&mut self, mut victims: Vec<u8>) -> Vec<Device> {
        let mut taken = Vec::new();
        while let Some(address) = victims.pop() {
            // Записей с одним адресом может быть несколько: составное устройство
            // — это один адрес и по записи на каждый его интерфейс HID. Забрать
            // одну и оставить остальные значило бы оставить в периодическом
            // списке кольцо, чья страница вот-вот будет освобождена.
            let mut removed = false;
            while let Some(index) = self.devices.iter().position(|d| d.address == address) {
                let device = self.devices.remove(index);
                taken.push(device);
                removed = true;
            }
            if removed {
                victims.extend(self.devices.iter().filter(|d| d.place.parent == address).map(|d| d.address));
            }
        }
        taken
    }

    /// Отцепить изъятые устройства от обоих списков и освободить их страницы.
    ///
    /// # Safety
    ///
    /// Устройства уже не в `self.devices`.
    unsafe fn retire(&mut self, taken: Vec<Device>, announce: bool) {
        if taken.is_empty() {
            return;
        }
        // SAFETY: оставшиеся устройства существуют.
        unsafe { self.relink_periodic() };
        self.relink_async();
        self.async_advance();
        // Периодический список контроллер читает раз в кадр — два кадра запаса.
        sleep_ms(2);
        for device in &taken {
            if announce {
                kprintln!("  ehci        : {} is empty now", device.place);
            }
            // SAFETY: страница больше не достижима ни из одного списка.
            unsafe { dma::free(&device.page) };
        }
    }

    /// Какие корневые порты заняты устройствами этого контроллера.
    fn connected_mask(&self) -> u32 {
        let mut mask = 0;
        for index in 0..self.ports {
            let status = self.port(index);
            if status & PORT_CCS != 0 && status & PORT_OWNER == 0 {
                mask |= 1 << index;
            }
        }
        mask
    }

    fn has_hubs(&self) -> bool {
        self.devices.iter().any(|d| d.hub_ports > 0)
    }

    /// Сверить порты — корневые и хабов — и поднять или забыть устройства.
    ///
    /// # Safety
    ///
    /// Контроллер работает; вызов из задачи.
    unsafe fn rescan(&mut self) -> bool {
        let mut changed = false;
        // Устройства, чья точка останавливается раз за разом: забыть и поднять
        // заново, как после переподключения.
        for address in core::mem::take(&mut self.recover) {
            let Some(place) = self.devices.iter().find(|d| d.address == address).map(|d| d.place) else {
                continue;
            };
            kprintln!("  ehci        : {place}: bringing the device up again, as if it were plugged back in");
            let taken = self.take_subtree(alloc::vec![address]);
            // SAFETY: устройства изъяты из списка.
            unsafe { self.retire(taken, false) };
            if place.parent == 0 {
                // SAFETY: контракт функции.
                unsafe { self.attach_root_port(usize::from(place.root_port - 1)) };
            } else {
                self.set_hub_bit(place.parent, place.hub_port, false);
                // SAFETY: хаб в списке — или уже изъят, тогда сверка молчит.
                unsafe { self.check_hub_port(place.parent, place.hub_port) };
            }
            changed = true;
        }
        // Порт, отданный спутнику, драйвер назад не забирает. У такого порта
        // EHCI всегда видит `CCS = 0`, и «вернуть опустевший» означало бы
        // отбирать устройство у спутника на каждой сверке. Спецификация
        // (4.2.2) возвращает порт этому контроллеру сама — когда устройство
        // отключают.
        let mask = self.connected_mask();
        for index in 0..self.ports {
            let bit = 1 << index;
            let root_port = (index + 1) as u8;
            let replugged = self.port(index) & PORT_CSC != 0 && mask & bit != 0 && self.connected & bit != 0;
            if self.connected & bit != 0 && (mask & bit == 0 || replugged) {
                let victims: Vec<u8> = self
                    .devices
                    .iter()
                    .filter(|d| d.place.root_port == root_port && d.place.parent == 0)
                    .map(|d| d.address)
                    .collect();
                let taken = self.take_subtree(victims);
                // SAFETY: устройства изъяты.
                unsafe { self.retire(taken, true) };
                changed = true;
            }
            if mask & bit != 0 && (self.connected & bit == 0 || replugged) {
                sleep_ms(DEBOUNCE_MS);
                // SAFETY: контракт функции.
                unsafe { self.attach_root_port(index) };
                changed = true;
            }
        }
        self.connected = self.connected_mask();

        let hubs: Vec<(u8, u8)> = self.devices.iter().filter(|d| d.hub_ports > 0).map(|d| (d.address, d.hub_ports)).collect();
        for (address, ports) in hubs {
            for port in 1..=ports {
                // SAFETY: хаб в списке (или уже изъят — тогда сверка молчит).
                changed |= unsafe { self.check_hub_port(address, port) };
            }
        }
        changed
    }

    /// Забрать пришедшие отчёты.
    fn service(&mut self) {
        self.services += 1;
        let status = self.read(OP_USBSTS);
        if status & STS_HOST_ERROR != 0 && !self.unrecoverable {
            kprintln!("  ehci        : {} reported a host system error", self.pci);
            self.unrecoverable = true;
        }
        let mut errors = 0;
        let mut recover = Vec::new();
        for device in &mut self.devices {
            if device.reader.is_none() {
                continue;
            }
            let pipe = device.pipe();
            let token = pipe.read(PAGE_TD_REPORT + TD_TOKEN);
            if token & TOKEN_ACTIVE != 0 {
                continue;
            }
            if token & TOKEN_HALTED != 0 {
                // Точка остановлена ошибкой: снять остановку с окна и запросить
                // отчёт заново. Молча терять клавиатуру из-за одного испорченного
                // пакета — хуже.
                errors += 1;
                device.halts += 1;
                // Первая остановка и та, после которой устройство поднимают
                // заново, — в журнал: ноутбук, где клавиатура гасла, пишет
                // только на экран, и `dmesg` после переподключения — единственный
                // способ узнать, что было. Каждую остановку печатать нельзя: при
                // STALL они идут раз в десять миллисекунд.
                if device.halts == 1 || device.halts == RECOVER_AFTER {
                    kprintln!(
                        "  ehci        : {}: the report endpoint halted ({} in a row): {} (token {token:#010x})",
                        device.place,
                        device.halts,
                        token_name(token)
                    );
                }
                if device.halts == RECOVER_AFTER {
                    recover.push(device.address);
                }
                pipe.write(PAGE_INTERRUPT_QH + QH_OVERLAY_TOKEN, 0);
            } else {
                if device.halts > 1 {
                    kprintln!("  ehci        : {}: reports again after {} halt(s)", device.place, device.halts);
                }
                device.halts = 0;
                let left = (token >> TOKEN_LENGTH_SHIFT) & TOKEN_LENGTH_MASK;
                let length = u32::from(device.report_len).saturating_sub(left) as usize;
                if length > 0 {
                    // SAFETY: буфер отчёта внутри страницы, длина не больше
                    // запрошенной.
                    let report = unsafe {
                        core::slice::from_raw_parts((pipe.virt + PAGE_REPORT) as *const u8, length)
                    };
                    if let Some(reader) = device.reader.as_mut() {
                        reader.handle_report(report);
                    }
                }
            }
            let pipe = device.pipe();
            let report_len = device.report_len;
            let segment = self.segment;
            // `queue_report` без заимствования контроллера: цикл держит его
            // устройства изменяемыми.
            //
            // Это **вторая копия** одного и того же заполнения, и она уже
            // однажды солгала: бит «прервать по завершении» поставили в
            // `queue_report`, а сюда забыли — и прерывания не приходили вовсе,
            // хотя всё остальное было настроено верно. Первый дескриптор
            // ставится при перечислении, все следующие — здесь, то есть именно
            // здесь и решается, будет ли работать прерывание.
            pipe.write(PAGE_TD_REPORT + TD_NEXT, LINK_TERMINATE);
            pipe.write(PAGE_TD_REPORT + TD_ALT, LINK_TERMINATE);
            pipe.write(PAGE_TD_REPORT + TD_BUFFER, pipe.at(PAGE_REPORT));
            pipe.write(PAGE_TD_REPORT + TD_BUFFER_HIGH, segment);
            pipe.write(
                PAGE_TD_REPORT + TD_TOKEN,
                TOKEN_ACTIVE
                    | TOKEN_PID_IN
                    | TOKEN_RETRIES
                    | report_interrupt()
                    | (u32::from(report_len) << TOKEN_LENGTH_SHIFT),
            );
            pipe.write(PAGE_INTERRUPT_QH + QH_OVERLAY_NEXT, pipe.at(PAGE_TD_REPORT));
        }
        self.errors += errors;
        for address in recover {
            if !self.recover.contains(&address) {
                self.recover.push(address);
            }
        }
    }

    fn summary_into(&self, summary: &mut Summary) {
        summary.controllers += 1;
        summary.ports += self.ports;
        summary.occupied += self.connected.count_ones() as usize;
        summary.hubs += self.devices.iter().filter(|d| d.hub_ports > 0).count();
        for device in self.devices.iter().filter(|d| d.reader.is_some()) {
            summary.devices += 1;
            match device.reader {
                Some(Reader::Keyboard(_)) => summary.keyboards += 1,
                Some(Reader::Mouse(_)) => summary.mice += 1,
                None => {}
            }
            summary.reports += device.reader.as_ref().map_or(0, Reader::reports);
            if let Some(slot) = summary.attached.iter_mut().find(|slot| slot.port == 0) {
                *slot = Attached {
                    port: device.place.root_port,
                    vendor: device.identity.0,
                    product: device.identity.1,
                    kind: device.reader.as_ref().map_or("unknown", Reader::name),
                    descriptor: device.described_by,
                    interface: device.interface.0,
                    interfaces: device.interface.1,
                };
            }
        }
        summary.errors += self.errors;
        summary.services += self.services;
        if self.last_error.is_some() {
            summary.last_error = self.last_error;
        }
        summary.unrecoverable |= self.unrecoverable;
    }

    fn has(&self, protocol: u8) -> bool {
        self.devices.iter().any(|d| d.reader.as_ref().is_some_and(|r| r.protocol() == protocol))
    }
}

/// Забрать контроллер у прошивки (USB Legacy Support, EHCI 1.0, 5.1).
///
/// Прошивка с поддержкой USB-клавиатуры владеет контроллером через SMM: она же
/// изображает клавиатуру PS/2 для меню загрузки. Возможность лежит не в окне
/// регистров, а в конфигурационном пространстве PCI — по смещению `EECP`. Порядок
/// тот же, что у Linux (`ehci_bios_handoff`): попросить, подождать секунду, при
/// отказе забрать силой и в любом случае выключить SMI.
///
/// # Safety
///
/// Конфигурационное пространство устройства отображено; вызывать до остановки
/// контроллера.
unsafe fn take_from_firmware(device: &pci::Device, eecp: usize) {
    let mut at = eecp;
    for _ in 0..EXT_CAP_LIMIT {
        // Указатель внутрь стандартного заголовка невозможен: там регистры PCI.
        if at < 0x40 || at + 8 > 0x100 {
            return;
        }
        if device.config8(at) == LEGSUP_ID {
            break;
        }
        at = usize::from(device.config8(at + 1));
    }
    if at < 0x40 || device.config8(at) != LEGSUP_ID {
        return;
    }

    let value = device.config32(at);
    if value & LEGSUP_BIOS_OWNED != 0 {
        // SAFETY: байт `OS Owned` возможности USB Legacy Support.
        unsafe { device.write_config8(at + LEGSUP_OS_BYTE, 1) };
        let mut timeout = Timeout::new(HANDOFF_TIMEOUT_MS);
        loop {
            if device.config32(at) & LEGSUP_BIOS_OWNED == 0 {
                kprintln!("  ehci        : the firmware handed the controller over");
                break;
            }
            if timeout.expired() {
                // SAFETY: байт `BIOS Owned` той же возможности.
                unsafe { device.write_config8(at + LEGSUP_BIOS_BYTE, 0) };
                kprintln!("  ehci        : the firmware kept the controller for {HANDOFF_TIMEOUT_MS} ms; taken anyway");
                break;
            }
            core::hint::spin_loop();
        }
    }
    // SAFETY: `USBLEGCTLSTS` той же возможности; ноль выключает все SMI.
    unsafe { device.write_config32(at + LEGCTLSTS, 0) };
}

/// Отобразить окно регистров контроллера. Регистры EHCI укладываются в
/// страницу.
///
/// # Safety
///
/// Ядро исполняется на собственных таблицах страниц.
unsafe fn map_bar(bar: PhysAddr) -> Result<usize, MapError> {
    let page = bar.page_align_down();
    let virt = page.to_direct_map();
    let flags = crate::mm::PageFlags::READ | crate::mm::PageFlags::WRITE | crate::mm::PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему; прямое отображение взаимно
    // однозначно.
    unsafe { crate::arch::map_active(virt, page, PAGE_SIZE, flags) }?;
    Ok(virt.as_usize() + (bar.as_u64() - page.as_u64()) as usize)
}

// ---------------------------------------------------------------------------
// Сводка и точки входа
// ---------------------------------------------------------------------------

/// Что драйвер сообщает о себе наружу — по всем контроллерам сразу.
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    pub controllers: usize,
    /// Сколько устройств ввода поднято (хабы не считаются).
    pub devices: usize,
    pub keyboards: usize,
    pub mice: usize,
    pub hubs: usize,
    /// Корневых портов всего и сколько из них заняты устройствами этого драйвера.
    pub ports: usize,
    pub occupied: usize,
    pub attached: [Attached; ATTACHED_MAX],
    pub reports: u64,
    pub errors: u64,
    pub services: u64,
    pub last_error: Option<(u8, Stage, EhciError)>,
    pub unrecoverable: bool,
}

/// Все контроллеры EHCI машины. У ноутбука на Cougar Point их два.
static CONTROLLERS: crate::sync::SpinLock<Vec<Controller>> = crate::sync::SpinLock::new(Vec::new());

/// Program Interface EHCI.
const PROG_IF_EHCI: u8 = 0x20;

/// Поднять все контроллеры и всё, что на них висит. `true`, если ввод появился.
///
/// # Safety
///
/// Ядро на собственных таблицах, прерывания разрешены, таблицы ACPI целы, ни
/// одного [`crate::sync::SpinLock`] не удерживается.
pub unsafe fn init(rsdp: u64) -> bool {
    // SAFETY: контракт функции.
    let root = match unsafe { pci::Root::discover(rsdp) } {
        Ok(root) => root,
        Err(_) => return false,
    };
    let mut found: Vec<pci::Device> = Vec::new();
    // SAFETY: контракт функции.
    unsafe {
        pci::for_each(&root, |device| {
            if device.class == pci::CLASS_SERIAL_BUS
                && device.subclass == pci::SUBCLASS_USB
                && device.prog_if == PROG_IF_EHCI
            {
                found.push(*device);
            }
            true
        });
    }
    // Контроллеров нет — молчание: перепись уже перечислила всё, что на шине.
    if found.is_empty() {
        return false;
    }

    let mut controllers = Vec::new();
    for device in &found {
        // SAFETY: контракт функции.
        match unsafe { Controller::init(device) } {
            Ok(mut controller) => {
                // Прерывания просятся **до** перечисления, и это не порядок
                // ради порядка. Первый дескриптор запроса отчёта ставится
                // именно при перечислении, а бит «прервать по завершении»
                // берётся из признака в момент заполнения. Попроси мы
                // прерывания позже — первый отчёт пришёл бы без сигнала, и
                // заметить это можно было бы только по сроку ожидания,
                // который его подберёт.
                enable_interrupts(controllers.len(), device, &controller);
                // SAFETY: контроллер работает.
                unsafe { controller.attach_devices() };
                let mut summary = Summary::default();
                controller.summary_into(&mut summary);
                kprintln!(
                    "  ehci        : {}: {} device(s) on {} of {} port(s), {} hub(s), {} keyboard(s), {} pointer(s)",
                    device.address,
                    summary.devices,
                    summary.occupied,
                    summary.ports,
                    summary.hubs,
                    summary.keyboards,
                    summary.mice
                );
                controllers.push(controller);
            }
            Err(err) => kprintln!("  ehci        : {} unavailable: {err}", device.address),
        }
    }

    let keyboard = controllers.iter().any(|c| c.has(usb::PROTOCOL_KEYBOARD));
    let mouse = controllers.iter().any(|c| c.has(usb::PROTOCOL_MOUSE));
    let sources = input::sources();
    input::set_sources(input::Sources {
        keyboard: sources.keyboard || keyboard,
        mouse: sources.mouse || mouse,
        ..sources
    });
    let any = !controllers.is_empty();
    *CONTROLLERS.lock() = controllers;
    any && (keyboard || mouse)
}

/// Поднялся ли хоть один контроллер.
#[must_use]
pub fn is_present() -> bool {
    !CONTROLLERS.lock().is_empty()
}

/// Как часто забирать отчёты, когда устройства есть.
const POLL_PERIOD_MS: u64 = 10;
/// Как часто сверять порты, когда устройств ввода нет.
const IDLE_PERIOD_MS: u64 = 500;
/// Как часто сверять порты при работающих устройствах.
const PORT_CHECK_PERIOD_MS: u64 = 500;

/// После скольких остановок точки прерываний подряд устройство поднимают заново.
///
/// Одна-две — испорченный пакет, и повторный запрос лечит их сам. Устройство,
/// чья точка останавливается раз за разом, повтором не вернуть: на ноутбуке
/// Романа клавиатура гасла при входе в стол и оживала только переподключением —
/// то есть сбросом порта и новым перечислением. Здесь делается то же самое.
const RECOVER_AFTER: u32 = 8;

/// Попросить прерывания у одного поднятого контроллера.
///
/// Отказ не ошибка: контроллер без известной линии остаётся на опросе и
/// работает ровно как раньше. Сказать об этом надо вслух — «USB работает
/// медленнее, чем мог бы» человек должен видеть, а не угадывать.
fn enable_interrupts(index: usize, device: &pci::Device, controller: &Controller) {
    if index >= MAX_CONTROLLERS {
        kprintln!("  ehci        : {} has no slot for interrupts; reports will be polled for", controller.pci);
        return;
    }
    // Адрес окна кладётся до разрешения: обработчик без него не снимет
    // признак, то есть оставит уровневую линию поднятой навсегда.
    OPERATIONAL[index].store(controller.op, core::sync::atomic::Ordering::Relaxed);
    let Some(gsi) = crate::irq::routing::request(device, on_interrupt) else {
        OPERATIONAL[index].store(0, core::sync::atomic::Ordering::Relaxed);
        kprintln!("  ehci        : {} has no interrupt line known; reports will be polled for", controller.pci);
        return;
    };
    // Накопленное снимается до разрешения: иначе первое же прерывание придёт за
    // событие, которого мы не видели.
    controller.write(OP_USBSTS, INTERRUPT_CAUSES);
    controller.write(OP_USBINTR, INTERRUPT_CAUSES);
    // Признак один на всех контроллеров: дескрипторы просят прерывания только
    // когда есть кому их принять, а принимает один обработчик.
    WANT_INTERRUPTS.store(true, core::sync::atomic::Ordering::Relaxed);
    kprintln!("  ehci        : {} INTx on GSI {gsi}, completed transfers arrive by interrupt", controller.pci);
}

/// Тело задачи, обслуживающей контроллеры.
pub fn service_task() {
    let mut next_port_check = 0u64;
    loop {
        let (devices, check, now) = {
            let mut guard = CONTROLLERS.lock();
            if guard.is_empty() {
                return;
            }
            for controller in guard.iter_mut() {
                controller.service();
            }
            let devices: usize = guard.iter().map(|c| c.devices.iter().filter(|d| d.reader.is_some()).count()).sum();
            let check = guard.iter().any(|c| c.connected != c.connected_mask() || c.has_hubs() || !c.recover.is_empty());
            (devices, check, crate::time::uptime_ms())
        };

        let recovering = CONTROLLERS.lock().iter().any(|c| !c.recover.is_empty());
        if now >= next_port_check || recovering {
            next_port_check = now.saturating_add(PORT_CHECK_PERIOD_MS);
            if check {
                poll_hotplug();
            }
        }
        // Срок остаётся и на пути с прерыванием: подключение устройства в порт
        // прерыванием не сопровождается — его находит сверка портов, а её
        // заводит именно срок.
        if WANT_INTERRUPTS.load(core::sync::atomic::Ordering::Relaxed) {
            // С прерываниями срок перестаёт быть опросом и становится тем, чем
            // он и должен быть: часами сверки портов. Отчёт будит сам, а
            // подключение устройства в порт прерыванием не сопровождается — его
            // ищет сверка, и чаще, чем раз в полсекунды, ей незачем. Оставить
            // здесь десять миллисекунд значило бы получить прерывания и всё
            // равно просыпаться сто раз в секунду.
            crate::sched::block_on_irq_until(IRQ_SOURCE, PORT_CHECK_PERIOD_MS, || false);
        } else {
            crate::sched::sleep_ms(if devices == 0 { IDLE_PERIOD_MS } else { POLL_PERIOD_MS });
        }
    }
}

/// Сверить порты всех контроллеров.
///
/// Контроллеры забираются из глобала целиком: сверка хабов — это управляющие
/// передачи с ожиданиями по часам, а [`crate::sync::SpinLock`] держится с
/// запрещёнными прерываниями.
fn poll_hotplug() {
    let mut controllers = core::mem::take(&mut *CONTROLLERS.lock());
    let mut changed = false;
    for controller in &mut controllers {
        // SAFETY: контроллер работает, вызов из задачи.
        changed |= unsafe { controller.rescan() };
    }
    if changed {
        let mut summary = Summary::default();
        for controller in &controllers {
            controller.summary_into(&mut summary);
        }
        kprintln!(
            "  ehci        : now {} device(s), {} hub(s), {} keyboard(s), {} pointer(s)",
            summary.devices,
            summary.hubs,
            summary.keyboards,
            summary.mice
        );
        let keyboard = controllers.iter().any(|c| c.has(usb::PROTOCOL_KEYBOARD));
        let mouse = controllers.iter().any(|c| c.has(usb::PROTOCOL_MOUSE));
        let sources = input::sources();
        input::set_sources(input::Sources {
            keyboard: sources.keyboard || keyboard,
            mouse: sources.mouse || mouse,
            ..sources
        });
    }
    *CONTROLLERS.lock() = controllers;
}

/// Сводка по всем контроллерам.
#[must_use]
pub fn summary() -> Option<Summary> {
    let guard = CONTROLLERS.lock();
    if guard.is_empty() {
        return None;
    }
    let mut summary = Summary::default();
    for controller in guard.iter() {
        controller.summary_into(&mut summary);
    }
    Some(summary)
}
