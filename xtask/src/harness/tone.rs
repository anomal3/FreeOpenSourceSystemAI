//! Тон в звуке гостя (фаза 67e).
//!
//! QEMU пишет всё, что гость сыграл, в файл WAV (`-audiodev wav`), и проверка
//! звука сводится к вопросу: есть ли там тон нужной частоты. Частота меряется
//! по пересечениям нуля снизу вверх на самом громком участке — без
//! преобразования Фурье: тон один и чистый, а ложное срабатывание исключает
//! само требование громкости.
//!
//! Заголовок читается только ради формата (частота, каналы, разрядность):
//! длины в нём QEMU дописывает при выходе, а файл читается, пока машина
//! работает, — поэтому данные берутся от метки `data` до конца файла.

use std::path::Path;

use anyhow::{Context, Result, bail};

/// Окно, по которому меряется громкость, — в кадрах.
const WINDOW: usize = 1024;

/// Левый канал звука гостя и частота кадров.
fn left_channel(path: &Path) -> Result<(Vec<i32>, u32)> {
    let bytes = std::fs::read(path).with_context(|| format!("нет звука гостя {}", path.display()))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("{} — не WAV", path.display());
    }
    let channels = usize::from(u16::from_le_bytes([bytes[22], bytes[23]]).max(1));
    let rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
    let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
    if bits != 16 || rate == 0 {
        bail!("WAV {rate} Гц, {bits} бит: стенд читает только 16 бит");
    }
    let data = bytes
        .windows(4)
        .position(|tag| tag == b"data")
        .map(|at| at + 8)
        .context("в WAV нет данных")?;
    // Левый канал.
    let samples: Vec<i32> = bytes[data..]
        .chunks_exact(2 * channels)
        .map(|frame| i32::from(i16::from_le_bytes([frame[0], frame[1]])))
        .collect();
    Ok((samples, rate))
}

/// Громкость окнами по [`WINDOW`] кадров: среднее абсолютное значение.
fn loudness(samples: &[i32]) -> Vec<i64> {
    samples
        .chunks(WINDOW)
        .map(|window| window.iter().map(|value| i64::from(value.abs())).sum::<i64>() / window.len() as i64)
        .collect()
}

/// Сколько миллисекунд в звуке гостя слышно хоть что-то (фаза 68e): окна,
/// где средняя громкость не ниже `floor`. Для звуков игры, у которых нет
/// одного тона, — выстрел, шаги, щелчок меню.
pub fn heard(path: &Path, floor: i64) -> Result<u64> {
    let (samples, rate) = left_channel(path)?;
    let loud = loudness(&samples).iter().filter(|value| **value >= floor).count() as u64;
    Ok(loud * WINDOW as u64 * 1000 / u64::from(rate.max(1)))
}

/// Самый громкий участок: частота тона на нём и его длительность в мс.
pub fn dominant(path: &Path) -> Result<(u32, u64)> {
    let (samples, rate) = left_channel(path)?;
    if samples.len() < WINDOW * 4 {
        bail!("в звуке гостя {} отсчётов — почти ничего", samples.len());
    }

    // Громкость окнами, потом — участок, где она не ниже трети наибольшей.
    let loudness = loudness(&samples);
    let peak = loudness.iter().copied().max().unwrap_or(0);
    if peak < 500 {
        bail!("в звуке гостя тишина (средняя громкость не выше {peak})");
    }
    let loud: Vec<usize> = loudness
        .iter()
        .enumerate()
        .filter(|(_, value)| **value * 3 >= peak)
        .map(|(index, _)| index)
        .collect();
    let first = loud.first().copied().unwrap_or(0) * WINDOW;
    let last = (loud.last().copied().unwrap_or(0) + 1) * WINDOW;
    let span = &samples[first..last.min(samples.len())];

    let crossings = span.windows(2).filter(|pair| pair[0] < 0 && pair[1] >= 0).count() as u64;
    let seconds_x1000 = span.len() as u64 * 1000 / u64::from(rate);
    if seconds_x1000 == 0 {
        bail!("громкий участок пуст");
    }
    let hz = (crossings * 1000 / seconds_x1000) as u32;
    Ok((hz, seconds_x1000))
}
