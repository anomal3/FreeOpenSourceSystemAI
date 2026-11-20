//! exFAT в VFS (20.11): внешние диски и большие флешки, отформатированные
//! Windows, — чтение и запись.
//!
//! Сам формат — в `disk::exfat` (там же хостовые тесты на образе чужой
//! сборки); здесь — замок тома, узлы VFS и перевод ошибок. Узел хранит снимок
//! набора записей и перечитывает его перед каждым действием: размер файла
//! меняет запись через любой другой дескриптор.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use disk::exfat::{self, Cursor, Volume};

use crate::block::Shared;
use crate::sync::{Mutex, SpinLock};
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Права всех файлов: exFAT их не хранит, а съёмный диск — общий
/// (как `umask=0` у Linux для флешек).
const MODE: u16 = 0o777;

struct Inner {
    volume: Mutex<Volume<Shared>>,
}

/// Смонтированный том exFAT.
pub struct ExfatFs(Arc<Inner>);

impl ExfatFs {
    /// Открыть том с сектора `first_lba` длиной `sectors`.
    pub fn mount(device: Shared, first_lba: u64, sectors: u64, read_only: bool) -> Result<Self, exfat::Error> {
        let volume = Volume::mount(device, first_lba, sectors, read_only)?;
        Ok(Self(Arc::new(Inner { volume: Mutex::new(volume) })))
    }

    #[must_use]
    pub fn label(&self) -> String {
        String::from(self.0.volume.lock().label())
    }

    #[must_use]
    pub fn read_only(&self) -> bool {
        self.0.volume.lock().read_only()
    }

    #[must_use]
    pub fn was_dirty(&self) -> bool {
        self.0.volume.lock().was_dirty()
    }

    fn node(&self, node: exfat::Node) -> Box<dyn Node> {
        Box::new(ExfatNode::new(Arc::clone(&self.0), node))
    }

    /// Каталог по пути внутри тома.
    fn directory(&self, path: &str) -> VfsResult<exfat::Node> {
        let mut volume = self.0.volume.lock();
        let mut node = volume.root();
        for part in crate::vfs::path::components(path)? {
            node = volume.lookup(&node, part).map_err(convert)?;
        }
        if !node.is_dir() {
            return Err(VfsError::WrongKind);
        }
        Ok(node)
    }
}

fn convert(err: exfat::Error) -> VfsError {
    match err {
        exfat::Error::Disk(_) => VfsError::Io,
        exfat::Error::NotExfat | exfat::Error::Corrupt => VfsError::Corrupt,
        exfat::Error::NotFound => VfsError::NotFound,
        exfat::Error::Exists => VfsError::Exists,
        exfat::Error::NotEmpty => VfsError::NotEmpty,
        exfat::Error::NotDir | exfat::Error::IsDir => VfsError::WrongKind,
        exfat::Error::BadName => VfsError::BadPath,
        exfat::Error::NoSpace | exfat::Error::TooBig => VfsError::NoSpace,
        exfat::Error::ReadOnly => VfsError::ReadOnly,
    }
}

fn now() -> u64 {
    crate::time::now_unix().unwrap_or(0)
}

impl FileSystem for ExfatFs {
    fn name(&self) -> &'static str {
        "exFAT"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        let root = self.0.volume.lock().root();
        Ok(self.node(root))
    }

    fn space(&self) -> Option<(u64, u64)> {
        Some(self.0.volume.lock().space())
    }

    fn label(&self) -> Option<String> {
        let label = self.label();
        (!label.is_empty()).then_some(label)
    }

    fn rename(&self, old: &str, new: &str) -> VfsResult<()> {
        let (old_parent, old_name) = super::split_parent(old)?;
        let (new_parent, new_name) = super::split_parent(new)?;
        // Каталог в самого себя или в своё же поддерево — отказ, как у
        // `rename(2)` (EINVAL): иначе он отрезал бы себя от дерева.
        let old_trim = old.trim_end_matches('/');
        if new.starts_with(old_trim) && new[old_trim.len()..].starts_with('/') {
            return Err(VfsError::BadPath);
        }
        let from = self.directory(old_parent)?;
        let to = self.directory(new_parent)?;
        self.0.volume.lock().rename(&from, old_name, &to, new_name).map_err(convert)
    }

    fn sync(&self) -> VfsResult<()> {
        self.0.volume.lock().clean().map_err(convert)
    }

    /// Записи и так идут на носитель сразу; здесь — сброс его кэша и снятие
    /// пометки «грязный», чтобы выдернутый после паузы диск был чистым.
    fn writeback(&self) -> VfsResult<()> {
        self.0.volume.lock().clean().map_err(convert)
    }

    fn fsync(&self) -> VfsResult<()> {
        self.0.volume.lock().flush().map_err(convert)
    }
}

/// Файл или каталог тома.
struct ExfatNode {
    fs: Arc<Inner>,
    node: SpinLock<exfat::Node>,
    cursor: SpinLock<Cursor>,
}

impl ExfatNode {
    fn new(fs: Arc<Inner>, node: exfat::Node) -> Self {
        Self { fs, node: SpinLock::new(node), cursor: SpinLock::new(Cursor::default()) }
    }

    fn snapshot(&self) -> exfat::Node {
        self.node.lock().clone()
    }

    fn store(&self, node: exfat::Node) {
        *self.node.lock() = node;
    }

    /// Перечитать набор записей под замком тома.
    fn fresh(&self, volume: &mut Volume<Shared>) -> VfsResult<exfat::Node> {
        let node = volume.refresh(&self.snapshot()).map_err(convert)?;
        self.store(node.clone());
        Ok(node)
    }

    fn child(&self, node: exfat::Node) -> Box<dyn Node> {
        Box::new(ExfatNode::new(Arc::clone(&self.fs), node))
    }
}

fn metadata_of(node: &exfat::Node) -> Metadata {
    let kind = if node.is_dir() { NodeKind::Directory } else { NodeKind::File };
    Metadata { kind, size: node.size(), mode: MODE, uid: 0, gid: 0, mtime: node.modified.min(u64::from(u32::MAX)) as u32 }
}

impl Node for ExfatNode {
    fn metadata(&self) -> Metadata {
        let mut volume = self.fs.volume.lock();
        match self.fresh(&mut volume) {
            Ok(node) => metadata_of(&node),
            Err(_) => metadata_of(&self.snapshot()),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let mut volume = self.fs.volume.lock();
        let node = self.fresh(&mut volume)?;
        if node.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let mut cursor = *self.cursor.lock();
        let read = volume.read(&node, offset, buf, &mut cursor).map_err(convert)?;
        *self.cursor.lock() = cursor;
        Ok(read)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let mut volume = self.fs.volume.lock();
        let node = self.fresh(&mut volume)?;
        if !node.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let entries = volume.list(&node).map_err(convert)?;
        Ok(entries
            .iter()
            .map(|entry| {
                let meta = metadata_of(entry);
                DirEntry {
                    name: entry.name.clone(),
                    kind: meta.kind,
                    size: meta.size,
                    mode: meta.mode,
                    uid: 0,
                    gid: 0,
                    mtime: meta.mtime,
                }
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        let mut volume = self.fs.volume.lock();
        let node = self.fresh(&mut volume)?;
        if !node.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let found = match name {
            "." => node,
            _ => volume.lookup(&node, name).map_err(convert)?,
        };
        Ok(self.child(found))
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        let mut volume = self.fs.volume.lock();
        let node = volume.write(&self.snapshot(), offset, data, now()).map_err(convert)?;
        self.store(node);
        // Цепочка могла смениться (непрерывный файл стал цепочкой FAT).
        *self.cursor.lock() = Cursor::default();
        Ok(data.len())
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        let mut volume = self.fs.volume.lock();
        let node = volume.truncate(&self.snapshot(), size, now()).map_err(convert)?;
        self.store(node);
        *self.cursor.lock() = Cursor::default();
        Ok(())
    }

    /// Прав exFAT не хранит — принять молча, как `vfat` у Linux.
    fn chmod(&self, _mode: u16) -> VfsResult<()> {
        Ok(())
    }

    fn create(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let mut volume = self.fs.volume.lock();
        let node = volume.create(&self.snapshot(), name, false, now()).map_err(convert)?;
        Ok(self.child(node))
    }

    fn mkdir(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let mut volume = self.fs.volume.lock();
        let node = volume.create(&self.snapshot(), name, true, now()).map_err(convert)?;
        Ok(self.child(node))
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        self.fs.volume.lock().remove(&self.snapshot(), name, false).map_err(convert)
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        self.fs.volume.lock().remove(&self.snapshot(), name, true).map_err(convert)
    }

    fn file_id(&self) -> Option<(usize, u64)> {
        Some((Arc::as_ptr(&self.fs) as *const () as usize, self.snapshot().id()))
    }
}
