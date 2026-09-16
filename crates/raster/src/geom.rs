//! Геометрия: матрица 2×3 GDI+ и перевод пути в ломаные.

use alloc::vec::Vec;

use crate::{Error, push};

/// Точка в пространстве растеризатора.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[must_use]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
}

/// Аффинное преобразование в порядке элементов `Matrix.Elements` у .NET:
/// `m11 m12 m21 m22 dx dy`, точка — строка слева: `x' = x·m11 + y·m21 + dx`.
///
/// Считается в `f64`, хотя .NET хранит `float`: погрешность одинарной
/// точности на повороте набегает до сотых точки на краю окна, и сглаженный
/// край дрожал бы от кадра к кадру при одной и той же фигуре.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix {
    pub m11: f64,
    pub m12: f64,
    pub m21: f64,
    pub m22: f64,
    pub dx: f64,
    pub dy: f64,
}

impl Matrix {
    pub const IDENTITY: Self = Self { m11: 1.0, m12: 0.0, m21: 0.0, m22: 1.0, dx: 0.0, dy: 0.0 };

    /// Из шести элементов `Matrix.Elements`. `None` — элементов не шесть или
    /// среди них бесконечность: такой матрицей нечего рисовать.
    #[must_use]
    pub fn from_elements(elements: &[f32]) -> Option<Self> {
        let [m11, m12, m21, m22, dx, dy] = <[f32; 6]>::try_from(elements.get(..6)?).ok()?;
        let matrix = Self {
            m11: f64::from(m11),
            m12: f64::from(m12),
            m21: f64::from(m21),
            m22: f64::from(m22),
            dx: f64::from(dx),
            dy: f64::from(dy),
        };
        [m11, m12, m21, m22, dx, dy].iter().all(|v| v.is_finite()).then_some(matrix)
    }

    #[must_use]
    pub fn apply(&self, p: Point) -> Point {
        Point::new(p.x * self.m11 + p.y * self.m21 + self.dx, p.x * self.m12 + p.y * self.m22 + self.dy)
    }

    /// Сначала `self`, потом `then`.
    #[must_use]
    pub fn then(&self, then: &Self) -> Self {
        Self {
            m11: self.m11 * then.m11 + self.m12 * then.m21,
            m12: self.m11 * then.m12 + self.m12 * then.m22,
            m21: self.m21 * then.m11 + self.m22 * then.m21,
            m22: self.m21 * then.m12 + self.m22 * then.m22,
            dx: self.dx * then.m11 + self.dy * then.m21 + then.dx,
            dy: self.dx * then.m12 + self.dy * then.m22 + then.dy,
        }
    }

    /// Сдвиг после преобразования.
    #[must_use]
    pub fn translated(&self, dx: f64, dy: f64) -> Self {
        Self { dx: self.dx + dx, dy: self.dy + dy, ..*self }
    }

    #[must_use]
    pub fn invert(&self) -> Option<Self> {
        let det = self.m11 * self.m22 - self.m12 * self.m21;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let m11 = self.m22 / det;
        let m12 = -self.m12 / det;
        let m21 = -self.m21 / det;
        let m22 = self.m11 / det;
        Some(Self {
            m11,
            m12,
            m21,
            m22,
            dx: -(self.dx * m11 + self.dy * m21),
            dy: -(self.dx * m12 + self.dy * m22),
        })
    }

    /// Во сколько раз преобразование самое большее растягивает отрезок —
    /// оценка сверху по столбцам. Нужна, чтобы кривая, развёрнутая в мировых
    /// координатах, не распалась на заметные глазу хорды после увеличения.
    #[must_use]
    pub fn scale(&self) -> f64 {
        let a = libm::sqrt(self.m11 * self.m11 + self.m12 * self.m12);
        let b = libm::sqrt(self.m21 * self.m21 + self.m22 * self.m22);
        a.max(b)
    }
}

/// Замкнутая или открытая ломаная — одна фигура пути после развёртки кривых.
#[derive(Clone, Debug, PartialEq)]
pub struct Polyline {
    pub points: Vec<Point>,
    pub closed: bool,
}

/// Вид точки пути — младшие три бита `PathPointType`.
pub const TYPE_START: u8 = 0;
pub const TYPE_LINE: u8 = 1;
pub const TYPE_BEZIER: u8 = 3;
pub const TYPE_MASK: u8 = 0x07;
/// Точка замыкает фигуру.
pub const TYPE_CLOSE: u8 = 0x80;

/// Сколько точек растеризатор разворачивает из одного пути.
///
/// Предел против программы, которая передаёт миллионы точек или кривые с
/// координатами порядка 1e30: без него развёртка съела бы кучу `/bin/dotnet`
/// раньше, чем дойдёт до отказа распределителя, — а тот в чужом процессе
/// не всегда успевает вернуть ошибку.
pub const MAX_POINTS: usize = 1 << 20;

/// Самая большая координата, с которой идёт счёт. Дальше — прижимается: точка
/// за миллион экранов не отличается от точки за край, а в умножениях
/// растеризатора `1e300` превратилось бы в бесконечность.
const LIMIT: f64 = 16_777_216.0;

fn clamp(p: Point) -> Point {
    Point::new(p.x.clamp(-LIMIT, LIMIT), p.y.clamp(-LIMIT, LIMIT))
}

/// Развернуть путь GDI+ (`PathPoints` парами `x, y` и `PathTypes`) в ломаные,
/// преобразовав его матрицей. Кривые делятся на хорды не длиннее, чем
/// отклоняются от кривой на `tolerance` точек **после** преобразования.
///
/// Путь приходит от программы, и верить ему нельзя: неизвестный вид точки
/// считается отрезком, кривая без трёх точек — отрезками к тому, что есть,
/// нечисловая координата — отказом от всего пути (рисовать половину фигуры
/// хуже, чем ничего: это выглядит дефектом отрисовки, а не данных).
pub fn flatten(points: &[f32], types: &[u8], matrix: &Matrix, tolerance: f64) -> Result<Vec<Polyline>, Error> {
    let count = (points.len() / 2).min(types.len());
    let mut figures = Vec::new();
    if points[..count * 2].iter().any(|v| !v.is_finite()) {
        return Ok(figures);
    }
    let at = |index: usize| clamp(matrix.apply(Point::new(f64::from(points[index * 2]), f64::from(points[index * 2 + 1]))));
    let tolerance = if tolerance > 0.001 { tolerance } else { 0.001 };
    let mut current: Option<Polyline> = None;
    let mut total = 0usize;
    let mut index = 0;
    while index < count {
        let kind = types[index] & TYPE_MASK;
        let figure = match current.as_mut() {
            Some(figure) if kind != TYPE_START => figure,
            _ => {
                if let Some(done) = current.take() {
                    push(&mut figures, done)?;
                }
                let mut fresh = Vec::new();
                push(&mut fresh, at(index))?;
                total += 1;
                // Фигура из одной точки с пометкой «замкнуть» — пустая, но
                // следующая точка обязана начать новую, а не продолжить её.
                if types[index] & TYPE_CLOSE != 0 {
                    push(&mut figures, Polyline { points: fresh, closed: true })?;
                } else {
                    current = Some(Polyline { points: fresh, closed: false });
                }
                index += 1;
                continue;
            }
        };
        if kind == TYPE_BEZIER && index + 2 < count {
            let p0 = *figure.points.last().unwrap_or(&at(index));
            let (p1, p2, p3) = (at(index), at(index + 1), at(index + 2));
            let segments = bezier_segments(p0, p1, p2, p3, tolerance);
            for step in 1..=segments {
                let t = step as f64 / segments as f64;
                push(&mut figure.points, bezier(p0, p1, p2, p3, t))?;
            }
            total += segments;
            let closes = types[index + 2] & TYPE_CLOSE != 0;
            index += 3;
            if closes {
                figure.closed = true;
            }
        } else {
            push(&mut figure.points, at(index))?;
            total += 1;
            if types[index] & TYPE_CLOSE != 0 {
                figure.closed = true;
            }
            index += 1;
        }
        if total > MAX_POINTS {
            return Err(Error::TooComplex);
        }
        // Замкнутая фигура кончилась: следующая точка начинает новую, даже
        // если её вид — «отрезок» (так пишет GDI+ после `CloseFigure`).
        if current.as_ref().is_some_and(|figure| figure.closed) {
            if let Some(done) = current.take() {
                push(&mut figures, done)?;
            }
        }
    }
    if let Some(done) = current.take() {
        push(&mut figures, done)?;
    }
    Ok(figures)
}

/// Сколько хорд нужно кубической кривой, чтобы отойти от неё не больше чем
/// на `tolerance`: отклонение хорды не больше `3/4 · max|вторая разность| /
/// n²` (оценка Вана для кривых Безье).
fn bezier_segments(p0: Point, p1: Point, p2: Point, p3: Point, tolerance: f64) -> usize {
    let dd = |a: Point, b: Point, c: Point| libm::hypot(a.x - 2.0 * b.x + c.x, a.y - 2.0 * b.y + c.y);
    let bend = dd(p0, p1, p2).max(dd(p1, p2, p3));
    let n = libm::ceil(libm::sqrt(0.75 * bend / tolerance));
    if n.is_finite() { (n as usize).clamp(1, 1024) } else { 1024 }
}

fn bezier(p0: Point, p1: Point, p2: Point, p3: Point, t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    Point::new(a * p0.x + b * p1.x + c * p2.x + d * p3.x, a * p0.y + b * p1.y + c * p2.y + d * p3.y)
}
