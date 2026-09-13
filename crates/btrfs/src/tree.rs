//! Чтение блоков тома и обход B-дерева.
//!
//! # Что проверяется у каждого прочитанного узла
//!
//! Три вещи, и все три — против ошибки, которая иначе выглядит как обычные
//! данные:
//!
//! 1. **контрольная сумма** — носитель мог отдать не те байты;
//! 2. **собственный адрес** в заголовке — узел знает, где он лежит, и если это
//!    не тот адрес, по которому мы читали, значит сбился перевод логического
//!    адреса (самая правдоподобная из ошибок: мусор при этом выглядит как
//!    дерево);
//! 3. **`fsid`** — узел от другого тома.
//!
//! Ни одна из трёх не стоит заметного времени, а без них порча в середине
//! дерева проявится не отказом, а неправильным содержимым файла.
//!
//! # Почему обход не держит узлы пути в памяти
//!
//! Обычная реализация тащит с собой весь путь от корня — по узлу на уровень,
//! то есть до восьми блоков по 16 КиБ. Здесь хранятся только адреса и номера
//! слотов, а родитель перечитывается в тот момент, когда лист кончился.
//!
//! Плата — одно-два лишних чтения на каждый лист (а не на каждый элемент);
//! выгода — обход каталога из тысяч записей не держит сотню килобайт, и в
//! ядре, где под кучей несколько мегабайт, это правильный обмен.

use alloc::vec::Vec;

use disk::BlockDevice;

use crate::chunk::ChunkMap;
use crate::layout::*;
use crate::{Error, Result, try_zeroed};

/// Предел глубины дерева.
///
/// Формат допускает восемь уровней, и больше не бывает по построению. Предел
/// нужен не ради экономии, а чтобы испорченное дерево со ссылкой на самого
/// себя кончалось ошибкой, а не зависанием.
const MAX_LEVEL: usize = 8;

/// Сколько памяти отдаётся под запомненные узлы дерева.
///
/// Считается в байтах, а не в узлах: `nodesize` бывает от 4 до 64 КиБ, и
/// «восемь узлов» означало бы то полсотни килобайт, то полмегабайта. Сто
/// двадцать восемь килобайт — это всегда сто двадцать восемь килобайт.
const CACHE_BYTES: usize = 128 * 1024;

/// Сколько узлов держать, если под выбранный размер их помещается совсем мало.
///
/// Четыре — не круглое число, а минимум, при котором кэш вообще имеет смысл:
/// одно чтение из файла трогает корень и лист дерева ФС плюс корень и лист
/// дерева сумм. При трёх слотах каждое следующее чтение вытесняло бы то, что
/// понадобится через мгновение, и кэш работал бы хуже, чем его отсутствие.
const MIN_CACHE_SLOTS: usize = 4;

/// Несколько последних узлов дерева, прочитанных с диска.
///
/// # Почему кэш здесь есть, а в ext2 его нет
///
/// В ext2 указатели на блоки файла лежат в самом inode — он уже на руках у
/// вызывающего, и чтение 512 байт стоит одного обращения к диску. В btrfs их
/// нет нигде: чтобы узнать, где лежит байт, надо спуститься по дереву, и ещё
/// раз — по дереву сумм. Без кэша чтение полукилобайта обходилось бы в четыре
/// узла по 16 КиБ, то есть в **сто с лишним раз** больше данных, чем просили.
/// Это не оптимизация, это разница между «работает» и «не работает».
///
/// Вытеснение круговое, без учёта частоты. Причина та же, что у `SectorCache` в
/// FAT32: политика умнее круга требует счётчиков, счётчики требуют проверки, а
/// проверять их нечем — на дереве из двух уровней промахов почти не бывает.
///
/// **Фазе записи придётся вернуться сюда.** Пока том только читается,
/// запомненный узел не может устареть; с первой же записью — может.
struct NodeCache {
    slots: Vec<(u64, Vec<u8>)>,
    /// Куда положить следующий узел.
    next: usize,
}

impl NodeCache {
    fn new(nodesize: u32) -> Self {
        let slots = (CACHE_BYTES / nodesize.max(1) as usize).max(MIN_CACHE_SLOTS);
        let mut store = Vec::new();
        // Нехватка памяти здесь не отказ: кэш — ускорение, а не условие
        // работы, и том обязан читаться даже тогда, когда под кэш не нашлось
        // ни слота.
        if store.try_reserve_exact(slots).is_ok() {
            store.resize_with(slots, || (0, Vec::new()));
        }
        Self { slots: store, next: 0 }
    }

    fn get(&self, logical: u64) -> Option<&[u8]> {
        self.slots
            .iter()
            .find(|(address, node)| *address == logical && !node.is_empty())
            .map(|(_, node)| node.as_slice())
    }

    fn put(&mut self, logical: u64, node: &[u8]) {
        if self.slots.is_empty() {
            return;
        }
        let mut copy = Vec::new();
        if copy.try_reserve_exact(node.len()).is_err() {
            return;
        }
        copy.extend_from_slice(node);
        self.slots[self.next] = (logical, copy);
        self.next = (self.next + 1) % self.slots.len();
    }
}

/// Геометрия тома — всё, что нужно, чтобы прочитать любой блок.
///
/// Существует отдельно от [`crate::Btrfs`] потому, что нужна **до** того, как
/// том смонтирован: дерево кусков читается тем же кодом, что и всё остальное,
/// и к моменту его чтения ни корней, ни метки ещё нет.
pub(crate) struct Volume {
    pub(crate) chunks: ChunkMap,
    pub(crate) nodesize: u32,
    pub(crate) sectorsize: u32,
    pub(crate) fsid: [u8; 16],
    /// Поколение последней завершённой транзакции, из суперблока.
    pub(crate) generation: u64,
    /// Первый сектор раздела на носителе.
    pub(crate) first_lba: u64,
    cache: NodeCache,
}

impl Volume {
    /// Прочитать `buf.len()` байт по физическому смещению внутри раздела.
    ///
    /// Смещение байтовое, а носитель адресуется секторами, и они не обязаны
    /// совпадать: на диске с сектором 4096 начало 16-килобайтного узла может
    /// прийтись на его середину. Ровно на этом ломался ext2 до Phase 26c —
    /// формула «смещение делить на 512» верна по совпадению, пока сектор 512.
    fn read_physical(
        &self,
        dev: &mut dyn BlockDevice,
        physical: u64,
        buf: &mut [u8],
    ) -> Result<()> {
        let sector = u64::from(dev.sector_size());
        if sector == 0 {
            return Err(Error::Unsupported);
        }
        let base = self
            .first_lba
            .checked_mul(sector)
            .and_then(|start| start.checked_add(physical))
            .ok_or(Error::Corrupt)?;
        let lba = base / sector;
        let within = (base % sector) as usize;

        // Выровненное чтение идёт **прямо в буфер вызывающего**. Это не
        // украшение: так читаются данные файла, и лишняя копия мегабайтов
        // через промежуточный буфер видна в эмуляторе как секунды.
        if within == 0 && buf.len() % sector as usize == 0 {
            dev.read(lba, buf)?;
            return Ok(());
        }

        let span = (within + buf.len()).div_ceil(sector as usize);
        let mut raw = try_zeroed(span * sector as usize)?;
        dev.read(lba, &mut raw)?;
        buf.copy_from_slice(&raw[within..within + buf.len()]);
        Ok(())
    }

    /// Прочитать `buf.len()` байт по логическому адресу.
    ///
    /// Отрезок обязан целиком лежать в одном куске: разрыв по границе куска
    /// разбирается вызывающим, потому что только он знает, можно ли отдать
    /// часть.
    pub(crate) fn read_logical(
        &mut self,
        dev: &mut dyn BlockDevice,
        logical: u64,
        buf: &mut [u8],
    ) -> Result<()> {
        let (physical, run) = self.chunks.translate(logical)?;
        if (buf.len() as u64) > run {
            return Err(Error::Corrupt);
        }
        self.read_physical(dev, physical, buf)
    }

    /// Сколько байт после этого логического адреса лежат на диске подряд.
    pub(crate) fn contiguous(&self, logical: u64) -> Result<u64> {
        Ok(self.chunks.translate(logical)?.1)
    }

    /// Завести том и кэш под него.
    pub(crate) fn new(
        chunks: ChunkMap,
        nodesize: u32,
        sectorsize: u32,
        fsid: [u8; 16],
        generation: u64,
        first_lba: u64,
    ) -> Self {
        Self {
            chunks,
            nodesize,
            sectorsize,
            fsid,
            generation,
            first_lba,
            cache: NodeCache::new(nodesize),
        }
    }

    /// Прочитать узел дерева и убедиться, что это он.
    ///
    /// Узел отдаётся копией, а не ссылкой в кэш: обход, переходя к следующему
    /// листу, читает родителя, не отпуская лист, и ссылка в тот же кэш стала бы
    /// висячей. Копия 16 КиБ стоит несопоставимо меньше, чем обращение к диску
    /// и пересчёт контрольной суммы, ради которых кэш и заведён.
    pub(crate) fn read_node(
        &mut self,
        dev: &mut dyn BlockDevice,
        logical: u64,
    ) -> Result<Vec<u8>> {
        if let Some(hit) = self.cache.get(logical) {
            let mut copy = Vec::new();
            copy.try_reserve_exact(hit.len()).map_err(|_| Error::NoMemory)?;
            copy.extend_from_slice(hit);
            return Ok(copy);
        }
        let mut node = try_zeroed(self.nodesize as usize)?;
        self.read_logical(dev, logical, &mut node)?;
        verify_block(&node)?;

        let header = Header::parse(&node)?;
        if header.bytenr != logical {
            // Сумма сошлась, а адрес не тот — значит прочитан настоящий узел,
            // но не тот, который просили. Это ошибка перевода адреса, и она
            // куда вероятнее порчи носителя.
            return Err(Error::Corrupt);
        }
        if node[HDR_FSID..HDR_FSID + 16] != self.fsid {
            return Err(Error::Corrupt);
        }
        if header.generation > self.generation {
            // Узел из поколения, которого суперблок ещё не подтвердил. Сумма у
            // такого блока сходится — её посчитали при записи, — но
            // транзакция, к которой он относится, не завершилась, и остальные
            // её блоки могут быть не записаны вовсе. Это единственная порча,
            // которую контрольная сумма поймать не может в принципе.
            return Err(Error::Corrupt);
        }
        let per_item = if header.level == 0 { ITEM_SIZE } else { KEY_PTR_SIZE };
        if HEADER_SIZE + header.nritems as usize * per_item > node.len() {
            return Err(Error::Corrupt);
        }
        // В кэш узел попадает **проверенным**: иначе повторное чтение
        // возвращало бы его, минуя все три проверки, и порча, найденная один
        // раз, во второй прошла бы молча.
        self.cache.put(logical, &node);
        Ok(node)
    }
}

/// Положение в дереве: лист в памяти и путь к нему адресами.
pub(crate) struct Cursor {
    /// (логический адрес узла, выбранный слот) сверху вниз; последний элемент
    /// описывает сам лист.
    path: Vec<(u64, usize)>,
    leaf: Vec<u8>,
    slot: usize,
    nritems: usize,
    done: bool,
}

impl Cursor {
    /// Встать на первый элемент дерева с ключом не меньше `target`.
    pub(crate) fn seek(
        volume: &mut Volume,
        dev: &mut dyn BlockDevice,
        root: u64,
        level: u8,
        target: Key,
    ) -> Result<Self> {
        if level as usize >= MAX_LEVEL {
            return Err(Error::Corrupt);
        }
        let mut path = Vec::new();
        path.try_reserve_exact(level as usize + 1).map_err(|_| Error::NoMemory)?;

        let mut address = root;
        let mut expected = level;
        loop {
            let node = volume.read_node(dev, address)?;
            let header = Header::parse(&node)?;
            // Уровень узла обязан совпасть с обещанным: иначе мы разбираем
            // лист как внутренний узел или наоборот, а форматы у них разные.
            if header.level != expected {
                return Err(Error::Corrupt);
            }
            let count = header.nritems as usize;

            if header.level == 0 {
                let slot = leaf_lower_bound(&node, count, target);
                path.push((address, slot));
                let mut cursor = Self { path, leaf: node, slot, nritems: count, done: false };
                if slot >= count {
                    cursor.next_leaf(volume, dev)?;
                }
                return Ok(cursor);
            }

            if count == 0 {
                return Err(Error::Corrupt);
            }
            let slot = node_upper_bound(&node, count, target);
            path.push((address, slot));
            address = node_child(&node, slot);
            expected -= 1;
        }
    }

    /// Ключ текущего элемента; `None` — обход кончился.
    pub(crate) fn key(&self) -> Option<Key> {
        if self.done || self.slot >= self.nritems {
            None
        } else {
            Some(leaf_key(&self.leaf, self.slot))
        }
    }

    /// Данные текущего элемента.
    pub(crate) fn item(&self) -> Result<&[u8]> {
        if self.done || self.slot >= self.nritems {
            return Err(Error::NotFound);
        }
        leaf_item(&self.leaf, self.slot)
    }

    /// Перейти к следующему элементу дерева.
    pub(crate) fn next(&mut self, volume: &mut Volume, dev: &mut dyn BlockDevice) -> Result<()> {
        if self.done {
            return Ok(());
        }
        self.slot += 1;
        if self.slot < self.nritems {
            return Ok(());
        }
        self.next_leaf(volume, dev)
    }

    /// Перейти к предыдущему элементу дерева.
    ///
    /// Нужен там, где ищется «элемент, покрывающий адрес»: экстент файла и
    /// контрольная сумма сектора найдены **не** точным ключом, а последним, чей
    /// ключ не больше искомого. Поиск даёт первый не меньший — то есть, как
    /// правило, следующий за нужным, и один шаг назад обязателен.
    ///
    /// Возвращает `false`, если шагать было некуда: обход при этом остаётся на
    /// месте, а не уходит в неопределённое состояние.
    pub(crate) fn prev(&mut self, volume: &mut Volume, dev: &mut dyn BlockDevice) -> Result<bool> {
        // Обход, дошедший до конца, всё ещё стоит на последнем листе — шаг
        // назад из этого положения осмыслен и нужен: ровно так находится
        // экстент, лежащий за всеми прочитанными.
        if !self.done && self.slot > 0 {
            self.slot -= 1;
            return Ok(true);
        }
        if !self.done && self.nritems == 0 {
            return Ok(false);
        }

        let Some(mut level) = self.path.len().checked_sub(1) else {
            return Ok(false);
        };
        if self.done && self.nritems > 0 {
            // Стоим за последним элементом текущего листа — назад ходить не
            // надо, достаточно вернуться на него.
            self.slot = self.nritems - 1;
            self.done = false;
            return Ok(true);
        }

        let mut child = None;
        while level > 0 {
            level -= 1;
            let (address, slot) = self.path[level];
            if slot > 0 {
                let node = volume.read_node(dev, address)?;
                self.path[level].1 = slot - 1;
                child = Some((level + 1, node_child(&node, slot - 1)));
                break;
            }
        }
        let Some((mut depth, mut address)) = child else {
            return Ok(false);
        };

        // Спуск по правому краю поддерева.
        loop {
            let node = volume.read_node(dev, address)?;
            let header = Header::parse(&node)?;
            if depth >= self.path.len() || header.nritems == 0 {
                return Err(Error::Corrupt);
            }
            let last = header.nritems as usize - 1;
            self.path[depth] = (address, last);
            if header.level == 0 {
                self.leaf = node;
                self.nritems = header.nritems as usize;
                self.slot = last;
                self.done = false;
                return Ok(true);
            }
            address = node_child(&node, last);
            depth += 1;
        }
    }

    /// Перейти на первый элемент следующего листа.
    ///
    /// Поднимаемся, пока у какого-нибудь предка не найдётся ещё один потомок,
    /// и спускаемся от него по левому краю. Пустые листья формат не создаёт,
    /// но если такой встретится — пропускаем его, а не останавливаем обход:
    /// иначе одна порченая страница обрезала бы каталог молча.
    fn next_leaf(&mut self, volume: &mut Volume, dev: &mut dyn BlockDevice) -> Result<()> {
        loop {
            let Some(mut level) = self.path.len().checked_sub(1) else {
                self.done = true;
                return Ok(());
            };

            let mut child = None;
            while level > 0 {
                level -= 1;
                let (address, slot) = self.path[level];
                let node = volume.read_node(dev, address)?;
                let header = Header::parse(&node)?;
                if slot + 1 < header.nritems as usize {
                    self.path[level].1 = slot + 1;
                    child = Some((level + 1, node_child(&node, slot + 1)));
                    break;
                }
            }

            let Some((mut depth, mut address)) = child else {
                self.done = true;
                return Ok(());
            };

            // Спуск по левому краю поддерева.
            loop {
                let node = volume.read_node(dev, address)?;
                let header = Header::parse(&node)?;
                if depth >= self.path.len() {
                    return Err(Error::Corrupt);
                }
                self.path[depth] = (address, 0);
                if header.level == 0 {
                    self.leaf = node;
                    self.slot = 0;
                    self.nritems = header.nritems as usize;
                    break;
                }
                if header.nritems == 0 {
                    return Err(Error::Corrupt);
                }
                address = node_child(&node, 0);
                depth += 1;
            }

            if self.nritems > 0 {
                return Ok(());
            }
            // Лист оказался пуст — ищем следующий за ним.
        }
    }
}

/// Первый слот листа с ключом не меньше `target`.
fn leaf_lower_bound(node: &[u8], count: usize, target: Key) -> usize {
    let (mut low, mut high) = (0usize, count);
    while low < high {
        let middle = low + (high - low) / 2;
        if leaf_key(node, middle) < target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low
}

/// Слот внутреннего узла, в поддереве которого может лежать `target`.
///
/// Это последний потомок с ключом не больше искомого; если искомый меньше
/// первого — берётся нулевой. Последнее не оговорка: в дереве, где элемент
/// удалили, ключ первого потомка может оказаться больше любого, что мы ищем,
/// и уйти «левее нуля» некуда.
fn node_upper_bound(node: &[u8], count: usize, target: Key) -> usize {
    let (mut low, mut high) = (0usize, count);
    while low < high {
        let middle = low + (high - low) / 2;
        if node_key(node, middle) <= target {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    low.saturating_sub(1)
}
