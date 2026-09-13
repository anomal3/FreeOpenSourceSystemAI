//! Том btrfs за интерфейсом [`crate::vfs`].
//!
//! Переходник, и только: разбор формата целиком в крейте `btrfs`, который
//! проверяется на хосте образом от `mkfs.btrfs`. Устроен по образцу
//! [`super::ext2fs`] — та же схема с замком, тем же `Counted` и теми же
//! обёртками, — и это сделано намеренно: два драйвера ФС, написанные
//! по-разному, расходятся в мелочах, которые потом объясняются как «на btrfs
//! почему-то иначе».
//!
//! # Пока только чтение
//!
//! Ни одной операции записи здесь нет, и это не заглушка, а состояние дел: том
//! btrfs у системы появляется готовым, созданным чужим `mkfs.btrfs`. Все
//! изменяющие методы [`crate::vfs::Node`] по умолчанию отвечают
//! [`VfsError::Unsupported`], и именно так и должно быть, пока запись не
//! написана и не проверена `btrfs check`.
//!
//! Отсюда же важное следствие для монтирования: том **не помечается
//! используемым**. В ext2 открытие редактора пишет в суперблок; здесь на диск
//! не уходит ни байта, и это единственный способ смонтировать том, в целости
//! которого мы не уверены, не сделав ему хуже.

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::block::Counted;
use crate::sync::Mutex;
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Диск вместе с разобранным на нём томом.
struct Inner {
    disk: Counted,
    fs: btrfs::Btrfs,
}

/// Смонтированный том.
pub struct BtrfsFs {
    inner: Mutex<Inner>,
}

/// То, что отдаётся в [`crate::fs::mount_at`].
pub struct BtrfsMount(Arc<BtrfsFs>);

/// Перевести ошибку крейта в ошибку VFS.
///
/// Отображение не механическое. Отдельного внимания стоит
/// [`btrfs::Error::BadChecksum`]: он значит «носитель отдал не те байты», то
/// есть ровно то, ради чего btrfs здесь и появился. Общего кода под это в VFS
/// нет, и `Corrupt` — ближайшее, что не врёт: содержимое действительно не то,
/// которым его считает том.
fn convert(err: btrfs::Error) -> VfsError {
    match err {
        btrfs::Error::Io => VfsError::Io,
        btrfs::Error::NotFound => VfsError::NotFound,
        btrfs::Error::NotADirectory | btrfs::Error::IsADirectory => VfsError::WrongKind,
        btrfs::Error::BadName => VfsError::BadPath,
        btrfs::Error::NoMemory => VfsError::OutOfMemory,
        btrfs::Error::Unsupported => VfsError::Unsupported,
        btrfs::Error::Corrupt | btrfs::Error::BadChecksum | btrfs::Error::TooSmall => {
            VfsError::Corrupt
        }
    }
}

fn kind_of(kind: btrfs::FileType) -> NodeKind {
    match kind {
        btrfs::FileType::Directory => NodeKind::Directory,
        _ => NodeKind::File,
    }
}

fn metadata_of(inode: &btrfs::Inode) -> Metadata {
    Metadata {
        kind: kind_of(inode.kind),
        size: inode.size,
        mode: inode.mode,
        uid: inode.uid,
        gid: inode.gid,
        // Время в btrfs 64-битное и знаковое, а в VFS — 32-битное без знака,
        // как в ext2. Обрезаем, а не паникуем: дата 2106 года на томе с
        // данными — повод показать не то число, но не повод не смонтировать.
        mtime: u32::try_from(inode.mtime).unwrap_or(0),
    }
}

impl BtrfsFs {
    /// Смонтировать том, начинающийся с сектора `first_lba`.
    pub fn mount(
        device: Box<dyn disk::BlockDevice + Send>,
        first_lba: u64,
    ) -> VfsResult<BtrfsMount> {
        let mut disk = Counted::new(device);
        let fs = btrfs::Btrfs::mount(&mut disk, first_lba).map_err(convert)?;
        Ok(BtrfsMount(Arc::new(Self {
            inner: Mutex::new(Inner { disk, fs }),
        })))
    }
}

impl Clone for BtrfsMount {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl BtrfsMount {
    /// Что сказать человеку при монтировании: метка, поколение, размеры,
    /// число кусков и сколько обращений к диску это стоило.
    ///
    /// Счётчик запросов здесь по той же причине, что и у ext2: он единственное
    /// доказательство, что чтение дошло до устройства.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, u64, usize, u64) {
        let guard = self.0.inner.lock();
        let (total, used) = guard.fs.usage();
        (
            guard.fs.generation(),
            total,
            used,
            guard.fs.chunks(),
            guard.disk.requests(),
        )
    }

    /// Метка тома.
    #[must_use]
    pub fn label(&self) -> alloc::string::String {
        alloc::string::String::from(self.0.inner.lock().fs.label())
    }

    /// Размер сектора данных и размер узла дерева.
    #[must_use]
    pub fn geometry(&self) -> (u32, u32) {
        let guard = self.0.inner.lock();
        (guard.fs.sector_size(), guard.fs.node_size())
    }

    /// Закрыли ли том чисто в прошлый раз.
    #[must_use]
    pub fn was_clean(&self) -> bool {
        self.0.inner.lock().fs.was_clean()
    }
}

impl FileSystem for BtrfsMount {
    fn name(&self) -> &'static str {
        "btrfs"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        let inode = {
            let mut guard = self.0.inner.lock();
            let Inner { disk, fs } = &mut *guard;
            fs.root(disk).map_err(convert)?
        };
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.0), inode }))
    }

    /// Сбрасывать нечего: том открыт только на чтение и не менялся.
    ///
    /// Пустая реализация здесь честнее, чем отсутствие: `sync_all` перед
    /// выключением проходит по всем томам, и том, который отвечает «готово»,
    /// не соврал.
    fn sync(&self) -> VfsResult<()> {
        Ok(())
    }
}

/// Узел тома: файл или каталог.
struct BtrfsNode {
    fs: Arc<BtrfsFs>,
    inode: btrfs::Inode,
}

impl Node for BtrfsNode {
    fn metadata(&self) -> Metadata {
        metadata_of(&self.inode)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.inode.kind != btrfs::FileType::Regular {
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
            // Права и размер живут в inode, а не в записи каталога — читать
            // приходится каждый. Та же цена, что у ext2, и по той же причине
            // приемлемая: перечисление вызывает человек, а не горячий путь.
            let node = fs.inode(disk, entry.inode).map_err(convert)?;
            out.push(DirEntry {
                name: entry.name,
                kind: kind_of(entry.kind),
                size: node.size,
                mode: node.mode,
                uid: node.uid,
                gid: node.gid,
                mtime: u32::try_from(node.mtime).unwrap_or(0),
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
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.fs), inode }))
    }
}
