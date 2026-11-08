//! `browser` — Servo в окне на столе FreeOS (N7, первая видимая версия,
//! 08.11.2026). Без своей оболочки (адресная строка, вкладки — дальше, на
//! egui): окно, в нём страница, мышь, колесо, клавиатура, изменение размера.
//!
//! Рисует swgl (программный OpenGL, `swgl_ctx`); готовый кадр копируется на
//! поверхность окна (`freeos_window_*` из libfreeos: поверхность отображена в
//! память программы, `commit` — показать). Адрес — первым аргументом; без
//! него — встроенная стартовая страница.

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
use servo::{
    DevicePoint, DeviceVector2D, EventLoopWaker, InputEvent, Key, KeyState, KeyboardEvent,
    LoadStatus, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, NamedKey,
    RenderingContext, Scroll, ServoBuilder, WebView, WebViewBuilder, WebViewDelegate, WebViewPoint,
    WebViewVector,
};
use swgl_ctx::SwglRenderingContext;

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
    /// Красный в младшем байте (FREEOS_PIXEL_RGB) — надо переставить R и B
    /// у кадра swgl (BGRA).
    swap_rb: bool,
    frames: Cell<u64>,
}

impl Window {
    fn open(title: &str, width: u32, height: u32) -> Option<Rc<Window>> {
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
        Some(Rc::new(Window {
            id,
            surface: Cell::new(surface as *mut u32),
            width: Cell::new(width),
            height: Cell::new(height),
            swap_rb: fmt == PIXEL_RGB,
            frames: Cell::new(0),
        }))
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

    /// Кадр swgl (BGRA, строки снизу вверх) → поверхность окна.
    fn blit(&self, frame: &[u8], fw: u32, fh: u32) {
        let (w, h) = (self.width.get().min(fw), self.height.get().min(fh));
        let surface = self.surface.get();
        let stride = self.width.get() as usize;
        for y in 0..h as usize {
            let src = &frame[(fh as usize - 1 - y) * fw as usize * 4..][..w as usize * 4];
            // SAFETY: строка `y` поверхности — `stride` точек, пишем `w` <= stride.
            let dst = unsafe { std::slice::from_raw_parts_mut(surface.add(y * stride), w as usize) };
            for (px, s) in dst.iter_mut().zip(src.chunks_exact(4)) {
                let (b, g, r) = (s[0] as u32, s[1] as u32, s[2] as u32);
                *px = if self.swap_rb { 0xff00_0000 | (b << 16) | (g << 8) | r } else { 0xff00_0000 | (r << 16) | (g << 8) | b };
            }
        }
        // SAFETY: окно наше; нулевой размер — всё окно.
        unsafe { freeos_window_commit(self.id, 0, 0, 0, 0) };
        let n = self.frames.get() + 1;
        self.frames.set(n);
        if n <= 3 || n % 100 == 0 {
            println!("browser: frame {n} presented {w}x{h}");
        }
    }

    fn close(&self) {
        // SAFETY: окно наше.
        unsafe { freeos_window_close(self.id) };
    }
}

// ── Servo ───────────────────────────────────────────────────────────────────

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

struct Delegate {
    window: Rc<Window>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        webview.paint();
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        println!("browser: load status {status:?}");
    }

    fn notify_page_title_changed(&self, _webview: WebView, title: Option<String>) {
        let title = title.filter(|t| !t.trim().is_empty());
        println!("browser: title {title:?}");
        match title {
            Some(t) => self.window.set_title(&format!("Servo — {}", t.trim())),
            None => self.window.set_title("Servo"),
        }
    }

    fn notify_url_changed(&self, _webview: WebView, url: url::Url) {
        let shown = url.as_str();
        println!("browser: url {}", if shown.starts_with("data:") { "data:…" } else { shown });
    }
}

const START: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>FreeOS</title></head>\
<body style=\"margin:0;font-family:sans-serif;background:#f4f6fb;color:#1d2433\">\
<div style=\"background:linear-gradient(90deg,#2d6cdf,#7a4fd8);color:white;padding:28px 36px\">\
<h1 style=\"margin:0;font-size:34px\">Браузер FreeOS</h1>\
<p style=\"margin:8px 0 0;font-size:17px;opacity:.9\">Движок Servo · JavaScript SpiderMonkey · рисует swgl на процессоре</p></div>\
<div style=\"padding:24px 36px;font-size:16px;line-height:1.5\">\
<p>Это первая видимая сборка: страница, мышь, колесо и клавиатура. Адресная строка и вкладки — следующим шагом.</p>\
<p id=\"js\" style=\"padding:12px 16px;background:#fff;border-radius:8px;border:1px solid #d8deea\">JavaScript: не выполнялся</p>\
<p><button id=\"btn\" style=\"font-size:16px;padding:8px 18px;border-radius:6px;border:0;background:#2d6cdf;color:#fff\">Нажми меня</button>\
 <span id=\"count\"></span></p>\
<p><input id=\"field\" placeholder=\"Можно печатать сюда\" style=\"font-size:16px;padding:8px;width:320px\"></p>\
<ul><li><a href=\"https://example.com/\">example.com</a></li><li><a href=\"https://servo.org/\">servo.org</a></li></ul>\
<div style=\"height:600px;background:repeating-linear-gradient(45deg,#e7ebf5 0 20px,#f4f6fb 20px 40px);border-radius:8px\"></div>\
<p>Конец страницы — прокрутка колесом работает.</p></div>\
<script>document.getElementById('js').textContent='JavaScript: работает, '+new Date().getFullYear()+' год, '+navigator.userAgent.split(' ')[0];\
var n=0;document.getElementById('btn').onclick=function(){n++;document.getElementById('count').textContent='нажато '+n+' раз';console.log('browser-click '+n)};</script>\
</body></html>";

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

fn start_url() -> url::Url {
    match std::env::args().nth(1) {
        Some(arg) => {
            let text = if arg.contains("://") || arg.starts_with("data:") || arg.starts_with("about:") {
                arg
            } else if arg.starts_with('/') {
                format!("file://{arg}")
            } else {
                format!("https://{arg}")
            };
            url::Url::parse(&text).unwrap_or_else(|_| url::Url::parse(&data_url(START)).expect("start page"))
        }
        None => url::Url::parse(&data_url(START)).expect("start page"),
    }
}

/// Клавиша окна → клавиша Servo.
fn key_of(code: u32) -> Option<Key> {
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

fn modifiers_of(mask: i32) -> Modifiers {
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

fn main() {
    let t0 = Instant::now();
    println!("browser: start");
    let Some(window) = Window::open("Servo", 1024, 700) else {
        std::process::exit(1);
    };
    let context = SwglRenderingContext::new(PhysicalSize::new(window.width.get(), window.height.get()));
    {
        let window = window.clone();
        context.set_present_hook(Box::new(move |frame, w, h| window.blit(frame, w, h)));
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
    let delegate = Rc::new(Delegate { window: window.clone() });
    let webview = WebViewBuilder::new(&servo, context.clone() as Rc<dyn RenderingContext>)
        .delegate(delegate.clone())
        .url(start_url())
        .build();
    println!("browser: servo up in {:?}", t0.elapsed());

    let mouse = RefCell::new(DevicePoint::new(0.0, 0.0));
    let point = |x: i32, y: i32| {
        let p = DevicePoint::new(x as f32, y as f32);
        *mouse.borrow_mut() = p;
        WebViewPoint::Device(p)
    };
    'run: loop {
        while let Some(ev) = window.next_event() {
            match ev.kind {
                WIN_CLOSE => break 'run,
                WIN_RESIZE => {
                    let (w, h) = (ev.x.max(1) as u32, ev.y.max(1) as u32);
                    if window.resize(w, h) {
                        webview.resize(PhysicalSize::new(w, h));
                        println!("browser: resized {w}x{h}");
                    }
                }
                WIN_MOVE => {
                    webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(point(ev.x, ev.y))));
                }
                WIN_POINTER | WIN_RELEASE => {
                    let button = if ev.code == 2 { MouseButton::Secondary } else { MouseButton::Primary };
                    let action = if ev.kind == WIN_POINTER { MouseButtonAction::Down } else { MouseButtonAction::Up };
                    webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        action,
                        button,
                        point(ev.x, ev.y),
                    )));
                    if ev.kind == WIN_POINTER {
                        println!("browser: click at {},{}", ev.x, ev.y);
                    }
                }
                WIN_WHEEL => {
                    // Щелчок колеса — 60 точек; к себе (x < 0) — вниз по странице.
                    let delta = DeviceVector2D::new(0.0, -(ev.x as f32) * 60.0);
                    let at = *mouse.borrow();
                    webview.notify_scroll_event(
                        Scroll::Delta(WebViewVector::Device(delta)),
                        WebViewPoint::Device(at),
                    );
                }
                WIN_KEY | WIN_KEYUP => {
                    if let Some(key) = key_of(ev.code) {
                        let state = if ev.kind == WIN_KEY { KeyState::Down } else { KeyState::Up };
                        let mut event = KeyboardEvent::from_state_and_key(state, key);
                        event.event.modifiers = modifiers_of(ev.x);
                        webview.notify_input_event(InputEvent::Keyboard(event));
                    }
                }
                WIN_LEAVE => {}
                _ => {}
            }
        }
        servo.spin_event_loop();
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("browser: closing");
    drop(webview);
    drop(servo);
    window.close();
    println!("browser: closed");
}
