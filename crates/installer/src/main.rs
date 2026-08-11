//! Графический установщик FreeOS.
//!
//! Отдельное UEFI-приложение, а не мастер первого запуска внутри системы, и это
//! решение стоит объяснить, потому что оно определило всё остальное.
//!
//! Разметка и форматирование диска — операция до операционной системы. Делать
//! её изнутри ядра значило бы доверить чужие данные собственному, ещё не
//! отлаженному дисковому коду в момент, когда отладчика нет. Здесь же вокруг
//! живая прошивка: она даёт `EFI_BLOCK_IO_PROTOCOL` для доступа к носителям,
//! `EFI_SIMPLE_TEXT_INPUT_PROTOCOL` для клавиатуры и GOP для экрана. Из
//! второго следует главная практическая выгода: **готовность установщика не
//! зависит от готовности драйверов ядра**. Клавиатуру ему даёт прошивка, и он
//! работал бы ещё до того, как в ядре появился USB.
//!
//! Единственное, что установщик делает сам, — разметку: её выполняет крейт
//! `disk`, покрытый тестами на хосте и проверенный там же сборкой загрузочного
//! образа. Код, который стирает чужой диск, обязан быть отлажен до того, как
//! доберётся до чужого диска.
//!
//! # Порядок экранов
//!
//! Язык, приветствие, выбор диска, учётная запись, раскладка, часовой пояс и
//! только потом подтверждение. Подтверждение стоит последним не для симметрии:
//! это единственная точка невозврата, и всё, о чём можно передумать, должно
//! спрашиваться до неё, а не после.

#![no_std]
#![no_main]

extern crate alloc;

mod account;
mod disks;
mod install;
mod keys;
mod lang;
mod log;
mod payload;
mod screen;
mod ui;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use uefi::runtime::ResetType;
use uefi::{Status, entry, println};

use crate::account::{Draft, Invalid};
use crate::disks::Disk;
use crate::install::{Plan, Step};
use crate::keys::Key;
use crate::lang::{Language, Strings};
use crate::payload::Payload;
use crate::screen::Display;

/// Экран, на котором сейчас человек.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Language,
    Welcome,
    DiskPick,
    Account,
    Keyboard,
    Timezone,
    Confirm,
    Installing,
    Done,
    Failed,
}

impl Stage {
    /// Номер шага для подписи в углу; у экранов без вопросов его нет.
    const fn number(self) -> Option<u32> {
        match self {
            Stage::Language => Some(1),
            Stage::Welcome => Some(2),
            Stage::DiskPick => Some(3),
            Stage::Account => Some(4),
            Stage::Keyboard => Some(5),
            Stage::Timezone => Some(6),
            Stage::Confirm => Some(7),
            Stage::Installing | Stage::Done | Stage::Failed => None,
        }
    }
}

/// Почему установка не состоялась.
#[derive(Clone, Copy)]
pub enum Failure {
    Payload(payload::Error),
    Install(install::Error),
}

impl Failure {
    /// Объяснение на языке интерфейса.
    const fn text(self, strings: &Strings) -> &str {
        match self {
            Failure::Payload(payload::Error::NoMemory(_)) => strings.error_memory,
            Failure::Payload(_) => strings.error_no_payload,
            Failure::Install(install::Error::Payload(payload::Error::NoMemory(_))) => {
                strings.error_memory
            }
            Failure::Install(install::Error::Payload(_)) => strings.error_no_payload,
            Failure::Install(install::Error::TooSmall) => strings.disk_too_small,
            Failure::Install(install::Error::Disk) => strings.error_disk,
            Failure::Install(install::Error::RootFs) => strings.error_root_fs,
        }
    }
}

/// Состояние установщика целиком.
pub struct App {
    pub stage: Stage,
    pub language: Language,
    pub language_index: usize,
    pub disks: Vec<Disk>,
    /// Разметка, посчитанная под каждый диск; `None` — диск не подходит.
    ///
    /// Считается заранее, при перечислении: иначе «слишком мал» выяснялось бы
    /// уже после того, как человек выбрал диск и нажал Enter, — то есть в
    /// худший момент.
    pub plans: Vec<Option<Plan>>,
    pub disk_index: usize,
    pub confirm_index: usize,
    pub account: Draft,
    /// Поле учётной записи под курсором: имя, пароль, повтор.
    pub field: usize,
    pub account_error: Option<Invalid>,
    pub keyboard: usize,
    pub timezone: usize,
    /// Строки описи переносимого — для экрана приветствия.
    pub payload_summary: Vec<String>,
    pub progress: (u32, Step),
    pub failure: Option<Failure>,
}

impl App {
    /// Выбранный диск.
    #[must_use]
    pub fn selected_disk(&self) -> Option<&Disk> {
        self.disks.get(self.disk_index)
    }

    /// Разметка под выбранный диск.
    #[must_use]
    pub fn plan(&self) -> Option<&Plan> {
        self.plans.get(self.disk_index)?.as_ref()
    }

    /// Годится ли диск под установку.
    #[must_use]
    pub fn disk_ok(&self, index: usize) -> bool {
        self.disks.get(index).is_some_and(Disk::is_usable)
            && self.plans.get(index).is_some_and(Option::is_some)
    }
}

#[entry]
fn main() -> Status {
    // До успешного init() нет ни глобального аллокатора, ни вывода, поэтому
    // сообщить о провале некуда — остаётся вернуть код ошибки прошивке.
    if uefi::helpers::init().is_err() {
        return Status::ABORTED;
    }
    log::init();
    logln!("FreeOS installer");

    let Some(mut display) = Display::open() else {
        // Единственное место, где установщик пишет в консоль прошивки: экрана
        // у него нет, а молчать нельзя. Дальше вывод идёт только в
        // последовательный порт — консоль прошивки рисовала бы поверх
        // интерфейса.
        println!("FreeOS installer: no linear framebuffer, cannot draw the interface.");
        println!("The machine offers no GOP mode with a framebuffer address.");
        logln!("[boot] no framebuffer, giving up");
        return Status::UNSUPPORTED;
    };

    let mut payload = match payload::probe() {
        Ok(payload) => payload,
        Err(err) => {
            // Носитель неполон. Показать это надо на экране, а не в консоли:
            // человек смотрит на экран.
            let mut app = fresh_app(Vec::new(), Vec::new(), Vec::new());
            app.stage = Stage::Failed;
            app.failure = Some(Failure::Payload(err));
            ui::draw(&mut display, &app);
            wait_and_reboot();
        }
    };

    let disks = disks::enumerate();
    let total = payload.total_bytes();
    let plans: Vec<Option<Plan>> = disks
        .iter()
        .map(|disk| {
            if !disk.is_usable() {
                return None;
            }
            Plan::for_disk(disk, total).ok()
        })
        .collect();

    let summary = payload
        .items
        .iter()
        .map(|item| format!("  \\{}  ({} KiB)", item.target, item.size / 1024))
        .collect();

    let mut app = fresh_app(disks, plans, summary);
    // Курсор ставится на первый пригодный диск: установочный носитель и
    // диски только для чтения уже отсортированы в конец списка, но если
    // пригодных нет вовсе, курсор просто останется на первом.
    app.disk_index = (0..app.disks.len()).find(|&index| app.disk_ok(index)).unwrap_or(0);

    run(&mut display, &mut app, &mut payload)
}

fn fresh_app(disks: Vec<Disk>, plans: Vec<Option<Plan>>, payload_summary: Vec<String>) -> App {
    App {
        stage: Stage::Language,
        language: Language::English,
        language_index: 0,
        disks,
        plans,
        disk_index: 0,
        // Курсор подтверждения стоит на «нет». Согласие обязано быть
        // осознанным действием, а не следствием того, что Enter нажали дважды.
        confirm_index: 0,
        account: Draft::default(),
        field: 0,
        account_error: None,
        keyboard: 0,
        // Смещение UTC+00:00 — середина списка.
        timezone: (-ui::TIMEZONE_MIN) as usize,
        payload_summary,
        progress: (0, Step::Wipe),
        failure: None,
    }
}

/// Главный цикл: нарисовать, дождаться нажатия, обработать.
fn run(display: &mut Display, app: &mut App, payload: &mut Payload) -> Status {
    loop {
        ui::draw(display, app);

        if app.stage == Stage::Installing {
            perform(display, app, payload);
            continue;
        }

        let key = keys::wait();
        match app.stage {
            Stage::Language => language_keys(app, key),
            Stage::Welcome => match key {
                Key::Enter => app.stage = Stage::DiskPick,
                Key::Escape => app.stage = Stage::Language,
                _ => {}
            },
            Stage::DiskPick => disk_keys(app, key),
            Stage::Account => account_keys(app, key),
            Stage::Keyboard => {
                if let Some(stage) = choose(&mut app.keyboard, ui::KEYBOARDS.len(), key) {
                    app.stage = if stage { Stage::Timezone } else { Stage::Account };
                }
            }
            Stage::Timezone => {
                let count = (ui::TIMEZONE_MAX - ui::TIMEZONE_MIN + 1) as usize;
                if let Some(stage) = choose(&mut app.timezone, count, key) {
                    app.stage = if stage { Stage::Confirm } else { Stage::Keyboard };
                }
            }
            Stage::Confirm => confirm_keys(app, key),
            Stage::Done => {
                if key == Key::Enter {
                    reboot();
                }
            }
            Stage::Failed => match key {
                Key::Enter => reboot(),
                // Отказ не обязан быть окончательным: диск мог быть выбран не
                // тот, а носитель — не тот вставлен.
                Key::Escape => {
                    app.failure = None;
                    app.stage = Stage::DiskPick;
                }
                _ => {}
            },
            Stage::Installing => unreachable!("обработано выше"),
        }
    }
}

fn language_keys(app: &mut App, key: Key) {
    match key {
        Key::Up | Key::Down => {
            app.language_index = 1 - app.language_index;
            // Язык меняется сразу, а не по Enter: выбор языка — единственный
            // экран, где человек не может прочитать подпись, пока не выбрал.
            app.language = if app.language_index == 0 {
                Language::English
            } else {
                Language::Russian
            };
        }
        Key::Enter => app.stage = Stage::Welcome,
        _ => {}
    }
}

fn disk_keys(app: &mut App, key: Key) {
    match key {
        Key::Up => app.disk_index = app.disk_index.saturating_sub(1),
        Key::Down => {
            if app.disk_index + 1 < app.disks.len() {
                app.disk_index += 1;
            }
        }
        Key::Enter => {
            // Непригодный диск просто не пропускает дальше: причина уже
            // подписана в самой строке списка, и второе сообщение об этом было
            // бы шумом.
            if app.disk_ok(app.disk_index) {
                app.stage = Stage::Account;
            }
        }
        Key::Escape => app.stage = Stage::Welcome,
        _ => {}
    }
}

fn account_keys(app: &mut App, key: Key) {
    const FIELDS: usize = 3;
    match key {
        Key::Tab | Key::Down => app.field = (app.field + 1) % FIELDS,
        Key::Up => app.field = (app.field + FIELDS - 1) % FIELDS,
        Key::Backspace => {
            field_mut(app).pop();
            app.account_error = None;
        }
        Key::Char(ch) => {
            let limit = if app.field == 0 {
                account::MAX_NAME
            } else {
                account::MAX_PASSWORD
            };
            // Имя ограничено по набору знаков, пароль — нет: единственное
            // требование к паролю здесь в том, чтобы он поместился в строку.
            let allowed = if app.field == 0 {
                account::is_name_char(ch)
            } else {
                !ch.is_control()
            };
            let field = field_mut(app);
            if allowed && field.chars().count() < limit {
                field.push(ch);
                app.account_error = None;
            }
        }
        Key::Enter => match app.account.validate() {
            Ok(()) => {
                app.account_error = None;
                app.stage = Stage::Keyboard;
            }
            Err(problem) => {
                // Курсор переставляется на поле, из-за которого отказ: иначе
                // сообщение указывает на одно, а правки уходят в другое.
                app.field = match problem {
                    Invalid::Name => 0,
                    Invalid::Password => 1,
                    Invalid::Mismatch => 2,
                };
                app.account_error = Some(problem);
            }
        },
        Key::Escape => app.stage = Stage::DiskPick,
        _ => {}
    }
}

fn field_mut(app: &mut App) -> &mut String {
    match app.field {
        0 => &mut app.account.name,
        1 => &mut app.account.password,
        _ => &mut app.account.repeat,
    }
}

/// Общая обработка экрана-списка.
///
/// `Some(true)` — идти вперёд, `Some(false)` — назад, `None` — остаться.
fn choose(index: &mut usize, count: usize, key: Key) -> Option<bool> {
    match key {
        Key::Up => {
            *index = index.saturating_sub(1);
            None
        }
        Key::Down => {
            if *index + 1 < count {
                *index += 1;
            }
            None
        }
        Key::Enter => Some(true),
        Key::Escape => Some(false),
        _ => None,
    }
}

fn confirm_keys(app: &mut App, key: Key) {
    match key {
        Key::Up | Key::Down => app.confirm_index = 1 - app.confirm_index,
        Key::Enter => {
            if app.confirm_index == 1 {
                app.stage = Stage::Installing;
            } else {
                app.stage = Stage::Timezone;
            }
        }
        Key::Escape => app.stage = Stage::Timezone,
        _ => {}
    }
}

/// Выполнить установку, обновляя экран между шагами.
fn perform(display: &mut Display, app: &mut App, payload: &mut Payload) {
    // Всё нужное вынимается из состояния до вызова: замыкание хода работ
    // держит `&mut Display`, и одновременное заимствование `App` на чтение
    // сделало бы вызов невозможным.
    let Some(target) = app.selected_disk().cloned() else {
        app.stage = Stage::Failed;
        app.failure = Some(Failure::Install(install::Error::Disk));
        return;
    };
    let Some(plan) = app.plans.get(app.disk_index).and_then(Option::as_ref) else {
        app.stage = Stage::Failed;
        app.failure = Some(Failure::Install(install::Error::TooSmall));
        return;
    };
    let plan = *plan;
    let account = core::mem::take(&mut app.account);
    let language = app.language;
    let timezone = ui::timezone_text(ui::TIMEZONE_MIN + app.timezone as i32);
    let settings = install::Settings {
        language,
        keyboard: ui::KEYBOARDS[app.keyboard.min(ui::KEYBOARDS.len() - 1)].0,
        timezone: &timezone,
        entropy: entropy(&target),
        unix_time: install::unix_now(),
    };

    let result = install::run(
        &target,
        &plan,
        payload,
        &account,
        &settings,
        |done, step| ui::draw_progress(display, language, done, step),
    );

    app.account = account;
    match result {
        Ok(()) => app.stage = Stage::Done,
        Err(err) => {
            logln!("[install] failed");
            app.failure = Some(Failure::Install(err));
            app.stage = Stage::Failed;
        }
    }
}

/// Источник «случайности» для GUID разделов и соли пароля.
///
/// Криптостойкости здесь нет и не требуется (см. `account`): нужно лишь, чтобы
/// две установки не дали одинаковых идентификаторов. Часы прошивки с точностью
/// до наносекунд плюс адрес выделенного блока и параметры диска дают это с
/// огромным запасом; если часов нет, остаются адрес и параметры, которые всё
/// равно различаются от машины к машине.
fn entropy(target: &Disk) -> u64 {
    let mut value: u64 = 0x9E37_79B9_7F4A_7C15;
    if let Ok(time) = uefi::runtime::get_time() {
        value ^= u64::from(time.nanosecond());
        value = value.rotate_left(17) ^ u64::from(time.second());
        value = value.rotate_left(13) ^ u64::from(time.minute());
        value = value.rotate_left(11) ^ u64::from(time.hour());
        value = value.rotate_left(7) ^ u64::from(time.day());
        value = value.rotate_left(5) ^ u64::from(time.year());
    }
    // Адрес кучи: аллокатор прошивки выдаёт разные адреса на разных машинах и
    // при разной истории выделений.
    let probe = alloc::boxed::Box::new(0u8);
    value = value.rotate_left(23) ^ (alloc::boxed::Box::into_raw(probe) as u64);
    value.rotate_left(19) ^ target.sectors ^ u64::from(target.media_id)
}

/// Показать последний экран и уйти в перезагрузку по нажатию.
fn wait_and_reboot() -> ! {
    loop {
        if keys::wait() == Key::Enter {
            reboot();
        }
    }
}

fn reboot() -> ! {
    logln!("[boot] resetting the machine");
    uefi::runtime::reset(ResetType::COLD, Status::SUCCESS, None)
}

// --- Заглушки, которые требует кодогенерация -------------------------------

/// Реализация `wcslen` для оптимизатора.
///
/// В release-сборке LLVM распознаёт цикл вычисления длины UTF-16 строки (крейт
/// `uefi` повсеместно работает с `CStr16`) и заменяет его вызовом libc-функции
/// `wcslen`. В bare-metal окружении libc нет, и линковка падает с `undefined
/// symbol: wcslen` — причём только в release, debug эту замену не делает.
/// В UEFI символ строки всегда 16-битный, поэтому реализация тривиальна.
#[unsafe(no_mangle)]
extern "C" fn wcslen(s: *const u16) -> usize {
    let mut len = 0;
    // SAFETY: контракт C-функции обязывает вызывающего передать указатель на
    // нуль-терминированную строку; за терминатор мы не читаем.
    unsafe {
        while *s.add(len) != 0 {
            len += 1;
        }
    }
    len
}
