//! Значки набора FreeOS Icons 1.0 из макета Романа (0.8).
//!
//! # Чем отличается от [`crate::glyphicon`]
//!
//! Там значки описаны штрихами руками, в сетке 16×16. Здесь — 68 значков макета
//! как они есть: `cargo xtask icons` раскладывает их SVG (дуги, кривые Безье,
//! скруглённые прямоугольники) в ломаные, и таблица лежит в
//! [`crate::iconset_data`]. Значок совпадает с макетом, а не похож на него.
//!
//! # Как рисуется
//!
//! Каждый отрезок ломаной — «капсула»: всё, что ближе половины толщины пера к
//! отрезку. Скруглённые концы и углы макета (`stroke-linecap/linejoin:
//! round`) получаются сами, а отрезок нулевой длины — это точка (так в макете
//! нарисованы точки на клавиатуре и «три точки»). Покрытие точки — по
//! расстоянию до ближайшего отрезка, с полупиксельным сглаживанием; отрезки
//! складываются **наибольшим** покрытием, а не наложением, иначе стыки
//! ломаной темнели бы пятнами. Плавающей точки нет: ядро работает без
//! FPU-контекста, всё — в 1/64 точки.
//!
//! Толщина — 1,75 клетки из 24, как в макете, но не тоньше точки: значок на
//! 12 точек иначе стал бы пунктиром.

use alloc::vec;

use crate::draw::blend_pixel;
use crate::iconset_data::{ICONS, POINTS, STROKES};
use crate::{Color, Surface};

pub use crate::iconset_data::Fi;

/// Субпиксель: 1/64 точки.
const SUB: i64 = 64;

/// Нарисовать значок стороной `size` точек с левым верхним углом в `(x, y)`.
pub fn draw(surface: &mut Surface, icon: Fi, x: i32, y: i32, size: u32, color: Color, alpha: u8) {
    if size == 0 || size > 512 || alpha == 0 {
        return;
    }
    let side = size as usize;
    let size = i64::from(size);
    // Координата таблицы (1/16 клетки, квадрат 384) → 1/64 точки.
    let scale = |v: i16| i64::from(v) * size * SUB / 384;
    // Половина толщины пера: 1,75/24 стороны пополам, не меньше полуточки.
    let half = (size * SUB * 175 / 4800).max(SUB / 2);

    let mut cover = vec![0u8; side * side];
    let (first, count) = ICONS[icon as usize];
    for stroke in &STROKES[usize::from(first)..usize::from(first) + usize::from(count)] {
        let (start, len) = (usize::from(stroke.0), usize::from(stroke.1));
        let points = &POINTS[start..start + len];
        if points.len() == 1 {
            let p = (scale(points[0].0), scale(points[0].1));
            capsule(&mut cover, side, p, p, half);
        }
        for pair in points.windows(2) {
            let a = (scale(pair[0].0), scale(pair[0].1));
            let b = (scale(pair[1].0), scale(pair[1].1));
            capsule(&mut cover, side, a, b, half);
        }
    }

    for row in 0..side {
        for col in 0..side {
            let c = cover[row * side + col];
            if c != 0 {
                let a = (u32::from(c) * u32::from(alpha) / 255) as u8;
                blend_pixel(surface, x + col as i32, y + row as i32, color, a);
            }
        }
    }
}

/// Покрытие капсулы отрезка `a–b` радиусом `half` (всё в 1/64 точки) —
/// наибольшим в буфер `cover` стороной `side`.
fn capsule(cover: &mut [u8], side: usize, a: (i64, i64), b: (i64, i64), half: i64) {
    let reach = half + SUB;
    let x0 = ((a.0.min(b.0) - reach) / SUB).max(0);
    let y0 = ((a.1.min(b.1) - reach) / SUB).max(0);
    let x1 = ((a.0.max(b.0) + reach) / SUB + 1).min(side as i64);
    let y1 = ((a.1.max(b.1) + reach) / SUB + 1).min(side as i64);
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length2 = dx * dx + dy * dy;
    for py in y0..y1 {
        for px in x0..x1 {
            // Центр точки.
            let (cx, cy) = (px * SUB + SUB / 2, py * SUB + SUB / 2);
            let (ax, ay) = (cx - a.0, cy - a.1);
            // Ближайшая точка отрезка: доля t в 1/4096, зажатая в [0, 1].
            let t = if length2 == 0 { 0 } else { ((ax * dx + ay * dy) * 4096 / length2).clamp(0, 4096) };
            let (qx, qy) = (a.0 + dx * t / 4096, a.1 + dy * t / 4096);
            let distance = isqrt((cx - qx) * (cx - qx) + (cy - qy) * (cy - qy));
            // Полоса сглаживания — одна точка, по полточки с каждой стороны края.
            let level = ((half + SUB / 2 - distance) * 255 / SUB).clamp(0, 255) as u8;
            let at = py as usize * side + px as usize;
            if level > cover[at] {
                cover[at] = level;
            }
        }
    }
}

/// Целый квадратный корень (с округлением вниз).
fn isqrt(value: i64) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut x = value;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + value / x) / 2;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_matches() {
        for v in [0i64, 1, 2, 3, 4, 15, 16, 17, 1_000_000, 123_456_789] {
            let r = isqrt(v);
            assert!(r * r <= v && (r + 1) * (r + 1) > v, "{v}");
        }
    }

    #[test]
    fn every_icon_has_strokes() {
        for (index, (_, count)) in ICONS.iter().enumerate() {
            assert!(*count > 0, "значок {index} без штрихов");
        }
    }
}
