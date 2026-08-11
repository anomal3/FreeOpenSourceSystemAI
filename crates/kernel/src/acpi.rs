//! Поиск таблиц ACPI по сигнатуре.
//!
//! # Почему это не в arch-слое
//!
//! ACPI — не свойство x86. UEFI на AArch64 предоставляет те же таблицы (у
//! QEMU virt и у прошивки Raspberry Pi 4 они есть), и ядру они нужны на обеих
//! архитектурах: `MCFG` описывает конфигурационное пространство PCI, без
//! которого не найти ни один контроллер на шине. Арх-специфичным остаётся только
//! содержимое отдельных таблиц — например `MADT`, откуда x86-64 берёт I/O APIC
//! (см. [`crate::arch`]).
//!
//! # Насколько это доверенные данные
//!
//! Не доверенные вовсе. Адрес RSDP приезжает от загрузчика, дальше идёт цепочка
//! указателей внутрь памяти, размеченной прошивкой. Поэтому проверяется всё:
//! сигнатуры, контрольные суммы (сумма всех байт таблицы обязана быть нулём по
//! модулю 256), длины на вменяемость. Любая нестыковка означает «таблицы нет», а
//! не «читаем дальше и надеемся».
//!
//! # Как читается физическая память
//!
//! Через прямое отображение ([`PhysAddr::to_direct_map`]). Identity-отображение
//! тоже подошло бы сегодня, но оно исчезнет при переезде ядра в верхнюю
//! половину, а прямое — нет.

use crate::mm::PhysAddr;

/// Заголовок любой системной таблицы ACPI (ACPI 6.5, 5.2.6).
pub const SDT_HEADER_LEN: usize = 36;

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
/// Настоящие DSDT доходят до сотен килобайт, но ядру нужны только небольшие
/// таблицы описания железа. Мегабайт — заведомо достаточный предел, за которым
/// начинается не «очень подробная таблица», а испорченное поле длины.
const MAX_TABLE_LEN: u32 = 1024 * 1024;

/// Сколько таблиц ядро согласно перебрать в корневой таблице.
const MAX_TABLES: usize = 64;

/// Почему таблицу не удалось прочитать.
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
    /// Таблицы с такой сигнатурой в списке нет.
    NotFound([u8; 4]),
    /// Таблица нашлась, но не прошла проверку контрольной суммы.
    BadChecksum([u8; 4]),
}

impl core::fmt::Display for AcpiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoRsdp => f.write_str("the bootloader passed no ACPI RSDP"),
            Self::BadRsdpSignature => f.write_str("no RSDP signature at the address given"),
            Self::BadRsdpChecksum => f.write_str("RSDP checksum mismatch"),
            Self::NoRootTable => f.write_str("RSDP names neither an RSDT nor an XSDT"),
            Self::BadRootTable => f.write_str("the root table failed validation"),
            Self::NotFound(sig) => {
                write!(f, "no '{}' table among the ACPI tables", Signature(*sig))
            }
            Self::BadChecksum(sig) => {
                write!(f, "the '{}' table failed its checksum", Signature(*sig))
            }
        }
    }
}

/// Сигнатура таблицы в печатаемом виде.
struct Signature([u8; 4]);

impl core::fmt::Display for Signature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for byte in self.0 {
            // Сигнатура приходит из памяти прошивки: непечатаемый байт в ней
            // возможен, и выводить его в консоль как есть незачем.
            let ch = if byte.is_ascii_graphic() { byte as char } else { '?' };
            f.write_str(ch.encode_utf8(&mut [0u8; 4]))?;
        }
        Ok(())
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
pub unsafe fn phys_slice(phys: u64, len: usize) -> &'static [u8] {
    let virt = PhysAddr::new(phys).to_direct_map();
    // SAFETY: условия делегированы вызывающему контрактом функции. Время жизни
    // `'static` корректно: физическая память под таблицами не освобождается.
    unsafe { core::slice::from_raw_parts(virt.as_usize() as *const u8, len) }
}

/// Сумма байт по модулю 256. У корректной таблицы ACPI она равна нулю.
fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte))
}

#[must_use]
pub fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[must_use]
pub fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

#[must_use]
pub fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}

/// Найти таблицу по сигнатуре и вернуть её целиком, с уже проверенной суммой.
///
/// `rsdp` — физический адрес RSDP из [`boot_info::BootInfo::acpi_rsdp`].
///
/// # Safety
///
/// Требования те же, что у [`phys_slice`]: прямое отображение активно, а память
/// с таблицами ACPI ещё не переиспользована.
pub unsafe fn find_table(rsdp: u64, signature: &[u8; 4]) -> Result<&'static [u8], AcpiError> {
    // SAFETY: контракт функции.
    let (root, entry_size) = unsafe { root_table(rsdp) }?;

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
        if &bytes[SDT_SIGNATURE..SDT_SIGNATURE + 4] != signature {
            continue;
        }
        if checksum(bytes) != 0 {
            return Err(AcpiError::BadChecksum(*signature));
        }
        return Ok(bytes);
    }

    Err(AcpiError::NotFound(*signature))
}

/// Физический адрес корневой таблицы и размер её записи (4 у RSDT, 8 у XSDT).
///
/// # Safety
///
/// См. [`phys_slice`].
unsafe fn root_table(rsdp: u64) -> Result<(u64, usize), AcpiError> {
    if rsdp == 0 {
        return Err(AcpiError::NoRsdp);
    }

    // SAFETY: контракт функции. Читаем сначала только 20 байт — версии 1.0
    // ровно столько и существует, и трогать 24-й байт до проверки ревизии
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
    if xsdt != 0 {
        return Ok((xsdt, 8));
    }
    if rsdt != 0 {
        return Ok((rsdt, 4));
    }
    Err(AcpiError::NoRootTable)
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
