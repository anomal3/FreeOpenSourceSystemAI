//! `servoprobe` — Servo 0.8 на FreeOS (N5/N6): страница без окна.
//!
//! Рисует swgl — программный OpenGL из Firefox («Software WebRender»): свой
//! [`RenderingContext`] поверх `swgl::Context`, без surfman, EGL и `dlopen`
//! (у Servo без `connection()` просто нет WebGL — штатный режим). Страница —
//! `data:` URL (цветные блоки и текст), снимок — `WebView::take_screenshot`,
//! проверка — цвета пикселей в известных местах и «чернила» текста; PNG
//! сохраняется в каталог из аргумента. Строки `servoprobe: ok …` ждёт
//! сценарий `servo-page`.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dpi::PhysicalSize;
use servo::{
    EventLoopWaker, LoadStatus, RenderingContext, RgbaImage, ServoBuilder, WebView, WebViewBuilder,
    WebViewDelegate,
};

static FAILED: AtomicUsize = AtomicUsize::new(0);

fn check(name: &str, ok: bool) {
    if ok {
        println!("servoprobe: ok {name}");
    } else {
        FAILED.fetch_add(1, Ordering::SeqCst);
        println!("servoprobe: FAILED {name}");
    }
}

const W: u32 = 640;
const H: u32 = 480;

mod swgl_ctx;
use swgl_ctx::SwglRenderingContext;

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

#[derive(Default)]
struct Delegate {
    loaded: Cell<bool>,
    frames: Cell<u32>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.frames.set(self.frames.get() + 1);
        webview.paint();
    }

    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        println!("servoprobe: load status {status:?}");
        if matches!(status, LoadStatus::Complete) {
            self.loaded.set(true);
        }
    }
}

const PAGE: &str = "<!doctype html><html><body style=\"margin:0;background:#00aa77\">\
<div style=\"width:200px;height:100px;background:#ff0000\"></div>\
<p style=\"margin:20px;font:40px sans-serif;color:#ffffff\">Привет, FreeOS!</p>\
<div style=\"position:absolute;left:400px;top:300px;width:100px;height:100px;background:#0000ff\"></div>\
</body></html>";

/// `data:` URL: всё, кроме букв и цифр ASCII, — `%XX`.
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

fn spin_until(servo: &servo::Servo, what: &str, limit: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() > limit {
            println!("servoprobe: timeout waiting for {what}");
            return false;
        }
        servo.spin_event_loop();
        std::thread::sleep(Duration::from_millis(1));
    }
    println!("servoprobe: {what} in {:?}", start.elapsed());
    true
}

fn main() {
    let out_dir = std::env::args().nth(1).map(PathBuf::from);
    println!("servoprobe: start");
    let t0 = Instant::now();
    let context = SwglRenderingContext::new(PhysicalSize::new(W, H));
    println!("servoprobe: gl renderer {:?}", swgl_ctx::renderer_name());
    check("swgl context", context.make_current().is_ok());

    let config_dir = swgl_ctx::prepare_dirs(out_dir.as_deref());
    println!("servoprobe: profile {}", config_dir.display());
    let opts = servo::Opts { config_dir: Some(config_dir), ..servo::Opts::default() };
    let servo = ServoBuilder::default()
        .opts(opts)
        .preferences(swgl_ctx::preferences())
        .event_loop_waker(Box::new(Waker(Arc::new(AtomicBool::new(false)))))
        .build();
    println!("servoprobe: servo built in {:?}", t0.elapsed());
    check("servo built", true);

    let delegate = Rc::new(Delegate::default());
    let url = url::Url::parse(&data_url(PAGE)).expect("data url");
    let webview = WebViewBuilder::new(&servo, context.clone() as Rc<dyn RenderingContext>)
        .delegate(delegate.clone())
        .url(url)
        .build();
    let loaded = spin_until(&servo, "page load", Duration::from_secs(180), || delegate.loaded.get());
    check("page loaded", loaded);

    let shot: Rc<RefCell<Option<Result<RgbaImage, String>>>> = Rc::new(RefCell::new(None));
    let slot = shot.clone();
    webview.take_screenshot(None, move |result| {
        *slot.borrow_mut() = Some(result.map_err(|e| format!("{e:?}")));
    });
    spin_until(&servo, "screenshot", Duration::from_secs(180), || shot.borrow().is_some());
    println!("servoprobe: frames painted {}", delegate.frames.get());

    match shot.borrow_mut().take() {
        Some(Ok(image)) => inspect(&image, out_dir),
        Some(Err(e)) => check(&format!("screenshot ({e})"), false),
        None => check("screenshot", false),
    }
    drop(webview);
    drop(servo);
    let failed = FAILED.load(Ordering::SeqCst);
    println!("servoprobe: done in {:?}, {failed} check(s) failed", t0.elapsed());
    std::process::exit(if failed == 0 { 0 } else { 1 });
}

/// Цвета в известных местах и «чернила» текста.
fn inspect(image: &RgbaImage, out_dir: Option<PathBuf>) {
    let (w, h) = image.dimensions();
    println!("servoprobe: screenshot {w}x{h}");
    check("screenshot size", w == W && h == H);
    let near = |x: u32, y: u32, rgb: [u8; 3]| {
        let p = image.get_pixel(x.min(w - 1), y.min(h - 1)).0;
        let ok = p[..3].iter().zip(rgb).all(|(a, b)| (*a as i32 - b as i32).abs() <= 8);
        println!("servoprobe: pixel ({x},{y}) = {:?}, want {:?}", &p[..3], rgb);
        ok
    };
    check("red block", near(100, 50, [255, 0, 0]));
    check("blue block", near(450, 350, [0, 0, 255]));
    check("page background", near(600, 450, [0, 0xaa, 0x77]));
    // Текст: белые точки в полосе строки (абзац ниже красного блока).
    let mut ink = 0u32;
    for y in 110..220.min(h) {
        for x in 0..w.min(600) {
            let p = image.get_pixel(x, y).0;
            if p[0] > 200 && p[1] > 200 && p[2] > 200 {
                ink += 1;
            }
        }
    }
    println!("servoprobe: text ink {ink} white pixels");
    check("text drawn", ink > 500);
    if let Some(dir) = out_dir {
        let path = dir.join("servo.png");
        match image.save(&path) {
            Ok(()) => {
                println!("servoprobe: saved {}", path.display());
                // Снимок — в журнал (последовательный порт) base64-строками:
                // стенд собирает из них PNG на хосте, без доступа к диску VM.
                if let Ok(png) = std::fs::read(&path) {
                    let text = base64(&png);
                    println!("servoprobe: png-begin {} bytes", png.len());
                    for chunk in text.as_bytes().chunks(96) {
                        println!("servoprobe-png {}", std::str::from_utf8(chunk).unwrap_or(""));
                    }
                    println!("servoprobe: png-end");
                }
            }
            Err(e) => println!("servoprobe: png not saved ({e})"),
        }
    }
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ABC[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
