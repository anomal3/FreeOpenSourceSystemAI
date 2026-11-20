//! `png2fimg ВХОД.png ВЫХОД.fimg` — значок PNG в FIMG стола (P12, 18.11.2026).
//!
//! Ярлыки программ Windows, поставленных в префикс Proton, `winemenubuilder`
//! пишет с картинками PNG (`drive_c/proton_shortcuts/icons/…`), а стол читает
//! только FIMG (`mini_ui::image`): разбирать PNG в ядре — это zlib и фильтры
//! строк над файлом из чужого установщика. Разбор здесь, в программе: упадёт —
//! упадёт она, а не стол. Стол запускает её сам, когда видит значок PNG без
//! FIMG рядом, и при следующем открытии «Пуска» показывает картинку.
//!
//! Пути — с `%20` вместо пробела (и `%25` вместо `%`): ядро делит строку запуска
//! по пробелам, а имена значков бывают с ними.
//!
//! Понимает то, что пишет WIC Wine и что бывает у значков: 8 и 16 бит на
//! канал, серый, RGB, палитра (1-8 бит, `tRNS`), серый и RGB с альфой; без
//! черезстрочности (Adam7) — такой файл отвергается.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use user_progs::{Args, close, exit, file_size, heap_size, open, open_write, println, read, write};

/// Самый большой PNG, который стоит читать (значки — десятки килобайт).
const FILE_LIMIT: usize = 4 * 1024 * 1024;

/// Самая большая сторона FIMG (`mini_ui::image`).
const MAX_SIDE: u32 = 512;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра по договору `_start`.
    let args = unsafe { Args::new(argc, argv) };
    // Файл, распакованные строки и точки — несколько мегабайт.
    heap_size(32 * 1024 * 1024);
    let (Some(input), Some(output)) = (args.get(1).map(unpct), args.get(2).map(unpct)) else {
        println("usage: png2fimg IN.png OUT.fimg (paths with %20 for spaces)");
        exit(2);
    };
    match convert(&input, &output) {
        Ok((width, height)) => {
            println(&alloc::format!("png2fimg: {output} {width}x{height}"));
            exit(0);
        }
        Err(why) => {
            println(&alloc::format!("png2fimg: {input}: {why}"));
            exit(1);
        }
    }
}

/// `%XX` — байт XX.
fn unpct(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if let Some(byte) = text.get(at + 1..at + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| String::from(text))
}

fn convert(input: &str, output: &str) -> Result<(u32, u32), &'static str> {
    let data = read_file(input)?;
    let (width, height, rgba) = decode(&data)?;
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err("bigger than 512x512");
    }
    let mut out = Vec::new();
    out.try_reserve_exact(8 + rgba.len()).map_err(|_| "out of memory")?;
    out.extend_from_slice(b"FIMG");
    out.extend_from_slice(&(width as u16).to_le_bytes());
    out.extend_from_slice(&(height as u16).to_le_bytes());
    out.extend_from_slice(&rgba);
    let fd = open_write(output, true, true);
    if fd < 0 {
        return Err("cannot create the output");
    }
    let wrote = write(fd, &out);
    close(fd);
    if wrote != out.len() as i64 {
        return Err("short write");
    }
    Ok((width, height))
}

fn read_file(path: &str) -> Result<Vec<u8>, &'static str> {
    let fd = open(path);
    if fd < 0 {
        return Err("cannot open");
    }
    let size = file_size(fd);
    if size <= 0 || size as usize > FILE_LIMIT {
        close(fd);
        return Err("empty or too big");
    }
    let mut data = Vec::new();
    if data.try_reserve_exact(size as usize).is_err() {
        close(fd);
        return Err("out of memory");
    }
    data.resize(size as usize, 0);
    let mut got = 0;
    while got < data.len() {
        let step = read(fd, &mut data[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    data.truncate(got);
    Ok(data)
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// PNG в точки `R G B A`.
fn decode(data: &[u8]) -> Result<(u32, u32, Vec<u8>), &'static str> {
    if data.get(..8) != Some(b"\x89PNG\r\n\x1a\n".as_slice()) {
        return Err("not a PNG");
    }
    let (mut width, mut height, mut depth, mut color) = (0u32, 0u32, 0u8, 0u8);
    let mut palette: &[u8] = &[];
    let mut trns: &[u8] = &[];
    let mut idat: Vec<u8> = Vec::new();
    let mut at = 8;
    while at + 8 <= data.len() {
        let len = be32(&data[at..]) as usize;
        let kind = &data[at + 4..at + 8];
        let body = data.get(at + 8..at + 8 + len).ok_or("truncated chunk")?;
        match kind {
            b"IHDR" if len >= 13 => {
                width = be32(body);
                height = be32(&body[4..]);
                depth = body[8];
                color = body[9];
                if body[12] != 0 {
                    return Err("interlaced");
                }
            }
            b"PLTE" => palette = body,
            b"tRNS" => trns = body,
            b"IDAT" => {
                idat.try_reserve(len).map_err(|_| "out of memory")?;
                idat.extend_from_slice(body);
            }
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 || height == 0 || width > 4096 || height > 4096 {
        return Err("bad size");
    }
    let channels: usize = match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return Err("bad color type"),
    };
    let ok_depth = match color {
        3 => matches!(depth, 1 | 2 | 4 | 8),
        0 => matches!(depth, 8 | 16),
        _ => matches!(depth, 8 | 16),
    };
    if !ok_depth {
        return Err("unsupported bit depth");
    }
    let bits = channels * depth as usize;
    let stride = (width as usize * bits).div_ceil(8);
    let bpp = bits.div_ceil(8).max(1);
    let expected = (stride + 1) * height as usize;
    let mut raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&idat, expected)
        .map_err(|_| "bad zlib data")?;
    if raw.len() < expected {
        return Err("short image data");
    }
    // Фильтры строк (PNG, раздел 9): каждая строка — байт фильтра и данные.
    let mut prev: Vec<u8> = alloc::vec![0; stride];
    let mut rows: Vec<u8> = Vec::new();
    rows.try_reserve_exact(stride * height as usize).map_err(|_| "out of memory")?;
    for y in 0..height as usize {
        let start = y * (stride + 1);
        let filter = raw[start];
        let line = &mut raw[start + 1..start + 1 + stride];
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
                _ => return Err("bad filter"),
            };
            line[x] = line[x].wrapping_add(add);
        }
        prev.copy_from_slice(line);
        rows.extend_from_slice(line);
    }
    // В точки RGBA: от 16 бит — старший байт.
    let mut rgba: Vec<u8> = Vec::new();
    rgba.try_reserve_exact(width as usize * height as usize * 4).map_err(|_| "out of memory")?;
    let wide = depth == 16;
    for y in 0..height as usize {
        let line = &rows[y * stride..(y + 1) * stride];
        for x in 0..width as usize {
            let sample = |n: usize| -> u8 { if wide { line[(x * channels + n) * 2] } else { line[x * channels + n] } };
            let px = match color {
                0 => {
                    let g = sample(0);
                    let alpha = if !wide && trns.len() >= 2 && u16::from(g) == u16::from_be_bytes([trns[0], trns[1]]) { 0 } else { 255 };
                    [g, g, g, alpha]
                }
                2 => [sample(0), sample(1), sample(2), 255],
                3 => {
                    let per = 8 / depth as usize;
                    let byte = line[x / per];
                    let shift = (per - 1 - x % per) * depth as usize;
                    let index = usize::from((byte >> shift) & ((1u16 << depth) - 1) as u8);
                    let rgb = palette.get(index * 3..index * 3 + 3).unwrap_or(&[0, 0, 0]);
                    [rgb[0], rgb[1], rgb[2], trns.get(index).copied().unwrap_or(255)]
                }
                4 => {
                    let g = sample(0);
                    [g, g, g, sample(1)]
                }
                _ => [sample(0), sample(1), sample(2), sample(3)],
            };
            rgba.extend_from_slice(&px);
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
