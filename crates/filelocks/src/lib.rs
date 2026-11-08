// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Таблица рекомендательных блокировок файлов (08.11.2026).
//!
//! # Зачем
//!
//! SQLite (профиль браузера на Servo: cookies, IndexedDB, история) защищает
//! базу блокировками `fcntl(F_SETLK)` на диапазонах байтов. Без них два окна,
//! два запуска браузера или два соединения одной программы пишут базу
//! вперемешку и портят её. Chromium, Java и Wine в слое Linux пользуются тем же
//! и ещё `flock(2)`. Блокировки рекомендательные: читать и писать файл они не
//! мешают, мешают только взять конфликтующую блокировку.
//!
//! # Три вида — одна таблица
//!
//! * **POSIX** (`F_GETLK`/`F_SETLK`/`F_SETLKW`): владелец — процесс
//!   ([`Owner::Process`]). Свои блокировки процесса друг другу не мешают и
//!   сливаются/режутся по диапазону; снимаются, когда процесс закрывает
//!   **любой** свой дескриптор этого файла, и при его выходе.
//! * **OFD** (`F_OFD_*`, Linux): владелец — открытый файл
//!   ([`Owner::Open`]: все копии дескриптора после `dup`/`fork`). Снимаются,
//!   когда закрыта последняя копия. С POSIX-блокировками конфликтуют — это
//!   одно пространство [`Space::Record`].
//! * **`flock(2)`**: владелец — открытый файл, весь файл целиком, своё
//!   пространство [`Space::Flock`] — с `fcntl` не пересекается (как в Linux).
//!
//! Здесь только логика. Замок вокруг таблицы, ожидание (`F_SETLKW`) и
//! ключ файла (том + номер inode) — у ядра.

#![no_std]

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// Конец диапазона «до конца файла и дальше» (длина 0 у POSIX).
pub const EOF: u64 = u64::MAX;

/// Кто держит блокировку. Числа — у ядра: адрес процесса, адрес открытого
/// файла; сравниваются только на равенство.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Owner {
    Process(usize),
    Open(usize),
}

/// Пространство: `fcntl` (POSIX и OFD) и `flock` друг другу не мешают.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Space {
    Record,
    Flock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Разделяемая (`F_RDLCK`, `LOCK_SH`): читающих может быть сколько угодно.
    Read,
    /// Исключительная (`F_WRLCK`, `LOCK_EX`).
    Write,
}

/// Одна блокировка: `[start, end)`, `end == EOF` — до конца.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lock {
    pub owner: Owner,
    pub space: Space,
    pub kind: Kind,
    pub start: u64,
    pub end: u64,
    /// Что ответить в `l_pid` у `F_GETLK`: номер процесса, у OFD — `-1`.
    pub pid: i32,
}

impl Lock {
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.start < end && start < self.end
    }
}

/// `(начало, длина)` POSIX → `[start, end)`. Длина 0 — до конца файла;
/// переполнение — `None` (`EOVERFLOW`/`EINVAL` у вызывающего).
pub fn range(start: u64, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return Some((start, EOF));
    }
    let end = start.checked_add(len)?;
    (end != EOF).then_some((start, end))
}

/// `[start, end)` → `(начало, длина)` для ответа `F_GETLK`.
pub fn to_len(start: u64, end: u64) -> (u64, u64) {
    if end == EOF { (start, 0) } else { (start, end - start) }
}

/// Блокировки по файлам. `K` — ключ файла (у ядра — том и номер inode).
pub struct Table<K> {
    files: BTreeMap<K, Vec<Lock>>,
}

impl<K: Ord + Clone> Default for Table<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Ord + Clone> Table<K> {
    pub const fn new() -> Self {
        Table { files: BTreeMap::new() }
    }

    /// Блокировки файла (для отладки и тестов).
    pub fn locks(&self, key: &K) -> &[Lock] {
        self.files.get(key).map_or(&[], Vec::as_slice)
    }

    /// Нет ни одной блокировки.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Первая чужая блокировка, мешающая взять `kind` на `[start, end)` —
    /// ответ `F_GETLK` и проверка перед `F_SETLK`.
    pub fn conflict(&self, key: &K, owner: Owner, space: Space, kind: Kind, start: u64, end: u64) -> Option<Lock> {
        self.locks(key)
            .iter()
            .find(|l| {
                l.space == space
                    && l.owner != owner
                    && l.overlaps(start, end)
                    && (kind == Kind::Write || l.kind == Kind::Write)
            })
            .copied()
    }

    /// Взять (`Some(kind)`) или снять (`None`) блокировку на `[start, end)`.
    ///
    /// Свои блокировки владельца в этом диапазоне заменяются (так «читающая»
    /// становится «пишущей» и обратно, а снятие в середине режет блокировку
    /// надвое). Чужая конфликтующая — `Err` с ней, и таблица не меняется.
    /// Снятие не отказывает никогда.
    // Аргументы — ровно поля `struct flock` плюс владелец и пространство;
    // структура-обёртка только переложила бы их ещё раз.
    #[allow(clippy::too_many_arguments)]
    pub fn set(&mut self, key: &K, owner: Owner, space: Space, kind: Option<Kind>, start: u64, end: u64, pid: i32) -> Result<(), Lock> {
        if start >= end {
            return Ok(());
        }
        if let Some(kind) = kind {
            if let Some(c) = self.conflict(key, owner, space, kind, start, end) {
                return Err(c);
            }
        }
        let old = self.files.remove(key).unwrap_or_default();
        let mut out = Vec::with_capacity(old.len() + 2);
        for l in old {
            if l.owner == owner && l.space == space && l.overlaps(start, end) {
                if l.start < start {
                    out.push(Lock { end: start, ..l });
                }
                if end < l.end {
                    out.push(Lock { start: end, ..l });
                }
            } else {
                out.push(l);
            }
        }
        if let Some(kind) = kind {
            out.push(Lock { owner, space, kind, start, end, pid });
        }
        self.store(key, out);
        Ok(())
    }

    /// Снять все блокировки владельца на файле в пространстве `space`
    /// (`None` — во всех): `close` любого дескриптора файла снимает
    /// POSIX-блокировки процесса.
    pub fn release_file(&mut self, key: &K, owner: Owner, space: Option<Space>) {
        if let Some(list) = self.files.remove(key) {
            let rest = list.into_iter().filter(|l| !(l.owner == owner && space.is_none_or(|s| l.space == s))).collect();
            self.store(key, rest);
        }
    }

    /// Снять все блокировки владельца на всех файлах: выход процесса,
    /// закрытие последней копии открытого файла. Ответ — снято ли что-нибудь
    /// (будить ли ждущих).
    pub fn release(&mut self, owner: Owner) -> bool {
        let mut changed = false;
        self.files.retain(|_, list| {
            let before = list.len();
            list.retain(|l| l.owner != owner);
            changed |= list.len() != before;
            !list.is_empty()
        });
        changed
    }

    /// Положить список файла: соседние и перекрывающиеся блокировки одного
    /// владельца, пространства и вида — склеить; пустой — убрать ключ.
    fn store(&mut self, key: &K, mut list: Vec<Lock>) {
        if list.is_empty() {
            return;
        }
        list.sort_by_key(|l| (l.owner, l.space, l.kind, l.start));
        let mut merged: Vec<Lock> = Vec::with_capacity(list.len());
        for l in list {
            if let Some(prev) = merged.last_mut() {
                if prev.owner == l.owner && prev.space == l.space && prev.kind == l.kind && l.start <= prev.end {
                    prev.end = prev.end.max(l.end);
                    continue;
                }
            }
            merged.push(l);
        }
        self.files.insert(key.clone(), merged);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const A: Owner = Owner::Process(1);
    const B: Owner = Owner::Process(2);
    const R: Space = Space::Record;

    fn spans(t: &Table<u32>, owner: Owner) -> Vec<(Kind, u64, u64)> {
        t.locks(&7).iter().filter(|l| l.owner == owner).map(|l| (l.kind, l.start, l.end)).collect()
    }

    #[test]
    fn readers_share_writer_excludes() {
        let mut t = Table::new();
        t.set(&7, A, R, Some(Kind::Read), 0, 10, 1).unwrap();
        t.set(&7, B, R, Some(Kind::Read), 5, 15, 2).unwrap();
        let c = t.set(&7, B, R, Some(Kind::Write), 0, 3, 2).unwrap_err();
        assert_eq!((c.owner, c.pid), (A, 1));
        // Не пересекается — можно.
        t.set(&7, B, R, Some(Kind::Write), 20, 30, 2).unwrap();
        // Свои не мешают: A поднимает свою до пишущей там, где нет B.
        t.set(&7, A, R, Some(Kind::Write), 0, 5, 1).unwrap();
        assert!(t.set(&7, A, R, Some(Kind::Write), 0, 6, 1).is_err());
    }

    #[test]
    fn unlock_in_the_middle_splits() {
        let mut t = Table::new();
        t.set(&7, A, R, Some(Kind::Write), 0, 100, 1).unwrap();
        t.set(&7, A, R, None, 40, 60, 1).unwrap();
        assert_eq!(spans(&t, A), vec![(Kind::Write, 0, 40), (Kind::Write, 60, 100)]);
        t.set(&7, B, R, Some(Kind::Write), 40, 60, 2).unwrap();
        // Обратно: склеивается в одну.
        t.set(&7, B, R, None, 0, EOF, 2).unwrap();
        t.set(&7, A, R, Some(Kind::Write), 40, 60, 1).unwrap();
        assert_eq!(spans(&t, A), vec![(Kind::Write, 0, 100)]);
    }

    #[test]
    fn downgrade_part_of_a_lock() {
        let mut t = Table::new();
        t.set(&7, A, R, Some(Kind::Write), 0, EOF, 1).unwrap();
        t.set(&7, A, R, Some(Kind::Read), 10, 20, 1).unwrap();
        assert_eq!(spans(&t, A), vec![(Kind::Read, 10, 20), (Kind::Write, 0, 10), (Kind::Write, 20, EOF)]);
        // Читать в [10, 20) теперь можно и другому.
        t.set(&7, B, R, Some(Kind::Read), 12, 18, 2).unwrap();
        assert!(t.set(&7, B, R, Some(Kind::Read), 19, 21, 2).is_err());
    }

    #[test]
    fn flock_and_record_are_separate_spaces() {
        let mut t = Table::new();
        let (f1, f2) = (Owner::Open(10), Owner::Open(11));
        t.set(&7, f1, Space::Flock, Some(Kind::Write), 0, EOF, -1).unwrap();
        // Второе открытие того же файла тем же процессом — другой владелец.
        assert!(t.set(&7, f2, Space::Flock, Some(Kind::Read), 0, EOF, -1).is_err());
        // fcntl с flock не конфликтует.
        t.set(&7, A, R, Some(Kind::Write), 0, EOF, 1).unwrap();
        // OFD с POSIX — конфликтует (одно пространство).
        assert_eq!(t.set(&7, f2, R, Some(Kind::Read), 0, 1, -1).unwrap_err().owner, A);
    }

    #[test]
    fn release_by_owner_and_by_file() {
        let mut t: Table<u32> = Table::new();
        t.set(&7, A, R, Some(Kind::Write), 0, 10, 1).unwrap();
        t.set(&8, A, R, Some(Kind::Write), 0, 10, 1).unwrap();
        t.set(&8, Owner::Open(5), Space::Flock, Some(Kind::Write), 0, EOF, -1).unwrap();
        t.release_file(&7, A, Some(R));
        assert!(t.locks(&7).is_empty());
        assert!(t.release(A));
        assert!(!t.release(A));
        assert_eq!(t.locks(&8).len(), 1);
        assert!(t.release(Owner::Open(5)));
        assert!(t.is_empty());
    }

    #[test]
    fn ranges() {
        assert_eq!(range(5, 0), Some((5, EOF)));
        assert_eq!(range(5, 10), Some((5, 15)));
        assert_eq!(range(u64::MAX - 1, 5), None);
        assert_eq!(to_len(5, EOF), (5, 0));
        assert_eq!(to_len(5, 15), (5, 10));
        let mut t: Table<u32> = Table::new();
        t.set(&7, A, R, Some(Kind::Write), 9, 9, 1).unwrap(); // пустой — ничего
        assert!(t.is_empty());
    }

    /// Как SQLite (os_unix.c): PENDING 0x40000000, RESERVED +1, SHARED — 510
    /// байт после них. Два процесса на одной базе.
    #[test]
    fn sqlite_two_processes() {
        const PENDING: u64 = 0x4000_0000;
        const RESERVED: u64 = PENDING + 1;
        const SHARED: u64 = PENDING + 2;
        const SHARED_END: u64 = SHARED + 510;
        let mut t = Table::new();
        let shared = |t: &mut Table<u32>, o: Owner| -> Result<(), Lock> {
            t.set(&7, o, R, Some(Kind::Read), PENDING, PENDING + 1, 0)?;
            t.set(&7, o, R, Some(Kind::Read), SHARED, SHARED_END, 0)?;
            t.set(&7, o, R, None, PENDING, PENDING + 1, 0)
        };
        shared(&mut t, A).unwrap();
        shared(&mut t, B).unwrap();
        // A — RESERVED (пишет), B его уже не возьмёт.
        t.set(&7, A, R, Some(Kind::Write), RESERVED, RESERVED + 1, 1).unwrap();
        assert!(t.set(&7, B, R, Some(Kind::Write), RESERVED, RESERVED + 1, 2).is_err());
        // A — PENDING, затем EXCLUSIVE: не выйдет, пока B читает.
        t.set(&7, A, R, Some(Kind::Write), PENDING, PENDING + 1, 1).unwrap();
        assert!(t.set(&7, A, R, Some(Kind::Write), SHARED, SHARED_END, 1).is_err());
        // Новый читатель не войдёт (PENDING держит A).
        assert!(shared(&mut t, Owner::Process(3)).is_err());
        // B закончил чтение.
        t.set(&7, B, R, None, 0, EOF, 2).unwrap();
        t.set(&7, A, R, Some(Kind::Write), SHARED, SHARED_END, 1).unwrap();
        // A записал и отпустил всё — B снова читает.
        t.set(&7, A, R, None, 0, EOF, 1).unwrap();
        shared(&mut t, B).unwrap();
    }
}
