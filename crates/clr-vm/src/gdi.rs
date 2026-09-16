//! Члены `System.Drawing.GdiNative` (фаза N9): рисование `Graphics` растеризатором
//! `raster` в окно формы или в точки `Bitmap`.
//!
//! # Как C# передаёт операцию
//!
//! Геометрия, кисть, перо и отсечение приходят массивами чисел, а не объектами:
//! так член в Rust не зависит от раскладки полей классов C#, которую меняет
//! любая правка базовой библиотеки, а C# собирает массивы один раз на вызов.
//!
//! Цель — `int[8]`: `окно, ширина, высота, отсечение x, y, ширина, высота,
//! флаги`. Окно не меньше нуля — рисуем в окно хоста; иначе в `int[]` точек
//! `Bitmap` (ARGB без умножения на прозрачность, как `Format32bppArgb`).
//! Флаги: 1 — сглаживание, 2 — `PixelOffsetMode.Half`, 4 — `SourceCopy`,
//! 8 — у картинки нет прозрачности (`Format24bppRgb`).
//!
//! Кисть — `int[]` и `float[]`: сплошная `[0, 0, argb]`; градиент
//! `[1, wrap, n, цвета…]` и `[из устройства в кисть ×6, x0, y0, x1, y1, доли…]`;
//! текстура `[2, wrap, ширина, высота]`, `[из устройства в текстуру ×6]` и
//! точки картинки третьим массивом.
//!
//! Отсечение сложнее прямоугольника — программа области
//! ([`raster::region::decode`]) в координатах устройства.

use alloc::vec::Vec;

use raster::draw::{fill_path, stroke_path};
use raster::image::{Interpolation, Source};
use raster::paint::{Canvas, Compositing, Paint, PixelFormat, Target, Wrap};
use raster::region::{self, Extent, Mask};
use raster::scan::{Bounds, FillRule};
use raster::stroke::{Cap, Join, Pen};
use raster::{Matrix, Point};

use crate::heap::{Items, Object};
use crate::value::Value;
use crate::vm::Vm;
use crate::{Host, VmError, WindowPixels};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gdi {
    FillPath,
    StrokePath,
    FillRect,
    DrawImage,
    RegionContains,
    RegionBounds,
}

pub(crate) const TABLE: &[(&str, Gdi)] = &[
    (
        "System.Drawing.GdiNative::FillPath(int32[],int32[],int32[],float32[],uint8[],float32[],uint8[],float32[],int32,int32[],float32[],int32[])",
        Gdi::FillPath,
    ),
    (
        "System.Drawing.GdiNative::StrokePath(int32[],int32[],int32[],float32[],uint8[],float32[],uint8[],float32[],float32[],int32[],int32[],float32[],int32[])",
        Gdi::StrokePath,
    ),
    ("System.Drawing.GdiNative::FillRect(int32[],int32[],int32,int32,int32,int32,int32)", Gdi::FillRect),
    (
        "System.Drawing.GdiNative::DrawImage(int32[],int32[],int32[],float32[],uint8[],int32[],int32,int32,float32[],float32[],int32,int32)",
        Gdi::DrawImage,
    ),
    ("System.Drawing.GdiNative::RegionContains(int32[],float32[],uint8[],float32,float32)", Gdi::RegionContains),
    ("System.Drawing.GdiNative::RegionBounds(int32[],float32[],uint8[],float32[])", Gdi::RegionBounds),
];

const FLAG_ANTIALIAS: i32 = 1;
const FLAG_HALF_PIXEL: i32 = 2;
const FLAG_SOURCE_COPY: i32 = 4;
const FLAG_OPAQUE: i32 = 8;

/// Отказ растеризатора как отказ среды: память — `OutOfMemory`, слишком сложная
/// фигура — исключение программы, как `OverflowException` у GDI+.
fn raster_error<H: Host>(vm: &Vm<'_, H>, error: raster::Error) -> VmError {
    match error {
        raster::Error::OutOfMemory => VmError::OutOfMemory,
        raster::Error::TooComplex => vm.exception("System.OverflowException"),
    }
}

pub(crate) fn call<H: Host>(vm: &mut Vm<'_, H>, gdi: Gdi, args: &[Value]) -> Result<Option<Value>, VmError> {
    let arg = |index: usize| args.get(index).copied().ok_or(VmError::Invalid { what: "missing argument", at: alloc::string::String::new() });
    Ok(match gdi {
        Gdi::FillPath => {
            let target = ints(vm, arg(0)?)?.unwrap_or_default();
            let clip = clip_program(vm, arg(2)?, arg(3)?, arg(4)?)?;
            let points = floats(vm, arg(5)?)?.unwrap_or_default();
            let types = bytes(vm, arg(6)?)?.unwrap_or_default();
            let matrix = matrix_arg(vm, arg(7)?)?;
            let rule = if vm.int32(arg(8)?)? == 1 { FillRule::NonZero } else { FillRule::EvenOdd };
            let paint = PaintData::read(vm, arg(9)?, arg(10)?, arg(11)?)?;
            let Some(matrix) = matrix else { return Ok(None) };
            let antialias = flags(&target) & FLAG_ANTIALIAS != 0;
            let result = with_target(vm, &target, arg(1)?, &clip, |target| {
                let paint = paint.paint();
                fill_path(target, &paint, &points, &types, &matrix, rule, antialias)
            })?;
            if let Some(Err(error)) = result {
                return Err(raster_error(vm, error));
            }
            None
        }
        Gdi::StrokePath => {
            let target = ints(vm, arg(0)?)?.unwrap_or_default();
            let clip = clip_program(vm, arg(2)?, arg(3)?, arg(4)?)?;
            let points = floats(vm, arg(5)?)?.unwrap_or_default();
            let types = bytes(vm, arg(6)?)?.unwrap_or_default();
            let matrix = matrix_arg(vm, arg(7)?)?;
            let pen_floats = floats(vm, arg(8)?)?.unwrap_or_default();
            let pen_ints = ints(vm, arg(9)?)?.unwrap_or_default();
            let paint = PaintData::read(vm, arg(10)?, arg(11)?, arg(12)?)?;
            let (Some(matrix), [width, miter_limit, dash_offset, dashes @ ..]) = (matrix, pen_floats.as_slice()) else {
                return Ok(None);
            };
            let code = |index: usize| pen_ints.get(index).copied().unwrap_or(0);
            let pen = Pen {
                width: f64::from(*width),
                join: Join::from_code(code(0)),
                start_cap: Cap::from_code(code(1)),
                end_cap: Cap::from_code(code(2)),
                miter_limit: f64::from(*miter_limit),
                dashes,
                dash_offset: f64::from(*dash_offset),
            };
            let antialias = flags(&target) & FLAG_ANTIALIAS != 0;
            let result = with_target(vm, &target, arg(1)?, &clip, |target| {
                let paint = paint.paint();
                stroke_path(target, &paint, &points, &types, &matrix, &pen, antialias)
            })?;
            if let Some(Err(error)) = result {
                return Err(raster_error(vm, error));
            }
            None
        }
        // Прямоугольник в целых точках устройства сплошным цветом — то, чем
        // рисуют элементы WinForms. Без путей и массивов: форма перерисовывает
        // сотни таких за кадр.
        Gdi::FillRect => {
            let target = ints(vm, arg(0)?)?.unwrap_or_default();
            let (x, y, width, height) = (vm.int32(arg(2)?)?, vm.int32(arg(3)?)?, vm.int32(arg(4)?)?, vm.int32(arg(5)?)?);
            let argb = vm.int32(arg(6)?)? as u32;
            if width <= 0 || height <= 0 {
                return Ok(None);
            }
            let area = Bounds::new(x, y, x.saturating_add(width), y.saturating_add(height));
            with_target(vm, &target, arg(1)?, &None, |target| {
                let area = area.intersect(&target.area());
                let compositing = target.compositing;
                for py in area.y0..area.y1 {
                    for px in area.x0..area.x1 {
                        target.canvas.blend(px, py, argb, 255, compositing);
                    }
                }
            })?;
            None
        }
        Gdi::DrawImage => {
            let target = ints(vm, arg(0)?)?.unwrap_or_default();
            let clip = clip_program(vm, arg(2)?, arg(3)?, arg(4)?)?;
            let Some(source) = ints(vm, arg(5)?)? else {
                return Err(vm.exception("System.ArgumentNullException"));
            };
            let (width, height) = (vm.int32(arg(6)?)?, vm.int32(arg(7)?)?);
            let part = floats(vm, arg(8)?)?.unwrap_or_default();
            let to_device = matrix_arg(vm, arg(9)?)?;
            let interpolation = if vm.int32(arg(10)?)? == 5 { Interpolation::Nearest } else { Interpolation::Bilinear };
            let opacity = vm.int32(arg(11)?)?.clamp(0, 255) as u8;
            let (Ok(width), Ok(height), Some(to_device), [sx, sy, sw, sh, ..]) =
                (u32::try_from(width), u32::try_from(height), to_device, part.as_slice())
            else {
                return Ok(None);
            };
            if (width as usize).checked_mul(height as usize).is_none_or(|count| count > source.len()) {
                return Err(vm.exception("System.ArgumentException"));
            }
            let pixels = as_unsigned(&source);
            let part = [f64::from(*sx), f64::from(*sy), f64::from(*sw), f64::from(*sh)];
            with_target(vm, &target, arg(1)?, &clip, |target| {
                let source = Source { pixels, width, height };
                raster::image::draw(target, &source, part, &to_device, interpolation, opacity);
            })?;
            None
        }
        Gdi::RegionContains => {
            let ops = ints(vm, arg(0)?)?.unwrap_or_default();
            let points = floats(vm, arg(1)?)?.unwrap_or_default();
            let types = bytes(vm, arg(2)?)?.unwrap_or_default();
            let (x, y) = (float(vm, arg(3)?)?, float(vm, arg(4)?)?);
            let elements = region::decode(&ops, &points, &types).map_err(|error| raster_error(vm, error))?;
            let inside = region::contains(&elements, x, y).map_err(|error| raster_error(vm, error))?;
            Some(Value::I32(i32::from(inside)))
        }
        Gdi::RegionBounds => {
            let ops = ints(vm, arg(0)?)?.unwrap_or_default();
            let points = floats(vm, arg(1)?)?.unwrap_or_default();
            let types = bytes(vm, arg(2)?)?.unwrap_or_default();
            let elements = region::decode(&ops, &points, &types).map_err(|error| raster_error(vm, error))?;
            let extent = region::extent(&elements).map_err(|error| raster_error(vm, error))?;
            let (kind, values) = match extent {
                Extent::Infinite => (0, [0.0; 4]),
                Extent::Empty => (1, [0.0; 4]),
                Extent::Box([x0, y0, x1, y1]) => (2, [x0, y0, x1 - x0, y1 - y0]),
            };
            if let Value::Obj(Some(out)) = arg(3)? {
                if let Some(Object::Array { items: Items::F32(slots), .. }) = vm.heap.get_mut(out) {
                    for (slot, value) in slots.iter_mut().zip(values) {
                        *slot = value as f32;
                    }
                }
            }
            Some(Value::I32(kind))
        }
    })
}

fn flags(target: &[i32]) -> i32 {
    target.get(7).copied().unwrap_or(0)
}

/// Кисть, прочитанная из массивов, — владеет копиями, [`Paint`] на них ссылается.
struct PaintData {
    kind: i32,
    ints: Vec<i32>,
    floats: Vec<f32>,
    colors: Vec<u32>,
    texture: Vec<i32>,
}

impl PaintData {
    fn read<H: Host>(vm: &Vm<'_, H>, ints_value: Value, floats_value: Value, texture: Value) -> Result<Self, VmError> {
        let ints = ints(vm, ints_value)?.unwrap_or_default();
        let floats = floats(vm, floats_value)?.unwrap_or_default();
        let texture = ints_texture(vm, texture)?;
        let kind = ints.first().copied().unwrap_or(0);
        let mut colors = Vec::new();
        if kind == 1 {
            let count = usize::try_from(ints.get(2).copied().unwrap_or(0)).unwrap_or(0);
            let listed = ints.get(3..).unwrap_or(&[]);
            colors.try_reserve_exact(count.min(listed.len())).map_err(|_| VmError::OutOfMemory)?;
            colors.extend(listed.iter().take(count).map(|c| *c as u32));
        }
        Ok(Self { kind, ints, floats, colors, texture })
    }

    fn paint(&self) -> Paint<'_> {
        let int = |index: usize| self.ints.get(index).copied().unwrap_or(0);
        let matrix = || Matrix::from_elements(self.floats.get(..6).unwrap_or(&[])).unwrap_or(Matrix::IDENTITY);
        match self.kind {
            1 => {
                let f = |index: usize| f64::from(self.floats.get(index).copied().unwrap_or(0.0));
                let positions = self.floats.get(10..10 + self.colors.len()).unwrap_or(&[]);
                Paint::Linear {
                    to_brush: matrix(),
                    start: Point::new(f(6), f(7)),
                    end: Point::new(f(8), f(9)),
                    positions,
                    colors: if positions.len() == self.colors.len() { &self.colors } else { &[] },
                    wrap: Wrap::from_code(int(1)),
                }
            }
            2 => {
                let (width, height) = (u32::try_from(int(2)).unwrap_or(0), u32::try_from(int(3)).unwrap_or(0));
                let enough = (width as usize).checked_mul(height as usize).is_some_and(|count| count <= self.texture.len());
                Paint::Texture {
                    to_texture: matrix(),
                    pixels: as_unsigned(&self.texture),
                    width: if enough { width } else { 0 },
                    height: if enough { height } else { 0 },
                    wrap: Wrap::from_code(int(1)),
                }
            }
            _ => Paint::Solid(int(2) as u32),
        }
    }
}

fn ints_texture<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Vec<i32>, VmError> {
    Ok(ints(vm, value)?.unwrap_or_default())
}

/// Программа отсечения: `None` — прямоугольника из цели достаточно.
type ClipProgram = Option<(Vec<i32>, Vec<f32>, Vec<u8>)>;

fn clip_program<H: Host>(vm: &Vm<'_, H>, ops: Value, points: Value, types: Value) -> Result<ClipProgram, VmError> {
    let Some(ops) = ints(vm, ops)? else { return Ok(None) };
    Ok(Some((ops, floats(vm, points)?.unwrap_or_default(), bytes(vm, types)?.unwrap_or_default())))
}

/// Собрать цель рисования и выполнить над ней `draw`. Окна или картинки нет —
/// рисовать некуда, и это не ошибка: форма могла закрыться между событиями.
fn with_target<H: Host, R>(
    vm: &mut Vm<'_, H>,
    target: &[i32],
    image: Value,
    clip: &ClipProgram,
    draw: impl FnOnce(&mut Target<'_, '_>) -> R,
) -> Result<Option<R>, VmError> {
    let [window, width, height, cx, cy, cw, ch, flags, ..] = *target else {
        return Err(vm.invalid("drawing target is shorter than 8 numbers"));
    };
    let clip_bounds = Bounds::new(cx, cy, cx.saturating_add(cw.max(0)), cy.saturating_add(ch.max(0)));
    let shift = if flags & FLAG_HALF_PIXEL != 0 { 0.0 } else { 0.5 };
    let compositing = if flags & FLAG_SOURCE_COPY != 0 { Compositing::SourceCopy } else { Compositing::SourceOver };
    // Размер холста нужен маске до того, как холст занят: маска строится
    // только над видимой частью, а не над прямоугольником отсечения, который
    // у бесконечной области — миллионы точек.
    let canvas_size = if window >= 0 {
        vm.host.window_pixels(window as u32).map(|pixels| (pixels.width, pixels.height))
    } else {
        u32::try_from(width).ok().zip(u32::try_from(height).ok())
    };
    let Some((canvas_width, canvas_height)) = canvas_size else { return Ok(None) };
    let visible = clip_bounds.intersect(&Bounds::new(
        0,
        0,
        i32::try_from(canvas_width).unwrap_or(i32::MAX),
        i32::try_from(canvas_height).unwrap_or(i32::MAX),
    ));
    let mask: Option<Mask> = match clip {
        Some((ops, points, types)) => {
            let elements = region::decode(ops, points, types).map_err(|error| raster_error(vm, error))?;
            let built = region::mask(&elements, &Matrix::IDENTITY.translated(shift, shift), visible);
            Some(built.map_err(|error| raster_error(vm, error))?)
        }
        None => None,
    };
    let clip_bounds = visible;
    if window >= 0 {
        let Some(WindowPixels { pixels, width, height, red_low }) = vm.host.window_pixels(window as u32) else {
            return Ok(None);
        };
        let format = if red_low { PixelFormat::Xbgr } else { PixelFormat::Xrgb };
        let mut target = Target { canvas: Canvas { pixels, width, height, format }, compositing, clip: clip_bounds, mask: mask.as_ref(), shift };
        return Ok(Some(draw(&mut target)));
    }
    let Value::Obj(Some(image)) = image else { return Ok(None) };
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else { return Ok(None) };
    let Some(Object::Array { items: Items::I32(pixels), .. }) = vm.heap.get_mut(image) else {
        return Err(VmError::Invalid { what: "bitmap pixels are not an int array", at: alloc::string::String::new() });
    };
    let format = if flags & FLAG_OPAQUE != 0 { PixelFormat::Xrgb } else { PixelFormat::Argb };
    let pixels = as_unsigned_mut(pixels);
    let mut target = Target { canvas: Canvas { pixels, width, height, format }, compositing, clip: clip_bounds, mask: mask.as_ref(), shift };
    Ok(Some(draw(&mut target)))
}

fn as_unsigned(values: &[i32]) -> &[u32] {
    // SAFETY: `i32` и `u32` одного размера и выравнивания, и любой набор бит —
    // допустимое значение обоих; срез живёт столько же, сколько исходный.
    unsafe { core::slice::from_raw_parts(values.as_ptr().cast::<u32>(), values.len()) }
}

fn as_unsigned_mut(values: &mut [i32]) -> &mut [u32] {
    // SAFETY: то же, что в `as_unsigned`; изменяемая ссылка на исходный срез
    // одна и уходит в новый срез целиком.
    unsafe { core::slice::from_raw_parts_mut(values.as_mut_ptr().cast::<u32>(), values.len()) }
}

fn matrix_arg<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Option<Matrix>, VmError> {
    Ok(floats(vm, value)?.and_then(|elements| Matrix::from_elements(&elements)))
}

fn float<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<f64, VmError> {
    match value {
        Value::F(x) => Ok(x),
        Value::F32(x) => Ok(f64::from(x)),
        _ => Err(vm.invalid("expected a floating point number")),
    }
}

/// Копия `int[]`; `null` — `None`.
fn ints<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Option<Vec<i32>>, VmError> {
    array(vm, value, |items| match items {
        Items::I32(values) => Some(values.as_slice()),
        _ => None,
    })
}

fn floats<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Option<Vec<f32>>, VmError> {
    array(vm, value, |items| match items {
        Items::F32(values) => Some(values.as_slice()),
        _ => None,
    })
}

fn bytes<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Option<Vec<u8>>, VmError> {
    array(vm, value, |items| match items {
        Items::U8(values) => Some(values.as_slice()),
        _ => None,
    })
}

fn array<H: Host, T: Copy>(vm: &Vm<'_, H>, value: Value, pick: impl Fn(&Items) -> Option<&[T]>) -> Result<Option<Vec<T>>, VmError> {
    match value {
        Value::Obj(None) => Ok(None),
        Value::Obj(Some(object)) => match vm.heap.get(object) {
            Some(Object::Array { items, .. }) => {
                let Some(values) = pick(items) else {
                    return Err(vm.invalid("drawing array of an unexpected element type"));
                };
                let mut copy = Vec::new();
                copy.try_reserve_exact(values.len()).map_err(|_| VmError::OutOfMemory)?;
                copy.extend_from_slice(values);
                Ok(Some(copy))
            }
            _ => Err(vm.invalid("expected an array")),
        },
        _ => Err(vm.invalid("expected an array reference")),
    }
}
