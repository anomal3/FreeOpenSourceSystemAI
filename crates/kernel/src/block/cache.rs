//! Кеш блоков файловой системы (фаза 68d).
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
//! Обёртка над носителем: чтение одного блока файловой системы (не длиннее
//! [`MAX_UNIT`]) запоминается по номеру сектора, повторное отдаётся из
//! памяти. Длинные чтения — отрезки подряд идущих блоков — идут мимо: их
//! запоминать незачем, крейт сам читает их одним запросом. Запись идёт на
//! носитель всегда (кеш не откладывает её) и выбрасывает всё, что
//! пересекается с записанным, — устаревший блок отдан не будет.
//!
//! Вытеснение — «часы»: у записи бит обращения, стрелка обходит записи по
//! кругу и выбрасывает первую, к которой с прошлого круга не обращались.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Сколько блоков помнить: 1024 блока — 4 МиБ при блоке в 4 КиБ.
const CAPACITY: usize = 1024;
/// Длиннее этого чтение не запоминается.
const MAX_UNIT: usize = 4096;

struct Entry {
    lba: u64,
    data: Box<[u8]>,
    used: bool,
}

/// Носитель с кешем блоков.
pub struct Cached<D> {
    inner: D,
    entries: Vec<Entry>,
    index: BTreeMap<u64, usize>,
    hand: usize,
}

impl<D: disk::BlockDevice> Cached<D> {
    pub fn new(inner: D) -> Self {
        Self { inner, entries: Vec::new(), index: BTreeMap::new(), hand: 0 }
    }

    /// Носитель под кешем.
    pub const fn inner(&self) -> &D {
        &self.inner
    }

    fn sectors(&self, bytes: usize) -> u64 {
        let sector = u64::from(self.inner.sector_size().max(1));
        (bytes as u64).div_ceil(sector)
    }

    /// Запомнить прочитанный блок.
    fn remember(&mut self, lba: u64, data: &[u8]) {
        if self.entries.len() < CAPACITY {
            if self.entries.try_reserve(1).is_err() {
                return;
            }
            self.index.insert(lba, self.entries.len());
            self.entries.push(Entry { lba, data: data.into(), used: false });
            return;
        }
        // Часы: первая запись без обращения с прошлого круга.
        loop {
            let slot = self.hand;
            self.hand = (self.hand + 1) % self.entries.len();
            let entry = &mut self.entries[slot];
            if entry.used {
                entry.used = false;
                continue;
            }
            self.index.remove(&entry.lba);
            if entry.data.len() == data.len() {
                entry.data.copy_from_slice(data);
            } else {
                entry.data = data.into();
            }
            entry.lba = lba;
            self.index.insert(lba, slot);
            return;
        }
    }

    /// Выбросить всё, что пересекается с секторами `[lba, lba + count)`.
    fn forget(&mut self, lba: u64, count: u64) {
        // Запись может начинаться раньше `lba` и заходить в диапазон: самая
        // длинная — `MAX_UNIT` байт, поэтому смотреть назад дальше не нужно.
        let back = self.sectors(MAX_UNIT);
        let start = lba.saturating_sub(back);
        let end = lba.saturating_add(count);
        let stale: Vec<(u64, usize)> =
            self.index.range(start..end).map(|(key, slot)| (*key, *slot)).collect();
        for (key, slot) in stale {
            let span = self.sectors(self.entries[slot].data.len());
            if key + span > lba {
                self.index.remove(&key);
                // Слот остаётся занятым, но без ключа — его переиспользуют
                // часы: запись без обращения, ключа в индексе нет.
                self.entries[slot].used = false;
                self.entries[slot].lba = u64::MAX;
            }
        }
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
        if buf.len() > MAX_UNIT {
            return self.inner.read(lba, buf);
        }
        if let Some(&slot) = self.index.get(&lba) {
            let entry = &mut self.entries[slot];
            if entry.data.len() == buf.len() {
                buf.copy_from_slice(&entry.data);
                entry.used = true;
                return Ok(());
            }
        }
        self.inner.read(lba, buf)?;
        if self.index.contains_key(&lba) {
            // Тот же сектор, другая длина: прежняя запись уступает новой.
            let count = self.sectors(buf.len());
            self.forget(lba, count);
        }
        self.remember(lba, buf);
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        let count = self.sectors(buf.len());
        self.forget(lba, count);
        self.inner.write(lba, buf)
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.inner.flush()
    }
}
