//! Пачка значков темы (22.11): `assets/icons/themes/<тема>/*.png` → один
//! файл FIPK (`initrd/usr/share/icons/<тема>.pak`).
//!
//! Исходник каждого значка — квадратный PNG 128 (мастер 256 с чисткой
//! свечения делает скрипт вне репозитория, см. ROADMAP «Темы значков»). Из
//! него здесь получаются все размеры, которые рисует система: 128 (крупные
//! значки на 150–200 %), 64, 48, 32, 24 и 16. Уменьшение — честное усреднение
//! по площади с долями точек на краях, в умноженной на альфу форме (прозрачный
//! край не тянет цвет в чёрное); с 48 и ниже — лёгкое подчёркивание краёв,
//! иначе мелкий значок выходит мыльным. Каждый размер — QOI с альфой: значки
//! почти целиком из ровных заливок и прозрачности, сжимаются в разы.
//!
//! Формат пачки — в `mini_ui::image` (`PACK_MAGIC`, `pack_find`).

use std::path::Path;

use anyhow::{Context, Result, bail, ensure};

/// Размеры значка в пачке, от большего к меньшему.
pub const LEVELS: [u32; 6] = [128, 64, 48, 32, 24, 16];

/// Собрать пачку из каталога `dir` в файл `out`. Возвращает число значков и
/// размер файла.
pub fn build(dir: &Path, out: &Path) -> Result<(usize, usize)> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("нет каталога {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "png"))
        .collect();
    names.sort();
    ensure!(!names.is_empty(), "{}: нет значков", dir.display());
    ensure!(names.len() <= usize::from(u16::MAX), "слишком много значков");
    let mut entries = Vec::new();
    for path in &names {
        let name = path.file_stem().and_then(|stem| stem.to_str()).context("имя значка")?.to_string();
        ensure!(name.len() < 256, "{name}: длинное имя");
        let (width, height, rgba) = crate::fimg::decode(&std::fs::read(path)?).with_context(|| format!("{}", path.display()))?;
        if width != height {
            bail!("{}: не квадрат ({width}x{height})", path.display());
        }
        let mut blob = Vec::new();
        // От меньшего к большему: программа с маленькой кучей распаковывает
        // только нужный размер и останавливается (`Image::parse_for`).
        for side in LEVELS.iter().rev().copied() {
            let mut level = resample(&rgba, width, side);
            if side <= 48 {
                sharpen(&mut level, side, if side <= 24 { 0.45 } else { 0.3 });
            }
            blob.extend_from_slice(&encode_rgba(side, side, &unpremultiply(&level)));
        }
        entries.push((name, blob));
    }
    let index_len: usize = 12 + entries.iter().map(|(name, _)| 9 + name.len()).sum::<usize>();
    let mut file = Vec::new();
    file.extend_from_slice(b"FIPK");
    file.extend_from_slice(&1u16.to_le_bytes());
    file.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    file.extend_from_slice(&(index_len as u32).to_le_bytes());
    let mut offset = index_len;
    for (name, blob) in &entries {
        file.push(name.len() as u8);
        file.extend_from_slice(name.as_bytes());
        file.extend_from_slice(&(offset as u32).to_le_bytes());
        file.extend_from_slice(&(blob.len() as u32).to_le_bytes());
        offset += blob.len();
    }
    for (_, blob) in &entries {
        file.extend_from_slice(blob);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(out, &file).with_context(|| format!("не записать {}", out.display()))?;
    Ok((entries.len(), file.len()))
}

/// Точки в умноженной на альфу форме, `f32` на канал.
type Premul = Vec<[f32; 4]>;

/// Уменьшить (или оставить) квадрат `src × src` до `side × side`: усреднение
/// по площади с долями крайних точек.
fn resample(rgba: &[u8], src: u32, side: u32) -> Premul {
    let pre: Premul = rgba
        .chunks_exact(4)
        .map(|px| {
            let a = f32::from(px[3]) / 255.0;
            [f32::from(px[0]) * a, f32::from(px[1]) * a, f32::from(px[2]) * a, f32::from(px[3])]
        })
        .collect();
    if side >= src {
        return pre;
    }
    let weights = axis_weights(src, side);
    let (w, s) = (src as usize, side as usize);
    // По строкам, затем по столбцам.
    let mut rows = vec![[0f32; 4]; s * w];
    for y in 0..w {
        for (tx, taps) in weights.iter().enumerate() {
            let mut acc = [0f32; 4];
            for &(sx, wt) in taps {
                let p = pre[y * w + sx];
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            rows[y * s + tx] = acc;
        }
    }
    let mut out = vec![[0f32; 4]; s * s];
    for (ty, taps) in weights.iter().enumerate() {
        for x in 0..s {
            let mut acc = [0f32; 4];
            for &(sy, wt) in taps {
                let p = rows[sy * s + x];
                for c in 0..4 {
                    acc[c] += p[c] * wt;
                }
            }
            out[ty * s + x] = acc;
        }
    }
    out
}

/// Веса точек источника для каждой точки цели (сумма — 1).
fn axis_weights(src: u32, dst: u32) -> Vec<Vec<(usize, f32)>> {
    let scale = f64::from(src) / f64::from(dst);
    (0..dst)
        .map(|t| {
            let (start, end) = (f64::from(t) * scale, f64::from(t + 1) * scale);
            let mut taps = Vec::new();
            let mut sx = start.floor() as usize;
            while (sx as f64) < end && sx < src as usize {
                let overlap = (end.min(sx as f64 + 1.0) - start.max(sx as f64)).max(0.0);
                if overlap > 0.0 {
                    taps.push((sx, (overlap / scale) as f32));
                }
                sx += 1;
            }
            taps
        })
        .collect()
}

/// Подчеркнуть края: `x + k·(x − размытие 3×3)`, с обрезкой каждого канала
/// в `0..=альфа` (иначе на краю вылезает ореол).
fn sharpen(px: &mut Premul, side: u32, amount: f32) {
    let s = side as usize;
    let src = px.clone();
    let at = |x: isize, y: isize| src[(y.clamp(0, s as isize - 1) as usize) * s + x.clamp(0, s as isize - 1) as usize];
    for y in 0..s as isize {
        for x in 0..s as isize {
            let mut blur = [0f32; 4];
            for (dy, ky) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                for (dx, kx) in [(-1, 1.0), (0, 2.0), (1, 1.0)] {
                    let p = at(x + dx, y + dy);
                    for c in 0..4 {
                        blur[c] += p[c] * ky * kx / 16.0;
                    }
                }
            }
            let me = at(x, y);
            let mut out = [0f32; 4];
            out[3] = (me[3] + amount * (me[3] - blur[3])).clamp(0.0, 255.0);
            let limit = out[3];
            for c in 0..3 {
                out[c] = (me[c] + amount * (me[c] - blur[c])).clamp(0.0, limit);
            }
            px[y as usize * s + x as usize] = out;
        }
    }
}

/// Обратно в `R G B A` без умножения.
fn unpremultiply(px: &Premul) -> Vec<u8> {
    let mut out = Vec::with_capacity(px.len() * 4);
    for p in px {
        let a = p[3].round().clamp(0.0, 255.0);
        if a < 1.0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        let k = 255.0 / a;
        for c in 0..3 {
            out.push((p[c] * k).round().clamp(0.0, 255.0) as u8);
        }
        out.push(a as u8);
    }
    out
}

/// QOI с альфой (четыре канала).
fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    const OP_INDEX: u8 = 0x00;
    const OP_DIFF: u8 = 0x40;
    const OP_LUMA: u8 = 0x80;
    const OP_RUN: u8 = 0xC0;
    const OP_RGB: u8 = 0xFE;
    const OP_RGBA: u8 = 0xFF;
    let mut out = Vec::with_capacity(rgba.len() / 3);
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.push(4);
    out.push(0);
    let mut seen = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    let count = (width * height) as usize;
    for index in 0..count {
        let px = [rgba[index * 4], rgba[index * 4 + 1], rgba[index * 4 + 2], rgba[index * 4 + 3]];
        if px == prev {
            run += 1;
            if run == 62 || index + 1 == count {
                out.push(OP_RUN | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(OP_RUN | (run - 1));
            run = 0;
        }
        let slot = (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + usize::from(px[3]) * 11) % 64;
        if seen[slot] == px {
            out.push(OP_INDEX | slot as u8);
        } else {
            seen[slot] = px;
            if px[3] == prev[3] {
                let dr = px[0].wrapping_sub(prev[0]) as i8;
                let dg = px[1].wrapping_sub(prev[1]) as i8;
                let db = px[2].wrapping_sub(prev[2]) as i8;
                let (dr_dg, db_dg) = (dr.wrapping_sub(dg), db.wrapping_sub(dg));
                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    out.push(OP_DIFF | (((dr + 2) as u8) << 4) | (((dg + 2) as u8) << 2) | (db + 2) as u8);
                } else if (-32..=31).contains(&dg) && (-8..=7).contains(&dr_dg) && (-8..=7).contains(&db_dg) {
                    out.push(OP_LUMA | (dg + 32) as u8);
                    out.push((((dr_dg + 8) as u8) << 4) | (db_dg + 8) as u8);
                } else {
                    out.extend_from_slice(&[OP_RGB, px[0], px[1], px[2]]);
                }
            } else {
                out.extend_from_slice(&[OP_RGBA, px[0], px[1], px[2], px[3]]);
            }
        }
        prev = px;
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one() {
        for (src, dst) in [(128, 48), (128, 24), (128, 64), (128, 16)] {
            for taps in axis_weights(src, dst) {
                let sum: f32 = taps.iter().map(|(_, w)| w).sum();
                assert!((sum - 1.0).abs() < 1e-4, "{src}->{dst}: {sum}");
            }
        }
    }

    #[test]
    fn qoi_header_and_end() {
        let px = vec![10u8, 20, 30, 128, 10, 20, 30, 128, 0, 0, 0, 0, 255, 255, 255, 255];
        let q = encode_rgba(2, 2, &px);
        assert_eq!(&q[..4], b"qoif");
        assert_eq!(q[12], 4);
        assert_eq!(&q[q.len() - 8..], &[0, 0, 0, 0, 0, 0, 0, 1]);
    }
}
