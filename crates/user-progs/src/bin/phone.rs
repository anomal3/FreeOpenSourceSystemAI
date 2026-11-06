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
use mini_ui::glyphicon::{self, Icon};
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
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        if self.in_call() {
            self.draw_call(s, area, ctx, &plan);
        } else {
            self.draw_dialpad(s, ctx, &plan);
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
        if self.in_call() {
            // Экран вызова: у входящего — «принять» слева и «отклонить»
            // справа, у набора и разговора — одна «положить» посередине.
            let (answer, decline, hangup) = call_buttons(&plan);
            if self.call_state == user_abi::CALL_INCOMING {
                if answer.contains(x, y) {
                    user_progs::modem_answer_or_hangup(true);
                    return true;
                }
                if decline.contains(x, y) {
                    self.hang_up();
                    return true;
                }
            } else if hangup.contains(x, y) {
                self.hang_up();
                return true;
            }
            return false;
        }
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

const GREEN: Color = Color::rgb(0x2E, 0xB8, 0x5C);
const RED: Color = Color::rgb(0xE5, 0x3E, 0x3E);

/// Кнопки экрана вызова: «принять» и «отклонить» по краям нижнего ряда
/// (под колонками 1 и 3), «положить» — посередине.
fn call_buttons(plan: &Plan) -> (Rect, Rect, Rect) {
    let y = plan.call.y;
    let side = plan.call.w;
    let left = Rect::new(plan.keys[0][0].x, y, side, side);
    let right = Rect::new(plan.keys[0][2].x, y, side, side);
    (left, right, plan.call)
}

/// Контур трубки — точки значка `glyphicon::Icon::Phone` (сетка 256×256).
const HANDSET: [(i32, i32); 14] = [
    (51, 42), (93, 42), (110, 90), (85, 109), (110, 146), (147, 171), (166, 146),
    (214, 163), (214, 205), (197, 221), (139, 204), (88, 168), (52, 117), (35, 59),
];

/// Круглая кнопка с трубкой. `down` — «положить»: трубка повёрнута на 135°,
/// как на кнопке отбоя любого телефона (контур значка `Phone`, повёрнутый
/// здесь — у набора значков поворота нет).
fn round_button(ctx: Ctx, s: &mut Surface, rect: Rect, fill: Color, down: bool) {
    let (cx, cy, r) = (rect.x + rect.w as i32 / 2, rect.y + rect.h as i32 / 2, rect.w / 2);
    draw::circle(s, cx, cy, r, fill, 255);
    let size = (rect.w * 9) / 20;
    let white = Color::rgb(0xFF, 0xFF, 0xFF);
    if !down {
        glyphicon::draw(s, Icon::Phone, cx - size as i32 / 2, cy - size as i32 / 2, size, white, 255);
        return;
    }
    // Поворот на 135° вокруг центра сетки: cos = −sin = −0,7071 (в 1/1024).
    const C: i32 = -724;
    const S: i32 = 724;
    let k = size as i32;
    let point = |(x, y): (i32, i32)| {
        let (dx, dy) = (x - 128, y - 128);
        let (rx, ry) = ((dx * C - dy * S) / 1024, (dx * S + dy * C) / 1024);
        (cx + rx * k / 256, cy + ry * k / 256)
    };
    // Толстая обводка — кружками вдоль отрезков: перо `draw::line` у этого
    // размера выходит в точку-две, и трубка терялась на красном.
    let radius = (size / 18).max(ctx.px(2));
    for i in 0..HANDSET.len() {
        let (ax, ay) = point(HANDSET[i]);
        let (bx, by) = point(HANDSET[(i + 1) % HANDSET.len()]);
        let steps = (bx - ax).abs().max((by - ay).abs()).max(1);
        for t in 0..=steps {
            let x = ax + (bx - ax) * t / steps;
            let y = ay + (by - ay) * t / steps;
            draw::circle(s, x, y, radius, white, 255);
        }
    }
}

impl Phone {
    /// Набор номера: номер, клавиатура 3×4, зелёная «вызов», «стереть».
    fn draw_dialpad(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let shown = self.number.as_str();
        let role = if ctx.face(Role::Heading).width(shown) + ctx.px(8) < plan.number.w { Role::Heading } else { Role::Title };
        let base = plan.number.y + (plan.number.h as i32 - i32::from(ctx.face(role).line)) / 2;
        if shown.is_empty() {
            paint::text_centered(ctx, s, Role::Body, plan.number, "Введите номер", p.ink4);
        } else {
            let w = ctx.face(role).width(shown);
            paint::text(ctx, s, role, plan.number.x + (plan.number.w as i32 - w as i32) / 2, base, shown, p.ink);
        }
        if !self.note.is_empty() {
            paint::text_centered(ctx, s, Role::Caption, plan.note, self.note, p.ink3);
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
        round_button(ctx, s, plan.call, GREEN, false);
        if !self.number.is_empty() {
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

    /// Экран вызова (06.11, Роман: «без кнопок, в середине номер, снизу две
    /// кнопки — слева приём, справа отмена»): клавиатуры нет; номер крупно
    /// посередине, под ним состояние; внизу кнопки.
    fn draw_call(&self, s: &mut Surface, area: Rect, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let pad = ctx.px(24) as i32;
        let mid = area.y + area.h as i32 * 2 / 5;
        let line = Rect::new(area.x + pad, mid - ctx.px(40) as i32, area.w.saturating_sub(pad as u32 * 2), ctx.px(56));
        let number = if self.peer.is_empty() { "Номер скрыт" } else { self.peer.as_str() };
        let role = if ctx.face(Role::Heading).width(number) + ctx.px(8) < line.w { Role::Heading } else { Role::Title };
        paint::text_centered(ctx, s, role, line, number, p.ink);
        let status = Rect::new(line.x, line.bottom() + ctx.px(8) as i32, line.w, ctx.px(28));
        paint::text_centered(ctx, s, Role::Body, status, &self.status_text(), p.ink3);
        let (answer, decline, hangup) = call_buttons(plan);
        if self.call_state == user_abi::CALL_INCOMING {
            round_button(ctx, s, answer, GREEN, false);
            round_button(ctx, s, decline, RED, true);
            let cap = |r: Rect| Rect::new(r.x - ctx.px(10) as i32, r.bottom() + ctx.px(6) as i32, r.w + ctx.px(20), ctx.px(24));
            paint::text_centered(ctx, s, Role::Caption, cap(answer), "Принять", p.ink3);
            paint::text_centered(ctx, s, Role::Caption, cap(decline), "Отклонить", p.ink3);
        } else {
            round_button(ctx, s, hangup, RED, true);
        }
    }
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
