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

use crate::block::Counted;
use crate::sync::Mutex;
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};
use disk::BlockDevice as _;

/// Диск вместе с разобранным на нём томом.
struct Inner {
    disk: Counted,
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
}

/// Больше скольких незафиксированных операций не копить.
const MAX_PENDING: usize = 4096;

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
        let mut disk = Counted::new(device);
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
        Ok(BtrfsMount(Arc::new(Self {
            inner: Mutex::new(Inner { disk, fs, first_lba, writer, refused, pending: 0 }),
        })))
    }

    /// Выполнить изменение тома; фиксация — позже ([`Inner::commit`]).
    fn change<R>(
        &self,
        action: impl FnOnce(&mut Counted, &mut btrfs::Writer, u64) -> Result<R, btrfs::Error>,
    ) -> VfsResult<R> {
        let mut guard = self.inner.lock();
        let Inner { disk, writer, .. } = &mut *guard;
        let Some(active) = writer.as_mut() else {
            return Err(VfsError::ReadOnly);
        };
        // Часы спрашиваются на каждую правку — по той же причине, что у ext2.
        // Часов нет — ноль, а не выдуманная дата.
        let time = crate::time::now_unix().unwrap_or(0);
        let outcome = action(disk, active, time);
        guard.pending += 1;

        // Отказ — фиксация сразу. Отказ до первой правки оставляет память
        // согласованной, и фиксируется накопленное; отказ на середине
        // возвращается из фиксации как `Aborted` — и писатель открывается
        // заново.
        if outcome.is_err() || guard.pending >= MAX_PENDING {
            let committed = guard.commit();
            let result = outcome.map_err(convert)?;
            committed?;
            return Ok(result);
        }
        outcome.map_err(convert)
    }
}

impl Inner {
    /// Зафиксировать накопленное; после отказа — открыть писатель с диска.
    fn commit(&mut self) -> VfsResult<()> {
        let Inner { disk, fs, first_lba, writer, refused, pending } = self;
        let Some(active) = writer.as_mut() else {
            return Ok(());
        };
        *pending = 0;
        let committed = active.commit(disk);
        let generation = active.generation();
        if committed.is_err() {
            match btrfs::Writer::open(disk, *first_lba) {
                Ok(fresh) => *writer = Some(fresh),
                Err(err) => {
                    *writer = None;
                    *refused = Some(convert(err));
                }
            }
        }
        if committed.is_err() || generation != fs.generation() {
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

    /// Inode по номеру: у писателя — из его памяти, иначе с диска.
    fn inode(&mut self, number: u64) -> VfsResult<btrfs::Inode> {
        match self.writer.as_ref() {
            Some(writer) => writer.inode(number).map_err(convert),
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
            let number = match guard.writer.as_ref() {
                Some(writer) => writer.resolve("/").map_err(convert)?,
                None => {
                    let Inner { disk, fs, .. } = &mut *guard;
                    fs.root(disk).map_err(convert)?.number
                }
            };
            guard.inode(number)?
        };
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.0), inode }))
    }

    fn check(&self) -> Option<VfsResult<crate::vfs::CheckSummary>> {
        Some(self.summary())
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
        let mut guard = self.0.inner.lock();
        if guard.pending == 0 {
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
        self.0.change(|_, writer, time| {
            let from = writer.resolve(old_parent)?;
            let to = writer.resolve(new_parent)?;
            writer.rename(from, old_name, to, new_name, time)
        })
    }
}

/// Узел тома: файл или каталог.
struct BtrfsNode {
    fs: Arc<BtrfsFs>,
    inode: btrfs::Inode,
}

impl Node for BtrfsNode {
    /// Свежий inode, а не снимок открытия: запись через этот или другой
    /// дескриптор меняет длину и время.
    fn metadata(&self) -> Metadata {
        match self.fs.inner.lock().inode(self.inode.number) {
            Ok(inode) => metadata_of(&inode),
            Err(_) => metadata_of(&self.inode),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let mut guard = self.fs.inner.lock();
        let Inner { disk, fs, writer, .. } = &mut *guard;
        if let Some(writer) = writer.as_ref() {
            return writer.read_at(disk, self.inode.number, offset, buf).map_err(convert);
        }
        let inode = fs.inode(disk, self.inode.number).map_err(convert)?;
        fs.read_at(disk, &inode, offset, buf).map_err(convert)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let mut guard = self.fs.inner.lock();
        let entries = match guard.writer.as_ref() {
            Some(writer) => writer.list(self.inode.number).map_err(convert)?,
            None => {
                let Inner { disk, fs, .. } = &mut *guard;
                fs.list(disk, &self.inode).map_err(convert)?
            }
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
        let inode = {
            let mut guard = self.fs.inner.lock();
            let number = match guard.writer.as_ref() {
                Some(writer) => {
                    if self.inode.kind != btrfs::FileType::Directory {
                        return Err(VfsError::WrongKind);
                    }
                    writer.lookup(self.inode.number, name).map_err(convert)?
                }
                None => {
                    let Inner { disk, fs, .. } = &mut *guard;
                    fs.lookup(disk, &self.inode, name).map_err(convert)?.map(|entry| entry.inode)
                }
            };
            guard.inode(number.ok_or(VfsError::NotFound)?)?
        };
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.fs), inode }))
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let number = self.inode.number;
        self.fs
            .change(|disk, writer, time| writer.write_at(disk, number, offset, data, time))
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        if self.inode.kind != btrfs::FileType::Regular {
            return Err(VfsError::WrongKind);
        }
        let number = self.inode.number;
        self.fs
            .change(|disk, writer, time| writer.truncate(disk, number, size, time))
    }

    fn create(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let parent = self.inode.number;
        let number = self.fs.change(|disk, writer, time| {
            let attrs = btrfs::Attributes { mode, uid, gid, time };
            writer.create_file(disk, parent, name, &[], &attrs)
        })?;
        self.child(number)
    }

    fn mkdir(&self, name: &str, mode: u16, uid: u32, gid: u32) -> VfsResult<Box<dyn Node>> {
        let parent = self.inode.number;
        let number = self.fs.change(|_, writer, time| {
            let attrs = btrfs::Attributes { mode, uid, gid, time };
            writer.create_directory(parent, name, &attrs)
        })?;
        self.child(number)
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let parent = self.inode.number;
        self.fs.change(|_, writer, time| writer.unlink(parent, name, time))
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        let parent = self.inode.number;
        self.fs
            .change(|_, writer, time| writer.remove_directory(parent, name, time))
    }
}

impl BtrfsNode {
    /// Узел по номеру inode — то, что возвращают создающие операции. Inode —
    /// из памяти писателя: создание ещё не зафиксировано.
    fn child(&self, number: u64) -> VfsResult<Box<dyn Node>> {
        let inode = self.fs.inner.lock().inode(number)?;
        Ok(Box::new(BtrfsNode { fs: Arc::clone(&self.fs), inode }))
    }
}
