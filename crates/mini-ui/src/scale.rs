//! Масштаб интерфейса — дробный, в процентах (20.11).
//!
//! # Зачем отдельный тип, а не `u32`
//!
//! До 20.11 масштаб был целым множителем (1 или 2), и по всему столу он
//! умножался прямо: `R_WINDOW * scale`, `scale >= 2`, `scale.clamp(1, 2)`.
//! Дробный масштаб (125/150/175 %) в том же `u32` молча ломал бы каждое такое
//! место. Отдельный тип заставляет компилятор показать их все, а умножение
//! `u32 * Scale` оставляет привычную запись и округляет само.
//!
//! # Как округляется
//!
//! До ближайшей целой точки: геометрия из макета ложится на сетку экрана, и
//! края остаются чёткими. Однопиксельная линия — исключение: её не
//! округляют вверх (1 × 150 % — это 1, а не 2 точки), она толстеет только с
//! целым множителем; иначе на 150 % каждая разделительная черта становилась
//! бы вдвое жирнее соседнего текста.

use core::fmt;
use core::ops::Mul;

/// Масштаб интерфейса в процентах: 100, 125, 150, 175, 200.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Scale(u16);

impl Scale {
    /// 100 %.
    pub const ONE: Self = Self(100);
    /// 200 %.
    pub const TWO: Self = Self(200);
    /// Ступени, которые предлагают «Параметры».
    pub const STEPS: [u32; 5] = [100, 125, 150, 175, 200];

    /// Масштаб из процентов: зажат в 100..=300 и сведён к ступени в 25 %.
    #[must_use]
    pub const fn percent(percent: u32) -> Self {
        let p = if percent < 100 {
            100
        } else if percent > 300 {
            300
        } else {
            percent
        };
        Self(((p + 12) / 25 * 25) as u16)
    }

    /// Целый множитель (1 — 100 %, 2 — 200 %).
    #[must_use]
    pub const fn whole(times: u32) -> Self {
        Self::percent(times.saturating_mul(100))
    }

    /// Проценты.
    #[must_use]
    pub const fn pct(self) -> u32 {
        self.0 as u32
    }

    /// Размер из макета в точках экрана, с округлением до целой точки.
    /// Однопиксельная линия не толстеет от дробного множителя.
    #[must_use]
    pub const fn px(self, value: u32) -> u32 {
        let p = self.0 as u64;
        if value == 1 {
            return (p / 100) as u32;
        }
        ((value as u64 * p + 50) / 100) as u32
    }

    /// То же для знаковых смещений.
    #[must_use]
    pub const fn pxi(self, value: i32) -> i32 {
        if value < 0 { -(self.px(value.unsigned_abs()) as i32) } else { self.px(value as u32) as i32 }
    }

    /// Перевести размер, посчитанный в масштабе `from`, в этот.
    #[must_use]
    pub const fn rescale(self, value: u32, from: Self) -> u32 {
        let from = from.0 as u64;
        ((value as u64 * self.0 as u64 + from / 2) / from) as u32
    }

    /// Ближайший целый множитель (125 % → 1, 150 % и 175 % → 2): для того,
    /// что бывает только в целых размерах — растровый указатель, растровый
    /// шрифт 8×8.
    #[must_use]
    pub const fn whole_part(self) -> u32 {
        (self.0 as u32 + 50) / 100
    }

    /// Масштаб не целый (125/150/175 %).
    #[must_use]
    pub const fn is_fractional(self) -> bool {
        self.0 % 100 != 0
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self::ONE
    }
}

impl Mul<Scale> for u32 {
    type Output = u32;
    fn mul(self, scale: Scale) -> u32 {
        scale.px(self)
    }
}

impl Mul<Scale> for i32 {
    type Output = i32;
    fn mul(self, scale: Scale) -> i32 {
        scale.pxi(self)
    }
}

/// Для журнала и стенда: `1`, `1.25`, `1.5`, `1.75`, `2` — целые пишутся
/// как раньше (`ui scale 2`), чтобы прежние строки журнала не менялись.
impl fmt::Display for Scale {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = u32::from(self.0);
        match p % 100 {
            0 => write!(f, "{}", p / 100),
            r if r % 10 == 0 => write!(f, "{}.{}", p / 100, r / 10),
            r => write!(f, "{}.{r:02}", p / 100),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Scale;

    #[test]
    fn rounding() {
        let s = Scale::percent(150);
        assert_eq!(32 * s, 48);
        assert_eq!(1 * s, 1);
        assert_eq!(1 * Scale::TWO, 2);
        assert_eq!(5 * Scale::percent(125), 6);
        assert_eq!(-4 * s, -6);
        assert_eq!(Scale::percent(140).pct(), 150);
        assert_eq!(Scale::percent(150).rescale(200, Scale::ONE), 300);
    }
}
