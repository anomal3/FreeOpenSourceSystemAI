// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Синхрообъекты DRM (`drm_syncobj`): двоичные и временные шкалы, без
//! ожидания и блокировок — только учёт. Спать, будить и держать замки —
//! дело слоя Linux (WP-C): он зовёт [`wait_poll`] в цикле, пока не
//! `Ready`, не истекло время или не пришёл сигнал.
//!
//! # Модель
//!
//! Забор ([`Fence`]) — набор пар «контекст, номер»: контекст — канал
//! (или что угодно, что умеет ответить «номер N выполнен»), номер — seq из
//! [`GpuBackend::chan_submit`](crate::GpuBackend::chan_submit). Пустой набор
//! — забор уже сигнален. Проверка — функция `done(ctx, seq)` от вызывающего;
//! `Err` (канал убит) считается выполненным, как в Linux: ожидание
//! заканчивается, ошибку NVK узнаёт пустым EXEC.
//!
//! Временная шкала — точки `номер -> забор`; точка `p` сигнальна, когда
//! сигнальны все точки до первой, не меньшей `p` (цепочка `dma_fence_chain`).
//! Точка 0 — «последний забор» (так двоичный объект и живёт).

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::Errno;
use crate::uapi::{DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE, DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT};

/// Ошибка истечения времени у ожиданий синхрообъектов (`-ETIME`).
pub const ETIME: Errno = Errno(62);

/// Забор: что должно выполниться.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Fence {
    /// `(контекст, номер)`; пусто — уже сигнален.
    pub deps: Vec<(u64, u64)>,
}

impl Fence {
    /// Уже сигнальный.
    pub fn signaled() -> Fence {
        Fence { deps: Vec::new() }
    }

    /// Забор канала `ctx` с номером `seq`.
    pub fn seq(ctx: u64, seq: u64) -> Fence {
        Fence { deps: alloc::vec![(ctx, seq)] }
    }

    /// Выполнен ли (ошибка канала — тоже «выполнен»).
    pub fn is_signaled(&self, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) -> bool {
        self.deps.iter().all(|&(c, s)| done(c, s).unwrap_or(true))
    }

    /// Выбросить выполненное.
    pub fn prune(&mut self, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) {
        self.deps.retain(|&(c, s)| !done(c, s).unwrap_or(true));
    }

    /// Слить с другим (ждать оба); по каждому контексту остаётся больший
    /// номер.
    pub fn merge(&mut self, other: &Fence) {
        for &(c, s) in &other.deps {
            match self.deps.iter_mut().find(|d| d.0 == c) {
                Some(d) => d.1 = d.1.max(s),
                None => self.deps.push((c, s)),
            }
        }
    }
}

/// Один синхрообъект.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Syncobj {
    /// Точки временной шкалы.
    points: BTreeMap<u64, Fence>,
    /// Все точки `<= base` уже сигнальны и выброшены.
    base: u64,
    /// Последний забор (точка 0; у двоичного — единственный). `None` — нет
    /// забора («не отправлено»).
    last: Option<Fence>,
}

impl Syncobj {
    /// `DRM_IOCTL_SYNCOBJ_CREATE`; `signaled` — флаг CREATE_SIGNALED.
    pub fn new(signaled: bool) -> Syncobj {
        Syncobj { points: BTreeMap::new(), base: 0, last: signaled.then(Fence::signaled) }
    }

    /// Заменить забор (двоичный сигнал EXEC/VM_BIND, SIGNAL, импорт
    /// sync_file). Временная шкала при этом не меняется — как
    /// `drm_syncobj_replace_fence`.
    pub fn replace(&mut self, f: Option<Fence>) {
        self.last = f;
    }

    /// `DRM_IOCTL_SYNCOBJ_RESET`: забора больше нет, шкала пуста.
    pub fn reset(&mut self) {
        self.points.clear();
        self.base = 0;
        self.last = None;
    }

    /// Добавить точку (сигнал временной шкалы из EXEC/VM_BIND, TIMELINE_SIGNAL).
    /// Точка не больше уже известной — как и Linux, принимаем (он ругается в
    /// журнал); ожидания считают по порядку номеров.
    pub fn add_point(&mut self, point: u64, f: Fence) {
        if point == 0 {
            self.replace(Some(f));
            return;
        }
        match self.points.get_mut(&point) {
            Some(old) => old.merge(&f),
            None => {
                self.points.insert(point, f.clone());
            }
        }
        if point <= self.base {
            // Точка «в прошлом»: ждать её опять надо.
            self.base = self.base.min(point - 1);
        }
        self.last = Some(f);
    }

    /// Самая большая отправленная точка (`QUERY` с LAST_SUBMITTED).
    pub fn last_submitted(&self) -> u64 {
        self.points.keys().next_back().copied().unwrap_or(0).max(self.base)
    }

    /// Выбросить сигнальный префикс; вернуть сигнальное значение шкалы
    /// (`QUERY` без флагов).
    pub fn collect(&mut self, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) -> u64 {
        while let Some((&p, f)) = self.points.iter().next() {
            if !f.is_signaled(done) {
                break;
            }
            self.points.remove(&p);
            self.base = self.base.max(p);
        }
        if let Some(f) = &mut self.last {
            f.prune(done);
        }
        self.base
    }

    /// Значение шкалы без изменения объекта.
    pub fn query(&self, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) -> u64 {
        let mut v = self.base;
        for (&p, f) in &self.points {
            if !f.is_signaled(done) {
                break;
            }
            v = p;
        }
        v
    }

    /// Забор, который надо ждать для точки `point` (0 — последний забор).
    /// `None` — точка ещё не отправлена.
    pub fn fence_for(&self, point: u64) -> Option<Fence> {
        if point == 0 {
            return self.last.clone();
        }
        if point <= self.base {
            return Some(Fence::signaled());
        }
        // Первая точка >= point; ждать её и все до неё.
        let (&end, _) = self.points.range(point..).next()?;
        let mut f = Fence::signaled();
        for (_, pf) in self.points.range(..=end) {
            f.merge(pf);
        }
        Some(f)
    }

    /// `DRM_IOCTL_SYNCOBJ_TRANSFER`: забор точки `src_point` источника
    /// становится точкой `dst_point` (0 — заменить забор) здесь.
    pub fn transfer_from(&mut self, src: &Syncobj, src_point: u64, dst_point: u64) -> Result<(), Errno> {
        let f = src.fence_for(src_point).ok_or(Errno::EINVAL)?;
        if dst_point == 0 {
            self.replace(Some(f));
        } else {
            self.add_point(dst_point, f);
        }
        Ok(())
    }
}

/// Состояние одного объекта в ожидании.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointState {
    /// Забора нет (не отправлено).
    NotSubmitted,
    /// Отправлено, ещё не выполнено.
    Pending,
    Signaled,
}

pub fn point_state(s: &Syncobj, point: u64, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) -> PointState {
    match s.fence_for(point) {
        None => PointState::NotSubmitted,
        Some(f) if f.is_signaled(done) => PointState::Signaled,
        Some(_) => PointState::Pending,
    }
}

/// Итог одного опроса ожидания.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitPoll {
    /// Готово; `first` — индекс первого готового (для ANY, `first_signaled`).
    Ready { first: u32 },
    /// Ждать дальше (или `-ETIME`, если время вышло/нулевой таймаут).
    Pending,
}

/// Один опрос `SYNCOBJ_WAIT` / `SYNCOBJ_TIMELINE_WAIT` (Linux
/// `drm_syncobj_array_wait_timeout`). `objs[i]` с точкой `points[i]` (у
/// двоичного WAIT — 0). Флаги — `DRM_SYNCOBJ_WAIT_FLAGS_*`:
/// * без WAIT_FOR_SUBMIT: неотправленный объект — сразу `EINVAL`;
/// * WAIT_AVAILABLE: достаточно, чтобы забор был отправлен;
/// * WAIT_ALL: все, иначе — любой.
pub fn wait_poll(objs: &[&Syncobj], points: &[u64], flags: u32, done: &dyn Fn(u64, u64) -> Result<bool, Errno>) -> Result<WaitPoll, Errno> {
    if objs.is_empty() || objs.len() != points.len() {
        return Err(Errno::EINVAL);
    }
    let all = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL != 0;
    let for_submit = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT != 0;
    let available = flags & DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE != 0;
    let mut first = None;
    let mut all_ready = true;
    for (i, (s, &p)) in objs.iter().zip(points).enumerate() {
        let ready = match point_state(s, p, done) {
            PointState::NotSubmitted if !for_submit => return Err(Errno::EINVAL),
            PointState::NotSubmitted => false,
            PointState::Pending => available,
            PointState::Signaled => true,
        };
        if ready && first.is_none() {
            first = Some(i as u32);
        }
        all_ready &= ready;
    }
    Ok(match (all, first) {
        (true, _) if all_ready => WaitPoll::Ready { first: 0 },
        (false, Some(i)) => WaitPoll::Ready { first: i },
        _ => WaitPoll::Pending,
    })
}

/// Таблица ручек (синхрообъекты, GEM): номера с 1, ноль — «нет ручки».
#[derive(Clone, Debug)]
pub struct HandleTable<T> {
    map: BTreeMap<u32, T>,
    next: u32,
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        HandleTable { map: BTreeMap::new(), next: 1 }
    }
}

impl<T> HandleTable<T> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, v: T) -> u32 {
        // Номера не переиспользуются, пока не обернутся через 2^32.
        loop {
            let h = self.next;
            self.next = self.next.wrapping_add(1).max(1);
            if let alloc::collections::btree_map::Entry::Vacant(e) = self.map.entry(h) {
                e.insert(v);
                return h;
            }
        }
    }

    pub fn get(&self, h: u32) -> Option<&T> {
        self.map.get(&h)
    }
    pub fn get_mut(&mut self, h: u32) -> Option<&mut T> {
        self.map.get_mut(&h)
    }
    pub fn remove(&mut self, h: u32) -> Option<T> {
        self.map.remove(&h)
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.map.iter().map(|(&h, v)| (h, v))
    }
    /// Забрать всё (закрытие файла).
    pub fn drain(&mut self) -> Vec<(u32, T)> {
        core::mem::take(&mut self.map).into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::Cell;

    use crate::uapi::DRM_SYNCOBJ_WAIT_FLAGS_WAIT_ALL as ALL;
    use crate::uapi::DRM_SYNCOBJ_WAIT_FLAGS_WAIT_AVAILABLE as AVAIL;
    use crate::uapi::DRM_SYNCOBJ_WAIT_FLAGS_WAIT_FOR_SUBMIT as SUBMIT;

    /// Канал 1 выполнил всё до `done1`; канал 2 убит.
    struct Gpu {
        done1: Cell<u64>,
    }
    impl Gpu {
        fn done(&self) -> impl Fn(u64, u64) -> Result<bool, Errno> + '_ {
            move |c, s| match c {
                1 => Ok(s <= self.done1.get()),
                _ => Err(Errno::ENODEV),
            }
        }
    }

    #[test]
    fn binary() {
        let g = Gpu { done1: Cell::new(0) };
        let d = g.done();
        let mut s = Syncobj::new(false);
        assert_eq!(point_state(&s, 0, &d), PointState::NotSubmitted);
        assert_eq!(wait_poll(&[&s], &[0], 0, &d), Err(Errno::EINVAL));
        assert_eq!(wait_poll(&[&s], &[0], SUBMIT, &d), Ok(WaitPoll::Pending));
        s.replace(Some(Fence::seq(1, 5)));
        assert_eq!(wait_poll(&[&s], &[0], 0, &d), Ok(WaitPoll::Pending));
        assert_eq!(wait_poll(&[&s], &[0], AVAIL, &d), Ok(WaitPoll::Ready { first: 0 }));
        g.done1.set(5);
        assert_eq!(wait_poll(&[&s], &[0], 0, &d), Ok(WaitPoll::Ready { first: 0 }));
        s.reset();
        assert_eq!(point_state(&s, 0, &d), PointState::NotSubmitted);
        let s = Syncobj::new(true);
        assert_eq!(point_state(&s, 0, &d), PointState::Signaled);
        // Убитый канал — «выполнено».
        let mut k = Syncobj::new(false);
        k.replace(Some(Fence::seq(2, 100)));
        assert_eq!(point_state(&k, 0, &d), PointState::Signaled);
    }

    #[test]
    fn timeline_chain() {
        let g = Gpu { done1: Cell::new(0) };
        let d = g.done();
        let mut t = Syncobj::new(false);
        t.add_point(1, Fence::seq(1, 10));
        t.add_point(3, Fence::seq(1, 11));
        t.add_point(5, Fence::seq(1, 12));
        assert_eq!(t.last_submitted(), 5);
        assert_eq!(t.query(&d), 0);
        // Точка 2 «живёт» в точке 3.
        assert_eq!(point_state(&t, 2, &d), PointState::Pending);
        assert_eq!(point_state(&t, 6, &d), PointState::NotSubmitted);
        g.done1.set(11);
        assert_eq!(point_state(&t, 2, &d), PointState::Signaled);
        assert_eq!(point_state(&t, 3, &d), PointState::Signaled);
        assert_eq!(point_state(&t, 4, &d), PointState::Pending);
        assert_eq!(t.query(&d), 3);
        assert_eq!(t.collect(&d), 3);
        assert_eq!(point_state(&t, 1, &d), PointState::Signaled, "выброшенные точки сигнальны");
        // TIMELINE_SIGNAL с CPU.
        t.add_point(7, Fence::signaled());
        assert_eq!(t.last_submitted(), 7);
        assert_eq!(point_state(&t, 7, &d), PointState::Pending, "ждёт точку 5 перед собой");
        g.done1.set(12);
        assert_eq!(t.collect(&d), 7);
        // Точка 0 у шкалы — последний забор.
        assert_eq!(point_state(&t, 0, &d), PointState::Signaled);
    }

    #[test]
    fn wait_all_any() {
        let g = Gpu { done1: Cell::new(1) };
        let d = g.done();
        let mut a = Syncobj::new(false);
        let mut b = Syncobj::new(false);
        a.add_point(1, Fence::seq(1, 2));
        b.add_point(4, Fence::seq(1, 1));
        let r = wait_poll(&[&a, &b], &[1, 4], 0, &d).unwrap();
        assert_eq!(r, WaitPoll::Ready { first: 1 });
        assert_eq!(wait_poll(&[&a, &b], &[1, 4], ALL, &d).unwrap(), WaitPoll::Pending);
        // Неотправленная точка без WAIT_FOR_SUBMIT — EINVAL даже при ANY.
        assert_eq!(wait_poll(&[&a, &b], &[1, 9], 0, &d), Err(Errno::EINVAL));
        assert_eq!(wait_poll(&[&a, &b], &[1, 9], SUBMIT, &d).unwrap(), WaitPoll::Pending);
        g.done1.set(2);
        assert_eq!(wait_poll(&[&a, &b], &[1, 4], ALL, &d).unwrap(), WaitPoll::Ready { first: 0 });
        assert_eq!(wait_poll(&[], &[], 0, &d), Err(Errno::EINVAL));
    }

    #[test]
    fn transfer() {
        let g = Gpu { done1: Cell::new(0) };
        let d = g.done();
        let mut src = Syncobj::new(false);
        src.add_point(2, Fence::seq(1, 3));
        src.add_point(4, Fence::seq(1, 4));
        let mut dst = Syncobj::new(false);
        dst.transfer_from(&src, 3, 0).unwrap();
        assert_eq!(dst.fence_for(0).unwrap().deps, [(1, 4)], "точка 3 = цепочка до 4");
        assert_eq!(dst.transfer_from(&src, 9, 1), Err(Errno::EINVAL));
        dst.transfer_from(&src, 2, 10).unwrap();
        g.done1.set(3);
        assert_eq!(point_state(&dst, 10, &d), PointState::Signaled);
        assert_eq!(point_state(&dst, 0, &d), PointState::Signaled, "последний — перенесённая точка 2");
    }

    #[test]
    fn handles() {
        let mut t = HandleTable::new();
        let a = t.insert('a');
        let b = t.insert('b');
        assert_eq!((a, b), (1, 2));
        assert_eq!(t.remove(a), Some('a'));
        assert_eq!(t.insert('c'), 3, "номера не переиспользуются сразу");
        assert_eq!(t.get(a), None);
        assert_eq!(t.drain().len(), 2);
        assert!(t.is_empty());
    }
}
