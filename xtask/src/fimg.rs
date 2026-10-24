//! Значок программы для пакета: PNG → FIMG (формат `mini_ui::image`).
//!
//! Разбирается обычный PNG без чересстрочности: 8 бит на канал, серый, RGB,
//! палитра, с альфой и без. Этого хватает значкам из `hicolor` пакетов Debian;
//! всё прочее — отказ, а не догадка.

use std::io::Read;

use anyhow::{Context, Result, bail};

/// Сторона значка в пакете: стол рисует 32–48, панель 24, а запас вдвое
/// даёт чистое уменьшение на плотном экране.
pub const SIDE: u32 = 64;

/// PNG → FIMG `SIDE × SIDE`.
pub fn from_png(png: &[u8]) -> Result<Vec<u8>> {
    let (width, height, rgba) = decode(png)?;
    let side = SIDE;
    let mut out = Vec::with_capacity(8 + (side * side * 4) as usize);
    out.extend_from_slice(b"FIMG");
    out.extend_from_slice(&(side as u16).to_le_bytes());
    out.extend_from_slice(&(side as u16).to_le_bytes());
    // Усреднение по площади с весом альфы — как у `Image::draw`.
    for ty in 0..side {
        let sy0 = ty * height / side;
        let sy1 = ((ty + 1) * height / side).max(sy0 + 1).min(height);
        for tx in 0..side {
            let sx0 = tx * width / side;
            let sx1 = ((tx + 1) * width / side).max(sx0 + 1).min(width);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    let at = ((sy * width + sx) * 4) as usize;
                    let pa = u64::from(rgba[at + 3]);
                    r += u64::from(rgba[at]) * pa;
                    g += u64::from(rgba[at + 1]) * pa;
                    b += u64::from(rgba[at + 2]) * pa;
                    a += pa;
                    n += 1;
                }
            }
            if a == 0 {
                out.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                out.extend_from_slice(&[(r / a) as u8, (g / a) as u8, (b / a) as u8, (a / n) as u8]);
            }
        }
    }
    Ok(out)
}

/// Разобрать PNG в `RGBA`.
fn decode(png: &[u8]) -> Result<(u32, u32, Vec<u8>)> {
    if png.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice()) {
        bail!("не PNG");
    }
    let mut at = 8;
    let (mut width, mut height, mut depth, mut kind, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut alpha: Vec<u8> = Vec::new();
    let mut data = Vec::new();
    while at + 8 <= png.len() {
        let len = u32::from_be_bytes(png[at..at + 4].try_into()?) as usize;
        let tag = &png[at + 4..at + 8];
        let body = png.get(at + 8..at + 8 + len).context("PNG обрезан")?;
        match tag {
            b"IHDR" => {
                width = u32::from_be_bytes(body[0..4].try_into()?);
                height = u32::from_be_bytes(body[4..8].try_into()?);
                depth = body[8];
                kind = body[9];
                interlace = body[12];
            }
            b"PLTE" => palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect(),
            b"tRNS" => alpha = body.to_vec(),
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if depth != 8 || interlace != 0 || width == 0 || height == 0 {
        bail!("PNG {width}x{height}, глубина {depth}, чересстрочность {interlace} — не разбираю");
    }
    let channels = match kind {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        other => bail!("PNG с цветом вида {other}"),
    };
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(data.as_slice()).read_to_end(&mut raw).context("PNG: zlib")?;
    let stride = (width * channels) as usize;
    if raw.len() < (stride + 1) * height as usize {
        bail!("PNG: данных меньше, чем строк");
    }
    let mut pixels = vec![0u8; stride * height as usize];
    for y in 0..height as usize {
        let filter = raw[y * (stride + 1)];
        let line = &raw[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= channels as usize { pixels[y * stride + x - channels as usize] } else { 0 };
            let b = if y > 0 { pixels[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= channels as usize { pixels[(y - 1) * stride + x - channels as usize] } else { 0 };
            let value = match filter {
                0 => line[x],
                1 => line[x].wrapping_add(a),
                2 => line[x].wrapping_add(b),
                3 => line[x].wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => line[x].wrapping_add(paeth(a, b, c)),
                other => bail!("PNG: фильтр строки {other}"),
            };
            pixels[y * stride + x] = value;
        }
    }
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for px in pixels.chunks_exact(channels as usize) {
        match kind {
            0 => rgba.extend_from_slice(&[px[0], px[0], px[0], 255]),
            2 => rgba.extend_from_slice(&[px[0], px[1], px[2], 255]),
            3 => {
                let index = px[0] as usize;
                let [r, g, b] = palette.get(index).copied().unwrap_or([0, 0, 0]);
                rgba.extend_from_slice(&[r, g, b, alpha.get(index).copied().unwrap_or(255)]);
            }
            4 => rgba.extend_from_slice(&[px[0], px[0], px[0], px[1]]),
            _ => rgba.extend_from_slice(px),
        }
    }
    Ok((width, height, rgba))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = ((p - i16::from(a)).abs(), (p - i16::from(b)).abs(), (p - i16::from(c)).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}
