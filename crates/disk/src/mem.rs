//! Носитель в оперативной памяти.
//!
//! Существует ради двух совершенно разных задач, у которых оказалась одна
//! реализация. Первая — тесты: разметку надо прогонять там, где есть `cargo
//! test`, а не только на живом диске под прошивкой. Вторая — сборка образа на
//! хосте: `xtask image` формирует весь образ в памяти и сбрасывает его одним
//! куском, потому что разметка — это множество мелких перемещений по носителю,
//! а буферизованного read-write-seek потока в `std` нет (та же причина, что и у
//! сборщика initrd).

use alloc::vec::Vec;

use crate::{BlockDevice, Error, Result, SECTOR_SIZE};

/// Образ носителя целиком в памяти.
pub struct MemDisk {
    data: Vec<u8>,
}

impl MemDisk {
    /// Создать нулевой образ на `sectors` секторов.
    ///
    /// Возвращает `None`, если памяти не хватило: образ — это десятки
    /// мегабайт, и отказ здесь реален. Паниковать из-за нехватки памяти в
    /// коде, который заодно работает внутри установщика, нельзя.
    #[must_use]
    pub fn new(sectors: u64) -> Option<Self> {
        let len = usize::try_from(sectors.checked_mul(SECTOR_SIZE as u64)?).ok()?;
        let mut data = Vec::new();
        data.try_reserve_exact(len).ok()?;
        data.resize(len, 0);
        Some(Self { data })
    }

    /// Обернуть готовый образ. Длина обязана быть кратна сектору.
    #[must_use]
    pub fn from_vec(data: Vec<u8>) -> Option<Self> {
        if data.len() % SECTOR_SIZE != 0 || data.is_empty() {
            return None;
        }
        Some(Self { data })
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
        if len % SECTOR_SIZE != 0 || len == 0 {
            return Err(Error::Unaligned);
        }
        let start = usize::try_from(lba.checked_mul(SECTOR_SIZE as u64).ok_or(Error::OutOfRange)?)
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
        SECTOR_SIZE as u32
    }

    fn sector_count(&self) -> u64 {
        (self.data.len() / SECTOR_SIZE) as u64
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
    let len = usize::try_from(sectors.checked_mul(SECTOR_SIZE as u64)?).ok()?;
    let mut data = Vec::new();
    data.try_reserve_exact(len).ok()?;
    data.resize(len, 0);
    for (index, byte) in data.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(37).wrapping_add(11);
    }
    MemDisk::from_vec(data)
}
