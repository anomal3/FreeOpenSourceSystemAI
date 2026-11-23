//! «Управление компьютером» (фаза П3, 20.11) — как compmgmt.msc в Windows.
//!
//! # Зачем
//!
//! Журнал, службы, устройства, учётные записи и диски — пять разных окон, а
//! разбирающему машину нужно видеть их рядом: служба не поднялась — что в
//! журнале, жив ли её диск, есть ли драйвер. Здесь одно окно: слева дерево,
//! справа — вид выбранного узла. Виды настоящие (те же источники, что у
//! отдельных инструментов), а кнопка на панели открывает полный инструмент.
//!
//! # Узлы дерева
//!
//! * «Журнал событий» — `dmesg 400` через `SYS_KERNEL_COMMAND` (только root),
//!   новые сверху, фильтр «ошибки»/«предупреждения».
//! * «Службы» — `/etc/services` и список задач; «Перезапустить» у root.
//! * «Диспетчер устройств» — `SYS_DEVICES` по шинам.
//! * «Пользователи и группы» — `/etc/passwd` (`имя:uid:gid:…`) и `/etc/group`.
//! * «Диски и тома» — новое: `SYS_MOUNTS` (точка, ФС, всего, свободно, метка —
//!   у exFAT-флешки `/usbN` это её имя) с полосой занятого и физические диски
//!   из `SYS_DEVICES`.
//! * «Видеокарта и драйверы» — команды ядра `gpu`, `nvk`, `outputs` (только
//!   чтение, без аргументов: ничего не запускают), узел `/dev/dri/renderD128`
//!   и драйверы Vulkan (`icd.d/*.json`).
//!
//! `compmgmt --page events|services|devices|users|disks|gpu` открывает окно
//! сразу на узле; клавиши 1–6 — узлы в порядке дерева.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell as Shared;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, KIND_DIRECTORY, SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_UP, close, kernel_command, open, println, read, spawn, stat,
};

const SPEC: Spec = Spec { name: "compmgmt", title: "Computer Management", period_ms: 5_000 };

const ENTER: u32 = '\n' as u32;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;
const ROW_H: u32 = 32;
const TREE_W: u32 = 250;
const TREE_ROW_H: u32 = 32;
const NODE_PATH: &str = "/dev/dri/renderD128";

// ---------------------------------------------------------------------------
// Узлы
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Page {
    Events,
    Services,
    Devices,
    Users,
    Disks,
    Gpu,
}

impl Page {
    /// Порядок дерева — он же порядок клавиш 1–6.
    const ALL: [Self; 6] = [Self::Events, Self::Services, Self::Devices, Self::Users, Self::Disks, Self::Gpu];

    const fn key(self) -> &'static str {
        match self {
            Self::Events => "events",
            Self::Services => "services",
            Self::Devices => "devices",
            Self::Users => "users",
            Self::Disks => "disks",
            Self::Gpu => "gpu",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Events => "Журнал событий",
            Self::Services => "Службы",
            Self::Devices => "Диспетчер устройств",
            Self::Users => "Пользователи и группы",
            Self::Disks => "Диски и тома",
            Self::Gpu => "Видеокарта и драйверы",
        }
    }

    const fn icon(self) -> Fi {
        match self {
            Self::Events => Fi::FileText,
            Self::Services => Fi::Server,
            Self::Devices => Fi::Layers,
            Self::Users => Fi::User,
            Self::Disks => Fi::Drive,
            Self::Gpu => Fi::Monitor,
        }
    }

    fn named(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|page| page.key() == key)
    }

    /// Столбцы: подпись и ширина в логических точках; ноль — резиновый.
    const fn columns(self) -> &'static [(&'static str, u32)] {
        match self {
            Self::Events => &[("УРОВЕНЬ", 140), ("СООБЩЕНИЕ", 0)],
            Self::Services => &[("СЛУЖБА", 170), ("СОСТОЯНИЕ", 170), ("ЗАДАЧА", 90), ("ПРОГРАММА", 0)],
            Self::Devices => &[("УСТРОЙСТВО", 0), ("МЕСТО", 170), ("ИДЕНТИФИКАТОР", 130), ("ДРАЙВЕР", 120), ("СОСТОЯНИЕ", 140)],
            Self::Users => &[("ИМЯ", 170), ("UID", 80), ("ГРУППА", 150), ("ДОМАШНЯЯ ПАПКА", 0), ("ВИД", 150)],
            Self::Disks => &[("ТОМ", 180), ("МЕТКА", 140), ("ФС", 80), ("ЁМКОСТЬ", 100), ("СВОБОДНО", 100), ("ЗАНЯТО", 0)],
            Self::Gpu => &[("ПАРАМЕТР", 240), ("ЗНАЧЕНИЕ", 0)],
        }
    }
}

/// Строка дерева: глубина, подпись, значок и узел (у заголовков — нет).
const TREE: [(u32, &str, Fi, Option<Page>); 10] = [
    (0, "Управление компьютером (локальным)", Fi::Laptop, None),
    (1, "Служебные программы", Fi::Sliders, None),
    (2, "", Fi::FileText, Some(Page::Events)),
    (2, "", Fi::Server, Some(Page::Services)),
    (2, "", Fi::Layers, Some(Page::Devices)),
    (2, "", Fi::User, Some(Page::Users)),
    (1, "Запоминающие устройства", Fi::Disc, None),
    (2, "", Fi::Drive, Some(Page::Disks)),
    (1, "Оборудование", Fi::Cpu, None),
    (2, "", Fi::Monitor, Some(Page::Gpu)),
];

// ---------------------------------------------------------------------------
// Строки вида
// ---------------------------------------------------------------------------

enum Cell {
    Text(String),
    Strong(String),
    Dim(String),
    Mono(String),
    Chip(String, Tone),
    /// Занято из всего: полоса и проценты.
    Bar(u64, u64),
}

enum Row {
    Group(String),
    Item { icon: Fi, cells: Vec<Cell>, key: String },
    /// Строка во всю ширину моноширинным (вывод команд).
    Line(String, Tone),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Errors,
    Warnings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Refresh,
    Tool(&'static str),
    Filter(Filter),
    Restart,
    OpenFiles,
    Cleanup,
    Settings(&'static str),
}

fn actions(page: Page) -> &'static [(Act, Fi, &'static str)] {
    match page {
        Page::Events => &[
            (Act::Refresh, Fi::Refresh, "Обновить"),
            (Act::Filter(Filter::All), Fi::List, "Все"),
            (Act::Filter(Filter::Errors), Fi::X, "Ошибки"),
            (Act::Filter(Filter::Warnings), Fi::Warning, "Предупреждения"),
            (Act::Tool("eventlog"), Fi::FileText, "Открыть журнал событий"),
        ],
        Page::Services => &[
            (Act::Refresh, Fi::Refresh, "Обновить"),
            (Act::Restart, Fi::Restart, "Перезапустить"),
            (Act::Tool("services"), Fi::Server, "Открыть «Службы»"),
        ],
        Page::Devices => &[(Act::Refresh, Fi::Refresh, "Обновить"), (Act::Tool("devmgr"), Fi::Layers, "Открыть диспетчер устройств")],
        Page::Users => &[(Act::Refresh, Fi::Refresh, "Обновить"), (Act::Settings("accounts"), Fi::User, "Учётные записи")],
        Page::Disks => &[
            (Act::Refresh, Fi::Refresh, "Обновить"),
            (Act::OpenFiles, Fi::FolderOpen, "Открыть в Файлах"),
            (Act::Cleanup, Fi::Trash, "Очистка диска"),
        ],
        Page::Gpu => &[
            (Act::Refresh, Fi::Refresh, "Обновить"),
            (Act::Settings("display"), Fi::Monitor, "Параметры экрана"),
            (Act::Tool("devmgr"), Fi::Layers, "Диспетчер устройств"),
        ],
    }
}

/// Что собрал узел: строки, сводка для человека и для журнала стенда.
struct View {
    rows: Vec<Row>,
    summary: String,
    log: String,
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    buttons: Vec<(Act, Fi, &'static str, Rect)>,
    toolbar: Rect,
    tree: Rect,
    head: Rect,
    header: Rect,
    list: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, page: Page) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let mut x = frame.toolbar.x + pad;
        let mut buttons = Vec::new();
        for (act, icon, label) in actions(page) {
            let rect = Rect::new(x, by, face.width(label) + ctx.px(46), bh);
            x = rect.right() + ctx.px(6) as i32;
            buttons.push((*act, *icon, *label, rect));
        }
        let tree_w = ctx.px(TREE_W).min(frame.body.w * 2 / 5);
        let tree = Rect::new(frame.body.x, frame.body.y, tree_w, frame.body.h);
        let right = Rect::new(tree.right(), frame.body.y, frame.body.w.saturating_sub(tree_w), frame.body.h);
        let head_h = u32::from(ctx.face(Role::Heading).line) + u32::from(ctx.face(Role::Body).line) + ctx.px(26);
        let head = Rect::new(right.x, right.y, right.w, head_h.min(right.h));
        let header_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14);
        let header = Rect::new(right.x, head.bottom(), right.w, header_h);
        let list = Rect::new(right.x, header.bottom(), right.w, (right.bottom() - header.bottom()).max(0) as u32);
        Self { buttons, toolbar: frame.toolbar, tree, head, header, list, status: frame.status }
    }

    fn tree_rows(&self, ctx: Ctx) -> Vec<Rect> {
        let row_h = ctx.px(TREE_ROW_H);
        let x = self.tree.x + ctx.px(6) as i32;
        let w = self.tree.w.saturating_sub(ctx.px(12));
        (0..TREE.len()).map(|index| Rect::new(x, self.tree.y + ctx.px(8) as i32 + (index as u32 * row_h) as i32, w, row_h)).collect()
    }

    /// Столбцы вида: резиновый забирает остаток; если места мало, твёрдые
    /// сжимаются пропорционально, а резиновому остаётся хотя бы 120 точек.
    fn columns(&self, ctx: Ctx, page: Page) -> Vec<Rect> {
        let specs = page.columns();
        let x0 = self.list.x + ctx.px(20) as i32;
        let room = self.list.w.saturating_sub(ctx.px(36));
        let fixed: u32 = specs.iter().map(|(_, w)| ctx.px(*w)).sum();
        let flex_min = ctx.px(120);
        let (num, den) = if fixed + flex_min > room && fixed > 0 { (room.saturating_sub(flex_min), fixed) } else { (1, 1) };
        let widths: Vec<u32> = specs.iter().map(|(_, w)| (u64::from(ctx.px(*w)) * u64::from(num) / u64::from(den)) as u32).collect();
        let used: u32 = widths.iter().sum();
        let flex = room.saturating_sub(used);
        let mut x = x0;
        widths
            .iter()
            .map(|w| {
                let w = if *w == 0 { flex } else { *w };
                let rect = Rect::new(x, self.list.y, w, self.list.h);
                x += w as i32;
                rect
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

struct Manager {
    page: Page,
    view: View,
    filter: Filter,
    selected: Option<usize>,
    top: usize,
    visible: Shared<usize>,
    note: Option<String>,
    root: bool,
}

impl Manager {
    fn new(page: Page) -> Self {
        let mut window = Self {
            page,
            view: View { rows: Vec::new(), summary: String::new(), log: String::new(), error: None },
            filter: Filter::All,
            selected: None,
            top: 0,
            visible: Shared::new(10),
            note: None,
            root: user_progs::uid() == 0,
        };
        window.load(true);
        println(&format!("compmgmt: ready, page {}", page.key()));
        window
    }

    /// Собрать вид узла заново. `announce` — сказать в журнал (смена узла,
    /// «Обновить»); тихое обновление по таймеру молчит.
    fn load(&mut self, announce: bool) {
        let keep = self.selected_key();
        self.view = match self.page {
            Page::Events => events(self.filter),
            Page::Services => services(),
            Page::Devices => devices(),
            Page::Users => users(),
            Page::Disks => disks(announce),
            Page::Gpu => gpu(),
        };
        self.selected = keep.and_then(|key| self.view.rows.iter().position(|row| matches!(row, Row::Item { key: k, .. } if *k == key)));
        if self.selected.is_none() && matches!(self.page, Page::Disks | Page::Services) {
            self.selected = self.view.rows.iter().position(|row| matches!(row, Row::Item { .. }));
        }
        let max_top = self.view.rows.len().saturating_sub(self.visible.get().max(1));
        self.top = self.top.min(max_top);
        if announce {
            println(&format!("compmgmt: page {}: {}", self.page.key(), self.view.log));
        }
    }

    fn set_page(&mut self, page: Page) {
        if page == self.page {
            return;
        }
        self.page = page;
        self.selected = None;
        self.top = 0;
        self.note = None;
        self.load(true);
    }

    fn selected_key(&self) -> Option<String> {
        match self.view.rows.get(self.selected?) {
            Some(Row::Item { key, .. }) if !key.is_empty() => Some(key.clone()),
            _ => None,
        }
    }

    fn scroll(&mut self, delta: isize) {
        let max_top = self.view.rows.len().saturating_sub(self.visible.get().max(1));
        self.top = (self.top as isize + delta).clamp(0, max_top as isize) as usize;
    }

    /// Выбрать следующую/предыдущую строку-элемент и показать её.
    fn step_row(&mut self, forward: bool) {
        let items: Vec<usize> = self.view.rows.iter().enumerate().filter(|(_, row)| matches!(row, Row::Item { .. })).map(|(i, _)| i).collect();
        if items.is_empty() {
            return;
        }
        let next = match self.selected.and_then(|at| items.iter().position(|i| *i == at)) {
            Some(pos) if forward => items[(pos + 1).min(items.len() - 1)],
            Some(pos) => items[pos.saturating_sub(1)],
            None => items[0],
        };
        self.selected = Some(next);
        let visible = self.visible.get().max(1);
        if next < self.top {
            self.top = next;
        } else if next >= self.top + visible {
            self.top = next + 1 - visible;
        }
    }

    fn act(&mut self, act: Act) {
        match act {
            Act::Refresh => {
                self.load(true);
                self.note = Some(String::from("Обновлено"));
            }
            Act::Tool(program) => self.launch(&format!("/bin/{program}")),
            Act::Filter(filter) => {
                self.filter = filter;
                self.top = 0;
                self.selected = None;
                self.load(true);
            }
            Act::Restart => self.restart(),
            Act::OpenFiles => match self.selected_key() {
                Some(point) => self.launch(&format!("/bin/files {point}")),
                None => self.note = Some(String::from("Выберите том в списке")),
            },
            Act::Cleanup => self.launch("/bin/cleanmgr"),
            Act::Settings(section) => {
                let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
                let got = kernel_command(&format!("ui open settings {section}"), &mut buffer);
                if got >= 0 {
                    println(&format!("compmgmt: opened settings {section}"));
                    self.note = Some(String::from("«Параметры» открыты"));
                } else {
                    println(&format!("compmgmt: settings {section} FAILED {got}"));
                    self.note = Some(format!("«Параметры» не открылись: код {got}"));
                }
            }
        }
    }

    /// Главное действие узла — Enter.
    fn primary(&mut self) {
        match self.page {
            Page::Events => self.act(Act::Tool("eventlog")),
            Page::Services => self.act(Act::Restart),
            Page::Devices | Page::Gpu => self.act(Act::Tool("devmgr")),
            Page::Users => self.act(Act::Settings("accounts")),
            Page::Disks => self.act(Act::OpenFiles),
        }
    }

    fn launch(&mut self, command: &str) {
        let id = spawn(command);
        if id >= 0 {
            println(&format!("compmgmt: opened {command}"));
            self.note = Some(format!("Запущено: {command}"));
        } else {
            println(&format!("compmgmt: FAILED opening {command}: {id}"));
            self.note = Some(format!("Не удалось запустить {command}: код {id}"));
        }
    }

    fn restart(&mut self) {
        if !self.root {
            self.note = Some(String::from("Перезапуск служб — только у администратора (root)"));
            return;
        }
        let Some(key) = self.selected_key() else {
            self.note = Some(String::from("Выберите службу в списке"));
            return;
        };
        // Ключ службы: `задача\tимя\tпрограмма\tuid\tgid`.
        let fields: Vec<&str> = key.split('\t').collect();
        let name = fields.get(1).copied().unwrap_or("?");
        match fields.first().and_then(|task| task.parse::<u32>().ok()) {
            Some(task) => {
                let result = user_progs::kill(task);
                if result >= 0 {
                    println(&format!("compmgmt: restarting '{name}': task {task} stopped, init brings it back"));
                    self.note = Some(format!("«{name}»: задача {task} снята, init поднимет службу заново"));
                } else {
                    self.note = Some(format!("Не удалось снять задачу {task}: код {result}"));
                }
            }
            None => {
                let exec = fields.get(2).copied().unwrap_or("");
                let id = match (fields.get(3).and_then(|t| t.parse().ok()), fields.get(4).and_then(|t| t.parse().ok())) {
                    (Some(uid), Some(gid)) => user_progs::spawn_as(exec, uid, gid),
                    _ => spawn(exec),
                };
                if id >= 0 {
                    println(&format!("compmgmt: started '{name}' as {id}"));
                    self.note = Some(format!("«{name}» запущена как задача {id} — без надзора init до перезагрузки"));
                } else {
                    self.note = Some(format!("Не удалось запустить «{name}»: код {id}"));
                }
            }
        }
    }

    fn enabled(&self, act: Act) -> bool {
        match act {
            Act::Restart => self.root && self.selected_key().is_some(),
            Act::OpenFiles => self.selected_key().is_some(),
            _ => true,
        }
    }
}

impl App for Manager {
    fn key(&mut self, code: u32) -> bool {
        let page = self.visible.get().max(1) as isize;
        let index = Page::ALL.iter().position(|p| *p == self.page).unwrap_or(0);
        match code {
            WIN_KEY_UP => self.set_page(Page::ALL[index.saturating_sub(1)]),
            WIN_KEY_DOWN => self.set_page(Page::ALL[(index + 1).min(Page::ALL.len() - 1)]),
            WIN_KEY_PAGE_UP => self.scroll(-page),
            WIN_KEY_PAGE_DOWN => self.scroll(page),
            WIN_KEY_HOME => self.top = 0,
            WIN_KEY_END => self.scroll(isize::MAX / 2),
            ENTER => self.primary(),
            F5 => self.act(Act::Refresh),
            code if code == 'r' as u32 => self.act(Act::Refresh),
            code if code == 'j' as u32 => self.step_row(true),
            code if code == 'k' as u32 => self.step_row(false),
            code if code == 'o' as u32 => self.primary(),
            code if self.page == Page::Events && code == 'a' as u32 => self.act(Act::Filter(Filter::All)),
            code if self.page == Page::Events && code == 'e' as u32 => self.act(Act::Filter(Filter::Errors)),
            code if self.page == Page::Events && code == 'w' as u32 => self.act(Act::Filter(Filter::Warnings)),
            code if ('1' as u32..='6' as u32).contains(&code) => self.set_page(Page::ALL[(code - '1' as u32) as usize]),
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area, self.page);
        if let Some((act, ..)) = plan.buttons.iter().find(|(.., rect)| rect.contains(x, y)) {
            if self.enabled(*act) {
                self.act(*act);
            }
            return true;
        }
        if plan.tree.contains(x, y) {
            if let Some(index) = plan.tree_rows(ctx).iter().position(|rect| rect.contains(x, y)) {
                if let Some(page) = TREE[index].3 {
                    self.set_page(page);
                }
            }
            return true;
        }
        if plan.list.contains(x, y) {
            let index = self.top + ((y - plan.list.y) as u32 / ctx.px(ROW_H)) as usize;
            if matches!(self.view.rows.get(index), Some(Row::Item { .. })) {
                self.selected = Some(index);
            }
            return true;
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        self.scroll(if clicks > 0 { -3 } else { 3 });
        true
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Computer Management\x1fУправление компьютером"))
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        // Журнал и видеокарта — по «Обновить»: журнал прыгал бы под глазами,
        // а команды видеокарты незачем гонять каждые пять секунд.
        if matches!(self.page, Page::Services | Page::Disks | Page::Devices) {
            self.load(false);
            return true;
        }
        false
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area, self.page);

        // Панель с кнопками узла.
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        for (act, icon, label, rect) in &plan.buttons {
            let active = matches!(act, Act::Filter(filter) if *filter == self.filter);
            tool_button(bar, s, *rect, *icon, label, self.enabled(*act), active);
        }

        // Дерево.
        s.fill(plan.tree, ctx.flat(p.panel));
        draw::vline(s, plan.tree.right() - 1, plan.tree.y, plan.tree.h, p.line.color, p.line.alpha);
        let tree_ctx = ctx.on(theme::panel_bg());
        let g = ctx.px(16);
        for ((depth, label, icon, page), rect) in TREE.iter().zip(plan.tree_rows(ctx)) {
            if rect.bottom() > plan.tree.bottom() {
                break;
            }
            let on = *page == Some(self.page);
            if on {
                paint::row(tree_ctx, s, rect, paint::RowState::Selected);
            }
            let x = rect.x + ctx.px(8 + depth * 14) as i32;
            let (text, role, ink) = match page {
                Some(page) => (page.title(), Role::Body, if on { p.ink } else { p.ink2 }),
                None => (*label, if *depth == 0 { Role::Strong } else { Role::Body }, p.ink3),
            };
            let icon_ink = if on { p.acc_ink } else if page.is_some() { p.ink3 } else { p.ink4 };
            iconset::draw(s, *icon, x, rect.y + (rect.h - g) as i32 / 2, g, icon_ink, 255);
            let tx = x + (g + ctx.px(8)) as i32;
            paint::text_clipped(tree_ctx, s, role, tx, paint::baseline(tree_ctx, role, rect), (rect.right() - tx).max(0) as u32, text, ink);
        }

        // Заголовок вида.
        let pad = ctx.px(20) as i32;
        let room = plan.head.w.saturating_sub(pad as u32 * 2);
        let mut hy = plan.head.y + ctx.px(12) as i32;
        let icon_g = ctx.px(22);
        iconset::draw(s, self.page.icon(), plan.head.x + pad, hy + (i32::from(ctx.face(Role::Heading).line) - icon_g as i32) / 2, icon_g, p.acc, 255);
        let title_x = plan.head.x + pad + (icon_g + ctx.px(10)) as i32;
        paint::text_clipped(ctx, s, Role::Heading, title_x, hy, room.saturating_sub(icon_g + ctx.px(10)), self.page.title(), p.ink);
        hy += i32::from(ctx.face(Role::Heading).line) + ctx.px(4) as i32;
        let (summary, ink) = match &self.view.error {
            Some(error) => (error.as_str(), p.warn),
            None => (self.view.summary.as_str(), p.ink3),
        };
        paint::text_clipped(ctx, s, Role::Body, plan.head.x + pad, hy, room, summary, ink);

        // Шапка столбцов.
        let columns = plan.columns(ctx, self.page);
        let cy = paint::baseline(ctx, Role::MonoCaps, plan.header);
        for ((title, _), rect) in self.page.columns().iter().zip(&columns) {
            if rect.w > ctx.px(24) {
                paint::caps(ctx, s, rect.x, cy, title);
            }
        }
        draw::hline(s, plan.header.x + ctx.px(12) as i32, plan.header.bottom() - 1, plan.header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);

        // Строки.
        let row_h = ctx.px(ROW_H);
        let visible = ((plan.list.h / row_h) as usize).max(1);
        self.visible.set(visible);
        let top = self.top.min(self.view.rows.len().saturating_sub(visible));
        for (index, row) in self.view.rows.iter().enumerate().skip(top).take(visible) {
            let at = (index - top) as u32;
            let band = Rect::new(plan.list.x + ctx.px(8) as i32, plan.list.y + (at * row_h) as i32, plan.list.w.saturating_sub(ctx.px(24)), row_h.saturating_sub(ctx.px(2)));
            match row {
                Row::Group(title) => {
                    paint::text_clipped(ctx, s, Role::Strong, columns[0].x, paint::baseline(ctx, Role::Strong, band), band.w, title, p.acc_ink);
                }
                Row::Line(text, tone) => {
                    let ink = tone_ink(ctx, *tone);
                    let room = (band.right() - columns[0].x).max(0) as u32;
                    paint::text_clipped(ctx, s, Role::Mono, columns[0].x, paint::baseline(ctx, Role::Mono, band), room, text, ink);
                }
                Row::Item { icon, cells, .. } => {
                    let on = self.selected == Some(index);
                    if on {
                        paint::row(ctx, s, band, paint::RowState::Selected);
                    }
                    for (column, (cell, rect)) in cells.iter().zip(&columns).enumerate() {
                        let mut x = rect.x;
                        if column == 0 {
                            iconset::draw(s, *icon, x, band.y + (band.h - g) as i32 / 2, g, if on { p.acc_ink } else { p.ink3 }, 255);
                            x += (g + ctx.px(8)) as i32;
                        }
                        draw_cell(ctx, s, cell, *rect, x, band);
                    }
                }
            }
        }
        if self.view.rows.is_empty() {
            let text = if self.view.error.is_some() { "Нет данных" } else { "Пусто" };
            paint::text(ctx, s, Role::Body, columns[0].x, plan.list.y + ctx.px(12) as i32, text, p.ink4);
        }
        if self.view.rows.len() > visible {
            let track = Rect::new(plan.list.right() - ctx.px(10) as i32, plan.list.y, ctx.px(4), (visible as u32 * row_h).min(plan.list.h));
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / self.view.rows.len()) as u32).max(ctx.px(20)).min(track.h);
            let max_top = self.view.rows.len() - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }

        let hint = match self.page {
            Page::Events => "A/E/W — все/ошибки/предупреждения",
            Page::Services => "Enter — перезапустить",
            Page::Disks => "Enter — открыть том в «Файлах»",
            Page::Users => "Enter — учётные записи",
            Page::Devices | Page::Gpu => "Enter — диспетчер устройств",
        };
        let status = self.note.clone().unwrap_or_else(|| {
            format!("{} · 1–6, ↑↓ — узлы · J/K — строки · {hint} · R — обновить{}", self.page.title(), if self.root { "" } else { " · только просмотр" })
        });
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

fn tone_ink(ctx: Ctx, tone: Tone) -> mini_ui::Color {
    let p = ctx.palette;
    match tone {
        Tone::Bad => p.bad_ink,
        Tone::Warn => p.warn,
        Tone::Ok => p.ok_ink,
        Tone::Accent => p.acc_ink,
        Tone::Muted => p.ink3,
    }
}

fn draw_cell(ctx: Ctx, s: &mut Surface, cell: &Cell, column: Rect, x: i32, row: Rect) {
    let p = ctx.palette;
    match cell {
        Cell::Text(text) => kit::cell(ctx, s, column, x, row, Role::Body, text, p.ink),
        Cell::Strong(text) => kit::cell(ctx, s, column, x, row, Role::Strong, text, p.ink),
        Cell::Dim(text) => kit::cell(ctx, s, column, x, row, Role::Body, text, p.ink3),
        Cell::Mono(text) => kit::cell(ctx, s, column, x, row, Role::Mono, text, p.ink2),
        Cell::Chip(label, tone) => {
            let w = paint::chip_width(ctx, label).min(column.w.saturating_sub(ctx.px(8)));
            if w > 0 {
                let chip = Rect::new(x, row.y + (row.h as i32 - ctx.px(22) as i32) / 2, w, ctx.px(22));
                paint::chip(ctx, s, chip, label, *tone);
            }
        }
        Cell::Bar(used, total) => {
            let pct = if *total == 0 { 0 } else { (used.saturating_mul(100) / total) as u32 };
            let tone = if pct >= 90 { Tone::Bad } else if pct >= 75 { Tone::Warn } else { Tone::Accent };
            let label = format!("{pct} %");
            let label_w = ctx.face(Role::Mono).width("100 %") + ctx.px(8);
            let bar_w = column.w.saturating_sub(label_w + ctx.px(12)).min(ctx.px(240));
            if bar_w >= ctx.px(24) {
                let bar = Rect::new(x, row.y + (row.h as i32 - ctx.px(8) as i32) / 2, bar_w, ctx.px(8));
                paint::progress(ctx, s, bar, pct, 100, tone);
            }
            let lx = if bar_w >= ctx.px(24) { x + (bar_w + ctx.px(8)) as i32 } else { x };
            kit::cell(ctx, s, column, lx, row, Role::Mono, &label, if tone == Tone::Bad { p.bad_ink } else { p.ink2 });
        }
    }
}

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str, enabled: bool, active: bool) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, if active { Weight::Normal } else { Weight::Ghost }, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    let ink = if enabled { p.ink } else { p.ink5 };
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, if enabled { p.ink2 } else { p.ink5 }, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, ink);
}

fn item(icon: Fi, key: impl Into<String>, cells: Vec<Cell>) -> Row {
    Row::Item { icon, cells, key: key.into() }
}

fn text(value: impl Into<String>) -> Cell {
    Cell::Text(value.into())
}

fn dim(value: impl Into<String>) -> Cell {
    Cell::Dim(value.into())
}

// ---------------------------------------------------------------------------
// Журнал событий
// ---------------------------------------------------------------------------

/// Уровень строки журнала по словам, как в «Журнале событий».
fn level_of(line: &str) -> Tone {
    let lower = line.to_lowercase();
    if ["panic", "error", "fail", "cannot", "giving up", "fault", "ошибк"].iter().any(|word| lower.contains(word)) {
        Tone::Bad
    } else if ["warn", "timeout", "timed out", "retry", "not found", "missing"].iter().any(|word| lower.contains(word)) {
        Tone::Warn
    } else {
        Tone::Muted
    }
}

fn events(filter: Filter) -> View {
    let text = match kernel_text("dmesg 400") {
        Ok(text) => text,
        Err(error) => {
            return View { rows: Vec::new(), summary: String::new(), log: String::from("no access (root only)"), error: Some(error) };
        }
    };
    let lines: Vec<(&str, Tone)> = text.lines().map(str::trim).filter(|line| !line.is_empty()).map(|line| (line, level_of(line))).collect();
    let errors = lines.iter().filter(|(_, tone)| *tone == Tone::Bad).count();
    let warnings = lines.iter().filter(|(_, tone)| *tone == Tone::Warn).count();
    let mut rows = Vec::new();
    // Новые сверху, как в журнале Windows.
    for (line, tone) in lines.iter().rev() {
        let keep = match filter {
            Filter::All => true,
            Filter::Errors => *tone == Tone::Bad,
            Filter::Warnings => *tone == Tone::Warn,
        };
        if !keep {
            continue;
        }
        let (label, icon) = match tone {
            Tone::Bad => ("Ошибка", Fi::X),
            Tone::Warn => ("Предупреждение", Fi::Warning),
            _ => ("Сведения", Fi::Info),
        };
        rows.push(item(icon, "", vec![Cell::Chip(String::from(label), *tone), Cell::Mono((*line).to_string())]));
    }
    let filter_text = match filter {
        Filter::All => "все",
        Filter::Errors => "ошибки",
        Filter::Warnings => "предупреждения",
    };
    View {
        summary: format!("Журнал ядра этой загрузки · записей: {} · ошибок: {errors} · предупреждений: {warnings} · показаны: {filter_text}", lines.len()),
        log: format!("{} line(s), {errors} error(s), {warnings} warning(s), shown {}", lines.len(), rows.len()),
        rows,
        error: None,
    }
}

// ---------------------------------------------------------------------------
// Службы
// ---------------------------------------------------------------------------

fn services() -> View {
    let tasks = call_text(|buffer| user_progs::tasks(buffer), 64 * 1024);
    let log = kernel_text("dmesg init:").unwrap_or_default();
    let text = read_text("/etc/services", 8 * 1024).or_else(|| read_text("/usr/share/defaults/etc/services", 8 * 1024)).unwrap_or_default();
    let mut rows = Vec::new();
    let mut running = 0;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let mut fields = line.split_whitespace();
        let (Some(name), Some(exec)) = (fields.next(), fields.next()) else { continue };
        let uid = fields.next().unwrap_or("");
        let gid = fields.next().unwrap_or("");
        let task: Option<u32> = tasks.lines().find_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields.get(6) == Some(&exec) && fields.get(2) != Some(&"finished")).then(|| fields[0].parse().ok()).flatten()
        });
        let quoted = format!("'{name}'");
        let last = log.lines().filter(|line| line.contains(&quoted)).last().unwrap_or("");
        let state = match task {
            Some(_) => {
                running += 1;
                ("Работает", Tone::Ok)
            }
            None if last.contains("giving up") => ("Сбой: init сдался", Tone::Bad),
            None if last.contains("restarting") => ("Перезапускается", Tone::Warn),
            None => ("Остановлена", Tone::Muted),
        };
        let key = format!("{}\t{name}\t{exec}\t{uid}\t{gid}", task.map_or_else(String::new, |task| format!("{task}")));
        let who = if uid.is_empty() { String::from("как init") } else { format!("uid {uid}") };
        rows.push(item(
            Fi::Server,
            key,
            vec![
                Cell::Strong(name.to_string()),
                Cell::Chip(String::from(state.0), state.1),
                Cell::Mono(task.map_or_else(|| String::from("—"), |task| format!("#{task}"))),
                dim(format!("{exec} · {who}")),
            ],
        ));
    }
    let count = rows.len();
    let error = (count == 0).then(|| String::from("/etc/services пуст или не найден"));
    View {
        rows,
        summary: format!("Службы /bin/init · всего: {count} · работают: {running}"),
        log: format!("{count} service(s), {running} running"),
        error,
    }
}

// ---------------------------------------------------------------------------
// Устройства
// ---------------------------------------------------------------------------

fn device_state(state: &str) -> (&'static str, Tone) {
    match state {
        "active" => ("Работает", Tone::Ok),
        "idle" => ("Не занято", Tone::Muted),
        "none" => ("Нет драйвера", Tone::Warn),
        "not-needed" => ("Не требуется", Tone::Muted),
        _ => ("—", Tone::Muted),
    }
}

fn devices() -> View {
    let listing = call_text(|buffer| user_progs::devices(buffer), 32 * 1024);
    let mut rows = Vec::new();
    let mut count = 0;
    let mut missing = 0;
    for (bus, title, icon) in [
        ("sys", "Системные устройства", Fi::Cpu),
        ("pci", "Шина PCI", Fi::Layers),
        ("usb", "USB", Fi::Usb),
        ("disk", "Дисковые устройства", Fi::Drive),
    ] {
        let lines: Vec<&str> = listing.lines().filter(|line| line.split('\t').next() == Some(bus)).collect();
        if lines.is_empty() {
            continue;
        }
        rows.push(Row::Group(format!("{title} ({})", lines.len())));
        for line in lines {
            let fields: Vec<&str> = line.split('\t').collect();
            let field = |index: usize| fields.get(index).copied().unwrap_or("");
            let id = if bus == "disk" { format!("{} МиБ", field(2)) } else { field(2).to_string() };
            let state = device_state(field(5));
            if field(5) == "none" {
                missing += 1;
            }
            count += 1;
            rows.push(item(
                icon,
                format!("{bus} {}", field(1)),
                vec![
                    text(field(3)),
                    dim(field(1)),
                    Cell::Mono(id),
                    text(if field(4).is_empty() || field(4) == "-" { "—" } else { field(4) }),
                    Cell::Chip(String::from(state.0), state.1),
                ],
            ));
        }
    }
    let error = (count == 0).then(|| String::from("Ядро не ответило на перепись устройств"));
    View {
        rows,
        summary: format!("Устройств: {count} · без драйвера: {missing}"),
        log: format!("{count} device(s), {missing} without a driver"),
        error,
    }
}

// ---------------------------------------------------------------------------
// Пользователи
// ---------------------------------------------------------------------------

/// Номер из строки `/etc/group`: `имя:gid…` или `имя:x:gid…`.
fn group_names() -> Vec<(u32, String)> {
    let text = read_text("/etc/group", 16 * 1024).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let mut fields = line.trim().split(':');
            let name = fields.next()?;
            let second = fields.next()?;
            let gid = second.parse().ok().or_else(|| fields.next()?.parse().ok())?;
            Some((gid, name.to_string()))
        })
        .collect()
}

fn users() -> View {
    let me = user_progs::uid();
    let groups = group_names();
    let group = |gid: u32| {
        groups.iter().find(|(id, _)| *id == gid).map(|(_, name)| format!("{name} ({gid})")).unwrap_or_else(|| if gid == 0 { String::from("root (0)") } else { format!("{gid}") })
    };
    let mut rows = vec![Row::Group(String::from("Пользователи"))];
    let passwd = read_text("/etc/passwd", 16 * 1024);
    let mut count = 0;
    // root в файле нет никогда (см. `user::session`): он — сама система.
    rows.push(item(
        Fi::ShieldAdmin,
        "user root",
        vec![
            Cell::Strong(String::from(if me == 0 { "root · вы" } else { "root" })),
            Cell::Mono(String::from("0")),
            text(group(0)),
            dim("/root"),
            Cell::Chip(String::from("Администратор"), Tone::Accent),
        ],
    ));
    count += 1;
    // `имя:uid:gid:вид:дом:алгоритм:соль:отпечаток`; отпечаток не читается.
    for line in passwd.as_deref().unwrap_or("").lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
        let fields: Vec<&str> = line.split(':').collect();
        let (Some(name), Some(Ok(uid)), Some(Ok(gid))) = (fields.first(), fields.get(1).map(|t| t.parse::<u32>()), fields.get(2).map(|t| t.parse::<u32>())) else {
            continue;
        };
        if uid == 0 {
            continue;
        }
        let kind = fields.get(3).copied().unwrap_or("");
        let home = fields.get(4).copied().filter(|home| home.starts_with('/')).map_or_else(|| format!("/home/{name}"), String::from);
        let (label, tone) = if kind.contains("admin") || kind.contains("wheel") || kind == "1" { ("Администратор", Tone::Accent) } else { ("Обычный", Tone::Muted) };
        let shown = if uid == me { format!("{name} · вы") } else { (*name).to_string() };
        rows.push(item(Fi::User, format!("user {name}"), vec![Cell::Strong(shown), Cell::Mono(format!("{uid}")), text(group(gid)), dim(home), Cell::Chip(String::from(label), tone)]));
        count += 1;
    }
    if !groups.is_empty() {
        rows.push(Row::Group(String::from("Группы")));
        for (gid, name) in &groups {
            rows.push(item(Fi::Apps, format!("group {name}"), vec![Cell::Strong(name.clone()), Cell::Mono(format!("{gid}")), dim("группа"), dim(""), Cell::Chip(String::from("Группа"), Tone::Muted)]));
        }
    }
    let summary = match &passwd {
        Some(_) => format!("Учётных записей: {count} · групп: {} · вы: uid {me}", groups.len()),
        None if me == 0 => String::from("/etc/passwd не прочитан: живая система без учётных записей — работает root"),
        None => format!("/etc/passwd закрыт для обычного пользователя (права 0640) · вы: uid {me}"),
    };
    View { rows, summary, log: format!("{count} account(s), {} group(s)", groups.len()), error: None }
}

// ---------------------------------------------------------------------------
// Диски и тома
// ---------------------------------------------------------------------------

fn fs_name(kind: &str) -> String {
    match kind {
        "exfat" => String::from("exFAT"),
        "fat" | "fat32" | "vfat" => String::from("FAT32"),
        "btrfs" => String::from("Btrfs"),
        other => other.to_string(),
    }
}

fn volume_icon(point: &str) -> Fi {
    if point.starts_with("/usb") || point.starts_with("/media") {
        Fi::Usb
    } else if point == "/tmp" || point == "/root" || point.starts_with("/proc") || point.starts_with("/dev") {
        Fi::Memory
    } else {
        Fi::Drive
    }
}

fn disks(announce: bool) -> View {
    let listing = call_text(|buffer| user_progs::mounts(buffer), 8 * 1024);
    let mut rows = vec![Row::Group(String::from("Тома"))];
    let mut volumes = 0;
    let (mut all_total, mut all_free) = (0u64, 0u64);
    for line in listing.lines().filter(|line| !line.trim().is_empty()) {
        // `точка\tФС\tвсего|-\tсвободно|-[\tметка]`.
        let fields: Vec<&str> = line.split('\t').collect();
        let point = fields.first().copied().unwrap_or("?");
        let kind = fields.get(1).copied().unwrap_or("?");
        let total = fields.get(2).and_then(|t| t.parse::<u64>().ok());
        let free = fields.get(3).and_then(|t| t.parse::<u64>().ok());
        let label = fields.get(4).map(|t| t.trim()).unwrap_or("");
        volumes += 1;
        if announce {
            let show = |value: Option<u64>| value.map_or_else(|| String::from("-"), |v| format!("{v}"));
            println(&format!("compmgmt: volume {point} {kind} '{label}' {} {}", show(total), show(free)));
        }
        let name = if label.is_empty() { point.to_string() } else { format!("{label} ({point})") };
        let mut cells = vec![Cell::Strong(name), text(if label.is_empty() { "—" } else { label }), text(fs_name(kind))];
        match (total, free) {
            (Some(total), Some(free)) => {
                all_total += total;
                all_free += free;
                cells.push(Cell::Mono(kit::size_text(total)));
                cells.push(Cell::Mono(kit::size_text(free)));
                cells.push(Cell::Bar(total.saturating_sub(free), total));
            }
            _ => {
                cells.push(dim("—"));
                cells.push(dim("—"));
                cells.push(dim("о месте не сообщает"));
            }
        }
        rows.push(item(volume_icon(point), point, cells));
    }
    let devices = call_text(|buffer| user_progs::devices(buffer), 32 * 1024);
    let mut disks = 0;
    let disk_lines: Vec<&str> = devices.lines().filter(|line| line.split('\t').next() == Some("disk")).collect();
    if !disk_lines.is_empty() {
        rows.push(Row::Group(String::from("Физические диски")));
    }
    for line in disk_lines {
        let fields: Vec<&str> = line.split('\t').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or("");
        let size = field(2).parse::<u64>().map_or_else(|_| String::from("—"), |mib| kit::size_text(mib * 1024 * 1024));
        let state = device_state(field(5));
        disks += 1;
        rows.push(item(
            Fi::Disc,
            "",
            vec![Cell::Strong(field(1).to_string()), dim(field(3)), text(if field(4) == "-" { "—" } else { field(4) }), Cell::Mono(size), dim(""), Cell::Chip(String::from(state.0), state.1)],
        ));
    }
    View {
        rows,
        summary: format!("Томов: {volumes} · физических дисков: {disks} · всего {} · свободно {}", kit::size_text(all_total), kit::size_text(all_free)),
        log: format!("{volumes} volume(s), {disks} disk(s)"),
        error: (volumes == 0).then(|| String::from("Ядро не сообщило ни одного тома")),
    }
}

// ---------------------------------------------------------------------------
// Видеокарта и драйверы
// ---------------------------------------------------------------------------

fn gpu() -> View {
    let mut rows = Vec::new();
    let mut raw = Vec::new();
    let mut denied = false;
    let mut command = |line: &str| -> Vec<String> {
        match kernel_text(line) {
            Ok(text) => {
                let lines: Vec<String> = text.lines().map(str::trim).filter(|line| !line.is_empty()).map(String::from).collect();
                for out in &lines {
                    raw.push(format!("{line} > {out}"));
                }
                lines
            }
            Err(error) => {
                denied = true;
                raw.push(format!("{line} > {error}"));
                Vec::new()
            }
        }
    };
    let card_lines = command("gpu");
    let nvk_lines = if cfg!(target_arch = "x86_64") { command("nvk") } else { Vec::new() };
    let output_lines = command("outputs");

    // Видеокарта.
    rows.push(Row::Group(String::from("Видеокарта")));
    let strip = |line: &str| line.trim_start_matches("gpu:").trim().to_string();
    let card = card_lines.iter().map(|line| strip(line)).find(|line| line.starts_with("NVIDIA") || line.contains("no NVIDIA card"));
    let card_text = match &card {
        Some(line) if line.contains("no NVIDIA card") => String::from("не найдена"),
        Some(line) => line.clone(),
        None if denied => String::from("нет доступа к командам ядра"),
        None => String::from("нет сведений"),
    };
    rows.push(item(Fi::Monitor, "", vec![text("Видеокарта NVIDIA"), Cell::Strong(card_text.clone())]));
    for line in card_lines.iter().map(|line| strip(line)) {
        if let Some(memory) = line.strip_prefix("video memory ") {
            rows.push(item(Fi::Memory, "", vec![text("Видеопамять"), text(memory)]));
        } else if line.contains("GSP") {
            let tone = if line.contains("not running") { Tone::Warn } else { Tone::Ok };
            rows.push(item(Fi::Cpu, "", vec![text("GSP-RM"), Cell::Chip(line.clone(), tone)]));
        }
    }
    let pci = call_text(|buffer| user_progs::devices(buffer), 32 * 1024);
    for line in pci.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or("");
        let what = field(3).to_lowercase();
        let display = (field(0) == "pci" && (what.contains("display") || what.contains("vga") || what.contains("3d"))) || (field(0) == "sys" && field(1) == "display");
        if display {
            let driver = if field(4).is_empty() || field(4) == "-" { "без драйвера" } else { field(4) };
            rows.push(item(Fi::Layers, "", vec![text(if field(0) == "sys" { "Вывод на экран" } else { "Видеоадаптер PCI" }), text(format!("{} · {} · {} · {}", field(3), field(1), field(2), driver))]));
        }
    }

    // NVK и Vulkan.
    rows.push(Row::Group(String::from("Драйвер NVK и Vulkan")));
    let mut info = user_abi::Stat::default();
    let node_file = stat(NODE_PATH, &mut info) >= 0;
    let node_line = nvk_lines.iter().find(|line| line.contains("renderD128")).cloned();
    let node_present = node_line.as_deref().map_or(node_file, |line| line.contains(" present"));
    rows.push(item(
        Fi::Cpu,
        "",
        vec![text("Узел рендеринга"), Cell::Chip(String::from(if node_present { "/dev/dri/renderD128 есть" } else { "/dev/dri/renderD128 нет" }), if node_present { Tone::Ok } else { Tone::Muted })],
    ));
    if let Some(line) = &node_line {
        if let (Some(open), Some(end)) = (line.find('('), line.rfind(')')) {
            rows.push(item(Fi::Sliders, "", vec![text("Модули NVK"), text(&line[open + 1..end])]));
        }
    }
    if let Some(line) = nvk_lines.iter().find(|line| line.contains("local memory")) {
        rows.push(item(Fi::Memory, "", vec![text("Локальная память NVK"), text(line.trim_start_matches("nvk:").trim())]));
    }
    let mut icds = 0;
    for dir in ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"] {
        for name in list_files(dir).into_iter().filter(|name| name.ends_with(".json")) {
            let path = format!("{dir}/{name}");
            let library = read_text(&path, 8 * 1024).and_then(|json| json_string(&json, "library_path")).unwrap_or_else(|| String::from("library_path не найден"));
            rows.push(item(Fi::Package, "", vec![text(format!("Vulkan ICD {name}")), dim(format!("{library} · {dir}"))]));
            icds += 1;
        }
    }
    if icds == 0 {
        rows.push(item(Fi::Package, "", vec![text("Драйверы Vulkan (ICD)"), dim("не найдены в /usr/share/vulkan/icd.d и /etc/vulkan/icd.d")]));
    }

    // Мониторы.
    rows.push(Row::Group(String::from("Мониторы")));
    let mut monitors = 0;
    for line in &output_lines {
        if let Some((index, rest)) = line.split_once(": ").filter(|(index, _)| index.parse::<u32>().is_ok()) {
            monitors += 1;
            rows.push(item(Fi::Monitor, "", vec![text(format!("Монитор {index}")), text(rest)]));
        } else if let Some(rest) = line.strip_prefix("desktop ") {
            rows.push(item(Fi::Laptop, "", vec![text("Рабочий стол"), text(rest)]));
        } else if line.contains("none in the registry") {
            rows.push(item(Fi::Monitor, "", vec![text("Мониторы"), dim("один экран прошивки, реестра нет")]));
        }
    }

    rows.push(Row::Group(String::from("Вывод команд ядра")));
    for line in raw {
        let tone = level_of(&line);
        rows.push(Row::Line(line, if tone == Tone::Muted { Tone::Muted } else { tone }));
    }
    let card_log = match &card {
        Some(line) if line.contains("no NVIDIA card") => String::from("no NVIDIA card"),
        Some(line) => line.clone(),
        None => String::from("unknown"),
    };
    let summary_log = format!("card {card_log}, nvk node {}, {icds} icd, {monitors} output(s)", if node_present { "present" } else { "absent" });
    println(&format!("compmgmt: gpu: {summary_log}"));
    View {
        rows,
        summary: format!("NVIDIA: {card_text} · узел NVK: {} · Vulkan ICD: {icds} · мониторов: {monitors}", if node_present { "есть" } else { "нет" }),
        log: summary_log,
        error: denied.then(|| String::from("Часть сведений видна только администратору (root)")),
    }
}

/// Значение строкового ключа JSON без разбора всего файла: `"ключ": "значение"`.
fn json_string(json: &str, key: &str) -> Option<String> {
    let at = json.find(&format!("\"{key}\""))?;
    let rest = &json[at + key.len() + 2..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

// ---------------------------------------------------------------------------
// Ввод-вывод
// ---------------------------------------------------------------------------

fn kernel_text(line: &str) -> Result<String, String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = kernel_command(line, &mut buffer);
    if got < 0 {
        return Err(match got {
            user_abi::ERR_PERMISSION => String::from("Журнал и команды ядра видны только администратору (root)"),
            user_abi::ERR_AGAIN => String::from("Ядро занято другой командой — нажмите «Обновить»"),
            code => format!("Команда ядра не выполнена: код {code}"),
        });
    }
    Ok(String::from_utf8_lossy(&buffer[..got as usize]).to_string())
}

fn call_text(call: impl FnOnce(&mut [u8]) -> i64, size: usize) -> String {
    let mut buffer = vec![0u8; size];
    let got = call(&mut buffer);
    if got <= 0 {
        return String::new();
    }
    String::from_utf8_lossy(&buffer[..(got as usize).min(size)]).to_string()
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

fn list_files(path: &str) -> Vec<String> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = user_progs::Dirent::default();
    while user_progs::readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if !name.starts_with('.') && entry.kind != KIND_DIRECTORY {
            out.push(name);
        }
    }
    close(fd);
    out.sort();
    out
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: ядро кладёт `argc` указателей на строки, живущие всю программу.
    let args = unsafe { Args::new(argc, argv) };
    let mut page = Page::Events;
    let mut index = 1;
    while let Some(arg) = args.get(index) {
        let value = if arg == "--page" {
            index += 1;
            args.get(index)
        } else {
            arg.strip_prefix("--page=")
        };
        if let Some(found) = value.and_then(Page::named) {
            page = found;
        }
        index += 1;
    }
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (1040, 660));
    app::run(&SPEC, &info, size, move |_, _| Manager::new(page))
}
