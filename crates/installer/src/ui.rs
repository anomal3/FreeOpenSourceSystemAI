//! Отрисовка экранов: состояние на входе, картинка на выходе.
//!
//! Ни один экран не хранит собственного состояния — всё берётся из [`App`] и
//! рисуется заново. Обработка нажатий живёт в `main`, рисование здесь, и
//! пересечения между ними нет: экран не может «отстать» от состояния,
//! потому что помнить ему нечего.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::widget::{self, Canvas, DARK};

use crate::App;
use crate::Stage;
use crate::install::{self, Step};
use crate::lang::{Language, Strings};
use crate::payload::What;
use crate::screen::Display;

/// Название программы. Одно на оба языка: это имя, а не слово.
const APP: &str = "FreeOS";

/// Всего шагов с вопросами — то, что видно в правом верхнем углу.
const TOTAL_STAGES: u32 = 7;

/// Нарисовать текущее состояние и вывести его на экран.
pub fn draw(display: &mut Display, app: &App) {
    let strings = app.language.strings();
    let theme = DARK;
    let metrics = display.metrics();

    let heading = heading_of(app, strings);
    let step_text = match app.stage.number() {
        Some(number) => format!("{} {number} {} {TOTAL_STAGES}", strings.step, strings.of),
        None => String::new(),
    };
    let footer = footer_of(app, strings);

    let body = widget::frame(
        display.surface(),
        theme,
        metrics,
        &format!("{APP} - {heading}"),
        &step_text,
        footer,
    );
    let mut canvas = Canvas::new(display.surface(), body, theme, metrics);

    match app.stage {
        Stage::Language => language(&mut canvas, app, strings),
        Stage::Welcome => welcome(&mut canvas, app, strings),
        Stage::DiskPick => disk_pick(&mut canvas, app, strings),
        Stage::Confirm => confirm(&mut canvas, app, strings),
        Stage::Account => account(&mut canvas, app, strings),
        Stage::Keyboard => choice(&mut canvas, strings.keyboard_body, KEYBOARDS, app.keyboard),
        Stage::Timezone => timezone(&mut canvas, app, strings),
        Stage::Installing => {
            let (done, step) = app.progress;
            progress_body(&mut canvas, strings, done, step);
        }
        Stage::Done => done(&mut canvas, strings),
        Stage::Failed => failed(&mut canvas, app, strings),
    }

    display.present();
}

/// Нарисовать экран хода работ.
///
/// Отдельно от [`draw`] потому, что во время установки состояние [`App`]
/// заимствовано вызовом, который эту установку и выполняет: полосе хода работ
/// от состояния нужны ровно два числа, и их проще передать, чем возвращать
/// заимствование.
pub fn draw_progress(display: &mut Display, language: Language, done: u32, step: Step) {
    let strings = language.strings();
    let theme = DARK;
    let metrics = display.metrics();

    let body = widget::frame(
        display.surface(),
        theme,
        metrics,
        &format!("{APP} - {}", strings.install_heading),
        "",
        strings.hint_wait,
    );
    let mut canvas = Canvas::new(display.surface(), body, theme, metrics);
    progress_body(&mut canvas, strings, done, step);
    display.present();
}

/// Раскладки, между которыми предлагается выбрать.
///
/// Подписи латиницей на обоих языках намеренно: это обозначения раскладок, а
/// не слова, и «ЙЦУКЕН» в английском интерфейсе выглядел бы опечаткой.
pub const KEYBOARDS: &[(&str, &str)] = &[("us", "US (QWERTY)"), ("ru", "RU (JCUKEN)")];

/// Часовые пояса: целые часы от UTC-12 до UTC+14.
///
/// Половинные и четвертьчасовые смещения существуют, но список из тридцати
/// восьми строк на экране в десять строк — это не выбор, а пролистывание.
/// Смещение всё равно записывается в файл настроек текстом, поэтому уточнить
/// его потом можно правкой одной строки.
pub const TIMEZONE_MIN: i32 = -12;
pub const TIMEZONE_MAX: i32 = 14;

/// Текст пояса по его смещению.
#[must_use]
pub fn timezone_text(offset: i32) -> String {
    let sign = if offset < 0 { '-' } else { '+' };
    format!("UTC{sign}{:02}:00", offset.abs())
}

fn heading_of(app: &App, strings: &Strings) -> &'static str {
    match app.stage {
        Stage::Language => strings.language_heading,
        Stage::Welcome => strings.welcome_heading,
        Stage::DiskPick => strings.disk_heading,
        Stage::Confirm => strings.confirm_heading,
        Stage::Account => strings.account_heading,
        Stage::Keyboard => strings.keyboard_heading,
        Stage::Timezone => strings.timezone_heading,
        Stage::Installing => strings.install_heading,
        Stage::Done => strings.done_heading,
        Stage::Failed => strings.failed_heading,
    }
}

fn footer_of(app: &App, strings: &Strings) -> &'static str {
    match app.stage {
        Stage::Language | Stage::DiskPick | Stage::Confirm | Stage::Keyboard | Stage::Timezone => {
            strings.hint_select
        }
        Stage::Welcome => strings.hint_next,
        Stage::Account => strings.hint_fields,
        Stage::Installing => strings.hint_wait,
        Stage::Done | Stage::Failed => strings.hint_finish,
    }
}

fn language(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.hint(strings.language_body);
    canvas.gap(1);
    let items = [Language::English.endonym(), Language::Russian.endonym()];
    canvas.list(&items, app.language_index);
}

fn welcome(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.paragraph(strings.welcome_body, canvas.theme().text);
    canvas.gap(1);
    canvas.hint(strings.welcome_payload);
    for item in &app.payload_summary {
        canvas.body(item);
    }
}

fn disk_pick(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.line(strings.disk_body, canvas.theme().warning);
    canvas.gap(1);

    if app.disks.is_empty() {
        canvas.paragraph(strings.disk_none, canvas.theme().text);
        return;
    }

    let labels: Vec<String> = app
        .disks
        .iter()
        .enumerate()
        .map(|(index, disk)| {
            let note = if disk.is_install_media {
                strings.disk_install_media
            } else if disk.read_only {
                strings.disk_read_only
            } else if !app.disk_ok(index) {
                strings.disk_too_small
            } else {
                ""
            };
            if note.is_empty() {
                format!("{:<6} {}", disk.bus, disk.size_text())
            } else {
                format!("{:<6} {:<12} ({note})", disk.bus, disk.size_text())
            }
        })
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    scrolled_list(canvas, &refs, app.disk_index);
}

fn confirm(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.line(strings.confirm_warning, canvas.theme().warning);
    canvas.gap(1);

    if let (Some(disk), Some(plan)) = (app.selected_disk(), app.plan()) {
        canvas.body(&format!("{}  {}", disk.bus, disk.size_text()));
        canvas.gap(1);
        canvas.hint(strings.confirm_scheme);
        canvas.body(&format!(
            "  1. {}  -  {}",
            size_text(plan.esp_bytes),
            strings.confirm_esp
        ));
        if plan.root_bytes > 0 {
            canvas.body(&format!(
                "  2. {}  -  {}",
                size_text(plan.root_bytes),
                strings.confirm_root
            ));
        }
        canvas.gap(1);
    }

    let items = [strings.confirm_no, strings.confirm_yes];
    canvas.list(&items, app.confirm_index);
}

fn account(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.paragraph(strings.account_body, canvas.theme().dim);
    canvas.gap(1);
    canvas.field(strings.account_name, &app.account.name, false, app.field == 0);
    canvas.field(
        strings.account_password,
        &app.account.password,
        true,
        app.field == 1,
    );
    canvas.field(
        strings.account_repeat,
        &app.account.repeat,
        true,
        app.field == 2,
    );
    if let Some(problem) = app.account_error {
        let text = match problem {
            crate::account::Invalid::Name => strings.account_err_name,
            crate::account::Invalid::Password => strings.account_err_password,
            crate::account::Invalid::Mismatch => strings.account_err_mismatch,
        };
        canvas.line(text, canvas.theme().warning);
    }
}

fn choice(canvas: &mut Canvas, body: &str, items: &[(&str, &str)], selected: usize) {
    canvas.paragraph(body, canvas.theme().dim);
    canvas.gap(1);
    let labels: Vec<&str> = items.iter().map(|(_, label)| *label).collect();
    canvas.list(&labels, selected);
}

fn timezone(canvas: &mut Canvas, app: &App, strings: &Strings) {
    canvas.hint(strings.timezone_body);
    canvas.gap(1);
    let labels: Vec<String> = (TIMEZONE_MIN..=TIMEZONE_MAX).map(timezone_text).collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    scrolled_list(canvas, &refs, app.timezone);
}

fn progress_body(canvas: &mut Canvas, strings: &Strings, done: u32, step: Step) {
    let label = match step {
        Step::Wipe => strings.step_wipe.to_string(),
        Step::Gpt => strings.step_gpt.to_string(),
        Step::FormatEsp => strings.step_format_esp.to_string(),
        Step::Copy(what) => format!("{}: {}", strings.step_copy, file_name(what)),
        Step::FormatRoot => strings.step_format_root.to_string(),
        Step::Config => strings.step_config.to_string(),
        Step::Flush => strings.step_flush.to_string(),
    };
    canvas.gap(1);
    canvas.progress(done, install::TOTAL_STEPS, &label);
}

fn done(canvas: &mut Canvas, strings: &Strings) {
    canvas.paragraph(strings.done_body, canvas.theme().success);
}

fn failed(canvas: &mut Canvas, app: &App, strings: &Strings) {
    if let Some(reason) = app.failure {
        canvas.paragraph(reason.text(strings), canvas.theme().warning);
        canvas.gap(1);
    }
    canvas.paragraph(strings.failed_body, canvas.theme().dim);
}

/// Имя файла на целевом диске — то, что человеку осмысленнее роли.
const fn file_name(what: What) -> &'static str {
    match what {
        What::Bootloader => crate::payload::BOOT_FILE,
        What::Kernel => "kernel.elf",
        What::Initrd => "initrd.img",
        // Программ несколько, и показывать их по одной значило бы мелькать
        // именами быстрее, чем человек успевает прочесть. Каталог назван
        // целиком: он и есть то, что появляется на диске.
        What::Program => "/bin",
    }
}

/// Список, который не помещается на экран целиком.
///
/// Окно едет за выделенной строкой и держит её в середине, пока это возможно.
/// Полосы прокрутки нет: она заняла бы место, которое здесь дороже, а её
/// работу выполняет счётчик «n из m» под списком.
fn scrolled_list(canvas: &mut Canvas, items: &[&str], selected: usize) {
    let row = canvas.metrics().row().max(1);
    // Одна строка резервируется под счётчик.
    let capacity = (canvas.remaining() / row).saturating_sub(1).max(1) as usize;

    if items.len() <= capacity {
        canvas.list(items, selected);
        return;
    }

    let half = capacity / 2;
    let start = selected.saturating_sub(half).min(items.len() - capacity);
    let end = start + capacity;
    canvas.list(&items[start..end], selected - start);
    canvas.hint(&format!("{} / {}", selected + 1, items.len()));
}

/// Читаемый размер раздела.
fn size_text(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;
    if bytes >= GIB {
        format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}
