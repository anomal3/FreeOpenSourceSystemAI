//! Проход по окну и кирпичи раскладки «Параметров» (фаза 69b).
//!
//! # Почему пункты считает сам проход
//!
//! До 69b список пунктов раздела строился дважды: отрисовкой и отдельной
//! функцией для клавиатуры, и договор «порядок одинаков» держался на честном
//! слове. С подразделами и прокруткой таких списков стало бы два десятка.
//! Теперь проход записывает каждый пункт, мимо которого прошёл, в
//! [`Pass::order`] — вместе с прямоугольником, — и клавиатура ходит ровно по
//! тому, что нарисовано. Второму списку взяться неоткуда.
//!
//! # Почему прокрутка не режет строки пополам
//!
//! Поверхность окна не умеет обрезать рисование по прямоугольнику, а строка,
//! торчащая наполовину над заголовком страницы, налезала бы на него. Поэтому
//! элемент рисуется, только если помещается целиком ([`Pass::visible`]), и
//! невидимому не заводится попадание: щелчок не должен выполнять то, чего не
//! видно.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use super::{Deed, Section};

/// Куда попадает щелчок.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum Spot {
    /// Стрелка назад в заголовке (телефон) или хлебная крошка (ПК).
    Back,
    /// Строка бокового списка.
    Nav(Section),
    /// Поле «Найти параметр».
    Search,
    /// Пункт содержимого.
    Deed(Deed),
}

/// Прямоугольник вместе с тем, что за ним стоит.
pub(super) struct Hit {
    pub rect: Rect,
    pub spot: Spot,
}

/// Один проход по окну: он и рисует, и собирает то, по чему можно нажать.
pub(super) struct Pass<'a> {
    pub ctx: Ctx,
    /// Куда рисовать. `None` — проход только считает.
    pub surface: Option<&'a mut Surface>,
    pub hits: Vec<Hit>,
    /// Номер пункта, на котором стоит клавиатурный фокус.
    pub focus: Option<usize>,
    /// Пункты в порядке прохода — с прямоугольниками, даже невидимые.
    pub order: Vec<(Deed, Rect)>,
    /// Выше этой строки содержимое не рисуется (заголовок страницы).
    pub top: i32,
    /// Ниже этой строки содержимое не рисуется и не нажимается.
    pub limit: i32,
    /// Самый нижний край, до которого дошло содержимое, — для прокрутки.
    pub extent: i32,
}

impl Pass<'_> {
    /// Сделать что-то с поверхностью, если она есть.
    pub fn on(&mut self, f: impl FnOnce(&mut Surface)) {
        if let Some(surface) = self.surface.as_deref_mut() {
            f(surface);
        }
    }

    /// Помещается ли элемент в отведённое место целиком.
    pub fn visible(&mut self, rect: Rect) -> bool {
        self.extent = self.extent.max(rect.bottom());
        rect.y >= self.top && rect.bottom() <= self.limit
    }

    pub fn hit(&mut self, rect: Rect, spot: Spot) {
        self.hits.push(Hit { rect, spot });
    }

    /// Завести пункт. Возвращает `true`, если на нём стоит клавиатурный фокус.
    ///
    /// Номер выдаётся всегда, даже пункту, который не поместился: по нему
    /// ходят стрелки, и клавиатура обязана доходить до того, что сейчас за
    /// краем, — окно прокрутится к нему само.
    pub fn deed(&mut self, rect: Rect, deed: Deed) -> bool {
        self.deed_in(rect, rect, deed)
    }

    /// Пункт, у которого нажимается одно (`hit` — кнопка), а виден он вместе
    /// с другим (`view` — вся строка). Прокрутка за фокусом смотрит на
    /// `view`: кнопка, влезшая в окно без своей строки, не нарисуется вовсе.
    pub fn deed_in(&mut self, hit: Rect, view: Rect, deed: Deed) -> bool {
        let index = self.order.len();
        if self.visible(view) && !hit.is_empty() {
            self.hits.push(Hit { rect: hit, spot: Spot::Deed(deed.clone()) });
        }
        self.order.push((deed, view));
        self.focus == Some(index)
    }

    pub fn text(&mut self, role: Role, x: i32, y: i32, room: u32, t: &str, color: Color) {
        let ctx = self.ctx;
        self.on(|s| {
            paint::text_clipped(ctx, s, role, x, y, room, t, color);
        });
    }

    pub fn icon(&mut self, icon: Fi, x: i32, y: i32, size: u32, color: Color) {
        self.on(|s| iconset::draw(s, icon, x, y, size, color, 255));
    }

    /// Мелкое пояснение. Возвращает координату следующего элемента.
    pub fn note(&mut self, x: i32, y: i32, room: u32, t: &str) -> i32 {
        let ctx = self.ctx;
        let rect = Rect::new(x, y, room, line_h(ctx, Role::Caption) as u32);
        if self.visible(rect) {
            let color = ctx.palette.ink4;
            self.text(Role::Caption, x, y, room, t, color);
        }
        y + line_h(ctx, Role::Caption) + ctx.px(10) as i32
    }

    /// Заголовок группы строк («Рекомендуемые»). Возвращает координату строк.
    pub fn group(&mut self, x: i32, y: i32, w: u32, title: &str) -> i32 {
        let ctx = self.ctx;
        let y = y + ctx.px(14) as i32;
        let rect = Rect::new(x, y, w, line_h(ctx, Role::Strong) as u32);
        if self.visible(rect) {
            let ink = ctx.palette.ink;
            self.text(Role::Strong, x + ctx.px(2) as i32, y, w, title, ink);
        }
        y + line_h(ctx, Role::Strong) + ctx.px(8) as i32
    }
}

/// Высота строки этого начертания.
pub(super) fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// Ширина кнопки с такой подписью.
pub(super) fn button_width(ctx: Ctx, label: &str) -> u32 {
    ctx.face(Role::Body).width(label) + ctx.px(32)
}

/// Что стоит у правого края строки.
pub(super) enum Ctl<'a> {
    /// Ничего: строка только рассказывает.
    None,
    /// Значение текстом.
    Value(&'a str),
    /// Значение моноширинным — адреса, версии, размеры.
    Mono(&'a str),
    /// Стрелка «дальше»: строка открывает подраздел.
    Nav,
    /// Значение и стрелка.
    NavValue(&'a str),
    /// Переключатель.
    Toggle(bool),
    /// Кружок выбора: одно из нескольких.
    Radio(bool),
    /// Кнопка.
    Button(&'a str, Weight),
    /// Отметка.
    Chip(&'a str, Tone),
}

/// Строка-карточка макета: значок, подпись с пояснением, управление справа.
///
/// `deed` — что делает строка. У кнопки нажимается только кнопка (строку с
/// кнопкой «Удалить» нельзя удалить промахом мимо неё); у переключателя,
/// стрелки и кружка — вся строка, как в макете. Возвращает `y` следующей.
pub(super) fn row(
    pass: &mut Pass,
    x: i32,
    y: i32,
    w: u32,
    icon: Option<Fi>,
    title: &str,
    desc: &str,
    ctl: Ctl,
    deed: Option<Deed>,
) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let h = if desc.is_empty() { ctx.px(50) } else { ctx.px(60) };
    let rect = Rect::new(x, y, w, h);
    let next = y + (h + ctx.px(4)) as i32;

    // Место под управление справа.
    let pad = ctx.px(18) as i32;
    let control: Option<Rect> = match &ctl {
        Ctl::Button(label, _) => {
            let bw = button_width(ctx, label);
            let bh = ctx.px(32);
            Some(Rect::new(rect.right() - pad - bw as i32, rect.y + (h as i32 - bh as i32) / 2, bw, bh))
        }
        _ => None,
    };
    let whole = !matches!(ctl, Ctl::Button(..));
    let focused = match deed {
        Some(deed) => pass.deed_in(if whole { rect } else { control.unwrap_or(rect) }, rect, deed),
        None => false,
    };
    if !pass.visible(rect) {
        return next;
    }

    let card = ctx.flat(p.card);
    let inner = ctx.on(card);
    let radius = ctx.px(8);
    pass.on(|s| {
        draw::rounded(s, rect, radius, card, 255);
        draw::rounded_stroke(s, rect, radius, p.line2.color, p.line2.alpha);
        if focused {
            draw::rounded_stroke(s, rect, radius, p.acc, 255);
        }
    });

    let mut text_x = rect.x + pad;
    if let Some(icon) = icon {
        let size = ctx.px(22);
        let ink = p.ink2;
        pass.icon(icon, text_x, rect.y + (h as i32 - size as i32) / 2, size, ink);
        text_x += (size + ctx.px(16)) as i32;
    }

    // Справа налево: сначала управление, потом то, что осталось под текст.
    let mut right = rect.right() - pad;
    let value_ink = p.ink3;
    match ctl {
        Ctl::None => {}
        Ctl::Value(text) | Ctl::Mono(text) => {
            let role = if matches!(ctl, Ctl::Mono(_)) { Role::Mono } else { Role::Body };
            let room = (w / 2).min(ctx.face(role).width(text));
            let tx = right - room as i32;
            pass.text(role, tx, paint::baseline(ctx, role, rect), room, text, value_ink);
            right = tx - ctx.px(16) as i32;
        }
        Ctl::Nav => {
            right = chevron(pass, rect, right);
        }
        Ctl::NavValue(text) => {
            right = chevron(pass, rect, right);
            let room = (w / 3).min(ctx.face(Role::Body).width(text));
            let tx = right - room as i32;
            pass.text(Role::Body, tx, paint::baseline(ctx, Role::Body, rect), room, text, value_ink);
            right = tx - ctx.px(16) as i32;
        }
        Ctl::Toggle(on) => {
            let sw = Rect::new(right - ctx.px(42) as i32, rect.y + (h as i32 - ctx.px(22) as i32) / 2, ctx.px(42), ctx.px(22));
            pass.on(|s| toggle(inner, s, sw, on));
            let label = if on { "Вкл." } else { "Откл." };
            let lw = ctx.face(Role::Body).width(label);
            let tx = sw.x - ctx.px(12) as i32 - lw as i32;
            pass.text(Role::Body, tx, paint::baseline(ctx, Role::Body, rect), lw, label, value_ink);
            right = tx - ctx.px(12) as i32;
        }
        Ctl::Radio(on) => {
            let d = ctx.px(18);
            let cx = right - d as i32 / 2;
            let cy = rect.y + h as i32 / 2;
            pass.on(|s| {
                if on {
                    draw::circle(s, cx, cy, d / 2, p.acc, 255);
                    draw::circle(s, cx, cy, d / 4, card, 255);
                } else {
                    draw::circle_outline(s, cx, cy, d / 2, ctx.px(2), p.ink4, 255);
                }
            });
            right -= (d + ctx.px(14)) as i32;
        }
        Ctl::Button(label, weight) => {
            if let Some(button) = control {
                pass.on(|s| paint::button(inner, s, button, weight, label, focused));
                right = button.x - ctx.px(14) as i32;
            }
        }
        Ctl::Chip(label, tone) => {
            let cw = paint::chip_width(ctx, label);
            let chip = Rect::new(right - cw as i32, rect.y + (h as i32 - ctx.px(22) as i32) / 2, cw, ctx.px(22));
            pass.on(|s| paint::chip(inner, s, chip, label, tone));
            right = chip.x - ctx.px(14) as i32;
        }
    }

    let room = (right - text_x).max(0) as u32;
    let title_h = line_h(ctx, Role::Body);
    if desc.is_empty() {
        pass.text(Role::Body, text_x, paint::baseline(ctx, Role::Body, rect), room, title, p.ink);
    } else {
        let both = title_h + ctx.px(3) as i32 + line_h(ctx, Role::Caption);
        let top = rect.y + (h as i32 - both) / 2;
        pass.text(Role::Body, text_x, top, room, title, p.ink);
        pass.text(Role::Caption, text_x, top + title_h + ctx.px(3) as i32, room, desc, p.ink4);
    }
    next
}

/// Стрелка «дальше» у правого края. Возвращает новый правый край.
fn chevron(pass: &mut Pass, rect: Rect, right: i32) -> i32 {
    let ctx = pass.ctx;
    let size = ctx.px(18);
    let ink = ctx.palette.ink4;
    pass.icon(Fi::ChevronRight, right - size as i32, rect.y + (rect.h as i32 - size as i32) / 2, size, ink);
    right - (size + ctx.px(12)) as i32
}

/// Переключатель макета: пилюля 42×22, у включённого — заливка акцентом.
pub(super) fn toggle(ctx: Ctx, s: &mut Surface, area: Rect, on: bool) {
    let p = ctx.palette;
    let r = area.h / 2;
    let knob = area.h.saturating_sub(ctx.px(10)) / 2;
    let cy = area.y + area.h as i32 / 2;
    if on {
        draw::rounded(s, area, r, p.acc, 255);
        let cx = area.right() - r as i32;
        draw::circle(s, cx, cy, knob, Color::rgb(0x0F, 0x12, 0x18), 255);
    } else {
        draw::rounded_stroke(s, area, r, p.ink4, 255);
        let cx = area.x + r as i32;
        draw::circle(s, cx, cy, knob, p.ink3, 255);
    }
}

/// Карточка без строк — подложка «героя» страницы.
pub(super) fn hero_card(pass: &mut Pass, rect: Rect) -> bool {
    let ctx = pass.ctx;
    let p = ctx.palette;
    if !pass.visible(rect) {
        return false;
    }
    let card = ctx.flat(p.card);
    let radius = ctx.px(10);
    pass.on(|s| {
        draw::rounded(s, rect, radius, card, 255);
        draw::rounded_stroke(s, rect, radius, p.line2.color, p.line2.alpha);
    });
    true
}

/// Круг с галочкой («Система актуальна», «Загрузка подтверждена»).
pub(super) fn ok_badge(pass: &mut Pass, cx: i32, cy: i32, d: u32, tone: Tone) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let (fill, icon) = match tone {
        Tone::Ok => (p.ok, Fi::Check),
        Tone::Warn => (p.warn, Fi::Warning),
        Tone::Bad => (p.bad, Fi::X),
        _ => (p.acc, Fi::Info),
    };
    let glyph = d * 3 / 5;
    pass.on(|s| {
        draw::circle(s, cx, cy, d / 2, fill, 255);
        iconset::draw(s, icon, cx - glyph as i32 / 2, cy - glyph as i32 / 2, glyph, Color::rgb(0x0F, 0x12, 0x18), 255);
    });
}

/// Полоса заполнения с подписью «занято из» справа над ней.
pub(super) fn usage_bar(pass: &mut Pass, x: i32, y: i32, w: u32, label: &str, used: u64, total: u64) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let text = alloc::format!("{} из {}", size_text(used), size_text(total));
    let rect = Rect::new(x, y, w, (line_h(ctx, Role::Body) + ctx.px(14) as i32) as u32);
    if pass.visible(rect) {
        pass.text(Role::Body, x, y, w / 2, label, p.ink);
        let tw = ctx.face(Role::Caption).width(&text);
        pass.text(Role::Caption, x + w as i32 - tw as i32, y + ctx.px(2) as i32, tw, &text, p.ink3);
        let bar = Rect::new(x, y + line_h(ctx, Role::Body) + ctx.px(8) as i32, w, ctx.px(6));
        let (done, all) = scale_pair(used, total);
        let tone = if total > 0 && used * 10 > total * 9 { Tone::Warn } else { Tone::Accent };
        pass.on(|s| paint::progress(ctx, s, bar, done, all, tone));
    }
    rect.bottom() + ctx.px(6) as i32
}

/// Привести пару к `u32` без потери отношения.
fn scale_pair(used: u64, total: u64) -> (u32, u32) {
    let mut used = used;
    let mut total = total;
    while total > u64::from(u32::MAX) {
        used >>= 10;
        total >>= 10;
    }
    (used as u32, total as u32)
}

/// Размер словами: «26,4 ГиБ», «512 МиБ», «14 КиБ».
pub(super) fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КиБ", "МиБ", "ГиБ", "ТиБ"];
    let mut unit = 0;
    let mut tenths = bytes * 10;
    while tenths >= 10 * 1024 && unit < UNITS.len() - 1 {
        tenths /= 1024;
        unit += 1;
    }
    if unit == 0 || tenths >= 1000 || tenths % 10 == 0 {
        alloc::format!("{} {}", (tenths + 5) / 10, UNITS[unit])
    } else {
        alloc::format!("{},{} {}", tenths / 10, tenths % 10, UNITS[unit])
    }
}

/// Скругление по умолчанию для строк бокового списка.
pub(super) const fn nav_radius(ctx: Ctx) -> u32 {
    ctx.px(theme::R_TAB)
}
