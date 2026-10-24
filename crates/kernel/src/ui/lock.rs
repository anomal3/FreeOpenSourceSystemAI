//! Экран блокировки и входа (фаза 72h-4, макет 20).
//!
//! Win+L закрывает стол: обои, крупные часы и дата, «Нажмите любую
//! клавишу». Клавиша или щелчок — карточка входа: буква имени в круге, имя,
//! поле пароля точками. Пароль сверяется с отпечатком из `/etc/passwd`
//! (`user::session::verify_password`); у учётной записи без пароля (живая
//! система) Enter просто открывает стол.
//!
//! При загрузке экран не показывается: сеанс входит сам, как и прежде.
//!
//! Шрифта крупнее заголовка в системе нет, поэтому часы рисуются обычным
//! заголовком в маску и увеличиваются втрое со сглаживанием.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw, theme};

/// Что на экране.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// Часы и подсказка.
    Clock,
    /// Карточка с полем пароля.
    Password,
}

/// Чем кончилось нажатие.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Перерисовать.
    Redraw,
    /// Пароль верен — открыть стол.
    Unlock,
    /// Нажали «Выключение».
    Power,
    Nothing,
}

const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
/// Во сколько раз часы крупнее заголовка.
const CLOCK_ZOOM: u32 = 3;

pub struct Lock {
    pub phase: Phase,
    typed: String,
    wrong: bool,
    user: String,
    /// Часы, нарисованные в последний раз: сменились — перерисовать.
    pub shown_clock: String,
    power: Rect,
}

impl Lock {
    #[must_use]
    pub fn new() -> Self {
        let user = crate::user::session::with_name(|name| String::from(name));
        Self { phase: Phase::Clock, typed: String::new(), wrong: false, user, shown_clock: String::new(), power: Rect::EMPTY }
    }

    /// Нажатие клавиши: символ (если есть) и признаки Enter / Backspace /
    /// Esc.
    pub fn key(&mut self, ch: Option<char>, enter: bool, backspace: bool, escape: bool) -> Outcome {
        if self.phase == Phase::Clock {
            self.phase = Phase::Password;
            return Outcome::Redraw;
        }
        if escape {
            self.typed.clear();
            self.wrong = false;
            self.phase = Phase::Clock;
            return Outcome::Redraw;
        }
        if enter {
            return match crate::user::session::verify_password(&self.typed) {
                None | Some(true) => Outcome::Unlock,
                Some(false) => {
                    self.typed.clear();
                    self.wrong = true;
                    Outcome::Redraw
                }
            };
        }
        if backspace {
            self.typed.pop();
            return Outcome::Redraw;
        }
        match ch {
            Some(ch) if !ch.is_control() && self.typed.chars().count() < 32 => {
                self.typed.push(ch);
                self.wrong = false;
                Outcome::Redraw
            }
            _ => Outcome::Nothing,
        }
    }

    /// Щелчок.
    pub fn click(&mut self, x: i32, y: i32) -> Outcome {
        if self.power.contains(x, y) {
            return Outcome::Power;
        }
        if self.phase == Phase::Clock {
            self.phase = Phase::Password;
            return Outcome::Redraw;
        }
        Outcome::Nothing
    }

    /// Нарисовать экран поверх уже нарисованных обоев в `s` (весь экран).
    pub fn draw(&mut self, s: &mut Surface, scale: u32) {
        let ctx = Ctx::scaled(scale);
        let p = ctx.palette;
        let px = |value: u32| value * scale;
        let screen = s.bounds();
        // Обои притушены, как у Windows.
        draw::blend_rect(s, screen, Color::rgb(0x04, 0x08, 0x14), 110);
        let clock = crate::time::clock_text().unwrap_or_default();
        self.shown_clock.clone_from(&clock);
        let date = crate::time::now_local().map(|now| {
            const MONTHS: [&str; 12] = [
                "января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря",
            ];
            const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
            let days = calendar::days_from_civil(i64::from(now.year), now.month, now.day);
            alloc::format!("{}, {} {}", DAYS[(days + 3).rem_euclid(7) as usize], now.day, MONTHS[usize::from(now.month.max(1) - 1) % 12])
        });

        match self.phase {
            Phase::Clock => {
                let x = px(88) as i32;
                let y = screen.h as i32 / 6;
                big_text(ctx, s, x, y, &clock, scale);
                let line = i32::from(ctx.face(Role::Heading).line) * CLOCK_ZOOM as i32;
                if let Some(date) = &date {
                    paint::text(ctx, s, Role::Heading, x + px(4) as i32, y + line, date, Color::rgb(0xD6, 0xDE, 0xEC));
                }
                let hint = Rect::new(0, screen.bottom() - px(64) as i32, screen.w, px(24));
                paint::text_centered(ctx, s, Role::Body, hint, "Нажмите любую клавишу", Color::rgb(0xC8, 0xD0, 0xDE));
            }
            Phase::Password => {
                let cx = screen.w as i32 / 2;
                let top = screen.h as i32 / 2 - px(150) as i32;
                let avatar = Rect::new(cx - px(48) as i32, top, px(96), px(96));
                draw::rounded(s, avatar, px(48), p.acc, 255);
                let initial: String = self.user.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
                big_text_centered(ctx, s, avatar, &initial, scale);
                let name = Rect::new(0, avatar.bottom() + px(16) as i32, screen.w, px(32));
                paint::text_centered(ctx, s, Role::Heading, name, &self.user, WHITE);
                let field = Rect::new(cx - px(150) as i32, name.bottom() + px(20) as i32, px(300), px(40));
                draw::rounded(s, field, px(theme::R_ROW), Color::rgb(0x11, 0x15, 0x1D), 230);
                draw::rounded_stroke(s, field, px(theme::R_ROW), if self.wrong { p.bad } else { p.acc }, 255);
                // Пароль — точками (кружками: знака «•» в шрифте нет).
                let fctx = ctx.on(Color::rgb(0x11, 0x15, 0x1D));
                if self.typed.is_empty() {
                    paint::text_clipped(fctx, s, Role::Body, field.x + px(14) as i32, paint::baseline(fctx, Role::Body, field), field.w - px(28), "Пароль", p.ink4);
                } else {
                    let count = self.typed.chars().count().min(24) as i32;
                    for index in 0..count {
                        draw::circle(s, field.x + px(20) as i32 + index * px(12) as i32, field.y + field.h as i32 / 2, px(3), WHITE, 255);
                    }
                }
                let note = Rect::new(0, field.bottom() + px(12) as i32, screen.w, px(20));
                let (text, color) = if self.wrong {
                    ("Неверный пароль. Попробуйте ещё раз.", p.bad_ink)
                } else {
                    ("Enter — войти    Esc — назад", Color::rgb(0xC8, 0xD0, 0xDE))
                };
                paint::text_centered(ctx, s, Role::Caption, note, text, color);
            }
        }

        // Кнопка выключения справа внизу, как в макете.
        let side = px(44);
        self.power = Rect::new(screen.right() - px(24) as i32 - side as i32, screen.bottom() - px(24) as i32 - side as i32, side, side);
        draw::rounded(s, self.power, px(theme::R_ROW), Color::rgb(0x30, 0x3A, 0x55), 200);
        let glyph = px(18);
        glyphicon::draw(s, Icon::Power, self.power.x + (side as i32 - glyph as i32) / 2, self.power.y + (side as i32 - glyph as i32) / 2, glyph, WHITE, 255);
    }
}

impl Default for Lock {
    fn default() -> Self {
        Self::new()
    }
}

/// Текст заголовком, увеличенный в [`CLOCK_ZOOM`] раз со сглаживанием:
/// рисуется белым по чёрному в маску, яркость маски — прозрачность.
fn big_text(ctx: Ctx, s: &mut Surface, x: i32, y: i32, text: &str, scale: u32) {
    let face = ctx.face(Role::Heading);
    let (w, h) = (face.width(text).max(1) + 2, u32::from(face.line).max(1));
    let Some(mut mask) = Surface::new(w, h, Color::rgb(0, 0, 0)) else {
        return;
    };
    paint::text(ctx.on(Color::rgb(0, 0, 0)), &mut mask, Role::Heading, 1, 0, text, WHITE);
    let alpha: Vec<u8> = (0..h).flat_map(|row| mask.row(row).iter().map(|pixel| Color::from_pixel(*pixel).g).collect::<Vec<_>>()).collect();
    let _ = scale;
    let zoom = CLOCK_ZOOM;
    let at = |cx: u32, cy: u32| -> u32 { u32::from(alpha[(cy.min(h - 1) * w + cx.min(w - 1)) as usize]) };
    for ty in 0..h * zoom {
        for tx in 0..w * zoom {
            // Билинейно в неподвижной точке 8.8.
            let fx = (tx * 256 + 128) / zoom;
            let fy = (ty * 256 + 128) / zoom;
            let (x0, y0) = (fx.saturating_sub(128) / 256, fy.saturating_sub(128) / 256);
            let (wx, wy) = (fx.saturating_sub(128) % 256, fy.saturating_sub(128) % 256);
            let top = at(x0, y0) * (256 - wx) + at(x0 + 1, y0) * wx;
            let bottom = at(x0, y0 + 1) * (256 - wx) + at(x0 + 1, y0 + 1) * wx;
            let a = ((top * (256 - wy) + bottom * wy) >> 16) as u8;
            if a > 0 {
                draw::blend_pixel(s, x + tx as i32, y + ty as i32, WHITE, a);
            }
        }
    }
}

/// То же по центру прямоугольника.
fn big_text_centered(ctx: Ctx, s: &mut Surface, area: Rect, text: &str, scale: u32) {
    let face = ctx.face(Role::Heading);
    let (w, h) = (face.width(text) * CLOCK_ZOOM, u32::from(face.line) * CLOCK_ZOOM);
    big_text(ctx, s, area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, text, scale);
}
