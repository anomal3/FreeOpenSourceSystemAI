//! Носитель в оперативной памяти.
//!
//! Существует ради двух совершенно разных задач, у которых оказалась одна
//! реализация. Первая — тесты: разметку надо прогонять там, где есть `cargo
//! test`, а не только на живом диске под прошивкой. Вторая — сборка образа на
//! хосте: `xtask image` формирует весь образ в памяти и сбрасывает его одним
//! куском, потому что разметка — это множество мелких перемещений по носителю,
//! а буферизованного read-write-seek потока в `std` нет (та же причина, что и у
//! сборщика initrd).
//!
//! # Размер сектора здесь — параметр
//!
//! С Phase 26c образ умеет притворяться диском с любым допустимым сектором, и
//! это не украшение: 4Kn-диск иначе проверялся бы только под эмулятором, где
//! отладка стоит на два порядка дороже, чем в `cargo test`. Смысл всей фазы в
//! том, что заголовок GPT на таком диске лежит в другом месте, — а «в другом
//! месте» проверяется сравнением байт, для которого эмулятор не нужен.

use alloc::vec::Vec;

use crate::{BlockDevice, DEFAULT_SECTOR_SIZE, Error, Result, sector_size_supported};

/// Образ носителя целиком в памяти.
pub struct MemDisk {
    data: Vec<u8>,
    sector: usize,
}

impl MemDisk {
    /// Создать нулевой образ на `sectors` секторов по 512 байт.
    ///
    /// Возвращает `None`, если памяти не хватило: образ — это десятки
    /// мегабайт, и отказ здесь реален. Паниковать из-за нехватки памяти в
    /// коде, который заодно работает внутри установщика, нельзя.
    #[must_use]
    pub fn new(sectors: u64) -> Option<Self> {
        Self::with_sector_size(sectors, DEFAULT_SECTOR_SIZE)
    }

    /// Создать нулевой образ с заданным размером сектора.
    #[must_use]
    pub fn with_sector_size(sectors: u64, sector: usize) -> Option<Self> {
        if !sector_size_supported(u32::try_from(sector).ok()?) {
            return None;
        }
        let len = usize::try_from(sectors.checked_mul(sector as u64)?).ok()?;
        let mut data = Vec::new();
        data.try_reserve_exact(len).ok()?;
        data.resize(len, 0);
        Some(Self { data, sector })
    }

    /// Обернуть готовый образ. Длина обязана быть кратна сектору.
    #[must_use]
    pub fn from_vec(data: Vec<u8>) -> Option<Self> {
        Self::from_vec_with_sector_size(data, DEFAULT_SECTOR_SIZE)
    }

    /// Обернуть готовый образ, объявив размер сектора.
    #[must_use]
    pub fn from_vec_with_sector_size(data: Vec<u8>, sector: usize) -> Option<Self> {
        if !sector_size_supported(u32::try_from(sector).ok()?) {
            return None;
        }
        if data.len() % sector != 0 || data.is_empty() {
            return None;
        }
        Some(Self { data, sector })
    }

    /// Отдать образ наружу — чтобы записать его в файл.
    #[must_use]
    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Диапазон байт под сектора `[lba, lba + count)`, если он существует.
    fn range(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>> {
        if len % self.sector != 0 || len == 0 {
            return Err(Error::Unaligned);
        }
        let start = usize::try_from(lba.checked_mul(self.sector as u64).ok_or(Error::OutOfRange)?)
            .map_err(|_| Error::OutOfRange)?;
        let end = start.checked_add(len).ok_or(Error::OutOfRange)?;
        if end > self.data.len() {
            return Err(Error::OutOfRange);
        }
        Ok(start..end)
    }
}

impl BlockDevice for MemDisk {
    fn sector_size(&self) -> u32 {
        self.sector as u32
    }

    fn sector_count(&self) -> u64 {
        (self.data.len() / self.sector) as u64
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let range = self.range(lba, buf.len())?;
        buf.copy_from_slice(&self.data[range]);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<()> {
        let range = self.range(lba, buf.len())?;
        self.data[range].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

/// Образ, заполненный ненулевым мусором.
///
/// Нужен тестам, которые проверяют, что разметка действительно затирает чужие
/// структуры: на нулевом образе такая проверка проходит сама собой и ничего не
/// доказывает.
#[cfg(test)]
#[must_use]
pub(crate) fn junk_disk(sectors: u64) -> Option<MemDisk> {
    junk_disk_with_sector_size(sectors, DEFAULT_SECTOR_SIZE)
}

#[cfg(test)]
#[must_use]
pub(crate) fn junk_disk_with_sector_size(sectors: u64, sector: usize) -> Option<MemDisk> {
    let len = usize::try_from(sectors.checked_mul(sector as u64)?).ok()?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).ok()?;
    data.resize(len, 0);
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
    }
    MemDisk::from_vec_with_sector_size(data, sector)
}
