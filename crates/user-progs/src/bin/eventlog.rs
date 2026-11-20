//! «Журнал событий» (фаза П2, 20.11): что система говорила о себе.
//!
//! # Зачем
//!
//! Когда что-то не работает, вопрос всегда один: что было в журнале. До сих
//! пор ответ давали `dmesg` в терминале, шторка «Журнал ядра» в диспетчере
//! устройств и `FREEOS\LOG.TXT` на флешке, который Роман присылает в разборах.
//! Здесь всё это одним окном, как «Просмотр событий» Windows: строки с уровнем
//! (ошибка, предупреждение, сведения) и источником, фильтры, поиск, свежие
//! сверху, и кнопка «Сохранить для разбора» — журнал вместе со сведениями о
//! машине одним текстовым файлом.
//!
//! # Откуда строки
//!
//! * **Эта загрузка** — кольцо журнала ядра (`dmesg` через
//!   `SYS_KERNEL_COMMAND`; 32 КиБ, старое вытесняется новым). Только root:
//!   журнал видит всю машину.
//! * **Прошлый сбой** — снимок журнала загрузки, кончившейся паникой
//!   (`lastpanic`).
//! * **Флешка** — `FREEOS/LOG.TXT` на смонтированном томе FAT, если система
//!   загружена с флешки: туда ядро пишет весь журнал с начала загрузки,
//!   непрерывно (`usb/stick_log.rs`), так что он длиннее кольца.
//!
//! # Уровень строки
//!
//! У строк журнала ядра уровня нет — он угадывается по словам («FAILED»,
//! «error», «panic» — ошибка; «warn», «timeout», «missing» — предупреждение).
//! Угадывание честно названо в подсказке окна; правило одно и в одном месте
//! ([`Level::of`]).

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
    Dirent, KIND_DIRECTORY, SysInfo, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_UP, close, file_size, kernel_command, open, open_write, println, read, readdir, stat, write,
};

const SPEC: Spec = Spec { name: "eventlog", title: "Event Log", period_ms: 3_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const CTRL_C: u32 = 0x03;
const CTRL_S: u32 = 0x13;
const F5: u32 = user_abi::WIN_KEY_F1 + 4;

const ROW_H: u32 = 28;
const LEFT_W: u32 = 220;
const DETAIL_H: u32 = 110;
const MAX_STICK_LOG: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Error,
    Warning,
    Info,
}

impl Level {
    /// Уровень по словам строки — единственное место этого правила.
    fn of(text: &str) -> Self {
        let lower = text.to_lowercase();
        const ERRORS: [&str; 12] = [
            "failed", "error", "panic", "fault", "cannot ", "can't", "crash", "giving up", "killed", "stack smashed", "refused", "corrupt",
        ];
        const WARNINGS: [&str; 10] = ["warn", "timeout", "timed out", "missing", "no driver", "not found", "retry", "dropped", "restarting", "slow"];
        if ERRORS.iter().any(|word| lower.contains(word)) {
            Self::Error
        } else if WARNINGS.iter().any(|word| lower.contains(word)) {
            Self::Warning
        } else {
            Self::Info
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Error => "Ошибка",
            Self::Warning => "Предупреждение",
            Self::Info => "Сведения",
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Errors,
    Warnings,
    Info,
}

impl Filter {
    const ALL: [Self; 4] = [Self::All, Self::Errors, Self::Warnings, Self::Info];

    const fn title(self) -> &'static str {
        match self {
            Self::All => "Все",
            Self::Errors => "Ошибки",
            Self::Warnings => "Предупреждения",
            Self::Info => "Сведения",
        }
    }

    fn passes(self, level: Level) -> bool {
        match self {
            Self::All => true,
            Self::Errors => level == Level::Error,
            Self::Warnings => level == Level::Warning,
            Self::Info => level == Level::Info,
        }
    }
}

/// Откуда журнал.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Boot,
    LastPanic,
    Stick,
}

impl Origin {
    const ALL: [Self; 3] = [Self::Boot, Self::LastPanic, Self::Stick];

    const fn title(self) -> &'static str {
        match self {
            Self::Boot => "Эта загрузка",
            Self::LastPanic => "Прошлый сбой",
            Self::Stick => "Флешка: LOG.TXT",
        }
    }

    const fn icon(self) -> Fi {
        match self {
            Self::Boot => Fi::Activity,
            Self::LastPanic => Fi::Warning,
            Self::Stick => Fi::Usb,
        }
    }
}

struct Event {
    /// Номер строки в журнале, с единицы.
    number: usize,
    level: Level,
    source: String,
    text: String,
}

impl Event {
    fn parse(number: usize, line: &str) -> Self {
        let trimmed = line.trim();
        let (source, text) = match trimmed.split_once(" : ") {
            Some((tag, rest)) if tag.trim().len() <= 16 && !tag.trim().contains(' ') => (tag.trim().to_string(), rest.trim().to_string()),
            _ => match trimmed.split_once(": ") {
                Some((tag, rest)) if tag.len() <= 16 && !tag.contains(' ') => (tag.to_string(), rest.to_string()),
                _ => (String::from("kernel"), trimmed.to_string()),
            },
        };
        Self { number, level: Level::of(trimmed), source, text }
    }
}

struct Plan {
    toolbar: Rect,
    refresh: Rect,
    save: Rect,
    chips: Vec<Rect>,
    search: Rect,
    left: Rect,
    list: Rect,
    header: Rect,
    detail: Rect,
    status: Rect,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect) -> Self {
        let frame = kit::Frame::new(ctx, area);
        let toolbar = frame.toolbar;
        let pad = ctx.px(12) as i32;
        let bh = ctx.px(32);
        let by = toolbar.y + (toolbar.h as i32 - bh as i32) / 2;
        let face = ctx.face(Role::Body);
        let refresh = Rect::new(toolbar.x + pad, by, face.width("Обновить") + ctx.px(46), bh);
        let save = Rect::new(refresh.right() + ctx.px(6) as i32, by, face.width("Сохранить для разбора") + ctx.px(46), bh);
        let search_w = ctx.px(220).min(toolbar.w / 4);
        let search = Rect::new(toolbar.right() - pad - search_w as i32, by, search_w, bh);
        let mut chips = Vec::new();
        let mut cx = save.right() + ctx.px(16) as i32;
        for filter in Filter::ALL {
            let w = ctx.face(Role::Caption).width(filter.title()) + ctx.px(52);
            if cx + w as i32 > search.x - ctx.px(8) as i32 {
                chips.push(Rect::EMPTY);
                continue;
            }
            chips.push(Rect::new(cx, toolbar.y + (toolbar.h as i32 - ctx.px(26) as i32) / 2, w, ctx.px(26)));
            cx += (w + ctx.px(6)) as i32;
        }
        let status = frame.status;
        let body = frame.body;
        let left_w = ctx.px(LEFT_W).min(body.w / 4);
        let left = Rect::new(body.x, body.y, left_w, body.h);
        let detail_h = ctx.px(DETAIL_H).min(body.h / 3);
        let detail = Rect::new(left.right(), body.bottom() - detail_h as i32, body.w.saturating_sub(left_w), detail_h);
        let header_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14);
        let header = Rect::new(left.right(), body.y, body.w.saturating_sub(left_w), header_h);
        let list = Rect::new(left.right(), header.bottom(), header.w, (detail.y - header.bottom()).max(0) as u32);
        Self { toolbar, refresh, save, chips, search, left, list, header, detail, status }
    }

    fn columns(&self, ctx: Ctx) -> [Rect; 4] {
        let x = self.list.x + ctx.px(20) as i32;
        let level_w = ctx.px(160);
        let number_w = ctx.px(64);
        let source_w = ctx.px(130);
        let rest = self.list.right() - ctx.px(16) as i32;
        let level = Rect::new(x, self.list.y, level_w, self.list.h);
        let number = Rect::new(level.right(), self.list.y, number_w, self.list.h);
        let source = Rect::new(number.right(), self.list.y, source_w, self.list.h);
        let text = Rect::new(source.right(), self.list.y, (rest - source.right()).max(0) as u32, self.list.h);
        [level, number, source, text]
    }
}

struct EventLog {
    origin: Origin,
    events: Vec<Event>,
    error: Option<String>,
    filter: Filter,
    /// Источник: `None` — все.
    source: Option<String>,
    query: String,
    searching: bool,
    /// Выбранная строка — номер в отфильтрованном списке (свежие сверху).
    selected: usize,
    top: usize,
    /// Поступившие строки сами появляются сверху, пока выбор — на первой.
    follow: bool,
    note: Option<String>,
    stick_path: Option<String>,
    visible: core::cell::Cell<usize>,
}

impl EventLog {
    fn new() -> Self {
        let mut log = Self {
            origin: Origin::Boot,
            events: Vec::new(),
            error: None,
            filter: Filter::All,
            source: None,
            query: String::new(),
            searching: false,
            selected: 0,
            top: 0,
            follow: true,
            note: None,
            stick_path: find_stick_log(),
            visible: core::cell::Cell::new(20),
        };
        log.reload();
        let (errors, warnings) = log.counts();
        println(&format!(
            "eventlog: ready, {} event(s): {errors} error(s), {warnings} warning(s); stick log {}",
            log.events.len(),
            log.stick_path.as_deref().unwrap_or("not found")
        ));
        log
    }

    fn counts(&self) -> (usize, usize) {
        let errors = self.events.iter().filter(|event| event.level == Level::Error).count();
        let warnings = self.events.iter().filter(|event| event.level == Level::Warning).count();
        (errors, warnings)
    }

    fn reload(&mut self) {
        let text = match self.origin {
            Origin::Boot => kernel_text("dmesg 4000"),
            Origin::LastPanic => kernel_text("lastpanic 4000"),
            Origin::Stick => match &self.stick_path {
                Some(path) => read_text(path, MAX_STICK_LOG).ok_or_else(|| format!("Не удалось прочитать {path}.")),
                None => Err(String::from("Журнала на флешке нет: система загружена не с флешки FossAi или том FAT не смонтирован.")),
            },
        };
        match text {
            Ok(text) => {
                self.error = None;
                self.events = text
                    .lines()
                    .map(|line| line.trim_end_matches(|ch: char| ch == ' ' || ch == '\r' || ch == '\0'))
                    .filter(|line| !line.trim().is_empty())
                    .enumerate()
                    .map(|(index, line)| Event::parse(index + 1, line))
                    .collect();
                if self.origin == Origin::LastPanic && self.events.len() == 1 && self.events[0].text.contains("no panic snapshot") {
                    self.events.clear();
                    self.error = Some(String::from("Прошлая загрузка закончилась без паники — снимка нет."));
                }
            }
            Err(error) => {
                self.events.clear();
                self.error = Some(error);
            }
        }
        let shown = self.shown().len();
        if self.follow || self.selected >= shown {
            self.selected = 0;
            self.top = 0;
        }
    }

    /// Номера событий под фильтрами, свежие первыми.
    fn shown(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.events
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, event)| self.filter.passes(event.level))
            .filter(|(_, event)| self.source.as_ref().is_none_or(|source| &event.source == source))
            .filter(|(_, event)| query.is_empty() || event.text.to_lowercase().contains(&query) || event.source.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect()
    }

    /// Источники с числом строк, самые говорливые первыми.
    fn sources(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for event in &self.events {
            match out.iter_mut().find(|(name, _)| *name == event.source) {
                Some((_, count)) => *count += 1,
                None => out.push((event.source.clone(), 1)),
            }
        }
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out
    }

    fn step(&mut self, delta: isize) {
        let count = self.shown().len();
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).clamp(0, count as isize - 1) as usize;
        self.follow = self.selected == 0;
        let visible = self.visible.get().max(1);
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + visible {
            self.top = self.selected + 1 - visible;
        }
    }

    fn set_origin(&mut self, origin: Origin) {
        self.origin = origin;
        self.source = None;
        self.follow = true;
        self.reload();
        println(&format!("eventlog: origin '{}', {} event(s)", origin.title(), self.events.len()));
    }

    fn set_filter(&mut self, filter: Filter) {
        self.filter = filter;
        self.selected = 0;
        self.top = 0;
        println(&format!("eventlog: filter '{}', {} event(s) shown", filter.title(), self.shown().len()));
    }

    fn copy_selected(&mut self) {
        let shown = self.shown();
        if let Some(event) = shown.get(self.selected).and_then(|index| self.events.get(*index)) {
            let text = format!("{} {} : {}", event.level.title(), event.source, event.text);
            let ok = user_progs::clipboard_set(&text);
            self.note = Some(String::from(if ok { "Строка скопирована в буфер обмена" } else { "Буфер обмена не ответил" }));
        }
    }

    /// Журнал и сведения о машине — одним файлом.
    fn save_report(&mut self) {
        let mut text = String::new();
        text.push_str("FossAi: отчёт для разбора\n");
        text.push_str("=========================\n\n");
        text.push_str(&machine_summary());
        text.push_str("\n--- Устройства ---\n");
        text.push_str(&devices_text());
        text.push_str("\n--- Тома ---\n");
        text.push_str(&mounts_text());
        let (errors, warnings) = self.counts();
        text.push_str(&format!("\n--- Журнал: {} ({} строк, ошибок {errors}, предупреждений {warnings}) ---\n", self.origin.title(), self.events.len()));
        for event in &self.events {
            text.push_str(&format!("{:>5} {:<7} {:<14} : {}\n", event.number, event.level.tag(), event.source, event.text));
        }
        if self.origin != Origin::LastPanic {
            if let Ok(panic) = kernel_text("lastpanic 200") {
                if !panic.contains("no panic snapshot") {
                    text.push_str("\n--- Прошлый сбой (паника) ---\n");
                    text.push_str(&panic);
                }
            }
        }
        let dir = report_dir();
        let stamp = user_progs::time_now();
        let path = format!("{dir}/fossai-report-{stamp}.txt");
        let fd = open_write(&path, true, true);
        let ok = fd >= 0 && {
            let mut done = 0usize;
            let bytes = text.as_bytes();
            while done < bytes.len() {
                let wrote = write(fd, &bytes[done..]);
                if wrote <= 0 {
                    break;
                }
                done += wrote as usize;
            }
            done == bytes.len()
        };
        if fd >= 0 {
            close(fd);
        }
        if ok {
            println(&format!("eventlog: report saved to {path}, {} byte(s), {} event(s)", text.len(), self.events.len()));
            self.note = Some(format!("Отчёт сохранён: {path}"));
        } else {
            println(&format!("eventlog: FAILED saving the report to {path} ({fd})"));
            self.note = Some(format!("Не удалось сохранить {path}"));
        }
    }

    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        tool_button(bar, s, plan.refresh, Fi::Refresh, "Обновить");
        tool_button(bar, s, plan.save, Fi::Download, "Сохранить для разбора");
        let (errors, warnings) = self.counts();
        for (filter, rect) in Filter::ALL.into_iter().zip(&plan.chips) {
            if rect.is_empty() {
                continue;
            }
            let count = match filter {
                Filter::All => self.events.len(),
                Filter::Errors => errors,
                Filter::Warnings => warnings,
                Filter::Info => self.events.len() - errors - warnings,
            };
            let on = filter == self.filter;
            let r = rect.h / 2;
            if on {
                draw::rounded(s, *rect, r, bar.flat(p.acctint), 255);
                draw::rounded_stroke(s, *rect, r, p.accedge, 255);
            } else {
                draw::rounded_stroke(s, *rect, r, p.line3.color, p.line3.alpha);
            }
            let dot = match filter {
                Filter::All => p.ink4,
                Filter::Errors => p.bad,
                Filter::Warnings => p.warn,
                Filter::Info => p.acc,
            };
            draw::circle(s, rect.x + ctx.px(12) as i32, rect.y + rect.h as i32 / 2, ctx.px(3), dot, 255);
            let tx = rect.x + ctx.px(22) as i32;
            let w = paint::text(bar, s, Role::Caption, tx, paint::baseline(bar, Role::Caption, *rect), filter.title(), if on { p.ink } else { p.ink2 });
            paint::text(bar, s, Role::MonoSmall, tx + w as i32 + ctx.px(6) as i32, paint::baseline(bar, Role::MonoSmall, *rect), &format!("{count}"), p.ink4);
        }
        let field = plan.search;
        paint::sunk(bar, s, field, ctx.px(6));
        if self.searching {
            draw::rounded_stroke(s, field, ctx.px(6), p.acc, 255);
        }
        let g = ctx.px(16);
        iconset::draw(s, Fi::Search, field.x + ctx.px(10) as i32, field.y + (field.h - g) as i32 / 2, g, p.ink4, 255);
        let tx = field.x + ctx.px(34) as i32;
        let room = (field.right() - tx - ctx.px(8) as i32).max(0) as u32;
        let y = paint::baseline(bar, Role::Body, field);
        if self.query.is_empty() && !self.searching {
            paint::text_clipped(bar, s, Role::Body, tx, y, room, "Поиск в журнале", p.ink5);
        } else {
            let w = paint::text_clipped(bar, s, Role::Body, tx, y, room, &self.query, p.ink);
            if self.searching {
                s.fill(Rect::new(tx + w.min(room) as i32 + 1, field.y + ctx.px(8) as i32, ctx.px(2), field.h.saturating_sub(ctx.px(16))), p.acc);
            }
        }
    }

    /// Строки левой колонки: журналы, потом источники.
    fn left_rows(&self, ctx: Ctx, plan: &Plan) -> Vec<(LeftRow, Rect)> {
        let row_h = ctx.px(30);
        let x = plan.left.x + ctx.px(8) as i32;
        let w = plan.left.w.saturating_sub(ctx.px(16));
        let mut y = plan.left.y + ctx.px(8) as i32;
        let mut out = Vec::new();
        y += ctx.px(20) as i32;
        for origin in Origin::ALL {
            out.push((LeftRow::Origin(origin), Rect::new(x, y, w, row_h)));
            y += row_h as i32;
        }
        y += ctx.px(28) as i32;
        out.push((LeftRow::Source(None), Rect::new(x, y, w, row_h)));
        y += row_h as i32;
        for (index, _) in self.sources().iter().enumerate() {
            if y + row_h as i32 > plan.left.bottom() {
                break;
            }
            out.push((LeftRow::Source(Some(index)), Rect::new(x, y, w, row_h)));
            y += row_h as i32;
        }
        out
    }

    fn draw_left(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        draw::vline(s, plan.left.right() - 1, plan.left.y, plan.left.h, p.line.color, p.line.alpha);
        let rows = self.left_rows(ctx, plan);
        let sources = self.sources();
        if let Some((_, first)) = rows.first() {
            paint::caps(ctx, s, first.x + ctx.px(6) as i32, first.y - ctx.px(20) as i32, "ЖУРНАЛЫ");
        }
        let mut caps_done = false;
        let g = ctx.px(16);
        for (row, rect) in &rows {
            let (on, icon, title, count) = match row {
                LeftRow::Origin(origin) => (*origin == self.origin, Some(origin.icon()), String::from(origin.title()), None),
                LeftRow::Source(None) => (self.source.is_none(), None, String::from("Все источники"), Some(self.events.len())),
                LeftRow::Source(Some(index)) => {
                    let (name, count) = &sources[*index];
                    (self.source.as_ref() == Some(name), None, name.clone(), Some(*count))
                }
            };
            if matches!(row, LeftRow::Source(None)) && !caps_done {
                paint::caps(ctx, s, rect.x + ctx.px(6) as i32, rect.y - ctx.px(20) as i32, "ИСТОЧНИКИ");
                caps_done = true;
            }
            paint::row(ctx, s, *rect, if on { paint::RowState::Selected } else { paint::RowState::Idle });
            let mut tx = rect.x + ctx.px(10) as i32;
            if let Some(icon) = icon {
                iconset::draw(s, icon, tx, rect.y + (rect.h - g) as i32 / 2, g, if on { p.acc_ink } else { p.ink3 }, 255);
                tx += (g + ctx.px(10)) as i32;
            }
            let room = (rect.right() - tx - ctx.px(44) as i32).max(0) as u32;
            paint::text_clipped(ctx, s, Role::Body, tx, paint::baseline(ctx, Role::Body, *rect), room, &title, if on { p.ink } else { p.ink2 });
            if let Some(count) = count {
                paint::text_right(ctx, s, Role::MonoSmall, rect.right() - ctx.px(8) as i32, paint::baseline(ctx, Role::MonoSmall, *rect), &format!("{count}"), p.ink4);
            }
        }
    }

    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let columns = plan.columns(ctx);
        let y = paint::baseline(ctx, Role::MonoCaps, plan.header);
        for (rect, title) in columns.iter().zip(["УРОВЕНЬ", "№", "ИСТОЧНИК", "СООБЩЕНИЕ"]) {
            if rect.w > 0 {
                paint::caps(ctx, s, rect.x, y, title);
            }
        }
        draw::hline(s, plan.header.x + ctx.px(12) as i32, plan.header.bottom() - 1, plan.header.w.saturating_sub(ctx.px(24)), p.line.color, p.line.alpha);
        if let Some(error) = &self.error {
            paint::text_clipped(ctx, s, Role::Body, columns[0].x, plan.list.y + ctx.px(12) as i32, plan.list.w.saturating_sub(ctx.px(40)), error, p.ink3);
            return;
        }
        let shown = self.shown();
        let row_h = ctx.px(ROW_H);
        let visible = (plan.list.h / row_h) as usize;
        self.visible.set(visible.max(1));
        if shown.is_empty() {
            paint::text(ctx, s, Role::Body, columns[0].x, plan.list.y + ctx.px(12) as i32, "Под фильтр ничего не подходит", p.ink4);
            return;
        }
        let g = ctx.px(16);
        for (offset, index) in shown.iter().enumerate().skip(self.top).take(visible) {
            let event = &self.events[*index];
            let row = Rect::new(plan.list.x + ctx.px(8) as i32, plan.list.y + ((offset - self.top) as u32 * row_h) as i32, plan.list.w.saturating_sub(ctx.px(16)), row_h.saturating_sub(ctx.px(2)));
            let on = offset == self.selected;
            if on {
                paint::row(ctx, s, row, paint::RowState::Selected);
            }
            let (icon, tint) = match event.level {
                Level::Error => (Fi::Warning, p.bad_ink),
                Level::Warning => (Fi::Warning, p.warn),
                Level::Info => (Fi::Info, p.acc_ink),
            };
            iconset::draw(s, icon, columns[0].x, row.y + (row.h - g) as i32 / 2, g, tint, 255);
            let by = paint::baseline(ctx, Role::Body, row);
            kit::cell(ctx, s, columns[0], columns[0].x + (g + ctx.px(8)) as i32, row, Role::Body, event.level.title(), if on { p.ink } else { p.ink2 });
            kit::cell(ctx, s, columns[1], columns[1].x, row, Role::Mono, &format!("{}", event.number), p.ink4);
            kit::cell(ctx, s, columns[2], columns[2].x, row, Role::Mono, &event.source, p.acc_ink);
            let ink = match event.level {
                Level::Error => p.bad_ink,
                _ if on => p.ink,
                _ => p.ink2,
            };
            paint::text_clipped(ctx, s, Role::Body, columns[3].x, by, columns[3].w, &event.text, ink);
        }
        // Где мы в длинном журнале.
        if shown.len() > visible && visible > 0 {
            let track = Rect::new(plan.list.right() - ctx.px(8) as i32, plan.list.y + ctx.px(2) as i32, ctx.px(4), plan.list.h.saturating_sub(ctx.px(4)));
            draw::rounded(s, track, ctx.px(2), ctx.flat(p.sunk), 255);
            let thumb_h = ((track.h as usize * visible / shown.len()) as u32).max(ctx.px(24)).min(track.h);
            let max_top = shown.len() - visible;
            let thumb_y = track.y + ((track.h - thumb_h) as usize * self.top.min(max_top) / max_top.max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
    }

    fn draw_detail(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        s.fill(plan.detail, fill);
        draw::hline(s, plan.detail.x, plan.detail.y, plan.detail.w, p.line.color, p.line.alpha);
        let inner = ctx.on(fill);
        let shown = self.shown();
        let Some(event) = shown.get(self.selected).and_then(|index| self.events.get(*index)) else {
            return;
        };
        let pad = ctx.px(16) as i32;
        let x = plan.detail.x + pad;
        let w = plan.detail.w.saturating_sub(pad as u32 * 2);
        let mut y = plan.detail.y + ctx.px(10) as i32;
        let head = format!("{} · {} · строка {} · {}", event.level.title(), event.source, event.number, self.origin.title());
        paint::text_clipped(inner, s, Role::Strong, x, y, w, &head, p.ink);
        y += i32::from(ctx.face(Role::Strong).line) + ctx.px(6) as i32;
        let line_h = i32::from(ctx.face(Role::Mono).line);
        let face = ctx.face(Role::Mono);
        // Полный текст строки — с переносом по ширине.
        let mut rest: &str = &event.text;
        while !rest.is_empty() && y + line_h <= plan.detail.bottom() - ctx.px(4) as i32 {
            let fits = face.fits(rest, w).max(1);
            let cut = rest.char_indices().nth(fits).map_or(rest.len(), |(at, _)| at);
            paint::text(inner, s, Role::Mono, x, y, &rest[..cut], p.ink2);
            rest = &rest[cut..];
            y += line_h;
        }
    }
}

enum LeftRow {
    Origin(Origin),
    Source(Option<usize>),
}

impl App for EventLog {
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & user_abi::WIN_MOD_CTRL != 0;
        if code == CTRL_C || ctrl && (latin == 'c' as u32 || latin == 'C' as u32) {
            self.copy_selected();
            return true;
        }
        if code == CTRL_S || ctrl && (latin == 's' as u32 || latin == 'S' as u32) {
            self.save_report();
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
                code if code < 0x0100_0000 => match char::from_u32(code) {
                    Some(ch) if !ch.is_control() && self.query.chars().count() < 40 => self.query.push(ch),
                    _ => return false,
                },
                _ => return false,
            }
            self.selected = 0;
            self.top = 0;
            println(&format!("eventlog: search '{}' -> {} event(s)", self.query, self.shown().len()));
            return true;
        }
        let page = self.visible.get().max(1) as isize;
        match code {
            WIN_KEY_UP => self.step(-1),
            WIN_KEY_DOWN => self.step(1),
            WIN_KEY_PAGE_UP => self.step(-page),
            WIN_KEY_PAGE_DOWN => self.step(page),
            WIN_KEY_HOME => self.step(isize::MIN / 2),
            WIN_KEY_END => self.step(isize::MAX / 2),
            F5 => self.reload(),
            code if code == 'r' as u32 => self.reload(),
            code if code == 'e' as u32 => self.set_filter(if self.filter == Filter::Errors { Filter::All } else { Filter::Errors }),
            code if code == 'w' as u32 => self.set_filter(if self.filter == Filter::Warnings { Filter::All } else { Filter::Warnings }),
            code if code == 'a' as u32 => self.set_filter(Filter::All),
            code if code == '/' as u32 || ctrl && latin == 'f' as u32 => self.searching = true,
            code if code == '1' as u32 => self.set_origin(Origin::Boot),
            code if code == '2' as u32 => self.set_origin(Origin::LastPanic),
            code if code == '3' as u32 => self.set_origin(Origin::Stick),
            ESCAPE => {
                self.query.clear();
                self.source = None;
                self.filter = Filter::All;
                self.note = None;
            }
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = Plan::new(ctx, area);
        self.searching = false;
        if plan.refresh.contains(x, y) {
            self.reload();
            return true;
        }
        if plan.save.contains(x, y) {
            self.save_report();
            return true;
        }
        if plan.search.contains(x, y) {
            self.searching = true;
            return true;
        }
        for (filter, rect) in Filter::ALL.into_iter().zip(&plan.chips) {
            if rect.contains(x, y) {
                self.set_filter(filter);
                return true;
            }
        }
        let sources = self.sources();
        for (row, rect) in self.left_rows(ctx, &plan) {
            if rect.contains(x, y) {
                match row {
                    LeftRow::Origin(origin) => self.set_origin(origin),
                    LeftRow::Source(None) => self.source = None,
                    LeftRow::Source(Some(index)) => self.source = sources.get(index).map(|(name, _)| name.clone()),
                }
                self.selected = 0;
                self.top = 0;
                return true;
            }
        }
        if plan.list.contains(x, y) {
            let offset = ((y - plan.list.y) as u32 / ctx.px(ROW_H)) as usize;
            let index = self.top + offset;
            if index < self.shown().len() {
                self.selected = index;
                self.follow = index == 0;
            }
            return true;
        }
        false
    }

    fn wheel(&mut self, clicks: i32) -> bool {
        let count = self.shown().len();
        let visible = self.visible.get().max(1);
        let max_top = count.saturating_sub(visible);
        self.top = if clicks > 0 { self.top.saturating_sub(3) } else { (self.top + 3).min(max_top) };
        if self.selected < self.top {
            self.selected = self.top;
        } else if self.selected >= self.top + visible {
            self.selected = self.top + visible - 1;
        }
        self.follow = self.selected == 0;
        true
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        // Живой хвост — только у этой загрузки и только пока смотрят на свежее.
        if self.origin == Origin::Boot && self.follow && !self.searching {
            let before = self.events.len();
            self.reload();
            return self.events.len() != before;
        }
        false
    }

    fn quits_on_q(&self) -> bool {
        !self.searching
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        s.fill(area, theme::window_bg());
        let plan = Plan::new(ctx, area);
        self.draw_toolbar(s, ctx, &plan);
        self.draw_left(s, ctx, &plan);
        self.draw_list(s, ctx, &plan);
        self.draw_detail(s, ctx, &plan);
        let (errors, warnings) = self.counts();
        let status = match &self.note {
            Some(note) => note.clone(),
            None => format!(
                "{} · событий: {} · ошибок: {errors} · предупреждений: {warnings} · уровень угадан по словам строки",
                self.origin.title(),
                self.events.len()
            ),
        };
        kit::status_bar(ctx, s, plan.status, &status);
    }
}

// ---------------------------------------------------------------------------
// Кирпичи
// ---------------------------------------------------------------------------

fn tool_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Fi, label: &str) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Ghost, "", false);
    let g = ctx.px(16);
    let x = rect.x + ctx.px(12) as i32;
    iconset::draw(s, icon, x, rect.y + (rect.h - g) as i32 / 2, g, p.ink2, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(8)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
}

/// Вывод команды оболочки ядра или объяснение, почему его нет.
fn kernel_text(line: &str) -> Result<String, String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = kernel_command(line, &mut buffer);
    if got < 0 {
        return Err(match got {
            user_abi::ERR_PERMISSION => String::from("Журнал ядра виден только администратору (root): запустите «Журнал событий» от его имени."),
            user_abi::ERR_AGAIN => String::from("Ядро занято другой командой — «Обновить», чтобы повторить."),
            code => format!("Журнал не прочитан: код {code}."),
        });
    }
    Ok(String::from_utf8_lossy(&buffer[..got as usize]).to_string())
}

/// `FREEOS/LOG.TXT` на смонтированном томе FAT.
fn find_stick_log() -> Option<String> {
    let mut buffer = vec![0u8; 4096];
    let got = user_progs::mounts(&mut buffer);
    if got <= 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer[..got as usize]).to_string();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(point), Some(kind)) = (fields.next(), fields.next()) else { continue };
        if !kind.to_lowercase().contains("fat") {
            continue;
        }
        for name in ["FREEOS/LOG.TXT", "freeos/log.txt", "FreeOS/LOG.TXT"] {
            let path = if point == "/" { format!("/{name}") } else { format!("{point}/{name}") };
            let mut info = user_abi::Stat::default();
            if stat(&path, &mut info) >= 0 && info.kind != KIND_DIRECTORY {
                return Some(path);
            }
        }
    }
    None
}

fn read_text(path: &str, limit: usize) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let size = (file_size(fd).max(0) as usize).min(limit);
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(size.max(1)).is_err() {
        close(fd);
        return None;
    }
    buffer.resize(size, 0u8);
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

/// Строки «что за машина» для отчёта.
fn machine_summary() -> String {
    let mut out = String::new();
    let version = kernel_text("dmesg build").ok().and_then(|text| {
        text.lines().rev().find_map(|line| line.split_once("FossAi ").map(|(_, rest)| rest.trim().to_string()))
    });
    out.push_str(&format!("Система       : FossAi {}\n", version.as_deref().unwrap_or("(версия не найдена в журнале)")));
    if let Some(info) = user_progs::sysinfo() {
        out.push_str(&format!("Процессоры    : {}\n", info.cpus));
        out.push_str(&format!("Память        : {} МиБ, свободно {} МиБ\n", info.frames_total * 4 / 1024, info.frames_free * 4 / 1024));
        out.push_str(&format!("Экран         : {}x{}\n", info.screen_w, info.screen_h));
        out.push_str(&format!("Работает      : {}\n", uptime_text(info.uptime_ms)));
        out.push_str(&format!("Задач         : {}, окон {}\n", info.tasks_alive, info.windows));
    }
    if let Ok(date) = kernel_text("date") {
        for line in date.lines().filter(|line| !line.trim().is_empty()) {
            out.push_str(&format!("Часы          : {}\n", line.trim()));
        }
    }
    if let Ok(slots) = kernel_text("slots") {
        for line in slots.lines().filter(|line| !line.trim().is_empty()) {
            out.push_str(&format!("Слоты         : {}\n", line.trim()));
        }
    }
    out.push_str(&format!("Пользователь  : uid {}\n", user_progs::uid()));
    out
}

fn devices_text() -> String {
    let mut buffer = vec![0u8; 16 * 1024];
    let got = user_progs::devices(&mut buffer);
    if got <= 0 {
        return String::from("(перепись пуста)\n");
    }
    let text = String::from_utf8_lossy(&buffer[..got as usize]).to_string();
    let mut out = String::new();
    for line in text.lines() {
        out.push_str(&line.replace('\t', "  "));
        out.push('\n');
    }
    out
}

fn mounts_text() -> String {
    let mut buffer = vec![0u8; 4096];
    let got = user_progs::mounts(&mut buffer);
    if got <= 0 {
        return String::from("(нет данных)\n");
    }
    String::from_utf8_lossy(&buffer[..got as usize]).replace('\t', "  ")
}

fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{} ч {} мин {} с", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

/// Куда класть отчёт: «Рабочий стол», если он есть, иначе домашняя папка.
fn report_dir() -> String {
    let home = home_dir();
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

fn home_dir() -> String {
    let me = user_progs::uid();
    if me == 0 {
        return String::from("/root");
    }
    let fd = open("/home");
    if fd >= 0 {
        let mut entry = Dirent::default();
        while readdir(fd, &mut entry) {
            let len = (entry.name_len as usize).min(entry.name.len());
            let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
            if name.starts_with('.') || entry.kind != KIND_DIRECTORY {
                continue;
            }
            if entry.uid == me {
                close(fd);
                return format!("/home/{name}");
            }
        }
        close(fd);
    }
    String::from("/home")
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let size = app::two_thirds(&info);
    app::run(&SPEC, &info, size, |_, _| EventLog::new())
}
