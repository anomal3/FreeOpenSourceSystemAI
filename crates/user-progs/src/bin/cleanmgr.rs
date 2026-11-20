//! «Очистка диска» (фаза П2, 20.11) — как cleanmgr в Windows.
//!
//! # Что чистится
//!
//! Только то, что система или программы создадут заново, и только по галочке:
//!
//! * **Временные файлы** — `/tmp`.
//! * **Корзина** — `~/.local/share/Trash` (то, что «Удалить» на столе перенесло).
//! * **Загруженные обновления** — `/var/cache/updates` (`sysupdate get`).
//! * **Загруженные драйверы** — `/var/cache/drivers` (`drvd`).
//! * **Кэш программ** — `~/.cache` (по умолчанию выключено: Proton держит там
//!   префиксы, и первый запуск игры после очистки будет долгим).
//!
//! Размер считается обходом каталога; удаление — тем же обходом снизу вверх.
//! Ссылки (`S_IFLNK` в `mode`) не раскрываются: ссылка на `/` внутри `/tmp`
//! иначе стёрла бы систему. Глубина обхода ограничена.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{Dirent, KIND_DIRECTORY, WIN_KEY_DOWN, WIN_KEY_UP, close, open, println, readdir, remove, stat};

const SPEC: Spec = Spec { name: "cleanmgr", title: "Disk Cleanup", period_ms: 10_000 };

const ENTER: u32 = '\n' as u32;
const SPACE: u32 = ' ' as u32;
const MAX_DEPTH: usize = 24;
const S_IFMT: u32 = 0o170_000;
const S_IFLNK: u32 = 0o120_000;

struct Category {
    title: &'static str,
    about: &'static str,
    icon: Fi,
    path: String,
    /// Удалять сам каталог (нет) или только его содержимое (да).
    checked: bool,
    bytes: u64,
    files: u64,
    exists: bool,
}

struct Plan {
    head: Rect,
    list: Rect,
    clean: Rect,
    rescan: Rect,
    status: Rect,
    row_h: u32,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let status_h = ctx.px(theme::STATUS_H);
        let status = Rect::new(area.x, area.bottom() - status_h as i32, area.w, status_h);
        let pad = ctx.px(20) as i32;
        let head = Rect::new(area.x + pad, area.y + ctx.px(18) as i32, area.w.saturating_sub(pad as u32 * 2), ctx.px(64));
        let bh = ctx.px(34);
        let by = status.y - ctx.px(16) as i32 - bh as i32;
        let bw = ctx.px(170);
        let clean = Rect::new(area.right() - pad - bw as i32, by, bw, bh);
        let rescan = Rect::new(clean.x - ctx.px(10) as i32 - ctx.px(150) as i32, by, ctx.px(150), bh);
        let list = Rect::new(area.x + pad, head.bottom() + ctx.px(10) as i32, area.w.saturating_sub(pad as u32 * 2), (by - head.bottom() - ctx.px(26) as i32).max(0) as u32);
        Self { head, list, clean, rescan, status, row_h: ctx.px(62) }
    }
}

struct Cleaner {
    categories: Vec<Category>,
    selected: usize,
    note: Option<String>,
    /// Подтверждение: второе нажатие «Очистить» в течение этого окна.
    confirm: bool,
}

impl Cleaner {
    fn new() -> Self {
        let home = home_dir();
        let category = |title, about, icon, path: String, checked| Category { title, about, icon, path, checked, bytes: 0, files: 0, exists: false };
        let mut cleaner = Self {
            categories: alloc::vec![
                category("Временные файлы", "/tmp — то, что программы оставили после себя", Fi::File, String::from("/tmp"), true),
                category("Корзина", "Удалённое со стола и из «Файлов»", Fi::Trash, format!("{home}/.local/share/Trash"), true),
                category("Загруженные обновления системы", "/var/cache/updates — уже применённые пакеты sysupdate", Fi::Download, String::from("/var/cache/updates"), true),
                category("Загруженные драйверы", "/var/cache/drivers — пакеты, скачанные drvd", Fi::Package, String::from("/var/cache/drivers"), true),
                category("Кэш программ", "~/.cache — Proton и другие создадут его заново (первый запуск дольше)", Fi::Layers, format!("{home}/.cache"), false),
            ],
            selected: 0,
            note: None,
            confirm: false,
        };
        cleaner.scan();
        cleaner
    }

    fn scan(&mut self) {
        for category in &mut self.categories {
            let mut info = user_abi::Stat::default();
            category.exists = stat(&category.path, &mut info) >= 0 && info.kind == KIND_DIRECTORY;
            let (bytes, files) = if category.exists { measure(&category.path, 0) } else { (0, 0) };
            category.bytes = bytes;
            category.files = files;
            println(&format!("cleanmgr: {} {} byte(s) in {} file(s)", category.path, bytes, files));
        }
        let total = self.selected_bytes();
        println(&format!("cleanmgr: scan done, {total} byte(s) selected"));
    }

    fn selected_bytes(&self) -> u64 {
        self.categories.iter().filter(|category| category.checked).map(|category| category.bytes).sum()
    }

    fn clean(&mut self) {
        if !self.confirm {
            self.confirm = true;
            self.note = Some(format!("Удалить {} без возврата? Нажмите «Очистить» ещё раз", size_text(self.selected_bytes())));
            return;
        }
        self.confirm = false;
        let mut freed = 0u64;
        let mut failed = 0u64;
        for category in self.categories.iter().filter(|category| category.checked && category.exists) {
            let (bytes, bad) = wipe_children(&category.path, 0);
            freed += bytes;
            failed += bad;
            println(&format!("cleanmgr: cleaned {}: {bytes} byte(s) freed, {bad} left", category.path));
        }
        println(&format!("cleanmgr: freed {freed} byte(s), {failed} item(s) could not be removed"));
        self.scan();
        self.note = Some(if failed == 0 {
            format!("Освобождено {}", size_text(freed))
        } else {
            format!("Освобождено {} · не удалось удалить: {failed} (нет прав или файл занят)", size_text(freed))
        });
    }

    fn toggle(&mut self, index: usize) {
        if let Some(category) = self.categories.get_mut(index) {
            category.checked = !category.checked;
            self.confirm = false;
            self.note = None;
        }
    }
}

impl App for Cleaner {
    fn title(&self) -> Option<String> {
        Some(String::from("Disk Cleanup\x1fОчистка диска"))
    }

    fn key(&mut self, code: u32) -> bool {
        match code {
            WIN_KEY_UP => self.selected = self.selected.saturating_sub(1),
            WIN_KEY_DOWN => self.selected = (self.selected + 1).min(self.categories.len() - 1),
            SPACE => self.toggle(self.selected),
            ENTER => self.clean(),
            code if code == 'r' as u32 => self.scan(),
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.clean.contains(x, y) {
            self.clean();
        } else if plan.rescan.contains(x, y) {
            self.confirm = false;
            self.scan();
        } else if plan.list.contains(x, y) {
            let index = ((y - plan.list.y) as u32 / plan.row_h) as usize;
            if index < self.categories.len() {
                self.selected = index;
                self.toggle(index);
            }
        } else {
            return false;
        }
        true
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let tile = Rect::new(plan.head.x, plan.head.y, ctx.px(48), ctx.px(48));
        draw::rounded(s, tile, ctx.px(10), ctx.flat(p.acctint), 255);
        draw::rounded_stroke(s, tile, ctx.px(10), p.accedge, 255);
        let g = ctx.px(26);
        iconset::draw(s, Fi::Drive, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, p.acc_ink, 255);
        let tx = tile.right() + ctx.px(16) as i32;
        let room = (plan.head.right() - tx).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Heading, tx, tile.y, room, "Очистка диска", p.ink);
        let sub = format!("Можно освободить {} — отметьте, что удалить", size_text(self.selected_bytes()));
        paint::text_clipped(ctx, s, Role::Body, tx, tile.y + i32::from(ctx.face(Role::Heading).line) + ctx.px(4) as i32, room, &sub, p.ink3);

        for (index, category) in self.categories.iter().enumerate() {
            let row = Rect::new(plan.list.x, plan.list.y + (index as u32 * plan.row_h) as i32, plan.list.w, plan.row_h.saturating_sub(ctx.px(6)));
            if row.bottom() > plan.list.bottom() {
                break;
            }
            let on = index == self.selected;
            paint::card(ctx, s, row);
            if on {
                draw::rounded_stroke(s, row, ctx.px(theme::R_CARD), p.accedge, 255);
            }
            let inner = ctx.on(ctx.flat(p.card));
            // Галочка.
            let box_side = ctx.px(20);
            let check = Rect::new(row.x + ctx.px(16) as i32, row.y + (row.h - box_side) as i32 / 2, box_side, box_side);
            if category.checked {
                draw::rounded(s, check, ctx.px(4), p.acc, 255);
                iconset::draw(s, Fi::Check, check.x + ctx.px(3) as i32, check.y + ctx.px(3) as i32, box_side - ctx.px(6), mini_ui::Color::rgb(0xFF, 0xFF, 0xFF), 255);
            } else {
                draw::rounded_stroke(s, check, ctx.px(4), p.line3.color, 255);
            }
            let ig = ctx.px(20);
            let ix = check.right() + ctx.px(16) as i32;
            iconset::draw(s, category.icon, ix, row.y + (row.h - ig) as i32 / 2, ig, p.ink2, 255);
            let lx = ix + (ig + ctx.px(14)) as i32;
            let size = if category.exists { size_text(category.bytes) } else { String::from("нет") };
            let size_w = inner.face(Role::Strong).width(&size) + ctx.px(20);
            let lroom = (row.right() - lx - size_w as i32).max(0) as u32;
            let line = i32::from(ctx.face(Role::Body).line);
            let ty = row.y + (row.h as i32 - line * 2 - ctx.px(2) as i32) / 2;
            paint::text_clipped(inner, s, Role::Body, lx, ty, lroom, category.title, p.ink);
            let about = format!("{} · файлов: {}", category.about, category.files);
            paint::text_clipped(inner, s, Role::Caption, lx, ty + line + ctx.px(2) as i32, lroom, &about, p.ink4);
            paint::text_right(inner, s, Role::Strong, row.right() - ctx.px(16) as i32, paint::baseline(inner, Role::Strong, row), &size, if category.checked { p.ink } else { p.ink4 });
        }

        paint::button(ctx, s, plan.rescan, Weight::Normal, "Пересчитать", false);
        let label = if self.confirm { "Да, удалить" } else { "Очистить" };
        paint::button(ctx, s, plan.clean, if self.confirm { Weight::Danger } else { Weight::Primary }, label, false);
        let status = self.note.clone().unwrap_or_else(|| String::from("Пробел — отметить, Enter — очистить, R — пересчитать"));
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

// ---------------------------------------------------------------------------
// Обход
// ---------------------------------------------------------------------------

fn entries(path: &str) -> Vec<(String, bool, u64, u32)> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if name == "." || name == ".." || name.is_empty() {
            continue;
        }
        out.push((name, entry.kind == KIND_DIRECTORY, entry.size, entry.mode));
    }
    close(fd);
    out
}

fn is_link(mode: u32) -> bool {
    mode & S_IFMT == S_IFLNK
}

/// Байты и файлы под `path`.
fn measure(path: &str, depth: usize) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut files = 0u64;
    for (name, dir, size, mode) in entries(path) {
        let child = format!("{}/{name}", path.trim_end_matches('/'));
        if dir && !is_link(mode) && depth < MAX_DEPTH {
            let (b, f) = measure(&child, depth + 1);
            bytes += b;
            files += f;
        } else {
            bytes += size;
            files += 1;
        }
    }
    (bytes, files)
}

/// Удалить всё под `path` (сам `path` остаётся). Ответ — освобождено байт и
/// сколько записей осталось.
fn wipe_children(path: &str, depth: usize) -> (u64, u64) {
    let mut freed = 0u64;
    let mut left = 0u64;
    for (name, dir, size, mode) in entries(path) {
        let child = format!("{}/{name}", path.trim_end_matches('/'));
        if dir && !is_link(mode) {
            if depth >= MAX_DEPTH {
                left += 1;
                continue;
            }
            let (f, l) = wipe_children(&child, depth + 1);
            freed += f;
            left += l;
            if remove(&child) < 0 {
                left += 1;
            }
        } else if remove(&child) >= 0 {
            freed += size;
        } else {
            left += 1;
        }
    }
    (freed, left)
}

fn size_text(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{},{} ГБ", bytes / GIB, bytes % GIB * 10 / GIB)
    } else if bytes >= MIB {
        format!("{},{} МБ", bytes / MIB, bytes % MIB * 10 / MIB)
    } else if bytes >= KIB {
        format!("{} КБ", bytes / KIB)
    } else {
        format!("{bytes} Б")
    }
}

fn home_dir() -> String {
    let me = user_progs::uid();
    if me == 0 {
        return String::from("/root");
    }
    for (name, dir, _, _) in entries("/home") {
        if !dir || name.starts_with('.') {
            continue;
        }
        let path = format!("/home/{name}");
        let mut info = user_abi::Stat::default();
        if stat(&path, &mut info) >= 0 && info.uid == me {
            return path;
        }
    }
    String::from("/home")
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let scale = theme::geometry_scale_for(user_abi::sysinfo_scale_choice(info.flags), info.screen_w.max(1), info.screen_h.max(1));
    let width = (640 * scale).min(info.screen_w);
    let height = (560 * scale).min(info.screen_h.saturating_sub(60 * scale)).max(400);
    app::run(&SPEC, &info, (width, height), |_, _| Cleaner::new())
}
