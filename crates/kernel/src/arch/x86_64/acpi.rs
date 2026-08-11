//! Разбор таблиц ACPI — ровно настолько, чтобы найти I/O APIC.
//!
//! # Зачем это понадобилось именно сейчас
//!
//! Прерывания устройств на x86-64 приходят не в процессор напрямую, а через
//! I/O APIC, и его адрес архитектурой не задан. Значение `0xFEC0_0000`,
//! которое приводят все руководства, — это лишь то, что выставляет прошивка
//! на подавляющем большинстве машин; чипсет вправе поставить контроллер куда
//! угодно и даже иметь их несколько. Единственный законный источник — таблица
//! MADT.
//!
//! Вторая причина важнее адреса: **Interrupt Source Override**. Древние IRQ
//! шины ISA соответствуют входам I/O APIC один в один далеко не всегда. Самый
//! известный случай — таймер PIT: IRQ 0 почти на всех машинах заведён на вход
//! GSI 2, а не 0. Если положиться на равенство «IRQ = GSI», часть устройств
//! окажется настроена на чужой вход, и это не даст ошибки — просто прерывания
//! не будут приходить. Хуже диагностируемого симптома придумать сложно.
//!
//! # Насколько это доверенные данные
//!
//! Не доверенные вовсе. Адрес RSDP приезжает от загрузчика, дальше идёт цепочка
//! указателей внутрь памяти, размеченной прошивкой. Поэтому проверяется всё:
//! сигнатуры, контрольные суммы (сумма всех байт таблицы обязана быть нулём по
//! модулю 256), длины на вменяемость и каждая запись — на то, что она целиком
//! лежит внутри таблицы. Любая нестыковка означает «таблицы нет», а не «читаем
//! дальше и надеемся».
//!
//! # Как читается физическая память
//!
//! Через прямое отображение ([`PhysAddr::to_direct_map`]). Identity-отображение
//! тоже подошло бы сегодня, но оно исчезнет при переезде ядра в верхнюю
//! половину, а прямое — нет.

use crate::mm::PhysAddr;

/// Заголовок любой системной таблицы ACPI (ACPI 6.5, 5.2.6).
const SDT_HEADER_LEN: usize = 36;

/// Смещения полей в заголовке.
const SDT_SIGNATURE: usize = 0;
const SDT_LENGTH: usize = 4;

/// RSDP версии 1.0 — первые 20 байт; контрольная сумма считается по ним.
const RSDP_V1_LEN: usize = 20;

/// Смещения полей RSDP.
const RSDP_SIGNATURE: usize = 0;
const RSDP_REVISION: usize = 15;
const RSDP_RSDT_ADDRESS: usize = 16;
const RSDP_LENGTH: usize = 20;
const RSDP_XSDT_ADDRESS: usize = 24;

const RSDP_SIGNATURE_BYTES: &[u8; 8] = b"RSD PTR ";

/// Верхняя граница длины таблицы, которую ядро согласно прочитать.
///
/// Настоящие DSDT доходят до сотен килобайт, но нас интересует только MADT —
/// она измеряется сотнями байт. Мегабайт — заведомо достаточный предел, за
/// которым начинается не «очень подробная таблица», а испорченное поле длины.
const MAX_TABLE_LEN: u32 = 1024 * 1024;

/// Сколько таблиц ядро согласно перебрать в XSDT.
const MAX_TABLES: usize = 64;

/// Сколько I/O APIC ядро согласно запомнить.
///
/// На односокетных машинах он один; два-четыре встречаются на серверах. Больше
/// восьми — признак того, что мы читаем не MADT.
const MAX_IO_APICS: usize = 8;

/// Сколько переопределений источников ядро согласно запомнить. Типичная
/// прошивка объявляет одно-два (IRQ 0 и IRQ 9).
const MAX_OVERRIDES: usize = 24;

// --- Записи MADT --------------------------------------------------------------

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

/// Прочитать `len` байт физической памяти как срез.
///
/// # Safety
///
/// Диапазон `phys..phys + len` обязан быть отображён прямым отображением (то
/// есть описан картой памяти и не выброшен как большое окно устройства) и не
/// изменяться параллельно. Для таблиц ACPI это выполняется: прошивка размещает
/// их в памяти типа `AcpiReclaimable`, а ядро её не переиспользует, пока
/// таблицы нужны.
unsafe fn phys_slice(phys: u64, len: usize) -> &'static [u8] {
    let virt = PhysAddr::new(phys).to_direct_map();
    // SAFETY: условия делегированы вызывающему контрактом функции. Время жизни
    // `'static` корректно: физическая память под таблицами не освобождается.
    unsafe { core::slice::from_raw_parts(virt.as_usize() as *const u8, len) }
}

/// Сумма байт по модулю 256. У корректной таблицы ACPI она равна нулю.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte))
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

/// Почему таблицы не удалось прочитать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpiError {
    /// Загрузчик не передал адрес RSDP: машина без ACPI либо прошивка, у
    /// которой не нашлось соответствующей конфигурационной таблицы UEFI.
    NoRsdp,
    /// По указанному адресу лежит не RSDP.
    BadRsdpSignature,
    /// Контрольная сумма RSDP не сходится.
    BadRsdpChecksum,
    /// Ни RSDT, ни XSDT не указаны.
    NoRootTable,
    /// Корневая таблица не прошла проверку.
    BadRootTable,
    /// Таблица MADT (`APIC`) в списке отсутствует.
    NoMadt,
    /// MADT нашлась, но не прошла проверку.
    BadMadt,
}

impl core::fmt::Display for AcpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::NoRsdp => "the bootloader passed no ACPI RSDP",
            Self::BadRsdpSignature => "no RSDP signature at the address given",
            Self::BadRsdpChecksum => "RSDP checksum mismatch",
            Self::NoRootTable => "RSDP names neither an RSDT nor an XSDT",
            Self::BadRootTable => "the root table failed validation",
            Self::NoMadt => "no MADT (signature 'APIC') among the tables",
            Self::BadMadt => "the MADT failed validation",
        };
        f.write_str(text)
    }
}

/// Найти и разобрать MADT.
///
/// `rsdp` — физический адрес RSDP из [`boot_info::BootInfo::acpi_rsdp`].
///
/// # Safety
///
/// Требования те же, что у [`phys_slice`]: прямое отображение активно, а память
/// с таблицами ACPI ещё не переиспользована.
pub unsafe fn find_madt(rsdp: u64) -> Result<Madt, AcpiError> {
    if rsdp == 0 {
        return Err(AcpiError::NoRsdp);
    }

    // SAFETY: контракт функции. Читаем сначала только 20 байт — версии 1.0
    // ровно столько и существует, и трогать 36-й байт до проверки ревизии
    // значило бы читать за концом таблицы на машине с ACPI 1.0.
    let head = unsafe { phys_slice(rsdp, RSDP_V1_LEN) };
    if &head[RSDP_SIGNATURE..RSDP_SIGNATURE + 8] != RSDP_SIGNATURE_BYTES {
        return Err(AcpiError::BadRsdpSignature);
    }
    if checksum(head) != 0 {
        return Err(AcpiError::BadRsdpChecksum);
    }

    let revision = head[RSDP_REVISION];
    let rsdt = u64::from(read_u32(head, RSDP_RSDT_ADDRESS));

    // Ревизия 0 — это ACPI 1.0, где полей XSDT физически нет. Читать их по
    // смещению 24 на такой машине означает читать чужую память и почти
    // наверняка получить мусорный 64-битный адрес.
    let xsdt = if revision >= 2 {
        // SAFETY: см. выше; расширенная часть существует при ревизии >= 2.
        let full_head = unsafe { phys_slice(rsdp, RSDP_LENGTH + 4) };
        let length = read_u32(full_head, RSDP_LENGTH) as usize;
        if length < RSDP_XSDT_ADDRESS + 8 || length > MAX_TABLE_LEN as usize {
            0
        } else {
            // SAFETY: длина проверена на вменяемость и покрывает поле XSDT.
            let full = unsafe { phys_slice(rsdp, length) };
            if checksum(full) != 0 {
                // Расширенная сумма не сошлась — расширенной части не верим и
                // работаем как с ACPI 1.0. Отказываться совсем незачем: RSDT
                // проверен собственной суммой и описывает те же таблицы.
                0
            } else {
                read_u64(full, RSDP_XSDT_ADDRESS)
            }
        }
    } else {
        0
    };

    // XSDT предпочтительнее не из принципа, а потому что RSDT хранит адреса в
    // 32 битах: таблица, лежащая выше 4 ГиБ, в нём просто не выражается.
    let (root, entry_size) = if xsdt != 0 { (xsdt, 8) } else { (rsdt, 4) };
    if root == 0 {
        return Err(AcpiError::NoRootTable);
    }

    // SAFETY: контракт функции.
    let root_len = unsafe { table_length(root) }.ok_or(AcpiError::BadRootTable)?;
    // SAFETY: длина взята из заголовка той же таблицы и проверена на вменяемость.
    let root_bytes = unsafe { phys_slice(root, root_len as usize) };
    if checksum(root_bytes) != 0 {
        return Err(AcpiError::BadRootTable);
    }

    let entries = (root_len as usize - SDT_HEADER_LEN) / entry_size;
    for index in 0..entries.min(MAX_TABLES) {
        let offset = SDT_HEADER_LEN + index * entry_size;
        let table = if entry_size == 8 {
            read_u64(root_bytes, offset)
        } else {
            u64::from(read_u32(root_bytes, offset))
        };
        if table == 0 {
            continue;
        }
        // SAFETY: контракт функции.
        let Some(len) = (unsafe { table_length(table) }) else {
            continue;
        };
        // SAFETY: длина из заголовка проверена.
        let bytes = unsafe { phys_slice(table, len as usize) };
        if &bytes[SDT_SIGNATURE..SDT_SIGNATURE + 4] != b"APIC" {
            continue;
        }
        if checksum(bytes) != 0 {
            return Err(AcpiError::BadMadt);
        }
        return parse_madt(bytes).ok_or(AcpiError::BadMadt);
    }

    Err(AcpiError::NoMadt)
}

/// Длина таблицы из её заголовка, если она вменяема.
///
/// # Safety
///
/// См. [`phys_slice`].
unsafe fn table_length(phys: u64) -> Option<u32> {
    // SAFETY: контракт функции; заголовок фиксированной длины читается первым,
    // и только он позволяет узнать полную длину.
    let header = unsafe { phys_slice(phys, SDT_HEADER_LEN) };
    let length = read_u32(header, SDT_LENGTH);
    if (length as usize) < SDT_HEADER_LEN || length > MAX_TABLE_LEN {
        return None;
    }
    Some(length)
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
