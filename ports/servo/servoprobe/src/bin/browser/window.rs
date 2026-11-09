//! Окно стола FreeOS (`freeos_window_*` из libfreeos), кадр страницы и
//! сборка кадра окна; запуск других программ (`SYS_SPAWN`).

use std::cell::Cell;
use std::ffi::{CString, c_void};
use std::os::raw::{c_char, c_int};

use egui_software_backend::{BufferMutRef, EguiSoftwareRender};

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct WinEvent {
    pub kind: u32,
    pub code: u32,
    pub x: i32,
    pub y: i32,
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

pub const WIN_KEY: u32 = 1;
pub const WIN_POINTER: u32 = 2;
pub const WIN_CLOSE: u32 = 3;
pub const WIN_MOVE: u32 = 4;
pub const WIN_LEAVE: u32 = 5;
pub const WIN_RELEASE: u32 = 6;
pub const WIN_RESIZE: u32 = 7;
pub const WIN_KEYUP: u32 = 9;
pub const WIN_WHEEL: u32 = 10;
const WIN_STYLE_RESIZABLE: u32 = 1;
pub const WIN_KEY_NAMED: u32 = 0x0100_0000;
const PIXEL_RGB: u32 = 1;

/// Окно: поверхность (u32 на точку, строки сверху вниз) и её размер.
pub struct Window {
    id: i64,
    surface: Cell<*mut u32>,
    pub width: Cell<u32>,
    pub height: Cell<u32>,
    /// Красный в младшем байте (FREEOS_PIXEL_RGB): байты точки R,G,B,A;
    /// иначе B,G,R,A.
    pub swap_rb: bool,
    cursor: Cell<CursorShape>,
}

impl Window {
    pub fn open(title: &str, width: u32, height: u32) -> Option<Window> {
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
            cursor: Cell::new(CursorShape::Arrow),
        })
    }

    pub fn next_event(&self) -> Option<WinEvent> {
        let mut ev = WinEvent::default();
        // SAFETY: окно наше, структура — по договору user-abi `WinEvent`.
        (unsafe { freeos_window_event(self.id, &mut ev) } == 1).then_some(ev)
    }

    pub fn resize(&self, width: u32, height: u32) -> bool {
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

    pub fn set_cursor(&self, shape: CursorShape) {
        if self.cursor.get() != shape {
            self.cursor.set(shape);
            // SAFETY: окно наше, вызов без адресов.
            unsafe { syscall3(SYS_WINCURSOR, self.id as usize, shape as usize, 0) };
        }
    }

    pub fn set_title(&self, title: &str) {
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
    #[allow(clippy::mut_from_ref)]
    fn pixels(&self) -> &mut [u32] {
        let len = self.width.get() as usize * self.height.get() as usize;
        // SAFETY: поверхность окна — width*height точек, отображена в нашу
        // память и живёт до следующего `resize`; ссылка дальше кадра не живёт.
        unsafe { std::slice::from_raw_parts_mut(self.surface.get(), len) }
    }

    pub fn close(&self) {
        // SAFETY: окно наше.
        unsafe { freeos_window_close(self.id) };
    }

    /// Собрать кадр окна: копия кадра страницы в её прямоугольнике, холст
    /// egui поверх (панели непрозрачны, над страницей холст прозрачен).
    pub fn compose(&self, page: &PageFrame, at: (usize, usize), renderer: &mut EguiSoftwareRender) {
        let (ww, wh) = (self.width.get() as usize, self.height.get() as usize);
        let (px, py) = (at.0.min(ww), at.1.min(wh));
        let surface = self.pixels();
        let (w, h) = ((ww - px).min(page.width), (wh - py).min(page.height));
        let blank = if self.swap_rb { 0xffff_ffff } else { 0xffff_ffff };
        for y in py..wh {
            let row = &mut surface[y * ww..][..ww];
            if y - py < h {
                row[px..px + w].copy_from_slice(&page.pixels[(y - py) * page.width..][..w]);
                row[px + w..].fill(blank);
            } else {
                row[px..].fill(blank);
            }
        }
        // SAFETY: [u8; 4] и u32 одного размера; выравнивание u8 не строже.
        let bytes = unsafe { std::slice::from_raw_parts_mut(surface.as_mut_ptr() as *mut [u8; 4], surface.len()) };
        let mut buffer = BufferMutRef::new(bytes, ww, wh);
        renderer.blit_canvas_to_buffer(&mut buffer);
        // SAFETY: окно наше; нулевой размер — всё окно.
        unsafe { freeos_window_commit(self.id, 0, 0, 0, 0) };
    }
}

/// Последний кадр страницы — уже в формате точек окна, строки сверху вниз.
#[derive(Default)]
pub struct PageFrame {
    pub pixels: Vec<u32>,
    pub width: usize,
    pub height: usize,
    pub fresh: bool,
    pub count: u64,
}

impl PageFrame {
    /// Кадр swgl (BGRA, строки снизу вверх).
    pub fn store(&mut self, frame: &[u8], fw: u32, fh: u32, swap_rb: bool) {
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

const SYS_SPAWN: usize = 21;
const SYS_CLIPBOARD: usize = 85;
const CLIP_SET: usize = 1;
const CLIP_GET: usize = 2;
const SYS_WINCURSOR: usize = 89;
const SYS_TZINFO: usize = 90;
/// Наследовать консоль родителя (`FREEOS_SPAWN_INHERIT`).
const SPAWN_INHERIT: isize = -1;
/// Нет такого вызова (`ERR_NO_SYSCALL`: ядро старше браузера).
const ERR_NOSYS: isize = -2;

/// Ловушка ядра FreeOS, как `freeos_syscall` в libc/freeos/freeos-syscall.h:
/// номер и три аргумента.
///
/// # Safety
/// Аргументы-адреса должны быть верны для вызова `number`.
unsafe fn syscall3(number: usize, a0: usize, a1: usize, a2: usize) -> isize {
    let result: isize;
    // SAFETY: договор ловушки FreeOS; адреса проверяет вызывающий.
    unsafe {
        #[cfg(target_arch = "x86_64")]
        std::arch::asm!("int 0x80", inlateout("rax") number as isize => result,
            in("rdi") a0, in("rsi") a1, in("rdx") a2, options(nostack));
        #[cfg(target_arch = "aarch64")]
        std::arch::asm!("svc #0", in("x8") number, inlateout("x0") a0 as isize => result,
            in("x1") a1, in("x2") a2, options(nostack));
    }
    result
}

/// Запустить программу, не дожидаясь её. Строка разбирается тем же
/// разборщиком, что у оболочки (пробелы — разделители), поэтому `&` и `?` в
/// адресе не мешают — в отличие от `system()`, которая такие знаки
/// отвергает. Возвращает номер задачи.
pub fn spawn(command: &str) -> Result<i64, i64> {
    let bytes = command.as_bytes();
    // SAFETY: строка живёт до возврата из вызова.
    let result = unsafe { syscall3(SYS_SPAWN, bytes.as_ptr() as usize, bytes.len(), SPAWN_INHERIT as usize) };
    if result < 0 { Err(result as i64) } else { Ok(result as i64) }
}

/// Буфер обмена системы (общий с терминалом, Блокнотом, «Файлами»).
pub fn clipboard_set(text: &str) {
    // SAFETY: строка живёт до возврата.
    unsafe { syscall3(SYS_CLIPBOARD, CLIP_SET, text.as_ptr() as usize, text.len()) };
}

pub fn clipboard_get() -> Option<String> {
    let mut buf = vec![0u8; 4096];
    loop {
        // SAFETY: буфер наш, длина — его.
        let n = unsafe { syscall3(SYS_CLIPBOARD, CLIP_GET, buf.as_mut_ptr() as usize, buf.len()) };
        if n < 0 {
            return None;
        }
        let n = n as usize;
        if n <= buf.len() {
            buf.truncate(n);
            return String::from_utf8(buf).ok();
        }
        buf = vec![0u8; n];
    }
}

/// Смещение часового пояса системы от UTC в минутах (`None` — ядро старое).
pub fn timezone_minutes() -> Option<i32> {
    // SAFETY: вызов без адресов.
    let r = unsafe { syscall3(SYS_TZINFO, 0, 0, 0) };
    (r != ERR_NOSYS && r.abs() <= 18 * 60).then_some(r as i32)
}

/// Форма указателя над окном (`WIN_CURSOR_*`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CursorShape {
    Arrow = 0,
    Text = 1,
    Hand = 2,
    Move = 3,
    NotAllowed = 4,
    ResizeEw = 5,
    ResizeNs = 6,
}

pub const CHROMIUM: &str = "/opt/chromium/bin/chromium";

pub fn chromium_installed() -> bool {
    std::path::Path::new(CHROMIUM).exists()
}

/// Открыть адрес в Chromium (пакет со слоем Linux).
pub fn open_in_chromium(url: &str) -> Result<i64, i64> {
    let safe: String = url.chars().filter(|c| !c.is_whitespace()).collect();
    spawn(&format!("/opt/debian/bin/bash {CHROMIUM} {safe}"))
}
