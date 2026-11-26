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

/// Put a small file on the EFI system partition of a disk image (25.11:
/// `\FREEOS\CONSOLE` - the harness asks the installed system for its console
/// window at boot, as `qemu::prepare_esp` does on the live ESP). `dir` is
/// created if missing; an existing file is replaced.
pub fn put_esp_file(path: &Path, dir: &str, name: &str, contents: &[u8]) -> Result<()> {
    let fs = open_esp(path)?;
    {
        let root = fs.root_dir();
        let folder = match root.open_dir(dir) {
            Ok(folder) => folder,
            Err(_) => root.create_dir(dir).with_context(|| format!("не удалось создать \\{dir} на ESP"))?,
        };
        let mut file = folder.create_file(name).with_context(|| format!("не удалось создать \\{dir}\\{name} на ESP"))?;
        file.truncate()?;
        file.write_all(contents)?;
        file.flush()?;
    }
    fs.unmount().context("ESP образа не закрылась")?;
    Ok(())
}

/// Remove a file from the ESP of a disk image; a missing file is fine.
pub fn remove_esp_file(path: &Path, dir: &str, name: &str) -> Result<()> {
    let fs = open_esp(path)?;
    if let Ok(folder) = fs.root_dir().open_dir(dir) {
        let _ = folder.remove(name);
    }
    fs.unmount().context("ESP образа не закрылась")?;
    Ok(())
}

/// The FAT of the image's EFI system partition.
fn open_esp(path: &Path) -> Result<fatfs::FileSystem<Partition>> {
    let (first, last) = {
        let mut dev = DiskFile::open(path, 512)?;
        let table = disk::gpt::read(&mut dev).map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", path.display()))?;
        let esp = table.find(disk::gpt::ESP_TYPE).ok_or_else(|| anyhow::anyhow!("на образе {} нет ESP", path.display()))?;
        (esp.first_lba, esp.last_lba)
    };
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("не удалось открыть образ {}", path.display()))?;
    let part = Partition { file, start: first * 512, len: (last + 1 - first) * 512, pos: 0 };
    fatfs::FileSystem::new(part, fatfs::FsOptions::new()).context("ESP образа не читается как FAT")
}

/// A partition of an image file as a seekable stream (for `fatfs`).
struct Partition {
    file: File,
    start: u64,
    len: u64,
    pos: u64,
}

impl Read for Partition {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let got = self.file.read(&mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl Write for Partition {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = (buf.len() as u64).min(self.len.saturating_sub(self.pos)) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let put = self.file.write(&buf[..n])?;
        self.pos += put as u64;
        Ok(put)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Partition {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let next = match to {
            SeekFrom::Start(at) => at as i64,
            SeekFrom::End(delta) => self.len as i64 + delta,
            SeekFrom::Current(delta) => self.pos as i64 + delta,
        };
        if next < 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before the partition"));
        }
        self.pos = next as u64;
        Ok(self.pos)
    }
}
