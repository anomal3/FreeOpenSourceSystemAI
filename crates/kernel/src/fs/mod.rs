//! Реализации конкретных файловых систем.
//!
// Заголовок тома разбирается целиком, хотя используется пока не всё: неполный
// разбор означал бы, что противоречивый образ проходит проверку. То же
// обоснование, что в `vfs`.
#![allow(dead_code)]
//!
//! Модуль отделён от [`crate::vfs`] намеренно: там живёт контракт (трейты
//! [`BlockDevice`](crate::vfs::BlockDevice), [`Node`](crate::vfs::Node),
//! [`FileSystem`](crate::vfs::FileSystem)) и то, что нужно всем реализациям —
//! разбор путей и RAM-диск; здесь — сами реализации. Пока их одна, FAT32, но по
//! дорожной карте рядом встанет собственная inode-based ФС, и разводить их
//! задним числом дороже, чем сразу.
//!
//! Всё, что читается с носителя, — внешние данные. Разбор устроен так, чтобы
//! испорченный образ приводил к [`VfsError::Corrupt`](crate::vfs::VfsError), а
//! не к панике или зацикливанию ядра.

pub mod ext2fs;
pub mod fat;

pub use ext2fs::Ext2Fs;
pub use fat::Fat32;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use crate::vfs::{DirEntry, FileSystem, NodeKind, VfsError, VfsResult};

/// Единственная смонтированная файловая система.
///
/// Не таблица монтирования, и это честнее, чем заготовка под неё: точек
/// монтирования пока одна, второй носитель появится вместе с драйвером диска, и
/// как именно будет устроено пространство имён (`/`, `/boot`, что-то ещё) —
/// решение, которое незачем принимать заранее. Что нужно уже сейчас — чтобы
/// оболочка могла читать файлы, не получая ФС аргументом через полкода.
static ROOT: SpinLock<Option<Box<dyn FileSystem>>> = SpinLock::new(None);

/// Запомнить смонтированную ФС как корневую.
pub fn set_root(fs: Box<dyn FileSystem>) {
    *ROOT.lock() = Some(fs);
}

/// Сделать что-нибудь с корневой ФС. `None`, если ничего не смонтировано.
///
/// Замыкание исполняется под локом, поэтому обращаться из него к оболочке или к
/// экрану нельзя. Для чтения файлов это не ограничение: RAM-диск — это
/// копирование из памяти, оно не ждёт.
pub fn with_root<R>(f: impl FnOnce(&dyn FileSystem) -> R) -> Option<R> {
    let guard = ROOT.lock();
    guard.as_ref().map(|fs| f(&**fs))
}

/// Перечислить каталог корневой ФС.
///
/// Внешний `None` означает «ничего не смонтировано» — это не ошибка пути, и
/// сообщение о ней должно быть другим.
pub fn list(path: &str) -> Option<VfsResult<Vec<DirEntry>>> {
    with_root(|fs| {
        let node = fs.resolve(path)?;
        node.list()
    })
}

/// Прочитать не более `limit` байт файла. Возвращает прочитанное и полный размер
/// файла — чтобы вызывающий мог сказать, что показал не всё.
pub fn read(path: &str, limit: usize) -> Option<VfsResult<(Vec<u8>, u64)>> {
    with_root(|fs| {
        let node = fs.resolve(path)?;
        let meta = node.metadata();
        if meta.kind != NodeKind::File {
            return Err(VfsError::WrongKind);
        }
        let want = (meta.size as usize).min(limit);
        let mut buf = Vec::new();
        // `try_reserve`, а не `vec![]`: размер приходит из файловой системы, то
        // есть снаружи, и отказ аллокатора обязан стать ошибкой, а не паникой.
        buf.try_reserve_exact(want).map_err(|_| VfsError::OutOfMemory)?;
        buf.resize(want, 0);
        let read = node.read_at(0, &mut buf)?;
        buf.truncate(read);
        Ok((buf, meta.size))
    })
}
