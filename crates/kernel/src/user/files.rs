//! Открытые файлы программы.
//!
//! # Почему таблица одна на систему
//!
//! Потому что программа одна: второй запуск до возврата первого отвергается
//! (см. [`super::run`]). Таблица дескрипторов — это состояние процесса, и когда
//! процесс появится по-настоящему, она переедет в него; пока же отдельная
//! структура на одного владельца была бы честной ровно настолько же и стоила бы
//! лишнего слоя.
//!
//! Что действительно важно и сделано здесь — **таблица очищается вместе с
//! программой**, на всех путях выхода, включая отказ. Иначе следующая программа
//! унаследовала бы чужие дескрипторы: читала бы файл, который открывала не она,
//! и права на который проверялись не для неё.
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

use crate::sync::SpinLock;
use crate::vfs::perm::Access;
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

/// Таблица дескрипторов. Индекс `i` — это дескриптор `i + FD_FIRST`.
static TABLE: SpinLock<[Option<Open>; MAX_OPEN_FILES]> =
    SpinLock::new([const { None }; MAX_OPEN_FILES]);

/// Открыть файл от имени сеанса.
///
/// Права проверяются здесь и только здесь: дальше дескриптор уже открыт, и
/// перепроверять его на каждом чтении не нужно — ровно так же, как в Unix, где
/// смена прав не отбирает уже открытый файл.
pub fn open(path: &str) -> Result<usize, FileError> {
    let cred = super::session::credentials();
    let node = crate::fs::resolve_as(cred, path, Access::READ)
        .ok_or(FileError::NoFilesystem)?
        .map_err(FileError::Vfs)?;

    // Каталог открывать нечем: вызова, который вернул бы список имён, в
    // договоре нет. Отказать здесь честнее, чем отдать дескриптор, любое
    // чтение из которого — ошибка.
    if node.metadata().kind != NodeKind::File {
        return Err(FileError::Vfs(VfsError::WrongKind));
    }

    let mut table = TABLE.lock();
    let slot = table
        .iter()
        .position(Option::is_none)
        .ok_or(FileError::TooManyFiles)?;
    table[slot] = Some(Open { node, offset: 0 });
    Ok(slot + FD_FIRST)
}

/// Прочитать из дескриптора в буфер. Возвращает, сколько прочитано; ноль — это
/// конец файла, а не ошибка.
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize, FileError> {
    let mut table = TABLE.lock();
    let open = slot_mut(&mut table, fd)?;
    let read = open.node.read_at(open.offset, buf).map_err(FileError::Vfs)?;
    // Смещение двигается на прочитанное, а не на запрошенное: у конца файла
    // это разные числа, и второе увело бы следующее чтение за конец.
    open.offset += read as u64;
    Ok(read)
}

/// Закрыть дескриптор.
pub fn close(fd: usize) -> Result<(), FileError> {
    let mut table = TABLE.lock();
    let index = index_of(fd)?;
    match table[index].take() {
        Some(_) => Ok(()),
        None => Err(FileError::BadFd),
    }
}

/// Закрыть всё, что осталось открытым, и сказать сколько.
///
/// Вызывается при завершении программы — и на пути отказа тоже. Возвращаемое
/// число не для красоты: программа, забывшая закрыть файлы, ничем не отличается
/// от программы, снятой посреди чтения, и в журнале это видно.
pub fn close_all() -> usize {
    let mut table = TABLE.lock();
    let mut closed = 0;
    for slot in table.iter_mut() {
        if slot.take().is_some() {
            closed += 1;
        }
    }
    closed
}

/// Номер места в таблице по дескриптору.
fn index_of(fd: usize) -> Result<usize, FileError> {
    fd.checked_sub(FD_FIRST)
        .filter(|index| *index < MAX_OPEN_FILES)
        .ok_or(FileError::BadFd)
}

fn slot_mut<'a>(
    table: &'a mut [Option<Open>; MAX_OPEN_FILES],
    fd: usize,
) -> Result<&'a mut Open, FileError> {
    let index = index_of(fd)?;
    table[index].as_mut().ok_or(FileError::BadFd)
}
