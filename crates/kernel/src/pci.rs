//! Шина PCI Express: найти устройство и узнать, где его регистры.
//!
//! # Сначала ECAM, а если его нет — порты
//!
//! ECAM (Enhanced Configuration Access Mechanism) — отображённое в память
//! конфигурационное пространство, введённое вместе с PCI Express. Оно работает
//! одинаково на обеих архитектурах и адресует 4096 байт на функцию вместо 256,
//! поэтому это основной путь.
//!
//! Долгое время он был единственным, и рассуждение звучало так: портов
//! ввода-вывода на AArch64 не существует, значит общий драйвер должен обходиться
//! без них. Рассуждение верное, а вывод из него — нет. Порты нужны не ради
//! симметрии, а потому что **MCFG есть не у всех**: VirtualBox с чипсетом PIIX3
//! (его умолчание) таблицы не публикует, и ядро печатало «no 'MCFG' table»,
//! после чего на машине не было ни USB, ни диска — при исправных устройствах.
//!
//! Поэтому на x86-64 есть запасной путь: механизм конфигурации №1, пара портов
//! `0xCF8`/`0xCFC`. Он старше PCI Express и работает на всём, что притворяется
//! PC. На AArch64 его нет и быть не может — там машина без MCFG это машина без
//! PCI, и сказать об этом честно лучше, чем изображать поддержку.
//!
//! Где лежит окно ECAM, сообщает таблица ACPI `MCFG`. Ни на одной из наших
//! машин этот адрес не совпадает с чужим: у QEMU `q35` это `0xB000_0000`, у
//! QEMU `virt` — `0x4010_0000_00`, у Raspberry Pi 4 — своё. Захардкодить его
//! означало бы получить ядро, работающее ровно на одной машине.
//!
//! # Как устроен адрес
//!
//! ```text
//!   ECAM + (bus << 20) | (device << 15) | (function << 12) | register
//! ```
//!
//! То есть функция занимает ровно страницу (4 КиБ), устройство — восемь страниц,
//! шина — мегабайт. Отсюда и стратегия отображения: страницы окна доотображаются
//! шиной целиком, по мегабайту, и только те шины, которые действительно
//! перебираются. Отобразить окно полностью — это 256 МиБ трансляций под память,
//! которой на машине обычно нет.
//!
//! # Перебор с рекурсией, а не подряд
//!
//! Шины перебираются не «все 256», а обходом от нулевой с заходом за мосты.
//! Причина не в экономии: за мостом на Raspberry Pi 4 как раз и находится
//! контроллер xHCI (VL805 сидит на шине 1), так что без рекурсии целевая машина
//! этой фазы осталась бы без клавиатуры.

// Карта конфигурационного пространства заводится целиком: смещения и биты — это
// описание железа, а не код, и половина из них нужна первому же следующему
// драйверу. Так же поступают `mm` и `sched`.
#![allow(dead_code)]

use crate::acpi::{self, AcpiError, SDT_HEADER_LEN, read_u16, read_u64};
use crate::arch;
use crate::kprintln;
use crate::mm::{MapError, PAGE_SIZE, PageFlags, PhysAddr, VirtAddr};

// ---------------------------------------------------------------------------
// Конфигурационное пространство: смещения и биты
// ---------------------------------------------------------------------------

const CFG_VENDOR_ID: usize = 0x00;
const CFG_DEVICE_ID: usize = 0x02;
const CFG_COMMAND: usize = 0x04;
const CFG_REVISION: usize = 0x08;
const CFG_PROG_IF: usize = 0x09;
const CFG_SUBCLASS: usize = 0x0A;
const CFG_CLASS: usize = 0x0B;
const CFG_STATUS: usize = 0x06;
const CFG_HEADER_TYPE: usize = 0x0E;
const CFG_BAR0: usize = 0x10;
/// Смещение указателя на первую запись списка возможностей.
const CFG_CAPABILITIES_PTR: usize = 0x34;

/// `Status`, бит 4: у функции есть список возможностей.
const STATUS_CAPABILITIES: u16 = 1 << 4;
/// Номер шины за мостом (только у header type 1).
const CFG_SECONDARY_BUS: usize = 0x19;

/// `Command`, бит 1: устройство отвечает на обращения к своим BAR в памяти.
const COMMAND_MEMORY_SPACE: u16 = 1 << 1;
/// Бит 2: устройству разрешено самому быть инициатором на шине, то есть делать
/// DMA. Без него xHCI не прочитает ни одного дескриптора из кольца, а отказа не
/// будет — контроллер просто ничего не сделает.
const COMMAND_BUS_MASTER: u16 = 1 << 2;
/// Бит 10: запрет прерываний по линии INTx. Ставится сознательно — события
/// контроллера ядро опрашивает само (см. [`crate::usb::xhci`]), и молчащая линия
/// лучше приходящих в никуда прерываний.
const COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// Бит 7 регистра header type: функция многофункционального устройства.
const HEADER_TYPE_MULTIFUNCTION: u8 = 0x80;
/// Тип заголовка «PCI-to-PCI bridge».
const HEADER_TYPE_BRIDGE: u8 = 0x01;

/// Значение `Vendor ID` для отсутствующей функции. Чтение несуществующей функции
/// возвращает все единицы — так шина сообщает «здесь никого нет».
const VENDOR_ID_NONE: u16 = 0xFFFF;

/// Бит 0 BAR: регион в пространстве ввода-вывода, а не в памяти.
const BAR_IO_SPACE: u32 = 1 << 0;
/// Биты 2:1 BAR памяти: тип. `0b10` — 64-битный, занимает два BAR подряд.
const BAR_TYPE_MASK: u32 = 0b11 << 1;
const BAR_TYPE_64BIT: u32 = 0b10 << 1;
/// Маска адреса в BAR памяти: младшие четыре бита — признаки.
const BAR_MEMORY_ADDR_MASK: u32 = !0xF;

// ---------------------------------------------------------------------------
// Классы устройств
// ---------------------------------------------------------------------------

/// Изготовитель, под которым выступают все устройства virtio.
pub const VENDOR_VIRTIO: u16 = 0x1AF4;
/// Идентификатор virtio-blk в переходном (transitional) виде — именно такой
/// создаёт QEMU по `-device virtio-blk-pci`. Переходное устройство понимает и
/// старый интерфейс через порты ввода-вывода, и современный через возможности
/// PCI; мы пользуемся только вторым, потому что портов на AArch64 не бывает.
pub const DEVICE_VIRTIO_BLK_LEGACY: u16 = 0x1001;
/// Он же в современном виде: 0x1040 плюс номер типа устройства (2 — блочное).
pub const DEVICE_VIRTIO_BLK_MODERN: u16 = 0x1042;
/// virtio-net в переходном виде — то, что создаёт `-device virtio-net-pci`.
pub const DEVICE_VIRTIO_NET_LEGACY: u16 = 0x1000;
/// Он же в современном виде: 0x1040 плюс единица (1 — сетевое устройство).
pub const DEVICE_VIRTIO_NET_MODERN: u16 = 0x1041;

/// Идентификатор возможности «vendor specific» — под ним virtio описывает, где
/// лежат его структуры.
pub const CAP_ID_VENDOR: u8 = 0x09;

/// Идентификатор возможности MSI-X.
pub const CAP_ID_MSIX: u8 = 0x11;

/// `Message Control` возможности MSI-X: смещение от её начала.
const MSIX_CONTROL: usize = 0x02;
/// `Table Offset / BIR`: смещение таблицы внутри BAR и номер самого BAR.
const MSIX_TABLE: usize = 0x04;
/// Бит 15 `Message Control`: MSI-X включён.
const MSIX_CONTROL_ENABLE: u16 = 1 << 15;
/// Бит 14: все векторы замаскированы независимо от их собственных масок.
const MSIX_CONTROL_FUNCTION_MASK: u16 = 1 << 14;
/// Биты 10:0 `Message Control`: число векторов **минус один**.
const MSIX_CONTROL_SIZE_MASK: u16 = 0x07FF;
/// Младшие три бита `Table Offset / BIR`: номер BAR, в котором лежит таблица.
const MSIX_TABLE_BIR_MASK: u32 = 0b111;

/// Где у устройства лежит таблица MSI-X.
///
/// Сама таблица живёт не в конфигурационном пространстве, а в памяти
/// устройства, поэтому здесь только адрес адреса: номер BAR и смещение внутри
/// него. Отображает BAR драйвер — он же и так это делает ради регистров.
#[derive(Debug, Clone, Copy)]
pub struct MsiX {
    /// Смещение самой возможности в конфигурационном пространстве.
    pub capability: usize,
    /// Номер BAR, в котором лежит таблица.
    pub bir: usize,
    /// Смещение таблицы внутри этого BAR.
    pub table_offset: u32,
    /// Сколько векторов устройство поддерживает.
    pub vectors: u16,
}

/// Размер одной записи таблицы MSI-X, байт: адрес (8), данные (4), управление (4).
pub const MSIX_ENTRY_SIZE: usize = 16;
/// Бит 0 поля `Vector Control`: вектор замаскирован.
const MSIX_VECTOR_MASKED: u32 = 1 << 0;

/// Базовый класс «Serial Bus Controller».
pub const CLASS_SERIAL_BUS: u8 = 0x0C;
/// Подкласс «USB Controller».
pub const SUBCLASS_USB: u8 = 0x03;
/// Program Interface «xHCI». Именно он отличает xHCI от UHCI (`0x00`), OHCI
/// (`0x10`) и EHCI (`0x20`) — все они тоже «USB Controller», но программируются
/// совершенно иначе.
pub const PROG_IF_XHCI: u8 = 0x30;

// ---------------------------------------------------------------------------
// MCFG
// ---------------------------------------------------------------------------

/// Смещение первой записи MCFG: заголовок SDT плюс восемь зарезервированных
/// байт.
const MCFG_ENTRIES_OFFSET: usize = SDT_HEADER_LEN + 8;
/// Размер одной записи MCFG.
const MCFG_ENTRY_LEN: usize = 16;

/// Окно ECAM одного сегмента PCI.
#[derive(Clone, Copy, Debug)]
pub struct Ecam {
    /// Физический адрес начала окна.
    base: u64,
    /// Номер сегмента. Машин с несколькими сегментами ядро пока не встречает,
    /// но различать их надо сразу: одинаковые номера шин в разных сегментах —
    /// это разные устройства.
    segment: u16,
    start_bus: u8,
    end_bus: u8,
}

impl Ecam {
    #[must_use]
    pub const fn segment(&self) -> u16 {
        self.segment
    }

    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    #[must_use]
    pub const fn buses(&self) -> (u8, u8) {
        (self.start_bus, self.end_bus)
    }

    /// Физический адрес конфигурационного пространства функции.
    fn config_phys(&self, bus: u8, device: u8, function: u8) -> Option<PhysAddr> {
        if bus < self.start_bus || bus > self.end_bus || device >= 32 || function >= 8 {
            return None;
        }
        let offset = (u64::from(bus - self.start_bus) << 20)
            | (u64::from(device) << 15)
            | (u64::from(function) << 12);
        Some(PhysAddr::new(self.base + offset))
    }
}

/// Как ядро добирается до конфигурационного пространства.
///
/// # Почему способов два
///
/// Потому что MCFG — таблица необязательная. У PCIe она есть почти всегда, и
/// окно ECAM даёт все 4096 байт пространства каждой функции; у машины с
/// «обычным» PCI её нет вовсе, и остаются порты 0xCF8/0xCFC — механизм, который
/// старше самой шины PCIe и работает на всём, что притворяется PC.
///
/// Цена вопроса выяснилась на VirtualBox: его чипсет по умолчанию (PIIX3) MCFG
/// не публикует, и ядро, знавшее только ECAM, печатало «no 'MCFG' table» и не
/// видело ни контроллера USB, ни диска. Устройства были на месте — не было
/// способа их спросить.
///
/// Разница между способами не только в наличии: через порты доступны первые 256
/// байт, то есть заголовок и обычный список возможностей. Расширенные
/// возможности PCIe (со смещения 0x100) остаются недостижимыми — но всё, чем
/// пользуется это ядро, включая MSI-X, лежит ниже.
#[derive(Clone, Copy)]
pub enum Root {
    /// Окно ECAM, описанное в MCFG.
    Ecam(Ecam),
    /// Порты ввода-вывода. Существует только там, где есть само пространство
    /// ввода-вывода, то есть на x86-64.
    Ports,
}

impl Root {
    /// Выбрать способ: сначала ECAM, при его отсутствии — порты.
    ///
    /// Порядок именно такой. ECAM даёт больше и не требует записи в порт перед
    /// каждым чтением; порты — запасной путь, и уходить на него, когда прошивка
    /// описала окно, значило бы менять проверенное на редко исполняемое.
    ///
    /// # Safety
    ///
    /// Требования те же, что у [`acpi::find_table`].
    pub unsafe fn discover(rsdp: u64) -> Result<Self, AcpiError> {
        // SAFETY: контракт функции.
        match unsafe { find_ecam(rsdp) } {
            Ok(ecam) => Ok(Self::Ecam(ecam)),
            Err(err) if arch::HAS_PCI_PORTS => {
                kprintln!("  pci         : no ECAM window ({err}); falling back to ports 0xCF8/0xCFC");
                Ok(Self::Ports)
            }
            Err(err) => Err(err),
        }
    }

    /// Первая и последняя шина, которые имеет смысл обходить.
    const fn buses(&self) -> (u8, u8) {
        match self {
            Self::Ecam(ecam) => (ecam.start_bus, ecam.end_bus),
            // Через порты доступны все 256 шин: номер шины — часть адреса, и
            // ограничивать его нечем, кроме мостов, за которыми никого нет.
            Self::Ports => (0, 255),
        }
    }

    /// Номер сегмента. У портов сегмент всегда нулевой: сегменты — понятие
    /// PCIe, а механизм №1 о них не знает.
    const fn segment(&self) -> u16 {
        match self {
            Self::Ecam(ecam) => ecam.segment,
            Self::Ports => 0,
        }
    }
}

impl core::fmt::Display for Root {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Ecam(ecam) => write!(
                f,
                "ECAM at {:#012x}, segment {}, buses {}..={}",
                ecam.base, ecam.segment, ecam.start_bus, ecam.end_bus
            ),
            Self::Ports => f.write_str("configuration ports 0xCF8/0xCFC, no ECAM window"),
        }
    }
}

/// Найти окно ECAM в таблице MCFG.
///
/// Берётся первая запись: несколько записей означают несколько сегментов, а
/// устройство ядро ищет по всей машине и остановится на первом подходящем.
/// Поддержка второго сегмента — это цикл по записям здесь и ничего больше, но
/// заводить его без машины, на которой это можно проверить, незачем.
///
/// # Safety
///
/// Требования те же, что у [`acpi::find_table`].
pub unsafe fn find_ecam(rsdp: u64) -> Result<Ecam, AcpiError> {
    // SAFETY: контракт функции.
    let bytes = unsafe { acpi::find_table(rsdp, b"MCFG") }?;
    if bytes.len() < MCFG_ENTRIES_OFFSET + MCFG_ENTRY_LEN {
        return Err(AcpiError::NotFound(*b"MCFG"));
    }
    let entry = MCFG_ENTRIES_OFFSET;
    Ok(Ecam {
        base: read_u64(bytes, entry),
        segment: read_u16(bytes, entry + 8),
        start_bus: bytes[entry + 10],
        end_bus: bytes[entry + 11],
    })
}

// ---------------------------------------------------------------------------
// Отображение окна
// ---------------------------------------------------------------------------

/// Сколько байт занимает конфигурационное пространство одной шины.
const BUS_WINDOW: usize = 1 << 20;

/// Какие шины уже отображены: по биту на шину.
///
/// Повторное отображение того же диапазона не ошибка (запись просто повторяется),
/// но обход дерева таблиц на каждое чтение регистра — расточительство, а сама
/// проверка стоит один сдвиг.
static MAPPED_BUSES: crate::sync::SpinLock<[u64; 4]> = crate::sync::SpinLock::new([0; 4]);

/// Отобразить конфигурационное пространство шины как память устройства.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц, и в это время никто
/// другой не должен их править.
unsafe fn map_bus(ecam: &Ecam, bus: u8) -> Result<(), MapError> {
    {
        let mapped = MAPPED_BUSES.lock();
        if mapped[usize::from(bus) / 64] & (1u64 << (bus % 64)) != 0 {
            return Ok(());
        }
    }

    let Some(phys) = ecam.config_phys(bus, 0, 0) else {
        return Err(MapError::Misaligned);
    };
    let virt = phys.to_direct_map();
    let flags = PageFlags::READ | PageFlags::WRITE | PageFlags::DEVICE;

    // SAFETY: условия делегированы вызывающему. Окно ECAM — это регистры
    // корневого комплекса, а не память: Device-семантика для него обязательна, и
    // прямое отображение взаимно однозначно, поэтому эти адреса не могут
    // пересечься ни с кодом, ни со стеком.
    unsafe { arch::map_active(virt, phys, BUS_WINDOW, flags) }?;

    let mut mapped = MAPPED_BUSES.lock();
    mapped[usize::from(bus) / 64] |= 1u64 << (bus % 64);
    Ok(())
}

// ---------------------------------------------------------------------------
// Функция на шине
// ---------------------------------------------------------------------------

/// Адрес функции на шине.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub segment: u16,
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl core::fmt::Display for Address {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{}",
            self.segment, self.bus, self.device, self.function
        )
    }
}

/// Найденное устройство и его конфигурационное пространство.
#[derive(Clone, Copy)]
pub struct Device {
    pub address: Address,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub revision: u8,
    /// Чем читается конфигурационное пространство этой функции.
    config: Access,
}

/// Способ добраться до конфигурационного пространства одной функции.
#[derive(Clone, Copy)]
enum Access {
    /// Виртуальный адрес начала пространства в окне ECAM.
    Memory(VirtAddr),
    /// Через порты: адрес собирается из номеров шины, устройства и функции.
    Ports,
}

/// Собрать значение для порта 0xCF8.
///
/// Бит 31 — «обращение разрешено», дальше шина, устройство, функция и смещение.
/// Два младших бита смещения обнуляются: механизм №1 адресует двойными словами,
/// и невыровненный адрес читает соседний регистр, а не тот, который просили.
const fn port_address(address: Address, offset: usize) -> u32 {
    0x8000_0000
        | ((address.bus as u32) << 16)
        | ((address.device as u32) << 11)
        | ((address.function as u32) << 8)
        | ((offset as u32) & 0xFC)
}

impl Device {
    /// # Safety
    ///
    /// Страница конфигурационного пространства должна быть отображена, а
    /// смещение — лежать внутри неё (4096 байт).
    unsafe fn read8(&self, offset: usize) -> u8 {
        debug_assert!(offset < PAGE_SIZE);
        match self.config {
            // SAFETY: контракт функции. `volatile` обязателен: это регистры.
            Access::Memory(base) => unsafe {
                (base.as_usize() as *const u8).add(offset).read_volatile()
            },
            // Порты отдают только двойные слова, поэтому нужный байт вырезается
            // сдвигом. Читать «поменьше» механизм №1 не умеет вовсе.
            // SAFETY: см. выше.
            Access::Ports => {
                let word = unsafe { arch::pci_config_read32(port_address(self.address, offset)) };
                (word >> ((offset & 3) * 8)) as u8
            }
        }
    }

    /// # Safety
    ///
    /// См. [`Device::read8`]; смещение обязано быть кратно 2.
    unsafe fn read16(&self, offset: usize) -> u16 {
        debug_assert!(offset + 1 < PAGE_SIZE && offset % 2 == 0);
        match self.config {
            // SAFETY: контракт функции.
            Access::Memory(base) => unsafe {
                (base.as_usize() as *const u16).byte_add(offset).read_volatile()
            },
            // SAFETY: см. выше.
            Access::Ports => {
                let word = unsafe { arch::pci_config_read32(port_address(self.address, offset)) };
                (word >> ((offset & 2) * 8)) as u16
            }
        }
    }

    /// # Safety
    ///
    /// См. [`Device::read8`]; смещение обязано быть кратно 4.
    unsafe fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + 3 < PAGE_SIZE && offset % 4 == 0);
        match self.config {
            // SAFETY: контракт функции.
            Access::Memory(base) => unsafe {
                (base.as_usize() as *const u32).byte_add(offset).read_volatile()
            },
            // SAFETY: см. выше.
            Access::Ports => unsafe {
                arch::pci_config_read32(port_address(self.address, offset))
            },
        }
    }

    /// # Safety
    ///
    /// См. [`Device::read16`]. Запись меняет поведение устройства на шине.
    unsafe fn write16(&self, offset: usize, value: u16) {
        debug_assert!(offset + 1 < PAGE_SIZE && offset % 2 == 0);
        match self.config {
            // SAFETY: контракт функции.
            Access::Memory(base) => unsafe {
                (base.as_usize() as *mut u16).byte_add(offset).write_volatile(value);
            },
            // Через порты слово нельзя записать иначе как в составе двойного:
            // читаем соседнюю половину, подставляем свою, пишем обратно. Гонки
            // здесь нет — ядро однопоточно, а обработчики прерываний в
            // конфигурационное пространство не лезут.
            // SAFETY: см. выше.
            Access::Ports => unsafe {
                let address = port_address(self.address, offset);
                let word = arch::pci_config_read32(address);
                let shift = (offset & 2) * 8;
                let merged = (word & !(0xFFFF << shift)) | (u32::from(value) << shift);
                arch::pci_config_write32(address, merged);
            },
        }
    }

    /// Разрешить устройству отвечать на обращения к памяти и быть инициатором
    /// DMA, попутно закрыв ему линию прерывания INTx.
    ///
    /// Прошивка UEFI обычно уже сделала первое (её собственные драйверы работали
    /// с устройством), но полагаться на это нельзя: `ExitBootServices` вправе
    /// оставить устройства в любом состоянии, а часть прошивок сознательно
    /// выключает всё, что не нужно для загрузки.
    ///
    /// # Safety
    ///
    /// Bus Master означает, что устройство начнёт обращаться к памяти по тем
    /// адресам, которые ему сообщили. Включать его до того, как кольца
    /// дескрипторов построены и обнулены, — значит разрешить чтение мусора.
    pub unsafe fn enable_bus_master(&self) {
        // SAFETY: контракт функции; смещение регистра `Command` фиксировано
        // спецификацией PCI.
        let command = unsafe { self.read16(CFG_COMMAND) };
        let wanted = command | COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER | COMMAND_INTX_DISABLE;
        // SAFETY: см. выше.
        unsafe { self.write16(CFG_COMMAND, wanted) };
    }

    /// Найти возможность MSI-X, если устройство её объявляет.
    #[must_use]
    pub fn msix(&self) -> Option<MsiX> {
        let mut found = None;
        self.for_each_capability(|id, offset| {
            if id == CAP_ID_MSIX {
                found = Some(offset);
                return false;
            }
            true
        });
        let capability = found?;

        // SAFETY: смещение получено обходом списка возможностей, который уже
        // проверил, что оно внутри отображённой страницы.
        let control = unsafe { self.read16(capability + MSIX_CONTROL) };
        let table = self.config32(capability + MSIX_TABLE);

        Some(MsiX {
            capability,
            bir: (table & MSIX_TABLE_BIR_MASK) as usize,
            // Младшие три бита заняты номером BAR: таблица выровнена на восемь
            // байт, и место под них взяли оттуда.
            table_offset: table & !MSIX_TABLE_BIR_MASK,
            // В поле лежит число векторов минус один — нуля векторов у MSI-X не
            // бывает по определению.
            vectors: (control & MSIX_CONTROL_SIZE_MASK) + 1,
        })
    }

    /// Записать вектор в таблицу MSI-X и разрешить доставку.
    ///
    /// `table` — виртуальный адрес начала таблицы (BAR, отображённый драйвером,
    /// плюс [`MsiX::table_offset`]). `address` и `data` описывают, куда и что
    /// устройство запишет, чтобы прервать процессор: это не «номер линии», а
    /// самая обычная запись в память, которую перехватывает контроллер
    /// прерываний. Что именно туда класть, знает арх-часть.
    ///
    /// Маска снимается **после** записи адреса и данных: замаскированный вектор
    /// — единственный способ гарантировать, что устройство не прервёт процессор
    /// по недописанной записи.
    ///
    /// # Safety
    ///
    /// `table` обязан указывать на отображённую таблицу MSI-X этого устройства,
    /// а `index` — быть меньше [`MsiX::vectors`]. Обработчик по указанному
    /// адресу и данным должен быть установлен до вызова: прерывание может
    /// прийти немедленно.
    pub unsafe fn set_msix_vector(&self, msix: &MsiX, table: usize, index: usize, address: u64, data: u32) {
        let entry = table + index * MSIX_ENTRY_SIZE;
        // SAFETY: контракт функции; запись 32-битная, как требует спецификация —
        // таблица не обязана поддерживать обращения другой ширины.
        unsafe {
            core::ptr::write_volatile(entry as *mut u32, address as u32);
            core::ptr::write_volatile((entry + 4) as *mut u32, (address >> 32) as u32);
            core::ptr::write_volatile((entry + 8) as *mut u32, data);
            core::ptr::write_volatile((entry + 12) as *mut u32, 0); // маска снята
        }

        // SAFETY: смещение внутри возможности, найденной обходом списка.
        let control = unsafe { self.read16(msix.capability + MSIX_CONTROL) };
        let wanted = (control | MSIX_CONTROL_ENABLE) & !MSIX_CONTROL_FUNCTION_MASK;
        // SAFETY: см. выше.
        unsafe { self.write16(msix.capability + MSIX_CONTROL, wanted) };
    }

    /// Замаскировать вектор — например, при остановке драйвера.
    ///
    /// # Safety
    ///
    /// Те же требования к `table` и `index`, что и у [`Device::set_msix_vector`].
    pub unsafe fn mask_msix_vector(table: usize, index: usize) {
        let entry = table + index * MSIX_ENTRY_SIZE;
        // SAFETY: контракт функции.
        unsafe { core::ptr::write_volatile((entry + 12) as *mut u32, MSIX_VECTOR_MASKED) };
    }

    /// Байт конфигурационного пространства.
    ///
    /// Безопасная обёртка: устройство получено перебором, значит его страница
    /// отображена, а смещение проверяется здесь. Нужна драйверам, которые
    /// разбирают список возможностей, — у virtio там лежат адреса всех его
    /// структур.
    #[must_use]
    pub fn config8(&self, offset: usize) -> u8 {
        if offset >= PAGE_SIZE {
            return 0;
        }
        // SAFETY: страница отображена при перечислении, смещение проверено.
        unsafe { self.read8(offset) }
    }

    /// Слово конфигурационного пространства. Невыровненное смещение даёт ноль:
    /// невыровненное обращение к регистрам PCI — ошибка вызывающего, но ронять
    /// из-за неё ядро незачем.
    #[must_use]
    pub fn config32(&self, offset: usize) -> u32 {
        if offset % 4 != 0 || offset + 4 > PAGE_SIZE {
            return 0;
        }
        // SAFETY: см. `config8`.
        unsafe { self.read32(offset) }
    }

    /// Пройти список возможностей, вызывая `visit(id, offset)`.
    ///
    /// Обход прекращается, когда `visit` возвращает `false`.
    pub fn for_each_capability(&self, mut visit: impl FnMut(u8, usize) -> bool) {
        // SAFETY: страница отображена при перечислении.
        let status = unsafe { self.read16(CFG_STATUS) };
        if status & STATUS_CAPABILITIES == 0 {
            return;
        }

        // Предел обхода обязателен: список — это односвязная цепочка внутри
        // конфигурационного пространства, и запись, ссылающаяся сама на себя,
        // увела бы ядро в вечный цикл. 48 записей — больше, чем помещается в
        // 256 байт стандартного заголовка при минимальном размере записи.
        const MAX_CAPABILITIES: usize = 48;

        let mut offset = usize::from(self.config8(CFG_CAPABILITIES_PTR)) & !0b11;
        for _ in 0..MAX_CAPABILITIES {
            // Нулевое смещение — конец списка. Указатель внутрь первых 64 байт
            // заголовка невозможен: там стандартные регистры.
            if offset < 0x40 || offset + 2 > PAGE_SIZE {
                return;
            }
            let id = self.config8(offset);
            if !visit(id, offset) {
                return;
            }
            offset = usize::from(self.config8(offset + 1)) & !0b11;
        }
    }

    /// Текущее значение регистра `Command` — для диагностики.
    #[must_use]
    pub fn command(&self) -> u16 {
        // SAFETY: страница отображена (устройство получено перебором), смещение
        // внутри неё.
        unsafe { self.read16(CFG_COMMAND) }
    }

    /// Физический адрес и размер BAR памяти.
    ///
    /// Размер не определяется: узнать его можно только записью всех единиц в BAR
    /// с последующим чтением, а это на время ломает уже настроенное прошивкой
    /// отображение. Ядру размер и не нужен — сколько регистров у контроллера,
    /// сообщает сам контроллер (у xHCI это поле `Capability Register Length` и
    /// смещения внутри его же окна).
    ///
    /// Возвращает `None`, если BAR не занят, описывает пространство
    /// ввода-вывода или его старшая половина ушла за пределы заголовка.
    #[must_use]
    pub fn memory_bar(&self, index: usize) -> Option<PhysAddr> {
        if index >= 6 {
            return None;
        }
        let offset = CFG_BAR0 + index * 4;
        // SAFETY: смещения BAR лежат в первых 64 байтах заголовка, страница
        // отображена.
        let low = unsafe { self.read32(offset) };
        if low & BAR_IO_SPACE != 0 {
            return None;
        }
        let mut address = u64::from(low & BAR_MEMORY_ADDR_MASK);
        if low & BAR_TYPE_MASK == BAR_TYPE_64BIT {
            if index + 1 >= 6 {
                return None;
            }
            // SAFETY: см. выше.
            let high = unsafe { self.read32(offset + 4) };
            address |= u64::from(high) << 32;
        }
        if address == 0 {
            return None;
        }
        Some(PhysAddr::new(address))
    }
}

// ---------------------------------------------------------------------------
// Перебор
// ---------------------------------------------------------------------------

/// Насколько глубоко заходить за мосты.
///
/// Предел обязателен, а не на всякий случай: испорченный регистр `Secondary Bus`
/// может указывать на ту же шину, за которой мы уже стоим, и обход без предела
/// уйдёт в бесконечную рекурсию — то есть неверное значение в регистре уронит
/// ядро переполнением стека.
const MAX_BRIDGE_DEPTH: usize = 8;

/// Прочитать функцию, если она существует.
///
/// # Safety
///
/// См. [`map_bus`].
unsafe fn probe(root: &Root, bus: u8, device: u8, function: u8) -> Option<Device> {
    if device >= 32 || function >= 8 {
        return None;
    }
    let access = match root {
        Root::Ecam(ecam) => {
            // SAFETY: контракт функции.
            unsafe { map_bus(ecam, bus) }.ok()?;
            Access::Memory(ecam.config_phys(bus, device, function)?.to_direct_map())
        }
        // Портам отображать нечего: конфигурационное пространство здесь не
        // память, а пара регистров, доступных всегда.
        Root::Ports => Access::Ports,
    };

    let probe = Device {
        address: Address { segment: root.segment(), bus, device, function },
        vendor: 0,
        device: 0,
        class: 0,
        subclass: 0,
        prog_if: 0,
        revision: 0,
        config: access,
    };

    // SAFETY: страница шины отображена вызовом `map_bus` выше — либо доступ идёт
    // через порты, которым отображение не нужно.
    let vendor = unsafe { probe.read16(CFG_VENDOR_ID) };
    if vendor == VENDOR_ID_NONE {
        return None;
    }

    // SAFETY: см. выше.
    unsafe {
        Some(Device {
            vendor,
            device: probe.read16(CFG_DEVICE_ID),
            class: probe.read8(CFG_CLASS),
            subclass: probe.read8(CFG_SUBCLASS),
            prog_if: probe.read8(CFG_PROG_IF),
            revision: probe.read8(CFG_REVISION),
            ..probe
        })
    }
}

/// Обойти шину и всё, что за её мостами, вызывая `visit` для каждой функции.
///
/// Обход прекращается, когда `visit` возвращает `false`.
///
/// # Safety
///
/// См. [`map_bus`].
unsafe fn walk_bus(
    root: &Root,
    bus: u8,
    depth: usize,
    visit: &mut impl FnMut(&Device) -> bool,
) -> bool {
    for device in 0..32u8 {
        // SAFETY: контракт функции.
        let Some(first) = (unsafe { probe(root, bus, device, 0) }) else {
            // Функция 0 отсутствует — значит устройства нет вовсе. Так требует
            // спецификация: остальные функции без нулевой не существуют, и
            // перебирать их незачем.
            continue;
        };

        // SAFETY: страница отображена.
        let header = unsafe { first.read8(CFG_HEADER_TYPE) };
        let functions = if header & HEADER_TYPE_MULTIFUNCTION != 0 { 8 } else { 1 };

        for function in 0..functions {
            let found = if function == 0 {
                Some(first)
            } else {
                // SAFETY: контракт функции.
                unsafe { probe(root, bus, device, function) }
            };
            let Some(found) = found else {
                continue;
            };

            if !visit(&found) {
                return false;
            }

            // SAFETY: страница отображена.
            let kind = unsafe { found.read8(CFG_HEADER_TYPE) } & !HEADER_TYPE_MULTIFUNCTION;
            if kind == HEADER_TYPE_BRIDGE && depth < MAX_BRIDGE_DEPTH {
                // SAFETY: см. выше.
                let secondary = unsafe { found.read8(CFG_SECONDARY_BUS) };
                // Мост, ведущий на свою же шину (или назад), — испорченная
                // конфигурация. Проверка и есть то, что делает рекурсию
                // конечной, помимо предела глубины.
                if secondary > bus {
                    // SAFETY: контракт функции.
                    if !unsafe { walk_bus(root, secondary, depth + 1, visit) } {
                        return false;
                    }
                }
            }
        }
    }
    true
}

/// Обойти всю машину, вызывая `visit` для каждой найденной функции.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn for_each(root: &Root, mut visit: impl FnMut(&Device) -> bool) {
    // SAFETY: контракт функции.
    unsafe { walk_bus(root, root.buses().0, 0, &mut visit) };
}

/// Найти первое устройство заданного изготовителя с одним из перечисленных
/// идентификаторов.
///
/// Список, а не одно значение: у virtio переходное и современное устройства —
/// это два разных идентификатора при одном и том же программном интерфейсе, и
/// какой из них создаст QEMU, зависит от версии и ключей запуска.
///
/// # Safety
///
/// См. [`for_each`].
pub unsafe fn find_by_id(root: &Root, vendor: u16, devices: &[u16]) -> Option<Device> {
    let mut found = None;
    // SAFETY: контракт функции.
    unsafe {
        for_each(root, |device| {
            if device.vendor == vendor && devices.contains(&device.device) {
                found = Some(*device);
                return false;
            }
            true
        });
    }
    found
}

/// Найти первое устройство с заданными классом, подклассом и интерфейсом.
///
/// # Safety
///
/// См. [`for_each`].
pub unsafe fn find_by_class(
    root: &Root,
    class: u8,
    subclass: u8,
    prog_if: u8,
) -> Option<Device> {
    let mut found = None;
    // SAFETY: контракт функции.
    unsafe {
        for_each(root, |device| {
            if device.class == class && device.subclass == subclass && device.prog_if == prog_if {
                found = Some(*device);
                return false;
            }
            true
        });
    }
    found
}
