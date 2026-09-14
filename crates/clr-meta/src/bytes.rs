//! Числа little-endian с проверкой границ и сжатые целые ECMA-335.
//!
//! Каждое чтение возвращает `Option`: файл пришёл снаружи, и смещение внутри
//! него — такие же чужие данные, как всё остальное. Сложение смещения с длиной
//! тоже проверяется: `usize` на 32-битной машине переполняется от смещения,
//! записанного в файле, а не от нашей ошибки.

pub(crate) fn u8_at(data: &[u8], at: usize) -> Option<u8> {
    data.get(at).copied()
}

pub(crate) fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub(crate) fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub(crate) fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    let bytes = data.get(at..at.checked_add(8)?)?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(bytes);
    Some(u64::from_le_bytes(raw))
}

pub(crate) fn slice(data: &[u8], at: usize, len: usize) -> Option<&[u8]> {
    data.get(at..at.checked_add(len)?)
}

/// Сжатое беззнаковое целое (ECMA-335 II.23.2): значение и сколько байт оно
/// заняло.
///
/// Один байт — до `0x7F`, два (`10xxxxxx`) — до `0x3FFF`, четыре (`110xxxxx`) —
/// до `0x1FFFFFFF`. Порядок байтов в нём **старший первым**, в отличие от всего
/// остального в файле, и это ловушка номер один при разборе сигнатур.
#[must_use]
pub fn compressed_u32(data: &[u8]) -> Option<(u32, usize)> {
    let first = *data.first()?;
    if first & 0x80 == 0 {
        Some((u32::from(first), 1))
    } else if first & 0xC0 == 0x80 {
        let second = *data.get(1)?;
        Some(((u32::from(first & 0x3F) << 8) | u32::from(second), 2))
    } else if first & 0xE0 == 0xC0 {
        let rest = data.get(1..4)?;
        let value = (u32::from(first & 0x1F) << 24)
            | (u32::from(rest[0]) << 16)
            | (u32::from(rest[1]) << 8)
            | u32::from(rest[2]);
        Some((value, 4))
    } else {
        None
    }
}

/// Сжатое знаковое целое (ECMA-335 II.23.2).
///
/// Знак хранится в младшем бите, а значение сдвинуто на один влево — поэтому
/// отрицательное число восстанавливается дополнением старших битов до ширины
/// своей формы: 6, 13 или 28 значащих бит.
#[must_use]
pub fn compressed_i32(data: &[u8]) -> Option<(i32, usize)> {
    let (raw, used) = compressed_u32(data)?;
    let negative = raw & 1 != 0;
    let magnitude = raw >> 1;
    let value = if !negative {
        magnitude as i32
    } else {
        let fill: u32 = match used {
            1 => 0xFFFF_FFC0,
            2 => 0xFFFF_E000,
            _ => 0xF000_0000,
        };
        (magnitude | fill) as i32
    };
    Some((value, used))
}
