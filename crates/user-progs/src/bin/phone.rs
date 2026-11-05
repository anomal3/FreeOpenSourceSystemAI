//! «Телефон»: набор номера (03.11).
//!
//! Роман на аппарате: «нету телефона, чтобы номер набрать». Окно — как
//! номеронабиратель любого смартфона: номер крупно сверху, под ним
//! клавиатура 3×4 с буквами под цифрами, кнопка стирания и зелёная кнопка
//! вызова. С клавиатуры ПК набираются цифры, `*`, `#`, `+`, Backspace стирает,
//! Enter — вызов.
//!
//! # Что делает вызов
//!
//! С 06.11 — звонит: `SYS_MODEM` → ядро (`mtk_ccci`) шлёт модему
//! `ATD<номер>;`, `ATA`, `ATH` и ведёт состояние по его сообщениям (`RING`,
//! `+CLIP`, `+CLCC`, `NO CARRIER`). Окно раз в полсекунды спрашивает
//! состояние: набор, «звонит», разговор с таймером, входящий — с кнопками
//! «принять» (зелёная) и «положить» (красная).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;

use mini_ui::draw;
use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{SysInfo, println};

const SPEC: Spec = Spec { name: "phone", title: "Телефон", period_ms: 500 };

const ENTER: u32 = '\n' as u32;
const BACKSPACE: u32 = 0x08;
const ESCAPE: u32 = 0x1B;

/// Номер длиннее не бывает (E.164 — 15 цифр, с запасом на `*#` и паузы).
const MAX_LEN: usize = 24;

/// Клавиши: цифра и буквы под ней.
const KEYS: [[(char, &str); 3]; 4] = [
    [('1', ""), ('2', "ABC"), ('3', "DEF")],
    [('4', "GHI"), ('5', "JKL"), ('6', "MNO")],
    [('7', "PQRS"), ('8', "TUV"), ('9', "WXYZ")],
    [('*', ""), ('0', "+"), ('#', "")],
];

struct Phone {
    number: String,
    /// Состояние вызова (`user_abi::CALL_*`) и номер собеседника.
    call_state: u8,
    peer: String,
    /// Когда начался разговор (мс монотонных часов).
    talk_since: u64,
    /// Что сказать под номером (после попытки вызова).
    note: &'static str,
    /// Нажатая клавиша — подсветка до следующего такта.
    pressed: Option<(usize, usize)>,
}

/// Где что лежит в окне.
struct Plan {
    number: Rect,
    note: Rect,
    keys: [[Rect; 3]; 4],
    call: Rect,
    erase: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let pad = ctx.px(24) as i32;
        let width = area.w.saturating_sub(pad as u32 * 2);
        let number = Rect::new(area.x + pad, area.y + ctx.px(28) as i32, width, ctx.px(56));
        let note = Rect::new(area.x + pad, number.bottom() + ctx.px(4) as i32, width, ctx.px(22));
        // Кнопки круглые, как на смартфоне: сторона — по ширине трёх колонок
        // и по высоте пяти рядов (четыре ряда цифр и ряд вызова).
        let gap = ctx.px(18);
        let top = note.bottom() + ctx.px(18) as i32;
        let room_h = (area.bottom() - ctx.px(18) as i32 - top).max(0) as u32;
        let side = ((width.saturating_sub(gap * 2)) / 3).min(room_h.saturating_sub(gap * 4) / 5).min(ctx.px(84));
        let grid_w = side * 3 + gap * 2;
        let left = area.x + (area.w as i32 - grid_w as i32) / 2;
        let mut keys = [[Rect::EMPTY; 3]; 4];
        for (row, line) in keys.iter_mut().enumerate() {
            for (col, key) in line.iter_mut().enumerate() {
                *key = Rect::new(left + (col as u32 * (side + gap)) as i32, top + (row as u32 * (side + gap)) as i32, side, side);
            }
        }
        let call_y = top + (4 * (side + gap)) as i32;
        let call = Rect::new(left + (side + gap) as i32, call_y, side, side);
        let erase = Rect::new(left + (2 * (side + gap)) as i32, call_y, side, side);
        Self { number, note, keys, call, erase }
    }
}

impl Phone {
    fn type_char(&mut self, ch: char) {
        if self.number.chars().count() < MAX_LEN {
            self.number.push(ch);
            self.note = "";
        }
    }

    fn erase(&mut self) {
        self.number.pop();
        self.note = "";
    }

    fn call(&mut self) {
        match self.call_state {
            user_abi::CALL_INCOMING => {
                user_progs::modem_answer_or_hangup(true);
                return;
            }
            user_abi::CALL_IDLE => {}
            user_abi::CALL_NO_MODEM => {
                self.note = "Модем ещё не готов";
                return;
            }
            _ => {
                self.hang_up();
                return;
            }
        }
        if self.number.is_empty() {
            self.note = "Наберите номер";
            return;
        }
        let r = user_progs::modem_dial(&self.number);
        println(&alloc::format!("phone: dial {} -> {r}", self.number));
        if r == 0 {
            self.call_state = user_abi::CALL_DIALING;
            self.peer = self.number.clone();
        } else {
            self.note = "Не удалось набрать номер";
        }
    }

    fn hang_up(&mut self) {
        user_progs::modem_answer_or_hangup(false);
        self.call_state = user_abi::CALL_IDLE;
    }

    fn in_call(&self) -> bool {
        !matches!(self.call_state, user_abi::CALL_IDLE | user_abi::CALL_NO_MODEM)
    }

    /// Строка под номером: что сейчас с вызовом.
    fn status_text(&self) -> String {
        match self.call_state {
            user_abi::CALL_DIALING => String::from("Вызов…"),
            user_abi::CALL_ALERTING => String::from("Звонит…"),
            user_abi::CALL_ACTIVE => {
                let s = user_progs::monotonic_ms().saturating_sub(self.talk_since) / 1000;
                alloc::format!("Разговор {:02}:{:02}", s / 60, s % 60)
            }
            user_abi::CALL_INCOMING => String::from("Входящий вызов"),
            _ => String::from(self.note),
        }
    }
}

impl App for Phone {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);

        // Номер — крупно; длинный — мельче, чтобы поместился целиком.
        let shown = if self.in_call() && !self.peer.is_empty() {
            self.peer.as_str()
        } else if self.number.is_empty() {
            ""
        } else {
            self.number.as_str()
        };
        let role = if ctx.face(Role::Heading).width(shown) + ctx.px(8) < plan.number.w { Role::Heading } else { Role::Title };
        let base = plan.number.y + (plan.number.h as i32 - i32::from(ctx.face(role).line)) / 2;
        if shown.is_empty() {
            let hint = if self.call_state == user_abi::CALL_INCOMING { "Номер скрыт" } else { "Введите номер" };
            paint::text_centered(ctx, s, Role::Body, plan.number, hint, p.ink4);
        } else {
            let w = ctx.face(role).width(shown);
            paint::text(ctx, s, role, plan.number.x + (plan.number.w as i32 - w as i32) / 2, base, shown, p.ink);
        }
        let status = self.status_text();
        if !status.is_empty() {
            paint::text_centered(ctx, s, Role::Caption, plan.note, &status, p.ink3);
        }

        for (row, line) in KEYS.iter().enumerate() {
            for (col, (digit, letters)) in line.iter().enumerate() {
                let rect = plan.keys[row][col];
                let fill = if self.pressed == Some((row, col)) { p.btnhov } else { p.btn };
                draw::circle(s, rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2, rect.w / 2, ctx.flat(fill), 255);
                let mut digit_buf = [0u8; 4];
                let digit_text = digit.encode_utf8(&mut digit_buf);
                let title = ctx.face(Role::Heading);
                let small = ctx.face(Role::Caption);
                let block = i32::from(title.line) + if letters.is_empty() { 0 } else { i32::from(small.line) };
                let top = rect.y + (rect.h as i32 - block) / 2;
                let w = title.width(digit_text);
                paint::text(ctx, s, Role::Heading, rect.x + (rect.w as i32 - w as i32) / 2, top, digit_text, p.ink);
                if !letters.is_empty() {
                    let lw = small.width(letters);
                    paint::text(ctx, s, Role::Caption, rect.x + (rect.w as i32 - lw as i32) / 2, top + i32::from(title.line), letters, p.ink4);
                }
            }
        }

        // Вызов — зелёный круг с трубкой (рисуется линиями: значка трубки в
        // шрифте нет).
        let call = plan.call;
        let (cx, cy, r) = (call.x + call.w as i32 / 2, call.y + call.h as i32 / 2, call.w / 2);
        let red = Color::rgb(0xE5, 0x3E, 0x3E);
        let green = Color::rgb(0x2E, 0xB8, 0x5C);
        // Идёт вызов — на месте вызова красная «положить»; входящий — зелёная
        // «принять» там же и красная «положить» справа.
        let calling = self.in_call() && self.call_state != user_abi::CALL_INCOMING;
        draw::circle(s, cx, cy, r, if calling { red } else { green }, 255);
        draw_handset(ctx, s, cx, cy, r);
        if self.call_state == user_abi::CALL_INCOMING {
            let e = plan.erase;
            let (ex, ey, er) = (e.x + e.w as i32 / 2, e.y + e.h as i32 / 2, e.w / 2);
            draw::circle(s, ex, ey, er, red, 255);
            draw_handset(ctx, s, ex, ey, er);
        } else if !self.number.is_empty() && !self.in_call() {
            // «⌫» линиями: стрелка-ярлык влево и крестик в нём.
            let e = plan.erase;
            let (ex, ey) = (e.x + e.w as i32 / 2, e.y + e.h as i32 / 2);
            let h = ctx.px(11) as i32;
            let w = ctx.px(2);
            let ink = p.ink2;
            let left = (ex - h - h / 2, ey);
            let top_l = (ex - h / 2, ey - h);
            let bottom_l = (ex - h / 2, ey + h);
            let top_r = (ex + h + h / 2, ey - h);
            let bottom_r = (ex + h + h / 2, ey + h);
            for (a, b) in [(left, top_l), (left, bottom_l), (top_l, top_r), (bottom_l, bottom_r), (top_r, bottom_r)] {
                draw::line(s, a, b, w, ink, 255);
            }
            let k = h / 2;
            draw::line(s, (ex - k / 2, ey - k), (ex + k + k / 2, ey + k), w, ink, 255);
            draw::line(s, (ex - k / 2, ey + k), (ex + k + k / 2, ey - k), w, ink, 255);
        }
    }

    fn key(&mut self, code: u32) -> bool {
        match code {
            ENTER => self.call(),
            ESCAPE if self.in_call() => self.hang_up(),
            BACKSPACE => self.erase(),
            ESCAPE => {
                self.number.clear();
                self.note = "";
            }
            code => match char::from_u32(code) {
                Some(ch @ ('0'..='9' | '*' | '#' | '+')) => self.type_char(ch),
                _ => return false,
            },
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        for (row, line) in KEYS.iter().enumerate() {
            for (col, (digit, _)) in line.iter().enumerate() {
                if plan.keys[row][col].contains(x, y) {
                    self.pressed = Some((row, col));
                    self.type_char(*digit);
                    return true;
                }
            }
        }
        if plan.call.contains(x, y) {
            self.call();
            return true;
        }
        if plan.erase.contains(x, y) && self.call_state == user_abi::CALL_INCOMING {
            self.hang_up();
            return true;
        }
        if plan.erase.contains(x, y) && !self.number.is_empty() {
            self.erase();
            return true;
        }
        false
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        let before = (self.call_state, self.peer.clone());
        let (state, peer) = user_progs::modem_status();
        if state == user_abi::CALL_ACTIVE && self.call_state != user_abi::CALL_ACTIVE {
            self.talk_since = user_progs::monotonic_ms();
        }
        self.call_state = state;
        if !peer.is_empty() {
            self.peer = peer;
        }
        if !self.in_call() {
            self.peer.clear();
        }
        // Таймер разговора — перерисовка каждый такт.
        let changed = before != (self.call_state, self.peer.clone()) || self.call_state == user_abi::CALL_ACTIVE;
        self.pressed.take().is_some() || changed
    }

    fn quits_on_q(&self) -> bool {
        false
    }
}

/// Трубка линиями: дуга корпуса и два «уха».
fn draw_handset(ctx: Ctx, s: &mut Surface, cx: i32, cy: i32, r: u32) {
    let white = Color::rgb(0xFF, 0xFF, 0xFF);
    let k = (r as i32 * 2) / 5;
    draw::line(s, (cx - k, cy - k), (cx + k, cy + k), ctx.px(5), white, 255);
    draw::circle(s, cx - k, cy - k, ctx.px(6), white, 255);
    draw::circle(s, cx + k, cy + k, ctx.px(6), white, 255);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let scale = theme::geometry_scale_for(user_abi::sysinfo_scale_choice(info.flags), info.screen_w.max(1), info.screen_h.max(1));
    let width = (380 * scale).min(info.screen_w);
    let height = (640 * scale).min(info.screen_h.saturating_sub(80 * scale)).max(420);
    app::run(&SPEC, &info, (width, height), |_, _| Phone {
        number: String::new(),
        call_state: user_abi::CALL_NO_MODEM,
        peer: String::new(),
        talk_since: 0,
        note: "",
        pressed: None,
    })
}
