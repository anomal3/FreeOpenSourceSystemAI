//! Снимки экрана: PPM от QEMU → PNG на диске.
//!
//! Монитор умеет отдавать только PPM (см. [`super::monitor::Monitor::screendump`]),
//! а PPM не открывает почти ничто: ни просмотрщик Windows, ни браузер, ни
//! инструменты, которыми смотрят результат прогона. Снимок, который нельзя
//! посмотреть, — это отсутствующий снимок, поэтому перевод в PNG здесь не
//! удобство, а часть работы стенда.
//!
//! # Чего снимок не доказывает
//!
//! Он показывает **последний нарисованный** кадр, а не текущее состояние
//! системы. После падения на экране остаётся картинка той стадии, на которой всё
//! ещё было хорошо, и она бывает на два-три экрана позади журнала. Проверять по
//! снимку «система дошла до шага N» нельзя — это делает серийная линия; снимок
//! отвечает на другой вопрос: «как это выглядело».

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use anyhow::{Context, Result, bail};

/// Разобранный PPM.
struct Ppm {
    width: u32,
    height: u32,
    /// Пиксели RGB8 подряд, без выравнивания строк.
    pixels: Vec<u8>,
}

/// Очередное поле заголовка PPM.
fn token(bytes: &[u8], cursor: &mut usize) -> Result<String> {
    loop {
        // Пробелы между полями.
        while *cursor < bytes.len() && bytes[*cursor].is_ascii_whitespace() {
            *cursor += 1;
        }
        // Комментарий — до конца строки.
        if *cursor < bytes.len() && bytes[*cursor] == b'#' {
            while *cursor < bytes.len() && bytes[*cursor] != b'\n' {
                *cursor += 1;
            }
            continue;
        }
        break;
    }
    let start = *cursor;
    while *cursor < bytes.len() && !bytes[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
    if start == *cursor {
        bail!("файл кончился там, где ожидалось поле заголовка PPM");
    }
    Ok(String::from_utf8_lossy(&bytes[start..*cursor]).into_owned())
}

/// Разобрать бинарный PPM (`P6`).
///
/// Заголовок текстовый, тело — байты. Комментарии (`#`) спецификация разрешает
/// в любом месте заголовка; QEMU их не пишет, но парсер, который на них падает,
/// — это отложенный отказ на чужом файле.
fn parse_ppm(bytes: &[u8]) -> Result<Ppm> {
    let mut cursor = 0usize;

    let magic = token(bytes, &mut cursor)?;
    if magic != "P6" {
        bail!("это не бинарный PPM: сигнатура {magic:?}, ожидалась \"P6\"");
    }
    let width: u32 = token(bytes, &mut cursor)?
        .parse()
        .context("ширина PPM не число")?;
    let height: u32 = token(bytes, &mut cursor)?
        .parse()
        .context("высота PPM не число")?;
    let max: u32 = token(bytes, &mut cursor)?
        .parse()
        .context("максимум канала PPM не число")?;
    if max != 255 {
        bail!("PPM с максимумом канала {max}: стенд рассчитан на 8 бит");
    }
    // Ровно один разделитель между заголовком и телом — так велит формат.
    cursor += 1;

    let needed = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(3))
        .context("размеры PPM не помещаются в память")?;
    let body = bytes.get(cursor..).unwrap_or(&[]);
    if body.len() < needed {
        bail!(
            "PPM обрезан: {width}x{height} требует {needed} байт, есть {}",
            body.len()
        );
    }

    Ok(Ppm { width, height, pixels: body[..needed].to_vec() })
}

/// Перевести файл PPM в PNG рядом и удалить исходник.
pub fn ppm_to_png(ppm_path: &Path, png_path: &Path) -> Result<(u32, u32)> {
    let bytes = std::fs::read(ppm_path)
        .with_context(|| format!("не удалось прочитать снимок {}", ppm_path.display()))?;
    let image = parse_ppm(&bytes)
        .with_context(|| format!("не удалось разобрать снимок {}", ppm_path.display()))?;

    if let Some(parent) = png_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
    }
    let file = File::create(png_path)
        .with_context(|| format!("не удалось создать {}", png_path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), image.width, image.height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .with_context(|| format!("не удалось записать заголовок {}", png_path.display()))?;
    writer
        .write_image_data(&image.pixels)
        .with_context(|| format!("не удалось записать {}", png_path.display()))?;
    writer.finish().context("не удалось закрыть PNG")?;

    // Промежуточный PPM больше не нужен: он в двадцать раз больше результата и
    // ничем от него не отличается.
    std::fs::remove_file(ppm_path).ok();

    Ok((image.width, image.height))
}
