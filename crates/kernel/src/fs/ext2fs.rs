//! Корневая ФС ext2 за интерфейсом [`crate::vfs`].
//!
//! Разбор формата здесь не повторяется: он весь в крейте `ext2`, том же самом,
//! которым установщик этот том создавал. Здесь только переходник — из типов
//! крейта в типы VFS и обратно.
//!
//! # Почему замок
//!
//! Трейты VFS отдают узлы по `&self`: файл можно читать, ничего не изменяя.
//! Диск устроен иначе — у него одна очередь запросов, и обращение к ней меняет
//! её состояние. Замок и есть то место, где эти два взгляда сходятся, и он же
//! ровно то, что понадобилось бы настоящему драйверу в любом случае: два
//! одновременных запроса в одну очередь не отправить.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};
use crate::virtio::blk::VirtioBlk;

/// Диск вместе с разобранной на нём файловой системой.
struct Inner {
    disk: VirtioBlk,
    fs: ext2::Ext2,
}

/// Смонтированный том.
pub struct Ext2Fs {
    inner: SpinLock<Inner>,
}

/// То, что отдаётся в [`crate::fs::set_root`].
///
/// Отдельная обёртка вокруг `Arc`, потому что узлы обязаны держать ссылку на
/// том: [`Node::lookup`] возвращает новый узел, а тому нужен доступ к диску.
pub struct Ext2Mount(Arc<Ext2Fs>);

/// Перевести ошибку крейта в ошибку VFS.
///
/// Отображение не механическое: `Corrupt` и `Unsupported` означают разное для
/// того, кто увидит сообщение. Первое — испорченный носитель, второе — том,
/// созданный не нами.
fn convert(err: ext2::Error) -> VfsError {
    match err {
        ext2::Error::Io => VfsError::Io,
        ext2::Error::Corrupt => VfsError::Corrupt,
        ext2::Error::NotFound => VfsError::NotFound,
        ext2::Error::NotADirectory => VfsError::WrongKind,
        ext2::Error::BadName => VfsError::BadPath,
        ext2::Error::NoMemory => VfsError::OutOfMemory,
        ext2::Error::Unsupported => VfsError::Unsupported,
        _ => VfsError::Corrupt,
    }
}

fn kind_of(kind: ext2::FileType) -> NodeKind {
    match kind {
        ext2::FileType::Directory => NodeKind::Directory,
        // Всё, что не каталог, показывается файлом. Символических ссылок и
        // устройств ext2 у нас не создаёт, а притворяться, что мы умеем их
        // различать, значило бы обещать чтение, которого нет.
        _ => NodeKind::File,
    }
}

fn metadata_of(inode: &ext2::Inode) -> Metadata {
    Metadata {
        kind: kind_of(inode.kind),
        size: inode.size,
        mode: inode.mode,
        uid: inode.uid,
        gid: inode.gid,
    }
}

impl Ext2Fs {
    /// Смонтировать том, начинающийся с сектора `first_lba`.
    pub fn mount(mut disk: VirtioBlk, first_lba: u64) -> VfsResult<Ext2Mount> {
        let fs = ext2::Ext2::mount(&mut disk, first_lba).map_err(convert)?;
        Ok(Ext2Mount(Arc::new(Self {
            inner: SpinLock::new(Inner { disk, fs }),
        })))
    }

    fn root_inode(&self) -> VfsResult<ext2::Inode> {
        let mut guard = self.inner.lock();
        let Inner { disk, fs } = &mut *guard;
        fs.root(disk).map_err(convert)
    }
}

impl Ext2Mount {
    /// Геометрия тома и число обращений к диску — строка диагностики при
    /// монтировании. Счётчик запросов здесь не для красоты: он единственное
    /// доказательство, что чтение действительно дошло до устройства, а не было
    /// обслужено чем-то по дороге.
    #[must_use]
    pub fn stats(&self) -> (u32, u32, u32, u64) {
        let guard = self.0.inner.lock();
        let geometry = guard.fs.geometry();
        (
            geometry.blocks,
            geometry.block_size.bytes(),
            geometry.groups,
            guard.disk.requests(),
        )
    }
}

impl FileSystem for Ext2Mount {
    fn name(&self) -> &'static str {
        "ext2"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        let inode = self.0.root_inode()?;
        Ok(Box::new(Ext2Node { fs: Arc::clone(&self.0), inode }))
    }
}

/// Узел тома: файл или каталог.
struct Ext2Node {
    fs: Arc<Ext2Fs>,
    inode: ext2::Inode,
}

impl Node for Ext2Node {
    fn metadata(&self) -> Metadata {
        metadata_of(&self.inode)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.inode.kind != ext2::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs } = &mut *guard;
        fs.read_at(disk, &self.inode, offset, buf).map_err(convert)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs } = &mut *guard;
        let entries = fs.list(disk, &self.inode).map_err(convert)?;

        let mut out = Vec::new();
        out.try_reserve_exact(entries.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for entry in entries {
            // Ни размера, ни прав запись каталога не хранит — всё это в inode,
            // и его приходится читать на каждое имя. Лишнее обращение к диску
            // на запись заметно на большом каталоге, но перечисление вызывает
            // человек командой `ls`, а не горячий путь.
            let node = fs.inode(disk, entry.inode).map_err(convert)?;
            out.push(DirEntry {
                name: entry.name,
                kind: kind_of(entry.kind),
                size: node.size,
                mode: node.mode,
                uid: node.uid,
                gid: node.gid,
            });
        }
        Ok(out)
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        let inode = {
            let mut guard = self.fs.inner.lock();
            let Inner { disk, fs } = &mut *guard;
            let entry = fs
                .lookup(disk, &self.inode, name)
                .map_err(convert)?
                .ok_or(VfsError::NotFound)?;
            fs.inode(disk, entry.inode).map_err(convert)?
        };
        Ok(Box::new(Ext2Node { fs: Arc::clone(&self.fs), inode }))
    }
}

