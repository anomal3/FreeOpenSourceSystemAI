//! Файл образа как блочное устройство.
//!
//! # Зачем понадобился
//!
//! Чтобы положить файл **внутрь** уже установленной системы, не загружая её.
//! Обновление системы — контейнер в десятки мегабайт, и путь «через
//! установочный носитель» означал бы, что установка каждый раз переносит его
//! дважды: сначала на ESP, потом на корневой раздел. Это минуты на каждом
//! прогоне стенда и рост образа вдвое — ради файла, который нужен ровно двум
//! сценариям.
//!
//! Здесь вместо этого делается то же, что сделал бы человек с флешкой:
//! открывается готовый образ диска, находится корневой раздел, и файл кладётся
//! в него тем же крейтом `ext2`, которым его записал установщик.
//!
//! # Почему не [`disk::MemDisk`]
//!
//! Потому что образ — два гигабайта. `MemDisk` держит носитель в `Vec<u8>`
//! целиком: он написан под сборку образов, которые собираются с нуля и потому
//! невелики. Здесь носитель уже существует, и читать из него нужно сотую долю
//! процента.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};

/// Открытый образ диска.
pub struct DiskFile {
    file: File,
    sector: u32,
    sectors: u64,
}

impl DiskFile {
    /// Открыть образ на чтение и запись.
    ///
    /// Размер сектора задаётся снаружи, а не угадывается: файл о себе такого не
    /// сообщает, а знает его тот, кто этот образ создавал.
    pub fn open(path: &Path, sector: u32) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("не удалось открыть образ {}", path.display()))?;
        let len = file
            .metadata()
            .with_context(|| format!("не удалось узнать размер {}", path.display()))?
            .len();
        Ok(Self { file, sector, sectors: len / u64::from(sector) })
    }

    fn seek(&mut self, lba: u64) -> disk::Result<()> {
        self.file
            .seek(SeekFrom::Start(lba * u64::from(self.sector)))
            .map(|_| ())
            .map_err(|_| disk::Error::Io)
    }
}

impl disk::BlockDevice for DiskFile {
    fn sector_size(&self) -> u32 {
        self.sector
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if buf.len() % self.sector as usize != 0 {
            return Err(disk::Error::Unaligned);
        }
        if lba + (buf.len() / self.sector as usize) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.seek(lba)?;
        self.file.read_exact(buf).map_err(|_| disk::Error::Io)
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        if buf.len() % self.sector as usize != 0 {
            return Err(disk::Error::Unaligned);
        }
        if lba + (buf.len() / self.sector as usize) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        self.seek(lba)?;
        self.file.write_all(buf).map_err(|_| disk::Error::Io)
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.file.flush().map_err(|_| disk::Error::Io)
    }
}
