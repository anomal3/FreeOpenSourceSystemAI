//! Калькулятор: свой, с окном (фаза 70b).
//!
//! Как «Обычный» калькулятор Windows: табло с выражением сверху и числом под
//! ним, сетка кнопок — проценты, CE, C, ←, 1/x, x², √x, деление, цифры,
//! умножение, вычитание, сложение, ±, запятая и «=». Цифры и знаки набираются
//! и с клавиатуры: `0–9`, `+ - * /`, `,` или `.`, Enter или `=`, Backspace,
//! Esc — сброс, `%`.
//!
//! # Как считает
//!
//! В `f64` — как и сам калькулятор Windows в обычном режиме считает в двоичной
//! плавающей точке с округлением для показа. Квадратный корень — свой, Ньютоном:
//! в программе без стандартной библиотеки `sqrt` взять неоткуда. Показ — не
//! больше шестнадцати значащих цифр, хвост `0,1 + 0,2 = 0,30000000000000004`
//! срезается округлением, как у настоящего калькулятора.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};

use mini_ui::draw;
use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{SysInfo, println};

const SPEC: Spec = Spec { name: "calc", title: "Calculator", period_ms: 1_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;

/// Сколько знаков в числе можно набрать.
const MAX_DIGITS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

impl Op {
    fn sign(self) -> &'static str {
        match self {
            Op::Add => "+",
            // В строке выражения — знаки из шрифта: «−» и «÷» в нём нет.
            Op::Sub => "-",
            Op::Mul => "×",
            Op::Div => "/",
        }
    }

    fn apply(self, a: f64, b: f64) -> Option<f64> {
        match self {
            Op::Add => Some(a + b),
            Op::Sub => Some(a - b),
            Op::Mul => Some(a * b),
            Op::Div if b == 0.0 => None,
            Op::Div => Some(a / b),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Digit(u8),
    Point,
    Op(Op),
    Equals,
    Percent,
    ClearEntry,
    Clear,
    Back,
    Inverse,
    Square,
    Root,
    Negate,
}

/// Сетка кнопок — шесть рядов по четыре, сверху вниз.
const GRID: [[(Key, &str); 4]; 6] = [
    [(Key::Percent, "%"), (Key::ClearEntry, "CE"), (Key::Clear, "C"), (Key::Back, "⌫")],
    [(Key::Inverse, "1/x"), (Key::Square, "x²"), (Key::Root, "√x"), (Key::Op(Op::Div), "÷")],
    [(Key::Digit(7), "7"), (Key::Digit(8), "8"), (Key::Digit(9), "9"), (Key::Op(Op::Mul), "×")],
    [(Key::Digit(4), "4"), (Key::Digit(5), "5"), (Key::Digit(6), "6"), (Key::Op(Op::Sub), "−")],
    [(Key::Digit(1), "1"), (Key::Digit(2), "2"), (Key::Digit(3), "3"), (Key::Op(Op::Add), "+")],
    [(Key::Negate, "±"), (Key::Digit(0), "0"), (Key::Point, ","), (Key::Equals, "=")],
];

struct Calc {
    /// Что набрано или показано сейчас — текстом, как на табло.
    entry: String,
    /// Левый операнд и знак, ждущие правого.
    pending: Option<(f64, Op)>,
    /// Выражение над числом: «12 + », «12 + 7 =».
    expression: String,
    /// Следующая цифра начинает новое число, а не дописывается.
    fresh: bool,
    /// Повтор «=»: последний знак и правый операнд.
    last: Option<(Op, f64)>,
    error: bool,
    /// Кнопка, подсвеченная последним нажатием (с клавиатуры тоже).
    pressed: Option<(usize, usize)>,
}

impl Calc {
    fn new() -> Self {
        Self { entry: String::from("0"), pending: None, expression: String::new(), fresh: true, last: None, error: false, pressed: None }
    }

    fn value(&self) -> f64 {
        parse(&self.entry)
    }

    fn show(&mut self, value: f64) {
        if value.is_finite() {
            self.entry = format_number(value);
        } else {
            self.fail("Переполнение");
        }
    }

    fn fail(&mut self, text: &str) {
        self.entry = text.to_string();
        self.error = true;
        self.pending = None;
        self.last = None;
        self.fresh = true;
    }

    fn press(&mut self, key: Key) {
        if self.error && !matches!(key, Key::Clear | Key::ClearEntry | Key::Digit(_)) {
            return;
        }
        if self.error {
            *self = Self::new();
        }
        match key {
            Key::Digit(digit) => {
                if self.fresh {
                    self.entry = String::from("0");
                    self.fresh = false;
                    if self.pending.is_none() {
                        self.expression.clear();
                    }
                }
                let digits = self.entry.chars().filter(char::is_ascii_digit).count();
                if digits >= MAX_DIGITS {
                    return;
                }
                if self.entry == "0" {
                    self.entry.clear();
                } else if self.entry == "-0" {
                    self.entry = String::from("-");
                }
                self.entry.push(char::from(b'0' + digit));
            }
            Key::Point => {
                if self.fresh {
                    self.entry = String::from("0");
                    self.fresh = false;
                }
                if !self.entry.contains(',') {
                    self.entry.push(',');
                }
            }
            Key::Op(op) => {
                let value = self.value();
                let left = match self.pending {
                    // Знак сразу после знака — замена знака, а не вычисление.
                    Some((left, _)) if self.fresh => left,
                    Some((left, previous)) => match previous.apply(left, value) {
                        Some(result) => result,
                        None => return self.fail("Деление на ноль невозможно"),
                    },
                    None => value,
                };
                self.show(left);
                self.pending = Some((left, op));
                self.expression = format!("{} {} ", format_number(left), op.sign());
                self.fresh = true;
                self.last = None;
            }
            Key::Equals => {
                let (left, op, right) = match (self.pending, self.last) {
                    (Some((left, op)), _) => (left, op, self.value()),
                    // Повтор «=» повторяет последнее действие: 2 + 3 = = = → 11.
                    (None, Some((op, right))) => (self.value(), op, right),
                    (None, None) => {
                        self.expression = format!("{} =", self.entry);
                        self.fresh = true;
                        return;
                    }
                };
                match op.apply(left, right) {
                    Some(result) => {
                        self.expression = format!("{} {} {} =", format_number(left), op.sign(), format_number(right));
                        println(&format!("calc: {}", self.expression.clone() + " " + &format_number(result)));
                        self.show(result);
                        self.last = Some((op, right));
                        self.pending = None;
                        self.fresh = true;
                    }
                    None => self.fail("Деление на ноль невозможно"),
                }
            }
            Key::Percent => {
                // Как в Windows: при сложении и вычитании — процент от левого
                // операнда (200 + 10% = 220), при умножении и делении — доля.
                let value = self.value();
                let result = match self.pending {
                    Some((left, Op::Add | Op::Sub)) => left * value / 100.0,
                    _ => value / 100.0,
                };
                self.show(result);
                self.fresh = true;
            }
            Key::ClearEntry => {
                self.entry = String::from("0");
                self.fresh = true;
            }
            Key::Clear => *self = Self::new(),
            Key::Back => {
                if !self.fresh {
                    self.entry.pop();
                    if self.entry.is_empty() || self.entry == "-" {
                        self.entry = String::from("0");
                    }
                }
            }
            Key::Inverse => {
                let value = self.value();
                if value == 0.0 {
                    return self.fail("Деление на ноль невозможно");
                }
                self.expression = format!("1/({})", self.entry);
                self.show(1.0 / value);
                self.fresh = true;
            }
            Key::Square => {
                let value = self.value();
                self.expression = format!("sqr({})", self.entry);
                self.show(value * value);
                self.fresh = true;
            }
            Key::Root => {
                let value = self.value();
                if value < 0.0 {
                    return self.fail("Недопустимый ввод");
                }
                self.expression = format!("sqrt({})", self.entry);
                self.show(sqrt(value));
                self.fresh = true;
            }
            Key::Negate => {
                if self.entry != "0" {
                    if let Some(rest) = self.entry.strip_prefix('-') {
                        self.entry = rest.to_string();
                    } else {
                        self.entry.insert(0, '-');
                    }
                }
            }
        }
    }

    fn locate(key: Key) -> Option<(usize, usize)> {
        for (row, keys) in GRID.iter().enumerate() {
            for (col, (other, _)) in keys.iter().enumerate() {
                if *other == key {
                    return Some((row, col));
                }
            }
        }
        None
    }
}

struct Plan {
    expression: Rect,
    entry: Rect,
    cells: [[Rect; 4]; 6],
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let pad = ctx.px(8) as i32;
        let gap = ctx.px(4);
        let expression = Rect::new(area.x + pad, area.y + ctx.px(14) as i32, area.w.saturating_sub(ctx.px(16)), ctx.px(22));
        let entry = Rect::new(area.x + pad, expression.bottom() + ctx.px(4) as i32, area.w.saturating_sub(ctx.px(16)), ctx.px(64));
        let top = entry.bottom() + ctx.px(12) as i32;
        let grid_w = area.w.saturating_sub(ctx.px(16));
        let grid_h = (area.bottom() - pad - top).max(0) as u32;
        let cell_w = grid_w.saturating_sub(gap * 3) / 4;
        let cell_h = grid_h.saturating_sub(gap * 5) / 6;
        let mut cells = [[Rect::new(0, 0, 0, 0); 4]; 6];
        for (row, line) in cells.iter_mut().enumerate() {
            for (col, cell) in line.iter_mut().enumerate() {
                *cell = Rect::new(
                    area.x + pad + (col as u32 * (cell_w + gap)) as i32,
                    top + (row as u32 * (cell_h + gap)) as i32,
                    cell_w,
                    cell_h,
                );
            }
        }
        Self { expression, entry, cells }
    }
}

impl App for Calc {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        paint::text_right(ctx, s, Role::Body, plan.expression.right() - ctx.px(6) as i32, plan.expression.y, &self.expression, p.ink4);
        // Число табло — крупно и по правому краю; длинное — мельче, чтобы
        // поместилось целиком, а не обрезалось с начала.
        let role = if ctx.face(Role::Heading).width(&self.entry) + ctx.px(12) < plan.entry.w { Role::Heading } else { Role::Title };
        let base = plan.entry.y + (plan.entry.h as i32 - i32::from(ctx.face(role).line)) / 2;
        paint::text_right(ctx, s, role, plan.entry.right() - ctx.px(6) as i32, base, &self.entry, p.ink);

        for (row, keys) in GRID.iter().enumerate() {
            for (col, (key, label)) in keys.iter().enumerate() {
                let rect = plan.cells[row][col];
                let digit = matches!(key, Key::Digit(_) | Key::Point | Key::Negate);
                let pressed = self.pressed == Some((row, col));
                let r = ctx.px(6);
                if *key == Key::Equals {
                    let (top, bottom) = if pressed { (p.acc2, p.acc) } else { (p.acc, p.acc2) };
                    draw::rounded_gradient(s, rect, r, top, bottom, 255);
                    paint::text_centered(ctx, s, Role::Title, rect, label, mini_ui::Color::rgb(0xFF, 0xFF, 0xFF));
                    continue;
                }
                let fill = if pressed { p.btnhov } else if digit { p.btn } else { p.ghost };
                draw::rounded(s, rect, r, ctx.flat(fill), 255);
                draw::rounded_stroke(s, rect, r, p.btnline.color, p.btnline.alpha);
                let role = if digit { Role::Title } else { Role::Body };
                let ink = if digit { p.ink } else { p.ink2 };
                if !draw_sign(ctx, s, rect, *key, ink) {
                    paint::text_centered(ctx, s, role, rect, label, ink);
                }
            }
        }
    }

    fn key(&mut self, code: u32) -> bool {
        let key = match code {
            ENTER => Key::Equals,
            ESCAPE => Key::Clear,
            BACKSPACE => Key::Back,
            user_progs::WIN_KEY_DELETE => Key::ClearEntry,
            code => match char::from_u32(code) {
                Some(ch @ '0'..='9') => Key::Digit(ch as u8 - b'0'),
                Some(',' | '.' | 'б' | 'ю') => Key::Point,
                Some('+') => Key::Op(Op::Add),
                Some('-') => Key::Op(Op::Sub),
                Some('*' | 'x' | 'ч') => Key::Op(Op::Mul),
                Some('/') => Key::Op(Op::Div),
                Some('=') => Key::Equals,
                Some('%') => Key::Percent,
                Some('r' | 'к') => Key::Inverse,
                Some('@') => Key::Root,
                Some('q' | 'й') => Key::Square,
                _ => return false,
            },
        };
        self.pressed = Calc::locate(key);
        self.press(key);
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        for (row, keys) in GRID.iter().enumerate() {
            for (col, (key, _)) in keys.iter().enumerate() {
                if plan.cells[row][col].contains(x, y) {
                    self.pressed = Some((row, col));
                    self.press(*key);
                    return true;
                }
            }
        }
        false
    }

    /// Подсветка нажатой кнопки гаснет на следующем такте.
    fn tick(&mut self, _info: &SysInfo) -> bool {
        self.pressed.take().is_some()
    }

    /// `q` — «x²» с клавиатуры, а не выход.
    fn quits_on_q(&self) -> bool {
        false
    }
}

/// Знаки, которых нет в шрифте, — линиями: ÷, −, ±, √, x², ⌫. `false` —
/// подпись рисуется текстом как есть.
fn draw_sign(ctx: Ctx, s: &mut Surface, rect: Rect, key: Key, ink: mini_ui::Color) -> bool {
    let cx = rect.x + rect.w as i32 / 2;
    let cy = rect.y + rect.h as i32 / 2;
    let u = ctx.px(6) as i32;
    let w = ctx.px(2);
    let line = |s: &mut Surface, a: (i32, i32), b: (i32, i32)| draw::line(s, a, b, w, ink, 255);
    match key {
        Key::Op(Op::Sub) => line(s, (cx - u, cy), (cx + u, cy)),
        Key::Op(Op::Div) => {
            line(s, (cx - u, cy), (cx + u, cy));
            draw::circle(s, cx, cy - u, ctx.px(2), ink, 255);
            draw::circle(s, cx, cy + u, ctx.px(2), ink, 255);
        }
        Key::Negate => {
            line(s, (cx - u, cy - u / 2), (cx + u, cy - u / 2));
            line(s, (cx, cy - u - u / 2), (cx, cy + u / 2));
            line(s, (cx - u, cy + u), (cx + u, cy + u));
        }
        Key::Root => {
            // Галочка корня и черта над «x».
            line(s, (cx - u * 2, cy), (cx - u - u / 2, cy + u));
            line(s, (cx - u - u / 2, cy + u), (cx - u / 2, cy - u - u / 3));
            line(s, (cx - u / 2, cy - u - u / 3), (cx + u * 2, cy - u - u / 3));
            paint::text_centered(ctx, s, Role::Body, Rect::new(cx - u / 2, cy - u, (u * 2 + u / 2) as u32, (u * 2) as u32), "x", ink);
        }
        Key::Square => {
            let x_rect = Rect::new(cx - u * 2, rect.y, (u * 2) as u32, rect.h);
            paint::text_centered(ctx, s, Role::Body, x_rect, "x", ink);
            let two = Rect::new(cx, cy - u * 2, (u * 2) as u32, (u * 2) as u32);
            paint::text_centered(ctx, s, Role::Caption, two, "2", ink);
        }
        Key::Back => {
            // Клавиша «стереть»: пятиугольник со стрелкой влево и крестиком.
            let (l, r, t, b) = (cx - u * 2, cx + u * 2, cy - u, cy + u);
            line(s, (l, cy), (l + u, t));
            line(s, (l + u, t), (r, t));
            line(s, (r, t), (r, b));
            line(s, (r, b), (l + u, b));
            line(s, (l + u, b), (l, cy));
            line(s, (cx - u / 2, cy - u / 2), (cx + u / 2 + u / 3, cy + u / 2));
            line(s, (cx - u / 2, cy + u / 2), (cx + u / 2 + u / 3, cy - u / 2));
        }
        _ => return false,
    }
    true
}

// ---------------------------------------------------------------------------
// Числа
// ---------------------------------------------------------------------------

/// Число с табло: запятая — десятичный знак.
fn parse(text: &str) -> f64 {
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (whole, fraction) = body.split_once(',').unwrap_or((body, ""));
    let mut value = 0.0f64;
    for digit in whole.bytes().filter(u8::is_ascii_digit) {
        value = value * 10.0 + f64::from(digit - b'0');
    }
    let mut scale = 0.1f64;
    for digit in fraction.bytes().filter(u8::is_ascii_digit) {
        value += f64::from(digit - b'0') * scale;
        scale /= 10.0;
    }
    if negative { -value } else { value }
}

/// Число для табло: не больше шестнадцати значащих цифр, запятая, без
/// хвостовых нулей; очень большое и очень малое — в виде `1,5e+20`.
fn format_number(value: f64) -> String {
    if value == 0.0 {
        return String::from("0");
    }
    let magnitude = if value < 0.0 { -value } else { value };
    let text = if !(1e-9..1e16).contains(&magnitude) {
        format!("{value:.10e}")
    } else {
        // Шестнадцать значащих: столько знаков после запятой, сколько их
        // остаётся после целой части.
        let mut whole_digits = 1usize;
        let mut probe = magnitude;
        while probe >= 10.0 {
            probe /= 10.0;
            whole_digits += 1;
        }
        let decimals = 15usize.saturating_sub(whole_digits).min(15);
        format!("{value:.decimals$}")
    };
    // Хвостовые нули дробной части — прочь, точку — в запятую.
    let (mantissa, exponent) = match text.split_once('e') {
        Some((m, e)) => (String::from(m), Some(String::from(e))),
        None => (text.clone(), None),
    };
    let mut mantissa = mantissa;
    if mantissa.contains('.') {
        while mantissa.ends_with('0') {
            mantissa.pop();
        }
        if mantissa.ends_with('.') {
            mantissa.pop();
        }
    }
    let mut out = mantissa.replace('.', ",");
    if let Some(exponent) = exponent {
        let exponent = exponent.trim_start_matches('+');
        out.push('e');
        if !exponent.starts_with('-') {
            out.push('+');
        }
        out.push_str(exponent);
    }
    if out == "-0" { String::from("0") } else { out }
}

/// Квадратный корень Ньютоном: в программе без `std` его взять неоткуда.
fn sqrt(value: f64) -> f64 {
    if value == 0.0 {
        return 0.0;
    }
    let mut guess = if value > 1.0 { value / 2.0 } else { 1.0 };
    for _ in 0..100 {
        let next = 0.5 * (guess + value / guess);
        if next == guess {
            break;
        }
        guess = next;
    }
    guess
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    // Калькулятор — узкий и высокий, как в Windows, а не две трети экрана.
    let scale = theme::geometry_scale(info.screen_w.max(1), info.screen_h.max(1));
    let width = (340 * scale).min(info.screen_w);
    let height = (520 * scale).min(info.screen_h.saturating_sub(80 * scale)).max(360);
    app::run(&SPEC, &info, (width, height), |_, _| Calc::new())
}
