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

use user_abi::{FD_FIRST, MAX_OPEN_FILES, O_CREATE, O_TRUNC, O_WRITE};

/// Права, с которыми создаётся файл по [`O_CREATE`].
///
/// Постоянная, а не аргумент вызова: `umask` и режим создания — это уже
/// политика, а её место там, где есть кому её задавать. Читать всем, писать
/// владельцу — то же, что даёт `touch` под обычным `umask` 022.
const DEFAULT_MODE: u16 = 0o644;

use crate::vfs::perm::{Access, Credentials};
use crate::vfs::{Node, NodeKind, VfsError};

/// Открытый файл: узел, то, докуда программа его дочитала, и можно ли в него
/// писать.
struct Open {
    node: Box<dyn Node>,
    offset: u64,
    /// Право писать спрошено при открытии и запомнено здесь. Перепроверять его
    /// на каждой записи не нужно и неверно: в Unix смена прав не отбирает уже
    /// открытый файл, и ровно на это рассчитывает всякий, кто держит файл
    /// открытым дольше одной операции.
    writable: bool,
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
    pub fn open(&mut self, cred: Credentials, path: &str, flags: usize) -> Result<usize, FileError> {
        let writable = flags & O_WRITE != 0;
        let want = if writable { Access::WRITE } else { Access::READ };

        let node = match crate::fs::resolve_as(cred, path, want) {
            Some(Ok(node)) => node,
            // Файла нет, но просили создать. Создаём — от имени того же
            // сеанса и в том каталоге, который назвал путь; право писать в этот
            // каталог спросит `create_as`.
            Some(Err(VfsError::NotFound)) if flags & O_CREATE != 0 => {
                crate::fs::create_as(cred, path, DEFAULT_MODE)
                    .ok_or(FileError::NoFilesystem)?
                    .map_err(FileError::Vfs)?
            }
            Some(Err(err)) => return Err(FileError::Vfs(err)),
            None => return Err(FileError::NoFilesystem),
        };

        // Каталог открывать нечем: вызова, который вернул бы список имён, в
        // договоре нет. Отказать здесь честнее, чем отдать дескриптор, любое
        // чтение из которого — ошибка.
        if node.metadata().kind != NodeKind::File {
            return Err(FileError::Vfs(VfsError::WrongKind));
        }
        if flags & O_TRUNC != 0 && writable {
            node.truncate(0).map_err(FileError::Vfs)?;
        }

        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(FileError::TooManyFiles)?;
        self.slots[slot] = Some(Open { node, offset: 0, writable });
        Ok(slot + FD_FIRST)
    }

    /// Записать в дескриптор. Возвращает, сколько записано.
    pub fn write(&mut self, fd: usize, data: &[u8]) -> Result<usize, FileError> {
        let index = index_of(fd)?;
        let open = self.slots[index].as_mut().ok_or(FileError::BadFd)?;
        if !open.writable {
            return Err(FileError::Vfs(VfsError::PermissionDenied));
        }
        let written = open.node.write_at(open.offset, data).map_err(FileError::Vfs)?;
        open.offset += written as u64;
        Ok(written)
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
