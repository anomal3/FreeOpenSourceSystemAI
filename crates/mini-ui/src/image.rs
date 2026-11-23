//! Картинки — значки программ (фаза 71i, значки как в Windows).
//!
//! Формат свой и простейший — FIMG: `FIMG`, ширина и высота (`u16`,
//! little-endian), затем точки построчно по четыре байта `R G B A` (альфа не
//! умножена). PNG разбирает `xtask` при сборке пакета: декодер PNG в ядре —
//! это zlib и фильтры строк ради картинки в несколько килобайт, а вход у него
//! — файл из чужого пакета.
//!
//! С 22.11 значок может быть и QOI с альфой (`qoif`, четыре канала), а за
//! первой картинкой могут идти ещё — тот же значок мельче (128, 64, 48, 32,
//! 24, 16): у каждого размера своя подготовленная картинка с подчёркнутыми
//! краями, а не усреднение 128 в 16 на лету. Значки тем лежат пачкой FIPK
//! (см. [`pack_entry`]): `assets/icons/themes/<тема>` → `cargo xtask
//! wallpaper` → `initrd/usr/share/icons/<тема>.pak`.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::draw;
use crate::{Color, Rect, Surface};

/// Заголовок формата.
pub const MAGIC: &[u8; 4] = b"FIMG";

/// Самая большая сторона, которую стоит читать.
const MAX_SIDE: u32 = 512;

/// Картинка в памяти: точки `R G B A`.
pub struct Image {
    pub width: u32,
    pub height: u32,
    rgba: Vec<[u8; 4]>,
    /// Та же картинка мельче, от большей к меньшей (может быть пусто).
    levels: Vec<Image>,
}

impl Image {
    /// Разобрать FIMG или QOI — с меньшими размерами следом, если они есть.
    /// `None` — не тот формат или размер не сходится.
    #[must_use]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let (first, mut at) = parse_one(bytes)?;
        let mut all = alloc::vec![first];
        while at < bytes.len() {
            match parse_one(&bytes[at..]) {
                Some((level, used)) => {
                    all.push(level);
                    at += used;
                }
                None => break,
            }
        }
        // Главная — самая большая, остальные — от большей к меньшей.
        all.sort_by(|a, b| b.width.cmp(&a.width));
        let mut main = all.remove(0);
        main.levels = all;
        Some(main)
    }

    /// Разобрать только один размер — наименьший не меньше `side` (или самый
    /// большой). Для программ с кучей в мегабайт (22.11): значок темы со
    /// всеми размерами — почти сто килобайт, а нужен им один. Пачки пишут
    /// размеры от меньшего к большему, и большие тогда не распаковываются вовсе.
    #[must_use]
    pub fn parse_for(bytes: &[u8], side: u32) -> Option<Self> {
        let mut at = 0;
        let mut best: Option<Image> = None;
        while at < bytes.len() {
            let Some((level, used)) = parse_one(&bytes[at..]) else { break };
            at += used;
            let better = match &best {
                None => true,
                Some(known) if known.width >= side => level.width >= side && level.width < known.width,
                Some(known) => level.width > known.width,
            };
            if better {
                let enough = level.width >= side;
                best = Some(level);
                // Размеры идут по возрастанию: дальше только больше.
                if enough {
                    break;
                }
            }
        }
        best
    }

    /// Картинка размера, ближайшего сверху к `side` (самая большая, если
    /// мельче нет).
    fn level_for(&self, side: u32) -> &Image {
        let mut best = self;
        for level in &self.levels {
            if level.width >= side && level.width < best.width {
                best = level;
            }
        }
        best
    }

    /// Точка `R G B A` (за краем — прозрачная).
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        self.rgba[(y * self.width + x) as usize]
    }

    /// Нарисовать картинку в квадрат `rect` с прозрачностью.
    ///
    /// Уменьшение — усреднением по площади (иначе значок 128×128 в 24 точки
    /// выходит рваным), увеличение — ближайшей точкой. Альфа учитывается при
    /// усреднении: прозрачные точки не тянут цвет края в чёрное.
    pub fn draw(&self, surface: &mut Surface, rect: Rect) {
        if rect.w == 0 || rect.h == 0 {
            return;
        }
        let level = self.level_for(rect.w.max(rect.h));
        if !core::ptr::eq(level, self) {
            level.draw(surface, rect);
            return;
        }
        // Увеличение (дробный масштаб, 20.11: значок 32×32 в плитке 40 точек
        // на 125 %) — билинейно: ближайшая точка при нецелом отношении
        // рисует ступеньки разной ширины.
        if rect.w > self.width || rect.h > self.height {
            self.draw_bilinear(surface, rect);
            return;
        }
        // Уменьшение — честным усреднением по площади с долями точек на
        // краях (22.11): целые границы при отношении 1,4 брали то одну точку,
        // то две, и значок рябил.
        let xs = spans(self.width, rect.w);
        let ys = spans(self.height, rect.h);
        for (ty, &(y0, y1, wy0, wy1)) in ys.iter().enumerate() {
            for (tx, &(x0, x1, wx0, wx1)) in xs.iter().enumerate() {
                let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
                for sy in y0..y1 {
                    let wy = if sy == y0 { wy0 } else if sy + 1 == y1 { wy1 } else { 256 };
                    for sx in x0..x1 {
                        let wx = if sx == x0 { wx0 } else if sx + 1 == x1 { wx1 } else { 256 };
                        let weight = u64::from(wx * wy);
                        let [pr, pg, pb, pa] = self.rgba[(sy * self.width + sx) as usize];
                        let pa = u64::from(pa) * weight;
                        r += u64::from(pr) * pa;
                        g += u64::from(pg) * pa;
                        b += u64::from(pb) * pa;
                        a += pa;
                        n += weight;
                    }
                }
                if a == 0 || n == 0 {
                    continue;
                }
                let color = Color::rgb((r / a) as u8, (g / a) as u8, (b / a) as u8);
                let alpha = (a / n).min(255) as u8;
                draw::blend_pixel(surface, rect.x + tx as i32, rect.y + ty as i32, color, alpha);
            }
        }
    }

    /// Увеличить билинейно, с умножением на альфу (прозрачный край не
    /// тянет цвет в чёрное). Координаты — в 1/256 точки.
    fn draw_bilinear(&self, surface: &mut Surface, rect: Rect) {
        let (w, h) = (self.width, self.height);
        for ty in 0..rect.h {
            // Центр точки цели в точках источника минус половина.
            let fy = ((2 * ty + 1) * h * 128 / rect.h).saturating_sub(128);
            let (y0, wy) = (fy / 256, fy % 256);
            let y1 = (y0 + 1).min(h - 1);
            for tx in 0..rect.w {
                let fx = ((2 * tx + 1) * w * 128 / rect.w).saturating_sub(128);
                let (x0, wx) = (fx / 256, fx % 256);
                let x1 = (x0 + 1).min(w - 1);
                let taps = [
                    (self.pixel(x0, y0), (256 - wx) * (256 - wy)),
                    (self.pixel(x1, y0), wx * (256 - wy)),
                    (self.pixel(x0, y1), (256 - wx) * wy),
                    (self.pixel(x1, y1), wx * wy),
                ];
                let (mut r, mut g, mut b, mut a) = (0u64, 0u64, 0u64, 0u64);
                for ([pr, pg, pb, pa], weight) in taps {
                    let pa = u64::from(pa) * u64::from(weight);
                    r += u64::from(pr) * pa;
                    g += u64::from(pg) * pa;
                    b += u64::from(pb) * pa;
                    a += pa;
                }
                if a == 0 {
                    continue;
                }
                let color = Color::rgb((r / a) as u8, (g / a) as u8, (b / a) as u8);
                let alpha = (a / 65536).min(255) as u8;
                draw::blend_pixel(surface, rect.x + tx as i32, rect.y + ty as i32, color, alpha);
            }
        }
    }
}

/// Для каждой точки цели при уменьшении `src` → `dst`: первая и за
/// последней точки источника и вес (в 1/256) первой и последней.
fn spans(src: u32, dst: u32) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::with_capacity(dst as usize);
    for t in 0..dst {
        let start = u64::from(t) * u64::from(src) * 256 / u64::from(dst);
        let end = (u64::from(t + 1) * u64::from(src) * 256 / u64::from(dst)).max(start + 1);
        let first = (start / 256) as u32;
        let last = (end.div_ceil(256) as u32).min(src).max(first + 1);
        let w_first = (((u64::from(first) + 1) * 256).min(end) - start) as u32;
        let w_last = if last == first + 1 { w_first } else { (end - u64::from(last - 1) * 256) as u32 };
        out.push((first, last, w_first.max(1), w_last.max(1)));
    }
    out
}

/// Одна картинка с начала `bytes` и сколько байт она заняла.
fn parse_one(bytes: &[u8]) -> Option<(Image, usize)> {
    match bytes.get(..4)? {
        m if m == MAGIC => {
            let width = u32::from(u16::from_le_bytes([*bytes.get(4)?, *bytes.get(5)?]));
            let height = u32::from(u16::from_le_bytes([*bytes.get(6)?, *bytes.get(7)?]));
            if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
                return None;
            }
            let count = (width * height) as usize;
            let data = bytes.get(8..8 + count * 4)?;
            let mut rgba = Vec::new();
            rgba.try_reserve_exact(count).ok()?;
            rgba.extend(data.chunks_exact(4).map(|px| [px[0], px[1], px[2], px[3]]));
            Some((Image { width, height, rgba, levels: Vec::new() }, 8 + count * 4))
        }
        b"qoif" => parse_qoi(bytes),
        _ => None,
    }
}

/// QOI (<https://qoiformat.org/qoi-specification.pdf>) с альфой или без —
/// картинка и длина вместе с концевым маркером.
fn parse_qoi(bytes: &[u8]) -> Option<(Image, usize)> {
    let width = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(8..12)?.try_into().ok()?);
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let count = (width * height) as usize;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(count).ok()?;
    let mut seen = [[0u8; 4]; 64];
    let mut px = [0u8, 0, 0, 255];
    let mut at = 14;
    while rgba.len() < count {
        let op = *bytes.get(at)?;
        at += 1;
        let mut run = 1;
        match op {
            0xFE => {
                px = [*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, px[3]];
                at += 3;
            }
            0xFF => {
                px = [*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?, *bytes.get(at + 3)?];
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
                    let next = *bytes.get(at)?;
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
        for _ in 0..run.min(count - rgba.len()) {
            rgba.push(px);
        }
    }
    // Концевой маркер: семь нулей и единица.
    if bytes.get(at..at + 8) == Some(&[0, 0, 0, 0, 0, 0, 0, 1]) {
        at += 8;
    }
    Some((Image { width, height, rgba, levels: Vec::new() }, at))
}

// ── Темы значков (22.11) ────────────────────────────────────────────────────

/// Темы значков: имя файла пачки, название в «Параметрах», пояснение. Первая
/// — по умолчанию (её пачка ещё и вшита в ядро).
pub const ICON_THEMES: [(&str, &str, &str); 4] = [
    ("glossy", "Глянцевые", "Объёмные, с бликами — по умолчанию"),
    ("crisp", "Чёткие", "Векторные, светлые, тонкий контур"),
    ("soft", "Мягкие", "Светло-серые, спокойные, с контуром"),
    ("vivid", "Яркие", "Насыщенный синий, крупные формы"),
];

/// Тема значков, которой рисует эта программа (номер в [`ICON_THEMES`]).
static ICON_THEME: AtomicU8 = AtomicU8::new(0);

/// Сменить тему значков. `true` — сменилась.
pub fn set_icon_theme(index: u8) -> bool {
    let index = if usize::from(index) < ICON_THEMES.len() { index } else { 0 };
    ICON_THEME.swap(index, Ordering::Relaxed) != index
}

/// Номер текущей темы значков.
#[must_use]
pub fn icon_theme() -> u8 {
    ICON_THEME.load(Ordering::Relaxed)
}

/// Имя файла текущей темы (`glossy`, `crisp`).
#[must_use]
pub fn icon_theme_id() -> &'static str {
    ICON_THEMES[usize::from(icon_theme()).min(ICON_THEMES.len() - 1)].0
}

/// Номер темы по имени.
#[must_use]
pub fn icon_theme_index(id: &str) -> Option<u8> {
    ICON_THEMES.iter().position(|(known, ..)| *known == id).map(|index| index as u8)
}

/// Где лежит пачка темы.
#[must_use]
pub fn icon_pack_path(id: &str) -> alloc::string::String {
    alloc::format!("/usr/share/icons/{id}.pak")
}

/// Заголовок пачки значков: `FIPK`, версия (`u16`), число значков (`u16`),
/// конец оглавления (`u32`), все — little-endian. Оглавление: на значок байт
/// длины имени, имя, начало и длина данных (`u32`, от начала файла).
pub const PACK_MAGIC: &[u8; 4] = b"FIPK";

/// Длина заголовка пачки.
pub const PACK_HEAD: usize = 12;

/// Где кончается оглавление пачки (по её заголовку).
#[must_use]
pub fn pack_index_end(head: &[u8]) -> Option<usize> {
    if head.get(..4)? != PACK_MAGIC {
        return None;
    }
    Some(u32::from_le_bytes(head.get(8..12)?.try_into().ok()?) as usize)
}

/// Найти значок `name` в оглавлении: начало и длина его данных.
#[must_use]
pub fn pack_find(index: &[u8], name: &str) -> Option<(usize, usize)> {
    let count = usize::from(u16::from_le_bytes(index.get(6..8)?.try_into().ok()?));
    let mut at = PACK_HEAD;
    for _ in 0..count {
        let len = usize::from(*index.get(at)?);
        let known = index.get(at + 1..at + 1 + len)?;
        let offset = u32::from_le_bytes(index.get(at + 1 + len..at + 5 + len)?.try_into().ok()?) as usize;
        let size = u32::from_le_bytes(index.get(at + 5 + len..at + 9 + len)?.try_into().ok()?) as usize;
        if known == name.as_bytes() {
            return Some((offset, size));
        }
        at += 9 + len;
    }
    None
}

/// Данные значка `name` из пачки целиком в памяти.
#[must_use]
pub fn pack_entry<'a>(pack: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let (offset, size) = pack_find(pack, name)?;
    pack.get(offset..offset + size)
}

/// Стрелка ярлыка, как у Windows: белый квадрат со скруглением в левом
/// нижнем углу значка `icon` и синяя загнутая стрелка в нём.
pub fn draw_shortcut_arrow(surface: &mut Surface, icon: Rect) {
    let side = (icon.w * 3 / 8).max(10);
    let badge = Rect::new(icon.x, icon.bottom() - side as i32, side, side);
    draw::rounded(surface, badge, (side / 5).max(2), Color::rgb(0xFF, 0xFF, 0xFF), 255);
    draw::rounded_stroke(surface, badge, (side / 5).max(2), Color::rgb(0x80, 0x80, 0x80), 160);
    let ink = Color::rgb(0x1E, 0x5A, 0xD6);
    // Стрелка: дуга снизу вверх направо и наконечник — в долях стороны.
    let unit = side as i32;
    let at = |fx: i32, fy: i32| (badge.x + unit * fx / 16, badge.y + unit * fy / 16);
    let thick = (side / 7).max(2) as i32;
    // Хвост — вертикаль снизу, затем к правому верхнему углу.
    let (x0, y0) = at(5, 13);
    let (x1, y1) = at(5, 8);
    let (x2, y2) = at(11, 5);
    line(surface, x0, y0, x1, y1, thick, ink);
    line(surface, x1, y1, x2, y2, thick, ink);
    // Наконечник.
    let (h1x, h1y) = at(7, 4);
    let (h2x, h2y) = at(10, 9);
    line(surface, x2, y2, h1x, h1y, thick, ink);
    line(surface, x2, y2, h2x, h2y, thick, ink);
}

/// Толстая линия кругами — стрелка маленькая, сглаживать её незачем.
fn line(surface: &mut Surface, x0: i32, y0: i32, x1: i32, y1: i32, thick: i32, color: Color) {
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    let r = (thick / 2).max(1);
    for step in 0..=steps {
        let x = x0 + (x1 - x0) * step / steps;
        let y = y0 + (y1 - y0) * step / steps;
        draw::circle(surface, x, y, r as u32, color, 255);
    }
}
