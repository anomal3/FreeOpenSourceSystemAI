//! Разметка носителя: таблица разделов GPT и файловая система FAT32 на запись.
//!
//! Крейт существует ради одного требования: и хостовой сборщик образа
//! (`xtask image`), и установщик, работающий внутри UEFI, обязаны раскладывать
//! диск **одинаково**. Две реализации того же формата разошлись бы неизбежно, и
//! разошлись бы молча — образ, собранный на хосте, грузился бы, а собранный
//! установщиком на живой машине нет, причём отлаживать пришлось бы там, где
//! отладчика нет.
//!
//! Отсюда все остальные решения:
//!
//! * `no_std` + `alloc` — иначе крейт не собрать под `*-unknown-uefi`;
//! * работа через трейт [`BlockDevice`], а не через конкретный носитель, —
//!   на хосте за ним стоит [`MemDisk`] (обычный `Vec<u8>`), в установщике
//!   `EFI_BLOCK_IO_PROTOCOL`;
//! * `&mut dyn BlockDevice` в сигнатурах, а не дженерик, — код разметки
//!   размножать по типам носителей незачем, а в UEFI-приложении каждый
//!   килобайт образа виден.
//!
//! # Что проверяет тест, а что — прошивка
//!
//! Хостовые тесты читают собранный образ **посторонней** реализацией FAT
//! (крейт `fatfs`): свой писатель, проверенный своим же читателем, доказывает
//! только внутреннюю согласованность, а нужна согласованность со
//! спецификацией. Окончательная проверка всё равно за прошивкой — она читает
//! ESP своим драйвером, и `xtask run --image` доводит дело до неё.

#![no_std]

extern crate alloc;

// Тесты живут на хосте и читают собранный образ через `fatfs`, которому нужен
// `std::io`. Явное объявление обязательно: в `no_std`-крейте `std` не
// подключается сам даже там, где он доступен.
#[cfg(test)]
extern crate std;

pub mod crc32;
pub mod fat32;
pub mod gpt;
pub mod guid;
mod mem;

pub use mem::MemDisk;

use core::fmt;

/// Размер сектора, с которым работает крейт.
///
/// Жёстко 512, и это осознанное ограничение, а не упущение. Носители с сектором
/// 4096 («4Kn») существуют, но поддержать их вслепую нельзя: FAT32 с
/// `BytsPerSec = 4096` формально законен, а на практике часть прошивок его не
/// читает, и проверить это в QEMU (где диск всегда 512) невозможно. Код,
/// который нельзя прогнать, хуже отсутствующего — он выглядит рабочим.
/// Поэтому носитель с другим сектором отвергается с внятной ошибкой
/// [`Error::UnsupportedSectorSize`].
pub const SECTOR_SIZE: usize = 512;

/// Отказ операции над носителем.
///
/// Текст сообщений английский — как и весь вывод в лог по всему проекту;
/// перевод для человека делает тот, кто показывает ошибку на экране.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Нижележащий драйвер отказал.
    Io,
    /// Обращение за границы носителя.
    OutOfRange,
    /// Носитель отдаёт сектор не 512 байт.
    UnsupportedSectorSize(u32),
    /// Длина буфера не кратна размеру сектора.
    Unaligned,
    /// Носитель или раздел слишком мал для запрошенной структуры.
    TooSmall,
    /// На томе не осталось свободных кластеров.
    NoSpace,
    /// Имя не представимо в формате 8.3.
    BadName,
    /// Каталог по такому пути уже существует как файл (или наоборот).
    NotADirectory,
    /// Носитель только для чтения.
    ReadOnly,
    /// На носителе нет действующей таблицы разделов GPT.
    NotPartitioned,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io => f.write_str("the block device reported a failure"),
            Error::OutOfRange => f.write_str("access past the end of the device"),
            Error::UnsupportedSectorSize(size) => write!(
                f,
                "{size}-byte sectors are not supported, only 512-byte ones are"
            ),
            Error::Unaligned => f.write_str("buffer length is not a multiple of the sector size"),
            Error::TooSmall => f.write_str("the device or partition is too small"),
            Error::NoSpace => f.write_str("the volume has no free clusters left"),
            Error::BadName => f.write_str("the name cannot be expressed as a FAT 8.3 name"),
            Error::NotADirectory => f.write_str("a path component exists but is not a directory"),
            Error::ReadOnly => f.write_str("the block device is read-only"),
            Error::NotPartitioned => f.write_str("no valid GPT partition table on this device"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Носитель, адресуемый секторами.
///
/// Намеренно узкий: разметке нужны только «прочитать сектора», «записать
/// сектора» и «сбросить кэш». Всё, что сложнее (частичные сектора, буферизация,
/// упреждающее чтение), — забота реализации, если она вообще ей нужна.
pub trait BlockDevice {
    /// Размер сектора носителя. Всё, кроме [`SECTOR_SIZE`], крейт отвергает,
    /// но спросить обязан: ошибиться здесь молча — значит разметить 4Kn-диск
    /// так, будто он 512-байтный, и потерять на нём данные.
    fn sector_size(&self) -> u32;

    /// Число секторов носителя.
    fn sector_count(&self) -> u64;

    /// Только для чтения (например, физически защищённая карта памяти).
    fn is_read_only(&self) -> bool {
        false
    }

    /// Прочитать `buf.len() / SECTOR_SIZE` секторов, начиная с `lba`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<()>;

    /// Записать `buf.len() / SECTOR_SIZE` секторов, начиная с `lba`.
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<()>;

    /// Довести записи до носителя.
    fn flush(&mut self) -> Result<()>;
}

/// Проверяет, что с носителем вообще можно работать.
///
/// Вызывается в начале каждой публичной операции: лучше отказать до первой
/// записи, чем на середине разметки.
pub fn check_device(dev: &dyn BlockDevice) -> Result<()> {
    if dev.sector_size() as usize != SECTOR_SIZE {
        return Err(Error::UnsupportedSectorSize(dev.sector_size()));
    }
    if dev.is_read_only() {
        return Err(Error::ReadOnly);
    }
    if dev.sector_count() == 0 {
        return Err(Error::TooSmall);
    }
    Ok(())
}

/// Записать нулями `count` секторов начиная с `lba`.
///
/// Нужно в двух местах: под каталоги FAT (запись каталога обязана быть нулевой,
/// иначе мусор в первом байте выглядит как имя файла) и при затирании чужой
/// разметки.
pub(crate) fn zero_sectors(dev: &mut dyn BlockDevice, lba: u64, count: u64) -> Result<()> {
    /// Сколько секторов писать за один вызов.
    const CHUNK_SECTORS: usize = 16;
    /// Источник нулей — `static`, а не массив на стеке. У UEFI-приложения стек
    /// порядка сотни килобайт, и локальный буфер такого размера — прямая
    /// дорога к его переполнению, которое проявится не отказом, а порчей
    /// соседнего кадра.
    static ZEROS: [u8; CHUNK_SECTORS * SECTOR_SIZE] = [0; CHUNK_SECTORS * SECTOR_SIZE];

    let mut written = 0u64;
    while written < count {
        let batch = ((count - written) as usize).min(CHUNK_SECTORS);
        dev.write(lba + written, &ZEROS[..batch * SECTOR_SIZE])?;
        written += batch as u64;
    }
    Ok(())
}

/// Записать `u16` в little-endian по смещению.
#[inline]
pub(crate) fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

/// Записать `u32` в little-endian по смещению.
#[inline]
pub(crate) fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// Записать `u64` в little-endian по смещению.
#[inline]
pub(crate) fn put_u64(buf: &mut [u8], at: usize, value: u64) {
    buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
}
