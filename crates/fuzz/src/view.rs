//! Блочное устройство поверх среза: носитель, который ничего не копирует.
//!
//! # Зачем, когда есть `MemDisk`
//!
//! `MemDisk` владеет своими байтами, то есть требует `Vec`. Для фаззера это
//! значит копию образа на каждой итерации, а наименьший том btrfs — сто
//! двадцать восемь мегабайт: на копирование уходило всё время, и цель шла
//! двадцать четыре входа в секунду против тысяч у прочих. Итерация, которая
//! стоит копии образа, — это не проверка разбора, а проверка `memcpy`.
//!
//! Здесь носитель **смотрит** на чужие байты. Порча делается по месту, в одном
//! и том же буфере, и за итерацию не выделяется ничего.
//!
//! # Почему только на чтение
//!
//! Потому что цели фаззера ничего не пишут: монтирование, чтение файла,
//! проверка тома с `Fix::Nothing`. Запись — это уже другая проверка, и если она
//! понадобится, ей нужен изменяемый срез и отдельный тип, а не тихо
//! разрешённая запись здесь. Пока же попытка записи — честный отказ
//! «носитель только для чтения», ровно тот же, который вернула бы защищённая
//! карта памяти.

use disk::{BlockDevice, Error, Result};

/// Носитель, читающий чужой срез.
pub struct View<'a> {
    bytes: &'a [u8],
    sector: usize,
}

impl<'a> View<'a> {
    /// Носитель с секторами по 512 байт.
    #[must_use]
    pub const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, sector: 512 }
    }

    fn range(&self, lba: u64, len: usize) -> Result<core::ops::Range<usize>> {
        if len % self.sector != 0 {
            return Err(Error::Unaligned);
        }
        // Всё с проверкой переполнения: номер сектора приходит из разбора
        // испорченного тома, то есть это в точности то число, которое обязано
        // быть каким угодно.
        let start = usize::try_from(lba.checked_mul(self.sector as u64).ok_or(Error::OutOfRange)?)
            .map_err(|_| Error::OutOfRange)?;
        let end = start.checked_add(len).ok_or(Error::OutOfRange)?;
        if end > self.bytes.len() {
            return Err(Error::OutOfRange);
        }
        Ok(start..end)
    }
}

impl BlockDevice for View<'_> {
    fn sector_size(&self) -> u32 {
        self.sector as u32
    }

    fn sector_count(&self) -> u64 {
        (self.bytes.len() / self.sector) as u64
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let range = self.range(lba, buf.len())?;
        buf.copy_from_slice(&self.bytes[range]);
        Ok(())
    }

    fn write(&mut self, _lba: u64, _buf: &[u8]) -> Result<()> {
        Err(Error::ReadOnly)
    }

    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}
