//! «Монитор ресурсов» (фаза П3, 20.11): что именно занимает процессор, память,
//! диск и сеть — по ядрам и по процессам, как resmon.exe в Windows.
//!
//! # Чем он не диспетчер задач
//!
//! Диспетчер задач отвечает «что запущено и как это снять», монитор — «куда
//! уходит машина». Здесь нет снятия задач, зато есть то, чего нет там: загрузка
//! **каждого** ядра со своей историей, тома с занятым местом рядом с дисковым
//! обменом и сетевая карта рядом с сетевым.
//!
//! # Откуда числа
//!
//! Те же источники, что у `taskmgr`, без новых вызовов ядра:
//!
//! * `SYS_TASKS` — время на процессоре, страницы памяти, байты с носителем и
//!   по сети за жизнь задачи; скорости — разность двух опросов.
//! * Загрузка ядра — от его холостой задачи: у каждого процессора своя задача
//!   `idle`, номера идут по порядку процессоров. Доля холостого времени за
//!   промежуток, вычтенная из единицы, — это загрузка ядра.
//! * `SYS_SYSINFO` — кадры памяти, куча ядра, память устройств.
//! * `SYS_MOUNTS` — тома и место на них; `SYS_NETINFO` — адрес карты.

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
use mini_ui::{Color, Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    NetInfo, SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_UP, monotonic_ms,
    mounts, netinfo, println, sysinfo, tasks,
};

const SPEC: Spec = Spec { name: "resmon", title: "Resource Monitor", period_ms: 1_000 };

const TASKS_LIMIT: usize = 16 * 1024;
const MOUNTS_LIMIT: usize = 4 * 1024;
/// Точек истории: две минуты при опросе раз в секунду.
const HISTORY: usize = 120;
const ROW_H: u32 = 32;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Program,
    Service,
    Kernel,
}

impl Kind {
    fn icon(self) -> (Fi, Color) {
        match self {
            Kind::Program => (Fi::FileCode, Color::rgb(0x82, 0xCB, 0x92)),
            Kind::Service => (Fi::Server, Color::rgb(0x51, 0xCE, 0xC7)),
            Kind::Kernel => (Fi::Cpu, Color::rgb(0x80, 0xBD, 0xFB)),
        }
    }
}

/// Строка ответа `SYS_TASKS` и то, что из неё посчитано.
#[derive(Clone)]
struct Task {
    id: u32,
    name: String,
    state: String,
    cpu_ms: u64,
    kind: Kind,
    path: String,
    pages: Option<u64>,
    disk_bytes: Option<u64>,
    net_bytes: Option<u64>,
    /// Доля процессора за промежуток, в десятых процента (от всей машины).
    cpu: u32,
    disk_rate: u64,
    net_rate: u64,
}

impl Task {
    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let id = fields.next()?.parse().ok()?;
        let name = fields.next()?.to_string();
        let state = fields.next()?.to_string();
        let cpu_ms = fields.next()?.parse().ok()?;
        let kind = match fields.next()? {
            "program" => Kind::Program,
            "service" => Kind::Service,
            _ => Kind::Kernel,
        };
        let _uid = fields.next()?;
        let path = fields.next().unwrap_or("").to_string();
        let _switches = fields.next();
        let pages = fields.next().and_then(|text| text.parse().ok());
        let disk_bytes = fields.next().and_then(|text| text.parse().ok());
        let net_bytes = fields.next().and_then(|text| text.parse().ok());
        Some(Self { id, name, state, cpu_ms, kind, path, pages, disk_bytes, net_bytes, cpu: 0, disk_rate: 0, net_rate: 0 })
    }

    fn label(&self) -> &str {
        if self.path.is_empty() { &self.name } else { &self.path }
    }

    fn is_idle(&self) -> bool {
        self.kind == Kind::Kernel && self.name.starts_with("idle")
    }

    fn state_title(&self) -> &'static str {
        match self.state.as_str() {
            "running" => "работает",
            "ready" => "готова",
            "blocked" => "ожидает",
            _ => "—",
        }
    }
}

/// Том из `SYS_MOUNTS`.
struct Volume {
    point: String,
    fs: String,
    label: String,
    total: Option<u64>,
    free: Option<u64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Cpu,
    Memory,
    Disk,
    Network,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::Overview, Tab::Cpu, Tab::Memory, Tab::Disk, Tab::Network];

    fn title(self) -> &'static str {
        match self {
            Tab::Overview => "Обзор",
            Tab::Cpu => "Процессор",
            Tab::Memory => "Память",
            Tab::Disk => "Диск",
            Tab::Network => "Сеть",
        }
    }

    fn log_name(self) -> &'static str {
        match self {
            Tab::Overview => "overview",
            Tab::Cpu => "cpu",
            Tab::Memory => "memory",
            Tab::Disk => "disk",
            Tab::Network => "network",
        }
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    toolbar: Rect,
    tabs: kit::Tabs,
    refresh: Rect,
    body: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let face = ctx.face(Role::Caption);
        let widths: Vec<u32> = Tab::ALL.iter().map(|tab| face.width(tab.title()) + ctx.px(28)).collect();
        let tabs = kit::Tabs::new(ctx, frame.toolbar, frame.toolbar.x + ctx.px(12) as i32, &widths);
        let bh = ctx.px(32);
        let label_w = ctx.face(Role::Body).width("Обновить") + ctx.px(46);
        let refresh = Rect::new(
            frame.toolbar.right() - ctx.px(12) as i32 - label_w as i32,
            frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2,
            label_w,
            bh,
        );
        Self { toolbar: frame.toolbar, tabs, refresh, body: frame.body, status: frame.status }
    }

    /// Верх вкладки (графики) и низ (таблица процессов). `top_h` — желаемая
    /// высота верха, не больше половины тела.
    fn split(&self, ctx: Ctx, top_h: u32) -> (Rect, Rect) {
        let pad = ctx.px(16);
        let inner = Rect::new(self.body.x + pad as i32, self.body.y + pad as i32, self.body.w.saturating_sub(pad * 2), self.body.h.saturating_sub(pad));
        let top_h = top_h.min(inner.h / 2);
        let top = Rect::new(inner.x, inner.y, inner.w, top_h);
        let table_y = top.bottom() + ctx.px(12) as i32;
        let table = Rect::new(self.body.x, table_y, self.body.w, (self.body.bottom() - table_y).max(0) as u32);
        (top, table)
    }
}

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

struct Monitor {
    tab: Tab,
    info: SysInfo,
    tasks: Vec<Task>,
    before: Vec<(u32, u64, u64, u64)>,
    before_at: u64,
    cpus: usize,
    /// Загрузка каждого ядра в тысячных и её история.
    cores: Vec<u16>,
    core_history: Vec<Vec<u16>>,
    cpu_history: Vec<u16>,
    memory_history: Vec<u16>,
    /// Байт/с — нормируются при рисовании.
    disk_history: Vec<u64>,
    net_history: Vec<u64>,
    disk_total: u64,
    net_total: u64,
    volumes: Vec<Volume>,
    net: NetInfo,
    has_net: bool,
    selected: usize,
    top: Cell<usize>,
    visible: Cell<usize>,
}

impl Monitor {
    fn new(info: SysInfo) -> Self {
        let mut monitor = Self {
            tab: Tab::Overview,
            info,
            tasks: Vec::new(),
            before: Vec::new(),
            before_at: 0,
            cpus: info.cpus.max(1) as usize,
            cores: Vec::new(),
            core_history: Vec::new(),
            cpu_history: Vec::new(),
            memory_history: Vec::new(),
            disk_history: Vec::new(),
            net_history: Vec::new(),
            disk_total: 0,
            net_total: 0,
            volumes: Vec::new(),
            net: NetInfo::default(),
            has_net: false,
            selected: 0,
            top: Cell::new(0),
            visible: Cell::new(1),
        };
        monitor.refresh();
        println(&format!("resmon: ready, {} cpu(s)", monitor.cpus));
        monitor
    }

    fn refresh(&mut self) {
        if let Some(info) = sysinfo() {
            self.info = info;
        }
        let now = monotonic_ms();
        let mut tasks = list_tasks();
        let idle_count = tasks.iter().filter(|task| task.is_idle()).count();
        if idle_count > 0 {
            self.cpus = idle_count;
        }
        let elapsed = now.saturating_sub(self.before_at);
        let fresh = self.before_at != 0 && elapsed > 0;
        if fresh {
            for task in &mut tasks {
                if let Some((_, was, disk, net)) = self.before.iter().find(|(id, ..)| *id == task.id) {
                    let spent = task.cpu_ms.saturating_sub(*was);
                    task.cpu = (spent * 1000 / (elapsed * self.cpus as u64)).min(1000) as u32;
                    task.disk_rate = task.disk_bytes.unwrap_or(0).saturating_sub(*disk) * 1000 / elapsed;
                    task.net_rate = task.net_bytes.unwrap_or(0).saturating_sub(*net) * 1000 / elapsed;
                }
            }
        }
        // Ядра: холостые задачи по номерам — по порядку процессоров.
        let mut idle: Vec<&Task> = tasks.iter().filter(|task| task.is_idle()).collect();
        idle.sort_by_key(|task| task.id);
        let mut cores = Vec::with_capacity(idle.len());
        for task in &idle {
            let load = match self.before.iter().find(|(id, ..)| *id == task.id) {
                Some((_, was, ..)) if fresh => {
                    let spent = task.cpu_ms.saturating_sub(*was);
                    1000u64.saturating_sub((spent * 1000 / elapsed).min(1000)) as u16
                }
                _ => 0,
            };
            cores.push(load);
        }
        if cores.is_empty() {
            cores.push(self.cpu_total() as u16);
        }
        self.core_history.resize(cores.len(), Vec::new());
        if fresh {
            for (history, load) in self.core_history.iter_mut().zip(&cores) {
                push(history, *load);
            }
        }
        self.cores = cores;

        self.before = tasks
            .iter()
            .map(|task| (task.id, task.cpu_ms, task.disk_bytes.unwrap_or(0), task.net_bytes.unwrap_or(0)))
            .collect();
        self.before_at = now;
        self.tasks = tasks;

        self.disk_total = self.tasks.iter().map(|task| task.disk_rate).sum();
        self.net_total = self.tasks.iter().map(|task| task.net_rate).sum();
        if fresh {
            let total = self.cpu_total();
            push(&mut self.cpu_history, total as u16);
            let used = self.info.frames_total.saturating_sub(self.info.frames_free);
            push(&mut self.memory_history, permille(used, self.info.frames_total));
            push_rate(&mut self.disk_history, self.disk_total);
            push_rate(&mut self.net_history, self.net_total);
        }
        self.volumes = list_volumes();
        let mut net = NetInfo::default();
        self.has_net = netinfo(&mut net) >= 0 && net.present != 0;
        self.net = net;
        let rows = self.rows().len();
        self.selected = self.selected.min(rows.saturating_sub(1));
    }

    /// Доля процессора у всех, кроме бездействия, — в десятых процента.
    fn cpu_total(&self) -> u32 {
        self.tasks.iter().filter(|task| !task.is_idle()).map(|task| task.cpu).sum::<u32>().min(1000)
    }

    /// Задачи вкладки в её порядке (холостые не показываются нигде).
    fn rows(&self) -> Vec<&Task> {
        let mut rows: Vec<&Task> = self.tasks.iter().filter(|task| !task.is_idle()).collect();
        match self.tab {
            Tab::Overview | Tab::Cpu => rows.sort_by(|a, b| b.cpu.cmp(&a.cpu).then(b.cpu_ms.cmp(&a.cpu_ms))),
            Tab::Memory => rows.sort_by(|a, b| b.pages.unwrap_or(0).cmp(&a.pages.unwrap_or(0)).then(a.id.cmp(&b.id))),
            Tab::Disk => rows.sort_by(|a, b| b.disk_rate.cmp(&a.disk_rate).then(b.disk_bytes.unwrap_or(0).cmp(&a.disk_bytes.unwrap_or(0)))),
            Tab::Network => rows.sort_by(|a, b| b.net_rate.cmp(&a.net_rate).then(b.net_bytes.unwrap_or(0).cmp(&a.net_bytes.unwrap_or(0)))),
        }
        rows
    }

    fn set_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.selected = 0;
        self.top.set(0);
        println(&format!("resmon: tab {}: {}", tab.log_name(), self.summary()));
    }

    /// Одна строка о вкладке — для журнала.
    fn summary(&self) -> String {
        let rows = self.rows();
        let top = rows.first().map_or_else(|| String::from("-"), |task| task.label().to_string());
        match self.tab {
            Tab::Overview => format!(
                "cpu {}, memory {} of {} MiB, disk {} B/s, network {} B/s, top '{top}'",
                percent_text(self.cpu_total()),
                self.info.frames_total.saturating_sub(self.info.frames_free) >> 20,
                self.info.frames_total >> 20,
                self.disk_total,
                self.net_total
            ),
            Tab::Cpu => {
                let cores: Vec<String> = self.cores.iter().map(|load| percent_text(u32::from(*load))).collect();
                format!("{} core(s) [{}], top '{top}'", self.cores.len(), cores.join(" "))
            }
            Tab::Memory => format!(
                "{} of {} MiB used, top '{top}' {} pages",
                self.info.frames_total.saturating_sub(self.info.frames_free) >> 20,
                self.info.frames_total >> 20,
                rows.first().and_then(|task| task.pages).unwrap_or(0)
            ),
            Tab::Disk => format!("{} B/s, {} volume(s), top '{top}'", self.disk_total, self.volumes.len()),
            Tab::Network => format!(
                "{} B/s, adapter {}, top '{top}'",
                self.net_total,
                if self.has_net { ipv4(self.net.address) } else { String::from("none") }
            ),
        }
    }

    fn step(&mut self, delta: isize) {
        let rows = self.rows().len();
        if rows == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, rows as isize - 1) as usize;
    }

    // -- Рисование ---------------------------------------------------------

    /// Таблица процессов вкладки.
    fn draw_table(&self, s: &mut Surface, ctx: Ctx, rect: Rect) {
        let p = ctx.palette;
        let rows = self.rows();
        let (titles, widths): (&[&str], &[u32]) = match self.tab {
            Tab::Overview => (&["ПРОЦЕСС", "ЗАДАЧА", "ЦП", "ПАМЯТЬ", "ДИСК", "СЕТЬ"], &[0, 80, 80, 100, 110, 120]),
            Tab::Cpu => (&["ПРОЦЕСС", "ЗАДАЧА", "СОСТОЯНИЕ", "ЦП", "ВРЕМЯ ЦП"], &[0, 80, 120, 80, 110]),
            Tab::Memory => (&["ПРОЦЕСС", "ЗАДАЧА", "ПАМЯТЬ", "СТРАНИЦ", "ДОЛЯ"], &[0, 80, 110, 100, 80]),
            Tab::Disk => (&["ПРОЦЕСС", "ЗАДАЧА", "ДИСК СЕЙЧАС", "ВСЕГО"], &[0, 80, 130, 120]),
            Tab::Network => (&["ПРОЦЕСС", "ЗАДАЧА", "СЕТЬ СЕЙЧАС", "ВСЕГО"], &[0, 80, 140, 120]),
        };
        let widths: Vec<u32> = widths.iter().map(|w| ctx.px(*w)).collect();
        let table = kit::Table::new(ctx, rect).with_row(ctx.px(ROW_H));
        let columns = table.columns(ctx, &widths, ctx.px(180));
        table.draw_header(ctx, s, &columns, titles);
        let visible = table.visible().max(1);
        self.visible.set(visible);
        let top = view_top(self.top.get(), self.selected, visible, rows.len());
        self.top.set(top);
        let g = ctx.px(16);
        let total_mem = self.info.frames_total.max(1);
        for (offset, task) in rows.iter().enumerate().skip(top).take(visible) {
            let row = table.row_rect(ctx, offset - top);
            let on = offset == self.selected;
            if on {
                paint::row(ctx, s, row, RowState::Selected);
            }
            let (icon, tint) = task.kind.icon();
            iconset::draw(s, icon, columns[0].x, row.y + (row.h as i32 - g as i32) / 2, g, if on { p.acc_ink } else { tint }, 255);
            kit::cell(ctx, s, columns[0], columns[0].x + (g + ctx.px(8)) as i32, row, Role::Body, task.label(), p.ink);
            let mut cells: Vec<(String, Color)> = vec![(format!("#{}", task.id), p.ink3)];
            let memory = task.pages.map_or_else(|| String::from("—"), |pages| size_text(pages * 4096));
            match self.tab {
                Tab::Overview => {
                    cells.push((percent_text(task.cpu), load_ink(ctx, task.cpu)));
                    cells.push((memory, p.ink2));
                    cells.push((rate_text(task.disk_rate), if task.disk_rate > 0 { p.ink } else { p.ink4 }));
                    cells.push((net_rate_text(task.net_rate), if task.net_rate > 0 { p.ink } else { p.ink4 }));
                }
                Tab::Cpu => {
                    cells.push((task.state_title().to_string(), p.ink3));
                    cells.push((percent_text(task.cpu), load_ink(ctx, task.cpu)));
                    cells.push((time_text(task.cpu_ms), p.ink2));
                }
                Tab::Memory => {
                    cells.push((memory, p.ink));
                    cells.push((task.pages.map_or_else(|| String::from("—"), |pages| format!("{pages}")), p.ink3));
                    let share = task.pages.map_or(0, |pages| (pages * 4096 * 1000 / total_mem) as u32);
                    cells.push((percent_text(share), p.ink2));
                }
                Tab::Disk => {
                    cells.push((rate_text(task.disk_rate), if task.disk_rate > 0 { p.ink } else { p.ink4 }));
                    cells.push((task.disk_bytes.map_or_else(|| String::from("—"), size_text), p.ink3));
                }
                Tab::Network => {
                    cells.push((net_rate_text(task.net_rate), if task.net_rate > 0 { p.ink } else { p.ink4 }));
                    cells.push((task.net_bytes.map_or_else(|| String::from("—"), size_text), p.ink3));
                }
            }
            for (column, (text, ink)) in columns.iter().skip(1).zip(&cells) {
                kit::cell(ctx, s, *column, column.x, row, Role::Mono, text, *ink);
            }
        }
        if rows.is_empty() {
            paint::text(ctx, s, Role::Body, columns[0].x, table.list.y + ctx.px(10) as i32, "Список задач пуст", p.ink4);
        }
        scrollbar(ctx, s, table.list, rows.len(), visible, top, table.row_h);
    }

    /// Карточка-счётчик: подпись, значение и график.
    fn meter_card(&self, s: &mut Surface, ctx: Ctx, rect: Rect, title: &str, value: &str, note: &str, samples: &[u16]) {
        let p = ctx.palette;
        paint::card(ctx, s, rect);
        let inner = ctx.on(ctx.flat(p.card));
        let pad = ctx.px(12) as i32;
        let room = rect.w.saturating_sub(ctx.px(24));
        paint::text_clipped(inner, s, Role::Strong, rect.x + pad, rect.y + pad, room, title, p.ink);
        let value_y = rect.y + pad;
        paint::text_right(inner, s, Role::Mono, rect.right() - pad, value_y, value, p.ink2);
        let note_y = value_y + line_h(ctx, Role::Strong) + ctx.px(2) as i32;
        paint::text_clipped(inner, s, Role::Caption, rect.x + pad, note_y, room, note, p.ink4);
        let gy = note_y + line_h(ctx, Role::Caption) + ctx.px(8) as i32;
        let gh = (rect.bottom() - pad - gy).max(ctx.px(16) as i32) as u32;
        graph(ctx, s, Rect::new(rect.x + pad, gy, room, gh), samples, gh > ctx.px(60));
    }

    fn draw_overview(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let (top, table) = plan.split(ctx, ctx.px(170));
        let gap = ctx.px(12);
        // Четыре карточки в ряд; узкое окно — две в ряд.
        let per_row = if top.w >= ctx.px(4 * 200) { 4 } else { 2 };
        let rows = 4 / per_row;
        let w = (top.w.saturating_sub(gap * (per_row - 1))) / per_row;
        let h = (top.h.saturating_sub(gap * (rows - 1))) / rows;
        let mib = 1024 * 1024;
        let used = self.info.frames_total.saturating_sub(self.info.frames_free);
        let disk = normalize(&self.disk_history);
        let net = normalize(&self.net_history);
        let cards: [(&str, String, String, &[u16]); 4] = [
            ("Процессор", percent_text(self.cpu_total()), format!("ядер: {}", self.cores.len()), &self.cpu_history),
            ("Память", format!("{} / {} МиБ", used / mib, self.info.frames_total / mib), String::from("занятые кадры"), &self.memory_history),
            ("Диск", rate_text(self.disk_total), String::from("чтение и запись"), &disk),
            ("Сеть", net_rate_text(self.net_total), String::from("UDP и TCP"), &net),
        ];
        for (index, (title, value, note, samples)) in cards.iter().enumerate() {
            let col = index as u32 % per_row;
            let row = index as u32 / per_row;
            let rect = Rect::new(top.x + (col * (w + gap)) as i32, top.y + (row * (h + gap)) as i32, w, h);
            self.meter_card(s, ctx, rect, title, value, note, samples);
        }
        self.draw_table(s, ctx, table);
    }

    fn draw_cpu(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let count = self.cores.len().max(1) as u32;
        // Сетка графиков ядер: не уже 200 точек, не больше 8 в ряд.
        let pad = ctx.px(16);
        let inner_w = plan.body.w.saturating_sub(pad * 2);
        let gap = ctx.px(10);
        let per_row = (inner_w / ctx.px(200)).clamp(1, 8).min(count);
        let rows = count.div_ceil(per_row);
        let cell_h = ctx.px(96);
        let want = rows * cell_h + (rows - 1) * gap;
        let (top, table) = plan.split(ctx, want);
        let cell_h = ((top.h.saturating_sub((rows - 1) * gap)) / rows).max(ctx.px(40));
        let w = (top.w.saturating_sub(gap * (per_row - 1))) / per_row;
        for (index, load) in self.cores.iter().enumerate() {
            let col = index as u32 % per_row;
            let row = index as u32 / per_row;
            let rect = Rect::new(top.x + (col * (w + gap)) as i32, top.y + (row * (cell_h + gap)) as i32, w, cell_h);
            if rect.bottom() > top.bottom() + 1 {
                break;
            }
            paint::card(ctx, s, rect);
            let inner = ctx.on(ctx.flat(p.card));
            let x = rect.x + ctx.px(10) as i32;
            let y = rect.y + ctx.px(8) as i32;
            paint::text(inner, s, Role::Strong, x, y, &format!("ЦП {index}"), p.ink);
            paint::text_right(inner, s, Role::Mono, rect.right() - ctx.px(10) as i32, y, &percent_text(u32::from(*load)), load_ink(ctx, u32::from(*load)));
            let bar_y = y + line_h(ctx, Role::Strong) + ctx.px(4) as i32;
            let bar = Rect::new(x, bar_y, rect.w.saturating_sub(ctx.px(20)), ctx.px(6));
            paint::progress(inner, s, bar, u32::from(*load), 1000, load_tone(u32::from(*load)));
            let gy = bar.bottom() + ctx.px(6) as i32;
            let gh = (rect.bottom() - ctx.px(8) as i32 - gy).max(0) as u32;
            if gh >= ctx.px(14) {
                let history: &[u16] = self.core_history.get(index).map_or(&[], Vec::as_slice);
                graph(ctx, s, Rect::new(x, gy, bar.w, gh), history, false);
            }
        }
        self.draw_table(s, ctx, table);
    }

    fn draw_memory(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let (top, table) = plan.split(ctx, ctx.px(190));
        let info = &self.info;
        let mib = 1024 * 1024;
        let left_w = (top.w * 5 / 12).max(ctx.px(240)).min(top.w);
        let used = info.frames_total.saturating_sub(info.frames_free);
        let heap = info.heap_size.saturating_sub(info.heap_free);
        let bars: [(&str, String, u64, u64); 3] = [
            ("Физическая память", format!("{} / {} МиБ", used / mib, info.frames_total / mib), used, info.frames_total),
            ("Куча ядра", format!("{} / {} КиБ", heap / 1024, info.heap_size / 1024), heap, info.heap_size),
            ("Память устройств", format!("{} / {} КиБ", info.dma_used / 1024, info.dma_total / 1024), info.dma_used, info.dma_total),
        ];
        let step = (top.h / 3).max(ctx.px(40));
        for (index, (title, value, part, whole)) in bars.iter().enumerate() {
            let y = top.y + (index as u32 * step) as i32;
            if y + ctx.px(36) as i32 > top.bottom() + 1 {
                break;
            }
            paint::text_clipped(ctx, s, Role::Strong, top.x, y, left_w, title, p.ink);
            paint::text_right(ctx, s, Role::Mono, top.x + left_w as i32, y, value, p.ink2);
            let bar = Rect::new(top.x, y + line_h(ctx, Role::Strong) + ctx.px(6) as i32, left_w, ctx.px(10));
            let share = u32::from(permille(*part, *whole));
            paint::progress(ctx, s, bar, share, 1000, load_tone(share));
        }
        let gx = top.x + (left_w + ctx.px(24)) as i32;
        let gw = (top.right() - gx).max(0) as u32;
        if gw > ctx.px(120) {
            let caption = "Занятые кадры памяти за 2 минуты";
            paint::text_clipped(ctx, s, Role::Caption, gx, top.y, gw, caption, p.ink4);
            let gy = top.y + line_h(ctx, Role::Caption) + ctx.px(6) as i32;
            graph(ctx, s, Rect::new(gx, gy, gw, (top.bottom() - gy).max(0) as u32), &self.memory_history, true);
        }
        self.draw_table(s, ctx, table);
    }

    fn draw_disk(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let (top, table) = plan.split(ctx, ctx.px(200));
        let graph_w = (top.w / 2).max(ctx.px(200)).min(top.w);
        let caption = format!("Обмен с носителями за 2 минуты · сейчас {}", rate_text(self.disk_total));
        paint::text_clipped(ctx, s, Role::Caption, top.x, top.y, graph_w, &caption, p.ink4);
        let gy = top.y + line_h(ctx, Role::Caption) + ctx.px(6) as i32;
        graph(ctx, s, Rect::new(top.x, gy, graph_w, (top.bottom() - gy).max(0) as u32), &normalize(&self.disk_history), true);
        // Тома справа.
        let vx = top.x + (graph_w + ctx.px(24)) as i32;
        let vw = (top.right() - vx).max(0) as u32;
        if vw < ctx.px(160) {
            self.draw_table(s, ctx, table);
            return;
        }
        paint::caps(ctx, s, vx, top.y, "ТОМА");
        let mut y = top.y + line_h(ctx, Role::MonoCaps) + ctx.px(8) as i32;
        let step = line_h(ctx, Role::Body) + ctx.px(18) as i32;
        for volume in &self.volumes {
            if y + step > top.bottom() + 1 {
                break;
            }
            let name = if volume.label.is_empty() { volume.point.clone() } else { format!("{} ({})", volume.label, volume.point) };
            paint::text_clipped(ctx, s, Role::Body, vx, y, vw / 2, &name, p.ink);
            let detail = match (volume.total, volume.free) {
                (Some(total), Some(free)) => format!("{} · свободно {} из {}", volume.fs, size_text(free), size_text(total)),
                _ => format!("{} · размер не сообщается", volume.fs),
            };
            paint::text_right(ctx, s, Role::MonoSmall, vx + vw as i32, y + ctx.px(2) as i32, &detail, p.ink3);
            let bar = Rect::new(vx, y + line_h(ctx, Role::Body) + ctx.px(4) as i32, vw, ctx.px(6));
            if let (Some(total), Some(free)) = (volume.total, volume.free) {
                let share = u32::from(permille(total.saturating_sub(free), total));
                paint::progress(ctx, s, bar, share, 1000, load_tone(share));
            } else {
                paint::progress(ctx, s, bar, 0, 1, Tone::Muted);
            }
            y += step;
        }
        if self.volumes.is_empty() {
            paint::text(ctx, s, Role::Body, vx, y, "Томов нет", p.ink4);
        }
        self.draw_table(s, ctx, table);
    }

    fn draw_network(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let (top, table) = plan.split(ctx, ctx.px(180));
        let graph_w = (top.w * 3 / 5).max(ctx.px(200)).min(top.w);
        let caption = format!("Отдано и получено по сети за 2 минуты · сейчас {}", net_rate_text(self.net_total));
        paint::text_clipped(ctx, s, Role::Caption, top.x, top.y, graph_w, &caption, p.ink4);
        let gy = top.y + line_h(ctx, Role::Caption) + ctx.px(6) as i32;
        graph(ctx, s, Rect::new(top.x, gy, graph_w, (top.bottom() - gy).max(0) as u32), &normalize(&self.net_history), true);
        let vx = top.x + (graph_w + ctx.px(24)) as i32;
        let vw = (top.right() - vx).max(0) as u32;
        if vw >= ctx.px(160) {
            paint::caps(ctx, s, vx, top.y, "СЕТЕВАЯ КАРТА");
            let mut y = top.y + line_h(ctx, Role::MonoCaps) + ctx.px(8) as i32;
            let facts: Vec<(&str, String)> = if self.has_net {
                let mac = self.net.mac;
                vec![
                    ("Адрес", if self.net.address == 0 { String::from("не получен") } else { ipv4(self.net.address) }),
                    ("Маска", ipv4(self.net.netmask)),
                    ("Шлюз", ipv4(self.net.gateway)),
                    ("DNS", ipv4(self.net.dns)),
                    ("MAC", format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])),
                ]
            } else {
                vec![("Карта", String::from("нет в системе"))]
            };
            let label_w = ctx.px(70);
            for (label, value) in &facts {
                if y + line_h(ctx, Role::Body) > top.bottom() + 1 {
                    break;
                }
                paint::text(ctx, s, Role::Caption, vx, y + ctx.px(2) as i32, label, p.ink4);
                paint::text_clipped(ctx, s, Role::Mono, vx + label_w as i32, y, vw.saturating_sub(label_w), value, p.ink);
                y += line_h(ctx, Role::Body) + ctx.px(6) as i32;
            }
        }
        self.draw_table(s, ctx, table);
    }
}

impl App for Monitor {
    /// Smallest window (24.11, WM SDK): the tabs, a table and the graphs column.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(640, 420)
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        let labels: Vec<&str> = Tab::ALL.iter().map(|tab| tab.title()).collect();
        let active = Tab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        plan.tabs.draw(bar, s, &labels, active);
        if plan.refresh.x > plan.tabs.frame.right() + ctx.px(8) as i32 {
            tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить");
        }
        match self.tab {
            Tab::Overview => self.draw_overview(s, ctx, &plan),
            Tab::Cpu => self.draw_cpu(s, ctx, &plan),
            Tab::Memory => self.draw_memory(s, ctx, &plan),
            Tab::Disk => self.draw_disk(s, ctx, &plan),
            Tab::Network => self.draw_network(s, ctx, &plan),
        }
        let status = format!(
            "ЦП {} · ядер {} · память {} / {} МиБ · диск {} · сеть {} · 1–5 — вкладки, R — обновить",
            percent_text(self.cpu_total()),
            self.cores.len(),
            self.info.frames_total.saturating_sub(self.info.frames_free) >> 20,
            self.info.frames_total >> 20,
            rate_text(self.disk_total),
            net_rate_text(self.net_total)
        );
        kit::status_bar(ctx, s, plan.status, &status);
    }

    fn key(&mut self, code: u32) -> bool {
        match code {
            code if (u32::from('1')..=u32::from('5')).contains(&code) => {
                self.set_tab(Tab::ALL[(code - u32::from('1')) as usize]);
            }
            WIN_KEY_UP => self.step(-1),
            WIN_KEY_DOWN => self.step(1),
            WIN_KEY_PAGE_UP => self.step(-(self.visible.get() as isize)),
            WIN_KEY_PAGE_DOWN => self.step(self.visible.get() as isize),
            WIN_KEY_HOME => self.selected = 0,
            WIN_KEY_END => self.selected = self.rows().len().saturating_sub(1),
            F5 => self.refresh(),
            code if code == u32::from('r') || code == u32::from('R') => {
                self.refresh();
                println(&format!("resmon: refreshed, tab {}: {}", self.tab.log_name(), self.summary()));
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if let Some(index) = plan.tabs.hit(x, y) {
            self.set_tab(Tab::ALL[index]);
            return true;
        }
        if plan.refresh.contains(x, y) {
            self.refresh();
            return true;
        }
        // Строка таблицы: таблица всегда снизу, её верх — после графиков.
        let rows = self.rows().len();
        let row_h = ctx.px(ROW_H) as i32;
        let table_top = self.table_top(ctx, &plan);
        if y >= table_top && y < plan.body.bottom() {
            let index = self.top.get() + ((y - table_top) / row_h.max(1)) as usize;
            if index < rows {
                self.selected = index;
                return true;
            }
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        let rows = self.rows().len();
        let visible = self.visible.get().max(1);
        let max_top = rows.saturating_sub(visible);
        let top = if clicks > 0 { self.top.get().saturating_sub(3) } else { (self.top.get() + 3).min(max_top) };
        self.top.set(top);
        self.selected = self.selected.clamp(top, (top + visible).saturating_sub(1).min(rows.saturating_sub(1)).max(top));
        true
    }

    fn tick(&mut self, info: &SysInfo) -> bool {
        self.info = *info;
        self.refresh();
        true
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Resource Monitor\x1fМонитор ресурсов"))
    }
}

impl Monitor {
    /// Где начинаются строки таблицы (под её шапкой) — для щелчка.
    fn table_top(&self, ctx: Ctx, plan: &Plan) -> i32 {
        let want = match self.tab {
            Tab::Overview => ctx.px(170),
            Tab::Cpu => {
                let pad = ctx.px(16);
                let count = self.cores.len().max(1) as u32;
                let per_row = (plan.body.w.saturating_sub(pad * 2) / ctx.px(200)).clamp(1, 8).min(count);
                let rows = count.div_ceil(per_row);
                rows * ctx.px(96) + (rows - 1) * ctx.px(10)
            }
            Tab::Memory => ctx.px(190),
            Tab::Disk => ctx.px(200),
            Tab::Network => ctx.px(180),
        };
        let (_, table) = plan.split(ctx, want);
        let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(table.h);
        table.y + header_h as i32
    }
}

// ---------------------------------------------------------------------------
// Мелочи рисования
// ---------------------------------------------------------------------------

fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, p.ink2, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
}

/// График истории: столбики в тысячных от 0 до 1000.
fn graph(ctx: Ctx, s: &mut Surface, rect: Rect, samples: &[u16], grid: bool) {
    let p = ctx.palette;
    if rect.w < 4 || rect.h < 4 {
        return;
    }
    draw::rounded(s, rect, ctx.px(4), ctx.flat(p.sunk), 255);
    draw::rounded_stroke(s, rect, ctx.px(4), p.line2.color, p.line2.alpha);
    if grid {
        for step in 1..4 {
            let y = rect.y + (rect.h * step / 4) as i32;
            draw::hline(s, rect.x + 1, y, rect.w.saturating_sub(2), p.line.color, p.line.alpha);
        }
        for step in 1..6 {
            let x = rect.x + (rect.w * step / 6) as i32;
            draw::vline(s, x, rect.y + 1, rect.h.saturating_sub(2), p.line.color, p.line.alpha);
        }
    }
    if samples.is_empty() {
        return;
    }
    let inner = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2);
    let start = HISTORY - samples.len().min(HISTORY);
    let at = |slot: usize| inner.x + (slot as u32 * inner.w / HISTORY as u32) as i32;
    let mut previous: Option<(i32, i32)> = None;
    for (index, value) in samples.iter().enumerate() {
        let x0 = at(start + index);
        let x1 = at(start + index + 1);
        let h = (u32::from((*value).min(1000)) * inner.h / 1000).max(1);
        let column = Rect::new(x0, inner.bottom() - h as i32, (x1 - x0).max(1) as u32, h);
        draw::blend_rect(s, column, p.acc, 50);
        let point = (x0 + (x1 - x0) / 2, column.y);
        if let Some(from) = previous {
            draw::line(s, from, point, ctx.px(2), p.acc, 255);
        }
        previous = Some(point);
    }
}

/// Полоса прокрутки списка — когда строк больше, чем помещается.
fn scrollbar(ctx: Ctx, s: &mut Surface, list: Rect, rows: usize, visible: usize, top: usize, row_h: u32) {
    if rows <= visible {
        return;
    }
    let p = ctx.palette;
    let track = Rect::new(list.right() - ctx.px(8) as i32, list.y, ctx.px(4), (visible as u32 * row_h).min(list.h));
    draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
    let thumb_h = ((track.h as usize * visible / rows) as u32).max(ctx.px(20)).min(track.h);
    let max_top = rows - visible;
    let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
    draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
}

fn view_top(top: usize, selected: usize, visible: usize, rows: usize) -> usize {
    let visible = visible.max(1);
    let mut top = top.min(rows.saturating_sub(visible));
    if selected < top {
        top = selected;
    } else if selected >= top + visible {
        top = selected + 1 - visible;
    }
    top
}

fn load_tone(permille: u32) -> Tone {
    if permille >= 900 {
        Tone::Bad
    } else if permille >= 700 {
        Tone::Warn
    } else {
        Tone::Accent
    }
}

fn load_ink(ctx: Ctx, tenths: u32) -> Color {
    let p = ctx.palette;
    if tenths >= 500 {
        p.warn
    } else if tenths > 0 {
        p.ink
    } else {
        p.ink4
    }
}

// ---------------------------------------------------------------------------
// Числа
// ---------------------------------------------------------------------------

fn push(history: &mut Vec<u16>, value: u16) {
    if history.len() == HISTORY {
        history.remove(0);
    }
    history.push(value);
}

fn push_rate(history: &mut Vec<u64>, value: u64) {
    if history.len() == HISTORY {
        history.remove(0);
    }
    history.push(value);
}

/// Скорости в тысячные от наибольшей (не меньше 64 КиБ/с, чтобы шум в
/// несколько байт не рисовался полным столбиком).
fn normalize(history: &[u64]) -> Vec<u16> {
    let top = history.iter().copied().max().unwrap_or(0).max(64 * 1024);
    history.iter().map(|value| (value.saturating_mul(1000) / top).min(1000) as u16).collect()
}

fn permille(part: u64, whole: u64) -> u16 {
    if whole == 0 { 0 } else { (part.min(whole) * 1000 / whole) as u16 }
}

fn percent_text(tenths: u32) -> String {
    if tenths == 0 {
        String::from("0%")
    } else if tenths < 100 {
        format!("{},{}%", tenths / 10, tenths % 10)
    } else {
        format!("{}%", (tenths + 5) / 10)
    }
}

fn size_text(bytes: u64) -> String {
    kit::size_text(bytes)
}

/// «0 Б/с», «12 КБ/с», «1,4 МБ/с» (двоичные).
fn rate_text(bytes_per_s: u64) -> String {
    if bytes_per_s < 1024 {
        format!("{bytes_per_s} Б/с")
    } else if bytes_per_s < 1024 * 1024 {
        format!("{} КБ/с", bytes_per_s / 1024)
    } else {
        let tenths = bytes_per_s * 10 / (1024 * 1024);
        if tenths >= 100 { format!("{} МБ/с", tenths / 10) } else { format!("{},{} МБ/с", tenths / 10, tenths % 10) }
    }
}

/// Сеть — в битах, как принято: «0 бит/с», «840 Кбит/с», «2,1 Мбит/с».
fn net_rate_text(bytes_per_s: u64) -> String {
    let bits = bytes_per_s * 8;
    if bits < 1000 {
        format!("{bits} бит/с")
    } else if bits < 1_000_000 {
        format!("{} Кбит/с", bits / 1000)
    } else {
        let tenths = bits / 100_000;
        if tenths >= 100 { format!("{} Мбит/с", tenths / 10) } else { format!("{},{} Мбит/с", tenths / 10, tenths % 10) }
    }
}

fn time_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}

fn ipv4(address: u32) -> String {
    format!("{}.{}.{}.{}", address >> 24, (address >> 16) & 0xFF, (address >> 8) & 0xFF, address & 0xFF)
}

// ---------------------------------------------------------------------------
// Данные из ядра
// ---------------------------------------------------------------------------

fn call_text(call: impl FnOnce(&mut [u8]) -> i64, size: usize) -> String {
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(size).is_err() {
        return String::new();
    }
    buffer.resize(size, 0u8);
    let got = call(&mut buffer);
    if got <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buffer[..(got as usize).min(size)]).to_string()
}

fn list_tasks() -> Vec<Task> {
    call_text(|buffer| tasks(buffer), TASKS_LIMIT).lines().filter_map(Task::parse).filter(|task| task.state != "finished").collect()
}

/// `<точка>\t<ФС>\t<всего>\t<свободно>[\t<метка>]`.
fn list_volumes() -> Vec<Volume> {
    call_text(|buffer| mounts(buffer), MOUNTS_LIMIT)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            let point = fields.next()?.to_string();
            let fs = fields.next().unwrap_or("?").to_string();
            let total = fields.next().and_then(|text| text.parse().ok());
            let free = fields.next().and_then(|text| text.parse().ok());
            let label = fields.next().unwrap_or("").trim().to_string();
            Some(Volume { point, fs, label, total, free })
        })
        .collect()
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (960, 640));
    let fresh = sysinfo().unwrap_or(info);
    app::run(&SPEC, &info, size, move |_, _| Monitor::new(fresh))
}
