//! Картинка на холст: `Graphics.DrawImage` с масштабом, поворотом и частью
//! источника.
//!
//! Каждая точка назначения переводится обратной матрицей в координаты
//! источника и берёт цвет ближайшей точки или смесь четырёх. Обратное
//! отображение, а не прямое, потому что прямое при увеличении оставляет между
//! перенесёнными точками дыры.
//!
//! Правила выбора точки источника сняты пробой на Windows (см. комментарий в
//! цикле [`draw`]).

use crate::geom::{Matrix, Point};
use crate::paint::{Target, mul255};

/// `InterpolationMode`, сведённый к двум способам.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    Nearest,
    Bilinear,
}

/// Источник: ARGB без умножения, строками.
pub struct Source<'a> {
    pub pixels: &'a [u32],
    pub width: u32,
    pub height: u32,
}

impl Source<'_> {
    fn at(&self, x: i64, y: i64) -> u32 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 0;
        }
        self.pixels.get(y as usize * self.width as usize + x as usize).copied().unwrap_or(0)
    }
}

/// Нарисовать часть источника `[sx, sx+sw) × [sy, sy+sh)`, где `to_device`
/// переводит координаты источника в координаты устройства.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    target: &mut Target<'_, '_>,
    source: &Source<'_>,
    part: [f64; 4],
    to_device: &Matrix,
    interpolation: Interpolation,
    opacity: u8,
) {
    let [sx, sy, sw, sh] = part;
    // Часть за пределами источника обрезается им самим: точки вне картинки
    // прозрачны и не рисуются.
    let (x0, y0) = (sx.max(0.0), sy.max(0.0));
    let (x1, y1) = ((sx + sw).min(f64::from(source.width)), (sy + sh).min(f64::from(source.height)));
    if !(x1 > x0 && y1 > y0) {
        return;
    }
    let Some(to_source) = to_device.invert() else { return };
    // Рамка на точку источника шире части: смесь у края берёт и соседа за ним.
    let corners = [Point::new(x0 - 1.0, y0 - 1.0), Point::new(x1 + 1.0, y0 - 1.0), Point::new(x1 + 1.0, y1 + 1.0), Point::new(x0 - 1.0, y1 + 1.0)]
        .map(|p| to_device.apply(p));
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in corners {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let limit = |v: f64| libm::floor(v).clamp(-16_777_216.0, 16_777_216.0) as i32;
    let area = crate::scan::Bounds::new(limit(min_x), limit(min_y), limit(max_x) + 1, limit(max_y) + 1).intersect(&target.area());
    if area.is_empty() {
        return;
    }
    // Центр точки устройства — там же, где у заливки (сдвиг режима), и точка
    // источника `i` имеет центр в `i` при `PixelOffsetMode.None`: так GDI+
    // даёт на увеличении вдвое угол (1, 1) смесью синего угла и красных
    // соседей (255, 191, 0, 64) и половинную прозрачность правого края, а
    // ближайшая точка берётся округлением (у правого края увеличения вдвое
    // последняя точка назначения уже пуста). Без масштаба это ложится точка в
    // точку при любом режиме.
    let shift = target.shift;
    for y in area.y0..area.y1 {
        for x in area.x0..area.x1 {
            let p = to_source.apply(Point::new(f64::from(x) + 0.5 - shift, f64::from(y) + 0.5 - shift));
            let (u, v) = (p.x + shift - 0.5, p.y + shift - 0.5);
            let color = match interpolation {
                Interpolation::Nearest => {
                    let (ix, iy) = (libm::floor(u + 0.5), libm::floor(v + 0.5));
                    if !(ix >= x0 && ix < x1 && iy >= y0 && iy < y1) {
                        continue;
                    }
                    source.at(ix as i64, iy as i64)
                }
                Interpolation::Bilinear => {
                    if !(u > x0 - 1.0 && u < x1 && v > y0 - 1.0 && v < y1) {
                        continue;
                    }
                    bilinear(source, u, v, [x0, y0, x1, y1])
                }
            };
            let coverage = match target.mask {
                Some(mask) => mul255(u32::from(opacity), u32::from(mask.at(x, y))) as u8,
                None => opacity,
            };
            let compositing = target.compositing;
            target.canvas.blend(x, y, color, coverage, compositing);
        }
    }
}

/// Смесь четырёх точек вокруг `(u, v)` (центры точек источника — на целых).
/// Точки за пределами части прозрачны: край увеличенной картинки мягко уходит
/// в фон, как у GDI+, а не повторяет крайнюю точку.
fn bilinear(source: &Source<'_>, u: f64, v: f64, part: [f64; 4]) -> u32 {
    let (fx, fy) = (libm::floor(u), libm::floor(v));
    let (tx, ty) = (u - fx, v - fy);
    let inside = |x: f64, y: f64| x >= part[0] && x < part[2] && y >= part[1] && y < part[3];
    let pick = |x: f64, y: f64| if inside(x, y) { source.at(x as i64, y as i64) } else { 0 };
    let samples = [pick(fx, fy), pick(fx + 1.0, fy), pick(fx, fy + 1.0), pick(fx + 1.0, fy + 1.0)];
    let weights = [(1.0 - tx) * (1.0 - ty), tx * (1.0 - ty), (1.0 - tx) * ty, tx * ty];
    // Смешивание с умножением на прозрачность: иначе край непрозрачной
    // картинки у прозрачной соседки темнел бы её «цветом» — чёрным нулём.
    let mut alpha = 0.0;
    let mut channels = [0.0f64; 3];
    for (sample, weight) in samples.iter().zip(weights) {
        let a = f64::from((sample >> 24) & 0xFF) * weight;
        alpha += a;
        for (index, shift) in [16u32, 8, 0].iter().enumerate() {
            channels[index] += f64::from((sample >> shift) & 0xFF) * a;
        }
    }
    if alpha <= 0.0 {
        return 0;
    }
    let byte = |v: f64| (libm::floor(v + 0.5).clamp(0.0, 255.0)) as u32;
    (byte(alpha) << 24) | (byte(channels[0] / alpha) << 16) | (byte(channels[1] / alpha) << 8) | byte(channels[2] / alpha)
}
