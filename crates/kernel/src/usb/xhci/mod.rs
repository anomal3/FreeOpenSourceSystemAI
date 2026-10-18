//! Драйвер контроллера xHCI: от поиска на шине PCI до отчётов клавиатуры.
//!
//! # Как события доходят до драйвера
//!
//! Прерыванием — с Phase 18; до неё кольцо событий опрашивалось сто раз в
//! секунду. Разница не в задержке (10 мс для клавиатуры незаметны, задержка
//! самого USB того же порядка), а в том, что опрашивающая задача просыпалась
//! независимо от того, происходило ли хоть что-нибудь: за двадцать секунд —
//! две тысячи пробуждений, каждое с переключением контекста и обращениями к
//! регистрам. По прерываниям на том же отрезке их пятьдесят.
//!
//! Путь прерывания от устройства PCIe до обработчика — это MSI-X: устройство
//! не поднимает линию, а **пишет по адресу**, и что произойдёт от этой записи,
//! решает контроллер прерываний. Поэтому арх-часть отвечает ровно на один
//! вопрос — «куда и что писать» ([`crate::arch::interrupts::alloc_msi`]):
//!
//! * на x86-64 адрес опознаёт локальный APIC, а данные несут номер вектора,
//!   уже стоящего в IDT; разбирать `_PRT` из ACPI (то есть писать интерпретатор
//!   AML) не нужно — ради этого MSI-X и выбран вместо INTx;
//! * на AArch64 запись перехватывает приставка GICv2m и превращает её в обычное
//!   SPI. Оно обязано быть объявлено срабатывающим **по фронту**: запись — это
//!   импульс, и уровневое прерывание не доставляется вовсе, молча.
//!
//! Сам драйвер от этого не изменился: обработчик подтверждает прерывание и
//! будит задачу, а разбирает кольцо по-прежнему она — [`Controller::service`],
//! та же самая, что вызывалась по таймеру. Разбор в обработчике означал бы
//! сотни микросекунд с запрещёнными прерываниями, то есть ровно ту болезнь, от
//! которой прерывания лечат.
//!
//! Контроллер без MSI-X (Intel 7-й серии и xHCI VirtualBox, `8086:1e31`)
//! берёт MSI, а без него — линию `INTx` по `_PRT` (фаза 68k). Линия уровневая
//! и бывает общей, поэтому обработчик подтверждает прерывание сам, по
//! адресам регистров, не дожидаясь лока контроллера, и не трогает чужое.
//!
//! Машина, где не сложился ни один путь, остаётся на опросе, и это видно в
//! `usb`: счётчик `irqs` равен нулю. Отказываться от работающей клавиатуры из-за
//! отсутствия прерываний было бы странно.
//!
//! # Сколько устройств
//!
//! Все, что висят на корневых портах и понимают boot-протокол HID: сейчас это
//! клавиатура и мышь, каждая в своём слоте, со своим кольцом точки прерываний и
//! своим разборщиком отчётов. События приходят в одно кольцо на весь
//! контроллер, поэтому передача опознаётся по номеру слота из самого события —
//! иначе отчёт мыши достался бы клавиатуре.
//!
//! Хабы USB 2.0 драйвер проходит с фазы 68l: на настоящем ПК клавиатура и
//! мышь часто висят за хабом — передняя панель корпуса, хаб ноутбука или
//! монитора. Устройству за хабом контроллер нужен путь (route string, по
//! четыре бита на ярус), а медленному устройству за быстрым хабом — ещё и
//! транслятор этого хаба (TT). Об изменениях на своих портах хаб сообщает
//! точкой прерываний, и разбирает их та же задача, что и корневые порты.
//! Хабы USB 3 (свой дескриптор, `SET_HUB_DEPTH`) не проходятся: такой хаб
//! виден системе дважды, и клавиатура с мышью висят на его половине USB 2.
//!
//! # Порядок инициализации
//!
//! Он задан спецификацией (xHCI 1.2, 4.2) и переставлять его нельзя:
//!
//! 1. остановить контроллер и сбросить его — прошивка оставила его настроенным
//!    под себя, с собственными кольцами в памяти, которую ядро уже раздало;
//! 2. дождаться `CNR`: пока стоит этот бит, запись в регистры теряется молча;
//! 3. сообщить число слотов, массив контекстов, кольцо команд, кольцо событий;
//! 4. и только потом разрешить работу — контроллер начинает читать кольца
//!    немедленно, поэтому все они обязаны существовать заранее.

pub mod regs;
pub mod ring;

use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

use crate::acpi::AcpiError;
use crate::input;
use crate::time;
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{MapError, PAGE_SIZE, PhysAddr};
use crate::pci;
use crate::usb::hid::{REPORT_LEN, Reader, choose_reader};
use crate::usb::{self, Attached, HidInterface, Stage, Timeout, sleep_ms};

use alloc::vec::Vec;

use regs::Registers;
use ring::{EventRing, Ring, Trb};

/// Сколько дескрипторов в кольце. 256 — это 4 КиБ, то есть ровно страница; для
/// клавиатуры хватило бы и восьми, но страница всё равно минимальная единица
/// выделения.
const RING_ENTRIES: usize = PAGE_SIZE / ring::TRB_LEN;

/// Сколько слотов устройств ядро просит у контроллера.
///
/// Шестнадцать с фазы 68l (было четыре): хаб занимает слот сам, а за ним —
/// клавиатура, мышь и всё, что воткнуто рядом. Просить много не вредно, но
/// каждый слот — это указатель в массиве контекстов, а массив контроллер
/// читает целиком.
const SLOTS_WANTED: u8 = 16;

/// Сколько хабов подряд драйвер готов пройти — как у EHCI.
const HUB_DEPTH_MAX: u8 = 3;
/// Где в буфере устройства лежит карта изменений хаба: вторая половина
/// страницы, чтобы ответ точки прерываний не затёр данные управляющей
/// передачи к тому же хабу, идущей одновременно.
const HUB_STATUS_OFFSET: usize = PAGE_SIZE / 2;
/// Пауза после включения питания портов хаба, если хаб не назвал свою.
const HUB_POWER_SETTLE_MS: u64 = 100;

// Запросы хаба (USB 2.0, 11.24).
const CLASS_HUB: u8 = 9;
const DESC_HUB: u8 = 0x29;
const HUB_REQ_GET_STATUS: u8 = 0;
const HUB_REQ_CLEAR_FEATURE: u8 = 1;
const HUB_REQ_SET_FEATURE: u8 = 3;
const HUB_TO_HUB: u8 = 0x20;
const HUB_TO_PORT: u8 = 0x23;
const HUB_FROM_HUB: u8 = 0xA0;
const HUB_FROM_PORT: u8 = 0xA3;
const FEATURE_PORT_RESET: u8 = 4;
const FEATURE_PORT_POWER: u8 = 8;
/// `C_PORT_CONNECTION`; остальные признаки изменений порта идут за ним подряд.
const FEATURE_C_PORT_FIRST: u8 = 16;
const HUB_PORT_CONNECTION: u16 = 1 << 0;
const HUB_PORT_ENABLE: u16 = 1 << 1;
const HUB_PORT_RESET: u16 = 1 << 4;
const HUB_PORT_LOW_SPEED: u16 = 1 << 9;
const HUB_PORT_HIGH_SPEED: u16 = 1 << 10;
const HUB_CHANGE_CONNECTION: u16 = 1 << 0;
const HUB_CHANGE_RESET: u16 = 1 << 4;

/// Минимальная версия интерфейса. `0x0090` — это xHCI 0.9, черновик, который
/// встречался в первых чипсетах Intel и отличается расположением части полей.
const MIN_VERSION: u16 = 0x0090;

/// Сколько ждать сброса и запуска контроллера.
const RESET_TIMEOUT_MS: u64 = 1000;
/// Сколько ждать завершения команды.
const COMMAND_TIMEOUT_MS: u64 = 500;

/// Сколько ждать, пока прошивка отпустит контроллер. Столько же ждёт Linux.
const HANDOFF_TIMEOUT_MS: u64 = 1000;

/// Расширенная возможность USB Legacy Support (xHCI 1.1, 7.1).
const EXT_CAP_LEGACY: u8 = 1;
/// `USBLEGSUP`, бит 16: контроллером владеет прошивка.
const LEGSUP_BIOS_OWNED: u32 = 1 << 16;
/// `USBLEGSUP`, бит 24: система просит контроллер себе.
const LEGSUP_OS_OWNED: u32 = 1 << 24;
/// `USBLEGCTLSTS` — сразу за `USBLEGSUP`.
const LEGCTLSTS_OFFSET: usize = 4;
/// Биты `USBLEGCTLSTS`, которые сохраняются: всё, кроме разрешений SMI. Маска
/// та же, что в Linux (`XHCI_LEGACY_DISABLE_SMI`).
const LEGCTLSTS_KEEP: u32 = (0x7 << 1) | (0xFF << 5) | (0x7 << 17);
/// Признаки случившихся SMI; сбрасываются записью единицы.
const LEGCTLSTS_SMI_EVENTS: u32 = 0x7 << 29;
/// Сколько ждать завершения передачи по управляющей точке.
const TRANSFER_TIMEOUT_MS: u64 = 500;
/// Сколько ждать окончания сброса порта.
const PORT_RESET_TIMEOUT_MS: u64 = 500;
/// Пауза после сброса порта: спецификация USB требует дать устройству время на
/// восстановление, прежде чем обращаться к нему.
const PORT_RECOVERY_MS: u64 = 20;
/// Интервал дебаунса USB 2.0 — сколько подключение обязано простоять, прежде чем
/// его сбрасывают (USB 2.0, 7.1.7.3).
///
/// Ядро приходит к портам сразу после того, как перезапустило контроллер, и
/// попадает ровно в то окно, когда порты ещё сбрасываются сами. VirtualBox в
/// этом окне запрос отбрасывает вслух («reset request is ignored, the device is
/// already resetting»), но порт всё равно отчитывается о завершении сброса, и
/// дальше драйвер разговаривает с устройством, которое его не слышало. Стоило
/// это того, что на VirtualBox/Apple Silicon клавиатура раз через раз
/// опознавалась вторым планшетом — с чужим `VID:PID` и чужим дескриптором
/// отчётов.
const PORT_DEBOUNCE_MS: u64 = 100;

/// Почему контроллер или устройство не заработали.
#[derive(Debug, Clone, Copy)]
pub enum XhciError {
    /// Не нашлось таблиц ACPI, а значит и окна конфигурационного пространства.
    Acpi(AcpiError),
    /// На шине нет контроллера с классом xHCI.
    NoController,
    /// Контроллеры USB есть, но все — не xHCI.
    ///
    /// Отдельно от [`XhciError::NoController`] потому, что лечится по-другому:
    /// там машина без шины USB, здесь — машина, которой нужен драйвер, которого
    /// у нас нет. Ровно это и случилось на VirtualBox, где по умолчанию дают
    /// OHCI; перепись выше уже назвала каждый найденный контроллер поимённо.
    OtherController,
    /// У контроллера не заполнен BAR0 — прошивка не выделила ему окно.
    NoBar,
    /// Окно регистров не удалось отобразить.
    Map(MapError),
    /// Версия интерфейса ниже той, с которой драйвер умеет работать.
    Version(u16),
    /// Контроллер не завершил сброс.
    ResetTimeout,
    /// Контроллер не запустился.
    StartTimeout,
    /// Не удалось выделить буфер под кольца или контексты.
    Dma(DmaError),
    /// Команда не завершилась за отведённое время.
    CommandTimeout(u32),
    /// Контроллер адресует только 32 бита, а окно DMA лежит выше 4 ГиБ.
    AddressWidth(u64),
    /// Команда завершилась ошибкой.
    CommandFailed { command: u32, code: u32 },
    /// Ни на одном порту нет подключённого устройства.
    ///
    /// Больше не возвращается перечислением: пустой перебор портов — это не
    /// ошибка, а машина без устройств, и говорит об этом строка журнала.
    /// Вариант оставлен потому, что подключение устройства на ходу (событие
    /// изменения порта) будет отвечать именно им.
    #[allow(dead_code)]
    NoDevice,
    /// Порт не вышел из сброса.
    PortResetTimeout(u8),
    /// Передача по управляющей точке не удалась.
    TransferFailed { code: u32 },
    /// Передача не завершилась за отведённое время.
    ///
    /// Несёт, сколько ядро прождало и по какому пределу вышло: часы и витки —
    /// два разных отказа, а выглядят они одинаково.
    TransferTimeout { waited_ms: u64, spun_out: bool },
    /// Дескриптор пришёл короче, чем должен быть.
    ShortDescriptor,
    /// Устройство есть, но интерфейса HID с точкой прерываний среди его
    /// интерфейсов нет.
    NoHid,
    /// Интерфейс HID есть, но понять его нечем: boot-протокола он не объявляет,
    /// а из дескриптора отчётов ничего пригодного не вышло.
    ///
    /// Отдельная ошибка, а не молчание: устройство при этом исправно, и разница
    /// между «на порту ничего нет» и «на порту есть то, чего мы не понимаем» —
    /// это разница между «купите мышь» и «допишите разбор».
    UnknownHid,
    /// Хаб не отдал свой дескриптор.
    HubDescriptor,
    /// Хаб USB 3: его драйвер не проходит.
    SuperSpeedHub,
    /// Хабы вложены глубже [`HUB_DEPTH_MAX`].
    TooDeep,
}

impl core::fmt::Display for XhciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Acpi(err) => write!(f, "{err}"),
            Self::NoController => f.write_str("no xHCI controller on the PCI bus"),
            Self::OtherController => {
                f.write_str("the USB controllers here are not xHCI; see the list above")
            }
            Self::NoBar => f.write_str("the controller has no memory BAR assigned"),
            Self::Map(err) => write!(f, "mapping the register window failed: {err}"),
            Self::Version(version) => {
                write!(f, "interface version {version:#06x} predates xHCI 0.96")
            }
            Self::ResetTimeout => f.write_str("the controller did not finish its reset"),
            Self::StartTimeout => f.write_str("the controller did not start"),
            Self::Dma(err) => write!(f, "{err}"),
            Self::CommandTimeout(command) => write!(f, "command {command} never completed"),
            Self::AddressWidth(phys) => write!(
                f,
                "the controller addresses only 32 bits, but its rings lie at {phys:#x}, above 4 GiB"
            ),
            Self::CommandFailed { command, code } => write!(
                f,
                "command {command} failed: {} ({code})",
                ring::completion_name(*code)
            ),
            Self::NoDevice => f.write_str("no device is attached to any root port"),
            Self::PortResetTimeout(port) => write!(f, "port {port} did not leave reset"),
            Self::TransferFailed { code } => {
                write!(f, "control transfer failed: {} ({code})", ring::completion_name(*code))
            }
            Self::TransferTimeout { waited_ms, spun_out } => write!(
                f,
                "a control transfer never completed ({} after {waited_ms} ms)",
                if *spun_out { "ran out of spins" } else { "timed out by the clock" }
            ),
            Self::ShortDescriptor => f.write_str("the device returned a truncated descriptor"),
            Self::NoHid => f.write_str("the device has no HID interface with an interrupt endpoint"),
            Self::UnknownHid => f.write_str(
                "the HID interface declares no boot protocol and its report descriptor \
                 describes neither a pointer nor a keyboard",
            ),
            Self::HubDescriptor => f.write_str("the hub did not return its descriptor"),
            Self::SuperSpeedHub => f.write_str(
                "USB 3 hubs are not walked; keyboards and mice sit on the hub's USB 2 half",
            ),
            Self::TooDeep => f.write_str("hubs are nested deeper than the driver goes"),
        }
    }
}

impl From<DmaError> for XhciError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
}

/// Подключённое устройство.
struct Device {
    slot: u8,
    /// Корневой порт, к которому она подключена — сама или через хабы.
    port: u8,
    /// Место за хабом; у устройства на корневом порту — пустое.
    place: Place,
    /// Скорость: нужна детям хаба (транслятор) и его точке прерываний.
    speed: u32,
    /// Если это хаб — его порты.
    hub: Option<Hub>,
    /// Контекст устройства: его заполняет контроллер, ядро только читает.
    context: DmaBuffer,
    /// Входной контекст: через него драйвер сообщает контроллеру, что менять.
    input: DmaBuffer,
    /// Кольцо управляющей точки.
    ep0: Ring,
    /// Кольцо точки прерываний, по которой приходят отчёты.
    interrupt: Ring,
    /// Идентификатор точки прерываний для дверного звонка.
    interrupt_target: u8,
    /// Буфер, в который контроллер складывает отчёт.
    report: DmaBuffer,
    /// Размер отчёта, который запрашивается у устройства.
    report_len: u16,
    /// Разбор отчётов в события.
    ///
    /// `None` до тех пор, пока не прочитаны дескрипторы: до этого момента ещё
    /// неизвестно, клавиатура это или мышь, а буфер уже используется — под сами
    /// дескрипторы. Отчёты в это время не запрашиваются.
    reader: Option<Reader>,
    /// Ждёт ли сейчас устройство отчёта (дескриптор в кольце).
    queued: bool,
    /// Кто изготовитель и что за модель — из дескриптора устройства.
    ///
    /// Хранится ради одного вопроса, на который иначе нечем ответить на машине
    /// без последовательного порта: «а это вообще разные устройства?». Два
    /// одинаковых идентификатора на двух портах означают одно устройство,
    /// увиденное дважды, и это совсем другая неисправность, чем «клавиатура
    /// разобрана как указатель».
    identity: (u16, u16),
    /// Длина дескриптора отчётов, по которому устройство разобрано; ноль —
    /// разбирали не по нему, а по boot-протоколу.
    described_by: u16,
    /// Номер поднятого интерфейса и сколько их всего у устройства.
    ///
    /// Второе число важнее первого: составное устройство — «мышь» с
    /// клавиатурой внутри — драйвер обслуживает только наполовину, и без этого
    /// числа вторая половина невидима.
    interface: (u8, u8),
}

/// Где устройство стоит за хабом (фаза 68l).
#[derive(Clone, Copy, Default)]
struct Place {
    /// Слот хаба-родителя; ноль — устройство на корневом порту.
    parent: u8,
    /// Порт этого хаба.
    hub_port: u8,
    /// Route string: номер порта на каждом ярусе хабов, по четыре бита.
    route: u32,
    /// Сколько хабов над устройством.
    depth: u8,
    /// Транслятор для медленного устройства за быстрым хабом: слот хаба и
    /// его порт; нули — транслятор не нужен.
    tt: (u8, u8),
}

impl Place {
    /// Место устройства на порту `port` хаба `hub` при скорости `speed`.
    fn child(hub: &Device, port: u8, speed: u32) -> Self {
        let depth = hub.place.depth;
        // Портов с номером больше 15 в route string не бывает: поле
        // четырёхбитное, и число портов хаба этим же и ограничено.
        let route = hub.place.route | (u32::from(port.min(15)) << (4 * u32::from(depth)));
        // Транслятор — у ближайшего высокоскоростного хаба на пути.
        let tt = if speed == regs::SPEED_HIGH || speed >= regs::SPEED_SUPER {
            (0, 0)
        } else if hub.speed == regs::SPEED_HIGH {
            (hub.slot, port)
        } else {
            hub.place.tt
        };
        Self { parent: hub.slot, hub_port: port, route, depth: depth + 1, tt }
    }

    /// Название места для журнала.
    fn name(self, root: u8) -> alloc::string::String {
        if self.parent == 0 {
            alloc::format!("root port {root}")
        } else {
            alloc::format!("hub {} port {}", self.parent, self.hub_port)
        }
    }
}

/// Состояние хаба.
#[derive(Clone, Copy)]
struct Hub {
    ports: u8,
    /// Какие порты были заняты при последней сверке — по биту на порт.
    connected: u32,
    /// Карта изменений от точки прерываний хаба, ещё не разобранная: бит 0 —
    /// сам хаб, бит N — порт N.
    changed: u32,
}

/// Что за устройство нашлось по дескрипторам.
enum Described {
    Hid(HidInterface),
    Hub(HubInterface),
}

/// Интерфейс хаба и его точка прерываний.
#[derive(Clone, Copy)]
struct HubInterface {
    configuration: u8,
    endpoint: u8,
    max_packet_size: u16,
    interval: u8,
}

/// Контроллер и подключённые к нему устройства.
pub struct Controller {
    regs: Registers,
    /// Массив адресов контекстов устройств.
    dcbaa: DmaBuffer,
    /// Массив адресов буферов-черновиков; `None`, если контроллер их не требует.
    /// Сами буферы владельца не имеют — окно DMA не умеет освобождать, и они
    /// живут до конца работы ядра.
    scratchpad: Option<DmaBuffer>,
    command: Ring,
    events: EventRing,
    /// Таблица сегментов кольца событий. Контроллер читает её при старте, но
    /// адрес должен оставаться действительным всё время работы.
    erst: DmaBuffer,
    /// Устройства в порядке подключения. Вектор, а не одно поле: клавиатура и
    /// мышь — это два слота, два кольца прерываний и два разбора отчётов.
    devices: Vec<Device>,
    /// Сколько событий разобрано — диагностика.
    events_seen: u64,
    /// Сколько раз разбор кольца упёрся в предел за один проход. Отличен от нуля
    /// означает, что контроллер отдаёт события быстрее, чем драйвер их забирает,
    /// — либо что кольцо испорчено и цикл не сходится.
    event_floods: u64,
    /// Сколько передач завершилось ошибкой.
    transfer_errors: u64,
    /// Устройство на шине: нужно после инициализации, чтобы настроить MSI-X —
    /// его таблица включается через конфигурационное пространство.
    device: pci::Device,
    /// Место в списке контроллеров и в таблице [`ACK`].
    index: usize,
    /// Сколько раз задача просыпалась и разбирала кольцо.
    ///
    /// Это и есть цена опроса, выраженная числом: при опросе счётчик растёт сто
    /// раз в секунду независимо от того, происходит ли хоть что-нибудь, и
    /// каждый его шаг — это переключение контекста, перебор кольца и обращения
    /// к регистрам контроллера.
    services: u64,
    /// Сколько корневых портов оказались занятыми при перечислении.
    ///
    /// Хранится затем, чтобы отличить «устройства нет» от «устройство есть, но
    /// поднять его не удалось». На машине без последовательного порта это
    /// единственный способ такое различить: журнала там не существует, а числа
    /// видны в оболочке.
    occupied: usize,
    /// Чем закончилась последняя неудачная попытка поднять устройство: порт,
    /// шаг и сама ошибка.
    last_error: Option<(u8, Stage, XhciError)>,
    /// Состав портов изменился: либо контроллер сообщил событием, либо это
    /// заметила сверка маски. Разбирает признак задача, а не тот, кто его
    /// поставил.
    ports_changed: bool,
    /// Какие порты были заняты при последней сверке — по биту на порт.
    ///
    /// Событию об изменении состояния порта верить можно, но нельзя **только**
    /// ему: гипервизор вправе подключить устройство так, что событие до нас не
    /// доедет, а маска всё равно изменится. Сверка стоит одного чтения регистра
    /// на порт и закрывает этот случай.
    connected: u32,
}

impl Controller {
    /// Найти все контроллеры xHCI на шине (фаза 68m: у AMD их обычно два-три,
    /// у Intel бывает дополнительный на отдельной микросхеме).
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, а память с
    /// таблицами ACPI — оставаться нетронутой.
    unsafe fn find_all(rsdp: u64) -> Result<Vec<pci::Device>, XhciError> {
        // SAFETY: контракт функции.
        let root = unsafe { pci::Root::discover(rsdp) }.map_err(XhciError::Acpi)?;
        kprintln!("  pci         : {root}");

        // Перепись идёт **до** поиска своего контроллера и печатается всегда.
        // Машина, на которой ввода нет, обязана сказать почему: «есть OHCI,
        // драйвера нет» и «шины USB нет вовсе» — разные болезни, а выглядят они
        // одинаково — мёртвой клавиатурой.
        //
        // SAFETY: контракт функции.
        let census = unsafe { super::survey::take(&root) };

        let mut found = Vec::new();
        // SAFETY: контракт функции.
        unsafe {
            pci::for_each(&root, |device| {
                if device.class == pci::CLASS_SERIAL_BUS
                    && device.subclass == pci::SUBCLASS_USB
                    && device.prog_if == pci::PROG_IF_XHCI
                {
                    found.push(*device);
                }
                true
            });
        }
        if found.is_empty() {
            return Err(if census.found > 0 {
                XhciError::OtherController
            } else {
                XhciError::NoController
            });
        }
        Ok(found)
    }

    /// Поднять контроллер. `index` — его место в списке контроллеров и в
    /// таблице подтверждений прерываний.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, прерывания —
    /// быть разрешены (ожидания опираются на таймер).
    unsafe fn init(device: pci::Device, index: usize) -> Result<Self, XhciError> {
        kprintln!(
            "  xhci        : {} vendor {:#06x} device {:#06x} rev {:#04x}",
            device.address,
            device.vendor,
            device.device,
            device.revision
        );

        // Ответы на обращения к памяти разрешаются **до** первого чтения
        // регистров, и это не порядок ради порядка. При сброшенном бите Memory
        // Space устройство просто не отзывается на своём окне: чтение возвращает
        // нули или единицы, и выглядит это как «контроллер сообщает версию
        // 0x0000» — то есть как неисправное железо, а не как забытый бит.
        //
        // Bus Master включается тем же движением, хотя кольца ещё не построены.
        // Опасности нет: первое, что делает драйвер с контроллером ниже, — это
        // остановка и сброс, а остановленный контроллер к памяти не обращается.
        //
        // SAFETY: регистр `Command` — часть конфигурационного пространства,
        // отображённого при переборе шины.
        unsafe { device.enable_bus_master() };

        let bar = device.memory_bar(0).ok_or(XhciError::NoBar)?;
        // SAFETY: контракт функции; BAR указывает на окно регистров контроллера,
        // которому нужна именно Device-семантика.
        let base = unsafe { map_bar(bar) }.map_err(XhciError::Map)?;
        kprintln!(
            "  xhci        : BAR0 {:?} -> {base:#018x}, command {:#06x}",
            bar,
            device.command()
        );

        // SAFETY: окно отображено, адрес получен от прошивки через BAR.
        // SAFETY: окно отображено, адрес получен от прошивки через BAR.
        let regs = unsafe { Registers::probe(base) };
        if regs.version < MIN_VERSION {
            return Err(XhciError::Version(regs.version));
        }
        kprintln!(
            "  xhci        : version {}.{:x}, {} slots, {} ports, {}-byte contexts, {} scratchpad, {}-bit addresses",
            regs.version >> 8,
            (regs.version >> 4) & 0xF,
            regs.max_slots,
            regs.max_ports,
            regs.context_size,
            regs.max_scratchpad,
            if regs.ac64 { 64 } else { 32 }
        );

        // Контроллер забирается у прошивки до всего остального, в том числе до
        // остановки и сброса: пока им владеет её обработчик SMM, он вправе
        // вмешаться в любое наше обращение.
        // SAFETY: окно регистров отображено и прочитано `probe`.
        unsafe { take_from_firmware(&regs) };

        let mut controller = Self {
            regs,
            dcbaa: dma::alloc(8 * (usize::from(SLOTS_WANTED) + 1))?,
            scratchpad: None,
            command: Ring::new(dma::alloc(RING_ENTRIES * ring::TRB_LEN)?),
            events: EventRing::new(dma::alloc(RING_ENTRIES * ring::TRB_LEN)?),
            erst: dma::alloc(16)?,
            devices: Vec::new(),
            events_seen: 0,
            event_floods: 0,
            transfer_errors: 0,
            device,
            index,
            services: 0,
            occupied: 0,
            last_error: None,
            ports_changed: false,
            connected: 0,
        };

        // Контроллер без 64-битной адресации видит только первые 4 ГиБ. Окно
        // DMA — один непрерывный кусок размером `DMA_SIZE`, и все кольца лежат в
        // нём, поэтому достаточно проверить, где оно может кончаться. Сказать это
        // словами дешевле, чем разбирать «команда не завершилась» по фотографии.
        if !controller.regs.ac64 {
            let lowest = controller
                .dcbaa
                .phys()
                .as_u64()
                .min(controller.erst.phys().as_u64());
            if lowest + crate::mm::DMA_SIZE as u64 > 1 << 32 {
                return Err(XhciError::AddressWidth(lowest));
            }
        }

        // SAFETY: окно регистров отображено, кольца выделены и обнулены.
        unsafe { controller.reset() }?;
        // SAFETY: см. выше.
        unsafe { controller.configure() }?;
        // SAFETY: см. выше; контроллер сброшен и настроен.
        unsafe { controller.start() }?;
        // Прерывания разрешаются последними: кольцо событий к этому моменту
        // существует и обслуживается, поэтому первое же прерывание найдёт всё
        // на месте. Отказ здесь не отменяет работу драйвера — он лишь оставляет
        // его на опросе, и это печатается.
        //
        // SAFETY: контроллер запущен, кольцо событий работает.
        unsafe { controller.enable_interrupts() };

        Ok(controller)
    }

    /// Остановить и сбросить контроллер.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn reset(&mut self) -> Result<(), XhciError> {
        // SAFETY: контракт функции.
        let command = unsafe { self.regs.read_op32(regs::OP_USBCMD) };
        // Остановка перед сбросом обязательна: сброс работающего контроллера
        // спецификацией объявлен неопределённым поведением, а прошивка почти
        // наверняка оставила его работающим — её собственный драйвер USB
        // пользовался клавиатурой в меню загрузки.
        // SAFETY: см. выше.
        unsafe {
            self.regs.write_op32(regs::OP_USBCMD, command & !regs::USBCMD_RUN);
        }

        let mut timeout = Timeout::new(RESET_TIMEOUT_MS);
        // SAFETY: см. выше.
        while unsafe { self.regs.read_op32(regs::OP_USBSTS) } & regs::USBSTS_HALTED == 0 {
            if timeout.expired() {
                return Err(XhciError::ResetTimeout);
            }
        }

        // SAFETY: контроллер остановлен.
        unsafe { self.regs.write_op32(regs::OP_USBCMD, regs::USBCMD_RESET) };

        // Сброс считается законченным, когда сброшены **оба** бита: `HCRST`
        // (контроллер отпустил сам сброс) и `CNR` (внутренняя инициализация
        // завершена). Проверять только первый — классическая ошибка: запись в
        // регистры между этими двумя моментами теряется без всяких признаков.
        let mut timeout = Timeout::new(RESET_TIMEOUT_MS);
        loop {
            // SAFETY: см. выше.
            let (command, status) = unsafe {
                (
                    self.regs.read_op32(regs::OP_USBCMD),
                    self.regs.read_op32(regs::OP_USBSTS),
                )
            };
            if command & regs::USBCMD_RESET == 0 && status & regs::USBSTS_NOT_READY == 0 {
                break;
            }
            if timeout.expired() {
                return Err(XhciError::ResetTimeout);
            }
        }
        Ok(())
    }

    /// Сообщить контроллеру все структуры: слоты, контексты, кольца.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть сброшен и остановлен.
    unsafe fn configure(&mut self) -> Result<(), XhciError> {
        // Число слотов. Больше, чем поддерживает контроллер, просить нельзя.
        let slots = SLOTS_WANTED.min(self.regs.max_slots.max(1));
        // SAFETY: контракт функции.
        unsafe { self.regs.write_op32(regs::OP_CONFIG, u32::from(slots)) };

        // Буферы-черновики: память, которую контроллер использует для своих
        // внутренних нужд. Не выделить требуемое количество — значит получить
        // контроллер, который стартует и ведёт себя непредсказуемо.
        if self.regs.max_scratchpad > 0 {
            let count = usize::from(self.regs.max_scratchpad);
            let array = dma::alloc(count * 8)?;
            for index in 0..count {
                let buffer = dma::alloc(self.regs.page_size)?;
                // SAFETY: индекс внутри массива, выделенного под `count`
                // указателей; буфер только что выделен и обнулён.
                unsafe {
                    array
                        .as_ptr::<u64>()
                        .add(index)
                        .write_volatile(buffer.phys().as_u64());
                }
            }
            // Нулевой элемент массива контекстов по спецификации отведён под
            // адрес массива черновиков, а не под контекст устройства: слоты
            // нумеруются с единицы.
            // SAFETY: массив контекстов выделен под `slots + 1` элементов.
            unsafe { self.dcbaa.as_ptr::<u64>().write_volatile(array.phys().as_u64()) };
            self.scratchpad = Some(array);
        }

        // SAFETY: контракт функции; массив контекстов выделен и заполнен.
        unsafe { self.regs.write_op64(regs::OP_DCBAAP, self.dcbaa.phys().as_u64()) };

        // Кольцо команд. Бит `RCS` обязан совпадать с тем, каким кольцо помечает
        // свои дескрипторы, иначе контроллер сочтёт кольцо пустым и не выполнит
        // ни одной команды — молча.
        let crcr = self.command.phys()
            | if self.command.initial_cycle() { regs::CRCR_RING_CYCLE_STATE } else { 0 };
        // SAFETY: см. выше.
        unsafe { self.regs.write_op64(regs::OP_CRCR, crcr) };

        // Таблица сегментов кольца событий: у нас один сегмент.
        // SAFETY: буфер выделен под 16 байт — ровно одна запись таблицы.
        unsafe {
            self.erst.as_ptr::<u64>().write_volatile(self.events.phys());
            self.erst
                .as_ptr::<u32>()
                .add(2)
                .write_volatile(self.events.entries() as u32);
        }

        // Порядок трёх записей ниже задан спецификацией: размер таблицы, затем
        // позиция потребителя, затем адрес таблицы. Последняя запись и есть та,
        // которая вводит кольцо в работу.
        // SAFETY: контракт функции.
        unsafe {
            self.regs.write_interrupter32(0, regs::IR_ERSTSZ, 1);
            self.regs.write_interrupter64(0, regs::IR_ERDP, self.events.dequeue_phys());
            self.regs.write_interrupter64(0, regs::IR_ERSTBA, self.erst.phys().as_u64());
            // Прерывания от контроллера не разрешаются: события опрашиваются.
            // `IMOD` при этом не важен, но обнуляется, чтобы состояние регистра
            // не зависело от того, что оставила прошивка.
            self.regs.write_interrupter32(0, regs::IR_IMOD, 0);
        }
        Ok(())
    }

    /// Разрешить контроллеру работать.
    ///
    /// # Safety
    ///
    /// Все структуры должны быть уже сообщены (см. [`Controller::configure`]).
    unsafe fn start(&mut self) -> Result<(), XhciError> {
        // `HSEE` включается вместе с запуском: без него ошибка обращения
        // контроллера к памяти не будет видна нигде, кроме бита в `USBSTS`.
        // SAFETY: контракт функции.
        unsafe {
            self.regs
                .write_op32(regs::OP_USBCMD, regs::USBCMD_RUN | regs::USBCMD_HSEE);
        }

        let mut timeout = Timeout::new(RESET_TIMEOUT_MS);
        // SAFETY: см. выше.
        while unsafe { self.regs.read_op32(regs::OP_USBSTS) } & regs::USBSTS_HALTED != 0 {
            if timeout.expired() {
                return Err(XhciError::StartTimeout);
            }
        }

        // Пустая команда — самая дешёвая проверка того, что кольцо команд,
        // кольцо событий и дверной звонок согласованы между собой. Если здесь
        // ничего не приходит, дальше можно не идти: остальные команды устроены
        // так же, но их отказ выглядел бы как проблема с устройством.
        // SAFETY: контроллер работает, кольца сообщены.
        unsafe {
            self.command_execute(Trb {
                parameter: 0,
                status: 0,
                control: ring::TRB_NO_OP_COMMAND << ring::TRB_TYPE_SHIFT,
            })
        }?;
        Ok(())
    }

    /// Обновить позицию потребителя кольца событий.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn update_erdp(&mut self) {
        // Бит `EHB` сбрасывается записью единицы вместе с новым указателем: он
        // означает «драйвер разбирает события», и не сняв его, следующего
        // прерывания не будет. Прерывания мы не используем, но оставленный бит
        // мешает и опросу — контроллер считает, что драйвер ещё занят.
        let value = self.events.dequeue_phys() | regs::ERDP_EVENT_HANDLER_BUSY;
        // SAFETY: контракт функции.
        unsafe { self.regs.write_interrupter64(0, regs::IR_ERDP, value) };
    }

    /// Разобрать все накопившиеся события.
    ///
    /// `awaited` — физический адрес дескриптора, событие о котором нужно вернуть;
    /// всё остальное обрабатывается по дороге. Такое устройство неизбежно:
    /// события приходят в одно кольцо, и ожидая завершения команды, драйвер
    /// обязан не потерять отчёт клавиатуры, пришедший в тот же момент.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn drain_events(&mut self, awaited: Option<u64>) -> Option<Trb> {
        let mut result = None;
        let mut moved = false;
        // Предел на один проход. Кольцо конечно, и честный контроллер отдаст
        // не больше событий, чем в нём мест; но здесь единственный цикл во всём
        // драйвере, который не ограничен ничем, кроме доброй воли устройства, —
        // а поведение чужого контроллера это ровно то, чего мы не знаем. Без
        // предела отказ выглядит как молчаливое зависание загрузки, то есть
        // самый дорогой в диагностике вид отказа.
        let mut left = RING_ENTRIES * 2;

        while let Some(event) = self.events.pop() {
            self.events_seen += 1;
            moved = true;
            left -= 1;
            if left == 0 {
                self.event_floods += 1;
                break;
            }

            match event.kind() {
                ring::TRB_COMMAND_COMPLETION | ring::TRB_TRANSFER_EVENT => {
                    if awaited == Some(event.parameter) && result.is_none() {
                        result = Some(event);
                        continue;
                    }
                    if event.kind() == ring::TRB_TRANSFER_EVENT {
                        self.handle_transfer_event(&event);
                    }
                }
                // Изменение состояния порта: что-то воткнули или вынули.
                // Разбирать прямо здесь нельзя — перечисление длится сотни
                // миллисекунд, а мы внутри лока с запрещёнными прерываниями.
                // Поэтому только признак; работу делает задача (см.
                // [`poll_hotplug`]).
                ring::TRB_PORT_STATUS_CHANGE => self.ports_changed = true,
                _ => {}
            }
        }

        if moved {
            // SAFETY: контракт функции.
            unsafe { self.update_erdp() };
        }
        result
    }

    /// Отправить команду и дождаться её завершения.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать, а кольцо команд — быть ему сообщено.
    unsafe fn command_execute(&mut self, trb: Trb) -> Result<Trb, XhciError> {
        let kind = trb.kind();
        let addr = self.command.push(trb);
        // SAFETY: дескриптор записан в кольцо целиком до звонка; слот 0 и цель 0
        // — это кольцо команд.
        unsafe { self.regs.ring_doorbell(0, 0) };

        let mut timeout = Timeout::new(COMMAND_TIMEOUT_MS);
        loop {
            // SAFETY: контракт функции.
            if let Some(event) = unsafe { self.drain_events(Some(addr)) } {
                if event.is_success() {
                    return Ok(event);
                }
                return Err(XhciError::CommandFailed {
                    command: kind,
                    code: event.completion_code(),
                });
            }
            if timeout.expired() {
                return Err(XhciError::CommandTimeout(kind));
            }
        }
    }

    /// Найти следующий порт с подключённым устройством, начиная с `from`.
    ///
    /// Начало перебора — параметр, а не единица: перечисление устройств идёт по
    /// портам подряд, и без него поиск после первого найденного возвращал бы его
    /// же вечно.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn find_device_port(&mut self, from: u8) -> Option<(u8, u32)> {
        for port in from.max(1)..=self.regs.max_ports {
            // SAFETY: номер порта в пределах, сообщённых контроллером.
            let status = unsafe { self.regs.read_portsc(port) };
            if status & regs::PORTSC_CONNECTED == 0 {
                continue;
            }
            let speed = (status >> regs::PORTSC_SPEED_SHIFT) & regs::PORTSC_SPEED_MASK;
            return Some((port, speed));
        }
        None
    }

    /// Сбросить порт и дождаться, пока он заработает.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn reset_port(&mut self, port: u8) -> Result<u32, XhciError> {
        // SAFETY: номер порта проверен вызывающим.
        let status = unsafe { self.regs.read_portsc(port) };

        // Питание подаётся, если его нет: без него устройство не ответит, а
        // признак подключения при этом может уже стоять.
        if status & regs::PORTSC_POWER == 0 {
            // SAFETY: см. выше. Из прочитанного значения вычищаются все
            // RW1C-биты — иначе обратная запись сбросила бы признаки, которых мы
            // ещё не разобрали, и, что хуже, выключила бы порт (бит `PED` тоже
            // RW1C).
            unsafe {
                self.regs.write_portsc(
                    port,
                    (status & !regs::PORTSC_RW1C_MASK) | regs::PORTSC_POWER,
                );
            }
            sleep_ms(PORT_RECOVERY_MS);
        }

        // SAFETY: см. выше.
        let status = unsafe { self.regs.read_portsc(port) };
        let speed = (status >> regs::PORTSC_SPEED_SHIFT) & regs::PORTSC_SPEED_MASK;
        if status & regs::PORTSC_ENABLED != 0 && speed >= regs::SPEED_SUPER {
            // SuperSpeed: обучение линии контроллер проводит сам, порт включается
            // без участия драйвера, и сброс здесь только сломал бы уже
            // установленное соединение.
            return Ok(speed);
        }

        // А вот включённый порт USB 2 сбрасывать **обязательно**, и это стоило
        // отдельного вечера. Прошивка пользуется клавиатурой в своём загрузочном
        // меню и оставляет её адресованной; сброс контроллера (`HCRST`) обнуляет
        // состояние **контроллера**, но не устройства. Пропустив сброс порта, мы
        // выдаём устройству новый адрес командой `Address Device`, а оно
        // продолжает слушать прежний — и молчит. Выглядит это как «управляющая
        // передача не завершилась за 500 мс» на исправном устройстве, причём
        // через раз: смотря трогала ли прошивка эту клавиатуру в этот раз.
        //
        // Сброс порта — единственное, что возвращает устройство USB 2 в
        // состояние Default с адресом 0.
        //
        // Но сбрасывать порт, который сбрасывается прямо сейчас, бесполезно:
        // такой запрос вправе пропасть (см. [`PORT_DEBOUNCE_MS`]). Поэтому
        // сначала дожидаемся чужого сброса, затем выдерживаем дебаунс — и
        // только потом просим свой.
        let mut settle = Timeout::new(PORT_RESET_TIMEOUT_MS);
        loop {
            // SAFETY: см. выше.
            let status = unsafe { self.regs.read_portsc(port) };
            if status & regs::PORTSC_RESET == 0 || settle.expired() {
                break;
            }
        }
        sleep_ms(PORT_DEBOUNCE_MS);
        // Значение, прочитанное до пауз, устарело: за это время порт мог
        // закончить чужой сброс и сменить и скорость, и признаки изменений.
        // SAFETY: см. выше.
        let status = unsafe { self.regs.read_portsc(port) };

        // SAFETY: см. выше; вместе со сбросом снимаем признак изменения
        // подключения — он уже разобран, и оставлять его незачем.
        unsafe {
            self.regs.write_portsc(
                port,
                (status & !regs::PORTSC_RW1C_MASK)
                    | regs::PORTSC_RESET
                    | regs::PORTSC_CONNECT_CHANGE,
            );
        }

        let mut timeout = Timeout::new(PORT_RESET_TIMEOUT_MS);
        loop {
            // SAFETY: см. выше.
            let status = unsafe { self.regs.read_portsc(port) };
            if status & regs::PORTSC_RESET_CHANGE != 0 && status & regs::PORTSC_ENABLED != 0 {
                // Признак завершения сброса снимается явно, иначе он останется
                // стоять и следующий сброс будет неотличим от этого.
                // SAFETY: см. выше.
                unsafe {
                    self.regs.write_portsc(
                        port,
                        (status & !regs::PORTSC_RW1C_MASK) | regs::PORTSC_RESET_CHANGE,
                    );
                }
                sleep_ms(PORT_RECOVERY_MS);
                return Ok((status >> regs::PORTSC_SPEED_SHIFT) & regs::PORTSC_SPEED_MASK);
            }
            if timeout.expired() {
                return Err(XhciError::PortResetTimeout(port));
            }
        }
    }

    /// Перечислить корневые порты и подключить всё, что понимает boot-протокол.
    ///
    /// Отказ на одном порте не прекращает перебор: мышь не должна пропадать
    /// оттого, что на соседнем порту висит устройство неизвестного класса.
    /// Возвращает набор поднятых источников ввода.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать.
    pub unsafe fn attach_devices(&mut self) -> (bool, bool) {
        // Карта занятых портов — одной строкой, до первой попытки с ними
        // что-нибудь сделать. Без неё «перечисление ничего не нашло» и
        // «перечисление не дошло до первого порта» выглядят одинаково: молчание.
        // На чужой машине с четырнадцатью портами это первое, что хочется
        // знать, и стоит оно одного чтения на порт.
        // SAFETY: контракт функции.
        let connected = unsafe { self.connected_mask() };
        kprintln!(
            "  usb         : {} root port(s), connected mask {connected:#010x}",
            self.regs.max_ports
        );

        // SAFETY: см. выше.
        unsafe { self.attach_missing() }
    }

    /// Битовая карта занятых портов: по биту на порт, начиная с первого.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn connected_mask(&mut self) -> u32 {
        let mut connected = 0u32;
        for port in 1..=self.regs.max_ports.min(32) {
            // SAFETY: номер порта в пределах, сообщённых контроллером.
            if unsafe { self.regs.read_portsc(port) } & regs::PORTSC_CONNECTED != 0 {
                connected |= 1 << (port - 1);
            }
        }
        self.connected = connected;
        self.occupied = connected.count_ones() as usize;
        connected
    }

    /// Поднять всё, что занимает порт и ещё не обслуживается.
    ///
    /// Возвращает, появились ли клавиатура и указатель — **среди новых**.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать.
    unsafe fn attach_missing(&mut self) -> (bool, bool) {
        let (mut keyboard, mut mouse) = (false, false);
        let mut next = 1u8;

        while next <= self.regs.max_ports {
            // SAFETY: контракт функции.
            let Some((port, _)) = (unsafe { self.find_device_port(next) }) else {
                break;
            };
            next = port.saturating_add(1);

            // Порт, который уже обслуживается, трогать нельзя: повторный сброс
            // отобрал бы у работающего устройства адрес.
            if self.devices.iter().any(|device| device.port == port) {
                continue;
            }

            // SAFETY: см. выше.
            match unsafe { self.attach_one(port) } {
                Ok((k, m)) => {
                    keyboard |= k;
                    mouse |= m;
                }
                Err((stage, err)) => {
                    kprintln!("  usb         : port {port} failed while {stage}: {err}");
                    self.last_error = Some((port, stage, err));
                }
            }
        }

        (keyboard, mouse)
    }

    /// Перечислить порты заново: поднять появившиеся устройства и забыть
    /// исчезнувшие.
    ///
    /// Возвращает, изменился ли состав.
    ///
    /// # Почему это делает задача, а не обработчик события
    ///
    /// Потому что перечисление — это сбросы портов и управляющие передачи, то
    /// есть сотни миллисекунд ожиданий. Обработчик исполняется с запрещёнными
    /// прерываниями, и на это время остановились бы и часы, и планировщик —
    /// включая тот самый таймер, по которому ожидания и отсчитываются.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать, и вызывать это можно только из задачи.
    pub unsafe fn rescan(&mut self) -> bool {
        self.ports_changed = false;
        let before = self.devices.len();

        // SAFETY: контракт функции.
        let connected = unsafe { self.connected_mask() };

        // Сначала — исчезнувшие: их слоты нужны тем, кто пришёл на их место.
        // Вместе с хабом уходит всё, что за ним: у таких устройств тот же
        // корневой порт.
        let mut index = 0;
        let mut gone = false;
        while index < self.devices.len() {
            let port = self.devices[index].port;
            if connected & (1 << (port - 1)) != 0 {
                index += 1;
                continue;
            }
            let slot = self.devices[index].slot;
            kprintln!("  usb         : device on root port {port} is gone, freeing slot {slot}");
            // SAFETY: устройства на этом порту больше нет.
            unsafe { self.forget(index) };
            gone = true;
        }

        // SAFETY: см. выше.
        let (mut keyboard, mut mouse) = unsafe { self.attach_missing() };
        // Хабы, приславшие карту изменений: их порты сверяются по одному.
        // SAFETY: см. выше.
        let (k, m, hubs_changed) = unsafe { self.scan_changed_hubs() };
        keyboard |= k;
        mouse |= m;
        if keyboard || mouse {
            let sources = input::sources();
            input::set_sources(input::Sources {
                keyboard: sources.keyboard || keyboard,
                mouse: sources.mouse || mouse,
                ..sources
            });
        }

        gone || hubs_changed || before != self.devices.len()
    }

    /// Забыть устройство: освободить слот и вернуть память.
    ///
    /// # Safety
    ///
    /// Устройства с этим индексом больше нет на шине (или его подъём не
    /// удался); дверной звонок ему больше не звонит.
    unsafe fn forget(&mut self, index: usize) {
        let slot = self.devices[index].slot;
        // SAFETY: контракт функции.
        unsafe { self.release_slot(slot) };
        let device = self.devices.remove(index);
        // SAFETY: см. `free_device`.
        unsafe { free_device(&device) };
    }

    /// Забыть устройство на порту `port` хаба `hub` и всё, что за ним.
    ///
    /// # Safety
    ///
    /// См. [`Controller::forget`].
    unsafe fn forget_subtree(&mut self, hub: u8, port: u8) {
        let mut doomed: Vec<u8> = self
            .devices
            .iter()
            .filter(|d| d.place.parent == hub && d.place.hub_port == port)
            .map(|d| d.slot)
            .collect();
        // Потомки — до неподвижной точки: хабов подряд не больше трёх.
        let mut at = 0;
        while at < doomed.len() {
            let parent = doomed[at];
            for device in &self.devices {
                if device.place.parent == parent && !doomed.contains(&device.slot) {
                    doomed.push(device.slot);
                }
            }
            at += 1;
        }
        for slot in doomed {
            if let Some(index) = self.devices.iter().position(|d| d.slot == slot) {
                kprintln!("  usb         : hub {hub} port {port}: slot {slot} is gone");
                // SAFETY: контракт функции.
                unsafe { self.forget(index) };
            }
        }
    }

    /// Вернуть слот контроллеру и убрать его из массива контекстов.
    ///
    /// Память под кольца и контексты при этом не освобождается: окно DMA у
    /// ядра пока одностороннее — выделять умеет, возвращать нет. Устройство,
    /// воткнутое и вынутое сто раз, исчерпает его; это названо вслух, а не
    /// обойдено молча, и лечится общим освобождением DMA, а не здесь.
    ///
    /// # Safety
    ///
    /// Слот должен принадлежать устройству, которого больше нет на шине.
    unsafe fn release_slot(&mut self, slot: u8) {
        // SAFETY: контракт функции; команда освобождает слот у контроллера.
        let result = unsafe {
            self.command_execute(Trb {
                parameter: 0,
                status: 0,
                control: (ring::TRB_DISABLE_SLOT << ring::TRB_TYPE_SHIFT)
                    | (u32::from(slot) << 24),
            })
        };
        if let Err(err) = result {
            kprintln!("  usb         : freeing slot {slot} failed: {err}");
        }
        // Ссылка на контекст убирается в любом случае: слот, о котором
        // контроллер продолжает читать контекст выброшенного устройства, —
        // это чтение памяти, которую ядро вправе раздать заново.
        // SAFETY: слот в пределах массива, выделенного под `SLOTS_WANTED + 1`.
        unsafe {
            self.dcbaa.as_ptr::<u64>().add(usize::from(slot)).write_volatile(0);
        }
    }

    /// Подключить устройство на корневом порту.
    ///
    /// Возвращает, появились ли клавиатура и указатель — за хабом их может
    /// оказаться несколько, — либо шаг, на котором всё остановилось, вместе с
    /// ошибкой. Шаг возвращается наружу, а не только печатается, потому что на
    /// машине без последовательного порта печать некуда девать — а разница
    /// между «не ответило на сброс» и «не отдало дескриптор» решает, куда
    /// смотреть дальше.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать, а порт — иметь подключённое устройство.
    unsafe fn attach_one(&mut self, port: u8) -> Result<(bool, bool), (Stage, XhciError)> {
        // Строка **до** сброса, а не после. Сброс порта — первое, что драйвер
        // делает с чужим устройством, и первое, что на чужой машине может не
        // вернуться; отчёт после него сообщает об успехе, а нужен признак
        // попытки. Разница между «не дошли до порта» и «застряли на порте»
        // стоит одной строки.
        kprintln!("  usb         : root port {port} occupied, resetting");
        // SAFETY: контракт функции.
        let speed = unsafe { self.reset_port(port) }.map_err(|err| (Stage::Reset, err))?;
        kprintln!(
            "  usb         : device on root port {port}, {}",
            regs::speed_name(speed)
        );
        // SAFETY: порт сброшен.
        unsafe { self.attach_device(port, Place::default(), speed) }
    }

    /// Подключить одно устройство: слот, адрес, дескрипторы, конфигурация. Хаб
    /// после подъёма сразу проходится по портам.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать, порт (корневой или хаба) — сброшен и
    /// разрешён.
    unsafe fn attach_device(
        &mut self,
        port: u8,
        place: Place,
        speed: u32,
    ) -> Result<(bool, bool), (Stage, XhciError)> {
        // SAFETY: контракт функции.
        let event = unsafe {
            self.command_execute(Trb {
                parameter: 0,
                status: 0,
                control: ring::TRB_ENABLE_SLOT << ring::TRB_TYPE_SHIFT,
            })
        }
        .map_err(|err| (Stage::Address, err))?;
        let slot = event.slot_id();

        let allocate = |bytes| dma::alloc(bytes).map_err(|err| (Stage::Address, err.into()));
        let context = allocate(self.regs.context_size * 32)?;
        let input = allocate(self.regs.context_size * 33)?;
        let ep0 = Ring::new(allocate(RING_ENTRIES * ring::TRB_LEN)?);
        let interrupt = Ring::new(allocate(RING_ENTRIES * ring::TRB_LEN)?);
        let report = allocate(PAGE_SIZE)?;

        // Массив контекстов: контроллер узнаёт из него, где лежит состояние
        // устройства этого слота.
        // SAFETY: слот выдан контроллером и не превышает числа слотов, под
        // которое выделен массив.
        unsafe {
            self.dcbaa
                .as_ptr::<u64>()
                .add(usize::from(slot))
                .write_volatile(context.phys().as_u64());
        }

        let mut device = Device {
            slot,
            port,
            place,
            speed,
            hub: None,
            context,
            input,
            ep0,
            interrupt,
            interrupt_target: 0,
            report,
            report_len: REPORT_LEN as u16,
            reader: None,
            queued: false,
            identity: (0, 0),
            described_by: 0,
            interface: (0, 0),
        };

        // SAFETY: контракт функции; контексты выделены и обнулены.
        let protocol = match unsafe { self.bring_up(&mut device) } {
            Ok(protocol) => protocol,
            Err(failure) => {
                // Слот возвращается и при отказе: за хабом устройств бывает
                // больше, чем при одном корневом порте, и потерянные слоты
                // кончились бы.
                // SAFETY: устройство не в списке, звонков ему не будет.
                unsafe {
                    self.release_slot(slot);
                    free_device(&device);
                }
                return Err(failure);
            }
        };

        let is_hub = device.hub.is_some();
        self.devices.push(device);
        // SAFETY: устройство настроено, кольцо точки прерываний готово.
        unsafe { self.queue_reports() };
        if is_hub {
            // SAFETY: хаб поднят и стоит в списке устройств.
            let (keyboard, mouse, _) = unsafe { self.scan_hub(slot) };
            return Ok((keyboard, mouse));
        }
        Ok((protocol == usb::PROTOCOL_KEYBOARD, protocol == usb::PROTOCOL_MOUSE))
    }

    /// Адрес, дескрипторы и конфигурация — как хаба или как HID. Возвращает
    /// протокол HID (у хаба — ноль).
    ///
    /// # Safety
    ///
    /// См. [`Controller::attach_device`]; слот выделен.
    unsafe fn bring_up(&mut self, device: &mut Device) -> Result<u8, (Stage, XhciError)> {
        let (slot, speed) = (device.slot, device.speed);
        // SAFETY: контракт функции; контексты выделены и обнулены.
        unsafe { self.address_device(device, speed) }.map_err(|err| (Stage::Address, err))?;
        kprintln!("  usb         : slot {slot} addressed");

        // SAFETY: устройство отвечает на управляющей точке.
        let found = match unsafe { self.describe_device(device, speed) }
            .map_err(|err| (Stage::Describe, err))?
        {
            Described::Hid(found) => found,
            Described::Hub(hub) => {
                // SAFETY: см. выше.
                unsafe { self.bring_up_hub(device, &hub) }.map_err(|err| (Stage::Configure, err))?;
                return Ok(0);
            }
        };
        let micros = interval_micros(endpoint_interval(speed, found.interval));
        kprintln!(
            "  usb         : HID interface {}, endpoint {} IN, {}-byte reports every {}.{:03} ms{}",
            found.interface,
            found.endpoint,
            found.max_packet_size,
            micros / 1000,
            micros % 1000,
            if found.boot { ", boot protocol offered" } else { "" }
        );

        device.report_len = found.max_packet_size.min(PAGE_SIZE as u16);
        device.described_by = found.report_len;
        device.interface = (found.interface, found.interfaces);
        // SAFETY: см. выше.
        unsafe {
            self.configure_endpoint(
                device,
                found.endpoint,
                found.interval,
                found.max_packet_size,
                None,
            )
        }
        .map_err(|err| (Stage::Configure, err))?;
        // SAFETY: см. выше.
        let reader =
            unsafe { self.enable_reports(device, &found) }.map_err(|err| (Stage::Enable, err))?;
        let protocol = reader.protocol();
        kprintln!("  usb         : slot {slot} is a {}", reader.name());

        // Разборщик ставится последним: до этого момента буфер занят
        // дескрипторами, и отчёт в него ещё не запрашивался.
        device.reader = Some(reader);
        Ok(protocol)
    }

    /// Поднять хаб: конфигурация, дескриптор хаба, точка прерываний, питание
    /// портов.
    ///
    /// # Safety
    ///
    /// Устройство адресовано и описано.
    unsafe fn bring_up_hub(&mut self, device: &mut Device, found: &HubInterface) -> Result<(), XhciError> {
        if device.speed >= regs::SPEED_SUPER {
            return Err(XhciError::SuperSpeedHub);
        }
        if device.place.depth >= HUB_DEPTH_MAX {
            return Err(XhciError::TooDeep);
        }
        // SAFETY: контракт функции.
        unsafe {
            self.control_transfer(
                device,
                [0, usb::REQ_SET_CONFIGURATION, found.configuration, 0, 0, 0, 0, 0],
                0,
                false,
            )
        }?;
        // SAFETY: хаб сконфигурирован.
        let read = unsafe {
            self.control_transfer(
                device,
                [HUB_FROM_HUB, usb::REQ_GET_DESCRIPTOR, 0, DESC_HUB, 0, 0, 9, 0],
                9,
                true,
            )
        }?;
        if read < 7 {
            return Err(XhciError::HubDescriptor);
        }
        // SAFETY: буфер выделен на страницу, прочитано `read` байт.
        let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
        let ports = bytes[2].min(15);
        // Время «обдумывания» транслятора (биты 6:5 `wHubCharacteristics`) —
        // контроллер должен знать его у высокоскоростного хаба.
        let think_time = (u32::from(u16::from_le_bytes([bytes[3], bytes[4]])) >> 5) & 0b11;
        let power_good = u64::from(bytes[5]) * 2;

        // SAFETY: см. выше.
        unsafe {
            self.configure_endpoint(
                device,
                found.endpoint,
                found.interval,
                found.max_packet_size,
                Some((ports, if device.speed == regs::SPEED_HIGH { think_time } else { 0 })),
            )
        }?;
        for port in 1..=ports {
            // SAFETY: см. выше.
            let _ = unsafe {
                self.control_transfer(
                    device,
                    [HUB_TO_PORT, HUB_REQ_SET_FEATURE, FEATURE_PORT_POWER, 0, port, 0, 0, 0],
                    0,
                    false,
                )
            };
        }
        sleep_ms(power_good.max(HUB_POWER_SETTLE_MS));

        // Карта изменений: бит на хаб и по биту на порт.
        device.report_len = found.max_packet_size.clamp(1, 4);
        device.hub = Some(Hub { ports, connected: 0, changed: 0 });
        kprintln!(
            "  usb         : {}: hub {:04x}:{:04x} in slot {}, {}, {ports} port(s)",
            device.place.name(device.port),
            device.identity.0,
            device.identity.1,
            device.slot,
            regs::speed_name(device.speed)
        );
        Ok(())
    }

    /// Пройти все порты хаба. Возвращает новые клавиатуру и указатель и
    /// признак «что-то изменилось».
    ///
    /// # Safety
    ///
    /// Хаб в слоте `hub` поднят и стоит в списке устройств; вызов из задачи.
    unsafe fn scan_hub(&mut self, hub: u8) -> (bool, bool, bool) {
        let Some(ports) = self.find(hub).and_then(|d| d.hub).map(|h| h.ports) else {
            return (false, false, false);
        };
        let (mut keyboard, mut mouse, mut changed) = (false, false, false);
        for port in 1..=ports {
            // SAFETY: контракт функции.
            let (k, m, c) = unsafe { self.check_hub_port(hub, port) };
            keyboard |= k;
            mouse |= m;
            changed |= c;
        }
        (keyboard, mouse, changed)
    }

    /// Хабы, приславшие карту изменений, — по одному, все их порты.
    ///
    /// # Safety
    ///
    /// Вызов из задачи, контроллер работает.
    unsafe fn scan_changed_hubs(&mut self) -> (bool, bool, bool) {
        let hubs: Vec<(u8, u32)> = self
            .devices
            .iter()
            .filter_map(|d| d.hub.filter(|h| h.changed != 0).map(|h| (d.slot, h.changed)))
            .collect();
        let (mut keyboard, mut mouse, mut changed) = (false, false, false);
        for (hub, bits) in hubs {
            if let Some(state) = self.find_mut(hub).and_then(|d| d.hub.as_mut()) {
                state.changed = 0;
            }
            if bits & 1 != 0 {
                // Изменение у самого хаба (питание, перегрузка): признаки
                // снимаются, иначе хаб будет присылать карту без конца.
                // SAFETY: контракт функции.
                unsafe { self.clear_hub_changes(hub) };
            }
            // SAFETY: контракт функции; хаб из списка устройств.
            let (k, m, c) = unsafe { self.scan_hub(hub) };
            keyboard |= k;
            mouse |= m;
            changed |= c;
        }
        (keyboard, mouse, changed)
    }

    /// Снять признаки изменений самого хаба: `C_HUB_LOCAL_POWER` и
    /// `C_HUB_OVER_CURRENT`.
    ///
    /// # Safety
    ///
    /// См. [`Controller::scan_changed_hubs`].
    unsafe fn clear_hub_changes(&mut self, hub: u8) {
        // SAFETY: контракт функции.
        let Ok(read) = (unsafe {
            self.hub_control(hub, [HUB_FROM_HUB, HUB_REQ_GET_STATUS, 0, 0, 0, 0, 4, 0], 4, true)
        }) else {
            return;
        };
        let change = self.hub_word(hub, read, 2);
        for feature in 0..2u8 {
            if change & (1 << feature) != 0 {
                // SAFETY: см. выше.
                let _ = unsafe {
                    self.hub_control(hub, [HUB_TO_HUB, HUB_REQ_CLEAR_FEATURE, feature, 0, 0, 0, 0, 0], 0, false)
                };
            }
        }
    }

    /// Сверить один порт хаба с тем, что о нём известно, и поднять или забыть
    /// устройство. Возвращает новые клавиатуру и указатель и признак «что-то
    /// изменилось».
    ///
    /// # Safety
    ///
    /// См. [`Controller::scan_hub`].
    unsafe fn check_hub_port(&mut self, hub: u8, port: u8) -> (bool, bool, bool) {
        let nothing = (false, false, false);
        let Some(known) = self.find(hub).and_then(|d| d.hub).map(|h| h.connected & (1 << port) != 0)
        else {
            return nothing;
        };
        // SAFETY: контракт функции.
        let (status, change) = match unsafe { self.hub_port_status(hub, port) } {
            Ok(state) => state,
            Err(err) => {
                kprintln!("  usb         : hub {hub} port {port}: the port status could not be read: {err}");
                return nothing;
            }
        };
        // Все признаки изменений снимаются: оставленный признак хаб
        // повторял бы в карте изменений без конца.
        for bit in 0..5u8 {
            if change & (1 << bit) != 0 {
                // SAFETY: см. выше.
                let _ = unsafe { self.hub_port_feature(hub, port, false, FEATURE_C_PORT_FIRST + bit) };
            }
        }
        let connected = status & HUB_PORT_CONNECTION != 0;
        // Переподключение между двумя сверками: признак изменения стоит, а
        // устройство, которое мы знали, на порту уже другое.
        let replugged = known && connected && change & HUB_CHANGE_CONNECTION != 0;

        if known && (!connected || replugged) {
            // SAFETY: устройства на порту больше нет.
            unsafe { self.forget_subtree(hub, port) };
            self.set_hub_bit(hub, port, false);
            if !replugged {
                return (false, false, true);
            }
        }
        if !connected || (known && !replugged) {
            return nothing;
        }

        // Бит ставится до попытки: отказавшее устройство не должно
        // сбрасываться заново на каждой сверке.
        self.set_hub_bit(hub, port, true);
        kprintln!("  usb         : hub {hub} port {port}: connected, resetting");
        sleep_ms(PORT_DEBOUNCE_MS);
        // SAFETY: см. выше.
        let speed = match unsafe { self.reset_hub_port(hub, port) } {
            Ok(speed) => speed,
            Err(err) => {
                kprintln!("  usb         : hub {hub} port {port} failed while {}: {err}", Stage::Reset);
                let root = self.find(hub).map_or(0, |d| d.port);
                self.last_error = Some((root, Stage::Reset, err));
                return (false, false, true);
            }
        };
        let Some((root, place)) = self.find(hub).map(|d| (d.port, Place::child(d, port, speed))) else {
            return nothing;
        };
        kprintln!(
            "  usb         : device on hub {hub} port {port}, {}{}",
            regs::speed_name(speed),
            if place.tt.0 != 0 { ", through the hub's translator" } else { "" }
        );
        // SAFETY: порт хаба сброшен и разрешён.
        match unsafe { self.attach_device(root, place, speed) } {
            Ok((keyboard, mouse)) => (keyboard, mouse, true),
            Err((stage, err)) => {
                kprintln!("  usb         : hub {hub} port {port} failed while {stage}: {err}");
                self.last_error = Some((root, stage, err));
                (false, false, true)
            }
        }
    }

    fn set_hub_bit(&mut self, hub: u8, port: u8, on: bool) {
        if let Some(state) = self.find_mut(hub).and_then(|d| d.hub.as_mut()) {
            if on {
                state.connected |= 1 << port;
            } else {
                state.connected &= !(1 << port);
            }
        }
    }

    fn find(&self, slot: u8) -> Option<&Device> {
        self.devices.iter().find(|d| d.slot == slot)
    }

    fn find_mut(&mut self, slot: u8) -> Option<&mut Device> {
        self.devices.iter_mut().find(|d| d.slot == slot)
    }

    /// Слово из ответа хаба, лежащего в его буфере (`read` байт).
    fn hub_word(&self, hub: u8, read: usize, at: usize) -> u16 {
        match self.find(hub) {
            Some(device) if read >= at + 2 => {
                // SAFETY: буфер выделен на страницу; читается внутри ответа.
                let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
                u16::from_le_bytes([bytes[at], bytes[at + 1]])
            }
            _ => 0,
        }
    }

    /// Состояние порта хаба: `(status, change)`.
    ///
    /// # Safety
    ///
    /// Хаб сконфигурирован и стоит в списке устройств.
    unsafe fn hub_port_status(&mut self, hub: u8, port: u8) -> Result<(u16, u16), XhciError> {
        // SAFETY: контракт функции.
        let read = unsafe {
            self.hub_control(hub, [HUB_FROM_PORT, HUB_REQ_GET_STATUS, 0, 0, port, 0, 4, 0], 4, true)
        }?;
        if read < 4 {
            return Err(XhciError::ShortDescriptor);
        }
        Ok((self.hub_word(hub, read, 0), self.hub_word(hub, read, 2)))
    }

    /// `SET_FEATURE` или `CLEAR_FEATURE` у порта хаба.
    ///
    /// # Safety
    ///
    /// См. [`Controller::hub_port_status`].
    unsafe fn hub_port_feature(&mut self, hub: u8, port: u8, set: bool, feature: u8) -> Result<(), XhciError> {
        let request = if set { HUB_REQ_SET_FEATURE } else { HUB_REQ_CLEAR_FEATURE };
        // SAFETY: контракт функции.
        unsafe { self.hub_control(hub, [HUB_TO_PORT, request, feature, 0, port, 0, 0, 0], 0, false) }
            .map(|_| ())
    }

    /// Сбросить порт хаба и узнать скорость устройства на нём (номер скорости
    /// xHCI).
    ///
    /// # Safety
    ///
    /// См. [`Controller::hub_port_status`].
    unsafe fn reset_hub_port(&mut self, hub: u8, port: u8) -> Result<u32, XhciError> {
        // SAFETY: контракт функции.
        unsafe { self.hub_port_feature(hub, port, true, FEATURE_PORT_RESET) }?;
        let mut timeout = Timeout::new(PORT_RESET_TIMEOUT_MS);
        loop {
            sleep_ms(10);
            // SAFETY: см. выше.
            let (status, change) = unsafe { self.hub_port_status(hub, port) }?;
            if change & HUB_CHANGE_RESET != 0
                || (status & HUB_PORT_RESET == 0 && status & HUB_PORT_ENABLE != 0)
            {
                break;
            }
            if timeout.expired() {
                return Err(XhciError::PortResetTimeout(port));
            }
        }
        // SAFETY: см. выше.
        let _ = unsafe { self.hub_port_feature(hub, port, false, FEATURE_C_PORT_FIRST + 4) };
        sleep_ms(PORT_RECOVERY_MS);
        // SAFETY: см. выше.
        let (status, _) = unsafe { self.hub_port_status(hub, port) }?;
        if status & HUB_PORT_ENABLE == 0 {
            return Err(XhciError::PortResetTimeout(port));
        }
        Ok(if status & HUB_PORT_LOW_SPEED != 0 {
            regs::SPEED_LOW
        } else if status & HUB_PORT_HIGH_SPEED != 0 {
            regs::SPEED_HIGH
        } else {
            regs::SPEED_FULL
        })
    }

    /// Записать слово в контекст.
    ///
    /// `index` — номер контекста внутри структуры (0 — управляющий у входного,
    /// слот у выходного), `dword` — номер слова внутри контекста.
    ///
    /// # Safety
    ///
    /// Буфер должен быть выделен под нужное число контекстов.
    unsafe fn write_context(&self, buffer: &DmaBuffer, index: usize, dword: usize, value: u32) {
        let offset = index * self.regs.context_size + dword * 4;
        debug_assert!(offset + 4 <= buffer.len());
        // SAFETY: контракт функции; смещение внутри буфера.
        unsafe { buffer.as_ptr::<u8>().add(offset).cast::<u32>().write_volatile(value) };
    }

    /// # Safety
    ///
    /// См. [`Controller::write_context`].
    unsafe fn read_context(&self, buffer: &DmaBuffer, index: usize, dword: usize) -> u32 {
        let offset = index * self.regs.context_size + dword * 4;
        debug_assert!(offset + 4 <= buffer.len());
        // SAFETY: контракт функции.
        unsafe { buffer.as_ptr::<u8>().add(offset).cast::<u32>().read_volatile() }
    }

    /// Обнулить входной контекст.
    ///
    /// Обязательно перед каждой командой, которая его читает: контроллер разбирает
    /// структуру целиком, и оставшиеся от прошлой команды поля он истолкует как
    /// требование их применить.
    ///
    /// # Safety
    ///
    /// См. [`Controller::write_context`].
    unsafe fn clear_input(&self, device: &Device) {
        let words = device.input.len() / 4;
        for index in 0..words {
            // SAFETY: индекс внутри буфера.
            unsafe { device.input.as_ptr::<u32>().add(index).write_volatile(0) };
        }
    }

    /// Выдать устройству адрес: команда `Address Device`.
    ///
    /// # Safety
    ///
    /// Слот должен быть выделен, а контексты — принадлежать этому слоту.
    unsafe fn address_device(&mut self, device: &mut Device, speed: u32) -> Result<(), XhciError> {
        // SAFETY: контракт функции.
        unsafe { self.clear_input(device) };

        // Управляющий контекст: добавляем слот (бит 0) и точку 0 (бит 1).
        // SAFETY: см. выше.
        unsafe { self.write_context(&device.input, 0, 1, 0b11) };

        // Контекст слота. `Context Entries` = 1 означает «описана одна точка,
        // нулевая»; поле считает контексты, а не точки, поэтому единица здесь —
        // это EP0 и ничего больше.
        //
        // За хабом (фаза 68l) — ещё путь от корневого порта (route string) и,
        // у медленного устройства за быстрым хабом, транслятор: слот хаба и
        // номер его порта.
        let place = device.place;
        // SAFETY: см. выше.
        unsafe {
            self.write_context(&device.input, 1, 0, (1 << 27) | (speed << 20) | place.route);
            self.write_context(&device.input, 1, 1, u32::from(device.port) << 16);
            self.write_context(
                &device.input,
                1,
                2,
                u32::from(place.tt.0) | (u32::from(place.tt.1) << 8),
            );
        }

        // Контекст управляющей точки. `CErr = 3` — сколько раз контроллер
        // повторит передачу при ошибке шины; ноль означал бы «не повторять», и
        // единственная помеха на линии выглядела бы как отказ устройства.
        let max_packet = regs::default_max_packet_size(speed);
        let dequeue = device.ep0.phys() | if device.ep0.initial_cycle() { 1 } else { 0 };
        // SAFETY: см. выше.
        unsafe {
            self.write_context(&device.input, 2, 1, (u32::from(max_packet) << 16) | (4 << 3) | (3 << 1));
            self.write_context(&device.input, 2, 2, dequeue as u32);
            self.write_context(&device.input, 2, 3, (dequeue >> 32) as u32);
            // Средняя длина передачи: подсказка контроллеру для планирования
            // полосы. Для управляющей точки спецификация предписывает 8.
            self.write_context(&device.input, 2, 4, 8);
        }

        // SAFETY: контракт функции; входной контекст заполнен.
        unsafe {
            self.command_execute(Trb {
                parameter: device.input.phys().as_u64(),
                status: 0,
                control: (ring::TRB_ADDRESS_DEVICE << ring::TRB_TYPE_SHIFT)
                    | (u32::from(device.slot) << 24),
            })
        }?;
        Ok(())
    }

    /// Прочитать дескрипторы и понять, хаб это или HID.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn describe_device(
        &mut self,
        device: &mut Device,
        speed: u32,
    ) -> Result<Described, XhciError> {
        // Первые восемь байт дескриптора устройства: больше читать нельзя, пока
        // неизвестен размер пакета управляющей точки, — а он лежит именно в этих
        // восьми байтах (байт 7). Классическая курица с яйцом, решённая
        // спецификацией: чтение восьми байт обязано работать при любом размере
        // пакета.
        // SAFETY: контракт функции.
        let read = unsafe { self.get_descriptor(device, usb::DESC_DEVICE, 0, 8) }?;
        if read < 8 {
            return Err(XhciError::ShortDescriptor);
        }
        // SAFETY: буфер отчётов используется и как приёмник дескрипторов; он
        // выделен на страницу, читается ровно столько, сколько передано.
        let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
        let desc = usb::DeviceDescriptor::parse(bytes).ok_or(XhciError::ShortDescriptor)?;
        let max_packet = desc.max_packet_size0;
        kprintln!(
            "  usb         : USB {}.{:02x} device, {}-byte control packets",
            desc.usb_version >> 8,
            desc.usb_version & 0xFF,
            max_packet
        );

        // Размер пакета оказался не тем, что предполагалось по скорости, —
        // контроллеру надо сообщить настоящий, иначе следующая же передача
        // длиннее пакета разъедется по границам. Сравнивается именно с тем
        // значением, которое было записано в контекст (см. `address_device`), а
        // не с чем-то отдельно взятым: расхождение возможно только у low- и
        // full-speed, где предполагалось 8, а разрешены и 16, 32, 64.
        if u16::from(max_packet) != regs::default_max_packet_size(speed) {
            // SAFETY: см. выше.
            unsafe { self.update_max_packet(device, u16::from(max_packet)) }?;
        }

        // Полный дескриптор устройства — ради одних только идентификаторов.
        // Читается отдельной передачей, потому что первые восемь байт их не
        // содержат, а знать, кто перед нами, надо на любой машине: на той, где
        // журнала нет, это единственный способ отличить два разных устройства
        // от одного, увиденного дважды. Отказ здесь не смертелен — устройство
        // останется безымянным, и только.
        // Класс устройства (байт 4) — оттуда же: хаб объявляет себя им.
        let mut class = 0;
        // SAFETY: см. выше.
        if let Ok(read) = unsafe { self.get_descriptor(device, usb::DESC_DEVICE, 0, 18) } {
            // SAFETY: см. выше.
            let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
            if let Some(full) = usb::DeviceDescriptor::parse(bytes) {
                device.identity = (full.vendor, full.product);
            }
            if read >= 5 {
                class = bytes[4];
            }
        }

        // Дескриптор конфигурации: сначала девять байт, чтобы узнать полную
        // длину, потом всё целиком. Читать сразу «побольше» нельзя — устройство
        // вправе ответить ошибкой на запрос длиннее того, что у него есть.
        // SAFETY: см. выше.
        let read = unsafe { self.get_descriptor(device, usb::DESC_CONFIGURATION, 0, 9) }?;
        if read < 9 {
            return Err(XhciError::ShortDescriptor);
        }
        // SAFETY: см. выше.
        let total = {
            let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
            u16::from_le_bytes([bytes[2], bytes[3]])
        };
        let total = total.min(PAGE_SIZE as u16);

        // SAFETY: см. выше.
        let read = unsafe { self.get_descriptor(device, usb::DESC_CONFIGURATION, 0, total) }?;
        // SAFETY: см. выше.
        let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
        let interface_class =
            if bytes.len() >= 18 && bytes[10] == usb::DESC_INTERFACE { bytes[14] } else { 0 };
        if class == CLASS_HUB || interface_class == CLASS_HUB {
            return find_hub_endpoint(bytes).map(Described::Hub).ok_or(XhciError::HubDescriptor);
        }
        usb::find_hid(bytes).map(Described::Hid).ok_or(XhciError::NoHid)
    }

    /// Сообщить контроллеру настоящий размер пакета управляющей точки.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn update_max_packet(&mut self, device: &mut Device, size: u16) -> Result<(), XhciError> {
        // SAFETY: контракт функции.
        unsafe {
            self.clear_input(device);
            // Только точка 0 — слот не меняется.
            self.write_context(&device.input, 0, 1, 0b10);
            let dequeue = device.ep0.phys() | 1;
            self.write_context(&device.input, 2, 1, (u32::from(size) << 16) | (4 << 3) | (3 << 1));
            self.write_context(&device.input, 2, 2, dequeue as u32);
            self.write_context(&device.input, 2, 3, (dequeue >> 32) as u32);
            self.write_context(&device.input, 2, 4, 8);
        }
        // SAFETY: входной контекст заполнен.
        unsafe {
            self.command_execute(Trb {
                parameter: device.input.phys().as_u64(),
                status: 0,
                control: (ring::TRB_EVALUATE_CONTEXT << ring::TRB_TYPE_SHIFT)
                    | (u32::from(device.slot) << 24),
            })
        }?;
        Ok(())
    }

    /// Добавить точку прерываний: команда `Configure Endpoint`.
    ///
    /// `hub` — число портов и время транслятора, если устройство — хаб:
    /// контроллер должен знать это о хабе до первого устройства за ним.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано, а кольцо точки — существовать.
    unsafe fn configure_endpoint(
        &mut self,
        device: &mut Device,
        endpoint: u8,
        b_interval: u8,
        max_packet_size: u16,
        hub: Option<(u8, u32)>,
    ) -> Result<(), XhciError> {
        let speed = device.speed;
        // Идентификатор контекста точки: `2 * номер + 1` для направления IN. Он
        // же — цель дверного звонка.
        let target = endpoint * 2 + 1;
        device.interrupt_target = target;
        let context_index = usize::from(target);

        // SAFETY: контракт функции.
        unsafe {
            self.clear_input(device);
            // Добавляем слот (его `Context Entries` меняется) и нашу точку.
            self.write_context(&device.input, 0, 1, 1 | (1 << context_index));

            // Контекст слота копируется из выходного, а не собирается заново:
            // там уже лежит адрес устройства и скорость, выставленные
            // контроллером при `Address Device`, и обнулить их значило бы
            // отобрать у устройства адрес.
            for dword in 0..4 {
                let value = self.read_context(&device.context, 0, dword);
                self.write_context(&device.input, 1, dword, value);
            }
            // Кроме одного поля: число описанных контекстов должно покрывать
            // новую точку.
            let slot0 = self.read_context(&device.context, 0, 0);
            let entries = context_index as u32;
            let mut slot0 = (slot0 & !(0x1F << 27)) | (entries << 27);
            if let Some((ports, think_time)) = hub {
                // Флаг Hub, число портов, время транслятора.
                slot0 |= 1 << 26;
                let slot1 = self.read_context(&device.context, 0, 1);
                self.write_context(
                    &device.input,
                    1,
                    1,
                    (slot1 & !(0xFF << 24)) | (u32::from(ports) << 24),
                );
                let slot2 = self.read_context(&device.context, 0, 2);
                self.write_context(&device.input, 1, 2, (slot2 & !(0b11 << 16)) | (think_time << 16));
            }
            self.write_context(&device.input, 1, 0, slot0);

            let interval = endpoint_interval(speed, b_interval);
            let max_packet = u32::from(max_packet_size);
            // Тип точки 7 — Interrupt IN. `CErr = 3` — см. `address_device`.
            self.write_context(&device.input, context_index + 1, 0, u32::from(interval) << 16);
            self.write_context(
                &device.input,
                context_index + 1,
                1,
                (max_packet << 16) | (7 << 3) | (3 << 1),
            );
            let dequeue =
                device.interrupt.phys() | if device.interrupt.initial_cycle() { 1 } else { 0 };
            self.write_context(&device.input, context_index + 1, 2, dequeue as u32);
            self.write_context(&device.input, context_index + 1, 3, (dequeue >> 32) as u32);
            // Средняя длина передачи и максимальная нагрузка за интервал: для
            // точки прерываний это размер пакета.
            self.write_context(&device.input, context_index + 1, 4, max_packet | (max_packet << 16));
        }

        // SAFETY: входной контекст заполнен.
        unsafe {
            self.command_execute(Trb {
                parameter: device.input.phys().as_u64(),
                status: 0,
                control: (ring::TRB_CONFIGURE_ENDPOINT << ring::TRB_TYPE_SHIFT)
                    | (u32::from(device.slot) << 24),
            })
        }?;
        Ok(())
    }

    /// Выбрать конфигурацию, понять устройство и включить нужный протокол.
    ///
    /// Возвращает разборщик, которому достанутся отчёты.
    ///
    /// # Safety
    ///
    /// Точка прерываний должна быть уже настроена.
    unsafe fn enable_reports(
        &mut self,
        device: &mut Device,
        found: &HidInterface,
    ) -> Result<Reader, XhciError> {
        // SET_CONFIGURATION: до него устройство не отвечает ни на одном
        // интерфейсе, кроме нулевой точки. Дескриптор отчётов адресован именно
        // интерфейсу, поэтому читать его раньше этого запроса нельзя.
        // SAFETY: контракт функции.
        unsafe {
            self.control_transfer(
                device,
                [0, usb::REQ_SET_CONFIGURATION, found.configuration, 0, 0, 0, 0, 0],
                0,
                false,
            )
        }?;

        // SAFETY: устройство сконфигурировано, буфер отчётов ещё свободен —
        // разборщик поставят после возврата отсюда.
        let described = unsafe { self.read_report_descriptor(device, found) };
        let (reader, boot) = choose_reader(found, &described).ok_or(XhciError::UnknownHid)?;

        // Протокол запрашивается явно, и именно тот, на котором собрались
        // читать. Запрос имеет смысл только у интерфейса с boot-подклассом: у
        // остальных протокол один, менять нечего, и устройство законно ответит
        // отказом.
        let request = usb::REQ_TYPE_CLASS | usb::REQ_RECIPIENT_INTERFACE;
        if found.boot {
            let wanted = if boot { usb::HID_PROTOCOL_BOOT } else { usb::HID_PROTOCOL_REPORT };
            // SAFETY: см. выше.
            let protocol = unsafe {
                self.control_transfer(
                    device,
                    [
                        request,
                        usb::REQ_HID_SET_PROTOCOL,
                        wanted as u8,
                        0,
                        found.interface,
                        0,
                        0,
                        0,
                    ],
                    0,
                    false,
                )
            };
            if protocol.is_err() {
                // Отказ не смертелен: устройство могло не поддерживать запрос,
                // оставаясь при этом в нужном протоколе (report — состояние по
                // умолчанию после сброса). Молчать нельзя: если отчёты потом
                // окажутся бессмыслицей, причина будет здесь.
                kprintln!(
                    "  usb         : SET_PROTOCOL({}) refused; assuming the device is in it anyway",
                    if boot { "boot" } else { "report" }
                );
            }
        }

        // SET_IDLE с нулевой длительностью означает «сообщать только об
        // изменениях». Без него клавиатура повторяет отчёт каждые несколько
        // миллисекунд, и ядро тратит время на разбор одного и того же.
        // SAFETY: см. выше.
        let idle = unsafe {
            self.control_transfer(
                device,
                [request, usb::REQ_HID_SET_IDLE, 0, 0, found.interface, 0, 0, 0],
                0,
                false,
            )
        };
        if idle.is_err() {
            kprintln!("  usb         : SET_IDLE refused; reports may repeat");
        }
        Ok(reader)
    }

    /// Прочитать и разобрать дескриптор отчётов.
    ///
    /// Неудача здесь не является отказом: у устройства с boot-подклассом
    /// остаётся запасной формат, и разбор превращается в необязательное
    /// улучшение. Поэтому функция ничего не возвращает в виде ошибки — она
    /// возвращает то, что удалось понять, и печатает это.
    ///
    /// # Safety
    ///
    /// Устройство должно быть сконфигурировано, а буфер отчётов — свободен.
    unsafe fn read_report_descriptor(
        &mut self,
        device: &mut Device,
        found: &HidInterface,
    ) -> usb_hid::Descriptor {
        if found.report_len == 0 {
            kprintln!("  usb         : the interface declares no report descriptor");
            return usb_hid::Descriptor::default();
        }

        let length = found.report_len.min(PAGE_SIZE as u16);
        // Получатель — **интерфейс**, а не устройство: дескриптор отчётов
        // принадлежит интерфейсу, и запрос к устройству вернёт отказ. Номер
        // интерфейса едет в `wIndex`, тип — в старшем байте `wValue`.
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
        let read = match unsafe { self.control_transfer(device, setup, length, true) } {
            Ok(read) => read,
            Err(err) => {
                kprintln!("  usb         : the report descriptor could not be read: {err}");
                return usb_hid::Descriptor::default();
            }
        };

        // SAFETY: буфер выделен на страницу, читается ровно столько, сколько
        // сообщил контроллер.
        let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
        let parsed = usb_hid::parse(bytes);

        // Разобранное печатается целиком, и это не многословность. Дескриптор
        // приходит от чужого устройства, а ошибка разбора выглядит как «курсор
        // ездит наискось» — то есть как неисправная мышь. Строка ниже отвечает
        // на вопрос «что именно ядро поняло» до того, как его зададут.
        match parsed.pointer {
            Some(map) if map.is_absolute() => {
                let (min, max) = map.range();
                kprintln!(
                    "  usb         : report descriptor {read} bytes: pointer, absolute {min}..{max}, {} buttons{}",
                    map.button_count(),
                    if map.has_wheel() { ", wheel" } else { "" }
                );
            }
            Some(map) => kprintln!(
                "  usb         : report descriptor {read} bytes: pointer, relative, {} buttons{}",
                map.button_count(),
                if map.has_wheel() { ", wheel" } else { "" }
            ),
            None => {}
        }
        if let Some(map) = parsed.keyboard {
            kprintln!(
                "  usb         : report descriptor {read} bytes: keyboard, {}, {}-key array",
                if map.has_modifiers() { "modifiers" } else { "no modifiers" },
                map.key_slots()
            );
        }
        if parsed.pointer.is_none() && parsed.keyboard.is_none() {
            kprintln!("  usb         : report descriptor {read} bytes: nothing the kernel can use");
        }

        parsed
    }

    /// Прочитать дескриптор. Возвращает число полученных байт; данные лежат в
    /// `device.report`.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn get_descriptor(
        &mut self,
        device: &mut Device,
        kind: u8,
        index: u8,
        length: u16,
    ) -> Result<usize, XhciError> {
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
        unsafe { self.control_transfer(device, setup, length, true) }
    }

    /// Выполнить передачу по управляющей точке.
    ///
    /// `length` — сколько байт данных, `is_in` — направление. Данные всегда идут
    /// через `device.report`: другого буфера под управляющие передачи не нужно,
    /// а один общий делает невозможной ошибку «прочитали в буфер, который уже
    /// занят отчётом» — отчёты начинают приходить только после того, как все
    /// управляющие передачи закончены.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано, а кольцо точки 0 — сообщено
    /// контроллеру.
    unsafe fn control_transfer(
        &mut self,
        device: &mut Device,
        setup: [u8; 8],
        length: u16,
        is_in: bool,
    ) -> Result<usize, XhciError> {
        let last = push_control(device, setup, length, is_in);
        // SAFETY: контракт функции.
        unsafe { self.finish_control(device.slot, last, length) }
    }

    /// Управляющая передача хабу, который уже стоит в списке устройств.
    ///
    /// Хаб из списка не вынимается: его точка прерываний в это время может
    /// завершиться, и событие должно найти устройство на месте. Данные — в
    /// начале его буфера; карта изменений лежит дальше
    /// ([`HUB_STATUS_OFFSET`]).
    ///
    /// # Safety
    ///
    /// Хаб сконфигурирован.
    unsafe fn hub_control(
        &mut self,
        hub: u8,
        setup: [u8; 8],
        length: u16,
        is_in: bool,
    ) -> Result<usize, XhciError> {
        let device = self.find_mut(hub).ok_or(XhciError::NoDevice)?;
        let last = push_control(device, setup, length, is_in);
        // SAFETY: контракт функции.
        unsafe { self.finish_control(hub, last, length) }
    }

    /// Позвонить управляющей точке и дождаться события о последнем
    /// дескрипторе передачи.
    ///
    /// # Safety
    ///
    /// Дескрипторы передачи уже в кольце точки 0 этого слота.
    unsafe fn finish_control(&mut self, slot: u8, last: u64, length: u16) -> Result<usize, XhciError> {
        // SAFETY: дескрипторы записаны целиком; цель 1 — управляющая точка.
        unsafe { self.regs.ring_doorbell(slot, 1) };

        let mut timeout = Timeout::new(TRANSFER_TIMEOUT_MS);
        loop {
            // SAFETY: контракт функции.
            if let Some(event) = unsafe { self.drain_events(Some(last)) } {
                if !event.is_success() {
                    return Err(XhciError::TransferFailed { code: event.completion_code() });
                }
                // Контроллер сообщает остаток, а не переданное: сколько байт
                // **не** доехало. Для дескрипторов короткий ответ — норма, а не
                // ошибка, поэтому длина считается вычитанием.
                let residual = event.residual().min(u32::from(length));
                return Ok((u32::from(length) - residual) as usize);
            }
            if timeout.expired() {
                let (waited_ms, spun_out) = timeout.report();
                return Err(XhciError::TransferTimeout { waited_ms, spun_out });
            }
        }
    }
}

/// Записать в кольцо точки 0 дескрипторы управляющей передачи. Возвращает
/// адрес последнего — о нём придёт событие.
fn push_control(device: &mut Device, setup: [u8; 8], length: u16, is_in: bool) -> u64 {
    /// Тип передачи в дескрипторе Setup Stage: нет данных.
    const TRT_NO_DATA: u32 = 0 << 16;
    /// Данные от устройства к хосту.
    const TRT_IN: u32 = 3 << 16;
    /// Данные от хоста к устройству.
    const TRT_OUT: u32 = 2 << 16;
    /// Бит 16 дескрипторов Data/Status Stage: направление IN.
    const DIR_IN: u32 = 1 << 16;

    let trt = if length == 0 {
        TRT_NO_DATA
    } else if is_in {
        TRT_IN
    } else {
        TRT_OUT
    };

    device.ep0.push(Trb {
        // Восемь байт запроса едут в самом дескрипторе, а не по ссылке —
        // отсюда флаг `IDT`. Иначе под них понадобился бы отдельный буфер
        // DMA на каждую передачу.
        parameter: u64::from_le_bytes(setup),
        status: 8,
        control: (ring::TRB_SETUP_STAGE << ring::TRB_TYPE_SHIFT) | ring::TRB_IDT | trt,
    });

    if length > 0 {
        device.ep0.push(Trb {
            parameter: device.report.phys().as_u64(),
            status: u32::from(length),
            control: (ring::TRB_DATA_STAGE << ring::TRB_TYPE_SHIFT)
                | if is_in { DIR_IN } else { 0 },
        });
    }

    // Дескриптор состояния — единственный с флагом `IOC`: событие нужно одно,
    // о завершении всей передачи. Его направление противоположно данным (у
    // передачи без данных — IN).
    let status_in = length == 0 || !is_in;
    device.ep0.push(Trb {
        parameter: 0,
        status: 0,
        control: (ring::TRB_STATUS_STAGE << ring::TRB_TYPE_SHIFT)
            | ring::TRB_IOC
            | if status_in { DIR_IN } else { 0 },
    })
}

impl Controller {
    /// Поставить в кольцо запрос отчёта.
    ///
    /// # Safety
    ///
    /// Точка прерываний должна быть настроена.
    unsafe fn queue_reports(&mut self) {
        for index in 0..self.devices.len() {
            let Some(device) = self.devices.get_mut(index) else {
                continue;
            };
            // Пока разборщика нет, устройство ещё описывается, и его буфер занят
            // дескрипторами: запрос отчёта затёр бы их на полпути. Хаб ждёт
            // карту изменений во второй половине буфера.
            if device.queued || (device.reader.is_none() && device.hub.is_none()) {
                continue;
            }
            let offset = if device.hub.is_some() { HUB_STATUS_OFFSET } else { 0 };
            device.interrupt.push(Trb {
                parameter: device.report.phys().as_u64() + offset as u64,
                status: u32::from(device.report_len),
                control: (ring::TRB_NORMAL << ring::TRB_TYPE_SHIFT)
                    | ring::TRB_IOC
                    // Короткий пакет тоже должен породить событие: устройство
                    // вправе ответить меньше запрошенного, и без этого флага
                    // такой отчёт остался бы незамеченным.
                    | (1 << 2),
            });
            device.queued = true;
            let (slot, target) = (device.slot, device.interrupt_target);
            // SAFETY: дескриптор записан целиком до звонка.
            unsafe { self.regs.ring_doorbell(slot, target) };
        }
    }

    /// Разобрать событие о завершении передачи.
    fn handle_transfer_event(&mut self, event: &Trb) {
        // Слот — единственное, что отличает отчёт мыши от отчёта клавиатуры:
        // кольцо событий у контроллера одно на все устройства. Пока устройство
        // было одно, номер слота можно было не смотреть; теперь его пропуск
        // означал бы, что движение мыши разбирается как нажатие клавиш.
        let slot = event.slot_id();
        let Some(device) = self.devices.iter_mut().find(|device| device.slot == slot) else {
            return;
        };
        // Событие относится к точке прерываний? Идентификатор точки — биты 20:16
        // управляющего слова.
        let endpoint = ((event.control >> 16) & 0x1F) as u8;
        if endpoint != device.interrupt_target {
            return;
        }

        device.queued = false;
        if !event.is_success() {
            // Счётчик, а не восстановление, и об этом стоит сказать прямо. Часть
            // кодов завершения (в первую очередь Stall) означает, что точка
            // остановлена, и новый дескриптор в её кольце контроллер исполнять не
            // станет, пока не придёт команда `Reset Endpoint`. Реализовать её
            // несложно, но проверить нечем: ни QEMU, ни живая клавиатура на
            // boot-протоколе так не отвечают, а восстановление, которое ни разу
            // не исполнялось, — это не восстановление. Ошибки видны в `usb`.
            self.transfer_errors += 1;
            return;
        }

        let received = (u32::from(device.report_len) - event.residual().min(u32::from(device.report_len))) as usize;
        if let Some(hub) = device.hub.as_mut() {
            // Карта изменений хаба: разбирать её здесь нельзя (запросы к
            // хабу — это управляющие передачи), только запомнить и поставить
            // признак, по которому задача сверит порты.
            // SAFETY: буфер выделен на страницу, карта — не длиннее 4 байт.
            let bytes = unsafe {
                core::slice::from_raw_parts(device.report.as_ptr::<u8>().add(HUB_STATUS_OFFSET), received)
            };
            for (index, byte) in bytes.iter().enumerate() {
                hub.changed |= u32::from(*byte) << (8 * index);
            }
            self.ports_changed = true;
            return;
        }
        // SAFETY: буфер отчёта выделен на страницу; читается ровно столько, сколько
        // сообщил контроллер. `volatile` не нужен — передача уже завершена, и
        // содержимое больше не меняется, а отображение некешируемое.
        let report = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), received) };
        if let Some(reader) = device.reader.as_mut() {
            reader.handle_report(report);
        }
    }

    /// Перевести контроллер с опроса на прерывания.
    ///
    /// Путь выбирается в порядке предпочтения: MSI-X, MSI, линия `INTx` (фаза
    /// 68k). У xHCI VirtualBox (`8086:1e31`, как у Intel 7-й серии) MSI-X нет
    /// вовсе, и до 68k он оставался на опросе сто раз в секунду.
    ///
    /// Порядок здесь важнее обычного, и он такой: сперва обработчик и адреса
    /// для подтверждения, потом путь доставки, и только затем разрешение у
    /// самого контроллера. Обратный порядок означал бы прерывание, которому
    /// некуда прийти, — а для уровневой линии ещё и не снятое никем.
    ///
    /// Молчит, если ни один путь не сложился: тогда драйвер остаётся на опросе,
    /// и об этом сообщает [`Controller::summary`]. Отказываться от работающей
    /// клавиатуры из-за отсутствия прерываний было бы странно.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть запущен, а кольцо событий — рабочим: прерывание
    /// может прийти немедленно после последней записи.
    unsafe fn enable_interrupts(&mut self) {
        let (status, iman) = self.regs.interrupt_registers();
        let ack = &ACK[self.index];
        ack.status.store(status, Ordering::Release);
        ack.iman.store(iman, Ordering::Release);

        // SAFETY: контракт функции.
        let delivery = unsafe {
            if self.device.msix().is_some() {
                self.enable_msix()
            } else if self.device.msi().is_some() {
                self.enable_msi()
            } else {
                self.enable_intx()
            }
        };
        let Some(delivery) = delivery else {
            return;
        };
        ack.delivery.store(delivery as u8, Ordering::Release);

        // Разрешение у контроллера — двумя битами в разных регистрах: у
        // прерывателя и общий. Нужны оба; забытый второй выглядит как
        // «прерывания настроены, но не приходят».
        // SAFETY: контракт функции.
        unsafe {
            self.regs
                .write_interrupter32(0, regs::IR_IMAN, regs::IMAN_INTERRUPT_ENABLE);
            let command = self.regs.read_op32(regs::OP_USBCMD);
            self.regs.write_op32(regs::OP_USBCMD, command | regs::USBCMD_INTE);
        }
    }

    /// MSI-X: вектор 0 таблицы.
    ///
    /// # Safety
    ///
    /// См. [`Controller::enable_interrupts`].
    unsafe fn enable_msix(&mut self) -> Option<Delivery> {
        let msix = self.device.msix()?;
        let Some((address, data)) = crate::arch::interrupts::alloc_msi(on_interrupt) else {
            kprintln!("  xhci        : no MSI target on this machine; events will be polled");
            return None;
        };
        let Some(bar) = self.device.memory_bar(msix.bir) else {
            kprintln!("  xhci        : MSI-X table lives in BAR{} which is empty", msix.bir);
            return None;
        };

        // Таблица лежит в том же BAR, что и регистры, но не обязана: смещение
        // берётся из самой возможности, а не предполагается нулевым.
        let table_page = crate::mm::PhysAddr::new(bar.as_u64() + u64::from(msix.table_offset));
        // SAFETY: адрес получен из BAR устройства; окну нужна Device-семантика,
        // как и остальным его регистрам.
        let table = match unsafe { map_bar(table_page) } {
            Ok(table) => table,
            Err(err) => {
                kprintln!("  xhci        : cannot map the MSI-X table ({err:?}); events will be polled");
                return None;
            }
        };

        // SAFETY: таблица отображена, индекс 0 существует всегда (векторов не
        // бывает ноль), обработчик уже стоит — на x86-64 он в IDT с загрузки, на
        // AArch64 его поставил `alloc_msi`.
        unsafe { self.device.set_msix_vector(&msix, table, 0, address, data) };
        kprintln!(
            "  xhci        : MSI-X vector 0 -> {address:#018x} data {data:#x}, {} vectors available",
            msix.vectors
        );
        Some(Delivery::MsiX)
    }

    /// MSI: одно сообщение, как у AHCI.
    ///
    /// # Safety
    ///
    /// См. [`Controller::enable_interrupts`].
    unsafe fn enable_msi(&mut self) -> Option<Delivery> {
        let msi = self.device.msi()?;
        let Some((address, data)) = crate::arch::interrupts::alloc_msi(on_interrupt) else {
            kprintln!("  xhci        : no MSI target on this machine; events will be polled");
            return None;
        };
        // SAFETY: возможность получена от этого же устройства, обработчик
        // поставлен `alloc_msi` выше.
        unsafe { self.device.set_msi_vector(&msi, address, data) };
        kprintln!("  xhci        : no MSI-X; MSI -> {address:#018x} data {data:#x}");
        Some(Delivery::Msi)
    }

    /// Линия `INTx` — уровневая и, возможно, общая с соседями.
    ///
    /// Признаки, оставшиеся с запуска, снимаются до разрешения линии: иначе
    /// уровень поднялся бы за событие, которого задача ещё не ждёт. Сам
    /// контроллер не поднимет линию, пока не разрешён `IMAN.IE` — это делает
    /// вызывающий после возврата.
    ///
    /// # Safety
    ///
    /// См. [`Controller::enable_interrupts`].
    unsafe fn enable_intx(&mut self) -> Option<Delivery> {
        // SAFETY: контракт функции.
        unsafe {
            self.regs.write_op32(regs::OP_USBSTS, regs::USBSTS_EVENT_INTERRUPT);
            self.regs
                .write_interrupter32(0, regs::IR_IMAN, regs::IMAN_INTERRUPT_PENDING);
        }
        let Some(gsi) = crate::irq::routing::request(&self.device, on_interrupt) else {
            kprintln!("  xhci        : no MSI-X, no MSI and no interrupt line known; events will be polled");
            return None;
        };
        kprintln!("  xhci        : no MSI-X or MSI; INTx on GSI {gsi}");
        Some(Delivery::Intx)
    }

    /// Разобрать накопившиеся события и снова запросить отчёты.
    ///
    /// Вызывается из задачи — по прерыванию или по часам.
    pub fn service(&mut self) {
        self.services += 1;
        // SAFETY: окно регистров отображено на всё время жизни контроллера.
        unsafe {
            self.drain_events(None);
            self.queue_reports();
        }
    }

    /// Не изменился ли состав портов с прошлой сверки.
    ///
    /// Дёшево (чтение одного регистра на порт) и сознательно дублирует событие
    /// об изменении состояния порта: событие может не дойти — например, потому
    /// что прерывания от контроллера на этой машине не работают вовсе и кольцо
    /// разбирается опросом с задержкой, — а маска не соврёт никогда.
    ///
    /// # Safety
    ///
    /// Окно регистров должно быть отображено.
    unsafe fn ports_differ(&mut self, compare_mask: bool) -> bool {
        // Признак от события проверяется первым и без всяких условий: он уже
        // стоит, читать ради него регистры незачем, а отложить его до срока
        // сверки значит потерять — сверка обнулит маску и разницы не увидит.
        if self.ports_changed {
            return true;
        }
        if !compare_mask {
            return false;
        }
        let known = self.connected;
        // SAFETY: контракт функции.
        known != unsafe { self.connected_mask() }
    }

    /// Сводка для диагностики: устройства, отчёты, события, ошибки.
    #[must_use]
    pub fn summary(&self) -> Summary {
        let mut reports = 0;
        for device in &self.devices {
            reports += device.reader.as_ref().map_or(0, Reader::reports);
        }
        let mut attached = [Attached::default(); usb::ATTACHED_MAX];
        let functions = || self.devices.iter().filter(|device| device.hub.is_none());
        for (slot, device) in attached.iter_mut().zip(functions()) {
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

        let keyboards = self
            .devices
            .iter()
            .filter(|device| matches!(device.reader, Some(Reader::Keyboard(_))))
            .count();
        let mice = self
            .devices
            .iter()
            .filter(|device| matches!(device.reader, Some(Reader::Mouse(_))))
            .count();
        Summary {
            devices: functions().count(),
            hubs: self.devices.len() - functions().count(),
            keyboards,
            mice,
            occupied: self.occupied,
            last_error: self.last_error,
            attached,
            slot: functions().next().map_or(0, |device| device.slot),
            port: functions().next().map_or(0, |device| device.port),
            reports,
            events: self.events_seen,
            errors: self.transfer_errors,
            event_floods: self.event_floods,
            interrupts: ACK[self.index].count.load(Ordering::Relaxed),
            delivery: self.delivery().name(),
            controllers: 1,
            services: self.services,
        }
    }
}

/// Что драйвер сообщает о себе наружу.
///
/// Структура, а не кортеж из шести чисел: с появлением второго устройства
/// читать `summary().3` стало нельзя без подглядывания в объявление.
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    /// Сколько устройств поднято, не считая хабов.
    pub devices: usize,
    /// Сколько хабов пройдено (фаза 68l).
    pub hubs: usize,
    /// Сколько из них разбираются как клавиатуры.
    ///
    /// Отдельно от [`Summary::devices`] потому, что «устройство поднялось» и
    /// «устройство опознано тем, чем оно является» — разные утверждения, и
    /// расхождение между ними это самый частый отказ на чужой машине.
    pub keyboards: usize,
    /// Сколько из них разбираются как указатели.
    pub mice: usize,
    /// Сколько корневых портов заняты. Больше, чем [`Summary::devices`], —
    /// значит устройство на порту есть, а поднять его не удалось.
    pub occupied: usize,
    /// Чем закончилась последняя неудачная попытка: порт, шаг и ошибка.
    /// `None` — неудач не было.
    pub last_error: Option<(u8, Stage, XhciError)>,
    /// По записи на поднятое устройство, в порядке портов.
    ///
    /// Массив, а не вектор: сводку спрашивают в том числе из мест, где нельзя
    /// выделять память, и четырёх записей хватает — столько же слотов драйвер
    /// просит у контроллера.
    pub attached: [Attached; usb::ATTACHED_MAX],
    /// Слот первого из них.
    pub slot: u8,
    /// Порт первого из них.
    pub port: u8,
    /// Сколько отчётов разобрано суммарно.
    pub reports: u64,
    /// Сколько событий пришло от контроллера.
    pub events: u64,
    /// Сколько передач завершилось ошибкой.
    pub errors: u64,
    /// Сколько раз разбор кольца событий упёрся в предел за один проход.
    /// Отличное от нуля значение — признак, ради которого предел и введён.
    pub event_floods: u64,
    /// Сколько прерываний пришло от контроллера. Ноль при работающей
    /// клавиатуре означает, что события забирает опрос.
    pub interrupts: u64,
    /// Каким путём: `MSI-X`, `MSI`, `INTx`, `polled`; у нескольких
    /// контроллеров с разными путями — `mixed`.
    pub delivery: &'static str,
    /// Сколько контроллеров xHCI поднято (фаза 68m).
    pub controllers: usize,
    /// Сколько раз задача просыпалась разбирать кольцо.
    pub services: u64,
}

/// Вернуть память устройства.
///
/// Память возвращается **после** освобождения слота, и порядок здесь не
/// формальность: пока слот жив, контроллер вправе читать контекст устройства и
/// его кольца. Вернув буферы раньше, мы отдали бы их следующему просителю, а
/// контроллер продолжал бы разбирать дескрипторы в чужой памяти.
///
/// # Safety
///
/// Слот устройства освобождён, дверной звонок ему больше не звонит, и никаких
/// дескрипторов в его кольцах не осталось.
unsafe fn free_device(device: &Device) {
    // SAFETY: контракт функции.
    unsafe {
        dma::free(&device.context);
        dma::free(&device.input);
        dma::free(device.ep0.buffer());
        dma::free(device.interrupt.buffer());
        dma::free(&device.report);
    }
}

/// Найти в конфигурации хаба его точку прерываний IN (карта изменений).
fn find_hub_endpoint(bytes: &[u8]) -> Option<HubInterface> {
    let configuration = *bytes.get(5)?;
    let mut at = 0;
    while at + 2 <= bytes.len() {
        let length = usize::from(bytes[at]);
        if length < 2 || at + length > bytes.len() {
            return None;
        }
        // Первая точка — у альтернативы 0, с одним транслятором.
        if bytes[at + 1] == usb::DESC_ENDPOINT && length >= 7 && bytes[at + 2] & 0x80 != 0 {
            return Some(HubInterface {
                configuration,
                endpoint: bytes[at + 2] & 0x0F,
                max_packet_size: u16::from_le_bytes([bytes[at + 4], bytes[at + 5]]) & 0x7FF,
                interval: bytes[at + 6],
            });
        }
        at += length;
    }
    None
}

/// Перевести `bInterval` из дескриптора в поле `Interval` контекста точки.
///
/// Контроллер всегда считает интервал показателем двойки от 125 микросекунд:
/// период равен `2^Interval * 125 мкс`. А вот `bInterval` в дескрипторе значит
/// разное в зависимости от скорости — и это не тонкость, а место, где легко
/// ошибиться в восемь раз в любую сторону:
///
/// * у low- и full-speed это **число миллисекунд** (1..255), поэтому показатель
///   получается как `3 + log2(bInterval)`: 2³ · 125 мкс и есть 1 мс;
/// * у high-speed и выше `bInterval` **сам является показателем**, но отсчёт у
///   него с единицы, а не с нуля, — отсюда вычитание.
const fn endpoint_interval(speed: u32, b_interval: u8) -> u8 {
    match speed {
        regs::SPEED_HIGH | regs::SPEED_SUPER => b_interval.saturating_sub(1),
        _ => {
            let ms = if b_interval == 0 { 1 } else { b_interval };
            3 + log2_floor(ms as u32) as u8
        }
    }
}

/// Период опроса точки в микросекундах — только для диагностики.
const fn interval_micros(interval: u8) -> u32 {
    // Сдвиг ограничен: поле шестибитное по спецификации, но приходит из
    // дескриптора устройства, а сдвиг на 32 и больше — это паника.
    let interval = if interval > 15 { 15 } else { interval };
    125u32 << (interval as u32)
}

/// Целая часть двоичного логарифма.
const fn log2_floor(value: u32) -> u32 {
    if value == 0 { 0 } else { 31 - value.leading_zeros() }
}

/// Отобразить окно регистров контроллера.
///
/// Окно xHCI занимает больше страницы: блок Runtime у QEMU лежит по смещению
/// `0x600`, а массив дверных звонков — по `0x800`, но на реальных контроллерах
/// смещения доходят до десятков килобайт. Отображаются 64 КиБ — столько
/// спецификация отводит под весь набор регистров вместе с расширенными
/// возможностями.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
unsafe fn map_bar(bar: PhysAddr) -> Result<usize, MapError> {
    /// Сколько отображать под регистры контроллера.
    const WINDOW: usize = 64 * 1024;

    let page = bar.page_align_down();
    let virt = page.to_direct_map();
    let flags = crate::mm::PageFlags::READ | crate::mm::PageFlags::WRITE | crate::mm::PageFlags::DEVICE;
    // SAFETY: условия делегированы вызывающему; прямое отображение взаимно
    // однозначно, поэтому эти адреса не могут пересечься с кодом или стеком.
    unsafe { crate::arch::map_active(virt, page, WINDOW, flags) }?;
    Ok(virt.as_usize() + (bar.as_u64() - page.as_u64()) as usize)
}

/// Контроллеры, которыми ядро распоряжается (фаза 68m: все, а не первый).
///
/// Глобальное состояние здесь неизбежно: опрос делает задача, а поднимает
/// контроллеры код запуска, и передать владение между ними нечем — задачи
/// принимают только `fn()`.
static CONTROLLERS: crate::sync::SpinLock<Vec<Controller>> = crate::sync::SpinLock::new(Vec::new());

/// Сколько контроллеров ядро готово поднять.
const CONTROLLERS_MAX: usize = 4;

/// Адреса поднятых контроллеров на шине — для переписи устройств. Отдельно от
/// [`CONTROLLERS`], которые на время перечисления портов вынимаются целиком.
static DRIVEN: crate::sync::SpinLock<Vec<pci::Address>> = crate::sync::SpinLock::new(Vec::new());

/// Поднять контроллеры и устройства на них. Возвращает `true`, если нашлись
/// клавиатура или указатель.
///
/// Печатает всё, что узнала по дороге: без этого отладка драйвера, половина
/// шагов которого не даёт признаков отказа, невозможна.
///
/// # Safety
///
/// См. [`Controller::init`]. Дополнительно: вызывать **не** удерживая ни один
/// [`crate::sync::SpinLock`]. Внутри есть ожидания по таймеру, а лок держится с
/// запрещёнными прерываниями — то есть счётчик тиков не сдвинется, и ожидание
/// превратится в почти вечное.
pub unsafe fn init(rsdp: u64) -> bool {
    // SAFETY: контракт функции.
    let found = match unsafe { Controller::find_all(rsdp) } {
        Ok(found) => found,
        Err(err) => {
            kprintln!("  xhci        : unavailable: {err}");
            return false;
        }
    };
    if found.len() > CONTROLLERS_MAX {
        kprintln!(
            "  xhci        : {} controllers; only the first {CONTROLLERS_MAX} are brought up",
            found.len()
        );
    }

    let mut controllers = Vec::new();
    let (mut keyboard, mut mouse) = (false, false);
    for device in found.into_iter().take(CONTROLLERS_MAX) {
        // Отказ одного контроллера не отменяет остальные: клавиатура может
        // висеть как раз на следующем.
        // SAFETY: контракт функции.
        let mut controller = match unsafe { Controller::init(device, controllers.len()) } {
            Ok(controller) => controller,
            Err(err) => {
                kprintln!("  xhci        : {}: unavailable: {err}", device.address);
                continue;
            }
        };
        // SAFETY: контроллер работает.
        //
        // Контроллер остаётся поднятым, даже если ничего не нашлось, и это не
        // упрямство: он продолжает складывать в кольцо события об изменении
        // портов, то есть остаётся способом узнать о подключении устройства
        // позже.
        let (k, m) = unsafe { controller.attach_devices() };
        keyboard |= k;
        mouse |= m;
        controllers.push(controller);
    }
    if controllers.is_empty() {
        return false;
    }
    if controllers.len() > 1 {
        kprintln!("  xhci        : {} controllers brought up", controllers.len());
    }
    if !keyboard && !mouse {
        kprintln!("  usb         : no boot-protocol device on any root port");
    }

    let sources = input::sources();
    input::set_sources(input::Sources {
        keyboard: sources.keyboard || keyboard,
        mouse: sources.mouse || mouse,
        ..sources
    });
    *DRIVEN.lock() = controllers.iter().map(|c| c.device.address).collect();
    *CONTROLLERS.lock() = controllers;
    keyboard || mouse
}

/// Ведёт ли драйвер контроллер с этим адресом — для переписи устройств.
#[must_use]
pub fn drives(address: pci::Address) -> bool {
    DRIVEN.lock().contains(&address)
}

/// Ярлык источника прерывания для планировщика.
///
/// Число произвольное и ничего не значит снаружи: [`crate::sched::Wait::Irq`]
/// различает источники, а не адресует их. Важно лишь, что обработчик и задача
/// договорились об одном и том же.
const IRQ_SOURCE: u32 = crate::irq::source::XHCI;

/// Пришло ли прерывание с тех пор, как задача в последний раз разбирала кольцо.
///
/// Признак нужен именно как признак, а не как счётчик: задача разбирает **все**
/// накопившиеся события всех контроллеров за один проход, и три прерывания
/// подряд означают ровно то же, что одно, — «сходи посмотри».
static EVENT_PENDING: AtomicBool = AtomicBool::new(false);

/// Что обработчику прерывания нужно знать о контроллере, не беря его лок.
struct Ack {
    /// Адрес `USBSTS`; ноль — прерывания не настраивались.
    status: AtomicUsize,
    /// Адрес `IMAN` прерывателя 0.
    iman: AtomicUsize,
    /// Каким путём приходят прерывания: [`Delivery`] числом.
    delivery: AtomicU8,
    /// Сколько прерываний пришло. Единственное, чем «работает по
    /// прерываниям» отличается от «работает по опросу» на взгляд снаружи, —
    /// поэтому счётчик и печатается.
    count: AtomicU64,
}

impl Ack {
    const fn new() -> Self {
        Self {
            status: AtomicUsize::new(0),
            iman: AtomicUsize::new(0),
            delivery: AtomicU8::new(Delivery::Polled as u8),
            count: AtomicU64::new(0),
        }
    }
}

/// По записи на контроллер, в порядке списка.
static ACK: [Ack; CONTROLLERS_MAX] = [Ack::new(), Ack::new(), Ack::new(), Ack::new()];

/// Путь прерываний от контроллера.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum Delivery {
    /// Прерываний нет, кольцо разбирается по часам.
    Polled = 0,
    MsiX = 1,
    Msi = 2,
    /// Уровневая линия через I/O APIC или GIC.
    Intx = 3,
}

impl Delivery {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::MsiX,
            2 => Self::Msi,
            3 => Self::Intx,
            _ => Self::Polled,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Polled => "polled",
            Self::MsiX => "MSI-X",
            Self::Msi => "MSI",
            Self::Intx => "INTx",
        }
    }
}

impl Controller {
    fn delivery(&self) -> Delivery {
        Delivery::from_u8(ACK[self.index].delivery.load(Ordering::Acquire))
    }
}

impl Summary {
    /// Сложить сводки двух контроллеров.
    fn merge(mut self, other: Self) -> Self {
        let mut free = self.attached.iter_mut().filter(|entry| entry.port == 0);
        for entry in other.attached.iter().filter(|entry| entry.port != 0) {
            match free.next() {
                Some(slot) => *slot = *entry,
                None => break,
            }
        }
        if self.devices == 0 {
            self.slot = other.slot;
            self.port = other.port;
        }
        if other.last_error.is_some() {
            self.last_error = other.last_error;
        }
        if self.delivery != other.delivery {
            self.delivery = "mixed";
        }
        self.devices += other.devices;
        self.hubs += other.hubs;
        self.keyboards += other.keyboards;
        self.mice += other.mice;
        self.occupied += other.occupied;
        self.reports += other.reports;
        self.events += other.events;
        self.errors += other.errors;
        self.event_floods += other.event_floods;
        self.interrupts += other.interrupts;
        self.services += other.services;
        self.controllers += other.controllers;
        self
    }
}

/// Сводка по всем контроллерам списка.
fn total(list: &[Controller]) -> Option<Summary> {
    let mut iter = list.iter();
    let first = iter.next()?.summary();
    Some(iter.fold(first, |acc, controller| acc.merge(controller.summary())))
}

/// Забрать контроллер у прошивки (USB Legacy Support, xHCI 1.1, 7.1).
///
/// Прошивка, у которой в настройках включена поддержка USB-клавиатуры, владеет
/// контроллером через обработчик SMM: он же изображает клавиатуру PS/2 для
/// загрузочного меню. Система обязана попросить контроллер себе и выключить
/// прерывания SMI — иначе обработчик прошивки продолжает работать с тем же
/// железом, что и драйвер. Linux делает это для каждого USB-контроллера ещё при
/// разборе PCI. У QEMU этой возможности нет, и там функция молчит.
///
/// Прошивка, не отпустившая контроллер за секунду, лишается его силой — тем же
/// способом, что в Linux: без контроллера система останется без ввода, а с
/// навязанным ей решением прошивка обычно справляется.
///
/// # Safety
///
/// Окно регистров должно быть отображено; вызывать до остановки и сброса.
unsafe fn take_from_firmware(regs: &Registers) {
    // SAFETY: контракт функции.
    let Some(at) = (unsafe { regs.find_ext_cap(EXT_CAP_LEGACY) }) else {
        return;
    };
    // SAFETY: `at` — найденная возможность USB Legacy Support; оба её регистра
    // по 32 бита.
    unsafe {
        let value = regs.read_cap32(at);
        if value & LEGSUP_BIOS_OWNED != 0 {
            regs.write_cap32(at, value | LEGSUP_OS_OWNED);
            let mut timeout = Timeout::new(HANDOFF_TIMEOUT_MS);
            loop {
                let now = regs.read_cap32(at);
                if now & LEGSUP_BIOS_OWNED == 0 {
                    kprintln!("  xhci        : the firmware handed the controller over");
                    break;
                }
                if timeout.expired() {
                    regs.write_cap32(at, (now & !LEGSUP_BIOS_OWNED) | LEGSUP_OS_OWNED);
                    kprintln!(
                        "  xhci        : the firmware kept the controller for {HANDOFF_TIMEOUT_MS} ms; taken anyway"
                    );
                    break;
                }
                core::hint::spin_loop();
            }
        } else {
            kprintln!("  xhci        : legacy support present, the firmware does not own the controller");
        }
        let control = regs.read_cap32(at + LEGCTLSTS_OFFSET);
        regs.write_cap32(at + LEGCTLSTS_OFFSET, (control & LEGCTLSTS_KEEP) | LEGCTLSTS_SMI_EVENTS);
    }
}

/// Обработчик прерывания контроллеров.
///
/// Делает три вещи и ни одной лишней: подтверждает прерывание у контроллера,
/// поднимает признак и будит задачу. Разбор колец остаётся задаче, потому что
/// он занимает сотни микросекунд, а всё это время прерывания были бы запрещены
/// — то есть ровно та болезнь, от которой прерывания и лечат.
///
/// Обработчик один на все контроллеры (фаза 68m): `fn()` не несёт, от кого
/// прерывание, поэтому подтверждаются все, у кого оно есть. У MSI и MSI-X
/// признак `IP` контроллер снимает сам, когда сообщение ушло, — там
/// подтверждается без проверки; общую линию `INTx` без признаков не трогаем.
///
/// Подтверждение идёт по адресам из [`ACK`], а не через лок контроллера (фаза
/// 68k): [`poll_hotplug`] держит контроллеры «в руках» сотни миллисекунд, и на
/// уровневой линии не снятый признак означал бы прерывание, повторяющееся без
/// конца всё это время. Оба бита сбрасываются записью единицы, и оба
/// обязательны: `EINT` в общем состоянии и `IP` у прерывателя — пока `IP`
/// стоит, следующего прерывания не будет, а линия не опустится.
pub fn on_interrupt() {
    let mut ours = false;
    for ack in &ACK {
        let status = ack.status.load(Ordering::Acquire);
        let iman = ack.iman.load(Ordering::Acquire);
        let delivery = Delivery::from_u8(ack.delivery.load(Ordering::Acquire));
        if status == 0 || iman == 0 || delivery == Delivery::Polled {
            continue;
        }
        // SAFETY: адреса положены `enable_interrupts` из отображённого окна
        // регистров, которое живёт всё время работы ядра.
        unsafe {
            let pending = core::ptr::read_volatile(status as *const u32)
                & regs::USBSTS_EVENT_INTERRUPT;
            let raised = core::ptr::read_volatile(iman as *const u32)
                & regs::IMAN_INTERRUPT_PENDING;
            if pending == 0 && raised == 0 && delivery == Delivery::Intx {
                continue;
            }
            core::ptr::write_volatile(status as *mut u32, regs::USBSTS_EVENT_INTERRUPT);
            core::ptr::write_volatile(
                iman as *mut u32,
                regs::IMAN_INTERRUPT_ENABLE | regs::IMAN_INTERRUPT_PENDING,
            );
        }
        ack.count.fetch_add(1, Ordering::Relaxed);
        ours = true;
    }
    if !ours {
        return;
    }
    EVENT_PENDING.store(true, Ordering::Release);
    crate::sched::wake_irq(IRQ_SOURCE);
}

/// Разобрать накопившиеся события контроллеров.
///
/// Вызывается задачей — из обработчика прерывания её звать нельзя, см.
/// [`on_interrupt`].
pub fn service() {
    for controller in CONTROLLERS.lock().iter_mut() {
        controller.service();
    }
}

/// У каких контроллеров на портах что-то новое (или исчезло старое): по биту
/// на контроллер.
fn ports_changed(compare_mask: bool) -> u32 {
    let mut changed = 0;
    for (index, controller) in CONTROLLERS.lock().iter_mut().enumerate() {
        // Все контроллеры, без короткого замыкания: сверка обновляет маску.
        // SAFETY: контроллер существует, значит окно его регистров отображено.
        if unsafe { controller.ports_differ(compare_mask) } {
            changed |= 1 << index;
        }
    }
    changed
}

/// Перечислить порты заново у контроллеров из маски — то, что делает
/// «воткнули на ходу» работающим.
///
/// # Почему контроллеры забираются из глобала целиком
///
/// Потому что перечисление длится сотни миллисекунд, а [`crate::sync::SpinLock`]
/// держится с запрещёнными прерываниями: провести под ним сброс порта значило бы
/// остановить на это время часы и планировщик — то есть те самые часы, по
/// которым перечисление отсчитывает свои ожидания.
///
/// Пока контроллеры «в руках», опрос видит пустой список и ничего не делает.
/// Потерять этим нечего: события копятся в кольцах, а разберёт их первый же
/// вызов [`service`] после возврата.
fn poll_hotplug(mask: u32) {
    let mut taken = core::mem::take(&mut *CONTROLLERS.lock());
    let mut changed = false;
    for (index, controller) in taken.iter_mut().enumerate() {
        if mask & (1 << index) != 0 {
            // SAFETY: контроллер работает, вызов идёт из задачи.
            changed |= unsafe { controller.rescan() };
        }
    }
    if changed {
        if let Some(summary) = total(&taken) {
            kprintln!(
                "  usb         : now {} device(s), {} keyboard(s), {} pointer(s)",
                summary.devices,
                summary.keyboards,
                summary.mice
            );
        }
    }
    *CONTROLLERS.lock() = taken;
}

/// Поднялся ли хоть один контроллер.
///
/// Нужно тому, кто решает, заводить ли задачу опроса: на машине без xHCI она
/// просыпалась бы десять раз в секунду ради вызова, которому нечего делать.
#[must_use]
pub fn is_present() -> bool {
    !DRIVEN.lock().is_empty()
}

/// Как часто опрашивать контроллер, когда прерывания недоступны.
///
/// Десять миллисекунд — это период опроса конечной точки прерываний у обеих
/// загрузочных устройств: чаще бессмысленно, реже — заметно пальцам.
const POLL_PERIOD_MS: u64 = 10;

/// Как часто сверять состав портов.
///
/// Полсекунды: человек, воткнувший мышь, не замечает такой задержки, а чтение
/// четырнадцати регистров дважды в секунду не стоит ничего. Чаще незачем — это
/// страховка на случай, когда событие от контроллера не пришло.
const PORT_CHECK_PERIOD_MS: u64 = 500;

/// Настроены ли прерывания у **всех** контроллеров. Решает, ждать задаче
/// события или часов: один контроллер на опросе — и опрашиваются все.
fn interrupts_enabled() -> bool {
    CONTROLLERS.lock().iter().all(|c| c.delivery() != Delivery::Polled)
}

/// Тело задачи, обслуживающей контроллер.
///
/// До Phase 13d опрос жил в задаче оболочки, и это работало ровно потому, что
/// оболочка крутилась без остановки. Как только она научилась спать в ожидании
/// ввода, оказалось, что ждать ей нечего: события ввода рождал этот самый опрос,
/// и оболочка, уснувшая до него, не проснулась бы никогда. Круг разорвало то,
/// что опрос стал отдельной задачей со своими часами.
///
/// Теперь у той же задачи часов нет. Она спит, пока контроллер не подаст
/// прерывание, и просыпается ровно на события — а не сто раз в секунду ради
/// того, чтобы убедиться, что никто не нажал ни одной клавиши. Разница видна не
/// в клавиатуре (задержка была и осталась незаметной), а в том, что машине,
/// которой нечего делать, больше ничто не мешает стоять.
///
/// На машине без MSI-X задача возвращается к прежнему поведению. Это не
/// запасной путь на всякий случай: контроллер, чья таблица векторов лежит в
/// незанятом BAR, существует, и клавиатура на нём должна работать.
pub fn service_task() {
    let by_interrupt = interrupts_enabled();
    let mut next_port_check = 0u64;

    loop {
        service();

        // Порты сверяются по часам, а не только по событию, и период у сверки
        // свой — она стоит чтения регистра на порт, тогда как разбор кольца
        // происходит на каждое прерывание.
        //
        // Зачем вообще сверять, если контроллер обязан прислать событие: затем,
        // что «обязан» — это про исправную машину. На той, где мышь работает, а
        // клавиатура появляется на шине через несколько секунд после загрузки,
        // событие до драйвера так и не дошло, и устройство не существовало для
        // системы до перезагрузки.
        let now = time::uptime_ms();
        let compare_mask = now >= next_port_check;
        if compare_mask {
            next_port_check = now.saturating_add(PORT_CHECK_PERIOD_MS);
        }
        let changed = ports_changed(compare_mask);
        if changed != 0 {
            poll_hotplug(changed);
        }

        if by_interrupt {
            // Признак проверяется **под локом планировщика** — в этом весь
            // смысл `block_on_irq`. Прерывание, пришедшее между «проверил» и
            // «уснул», иначе не разбудило бы никого: будить было бы ещё некого.
            //
            // Здесь сон без срока, и это осознанно: машина с работающими
            // прерываниями присылает событие об изменении состояния порта, и
            // именно оно разбудит задачу. Сверка по часам остаётся страховкой
            // для машин на опросе — а это ровно те машины, где событие и не
            // доходило.
            crate::sched::block_on_irq(IRQ_SOURCE, || {
                EVENT_PENDING.swap(false, Ordering::AcqRel)
            });
        } else {
            crate::sched::sleep_ms(POLL_PERIOD_MS);
        }
    }
}

/// Сводка для диагностики — по всем контроллерам.
#[must_use]
pub fn summary() -> Option<Summary> {
    total(&CONTROLLERS.lock())
}
