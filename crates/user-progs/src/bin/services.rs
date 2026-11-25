//! «Службы» (фаза П2, 20.11): службы `/bin/init` одним окном, как
//! services.msc в Windows.
//!
//! # Что видно
//!
//! Строки `/etc/services` (имя, программа, учётная запись) вместе с тем, что о
//! них знает система: работает ли (задача с той же программой в списке
//! задач), сколько раз init её поднимал и сдался ли (строки `init:` журнала
//! ядра). Снизу — журнал выбранной службы.
//!
//! # Что можно сделать
//!
//! * **Перезапустить** — снять задачу службы; init поднимает её через полсекунды
//!   (так он устроен: см. `init.rs`). Это настоящий перезапуск, а не имитация.
//! * **Запустить** — для службы, на которой init сдался: запуск той же строки
//!   от её учётной записи. Надзора init у неё уже не будет, и окно так и пишет.
//! * **Остановить насовсем** — нельзя: init поднимет службу обратно, а вызова
//!   «скажи init остановить» пока нет. Кнопка видна и говорит, что функция
//!   запланирована (правило вехи v0.7b).
//!
//! Действия — только у root; у остальных окно только показывает.

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
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{SysInfo, WIN_KEY_DOWN, WIN_KEY_UP, close, kernel_command, open, println, read};

const SPEC: Spec = Spec { name: "services", title: "Services", period_ms: 2_000 };

const ENTER: u32 = '\n' as u32;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;
const ROW_H: u32 = 34;
const LOG_H: u32 = 170;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    Restarting,
    GaveUp,
    Stopped,
}

impl State {
    const fn title(self) -> &'static str {
        match self {
            Self::Running => "Работает",
            Self::Restarting => "Перезапускается",
            Self::GaveUp => "Сбой: init сдался",
            Self::Stopped => "Остановлена",
        }
    }

    const fn tone(self) -> Tone {
        match self {
            Self::Running => Tone::Ok,
            Self::Restarting => Tone::Warn,
            Self::GaveUp => Tone::Bad,
            Self::Stopped => Tone::Muted,
        }
    }
}

struct Service {
    name: String,
    exec: String,
    uid: Option<u32>,
    gid: Option<u32>,
    task: Option<u32>,
    restarts: usize,
    state: State,
    log: Vec<String>,
}

struct Plan {
    toolbar: Rect,
    restart: Rect,
    start: Rect,
    stop: Rect,
    refresh: Rect,
    header: Rect,
    list: Rect,
    log: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = frame.toolbar.y + (frame.toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let mut x = frame.toolbar.x + pad;
        let mut button = |label: &str| {
            let rect = Rect::new(x, by, face.width(label) + ctx.px(46), bh);
            x = rect.right() + ctx.px(6) as i32;
            rect
        };
        let restart = button("Перезапустить");
        let start = button("Запустить");
        let stop = button("Остановить");
        let refresh = button("Обновить");
        // Журнал — не больше трети: список служб важнее (20.11: в окне
        // 1280×720 при журнале в половину четвёртая служба не помещалась).
        let log_h = ctx.px(LOG_H).min(frame.body.h / 3);
        let log = Rect::new(frame.body.x, frame.body.bottom() - log_h as i32, frame.body.w, log_h);
        let header_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14);
        let header = Rect::new(frame.body.x, frame.body.y, frame.body.w, header_h);
        let list = Rect::new(frame.body.x, header.bottom(), frame.body.w, (log.y - header.bottom()).max(0) as u32);
        Self { toolbar: frame.toolbar, restart, start, stop, refresh, header, list, log, status: frame.status }
    }

    fn columns(&self, ctx: Ctx) -> [Rect; 5] {
        let x = self.list.x + ctx.px(24) as i32;
        let right = self.list.right() - ctx.px(20) as i32;
        let widths = [ctx.px(190), ctx.px(90), ctx.px(110), ctx.px(130)];
        let mut out = [Rect::EMPTY; 5];
        let rest = (right - x - widths.iter().sum::<u32>() as i32).max(ctx.px(160) as i32) as u32;
        let mut cx = x;
        out[0] = Rect::new(cx, self.list.y, ctx.px(170), self.list.h);
        cx = out[0].right();
        out[1] = Rect::new(cx, self.list.y, widths[0], self.list.h);
        cx = out[1].right();
        out[2] = Rect::new(cx, self.list.y, widths[1], self.list.h);
        cx = out[2].right();
        out[3] = Rect::new(cx, self.list.y, widths[2], self.list.h);
        cx = out[3].right();
        out[4] = Rect::new(cx, self.list.y, rest.saturating_sub(ctx.px(170)), self.list.h);
        out
    }
}

struct Services {
    services: Vec<Service>,
    selected: usize,
    note: Option<String>,
    root: bool,
    log_error: Option<String>,
    /// Первая видимая строка списка: служб бывает больше, чем строк в окне.
    top: core::cell::Cell<usize>,
    /// Сколько строк помещалось при последней отрисовке.
    visible: core::cell::Cell<usize>,
}

impl Services {
    fn new() -> Self {
        let mut window = Self {
            services: Vec::new(),
            selected: 0,
            note: None,
            root: user_progs::uid() == 0,
            log_error: None,
            top: core::cell::Cell::new(0),
            visible: core::cell::Cell::new(1),
        };
        window.refresh();
        let running = window.services.iter().filter(|service| service.state == State::Running).count();
        println(&format!("services: {} described, {running} running", window.services.len()));
        for service in &window.services {
            println(&format!("services: {} {} {}", service.name, service.exec, service.state.title()));
        }
        window
    }

    fn refresh(&mut self) {
        let tasks = call_text(|buffer| user_progs::tasks(buffer), 64 * 1024);
        let log = match kernel_text("dmesg init:") {
            Ok(text) => {
                self.log_error = None;
                text
            }
            Err(error) => {
                self.log_error = Some(error);
                String::new()
            }
        };
        let text = read_text("/etc/services").or_else(|| read_text("/usr/share/defaults/etc/services")).unwrap_or_default();
        let mut out = Vec::new();
        for line in text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')) {
            let mut fields = line.split_whitespace();
            let (Some(name), Some(exec)) = (fields.next(), fields.next()) else { continue };
            let uid = fields.next().and_then(|text| text.parse().ok());
            let gid = fields.next().and_then(|text| text.parse().ok());
            let task = tasks.lines().find_map(|line| {
                let fields: Vec<&str> = line.split('\t').collect();
                (fields.get(6) == Some(&exec) && fields.get(2) != Some(&"finished")).then(|| fields[0].parse().ok()).flatten()
            });
            let quoted = format!("'{name}'");
            let lines: Vec<String> = log.lines().filter(|line| line.contains(&quoted)).map(|line| line.trim().to_string()).collect();
            let restarts = lines.iter().filter(|line| line.contains("restarting")).count();
            let gave_up = lines.last().is_some_and(|line| line.contains("giving up"));
            let state = match task {
                Some(_) => State::Running,
                None if gave_up => State::GaveUp,
                None if lines.last().is_some_and(|line| line.contains("restarting")) => State::Restarting,
                None => State::Stopped,
            };
            out.push(Service { name: name.to_string(), exec: exec.to_string(), uid, gid, task, restarts, state, log: lines });
        }
        self.services = out;
        self.selected = self.selected.min(self.services.len().saturating_sub(1));
    }

    /// Первая видимая строка так, чтобы выбранная была видна, при `visible`
    /// строках в окне.
    fn view_top(&self, visible: usize) -> usize {
        let visible = visible.max(1);
        let max_top = self.services.len().saturating_sub(visible);
        let mut top = self.top.get().min(max_top);
        if self.selected < top {
            top = self.selected;
        } else if self.selected >= top + visible {
            top = self.selected + 1 - visible;
        }
        top
    }

    fn restart(&mut self) {
        let Some(service) = self.services.get(self.selected) else { return };
        if !self.root {
            self.note = Some(String::from("Перезапуск служб — только у администратора (root)"));
            return;
        }
        let name = service.name.clone();
        match service.task {
            Some(task) => {
                let result = user_progs::kill(task);
                if result >= 0 {
                    println(&format!("services: restarting '{name}': task {task} stopped, init brings it back"));
                    self.note = Some(format!("«{name}»: задача {task} снята, init поднимет службу заново"));
                } else {
                    println(&format!("services: FAILED stopping task {task} of '{name}': {result}"));
                    self.note = Some(format!("Не удалось снять задачу {task}: код {result}"));
                }
            }
            None => self.start(),
        }
    }

    fn start(&mut self) {
        let Some(service) = self.services.get(self.selected) else { return };
        if !self.root {
            self.note = Some(String::from("Запуск служб — только у администратора (root)"));
            return;
        }
        if service.task.is_some() {
            self.note = Some(format!("«{}» уже работает", service.name));
            return;
        }
        let name = service.name.clone();
        let id = match (service.uid, service.gid) {
            (Some(uid), Some(gid)) => user_progs::spawn_as(&service.exec, uid, gid),
            _ => user_progs::spawn(&service.exec),
        };
        if id >= 0 {
            println(&format!("services: started '{name}' as {id} (outside init supervision)"));
            self.note = Some(format!("«{name}» запущена как задача {id} — без надзора init до перезагрузки"));
        } else {
            println(&format!("services: FAILED starting '{name}': {id}"));
            self.note = Some(format!("Не удалось запустить «{name}»: код {id}"));
        }
    }
}

impl App for Services {
    /// Smallest window (24.11, WM SDK): the services table and the action buttons.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(560, 360)
    }

    fn key(&mut self, code: u32) -> bool {
        match code {
            WIN_KEY_UP => self.selected = self.selected.saturating_sub(1),
            WIN_KEY_DOWN => self.selected = (self.selected + 1).min(self.services.len().saturating_sub(1)),
            ENTER => self.restart(),
            F5 => self.refresh(),
            code if code == 'r' as u32 => self.refresh(),
            code if code == 's' as u32 => self.start(),
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        if plan.restart.contains(x, y) {
            self.restart();
        } else if plan.start.contains(x, y) {
            self.start();
        } else if plan.stop.contains(x, y) {
            self.note = Some(String::from("Остановка службы — функция запланирована: init пока поднимает службу обратно"));
        } else if plan.refresh.contains(x, y) {
            self.refresh();
        } else if plan.list.contains(x, y) {
            let index = self.top.get() + ((y - plan.list.y) as u32 / ctx.px(ROW_H)) as usize;
            if index < self.services.len() {
                self.selected = index;
            }
        } else {
            return false;
        }
        true
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        let visible = self.visible.get().max(1);
        let max_top = self.services.len().saturating_sub(visible);
        let top = if clicks > 0 { self.top.get().saturating_sub(1) } else { (self.top.get() + 1).min(max_top) };
        self.top.set(top);
        // Выбор остаётся в видимой части, как в списке Windows.
        self.selected = self.selected.clamp(top, (top + visible - 1).min(self.services.len().saturating_sub(1)).max(top));
        true
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Services\x1fСлужбы"))
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        self.refresh();
        true
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        let selected = self.services.get(self.selected);
        let can_start = self.root && selected.is_some_and(|service| service.task.is_none());
        tool_button(bar, s, plan.restart, Fi::Restart, "Перезапустить", self.root && selected.is_some());
        tool_button(bar, s, plan.start, Fi::Play, "Запустить", can_start);
        tool_button(bar, s, plan.stop, Fi::Power, "Остановить", false);
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить", true);

        let columns = plan.columns(ctx);
        let y = paint::baseline(ctx, Role::MonoCaps, plan.header);
        for (rect, title) in columns.iter().zip(["СЛУЖБА", "СОСТОЯНИЕ", "ЗАДАЧА", "ПЕРЕЗАПУСКОВ", "ПРОГРАММА"]) {
            paint::caps(ctx, s, rect.x, y, title);
        }
        draw::hline(s, plan.header.x + ctx.px(12) as i32, plan.header.bottom() - 1, plan.header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);
        let row_h = ctx.px(ROW_H);
        let g = ctx.px(16);
        let visible = ((plan.list.h / row_h) as usize).max(1);
        let top = self.view_top(visible);
        self.visible.set(visible);
        self.top.set(top);
        for (index, service) in self.services.iter().enumerate().skip(top).take(visible) {
            let at = (index - top) as u32;
            let row = Rect::new(plan.list.x + ctx.px(8) as i32, plan.list.y + (at * row_h) as i32, plan.list.w.saturating_sub(ctx.px(16)), row_h.saturating_sub(ctx.px(2)));
            let on = index == self.selected;
            if on {
                paint::row(ctx, s, row, paint::RowState::Selected);
            }
            iconset::draw(s, Fi::Server, columns[0].x, row.y + (row.h - g) as i32 / 2, g, if on { p.acc_ink } else { p.ink3 }, 255);
            kit::cell(ctx, s, columns[0], columns[0].x + (g + ctx.px(8)) as i32, row, Role::Body, &service.name, p.ink);
            let chip_w = paint::chip_width(ctx, service.state.title()).min(columns[1].w.saturating_sub(ctx.px(8)));
            let chip = Rect::new(columns[1].x, row.y + (row.h as i32 - ctx.px(22) as i32) / 2, chip_w, ctx.px(22));
            paint::chip(ctx, s, chip, service.state.title(), service.state.tone());
            let task = service.task.map_or_else(|| String::from("—"), |task| format!("#{task}"));
            kit::cell(ctx, s, columns[2], columns[2].x, row, Role::Mono, &task, p.ink2);
            kit::cell(ctx, s, columns[3], columns[3].x, row, Role::Mono, &format!("{}", service.restarts), if service.restarts > 0 { p.warn } else { p.ink4 });
            let who = service.uid.map_or_else(|| String::from("как init"), |uid| format!("uid {uid}"));
            kit::cell(ctx, s, columns[4], columns[4].x, row, Role::Mono, &format!("{} · {who}", service.exec), p.ink3);
        }
        if self.services.is_empty() {
            paint::text(ctx, s, Role::Body, columns[0].x, plan.list.y + ctx.px(12) as i32, "/etc/services пуст или не найден", p.ink4);
        }
        // Полоса прокрутки — когда служб больше, чем строк.
        if self.services.len() > visible {
            let track = Rect::new(plan.list.right() - ctx.px(8) as i32, plan.list.y, ctx.px(4), (visible as u32 * row_h).min(plan.list.h));
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / self.services.len()) as u32).max(ctx.px(20)).min(track.h);
            let max_top = self.services.len() - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }

        // Журнал выбранной службы.
        let fill = ctx.flat(p.panel);
        s.fill(plan.log, fill);
        draw::hline(s, plan.log.x, plan.log.y, plan.log.w, p.line.color, p.line.alpha);
        let inner = ctx.on(fill);
        let pad = ctx.px(16) as i32;
        let mut ly = plan.log.y + ctx.px(10) as i32;
        let title = selected.map_or_else(|| String::from("Журнал службы"), |service| format!("Журнал службы «{}»", service.name));
        paint::text(inner, s, Role::Strong, plan.log.x + pad, ly, &title, p.ink);
        ly += i32::from(ctx.face(Role::Strong).line) + ctx.px(6) as i32;
        let line_h = i32::from(ctx.face(Role::Mono).line) + ctx.px(2) as i32;
        let room = plan.log.w.saturating_sub(pad as u32 * 2);
        match (&self.log_error, selected) {
            (Some(error), _) => {
                paint::text_clipped(inner, s, Role::Body, plan.log.x + pad, ly, room, error, p.ink3);
            }
            (None, Some(service)) if service.log.is_empty() => {
                paint::text_clipped(inner, s, Role::Body, plan.log.x + pad, ly, room, "В журнале ядра о ней ничего нет", p.ink4);
            }
            (None, Some(service)) => {
                let shown = ((plan.log.bottom() - ly) / line_h).max(1) as usize;
                for line in service.log.iter().skip(service.log.len().saturating_sub(shown)) {
                    let ink = if line.contains("giving up") || line.contains("cannot") { p.bad_ink } else if line.contains("restarting") { p.warn } else { p.ink2 };
                    paint::text_clipped(inner, s, Role::Mono, plan.log.x + pad, ly, room, line, ink);
                    ly += line_h;
                }
            }
            _ => {}
        }

        let running = self.services.iter().filter(|service| service.state == State::Running).count();
        let status = self.note.clone().unwrap_or_else(|| {
            format!(
                "Служб: {} · работают: {running} · {} · Enter — перезапустить, R — обновить",
                self.services.len(),
                if self.root { "управление доступно" } else { "только просмотр: нужен root" }
            )
        });
        kit::status_bar(ctx, s, plan.status, &status);
    }
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

fn kernel_text(line: &str) -> Result<String, String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = kernel_command(line, &mut buffer);
    if got < 0 {
        return Err(match got {
            user_abi::ERR_PERMISSION => String::from("Журнал ядра виден только администратору (root)."),
            code => format!("Журнал не прочитан: код {code}."),
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

fn read_text(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = vec![0u8; 8 * 1024];
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
    let size = app::window_size(&info, (900, 560));
    app::run(&SPEC, &info, size, |_, _| Services::new())
}
