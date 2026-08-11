//! Виртуальная файловая система: единый интерфейс поверх любой ФС.
//!
//! Слой существует ради того, чтобы остальное ядро — а позже и пользовательские
//! программы — не знали, какая именно файловая система под ними. Сейчас
//! реализация одна (FAT32 на RAM-диске), и соблазн обойтись без абстракции
//! велик; но по дорожной карте корневой ФС станет собственная inode-based, а
//! FAT32 останется только на загрузочном разделе, который требует спецификация
//! UEFI. То есть двух реализаций не избежать, и дешевле развести их сразу.
//!
//! # Метаданные
//!
//! [`Metadata`] с самого начала несёт `mode`, `uid` и `gid`, хотя FAT32 их не
//! хранит и подставляет значения по умолчанию. Причина та же, что заставила
//! завести эти поля в формате собственной ФС: добавить права после того, как на
//! диске появились пользовательские данные, значит менять формат и мигрировать.
//! Проверка прав (enforcement) появится вместе с пользовательскими процессами;
//! до тех пор поля переносятся, но ни на что не влияют.

pub mod path;
pub mod ramdisk;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// Размер блока, которым оперируют устройства. Совпадает с сектором и с тем,
/// что ожидает FAT.
pub const BLOCK_SIZE: usize = 512;

/// Что пошло не так при обращении к файловой системе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VfsError {
    /// Такого файла или каталога нет.
    NotFound,
    /// Ожидался каталог, а это файл (или наоборот).
    WrongKind,
    /// Путь синтаксически некорректен.
    BadPath,
    /// Носитель отдал не то, что обещал: испорченная структура на диске.
    Corrupt,
    /// Чтение за пределами устройства или файла.
    OutOfBounds,
    /// Устройство отказало.
    Io,
    /// Операция не поддерживается этой реализацией (например запись в
    /// смонтированный только на чтение образ).
    Unsupported,
    /// Не хватило памяти.
    OutOfMemory,
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::NotFound => "no such file or directory",
            Self::WrongKind => "path component is not of the expected kind",
            Self::BadPath => "malformed path",
            Self::Corrupt => "on-disk structure is inconsistent",
            Self::OutOfBounds => "read past the end of the device or file",
            Self::Io => "device error",
            Self::Unsupported => "operation not supported by this filesystem",
            Self::OutOfMemory => "out of memory",
        };
        f.write_str(text)
    }
}

pub type VfsResult<T> = Result<T, VfsError>;

/// Источник блоков фиксированного размера.
///
/// Намеренно отделён от файловой системы: одна и та же ФС должна одинаково
/// работать поверх RAM-диска, раздела на SD-карте и файла-образа, а различаются
/// они только тем, откуда берутся блоки.
pub trait BlockDevice: Send + Sync {
    /// Прочитать `buf.len() / BLOCK_SIZE` блоков, начиная с `block`.
    ///
    /// Длина буфера обязана быть кратна [`BLOCK_SIZE`].
    fn read_blocks(&self, block: u64, buf: &mut [u8]) -> VfsResult<()>;

    /// Записать блоки. Устройство только для чтения возвращает
    /// [`VfsError::Unsupported`].
    fn write_blocks(&self, block: u64, buf: &[u8]) -> VfsResult<()>;

    /// Сколько всего блоков на устройстве.
    fn block_count(&self) -> u64;
}

/// Тип узла файловой системы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Directory,
}

/// Права и владение — в терминах, которые FAT32 не хранит, а собственная ФС
/// будет хранить. См. заголовок модуля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    pub kind: NodeKind,
    pub size: u64,
    /// Права в unix-нотации: `rwxrwxrwx` в младших девяти битах.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
}

impl Metadata {
    /// Значения по умолчанию для ФС, которая прав не хранит: владелец root,
    /// чтение всем, запись владельцу, исполнение только у каталогов.
    #[must_use]
    pub const fn defaults(kind: NodeKind, size: u64) -> Self {
        let mode = match kind {
            NodeKind::Directory => 0o755,
            NodeKind::File => 0o644,
        };
        Self { kind, size, mode, uid: 0, gid: 0 }
    }
}

/// Запись в каталоге.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub kind: NodeKind,
    pub size: u64,
}

/// Узел файловой системы — файл или каталог.
///
/// Возвращается в `Box`, а не по значению: конкретный тип узла зависит от
/// реализации ФС, а вызывающий обязан работать с любой.
pub trait Node: Send + Sync {
    fn metadata(&self) -> Metadata;

    /// Прочитать до `buf.len()` байт, начиная со смещения. Возвращает, сколько
    /// прочитано: у конца файла это меньше запрошенного, и это не ошибка.
    ///
    /// Каталог возвращает [`VfsError::WrongKind`].
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize>;

    /// Перечислить содержимое каталога. Файл возвращает [`VfsError::WrongKind`].
    fn list(&self) -> VfsResult<Vec<DirEntry>>;

    /// Найти запись по имени в этом каталоге. Имя — один компонент пути, без
    /// разделителей.
    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>>;
}

/// Смонтированная файловая система.
pub trait FileSystem: Send + Sync {
    /// Человекочитаемое имя для диагностики: `"FAT32"`, и так далее.
    fn name(&self) -> &'static str;

    /// Корневой каталог.
    fn root(&self) -> VfsResult<Box<dyn Node>>;

    /// Найти узел по абсолютному пути.
    ///
    /// Реализация по умолчанию разбирает путь на компоненты и идёт от корня
    /// через [`Node::lookup`]; переопределять её стоит только там, где ФС умеет
    /// это быстрее.
    fn resolve(&self, path: &str) -> VfsResult<Box<dyn Node>> {
        let mut node = self.root()?;
        for component in path::components(path)? {
            node = node.lookup(component)?;
        }
        Ok(node)
    }
}
