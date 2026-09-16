//! Развёртка многоугольников по строкам: покрытие каждой точки.
//!
//! # Как считается покрытие
//!
//! Со сглаживанием строка точек делится на [`SUBSAMPLES`] горизонтальных
//! проходов, и на каждом проходе отрезки «внутри фигуры» ложатся на точки
//! **точной** долей своей длины. По вертикали — шестнадцать уровней, по
//! горизонтали — непрерывно: наклонный край выходит гладким, а вертикальный,
//! стоящий на половине точки, — ровно половиной (так его рисует и GDI+).
//!
//! Без сглаживания проход один, через центры точек, и точка закрашена, если её
//! центр внутри: левый и верхний край входят, правый и нижний — нет. Это то
//! правило, по которому `FillRectangle(0, 0, 10, 10)` закрашивает ровно сто
//! точек, а два прямоугольника встык не перекрываются и не оставляют щели.

use alloc::vec::Vec;

use crate::geom::Point;
use crate::{Error, filled, push};

/// Правило заливки: `FillMode.Alternate` и `FillMode.Winding`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillRule {
    /// Внутри — нечётное число пересечений: вложенная фигура становится дырой.
    EvenOdd,
    /// Внутри — ненулевая сумма направлений: вложенная того же направления
    /// заливается.
    NonZero,
}

/// Прямоугольник точек `[x0, x1) × [y0, y1)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Bounds {
    #[must_use]
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    #[must_use]
    pub fn intersect(&self, other: &Self) -> Self {
        Self::new(self.x0.max(other.x0), self.y0.max(other.y0), self.x1.min(other.x1), self.y1.min(other.y1))
    }

    #[must_use]
    pub const fn width(&self) -> usize {
        if self.x1 > self.x0 { (self.x1 - self.x0) as usize } else { 0 }
    }

    #[must_use]
    pub const fn height(&self) -> usize {
        if self.y1 > self.y0 { (self.y1 - self.y0) as usize } else { 0 }
    }
}

/// Проходов на строку со сглаживанием.
pub const SUBSAMPLES: usize = 16;

struct Edge {
    x_top: f64,
    slope: f64,
    y_top: f64,
    y_bottom: f64,
    direction: i32,
}

/// Развернуть многоугольники и отдать покрытие построчно: `sink(y, x, row)`,
/// где `row[i]` — покрытие точки `(x + i, y)` от 0 до 255. Строки без
/// покрытия не отдаются, края строки обрезаны до первой и последней ненулевой.
///
/// Многоугольники замкнуты неявно: последняя точка соединяется с первой.
pub fn rasterize(
    polygons: &[Vec<Point>],
    rule: FillRule,
    antialias: bool,
    clip: Bounds,
    sink: &mut dyn FnMut(i32, i32, &[u8]),
) -> Result<(), Error> {
    let mut edges = Vec::new();
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for polygon in polygons {
        let count = polygon.len();
        if count < 2 {
            continue;
        }
        for i in 0..count {
            let (a, b) = (polygon[i], polygon[(i + 1) % count]);
            min_x = min_x.min(a.x);
            max_x = max_x.max(a.x);
            min_y = min_y.min(a.y);
            max_y = max_y.max(a.y);
            if a.y == b.y {
                continue;
            }
            let (top, bottom, direction) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            let slope = (bottom.x - top.x) / (bottom.y - top.y);
            push(&mut edges, Edge { x_top: top.x, slope, y_top: top.y, y_bottom: bottom.y, direction })?;
        }
    }
    if edges.is_empty() {
        return Ok(());
    }
    let area = Bounds::new(
        floor_i32(min_x) - 1,
        floor_i32(min_y) - 1,
        floor_i32(max_x) + 2,
        floor_i32(max_y) + 2,
    )
    .intersect(&clip);
    if area.is_empty() {
        return Ok(());
    }
    edges.sort_unstable_by(|a, b| a.y_top.partial_cmp(&b.y_top).unwrap_or(core::cmp::Ordering::Equal));

    let width = area.width();
    // Покрытие долями точки и разности «полных» отрезков: отрезок во всю
    // ширину окна стоит две записи, а не восемьсот.
    let mut cover: Vec<f32> = filled(width + 2, 0.0)?;
    let mut run: Vec<f32> = filled(width + 2, 0.0)?;
    let mut row: Vec<u8> = filled(width, 0)?;
    let mut active: Vec<usize> = Vec::new();
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    active.try_reserve(edges.len()).map_err(|_| Error::OutOfMemory)?;
    crossings.try_reserve(edges.len()).map_err(|_| Error::OutOfMemory)?;
    let mut next = 0;
    let (samples, weight) = if antialias { (SUBSAMPLES, 1.0 / SUBSAMPLES as f32) } else { (1, 1.0) };
    let left = f64::from(area.x0);
    let right = f64::from(area.x1);

    for y in area.y0..area.y1 {
        let row_top = f64::from(y);
        while next < edges.len() && edges[next].y_top < row_top + 1.0 {
            active.push(next);
            next += 1;
        }
        active.retain(|&index| edges[index].y_bottom > row_top);
        if active.is_empty() {
            if next >= edges.len() {
                break;
            }
            continue;
        }
        let mut touched = false;
        for sample in 0..samples {
            let sy = if antialias { row_top + (sample as f64 + 0.5) / samples as f64 } else { row_top + 0.5 };
            crossings.clear();
            for &index in &active {
                let edge = &edges[index];
                if edge.y_top <= sy && sy < edge.y_bottom {
                    crossings.push((edge.x_top + (sy - edge.y_top) * edge.slope, edge.direction));
                }
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
            let mut winding = 0;
            let mut start = 0.0;
            for &(x, direction) in &crossings {
                let was_inside = inside(winding, rule);
                winding += direction;
                let now_inside = inside(winding, rule);
                if !was_inside && now_inside {
                    start = x;
                } else if was_inside && !now_inside {
                    let (xa, xb) = (start.max(left), x.min(right));
                    if xb > xa {
                        touched = true;
                        if antialias {
                            add_span(&mut cover, &mut run, xa - left, xb - left, weight);
                        } else {
                            // Центр точки `i` — `i + 0.5`; внутри, если xa ≤ центр < xb.
                            let first = libm::ceil(xa - 0.5).max(left) - left;
                            let last = libm::ceil(xb - 0.5).min(right) - left;
                            if last > first {
                                run[first as usize] += 1.0;
                                run[last as usize] -= 1.0;
                            }
                        }
                    }
                }
            }
        }
        if !touched {
            continue;
        }
        let mut sum = 0.0f32;
        let (mut first, mut last) = (usize::MAX, 0);
        for i in 0..width {
            sum += run[i];
            let value = (cover[i] + sum).clamp(0.0, 1.0);
            // Половина покрытия — 128, а не 127: так смешивает GDI+ (белое под
            // красным краем на половине точки даёт зелёный канал 127).
            let level = libm::floor(f64::from(value) * 255.0 + 0.5) as u8;
            row[i] = level;
            if level != 0 {
                first = first.min(i);
                last = i;
            }
            cover[i] = 0.0;
            run[i] = 0.0;
        }
        for slot in cover.iter_mut().skip(width).chain(run.iter_mut().skip(width)) {
            *slot = 0.0;
        }
        if first != usize::MAX {
            sink(y, area.x0 + first as i32, &row[first..=last]);
        }
    }
    Ok(())
}

const fn inside(winding: i32, rule: FillRule) -> bool {
    match rule {
        FillRule::EvenOdd => winding & 1 != 0,
        FillRule::NonZero => winding != 0,
    }
}

/// Отрезок `[xa, xb)` с весом прохода: доли у концов, полные точки между
/// ними — разностью.
fn add_span(cover: &mut [f32], run: &mut [f32], xa: f64, xb: f64, weight: f32) {
    let ia = libm::floor(xa) as usize;
    let ib = libm::floor(xb) as usize;
    if ia == ib {
        cover[ia] += (xb - xa) as f32 * weight;
        return;
    }
    cover[ia] += (ia as f64 + 1.0 - xa) as f32 * weight;
    run[ia + 1] += weight;
    run[ib] -= weight;
    cover[ib] += (xb - ib as f64) as f32 * weight;
}

fn floor_i32(value: f64) -> i32 {
    let value = libm::floor(value);
    if value < f64::from(i32::MIN / 2) {
        i32::MIN / 2
    } else if value > f64::from(i32::MAX / 2) {
        i32::MAX / 2
    } else {
        value as i32
    }
}

/// Внутри ли точка `(x, y)` многоугольников — по тому же правилу, что
/// растеризация без сглаживания: левый и верхний край входят.
#[must_use]
pub fn contains(polygons: &[Vec<Point>], rule: FillRule, x: f64, y: f64) -> bool {
    let mut crossings: Vec<(f64, i32)> = Vec::new();
    for polygon in polygons {
        let count = polygon.len();
        for i in 0..count {
            let (a, b) = (polygon[i], polygon[(i + 1) % count]);
            if a.y == b.y {
                continue;
            }
            let (top, bottom, direction) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            if top.y <= y && y < bottom.y {
                let at = top.x + (y - top.y) * (bottom.x - top.x) / (bottom.y - top.y);
                if push(&mut crossings, (at, direction)).is_err() {
                    return false;
                }
            }
        }
    }
    crossings.sort_unstable_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(core::cmp::Ordering::Equal));
    let mut winding = 0;
    for &(at, direction) in &crossings {
        if at > x {
            break;
        }
        winding += direction;
    }
    inside(winding, rule)
}
