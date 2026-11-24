//! «Сетевые учётные данные» (Roman 24.11): open `\\host\share`, asking for
//! a user name and a password when the computer wants them.
//!
//! ```text
//! netlogon --open \\192.168.88.246        Run box: open in «Файлы», ask if needed
//! netlogon --open /net/NAS/Общая          the same, already a /net path
//! netlogon /net/NAS                       ask only (Files' hook): exit 0 when
//!                                         the folder can be listed, 1 on cancel
//! ```
//!
//! The folder is tried first; only `ERR_PERMISSION` brings up the window
//! (like Windows: an open share opens at once). Credentials go to the
//! kernel through `/net/.login` (`host\nuser\npassword\nremember\n`); the
//! write succeeds only if the server accepted them, and `/net/.login` reads
//! back why not. With «Запомнить» the kernel keeps the NT hash in
//! `/etc/netcreds` (root only) — the password itself is stored nowhere.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{Args, Dirent, close, open, open_write, println, read, readdir_raw, spawn, write};

const SPEC: Spec = Spec { name: "netlogon", title: "Network credentials", period_ms: 5_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const TAB: u32 = '\t' as u32;
const SPACE: u32 = ' ' as u32;
const CTRL_V: u32 = 0x16;
const MAX_FIELD: usize = 120;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    User,
    Password,
    Remember,
}

struct Plan {
    icon: Rect,
    user: Rect,
    password: Rect,
    remember: Rect,
    message: Rect,
    ok: Rect,
    cancel: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let pad = ctx.px(22) as i32;
        let icon = Rect::new(area.x + pad, area.y + ctx.px(20) as i32, ctx.px(40), ctx.px(40));
        let fw = area.w.saturating_sub(pad as u32 * 2);
        let fh = ctx.px(34);
        let user = Rect::new(area.x + pad, icon.bottom() + ctx.px(46) as i32, fw, fh);
        let password = Rect::new(user.x, user.bottom() + ctx.px(10) as i32, fw, fh);
        let remember = Rect::new(user.x, password.bottom() + ctx.px(12) as i32, fw, ctx.px(26));
        let message = Rect::new(user.x, remember.bottom() + ctx.px(6) as i32, fw, ctx.px(24));
        let bh = ctx.px(34);
        let bw = ctx.px(116);
        let by = area.bottom() - pad - bh as i32;
        let cancel = Rect::new(area.right() - pad - bw as i32, by, bw, bh);
        let ok = Rect::new(cancel.x - ctx.px(10) as i32 - bw as i32, by, bw, bh);
        Self { icon, user, password, remember, message, ok, cancel }
    }
}

struct Logon {
    /// `/net/host/...` to open.
    path: String,
    /// `host` (with `:port` if any).
    host: String,
    open_after: bool,
    user: String,
    password: String,
    remember: bool,
    focus: Focus,
    message: Option<String>,
    done: bool,
}

impl Logon {
    fn submit(&mut self) {
        if self.user.trim().is_empty() {
            self.message = Some(String::from("Введите имя пользователя."));
            self.focus = Focus::User;
            return;
        }
        let request = format!("{}\n{}\n{}\n{}\n", self.host, self.user.trim(), self.password, if self.remember { 1 } else { 0 });
        let fd = open_write("/net/.login", false, true);
        let wrote = if fd < 0 { fd } else { write(fd, request.as_bytes()) };
        if fd >= 0 {
            close(fd);
        }
        if wrote >= 0 {
            println(&format!("netlogon: credentials for {} accepted{}", self.host, if self.remember { ", remembered" } else { "" }));
            // The folder itself may still be closed to this user.
            match access(&self.path) {
                Ok(()) => self.finish(),
                Err(user_abi::ERR_PERMISSION) => {
                    self.message = Some(format!("Вход выполнен, но у «{}» нет доступа к этой папке.", self.user.trim()));
                }
                Err(code) => self.message = Some(why(code, &self.host)),
            }
            return;
        }
        let reason = read_reason();
        println(&format!("netlogon: credentials for {} refused: {} ({wrote})", self.host, reason.trim()));
        self.password.clear();
        self.focus = Focus::Password;
        self.message = Some(match wrote {
            user_abi::ERR_PERMISSION if reason.contains("LOCKED") => String::from("Учётная запись заблокирована."),
            user_abi::ERR_PERMISSION if reason.contains("DISABLED") => String::from("Учётная запись отключена."),
            user_abi::ERR_PERMISSION if reason.contains("EXPIRED") || reason.contains("MUST_CHANGE") => String::from("Срок действия пароля истёк."),
            user_abi::ERR_PERMISSION => String::from("Неверное имя пользователя или пароль."),
            user_abi::ERR_UNSUPPORTED if reason.contains("encrypt") => String::from("Компьютер требует шифрования SMB3 — пока не поддерживается."),
            user_abi::ERR_UNSUPPORTED => format!("Компьютер отказал: {}", reason.trim()),
            code => why(code, &self.host),
        });
    }

    fn finish(&mut self) {
        if self.open_after {
            let id = spawn(&format!("/bin/files {}", self.path));
            println(&format!("netlogon: opening {} ({id})", self.path));
        }
        self.done = true;
    }

    fn field(&mut self) -> Option<&mut String> {
        match self.focus {
            Focus::User => Some(&mut self.user),
            Focus::Password => Some(&mut self.password),
            Focus::Remember => None,
        }
    }

    fn insert(&mut self, ch: char) {
        if let Some(field) = self.field() {
            if field.chars().count() < MAX_FIELD {
                field.push(ch);
            }
        }
        self.message = None;
    }
}

impl App for Logon {
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & user_abi::WIN_MOD_CTRL != 0;
        let shift = mods & user_abi::WIN_MOD_SHIFT != 0;
        match code {
            ENTER => self.submit(),
            ESCAPE => {
                println("netlogon: cancelled");
                self.done = true;
            }
            TAB => {
                self.focus = match (self.focus, shift) {
                    (Focus::User, false) | (Focus::Remember, true) => Focus::Password,
                    (Focus::Password, false) | (Focus::User, true) => Focus::Remember,
                    (Focus::Remember, false) | (Focus::Password, true) => Focus::User,
                };
            }
            SPACE if self.focus == Focus::Remember => self.remember = !self.remember,
            BACKSPACE => {
                if let Some(field) = self.field() {
                    field.pop();
                }
            }
            CTRL_V => {
                for ch in user_progs::clipboard_get().chars().filter(|ch| !ch.is_control()) {
                    self.insert(ch);
                }
            }
            _ if ctrl && (latin == 'v' as u32 || latin == 'V' as u32) => {
                for ch in user_progs::clipboard_get().chars().filter(|ch| !ch.is_control()) {
                    self.insert(ch);
                }
            }
            code if code < 0x0100_0000 => match char::from_u32(code) {
                Some(ch) if !ch.is_control() => self.insert(ch),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.user.contains(x, y) {
            self.focus = Focus::User;
        } else if plan.password.contains(x, y) {
            self.focus = Focus::Password;
        } else if plan.remember.contains(x, y) {
            self.focus = Focus::Remember;
            self.remember = !self.remember;
        } else if plan.ok.contains(x, y) {
            self.submit();
        } else if plan.cancel.contains(x, y) {
            println("netlogon: cancelled");
            self.done = true;
        }
        true
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn resizable(&self) -> bool {
        false
    }

    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Network credentials\x1fБезопасность FossAi"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);

        draw::rounded(s, plan.icon, ctx.px(10), ctx.flat(p.acctint), 255);
        draw::rounded_stroke(s, plan.icon, ctx.px(10), p.accedge, 255);
        let g = ctx.px(22);
        iconset::draw(s, Fi::Lock, plan.icon.x + (plan.icon.w - g) as i32 / 2, plan.icon.y + (plan.icon.h - g) as i32 / 2, g, p.acc_ink, 255);
        let tx = plan.icon.right() + ctx.px(14) as i32;
        let room = (area.right() - tx - ctx.px(22) as i32).max(0) as u32;
        let line = i32::from(ctx.face(Role::Body).line);
        paint::text_clipped(ctx, s, Role::Title, tx, plan.icon.y, room, "Введите сетевые учётные данные", p.ink);
        let target = unc(&self.path);
        paint::text_clipped(ctx, s, Role::Body, tx, plan.icon.y + line + ctx.px(8) as i32, room, &format!("Для подключения к {target}"), p.ink3);

        let user_hint = "Имя пользователя (или КОМПЬЮТЕР\\имя)";
        input(ctx, s, plan.user, &self.user, user_hint, self.focus == Focus::User);
        let stars: String = "*".repeat(self.password.chars().count());
        input(ctx, s, plan.password, &stars, "Пароль", self.focus == Focus::Password);

        // «Запомнить учётные данные».
        let b = ctx.px(18);
        let bx = Rect::new(plan.remember.x, plan.remember.y + (plan.remember.h - b) as i32 / 2, b, b);
        if self.remember {
            draw::rounded(s, bx, ctx.px(4), p.acc, 255);
            iconset::draw(s, Fi::Check, bx.x + ctx.px(2) as i32, bx.y + ctx.px(2) as i32, b - ctx.px(4), p.acc_ink, 255);
        } else {
            draw::rounded_stroke(s, bx, ctx.px(4), p.ink3, 255);
        }
        if self.focus == Focus::Remember {
            draw::rounded_stroke(s, Rect::new(bx.x - 3, bx.y - 3, bx.w + 6, bx.h + 6), ctx.px(6), p.acc, 255);
        }
        paint::text_clipped(ctx, s, Role::Body, bx.right() + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, plan.remember), plan.remember.w.saturating_sub(b + ctx.px(10)), "Запомнить учётные данные", p.ink);

        if let Some(message) = &self.message {
            let g = ctx.px(16);
            iconset::draw(s, Fi::Warning, plan.message.x, plan.message.y + (plan.message.h - g) as i32 / 2, g, p.bad_ink, 255);
            paint::text_clipped(ctx, s, Role::Caption, plan.message.x + ctx.px(24) as i32, paint::baseline(ctx, Role::Caption, plan.message), plan.message.w.saturating_sub(ctx.px(24)), message, p.bad_ink);
        }

        paint::button(ctx, s, plan.ok, Weight::Primary, "ОК", false);
        paint::button(ctx, s, plan.cancel, Weight::Normal, "Отмена", false);
    }
}

/// An input box with a grey hint while empty and a caret when focused.
fn input(ctx: Ctx, s: &mut Surface, rect: Rect, text: &str, hint: &str, focused: bool) {
    let p = ctx.palette;
    paint::sunk(ctx, s, rect, ctx.px(6));
    draw::rounded_stroke(s, rect, ctx.px(6), if focused { p.acc } else { p.ink4 }, 255);
    let x = rect.x + ctx.px(10) as i32;
    let room = rect.w.saturating_sub(ctx.px(20));
    let y = paint::baseline(ctx, Role::Body, rect);
    if text.is_empty() {
        paint::text_clipped(ctx, s, Role::Body, x, y, room, hint, p.ink4);
    } else {
        // Long text: show its end, where the caret is.
        let face = ctx.face(Role::Body);
        let mut shown = text;
        while face.width(shown) > room.saturating_sub(ctx.px(4)) && !shown.is_empty() {
            let mut chars = shown.chars();
            chars.next();
            shown = chars.as_str();
        }
        paint::text_clipped(ctx, s, Role::Body, x, y, room, shown, p.ink);
    }
    if focused {
        let face = ctx.face(Role::Body);
        let cx = x + face.width(text).min(room) as i32;
        s.fill(Rect::new(cx + 1, rect.y + ctx.px(8) as i32, ctx.px(2).max(1), rect.h.saturating_sub(ctx.px(16))), p.acc);
    }
}

/// `/net/a/b` as `\\a\b`.
fn unc(path: &str) -> String {
    sysconf::winpath::net_to_unc(path).unwrap_or_else(|| path.to_string())
}

/// Can the folder be listed: `Ok`, or the kernel's error code.
fn access(path: &str) -> Result<(), i64> {
    let fd = open(path);
    if fd < 0 {
        return Err(fd);
    }
    let mut entry = Dirent::default();
    let first = readdir_raw(fd, &mut entry);
    close(fd);
    if first < 0 { Err(first) } else { Ok(()) }
}

fn read_reason() -> String {
    let fd = open("/net/.login");
    if fd < 0 {
        return String::new();
    }
    let mut buffer = [0u8; 256];
    let got = read(fd, &mut buffer);
    close(fd);
    String::from_utf8_lossy(&buffer[..got.max(0) as usize]).to_string()
}

fn why(code: i64, host: &str) -> String {
    match code {
        user_abi::ERR_NOT_FOUND => format!("Не удаётся найти «{host}» или эту папку. Проверьте имя."),
        user_abi::ERR_IO => format!("Компьютер «{host}» не отвечает (нет связи по SMB, порт 445)."),
        user_abi::ERR_UNSUPPORTED => String::from("Компьютер требует то, чего FossAi пока не умеет (например, шифрование SMB3)."),
        other => format!("Не удалось подключиться: код {other}."),
    }
}

/// A full-screen-free failure: print it and say it in a window.
struct Failure {
    text: String,
    done: bool,
}

impl App for Failure {
    fn key_with(&mut self, code: u32, _mods: i32, _latin: u32) -> bool {
        if code == ENTER || code == ESCAPE || code == SPACE {
            self.done = true;
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.cancel.contains(x, y) {
            self.done = true;
        }
        true
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn resizable(&self) -> bool {
        false
    }

    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Network credentials\x1fСеть"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let g = ctx.px(32);
        iconset::draw(s, Fi::Warning, plan.icon.x, plan.icon.y, g, p.bad_ink, 255);
        let tx = plan.icon.x + g as i32 + ctx.px(14) as i32;
        let room = (area.right() - tx - ctx.px(22) as i32).max(0) as u32;
        let line = i32::from(ctx.face(Role::Body).line) + ctx.px(4) as i32;
        let mut y = plan.icon.y;
        for part in wrap(ctx, &self.text, room) {
            paint::text_clipped(ctx, s, Role::Body, tx, y, room, &part, p.ink);
            y += line;
        }
        paint::button(ctx, s, plan.cancel, Weight::Primary, "Закрыть", false);
    }
}

fn wrap(ctx: Ctx, text: &str, width: u32) -> Vec<String> {
    let face = ctx.face(Role::Body);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split(' ') {
        let candidate = if current.is_empty() { word.to_string() } else { format!("{current} {word}") };
        if face.width(&candidate) > width && !current.is_empty() {
            lines.push(core::mem::replace(&mut current, word.to_string()));
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: values as the kernel passed them to `_start`.
    let args = unsafe { Args::new(argc, argv) };
    let mut open_after = false;
    let mut words: Vec<&str> = Vec::new();
    for index in 1..args.len() {
        match args.get(index) {
            Some("--open") => open_after = true,
            Some(word) => words.push(word),
            None => {}
        }
    }
    // The kernel splits the command line at spaces: a share name with spaces
    // arrives in pieces.
    let typed = words.join(" ");
    let path = String::from(sysconf::winpath::to_unix(typed.trim()).trim_end_matches('/'));
    let host = path.strip_prefix("/net/").and_then(|rest| rest.split('/').next()).filter(|h| !h.is_empty()).map(String::from);
    let Some(host) = host else {
        println(&format!("netlogon: not a network path: '{typed}'"));
        user_progs::exit(2);
    };
    let check = access(&path);
    println(&format!("netlogon: {path}: {}", match check {
        Ok(()) => String::from("open"),
        Err(code) => format!("error {code}"),
    }));
    let info = app::start(&SPEC);
    let scale = app::ui_scale(&info);
    let width = (500u32 * scale).min(info.screen_w);
    let height = (360u32 * scale).min(info.screen_h);
    match check {
        Ok(()) => {
            if open_after {
                spawn(&format!("/bin/files {path}"));
            }
            user_progs::exit(0);
        }
        Err(user_abi::ERR_PERMISSION) => {
            let user = default_user(&host);
            app::run(&SPEC, &info, (width, height), move |_, _| Logon {
                path: path.clone(),
                host: host.clone(),
                open_after,
                focus: if user.is_empty() { Focus::User } else { Focus::Password },
                user: user.clone(),
                password: String::new(),
                remember: false,
                message: None,
                done: false,
            })
        }
        Err(code) => {
            let text = format!("Нет доступа к {}. {}", unc(&path), why(code, &host));
            app::run(&SPEC, &info, (width, (220u32 * scale).min(info.screen_h)), move |_, _| Failure { text: text.clone(), done: false })
        }
    }
}

/// Nothing pre-filled: the account on the other computer is usually not
/// ours. Kept as a function for a future "last user for this host".
fn default_user(_host: &str) -> String {
    String::new()
}
