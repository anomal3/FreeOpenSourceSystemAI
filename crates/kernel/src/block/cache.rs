//! Кеш блоков файловой системы (фаза 68d, в кадрах памяти — фаза 74).
//!
//! # Зачем
//!
//! Чтение файла в ext2 на каждый вызов заново проходит таблицы косвенности:
//! у файла в 29 МБ это две таблицы и сам блок — три запроса к носителю на
//! одно чтение, и каждый ждёт устройство. Разбор таблиц внутри одного вызова
//! крейт `ext2` уже помнит, а между вызовами — нет. Программа, читающая файл
//! мелкими кусками вразброс (Doom грузит текстуры тысячами `fseek` + `fread`
//! по WAD), упиралась в это намертво: минуты на загрузку уровня при простое
//! процессора.
//!
//! # Как устроен
//!
//! Обёртка над носителем: блок файловой системы (не длиннее [`MAX_UNIT`])
//! запоминается по номеру сектора, повторное чтение отдаётся из памяти.
//! Длинное чтение, кратное [`MAX_UNIT`], режется на блоки: что есть — из
//! памяти, отрезки пропусков — одним запросом каждый, и прочитанное тоже
//! запоминается (до фазы 74 длинные чтения шли мимо, и Rider перечитывал
//! одни и те же файлы с диска).
//!
//! Блоки лежат в **кадрах физической памяти**, а не в куче ядра (64 МиБ на
//! всё): кэш растёт, пока свободно больше четверти памяти, до 1/16 всей
//! памяти на все тома вместе ([`budget`]). Дальше — вытеснение «часами»: у
//! записи бит обращения, стрелка обходит записи по кругу и берёт первую, к
//! которой с прошлого круга не обращались.
//!
//! # Отложенная запись (фаза 74)
//!
//! Для ext2 кэш включается в режиме отложенной записи ([`Cached::deferred`]):
//! блок, кратный [`MAX_UNIT`] или короче его, не уходит на носитель сразу, а
//! остаётся в кэше помеченным грязным; повторная запись того же блока
//! (счётчики тома, суперблок, карты) просто заменяет прежнюю. Сброс —
//! [`Cached::write_back`]: грязные блоки по возрастанию номера, подряд идущие
//! склеены в один запрос. Вызывают его `flush`, служебная задача раз в
//! несколько секунд, `fsync`/`sync` и выключение. Как у Linux на ext2 без
//! журнала: внезапное выключение теряет последние секунды, а `fsck` чинит
//! том. Чтение или запись другой геометрии поверх грязного блока сперва
//! сбрасывают всё грязное — чужая геометрия не склеивается.
//!
//! Правило, на котором держится верность: запись **выбрасывает** все записи
//! кэша, пересекающиеся с ней иначе, чем точным совпадением, — поэтому
//! найденная по точному ключу запись всегда последнее слово о своих секторах.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::mm::{FrameAllocator, PhysAddr};

/// Длиннее этого блок не запоминается (и длинные запросы режутся на такие).
const MAX_UNIT: usize = 4096;
/// Самый длинный склеенный запрос при сбросе.
const MAX_RUN: usize = 256 * 1024;
/// Больше скольких грязных блоков не копить (64 МиБ) — дальше сброс.
const MAX_DIRTY: usize = 16 * 1024;
/// Самая длинная запись, которая откладывается (длиннее — сразу на носитель).
const MAX_DEFERRED_WRITE: usize = 1024 * 1024;

/// Сколько кадров держат кэши всех томов.
static HELD: AtomicUsize = AtomicUsize::new(0);

/// Предел кадров на все кэши блоков: 1/16 памяти, от 4 до 256 МиБ.
///
/// Потолок — из-за кучи ядра (64 МиБ на всё): запись кэша — 32 байта в
/// массиве и узел индекса, на 64 тысячи блоков это уже 4–5 МиБ кучи.
fn budget(total: usize) -> usize {
    (total / 16).clamp(1024, 64 * 1024)
}

/// Свободной кучи ядра меньше этого — кэши не растут (фаза 74: Rider с
/// Chromium в VirtualBox исчерпали кучу, и ядро упало).
pub const HEAP_RESERVE: usize = 16 * 1024 * 1024;

/// Хватает ли кучи ядра, чтобы кэшам расти.
#[must_use]
pub fn heap_roomy() -> bool {
    crate::mm::heap::stats().free > HEAP_RESERVE
}

/// Сколько кадров держат кэши блоков (для диагностики).
#[must_use]
pub fn held_frames() -> usize {
    HELD.load(Ordering::Relaxed)
}

struct Entry {
    lba: u64,
    frame: PhysAddr,
    len: usize,
    used: bool,
    dirty: bool,
}

impl Entry {
    fn bytes(&self) -> &[u8] {
        // SAFETY: кадр принадлежит этому кэшу и виден через прямое
        // отображение; `len` не больше страницы.
        unsafe { core::slice::from_raw_parts(self.frame.to_direct_map().as_usize() as *const u8, self.len) }
    }

    fn fill(&mut self, data: &[u8]) {
        self.len = data.len();
        // SAFETY: см. `bytes`; кадр ничем больше не отображён.
        unsafe {
            core::slice::from_raw_parts_mut(self.frame.to_direct_map().as_usize() as *mut u8, data.len())
                .copy_from_slice(data);
        }
    }
}

/// Носитель с кешем блоков.
pub struct Cached<D> {
    inner: D,
    entries: Vec<Entry>,
    index: BTreeMap<u64, usize>,
    hand: usize,
    /// Режим отложенной записи включён.
    deferred: bool,
    /// Ключи грязных записей (записаны в кэш, но не на носитель).
    dirty: BTreeSet<u64>,
}

impl<D: disk::BlockDevice> Cached<D> {
    pub fn new(inner: D) -> Self {
        Self { inner, entries: Vec::new(), index: BTreeMap::new(), hand: 0, deferred: false, dirty: BTreeSet::new() }
    }

    /// Кэш с отложенной записью.
    pub fn deferred(inner: D) -> Self {
        let mut cache = Self::new(inner);
        cache.deferred = true;
        cache
    }

    /// Есть ли не сброшенное.
    pub fn is_dirty(&self) -> bool {
        !self.dirty.is_empty()
    }

    /// Носитель под кешем.
    pub const fn inner(&self) -> &D {
        &self.inner
    }

    fn sectors(&self, bytes: usize) -> u64 {
        let sector = u64::from(self.inner.sector_size().max(1));
        (bytes as u64).div_ceil(sector)
    }

    /// Записать грязные блоки на носитель (без `flush` самого носителя).
    pub fn write_back(&mut self) -> disk::Result<()> {
        self.write_back_some(usize::MAX).map(|_| ())
    }

    /// Записать грязные блоки, но не больше примерно `limit` байт; `true` —
    /// грязное ещё осталось. Порциями пишет служебная задача: между ними
    /// отпускается замок тома, и чтение из кэша не ждёт всего сброса.
    pub fn write_back_some(&mut self, limit: usize) -> disk::Result<bool> {
        if self.dirty.is_empty() {
            return Ok(false);
        }
        let mut flushed = 0usize;
        let keys: Vec<u64> = self.dirty.iter().copied().collect();
        let mut run: Vec<u8> = Vec::new();
        let mut run_lba = 0u64;
        let mut run_keys: Vec<u64> = Vec::new();
        for lba in keys {
            let Some(&slot) = self.index.get(&lba) else {
                self.dirty.remove(&lba);
                continue;
            };
            let len = self.entries[slot].len;
            let end = run_lba + self.sectors(run.len());
            if !run.is_empty() && (end != lba || run.len() + len > MAX_RUN) {
                self.flush_run(run_lba, &run, &run_keys)?;
                flushed += run.len();
                run.clear();
                run_keys.clear();
                if flushed >= limit {
                    return Ok(!self.dirty.is_empty());
                }
            }
            if run.is_empty() {
                run_lba = lba;
            }
            run.extend_from_slice(self.entries[slot].bytes());
            run_keys.push(lba);
        }
        if !run.is_empty() {
            self.flush_run(run_lba, &run, &run_keys)?;
        }
        Ok(!self.dirty.is_empty())
    }

    /// Записать склеенный отрезок и снять с его блоков пометку «грязный».
    /// Не записалось — блоки остаются грязными.
    fn flush_run(&mut self, lba: u64, run: &[u8], keys: &[u64]) -> disk::Result<()> {
        self.inner.write(lba, run)?;
        for key in keys {
            self.dirty.remove(key);
            if let Some(&slot) = self.index.get(key) {
                self.entries[slot].dirty = false;
            }
        }
        Ok(())
    }

    /// Пересекается ли `[lba, lba + count)` с грязным блоком, который не
    /// совпадает с одним из кусков `[lba + k·unit, +unit_len)` целиком.
    fn dirty_overlaps(&self, lba: u64, count: u64, unit_len: usize) -> bool {
        if self.dirty.is_empty() {
            return false;
        }
        let back = self.sectors(MAX_UNIT);
        let unit = self.sectors(unit_len).max(1);
        let start = lba.saturating_sub(back);
        let end = lba.saturating_add(count);
        self.dirty.range(start..end).any(|key| {
            let len = self.index.get(key).map_or(0, |&slot| self.entries[slot].len);
            let span = self.sectors(len);
            let exact = *key >= lba && (key - lba) % unit == 0 && len == unit_len && key + span <= end;
            key + span > lba && !exact
        })
    }

    /// Свободный слот под новую запись: новый кадр, пока позволяет память,
    /// иначе — вытесненная «часами» чистая запись. `None` — всё грязное или
    /// кадров нет совсем.
    fn slot_for_new(&mut self) -> Option<usize> {
        let held = HELD.load(Ordering::Relaxed);
        let mut pressure = false;
        let roomy = heap_roomy();
        let grown = crate::mm::frame::with(|frames| {
            let stats = frames.stats();
            pressure = stats.free <= stats.total / 8;
            if !roomy || held >= budget(stats.total) || stats.free <= stats.total / 4 {
                return None;
            }
            frames.allocate()
        })
        .flatten();
        // Памяти мало (программам) — кэш отдаёт по кадру на каждый новый блок,
        // пока не сожмётся до 1 МиБ.
        if pressure && self.entries.len() > 256 {
            self.release_one();
        }
        if let Some(frame) = grown {
            // Массив резервируется сразу под весь предел: удвоение посреди
            // работы просило бы у кучи цельный кусок в мегабайты.
            if self.entries.capacity() == self.entries.len() && self.entries.is_empty() {
                let whole = crate::mm::frame::stats().total;
                let _ = self.entries.try_reserve_exact(budget(whole));
            }
            if self.entries.try_reserve(1).is_ok() {
                HELD.fetch_add(1, Ordering::Relaxed);
                self.entries.push(Entry { lba: u64::MAX, frame, len: 0, used: false, dirty: false });
                return Some(self.entries.len() - 1);
            }
            // SAFETY: кадр только что выдан и никуда не попал.
            crate::mm::frame::with(|frames| unsafe { frames.free(frame) });
        }
        if self.entries.is_empty() {
            return None;
        }
        // Часы: первая чистая запись без обращения с прошлого круга. Два
        // круга без находки — всё грязное или всё в ходу.
        for _ in 0..self.entries.len() * 2 {
            let slot = self.hand;
            self.hand = (self.hand + 1) % self.entries.len();
            let entry = &mut self.entries[slot];
            if entry.dirty {
                continue;
            }
            if entry.used {
                entry.used = false;
                continue;
            }
            if entry.lba != u64::MAX {
                let lba = entry.lba;
                entry.lba = u64::MAX;
                self.index.remove(&lba);
            }
            return Some(slot);
        }
        None
    }

    /// Отдать в пул кадр одной чистой записи.
    fn release_one(&mut self) {
        let Some(slot) = (0..self.entries.len())
            .map(|step| (self.hand + step) % self.entries.len())
            .find(|&slot| !self.entries[slot].dirty)
        else {
            return;
        };
        let entry = self.entries.swap_remove(slot);
        if entry.lba != u64::MAX {
            self.index.remove(&entry.lba);
        }
        // Последняя запись встала на место ушедшей — её ключ теперь там.
        if slot < self.entries.len() {
            let moved = self.entries[slot].lba;
            if moved != u64::MAX {
                self.index.insert(moved, slot);
            }
        }
        if self.hand >= self.entries.len() {
            self.hand = 0;
        }
        // SAFETY: кадр принадлежал этой записи и ничем не отображён.
        crate::mm::frame::with(|frames| unsafe { frames.free(entry.frame) });
        HELD.fetch_sub(1, Ordering::Relaxed);
    }

    /// Положить блок в кэш (заменив запись с тем же ключом). `false` — места
    /// нет (кадров нет, всё грязное).
    fn put(&mut self, lba: u64, data: &[u8], dirty: bool) -> bool {
        let slot = match self.index.get(&lba) {
            Some(&slot) => slot,
            None => {
                let Some(slot) = self.slot_for_new() else {
                    return false;
                };
                self.entries[slot].lba = lba;
                self.index.insert(lba, slot);
                slot
            }
        };
        let entry = &mut self.entries[slot];
        entry.fill(data);
        entry.used = true;
        entry.dirty |= dirty;
        if entry.dirty {
            self.dirty.insert(lba);
        }
        true
    }

    /// Выбросить всё, что пересекается с секторами `[lba, lba + count)`,
    /// кроме кусков `[lba + k·unit, +unit_len)` (их перепишут на месте).
    /// Грязных среди выбрасываемых быть не должно — их сбросили раньше.
    fn forget(&mut self, lba: u64, count: u64, unit_len: usize) {
        // Запись может начинаться раньше `lba` и заходить в диапазон: самая
        // длинная — `MAX_UNIT` байт, поэтому смотреть назад дальше не нужно.
        let back = self.sectors(MAX_UNIT);
        let unit = self.sectors(unit_len).max(1);
        let start = lba.saturating_sub(back);
        let end = lba.saturating_add(count);
        let stale: Vec<(u64, usize)> =
            self.index.range(start..end).map(|(key, slot)| (*key, *slot)).collect();
        for (key, slot) in stale {
            let len = self.entries[slot].len;
            let span = self.sectors(len);
            let exact = key >= lba && (key - lba) % unit == 0 && len == unit_len && key + span <= end;
            if key + span > lba && !exact {
                self.index.remove(&key);
                self.dirty.remove(&key);
                let entry = &mut self.entries[slot];
                entry.used = false;
                entry.dirty = false;
                entry.lba = u64::MAX;
            }
        }
    }

    /// Чтение, кратное [`MAX_UNIT`]: по блокам, пропуски — отрезками.
    fn read_units(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        let unit = self.sectors(MAX_UNIT);
        let units = buf.len() / MAX_UNIT;
        let mut at = 0;
        while at < units {
            let key = lba + at as u64 * unit;
            if let Some(&slot) = self.index.get(&key) {
                let entry = &mut self.entries[slot];
                if entry.len == MAX_UNIT {
                    buf[at * MAX_UNIT..(at + 1) * MAX_UNIT].copy_from_slice(entry.bytes());
                    entry.used = true;
                    at += 1;
                    continue;
                }
            }
            // Отрезок пропусков: до первого блока, который есть в кэше.
            let mut miss_end = at + 1;
            while miss_end < units && !self.index.contains_key(&(lba + miss_end as u64 * unit)) {
                miss_end += 1;
            }
            let part = &mut buf[at * MAX_UNIT..miss_end * MAX_UNIT];
            self.inner.read(key, part)?;
            for k in at..miss_end {
                let chunk_key = lba + k as u64 * unit;
                if !self.put(chunk_key, &buf[k * MAX_UNIT..(k + 1) * MAX_UNIT], false) {
                    break;
                }
            }
            at = miss_end;
        }
        Ok(())
    }
}

impl<D> Drop for Cached<D> {
    fn drop(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let count = self.entries.len();
        crate::mm::frame::with(|frames| {
            for entry in &self.entries {
                // SAFETY: кадры выдавались этому кэшу и ничем не отображены.
                unsafe { frames.free(entry.frame) };
            }
        });
        HELD.fetch_sub(count, Ordering::Relaxed);
    }
}

impl<D: disk::BlockDevice> disk::BlockDevice for Cached<D> {
    fn sector_size(&self) -> u32 {
        self.inner.sector_size()
    }

    fn sector_count(&self) -> u64 {
        self.inner.sector_count()
    }

    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        let count = self.sectors(buf.len());
        if buf.len() > MAX_UNIT {
            let units = buf.len() % MAX_UNIT == 0;
            if self.dirty_overlaps(lba, count, if units { MAX_UNIT } else { usize::MAX }) {
                self.write_back()?;
            }
            if units {
                return self.read_units(lba, buf);
            }
            return self.inner.read(lba, buf);
        }
        if let Some(&slot) = self.index.get(&lba) {
            let entry = &mut self.entries[slot];
            if entry.len == buf.len() {
                buf.copy_from_slice(entry.bytes());
                entry.used = true;
                return Ok(());
            }
        }
        if self.dirty_overlaps(lba, count, buf.len()) {
            self.write_back()?;
        }
        self.inner.read(lba, buf)?;
        self.put(lba, buf, false);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        let count = self.sectors(buf.len());
        // Геометрия записи: один блок до `MAX_UNIT` или ряд блоков по нему.
        let unit_len = if buf.len() <= MAX_UNIT {
            buf.len()
        } else if buf.len() % MAX_UNIT == 0 {
            MAX_UNIT
        } else {
            usize::MAX
        };
        if self.dirty_overlaps(lba, count, unit_len) {
            self.write_back()?;
        }
        self.forget(lba, count, unit_len);
        let defer = self.deferred && unit_len != usize::MAX && buf.len() <= MAX_DEFERRED_WRITE;
        if !defer {
            self.inner.write(lba, buf)?;
        }
        // Кэш получает записанное в любом случае: следующее чтение — из
        // памяти. Не легло (места нет) — пишется на носитель сейчас.
        let unit = self.sectors(unit_len.min(MAX_UNIT)).max(1);
        let pieces = if unit_len == usize::MAX { 0 } else { buf.len().div_ceil(unit_len) };
        for k in 0..pieces {
            let piece = &buf[k * unit_len..((k + 1) * unit_len).min(buf.len())];
            let key = lba + k as u64 * unit;
            if !self.put(key, piece, defer) && defer {
                self.forget(key, self.sectors(piece.len()), usize::MAX);
                self.inner.write(key, piece)?;
            }
        }
        if self.dirty.len() >= MAX_DIRTY {
            self.write_back()?;
        }
        Ok(())
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.write_back()?;
        self.inner.flush()
    }
}
