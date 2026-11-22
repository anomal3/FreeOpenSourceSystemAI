//! `wallconv` — картинка в обои стола (22.11.2026).
//!
//! Стол рисуется ядром, а ядро читает только QOI (`mini_ui::qoi`): разбирать
//! JPEG и PNG в ядре — это таблицы Хаффмана, zlib и фильтры строк над файлом,
//! который человек принёс откуда угодно. Разбор здесь, в программе: упадёт —
//! упадёт она, а стол нарисует прежние обои.
//!
//! - `wallconv conv ВХОД ВЫХОД.qoi [Ш В]` — перевести картинку (галерея
//!   `/usr/share/wallpapers` или своя) под монитор Ш×В.
//! - `wallconv pick ПАПКА [Ш В]` — окно «Открыть» (`/bin/filedlg`) с
//!   картинками, выбранное — в `ПАПКА/<ключ>-<Ш>x<В>.qoi`, путь к нему —
//!   строкой в `ПАПКА/picked.txt`. Ключ — FNV-1a пути (как у ядра,
//!   `ui::wallpaper`).
//!
//! Понимает JPEG (базовый и прогрессивный, `zune-jpeg`), PNG (8/16 бит,
//! палитра, серый, альфа; без черезстрочности), BMP (24/32 бита, палитра 8
//! бит, без сжатия) и QOI. Больше, чем нужно, чтобы закрыть монитор (не
//! больше 4K, 3840×2160), картинка не хранится: уменьшается усреднением по
//! площади — один раз здесь, а не на каждом кадре; ядру (куча от 64 МиБ) не
//! приходится держать лишнего. Меньше — остаётся как есть (увеличивает ядро
//! под размер монитора).
//!
//! Пути — с `%20` вместо пробела (и `%25` вместо `%`): ядро делит строку
//! запуска по пробелам.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user_progs::filedlg::{Dialog, Filter, Options, Outcome};
use user_progs::{Args, close, exit, file_size, heap_size, mmap, munmap, open, open_write, println, read, remove, rename, write};

/// Самый большой файл, который стоит читать.
const FILE_LIMIT: usize = 64 * 1024 * 1024;

/// Самая большая сторона, которую берётся разобрать (точек).
const MAX_INPUT_SIDE: usize = 16_384;

/// Что картинка обязана закрыть целиком — экран 4K.
const COVER: (usize, usize) = (3840, 2160);

/// Больше этого по стороне ядро не распаковывает (`mini_ui::qoi`).
const MAX_SIDE: usize = 4096;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра по договору `_start`.
    let args = unsafe { Args::new(argc, argv) };
    // Куча — сколько даст ядро: файл, точки картинки 24 Мп и уменьшенная
    // копия — до трёх сотен мегабайт, а на машине с 512 МБ столько не дадут
    // (куча берётся одним куском, отказ — паника на первом же выделении).
    // Пробуем по убыванию, до первого выделения.
    for mib in [384usize, 256, 192, 128, 96, 64, 32] {
        let bytes = mib * 1024 * 1024;
        let base = mmap(bytes, 0);
        if base >= 0 {
            munmap(base as usize, bytes);
            heap_size(bytes);
            break;
        }
    }
    match args.get(1) {
        Some("conv") => {
            let (Some(input), Some(output)) = (args.get(2).map(unpct), args.get(3).map(unpct)) else {
                usage();
            };
            let cover = cover_arg(args.get(4), args.get(5));
            match convert(&input, &output, cover) {
                Ok(text) => {
                    println(&format!("wallconv: {input} -> {output} {text}"));
                    exit(0);
                }
                Err(why) => {
                    println(&format!("wallconv: {input}: {why}"));
                    exit(1);
                }
            }
        }
        Some("pick") => {
            let Some(dir) = args.get(2).map(unpct) else {
                usage();
            };
            let cover = cover_arg(args.get(3), args.get(4));
            exit(pick(&dir, cover));
        }
        _ => usage(),
    }
}

fn usage() -> ! {
    println("usage: wallconv conv IN OUT.qoi [W H] | wallconv pick DIR [W H] (paths with %20 for spaces)");
    exit(2);
}

/// Что картинка обязана закрыть: размер монитора от ядра, не больше 4K.
fn cover_arg(w: Option<&str>, h: Option<&str>) -> (usize, usize) {
    match (w.and_then(|w| w.parse::<usize>().ok()), h.and_then(|h| h.parse::<usize>().ok())) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w.min(COVER.0), h.min(COVER.1)),
        _ => COVER,
    }
}

/// «Обзор…»: окно выбора файла, затем перевод выбранного.
fn pick(dir: &str, cover: (usize, usize)) -> i64 {
    let mut options = Options::open();
    options.title = Some(String::from("Выберите картинку для фона"));
    // Начать с «Изображений», если такая папка есть.
    let home = user_progs::filedlg::home_dir();
    let pictures = format!("{home}/Pictures");
    let fd = open(&pictures);
    if fd >= 0 {
        close(fd);
        options.dir = Some(pictures);
    }
    options.filters = alloc::vec![
        Filter::new("Изображения", &["jpg", "jpeg", "png", "bmp", "qoi"]),
        Filter::all(),
    ];
    let dialog = match Dialog::start_owned(&options, 0, "wallconv") {
        Ok(dialog) => dialog,
        Err(why) => {
            println(&format!("wallconv: file dialog: {why}"));
            return 1;
        }
    };
    let path = match dialog.wait() {
        Outcome::Chosen(path) => path,
        _ => {
            println("wallconv: nothing picked");
            return 3;
        }
    };
    let output = format!("{dir}/{:016x}-{}x{}.qoi", fnv1a64(path.as_bytes()), cover.0, cover.1);
    match convert(&path, &output, cover) {
        Ok(text) => {
            let note = format!("{dir}/picked.txt");
            let fd = open_write(&note, true, true);
            if fd < 0 {
                println(&format!("wallconv: cannot write {note}"));
                return 1;
            }
            write(fd, path.as_bytes());
            close(fd);
            println(&format!("wallconv: picked {path} -> {output} {text}"));
            0
        }
        Err(why) => {
            println(&format!("wallconv: {path}: {why}"));
            1
        }
    }
}

/// FNV-1a, 64 бита — ключ кэша по пути (тот же счёт в ядре).
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
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

/// Картинка в точках `R G B`.
struct Rgb {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

fn convert(input: &str, output: &str, cover: (usize, usize)) -> Result<String, &'static str> {
    let file = read_file(input)?;
    let mut picture = decode(&file)?;
    drop(file);
    let (sw, sh) = (picture.width, picture.height);
    // Во сколько раз уменьшить: так, чтобы картинка всё ещё закрывала
    // монитор, но не длиннее предела ядра по стороне. Увеличивать — никогда.
    let (dw, dh) = target_size(sw, sh, cover);
    if (dw, dh) != (sw, sh) {
        picture = shrink(&picture, dw, dh)?;
    }
    let qoi = encode_qoi(&picture)?;
    drop(picture);
    let part = format!("{output}.part");
    let fd = open_write(&part, true, true);
    if fd < 0 {
        return Err("cannot create the output");
    }
    let mut sent = 0;
    while sent < qoi.len() {
        let step = write(fd, &qoi[sent..]);
        if step <= 0 {
            break;
        }
        sent += step as usize;
    }
    close(fd);
    if sent != qoi.len() {
        remove(&part);
        return Err("short write (disk full?)");
    }
    // Готовый файл появляется целиком: стол ждёт именно его имени.
    remove(output);
    if rename(&part, output) < 0 {
        remove(&part);
        return Err("cannot rename the output");
    }
    Ok(format!("{sw}x{sh} -> {dw}x{dh}, {} KiB", qoi.len() / 1024))
}

/// Размер, до которого уменьшить картинку `w`×`h`.
fn target_size(w: usize, h: usize, cover: (usize, usize)) -> (usize, usize) {
    // Множитель — дробью `num/den`: меньший из «закрыть монитор» и «влезть в
    // предел». Закрыть: max(W/w, H/h). Влезть: 4096/max(w, h).
    let (cover_num, cover_den) = if cover.0 * h >= cover.1 * w { (cover.0, w) } else { (cover.1, h) };
    let (fit_num, fit_den) = (MAX_SIDE, w.max(h));
    let (num, den) = if cover_num * fit_den <= fit_num * cover_den { (cover_num, cover_den) } else { (fit_num, fit_den) };
    if num >= den {
        return (w, h);
    }
    (((w * num + den / 2) / den).max(1), ((h * num + den / 2) / den).max(1))
}

fn read_file(path: &str) -> Result<Vec<u8>, &'static str> {
    let fd = open(path);
    if fd < 0 {
        return Err("cannot open");
    }
    let size = file_size(fd);
    if size <= 0 || size as usize > FILE_LIMIT {
        close(fd);
        return Err("empty or bigger than 64 MiB");
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

/// Разобрать по сигнатуре, а не по расширению: «.jpg», который на деле PNG, —
/// обычное дело у скачанных картинок.
fn decode(data: &[u8]) -> Result<Rgb, &'static str> {
    if data.starts_with(&[0xFF, 0xD8]) {
        decode_jpeg(data)
    } else if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        decode_png(data)
    } else if data.starts_with(b"BM") {
        decode_bmp(data)
    } else if data.starts_with(b"qoif") {
        decode_qoi(data)
    } else {
        Err("not a JPEG, PNG, BMP or QOI picture")
    }
}

fn alloc_rgb(width: usize, height: usize) -> Result<Vec<u8>, &'static str> {
    let mut data = Vec::new();
    data.try_reserve_exact(width * height * 3).map_err(|_| "out of memory")?;
    Ok(data)
}

fn decode_jpeg(data: &[u8]) -> Result<Rgb, &'static str> {
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(MAX_INPUT_SIDE)
        .set_max_height(MAX_INPUT_SIDE)
        .set_strict_mode(false);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(data, options);
    let pixels = decoder.decode().map_err(|_| "bad or unsupported JPEG")?;
    let (width, height) = decoder.dimensions().ok_or("bad JPEG size")?;
    if pixels.len() < width * height * 3 {
        return Err("short JPEG data");
    }
    Ok(Rgb { width, height, data: pixels })
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

/// PNG в точки `R G B` (прозрачное — по чёрному: обоям альфа не нужна).
fn decode_png(data: &[u8]) -> Result<Rgb, &'static str> {
    let (mut width, mut height, mut depth, mut color) = (0usize, 0usize, 0u8, 0u8);
    let mut palette: &[u8] = &[];
    let mut idat: Vec<u8> = Vec::new();
    let mut at = 8;
    while at + 8 <= data.len() {
        let len = be32(&data[at..]) as usize;
        let kind = &data[at + 4..at + 8];
        let body = data.get(at + 8..at + 8 + len).ok_or("truncated PNG chunk")?;
        match kind {
            b"IHDR" if len >= 13 => {
                width = be32(body) as usize;
                height = be32(&body[4..]) as usize;
                depth = body[8];
                color = body[9];
                if body[12] != 0 {
                    return Err("interlaced PNG is not supported");
                }
            }
            b"PLTE" => palette = body,
            b"IDAT" => {
                idat.try_reserve(len).map_err(|_| "out of memory")?;
                idat.extend_from_slice(body);
            }
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if width == 0 || height == 0 || width > MAX_INPUT_SIDE || height > MAX_INPUT_SIDE {
        return Err("bad PNG size");
    }
    let channels: usize = match color {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return Err("bad PNG color type"),
    };
    let ok_depth = if color == 3 { matches!(depth, 1 | 2 | 4 | 8) } else { matches!(depth, 8 | 16) };
    if !ok_depth {
        return Err("unsupported PNG bit depth");
    }
    let bits = channels * depth as usize;
    let stride = (width * bits).div_ceil(8);
    let bpp = bits.div_ceil(8).max(1);
    let expected = (stride + 1) * height;
    let mut raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&idat, expected).map_err(|_| "bad PNG zlib data")?;
    drop(idat);
    if raw.len() < expected {
        return Err("short PNG data");
    }
    let mut out = alloc_rgb(width, height)?;
    let mut prev: Vec<u8> = alloc::vec![0; stride];
    let wide = depth == 16;
    for y in 0..height {
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
                _ => return Err("bad PNG filter"),
            };
            line[x] = line[x].wrapping_add(add);
        }
        prev.copy_from_slice(line);
        for x in 0..width {
            let sample = |n: usize| -> u8 { if wide { line[(x * channels + n) * 2] } else { line[x * channels + n] } };
            let px = match color {
                0 => {
                    let g = sample(0);
                    [g, g, g]
                }
                2 => [sample(0), sample(1), sample(2)],
                3 => {
                    let per = 8 / depth as usize;
                    let byte = line[x / per];
                    let shift = (per - 1 - x % per) * depth as usize;
                    let index = usize::from((byte >> shift) & ((1u16 << depth) - 1) as u8);
                    let rgb = palette.get(index * 3..index * 3 + 3).unwrap_or(&[0, 0, 0]);
                    [rgb[0], rgb[1], rgb[2]]
                }
                4 => {
                    let (g, a) = (u16::from(sample(0)), u16::from(sample(1)));
                    let g = (g * a / 255) as u8;
                    [g, g, g]
                }
                _ => {
                    let a = u16::from(sample(3));
                    let k = |v: u8| (u16::from(v) * a / 255) as u8;
                    [k(sample(0)), k(sample(1)), k(sample(2))]
                }
            };
            out.extend_from_slice(&px);
        }
    }
    Ok(Rgb { width, height, data: out })
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

/// BMP без сжатия: 24 и 32 бита, палитра 8 бит; снизу вверх и сверху вниз.
fn decode_bmp(data: &[u8]) -> Result<Rgb, &'static str> {
    if data.len() < 54 {
        return Err("short BMP");
    }
    let offset = le32(data, 10) as usize;
    let header = le32(data, 14) as usize;
    let width = le32(data, 18) as i32;
    let raw_height = le32(data, 22) as i32;
    let bits = le16(data, 28);
    let compression = le32(data, 30);
    // BI_RGB (0) или BI_BITFIELDS (3) у 32 бит с обычным порядком.
    if !(compression == 0 || (compression == 3 && bits == 32)) {
        return Err("compressed BMP is not supported");
    }
    if width <= 0 || raw_height == 0 || width as usize > MAX_INPUT_SIDE || raw_height.unsigned_abs() as usize > MAX_INPUT_SIDE {
        return Err("bad BMP size");
    }
    let (width, height) = (width as usize, raw_height.unsigned_abs() as usize);
    let top_down = raw_height < 0;
    let palette_at = 14 + header;
    let stride = (width * bits as usize).div_ceil(32) * 4;
    if data.len() < offset + stride * height {
        return Err("short BMP data");
    }
    let mut out = alloc_rgb(width, height)?;
    for y in 0..height {
        let row = if top_down { y } else { height - 1 - y };
        let line = &data[offset + row * stride..offset + (row + 1) * stride];
        for x in 0..width {
            let px = match bits {
                24 => [line[x * 3 + 2], line[x * 3 + 1], line[x * 3]],
                32 => [line[x * 4 + 2], line[x * 4 + 1], line[x * 4]],
                8 => {
                    let at = palette_at + usize::from(line[x]) * 4;
                    let entry = data.get(at..at + 3).unwrap_or(&[0, 0, 0]);
                    [entry[2], entry[1], entry[0]]
                }
                _ => return Err("unsupported BMP bit depth"),
            };
            out.extend_from_slice(&px);
        }
    }
    Ok(Rgb { width, height, data: out })
}

/// QOI (3 или 4 канала; альфа отбрасывается).
fn decode_qoi(bytes: &[u8]) -> Result<Rgb, &'static str> {
    if bytes.len() < 14 {
        return Err("short QOI");
    }
    let width = be32(&bytes[4..]) as usize;
    let height = be32(&bytes[8..]) as usize;
    if width == 0 || height == 0 || width > MAX_INPUT_SIDE || height > MAX_INPUT_SIDE {
        return Err("bad QOI size");
    }
    let count = width * height;
    let mut out = alloc_rgb(width, height)?;
    let mut seen = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut at = 14;
    let mut done = 0;
    let get = |at: usize| bytes.get(at).copied().ok_or("short QOI data");
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
        for _ in 0..run {
            out.extend_from_slice(&px[..3]);
        }
        done += run;
    }
    Ok(Rgb { width, height, data: out })
}

/// Вклады точек источника в точки результата по одной оси: усреднение по
/// площади. Точка результата `d` накрывает отрезок `[d·src, (d+1)·src)` в
/// единицах `1/dst`, точка источника `s` — `[s·dst, (s+1)·dst)`; вес —
/// длина пересечения, сумма весов точки — `src` (ровно 1 после деления).
struct Taps {
    first: Vec<u32>,
    count: Vec<u32>,
    /// Веса в долях 1/65536, подряд для всех точек результата.
    weights: Vec<u32>,
    offsets: Vec<u32>,
}

fn taps(src: usize, dst: usize) -> Taps {
    let mut taps = Taps { first: Vec::with_capacity(dst), count: Vec::with_capacity(dst), weights: Vec::new(), offsets: Vec::with_capacity(dst) };
    for d in 0..dst {
        let (lo, hi) = (d * src, (d + 1) * src);
        let first = lo / dst;
        let last = (hi - 1) / dst;
        taps.first.push(first as u32);
        taps.count.push((last - first + 1) as u32);
        taps.offsets.push(taps.weights.len() as u32);
        let mut total = 0u32;
        for s in first..=last {
            let overlap = hi.min((s + 1) * dst) - lo.max(s * dst);
            let weight = ((overlap as u64 * 65_536) / src as u64) as u32;
            taps.weights.push(weight);
            total += weight;
        }
        // Остаток округления — самой большой доле, чтобы сумма была ровно 1.
        if let Some(fix) = taps.weights.last_mut() {
            *fix += 65_536 - total.min(65_536);
        }
    }
    taps
}

/// Уменьшить усреднением по площади: сначала строки, потом столбцы.
fn shrink(src: &Rgb, dw: usize, dh: usize) -> Result<Rgb, &'static str> {
    let across = taps(src.width, dw);
    let down = taps(src.height, dh);
    let mut wide = Vec::new();
    wide.try_reserve_exact(dw * src.height * 3).map_err(|_| "out of memory")?;
    for y in 0..src.height {
        let line = &src.data[y * src.width * 3..(y + 1) * src.width * 3];
        for d in 0..dw {
            let (first, count, at) = (across.first[d] as usize, across.count[d] as usize, across.offsets[d] as usize);
            let mut sum = [0u32; 3];
            for k in 0..count {
                let weight = across.weights[at + k];
                let px = &line[(first + k) * 3..(first + k) * 3 + 3];
                for c in 0..3 {
                    sum[c] += u32::from(px[c]) * weight;
                }
            }
            for value in sum {
                wide.push(((value + 32_768) >> 16).min(255) as u8);
            }
        }
    }
    let mut out = alloc_rgb(dw, dh)?;
    out.resize(dw * dh * 3, 0);
    let row = dw * 3;
    for d in 0..dh {
        let (first, count, at) = (down.first[d] as usize, down.count[d] as usize, down.offsets[d] as usize);
        let target = &mut out[d * row..(d + 1) * row];
        let mut sums: Vec<u32> = alloc::vec![0; row];
        for k in 0..count {
            let weight = down.weights[at + k];
            let line = &wide[(first + k) * row..(first + k + 1) * row];
            for (sum, value) in sums.iter_mut().zip(line) {
                *sum += u32::from(*value) * weight;
            }
        }
        for (slot, sum) in target.iter_mut().zip(sums) {
            *slot = ((sum + 32_768) >> 16).min(255) as u8;
        }
    }
    Ok(Rgb { width: dw, height: dh, data: out })
}

/// Закодировать в QOI с тремя каналами (как `xtask/src/wallpaper.rs`).
fn encode_qoi(picture: &Rgb) -> Result<Vec<u8>, &'static str> {
    const OP_INDEX: u8 = 0x00;
    const OP_DIFF: u8 = 0x40;
    const OP_LUMA: u8 = 0x80;
    const OP_RUN: u8 = 0xC0;
    const OP_RGB: u8 = 0xFE;
    let mut out = Vec::new();
    out.try_reserve(picture.data.len() / 2 + 64).map_err(|_| "out of memory")?;
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&(picture.width as u32).to_be_bytes());
    out.extend_from_slice(&(picture.height as u32).to_be_bytes());
    out.push(3);
    out.push(0);
    let mut seen = [[0u8; 3]; 64];
    let mut prev = [0u8, 0, 0];
    let mut run = 0u8;
    let count = picture.width * picture.height;
    for index in 0..count {
        let d = &picture.data[index * 3..index * 3 + 3];
        let px = [d[0], d[1], d[2]];
        if out.len() + 8 > out.capacity() {
            out.try_reserve(out.len() / 2 + 64).map_err(|_| "out of memory")?;
        }
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
        let slot = (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + 255 * 11) % 64;
        if seen[slot] == px {
            out.push(OP_INDEX | slot as u8);
        } else {
            seen[slot] = px;
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
        }
        prev = px;
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
    Ok(out)
}
