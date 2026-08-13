//! Разбор таблиц ACPI, нужных ARM: где контроллер прерываний и где консоль.
//!
//! # Почему это понадобилось
//!
//! Потому что адреса были зашиты константами под QEMU `virt`, и на первой же
//! чужой машине — VirtualBox на Apple Silicon — ядро напечатало «unsupported
//! interrupt controller (Unknown) at 0x08000000» и осталось без таймера и без
//! клавиатуры. Всё остальное работало: память, часы, initrd, PCI, xHCI. Не
//! работал ровно тот узел, адрес которого был угадан, а не прочитан.
//!
//! Правильный источник у ARM ровно один — таблицы прошивки. На платформах с
//! ACPI это MADT (она же `APIC`), где distributor описан записью типа
//! [`ENTRY_GICD`], а redistributor'ы GICv3 — записями [`ENTRY_GICR`] или полем
//! внутри записи процессора [`ENTRY_GICC`].
//!
//! # Чего здесь нет
//!
//! Device tree. Машина без ACPI (Raspberry Pi без UEFI, будущий телефон) сюда
//! не попадёт вовсе: у неё не будет и `BootInfo::acpi_rsdp`. Разбор FDT — это
//! отдельный формат и отдельная фаза, и делать его вслепую, не имея машины, где
//! его можно прогнать, значит писать код, который выглядит рабочим.

use crate::acpi::{self, AcpiError};

/// Запись MADT: процессорный интерфейс GIC.
const ENTRY_GICC: u8 = 0x0B;
/// Запись MADT: distributor.
const ENTRY_GICD: u8 = 0x0C;
/// Запись MADT: диапазон redistributor'ов (GICv3 и новее).
const ENTRY_GICR: u8 = 0x0E;

/// Длина заголовка таблицы ACPI, после которого начинаются записи.
const SDT_HEADER: usize = 36;
/// В MADT после общего заголовка идут ещё два поля: адрес локального APIC и
/// флаги. Для ARM они бессмысленны, но место занимают.
const MADT_FIXED: usize = 8;

/// То, что ядро узнало о контроллере прерываний.
#[derive(Debug, Clone, Copy)]
pub struct GicLayout {
    /// Физический адрес distributor'а.
    pub distributor: usize,
    /// Физический адрес процессорного интерфейса (GICv2). У GICv3 его нет —
    /// там интерфейс это системные регистры, а не окно памяти.
    pub cpu_interface: Option<usize>,
    /// Физический адрес redistributor'а этого ядра (GICv3).
    pub redistributor: Option<usize>,
    /// Версия, объявленная прошивкой: 2, 3, 4 — или 0, если она промолчала.
    pub version: u8,
}

/// Прочитать раскладку GIC из MADT.
///
/// `None`, если таблиц нет или в них нет distributor'а — тогда остаётся
/// довериться константам, и это состояние печатается вслух.
///
/// # Safety
///
/// `rsdp` обязан быть либо нулём, либо физическим адресом настоящего RSDP в
/// отображённой памяти.
pub unsafe fn find_gic(rsdp: u64) -> Option<GicLayout> {
    if rsdp == 0 {
        return None;
    }
    // SAFETY: контракт функции.
    let madt = match unsafe { acpi::find_table(rsdp, b"APIC") } {
        Ok(table) => table,
        Err(AcpiError::NotFound(_)) => return None,
        Err(_) => return None,
    };

    let mut layout = GicLayout {
        distributor: 0,
        cpu_interface: None,
        redistributor: None,
        version: 0,
    };

    let mut at = SDT_HEADER + MADT_FIXED;
    while at + 2 <= madt.len() {
        let kind = madt[at];
        let len = madt[at + 1] as usize;
        // Нулевая длина — испорченная таблица: без этой проверки обход зациклился
        // бы навсегда, причём на машине, о которой мы ничего не знаем.
        if len < 2 || at + len > madt.len() {
            break;
        }

        match kind {
            ENTRY_GICD if len >= 24 => {
                layout.distributor = acpi::read_u64(madt, at + 8) as usize;
                layout.version = madt[at + 20];
            }
            // У GICv2 здесь адрес процессорного интерфейса, у GICv3 — адрес
            // redistributor'а этого ядра. Берём оба: какой из них осмыслен,
            // решает версия.
            ENTRY_GICC if len >= 76 => {
                let cpu = acpi::read_u64(madt, at + 32) as usize;
                if cpu != 0 && layout.cpu_interface.is_none() {
                    layout.cpu_interface = Some(cpu);
                }
                let redistributor = acpi::read_u64(madt, at + 60) as usize;
                if redistributor != 0 && layout.redistributor.is_none() {
                    layout.redistributor = Some(redistributor);
                }
            }
            // Отдельная запись с диапазоном redistributor'ов. Она главнее поля
            // в записи процессора: прошивка вправе описать диапазон и оставить
            // поле нулевым.
            ENTRY_GICR if len >= 16 => {
                let base = acpi::read_u64(madt, at + 4) as usize;
                if base != 0 {
                    layout.redistributor = Some(base);
                }
            }
            _ => {}
        }

        at += len;
    }

    if layout.distributor == 0 {
        return None;
    }
    Some(layout)
}

// ---------------------------------------------------------------------------
// SPCR: где на этой машине консольный порт
// ---------------------------------------------------------------------------

/// Смещение поля `Interface Type` в SPCR.
const SPCR_INTERFACE: usize = 36;
/// Смещение структуры `Base Address` (Generic Address Structure).
const SPCR_BASE: usize = 40;
/// Внутри GAS: идентификатор адресного пространства и сам адрес.
const GAS_SPACE_ID: usize = 0;
const GAS_ADDRESS: usize = 4;
/// Адресное пространство «системная память».
const GAS_SPACE_MEMORY: u8 = 0;

/// Типы интерфейса, которые понимает драйвер PL011.
///
/// `0x03` — сам PL011; `0x0D` и `0x0E` — SBSA-вариант того же регистрового
/// набора (полный и 32-разрядный), совместимый с ним по всем регистрам, которые
/// этот драйвер трогает. Всё остальное (16550, DCC, MediaTek) — другое
/// устройство, и молча считать его PL011 значило бы писать байты в чужие
/// регистры.
const INTERFACE_PL011: u8 = 0x03;
const INTERFACE_SBSA_32: u8 = 0x0D;
const INTERFACE_SBSA: u8 = 0x0E;

/// Физический адрес консольного порта из SPCR.
///
/// # Зачем это ядру
///
/// Затем же, зачем MADT: адрес UART на ARM ничем не закреплён. У QEMU `virt`
/// это `0x0900_0000`, у VirtualBox на Apple Silicon — `0xffdd_f000`, и ядро,
/// знающее только первый, на второй машине немо. Немо буквально: там не было ни
/// одной строки журнала, и единственный способ понять, почему не работает
/// клавиатура, — снимок экрана, на котором журнал уже затёрт рабочим столом.
///
/// SPCR (Serial Port Console Redirection) — та самая таблица, в которой прошивка
/// говорит, куда она сама выводит консоль. Прошивка VirtualBox туда и пишет: её
/// вывод виден в файле, к которому подключён порт, — а ядро молчало.
///
/// `None` означает «таблицы нет либо описан не наш порт» — тогда остаётся
/// умолчание QEMU, и это состояние печатается вслух.
///
/// # Safety
///
/// См. [`find_gic`].
pub unsafe fn find_uart(rsdp: u64) -> Option<usize> {
    if rsdp == 0 {
        return None;
    }
    // SAFETY: контракт функции.
    if let Some(base) = unsafe { uart_from_spcr(rsdp) } {
        return Some(base);
    }
    // SAFETY: см. выше.
    unsafe { uart_from_dbg2(rsdp) }
}

/// Порт из SPCR — таблицы, которой прошивка объявляет консоль.
///
/// # Safety
///
/// См. [`find_gic`].
unsafe fn uart_from_spcr(rsdp: u64) -> Option<usize> {
    // SAFETY: контракт функции.
    let spcr = unsafe { acpi::find_table(rsdp, b"SPCR") }.ok()?;
    if spcr.len() < SPCR_BASE + 12 {
        return None;
    }

    let interface = spcr[SPCR_INTERFACE];
    if !matches!(interface, INTERFACE_PL011 | INTERFACE_SBSA | INTERFACE_SBSA_32) {
        return None;
    }
    // Порт в пространстве ввода-вывода на ARM невозможен физически: такого
    // пространства у архитектуры нет. Значение, отличное от «памяти», означает
    // испорченную таблицу либо порт, до которого этому драйверу не дотянуться.
    if spcr[SPCR_BASE + GAS_SPACE_ID] != GAS_SPACE_MEMORY {
        return None;
    }

    let base = acpi::read_u64(spcr, SPCR_BASE + GAS_ADDRESS) as usize;
    if base == 0 { None } else { Some(base) }
}

/// Порт из DBG2 — таблицы отладочных портов.
///
/// Она существует отдельно от SPCR и описывает то же самое устройство с другой
/// целью: SPCR отвечает на вопрос «куда прошивка выводит консоль», DBG2 — «какие
/// порты пригодны для отладки». Прошивка вправе объявить одну из них, обе или ни
/// одной, поэтому смотреть надо в обе: молчание системы стоит дороже двух
/// разборов по тридцать строк.
///
/// # Safety
///
/// См. [`find_gic`].
unsafe fn uart_from_dbg2(rsdp: u64) -> Option<usize> {
    /// Смещение списка устройств и их число в заголовке DBG2.
    const DBG2_LIST_OFFSET: usize = 36;
    const DBG2_LIST_COUNT: usize = 40;
    /// Поля записи Debug Device Information.
    const DEVICE_LENGTH: usize = 1;
    const DEVICE_REGISTER_COUNT: usize = 3;
    const DEVICE_PORT_TYPE: usize = 12;
    const DEVICE_PORT_SUBTYPE: usize = 14;
    const DEVICE_REGISTERS_OFFSET: usize = 18;
    /// Тип порта «последовательный».
    const PORT_TYPE_SERIAL: u16 = 0x8000;
    /// Подтипы, которые понимает драйвер PL011 (те же, что у SPCR).
    const SUBTYPE_PL011: u16 = 0x0003;
    const SUBTYPE_SBSA_32: u16 = 0x000D;
    const SUBTYPE_SBSA: u16 = 0x000E;

    // SAFETY: контракт функции.
    let dbg2 = unsafe { acpi::find_table(rsdp, b"DBG2") }.ok()?;
    if dbg2.len() < DBG2_LIST_COUNT + 4 {
        return None;
    }

    let mut at = acpi::read_u32(dbg2, DBG2_LIST_OFFSET) as usize;
    let count = acpi::read_u32(dbg2, DBG2_LIST_COUNT) as usize;

    for _ in 0..count.min(8) {
        if at + DEVICE_REGISTERS_OFFSET + 2 > dbg2.len() {
            break;
        }
        let length = usize::from(acpi::read_u16(dbg2, at + DEVICE_LENGTH));
        // Нулевая длина — испорченная таблица, а не пустая запись: без проверки
        // обход зациклился бы навсегда.
        if length < 22 || at + length > dbg2.len() {
            break;
        }

        let port_type = acpi::read_u16(dbg2, at + DEVICE_PORT_TYPE);
        let subtype = acpi::read_u16(dbg2, at + DEVICE_PORT_SUBTYPE);
        let registers = usize::from(acpi::read_u16(dbg2, at + DEVICE_REGISTERS_OFFSET));

        if port_type == PORT_TYPE_SERIAL
            && matches!(subtype, SUBTYPE_PL011 | SUBTYPE_SBSA | SUBTYPE_SBSA_32)
            && dbg2[at + DEVICE_REGISTER_COUNT] > 0
            && at + registers + 12 <= dbg2.len()
            && dbg2[at + registers + GAS_SPACE_ID] == GAS_SPACE_MEMORY
        {
            let base = acpi::read_u64(dbg2, at + registers + GAS_ADDRESS) as usize;
            if base != 0 {
                return Some(base);
            }
        }

        at += length;
    }
    None
}
