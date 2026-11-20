//! «Автозагрузка» (фаза П3, 20.11): что открывается само, когда система
//! поднялась, — программы при входе и службы init, как вкладка «Автозагрузка»
//! диспетчера задач и msconfig в Windows.
//!
//! # Два списка, два файла
//!
//! * **Программы при входе** — `/etc/autostart`, по строке `on <команда>` или
//!   `off <команда>`, `#` — комментарий (сохраняется как есть). Исполняет его
//!   служба `autorun` (`/bin/autorun`), когда стол поднялся. Выключенная
//!   строка остаётся в файле: включить обратно — одно нажатие.
//! * **Службы системы** — `/etc/services`, тот самый файл, который читает
//!   `/bin/init`. Выключенная служба — строка, закомментированная как
//!   `#off <строка>`: init пропускает комментарии, а окно узнаёт свою пометку и
//!   включает службу обратно, не теряя её учётную запись. Пока `/etc/services`
//!   нет, читается эталон образа (`/usr/share/defaults/etc/services`); первая
//!   правка кладёт в `/etc` полную копию.
//!
//! Всё действует со **следующей загрузки**: init читает файл один раз, и окно
//! говорит это словами, а не делает вид, что служба уже остановлена.
//!
//! # Почему `logger` не выключается
//!
//! `logger` (`/bin/svclog`) принимает журналы остальных служб. Без него их
//! сообщения пропадают, и сломанную службу не разобрать — поэтому переключатель
//! у него неактивен, а окно объясняет почему.
//!
//! Менять списки может только root; у остальных окно только показывает.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_UP, close, open, open_write, println, read, stat, write};

const SPEC: Spec = Spec { name: "startup", title: "Startup Apps", period_ms: 5_000 };

const AUTOSTART: &str = "/etc/autostart";
const SERVICES: &str = "/etc/services";
const SERVICES_DEFAULT: &str = "/usr/share/defaults/etc/services";
/// Пометка выключенной службы в `/etc/services`.
const OFF_MARK: &str = "#off ";

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const SPACE: u32 = ' ' as u32;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;
const ROW_H: u32 = 44;
const FILE_LIMIT: usize = 16 * 1024;

/// Программы с окном, которые можно поставить в автозагрузку: имя в `/bin` и
/// подпись.
const CANDIDATES: [(&str, &str); 15] = [
    ("files", "Файлы"),
    ("notepad", "Блокнот"),
    ("calc", "Калькулятор"),
    ("taskmgr", "Диспетчер задач"),
    ("sysmon", "Монитор системы"),
    ("resmon", "Монитор ресурсов"),
    ("eventlog", "Журнал событий"),
    ("services", "Службы"),
    ("sysinfo", "Сведения о системе"),
    ("charmap", "Таблица символов"),
    ("cleanmgr", "Очистка диска"),
    ("devmgr", "Диспетчер устройств"),
    ("compmgmt", "Управление компьютером"),
    ("cfgedit", "Редактор конфигурации"),
    ("admin", "Администрирование"),
];

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Programs,
    Services,
}

/// Строка `/etc/autostart`.
enum AutoLine {
    /// Комментарий или пустая строка — как есть.
    Other(String),
    Entry { on: bool, command: String },
}

/// Служба — строка `/etc/services` (номер строки в файле).
struct Service {
    line: usize,
    name: String,
    exec: String,
    uid: Option<String>,
    on: bool,
}

struct Picker {
    /// Номера в [`CANDIDATES`], которые есть в `/bin`.
    items: Vec<usize>,
    selected: usize,
}

struct Plan {
    toolbar: Rect,
    tabs: kit::Tabs,
    add: Rect,
    remove: Rect,
    refresh: Rect,
    banner: Rect,
    header: Rect,
    list: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let caption = ctx.face(Role::Caption);
        let widths = [caption.width("Программы при входе") + ctx.px(28), caption.width("Службы системы") + ctx.px(28)];
        let tabs = kit::Tabs::new(ctx, frame.toolbar, frame.toolbar.x + ctx.px(12) as i32, &widths);
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let body = ctx.face(Role::Body);
        let mut x = tabs.frame.right() + ctx.px(16) as i32;
        let mut button = |label: &str| {
            let rect = Rect::new(x, by, body.width(label) + ctx.px(46), bh);
            x = rect.right() + ctx.px(6) as i32;
            rect
        };
        let add = button("Добавить");
        let remove = button("Убрать");
        let refresh = button("Обновить");
        let pad = ctx.px(16);
        let banner_h = u32::from(ctx.face(Role::Body).line) * 2 + ctx.px(26);
        let banner = Rect::new(frame.body.x + pad as i32, frame.body.y + ctx.px(12) as i32, frame.body.w.saturating_sub(pad * 2), banner_h);
        let header_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14);
        let header = Rect::new(frame.body.x, banner.bottom() + ctx.px(8) as i32, frame.body.w, header_h);
        let list = Rect::new(frame.body.x, header.bottom(), frame.body.w, (frame.body.bottom() - header.bottom()).max(0) as u32);
        Self { toolbar: frame.toolbar, tabs, add, remove, refresh, banner, header, list, status: frame.status }
    }

    /// Переключатель в строке.
    fn switch(&self, ctx: Ctx, row: Rect) -> Rect {
        let w = ctx.px(40);
        let h = ctx.px(22);
        Rect::new(row.x + ctx.px(12) as i32, row.y + (row.h as i32 - h as i32) / 2, w, h)
    }
}

struct Startup {
    section: Section,
    programs: Vec<AutoLine>,
    /// Строки `/etc/services` как есть и разобранные службы.
    service_lines: Vec<String>,
    services: Vec<Service>,
    /// Службы прочитаны из эталона образа: в `/etc` файла нет.
    services_default: bool,
    selected: usize,
    picker: Option<Picker>,
    note: Option<String>,
    root: bool,
    top: Cell<usize>,
    visible: Cell<usize>,
}

impl Startup {
    fn new() -> Self {
        let mut window = Self {
            section: Section::Programs,
            programs: Vec::new(),
            service_lines: Vec::new(),
            services: Vec::new(),
            services_default: false,
            selected: 0,
            picker: None,
            note: None,
            root: user_progs::uid() == 0,
            top: Cell::new(0),
            visible: Cell::new(1),
        };
        window.reload();
        println(&format!("startup: ready, {} program(s), {} service(s)", window.entries().len(), window.services.len()));
        window
    }

    fn reload(&mut self) {
        self.programs = read_text(AUTOSTART)
            .map(|text| {
                text.lines()
                    .map(|line| {
                        let trimmed = line.trim();
                        let entry = trimmed
                            .strip_prefix("on ")
                            .map(|rest| (true, rest))
                            .or_else(|| trimmed.strip_prefix("off ").map(|rest| (false, rest)));
                        match entry {
                            Some((on, command)) if !command.trim().is_empty() => AutoLine::Entry { on, command: command.trim().to_string() },
                            _ => AutoLine::Other(line.to_string()),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (text, default) = match read_text(SERVICES) {
            Some(text) => (text, false),
            None => (read_text(SERVICES_DEFAULT).unwrap_or_default(), true),
        };
        self.services_default = default;
        self.service_lines = text.lines().map(String::from).collect();
        self.parse_services();
        self.selected = self.selected.min(self.count().saturating_sub(1));
    }

    fn parse_services(&mut self) {
        self.services.clear();
        for (index, line) in self.service_lines.iter().enumerate() {
            let trimmed = line.trim();
            let (on, body) = match trimmed.strip_prefix(OFF_MARK) {
                Some(rest) => (false, rest.trim()),
                None if trimmed.starts_with('#') || trimmed.is_empty() => continue,
                None => (true, trimmed),
            };
            let mut fields = body.split_whitespace();
            let (Some(name), Some(exec)) = (fields.next(), fields.next()) else { continue };
            if !exec.starts_with('/') {
                continue;
            }
            let uid = fields.next().map(String::from);
            self.services.push(Service { line: index, name: name.to_string(), exec: exec.to_string(), uid, on });
        }
    }

    /// Номера строк-программ в `self.programs`.
    fn entries(&self) -> Vec<usize> {
        self.programs.iter().enumerate().filter(|(_, line)| matches!(line, AutoLine::Entry { .. })).map(|(index, _)| index).collect()
    }

    fn count(&self) -> usize {
        match self.section {
            Section::Programs => self.entries().len(),
            Section::Services => self.services.len(),
        }
    }

    fn set_section(&mut self, section: Section) {
        self.section = section;
        self.selected = 0;
        self.top.set(0);
        self.picker = None;
        self.note = None;
    }

    fn may_change(&mut self) -> bool {
        if !self.root {
            self.note = Some(String::from("Менять автозагрузку может только администратор (root)"));
        }
        self.root
    }

    fn toggle(&mut self) {
        match self.section {
            Section::Programs => {
                let entries = self.entries();
                let Some(&index) = entries.get(self.selected) else { return };
                if !self.may_change() {
                    return;
                }
                let (now_on, command) = match &mut self.programs[index] {
                    AutoLine::Entry { on, command } => {
                        *on = !*on;
                        (*on, command.clone())
                    }
                    AutoLine::Other(_) => return,
                };
                if self.save_programs() {
                    println(&format!("startup: '{command}' {} saved", if now_on { "on" } else { "off" }));
                    self.note = Some(format!(
                        "«{command}» {} при следующем запуске стола",
                        if now_on { "откроется" } else { "не откроется" }
                    ));
                } else if let AutoLine::Entry { on, .. } = &mut self.programs[index] {
                    *on = !*on;
                }
            }
            Section::Services => {
                let Some(service) = self.services.get(self.selected) else { return };
                if service.name == "logger" {
                    self.note = Some(String::from(
                        "«logger» собирает журналы остальных служб: без него их сообщения пропадут — выключать нельзя",
                    ));
                    return;
                }
                let (line, name, on) = (service.line, service.name.clone(), !service.on);
                if !self.may_change() {
                    return;
                }
                let before = self.service_lines[line].clone();
                let body = before.trim().strip_prefix(OFF_MARK).unwrap_or(before.trim()).trim().to_string();
                self.service_lines[line] = if on { body } else { format!("{OFF_MARK}{body}") };
                if self.save_services() {
                    println(&format!("startup: service '{name}' {} saved", if on { "on" } else { "off" }));
                    self.note = Some(format!(
                        "Служба «{name}» {} со следующей загрузки",
                        if on { "запустится" } else { "не запустится" }
                    ));
                    self.parse_services();
                } else {
                    self.service_lines[line] = before;
                }
            }
        }
    }

    fn open_picker(&mut self) {
        if self.section != Section::Programs {
            self.set_section(Section::Programs);
        }
        if !self.may_change() {
            return;
        }
        let items: Vec<usize> = CANDIDATES
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| {
                let mut info = user_abi::Stat::default();
                stat(&format!("/bin/{name}"), &mut info) >= 0
            })
            .map(|(index, _)| index)
            .collect();
        println(&format!("startup: picker with {} program(s)", items.len()));
        self.picker = Some(Picker { items, selected: 0 });
    }

    fn add_picked(&mut self) {
        let Some(picker) = self.picker.take() else { return };
        let Some(&index) = picker.items.get(picker.selected) else { return };
        let (name, caption) = CANDIDATES[index];
        let command = format!("/bin/{name}");
        let present = self.programs.iter().any(|line| matches!(line, AutoLine::Entry { command: known, .. } if *known == command));
        if present {
            self.note = Some(format!("«{caption}» уже в списке"));
            return;
        }
        self.programs.push(AutoLine::Entry { on: true, command: command.clone() });
        if self.save_programs() {
            println(&format!("startup: '{command}' on saved"));
            self.note = Some(format!("«{caption}» откроется при следующем запуске стола"));
            self.selected = self.entries().len().saturating_sub(1);
        } else {
            self.programs.pop();
        }
    }

    fn remove(&mut self) {
        if self.section != Section::Programs {
            self.note = Some(String::from("Службу не убирают из списка, а выключают: переключатель слева"));
            return;
        }
        let entries = self.entries();
        let Some(&index) = entries.get(self.selected) else { return };
        if !self.may_change() {
            return;
        }
        let removed = self.programs.remove(index);
        if self.save_programs() {
            if let AutoLine::Entry { command, .. } = &removed {
                println(&format!("startup: '{command}' removed saved"));
                self.note = Some(format!("«{command}» убрана из автозагрузки"));
            }
            self.selected = self.selected.min(self.entries().len().saturating_sub(1));
        } else {
            self.programs.insert(index, removed);
        }
    }

    fn save_programs(&mut self) -> bool {
        let mut text = String::new();
        if !self.programs.iter().any(|line| matches!(line, AutoLine::Other(line) if line.starts_with('#'))) {
            text.push_str("# Programs opened when the desktop is up (/bin/autorun): on|off <command line>\n");
        }
        for line in &self.programs {
            match line {
                AutoLine::Other(line) => text.push_str(line),
                AutoLine::Entry { on, command } => {
                    text.push_str(if *on { "on " } else { "off " });
                    text.push_str(command);
                }
            }
            text.push('\n');
        }
        self.write_file(AUTOSTART, &text)
    }

    fn save_services(&mut self) -> bool {
        let mut text = self.service_lines.join("\n");
        text.push('\n');
        let ok = self.write_file(SERVICES, &text);
        if ok {
            self.services_default = false;
        }
        ok
    }

    fn write_file(&mut self, path: &str, text: &str) -> bool {
        let fd = open_write(path, true, true);
        if fd < 0 {
            println(&format!("startup: save FAILED {path}: {fd}"));
            self.note = Some(format!("Не удалось записать {path}: {} (код {fd})", error_text(fd)));
            return false;
        }
        let got = write(fd, text.as_bytes());
        close(fd);
        if got < 0 || got as usize != text.len() {
            println(&format!("startup: save FAILED {path}: {got}"));
            self.note = Some(format!("Запись {path} оборвалась (код {got})"));
            return false;
        }
        true
    }

    fn row_at(&self, ctx: Ctx, plan: &Plan, y: i32) -> Option<usize> {
        if y < plan.list.y {
            return None;
        }
        let index = self.top.get() + ((y - plan.list.y) as u32 / ctx.px(ROW_H)) as usize;
        (index < self.count()).then_some(index)
    }

    fn picker_rect(ctx: Ctx, area: Rect, items: usize) -> Rect {
        let w = ctx.px(380).min(area.w.saturating_sub(ctx.px(40)));
        let row = ctx.px(36);
        let head = ctx.px(52);
        let foot = ctx.px(40);
        let h = (head + row * items as u32 + foot).min(area.h.saturating_sub(ctx.px(40)));
        Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h)
    }

    // -- Рисование ---------------------------------------------------------

    fn draw_banner(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        paint::card(ctx, s, plan.banner);
        let inner = ctx.on(ctx.flat(p.card));
        let g = ctx.px(20);
        let x = plan.banner.x + ctx.px(14) as i32;
        let icon = if self.section == Section::Programs { Fi::Play } else { Fi::Server };
        iconset::draw(s, icon, x, plan.banner.y + ctx.px(12) as i32, g, p.acc, 255);
        let tx = x + (g + ctx.px(12)) as i32;
        let room = (plan.banner.right() - tx - ctx.px(12) as i32).max(0) as u32;
        let (first, second) = match self.section {
            Section::Programs => (
                String::from("Эти программы открываются сами, когда стол поднялся после загрузки."),
                format!("Список — {AUTOSTART}, его исполняет служба autorun. Выключенная строка остаётся в списке."),
            ),
            Section::Services => (
                String::from("Службы, которые init запускает при загрузке. Изменения — со следующей загрузки."),
                if self.services_default {
                    format!("Сейчас действует эталон образа; первая правка запишет копию в {SERVICES}.")
                } else {
                    format!("Файл {SERVICES}; выключенная служба помечена «#off» и остаётся в файле.")
                },
            ),
        };
        let y = plan.banner.y + ctx.px(10) as i32;
        paint::text_clipped(inner, s, Role::Body, tx, y, room, &first, p.ink);
        paint::text_clipped(inner, s, Role::Caption, tx, y + line_h(ctx, Role::Body) + ctx.px(4) as i32, room, &second, p.ink3);
    }

    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let pad = ctx.px(24) as i32;
        let switch_x = plan.list.x + pad;
        let name_x = switch_x + ctx.px(64) as i32;
        let state_w = ctx.px(130);
        let state_x = plan.list.right() - ctx.px(24) as i32 - state_w as i32;
        let header_y = paint::baseline(ctx, Role::MonoCaps, plan.header);
        paint::caps(ctx, s, switch_x, header_y, "ВКЛ.");
        paint::caps(ctx, s, name_x, header_y, if self.section == Section::Programs { "ПРОГРАММА" } else { "СЛУЖБА" });
        paint::caps(ctx, s, state_x, header_y, "СОСТОЯНИЕ");
        draw::hline(s, plan.header.x + ctx.px(12) as i32, plan.header.bottom() - 1, plan.header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);

        let row_h = ctx.px(ROW_H);
        let visible = ((plan.list.h / row_h) as usize).max(1);
        self.visible.set(visible);
        let count = self.count();
        let mut top = self.top.get().min(count.saturating_sub(visible));
        if self.selected < top {
            top = self.selected;
        } else if self.selected >= top + visible {
            top = self.selected + 1 - visible;
        }
        self.top.set(top);
        let rows: Vec<(String, String, bool, bool)> = match self.section {
            Section::Programs => self
                .entries()
                .into_iter()
                .filter_map(|index| match &self.programs[index] {
                    AutoLine::Entry { on, command } => Some((caption_of(command), command.clone(), *on, true)),
                    AutoLine::Other(_) => None,
                })
                .collect(),
            Section::Services => self
                .services
                .iter()
                .map(|service| {
                    let who = service.uid.as_ref().map_or_else(|| String::from("как init"), |uid| format!("uid {uid}"));
                    (service.name.clone(), format!("{} · {who}", service.exec), service.on, service.name != "logger")
                })
                .collect(),
        };
        for (index, (title, detail, on, enabled)) in rows.iter().enumerate().skip(top).take(visible) {
            let at = (index - top) as u32;
            let row = Rect::new(plan.list.x + ctx.px(8) as i32, plan.list.y + (at * row_h) as i32, plan.list.w.saturating_sub(ctx.px(16)), row_h.saturating_sub(ctx.px(2)));
            let chosen = index == self.selected;
            if chosen {
                paint::row(ctx, s, row, RowState::Selected);
            }
            let switch = plan.switch(ctx, row);
            draw_switch(ctx, s, switch, *on, *enabled && self.root);
            let room = (state_x - name_x - ctx.px(12) as i32).max(0) as u32;
            let ty = row.y + ctx.px(4) as i32;
            paint::text_clipped(ctx, s, Role::Body, name_x, ty, room, title, p.ink);
            paint::text_clipped(ctx, s, Role::MonoSmall, name_x, ty + line_h(ctx, Role::Body) + ctx.px(1) as i32, room, detail, p.ink4);
            let (label, tone) = match (on, enabled) {
                (true, false) => ("ОБЯЗАТЕЛЬНА", Tone::Accent),
                (true, true) => ("ВКЛЮЧЕНА", Tone::Ok),
                (false, _) => ("ОТКЛЮЧЕНА", Tone::Muted),
            };
            let chip_w = paint::chip_width(ctx, label).min(state_w);
            let chip = Rect::new(state_x, row.y + (row.h as i32 - ctx.px(22) as i32) / 2, chip_w, ctx.px(22));
            paint::chip(ctx, s, chip, label, tone);
        }
        if rows.is_empty() {
            let text = match self.section {
                Section::Programs => "Список пуст. «Добавить» (клавиша A) — выбрать программу",
                Section::Services => "Описаний служб не найдено",
            };
            paint::text(ctx, s, Role::Body, name_x, plan.list.y + ctx.px(14) as i32, text, p.ink4);
        }
        if count > visible {
            let track = Rect::new(plan.list.right() - ctx.px(8) as i32, plan.list.y, ctx.px(4), (visible as u32 * row_h).min(plan.list.h));
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / count) as u32).max(ctx.px(20)).min(track.h);
            let max_top = count - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
    }

    fn draw_picker(&self, s: &mut Surface, ctx: Ctx, area: Rect, picker: &Picker) {
        let p = ctx.palette;
        draw::blend_rect(s, area, p.ink, 40);
        let rect = Self::picker_rect(ctx, area, picker.items.len());
        let card = kit::card(ctx, s, rect);
        let pad = ctx.px(16) as i32;
        paint::text(card, s, Role::Title, rect.x + pad, rect.y + ctx.px(14) as i32, "Добавить в автозагрузку", p.ink);
        let row_h = ctx.px(36);
        let list_y = rect.y + ctx.px(52) as i32;
        let g = ctx.px(16);
        let fits = ((rect.h.saturating_sub(ctx.px(92))) / row_h) as usize;
        let first = picker.selected.saturating_sub(fits.saturating_sub(1));
        for (offset, &index) in picker.items.iter().enumerate().skip(first).take(fits.max(1)) {
            let (name, caption) = CANDIDATES[index];
            let row = Rect::new(rect.x + ctx.px(8) as i32, list_y + ((offset - first) as u32 * row_h) as i32, rect.w.saturating_sub(ctx.px(16)), row_h.saturating_sub(ctx.px(2)));
            let on = offset == picker.selected;
            if on {
                paint::row(card, s, row, RowState::Selected);
            }
            iconset::draw(s, Fi::Apps, row.x + ctx.px(10) as i32, row.y + (row.h as i32 - g as i32) / 2, g, if on { p.acc_ink } else { p.ink3 }, 255);
            let tx = row.x + ctx.px(36) as i32;
            paint::text(card, s, Role::Body, tx, paint::baseline(card, Role::Body, row), caption, p.ink);
            paint::text_right(card, s, Role::MonoSmall, row.right() - ctx.px(10) as i32, paint::baseline(card, Role::MonoSmall, row), &format!("/bin/{name}"), p.ink4);
        }
        let hint = "Enter — добавить · Esc — отмена";
        paint::text(card, s, Role::Caption, rect.x + pad, rect.bottom() - ctx.px(28) as i32, hint, p.ink4);
    }
}

impl App for Startup {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        plan.tabs.draw(bar, s, &["Программы при входе", "Службы системы"], usize::from(self.section == Section::Services));
        let programs = self.section == Section::Programs;
        tool_button(bar, s, plan.add, Fi::Plus, "Добавить", self.root && programs);
        tool_button(bar, s, plan.remove, Fi::Trash, "Убрать", self.root && programs && self.count() > 0);
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить", true);
        self.draw_banner(s, ctx, &plan);
        self.draw_list(s, ctx, &plan);
        let on = match self.section {
            Section::Programs => self.programs.iter().filter(|line| matches!(line, AutoLine::Entry { on: true, .. })).count(),
            Section::Services => self.services.iter().filter(|service| service.on).count(),
        };
        let status = self.note.clone().unwrap_or_else(|| {
            format!(
                "Всего: {} · включено: {on} · {} · Пробел — вкл./выкл., A — добавить, Del — убрать, 1/2 — списки",
                self.count(),
                if self.root { "изменения доступны" } else { "только просмотр: нужен root" }
            )
        });
        kit::status_bar(ctx, s, plan.status, &status);
        if let Some(picker) = &self.picker {
            self.draw_picker(s, ctx, area, picker);
        }
    }

    fn key(&mut self, code: u32) -> bool {
        if let Some(picker) = &mut self.picker {
            match code {
                WIN_KEY_UP => picker.selected = picker.selected.saturating_sub(1),
                WIN_KEY_DOWN => picker.selected = (picker.selected + 1).min(picker.items.len().saturating_sub(1)),
                ENTER => self.add_picked(),
                ESCAPE => self.picker = None,
                _ => return false,
            }
            return true;
        }
        match code {
            code if code == u32::from('1') => self.set_section(Section::Programs),
            code if code == u32::from('2') => self.set_section(Section::Services),
            WIN_KEY_UP => self.selected = self.selected.saturating_sub(1),
            WIN_KEY_DOWN => self.selected = (self.selected + 1).min(self.count().saturating_sub(1)),
            SPACE | ENTER => self.toggle(),
            WIN_KEY_DELETE => self.remove(),
            code if code == u32::from('a') || code == u32::from('A') => self.open_picker(),
            code if code == u32::from('r') || code == u32::from('R') || code == F5 => {
                self.reload();
                self.note = None;
            }
            _ => return false,
        }
        true
    }

    fn quits_on_q(&self) -> bool {
        self.picker.is_none()
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if let Some(picker) = &self.picker {
            let rect = Self::picker_rect(ctx, area, picker.items.len());
            if !rect.contains(x, y) {
                self.picker = None;
                return true;
            }
            let row_h = ctx.px(36) as i32;
            let list_y = rect.y + ctx.px(52) as i32;
            let fits = ((rect.h.saturating_sub(ctx.px(92))) / row_h as u32) as usize;
            let first = picker.selected.saturating_sub(fits.saturating_sub(1));
            if y >= list_y {
                let index = first + ((y - list_y) / row_h) as usize;
                if index < picker.items.len() && index < first + fits.max(1) {
                    if let Some(picker) = &mut self.picker {
                        picker.selected = index;
                    }
                    self.add_picked();
                }
            }
            return true;
        }
        let plan = Plan::new(ctx, area);
        if let Some(index) = plan.tabs.hit(x, y) {
            self.set_section(if index == 0 { Section::Programs } else { Section::Services });
        } else if plan.add.contains(x, y) {
            self.open_picker();
        } else if plan.remove.contains(x, y) {
            self.remove();
        } else if plan.refresh.contains(x, y) {
            self.reload();
            self.note = None;
        } else if plan.list.contains(x, y) {
            let Some(index) = self.row_at(ctx, &plan, y) else { return false };
            self.selected = index;
            let at = (index - self.top.get()) as u32;
            let row = Rect::new(plan.list.x + ctx.px(8) as i32, plan.list.y + (at * ctx.px(ROW_H)) as i32, plan.list.w.saturating_sub(ctx.px(16)), ctx.px(ROW_H));
            if plan.switch(ctx, row).contains(x, y) {
                self.toggle();
            }
        } else {
            return false;
        }
        true
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        let count = self.count();
        let visible = self.visible.get().max(1);
        let max_top = count.saturating_sub(visible);
        let top = if clicks > 0 { self.top.get().saturating_sub(1) } else { (self.top.get() + 1).min(max_top) };
        self.top.set(top);
        self.selected = self.selected.clamp(top, (top + visible).saturating_sub(1).min(count.saturating_sub(1)).max(top));
        true
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Startup Apps\x1fАвтозагрузка"))
    }
}

// ---------------------------------------------------------------------------
// Мелочи
// ---------------------------------------------------------------------------

fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// Переключатель: дорожка и бегунок.
fn draw_switch(ctx: Ctx, s: &mut Surface, rect: Rect, on: bool, enabled: bool) {
    let p = ctx.palette;
    let r = rect.h / 2;
    if on {
        draw::rounded(s, rect, r, p.acc, if enabled { 255 } else { 120 });
    } else {
        draw::rounded(s, rect, r, ctx.flat(p.sunk), 255);
        draw::rounded_stroke(s, rect, r, p.ink4, if enabled { 255 } else { 120 });
    }
    let knob = rect.h.saturating_sub(ctx.px(8));
    let kx = if on { rect.right() - ctx.px(4) as i32 - knob as i32 } else { rect.x + ctx.px(4) as i32 };
    let color = if on { p.acc_ink } else { p.ink3 };
    draw::rounded(s, Rect::new(kx, rect.y + ctx.px(4) as i32, knob, knob), knob / 2, color, if enabled { 255 } else { 140 });
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str, enabled: bool) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    let ink = if enabled { p.ink } else { p.ink5 };
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, if enabled { p.ink2 } else { p.ink5 }, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, ink);
}

/// Подпись программы по команде: «Блокнот» для `/bin/notepad`.
fn caption_of(command: &str) -> String {
    let program = command.split_whitespace().next().unwrap_or(command);
    let name = program.rsplit('/').next().unwrap_or(program);
    CANDIDATES.iter().find(|(known, _)| *known == name).map_or_else(|| name.to_string(), |(_, caption)| (*caption).to_string())
}

fn error_text(code: i64) -> &'static str {
    match code {
        user_abi::ERR_PERMISSION => "нет прав",
        user_abi::ERR_NOT_FOUND => "нет папки /etc",
        user_abi::ERR_NO_SPACE => "нет места",
        _ => "том только для чтения или ошибка записи",
    }
}

fn read_text(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = vec![0u8; FILE_LIMIT];
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    buffer.truncate(got);
    Some(String::from_utf8_lossy(&buffer).to_string())
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (860, 560));
    app::run(&SPEC, &info, size, |_, _| Startup::new())
}
