// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Распределители диапазонов: видеопамять (арены из нескольких кусков) и
//! адреса GPU.
//!
//! Один и тот же [`RangeAlloc`] — список свободных кусков, упорядоченный по
//! адресу, первый подходящий с выравниванием, соседи при освобождении
//! сливаются. Кусков может быть сколько угодно: у WP-A служебная арена
//! видеопамяти — два окна BAR1 по обе стороны от кадров дисплея
//! (`[0x0610_0000, 0x0800_0000)` и `[0x0e80_0000, 0x1000_0000)`), а общая —
//! от `0x1000_0000` до конца области FB.
//!
//! [`VramAlloc`] — две арены: видимая процессору (таблицы страниц, блоки
//! экземпляров, USERD — всё, что пишет CPU через BAR1) и общая (буферы,
//! которые видит только GPU, контекст GR). Если общая кончилась, служебная
//! её не выручает — и наоборот: служебная мала, и забить её пользовательскими
//! буферами значит остаться без таблиц страниц.

use alloc::collections::BTreeMap;

/// Гранулярность по умолчанию — страница GPU.
pub const PAGE: u64 = 0x1000;

/// Выровнять вверх (`align` — степень двойки).
pub const fn align_up(v: u64, align: u64) -> u64 {
    (v + align - 1) & !(align - 1)
}

/// Свободные куски `[start, end)`.
#[derive(Clone, Debug, Default)]
pub struct RangeAlloc {
    /// начало -> конец (исключительно); куски не пересекаются и не касаются.
    free: BTreeMap<u64, u64>,
    total: u64,
    used: u64,
    gran: u64,
}

impl RangeAlloc {
    /// Пустой распределитель с гранулярностью `gran` (степень двойки).
    pub fn new(gran: u64) -> RangeAlloc {
        assert!(gran.is_power_of_two());
        RangeAlloc { free: BTreeMap::new(), total: 0, used: 0, gran }
    }

    /// Добавить кусок (обрезается внутрь до гранулярности). Пересечение с уже
    /// добавленным — ошибка вызывающего, кусок тогда не добавляется: `false`.
    pub fn add_range(&mut self, start: u64, len: u64) -> bool {
        let s = align_up(start, self.gran);
        let e = (start.saturating_add(len)) & !(self.gran - 1);
        if e <= s || self.overlaps(s, e) {
            return false;
        }
        self.total += e - s;
        self.insert_free(s, e);
        true
    }

    pub fn total(&self) -> u64 {
        self.total
    }
    pub fn used(&self) -> u64 {
        self.used
    }
    pub fn free_bytes(&self) -> u64 {
        self.total - self.used
    }
    pub fn granularity(&self) -> u64 {
        self.gran
    }

    /// Свободные куски по порядку (для журнала и тестов).
    pub fn free_ranges(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.free.iter().map(|(&s, &e)| (s, e))
    }

    fn overlaps(&self, s: u64, e: u64) -> bool {
        // Ближайший кусок, начинающийся до e.
        if let Some((_, &pe)) = self.free.range(..e).next_back() {
            if pe > s {
                return true;
            }
        }
        false
    }

    /// Выделить `size` байт с выравниванием `align` (оба округляются до
    /// гранулярности), первый подходящий по адресу.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        if size == 0 {
            return None;
        }
        let size = align_up(size, self.gran);
        let align = align.max(self.gran).next_power_of_two();
        let mut hit = None;
        for (&s, &e) in &self.free {
            let a = align_up(s, align);
            if a >= s && a.checked_add(size).is_some_and(|end| end <= e) {
                hit = Some(a);
                break;
            }
        }
        let a = hit?;
        self.take(a, a + size);
        Some(a)
    }

    /// Занять ровно `[addr, addr+size)`; `false`, если хоть что-то там занято
    /// или вне кусков.
    pub fn alloc_at(&mut self, addr: u64, size: u64) -> bool {
        if size == 0 || addr % self.gran != 0 {
            return false;
        }
        let size = align_up(size, self.gran);
        let Some(end) = addr.checked_add(size) else { return false };
        match self.free.range(..=addr).next_back() {
            Some((&s, &e)) if s <= addr && end <= e => {
                self.take(addr, end);
                true
            }
            _ => false,
        }
    }

    /// Вернуть `[addr, addr+size)`. Двойное освобождение и чужие адреса
    /// ловятся: `false` и ничего не меняется.
    pub fn free(&mut self, addr: u64, size: u64) -> bool {
        if size == 0 {
            return false;
        }
        let size = align_up(size, self.gran);
        let end = addr + size;
        if size > self.used || self.overlaps(addr, end) {
            return false;
        }
        self.used -= size;
        self.insert_free(addr, end);
        true
    }

    /// Свободен ли весь `[addr, addr+size)`.
    pub fn is_free(&self, addr: u64, size: u64) -> bool {
        let end = addr + size;
        matches!(self.free.range(..=addr).next_back(), Some((&s, &e)) if s <= addr && end <= e)
    }

    fn take(&mut self, a: u64, b: u64) {
        let (&s, &e) = self.free.range(..=a).next_back().expect("take: не в свободном куске");
        debug_assert!(s <= a && b <= e);
        self.free.remove(&s);
        if s < a {
            self.free.insert(s, a);
        }
        if b < e {
            self.free.insert(b, e);
        }
        self.used += b - a;
    }

    fn insert_free(&mut self, mut s: u64, mut e: u64) {
        // Слить с левым соседом.
        if let Some((&ps, &pe)) = self.free.range(..s).next_back() {
            if pe == s {
                self.free.remove(&ps);
                s = ps;
            }
        }
        // И с правым.
        if let Some(&ne) = self.free.get(&e) {
            self.free.remove(&e);
            e = ne;
        }
        self.free.insert(s, e);
    }
}

/// Какая арена видеопамяти.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Arena {
    /// Видимая процессору через BAR1: таблицы страниц, блоки экземпляров, USERD.
    Cpu,
    /// Всё остальное.
    Bulk,
}

/// Видеопамять: две арены, каждая — из нескольких кусков.
#[derive(Clone, Debug)]
pub struct VramAlloc {
    pub cpu: RangeAlloc,
    pub bulk: RangeAlloc,
}

impl Default for VramAlloc {
    fn default() -> Self {
        Self::new()
    }
}

impl VramAlloc {
    pub fn new() -> VramAlloc {
        VramAlloc { cpu: RangeAlloc::new(PAGE), bulk: RangeAlloc::new(PAGE) }
    }

    /// Собрать по спискам кусков `(начало, длина)`.
    pub fn with_ranges(cpu: &[(u64, u64)], bulk: &[(u64, u64)]) -> VramAlloc {
        let mut v = VramAlloc::new();
        for &(s, l) in cpu {
            v.cpu.add_range(s, l);
        }
        for &(s, l) in bulk {
            v.bulk.add_range(s, l);
        }
        v
    }

    fn arena(&mut self, a: Arena) -> &mut RangeAlloc {
        match a {
            Arena::Cpu => &mut self.cpu,
            Arena::Bulk => &mut self.bulk,
        }
    }

    pub fn alloc(&mut self, arena: Arena, size: u64, align: u64) -> Option<u64> {
        self.arena(arena).alloc(size, align)
    }

    pub fn free(&mut self, arena: Arena, addr: u64, size: u64) -> bool {
        self.arena(arena).free(addr, size)
    }

    /// Занято всего (GETPARAM VRAM_USED).
    pub fn used(&self) -> u64 {
        self.cpu.used() + self.bulk.used()
    }

    pub fn total(&self) -> u64 {
        self.cpu.total() + self.bulk.total()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    #[test]
    fn first_fit_align_merge() {
        let mut r = RangeAlloc::new(PAGE);
        assert!(r.add_range(0x10000, 0x10000));
        assert_eq!(r.total(), 0x10000);
        let a = r.alloc(0x1000, 0).unwrap();
        assert_eq!(a, 0x10000);
        // Выравнивание 64 КиБ: следующий подходящий адрес — 0x10000 занят,
        // 0x20000 вне куска -> нет места.
        assert_eq!(r.alloc(0x1000, 0x10000), None);
        let b = r.alloc(0x2000, 0x2000).unwrap();
        assert_eq!(b, 0x12000);
        assert_eq!(r.used(), 0x3000);
        assert!(r.free(a, 0x1000));
        assert!(!r.free(a, 0x1000), "двойное освобождение ловится");
        assert!(r.free(b, 0x2000));
        assert_eq!(r.used(), 0);
        let v: Vec<_> = r.free_ranges().collect();
        assert_eq!(v, [(0x10000, 0x20000)], "соседи слились");
    }

    #[test]
    fn multi_range_like_wp_a() {
        // Служебная арена WP-A: два окна вокруг кадров дисплея.
        let mut v = VramAlloc::with_ranges(
            &[(0x0610_0000, 0x0800_0000 - 0x0610_0000), (0x0e80_0000, 0x1000_0000 - 0x0e80_0000)],
            &[(0x1000_0000, 0x2_f06e_0000 - 0x1000_0000)],
        );
        // Первый кусок 31 МиБ: просим 30 МиБ, потом ещё 8 МиБ — должны уйти во второй.
        let a = v.alloc(Arena::Cpu, 30 << 20, PAGE).unwrap();
        assert_eq!(a, 0x0610_0000);
        let b = v.alloc(Arena::Cpu, 8 << 20, PAGE).unwrap();
        assert_eq!(b, 0x0e80_0000);
        // Кадры дисплея никогда не выдаются.
        assert!(!(0x0800_0000..0x0e80_0000).contains(&b));
        let c = v.alloc(Arena::Bulk, 1 << 30, 2 << 20).unwrap();
        assert_eq!(c, 0x1000_0000);
        assert_eq!(v.used(), (30 << 20) + (8 << 20) + (1 << 30));
        assert!(v.free(Arena::Cpu, a, 30 << 20));
        assert!(v.free(Arena::Bulk, c, 1 << 30));
    }

    #[test]
    fn alloc_at_and_overlap() {
        let mut r = RangeAlloc::new(PAGE);
        r.add_range(0, 1 << 40);
        assert!(!r.add_range(0x1000, 0x1000), "пересекающийся кусок не добавляется");
        assert!(r.alloc_at(1 << 39, 0x2000_0000));
        assert!(!r.alloc_at((1 << 39) + 0x1000, 0x1000));
        assert!(!r.is_free(1 << 39, 0x1000));
        assert!(r.is_free(0, 0x1000));
        assert!(r.free(1 << 39, 0x2000_0000));
        assert!(r.is_free(1 << 39, 0x2000_0000));
        assert_eq!(r.free_ranges().count(), 1);
    }
}
