//! Раскладка btrfs на диске: смещения полей, ключи и разбор структур.
//!
//! # Как устроен том
//!
//! ```text
//!  байт 65536      суперблок (копии на 64 МиБ, 256 ГиБ и 1 ПиБ)
//!  далее           всё остальное адресуется ЛОГИЧЕСКИ, а не по диску:
//!
//!    дерево кусков (chunk tree)   логический адрес -> физический
//!    дерево корней (root tree)    где лежит корень каждого дерева
//!    дерево ФС     (fs tree)      inode, каталоги, экстенты
//!    дерево сумм   (csum tree)    crc32c каждого сектора данных
//! ```
//!
//! Курица и яйцо: чтобы прочитать дерево кусков, нужен перевод адреса, а его
//! даёт дерево кусков. Разрывается это массивом `sys_chunk_array` внутри
//! суперблока — там лежат куски, которых хватает, чтобы добраться до корня
//! дерева кусков, и только они.
//!
//! # Все структуры упакованы
//!
//! Поля лежат без выравнивания: `chunk_root_generation` начинается на 164-м
//! байте суперблока, `dev_item` — на 201-м. Поэтому здесь нет ни `repr(C)`, ни
//! чтения структур «наложением» — только явные смещения и `from_le_bytes`.
//! Наложение на неровный адрес в `no_std` — это не медленно, это неопределённое
//! поведение.
//!
//! # Одно дерево, два вида узлов
//!
//! ```text
//!  заголовок (101 байт, одинаков у обоих)
//!  level > 0:  массив из nritems пар (ключ, адрес потомка)  — по 33 байта
//!  level == 0: массив из nritems описаний (ключ, смещение, длина) — по 25,
//!              а сами данные растут навстречу, от конца узла
//! ```

use crate::crc32c;
use crate::{Error, Result};

// --- суперблок ---------------------------------------------------------------

/// Смещение суперблока от начала тома. Не зависит ни от чего: 64 КиБ в начале
/// оставлены под чужие загрузочные записи.
pub(crate) const SUPERBLOCK_OFFSET: u64 = 0x1_0000;
pub(crate) const SUPERBLOCK_SIZE: usize = 4096;

/// Смещения копий суперблока от начала тома.
///
/// Копий четыре, но последние две существуют только на томах в сотни гигабайт
/// и петабайты. Читается первая доступная: том, у которого испорчен первый
/// суперблок, обязан монтироваться со второго — иначе резервные копии не
/// делают ничего.
pub(crate) const SUPERBLOCK_COPIES: [u64; 4] = [
    SUPERBLOCK_OFFSET,
    0x400_0000,
    0x40_0000_0000,
    0x4_0000_0000_0000,
];

/// `_BHRfS_M` — подпись btrfs, восемь байт на 64-м.
pub(crate) const MAGIC: u64 = 0x4D5F_5366_5248_425F;

pub(crate) const SB_FSID: usize = 32;
pub(crate) const SB_BYTENR: usize = 48;
pub(crate) const SB_MAGIC: usize = 64;
pub(crate) const SB_GENERATION: usize = 72;
pub(crate) const SB_ROOT: usize = 80;
pub(crate) const SB_CHUNK_ROOT: usize = 88;
pub(crate) const SB_LOG_ROOT: usize = 96;
pub(crate) const SB_TOTAL_BYTES: usize = 112;
pub(crate) const SB_BYTES_USED: usize = 120;
pub(crate) const SB_NUM_DEVICES: usize = 136;
pub(crate) const SB_SECTORSIZE: usize = 144;
pub(crate) const SB_NODESIZE: usize = 148;
pub(crate) const SB_SYS_ARRAY_SIZE: usize = 160;
pub(crate) const SB_INCOMPAT_FLAGS: usize = 188;
pub(crate) const SB_CSUM_TYPE: usize = 196;
pub(crate) const SB_ROOT_LEVEL: usize = 198;
pub(crate) const SB_CHUNK_ROOT_LEVEL: usize = 199;
pub(crate) const SB_LABEL: usize = 299;
pub(crate) const SB_LABEL_SIZE: usize = 256;
pub(crate) const SB_SYS_CHUNK_ARRAY: usize = 811;
pub(crate) const SB_SYS_CHUNK_ARRAY_SIZE: usize = 2048;

/// Контрольная сумма считается со смещения 32 и до конца блока: первые 32
/// байта — сама сумма, и включать её в счёт нельзя.
pub(crate) const CSUM_SIZE: usize = 32;

/// Тип контрольной суммы. Кроме нуля (crc32c) бывают xxhash, sha256 и blake2,
/// и все три означают, что читать том нечем.
pub(crate) const CSUM_TYPE_CRC32C: u16 = 0;

// --- возможности формата -----------------------------------------------------

/// Возможности, с которыми читатель справляется.
///
/// Список не «что видели», а «что разобрано в коде». Любой бит вне его
/// означает том, в котором есть структуры, нам неизвестные, — и лучше честный
/// отказ, чем половина файлов.
pub(crate) const INCOMPAT_SUPPORTED: u64 = INCOMPAT_MIXED_BACKREF
    | INCOMPAT_BIG_METADATA
    | INCOMPAT_EXTENDED_IREF
    | INCOMPAT_SKINNY_METADATA
    | INCOMPAT_NO_HOLES;

/// Обратные ссылки смешанного вида. На чтение не влияет: мы не ходим по дереву
/// экстентов вовсе.
pub(crate) const INCOMPAT_MIXED_BACKREF: u64 = 1 << 0;
/// Узлы больше страницы. Читателю безразлично — размер берётся из суперблока.
pub(crate) const INCOMPAT_BIG_METADATA: u64 = 1 << 5;
/// Длинные обратные ссылки inode. Мы их не читаем.
pub(crate) const INCOMPAT_EXTENDED_IREF: u64 = 1 << 6;
/// Укороченные записи метаданных в дереве экстентов. Мы туда не ходим.
pub(crate) const INCOMPAT_SKINNY_METADATA: u64 = 1 << 8;
/// Дыры в файле не описываются записью, их просто нет в дереве. **Это влияет
/// на чтение**: без разбора признака дыра выглядела бы концом файла.
pub(crate) const INCOMPAT_NO_HOLES: u64 = 1 << 9;

// --- ключ --------------------------------------------------------------------

/// Размер ключа на диске: 8 + 1 + 8, без выравнивания.
pub(crate) const KEY_SIZE: usize = 17;

/// Ключ элемента дерева.
///
/// Порядок — лексикографический по тройке, и все три части **беззнаковые**.
/// Это важнее, чем кажется: дерево сумм живёт под объектом `-10`, то есть
/// `0xFFFF_FFFF_FFFF_FFF6`, и при знаковом сравнении оказалось бы перед корнем
/// тома, а не после него.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub objectid: u64,
    pub kind: u8,
    pub offset: u64,
}

impl Key {
    #[must_use]
    pub const fn new(objectid: u64, kind: u8, offset: u64) -> Self {
        Self { objectid, kind, offset }
    }

    /// Наименьший ключ с таким объектом и типом.
    #[must_use]
    pub const fn first(objectid: u64, kind: u8) -> Self {
        Self::new(objectid, kind, 0)
    }

    /// Следующий ключ в порядке дерева.
    ///
    /// Нужен обходу: «продолжить после этого» выражается как «искать не меньше
    /// следующего». Переполнение обрабатывается переносом в старшую часть —
    /// без этого обход остановился бы на элементе со смещением `u64::MAX`,
    /// которое в дереве сумм встречается не как курьёз, а как обычное дело.
    #[must_use]
    pub const fn next(self) -> Self {
        if self.offset != u64::MAX {
            Self::new(self.objectid, self.kind, self.offset + 1)
        } else if self.kind != u8::MAX {
            Self::new(self.objectid, self.kind + 1, 0)
        } else {
            Self::new(self.objectid.wrapping_add(1), 0, 0)
        }
    }

    pub(crate) fn parse(raw: &[u8], at: usize) -> Self {
        Self {
            objectid: u64_at(raw, at),
            kind: raw[at + 8],
            offset: u64_at(raw, at + 9),
        }
    }

    /// Записать ключ в том же упакованном виде, в каком он читается.
    pub(crate) fn store(self, raw: &mut [u8], at: usize) {
        put_u64(raw, at, self.objectid);
        raw[at + 8] = self.kind;
        put_u64(raw, at + 9, self.offset);
    }
}

/// Типы элементов: те, что читатель понимает, и те, что пишет `mkfs`.
pub mod item_type {
    pub const INODE_ITEM: u8 = 1;
    pub const INODE_REF: u8 = 12;
    pub const DIR_ITEM: u8 = 84;
    pub const DIR_INDEX: u8 = 96;
    pub const EXTENT_DATA: u8 = 108;
    pub const EXTENT_CSUM: u8 = 128;
    pub const ROOT_ITEM: u8 = 132;
    /// Экстент данных в дереве экстентов; длина лежит в смещении ключа.
    pub const EXTENT_ITEM: u8 = 168;
    /// Узел дерева в дереве экстентов; уровень узла лежит в смещении ключа.
    pub const METADATA_ITEM: u8 = 169;
    /// Ссылка «этот узел принадлежит дереву N».
    pub const TREE_BLOCK_REF: u8 = 176;
    /// Ссылка «эти данные — у такого-то файла такого-то дерева».
    pub const EXTENT_DATA_REF: u8 = 178;
    /// Ссылка на узел через родителя — признак общих узлов (снимков).
    pub const SHARED_BLOCK_REF: u8 = 182;
    /// Ссылка на данные через родителя — тоже от снимков.
    pub const SHARED_DATA_REF: u8 = 184;
    /// Длинная обратная ссылка inode — появляется у сотен жёстких ссылок.
    pub const INODE_EXTREF: u8 = 13;
    pub const BLOCK_GROUP_ITEM: u8 = 192;
    pub const FREE_SPACE_INFO: u8 = 198;
    pub const FREE_SPACE_EXTENT: u8 = 199;
    pub const DEV_EXTENT: u8 = 204;
    pub const DEV_ITEM: u8 = 216;
    pub const CHUNK_ITEM: u8 = 228;
    pub const UUID_KEY_SUBVOL: u8 = 251;
}

/// Номера деревьев и особых объектов.
pub(crate) mod objectid {
    /// Дерево корней. Его собственный корень лежит в суперблоке.
    pub const ROOT_TREE: u64 = 1;
    pub const EXTENT_TREE: u64 = 2;
    pub const CHUNK_TREE: u64 = 3;
    pub const DEV_TREE: u64 = 4;
    /// Под этим номером в дереве кусков лежат описания устройств.
    pub const DEV_ITEMS: u64 = 1;
    /// Дерево файловой системы — то самое, где лежат файлы.
    pub const FS_TREE: u64 = 5;
    /// Каталог внутри дерева корней, где подтома записаны по именам.
    pub const ROOT_TREE_DIR: u64 = 6;
    /// Дерево контрольных сумм данных.
    pub const CSUM_TREE: u64 = 7;
    pub const UUID_TREE: u64 = 9;
    pub const FREE_SPACE_TREE: u64 = 10;
    /// Дерево перемещения данных: `-9` в беззнаковом виде.
    pub const DATA_RELOC_TREE: u64 = u64::MAX - 8;
    /// Объект, под которым лежат все суммы: `-10` в беззнаковом виде.
    pub const EXTENT_CSUM: u64 = u64::MAX - 9;
    /// Первый номер, который достаётся файлам и каталогам. Корневой каталог
    /// подтома — всегда он.
    pub const FIRST_FREE: u64 = 256;
    /// Последний номер для файлов: выше лежат особые объекты (`-255` и ниже).
    pub const LAST_FREE: u64 = u64::MAX - 256;
    /// Объект, под которым в дереве кусков лежат сами куски.
    pub const FIRST_CHUNK_TREE: u64 = 256;
}

// --- заголовок узла ----------------------------------------------------------

/// Размер заголовка узла: 101 байт, и он же — начало отсчёта смещений данных в
/// листе.
pub(crate) const HEADER_SIZE: usize = 101;

pub(crate) const HDR_FSID: usize = 32;
pub(crate) const HDR_BYTENR: usize = 48;
pub(crate) const HDR_GENERATION: usize = 80;
pub(crate) const HDR_NRITEMS: usize = 96;
pub(crate) const HDR_LEVEL: usize = 100;

/// Размер описания элемента в листе: ключ + смещение + длина.
pub(crate) const ITEM_SIZE: usize = KEY_SIZE + 8;
/// Размер указателя на потомка во внутреннем узле: ключ + адрес + поколение.
pub(crate) const KEY_PTR_SIZE: usize = KEY_SIZE + 16;

/// Разобранный заголовок узла.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Header {
    pub bytenr: u64,
    /// Поколение, в котором узел записан.
    ///
    /// Сверяется с поколением суперблока: узел «из будущего» означает, что
    /// прочитан блок, который транзакция ещё не подтвердила, — а такой блок
    /// может быть записан наполовину, и сумма у него сойдётся, потому что
    /// писалась она от той же половины.
    pub generation: u64,
    pub nritems: u32,
    pub level: u8,
}

impl Header {
    pub(crate) fn parse(node: &[u8]) -> Result<Self> {
        if node.len() < HEADER_SIZE {
            return Err(Error::Corrupt);
        }
        Ok(Self {
            bytenr: u64_at(node, HDR_BYTENR),
            generation: u64_at(node, HDR_GENERATION),
            nritems: u32_at(node, HDR_NRITEMS),
            level: node[HDR_LEVEL],
        })
    }
}

/// Ключ `index`-го элемента листа.
pub(crate) fn leaf_key(node: &[u8], index: usize) -> Key {
    Key::parse(node, HEADER_SIZE + index * ITEM_SIZE)
}

/// Данные `index`-го элемента листа.
///
/// Смещение в описании считается **от конца заголовка**, а не от начала узла:
/// данные растут навстречу описаниям, от конца блока. Забыть про эту сотню с
/// небольшим байт значит читать чужой элемент — и не заметить этого, потому
/// что там тоже лежит что-то похожее на структуру.
pub(crate) fn leaf_item<'a>(node: &'a [u8], index: usize) -> Result<&'a [u8]> {
    let at = HEADER_SIZE + index * ITEM_SIZE;
    if at + ITEM_SIZE > node.len() {
        return Err(Error::Corrupt);
    }
    let offset = u32_at(node, at + KEY_SIZE) as usize;
    let size = u32_at(node, at + KEY_SIZE + 4) as usize;
    let start = HEADER_SIZE
        .checked_add(offset)
        .ok_or(Error::Corrupt)?;
    let end = start.checked_add(size).ok_or(Error::Corrupt)?;
    if end > node.len() {
        return Err(Error::Corrupt);
    }
    Ok(&node[start..end])
}

/// Ключ `index`-го потомка внутреннего узла.
pub(crate) fn node_key(node: &[u8], index: usize) -> Key {
    Key::parse(node, HEADER_SIZE + index * KEY_PTR_SIZE)
}

/// Логический адрес `index`-го потомка внутреннего узла.
pub(crate) fn node_child(node: &[u8], index: usize) -> u64 {
    u64_at(node, HEADER_SIZE + index * KEY_PTR_SIZE + KEY_SIZE)
}

// --- куски -------------------------------------------------------------------

/// Размер описания куска без полос.
pub(crate) const CHUNK_HEAD_SIZE: usize = 48;
/// Размер описания одной полосы.
pub(crate) const STRIPE_SIZE: usize = 32;

pub(crate) const CHUNK_LENGTH: usize = 0;
pub(crate) const CHUNK_TYPE: usize = 24;
pub(crate) const CHUNK_NUM_STRIPES: usize = 44;
pub(crate) const STRIPE_DEVID: usize = 0;
pub(crate) const STRIPE_OFFSET: usize = 8;

/// Биты вида размещения в поле `type` куска: с третьего по десятый.
///
/// Читатель работает только с `single` (ни одного бита) и `dup`; всё
/// остальное — разные виды RAID, где физический адрес зависит от полосы, и
/// придумывать эту арифметику вслепую нельзя. `single` — это именно **ноль**
/// бит профиля, отдельного признака у него нет.
pub(crate) const BLOCK_GROUP_PROFILE_MASK: u64 = 0x7F8;
/// `DUP` — две копии на одном устройстве. Читаем первую.
///
/// Значение проверено на образе от `mkfs.btrfs` с умолчаниями: там системный
/// кусок выходит `SYSTEM|DUP`, то есть `0x22`.
pub(crate) const BLOCK_GROUP_DUP: u64 = 1 << 5;

// --- inode -------------------------------------------------------------------

pub(crate) const INODE_ITEM_SIZE: usize = 160;
pub(crate) const INODE_SIZE_FIELD: usize = 16;
pub(crate) const INODE_NLINK: usize = 40;
pub(crate) const INODE_UID: usize = 44;
pub(crate) const INODE_GID: usize = 48;
pub(crate) const INODE_MODE: usize = 52;
pub(crate) const INODE_FLAGS: usize = 64;
pub(crate) const INODE_MTIME: usize = 136;

/// «Не считать контрольных сумм для данных этого файла».
///
/// Единственный флаг inode, который меняет чтение: без него отсутствие суммы —
/// это порча, а с ним — норма.
pub(crate) const INODE_FLAG_NODATASUM: u64 = 1 << 0;

pub(crate) const MODE_FORMAT_MASK: u32 = 0xF000;
pub(crate) const MODE_DIRECTORY: u32 = 0x4000;
pub(crate) const MODE_REGULAR: u32 = 0x8000;

// --- запись каталога ---------------------------------------------------------

/// Размер шапки записи каталога; за ней идут имя и данные.
pub(crate) const DIR_ITEM_HEAD_SIZE: usize = 30;
pub(crate) const DIR_ITEM_LOCATION: usize = 0;
pub(crate) const DIR_ITEM_DATA_LEN: usize = 25;
pub(crate) const DIR_ITEM_NAME_LEN: usize = 27;

// --- экстент файла -----------------------------------------------------------

pub(crate) const EXTENT_RAM_BYTES: usize = 8;
pub(crate) const EXTENT_COMPRESSION: usize = 16;
pub(crate) const EXTENT_ENCRYPTION: usize = 17;
pub(crate) const EXTENT_TYPE: usize = 20;
/// Встроенные данные начинаются сразу за признаком вида.
pub(crate) const EXTENT_INLINE_DATA: usize = 21;
pub(crate) const EXTENT_DISK_BYTENR: usize = 21;
pub(crate) const EXTENT_OFFSET: usize = 37;
pub(crate) const EXTENT_NUM_BYTES: usize = 45;
/// Размер записи об обычном (не встроенном) экстенте.
pub(crate) const EXTENT_REGULAR_SIZE: usize = 53;

pub(crate) const EXTENT_TYPE_INLINE: u8 = 0;
pub(crate) const EXTENT_TYPE_REGULAR: u8 = 1;
pub(crate) const EXTENT_TYPE_PREALLOC: u8 = 2;

// --- элемент дерева корней ---------------------------------------------------

/// Адрес корня дерева лежит за встроенным inode.
pub(crate) const ROOT_ITEM_BYTENR: usize = INODE_ITEM_SIZE + 16;
/// Уровень корня — однобайтовое поле далеко за ним.
pub(crate) const ROOT_ITEM_LEVEL: usize = 238;
/// Столько байт занимает элемент дерева корней целиком.
pub(crate) const ROOT_ITEM_MIN_SIZE: usize = 239;

// --- то, что нужно только тому, кто создаёт том -------------------------------
//
// Читателю эти поля безразличны, но без любого из них `btrfs check` объявит
// том несогласованным. Смещения сверены с байтами тома от `mkfs.btrfs` 6.8.1
// (`od` по адресам из `btrfs inspect-internal dump-tree`), а не только с
// описанием формата.

pub(crate) const SB_FLAGS: usize = 56;
pub(crate) const SB_ROOT_DIR: usize = 128;
pub(crate) const SB_LEAFSIZE: usize = 152;
pub(crate) const SB_STRIPESIZE: usize = 156;
pub(crate) const SB_CHUNK_ROOT_GENERATION: usize = 164;
pub(crate) const SB_COMPAT_RO_FLAGS: usize = 180;
pub(crate) const SB_DEV_ITEM: usize = 201;
/// Четыре запасных набора корней, по 168 байт.
pub(crate) const SB_SUPER_ROOTS: usize = 2859;

pub(crate) const BACKUP_TREE_ROOT: usize = 0;
pub(crate) const BACKUP_CHUNK_ROOT: usize = 16;
pub(crate) const BACKUP_EXTENT_ROOT: usize = 32;
pub(crate) const BACKUP_FS_ROOT: usize = 48;
pub(crate) const BACKUP_DEV_ROOT: usize = 64;
pub(crate) const BACKUP_CSUM_ROOT: usize = 80;
pub(crate) const BACKUP_TOTAL_BYTES: usize = 96;
pub(crate) const BACKUP_BYTES_USED: usize = 104;
pub(crate) const BACKUP_NUM_DEVICES: usize = 112;

/// Том ведёт дерево свободного места, и оно согласовано с деревом экстентов.
///
/// Без второго бита ядро Linux при первом монтировании перестроило бы дерево
/// само — то есть молча переписало бы то, что мы проверяли.
pub(crate) const COMPAT_RO_FREE_SPACE_TREE: u64 = 1 << 0;
pub(crate) const COMPAT_RO_FREE_SPACE_TREE_VALID: u64 = 1 << 1;

/// Возможности, с которыми создаётся том: те же, что у `mkfs.btrfs` 6.8.1 по
/// умолчанию, — `0x341` в его `dump-super`.
pub(crate) const INCOMPAT_CREATED: u64 =
    INCOMPAT_MIXED_BACKREF | INCOMPAT_EXTENDED_IREF | INCOMPAT_SKINNY_METADATA | INCOMPAT_NO_HOLES;

pub(crate) const HDR_FLAGS: usize = 56;
pub(crate) const HDR_CHUNK_TREE_UUID: usize = 64;
pub(crate) const HDR_OWNER: usize = 88;

/// Блок записан. Тот же бит стоит и в поле флагов суперблока.
pub(crate) const HEADER_FLAG_WRITTEN: u64 = 1 << 0;
/// Версия обратных ссылок живёт в старшем байте флагов узла. Первая —
/// «смешанные», единственная, которую понимает современное ядро.
pub(crate) const HEADER_BACKREF_REV_MIXED: u64 = 1 << 56;

pub(crate) const DEV_ITEM_SIZE: usize = 98;
pub(crate) const DEV_ITEM_DEVID: usize = 0;
pub(crate) const DEV_ITEM_TOTAL_BYTES: usize = 8;
pub(crate) const DEV_ITEM_BYTES_USED: usize = 16;
pub(crate) const DEV_ITEM_IO_ALIGN: usize = 24;
pub(crate) const DEV_ITEM_IO_WIDTH: usize = 28;
pub(crate) const DEV_ITEM_SECTOR_SIZE: usize = 32;
pub(crate) const DEV_ITEM_UUID: usize = 66;
pub(crate) const DEV_ITEM_FSID: usize = 82;

/// Описание куска с одной полосой.
pub(crate) const CHUNK_ITEM_SIZE: usize = CHUNK_HEAD_SIZE + STRIPE_SIZE;
pub(crate) const CHUNK_OWNER: usize = 8;
pub(crate) const CHUNK_STRIPE_LEN: usize = 16;
pub(crate) const CHUNK_IO_ALIGN: usize = 32;
pub(crate) const CHUNK_IO_WIDTH: usize = 36;
pub(crate) const CHUNK_SECTOR_SIZE: usize = 40;
pub(crate) const CHUNK_SUB_STRIPES: usize = 46;
pub(crate) const STRIPE_DEV_UUID: usize = 16;

pub(crate) const BLOCK_GROUP_DATA: u64 = 1 << 0;
pub(crate) const BLOCK_GROUP_SYSTEM: u64 = 1 << 1;
pub(crate) const BLOCK_GROUP_METADATA: u64 = 1 << 2;

pub(crate) const BLOCK_GROUP_ITEM_SIZE: usize = 24;
pub(crate) const BLOCK_GROUP_USED: usize = 0;
pub(crate) const BLOCK_GROUP_CHUNK_OBJECTID: usize = 8;
pub(crate) const BLOCK_GROUP_FLAGS: usize = 16;

pub(crate) const EXTENT_ITEM_SIZE: usize = 24;
pub(crate) const EXTENT_ITEM_REFS: usize = 0;
pub(crate) const EXTENT_ITEM_GENERATION: usize = 8;
pub(crate) const EXTENT_ITEM_FLAGS: usize = 16;
pub(crate) const EXTENT_FLAG_TREE_BLOCK: u64 = 1 << 1;
/// Запись об узле дерева с одной встроенной ссылкой: 24 + тип + владелец.
pub(crate) const METADATA_ITEM_SIZE: usize = EXTENT_ITEM_SIZE + 1 + 8;

pub(crate) const DEV_EXTENT_SIZE: usize = 48;
pub(crate) const DEV_EXTENT_CHUNK_TREE: usize = 0;
pub(crate) const DEV_EXTENT_CHUNK_OBJECTID: usize = 8;
pub(crate) const DEV_EXTENT_CHUNK_OFFSET: usize = 16;
pub(crate) const DEV_EXTENT_LENGTH: usize = 24;
pub(crate) const DEV_EXTENT_UUID: usize = 32;

pub(crate) const FREE_SPACE_INFO_SIZE: usize = 8;
pub(crate) const FREE_SPACE_INFO_EXTENT_COUNT: usize = 0;

pub(crate) const INODE_GENERATION: usize = 0;
pub(crate) const INODE_NBYTES: usize = 24;
pub(crate) const INODE_ATIME: usize = 112;
pub(crate) const INODE_CTIME: usize = 124;
pub(crate) const INODE_OTIME: usize = 148;

pub(crate) const INODE_REF_HEAD_SIZE: usize = 10;
pub(crate) const INODE_REF_NAME_LEN: usize = 8;

pub(crate) const DIR_ITEM_TYPE: usize = 29;
pub(crate) const FILE_TYPE_DIRECTORY: u8 = 2;

pub(crate) const ROOT_ITEM_SIZE: usize = 439;
pub(crate) const ROOT_ITEM_GENERATION: usize = INODE_ITEM_SIZE;
pub(crate) const ROOT_ITEM_DIRID: usize = INODE_ITEM_SIZE + 8;
pub(crate) const ROOT_ITEM_BYTES_USED: usize = INODE_ITEM_SIZE + 32;
pub(crate) const ROOT_ITEM_REFS: usize = 216;
pub(crate) const ROOT_ITEM_GENERATION_V2: usize = 239;
pub(crate) const ROOT_ITEM_UUID: usize = 247;
pub(crate) const ROOT_ITEM_CTIME: usize = 327;
pub(crate) const ROOT_ITEM_OTIME: usize = 339;

// --- то, что нужно писателю в существующий том ---------------------------------
//
// Сверено с томом, в который писало ядро Linux (`dump-tree` после `mount`,
// записи файла и `umount`).

pub(crate) const BACKUP_SIZE: usize = 168;
pub(crate) const BACKUP_TREE_ROOT_LEVEL: usize = 152;
pub(crate) const BACKUP_CHUNK_ROOT_LEVEL: usize = 153;
pub(crate) const BACKUP_EXTENT_ROOT_LEVEL: usize = 154;
pub(crate) const BACKUP_FS_ROOT_LEVEL: usize = 155;
pub(crate) const BACKUP_DEV_ROOT_LEVEL: usize = 156;
pub(crate) const BACKUP_CSUM_ROOT_LEVEL: usize = 157;

/// Вид группы блоков без профиля размещения.
pub(crate) const BLOCK_GROUP_TYPE_MASK: u64 =
    BLOCK_GROUP_DATA | BLOCK_GROUP_SYSTEM | BLOCK_GROUP_METADATA;

pub(crate) const EXTENT_FLAG_DATA: u64 = 1 << 0;

/// Запись об экстенте данных со встроенной ссылкой: 24 + тип + ссылка (28).
pub(crate) const DATA_EXTENT_ITEM_SIZE: usize = EXTENT_ITEM_SIZE + 1 + 28;
/// Поля ссылки на данные — считаются от байта за её типом.
pub(crate) const DATA_REF_ROOT: usize = 1;
pub(crate) const DATA_REF_OBJECTID: usize = 9;
pub(crate) const DATA_REF_OFFSET: usize = 17;
pub(crate) const DATA_REF_COUNT: usize = 25;

pub(crate) const EXTENT_GENERATION: usize = 0;
pub(crate) const EXTENT_DISK_NUM_BYTES: usize = 29;

pub(crate) const INODE_TRANSID: usize = 8;
pub(crate) const DIR_ITEM_TRANSID: usize = 17;
pub(crate) const FILE_TYPE_REGULAR: u8 = 1;

pub(crate) const ROOT_ITEM_CTRANSID: usize = 295;

// --- запись полей ------------------------------------------------------------

#[inline]
pub(crate) fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u64(buf: &mut [u8], at: usize, value: u64) {
    buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
}

// --- чтение полей ------------------------------------------------------------

#[inline]
pub(crate) fn u16_at(buf: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([buf[at], buf[at + 1]])
}

#[inline]
pub(crate) fn u32_at(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[inline]
pub(crate) fn u64_at(buf: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buf[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// Сверить контрольную сумму блока с той, что записана в его первых байтах.
///
/// Одна функция и на суперблок, и на узел дерева: считается одинаково — со
/// смещения 32 и до конца блока.
pub(crate) fn verify_block(block: &[u8]) -> Result<()> {
    if block.len() <= CSUM_SIZE {
        return Err(Error::Corrupt);
    }
    let stored = u32_at(block, 0);
    if crc32c::checksum(&block[CSUM_SIZE..]) == stored {
        Ok(())
    } else {
        Err(Error::BadChecksum)
    }
}
