//! «Оформление» (20.11, пожелания Романа 8 и 9): насколько дорого стол
//! рисует себя и в скольких цветах выводит кадр.
//!
//! # Переключатели
//!
//! Каждый эффект — свой бит в [`FLAGS`], и каждый проверяется там, где
//! эффект рисуется (`compositor`, `window`), а не где-то наверху одной общей
//! «экономией»: выключенная тень не должна отключать заодно анимацию.
//!
//! * [`Flag::MinMax`] — полёт окна в кнопку панели и обратно, перелив
//!   «развернуть/восстановить» (`Flight::new`, `Flight::morph`).
//! * [`Flag::OpenClose`] — рост окна из плитки и уход в неё (`Flight::zoom`).
//! * [`Flag::Shadows`] — мягкие тени слоёв (`Compositor::drop_shadow`); без них
//!   и ореол вокруг переезжающего слоя ноль — помечается меньше точек.
//! * [`Flag::Effects`] — размытая подложка «подсмотра» окна из миниатюр
//!   панели (`build_peek_back`); выключено — окно показывается поверх как
//!   есть, без размытия всего стола (на 2560x1440 это десятки мс).
//! * [`Flag::Outline`] — «Показывать контур при перетаскивании»: окно не едет
//!   за указателем, едет штриховая рамка (XOR-инверсия, как в Windows 98);
//!   окно переносится и меняет размер один раз — при отпускании.
//! * [`Flag::Classic`] — классика Windows 98 (с 22.11 — во всём интерфейсе):
//!   рамки с объёмной кромкой и синим заголовком с градиентом, кнопки 16×14
//!   группой, серая грань #C0C0C0 у панели задач, меню, «Параметров» и
//!   программ (`mini_ui::theme::is_classic`, `SYSINFO_CLASSIC`), прямые углы,
//!   без теней и прозрачности.
//!
//! Набор «Современный» — всё, кроме контура и классики; «Простой» — только
//! контур и классика. Отличается от обоих — «Своё».
//!
//! # Глубина цвета
//!
//! Стол всегда собирает кадр в 32 битах — окна, программы, Wine рисуют в
//! 32-битные поверхности, и переделать их всех ради режима нельзя. Глубина
//! применяется на выводе ([`quantize`]): полоса кадра перед копией на экран
//! сводится к RGB565, к палитре 256 цветов (куб 6x6x6 и 24 серых) или к 16
//! цветам VGA, для 8 и 4 бит — с упорядоченным сглаживанием Байера 4x4.
//! Сканаут остаётся 32-битным (см. ROADMAP «Оформление»: где меньше бит дали
//! бы выигрыш и почему его пока нет).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use mini_ui::{Color, Rect, Surface};

use crate::kprintln;
use crate::sync::SpinLock;

/// Один эффект оформления.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flag {
    MinMax,
    OpenClose,
    Shadows,
    Effects,
    Outline,
    Classic,
}

impl Flag {
    pub const ALL: [Flag; 6] = [Flag::MinMax, Flag::OpenClose, Flag::Shadows, Flag::Effects, Flag::Outline, Flag::Classic];

    const fn bit(self) -> u32 {
        1 << self as u32
    }

    /// Имя в `desktop.cfg`, в журнале и в `ui look`.
    pub const fn tag(self) -> &'static str {
        match self {
            Flag::MinMax => "minmax",
            Flag::OpenClose => "openclose",
            Flag::Shadows => "shadows",
            Flag::Effects => "effects",
            Flag::Outline => "outline",
            Flag::Classic => "classic",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|flag| flag.tag() == tag)
    }
}

/// Готовый набор.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Preset {
    Modern,
    Simple,
    Custom,
}

impl Preset {
    pub const fn tag(self) -> &'static str {
        match self {
            Preset::Modern => "modern",
            Preset::Simple => "simple",
            Preset::Custom => "custom",
        }
    }
}

const MODERN: u32 = Flag::MinMax.bit() | Flag::OpenClose.bit() | Flag::Shadows.bit() | Flag::Effects.bit();
const SIMPLE: u32 = Flag::Outline.bit() | Flag::Classic.bit();

static FLAGS: AtomicU32 = AtomicU32::new(MODERN);

/// Глубина цвета вывода: 32, 16, 8 или 4.
static DEPTH: AtomicU32 = AtomicU32::new(32);

/// Таблица «цвет 5:5:5 → пиксель палитры» для 8 и 4 бит (32768 точек).
static LUT: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());

/// Включён ли эффект.
#[must_use]
pub fn on(flag: Flag) -> bool {
    FLAGS.load(Ordering::Relaxed) & flag.bit() != 0
}

/// Включить или выключить эффект (без записи в файл).
pub fn set(flag: Flag, enabled: bool) {
    if enabled {
        FLAGS.fetch_or(flag.bit(), Ordering::Relaxed);
    } else {
        FLAGS.fetch_and(!flag.bit(), Ordering::Relaxed);
    }
    sync_classic();
}

/// Классика — не только рамки окон (22.11, Роман): палитра Windows 98 и
/// объёмные элементы во всём интерфейсе — у стола (`mini_ui::theme`) и у
/// программ (`SYSINFO_CLASSIC`).
fn sync_classic() {
    mini_ui::theme::set_classic(on(Flag::Classic));
}

/// Какой набор сейчас.
#[must_use]
pub fn preset() -> Preset {
    match FLAGS.load(Ordering::Relaxed) {
        MODERN => Preset::Modern,
        SIMPLE => Preset::Simple,
        _ => Preset::Custom,
    }
}

pub fn apply_preset(preset: Preset) {
    match preset {
        Preset::Modern => FLAGS.store(MODERN, Ordering::Relaxed),
        Preset::Simple => FLAGS.store(SIMPLE, Ordering::Relaxed),
        Preset::Custom => {}
    }
    sync_classic();
}

/// Анимация вида `flag` разрешена (телефону анимация — часть жестов, её не
/// выключают).
#[must_use]
pub fn anim(flag: Flag) -> bool {
    mini_ui::theme::is_mobile() || on(flag)
}

/// Радиус скругления слоя: у классических рамок углы прямые.
#[must_use]
pub fn corner(radius: u32) -> u32 {
    if on(Flag::Classic) && !mini_ui::theme::is_mobile() { 0 } else { radius }
}

/// Тащить контуром (только на ПК).
#[must_use]
pub fn outline_drag() -> bool {
    on(Flag::Outline) && !mini_ui::theme::is_mobile()
}

#[must_use]
pub fn depth() -> u32 {
    DEPTH.load(Ordering::Relaxed)
}

/// Сменить глубину вывода. Неизвестное значение — 32.
pub fn set_depth(bits: u32) {
    let bits = match bits {
        16 | 8 | 4 => bits,
        _ => 32,
    };
    if bits == 8 || bits == 4 {
        build_lut(bits);
    }
    DEPTH.store(bits, Ordering::Relaxed);
}

/// Подпись глубины для «Параметров».
#[must_use]
pub const fn depth_title(bits: u32) -> &'static str {
    match bits {
        16 => "16 бит (65 536 цветов)",
        8 => "8 бит (256 цветов)",
        4 => "4 бита (16 цветов)",
        _ => "32 бита (макс.)",
    }
}

// ── Хранение ────────────────────────────────────────────────────────────────

/// Строка `look=` — включённые эффекты через запятую.
fn format_flags() -> alloc::string::String {
    let mut out = alloc::string::String::new();
    for flag in Flag::ALL.into_iter().filter(|flag| on(*flag)) {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(flag.tag());
    }
    if out.is_empty() {
        out.push_str("none");
    }
    out
}

/// Прочитать `look=` и `depth=` из `desktop.cfg` (зовёт `prefs::adopt`).
pub fn adopt(text: &str, path: &str) {
    if let Some(list) = sysconf::value(text, "look") {
        let mut bits = 0;
        for tag in list.split(',').map(str::trim) {
            if let Some(flag) = Flag::from_tag(tag) {
                bits |= flag.bit();
            }
        }
        FLAGS.store(bits, Ordering::Relaxed);
        sync_classic();
        kprintln!("  desktop     : look {} ({}) from {}", list.trim(), preset().tag(), path);
    }
    if let Some(bits) = sysconf::value(text, "depth").and_then(|value| value.trim().parse::<u32>().ok()) {
        set_depth(bits);
        kprintln!("  desktop     : colour depth {} bit from {}", depth(), path);
    }
}

/// Запомнить эффекты.
pub fn store() -> Result<(), crate::vfs::VfsError> {
    super::prefs::store_line("look", &format_flags())
}

/// Запомнить глубину цвета.
pub fn store_depth() -> Result<(), crate::vfs::VfsError> {
    super::prefs::store_line("depth", &alloc::format!("{}", depth()))
}

/// Одна строка состояния — в журнал (по ней ждёт стенд).
pub fn describe() -> alloc::string::String {
    alloc::format!("{} [{}] depth {}", preset().tag(), format_flags(), depth())
}

// ── `ui look …` ─────────────────────────────────────────────────────────────

/// `ui look modern|simple`, `ui look <эффект> on|off`, `ui look depth 32|16|8|4`,
/// `ui look settings` — то же, что страница «Оформление», для стенда.
pub fn command(words: &str) {
    let status = super::status_now();
    let title_before = super::window::Window::chrome_key();
    let mut words = words.split_whitespace();
    let first = words.next().unwrap_or("");
    match first {
        "settings" => {
            let section = if words.next() == Some("display") { super::settings::Section::Display } else { super::settings::Section::Appearance };
            super::with_desktop(|desktop| {
                super::open_settings(desktop, section, &status);
                desktop.present();
            });
            return;
        }
        // Бирюзовый стол классики (22.11) — то же, что переключатель на
        // странице «Оформление».
        "teal" => {
            let on = words.next() != Some("off");
            let (said, result) = super::wallpaper::apply(super::wallpaper::WallDeed::ClassicTeal(on));
            kprintln!("  desktop     : {said}{}", if result.is_err() { " (not saved)" } else { "" });
            super::with_desktop(|desktop| {
                desktop.restyle(&status);
                desktop.present();
            });
            return;
        }
        "modern" => apply_preset(Preset::Modern),
        "simple" => apply_preset(Preset::Simple),
        "depth" => {
            let bits = words.next().and_then(|word| word.parse::<u32>().ok()).unwrap_or(32);
            set_depth(bits);
            if let Err(err) = store_depth() {
                kprintln!("  desktop     : colour depth not saved: {err}");
            }
        }
        "" => {}
        tag => match Flag::from_tag(tag) {
            Some(flag) => set(flag, words.next() != Some("off")),
            None => {
                kprintln!("  desktop     : ui look: modern, simple, depth N, settings or <minmax|openclose|shadows|effects|outline|classic> on|off");
                return;
            }
        },
    }
    if !first.is_empty() && first != "depth" {
        if let Err(err) = store() {
            kprintln!("  desktop     : look not saved: {err}");
        }
    }
    kprintln!("  desktop     : look {}", describe());
    // Классика меняет высоту заголовка (22.11) — окна пересобираются.
    let retitle = super::window::Window::chrome_key() != title_before;
    super::with_desktop(|desktop| {
        if retitle {
            desktop.retitle_all(&status);
        } else {
            desktop.restyle(&status);
        }
        desktop.present();
    });
}

// ── Вывод с меньшей глубиной ────────────────────────────────────────────────

/// Порог Байера 4x4, 0..15.
const BAYER: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// 16 цветов VGA (текстовый режим и 640x480x4).
const VGA: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (0, 0, 170),
    (0, 170, 0),
    (0, 170, 170),
    (170, 0, 0),
    (170, 0, 170),
    (170, 85, 0),
    (170, 170, 170),
    (85, 85, 85),
    (85, 85, 255),
    (85, 255, 85),
    (85, 255, 255),
    (255, 85, 85),
    (255, 85, 255),
    (255, 255, 85),
    (255, 255, 255),
];

/// Уровни куба 6x6x6.
const CUBE: [u8; 6] = [0, 51, 102, 153, 204, 255];

/// Палитра 8 бит: куб и 24 серых между его ступенями.
fn palette8() -> Vec<(u8, u8, u8)> {
    let mut out = Vec::with_capacity(240);
    for r in CUBE {
        for g in CUBE {
            for b in CUBE {
                out.push((r, g, b));
            }
        }
    }
    for i in 0..24u32 {
        let v = (8 + i * 10) as u8;
        out.push((v, v, v));
    }
    out
}

/// Пиксель машины → три байта по порядку в слове (младший первым). Порядок
/// RGB/BGR не важен: палитры строятся через [`Color::pixel`] в том же порядке.
#[inline]
const fn split(pixel: u32) -> (i32, i32, i32) {
    ((pixel & 0xFF) as i32, ((pixel >> 8) & 0xFF) as i32, ((pixel >> 16) & 0xFF) as i32)
}

/// Собрать таблицу ближайших цветов для 8 или 4 бит.
fn build_lut(bits: u32) {
    let colors: Vec<(u8, u8, u8)> = if bits == 4 { VGA.to_vec() } else { palette8() };
    // Палитра — в порядке байтов машины.
    let native: Vec<(u32, (i32, i32, i32))> = colors
        .iter()
        .map(|&(r, g, b)| {
            let pixel = Color::rgb(r, g, b).pixel();
            (pixel, split(pixel))
        })
        .collect();
    let mut lut = Vec::new();
    if lut.try_reserve_exact(1 << 15).is_err() {
        return;
    }
    for index in 0..(1u32 << 15) {
        // Середина клетки 5:5:5.
        let c0 = (((index & 31) << 3) | 4) as i32;
        let c1 = ((((index >> 5) & 31) << 3) | 4) as i32;
        let c2 = ((((index >> 10) & 31) << 3) | 4) as i32;
        let mut best = (i32::MAX, 0u32);
        for &(pixel, (p0, p1, p2)) in &native {
            // Зелёный (средний байт) глаз различает лучше — вес 2.
            let d = (c0 - p0) * (c0 - p0) + 2 * (c1 - p1) * (c1 - p1) + (c2 - p2) * (c2 - p2);
            if d < best.0 {
                best = (d, pixel);
            }
        }
        lut.push(best.1);
    }
    *LUT.lock() = lut;
    kprintln!("  desktop     : {bits}-bit palette table built");
}

/// Свести строки `0..rows` буфера `back` в столбцах `x0..x0+w` к текущей
/// глубине. `top` — строка стола, с которой начинается буфер (для узора
/// сглаживания: он привязан к экрану, а не к полосе, и не «плывёт»).
pub fn quantize(back: &mut Surface, x0: i32, w: u32, rows: u32, top: i32) {
    let bits = depth();
    if bits == 32 {
        return;
    }
    let from = x0.max(0) as usize;
    let to = ((x0 + w as i32).max(0) as usize).min(back.width() as usize);
    if from >= to {
        return;
    }
    let rows = rows.min(back.height());
    if bits == 16 {
        for y in 0..rows {
            for pixel in &mut back.row_mut(y)[from..to] {
                let p = *pixel;
                // 5:6:5 — средний байт шесть бит, крайние по пять; младшие
                // биты повторяют старшие, чтобы белый остался белым.
                let c0 = p & 0xF8;
                let c1 = (p >> 8) & 0xFC;
                let c2 = (p >> 16) & 0xF8;
                *pixel = 0xFF00_0000 | (c0 | c0 >> 5) | ((c1 | c1 >> 6) << 8) | ((c2 | c2 >> 5) << 16);
            }
        }
        return;
    }
    let lut = LUT.lock();
    if lut.len() != 1 << 15 {
        return;
    }
    // Размах сглаживания — шаг палитры: куб 51, VGA около 85.
    let step = if bits == 8 { 51 } else { 85 };
    for y in 0..rows {
        let line = BAYER[((top + y as i32) & 3) as usize];
        let row = back.row_mut(y);
        for (x, pixel) in row[from..to].iter_mut().enumerate() {
            let t = line[(from + x) & 3];
            let off = (t * 2 - 15) * step / 32;
            let (c0, c1, c2) = split(*pixel);
            let q = |c: i32| ((c + off).clamp(0, 255) >> 3) as usize;
            *pixel = lut[q(c0) | (q(c1) << 5) | (q(c2) << 10)];
        }
    }
}

/// Нарисовать контур перетаскивания: рамка толщиной `t`, каждая вторая
/// точка в шахматку инвертирована (XOR, как в Windows 98) — видна на любом
/// фоне и не требует ни цвета, ни смешивания.
pub fn draw_outline(back: &mut Surface, band: Rect, dy: i32, rect: Rect, t: u32) {
    let t = t.max(1).min(rect.w / 2).min(rect.h / 2).max(1);
    let edges = [
        Rect::new(rect.x, rect.y, rect.w, t),
        Rect::new(rect.x, rect.bottom() - t as i32, rect.w, t),
        Rect::new(rect.x, rect.y + t as i32, t, rect.h.saturating_sub(t * 2)),
        Rect::new(rect.right() - t as i32, rect.y + t as i32, t, rect.h.saturating_sub(t * 2)),
    ];
    let width = back.width() as i32;
    let height = back.height() as i32;
    for edge in edges {
        let part = edge.intersect(&band);
        if part.is_empty() {
            continue;
        }
        for y in part.y..part.bottom() {
            let out_y = y + dy;
            if out_y < 0 || out_y >= height {
                continue;
            }
            let row = back.row_mut(out_y as u32);
            for x in part.x.max(0)..part.right().min(width) {
                if (x + y) & 1 == 0 {
                    row[x as usize] ^= 0x00FF_FFFF;
                }
            }
        }
    }
}

/// Четыре полосы рамки — то, что надо перерисовать, когда контур сдвинулся.
#[must_use]
pub fn outline_strips(rect: Rect, t: u32) -> [Rect; 4] {
    let t = t.max(1);
    [
        Rect::new(rect.x, rect.y, rect.w, t),
        Rect::new(rect.x, rect.bottom() - t as i32, rect.w, t),
        Rect::new(rect.x, rect.y, t, rect.h),
        Rect::new(rect.right() - t as i32, rect.y, t, rect.h),
    ]
}
