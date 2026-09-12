//! Словарь элементов: из чего собраны все окна стола.
//!
//! # Зачем отдельный модуль
//!
//! Кнопка «Применить» в параметрах и кнопка «Открыть» в файловом менеджере —
//! одна и та же кнопка. Пока каждое окно рисует её само, они одинаковы ровно до
//! первой правки: кто-то поправит скругление у себя, и стол разъедется на
//! четыре разных стола. Здесь собрано то, что повторяется, и правка скругления
//! делается один раз.
//!
//! # Почему [`Ctx`], а не глобальные функции
//!
//! Рисующему нужны три вещи разом: текущая палитра, размерный ряд шрифта и
//! множитель геометрии. Тащить их тремя параметрами через каждый вызов —
//! верный способ однажды передать палитру одной темы вместе с кеглем другой.
//! Контекст создаётся один раз на кадр и дальше передаётся целиком.
//!
//! # Почему всё принимает `Rect`, а не координаты
//!
//! Потому что вызывающий и так считает раскладку прямоугольниками, и половина
//! ошибок раскладки — это перепутанные местами ширина и высота в списке из
//! четырёх чисел.

use crate::draw;
use crate::glyphicon::{self, Icon};
use crate::theme::{self, Ink, Palette};
use crate::typeface::{self, Face, Role, Tier};
use crate::{Color, Rect, Surface};

/// Всё, что нужно знать, чтобы нарисовать элемент.
#[derive(Clone, Copy)]
pub struct Ctx {
    pub palette: &'static Palette,
    pub tier: Tier,
    /// Множитель геометрии: 1 на обычном экране, 2 на очень плотном.
    pub scale: u32,
    /// Цвет, поверх которого рисуют. Нужен сведению полупрозрачных токенов
    /// там, где подложка не окно: карточка внутри карточки, панель на обоях.
    pub under: Color,
}

impl Ctx {
    /// Контекст по уже выбранному множителю геометрии.
    ///
    /// Окно ширины экрана не помнит и помнить не должно: его перенесут на
    /// другой экран, а множитель у стола один. Ряд шрифта выводится из
    /// множителя, потому что порог у них общий — тот же 2560.
    #[must_use]
    pub fn scaled(scale: u32) -> Self {
        Self {
            palette: theme::palette(),
            tier: if scale >= 2 { Tier::Large } else { Tier::Normal },
            scale: scale.max(1),
            under: theme::window_bg(),
        }
    }

    /// Тот же контекст, но с другой подложкой.
    ///
    /// Возвращает копию, а не меняет себя: вложенная карточка рисуется поверх
    /// панели, но соседняя с ней — по-прежнему поверх окна, и забытый возврат
    /// подложки обратно даёт расхождение в один-два уровня яркости, которое
    /// глазом не поймать, а на снимке видно.
    #[must_use]
    pub const fn on(self, under: Color) -> Self {
        Self { under, ..self }
    }

    /// Размер из макета, приведённый к этому экрану.
    #[must_use]
    pub const fn px(self, value: u32) -> u32 {
        value * self.scale
    }

    /// Начертание для роли.
    #[must_use]
    pub fn face(self, role: Role) -> &'static Face {
        typeface::face(role, self.tier)
    }

    /// Свести токен к непрозрачному цвету поверх текущей подложки.
    #[must_use]
    pub fn flat(self, ink: Ink) -> Color {
        ink.over(self.under)
    }
}

// ── Текст ────────────────────────────────────────────────────────────────────

/// Написать строку. Возвращает её ширину.
pub fn text(ctx: Ctx, s: &mut Surface, role: Role, x: i32, y: i32, t: &str, color: Color) -> u32 {
    typeface::draw(s, ctx.face(role), x, y, t, color, 255)
}

/// Написать строку, обрезав по ширине многоточием.
///
/// Параметров восемь, и свести их в структуру было бы хуже: каждый из них
/// меняется на каждом вызове, и структура из восьми обязательных полей — это
/// тот же список, только записанный длиннее.
#[allow(clippy::too_many_arguments)]
pub fn text_clipped(
    ctx: Ctx,
    s: &mut Surface,
    role: Role,
    x: i32,
    y: i32,
    room: u32,
    t: &str,
    color: Color,
) -> u32 {
    typeface::draw_clipped(s, ctx.face(role), x, y, t, room, color, 255)
}

/// Написать строку по центру прямоугольника.
pub fn text_centered(ctx: Ctx, s: &mut Surface, role: Role, area: Rect, t: &str, color: Color) {
    typeface::draw_centered(s, ctx.face(role), area, t, color, 255);
}

/// Написать строку, прижав её к правому краю.
pub fn text_right(ctx: Ctx, s: &mut Surface, role: Role, right: i32, y: i32, t: &str, color: Color) {
    typeface::draw_right(s, ctx.face(role), right, y, t, color, 255);
}

/// Заголовок секции: прописные вразрядку.
///
/// Разрядка здесь не по вкусу, а по необходимости: прописные без просвета
/// сливаются в сплошную полосу, и «СИСТЕМНЫЕ СЛОТЫ» читается как одно длинное
/// слово.
pub fn caps(ctx: Ctx, s: &mut Surface, x: i32, y: i32, t: &str) -> u32 {
    typeface::draw_tracked(
        s,
        ctx.face(Role::MonoCaps),
        x,
        y,
        t,
        ctx.palette.ink5,
        255,
        theme::CAPS_TRACKING * ctx.scale,
    )
}

/// Выровнять строку по вертикали внутри полосы такой высоты.
///
/// Отдельная функция, потому что «по центру» для текста — это не центр
/// прямоугольника строки, а центр её высоты, и разница в одну точку заметна на
/// каждой кнопке.
#[must_use]
pub fn baseline(ctx: Ctx, role: Role, area: Rect) -> i32 {
    area.y + (area.h as i32 - i32::from(ctx.face(role).line)) / 2
}

// ── Поверхности ──────────────────────────────────────────────────────────────

/// Карточка: слабая заливка и линия по контуру.
pub fn card(ctx: Ctx, s: &mut Surface, area: Rect) {
    let p = ctx.palette;
    draw::rounded(s, area, ctx.px(theme::R_CARD), ctx.flat(p.card), 255);
    draw::rounded_stroke(s, area, ctx.px(theme::R_CARD), p.line2.color, p.line2.alpha);
}

/// Утопленное поле: строка адреса, поиск, дорожка полосы хода.
pub fn sunk(ctx: Ctx, s: &mut Surface, area: Rect, radius: u32) {
    let p = ctx.palette;
    draw::rounded(s, area, radius, ctx.flat(p.sunk), 255);
    draw::rounded_stroke(s, area, radius, p.line2.color, p.line2.alpha);
}

/// Разделительная линия на всю ширину.
pub fn separator(ctx: Ctx, s: &mut Surface, x: i32, y: i32, w: u32) {
    let p = ctx.palette;
    draw::hline(s, x, y, w, p.line.color, p.line.alpha);
}

// ── Состояния строки списка ──────────────────────────────────────────────────

/// Как выглядит строка списка.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// Обычная.
    Idle,
    /// Под указателем.
    Hover,
    /// Выбранная.
    Selected,
}

/// Подложка строки списка.
///
/// Выбранная строка — не сплошная заливка акцентом, а градиент от него к почти
/// прозрачному: сплошная полоса поперёк окна перетягивает на себя внимание с
/// того, что в ней написано.
pub fn row(ctx: Ctx, s: &mut Surface, area: Rect, state: RowState) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_ROW);
    match state {
        RowState::Idle => {}
        RowState::Hover => {
            draw::rounded(s, area, r, p.hover1.color, p.hover1.alpha);
        }
        RowState::Selected => {
            draw::horizontal_gradient(
                s,
                area,
                r,
                p.sel1.over(ctx.under),
                p.sel2.over(ctx.under),
                255,
            );
            draw::rounded_stroke(s, area, r, p.accedge, 255);
        }
    }
}

/// Цвет текста строки списка в этом состоянии.
#[must_use]
pub fn row_ink(ctx: Ctx, state: RowState) -> Color {
    match state {
        RowState::Selected => ctx.palette.ink,
        RowState::Hover => ctx.palette.ink2,
        RowState::Idle => ctx.palette.ink3,
    }
}

// ── Кнопки ───────────────────────────────────────────────────────────────────

/// Вес кнопки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// Основное действие: заливка акцентом, белый текст.
    Primary,
    /// Обычное: слабая заливка и контур.
    Normal,
    /// Тихое: ничего, пока на неё не навели.
    Ghost,
    /// Опасное: закрыть, выключить, удалить.
    Danger,
}

/// Нарисовать кнопку с подписью.
pub fn button(ctx: Ctx, s: &mut Surface, area: Rect, weight: Weight, label: &str, hover: bool) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_ROW);
    let ink = match weight {
        Weight::Primary => {
            draw::rounded_gradient(s, area, r, p.acc, p.acc2, 255);
            draw::rounded_stroke(s, area, r, p.accline.color, p.accline.alpha);
            Color::rgb(0xFF, 0xFF, 0xFF)
        }
        Weight::Normal => {
            let fill = if hover { p.btnhov } else { p.btn };
            draw::rounded(s, area, r, ctx.flat(fill), 255);
            draw::rounded_stroke(s, area, r, p.btnline.color, p.btnline.alpha);
            p.ink2
        }
        Weight::Ghost => {
            if hover {
                draw::rounded(s, area, r, ctx.flat(p.ghost), 255);
            }
            if hover { p.ink2 } else { p.ink3 }
        }
        Weight::Danger => {
            if hover {
                draw::rounded(s, area, r, p.bad, 255);
            } else {
                draw::rounded(s, area, r, ctx.flat(p.badbg), 255);
                draw::rounded_stroke(s, area, r, p.badline, 255);
            }
            if hover { Color::rgb(0xFF, 0xFF, 0xFF) } else { p.bad_ink }
        }
    };
    let role = if weight == Weight::Primary { Role::Label } else { Role::Body };
    text_centered(ctx, s, role, area, label, ink);
}

/// Квадратная кнопка со значком: заголовок окна, панель инструментов.
pub fn icon_button(ctx: Ctx, s: &mut Surface, area: Rect, icon: Icon, weight: Weight, hover: bool) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_CHIP);
    let ink = match weight {
        Weight::Primary => {
            draw::rounded_gradient(s, area, r, p.acc, p.acc2, 255);
            draw::rounded_stroke(s, area, r, p.accline.color, p.accline.alpha);
            Color::rgb(0xFF, 0xFF, 0xFF)
        }
        Weight::Normal => {
            let fill = if hover { p.btnhov } else { p.btn };
            draw::rounded(s, area, r, ctx.flat(fill), 255);
            draw::rounded_stroke(s, area, r, p.btnline.color, p.btnline.alpha);
            p.ink3
        }
        Weight::Ghost => {
            if hover {
                draw::rounded(s, area, r, ctx.flat(p.ghost), 255);
            }
            draw::rounded_stroke(s, area, r, p.line2.color, p.line2.alpha);
            p.ink6
        }
        Weight::Danger => {
            if hover {
                draw::rounded(s, area, r, p.bad, 255);
                Color::rgb(0xFF, 0xFF, 0xFF)
            } else {
                draw::rounded(s, area, r, ctx.flat(p.badbg), 255);
                draw::rounded_stroke(s, area, r, p.badline, 255);
                p.bad_ink
            }
        }
    };
    // Значок занимает чуть больше половины кнопки: в макете это 11 точек в
    // кнопке 30, то есть та же пропорция, что у штриховых значков 16 в плитке 46.
    let side = area.w.min(area.h) * 11 / 30;
    let x = area.x + (area.w as i32 - side as i32) / 2;
    let y = area.y + (area.h as i32 - side as i32) / 2;
    glyphicon::draw(s, icon, x, y, side, ink, 255);
}

// ── Мелочь ───────────────────────────────────────────────────────────────────

/// Оттенок отметки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Accent,
    Ok,
    Warn,
    Bad,
    /// Без цвета: только контур и приглушённый текст.
    Muted,
}

impl Tone {
    /// Заливка, контур и цвет текста для этого оттенка.
    fn parts(self, p: &Palette) -> (Option<Ink>, Option<Color>, Color) {
        match self {
            Self::Accent => (Some(p.acctint), Some(p.accedge), p.acc_ink),
            Self::Ok => (Some(p.okbg), Some(p.okline), p.ok_ink),
            Self::Warn => (Some(p.warnbg), Some(p.warnline), p.warn),
            Self::Bad => (Some(p.badbg), Some(p.badline), p.bad_ink),
            Self::Muted => (None, None, p.ink4),
        }
    }
}

/// Отметка-пилюля: «ЗАГРУЖЕН», «ОТКАТ», «3 РАБОТАЮТ».
pub fn chip(ctx: Ctx, s: &mut Surface, area: Rect, label: &str, tone: Tone) {
    let p = ctx.palette;
    let (fill, edge, ink) = tone.parts(p);
    let r = area.h / 2;
    if let Some(fill) = fill {
        draw::rounded(s, area, r, ctx.flat(fill), 255);
    }
    if let Some(edge) = edge {
        draw::rounded_stroke(s, area, r, edge, 255);
    } else {
        draw::rounded_stroke(s, area, r, p.line2.color, p.line2.alpha);
    }
    text_centered(ctx, s, Role::MonoCaps, area, label, ink);
}

/// Ширина, которая понадобится отметке с такой подписью.
#[must_use]
pub fn chip_width(ctx: Ctx, label: &str) -> u32 {
    ctx.face(Role::MonoCaps).width(label) + ctx.px(20)
}

/// Полоса хода.
pub fn progress(ctx: Ctx, s: &mut Surface, area: Rect, done: u32, total: u32, tone: Tone) {
    let p = ctx.palette;
    let r = area.h / 2;
    draw::rounded(s, area, r, ctx.flat(p.sunk), 255);
    draw::rounded_stroke(s, area, r, p.line2.color, p.line2.alpha);
    if total == 0 || done == 0 {
        return;
    }
    let filled = (u64::from(area.w) * u64::from(done.min(total)) / u64::from(total)) as u32;
    // Скругление не даёт полосе стать короче собственного скругления: иначе
    // единственный процент рисуется каплей шире, чем должен.
    let filled = filled.max(area.h);
    let bar = Rect::new(area.x, area.y, filled.min(area.w), area.h);
    match tone {
        Tone::Accent => draw::horizontal_gradient(s, bar, r, p.acc2, p.acc, 255),
        Tone::Ok => draw::rounded(s, bar, r, p.ok, 255),
        Tone::Warn => draw::rounded(s, bar, r, p.warn, 255),
        Tone::Bad => draw::rounded(s, bar, r, p.bad, 255),
        Tone::Muted => draw::rounded(s, bar, r, p.ink6, 255),
    }
}

/// Плитка со значком: цветная — для выбранного и главного, серая — для прочего.
pub fn icon_tile(ctx: Ctx, s: &mut Surface, area: Rect, icon: Icon, tone: Tone, filled: bool) {
    let p = ctx.palette;
    // Скругление считается от стороны, а не берётся числом: одна и та же плитка
    // бывает и 20 точек в строке меню, и 46 на столе, и постоянный радиус
    // делает мелкую круглой, а крупную — почти квадратной. Три десятых стороны
    // повторяют пропорцию макета на всех его размерах.
    let r = (area.w.min(area.h) * 3 / 10).max(3);
    let ink = if filled {
        let (top, bottom, edge) = match tone {
            Tone::Ok => (p.ok, p.ok2, p.okline),
            Tone::Bad => (p.bad, p.bad, p.badline),
            Tone::Warn => (p.warn, p.warn, p.warnline),
            _ => (p.acc, p.acc2, p.accedge),
        };
        // Диагональный градиент макета (`160deg`) сведён к вертикальному:
        // разница на плитке в тридцать точек — полтона, а стоит она отдельного
        // прохода по каждой точке.
        draw::rounded_gradient(s, area, r, top, bottom, 255);
        draw::rounded_stroke(s, area, r, edge, 255);
        Color::rgb(0xFF, 0xFF, 0xFF)
    } else {
        draw::rounded(s, area, r, ctx.flat(p.ghost), 255);
        draw::rounded_stroke(s, area, r, p.btnline.color, p.btnline.alpha);
        p.ink3
    };
    let side = area.w.min(area.h) * 4 / 7;
    let x = area.x + (area.w as i32 - side as i32) / 2;
    let y = area.y + (area.h as i32 - side as i32) / 2;
    glyphicon::draw(s, icon, x, y, side, ink, 255);
}

/// Значок программы в заголовке окна и в меню: скруглённый квадрат с акцентом.
pub fn badge(ctx: Ctx, s: &mut Surface, area: Rect, tone: Tone) {
    let p = ctx.palette;
    let r = ctx.px(6).min(area.w / 3);
    let (top, bottom, edge) = match tone {
        Tone::Ok => (p.ok, p.ok2, p.okline),
        Tone::Muted => (p.line3.over(ctx.under), p.line3.over(ctx.under), p.line2.over(ctx.under)),
        _ => (p.acc, p.acc2, p.accedge),
    };
    draw::rounded_gradient(s, area, r, top, bottom, 255);
    draw::rounded_stroke(s, area, r, edge, 255);
}
