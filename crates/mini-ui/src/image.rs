//! Картинки — значки программ (фаза 71i, значки как в Windows).
//!
//! Формат свой и простейший — FIMG: `FIMG`, ширина и высота (`u16`,
//! little-endian), затем точки построчно по четыре байта `R G B A` (альфа не
//! умножена). PNG разбирает `xtask` при сборке пакета: декодер PNG в ядре —
//! это zlib и фильтры строк ради картинки в несколько килобайт, а вход у него
//! — файл из чужого пакета.

use alloc::vec::Vec;

use crate::draw;
use crate::{Color, Rect, Surface};

/// Заголовок формата.
pub const MAGIC: &[u8; 4] = b"FIMG";

/// Самая большая сторона, которую стоит читать.
const MAX_SIDE: u32 = 512;

/// Картинка в памяти: точки `R G B A`.
pub struct Image {
    pub width: u32,
    pub height: u32,
    rgba: Vec<[u8; 4]>,
}

impl Image {
    /// Разобрать FIMG. `None` — не тот формат или размер не сходится.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != MAGIC {
            return None;
        }
        let width = u32::from(u16::from_le_bytes([*bytes.get(4)?, *bytes.get(5)?]));
        let height = u32::from(u16::from_le_bytes([*bytes.get(6)?, *bytes.get(7)?]));
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        let count = (width * height) as usize;
        let data = bytes.get(8..8 + count * 4)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(count).ok()?;
        rgba.extend(data.chunks_exact(4).map(|px| [px[0], px[1], px[2], px[3]]));
        Some(Self { width, height, rgba })
    }

    /// Точка `R G B A` (за краем — прозрачная).
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        self.rgba[(y * self.width + x) as usize]
    }

    /// Нарисовать картинку в квадрат `rect` с прозрачностью.
    ///
    /// Уменьшение — усреднением по площади (иначе значок 128×128 в 24 точки
    /// выходит рваным), увеличение — ближайшей точкой. Альфа учитывается при
    /// усреднении: прозрачные точки не тянут цвет края в чёрное.
    pub fn draw(&self, surface: &mut Surface, rect: Rect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        for ty in 0..rect.h {
            let sy0 = ty * self.height / rect.h;
            let sy1 = ((ty + 1) * self.height / rect.h).max(sy0 + 1).min(self.height);
            for tx in 0..rect.w {
                let sx0 = tx * self.width / rect.w;
                let sx1 = ((tx + 1) * self.width / rect.w).max(sx0 + 1).min(self.width);
                let (mut r, mut g, mut b, mut a, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let [pr, pg, pb, pa] = self.rgba[(sy * self.width + sx) as usize];
                        let pa = u32::from(pa);
                        r += u32::from(pr) * pa;
                        g += u32::from(pg) * pa;
                        b += u32::from(pb) * pa;
                        a += pa;
                        n += 1;
                    }
                }
                if a == 0 || n == 0 {
                    continue;
                }
                let color = Color::rgb((r / a) as u8, (g / a) as u8, (b / a) as u8);
                let alpha = (a / n).min(255) as u8;
                draw::blend_pixel(surface, rect.x + tx as i32, rect.y + ty as i32, color, alpha);
            }
        }
    }
}

/// Стрелка ярлыка, как у Windows: белый квадрат со скруглением в левом
/// нижнем углу значка `icon` и синяя загнутая стрелка в нём.
pub fn draw_shortcut_arrow(surface: &mut Surface, icon: Rect) {
    let side = (icon.w * 3 / 8).max(10);
    let badge = Rect::new(icon.x, icon.bottom() - side as i32, side, side);
    draw::rounded(surface, badge, (side / 5).max(2), Color::rgb(0xFF, 0xFF, 0xFF), 255);
    draw::rounded_stroke(surface, badge, (side / 5).max(2), Color::rgb(0x80, 0x80, 0x80), 160);
    let ink = Color::rgb(0x1E, 0x5A, 0xD6);
    // Стрелка: дуга снизу вверх направо и наконечник — в долях стороны.
    let unit = side as i32;
    let at = |fx: i32, fy: i32| (badge.x + unit * fx / 16, badge.y + unit * fy / 16);
    let thick = (side / 7).max(2) as i32;
    // Хвост — вертикаль снизу, затем к правому верхнему углу.
    let (x0, y0) = at(5, 13);
    let (x1, y1) = at(5, 8);
    let (x2, y2) = at(11, 5);
    line(surface, x0, y0, x1, y1, thick, ink);
    line(surface, x1, y1, x2, y2, thick, ink);
    // Наконечник.
    let (h1x, h1y) = at(7, 4);
    let (h2x, h2y) = at(10, 9);
    line(surface, x2, y2, h1x, h1y, thick, ink);
    line(surface, x2, y2, h2x, h2y, thick, ink);
}

/// Толстая линия кругами — стрелка маленькая, сглаживать её незачем.
fn line(surface: &mut Surface, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, color: Color) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    let r = (thick / 2).max(1);
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        draw::circle(surface, x, y, r as u32, color, 255);
    }
}
