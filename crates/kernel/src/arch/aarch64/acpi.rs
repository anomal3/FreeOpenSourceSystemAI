//! Разбор MADT: где на этой машине контроллер прерываний и какой он.
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
