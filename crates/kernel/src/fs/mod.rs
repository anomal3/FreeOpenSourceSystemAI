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
//!
//! # Два входа: с проверкой прав и без
//!
//! [`resolve_as`] спрашивает права у каждого каталога на пути и у самого узла;
//! [`read`] и [`list`] не спрашивают ничего. Это не две степени аккуратности, а
//! два разных вызывающих. Первый — системный вызов пользовательской программы,
//! то есть недоверенная сторона. Второй — код ядра: оболочка, файловый
//! менеджер, чтение `/etc/passwd` при загрузке. Проверять код ядра значило бы
//! проверять того, кто в любом случае может прочитать диск сектор за сектором;
//! граница проходит по кольцу привилегий, а не по слою кода. Подробнее — в
//! [`crate::vfs::perm`].

pub mod ext2fs;
pub mod fat;

pub use ext2fs::Ext2Fs;
pub use fat::Fat32;

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::sync::Mutex;
use crate::vfs::perm::{Access, Credentials, permits};
use crate::vfs::{DirEntry, FileSystem, Node, NodeKind, VfsError, VfsResult};

/// Единственная смонтированная файловая система.
///
/// Не таблица монтирования, и это честнее, чем заготовка под неё: точек
/// монтирования пока одна, второй носитель появится вместе с драйвером диска, и
/// как именно будет устроено пространство имён (`/`, `/boot`, что-то ещё) —
/// решение, которое незачем принимать заранее. Что нужно уже сейчас — чтобы
/// оболочка могла читать файлы, не получая ФС аргументом через полкода.
static ROOT: Mutex<Option<Box<dyn FileSystem>>> = Mutex::new(None);

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

/// Найти узел по пути, проверяя права на каждом шаге.
///
/// Проверяется не только сам файл. Чтобы дойти до `/root/notes.txt`, нужно
/// пройти сквозь `/` и `/root`, а для этого нужен бит поиска
/// ([`Access::SEARCH`]) на каждом из них. Каталог `0700`, принадлежащий чужому,
/// закрывает всё, что внутри, — сколько бы прав ни стояло на самих файлах. Без
/// прохода по каталогам проверка выглядела бы работающей и не работала бы:
/// установщик выставляет права именно так, как это принято в Unix, то есть
/// рассчитывая на такую семантику.
///
/// Внешний `None` означает «ничего не смонтировано»: это не отказ в правах и не
/// отсутствие файла, и путать их нельзя.
pub fn resolve_as(
    cred: Credentials,
    path: &str,
    want: Access,
) -> Option<VfsResult<Box<dyn Node>>> {
    with_root(|fs| {
        let mut node = fs.root()?;
        for component in crate::vfs::path::components(path)? {
            // Право пройти спрашивается у каталога, в котором мы стоим, — до
            // того, как станет известно, есть ли там такое имя. Иначе ответ
            // «нет такого файла» рассказывал бы о содержимом каталога, в
            // который спрашивающему не дали заглянуть.
            if !permits(cred, &node.metadata(), Access::SEARCH) {
                return Err(VfsError::PermissionDenied);
            }
            node = node.lookup(component)?;
        }
        if !permits(cred, &node.metadata(), want) {
            return Err(VfsError::PermissionDenied);
        }
        Ok(node)
    })
}

/// Разделить путь на «каталог» и «последнее имя».
///
/// Создание и удаление работают не с путём целиком, а с именем внутри
/// каталога: право спрашивается у **каталога**, а не у того, чего в нём пока
/// нет либо уже не будет.
fn split_parent(path: &str) -> VfsResult<(&str, &str)> {
    let trimmed = path.trim_end_matches('/');
    let (parent, name) = match trimmed.rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", trimmed),
    };
    if name.is_empty() || name == "." || name == ".." {
        return Err(VfsError::BadPath);
    }
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

/// Создать файл по пути от имени `cred`.
///
/// Право писать спрашивается у каталога — там, где появляется новая запись.
/// Прав на сам файл не существует: его ещё нет.
pub fn create_as(cred: Credentials, path: &str, mode: u16) -> Option<VfsResult<Box<dyn Node>>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    Some(dir.create(name, mode, cred.uid, cred.gid))
}

/// Создать каталог по пути от имени `cred`.
pub fn mkdir_as(cred: Credentials, path: &str, mode: u16) -> Option<VfsResult<()>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    Some(dir.mkdir(name, mode, cred.uid, cred.gid).map(|_| ()))
}

/// Удалить файл или пустой каталог по пути от имени `cred`.
///
/// Что именно удалять, решает не вызывающий, а тип того, что нашлось: два
/// вызова на одно действие означали бы, что `rm` каталога отказывает не потому,
/// что каталог не пуст, а потому, что человек выбрал не ту команду.
pub fn remove_as(cred: Credentials, path: &str) -> Option<VfsResult<()>> {
    let (parent, name) = match split_parent(path) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let dir = match resolve_as(cred, parent, Access::WRITE)? {
        Ok(dir) => dir,
        Err(err) => return Some(Err(err)),
    };
    let kind = match dir.lookup(name) {
        Ok(node) => node.metadata().kind,
        Err(err) => return Some(Err(err)),
    };
    Some(match kind {
        NodeKind::Directory => dir.rmdir(name),
        NodeKind::File => dir.unlink(name),
    })
}

/// Переименовать файл или каталог от имени `cred`.
///
/// Право писать спрашивается у **обоих** каталогов: имя исчезает в одном и
/// появляется в другом, и каждое из этих действий — запись в свой каталог. Прав
/// на сам файл не требуется, ровно как в Unix: переименование меняет запись
/// каталога, а не содержимое.
///
/// Оба вопроса задаются до того, как файловая система что-нибудь сделает.
/// Порядок обязателен: отказ на середине оставил бы файл видимым под двумя
/// именами или ни под одним.
pub fn rename_as(cred: Credentials, old: &str, new: &str) -> Option<VfsResult<()>> {
    let (old_parent, _) = match split_parent(old) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };
    let (new_parent, _) = match split_parent(new) {
        Ok(pair) => pair,
        Err(err) => return Some(Err(err)),
    };

    // Узлы каталогов дальше не нужны — нужен ответ «пустят ли». Сама операция
    // идёт через файловую систему, потому что затрагивает оба каталога сразу.
    if let Err(err) = resolve_as(cred, old_parent, Access::WRITE)? {
        return Some(Err(err));
    }
    if let Err(err) = resolve_as(cred, new_parent, Access::WRITE)? {
        return Some(Err(err));
    }

    with_root(|fs| fs.rename(old, new))
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

/// Проверить корневую ФС, ничего не меняя.
///
/// Внешний `None` — «ничего не смонтировано», внутренний — «эта файловая
/// система себя проверять не умеет». Разные ответы на разные вопросы, и
/// сливать их в один было бы враньём: в первом случае корня нет вовсе, во
/// втором он есть и с ним всё в порядке настолько, насколько это вообще можно
/// узнать.
pub fn check_root() -> Option<Option<VfsResult<crate::vfs::CheckSummary>>> {
    with_root(|fs| fs.check())
}

/// Сбросить корневую ФС на носитель.
///
/// `None` означает «ничего не смонтировано» — это не отказ, а обычное
/// состояние живого ISO, и путать его с ошибкой нельзя.
pub fn sync_root() -> Option<VfsResult<()>> {
    with_root(|fs| fs.sync())
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
