//! Шина PCI Express: найти устройство и узнать, где его регистры.
//!
//! # Почему только ECAM и никаких портов ввода-вывода
//!
//! Исторический способ добраться до конфигурационного пространства на PC — пара
//! портов `0xCF8`/`0xCFC`. Он здесь не реализован, и это не упущение: портов
//! ввода-вывода на AArch64 не существует вовсе, а весь смысл этого модуля в том,
//! чтобы драйвер xHCI был один на обе архитектуры. ECAM (Enhanced Configuration
//! Access Mechanism) — отображённое в память конфигурационное пространство,
//! введённое вместе с PCI Express, — работает одинаково всюду и вдобавок
//! адресует 4096 байт на функцию вместо 256.
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
const CFG_HEADER_TYPE: usize = 0x0E;
const CFG_BAR0: usize = 0x10;
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
    /// Виртуальный адрес начала конфигурационного пространства этой функции.
    config: VirtAddr,
}

impl Device {
    /// # Safety
    ///
    /// Страница конфигурационного пространства должна быть отображена, а
    /// смещение — лежать внутри неё (4096 байт).
    unsafe fn read8(&self, offset: usize) -> u8 {
        debug_assert!(offset < PAGE_SIZE);
        // SAFETY: контракт функции. `volatile` обязателен: это регистры.
        unsafe { (self.config.as_usize() as *const u8).add(offset).read_volatile() }
    }

    /// # Safety
    ///
    /// См. [`Device::read8`]; смещение обязано быть кратно 2.
    unsafe fn read16(&self, offset: usize) -> u16 {
        debug_assert!(offset + 1 < PAGE_SIZE && offset % 2 == 0);
        // SAFETY: контракт функции.
        unsafe { (self.config.as_usize() as *const u16).byte_add(offset).read_volatile() }
    }

    /// # Safety
    ///
    /// См. [`Device::read8`]; смещение обязано быть кратно 4.
    unsafe fn read32(&self, offset: usize) -> u32 {
        debug_assert!(offset + 3 < PAGE_SIZE && offset % 4 == 0);
        // SAFETY: контракт функции.
        unsafe { (self.config.as_usize() as *const u32).byte_add(offset).read_volatile() }
    }

    /// # Safety
    ///
    /// См. [`Device::read16`]. Запись меняет поведение устройства на шине.
    unsafe fn write16(&self, offset: usize, value: u16) {
        debug_assert!(offset + 1 < PAGE_SIZE && offset % 2 == 0);
        // SAFETY: контракт функции.
        unsafe { (self.config.as_usize() as *mut u16).byte_add(offset).write_volatile(value) };
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
unsafe fn probe(ecam: &Ecam, bus: u8, device: u8, function: u8) -> Option<Device> {
    // SAFETY: контракт функции.
    unsafe { map_bus(ecam, bus) }.ok()?;
    let phys = ecam.config_phys(bus, device, function)?;

    let probe = Device {
        address: Address { segment: ecam.segment, bus, device, function },
        vendor: 0,
        device: 0,
        class: 0,
        subclass: 0,
        prog_if: 0,
        revision: 0,
        config: phys.to_direct_map(),
    };

    // SAFETY: страница шины отображена вызовом `map_bus` выше.
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
    ecam: &Ecam,
    bus: u8,
    depth: usize,
    visit: &mut impl FnMut(&Device) -> bool,
) -> bool {
    for device in 0..32u8 {
        // SAFETY: контракт функции.
        let Some(first) = (unsafe { probe(ecam, bus, device, 0) }) else {
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
                unsafe { probe(ecam, bus, device, function) }
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
                    if !unsafe { walk_bus(ecam, secondary, depth + 1, visit) } {
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
pub unsafe fn for_each(ecam: &Ecam, mut visit: impl FnMut(&Device) -> bool) {
    // SAFETY: контракт функции.
    unsafe { walk_bus(ecam, ecam.start_bus, 0, &mut visit) };
}

/// Найти первое устройство с заданными классом, подклассом и интерфейсом.
///
/// # Safety
///
/// См. [`for_each`].
pub unsafe fn find_by_class(
    ecam: &Ecam,
    class: u8,
    subclass: u8,
    prog_if: u8,
) -> Option<Device> {
    let mut found = None;
    // SAFETY: контракт функции.
    unsafe {
        for_each(ecam, |device| {
            if device.class == class && device.subclass == subclass && device.prog_if == prog_if {
                found = Some(*device);
                return false;
            }
            true
        });
    }
    found
}
