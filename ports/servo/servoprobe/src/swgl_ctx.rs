//! Контекст рисования Servo на swgl — программном OpenGL из Firefox
//! («Software WebRender», N5, 08.11.2026). Без surfman, EGL и `dlopen`: у Servo
//! без `connection()` просто нет WebGL — штатный режим. Общий для `servoprobe`
//! (страница без окна) и `browser` (окно на столе FreeOS).

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CStr};
use std::os::raw::c_char;
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use gleam::gl::{self, Gl};
use servo::{DeviceIntRect, RenderingContext, RgbaImage};

// Функции swgl (C++, `extern "C"`, имена без `gl`): glow при создании
// спрашивает версию и расширения — только их и отдаём. Остальное glow
// оставляет пустым, и Servo его не зовёт (рисует через gleam).
extern "C" {
    fn GetString(name: u32) -> *const c_char;
    fn GetStringi(name: u32, index: u32) -> *const c_char;
    fn GetIntegerv(pname: u32, params: *mut i32);
}

fn glow_loader(name: &str) -> *const c_void {
    match name {
        "glGetString" => GetString as usize as *const c_void,
        "glGetStringi" => GetStringi as usize as *const c_void,
        "glGetIntegerv" => GetIntegerv as usize as *const c_void,
        _ => std::ptr::null(),
    }
}

/// Имя рендерера («Software WebRender»).
pub fn renderer_name() -> String {
    // SAFETY: `GetString` swgl отдаёт статическую строку.
    unsafe { CStr::from_ptr(GetString(gl::RENDERER)) }.to_string_lossy().into_owned()
}

/// Что делать с готовым кадром: (пиксели BGRA, строки снизу вверх, ширина,
/// высота). Окно копирует их на свою поверхность.
pub type PresentHook = Box<dyn Fn(&[u8], u32, u32)>;

pub struct SwglRenderingContext {
    ctx: swgl::Context,
    gl: Rc<dyn Gl>,
    glow: Arc<glow::Context>,
    size: Cell<PhysicalSize<u32>>,
    /// Кадр: BGRA, строка — ширина × 4, строки снизу вверх (порядок GL).
    frame: RefCell<Vec<u8>>,
    present_hook: RefCell<Option<PresentHook>>,
}

impl SwglRenderingContext {
    pub fn new(size: PhysicalSize<u32>) -> Rc<Self> {
        let ctx = swgl::Context::create();
        ctx.make_current();
        // SAFETY: загрузчик отдаёт адреса настоящих функций swgl или ноль.
        let glow = unsafe { glow::Context::from_loader_function(glow_loader) };
        let this = SwglRenderingContext {
            ctx,
            gl: Rc::new(ctx),
            glow: Arc::new(glow),
            size: Cell::new(size),
            frame: RefCell::new(Vec::new()),
            present_hook: RefCell::new(None),
        };
        this.realloc(size);
        Rc::new(this)
    }

    pub fn set_present_hook(&self, hook: PresentHook) {
        *self.present_hook.borrow_mut() = Some(hook);
    }

    fn realloc(&self, size: PhysicalSize<u32>) {
        let mut frame = self.frame.borrow_mut();
        frame.clear();
        frame.resize((size.width.max(1) * size.height.max(1) * 4) as usize, 0);
        self.ctx.make_current();
        self.ctx.init_default_framebuffer(
            0,
            0,
            size.width.max(1) as i32,
            size.height.max(1) as i32,
            (size.width.max(1) * 4) as i32,
            frame.as_mut_ptr() as *mut c_void,
        );
    }
}

impl RenderingContext for SwglRenderingContext {
    fn prepare_for_rendering(&self) {
        self.gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        // Кадр рисуется поверх прошлого: страница без фона во всё окно
        // (ошибка «Unknown content type», короткий документ) оставляла под
        // собой предыдущую страницу. Холст браузера — белый.
        self.gl.clear_color(1.0, 1.0, 1.0, 1.0);
        self.gl.clear(gl::COLOR_BUFFER_BIT);
    }

    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        self.ctx.make_current();
        self.gl.bind_framebuffer(gl::FRAMEBUFFER, 0);
        let (x, y, w, h) = (rect.min.x, rect.min.y, rect.width(), rect.height());
        let pixels = self.gl.read_pixels(x, y, w, h, gl::RGBA, gl::UNSIGNED_BYTE);
        // GL отдаёт строки снизу вверх — картинке нужны сверху вниз.
        let row = (w * 4) as usize;
        let mut flipped = vec![0u8; pixels.len()];
        for (i, line) in pixels.chunks_exact(row).enumerate() {
            let at = (h as usize - 1 - i) * row;
            flipped[at..at + row].copy_from_slice(line);
        }
        RgbaImage::from_raw(w as u32, h as u32, flipped)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.size.set(size);
        self.realloc(size);
    }

    fn present(&self) {
        // Кадр дорисован (swgl рисует синхронно) — отдать окну, если оно есть.
        if let Some(hook) = self.present_hook.borrow().as_ref() {
            self.ctx.make_current();
            self.gl.finish();
            let size = self.size.get();
            hook(&self.frame.borrow(), size.width, size.height);
        }
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.ctx.make_current();
        Ok(())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow.clone()
    }
}

/// Каталоги Servo (08.11.2026): профиль — `<дом>/.config/servo` (хранилище
/// сайтов, куки), временные файлы — `<дом>/.cache/servo/tmp` через `TMPDIR`:
/// `/tmp` на установленной FreeOS нет, и хранилище Servo падало на
/// `tempfile::tempdir()`. Дом — `HOME` (его ставит стартовый код libc), иначе
/// `fallback`, иначе `/home`.
pub fn prepare_dirs(fallback: Option<&std::path::Path>) -> std::path::PathBuf {
    use std::path::PathBuf;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| fallback.map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/home"));
    let config = home.join(".config").join("servo");
    let tmp = home.join(".cache").join("servo").join("tmp");
    let _ = std::fs::create_dir_all(&config);
    let _ = std::fs::create_dir_all(&tmp);
    // SAFETY (edition 2021: безопасно): до запуска потоков Servo.
    std::env::set_var("TMPDIR", &tmp);
    config
}

/// Настройки Servo на FreeOS. Корни сертификатов — встроенные Mozilla
/// (`webpki-roots`), как у Servo на Android: системного хранилища CA у FreeOS
/// пока нет, и проверка «как в системе» роняла сетевой поток Servo на старте
/// («No CA certificates were loaded from the system»).
pub fn preferences() -> servo::Preferences {
    let mut prefs = servo::Preferences::default();
    prefs.network_use_webpki_roots = true;
    prefs
}
