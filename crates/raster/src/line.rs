//! Тонкая линия без сглаживания — «косметическое» перо GDI+.
//!
//! Перо толщиной в точку GDI+ рисует не многоугольником, а по Брезенхэму, и
//! **оба** конца входят: `DrawLine(2, 10, 30, 10)` закрашивает столбцы 2..30,
//! а `DrawRectangle(5, 5, 10, 10)` — квадрат 11×11. Многоугольник толщиной в
//! точку потерял бы правый конец и угол прямоугольника, и рамки элементов
//! WinForms вышли бы с дырой в правом нижнем углу. Правило выбора строки
//! снято пробой: на ничьей (ровно между двумя точками) берётся верхняя.

use crate::geom::{Point, Polyline};
use crate::scan::Bounds;

/// Номер точки для координаты в пространстве растеризатора (центр точки `i`
/// — `i + 0.5`); на ничьей — меньший.
fn cell(v: f64) -> i32 {
    let value = libm::ceil(v - 1.0);
    if value.is_finite() { value.clamp(-16_777_216.0, 16_777_216.0) as i32 } else { 0 }
}

/// Пройти ломаные и назвать каждую точку линии внутри `clip` один раз: стык
/// двух отрезков не повторяется, иначе полупрозрачное перо темнело бы в
/// каждом изломе.
pub fn cosmetic(figures: &[Polyline], clip: Bounds, plot: &mut dyn FnMut(i32, i32)) {
    if clip.is_empty() {
        return;
    }
    for figure in figures {
        let count = figure.points.len();
        if count == 0 {
            continue;
        }
        if count == 1 {
            let (x, y) = (cell(figure.points[0].x), cell(figure.points[0].y));
            if x >= clip.x0 && x < clip.x1 && y >= clip.y0 && y < clip.y1 {
                plot(x, y);
            }
            continue;
        }
        let segments = if figure.closed { count } else { count - 1 };
        for i in 0..segments {
            let a = figure.points[i];
            let b = figure.points[(i + 1) % count];
            let closing = figure.closed && i + 1 == segments;
            segment(a, b, i > 0, closing, clip, plot);
        }
    }
}

/// Отрезок; `skip_start` — не повторять точку начала (её назвал предыдущий),
/// `skip_end` — точку конца (её назвал первый отрезок замкнутой фигуры).
fn segment(a: Point, b: Point, skip_start: bool, skip_end: bool, clip: Bounds, plot: &mut dyn FnMut(i32, i32)) {
    let start = (cell(a.x), cell(a.y));
    let end = (cell(b.x), cell(b.y));
    let mut emit = |x: i32, y: i32| {
        if (skip_start && (x, y) == start) || (skip_end && (x, y) == end) {
            return;
        }
        if x >= clip.x0 && x < clip.x1 && y >= clip.y0 && y < clip.y1 {
            plot(x, y);
        }
    };
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    // Проход только по той части главной оси, что внутри отсечения: отрезок
    // от -1e7 до 1e7 не должен стоить двадцати миллионов шагов.
    if libm::fabs(dx) >= libm::fabs(dy) {
        let low = start.0.min(end.0).max(clip.x0 - 1);
        let high = start.0.max(end.0).min(clip.x1);
        for x in low..=high {
            let y = if dx == 0.0 { a.y } else { a.y + (f64::from(x) + 0.5 - a.x) * dy / dx };
            let y = if x == start.0 { start.1 } else if x == end.0 { end.1 } else { cell(y) };
            emit(x, y);
        }
    } else {
        let low = start.1.min(end.1).max(clip.y0 - 1);
        let high = start.1.max(end.1).min(clip.y1);
        for y in low..=high {
            let x = a.x + (f64::from(y) + 0.5 - a.y) * dx / dy;
            let x = if y == start.1 { start.0 } else if y == end.1 { end.0 } else { cell(x) };
            emit(x, y);
        }
    }
}
