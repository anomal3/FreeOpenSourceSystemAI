//! Перо: ломаная толщиной `width` со стыками, концами и штрихами.
//!
//! # Почему обводка — объединение многоугольников
//!
//! Каждый отрезок пера — четырёхугольник, каждый стык и конец — свой
//! многоугольник, и все они заливаются вместе правилом ненулевой суммы.
//! Чтобы перекрытие не превратилось в дыру, у всех многоугольников одно
//! направление обхода ([`oriented`]). Контур обводки «одним куском» был бы
//! экономнее, но на самопересекающейся ломаной (восьмёрка, петля кривой)
//! такой контур выворачивается и выгрызает из линии треугольники — ровно
//! этот дефект объединение не допускает.

use alloc::vec::Vec;

use crate::geom::{Point, Polyline};
use crate::{Error, push};

/// `LineJoin`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Miter,
    Bevel,
    Round,
    MiterClipped,
}

impl Join {
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Bevel,
            2 => Self::Round,
            3 => Self::MiterClipped,
            _ => Self::Miter,
        }
    }
}

/// `LineCap`. Якоря (`ArrowAnchor` и прочие) рисуются плоским концом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    Flat,
    Square,
    Round,
    Triangle,
}

impl Cap {
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 | 0x11 => Self::Square,
            2 | 0x12 => Self::Round,
            3 => Self::Triangle,
            _ => Self::Flat,
        }
    }
}

/// Перо в координатах программы.
#[derive(Clone, Debug)]
pub struct Pen<'a> {
    pub width: f64,
    pub join: Join,
    pub start_cap: Cap,
    pub end_cap: Cap,
    pub miter_limit: f64,
    /// Длины штрихов и промежутков в толщинах пера; пусто — сплошная.
    pub dashes: &'a [f32],
    pub dash_offset: f64,
}

/// Многоугольники обводки ломаных.
pub fn stroke(figures: &[Polyline], pen: &Pen<'_>) -> Result<Vec<Vec<Point>>, Error> {
    let mut out = Vec::new();
    let half = pen.width / 2.0;
    if !(half > 0.0) || !half.is_finite() {
        return Ok(out);
    }
    for figure in figures {
        let points = dedup(&figure.points)?;
        if pen.dashes.is_empty() {
            outline(&points, figure.closed, pen, half, pen.start_cap, pen.end_cap, &mut out)?;
        } else {
            for dash in split_dashes(&points, figure.closed, pen)? {
                outline(&dash, false, pen, half, pen.start_cap, pen.end_cap, &mut out)?;
            }
        }
    }
    Ok(out)
}

/// Соседние совпадающие точки убираются: у отрезка нулевой длины нет нормали.
fn dedup(points: &[Point]) -> Result<Vec<Point>, Error> {
    let mut out: Vec<Point> = Vec::new();
    for &p in points {
        if out.last().is_some_and(|last| distance(*last, p) < 1e-9) {
            continue;
        }
        push(&mut out, p)?;
    }
    Ok(out)
}

fn distance(a: Point, b: Point) -> f64 {
    libm::hypot(b.x - a.x, b.y - a.y)
}

#[allow(clippy::too_many_arguments)]
fn outline(
    points: &[Point],
    closed: bool,
    pen: &Pen<'_>,
    half: f64,
    start_cap: Cap,
    end_cap: Cap,
    out: &mut Vec<Vec<Point>>,
) -> Result<(), Error> {
    let mut points = points;
    let mut closed = closed;
    // Замкнутая фигура, последняя точка которой совпала с первой, — та же
    // фигура без повтора; иначе замыкающий отрезок нулевой длины.
    if closed && points.len() > 2 && distance(points[0], points[points.len() - 1]) < 1e-9 {
        points = &points[..points.len() - 1];
    }
    if points.len() < 2 {
        closed = false;
    }
    if points.len() == 1 {
        // Точка: видна только с круглым или квадратным концом.
        let p = points[0];
        match start_cap {
            Cap::Round => push(out, circle(p, half)?)?,
            Cap::Square => push(out, oriented(poly(&[
                Point::new(p.x - half, p.y - half),
                Point::new(p.x + half, p.y - half),
                Point::new(p.x + half, p.y + half),
                Point::new(p.x - half, p.y + half),
            ])?))?,
            _ => {}
        }
        return Ok(());
    }
    if points.is_empty() {
        return Ok(());
    }
    let count = points.len();
    let segments = if closed { count } else { count - 1 };
    for i in 0..segments {
        let (a, b) = (points[i], points[(i + 1) % count]);
        let (nx, ny) = normal(a, b);
        let quad = poly(&[
            Point::new(a.x + nx * half, a.y + ny * half),
            Point::new(b.x + nx * half, b.y + ny * half),
            Point::new(b.x - nx * half, b.y - ny * half),
            Point::new(a.x - nx * half, a.y - ny * half),
        ])?;
        push(out, oriented(quad))?;
    }
    let joins = if closed { 0..count } else { 1..count - 1 };
    for i in joins {
        let prev = points[(i + count - 1) % count];
        let here = points[i];
        let next = points[(i + 1) % count];
        join(prev, here, next, pen, half, out)?;
    }
    if !closed {
        cap(points[1], points[0], start_cap, half, out)?;
        cap(points[count - 2], points[count - 1], end_cap, half, out)?;
    }
    Ok(())
}

/// Единичная нормаль отрезка — влево по ходу в координатах с осью y вниз.
fn normal(a: Point, b: Point) -> (f64, f64) {
    let length = distance(a, b);
    if length == 0.0 {
        return (0.0, 0.0);
    }
    (-(b.y - a.y) / length, (b.x - a.x) / length)
}

fn join(prev: Point, here: Point, next: Point, pen: &Pen<'_>, half: f64, out: &mut Vec<Vec<Point>>) -> Result<(), Error> {
    let (n1x, n1y) = normal(prev, here);
    let (n2x, n2y) = normal(here, next);
    let cross = (here.x - prev.x) * (next.y - here.y) - (here.y - prev.y) * (next.x - here.x);
    if cross.abs() < 1e-12 {
        // Продолжение по прямой или разворот: разворот закрывается кругом при
        // круглом стыке, иначе нечем — отрезки и так накрывают точку.
        if pen.join == Join::Round {
            push(out, circle(here, half)?)?;
        }
        return Ok(());
    }
    // Внешняя сторона поворота — противоположная направлению поворота.
    let side = if cross > 0.0 { -1.0 } else { 1.0 };
    let a = Point::new(here.x + n1x * half * side, here.y + n1y * half * side);
    let b = Point::new(here.x + n2x * half * side, here.y + n2y * half * side);
    match pen.join {
        Join::Round => push(out, circle(here, half)?),
        Join::Bevel => push(out, oriented(poly(&[here, a, b])?)),
        Join::Miter | Join::MiterClipped => {
            // Острие — пересечение внешних кромок; длина от точки стыка
            // `half / cos(θ/2)`, и при пределе больше `miter_limit · half`
            // стык срезается, как у GDI+.
            let (bx, by) = (n1x + n2x, n1y + n2y);
            let bisector = libm::hypot(bx, by);
            if bisector < 1e-12 {
                return push(out, oriented(poly(&[here, a, b])?));
            }
            let cos_half = bisector / 2.0;
            let length = half / cos_half;
            let limit = pen.miter_limit.max(1.0) * half;
            if length > limit {
                if pen.join == Join::MiterClipped {
                    let tip = Point::new(here.x + bx / bisector * limit * side, here.y + by / bisector * limit * side);
                    return push(out, oriented(poly(&[here, a, tip, b])?));
                }
                return push(out, oriented(poly(&[here, a, b])?));
            }
            let tip = Point::new(here.x + bx / bisector * length * side, here.y + by / bisector * length * side);
            push(out, oriented(poly(&[here, a, tip, b])?))
        }
    }
}

/// Конец в точке `end`, пришедший из `from`.
fn cap(from: Point, end: Point, cap: Cap, half: f64, out: &mut Vec<Vec<Point>>) -> Result<(), Error> {
    let length = distance(from, end);
    if length == 0.0 {
        return Ok(());
    }
    let (tx, ty) = ((end.x - from.x) / length, (end.y - from.y) / length);
    let (nx, ny) = (-ty, tx);
    match cap {
        Cap::Flat => Ok(()),
        Cap::Square => push(out, oriented(poly(&[
            Point::new(end.x + nx * half, end.y + ny * half),
            Point::new(end.x + nx * half + tx * half, end.y + ny * half + ty * half),
            Point::new(end.x - nx * half + tx * half, end.y - ny * half + ty * half),
            Point::new(end.x - nx * half, end.y - ny * half),
        ])?)),
        Cap::Round => push(out, circle(end, half)?),
        Cap::Triangle => push(out, oriented(poly(&[
            Point::new(end.x + nx * half, end.y + ny * half),
            Point::new(end.x + tx * half, end.y + ty * half),
            Point::new(end.x - nx * half, end.y - ny * half),
        ])?)),
    }
}

/// Многоугольник из точек, без паники при отказе распределителя.
fn poly(points: &[Point]) -> Result<Vec<Point>, Error> {
    let mut out = Vec::new();
    out.try_reserve_exact(points.len()).map_err(|_| Error::OutOfMemory)?;
    out.extend_from_slice(points);
    Ok(out)
}

/// Круг многоугольником: хорда отходит от окружности не больше чем на 0.05
/// единицы — в точках после преобразования это ещё мельче при уменьшении и
/// заметно только при увеличении больше чем в пять раз.
fn circle(center: Point, radius: f64) -> Result<Vec<Point>, Error> {
    let steps = if radius <= 0.05 {
        8
    } else {
        let angle = 2.0 * libm::acos((1.0 - 0.05 / radius).clamp(-1.0, 1.0));
        let n = libm::ceil(core::f64::consts::TAU / angle.max(1e-3));
        if n.is_finite() { (n as usize).clamp(8, 256) } else { 256 }
    };
    let mut points = Vec::new();
    points.try_reserve_exact(steps).map_err(|_| Error::OutOfMemory)?;
    for i in 0..steps {
        let t = core::f64::consts::TAU * i as f64 / steps as f64;
        points.push(Point::new(center.x + radius * libm::cos(t), center.y + radius * libm::sin(t)));
    }
    Ok(oriented(points))
}

/// Многоугольник с положительной площадью (обход по часовой на экране).
fn oriented(mut points: Vec<Point>) -> Vec<Point> {
    let count = points.len();
    let mut area = 0.0;
    for i in 0..count {
        let (a, b) = (points[i], points[(i + 1) % count]);
        area += a.x * b.y - b.x * a.y;
    }
    if area < 0.0 {
        points.reverse();
    }
    points
}

/// Разрезать ломаную на штрихи. Длины — в толщинах пера, как у
/// `Pen.DashPattern`; у замкнутой фигуры узор идёт и по замыкающему отрезку.
fn split_dashes(points: &[Point], closed: bool, pen: &Pen<'_>) -> Result<Vec<Vec<Point>>, Error> {
    let mut dashes = Vec::new();
    let scale = pen.width.max(1e-6);
    let pattern_total: f64 = pen.dashes.iter().map(|d| f64::from(*d).max(0.0) * scale).sum();
    if !(pattern_total > 1e-9) || points.len() < 2 {
        return Ok(dashes);
    }
    let mut path: Vec<Point> = Vec::new();
    for &p in points {
        push(&mut path, p)?;
    }
    if closed {
        push(&mut path, points[0])?;
    }
    // Где в узоре начинается ломаная.
    let mut offset = (pen.dash_offset * scale) % pattern_total;
    if offset < 0.0 {
        offset += pattern_total;
    }
    let mut index = 0;
    let mut remaining = f64::from(pen.dashes[0]).max(0.0) * scale;
    while offset > 0.0 {
        if offset >= remaining {
            offset -= remaining;
            index = (index + 1) % pen.dashes.len();
            remaining = f64::from(pen.dashes[index]).max(0.0) * scale;
        } else {
            remaining -= offset;
            offset = 0.0;
        }
    }
    let mut current: Vec<Point> = Vec::new();
    let mut drawing = index % 2 == 0;
    if drawing {
        push(&mut current, path[0])?;
    }
    let mut guard = 0usize;
    for i in 0..path.len() - 1 {
        let (mut a, b) = (path[i], path[i + 1]);
        let mut length = distance(a, b);
        while length > 0.0 {
            guard += 1;
            if guard > crate::geom::MAX_POINTS {
                return Err(Error::TooComplex);
            }
            if remaining >= length {
                remaining -= length;
                if drawing {
                    push(&mut current, b)?;
                }
                length = 0.0;
            } else {
                let t = remaining / length;
                let cut = Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
                if drawing {
                    push(&mut current, cut)?;
                    push(&mut dashes, core::mem::take(&mut current))?;
                } else {
                    push(&mut current, cut)?;
                }
                drawing = !drawing;
                length -= remaining;
                a = cut;
                index = (index + 1) % pen.dashes.len();
                remaining = f64::from(pen.dashes[index]).max(0.0) * scale;
                if !drawing {
                    current.clear();
                }
            }
        }
    }
    if drawing && current.len() > 1 {
        push(&mut dashes, current)?;
    }
    Ok(dashes)
}
