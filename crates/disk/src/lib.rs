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
pub mod iso9660;
pub mod guid;
mod mem;

pub use mem::MemDisk;

use core::fmt;

/// Размер сектора, который был единственным до Phase 26c.
///
/// Остаётся как значение по умолчанию там, где носитель придумываем мы сами:
/// образ в памяти для тестов, расчёты «сколько это в секторах» для случая, где
/// устройства ещё нет. Кодом, работающим с настоящим носителем, **не
/// используется**: там размер спрашивается у него самого.
pub const DEFAULT_SECTOR_SIZE: usize = 512;

/// Наибольший размер сектора, который крейт готов обслуживать.
///
/// Существует не как предел возможностей, а как размер буфера: заголовок GPT,
/// загрузочная запись и запись каталога FAT читаются в массив на стеке, и в
/// UEFI-приложении, где стека около сотни килобайт, этот массив обязан иметь
/// известный размер. Четыре килобайта покрывают всё, что бывает у дисков
/// сегодня: 512, 520 (отвергается — не степень двойки), 4096.
pub const MAX_SECTOR_SIZE: usize = 4096;

/// Годится ли такой размер сектора.
///
/// Требуется степень двойки от 512 до [`MAX_SECTOR_SIZE`]. Не «любое число»:
/// вся арифметика разметки — деления и выравнивания, и на размере, не кратном
/// степени двойки, она перестаёт быть точной там, где этого никто не заметит.
#[must_use]
pub const fn sector_size_supported(size: u32) -> bool {
    size >= 512 && size as usize <= MAX_SECTOR_SIZE && size.is_power_of_two()
}

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
    /// Размер сектора носителя не из тех, что крейт умеет.
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
    /// Том не похож на FAT32: подпись, поля BPB или число кластеров не те.
    NotFat32,
    /// Такого файла или каталога на томе нет.
    NotFound,
    /// Структуры тома противоречат сами себе: цепочка кластеров уводит за
    /// пределы тома либо в служебные записи.
    Corrupt,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io => f.write_str("the block device reported a failure"),
            Error::OutOfRange => f.write_str("access past the end of the device"),
            Error::UnsupportedSectorSize(size) => write!(
                f,
                "{size}-byte sectors are not supported: a power of two from 512 to {MAX_SECTOR_SIZE} is required"
            ),
            Error::Unaligned => f.write_str("buffer length is not a multiple of the sector size"),
            Error::TooSmall => f.write_str("the device or partition is too small"),
            Error::NoSpace => f.write_str("the volume has no free clusters left"),
            Error::BadName => f.write_str("the name cannot be expressed as a FAT 8.3 name"),
            Error::NotADirectory => f.write_str("a path component exists but is not a directory"),
            Error::ReadOnly => f.write_str("the block device is read-only"),
            Error::NotPartitioned => f.write_str("no valid GPT partition table on this device"),
            Error::NotFat32 => f.write_str("this volume is not FAT32"),
            Error::NotFound => f.write_str("no such file or directory on the volume"),
            Error::Corrupt => f.write_str("the volume structures contradict each other"),
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
    /// Размер сектора носителя, в байтах.
    ///
    /// Спрашивается у устройства и используется во всей арифметике разметки —
    /// с Phase 26c по-настоящему, а не для проверки. Ошибиться здесь значит
    /// разметить 4Kn-диск так, будто он 512-байтный: заголовок GPT окажется не
    /// там, где его будет искать чужая система, а FAT — не там, где его будет
    /// искать прошивка.
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
    if !sector_size_supported(dev.sector_size()) {
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
pub fn zero_sectors(dev: &mut dyn BlockDevice, lba: u64, count: u64) -> Result<()> {
    /// Сколько байт писать за один вызов.
    ///
    /// Считается в байтах, а не в секторах: при секторе 4096 шестнадцать
    /// секторов — это уже 64 КиБ, и заводить статический буфер такого размера
    /// ради нулей незачем. Восьми килобайт хватает и на шестнадцать секторов по
    /// 512, и на два по 4096.
    const CHUNK_BYTES: usize = 8 * 1024;
    /// Источник нулей — `static`, а не массив на стеке. У UEFI-приложения стек
    /// порядка сотни килобайт, и локальный буфер такого размера — прямая
    /// дорога к его переполнению, которое проявится не отказом, а порчей
    /// соседнего кадра.
    static ZEROS: [u8; CHUNK_BYTES] = [0; CHUNK_BYTES];

    let sector = dev.sector_size() as usize;
    if sector == 0 || sector > CHUNK_BYTES {
        return Err(Error::UnsupportedSectorSize(dev.sector_size()));
    }
    let chunk_sectors = CHUNK_BYTES / sector;

    let mut written = 0u64;
    while written < count {
        let batch = ((count - written) as usize).min(chunk_sectors);
        dev.write(lba + written, &ZEROS[..batch * sector])?;
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
