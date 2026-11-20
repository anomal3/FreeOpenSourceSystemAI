//! exFAT: чтение и запись (20.11, внешний SSD Романа «Netac MobileDataStar»).
//!
//! # Зачем
//!
//! Большие флешки и внешние диски продаются и форматируются Windows в exFAT;
//! FAT32 не держит файлов больше 4 ГиБ, а игры (Diablo II Resurrected на
//! `F:\D2RLAN`) — это десятки гигабайт. Без exFAT такой диск система просто не
//! видит.
//!
//! # Что здесь есть
//!
//! Всё по спецификации Microsoft (exFAT 1.00): загрузочная область с
//! контрольной суммой, FAT, битовая карта занятости, таблица прописных букв,
//! наборы записей каталога (файл + поток + имена UTF-16) с хешем имени и
//! контрольной суммой набора, непрерывные файлы без цепочки в FAT
//! (`NoFatChain`), «действительная длина» (`ValidDataLength`), метки времени с
//! десятками миллисекунд и смещением от UTC.
//!
//! Запись — по месту и сразу на носитель (без отложенного кэша): съёмный диск
//! выдёргивают без спроса. На время записи том помечается грязным
//! (`VolumeDirty`), [`Volume::clean`] снимает пометку, — так Windows узнаёт,
//! что диск вынули посреди записи, и предлагает проверку.
//!
//! Чего нет: TexFAT (два FAT) — такой том открывается только на чтение.
//!
//! Код без `std`: им пользуется ядро (`fs::exfatfs`), а тесты на хосте
//! читают образ, собранный **чужой** реализацией (`mkfs.exfat` + `exfat-fuse`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use crate::BlockDevice;

/// Атрибуты файла (поле `FileAttributes`).
pub const ATTR_READ_ONLY: u16 = 0x01;
pub const ATTR_HIDDEN: u16 = 0x02;
pub const ATTR_SYSTEM: u16 = 0x04;
pub const ATTR_DIRECTORY: u16 = 0x10;
pub const ATTR_ARCHIVE: u16 = 0x20;

/// Типы записей каталога.
const ENTRY_BITMAP: u8 = 0x81;
const ENTRY_UPCASE: u8 = 0x82;
const ENTRY_LABEL: u8 = 0x83;
const ENTRY_FILE: u8 = 0x85;
const ENTRY_STREAM: u8 = 0xC0;
const ENTRY_NAME: u8 = 0xC1;
const IN_USE: u8 = 0x80;

/// Флаги записи потока.
const FLAG_ALLOCATION_POSSIBLE: u8 = 0x01;
const FLAG_NO_FAT_CHAIN: u8 = 0x02;

/// Флаг тома «грязный» в `VolumeFlags`.
const VOLUME_DIRTY: u16 = 0x0002;

const EOC: u32 = 0xFFFF_FFFF;
const ENTRY: usize = 32;
/// Имя — не длиннее 255 знаков UTF-16; по 15 в записи имени.
const NAME_MAX: usize = 255;
const NAME_PER_ENTRY: usize = 15;

/// Кэш служебных секторов: кусок в 4 КиБ от начала тома.
const UNIT: u64 = 4096;
const CACHE_UNITS: usize = 256;
/// Самый большой кусок прямого чтения или записи данных за одну команду.
const CHUNK: usize = 1 << 20;

/// Что пошло не так.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Отказ носителя.
    Disk(crate::Error),
    /// Это не exFAT.
    NotExfat,
    /// Структуры тома противоречат сами себе.
    Corrupt,
    NotFound,
    Exists,
    NotEmpty,
    NotDir,
    IsDir,
    /// Имя непредставимо: пустое, длиннее 255, с запрещёнными знаками.
    BadName,
    NoSpace,
    ReadOnly,
    /// Каталог больше, чем допускает формат (256 МиБ).
    TooBig,
}

impl From<crate::Error> for Error {
    fn from(err: crate::Error) -> Self {
        Self::Disk(err)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disk(err) => write!(f, "device: {err}"),
            Self::NotExfat => f.write_str("not an exFAT volume"),
            Self::Corrupt => f.write_str("the exFAT structures contradict each other"),
            Self::NotFound => f.write_str("no such file or directory"),
            Self::Exists => f.write_str("the name already exists"),
            Self::NotEmpty => f.write_str("the directory is not empty"),
            Self::NotDir => f.write_str("not a directory"),
            Self::IsDir => f.write_str("is a directory"),
            Self::BadName => f.write_str("the name is not allowed on exFAT"),
            Self::NoSpace => f.write_str("no free clusters left"),
            Self::ReadOnly => f.write_str("the volume is read-only"),
            Self::TooBig => f.write_str("the directory is too large"),
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Цепочка кластеров: первый, непрерывна ли (`NoFatChain`), длина в байтах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chain {
    pub first: u32,
    pub contiguous: bool,
    pub size: u64,
}

/// Где лежит набор записей: каталог, смещение записи файла, число записей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub dir: Chain,
    pub offset: u64,
    pub count: u8,
}

/// Файл или каталог, как его описывает набор записей.
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub attributes: u16,
    pub data: Chain,
    /// `ValidDataLength`: дальше — нули, даже если на диске что-то есть.
    pub valid: u64,
    /// Время создания, изменения и доступа — секунды Unix, UTC.
    pub created: u64,
    pub modified: u64,
    pub accessed: u64,
    /// `None` — корневой каталог.
    pub pos: Option<Pos>,
}

impl Node {
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.attributes & ATTR_DIRECTORY != 0
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.data.size
    }

    /// Постоянный номер узла на томе (для `st_ino`): место набора записей.
    #[must_use]
    pub fn id(&self) -> u64 {
        match self.pos {
            None => 1,
            Some(pos) => (u64::from(pos.dir.first) << 32) ^ (pos.offset / ENTRY as u64 + 2),
        }
    }
}

/// Где остановилось последнее чтение по цепочке FAT: номер кластера в файле и
/// сам кластер. Последовательное чтение тогда не ходит по цепочке с начала.
#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    index: u64,
    cluster: u32,
}

/// Открытый том exFAT.
pub struct Volume<D: BlockDevice> {
    dev: D,
    /// Первый сектор тома на носителе (в секторах носителя).
    base: u64,
    dev_sector: u64,
    /// Сколько байт носителя от начала тома.
    dev_bytes: u64,
    cluster_shift: u32,
    fat_offset: u64,
    heap_offset: u64,
    cluster_count: u32,
    root: Chain,
    serial: u32,
    label: String,
    bitmap: Vec<u8>,
    bitmap_clusters: Vec<u32>,
    free: u64,
    hint: u32,
    upcase: Vec<u16>,
    read_only: bool,
    dirty: bool,
    was_dirty: bool,
    cache: BTreeMap<u64, (Vec<u8>, u64)>,
    tick: u64,
}

impl<D: BlockDevice> Volume<D> {
    /// Открыть том с сектора `base` носителя длиной `sectors` секторов.
    pub fn mount(mut dev: D, base: u64, sectors: u64, read_only: bool) -> Result<Self> {
        let dev_sector = u64::from(dev.sector_size());
        if !crate::sector_size_supported(dev_sector as u32) {
            return Err(Error::NotExfat);
        }
        let mut first = vec![0u8; dev_sector as usize];
        dev.read(base, &mut first)?;
        if &first[3..11] != b"EXFAT   " || first[11..64].iter().any(|b| *b != 0) || first[510..512] != [0x55, 0xAA] {
            return Err(Error::NotExfat);
        }
        let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let u64_at = |b: &[u8], at: usize| u64::from(u32_at(b, at)) | (u64::from(u32_at(b, at + 4)) << 32);
        let sector_shift = u32::from(first[108]);
        let spc_shift = u32::from(first[109]);
        if !(9..=12).contains(&sector_shift) || sector_shift + spc_shift > 25 {
            return Err(Error::NotExfat);
        }
        let bps = 1u64 << sector_shift;
        let volume_length = u64_at(&first, 72);
        let fat_offset = u64::from(u32_at(&first, 80)) << sector_shift;
        let fat_length = u64::from(u32_at(&first, 84)) << sector_shift;
        let heap_offset = u64::from(u32_at(&first, 88)) << sector_shift;
        let cluster_count = u32_at(&first, 92);
        let root_cluster = u32_at(&first, 96);
        let serial = u32_at(&first, 100);
        let revision = u16::from_le_bytes([first[104], first[105]]);
        let flags = u16::from_le_bytes([first[106], first[107]]);
        let fats = first[110];
        let cluster_shift = sector_shift + spc_shift;
        let dev_bytes = sectors.saturating_mul(dev_sector);
        let vol_bytes = volume_length.checked_shl(sector_shift).ok_or(Error::NotExfat)?;
        if revision >> 8 != 1 || !(1..=2).contains(&fats) {
            return Err(Error::NotExfat);
        }
        let heap_end = heap_offset + (u64::from(cluster_count) << cluster_shift);
        if fat_offset < 24 * bps
            || fat_length < (u64::from(cluster_count) + 2) * 4
            || heap_offset < fat_offset + fat_length * u64::from(fats)
            || heap_end > vol_bytes
            || vol_bytes > dev_bytes
            || cluster_count == 0
            || cluster_count > 0xFFFF_FFF5
            || root_cluster < 2
            || root_cluster >= cluster_count + 2
        {
            return Err(Error::Corrupt);
        }

        // Контрольная сумма загрузочной области: 11 секторов, кроме
        // VolumeFlags и PercentInUse; двенадцатый — сумма, повторённая.
        let mut region = vec![0u8; (12 * bps) as usize];
        read_bytes(&mut dev, base, dev_sector, 0, &mut region)?;
        let mut sum: u32 = 0;
        for (index, byte) in region[..(11 * bps) as usize].iter().enumerate() {
            if matches!(index, 106 | 107 | 112) {
                continue;
            }
            sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 }) + (sum >> 1) + u32::from(*byte);
        }
        if region[(11 * bps) as usize..].chunks_exact(4).any(|word| u32_at(word, 0) != sum) {
            return Err(Error::Corrupt);
        }

        let mut read_only = read_only || dev.is_read_only();
        // TexFAT (два FAT) — только чтение: какой FAT активен, решает флаг.
        let fat_offset = if fats == 2 {
            read_only = true;
            if flags & 1 != 0 { fat_offset + fat_length } else { fat_offset }
        } else {
            fat_offset
        };
        let mut volume = Self {
            dev,
            base,
            dev_sector,
            dev_bytes,
            cluster_shift,
            fat_offset,
            heap_offset,
            cluster_count,
            root: Chain { first: root_cluster, contiguous: false, size: 0 },
            serial,
            label: String::new(),
            bitmap: Vec::new(),
            bitmap_clusters: Vec::new(),
            free: 0,
            hint: 2,
            upcase: Vec::new(),
            read_only,
            dirty: false,
            was_dirty: flags & VOLUME_DIRTY != 0,
            cache: BTreeMap::new(),
            tick: 0,
        };
        let root_clusters = volume.walk(root_cluster, None)?;
        volume.root.size = (root_clusters.len() as u64) << cluster_shift;

        // Служебные записи корня: карта, таблица прописных, метка.
        let root = volume.root;
        let bytes = volume.load_dir(&root)?;
        let mut bitmap = None;
        let mut upcase = None;
        for entry in bytes.chunks_exact(ENTRY) {
            match entry[0] {
                0 => break,
                ENTRY_BITMAP if bitmap.is_none() || (fats == 2 && (entry[1] & 1) == (flags & 1) as u8) => {
                    bitmap = Some((u32_at(entry, 20), u64_at(entry, 24)));
                }
                ENTRY_UPCASE => upcase = Some((u32_at(entry, 4), u32_at(entry, 20), u64_at(entry, 24))),
                ENTRY_LABEL => {
                    let count = usize::from(entry[1]).min(11);
                    let units: Vec<u16> =
                        (0..count).map(|i| u16::from_le_bytes([entry[2 + 2 * i], entry[3 + 2 * i]])).collect();
                    volume.label = decode(&units);
                }
                _ => {}
            }
        }
        let (bitmap_first, bitmap_len) = bitmap.ok_or(Error::Corrupt)?;
        let need = (u64::from(cluster_count) + 7) / 8;
        if bitmap_len < need {
            return Err(Error::Corrupt);
        }
        let clusters = volume.walk(bitmap_first, Some(bitmap_len))?;
        let mut map = Vec::new();
        map.try_reserve_exact(need as usize).map_err(|_| Error::TooBig)?;
        map.resize(need as usize, 0);
        let cs = 1u64 << cluster_shift;
        if (clusters.len() as u64) * cs < need {
            return Err(Error::Corrupt);
        }
        for (index, cluster) in clusters.iter().enumerate() {
            let start = index as u64 * cs;
            if start >= need {
                break;
            }
            let end = (start + cs).min(need);
            let at = volume.cluster_at(*cluster);
            volume.data_read(at, &mut map[start as usize..end as usize])?;
        }
        // Хвост последнего байта за пределами тома — не кластеры.
        let tail = cluster_count % 8;
        if tail != 0 {
            let last = map.len() - 1;
            map[last] &= (1u8 << tail) - 1;
        }
        let used: u64 = map.iter().map(|b| u64::from(b.count_ones())).sum();
        volume.free = u64::from(cluster_count).saturating_sub(used);
        volume.bitmap = map;
        volume.bitmap_clusters = clusters;

        // Таблица прописных: без неё нельзя ни сравнивать имена, ни считать
        // хеш, — и том без неё открывается только на чтение.
        let mut table: Vec<u16> = (0..=0xFFFFu32).map(|c| c as u16).collect();
        let mut table_ok = false;
        if let Some((checksum, first_cluster, length)) = upcase {
            if length > 0 && length <= 2 * 0x1_0000 * 2 {
                let mut raw = vec![0u8; length as usize];
                let clusters = volume.walk(first_cluster, Some(length))?;
                if (clusters.len() as u64) < length.div_ceil(cs) {
                    return Err(Error::Corrupt);
                }
                for (index, cluster) in clusters.iter().enumerate() {
                    let start = index as u64 * cs;
                    let end = (start + cs).min(length);
                    let at = volume.cluster_at(*cluster);
                    volume.data_read(at, &mut raw[start as usize..end as usize])?;
                }
                let mut sum: u32 = 0;
                for byte in &raw {
                    sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 }) + (sum >> 1) + u32::from(*byte);
                }
                if sum == checksum {
                    let mut index = 0usize;
                    let mut words = raw.chunks_exact(2).map(|w| u16::from_le_bytes([w[0], w[1]]));
                    while let Some(word) = words.next() {
                        if index >= table.len() {
                            break;
                        }
                        if word == 0xFFFF {
                            index += usize::from(words.next().unwrap_or(0));
                        } else {
                            table[index] = word;
                            index += 1;
                        }
                    }
                    table_ok = true;
                }
            }
        }
        if !table_ok {
            for c in u16::from(b'a')..=u16::from(b'z') {
                table[usize::from(c)] = c - 32;
            }
            volume.read_only = true;
        }
        volume.upcase = table;
        Ok(volume)
    }

    // --- о томе -----------------------------------------------------------

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn serial(&self) -> u32 {
        self.serial
    }

    #[must_use]
    pub fn read_only(&self) -> bool {
        self.read_only
    }

    /// Был ли том грязным при открытии (вынут посреди записи).
    #[must_use]
    pub fn was_dirty(&self) -> bool {
        self.was_dirty
    }

    #[must_use]
    pub fn cluster_size(&self) -> u64 {
        1 << self.cluster_shift
    }

    /// Всего и свободно, в байтах.
    #[must_use]
    pub fn space(&self) -> (u64, u64) {
        (u64::from(self.cluster_count) << self.cluster_shift, self.free << self.cluster_shift)
    }

    /// Носитель (для тестов: отдать образ обратно).
    pub fn into_device(self) -> D {
        self.dev
    }

    /// Носитель под томом (21.11): ядро держит под ним кэш блоков и читает
    /// данные файлов мимо замка тома.
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    /// Первый сектор тома на носителе, размер сектора носителя и длина тома
    /// в байтах.
    #[must_use]
    pub fn geometry(&self) -> (u64, u64, u64) {
        (self.base, self.dev_sector, self.dev_bytes)
    }

    /// Таблица прописных букв тома (для сравнения имён без учёта регистра).
    #[must_use]
    pub fn upcase_table(&self) -> &[u16] {
        &self.upcase
    }

    /// План чтения `[offset, offset + len)` файла без самого чтения (21.11):
    /// куски тома (смещение от начала тома, длина) для байтов до
    /// `ValidDataLength`, сколько байт из них с носителя и сколько всего
    /// (дальше `from_disk` до `total` — нули). Как [`Volume::read`].
    pub fn plan_read(&mut self, node: &Node, offset: u64, len: u64, cursor: &mut Cursor) -> Result<(Vec<(u64, u64)>, u64, u64)> {
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let size = node.data.size;
        if offset >= size || len == 0 {
            return Ok((Vec::new(), 0, 0));
        }
        let total = len.min(size - offset);
        let valid = node.valid.min(size);
        let disk_end = valid.max(offset).min(offset + total);
        let from_disk = disk_end - offset;
        let extents = if from_disk > 0 { self.extents(&node.data, offset, from_disk, cursor)? } else { Vec::new() };
        Ok((extents, from_disk, total))
    }

    #[must_use]
    pub fn root(&self) -> Node {
        Node {
            name: String::new(),
            attributes: ATTR_DIRECTORY,
            data: self.root,
            valid: self.root.size,
            created: 0,
            modified: 0,
            accessed: 0,
            pos: None,
        }
    }

    // --- носитель ---------------------------------------------------------

    fn cluster_at(&self, cluster: u32) -> u64 {
        self.heap_offset + (u64::from(cluster - 2) << self.cluster_shift)
    }

    fn valid_cluster(&self, cluster: u32) -> bool {
        cluster >= 2 && cluster < self.cluster_count + 2
    }

    /// Кусок кэша служебных данных.
    fn unit(&mut self, index: u64) -> Result<&mut Vec<u8>> {
        self.tick += 1;
        let tick = self.tick;
        if !self.cache.contains_key(&index) {
            if self.cache.len() >= CACHE_UNITS {
                let oldest = self.cache.iter().min_by_key(|(_, (_, t))| *t).map(|(k, _)| *k);
                if let Some(oldest) = oldest {
                    self.cache.remove(&oldest);
                }
            }
            let start = index * UNIT;
            let len = UNIT.min(self.dev_bytes.saturating_sub(start));
            if len == 0 {
                return Err(Error::Corrupt);
            }
            let mut buf = vec![0u8; len as usize];
            self.dev.read(self.base + start / self.dev_sector, &mut buf)?;
            self.cache.insert(index, (buf, tick));
        }
        let entry = self.cache.get_mut(&index).ok_or(Error::Corrupt)?;
        entry.1 = tick;
        Ok(&mut entry.0)
    }

    /// Прочитать служебные байты (каталог, FAT) через кэш.
    fn meta_read(&mut self, mut at: u64, mut out: &mut [u8]) -> Result<()> {
        while !out.is_empty() {
            let index = at / UNIT;
            let within = (at % UNIT) as usize;
            let unit = self.unit(index)?;
            let take = out.len().min(unit.len().saturating_sub(within));
            if take == 0 {
                return Err(Error::Corrupt);
            }
            out[..take].copy_from_slice(&unit[within..within + take]);
            at += take as u64;
            out = &mut out[take..];
        }
        Ok(())
    }

    /// Записать служебные байты: кэш и затронутые сектора носителя сразу.
    fn meta_write(&mut self, mut at: u64, mut data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let sector = self.dev_sector as usize;
        while !data.is_empty() {
            let index = at / UNIT;
            let within = (at % UNIT) as usize;
            let base = self.base;
            let dev_sector = self.dev_sector;
            let unit = self.unit(index)?;
            let take = data.len().min(unit.len().saturating_sub(within));
            if take == 0 {
                return Err(Error::Corrupt);
            }
            unit[within..within + take].copy_from_slice(&data[..take]);
            let first = within / sector * sector;
            let last = (within + take).div_ceil(sector) * sector;
            let chunk = unit[first..last].to_vec();
            let lba = base + (index * UNIT + first as u64) / dev_sector;
            self.dev.write(lba, &chunk)?;
            at += take as u64;
            data = &data[take..];
        }
        Ok(())
    }

    fn invalidate(&mut self, at: u64, len: u64) {
        if self.cache.is_empty() || len == 0 {
            return;
        }
        let first = at / UNIT;
        let last = (at + len - 1) / UNIT;
        let doomed: Vec<u64> = self.cache.range(first..=last).map(|(k, _)| *k).collect();
        for key in doomed {
            self.cache.remove(&key);
        }
    }

    /// Прочитать данные мимо кэша (смещение от начала тома, любое).
    fn data_read(&mut self, at: u64, buf: &mut [u8]) -> Result<()> {
        if at + buf.len() as u64 > self.dev_bytes {
            return Err(Error::Corrupt);
        }
        read_bytes(&mut self.dev, self.base, self.dev_sector, at, buf)
    }

    /// Записать данные мимо кэша.
    fn data_write(&mut self, mut at: u64, mut data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if at + data.len() as u64 > self.dev_bytes {
            return Err(Error::Corrupt);
        }
        self.invalidate(at, data.len() as u64);
        let sector = self.dev_sector;
        let s = sector as usize;
        // Неполный первый сектор — чтение, правка, запись.
        let mis = (at % sector) as usize;
        if mis != 0 || data.len() < s {
            let mut buf = vec![0u8; s];
            let lba = self.base + at / sector;
            self.dev.read(lba, &mut buf)?;
            let take = data.len().min(s - mis);
            buf[mis..mis + take].copy_from_slice(&data[..take]);
            self.dev.write(lba, &buf)?;
            at += take as u64;
            data = &data[take..];
        }
        let whole = data.len() / s * s;
        let mut done = 0usize;
        while done < whole {
            let take = (whole - done).min(CHUNK);
            self.dev.write(self.base + (at + done as u64) / sector, &data[done..done + take])?;
            done += take;
        }
        at += whole as u64;
        data = &data[whole..];
        if !data.is_empty() {
            let mut buf = vec![0u8; s];
            let lba = self.base + at / sector;
            self.dev.read(lba, &mut buf)?;
            buf[..data.len()].copy_from_slice(data);
            self.dev.write(lba, &buf)?;
        }
        Ok(())
    }

    // --- FAT и карта занятости ---------------------------------------------

    fn fat_get(&mut self, cluster: u32) -> Result<u32> {
        let mut word = [0u8; 4];
        self.meta_read(self.fat_offset + u64::from(cluster) * 4, &mut word)?;
        Ok(u32::from_le_bytes(word))
    }

    fn fat_set(&mut self, cluster: u32, value: u32) -> Result<()> {
        self.meta_write(self.fat_offset + u64::from(cluster) * 4, &value.to_le_bytes())
    }

    /// Кластеры цепочки FAT от `first`; `limit` — не больше стольких байт.
    fn walk(&mut self, first: u32, limit: Option<u64>) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        if first == 0 {
            return Ok(out);
        }
        let max = match limit {
            Some(bytes) => bytes.div_ceil(self.cluster_size()),
            None => u64::from(self.cluster_count),
        };
        let mut cluster = first;
        loop {
            if !self.valid_cluster(cluster) || out.len() as u64 >= u64::from(self.cluster_count) {
                return Err(Error::Corrupt);
            }
            out.push(cluster);
            if out.len() as u64 >= max {
                break;
            }
            let next = self.fat_get(cluster)?;
            if next >= 0xFFFF_FFF8 {
                break;
            }
            cluster = next;
        }
        Ok(out)
    }

    /// Кластеры цепочки целиком.
    fn clusters_of(&mut self, chain: &Chain) -> Result<Vec<u32>> {
        if chain.first == 0 {
            return Ok(Vec::new());
        }
        let count = chain.size.div_ceil(self.cluster_size());
        if chain.contiguous {
            if chain.first as u64 + count > u64::from(self.cluster_count) + 2 {
                return Err(Error::Corrupt);
            }
            return Ok((0..count).map(|i| chain.first + i as u32).collect());
        }
        // Корень длины не хранит: идём до конца цепочки.
        let limit = if chain.size == 0 || (chain.first == self.root.first) { None } else { Some(chain.size) };
        self.walk(chain.first, limit)
    }

    /// Кластер номер `index` файла.
    fn map(&mut self, chain: &Chain, index: u64, cursor: &mut Cursor) -> Result<u32> {
        if chain.first == 0 {
            return Err(Error::Corrupt);
        }
        if chain.contiguous {
            let cluster = u64::from(chain.first) + index;
            if cluster >= u64::from(self.cluster_count) + 2 {
                return Err(Error::Corrupt);
            }
            return Ok(cluster as u32);
        }
        let (mut at, mut cluster) = if cursor.cluster != 0 && cursor.index <= index {
            (cursor.index, cursor.cluster)
        } else {
            (0, chain.first)
        };
        while at < index {
            let next = self.fat_get(cluster)?;
            if !self.valid_cluster(next) {
                return Err(Error::Corrupt);
            }
            cluster = next;
            at += 1;
        }
        *cursor = Cursor { index, cluster };
        Ok(cluster)
    }

    /// Куски тома под байты `[offset, offset + len)` файла.
    fn extents(&mut self, chain: &Chain, offset: u64, len: u64, cursor: &mut Cursor) -> Result<Vec<(u64, u64)>> {
        let mut out: Vec<(u64, u64)> = Vec::new();
        if len == 0 {
            return Ok(out);
        }
        let cs = self.cluster_size();
        let mut index = offset >> self.cluster_shift;
        let mut within = offset & (cs - 1);
        let mut left = len;
        let mut cluster = self.map(chain, index, cursor)?;
        loop {
            let take = left.min(cs - within);
            let at = self.cluster_at(cluster) + within;
            match out.last_mut() {
                Some(last) if last.0 + last.1 == at => last.1 += take,
                _ => out.push((at, take)),
            }
            left -= take;
            if left == 0 {
                break;
            }
            index += 1;
            within = 0;
            cluster = if chain.contiguous {
                let next = cluster + 1;
                if !self.valid_cluster(next) {
                    return Err(Error::Corrupt);
                }
                next
            } else {
                let next = self.fat_get(cluster)?;
                if !self.valid_cluster(next) {
                    return Err(Error::Corrupt);
                }
                *cursor = Cursor { index, cluster: next };
                next
            };
        }
        Ok(out)
    }

    fn bit(&self, cluster: u32) -> bool {
        let index = (cluster - 2) as usize;
        self.bitmap[index / 8] & (1 << (index % 8)) != 0
    }

    /// Пометить `count` кластеров с `first` занятыми или свободными — в памяти и
    /// на носителе.
    fn mark(&mut self, first: u32, count: u32, used: bool) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        for cluster in first..first + count {
            let index = (cluster - 2) as usize;
            let mask = 1u8 << (index % 8);
            let was = self.bitmap[index / 8] & mask != 0;
            if was != used {
                if used {
                    self.bitmap[index / 8] |= mask;
                    self.free -= 1;
                } else {
                    self.bitmap[index / 8] &= !mask;
                    self.free += 1;
                }
            }
        }
        let lo = ((first - 2) / 8) as u64;
        let hi = ((first + count - 3) / 8) as u64 + 1;
        self.write_bitmap(lo, hi)
    }

    /// Записать байты карты `[lo, hi)` на носитель.
    fn write_bitmap(&mut self, lo: u64, hi: u64) -> Result<()> {
        let cs = self.cluster_size();
        let mut at = lo;
        while at < hi {
            let index = (at / cs) as usize;
            let within = at % cs;
            let take = (hi - at).min(cs - within);
            let cluster = *self.bitmap_clusters.get(index).ok_or(Error::Corrupt)?;
            let target = self.cluster_at(cluster) + within;
            let bytes = self.bitmap[at as usize..(at + take) as usize].to_vec();
            self.meta_write(target, &bytes)?;
            at += take;
        }
        Ok(())
    }

    /// Свободны ли `count` кластеров с `first`.
    fn range_free(&self, first: u32, count: u64) -> bool {
        if u64::from(first) + count > u64::from(self.cluster_count) + 2 {
            return false;
        }
        (0..count).all(|i| !self.bit(first + i as u32))
    }

    /// Первый свободный пробег из `count` кластеров, начиная с подсказки.
    fn find_run(&self, count: u64) -> Option<u32> {
        let total = u64::from(self.cluster_count);
        if count == 0 || count > self.free {
            return None;
        }
        for (from, to) in [(u64::from(self.hint - 2), total), (0, u64::from(self.hint - 2))] {
            let mut run = 0u64;
            let mut index = from;
            while index < to {
                let byte = self.bitmap[(index / 8) as usize];
                if index % 8 == 0 && byte == 0xFF {
                    run = 0;
                    index += 8;
                    continue;
                }
                if byte & (1 << (index % 8)) == 0 {
                    run += 1;
                    if run == count {
                        return Some((index + 1 - count) as u32 + 2);
                    }
                } else {
                    run = 0;
                }
                index += 1;
            }
        }
        None
    }

    /// Взять `count` свободных кластеров где придётся, начиная с `near`.
    fn take_scattered(&mut self, count: u64, near: u32) -> Result<Vec<u32>> {
        if count > self.free {
            return Err(Error::NoSpace);
        }
        let total = u64::from(self.cluster_count);
        let start = if self.valid_cluster(near) { u64::from(near - 2) } else { u64::from(self.hint - 2) };
        let mut out = Vec::new();
        let mut index = start;
        let mut seen = 0u64;
        while (out.len() as u64) < count && seen < total {
            let byte = self.bitmap[(index / 8) as usize];
            if !(index % 8 == 0 && byte == 0xFF && index + 8 <= total) {
                if byte & (1 << (index % 8)) == 0 {
                    out.push(index as u32 + 2);
                }
                index += 1;
                seen += 1;
            } else {
                index += 8;
                seen += 8;
            }
            if index >= total {
                index = 0;
            }
        }
        if (out.len() as u64) < count {
            return Err(Error::NoSpace);
        }
        Ok(out)
    }

    /// Нарастить цепочку с `have` до `want` кластеров.
    fn grow(&mut self, chain: &mut Chain, have: u64, want: u64) -> Result<()> {
        if want <= have {
            return Ok(());
        }
        let add = want - have;
        if add > self.free {
            return Err(Error::NoSpace);
        }
        if have == 0 {
            if let Some(start) = self.find_run(add) {
                self.mark(start, add as u32, true)?;
                chain.first = start;
                chain.contiguous = true;
                self.hint = start;
                return Ok(());
            }
            let list = self.take_scattered(add, self.hint)?;
            self.link(&list)?;
            chain.first = list[0];
            chain.contiguous = false;
            return Ok(());
        }
        let last = if chain.contiguous {
            let next = u64::from(chain.first) + have;
            if next <= u32::MAX as u64 && self.range_free(next as u32, add) {
                self.mark(next as u32, add as u32, true)?;
                return Ok(());
            }
            // Дальше занято: непрерывный файл становится цепочкой в FAT.
            for i in 0..have - 1 {
                let cluster = chain.first + i as u32;
                self.fat_set(cluster, cluster + 1)?;
            }
            let last = chain.first + (have - 1) as u32;
            self.fat_set(last, EOC)?;
            chain.contiguous = false;
            last
        } else {
            let clusters = self.walk(chain.first, Some(have << self.cluster_shift))?;
            *clusters.last().ok_or(Error::Corrupt)?
        };
        let list = self.take_scattered(add, last + 1)?;
        self.link(&list)?;
        self.fat_set(last, list[0])
    }

    /// Занять кластеры списка и связать их цепочкой в FAT.
    fn link(&mut self, list: &[u32]) -> Result<()> {
        for (index, cluster) in list.iter().enumerate() {
            let next = list.get(index + 1).copied().unwrap_or(EOC);
            self.fat_set(*cluster, next)?;
            self.mark(*cluster, 1, true)?;
        }
        if let Some(last) = list.last() {
            self.hint = *last;
        }
        Ok(())
    }

    /// Укоротить цепочку с `have` до `keep` кластеров.
    fn shrink(&mut self, chain: &mut Chain, have: u64, keep: u64) -> Result<()> {
        if keep >= have || chain.first == 0 {
            return Ok(());
        }
        if chain.contiguous {
            self.mark(chain.first + keep as u32, (have - keep) as u32, false)?;
        } else {
            let clusters = self.walk(chain.first, Some(have << self.cluster_shift))?;
            if (clusters.len() as u64) < keep {
                return Err(Error::Corrupt);
            }
            if keep > 0 {
                self.fat_set(clusters[keep as usize - 1], EOC)?;
            }
            for cluster in &clusters[keep as usize..] {
                self.mark(*cluster, 1, false)?;
            }
        }
        if keep == 0 {
            chain.first = 0;
            chain.contiguous = false;
        }
        Ok(())
    }

    /// Пометить том грязным перед первой записью.
    fn begin_write(&mut self) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if !self.dirty {
            self.set_volume_flags(true)?;
            self.dirty = true;
        }
        Ok(())
    }

    fn set_volume_flags(&mut self, dirty: bool) -> Result<()> {
        let mut boot = vec![0u8; self.dev_sector as usize];
        self.dev.read(self.base, &mut boot)?;
        let mut flags = u16::from_le_bytes([boot[106], boot[107]]);
        if dirty {
            flags |= VOLUME_DIRTY;
        } else {
            flags &= !VOLUME_DIRTY;
        }
        boot[106..108].copy_from_slice(&flags.to_le_bytes());
        // PercentInUse — подсказка Windows; пересчитываем заодно.
        if !dirty {
            let used = u64::from(self.cluster_count) - self.free;
            boot[112] = (used * 100 / u64::from(self.cluster_count)) as u8;
        }
        self.invalidate(0, self.dev_sector);
        self.dev.write(self.base, &boot)?;
        self.dev.flush()?;
        Ok(())
    }

    /// Довести записи до носителя и снять пометку «грязный». Если том был
    /// грязным уже при открытии, пометка остаётся — проверку решит Windows.
    pub fn clean(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.dev.flush()?;
        if !self.was_dirty {
            self.set_volume_flags(false)?;
        }
        self.dirty = false;
        Ok(())
    }

    /// Сбросить кэш носителя.
    pub fn flush(&mut self) -> Result<()> {
        self.dev.flush()?;
        Ok(())
    }

    // --- каталоги ----------------------------------------------------------

    /// Содержимое каталога целиком.
    fn load_dir(&mut self, dir: &Chain) -> Result<Vec<u8>> {
        let clusters = self.clusters_of(dir)?;
        let cs = self.cluster_size();
        if (clusters.len() as u64) * cs > 256 << 20 {
            return Err(Error::TooBig);
        }
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(clusters.len() * cs as usize).map_err(|_| Error::TooBig)?;
        bytes.resize(clusters.len() * cs as usize, 0);
        for (index, cluster) in clusters.iter().enumerate() {
            let at = self.cluster_at(*cluster);
            let start = index * cs as usize;
            self.meta_read(at, &mut bytes[start..start + cs as usize])?;
        }
        Ok(bytes)
    }

    /// Цепочка каталога, в котором живут дети узла.
    fn dir_chain(&self, dir: &Node) -> Chain {
        if dir.pos.is_none() { self.root } else { dir.data }
    }

    /// Место записи каталога на томе.
    fn entry_at(&mut self, dir: &Chain, offset: u64) -> Result<u64> {
        let index = offset >> self.cluster_shift;
        let mut cursor = Cursor::default();
        let cluster = self.map(dir, index, &mut cursor)?;
        Ok(self.cluster_at(cluster) + (offset & (self.cluster_size() - 1)))
    }

    fn read_set(&mut self, pos: &Pos) -> Result<Vec<u8>> {
        let mut bytes = vec![0u8; usize::from(pos.count) * ENTRY];
        for (index, entry) in bytes.chunks_exact_mut(ENTRY).enumerate() {
            let at = self.entry_at(&pos.dir, pos.offset + (index * ENTRY) as u64)?;
            self.meta_read(at, entry)?;
        }
        Ok(bytes)
    }

    fn write_set(&mut self, dir: &Chain, offset: u64, bytes: &[u8]) -> Result<()> {
        for (index, entry) in bytes.chunks_exact(ENTRY).enumerate() {
            let at = self.entry_at(dir, offset + (index * ENTRY) as u64)?;
            self.meta_write(at, entry)?;
        }
        Ok(())
    }

    /// Разобрать набор записей с `offset`; `None` — там не файл.
    fn parse_set(&self, dir: &Chain, bytes: &[u8], offset: usize) -> Option<Node> {
        let first = bytes.get(offset..offset + ENTRY)?;
        if first[0] != ENTRY_FILE {
            return None;
        }
        let secondary = usize::from(first[1]);
        if !(2..=18).contains(&secondary) {
            return None;
        }
        let set = bytes.get(offset..offset + (secondary + 1) * ENTRY)?;
        if u16::from_le_bytes([set[2], set[3]]) != set_checksum(set) {
            return None;
        }
        let stream = &set[ENTRY..2 * ENTRY];
        if stream[0] != ENTRY_STREAM {
            return None;
        }
        let name_len = usize::from(stream[3]);
        let mut units = Vec::with_capacity(name_len);
        for entry in set[2 * ENTRY..].chunks_exact(ENTRY) {
            if entry[0] & IN_USE == 0 {
                return None;
            }
            if entry[0] == ENTRY_NAME {
                for i in 0..NAME_PER_ENTRY {
                    if units.len() < name_len {
                        units.push(u16::from_le_bytes([entry[2 + 2 * i], entry[3 + 2 * i]]));
                    }
                }
            }
        }
        if units.len() != name_len || name_len == 0 {
            return None;
        }
        let u32_at = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
        let u64_at = |b: &[u8], at: usize| u64::from(u32_at(b, at)) | (u64::from(u32_at(b, at + 4)) << 32);
        let size = u64_at(stream, 24);
        let first_cluster = u32_at(stream, 20);
        Some(Node {
            name: decode(&units),
            attributes: u16::from_le_bytes([first[4], first[5]]),
            data: Chain {
                first: first_cluster,
                contiguous: stream[1] & FLAG_NO_FAT_CHAIN != 0,
                size,
            },
            valid: u64_at(stream, 8).min(size),
            created: decode_time(u32_at(first, 8), first[20], first[22]),
            modified: decode_time(u32_at(first, 12), first[21], first[23]),
            accessed: decode_time(u32_at(first, 16), 0, first[24]),
            pos: Some(Pos { dir: *dir, offset: offset as u64, count: (secondary + 1) as u8 }),
        })
    }

    /// Все файлы и каталоги каталога.
    pub fn list(&mut self, dir: &Node) -> Result<Vec<Node>> {
        if !dir.is_dir() {
            return Err(Error::NotDir);
        }
        let chain = self.dir_chain(dir);
        let bytes = self.load_dir(&chain)?;
        let mut out = Vec::new();
        let mut offset = 0usize;
        while offset + ENTRY <= bytes.len() {
            let kind = bytes[offset];
            if kind == 0 {
                break;
            }
            if kind == ENTRY_FILE {
                if let Some(node) = self.parse_set(&chain, &bytes, offset) {
                    offset += node.pos.map_or(1, |p| usize::from(p.count)) * ENTRY;
                    out.push(node);
                    continue;
                }
            }
            offset += ENTRY;
        }
        Ok(out)
    }

    fn upcased(&self, units: &[u16]) -> Vec<u16> {
        units.iter().map(|u| self.upcase[usize::from(*u)]).collect()
    }

    fn same_name(&self, a: &str, b_up: &[u16]) -> bool {
        let mut units = a.encode_utf16();
        for want in b_up {
            match units.next() {
                Some(u) if self.upcase[usize::from(u)] == *want => {}
                _ => return false,
            }
        }
        units.next().is_none()
    }

    /// Найти имя в каталоге (без учёта регистра, как Windows).
    pub fn lookup(&mut self, dir: &Node, name: &str) -> Result<Node> {
        let up: Vec<u16> = self.upcased(&name.encode_utf16().collect::<Vec<u16>>());
        self.list(dir)?.into_iter().find(|node| self.same_name(&node.name, &up)).ok_or(Error::NotFound)
    }

    /// Перечитать узел с носителя: размер и время могли смениться.
    pub fn refresh(&mut self, node: &Node) -> Result<Node> {
        let Some(pos) = node.pos else {
            return Ok(self.root());
        };
        let bytes = self.read_set(&pos)?;
        let mut fresh = self.parse_set(&pos.dir, &bytes, 0).ok_or(Error::NotFound)?;
        // Разбирался один набор, и смещение в нём — ноль; место — прежнее.
        fresh.pos = Some(pos);
        let up = self.upcased(&node.name.encode_utf16().collect::<Vec<u16>>());
        if !self.same_name(&fresh.name, &up) {
            return Err(Error::NotFound);
        }
        Ok(fresh)
    }

    // --- данные ------------------------------------------------------------

    /// Прочитать до `buf.len()` байт файла с `offset`.
    pub fn read(&mut self, node: &Node, offset: u64, buf: &mut [u8], cursor: &mut Cursor) -> Result<usize> {
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let size = node.data.size;
        if offset >= size || buf.is_empty() {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(size - offset) as usize;
        let valid = node.valid.min(size);
        let disk_end = valid.max(offset).min(offset + len as u64);
        let from_disk = (disk_end - offset) as usize;
        if from_disk > 0 {
            let extents = self.extents(&node.data, offset, from_disk as u64, cursor)?;
            let mut done = 0usize;
            for (at, n) in extents {
                let n = n as usize;
                self.data_read(at, &mut buf[done..done + n])?;
                done += n;
            }
        }
        buf[from_disk..len].fill(0);
        Ok(len)
    }

    /// Записать `data` в файл с `offset`; файл растёт по надобности.
    pub fn write(&mut self, node: &Node, offset: u64, data: &[u8], now: u64) -> Result<Node> {
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let mut n = self.refresh(node)?;
        if data.is_empty() {
            return Ok(n);
        }
        self.begin_write()?;
        let end = offset.checked_add(data.len() as u64).ok_or(Error::NoSpace)?;
        let cs = self.cluster_size();
        let have = n.data.size.div_ceil(cs);
        let want = end.max(n.data.size).div_ceil(cs);
        if want > have {
            self.grow(&mut n.data, have, want)?;
        }
        if end > n.data.size {
            n.data.size = end;
        }
        let mut cursor = Cursor::default();
        // Между прежней действительной длиной и началом записи — нули на
        // носителе: дальше `ValidDataLength` их больше никто не подставит.
        if offset > n.valid {
            let zeros = vec![0u8; CHUNK.min((offset - n.valid) as usize)];
            let mut at = n.valid;
            while at < offset {
                let take = (offset - at).min(zeros.len() as u64);
                for (pos, len) in self.extents(&n.data, at, take, &mut cursor)? {
                    self.data_write(pos, &zeros[..len as usize])?;
                }
                at += take;
            }
        }
        let mut done = 0usize;
        for (at, len) in self.extents(&n.data, offset, data.len() as u64, &mut cursor)? {
            let len = len as usize;
            self.data_write(at, &data[done..done + len])?;
            done += len;
        }
        n.valid = n.valid.max(end);
        n.modified = now;
        n.accessed = now;
        n.attributes |= ATTR_ARCHIVE;
        self.update_set(&n)?;
        Ok(n)
    }

    /// Сменить длину файла.
    pub fn truncate(&mut self, node: &Node, size: u64, now: u64) -> Result<Node> {
        if node.is_dir() {
            return Err(Error::IsDir);
        }
        let mut n = self.refresh(node)?;
        if n.data.size == size {
            return Ok(n);
        }
        self.begin_write()?;
        let cs = self.cluster_size();
        let have = n.data.size.div_ceil(cs);
        let want = size.div_ceil(cs);
        if want > have {
            self.grow(&mut n.data, have, want)?;
        } else {
            self.shrink(&mut n.data, have, want)?;
        }
        n.data.size = size;
        n.valid = n.valid.min(size);
        n.modified = now;
        n.attributes |= ATTR_ARCHIVE;
        self.update_set(&n)?;
        Ok(n)
    }

    /// Записать поля узла в его набор записей.
    fn update_set(&mut self, node: &Node) -> Result<()> {
        let Some(pos) = node.pos else {
            return Ok(());
        };
        let mut set = self.read_set(&pos)?;
        patch_set(&mut set, node);
        self.write_set(&pos.dir, pos.offset, &set)
    }

    /// Свободное место под `count` записей подряд; `None` — каталог полон.
    fn find_slots(bytes: &[u8], count: usize) -> Option<usize> {
        let mut run = 0usize;
        for (index, entry) in bytes.chunks_exact(ENTRY).enumerate() {
            if entry[0] & IN_USE == 0 {
                run += 1;
                if run == count {
                    return Some((index + 1 - count) * ENTRY);
                }
            } else {
                run = 0;
            }
        }
        None
    }

    /// Нарастить каталог на кластер (нулевой).
    fn grow_dir(&mut self, dir: &mut Node) -> Result<()> {
        let cs = self.cluster_size();
        let mut chain = self.dir_chain(dir);
        let have = chain.size.div_ceil(cs);
        if (have + 1) * cs > 256 << 20 {
            return Err(Error::TooBig);
        }
        self.grow(&mut chain, have, have + 1)?;
        chain.size = (have + 1) * cs;
        let mut cursor = Cursor::default();
        let cluster = self.map(&chain, have, &mut cursor)?;
        let at = self.cluster_at(cluster);
        self.data_write(at, &vec![0u8; cs as usize])?;
        if dir.pos.is_none() {
            self.root = chain;
            dir.data = chain;
            dir.valid = chain.size;
        } else {
            dir.data = chain;
            dir.valid = chain.size;
            self.update_set(dir)?;
        }
        Ok(())
    }

    /// Вставить набор записей в каталог; возвращает место.
    fn insert_set(&mut self, dir: &mut Node, set: &[u8]) -> Result<Pos> {
        let count = set.len() / ENTRY;
        loop {
            let chain = self.dir_chain(dir);
            let bytes = self.load_dir(&chain)?;
            if let Some(offset) = Self::find_slots(&bytes, count) {
                self.write_set(&chain, offset as u64, set)?;
                return Ok(Pos { dir: chain, offset: offset as u64, count: count as u8 });
            }
            self.grow_dir(dir)?;
        }
    }

    /// Пометить набор записей удалённым.
    fn delete_set(&mut self, pos: &Pos) -> Result<()> {
        let mut set = self.read_set(pos)?;
        for entry in set.chunks_exact_mut(ENTRY) {
            entry[0] &= !IN_USE;
        }
        self.write_set(&pos.dir, pos.offset, &set)
    }

    /// Создать пустой файл или каталог.
    pub fn create(&mut self, dir: &Node, name: &str, directory: bool, now: u64) -> Result<Node> {
        let units = name_units(name)?;
        let mut parent = self.refresh(dir)?;
        if !parent.is_dir() {
            return Err(Error::NotDir);
        }
        match self.lookup(&parent, name) {
            Ok(_) => return Err(Error::Exists),
            Err(Error::NotFound) => {}
            Err(err) => return Err(err),
        }
        self.begin_write()?;
        let mut node = Node {
            name: String::from(name),
            attributes: if directory { ATTR_DIRECTORY } else { ATTR_ARCHIVE },
            data: Chain { first: 0, contiguous: false, size: 0 },
            valid: 0,
            created: now,
            modified: now,
            accessed: now,
            pos: None,
        };
        if directory {
            let cs = self.cluster_size();
            self.grow(&mut node.data, 0, 1)?;
            node.data.size = cs;
            node.valid = cs;
            let at = self.cluster_at(node.data.first);
            self.data_write(at, &vec![0u8; cs as usize])?;
        }
        let set = self.build_set(&node, &units);
        node.pos = Some(self.insert_set(&mut parent, &set)?);
        Ok(node)
    }

    /// Пуст ли каталог.
    fn is_empty_dir(&mut self, dir: &Node) -> Result<bool> {
        Ok(self.list(dir)?.is_empty())
    }

    /// Удалить файл (`directory == false`) или пустой каталог.
    pub fn remove(&mut self, dir: &Node, name: &str, directory: bool) -> Result<()> {
        let parent = self.refresh(dir)?;
        let node = self.lookup(&parent, name)?;
        match (node.is_dir(), directory) {
            (true, false) => return Err(Error::IsDir),
            (false, true) => return Err(Error::NotDir),
            _ => {}
        }
        if node.is_dir() && !self.is_empty_dir(&node)? {
            return Err(Error::NotEmpty);
        }
        self.begin_write()?;
        self.drop_node(node)
    }

    fn drop_node(&mut self, mut node: Node) -> Result<()> {
        let pos = node.pos.ok_or(Error::Corrupt)?;
        // Сначала запись, потом кластеры: оборванное удаление оставит
        // потерянные кластеры, а не два файла на одних и тех же.
        self.delete_set(&pos)?;
        let have = node.data.size.div_ceil(self.cluster_size());
        self.shrink(&mut node.data, have, 0)
    }

    /// Переименовать или перенести: `old_name` из `old_dir` в `new_dir` под
    /// `new_name`. Существующий файл с новым именем заменяется.
    pub fn rename(&mut self, old_dir: &Node, old_name: &str, new_dir: &Node, new_name: &str) -> Result<()> {
        let units = name_units(new_name)?;
        let source_dir = self.refresh(old_dir)?;
        let source = self.lookup(&source_dir, old_name)?;
        let mut target_dir = self.refresh(new_dir)?;
        if !target_dir.is_dir() {
            return Err(Error::NotDir);
        }
        let source_pos = source.pos.ok_or(Error::Corrupt)?;
        match self.lookup(&target_dir, new_name) {
            Ok(existing) if existing.pos == source.pos => {
                if existing.name == new_name {
                    return Ok(());
                }
            }
            Ok(existing) => {
                match (existing.is_dir(), source.is_dir()) {
                    (true, false) => return Err(Error::IsDir),
                    (false, true) => return Err(Error::NotDir),
                    _ => {}
                }
                if existing.is_dir() && !self.is_empty_dir(&existing)? {
                    return Err(Error::NotEmpty);
                }
                self.begin_write()?;
                self.drop_node(existing)?;
                target_dir = self.refresh(new_dir)?;
            }
            Err(Error::NotFound) => {}
            Err(err) => return Err(err),
        }
        self.begin_write()?;
        let old = self.read_set(&source_pos)?;
        let mut renamed = source.clone();
        renamed.name = String::from(new_name);
        let mut set = self.build_set(&renamed, &units);
        // Время и атрибуты — как были (запись файла целиком, кроме числа
        // вторичных записей и суммы).
        set[4..ENTRY].copy_from_slice(&old[4..ENTRY]);
        let sum = set_checksum(&set);
        set[2..4].copy_from_slice(&sum.to_le_bytes());
        self.insert_set(&mut target_dir, &set)?;
        // Каталог-источник мог вырасти (тот же каталог) — место старого
        // набора от этого не сдвигается.
        self.delete_set(&source_pos)
    }

    /// Набор записей для узла: файл, поток, имена.
    fn build_set(&self, node: &Node, units: &[u16]) -> Vec<u8> {
        let names = units.len().div_ceil(NAME_PER_ENTRY);
        let mut set = vec![0u8; (2 + names) * ENTRY];
        set[0] = ENTRY_FILE;
        set[1] = (1 + names) as u8;
        set[ENTRY] = ENTRY_STREAM;
        set[ENTRY + 3] = units.len() as u8;
        let hash = name_hash(&self.upcased(units));
        set[ENTRY + 4..ENTRY + 6].copy_from_slice(&hash.to_le_bytes());
        for (index, chunk) in units.chunks(NAME_PER_ENTRY).enumerate() {
            let entry = &mut set[(2 + index) * ENTRY..(3 + index) * ENTRY];
            entry[0] = ENTRY_NAME;
            for (i, unit) in chunk.iter().enumerate() {
                entry[2 + 2 * i..4 + 2 * i].copy_from_slice(&unit.to_le_bytes());
            }
        }
        let (ts, ms, utc) = encode_time(node.created);
        set[8..12].copy_from_slice(&ts.to_le_bytes());
        set[20] = ms;
        set[22] = utc;
        patch_set(&mut set, node);
        set
    }
}

/// Записать в набор записей поля узла и пересчитать сумму.
fn patch_set(set: &mut [u8], node: &Node) {
    set[4..6].copy_from_slice(&node.attributes.to_le_bytes());
    let (ts, ms, utc) = encode_time(node.modified);
    set[12..16].copy_from_slice(&ts.to_le_bytes());
    set[21] = ms;
    set[23] = utc;
    let (ts, _, utc) = encode_time(node.accessed);
    set[16..20].copy_from_slice(&ts.to_le_bytes());
    set[24] = utc;
    let stream = &mut set[ENTRY..2 * ENTRY];
    let mut flags = (stream[1] & !(FLAG_NO_FAT_CHAIN)) | FLAG_ALLOCATION_POSSIBLE;
    if node.data.contiguous && node.data.first != 0 {
        flags |= FLAG_NO_FAT_CHAIN;
    }
    stream[1] = flags;
    stream[8..16].copy_from_slice(&node.valid.to_le_bytes());
    stream[20..24].copy_from_slice(&node.data.first.to_le_bytes());
    stream[24..32].copy_from_slice(&node.data.size.to_le_bytes());
    let sum = set_checksum(set);
    set[2..4].copy_from_slice(&sum.to_le_bytes());
}

/// Прочитать байты тома с любого смещения через сектора носителя.
fn read_bytes<D: BlockDevice>(dev: &mut D, base: u64, sector: u64, mut at: u64, mut buf: &mut [u8]) -> Result<()> {
    let s = sector as usize;
    let mis = (at % sector) as usize;
    if mis != 0 || (buf.len() < s && !buf.is_empty()) {
        let mut tmp = vec![0u8; s];
        dev.read(base + at / sector, &mut tmp)?;
        let take = buf.len().min(s - mis);
        buf[..take].copy_from_slice(&tmp[mis..mis + take]);
        at += take as u64;
        buf = &mut buf[take..];
    }
    let whole = buf.len() / s * s;
    let mut done = 0usize;
    while done < whole {
        let take = (whole - done).min(CHUNK);
        dev.read(base + (at + done as u64) / sector, &mut buf[done..done + take])?;
        done += take;
    }
    at += whole as u64;
    let rest = &mut buf[whole..];
    if !rest.is_empty() {
        let mut tmp = vec![0u8; s];
        dev.read(base + at / sector, &mut tmp)?;
        let n = rest.len();
        rest.copy_from_slice(&tmp[..n]);
    }
    Ok(())
}

/// Контрольная сумма набора записей (кроме самого поля суммы).
fn set_checksum(set: &[u8]) -> u16 {
    let mut sum: u16 = 0;
    for (index, byte) in set.iter().enumerate() {
        if index == 2 || index == 3 {
            continue;
        }
        sum = (if sum & 1 != 0 { 0x8000u16 } else { 0 }).wrapping_add(sum >> 1).wrapping_add(u16::from(*byte));
    }
    sum
}

/// Хеш имени: по прописным знакам, младший байт первым.
fn name_hash(upcased: &[u16]) -> u16 {
    let mut hash: u16 = 0;
    for unit in upcased {
        for byte in unit.to_le_bytes() {
            hash = (if hash & 1 != 0 { 0x8000u16 } else { 0 }).wrapping_add(hash >> 1).wrapping_add(u16::from(byte));
        }
    }
    hash
}

/// Имя — в знаки UTF-16, с проверкой того, что Windows такое примет.
fn name_units(name: &str) -> Result<Vec<u16>> {
    if name.is_empty() || name == "." || name == ".." || name.ends_with('.') || name.ends_with(' ') {
        return Err(Error::BadName);
    }
    if name.chars().any(|c| (c as u32) < 0x20 || matches!(c, '"' | '*' | '/' | ':' | '<' | '>' | '?' | '\\' | '|')) {
        return Err(Error::BadName);
    }
    let units: Vec<u16> = name.encode_utf16().collect();
    if units.len() > NAME_MAX {
        return Err(Error::BadName);
    }
    Ok(units)
}

fn decode(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect()
}

// --- время -----------------------------------------------------------------

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, month, day)
}

/// Метка времени exFAT в секунды Unix (UTC).
fn decode_time(stamp: u32, ten_ms: u8, utc: u8) -> u64 {
    if stamp == 0 {
        return 0;
    }
    let year = 1980 + i64::from(stamp >> 25);
    let month = i64::from((stamp >> 21) & 0xF).clamp(1, 12);
    let day = i64::from((stamp >> 16) & 0x1F).max(1);
    let hour = i64::from((stamp >> 11) & 0x1F);
    let minute = i64::from((stamp >> 5) & 0x3F);
    let second = i64::from(stamp & 0x1F) * 2 + i64::from(ten_ms.min(199)) / 100;
    let mut unix = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    if utc & 0x80 != 0 {
        // Смещение — семь бит со знаком, в четвертях часа.
        let quarter = i64::from(((utc << 1) as i8) >> 1);
        unix -= quarter * 15 * 60;
    }
    unix.max(0) as u64
}

/// Секунды Unix в метку exFAT: время UTC, смещение 0 объявлено.
fn encode_time(unix: u64) -> (u32, u8, u8) {
    let min = days_from_civil(1980, 1, 1) * 86_400;
    let max = days_from_civil(2107, 12, 31) * 86_400 + 86_399;
    let t = (unix as i64).clamp(min, max);
    let (year, month, day) = civil_from_days(t.div_euclid(86_400));
    let rem = t.rem_euclid(86_400);
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let stamp = ((year - 1980) as u32) << 25
        | (month as u32) << 21
        | (day as u32) << 16
        | (hour as u32) << 11
        | (minute as u32) << 5
        | (second as u32 / 2);
    (stamp, ((second % 2) * 100) as u8, 0x80)
}

#[cfg(test)]
mod tests;
