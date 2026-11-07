// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Гибридные логические часы (HLC, Kulkarni и др., 2014).
//!
//! Метка — `(wall, count, device)`: миллисекунды настенных часов, счётчик
//! событий в пределах одной миллисекунды и id устройства. Порядок меток —
//! лексикографический по этим трём полям, поэтому он **полный и одинаковый
//! на всех устройствах**: две разные операции никогда не равны (у разных
//! устройств разный id, у одного — растущие `wall`/`count`).
//!
//! Часы не ходят назад: если системное время откатилось (сбитые часы, смена
//! пояса без UTC, ручная правка), `wall` остаётся прежним, а растёт `count`.
//! Прочитав чужую операцию, часы подтягиваются к её метке (`observe`) —
//! так любая наша следующая операция «позже» всего, что мы видели, даже если
//! наши часы отстают от чужих. На этом держится и удаление: `del`, сделанный
//! после того, как устройство увидело `put`, всегда побеждает этот `put`.

use alloc::format;
use alloc::string::String;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Hlc {
    pub wall: u64,
    pub count: u32,
    pub device: String,
}

impl Hlc {
    pub fn new(wall: u64, count: u32, device: &str) -> Self {
        Hlc { wall, count, device: String::from(device) }
    }

    /// Время и счётчик текстом: `"<wall>.<count>"` (id устройства пишется
    /// отдельным полем).
    pub fn stamp(&self) -> String {
        format!("{}.{}", self.wall, self.count)
    }

    /// Обратное к `stamp`.
    pub fn parse_stamp(stamp: &str, device: &str) -> Option<Self> {
        let (w, c) = stamp.split_once('.')?;
        if w.is_empty() || c.is_empty() || !w.bytes().chain(c.bytes()).all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Hlc { wall: w.parse().ok()?, count: c.parse().ok()?, device: String::from(device) })
    }
}

/// Часы одного устройства.
#[derive(Clone, Debug)]
pub struct Clock {
    device: String,
    wall: u64,
    count: u32,
}

impl Clock {
    pub fn new(device: &str) -> Self {
        Clock { device: String::from(device), wall: 0, count: 0 }
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// Метка новой операции. `phys_ms` — системное время в мс от эпохи Unix.
    pub fn tick(&mut self, phys_ms: u64) -> Hlc {
        if phys_ms > self.wall {
            self.wall = phys_ms;
            self.count = 0;
        } else if self.count == u32::MAX {
            // 4 миллиарда операций в одну миллисекунду не бывает, но если
            // часы стоят — пусть время чуть убежит вперёд, а не повторится.
            self.wall += 1;
            self.count = 0;
        } else {
            self.count += 1;
        }
        Hlc { wall: self.wall, count: self.count, device: self.device.clone() }
    }

    /// Учесть увиденную метку (своего журнала после перезапуска или чужого):
    /// следующая `tick` выдаст метку больше неё.
    pub fn observe(&mut self, seen: &Hlc) {
        if (seen.wall, seen.count) > (self.wall, self.count) {
            self.wall = seen.wall;
            self.count = seen.count;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_under_clock_skew() {
        let mut c = Clock::new("pc");
        let a = c.tick(1000);
        let b = c.tick(1000);
        let d = c.tick(500); // часы откатились
        let e = c.tick(2000);
        assert!(a < b && b < d && d < e);
        assert_eq!(d.wall, 1000);
        assert_eq!(e, Hlc::new(2000, 0, "pc"));
    }

    #[test]
    fn observe_moves_past_remote() {
        let mut slow = Clock::new("phone");
        let remote = Hlc::new(10_000, 7, "pc");
        slow.observe(&remote);
        let mine = slow.tick(5); // свои часы далеко позади
        assert!(mine > remote);
        // Своя прошлая метка не уводит назад.
        slow.observe(&Hlc::new(1, 0, "phone"));
        assert!(slow.tick(5) > mine);
    }

    #[test]
    fn device_breaks_ties() {
        assert!(Hlc::new(5, 0, "a") < Hlc::new(5, 0, "b"));
        assert!(Hlc::new(5, 1, "a") > Hlc::new(5, 0, "b"));
    }

    #[test]
    fn stamp_roundtrip() {
        let h = Hlc::new(1_762_560_000_123, 42, "pc-1");
        assert_eq!(Hlc::parse_stamp(&h.stamp(), "pc-1"), Some(h));
        for bad in ["", "1", "1.", ".1", "a.1", "1.-1", "1.2.3", "99999999999999999999.0"] {
            assert_eq!(Hlc::parse_stamp(bad, "x"), None, "{bad}");
        }
    }

    #[test]
    fn counter_overflow_advances_wall() {
        let mut c = Clock::new("x");
        c.observe(&Hlc::new(7, u32::MAX, "y"));
        assert_eq!(c.tick(0), Hlc::new(8, 0, "x"));
    }
}
