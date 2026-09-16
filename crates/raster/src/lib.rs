// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Растеризатор `System.Drawing` своей среды .NET (веха v0.7c, фаза N9).
//!
//! # Зачем отдельный крейт
//!
//! До N9 форма WinForms умела ровно то, что понадобилось её элементам:
//! залить прямоугольник и написать строку. Чужая программа на C# рисует через
//! GDI+ — пути из кривых Безье, перья с толщиной и стыками, сглаживание,
//! преобразования, отсечение произвольной областью, — и без этого слоя
//! отваливается на первом же `Graphics.FillEllipse`.
//!
//! Растеризатор — чистая функция «геометрия + кисть → точки буфера»: ни окон,
//! ни кучи среды, ни системных вызовов. Поэтому он живёт отдельно от `clr-vm`
//! и проверяется `cargo test` на машине разработчика, где видна каждая точка,
//! а в образе его зовёт один и тот же код и для `Bitmap` (массив в куче среды),
//! и для окна формы (страницы, отображённые ядром).
//!
//! # Почему в Rust, а не на C#
//!
//! Базовая библиотека исполняется интерпретатором IL, и цикл по каждой точке
//! окна в нём стоил бы секунды на кадр в отладочной сборке. Здесь — одна
//! инструкция IL на фигуру.
//!
//! # Модель точки — как у GDI+
//!
//! Проверено на Windows пробой (`tools/dotnet/samples/drawing`): при
//! `PixelOffsetMode.None` центр точки `(x, y)` лежит на **целых**
//! координатах, а не на половинках. `FillRectangle(10, 10, 20, 10)` без
//! сглаживания закрашивает столбцы 10..29, а со сглаживанием — половиной
//! столбцы 10 и 30. Весь крейт считает в пространстве, где точка `(x, y)`
//! занимает квадрат `[x, x+1) × [y, y+1)`; вызывающий сдвигает геометрию на
//! половину точки ([`Target::shift`]) — ровно это и делает режим GDI+.

#![no_std]

extern crate alloc;

pub mod draw;
pub mod geom;
pub mod image;
pub mod line;
pub mod paint;
pub mod region;
pub mod scan;
pub mod stroke;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use alloc::vec::Vec;

pub use geom::{Matrix, Point, Polyline};
pub use paint::{Canvas, Compositing, Paint, PixelFormat, Target};
pub use scan::{Bounds, FillRule};

/// Почему рисование не состоялось.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Кончилась память. Путь из миллиона точек — данные программы, и отказ
    /// распределителя обязан стать ошибкой, а не остановкой `/bin/dotnet`.
    OutOfMemory,
    /// Фигура сложнее, чем растеризатор соглашается разбирать (см.
    /// [`geom::MAX_POINTS`]).
    TooComplex,
}

/// Дописать в вектор, не паникуя при отказе распределителя.
pub(crate) fn push<T>(items: &mut Vec<T>, item: T) -> Result<(), Error> {
    items.try_reserve(1).map_err(|_| Error::OutOfMemory)?;
    items.push(item);
    Ok(())
}

/// Вектор из `count` копий значения.
pub(crate) fn filled<T: Clone>(count: usize, value: T) -> Result<Vec<T>, Error> {
    let mut items = Vec::new();
    items.try_reserve_exact(count).map_err(|_| Error::OutOfMemory)?;
    items.resize(count, value);
    Ok(items)
}
