//! Запись в существующий том btrfs.
//!
//! # Транзакция перестраивает тронутые деревья целиком
//!
//! Ядро Linux пишет в btrfs инкрементально: тронутый лист копируется на новое
//! место, за ним — путь до корня, а запись об этом в дереве экстентов
//! откладывается, потому что она сама меняет дерево экстентов и порождает новые
//! копии. Это десятки тысяч строк, и заметная часть их — о том, как не уйти в
//! бесконечную рекурсию.
//!
//! Здесь сделано иначе, и нарочно. Метаданные тома целиком лежат в памяти,
//! операции меняют их там, а фиксация раскладывает каждое тронутое дерево в узлы
//! **заново** и пишет их на свободное место. Дерево экстентов, дерево свободного
//! места и дерево корней не правятся по ходу, а вычисляются из результата.
//! Рекурсия превращается в подбор неподвижной точки: число узлов дерева
//! экстентов зависит от числа узлов вообще, и два-три круга делают его
//! устойчивым.
//!
//! Цена названа: каждая фиксация переписывает тронутые деревья целиком, а
//! метаданные всего тома держатся в памяти. На разделе состояния — `/etc` и
//! домашние каталоги, сотни килобайт метаданных — это десятки узлов на
//! транзакцию. На томе с миллионом файлов так было бы нельзя, и тогда придётся
//! писать инкрементальную запись. Но проверять её нечем лучше, чем эту: здесь
//! ошибка в дереве экстентов не может накопиться — оно каждый раз строится из
//! того, что есть.
//!
//! # Что гарантирует порядок записи
//!
//! Ни один блок, на который ссылается последний записанный суперблок, не
//! перезаписывается до следующего суперблока: данные и узлы идут только на
//! место, свободное в старом состоянии, а старые узлы остаются занятыми до
//! конца фиксации. Обрыв питания в любой момент оставляет прежний том или новый.
//!
//! 1. Данные файла пишутся сразу, при операции.
//! 2. Фиксация пишет узлы, затем `flush`.
//! 3. Суперблок — основной, затем копии, затем `flush`.
//!
//! # С какими томами писатель не работает
//!
//! С любыми, где перестройка деревьев разрушила бы то, чего мы не понимаем:
//! подтома и снимки (узлы там общие), квоты, журнал недописанной транзакции,
//! дублированные и RAID-куски, тома без дерева свободного места. Каждый случай —
//! [`Error::Unsupported`] при открытии, а не порча при фиксации.

use alloc::borrow::Cow;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use disk::BlockDevice;

use crate::chunk::ChunkMap;
use crate::crc32c;
use crate::layout::*;
use crate::node::{self, Stamp};
use crate::read::{DirEntry, Inode, parse_dir_item, read_superblock};
use crate::tree::Volume;
use crate::{Error, Result, try_zeroed};

const MIB: u64 = 1024 * 1024;

/// Предел встраивания данных в лист — умолчание ядра Linux (`max_inline`).
const MAX_INLINE: usize = 2048;

/// Наибольший экстент данных — тот же предел, что у ядра Linux.
const MAX_EXTENT: u64 = 128 * MIB;

/// Сколько секторов накрывает один элемент дерева сумм.
///
/// Своё число: элемент в 4 КиБ занимает четверть листа, и лист не превращается
/// в один гигантский элемент, который не во что расщепить.
const CSUM_ITEM_SECTORS: u64 = 1024;

/// Сколько кругов подбора раскладки допускается.
///
/// Сходится за два-три; предел нужен, чтобы ошибка в подсчёте кончалась
/// отказом, а не зависанием.
const MAX_ROUNDS: usize = 16;

const MAX_DEPTH: usize = 8;

/// Отрезок, который распределитель обходит вокруг копии суперблока.
///
/// Дерево свободного места копию **не** исключает — проверено на томе, где
/// кусок данных накрыл 64 МиБ: ядро записало весь кусок свободным и исключает
/// копию в памяти. Значит, обходить её обязан тот, кто раздаёт место.
const STRIPE_LEN: u64 = 64 * 1024;

/// Деревья, которые писатель умеет перестраивать или хотя бы не трогать.
const WRITABLE_TREES: [u64; 7] = [
    objectid::EXTENT_TREE,
    objectid::DEV_TREE,
    objectid::FS_TREE,
    objectid::CSUM_TREE,
    objectid::UUID_TREE,
    objectid::FREE_SPACE_TREE,
    objectid::DATA_RELOC_TREE,
];

type Items = Vec<(Key, Vec<u8>)>;

/// Владелец, права и время нового файла или каталога.
#[derive(Debug, Clone, Copy)]
pub struct Attributes {
    /// Права: `rwxrwxrwx` и три старших бита.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    /// Секунды эпохи Unix.
    pub time: u64,
}

/// Одно дерево в памяти.
struct Tree {
    id: u64,
    /// Все элементы, по возрастанию ключа.
    items: Items,
    /// Узлы, из которых дерево состоит на диске сейчас, по уровням (0 —
    /// листья) и внутри уровня по ключу. На верхнем уровне один узел — корень.
    levels: Vec<Vec<Placed>>,
    dirty: bool,
    /// Листья подгружаются по требованию (31.10, дерево ФС): в `items` —
    /// элементы только тех листьев, у которых `Placed::loaded`.
    paged: bool,
}

impl Tree {
    fn node_count(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }
}

/// Узел на диске — то, что фиксации нужно, чтобы оставить его на месте.
///
/// С 30.10 фиксация пишет не дерево целиком, а только узлы, чьё содержимое
/// изменилось: элементы раскладываются по прежним границам листьев, и лист,
/// чьи элементы совпали с прежними (по числу и хешу), остаётся на своём
/// адресе со своим поколением и своей записью в дереве экстентов. Внутренний
/// узел остаётся, если остались все его потомки. Rider и Debian на томе — это
/// тысячи листьев, а правка трогает единицы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Placed {
    address: u64,
    first: Key,
    generation: u64,
    /// Хеш содержимого: элементы листа или указатели внутреннего узла.
    hash: u64,
    count: usize,
    /// Элементы листа лежат в `Tree::items` (у внутренних узлов — всегда).
    /// Неподгруженный лист заведомо не тронут: на месте при любой фиксации,
    /// `hash` и `count` у него не известны.
    loaded: bool,
}

/// Узел новой раскладки: прежний или новый (диапазон элементов у листа,
/// диапазон узлов уровня ниже у внутреннего).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Kept(Placed),
    New { start: usize, end: usize },
}

/// Раскладка дерева по уровням; на верхнем один узел.
type Layout = Vec<Vec<Slot>>;

/// Группа блоков: кусок, отданный под один вид содержимого.
#[derive(Clone, Copy)]
struct Group {
    start: u64,
    length: u64,
    kind: u64,
}

/// Раскладка фиксации, посчитанная до первой записи.
struct Plan<'a> {
    rebuilt: Vec<usize>,
    /// Раскладка каждого перестраиваемого дерева.
    layouts: Vec<Layout>,
    /// Адреса **новых** узлов каждого дерева: уровень за уровнем, по ключу.
    placed: Vec<Vec<(u64, u8)>>,
    /// Корень каждого дерева: адрес, уровень, поколение.
    tops: Vec<(u64, u8, u64)>,
    /// Новое дерево экстентов — взглядом на прежнее ([`derive_extent`]).
    extent: View<'a>,
    /// Адреса заменённых узлов, по возрастанию: их записи ушли из `extent`.
    replaced: Vec<u64>,
    free_space: Items,
    root: Items,
    used: u64,
}

/// Том, открытый на запись.
pub struct Writer {
    first_lba: u64,
    /// Суперблок последней завершённой транзакции, байты целиком: поля, которых
    /// мы не понимаем, переносятся в следующий как есть.
    superblock: Vec<u8>,
    generation: u64,
    nodesize: u32,
    sectorsize: u32,
    fsid: [u8; 16],
    chunk_uuid: [u8; 16],
    total_bytes: u64,
    chunks: ChunkMap,
    trees: Vec<Tree>,
    next_inode: u64,
    /// Время последней операции — оно же время изменения подтома.
    time: u64,
    /// Операция сорвалась на середине, и память больше не описывает ничего
    /// согласованного. Такая транзакция не доедет до диска.
    broken: bool,
    /// Группы блоков и занятые отрезки для размещения данных (см.
    /// [`Writer::allocate_data`]); `None` — построить заново.
    space: Option<(Vec<Group>, Vec<(u64, u64)>)>,
    /// Inode, чьи элементы `EXTENT_DATA` менялись с прошлой фиксации, и
    /// экстенты, на которые они ссылались до первой правки (см.
    /// [`Writer::settle_data`]).
    touched: BTreeMap<u64, Vec<u64>>,
    /// Порог выгрузки дерева ФС после фиксации ([`LOADED_ITEMS_LIMIT`];
    /// тесты ставят меньше, чтобы проверить выгрузку и подгрузку заново).
    loaded_limit: usize,
}

impl Writer {
    /// Открыть том на запись: прочитать и проверить все его метаданные.
    pub fn open(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Self> {
        disk::check_device(dev)?;
        let superblock = read_superblock(dev, first_lba)?;
        let sb = &superblock;

        let sectorsize = u32_at(sb, SB_SECTORSIZE);
        let nodesize = u32_at(sb, SB_NODESIZE);
        if !sectorsize.is_power_of_two()
            || !nodesize.is_power_of_two()
            || sectorsize < 512
            || nodesize < sectorsize
            || nodesize > 64 * 1024
        {
            return Err(Error::Corrupt);
        }
        if sectorsize % dev.sector_size() != 0 {
            return Err(Error::Unsupported);
        }
        // Дерево свободного места обязано быть и быть согласованным: мы
        // перестраиваем его с нуля, и том, где место учитывается по-старому
        // (файлами кэша), после нас нёс бы два противоречащих учёта.
        if u16_at(sb, SB_CSUM_TYPE) != CSUM_TYPE_CRC32C
            || u64_at(sb, SB_INCOMPAT_FLAGS) & !INCOMPAT_SUPPORTED != 0
            || u64_at(sb, SB_NUM_DEVICES) != 1
            || u64_at(sb, SB_COMPAT_RO_FLAGS)
                != COMPAT_RO_FREE_SPACE_TREE | COMPAT_RO_FREE_SPACE_TREE_VALID
            // Недописанная транзакция в журнале — это чужие данные, которые
            // ядро Linux доиграет при монтировании. Перестроив деревья поверх,
            // мы бы их потеряли.
            || u64_at(sb, SB_LOG_ROOT) != 0
        {
            return Err(Error::Unsupported);
        }

        let generation = u64_at(sb, SB_GENERATION);
        let mut fsid = [0u8; 16];
        fsid.copy_from_slice(&sb[SB_FSID..SB_FSID + 16]);
        let mut volume =
            Volume::new(ChunkMap::new(), nodesize, sectorsize, fsid, generation, first_lba);
        volume.chunks.load_system_array(sb)?;

        let chunk_root = u64_at(sb, SB_CHUNK_ROOT);
        let chunk_tree =
            load_tree(&mut volume, dev, objectid::CHUNK_TREE, chunk_root, sb[SB_CHUNK_ROOT_LEVEL])?;
        for (key, raw) in &chunk_tree.items {
            if key.objectid != objectid::FIRST_CHUNK_TREE || key.kind != item_type::CHUNK_ITEM {
                continue;
            }
            if raw.len() < CHUNK_HEAD_SIZE {
                return Err(Error::Corrupt);
            }
            // Читатель берёт первую копию `dup`, а писатель обязан писать обе.
            // Пока он этого не умеет — отказ, а не том с одной свежей копией.
            if u64_at(raw, CHUNK_TYPE) & BLOCK_GROUP_PROFILE_MASK != 0 {
                return Err(Error::Unsupported);
            }
            volume.chunks.add(key.offset, raw)?;
        }
        let mut chunk_uuid = [0u8; 16];
        chunk_uuid.copy_from_slice(
            &volume.read_node(dev, chunk_root)?[HDR_CHUNK_TREE_UUID..HDR_CHUNK_TREE_UUID + 16],
        );

        let root_tree =
            load_tree(&mut volume, dev, objectid::ROOT_TREE, u64_at(sb, SB_ROOT), sb[SB_ROOT_LEVEL])?;
        let mut trees = Vec::new();
        trees.try_reserve(WRITABLE_TREES.len() + 2).map_err(|_| Error::NoMemory)?;
        for (key, raw) in &root_tree.items {
            if key.kind != item_type::ROOT_ITEM {
                continue;
            }
            // Подтом, снимок или квоты: у них общие с другими деревьями узлы,
            // и перестройка одного дерева разрушила бы соседнее.
            if key.offset != 0 || !WRITABLE_TREES.contains(&key.objectid) {
                return Err(Error::Unsupported);
            }
            if raw.len() < ROOT_ITEM_SIZE {
                return Err(Error::Unsupported);
            }
            // Дерево ФС — без листьев, по требованию: на томе с Debian и
            // Rider его элементы — сотни мегабайт кучи ядра.
            let load = if key.objectid == objectid::FS_TREE { load_tree_paged } else { load_tree };
            let tree = load(&mut volume, dev, key.objectid, u64_at(raw, ROOT_ITEM_BYTENR), raw[ROOT_ITEM_LEVEL])?;
            trees.push(tree);
        }
        trees.push(chunk_tree);
        trees.push(root_tree);

        let total_bytes = u64_at(sb, SB_TOTAL_BYTES);
        let writer = Self {
            first_lba,
            superblock,
            generation,
            nodesize,
            sectorsize,
            fsid,
            chunk_uuid,
            total_bytes,
            chunks: volume.chunks,
            trees,
            next_inode: 0,
            time: 0,
            broken: false,
            space: None,
            touched: BTreeMap::new(),
            loaded_limit: LOADED_ITEMS_LIMIT,
        };
        writer.validate()?;

        // Наибольший номер inode — в последнем листе, начатом номером из
        // обычного диапазона (за ним — особые объекты с огромными номерами).
        let mut writer = writer;
        let last_first = {
            let fs = writer.tree(objectid::FS_TREE)?;
            fs.levels[0]
                .iter()
                .rev()
                .find(|leaf| leaf.first.objectid <= objectid::LAST_FREE)
                .map(|leaf| leaf.first)
        };
        if let Some(first) = last_first {
            writer.ensure_range(dev, first, Key::new(objectid::LAST_FREE + 1, 0, 0))?;
        }
        let highest = writer
            .tree(objectid::FS_TREE)?
            .items
            .iter()
            .map(|(key, _)| key.objectid)
            .filter(|number| (objectid::FIRST_FREE..=objectid::LAST_FREE).contains(number))
            .max()
            .unwrap_or(objectid::FIRST_FREE);
        writer.next_inode = highest + 1;
        Ok(writer)
    }

    /// Убедиться, что в дереве экстентов нет ничего, что перестройка испортит.
    fn validate(&self) -> Result<()> {
        for id in [
            objectid::EXTENT_TREE,
            objectid::DEV_TREE,
            objectid::FS_TREE,
            objectid::CSUM_TREE,
            objectid::FREE_SPACE_TREE,
        ] {
            self.tree(id)?;
        }
        for (key, raw) in &self.tree(objectid::EXTENT_TREE)?.items {
            match key.kind {
                // Узел, на который ссылается больше одного дерева, или ссылка
                // «по родителю» — признак снимков. Выбросив запись о нём при
                // перестройке своего дерева, мы бы освободили чужой узел.
                item_type::METADATA_ITEM => {
                    if raw.len() < EXTENT_ITEM_SIZE
                        || u64_at(raw, EXTENT_ITEM_REFS) != 1
                        || u64_at(raw, EXTENT_ITEM_FLAGS) != EXTENT_FLAG_TREE_BLOCK
                    {
                        return Err(Error::Unsupported);
                    }
                }
                // Длинная запись об узле — том без `skinny metadata`. У
                // экстента данных все ссылки обязаны быть встроенными и вести
                // в дерево ФС: ссылки отдельными элементами и ссылки «через
                // родителя» появляются от клонов и снимков, а ссылки мы
                // выводим заново при каждой фиксации.
                item_type::EXTENT_ITEM => {
                    if raw.len() < EXTENT_ITEM_SIZE
                        || u64_at(raw, EXTENT_ITEM_FLAGS) != EXTENT_FLAG_DATA
                    {
                        return Err(Error::Unsupported);
                    }
                    data_refs(raw)?;
                }
                item_type::TREE_BLOCK_REF
                | item_type::SHARED_BLOCK_REF
                | item_type::EXTENT_DATA_REF
                | item_type::SHARED_DATA_REF => {
                    return Err(Error::Unsupported);
                }
                item_type::BLOCK_GROUP_ITEM => {
                    if raw.len() < BLOCK_GROUP_ITEM_SIZE {
                        return Err(Error::Corrupt);
                    }
                    let flags = u64_at(raw, BLOCK_GROUP_FLAGS);
                    let kind = flags & BLOCK_GROUP_TYPE_MASK;
                    if flags & BLOCK_GROUP_PROFILE_MASK != 0
                        || !matches!(
                            kind,
                            BLOCK_GROUP_DATA | BLOCK_GROUP_METADATA | BLOCK_GROUP_SYSTEM
                        )
                    {
                        return Err(Error::Unsupported);
                    }
                }
                _ => {}
            }
        }
        // Сжатый или шифрованный экстент нельзя переписать по сектору, а
        // длинные обратные ссылки (сотни жёстких ссылок) мы не разбираем.
        // Подгружаемые листья проверяются при подгрузке.
        if self.tree(objectid::FS_TREE)?.items.iter().any(|(key, raw)| unsupported_fs_item(key, raw)) {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    /// Номер последней завершённой транзакции.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    // --- подгрузка листьев дерева ФС ---------------------------------------------

    /// Узел дерева по логическому адресу — с носителя, со сверкой суммы,
    /// адреса и тома (без кэша: фиксация переиспользует адреса).
    fn read_node(&self, dev: &mut dyn BlockDevice, address: u64) -> Result<Vec<u8>> {
        let (physical, run) = self.chunks.translate(address)?;
        let size = self.nodesize as usize;
        let sector = u64::from(dev.sector_size());
        if run < size as u64 || physical % sector != 0 {
            return Err(Error::Corrupt);
        }
        let mut node = try_zeroed(size)?;
        dev.read(self.first_lba + physical / sector, &mut node)?;
        if u32_at(&node, 0) != crc32c::checksum(&node[CSUM_SIZE..]) {
            return Err(Error::BadChecksum);
        }
        if u64_at(&node, HDR_BYTENR) != address || node[HDR_FSID..HDR_FSID + 16] != self.fsid {
            return Err(Error::Corrupt);
        }
        Ok(node)
    }

    /// Подгрузить листья дерева ФС, задевающие ключи `[from, to)`. Всякая
    /// правка и всякое чтение дерева ФС идут после этого — по номерам
    /// объектов, которых они касаются ([`Writer::ensure_objects`]).
    fn ensure_range(&mut self, dev: &mut dyn BlockDevice, from: Key, to: Key) -> Result<()> {
        let index = self.tree_index(objectid::FS_TREE)?;
        if !self.trees[index].paged {
            return Ok(());
        }
        let mut wanted = Vec::new();
        {
            let leaves = &self.trees[index].levels[0];
            let first = leaves.partition_point(|leaf| leaf.first <= from).saturating_sub(1);
            for (n, leaf) in leaves.iter().enumerate().skip(first) {
                if n > first && leaf.first >= to {
                    break;
                }
                if !leaf.loaded {
                    push(&mut wanted, n)?;
                }
            }
        }
        for n in wanted {
            let placed = self.trees[index].levels[0][n];
            let node = self.read_node(dev, placed.address)?;
            let header = Header::parse(&node)?;
            if header.level != 0 || header.generation != placed.generation {
                return Err(Error::Corrupt);
            }
            let count = header.nritems as usize;
            let mut parsed: Items = Vec::new();
            parsed.try_reserve_exact(count).map_err(|_| Error::NoMemory)?;
            let mut hash = Hasher::new();
            for at in 0..count {
                let key = leaf_key(&node, at);
                let data = leaf_item(&node, at)?;
                if unsupported_fs_item(&key, data) {
                    return Err(Error::Unsupported);
                }
                hash.item(key, data);
                parsed.push((key, copy(data)?));
            }
            if parsed.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                || parsed.first().is_some_and(|(key, _)| *key != placed.first)
            {
                return Err(Error::Corrupt);
            }
            let items = &mut self.trees[index].items;
            let at = items.partition_point(|(key, _)| *key < placed.first);
            items.try_reserve(parsed.len()).map_err(|_| Error::NoMemory)?;
            items.splice(at..at, parsed);
            let leaf = &mut self.trees[index].levels[0][n];
            leaf.loaded = true;
            leaf.hash = hash.finish();
            leaf.count = count;
        }
        Ok(())
    }

    /// Порог выгрузки дерева ФС после фиксации — для проверок.
    #[cfg(test)]
    pub(crate) fn set_loaded_limit(&mut self, limit: usize) {
        self.loaded_limit = limit;
    }

    /// Сколько элементов дерева ФС сейчас в памяти — для проверок.
    #[cfg(test)]
    pub(crate) fn loaded_items(&self) -> usize {
        self.tree(objectid::FS_TREE).map_or(0, |tree| tree.items.len())
    }

    /// Подгрузить элементы объектов (inode) `numbers`.
    fn ensure_objects(&mut self, dev: &mut dyn BlockDevice, numbers: &[u64]) -> Result<()> {
        for &number in numbers {
            self.ensure_range(dev, Key::new(number, 0, 0), Key::new(number.saturating_add(1), 0, 0))?;
        }
        Ok(())
    }

    /// После фиксации: подгружено слишком много — выгрузить всё (всё уже на
    /// диске, и следующая правка подгрузит своё заново).
    fn unload_if_large(&mut self) {
        let Ok(index) = self.tree_index(objectid::FS_TREE) else { return };
        let tree = &mut self.trees[index];
        if !tree.paged || tree.items.len() <= self.loaded_limit || tree.dirty {
            return;
        }
        tree.items = Vec::new();
        for leaf in &mut tree.levels[0] {
            leaf.loaded = false;
        }
    }

    /// Найти inode по абсолютному пути — в памяти, с учётом ещё не
    /// зафиксированного.
    pub fn resolve(&mut self, dev: &mut dyn BlockDevice, path: &str) -> Result<u64> {
        let mut node = objectid::FIRST_FREE;
        for component in path.split('/').filter(|part| !part.is_empty() && *part != ".") {
            self.ensure_objects(dev, &[node])?;
            if !self.is_directory(node)? {
                return Err(Error::NotADirectory);
            }
            node = self.find_name(node, component)?.ok_or(Error::NotFound)?;
        }
        self.ensure_objects(dev, &[node])?;
        Ok(node)
    }

    /// Найти имя в каталоге.
    pub fn lookup(&mut self, dev: &mut dyn BlockDevice, parent: u64, name: &str) -> Result<Option<u64>> {
        self.ensure_objects(dev, &[parent])?;
        self.find_name(parent, name)
    }

    /// То же, что [`Writer::lookup`], по уже подгруженному каталогу.
    fn find_name(&self, parent: u64, name: &str) -> Result<Option<u64>> {
        check_name(name)?;
        let fs = self.tree(objectid::FS_TREE)?;
        let key = Key::new(parent, item_type::DIR_ITEM, crc32c::name_hash(name.as_bytes()));
        let Ok(at) = find(&fs.items, key) else {
            return Ok(None);
        };
        // Совпадение хешей — обычное дело: тогда в элементе несколько записей.
        let mut raw = fs.items[at].1.as_slice();
        while !raw.is_empty() {
            let (entry, used) = parse_dir_item(raw)?;
            if entry.name == name {
                return Ok(Some(entry.inode));
            }
            raw = &raw[used..];
        }
        Ok(None)
    }

    // --- чтение из памяти --------------------------------------------------------
    //
    // С 30.10 ядро фиксирует не каждую правку, а раз в несколько секунд и по
    // `sync`, и пока писатель открыт, читают **его**: читатель с диска видел
    // бы прежний том. Те же деревья в памяти, по которым правки и делаются;
    // данные — с носителя, со сверкой сумм, как у читателя.

    /// Inode по номеру.
    pub fn inode(&mut self, dev: &mut dyn BlockDevice, number: u64) -> Result<Inode> {
        self.ensure_objects(dev, &[number])?;
        let fs = self.tree(objectid::FS_TREE)?;
        let at = find(&fs.items, Key::new(number, item_type::INODE_ITEM, 0)).map_err(|_| Error::NotFound)?;
        Inode::parse(number, &fs.items[at].1)
    }

    /// Записи каталога в порядке создания (`DIR_INDEX`), как у читателя.
    pub fn list(&mut self, dev: &mut dyn BlockDevice, dir: u64) -> Result<Vec<DirEntry>> {
        self.ensure_objects(dev, &[dir])?;
        if !self.is_directory(dir)? {
            return Err(Error::NotADirectory);
        }
        let fs = self.tree(objectid::FS_TREE)?;
        let first = fs.items.partition_point(|(key, _)| *key < Key::first(dir, item_type::DIR_INDEX));
        let mut out = Vec::new();
        for (key, raw) in &fs.items[first..] {
            if key.objectid != dir || key.kind != item_type::DIR_INDEX {
                break;
            }
            push(&mut out, parse_dir_item(raw)?.0)?;
        }
        Ok(out)
    }

    /// Прочитать до `buf.len()` байт файла с `offset`; у конца файла — меньше.
    pub fn read_at(&mut self, dev: &mut dyn BlockDevice, number: u64, offset: u64, buf: &mut [u8]) -> Result<usize> {
        /// Длиннее за одно обращение к носителю не читается.
        const RUN: u64 = 256 * 1024;
        self.ensure_objects(dev, &[number])?;
        let (size, flags) = self.regular_inode(number)?;
        if offset >= size {
            return Ok(0);
        }
        let want = (size - offset).min(buf.len() as u64) as usize;
        let fs = self.tree(objectid::FS_TREE)?;
        let sector = u64::from(self.sectorsize);
        let device_sector = u64::from(dev.sector_size());
        let is_extent = |key: &Key| key.objectid == number && key.kind == item_type::EXTENT_DATA;
        let mut scratch = Vec::new();
        let mut done = 0usize;
        while done < want {
            let position = offset + done as u64;
            let next = fs.items.partition_point(|(key, _)| *key <= Key::new(number, item_type::EXTENT_DATA, position));
            // Где кончается дыра: следующий экстент или конец файла.
            let gap_end = fs.items.get(next).filter(|(key, _)| is_extent(key)).map_or(size, |(key, _)| key.offset).min(size);
            let hole = |buf: &mut [u8], done: usize| -> Result<usize> {
                let take = (gap_end.saturating_sub(position) as usize).min(want - done);
                if take == 0 {
                    return Err(Error::Corrupt);
                }
                buf[done..done + take].fill(0);
                Ok(take)
            };
            let Some((key, raw)) = next.checked_sub(1).map(|at| &fs.items[at]).filter(|(key, _)| is_extent(key)) else {
                done += hole(buf, done)?;
                continue;
            };
            if raw.len() <= EXTENT_TYPE {
                return Err(Error::Corrupt);
            }
            if raw[EXTENT_COMPRESSION] != 0 || raw[EXTENT_ENCRYPTION] != 0 {
                return Err(Error::Unsupported);
            }
            let within = position - key.offset;
            if raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
                let ram = u64_at(raw, EXTENT_RAM_BYTES);
                let data = &raw[EXTENT_INLINE_DATA..];
                if (data.len() as u64) < ram {
                    return Err(Error::Corrupt);
                }
                if within >= ram {
                    done += hole(buf, done)?;
                    continue;
                }
                let take = ((ram - within) as usize).min(want - done);
                buf[done..done + take].copy_from_slice(&data[within as usize..within as usize + take]);
                done += take;
                continue;
            }
            if raw.len() < EXTENT_REGULAR_SIZE {
                return Err(Error::Corrupt);
            }
            let length = u64_at(raw, EXTENT_NUM_BYTES);
            if within >= length {
                done += hole(buf, done)?;
                continue;
            }
            let take = (length - within).min((want - done) as u64).min(RUN) as usize;
            let bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
            if bytenr == 0 || raw[EXTENT_TYPE] == EXTENT_TYPE_PREALLOC {
                buf[done..done + take].fill(0);
                done += take;
                continue;
            }
            let logical = bytenr + u64_at(raw, EXTENT_OFFSET) + within;
            let start = align_down(logical, sector);
            let stop = align_up(logical + take as u64, sector);
            let (physical, run) = self.chunks.translate(start)?;
            if run < stop - start || physical % device_sector != 0 {
                return Err(Error::Corrupt);
            }
            scratch.clear();
            resize(&mut scratch, (stop - start) as usize)?;
            dev.read(self.first_lba + physical / device_sector, &mut scratch)?;
            if flags & INODE_FLAG_NODATASUM == 0 {
                for (n, piece) in scratch.chunks_exact(sector as usize).enumerate() {
                    match self.stored_checksum(start + n as u64 * sector)? {
                        Some(sum) if crc32c::checksum(piece) == sum => {}
                        Some(_) => return Err(Error::BadChecksum),
                        None => return Err(Error::Corrupt),
                    }
                }
            }
            let from = (logical - start) as usize;
            buf[done..done + take].copy_from_slice(&scratch[from..from + take]);
            done += take;
        }
        Ok(done)
    }

    /// Есть ли не зафиксированные правки.
    #[must_use]
    pub fn pending(&self) -> bool {
        self.trees.iter().any(|tree| tree.dirty)
    }

    /// Создать каталог.
    pub fn create_directory(&mut self, dev: &mut dyn BlockDevice, parent: u64, name: &str, attrs: &Attributes) -> Result<u64> {
        self.ensure_objects(dev, &[parent, self.next_inode])?;
        self.prepare(parent, name)?;
        let number = self.next_inode;
        let generation = self.generation + 1;
        let inode = inode_item(generation, MODE_DIRECTORY | u32::from(attrs.mode & 0o7777), 0, 0, attrs)?;

        self.broken = true;
        let fs = self.tree_index(objectid::FS_TREE)?;
        insert(&mut self.trees[fs].items, Key::new(number, item_type::INODE_ITEM, 0), inode)?;
        self.link(parent, name.as_bytes(), number, FILE_TYPE_DIRECTORY, attrs.time)?;
        self.next_inode += 1;
        self.time = attrs.time;
        self.broken = false;
        Ok(number)
    }

    /// Создать файл с содержимым.
    ///
    /// Данные пишутся на диск сразу — на место, свободное в последнем
    /// зафиксированном состоянии, — и становятся видны только после
    /// [`Writer::commit`].
    pub fn create_file(
        &mut self,
        dev: &mut dyn BlockDevice,
        parent: u64,
        name: &str,
        data: &[u8],
        attrs: &Attributes,
    ) -> Result<u64> {
        self.ensure_objects(dev, &[parent, self.next_inode])?;
        self.prepare(parent, name)?;
        let number = self.next_inode;
        let generation = self.generation + 1;
        let size = data.len() as u64;

        self.broken = true;
        let fs = self.tree_index(objectid::FS_TREE)?;
        let nbytes = if data.is_empty() {
            0
        } else if data.len() <= MAX_INLINE && data.len() < self.sectorsize as usize {
            // Короткий файл живёт в самом листе: отдельного блока, экстента и
            // суммы у него нет, а сумма узла накрывает и его.
            let body = item(EXTENT_INLINE_DATA + data.len(), |raw| {
                put_u64(raw, EXTENT_GENERATION, generation);
                put_u64(raw, EXTENT_RAM_BYTES, size);
                raw[EXTENT_TYPE] = EXTENT_TYPE_INLINE;
                raw[EXTENT_INLINE_DATA..].copy_from_slice(data);
            })?;
            insert(&mut self.trees[fs].items, Key::new(number, item_type::EXTENT_DATA, 0), body)?;
            size
        } else {
            self.write_extents(dev, number, 0, data)?
        };

        let inode = inode_item(generation, MODE_REGULAR | u32::from(attrs.mode & 0o7777), size, nbytes, attrs)?;
        insert(&mut self.trees[fs].items, Key::new(number, item_type::INODE_ITEM, 0), inode)?;
        self.link(parent, name.as_bytes(), number, FILE_TYPE_REGULAR, attrs.time)?;
        self.trees[fs].dirty = true;
        self.next_inode += 1;
        self.time = attrs.time;
        self.broken = false;
        Ok(number)
    }

    /// Зафиксировать транзакцию.
    pub fn commit(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        if !self.trees.iter().any(|tree| tree.dirty) {
            return Ok(());
        }
        disk::check_device(dev)?;
        let generation = self.generation + 1;
        // Фиксация меняет занятость (освобождённые экстенты, новые узлы):
        // список для размещения данных строится заново после неё.
        self.space = None;

        if self.tree(objectid::FS_TREE)?.dirty {
            self.broken = true;
            self.settle_data(generation)?;
            self.broken = false;
        }

        let plan = match self.plan_any(generation) {
            // Метаданным не хватило места. Кусок заводится в памяти — это
            // согласованное добавление, и оно просто уедет в эту же фиксацию.
            Err(Error::NoSpace) => {
                let nodes: usize = self.trees.iter().map(Tree::node_count).sum();
                let estimate = (nodes as u64 + 16) * u64::from(self.nodesize) * 2;
                self.allocate_chunk(BLOCK_GROUP_METADATA, estimate)?;
                self.plan_any(generation)?
            }
            other => other?,
        };

        let stamp = Stamp {
            nodesize: self.nodesize,
            fsid: self.fsid,
            chunk_uuid: self.chunk_uuid,
            generation,
        };
        let mut levels = Vec::new();
        for (slot, &index) in plan.rebuilt.iter().enumerate() {
            let tree = &self.trees[index];
            let (layout, placed) = (&plan.layouts[slot], &plan.placed[slot]);
            let (nodes, built) = match tree.id {
                objectid::EXTENT_TREE => build_tree(&plan.extent, layout, placed, tree.id, &stamp)?,
                objectid::FREE_SPACE_TREE => build_tree(&plan.free_space, layout, placed, tree.id, &stamp)?,
                objectid::ROOT_TREE => build_tree(&plan.root, layout, placed, tree.id, &stamp)?,
                _ => build_tree(&tree.items, layout, placed, tree.id, &stamp)?,
            };
            for (address, node) in nodes {
                self.write_logical(dev, address, &node)?;
            }
            push(&mut levels, built)?;
        }
        dev.flush()?;

        let sb = self.next_superblock(&plan, generation)?;
        // Из взгляда на прежнее дерево экстентов — только своё (новые и
        // изменённые элементы): дальше память правится на месте.
        let Plan { rebuilt, extent, replaced, free_space, root, .. } = plan;
        let changed = owned_part(extent)?;
        self.write_superblocks(dev, sb)?;

        // Записано — теперь и память описывает новое состояние.
        for (index, built) in rebuilt.into_iter().zip(levels) {
            let tree = &mut self.trees[index];
            tree.levels = built;
            tree.dirty = false;
        }
        let extent_index = self.tree_index(objectid::EXTENT_TREE)?;
        let old = core::mem::take(&mut self.trees[extent_index].items);
        match apply_extent(old, &replaced, changed) {
            Ok(items) => self.trees[extent_index].items = items,
            Err(error) => {
                // На диске — новое, в памяти — ни то ни другое.
                self.broken = true;
                return Err(error);
            }
        }
        let free_index = self.tree_index(objectid::FREE_SPACE_TREE)?;
        self.trees[free_index].items = free_space;
        let root_index = self.tree_index(objectid::ROOT_TREE)?;
        self.trees[root_index].items = root;
        self.generation = generation;
        self.space = None;
        self.unload_if_large();
        Ok(())
    }

    /// Записать данные в файл по смещению.
    ///
    /// Запись — это copy-on-write **только тронутых секторов**: элемент
    /// экстента, накрывающий их, делится на левый и правый остатки, которые
    /// продолжают ссылаться на прежний экстент, а тронутое ложится в новый. Так
    /// делает и ядро Linux, и так ссылок на экстент становится больше по
    /// счётчику, но не по разнообразию: у обоих остатков одна и та же пара
    /// (inode, смещение начала экстента в файле).
    ///
    /// Сектор, который пишется не целиком, сначала читается — со сверкой суммы.
    /// Без сверки испорченный сектор переехал бы в новый экстент с новой,
    /// правильной суммой, и порча стала бы невидимой навсегда.
    pub fn write_at(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u64,
        offset: u64,
        data: &[u8],
        time: u64,
    ) -> Result<usize> {
        if self.broken {
            return Err(Error::Aborted);
        }
        self.ensure_objects(dev, &[number])?;
        let (size, _) = self.regular_inode(number)?;
        if data.is_empty() {
            return Ok(0);
        }
        let end = offset.checked_add(data.len() as u64).ok_or(Error::Unsupported)?;
        let sector = u64::from(self.sectorsize);
        let new_size = size.max(end);
        let fits_inline = new_size <= MAX_INLINE as u64 && new_size < sector;

        self.broken = true;
        match self.inline_content(number)? {
            Some(old) if fits_inline => {
                let mut content = old;
                resize(&mut content, new_size as usize)?;
                content[offset as usize..end as usize].copy_from_slice(data);
                self.set_inline(number, &content)?;
            }
            // Встроенный файл вырос из листа: прежнее содержимое переезжает в
            // обычный экстент, и дальше это обычная запись.
            Some(old) => {
                self.remove_extent_items(number)?;
                if !old.is_empty() {
                    self.write_extents(dev, number, 0, &old)?;
                }
                self.overwrite(dev, number, size, offset, data)?;
            }
            None if fits_inline && size == 0 && !self.has_extents(number)? => {
                let mut content = try_zeroed(new_size as usize)?;
                content[offset as usize..].copy_from_slice(data);
                self.set_inline(number, &content)?;
            }
            None => self.overwrite(dev, number, size, offset, data)?,
        }
        self.finish_file_change(number, new_size, time)?;
        self.broken = false;
        Ok(data.len())
    }

    /// Усечь или продлить файл.
    ///
    /// Продление не пишет ничего: при `NO_HOLES` дыру не описывает ни один
    /// элемент. Усечение обнуляет хвост последнего сектора — иначе при
    /// следующем росте файла там проступило бы прежнее содержимое, то самое,
    /// которое усечением и убирали.
    pub fn truncate(&mut self, dev: &mut dyn BlockDevice, number: u64, size: u64, time: u64) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        self.ensure_objects(dev, &[number])?;
        let (old_size, _) = self.regular_inode(number)?;
        let sector = u64::from(self.sectorsize);

        self.broken = true;
        if let Some(mut content) = self.inline_content(number)? {
            if size == 0 {
                self.remove_extent_items(number)?;
            } else if size <= MAX_INLINE as u64 && size < sector {
                resize(&mut content, size as usize)?;
                self.set_inline(number, &content)?;
            } else {
                // Встроенный экстент обязан описывать файл целиком: файл больше
                // листа со встроенным началом ядро Linux считает порчей.
                self.remove_extent_items(number)?;
                if !content.is_empty() {
                    self.write_extents(dev, number, 0, &content)?;
                }
            }
        } else if size < old_size {
            self.punch(number, align_up(size, sector), u64::MAX)?;
            if size % sector != 0 {
                let start = align_down(size, sector);
                let mut last = try_zeroed(sector as usize)?;
                if self.read_file_sector(dev, number, start, &mut last)? {
                    last[(size - start) as usize..].fill(0);
                    self.punch(number, start, start + sector)?;
                    self.write_extents(dev, number, start, &last)?;
                }
            }
        }
        self.finish_file_change(number, size, time)?;
        self.broken = false;
        Ok(())
    }

    /// Удалить файл из каталога.
    ///
    /// Каталог так не удаляется — для него [`Writer::remove_directory`]. Данные
    /// освобождаются при фиксации, когда на экстент не останется ссылок.
    pub fn unlink(&mut self, dev: &mut dyn BlockDevice, parent: u64, name: &str, time: u64) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        check_name(name)?;
        self.ensure_objects(dev, &[parent])?;
        let (number, file_type) = self.entry(parent, name)?.ok_or(Error::NotFound)?;
        self.ensure_objects(dev, &[number])?;
        if file_type == FILE_TYPE_DIRECTORY {
            return Err(Error::IsADirectory);
        }
        self.broken = true;
        self.unlink_entry(parent, name.as_bytes(), number, time)?;
        self.drop_link(number)?;
        self.time = time;
        self.broken = false;
        Ok(())
    }

    /// Удалить пустой каталог.
    pub fn remove_directory(&mut self, dev: &mut dyn BlockDevice, parent: u64, name: &str, time: u64) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        check_name(name)?;
        self.ensure_objects(dev, &[parent])?;
        let (number, file_type) = self.entry(parent, name)?.ok_or(Error::NotFound)?;
        self.ensure_objects(dev, &[number])?;
        if file_type != FILE_TYPE_DIRECTORY {
            return Err(Error::NotADirectory);
        }
        let fs = self.tree(objectid::FS_TREE)?;
        let first = fs.items.partition_point(|(key, _)| *key < Key::first(number, item_type::DIR_INDEX));
        if fs.items.get(first).is_some_and(|(key, _)| key.objectid == number && key.kind == item_type::DIR_INDEX) {
            return Err(Error::NotEmpty);
        }
        self.broken = true;
        self.unlink_entry(parent, name.as_bytes(), number, time)?;
        self.remove_inode_items(number)?;
        self.time = time;
        self.broken = false;
        Ok(())
    }

    /// Переименовать или перенести.
    ///
    /// Семантика та же, что у `ext2::Editor::rename`, и по тем же причинам:
    /// занятое имя — отказ [`Error::Exists`], а не молчаливая замена, и каталог
    /// не переносится в другого родителя. Новое имя появляется раньше, чем
    /// исчезает старое, — но в btrfs это порядок в памяти: на диск уходит
    /// только готовая транзакция целиком.
    pub fn rename(
        &mut self,
        dev: &mut dyn BlockDevice,
        old_parent: u64,
        old_name: &str,
        new_parent: u64,
        new_name: &str,
        time: u64,
    ) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        check_name(old_name)?;
        check_name(new_name)?;
        self.ensure_objects(dev, &[old_parent, new_parent])?;
        let (number, file_type) = self.entry(old_parent, old_name)?.ok_or(Error::NotFound)?;
        self.ensure_objects(dev, &[number])?;
        if !self.is_directory(new_parent)? {
            return Err(Error::NotADirectory);
        }
        if self.entry(new_parent, new_name)?.is_some() {
            return Err(Error::Exists);
        }
        if file_type == FILE_TYPE_DIRECTORY && old_parent != new_parent {
            return Err(Error::Unsupported);
        }
        self.broken = true;
        self.link(new_parent, new_name.as_bytes(), number, file_type, time)?;
        self.unlink_entry(old_parent, old_name.as_bytes(), number, time)?;
        let generation = self.generation + 1;
        let raw = self.inode_mut(number)?;
        put_u64(raw, INODE_TRANSID, generation);
        put_u64(raw, INODE_CTIME, time);
        put_u32(raw, INODE_CTIME + 8, 0);
        self.time = time;
        self.broken = false;
        Ok(())
    }

    // --- операции над деревом ФС ---------------------------------------------

    /// Переписать отрезок обычного файла на новом месте.
    fn overwrite(&mut self, dev: &mut dyn BlockDevice, number: u64, size: u64, offset: u64, data: &[u8]) -> Result<()> {
        let sector = u64::from(self.sectorsize);
        let end = offset + data.len() as u64;
        let start = align_down(offset, sector);
        let stop = align_up(end, sector);
        let mut buf = try_zeroed((stop - start) as usize)?;
        let sector_len = sector as usize;
        if offset > start {
            self.read_file_sector(dev, number, start, &mut buf[..sector_len])?;
        }
        if end < stop && (stop - sector > start || offset == start) {
            let at = (stop - sector - start) as usize;
            self.read_file_sector(dev, number, stop - sector, &mut buf[at..at + sector_len])?;
        }
        // Байты за прежним концом файла в краевом секторе — нули, а не то, что
        // случайно лежало на диске за концом данных.
        if size < stop {
            let from = size.max(start) - start;
            let to = (offset.max(size) - start) as usize;
            if (from as usize) < to {
                buf[from as usize..to].fill(0);
            }
        }
        buf[(offset - start) as usize..(end - start) as usize].copy_from_slice(data);
        self.punch(number, start, stop)?;
        self.write_extents(dev, number, start, &buf)?;
        Ok(())
    }

    /// Вырезать из элементов экстентов файла отрезок `[start, stop)`.
    ///
    /// Остатки слева и справа продолжают ссылаться на прежний экстент со
    /// сдвинутым `extent offset` — ровно так, как их оставляет ядро Linux.
    fn punch(&mut self, number: u64, start: u64, stop: u64) -> Result<()> {
        self.touch(number)?;
        let fs = self.tree_index(objectid::FS_TREE)?;
        let items = &mut self.trees[fs].items;
        let first = items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA));
        let last = items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA + 1));

        let mut touched = Vec::new();
        for at in first..last {
            let (key, raw) = &items[at];
            if raw.len() < EXTENT_REGULAR_SIZE || raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
                return Err(Error::Unsupported);
            }
            let length = u64_at(raw, EXTENT_NUM_BYTES);
            if key.offset < stop && key.offset.saturating_add(length) > start {
                push(&mut touched, at)?;
            }
        }
        let mut remainders = Vec::new();
        for &at in touched.iter().rev() {
            let (key, raw) = items.remove(at);
            let length = u64_at(&raw, EXTENT_NUM_BYTES);
            let extent_offset = u64_at(&raw, EXTENT_OFFSET);
            if key.offset < start {
                let mut left = copy(&raw)?;
                put_u64(&mut left, EXTENT_NUM_BYTES, start - key.offset);
                push(&mut remainders, (key, left))?;
            }
            if key.offset + length > stop {
                let mut right = raw;
                put_u64(&mut right, EXTENT_OFFSET, extent_offset + (stop - key.offset));
                put_u64(&mut right, EXTENT_NUM_BYTES, key.offset + length - stop);
                push(&mut remainders, (Key::new(number, item_type::EXTENT_DATA, stop), right))?;
            }
        }
        for (key, raw) in remainders {
            insert(items, key, raw)?;
        }
        self.trees[fs].dirty = true;
        Ok(())
    }

    /// Прочитать сектор файла со сверкой суммы; `false` — сектор в дыре.
    fn read_file_sector(&self, dev: &mut dyn BlockDevice, number: u64, position: u64, out: &mut [u8]) -> Result<bool> {
        out.fill(0);
        let (_, flags) = self.regular_inode(number)?;
        let fs = self.tree(objectid::FS_TREE)?;
        let next = fs.items.partition_point(|(key, _)| *key <= Key::new(number, item_type::EXTENT_DATA, position));
        let Some(at) = next.checked_sub(1) else {
            return Ok(false);
        };
        let (key, raw) = &fs.items[at];
        if key.objectid != number || key.kind != item_type::EXTENT_DATA {
            return Ok(false);
        }
        if raw.len() < EXTENT_REGULAR_SIZE || raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
            return Err(Error::Unsupported);
        }
        if position >= key.offset + u64_at(raw, EXTENT_NUM_BYTES) {
            return Ok(false);
        }
        let bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
        if bytenr == 0 || raw[EXTENT_TYPE] == EXTENT_TYPE_PREALLOC {
            return Ok(false);
        }
        let logical = bytenr + u64_at(raw, EXTENT_OFFSET) + (position - key.offset);
        let (physical, run) = self.chunks.translate(logical)?;
        let sector = u64::from(dev.sector_size());
        if run < out.len() as u64 || physical % sector != 0 {
            return Err(Error::Corrupt);
        }
        dev.read(self.first_lba + physical / sector, out)?;
        if flags & INODE_FLAG_NODATASUM == 0 {
            match self.stored_checksum(logical)? {
                Some(sum) if crc32c::checksum(out) == sum => {}
                Some(_) => return Err(Error::BadChecksum),
                None => return Err(Error::Corrupt),
            }
        }
        Ok(true)
    }

    fn stored_checksum(&self, logical: u64) -> Result<Option<u32>> {
        let csum = self.tree(objectid::CSUM_TREE)?;
        let probe = Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, logical);
        let next = csum.items.partition_point(|(key, _)| *key <= probe);
        let Some(at) = next.checked_sub(1) else {
            return Ok(None);
        };
        let (key, raw) = &csum.items[at];
        if key.objectid != objectid::EXTENT_CSUM || key.kind != item_type::EXTENT_CSUM {
            return Ok(None);
        }
        let index = ((logical - key.offset) / u64::from(self.sectorsize)) as usize;
        Ok((index * 4 + 4 <= raw.len()).then(|| u32_at(raw, index * 4)))
    }

    /// Размер и флаги обычного файла.
    fn regular_inode(&self, number: u64) -> Result<(u64, u64)> {
        let fs = self.tree(objectid::FS_TREE)?;
        let at = find(&fs.items, Key::new(number, item_type::INODE_ITEM, 0)).map_err(|_| Error::NotFound)?;
        let raw = &fs.items[at].1;
        if raw.len() < INODE_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        match u32_at(raw, INODE_MODE) & MODE_FORMAT_MASK {
            MODE_REGULAR => Ok((u64_at(raw, INODE_SIZE_FIELD), u64_at(raw, INODE_FLAGS))),
            MODE_DIRECTORY => Err(Error::IsADirectory),
            _ => Err(Error::Unsupported),
        }
    }

    fn inode_mut(&mut self, number: u64) -> Result<&mut Vec<u8>> {
        let fs = self.tree_index(objectid::FS_TREE)?;
        self.trees[fs].dirty = true;
        let items = &mut self.trees[fs].items;
        let at = find(items, Key::new(number, item_type::INODE_ITEM, 0)).map_err(|_| Error::NotFound)?;
        let raw = &mut items[at].1;
        if raw.len() < INODE_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        Ok(raw)
    }

    /// Содержимое встроенного файла, если он встроенный.
    fn inline_content(&self, number: u64) -> Result<Option<Vec<u8>>> {
        let fs = self.tree(objectid::FS_TREE)?;
        let Ok(at) = find(&fs.items, Key::new(number, item_type::EXTENT_DATA, 0)) else {
            return Ok(None);
        };
        let raw = &fs.items[at].1;
        if raw.len() <= EXTENT_TYPE || raw[EXTENT_TYPE] != EXTENT_TYPE_INLINE {
            return Ok(None);
        }
        let length = u64_at(raw, EXTENT_RAM_BYTES) as usize;
        if raw.len() != EXTENT_INLINE_DATA + length {
            return Err(Error::Unsupported);
        }
        copy(&raw[EXTENT_INLINE_DATA..]).map(Some)
    }

    fn has_extents(&self, number: u64) -> Result<bool> {
        let fs = self.tree(objectid::FS_TREE)?;
        let at = fs.items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA));
        Ok(fs.items.get(at).is_some_and(|(key, _)| key.objectid == number && key.kind == item_type::EXTENT_DATA))
    }

    fn set_inline(&mut self, number: u64, content: &[u8]) -> Result<()> {
        self.remove_extent_items(number)?;
        let generation = self.generation + 1;
        let body = item(EXTENT_INLINE_DATA + content.len(), |raw| {
            put_u64(raw, EXTENT_GENERATION, generation);
            put_u64(raw, EXTENT_RAM_BYTES, content.len() as u64);
            raw[EXTENT_TYPE] = EXTENT_TYPE_INLINE;
            raw[EXTENT_INLINE_DATA..].copy_from_slice(content);
        })?;
        let fs = self.tree_index(objectid::FS_TREE)?;
        insert(&mut self.trees[fs].items, Key::new(number, item_type::EXTENT_DATA, 0), body)
    }

    fn remove_extent_items(&mut self, number: u64) -> Result<()> {
        self.touch(number)?;
        self.remove_range(Key::first(number, item_type::EXTENT_DATA), Key::first(number, item_type::EXTENT_DATA + 1))
    }

    fn remove_inode_items(&mut self, number: u64) -> Result<()> {
        self.touch(number)?;
        self.remove_range(Key::new(number, 0, 0), Key::new(number + 1, 0, 0))
    }

    /// Экстенты данных, на которые сейчас ссылается inode.
    fn extent_bytenrs(&self, number: u64) -> Result<Vec<u64>> {
        let fs = self.tree(objectid::FS_TREE)?;
        let first = fs.items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA));
        let mut out = Vec::new();
        for (key, raw) in &fs.items[first..] {
            if key.objectid != number || key.kind != item_type::EXTENT_DATA {
                break;
            }
            if raw.len() >= EXTENT_REGULAR_SIZE && raw[EXTENT_TYPE] != EXTENT_TYPE_INLINE {
                let bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
                if bytenr != 0 {
                    push(&mut out, bytenr)?;
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Элементы `EXTENT_DATA` inode сейчас изменятся: запомнить, на какие
    /// экстенты он ссылался до первой правки с прошлой фиксации.
    fn touch(&mut self, number: u64) -> Result<()> {
        if self.touched.contains_key(&number) {
            return Ok(());
        }
        let old = self.extent_bytenrs(number)?;
        self.touched.insert(number, old);
        Ok(())
    }

    fn remove_range(&mut self, from: Key, to: Key) -> Result<()> {
        let fs = self.tree_index(objectid::FS_TREE)?;
        let items = &mut self.trees[fs].items;
        let first = items.partition_point(|(key, _)| *key < from);
        let last = items.partition_point(|(key, _)| *key < to);
        items.drain(first..last);
        self.trees[fs].dirty = true;
        Ok(())
    }

    /// Новый размер файла, пересчитанные занятые байты и времена.
    fn finish_file_change(&mut self, number: u64, size: u64, time: u64) -> Result<()> {
        let fs = self.tree(objectid::FS_TREE)?;
        let first = fs.items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA));
        let mut nbytes = 0u64;
        for (key, raw) in &fs.items[first..] {
            if key.objectid != number || key.kind != item_type::EXTENT_DATA {
                break;
            }
            if raw.len() > EXTENT_TYPE && raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
                nbytes += u64_at(raw, EXTENT_RAM_BYTES);
            } else if raw.len() >= EXTENT_REGULAR_SIZE && u64_at(raw, EXTENT_DISK_BYTENR) != 0 {
                nbytes += u64_at(raw, EXTENT_NUM_BYTES);
            }
        }
        let generation = self.generation + 1;
        let raw = self.inode_mut(number)?;
        put_u64(raw, INODE_SIZE_FIELD, size);
        put_u64(raw, INODE_NBYTES, nbytes);
        put_u64(raw, INODE_TRANSID, generation);
        for field in [INODE_CTIME, INODE_MTIME] {
            put_u64(raw, field, time);
            put_u32(raw, field + 8, 0);
        }
        self.time = time;
        Ok(())
    }

    /// Запись каталога по имени: номер inode и тип.
    fn entry(&self, parent: u64, name: &str) -> Result<Option<(u64, u8)>> {
        let fs = self.tree(objectid::FS_TREE)?;
        let key = Key::new(parent, item_type::DIR_ITEM, crc32c::name_hash(name.as_bytes()));
        let Ok(at) = find(&fs.items, key) else {
            return Ok(None);
        };
        let raw = fs.items[at].1.as_slice();
        Ok(find_named(raw, name.as_bytes(), dir_entry_len)?
            .map(|(start, _)| (u64_at(raw, start + DIR_ITEM_LOCATION), raw[start + DIR_ITEM_TYPE])))
    }

    /// Убрать имя из каталога: запись по хешу, по номеру и обратную ссылку.
    fn unlink_entry(&mut self, parent: u64, name: &[u8], number: u64, time: u64) -> Result<()> {
        let generation = self.generation + 1;
        let fs = self.tree_index(objectid::FS_TREE)?;
        let items = &mut self.trees[fs].items;

        let hashed = Key::new(parent, item_type::DIR_ITEM, crc32c::name_hash(name));
        remove_named_entry(items, hashed, name, dir_entry_len)?;

        let reference = Key::new(number, item_type::INODE_REF, parent);
        let at = find(items, reference).map_err(|_| Error::Unsupported)?;
        let (start, _) = find_named(&items[at].1, name, inode_ref_len)?.ok_or(Error::Unsupported)?;
        let index = u64_at(&items[at].1, start);
        remove_named_entry(items, reference, name, inode_ref_len)?;

        let indexed = Key::new(parent, item_type::DIR_INDEX, index);
        let at = find(items, indexed).map_err(|_| Error::Corrupt)?;
        items.remove(at);

        let at = find(items, Key::new(parent, item_type::INODE_ITEM, 0)).map_err(|_| Error::Corrupt)?;
        let raw = &mut items[at].1;
        let size = u64_at(raw, INODE_SIZE_FIELD).saturating_sub(2 * name.len() as u64);
        put_u64(raw, INODE_SIZE_FIELD, size);
        put_u64(raw, INODE_TRANSID, generation);
        for field in [INODE_CTIME, INODE_MTIME] {
            put_u64(raw, field, time);
            put_u32(raw, field + 8, 0);
        }
        self.trees[fs].dirty = true;
        Ok(())
    }

    /// Отнять одну ссылку; последняя уносит inode целиком.
    fn drop_link(&mut self, number: u64) -> Result<()> {
        let generation = self.generation + 1;
        let raw = self.inode_mut(number)?;
        let links = u32_at(raw, INODE_NLINK).saturating_sub(1);
        if links > 0 {
            put_u32(raw, INODE_NLINK, links);
            put_u64(raw, INODE_TRANSID, generation);
            return Ok(());
        }
        self.remove_inode_items(number)
    }

    /// Привести дерево экстентов и дерево сумм в соответствие дереву ФС.
    ///
    /// Ссылки на экстенты данных не правятся по ходу операций, а выводятся
    /// здесь из элементов `EXTENT_DATA` — но с 31.10 только для экстентов,
    /// которых касались правленные inode (`touched`): до этого фиксация
    /// обходила всё дерево ФС и всё дерево сумм, а для тома с Debian и Rider
    /// это сотни тысяч элементов на каждую фиксацию. Ссылки нетронутых inode
    /// на те же экстенты переносятся из прежнего элемента как есть; экстент,
    /// на который не осталось ссылок, исчезает вместе с суммами своих
    /// секторов.
    fn settle_data(&mut self, generation: u64) -> Result<()> {
        let touched = core::mem::take(&mut self.touched);
        if touched.is_empty() {
            return Ok(());
        }
        // (адрес экстента, его длина, inode, смещение начала экстента в файле)
        let mut refs = Vec::new();
        let mut candidates = Vec::new();
        {
            let fs = self.tree(objectid::FS_TREE)?;
            for (&number, old) in &touched {
                for &bytenr in old {
                    push(&mut candidates, bytenr)?;
                }
                let first = fs.items.partition_point(|(key, _)| *key < Key::first(number, item_type::EXTENT_DATA));
                for (key, raw) in &fs.items[first..] {
                    if key.objectid != number || key.kind != item_type::EXTENT_DATA {
                        break;
                    }
                    if raw.len() < EXTENT_REGULAR_SIZE || raw[EXTENT_TYPE] == EXTENT_TYPE_INLINE {
                        continue;
                    }
                    let bytenr = u64_at(raw, EXTENT_DISK_BYTENR);
                    if bytenr == 0 {
                        continue;
                    }
                    let start = key.offset.wrapping_sub(u64_at(raw, EXTENT_OFFSET));
                    push(&mut refs, (bytenr, u64_at(raw, EXTENT_DISK_NUM_BYTES), key.objectid, start))?;
                    push(&mut candidates, bytenr)?;
                }
            }
        }
        refs.sort_unstable();
        candidates.sort_unstable();
        candidates.dedup();

        let extent = self.tree_index(objectid::EXTENT_TREE)?;
        let mut freed = Vec::new();
        for bytenr in candidates {
            // Новые ссылки правленных inode на этот экстент.
            let from = refs.partition_point(|r| r.0 < bytenr);
            let to = refs.partition_point(|r| r.0 <= bytenr);
            let mut group: Vec<(u64, u64, u32)> = Vec::new();
            let mut length = None;
            for &(_, len, owner, start) in &refs[from..to] {
                if length.is_some_and(|known| known != len) {
                    return Err(Error::Corrupt);
                }
                length = Some(len);
                match group.last_mut() {
                    Some(last) if last.0 == owner && last.1 == start => last.2 += 1,
                    _ => push(&mut group, (owner, start, 1))?,
                }
            }
            let items = &mut self.trees[extent].items;
            let at = items.partition_point(|(key, _)| *key < Key::first(bytenr, item_type::EXTENT_ITEM));
            let old = items.get(at).filter(|(key, _)| key.objectid == bytenr && key.kind == item_type::EXTENT_ITEM);
            // Ссылки нетронутых inode — из прежнего элемента.
            let previous = match old {
                Some((key, raw)) => {
                    let refs = data_refs(raw)?;
                    for &(owner, start, count) in refs.iter().filter(|r| !touched.contains_key(&r.0)) {
                        push(&mut group, (owner, start, count))?;
                    }
                    if length.is_some_and(|known| known != key.offset) {
                        return Err(Error::Corrupt);
                    }
                    length = Some(key.offset);
                    Some((refs, u64_at(raw, EXTENT_ITEM_GENERATION)))
                }
                None => None,
            };
            group.sort_unstable_by_key(|r| (r.0, r.1));
            let Some(length) = length else { continue };
            if group.is_empty() {
                if previous.is_some() {
                    items.remove(at);
                    push(&mut freed, (bytenr, length))?;
                }
                continue;
            }
            // Экстент и ссылки на него те же — элемент остаётся байт в байт.
            // Порядок нескольких встроенных ссылок ядро сверяет по хешу,
            // которого мы не считаем, так что чужой элемент с несколькими
            // ссылками переносится только нетронутым.
            if previous.as_ref().is_some_and(|(refs, _)| *refs == group) {
                continue;
            }
            if group.len() != 1 {
                return Err(Error::Unsupported);
            }
            let item_generation = previous.as_ref().map_or(generation, |(_, made)| *made);
            let (owner, start, count) = group[0];
            let record = item(DATA_EXTENT_ITEM_SIZE, |raw| {
                put_u64(raw, EXTENT_ITEM_REFS, u64::from(count));
                put_u64(raw, EXTENT_ITEM_GENERATION, item_generation);
                put_u64(raw, EXTENT_ITEM_FLAGS, EXTENT_FLAG_DATA);
                raw[EXTENT_ITEM_SIZE] = item_type::EXTENT_DATA_REF;
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_ROOT, objectid::FS_TREE);
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_OBJECTID, owner);
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_OFFSET, start);
                put_u32(raw, EXTENT_ITEM_SIZE + DATA_REF_COUNT, count);
            })?;
            let key = Key::new(bytenr, item_type::EXTENT_ITEM, length);
            if previous.is_some() {
                items[at] = (key, record);
            } else {
                insert(items, key, record)?;
            }
        }
        self.trees[extent].dirty = true;

        // Суммы освобождённых экстентов — вырезать их сектора.
        for (start, length) in freed {
            self.remove_checksums(start, length)?;
        }
        Ok(())
    }

    /// Вырезать из дерева сумм сектора `[start, start + length)`: элементы,
    /// задевающие отрезок, режутся на левый и правый остатки.
    fn remove_checksums(&mut self, start: u64, length: u64) -> Result<()> {
        let sector = u64::from(self.sectorsize);
        let end = start + length;
        let csum = self.tree_index(objectid::CSUM_TREE)?;
        let items = &mut self.trees[csum].items;
        // Элемент, начатый раньше `start`, может заходить в отрезок.
        let probe = Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, start);
        let mut at = items.partition_point(|(key, _)| *key < probe).saturating_sub(1);
        let mut remainders = Vec::new();
        while at < items.len() {
            let (key, raw) = &items[at];
            if key.objectid != objectid::EXTENT_CSUM || key.kind != item_type::EXTENT_CSUM {
                if key.offset >= end || key.objectid > objectid::EXTENT_CSUM {
                    break;
                }
                at += 1;
                continue;
            }
            let item_start = key.offset;
            let item_end = item_start + (raw.len() / 4) as u64 * sector;
            if item_start >= end {
                break;
            }
            if item_end <= start {
                at += 1;
                continue;
            }
            let (_, raw) = items.remove(at);
            if item_start < start {
                let keep = ((start - item_start) / sector) as usize * 4;
                push(&mut remainders, (Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, item_start), copy(&raw[..keep])?))?;
            }
            if item_end > end {
                let skip = ((end - item_start) / sector) as usize * 4;
                push(&mut remainders, (Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, end), copy(&raw[skip..])?))?;
            }
        }
        for (key, raw) in remainders {
            insert(items, key, raw)?;
        }
        self.trees[csum].dirty = true;
        Ok(())
    }

    fn prepare(&self, parent: u64, name: &str) -> Result<()> {
        if self.broken {
            return Err(Error::Aborted);
        }
        check_name(name)?;
        if !self.is_directory(parent)? {
            return Err(Error::NotADirectory);
        }
        if self.find_name(parent, name)?.is_some() {
            return Err(Error::Exists);
        }
        if self.next_inode > objectid::LAST_FREE {
            return Err(Error::NoSpace);
        }
        Ok(())
    }

    /// Вписать имя в каталог: три элемента и правка самого каталога.
    fn link(&mut self, parent: u64, name: &[u8], number: u64, file_type: u8, time: u64) -> Result<()> {
        let generation = self.generation + 1;
        let fs = self.tree_index(objectid::FS_TREE)?;
        let index = next_index(&self.trees[fs].items, parent);
        let entry = dir_entry(number, generation, file_type, name)?;
        let items = &mut self.trees[fs].items;

        // По хешу имени — для поиска; по порядковому номеру — для перечисления
        // в порядке создания.
        let hashed = Key::new(parent, item_type::DIR_ITEM, crc32c::name_hash(name));
        match find(items, hashed) {
            Ok(at) => {
                let data = &mut items[at].1;
                data.try_reserve(entry.len()).map_err(|_| Error::NoMemory)?;
                data.extend_from_slice(&entry);
            }
            Err(_) => insert(items, hashed, copy(&entry)?)?,
        }
        insert(items, Key::new(parent, item_type::DIR_INDEX, index), entry)?;
        // Обратных ссылок в одного родителя может быть несколько — это имена
        // одного inode в одном каталоге, и лежат они в одном элементе подряд.
        let reference = Key::new(number, item_type::INODE_REF, parent);
        let body = inode_ref(index, name)?;
        match find(items, reference) {
            Ok(at) => {
                let data = &mut items[at].1;
                data.try_reserve(body.len()).map_err(|_| Error::NoMemory)?;
                data.extend_from_slice(&body);
            }
            Err(_) => insert(items, reference, body)?,
        }

        // Размер каталога в btrfs — удвоенная сумма длин имён: по разу за
        // запись по хешу и за запись по номеру. `btrfs check` сверяет это.
        let at = find(items, Key::new(parent, item_type::INODE_ITEM, 0)).map_err(|_| Error::NotFound)?;
        let raw = &mut items[at].1;
        if raw.len() < INODE_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        let size = u64_at(raw, INODE_SIZE_FIELD) + 2 * name.len() as u64;
        put_u64(raw, INODE_SIZE_FIELD, size);
        put_u64(raw, INODE_TRANSID, generation);
        for field in [INODE_CTIME, INODE_MTIME] {
            put_u64(raw, field, time);
            put_u32(raw, field + 8, 0);
        }
        self.trees[fs].dirty = true;
        Ok(())
    }

    /// Разложить данные по экстентам начиная со смещения `file_offset` (кратного
    /// сектору); возвращает занятые байты.
    fn write_extents(&mut self, dev: &mut dyn BlockDevice, number: u64, file_offset: u64, data: &[u8]) -> Result<u64> {
        self.touch(number)?;
        let sector = u64::from(self.sectorsize);
        let aligned = (data.len() as u64).div_ceil(sector) * sector;
        let generation = self.generation + 1;
        let mut done = 0u64;
        while done < aligned {
            let want = (aligned - done).min(MAX_EXTENT);
            let (logical, length) = self.allocate_data(want)?;
            self.write_data(dev, logical, data, done, length)?;
            self.add_checksums(logical, data, done, length)?;

            let fs = self.tree_index(objectid::FS_TREE)?;
            let file_extent = item(EXTENT_REGULAR_SIZE, |raw| {
                put_u64(raw, EXTENT_GENERATION, generation);
                put_u64(raw, EXTENT_RAM_BYTES, length);
                raw[EXTENT_TYPE] = EXTENT_TYPE_REGULAR;
                put_u64(raw, EXTENT_DISK_BYTENR, logical);
                put_u64(raw, EXTENT_DISK_NUM_BYTES, length);
                put_u64(raw, EXTENT_NUM_BYTES, length);
            })?;
            insert(
                &mut self.trees[fs].items,
                Key::new(number, item_type::EXTENT_DATA, file_offset + done),
                file_extent,
            )?;

            let extent = self.tree_index(objectid::EXTENT_TREE)?;
            let record = item(DATA_EXTENT_ITEM_SIZE, |raw| {
                put_u64(raw, EXTENT_ITEM_REFS, 1);
                put_u64(raw, EXTENT_ITEM_GENERATION, generation);
                put_u64(raw, EXTENT_ITEM_FLAGS, EXTENT_FLAG_DATA);
                raw[EXTENT_ITEM_SIZE] = item_type::EXTENT_DATA_REF;
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_ROOT, objectid::FS_TREE);
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_OBJECTID, number);
                put_u64(raw, EXTENT_ITEM_SIZE + DATA_REF_OFFSET, file_offset + done);
                put_u32(raw, EXTENT_ITEM_SIZE + DATA_REF_COUNT, 1);
            })?;
            insert(
                &mut self.trees[extent].items,
                Key::new(logical, item_type::EXTENT_ITEM, length),
                record,
            )?;
            self.trees[extent].dirty = true;
            done += length;
        }
        Ok(aligned)
    }

    /// Записать кусок файла `[from, from + length)` по логическому адресу.
    ///
    /// Хвост последнего сектора дополняется нулями: сумма считается по сектору
    /// целиком, и мусор за концом файла дал бы сумму, которую потом не
    /// воспроизвести.
    fn write_data(&self, dev: &mut dyn BlockDevice, logical: u64, data: &[u8], from: u64, length: u64) -> Result<()> {
        let sector = u64::from(self.sectorsize);
        let start = from as usize;
        let end = (from + length) as usize;
        let full = ((data.len() as u64 / sector) * sector) as usize;
        let whole_end = end.min(full).max(start);
        if whole_end > start {
            self.write_logical(dev, logical, &data[start..whole_end])?;
        }
        if end > whole_end {
            let mut tail = try_zeroed(end - whole_end)?;
            let have = data.len() - whole_end;
            tail[..have].copy_from_slice(&data[whole_end..]);
            self.write_logical(dev, logical + (whole_end - start) as u64, &tail)?;
        }
        Ok(())
    }

    fn add_checksums(&mut self, logical: u64, data: &[u8], from: u64, length: u64) -> Result<()> {
        let sector = u64::from(self.sectorsize);
        let csum = self.tree_index(objectid::CSUM_TREE)?;
        let sectors = length / sector;
        let mut pad = try_zeroed(sector as usize)?;
        let mut index = 0u64;
        while index < sectors {
            let count = (sectors - index).min(CSUM_ITEM_SECTORS);
            let mut body = try_zeroed(count as usize * 4)?;
            for k in 0..count {
                let begin = (from + (index + k) * sector) as usize;
                let end = (begin + sector as usize).min(data.len());
                let sum = if end - begin == sector as usize {
                    crc32c::checksum(&data[begin..end])
                } else {
                    pad.fill(0);
                    pad[..end - begin].copy_from_slice(&data[begin..end]);
                    crc32c::checksum(&pad)
                };
                put_u32(&mut body, k as usize * 4, sum);
            }
            insert(
                &mut self.trees[csum].items,
                Key::new(objectid::EXTENT_CSUM, item_type::EXTENT_CSUM, logical + index * sector),
                body,
            )?;
            index += count;
        }
        self.trees[csum].dirty = true;
        Ok(())
    }

    // --- место ----------------------------------------------------------------

    /// Отдать под данные до `want` байт подряд.
    ///
    /// Группы и занятые отрезки — из [`Writer::space`], а не обходом всего
    /// дерева экстентов на каждый экстент (так было до 30.10: запись тысяч
    /// файлов стоила квадрата). Между фиксациями занятость только растёт —
    /// освобождает `settle_data`, — поэтому отданное просто дописывается.
    fn allocate_data(&mut self, want: u64) -> Result<(u64, u64)> {
        for attempt in 0..2 {
            if self.space.is_none() {
                let extent = &self.tree(objectid::EXTENT_TREE)?.items;
                self.space = Some((groups(extent)?, busy_ranges(extent, self.nodesize)?));
            }
            let found = match &self.space {
                Some((groups, busy)) => self.first_free(BLOCK_GROUP_DATA, groups, busy, want, true)?,
                None => None,
            };
            if let Some(found) = found {
                if let Some((_, busy)) = self.space.as_mut() {
                    let at = busy.partition_point(|&(start, _)| start < found.0);
                    busy.try_reserve(1).map_err(|_| Error::NoMemory)?;
                    busy.insert(at, found);
                }
                return Ok(found);
            }
            if attempt == 0 {
                self.allocate_chunk(BLOCK_GROUP_DATA, want)?;
                self.space = None;
            }
        }
        Err(Error::NoSpace)
    }

    /// Первый свободный отрезок в группах данного вида.
    ///
    /// `partial` — годится ли кусок короче `want` (данным годится: файл ляжет в
    /// несколько экстентов; узлу — нет).
    fn first_free(
        &self,
        kind: u64,
        groups: &[Group],
        busy: &[(u64, u64)],
        want: u64,
        partial: bool,
    ) -> Result<Option<(u64, u64)>> {
        let align = u64::from(self.sectorsize);
        for group in groups.iter().filter(|group| group.kind == kind) {
            let end = group.start + group.length;
            let mirrors = self.mirrors(group)?;
            let mut index = busy.partition_point(|&(start, len)| start + len <= group.start);
            let mut mirror = 0;
            let mut cursor = group.start;
            loop {
                let next_busy = busy.get(index).copied().filter(|&(start, _)| start < end);
                let next_mirror = mirrors.get(mirror).copied();
                let obstacle = match (next_busy, next_mirror) {
                    (Some(taken), Some(copy)) if taken.0 <= copy.0 => {
                        index += 1;
                        Some(taken)
                    }
                    (_, Some(copy)) => {
                        mirror += 1;
                        Some(copy)
                    }
                    (Some(taken), None) => {
                        index += 1;
                        Some(taken)
                    }
                    (None, None) => None,
                };
                let gap_end = obstacle.map_or(end, |(start, _)| start.min(end));
                if let Some(found) = fit(cursor, gap_end, want, align, partial) {
                    return Ok(Some(found));
                }
                match obstacle {
                    Some((start, len)) => cursor = cursor.max(start + len),
                    None => break,
                }
                if cursor >= end {
                    break;
                }
            }
        }
        Ok(None)
    }

    /// Отрезки группы, которые накрывают копии суперблока.
    fn mirrors(&self, group: &Group) -> Result<Vec<(u64, u64)>> {
        let (physical, _) = self.chunks.translate(group.start)?;
        let mut out = Vec::new();
        for copy in SUPERBLOCK_COPIES {
            if copy >= physical && copy < physical + group.length {
                let logical = group.start + (copy - physical);
                push(&mut out, (align_down(logical, STRIPE_LEN), STRIPE_LEN))?;
            }
        }
        Ok(out)
    }

    /// Завести новый кусок.
    ///
    /// Логический адрес — за последним куском: ядро Linux тоже никогда не
    /// отдаёт логический адрес повторно, и расходиться с ним здесь незачем.
    /// Физическое место — первый подходящий промежуток устройства.
    fn allocate_chunk(&mut self, kind: u64, at_least: u64) -> Result<()> {
        // Системный кусок пришлось бы вписывать ещё и в массив суперблока.
        if kind == BLOCK_GROUP_SYSTEM {
            return Err(Error::Unsupported);
        }

        let wanted = chunk_size(kind, self.total_bytes, at_least);
        let minimum = align_up(at_least.max(1), 4 * MIB);
        let mut taken = Vec::new();
        for chunk in self.chunks.iter() {
            push(&mut taken, (chunk.physical, chunk.length))?;
        }
        taken.sort_unstable();

        let mut best: Option<(u64, u64)> = None;
        let mut cursor = MIB;
        let consider = |start: u64, end: u64, best: &mut Option<(u64, u64)>| {
            let start = align_up(start, MIB);
            if start >= end {
                return;
            }
            let room = align_down(end - start, MIB);
            let length = room.min(wanted);
            if length >= minimum && best.is_none_or(|(_, have)| have < wanted && length > have) {
                *best = Some((start, length));
            }
        };
        for &(start, length) in &taken {
            if start > cursor {
                consider(cursor, start, &mut best);
            }
            cursor = cursor.max(start + length);
        }
        consider(cursor, self.total_bytes, &mut best);
        let (physical, length) = best.ok_or(Error::NoSpace)?;
        let logical = self.chunks.iter().map(|chunk| chunk.logical + chunk.length).max().unwrap_or(MIB);

        let chunk_index = self.tree_index(objectid::CHUNK_TREE)?;
        let dev_key = Key::new(objectid::DEV_ITEMS, item_type::DEV_ITEM, 1);
        let at = find(&self.trees[chunk_index].items, dev_key).map_err(|_| Error::Corrupt)?;
        if self.trees[chunk_index].items[at].1.len() < DEV_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        let mut device_uuid = [0u8; 16];
        device_uuid.copy_from_slice(&self.trees[chunk_index].items[at].1[DEV_ITEM_UUID..DEV_ITEM_UUID + 16]);

        // Отсюда начинаются правки: до этой строки отказ (нет места, нет
        // памяти) оставляет транзакцию целой, после — уже нет.
        let was_broken = self.broken;
        self.broken = true;
        let sectorsize = self.sectorsize;
        let chunk = item(CHUNK_ITEM_SIZE, |raw| {
            put_u64(raw, CHUNK_LENGTH, length);
            put_u64(raw, CHUNK_OWNER, objectid::EXTENT_TREE);
            put_u64(raw, CHUNK_STRIPE_LEN, STRIPE_LEN);
            put_u64(raw, CHUNK_TYPE, kind);
            put_u32(raw, CHUNK_IO_ALIGN, STRIPE_LEN as u32);
            put_u32(raw, CHUNK_IO_WIDTH, STRIPE_LEN as u32);
            put_u32(raw, CHUNK_SECTOR_SIZE, sectorsize);
            put_u16(raw, CHUNK_NUM_STRIPES, 1);
            put_u16(raw, CHUNK_SUB_STRIPES, 1);
            put_u64(raw, CHUNK_HEAD_SIZE + STRIPE_DEVID, 1);
            put_u64(raw, CHUNK_HEAD_SIZE + STRIPE_OFFSET, physical);
            raw[CHUNK_HEAD_SIZE + STRIPE_DEV_UUID..CHUNK_HEAD_SIZE + STRIPE_DEV_UUID + 16]
                .copy_from_slice(&device_uuid);
        })?;
        self.chunks.add(logical, &chunk)?;
        insert(
            &mut self.trees[chunk_index].items,
            Key::new(objectid::FIRST_CHUNK_TREE, item_type::CHUNK_ITEM, logical),
            chunk,
        )?;
        let device = &mut self.trees[chunk_index].items[at].1;
        let used = u64_at(device, DEV_ITEM_BYTES_USED) + length;
        put_u64(device, DEV_ITEM_BYTES_USED, used);
        self.trees[chunk_index].dirty = true;

        let chunk_uuid = self.chunk_uuid;
        let dev_index = self.tree_index(objectid::DEV_TREE)?;
        let dev_extent = item(DEV_EXTENT_SIZE, |raw| {
            put_u64(raw, DEV_EXTENT_CHUNK_TREE, objectid::CHUNK_TREE);
            put_u64(raw, DEV_EXTENT_CHUNK_OBJECTID, objectid::FIRST_CHUNK_TREE);
            put_u64(raw, DEV_EXTENT_CHUNK_OFFSET, logical);
            put_u64(raw, DEV_EXTENT_LENGTH, length);
            raw[DEV_EXTENT_UUID..DEV_EXTENT_UUID + 16].copy_from_slice(&chunk_uuid);
        })?;
        insert(&mut self.trees[dev_index].items, Key::new(1, item_type::DEV_EXTENT, physical), dev_extent)?;
        self.trees[dev_index].dirty = true;

        let extent_index = self.tree_index(objectid::EXTENT_TREE)?;
        let group = item(BLOCK_GROUP_ITEM_SIZE, |raw| {
            put_u64(raw, BLOCK_GROUP_CHUNK_OBJECTID, objectid::FIRST_CHUNK_TREE);
            put_u64(raw, BLOCK_GROUP_FLAGS, kind);
        })?;
        insert(
            &mut self.trees[extent_index].items,
            Key::new(logical, item_type::BLOCK_GROUP_ITEM, length),
            group,
        )?;
        self.trees[extent_index].dirty = true;

        self.broken = was_broken;
        Ok(())
    }

    // --- фиксация ---------------------------------------------------------------

    /// Раскладка с прежними узлами; не сошлась — прежним способом, всё
    /// тронутое заново. Второй путь — страховка: он тот, что жил до 30.10.
    fn plan_any(&self, generation: u64) -> Result<Plan<'_>> {
        match self.plan(generation, true) {
            Err(Error::Unsupported) => self.plan(generation, false),
            other => other,
        }
    }

    /// Посчитать раскладку фиксации, ничего не меняя. `reuse` — оставлять
    /// нетронутые узлы на месте.
    fn plan(&self, generation: u64, reuse: bool) -> Result<Plan<'_>> {
        let nodesize = u64::from(self.nodesize);
        let extent_index = self.tree_index(objectid::EXTENT_TREE)?;
        let free_index = self.tree_index(objectid::FREE_SPACE_TREE)?;
        let root_index = self.tree_index(objectid::ROOT_TREE)?;
        let derived = [extent_index, free_index, root_index];

        // Перестраивается всё тронутое и три дерева, которые меняются от любой
        // перестройки: экстентов (новые узлы), свободного места и корней.
        let mut rebuilt = Vec::new();
        for (index, tree) in self.trees.iter().enumerate() {
            if tree.dirty || derived.contains(&index) {
                push(&mut rebuilt, index)?;
            }
        }

        // Все прежние узлы перестраиваемых деревьев остаются занятыми до конца
        // фиксации — на них ссылается суперблок; записи уходят только у
        // заменённых.
        let groups = groups(&self.trees[extent_index].items)?;
        // Узлам нужны только группы метаданных: экстенты данных (на томе с
        // Rider — сотни тысяч) в список занятого для них не входят, иначе
        // каждая вставка двигала бы мегабайты.
        let mut taken = busy_ranges(&self.trees[extent_index].items, self.nodesize)?;
        taken.retain(|&(at, len)| {
            groups.iter().any(|group| {
                group.kind != BLOCK_GROUP_DATA && at < group.start + group.length && group.start < at + len
            })
        });
        for &index in &rebuilt {
            for placed in self.trees[index].levels.iter().flatten() {
                push(&mut taken, (placed.address, nodesize))?;
            }
        }
        taken.sort_unstable();

        // Корень дерева корней — всегда новый: его поколение обязано совпасть
        // с поколением суперблока.
        let mut memo = LeafMemo::new(&self.trees[extent_index].items);
        let mut layouts = Vec::new();
        for &index in &rebuilt {
            let tree = &self.trees[index];
            let first = if index == extent_index {
                layout_with(&tree.items, &tree.levels, self.nodesize, reuse, false, |placed, start, end| {
                    memo.base_leaf(placed, start, end)
                })?
            } else {
                layout(&tree.items, &tree.levels, self.nodesize, reuse, index == root_index)?
            };
            push(&mut layouts, first)?;
        }

        for _ in 0..MAX_ROUNDS {
            let mut shapes = Vec::new();
            for layout in &layouts {
                push(&mut shapes, layout.iter().map(|level| level.iter().filter(|slot| matches!(slot, Slot::New { .. })).count()).collect())?;
            }
            let placed = self.place(&rebuilt, &shapes, &groups, &taken)?;

            // Заменённые узлы: прежние, которых нет среди оставленных.
            let mut replaced = Vec::new();
            for (slot, &index) in rebuilt.iter().enumerate() {
                let mut kept = Vec::new();
                for entry in layouts[slot].iter().flatten() {
                    if let Slot::Kept(placed) = entry {
                        push(&mut kept, placed.address)?;
                    }
                }
                kept.sort_unstable();
                for placed in self.trees[index].levels.iter().flatten() {
                    if kept.binary_search(&placed.address).is_err() {
                        push(&mut replaced, placed.address)?;
                    }
                }
            }
            replaced.sort_unstable();

            let mut fresh = Vec::new();
            for (slot, &index) in rebuilt.iter().enumerate() {
                for &(address, level) in &placed[slot] {
                    let owner = self.trees[index].id;
                    let record = item(METADATA_ITEM_SIZE, |raw| {
                        put_u64(raw, EXTENT_ITEM_REFS, 1);
                        put_u64(raw, EXTENT_ITEM_GENERATION, generation);
                        put_u64(raw, EXTENT_ITEM_FLAGS, EXTENT_FLAG_TREE_BLOCK);
                        raw[EXTENT_ITEM_SIZE] = item_type::TREE_BLOCK_REF;
                        put_u64(raw, EXTENT_ITEM_SIZE + 1, owner);
                    })?;
                    push(&mut fresh, (Key::new(address, item_type::METADATA_ITEM, u64::from(level)), record))?;
                }
            }
            let mut extent = derive_extent(&self.trees[extent_index].items, &replaced, fresh)?;
            let busy = busy_ranges(&extent, self.nodesize)?;
            let used = set_group_usage(&mut extent, &busy)?;
            let free_space = free_space_items(&busy, &groups)?;

            let mut tops = Vec::new();
            for (slot, layout) in layouts.iter().enumerate() {
                push(&mut tops, top_of(layout, &placed[slot], generation)?)?;
            }

            let mut root = clone_items(&self.trees[root_index].items)?;
            for (slot, &index) in rebuilt.iter().enumerate() {
                let tree = &self.trees[index];
                if tree.id == objectid::ROOT_TREE || tree.id == objectid::CHUNK_TREE {
                    continue;
                }
                let (address, level, root_generation) = tops[slot];
                let nodes: usize = layouts[slot].iter().map(Vec::len).sum();
                let at = find(&root, Key::new(tree.id, item_type::ROOT_ITEM, 0)).map_err(|_| Error::Corrupt)?;
                let raw = &mut root[at].1;
                // Поколение корня, а не транзакции: нетронутое дерево
                // сохраняет прежний корень, и читающий сверяет его заголовок
                // именно с этим числом.
                put_u64(raw, ROOT_ITEM_GENERATION, root_generation);
                put_u64(raw, ROOT_ITEM_BYTENR, address);
                raw[ROOT_ITEM_LEVEL] = level;
                put_u64(raw, ROOT_ITEM_BYTES_USED, nodes as u64 * nodesize);
                // Второе поколение обязано совпасть с первым: по расхождению
                // ядро узнаёт элемент, записанный старым ядром.
                put_u64(raw, ROOT_ITEM_GENERATION_V2, root_generation);
                if tree.id == objectid::FS_TREE && tree.dirty {
                    put_u64(raw, ROOT_ITEM_CTRANSID, generation);
                    put_u64(raw, ROOT_ITEM_CTIME, self.time);
                    put_u32(raw, ROOT_ITEM_CTIME + 8, 0);
                }
            }

            // Раскладка трёх вычисляемых деревьев зависит от их новых
            // элементов, а те — от раскладки всех: подбор до неподвижной точки.
            let mut next = Vec::new();
            for (slot, &index) in rebuilt.iter().enumerate() {
                let levels = &self.trees[index].levels;
                let again = if index == extent_index {
                    layout_with(&extent, levels, self.nodesize, reuse, false, |placed, start, end| {
                        memo.view_leaf(&extent, placed, start, end)
                    })?
                } else if index == free_index {
                    layout(&free_space, levels, self.nodesize, reuse, false)?
                } else if index == root_index {
                    layout(&root, levels, self.nodesize, reuse, true)?
                } else {
                    layouts[slot].clone()
                };
                push(&mut next, again)?;
            }
            if next == layouts {
                return Ok(Plan { rebuilt, layouts, placed, tops, extent, replaced, free_space, root, used });
            }
            layouts = next;
        }
        // Не сошлось — это ошибка подсчёта, а не свойство тома.
        Err(Error::Unsupported)
    }

    /// Раздать адреса узлам всех перестраиваемых деревьев.
    fn place(
        &self,
        rebuilt: &[usize],
        shapes: &[Vec<usize>],
        groups: &[Group],
        taken: &[(u64, u64)],
    ) -> Result<Vec<Vec<(u64, u8)>>> {
        let nodesize = u64::from(self.nodesize);
        let mut busy = Vec::new();
        busy.try_reserve_exact(taken.len() + 64).map_err(|_| Error::NoMemory)?;
        busy.extend_from_slice(taken);

        let mut placed = Vec::new();
        for (slot, &index) in rebuilt.iter().enumerate() {
            // Дерево кусков обязано лежать в системном куске: только его
            // адреса есть в массиве суперблока.
            let kind = if self.trees[index].id == objectid::CHUNK_TREE {
                BLOCK_GROUP_SYSTEM
            } else {
                BLOCK_GROUP_METADATA
            };
            let mut nodes = Vec::new();
            for (level, &count) in shapes[slot].iter().enumerate() {
                for _ in 0..count {
                    let (address, _) = self
                        .first_free(kind, groups, &busy, nodesize, false)?
                        .ok_or(Error::NoSpace)?;
                    let at = busy.partition_point(|&(start, _)| start < address);
                    busy.try_reserve(1).map_err(|_| Error::NoMemory)?;
                    busy.insert(at, (address, nodesize));
                    push(&mut nodes, (address, level as u8))?;
                }
            }
            push(&mut placed, nodes)?;
        }
        Ok(placed)
    }

    fn next_superblock(&self, plan: &Plan<'_>, generation: u64) -> Result<Vec<u8>> {
        let mut sb = copy(&self.superblock)?;
        let slot_of = |id: u64| {
            plan.rebuilt.iter().position(|&index| self.trees[index].id == id)
        };

        let root_slot = slot_of(objectid::ROOT_TREE).ok_or(Error::Corrupt)?;
        let (root, root_level, root_generation) = plan.tops[root_slot];
        if root_generation != generation {
            return Err(Error::Corrupt);
        }
        put_u64(&mut sb, SB_GENERATION, generation);
        put_u64(&mut sb, SB_ROOT, root);
        sb[SB_ROOT_LEVEL] = root_level;
        if let Some(slot) = slot_of(objectid::CHUNK_TREE) {
            let (chunk_root, chunk_level, chunk_generation) = plan.tops[slot];
            put_u64(&mut sb, SB_CHUNK_ROOT, chunk_root);
            sb[SB_CHUNK_ROOT_LEVEL] = chunk_level;
            put_u64(&mut sb, SB_CHUNK_ROOT_GENERATION, chunk_generation);
        }
        put_u64(&mut sb, SB_BYTES_USED, plan.used);

        // Описание устройства в суперблоке — копия того, что в дереве кусков.
        let chunk_tree = self.tree(objectid::CHUNK_TREE)?;
        let at = find(&chunk_tree.items, Key::new(objectid::DEV_ITEMS, item_type::DEV_ITEM, 1))
            .map_err(|_| Error::Corrupt)?;
        let device = &chunk_tree.items[at].1;
        if device.len() < DEV_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        sb[SB_DEV_ITEM..SB_DEV_ITEM + DEV_ITEM_SIZE].copy_from_slice(&device[..DEV_ITEM_SIZE]);

        // Запасные корни идут по кругу: следующий слот после самого свежего.
        let newest = (0..4)
            .max_by_key(|&slot| u64_at(&sb, SB_SUPER_ROOTS + slot * BACKUP_SIZE + BACKUP_TREE_ROOT + 8))
            .unwrap_or(0);
        let base = SB_SUPER_ROOTS + ((newest + 1) % 4) * BACKUP_SIZE;
        sb[base..base + BACKUP_SIZE].fill(0);
        put_u64(&mut sb, base + BACKUP_TREE_ROOT, root);
        put_u64(&mut sb, base + BACKUP_TREE_ROOT + 8, generation);
        sb[base + BACKUP_TREE_ROOT_LEVEL] = root_level;
        let chunk_root = u64_at(&sb, SB_CHUNK_ROOT);
        let chunk_generation = u64_at(&sb, SB_CHUNK_ROOT_GENERATION);
        put_u64(&mut sb, base + BACKUP_CHUNK_ROOT, chunk_root);
        put_u64(&mut sb, base + BACKUP_CHUNK_ROOT + 8, chunk_generation);
        sb[base + BACKUP_CHUNK_ROOT_LEVEL] = sb[SB_CHUNK_ROOT_LEVEL];
        for (field, level_field, id) in [
            (BACKUP_EXTENT_ROOT, BACKUP_EXTENT_ROOT_LEVEL, objectid::EXTENT_TREE),
            (BACKUP_FS_ROOT, BACKUP_FS_ROOT_LEVEL, objectid::FS_TREE),
            (BACKUP_DEV_ROOT, BACKUP_DEV_ROOT_LEVEL, objectid::DEV_TREE),
            (BACKUP_CSUM_ROOT, BACKUP_CSUM_ROOT_LEVEL, objectid::CSUM_TREE),
        ] {
            let at = find(&plan.root, Key::new(id, item_type::ROOT_ITEM, 0)).map_err(|_| Error::Corrupt)?;
            let raw = &plan.root[at].1;
            put_u64(&mut sb, base + field, u64_at(raw, ROOT_ITEM_BYTENR));
            put_u64(&mut sb, base + field + 8, u64_at(raw, ROOT_ITEM_GENERATION));
            sb[base + level_field] = raw[ROOT_ITEM_LEVEL];
        }
        put_u64(&mut sb, base + BACKUP_TOTAL_BYTES, self.total_bytes);
        put_u64(&mut sb, base + BACKUP_BYTES_USED, plan.used);
        put_u64(&mut sb, base + BACKUP_NUM_DEVICES, 1);
        Ok(sb)
    }

    /// Записать суперблок и его копии.
    ///
    /// Основной — первым. Оборвись запись на нём, сумма не сойдётся, и
    /// читатель возьмёт копию — старую, но целую, а её деревья не тронуты:
    /// новые узлы писались только на свободное место.
    fn write_superblocks(&mut self, dev: &mut dyn BlockDevice, mut sb: Vec<u8>) -> Result<()> {
        let sector = u64::from(dev.sector_size());
        let device_bytes = dev.sector_count().saturating_sub(self.first_lba).saturating_mul(sector);
        let limit = self.total_bytes.min(device_bytes);
        for copy in SUPERBLOCK_COPIES {
            if copy + SUPERBLOCK_SIZE as u64 > limit {
                break;
            }
            put_u64(&mut sb, SB_BYTENR, copy);
            node::seal(&mut sb);
            let lba = self.first_lba.checked_add(copy / sector).ok_or(Error::Corrupt)?;
            dev.write(lba, &sb)?;
        }
        dev.flush()?;
        put_u64(&mut sb, SB_BYTENR, SUPERBLOCK_OFFSET);
        node::seal(&mut sb);
        self.superblock = sb;
        Ok(())
    }

    // --- мелочи ---------------------------------------------------------------------

    fn write_logical(&self, dev: &mut dyn BlockDevice, logical: u64, buf: &[u8]) -> Result<()> {
        let (physical, run) = self.chunks.translate(logical)?;
        if buf.len() as u64 > run {
            return Err(Error::Corrupt);
        }
        let sector = u64::from(dev.sector_size());
        if physical % sector != 0 || buf.len() as u64 % sector != 0 {
            return Err(Error::Unsupported);
        }
        let lba = self.first_lba.checked_add(physical / sector).ok_or(Error::Corrupt)?;
        dev.write(lba, buf)?;
        Ok(())
    }

    fn tree_index(&self, id: u64) -> Result<usize> {
        self.trees.iter().position(|tree| tree.id == id).ok_or(Error::Corrupt)
    }

    fn tree(&self, id: u64) -> Result<&Tree> {
        self.trees.iter().find(|tree| tree.id == id).ok_or(Error::Corrupt)
    }

    fn is_directory(&self, number: u64) -> Result<bool> {
        let fs = self.tree(objectid::FS_TREE)?;
        let at = find(&fs.items, Key::new(number, item_type::INODE_ITEM, 0)).map_err(|_| Error::NotFound)?;
        let raw = &fs.items[at].1;
        if raw.len() < INODE_ITEM_SIZE {
            return Err(Error::Corrupt);
        }
        Ok(u32_at(raw, INODE_MODE) & MODE_FORMAT_MASK == MODE_DIRECTORY)
    }
}

// --- чтение тома в память ---------------------------------------------------------

fn load_tree(volume: &mut Volume, dev: &mut dyn BlockDevice, id: u64, root: u64, level: u8) -> Result<Tree> {
    if level as usize >= MAX_DEPTH {
        return Err(Error::Corrupt);
    }
    let mut tree = Tree { id, items: Vec::new(), levels: Vec::new(), dirty: false, paged: false };
    for _ in 0..=level {
        push(&mut tree.levels, Vec::new())?;
    }
    // Обход в глубину слева направо: элементы приходят уже упорядоченными, и
    // узлы каждого уровня — тоже.
    let mut stack = Vec::new();
    push(&mut stack, (root, level))?;
    while let Some((address, expected)) = stack.pop() {
        let node = volume.read_node(dev, address)?;
        let header = Header::parse(&node)?;
        if header.level != expected {
            return Err(Error::Corrupt);
        }
        let count = header.nritems as usize;
        let mut hash = Hasher::new();
        let first;
        if expected == 0 {
            first = if count == 0 { Key::new(0, 0, 0) } else { leaf_key(&node, 0) };
            tree.items.try_reserve(count).map_err(|_| Error::NoMemory)?;
            for index in 0..count {
                let key = leaf_key(&node, index);
                let data = leaf_item(&node, index)?;
                hash.item(key, data);
                tree.items.push((key, copy(data)?));
            }
        } else {
            if count == 0 {
                return Err(Error::Corrupt);
            }
            first = node_key(&node, 0);
            for index in 0..count {
                hash.child(node_key(&node, index), node_child(&node, index), node_child_generation(&node, index));
            }
            for index in (0..count).rev() {
                push(&mut stack, (node_child(&node, index), expected - 1))?;
            }
        }
        let placed = Placed { address, first, generation: header.generation, hash: hash.finish(), count, loaded: true };
        push(&mut tree.levels[expected as usize], placed)?;
    }
    if tree.levels.last().map(Vec::len) != Some(1) {
        return Err(Error::Corrupt);
    }
    // Порядок, в котором их положили чужие руки, проверяется, а не принимается:
    // на непорядке двоичный поиск находит не то, что есть.
    if tree.items.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err(Error::Corrupt);
    }
    // Узлы уровня — по возрастанию первого ключа: по их границам фиксация
    // раскладывает элементы.
    if tree.levels.iter().any(|level| level.windows(2).any(|pair| pair[0].first >= pair[1].first)) {
        return Err(Error::Corrupt);
    }
    Ok(tree)
}

/// Дерево без листьев (31.10, дерево ФС большого тома): читаются только
/// внутренние узлы, лист известен по указателю родителя — адрес, первый ключ,
/// поколение — и подгружается по требованию ([`Writer::ensure_range`]).
/// Дерево из одного листа читается целиком.
fn load_tree_paged(volume: &mut Volume, dev: &mut dyn BlockDevice, id: u64, root: u64, level: u8) -> Result<Tree> {
    if level == 0 {
        let mut tree = load_tree(volume, dev, id, root, level)?;
        tree.paged = true;
        return Ok(tree);
    }
    if level as usize >= MAX_DEPTH {
        return Err(Error::Corrupt);
    }
    let mut tree = Tree { id, items: Vec::new(), levels: Vec::new(), dirty: false, paged: true };
    for _ in 0..=level {
        push(&mut tree.levels, Vec::new())?;
    }
    let mut stack = Vec::new();
    push(&mut stack, (root, level))?;
    while let Some((address, expected)) = stack.pop() {
        let node = volume.read_node(dev, address)?;
        let header = Header::parse(&node)?;
        let count = header.nritems as usize;
        if header.level != expected || expected == 0 || count == 0 {
            return Err(Error::Corrupt);
        }
        let mut hash = Hasher::new();
        for index in 0..count {
            let (key, child, generation) = (node_key(&node, index), node_child(&node, index), node_child_generation(&node, index));
            hash.child(key, child, generation);
            if expected == 1 {
                let leaf = Placed { address: child, first: key, generation, hash: 0, count: 0, loaded: false };
                push(&mut tree.levels[0], leaf)?;
            } else {
                push(&mut stack, (child, expected - 1))?;
            }
        }
        let placed = Placed {
            address,
            first: node_key(&node, 0),
            generation: header.generation,
            hash: hash.finish(),
            count,
            loaded: true,
        };
        push(&mut tree.levels[expected as usize], placed)?;
    }
    for level in &mut tree.levels {
        level.sort_unstable_by_key(|placed| placed.first);
    }
    if tree.levels.last().map(Vec::len) != Some(1)
        || tree.levels.iter().any(|level| level.windows(2).any(|pair| pair[0].first >= pair[1].first))
    {
        return Err(Error::Corrupt);
    }
    Ok(tree)
}

/// Элемент дерева ФС, который писатель не умеет переписать: сжатый или
/// шифрованный экстент, длинные обратные ссылки (сотни жёстких ссылок).
fn unsupported_fs_item(key: &Key, raw: &[u8]) -> bool {
    match key.kind {
        item_type::EXTENT_DATA => raw.len() <= EXTENT_TYPE || raw[EXTENT_COMPRESSION] != 0 || raw[EXTENT_ENCRYPTION] != 0,
        item_type::INODE_EXTREF => true,
        _ => false,
    }
}

/// Сколько элементов дерева ФС держать подгруженными после фиксации; больше
/// — выгрузить всё (всё уже на диске).
const LOADED_ITEMS_LIMIT: usize = 100_000;

/// Хеш содержимого узла (64 бита): по нему фиксация узнаёт узел, который
/// можно оставить на месте. Совпадение числа элементов проверяется отдельно.
///
/// Словами по 8 байт, умножение со сдвигом (31.10; было FNV-1a по байту —
/// на томе с Rider фиксация хешировала деревья экстентов и сумм каждый круг
/// подбора, ~10 мс на круг). Хеш живёт только в памяти, на диск не пишется.
struct Hasher(u64);

impl Hasher {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn word(&mut self, word: u64) {
        let mixed = (self.0 ^ word).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        self.0 = mixed ^ (mixed >> 29);
    }

    fn bytes(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let mut word = [0u8; 8];
            word.copy_from_slice(chunk);
            self.word(u64::from_le_bytes(word));
        }
        let rest = chunks.remainder();
        let mut word = [0u8; 8];
        word[..rest.len()].copy_from_slice(rest);
        // Длина хвоста — в старшем байте: «ab» и «ab\0» различаются.
        word[7] ^= rest.len() as u8;
        self.word(u64::from_le_bytes(word));
    }

    fn key(&mut self, key: Key) {
        self.word(key.objectid);
        self.word(u64::from(key.kind));
        self.word(key.offset);
    }

    fn item(&mut self, key: Key, data: &[u8]) {
        self.key(key);
        self.word(data.len() as u64);
        self.bytes(data);
    }

    fn child(&mut self, key: Key, address: u64, generation: u64) {
        self.key(key);
        self.word(address);
        self.word(generation);
    }

    const fn finish(&self) -> u64 {
        self.0
    }
}

fn hash_items<D: AsRef<[u8]>>(items: &[(Key, D)]) -> u64 {
    let mut hash = Hasher::new();
    for (key, data) in items {
        hash.item(*key, data.as_ref());
    }
    hash.finish()
}

// --- раскладка --------------------------------------------------------------------

/// Уложить листьями подряд элементы `[start, end)`.
fn pack_leaves<D: AsRef<[u8]>>(items: &[(Key, D)], start: usize, end: usize, nodesize: u32, out: &mut Vec<Slot>) -> Result<()> {
    let capacity = node::leaf_capacity(nodesize);
    let mut from = start;
    let mut used = 0usize;
    for (index, (_, data)) in items.iter().enumerate().take(end).skip(start) {
        let need = ITEM_SIZE + data.as_ref().len();
        if need > capacity {
            return Err(Error::Unsupported);
        }
        if used + need > capacity {
            push(out, Slot::New { start: from, end: index })?;
            from = index;
            used = 0;
        }
        used += need;
    }
    if from < end {
        push(out, Slot::New { start: from, end })?;
    }
    Ok(())
}

/// Уложить внутренними узлами подряд узлы уровня ниже `[start, end)`.
fn pack_internal(start: usize, end: usize, nodesize: u32, out: &mut Vec<Slot>) -> Result<()> {
    let per_node = node::pointers_per_node(nodesize);
    let mut from = start;
    while from < end {
        let to = (from + per_node).min(end);
        push(out, Slot::New { start: from, end: to })?;
        from = to;
    }
    Ok(())
}

/// Разложить записи уровня (`keys` — первый ключ каждой) по границам прежних
/// узлов этого уровня: прежний узел остаётся, если его отрезок совпал
/// (`same`), подряд идущие несовпавшие отрезки укладываются заново (`pack`).
fn relay(
    keys: &[Key],
    old: &[Placed],
    mut same: impl FnMut(&Placed, usize, usize) -> bool,
    mut pack: impl FnMut(usize, usize, &mut Vec<Slot>) -> Result<()>,
) -> Result<Vec<Slot>> {
    let mut out = Vec::new();
    if old.is_empty() {
        pack(0, keys.len(), &mut out)?;
        return Ok(out);
    }
    // Начало несовпавшего хвоста, который ещё не уложен.
    let mut pending: Option<usize> = None;
    for (n, placed) in old.iter().enumerate() {
        let start = if n == 0 { 0 } else { keys.partition_point(|key| *key < placed.first) };
        let end = old.get(n + 1).map_or(keys.len(), |next| keys.partition_point(|key| *key < next.first));
        // Неподгруженный лист не тронут по определению — на месте. Элементы в
        // его диапазоне значили бы, что правку сделали мимо подгрузки, и
        // честнее отказать, чем потерять его содержимое.
        if !placed.loaded {
            if end > start {
                return Err(Error::Corrupt);
            }
            if let Some(from) = pending.take() {
                pack(from, start, &mut out)?;
            }
            push(&mut out, Slot::Kept(*placed))?;
            continue;
        }
        if end > start && end - start == placed.count && same(placed, start, end) {
            if let Some(from) = pending.take() {
                pack(from, start, &mut out)?;
            }
            push(&mut out, Slot::Kept(*placed))?;
        } else if pending.is_none() {
            pending = Some(start);
        }
    }
    if let Some(from) = pending {
        pack(from, keys.len(), &mut out)?;
    }
    Ok(out)
}

/// Раскладка дерева: что остаётся на месте, что пишется заново.
///
/// `reuse = false` — всё заново (так было до 30.10). `new_root` — корень
/// новый в любом случае (дерево корней: его поколение — поколение
/// суперблока).
fn layout<D: AsRef<[u8]>>(items: &[(Key, D)], old: &[Vec<Placed>], nodesize: u32, reuse: bool, new_root: bool) -> Result<Layout> {
    layout_with(items, old, nodesize, reuse, new_root, |placed, start, end| hash_items(&items[start..end]) == placed.hash)
}

/// То же, что [`layout`], со своей сверкой листа (`same_leaf`, см.
/// [`LeafMemo`]).
fn layout_with<D: AsRef<[u8]>>(
    items: &[(Key, D)],
    old: &[Vec<Placed>],
    nodesize: u32,
    reuse: bool,
    new_root: bool,
    same_leaf: impl FnMut(&Placed, usize, usize) -> bool,
) -> Result<Layout> {
    // «Всё заново» для дерева с неподгруженными листьями выбросило бы их
    // элементы — их нет в `items`.
    if !reuse && old.first().is_some_and(|leaves| leaves.iter().any(|leaf| !leaf.loaded)) {
        return Err(Error::Unsupported);
    }
    let none: &[Placed] = &[];
    let old_level = |level: usize| if reuse { old.get(level).map_or(none, Vec::as_slice) } else { none };

    let mut keys = Vec::new();
    keys.try_reserve_exact(items.len()).map_err(|_| Error::NoMemory)?;
    keys.extend(items.iter().map(|(key, _)| *key));
    let mut leaves = relay(&keys, old_level(0), same_leaf, |start, end, out| pack_leaves(items, start, end, nodesize, out))?;
    // Пустое дерево — один пустой лист; других пустых листьев не бывает.
    if leaves.is_empty() {
        push(&mut leaves, Slot::New { start: 0, end: 0 })?;
    }
    let mut out: Layout = Vec::new();
    let mut below_keys: Vec<Key> = Vec::new();
    for slot in &leaves {
        push(
            &mut below_keys,
            match *slot {
                Slot::Kept(placed) => placed.first,
                Slot::New { start, .. } => items.get(start).map_or(Key::new(0, 0, 0), |(key, _)| *key),
            },
        )?;
    }
    push(&mut out, leaves)?;

    while out.last().map_or(0, Vec::len) > 1 {
        if out.len() >= MAX_DEPTH {
            return Err(Error::Unsupported);
        }
        let below = out.last().ok_or(Error::Corrupt)?;
        let level = relay(
            &below_keys,
            old_level(out.len()),
            |placed, start, end| {
                let mut hash = Hasher::new();
                for slot in &below[start..end] {
                    let Slot::Kept(child) = slot else { return false };
                    hash.child(child.first, child.address, child.generation);
                }
                hash.finish() == placed.hash
            },
            |start, end, out| pack_internal(start, end, nodesize, out),
        )?;
        let mut keys = Vec::new();
        for slot in &level {
            push(
                &mut keys,
                match *slot {
                    Slot::Kept(placed) => placed.first,
                    Slot::New { start, .. } => below_keys[start],
                },
            )?;
        }
        below_keys = keys;
        push(&mut out, level)?;
    }

    if new_root {
        let top = out.len() - 1;
        let end = if top == 0 { items.len() } else { out[top - 1].len() };
        if let Some(slot @ Slot::Kept(_)) = out[top].first_mut() {
            *slot = Slot::New { start: 0, end };
        }
    }
    Ok(out)
}

/// Собрать новые узлы дерева по раскладке и розданным адресам; вместе с
/// ними — раскладку, какой она станет на диске.
fn build_tree<D: AsRef<[u8]>>(
    items: &[(Key, D)],
    layout: &Layout,
    placed: &[(u64, u8)],
    owner: u64,
    stamp: &Stamp,
) -> Result<(Vec<(u64, Vec<u8>)>, Vec<Vec<Placed>>)> {
    let mut addresses = placed.iter();
    let mut out = Vec::new();
    let mut built: Vec<Vec<Placed>> = Vec::new();
    for (level, slots) in layout.iter().enumerate() {
        let mut row = Vec::new();
        for slot in slots {
            let placed = match *slot {
                Slot::Kept(placed) => placed,
                Slot::New { start, end } => {
                    let &(address, placed_level) = addresses.next().ok_or(Error::Corrupt)?;
                    if usize::from(placed_level) != level {
                        return Err(Error::Corrupt);
                    }
                    if level == 0 {
                        let slice = &items[start..end];
                        let first = slice.first().map_or(Key::new(0, 0, 0), |(key, _)| *key);
                        push(&mut out, (address, node::leaf(slice, address, owner, stamp)?))?;
                        Placed { address, first, generation: stamp.generation, hash: hash_items(slice), count: end - start, loaded: true }
                    } else {
                        let below = built.get(level - 1).ok_or(Error::Corrupt)?;
                        let children = below.get(start..end).ok_or(Error::Corrupt)?;
                        let mut pointers = Vec::new();
                        let mut hash = Hasher::new();
                        for child in children {
                            push(&mut pointers, (child.first, child.address, child.generation))?;
                            hash.child(child.first, child.address, child.generation);
                        }
                        push(&mut out, (address, node::internal(&pointers, address, owner, level as u8, stamp)?))?;
                        Placed {
                            address,
                            first: pointers.first().ok_or(Error::Corrupt)?.0,
                            generation: stamp.generation,
                            hash: hash.finish(),
                            count: end - start,
                            loaded: true,
                        }
                    }
                }
            };
            push(&mut row, placed)?;
        }
        push(&mut built, row)?;
    }
    if addresses.next().is_some() {
        return Err(Error::Corrupt);
    }
    Ok((out, built))
}

/// Корень раскладки: адрес, уровень, поколение. Новые узлы раздаются
/// уровень за уровнем, и новый корень — последний из них.
fn top_of(layout: &Layout, placed: &[(u64, u8)], generation: u64) -> Result<(u64, u8, u64)> {
    let level = layout.len().checked_sub(1).ok_or(Error::Corrupt)?;
    match layout[level].as_slice() {
        [Slot::Kept(placed)] => Ok((placed.address, level as u8, placed.generation)),
        [Slot::New { .. }] => {
            let &(address, placed_level) = placed.last().ok_or(Error::Corrupt)?;
            if usize::from(placed_level) != level {
                return Err(Error::Corrupt);
            }
            Ok((address, level as u8, generation))
        }
        _ => Err(Error::Corrupt),
    }
}

/// Дерево экстентов следующей фиксации — взгляд на прежнее (31.10): нетронутые
/// элементы заимствуются, а не копируются. На томе с Rider прежнее дерево —
/// сотни тысяч элементов, и копия на каждый круг подбора стоила ~16 мс.
type View<'a> = Vec<(Key, Cow<'a, [u8]>)>;

/// Прежнее дерево экстентов без записей заменённых узлов (`replaced` — по
/// возрастанию) и с записями новых (`fresh`).
fn derive_extent<'a>(base: &'a Items, replaced: &[u64], mut fresh: Items) -> Result<View<'a>> {
    fresh.sort_unstable_by_key(|(key, _)| *key);
    let mut out: View<'a> = Vec::new();
    out.try_reserve_exact(base.len() + fresh.len()).map_err(|_| Error::NoMemory)?;
    let mut fresh = fresh.into_iter().peekable();
    for (key, raw) in base {
        if is_node_record_of(key, replaced) {
            continue;
        }
        while let Some((next, _)) = fresh.peek() {
            if *next > *key {
                break;
            }
            // Совпавший ключ — ошибка подсчёта: молча заменить значило бы
            // потерять элемент.
            if *next == *key {
                return Err(Error::Corrupt);
            }
            let (next, record) = fresh.next().ok_or(Error::Corrupt)?;
            out.push((next, Cow::Owned(record)));
        }
        out.push((*key, Cow::Borrowed(raw.as_slice())));
    }
    for (key, record) in fresh {
        out.push((key, Cow::Owned(record)));
    }
    Ok(out)
}

/// Итог сверки листьев дерева экстентов с прежними на одну фиксацию: лист
/// (адрес) и отрезок прежнего дерева (начало, длина) → совпал ли. Между
/// кругами подбора взгляд на дерево меняется в единицах мест, и хешировать
/// сотни тысяч элементов на каждом круге незачем.
struct LeafMemo<'a> {
    base: &'a Items,
    seen: BTreeMap<(u64, usize, usize), bool>,
}

impl<'a> LeafMemo<'a> {
    fn new(base: &'a Items) -> Self {
        Self { base, seen: BTreeMap::new() }
    }

    /// Лист по отрезку прежнего дерева.
    fn base_leaf(&mut self, placed: &Placed, start: usize, end: usize) -> bool {
        let base = self.base;
        *self
            .seen
            .entry((placed.address, start, end - start))
            .or_insert_with(|| hash_items(&base[start..end]) == placed.hash)
    }

    /// Лист по отрезку взгляда. Отрезок, целиком заимствованный у прежнего
    /// дерева, у которого крайние ключи совпали с отрезком прежнего той же
    /// длины, — это и есть тот отрезок: заимствованные идут по ключу, без
    /// повторов, а между крайними у прежнего ровно столько же элементов.
    fn view_leaf(&mut self, view: &View<'_>, placed: &Placed, start: usize, end: usize) -> bool {
        let range = &view[start..end];
        let borrowed = range.iter().all(|(_, data)| matches!(data, Cow::Borrowed(_)));
        if let (true, Some((first, _)), Some((last, _))) = (borrowed, range.first(), range.last()) {
            if let Ok(at) = find(self.base, *first) {
                if self.base.get(at + range.len() - 1).is_some_and(|(key, _)| key == last) {
                    return self.base_leaf(placed, at, at + range.len());
                }
            }
        }
        hash_items(range) == placed.hash
    }
}

fn is_node_record_of(key: &Key, replaced: &[u64]) -> bool {
    matches!(key.kind, item_type::METADATA_ITEM | item_type::EXTENT_ITEM) && replaced.binary_search(&key.objectid).is_ok()
}

/// Своё во взгляде: новые записи и изменённые (занятость групп).
fn owned_part(view: View<'_>) -> Result<Items> {
    let mut out = Vec::new();
    for (key, data) in view {
        if let Cow::Owned(data) = data {
            push(&mut out, (key, data))?;
        }
    }
    Ok(out)
}

/// Привести прежнее дерево экстентов к зафиксированному: убрать записи
/// заменённых узлов, своё из взгляда — вставить или заменить. Элементы
/// переносятся, а не копируются.
fn apply_extent(old: Items, replaced: &[u64], changed: Items) -> Result<Items> {
    let mut out = Vec::new();
    out.try_reserve_exact(old.len() + changed.len()).map_err(|_| Error::NoMemory)?;
    let mut changed = changed.into_iter().peekable();
    for (key, raw) in old {
        if is_node_record_of(&key, replaced) {
            continue;
        }
        let mut same = false;
        while let Some((next, _)) = changed.peek() {
            if *next > key {
                break;
            }
            same = *next == key;
            let entry = changed.next().ok_or(Error::Corrupt)?;
            out.push(entry);
        }
        if !same {
            out.push((key, raw));
        }
    }
    out.extend(changed);
    Ok(out)
}

/// Группы блоков, как их описывает дерево экстентов.
fn groups(extent: &Items) -> Result<Vec<Group>> {
    let mut out = Vec::new();
    for (key, raw) in extent {
        if key.kind == item_type::BLOCK_GROUP_ITEM {
            if raw.len() < BLOCK_GROUP_ITEM_SIZE {
                return Err(Error::Corrupt);
            }
            let kind = u64_at(raw, BLOCK_GROUP_FLAGS) & BLOCK_GROUP_TYPE_MASK;
            push(&mut out, Group { start: key.objectid, length: key.offset, kind })?;
        }
    }
    Ok(out)
}

/// Занятые отрезки по возрастанию адреса: экстенты данных и узлы.
fn busy_ranges<D>(extent: &[(Key, D)], nodesize: u32) -> Result<Vec<(u64, u64)>> {
    let mut out = Vec::new();
    for (key, _) in extent {
        match key.kind {
            item_type::EXTENT_ITEM => push(&mut out, (key.objectid, key.offset))?,
            item_type::METADATA_ITEM => push(&mut out, (key.objectid, u64::from(nodesize)))?,
            _ => {}
        }
    }
    Ok(out)
}

/// Пересчитать `used` каждой группы; возвращает сумму по тому.
fn set_group_usage(extent: &mut View<'_>, busy: &[(u64, u64)]) -> Result<u64> {
    let mut total = 0;
    for (key, raw) in extent.iter_mut() {
        if key.kind != item_type::BLOCK_GROUP_ITEM {
            continue;
        }
        let (start, end) = (key.objectid, key.objectid + key.offset);
        // Занятые отрезки идут по адресу: только те, что в группе.
        let from = busy.partition_point(|&(at, len)| at + len <= start);
        let used: u64 = busy[from..]
            .iter()
            .take_while(|&&(at, _)| at < end)
            .map(|&(at, len)| (at + len).min(end).saturating_sub(at.max(start)))
            .sum();
        // Своя копия элемента — только если число и правда другое.
        if u64_at(raw, BLOCK_GROUP_USED) != used {
            put_u64(raw.to_mut(), BLOCK_GROUP_USED, used);
        }
        total += used;
    }
    Ok(total)
}

/// Дерево свободного места, построенное из дерева экстентов.
///
/// Копии суперблока здесь не исключаются — так делает и ядро Linux (см.
/// [`STRIPE_LEN`]).
fn free_space_items(busy: &[(u64, u64)], groups: &[Group]) -> Result<Items> {
    let mut sorted = Vec::new();
    for group in groups {
        push(&mut sorted, *group)?;
    }
    sorted.sort_unstable_by_key(|group| group.start);

    let mut out = Vec::new();
    for group in &sorted {
        let end = group.start + group.length;
        let mut free = Vec::new();
        let mut cursor = group.start;
        let from = busy.partition_point(|&(at, len)| at + len <= group.start);
        for &(at, len) in &busy[from..] {
            if at >= end {
                break;
            }
            if at > cursor {
                push(&mut free, (cursor, at - cursor))?;
            }
            cursor = cursor.max(at + len);
        }
        if cursor < end {
            push(&mut free, (cursor, end - cursor))?;
        }
        let count = free.len() as u32;
        push(
            &mut out,
            (
                Key::new(group.start, item_type::FREE_SPACE_INFO, group.length),
                item(FREE_SPACE_INFO_SIZE, |raw| put_u32(raw, FREE_SPACE_INFO_EXTENT_COUNT, count))?,
            ),
        )?;
        for (at, len) in free {
            push(&mut out, (Key::new(at, item_type::FREE_SPACE_EXTENT, len), Vec::new()))?;
        }
    }
    Ok(out)
}

fn fit(start: u64, end: u64, want: u64, align: u64, partial: bool) -> Option<(u64, u64)> {
    let start = align_up(start, align);
    if start >= end {
        return None;
    }
    let room = align_down(end - start, align);
    if room >= want {
        Some((start, want))
    } else if partial && room > 0 {
        Some((start, room))
    } else {
        None
    }
}

/// Размер нового куска. Правило своё: десятая часть тома, не меньше 8 МиБ и не
/// больше гигабайта для данных и 256 МиБ для метаданных, но всегда не меньше
/// того, что просят.
fn chunk_size(kind: u64, total: u64, at_least: u64) -> u64 {
    let cap = if kind == BLOCK_GROUP_DATA { 1024 * MIB } else { 256 * MIB };
    align_down(total / 10, 4 * MIB).clamp(8 * MIB, cap).max(align_up(at_least, 4 * MIB))
}

const fn align_up(value: u64, align: u64) -> u64 {
    value.div_ceil(align) * align
}

const fn align_down(value: u64, align: u64) -> u64 {
    value / align * align
}

// --- элементы -----------------------------------------------------------------------

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 255 || name.contains('/') || name.contains('\0') {
        return Err(Error::BadName);
    }
    Ok(())
}

/// Следующий порядковый номер записи в каталоге. Первые два заняты «.» и «..»,
/// которых в btrfs нет, но нумерация их помнит.
fn next_index(items: &Items, parent: u64) -> u64 {
    let end = items.partition_point(|(key, _)| *key < Key::new(parent, item_type::DIR_INDEX + 1, 0));
    match end.checked_sub(1).map(|at| items[at].0) {
        Some(key) if key.objectid == parent && key.kind == item_type::DIR_INDEX => (key.offset + 1).max(2),
        _ => 2,
    }
}

fn inode_item(generation: u64, mode: u32, size: u64, nbytes: u64, attrs: &Attributes) -> Result<Vec<u8>> {
    item(INODE_ITEM_SIZE, |raw| {
        put_u64(raw, INODE_GENERATION, generation);
        put_u64(raw, INODE_TRANSID, generation);
        put_u64(raw, INODE_SIZE_FIELD, size);
        put_u64(raw, INODE_NBYTES, nbytes);
        put_u32(raw, INODE_NLINK, 1);
        put_u32(raw, INODE_UID, attrs.uid);
        put_u32(raw, INODE_GID, attrs.gid);
        put_u32(raw, INODE_MODE, mode);
        for field in [INODE_ATIME, INODE_CTIME, INODE_MTIME, INODE_OTIME] {
            put_u64(raw, field, attrs.time);
        }
    })
}

fn inode_ref(index: u64, name: &[u8]) -> Result<Vec<u8>> {
    item(INODE_REF_HEAD_SIZE + name.len(), |raw| {
        put_u64(raw, 0, index);
        put_u16(raw, INODE_REF_NAME_LEN, name.len() as u16);
        raw[INODE_REF_HEAD_SIZE..].copy_from_slice(name);
    })
}

fn dir_entry(number: u64, generation: u64, file_type: u8, name: &[u8]) -> Result<Vec<u8>> {
    item(DIR_ITEM_HEAD_SIZE + name.len(), |raw| {
        Key::new(number, item_type::INODE_ITEM, 0).store(raw, DIR_ITEM_LOCATION);
        put_u64(raw, DIR_ITEM_TRANSID, generation);
        put_u16(raw, DIR_ITEM_NAME_LEN, name.len() as u16);
        raw[DIR_ITEM_TYPE] = file_type;
        raw[DIR_ITEM_HEAD_SIZE..].copy_from_slice(name);
    })
}

/// Встроенные ссылки экстента данных: (inode, смещение, счётчик).
fn data_refs(raw: &[u8]) -> Result<Vec<(u64, u64, u32)>> {
    let mut out = Vec::new();
    // Смещения полей ссылки считаются от байта её типа: корень на `+1`,
    // счётчик на `+25`, вся запись — 29 байт.
    let mut at = EXTENT_ITEM_SIZE;
    while at < raw.len() {
        if raw[at] != item_type::EXTENT_DATA_REF || at + DATA_REF_COUNT + 4 > raw.len() {
            return Err(Error::Unsupported);
        }
        if u64_at(raw, at + DATA_REF_ROOT) != objectid::FS_TREE {
            return Err(Error::Unsupported);
        }
        push(
            &mut out,
            (
                u64_at(raw, at + DATA_REF_OBJECTID),
                u64_at(raw, at + DATA_REF_OFFSET),
                u32_at(raw, at + DATA_REF_COUNT),
            ),
        )?;
        at += DATA_REF_COUNT + 4;
    }
    Ok(out)
}

/// Как устроена одна запись среди нескольких подряд: (начало имени, длина
/// имени, длина записи). `None` — запись обрывается раньше, чем кончается.
type Measure = fn(&[u8]) -> Option<(usize, usize, usize)>;

fn dir_entry_len(raw: &[u8]) -> Option<(usize, usize, usize)> {
    if raw.len() < DIR_ITEM_HEAD_SIZE {
        return None;
    }
    let name = u16_at(raw, DIR_ITEM_NAME_LEN) as usize;
    let data = u16_at(raw, DIR_ITEM_DATA_LEN) as usize;
    let total = DIR_ITEM_HEAD_SIZE + name + data;
    (total <= raw.len()).then_some((DIR_ITEM_HEAD_SIZE, name, total))
}

fn inode_ref_len(raw: &[u8]) -> Option<(usize, usize, usize)> {
    if raw.len() < INODE_REF_HEAD_SIZE {
        return None;
    }
    let name = u16_at(raw, INODE_REF_NAME_LEN) as usize;
    let total = INODE_REF_HEAD_SIZE + name;
    (total <= raw.len()).then_some((INODE_REF_HEAD_SIZE, name, total))
}

/// Найти запись с именем среди нескольких подряд. Возвращает (начало, длину).
fn find_named(raw: &[u8], name: &[u8], measure: Measure) -> Result<Option<(usize, usize)>> {
    let mut at = 0;
    while at < raw.len() {
        let (name_at, name_len, total) = measure(&raw[at..]).ok_or(Error::Corrupt)?;
        if raw[at + name_at..at + name_at + name_len] == *name {
            return Ok(Some((at, total)));
        }
        at += total;
    }
    Ok(None)
}

/// Вырезать запись с именем из элемента; пустой элемент исчезает.
fn remove_named_entry(items: &mut Items, key: Key, name: &[u8], measure: Measure) -> Result<()> {
    let at = find(items, key).map_err(|_| Error::Corrupt)?;
    let (start, total) = find_named(&items[at].1, name, measure)?.ok_or(Error::Corrupt)?;
    let data = &mut items[at].1;
    data.drain(start..start + total);
    if data.is_empty() {
        items.remove(at);
    }
    Ok(())
}

fn resize(content: &mut Vec<u8>, len: usize) -> Result<()> {
    if len > content.len() {
        content.try_reserve_exact(len - content.len()).map_err(|_| Error::NoMemory)?;
    }
    content.resize(len, 0);
    Ok(())
}

fn item(len: usize, fill: impl FnOnce(&mut [u8])) -> Result<Vec<u8>> {
    let mut raw = try_zeroed(len)?;
    fill(&mut raw);
    Ok(raw)
}

fn find(items: &Items, key: Key) -> core::result::Result<usize, usize> {
    items.binary_search_by(|(probe, _)| probe.cmp(&key))
}

fn insert(items: &mut Items, key: Key, data: Vec<u8>) -> Result<()> {
    match find(items, key) {
        // Совпавший ключ — ошибка вызывающего: молча заменить значило бы
        // потерять элемент.
        Ok(_) => Err(Error::Corrupt),
        Err(at) => {
            items.try_reserve(1).map_err(|_| Error::NoMemory)?;
            items.insert(at, (key, data));
            Ok(())
        }
    }
}

fn copy(raw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    out.try_reserve_exact(raw.len()).map_err(|_| Error::NoMemory)?;
    out.extend_from_slice(raw);
    Ok(out)
}

fn clone_items(items: &Items) -> Result<Items> {
    let mut out = Vec::new();
    out.try_reserve_exact(items.len()).map_err(|_| Error::NoMemory)?;
    for (key, raw) in items {
        out.push((*key, copy(raw)?));
    }
    Ok(out)
}

fn push<T>(list: &mut Vec<T>, value: T) -> Result<()> {
    list.try_reserve(1).map_err(|_| Error::NoMemory)?;
    list.push(value);
    Ok(())
}
