//! Диспетчер задач: процессы, память и службы — окно, из которого задачу можно
//! снять.
//!
//! # Откуда он взялся
//!
//! Из монитора системы (`sysmon`, фаза 47b): тот показывал семь строк
//! счётчиков встроенным шрифтом и ничего не умел, кроме как закрыться. Роман
//! посмотрел на стол глазами (фаза С0 вехи v0.7b) и спросил ровно то, что
//! спросил бы человек с Windows: где диспетчер задач, где «снять задачу», где
//! «открыть расположение файла». Это окно и есть ответ; `sysmon` остаётся
//! рядом, потому что на его окне «System» стоит десяток шагов стенда.
//!
//! # Что здесь из ядра
//!
//! Список задач приходит вызовом `SYS_TASKS` строками — тот самый список,
//! которого `SYS_SYSINFO` себе не взял. Снятие — `SYS_KILL`, и это просьба, а
//! не приказ: программа снимается на ближайшем возврате в третье кольцо, и
//! окно узнаёт об этом при следующем обновлении списка, а не по ответу вызова.
//! Чужую программу снимает только root — так решает ядро, окно лишь
//! показывает отказ словами.
//!
//! Окно и цикл — у `user_progs::app`; панели, вкладки, таблица и меню — у
//! `mini_ui::kit` (фаза С8). Своего здесь — задачи, службы и действия над ними.
//!
//! # Чего окно не умеет и говорит об этом
//!
//! Службы оно только перечисляет: файл `/etc/services` прочитан, у каждой
//! строки найдена живая задача по пути, но запустить или остановить службу
//! отсюда нельзя — супервизор (`/bin/init`) перезапустил бы остановленную, и
//! кнопка «остановить» врала бы. Вкладка об этом говорит подписью «функция
//! запланирована», по правилу вехи.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::glyphicon::Icon;
use mini_ui::kit::{self, Entry, Frame, Table, Tabs};
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_MENU,
    WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, kill, nanosleep, open,
    println, read, spawn, sysinfo, tasks,
};

const SPEC: Spec = Spec { name: "taskmgr", title: "Task Manager", period_ms: 2_000 };

const TASKS_LIMIT: usize = 16 * 1024;
const SERVICES_LIMIT: usize = 8 * 1024;

/// Клавиши с символом.
const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const REFRESH: u32 = 'r' as u32;
const MENU: u32 = 'm' as u32;
const TAB_1: u32 = '1' as u32;
const TAB_2: u32 = '2' as u32;
const TAB_3: u32 = '3' as u32;

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
    fn title(self) -> &'static str {
        match self {
            Kind::Program => "Программа",
            Kind::Service => "Служба",
            Kind::Kernel => "Ядро",
        }
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
}

impl Task {
    /// Разобрать строку `id\tимя\tсостояние\tмс\tрод\tuid\tпуть`.
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
        Some(Self { id, name, state, cpu_ms, kind, uid, path })
    }

    /// Состояние — словом, которое читает человек.
    fn state_title(&self) -> &'static str {
        match self.state.as_str() {
            "running" => "Работает",
            "ready" => "Готова",
            "blocked" => "Ждёт",
            "finished" => "Завершена",
            _ => "—",
        }
    }

    fn icon(&self) -> (Icon, Tone) {
        match self.kind {
            Kind::Program => (Icon::Terminal, Tone::Ok),
            Kind::Service => (Icon::Service, Tone::Accent),
            Kind::Kernel => (Icon::Settings, Tone::Muted),
        }
    }
}

/// Строка файла служб вместе с тем, что о ней известно из списка задач.
struct Service {
    name: String,
    exec: String,
    uid: Option<u32>,
    /// Номер живой задачи с таким путём, если она есть.
    running: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Processes,
    Performance,
    Services,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Processes, Tab::Performance, Tab::Services];
    const TITLES: [&'static str; 3] = ["Процессы", "Производительность", "Службы"];

    fn index(self) -> usize {
        Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0)
    }

    fn log_name(self) -> &'static str {
        match self {
            Tab::Processes => "processes",
            Tab::Performance => "performance",
            Tab::Services => "services",
        }
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

/// Пункты меню для `kit`: снятие — опасное, обновление — отдельной группой.
const ENTRIES: [Entry<'static>; 3] = [
    Entry { title: "Снять задачу", icon: Some(Icon::Close), danger: true, group: 0 },
    Entry { title: "Открыть расположение файла", icon: Some(Icon::Folder), danger: false, group: 0 },
    Entry { title: "Обновить", icon: Some(Icon::Update), danger: false, group: 1 },
];

/// Контекстное меню: где открыто и какой пункт выбран.
struct Menu {
    /// Точка щелчка; `None` — открыто клавишей, и карточка встаёт у строки.
    at: Option<(i32, i32)>,
    selected: usize,
}

/// Столбцы процессов по важности.
const PROCESS_TITLES: [&str; 7] = ["ИМЯ", "НОМЕР", "СОСТОЯНИЕ", "ЦП, МС", "РОД", "ПОЛЬЗОВАТЕЛЬ", "ПУТЬ"];
const SERVICE_TITLES: [&str; 4] = ["СЛУЖБА", "СОСТОЯНИЕ", "ПОЛЬЗОВАТЕЛЬ", "ПУТЬ"];

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    frame: Frame,
    tabs: Tabs,
    table: Table,
    buttons_y: i32,
    button_h: u32,
    end_button: Rect,
    open_button: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, tab: Tab) -> Self {
        let frame = Frame::new(ctx, area);
        let pad = ctx.px(14);
        let tabs = Tabs::new(ctx, frame.toolbar, area.x + pad as i32, &[ctx.px(96), ctx.px(150), ctx.px(80)]);
        let button_h = ctx.px(30);
        let buttons_h = if tab == Tab::Performance { 0 } else { button_h + ctx.px(20) };
        let buttons_y = frame.status.y - (button_h + ctx.px(10)) as i32;
        let table = Table::new(ctx, frame.body).leave_bottom(buttons_h);
        let end_w = paint::chip_width(ctx, "Снять задачу") + ctx.px(40);
        let open_w = paint::chip_width(ctx, "Открыть расположение файла") + ctx.px(40);
        let end_button = Rect::new(area.right() - pad as i32 - end_w as i32, buttons_y, end_w, button_h);
        let open_button = Rect::new(end_button.x - ctx.px(8) as i32 - open_w as i32, buttons_y, open_w, button_h);
        Self { frame, tabs, table, buttons_y, button_h, end_button, open_button }
    }

    fn process_columns(&self, ctx: Ctx) -> Vec<Rect> {
        self.table.columns(
            ctx,
            &[0, ctx.px(70), ctx.px(100), ctx.px(80), ctx.px(100), ctx.px(120), ctx.px(200)],
            ctx.px(200),
        )
    }

    fn service_columns(&self) -> [Rect; 4] {
        let list = self.table.list;
        let left = list.x + 24;
        let total = (list.right() - 22 - left).max(0) as u32;
        let name_w = total / 4;
        let state_w = total / 5;
        let user_w = total / 6;
        let path_w = total.saturating_sub(name_w + state_w + user_w);
        [
            Rect::new(left, list.y, name_w, list.h),
            Rect::new(left + name_w as i32, list.y, state_w, list.h),
            Rect::new(left + (name_w + state_w) as i32, list.y, user_w, list.h),
            Rect::new(left + (name_w + state_w + user_w) as i32, list.y, path_w, list.h),
        ]
    }
}

// ---------------------------------------------------------------------------
// Состояние окна
// ---------------------------------------------------------------------------

struct Manager {
    tab: Tab,
    tasks: Vec<Task>,
    /// Выбранная задача — по номеру, а не по строке: список перечитывается
    /// каждые две секунды, и строка с тем же номером может переехать.
    selected: Option<u32>,
    services: Vec<Service>,
    info: SysInfo,
    menu: Option<Menu>,
    /// Ответ последнего действия — в строке состояния.
    note: Option<String>,
    /// Последняя напечатанная сводка — чтобы не повторять её каждые две
    /// секунды, когда ничего не изменилось.
    last_summary: String,
}

impl Manager {
    fn new(info: SysInfo) -> Self {
        let mut manager = Self {
            tab: Tab::Processes,
            tasks: Vec::new(),
            selected: None,
            services: Vec::new(),
            info,
            menu: None,
            note: None,
            last_summary: String::new(),
        };
        manager.refresh();
        manager
    }

    /// Перечитать задачи и службы.
    fn refresh(&mut self) {
        self.tasks = list_tasks();
        // Программы и службы — первыми, новые сверху; задачи ядра — за ними,
        // по номеру. Человек ищет глазами то, что запустил только что, а не
        // «idle»; и сам диспетчер, как самая новая программа, стоит первым —
        // по нему стенд и считает строки.
        self.tasks.sort_by(|a, b| {
            let rank = |task: &Task| u8::from(task.kind == Kind::Kernel);
            rank(a).cmp(&rank(b)).then_with(|| match a.kind {
                Kind::Kernel => a.id.cmp(&b.id),
                _ => b.id.cmp(&a.id),
            })
        });
        // Выбранная задача, которой больше нет, — это снятая задача: выбор
        // переходит на первую строку, а не пропадает.
        if self.selected.is_some_and(|id| !self.tasks.iter().any(|task| task.id == id)) {
            self.selected = None;
        }
        if self.selected.is_none() {
            self.selected = self.tasks.first().map(|task| task.id);
        }
        self.services = list_services(&self.tasks);

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
        }
    }

    fn selected_index(&self) -> Option<usize> {
        let id = self.selected?;
        self.tasks.iter().position(|task| task.id == id)
    }

    fn selected_task(&self) -> Option<&Task> {
        self.selected_index().map(|index| &self.tasks[index])
    }

    fn select_index(&mut self, index: usize) {
        if let Some(task) = self.tasks.get(index) {
            if self.selected != Some(task.id) {
                self.selected = Some(task.id);
                println(&format!("taskmgr: selected #{} '{}'", task.id, task.name));
            }
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.menu = None;
            println(&format!("taskmgr: tab {}", tab.log_name()));
            if tab == Tab::Services {
                let running = self.services.iter().filter(|s| s.running.is_some()).count();
                println(&format!("taskmgr: services: {} described, {running} running", self.services.len()));
            }
        }
    }

    /// Первая показанная строка таблицы процессов.
    fn first_visible(&self, shown: usize) -> usize {
        kit::first_visible(self.selected_index().unwrap_or(0), shown)
    }

    // -----------------------------------------------------------------------
    // Действия
    // -----------------------------------------------------------------------

    /// Снять выбранную задачу.
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
        // Список перечитывается сразу же: программа снимается на ближайшем
        // возврате в третье кольцо, и ждать две секунды, чтобы увидеть это,
        // незачем — а если она ещё жива, следующее обновление покажет.
        nanosleep(0, 100_000_000);
        self.refresh();
    }

    /// Открыть каталог программы в «Файлах».
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

    /// Где карточка меню.
    fn menu_rect(&self, ctx: Ctx, area: Rect) -> Option<Rect> {
        let menu = self.menu.as_ref()?;
        let size = kit::menu_size(ctx, &ENTRIES);
        let at = menu.at.unwrap_or_else(|| {
            // У клавиатуры точки нет: карточка встаёт у выбранной строки.
            let plan = Plan::new(ctx, area, self.tab);
            let shown = plan.table.shown(self.tasks.len());
            let offset = self.selected_index().unwrap_or(0).saturating_sub(self.first_visible(shown));
            let row = plan.table.row_rect(ctx, offset);
            (row.x + ctx.px(60) as i32, row.bottom())
        });
        Some(kit::place(at, size, area))
    }

    fn row_at(&self, plan: &Plan, x: i32, y: i32) -> Option<usize> {
        let shown = plan.table.shown(self.tasks.len());
        let index = self.first_visible(shown) + plan.table.offset_at(x, y, shown)?;
        (index < self.tasks.len()).then_some(index)
    }

    // -----------------------------------------------------------------------
    // Отрисовка
    // -----------------------------------------------------------------------

    /// Таблица задач и кнопки под ней.
    fn draw_processes(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let cols = plan.process_columns(ctx);
        plan.table.draw_header(ctx, s, &cols, &PROCESS_TITLES);

        let shown = plan.table.shown(self.tasks.len());
        let first = self.first_visible(shown);
        for (offset, task) in self.tasks.iter().skip(first).take(shown).enumerate() {
            let rect = plan.table.row_rect(ctx, offset);
            let selected = Some(task.id) == self.selected;
            paint::row(ctx, s, rect, if selected { RowState::Selected } else { RowState::Idle });
            let side = ctx.px(22);
            let tile = Rect::new(rect.x + ctx.px(6) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            let (icon, tone) = task.icon();
            paint::icon_tile(ctx, s, tile, icon, tone, false);
            let ink = if selected {
                p.ink
            } else if task.kind == Kind::Kernel {
                p.ink4
            } else {
                p.ink2
            };
            let cells = [
                (task.name.clone(), if selected { Role::Title } else { Role::Body }, ink),
                (format!("#{}", task.id), Role::Mono, p.ink4),
                (task.state_title().to_string(), Role::Body, if task.state == "running" { p.ok_ink } else { p.ink3 }),
                (format!("{}", task.cpu_ms), Role::Mono, p.ink4),
                (task.kind.title().to_string(), Role::Body, p.ink3),
                (
                    task.uid.map_or(String::from("—"), |uid| if uid == 0 { String::from("root") } else { format!("uid {uid}") }),
                    Role::Body,
                    p.ink3,
                ),
                (task.path.clone(), Role::Mono, p.ink4),
            ];
            for (index, (col, (text, role, ink))) in cols.iter().zip(cells).enumerate() {
                let x = if index == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                kit::cell(ctx, s, *col, x, rect, role, &text, ink);
            }
        }
        plan.table.draw_overflow(ctx, s, self.tasks.len(), first, shown);
        if self.tasks.is_empty() {
            let rect = plan.table.row_rect(ctx, 0);
            paint::text_clipped(ctx, s, Role::Body, rect.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, rect), rect.w, "Список задач пуст — ядро не ответило", p.bad_ink);
        }

        // Кнопки: «снять» — опасная, потому красная; для задачи ядра она
        // гаснет, а не пропадает.
        let can_end = self.selected_task().is_some_and(|task| task.kind != Kind::Kernel);
        let can_open = self.selected_task().is_some_and(|task| !task.path.is_empty());
        paint::button(ctx, s, plan.end_button, if can_end { Weight::Danger } else { Weight::Ghost }, "Снять задачу", false);
        paint::button(ctx, s, plan.open_button, if can_open { Weight::Normal } else { Weight::Ghost }, "Открыть расположение файла", false);
    }

    /// Память и время — полосами и строками.
    fn draw_performance(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let info = &self.info;
        let area = plan.frame.body;
        let pad = ctx.px(24);
        let x = area.x + pad as i32;
        let room = area.w.saturating_sub(pad * 2).min(ctx.px(640));
        let mut y = area.y + pad as i32;
        for (title, done, total, text, tone) in bars(info) {
            if y + ctx.px(70) as i32 > area.bottom() {
                return;
            }
            paint::caps(ctx, s, x, y, title);
            y += ctx.px(20) as i32;
            let scale = |value: u64| u32::try_from(value / 4096).unwrap_or(u32::MAX);
            paint::progress(ctx, s, Rect::new(x, y, room, ctx.px(14)), scale(done), scale(total), tone);
            y += ctx.px(22) as i32;
            paint::text_clipped(ctx, s, Role::Body, x, y, room, &text, p.ink3);
            y += ctx.px(34) as i32;
        }

        if y + ctx.px(90) as i32 > area.bottom() {
            return;
        }
        paint::caps(ctx, s, x, y, "СИСТЕМА");
        y += ctx.px(20) as i32;
        let step = i32::from(ctx.face(Role::Body).line) + ctx.px(6) as i32;
        let programs = self.tasks.iter().filter(|task| task.kind != Kind::Kernel).count();
        let facts = [
            format!("Время работы: {}", uptime_text(info.uptime_ms)),
            format!("Задач: {} всего, {programs} программ и служб, {} живых по счёту ядра", self.tasks.len(), info.tasks_alive),
            format!("Окон на столе: {}, кадров собрано: {}", info.windows, info.frames_composed),
            format!("Клавиш: {} принято, {} потеряно", info.keys_posted, info.keys_dropped),
        ];
        for fact in facts {
            if y + step > area.bottom() {
                return;
            }
            paint::text_clipped(ctx, s, Role::Body, x, y, room, &fact, p.ink3);
            y += step;
        }
    }

    /// Службы из `/etc/services` и их живые задачи.
    fn draw_services(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let cols = plan.service_columns();
        plan.table.draw_header(ctx, s, &cols, &SERVICE_TITLES);

        let shown = plan.table.shown(self.services.len());
        for (offset, service) in self.services.iter().take(shown).enumerate() {
            let rect = plan.table.row_rect(ctx, offset);
            let side = ctx.px(22);
            let tile = Rect::new(rect.x + ctx.px(6) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            let running = service.running.is_some();
            paint::icon_tile(ctx, s, tile, Icon::Service, if running { Tone::Ok } else { Tone::Muted }, false);
            let state = match service.running {
                Some(id) => format!("Работает (#{id})"),
                None => String::from("Не запущена"),
            };
            let user = service.uid.map_or(String::from("как init"), |uid| if uid == 0 { String::from("root") } else { format!("uid {uid}") });
            let cells = [
                (service.name.clone(), Role::Body, p.ink2),
                (state, Role::Body, if running { p.ok_ink } else { p.ink4 }),
                (user, Role::Body, p.ink3),
                (service.exec.clone(), Role::Mono, p.ink4),
            ];
            for (index, (col, (text, role, ink))) in cols.iter().zip(cells).enumerate() {
                let x = if index == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                kit::cell(ctx, s, *col, x, rect, role, &text, ink);
            }
        }
        plan.table.draw_overflow(ctx, s, self.services.len(), 0, shown);
        if self.services.is_empty() {
            let rect = plan.table.row_rect(ctx, 0);
            paint::text_clipped(ctx, s, Role::Body, rect.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, rect), rect.w, "Файл служб не прочитан", p.ink5);
        }
        // Правило вехи: то, чего нет, названо, а не спрятано.
        let list = plan.table.list;
        let note = Rect::new(list.x + ctx.px(12) as i32, plan.buttons_y, list.w.saturating_sub(ctx.px(24)), plan.button_h);
        paint::text_clipped(ctx, s, Role::Caption, note.x, paint::baseline(ctx, Role::Caption, note), note.w, "Запуск и остановка служб — функция запланирована; пока это делает /bin/init по файлу /etc/services", p.ink4);
    }
}

impl App for Manager {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area, self.tab);

        let bar = kit::toolbar(ctx, s, plan.frame.toolbar);
        plan.tabs.draw(bar, s, &Tab::TITLES, self.tab.index());
        // Справа — сколько задач живо: это то число, ради которого окно
        // открывают чаще всего.
        paint::text_right(
            bar,
            s,
            Role::Mono,
            plan.frame.toolbar.right() - ctx.px(16) as i32,
            paint::baseline(bar, Role::Mono, plan.frame.toolbar),
            &format!("{} задач", self.tasks.len()),
            p.ink3,
        );

        match self.tab {
            Tab::Processes => self.draw_processes(s, ctx, &plan),
            Tab::Performance => self.draw_performance(s, ctx, &plan),
            Tab::Services => self.draw_services(s, ctx, &plan),
        }

        let text = match (&self.note, self.tab) {
            (Some(note), _) => note.clone(),
            (None, Tab::Processes) => String::from("Delete — снять задачу    Enter — расположение файла    правая кнопка — меню    1 2 3 — вкладки"),
            (None, Tab::Performance) => String::from("Числа обновляются каждые две секунды    1 2 3 — вкладки"),
            (None, Tab::Services) => String::from("Службы читаются из /etc/services    1 2 3 — вкладки"),
        };
        kit::status_bar(ctx, s, plan.frame.status, &text);

        if let (Some(menu), Some(rect)) = (self.menu.as_ref(), self.menu_rect(ctx, area)) {
            kit::draw_menu(ctx, s, rect, &ENTRIES, menu.selected);
        }
    }

    fn key(&mut self, code: u32) -> bool {
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
        match code {
            TAB_1 => self.set_tab(Tab::Processes),
            TAB_2 => self.set_tab(Tab::Performance),
            TAB_3 => self.set_tab(Tab::Services),
            WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                let at = self.tab.index();
                let next = if code == WIN_KEY_RIGHT { (at + 1).min(Tab::ALL.len() - 1) } else { at.saturating_sub(1) };
                self.set_tab(Tab::ALL[next]);
            }
            REFRESH => self.refresh(),
            _ if self.tab != Tab::Processes => return false,
            WIN_KEY_UP => {
                let at = self.selected_index().unwrap_or(0);
                self.select_index(at.saturating_sub(1));
            }
            WIN_KEY_DOWN => {
                let at = self.selected_index().unwrap_or(0);
                self.select_index((at + 1).min(self.tasks.len().saturating_sub(1)));
            }
            WIN_KEY_PAGE_UP => {
                let at = self.selected_index().unwrap_or(0);
                self.select_index(at.saturating_sub(10));
            }
            WIN_KEY_PAGE_DOWN => {
                let at = self.selected_index().unwrap_or(0);
                self.select_index((at + 10).min(self.tasks.len().saturating_sub(1)));
            }
            WIN_KEY_HOME => self.select_index(0),
            WIN_KEY_END => self.select_index(self.tasks.len().saturating_sub(1)),
            WIN_KEY_DELETE => self.end_task(),
            ENTER => self.open_location(),
            WIN_KEY_MENU | MENU => self.open_menu(None),
            ESCAPE => self.note = None,
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.menu.is_some() {
            let hit = self.menu_rect(ctx, area).and_then(|rect| kit::menu_hit(ctx, rect, &ENTRIES, x, y));
            match hit {
                Some(index) => self.run_menu_item(MenuItem::ALL[index]),
                // Щелчок мимо карточки закрывает меню и больше ничего не
                // делает: иначе щелчок «мимо» выбирал бы строку под собой.
                None => self.close_menu(),
            }
            return true;
        }
        let plan = Plan::new(ctx, area, self.tab);
        if let Some(index) = plan.tabs.hit(x, y) {
            self.set_tab(Tab::ALL[index]);
            return true;
        }
        if self.tab == Tab::Processes {
            if plan.end_button.contains(x, y) {
                self.end_task();
                return true;
            }
            if plan.open_button.contains(x, y) {
                self.open_location();
                return true;
            }
            if let Some(index) = self.row_at(&plan, x, y) {
                self.select_index(index);
                return true;
            }
        }
        false
    }

    fn click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.menu.is_some() {
            self.close_menu();
            return true;
        }
        if self.tab != Tab::Processes {
            return false;
        }
        let plan = Plan::new(ctx, area, self.tab);
        if let Some(index) = self.row_at(&plan, x, y) {
            self.select_index(index);
            self.open_menu(Some((x, y)));
            return true;
        }
        false
    }

    fn tick(&mut self, info: &SysInfo) -> bool {
        self.info = *info;
        self.refresh();
        true
    }

    /// `q` в открытом меню — не выход: меню закрывается своими клавишами.
    fn quits_on_q(&self) -> bool {
        self.menu.is_none()
    }
}

/// Полосы вкладки «Производительность».
fn bars(info: &SysInfo) -> [(&'static str, u64, u64, String, Tone); 3] {
    let mib = 1024 * 1024;
    let memory_used = info.frames_total.saturating_sub(info.frames_free);
    let heap_used = info.heap_size.saturating_sub(info.heap_free);
    [
        ("ПАМЯТЬ", memory_used, info.frames_total, format!("{} МиБ занято из {}", memory_used / mib, info.frames_total / mib), Tone::Accent),
        ("КУЧА ЯДРА", heap_used, info.heap_size, format!("{} КиБ занято из {}", heap_used / 1024, info.heap_size / 1024), Tone::Ok),
        ("ПАМЯТЬ УСТРОЙСТВ (DMA)", info.dma_used, info.dma_total, format!("{} КиБ занято из {}", info.dma_used / 1024, info.dma_total / 1024), Tone::Warn),
    ]
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
    // Завершённые задачи — это слоты, которые уборщик ещё не освободил; для
    // человека их нет, и в списке они читались бы как «зависшие».
    text.lines().filter_map(Task::parse).filter(|task| task.state != "finished").collect()
}

/// Прочитать файл служб — правленый в `/etc`, иначе эталон из образа.
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

fn read_small(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(SERVICES_LIMIT).is_err() {
        close(fd);
        return None;
    }
    buffer.resize(SERVICES_LIMIT, 0u8);
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
        user_abi::ERR_NOT_FOUND => String::from("такой задачи уже нет"),
        user_abi::ERR_PERMISSION => String::from("это чужая программа, снять её может только root"),
        user_abi::ERR_UNSUPPORTED => String::from("задачу ядра снять нельзя"),
        user_abi::ERR_TOO_MANY_TASKS => String::from("слишком много задач"),
        _ => format!("код {code}"),
    }
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    // Первые счётчики берутся заново: между ожиданием графики и открытием окна
    // проходят секунды, а вкладка производительности показывает их сразу.
    let fresh = sysinfo().unwrap_or(info);
    app::run(&SPEC, &info, size, move |_, _| Manager::new(fresh))
}
