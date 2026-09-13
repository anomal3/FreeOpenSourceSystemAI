//! Создание пустого тома btrfs — свой `mkfs`.
//!
//! # Что пишется
//!
//! Ровно то, что кладёт в пустой том `mkfs.btrfs -m single -d single` версии
//! 6.8.1: девять деревьев по одному листу, три куска и суперблок с копиями.
//! Раскладка снята с тома, созданного этой программой
//! (`btrfs inspect-internal dump-tree`), а не выведена из описания формата.
//! Описание говорит, какие структуры **допустимы**; нужно было знать, какие
//! **ожидаются** — `btrfs check` сверяет их все разом, и первая же
//! недостающая запись в дереве свободного места делает том «несогласованным».
//!
//! ```text
//!  логический адрес = физическому: каждый кусок лежит там, где адресуется
//!
//!   0 .. 1 МиБ     затирается; суперблок на 64 КиБ
//!   1 МиБ, 4 МиБ   SYSTEM    дерево кусков
//!   5 МиБ, 8 МиБ   METADATA  остальные восемь деревьев подряд
//!  13 МиБ, 8 МиБ   DATA      пусто
//!  64 МиБ          копия суперблока (и дальше, если том достаточно велик)
//! ```
//!
//! # Почему куски такие маленькие
//!
//! `mkfs.btrfs` даёт те же 4 + 8 + 8 МиБ на томе **любого** размера — проверено
//! на 128 МиБ, 1, 2 и 8 ГиБ. Остальное место остаётся нераспределённым, и новые
//! куски заводит тот, кто пишет, — под то, чего не хватило.
//!
//! Разметить весь раздел одним куском данных было бы проще, и это ровно
//! знаменитый тупик btrfs: место под данные есть, а под метаданные взять
//! негде, и том отвечает «нет места» при свободных гигабайтах. Цена названа:
//! писателю (фаза 50b) придётся уметь заводить кусок самому, потому что восемь
//! мегабайт данных кончатся быстро.
//!
//! # Одно поколение на всё
//!
//! На томе от `mkfs.btrfs` у разных деревьев поколения 3, 5 и 6 — следы его
//! многошаговой истории: он сначала строит временный том, а потом
//! переписывает его несколькими транзакциями. Здесь всё создаётся одним
//! действием, поэтому поколение у всего одно — первое. Суперблок, заголовки
//! узлов, элементы корней и дерева экстентов обязаны сходиться в нём: узел
//! «из будущего» читатель отвергает как недописанную транзакцию.
//!
//! # Порядок записи
//!
//! 1. Затирается начало тома и все места копий суперблока.
//! 2. Пишутся деревья, и носителю велят довести их до конца.
//! 3. Пишутся копии суперблока, и **последним** — основной.
//!
//! Первый шаг не про аккуратность. Раздел, на котором уже был btrfs, хранит
//! копию старого суперблока на 64 МиБ, а старые деревья лежат по тем же
//! адресам, что и новые, — `mkfs.btrfs` раскладывает тома одинаково. Прерви
//! форматирование на втором шаге, не затерев копию, — и читатель найдёт
//! старый суперблок, указывающий на деревья, половина которых уже новые. Это
//! не отказ, а правдоподобно смонтированная каша. Так прерванное
//! форматирование оставляет либо старый том целиком, либо никакого, либо новый.
//!
//! # Чем проверяется
//!
//! Своим читателем — в `cargo test`, и это доказывает только согласованность с
//! собой. Чужой взгляд — `cargo xtask btrfs-linux-check`: образ уходит в
//! Linux, где его проверяет `btrfs check`, монтирует ядро, пишет в него файлы,
//! и `btrfs check` проверяет снова.

use alloc::vec::Vec;

use disk::BlockDevice;

use crate::crc32c;
use crate::layout::*;
use crate::node::{self, Stamp};
use crate::read::Btrfs;
use crate::{Error, Result, try_zeroed};

const MIB: u64 = 1024 * 1024;

/// Размер узла дерева — умолчание `mkfs.btrfs`.
const NODESIZE: u32 = 16 * 1024;
const NODE: u64 = NODESIZE as u64;

/// Сектор данных, единица контрольной суммы.
///
/// Равен странице намеренно: ядро Linux долго не монтировало том, у которого
/// сектор отличается от размера страницы, и том для установленной системы не
/// должен зависеть от того, насколько свежее ядро его увидит.
const SECTORSIZE: u32 = 4096;

/// Длина полосы в описании куска. На `single` она ничего не значит, но формат
/// требует именно этого числа.
const STRIPE_LEN: u64 = 64 * 1024;

/// Поколение всего, что создаётся (см. заголовок модуля).
const GENERATION: u64 = 1;

/// Наименьший том, который здесь создаётся.
///
/// Причина своя, а не взятая у `mkfs.btrfs`: том обязан вместить первую копию
/// суперблока на 64 МиБ и оставить ядру место под новые куски сверх трёх
/// начальных. Том без копии суперблока теряет всё при порче одного сектора —
/// а btrfs здесь ради того, чтобы порча переставала быть бедой.
pub const MIN_VOLUME_BYTES: u64 = 128 * MIB;

/// Шаг, которым затирается начало тома.
const ZERO_STEP: usize = 64 * 1024;

/// Один кусок начальной раскладки.
#[derive(Clone, Copy)]
struct Plan {
    start: u64,
    length: u64,
    kind: u64,
    /// Выравнивание в описании куска. У системного куска оно 4096, у двух
    /// других — 65536, а `sub_stripes` — 0 и 1: так пишет `mkfs.btrfs`,
    /// создающий системный кусок другим путём. Ядро эти поля не читает, но
    /// расходиться с эталоном без причины — лишний повод гадать, когда
    /// `btrfs check` чем-то недоволен.
    io_align: u32,
    sub_stripes: u16,
    /// Сколько байт в начале куска занято деревьями.
    used: u64,
}

const SYSTEM: Plan = Plan {
    start: MIB,
    length: 4 * MIB,
    kind: BLOCK_GROUP_SYSTEM,
    io_align: SECTORSIZE,
    sub_stripes: 0,
    used: NODE,
};

const METADATA: Plan = Plan {
    start: 5 * MIB,
    length: 8 * MIB,
    kind: BLOCK_GROUP_METADATA,
    io_align: STRIPE_LEN as u32,
    sub_stripes: 1,
    used: 8 * NODE,
};

const DATA: Plan = Plan {
    start: 13 * MIB,
    length: 8 * MIB,
    kind: BLOCK_GROUP_DATA,
    io_align: STRIPE_LEN as u32,
    sub_stripes: 1,
    used: 0,
};

const CHUNKS: [Plan; 3] = [SYSTEM, METADATA, DATA];

const CHUNK_BLOCK: u64 = SYSTEM.start;
const ROOT_BLOCK: u64 = METADATA.start;
const EXTENT_BLOCK: u64 = METADATA.start + NODE;
const DEV_BLOCK: u64 = METADATA.start + 2 * NODE;
const FS_BLOCK: u64 = METADATA.start + 3 * NODE;
const CSUM_BLOCK: u64 = METADATA.start + 4 * NODE;
const UUID_BLOCK: u64 = METADATA.start + 5 * NODE;
const FREE_SPACE_BLOCK: u64 = METADATA.start + 6 * NODE;
const RELOC_BLOCK: u64 = METADATA.start + 7 * NODE;

/// Лист каждого дерева и дерево, которому он принадлежит.
///
/// Дерево экстентов обязано назвать владельца **каждого** узла, включая
/// собственный и узел дерева кусков: `btrfs check` сверяет эту таблицу с тем,
/// что нашёл, пройдя по деревьям, и узел без записи — это «утёкший» блок.
const BLOCKS: [(u64, u64); 9] = [
    (CHUNK_BLOCK, objectid::CHUNK_TREE),
    (ROOT_BLOCK, objectid::ROOT_TREE),
    (EXTENT_BLOCK, objectid::EXTENT_TREE),
    (DEV_BLOCK, objectid::DEV_TREE),
    (FS_BLOCK, objectid::FS_TREE),
    (CSUM_BLOCK, objectid::CSUM_TREE),
    (UUID_BLOCK, objectid::UUID_TREE),
    (FREE_SPACE_BLOCK, objectid::FREE_SPACE_TREE),
    (RELOC_BLOCK, objectid::DATA_RELOC_TREE),
];

/// Сколько байт занимают деревья — поле `bytes_used` суперблока.
const TREE_BYTES: u64 = BLOCKS.len() as u64 * NODE;

/// Параметры создания тома.
pub struct FormatOptions<'a> {
    /// Метка тома: до 255 байт, без нулевого байта.
    pub label: &'a str,
    /// Идентификатор тома (`fsid`).
    ///
    /// Из него же выводятся идентификаторы устройства, дерева кусков и
    /// подтома: формату нужны четыре разных, а случайных чисел у крейта нет —
    /// их даёт прошивка, о которой он знать не должен.
    pub uuid: [u8; 16],
    /// Время создания, секунды эпохи Unix.
    ///
    /// Задаётся снаружи по той же причине, что у ext2: часы есть у ядра и у
    /// хоста, а не у формата, и фиксированное время даёт воспроизводимый образ.
    pub time: u64,
}

/// Четыре идентификатора тома.
struct Ids {
    fsid: [u8; 16],
    device: [u8; 16],
    chunk_tree: [u8; 16],
    subvolume: [u8; 16],
}

impl Ids {
    /// Вывести остальные три из идентификатора тома.
    ///
    /// Последний байт меняется на разные ненулевые маски, поэтому все четыре
    /// различны при любом исходном значении. Уникальность среди других томов
    /// ровно та же, что у самого `fsid`, — больше и не требуется: ядро Linux
    /// ищет устройство по паре (`fsid`, номер), а не по его собственному UUID.
    fn derive(uuid: [u8; 16]) -> Self {
        let salted = |salt: u8| {
            let mut out = uuid;
            out[15] ^= salt;
            out
        };
        Self { fsid: uuid, device: salted(1), chunk_tree: salted(2), subvolume: salted(3) }
    }
}

/// Создать том btrfs на разделе и смонтировать его.
///
/// Возвращается смонтированный том, а не пустота: разметка, после которой
/// том не читается, — ошибка, и узнать о ней надо здесь, а не при следующей
/// загрузке.
pub fn format(
    dev: &mut dyn BlockDevice,
    first_lba: u64,
    sectors: u64,
    options: &FormatOptions,
) -> Result<Btrfs> {
    disk::check_device(dev)?;
    let end = first_lba.checked_add(sectors).ok_or(Error::TooSmall)?;
    if end > dev.sector_count() {
        return Err(Error::TooSmall);
    }
    // Все адреса ниже кратны 4096, и запись идёт целыми секторами носителя
    // только тогда, когда сектор делит 4096. Для 512 и 4096 это так; носитель
    // с другим сектором получит отказ, а не том со сдвинутыми структурами.
    let sector = u64::from(dev.sector_size());
    if sector == 0 || u64::from(SECTORSIZE) % sector != 0 {
        return Err(Error::Unsupported);
    }
    let bytes = sectors.checked_mul(sector).ok_or(Error::TooSmall)?;
    let total = bytes - bytes % u64::from(SECTORSIZE);
    if total < MIN_VOLUME_BYTES {
        return Err(Error::TooSmall);
    }
    let label = options.label.as_bytes();
    if label.len() >= SB_LABEL_SIZE || label.contains(&0) {
        return Err(Error::BadName);
    }

    let ids = Ids::derive(options.uuid);
    let time = options.time;

    // Все узлы строятся в памяти **до** первой записи: нехватка памяти на
    // середине построения должна оставить раздел нетронутым.
    let nodes = [
        (CHUNK_BLOCK, chunk_tree(&ids, total)?),
        (ROOT_BLOCK, root_tree(&ids, time)?),
        (EXTENT_BLOCK, extent_tree(&ids)?),
        (DEV_BLOCK, dev_tree(&ids)?),
        (FS_BLOCK, fs_tree(&ids, time)?),
        (CSUM_BLOCK, Items::new().leaf(CSUM_BLOCK, objectid::CSUM_TREE, &ids)?),
        (UUID_BLOCK, uuid_tree(&ids)?),
        (FREE_SPACE_BLOCK, free_space_tree(&ids)?),
        (RELOC_BLOCK, reloc_tree(&ids, time)?),
    ];
    let fits = |copy: u64| copy + SUPERBLOCK_SIZE as u64 <= total;

    // 1. Начало тома и места копий суперблока.
    let zeros = try_zeroed(ZERO_STEP)?;
    let mut offset = 0;
    while offset < SYSTEM.start {
        write_at(dev, first_lba, offset, &zeros)?;
        offset += ZERO_STEP as u64;
    }
    for copy in SUPERBLOCK_COPIES.into_iter().filter(|&copy| copy >= SYSTEM.start && fits(copy)) {
        write_at(dev, first_lba, copy, &zeros[..SUPERBLOCK_SIZE])?;
    }

    // 2. Деревья. Логический адрес узла равен физическому — так разложены куски.
    for (address, node) in &nodes {
        write_at(dev, first_lba, *address, node)?;
    }
    dev.flush()?;

    // 3. Суперблок: копии, затем основной.
    for copy in SUPERBLOCK_COPIES.into_iter().rev().filter(|&copy| fits(copy)) {
        let sb = superblock(&ids, label, total, copy)?;
        write_at(dev, first_lba, copy, &sb)?;
    }
    dev.flush()?;

    Btrfs::mount(dev, first_lba)
}

/// Записать блок по байтовому смещению внутри раздела.
fn write_at(dev: &mut dyn BlockDevice, first_lba: u64, offset: u64, buf: &[u8]) -> Result<()> {
    let sector = u64::from(dev.sector_size());
    if offset % sector != 0 || buf.len() as u64 % sector != 0 {
        return Err(Error::Unsupported);
    }
    let lba = first_lba.checked_add(offset / sector).ok_or(Error::TooSmall)?;
    dev.write(lba, buf)?;
    Ok(())
}

/// Нулевой буфер, заполненный вызывающим.
fn item(len: usize, fill: impl FnOnce(&mut [u8])) -> Result<Vec<u8>> {
    let mut raw = try_zeroed(len)?;
    fill(&mut raw);
    Ok(raw)
}

// --- лист ----------------------------------------------------------------------

/// Элементы будущего листа.
struct Items {
    list: Vec<(Key, Vec<u8>)>,
}

impl Items {
    const fn new() -> Self {
        Self { list: Vec::new() }
    }

    fn push(&mut self, key: Key, data: Vec<u8>) -> Result<()> {
        self.list.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.list.push((key, data));
        Ok(())
    }

    /// Уложить элементы в лист и запечатать его.
    ///
    /// Элементы добавлялись в порядке, удобном для чтения кода, — сортируются
    /// здесь. Совпавшие ключи означают ошибку построения, и отдать такой лист
    /// на диск значило бы спрятать один из элементов навсегда.
    fn leaf(mut self, address: u64, owner: u64, ids: &Ids) -> Result<Vec<u8>> {
        self.list.sort_unstable_by_key(|(key, _)| *key);
        let stamp = Stamp {
            nodesize: NODESIZE,
            fsid: ids.fsid,
            chunk_uuid: ids.chunk_tree,
            generation: GENERATION,
        };
        node::leaf(&self.list, address, owner, &stamp)
    }
}

// --- суперблок -------------------------------------------------------------------

fn superblock(ids: &Ids, label: &[u8], total: u64, copy: u64) -> Result<Vec<u8>> {
    let mut sb = try_zeroed(SUPERBLOCK_SIZE)?;
    sb[SB_FSID..SB_FSID + 16].copy_from_slice(&ids.fsid);
    // Каждая копия знает своё место — по нему читатель отличает копию этого
    // тома от чужого суперблока, оказавшегося рядом.
    put_u64(&mut sb, SB_BYTENR, copy);
    put_u64(&mut sb, SB_FLAGS, HEADER_FLAG_WRITTEN);
    put_u64(&mut sb, SB_MAGIC, MAGIC);
    put_u64(&mut sb, SB_GENERATION, GENERATION);
    put_u64(&mut sb, SB_ROOT, ROOT_BLOCK);
    put_u64(&mut sb, SB_CHUNK_ROOT, CHUNK_BLOCK);
    put_u64(&mut sb, SB_TOTAL_BYTES, total);
    put_u64(&mut sb, SB_BYTES_USED, TREE_BYTES);
    put_u64(&mut sb, SB_ROOT_DIR, objectid::ROOT_TREE_DIR);
    put_u64(&mut sb, SB_NUM_DEVICES, 1);
    put_u32(&mut sb, SB_SECTORSIZE, SECTORSIZE);
    put_u32(&mut sb, SB_NODESIZE, NODESIZE);
    put_u32(&mut sb, SB_LEAFSIZE, NODESIZE);
    put_u32(&mut sb, SB_STRIPESIZE, SECTORSIZE);
    put_u64(&mut sb, SB_CHUNK_ROOT_GENERATION, GENERATION);
    put_u64(&mut sb, SB_COMPAT_RO_FLAGS, COMPAT_RO_FREE_SPACE_TREE | COMPAT_RO_FREE_SPACE_TREE_VALID);
    put_u64(&mut sb, SB_INCOMPAT_FLAGS, INCOMPAT_CREATED);
    put_u16(&mut sb, SB_CSUM_TYPE, CSUM_TYPE_CRC32C);
    // Уровни корней — нули: все деревья из одного листа.

    dev_item(&mut sb[SB_DEV_ITEM..SB_DEV_ITEM + DEV_ITEM_SIZE], ids, total);
    sb[SB_LABEL..SB_LABEL + label.len()].copy_from_slice(label);

    // В массиве системных кусков — только системный: ровно столько нужно,
    // чтобы добраться до дерева кусков.
    let array = SB_SYS_CHUNK_ARRAY;
    Key::new(objectid::FIRST_CHUNK_TREE, item_type::CHUNK_ITEM, SYSTEM.start).store(&mut sb, array);
    chunk_item(&mut sb[array + KEY_SIZE..array + KEY_SIZE + CHUNK_ITEM_SIZE], &SYSTEM, ids);
    put_u32(&mut sb, SB_SYS_ARRAY_SIZE, (KEY_SIZE + CHUNK_ITEM_SIZE) as u32);

    // Первая запасная копия корней. Ядро Linux читает их только при
    // `-o usebackuproot`, но на томе, где их нет вовсе, этот способ спасения
    // не работает с первой же транзакции.
    let backup = SB_SUPER_ROOTS;
    for (field, value) in [
        (BACKUP_TREE_ROOT, ROOT_BLOCK),
        (BACKUP_CHUNK_ROOT, CHUNK_BLOCK),
        (BACKUP_EXTENT_ROOT, EXTENT_BLOCK),
        (BACKUP_FS_ROOT, FS_BLOCK),
        (BACKUP_DEV_ROOT, DEV_BLOCK),
        (BACKUP_CSUM_ROOT, CSUM_BLOCK),
    ] {
        put_u64(&mut sb, backup + field, value);
        put_u64(&mut sb, backup + field + 8, GENERATION);
    }
    put_u64(&mut sb, backup + BACKUP_TOTAL_BYTES, total);
    put_u64(&mut sb, backup + BACKUP_BYTES_USED, TREE_BYTES);
    put_u64(&mut sb, backup + BACKUP_NUM_DEVICES, 1);

    node::seal(&mut sb);
    Ok(sb)
}

/// Описание единственного устройства — в суперблоке и в дереве кусков.
fn dev_item(raw: &mut [u8], ids: &Ids, total: u64) {
    put_u64(raw, DEV_ITEM_DEVID, 1);
    put_u64(raw, DEV_ITEM_TOTAL_BYTES, total);
    put_u64(raw, DEV_ITEM_BYTES_USED, CHUNKS.iter().map(|plan| plan.length).sum());
    put_u32(raw, DEV_ITEM_IO_ALIGN, SECTORSIZE);
    put_u32(raw, DEV_ITEM_IO_WIDTH, SECTORSIZE);
    put_u32(raw, DEV_ITEM_SECTOR_SIZE, SECTORSIZE);
    raw[DEV_ITEM_UUID..DEV_ITEM_UUID + 16].copy_from_slice(&ids.device);
    raw[DEV_ITEM_FSID..DEV_ITEM_FSID + 16].copy_from_slice(&ids.fsid);
}

/// Описание куска с одной полосой.
fn chunk_item(raw: &mut [u8], plan: &Plan, ids: &Ids) {
    put_u64(raw, CHUNK_LENGTH, plan.length);
    // Владелец — номер дерева **экстентов**, а не кусков. Это не опечатка, а
    // история формата, которую повторяют и ядро, и `mkfs.btrfs`.
    put_u64(raw, CHUNK_OWNER, objectid::EXTENT_TREE);
    put_u64(raw, CHUNK_STRIPE_LEN, STRIPE_LEN);
    put_u64(raw, CHUNK_TYPE, plan.kind);
    put_u32(raw, CHUNK_IO_ALIGN, plan.io_align);
    put_u32(raw, CHUNK_IO_WIDTH, plan.io_align);
    put_u32(raw, CHUNK_SECTOR_SIZE, SECTORSIZE);
    put_u16(raw, CHUNK_NUM_STRIPES, 1);
    put_u16(raw, CHUNK_SUB_STRIPES, plan.sub_stripes);
    let stripe = CHUNK_HEAD_SIZE;
    put_u64(raw, stripe + STRIPE_DEVID, 1);
    put_u64(raw, stripe + STRIPE_OFFSET, plan.start);
    raw[stripe + STRIPE_DEV_UUID..stripe + STRIPE_DEV_UUID + 16].copy_from_slice(&ids.device);
}

// --- деревья ---------------------------------------------------------------------

fn chunk_tree(ids: &Ids, total: u64) -> Result<Vec<u8>> {
    let mut items = Items::new();
    items.push(
        Key::new(objectid::DEV_ITEMS, item_type::DEV_ITEM, 1),
        item(DEV_ITEM_SIZE, |raw| dev_item(raw, ids, total))?,
    )?;
    for plan in &CHUNKS {
        items.push(
            Key::new(objectid::FIRST_CHUNK_TREE, item_type::CHUNK_ITEM, plan.start),
            item(CHUNK_ITEM_SIZE, |raw| chunk_item(raw, plan, ids))?,
        )?;
    }
    items.leaf(CHUNK_BLOCK, objectid::CHUNK_TREE, ids)
}

/// Имя, под которым дерево ФС значится в каталоге дерева корней.
///
/// Ядро Linux ищет подтом по умолчанию именно по нему, а не по номеру: без
/// записи `default` том монтируется, но `btrfs subvolume get-default` и
/// `btrfs check` считают дерево корней неполным.
const DEFAULT_SUBVOLUME: &[u8] = b"default";

fn root_tree(ids: &Ids, time: u64) -> Result<Vec<u8>> {
    let mut items = Items::new();
    for (tree, address, dirid, uuid, stamp) in [
        (objectid::EXTENT_TREE, EXTENT_BLOCK, 0, [0; 16], 0),
        (objectid::DEV_TREE, DEV_BLOCK, 0, [0; 16], 0),
        // Только у подтома есть свой UUID, корневой каталог и время создания.
        (objectid::FS_TREE, FS_BLOCK, objectid::FIRST_FREE, ids.subvolume, time),
        (objectid::CSUM_TREE, CSUM_BLOCK, 0, [0; 16], 0),
        (objectid::UUID_TREE, UUID_BLOCK, 0, [0; 16], 0),
        (objectid::FREE_SPACE_TREE, FREE_SPACE_BLOCK, 0, [0; 16], 0),
        // У дерева перемещения данных корневой каталог есть, а UUID — нет.
        (objectid::DATA_RELOC_TREE, RELOC_BLOCK, objectid::FIRST_FREE, [0; 16], 0),
    ] {
        items.push(
            Key::new(tree, item_type::ROOT_ITEM, 0),
            root_item(address, dirid, uuid, stamp)?,
        )?;
    }

    // Каталог дерева корней: inode, ссылка на себя и запись `default`.
    items.push(
        Key::new(objectid::ROOT_TREE_DIR, item_type::INODE_ITEM, 0),
        directory_inode(NODE, time, time)?,
    )?;
    items.push(
        Key::new(objectid::ROOT_TREE_DIR, item_type::INODE_REF, objectid::ROOT_TREE_DIR),
        inode_ref(b"..")?,
    )?;
    items.push(
        Key::new(
            objectid::ROOT_TREE_DIR,
            item_type::DIR_ITEM,
            crc32c::name_hash(DEFAULT_SUBVOLUME),
        ),
        item(DIR_ITEM_HEAD_SIZE + DEFAULT_SUBVOLUME.len(), |raw| {
            // Запись указывает не на inode, а на корень подтома: смещение
            // `u64::MAX` значит «последний снимок», то есть сам подтом.
            Key::new(objectid::FS_TREE, item_type::ROOT_ITEM, u64::MAX)
                .store(raw, DIR_ITEM_LOCATION);
            put_u16(raw, DIR_ITEM_NAME_LEN, DEFAULT_SUBVOLUME.len() as u16);
            raw[DIR_ITEM_TYPE] = FILE_TYPE_DIRECTORY;
            raw[DIR_ITEM_HEAD_SIZE..].copy_from_slice(DEFAULT_SUBVOLUME);
        })?,
    )?;
    // Обратная ссылка: подтом знает, под каким именем он записан.
    items.push(
        Key::new(objectid::FS_TREE, item_type::INODE_REF, objectid::ROOT_TREE_DIR),
        inode_ref(DEFAULT_SUBVOLUME)?,
    )?;
    items.leaf(ROOT_BLOCK, objectid::ROOT_TREE, ids)
}

fn root_item(address: u64, dirid: u64, uuid: [u8; 16], stamp: u64) -> Result<Vec<u8>> {
    item(ROOT_ITEM_SIZE, |raw| {
        // Встроенный inode ядро не читает, но `mkfs.btrfs` заполняет его
        // одинаково у всех деревьев — повторено байт в байт.
        put_u64(raw, INODE_GENERATION, 1);
        put_u64(raw, INODE_SIZE_FIELD, 3);
        put_u64(raw, INODE_NBYTES, NODE);
        put_u32(raw, INODE_NLINK, 1);
        put_u32(raw, INODE_MODE, MODE_DIRECTORY | 0o755);

        put_u64(raw, ROOT_ITEM_GENERATION, GENERATION);
        put_u64(raw, ROOT_ITEM_DIRID, dirid);
        put_u64(raw, ROOT_ITEM_BYTENR, address);
        put_u64(raw, ROOT_ITEM_BYTES_USED, NODE);
        put_u32(raw, ROOT_ITEM_REFS, 1);
        raw[ROOT_ITEM_LEVEL] = 0;
        // Второе поколение обязано совпасть с первым: по расхождению ядро
        // узнаёт элемент, записанный старым ядром, и не верит новым полям.
        put_u64(raw, ROOT_ITEM_GENERATION_V2, GENERATION);
        raw[ROOT_ITEM_UUID..ROOT_ITEM_UUID + 16].copy_from_slice(&uuid);
        put_u64(raw, ROOT_ITEM_CTIME, stamp);
        put_u64(raw, ROOT_ITEM_OTIME, stamp);
    })
}

/// Inode каталога `0755 root:root`.
///
/// `nbytes` у корневых каталогов `mkfs.btrfs` пишет равным размеру узла, а у
/// дерева перемещения — нулю; `btrfs check` принимает оба, и оба повторены.
fn directory_inode(nbytes: u64, time: u64, otime: u64) -> Result<Vec<u8>> {
    item(INODE_ITEM_SIZE, |raw| {
        put_u64(raw, INODE_GENERATION, GENERATION);
        put_u64(raw, INODE_NBYTES, nbytes);
        put_u32(raw, INODE_NLINK, 1);
        put_u32(raw, INODE_MODE, MODE_DIRECTORY | 0o755);
        put_u64(raw, INODE_ATIME, time);
        put_u64(raw, INODE_CTIME, time);
        put_u64(raw, INODE_MTIME, time);
        put_u64(raw, INODE_OTIME, otime);
    })
}

fn inode_ref(name: &[u8]) -> Result<Vec<u8>> {
    item(INODE_REF_HEAD_SIZE + name.len(), |raw| {
        put_u16(raw, INODE_REF_NAME_LEN, name.len() as u16);
        raw[INODE_REF_HEAD_SIZE..].copy_from_slice(name);
    })
}

fn extent_tree(ids: &Ids) -> Result<Vec<u8>> {
    let mut items = Items::new();
    for (address, owner) in BLOCKS {
        // Короткая запись (`skinny metadata`): уровень узла лежит в ключе, а
        // единственная ссылка — прямо в элементе.
        items.push(
            Key::new(address, item_type::METADATA_ITEM, 0),
            item(METADATA_ITEM_SIZE, |raw| {
                put_u64(raw, EXTENT_ITEM_REFS, 1);
                put_u64(raw, EXTENT_ITEM_GENERATION, GENERATION);
                put_u64(raw, EXTENT_ITEM_FLAGS, EXTENT_FLAG_TREE_BLOCK);
                raw[EXTENT_ITEM_SIZE] = item_type::TREE_BLOCK_REF;
                put_u64(raw, EXTENT_ITEM_SIZE + 1, owner);
            })?,
        )?;
    }
    for plan in &CHUNKS {
        items.push(
            Key::new(plan.start, item_type::BLOCK_GROUP_ITEM, plan.length),
            item(BLOCK_GROUP_ITEM_SIZE, |raw| {
                put_u64(raw, BLOCK_GROUP_USED, plan.used);
                put_u64(raw, BLOCK_GROUP_CHUNK_OBJECTID, objectid::FIRST_CHUNK_TREE);
                put_u64(raw, BLOCK_GROUP_FLAGS, plan.kind);
            })?,
        )?;
    }
    items.leaf(EXTENT_BLOCK, objectid::EXTENT_TREE, ids)
}

fn dev_tree(ids: &Ids) -> Result<Vec<u8>> {
    let mut items = Items::new();
    for plan in &CHUNKS {
        // Обратная сторона описания куска: какой отрезок устройства им занят.
        items.push(
            Key::new(1, item_type::DEV_EXTENT, plan.start),
            item(DEV_EXTENT_SIZE, |raw| {
                put_u64(raw, DEV_EXTENT_CHUNK_TREE, objectid::CHUNK_TREE);
                put_u64(raw, DEV_EXTENT_CHUNK_OBJECTID, objectid::FIRST_CHUNK_TREE);
                put_u64(raw, DEV_EXTENT_CHUNK_OFFSET, plan.start);
                put_u64(raw, DEV_EXTENT_LENGTH, plan.length);
                raw[DEV_EXTENT_UUID..DEV_EXTENT_UUID + 16].copy_from_slice(&ids.chunk_tree);
            })?,
        )?;
    }
    items.leaf(DEV_BLOCK, objectid::DEV_TREE, ids)
}

fn fs_tree(ids: &Ids, time: u64) -> Result<Vec<u8>> {
    let mut items = Items::new();
    let root = objectid::FIRST_FREE;
    items.push(Key::new(root, item_type::INODE_ITEM, 0), directory_inode(NODE, time, time)?)?;
    items.push(Key::new(root, item_type::INODE_REF, root), inode_ref(b"..")?)?;
    items.leaf(FS_BLOCK, objectid::FS_TREE, ids)
}

fn uuid_tree(ids: &Ids) -> Result<Vec<u8>> {
    let mut items = Items::new();
    // Ключ — сам UUID подтома, разрезанный на две половины по восемь байт.
    let mut high = [0u8; 8];
    let mut low = [0u8; 8];
    high.copy_from_slice(&ids.subvolume[..8]);
    low.copy_from_slice(&ids.subvolume[8..]);
    items.push(
        Key::new(u64::from_le_bytes(high), item_type::UUID_KEY_SUBVOL, u64::from_le_bytes(low)),
        item(8, |raw| put_u64(raw, 0, objectid::FS_TREE))?,
    )?;
    items.leaf(UUID_BLOCK, objectid::UUID_TREE, ids)
}

fn free_space_tree(ids: &Ids) -> Result<Vec<u8>> {
    let mut items = Items::new();
    for plan in &CHUNKS {
        // Деревья лежат в начале своих кусков подряд, поэтому свободное место
        // каждого куска — один отрезок от конца занятого до конца куска.
        let free = plan.length - plan.used;
        items.push(
            Key::new(plan.start, item_type::FREE_SPACE_INFO, plan.length),
            item(FREE_SPACE_INFO_SIZE, |raw| {
                put_u32(raw, FREE_SPACE_INFO_EXTENT_COUNT, u32::from(free > 0));
            })?,
        )?;
        if free > 0 {
            items.push(
                Key::new(plan.start + plan.used, item_type::FREE_SPACE_EXTENT, free),
                Vec::new(),
            )?;
        }
    }
    items.leaf(FREE_SPACE_BLOCK, objectid::FREE_SPACE_TREE, ids)
}

fn reloc_tree(ids: &Ids, time: u64) -> Result<Vec<u8>> {
    let mut items = Items::new();
    let root = objectid::FIRST_FREE;
    // Времени создания у этого каталога `mkfs.btrfs` не ставит.
    items.push(Key::new(root, item_type::INODE_ITEM, 0), directory_inode(0, time, 0)?)?;
    items.push(Key::new(root, item_type::INODE_REF, root), inode_ref(b"..")?)?;
    items.leaf(RELOC_BLOCK, objectid::DATA_RELOC_TREE, ids)
}
