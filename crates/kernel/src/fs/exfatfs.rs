//! exFAT в VFS (20.11): внешние диски и большие флешки, отформатированные
//! Windows, — чтение и запись.
//!
//! Сам формат — в `disk::exfat` (там же хостовые тесты на образе чужой
//! сборки); здесь — замок тома, узлы VFS и перевод ошибок.
//!
//! # Скорость (21.11, журнал 13: D2R с exFAT-SSD)
//!
//! До 21.11 каждое чтение брало один замок тома на всё время ожидания USB,
//! перечитывало набор записей файла и шло на носитель мимо всякого кэша:
//! потоки D2R читали строго по одному, ~0,2–0,4 мс на запрос, `openat` —
//! 0,4 мс (разбор всего каталога на каждый шаг пути). Теперь:
//! - под томом — кэш блоков ядра ([`Cached`], 4 КиБ в кадрах памяти, до
//!   1/16 памяти на все тома): и служебные данные, и данные файлов;
//! - данные файлов читаются как у ext2: под замком тома — только план
//!   (куски тома) и то, что уже в кэше; пропуски — с носителя **без** замка
//!   тома, прочитанное ложится в кэш, если в эти секторы за это время никто
//!   не писал; при последовательном чтении — упреждающее чтение
//!   ([`READAHEAD`]);
//! - номер изменения тома ([`Inner::generation`]): любая запись через этот
//!   том его увеличивает; пока он прежний, снимок набора записей узла верен,
//!   и чтение не перечитывает его с носителя;
//! - кэш каталогов для поиска имени и списка: разобранный каталог с
//!   индексом по имени в прописных буквах, сбрасывается любой записью.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use disk::BlockDevice;
use disk::exfat::{self, Cursor, Volume};

use crate::block::Shared;
use crate::block::cache::Cached;
use crate::sync::{Mutex, SpinLock};
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Права всех файлов: exFAT их не хранит, а съёмный диск — общий
/// (как `umask=0` у Linux для флешек).
const MODE: u16 = 0o777;

/// Блок кэша (как [`Cached`] и кэш служебных данных `disk::exfat`).
const BLOCK: u64 = 4096;
/// Самый длинный кусок одного чтения мимо замка тома.
const PIECE: usize = 1 << 20;
/// Упреждающее чтение при последовательном чтении файла.
const READAHEAD: u64 = 512 * 1024;
/// Упреждение после пропуска при чтении вразброс (не подряд), байт
/// (`exfatra KIB`). hw-log15: 124 KiB (was 60): with the 128 KiB USB window
/// a miss plus its read-ahead is one disk command, and a USB command costs
/// its round trip whatever its length up to that.
pub static READAHEAD_RANDOM: AtomicU64 = AtomicU64::new(124 * 1024);
/// File data reads (pieces): count, ns, those that went to the disk and
/// the bytes they read (data + read-ahead) - perf-diag.
static READS: AtomicU64 = AtomicU64::new(0);
static READ_NS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static MISS_NS: AtomicU64 = AtomicU64::new(0);
static MISS_BYTES: AtomicU64 = AtomicU64::new(0);

/// `(reads, ns, reads that went to the disk, their ns, bytes read from the
/// disk)` of exFAT file data since boot.
#[must_use]
pub fn read_stats() -> (u64, u64, u64, u64, u64) {
    (
        READS.load(Ordering::Relaxed),
        READ_NS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        MISS_NS.load(Ordering::Relaxed),
        MISS_BYTES.load(Ordering::Relaxed),
    )
}
/// Больше стольких узлов во всех запомненных каталогах — кэш сбрасывается
/// (куча ядра общая и небольшая).
const DIR_CACHE_NODES: usize = 32 * 1024;

type Dev = Cached<Shared>;

/// Быстрый путь включён (`exfatcache on|off` оболочки ядра — замер «до/после»
/// на одном прогоне): выключенный читает как до 21.11 — под замком тома,
/// с носителя мимо кэша, набор записей и каталог перечитываются каждый раз.
static FAST: AtomicBool = AtomicBool::new(true);

/// Включить или выключить быстрый путь.
pub fn set_fast(on: bool) {
    FAST.store(on, Ordering::Relaxed);
}

/// Включён ли быстрый путь.
#[must_use]
pub fn fast() -> bool {
    FAST.load(Ordering::Relaxed)
}

/// Прочитать байты тома с любого смещения прямо с носителя (секторами).
fn read_direct(lane: &mut Shared, base: u64, sector: u64, at: u64, buf: &mut [u8]) -> VfsResult<()> {
    let first = at / sector;
    let last = (at + buf.len() as u64).div_ceil(sector);
    let mut bounce: Vec<u8> = Vec::new();
    bounce.try_reserve_exact(((last - first) * sector) as usize).map_err(|_| VfsError::OutOfMemory)?;
    bounce.resize(((last - first) * sector) as usize, 0);
    lane.read(base + first, &mut bounce).map_err(|_| VfsError::Io)?;
    let skip = (at - first * sector) as usize;
    buf.copy_from_slice(&bounce[skip..skip + buf.len()]);
    Ok(())
}

/// Разобранный каталог: узлы и индекс имён в прописных буквах.
struct Listing {
    nodes: Vec<exfat::Node>,
    by_name: BTreeMap<Vec<u16>, usize>,
}

struct DirCache {
    generation: u64,
    nodes: usize,
    dirs: BTreeMap<u64, Arc<Listing>>,
}

struct Inner {
    volume: Mutex<Volume<Dev>>,
    /// Тот же носитель — для чтения мимо замка тома.
    lane: Shared,
    /// Первый сектор тома, сектор носителя, длина тома в байтах.
    geometry: (u64, u64, u64),
    upcase: Vec<u16>,
    /// Номер изменения тома: растёт после каждой записи через этот том.
    generation: AtomicU64,
    dirs: SpinLock<DirCache>,
}

impl Inner {
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// После записи (под замком тома).
    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Имя в прописных буквах UTF-16 — ключ поиска (как сравнивает Windows).
    fn key(&self, name: &str) -> Vec<u16> {
        name.encode_utf16().map(|u| self.upcase.get(usize::from(u)).copied().unwrap_or(u)).collect()
    }

    fn cached_listing(&self, id: u64, generation: u64) -> Option<Arc<Listing>> {
        if !fast() {
            return None;
        }
        let dirs = self.dirs.lock();
        if dirs.generation != generation {
            return None;
        }
        dirs.dirs.get(&id).cloned()
    }

    /// Разобранный каталог: из кэша или с тома (под замком тома).
    fn listing(&self, dir: &exfat::Node, generation: u64, volume: &mut Volume<Dev>) -> VfsResult<Arc<Listing>> {
        if let Some(listing) = self.cached_listing(dir.id(), generation) {
            return Ok(listing);
        }
        let nodes = volume.list(dir).map_err(convert)?;
        let mut by_name = BTreeMap::new();
        for (index, node) in nodes.iter().enumerate() {
            by_name.entry(self.key(&node.name)).or_insert(index);
        }
        let count = nodes.len();
        let listing = Arc::new(Listing { nodes, by_name });
        if !fast() {
            return Ok(listing);
        }
        let mut dirs = self.dirs.lock();
        if dirs.generation != generation || dirs.nodes + count > DIR_CACHE_NODES {
            dirs.dirs.clear();
            dirs.nodes = 0;
            dirs.generation = generation;
        }
        if count <= DIR_CACHE_NODES && dirs.dirs.insert(dir.id(), Arc::clone(&listing)).is_none() {
            dirs.nodes += count;
        }
        Ok(listing)
    }
}

/// Смонтированный том exFAT.
pub struct ExfatFs(Arc<Inner>);

impl ExfatFs {
    /// Открыть том с сектора `first_lba` длиной `sectors`.
    pub fn mount(device: Shared, first_lba: u64, sectors: u64, read_only: bool) -> Result<Self, exfat::Error> {
        let lane = device.clone();
        // Запись — сразу на носитель (как было): выдернутый диск теряет не
        // больше, чем при прежнем порядке; кэш только для чтения.
        let volume = Volume::mount(Cached::new(device), first_lba, sectors, read_only)?;
        let geometry = volume.geometry();
        let upcase = volume.upcase_table().to_vec();
        Ok(Self(Arc::new(Inner {
            volume: Mutex::new(volume),
            lane,
            geometry,
            upcase,
            generation: AtomicU64::new(0),
            dirs: SpinLock::new(DirCache { generation: 0, nodes: 0, dirs: BTreeMap::new() }),
        })))
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
        Box::new(ExfatNode::new(Arc::clone(&self.0), node, self.0.generation()))
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
        let mut volume = self.0.volume.lock();
        let result = volume.rename(&from, old_name, &to, new_name).map_err(convert);
        self.0.changed();
        result
    }

    /// `sync` зовут выключение и перезагрузка (`fs::sync_all`): том
    /// закрывается чисто, пометка «грязный» снимается и с тома, грязного при
    /// открытии (21.11).
    fn sync(&self) -> VfsResult<()> {
        let mut volume = self.0.volume.lock();
        let was = volume.was_dirty();
        volume.close().map_err(convert)?;
        if was {
            crate::kprintln!("  exfat       : '{}' was not clean at mount - marked clean on close", volume.label());
        }
        Ok(())
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
    /// Снимок набора записей и номер изменения тома, при котором он снят.
    node: SpinLock<(exfat::Node, u64)>,
    cursor: SpinLock<Cursor>,
    /// Где кончилось прошлое чтение (для упреждающего).
    next: AtomicU64,
}

impl ExfatNode {
    fn new(fs: Arc<Inner>, node: exfat::Node, generation: u64) -> Self {
        Self { fs, node: SpinLock::new((node, generation)), cursor: SpinLock::new(Cursor::default()), next: AtomicU64::new(u64::MAX) }
    }

    fn snapshot(&self) -> exfat::Node {
        self.node.lock().0.clone()
    }

    fn store(&self, node: exfat::Node, generation: u64) {
        *self.node.lock() = (node, generation);
    }

    /// Снимок, если том с него не менялся.
    fn current(&self) -> Option<exfat::Node> {
        if !fast() {
            return None;
        }
        let generation = self.fs.generation();
        let guard = self.node.lock();
        (guard.1 == generation).then(|| guard.0.clone())
    }

    /// Перечитать набор записей под замком тома (если том менялся).
    fn fresh(&self, volume: &mut Volume<Dev>) -> VfsResult<exfat::Node> {
        let generation = self.fs.generation();
        if fast() {
            let guard = self.node.lock();
            if guard.1 == generation {
                return Ok(guard.0.clone());
            }
        }
        let node = volume.refresh(&self.snapshot()).map_err(convert)?;
        self.store(node.clone(), generation);
        Ok(node)
    }

    /// Снимок без замка тома, если можно; иначе — перечитанный.
    fn fresh_unlocked(&self) -> VfsResult<exfat::Node> {
        match self.current() {
            Some(node) => Ok(node),
            None => {
                let mut volume = self.fs.volume.lock();
                self.fresh(&mut volume)
            }
        }
    }

    fn child(&self, node: exfat::Node) -> Box<dyn Node> {
        Box::new(ExfatNode::new(Arc::clone(&self.fs), node, self.fs.generation()))
    }

    /// Разобранный каталог этого узла.
    fn listing(&self) -> VfsResult<(exfat::Node, Arc<Listing>)> {
        let generation = self.fs.generation();
        if let Some(node) = self.current() {
            if !node.is_dir() {
                return Err(VfsError::WrongKind);
            }
            if let Some(listing) = self.fs.cached_listing(node.id(), generation) {
                return Ok((node, listing));
            }
        }
        let mut volume = self.fs.volume.lock();
        let node = self.fresh(&mut volume)?;
        if !node.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let generation = self.fs.generation();
        let listing = self.fs.listing(&node, generation, &mut volume)?;
        Ok((node, listing))
    }

    /// Прочитать не больше [`PIECE`] байт с `offset` (вызывающий режет).
    fn read_piece(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let fs = &*self.fs;
        let (base, sector, volume_bytes) = fs.geometry;
        let sequential = self.next.load(Ordering::Relaxed) == offset;
        // Блоки тома (по 4 КиБ от начала тома), которых нет в кэше: нужные
        // для чтения и упреждаемые.
        let mut wanted: Vec<u64> = Vec::new();
        let mut ahead: Vec<u64> = Vec::new();
        let mut block = [0u8; BLOCK as usize];
        let (extents, from_disk, total, since) = {
            let mut volume = fs.volume.lock();
            let node = self.fresh(&mut volume)?;
            if node.is_dir() {
                return Err(VfsError::WrongKind);
            }
            let mut cursor = *self.cursor.lock();
            let (extents, from_disk, total) =
                volume.plan_read(&node, offset, buf.len() as u64, &mut cursor).map_err(convert)?;
            *self.cursor.lock() = cursor;
            if total == 0 {
                return Ok(0);
            }
            // Геометрия, при которой блоки 4 КиБ не годятся, — прежний путь.
            let usable = sector > 0 && BLOCK % sector == 0;
            let mut done = 0usize;
            let mut fallback = !usable;
            for &(at, len) in &extents {
                if fallback {
                    break;
                }
                let end = at + len;
                if end.div_ceil(BLOCK) * BLOCK > volume_bytes {
                    fallback = true;
                    break;
                }
                let mut k = at / BLOCK;
                while k * BLOCK < end {
                    let lo = (k * BLOCK).max(at);
                    let hi = ((k + 1) * BLOCK).min(end);
                    let lba = base + k * BLOCK / sector.max(1);
                    if volume.device_mut().peek(lba, &mut block) {
                        let into = done + (lo - at) as usize;
                        buf[into..into + (hi - lo) as usize]
                            .copy_from_slice(&block[(lo - k * BLOCK) as usize..(hi - k * BLOCK) as usize]);
                    } else if wanted.try_reserve(1).is_ok() {
                        wanted.push(k);
                    } else {
                        fallback = true;
                        break;
                    }
                    k += 1;
                }
                done += len as usize;
            }
            if fallback {
                let mut cursor = *self.cursor.lock();
                let read = volume.read(&node, offset, buf, &mut cursor).map_err(convert)?;
                *self.cursor.lock() = cursor;
                self.next.store(offset + read as u64, Ordering::Relaxed);
                return Ok(read);
            }
            // Упреждающее чтение: был пропуск или чтение идёт подряд, а
            // следующий блок не в кэше. Курсор — копия: настоящий не должен
            // уйти дальше прочитанного.
            let end_of_read = offset + total;
            if (!wanted.is_empty() || sequential) && end_of_read < node.valid {
                let mut probe = *self.cursor.lock();
                let window = if sequential { READAHEAD } else { READAHEAD_RANDOM.load(Ordering::Relaxed) };
                let want = window.min(node.valid - end_of_read);
                if let Ok((more, _, _)) = volume.plan_read(&node, end_of_read, want, &mut probe) {
                    let mut seen = 0usize;
                    'more: for (at, len) in more {
                        let end = at + len;
                        if end.div_ceil(BLOCK) * BLOCK > volume_bytes {
                            break;
                        }
                        let mut k = at / BLOCK;
                        while k * BLOCK < end {
                            let lba = base + k * BLOCK / sector;
                            if wanted.last() != Some(&k) && ahead.last() != Some(&k) {
                                seen += 1;
                                if volume.device_mut().peek(lba, &mut block) {
                                    // Чтение подряд, а дальше уже в кэше
                                    // (упреждали раньше) — упреждать нечего.
                                    // Первый блок может быть хвостом
                                    // прочитанного — его не считать.
                                    if ahead.is_empty() && wanted.is_empty() && seen > 1 {
                                        break 'more;
                                    }
                                } else if ahead.try_reserve(1).is_ok() {
                                    ahead.push(k);
                                } else {
                                    break 'more;
                                }
                            }
                            k += 1;
                        }
                    }
                }
            }
            if wanted.is_empty() && ahead.is_empty() {
                buf[from_disk as usize..total as usize].fill(0);
                self.next.store(end_of_read, Ordering::Relaxed);
                return Ok(total as usize);
            }
            let dev = volume.device_mut();
            let all_clean = wanted
                .iter()
                .chain(ahead.iter())
                .all(|k| dev.clean_on_media(base + k * BLOCK / sector, BLOCK as usize));
            let since = dev.generation();
            if !all_clean {
                let mut cursor = *self.cursor.lock();
                let read = volume.read(&node, offset, buf, &mut cursor).map_err(convert)?;
                *self.cursor.lock() = cursor;
                self.next.store(offset + read as u64, Ordering::Relaxed);
                return Ok(read);
            }
            (extents, from_disk, total, since)
        };
        // С носителя без замка тома: подряд идущие блоки — одним запросом.
        let mut blocks: Vec<u64> = Vec::new();
        blocks.try_reserve_exact(wanted.len() + ahead.len()).map_err(|_| VfsError::OutOfMemory)?;
        blocks.extend_from_slice(&wanted);
        blocks.extend_from_slice(&ahead);
        blocks.sort_unstable();
        blocks.dedup();
        let mut runs: Vec<(u64, usize)> = Vec::new();
        for &k in &blocks {
            match runs.last_mut() {
                Some((first, count)) if *first + *count as u64 == k && (*count + 1) * BLOCK as usize <= PIECE => *count += 1,
                _ => runs.push((k, 1)),
            }
        }
        let mut data: Vec<u8> = Vec::new();
        data.try_reserve_exact(blocks.len() * BLOCK as usize).map_err(|_| VfsError::OutOfMemory)?;
        data.resize(blocks.len() * BLOCK as usize, 0);
        let mut lane = fs.lane.clone();
        let mut at = 0usize;
        MISSES.fetch_add(1, Ordering::Relaxed);
        MISS_BYTES.fetch_add(data.len() as u64, Ordering::Relaxed);
        for &(first, count) in &runs {
            let bytes = count * BLOCK as usize;
            lane.read(base + first * BLOCK / sector, &mut data[at..at + bytes]).map_err(|_| VfsError::Io)?;
            at += bytes;
        }
        let mut volume = fs.volume.lock();
        let stale = {
            let dev = volume.device_mut();
            runs.iter()
                .any(|&(first, count)| dev.written_since(since, base + first * BLOCK / sector, count as u64 * BLOCK / sector))
        };
        if stale {
            // Писали в эти секторы, пока читали: кусок — обычным путём.
            let node = self.fresh(&mut volume)?;
            let mut cursor = *self.cursor.lock();
            let read = volume.read(&node, offset, buf, &mut cursor).map_err(convert)?;
            *self.cursor.lock() = cursor;
            self.next.store(offset + read as u64, Ordering::Relaxed);
            return Ok(read);
        }
        let dev = volume.device_mut();
        let mut at = 0usize;
        for &(first, count) in &runs {
            let bytes = count * BLOCK as usize;
            dev.adopt(base + first * BLOCK / sector, &data[at..at + bytes]);
            at += bytes;
        }
        drop(volume);
        // Пропуски — в `buf` из прочитанного.
        let block_data = |k: u64| -> Option<&[u8]> {
            let index = blocks.binary_search(&k).ok()?;
            Some(&data[index * BLOCK as usize..(index + 1) * BLOCK as usize])
        };
        let mut done = 0usize;
        for &(at, len) in &extents {
            let end = at + len;
            let mut k = at / BLOCK;
            while k * BLOCK < end {
                if let Some(bytes) = block_data(k) {
                    let lo = (k * BLOCK).max(at);
                    let hi = ((k + 1) * BLOCK).min(end);
                    let into = done + (lo - at) as usize;
                    buf[into..into + (hi - lo) as usize]
                        .copy_from_slice(&bytes[(lo - k * BLOCK) as usize..(hi - k * BLOCK) as usize]);
                }
                k += 1;
            }
            done += len as usize;
        }
        buf[from_disk as usize..total as usize].fill(0);
        self.next.store(offset + total, Ordering::Relaxed);
        Ok(total as usize)
    }
}

fn metadata_of(node: &exfat::Node) -> Metadata {
    let kind = if node.is_dir() { NodeKind::Directory } else { NodeKind::File };
    Metadata { kind, size: node.size(), mode: MODE, uid: 0, gid: 0, mtime: node.modified.min(u64::from(u32::MAX)) as u32 }
}

impl Node for ExfatNode {
    fn metadata(&self) -> Metadata {
        match self.fresh_unlocked() {
            Ok(node) => metadata_of(&node),
            Err(_) => metadata_of(&self.snapshot()),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if !fast() {
            // Как до 21.11: всё под замком тома, данные — с носителя.
            let (base, sector, _) = self.fs.geometry;
            let mut volume = self.fs.volume.lock();
            let node = self.fresh(&mut volume)?;
            let mut cursor = *self.cursor.lock();
            let (extents, from_disk, total) =
                volume.plan_read(&node, offset, buf.len() as u64, &mut cursor).map_err(convert)?;
            *self.cursor.lock() = cursor;
            let mut lane = self.fs.lane.clone();
            let mut done = 0usize;
            for (at, len) in extents {
                read_direct(&mut lane, base, sector.max(1), at, &mut buf[done..done + len as usize])?;
                done += len as usize;
            }
            buf[from_disk as usize..total as usize].fill(0);
            return Ok(total as usize);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let take = (buf.len() - done).min(PIECE);
            let started = crate::time::uptime_ns();
            let misses = MISSES.load(Ordering::Relaxed);
            let read = self.read_piece(offset + done as u64, &mut buf[done..done + take]);
            let took = crate::time::uptime_ns().saturating_sub(started);
            READS.fetch_add(1, Ordering::Relaxed);
            READ_NS.fetch_add(took, Ordering::Relaxed);
            if MISSES.load(Ordering::Relaxed) != misses {
                MISS_NS.fetch_add(took, Ordering::Relaxed);
            }
            let read = read?;
            done += read;
            if read < take {
                break;
            }
        }
        Ok(done)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let (_, listing) = self.listing()?;
        Ok(listing
            .nodes
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
        let generation = self.fs.generation();
        let (node, listing) = self.listing()?;
        if name == "." {
            return Ok(self.child(node));
        }
        let index = *listing.by_name.get(&self.fs.key(name)).ok_or(VfsError::NotFound)?;
        Ok(Box::new(ExfatNode::new(Arc::clone(&self.fs), listing.nodes[index].clone(), generation)))
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        let mut volume = self.fs.volume.lock();
        let result = volume.write(&self.snapshot(), offset, data, now()).map_err(convert);
        self.fs.changed();
        let node = result?;
        self.store(node, self.fs.generation());
        // Цепочка могла смениться (непрерывный файл стал цепочкой FAT).
        *self.cursor.lock() = Cursor::default();
        Ok(data.len())
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        let mut volume = self.fs.volume.lock();
        let result = volume.truncate(&self.snapshot(), size, now()).map_err(convert);
        self.fs.changed();
        let node = result?;
        self.store(node, self.fs.generation());
        *self.cursor.lock() = Cursor::default();
        Ok(())
    }

    /// Прав exFAT не хранит — принять молча, как `vfat` у Linux.
    fn chmod(&self, _mode: u16) -> VfsResult<()> {
        Ok(())
    }

    fn create(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let mut volume = self.fs.volume.lock();
        let result = volume.create(&self.snapshot(), name, false, now()).map_err(convert);
        self.fs.changed();
        Ok(self.child(result?))
    }

    fn mkdir(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let mut volume = self.fs.volume.lock();
        let result = volume.create(&self.snapshot(), name, true, now()).map_err(convert);
        self.fs.changed();
        Ok(self.child(result?))
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let mut volume = self.fs.volume.lock();
        let result = volume.remove(&self.snapshot(), name, false).map_err(convert);
        self.fs.changed();
        result
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        let mut volume = self.fs.volume.lock();
        let result = volume.remove(&self.snapshot(), name, true).map_err(convert);
        self.fs.changed();
        result
    }

    fn file_id(&self) -> Option<(usize, u64)> {
        Some((Arc::as_ptr(&self.fs) as *const () as usize, self.snapshot().id()))
    }
}
