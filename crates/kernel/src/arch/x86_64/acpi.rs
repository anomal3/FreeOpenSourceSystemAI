//! MADT: где искать I/O APIC и куда на самом деле заведены IRQ шины ISA.
//!
//! Общий обход таблиц ACPI живёт в [`crate::acpi`] — он нужен обеим
//! архитектурам. Здесь только то, что осмысленно исключительно на x86-64.
//!
//! # Зачем это понадобилось
//!
//! Прерывания устройств на x86-64 приходят не в процессор напрямую, а через
//! I/O APIC, и его адрес архитектурой не задан. Значение `0xFEC0_0000`, которое
//! приводят все руководства, — это лишь то, что выставляет прошивка на
//! подавляющем большинстве машин; чипсет вправе поставить контроллер куда угодно
//! и даже иметь их несколько.
//!
//! Вторая причина важнее адреса: **Interrupt Source Override**. Древние IRQ шины
//! ISA соответствуют входам I/O APIC один в один далеко не всегда. Самый
//! известный случай — таймер PIT: IRQ 0 почти на всех машинах заведён на вход
//! GSI 2, а не 0. Если положиться на равенство «IRQ = GSI», часть устройств
//! окажется настроена на чужой вход, и это не даст ошибки — просто прерывания не
//! будут приходить. Хуже диагностируемого симптома придумать сложно.

use crate::acpi::{AcpiError, SDT_HEADER_LEN, read_u16, read_u32};

/// Сколько I/O APIC ядро согласно запомнить.
///
/// На односокетных машинах он один; два-четыре встречаются на серверах. Больше
/// восьми — признак того, что мы читаем не MADT.
const MAX_IO_APICS: usize = 8;

/// Сколько переопределений источников ядро согласно запомнить. Типичная
/// прошивка объявляет одно-два (IRQ 0 и IRQ 9).
const MAX_OVERRIDES: usize = 24;

/// Тип записи «I/O APIC» (ACPI 6.5, 5.2.12.3).
const MADT_ENTRY_IO_APIC: u8 = 1;
/// Тип записи «Interrupt Source Override» (5.2.12.5).
const MADT_ENTRY_SOURCE_OVERRIDE: u8 = 2;

/// Минимальная длина записи с заголовком `type` + `length`.
const MADT_ENTRY_HEADER_LEN: usize = 2;

/// Один I/O APIC.
#[derive(Clone, Copy, Debug)]
pub struct IoApic {
    pub id: u8,
    /// Физический адрес окна регистров.
    pub address: u32,
    /// Номер первого GSI, который обслуживает этот контроллер.
    pub gsi_base: u32,
}

impl IoApic {
    const EMPTY: Self = Self { id: 0, address: 0, gsi_base: 0 };
}

/// Переопределение соответствия «IRQ шины ISA → GSI».
#[derive(Clone, Copy, Debug)]
pub struct SourceOverride {
    /// Номер IRQ на шине ISA.
    pub source: u8,
    /// Вход I/O APIC, на который он на самом деле заведён.
    pub gsi: u32,
    /// Флаги MPS INTI: биты 0..1 — полярность, 2..3 — тип срабатывания.
    pub flags: u16,
}

impl SourceOverride {
    const EMPTY: Self = Self { source: 0, gsi: 0, flags: 0 };
}

/// Полярность из флагов MPS INTI: `0b01` — active high, `0b11` — active low,
/// `0b00` — «как принято на шине» (для ISA это active high).
const INTI_POLARITY_MASK: u16 = 0b11;
const INTI_POLARITY_ACTIVE_LOW: u16 = 0b11;
/// Тип срабатывания: `0b01` — по фронту, `0b11` — по уровню, `0b00` — как
/// принято на шине (для ISA это фронт).
const INTI_TRIGGER_MASK: u16 = 0b11 << 2;
const INTI_TRIGGER_LEVEL: u16 = 0b11 << 2;

impl SourceOverride {
    /// Активный уровень — низкий.
    #[must_use]
    pub const fn active_low(self) -> bool {
        self.flags & INTI_POLARITY_MASK == INTI_POLARITY_ACTIVE_LOW
    }

    /// Срабатывание по уровню, а не по фронту.
    #[must_use]
    pub const fn level_triggered(self) -> bool {
        self.flags & INTI_TRIGGER_MASK == INTI_TRIGGER_LEVEL
    }
}

/// Всё, что ядру нужно из MADT.
pub struct Madt {
    io_apics: [IoApic; MAX_IO_APICS],
    io_apic_count: usize,
    overrides: [SourceOverride; MAX_OVERRIDES],
    override_count: usize,
    /// Сколько записей не поместилось в массивы выше. Ненулевое значение —
    /// повод напечатать предупреждение, а не тихо работать с частью таблицы.
    pub truncated: usize,
}

impl Madt {
    /// Найденные контроллеры.
    #[must_use]
    pub fn io_apics(&self) -> &[IoApic] {
        &self.io_apics[..self.io_apic_count]
    }

    /// Объявленные переопределения.
    #[must_use]
    pub fn overrides(&self) -> &[SourceOverride] {
        &self.overrides[..self.override_count]
    }

    /// В какой GSI на самом деле заведён этот IRQ шины ISA.
    ///
    /// Без переопределения номера совпадают — так требует спецификация: «если
    /// переопределения нет, IRQ шины ISA идентично соответствует GSI с тем же
    /// номером».
    #[must_use]
    pub fn gsi_for_irq(&self, irq: u8) -> (u32, Option<SourceOverride>) {
        for entry in self.overrides() {
            if entry.source == irq {
                return (entry.gsi, Some(*entry));
            }
        }
        (u32::from(irq), None)
    }

    /// Контроллер, обслуживающий этот GSI.
    ///
    /// Диапазон каждого контроллера MADT не сообщает — его надо спрашивать у
    /// самого железа (регистр `IOAPICVER` знает число входов). Поэтому здесь
    /// выбирается контроллер с наибольшим `gsi_base`, не превышающим искомый
    /// номер: именно так устроено разбиение пространства GSI между
    /// контроллерами.
    #[must_use]
    pub fn io_apic_for_gsi(&self, gsi: u32) -> Option<IoApic> {
        self.io_apics()
            .iter()
            .filter(|apic| apic.gsi_base <= gsi)
            .max_by_key(|apic| apic.gsi_base)
            .copied()
    }
}

/// Найти и разобрать MADT.
///
/// # Safety
///
/// Требования те же, что у [`crate::acpi::find_table`]: прямое отображение
/// активно, а память с таблицами ACPI ещё не переиспользована.
pub unsafe fn find_madt(rsdp: u64) -> Result<Madt, AcpiError> {
    // SAFETY: контракт функции.
    let bytes = unsafe { crate::acpi::find_table(rsdp, b"APIC") }?;
    parse_madt(bytes).ok_or(AcpiError::BadChecksum(*b"APIC"))
}

/// Разобрать записи MADT.
///
/// `bytes` — таблица целиком, уже с проверенной суммой.
fn parse_madt(bytes: &[u8]) -> Option<Madt> {
    /// Смещение первой записи: заголовок SDT, затем адрес локального APIC (u32)
    /// и флаги (u32).
    const MADT_ENTRIES_OFFSET: usize = SDT_HEADER_LEN + 8;

    if bytes.len() < MADT_ENTRIES_OFFSET {
        return None;
    }

    let mut madt = Madt {
        io_apics: [IoApic::EMPTY; MAX_IO_APICS],
        io_apic_count: 0,
        overrides: [SourceOverride::EMPTY; MAX_OVERRIDES],
        override_count: 0,
        truncated: 0,
    };

    let mut offset = MADT_ENTRIES_OFFSET;
    while offset + MADT_ENTRY_HEADER_LEN <= bytes.len() {
        let kind = bytes[offset];
        let length = bytes[offset + 1] as usize;
        // Нулевая длина — это бесконечный цикл, а не короткая запись. Длина
        // меньше заголовка — тоже: следующая позиция не сдвинулась бы вперёд.
        if length < MADT_ENTRY_HEADER_LEN || offset + length > bytes.len() {
            break;
        }

        match kind {
            // I/O APIC: id, reserved, address (u32), gsi_base (u32) — 12 байт.
            MADT_ENTRY_IO_APIC if length >= 12 => {
                if madt.io_apic_count < MAX_IO_APICS {
                    madt.io_apics[madt.io_apic_count] = IoApic {
                        id: bytes[offset + 2],
                        address: read_u32(bytes, offset + 4),
                        gsi_base: read_u32(bytes, offset + 8),
                    };
                    madt.io_apic_count += 1;
                } else {
                    madt.truncated += 1;
                }
            }
            // Interrupt Source Override: bus, source, gsi (u32), flags (u16) —
            // 10 байт.
            MADT_ENTRY_SOURCE_OVERRIDE if length >= 10 => {
                if madt.override_count < MAX_OVERRIDES {
                    madt.overrides[madt.override_count] = SourceOverride {
                        source: bytes[offset + 3],
                        gsi: read_u32(bytes, offset + 4),
                        flags: read_u16(bytes, offset + 8),
                    };
                    madt.override_count += 1;
                } else {
                    madt.truncated += 1;
                }
            }
            // Остальные типы записей (локальные APIC, NMI, x2APIC) ядру на этой
            // фазе не нужны: процессор один, и его APIC уже поднят.
            _ => {}
        }

        offset += length;
    }

    Some(madt)
}
