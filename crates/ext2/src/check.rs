//! Проверка и починка тома — то, что в Unix называется `fsck`.
//!
//! # Зачем это здесь, а не «принесите Linux»
//!
//! У ext2 нет журнала, и это сказано прямо в заголовке крейта: после пропажи
//! питания счётчики свободного могут разъехаться с битовыми картами, а только
//! что выделенный блок — оказаться занятым по карте и не принадлежать никакому
//! файлу. Пока починить это могла только чужая машина с `e2fsck`, система
//! оставалась той, которую «несут в сервис». Здесь она чинит себя сама.
//!
//! # Порядок проверок — не произвольный
//!
//! Тот же, в котором идёт `e2fsck`, и по той же причине: каждый следующий шаг
//! опирается на результат предыдущего.
//!
//! 1. **Inode.** Что вообще занято: у каждого занятого inode собираются его
//!    блоки. Это даёт «карту блоков, как она должна выглядеть».
//! 2. **Каталоги.** Каждая запись проверяется на то, что указывает на живой
//!    inode, и считается как ссылка. Считаются **все** записи, включая «.» и
//!    «..», — тогда сумма в точности равна тому, что обязано лежать в
//!    `i_links_count`.
//! 3. **Достижимость.** Обход дерева от корня: занятый inode, до которого не
//!    ведёт ни одна запись, — потерянный файл.
//! 4. **Ссылки.** Посчитанное сверяется с записанным в inode.
//! 5. **Карты и счётчики.** И только теперь — битовые карты и числа свободного,
//!    потому что до этого шага неизвестно, что считать занятым.
//!
//! Переставить шаги нельзя: починив карты первыми, пришлось бы считать их
//! заново после каждого следующего исправления.
//!
//! # Что чинится само, а что только называется
//!
//! Само — то, что чинится **однозначно**: битовые карты, счётчики свободного,
//! число ссылок и остаток от прерванного создания файла (занятый inode, на
//! который никто не ссылается и у которого ноль ссылок, — до него нет пути из
//! дерева, и его блоки не принадлежат никому).
//!
//! Не само — всё, где нужно решение человека: запись каталога, указывающая на
//! свободный inode (удалить её значит потерять имя), блок, поделённый двумя
//! файлами (кому его отдать — неизвестно), указатель за пределы тома. Такое
//! называется и остаётся как есть. «Умный» ремонт данных без спроса — это и
//! есть способ потерять их окончательно.
//!
//! Потерянный файл — случай посередине: его не удаляют и не оставляют
//! невидимым, а переносят в `/lost+found` под именем из номера inode. Данные
//! при этом целы, а имя всё равно утеряно вместе с записью каталога.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use disk::BlockDevice;

use crate::edit::Editor;
use crate::layout::*;
use crate::write::{InodeData, test_bit, set_bit, try_vec, try_zeroed};
use crate::{Error, Result};

/// Что делать с найденным.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fix {
    /// Ничего: только посмотреть и рассказать.
    Nothing,
    /// Исправить то, что исправляется однозначно.
    Safe,
}

/// Сколько находок запоминается.
///
/// Предел нужен не ради экономии, а ради того, чтобы проверка вдребезги
/// разбитого тома не съела всю память под список жалоб. Отброшенные считаются —
/// «и ещё столько же» честнее, чем оборванный список.
const MAX_PROBLEMS: usize = 64;

/// Имя каталога, куда переезжают потерянные файлы. Задано традицией Unix и
/// понимается всеми чужими инструментами.
const LOST_FOUND: &str = "lost+found";

/// Одна находка.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Problem {
    /// Битовая карта блоков группы разошлась с тем, что занято на самом деле.
    BlockBitmap { group: u32, leaked: u32, missing: u32 },
    /// То же для карты inode.
    InodeBitmap { group: u32, leaked: u32, missing: u32 },
    /// Число свободных блоков в суперблоке.
    FreeBlocks { was: u32, correct: u32 },
    /// Число свободных inode в суперблоке.
    FreeInodes { was: u32, correct: u32 },
    /// Счётчики группы: свободные блоки, свободные inode, каталоги.
    GroupCounts { group: u32 },
    /// Число ссылок на inode не совпадает с числом записей, на него указывающих.
    Links { inode: u32, was: u16, correct: u16 },
    /// Запись каталога указывает на inode, которого нет или который свободен.
    Dangling { dir: u32, inode: u32, name: String },
    /// Занятый inode, до которого не ведёт ни одна запись каталога.
    Lost { inode: u32, links: u16 },
    /// Остаток прерванного создания файла: занят, ссылок ноль, записей нет.
    Abandoned { inode: u32 },
    /// Указатель на блок за пределами тома.
    BadPointer { inode: u32, block: u32 },
    /// Один блок принадлежит двум inode сразу.
    Shared { inode: u32, block: u32 },
    /// У каталога нет записей «.» и «..».
    NoDots { inode: u32 },
    /// Inode пользуется тем, чего этот крейт не умеет (тройная косвенность).
    Unsupported { inode: u32 },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::BlockBitmap { group, leaked, missing } => write!(
                f,
                "group {group}: block bitmap wrong, {leaked} block(s) marked used by nobody, {missing} in use but marked free"
            ),
            Problem::InodeBitmap { group, leaked, missing } => write!(
                f,
                "group {group}: inode bitmap wrong, {leaked} marked used by nobody, {missing} in use but marked free"
            ),
            Problem::FreeBlocks { was, correct } => {
                write!(f, "free block count is {was}, should be {correct}")
            }
            Problem::FreeInodes { was, correct } => {
                write!(f, "free inode count is {was}, should be {correct}")
            }
            Problem::GroupCounts { group } => write!(f, "group {group}: counters do not match"),
            Problem::Links { inode, was, correct } => write!(
                f,
                "inode {inode}: link count is {was}, {correct} directory entr(ies) point at it"
            ),
            Problem::Dangling { dir, inode, name } => write!(
                f,
                "directory {dir}: entry '{name}' points at inode {inode}, which is not in use"
            ),
            Problem::Lost { inode, links } => {
                write!(f, "inode {inode}: {links} link(s) but no directory entry, file is lost")
            }
            Problem::Abandoned { inode } => {
                write!(f, "inode {inode}: in use, no links, no entries — leftover of an interrupted create")
            }
            Problem::BadPointer { inode, block } => {
                write!(f, "inode {inode}: block pointer {block} is outside the volume")
            }
            Problem::Shared { inode, block } => {
                write!(f, "inode {inode}: block {block} already belongs to another inode")
            }
            Problem::NoDots { inode } => write!(f, "directory {inode}: '.' or '..' is missing"),
            Problem::Unsupported { inode } => {
                write!(f, "inode {inode}: uses triple indirection, which this driver cannot follow")
            }
        }
    }
}

impl Problem {
    /// Чинится ли это однозначно — то есть без решения человека.
    #[must_use]
    pub const fn is_safe_to_fix(&self) -> bool {
        matches!(
            self,
            Problem::BlockBitmap { .. }
                | Problem::InodeBitmap { .. }
                | Problem::FreeBlocks { .. }
                | Problem::FreeInodes { .. }
                | Problem::GroupCounts { .. }
                | Problem::Links { .. }
                | Problem::Abandoned { .. }
                | Problem::Lost { .. }
        )
    }
}

/// Что нашла проверка.
#[derive(Debug)]
pub struct Report {
    /// Находки в порядке обнаружения, не больше [`MAX_PROBLEMS`].
    pub problems: Vec<Problem>,
    /// Сколько находок не поместилось в список.
    pub dropped: usize,
    /// Сколько находок исправлено.
    pub fixed: usize,
    /// Сколько потерянных файлов переехало в `/lost+found`.
    pub rescued: usize,
    /// Занято inode и блоков — по итогам обхода, а не по счётчикам тома.
    pub inodes_used: u32,
    pub blocks_used: u32,
}

impl Report {
    /// Всё ли в порядке.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.problems.is_empty() && self.dropped == 0
    }

    /// Осталось ли то, что человеку придётся решать самому.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.dropped > 0 || self.problems.iter().any(|problem| !problem.is_safe_to_fix())
    }
}

/// Проверить том и, если попросили, починить то, что чинится однозначно.
///
/// # Ошибки
///
/// [`Error::Corrupt`] — том не разбирается вовсе (не тот суперблок, геометрия
/// не сходится сама с собой): чинить в этом случае нечего, а притворяться, что
/// починили, — худшее из возможного.
pub fn check(dev: &mut dyn BlockDevice, first_lba: u64, fix: Fix) -> Result<Report> {
    let mut checker = Checker::new(dev, first_lba)?;
    checker.scan_inodes(dev)?;
    checker.scan_directories(dev)?;
    checker.walk_tree()?;
    checker.compare_links()?;
    checker.compare_maps(dev)?;
    if fix == Fix::Safe {
        checker.repair(dev)?;
    }
    Ok(checker.report)
}

/// Состояние проверки. Живёт ровно один вызов [`check`].
struct Checker {
    geometry: Geometry,
    /// Каким том был по данным суперблока до проверки.
    free_blocks: u32,
    free_inodes: u32,
    /// Счётчики групп, прочитанные с диска.
    disk_group_blocks: Vec<u32>,
    disk_group_inodes: Vec<u32>,
    disk_group_dirs: Vec<u16>,
    /// Битовые карты с диска и посчитанные обходом — в одной раскладке:
    /// по блоку на группу, бит `i` группы `g` — блок `group_first_block(g) + i`.
    disk_blocks: Vec<u8>,
    disk_inodes: Vec<u8>,
    seen_blocks: Vec<u8>,
    seen_inodes: Vec<u8>,
    /// Сколько записей каталогов указывает на каждый inode (индекс — номер).
    links: Vec<u16>,
    /// Что записано в самом inode.
    disk_links: Vec<u16>,
    /// Каталог ли это.
    is_dir: Vec<bool>,
    /// Куда ведёт первая найденная запись каталога — для обхода дерева.
    /// Хранится списком пар (каталог, потомок), потому что дерево у нас
    /// маленькое, а таблица «родитель → дети» стоила бы отдельной аллокации на
    /// каждый каталог.
    edges: Vec<(u32, u32)>,
    /// Достижимые от корня.
    reachable: Vec<bool>,
    report: Report,
}

impl Checker {
    fn new(dev: &mut dyn BlockDevice, first_lba: u64) -> Result<Self> {
        let sector = dev.sector_size() as u64;
        if sector == 0 {
            return Err(Error::Unsupported);
        }
        // Суперблок читается здесь, а не через `Ext2::mount`: монтирование
        // проверяет корневой inode и отказывает на томе, который мы как раз
        // собрались чинить.
        let lba = first_lba + SUPERBLOCK_OFFSET / sector;
        let within = (SUPERBLOCK_OFFSET % sector) as usize;
        let span = (within + SUPERBLOCK_SIZE).div_ceil(sector as usize);
        let mut raw = try_zeroed(span * sector as usize)?;
        dev.read(lba, &mut raw)?;
        let sb = &raw[within..within + SUPERBLOCK_SIZE];

        let geometry = Geometry::from_superblock(first_lba, sb, dev.sector_size())?;
        let groups = geometry.groups as usize;
        let block_bytes = geometry.block_size.bytes() as usize;
        let inodes = geometry.inodes() as usize;

        let mut checker = Self {
            geometry,
            free_blocks: u32_at(sb, 12),
            free_inodes: u32_at(sb, 16),
            disk_group_blocks: try_vec(groups, 0u32)?,
            disk_group_inodes: try_vec(groups, 0u32)?,
            disk_group_dirs: try_vec(groups, 0u16)?,
            disk_blocks: try_zeroed(groups * block_bytes)?,
            disk_inodes: try_zeroed(groups * block_bytes)?,
            seen_blocks: try_zeroed(groups * block_bytes)?,
            seen_inodes: try_zeroed(groups * block_bytes)?,
            // Индекс — номер inode, поэтому нулевой элемент лишний и не
            // используется: inode нумеруются с единицы.
            links: try_vec(inodes + 1, 0u16)?,
            disk_links: try_vec(inodes + 1, 0u16)?,
            is_dir: try_vec(inodes + 1, false)?,
            edges: Vec::new(),
            reachable: try_vec(inodes + 1, false)?,
            report: Report {
                problems: Vec::new(),
                dropped: 0,
                fixed: 0,
                rescued: 0,
                inodes_used: 0,
                blocks_used: 0,
            },
        };
        checker.load_maps(dev)?;
        checker.mark_metadata();
        Ok(checker)
    }

    /// Прочитать с диска дескрипторы групп и обе битовые карты.
    fn load_maps(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let table_start = geometry.group_first_block(0) + 1;
        let bytes = geometry.groups as usize * GROUP_DESC_SIZE;

        let mut table = try_zeroed(bytes.next_multiple_of(block_bytes))?;
        for index in 0..(table.len() / block_bytes) {
            let buf = read_block(dev, &geometry, table_start + index as u32)?;
            table[index * block_bytes..(index + 1) * block_bytes].copy_from_slice(&buf);
        }

        for group in 0..geometry.groups {
            let at = group as usize * GROUP_DESC_SIZE;
            self.disk_group_blocks[group as usize] = u32::from(u16_at(&table, at + 12));
            self.disk_group_inodes[group as usize] = u32::from(u16_at(&table, at + 14));
            self.disk_group_dirs[group as usize] = u16_at(&table, at + 16);

            // Битовые карты берутся с тех мест, которые вычисляет геометрия, а
            // не с тех, что записаны в дескрипторе. Расхождение между ними —
            // само по себе повреждение, но чинить его нечем: сдвинуть таблицу
            // inode на живом томе нельзя, а поверить дескриптору значит читать
            // карту неизвестно откуда.
            let map = read_block(dev, &geometry, geometry.block_bitmap_block(group))?;
            let at = group as usize * block_bytes;
            self.disk_blocks[at..at + block_bytes].copy_from_slice(&map);
            let map = read_block(dev, &geometry, geometry.inode_bitmap_block(group))?;
            self.disk_inodes[at..at + block_bytes].copy_from_slice(&map);
        }
        Ok(())
    }

    /// Отметить занятым всё служебное: суперблок с копиями, дескрипторы, обе
    /// карты и таблицу inode каждой группы.
    ///
    /// Ровно то же делает разметка ([`crate::write`]); расхождение здесь дало бы
    /// «починку», после которой файл лёг бы поверх таблицы inode.
    fn mark_metadata(&mut self) {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        for group in 0..geometry.groups {
            for index in 0..geometry.group_overhead_blocks(group) {
                set_bit(&mut self.seen_blocks, block_bytes, group, index);
            }
            // Биты за концом группы стоят единицами: последняя группа короче
            // карты, и ноль там означал бы свободный блок за краем тома.
            for index in geometry.blocks_in_group(group)..geometry.blocks_per_group {
                set_bit(&mut self.seen_blocks, block_bytes, group, index);
            }
            for index in geometry.inodes_per_group..geometry.blocks_per_group {
                set_bit(&mut self.seen_inodes, block_bytes, group, index);
            }
        }
        // Зарезервированные номера 1..=10 заняты всегда, даже если ими никто не
        // пользуется: так требует спецификация, и так их видит любой чужой
        // инструмент.
        for inode in 1..FIRST_INODE {
            if let Ok((group, index)) = geometry.locate_inode(inode) {
                set_bit(&mut self.seen_inodes, block_bytes, group, index);
            }
        }
    }

    // --- шаг 1: inode -------------------------------------------------------

    /// Обойти все занятые inode и собрать их блоки.
    fn scan_inodes(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        for number in 1..=geometry.inodes() {
            let (group, index) = geometry.locate_inode(number)?;
            if !test_bit(&self.disk_inodes, block_bytes, group, index) {
                continue;
            }
            self.report.inodes_used += 1;
            let inode = read_inode(dev, &geometry, number)?;
            self.disk_links[number as usize] = inode.links;
            self.is_dir[number as usize] = inode.mode & 0xF000 == MODE_DIRECTORY;

            // Зарезервированные inode не имеют содержимого и разбору не
            // подлежат: у них нулевой режим и мусор в указателях у любого
            // `mke2fs`.
            if number >= FIRST_INODE || number == ROOT_INODE {
                self.collect_blocks(dev, number, &inode)?;
            }
            set_bit(&mut self.seen_inodes, block_bytes, group, index);
        }
        Ok(())
    }

    /// Отметить занятыми все блоки одного inode, включая блоки косвенности.
    fn collect_blocks(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u32,
        inode: &InodeData,
    ) -> Result<()> {
        if inode.blocks[BLOCK_POINTERS - 1] != 0 {
            self.note(Problem::Unsupported { inode: number })?;
            return Ok(());
        }
        for index in 0..DIRECT_BLOCKS {
            self.claim(number, inode.blocks[index])?;
        }

        let indirect = inode.blocks[INDIRECT_INDEX];
        if indirect != 0 {
            self.claim(number, indirect)?;
            for block in self.pointers(dev, indirect)? {
                self.claim(number, block)?;
            }
        }

        let double = inode.blocks[DOUBLE_INDIRECT_INDEX];
        if double != 0 {
            self.claim(number, double)?;
            for table in self.pointers(dev, double)? {
                if table == 0 {
                    continue;
                }
                self.claim(number, table)?;
                for block in self.pointers(dev, table)? {
                    self.claim(number, block)?;
                }
            }
        }
        Ok(())
    }

    /// Прочитать блок указателей, отбросив нули.
    fn pointers(&mut self, dev: &mut dyn BlockDevice, block: u32) -> Result<Vec<u32>> {
        if !self.in_range(block) {
            return Ok(Vec::new());
        }
        let buf = read_block(dev, &self.geometry, block)?;
        let mut out = Vec::new();
        out.try_reserve_exact(buf.len() / 4).map_err(|_| Error::NoMemory)?;
        for word in buf.chunks_exact(4) {
            let value = u32::from_le_bytes([word[0], word[1], word[2], word[3]]);
            if value != 0 {
                out.push(value);
            }
        }
        Ok(out)
    }

    /// Записать блок за inode.
    fn claim(&mut self, number: u32, block: u32) -> Result<()> {
        if block == 0 {
            return Ok(());
        }
        if !self.in_range(block) {
            return self.note(Problem::BadPointer { inode: number, block });
        }
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let (group, index) = self.locate_block(block);
        if test_bit(&self.seen_blocks, block_bytes, group, index) {
            // Служебные блоки уже помечены: попадание в них означает, что
            // файл лёг поверх таблицы inode, и это тот же дефект, что общий
            // блок у двух файлов.
            return self.note(Problem::Shared { inode: number, block });
        }
        set_bit(&mut self.seen_blocks, block_bytes, group, index);
        self.report.blocks_used += 1;
        Ok(())
    }

    // --- шаг 2: каталоги ----------------------------------------------------

    /// Разобрать все каталоги и посчитать ссылки.
    fn scan_directories(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        for number in 1..=geometry.inodes() {
            if !self.is_dir[number as usize] {
                continue;
            }
            let (group, index) = geometry.locate_inode(number)?;
            if !test_bit(&self.disk_inodes, block_bytes, group, index) {
                continue;
            }
            let inode = read_inode(dev, &geometry, number)?;
            self.scan_directory(dev, number, &inode)?;
        }
        Ok(())
    }

    fn scan_directory(
        &mut self,
        dev: &mut dyn BlockDevice,
        number: u32,
        inode: &InodeData,
    ) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let blocks = (inode.size as usize).div_ceil(block_bytes);
        let mut dots = 0;

        for index in 0..blocks {
            let block = match self.directory_block(dev, inode, index)? {
                Some(block) => block,
                None => continue,
            };
            let buf = read_block(dev, &geometry, block)?;
            let mut at = 0usize;
            while at + 8 <= buf.len() {
                let target = u32_at(&buf, at);
                let rec_len = u16_at(&buf, at + 4) as usize;
                // Длина записи задаёт шаг по блоку: ноль или выход за край —
                // это не «пустая запись», а испорченный каталог, и дальше по
                // нему идти нельзя, иначе разбор зациклится.
                if rec_len < 8 || at + rec_len > buf.len() {
                    self.note(Problem::NoDots { inode: number })?;
                    break;
                }
                let name_len = buf[at + 6] as usize;
                if target != 0 && at + 8 + name_len <= buf.len() {
                    let name = String::from_utf8_lossy(&buf[at + 8..at + 8 + name_len]);
                    if name == "." || name == ".." {
                        dots += 1;
                    }
                    self.count_entry(number, target, &name)?;
                }
                at += rec_len;
            }
        }

        if dots < 2 {
            self.note(Problem::NoDots { inode: number })?;
        }
        Ok(())
    }

    /// Учесть одну запись каталога.
    fn count_entry(&mut self, dir: u32, target: u32, name: &str) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let Ok((group, index)) = geometry.locate_inode(target) else {
            return self.note(Problem::Dangling {
                dir,
                inode: target,
                name: own(name)?,
            });
        };
        if !test_bit(&self.disk_inodes, block_bytes, group, index) {
            return self.note(Problem::Dangling {
                dir,
                inode: target,
                name: own(name)?,
            });
        }

        self.links[target as usize] = self.links[target as usize].saturating_add(1);
        // «.» ведёт в сам каталог, «..» — вверх; ни то ни другое не делает
        // потомка достижимым, иначе достижимым оказалось бы всё сразу.
        if name != "." && name != ".." {
            self.edges.try_reserve(1).map_err(|_| Error::NoMemory)?;
            self.edges.push((dir, target));
        }
        Ok(())
    }

    /// Номер `index`-го блока каталога. `None` — дырка в файле.
    fn directory_block(
        &mut self,
        dev: &mut dyn BlockDevice,
        inode: &InodeData,
        index: usize,
    ) -> Result<Option<u32>> {
        let block = if index < DIRECT_BLOCKS {
            inode.blocks[index]
        } else {
            let pointers = self.geometry.block_size.pointers_per_block() as usize;
            let slot = index - DIRECT_BLOCKS;
            if slot >= pointers {
                return Ok(None);
            }
            let table = inode.blocks[INDIRECT_INDEX];
            if !self.in_range(table) {
                return Ok(None);
            }
            let buf = read_block(dev, &self.geometry, table)?;
            u32_at(&buf, slot * 4)
        };
        if block == 0 || !self.in_range(block) {
            return Ok(None);
        }
        Ok(Some(block))
    }

    // --- шаг 3: достижимость ------------------------------------------------

    /// Обойти дерево от корня.
    ///
    /// Обход по списку рёбер, а не рекурсией: глубина каталогов на диске ничем
    /// не ограничена, а стек ядра — ограничен, и переполнить его разбором
    /// испорченного тома было бы отличным способом превратить проверку в отказ.
    fn walk_tree(&mut self) -> Result<()> {
        let mut queue: Vec<u32> = Vec::new();
        queue.try_reserve(1).map_err(|_| Error::NoMemory)?;
        queue.push(ROOT_INODE);
        self.reachable[ROOT_INODE as usize] = true;

        while let Some(dir) = queue.pop() {
            // Копия списка потомков не делается: рёбра перебираются целиком.
            // Каталогов на наших томах десятки, и квадратичность здесь дешевле
            // отдельной таблицы на каждый каталог.
            for index in 0..self.edges.len() {
                let (parent, child) = self.edges[index];
                if parent != dir || self.reachable[child as usize] {
                    continue;
                }
                self.reachable[child as usize] = true;
                if self.is_dir[child as usize] {
                    queue.try_reserve(1).map_err(|_| Error::NoMemory)?;
                    queue.push(child);
                }
            }
        }
        Ok(())
    }

    // --- шаг 4: ссылки ------------------------------------------------------

    fn compare_links(&mut self) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        for number in FIRST_INODE..=geometry.inodes() {
            let (group, index) = geometry.locate_inode(number)?;
            let on_disk = test_bit(&self.disk_inodes, block_bytes, group, index);
            let counted = self.links[number as usize];
            let recorded = self.disk_links[number as usize];

            if !on_disk {
                continue;
            }
            if counted == 0 {
                if recorded == 0 {
                    // Занят, ссылок нет, записей нет: остаток прерванного
                    // создания. Ни один путь до него не ведёт — освобождение
                    // однозначно.
                    self.note(Problem::Abandoned { inode: number })?;
                } else {
                    // Данные есть, а имени нет: потерянный файл.
                    self.note(Problem::Lost { inode: number, links: recorded })?;
                }
                continue;
            }
            if !self.reachable[number as usize] {
                // Ссылки есть, но все они из каталогов, до которых самих нет
                // пути. Считается потерянным по тому же правилу.
                self.note(Problem::Lost { inode: number, links: recorded })?;
                continue;
            }
            if counted != recorded {
                self.note(Problem::Links { inode: number, was: recorded, correct: counted })?;
            }
        }
        Ok(())
    }

    // --- шаг 5: карты и счётчики -------------------------------------------

    fn compare_maps(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let _ = dev;
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        let mut free_blocks = 0u32;
        let mut free_inodes = 0u32;

        for group in 0..geometry.groups {
            // Имена у двух половин разные намеренно: одинаковые перекрыли бы
            // друг друга, и сравнение счётчиков блоков получило бы число
            // свободных inode. Так и случилось при первом прогоне.
            let (mut leaked, mut missing, mut group_blocks) = (0u32, 0u32, 0u32);
            for index in 0..geometry.blocks_per_group {
                let seen = test_bit(&self.seen_blocks, block_bytes, group, index);
                let disk = test_bit(&self.disk_blocks, block_bytes, group, index);
                if !seen && index < geometry.blocks_in_group(group) {
                    group_blocks += 1;
                }
                match (seen, disk) {
                    (false, true) => leaked += 1,
                    (true, false) => missing += 1,
                    _ => {}
                }
            }
            if leaked != 0 || missing != 0 {
                self.note(Problem::BlockBitmap { group, leaked, missing })?;
            }
            free_blocks += group_blocks;

            let (mut leaked, mut missing, mut group_inodes) = (0u32, 0u32, 0u32);
            for index in 0..geometry.inodes_per_group {
                let seen = test_bit(&self.seen_inodes, block_bytes, group, index);
                let disk = test_bit(&self.disk_inodes, block_bytes, group, index);
                if !seen {
                    group_inodes += 1;
                }
                match (seen, disk) {
                    (false, true) => leaked += 1,
                    (true, false) => missing += 1,
                    _ => {}
                }
            }
            if leaked != 0 || missing != 0 {
                self.note(Problem::InodeBitmap { group, leaked, missing })?;
            }
            free_inodes += group_inodes;

            if self.disk_group_blocks[group as usize] != group_blocks
                || self.disk_group_inodes[group as usize] != self.group_free_inodes(group)
                || self.disk_group_dirs[group as usize] != self.group_dirs(group)
            {
                self.note(Problem::GroupCounts { group })?;
            }
        }

        if self.free_blocks != free_blocks {
            self.note(Problem::FreeBlocks { was: self.free_blocks, correct: free_blocks })?;
        }
        if self.free_inodes != free_inodes {
            self.note(Problem::FreeInodes { was: self.free_inodes, correct: free_inodes })?;
        }
        Ok(())
    }

    /// Сколько inode свободно в группе по итогам обхода.
    fn group_free_inodes(&self, group: u32) -> u32 {
        let block_bytes = self.geometry.block_size.bytes() as usize;
        (0..self.geometry.inodes_per_group)
            .filter(|index| !test_bit(&self.seen_inodes, block_bytes, group, *index))
            .count() as u32
    }

    /// Сколько каталогов в группе по итогам обхода.
    fn group_dirs(&self, group: u32) -> u16 {
        let first = group * self.geometry.inodes_per_group + 1;
        let last = (first + self.geometry.inodes_per_group - 1).min(self.geometry.inodes());
        (first..=last)
            .filter(|number| self.is_dir[*number as usize] && self.links[*number as usize] > 0)
            .count() as u16
    }

    // --- починка ------------------------------------------------------------

    /// Записать на диск то, что чинится однозначно.
    fn repair(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        if self.report.problems.is_empty() {
            return Ok(());
        }

        // Порядок обязателен. Сначала брошенные inode: их освобождение меняет
        // карту, которую следующим шагом пишем на диск. Потом ссылки — они
        // лежат в самих inode. И только затем карты и счётчики, потому что они
        // подводят итог всему предыдущему.
        self.free_abandoned(dev)?;
        self.fix_links(dev)?;
        self.write_maps(dev)?;
        self.write_counts(dev)?;
        self.rescue_lost(dev)?;
        dev.flush()?;
        Ok(())
    }

    /// Освободить остатки прерванных созданий.
    fn free_abandoned(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let abandoned: Vec<u32> = self
            .report
            .problems
            .iter()
            .filter_map(|problem| match problem {
                Problem::Abandoned { inode } => Some(*inode),
                _ => None,
            })
            .collect();

        for number in abandoned {
            // Блоки такого inode отдаются тому только вместе с ним: они уже
            // отмечены занятыми обходом, и без этого шага утекли бы.
            let inode = read_inode(dev, &geometry, number)?;
            self.release_blocks(dev, &inode)?;
            let (group, index) = geometry.locate_inode(number)?;
            let at = group as usize * block_bytes + (index / 8) as usize;
            self.seen_inodes[at] &= !(1 << (index % 8));
            self.is_dir[number as usize] = false;

            // Inode обнуляется, но с ненулевым временем удаления: `e2fsck`
            // считает нулевое время у свободного inode отдельным дефектом.
            let mut cleared = InodeData::new(0, 0, 0, 0);
            cleared.deleted = 1;
            write_inode(dev, &geometry, number, &cleared)?;
            self.report.fixed += 1;
        }
        Ok(())
    }

    /// Снять пометку с блоков inode, который освобождается.
    fn release_blocks(&mut self, dev: &mut dyn BlockDevice, inode: &InodeData) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let mut blocks: Vec<u32> = Vec::new();
        for index in 0..DIRECT_BLOCKS {
            push(&mut blocks, inode.blocks[index])?;
        }
        let indirect = inode.blocks[INDIRECT_INDEX];
        if indirect != 0 {
            push(&mut blocks, indirect)?;
            for block in self.pointers(dev, indirect)? {
                push(&mut blocks, block)?;
            }
        }
        let double = inode.blocks[DOUBLE_INDIRECT_INDEX];
        if double != 0 {
            push(&mut blocks, double)?;
            for table in self.pointers(dev, double)? {
                push(&mut blocks, table)?;
                for block in self.pointers(dev, table)? {
                    push(&mut blocks, block)?;
                }
            }
        }

        for block in blocks {
            if !self.in_range(block) {
                continue;
            }
            let (group, index) = self.locate_block(block);
            let at = group as usize * block_bytes + (index / 8) as usize;
            self.seen_blocks[at] &= !(1 << (index % 8));
            self.report.blocks_used = self.report.blocks_used.saturating_sub(1);
        }
        Ok(())
    }

    /// Записать в inode посчитанное число ссылок.
    fn fix_links(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let repairs: Vec<(u32, u16)> = self
            .report
            .problems
            .iter()
            .filter_map(|problem| match problem {
                Problem::Links { inode, correct, .. } => Some((*inode, *correct)),
                _ => None,
            })
            .collect();

        for (number, links) in repairs {
            let mut inode = read_inode(dev, &geometry, number)?;
            inode.links = links;
            write_inode(dev, &geometry, number, &inode)?;
            self.report.fixed += 1;
        }
        Ok(())
    }

    /// Записать посчитанные битовые карты.
    fn write_maps(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;
        let mut wrote = false;

        for group in 0..geometry.groups {
            let at = group as usize * block_bytes;
            if self.seen_blocks[at..at + block_bytes] != self.disk_blocks[at..at + block_bytes] {
                let map = self.seen_blocks[at..at + block_bytes].to_vec();
                write_block(dev, &geometry, geometry.block_bitmap_block(group), &map)?;
                wrote = true;
            }
            if self.seen_inodes[at..at + block_bytes] != self.disk_inodes[at..at + block_bytes] {
                let map = self.seen_inodes[at..at + block_bytes].to_vec();
                write_block(dev, &geometry, geometry.inode_bitmap_block(group), &map)?;
                wrote = true;
            }
        }
        if wrote {
            self.report.fixed += 1;
        }
        Ok(())
    }

    /// Пересчитать и записать счётчики свободного — в суперблок, его копии и
    /// дескрипторы групп.
    fn write_counts(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let geometry = self.geometry;
        let block_bytes = geometry.block_size.bytes() as usize;

        let mut descriptors = try_zeroed(geometry.groups as usize * GROUP_DESC_SIZE)?;
        let mut free_blocks = 0u32;
        let mut free_inodes = 0u32;
        for group in 0..geometry.groups {
            let free_b = (0..geometry.blocks_in_group(group))
                .filter(|index| !test_bit(&self.seen_blocks, block_bytes, group, *index))
                .count() as u32;
            let free_i = self.group_free_inodes(group);
            free_blocks += free_b;
            free_inodes += free_i;

            let at = group as usize * GROUP_DESC_SIZE;
            put_u32(&mut descriptors, at, geometry.block_bitmap_block(group));
            put_u32(&mut descriptors, at + 4, geometry.inode_bitmap_block(group));
            put_u32(&mut descriptors, at + 8, geometry.inode_table_block(group));
            put_u16(&mut descriptors, at + 12, free_b as u16);
            put_u16(&mut descriptors, at + 14, free_i as u16);
            put_u16(&mut descriptors, at + 16, self.group_dirs(group));
        }

        for group in 0..geometry.groups {
            if !geometry.group_has_super(group) {
                continue;
            }
            let block = geometry.group_first_block(group);
            let within = if group == 0 && geometry.block_size != BlockSize::B1024 {
                SUPERBLOCK_OFFSET as usize
            } else {
                0
            };
            let mut buf = read_block(dev, &geometry, block)?;
            put_u32(&mut buf, within + 12, free_blocks);
            put_u32(&mut buf, within + 16, free_inodes);
            write_block(dev, &geometry, block, &buf)?;

            let table_start = block + 1;
            for (index, chunk) in descriptors.chunks(block_bytes).enumerate() {
                let mut buf = try_zeroed(block_bytes)?;
                buf[..chunk.len()].copy_from_slice(chunk);
                write_block(dev, &geometry, table_start + index as u32, &buf)?;
            }
        }
        self.report.fixed += 1;
        Ok(())
    }

    /// Перенести потерянные файлы в `/lost+found`.
    ///
    /// Делается **последним** и через обычный редактор: к этому моменту том уже
    /// согласован, и создание каталога идёт по тому же пути, что и всякая
    /// другая запись, — а не по особому пути «для починки», который никто
    /// больше не проверяет.
    fn rescue_lost(&mut self, dev: &mut dyn BlockDevice) -> Result<()> {
        let lost: Vec<(u32, bool)> = self
            .report
            .problems
            .iter()
            .filter_map(|problem| match problem {
                Problem::Lost { inode, .. } => Some((*inode, self.is_dir[*inode as usize])),
                _ => None,
            })
            .collect();
        if lost.is_empty() {
            return Ok(());
        }

        let mut editor = Editor::open(dev, self.geometry.first_lba)?;
        let home = editor.ensure_dir(dev, ROOT_INODE, LOST_FOUND, 0o700, 0, 0)?;
        for (number, is_dir) in lost {
            // Имя из номера: своё имя файл потерял вместе с записью каталога,
            // и придумывать ему другое — значит выдумывать то, чего никто не
            // знает. `e2fsck` называет их так же.
            let mut name = String::new();
            name.try_reserve(8).map_err(|_| Error::NoMemory)?;
            fmt::Write::write_fmt(&mut name, format_args!("#{number}")).map_err(|_| Error::NoMemory)?;
            editor.adopt_orphan(dev, home, &name, number, is_dir)?;
            self.report.rescued += 1;
            self.report.fixed += 1;
        }
        editor.flush_everywhere(dev)?;
        editor.mark_clean(dev)?;
        Ok(())
    }

    // --- мелочи -------------------------------------------------------------

    /// Запомнить находку, если список ещё не полон.
    fn note(&mut self, problem: Problem) -> Result<()> {
        if self.report.problems.len() >= MAX_PROBLEMS {
            self.report.dropped += 1;
            return Ok(());
        }
        self.report.problems.try_reserve(1).map_err(|_| Error::NoMemory)?;
        self.report.problems.push(problem);
        Ok(())
    }

    const fn in_range(&self, block: u32) -> bool {
        block >= self.geometry.first_data_block && block < self.geometry.blocks
    }

    /// Группа и номер бита блока в её карте.
    const fn locate_block(&self, block: u32) -> (u32, u32) {
        let relative = block - self.geometry.first_data_block;
        (relative / self.geometry.blocks_per_group, relative % self.geometry.blocks_per_group)
    }
}

// --- чтение и запись, не требующие редактора ---------------------------------

fn read_block(dev: &mut dyn BlockDevice, geometry: &Geometry, block: u32) -> Result<Vec<u8>> {
    let mut buf = try_zeroed(geometry.block_size.bytes() as usize)?;
    dev.read(geometry.block_lba(block), &mut buf)?;
    Ok(buf)
}

fn write_block(
    dev: &mut dyn BlockDevice,
    geometry: &Geometry,
    block: u32,
    data: &[u8],
) -> Result<()> {
    dev.write(geometry.block_lba(block), data)?;
    Ok(())
}

fn read_inode(dev: &mut dyn BlockDevice, geometry: &Geometry, number: u32) -> Result<InodeData> {
    let (block, within) = inode_place(geometry, number)?;
    let buf = read_block(dev, geometry, block)?;
    Ok(InodeData::decode(&buf[within..within + INODE_SIZE]))
}

fn write_inode(
    dev: &mut dyn BlockDevice,
    geometry: &Geometry,
    number: u32,
    inode: &InodeData,
) -> Result<()> {
    let (block, within) = inode_place(geometry, number)?;
    let mut buf = read_block(dev, geometry, block)?;
    inode.encode(&mut buf[within..within + INODE_SIZE]);
    write_block(dev, geometry, block, &buf)
}

fn inode_place(geometry: &Geometry, number: u32) -> Result<(u32, usize)> {
    let (group, index) = geometry.locate_inode(number)?;
    let block_bytes = geometry.block_size.bytes() as usize;
    let byte_offset = index as usize * INODE_SIZE;
    let block = geometry.inode_table_block(group) + (byte_offset / block_bytes) as u32;
    Ok((block, byte_offset % block_bytes))
}

/// Строка в куче с проверкой выделения.
fn own(text: &str) -> Result<String> {
    let mut out = String::new();
    out.try_reserve_exact(text.len()).map_err(|_| Error::NoMemory)?;
    out.push_str(text);
    Ok(out)
}

/// Дописать номер блока в список, если он не нулевой.
fn push(list: &mut Vec<u32>, block: u32) -> Result<()> {
    if block == 0 {
        return Ok(());
    }
    list.try_reserve(1).map_err(|_| Error::NoMemory)?;
    list.push(block);
    Ok(())
}
