//! FAT32, только чтение.
//!
//! Формат выбран не из любви к нему: спецификация UEFI требует, чтобы системный
//! раздел (ESP) был FAT, и прошивка читает загрузчик именно оттуда. Значит, ядру
//! всё равно придётся уметь FAT, и дешевле смонтировать тот же раздел, чем
//! заводить второй формат ради первых шагов.
//!
//! Реализация умышленно только читает. Запись в FAT — это отдельная история про
//! аллокацию кластеров, обновление обеих копий таблицы и FSInfo, и без журнала
//! любое падение посреди операции оставляет том в противоречивом состоянии.
//! Всё, что нужно этой фазе, — прочитать образ, собранный на хосте.
//!
//! # Что здесь считается внешними данными
//!
//! Всё, что пришло с носителя: поля BPB, номера кластеров, длины, записи
//! каталогов. Ни одно из них не используется без проверки границ, ни одна
//! структура не восстанавливается приведением указателя (выравнивание буфера
//! ничем не гарантировано) — только побайтовым чтением через `from_le_bytes`.
//! Поэтому во всём модуле нет ни одного `unsafe`.
//!
//! # Почему не крейт `fatfs`
//!
//! Опубликованная версия `fatfs` (0.3.6) в режиме `no_std` тянет `core_io` —
//! крейт, который работает только на конкретных ночных сборках и давно не
//! обновлялся; вариант с собственным трейтом `IoBase`, не требующий `core_io`,
//! живёт только в неопубликованной ветке. Плюс `fatfs` — это ФС на чтение и
//! запись поверх `Read + Write + Seek` с собственной буферизацией, то есть к
//! нему всё равно понадобился бы адаптер от [`BlockDevice`] и слой трансляции
//! ошибок, который схлопнул бы всё разнообразие [`VfsError`] в одну «io error».
//! Читающая половина FAT32 — это разбор BPB, обход цепочки и склейка LFN;
//! ровно то, что написано ниже, и ровно то, что мы обязаны проверять сами.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use crate::vfs::{
    BLOCK_SIZE, BlockDevice, DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult,
};

/// Размер записи каталога. Одинаков для короткой записи и для LFN.
const DIR_ENTRY_SIZE: usize = 32;

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;

/// Комбинация атрибутов, которой помечена запись длинного имени. Выбрана
/// авторами формата так, чтобы старые драйверы (знающие только 8.3) сочли
/// запись «скрытым системным томом только для чтения» и не показывали её.
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

/// Маска, по которой распознаётся LFN: сравнивать нужно младшие шесть бит, а не
/// весь байт — старшие два (`0x40`, `0x80`) зарезервированы и могут быть чем
/// угодно.
const ATTR_LONG_NAME_MASK: u8 = 0x3F;

/// Первый байт записи: каталог закончился, дальше смотреть незачем.
const ENTRY_END: u8 = 0x00;
/// Первый байт записи: запись удалена.
const ENTRY_FREE: u8 = 0xE5;
/// Настоящий первый байт имени, если в записи стоит `0x05`: значение `0xE5`
/// встречается как первый байт двухбайтовой японской кодировки и подменяется,
/// чтобы не выглядеть удалённой записью.
const ENTRY_KANJI_E5: u8 = 0x05;

/// Сколько символов имени несёт одна LFN-запись: 5 + 6 + 2, разнесённые по трём
/// кускам вокруг полей, которые старый драйвер читает как короткую запись.
const LFN_CHARS_PER_ENTRY: usize = 13;
/// Максимум LFN-записей на одно имя: 20 * 13 = 260 >= 255 символов предела.
const LFN_MAX_ENTRIES: usize = 20;
/// Бит в поле порядка, помечающий последнюю (то есть лежащую первой) запись.
const LFN_LAST: u8 = 0x40;
/// Смещения символов внутри LFN-записи.
const LFN_OFFSETS: [usize; LFN_CHARS_PER_ENTRY] =
    [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];

/// Значения элемента FAT: `>= 0x0FFF_FFF8` — конец цепочки.
const CLUSTER_EOC: u32 = 0x0FFF_FFF8;
/// Кластер помечен сбойным.
const CLUSTER_BAD: u32 = 0x0FFF_FFF7;
/// Значащих бит в элементе таблицы: четыре старших зарезервированы.
const FAT_ENTRY_MASK: u32 = 0x0FFF_FFFF;

/// Граница, ниже которой том — не FAT32. См. [`Layout::parse`].
const MIN_FAT32_CLUSTERS: u32 = 65525;

/// Потолок на число записей в одном каталоге. Спецификация ограничивает каталог
/// 2 МиБ, то есть 65536 записями; всё, что больше, — испорченная цепочка,
/// которую незачем дочитывать до конца.
const MAX_DIR_ENTRIES: u32 = 65_536;

/// Сколько секторов держим в кеше. Хватает: цепочка читается последовательно,
/// поэтому подряд идущие кластеры попадают в один и тот же сектор FAT (128
/// элементов на 512 байт), а данные полными секторами идут мимо кеша прямо в
/// буфер вызывающего.
const CACHE_SLOTS: usize = 4;

// -----------------------------------------------------------------------------
// Чтение полей из сырого буфера
// -----------------------------------------------------------------------------

fn u16_at(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn u32_at(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Буфер под данные с носителя. Размер приходит из образа, поэтому выделяется
/// через `try_reserve`: отказ кучи — это [`VfsError::OutOfMemory`], а не паника.
fn alloc_buf(len: usize) -> VfsResult<Vec<u8>> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len).map_err(|_| VfsError::OutOfMemory)?;
    buf.resize(len, 0);
    Ok(buf)
}

// -----------------------------------------------------------------------------
// Геометрия тома
// -----------------------------------------------------------------------------

/// Разобранный и проверенный BPB.
///
/// Все поля уже приведены к абсолютным номерам секторов и кластеров: дальше по
/// коду сырых значений из образа нет.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    /// Байт в кластере — произведение двух полей выше, посчитанное один раз.
    pub cluster_bytes: u32,
    /// Первый сектор активной копии FAT.
    pub fat_start: u32,
    pub fat_sectors: u32,
    pub num_fats: u32,
    /// Первый сектор области данных: с него начинается кластер номер 2.
    pub first_data_sector: u32,
    /// Число кластеров данных. Именно оно определяет тип FAT.
    pub total_clusters: u32,
    /// Наибольший допустимый номер кластера (`total_clusters + 1`, потому что
    /// нумерация данных начинается с двойки).
    pub max_cluster: u32,
    pub root_cluster: u32,
    /// Во сколько блоков [`BLOCK_SIZE`] превращается один сектор тома.
    pub blocks_per_sector: u32,
}

impl Geometry {
    /// Разобрать загрузочный сектор.
    ///
    /// # Как определяется, что это FAT32
    ///
    /// Не по строке `"FAT32   "` в поле `BS_FilSysType` — она информационная, её
    /// не обязаны заполнять правильно, и спецификация Microsoft прямо запрещает
    /// на неё опираться. Тип FAT определяется **числом кластеров данных**:
    ///
    /// ```text
    /// RootDirSectors = (RootEntCnt * 32 + BytsPerSec - 1) / BytsPerSec
    /// DataSectors    = TotSec - (RsvdSecCnt + NumFATs * FATSz + RootDirSectors)
    /// Clusters       = DataSectors / SecPerClus
    /// ```
    ///
    /// `Clusters < 4085` — FAT12, `< 65525` — FAT16, иначе FAT32. Границы
    /// именно такие (а не круглые `4096`/`65536`) — так исторически считает
    /// эталонная реализация, и любое «уточнение» разъезжается с чужими
    /// форматтерами на томах у самой границы.
    ///
    /// Дополнительно у настоящего FAT32 обязаны быть нулевыми `RootEntCnt` и
    /// `FATSz16`: корень здесь — обычная цепочка кластеров, а не выделенная
    /// область фиксированного размера.
    fn parse(boot: &[u8], device_blocks: u64) -> VfsResult<Self> {
        if boot.len() < BLOCK_SIZE {
            return Err(VfsError::Corrupt);
        }
        // Сигнатура и первая инструкция перехода: дешёвая отсечка «это вообще не
        // загрузочный сектор», до того как мы начнём верить его числам.
        if u16_at(boot, 510) != 0xAA55 {
            return Err(VfsError::Corrupt);
        }
        if !matches!(boot[0], 0xEB | 0xE9) {
            return Err(VfsError::Corrupt);
        }

        let bytes_per_sector = u32::from(u16_at(boot, 11));
        // Ограничение сверху не наше: спецификация допускает ровно эти четыре
        // размера. Кратность BLOCK_SIZE нужна, чтобы сектор тома складывался из
        // целого числа блоков устройства.
        if !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096) {
            return Err(VfsError::Corrupt);
        }
        let blocks_per_sector = bytes_per_sector / BLOCK_SIZE as u32;

        let sectors_per_cluster = u32::from(boot[13]);
        if sectors_per_cluster == 0
            || !sectors_per_cluster.is_power_of_two()
            || sectors_per_cluster > 128
        {
            return Err(VfsError::Corrupt);
        }
        // Обе величины уже ограничены сверху, поэтому кластер не превысит
        // 512 КиБ — важно, потому что именно столько составит буфер, который
        // выделяется под чтение каталога.
        let cluster_bytes = bytes_per_sector
            .checked_mul(sectors_per_cluster)
            .ok_or(VfsError::Corrupt)?;

        let reserved_sectors = u32::from(u16_at(boot, 14));
        if reserved_sectors == 0 {
            return Err(VfsError::Corrupt);
        }
        let num_fats = u32::from(boot[16]);
        if num_fats == 0 || num_fats > 8 {
            return Err(VfsError::Corrupt);
        }

        let root_entries = u32::from(u16_at(boot, 17));
        let fat_size_16 = u32::from(u16_at(boot, 22));
        let total_16 = u32::from(u16_at(boot, 19));

        // Оба поля имеют по «узкой» и «широкой» версии: нулевая узкая означает
        // «смотри широкую». Так FAT32 расширили, не сломав раскладку FAT16.
        let fat_sectors = if fat_size_16 != 0 { fat_size_16 } else { u32_at(boot, 36) };
        let total_sectors = if total_16 != 0 { total_16 } else { u32_at(boot, 32) };
        if fat_sectors == 0 || total_sectors == 0 {
            return Err(VfsError::Corrupt);
        }

        let root_dir_sectors = (root_entries * 32).div_ceil(bytes_per_sector);
        // В u64, потому что произведение u32 * u32 переполняется, а числа
        // приехали из образа и правдоподобными быть не обязаны.
        let meta_sectors = u64::from(reserved_sectors)
            + u64::from(num_fats) * u64::from(fat_sectors)
            + u64::from(root_dir_sectors);
        if meta_sectors >= u64::from(total_sectors) {
            return Err(VfsError::Corrupt);
        }
        let data_sectors = u64::from(total_sectors) - meta_sectors;
        let total_clusters = (data_sectors / u64::from(sectors_per_cluster)) as u32;

        if total_clusters < MIN_FAT32_CLUSTERS {
            // Структурно это может быть совершенно исправный FAT12/FAT16 —
            // просто не тот формат, который мы умеем.
            return Err(VfsError::Unsupported);
        }
        if root_entries != 0 || fat_size_16 != 0 {
            // Число кластеров говорит «FAT32», а поля — «FAT16». Заголовку
            // нельзя верить целиком, значит и остальным его полям тоже.
            return Err(VfsError::Corrupt);
        }

        // saturating: `total_clusters` пришло из образа и может быть каким
        // угодно, а переполнение здесь означало бы панику в разборе заголовка.
        let mut max_cluster = total_clusters.saturating_add(1);

        // Активная копия FAT. Обычно копии зеркалируются и годится любая, но
        // если в ExtFlags взведён бит 7, зеркалирование выключено и достоверна
        // ровно одна копия — та, чей номер лежит в младших четырёх битах.
        let ext_flags = u16_at(boot, 40);
        let active_fat = if ext_flags & 0x0080 != 0 { u32::from(ext_flags & 0x000F) } else { 0 };
        if active_fat >= num_fats {
            return Err(VfsError::Corrupt);
        }
        let fat_start = reserved_sectors
            .checked_add(active_fat.checked_mul(fat_sectors).ok_or(VfsError::Corrupt)?)
            .ok_or(VfsError::Corrupt)?;

        // Таблица обязана покрывать все кластеры. Если она короче заявленного,
        // мы не отказываем в монтировании, а сужаем диапазон допустимых
        // кластеров до того, что реально описано: так любое обращение за
        // границу таблицы становится обычным «нет такого кластера», а не
        // чтением соседних данных как элементов FAT.
        let fat_capacity = (u64::from(fat_sectors) * u64::from(bytes_per_sector) / 4)
            .min(u64::from(u32::MAX)) as u32;
        if fat_capacity < 3 {
            return Err(VfsError::Corrupt);
        }
        max_cluster = max_cluster.min(fat_capacity - 1);

        let first_data_sector = u32::try_from(meta_sectors).map_err(|_| VfsError::Corrupt)?;

        let root_cluster = u32_at(boot, 44) & FAT_ENTRY_MASK;
        if root_cluster < 2 || root_cluster > max_cluster {
            return Err(VfsError::Corrupt);
        }

        // Метаданные должны физически помещаться на устройство. Дальше границы
        // проверяет каждое чтение, но том, у которого уже FAT не помещается,
        // лучше отвергнуть сразу и внятно.
        let fat_end_blocks = (u64::from(fat_start) + u64::from(fat_sectors))
            .checked_mul(u64::from(blocks_per_sector));
        match fat_end_blocks {
            Some(end) if end <= device_blocks => {}
            _ => return Err(VfsError::Corrupt),
        }

        Ok(Self {
            bytes_per_sector,
            sectors_per_cluster,
            cluster_bytes,
            fat_start,
            fat_sectors,
            num_fats,
            first_data_sector,
            total_clusters,
            max_cluster,
            root_cluster,
            blocks_per_sector,
        })
    }
}

// -----------------------------------------------------------------------------
// Кеш секторов
// -----------------------------------------------------------------------------

struct CacheSlot {
    sector: Option<u64>,
    data: Vec<u8>,
}

/// Кеш на несколько секторов.
///
/// Нужен ровно за тем, чтобы обход цепочки и чтение по одному байту не
/// превращались в чтение сектора на каждое обращение. Вытеснение —
/// круговое: держать здесь LRU не за что, слотов всего [`CACHE_SLOTS`], а
/// сплошные куски данных идут мимо кеша прямо в буфер вызывающего.
struct SectorCache {
    slots: Vec<CacheSlot>,
    victim: usize,
}

impl SectorCache {
    fn new(sector_bytes: usize) -> VfsResult<Self> {
        let mut slots = Vec::new();
        slots.try_reserve_exact(CACHE_SLOTS).map_err(|_| VfsError::OutOfMemory)?;
        for _ in 0..CACHE_SLOTS {
            slots.push(CacheSlot { sector: None, data: alloc_buf(sector_bytes)? });
        }
        Ok(Self { slots, victim: 0 })
    }
}

// -----------------------------------------------------------------------------
// Том
// -----------------------------------------------------------------------------

struct Volume {
    device: Box<dyn BlockDevice>,
    geometry: Geometry,
    cache: SpinLock<SectorCache>,
}

impl Volume {
    /// Прочитать подряд идущие секторы тома прямо в `buf`, мимо кеша.
    fn read_sectors(&self, sector: u64, buf: &mut [u8]) -> VfsResult<()> {
        if buf.is_empty() || buf.len() % BLOCK_SIZE != 0 {
            return Err(VfsError::OutOfBounds);
        }
        let blocks = (buf.len() / BLOCK_SIZE) as u64;
        let start = sector
            .checked_mul(u64::from(self.geometry.blocks_per_sector))
            .ok_or(VfsError::OutOfBounds)?;
        let end = start.checked_add(blocks).ok_or(VfsError::OutOfBounds)?;
        if end > self.device.block_count() {
            return Err(VfsError::OutOfBounds);
        }
        self.device.read_blocks(start, buf)
    }

    /// Выполнить `f` над содержимым сектора, подняв его в кеш при промахе.
    ///
    /// `f` вызывается с удержанным замком, поэтому обязана быть короткой: это
    /// копирование нескольких байт, не более.
    fn with_sector<R>(&self, sector: u64, f: impl FnOnce(&[u8]) -> R) -> VfsResult<R> {
        let mut cache = self.cache.lock();

        if let Some(hit) = cache.slots.iter().position(|slot| slot.sector == Some(sector)) {
            return Ok(f(&cache.slots[hit].data));
        }

        let index = cache.victim;
        cache.victim = (index + 1) % CACHE_SLOTS;
        // Слот помечается пустым до чтения: если устройство откажет, в кеше не
        // должно остаться содержимое прошлого сектора под новым номером.
        cache.slots[index].sector = None;
        let mut buf = core::mem::take(&mut cache.slots[index].data);
        let result = self.read_sectors(sector, &mut buf);
        cache.slots[index].data = buf;
        result?;
        cache.slots[index].sector = Some(sector);
        Ok(f(&cache.slots[index].data))
    }

    /// Элемент таблицы FAT для кластера.
    fn fat_entry(&self, cluster: u32) -> VfsResult<u32> {
        if cluster < 2 || cluster > self.geometry.max_cluster {
            return Err(VfsError::Corrupt);
        }
        let offset = u64::from(cluster) * 4;
        let sector_bytes = u64::from(self.geometry.bytes_per_sector);
        let sector = u64::from(self.geometry.fat_start) + offset / sector_bytes;
        let within = (offset % sector_bytes) as usize;
        let raw = self.with_sector(sector, |data| u32_at(data, within))?;
        Ok(raw & FAT_ENTRY_MASK)
    }

    /// Следующий кластер цепочки: `None` — цепочка кончилась.
    fn next_cluster(&self, cluster: u32) -> VfsResult<Option<u32>> {
        let entry = self.fat_entry(cluster)?;
        if entry >= CLUSTER_EOC {
            return Ok(None);
        }
        // Свободный (0), зарезервированный (1), сбойный или выходящий за том
        // кластер внутри цепочки означает ровно одно: таблица испорчена.
        if entry < 2 || entry == CLUSTER_BAD || entry > self.geometry.max_cluster {
            return Err(VfsError::Corrupt);
        }
        Ok(Some(entry))
    }

    /// Начать обход цепочки с кластера `start`.
    ///
    /// `start == 0` — пустой файл: FAT записывает нулевой кластер там, где
    /// данных нет вовсе.
    fn chain(&self, start: u32) -> VfsResult<Chain<'_>> {
        let next = if start == 0 {
            None
        } else if start < 2 || start > self.geometry.max_cluster {
            return Err(VfsError::Corrupt);
        } else {
            Some(start)
        };
        let budget = self.geometry.total_clusters.saturating_add(1);
        Ok(Chain { volume: self, next, budget })
    }

    fn cluster_sector(&self, cluster: u32) -> VfsResult<u64> {
        if cluster < 2 || cluster > self.geometry.max_cluster {
            return Err(VfsError::Corrupt);
        }
        Ok(u64::from(self.geometry.first_data_sector)
            + u64::from(cluster - 2) * u64::from(self.geometry.sectors_per_cluster))
    }

    /// Прочитать кластер целиком. `buf` обязан быть размером с кластер.
    fn read_cluster(&self, cluster: u32, buf: &mut [u8]) -> VfsResult<()> {
        if buf.len() != self.geometry.cluster_bytes as usize {
            return Err(VfsError::OutOfBounds);
        }
        let sector = self.cluster_sector(cluster)?;
        self.read_sectors(sector, buf)
    }

    /// Прочитать кусок внутри одного кластера.
    ///
    /// Целые секторы читаются напрямую в `dst`; через кеш проходят только
    /// неполные края, ради которых и заведён кеш.
    fn read_in_cluster(&self, cluster: u32, offset: usize, dst: &mut [u8]) -> VfsResult<()> {
        let sector_bytes = self.geometry.bytes_per_sector as usize;
        if offset + dst.len() > self.geometry.cluster_bytes as usize {
            return Err(VfsError::OutOfBounds);
        }
        let base = self.cluster_sector(cluster)?;
        let mut sector = base + (offset / sector_bytes) as u64;
        let mut within = offset % sector_bytes;
        let mut done = 0usize;

        while done < dst.len() {
            let remaining = dst.len() - done;
            if within == 0 && remaining >= sector_bytes {
                let whole = remaining / sector_bytes;
                let bytes = whole * sector_bytes;
                self.read_sectors(sector, &mut dst[done..done + bytes])?;
                sector += whole as u64;
                done += bytes;
            } else {
                let chunk = core::cmp::min(sector_bytes - within, remaining);
                self.with_sector(sector, |data| {
                    dst[done..done + chunk].copy_from_slice(&data[within..within + chunk]);
                })?;
                sector += 1;
                done += chunk;
                within = 0;
            }
        }
        Ok(())
    }

    /// Прочитать файл по смещению. Возвращает, сколько байт реально прочитано:
    /// у конца файла это меньше запрошенного.
    fn read_file(
        &self,
        start_cluster: u32,
        size: u64,
        offset: u64,
        dst: &mut [u8],
    ) -> VfsResult<usize> {
        if offset >= size {
            return Ok(0);
        }
        let want = core::cmp::min(dst.len() as u64, size - offset) as usize;
        if want == 0 {
            return Ok(0);
        }

        let cluster_bytes = u64::from(self.geometry.cluster_bytes);
        let mut chain = self.chain(start_cluster)?;
        for _ in 0..(offset / cluster_bytes) {
            if chain.step()?.is_none() {
                // Цепочка короче, чем обещает размер в записи каталога.
                return Err(VfsError::Corrupt);
            }
        }

        let mut within = (offset % cluster_bytes) as usize;
        let mut done = 0usize;
        while done < want {
            let Some(cluster) = chain.step()? else {
                return Err(VfsError::Corrupt);
            };
            let take = core::cmp::min(self.geometry.cluster_bytes as usize - within, want - done);
            self.read_in_cluster(cluster, within, &mut dst[done..done + take])?;
            done += take;
            within = 0;
        }
        Ok(done)
    }

    /// Обойти записи каталога, начиная с кластера `start`.
    fn visit_dir<F>(&self, start: u32, mut visit: F) -> VfsResult<()>
    where
        F: FnMut(&RawEntry) -> VfsResult<Flow>,
    {
        let mut buf = alloc_buf(self.geometry.cluster_bytes as usize)?;
        let mut lfn = Lfn::new();
        let mut scanned: u32 = 0;
        let mut chain = self.chain(start)?;

        while let Some(cluster) = chain.step()? {
            self.read_cluster(cluster, &mut buf)?;
            for raw in buf.chunks_exact(DIR_ENTRY_SIZE) {
                scanned += 1;
                if scanned > MAX_DIR_ENTRIES {
                    return Err(VfsError::Corrupt);
                }
                match raw[0] {
                    ENTRY_END => return Ok(()),
                    ENTRY_FREE => {
                        // Удалённая запись рвёт набор LFN: то, что успело
                        // накопиться, к следующей короткой записи отношения
                        // не имеет.
                        lfn.reset();
                        continue;
                    }
                    _ => {}
                }

                let attr = raw[11];
                if attr & ATTR_LONG_NAME_MASK == ATTR_LONG_NAME {
                    lfn.push(raw);
                    continue;
                }

                let mut short = [0u8; 11];
                short.copy_from_slice(&raw[0..11]);
                let name = match lfn.finish(&short) {
                    Some(long) => long,
                    None => short_name(&short, raw[12]),
                };
                if name.is_empty() {
                    // Имя из одних пробелов не создаёт ни один форматтер;
                    // отдавать наружу безымянный узел — только путать
                    // вызывающего, который потом не сможет его найти.
                    continue;
                }

                let cluster = (u32::from(u16_at(raw, 20)) << 16) | u32::from(u16_at(raw, 26));
                let entry = RawEntry {
                    name,
                    short,
                    attr,
                    first_cluster: cluster & FAT_ENTRY_MASK,
                    size: u32_at(raw, 28),
                };
                if visit(&entry)? == Flow::Stop {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

/// Что делать после очередной записи каталога.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// Обход цепочки кластеров.
///
/// # Защита от зацикливания
///
/// Испорченная таблица легко описывает цикл (`A -> B -> A`) или петлю на себя,
/// и наивный обход в таком случае крутится вечно — то есть вешает ядро без
/// единого сообщения. Поэтому у обхода есть бюджет шагов: исправная цепочка не
/// может быть длиннее, чем всего кластеров на томе, а всё сверх этого — заведомо
/// цикл. Счётчик дешевле «зайца и черепахи»: тот потребовал бы вдвое больше
/// обращений к FAT ради того же результата.
struct Chain<'a> {
    volume: &'a Volume,
    next: Option<u32>,
    budget: u32,
}

impl Chain<'_> {
    /// Вернуть текущий кластер и перейти к следующему.
    fn step(&mut self) -> VfsResult<Option<u32>> {
        let Some(current) = self.next else {
            return Ok(None);
        };
        if self.budget == 0 {
            return Err(VfsError::Corrupt);
        }
        self.budget -= 1;
        self.next = self.volume.next_cluster(current)?;
        Ok(Some(current))
    }
}

/// Разобранная запись каталога.
struct RawEntry {
    /// Длинное имя, если оно было и прошло проверку; иначе — короткое, 8.3.
    name: String,
    /// Короткое имя как оно лежит на диске, без точки и с добивкой пробелами.
    short: [u8; 11],
    attr: u8,
    first_cluster: u32,
    size: u32,
}

impl RawEntry {
    fn is_dir(&self) -> bool {
        self.attr & ATTR_DIRECTORY != 0
    }

    fn is_volume_label(&self) -> bool {
        self.attr & ATTR_VOLUME_ID != 0
    }

    fn kind(&self) -> NodeKind {
        if self.is_dir() { NodeKind::Directory } else { NodeKind::File }
    }

    /// Размер: у каталогов поле размера обязано быть нулевым, и полагаться на
    /// него нельзя — длину каталога задаёт цепочка.
    fn size(&self) -> u64 {
        if self.is_dir() { 0 } else { u64::from(self.size) }
    }

    fn is_dot(&self) -> bool {
        &self.short == b".          " || &self.short == b"..         "
    }
}

// -----------------------------------------------------------------------------
// Имена
// -----------------------------------------------------------------------------

/// Контрольная сумма короткого имени — та самая, что дублируется в каждой
/// LFN-записи набора.
///
/// Смысл в том, что LFN-записи физически отделимы от своей короткой: старый
/// драйвер, ничего не знающий о длинных именах, мог удалить или переименовать
/// файл, оставив сиротские LFN лежать рядом. Сумма связывает набор с конкретным
/// коротким именем: не сойдётся — значит, эти записи не от него, и склеивать их
/// в имя нельзя.
fn short_checksum(short: &[u8; 11]) -> u8 {
    let mut sum: u8 = 0;
    for &byte in short {
        sum = sum.rotate_right(1).wrapping_add(byte);
    }
    sum
}

/// Собрать `NAME.EXT` из записи 8.3.
fn short_name(short: &[u8; 11], nt_flags: u8) -> String {
    let mut base_len = 8;
    while base_len > 0 && short[base_len - 1] == b' ' {
        base_len -= 1;
    }
    let mut ext_len = 3;
    while ext_len > 0 && short[8 + ext_len - 1] == b' ' {
        ext_len -= 1;
    }

    // Windows не заводит LFN для имён, которые целиком помещаются в 8.3 и
    // отличаются только регистром: вместо этого он взводит биты в
    // зарезервированном байте — 0x08 «имя строчными», 0x10 «расширение
    // строчными». Без их учёта `makefile` показался бы как `MAKEFILE`.
    let lower_base = nt_flags & 0x08 != 0;
    let lower_ext = nt_flags & 0x10 != 0;

    let mut name = String::new();
    for (index, &byte) in short[..base_len].iter().enumerate() {
        let byte = if index == 0 && byte == ENTRY_KANJI_E5 { 0xE5 } else { byte };
        name.push(oem_char(byte, lower_base));
    }
    if ext_len > 0 {
        name.push('.');
        for &byte in &short[8..8 + ext_len] {
            name.push(oem_char(byte, lower_ext));
        }
    }
    name
}

/// Байт короткого имени как символ.
///
/// Короткие имена записаны в OEM-кодировке (обычно CP437), которая совпадает с
/// ASCII только в младшей половине. Таблицы перекодировки в ядре нет и заводить
/// её ради имён, у которых почти всегда есть LFN, незачем: старший диапазон
/// отдаётся как U+FFFD, чтобы имя оставалось валидным UTF-8 и было видно, что
/// символ не распознан.
fn oem_char(byte: u8, lowercase: bool) -> char {
    if byte < 0x80 {
        let ch = char::from(byte);
        if lowercase { ch.to_ascii_lowercase() } else { ch }
    } else {
        core::char::REPLACEMENT_CHARACTER
    }
}

/// Накопитель длинного имени.
///
/// LFN-записи лежат **перед** своей короткой и в обратном порядке: первой идёт
/// последняя часть имени (с взведённым битом [`LFN_LAST`]), последней — первая.
/// Накопитель складывает части по их порядковым номерам и отдаёт имя только
/// тогда, когда набор пришёл целиком, без разрывов и с сошедшейся контрольной
/// суммой.
struct Lfn {
    parts: [[u16; LFN_CHARS_PER_ENTRY]; LFN_MAX_ENTRIES],
    checksum: u8,
    /// Порядковый номер последней записи — он же число частей в имени.
    top: u8,
    /// Какой номер обязан прийти следующим. Ноль означает «набор полон».
    expect: u8,
    active: bool,
}

impl Lfn {
    fn new() -> Self {
        Self {
            parts: [[0; LFN_CHARS_PER_ENTRY]; LFN_MAX_ENTRIES],
            checksum: 0,
            top: 0,
            expect: 0,
            active: false,
        }
    }

    fn reset(&mut self) {
        self.active = false;
    }

    fn push(&mut self, raw: &[u8]) {
        let order = raw[0];
        // Из байта порядка вычитается только бит «последняя запись». Остальные
        // старшие биты не определены форматом, и трактовать их как часть номера
        // — способ отвергнуть запись, в которой они взведены, вместо того чтобы
        // молча принять её за соседний номер.
        let index = order & !LFN_LAST;
        if index == 0 || usize::from(index) > LFN_MAX_ENTRIES {
            self.active = false;
            return;
        }

        if order & LFN_LAST != 0 {
            // Начало набора: всё, что накопилось раньше, было мусором.
            self.active = true;
            self.checksum = raw[13];
            self.top = index;
            self.expect = index;
        }
        if !self.active {
            // Часть без начала набора — сирота, склеивать нечего.
            return;
        }
        // Номера обязаны идти строго по убыванию без пропусков, тип записи —
        // быть нулевым, а контрольная сумма — одинаковой во всём наборе.
        if index != self.expect || raw[13] != self.checksum || raw[12] != 0 {
            self.active = false;
            return;
        }

        let slot = &mut self.parts[usize::from(index) - 1];
        for (position, &offset) in LFN_OFFSETS.iter().enumerate() {
            slot[position] = u16_at(raw, offset);
        }
        self.expect = index - 1;
    }

    /// Забрать накопленное имя, если оно принадлежит этой короткой записи.
    ///
    /// Вызывается на каждой короткой записи, в том числе когда LFN не было:
    /// накопитель обязан сбрасываться независимо от результата.
    fn finish(&mut self, short: &[u8; 11]) -> Option<String> {
        let complete = self.active && self.expect == 0;
        self.active = false;
        if !complete || self.checksum != short_checksum(short) {
            return None;
        }

        let mut units: Vec<u16> = Vec::new();
        'outer: for part in &self.parts[..usize::from(self.top)] {
            for &unit in part {
                // 0x0000 — конец имени, 0xFFFF — добивка хвоста последней
                // записи. Имя длиной ровно кратной тринадцати не имеет ни того,
                // ни другого и просто заканчивается вместе с набором.
                if unit == 0x0000 || unit == 0xFFFF {
                    break 'outer;
                }
                units.push(unit);
            }
        }
        if units.is_empty() {
            return None;
        }

        let mut name = String::new();
        for decoded in core::char::decode_utf16(units) {
            // Непарный суррогат в UCS-2 с диска — не повод отказывать в имени,
            // но и осмысленного символа из него не выйдет.
            name.push(decoded.unwrap_or(core::char::REPLACEMENT_CHARACTER));
        }
        Some(name)
    }
}

/// Сравнение имён без учёта регистра.
///
/// FAT нечувствителен к регистру, но его собственная таблица приведения зависит
/// от кодовой страницы тома. Здесь складывается только ASCII — остальное
/// сравнивается посимвольно как есть; для латинских имён, которыми пользуется
/// загрузочный раздел, этого достаточно.
fn names_eq(left: &str, right: &str) -> bool {
    let mut left = left.chars();
    let mut right = right.chars();
    loop {
        match (left.next(), right.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => {}
            _ => return false,
        }
    }
}

// -----------------------------------------------------------------------------
// Публичный интерфейс
// -----------------------------------------------------------------------------

/// Смонтированный том FAT32, только для чтения.
pub struct Fat32 {
    volume: Arc<Volume>,
}

impl Fat32 {
    /// Смонтировать том поверх блочного устройства.
    ///
    /// Читает и проверяет загрузочный сектор; ничего больше при монтировании не
    /// трогается, так что стоимость — одно чтение.
    pub fn mount(device: Box<dyn BlockDevice>) -> VfsResult<Self> {
        if device.block_count() == 0 {
            return Err(VfsError::OutOfBounds);
        }
        let mut boot = [0u8; BLOCK_SIZE];
        device.read_blocks(0, &mut boot)?;

        let geometry = Geometry::parse(&boot, device.block_count())?;
        let cache = SectorCache::new(geometry.bytes_per_sector as usize)?;

        Ok(Self { volume: Arc::new(Volume { device, geometry, cache: SpinLock::new(cache) }) })
    }

    /// Разобранная геометрия тома — для диагностики при загрузке.
    #[must_use]
    pub fn geometry(&self) -> Geometry {
        self.volume.geometry
    }

    /// Метка тома из записи корневого каталога, если она там есть.
    ///
    /// Именно из каталога, а не из поля `BS_VolLab` в BPB: последнее пишется при
    /// форматировании и после переименования тома остаётся старым.
    #[must_use]
    pub fn label(&self) -> Option<String> {
        let mut label: Option<String> = None;
        let scan = self.volume.visit_dir(self.volume.geometry.root_cluster, |entry| {
            if entry.is_volume_label() && !entry.is_dir() {
                let mut text = String::new();
                for &byte in &entry.short {
                    text.push(oem_char(byte, false));
                }
                label = Some(String::from(text.trim_end()));
                return Ok(Flow::Stop);
            }
            Ok(Flow::Continue)
        });
        // Метка — украшение; неудачное чтение не повод шуметь наружу.
        scan.ok().and(label)
    }
}

impl FileSystem for Fat32 {
    fn name(&self) -> &'static str {
        "FAT32"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        Ok(Box::new(FatNode {
            volume: Arc::clone(&self.volume),
            kind: NodeKind::Directory,
            first_cluster: self.volume.geometry.root_cluster,
            size: 0,
        }))
    }
}

/// Файл или каталог на томе FAT32.
pub struct FatNode {
    volume: Arc<Volume>,
    kind: NodeKind,
    first_cluster: u32,
    size: u64,
}

impl FatNode {
    fn from_entry(volume: &Arc<Volume>, entry: &RawEntry) -> Self {
        let kind = entry.kind();
        // У записи `..`, ведущей в корень, номер кластера записан нулём — так
        // повелось с FAT12/16, где корень лежал вне области данных и номера не
        // имел вовсе.
        let first_cluster = if kind == NodeKind::Directory && entry.first_cluster == 0 {
            volume.geometry.root_cluster
        } else {
            entry.first_cluster
        };
        Self { volume: Arc::clone(volume), kind, first_cluster, size: entry.size() }
    }
}

impl Node for FatNode {
    fn metadata(&self) -> Metadata {
        Metadata::defaults(self.kind, self.size)
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.kind != NodeKind::File {
            return Err(VfsError::WrongKind);
        }
        self.volume.read_file(self.first_cluster, self.size, offset, buf)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        if self.kind != NodeKind::Directory {
            return Err(VfsError::WrongKind);
        }
        let mut entries = Vec::new();
        self.volume.visit_dir(self.first_cluster, |entry| {
            // Метка тома — не файл, а `.` и `..` вызывающий и так знает; они
            // остаются доступными через `lookup`, но в перечислении только
            // мешали бы.
            if entry.is_volume_label() || entry.is_dot() {
                return Ok(Flow::Continue);
            }
            entries
                .try_reserve(1)
                .map_err(|_| VfsError::OutOfMemory)?;
            // Права подставляются значениями по умолчанию: FAT32 их не
            // хранит. См. `Metadata::defaults` — там же объяснено, почему поля
            // всё равно есть.
            let defaults = crate::vfs::Metadata::defaults(entry.kind(), entry.size());
            entries.push(DirEntry {
                name: entry.name.clone(),
                kind: entry.kind(),
                size: entry.size(),
                mode: defaults.mode,
                uid: defaults.uid,
                gid: defaults.gid,
            });
            Ok(Flow::Continue)
        })?;
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        if self.kind != NodeKind::Directory {
            return Err(VfsError::WrongKind);
        }
        if name.is_empty() || name.contains('/') {
            return Err(VfsError::BadPath);
        }

        let mut found = None;
        self.volume.visit_dir(self.first_cluster, |entry| {
            if entry.is_volume_label() {
                return Ok(Flow::Continue);
            }
            // Сравнение идёт и с длинным именем, и с коротким: файл `readme.txt`
            // должен находиться и по нему, и по `README~1.TXT`, под которым его
            // видит драйвер без поддержки LFN.
            let matches = names_eq(&entry.name, name)
                || names_eq(&short_name(&entry.short, 0), name);
            if matches {
                found = Some(FatNode::from_entry(&self.volume, entry));
                return Ok(Flow::Stop);
            }
            Ok(Flow::Continue)
        })?;

        match found {
            Some(node) => Ok(Box::new(node)),
            None => Err(VfsError::NotFound),
        }
    }
}
