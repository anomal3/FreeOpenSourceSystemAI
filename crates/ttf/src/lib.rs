//! TrueType: разбор шрифта и растеризация глифов (02.11, шрифты системы).
//!
//! # Зачем свой
//!
//! Шрифт интерфейса у системы растровый и вшит в каждую программу
//! (`mini_ui::typeface`, глифы растеризует `tools/font/genfont.py` на машине
//! сборки). Для Блокнота с выбором шрифта и размера этого мало: десяток
//! гарнитур в десятке размеров — десятки мегабайт в каждом бинаре. Здесь —
//! векторный шрифт с диска, растеризуемый на лету в нужном размере.
//!
//! # Что умеет
//!
//! Таблицы `head`, `hhea`, `maxp`, `hmtx`, `cmap` (форматы 4 и 12), `loca`,
//! `glyf` — простые и составные глифы. Переменные шрифты (`fvar`/`gvar`)
//! рисуются своим начертанием по умолчанию: вариации не применяются. Шрифты с
//! контурами CFF (`.otf` с таблицей `CFF `) не поддерживаются — [`Font::parse`]
//! отвечает `None`. Хинтинга нет: сглаживание по площади покрытия, как у
//! font-rs и stb_truetype без хинтов.
//!
//! # Недоверенные данные
//!
//! Файл шрифта приходит с диска, то есть из-за границы доверия. Каждое чтение
//! проверено по длине (`read_u16` и соседи отвечают `None` за краем),
//! вложенность составных глифов ограничена, число точек — тоже.

#![no_std]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

/// Сколько уровней вложенности составного глифа допускается.
const MAX_DEPTH: u32 = 8;
/// Самое большое число точек контура глифа.
const MAX_POINTS: usize = 16 * 1024;
/// Самый большой растр глифа по стороне.
const MAX_SIDE: i32 = 1024;

fn u16_at(data: &[u8], at: usize) -> Option<u16> {
    let bytes = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn i16_at(data: &[u8], at: usize) -> Option<i16> {
    u16_at(data, at).map(|value| value as i16)
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    let bytes = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Шрифт TrueType: данные файла и места таблиц в них.
pub struct Font {
    data: Vec<u8>,
    units_per_em: u16,
    long_loca: bool,
    glyph_count: u16,
    h_metrics: u16,
    ascender: i16,
    descender: i16,
    line_gap: i16,
    cmap: usize,
    cmap_format: u16,
    loca: usize,
    glyf: usize,
    glyf_len: usize,
    hmtx: usize,
}

/// Метрики строки в пикселях.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineMetrics {
    /// Над базовой линией.
    pub ascent: f32,
    /// Под базовой линией (положительное число).
    pub descent: f32,
    /// Шаг строки.
    pub line: f32,
}

/// Растр глифа: покрытие 0..=255 по строкам.
#[derive(Clone, Debug, Default)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Сдвиг левого края растра от начала глифа, точки.
    pub left: i32,
    /// Сдвиг верхнего края растра от базовой линии вверх — со знаком минус:
    /// растр рисуется с `y = база + top`.
    pub top: i32,
    /// Шаг пера после глифа, точки (дробные — по ним копится позиция строки).
    pub advance: f32,
    pub coverage: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Point {
    x: f32,
    y: f32,
}

impl Font {
    /// Разобрать файл шрифта. `None` — это не TrueType с контурами `glyf` или
    /// он испорчен.
    #[must_use]
    pub fn parse(data: Vec<u8>) -> Option<Self> {
        let tables = u16_at(&data, 4)? as usize;
        let find = |tag: &[u8; 4]| -> Option<(usize, usize)> {
            (0..tables).find_map(|index| {
                let record = 12 + index * 16;
                if data.get(record..record + 4)? == tag {
                    Some((u32_at(&data, record + 8)? as usize, u32_at(&data, record + 12)? as usize))
                } else {
                    None
                }
            })
        };
        let (head, _) = find(b"head")?;
        let (hhea, _) = find(b"hhea")?;
        let (maxp, _) = find(b"maxp")?;
        let (hmtx, _) = find(b"hmtx")?;
        let (cmap_table, _) = find(b"cmap")?;
        let (loca, _) = find(b"loca")?;
        let (glyf, glyf_len) = find(b"glyf")?;
        let units_per_em = u16_at(&data, head + 18)?;
        let long_loca = i16_at(&data, head + 50)? != 0;
        let glyph_count = u16_at(&data, maxp + 4)?;
        let ascender = i16_at(&data, hhea + 4)?;
        let descender = i16_at(&data, hhea + 6)?;
        let line_gap = i16_at(&data, hhea + 8)?;
        let h_metrics = u16_at(&data, hhea + 34)?;
        if units_per_em == 0 || h_metrics == 0 {
            return None;
        }
        // Подтаблица cmap: Юникод полный (формат 12), иначе BMP (формат 4).
        let count = u16_at(&data, cmap_table + 2)? as usize;
        let mut best: Option<(usize, u16)> = None;
        for index in 0..count {
            let record = cmap_table + 4 + index * 8;
            let platform = u16_at(&data, record)?;
            let encoding = u16_at(&data, record + 2)?;
            let offset = cmap_table + u32_at(&data, record + 4)? as usize;
            let format = u16_at(&data, offset)?;
            let unicode = platform == 0 || (platform == 3 && (encoding == 1 || encoding == 10));
            if !unicode {
                continue;
            }
            match (format, best) {
                (12, _) => best = Some((offset, 12)),
                (4, None) => best = Some((offset, 4)),
                _ => {}
            }
        }
        let (cmap, cmap_format) = best?;
        Some(Self {
            data,
            units_per_em,
            long_loca,
            glyph_count,
            h_metrics,
            ascender,
            descender,
            line_gap,
            cmap,
            cmap_format,
            loca,
            glyf,
            glyf_len,
            hmtx,
        })
    }

    /// Метрики строки для размера `px` (высота em в точках).
    #[must_use]
    pub fn line_metrics(&self, px: f32) -> LineMetrics {
        let scale = px / f32::from(self.units_per_em);
        let ascent = f32::from(self.ascender) * scale;
        let descent = -f32::from(self.descender) * scale;
        LineMetrics { ascent, descent, line: ascent + descent + f32::from(self.line_gap) * scale }
    }

    /// Номер глифа знака; ноль — знака в шрифте нет (глиф «.notdef»).
    #[must_use]
    pub fn glyph_index(&self, ch: char) -> u16 {
        let code = ch as u32;
        let data = &self.data;
        let found = if self.cmap_format == 12 {
            (|| {
                let groups = u32_at(data, self.cmap + 12)? as usize;
                let (mut low, mut high) = (0usize, groups);
                while low < high {
                    let mid = (low + high) / 2;
                    let group = self.cmap + 16 + mid * 12;
                    let start = u32_at(data, group)?;
                    let end = u32_at(data, group + 4)?;
                    if code < start {
                        high = mid;
                    } else if code > end {
                        low = mid + 1;
                    } else {
                        return Some((u32_at(data, group + 8)? + (code - start)) as u16);
                    }
                }
                None
            })()
        } else {
            (|| {
                if code > 0xFFFF {
                    return None;
                }
                let code = code as u16;
                let segments = (u16_at(data, self.cmap + 6)? / 2) as usize;
                let ends = self.cmap + 14;
                let starts = ends + segments * 2 + 2;
                let deltas = starts + segments * 2;
                let ranges = deltas + segments * 2;
                for segment in 0..segments {
                    let end = u16_at(data, ends + segment * 2)?;
                    if code > end {
                        continue;
                    }
                    let start = u16_at(data, starts + segment * 2)?;
                    if code < start {
                        return None;
                    }
                    let delta = u16_at(data, deltas + segment * 2)?;
                    let range_at = ranges + segment * 2;
                    let range = u16_at(data, range_at)?;
                    if range == 0 {
                        return Some(code.wrapping_add(delta));
                    }
                    let glyph_at = range_at + range as usize + (code - start) as usize * 2;
                    let glyph = u16_at(data, glyph_at)?;
                    return Some(if glyph == 0 { 0 } else { glyph.wrapping_add(delta) });
                }
                None
            })()
        };
        found.filter(|glyph| *glyph < self.glyph_count).unwrap_or(0)
    }

    /// Шаг пера глифа в единицах шрифта.
    fn advance_units(&self, glyph: u16) -> u16 {
        let index = glyph.min(self.h_metrics - 1) as usize;
        u16_at(&self.data, self.hmtx + index * 4).unwrap_or(0)
    }

    /// Шаг пера знака в точках для размера `px`.
    #[must_use]
    pub fn advance(&self, ch: char, px: f32) -> f32 {
        f32::from(self.advance_units(self.glyph_index(ch))) * px / f32::from(self.units_per_em)
    }

    /// Где в `glyf` лежит глиф: начало и длина.
    fn glyph_range(&self, glyph: u16) -> Option<(usize, usize)> {
        let index = glyph as usize;
        let (start, end) = if self.long_loca {
            (u32_at(&self.data, self.loca + index * 4)? as usize, u32_at(&self.data, self.loca + index * 4 + 4)? as usize)
        } else {
            (u16_at(&self.data, self.loca + index * 2)? as usize * 2, u16_at(&self.data, self.loca + index * 2 + 2)? as usize * 2)
        };
        if end < start || end > self.glyf_len {
            return None;
        }
        Some((self.glyf + start, end - start))
    }

    /// Контуры глифа в единицах шрифта: каждый — список точек с признаком
    /// «на кривой».
    fn contours(&self, glyph: u16, depth: u32, out: &mut Vec<Vec<(Point, bool)>>) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (at, len) = self.glyph_range(glyph)?;
        if len == 0 {
            return Some(());
        }
        let data = &self.data;
        let contours = i16_at(data, at)?;
        if contours < 0 {
            return self.composite(at + 10, depth, out);
        }
        let contours = contours as usize;
        let ends_at = at + 10;
        let mut ends = Vec::with_capacity(contours);
        for index in 0..contours {
            ends.push(u16_at(data, ends_at + index * 2)? as usize);
        }
        let points = ends.last().map_or(0, |last| last + 1);
        if points > MAX_POINTS {
            return None;
        }
        let instructions = u16_at(data, ends_at + contours * 2)? as usize;
        let mut cursor = ends_at + contours * 2 + 2 + instructions;
        // Флаги с повторами.
        let mut flags = Vec::with_capacity(points);
        while flags.len() < points {
            let flag = *data.get(cursor)?;
            cursor += 1;
            flags.push(flag);
            if flag & 8 != 0 {
                let repeat = *data.get(cursor)?;
                cursor += 1;
                for _ in 0..repeat {
                    flags.push(flag);
                }
            }
        }
        flags.truncate(points);
        let mut read_coords = |short: u8, same: u8| -> Option<Vec<i32>> {
            let mut value = 0i32;
            let mut out = Vec::with_capacity(points);
            for flag in &flags {
                if flag & short != 0 {
                    let delta = i32::from(*data.get(cursor)?);
                    cursor += 1;
                    value += if flag & same != 0 { delta } else { -delta };
                } else if flag & same == 0 {
                    value += i32::from(i16_at(data, cursor)?);
                    cursor += 2;
                }
                out.push(value);
            }
            Some(out)
        };
        let xs = read_coords(2, 16)?;
        let ys = read_coords(4, 32)?;
        let mut start = 0;
        for end in ends {
            if end < start || end >= points {
                return None;
            }
            out.push((start..=end).map(|index| (Point { x: xs[index] as f32, y: ys[index] as f32 }, flags[index] & 1 != 0)).collect());
            start = end + 1;
        }
        Some(())
    }

    /// Составной глиф: части со сдвигом и масштабом.
    fn composite(&self, mut at: usize, depth: u32, out: &mut Vec<Vec<(Point, bool)>>) -> Option<()> {
        let data = &self.data;
        loop {
            let flags = u16_at(data, at)?;
            let glyph = u16_at(data, at + 2)?;
            at += 4;
            let (dx, dy) = if flags & 1 != 0 {
                let pair = (i16_at(data, at)?, i16_at(data, at + 2)?);
                at += 4;
                (f32::from(pair.0), f32::from(pair.1))
            } else {
                let pair = (*data.get(at)? as i8, *data.get(at + 1)? as i8);
                at += 2;
                (f32::from(pair.0), f32::from(pair.1))
            };
            let f2dot14 = |at: usize| i16_at(data, at).map(|value| f32::from(value) / 16384.0);
            let (mut a, mut b, mut c, mut d) = (1.0f32, 0.0f32, 0.0f32, 1.0f32);
            if flags & 8 != 0 {
                a = f2dot14(at)?;
                d = a;
                at += 2;
            } else if flags & 0x40 != 0 {
                a = f2dot14(at)?;
                d = f2dot14(at + 2)?;
                at += 4;
            } else if flags & 0x80 != 0 {
                a = f2dot14(at)?;
                b = f2dot14(at + 2)?;
                c = f2dot14(at + 4)?;
                d = f2dot14(at + 6)?;
                at += 8;
            }
            let mut part = Vec::new();
            self.contours(glyph, depth + 1, &mut part)?;
            // Сдвиг — по координатам только если ARGS_ARE_XY_VALUES; привязку
            // точек (иначе) не поддерживаем — часть без сдвига.
            let (dx, dy) = if flags & 2 != 0 { (dx, dy) } else { (0.0, 0.0) };
            for contour in part {
                out.push(
                    contour
                        .into_iter()
                        .map(|(p, on)| (Point { x: a * p.x + c * p.y + dx, y: b * p.x + d * p.y + dy }, on))
                        .collect(),
                );
            }
            if out.iter().map(Vec::len).sum::<usize>() > MAX_POINTS {
                return None;
            }
            if flags & 0x20 == 0 {
                return Some(());
            }
        }
    }

    /// Растр знака для размера `px`. Знака нет — глиф «.notdef».
    #[must_use]
    pub fn rasterize(&self, ch: char, px: f32) -> Bitmap {
        let glyph = self.glyph_index(ch);
        let scale = px / f32::from(self.units_per_em);
        let advance = f32::from(self.advance_units(glyph)) * scale;
        let mut contours = Vec::new();
        if self.contours(glyph, 0, &mut contours).is_none() || contours.is_empty() {
            return Bitmap { advance, ..Bitmap::default() };
        }
        // В точки, ось y — вниз.
        let mut min = Point { x: f32::MAX, y: f32::MAX };
        let mut max = Point { x: f32::MIN, y: f32::MIN };
        for contour in &mut contours {
            for (p, _) in contour.iter_mut() {
                p.x *= scale;
                p.y = -p.y * scale;
                min.x = min.x.min(p.x);
                min.y = min.y.min(p.y);
                max.x = max.x.max(p.x);
                max.y = max.y.max(p.y);
            }
        }
        let left = floor(min.x);
        let top = floor(min.y);
        let width = (ceil(max.x) - left).clamp(0, MAX_SIDE);
        let height = (ceil(max.y) - top).clamp(0, MAX_SIDE);
        if width == 0 || height == 0 {
            return Bitmap { advance, left, top, ..Bitmap::default() };
        }
        let mut raster = Raster::new(width as usize, height as usize);
        let shift = Point { x: left as f32, y: top as f32 };
        for contour in &contours {
            raster.contour(contour, shift);
        }
        Bitmap { width: width as u32, height: height as u32, left, top, advance, coverage: raster.finish() }
    }
}

/// Целая часть вниз (в `core` у `f32` нет `floor`).
fn floor(x: f32) -> i32 {
    let i = x as i32;
    if (i as f32) > x { i - 1 } else { i }
}

fn ceil(x: f32) -> i32 {
    let i = x as i32;
    if (i as f32) < x { i + 1 } else { i }
}

fn sqrt(x: f32) -> f32 {
    if x <= 0.0 {
        return 0.0;
    }
    let mut guess = if x > 1.0 { x / 2.0 } else { 1.0 };
    for _ in 0..8 {
        guess = 0.5 * (guess + x / guess);
    }
    guess
}

/// Накопитель площадей (как у font-rs): отрезок контура прибавляет к ячейкам
/// свою долю покрытия со знаком направления; сумма по строке — покрытие.
struct Raster {
    w: usize,
    h: usize,
    acc: Vec<f32>,
}

impl Raster {
    fn new(w: usize, h: usize) -> Self {
        Self { w, h, acc: vec![0.0; w * h + 4] }
    }

    /// Контур TrueType: точки на кривой и контрольные; две контрольные подряд
    /// — между ними подразумевается точка на кривой.
    fn contour(&mut self, points: &[(Point, bool)], shift: Point) {
        if points.len() < 2 {
            return;
        }
        let at = |index: usize| {
            let (p, on) = points[index % points.len()];
            (Point { x: p.x - shift.x, y: p.y - shift.y }, on)
        };
        let mid = |a: Point, b: Point| Point { x: (a.x + b.x) / 2.0, y: (a.y + b.y) / 2.0 };
        // Начало — точка на кривой (или середина между двумя контрольными).
        let first_on = (0..points.len()).find(|index| points[*index].1);
        let (start, begin) = match first_on {
            Some(index) => (at(index).0, index),
            None => (mid(at(0).0, at(1).0), 0),
        };
        let mut current = start;
        let mut control: Option<Point> = None;
        for step in 1..=points.len() {
            let (p, on) = at(begin + step);
            match (on, control) {
                (true, None) => {
                    self.line(current, p);
                    current = p;
                }
                (true, Some(c)) => {
                    self.quad(current, c, p);
                    current = p;
                    control = None;
                }
                (false, None) => control = Some(p),
                (false, Some(c)) => {
                    let m = mid(c, p);
                    self.quad(current, c, m);
                    current = m;
                    control = Some(p);
                }
            }
        }
        match control {
            Some(c) => self.quad(current, c, start),
            None => self.line(current, start),
        }
    }

    fn quad(&mut self, p0: Point, p1: Point, p2: Point) {
        let dx = p0.x - 2.0 * p1.x + p2.x;
        let dy = p0.y - 2.0 * p1.y + p2.y;
        let dev = dx * dx + dy * dy;
        if dev < 0.333 {
            self.line(p0, p2);
            return;
        }
        let n = (1.0 + sqrt(sqrt(3.0 * dev))) as usize;
        let n = n.clamp(1, 64);
        let mut previous = p0;
        for index in 1..=n {
            let t = index as f32 / n as f32;
            let u = 1.0 - t;
            let p = Point { x: u * u * p0.x + 2.0 * u * t * p1.x + t * t * p2.x, y: u * u * p0.y + 2.0 * u * t * p1.y + t * t * p2.y };
            self.line(previous, p);
            previous = p;
        }
    }

    fn line(&mut self, p0: Point, p1: Point) {
        if p0.y == p1.y {
            return;
        }
        let (dir, p0, p1) = if p0.y < p1.y { (1.0f32, p0, p1) } else { (-1.0f32, p1, p0) };
        let dxdy = (p1.x - p0.x) / (p1.y - p0.y);
        let mut x = p0.x;
        let y0 = p0.y.max(0.0) as usize;
        if p0.y < 0.0 {
            x -= p0.y * dxdy;
        }
        let w = self.w as f32;
        let y_end = (ceil(p1.y).max(0) as usize).min(self.h);
        for y in y0..y_end {
            let line_start = y * self.w;
            let dy = ((y + 1) as f32).min(p1.y) - (y as f32).max(p0.y);
            let x_next = x + dxdy * dy;
            let d = dy * dir;
            let (x0, x1) = if x < x_next { (x, x_next) } else { (x_next, x) };
            let (x0, x1) = (x0.clamp(0.0, w), x1.clamp(0.0, w));
            let x0_floor = floor(x0) as f32;
            let x0i = x0_floor as usize;
            let x1_ceil = ceil(x1) as f32;
            let x1i = x1_ceil as usize;
            if x1i <= x0i + 1 {
                let xmf = 0.5 * (x + x_next) - x0_floor;
                let xmf = xmf.clamp(0.0, 1.0);
                self.add(line_start + x0i, d - d * xmf);
                self.add(line_start + x0i + 1, d * xmf);
            } else {
                let s = 1.0 / (x1 - x0);
                let x0f = x0 - x0_floor;
                let a0 = 0.5 * s * (1.0 - x0f) * (1.0 - x0f);
                let x1f = x1 - x1_ceil + 1.0;
                let am = 0.5 * s * x1f * x1f;
                self.add(line_start + x0i, d * a0);
                if x1i == x0i + 2 {
                    self.add(line_start + x0i + 1, d * (1.0 - a0 - am));
                } else {
                    let a1 = s * (1.5 - x0f);
                    self.add(line_start + x0i + 1, d * (a1 - a0));
                    for xi in x0i + 2..x1i - 1 {
                        self.add(line_start + xi, d * s);
                    }
                    let a2 = a1 + (x1i - x0i - 3) as f32 * s;
                    self.add(line_start + x1i - 1, d * (1.0 - a2 - am));
                }
                self.add(line_start + x1i, d * am);
            }
            x = x_next;
        }
    }

    fn add(&mut self, index: usize, value: f32) {
        if let Some(cell) = self.acc.get_mut(index) {
            *cell += value;
        }
    }

    fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.w * self.h);
        let mut sum = 0.0f32;
        for index in 0..self.w * self.h {
            // Сумма — по всему растру подряд: в конце строки она к нулю
            // возвращается сама (контур замкнут), как у font-rs.
            sum += self.acc[index];
            let value = if sum < 0.0 { -sum } else { sum };
            out.push((value.min(1.0) * 255.0 + 0.5) as u8);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn font(name: &str) -> Font {
        let path = alloc::format!("{}/../../assets/fonts/{name}", env!("CARGO_MANIFEST_DIR"));
        let data = std::fs::read(&path).expect("шрифт из assets/fonts");
        Font::parse(data).expect("TrueType")
    }

    extern crate std;

    #[test]
    fn latin_and_cyrillic() {
        for name in ["Inter.ttf", "JetBrainsMono.ttf", "DejaVuSans.ttf", "NotoSans.ttf", "FiraCode.ttf", "Hack-Regular.ttf"] {
            let font = font(name);
            for ch in ['A', 'g', 'Ж', 'ё', '0', '{'] {
                assert_ne!(font.glyph_index(ch), 0, "{name}: нет '{ch}'");
                let bitmap = font.rasterize(ch, 16.0);
                assert!(bitmap.width > 0 && bitmap.height > 0, "{name}: пустой '{ch}'");
                let ink: u32 = bitmap.coverage.iter().map(|value| u32::from(*value)).sum();
                assert!(ink > 255 * 4, "{name}: '{ch}' почти не закрашен");
                assert!(bitmap.advance > 4.0 && bitmap.advance < 24.0, "{name}: шаг '{ch}' {}", bitmap.advance);
            }
            assert_eq!(font.rasterize(' ', 16.0).coverage.len(), 0);
        }
    }

    #[test]
    fn mono_is_mono() {
        let font = font("JetBrainsMono.ttf");
        let w = font.advance('i', 14.0);
        assert!((font.advance('W', 14.0) - w).abs() < 0.01);
        let metrics = font.line_metrics(14.0);
        assert!(metrics.ascent > 9.0 && metrics.line > metrics.ascent);
    }

    #[test]
    fn coverage_of_a_box_is_full_inside() {
        // «█» (U+2588) в DejaVu Sans Mono — сплошной прямоугольник.
        let font = font("DejaVuSansMono.ttf");
        let bitmap = font.rasterize('█', 20.0);
        let w = bitmap.width as usize;
        let h = bitmap.height as usize;
        assert!(w > 4 && h > 4);
        assert!(bitmap.coverage[(h / 2) * w + w / 2] > 250);
    }

    /// Глазами: `cargo test -p ttf show -- --ignored --nocapture`.
    #[test]
    #[ignore = "печатает глифы для глаза"]
    fn show() {
        let font = font("Inter.ttf");
        for ch in ['A', 'g', 'Ж', 'é'] {
            let bitmap = font.rasterize(ch, 24.0);
            std::println!("'{ch}' {}x{} left {} top {} advance {}", bitmap.width, bitmap.height, bitmap.left, bitmap.top, bitmap.advance);
            for row in bitmap.coverage.chunks(bitmap.width as usize) {
                let line: alloc::string::String = row.iter().map(|value| match value { 0..=40 => ' ', 41..=120 => '.', 121..=200 => '+', _ => '#' }).collect();
                std::println!("|{line}|");
            }
        }
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(Font::parse(alloc::vec![0u8; 64]).is_none());
        assert!(Font::parse(alloc::vec![0xFF; 16]).is_none());
    }
}
