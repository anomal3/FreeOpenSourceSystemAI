//! Звук: одна дверь к звуковой карте, какой бы она ни была (фаза 68a).
//!
//! Карт две: virtio-snd (QEMU, фаза 67e) и Intel HDA (настоящие машины,
//! VirtualBox, QEMU с `intel-hda`). Обе принимают одно и то же — 16 бит со
//! знаком, два канала, 48 кГц — и задают темп сами; `beep` и `SYS_SOUND`
//! говорят только с этим модулем. Если есть обе, звук идёт в virtio-snd.

pub mod hda;

use crate::pci;
use crate::virtio::{self, VirtioError};

pub const SAMPLE_RATE: u32 = 48_000;

/// Байт в периоде — столько стоит отдавать за раз, чтобы не ждать зря.
pub const PERIOD: usize = 4096;

#[derive(Debug, Clone, Copy)]
pub enum SoundError {
    NoDevice,
    Virtio(VirtioError),
    Hda(hda::HdaError),
}

impl core::fmt::Display for SoundError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDevice => write!(f, "no sound device"),
            Self::Virtio(err) => write!(f, "virtio-snd: {err}"),
            Self::Hda(err) => write!(f, "hda: {err}"),
        }
    }
}

/// Найти звуковые карты.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц.
pub unsafe fn probe(root: &pci::Root) {
    // SAFETY: контракт функции.
    unsafe {
        virtio::snd::probe(root);
        hda::probe(root);
    }
}

/// Есть ли звук.
#[must_use]
pub fn present() -> bool {
    virtio::snd::present() || hda::present()
}

/// Отдать отсчёты карте: ждёт места, хвост меньше периода копится.
pub fn feed(bytes: &[u8]) -> Result<(), SoundError> {
    if virtio::snd::present() {
        virtio::snd::feed(bytes).map_err(SoundError::Virtio)
    } else if hda::present() {
        hda::feed(bytes).map_err(SoundError::Hda)
    } else {
        Err(SoundError::NoDevice)
    }
}

/// Доиграть всё отданное и остановить поток.
pub fn drain() -> Result<(), SoundError> {
    if virtio::snd::present() {
        virtio::snd::drain().map_err(SoundError::Virtio)
    } else if hda::present() {
        hda::drain().map_err(SoundError::Hda)
    } else {
        Err(SoundError::NoDevice)
    }
}

/// Сыграть синус `hz` длиной `ms` миллисекунд. Ждёт, пока карта сыграет всё:
/// звук задаёт темп сам.
pub fn play_tone(hz: u32, ms: u32) -> Result<(), SoundError> {
    let frames = u64::from(SAMPLE_RATE) * u64::from(ms) / 1000;
    let mut tone = Tone::new(hz);
    let mut period = [0u8; PERIOD];
    let mut left = frames;
    while left > 0 {
        let take = left.min((PERIOD / 4) as u64) as usize;
        for frame in 0..take {
            let value = tone.next().to_le_bytes();
            period[frame * 4..frame * 4 + 2].copy_from_slice(&value);
            period[frame * 4 + 2..frame * 4 + 4].copy_from_slice(&value);
        }
        if let Err(err) = feed(&period[..take * 4]) {
            let _ = drain();
            return Err(err);
        }
        left -= take as u64;
    }
    drain()
}

/// Синус на целых числах: фаза — 32-битный счётчик, отсчёт — по приближению
/// Бхаскары I, `16x(π−x) / (5π² − 4x(π−x))`. Погрешность — доли процента:
/// для гудка этого хватает с запасом, а плавающей точки в ядре нет.
struct Tone {
    phase: u32,
    step: u32,
}

impl Tone {
    fn new(hz: u32) -> Self {
        let step = ((u64::from(hz) << 32) / u64::from(SAMPLE_RATE)) as u32;
        Self { phase: 0, step }
    }

    /// Следующий отсчёт, громкость — четверть шкалы.
    fn next(&mut self) -> i16 {
        // Половина периода — `x` от 0 до π, в долях 1/65536 от π.
        let half = self.phase >> 31;
        let x = i64::from((self.phase >> 15) & 0xFFFF);
        let pi = 65_536i64;
        let product = x * (pi - x);
        let value = 16 * product * 8192 / (5 * pi * pi - 4 * product);
        self.phase = self.phase.wrapping_add(self.step);
        let value = value.clamp(0, i64::from(i16::MAX)) as i16;
        if half == 0 { value } else { -value }
    }
}
