//! Области отсечения: `Region` и `Graphics.Clip` как программа операций.
//!
//! `Region` в GDI+ — не прямоугольник, а результат операций над путями:
//! объединить с эллипсом, вычесть прямоугольник, исключающее «или» с другой
//! областью. C# хранит эти операции как есть (`System.Drawing.Region`) в
//! обратной польской записи: фигура кладётся на стек, операция снимает две
//! верхние и кладёт результат. Запись, а не цепочка «слева направо», потому
//! что `a.Union(b)`, где `b` сама собрана из трёх фигур, цепочкой не
//! выражается. Здесь программа превращается в маску точек ровно там, где идёт
//! рисование, — или проверяется в одной точке для `IsVisible`.
//!
//! Маска без сглаживания: у GDI+ область отсекает по точкам целиком, и край
//! эллипса, которым отсечена заливка, выходит ступенчатым даже при
//! `SmoothingMode.AntiAlias`.

use alloc::vec::Vec;

use crate::geom::{Matrix, Point, flatten};
use crate::scan::{self, Bounds, FillRule};
use crate::{Error, filled, push};

/// Покрытие точек прямоугольника: 0 — отсечено, 255 — видно.
pub struct Mask {
    bounds: Bounds,
    data: Vec<u8>,
}

impl Mask {
    #[must_use]
    pub const fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// Покрытие точки; снаружи прямоугольника — ноль.
    #[must_use]
    pub fn at(&self, x: i32, y: i32) -> u8 {
        if x < self.bounds.x0 || y < self.bounds.y0 || x >= self.bounds.x1 || y >= self.bounds.y1 {
            return 0;
        }
        let index = (y - self.bounds.y0) as usize * self.bounds.width() + (x - self.bounds.x0) as usize;
        self.data.get(index).copied().unwrap_or(0)
    }
}

/// `CombineMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    Replace,
    Intersect,
    Union,
    Xor,
    Exclude,
    Complement,
}

impl Combine {
    /// Код `System.Drawing.Drawing2D.CombineMode`.
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Intersect,
            2 => Self::Union,
            3 => Self::Xor,
            4 => Self::Exclude,
            5 => Self::Complement,
            _ => Self::Replace,
        }
    }

    const fn apply(self, a: bool, b: bool) -> bool {
        match self {
            Self::Replace => b,
            Self::Intersect => a && b,
            Self::Union => a || b,
            Self::Xor => a != b,
            Self::Exclude => a && !b,
            Self::Complement => b && !a,
        }
    }
}

/// Шаг программы.
#[derive(Clone, Copy)]
pub enum Element<'a> {
    Infinite,
    Empty,
    Path { points: &'a [f32], types: &'a [u8], rule: FillRule },
    Combine(Combine),
}

/// Прочитать программу, как её раскладывает C#: по пять чисел на шаг —
/// `вид (0 бесконечность, 1 пусто, 2 путь, 3 операция), CombineMode, первая
/// точка, число точек, FillMode` — и общие для всех путей точки и виды точек.
/// Путь с диапазоном за пределами массивов считается пустым: это данные
/// программы, и падать на них среда не вправе.
pub fn decode<'a>(ops: &[i32], points: &'a [f32], types: &'a [u8]) -> Result<Vec<Element<'a>>, Error> {
    let mut elements = Vec::new();
    for op in ops.chunks_exact(5) {
        let element = match op[0] {
            0 => Element::Infinite,
            1 => Element::Empty,
            3 => Element::Combine(Combine::from_code(op[1])),
            _ => {
                let range = usize::try_from(op[2]).ok().zip(usize::try_from(op[3]).ok()).and_then(|(start, count)| {
                    let end = start.checked_add(count)?;
                    (end <= types.len() && end.checked_mul(2)? <= points.len()).then_some((start, end))
                });
                match range {
                    Some((start, end)) => Element::Path {
                        points: &points[start * 2..end * 2],
                        types: &types[start..end],
                        rule: if op[4] == 1 { FillRule::NonZero } else { FillRule::EvenOdd },
                    },
                    None => Element::Empty,
                }
            }
        };
        push(&mut elements, element)?;
    }
    Ok(elements)
}

fn polygons(points: &[f32], types: &[u8], matrix: &Matrix) -> Result<Vec<Vec<Point>>, Error> {
    let mut out = Vec::new();
    for figure in flatten(points, types, matrix, 0.25)? {
        push(&mut out, figure.points)?;
    }
    Ok(out)
}

/// Маска программы в прямоугольнике `bounds` после преобразования `matrix`
/// (координаты области → пространство растеризатора). Пустая программа —
/// бесконечная область; лишнее на стеке после конца объединяется, как если
/// бы программа кончалась объединением (так её не построит C#, но и падать
/// тут не на чем).
pub fn mask(elements: &[Element<'_>], matrix: &Matrix, bounds: Bounds) -> Result<Mask, Error> {
    let size = bounds.width() * bounds.height();
    let width = bounds.width();
    let mut stack: Vec<Vec<u8>> = Vec::new();
    for element in elements {
        match *element {
            Element::Infinite => push(&mut stack, filled(size, 255u8)?)?,
            Element::Empty => push(&mut stack, filled(size, 0u8)?)?,
            Element::Path { points, types, rule } => {
                let mut shape = filled(size, 0u8)?;
                let polygons = polygons(points, types, matrix)?;
                scan::rasterize(&polygons, rule, false, bounds, &mut |y, x, row| {
                    let base = (y - bounds.y0) as usize * width + (x - bounds.x0) as usize;
                    if let Some(slots) = shape.get_mut(base..base + row.len()) {
                        slots.copy_from_slice(row);
                    }
                })?;
                push(&mut stack, shape)?;
            }
            Element::Combine(combine) => {
                let Some(b) = stack.pop() else { continue };
                let Some(mut a) = stack.pop() else {
                    push(&mut stack, b)?;
                    continue;
                };
                for (a, &b) in a.iter_mut().zip(b.iter()) {
                    *a = if combine.apply(*a != 0, b != 0) { 255 } else { 0 };
                }
                push(&mut stack, a)?;
            }
        }
    }
    let mut data = match stack.pop() {
        Some(data) => data,
        None => filled(size, 255u8)?,
    };
    while let Some(rest) = stack.pop() {
        for (a, &b) in data.iter_mut().zip(rest.iter()) {
            *a = (*a).max(b);
        }
    }
    Ok(Mask { bounds, data })
}

/// Лежит ли точка области в программе — пересечениями, без растеризации.
pub fn contains(elements: &[Element<'_>], x: f64, y: f64) -> Result<bool, Error> {
    let mut stack: Vec<bool> = Vec::new();
    for element in elements {
        match *element {
            Element::Infinite => push(&mut stack, true)?,
            Element::Empty => push(&mut stack, false)?,
            Element::Path { points, types, rule } => {
                let polygons = polygons(points, types, &Matrix::IDENTITY)?;
                push(&mut stack, scan::contains(&polygons, rule, x, y))?;
            }
            Element::Combine(combine) => {
                let b = stack.pop().unwrap_or(false);
                let a = stack.pop().unwrap_or(false);
                push(&mut stack, combine.apply(a, b))?;
            }
        }
    }
    Ok(stack.iter().any(|inside| *inside) || elements.is_empty())
}

/// Границы области.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Extent {
    Infinite,
    Empty,
    Box([f64; 4]),
}

/// Границы программы оценкой по прямоугольникам: объединение и «или» —
/// объединение границ, пересечение — пересечение, вычитание — границы
/// уменьшаемого. Совпадает с `Region.GetBounds` у GDI+ на пробе (объединение,
/// потом пересечение, потом «или» и вычитание угла); точная граница после
/// вычитания, отрезающего целую сторону, потребовала бы растеризации.
pub fn extent(elements: &[Element<'_>]) -> Result<Extent, Error> {
    let mut stack: Vec<Extent> = Vec::new();
    for element in elements {
        let value = match *element {
            Element::Infinite => Extent::Infinite,
            Element::Empty => Extent::Empty,
            Element::Path { points, .. } => {
                let mut b: Option<[f64; 4]> = None;
                for pair in points.chunks_exact(2) {
                    let (x, y) = (f64::from(pair[0]), f64::from(pair[1]));
                    if !x.is_finite() || !y.is_finite() {
                        continue;
                    }
                    b = Some(match b {
                        None => [x, y, x, y],
                        Some([x0, y0, x1, y1]) => [x0.min(x), y0.min(y), x1.max(x), y1.max(y)],
                    });
                }
                match b {
                    Some(b) if b[2] > b[0] && b[3] > b[1] => Extent::Box(b),
                    _ => Extent::Empty,
                }
            }
            Element::Combine(combine) => {
                let b = stack.pop().unwrap_or(Extent::Empty);
                let a = stack.pop().unwrap_or(Extent::Empty);
                combine_extent(combine, a, b)
            }
        };
        push(&mut stack, value)?;
    }
    let mut result = stack.pop().unwrap_or(Extent::Infinite);
    while let Some(rest) = stack.pop() {
        result = combine_extent(Combine::Union, rest, result);
    }
    Ok(result)
}

fn combine_extent(combine: Combine, a: Extent, b: Extent) -> Extent {
    match combine {
        Combine::Replace => b,
        Combine::Intersect => match (a, b) {
            (Extent::Infinite, other) | (other, Extent::Infinite) => other,
            (Extent::Box(a), Extent::Box(b)) => {
                let r = [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])];
                if r[2] > r[0] && r[3] > r[1] { Extent::Box(r) } else { Extent::Empty }
            }
            _ => Extent::Empty,
        },
        Combine::Union | Combine::Xor => match (a, b) {
            (Extent::Infinite, _) | (_, Extent::Infinite) => Extent::Infinite,
            (Extent::Empty, other) | (other, Extent::Empty) => other,
            (Extent::Box(a), Extent::Box(b)) => Extent::Box([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        },
        Combine::Exclude => a,
        Combine::Complement => b,
    }
}
