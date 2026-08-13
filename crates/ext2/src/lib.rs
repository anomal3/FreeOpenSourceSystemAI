//! Файловая система ext2: раскладка на диске, форматирование, запись и чтение.
//!
//! # Почему ext2, а не собственный формат
//!
//! Изначально планировалась своя inode-based ФС. Решающим оказался не сам
//! формат, а то, **чем его проверять**. Весь проект стоит на правиле «проверять,
//! а не утверждать»: том FAT32, который пишет крейт `disk`, читается обратно
//! посторонней реализацией, потому что свой писатель, проверенный своим же
//! читателем, доказывает лишь внутреннюю согласованность, а нужна
//! согласованность со спецификацией.
//!
//! У ext2 такая возможность есть, а у собственного формата не было бы никогда:
//! в тестах образ читает крейт `ext4-view`, а на любой машине с Linux работают
//! `e2fsck` и обычный `mount`. Побочная выгода того же свойства: сломанную
//! установку можно примонтировать и починить снаружи.
//!
//! Исходная причина, по которой FAT32 не годится под корень, при этом
//! удовлетворена: `uid`, `gid` и `mode` лежат в inode на диске.
//!
//! Цена названа прямо: ext2 — формат 1993 года, без контрольных сумм и
//! снапшотов, и после потери питания ему нужен fsck. Журналирование ext3 —
//! надстройка над тем же форматом, то есть добавляется позже без миграции
//! данных.
//!
//! # Что здесь есть и чего нет
//!
//! Есть: форматирование, создание каталогов и файлов, чтение всего этого
//! обратно. Нет: удаления, усечения, жёстких ссылок, символических ссылок,
//! расширенных атрибутов и тройной косвенности. Всё это отсутствует не по
//! недосмотру — у каждого пункта нет сегодняшнего потребителя, а
//! непроверяемый код в разметке диска опаснее отсутствующего.
//!
//! # Единицы измерения, в которых легко ошибиться
//!
//! В ext2 их три, и они не совпадают:
//!
//! * **сектор** — 512 байт, этим оперирует [`disk::BlockDevice`];
//! * **блок** — 1024, 2048 или 4096 байт, этим оперирует сама ФС;
//! * **`i_blocks` в inode** — считается в **512-байтных** секторах, а не в
//!   блоках ФС, несмотря на название. Ошибка здесь не мешает читать файл, но
//!   `e2fsck` о ней сообщит.

#![no_std]

extern crate alloc;

// Тесты живут на хосте и сверяют образ с чужой реализацией, которой нужен
// `std::io`. Объявление обязательно: в `no_std`-крейте `std` не подключается
// сам даже там, где он доступен.
#[cfg(test)]
extern crate std;

mod check;
mod edit;
mod layout;
mod read;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_fsck;
mod write;

pub use check::{Fix, Problem, Report, check};
pub use layout::{BlockSize, Geometry, ROOT_INODE};
pub use read::{DirEntry, Ext2, FileType, Inode};
pub use edit::Editor;
pub use write::{FormatOptions, format, format_with};

use core::fmt;

/// Отказ операции над файловой системой.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Носитель отказал.
    Io,
    /// Раздел слишком мал под файловую систему.
    TooSmall,
    /// На диске не то, что ожидалось: подпись, версия или счётчики не сходятся.
    Corrupt,
    /// Закончились блоки.
    NoSpace,
    /// Закончились inode. Отдельно от [`Error::NoSpace`] намеренно: причина
    /// другая, и лечится она другим (изменением плотности inode), поэтому
    /// сообщения обязаны различаться.
    NoInodes,
    /// Пути нет.
    NotFound,
    /// Компонент пути оказался не каталогом.
    NotADirectory,
    /// Имя пустое, слишком длинное или содержит `/`.
    BadName,
    /// Такое имя в каталоге уже есть.
    Exists,
    /// Каталог не пуст, а удалять содержимое за вызывающего этот крейт не
    /// станет: обход дерева с освобождением — то место, где ошибка стирает не
    /// то, что просили.
    NotEmpty,
    /// Операция для файла, а имя оказалось каталогом.
    IsADirectory,
    /// Файл использует возможность, которой здесь нет (тройная косвенность,
    /// расширения ext3/ext4).
    Unsupported,
    /// Не хватило памяти.
    NoMemory,
}

impl From<disk::Error> for Error {
    fn from(err: disk::Error) -> Self {
        match err {
            disk::Error::TooSmall => Error::TooSmall,
            _ => Error::Io,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Error::Io => "the block device reported a failure",
            Error::TooSmall => "the partition is too small for an ext2 filesystem",
            Error::Corrupt => "on-disk structure is inconsistent",
            Error::NoSpace => "the filesystem has no free blocks left",
            Error::NoInodes => "the filesystem has no free inodes left",
            Error::NotFound => "no such file or directory",
            Error::NotADirectory => "a path component is not a directory",
            Error::BadName => "the name is empty, too long or contains a slash",
            Error::Exists => "the name already exists in this directory",
            Error::NotEmpty => "the directory is not empty",
            Error::IsADirectory => "the name is a directory",
            Error::Unsupported => "the filesystem uses a feature this implementation lacks",
            Error::NoMemory => "out of memory",
        };
        f.write_str(text)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Проверить имя одного компонента пути.
pub(crate) fn check_name(name: &str) -> Result<()> {
    // 255 — предел `name_len` в записи каталога: поле однобайтовое.
    if name.is_empty() || name.len() > 255 {
        return Err(Error::BadName);
    }
    if name.contains('/') || name.contains('\0') || name == "." || name == ".." {
        return Err(Error::BadName);
    }
    Ok(())
}
