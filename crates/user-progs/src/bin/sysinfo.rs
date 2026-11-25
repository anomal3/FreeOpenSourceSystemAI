//! «Сведения о системе» (фаза П1, 20.11) — как msinfo32 в Windows.
//!
//! # Зачем
//!
//! В разборах Роман присылает одно и то же: что за машина, какая версия, какие
//! устройства и чем они обслуживаются, что смонтировано, какие службы живы.
//! Здесь это одно окно: слева разделы, справа таблица «элемент — значение»,
//! кнопка «Экспорт» кладёт всё текстом в файл (`~/Desktop/sysinfo.txt`, или в
//! домашнюю папку), «Копировать» — раздел в буфер обмена.
//!
//! # Откуда сведения
//!
//! `SYS_SYSINFO` (память, экран, процессоры), `SYS_DEVICES` (перепись шин),
//! `SYS_MOUNTS`, `SYS_TASKS`, `SYS_NETINFO`, `/etc/services`, `/etc/hostname`
//! и — у root — команды оболочки ядра (`date`, `slots`, `mem`, строка сборки
//! из журнала). Чего обычному пользователю не видно, то так и названо.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, KIND_DIRECTORY, SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_UP, close, kernel_command, open, open_write, println, read, stat, write,
};

const SPEC: Spec = Spec { name: "sysinfo", title: "System Information", period_ms: 5_000 };

const CTRL_C: u32 = 0x03;
const CTRL_S: u32 = 0x13;
const TAB: u32 = '\t' as u32;
const ROW_H: u32 = 30;
const LEFT_W: u32 = 230;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Summary,
    Hardware,
    Devices,
    Storage,
    Network,
    Software,
    Services,
}

impl Section {
    const ALL: [Self; 7] = [Self::Summary, Self::Hardware, Self::Devices, Self::Storage, Self::Network, Self::Software, Self::Services];

    const fn title(self) -> &'static str {
        match self {
            Self::Summary => "Сводка о системе",
            Self::Hardware => "Процессор и память",
            Self::Devices => "Устройства",
            Self::Storage => "Хранилище",
            Self::Network => "Сеть",
            Self::Software => "Программная среда",
            Self::Services => "Службы",
        }
    }

    const fn icon(self) -> Fi {
        match self {
            Self::Summary => Fi::Info,
            Self::Hardware => Fi::Cpu,
            Self::Devices => Fi::Layers,
            Self::Storage => Fi::Drive,
            Self::Network => Fi::Globe,
            Self::Software => Fi::Apps,
            Self::Services => Fi::Server,
        }
    }
}

/// Строка таблицы: подпись и значение; пустая подпись — заголовок группы.
type Row = (String, String);

fn group(title: &str) -> Row {
    (String::new(), title.to_string())
}

fn item(label: &str, value: impl Into<String>) -> Row {
    (label.to_string(), value.into())
}

struct Plan {
    toolbar: Rect,
    refresh: Rect,
    export: Rect,
    copy: Rect,
    left: Rect,
    table: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let refresh = Rect::new(frame.toolbar.x + pad, by, face.width("Обновить") + ctx.px(46), bh);
        let export = Rect::new(refresh.right() + ctx.px(6) as i32, by, face.width("Экспорт в файл") + ctx.px(46), bh);
        let copy = Rect::new(export.right() + ctx.px(6) as i32, by, face.width("Копировать раздел") + ctx.px(46), bh);
        let left_w = ctx.px(LEFT_W).min(frame.body.w / 3);
        let left = Rect::new(frame.body.x, frame.body.y, left_w, frame.body.h);
        let table = Rect::new(left.right(), frame.body.y, frame.body.w.saturating_sub(left_w), frame.body.h);
        Self { toolbar: frame.toolbar, refresh, export, copy, left, table, status: frame.status }
    }
}

struct Info {
    section: Section,
    rows: Vec<Row>,
    top: usize,
    note: Option<String>,
    visible: core::cell::Cell<usize>,
}

impl Info {
    fn new() -> Self {
        let mut info = Self { section: Section::Summary, rows: Vec::new(), top: 0, note: None, visible: core::cell::Cell::new(20) };
        info.reload();
        info
    }

    fn reload(&mut self) {
        self.rows = collect(self.section);
    }

    fn set_section(&mut self, section: Section) {
        self.section = section;
        self.top = 0;
        self.reload();
        println(&format!("sysinfo: section '{}', {} row(s)", section.title(), self.rows.len()));
    }

    fn scroll(&mut self, delta: isize) {
        let max_top = self.rows.len().saturating_sub(self.visible.get().max(1));
        self.top = (self.top as isize + delta).clamp(0, max_top as isize) as usize;
    }

    fn export(&mut self) {
        let text = full_report();
        let path = format!("{}/sysinfo.txt", report_dir());
        match save(&path, &text) {
            Ok(()) => {
                println(&format!("sysinfo: export to {path}: ok, {} line(s)", text.lines().count()));
                self.note = Some(format!("Сведения сохранены: {path}"));
            }
            Err(code) => {
                println(&format!("sysinfo: export to {path}: FAILED {code} (no writable folder?)"));
                self.note = Some(format!("Не удалось записать {path} (код {code})"));
            }
        }
    }

    fn copy(&mut self) {
        let text = section_text(self.section, &self.rows);
        let ok = user_progs::clipboard_set(&text);
        self.note = Some(String::from(if ok { "Раздел скопирован в буфер обмена" } else { "Буфер обмена не ответил" }));
    }

    fn section_rows(ctx: Ctx, plan: &Plan) -> Vec<(Section, Rect)> {
        let row_h = ctx.px(34);
        let x = plan.left.x + ctx.px(8) as i32;
        let w = plan.left.w.saturating_sub(ctx.px(16));
        Section::ALL.iter().enumerate().map(|(index, section)| (*section, Rect::new(x, plan.left.y + ctx.px(10) as i32 + (index as u32 * row_h) as i32, w, row_h))).collect()
    }
}

impl App for Info {
    /// Smallest window (24.11, WM SDK): the 230-point section list and the facts table.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(600, 400)
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & user_abi::WIN_MOD_CTRL != 0;
        let page = self.visible.get().max(1) as isize;
        let index = Section::ALL.iter().position(|section| *section == self.section).unwrap_or(0);
        match code {
            CTRL_C => self.copy(),
            CTRL_S => self.export(),
            _ if ctrl && latin == 'c' as u32 => self.copy(),
            _ if ctrl && latin == 's' as u32 => self.export(),
            WIN_KEY_UP => self.scroll(-1),
            WIN_KEY_DOWN => self.scroll(1),
            WIN_KEY_PAGE_UP => self.scroll(-page),
            WIN_KEY_PAGE_DOWN => self.scroll(page),
            WIN_KEY_HOME => self.top = 0,
            WIN_KEY_END => self.scroll(isize::MAX / 2),
            TAB => self.set_section(Section::ALL[(index + 1) % Section::ALL.len()]),
            code if code == 'r' as u32 => self.reload(),
            code if code == 'e' as u32 => self.export(),
            code if ('1' as u32..='7' as u32).contains(&code) => self.set_section(Section::ALL[(code - '1' as u32) as usize]),
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.refresh.contains(x, y) {
            self.reload();
        } else if plan.export.contains(x, y) {
            self.export();
        } else if plan.copy.contains(x, y) {
            self.copy();
        } else if let Some((section, _)) = Self::section_rows(ctx, &plan).into_iter().find(|(_, rect)| rect.contains(x, y)) {
            self.set_section(section);
        } else {
            return false;
        }
        true
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        self.scroll(if clicks > 0 { -3 } else { 3 });
        true
    }

    fn title(&self) -> Option<String> {
        Some(String::from("System Information\x1fСведения о системе"))
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        if matches!(self.section, Section::Summary | Section::Hardware | Section::Services) {
            self.reload();
            return true;
        }
        false
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить");
        tool_button(bar, s, plan.export, Fi::Download, "Экспорт в файл");
        tool_button(bar, s, plan.copy, Fi::Copy, "Копировать раздел");

        // Разделы.
        draw::vline(s, plan.left.right() - 1, plan.left.y, plan.left.h, p.line.color, p.line.alpha);
        let g = ctx.px(16);
        for (section, rect) in Self::section_rows(ctx, &plan) {
            let on = section == self.section;
            paint::row(ctx, s, rect, if on { paint::RowState::Selected } else { paint::RowState::Idle });
            iconset::draw(s, section.icon(), rect.x + ctx.px(10) as i32, rect.y + (rect.h - g) as i32 / 2, g, if on { p.acc_ink } else { p.ink3 }, 255);
            let tx = rect.x + ctx.px(36) as i32;
            paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), (rect.right() - tx).max(0) as u32, section.title(), if on { p.ink } else { p.ink2 });
        }

        // Таблица.
        let pad = ctx.px(20) as i32;
        let head = Rect::new(plan.table.x + pad, plan.table.y + ctx.px(14) as i32, plan.table.w.saturating_sub(pad as u32 * 2), u32::from(ctx.face(Role::Heading).line));
        paint::text_clipped(ctx, s, Role::Heading, head.x, head.y, head.w, self.section.title(), p.ink);
        let header = Rect::new(plan.table.x, head.bottom() + ctx.px(10) as i32, plan.table.w, u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(12));
        let label_w = (plan.table.w * 2 / 5).min(ctx.px(280));
        let label_x = plan.table.x + pad;
        let value_x = label_x + label_w as i32;
        let cy = paint::baseline(ctx, Role::MonoCaps, header);
        paint::caps(ctx, s, label_x, cy, "ЭЛЕМЕНТ");
        paint::caps(ctx, s, value_x, cy, "ЗНАЧЕНИЕ");
        draw::hline(s, plan.table.x + ctx.px(12) as i32, header.bottom() - 1, plan.table.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);
        let list = Rect::new(plan.table.x, header.bottom() + ctx.px(4) as i32, plan.table.w, (plan.table.bottom() - header.bottom() - ctx.px(4) as i32).max(0) as u32);
        let row_h = ctx.px(ROW_H);
        let visible = (list.h / row_h) as usize;
        self.visible.set(visible.max(1));
        let value_w = (plan.table.right() - value_x - pad).max(0) as u32;
        for (offset, (label, value)) in self.rows.iter().skip(self.top).take(visible).enumerate() {
            let row = Rect::new(plan.table.x + ctx.px(8) as i32, list.y + (offset as u32 * row_h) as i32, plan.table.w.saturating_sub(ctx.px(16)), row_h);
            let y = paint::baseline(ctx, Role::Body, row);
            if label.is_empty() {
                paint::text_clipped(ctx, s, Role::Strong, label_x, paint::baseline(ctx, Role::Strong, row), plan.table.w, value, p.acc_ink);
                continue;
            }
            if offset % 2 == 1 {
                draw::rounded(s, row, ctx.px(4), p.hover1.color, p.hover1.alpha / 2);
            }
            paint::text_clipped(ctx, s, Role::Body, label_x, y, label_w.saturating_sub(ctx.px(10)), label, p.ink3);
            paint::text_clipped(ctx, s, Role::Body, value_x, y, value_w, value, p.ink);
        }
        if self.rows.len() > visible && visible > 0 {
            let track = Rect::new(plan.table.right() - ctx.px(8) as i32, list.y, ctx.px(4), list.h);
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / self.rows.len()) as u32).max(ctx.px(24)).min(track.h);
            let max_top = self.rows.len() - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * self.top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
        let status = self.note.clone().unwrap_or_else(|| format!("{} · строк: {} · 1–7 — разделы, E — экспорт, Ctrl+C — копировать", self.section.title(), self.rows.len()));
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

// ---------------------------------------------------------------------------
// Сбор сведений
// ---------------------------------------------------------------------------

fn collect(section: Section) -> Vec<Row> {
    let info = user_progs::sysinfo();
    let mut rows = Vec::new();
    match section {
        Section::Summary => {
            rows.push(item("Имя ОС", "FossAi"));
            rows.push(item("Версия", version().unwrap_or_else(|| String::from("неизвестна"))));
            rows.push(item("Архитектура", if cfg!(target_arch = "aarch64") { "AArch64" } else { "x86-64" }));
            rows.push(item("Имя компьютера", read_text("/etc/hostname", 256).map_or_else(|| String::from("freeos"), |text| text.trim().to_string())));
            rows.push(item("Пользователь", user_name()));
            if let Some(info) = &info {
                rows.push(item("Процессоры", format!("{}", info.cpus)));
                // SysInfo — в байтах (договор user_abi), а не в кадрах.
                rows.push(item("Оперативная память", size_text(info.frames_total)));
                rows.push(item("Свободно памяти", size_text(info.frames_free)));
                rows.push(item("Экран", format!("{} × {}", info.screen_w, info.screen_h)));
                rows.push(item("Время работы", uptime_text(info.uptime_ms)));
            }
            for line in kernel_lines("date") {
                rows.push(item("Часы", line));
            }
            for line in kernel_lines("slots") {
                rows.push(item("Слоты системы", line));
            }
        }
        Section::Hardware => {
            if let Some(info) = &info {
                rows.push(group("Процессор"));
                rows.push(item("Логических процессоров", format!("{}", info.cpus)));
                rows.push(item("Тиков таймера", format!("{}", info.ticks)));
                rows.push(group("Память"));
                rows.push(item("Всего", format!("{} ({} кадров по 4 КиБ)", size_text(info.frames_total), info.frames_total / 4096)));
                rows.push(item("Свободно", size_text(info.frames_free)));
                rows.push(item("Куча ядра", format!("{} КиБ, свободно {} КиБ", info.heap_size / 1024, info.heap_free / 1024)));
                rows.push(item("Окно DMA", format!("{} КиБ, занято {} КиБ", info.dma_total / 1024, info.dma_used / 1024)));
                rows.push(group("Графика и ввод"));
                rows.push(item("Экран", format!("{} × {}", info.screen_w, info.screen_h)));
                rows.push(item("Окон", format!("{}", info.windows)));
                rows.push(item("Кадров составлено", format!("{}", info.frames_composed)));
                rows.push(item("Нажатий клавиш", format!("{} (потеряно {})", info.keys_posted, info.keys_dropped)));
            }
            let mem = kernel_lines("mem");
            if !mem.is_empty() {
                rows.push(group("Ядро: mem"));
                for line in mem {
                    let (label, value) = split_label(&line);
                    rows.push(item(&label, value));
                }
            }
        }
        Section::Devices => {
            let text = call_text(|buffer| user_progs::devices(buffer), 16 * 1024);
            for bus in ["sys", "pci", "usb", "disk"] {
                let lines: Vec<&str> = text.lines().filter(|line| line.split('\t').next() == Some(bus)).collect();
                if lines.is_empty() {
                    continue;
                }
                rows.push(group(match bus {
                    "sys" => "Системные",
                    "pci" => "Шина PCI",
                    "usb" => "USB",
                    _ => "Диски",
                }));
                for line in lines {
                    let fields: Vec<&str> = line.split('\t').collect();
                    let place = fields.get(1).copied().unwrap_or("");
                    let id = fields.get(2).copied().unwrap_or("");
                    let what = fields.get(3).copied().unwrap_or("");
                    let driver = fields.get(4).copied().unwrap_or("-");
                    let state = fields.get(5).copied().unwrap_or("");
                    rows.push(item(&format!("{place} {what}"), format!("{id} · драйвер {driver} · {state}")));
                }
            }
            if rows.is_empty() {
                rows.push(item("Перепись", "ядро не ответило"));
            }
        }
        Section::Storage => {
            let text = call_text(|buffer| user_progs::mounts(buffer), 4096);
            for line in text.lines() {
                let fields: Vec<&str> = line.split('\t').collect();
                let point = fields.first().copied().unwrap_or("?");
                let kind = fields.get(1).copied().unwrap_or("?");
                let space = match (fields.get(2).and_then(|t| t.parse::<u64>().ok()), fields.get(3).and_then(|t| t.parse::<u64>().ok())) {
                    (Some(total), Some(free)) => format!("{} · {} свободно из {}", kind, size_text(free), size_text(total)),
                    _ => kind.to_string(),
                };
                rows.push(item(point, space));
            }
            if rows.is_empty() {
                rows.push(item("Тома", "нет данных"));
            }
        }
        Section::Network => {
            let mut net = user_progs::NetInfo::default();
            if user_progs::netinfo(&mut net) >= 0 && net.present != 0 {
                let mac: Vec<String> = net.mac.iter().map(|b| format!("{b:02x}")).collect();
                rows.push(item("Сетевая карта", "есть"));
                rows.push(item("MAC-адрес", mac.join(":")));
                rows.push(item("Адрес IPv4", ip_text(net.address)));
                rows.push(item("Маска", ip_text(net.netmask)));
                rows.push(item("Шлюз", ip_text(net.gateway)));
                rows.push(item("DNS", ip_text(net.dns)));
            } else {
                rows.push(item("Сеть", "сетевой карты нет или стек не поднят"));
            }
        }
        Section::Software => {
            let tasks = call_text(|buffer| user_progs::tasks(buffer), 64 * 1024);
            let mut programs = 0;
            let mut services = 0;
            let mut kernel = 0;
            for line in tasks.lines() {
                match line.split('\t').nth(4) {
                    Some("program") => programs += 1,
                    Some("service") => services += 1,
                    Some(_) => kernel += 1,
                    None => {}
                }
            }
            rows.push(group("Задачи"));
            rows.push(item("Программы", format!("{programs}")));
            rows.push(item("Службы", format!("{services}")));
            rows.push(item("Задачи ядра", format!("{kernel}")));
            rows.push(group("Программы в /bin"));
            let mut names = list_names("/bin");
            names.sort();
            rows.push(item("Всего", format!("{}", names.len())));
            for chunk in names.chunks(6) {
                rows.push(item("", chunk.join("  ")));
            }
            rows.push(group("Пакеты (/var/lib/pkg)"));
            let mut packages = list_names("/var/lib/pkg");
            packages.sort();
            if packages.is_empty() {
                rows.push(item("Установлено", "нет"));
            }
            for name in packages {
                rows.push(item(name.trim_end_matches(".pkg"), "установлен"));
            }
            // Подпись-пустышка у списка /bin выглядела бы как заголовок группы.
            for row in &mut rows {
                if row.0.is_empty() && row.1.contains("  ") {
                    row.0 = String::from(" ");
                }
            }
        }
        Section::Services => {
            let tasks = call_text(|buffer| user_progs::tasks(buffer), 64 * 1024);
            let text = read_text("/etc/services", 8 * 1024).or_else(|| read_text("/usr/share/defaults/etc/services", 8 * 1024)).unwrap_or_default();
            for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
                let mut fields = line.split_whitespace();
                let (Some(name), Some(exec)) = (fields.next(), fields.next()) else { continue };
                let running = tasks.lines().find(|task| {
                    let fields: Vec<&str> = task.split('\t').collect();
                    fields.get(6) == Some(&exec) && fields.get(2) != Some(&"finished")
                });
                let state = match running.and_then(|task| task.split('\t').next()) {
                    Some(id) => format!("работает · задача {id} · {exec}"),
                    None => format!("остановлена · {exec}"),
                };
                rows.push(item(name, state));
            }
            if rows.is_empty() {
                rows.push(item("Службы", "/etc/services пуст или не найден"));
            }
        }
    }
    rows
}

fn full_report() -> String {
    let mut out = String::from("FossAi: сведения о системе\n==========================\n");
    for section in Section::ALL {
        out.push('\n');
        out.push_str(&section_text(section, &collect(section)));
    }
    out
}

fn section_text(section: Section, rows: &[Row]) -> String {
    let mut out = format!("[{}]\n", section.title());
    for (label, value) in rows {
        if label.is_empty() {
            out.push_str(&format!("-- {value} --\n"));
        } else {
            out.push_str(&format!("{label:<28} {value}\n"));
        }
    }
    out
}

/// Версия системы: `0.8.970 x86_64` (20.11 — команда ядра `version`, она
/// открыта и не root; раньше — строка `build` из журнала, которой в кольце
/// журнала к этому времени уже не было). Запас — `/os-release` образа.
fn version() -> Option<String> {
    // Архитектура — своей строкой ниже, здесь только номер.
    let from_kernel = kernel_lines("version")
        .into_iter()
        .find_map(|line| line.strip_prefix("FossAi ").and_then(|rest| rest.split_whitespace().next()).map(String::from));
    from_kernel.or_else(|| {
        read_text("/os-release", 4096)?.lines().find_map(|line| line.trim().strip_prefix("version=").map(|value| value.trim().to_string()))
    })
}

/// Строки команды оболочки ядра; пусто, если она недоступна (не root).
fn kernel_lines(line: &str) -> Vec<String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = kernel_command(line, &mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    String::from_utf8_lossy(&buffer[..got as usize]).lines().map(|line| line.trim().to_string()).filter(|line| !line.is_empty()).collect()
}

fn call_text(call: impl FnOnce(&mut [u8]) -> i64, size: usize) -> String {
    let mut buffer = vec![0u8; size];
    let got = call(&mut buffer);
    if got <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buffer[..(got as usize).min(size)]).to_string()
}

/// `frames   12 of 34 used` → («frames», «12 of 34 used»).
fn split_label(line: &str) -> (String, String) {
    let line = line.trim();
    match line.split_once(char::is_whitespace) {
        Some((label, rest)) => (label.trim_end_matches(':').to_string(), rest.trim().to_string()),
        None => (line.to_string(), String::new()),
    }
}

fn user_name() -> String {
    let me = user_progs::uid();
    let name = read_text("/etc/passwd", 16 * 1024).and_then(|text| {
        text.lines().find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid: u32 = fields.next()?.parse().ok()?;
            (uid == me).then(|| name.to_string())
        })
    });
    match name {
        Some(name) => format!("{name} (uid {me})"),
        None if me == 0 => String::from("root (uid 0)"),
        None => format!("uid {me}"),
    }
}

fn ip_text(address: u32) -> String {
    if address == 0 {
        return String::from("не задан");
    }
    let b = address.to_be_bytes();
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}

fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{} ч {} мин {} с", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

fn size_text(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        format!("{},{} ГиБ", bytes / GIB, bytes % GIB * 10 / GIB)
    } else {
        format!("{} МиБ", bytes / (1024 * 1024))
    }
}

fn list_names(path: &str) -> Vec<String> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = user_progs::Dirent::default();
    while user_progs::readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if name != "." && name != ".." && entry.kind != KIND_DIRECTORY {
            out.push(name);
        }
    }
    close(fd);
    out
}

fn read_text(path: &str, limit: usize) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = vec![0u8; limit];
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

fn save(path: &str, text: &str) -> Result<(), i64> {
    let fd = open_write(path, true, true);
    if fd < 0 {
        return Err(fd);
    }
    let bytes = text.as_bytes();
    let mut done = 0usize;
    while done < bytes.len() {
        let wrote = write(fd, &bytes[done..]);
        if wrote <= 0 {
            close(fd);
            return Err(wrote);
        }
        done += wrote as usize;
    }
    close(fd);
    Ok(())
}

fn report_dir() -> String {
    let me = user_progs::uid();
    let home = if me == 0 {
        String::from("/root")
    } else {
        let mut found = String::from("/home");
        for name in list_dirs("/home") {
            let path = format!("/home/{name}");
            let mut info = user_abi::Stat::default();
            if stat(&path, &mut info) >= 0 && info.uid == me {
                found = path;
                break;
            }
        }
        found
    };
    let desktop = format!("{home}/Desktop");
    let mut info = user_abi::Stat::default();
    if stat(&desktop, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
        return desktop;
    }
    // Живая система без домашней папки (у root на флешке её нет) — /tmp.
    if stat(&home, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
        return home;
    }
    // На живой системе и /tmp может не быть: заводится при нужде.
    let _ = user_progs::mkdir("/tmp", 0o1777);
    String::from("/tmp")
}

fn list_dirs(path: &str) -> Vec<String> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = user_progs::Dirent::default();
    while user_progs::readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if !name.starts_with('.') && entry.kind == KIND_DIRECTORY {
            out.push(name);
        }
    }
    close(fd);
    out
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, p.ink2, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: ядро кладёт `argc` указателей на строки, живущие всю программу.
    let args = unsafe { Args::new(argc, argv) };
    // `sysinfo --export [файл]` — без окна: для стенда и для терминала.
    if args.get(1) == Some("--export") {
        let path = args.get(2).map_or_else(|| format!("{}/sysinfo.txt", report_dir()), String::from);
        let text = full_report();
        match save(&path, &text) {
            Ok(()) => println(&format!("sysinfo: export to {path}: ok, {} line(s)", text.lines().count())),
            Err(code) => println(&format!("sysinfo: export to {path}: FAILED {code}")),
        }
        user_progs::exit(0);
    }
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (900, 620));
    app::run(&SPEC, &info, size, |_, _| Info::new())
}
