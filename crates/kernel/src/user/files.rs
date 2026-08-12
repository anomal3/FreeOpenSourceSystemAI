//! Открытые файлы программы.
//!
//! # Таблица принадлежит программе, а не системе
//!
//! С Phase 13a программ одновременно бывает несколько, и таблица у каждой своя:
//! она живёт в [`super::Program`], то есть в задаче. Общая таблица означала бы,
//! что дескриптор, выданный одной программе, виден другой — доступ, права на
//! который проверялись не для неё.
//!
//! Уничтожается таблица вместе с программой, на всех путях выхода, включая
//! отказ.
//!
//! # Почему предел маленький
//!
//! [`MAX_OPEN_FILES`] — восемь. Таблица не растёт, и это записано в договоре с
//! программой: вызов, который иногда отвечает «слишком много открытых файлов»,
//! понятнее вызова, который иногда съедает кучу ядра. Узел ext2 держит `Arc` на
//! том и разобранный inode — сотни байт, но выделяет их куча ядра, а не
//! программа, и предела у неё нет.

use alloc::boxed::Box;

use user_abi::{FD_FIRST, MAX_OPEN_FILES};

use crate::vfs::perm::{Access, Credentials};
use crate::vfs::{Node, NodeKind, VfsError};

/// Открытый файл: узел и то, докуда программа его дочитала.
struct Open {
    node: Box<dyn Node>,
    offset: u64,
}

/// Почему не получилось.
#[derive(Debug, Clone, Copy)]
pub enum FileError {
    /// Файловая система не смонтирована.
    NoFilesystem,
    /// Дескриптора с таким номером у программы нет.
    BadFd,
    /// Все [`MAX_OPEN_FILES`] мест заняты.
    TooManyFiles,
    /// Отказала файловая система — включая отказ в правах.
    Vfs(VfsError),
}

/// Таблица дескрипторов одной программы. Место `i` — это дескриптор
/// `i + FD_FIRST`.
pub struct Table {
    slots: [Option<Open>; MAX_OPEN_FILES],
}

impl Table {
    #[must_use]
    pub const fn new() -> Self {
        Self { slots: [const { None }; MAX_OPEN_FILES] }
    }

    /// Открыть файл от имени `cred`.
    ///
    /// Права проверяются здесь и только здесь: дальше дескриптор уже открыт, и
    /// перепроверять его на каждом чтении не нужно — ровно так же, как в Unix,
    /// где смена прав не отбирает уже открытый файл.
    pub fn open(&mut self, cred: Credentials, path: &str) -> Result<usize, FileError> {
        let node = crate::fs::resolve_as(cred, path, Access::READ)
            .ok_or(FileError::NoFilesystem)?
            .map_err(FileError::Vfs)?;

        // Каталог открывать нечем: вызова, который вернул бы список имён, в
        // договоре нет. Отказать здесь честнее, чем отдать дескриптор, любое
        // чтение из которого — ошибка.
        if node.metadata().kind != NodeKind::File {
            return Err(FileError::Vfs(VfsError::WrongKind));
        }

        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(FileError::TooManyFiles)?;
        self.slots[slot] = Some(Open { node, offset: 0 });
        Ok(slot + FD_FIRST)
    }

    /// Прочитать из дескриптора в буфер. Возвращает, сколько прочитано; ноль —
    /// это конец файла, а не ошибка.
    pub fn read(&mut self, fd: usize, buf: &mut [u8]) -> Result<usize, FileError> {
        let index = index_of(fd)?;
        let open = self.slots[index].as_mut().ok_or(FileError::BadFd)?;
        let read = open.node.read_at(open.offset, buf).map_err(FileError::Vfs)?;
        // Смещение двигается на прочитанное, а не на запрошенное: у конца файла
        // это разные числа, и второе увело бы следующее чтение за конец.
        open.offset += read as u64;
        Ok(read)
    }

    /// Закрыть дескриптор.
    pub fn close(&mut self, fd: usize) -> Result<(), FileError> {
        let index = index_of(fd)?;
        match self.slots[index].take() {
            Some(_) => Ok(()),
            None => Err(FileError::BadFd),
        }
    }

    /// Сколько дескрипторов осталось открытыми.
    ///
    /// Спрашивается при завершении программы — и на пути отказа тоже.
    /// Программа, забывшая закрыть файлы, ничем не отличается от снятой посреди
    /// чтения, и в журнале это видно.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.slots.iter().flatten().count()
    }
}

/// Номер места в таблице по дескриптору.
fn index_of(fd: usize) -> Result<usize, FileError> {
    fd.checked_sub(FD_FIRST)
        .filter(|index| *index < MAX_OPEN_FILES)
        .ok_or(FileError::BadFd)
}
