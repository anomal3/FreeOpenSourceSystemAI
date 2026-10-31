//! Том btrfs за интерфейсом [`crate::vfs`].
//!
//! Переходник, и только: разбор формата целиком в крейте `btrfs`, который
//! проверяется на хосте образом от `mkfs.btrfs`. Устроен по образцу
//! [`super::ext2fs`] — та же схема с замком, тем же `Counted` и теми же
//! обёртками, — и это сделано намеренно: два драйвера ФС, написанные
//! по-разному, расходятся в мелочах, которые потом объясняются как «на btrfs
//! почему-то иначе».
//!
//! # Отложенная фиксация (фаза 74, 30.10)
//!
//! До 30.10 каждое изменение фиксировалось до возврата из вызова: читатель
//! ([`btrfs::Btrfs`]) видит только диск, а второй путь чтения казался лишним
//! (фаза 50d). Для `/data`, куда пишет человек, так было терпимо; для дома и
//! `/opt` с Rider и Debian — нет: копирование кусками по 4 КиБ было
//! транзакцией на кусок с записью суперблока.
//!
//! Теперь правки копятся в памяти писателя, а фиксация — служебной задачей
//! раз в 5 с ([`FileSystem::writeback`], как сброс ext2), по `sync`/`fsync`,
//! при выключении, после правок родных программ и каждые [`MAX_PENDING`]
//! операций. Пока писатель открыт, **читают его**: inode, каталоги и
//! экстенты — из его памяти, данные — с носителя со сверкой сумм. Читатель с
//! диска остаётся тому только на чтение и проверке.
//!
//! Цена: внезапное выключение теряет правки последних секунд, как у Linux, —
//! но том остаётся прежним целым, а не испорченным: на диске всегда последняя
//! завершённая транзакция. И операция, сорвавшаяся на середине (кончилось
//! место, отказал носитель), теряет вместе с собой всю незафиксированную
//! пачку: память писателя больше не согласована, и он открывается с диска.
//!
//! # Читатель после фиксации открывается заново
//!
//! Не из осторожности. Кэш узлов читателя хранит их по логическому адресу, а
//! фиксация освобождает прежние узлы, и следующая вправе положить новый узел
//! ровно туда же — кэш отдал бы старый, минуя все проверки. К тому же читатель
//! отвергает узлы поколения новее своего суперблока, то есть всё записанное
//! после его монтирования. Открыть заново — это суперблок и два дерева, и это
//! дешевле, чем учить кэш забывать.
//!
//! # Сорванная операция
//!
//! Если операция или фиксация отказала на середине, память писателя больше не
//! описывает ничего согласованного, и он открывается заново с диска. На диске
//! при этом лежит последняя завершённая транзакция — ровно то, что осталось бы
//! после пропажи питания, и это не совпадение, а следствие порядка записи.
//!
//! # Когда писать нельзя
//!
//! Том не помечается используемым: у btrfs такого признака нет, его работу
//! делает атомарная смена суперблока. В безопасном режиме писатель не
//! открывается вовсе, а от тома, который понимает не целиком (подтома, журнал
//! недописанной транзакции, RAID), отказывается сам — тогда том монтируется
//! только на чтение, и причина остаётся в [`BtrfsMount::refused`].

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use alloc::collections::BTreeMap;
use alloc::string::String;

use crate::sync::{Mutex, SpinLock};
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};
use disk::BlockDevice as _;

/// Носитель тома: кэш блоков в кадрах памяти поверх счётчика запросов (с
/// 31.10, когда на btrfs переехали `/home` и `/opt`: без кэша каждое чтение
/// файла шло на носитель). Запись — сквозная: копирование при записи пишет
/// только на новые места, а запись выбрасывает из кэша всё, что с ней
/// пересекается, — освобождённый и занятый заново экстент не отдаст старого.
///
/// Устройство — за своим замком ([`super::ext2fs::Lane`]): данные файлов
/// читаются мимо замка тома (31.10, см. [`BtrfsNode::read_piece`]).
type Disk = crate::block::cache::Cached<Lane>;
use super::ext2fs::Lane;

/// Диск вместе с разобранным на нём томом.
struct Inner {
    disk: Disk,
    /// Читатель. Смотрит на последнюю завершённую транзакцию — и только на неё.
    fs: btrfs::Btrfs,
    first_lba: u64,
    /// Писатель — только у тома, открытого на запись.
    ///
    /// `None` значит то же, что у ext2: писать нечем, и забыть проверку в
    /// одном из путей записи нельзя.
    writer: Option<btrfs::Writer>,
    /// Почему писателя нет, хотя его просили.
    refused: Option<VfsError>,
    /// Сколько операций ждут фиксации.
    pending: usize,
    /// Адрес тома — первая половина ключа кэша страниц его файлов
    /// ([`BtrfsNode::key`]).
    volume: usize,
    /// Когда кончилась последняя фиксация (мс от загрузки) — для `fsync`.
    last_commit: u64,
    /// Читатель отстал от фиксаций писателя ([`Inner::fresh_reader`]).
    reader_stale: bool,
    /// Тот же кэш имён, что у [`BtrfsFs`]: отказ фиксации чистит его.
    meta: Arc<SpinLock<Meta>>,
}

/// Сколько имён и inode помнить, прежде чем начать заново.
const META_LIMIT: usize = 20_000;

/// Кэш имён и inode (31.10, как у ext2 с фазы 74): `stat`, `access` и `open`
/// уже виденного не берут замок тома — и не ждут фиксаций. Наполняется и
/// чистится под замком тома, поэтому устаревшее в него не попадает.
#[derive(Default)]
struct Meta {
    /// Каталог → имя → inode; `None` — имени нет (отрицательный ответ: слой
    /// Linux ищет каждый путь сперва внутри корня Debian).
    names: BTreeMap<u64, BTreeMap<String, Option<u64>>>,
    inodes: BTreeMap<u64, btrfs::Inode>,
    count: usize,
}

/// Что правка сделала устаревшим в кэше имён.
enum Touched<'a> {
    Inode(u64),
    Entry(u64, &'a str),
    All,
}

impl Meta {
    fn clear(&mut self) {
        *self = Self::default();
    }

    fn forget(&mut self, touched: &[Touched<'_>]) {
        for item in touched {
            match item {
                Touched::Inode(number) => {
                    self.inodes.remove(number);
                }
                Touched::Entry(dir, name) => {
                    if let Some(names) = self.names.get_mut(dir) {
                        names.remove(*name);
                    }
                }
                Touched::All => self.clear(),
            }
        }
    }

    fn room(&mut self) {
        self.count += 1;
        if self.count > META_LIMIT || (self.count % 256 == 0 && !crate::block::cache::heap_roomy()) {
            self.clear();
        }
    }

    fn keep_inode(&mut self, inode: &btrfs::Inode) {
        self.room();
        self.inodes.insert(inode.number, inode.clone());
    }

    fn keep_name(&mut self, dir: u64, name: &str, number: Option<u64>) {
        self.room();
        self.names.entry(dir).or_default().insert(String::from(name), number);
    }
}

/// Чаще этого `fsync` программы фиксацию не вызывает (мс): остальное
/// фиксирует служебная задача раз в 5 с. Замер 31.10 (Rider в VirtualBox):
/// 305 фиксаций по 30–1300 мс за запуск, почти все — от `fdatasync`.
const FSYNC_INTERVAL_MS: u64 = 2_000;

/// С какого числа грязных блоков (по 4 КиБ, 16 МиБ) запись сама сбрасывает
/// кэш порциями после правки.
const EARLY_WRITE_BACK: usize = 4096;

/// Больше скольких незафиксированных операций не копить.
const MAX_PENDING: usize = 4096;

/// Смонтированный том.
pub struct BtrfsFs {
    inner: Mutex<Inner>,
    /// Кэш имён и inode — мимо замка тома ([`Meta`]).
    meta: Arc<SpinLock<Meta>>,
    /// То же устройство, что под кэшем, — для чтения мимо замка тома.
    lane: Lane,
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
        btrfs::Error::Exists => VfsError::Exists,
        btrfs::Error::NotEmpty => VfsError::NotEmpty,
        btrfs::Error::NoSpace => VfsError::NoSpace,
        // Транзакция, сорванная прежним отказом носителя или памяти. Наружу
        // почти не выходит — переходник открывает писатель заново, — а когда
        // выходит, ближайшее честное — «устройство отказало»: причина была там.
        btrfs::Error::Aborted => VfsError::Io,
        btrfs::Error::Corrupt | btrfs::Error::BadChecksum | btrfs::Error::TooSmall => {
            VfsError::Corrupt
        }
    }
}

/// Разбить путь на каталог и последнее имя.
///
/// Тот же, что в [`super::ext2fs`], и свой по той же причине: это частный
/// разбор внутри реализации файловой системы, а не общий интерфейс.
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
    ///
    /// `writable` — просьба, а не обещание: писатель отказывается от тома, в
    /// котором понимает не всё, и тогда том открывается только на чтение.
    /// Отказать в монтировании из-за этого было бы хуже — данные на таком томе
    /// читаются, и прятать их незачем.
    pub fn mount(
        device: Box<dyn disk::BlockDevice + Send>,
        first_lba: u64,
        writable: bool,
    ) -> VfsResult<BtrfsMount> {
        let lane = Lane::new(device);
        // Отложенная запись, как у ext2: данные ложатся только на свободные
        // места, а фиксация сбрасывает кэш до суперблока и после него
        // (`Writer::commit`), — порядок записей тот же. Без неё запись файла
        // ждала диска под замком тома, и `stat`/`mkdir` соседей стояли
        // (Rider 31.10: `mkdir` 12 мс, `access` 5 мс).
        let mut disk = Disk::deferred(lane.clone());
        let fs = btrfs::Btrfs::mount(&mut disk, first_lba).map_err(convert)?;
        // Открытие писателя ничего не пишет: оно читает метаданные тома в
        // память. Том, от которого писатель отказался, остаётся нетронутым.
        let (writer, refused) = if writable {
            match btrfs::Writer::open(&mut disk, first_lba) {
                Ok(writer) => (Some(writer), None),
                Err(err) => (None, Some(convert(err))),
            }
        } else {
            (None, None)
        };
        let meta = Arc::new(SpinLock::new(Meta::default()));
        let mounted = Arc::new(Self {
            inner: Mutex::new(Inner {
                disk,
                fs,
                first_lba,
                writer,
                refused,
                pending: 0,
                volume: 0,
                last_commit: 0,
                reader_stale: false,
                meta: Arc::clone(&meta),
            }),
            meta,
            lane,
        });
        mounted.inner.lock().volume = Arc::as_ptr(&mounted) as *const () as usize;
        Ok(BtrfsMount(mounted))
    }

    /// Выполнить изменение тома; фиксация — позже ([`Inner::commit`]).
    /// `touched` — что правка делает устаревшим в кэше имён.
    fn change<R>(
        &self,
        touched: &[Touched<'_>],
        action: impl FnOnce(&mut Disk, &mut btrfs::Writer, u64) -> Result<R, btrfs::Error>,
    ) -> VfsResult<R> {
        let asked = crate::time::uptime_ms();
        let mut guard = self.inner.lock();
        let got = crate::time::uptime_ms();
        let Inner { disk, writer, .. } = &mut *guard;
        let Some(active) = writer.as_mut() else {
            return Err(VfsError::ReadOnly);
        };
        // Часы спрашиваются на каждую правку — по той же причине, что у ext2.
        // Часов нет — ноль, а не выдуманная дата.
        let time = crate::time::now_unix().unwrap_or(0);
        let outcome = action(disk, active, time);
        // Замер 31.10: долгая правка держит замок тома для всех.
        let held = crate::time::uptime_ms().saturating_sub(got);
        let waited = got.saturating_sub(asked);
        if held >= 20 || waited >= 100 {
            static SAID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
            if SAID.fetch_add(1, core::sync::atomic::Ordering::Relaxed) < 50 {
                let what = touched.iter().find_map(|item| match item {
                    Touched::Entry(_, name) => Some(*name),
                    _ => None,
                });
                crate::kprintln!(
                    "  btrfs       : op latency held {held} ms, waited {waited} ms, entry {:?}, pending {}",
                    what,
                    guard.pending
                );
            }
        }
        // Кэш имён чистится и при отказе: правка могла успеть наполовину.
        self.meta.lock().forget(touched);
        guard.pending += 1;
        // Грязного много — сбросить порциями уже без замка, не дожидаясь,
        // пока кэш упрётся в свой предел (64 МиБ) и сбросит всё разом посреди
        // чьей-то записи под замком (Rider 01.11: правки по 70–110 мс).
        if outcome.is_ok() && guard.pending < MAX_PENDING && guard.disk.dirty_blocks() >= EARLY_WRITE_BACK {
            drop(guard);
            self.write_back_portions()?;
            return outcome.map_err(convert);
        }

        // Отказ на середине правки — фиксация сразу: она вернёт `Aborted`, и
        // писатель откроется заново. Отказ до первой правки («имя занято» —
        // `mkdir` существующего каталога, а Rider делает это тысячами) память
        // не трогал, и фиксировать из-за него незачем (01.11: каждая такая
        // `mkdir` стоила фиксации тома, 11 мс в среднем).
        let broken = guard.writer.as_ref().is_some_and(btrfs::Writer::is_broken);
        if (outcome.is_err() && broken) || guard.pending >= MAX_PENDING {
            let committed = guard.commit();
            let result = outcome.map_err(convert)?;
            committed?;
            return Ok(result);
        }
        outcome.map_err(convert)
    }

    /// Довести грязные блоки кэша до носителя порциями по 1 МиБ, отпуская
    /// замок тома между ними: данные лежат на свободных местах и годны к
    /// записи в любой момент, а фиксация после этого коротка. Порция меньше,
    /// чем у ext2 (4 МиБ): в VirtualBox соседи ждали за ней замок до 390 мс.
    fn write_back_portions(&self) -> VfsResult<()> {
        const PORTION: usize = 1024 * 1024;
        loop {
            let mut guard = self.inner.lock();
            if !guard.disk.is_dirty() {
                return Ok(());
            }
            let more = guard.disk.write_back_some(PORTION).map_err(|_| VfsError::Io)?;
            drop(guard);
            if !more {
                return Ok(());
            }
            crate::sched::yield_now();
        }
    }

    /// Inode по номеру: из кэша имён, иначе под замком тома.
    fn inode(&self, number: u64) -> VfsResult<btrfs::Inode> {
        if let Some(inode) = self.meta.lock().inodes.get(&number) {
            return Ok(inode.clone());
        }
        let mut guard = self.inner.lock();
        let inode = guard.inode(number)?;
        self.meta.lock().keep_inode(&inode);
        Ok(inode)
    }
}

impl Inner {
    /// Зафиксировать накопленное; после отказа — открыть писатель с диска.
    fn commit(&mut self) -> VfsResult<()> {
        let Inner { disk, fs, first_lba, writer, refused, pending, volume, last_commit, reader_stale, meta } = self;
        let Some(active) = writer.as_mut() else {
            return Ok(());
        };
        let ops = *pending;
        *pending = 0;
        let started = crate::time::uptime_ms();
        let committed = active.commit(disk);
        *last_commit = crate::time::uptime_ms();
        let took = last_commit.saturating_sub(started);
        // Фиксация держит замок тома: всё, что ждёт `stat` и `open`, видно
        // здесь (замер 31.10 — btrfs-состояние медленнее ext2 на метаданных).
        if took >= 100 {
            crate::kprintln!("  btrfs       : commit latency {took} ms, {ops} op(s)");
        }
        let generation = active.generation();
        if committed.is_err() {
            // Страницы, прочитанные из памяти писателя, могли описывать
            // незафиксированное — его больше нет.
            crate::mm::pagecache::forget_volume(*volume);
            match btrfs::Writer::open(disk, *first_lba) {
                Ok(fresh) => *writer = Some(fresh),
                Err(err) => {
                    *writer = None;
                    *refused = Some(convert(err));
                }
            }
        }
        if committed.is_err() {
            meta.lock().clear();
        }
        // Читатель нужен писателю только для статистики и проверки тома —
        // открывается заново лениво ([`Inner::fresh_reader`]), а не на каждой
        // фиксации под замком тома. Без писателя его нет: тогда — сразу.
        if committed.is_ok() && writer.is_some() && generation != fs.generation() {
            *reader_stale = true;
        } else if committed.is_err() || generation != fs.generation() {
            match btrfs::Btrfs::mount(disk, *first_lba) {
                Ok(fresh) => *fs = fresh,
                Err(err) => {
                    // Прежний читатель верен ровно до следующей фиксации:
                    // только она может положить новый узел или данные на место
                    // тех, что видит он. Значит, следующей не будет.
                    *writer = None;
                    *refused = Some(convert(err));
                    return Err(convert(err));
                }
            }
        }
        committed.map_err(convert)
    }

    /// Открыть читатель заново, если с его монтирования были фиксации.
    fn fresh_reader(&mut self) -> VfsResult<()> {
        if !self.reader_stale {
            return Ok(());
        }
        match btrfs::Btrfs::mount(&mut self.disk, self.first_lba) {
            Ok(fresh) => {
                self.fs = fresh;
                self.reader_stale = false;
                Ok(())
            }
            Err(err) => {
                self.writer = None;
                self.refused = Some(convert(err));
                Err(convert(err))
            }
        }
    }

    /// Inode по номеру: у писателя — из его памяти, иначе с диска.
    fn inode(&mut self, number: u64) -> VfsResult<btrfs::Inode> {
        match self.writer.as_mut() {
            Some(writer) => writer.inode(&mut self.disk, number).map_err(convert),
            None => self.fs.inode(&mut self.disk, number).map_err(convert),
        }
    }
}

impl Clone for BtrfsMount {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl BtrfsMount {
    /// Что сказать человеку при монтировании: поколение, размеры, число
    /// кусков и сколько обращений к диску это стоило.
    ///
    /// Счётчик запросов здесь по той же причине, что и у ext2: он единственное
    /// доказательство, что чтение дошло до устройства.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, u64, usize, u64) {
        let mut guard = self.0.inner.lock();
        let _ = guard.fresh_reader();
        let (total, used) = guard.fs.usage();
        (
            guard.fs.generation(),
            total,
            used,
            guard.fs.chunks(),
            self.0.lane.requests(),
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

    /// Можно ли в том писать прямо сейчас.
    #[must_use]
    pub fn writable(&self) -> bool {
        self.0.inner.lock().writer.is_some()
    }

    /// Почему писать нельзя, если писать просили.
    ///
    /// `None` и у тома на запись, и у тома, открытого на чтение по просьбе
    /// (безопасный режим): отказом это не было.
    #[must_use]
    pub fn refused(&self) -> Option<VfsError> {
        self.0.inner.lock().refused
    }

    /// Обойти том целиком и сверить всё, что сверяется.
    ///
    /// Замок держится всю проверку — как и у ext2, и по той же причине: без
    /// него другая задача успела бы записать файл посреди обхода, и «находка»
    /// описывала бы не том, а гонку. Правка берёт тот же замок и ждёт.
    fn summary(&self) -> VfsResult<crate::vfs::CheckSummary> {
        let report = {
            let mut guard = self.0.inner.lock();
            // Проверка обходит диск: сперва на него должно лечь накопленное.
            guard.commit()?;
            guard.fresh_reader()?;
            let Inner { disk, fs, .. } = &mut *guard;
            fs.check(disk).map_err(convert)?
        };

        let mut problems = Vec::new();
        problems
            .try_reserve_exact(report.problems.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for problem in &report.problems {
            problems.push(btrfs::describe(problem).map_err(convert)?);
        }

        Ok(crate::vfs::CheckSummary {
            // Всё, что нашлось, требует решения человека: писатель умеет
            // транзакции, а не починку. Сказать «починится при следующей
            // загрузке» было бы прямым враньём.
            needs_attention: !problems.is_empty() || report.dropped > 0,
            problems,
            dropped: report.dropped,
            // Числа — **по итогам обхода**, а не из счётчиков тома: счётчики
            // проверка как раз и сверяет, и брать их отсюда значило бы
            // отчитаться тем, что проверялось.
            inodes_used: u32::try_from(report.inodes).unwrap_or(u32::MAX),
            blocks_used: u32::try_from(report.sectors).unwrap_or(u32::MAX),
        })
    }
}

impl FileSystem for BtrfsMount {
    fn name(&self) -> &'static str {
        "btrfs"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        let inode = {
            let mut guard = self.0.inner.lock();
            let Inner { disk, writer, fs, .. } = &mut *guard;
            let number = match writer.as_mut() {
                Some(writer) => writer.resolve(disk, "/").map_err(convert)?,
                None => fs.root(disk).map_err(convert)?.number,
            };
            guard.inode(number)?
        };
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.0), inode }))
    }

    fn check(&self) -> Option<VfsResult<crate::vfs::CheckSummary>> {
        Some(self.summary())
    }

    /// Место по последней зафиксированной транзакции: незафиксированное
    /// добавит не больше нескольких секунд правок.
    fn space(&self) -> Option<(u64, u64)> {
        let mut guard = self.0.inner.lock();
        guard.writer.as_ref()?;
        guard.fresh_reader().ok()?;
        let (total, used) = guard.fs.usage();
        Some((total, total.saturating_sub(used)))
    }

    /// Зафиксировать накопленное и довести до носителя.
    ///
    /// `flush` зовётся и после фиксации (она сама его делает): он дёшев, а
    /// «записано» без него значит «лежит в кеше диска». Помечать том чистым не
    /// нужно: такого признака у btrfs нет, его работу делает смена суперблока.
    fn sync(&self) -> VfsResult<()> {
        let mut guard = self.0.inner.lock();
        if guard.writer.is_none() {
            return Ok(());
        }
        guard.commit()?;
        guard.disk.flush().map_err(|_| VfsError::Io)
    }

    /// Фиксация накопленного — служебной задачей раз в 5 с, `fsync` и после
    /// правок родных программ (как сброс у ext2).
    fn writeback(&self) -> VfsResult<()> {
        self.0.write_back_portions()?;
        let mut guard = self.0.inner.lock();
        if guard.pending == 0 {
            return Ok(());
        }
        guard.commit()
    }

    /// `fsync`: фиксация, только если прошлая была дольше
    /// [`FSYNC_INTERVAL_MS`] назад. У Linux btrfs для этого журнал (`log
    /// tree`), у нас его нет; цена — внезапное выключение теряет до пяти
    /// секунд, но порядок записей сохраняется: том — целое прошлое состояние.
    fn fsync(&self) -> VfsResult<()> {
        self.0.write_back_portions()?;
        let mut guard = self.0.inner.lock();
        if guard.pending == 0 || crate::time::uptime_ms().saturating_sub(guard.last_commit) < FSYNC_INTERVAL_MS {
            return Ok(());
        }
        guard.commit()
    }

    /// Переименовать: то же содержимое под другим именем.
    ///
    /// Каталоги ищутся не заранее, как у ext2, а внутри правки: писатель
    /// разбирает путь по своим деревьям в памяти, и второй захват замка не
    /// нужен.
    fn rename(&self, old: &str, new: &str) -> VfsResult<()> {
        let (old_parent, old_name) = split_parent(old)?;
        let (new_parent, new_name) = split_parent(new)?;
        // Переименование поверх существующего файла удаляет тот: его
        // страницы в кэше больше не его.
        if let Ok(target) = self.resolve(new) {
            if let Some(key) = target.cache_key() {
                crate::mm::pagecache::forget(key);
            }
        }
        // Что устарело в кэше имён: оба имени, оба каталога, переносимое и
        // заменяемое. Не нашлось чего-то — забыть всё.
        let numbers = (|| -> VfsResult<(u64, u64, u64)> {
            let from = self.resolve(old_parent)?.file_id().ok_or(VfsError::NotFound)?.1;
            let to = self.resolve(new_parent)?.file_id().ok_or(VfsError::NotFound)?.1;
            let moved = self.resolve(old)?.file_id().ok_or(VfsError::NotFound)?.1;
            Ok((from, to, moved))
        })();
        let replaced = self.resolve(new).ok().and_then(|node| node.file_id()).map(|id| id.1);
        let touched_known;
        let touched: &[Touched<'_>] = match numbers {
            Ok((from, to, moved)) => {
                touched_known = [
                    Touched::Entry(from, old_name),
                    Touched::Entry(to, new_name),
                    Touched::Inode(from),
                    Touched::Inode(to),
                    Touched::Inode(moved),
                    replaced.map_or(Touched::Inode(moved), Touched::Inode),
                ];
                &touched_known
            }
            Err(_) => &[Touched::All],
        };
        self.0.change(touched, |disk, writer, time| {
            let from = writer.resolve(disk, old_parent)?;
            let to = writer.resolve(disk, new_parent)?;
            writer.rename(disk, from, old_name, to, new_name, time)
        })
    }
}

/// Узел тома: файл или каталог.
struct BtrfsNode {
    fs: Arc<BtrfsFs>,
    inode: btrfs::Inode,
}

impl Node for BtrfsNode {
    fn cache_key(&self) -> Option<(usize, u64)> {
        (self.inode.kind == btrfs::FileType::Regular).then(|| self.key())
    }

    fn file_id(&self) -> Option<(usize, u64)> {
        Some(self.key())
    }

    /// Свежий inode, а не снимок открытия: запись через этот или другой
    /// дескриптор меняет длину и время.
    fn metadata(&self) -> Metadata {
        match self.fs.inode(self.inode.number) {
            Ok(inode) => metadata_of(&inode),
            Err(_) => metadata_of(&self.inode),
        }
    }

    /// Через буфер ядра, кусками, как у ext2: под замком тома память программы
    /// не трогается (её страница может отказать, а обработчику нужен замок
    /// процесса, который держит, скажем, `munmap`, ждущий этот же том), и
    /// между кусками замок отпускается — длинное чтение не держит остальных.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let mut bounce = super::ext2fs::bounce_buffer(buf.len())?;
        let mut done = 0;
        while done < buf.len() {
            let take = (buf.len() - done).min(bounce.len());
            let at = offset + done as u64;
            let got = self.read_piece(at, &mut bounce[..take])?;
            buf[done..done + got].copy_from_slice(&bounce[..got]);
            done += got;
            if got < take {
                break;
            }
        }
        Ok(done)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let mut guard = self.fs.inner.lock();
        let Inner { disk, writer, fs, .. } = &mut *guard;
        let entries = match writer.as_mut() {
            Some(writer) => writer.list(disk, self.inode.number).map_err(convert)?,
            None => fs.list(disk, &self.inode).map_err(convert)?,
        };

        let mut out = Vec::new();
        out.try_reserve_exact(entries.len())
            .map_err(|_| VfsError::OutOfMemory)?;
        for entry in entries {
            // Права и размер живут в inode, а не в записи каталога — читать
            // приходится каждый. Та же цена, что у ext2, и по той же причине
            // приемлемая: перечисление вызывает человек, а не горячий путь.
            let node = guard.inode(entry.inode)?;
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
        if self.inode.kind != btrfs::FileType::Directory {
            return Err(VfsError::WrongKind);
        }
        let dir = self.inode.number;
        // Кэш имён — без замка тома.
        let known = self.fs.meta.lock().names.get(&dir).and_then(|names| names.get(name).copied());
        let inode = match known {
            Some(None) => return Err(VfsError::NotFound),
            Some(Some(number)) => self.fs.inode(number)?,
            None => {
                let mut guard = self.fs.inner.lock();
                let Inner { disk, writer, fs, .. } = &mut *guard;
                let number = match writer.as_mut() {
                    Some(writer) => writer.lookup(disk, dir, name).map_err(convert)?,
                    None => fs.lookup(disk, &self.inode, name).map_err(convert)?.map(|entry| entry.inode),
                };
                let Some(number) = number else {
                    self.fs.meta.lock().keep_name(dir, name, None);
                    return Err(VfsError::NotFound);
                };
                let inode = guard.inode(number)?;
                let mut meta = self.fs.meta.lock();
                meta.keep_name(dir, name, Some(number));
                meta.keep_inode(&inode);
                inode
            }
        };
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.fs), inode }))
    }

    /// Через буфер ядра, по той же причине, что и `read_at`.
    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        crate::mm::pagecache::forget(self.key());
        let number = self.inode.number;
        let mut bounce = super::ext2fs::bounce_buffer(data.len())?;
        let mut done = 0;
        while done < data.len() {
            let take = (data.len() - done).min(bounce.len());
            bounce[..take].copy_from_slice(&data[done..done + take]);
            let piece = &bounce[..take];
            let at = offset + done as u64;
            let wrote = self
                .fs
                .change(&[Touched::Inode(number)], |disk, writer, time| writer.write_at(disk, number, at, piece, time))?;
            done += wrote;
            if wrote < take {
                break;
            }
        }
        Ok(done)
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        crate::mm::pagecache::forget(self.key());
        let number = self.inode.number;
        self.fs
            .change(&[Touched::Inode(number)], |disk, writer, time| writer.truncate(disk, number, size, time))
    }

    fn chmod(&self, mode: u16) -> VfsResult<()> {
        let number = self.inode.number;
        self.fs.change(&[Touched::Inode(number)], |disk, writer, time| writer.set_mode(disk, number, mode, time))
    }

    fn create(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let parent = self.inode.number;
        let number = self.fs.change(&[Touched::Entry(parent, name), Touched::Inode(parent)], |disk, writer, time| {
            let attrs = btrfs::Attributes { mode, uid, gid, time };
            writer.create_file(disk, parent, name, &[], &attrs)
        })?;
        self.child(number)
    }

    fn mkdir(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let parent = self.inode.number;
        let number = self.fs.change(&[Touched::Entry(parent, name), Touched::Inode(parent)], |disk, writer, time| {
            let attrs = btrfs::Attributes { mode, uid, gid, time };
            writer.create_directory(disk, parent, name, &attrs)
        })?;
        self.child(number)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let child = self.lookup(name).ok();
        if let Some(key) = child.as_ref().and_then(|child| child.cache_key()) {
            crate::mm::pagecache::forget(key);
        }
        let parent = self.inode.number;
        // Номер удаляемого неизвестен (не нашёлся) — забыть всё.
        let gone = child.and_then(|child| child.file_id()).map_or(Touched::All, |id| Touched::Inode(id.1));
        self.fs.change(&[Touched::Entry(parent, name), Touched::Inode(parent), gone], |disk, writer, time| {
            writer.unlink(disk, parent, name, time)
        })
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        let parent = self.inode.number;
        let gone = self.lookup(name).ok().and_then(|child| child.file_id()).map_or(Touched::All, |id| Touched::Inode(id.1));
        self.fs.change(&[Touched::Entry(parent, name), Touched::Inode(parent), gone], |disk, writer, time| {
            writer.remove_directory(disk, parent, name, time)
        })
    }
}

impl BtrfsNode {
    /// Прочитать кусок файла с `at` в `buf` целиком под замком тома.
    fn read_locked(&self, at: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs, writer, .. } = &mut *guard;
        match writer.as_mut() {
            Some(writer) => writer.read_at(disk, self.inode.number, at, buf),
            None => fs.inode(disk, self.inode.number).and_then(|inode| fs.read_at(disk, &inode, at, buf)),
        }
        .map_err(convert)
    }

    /// Прочитать кусок файла (31.10, как у ext2): под замком тома — только
    /// разметка (экстенты и суммы из памяти писателя) и то, что уже в кэше
    /// блоков; пропуски читаются с устройства **без** замка тома и сверяются
    /// с суммами, взятыми при разметке, — `stat` и `open` соседей не ждут
    /// диска. Сумма не сошлась (экстент освободили и заняли заново) или в эти
    /// секторы за время чтения писали — кусок перечитывается под замком.
    fn read_piece(&self, at: u64, buf: &mut [u8]) -> VfsResult<usize> {
        const BLOCK: usize = 4096;
        // Куски с носителя, которых нет в кэше: (номер куска, данные).
        let mut misses: Vec<(usize, Vec<u8>)> = Vec::new();
        let (pieces, since) = {
            let mut guard = self.fs.inner.lock();
            let Inner { disk, writer, .. } = &mut *guard;
            let Some(writer) = writer.as_mut() else {
                drop(guard);
                return self.read_locked(at, buf);
            };
            let pieces = writer.plan_read(disk, self.inode.number, at, buf.len()).map_err(convert)?;
            let mut done = 0usize;
            for (n, piece) in pieces.iter().enumerate() {
                let len = piece.len();
                match piece {
                    btrfs::ReadPiece::Zero(_) | btrfs::ReadPiece::Bytes(_) => {
                        piece.fill(disk, &mut Vec::new(), &mut buf[done..]).map_err(convert)?;
                    }
                    btrfs::ReadPiece::Disk { lba, bytes, skip, take, .. } => {
                        let mut data = Vec::new();
                        data.try_reserve_exact(*bytes).map_err(|_| VfsError::OutOfMemory)?;
                        data.resize(*bytes, 0);
                        let unit = (BLOCK as u64) / u64::from(disk.sector_size().max(1));
                        let cached = *bytes % BLOCK == 0
                            && data.chunks_exact_mut(BLOCK).enumerate().all(|(k, block)| disk.peek(lba + k as u64 * unit, block));
                        if cached {
                            buf[done..done + take].copy_from_slice(&data[*skip..*skip + *take]);
                        } else {
                            // Отложенная запись: грязное другой формы рядом —
                            // на носителе ещё не то, читать под замком.
                            if *bytes % BLOCK != 0 || !disk.clean_on_media(*lba, *bytes) || misses.try_reserve(1).is_err() {
                                drop(guard);
                                return self.read_locked(at, buf);
                            }
                            misses.push((n, data));
                        }
                    }
                }
                done += len;
            }
            if misses.is_empty() {
                return Ok(done);
            }
            (pieces, disk.generation())
        };

        let mut lane = self.fs.lane.clone();
        for (n, data) in &mut misses {
            let btrfs::ReadPiece::Disk { lba, .. } = &pieces[*n] else { continue };
            if lane.read(*lba, data).is_err() {
                return self.read_locked(at, buf);
            }
        }

        let mut guard = self.fs.inner.lock();
        let sector = u64::from(guard.disk.sector_size().max(1));
        let unit = BLOCK as u64 / sector;
        let stale = misses.iter().any(|(n, data)| match &pieces[*n] {
            btrfs::ReadPiece::Disk { lba, .. } => guard.disk.written_since(since, *lba, data.len() as u64 / sector),
            _ => false,
        });
        if stale {
            drop(guard);
            return self.read_locked(at, buf);
        }
        // Что есть в кэше — последнее слово о своих секторах (отложенная
        // запись: на носителе может быть ещё прежнее); сверка — после.
        let mut block = [0u8; BLOCK];
        for (n, data) in &mut misses {
            let btrfs::ReadPiece::Disk { lba, .. } = &pieces[*n] else { continue };
            for (k, piece) in data.chunks_exact_mut(BLOCK).enumerate() {
                if guard.disk.peek(lba + k as u64 * unit, &mut block) {
                    piece.copy_from_slice(&block);
                }
            }
            if pieces[*n].verify(data).is_err() {
                drop(guard);
                return self.read_locked(at, buf);
            }
        }
        for (n, data) in &misses {
            if let btrfs::ReadPiece::Disk { lba, .. } = &pieces[*n] {
                guard.disk.adopt(*lba, data);
            }
        }
        drop(guard);

        // Сборка: смещение каждого куска в `buf` — сумма длин до него.
        let mut offsets = Vec::new();
        offsets.try_reserve_exact(pieces.len()).map_err(|_| VfsError::OutOfMemory)?;
        let mut done = 0usize;
        for piece in &pieces {
            offsets.push(done);
            done += piece.len();
        }
        for (n, data) in &misses {
            if let btrfs::ReadPiece::Disk { skip, take, .. } = &pieces[*n] {
                let at = offsets[*n];
                buf[at..at + take].copy_from_slice(&data[*skip..*skip + *take]);
            }
        }
        Ok(done)
    }

    /// Том и номер файла — ключ кэша страниц и постоянное имя (`st_ino`).
    fn key(&self) -> (usize, u64) {
        (Arc::as_ptr(&self.fs) as *const () as usize, self.inode.number)
    }

    /// Узел по номеру inode — то, что возвращают создающие операции. Inode —
    /// из памяти писателя: создание ещё не зафиксировано.
    fn child(&self, number: u64) -> VfsResult<Box<dyn Node>> {
        let inode = self.fs.inner.lock().inode(number)?;
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.fs), inode }))
    }
}
