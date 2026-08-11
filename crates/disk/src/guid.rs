//! GUID в том виде, в каком его хранит GPT.
//!
//! Своя структура, а не `uefi::Guid`, по той же причине, по которой крейт
//! вообще существует: он собирается и под хост, где никакого `uefi` нет.

use core::fmt;

/// GUID (он же UUID) — 128 бит с исторически смешанным порядком байт.
///
/// Первые три поля на диске лежат в little-endian, последние два — как есть.
/// Это главная ловушка формата: GUID, записанный шестнадцатью байтами подряд в
/// том порядке, в каком его печатают, даёт раздел, который прошивка не узнает
/// (и, что хуже, узнает как чужой). Поэтому «текстовый» и «дисковый» виды
/// разведены по разным функциям, а не оставлены на усмотрение вызывающего.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid {
    time_low: u32,
    time_mid: u16,
    time_high_and_version: u16,
    /// Два старших байта — clock_seq, остальные шесть — node. На диске и в
    /// печати порядок один и тот же, поэтому держим их одним массивом.
    tail: [u8; 8],
}

impl Guid {
    /// Собрать GUID из полей в том порядке, в каком его записывают текстом:
    /// `{time_low}-{time_mid}-{time_high}-{tail[0..2]}-{tail[2..8]}`.
    #[must_use]
    pub const fn new(
        time_low: u32,
        time_mid: u16,
        time_high_and_version: u16,
        tail: [u8; 8],
    ) -> Self {
        Self {
            time_low,
            time_mid,
            time_high_and_version,
            tail,
        }
    }

    /// Нулевой GUID — «раздела нет». Ровно этим значением GPT помечает пустую
    /// запись таблицы.
    pub const ZERO: Self = Self::new(0, 0, 0, [0; 8]);

    /// Представление на диске: 16 байт со смешанным порядком.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.time_low.to_le_bytes());
        out[4..6].copy_from_slice(&self.time_mid.to_le_bytes());
        out[6..8].copy_from_slice(&self.time_high_and_version.to_le_bytes());
        out[8..16].copy_from_slice(&self.tail);
        out
    }

    /// Разбор представления на диске.
    #[must_use]
    pub fn from_bytes(raw: [u8; 16]) -> Self {
        Self {
            time_low: u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]),
            time_mid: u16::from_le_bytes([raw[4], raw[5]]),
            time_high_and_version: u16::from_le_bytes([raw[6], raw[7]]),
            tail: [
                raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
            ],
        }
    }

    /// Сделать из шестнадцати произвольных байт GUID версии 4.
    ///
    /// Уникальные идентификаторы диска и разделов обязаны различаться от
    /// установки к установке, но криптостойкий источник случайности здесь ни к
    /// чему: GUID нужен, чтобы отличать разделы, а не чтобы противостоять
    /// подбору. Откуда взять энтропию, решает вызывающий (время прошивки и
    /// счётчик тактов в установщике, хеш содержимого в сборщике образа —
    /// последнее заодно делает образ побайтово воспроизводимым).
    ///
    /// Биты версии и варианта проставляются по RFC 4122, чтобы посторонние
    /// инструменты видели корректный UUIDv4, а не мусор в этих полях.
    #[must_use]
    pub fn from_entropy(mut raw: [u8; 16]) -> Self {
        // Версия 4 — в старшем полубайте седьмого байта.
        raw[6] = (raw[6] & 0x0F) | 0x40;
        // Вариант RFC 4122 — в двух старших битах девятого.
        raw[8] = (raw[8] & 0x3F) | 0x80;
        // Порядок полей текстовый, а не дисковый: на входе просто байты, и
        // «перевернуть» их обратно всё равно нечему.
        Self {
            time_low: u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]),
            time_mid: u16::from_be_bytes([raw[4], raw[5]]),
            time_high_and_version: u16::from_be_bytes([raw[6], raw[7]]),
            tail: [
                raw[8], raw[9], raw[10], raw[11], raw[12], raw[13], raw[14], raw[15],
            ],
        }
    }

    #[must_use]
    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}

impl fmt::Display for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:08X}-{:04X}-{:04X}-{:02X}{:02X}-",
            self.time_low, self.time_mid, self.time_high_and_version, self.tail[0], self.tail[1]
        )?;
        for byte in &self.tail[2..] {
            write!(f, "{byte:02X}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Guid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тип раздела EFI System Partition из спецификации UEFI: раскладка его
    /// байт на диске известна и опубликована, поэтому он и служит эталоном
    /// смешанного порядка.
    #[test]
    fn esp_guid_matches_the_bytes_on_disk() {
        let guid = Guid::new(
            0xC12A_7328,
            0xF81F,
            0x11D2,
            [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
        );
        assert_eq!(
            guid.to_bytes(),
            [
                0x28, 0x73, 0x2A, 0xC1, // time_low, little-endian
                0x1F, 0xF8, // time_mid, little-endian
                0xD2, 0x11, // time_high, little-endian
                0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B, // как есть
            ]
        );
        assert_eq!(
            alloc::format!("{guid}"),
            "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
        );
    }

    #[test]
    fn bytes_round_trip() {
        let guid = Guid::new(1, 2, 3, [4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(Guid::from_bytes(guid.to_bytes()), guid);
    }

    #[test]
    fn entropy_stamps_version_and_variant() {
        let guid = Guid::from_entropy([0xFF; 16]);
        let bytes = guid.to_bytes();
        // Версия 4 живёт в старшем полубайте time_high, а на диске это байт 7.
        assert_eq!(bytes[7] & 0xF0, 0x40);
        // Вариант RFC 4122 — два старших бита первого байта хвоста.
        assert_eq!(bytes[8] & 0xC0, 0x80);
    }
}
