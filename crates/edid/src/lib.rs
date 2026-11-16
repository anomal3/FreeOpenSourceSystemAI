// Copyright (C) 2026 Виталий Ушаков, Роман Ощепков
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! EDID: что монитор о себе рассказывает (16.11.2026, мониторы «как в
//! Windows»: разрешение и частоту выбирают из того, что принимает монитор).
//!
//! # Что разбирается
//!
//! Базовый блок VESA E-EDID 1.3/1.4: производитель и модель, установленные
//! (`established`) и стандартные таймингы, четыре дескриптора — подробные
//! таймингы (DTD) и пределы частот; расширения CEA-861 (свои DTD и коды VIC —
//! телевизоры, 4K, 120 Гц) и DisplayID (подробные таймингы type I/VII — там
//! мониторы прячут 144/165/240 Гц на больших разрешениях).
//!
//! # Таймингы, а не только размеры
//!
//! Драйверу нужен полный тайминг: полные длины строк и кадров, синхроимпульсы
//! и частота точек. У DTD он записан как есть; у установленных и стандартных
//! режимов — только размер и частота, и тайминг строится по формуле VESA CVT
//! (с сокращённым гашением, где это допустимо), как делают Linux и Windows.
//! Частота кадров считается из тайминга точно, в тысячных герца: 59 940 мГц и
//! 60 000 — разные режимы, и монитор принимает именно тот, что записан.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Один режим монитора: всё, что нужно, чтобы его выставить.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Частота точек, кГц.
    pub clock_khz: u32,
    pub h_sync_start: u32,
    pub h_sync_end: u32,
    pub h_total: u32,
    pub v_sync_start: u32,
    pub v_sync_end: u32,
    pub v_total: u32,
    pub h_sync_positive: bool,
    pub v_sync_positive: bool,
    pub interlaced: bool,
    /// Родной режим монитора (первый DTD при флаге «preferred»).
    pub preferred: bool,
    /// Откуда взят — для журнала.
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Detailed,
    Established,
    Standard,
    Cea,
    DisplayId,
}

impl Mode {
    /// Частота кадров в тысячных герца.
    #[must_use]
    pub fn refresh_mhz(&self) -> u32 {
        let frame = u64::from(self.h_total) * u64::from(self.v_total);
        if frame == 0 {
            return 0;
        }
        let mut mhz = u64::from(self.clock_khz) * 1_000_000 / frame;
        if self.interlaced {
            mhz *= 2;
        }
        mhz as u32
    }

    /// Частота кадров, округлённая до целых герц (для списка в настройках).
    #[must_use]
    pub fn refresh_hz(&self) -> u32 {
        (self.refresh_mhz() + 500) / 1000
    }

    /// Тот же экран: размер и частота (с точностью до 0,5 Гц) совпадают.
    #[must_use]
    pub fn same_as(&self, other: &Mode) -> bool {
        self.width == other.width
            && self.height == other.height
            && self.interlaced == other.interlaced
            && self.refresh_mhz().abs_diff(other.refresh_mhz()) < 500
    }
}

/// Что известно о мониторе.
#[derive(Clone, Debug, Default)]
pub struct Monitor {
    /// Три буквы производителя (`DEL`, `BNQ`, `SAM`…).
    pub maker: [u8; 3],
    pub product: u16,
    pub serial: u32,
    /// Имя из дескриптора 0xFC.
    pub name: String,
    /// Серийный номер строкой (0xFF).
    pub serial_text: String,
    /// Физический размер, мм.
    pub width_mm: u32,
    pub height_mm: u32,
    /// Пределы из дескриптора 0xFD: кадров (Гц) и строк (кГц), частота точек (МГц).
    pub v_range: (u32, u32),
    pub h_range_khz: (u32, u32),
    pub max_clock_mhz: u32,
    /// Все режимы: родной первым, дальше — от большего к меньшему.
    pub modes: Vec<Mode>,
    /// EDID 1.4 и цифровой вход.
    pub digital: bool,
}

impl Monitor {
    /// Производитель тремя буквами.
    #[must_use]
    pub fn maker_str(&self) -> &str {
        core::str::from_utf8(&self.maker).unwrap_or("???")
    }

    /// Родной режим: помеченный, иначе самый большой.
    #[must_use]
    pub fn preferred(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.preferred).or_else(|| self.modes.first())
    }

    /// Разрешения без повторов, от большего к меньшему.
    #[must_use]
    pub fn resolutions(&self) -> Vec<(u32, u32)> {
        let mut out: Vec<(u32, u32)> = Vec::new();
        for mode in &self.modes {
            if !out.contains(&(mode.width, mode.height)) {
                out.push((mode.width, mode.height));
            }
        }
        out
    }

    /// Частоты (мГц) для разрешения, от большей к меньшей.
    #[must_use]
    pub fn refresh_rates(&self, width: u32, height: u32) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        for mode in self.modes.iter().filter(|m| m.width == width && m.height == height && !m.interlaced) {
            let r = mode.refresh_mhz();
            if !out.iter().any(|x| x.abs_diff(r) < 500) {
                out.push(r);
            }
        }
        out.sort_unstable_by(|a, b| b.cmp(a));
        out
    }

    /// Режим по размеру и частоте (ближайший по частоте).
    #[must_use]
    pub fn find(&self, width: u32, height: u32, refresh_mhz: u32) -> Option<&Mode> {
        self.modes
            .iter()
            .filter(|m| m.width == width && m.height == height && !m.interlaced)
            .min_by_key(|m| m.refresh_mhz().abs_diff(refresh_mhz))
    }
}

/// Почему EDID не разобран.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    TooShort,
    BadHeader,
    BadChecksum,
}

const HEADER: [u8; 8] = [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0];

/// Разобрать EDID (базовый блок и расширения, сколько есть в `bytes`).
pub fn parse(bytes: &[u8]) -> Result<Monitor, Error> {
    if bytes.len() < 128 {
        return Err(Error::TooShort);
    }
    if bytes[..8] != HEADER {
        return Err(Error::BadHeader);
    }
    if !checksum_ok(&bytes[..128]) {
        return Err(Error::BadChecksum);
    }
    let b = &bytes[..128];
    let mut m = Monitor::default();
    let id = u16::from_be_bytes([b[8], b[9]]);
    m.maker = [
        b'@' + ((id >> 10) & 31) as u8,
        b'@' + ((id >> 5) & 31) as u8,
        b'@' + (id & 31) as u8,
    ];
    m.product = u16::from_le_bytes([b[10], b[11]]);
    m.serial = u32::from_le_bytes([b[12], b[13], b[14], b[15]]);
    m.digital = b[20] & 0x80 != 0;
    m.width_mm = u32::from(b[21]) * 10;
    m.height_mm = u32::from(b[22]) * 10;
    let preferred_flag = b[24] & 0x02 != 0 || b[18] >= 1 && b[19] >= 4;
    let mut modes = Vec::new();

    // Четыре дескриптора по 18 байт.
    for (index, d) in b[54..126].chunks_exact(18).enumerate() {
        if d[0] != 0 || d[1] != 0 {
            if let Some(mut mode) = detailed(d, Source::Detailed) {
                mode.preferred = index == 0 && preferred_flag;
                modes.push(mode);
            }
            continue;
        }
        match d[3] {
            0xfc => m.name = text(&d[5..18]),
            0xff => m.serial_text = text(&d[5..18]),
            0xfd => {
                // Пределы; в EDID 1.4 биты 0..3 байта 4 дают +255.
                let add = |bit: u8| if d[4] & bit != 0 { 255 } else { 0 };
                m.v_range = (u32::from(d[5]) + add(0x01), u32::from(d[6]) + add(0x02));
                m.h_range_khz = (u32::from(d[7]) + add(0x04), u32::from(d[8]) + add(0x08));
                m.max_clock_mhz = u32::from(d[9]) * 10;
            }
            0xf7 => {
                // Ещё установленные режимы (EDID 1.4, «Established Timings III»).
                for (byte, bits) in d[6..12].iter().enumerate() {
                    for bit in 0..8 {
                        if bits & (0x80 >> bit) != 0 {
                            if let Some(&(w, h, r)) = ESTABLISHED_III.get(byte * 8 + bit) {
                                modes.push(cvt(w, h, r, Source::Established));
                            }
                        }
                    }
                }
            }
            0xfa => {
                for code in d[5..17].chunks_exact(2) {
                    if let Some(mode) = standard(code[0], code[1], b[19]) {
                        modes.push(mode);
                    }
                }
            }
            _ => {}
        }
    }

    // Установленные режимы (байты 35–37).
    for (bit, &(w, h, r)) in ESTABLISHED.iter().enumerate() {
        let byte = 35 + bit / 8;
        if b[byte] & (0x80 >> (bit % 8)) != 0 && w != 0 {
            modes.push(dmt_or_cvt(w, h, r, Source::Established));
        }
    }
    // Стандартные (38–53).
    for code in b[38..54].chunks_exact(2) {
        if let Some(mode) = standard(code[0], code[1], b[19]) {
            modes.push(mode);
        }
    }

    // Расширения.
    let count = usize::from(b[126]);
    for block in 1..=count {
        let Some(e) = bytes.get(block * 128..block * 128 + 128) else { break };
        if !checksum_ok(e) {
            continue;
        }
        match e[0] {
            0x02 => cea(e, &mut modes),
            0x70 => displayid(e, &mut modes),
            _ => {}
        }
    }

    // Без повторов; родной первым, дальше — больше и чаще раньше.
    let mut unique: Vec<Mode> = Vec::new();
    for mode in modes {
        if mode.width == 0 || mode.height == 0 || mode.clock_khz == 0 {
            continue;
        }
        if let Some(have) = unique.iter_mut().find(|u| u.same_as(&mode)) {
            // Подробный тайминг важнее вычисленного по формуле.
            if have.source != Source::Detailed && mode.source == Source::Detailed {
                let preferred = have.preferred || mode.preferred;
                *have = mode;
                have.preferred = preferred;
            } else {
                have.preferred |= mode.preferred;
            }
            continue;
        }
        unique.push(mode);
    }
    unique.sort_by(|a, b| {
        b.preferred
            .cmp(&a.preferred)
            .then((u64::from(b.width) * u64::from(b.height)).cmp(&(u64::from(a.width) * u64::from(a.height))))
            .then(b.width.cmp(&a.width))
            .then(b.refresh_mhz().cmp(&a.refresh_mhz()))
    });
    m.modes = unique;
    Ok(m)
}

fn checksum_ok(block: &[u8]) -> bool {
    block.iter().fold(0u8, |a, b| a.wrapping_add(*b)) == 0
}

fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == b'\n').unwrap_or(bytes.len());
    bytes[..end].iter().map(|b| if b.is_ascii_graphic() || *b == b' ' { char::from(*b) } else { '?' }).collect::<String>().trim().into()
}

/// Подробный тайминг (18 байт: EDID DTD и CEA).
fn detailed(d: &[u8], source: Source) -> Option<Mode> {
    let clock = u32::from(u16::from_le_bytes([d[0], d[1]])) * 10;
    if clock == 0 {
        return None;
    }
    let ha = u32::from(d[2]) | (u32::from(d[4] & 0xf0) << 4);
    let hb = u32::from(d[3]) | (u32::from(d[4] & 0x0f) << 8);
    let va = u32::from(d[5]) | (u32::from(d[7] & 0xf0) << 4);
    let vb = u32::from(d[6]) | (u32::from(d[7] & 0x0f) << 8);
    let hso = u32::from(d[8]) | (u32::from(d[11] & 0xc0) << 2);
    let hsw = u32::from(d[9]) | (u32::from(d[11] & 0x30) << 4);
    let vso = u32::from(d[10] >> 4) | (u32::from(d[11] & 0x0c) << 2);
    let vsw = u32::from(d[10] & 0x0f) | (u32::from(d[11] & 0x03) << 4);
    let flags = d[17];
    let interlaced = flags & 0x80 != 0;
    // Цифровая раздельная синхронизация: полярности в битах 2 и 1.
    let (hpos, vpos) = if flags & 0x18 == 0x18 { (flags & 0x02 != 0, flags & 0x04 != 0) } else { (false, false) };
    let (va, vb, vso, vsw) = if interlaced { (va * 2, vb * 2 + 1, vso * 2, vsw * 2) } else { (va, vb, vso, vsw) };
    Some(Mode {
        width: ha,
        height: va,
        clock_khz: clock,
        h_sync_start: ha + hso,
        h_sync_end: ha + hso + hsw,
        h_total: ha + hb,
        v_sync_start: va + vso,
        v_sync_end: va + vso + vsw,
        v_total: va + vb,
        h_sync_positive: hpos,
        v_sync_positive: vpos,
        interlaced,
        preferred: false,
        source,
    })
}

/// Стандартный тайминг: размер по ширине и пропорции, частота.
fn standard(a: u8, b: u8, revision: u8) -> Option<Mode> {
    if (a == 1 && b == 1) || a == 0 {
        return None;
    }
    let w = (u32::from(a) + 31) * 8;
    let h = match b >> 6 {
        0 if revision >= 3 => w * 10 / 16,
        0 => w,
        1 => w * 3 / 4,
        2 => w * 4 / 5,
        _ => w * 9 / 16,
    };
    let r = u32::from(b & 0x3f) + 60;
    Some(dmt_or_cvt(w, h, r, Source::Standard))
}

/// Известный режим VESA DMT, иначе — формула CVT.
fn dmt_or_cvt(w: u32, h: u32, r: u32, source: Source) -> Mode {
    for t in DMT {
        if t.0 == w && t.1 == h && t.2 == r {
            return Mode {
                width: w,
                height: h,
                clock_khz: t.3,
                h_sync_start: t.4,
                h_sync_end: t.5,
                h_total: t.6,
                v_sync_start: t.7,
                v_sync_end: t.8,
                v_total: t.9,
                h_sync_positive: t.10,
                v_sync_positive: t.11,
                interlaced: false,
                preferred: false,
                source,
            };
        }
    }
    cvt(w, h, r, source)
}

/// Тайминг по VESA CVT 1.2: сокращённое гашение (RB) для 60 Гц и кратных
/// ему частот у плоских панелей, иначе — обычный CVT.
#[must_use]
pub fn cvt(w: u32, h: u32, refresh: u32, source: Source) -> Mode {
    let reduced = refresh % 60 == 0 || refresh >= 100;
    let mut mode = if reduced { cvt_rb(w, h, refresh) } else { cvt_standard(w, h, refresh) };
    mode.source = source;
    mode
}

fn cvt_rb(w: u32, h: u32, refresh: u32) -> Mode {
    // CVT-RB (v1): гашение строки 160 точек, синхро 32; кадр — 460 мкс.
    const H_BLANK: u32 = 160;
    const H_SYNC: u32 = 32;
    const V_FRONT: u32 = 3;
    const MIN_V_BLANK_US: u64 = 460;
    let v_sync = vsync_width(w, h);
    let h_active = w / 8 * 8;
    // Период строки (нс*1000) = (1e6/refresh - 460) / v_active.
    let frame_us = 1_000_000_000u64 / u64::from(refresh); // в наносекундах
    let line_ns = (frame_us - MIN_V_BLANK_US * 1000) * 1000 / u64::from(h); // пикосекунды
    let v_blank_lines = (MIN_V_BLANK_US * 1_000_000 / line_ns + 1).max(u64::from(V_FRONT + v_sync + 6)) as u32;
    let h_total = h_active + H_BLANK;
    let v_total = h + v_blank_lines;
    // Частота точек шагом 250 кГц.
    let clock_khz = (u64::from(h_total) * u64::from(v_total) * u64::from(refresh) / 1000 / 250 * 250) as u32;
    Mode {
        width: h_active,
        height: h,
        clock_khz,
        h_sync_start: h_active + 48,
        h_sync_end: h_active + 48 + H_SYNC,
        h_total,
        v_sync_start: h + V_FRONT,
        v_sync_end: h + V_FRONT + v_sync,
        v_total,
        h_sync_positive: true,
        v_sync_positive: false,
        interlaced: false,
        preferred: false,
        source: Source::Standard,
    }
}

fn cvt_standard(w: u32, h: u32, refresh: u32) -> Mode {
    // Обычный CVT: минимальное время обратного хода 550 мкс, C'=30, M'=300.
    const MIN_VSYNC_BP_US: u64 = 550;
    const V_FRONT: u32 = 3;
    let v_sync = vsync_width(w, h);
    let h_active = w / 8 * 8;
    // Период строки, пс.
    let frame_ps = 1_000_000_000_000u64 / u64::from(refresh);
    let line_ps = (frame_ps - MIN_VSYNC_BP_US * 1_000_000) / u64::from(h + V_FRONT);
    let vsync_bp = ((MIN_VSYNC_BP_US * 1_000_000 / line_ps) + 1) as u32;
    let v_total = h + V_FRONT + vsync_bp.max(v_sync + 6);
    // Идеальная доля гашения: C' - M' * H_period(мкс) / 1000, в процентах.
    let duty_permille = (300i64 * 1000 - 300 * (line_ps as i64) / 1000) / 1000;
    let duty_permille = duty_permille.clamp(200, 300);
    let mut h_blank = (u64::from(h_active) * duty_permille as u64 / (1000 - duty_permille as u64)) as u32;
    h_blank = (h_blank / 16) * 16;
    let h_total = h_active + h_blank;
    let h_sync = (h_total * 8 / 100) / 8 * 8;
    let clock_khz = ((u64::from(h_total) * 1_000_000_000 / line_ps) / 250 * 250) as u32;
    Mode {
        width: h_active,
        height: h,
        clock_khz,
        h_sync_start: h_active + h_blank / 2 - h_sync,
        h_sync_end: h_active + h_blank / 2,
        h_total,
        v_sync_start: h + V_FRONT,
        v_sync_end: h + V_FRONT + v_sync,
        v_total,
        h_sync_positive: false,
        v_sync_positive: true,
        interlaced: false,
        preferred: false,
        source: Source::Standard,
    }
}

/// Ширина вертикального синхроимпульса CVT по пропорции.
fn vsync_width(w: u32, h: u32) -> u32 {
    match (w * 1000 / h.max(1), w, h) {
        (1333, ..) => 4,
        (1777 | 1778, ..) => 5,
        (1600, ..) => 6,
        (1250, ..) => 7,
        (1666 | 1667, ..) => 7,
        _ => 10,
    }
}

/// CEA-861: свои DTD и короткие описания видео (VIC).
fn cea(e: &[u8], modes: &mut Vec<Mode>) {
    let dtd_start = usize::from(e[2]);
    if dtd_start >= 4 && e[1] >= 3 {
        let mut at = 4;
        while at < dtd_start.min(127) {
            let tag = e[at] >> 5;
            let len = usize::from(e[at] & 0x1f);
            if tag == 2 {
                for svd in e.get(at + 1..at + 1 + len).unwrap_or(&[]) {
                    // Коды 1–64: бит 7 — «родной» (у 129–192 это часть кода).
                    // Бит «родной» у VIC не делает режим родным для ПК:
                    // родной — первый DTD базового блока.
                    let vic = if *svd > 0x80 && *svd <= 0x80 + 64 { *svd & 0x7f } else { *svd };
                    if let Some(mode) = vic_mode(vic) {
                        modes.push(mode);
                    }
                }
            }
            at += 1 + len;
        }
    }
    if dtd_start >= 4 {
        let mut at = dtd_start;
        while at + 18 <= 127 {
            if let Some(mode) = detailed(&e[at..at + 18], Source::Cea) {
                modes.push(mode);
            }
            at += 18;
        }
    }
}

/// DisplayID 1.x/2.x внутри расширения EDID: подробные таймингы type I и VII.
fn displayid(e: &[u8], modes: &mut Vec<Mode>) {
    let Some(section) = e.get(1..127) else { return };
    let length = usize::from(section.get(1).copied().unwrap_or(0));
    let mut at = 4;
    let end = (4 + length).min(section.len());
    while at + 3 <= end {
        let tag = section[at];
        let len = usize::from(section[at + 2]);
        let data = section.get(at + 3..at + 3 + len).unwrap_or(&[]);
        if tag == 0x03 || tag == 0x22 {
            // 20 байт на тайминг; частота точек — в единицах 10 кГц (type I) / 1 кГц (VII).
            for t in data.chunks_exact(20) {
                let raw = u32::from(t[0]) | (u32::from(t[1]) << 8) | (u32::from(t[2]) << 16);
                let clock_khz = if tag == 0x03 { (raw + 1) * 10 } else { raw + 1 };
                let w16 = |i: usize| u32::from(u16::from_le_bytes([t[i], t[i + 1]])) + 1;
                let (ha, hb, hsw) = (w16(4), w16(6), w16(10));
                let hso = u32::from(u16::from_le_bytes([t[8], t[9]]) & 0x7fff) + 1;
                let (va, vb, vsw) = (w16(12), w16(14), w16(18));
                let vso = u32::from(u16::from_le_bytes([t[16], t[17]]) & 0x7fff) + 1;
                modes.push(Mode {
                    width: ha,
                    height: va,
                    clock_khz,
                    h_sync_start: ha + hso,
                    h_sync_end: ha + hso + hsw,
                    h_total: ha + hb,
                    v_sync_start: va + vso,
                    v_sync_end: va + vso + vsw,
                    v_total: va + vb,
                    h_sync_positive: t[9] & 0x80 != 0,
                    v_sync_positive: t[17] & 0x80 != 0,
                    interlaced: t[3] & 0x10 != 0,
                    preferred: t[3] & 0x80 != 0,
                    source: Source::DisplayId,
                });
            }
        }
        at += 3 + len;
    }
}

/// Тайминг CEA-861 по коду VIC (самые ходовые: ПК и телевизоры).
fn vic_mode(vic: u8) -> Option<Mode> {
    // (vic, w, h, clock kHz, h front, h sync, h total, v front, v sync, v total, +h, +v)
    const T: &[(u8, u32, u32, u32, u32, u32, u32, u32, u32, u32, bool, bool)] = &[
        (1, 640, 480, 25175, 16, 96, 800, 10, 2, 525, false, false),
        (2, 720, 480, 27000, 16, 62, 858, 9, 6, 525, false, false),
        (3, 720, 480, 27000, 16, 62, 858, 9, 6, 525, false, false),
        (4, 1280, 720, 74250, 110, 40, 1650, 5, 5, 750, true, true),
        (16, 1920, 1080, 148500, 88, 44, 2200, 4, 5, 1125, true, true),
        (17, 720, 576, 27000, 12, 64, 864, 5, 5, 625, false, false),
        (18, 720, 576, 27000, 12, 64, 864, 5, 5, 625, false, false),
        (19, 1280, 720, 74250, 440, 40, 1980, 5, 5, 750, true, true),
        (31, 1920, 1080, 148500, 528, 44, 2640, 4, 5, 1125, true, true),
        (32, 1920, 1080, 74250, 638, 44, 2750, 4, 5, 1125, true, true),
        (33, 1920, 1080, 74250, 528, 44, 2640, 4, 5, 1125, true, true),
        (34, 1920, 1080, 74250, 88, 44, 2200, 4, 5, 1125, true, true),
        (47, 1280, 720, 148500, 110, 40, 1650, 5, 5, 750, true, true),
        (63, 1920, 1080, 297000, 88, 44, 2200, 4, 5, 1125, true, true),
        (64, 1920, 1080, 297000, 528, 44, 2640, 4, 5, 1125, true, true),
        (93, 3840, 2160, 297000, 1276, 88, 5500, 8, 10, 2250, true, true),
        (94, 3840, 2160, 297000, 1056, 88, 5280, 8, 10, 2250, true, true),
        (95, 3840, 2160, 297000, 176, 88, 4400, 8, 10, 2250, true, true),
        (96, 3840, 2160, 594000, 1056, 88, 5280, 8, 10, 2250, true, true),
        (97, 3840, 2160, 594000, 176, 88, 4400, 8, 10, 2250, true, true),
        (98, 4096, 2160, 297000, 1020, 88, 5500, 8, 10, 2250, true, true),
        (102, 4096, 2160, 594000, 88, 88, 4400, 8, 10, 2250, true, true),
        (117, 3840, 2160, 1188000, 1056, 88, 5280, 8, 10, 2250, true, true),
        (118, 3840, 2160, 1188000, 176, 88, 4400, 8, 10, 2250, true, true),
    ];
    let t = T.iter().find(|t| t.0 == vic)?;
    Some(Mode {
        width: t.1,
        height: t.2,
        clock_khz: t.3,
        h_sync_start: t.1 + t.4,
        h_sync_end: t.1 + t.4 + t.5,
        h_total: t.6,
        v_sync_start: t.2 + t.7,
        v_sync_end: t.2 + t.7 + t.8,
        v_total: t.9,
        h_sync_positive: t.10,
        v_sync_positive: t.11,
        interlaced: false,
        preferred: false,
        source: Source::Cea,
    })
}

/// Установленные режимы, байты 35–37, бит 7 первого байта — первым.
const ESTABLISHED: [(u32, u32, u32); 24] = [
    (720, 400, 70),
    (720, 400, 88),
    (640, 480, 60),
    (640, 480, 67),
    (640, 480, 72),
    (640, 480, 75),
    (800, 600, 56),
    (800, 600, 60),
    (800, 600, 72),
    (800, 600, 75),
    (832, 624, 75),
    (1024, 768, 87),
    (1024, 768, 60),
    (1024, 768, 70),
    (1024, 768, 75),
    (1280, 1024, 75),
    (1152, 870, 75),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
    (0, 0, 0),
];

/// «Established Timings III» (дескриптор 0xF7), по порядку битов.
const ESTABLISHED_III: [(u32, u32, u32); 44] = [
    (640, 350, 85),
    (640, 400, 85),
    (720, 400, 85),
    (640, 480, 85),
    (848, 480, 60),
    (800, 600, 85),
    (1024, 768, 85),
    (1152, 864, 75),
    (1280, 768, 60),
    (1280, 768, 60),
    (1280, 768, 75),
    (1280, 768, 85),
    (1280, 960, 60),
    (1280, 960, 85),
    (1280, 1024, 60),
    (1280, 1024, 85),
    (1360, 768, 60),
    (1440, 900, 60),
    (1440, 900, 60),
    (1440, 900, 75),
    (1440, 900, 85),
    (1400, 1050, 60),
    (1400, 1050, 60),
    (1400, 1050, 75),
    (1400, 1050, 85),
    (1680, 1050, 60),
    (1680, 1050, 60),
    (1680, 1050, 75),
    (1680, 1050, 85),
    (1600, 1200, 60),
    (1600, 1200, 65),
    (1600, 1200, 70),
    (1600, 1200, 75),
    (1600, 1200, 85),
    (1792, 1344, 60),
    (1792, 1344, 75),
    (1856, 1392, 60),
    (1856, 1392, 75),
    (1920, 1200, 60),
    (1920, 1200, 60),
    (1920, 1200, 75),
    (1920, 1200, 85),
    (1920, 1440, 60),
    (1920, 1440, 75),
];

/// Ходовые режимы VESA DMT: (w, h, Гц, кГц, hss, hse, ht, vss, vse, vt, +h, +v).
#[allow(clippy::type_complexity)]
const DMT: &[(u32, u32, u32, u32, u32, u32, u32, u32, u32, u32, bool, bool)] = &[
    (640, 480, 60, 25175, 656, 752, 800, 490, 492, 525, false, false),
    (640, 480, 72, 31500, 664, 704, 832, 489, 492, 520, false, false),
    (640, 480, 75, 31500, 656, 720, 840, 481, 484, 500, false, false),
    (800, 600, 56, 36000, 824, 896, 1024, 601, 603, 625, true, true),
    (800, 600, 60, 40000, 840, 968, 1056, 601, 605, 628, true, true),
    (800, 600, 72, 50000, 856, 976, 1040, 637, 643, 666, true, true),
    (800, 600, 75, 49500, 816, 896, 1056, 601, 604, 625, true, true),
    (1024, 768, 60, 65000, 1048, 1184, 1344, 771, 777, 806, false, false),
    (1024, 768, 70, 75000, 1048, 1184, 1328, 771, 777, 806, false, false),
    (1024, 768, 75, 78750, 1040, 1136, 1312, 769, 772, 800, true, true),
    (1280, 1024, 60, 108000, 1328, 1440, 1688, 1025, 1028, 1066, true, true),
    (1280, 1024, 75, 135000, 1296, 1440, 1688, 1025, 1028, 1066, true, true),
    (1280, 720, 60, 74250, 1390, 1430, 1650, 725, 730, 750, true, true),
    (1280, 800, 60, 71000, 1328, 1360, 1440, 803, 809, 823, true, false),
    (1440, 900, 60, 88750, 1488, 1520, 1600, 903, 909, 926, true, false),
    (1600, 900, 60, 108000, 1624, 1704, 1800, 901, 904, 1000, true, true),
    (1680, 1050, 60, 119000, 1728, 1760, 1840, 1053, 1059, 1080, true, false),
    (1920, 1080, 60, 148500, 2008, 2052, 2200, 1084, 1089, 1125, true, true),
    (1920, 1200, 60, 154000, 1968, 2000, 2080, 1203, 1209, 1235, true, false),
    (2560, 1440, 60, 241500, 2608, 2640, 2720, 1443, 1448, 1481, true, false),
];

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use alloc::vec;

    /// EDID с одним DTD 2560x1440 (как у BenQ Романа, 241,5 МГц), стандартным
    /// режимом 1920x1080@60 и установленным 1024x768@60.
    fn sample() -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&HEADER);
        // BNQ: B=2, N=14, Q=17.
        let id: u16 = (2 << 10) | (14 << 5) | 17;
        e[8..10].copy_from_slice(&id.to_be_bytes());
        e[18] = 1;
        e[19] = 4;
        e[20] = 0xa5;
        e[21] = 60;
        e[22] = 34;
        e[24] = 0x02;
        e[36] = 0x08; // 1024x768@60 — бит 3 второго байта
        // Стандартный: 1920x1080@60 → (1920/8-31)=209, 16:9 = 3<<6 | 0.
        e[38] = 209;
        e[39] = 0xc0;
        for i in 40..54 {
            e[i] = 1;
        }
        // DTD: 2560x1440, h blank 160, v blank 41, hso 48 hsw 32, vso 3 vsw 5.
        let d = &mut e[54..72];
        d[0..2].copy_from_slice(&24150u16.to_le_bytes());
        d[2] = (2560 & 0xff) as u8;
        d[3] = 160;
        d[4] = (((2560 >> 8) as u8) << 4) | 0;
        d[5] = (1440 & 0xff) as u8;
        d[6] = 41;
        d[7] = (((1440 >> 8) as u8) << 4) | 0;
        d[8] = 48;
        d[9] = 32;
        d[10] = (3 << 4) | 5;
        d[11] = 0;
        d[17] = 0x1a;
        // Имя монитора.
        let n = &mut e[72..90];
        n[3] = 0xfc;
        n[5..14].copy_from_slice(b"BenQ LCD\n");
        // Пустые дескрипторы.
        e[93] = 0x10;
        e[111] = 0x10;
        let sum = e[..127].iter().fold(0u8, |a, b| a.wrapping_add(*b));
        e[127] = 0u8.wrapping_sub(sum);
        e
    }

    #[test]
    fn parses_sample() {
        let m = parse(&sample()).unwrap();
        assert_eq!(m.maker_str(), "BNQ");
        assert_eq!(m.name, "BenQ LCD");
        let p = m.preferred().unwrap();
        assert_eq!((p.width, p.height), (2560, 1440));
        assert_eq!(p.refresh_hz(), 60);
        assert!(p.refresh_mhz() > 59_900 && p.refresh_mhz() < 60_000, "{}", p.refresh_mhz());
        assert!(m.resolutions().contains(&(1920, 1080)));
        assert!(m.resolutions().contains(&(1024, 768)));
        assert_eq!(m.resolutions()[0], (2560, 1440));
    }

    #[test]
    fn rejects_bad_checksum() {
        let mut e = sample();
        e[127] ^= 1;
        assert_eq!(parse(&e).unwrap_err(), Error::BadChecksum);
    }

    #[test]
    fn cvt_rb_is_close_to_dmt() {
        // CVT-RB 1920x1200@60 по стандарту: 154 МГц, 2080x1235.
        let m = cvt_rb(1920, 1200, 60);
        assert_eq!(m.h_total, 2080);
        assert!(m.v_total.abs_diff(1235) <= 2, "{}", m.v_total);
        assert!(m.clock_khz.abs_diff(154_000) <= 1000, "{}", m.clock_khz);
    }

    #[test]
    fn refresh_list() {
        let m = parse(&sample()).unwrap();
        let rates = m.refresh_rates(2560, 1440);
        assert_eq!(rates.len(), 1);
    }
}
