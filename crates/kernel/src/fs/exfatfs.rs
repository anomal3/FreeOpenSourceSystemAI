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
/// Asynchronous read-ahead of a sequential reader, bytes (`exfatra async
/// KIB`, 0 = off). hw-log17: a sequential reader stopped for the disk every
/// [`READAHEAD`] bytes; now, while it reads, the `exfat-ra` task brings the
/// next window into the cache, so the reader finds it there.
pub static READAHEAD_ASYNC: AtomicU64 = AtomicU64::new(2 * 1024 * 1024);
/// One disk read of the `exfat-ra` task: a reader that misses waits behind
/// at most this much of somebody else's read-ahead.
const AHEAD_PIECE: u64 = 256 * 1024;
/// The piece once the reader has caught up with the task and waits for it
/// (it reads faster than the disk): fewer commands and wake-ups, as a
/// synchronous reader with 1 MiB commands (QEMU `exfat-stream`, think 0:
/// 110 MB/s with 256 KiB pieces vs 178 MB/s without read-ahead).
const AHEAD_PIECE_HURRY: u64 = 1024 * 1024;
/// Read-ahead requests waiting for the task, at most.
const AHEAD_QUEUE: usize = 16;
/// Sequential streams remembered per open file (threads reading one file
/// through a shared descriptor interleave their streams).
const STREAMS: usize = 4;
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

/// Asynchronous read-ahead: requests, bytes the task read from the disk,
/// times a reader waited for it and their ns.
static AHEAD_REQUESTS: AtomicU64 = AtomicU64::new(0);
static AHEAD_BYTES: AtomicU64 = AtomicU64::new(0);
static AHEAD_WAITS: AtomicU64 = AtomicU64::new(0);
static AHEAD_WAIT_NS: AtomicU64 = AtomicU64::new(0);

/// `(read-ahead requests, bytes read by the task, reader waits, their ns)`.
#[must_use]
pub fn ahead_stats() -> (u64, u64, u64, u64) {
    (
        AHEAD_REQUESTS.load(Ordering::Relaxed),
        AHEAD_BYTES.load(Ordering::Relaxed),
        AHEAD_WAITS.load(Ordering::Relaxed),
        AHEAD_WAIT_NS.load(Ordering::Relaxed),
    )
}

/// perf-diag (hw-log17): USB disk commands by their data stage (the
/// transfer count of the "disk" line includes CBW/CSW) and the asynchronous
/// read-ahead since the previous call; `None` - nothing happened.
pub fn perf_line(secs: u64) -> Option<String> {
    static LAST: SpinLock<[u64; 7]> = SpinLock::new([0; 7]);
    let (commands, command_ns) = crate::usb::xhci::storage_data_stats();
    let (_, _, usb_bytes, _) = crate::usb::xhci::storage_stats();
    let (requests, bytes, waits, wait_ns) = ahead_stats();
    let now = [commands, command_ns, requests, bytes, waits, wait_ns, usb_bytes];
    let d: [u64; 7] = {
        let mut last = LAST.lock();
        let d = core::array::from_fn(|i| now[i].saturating_sub(last[i]));
        *last = now;
        d
    };
    if d[0] == 0 && d[2] == 0 {
        return None;
    }
    Some(alloc::format!(
        "{secs} s disk commands: {} usb data stages ({} us, {} KiB each, cap {} KiB); async read-ahead {} requests, {} KiB read, readers waited {} times ({} us each), exfatra async {} KiB",
        d[0],
        d[1] / d[0].max(1) / 1000,
        d[6] / d[0].max(1) / 1024,
        crate::usb::storage::WINDOW_CAP.load(Ordering::Relaxed) / 1024,
        d[2],
        d[3] / 1024,
        d[4],
        d[5] / d[4].max(1) / 1000,
        READAHEAD_ASYNC.load(Ordering::Relaxed) / 1024,
    ))
}

/// Mounted volumes (for [`drop_caches`]).
static VOLUMES: SpinLock<Vec<alloc::sync::Weak<Inner>>> = SpinLock::new(Vec::new());

/// Forget the clean cached blocks of every exFAT volume (`exfatcache drop`:
/// a cold read again for a measurement). Returns how many volumes.
pub fn drop_caches() -> usize {
    let volumes: Vec<Arc<Inner>> = VOLUMES.lock().iter().filter_map(alloc::sync::Weak::upgrade).collect();
    for inner in &volumes {
        inner.volume.lock().device_mut().drop_clean();
    }
    volumes.len()
}

/// Read-ahead state of an open file, shared with the `exfat-ra` task.
#[derive(Default)]
struct Ahead {
    /// A request of this file waits in the queue.
    queued: AtomicBool,
    /// The task works on a request of this file: `[from, to)`, read up to
    /// `done`.
    running: AtomicBool,
    /// A reader waited for the running request: larger pieces.
    hurry: AtomicBool,
    /// Times a reader waited for the task.
    waits: AtomicU64,
    from: AtomicU64,
    to: AtomicU64,
    done: AtomicU64,
}

impl Ahead {
    fn key(&self) -> usize {
        core::ptr::from_ref(self) as usize
    }
}

/// One request: bring `[from, to)` of `node` into the cache.
struct AheadRequest {
    fs: Arc<Inner>,
    node: exfat::Node,
    generation: u64,
    from: u64,
    to: u64,
    ahead: Arc<Ahead>,
}

static AHEAD: SpinLock<alloc::collections::VecDeque<AheadRequest>> =
    SpinLock::new(alloc::collections::VecDeque::new());
static AHEAD_TASK: AtomicBool = AtomicBool::new(false);
static AHEAD_KEY: u8 = 0;

fn ahead_key() -> usize {
    core::ptr::addr_of!(AHEAD_KEY) as usize
}

/// Queue a request; `false` - the queue is full or the task did not start.
fn ahead_push(request: AheadRequest) -> bool {
    if !AHEAD_TASK.swap(true, Ordering::AcqRel) {
        if crate::sched::spawn_daemon("exfat-ra", ahead_task).is_err() {
            AHEAD_TASK.store(false, Ordering::Release);
            return false;
        }
    }
    {
        let mut queue = AHEAD.lock();
        if queue.len() >= AHEAD_QUEUE || queue.try_reserve(1).is_err() {
            return false;
        }
        queue.push_back(request);
    }
    crate::sched::wake_futex(ahead_key(), 0);
    true
}

/// The `exfat-ra` task: requests one by one, each in [`AHEAD_PIECE`]s.
fn ahead_task() {
    loop {
        let next = AHEAD.lock().pop_front();
        let Some(request) = next else {
            crate::sched::block_on_futex(ahead_key(), Some(crate::time::uptime_ms() + 1000), || {
                !AHEAD.lock().is_empty()
            });
            continue;
        };
        let AheadRequest { fs, node, generation, from, to, ahead } = request;
        ahead.from.store(from, Ordering::Relaxed);
        ahead.to.store(to, Ordering::Relaxed);
        ahead.done.store(from, Ordering::Relaxed);
        // `hurry` stays from the previous request of the file while its
        // reader keeps catching up; a request nobody waited for clears it.
        let waits = ahead.waits.load(Ordering::Relaxed);
        ahead.running.store(true, Ordering::Release);
        ahead.queued.store(false, Ordering::Release);
        let mut cursor = Cursor::default();
        let mut at = from;
        while at < to {
            let piece = if ahead.hurry.load(Ordering::Relaxed) { AHEAD_PIECE_HURRY } else { AHEAD_PIECE };
            let take = (to - at).min(piece);
            if fetch(&fs, &node, generation, at, take, &mut cursor).is_err() {
                break;
            }
            at += take;
            ahead.done.store(at, Ordering::Release);
            crate::sched::wake_futex(ahead.key(), 0);
        }
        if ahead.waits.load(Ordering::Relaxed) == waits {
            ahead.hurry.store(false, Ordering::Relaxed);
        }
        ahead.running.store(false, Ordering::Release);
        crate::sched::wake_futex(ahead.key(), 0);
    }
}

/// Bring `[at, at + len)` of `node` into the block cache (the task's read):
/// the plan and the cache lookup under the volume lock, the disk read
/// without it, the blocks adopted only if nobody wrote there meanwhile.
fn fetch(fs: &Inner, node: &exfat::Node, generation: u64, at: u64, len: u64, cursor: &mut Cursor) -> VfsResult<()> {
    let (base, sector, volume_bytes) = fs.geometry;
    if sector == 0 || BLOCK % sector != 0 {
        return Err(VfsError::Unsupported);
    }
    let mut blocks: Vec<u64> = Vec::new();
    let since = {
        let mut volume = fs.volume.lock();
        if fs.generation() != generation || !fast() {
            return Err(VfsError::Io);
        }
        let (extents, _, _) = volume.plan_read(node, at, len, cursor).map_err(convert)?;
        let dev = volume.device_mut();
        for (start, length) in extents {
            let end = start + length;
            if end.div_ceil(BLOCK) * BLOCK > volume_bytes {
                break;
            }
            let mut k = start / BLOCK;
            while k * BLOCK < end {
                if blocks.last() != Some(&k) && !dev.holds(base + k * BLOCK / sector) {
                    blocks.try_reserve(1).map_err(|_| VfsError::OutOfMemory)?;
                    blocks.push(k);
                }
                k += 1;
            }
        }
        if blocks.is_empty() {
            return Ok(());
        }
        if !blocks.iter().all(|k| dev.clean_on_media(base + k * BLOCK / sector, BLOCK as usize)) {
            return Err(VfsError::Io);
        }
        dev.generation()
    };
    blocks.sort_unstable();
    blocks.dedup();
    let mut runs: Vec<(u64, usize)> = Vec::new();
    for &k in &blocks {
        match runs.last_mut() {
            Some((first, count)) if *first + *count as u64 == k => *count += 1,
            _ => runs.push((k, 1)),
        }
    }
    let mut data: Vec<u8> = Vec::new();
    data.try_reserve_exact(blocks.len() * BLOCK as usize).map_err(|_| VfsError::OutOfMemory)?;
    data.resize(blocks.len() * BLOCK as usize, 0);
    let mut lane = fs.lane.clone();
    let mut offset = 0usize;
    for &(first, count) in &runs {
        let bytes = count * BLOCK as usize;
        lane.read(base + first * BLOCK / sector, &mut data[offset..offset + bytes]).map_err(|_| VfsError::Io)?;
        offset += bytes;
    }
    AHEAD_BYTES.fetch_add(data.len() as u64, Ordering::Relaxed);
    let mut volume = fs.volume.lock();
    let dev = volume.device_mut();
    if runs
        .iter()
        .any(|&(first, count)| dev.written_since(since, base + first * BLOCK / sector, count as u64 * BLOCK / sector))
    {
        return Err(VfsError::Io);
    }
    let mut offset = 0usize;
    for &(first, count) in &runs {
        let bytes = count * BLOCK as usize;
        dev.adopt(base + first * BLOCK / sector, &data[offset..offset + bytes]);
        offset += bytes;
    }
    Ok(())
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
        let inner = Arc::new(Inner {
            volume: Mutex::new(volume),
            lane,
            geometry,
            upcase,
            generation: AtomicU64::new(0),
            dirs: SpinLock::new(DirCache { generation: 0, nodes: 0, dirs: BTreeMap::new() }),
        });
        {
            let mut volumes = VOLUMES.lock();
            volumes.retain(|weak| weak.strong_count() > 0);
            if volumes.try_reserve(1).is_ok() {
                volumes.push(Arc::downgrade(&inner));
            }
        }
        Ok(Self(inner))
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
    /// Где кончились прошлые чтения (для упреждающего): до [`STREAMS`]
    /// потоков `(next, asked)` - where a stream's last read ended and up to
    /// where asynchronous read-ahead was asked for it.
    streams: SpinLock<([(u64, u64); STREAMS], usize)>,
    ahead: Arc<Ahead>,
}

impl ExfatNode {
    fn new(fs: Arc<Inner>, node: exfat::Node, generation: u64) -> Self {
        Self {
            fs,
            node: SpinLock::new((node, generation)),
            cursor: SpinLock::new(Cursor::default()),
            streams: SpinLock::new(([(u64::MAX, 0); STREAMS], 0)),
            ahead: Arc::new(Ahead::default()),
        }
    }

    /// Does a read at `offset` continue one of the streams?
    fn sequential(&self, offset: u64) -> bool {
        self.streams.lock().0.iter().any(|(next, _)| *next == offset)
    }

    /// A read `[from, to)` is done: its stream moves on (or a new one
    /// replaces the oldest).
    fn advance(&self, from: u64, to: u64) {
        let mut guard = self.streams.lock();
        let (streams, victim) = &mut *guard;
        if let Some(stream) = streams.iter_mut().find(|(next, _)| *next == from) {
            stream.0 = to;
            return;
        }
        streams[*victim] = (to, 0);
        *victim = (*victim + 1) % STREAMS;
    }

    /// After a sequential read that ended at `end`: ask the `exfat-ra` task
    /// for the next [`READAHEAD_ASYNC`] bytes unless at least half of them
    /// is already asked for.
    fn schedule_ahead(&self, end: u64) {
        let window = READAHEAD_ASYNC.load(Ordering::Relaxed);
        if window == 0 || !fast() || self.ahead.queued.load(Ordering::Acquire) {
            return;
        }
        let asked = {
            let guard = self.streams.lock();
            match guard.0.iter().find(|(next, _)| *next == end) {
                Some((_, asked)) => *asked,
                None => return,
            }
        };
        if asked >= end + window / 2 {
            return;
        }
        let Some(node) = self.current() else {
            return;
        };
        let to = (end + window).min(node.valid);
        let from = asked.max(end);
        if node.is_dir() || from >= to {
            return;
        }
        self.ahead.queued.store(true, Ordering::Release);
        let request = AheadRequest {
            fs: Arc::clone(&self.fs),
            node,
            generation: self.fs.generation(),
            from,
            to,
            ahead: Arc::clone(&self.ahead),
        };
        if ahead_push(request) {
            AHEAD_REQUESTS.fetch_add(1, Ordering::Relaxed);
            if let Some(stream) = self.streams.lock().0.iter_mut().find(|(next, _)| *next == end) {
                stream.1 = to;
            }
        } else {
            self.ahead.queued.store(false, Ordering::Release);
        }
    }

    /// The task is reading `[offset, offset + len)` of this file right now:
    /// wait for it (up to 200 ms) instead of reading the same blocks twice.
    fn wait_ahead(&self, offset: u64, len: u64) {
        let ahead = &*self.ahead;
        let need = |ahead: &Ahead| {
            if !ahead.running.load(Ordering::Acquire) {
                return false;
            }
            let (from, to) = (ahead.from.load(Ordering::Relaxed), ahead.to.load(Ordering::Relaxed));
            offset >= from && offset < to && ahead.done.load(Ordering::Acquire) < (offset + len).min(to)
        };
        if !need(ahead) {
            return;
        }
        ahead.hurry.store(true, Ordering::Relaxed);
        ahead.waits.fetch_add(1, Ordering::Relaxed);
        let started = crate::time::uptime_ns();
        let until = crate::time::uptime_ms() + 200;
        while need(ahead) && crate::time::uptime_ms() < until {
            crate::sched::block_on_futex(ahead.key(), Some(until), || !need(ahead));
        }
        AHEAD_WAITS.fetch_add(1, Ordering::Relaxed);
        AHEAD_WAIT_NS.fetch_add(crate::time::uptime_ns().saturating_sub(started), Ordering::Relaxed);
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
        let sequential = self.sequential(offset);
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
                self.advance(offset, offset + read as u64);
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
                self.advance(offset, end_of_read);
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
                self.advance(offset, offset + read as u64);
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
            self.advance(offset, offset + read as u64);
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
        self.advance(offset, offset + total);
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
            let at = offset + done as u64;
            let started = crate::time::uptime_ns();
            self.wait_ahead(at, take as u64);
            let sequential = self.sequential(at);
            let misses = MISSES.load(Ordering::Relaxed);
            let read = self.read_piece(at, &mut buf[done..done + take]);
            let took = crate::time::uptime_ns().saturating_sub(started);
            READS.fetch_add(1, Ordering::Relaxed);
            READ_NS.fetch_add(took, Ordering::Relaxed);
            if MISSES.load(Ordering::Relaxed) != misses {
                MISS_NS.fetch_add(took, Ordering::Relaxed);
            }
            let read = read?;
            if sequential && read > 0 {
                self.schedule_ahead(at + read as u64);
            }
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

    fn times(&self) -> crate::vfs::Times {
        let node = self.fresh_unlocked().unwrap_or_else(|_| self.snapshot());
        let clip = |seconds: u64| seconds.min(u64::from(u32::MAX)) as u32;
        // exFAT keeps creation, not status change: `ctime` carries creation,
        // as Windows' «Дата создания».
        crate::vfs::Times { mtime: clip(node.modified), atime: clip(node.accessed), ctime: clip(node.created) }
    }

    fn set_times(&self, atime: Option<u32>, mtime: Option<u32>) -> VfsResult<()> {
        let mut volume = self.fs.volume.lock();
        let result = volume.set_times(&self.snapshot(), atime.map(u64::from), mtime.map(u64::from)).map_err(convert);
        self.fs.changed();
        let node = result?;
        self.store(node, self.fs.generation());
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
