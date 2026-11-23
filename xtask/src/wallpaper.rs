//! Обои стола: PNG из макета → QOI, который ядро вшивает в себя
//! (`crates/kernel/src/ui/wallart.rs`).
//!
//! QOI, а не PNG: распаковщик QOI — сотня строк без таблиц Хаффмана, и
//! распаковывает он в разы быстрее, а ядру разбирать картинку при каждой смене
//! режима экрана. Сжимает хуже, но на плавных градиентах обоев — терпимо.
//! Формат: <https://qoiformat.org/qoi-specification.pdf>.

use std::path::Path;

use anyhow::{Context, Result};

/// Перевести обои `png` в QOI `out` (три канала, без альфы).
pub fn generate(png: &Path, out: &Path) -> Result<()> {
    let bytes = std::fs::read(png).with_context(|| format!("не прочитать {}", png.display()))?;
    let (width, height, rgba) = crate::fimg::decode(&bytes).with_context(|| format!("{}", png.display()))?;
    let qoi = encode(width, height, &rgba);
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(out, &qoi).with_context(|| format!("не записать {}", out.display()))?;
    println!(
        "обои: {} {width}x{height} → {} ({} КиБ, PNG {} КиБ)",
        png.display(),
        out.display(),
        qoi.len() / 1024,
        bytes.len() / 1024
    );
    Ok(())
}

/// Сторона миниатюры галереи: 16:9, с запасом под плитку «Параметров» при
/// масштабе 150 %.
const THUMB: (usize, usize) = (320, 180);

/// Миниатюры галереи обоев (22.11): для каждой картинки `dir/*.jpg|png` —
/// `<имя>.thumb.qoi` 320×180, «с обрезкой» (как обои «Заполнение») и
/// усреднением по площади. Сами картинки ядро не читает — их переводит
/// `/bin/wallconv` при выборе; миниатюры нужны «Параметрам» сразу.
pub fn gallery_thumbs(dir: &Path) -> Result<()> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("нет каталога галереи {}", dir.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jpg" || ext == "png"))
        .collect();
    names.sort();
    for path in names {
        let bytes = std::fs::read(&path).with_context(|| format!("не прочитать {}", path.display()))?;
        let (width, height, rgb) = if path.extension().is_some_and(|ext| ext == "jpg") {
            use zune_jpeg::zune_core::colorspace::ColorSpace;
            use zune_jpeg::zune_core::options::DecoderOptions;
            let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::RGB);
            let mut decoder = zune_jpeg::JpegDecoder::new_with_options(&bytes[..], options);
            let rgb = decoder.decode().map_err(|err| anyhow::anyhow!("{}: {err:?}", path.display()))?;
            let (width, height) = decoder.dimensions().context("JPEG без размера")?;
            (width, height, rgb)
        } else {
            let (width, height, rgba) = crate::fimg::decode(&bytes).with_context(|| format!("{}", path.display()))?;
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect();
            (width as usize, height as usize, rgb)
        };
        // Обрезка до 16:9 по середине, затем усреднение по площади.
        let (cw, ch) = if width * THUMB.1 > height * THUMB.0 { (height * THUMB.0 / THUMB.1, height) } else { (width, width * THUMB.1 / THUMB.0) };
        let (x0, y0) = ((width - cw) / 2, (height - ch) / 2);
        let mut rgba = Vec::with_capacity(THUMB.0 * THUMB.1 * 4);
        for ty in 0..THUMB.1 {
            let (sy0, sy1) = (y0 + ty * ch / THUMB.1, (y0 + (ty + 1) * ch / THUMB.1).max(y0 + ty * ch / THUMB.1 + 1));
            for tx in 0..THUMB.0 {
                let (sx0, sx1) = (x0 + tx * cw / THUMB.0, (x0 + (tx + 1) * cw / THUMB.0).max(x0 + tx * cw / THUMB.0 + 1));
                let mut sum = [0u64; 3];
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let at = (sy * width + sx) * 3;
                        for c in 0..3 {
                            sum[c] += u64::from(rgb[at + c]);
                        }
                    }
                }
                let n = ((sy1 - sy0) * (sx1 - sx0)) as u64;
                rgba.extend_from_slice(&[(sum[0] / n) as u8, (sum[1] / n) as u8, (sum[2] / n) as u8, 255]);
            }
        }
        let qoi = encode(THUMB.0 as u32, THUMB.1 as u32, &rgba);
        let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or("wallpaper");
        let out = dir.join(format!("{stem}.thumb.qoi"));
        std::fs::write(&out, &qoi).with_context(|| format!("не записать {}", out.display()))?;
        println!("галерея: {} {width}x{height} → {} ({} КиБ)", path.display(), out.display(), qoi.len() / 1024);
    }
    Ok(())
}

/// Закодировать `RGBA` в QOI с тремя каналами: альфа отбрасывается.
#[must_use]
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    const OP_INDEX: u8 = 0x00;
    const OP_DIFF: u8 = 0x40;
    const OP_LUMA: u8 = 0x80;
    const OP_RUN: u8 = 0xC0;
    const OP_RGB: u8 = 0xFE;

    let mut out = Vec::with_capacity(rgba.len() / 2);
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&width.to_be_bytes());
    out.extend_from_slice(&height.to_be_bytes());
    out.push(3);
    out.push(0);

    let mut seen = [[0u8; 3]; 64];
    let mut prev = [0u8, 0, 0];
    let mut run = 0u8;
    let count = (width * height) as usize;
    for index in 0..count {
        let px = [rgba[index * 4], rgba[index * 4 + 1], rgba[index * 4 + 2]];
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
        let slot = hash(px);
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
    out
}

/// Место точки в таблице недавних: формула спецификации (альфа — 255).
fn hash(px: [u8; 3]) -> usize {
    (usize::from(px[0]) * 3 + usize::from(px[1]) * 5 + usize::from(px[2]) * 7 + 255 * 11) % 64
}
