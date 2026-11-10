//! OBSOS — Open Browser for System Open Source, браузер FreeOS на Servo.
//!
//! `obsos [--profile <имя>] [--sync-folder <папка>] [адрес]`
//!
//! Окно стола FreeOS, страницу рисует swgl (программный OpenGL, `swgl_ctx`),
//! оболочку — egui прямо в пиксели окна (`ui.rs`). Вкладки — `WebView`
//! одного контекста swgl, видима только активная. Синхронизация (свой
//! репозиторий GitHub/GitLab/Bitbucket с тегами-точками, Яндекс Диск, Google
//! Диск, WebDAV, папка) — в отдельном потоке (`syncer.rs`), пароли — в
//! зашифрованном хранилище (`vault.rs`).
//!
//! Клавиши — раскладка Chromium (Ctrl+T/W/L/D/H/F/G/R, Ctrl+Shift+T/O/Q,
//! Ctrl+1..9, Ctrl+Tab, Alt+←/→/Home, F3/F5/F6/F10, Ctrl±/0) и своя
//! Ctrl+Shift+S — синхронизировать.

#[path = "../../swgl_ctx.rs"]
mod swgl_ctx;

mod blocker;
mod devtools;
mod downloads;
mod net;
mod pages;
mod profile;
mod shell;
mod syncer;
mod tools;
mod ui;
mod vault;
mod window;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui_software_backend::{ColorFieldOrder, EguiSoftwareRender};
use servo::protocol_handler::ProtocolRegistry;
use servo::{
    DevicePoint, DeviceVector2D, EventLoopWaker, InputEvent, Key, KeyState, KeyboardEvent, Modifiers, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey, RenderingContext, Scroll, ServoBuilder,
    UserContentManager, UserScript,
    WebViewPoint, WebViewVector,
};

use crate::pages::{ObsosProtocol, PageData};
use crate::profile::Profile;
use crate::shell::{Action, Delegate, Modal, Panel, Shell, SyncUi, page_origin, page_size};
use crate::swgl_ctx::SwglRenderingContext;
use crate::syncer::Syncer;
use crate::vault::{OpenError, Vault};
use crate::window::*;

#[derive(Clone)]
struct Waker(Arc<AtomicBool>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

// ── Клавиши окна → Servo и egui ─────────────────────────────────────────────

fn servo_key(code: u32) -> Option<Key> {
    if code >= WIN_KEY_NAMED {
        let named = match code - WIN_KEY_NAMED {
            1 => NamedKey::ArrowLeft,
            2 => NamedKey::ArrowRight,
            3 => NamedKey::ArrowUp,
            4 => NamedKey::ArrowDown,
            5 => NamedKey::Home,
            6 => NamedKey::End,
            7 => NamedKey::PageUp,
            8 => NamedKey::PageDown,
            9 => NamedKey::Delete,
            11 => NamedKey::Insert,
            24 => NamedKey::Shift,
            25 => NamedKey::Control,
            26 => NamedKey::Alt,
            n @ 12..=23 => match n - 12 {
                0 => NamedKey::F1,
                1 => NamedKey::F2,
                2 => NamedKey::F3,
                3 => NamedKey::F4,
                4 => NamedKey::F5,
                5 => NamedKey::F6,
                6 => NamedKey::F7,
                7 => NamedKey::F8,
                8 => NamedKey::F9,
                9 => NamedKey::F10,
                10 => NamedKey::F11,
                _ => NamedKey::F12,
            },
            _ => return None,
        };
        return Some(Key::Named(named));
    }
    Some(match code {
        8 => Key::Named(NamedKey::Backspace),
        9 => Key::Named(NamedKey::Tab),
        10 | 13 => Key::Named(NamedKey::Enter),
        27 => Key::Named(NamedKey::Escape),
        127 => Key::Named(NamedKey::Delete),
        c if c < 32 => return None,
        c => Key::Character(char::from_u32(c)?.to_string()),
    })
}

fn servo_modifiers(mask: i32) -> Modifiers {
    let mut m = Modifiers::empty();
    if mask & 1 != 0 {
        m |= Modifiers::SHIFT;
    }
    if mask & 2 != 0 {
        m |= Modifiers::CONTROL;
    }
    if mask & 4 != 0 {
        m |= Modifiers::ALT;
    }
    m
}

fn egui_modifiers(mask: i32) -> egui::Modifiers {
    let ctrl = mask & 2 != 0;
    egui::Modifiers { alt: mask & 4 != 0, ctrl, shift: mask & 1 != 0, mac_cmd: false, command: ctrl }
}

/// Буква клавиши: сама буква или управляющий код Ctrl+буква (1..=26).
fn letter_of(code: u32) -> Option<char> {
    match code {
        1..=26 => Some((b'a' + code as u8 - 1) as char),
        c => char::from_u32(c).filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()),
    }
}

fn egui_key(code: u32) -> Option<egui::Key> {
    use egui::Key as K;
    if code >= WIN_KEY_NAMED {
        return Some(match code - WIN_KEY_NAMED {
            1 => K::ArrowLeft,
            2 => K::ArrowRight,
            3 => K::ArrowUp,
            4 => K::ArrowDown,
            5 => K::Home,
            6 => K::End,
            7 => K::PageUp,
            8 => K::PageDown,
            9 => K::Delete,
            11 => K::Insert,
            16 => K::F5,
            _ => return None,
        });
    }
    match code {
        8 => Some(K::Backspace),
        9 => Some(K::Tab),
        10 | 13 => Some(K::Enter),
        27 => Some(K::Escape),
        127 => Some(K::Delete),
        c => egui::Key::from_name(&letter_of(c)?.to_ascii_uppercase().to_string()),
    }
}

/// Пользовательские скрипты и стили профиля (`userscripts/*.js|*.css`) —
/// замена расширениям — и свой скрипт OBSOS (пароли, MediaSource).
fn content_manager(servo: &servo::Servo, profile: &Profile) -> Rc<UserContentManager> {
    let manager = Rc::new(UserContentManager::new(servo));
    manager.add_script(Rc::new(UserScript::new(shell::PAGE_SCRIPT.to_string(), None)));
    // Белый фон корня, как у других браузеров: страница без своего фона и
    // короче окна иначе оставляла под собой куски предыдущей (плитки
    // WebRender не перерисовывались). Стиль пользователя — свой фон
    // страницы его перекрывает.
    if let Ok(url) = url::Url::parse("obsos://style/base.css") {
        manager.add_stylesheet(Rc::new(servo::user_contents::UserStyleSheet::new(String::from("html { background-color: #fff; }"), url)));
    }
    let dir = profile.dir.join("userscripts");
    let mut files: Vec<_> = std::fs::read_dir(&dir).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
    files.sort();
    let (mut scripts, mut styles) = (0, 0);
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else { continue };
        match path.extension().and_then(|e| e.to_str()) {
            Some("js") => {
                manager.add_script(Rc::new(UserScript::new(source, Some(path.clone()))));
                scripts += 1;
            }
            Some("css") => {
                if let Ok(url) = url::Url::parse(&format!("file://{}", path.display())) {
                    manager.add_stylesheet(Rc::new(servo::user_contents::UserStyleSheet::new(source, url)));
                    styles += 1;
                }
            }
            _ => {}
        }
    }
    println!("browser: userscripts {scripts} js, {styles} css");
    manager
}

fn egui_cursor(icon: egui::CursorIcon) -> window::CursorShape {
    use egui::CursorIcon as C;
    use window::CursorShape as S;
    match icon {
        C::PointingHand => S::Hand,
        C::Text | C::VerticalText => S::Text,
        C::Move | C::Grab | C::Grabbing | C::AllScroll => S::Move,
        C::NotAllowed | C::NoDrop => S::NotAllowed,
        C::ResizeHorizontal | C::ResizeColumn | C::ResizeEast | C::ResizeWest => S::ResizeEw,
        C::ResizeVertical | C::ResizeRow | C::ResizeNorth | C::ResizeSouth => S::ResizeNs,
        _ => S::Arrow,
    }
}

/// Имя пояса для ICU по смещению: целые часы — `Etc/GMT-3` (знак у `Etc`
/// обратный: это Москва, UTC+3), иначе запись POSIX `<+0530>-5:30`.
fn tz_name(minutes: i32) -> String {
    if minutes == 0 {
        String::from("UTC")
    } else if minutes % 60 == 0 {
        format!("Etc/GMT{:+}", -minutes / 60)
    } else {
        let (sign, m) = if minutes > 0 { ('+', minutes) } else { ('-', -minutes) };
        let posix_sign = if minutes > 0 { '-' } else { '+' };
        format!("<{sign}{:02}{:02}>{posix_sign}{}:{:02}", m / 60, m % 60, m / 60, m % 60)
    }
}

/// Журнал Servo (`log`): предупреждения и ошибки — в журнал системы, по
/// ним видно, почему не загрузился файл страницы. Не больше 400 строк за
/// запуск — сайты бывают болтливы.
struct Logger;

static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl log::Log for Logger {
    fn enabled(&self, meta: &log::Metadata) -> bool {
        meta.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) || LOGGED.fetch_add(1, Ordering::Relaxed) >= 400 {
            return;
        }
        let text: String = record.args().to_string().chars().take(300).collect();
        println!("browser: log {} {}: {text}", record.level(), record.target());
    }

    fn flush(&self) {}
}

static LOGGER: Logger = Logger;

fn main() {
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Warn));
    // Дом должен быть записываемым (профиль, кеш и база Servo). На телефоне
    // корень — FAT32 только на чтение, а записываемый том в памяти ядро
    // монтирует в `/home`: туда, если в `HOME` писать нельзя.
    let writable = |dir: &std::path::Path| std::fs::create_dir_all(dir.join(".config")).is_ok();
    if !std::env::var_os("HOME").map(std::path::PathBuf::from).is_some_and(|home| writable(&home)) && writable(std::path::Path::new("/home")) {
        println!("browser: home /home (HOME is not writable)");
        // SAFETY (edition 2021: безопасно): до запуска любых потоков.
        std::env::set_var("HOME", "/home");
    }
    let t0 = Instant::now();
    println!("browser: start");
    let mut args = std::env::args().skip(1);
    let (mut profile_name, mut first_url) = (String::from("default"), None);
    let mut sync_folder: Option<String> = None;
    let mut private = false;
    while let Some(arg) = args.next() {
        if arg == "--private" {
            private = true;
        } else if arg == "--profile" {
            profile_name = args.next().unwrap_or_default();
        } else if arg == "--sync-folder" {
            // Синхронизация в папку без панели — для стенда и для общей
            // папки (флешка, сетевой диск): площадка проверяется,
            // сохраняется в хранилище и включается, как из «Синхронизации».
            sync_folder = args.next();
        } else {
            first_url = Some(arg);
        }
    }
    let Some(window) = Window::open(if private { "OBSOS — Инкогнито" } else { "OBSOS — Браузер" }, 1024, 700) else {
        std::process::exit(1);
    };
    let window = Rc::new(window);
    // TMPDIR и прочие каталоги Servo — до запуска его потоков.
    let _ = swgl_ctx::prepare_dirs(None);
    // Часовой пояс для JavaScript (`Date`, `Intl`): SpiderMonkey берёт его из
    // `TZ`, а пояс знает ядро (SYS_TZINFO, «Параметры → Дата и время»).
    if std::env::var_os("TZ").is_none() {
        if let Some(minutes) = window::timezone_minutes() {
            let tz = tz_name(minutes);
            println!("browser: timezone {tz}");
            // SAFETY (edition 2021: безопасно): до запуска потоков Servo.
            std::env::set_var("TZ", tz);
        }
    }
    let profile = if private { Profile::open_private() } else { Profile::open(&profile_name) };
    let blocker = Rc::new(RefCell::new(blocker::Blocker::load(&profile.dir, profile.settings.block_ads)));
    let downloads = Rc::new(downloads::Downloads::new());
    println!("browser: profile {} ({}), device {}", profile.name, profile.dir.display(), profile.settings.device);
    let (vault, need_master) = match Vault::open(&profile.dir, None) {
        Ok(v) => (Some(v), false),
        Err(OpenError::NeedMaster) => (None, true),
        Err(OpenError::Broken(why)) => {
            println!("browser: vault broken: {why}");
            (None, false)
        }
    };

    let page = Rc::new(RefCell::new(PageFrame::default()));
    let context = SwglRenderingContext::new(page_size(&profile.settings.look, window.width.get(), window.height.get()));
    {
        let (page, swap_rb) = (page.clone(), window.swap_rb);
        context.set_present_hook(Box::new(move |frame, w, h| page.borrow_mut().store(frame, w, h, swap_rb)));
    }
    let _ = context.make_current();
    println!("browser: renderer {}", swgl_ctx::renderer_name());

    let page_data = Arc::new(Mutex::new(PageData::default()));
    let mut protocols = ProtocolRegistry::with_internal_protocols();
    if protocols.register("obsos", ObsosProtocol { data: page_data.clone() }).is_err() {
        println!("browser: obsos:// not registered");
    }
    let opts = servo::Opts { config_dir: Some(profile.servo_dir()), ..servo::Opts::default() };
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(swgl_ctx::preferences())
        .protocol_registry(protocols)
        .event_loop_waker(Box::new(Waker(Arc::new(AtomicBool::new(false)))))
        .build();
    let content = content_manager(&servo, &profile);
    let delegate = Rc::new(Delegate {
        notes: RefCell::new(Vec::new()),
        context: context.clone(),
        clipboard: Rc::new(RefCell::new(String::new())),
        content,
        page_cursor: std::cell::Cell::new(window::CursorShape::Arrow),
        blocker: blocker.clone(),
        downloads: downloads.clone(),
        devlog: RefCell::new(devtools::DevLog::default()),
    });
    let draft_provider = vault.as_ref().and_then(|v| v.account().cloned());
    let mut shell = Shell {
        servo,
        context: context.clone(),
        delegate,
        tabs: Vec::new(),
        active: 0,
        address: String::new(),
        focus_address: false,
        opened: 0,
        profile,
        vault,
        page_data,
        panel: None,
        modal: need_master.then(|| Modal::Master { text: String::new(), error: String::new() }),
        infobars: Vec::new(),
        toasts: Vec::new(),
        filter: String::new(),
        syncer: Syncer::start(),
        sync: SyncUi {
            draft: draft_provider.unwrap_or_else(|| browser_sync::remote::Account::new(browser_sync::remote::Provider::GitHub)),
            label: String::new(),
            points: Vec::new(),
            status: String::new(),
            flow: None,
            sync_password: String::new(),
            // Первая автосинхронизация — через 10 с после запуска.
            next_auto: Instant::now() + Duration::from_secs(10),
            last_report: String::new(),
        },
        show_passwords: Default::default(),
        new_profile: String::new(),
        auth_tries: Vec::new(),
        find: Default::default(),
        tools: Default::default(),
        dev: Default::default(),
        closed: Vec::new(),
        quit: false,
    };
    if let Some(v) = &shell.vault {
        let key = v.sync_key;
        shell.profile.reload_logins(&key);
    }
    shell.update_page_data();

    // Первая вкладка: адрес из командной строки, иначе прошлый сеанс, иначе
    // домашняя страница.
    let session = shell.profile.settings.restore_session.then(|| shell.profile.load_session()).flatten();
    match (first_url.and_then(|a| shell.url_from_input(&a)), session) {
        (Some(url), _) => shell.new_tab(url, &window),
        (None, Some((urls, active))) => {
            for u in &urls {
                if let Ok(url) = url::Url::parse(u) {
                    shell.new_tab(url, &window);
                }
            }
            if shell.tabs.is_empty() {
                let home = shell.home_url();
                shell.new_tab(home, &window);
            } else {
                shell.activate(active.min(shell.tabs.len() - 1), &window);
            }
        }
        (None, None) => {
            let home = shell.home_url();
            shell.new_tab(home, &window);
        }
    }
    println!("browser: servo up in {:?}", t0.elapsed());
    if let Some(dir) = sync_folder {
        let mut account = browser_sync::remote::Account::new(browser_sync::remote::Provider::Folder);
        account.repo = dir;
        shell.sync.draft = account;
        shell.check_account();
    }

    let ctx = egui::Context::default();
    // Узкий экран (телефон, 720 точек): вкладки сверху — боковая панель
    // съела бы треть ширины. Только на этот запуск, в профиль не пишется.
    if window.width.get() < 800 && shell.profile.settings.look.side_tabs {
        shell.profile.settings.look.side_tabs = false;
        shell.resize_all(&window);
    }
    let mut look = shell.look().clone();
    ui::style(&ctx, &look);
    let mut renderer = EguiSoftwareRender::new(if window.swap_rb { ColorFieldOrder::Rgba } else { ColorFieldOrder::Bgra });
    let mut events: Vec<egui::Event> = Vec::new();
    let mut actions: Vec<Action> = Vec::new();
    let mut mask = 0i32;
    let mut mouse = DevicePoint::new(0.0, 0.0);
    let mut ui_due = Some(Instant::now());
    let mut ui_frames = 0u64;
    let mut over_ui = false;
    // Указатель над страницей (а не над оболочкой) — форму курсора выбирает
    // страница; иначе — egui.
    let mut over_page = false;
    let mut ui_cursor = egui::CursorIcon::Default;
    let started = Instant::now();
    let mut last_session = Instant::now();

    'run: loop {
        let mut input = false;
        let (ox, oy) = page_origin(&look);
        let page_right = window.width.get().saturating_sub(crate::shell::dev_dock()) as i32;
        let in_page = |x: i32, y: i32| x >= ox as i32 && y >= oy as i32 && x < page_right;
        let page_point = |x: i32, y: i32| DevicePoint::new((x - ox as i32) as f32, (y - oy as i32) as f32);
        while let Some(ev) = window.next_event() {
            input = true;
            let view = shell.active_view().cloned();
            let blocked = shell.modal.is_some();
            match ev.kind {
                WIN_CLOSE => break 'run,
                WIN_RESIZE => {
                    let (w, h) = (ev.x.max(1) as u32, ev.y.max(1) as u32);
                    println!("browser: resize asked {w}x{h}");
                    if window.resize(w, h) {
                        shell.resize_all(&window);
                        println!("browser: resized {w}x{h}");
                    } else {
                        println!("browser: resize to {w}x{h} failed: {}", std::io::Error::last_os_error());
                    }
                }
                WIN_MOVE => {
                    let pos = egui::pos2(ev.x as f32, ev.y as f32);
                    events.push(egui::Event::PointerMoved(pos));
                    over_ui = ctx.layer_id_at(pos).is_some_and(|l| l.order != egui::Order::Background);
                    over_page = in_page(ev.x, ev.y) && !over_ui && !blocked;
                    if over_page {
                        mouse = page_point(ev.x, ev.y);
                        if let Some(w) = &view {
                            w.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(WebViewPoint::Device(mouse))));
                        }
                    }
                }
                WIN_LEAVE => events.push(egui::Event::PointerGone),
                WIN_POINTER | WIN_RELEASE => {
                    let pressed = ev.kind == WIN_POINTER;
                    let button = match ev.code {
                        2 => egui::PointerButton::Secondary,
                        3 => egui::PointerButton::Middle,
                        _ => egui::PointerButton::Primary,
                    };
                    let pos = egui::pos2(ev.x as f32, ev.y as f32);
                    over_ui = ctx.layer_id_at(pos).is_some_and(|l| l.order != egui::Order::Background);
                    events.push(egui::Event::PointerMoved(pos));
                    events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: egui_modifiers(mask) });
                    let to_page = in_page(ev.x, ev.y) && !over_ui && !blocked;
                    if to_page {
                        mouse = page_point(ev.x, ev.y);
                        if let Some(w) = &view {
                            let button = if ev.code == 2 { MouseButton::Secondary } else { MouseButton::Primary };
                            let action = if pressed { MouseButtonAction::Down } else { MouseButtonAction::Up };
                            if pressed {
                                w.set_focused(true);
                                // Щелчок по странице закрывает выдвижную панель.
                                shell.panel = None;
                            }
                            w.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(action, button, WebViewPoint::Device(mouse))));
                        }
                    }
                    if pressed {
                        println!("browser: click at {},{}{}", ev.x, ev.y, if to_page { " (page)" } else { " (shell)" });
                    }
                }
                WIN_WHEEL => {
                    println!("browser: wheel {} at {},{}", ev.x, mouse.x, mouse.y);
                    if over_ui || blocked {
                        events.push(egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Line,
                            delta: egui::vec2(0.0, ev.x as f32),
                            phase: egui::TouchPhase::Move,
                            modifiers: egui_modifiers(mask),
                        });
                    } else if let Some(w) = &view {
                        let delta = DeviceVector2D::new(0.0, -(ev.x as f32) * 60.0);
                        w.notify_scroll_event(Scroll::Delta(WebViewVector::Device(delta)), WebViewPoint::Device(mouse));
                    }
                }
                WIN_KEY | WIN_KEYUP => {
                    let pressed = ev.kind == WIN_KEY;
                    mask = ev.x;
                    let (shift, ctrl, alt) = (mask & 1 != 0, mask & 2 != 0, mask & 4 != 0);
                    // `y` — латинский знак физической клавиши (ядро, user-abi). По
                    // нему, а не по коду, узнаются сочетания: Ctrl+H и
                    // Ctrl+Backspace приходят одним кодом 8, а Ctrl+буква в
                    // русской раскладке — латинской клавишей.
                    let latin = char::from_u32(ev.y as u32).filter(|c| c.is_ascii_graphic()).map(|c| c.to_ascii_lowercase());
                    let letter = if ctrl || alt { latin.filter(|c| c.is_ascii_alphanumeric()) } else { None };
                    let named = |n: u32| ev.code == WIN_KEY_NAMED + n;
                    let tabs = shell.tabs.len().max(1);
                    // Раскладка Chromium (Linux).
                    let shortcut = if !pressed || blocked {
                        None
                    } else if ctrl && alt && letter == Some('r') {
                        Some(Action::Reader)
                    } else if ctrl {
                        match (letter.or(latin), shift) {
                            (Some('t'), false) => Some(Action::NewTab),
                            (Some('t'), true) => Some(Action::Reopen),
                            (Some('w'), false) => Some(Action::Close(shell.active)),
                            (Some('l' | 'k' | 'e'), false) => Some(Action::FocusAddress),
                            (Some('d'), false) => Some(Action::ToggleBookmark),
                            (Some('h'), false) => Some(Action::Panel(Panel::History)),
                            (Some('j'), false) => Some(Action::Panel(Panel::Downloads)),
                            (Some('n'), true) => Some(Action::PrivateWindow),
                            (Some('i' | 'j'), true) => Some(Action::Panel(Panel::DevTools)),
                            (Some('o' | 'b'), true) => Some(Action::Panel(Panel::Bookmarks)),
                            (Some('f'), false) => Some(Action::FindOpen),
                            (Some('g'), _) => Some(Action::Find { back: shift }),
                            (Some('r'), _) => Some(Action::Reload),
                            (Some('q'), true) => Some(Action::Quit),
                            (Some('s'), true) => Some(Action::SyncNow(None)),
                            (Some(d @ '1'..='8'), false) => {
                                let n = d as usize - '1' as usize;
                                (n < shell.tabs.len()).then_some(Action::Activate(n))
                            }
                            (Some('9'), false) => Some(Action::Activate(tabs - 1)),
                            (Some('=' | '+'), _) => Some(Action::Zoom(1.1)),
                            (Some('-'), _) => Some(Action::Zoom(1.0 / 1.1)),
                            (Some('0'), false) => Some(Action::Zoom(0.0)),
                            _ if ev.code == 9 || named(8) => {
                                // Ctrl+Tab / Ctrl+PgDn — следующая, с Shift и Ctrl+PgUp — предыдущая.
                                let back = (ev.code == 9 && shift) || named(7);
                                Some(Action::Activate(if back { (shell.active + tabs - 1) % tabs } else { (shell.active + 1) % tabs }))
                            }
                            _ if named(7) => Some(Action::Activate((shell.active + tabs - 1) % tabs)),
                            _ if named(15) => Some(Action::Close(shell.active)),
                            _ if named(9) && shift => Some(Action::Panel(Panel::History)),
                            _ => None,
                        }
                    } else if alt {
                        match letter {
                            Some('d') => Some(Action::FocusAddress),
                            Some('f' | 'e') => Some(Action::Panel(Panel::Menu)),
                            _ if named(1) => Some(Action::Back),
                            _ if named(2) => Some(Action::Forward),
                            _ if named(5) => Some(Action::Home),
                            _ => None,
                        }
                    } else if named(16) {
                        Some(Action::Reload)
                    } else if named(20) {
                        Some(Action::Reader)
                    } else if named(23) {
                        Some(Action::Panel(Panel::DevTools))
                    } else if named(17) {
                        Some(Action::FocusAddress)
                    } else if named(21) {
                        Some(Action::Panel(Panel::Menu))
                    } else if named(14) {
                        Some(if shell.find.open { Action::Find { back: shift } } else { Action::FindOpen })
                    } else if ev.code == 27 && shell.find.open {
                        Some(Action::FindClose)
                    } else if ev.code == 27 && shell.panel.is_some() {
                        shell.panel = None;
                        println!("browser: panel closed");
                        ui_due = Some(Instant::now());
                        None
                    } else {
                        None
                    };
                    if let Some(action) = shortcut {
                        println!("browser: shortcut {action:?}");
                        if !shell.run(action, &window) {
                            break 'run;
                        }
                        continue;
                    }
                    let to_shell = blocked || shell.focus_address || ctx.egui_wants_keyboard_input();
                    if to_shell {
                        // Буфер обмена OBSOS: Ctrl+V/C/X в поле оболочки.
                        if pressed && ctrl && letter == Some('v') {
                            let text = window::clipboard_get().unwrap_or_else(|| shell.delegate.clipboard.borrow().clone());
                            events.push(egui::Event::Paste(text));
                        } else if pressed && ctrl && letter == Some('c') {
                            events.push(egui::Event::Copy);
                        } else if pressed && ctrl && letter == Some('x') {
                            events.push(egui::Event::Cut);
                        } else if let Some(key) = letter.and_then(|l| egui::Key::from_name(&l.to_ascii_uppercase().to_string())).or_else(|| egui_key(ev.code)) {
                            events.push(egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui_modifiers(mask) });
                        }
                        let printable = ev.code < WIN_KEY_NAMED && ev.code >= 32 && ev.code != 127;
                        if pressed && printable && !ctrl && !alt {
                            if let Some(c) = char::from_u32(ev.code) {
                                events.push(egui::Event::Text(c.to_string()));
                            }
                        }
                    } else if let Some(w) = &view {
                        // Ctrl+буква — странице буквой (Ctrl+A, Ctrl+C в полях сайта).
                        let key = match letter {
                            Some(l) if ctrl => Some(Key::Character(l.to_string())),
                            _ => servo_key(ev.code),
                        };
                        if let Some(key) = key {
                            let state = if pressed { KeyState::Down } else { KeyState::Up };
                            let mut event = KeyboardEvent::from_state_and_key(state, key);
                            event.event.modifiers = servo_modifiers(mask);
                            w.notify_input_event(InputEvent::Keyboard(event));
                        }
                    }
                }
                _ => {}
            }
        }

        shell.servo.spin_event_loop();
        let mut changed = shell.apply_notes(&window);
        changed |= shell.poll_sync(&window);
        if input || changed {
            ui_due = Some(Instant::now());
        }

        let mut ui_drawn = false;
        if ui_due.is_some_and(|due| Instant::now() >= due) {
            let (ww, wh) = (window.width.get(), window.height.get());
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(ww as f32, wh as f32))),
                time: Some(started.elapsed().as_secs_f64()),
                modifiers: egui_modifiers(mask),
                events: std::mem::take(&mut events),
                focused: true,
                ..Default::default()
            };
            let output = ctx.run_ui(raw, |ui| ui::shell_ui(ui, &mut shell, &mut actions));
            for command in &output.platform_output.commands {
                if let egui::OutputCommand::CopyText(text) = command {
                    *shell.delegate.clipboard.borrow_mut() = text.clone();
                    window::clipboard_set(text);
                }
            }
            ui_cursor = output.platform_output.cursor_icon;
            let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
            renderer.render_to_canvas(ww as usize, wh as usize, &primitives, &output.textures_delta, output.pixels_per_point);
            let delay = output.viewport_output.values().map(|v| v.repaint_delay).min().unwrap_or(Duration::MAX);
            ui_due = (delay < Duration::from_secs(3600)).then(|| Instant::now() + delay.max(Duration::from_millis(16)));
            ui_frames += 1;
            if ui_frames <= 2 || ui_frames % 200 == 0 {
                println!("browser: ui frame {ui_frames} ({} primitives)", primitives.len());
            }
            ui_drawn = true;
            for action in std::mem::take(&mut actions) {
                if !shell.run(action, &window) {
                    break 'run;
                }
                ui_due = Some(Instant::now());
            }
            // Инструменты разработчика открыли, закрыли или растянули — страница
            // уже или шире (встроены справа, как в Chromium).
            let want_dock = if shell.panel == Some(Panel::DevTools) {
                (shell.dev.width as u32).min(window.width.get().saturating_sub(300))
            } else {
                0
            };
            if want_dock != crate::shell::dev_dock() {
                crate::shell::set_dev_dock(want_dock);
                shell.resize_all(&window);
                ui_due = Some(Instant::now());
            }
            // Оформление поменялось — стиль egui и место страницы.
            if *shell.look() != look {
                let relayout = page_origin(shell.look()) != page_origin(&look);
                look = shell.look().clone();
                ui::style(&ctx, &look);
                shell.update_page_data();
                if relayout {
                    shell.resize_all(&window);
                }
                ui_due = Some(Instant::now());
            }
        }

        window.set_cursor(if over_page { shell.delegate.page_cursor.get() } else { egui_cursor(ui_cursor) });
        let page_fresh = std::mem::take(&mut page.borrow_mut().fresh);
        if page_fresh || ui_drawn {
            let (ox, oy) = page_origin(&look);
            window.compose(&page.borrow(), (ox as usize, oy as usize), &mut renderer);
        }
        if last_session.elapsed() > Duration::from_secs(20) {
            last_session = Instant::now();
            shell.profile.save_session(&shell.session_urls(), shell.active);
        }
        if shell.quit {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("browser: closing");
    shell.profile.save_session(&shell.session_urls(), shell.active);
    shell.profile.save_settings();
    // Последняя синхронизация — не дольше 10 с, окно уже не ждёт.
    if shell.account().is_some() && shell.profile.settings.sync_auto {
        shell.sync_now(None);
        let until = Instant::now() + Duration::from_secs(10);
        while shell.syncer.busy > 0 && Instant::now() < until {
            let _ = shell.poll_sync(&window);
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let private_dir = shell.profile.private.then(|| shell.profile.dir.clone());
    window.close();
    if let Some(dir) = private_dir {
        let _ = std::fs::remove_dir_all(dir);
    }
    // Всё своё уже сохранено. Разбор Servo на выходе (потоки, Service Worker)
    // мог висеть минутами — выходим сразу, память и потоки снимет ядро.
    println!("browser: closed");
    std::process::exit(0);
}
