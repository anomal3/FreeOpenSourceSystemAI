//! Диспетчер задач: процессы, производительность, службы — окно, из которого
//! задачу можно снять (с фазы 69c — по макету Романа).
//!
//! # Откуда он взялся
//!
//! Из монитора системы (`sysmon`, фаза 47b). Роман посмотрел на стол глазами и
//! спросил ровно то, что спросил бы человек с Windows: где диспетчер задач, где
//! «снять задачу», где «открыть расположение файла». С фазы 69c окно устроено
//! как в его макете: слева разделы (Процессы, Производительность,
//! Автозагрузка, Пользователи, Подробности, Службы, Журнал загрузки), справа —
//! таблица процессов по группам с долей процессора, памятью и числом
//! переключений, графики, службы и журнал ядра.
//!
//! # Что здесь из ядра
//!
//! Список задач — `SYS_TASKS` строками; с 69c в строке ещё число переключений
//! и страницы памяти программы. Доля процессора — не число из ядра, а разность:
//! сколько миллисекунд задача провела на процессоре между двумя обновлениями,
//! делённое на прошедшее время и число процессоров (их столько, сколько задач
//! `idle`). Снятие — `SYS_KILL`, просьба, а не приказ. Журнал загрузки —
//! `dmesg` через `SYS_KERNEL_COMMAND`, и только root: журнал видит всю машину.
//!
//! # Чего окно не умеет и говорит об этом
//!
//! «Диск» и «Сеть» (с 02.11) — тоже разности: ядро отдаёт байты, которые
//! задача за жизнь прочла и записала на носитель (на уровне драйвера, чтение
//! из кэша не в счёт) и отдала/получила по UDP и TCP. Запись, которую позже
//! сбросит фоновая задача, видна у неё, а не у программы.
//!
//! Запустить или остановить службу отсюда нельзя
//! — её перезапустил бы `/bin/init`; раздел «Службы» говорит об этом словами.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit::{self, Entry};
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_MENU,
    WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, kernel_command, kill,
    monotonic_ms, nanosleep, open, println, read, spawn, sysinfo, tasks,
};

const SPEC: Spec = Spec { name: "taskmgr", title: "Task Manager", period_ms: 2_000 };

const TASKS_LIMIT: usize = 16 * 1024;
const FILE_LIMIT: usize = 8 * 1024;
/// Сколько точек истории держит график: две минуты при обновлении раз в две
/// секунды.
const HISTORY: usize = 60;

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const REFRESH: u32 = 'r' as u32;
const MENU: u32 = 'm' as u32;
const FIND: u32 = '/' as u32;
const NEW_TASK: u32 = 'n' as u32;

// ---------------------------------------------------------------------------
// Геометрия — точки макета при множителе 1. Те же числа знает стенд
// (`Aim::TaskRow` в xtask): строка таблицы ниже заголовка, шапки и первой
// группы.
// ---------------------------------------------------------------------------

const NAV_W: u32 = 220;
const NAV_W_FOLDED: u32 = 56;
const HEADER_H: u32 = 56;
const HEAD_H: u32 = 46;
const GROUP_H: u32 = 36;
const ROW_H: u32 = 34;
const STATUS_H: u32 = 30;
/// Столбцов в таблице процессов.
const COLUMNS: usize = 6;

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
    const ALL: [Kind; 3] = [Kind::Program, Kind::Service, Kind::Kernel];

    fn group(self) -> &'static str {
        match self {
            Kind::Program => "Приложения",
            Kind::Service => "Фоновые процессы",
            Kind::Kernel => "Процессы ядра",
        }
    }

    fn icon(self) -> (Fi, Color) {
        match self {
            Kind::Program => (Fi::FileCode, Color::rgb(0x82, 0xCB, 0x92)),
            Kind::Service => (Fi::Server, Color::rgb(0x51, 0xCE, 0xC7)),
            Kind::Kernel => (Fi::Cpu, Color::rgb(0x80, 0xBD, 0xFB)),
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|kind| *kind == self).unwrap_or(0)
    }
}

/// Одна задача — строка ответа `SYS_TASKS`.
#[derive(Clone)]
struct Task {
    id: u32,
    name: String,
    state: String,
    cpu_ms: u64,
    kind: Kind,
    uid: Option<u32>,
    path: String,
    /// Переключений на неё (с 69c; у старого ядра поля нет).
    switches: Option<u64>,
    /// Страниц памяти программы.
    pages: Option<u64>,
    /// Байт с носителем и по сети за жизнь задачи (с 02.11; у старого ядра
    /// полей нет).
    disk_bytes: Option<u64>,
    net_bytes: Option<u64>,
    /// Доля процессора за последний промежуток, в десятых процента.
    cpu: u32,
    /// Скорость обмена с носителем и по сети за последний промежуток, байт/с.
    disk_rate: u64,
    net_rate: u64,
}

impl Task {
    /// `id\tимя\tсостояние\tмс\tрод\tuid\tпуть[\tпереключения\tстраницы[\tдиск\tсеть]]`.
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
        let uid = fields.next()?.parse().ok();
        let path = fields.next().unwrap_or("").to_string();
        let switches = fields.next().and_then(|text| text.parse().ok());
        let pages = fields.next().and_then(|text| text.parse().ok());
        let disk_bytes = fields.next().and_then(|text| text.parse().ok());
        let net_bytes = fields.next().and_then(|text| text.parse().ok());
        Some(Self {
            id,
            name,
            state,
            cpu_ms,
            kind,
            uid,
            path,
            switches,
            pages,
            disk_bytes,
            net_bytes,
            cpu: 0,
            disk_rate: 0,
            net_rate: 0,
        })
    }

    /// Имя в таблице: у программы — путь, как в макете (`/bin/count`).
    fn label(&self) -> &str {
        if self.path.is_empty() { &self.name } else { &self.path }
    }

    fn state_title(&self) -> &'static str {
        match self.state.as_str() {
            "running" => "работает",
            "ready" => "готова",
            "blocked" => "ожидает",
            "finished" => "завершена",
            _ => "—",
        }
    }

    fn is_idle(&self) -> bool {
        self.kind == Kind::Kernel && self.name.starts_with("idle")
    }

    fn memory_bytes(&self) -> Option<u64> {
        self.pages.map(|pages| pages * 4096)
    }
}

/// Строка файла служб вместе с тем, что о ней известно из списка задач.
struct Service {
    name: String,
    exec: String,
    uid: Option<u32>,
    running: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Processes,
    Performance,
    Startup,
    Users,
    Details,
    Services,
    BootLog,
}

impl Section {
    const ALL: [Section; 7] = [
        Section::Processes,
        Section::Performance,
        Section::Startup,
        Section::Users,
        Section::Details,
        Section::Services,
        Section::BootLog,
    ];

    fn title(self) -> &'static str {
        match self {
            Section::Processes => "Процессы",
            Section::Performance => "Производительность",
            Section::Startup => "Автозагрузка",
            Section::Users => "Пользователи",
            Section::Details => "Подробности",
            Section::Services => "Службы",
            Section::BootLog => "Журнал загрузки",
        }
    }

    fn icon(self) -> Fi {
        match self {
            Section::Processes => Fi::List,
            Section::Performance => Fi::Activity,
            Section::Startup => Fi::Play,
            Section::Users => Fi::User,
            Section::Details => Fi::Layers,
            Section::Services => Fi::Server,
            Section::BootLog => Fi::FileText,
        }
    }

    /// Слово в журнале — по нему стенд узнаёт, что раздел открылся.
    fn log_name(self) -> &'static str {
        match self {
            Section::Processes => "processes",
            Section::Performance => "performance",
            Section::Startup => "startup",
            Section::Users => "users",
            Section::Details => "details",
            Section::Services => "services",
            Section::BootLog => "bootlog",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|other| *other == self).unwrap_or(0)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    EndTask,
    OpenLocation,
    Refresh,
}

impl MenuItem {
    const ALL: [MenuItem; 3] = [MenuItem::EndTask, MenuItem::OpenLocation, MenuItem::Refresh];
}

const ENTRIES: [Entry<'static>; 3] = [
    Entry { title: "Снять задачу", icon: Some(mini_ui::glyphicon::Icon::Close), danger: true, group: 0, ..Entry::NONE },
    Entry { title: "Открыть расположение файла", icon: Some(mini_ui::glyphicon::Icon::Folder), danger: false, group: 0, ..Entry::NONE },
    Entry { title: "Обновить", icon: Some(mini_ui::glyphicon::Icon::Update), danger: false, group: 1, ..Entry::NONE },
];

struct Menu {
    at: Option<(i32, i32)>,
    selected: usize,
}

/// Окно «Запустить новую задачу».
struct RunDialog {
    text: String,
    error: Option<String>,
}

/// Строка таблицы процессов: заголовок группы или задача.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    Group(Kind, usize),
    Task(usize),
}

/// Что окно рисует в разделе «Производительность» слева.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Meter {
    Cpu,
    Memory,
    Heap,
    Dma,
}

impl Meter {
    const ALL: [Meter; 4] = [Meter::Cpu, Meter::Memory, Meter::Heap, Meter::Dma];

    fn title(self) -> &'static str {
        match self {
            Meter::Cpu => "ЦП",
            Meter::Memory => "Память",
            Meter::Heap => "Куча ядра",
            Meter::Dma => "Память устройств",
        }
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    nav: Rect,
    header: Rect,
    body: Rect,
    status: Rect,
    search: Rect,
    run_button: Rect,
    end_button: Rect,
    burger: Rect,
    /// Разделы свёрнуты до значков — с учётом узкого окна.
    folded: bool,
    /// 24.11: the sections were opened by the burger in a narrow window —
    /// they lie over the content (like Windows 11 Task Manager) instead of
    /// squeezing the table, which then ran left under the labels (clip-3).
    overlay: bool,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, folded: bool) -> Self {
        // Узкое окно (экран 1024, телефон) сворачивает разделы до значков
        // само, как Windows: иначе таблице процессов не остаётся места на
        // имя. Кнопка «бургер» тогда, наоборот, раскрывает их — поверх
        // таблицы (24.11), сама таблица остаётся при узкой полосе.
        let narrow = area.w < ctx.px(NAV_W + 352 + 40 + 200);
        let folded = folded != narrow;
        let overlay = narrow && !folded;
        let nav_w = ctx.px(if folded { NAV_W_FOLDED } else { NAV_W }).min(area.w * 2 / 3);
        let status_h = ctx.px(STATUS_H);
        let nav = Rect::new(area.x, area.y, nav_w, area.h.saturating_sub(status_h));
        let rail = if overlay { ctx.px(NAV_W_FOLDED) } else { nav_w };
        let right_x = area.x + rail as i32;
        let right_w = area.w.saturating_sub(rail);
        let header = Rect::new(right_x, area.y, right_w, ctx.px(HEADER_H));
        let status = Rect::new(area.x, area.bottom() - status_h as i32, area.w, status_h);
        let body = Rect::new(right_x, header.bottom(), right_w, (status.y - header.bottom()).max(0) as u32);
        let pad = ctx.px(16) as i32;
        let bh = ctx.px(32);
        let by = header.y + (header.h as i32 - bh as i32) / 2;
        let end_w = button_w(ctx, "Снять задачу");
        let run_w = button_w(ctx, "Новая задача");
        let end_button = Rect::new(header.right() - pad - end_w as i32, by, end_w, bh);
        let run_button = Rect::new(end_button.x - ctx.px(10) as i32 - run_w as i32, by, run_w, bh);
        let search_w = ctx.px(240);
        let search = Rect::new(run_button.x - ctx.px(10) as i32 - search_w as i32, by, search_w, bh);
        let burger = Rect::new(nav.x + ctx.px(8) as i32, nav.y + ctx.px(12) as i32, ctx.px(40), ctx.px(36));
        Self { nav, header, body, status, search, run_button, end_button, burger, folded, overlay }
    }

    /// Хватает ли шапке места на поле поиска.
    fn has_search(&self, ctx: Ctx) -> bool {
        self.search.x > self.header.x + ctx.px(170) as i32
    }

    fn nav_row(&self, ctx: Ctx, index: usize) -> Rect {
        let y = self.burger.bottom() + ctx.px(10) as i32 + (index as u32 * ctx.px(42)) as i32;
        Rect::new(self.nav.x + ctx.px(8) as i32, y, self.nav.w.saturating_sub(ctx.px(16)), ctx.px(40))
    }

    /// Строки таблицы ниже шапки.
    fn rows_area(&self, ctx: Ctx) -> Rect {
        let top = self.body.y + ctx.px(HEAD_H) as i32;
        Rect::new(self.body.x, top, self.body.w, (self.body.bottom() - top).max(0) as u32)
    }
}

/// Which columns of a table fit a body `body_w` wide (index 0 — the first,
/// always shown; `i + 1` — `widths[i]`): fixed columns give way in the order
/// `drop` until the first one keeps [`FIRST_MIN_W`] points.
fn shown_columns(ctx: Ctx, body_w: u32, widths: &[u32], drop: &[usize]) -> Vec<bool> {
    let mut shown = vec![true; widths.len() + 1];
    let room = body_w.saturating_sub(ctx.px(16 + 12 + FIRST_MIN_W));
    let mut used: u32 = widths.iter().map(|w| ctx.px(*w)).sum();
    for &column in drop {
        if used <= room {
            break;
        }
        if let Some(width) = column.checked_sub(1).and_then(|i| widths.get(i)) {
            shown[column] = false;
            used -= ctx.px(*width);
        }
    }
    shown
}

/// The first column of a table keeps at least this much (layout points).
const FIRST_MIN_W: u32 = 150;

fn button_w(ctx: Ctx, label: &str) -> u32 {
    ctx.face(Role::Body).width(label) + ctx.px(52)
}

/// Колонки таблицы процессов: имя, состояние, ЦП, память, диск, сеть (макет
/// 17; переключения — в «Подробностях»).
fn process_columns(ctx: Ctx, body: Rect) -> [Rect; COLUMNS] {
    let right = body.right() - ctx.px(12) as i32;
    // Числа — по 92, как в макете; «ЦП» уже: больше «100%» там не бывает.
    let mut widths = [ctx.px(100), ctx.px(64), ctx.px(92), ctx.px(92), ctx.px(104)];
    // Узкому окну (экран 1024) не хватает места на имя: первым уходит
    // «Состояние» — в макете его нет, а точка цвета остаётся у имени.
    let numbers: u32 = widths[1..].iter().sum();
    if body.w < numbers + widths[0] + ctx.px(16 + 180 + 12) {
        widths[0] = 0;
    }
    let mut x = right;
    let mut cols = [Rect::new(0, 0, 0, 0); COLUMNS];
    for (index, width) in widths.iter().enumerate().rev() {
        x -= *width as i32;
        cols[index + 1] = Rect::new(x, body.y, *width, body.h);
    }
    let name_x = body.x + ctx.px(16) as i32;
    cols[0] = Rect::new(name_x, body.y, (x - name_x).max(0) as u32, body.h);
    cols
}

// ---------------------------------------------------------------------------
// Состояние окна
// ---------------------------------------------------------------------------

struct Manager {
    section: Section,
    folded: bool,
    tasks: Vec<Task>,
    /// Время на процессоре, байты с носителем и по сети в прошлый раз — по
    /// номерам задач.
    before: Vec<(u32, u64, u64, u64)>,
    before_at: u64,
    cpus: usize,
    selected: Option<u32>,
    collapsed: [bool; 3],
    query: String,
    searching: bool,
    run: Option<RunDialog>,
    menu: Option<Menu>,
    note: Option<String>,
    services: Vec<Service>,
    accounts: Vec<(String, u32)>,
    info: SysInfo,
    cpu_history: Vec<u16>,
    memory_history: Vec<u16>,
    heap_history: Vec<u16>,
    dma_history: Vec<u16>,
    meter: Meter,
    /// Выбранная строка в разделах-списках (службы, автозагрузка, журнал).
    cursor: usize,
    log: Vec<String>,
    log_error: Option<String>,
    last_summary: String,
    /// Прокрутка таблицы открытого раздела (20.11): колесо и полоса двигают
    /// вид, а не выбранную строку.
    scroll: kit::Scroll,
    /// Где указатель над окном (20.11): строка таблицы под ним подсвечена.
    pointer: Option<(i32, i32)>,
    /// Подсвеченная строка — окно перерисовывается при её смене.
    hot_row: Option<usize>,
}

impl Manager {
    fn new(info: SysInfo) -> Self {
        let mut manager = Self {
            section: Section::Processes,
            folded: info.screen_w < 1000,
            tasks: Vec::new(),
            before: Vec::new(),
            before_at: 0,
            cpus: 1,
            selected: None,
            collapsed: [false; 3],
            query: String::new(),
            searching: false,
            run: None,
            menu: None,
            note: None,
            services: Vec::new(),
            accounts: read_accounts(),
            info,
            cpu_history: Vec::new(),
            memory_history: Vec::new(),
            heap_history: Vec::new(),
            dma_history: Vec::new(),
            meter: Meter::Cpu,
            cursor: 0,
            log: Vec::new(),
            log_error: None,
            last_summary: String::new(),
            scroll: kit::Scroll::new(),
            pointer: None,
            hot_row: None,
        };
        manager.refresh();
        manager
    }

    /// Перечитать задачи и службы, пересчитать доли процессора.
    fn refresh(&mut self) {
        let now = monotonic_ms();
        let mut tasks = list_tasks();
        self.cpus = tasks.iter().filter(|task| task.is_idle()).count().max(1);
        let elapsed = now.saturating_sub(self.before_at);
        if self.before_at != 0 && elapsed > 0 {
            for task in &mut tasks {
                if let Some((_, was, disk, net)) = self.before.iter().find(|(id, ..)| *id == task.id) {
                    let spent = task.cpu_ms.saturating_sub(*was);
                    task.cpu = (spent * 1000 / (elapsed * self.cpus as u64)).min(1000) as u32;
                    task.disk_rate = task.disk_bytes.unwrap_or(0).saturating_sub(*disk) * 1000 / elapsed;
                    task.net_rate = task.net_bytes.unwrap_or(0).saturating_sub(*net) * 1000 / elapsed;
                }
            }
        }
        self.before = tasks
            .iter()
            .map(|task| (task.id, task.cpu_ms, task.disk_bytes.unwrap_or(0), task.net_bytes.unwrap_or(0)))
            .collect();
        self.before_at = now;

        // Программы и службы — новые сверху; задачи ядра — по номеру. Сам
        // диспетчер, как самая новая программа, стоит первым — по нему стенд
        // и считает строки.
        tasks.sort_by(|a, b| {
            a.kind.index().cmp(&b.kind.index()).then_with(|| match a.kind {
                Kind::Kernel => a.id.cmp(&b.id),
                _ => b.id.cmp(&a.id),
            })
        });
        self.tasks = tasks;
        if self.selected.is_some_and(|id| !self.tasks.iter().any(|task| task.id == id)) {
            self.selected = None;
        }
        if self.selected.is_none() {
            self.selected = self.tasks.first().map(|task| task.id);
        }
        self.services = list_services(&self.tasks);

        let total = self.cpu_total();
        push(&mut self.cpu_history, total.min(1000) as u16);
        let used = self.info.frames_total.saturating_sub(self.info.frames_free);
        push(&mut self.memory_history, permille(used, self.info.frames_total));
        let heap = self.info.heap_size.saturating_sub(self.info.heap_free);
        push(&mut self.heap_history, permille(heap, self.info.heap_size));
        push(&mut self.dma_history, permille(self.info.dma_used, self.info.dma_total));

        let programs = self.tasks.iter().filter(|task| task.kind == Kind::Program).count();
        let services = self.tasks.iter().filter(|task| task.kind == Kind::Service).count();
        let summary = format!(
            "taskmgr: {} task(s): {programs} program(s), {services} service(s), {} kernel",
            self.tasks.len(),
            self.tasks.len() - programs - services
        );
        if summary != self.last_summary {
            println(&summary);
            self.last_summary = summary;
            // Для стенда: столбцы «Диск»/«Сеть» живые, а не прочерки старого
            // ядра.
            let disk = self.tasks.iter().filter(|task| task.disk_bytes.is_some_and(|bytes| bytes > 0)).count();
            let net = self.tasks.iter().filter(|task| task.net_bytes.is_some_and(|bytes| bytes > 0)).count();
            println(&format!("taskmgr: disk used by {disk} task(s), network by {net}"));
        }
    }

    /// Доля процессора у всех, кроме бездействия, — в десятых процента.
    fn cpu_total(&self) -> u32 {
        self.tasks.iter().filter(|task| !task.is_idle()).map(|task| task.cpu).sum::<u32>().min(1000)
    }

    /// Строки таблицы процессов: группы и задачи, прошедшие поиск.
    fn lines(&self) -> Vec<Line> {
        let query = self.query.to_lowercase();
        let mut out = Vec::new();
        for kind in Kind::ALL {
            let members: Vec<usize> = self
                .tasks
                .iter()
                .enumerate()
                .filter(|(_, task)| task.kind == kind)
                .filter(|(_, task)| query.is_empty() || task.label().to_lowercase().contains(&query))
                .map(|(index, _)| index)
                .collect();
            if members.is_empty() {
                continue;
            }
            out.push(Line::Group(kind, members.len()));
            if !self.collapsed[kind.index()] {
                out.extend(members.into_iter().map(Line::Task));
            }
        }
        out
    }

    /// Задачи в порядке строк — по ним ходят стрелки.
    fn visible_tasks(&self) -> Vec<usize> {
        self.lines()
            .into_iter()
            .filter_map(|line| match line {
                Line::Task(index) => Some(index),
                Line::Group(..) => None,
            })
            .collect()
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.tasks.iter().position(|task| task.id == id)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.selected_index().map(|index| &self.tasks[index])
    }

    fn select_task(&mut self, index: usize) {
        if let Some(task) = self.tasks.get(index) {
            if self.selected != Some(task.id) {
                self.selected = Some(task.id);
                println(&format!("taskmgr: selected #{} '{}'", task.id, task.name));
            }
        }
    }

    /// Сдвинуть выбор по видимым задачам.
    fn step(&mut self, delta: isize) {
        let order = match self.section {
            Section::Details => (0..self.tasks.len()).collect(),
            _ => self.visible_tasks(),
        };
        if order.is_empty() {
            return;
        }
        let at = self.selected_index().and_then(|index| order.iter().position(|other| *other == index)).unwrap_or(0);
        let next = (at as isize + delta).clamp(0, order.len() as isize - 1) as usize;
        self.select_task(order[next]);
        // Выбор клавишей — вид едет за ним (20.11). В «Процессах» строка —
        // это место в списке с заголовками групп, в «Подробностях» — номер.
        let line = match self.section {
            Section::Details => Some(order[next]),
            _ => self.lines().iter().position(|line| *line == Line::Task(order[next])),
        };
        if let Some(line) = line {
            self.scroll.follow(line);
        }
    }

    /// Высота строки таблицы процессов.
    fn line_height(ctx: Ctx, line: &Line) -> u32 {
        match line {
            Line::Group(..) => ctx.px(GROUP_H),
            Line::Task(_) => ctx.px(ROW_H),
        }
    }

    /// Видимая часть таблицы открытого раздела, высота всего содержимого и
    /// шаг строки — для прокрутки (20.11). Высота `0` — листать нечего.
    fn view_metrics(&self, ctx: Ctx, plan: &Plan) -> (Rect, u32, u32) {
        let rows = plan.rows_area(ctx);
        let row_h = ctx.px(ROW_H);
        match self.section {
            Section::Processes => (rows, self.line_y(ctx, &self.lines()).max(0) as u32, row_h),
            Section::Details => (rows, self.tasks.len() as u32 * row_h, row_h),
            Section::Users => (rows, self.user_count() as u32 * row_h, row_h),
            // Под таблицей служб — строка пояснения.
            Section::Services | Section::Startup => {
                let view = Rect::new(rows.x, rows.y, rows.w, rows.h.saturating_sub(ctx.px(34)));
                (view, self.services.len() as u32 * row_h, row_h)
            }
            Section::BootLog => {
                let (view, step) = Self::log_view(ctx, plan);
                (view, self.log.len() as u32 * step, step)
            }
            Section::Performance => (rows, 0, row_h),
        }
    }

    /// Журнал: видимая часть и шаг строки.
    fn log_view(ctx: Ctx, plan: &Plan) -> (Rect, u32) {
        let pad = ctx.px(16);
        let step = (line_h(ctx, Role::Mono) + ctx.px(4) as i32).max(1) as u32;
        let body = plan.body;
        (Rect::new(body.x, body.y + pad as i32, body.w, body.h.saturating_sub(pad * 2)), step)
    }

    /// Сколько разных пользователей у задач — строк раздела «Пользователи».
    fn user_count(&self) -> usize {
        let mut uids: Vec<Option<u32>> = Vec::new();
        for task in &self.tasks {
            if !uids.contains(&task.uid) {
                uids.push(task.uid);
            }
        }
        uids.len()
    }

    /// Полоса прокрутки открытого раздела, если всё не помещается.
    fn bar(&self, ctx: Ctx, plan: &Plan) -> Option<kit::ScrollBar> {
        let (view, content, _) = self.view_metrics(ctx, plan);
        kit::ScrollBar::vertical(ctx, view, content, &self.scroll)
    }

    fn set_section(&mut self, section: Section) {
        if self.section == section {
            return;
        }
        self.section = section;
        self.menu = None;
        self.searching = false;
        self.cursor = 0;
        self.scroll.reset();
        println(&format!("taskmgr: tab {}", section.log_name()));
        match section {
            Section::Services => {
                let running = self.services.iter().filter(|s| s.running.is_some()).count();
                println(&format!("taskmgr: services: {} described, {running} running", self.services.len()));
            }
            Section::BootLog => self.read_log(),
            _ => {}
        }
    }

    /// Журнал загрузки — `dmesg` ядра. Только root: журнал видит всю машину.
    fn read_log(&mut self) {
        let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
        let got = kernel_command("dmesg", &mut buffer);
        if got < 0 {
            self.log.clear();
            self.log_error = Some(match got {
                user_abi::ERR_PERMISSION => String::from("Журнал ядра виден только администратору (root)."),
                user_abi::ERR_AGAIN => String::from("Ядро занято другой командой — нажмите R, чтобы повторить."),
                code => format!("Журнал не прочитан: код {code}."),
            });
            println(&format!("taskmgr: boot log refused: {got}"));
            return;
        }
        let text = core::str::from_utf8(&buffer[..got as usize]).unwrap_or("");
        self.log = text.lines().map(|line| line.trim_end().to_string()).filter(|line| !line.is_empty()).collect();
        self.log_error = None;
        self.cursor = self.log.len().saturating_sub(1);
        println(&format!("taskmgr: boot log, {} line(s)", self.log.len()));
    }

    // -----------------------------------------------------------------------
    // Действия
    // -----------------------------------------------------------------------

    fn end_task(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        if task.kind == Kind::Kernel {
            println(&format!("taskmgr: #{} is a kernel task, not stopping it", task.id));
            self.note = Some(String::from("Задачу ядра снять нельзя: снимаются только программы"));
            return;
        }
        let code = kill(task.id);
        if code < 0 {
            println(&format!("taskmgr: cannot stop #{}: {code}", task.id));
            self.note = Some(format!("Не удалось снять «{}»: {}", task.name, error_text(code)));
            return;
        }
        println(&format!("taskmgr: asked #{} '{}' to stop", task.id, task.name));
        self.note = Some(format!("«{}» (#{}) попросили остановиться", task.name, task.id));
        nanosleep(0, 100_000_000);
        self.refresh();
    }

    fn open_location(&mut self) {
        let Some(task) = self.selected_task().cloned() else {
            return;
        };
        if task.path.is_empty() {
            println(&format!("taskmgr: #{} has no file, it is a kernel task", task.id));
            self.note = Some(String::from("У задачи ядра нет файла на диске"));
            return;
        }
        let dir = parent_of(&task.path);
        let code = spawn(&format!("/bin/files {dir}"));
        if code < 0 {
            println(&format!("taskmgr: cannot open {dir}: {code}"));
            self.note = Some(format!("Не удалось открыть «{dir}»: {}", error_text(code)));
            return;
        }
        println(&format!("taskmgr: opened location {dir} for #{} as #{code}", task.id));
        self.note = Some(format!("Открыто «{dir}» в «Файлах»"));
    }

    /// «Запустить новую задачу»: имя программы, как в `run` оболочки.
    fn run_task(&mut self) {
        let Some(dialog) = self.run.as_mut() else {
            return;
        };
        let raw = dialog.text.trim();
        if raw.is_empty() {
            dialog.error = Some(String::from("Введите имя программы, например count или /bin/mc"));
            return;
        }
        let line = if raw.starts_with('/') { raw.to_string() } else { format!("/bin/{raw}") };
        let code = spawn(&line);
        if code < 0 {
            println(&format!("taskmgr: cannot start '{line}': {code}"));
            dialog.error = Some(format!("Не удаётся запустить «{line}»: {}", error_text(code)));
            return;
        }
        println(&format!("taskmgr: started '{line}' as #{code}"));
        self.note = Some(format!("«{line}» запущена как #{code}"));
        self.run = None;
        nanosleep(0, 100_000_000);
        self.refresh();
        if let Some(index) = self.tasks.iter().position(|task| i64::from(task.id) == code) {
            self.select_task(index);
        }
    }

    fn run_menu_item(&mut self, item: MenuItem) {
        self.close_menu();
        match item {
            MenuItem::EndTask => self.end_task(),
            MenuItem::OpenLocation => self.open_location(),
            MenuItem::Refresh => self.refresh(),
        }
    }

    fn open_menu(&mut self, at: Option<(i32, i32)>) {
        let Some(task) = self.selected_task() else {
            return;
        };
        println(&format!("taskmgr: menu opened for #{} '{}'", task.id, task.name));
        self.menu = Some(Menu { at, selected: 0 });
        self.note = None;
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            println("taskmgr: menu closed");
        }
    }

    fn menu_rect(&self, ctx: Ctx, area: Rect) -> Option<Rect> {
        let menu = self.menu.as_ref()?;
        let size = kit::menu_size(ctx, &ENTRIES);
        let at = menu.at.unwrap_or_else(|| {
            let plan = Plan::new(ctx, area, self.folded);
            let rows = plan.rows_area(ctx);
            let lines = self.lines();
            let at = self
                .selected_index()
                .and_then(|index| lines.iter().position(|line| *line == Line::Task(index)))
                .unwrap_or(0);
            let y = rows.y + self.line_y(ctx, &lines[..at]) - self.scroll.offset() + ctx.px(ROW_H) as i32;
            (rows.x + ctx.px(80) as i32, y)
        });
        Some(kit::place(at, size, area))
    }

    /// Высота куска строк.
    fn line_y(&self, ctx: Ctx, lines: &[Line]) -> i32 {
        lines.iter().map(|line| Self::line_height(ctx, line) as i32).sum()
    }

    /// Строка таблицы процессов под точкой. Строки едут по точкам (20.11):
    /// верх первой — начало таблицы минус прокрутка.
    fn line_at(&self, ctx: Ctx, plan: &Plan, x: i32, y: i32) -> Option<Line> {
        let rows = plan.rows_area(ctx);
        if !rows.contains(x, y) {
            return None;
        }
        let mut top = rows.y - self.scroll.offset();
        for line in self.lines() {
            let h = Self::line_height(ctx, &line) as i32;
            if y >= top && y < top + h {
                return Some(line);
            }
            top += h;
        }
        None
    }

    // -----------------------------------------------------------------------
    // Отрисовка
    // -----------------------------------------------------------------------

    fn draw_nav(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        s.fill(plan.nav, fill);
        draw::vline(s, plan.nav.right() - 1, plan.nav.y, plan.nav.h, p.line.color, p.line.alpha);
        let inner = ctx.on(fill);
        let size = ctx.px(20);
        iconset::draw(s, Fi::Menu, plan.burger.x + (plan.burger.w - size) as i32 / 2, plan.burger.y + (plan.burger.h - size) as i32 / 2, size, p.ink2, 255);
        for (index, section) in Section::ALL.into_iter().enumerate() {
            let rect = plan.nav_row(ctx, index);
            if rect.bottom() > plan.nav.bottom() {
                break;
            }
            let chosen = section == self.section;
            if chosen {
                draw::rounded(s, rect, ctx.px(6), inner.flat(p.btn), 255);
                let mark = Rect::new(rect.x, rect.y + ctx.px(11) as i32, ctx.px(3), rect.h.saturating_sub(ctx.px(22)));
                draw::rounded(s, mark, ctx.px(2), p.acc, 255);
            }
            let ink = if chosen { p.ink } else { p.ink2 };
            let icon_x = rect.x + ctx.px(12) as i32;
            iconset::draw(s, section.icon(), icon_x, rect.y + (rect.h - size) as i32 / 2, size, if chosen { p.acc } else { p.ink2 }, 255);
            if !plan.folded {
                let tx = icon_x + (size + ctx.px(12)) as i32;
                let room = (rect.right() - tx - ctx.px(6) as i32).max(0) as u32;
                paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), room, section.title(), ink);
            }
        }
    }

    fn draw_header(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let pad = ctx.px(16) as i32;
        let title = self.section.title();
        paint::text(ctx, s, Role::Heading, plan.header.x + pad, paint::baseline(ctx, Role::Heading, plan.header), title, p.ink);
        draw::hline(s, plan.header.x, plan.header.bottom() - 1, plan.header.w, p.line.color, p.line.alpha);
        if !matches!(self.section, Section::Processes | Section::Details) {
            return;
        }
        if plan.has_search(ctx) {
            let field = plan.search;
            paint::sunk(ctx, s, field, ctx.px(6));
            if self.searching {
                draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
            }
            let g = ctx.px(16);
            iconset::draw(s, Fi::Search, field.x + ctx.px(10) as i32, field.y + (field.h - g) as i32 / 2, g, p.ink4, 255);
            let tx = field.x + ctx.px(34) as i32;
            let room = (field.right() - tx - ctx.px(8) as i32).max(0) as u32;
            if self.query.is_empty() && !self.searching {
                paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, "Поиск процесса", p.ink5);
            } else {
                paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, &self.query, p.ink);
                if self.searching {
                    let w = ctx.face(Role::Body).width(&self.query).min(room);
                    s.fill(Rect::new(tx + w as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h.saturating_sub(ctx.px(16))), p.acc);
                }
            }
        }
        icon_button(ctx, s, plan.run_button, Fi::Plus, "Новая задача", Weight::Normal);
        let can_end = self.selected_task().is_some_and(|task| task.kind != Kind::Kernel);
        // Макет 17: «Снять задачу» — красная; у задачи ядра гаснет.
        icon_button(ctx, s, plan.end_button, Fi::X, "Снять задачу", if can_end { Weight::Danger } else { Weight::Ghost });
    }

    fn draw_processes(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let cols = process_columns(ctx, plan.body);

        // ── Строки ──────────────────────────────────────────────────────────
        // Первыми (20.11): строки едут по точкам, и край строки, ушедшей
        // вверх, закрывает шапка, нарисованная поверх.
        let (rows, content, _) = self.view_metrics(ctx, plan);
        let lines = self.lines();
        self.scroll.sync_with(rows.h, content, |at| {
            let line = lines.get(at)?;
            Some((self.line_y(ctx, &lines[..at]), Self::line_height(ctx, line)))
        });
        let mut y = rows.y - self.scroll.offset();
        let group_fill = ctx.flat(p.panel);
        for line in &lines {
            let h = Self::line_height(ctx, line);
            let rect = Rect::new(rows.x, y, rows.w, h);
            y += h as i32;
            if rect.bottom() <= rows.y {
                continue;
            }
            if rect.y >= rows.bottom() {
                break;
            }
            match *line {
                Line::Group(kind, count) => {
                    s.fill(rect, group_fill);
                    draw::hline(s, rect.x, rect.bottom() - 1, rect.w, p.line.color, p.line.alpha);
                    let g = ctx.px(14);
                    let chevron = if self.collapsed[kind.index()] { Fi::ChevronRight } else { Fi::ChevronDown };
                    iconset::draw(s, chevron, rect.x + ctx.px(14) as i32, rect.y + (rect.h - g) as i32 / 2, g, p.ink3, 255);
                    let tx = rect.x + ctx.px(36) as i32;
                    let w = paint::text(ctx, s, Role::Strong, tx, paint::baseline(ctx, Role::Strong, rect), kind.group(), p.ink);
                    paint::text(ctx, s, Role::Body, tx + w as i32 + ctx.px(8) as i32, paint::baseline(ctx, Role::Body, rect), &format!("({count})"), p.ink4);
                }
                Line::Task(index) => self.draw_task_row(s, ctx, rect, &cols, &self.tasks[index]),
            }
        }
        if self.tasks.is_empty() {
            let rect = Rect::new(rows.x + ctx.px(16) as i32, rows.y, rows.w, ctx.px(ROW_H));
            paint::text(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, rect), "Список задач пуст — ядро не ответило", p.bad_ink);
        } else if lines.is_empty() {
            let rect = Rect::new(rows.x + ctx.px(16) as i32, rows.y, rows.w, ctx.px(ROW_H));
            paint::text(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, rect), "Ничего не нашлось", p.ink4);
        }

        // ── Шапка: итоги сверху, имена колонок снизу ─────────────────────────
        let head = Rect::new(plan.body.x, plan.body.y, plan.body.w, ctx.px(HEAD_H));
        s.fill(head, theme::window_bg());
        draw::hline(s, head.x, head.bottom() - 1, head.w, p.line.color, p.line.alpha);
        let total_cpu = self.cpu_total();
        let used = self.info.frames_total.saturating_sub(self.info.frames_free);
        let known = |field: fn(&Task) -> Option<u64>| self.tasks.iter().any(|task| field(task).is_some());
        let totals = [
            String::new(),
            String::new(),
            percent_text(total_cpu),
            percent_text(u32::from(permille(used, self.info.frames_total))),
            if known(|task| task.disk_bytes) { disk_rate_text(self.tasks.iter().map(|task| task.disk_rate).sum()) } else { String::new() },
            if known(|task| task.net_bytes) { net_rate_text(self.tasks.iter().map(|task| task.net_rate).sum()) } else { String::new() },
        ];
        let names = ["Имя", "Состояние", "ЦП", "Память", "Диск", "Сеть"];
        let top = head.y + ctx.px(6) as i32;
        let bottom = head.bottom() - ctx.px(6) as i32 - line_h(ctx, Role::Caption);
        for (index, col) in cols.iter().enumerate() {
            if col.w == 0 {
                continue;
            }
            let right_aligned = index >= 2;
            let name = names[index];
            if right_aligned {
                paint::text_right(ctx, s, Role::Caption, col.right() - ctx.px(10) as i32, bottom, name, p.ink4);
                if !totals[index].is_empty() {
                    paint::text_right(ctx, s, Role::Mono, col.right() - ctx.px(10) as i32, top, &totals[index], p.ink);
                }
            } else {
                let x = if index == 0 { col.x + ctx.px(18) as i32 } else { col.x };
                paint::text(ctx, s, Role::Caption, x, bottom, name, p.ink4);
            }
        }
    }

    fn draw_task_row(&self, s: &mut Surface, ctx: Ctx, rect: Rect, cols: &[Rect; COLUMNS], task: &Task) {
        let p = ctx.palette;
        let selected = Some(task.id) == self.selected;
        if selected {
            draw::blend_rect(s, rect, p.acctint.color, p.acctint.alpha);
            draw::vline(s, rect.x, rect.y, rect.h, p.acc, 255);
        }
        draw::hline(s, rect.x, rect.bottom() - 1, rect.w, p.line.color, p.line.alpha / 2);
        // Тепловая подложка у ЦП и памяти, как в макете: чем больше доля, тем
        // гуще акцент.
        let heat = |s: &mut Surface, col: Rect, permille: u32| {
            if permille > 4 {
                let alpha = (permille.min(500) * 120 / 500 + 20) as u8;
                draw::blend_rect(s, Rect::new(col.x, rect.y, col.w, rect.h), p.acc, alpha);
            }
        };
        // Бездействие не греется: его 97 % — это свободный процессор, а не
        // нагрузка, и самая яркая клетка таблицы не должна значить «всё тихо».
        if !task.is_idle() {
            heat(s, cols[2], task.cpu * 4);
        }
        let mem = task.memory_bytes().map_or(0, |bytes| u32::from(permille(bytes, self.info.frames_total)));
        heat(s, cols[3], mem * 20);

        let (icon, tint) = task.kind.icon();
        let g = ctx.px(18);
        let x0 = cols[0].x + ctx.px(18) as i32;
        iconset::draw(s, icon, x0, rect.y + (rect.h - g) as i32 / 2, g, tint, 255);
        let tx = x0 + (g + ctx.px(10)) as i32;
        let room = (cols[0].right() - tx - ctx.px(8) as i32).max(0) as u32;
        let ink = if task.kind == Kind::Kernel && !selected { p.ink3 } else { p.ink };
        // Путь не влез — имя без каталога: «taskmgr» понятнее, чем «/bi…».
        let label = if ctx.face(Role::Body).width(task.label()) > room { task.name.as_str() } else { task.label() };
        paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, rect), room, label, ink);

        // Состояние: точка цвета и слово.
        let state_ink = match task.state.as_str() {
            "running" => p.acc,
            "ready" => p.ok,
            _ => p.ink5,
        };
        let cy = rect.y + rect.h as i32 / 2;
        if cols[1].w == 0 {
            // Столбец убран узким окном: точка — перед значком.
            draw::circle(s, cols[0].x + ctx.px(6) as i32, cy, ctx.px(3), state_ink, 255);
        } else {
            draw::circle(s, cols[1].x + ctx.px(4) as i32, cy, ctx.px(3), state_ink, 255);
            paint::text_clipped(ctx, s, Role::Body, cols[1].x + ctx.px(14) as i32, paint::baseline(ctx, Role::Body, rect), cols[1].w.saturating_sub(ctx.px(14)), task.state_title(), p.ink2);
        }

        let right = |col: Rect| col.right() - ctx.px(10) as i32;
        let base = paint::baseline(ctx, Role::Mono, rect);
        paint::text_right(ctx, s, Role::Mono, right(cols[2]), base, &percent_text(task.cpu), p.ink2);
        let memory = task.memory_bytes().map_or_else(|| String::from("—"), size_text);
        paint::text_right(ctx, s, Role::Mono, right(cols[3]), base, &memory, p.ink2);
        // Ноль — тусклее: в тихой таблице глаз должен цепляться за тех, кто
        // пишет и качает.
        let quiet = |rate: u64| if rate == 0 { p.ink4 } else { p.ink2 };
        let disk = task.disk_bytes.map_or_else(|| String::from("—"), |_| disk_rate_text(task.disk_rate));
        paint::text_right(ctx, s, Role::Mono, right(cols[4]), base, &disk, quiet(task.disk_rate));
        let net = task.net_bytes.map_or_else(|| String::from("—"), |_| net_rate_text(task.net_rate));
        paint::text_right(ctx, s, Role::Mono, right(cols[5]), base, &net, quiet(task.net_rate));
    }

    fn draw_performance(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let body = plan.body;
        let pad = ctx.px(16);
        let list_w = ctx.px(220).min(body.w / 3);
        // ── Слева: четыре счётчика с маленькими графиками ────────────────────
        for (index, meter) in Meter::ALL.into_iter().enumerate() {
            let rect = Rect::new(body.x + pad as i32, body.y + pad as i32 + (index as u32 * ctx.px(74)) as i32, list_w, ctx.px(66));
            if rect.bottom() > body.bottom() {
                break;
            }
            let chosen = meter == self.meter;
            let fill = if chosen { ctx.flat(p.btn) } else { ctx.flat(p.card) };
            draw::rounded(s, rect, ctx.px(8), fill, 255);
            draw::rounded_stroke(s, rect, ctx.px(8), if chosen { p.accedge } else { p.line2.color }, if chosen { 255 } else { p.line2.alpha });
            let spark = Rect::new(rect.x + ctx.px(10) as i32, rect.y + ctx.px(10) as i32, ctx.px(64), rect.h.saturating_sub(ctx.px(20)));
            graph(ctx, s, spark, self.history(meter), false);
            let tx = spark.right() + ctx.px(12) as i32;
            let room = (rect.right() - tx - ctx.px(8) as i32).max(0) as u32;
            paint::text_clipped(ctx, s, Role::Strong, tx, rect.y + ctx.px(14) as i32, room, meter.title(), p.ink);
            paint::text_clipped(ctx, s, Role::Mono, tx, rect.y + ctx.px(36) as i32, room, &self.meter_value(meter), p.ink3);
        }

        // ── Справа: большой график выбранного и сведения ─────────────────────
        let x = body.x + (pad * 2 + list_w) as i32;
        let w = (body.right() - pad as i32 - x).max(0) as u32;
        let mut y = body.y + pad as i32;
        paint::text(ctx, s, Role::Heading, x, y, self.meter.title(), p.ink);
        let value = self.meter_value(self.meter);
        paint::text_right(ctx, s, Role::Mono, x + w as i32, y + ctx.px(6) as i32, &value, p.ink2);
        y += line_h(ctx, Role::Heading) + ctx.px(8) as i32;
        let caption = match self.meter {
            Meter::Cpu => format!("Доля времени на процессоре за 2 минуты · процессоров: {}", self.cpus),
            Meter::Memory => String::from("Занятые кадры физической памяти за 2 минуты"),
            Meter::Heap => String::from("Куча ядра: окна, буферы, таблицы"),
            Meter::Dma => String::from("Память, которую видят устройства: кольца USB, сеть, звук"),
        };
        paint::text_clipped(ctx, s, Role::Caption, x, y, w, &caption, p.ink4);
        y += line_h(ctx, Role::Caption) + ctx.px(8) as i32;
        let graph_h = (body.bottom() - y - ctx.px(130) as i32).max(ctx.px(60) as i32) as u32;
        let big = Rect::new(x, y, w, graph_h);
        graph(ctx, s, big, self.history(self.meter), true);
        y = big.bottom() + ctx.px(16) as i32;

        let info = &self.info;
        let mib = 1024 * 1024;
        let facts: [(&str, String); 6] = [
            ("Время работы", uptime_text(info.uptime_ms)),
            ("Задач", format!("{}", self.tasks.len())),
            ("Процессоров", format!("{}", self.cpus)),
            ("Память", format!("{} / {} МиБ", info.frames_total.saturating_sub(info.frames_free) / mib, info.frames_total / mib)),
            ("Окон на столе", format!("{}", info.windows)),
            ("Кадров собрано", group_digits(info.frames_composed)),
        ];
        let col_w = w / 3;
        for (index, (label, value)) in facts.iter().enumerate() {
            let cx = x + ((index as u32 % 3) * col_w) as i32;
            let cy = y + ((index as u32 / 3) * ctx.px(48)) as i32;
            if cy + ctx.px(40) as i32 > body.bottom() {
                break;
            }
            paint::text_clipped(ctx, s, Role::Caption, cx, cy, col_w, label, p.ink4);
            paint::text_clipped(ctx, s, Role::Title, cx, cy + line_h(ctx, Role::Caption) + ctx.px(2) as i32, col_w, value, p.ink);
        }
    }

    fn history(&self, meter: Meter) -> &[u16] {
        match meter {
            Meter::Cpu => &self.cpu_history,
            Meter::Memory => &self.memory_history,
            Meter::Heap => &self.heap_history,
            Meter::Dma => &self.dma_history,
        }
    }

    fn meter_value(&self, meter: Meter) -> String {
        let info = &self.info;
        let mib = 1024 * 1024;
        match meter {
            Meter::Cpu => percent_text(self.cpu_total()),
            Meter::Memory => {
                let used = info.frames_total.saturating_sub(info.frames_free);
                format!("{} / {} МиБ", used / mib, info.frames_total / mib)
            }
            Meter::Heap => {
                let used = info.heap_size.saturating_sub(info.heap_free);
                format!("{} / {} КиБ", used / 1024, info.heap_size / 1024)
            }
            Meter::Dma => format!("{} / {} КиБ", info.dma_used / 1024, info.dma_total / 1024),
        }
    }

    /// Простая таблица: шапка и строки, у первой колонки — значок.
    /// Строка таблицы под указателем (20.11) — для подсветки. Пока открыто
    /// меню или «Запустить», над полосой и мимо таблицы — никакой.
    fn row_under(&self, ctx: Ctx, plan: &Plan) -> Option<usize> {
        let (x, y) = self.pointer?;
        if self.menu.is_some() || self.run.is_some() || self.scroll.dragging() {
            return None;
        }
        let (view, _, row_h) = self.view_metrics(ctx, plan);
        if !view.contains(x, y) || self.bar(ctx, plan).is_some_and(|bar| bar.contains(x, y)) {
            return None;
        }
        self.scroll.row_at(y, view.y, row_h)
    }

    /// A table of the body: `widths` — fixed columns after the first (layout
    /// points), the first takes what is left. `drop` — which columns (1 is
    /// the first fixed one) give way, in this order, when the window is too
    /// narrow for all of them (24.11): before, the fixed columns ran left
    /// over the first one and the section list (clip-3).
    #[allow(clippy::too_many_arguments)]
    fn draw_table(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, titles: &[&str], widths: &[u32], drop: &[usize], rows: &[(Fi, Color, Vec<String>)], selected: Option<usize>) {
        let p = ctx.palette;
        let body = plan.body;
        let head = Rect::new(body.x, body.y, body.w, ctx.px(HEAD_H));
        let shown = shown_columns(ctx, body.w, widths, drop);
        // Колонки справа налево фиксированной ширины, первая — что осталось.
        let mut cols: Vec<Option<Rect>> = Vec::new();
        let mut x = body.right() - ctx.px(12) as i32;
        for (index, width) in widths.iter().enumerate().rev() {
            if !shown[index + 1] {
                cols.push(None);
                continue;
            }
            x -= ctx.px(*width) as i32;
            cols.push(Some(Rect::new(x, body.y, ctx.px(*width), body.h)));
        }
        let first_x = body.x + ctx.px(16) as i32;
        cols.push(Some(Rect::new(first_x, body.y, (x - first_x).max(0) as u32, body.h)));
        cols.reverse();
        // Строки — первыми и по точкам (20.11); края, ушедшие за вид,
        // закрывают шапка и полоса под таблицей.
        let (area, content, row_h) = self.view_metrics(ctx, plan);
        self.scroll.sync(area.h, content, row_h);
        let at = self.scroll.offset();
        let hot = self.row_under(ctx, plan);
        for index in self.scroll.rows(area.h, row_h, rows.len()) {
            let (icon, tint, cells) = &rows[index];
            let rect = Rect::new(area.x, area.y + (index as u32 * row_h) as i32 - at, area.w, row_h);
            if selected == Some(index) {
                draw::blend_rect(s, rect, p.acctint.color, p.acctint.alpha);
                draw::vline(s, rect.x, rect.y, rect.h, p.acc, 255);
            } else if hot == Some(index) {
                draw::blend_rect(s, rect, p.hover1.color, p.hover1.alpha);
            }
            draw::hline(s, rect.x, rect.bottom() - 1, rect.w, p.line.color, p.line.alpha / 2);
            for (index, (col, text)) in cols.iter().zip(cells).enumerate() {
                let Some(col) = col else { continue };
                let (tx, role, ink) = if index == 0 {
                    let g = ctx.px(18);
                    iconset::draw(s, *icon, col.x, rect.y + (rect.h - g) as i32 / 2, g, *tint, 255);
                    (col.x + ctx.px(28) as i32, Role::Body, p.ink)
                } else {
                    (col.x, if text.starts_with('/') || text.chars().next().is_some_and(|c| c.is_ascii_digit()) { Role::Mono } else { Role::Body }, p.ink3)
                };
                let room = (col.right() - tx - ctx.px(8) as i32).max(0) as u32;
                paint::text_clipped(ctx, s, role, tx, paint::baseline(ctx, role, rect), room, text, ink);
            }
        }
        if rows.is_empty() {
            let rect = Rect::new(area.x + ctx.px(16) as i32, area.y, area.w, row_h);
            paint::text(ctx, s, Role::Body, rect.x, paint::baseline(ctx, Role::Body, rect), "Пусто", p.ink4);
        }
        let below = Rect::new(body.x, area.bottom(), body.w, (body.bottom() - area.bottom()).max(0) as u32);
        s.fill(below, theme::window_bg());
        s.fill(head, theme::window_bg());
        draw::hline(s, head.x, head.bottom() - 1, head.w, p.line.color, p.line.alpha);
        let base = head.bottom() - ctx.px(10) as i32 - line_h(ctx, Role::Caption);
        for (col, title) in cols.iter().zip(titles) {
            let Some(col) = col else { continue };
            let tx = if col.x == first_x { col.x + ctx.px(28) as i32 } else { col.x };
            paint::text_clipped(ctx, s, Role::Caption, tx, base, col.w, title, p.ink4);
        }
    }

    fn user_name(&self, uid: Option<u32>) -> String {
        match uid {
            None => String::from("ядро"),
            Some(0) => String::from("root"),
            Some(uid) => self.accounts.iter().find(|(_, id)| *id == uid).map_or_else(|| format!("uid {uid}"), |(name, _)| name.clone()),
        }
    }

    fn draw_details(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let rows: Vec<(Fi, Color, Vec<String>)> = self
            .tasks
            .iter()
            .map(|task| {
                let (icon, tint) = task.kind.icon();
                (
                    icon,
                    tint,
                    vec![
                        task.name.clone(),
                        format!("{}", task.id),
                        task.state_title().to_string(),
                        self.user_name(task.uid),
                        percent_text(task.cpu),
                        format!("{} мс", group_digits(task.cpu_ms)),
                        task.switches.map_or_else(|| String::from("—"), group_digits),
                        task.memory_bytes().map_or_else(|| String::from("—"), size_text),
                        if task.path.is_empty() { String::from("—") } else { task.path.clone() },
                    ],
                )
            })
            .collect();
        self.draw_table(s, ctx, plan, &["Имя", "ИД", "Состояние", "Пользователь", "ЦП", "Время ЦП", "Переключения", "Память", "Путь"], &[60, 90, 110, 60, 100, 110, 90, 150], &[6, 5, 3, 2, 4, 1, 8, 7], &rows, self.selected_index());
    }

    fn draw_users(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let mut uids: Vec<Option<u32>> = Vec::new();
        for task in &self.tasks {
            if !uids.contains(&task.uid) {
                uids.push(task.uid);
            }
        }
        uids.sort_by_key(|uid| uid.map_or(u64::MAX, u64::from));
        let rows: Vec<(Fi, Color, Vec<String>)> = uids
            .iter()
            .map(|uid| {
                let mine: Vec<&Task> = self.tasks.iter().filter(|task| task.uid == *uid).collect();
                let cpu: u32 = mine.iter().filter(|task| !task.is_idle()).map(|task| task.cpu).sum();
                let memory: u64 = mine.iter().filter_map(|task| task.memory_bytes()).sum();
                let icon = if uid.is_none() { Fi::Cpu } else { Fi::User };
                (
                    icon,
                    Color::rgb(0xF6, 0x9B, 0x94),
                    vec![
                        self.user_name(*uid),
                        uid.map_or_else(|| String::from("—"), |uid| format!("{uid}")),
                        format!("{}", mine.len()),
                        percent_text(cpu.min(1000)),
                        if uid.is_none() { String::from("—") } else { size_text(memory) },
                    ],
                )
            })
            .collect();
        self.draw_table(s, ctx, plan, &["Пользователь", "uid", "Задач", "ЦП", "Память"], &[80, 80, 90, 120], &[1, 2, 3, 4], &rows, None);
    }

    fn draw_services(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, startup: bool) {
        let p = ctx.palette;
        let rows: Vec<(Fi, Color, Vec<String>)> = self
            .services
            .iter()
            .enumerate()
            .map(|(index, service)| {
                let user = service.uid.map_or(String::from("как init"), |uid| self.user_name(Some(uid)));
                let tint = if service.running.is_some() { p.ok } else { p.ink5 };
                let cells = if startup {
                    vec![service.name.clone(), service.exec.clone(), format!("{}", index + 1), user, String::from("Включено")]
                } else {
                    let state = match service.running {
                        Some(id) => format!("Работает (#{id})"),
                        None => String::from("Не запущена"),
                    };
                    let cpu = service
                        .running
                        .and_then(|id| self.tasks.iter().find(|task| task.id == id))
                        .map_or_else(|| String::from("—"), |task| percent_text(task.cpu));
                    vec![service.name.clone(), state, cpu, user, service.exec.clone()]
                };
                (Fi::Server, tint, cells)
            })
            .collect();
        if startup {
            self.draw_table(s, ctx, plan, &["Имя", "Команда", "Порядок", "Пользователь", "Состояние"], &[220, 80, 120, 110], &[2, 3, 4, 1], &rows, Some(self.cursor));
        } else {
            self.draw_table(s, ctx, plan, &["Служба", "Состояние", "ЦП", "Пользователь", "Путь"], &[150, 70, 120, 220], &[4, 3, 2, 1], &rows, Some(self.cursor));
        }
        let note = if startup {
            "Порядок — строки /etc/services: /bin/init запускает их при загрузке и перезапускает после сбоя"
        } else {
            "Запуск и остановка служб — функция запланирована; пока ими управляет /bin/init по /etc/services"
        };
        let rect = Rect::new(plan.body.x + ctx.px(16) as i32, plan.body.bottom() - ctx.px(30) as i32, plan.body.w.saturating_sub(ctx.px(32)), ctx.px(24));
        paint::text_clipped(ctx, s, Role::Caption, rect.x, paint::baseline(ctx, Role::Caption, rect), rect.w, note, p.ink4);
    }

    fn draw_log(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let body = plan.body;
        let pad = ctx.px(16) as i32;
        if let Some(error) = &self.log_error {
            let rect = Rect::new(body.x + pad, body.y + pad, body.w, ctx.px(30));
            iconset::draw(s, Fi::Lock, rect.x, rect.y + ctx.px(5) as i32, ctx.px(20), p.warn, 255);
            paint::text(ctx, s, Role::Body, rect.x + ctx.px(30) as i32, paint::baseline(ctx, Role::Body, rect), error, p.ink2);
            return;
        }
        // Прокрутка по точкам (20.11): колесо листает журнал, курсор стоит.
        let (view, content, step_u) = self.view_metrics(ctx, plan);
        self.scroll.sync(view.h, content, step_u);
        let step = step_u as i32;
        let at = self.scroll.offset();
        for index in self.scroll.rows(view.h, step_u, self.log.len()) {
            let line = &self.log[index];
            let y = view.y + index as i32 * step - at;
            if y < view.y - ctx.px(2) as i32 || y + step > view.bottom() + ctx.px(2) as i32 {
                continue;
            }
            let row = Rect::new(body.x, y - ctx.px(2) as i32, body.w, step as u32);
            if index == self.cursor {
                draw::blend_rect(s, row, p.acctint.color, p.acctint.alpha);
            }
            // «  usb         : текст» — тег цветом акцента, как в макете.
            let trimmed = line.trim_start();
            let (tag, rest) = match trimmed.split_once(" : ") {
                Some((tag, rest)) if tag.len() <= 14 => (tag.trim_end(), rest),
                _ => ("", trimmed),
            };
            let x = body.x + pad;
            let room = (body.right() - x - pad).max(0) as u32;
            if tag.is_empty() {
                paint::text_clipped(ctx, s, Role::Mono, x, y, room, rest, p.ink3);
            } else {
                let tag_w = ctx.px(110);
                paint::text_clipped(ctx, s, Role::Mono, x, y, tag_w, tag, p.acc_ink);
                paint::text_clipped(ctx, s, Role::Mono, x + tag_w as i32, y, room.saturating_sub(tag_w), rest, p.ink2);
            }
        }
    }

    fn draw_run_dialog(&self, s: &mut Surface, ctx: Ctx, area: Rect, dialog: &RunDialog) {
        let p = ctx.palette;
        draw::blend_rect(s, area, Color::rgb(0, 0, 0), 90);
        let w = ctx.px(460).min(area.w.saturating_sub(ctx.px(40)));
        let h = ctx.px(200);
        let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 3, w, h);
        let inner = kit::card(ctx, s, card);
        let pad = ctx.px(20) as i32;
        let mut y = card.y + pad;
        paint::text(inner, s, Role::Title, card.x + pad, y, "Создание задачи", p.ink);
        y += line_h(ctx, Role::Title) + ctx.px(8) as i32;
        paint::text_clipped(inner, s, Role::Caption, card.x + pad, y, w - ctx.px(40), "Введите имя программы: count, mc, /opt/hello/bin/hello", p.ink4);
        y += line_h(ctx, Role::Caption) + ctx.px(12) as i32;
        let field = Rect::new(card.x + pad, y, w - ctx.px(40), ctx.px(34));
        paint::sunk(inner, s, field, ctx.px(6));
        draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
        let tx = field.x + ctx.px(10) as i32;
        paint::text_clipped(inner, s, Role::Mono, tx, paint::baseline(ctx, Role::Mono, field), field.w - ctx.px(20), &dialog.text, p.ink);
        let tw = ctx.face(Role::Mono).width(&dialog.text).min(field.w - ctx.px(20));
        s.fill(Rect::new(tx + tw as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h - ctx.px(16)), p.acc);
        y = field.bottom() + ctx.px(10) as i32;
        if let Some(error) = &dialog.error {
            paint::text_clipped(inner, s, Role::Caption, card.x + pad, y, w - ctx.px(40), error, p.bad_ink);
        }
        let hint = Rect::new(card.x + pad, card.bottom() - pad - ctx.px(20) as i32, w - ctx.px(40), ctx.px(20));
        paint::text_clipped(inner, s, Role::Caption, hint.x, paint::baseline(ctx, Role::Caption, hint), hint.w, "Enter — запустить    Esc — отмена", p.ink4);
    }
}

impl App for Manager {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area, self.folded);
        if plan.overlay {
            // The rail under the opened sections (the content starts right of it).
            s.fill(Rect::new(area.x, area.y, ctx.px(NAV_W_FOLDED), plan.nav.h), ctx.flat(ctx.palette.panel));
        } else {
            self.draw_nav(s, ctx, &plan);
        }
        self.draw_header(s, ctx, &plan);
        match self.section {
            Section::Processes => self.draw_processes(s, ctx, &plan),
            Section::Performance => self.draw_performance(s, ctx, &plan),
            Section::Startup => self.draw_services(s, ctx, &plan, true),
            Section::Users => self.draw_users(s, ctx, &plan),
            Section::Details => self.draw_details(s, ctx, &plan),
            Section::Services => self.draw_services(s, ctx, &plan, false),
            Section::BootLog => self.draw_log(s, ctx, &plan),
        }

        if let Some(bar) = self.bar(ctx, &plan) {
            bar.draw(ctx, s, &self.scroll);
        }
        if plan.overlay {
            // Opened over the content (24.11): a soft shadow on its right edge.
            self.draw_nav(s, ctx, &plan);
            for i in 0..ctx.px(8) {
                let alpha = (40 * (ctx.px(8) - i) / ctx.px(8).max(1)) as u8;
                draw::vline(s, plan.nav.right() + i as i32, plan.nav.y, plan.nav.h, Color::rgb(0, 0, 0), alpha);
            }
        }

        // Строка состояния единого вида (20.11): части через черту.
        let programs = self.tasks.iter().filter(|task| task.kind != Kind::Kernel).count();
        let used = self.info.frames_total.saturating_sub(self.info.frames_free);
        let mut parts = vec![
            format!("Процессов: {}", self.tasks.len()),
            format!("Программ и служб: {programs}"),
            format!("ЦП: {}", percent_text(self.cpu_total())),
            format!("Память: {}", percent_text(u32::from(permille(used, self.info.frames_total)))),
        ];
        if let Some(note) = &self.note {
            parts.push(note.clone());
        }
        let right = match (self.section, self.selected_task()) {
            (Section::Processes | Section::Details, Some(task)) => format!("#{} {}", task.id, task.label()),
            (Section::Processes | Section::Details, None) => String::from("Выберите задачу"),
            _ => String::from("1–7 — разделы    R — обновить"),
        };
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        kit::status_line(ctx, s, plan.status, &parts, &right, 0);

        if let (Some(menu), Some(rect)) = (self.menu.as_ref(), self.menu_rect(ctx, area)) {
            kit::draw_menu(ctx, s, rect, &ENTRIES, menu.selected);
        }
        if let Some(dialog) = &self.run {
            self.draw_run_dialog(s, ctx, area, dialog);
        }
    }

    fn key(&mut self, code: u32) -> bool {
        if let Some(dialog) = self.run.as_mut() {
            match code {
                ESCAPE => self.run = None,
                ENTER => self.run_task(),
                BACKSPACE => {
                    dialog.text.pop();
                    dialog.error = None;
                }
                code if code < 0x0100_0000 => {
                    if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                        if dialog.text.len() < 64 {
                            dialog.text.push(ch);
                        }
                        dialog.error = None;
                    }
                }
                _ => {}
            }
            return true;
        }
        if self.searching {
            match code {
                ESCAPE => {
                    self.searching = false;
                    self.query.clear();
                }
                ENTER => self.searching = false,
                BACKSPACE => {
                    self.query.pop();
                }
                WIN_KEY_UP => self.step(-1),
                WIN_KEY_DOWN => self.step(1),
                code if code < 0x0100_0000 => {
                    if let Some(ch) = char::from_u32(code).filter(|ch| !ch.is_control()) {
                        if self.query.len() < 32 {
                            self.query.push(ch);
                        }
                    }
                }
                _ => return false,
            }
            // Выбор переходит на первую подходящую задачу: искали ведь её.
            if let Some(first) = self.visible_tasks().first().copied() {
                if !self.visible_tasks().contains(&self.selected_index().unwrap_or(usize::MAX)) {
                    self.select_task(first);
                }
            }
            return true;
        }
        if let Some(menu) = self.menu.as_mut() {
            match code {
                WIN_KEY_UP => menu.selected = menu.selected.saturating_sub(1),
                WIN_KEY_DOWN => menu.selected = (menu.selected + 1).min(MenuItem::ALL.len() - 1),
                ENTER => {
                    let item = MenuItem::ALL[menu.selected.min(MenuItem::ALL.len() - 1)];
                    self.run_menu_item(item);
                }
                ESCAPE | WIN_KEY_MENU | MENU => self.close_menu(),
                _ => return false,
            }
            return true;
        }
        // Цифры — разделы по порядку слева.
        if let Some(digit) = char::from_u32(code).and_then(|ch| ch.to_digit(10)) {
            if (1..=Section::ALL.len() as u32).contains(&digit) {
                self.set_section(Section::ALL[digit as usize - 1]);
                return true;
            }
        }
        match code {
            WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                let at = self.section.index();
                let next = if code == WIN_KEY_RIGHT { (at + 1).min(Section::ALL.len() - 1) } else { at.saturating_sub(1) };
                self.set_section(Section::ALL[next]);
            }
            REFRESH => {
                self.refresh();
                if self.section == Section::BootLog {
                    self.read_log();
                }
            }
            NEW_TASK => {
                self.run = Some(RunDialog { text: String::new(), error: None });
                println("taskmgr: run dialog opened");
            }
            _ if matches!(self.section, Section::Performance) => match code {
                WIN_KEY_UP => self.meter = Meter::ALL[Meter::ALL.iter().position(|m| *m == self.meter).unwrap_or(0).saturating_sub(1)],
                WIN_KEY_DOWN => self.meter = Meter::ALL[(Meter::ALL.iter().position(|m| *m == self.meter).unwrap_or(0) + 1).min(3)],
                _ => return false,
            },
            _ if matches!(self.section, Section::Services | Section::Startup | Section::BootLog) => {
                let count = if self.section == Section::BootLog { self.log.len() } else { self.services.len() };
                let last = count.saturating_sub(1);
                match code {
                    WIN_KEY_UP => self.cursor = self.cursor.saturating_sub(1),
                    WIN_KEY_DOWN => self.cursor = (self.cursor + 1).min(last),
                    WIN_KEY_PAGE_UP => self.cursor = self.cursor.saturating_sub(15),
                    WIN_KEY_PAGE_DOWN => self.cursor = (self.cursor + 15).min(last),
                    WIN_KEY_HOME => self.cursor = 0,
                    WIN_KEY_END => self.cursor = last,
                    _ => return false,
                }
                self.scroll.follow(self.cursor);
            }
            _ if !matches!(self.section, Section::Processes | Section::Details) => return false,
            WIN_KEY_UP => self.step(-1),
            WIN_KEY_DOWN => self.step(1),
            WIN_KEY_PAGE_UP => self.step(-10),
            WIN_KEY_PAGE_DOWN => self.step(10),
            WIN_KEY_HOME => self.step(isize::MIN / 2),
            WIN_KEY_END => self.step(isize::MAX / 2),
            WIN_KEY_DELETE => self.end_task(),
            ENTER => self.open_location(),
            FIND => self.searching = true,
            WIN_KEY_MENU | MENU => self.open_menu(None),
            ESCAPE => {
                self.note = None;
                self.query.clear();
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.run.is_some() {
            return true;
        }
        if self.menu.is_some() {
            let hit = self.menu_rect(ctx, area).and_then(|rect| kit::menu_hit(ctx, rect, &ENTRIES, x, y));
            match hit {
                Some(index) => self.run_menu_item(MenuItem::ALL[index]),
                None => self.close_menu(),
            }
            return true;
        }
        let plan = Plan::new(ctx, area, self.folded);
        if plan.burger.contains(x, y) {
            self.folded = !self.folded;
            return true;
        }
        for (index, section) in Section::ALL.into_iter().enumerate() {
            if plan.nav_row(ctx, index).contains(x, y) {
                self.set_section(section);
                // Opened over the content (24.11): a choice closes it, as in Windows.
                if plan.overlay {
                    self.folded = !self.folded;
                }
                return true;
            }
        }
        // A click beside the opened sections closes them first.
        if plan.overlay {
            if !plan.nav.contains(x, y) {
                self.folded = !self.folded;
            }
            return true;
        }
        self.searching = false;
        // Полоса прокрутки (20.11): бегунок тянут, по дорожке листают.
        if let Some(bar) = self.bar(ctx, &plan).filter(|bar| bar.contains(x, y)) {
            bar.press(&self.scroll, x, y);
            return true;
        }
        if matches!(self.section, Section::Processes | Section::Details) {
            if plan.has_search(ctx) && plan.search.contains(x, y) {
                self.searching = true;
                return true;
            }
            if plan.run_button.contains(x, y) {
                self.run = Some(RunDialog { text: String::new(), error: None });
                println("taskmgr: run dialog opened");
                return true;
            }
            if plan.end_button.contains(x, y) {
                self.end_task();
                return true;
            }
        }
        match self.section {
            Section::Processes => match self.line_at(ctx, &plan, x, y) {
                Some(Line::Group(kind, _)) => {
                    let at = kind.index();
                    self.collapsed[at] = !self.collapsed[at];
                    true
                }
                Some(Line::Task(index)) => {
                    self.select_task(index);
                    true
                }
                None => false,
            },
            Section::Details | Section::Services | Section::Startup => {
                let (rows, _, row_h) = self.view_metrics(ctx, &plan);
                if !rows.contains(x, y) {
                    return false;
                }
                let Some(index) = self.scroll.row_at(y, rows.y, row_h) else {
                    return false;
                };
                if self.section == Section::Details {
                    if index < self.tasks.len() {
                        self.select_task(index);
                        self.scroll.follow(index);
                        return true;
                    }
                } else if index < self.services.len() {
                    self.cursor = index;
                    self.scroll.follow(index);
                    return true;
                }
                false
            }
            Section::Performance => {
                let pad = ctx.px(16);
                for (index, meter) in Meter::ALL.into_iter().enumerate() {
                    let rect = Rect::new(plan.body.x + pad as i32, plan.body.y + pad as i32 + (index as u32 * ctx.px(74)) as i32, ctx.px(220).min(plan.body.w / 3), ctx.px(66));
                    if rect.contains(x, y) {
                        self.meter = meter;
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.menu.is_some() {
            self.close_menu();
            return true;
        }
        if self.section != Section::Processes || self.run.is_some() {
            return false;
        }
        let plan = Plan::new(ctx, area, self.folded);
        if let Some(Line::Task(index)) = self.line_at(ctx, &plan, x, y) {
            self.select_task(index);
            self.open_menu(Some((x, y)));
            return true;
        }
        false
    }

    /// Колесо двигает вид, а не выбранную строку (20.11, как в Windows).
    fn wheel_at(&mut self, area: Rect, ctx: Ctx, clicks: i32) -> bool {
        if self.menu.is_some() || self.run.is_some() {
            return false;
        }
        let plan = Plan::new(ctx, area, self.folded);
        let (view, content, line) = self.view_metrics(ctx, &plan);
        self.scroll.wheel(clicks, view.h, content, line)
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area, self.folded);
        self.bar(ctx, &plan).is_some_and(|bar| bar.drag(&self.scroll, x, y))
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.scroll.release()
    }

    /// Полоса прокрутки под указателем шире, строка под ним подсвечена (20.11).
    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area, self.folded);
        let bar = self.bar(ctx, &plan);
        let hot = self.scroll.hover(bar.as_ref(), x, y);
        self.pointer = Some((x, y));
        let row = self.row_under(ctx, &plan);
        let changed = row != self.hot_row;
        self.hot_row = row;
        hot | changed
    }

    fn leave(&mut self) -> bool {
        self.pointer = None;
        let changed = self.hot_row.take().is_some();
        self.scroll.hover(None, 0, 0) | changed
    }

    fn tick(&mut self, info: &SysInfo) -> bool {
        self.info = *info;
        self.refresh();
        true
    }

    fn quits_on_q(&self) -> bool {
        self.menu.is_none() && self.run.is_none() && !self.searching
    }

    /// 24.11: the folded rail, a table with its name column, «Новая задача»
    /// and «Снять задачу» next to the section title, all seven sections.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(600, 400)
    }
}

// ---------------------------------------------------------------------------
// Кирпичи
// ---------------------------------------------------------------------------

fn line_h(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// Кнопка шапки: значок и подпись.
fn icon_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str, weight: Weight) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, weight, "", false);
    let ink = if weight == Weight::Ghost { p.ink5 } else { p.ink };
    let g = ctx.px(16);
    let x = rect.x + ctx.px(14) as i32;
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, ink, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, ink);
}

/// График истории: столбики в долях от 0 до 1000.
fn graph(ctx: Ctx, s: &mut Surface, rect: Rect, samples: &[u16], grid: bool) {
    let p = ctx.palette;
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
    if samples.is_empty() || rect.w < 4 || rect.h < 4 {
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

fn push(history: &mut Vec<u16>, value: u16) {
    if history.len() == HISTORY {
        history.remove(0);
    }
    history.push(value);
}

fn permille(part: u64, whole: u64) -> u16 {
    if whole == 0 { 0 } else { (part.min(whole) * 1000 / whole) as u16 }
}

/// «3,2%», «0%», «12%».
fn percent_text(tenths: u32) -> String {
    if tenths == 0 {
        String::from("0%")
    } else if tenths < 100 {
        format!("{},{}%", tenths / 10, tenths % 10)
    } else {
        format!("{}%", (tenths + 5) / 10)
    }
}

/// «541 КиБ», «1,8 МиБ».
fn size_text(bytes: u64) -> String {
    let kib = bytes / 1024;
    if kib < 1024 {
        format!("{kib} КиБ")
    } else {
        let tenths = kib * 10 / 1024;
        if tenths >= 100 { format!("{} МиБ", tenths / 10) } else { format!("{},{} МиБ", tenths / 10, tenths % 10) }
    }
}

/// «0,1 МБ/с», «12 МБ/с» — как в макете 17 (мегабайт здесь двоичный).
fn disk_rate_text(bytes_per_s: u64) -> String {
    let tenths = bytes_per_s * 10 / (1024 * 1024);
    if tenths >= 100 { format!("{} МБ/с", tenths / 10) } else { format!("{},{} МБ/с", tenths / 10, tenths % 10) }
}

/// «2,1 Мбит/с» — сеть по обычаю в битах и десятичных мегабитах.
fn net_rate_text(bytes_per_s: u64) -> String {
    let tenths = bytes_per_s * 8 / 100_000;
    if tenths >= 100 { format!("{} Мбит/с", tenths / 10) } else { format!("{},{} Мбит/с", tenths / 10, tenths % 10) }
}

/// «1 851» — разряды через пробел.
fn group_digits(value: u64) -> String {
    let digits = format!("{value}");
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push('\u{00a0}');
        }
        out.push(ch);
    }
    out
}

// ---------------------------------------------------------------------------
// Данные из ядра и с диска
// ---------------------------------------------------------------------------

fn list_tasks() -> Vec<Task> {
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(TASKS_LIMIT).is_err() {
        return Vec::new();
    }
    buffer.resize(TASKS_LIMIT, 0u8);
    let got = tasks(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("");
    text.lines().filter_map(Task::parse).filter(|task| task.state != "finished").collect()
}

fn list_services(tasks: &[Task]) -> Vec<Service> {
    let mut out = Vec::new();
    let Some(text) = read_small("/etc/services").or_else(|| read_small("/usr/share/defaults/etc/services")) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split_whitespace();
        let (Some(name), Some(exec)) = (fields.next(), fields.next()) else {
            continue;
        };
        let uid = fields.next().and_then(|text| text.parse().ok());
        let running = tasks.iter().find(|task| task.path == exec && task.state != "finished").map(|task| task.id);
        out.push(Service { name: name.to_string(), exec: exec.to_string(), uid, running });
    }
    out
}

/// Имена пользователей из `/etc/passwd` — без хешей. Файл закрыт для чужих
/// (`0640 root`): тогда в таблице остаются номера.
fn read_accounts() -> Vec<(String, u32)> {
    let Some(text) = read_small("/etc/passwd") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let uid = fields.next()?.parse().ok()?;
            (!name.is_empty()).then(|| (name.to_string(), uid))
        })
        .collect()
}

fn read_small(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(FILE_LIMIT).is_err() {
        close(fd);
        return None;
    }
    buffer.resize(FILE_LIMIT, 0u8);
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    core::str::from_utf8(&buffer[..got]).ok().map(String::from)
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => path[..index].to_string(),
    }
}

fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}

fn error_text(code: i64) -> String {
    match code {
        user_abi::ERR_NOT_FOUND => String::from("такой задачи или файла нет"),
        user_abi::ERR_PERMISSION => String::from("это чужая программа, снять её может только root"),
        user_abi::ERR_UNSUPPORTED => String::from("задачу ядра снять нельзя"),
        user_abi::ERR_TOO_MANY_TASKS => String::from("слишком много задач"),
        _ => format!("код {code}"),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::window_size(&info, app::size::TASKMGR);
    let fresh = sysinfo().unwrap_or(info);
    app::run(&SPEC, &info, size, move |_, _| Manager::new(fresh))
}
