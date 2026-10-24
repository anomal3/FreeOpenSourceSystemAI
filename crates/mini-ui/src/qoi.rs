//! Картинки QOI — обои стола (фаза 72a).
//!
//! Распаковка — по спецификации <https://qoiformat.org/qoi-specification.pdf>,
//! только три канала: обоям прозрачность не нужна. Упаковывает `cargo xtask
//! wallpaper`.
//!
//! Вписывание в экран — «с обрезкой», как у обоев в Windows («Заполнение»):
//! картинка растягивается до того, чтобы закрыть экран целиком, лишнее по
//! краям срезается, середина остаётся посередине. Выборка билинейная, в
//! неподвижной точке: ядро работает без FPU.

use alloc::vec::Vec;

use crate::{Color, Surface};

/// Распакованная картинка: точки `R G B` построчно.
pub struct Picture {
    pub width: u32,
    pub height: u32,
    rgb: Vec<u8>,
}

/// Больше этого по стороне не распаковывается: 4K — 3840.
const MAX_SIDE: u32 = 4096;

impl Picture {
    /// Распаковать QOI. `None` — не QOI, размер вне разумного или данных не
    /// хватило. Памяти не дали — тоже `None`: обои найдутся и без картинки.
    #[must_use]
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..4)? != b"qoif" {
            return None;
        }
        let width = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?);
        let height = u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?);
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return None;
        }
        let count = (width * height) as usize;
        let mut rgb = Vec::new();
        rgb.try_reserve_exact(count * 3).ok()?;

        let mut seen = [[0u8; 3]; 64];
        let mut px = [0u8, 0, 0];
        let mut at = 14;
        let mut done = 0;
        while done < count {
            let op = *bytes.get(at)?;
            at += 1;
            let mut run = 1;
            match op {
                0xFE => {
                    px = [*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?];
                    at += 3;
                }
                0xFF => {
                    px = [*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?];
                    at += 4;
                }
                _ => match op >> 6 {
                    0 => px = seen[usize::from(op & 0x3F)],
                    1 => {
                        px[0] = px[0].wrapping_add((op >> 4) & 3).wrapping_sub(2);
                        px[1] = px[1].wrapping_add((op >> 2) & 3).wrapping_sub(2);
                        px[2] = px[2].wrapping_add(op & 3).wrapping_sub(2);
                    }
                    2 => {
                        let next = *bytes.get(at)?;
                        at += 1;
                        let dg = (op & 0x3F).wrapping_sub(32);
                        px[0] = px[0].wrapping_add(dg).wrapping_add(next >> 4).wrapping_sub(8);
                        px[1] = px[1].wrapping_add(dg);
                        px[2] = px[2].wrapping_add(dg).wrapping_add(next & 0x0F).wrapping_sub(8);
                    }
                    _ => run = usize::from(op & 0x3F) + 1,
                },
            }
            let slot = (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + 255 * 11) % 64;
            seen[slot] = px;
            let run = run.min(count - done);
            for _ in 0..run {
                rgb.extend_from_slice(&px);
            }
            done += run;
        }
        Some(Self { width, height, rgb })
    }

    /// Заполнить `surface` целиком: растянуть с обрезкой краёв (см. заголовок
    /// модуля).
    pub fn cover(&self, surface: &mut Surface) {
        let (dw, dh) = (u64::from(surface.width()), u64::from(surface.height()));
        let (sw, sh) = (u64::from(self.width), u64::from(self.height));
        if dw == 0 || dh == 0 {
            return;
        }
        // Шаг по картинке на точку экрана, 16.16: меньший из двух отношений —
        // тот, при котором картинка закрывает обе стороны.
        let step = if sw * dh < sh * dw { (sw << 16) / dw } else { (sh << 16) / dh };
        // Отступ середины: срезанное делится поровну между краями.
        let x0 = ((sw << 16).saturating_sub(step * dw)) / 2;
        let y0 = ((sh << 16).saturating_sub(step * dh)) / 2;
        let max_x = (sw - 1) << 16;
        let max_y = (sh - 1) << 16;
        let mut columns = Vec::new();
        if columns.try_reserve_exact(dw as usize).is_err() {
            return;
        }
        // Центр точки экрана, а не её угол: иначе картинка съезжает на
        // полшага.
        for x in 0..dw {
            columns.push((x0 + step * x + step / 2).saturating_sub(1 << 15).min(max_x));
        }
        for y in 0..dh {
            let fy = (y0 + step * y + step / 2).saturating_sub(1 << 15).min(max_y);
            let (row0, wy) = ((fy >> 16) as usize, (fy & 0xFFFF) as u32 >> 8);
            let row1 = (row0 + 1).min(self.height as usize - 1);
            let line = surface.row_mut(y as u32);
            for (slot, fx) in line.iter_mut().zip(columns.iter()) {
                let (col0, wx) = ((fx >> 16) as usize, (fx & 0xFFFF) as u32 >> 8);
                let col1 = (col0 + 1).min(self.width as usize - 1);
                let a = self.at(row0, col0);
                let b = self.at(row0, col1);
                let c = self.at(row1, col0);
                let d = self.at(row1, col1);
                let mut out = [0u8; 3];
                for channel in 0..3 {
                    let top = u32::from(a[channel]) * (256 - wx) + u32::from(b[channel]) * wx;
                    let bottom = u32::from(c[channel]) * (256 - wx) + u32::from(d[channel]) * wx;
                    out[channel] = ((top * (256 - wy) + bottom * wy) >> 16) as u8;
                }
                *slot = Color::rgb(out[0], out[1], out[2]).pixel();
            }
        }
    }

    fn at(&self, row: usize, col: usize) -> [u8; 3] {
        let index = (row * self.width as usize + col) * 3;
        [self.rgb[index], self.rgb[index + 1], self.rgb[index + 2]]
    }
}
