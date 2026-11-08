//! `browser` — OBSOS, браузер FreeOS на Servo (N7).
//!
//! Окно стола FreeOS (`freeos_window_*` из libfreeos: поверхность отображена
//! в память программы, `commit` — показать). Слева — боковая панель вкладок,
//! сверху над страницей — назад/вперёд/обновить и адресная строка (раскладка
//! по мотивам Zervo, © Goddv, MPL-2.0; код свой). Оболочку рисует egui через
//! `egui_software_backend` прямо в пиксели, страницу — swgl (программный
//! OpenGL, `swgl_ctx`). Все вкладки — `WebView` одного контекста swgl,
//! видима только активная (`show`/`hide`).
//!
//! Кадр окна: копия последнего кадра страницы в своей области, поверх —
//! холст egui (панели непрозрачны, над страницей холст прозрачен).
//!
//! Клавиши: Ctrl+T — новая вкладка, Ctrl+W — закрыть, Ctrl+L — адресная
//! строка, Ctrl+Tab — следующая вкладка, F5 — обновить, Alt+←/→ — назад и
//! вперёд. Адрес — первым аргументом; без него — страница новой вкладки.

#[path = "../swgl_ctx.rs"]
mod swgl_ctx;

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CString};
use std::os::raw::{c_char, c_int};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use servo::{
    CreateNewWebViewRequest, DevicePoint, DeviceVector2D, EventLoopWaker, InputEvent, Key, KeyState,
    KeyboardEvent, LoadStatus, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent,
    NamedKey, RenderingContext, Scroll, Servo, ServoBuilder, WebView, WebViewBuilder, WebViewDelegate,
    WebViewId, WebViewPoint, WebViewVector,
};
use swgl_ctx::SwglRenderingContext;

// ── Геометрия оболочки (точки окна; масштаб 1) ──────────────────────────────
// Сценарий `servo-window` целится в эти места (`Aim::Content`) — менять вместе.

/// Ширина боковой панели вкладок.
const SIDEBAR_W: u32 = 216;
/// Высота панели над страницей (назад/вперёд/обновить/адрес).
const TOOLBAR_H: u32 = 44;
/// Шапка боковой панели «OBSOS» и кнопка «Новая вкладка» под ней.
const BRAND_H: f32 = 34.0;
const NEW_TAB_H: f32 = 30.0;
/// Строка вкладки в списке.
const TAB_ROW_H: f32 = 30.0;
const SIDE_PAD: i8 = 8;

// ── Окна FreeOS (libfreeos, libc/freeos/window.c) ───────────────────────────

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct WinEvent {
    kind: u32,
    code: u32,
    x: i32,
    y: i32,
}

extern "C" {
    fn freeos_window_open(title: *const c_char, width: u32, height: u32, surface: *mut *mut c_void) -> i64;
    fn freeos_window_commit(id: i64, x: i32, y: i32, width: i32, height: i32) -> c_int;
    fn freeos_window_event(id: i64, event: *mut WinEvent) -> c_int;
    fn freeos_window_resize(id: i64, width: u32, height: u32) -> *mut c_void;
    fn freeos_window_style(id: i64, flags: u32) -> c_int;
    fn freeos_window_title(id: i64, title: *const c_char) -> c_int;
    fn freeos_window_close(id: i64) -> c_int;
    fn freeos_screen(width: *mut u32, height: *mut u32, pixel_format: *mut u32) -> c_int;
}

const WIN_KEY: u32 = 1;
const WIN_POINTER: u32 = 2;
const WIN_CLOSE: u32 = 3;
const WIN_MOVE: u32 = 4;
const WIN_LEAVE: u32 = 5;
const WIN_RELEASE: u32 = 6;
const WIN_RESIZE: u32 = 7;
const WIN_KEYUP: u32 = 9;
const WIN_WHEEL: u32 = 10;
const WIN_STYLE_RESIZABLE: u32 = 1;
const WIN_KEY_NAMED: u32 = 0x0100_0000;
const PIXEL_RGB: u32 = 1;

/// Окно: поверхность (u32 на точку, строки сверху вниз) и её размер.
struct Window {
    id: i64,
    surface: Cell<*mut u32>,
    width: Cell<u32>,
    height: Cell<u32>,
    /// Красный в младшем байте (FREEOS_PIXEL_RGB): байты точки R,G,B,A;
    /// иначе B,G,R,A.
    swap_rb: bool,
}

impl Window {
    fn open(title: &str, width: u32, height: u32) -> Option<Window> {
        let (mut sw, mut sh, mut fmt) = (0u32, 0u32, 0u32);
        // SAFETY: три выходных числа.
        unsafe { freeos_screen(&mut sw, &mut sh, &mut fmt) };
        let width = if sw > 0 { width.min(sw.saturating_sub(40)) } else { width };
        let height = if sh > 0 { height.min(sh.saturating_sub(80)) } else { height };
        let title = CString::new(title).ok()?;
        let mut surface: *mut c_void = std::ptr::null_mut();
        // SAFETY: заголовок — строка C, поверхность — выходной адрес.
        let id = unsafe { freeos_window_open(title.as_ptr(), width, height, &mut surface) };
        if id <= 0 || surface.is_null() {
            println!("browser: window open failed ({id})");
            return None;
        }
        // SAFETY: окно наше.
        unsafe { freeos_window_style(id, WIN_STYLE_RESIZABLE) };
        println!("browser: window {width}x{height}, screen {sw}x{sh}, pixel format {fmt}");
        Some(Window {
            id,
            surface: Cell::new(surface as *mut u32),
            width: Cell::new(width),
            height: Cell::new(height),
            swap_rb: fmt == PIXEL_RGB,
        })
    }

    fn next_event(&self) -> Option<WinEvent> {
        let mut ev = WinEvent::default();
        // SAFETY: окно наше, структура — по договору user-abi `WinEvent`.
        (unsafe { freeos_window_event(self.id, &mut ev) } == 1).then_some(ev)
    }

    fn resize(&self, width: u32, height: u32) -> bool {
        // SAFETY: окно наше; прежняя поверхность после вызова недействительна.
        let surface = unsafe { freeos_window_resize(self.id, width, height) };
        if surface.is_null() {
            return false;
        }
        self.surface.set(surface as *mut u32);
        self.width.set(width);
        self.height.set(height);
        true
    }

    fn set_title(&self, title: &str) {
        // Не длиннее 64 байт UTF-8 (MAX_TITLE), по границе символа.
        let mut cut = title.len().min(60);
        while !title.is_char_boundary(cut) {
            cut -= 1;
        }
        if let Ok(text) = CString::new(&title[..cut]) {
            // SAFETY: окно наше, строка C.
            unsafe { freeos_window_title(self.id, text.as_ptr()) };
        }
    }

    /// Поверхность целиком (строки подряд, без зазоров).
    fn pixels(&self) -> &mut [u32] {
        let len = self.width.get() as usize * self.height.get() as usize;
        // SAFETY: поверхность окна — width*height точек, отображена в нашу
        // память и живёт до следующего `resize`; ссылка дальше кадра не живёт.
        unsafe { std::slice::from_raw_parts_mut(self.surface.get(), len) }
    }

    fn commit(&self) {
        // SAFETY: окно наше; нулевой размер — всё окно.
        unsafe { freeos_window_commit(self.id, 0, 0, 0, 0) };
    }

    fn close(&self) {
        // SAFETY: окно наше.
        unsafe { freeos_window_close(self.id) };
    }
}

/// Размер области страницы при таком окне.
fn page_size(width: u32, height: u32) -> PhysicalSize<u32> {
    PhysicalSize::new(width.saturating_sub(SIDEBAR_W).max(1), height.saturating_sub(TOOLBAR_H).max(1))
}

/// Последний кадр страницы — уже в формате точек окна, строки сверху вниз.
#[derive(Default)]
struct PageFrame {
    pixels: Vec<u32>,
    width: usize,
    height: usize,
    fresh: bool,
    count: u64,
}

impl PageFrame {
    /// Кадр swgl (BGRA, строки снизу вверх).
    fn store(&mut self, frame: &[u8], fw: u32, fh: u32, swap_rb: bool) {
        let (w, h) = (fw as usize, fh as usize);
        self.pixels.resize(w * h, 0xffff_ffff);
        self.width = w;
        self.height = h;
        for y in 0..h {
            let src = &frame[(h - 1 - y) * w * 4..][..w * 4];
            let dst = &mut self.pixels[y * w..][..w];
            for (px, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                let (b, g, r) = (s[0] as u32, s[1] as u32, s[2] as u32);
                *px = if swap_rb { 0xff00_0000 | (b << 16) | (g << 8) | r } else { 0xff00_0000 | (r << 16) | (g << 8) | b };
            }
        }
        self.fresh = true;
        self.count += 1;
        if self.count <= 3 || self.count % 100 == 0 {
            println!("browser: frame {} presented {w}x{h}", self.count);
        }
    }
}

// ── Servo: события вкладок копятся и разбираются в главном цикле ────────────

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

enum Note {
    Frame(WebViewId),
    Title(WebViewId, Option<String>),
    Url(WebViewId, url::Url),
    Load(WebViewId, LoadStatus),
    History,
    /// Страница открыла окно (`target=_blank`, `window.open`) — новая вкладка.
    Opened(WebView),
}

struct Delegate {
    notes: RefCell<Vec<Note>>,
    context: Rc<SwglRenderingContext>,
}

impl Delegate {
    fn note(&self, note: Note) {
        self.notes.borrow_mut().push(note);
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        // Кадр рисует и показывает встраивающая программа (`paint`, затем
        // `present`) — в главном цикле и только у видимой вкладки.
        self.note(Note::Frame(webview.id()));
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        println!("browser: load status {status:?}");
        self.note(Note::Load(webview.id(), status));
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        println!("browser: title {title:?}");
        self.note(Note::Title(webview.id(), title));
    }

    fn notify_url_changed(&self, webview: WebView, url: url::Url) {
        let shown = url.as_str();
        println!("browser: url {}", if shown.starts_with("data:") { "data:…" } else { shown });
        self.note(Note::Url(webview.id(), url));
    }

    fn notify_history_changed(&self, _webview: WebView, _entries: Vec<url::Url>, _current: usize) {
        self.note(Note::History);
    }

    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        let webview = request
            .builder(self.context.clone() as Rc<dyn RenderingContext>)
            .delegate(parent.delegate())
            .build();
        self.note(Note::Opened(webview));
    }
}

// ── Страница новой вкладки ──────────────────────────────────────────────────

const NEW_TAB: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Новая вкладка</title><style>\
body{margin:0;font-family:sans-serif;background:linear-gradient(160deg,#1b1f3a,#2a1f4a 55%,#123a44);color:#e8ecf8;min-height:100vh}\
.wrap{max-width:720px;margin:0 auto;padding:56px 28px 40px}\
h1{margin:0;font-size:44px;letter-spacing:2px}\
.sub{margin:6px 0 26px;opacity:.75;font-size:15px}\
#hello{font-size:20px;margin:0 0 18px}\
form{display:flex;margin:0 0 30px}\
input{flex:1;font-size:17px;padding:12px 16px;border-radius:10px 0 0 10px;border:1px solid #4a4f7a;background:#14172c;color:#fff}\
button{font-size:17px;padding:12px 20px;border:0;border-radius:0 10px 10px 0;background:linear-gradient(90deg,#7a4fd8,#2bb3b1);color:#fff}\
.tiles{display:flex;flex-wrap:wrap}\
.tiles a{display:block;width:150px;margin:0 14px 14px 0;padding:16px 14px;border-radius:12px;background:rgba(255,255,255,.07);border:1px solid rgba(255,255,255,.12);color:#fff;text-decoration:none}\
.tiles b{display:block;font-size:22px;margin-bottom:6px}\
.tiles span{font-size:13px;opacity:.7}\
.about{margin-top:40px;padding:18px 20px;border-radius:12px;background:rgba(0,0,0,.25);font-size:13px;line-height:1.6;opacity:.85}\
.about a{color:#8fd8ff}\
</style></head><body><div class=\"wrap\">\
<h1>OBSOS</h1><p class=\"sub\">Open Browser for System Open Source — браузер FreeOS</p>\
<p id=\"hello\">Привет!</p>\
<form action=\"https://duckduckgo.com/html/\" method=\"get\"><input name=\"q\" placeholder=\"Поиск в DuckDuckGo\"><button>Найти</button></form>\
<div class=\"tiles\">\
<a href=\"https://servo.org/\"><b>Servo</b><span>servo.org</span></a>\
<a href=\"https://ru.wikipedia.org/\"><b>Википедия</b><span>ru.wikipedia.org</span></a>\
<a href=\"https://github.com/\"><b>GitHub</b><span>github.com</span></a>\
<a href=\"https://example.com/\"><b>Example</b><span>example.com</span></a>\
<a href=\"https://www.openstreetmap.org/\"><b>Карты</b><span>openstreetmap.org</span></a>\
<a href=\"file:///opt/obsos/share/NOTICE.md\"><b>Лицензии</b><span>NOTICE.md</span></a>\
</div>\
<div class=\"about\"><b>О программе.</b> Движок Servo — © The Servo Project Developers, \
<a href=\"https://servo.org\">servo.org</a> (MPL-2.0); JavaScript — SpiderMonkey, рисование — swgl/WebRender, \
© Mozilla (MPL-2.0). Оболочка — своя, на egui; дизайн по мотивам Zervo — © Goddv, \
<a href=\"https://github.com/Goddv/Zervo\">github.com/Goddv/Zervo</a> (MPL-2.0). \
Текст лицензии — <a href=\"https://mozilla.org/MPL/2.0/\">mozilla.org/MPL/2.0</a>; полный список — \
<a href=\"file:///opt/obsos/share/NOTICE.md\">NOTICE.md</a>.</div>\
</div><script>\
var h=new Date().getHours();\
document.getElementById('hello').textContent=(h<5?'Доброй ночи':h<12?'Доброе утро':h<18?'Добрый день':'Добрый вечер')+' — JavaScript работает.';\
</script></body></html>";

fn data_url(html: &str) -> String {
    let mut out = String::from("data:text/html;charset=utf-8,");
    for b in html.bytes() {
        if b.is_ascii_alphanumeric() {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn new_tab_url() -> url::Url {
    url::Url::parse(&data_url(NEW_TAB)).expect("new tab page")
}

/// Что набрали в адресной строке → адрес: схема как есть, путь — файл,
/// похожее на имя сайта — https, остальное — поиск.
fn url_from_input(text: &str) -> Option<url::Url> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let full = if text.contains("://") || text.starts_with("data:") || text.starts_with("about:") {
        text.to_string()
    } else if text.starts_with('/') {
        format!("file://{text}")
    } else if !text.contains(' ') && (text.contains('.') || text.starts_with("localhost")) {
        format!("https://{text}")
    } else {
        let mut search = url::Url::parse("https://duckduckgo.com/html/").ok()?;
        search.query_pairs_mut().append_pair("q", text);
        return Some(search);
    };
    url::Url::parse(&full).ok()
}

/// Адрес для показа: внутренние страницы (`data:`) — пустая строка.
fn shown_url(url: &url::Url) -> String {
    if url.scheme() == "data" {
        String::new()
    } else {
        url.as_str().to_string()
    }
}

// ── Клавиши окна → Servo и egui ─────────────────────────────────────────────

/// Клавиша окна → клавиша Servo.
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
                4 => NamedKey::F5,
                _ => return None,
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

/// Клавиша окна → клавиша egui (буквы — для сочетаний с Ctrl).
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

/// Буква клавиши: сама буква или управляющий код Ctrl+буква (1..=26).
fn letter_of(code: u32) -> Option<char> {
    match code {
        1..=26 => Some((b'a' + code as u8 - 1) as char),
        c => char::from_u32(c).filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()),
    }
}

// ── Оболочка ────────────────────────────────────────────────────────────────

struct Tab {
    webview: WebView,
    title: String,
    url: String,
    loading: bool,
}

#[derive(Clone, PartialEq)]
enum Action {
    NewTab,
    Activate(usize),
    Close(usize),
    Back,
    Forward,
    Reload,
    Go(String),
    FocusAddress,
}

struct Shell {
    servo: Servo,
    context: Rc<SwglRenderingContext>,
    delegate: Rc<Delegate>,
    tabs: Vec<Tab>,
    active: usize,
    address: String,
    /// Поставить фокус в адресную строку на следующем кадре egui (и до тех
    /// пор клавиши — ей, а не странице).
    focus_address: bool,
    opened: usize,
}

impl Shell {
    fn active_view(&self) -> Option<&WebView> {
        self.tabs.get(self.active).map(|t| &t.webview)
    }

    fn open_tab(&mut self, webview: WebView, window: &Window) {
        self.opened += 1;
        self.tabs.push(Tab { webview, title: String::from("Новая вкладка"), url: String::new(), loading: true });
        let index = self.tabs.len() - 1;
        println!("browser: tab {} opened ({} open)", self.opened, self.tabs.len());
        self.activate(index, window);
    }

    fn new_tab(&mut self, url: url::Url, window: &Window) {
        let webview = WebViewBuilder::new(&self.servo, self.context.clone() as Rc<dyn RenderingContext>)
            .delegate(self.delegate.clone())
            .url(url)
            .build();
        self.open_tab(webview, window);
    }

    fn activate(&mut self, index: usize, window: &Window) {
        if index >= self.tabs.len() {
            return;
        }
        for (i, tab) in self.tabs.iter().enumerate() {
            if i != index {
                tab.webview.hide();
            }
        }
        self.active = index;
        let tab = &self.tabs[index];
        tab.webview.show();
        tab.webview.set_focused(true);
        tab.webview.paint();
        self.context.present();
        self.address = tab.url.clone();
        window.set_title(&format!("OBSOS — {}", tab.title));
        println!("browser: tab {} active", index + 1);
    }

    /// Закрыть вкладку; `false` — вкладок не осталось (закрыть окно).
    fn close_tab(&mut self, index: usize, window: &Window) -> bool {
        if index >= self.tabs.len() {
            return true;
        }
        self.tabs.remove(index);
        println!("browser: tab {} closed ({} open)", index + 1, self.tabs.len());
        if self.tabs.is_empty() {
            return false;
        }
        let next = if self.active > index || self.active >= self.tabs.len() { self.active.saturating_sub(1) } else { self.active };
        self.activate(next.min(self.tabs.len() - 1), window);
        true
    }

    fn tab_of(&mut self, id: WebViewId) -> Option<(usize, &mut Tab)> {
        self.tabs.iter_mut().enumerate().find(|(_, t)| t.webview.id() == id)
    }

    /// Разобрать накопленные события Servo. Возвращает: менялась ли оболочка.
    fn apply_notes(&mut self, window: &Window) -> bool {
        let notes = std::mem::take(&mut *self.delegate.notes.borrow_mut());
        let mut changed = false;
        let active = self.active;
        for note in notes {
            match note {
                Note::Frame(id) => {
                    if self.active_view().is_some_and(|w| w.id() == id) {
                        let webview = self.tabs[active].webview.clone();
                        webview.paint();
                        self.context.present();
                    }
                }
                Note::Title(id, title) => {
                    if let Some((i, tab)) = self.tab_of(id) {
                        tab.title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).unwrap_or_else(|| {
                            if tab.url.is_empty() { String::from("Новая вкладка") } else { tab.url.clone() }
                        });
                        if i == active {
                            window.set_title(&format!("OBSOS — {}", tab.title));
                        }
                        changed = true;
                    }
                }
                Note::Url(id, url) => {
                    let focused = self.focus_address;
                    let shown = shown_url(&url);
                    if let Some((i, tab)) = self.tab_of(id) {
                        tab.url = shown.clone();
                        if i == active && !focused {
                            self.address = shown;
                        }
                        changed = true;
                    }
                }
                Note::Load(id, status) => {
                    if let Some((_, tab)) = self.tab_of(id) {
                        tab.loading = status != LoadStatus::Complete;
                        changed = true;
                    }
                }
                Note::History => changed = true,
                Note::Opened(webview) => {
                    webview.resize(self.context.size());
                    self.open_tab(webview, window);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Выполнить действие оболочки. `false` — закрыть окно.
    fn run(&mut self, action: Action, window: &Window) -> bool {
        match action {
            Action::NewTab => {
                self.new_tab(new_tab_url(), window);
                self.address.clear();
                self.focus_address = true;
            }
            Action::Activate(i) => self.activate(i, window),
            Action::Close(i) => return self.close_tab(i, window),
            Action::Back => {
                if let Some(w) = self.active_view().filter(|w| w.can_go_back()) {
                    w.go_back(1);
                    println!("browser: back");
                }
            }
            Action::Forward => {
                if let Some(w) = self.active_view().filter(|w| w.can_go_forward()) {
                    w.go_forward(1);
                    println!("browser: forward");
                }
            }
            Action::Reload => {
                if let Some(w) = self.active_view() {
                    w.reload();
                    println!("browser: reload");
                }
            }
            Action::Go(text) => match url_from_input(&text) {
                Some(url) => {
                    println!("browser: go {}", url.as_str());
                    if let Some(tab) = self.tabs.get_mut(self.active) {
                        tab.url = shown_url(&url);
                        tab.loading = true;
                        tab.webview.load(url);
                        tab.webview.set_focused(true);
                    }
                }
                None => println!("browser: go ignored (empty)"),
            },
            Action::FocusAddress => self.focus_address = true,
        }
        true
    }
}

// ── Рисование оболочки (egui) ───────────────────────────────────────────────

const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x7a, 0x4f, 0xd8);
const TEAL: egui::Color32 = egui::Color32::from_rgb(0x2b, 0xb3, 0xb1);
const SIDEBAR_BG: egui::Color32 = egui::Color32::from_rgb(0x17, 0x1a, 0x2e);
const BAR_BG: egui::Color32 = egui::Color32::from_rgb(0x20, 0x24, 0x3c);
const TAB_ACTIVE: egui::Color32 = egui::Color32::from_rgb(0x34, 0x2c, 0x5c);
const TAB_HOVER: egui::Color32 = egui::Color32::from_rgb(0x26, 0x29, 0x45);
const TEXT: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xec, 0xf8);
const TEXT_DIM: egui::Color32 = egui::Color32::from_rgb(0x9a, 0xa2, 0xc0);

fn style(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BAR_BG;
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x12, 0x14, 0x26);
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, TEXT);
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(0x2a, 0x2f, 0x4d);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0x3a, 0x3f, 0x66);
    visuals.widgets.active.weak_bg_fill = ACCENT;
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(6);
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(6);
    ctx.set_visuals(visuals);
}

/// Обрезать подпись до `max` символов с многоточием.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Один проход интерфейса: панели и действия пользователя.
fn shell_ui(ui: &mut egui::Ui, shell: &mut Shell, actions: &mut Vec<Action>) {
    actions.clear();
    let address_id = egui::Id::new("obsos-address");

    egui::Panel::left("obsos-tabs")
        .exact_size(SIDEBAR_W as f32)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(SIDEBAR_BG).inner_margin(egui::Margin::symmetric(SIDE_PAD, 0)))
        .show_inside(ui, |ui| {
            let width = ui.available_width();
            // Шапка: название с полосой акцентных цветов значка.
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, BRAND_H), egui::Sense::hover());
            let painter = ui.painter();
            painter.circle_filled(egui::pos2(rect.left() + 10.0, rect.center().y), 7.0, ACCENT);
            painter.circle_filled(egui::pos2(rect.left() + 20.0, rect.center().y - 5.0), 3.0, TEAL);
            painter.text(
                egui::pos2(rect.left() + 30.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "OBSOS",
                egui::FontId::proportional(18.0),
                TEXT,
            );
            let new_tab = egui::Button::new(egui::RichText::new("+  Новая вкладка").color(TEXT))
                .fill(egui::Color32::from_rgb(0x2a, 0x2f, 0x4d))
                .corner_radius(8);
            if ui.add_sized([width, NEW_TAB_H], new_tab).clicked() {
                actions.push(Action::NewTab);
            }
            ui.add_space(6.0);
            let mut spacing = ui.spacing().item_spacing;
            spacing.y = 2.0;
            ui.spacing_mut().item_spacing = spacing;
            for (i, tab) in shell.tabs.iter().enumerate() {
                let (rect, response) = ui.allocate_exact_size(egui::vec2(width, TAB_ROW_H), egui::Sense::click());
                let close = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 14.0, rect.center().y),
                    egui::vec2(20.0, 20.0),
                );
                let over_close = response.hover_pos().is_some_and(|p| close.contains(p));
                let fill = if i == shell.active {
                    TAB_ACTIVE
                } else if response.hovered() {
                    TAB_HOVER
                } else {
                    SIDEBAR_BG
                };
                let painter = ui.painter();
                painter.rect_filled(rect, 7.0, fill);
                if i == shell.active {
                    painter.rect_filled(
                        egui::Rect::from_min_size(rect.left_top() + egui::vec2(0.0, 6.0), egui::vec2(3.0, TAB_ROW_H - 12.0)),
                        2.0,
                        TEAL,
                    );
                }
                let mark = if tab.loading { "◌ " } else { "" };
                painter.text(
                    egui::pos2(rect.left() + 12.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("{mark}{}", clip(&tab.title, 20)),
                    egui::FontId::proportional(14.0),
                    if i == shell.active { TEXT } else { TEXT_DIM },
                );
                if response.hovered() || i == shell.active {
                    if over_close {
                        painter.rect_filled(close, 4.0, egui::Color32::from_rgb(0x55, 0x3a, 0x5a));
                    }
                    painter.text(close.center(), egui::Align2::CENTER_CENTER, "×", egui::FontId::proportional(16.0), TEXT_DIM);
                }
                if response.clicked() {
                    actions.push(if over_close { Action::Close(i) } else { Action::Activate(i) });
                }
                if response.middle_clicked() {
                    actions.push(Action::Close(i));
                }
            }
        });

    egui::Panel::top("obsos-bar")
        .exact_size(TOOLBAR_H as f32)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::new().fill(BAR_BG).inner_margin(egui::Margin::symmetric(8, 7)))
        .show_inside(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let (back, forward) = shell
                    .active_view()
                    .map(|w| (w.can_go_back(), w.can_go_forward()))
                    .unwrap_or((false, false));
                let button = |text: &str| egui::Button::new(egui::RichText::new(text).size(16.0)).min_size(egui::vec2(30.0, 28.0));
                if ui.add_enabled(back, button("←")).on_hover_text("Назад (Alt+←)").clicked() {
                    actions.push(Action::Back);
                }
                if ui.add_enabled(forward, button("→")).on_hover_text("Вперёд (Alt+→)").clicked() {
                    actions.push(Action::Forward);
                }
                if ui.add(button("⟳")).on_hover_text("Обновить (F5)").clicked() {
                    actions.push(Action::Reload);
                }
                if shell.focus_address {
                    ui.memory_mut(|m| m.request_focus(address_id));
                    shell.focus_address = false;
                }
                let edit = egui::TextEdit::singleline(&mut shell.address)
                    .id(address_id)
                    .hint_text("Адрес или поиск")
                    .font(egui::FontId::proportional(15.0))
                    .margin(egui::Margin::symmetric(10, 5))
                    .desired_width(ui.available_width() - 4.0);
                let response = ui.add(edit);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::Go(shell.address.clone()));
                }
            });
        });
}

// ── Главный цикл ────────────────────────────────────────────────────────────

fn main() {
    let t0 = Instant::now();
    println!("browser: start");
    let Some(window) = Window::open("OBSOS — Браузер", 1024, 700) else {
        std::process::exit(1);
    };
    let window = Rc::new(window);
    let page = Rc::new(RefCell::new(PageFrame::default()));
    let context = SwglRenderingContext::new(page_size(window.width.get(), window.height.get()));
    {
        let (page, swap_rb) = (page.clone(), window.swap_rb);
        context.set_present_hook(Box::new(move |frame, w, h| page.borrow_mut().store(frame, w, h, swap_rb)));
    }
    let _ = context.make_current();
    println!("browser: renderer {}", swgl_ctx::renderer_name());

    let config_dir = swgl_ctx::prepare_dirs(None);
    println!("browser: profile {}", config_dir.display());
    let opts = servo::Opts { config_dir: Some(config_dir), ..servo::Opts::default() };
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(swgl_ctx::preferences())
        .event_loop_waker(Box::new(Waker(Arc::new(AtomicBool::new(false)))))
        .build();
    let delegate = Rc::new(Delegate { notes: RefCell::new(Vec::new()), context: context.clone() });
    let mut shell = Shell {
        servo,
        context: context.clone(),
        delegate,
        tabs: Vec::new(),
        active: 0,
        address: String::new(),
        focus_address: false,
        opened: 0,
    };
    let first = std::env::args().nth(1).and_then(|a| url_from_input(&a)).unwrap_or_else(new_tab_url);
    shell.new_tab(first, &window);
    println!("browser: servo up in {:?}", t0.elapsed());

    // egui: интерфейс в пиксели, порядок байтов — как у поверхности окна.
    let ctx = egui::Context::default();
    style(&ctx);
    let mut renderer =
        EguiSoftwareRender::new(if window.swap_rb { ColorFieldOrder::Rgba } else { ColorFieldOrder::Bgra });
    let mut events: Vec<egui::Event> = Vec::new();
    let mut actions: Vec<Action> = Vec::new();
    let mut mask = 0i32;
    let mut mouse = DevicePoint::new(0.0, 0.0);
    let mut ui_due = Some(Instant::now());
    let mut ui_frames = 0u64;
    let started = Instant::now();

    let in_page = |x: i32, y: i32| x >= SIDEBAR_W as i32 && y >= TOOLBAR_H as i32;
    let page_point = |x: i32, y: i32| DevicePoint::new((x - SIDEBAR_W as i32) as f32, (y - TOOLBAR_H as i32) as f32);

    'run: loop {
        let mut input = false;
        while let Some(ev) = window.next_event() {
            input = true;
            let view = shell.active_view().cloned();
            match ev.kind {
                WIN_CLOSE => break 'run,
                WIN_RESIZE => {
                    let (w, h) = (ev.x.max(1) as u32, ev.y.max(1) as u32);
                    if window.resize(w, h) {
                        let size = page_size(w, h);
                        for tab in &shell.tabs {
                            tab.webview.resize(size);
                        }
                        println!("browser: resized {w}x{h}");
                    }
                }
                WIN_MOVE => {
                    events.push(egui::Event::PointerMoved(egui::pos2(ev.x as f32, ev.y as f32)));
                    if in_page(ev.x, ev.y) {
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
                    events.push(egui::Event::PointerMoved(pos));
                    events.push(egui::Event::PointerButton { pos, button, pressed, modifiers: egui_modifiers(mask) });
                    if in_page(ev.x, ev.y) {
                        mouse = page_point(ev.x, ev.y);
                        if let Some(w) = &view {
                            let button = if ev.code == 2 { MouseButton::Secondary } else { MouseButton::Primary };
                            let action = if pressed { MouseButtonAction::Down } else { MouseButtonAction::Up };
                            if pressed {
                                w.set_focused(true);
                            }
                            w.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                                action,
                                button,
                                WebViewPoint::Device(mouse),
                            )));
                        }
                    }
                    if pressed {
                        println!("browser: click at {},{}{}", ev.x, ev.y, if in_page(ev.x, ev.y) { " (page)" } else { " (shell)" });
                    }
                }
                WIN_WHEEL => {
                    // Щелчок колеса — 60 точек; к себе (x < 0) — вниз по странице.
                    // Позиция — последняя известная (у события колеса своей нет).
                    println!("browser: wheel {} at {},{}", ev.x, mouse.x, mouse.y);
                    if let Some(w) = &view {
                        let delta = DeviceVector2D::new(0.0, -(ev.x as f32) * 60.0);
                        w.notify_scroll_event(Scroll::Delta(WebViewVector::Device(delta)), WebViewPoint::Device(mouse));
                    }
                }
                WIN_KEY | WIN_KEYUP => {
                    let pressed = ev.kind == WIN_KEY;
                    mask = ev.x;
                    let (ctrl, alt) = (mask & 2 != 0, mask & 4 != 0);
                    // Сочетания оболочки — раньше страницы и адресной строки.
                    let shortcut = if !pressed {
                        None
                    } else if ctrl {
                        match (letter_of(ev.code), ev.code) {
                            (Some('t'), _) => Some(Action::NewTab),
                            (Some('w'), _) => Some(Action::Close(shell.active)),
                            (Some('l'), _) => Some(Action::FocusAddress),
                            (_, 9) => Some(Action::Activate((shell.active + 1) % shell.tabs.len().max(1))),
                            _ => None,
                        }
                    } else if alt && ev.code == WIN_KEY_NAMED + 1 {
                        Some(Action::Back)
                    } else if alt && ev.code == WIN_KEY_NAMED + 2 {
                        Some(Action::Forward)
                    } else if ev.code == WIN_KEY_NAMED + 16 {
                        Some(Action::Reload)
                    } else {
                        None
                    };
                    if let Some(action) = shortcut {
                        println!("browser: shortcut");
                        if !shell.run(action, &window) {
                            break 'run;
                        }
                        continue;
                    }
                    let to_shell = shell.focus_address || ctx.wants_keyboard_input();
                    if to_shell {
                        if let Some(key) = egui_key(ev.code) {
                            events.push(egui::Event::Key {
                                key,
                                physical_key: None,
                                pressed,
                                repeat: false,
                                modifiers: egui_modifiers(mask),
                            });
                        }
                        let printable = ev.code < WIN_KEY_NAMED && ev.code >= 32 && ev.code != 127;
                        if pressed && printable && !ctrl && !alt {
                            if let Some(c) = char::from_u32(ev.code) {
                                events.push(egui::Event::Text(c.to_string()));
                            }
                        }
                    } else if let (Some(w), Some(key)) = (&view, servo_key(ev.code)) {
                        let state = if pressed { KeyState::Down } else { KeyState::Up };
                        let mut event = KeyboardEvent::from_state_and_key(state, key);
                        event.event.modifiers = servo_modifiers(mask);
                        w.notify_input_event(InputEvent::Keyboard(event));
                    }
                }
                _ => {}
            }
        }

        shell.servo.spin_event_loop();
        let changed = shell.apply_notes(&window);
        if input || changed {
            ui_due = Some(Instant::now());
        }

        // Интерфейс: по вводу, по событиям вкладок и когда egui сам просит
        // (мигание курсора в адресной строке).
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
            let output = ctx.run_ui(raw, |ui| shell_ui(ui, &mut shell, &mut actions));
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
        }

        // Кадр окна: страница + интерфейс поверх.
        let page_fresh = std::mem::take(&mut page.borrow_mut().fresh);
        if page_fresh || ui_drawn {
            compose(&window, &page.borrow(), &mut renderer);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("browser: closing");
    shell.tabs.clear();
    drop(shell);
    window.close();
    println!("browser: closed");
}

/// Собрать кадр окна: копия кадра страницы в её области, холст egui поверх.
fn compose(window: &Window, page: &PageFrame, renderer: &mut EguiSoftwareRender) {
    let (ww, wh) = (window.width.get() as usize, window.height.get() as usize);
    let (px, py) = ((SIDEBAR_W as usize).min(ww), (TOOLBAR_H as usize).min(wh));
    let surface = window.pixels();
    let (w, h) = ((ww - px).min(page.width), (wh - py).min(page.height));
    for y in py..wh {
        let row = &mut surface[y * ww..][..ww];
        if y - py < h {
            row[px..px + w].copy_from_slice(&page.pixels[(y - py) * page.width..][..w]);
            row[px + w..].fill(0xffff_ffff);
        } else {
            row[px..].fill(0xffff_ffff);
        }
    }
    // SAFETY: [u8; 4] и u32 одного размера; выравнивание u8 не строже.
    let bytes = unsafe { std::slice::from_raw_parts_mut(surface.as_mut_ptr() as *mut [u8; 4], surface.len()) };
    let mut buffer = BufferMutRef::new(bytes, ww, wh);
    renderer.blit_canvas_to_buffer(&mut buffer);
    window.commit();
}
