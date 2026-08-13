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
//! вопрос — «куда и что писать» ([`crate::arch::interrupts::setup_xhci_msi`]):
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
//! Машина без MSI-X остаётся на опросе, и это видно в `usb`: счётчик `irqs`
//! равен нулю. Отказываться от работающей клавиатуры из-за отсутствия
//! прерываний было бы странно.
//!
//! # Сколько устройств
//!
//! Все, что висят на корневых портах и понимают boot-протокол HID: сейчас это
//! клавиатура и мышь, каждая в своём слоте, со своим кольцом точки прерываний и
//! своим разборщиком отчётов. События приходят в одно кольцо на весь
//! контроллер, поэтому передача опознаётся по номеру слота из самого события —
//! иначе отчёт мыши достался бы клавиатуре.
//!
//! Хабов драйвер не проходит: на пути к устройствам их нет ни в QEMU, ни на
//! Raspberry Pi 4, где VL805 сам является корневым хабом. Устройство за
//! внешним хабом видно не будет, и это сказано вслух, а не обойдено.
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

use core::sync::atomic::{AtomicBool, Ordering};

use crate::acpi::AcpiError;
use crate::input;
use crate::time;
use crate::kprintln;
use crate::mm::dma::{self, DmaBuffer, DmaError};
use crate::mm::{MapError, PAGE_SIZE, PhysAddr};
use crate::pci;
use crate::usb::hid::{self, REPORT_LEN};
use crate::usb::{self, HidInterface};

use alloc::vec::Vec;

use regs::Registers;
use ring::{EventRing, Ring, Trb};

/// Сколько дескрипторов в кольце. 256 — это 4 КиБ, то есть ровно страница; для
/// клавиатуры хватило бы и восьми, но страница всё равно минимальная единица
/// выделения.
const RING_ENTRIES: usize = PAGE_SIZE / ring::TRB_LEN;

/// Сколько слотов устройств ядро просит у контроллера.
///
/// Четыре: клавиатура и мышь занимают по слоту, и запас на пару устройств
/// оставлен затем, чтобы подключение третьего не требовало правки драйвера.
/// Просить много не вредно, но каждый слот — это указатель в массиве
/// контекстов, а массив контроллер читает целиком.
const SLOTS_WANTED: u8 = 4;

/// Минимальная версия интерфейса. `0x0090` — это xHCI 0.9, черновик, который
/// встречался в первых чипсетах Intel и отличается расположением части полей.
const MIN_VERSION: u16 = 0x0090;

/// Сколько ждать сброса и запуска контроллера.
const RESET_TIMEOUT_MS: u64 = 1000;
/// Сколько ждать завершения команды.
const COMMAND_TIMEOUT_MS: u64 = 500;
/// Сколько ждать завершения передачи по управляющей точке.
const TRANSFER_TIMEOUT_MS: u64 = 500;
/// Сколько ждать окончания сброса порта.
const PORT_RESET_TIMEOUT_MS: u64 = 500;
/// Пауза после сброса порта: спецификация USB требует дать устройству время на
/// восстановление, прежде чем обращаться к нему.
const PORT_RECOVERY_MS: u64 = 20;

/// Предел витков холостого ожидания.
///
/// Страховка на случай остановившегося таймера: без неё отказ таймера превратил
/// бы любое ожидание в вечное. Значение подобрано так, чтобы на любой мыслимой
/// частоте оно исчерпывалось позже, чем истекает время.
const SPIN_LIMIT: u32 = 200_000_000;

/// Через сколько витков спрашивать часы. См. [`Timeout::expired`].
const CLOCK_EVERY: u32 = 64;

/// Ожидание с двумя независимыми пределами.
struct Timeout {
    started_ms: u64,
    until_ms: u64,
    spins: u32,
}

impl Timeout {
    fn new(ms: u64) -> Self {
        Self { started_ms: time::uptime_ms(), until_ms: time::uptime_ms().saturating_add(ms), spins: 0 }
    }

    /// Сколько ждали и упёрлись ли в предел витков вместо часов.
    ///
    /// Различать это обязательно. «Часы отсчитали полсекунды» — значит
    /// устройство молчит; «кончились витки» — значит часы стоят, и настоящая
    /// неисправность совсем в другом месте. На машине без журнала эти два
    /// случая выглядят одинаково: «a control transfer never completed».
    fn report(&self) -> (u64, bool) {
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
    fn expired(&mut self) -> bool {
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
fn sleep_ms(ms: u64) {
    let mut timeout = Timeout::new(ms);
    while !timeout.expired() {}
}

/// Почему контроллер или устройство не заработали.
#[derive(Debug, Clone, Copy)]
pub enum XhciError {
    /// Не нашлось таблиц ACPI, а значит и окна конфигурационного пространства.
    Acpi(AcpiError),
    /// На шине нет контроллера с классом xHCI.
    NoController,
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
}

impl core::fmt::Display for XhciError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Acpi(err) => write!(f, "{err}"),
            Self::NoController => f.write_str("no xHCI controller on the PCI bus"),
            Self::NoBar => f.write_str("the controller has no memory BAR assigned"),
            Self::Map(err) => write!(f, "mapping the register window failed: {err}"),
            Self::Version(version) => {
                write!(f, "interface version {version:#06x} predates xHCI 0.96")
            }
            Self::ResetTimeout => f.write_str("the controller did not finish its reset"),
            Self::StartTimeout => f.write_str("the controller did not start"),
            Self::Dma(err) => write!(f, "{err}"),
            Self::CommandTimeout(command) => write!(f, "command {command} never completed"),
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
        }
    }
}

impl From<DmaError> for XhciError {
    fn from(err: DmaError) -> Self {
        Self::Dma(err)
    }
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

/// Кто разбирает отчёты этого устройства.
///
/// Разница между клавиатурой и мышью для драйвера контроллера ровно в этом:
/// один байт дескриптора интерфейса решает, какой разборщик получит отчёт.
/// Всё остальное — слот, адресация, кольца, дверной звонок — у них общее.
enum Reader {
    Keyboard(hid::Keyboard),
    Mouse(hid::Mouse),
}

impl Reader {
    fn handle_report(&mut self, report: &[u8]) {
        match self {
            Reader::Keyboard(keyboard) => keyboard.handle_report(report),
            Reader::Mouse(mouse) => mouse.handle_report(report),
        }
    }

    const fn reports(&self) -> u64 {
        match self {
            Reader::Keyboard(keyboard) => keyboard.reports(),
            Reader::Mouse(mouse) => mouse.reports(),
        }
    }

    const fn name(&self) -> &'static str {
        match self {
            Reader::Keyboard(_) => "keyboard",
            Reader::Mouse(_) => "mouse",
        }
    }

    /// Чем устройство оказалось на самом деле.
    ///
    /// Именно на самом деле, а не по байту дескриптора интерфейса: планшет
    /// объявляет протокол ноль, и поверив дескриптору, ядро сообщило бы, что
    /// указателя в системе нет, — при работающем указателе.
    const fn protocol(&self) -> u8 {
        match self {
            Reader::Keyboard(_) => usb::PROTOCOL_KEYBOARD,
            Reader::Mouse(_) => usb::PROTOCOL_MOUSE,
        }
    }
}

/// Подключённое устройство.
struct Device {
    slot: u8,
    /// Корневой порт, к которому она подключена.
    port: u8,
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
    /// Виртуальный адрес таблицы MSI-X, если прерывания настроены. `None`
    /// означает, что контроллер остался на опросе.
    msix_table: Option<usize>,
    /// Сколько прерываний пришло. Единственное, чем «работает по прерываниям»
    /// отличается от «работает по опросу» на взгляд снаружи, — поэтому счётчик
    /// и печатается.
    interrupts: u64,
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
    /// Найти контроллер, поднять его и подключить клавиатуру.
    ///
    /// # Safety
    ///
    /// Ядро должно исполняться на собственных таблицах страниц, прерывания —
    /// быть разрешены (ожидания опираются на таймер), а память с таблицами ACPI
    /// — оставаться нетронутой.
    pub unsafe fn init(rsdp: u64) -> Result<Self, XhciError> {
        // SAFETY: контракт функции.
        let root = unsafe { pci::Root::discover(rsdp) }.map_err(XhciError::Acpi)?;
        kprintln!("  pci         : {root}");

        // SAFETY: контракт функции.
        let device = unsafe {
            pci::find_by_class(&root, pci::CLASS_SERIAL_BUS, pci::SUBCLASS_USB, pci::PROG_IF_XHCI)
        }
        .ok_or(XhciError::NoController)?;
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
            "  xhci        : version {}.{:x}, {} slots, {} ports, {}-byte contexts, {} scratchpad",
            regs.version >> 8,
            (regs.version >> 4) & 0xF,
            regs.max_slots,
            regs.max_ports,
            regs.context_size,
            regs.max_scratchpad
        );

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
            msix_table: None,
            interrupts: 0,
            services: 0,
            occupied: 0,
            last_error: None,
            ports_changed: false,
            connected: 0,
        };

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
                Ok(usb::PROTOCOL_KEYBOARD) => keyboard = true,
                Ok(usb::PROTOCOL_MOUSE) => mouse = true,
                Ok(_) => {}
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

        // Сначала — исчезнувшие: их слоты нужны тем, кто пришёл на их место, а
        // слотов у контроллера всего четыре.
        let mut index = 0;
        while index < self.devices.len() {
            let port = self.devices[index].port;
            if connected & (1 << (port - 1)) != 0 {
                index += 1;
                continue;
            }
            let slot = self.devices[index].slot;
            kprintln!("  usb         : device on root port {port} is gone, freeing slot {slot}");
            // SAFETY: слот выдан контроллером этому устройству.
            unsafe { self.release_slot(slot) };
            self.devices.remove(index);
        }

        // SAFETY: см. выше.
        let (keyboard, mouse) = unsafe { self.attach_missing() };
        if keyboard || mouse {
            let sources = input::sources();
            input::set_sources(input::Sources {
                keyboard: sources.keyboard || keyboard,
                mouse: sources.mouse || mouse,
                ..sources
            });
        }

        before != self.devices.len()
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

    /// Подключить одно устройство: слот, адрес, дескрипторы, конфигурация.
    ///
    /// Возвращает протокол HID, по которому устройство опознано, либо шаг, на
    /// котором всё остановилось, вместе с ошибкой. Шаг возвращается наружу, а не
    /// только печатается, потому что на машине без последовательного порта
    /// печать некуда девать — а разница между «не ответило на сброс» и «не
    /// отдало дескриптор» решает, куда смотреть дальше.
    ///
    /// # Safety
    ///
    /// Контроллер должен работать, а порт — иметь подключённое устройство.
    unsafe fn attach_one(&mut self, port: u8) -> Result<u8, (Stage, XhciError)> {
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

        // SAFETY: см. выше.
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
        unsafe { self.address_device(&mut device, speed) }.map_err(|err| (Stage::Address, err))?;
        kprintln!("  usb         : slot {slot} addressed");

        // SAFETY: устройство отвечает на управляющей точке.
        let found = unsafe { self.describe_device(&mut device, speed) }
            .map_err(|err| (Stage::Describe, err))?;
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
        unsafe { self.configure_endpoint(&mut device, &found, speed) }
            .map_err(|err| (Stage::Configure, err))?;
        // SAFETY: см. выше.
        let reader =
            unsafe { self.enable_reports(&mut device, &found) }.map_err(|err| (Stage::Enable, err))?;
        let protocol = reader.protocol();
        kprintln!("  usb         : slot {slot} is a {}", reader.name());

        // Разборщик ставится последним: до этого момента буфер занят
        // дескрипторами, и отчёт в него ещё не запрашивался.
        device.reader = Some(reader);
        self.devices.push(device);
        // SAFETY: устройство настроено, кольцо точки прерываний готово.
        unsafe { self.queue_reports() };
        Ok(protocol)
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
        // SAFETY: см. выше.
        unsafe {
            self.write_context(&device.input, 1, 0, (1 << 27) | (speed << 20));
            self.write_context(&device.input, 1, 1, u32::from(device.port) << 16);
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

    /// Прочитать дескрипторы и найти интерфейс клавиатуры.
    ///
    /// # Safety
    ///
    /// Устройство должно быть адресовано.
    unsafe fn describe_device(
        &mut self,
        device: &mut Device,
        speed: u32,
    ) -> Result<HidInterface, XhciError> {
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
        // SAFETY: см. выше.
        if let Ok(read) = unsafe { self.get_descriptor(device, usb::DESC_DEVICE, 0, 18) } {
            // SAFETY: см. выше.
            let bytes = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), read) };
            if let Some(full) = usb::DeviceDescriptor::parse(bytes) {
                device.identity = (full.vendor, full.product);
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
        usb::find_hid(bytes).ok_or(XhciError::NoHid)
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
    /// # Safety
    ///
    /// Устройство должно быть адресовано, а кольцо точки — существовать.
    unsafe fn configure_endpoint(
        &mut self,
        device: &mut Device,
        found: &HidInterface,
        speed: u32,
    ) -> Result<(), XhciError> {
        // Идентификатор контекста точки: `2 * номер + 1` для направления IN. Он
        // же — цель дверного звонка.
        let target = found.endpoint * 2 + 1;
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
            self.write_context(&device.input, 1, 0, (slot0 & !(0x1F << 27)) | (entries << 27));

            let interval = endpoint_interval(speed, found.interval);
            let max_packet = u32::from(found.max_packet_size);
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
        let last = device.ep0.push(Trb {
            parameter: 0,
            status: 0,
            control: (ring::TRB_STATUS_STAGE << ring::TRB_TYPE_SHIFT)
                | ring::TRB_IOC
                | if status_in { DIR_IN } else { 0 },
        });

        // SAFETY: дескрипторы записаны целиком; цель 1 — управляющая точка.
        unsafe { self.regs.ring_doorbell(device.slot, 1) };

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
            // дескрипторами: запрос отчёта затёр бы их на полпути.
            if device.queued || device.reader.is_none() {
                continue;
            }
            device.interrupt.push(Trb {
                parameter: device.report.phys().as_u64(),
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
        // SAFETY: буфер отчёта выделен на страницу; читается ровно столько, сколько
        // сообщил контроллер. `volatile` не нужен — передача уже завершена, и
        // содержимое больше не меняется, а отображение некешируемое.
        let report = unsafe { core::slice::from_raw_parts(device.report.as_ptr::<u8>(), received) };
        if let Some(reader) = device.reader.as_mut() {
            reader.handle_report(report);
        }
    }

    /// Разобрать накопившиеся события и снова запросить отчёты.
    ///
    /// Вызывается из задачи. Когда появятся прерывания от контроллера, её будет
    /// вызывать обработчик — больше в драйвере не изменится ничего.
    /// Перевести контроллер с опроса на прерывания.
    ///
    /// Порядок здесь важнее обычного, и он такой: сперва обработчик, потом
    /// таблица MSI-X, и только затем разрешение у самого контроллера. Обратный
    /// порядок означал бы прерывание, которому некуда прийти.
    ///
    /// Молчит, если MSI-X нет: тогда драйвер остаётся на опросе, и об этом
    /// сообщает [`Controller::summary`]. Отказываться от работающей клавиатуры
    /// из-за отсутствия прерываний было бы странно.
    ///
    /// # Safety
    ///
    /// Контроллер должен быть запущен, а кольцо событий — рабочим: прерывание
    /// может прийти немедленно после последней записи.
    unsafe fn enable_interrupts(&mut self) {
        let Some(msix) = self.device.msix() else {
            kprintln!("  xhci        : no MSI-X capability; events will be polled");
            return;
        };
        let Some((address, data)) = crate::arch::interrupts::setup_xhci_msi() else {
            kprintln!("  xhci        : no MSI target on this machine; events will be polled");
            return;
        };
        let Some(bar) = self.device.memory_bar(msix.bir) else {
            kprintln!("  xhci        : MSI-X table lives in BAR{} which is empty", msix.bir);
            return;
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
                return;
            }
        };

        // SAFETY: таблица отображена, индекс 0 существует всегда (векторов не
        // бывает ноль), обработчик уже стоит — на x86-64 он в IDT с загрузки, на
        // AArch64 его поставил `setup_xhci_msi`.
        unsafe { self.device.set_msix_vector(&msix, table, 0, address, data) };

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

        self.msix_table = Some(table);
        kprintln!(
            "  xhci        : MSI-X vector 0 -> {address:#018x} data {data:#x}, {} vectors available",
            msix.vectors
        );
    }

    /// Подтвердить прерывание у контроллера.
    ///
    /// Оба бита сбрасываются записью единицы, и оба обязательны: `EINT` в общем
    /// состоянии и `IP` у прерывателя. Пока `IP` стоит, следующего прерывания не
    /// будет — то есть пропуск этой записи выглядит как «прерывание пришло ровно
    /// одно, а дальше тишина».
    ///
    /// # Safety
    ///
    /// Окно регистров отображено (оно живёт столько же, сколько контроллер).
    unsafe fn acknowledge_interrupt(&mut self) {
        self.interrupts += 1;
        // SAFETY: контракт функции.
        unsafe {
            self.regs.write_op32(regs::OP_USBSTS, regs::USBSTS_EVENT_INTERRUPT);
            self.regs.write_interrupter32(
                0,
                regs::IR_IMAN,
                regs::IMAN_INTERRUPT_ENABLE | regs::IMAN_INTERRUPT_PENDING,
            );
        }
    }

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
        let mut attached = [Attached::default(); SLOTS_WANTED as usize];
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
            devices: self.devices.len(),
            keyboards,
            mice,
            occupied: self.occupied,
            last_error: self.last_error,
            attached,
            slot: self.devices.first().map_or(0, |device| device.slot),
            port: self.devices.first().map_or(0, |device| device.port),
            reports,
            events: self.events_seen,
            errors: self.transfer_errors,
            event_floods: self.event_floods,
            interrupts: self.interrupts,
            services: self.services,
        }
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

/// Что драйвер сообщает о себе наружу.
///
/// Структура, а не кортеж из шести чисел: с появлением второго устройства
/// читать `summary().3` стало нельзя без подглядывания в объявление.
#[derive(Clone, Copy, Debug, Default)]
pub struct Summary {
    /// Сколько устройств поднято.
    pub devices: usize,
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
    pub attached: [Attached; SLOTS_WANTED as usize],
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
    /// Сколько раз задача просыпалась разбирать кольцо.
    pub services: u64,
}

/// Кто будет разбирать отчёты и на каком протоколе.
///
/// Возвращает разборщик и признак «на boot-протоколе»; `None` означает, что
/// устройство понять нечем.
///
/// # Почему дескриптор предпочтительнее boot-протокола
///
/// Не из любви к новому. Boot-протокол — упрощение, придуманное ради BIOS:
/// три байта у мыши, восемь у клавиатуры. Дескриптор описывает то, что
/// устройство шлёт **на самом деле**, и только он работает с теми, кто
/// boot-протокола не объявляет вовсе.
///
/// Держать его запасным путём и ходить по нему только на чужих машинах было бы
/// хуже всего: путь, по которому система ходит лишь там, где её некому чинить,
/// — это путь, который никто не проверял. Поэтому основной здесь он, а
/// boot-протокол остаётся для устройства, чей дескриптор разобрать не удалось.
fn choose_reader(found: &HidInterface, described: &usb_hid::Descriptor) -> Option<(Reader, bool)> {
    // Байт протокола главнее дескриптора ровно в одном: он решает, кем
    // устройство себя объявило. Клавиатура, у которой в дескрипторе нашлись
    // ещё и оси (такое бывает у клавиатур с тачпадом), не должна стать мышью.
    if found.protocol != usb::PROTOCOL_KEYBOARD {
        if let Some(map) = described.pointer {
            return Some((Reader::Mouse(hid::Mouse::described(map)), false));
        }
    }
    if let Some(map) = described.keyboard {
        return Some((Reader::Keyboard(hid::Keyboard::described(map)), false));
    }
    if let Some(map) = described.pointer {
        return Some((Reader::Mouse(hid::Mouse::described(map)), false));
    }

    if !found.boot {
        return None;
    }
    match found.protocol {
        usb::PROTOCOL_MOUSE => Some((Reader::Mouse(hid::Mouse::boot()), true)),
        usb::PROTOCOL_KEYBOARD => Some((Reader::Keyboard(hid::Keyboard::boot()), true)),
        // Boot-подкласс без протокола клавиатуры или мыши: спецификация такого
        // не описывает, и угадывать формат отчёта не по чему.
        _ => None,
    }
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

/// Единственный контроллер, которым ядро распоряжается.
///
/// Глобальное состояние здесь неизбежно: опрос делает задача, а поднимает
/// контроллер код запуска, и передать владение между ними нечем — задачи
/// принимают только `fn()`.
static CONTROLLER: crate::sync::SpinLock<Option<Controller>> = crate::sync::SpinLock::new(None);

/// Поднять контроллер и клавиатуру. Возвращает `true`, если клавиатура работает.
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
    let mut controller = match unsafe { Controller::init(rsdp) } {
        Ok(controller) => controller,
        Err(err) => {
            kprintln!("  xhci        : unavailable: {err}");
            return false;
        }
    };

    // SAFETY: контроллер работает.
    //
    // Контроллер остаётся поднятым, даже если ничего не нашлось, и это не
    // упрямство: он продолжает складывать в кольцо события об изменении портов,
    // то есть остаётся способом узнать о подключении устройства позже.
    let (keyboard, mouse) = unsafe { controller.attach_devices() };
    if !keyboard && !mouse {
        kprintln!("  usb         : no boot-protocol device on any root port");
    }

    let sources = input::sources();
    input::set_sources(input::Sources {
        keyboard: sources.keyboard || keyboard,
        mouse: sources.mouse || mouse,
        ..sources
    });
    *CONTROLLER.lock() = Some(controller);
    keyboard || mouse
}

/// Ярлык источника прерывания для планировщика.
///
/// Число произвольное и ничего не значит снаружи: [`crate::sched::Wait::Irq`]
/// различает источники, а не адресует их. Важно лишь, что обработчик и задача
/// договорились об одном и том же.
const IRQ_SOURCE: u32 = 1;

/// Пришло ли прерывание с тех пор, как задача в последний раз разбирала кольцо.
///
/// Признак нужен именно как признак, а не как счётчик: задача разбирает **все**
/// накопившиеся события за один проход, и три прерывания подряд означают ровно
/// то же, что одно, — «сходи посмотри».
static EVENT_PENDING: AtomicBool = AtomicBool::new(false);

/// Обработчик прерывания контроллера.
///
/// Делает три вещи и ни одной лишней: подтверждает прерывание у контроллера,
/// поднимает признак и будит задачу. Разбор колец остаётся задаче, потому что
/// он занимает сотни микросекунд, а всё это время прерывания были бы запрещены
/// — то есть ровно та болезнь, от которой прерывания и лечат.
///
/// Лок здесь брать безопасно: [`crate::sync::SpinLock`] удерживается с
/// запрещёнными прерываниями, поэтому обработчик не может застать его занятым
/// на этом же ядре.
pub fn on_interrupt() {
    if let Some(controller) = CONTROLLER.lock().as_mut() {
        // SAFETY: контроллер существует, значит его окно регистров отображено.
        unsafe { controller.acknowledge_interrupt() };
    }
    EVENT_PENDING.store(true, Ordering::Release);
    crate::sched::wake_irq(IRQ_SOURCE);
}

/// Разобрать накопившиеся события контроллера.
///
/// Вызывается задачей — из обработчика прерывания её звать нельзя, см.
/// [`on_interrupt`].
pub fn service() {
    if let Some(controller) = CONTROLLER.lock().as_mut() {
        controller.service();
    }
}

/// Появилось ли на портах что-то новое (или исчезло старое).
fn ports_changed(compare_mask: bool) -> bool {
    match CONTROLLER.lock().as_mut() {
        // SAFETY: контроллер существует, значит окно его регистров отображено.
        Some(controller) => unsafe { controller.ports_differ(compare_mask) },
        None => false,
    }
}

/// Перечислить порты заново — то, что делает «воткнули на ходу» работающим.
///
/// # Почему контроллер забирается из глобала целиком
///
/// Потому что перечисление длится сотни миллисекунд, а [`crate::sync::SpinLock`]
/// держится с запрещёнными прерываниями: провести под ним сброс порта значило бы
/// остановить на это время часы и планировщик — то есть те самые часы, по
/// которым перечисление отсчитывает свои ожидания.
///
/// Пока контроллер «в руках», обработчик прерывания и опрос видят пустое место и
/// ничего не делают. Потерять этим нечего: события копятся в кольце, а разберёт
/// их первый же вызов [`service`] после возврата.
pub fn poll_hotplug() {
    let taken = CONTROLLER.lock().take();
    let Some(mut controller) = taken else {
        return;
    };
    // SAFETY: контроллер работает, вызов идёт из задачи.
    let changed = unsafe { controller.rescan() };
    if changed {
        let summary = controller.summary();
        kprintln!(
            "  usb         : now {} device(s), {} keyboard(s), {} pointer(s)",
            summary.devices,
            summary.keyboards,
            summary.mice
        );
    }
    *CONTROLLER.lock() = Some(controller);
}

/// Поднялся ли контроллер.
///
/// Нужно тому, кто решает, заводить ли задачу опроса: на машине без xHCI она
/// просыпалась бы десять раз в секунду ради вызова, которому нечего делать.
#[must_use]
pub fn is_present() -> bool {
    CONTROLLER.lock().is_some()
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

/// Настроены ли прерывания. Решает, ждать задаче события или часов.
fn interrupts_enabled() -> bool {
    CONTROLLER.lock().as_ref().is_some_and(|c| c.msix_table.is_some())
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
        if ports_changed(compare_mask) {
            poll_hotplug();
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

/// Сводка для диагностики.
#[must_use]
pub fn summary() -> Option<Summary> {
    CONTROLLER.lock().as_ref().map(Controller::summary)
}
