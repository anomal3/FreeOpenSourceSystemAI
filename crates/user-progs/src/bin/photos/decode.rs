//! Picture decoders of «Фотографии» (24.11.2026): JPEG (`zune-jpeg`, baseline
//! and progressive), PNG (all colour types, 1–16 bits, Adam7 interlacing,
//! `tRNS`), BMP (1/4/8-bit palettes, 24 and 32 bits, no compression), GIF
//! (the first frame; the frame count is reported) and QOI.
//!
//! The format is taken from the signature, not from the extension: a «.jpg»
//! that is really a PNG is common among downloaded pictures. Pixels come out
//! packed for this machine ([`Packer`]); transparency is laid over a light
//! checkerboard, as picture editors show it. A picture larger than the
//! budget is shrunk by an integer factor (block average) right after
//! decoding — a 50-megapixel photo must not take the program down.
//!
//! The PNG, BMP and QOI readers started as copies of `wallconv`'s (which
//! keeps its own: it converts wallpapers in the kernel's name and should not
//! change under it).

use alloc::vec::Vec;

/// Largest side taken at all.
pub const MAX_SIDE: usize = 16_384;

/// Pixels packed for this machine (`mini_ui::Color::pixel` layout).
#[derive(Clone, Copy)]
pub struct Packer {
    red_shift: u32,
    blue_shift: u32,
}

impl Packer {
    /// Learn the layout from the toolkit (after `app::start`).
    #[must_use]
    pub fn new() -> Self {
        let red = mini_ui::Color::rgb(0xFF, 0, 0).pixel();
        if red & 0xFF == 0xFF { Self { red_shift: 0, blue_shift: 16 } } else { Self { red_shift: 16, blue_shift: 0 } }
    }

    #[inline]
    #[must_use]
    pub fn pack(self, r: u8, g: u8, b: u8) -> u32 {
        0xFF00_0000 | (u32::from(r) << self.red_shift) | (u32::from(g) << 8) | (u32::from(b) << self.blue_shift)
    }

    /// Pack with alpha laid over the checkerboard at `(x, y)`.
    #[inline]
    #[must_use]
    pub fn pack_alpha(self, r: u8, g: u8, b: u8, a: u8, x: usize, y: usize) -> u32 {
        if a == 255 {
            return self.pack(r, g, b);
        }
        let back: u32 = if ((x >> 3) + (y >> 3)) & 1 == 0 { 0xFF } else { 0xD6 };
        let a32 = u32::from(a);
        let mix = |c: u8| ((u32::from(c) * a32 + back * (255 - a32) + 127) / 255) as u8;
        self.pack(mix(r), mix(g), mix(b))
    }

    #[inline]
    #[must_use]
    pub fn rgb(self, pixel: u32) -> [u8; 3] {
        [(pixel >> self.red_shift) as u8, (pixel >> 8) as u8, (pixel >> self.blue_shift) as u8]
    }
}

/// A picture in packed pixels.
pub struct Picture {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

impl Picture {
    pub fn alloc(w: usize, h: usize) -> Result<Self, &'static str> {
        let mut px = Vec::new();
        px.try_reserve_exact(w.checked_mul(h).ok_or("too large")?).map_err(|_| "недостаточно памяти")?;
        px.resize(w * h, 0);
        Ok(Self { w, h, px })
    }
}

/// What came out of a file.
pub struct Decoded {
    pub picture: Picture,
    /// Size in the file (before a shrink).
    pub src_w: usize,
    pub src_h: usize,
    pub format: &'static str,
    /// `/bin/wallconv` (the desktop background) reads this file as it is.
    pub wall_ok: bool,
    /// Frames of an animated GIF (the first is shown).
    pub frames: usize,
}

/// Decode `data`. `budget` — most pixels kept (bigger is shrunk).
pub fn decode(data: &[u8], budget: usize, packer: Packer) -> Result<Decoded, &'static str> {
    let (picture, format, wall_ok, frames) = if data.starts_with(&[0xFF, 0xD8]) {
        (jpeg(data, budget, packer)?, "JPEG", true, 1)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (picture, interlaced) = png(data, packer)?;
        (picture, "PNG", !interlaced, 1)
    } else if data.starts_with(b"BM") {
        let (picture, plain) = bmp(data, packer)?;
        (picture, "BMP", plain, 1)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        let (picture, frames) = gif(data, packer)?;
        (picture, "GIF", false, frames)
    } else if data.starts_with(b"qoif") {
        (qoi(data, packer)?, "QOI", true, 1)
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Err("формат WebP пока не поддерживается");
    } else {
        return Err("это не картинка JPEG, PNG, BMP, GIF или QOI");
    };
    let (src_w, src_h) = (picture.w, picture.h);
    let picture = if picture.w * picture.h > budget { shrink_to_budget(picture, budget)? } else { picture };
    Ok(Decoded { picture, src_w, src_h, format, wall_ok, frames })
}

/// Integer factor so that `w*h/k²` fits the budget.
fn factor(w: usize, h: usize, budget: usize) -> usize {
    let mut k = 1;
    while (w / k) * (h / k) > budget.max(1) {
        k += 1;
    }
    k
}

/// Block average by an integer factor.
fn shrink_to_budget(src: Picture, budget: usize) -> Result<Picture, &'static str> {
    let k = factor(src.w, src.h, budget);
    let (w, h) = ((src.w / k).max(1), (src.h / k).max(1));
    let mut out = Picture::alloc(w, h)?;
    let n = (k * k) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 4];
            for dy in 0..k {
                let row = &src.px[(y * k + dy) * src.w + x * k..][..k];
                for &p in row {
                    sum[0] += p & 0xFF;
                    sum[1] += (p >> 8) & 0xFF;
                    sum[2] += (p >> 16) & 0xFF;
                }
            }
            out.px[y * w + x] = 0xFF00_0000 | (sum[0] / n) | ((sum[1] / n) << 8) | ((sum[2] / n) << 16);
        }
    }
    Ok(out)
}

fn jpeg(data: &[u8], budget: usize, packer: Packer) -> Result<Picture, &'static str> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_SIDE)
        .set_max_height(MAX_SIDE)
        .set_strict_mode(false);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(data, options);
    decoder.decode_headers().map_err(|_| "файл JPEG повреждён или не поддерживается")?;
    let (w, h) = decoder.dimensions().ok_or("файл JPEG повреждён")?;
    if w == 0 || h == 0 {
        return Err("файл JPEG повреждён");
    }
    // The decoder's own buffer (3 bytes a pixel) plus ours: refuse what
    // cannot fit rather than fail inside the allocator.
    if w.saturating_mul(h) > budget.saturating_mul(2) {
        return Err("фото слишком большое для свободной памяти");
    }
    let rgb = decoder.decode().map_err(|_| "файл JPEG повреждён или не поддерживается")?;
    if rgb.len() < w * h * 3 {
        return Err("файл JPEG обрезан");
    }
    let k = factor(w, h, budget);
    if k == 1 {
        let mut out = Picture::alloc(w, h)?;
        for (slot, c) in out.px.iter_mut().zip(rgb.chunks_exact(3)) {
            *slot = packer.pack(c[0], c[1], c[2]);
        }
        return Ok(out);
    }
    // Shrink while packing: never hold the full picture twice.
    let (ow, oh) = ((w / k).max(1), (h / k).max(1));
    let mut out = Picture::alloc(ow, oh)?;
    let n = (k * k) as u32;
    for y in 0..oh {
        for x in 0..ow {
            let mut sum = [0u32; 3];
            for dy in 0..k {
                let row = &rgb[((y * k + dy) * w + x * k) * 3..][..k * 3];
                for c in row.chunks_exact(3) {
                    sum[0] += u32::from(c[0]);
                    sum[1] += u32::from(c[1]);
                    sum[2] += u32::from(c[2]);
                }
            }
            out.px[y * ow + x] = packer.pack((sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8);
        }
    }
    Ok(out)
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le16(bytes: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
}

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
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

/// Adam7 passes: start x, start y, step x, step y.
const ADAM7: [(usize, usize, usize, usize); 7] = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];

/// PNG; `true` — interlaced.
fn png(data: &[u8], packer: Packer) -> Result<(Picture, bool), &'static str> {
    let (mut width, mut height, mut depth, mut color, mut interlace) = (0usize, 0usize, 0u8, 0u8, 0u8);
    let mut palette: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat: Vec<u8> = Vec::new();
    let mut at = 8;
    while at + 8 <= data.len() {
        let len = be32(&data[at..]) as usize;
        let kind = &data[at + 4..at + 8];
        let Some(body) = data.get(at + 8..at + 8 + len) else {
            // A cut file: show what arrived.
            break;
        };
        match kind {
            b"IHDR" if len >= 13 => {
                width = be32(body) as usize;
                height = be32(&body[4..]) as usize;
                depth = body[8];
                color = body[9];
                interlace = body[12];
            }
            b"PLTE" => palette = body,
            b"tRNS" => trns = body,
            b"IDAT" => {
                idat.try_reserve(len).map_err(|_| "недостаточно памяти")?;
                idat.extend_from_slice(body);
            }
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("неверный размер PNG");
    }
    let channels: usize = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err("неизвестный тип цвета PNG"),
    };
    let ok_depth = match color {
        3 => matches!(depth, 1 | 2 | 4 | 8),
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        _ => matches!(depth, 8 | 16),
    };
    if !ok_depth {
        return Err("неподдерживаемая глубина цвета PNG");
    }
    let bits = channels * depth as usize;
    let bpp = bits.div_ceil(8).max(1);
    let passes: alloc::vec::Vec<(usize, usize, usize, usize)> = if interlace == 1 { ADAM7.to_vec() } else { alloc::vec![(0, 0, 1, 1)] };
    // Bytes the unpacked stream must have.
    let mut expected = 0usize;
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = (width.saturating_sub(x0).div_ceil(dx), height.saturating_sub(y0).div_ceil(dy));
        if pw > 0 && ph > 0 {
            expected += ((pw * bits).div_ceil(8) + 1) * ph;
        }
    }
    let raw = match miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&idat, expected) {
        Ok(raw) => raw,
        Err(err) if !err.output.is_empty() => err.output,
        Err(_) => return Err("данные PNG повреждены"),
    };
    drop(idat);
    let mut out = Picture::alloc(width, height)?;
    // Unfilled rows of a cut file stay checkerboard.
    for y in 0..height {
        for x in 0..width {
            out.px[y * width + x] = packer.pack_alpha(0, 0, 0, 0, x, y);
        }
    }
    let wide = depth == 16;
    let key: Option<[u16; 3]> = match (color, trns.len()) {
        (0, n) if n >= 2 => Some([u16::from_be_bytes([trns[0], trns[1]]); 3]),
        (2, n) if n >= 6 => Some([u16::from_be_bytes([trns[0], trns[1]]), u16::from_be_bytes([trns[2], trns[3]]), u16::from_be_bytes([trns[4], trns[5]])]),
        _ => None,
    };
    let max_gray = (1u32 << depth.min(8)) - 1;
    let mut cursor = 0usize;
    for &(x0, y0, dx, dy) in &passes {
        let (pw, ph) = (width.saturating_sub(x0).div_ceil(dx), height.saturating_sub(y0).div_ceil(dy));
        if pw == 0 || ph == 0 {
            continue;
        }
        let stride = (pw * bits).div_ceil(8);
        let mut prev: Vec<u8> = alloc::vec![0; stride];
        let mut line: Vec<u8> = alloc::vec![0; stride];
        for row in 0..ph {
            if cursor + 1 + stride > raw.len() {
                return Ok((out, interlace == 1));
            }
            let filter = raw[cursor];
            line.copy_from_slice(&raw[cursor + 1..cursor + 1 + stride]);
            cursor += 1 + stride;
            for x in 0..stride {
                let a = if x >= bpp { line[x - bpp] } else { 0 };
                let b = prev[x];
                let c = if x >= bpp { prev[x - bpp] } else { 0 };
                let add = match filter {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                    4 => paeth(a, b, c),
                    _ => return Err("данные PNG повреждены"),
                };
                line[x] = line[x].wrapping_add(add);
            }
            prev.copy_from_slice(&line);
            let y = y0 + row * dy;
            for i in 0..pw {
                let x = x0 + i * dx;
                let sample16 = |n: usize| -> u16 {
                    if wide {
                        u16::from_be_bytes([line[(i * channels + n) * 2], line[(i * channels + n) * 2 + 1]])
                    } else {
                        u16::from(line[i * channels + n])
                    }
                };
                let sample = |n: usize| -> u8 { if wide { line[(i * channels + n) * 2] } else { line[i * channels + n] } };
                let small = |i: usize| -> u32 {
                    let per = 8 / depth as usize;
                    let byte = line[i / per];
                    let shift = (per - 1 - i % per) * depth as usize;
                    u32::from((byte >> shift) & ((1u16 << depth) - 1) as u8)
                };
                let (r, g, b, a) = match color {
                    0 => {
                        let raw_value = if depth < 8 { small(i) as u16 } else { sample16(0) };
                        let v = if depth < 8 { (small(i) * 255 / max_gray) as u8 } else { sample(0) };
                        let a = if key.is_some_and(|k| k[0] == raw_value) { 0 } else { 255 };
                        (v, v, v, a)
                    }
                    2 => {
                        let a = if key.is_some_and(|k| k == [sample16(0), sample16(1), sample16(2)]) { 0 } else { 255 };
                        (sample(0), sample(1), sample(2), a)
                    }
                    3 => {
                        let index = if depth == 8 { usize::from(line[i]) } else { small(i) as usize };
                        let rgb = palette.get(index * 3..index * 3 + 3).unwrap_or(&[0, 0, 0]);
                        (rgb[0], rgb[1], rgb[2], trns.get(index).copied().unwrap_or(255))
                    }
                    4 => (sample(0), sample(0), sample(0), sample(1)),
                    _ => (sample(0), sample(1), sample(2), sample(3)),
                };
                out.px[y * width + x] = packer.pack_alpha(r, g, b, a, x, y);
            }
        }
    }
    Ok((out, interlace == 1))
}

/// BMP without compression; `true` — `wallconv` reads it too (8/24/32 bits).
fn bmp(data: &[u8], packer: Packer) -> Result<(Picture, bool), &'static str> {
    if data.len() < 54 {
        return Err("файл BMP обрезан");
    }
    let offset = le32(data, 10) as usize;
    let header = le32(data, 14) as usize;
    let width = le32(data, 18) as i32;
    let raw_height = le32(data, 22) as i32;
    let bits = le16(data, 28);
    let compression = le32(data, 30);
    if !(compression == 0 || (compression == 3 && bits == 32)) {
        return Err("сжатый BMP не поддерживается");
    }
    if width <= 0 || raw_height == 0 || width as usize > MAX_SIDE || raw_height.unsigned_abs() as usize > MAX_SIDE {
        return Err("неверный размер BMP");
    }
    if !matches!(bits, 1 | 4 | 8 | 24 | 32) {
        return Err("неподдерживаемая глубина цвета BMP");
    }
    let (width, height) = (width as usize, raw_height.unsigned_abs() as usize);
    let top_down = raw_height < 0;
    let palette_at = 14 + header;
    let stride = (width * bits as usize).div_ceil(32) * 4;
    if data.len() < offset + stride * height {
        return Err("файл BMP обрезан");
    }
    let mut out = Picture::alloc(width, height)?;
    let entry = |index: usize| -> u32 {
        let at = palette_at + index * 4;
        let e = data.get(at..at + 3).unwrap_or(&[0, 0, 0]);
        packer.pack(e[2], e[1], e[0])
    };
    for y in 0..height {
        let row = if top_down { y } else { height - 1 - y };
        let line = &data[offset + row * stride..offset + (row + 1) * stride];
        let target = &mut out.px[y * width..(y + 1) * width];
        for (x, slot) in target.iter_mut().enumerate() {
            *slot = match bits {
                24 => packer.pack(line[x * 3 + 2], line[x * 3 + 1], line[x * 3]),
                32 => packer.pack(line[x * 4 + 2], line[x * 4 + 1], line[x * 4]),
                8 => entry(usize::from(line[x])),
                4 => entry(usize::from((line[x / 2] >> if x % 2 == 0 { 4 } else { 0 }) & 0x0F)),
                _ => entry(usize::from((line[x / 8] >> (7 - x % 8)) & 1)),
            };
        }
    }
    Ok((out, matches!(bits, 8 | 24 | 32)))
}

fn qoi(bytes: &[u8], packer: Packer) -> Result<Picture, &'static str> {
    if bytes.len() < 14 {
        return Err("файл QOI обрезан");
    }
    let width = be32(&bytes[4..]) as usize;
    let height = be32(&bytes[8..]) as usize;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err("неверный размер QOI");
    }
    let count = width * height;
    let mut out = Picture::alloc(width, height)?;
    let mut seen = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut at = 14;
    let mut done = 0;
    let get = |at: usize| bytes.get(at).copied().ok_or("файл QOI обрезан");
    while done < count {
        let op = get(at)?;
        at += 1;
        let mut run = 1;
        match op {
            0xFE => {
                px = [get(at)?, get(at + 1)?, get(at + 2)?, px[3]];
                at += 3;
            }
            0xFF => {
                px = [get(at)?, get(at + 1)?, get(at + 2)?, get(at + 3)?];
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
                    let next = get(at)?;
                    at += 1;
                    let dg = (op & 0x3F).wrapping_sub(32);
                    px[0] = px[0].wrapping_add(dg).wrapping_add(next >> 4).wrapping_sub(8);
                    px[1] = px[1].wrapping_add(dg);
                    px[2] = px[2].wrapping_add(dg).wrapping_add(next & 0x0F).wrapping_sub(8);
                }
                _ => run = usize::from(op & 0x3F) + 1,
            },
        }
        let slot = (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + usize::from(px[3]) * 11) % 64;
        seen[slot] = px;
        let run = run.min(count - done);
        for i in done..done + run {
            out.px[i] = packer.pack_alpha(px[0], px[1], px[2], px[3], i % width, i / width);
        }
        done += run;
    }
    Ok(out)
}

// ── GIF ─────────────────────────────────────────────────────────────────────

/// Sub-blocks from `at` joined; the position after the terminator.
fn sub_blocks(data: &[u8], mut at: usize, out: Option<&mut Vec<u8>>) -> Result<usize, &'static str> {
    let mut out = out;
    loop {
        let len = usize::from(*data.get(at).ok_or("файл GIF обрезан")?);
        at += 1;
        if len == 0 {
            return Ok(at);
        }
        let block = data.get(at..at + len).ok_or("файл GIF обрезан")?;
        if let Some(out) = out.as_deref_mut() {
            out.try_reserve(len).map_err(|_| "недостаточно памяти")?;
            out.extend_from_slice(block);
        }
        at += len;
    }
}

/// LZW of a GIF image into `count` colour indices.
fn lzw(data: &[u8], min_size: u8, count: usize) -> Result<Vec<u8>, &'static str> {
    if !(2..=11).contains(&min_size) {
        return Err("данные GIF повреждены");
    }
    let clear = 1usize << min_size;
    let end = clear + 1;
    let mut prefix = [0u16; 4096];
    let mut suffix = [0u8; 4096];
    let mut first = [0u8; 4096];
    for code in 0..clear {
        suffix[code] = code as u8;
        first[code] = code as u8;
    }
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(count).map_err(|_| "недостаточно памяти")?;
    let mut stack: Vec<u8> = Vec::with_capacity(4096);
    let mut size = u32::from(min_size) + 1;
    let mut next = clear + 2;
    let mut old: Option<usize> = None;
    let (mut bits, mut have) = (0u32, 0u32);
    let mut at = 0usize;
    while out.len() < count {
        while have < size {
            let Some(&byte) = data.get(at) else {
                // Short data: what decoded stays, the rest is index 0.
                out.resize(count, 0);
                return Ok(out);
            };
            bits |= u32::from(byte) << have;
            have += 8;
            at += 1;
        }
        let code = (bits & ((1 << size) - 1)) as usize;
        bits >>= size;
        have -= size;
        if code == clear {
            size = u32::from(min_size) + 1;
            next = clear + 2;
            old = None;
            continue;
        }
        if code == end {
            break;
        }
        let Some(prev) = old else {
            if code >= clear {
                return Err("данные GIF повреждены");
            }
            out.push(code as u8);
            old = Some(code);
            continue;
        };
        let (walk, extra) = if code < next {
            (code, None)
        } else if code == next {
            (prev, Some(first[prev]))
        } else {
            return Err("данные GIF повреждены");
        };
        stack.clear();
        let mut c = walk;
        while c >= clear {
            stack.push(suffix[c]);
            c = usize::from(prefix[c]);
        }
        stack.push(c as u8);
        for &byte in stack.iter().rev() {
            if out.len() < count {
                out.push(byte);
            }
        }
        if let Some(byte) = extra {
            if out.len() < count {
                out.push(byte);
            }
        }
        if next < 4096 {
            prefix[next] = prev as u16;
            suffix[next] = if code < next { first[walk] } else { first[prev] };
            first[next] = first[prev];
            next += 1;
            if next == (1 << size) && size < 12 {
                size += 1;
            }
        }
        old = Some(code);
    }
    out.resize(count, 0);
    Ok(out)
}

/// GIF: the first frame on the logical screen; how many frames there are.
fn gif(data: &[u8], packer: Packer) -> Result<(Picture, usize), &'static str> {
    if data.len() < 13 {
        return Err("файл GIF обрезан");
    }
    let (sw, sh) = (le16(data, 6) as usize, le16(data, 8) as usize);
    let flags = data[10];
    let mut at = 13;
    let mut global: &[u8] = &[];
    if flags & 0x80 != 0 {
        let len = 3 << ((flags & 7) + 1);
        global = data.get(at..at + len).ok_or("файл GIF обрезан")?;
        at += len;
    }
    let mut transparent: Option<u8> = None;
    let mut picture: Option<Picture> = None;
    let mut frames = 0usize;
    while at < data.len() {
        match data[at] {
            0x21 => {
                let label = *data.get(at + 1).ok_or("файл GIF обрезан")?;
                if label == 0xF9 && picture.is_none() {
                    if let Some(block) = data.get(at + 2..at + 7) {
                        transparent = (block[0] == 4 && block[1] & 1 != 0).then_some(block[4]);
                    }
                }
                at = sub_blocks(data, at + 2, None)?;
            }
            0x2C => {
                frames += 1;
                let d = data.get(at + 1..at + 10).ok_or("файл GIF обрезан")?;
                let (left, top, iw, ih) = (le16(d, 0) as usize, le16(d, 2) as usize, le16(d, 4) as usize, le16(d, 6) as usize);
                let iflags = d[8];
                at += 10;
                let mut table = global;
                if iflags & 0x80 != 0 {
                    let len = 3 << ((iflags & 7) + 1);
                    table = data.get(at..at + len).ok_or("файл GIF обрезан")?;
                    at += len;
                }
                let min_size = *data.get(at).ok_or("файл GIF обрезан")?;
                at += 1;
                if picture.is_some() {
                    at = sub_blocks(data, at, None)?;
                    continue;
                }
                let mut stream = Vec::new();
                at = sub_blocks(data, at, Some(&mut stream))?;
                let (w, h) = (sw.max(left + iw).max(1), sh.max(top + ih).max(1));
                if w > MAX_SIDE || h > MAX_SIDE {
                    return Err("неверный размер GIF");
                }
                let indices = lzw(&stream, min_size, iw * ih)?;
                let mut out = Picture::alloc(w, h)?;
                for y in 0..h {
                    for x in 0..w {
                        out.px[y * w + x] = packer.pack_alpha(0, 0, 0, 0, x, y);
                    }
                }
                // Interlaced rows come in four passes.
                let rows: Vec<usize> = if iflags & 0x40 != 0 {
                    let mut order = Vec::with_capacity(ih);
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        order.extend((start..ih).step_by(step));
                    }
                    order
                } else {
                    (0..ih).collect()
                };
                for (n, &row) in rows.iter().enumerate() {
                    let y = top + row;
                    for i in 0..iw {
                        let index = indices[n * iw + i];
                        if transparent == Some(index) {
                            continue;
                        }
                        let x = left + i;
                        let c = table.get(usize::from(index) * 3..usize::from(index) * 3 + 3).unwrap_or(&[0, 0, 0]);
                        out.px[y * w + x] = packer.pack(c[0], c[1], c[2]);
                    }
                }
                picture = Some(out);
            }
            0x3B => break,
            _ => break,
        }
    }
    picture.map(|p| (p, frames.max(1))).ok_or("в файле GIF нет кадров")
}

// ── Samples for the test bench ──────────────────────────────────────────────

/// Little-endian BMP, 24 bits, of an `R G B` picture.
#[must_use]
pub fn write_bmp(w: usize, h: usize, rgb: &[u8]) -> Vec<u8> {
    let stride = (w * 3).div_ceil(4) * 4;
    let size = 54 + stride * h;
    let mut out = Vec::with_capacity(size);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(size as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        for x in 0..w {
            let c = &rgb[(y * w + x) * 3..][..3];
            out.extend_from_slice(&[c[2], c[1], c[0]]);
        }
        out.resize(out.len() + stride - w * 3, 0);
    }
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// PNG, 8-bit RGBA.
#[must_use]
pub fn write_png(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w * 4 + 1) * h);
    for y in 0..h {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * w * 4..(y + 1) * w * 4]);
    }
    let packed = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk = |kind: &[u8], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let mut tagged = Vec::with_capacity(4 + body.len());
        tagged.extend_from_slice(kind);
        tagged.extend_from_slice(body);
        out.extend_from_slice(&tagged);
        out.extend_from_slice(&crc32(&tagged).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &packed);
    chunk(b"IEND", &[]);
    out
}

/// GIF89a with a 256-colour table and 8-bit indices, LZW without growth
/// (a clear code every 254 literals keeps codes 9 bits wide).
#[must_use]
pub fn write_gif(w: usize, h: usize, palette: &[[u8; 3]; 256], indices: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"GIF89a");
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.extend_from_slice(&[0xF7, 0, 0]);
    for c in palette {
        out.extend_from_slice(c);
    }
    out.push(0x2C);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    out.push(0);
    out.push(8);
    let mut stream: Vec<u8> = Vec::new();
    let (mut bits, mut have) = (0u32, 0u32);
    let mut put = |code: u32, stream: &mut Vec<u8>| {
        bits |= code << have;
        have += 9;
        while have >= 8 {
            stream.push(bits as u8);
            bits >>= 8;
            have -= 8;
        }
    };
    put(256, &mut stream);
    for (n, &index) in indices.iter().enumerate() {
        if n > 0 && n % 254 == 0 {
            put(256, &mut stream);
        }
        put(u32::from(index), &mut stream);
    }
    put(257, &mut stream);
    if have > 0 {
        stream.push(bits as u8);
    }
    for block in stream.chunks(255) {
        out.push(block.len() as u8);
        out.extend_from_slice(block);
    }
    out.push(0);
    out.push(0x3B);
    out
}
