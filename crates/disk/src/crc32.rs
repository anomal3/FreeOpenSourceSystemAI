//! CRC-32 (IEEE 802.3) — та самая контрольная сумма, которой GPT защищает
//! заголовок и таблицу разделов.
//!
//! Своя реализация вместо зависимости: это тридцать строк и один
//! общеизвестный полином. Внешний крейт пришлось бы тянуть в два разных
//! окружения (хост и UEFI) ради функции, которую видно целиком.

/// Отражённый полином IEEE 802.3 (0x04C11DB7 в прямом порядке бит).
const POLY: u32 = 0xEDB8_8320;

/// Таблица на байт, посчитанная на этапе компиляции.
static TABLE: [u32; 256] = build_table();

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0usize;
    while index < 256 {
        let mut crc = index as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ POLY } else { crc >> 1 };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

/// Накопитель CRC.
///
/// Заведён потому, что таблица разделов GPT — это 16 КиБ, которые считаются
/// одной суммой, а формируются по одной записи: собирать их в один буфер ради
/// вызова [`crc32`] было бы лишним копированием.
#[derive(Clone, Copy)]
pub struct Crc32 {
    state: u32,
}

impl Crc32 {
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0xFFFF_FFFF }
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut state = self.state;
        for &byte in data {
            state = TABLE[((state ^ u32::from(byte)) & 0xFF) as usize] ^ (state >> 8);
        }
        self.state = state;
    }

    #[must_use]
    pub const fn finish(self) -> u32 {
        self.state ^ 0xFFFF_FFFF
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC-32 одного среза.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = Crc32::new();
    crc.update(data);
    crc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Контрольные векторы из спецификации/классических реализаций zlib.
    ///
    /// Нужны именно чужие значения: собственная реализация, сверенная сама с
    /// собой, доказывает лишь детерминированность.
    #[test]
    fn known_vectors() {
        assert_eq!(crc32(b""), 0x0000_0000);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b"The quick brown fox jumps over the lazy dog"), 0x414F_A339);
    }

    /// Разбиение на части не должно менять результат — на этом стоит подсчёт
    /// суммы таблицы разделов по одной записи.
    #[test]
    fn streaming_matches_single_shot() {
        let data: [u8; 300] = core::array::from_fn(|i| (i * 7 + 3) as u8);
        let mut crc = Crc32::new();
        crc.update(&data[..17]);
        crc.update(&data[17..200]);
        crc.update(&data[200..]);
        assert_eq!(crc.finish(), crc32(&data));
    }
}
