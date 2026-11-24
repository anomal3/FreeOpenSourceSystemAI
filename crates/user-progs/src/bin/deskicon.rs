//! «Параметры значков рабочего стола» (24.11, Roman: «как в Windows»).
//!
//! # What it is
//!
//! The dialog of Windows' «Значки рабочего стола»: checkboxes of the system
//! icons («Компьютер», «Файлы пользователя», «Сеть», «Корзина», «Панель
//! управления» — and ours, «Терминал» and «О системе»), a preview of them,
//! «Сменить значок…» (a picture from the icon theme) and «Обычный значок»,
//! then «ОК», «Отмена», «Применить». Changes wait for «ОК» or «Применить».
//!
//! # How it talks to the desktop
//!
//! The icons are the desktop's (`kernel::ui::deskicons`); the dialog asks it
//! by `SYS_KERNEL_COMMAND`: `ui deskicons` answers lines `deskicons KEY on|off
//! ART LABEL`, `ui deskicons show|hide KEY` and `ui deskicons icon KEY
//! NAME|default` change them (allowed to every user — only the desktop icons
//! change). The desktop writes `desktop.cfg` and redraws itself.
//!
//! Started by «Персонализация» (the desktop's `ui::open_desktop_icons_dialog`)
//! or as `deskicon` from «Выполнить».

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
use user_progs::println;

const SPEC: Spec = Spec { name: "deskicon", title: "Desktop Icons", period_ms: 5_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;

/// The icons: key, checkbox caption, preview caption, default picture.
const ICONS: [(&str, &str, &str, &str); 7] = [
    ("computer", "Компьютер", "", "place-this-pc"),
    ("user", "Файлы пользователя", "", "folder-user"),
    ("network", "Сеть", "", "network"),
    ("trash", "Корзина", "", "place-trash-full"),
    ("control", "Панель управления", "", "settings"),
    ("terminal", "Терминал", "", "terminal"),
    ("about", "О системе", "", "about"),
];

/// Pictures «Сменить значок…» offers (names of the icon theme's pack).
const CHOICES: [&str; 30] = [
    "place-this-pc", "place-local-disk", "place-system-disk", "display", "sysinfo", "folder-user", "user", "folder",
    "folder-desktop", "folder-documents", "network", "place-network-folder", "wifi", "browser", "place-trash-empty",
    "place-trash-full", "settings", "admin", "compmgmt", "device-manager", "terminal", "about", "system-monitor",
    "task-manager", "files", "package", "services", "power", "lock", "search",
];

/// One icon in the dialog: the desktop's state and the dialog's pending one.
#[derive(Clone)]
struct Icon {
    key: &'static str,
    title: &'static str,
    default_art: &'static str,
    label: String,
    shown: bool,
    /// Picture chosen (`None` — the default).
    art: Option<String>,
}

impl Icon {
    fn art(&self) -> &str {
        self.art.as_deref().unwrap_or(self.default_art)
    }
}

struct Plan {
    group: Rect,
    checks: Vec<Rect>,
    preview: Rect,
    cells: Vec<Rect>,
    change: Rect,
    reset: Rect,
    ok: Rect,
    cancel: Rect,
    apply: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, count: usize, cells: usize) -> Self {
        let px = |v: u32| ctx.px(v);
        let pad = px(16) as i32;
        let inner_w = area.w.saturating_sub(px(32));
        let rows = count.div_ceil(2) as u32;
        let group = Rect::new(area.x + pad, area.y + px(12) as i32, inner_w, px(34) + rows * px(28) + px(8));
        let col_w = (group.w.saturating_sub(px(24))) / 2;
        let checks = (0..count)
            .map(|n| {
                // Column-wise like Windows: the left column first.
                let (col, row) = ((n as u32) / rows, (n as u32) % rows);
                Rect::new(group.x + px(12) as i32 + (col * col_w) as i32, group.y + px(32) as i32 + (row * px(28)) as i32, col_w, px(26))
            })
            .collect();
        let bh = px(32);
        let bottom = area.bottom() - pad - bh as i32;
        let bw = px(104);
        let gap = px(8) as i32;
        let apply = Rect::new(area.right() - pad - bw as i32, bottom, bw, bh);
        let cancel = Rect::new(apply.x - gap - bw as i32, bottom, bw, bh);
        let ok = Rect::new(cancel.x - gap - bw as i32, bottom, bw, bh);
        let cw = px(148);
        let row_y = bottom - px(18) as i32 - bh as i32;
        let reset = Rect::new(area.right() - pad - cw as i32, row_y, cw, bh);
        let change = Rect::new(reset.x - gap - cw as i32, row_y, cw, bh);
        let preview = Rect::new(area.x + pad, group.bottom() + px(14) as i32, inner_w, (row_y - px(12) as i32 - group.bottom() - px(14) as i32).max(px(60) as i32) as u32);
        let cell_w = px(96);
        let cell_h = px(92);
        let per_row = (preview.w.saturating_sub(px(16)) / cell_w).max(1);
        let cells = (0..cells)
            .map(|n| {
                let (col, row) = (n as u32 % per_row, n as u32 / per_row);
                Rect::new(preview.x + px(8) as i32 + (col * cell_w) as i32, preview.y + px(8) as i32 + (row * cell_h) as i32, cell_w, cell_h)
            })
            .collect();
        Self { group, checks, preview, cells, change, reset, ok, cancel, apply }
    }
}

struct Dialog {
    /// What the desktop has now and what the dialog shows.
    saved: Vec<Icon>,
    icons: Vec<Icon>,
    /// Selected icon of the preview (index in `icons`).
    selected: usize,
    /// «Сменить значок…» is open: the grid of pictures instead of the preview.
    picking: bool,
    message: Option<String>,
    done: bool,
}

/// Ask the desktop (`ui deskicons …`); the answer's text.
fn desktop(request: &str) -> Option<String> {
    let mut buffer = alloc::vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = user_progs::kernel_command(request, &mut buffer);
    if got < 0 {
        println(&format!("deskicon: '{request}' failed: {got}"));
        return None;
    }
    Some(String::from_utf8_lossy(&buffer[..got as usize]).to_string())
}

fn load() -> Vec<Icon> {
    let answer = desktop("ui deskicons").unwrap_or_default();
    ICONS
        .iter()
        .map(|&(key, title, _, default_art)| {
            let line = answer.lines().find_map(|line| line.strip_prefix("deskicons ").filter(|rest| rest.split_whitespace().next() == Some(key)));
            let mut parts = line.unwrap_or("").splitn(4, ' ');
            let _ = parts.next();
            let shown = parts.next() == Some("on");
            let art = parts.next().filter(|art| *art != "-" && *art != default_art).map(String::from);
            let label = parts.next().map_or_else(|| title.to_string(), String::from);
            Icon { key, title, default_art, label, shown, art }
        })
        .collect()
}

impl Dialog {
    fn new() -> Self {
        let saved = load();
        Self { icons: saved.clone(), saved, selected: 0, picking: false, message: None, done: false }
    }

    /// Preview cells: every icon (the trash twice — full and empty, as in
    /// Windows). The answer — (icon index, picture).
    fn cells(&self) -> Vec<(usize, String, String)> {
        let mut out = Vec::new();
        for (index, icon) in self.icons.iter().enumerate() {
            if icon.key == "trash" && icon.art.is_none() {
                out.push((index, String::from("place-trash-full"), format!("{} (полная)", icon.label)));
                out.push((index, String::from("place-trash-empty"), format!("{} (пустая)", icon.label)));
            } else {
                out.push((index, icon.art().to_string(), icon.label.clone()));
            }
        }
        out
    }

    fn changed(&self) -> bool {
        self.icons.iter().zip(&self.saved).any(|(now, was)| now.shown != was.shown || now.art != was.art)
    }

    /// Send the differences to the desktop.
    fn apply(&mut self) {
        let mut sent = 0;
        for (now, was) in self.icons.iter().zip(&self.saved) {
            if now.shown != was.shown {
                desktop(&format!("ui deskicons {} {}", if now.shown { "show" } else { "hide" }, now.key));
                sent += 1;
            }
            if now.art != was.art {
                desktop(&format!("ui deskicons icon {} {}", now.key, now.art.as_deref().unwrap_or("default")));
                sent += 1;
            }
        }
        println(&format!("deskicon: applied {sent} change(s)"));
        self.saved = load();
        // Names come from the desktop; the pending choices stay as applied.
        for (now, fresh) in self.icons.iter_mut().zip(&self.saved) {
            now.label = fresh.label.clone();
        }
    }
}

impl App for Dialog {
    fn key_with(&mut self, code: u32, _mods: i32, _latin: u32) -> bool {
        match code {
            ESCAPE if self.picking => self.picking = false,
            ESCAPE => {
                println("deskicon: cancelled");
                self.done = true;
            }
            ENTER => {
                self.apply();
                self.done = true;
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let cells = self.cells();
        let plan = Plan::new(ctx, area, self.icons.len(), if self.picking { CHOICES.len() } else { cells.len() });
        if let Some(n) = plan.checks.iter().position(|rect| rect.contains(x, y)) {
            if let Some(icon) = self.icons.get_mut(n) {
                icon.shown = !icon.shown;
                println(&format!("deskicon: {} {}", icon.key, if icon.shown { "checked" } else { "unchecked" }));
            }
            return true;
        }
        if let Some(n) = plan.cells.iter().position(|rect| rect.contains(x, y)) {
            if self.picking {
                if let (Some(name), Some(icon)) = (CHOICES.get(n), self.icons.get_mut(self.selected)) {
                    icon.art = (*name != icon.default_art).then(|| String::from(*name));
                    println(&format!("deskicon: {} picture {name}", icon.key));
                }
                self.picking = false;
            } else if let Some((index, _, _)) = cells.get(n) {
                self.selected = *index;
            }
            return true;
        }
        if plan.change.contains(x, y) {
            self.picking = !self.picking;
            if self.picking {
                println(&format!("deskicon: choosing a picture for {}", self.icons[self.selected].key));
            }
        } else if plan.reset.contains(x, y) {
            if let Some(icon) = self.icons.get_mut(self.selected) {
                icon.art = None;
                println(&format!("deskicon: {} default picture", icon.key));
            }
            self.picking = false;
        } else if plan.ok.contains(x, y) {
            self.apply();
            self.done = true;
        } else if plan.cancel.contains(x, y) {
            println("deskicon: cancelled");
            self.done = true;
        } else if plan.apply.contains(x, y) {
            self.apply();
        } else {
            return false;
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
        // The window's caption holds about 48 bytes: the short name.
        Some(String::from("Desktop Icons\x1fЗначки рабочего стола"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        let px = |v: u32| ctx.px(v);
        s.fill(area, theme::window_bg());
        let cells = self.cells();
        let plan = Plan::new(ctx, area, self.icons.len(), if self.picking { CHOICES.len() } else { cells.len() });

        // «Значки рабочего стола»: a frame with the checkboxes.
        draw::rounded_stroke(s, plan.group, px(6), p.line3.color, p.line3.alpha);
        paint::text(ctx, s, Role::Strong, plan.group.x + px(12) as i32, plan.group.y + px(8) as i32, "Значки рабочего стола", p.ink2);
        for (rect, icon) in plan.checks.iter().zip(&self.icons) {
            let side = px(18);
            let mark = Rect::new(rect.x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            if icon.shown {
                draw::rounded(s, mark, px(4), p.acc, 255);
                iconset::draw(s, Fi::Check, mark.x + px(2) as i32, mark.y + px(2) as i32, px(14), mini_ui::Color::rgb(0xFF, 0xFF, 0xFF), 255);
            } else {
                draw::rounded(s, mark, px(4), p.sunk.over(theme::window_bg()), 255);
                draw::rounded_stroke(s, mark, px(4), p.line3.color, p.line3.alpha.saturating_add(60));
            }
            paint::text_clipped(ctx, s, Role::Body, mark.right() + px(8) as i32, paint::baseline(ctx, Role::Body, *rect), rect.w.saturating_sub(side + px(8)), icon.title, p.ink);
        }

        // The preview, or the pictures to choose from.
        paint::sunk(ctx, s, plan.preview, px(6));
        let face = ctx.face(Role::Caption);
        if self.picking {
            for (rect, name) in plan.cells.iter().zip(CHOICES.iter()) {
                if rect.bottom() > plan.preview.bottom() {
                    break;
                }
                let current = self.icons.get(self.selected).is_some_and(|icon| icon.art() == *name);
                if current {
                    draw::rounded(s, rect.shrink(px(4)), px(6), p.sel1.over(theme::window_bg()), 255);
                }
                let side = px(48);
                let tile = Rect::new(rect.x + (rect.w as i32 - side as i32) / 2, rect.y + px(10) as i32, side, side);
                if !user_progs::icons::draw(s, tile, name) {
                    iconset::draw(s, Fi::Image, tile.x, tile.y, side, p.ink4, 255);
                }
                let line = Rect::new(rect.x + px(2) as i32, tile.bottom() + px(6) as i32, rect.w.saturating_sub(px(4)), u32::from(face.line));
                paint::text_centered(ctx, s, Role::Caption, line, name, p.ink3);
            }
        } else {
            for (rect, (index, art, label)) in plan.cells.iter().zip(cells.iter()) {
                if rect.bottom() > plan.preview.bottom() {
                    break;
                }
                if *index == self.selected {
                    draw::rounded(s, rect.shrink(px(4)), px(6), p.sel1.over(theme::window_bg()), 255);
                    draw::rounded_stroke(s, rect.shrink(px(4)), px(6), p.accedge, 255);
                }
                let side = px(48);
                let tile = Rect::new(rect.x + (rect.w as i32 - side as i32) / 2, rect.y + px(8) as i32, side, side);
                if !user_progs::icons::draw(s, tile, art) {
                    iconset::draw(s, Fi::Image, tile.x, tile.y, side, p.ink4, 255);
                }
                // Up to two lines under the picture, as on the desktop.
                let room = rect.w.saturating_sub(px(6));
                let (first, rest) = split_label(label, |text| face.width(text), room);
                let line = Rect::new(rect.x + px(3) as i32, tile.bottom() + px(4) as i32, room, u32::from(face.line));
                paint::text_centered(ctx, s, Role::Caption, line, &first, p.ink);
                if let Some(rest) = rest {
                    let second = Rect::new(line.x, line.bottom(), room, u32::from(face.line));
                    paint::text_centered(ctx, s, Role::Caption, second, &rest, p.ink);
                }
            }
        }

        paint::button(ctx, s, plan.change, if self.picking { Weight::Primary } else { Weight::Normal }, if self.picking { "Отменить выбор" } else { "Сменить значок…" }, false);
        paint::button(ctx, s, plan.reset, Weight::Normal, "Обычный значок", false);
        if let Some(message) = &self.message {
            paint::text(ctx, s, Role::Caption, plan.preview.x, plan.change.y + px(8) as i32, message, p.ink4);
        } else if self.picking {
            let name = self.icons.get(self.selected).map_or("", |icon| icon.label.as_str());
            paint::text_clipped(ctx, s, Role::Caption, plan.preview.x, paint::baseline(ctx, Role::Caption, plan.change), plan.change.x.saturating_sub(plan.preview.x + px(8) as i32) as u32, &format!("Значок для «{name}»"), p.ink3);
        }
        paint::button(ctx, s, plan.ok, Weight::Primary, "ОК", false);
        paint::button(ctx, s, plan.cancel, Weight::Normal, "Отмена", false);
        paint::button(ctx, s, plan.apply, Weight::Normal, "Применить", false);
        if !self.changed() {
            // «Применить» greyed while nothing changed, as in Windows.
            draw::rounded(s, plan.apply, px(6), theme::window_bg(), 140);
        }
    }
}

/// Split a caption into up to two lines that fit `room` points.
fn split_label(label: &str, width: impl Fn(&str) -> u32, room: u32) -> (String, Option<String>) {
    if width(label) <= room {
        return (label.to_string(), None);
    }
    let mut first = String::new();
    let mut rest = String::new();
    for word in label.split(' ') {
        let candidate = if first.is_empty() { word.to_string() } else { format!("{first} {word}") };
        if rest.is_empty() && width(&candidate) <= room {
            first = candidate;
        } else {
            if !rest.is_empty() {
                rest.push(' ');
            }
            rest.push_str(word);
        }
    }
    if first.is_empty() {
        // One long word: cut by characters.
        let mut cut = String::new();
        for ch in label.chars() {
            cut.push(ch);
            if width(&cut) > room {
                cut.pop();
                break;
            }
        }
        rest = label.chars().skip(cut.chars().count()).collect();
        first = cut;
    }
    (first, (!rest.is_empty()).then_some(rest))
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let scale = app::ui_scale(&info);
    let width = (460u32 * scale).min(info.screen_w);
    let height = (520u32 * scale).min(info.screen_h);
    app::run(&SPEC, &info, (width, height), |_, _| Dialog::new())
}
