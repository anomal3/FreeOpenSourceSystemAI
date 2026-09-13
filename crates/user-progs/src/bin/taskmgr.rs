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

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_progs::{
    Args, SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END,
    WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_MENU, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT,
    WIN_KEY_UP, WIN_POINTER, Window, close, exit, kill, monotonic_ms, nanosleep, open, println,
    read, spawn, sysinfo, tasks,
};

/// Имя окна. Латиницей: по нему стенд наводит мышь.
const TITLE: &str = "Task Manager";

/// Как часто перечитывать задачи и счётчики.
const PERIOD_MS: u64 = 2_000;
/// Пауза между опросами очереди событий.
const POLL_NS: u32 = 30_000_000;
/// Сколько байт отводится под список задач и под файл служб.
const TASKS_LIMIT: usize = 16 * 1024;
const SERVICES_LIMIT: usize = 8 * 1024;
/// Сколько ждать окна и графики при запуске — как у «Файлов».
const OPEN_WAIT_MS: u64 = 30_000;
const WAIT_GRAPHICS_MS: u64 = 10_000;

/// Клавиши с символом.
const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const QUIT: u32 = 'q' as u32;
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

    fn icon(&self) -> (Icon, Tone, bool) {
        match self.kind {
            Kind::Program => (Icon::Terminal, Tone::Ok, false),
            Kind::Service => (Icon::Service, Tone::Accent, false),
            Kind::Kernel => (Icon::Settings, Tone::Muted, false),
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

    fn title(self) -> &'static str {
        match self {
            Tab::Processes => "Процессы",
            Tab::Performance => "Производительность",
            Tab::Services => "Службы",
        }
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

    fn title(self) -> &'static str {
        match self {
            MenuItem::EndTask => "Снять задачу",
            MenuItem::OpenLocation => "Открыть расположение файла",
            MenuItem::Refresh => "Обновить",
        }
    }

    fn icon(self) -> Option<Icon> {
        match self {
            MenuItem::EndTask => Some(Icon::Close),
            MenuItem::OpenLocation => Some(Icon::Folder),
            MenuItem::Refresh => Some(Icon::Update),
        }
    }

    const fn danger(self) -> bool {
        matches!(self, MenuItem::EndTask)
    }
}

/// Контекстное меню — карточка поверх окна, как у «Файлов».
struct Menu {
    x: i32,
    y: i32,
    selected: usize,
}

/// Состояние окна.
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

    /// Перечитать задачи, счётчики и службы.
    fn refresh(&mut self) {
        if let Some(fresh) = sysinfo() {
            self.info = fresh;
        }
        self.tasks = list_tasks();
        // Программы и службы — первыми, новые сверху; задачи ядра — за ними,
        // по номеру. Человек ищет глазами то, что запустил только что, а не
        // «idle»; и сам диспетчер, как самая новая программа, стоит первым —
        // по нему стенд и считает строки.
        self.tasks.sort_by(|a, b| {
            let rank = |task: &Task| u8::from(task.kind == Kind::Kernel);
            rank(a)
                .cmp(&rank(b))
                .then_with(|| match a.kind {
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
                println(&format!(
                    "taskmgr: services: {} described, {running} running",
                    self.services.len()
                ));
            }
        }
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
        self.menu = None;
        println("taskmgr: menu closed");
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
        let (x, y) = at.unwrap_or((0, 0));
        println(&format!("taskmgr: menu opened for #{} '{}'", task.id, task.name));
        self.menu = Some(Menu { x, y, selected: 0 });
        self.note = None;
    }

    // -----------------------------------------------------------------------
    // Клавиши и мышь
    // -----------------------------------------------------------------------

    /// `true` — перерисовать.
    fn key(&mut self, code: u32) -> bool {
        if let Some(menu) = self.menu.as_mut() {
            match code {
                WIN_KEY_UP => menu.selected = menu.selected.saturating_sub(1),
                WIN_KEY_DOWN => menu.selected = (menu.selected + 1).min(MenuItem::ALL.len() - 1),
                ENTER => {
                    let item = MenuItem::ALL[menu.selected.min(MenuItem::ALL.len() - 1)];
                    self.run_menu_item(item);
                }
                ESCAPE | WIN_KEY_MENU | MENU => {
                    self.menu = None;
                    println("taskmgr: menu closed");
                }
                _ => return false,
            }
            return true;
        }
        match code {
            TAB_1 => self.set_tab(Tab::Processes),
            TAB_2 => self.set_tab(Tab::Performance),
            TAB_3 => self.set_tab(Tab::Services),
            WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                let at = Tab::ALL.iter().position(|tab| *tab == self.tab).unwrap_or(0);
                let next = if code == WIN_KEY_RIGHT {
                    (at + 1).min(Tab::ALL.len() - 1)
                } else {
                    at.saturating_sub(1)
                };
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
        let plan = layout(ctx, area, self.tab);
        if self.menu.is_some() {
            if let Some(card) = self.menu_rect(ctx, area) {
                if card.contains(x, y) {
                    let row_h = ctx.px(theme::MENU_ROW_H) as i32;
                    let offset = ((y - card.y - ctx.px(6) as i32) / row_h).max(0) as usize;
                    if let Some(item) = MenuItem::ALL.get(offset) {
                        self.run_menu_item(*item);
                    }
                    return true;
                }
            }
            self.menu = None;
            println("taskmgr: menu closed");
            return true;
        }
        for (rect, tab) in plan.tabs.iter().zip(Tab::ALL) {
            if rect.contains(x, y) {
                self.set_tab(tab);
                return true;
            }
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
        let plan = layout(ctx, area, self.tab);
        if self.menu.is_some() {
            self.menu = None;
            println("taskmgr: menu closed");
            return true;
        }
        if self.tab != Tab::Processes {
            return false;
        }
        if let Some(index) = self.row_at(&plan, x, y) {
            self.select_index(index);
            self.open_menu(Some((x, y)));
            return true;
        }
        false
    }

    fn row_at(&self, plan: &Plan, x: i32, y: i32) -> Option<usize> {
        if !plan.list.contains(x, y) {
            return None;
        }
        let step = plan.row_h.max(1) as i32;
        let offset = ((y - plan.list.y) / step).max(0) as usize;
        let visible = plan.visible();
        if offset >= visible {
            return None;
        }
        let index = self.first_visible(visible) + offset;
        (index < self.tasks.len()).then_some(index)
    }

    fn first_visible(&self, visible: usize) -> usize {
        let selected = self.selected_index().unwrap_or(0);
        if visible == 0 || selected < visible {
            0
        } else {
            selected + 1 - visible
        }
    }

    fn menu_rect(&self, ctx: Ctx, area: Rect) -> Option<Rect> {
        let menu = self.menu.as_ref()?;
        let mut widest = 0;
        for item in MenuItem::ALL {
            widest = widest.max(ctx.face(Role::Body).width(item.title()));
        }
        let w = widest + ctx.px(14 + 14 + 10 + 12) + ctx.px(12);
        let h = ctx.px(6) * 2 + ctx.px(theme::MENU_ROW_H) * MenuItem::ALL.len() as u32;
        let (mut x, mut y) = if menu.x != 0 || menu.y != 0 {
            (menu.x, menu.y)
        } else {
            let plan = layout(ctx, area, self.tab);
            let visible = plan.visible();
            let first = self.first_visible(visible);
            let offset = self.selected_index().unwrap_or(0).saturating_sub(first);
            let row = plan.row_rect(ctx, offset);
            (row.x + ctx.px(60) as i32, row.bottom())
        };
        x = x.clamp(area.x, (area.right() - w as i32).max(area.x));
        y = y.clamp(area.y, (area.bottom() - h as i32).max(area.y));
        Some(Rect::new(x, y, w, h))
    }

    // -----------------------------------------------------------------------
    // Отрисовка
    // -----------------------------------------------------------------------

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = layout(ctx, area, self.tab);
        self.draw_toolbar(s, ctx, &plan);
        match self.tab {
            Tab::Processes => self.draw_processes(s, ctx, &plan),
            Tab::Performance => self.draw_performance(s, ctx, &plan),
            Tab::Services => self.draw_services(s, ctx, &plan),
        }
        self.draw_status(s, ctx, &plan);
        self.draw_menu(s, ctx, area);
    }

    /// Панель с вкладками.
    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        s.fill(plan.toolbar, ctx.flat(p.panel));
        draw::hline(s, plan.toolbar.x, plan.toolbar.bottom() - 1, plan.toolbar.w, p.line.color, p.line.alpha);
        let bar = ctx.on(theme::panel_bg());
        paint::sunk(bar, s, plan.tab_box, ctx.px(theme::R_ROW));
        for (rect, tab) in plan.tabs.iter().zip(Tab::ALL) {
            let active = tab == self.tab;
            if active {
                draw::rounded(s, *rect, ctx.px(theme::R_TAB), ctx.flat(p.btn), 255);
                draw::rounded_stroke(s, *rect, ctx.px(theme::R_TAB), p.btnline.color, p.btnline.alpha);
            }
            paint::text_centered(bar, s, Role::Caption, *rect, tab.title(), if active { p.ink2 } else { p.ink4 });
        }
        // Справа — сколько задач живо: это то число, ради которого окно
        // открывают чаще всего.
        let text = format!("{} задач", self.tasks.len());
        paint::text_right(
            bar,
            s,
            Role::Mono,
            plan.toolbar.right() - ctx.px(16) as i32,
            paint::baseline(bar, Role::Mono, plan.toolbar),
            &text,
            p.ink3,
        );
    }

    /// Таблица задач и кнопки под ней.
    fn draw_processes(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        // Шапка столбцов.
        let header = plan.header;
        let y = paint::baseline(ctx, Role::MonoCaps, header);
        let cols = plan.columns(ctx);
        for (rect, title) in cols.iter().zip(["ИМЯ", "НОМЕР", "СОСТОЯНИЕ", "ЦП, МС", "РОД", "ПОЛЬЗОВАТЕЛЬ", "ПУТЬ"]) {
            if rect.w > 0 {
                paint::caps(ctx, s, rect.x, y, title);
            }
        }
        draw::hline(s, header.x + ctx.px(12) as i32, header.bottom() - 1, header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);

        let visible = plan.visible();
        let first = self.first_visible(visible);
        for (offset, task) in self.tasks.iter().skip(first).take(visible).enumerate() {
            let rect = plan.row_rect(ctx, offset);
            let selected = Some(task.id) == self.selected;
            let state = if selected { RowState::Selected } else { RowState::Idle };
            paint::row(ctx, s, rect, state);
            let tile_side = ctx.px(22);
            let tile = Rect::new(rect.x + ctx.px(6) as i32, rect.y + (rect.h as i32 - tile_side as i32) / 2, tile_side, tile_side);
            let (icon, tone, filled) = task.icon();
            paint::icon_tile(ctx, s, tile, icon, tone, filled);
            let ink = if selected { p.ink } else if task.kind == Kind::Kernel { p.ink4 } else { p.ink2 };
            let role = if selected { Role::Title } else { Role::Body };
            let cells = [
                (task.name.clone(), role, ink),
                (format!("#{}", task.id), Role::Mono, p.ink4),
                (task.state_title().to_string(), Role::Body, if task.state == "running" { p.ok_ink } else { p.ink3 }),
                (format!("{}", task.cpu_ms), Role::Mono, p.ink4),
                (task.kind.title().to_string(), Role::Body, p.ink3),
                (task.uid.map_or(String::from("—"), |uid| if uid == 0 { String::from("root") } else { format!("uid {uid}") }), Role::Body, p.ink3),
                (task.path.clone(), Role::Mono, p.ink4),
            ];
            let cols = plan.columns(ctx);
            for (index, (col, (text, role, ink))) in cols.iter().zip(cells).enumerate() {
                if col.w == 0 {
                    continue;
                }
                let x = if index == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                let room = (col.right() - x - ctx.px(8) as i32).max(0) as u32;
                paint::text_clipped(ctx, s, role, x, paint::baseline(ctx, role, rect), room, &text, ink);
            }
        }
        if self.tasks.is_empty() {
            let rect = plan.row_rect(ctx, 0);
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
        let area = plan.body;
        let pad = ctx.px(24);
        let x = area.x + pad as i32;
        let room = area.w.saturating_sub(pad * 2).min(ctx.px(640));
        let mut y = area.y + pad as i32;
        let mib = 1024 * 1024;

        let bars: [(&str, u64, u64, String, Tone); 3] = [
            (
                "ПАМЯТЬ",
                info.frames_total.saturating_sub(info.frames_free),
                info.frames_total,
                format!("{} МиБ занято из {}", (info.frames_total.saturating_sub(info.frames_free)) / mib, info.frames_total / mib),
                Tone::Accent,
            ),
            (
                "КУЧА ЯДРА",
                info.heap_size.saturating_sub(info.heap_free),
                info.heap_size,
                format!("{} КиБ занято из {}", (info.heap_size.saturating_sub(info.heap_free)) / 1024, info.heap_size / 1024),
                Tone::Ok,
            ),
            (
                "ПАМЯТЬ УСТРОЙСТВ (DMA)",
                info.dma_used,
                info.dma_total,
                format!("{} КиБ занято из {}", info.dma_used / 1024, info.dma_total / 1024),
                Tone::Warn,
            ),
        ];
        for (title, done, total, text, tone) in bars {
            if y + ctx.px(70) as i32 > area.bottom() {
                return;
            }
            paint::caps(ctx, s, x, y, title);
            y += ctx.px(20) as i32;
            let bar = Rect::new(x, y, room, ctx.px(14));
            let scale = |value: u64| u32::try_from(value / 4096).unwrap_or(u32::MAX);
            paint::progress(ctx, s, bar, scale(done), scale(total), tone);
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
        let header = plan.header;
        let y = paint::baseline(ctx, Role::MonoCaps, header);
        let cols = plan.service_columns(ctx);
        for (rect, title) in cols.iter().zip(["СЛУЖБА", "СОСТОЯНИЕ", "ПОЛЬЗОВАТЕЛЬ", "ПУТЬ"]) {
            paint::caps(ctx, s, rect.x, y, title);
        }
        draw::hline(s, header.x + ctx.px(12) as i32, header.bottom() - 1, header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);

        let visible = plan.visible();
        for (offset, service) in self.services.iter().take(visible).enumerate() {
            let rect = plan.row_rect(ctx, offset);
            let tile_side = ctx.px(22);
            let tile = Rect::new(rect.x + ctx.px(6) as i32, rect.y + (rect.h as i32 - tile_side as i32) / 2, tile_side, tile_side);
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
            let cols = plan.service_columns(ctx);
            for (index, (col, (text, role, ink))) in cols.iter().zip(cells).enumerate() {
                let x = if index == 0 { tile.right() + ctx.px(10) as i32 } else { col.x };
                let room = (col.right() - x - ctx.px(8) as i32).max(0) as u32;
                paint::text_clipped(ctx, s, role, x, paint::baseline(ctx, role, rect), room, &text, ink);
            }
        }
        if self.services.is_empty() {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(ctx, s, Role::Body, rect.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, rect), rect.w, "Файл служб не прочитан", p.ink5);
        }
        // Правило вехи: то, чего нет, названо, а не спрятано.
        let note = Rect::new(plan.list.x + ctx.px(12) as i32, plan.buttons_y, plan.list.w.saturating_sub(ctx.px(24)), plan.button_h);
        paint::text_clipped(ctx, s, Role::Caption, note.x, paint::baseline(ctx, Role::Caption, note), note.w, "Запуск и остановка служб — функция запланирована; пока это делает /bin/init по файлу /etc/services", p.ink4);
    }

    fn draw_status(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        s.fill(plan.status, ctx.flat(p.panel));
        draw::hline(s, plan.status.x, plan.status.y, plan.status.w, p.line2.color, p.line2.alpha);
        let bar = ctx.on(theme::panel_bg());
        let text = match (&self.note, self.tab) {
            (Some(note), _) => note.clone(),
            (None, Tab::Processes) => String::from("Delete — снять задачу    Enter — расположение файла    правая кнопка — меню    1 2 3 — вкладки"),
            (None, Tab::Performance) => String::from("Числа обновляются каждые две секунды    1 2 3 — вкладки"),
            (None, Tab::Services) => String::from("Службы читаются из /etc/services    1 2 3 — вкладки"),
        };
        let pad = ctx.px(14);
        paint::text_clipped(bar, s, Role::MonoSmall, plan.status.x + pad as i32, paint::baseline(bar, Role::MonoSmall, plan.status), plan.status.w.saturating_sub(pad * 2), &text, p.ink4);
    }

    fn draw_menu(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let Some(card) = self.menu_rect(ctx, area) else {
            return;
        };
        let p = ctx.palette;
        draw::shadow(s, card, ctx.px(theme::R_CARD), ctx.px(18), mini_ui::Color::rgb(0, 0, 0), 90);
        draw::rounded(s, card, ctx.px(theme::R_CARD), ctx.flat(p.panel), 255);
        draw::rounded_stroke(s, card, ctx.px(theme::R_CARD), p.line3.color, p.line3.alpha);
        let inner = ctx.on(theme::panel_bg());
        let pad = ctx.px(6);
        let row_h = ctx.px(theme::MENU_ROW_H);
        let mut y = card.y + pad as i32;
        for (index, item) in MenuItem::ALL.iter().enumerate() {
            let row = Rect::new(card.x + pad as i32, y, card.w.saturating_sub(pad * 2), row_h);
            let selected = index == menu.selected;
            paint::row(inner, s, row, if selected { RowState::Selected } else { RowState::Idle });
            let icon_side = ctx.px(14);
            if let Some(icon) = item.icon() {
                glyphicon::draw(s, icon, row.x + ctx.px(14) as i32, row.y + (row.h as i32 - icon_side as i32) / 2, icon_side, if item.danger() { p.bad_ink } else { p.ink3 }, 255);
            }
            let ink = if item.danger() { p.bad_ink } else if selected { p.ink } else { p.ink2 };
            let text_x = row.x + ctx.px(14 + 14 + 10) as i32;
            paint::text_clipped(inner, s, Role::Body, text_x, paint::baseline(inner, Role::Body, row), (row.right() - text_x - ctx.px(12) as i32).max(0) as u32, item.title(), ink);
            y += row_h as i32;
        }
    }
}

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

struct Plan {
    toolbar: Rect,
    tab_box: Rect,
    tabs: [Rect; 3],
    /// Всё между панелью и строкой состояния.
    body: Rect,
    header: Rect,
    list: Rect,
    buttons_y: i32,
    button_h: u32,
    end_button: Rect,
    open_button: Rect,
    status: Rect,
    row_h: u32,
}

impl Plan {
    fn visible(&self) -> usize {
        (self.list.h / self.row_h.max(1)) as usize
    }

    fn row_rect(&self, ctx: Ctx, offset: usize) -> Rect {
        let pad = ctx.px(12);
        Rect::new(self.list.x + pad as i32, self.list.y + (offset as u32 * self.row_h) as i32, self.list.w.saturating_sub(pad * 2), self.row_h.saturating_sub(ctx.px(2)))
    }

    /// Столбцы таблицы задач: имя тянется, остальные — по ширине содержимого;
    /// узкое окно теряет столбцы справа налево, а не сжимает все разом.
    fn columns(&self, ctx: Ctx) -> [Rect; 7] {
        let widths = [0, ctx.px(70), ctx.px(100), ctx.px(80), ctx.px(100), ctx.px(120), ctx.px(200)];
        let left = self.list.x + ctx.px(24) as i32;
        let right = self.list.right() - ctx.px(22) as i32;
        let mut out = [Rect::EMPTY; 7];
        let name_min = ctx.px(200);
        let mut used = 0u32;
        let mut shown = 1;
        for width in widths.iter().skip(1) {
            if left + (name_min + used + width) as i32 > right {
                break;
            }
            used += width;
            shown += 1;
        }
        let mut x = right - used as i32;
        out[0] = Rect::new(left, self.list.y, (x - left).max(0) as u32, self.list.h);
        for index in 1..shown {
            out[index] = Rect::new(x, self.list.y, widths[index], self.list.h);
            x += widths[index] as i32;
        }
        out
    }

    fn service_columns(&self, ctx: Ctx) -> [Rect; 4] {
        let left = self.list.x + ctx.px(24) as i32;
        let right = self.list.right() - ctx.px(22) as i32;
        let total = (right - left).max(0) as u32;
        let name_w = total / 4;
        let state_w = total / 5;
        let user_w = total / 6;
        let path_w = total.saturating_sub(name_w + state_w + user_w);
        [
            Rect::new(left, self.list.y, name_w, self.list.h),
            Rect::new(left + name_w as i32, self.list.y, state_w, self.list.h),
            Rect::new(left + (name_w + state_w) as i32, self.list.y, user_w, self.list.h),
            Rect::new(left + (name_w + state_w + user_w) as i32, self.list.y, path_w, self.list.h),
        ]
    }
}

fn layout(ctx: Ctx, area: Rect, tab: Tab) -> Plan {
    let pad = ctx.px(14);
    let toolbar_h = ctx.px(theme::TOOLBAR_H).min(area.h);
    let toolbar = Rect::new(area.x, area.y, area.w, toolbar_h);
    let tab_h = ctx.px(26);
    let box_h = ctx.px(32);
    let tab_ws = [ctx.px(96), ctx.px(150), ctx.px(80)];
    let box_w = tab_ws.iter().sum::<u32>() + ctx.px(6);
    let box_x = area.x + pad as i32;
    let box_y = toolbar.y + (toolbar_h as i32 - box_h as i32) / 2;
    let tab_box = Rect::new(box_x, box_y, box_w, box_h);
    let tab_y = box_y + (box_h as i32 - tab_h as i32) / 2;
    let mut tabs = [Rect::EMPTY; 3];
    let mut x = box_x + ctx.px(3) as i32;
    for (rect, w) in tabs.iter_mut().zip(tab_ws) {
        *rect = Rect::new(x, tab_y, w, tab_h);
        x += w as i32;
    }

    let status_h = ctx.px(theme::STATUS_H);
    let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
    let status = Rect::new(area.x, status_y, area.w, status_h);
    let body = Rect::new(area.x, toolbar.bottom(), area.w, (status.y - toolbar.bottom()).max(0) as u32);

    let button_h = ctx.px(30);
    let buttons_h = if tab == Tab::Performance { 0 } else { button_h + ctx.px(20) };
    let buttons_y = status.y - (button_h + ctx.px(10)) as i32;
    let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(body.h);
    let header = Rect::new(body.x, body.y, body.w, header_h);
    let list = Rect::new(body.x, header.bottom(), body.w, body.h.saturating_sub(header_h + buttons_h));

    let end_w = paint::chip_width(ctx, "Снять задачу") + ctx.px(40);
    let open_w = paint::chip_width(ctx, "Открыть расположение файла") + ctx.px(40);
    let end_button = Rect::new(area.right() - pad as i32 - end_w as i32, buttons_y, end_w, button_h);
    let open_button = Rect::new(end_button.x - ctx.px(8) as i32 - open_w as i32, buttons_y, open_w, button_h);

    Plan {
        toolbar,
        tab_box,
        tabs,
        body,
        header,
        list,
        buttons_y,
        button_h,
        end_button,
        open_button,
        status,
        row_h: ctx.px(32),
    }
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
    loop {
        if got >= buffer.len() {
            break;
        }
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
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно в том виде, в каком их положило
    // ядро.
    let _args = unsafe { Args::new(argc, argv) };

    let Some(info) = wait_for_graphics() else {
        println("taskmgr: no graphics on this machine, nothing to show");
        exit(0)
    };
    mini_ui::use_raw_format(info.pixel_format);
    theme::set_dark(info.flags & SYSINFO_DARK != 0);

    let width = (info.screen_w * 2 / 3).clamp(320, info.screen_w.max(320));
    let height = (info.screen_h * 2 / 3).clamp(240, info.screen_h.max(240));
    let scale = theme::geometry_scale(info.screen_w.max(1));

    let Some(mut window) = open_patiently(width, height) else {
        println("taskmgr: FAILED the desktop never freed up; no window");
        exit(1)
    };
    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно; второй ссылки на них нет.
    let Some(mut surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
        println("taskmgr: FAILED the surface the kernel gave makes no sense");
        exit(1)
    };

    let area = Rect::new(0, 0, width, height);
    let ctx = Ctx::scaled(scale);
    println(&format!("taskmgr: window '{TITLE}' opened, {width}x{height}"));
    let mut manager = Manager::new(info);

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next = monotonic_ms() + PERIOD_MS;
    let mut dirty = true;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            match event.kind {
                WIN_CLOSE => {
                    reason = "request";
                    break 'live;
                }
                WIN_KEY if event.code == QUIT && manager.menu.is_none() => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => {
                    if manager.key(event.code) {
                        dirty = true;
                    }
                }
                WIN_POINTER => {
                    let handled = if event.code == 2 {
                        manager.click_right(area, ctx, event.x, event.y)
                    } else {
                        manager.click(area, ctx, event.x, event.y)
                    };
                    if handled {
                        dirty = true;
                    }
                }
                _ => {}
            }
        }

        let now = monotonic_ms();
        if now >= next {
            next = now + PERIOD_MS;
            manager.refresh();
            let fresh_dark = manager.info.flags & SYSINFO_DARK != 0;
            if fresh_dark != dark {
                dark = fresh_dark;
                theme::set_dark(dark);
                println(if dark { "taskmgr: repainted for the dark theme" } else { "taskmgr: repainted for the light theme" });
            }
            dirty = true;
        }

        if dirty {
            manager.draw(&mut surface, area, Ctx::scaled(scale));
            if window.commit() >= 0 {
                dirty = false;
            }
        }
        nanosleep(0, POLL_NS);
    }

    println(&format!("taskmgr: closing on {reason}"));
    window.close();
    exit(0)
}

fn open_patiently(width: u32, height: u32) -> Option<Window> {
    let deadline = monotonic_ms() + OPEN_WAIT_MS;
    loop {
        match Window::open(TITLE, width, height) {
            Ok(window) => return Some(window),
            Err(code) => {
                if code != user_progs::ERR_AGAIN {
                    println(&format!("taskmgr: FAILED opening the window: {code}"));
                    return None;
                }
            }
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}

fn wait_for_graphics() -> Option<SysInfo> {
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    loop {
        let info = sysinfo()?;
        if info.pixel_format != 0 && info.screen_w != 0 {
            return Some(info);
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}
