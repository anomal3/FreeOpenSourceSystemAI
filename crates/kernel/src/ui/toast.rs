//! Уведомление над треем (фаза 72h-2, макет 19): «Chromium установлен».
//!
//! Пока одно событие — поставленная программа из пакета: стол замечает её
//! при перечитывании реестра и показывает карточку с кнопками «Открыть» и
//! «Закрыть». Через [`LIFETIME_MS`] карточка уходит сама.
//!
//! Размеры из таблицы макета: ширина 364, скругление 8, поле 16, до панели и
//! до края 12; шапка — значок 16, «Пакеты», «сейчас» и крестик; заголовок
//! 14 полужирным, текст приглушённый; кнопки высотой 32 с зазором 8.

use alloc::string::String;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw, theme};

use super::appicon::Art;

/// Сколько уведомление висит само.
pub const LIFETIME_MS: u64 = 10_000;

/// Что нажали.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Open,
    Close,
    Inside,
}

/// Уведомление о поставленной программе.
pub struct Toast {
    pub rect: Rect,
    surface: Surface,
    /// Командная строка программы и её права — для «Открыть».
    pub start: String,
    pub rights: fpk::Rights,
    pub until_ms: u64,
    open: Rect,
    close: Rect,
    cross: Rect,
}

/// Уведомление в истории (колокол трея, 02.11): заголовок, текст и когда.
#[derive(Clone)]
pub struct Note {
    pub title: alloc::string::String,
    pub text: alloc::string::String,
    pub at_ms: u64,
}

/// Сколько уведомлений помнить.
const NOTES_MAX: usize = 20;

/// История уведомлений — новые первыми.
static NOTES: crate::sync::SpinLock<alloc::vec::Vec<Note>> = crate::sync::SpinLock::new(alloc::vec::Vec::new());

/// Запомнить уведомление.
pub fn remember(title: &str, text: &str) {
    let mut notes = NOTES.lock();
    if notes.try_reserve(1).is_err() {
        return;
    }
    notes.insert(0, Note { title: alloc::string::String::from(title), text: alloc::string::String::from(text), at_ms: crate::time::uptime_ms() });
    notes.truncate(NOTES_MAX);
}

/// Уведомления, новые первыми.
#[must_use]
pub fn notes() -> alloc::vec::Vec<Note> {
    NOTES.lock().clone()
}

/// Забыть все («Очистить»).
pub fn clear_notes() {
    NOTES.lock().clear();
}

/// «сейчас», «5 мин назад», «2 ч назад».
#[must_use]
pub fn ago(at_ms: u64) -> alloc::string::String {
    let minutes = crate::time::uptime_ms().saturating_sub(at_ms) / 60_000;
    match minutes {
        0 => alloc::string::String::from("сейчас"),
        1..=59 => alloc::format!("{minutes} мин назад"),
        _ => alloc::format!("{} ч назад", minutes / 60),
    }
}

impl Toast {
    #[must_use]
    pub fn new(caption: &str, art: &Art, start: String, rights: fpk::Rights, area: Rect, panel_top: i32, scale: mini_ui::Scale) -> Option<Self> {
        remember(&alloc::format!("{caption} установлен"), "Пакеты · программа готова к запуску");
        Self::build(None, "Пакеты", &alloc::format!("{caption} установлен"), "Программа готова к запуску.", "Открыть", art, start, rights, area, panel_top, scale)
    }

    /// Any notification with one action (22.11, «Запись сохранена» —
    /// «Открыть папку»): `head` over the card, `title` and `text` beside the
    /// program icon, `open` runs `start` with `rights`.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn notice(head: &str, title: &str, text: &str, open: &str, art: &Art, start: String, rights: fpk::Rights, area: Rect, panel_top: i32, scale: mini_ui::Scale) -> Option<Self> {
        remember(title, &alloc::format!("{head} · {text}"));
        Self::build(Some(art), head, title, text, open, art, start, rights, area, panel_top, scale)
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        head_art: Option<&Art>,
        head_text: &str,
        title: &str,
        text: &str,
        open_label: &str,
        art: &Art,
        start: String,
        rights: fpk::Rights,
        area: Rect,
        panel_top: i32,
        scale: mini_ui::Scale,
    ) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        let p = ctx.palette;
        let px = |value: u32| value * scale;
        let (w, h) = (px(364), px(16 + 20 + 12 + 40 + 16 + 32 + 16));
        let rect = Rect::new(area.right() - px(12) as i32 - w as i32, panel_top - px(12) as i32 - h as i32, w, h);
        let fill = p.tb1.over(p.tb2.color);
        let mut surface = Surface::new(w, h, fill)?;
        let ctx = ctx.on(fill);
        let bounds = surface.bounds();
        draw::rounded_stroke(&mut surface, bounds, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let s = &mut surface;
        let left = px(16) as i32;

        // Шапка: значок пакетов, «Пакеты», «сейчас», крестик.
        let head = Rect::new(left, px(16) as i32, w - px(32), px(20));
        match head_art {
            Some(small) => small.draw(ctx, s, Rect::new(left, head.y + px(2) as i32, px(16), px(16))),
            None => {
                if let Some(image) = super::appicon::design("packages") {
                    image.draw(s, Rect::new(left, head.y + px(2) as i32, px(16), px(16)));
                }
            }
        }
        paint::text(ctx, s, Role::Caption, left + px(24) as i32, paint::baseline(ctx, Role::Caption, head), head_text, p.ink3);
        let cross = Rect::new(head.right() - px(24) as i32, head.y - px(2) as i32, px(24), px(24));
        let glyph = px(10);
        glyphicon::draw(s, Icon::Close, cross.x + (cross.w as i32 - glyph as i32) / 2, cross.y + (cross.h as i32 - glyph as i32) / 2, glyph, p.ink3, 255);
        paint::text_right(ctx, s, Role::Caption, cross.x - px(8) as i32, paint::baseline(ctx, Role::Caption, head), "сейчас", p.ink4);

        // Тело: значок программы, заголовок, текст.
        let body_y = head.bottom() + px(12) as i32;
        art.draw(ctx, s, Rect::new(left, body_y + px(2) as i32, px(36), px(36)));
        let tx = left + px(50) as i32;
        let room = (w as i32 - px(16) as i32 - tx).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Label, tx, body_y, room, title, p.ink);
        paint::text_clipped(ctx, s, Role::Caption, tx, body_y + px(20) as i32, room, text, p.ink3);

        // Кнопки.
        let by = body_y + px(40 + 16) as i32;
        let face = ctx.face(Role::Body);
        let open = Rect::new(left, by, face.width(open_label) + px(32), px(32));
        let close = Rect::new(open.right() + px(8) as i32, by, face.width("Закрыть") + px(32), px(32));
        draw::rounded(s, open, px(theme::R_ROW), p.acc, 255);
        paint::text_centered(ctx.on(p.acc), s, Role::Label, open, open_label, Color::rgb(0xFF, 0xFF, 0xFF));
        draw::rounded(s, close, px(theme::R_ROW), p.btn.over(fill), 255);
        draw::rounded_stroke(s, close, px(theme::R_ROW), p.btnline.color, p.btnline.alpha);
        paint::text_centered(ctx.on(p.btn.over(fill)), s, Role::Body, close, "Закрыть", p.ink);

        let at = |r: Rect| r.translate(rect.x, rect.y);
        Some(Self {
            rect,
            surface,
            start,
            rights,
            until_ms: crate::time::uptime_ms() + LIFETIME_MS,
            open: at(open),
            close: at(close),
            cross: at(cross),
        })
    }

    /// Move the card (and its buttons) to `rect.x, rect.y`.
    pub fn move_to(&mut self, rect: Rect) {
        let (dx, dy) = (rect.x - self.rect.x, rect.y - self.rect.y);
        self.rect = self.rect.translate(dx, dy);
        self.open = self.open.translate(dx, dy);
        self.close = self.close.translate(dx, dy);
        self.cross = self.cross.translate(dx, dy);
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if self.open.contains(x, y) {
            Some(Hit::Open)
        } else if self.close.contains(x, y) || self.cross.contains(x, y) {
            Some(Hit::Close)
        } else if self.rect.contains(x, y) {
            Some(Hit::Inside)
        } else {
            None
        }
    }
}
