//! Куда рисовать и чем: буфер точек, кисть, смешивание, маска отсечения.

use crate::geom::{Matrix, Point};
use crate::region::Mask;
use crate::scan::Bounds;

/// Как точка лежит в слове буфера.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// `Format32bppArgb` у `Bitmap`: прозрачность настоящая и **не**
    /// умножена на цвет — `GetPixel` отдаёт то, что записал `SetPixel`.
    Argb,
    /// Окно, у которого красный в третьем байте (`mini_ui::PIXEL_BGR`, в
    /// памяти B, G, R). Старший байт всегда 0xFF: окно непрозрачно.
    Xrgb,
    /// Окно, у которого красный в младшем байте (`mini_ui::PIXEL_RGB`).
    Xbgr,
}

/// Буфер точек строками сверху вниз.
pub struct Canvas<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
}

impl Canvas<'_> {
    /// Весь буфер как прямоугольник; буфер короче объявленного — по тому, что
    /// в нём действительно есть.
    #[must_use]
    pub fn bounds(&self) -> Bounds {
        let width = self.width as usize;
        let rows = if width == 0 { 0 } else { (self.pixels.len() / width).min(self.height as usize) };
        Bounds::new(0, 0, i32::try_from(self.width).unwrap_or(i32::MAX), i32::try_from(rows).unwrap_or(i32::MAX))
    }

    /// Точка как ARGB без умножения на прозрачность.
    #[must_use]
    pub fn get(&self, x: i32, y: i32) -> u32 {
        let Some(index) = self.index(x, y) else { return 0 };
        let raw = self.pixels[index];
        match self.format {
            PixelFormat::Argb => raw,
            PixelFormat::Xrgb => raw | 0xFF00_0000,
            PixelFormat::Xbgr => 0xFF00_0000 | swap_rb(raw),
        }
    }

    fn put(&mut self, x: i32, y: i32, argb: u32) {
        let Some(index) = self.index(x, y) else { return };
        self.pixels[index] = match self.format {
            PixelFormat::Argb => argb,
            PixelFormat::Xrgb => argb | 0xFF00_0000,
            PixelFormat::Xbgr => 0xFF00_0000 | swap_rb(argb),
        };
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (u32::try_from(x).ok()?, u32::try_from(y).ok()?);
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = y as usize * self.width as usize + x as usize;
        (index < self.pixels.len()).then_some(index)
    }

    /// Смешать цвет с точкой при покрытии `coverage` (0..=255).
    pub fn blend(&mut self, x: i32, y: i32, argb: u32, coverage: u8, compositing: Compositing) {
        if coverage == 0 {
            return;
        }
        if self.index(x, y).is_none() {
            return;
        }
        if compositing == Compositing::SourceCopy && coverage == 255 {
            // Полностью прозрачная точка у GDI+ — ноль целиком: пробой
            // `Clear(Color.Transparent)` (это 0x00FFFFFF) даёт 0,0,0,0.
            self.put(x, y, if argb >> 24 == 0 { 0 } else { argb });
            return;
        }
        let alpha = mul255((argb >> 24) & 0xFF, u32::from(coverage));
        if alpha == 0 {
            return;
        }
        if alpha == 255 {
            self.put(x, y, argb | 0xFF00_0000);
            return;
        }
        let dst = self.get(x, y);
        self.put(x, y, source_over(dst, argb, alpha));
    }
}

const fn swap_rb(pixel: u32) -> u32 {
    (pixel & 0x0000_FF00) | ((pixel >> 16) & 0xFF) | ((pixel & 0xFF) << 16)
}

/// `a · b / 255` с округлением — точное для всех байтов.
#[must_use]
pub const fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// «Источник поверх» так, как это делает GDI+ на `Format32bppArgb`: через
/// умноженные на прозрачность каналы, с округлением при умножении и
/// отбрасыванием дробной части при обратном делении. Правила сняты пробой
/// на Windows: белое под `(100, 255, 0, 0)` и затем `(200, 0, 128, 0)` даёт
/// `(255, 55, 133, 33)`, а полупрозрачное синее под полупрозрачным красным —
/// `(178, 143, 0, 111)`; обычное «делить с округлением» дало бы 134 и 112.
fn source_over(dst: u32, src: u32, alpha: u32) -> u32 {
    let da = (dst >> 24) & 0xFF;
    let inverse = 255 - alpha;
    let out_alpha = alpha + mul255(da, inverse);
    if out_alpha == 0 {
        return 0;
    }
    let channel = |shift: u32| {
        let s = mul255((src >> shift) & 0xFF, alpha);
        let d = mul255((dst >> shift) & 0xFF, da);
        let premultiplied = s + mul255(d, inverse);
        ((premultiplied * 255) / out_alpha).min(255)
    };
    (out_alpha << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Как новая точка ложится на старую.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compositing {
    SourceOver,
    SourceCopy,
}

/// Как кисть продолжается за своими пределами.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Tile,
    FlipX,
    FlipY,
    FlipXY,
    Clamp,
}

impl Wrap {
    #[must_use]
    pub const fn from_code(code: i32) -> Self {
        match code {
            1 => Self::FlipX,
            2 => Self::FlipY,
            3 => Self::FlipXY,
            4 => Self::Clamp,
            _ => Self::Tile,
        }
    }
}

/// Чем закрашивается фигура.
pub enum Paint<'a> {
    Solid(u32),
    /// `LinearGradientBrush`: доля `t` — проекция точки на отрезок
    /// `start → end` в пространстве кисти, цвет — по опорным точкам.
    Linear {
        /// Из точек устройства в пространство кисти.
        to_brush: Matrix,
        start: Point,
        end: Point,
        /// Доли по возрастанию от 0 до 1.
        positions: &'a [f32],
        /// ARGB у каждой доли.
        colors: &'a [u32],
        wrap: Wrap,
    },
    /// `TextureBrush`: картинка, повторённая по плоскости.
    Texture { to_texture: Matrix, pixels: &'a [u32], width: u32, height: u32, wrap: Wrap },
}

impl Paint<'_> {
    /// Цвет кисти в точке устройства `p` (уже центр точки в координатах
    /// программы, со сдвигом режима GDI+).
    #[must_use]
    pub fn color_at(&self, p: Point) -> u32 {
        match self {
            Self::Solid(argb) => *argb,
            Self::Linear { to_brush, start, end, positions, colors, wrap } => {
                let q = to_brush.apply(p);
                let (dx, dy) = (end.x - start.x, end.y - start.y);
                let length = dx * dx + dy * dy;
                let t = if length > 0.0 { ((q.x - start.x) * dx + (q.y - start.y) * dy) / length } else { 0.0 };
                gradient(positions, colors, wrap_unit(t, *wrap))
            }
            Self::Texture { to_texture, pixels, width, height, wrap } => {
                let q = to_texture.apply(p);
                let (Some(x), Some(y)) = (wrap_index(q.x, *width, *wrap, true), wrap_index(q.y, *height, *wrap, false)) else {
                    return 0;
                };
                pixels.get(y * *width as usize + x).copied().unwrap_or(0)
            }
        }
    }
}

/// Доля градиента в `[0, 1]` по правилу продолжения.
fn wrap_unit(t: f64, wrap: Wrap) -> f64 {
    if !t.is_finite() {
        return 0.0;
    }
    match wrap {
        Wrap::Clamp => t.clamp(0.0, 1.0),
        Wrap::Tile => t - libm::floor(t),
        _ => {
            let period = t - 2.0 * libm::floor(t / 2.0);
            if period > 1.0 { 2.0 - period } else { period }
        }
    }
}

fn wrap_index(v: f64, size: u32, wrap: Wrap, horizontal: bool) -> Option<usize> {
    if size == 0 || !v.is_finite() {
        return None;
    }
    let size_f = f64::from(size);
    let cell = libm::floor(v);
    let flip = match wrap {
        Wrap::FlipX => horizontal,
        Wrap::FlipY => !horizontal,
        Wrap::FlipXY => true,
        _ => false,
    };
    if wrap == Wrap::Clamp {
        return (cell >= 0.0 && cell < size_f).then_some(cell as usize);
    }
    let tile = libm::floor(cell / size_f);
    let mut index = cell - tile * size_f;
    if flip && (tile as i64) & 1 != 0 {
        index = size_f - 1.0 - index;
    }
    Some((index.clamp(0.0, size_f - 1.0)) as usize)
}

/// Цвет градиента в доле `t` между опорными точками, по каналам без
/// умножения на прозрачность.
///
/// Правило снято пробой GDI+ от чёрного к белому на сорок точек и совпадает
/// во всех сорока: канал считается от **ближнего** конца в шкале 256, а не
/// 255, — `round(256·t)` в первой половине и `255 − round(256·(1−t))` во
/// второй. Прямое `round(255·t)` расходится с ним в девяти точках из сорока
/// (57 против 58 в девятой), и программа, сверяющая цвет градиента, видела бы
/// другой цвет.
fn gradient(positions: &[f32], colors: &[u32], t: f64) -> u32 {
    let count = positions.len().min(colors.len());
    if count == 0 {
        return 0;
    }
    if count == 1 || t <= f64::from(positions[0]) {
        return colors[0];
    }
    for i in 1..count {
        let (p0, p1) = (f64::from(positions[i - 1]), f64::from(positions[i]));
        if t <= p1 {
            let local = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
            let (a, b) = (colors[i - 1], colors[i]);
            let mix = |shift: u32| {
                let (ca, cb) = (f64::from((a >> shift) & 0xFF), f64::from((b >> shift) & 0xFF));
                let step = (cb - ca) * 256.0 / 255.0;
                let value = if local <= 0.5 {
                    ca + libm::floor(step * local + 0.5)
                } else {
                    cb - libm::floor(step * (1.0 - local) + 0.5)
                };
                value.clamp(0.0, 255.0) as u32
            };
            return (mix(24) << 24) | (mix(16) << 16) | (mix(8) << 8) | mix(0);
        }
    }
    colors[count - 1]
}

/// Всё, что нужно одной операции рисования, кроме геометрии.
pub struct Target<'c, 'p> {
    pub canvas: Canvas<'c>,
    pub compositing: Compositing,
    /// Отсечение прямоугольником — уже пересечённое с буфером.
    pub clip: Bounds,
    /// Отсечение областью сложнее прямоугольника.
    pub mask: Option<&'p Mask>,
    /// На сколько сдвинуть координаты программы, чтобы точка `(x, y)`
    /// занимала `[x, x+1)`: 0.5 у `PixelOffsetMode.None`/`HighSpeed` (центр
    /// точки на целых), 0 у `Half`/`HighQuality`.
    pub shift: f64,
}

impl Target<'_, '_> {
    /// Прямоугольник, в котором разрешено рисовать.
    #[must_use]
    pub fn area(&self) -> Bounds {
        let mut area = self.clip.intersect(&self.canvas.bounds());
        if let Some(mask) = self.mask {
            area = area.intersect(&mask.bounds());
        }
        area
    }

    /// Отдать строку покрытия кистью.
    pub fn paint_row(&mut self, paint: &Paint<'_>, y: i32, x: i32, coverage: &[u8]) {
        let solid = match paint {
            Paint::Solid(argb) => Some(*argb),
            _ => None,
        };
        for (i, &cover) in coverage.iter().enumerate() {
            let px = x + i as i32;
            let cover = match self.mask {
                Some(mask) => mul255(u32::from(cover), u32::from(mask.at(px, y))) as u8,
                None => cover,
            };
            if cover == 0 {
                continue;
            }
            let color = match solid {
                Some(argb) => argb,
                None => paint.color_at(Point::new(f64::from(px) + 0.5 - self.shift, f64::from(y) + 0.5 - self.shift)),
            };
            self.canvas.blend(px, y, color, cover, self.compositing);
        }
    }
}
