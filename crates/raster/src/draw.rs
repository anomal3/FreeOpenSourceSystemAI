//! Операции `Graphics` целиком: залить путь, обвести путь пером.

use alloc::vec::Vec;

use crate::geom::{Matrix, Point, flatten};
use crate::line;
use crate::paint::{Paint, Target};
use crate::scan::{self, FillRule};
use crate::stroke::{self, Pen};
use crate::{Error, push};

/// Насколько хорда кривой отходит от неё, в точках устройства. Четверть
/// точки глазу не видна даже на сглаженном крае окружности радиусом в пять
/// точек, а хорд выходит вдвое меньше, чем при десятой.
const TOLERANCE: f64 = 0.2;

/// Залить путь (`points` парами `x, y`, `types` — `PathTypes`), переведённый
/// в координаты устройства матрицей `matrix`.
pub fn fill_path(
    target: &mut Target<'_, '_>,
    paint: &Paint<'_>,
    points: &[f32],
    types: &[u8],
    matrix: &Matrix,
    rule: FillRule,
    antialias: bool,
) -> Result<(), Error> {
    let shifted = matrix.translated(target.shift, target.shift);
    let mut polygons = Vec::new();
    for figure in flatten(points, types, &shifted, TOLERANCE)? {
        push(&mut polygons, figure.points)?;
    }
    fill_polygons(target, paint, &polygons, rule, antialias)
}

/// Залить многоугольники, уже стоящие в пространстве растеризатора.
pub fn fill_polygons(
    target: &mut Target<'_, '_>,
    paint: &Paint<'_>,
    polygons: &[Vec<Point>],
    rule: FillRule,
    antialias: bool,
) -> Result<(), Error> {
    let area = target.area();
    if area.is_empty() {
        return Ok(());
    }
    scan::rasterize(polygons, rule, antialias, area, &mut |y, x, row| target.paint_row(paint, y, x, row))
}

/// Обвести путь пером. Толщина пера — в координатах программы и растёт вместе
/// с преобразованием, как у GDI+; тоньше точки устройства перо не бывает.
pub fn stroke_path(
    target: &mut Target<'_, '_>,
    paint: &Paint<'_>,
    points: &[f32],
    types: &[u8],
    matrix: &Matrix,
    pen: &Pen<'_>,
    antialias: bool,
) -> Result<(), Error> {
    let scale = matrix.scale();
    if !(scale > 0.0) || !scale.is_finite() {
        return Ok(());
    }
    let shifted = matrix.translated(target.shift, target.shift);
    if !antialias && pen.width * scale <= 1.0 + 1e-9 && pen.dashes.is_empty() {
        let figures = flatten(points, types, &shifted, TOLERANCE)?;
        let area = target.area();
        line::cosmetic(&figures, area, &mut |x, y| target.paint_row(paint, y, x, &[255]));
        return Ok(());
    }
    // Обводка строится в координатах программы (иначе перо после сжатия по
    // одной оси было бы круглым, а у GDI+ оно сплющено), а кривые делятся с
    // допуском, пересчитанным в них же.
    let figures = flatten(points, types, &Matrix::IDENTITY, TOLERANCE / scale)?;
    let wide = Pen { width: pen.width.max(1.0 / scale), ..pen.clone() };
    let mut polygons = stroke::stroke(&figures, &wide)?;
    for polygon in &mut polygons {
        for point in polygon.iter_mut() {
            *point = shifted.apply(*point);
        }
    }
    fill_polygons(target, paint, &polygons, FillRule::NonZero, antialias)
}
