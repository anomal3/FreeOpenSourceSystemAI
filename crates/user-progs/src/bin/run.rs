//! «Выполнить» (Win+R): строка запуска по имени (фаза П1, 20.11).
//!
//! # Зачем
//!
//! Человек из Windows открывает диспетчер задач, службы и журнал не мышью
//! через три меню, а Win+R → `taskmgr`. Здесь то же самое: короткое имя
//! программы из `/bin`, привычные имена Windows (`msinfo32`, `eventvwr`,
//! `services.msc`, `cleanmgr`) и пути — папка открывается в «Файлах», текст —
//! в «Блокноте», программа запускается.
//!
//! # Как устроено
//!
//! Маленькое окно, как в Windows: поле «Открыть», под ним подсказки (то, что
//! уже набирали, и программы `/bin`, начинающиеся с набранного), кнопки
//! «ОК», «Отмена», «Обзор…». История — `~/.config/run.history`, свежие сверху.
//! Удачный запуск закрывает окно, неудачный — говорит под полем, что именно не
//! найдено, и окно остаётся: имя можно поправить.
//!
//! # Окна стола
//!
//! Терминал, «Параметры» и «О системе» — окна ядра. С П3 (20.11) их
//! открывает стол по узкой просьбе `ui open …` через `SYS_KERNEL_COMMAND`
//! (`settings:network`, `ms-settings:display`, `terminal`, `winver`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Dirent, KIND_DIRECTORY, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_RIGHT,
    WIN_KEY_UP, close, open, open_write, println, read, readdir, spawn, stat, write,
};

const SPEC: Spec = Spec { name: "run", title: "Run", period_ms: 5_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const TAB: u32 = '\t' as u32;
const CTRL_V: u32 = 0x16;
const CTRL_U: u32 = 0x15;

/// Сколько строк истории хранить.
const HISTORY_MAX: usize = 20;
/// Сколько подсказок показывать под полем.
const SUGGEST_MAX: usize = 4;
const MAX_INPUT: usize = 200;

/// Имена Windows и короткие имена → команда FossAi.
const ALIASES: &[(&str, &str, &str)] = &[
    ("taskmgr", "/bin/taskmgr", "Диспетчер задач"),
    ("devmgmt.msc", "/bin/devmgr", "Диспетчер устройств"),
    ("devmgr", "/bin/devmgr", "Диспетчер устройств"),
    ("explorer", "/bin/files", "Файлы"),
    ("files", "/bin/files", "Файлы"),
    ("notepad", "/bin/notepad", "Блокнот"),
    ("calc", "/bin/calc", "Калькулятор"),
    ("charmap", "/bin/charmap", "Таблица символов"),
    ("msinfo32", "/bin/sysinfo", "Сведения о системе"),
    ("sysinfo", "/bin/sysinfo", "Сведения о системе"),
    ("eventvwr", "/bin/eventlog", "Журнал событий"),
    ("eventvwr.msc", "/bin/eventlog", "Журнал событий"),
    ("eventlog", "/bin/eventlog", "Журнал событий"),
    ("services.msc", "/bin/services", "Службы"),
    ("services", "/bin/services", "Службы"),
    ("cleanmgr", "/bin/cleanmgr", "Очистка диска"),
    ("perfmon", "/bin/sysmon", "Монитор системы"),
    ("resmon", "/bin/resmon", "Монитор ресурсов"),
    ("perfmon.msc", "/bin/sysmon", "Монитор системы"),
    // П3 (20.11): «Администрирование» и его инструменты.
    ("control", "/bin/admin", "Администрирование"),
    ("admin", "/bin/admin", "Администрирование"),
    ("compmgmt.msc", "/bin/compmgmt", "Управление компьютером"),
    ("compmgmt", "/bin/compmgmt", "Управление компьютером"),
    ("diskmgmt.msc", "/bin/compmgmt --page disks", "Диски и тома"),
    ("lusrmgr.msc", "/bin/compmgmt --page users", "Пользователи и группы"),
    ("dxdiag", "/bin/compmgmt --page gpu", "Видеокарта и драйверы"),
    ("regedit", "/bin/cfgedit", "Редактор конфигурации"),
    ("cfgedit", "/bin/cfgedit", "Редактор конфигурации"),
    ("msconfig", "/bin/startup", "Автозагрузка"),
    ("startup", "/bin/startup", "Автозагрузка"),
    ("sysmon", "/bin/sysmon", "Монитор системы"),
    ("mc", "/bin/mc", "Midnight Commander"),
];

/// Имена окон ядра: их открывает стол по просьбе `ui open` (см. шапку).
const DESKTOP_ONLY: &[&str] = &["cmd", "terminal", "powershell", "settings", "ms-settings", "winver"];

/// Что получилось с командой.
enum Outcome {
    Started(String),
    Failed(String),
}

struct Plan {
    icon: Rect,
    field: Rect,
    suggest: Rect,
    message: Rect,
    ok: Rect,
    cancel: Rect,
    browse: Rect,
    /// Сколько подсказок помещается в окне.
    shown: usize,
}

impl Plan {
    fn new(ctx: Ctx, area: Rect, suggestions: usize) -> Self {
        let pad = ctx.px(20) as i32;
        let icon = Rect::new(area.x + pad, area.y + ctx.px(20) as i32, ctx.px(40), ctx.px(40));
        let label_w = ctx.face(Role::Body).width("Открыть:") + ctx.px(12);
        let fy = icon.bottom() + ctx.px(18) as i32;
        let field = Rect::new(area.x + pad + label_w as i32, fy, area.w.saturating_sub(pad as u32 * 2 + label_w), ctx.px(34));
        let row = ctx.px(28);
        // Подсказки ложатся поверх кнопок, но не за край окна (20.11: окно
        // стало ниже, как «Выполнить» в Windows).
        let room = (area.bottom() - ctx.px(8) as i32 - field.bottom() - ctx.px(2) as i32).max(0) as u32;
        let shown = suggestions.min((room.saturating_sub(ctx.px(8)) / row) as usize);
        let suggest = Rect::new(field.x, field.bottom() + ctx.px(2) as i32, field.w, row * shown as u32 + if shown > 0 { ctx.px(8) } else { 0 });
        let bh = ctx.px(34);
        let by = area.bottom() - pad - bh as i32;
        let bw = ctx.px(108);
        let gap = ctx.px(10) as i32;
        let browse = Rect::new(area.right() - pad - bw as i32, by, bw, bh);
        let cancel = Rect::new(browse.x - gap - bw as i32, by, bw, bh);
        let ok = Rect::new(cancel.x - gap - bw as i32, by, bw, bh);
        let message = Rect::new(area.x + pad, field.bottom() + ctx.px(8) as i32, area.w.saturating_sub(pad as u32 * 2), ctx.px(24));
        Self { icon, field, suggest, message, ok, cancel, browse, shown }
    }
}

struct Runner {
    input: String,
    /// Курсор — номер знака в `input`.
    caret: usize,
    history: Vec<String>,
    programs: Vec<String>,
    /// Подсказки показаны (стрелка вниз или набор).
    open: bool,
    /// Выбранная подсказка.
    chosen: Option<usize>,
    message: Option<String>,
    done: bool,
}

impl Runner {
    fn new() -> Self {
        let history = read_history();
        let input = history.first().cloned().unwrap_or_default();
        let caret = input.chars().count();
        let programs = list_bin();
        println(&format!("run: ready, {} program(s) in /bin, {} in history", programs.len(), history.len()));
        Self { input, caret, history, programs, open: false, chosen: None, message: None, done: false }
    }

    /// Подсказки к набранному: история, затем имена, затем программы `/bin`.
    fn suggestions(&self) -> Vec<(String, String)> {
        if !self.open {
            return Vec::new();
        }
        let typed = self.input.trim().to_lowercase();
        let mut out: Vec<(String, String)> = Vec::new();
        let push = |name: &str, hint: &str, out: &mut Vec<(String, String)>| {
            if out.len() < SUGGEST_MAX && !out.iter().any(|(n, _)| n == name) {
                out.push((name.to_string(), hint.to_string()));
            }
        };
        for line in &self.history {
            if line.to_lowercase().starts_with(&typed) {
                push(line, "недавнее", &mut out);
            }
        }
        for (name, _, title) in ALIASES {
            if !typed.is_empty() && name.starts_with(&typed) {
                push(name, title, &mut out);
            }
        }
        for name in &self.programs {
            if !typed.is_empty() && name.starts_with(&typed) {
                push(name, "/bin", &mut out);
            }
        }
        out
    }

    fn set_input(&mut self, text: &str) {
        self.input = text.chars().take(MAX_INPUT).collect();
        self.caret = self.input.chars().count();
    }

    fn insert(&mut self, ch: char) {
        if self.input.chars().count() >= MAX_INPUT {
            return;
        }
        let at = byte_at(&self.input, self.caret);
        self.input.insert(at, ch);
        self.caret += 1;
        self.open = true;
        self.chosen = None;
        self.message = None;
    }

    /// Выполнить набранное.
    fn execute(&mut self) {
        if let Some(index) = self.chosen.take() {
            if let Some((name, _)) = self.suggestions().get(index) {
                let name = name.clone();
                self.set_input(&name);
            }
        }
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.open = false;
        match resolve_and_start(&text) {
            Outcome::Started(command) => {
                println(&format!("run: started '{command}' for '{text}'"));
                remember(&mut self.history, &text);
                save_history(&self.history);
                self.done = true;
            }
            Outcome::Failed(why) => {
                println(&format!("run: cannot run '{text}': {why}"));
                self.message = Some(why);
            }
        }
    }

    fn browse(&mut self) {
        let path = if self.input.starts_with('/') { parent(self.input.trim()) } else { String::from("/bin") };
        if spawn(&format!("/bin/files {path}")) >= 0 {
            println(&format!("run: browsing {path}"));
            self.done = true;
        }
    }
}

impl App for Runner {
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let ctrl = mods & user_abi::WIN_MOD_CTRL != 0;
        match code {
            ENTER => self.execute(),
            ESCAPE if self.open => {
                self.open = false;
                self.chosen = None;
            }
            ESCAPE => {
                println("run: cancelled");
                self.done = true;
            }
            BACKSPACE => {
                if self.caret > 0 {
                    let at = byte_at(&self.input, self.caret - 1);
                    self.input.remove(at);
                    self.caret -= 1;
                    self.open = !self.input.is_empty();
                    self.chosen = None;
                    self.message = None;
                }
            }
            user_abi::WIN_KEY_DELETE => {
                if self.caret < self.input.chars().count() {
                    let at = byte_at(&self.input, self.caret);
                    self.input.remove(at);
                }
            }
            WIN_KEY_LEFT => self.caret = self.caret.saturating_sub(1),
            WIN_KEY_RIGHT => self.caret = (self.caret + 1).min(self.input.chars().count()),
            WIN_KEY_HOME => self.caret = 0,
            WIN_KEY_END => self.caret = self.input.chars().count(),
            WIN_KEY_DOWN => {
                if !self.open {
                    self.open = true;
                    self.chosen = None;
                }
                let count = self.suggestions().len();
                if count > 0 {
                    self.chosen = Some(self.chosen.map_or(0, |at| (at + 1).min(count - 1)));
                }
            }
            WIN_KEY_UP => {
                self.chosen = match self.chosen {
                    Some(0) | None => None,
                    Some(at) => Some(at - 1),
                };
            }
            TAB => {
                // Дополнить первой подсказкой, как в оболочке.
                self.open = true;
                let first = self.chosen.unwrap_or(0);
                if let Some((name, _)) = self.suggestions().get(first) {
                    let name = name.clone();
                    self.set_input(&name);
                }
            }
            CTRL_V => {
                let text = user_progs::clipboard_get();
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    self.insert(ch);
                }
            }
            CTRL_U => {
                self.input.clear();
                self.caret = 0;
            }
            _ if ctrl && (latin == 'v' as u32 || latin == 'V' as u32) => {
                let text = user_progs::clipboard_get();
                for ch in text.chars().filter(|ch| !ch.is_control()) {
                    self.insert(ch);
                }
            }
            code if code < 0x0100_0000 => match char::from_u32(code) {
                Some(ch) if !ch.is_control() => self.insert(ch),
                _ => return false,
            },
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let suggestions = self.suggestions();
        let plan = Plan::new(ctx, area, suggestions.len());
        if plan.shown > 0 && plan.suggest.contains(x, y) {
            let row = ((y - plan.suggest.y - ctx.px(4) as i32).max(0) as u32 / ctx.px(28)) as usize;
            if let Some((name, _)) = suggestions.get(row).filter(|_| row < plan.shown) {
                let name = name.clone();
                self.set_input(&name);
                self.open = false;
                self.chosen = None;
                self.execute();
            }
            return true;
        }
        if plan.ok.contains(x, y) {
            self.execute();
        } else if plan.cancel.contains(x, y) {
            println("run: cancelled");
            self.done = true;
        } else if plan.browse.contains(x, y) {
            self.browse();
        } else if plan.field.contains(x, y) {
            self.open = !self.open;
            self.chosen = None;
        } else {
            self.open = false;
        }
        true
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn resizable(&self) -> bool {
        false
    }

    /// Маленький диалог у кнопки «Пуск», как в Windows: только крестик.
    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG | user_abi::WIN_STYLE_AT_START
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Run\x1fВыполнить"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let suggestions = self.suggestions();
        let plan = Plan::new(ctx, area, suggestions.len());

        // Значок и пояснение.
        draw::rounded(s, plan.icon, ctx.px(10), ctx.flat(p.acctint), 255);
        draw::rounded_stroke(s, plan.icon, ctx.px(10), p.accedge, 255);
        let g = ctx.px(22);
        iconset::draw(s, Fi::Play, plan.icon.x + (plan.icon.w - g) as i32 / 2, plan.icon.y + (plan.icon.h - g) as i32 / 2, g, p.acc_ink, 255);
        let tx = plan.icon.right() + ctx.px(14) as i32;
        let room = (area.right() - tx - ctx.px(20) as i32).max(0) as u32;
        let line = i32::from(ctx.face(Role::Body).line);
        paint::text_clipped(ctx, s, Role::Body, tx, plan.icon.y, room, "Введите имя программы, папки или документа,", p.ink2);
        paint::text_clipped(ctx, s, Role::Body, tx, plan.icon.y + line, room, "и FossAi откроет их.", p.ink2);

        // Поле.
        let label_y = paint::baseline(ctx, Role::Body, plan.field);
        paint::text(ctx, s, Role::Body, area.x + ctx.px(20) as i32, label_y, "Открыть:", p.ink);
        paint::sunk(ctx, s, plan.field, ctx.px(6));
        draw::rounded_stroke(s, plan.field, ctx.px(6), p.acc, 255);
        let fx = plan.field.x + ctx.px(10) as i32;
        let froom = plan.field.w.saturating_sub(ctx.px(40));
        let face = ctx.face(Role::Body);
        // Длинная строка уезжает влево так, чтобы курсор оставался виден.
        let before: String = self.input.chars().take(self.caret).collect();
        let shift = face.width(&before).saturating_sub(froom.saturating_sub(ctx.px(4)));
        let field_ctx = ctx;
        let inner = Rect::new(fx, plan.field.y, froom, plan.field.h);
        draw_shifted(field_ctx, s, inner, &self.input, shift, p.ink);
        let caret_x = fx + face.width(&before) as i32 - shift as i32;
        s.fill(Rect::new(caret_x + 1, plan.field.y + ctx.px(8) as i32, ctx.px(2).max(1), plan.field.h.saturating_sub(ctx.px(16))), p.acc);
        let chev = ctx.px(14);
        iconset::draw(s, if self.open { Fi::ChevronUp } else { Fi::ChevronDown }, plan.field.right() - ctx.px(24) as i32, plan.field.y + (plan.field.h - chev) as i32 / 2, chev, p.ink3, 255);

        // Сообщение об ошибке.
        if suggestions.is_empty() {
            if let Some(message) = &self.message {
                let g = ctx.px(16);
                iconset::draw(s, Fi::Warning, plan.message.x, plan.message.y + (plan.message.h - g) as i32 / 2, g, p.bad_ink, 255);
                paint::text_clipped(ctx, s, Role::Caption, plan.message.x + ctx.px(24) as i32, paint::baseline(ctx, Role::Caption, plan.message), plan.message.w.saturating_sub(ctx.px(24)), message, p.bad_ink);
            } else {
                paint::text_clipped(ctx, s, Role::Caption, plan.message.x, paint::baseline(ctx, Role::Caption, plan.message), plan.message.w, "Например: taskmgr, eventvwr, /home, \\\\192.168.1.10", p.ink4);
            }
        }

        // Кнопки.
        paint::button(ctx, s, plan.ok, Weight::Primary, "ОК", false);
        paint::button(ctx, s, plan.cancel, Weight::Normal, "Отмена", false);
        paint::button(ctx, s, plan.browse, Weight::Normal, "Обзор…", false);

        // Подсказки — поверх всего.
        if plan.shown > 0 {
            let card = mini_ui::kit::card(ctx, s, plan.suggest);
            let row_h = ctx.px(28);
            for (index, (name, hint)) in suggestions.iter().enumerate().take(plan.shown) {
                let row = Rect::new(plan.suggest.x + ctx.px(4) as i32, plan.suggest.y + ctx.px(4) as i32 + (index as u32 * row_h) as i32, plan.suggest.w.saturating_sub(ctx.px(8)), row_h);
                let on = self.chosen == Some(index);
                paint::row(card, s, row, if on { paint::RowState::Selected } else { paint::RowState::Idle });
                let y = paint::baseline(card, Role::Body, row);
                let hint_w = card.face(Role::Caption).width(hint);
                paint::text_clipped(card, s, Role::Body, row.x + ctx.px(10) as i32, y, row.w.saturating_sub(hint_w + ctx.px(30)), name, if on { p.ink } else { p.ink2 });
                paint::text_right(card, s, Role::Caption, row.right() - ctx.px(10) as i32, paint::baseline(card, Role::Caption, row), hint, p.ink4);
            }
        }
    }
}

/// Текст поля, сдвинутый влево на `shift` точек и обрезанный по `inner`.
fn draw_shifted(ctx: Ctx, s: &mut Surface, inner: Rect, text: &str, shift: u32, ink: mini_ui::Color) {
    let face = ctx.face(Role::Body);
    // Отбросить знаки, целиком ушедшие за левый край.
    let mut skipped = 0u32;
    let mut start = 0usize;
    for (at, ch) in text.char_indices() {
        let mut buf = [0u8; 4];
        let w = face.width(ch.encode_utf8(&mut buf));
        if skipped + w > shift {
            start = at;
            break;
        }
        skipped += w;
        start = at + ch.len_utf8();
    }
    let x = inner.x - (shift - skipped.min(shift)) as i32;
    paint::text_clipped(ctx, s, Role::Body, x, paint::baseline(ctx, Role::Body, inner), inner.w, &text[start..], ink);
}

// ---------------------------------------------------------------------------
// Разбор команды
// ---------------------------------------------------------------------------

fn resolve_and_start(text: &str) -> Outcome {
    let (word, rest) = match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (text, ""),
    };
    let lower = word.to_lowercase();
    if lower.starts_with("settings:") || lower.starts_with("ms-settings:") || DESKTOP_ONLY.contains(&lower.as_str()) {
        let request = match lower.as_str() {
            "cmd" | "terminal" | "powershell" => String::from("ui open terminal"),
            "winver" => String::from("ui open about"),
            _ => format!("ui open settings {}", lower.split_once(':').map_or("", |(_, page)| page)),
        };
        return open_desktop_window(&request);
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Outcome::Failed(String::from("Для адресов в сети пока нет программы по умолчанию."));
    }
    // `\\host\share` (Roman 24.11): a shared folder of another computer —
    // `/net/host/share`. `netlogon` tries it, asks for credentials if the
    // computer wants them, and opens it in «Файлы».
    if text.starts_with("\\\\") || text.starts_with("/net/") {
        let path = sysconf::winpath::to_unix(text.trim()).into_owned();
        if path.trim_end_matches('/') == "/net" {
            return start("/bin/files /net");
        }
        return start(&format!("/bin/netlogon --open {path}"));
    }
    // Путь: папка — «Файлы», программа — запуск, остальное — «Блокнот».
    if word.starts_with('/') {
        let mut info = user_abi::Stat::default();
        if stat(word, &mut info) < 0 {
            return Outcome::Failed(format!("Не удаётся найти «{word}». Проверьте имя и повторите попытку."));
        }
        let command = if info.kind == KIND_DIRECTORY {
            format!("/bin/files {word}")
        } else if info.mode & 0o111 != 0 {
            text.to_string()
        } else {
            format!("/bin/notepad {word}")
        };
        return start(&command);
    }
    let target = ALIASES
        .iter()
        .find(|(name, _, _)| *name == lower)
        .map(|(_, path, _)| (*path).to_string())
        .unwrap_or_else(|| format!("/bin/{word}"));
    let mut info = user_abi::Stat::default();
    // У имени может быть свой хвост (`diskmgmt.msc` → `compmgmt --page disks`).
    let program = target.split_whitespace().next().unwrap_or("");
    if stat(program, &mut info) < 0 || info.kind == KIND_DIRECTORY {
        return Outcome::Failed(format!("Не удаётся найти «{word}». Проверьте имя и повторите попытку."));
    }
    let command = if rest.is_empty() { target } else { format!("{target} {rest}") };
    start(&command)
}

/// Окно стола по просьбе `ui open …` (П3, 20.11): ответ ядра — строка
/// `ui open: …` или `ui open: FAILED …`.
fn open_desktop_window(request: &str) -> Outcome {
    let mut buffer = alloc::vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = user_progs::kernel_command(request, &mut buffer);
    if got < 0 {
        return Outcome::Failed(format!("Стол не открыл окно: код {got}."));
    }
    let answer = String::from_utf8_lossy(&buffer[..got as usize]).trim().to_string();
    if answer.contains("FAILED") || answer.is_empty() {
        return Outcome::Failed(String::from("Такой страницы «Параметров» нет."));
    }
    Outcome::Started(String::from(request))
}

fn start(command: &str) -> Outcome {
    let id = spawn(command);
    if id < 0 {
        let why = match id {
            user_abi::ERR_PERMISSION => String::from("Недостаточно прав, чтобы запустить это."),
            user_abi::ERR_NO_SPACE => String::from("Не хватает памяти, чтобы запустить программу."),
            code => format!("Запуск не удался: код {code}."),
        };
        return Outcome::Failed(why);
    }
    Outcome::Started(String::from(command))
}

// ---------------------------------------------------------------------------
// История и `/bin`
// ---------------------------------------------------------------------------

fn home_dir() -> String {
    let me = user_progs::uid();
    if me == 0 {
        return String::from("/root");
    }
    for (name, folder) in list_dir("/home") {
        if !folder || name.starts_with('.') {
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

fn history_path() -> String {
    format!("{}/.config/run.history", home_dir())
}

fn read_history() -> Vec<String> {
    read_text(&history_path(), 16 * 1024)
        .map(|text| text.lines().map(str::trim).filter(|line| !line.is_empty()).take(HISTORY_MAX).map(String::from).collect())
        .unwrap_or_default()
}

fn remember(history: &mut Vec<String>, text: &str) {
    history.retain(|line| line != text);
    history.insert(0, text.to_string());
    history.truncate(HISTORY_MAX);
}

fn save_history(history: &[String]) {
    let dir = format!("{}/.config", home_dir());
    let _ = user_progs::mkdir(&dir, 0o755);
    let fd = open_write(&history_path(), true, true);
    if fd < 0 {
        return;
    }
    let mut text = String::new();
    for line in history {
        text.push_str(line);
        text.push('\n');
    }
    let _ = write(fd, text.as_bytes());
    close(fd);
}

fn list_bin() -> Vec<String> {
    let mut names: Vec<String> = list_dir("/bin").into_iter().filter(|(_, folder)| !folder).map(|(name, _)| name).collect();
    names.sort();
    names
}

fn list_dir(path: &str) -> Vec<(String, bool)> {
    let fd = open(path);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir(fd, &mut entry) {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).to_string();
        if name != "." && name != ".." {
            out.push((name, entry.kind == KIND_DIRECTORY));
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
    let mut buffer = alloc::vec![0u8; limit];
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

fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) | None => String::from("/"),
        Some(at) => path[..at].to_string(),
    }
}

/// Байтовое место знака номер `index`.
fn byte_at(text: &str, index: usize) -> usize {
    text.char_indices().nth(index).map_or(text.len(), |(at, _)| at)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let info = app::start(&SPEC);
    let scale = app::ui_scale(&info);
    // Компактный диалог, как «Выполнить» в Windows (20.11: был 520×300 с
    // пустым полем посередине); подсказки ложатся поверх кнопок.
    let width = (480u32 * scale).min(info.screen_w);
    let height = (250u32 * scale).min(info.screen_h);
    app::run(&SPEC, &info, (width, height), |_, _| Runner::new())
}
