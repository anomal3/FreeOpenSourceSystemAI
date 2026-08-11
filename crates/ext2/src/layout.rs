//! Раскладка ext2 на диске: геометрия тома и смещения полей.
//!
//! # Как устроен том
//!
//! ```text
//!  байт 1024        суперблок (ровно 1024 байта)
//!  далее            таблица дескрипторов групп блоков
//!  затем            группы блоков, одна за другой:
//!                     [резервная копия суперблока и таблицы дескрипторов]
//!                     битовая карта блоков   (1 блок)
//!                     битовая карта inode    (1 блок)
//!                     таблица inode          (несколько блоков)
//!                     данные
//! ```
//!
//! Размер группы задаётся не выбором, а арифметикой: битовая карта блоков —
//! это один блок, то есть `размер_блока * 8` бит, и ровно столько блоков в
//! группе может быть.
//!
//! # Резервные копии в каждой группе
//!
//! Возможность `sparse_super` кладёт копии суперблока лишь в группы с номерами
//! 0, 1 и степенями 3, 5, 7. Здесь она **не** включается, и копии пишутся во
//! все группы. Это законный ext2 (так вёл себя формат до появления
//! возможности), он проще, а на наших томах в несколько сотен мегабайт разница
//! в занятом месте измеряется сотнями килобайт. Включать возможность, которую
//! негде проверить, ради экономии, которой никто не заметит, — плохой обмен.

use crate::{Error, Result};

/// Подпись ext2 в суперблоке.
pub(crate) const MAGIC: u16 = 0xEF53;

/// Смещение суперблока от начала тома. Не зависит от размера блока: первый
/// килобайт тома зарезервирован под загрузочный сектор.
pub(crate) const SUPERBLOCK_OFFSET: u64 = 1024;
pub(crate) const SUPERBLOCK_SIZE: usize = 1024;

/// Размер записи в таблице дескрипторов групп.
pub(crate) const GROUP_DESC_SIZE: usize = 32;

/// Размер inode на диске.
///
/// 128 — исходный размер ext2. Больший (256) нужен ext4 под наносекундные
/// времена и расширенные атрибуты, которых здесь нет; лишние 128 байт на
/// каждый inode стоили бы на нашем томе несколько мегабайт впустую.
pub(crate) const INODE_SIZE: usize = 128;

/// Номер inode корневого каталога. Задан спецификацией.
pub const ROOT_INODE: u32 = 2;

/// Первый inode, доступный файлам. Номера 1..=10 зарезервированы.
pub(crate) const FIRST_INODE: u32 = 11;

/// Прямых указателей на блоки в inode.
pub(crate) const DIRECT_BLOCKS: usize = 12;
/// Индекс указателя на блок косвенности первого уровня.
pub(crate) const INDIRECT_INDEX: usize = 12;
/// Индекс указателя на блок косвенности второго уровня.
pub(crate) const DOUBLE_INDIRECT_INDEX: usize = 13;
/// Всего указателей в inode. Пятнадцатый — третий уровень косвенности; он
/// остаётся нулевым, потому что файлов такого размера здесь не бывает
/// (см. отказ `Unsupported` в разборе указателей).
pub(crate) const BLOCK_POINTERS: usize = 15;

/// Возможности, которые мы объявляем на томе.
///
/// `FILETYPE` кладёт тип файла в саму запись каталога, избавляя от чтения
/// inode ради того, чтобы отличить файл от каталога при перечислении. Она
/// понимается всеми реализациями с 1990-х и включена на любом созданном
/// `mke2fs` томе.
pub(crate) const FEATURE_INCOMPAT_FILETYPE: u32 = 0x0002;

/// Тип файла в записи каталога (при включённой `FILETYPE`).
pub(crate) const DIR_TYPE_REGULAR: u8 = 1;
pub(crate) const DIR_TYPE_DIRECTORY: u8 = 2;

/// Тип файла в поле `i_mode`.
pub(crate) const MODE_DIRECTORY: u16 = 0x4000;
pub(crate) const MODE_REGULAR: u16 = 0x8000;

/// Размер блока файловой системы.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockSize {
    B1024,
    B2048,
    B4096,
}

impl BlockSize {
    /// Размер в байтах.
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            BlockSize::B1024 => 1024,
            BlockSize::B2048 => 2048,
            BlockSize::B4096 => 4096,
        }
    }

    /// Значение поля `s_log_block_size`: размер равен `1024 << log`.
    #[must_use]
    pub const fn log(self) -> u32 {
        match self {
            BlockSize::B1024 => 0,
            BlockSize::B2048 => 1,
            BlockSize::B4096 => 2,
        }
    }

    pub(crate) const fn from_log(log: u32) -> Option<Self> {
        match log {
            0 => Some(BlockSize::B1024),
            1 => Some(BlockSize::B2048),
            2 => Some(BlockSize::B4096),
            _ => None,
        }
    }

    /// Подобрать размер блока под том.
    ///
    /// Мелкий блок экономит место на хвостах файлов, крупный — уменьшает и
    /// таблицы косвенности, и число групп. Порог в 64 МиБ примерно там же, где
    /// его ставит `mke2fs`.
    #[must_use]
    pub const fn for_volume(bytes: u64) -> Self {
        if bytes >= 64 * 1024 * 1024 {
            BlockSize::B4096
        } else {
            BlockSize::B1024
        }
    }

    /// Сколько указателей на блоки помещается в один блок косвенности.
    #[must_use]
    pub const fn pointers_per_block(self) -> u32 {
        self.bytes() / 4
    }
}

/// Плотность inode: один inode на столько байт тома.
///
/// 16 КиБ — умолчание `mke2fs` для обычных томов. Значение важнее, чем
/// кажется: изменить его после форматирования невозможно, а том, у которого
/// кончились inode при свободном месте, выглядит для человека как поломка.
const BYTES_PER_INODE: u64 = 16 * 1024;

/// Всё, что вычисляется из размера тома.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    /// Первый сектор тома на носителе.
    pub first_lba: u64,
    pub block_size: BlockSize,
    /// Всего блоков в файловой системе.
    pub blocks: u32,
    /// Номер первого блока данных: 1 при блоке в 1024 байта, иначе 0.
    ///
    /// Причина в том, что суперблок лежит по фиксированному смещению 1024. При
    /// блоке 1024 он занимает блок номер 1 целиком, при большем — помещается
    /// внутрь блока 0.
    pub first_data_block: u32,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub groups: u32,
    /// Блоков под таблицу дескрипторов групп.
    pub group_desc_blocks: u32,
    /// Блоков под таблицу inode в одной группе.
    pub inode_table_blocks: u32,
}

impl Geometry {
    /// Рассчитать геометрию тома длиной `sectors` секторов по 512 байт.
    pub fn plan(first_lba: u64, sectors: u64) -> Result<Self> {
        let bytes = sectors * 512;
        let block_size = BlockSize::for_volume(bytes);
        Self::plan_with(first_lba, sectors, block_size)
    }

    /// То же, но с заданным размером блока — нужно тестам, которые обязаны
    /// проверить обе ветки выбора.
    pub fn plan_with(first_lba: u64, sectors: u64, block_size: BlockSize) -> Result<Self> {
        let bytes = sectors * 512;
        let block_bytes = u64::from(block_size.bytes());

        let blocks = u32::try_from(bytes / block_bytes).map_err(|_| Error::TooSmall)?;
        let first_data_block = if block_size == BlockSize::B1024 { 1 } else { 0 };
        // Группа не может быть длиннее, чем описывает её битовая карта: один
        // блок, то есть размер_блока * 8 бит.
        let blocks_per_group = block_size.bytes() * 8;

        let usable = blocks.checked_sub(first_data_block).ok_or(Error::TooSmall)?;
        let groups = usable.div_ceil(blocks_per_group).max(1);

        // Число inode на группу: от плотности, но не больше, чем описывает
        // битовая карта inode, и кратно восьми — чтобы карта состояла из целых
        // байт и её конец не приходился на середину байта.
        let wanted = (bytes / BYTES_PER_INODE).max(u64::from(FIRST_INODE) + 1);
        let per_group = u32::try_from(wanted.div_ceil(u64::from(groups)))
            .unwrap_or(u32::MAX)
            .min(blocks_per_group)
            .next_multiple_of(8)
            .max(8);

        let inode_table_blocks =
            (per_group * INODE_SIZE as u32).div_ceil(block_size.bytes());
        let group_desc_blocks =
            (groups * GROUP_DESC_SIZE as u32).div_ceil(block_size.bytes());

        let geometry = Self {
            first_lba,
            block_size,
            blocks,
            first_data_block,
            blocks_per_group,
            inodes_per_group: per_group,
            groups,
            group_desc_blocks,
            inode_table_blocks,
        };

        // Проверяем, а не верим на слово: у крошечного тома служебные структуры
        // способны съесть все блоки до единого, и получилась бы файловая
        // система, в которую нельзя записать ни байта.
        let overhead = geometry.group_overhead_blocks(0);
        if geometry.blocks_in_group(0) <= overhead + 2 {
            return Err(Error::TooSmall);
        }

        Ok(geometry)
    }

    /// Восстановить геометрию из прочитанного суперблока.
    pub(crate) fn from_superblock(first_lba: u64, sb: &[u8]) -> Result<Self> {
        let magic = u16_at(sb, 56);
        if magic != MAGIC {
            return Err(Error::Corrupt);
        }
        let block_size = BlockSize::from_log(u32_at(sb, 24)).ok_or(Error::Corrupt)?;
        let blocks = u32_at(sb, 4);
        let first_data_block = u32_at(sb, 20);
        let blocks_per_group = u32_at(sb, 32);
        let inodes_per_group = u32_at(sb, 40);
        let inodes = u32_at(sb, 0);

        if blocks_per_group == 0 || inodes_per_group == 0 || blocks == 0 {
            return Err(Error::Corrupt);
        }
        // Размер inode: у rev0 он всегда 128, у rev1 записан в суперблоке. Всё,
        // что не 128, означает том, созданный не нами (скорее всего ext4), и
        // разбирать его здесь нечем.
        let inode_size = if u32_at(sb, 76) == 0 { 128 } else { u16_at(sb, 88) };
        if inode_size as usize != INODE_SIZE {
            return Err(Error::Unsupported);
        }
        // Возможности, которых мы не понимаем, обязаны приводить к отказу, а не
        // к попытке прочитать том по-своему: у ext4 та же подпись, и разница
        // видна только здесь.
        let incompat = u32_at(sb, 96);
        if incompat & !FEATURE_INCOMPAT_FILETYPE != 0 {
            return Err(Error::Unsupported);
        }

        let groups = blocks
            .saturating_sub(first_data_block)
            .div_ceil(blocks_per_group)
            .max(1);
        if inodes != groups * inodes_per_group {
            return Err(Error::Corrupt);
        }

        Ok(Self {
            first_lba,
            block_size,
            blocks,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            groups,
            group_desc_blocks: (groups * GROUP_DESC_SIZE as u32)
                .div_ceil(block_size.bytes()),
            inode_table_blocks: (inodes_per_group * INODE_SIZE as u32)
                .div_ceil(block_size.bytes()),
        })
    }

    /// Всего inode на томе.
    #[must_use]
    pub const fn inodes(&self) -> u32 {
        self.groups * self.inodes_per_group
    }

    /// Первый блок группы.
    #[must_use]
    pub const fn group_first_block(&self, group: u32) -> u32 {
        self.first_data_block + group * self.blocks_per_group
    }

    /// Сколько блоков в группе. Последняя обычно короче остальных.
    #[must_use]
    pub const fn blocks_in_group(&self, group: u32) -> u32 {
        let start = self.group_first_block(group);
        let end = start + self.blocks_per_group;
        if end > self.blocks { self.blocks - start } else { self.blocks_per_group }
    }

    /// Есть ли в группе резервная копия суперблока.
    ///
    /// Всегда: возможность `sparse_super` не включена (см. заголовок модуля).
    #[must_use]
    pub const fn group_has_super(&self, _group: u32) -> bool {
        true
    }

    /// Сколько блоков в группе занято служебными структурами.
    #[must_use]
    pub const fn group_overhead_blocks(&self, group: u32) -> u32 {
        let backup = if self.group_has_super(group) {
            1 + self.group_desc_blocks
        } else {
            0
        };
        // Битовая карта блоков, битовая карта inode и таблица inode.
        backup + 2 + self.inode_table_blocks
    }

    /// Блок с битовой картой блоков группы.
    #[must_use]
    pub const fn block_bitmap_block(&self, group: u32) -> u32 {
        let backup = if self.group_has_super(group) {
            1 + self.group_desc_blocks
        } else {
            0
        };
        self.group_first_block(group) + backup
    }

    /// Блок с битовой картой inode группы.
    #[must_use]
    pub const fn inode_bitmap_block(&self, group: u32) -> u32 {
        self.block_bitmap_block(group) + 1
    }

    /// Первый блок таблицы inode группы.
    #[must_use]
    pub const fn inode_table_block(&self, group: u32) -> u32 {
        self.inode_bitmap_block(group) + 1
    }

    /// Первый блок данных группы.
    #[must_use]
    pub const fn first_free_block_in_group(&self, group: u32) -> u32 {
        self.inode_table_block(group) + self.inode_table_blocks
    }

    /// Где на носителе лежит блок файловой системы.
    #[must_use]
    pub const fn block_lba(&self, block: u32) -> u64 {
        self.first_lba + block as u64 * (self.block_size.bytes() as u64 / 512)
    }

    /// Сколько секторов занимает один блок.
    #[must_use]
    pub const fn sectors_per_block(&self) -> u32 {
        self.block_size.bytes() / 512
    }

    /// В какой группе и под каким номером лежит inode.
    ///
    /// Нумерация inode начинается с единицы, поэтому вычитание обязательно;
    /// забыть его — классическая ошибка, после которой читается соседний inode.
    pub(crate) fn locate_inode(&self, inode: u32) -> Result<(u32, u32)> {
        if inode == 0 || inode > self.inodes() {
            return Err(Error::Corrupt);
        }
        let index = inode - 1;
        Ok((index / self.inodes_per_group, index % self.inodes_per_group))
    }
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
pub(crate) fn put_u16(buf: &mut [u8], at: usize, value: u16) {
    buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

#[inline]
pub(crate) fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 512 МиБ — примерно тот корневой раздел, который создаёт установщик.
    const HALF_GIB: u64 = 512 * 1024 * 1024 / 512;

    #[test]
    fn geometry_of_a_typical_root_partition() {
        let geometry = Geometry::plan(2048, HALF_GIB).expect("512 МиБ хватает");
        assert_eq!(geometry.block_size, BlockSize::B4096);
        assert_eq!(geometry.first_data_block, 0);
        assert_eq!(geometry.blocks, 131072);
        assert_eq!(geometry.blocks_per_group, 32768);
        assert_eq!(geometry.groups, 4);
        // Плотность: один inode на 16 КиБ, то есть 32768 на 512 МиБ.
        assert_eq!(geometry.inodes(), 32768);
        assert_eq!(geometry.inodes_per_group % 8, 0);
    }

    /// Маленький том обязан получить блок в 1024 байта, иначе групп выйдет
    /// меньше одной, а служебные структуры съедят весь том.
    #[test]
    fn small_volume_uses_small_blocks() {
        let geometry = Geometry::plan(0, 16 * 1024 * 1024 / 512).expect("8 МиБ");
        assert_eq!(geometry.block_size, BlockSize::B1024);
        assert_eq!(geometry.first_data_block, 1);
    }

    #[test]
    fn a_volume_that_cannot_hold_the_metadata_is_refused() {
        assert_eq!(Geometry::plan(0, 8), Err(Error::TooSmall));
    }

    /// Границы групп обязаны сходиться: последний блок последней группы — это
    /// последний блок тома, ни блоком больше и ни блоком меньше.
    #[test]
    fn groups_tile_the_volume_exactly() {
        for sectors in [HALF_GIB, 100 * 2048, 40 * 2048, 3 * 2048 * 1024] {
            let geometry = Geometry::plan(0, sectors).expect("том");
            let mut covered = geometry.first_data_block;
            for group in 0..geometry.groups {
                assert_eq!(geometry.group_first_block(group), covered);
                covered += geometry.blocks_in_group(group);
            }
            assert_eq!(covered, geometry.blocks, "секторов {sectors}");
        }
    }

    /// Нумерация inode начинается с единицы — самое подходящее место, чтобы
    /// ошибиться на один и читать чужой inode.
    #[test]
    fn inode_numbering_starts_at_one() {
        let geometry = Geometry::plan(0, HALF_GIB).expect("том");
        assert_eq!(geometry.locate_inode(1), Ok((0, 0)));
        assert_eq!(geometry.locate_inode(ROOT_INODE), Ok((0, 1)));
        let per_group = geometry.inodes_per_group;
        assert_eq!(geometry.locate_inode(per_group), Ok((0, per_group - 1)));
        assert_eq!(geometry.locate_inode(per_group + 1), Ok((1, 0)));
        assert_eq!(geometry.locate_inode(0), Err(Error::Corrupt));
    }
}
