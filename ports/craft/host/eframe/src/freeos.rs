//! Окно стола FreeOS (`freeos_window_*` из libfreeos), буфер обмена и форма
//! указателя (`SYS_CLIPBOARD`, `SYS_WINCURSOR`) — как в OBSOS
//! (ports/servo/servoprobe/src/bin/browser/window.rs). Открыт для крейтов-
//! заменителей `rfd` (окно выбора файла) и `arboard` (буфер обмена).

use std::cell::Cell;
use std::ffi::{CString, c_void};
use std::os::raw::{c_char, c_int};

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
pub struct WinEvent {
    pub kind: u32,
    pub code: u32,
    pub x: i32,
    pub y: i32,
}

unsafe extern "C" {
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
/// Именованные клавиши: `WIN_KEY_NAMED + n` (стрелки 1–4, Home/End 5/6,
/// PgUp/PgDn 7/8, Delete 9, Insert 11, F1–F12 12–23).
pub const WIN_KEY_NAMED: u32 = 0x0100_0000;
const PIXEL_RGB: u32 = 1;

/// Размер экрана (0×0 — ядро не сказало).
pub fn screen() -> (u32, u32) {
    let (mut sw, mut sh, mut fmt) = (0u32, 0u32, 0u32);
    // SAFETY: три выходных числа.
    unsafe { freeos_screen(&mut sw, &mut sh, &mut fmt) };
    (sw, sh)
}

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
    /// Окно не больше экрана (с полями под рамку и панель задач).
    pub fn open(title: &str, width: u32, height: u32, resizable: bool) -> Option<Window> {
        let (mut sw, mut sh, mut fmt) = (0u32, 0u32, 0u32);
        // SAFETY: три выходных числа.
        unsafe { freeos_screen(&mut sw, &mut sh, &mut fmt) };
        let width = if sw > 0 { width.min(sw.saturating_sub(40)) } else { width }.max(64);
        let height = if sh > 0 { height.min(sh.saturating_sub(80)) } else { height }.max(48);
        let title = CString::new(clip_title(title)).ok()?;
        let mut surface: *mut c_void = std::ptr::null_mut();
        // SAFETY: заголовок — строка C, поверхность — выходной адрес.
        let id = unsafe { freeos_window_open(title.as_ptr(), width, height, &mut surface) };
        if id <= 0 || surface.is_null() {
            log::error!("eframe: window open failed ({id})");
            return None;
        }
        if resizable {
            // SAFETY: окно наше.
            unsafe { freeos_window_style(id, WIN_STYLE_RESIZABLE) };
        }
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

    /// Новый размер; прежняя поверхность после вызова недействительна.
    pub fn resize(&self, width: u32, height: u32) -> bool {
        // SAFETY: окно наше.
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
        if let Ok(text) = CString::new(clip_title(title)) {
            // SAFETY: окно наше, строка C.
            unsafe { freeos_window_title(self.id, text.as_ptr()) };
        }
    }

    /// Поверхность целиком (строки подряд, без зазоров) как точки по 4 байта.
    #[allow(clippy::mut_from_ref)]
    pub fn pixels(&self) -> &mut [[u8; 4]] {
        let len = self.width.get() as usize * self.height.get() as usize;
        // SAFETY: поверхность окна — width*height точек u32, отображена в нашу
        // память и живёт до следующего `resize`; [u8; 4] того же размера, а
        // выравнивание u8 не строже. Ссылка дальше кадра не живёт.
        unsafe { std::slice::from_raw_parts_mut(self.surface.get() as *mut [u8; 4], len) }
    }

    /// Показать поверхность (всё окно).
    pub fn commit(&self) {
        // SAFETY: окно наше; нулевой размер — всё окно.
        unsafe { freeos_window_commit(self.id, 0, 0, 0, 0) };
    }

    pub fn close(&self) {
        // SAFETY: окно наше.
        unsafe { freeos_window_close(self.id) };
    }
}

/// Заголовок окна — не длиннее 64 байт UTF-8 (MAX_TITLE), по границе символа.
fn clip_title(title: &str) -> &str {
    let mut cut = title.len().min(60);
    while !title.is_char_boundary(cut) {
        cut -= 1;
    }
    &title[..cut]
}

const SYS_SPAWN: usize = 21;
const SYS_CLIPBOARD: usize = 85;
const CLIP_SET: usize = 1;
const CLIP_GET: usize = 2;
const SYS_WINCURSOR: usize = 89;
/// Наследовать консоль родителя (`FREEOS_SPAWN_INHERIT`).
const SPAWN_INHERIT: isize = -1;

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

/// Запустить программу, не дожидаясь её (ссылки `OpenUrl` — в OBSOS).
pub fn spawn(command: &str) -> Result<i64, i64> {
    let bytes = command.as_bytes();
    // SAFETY: строка живёт до возврата из вызова.
    let result = unsafe { syscall3(SYS_SPAWN, bytes.as_ptr() as usize, bytes.len(), SPAWN_INHERIT as usize) };
    if result < 0 { Err(result as i64) } else { Ok(result as i64) }
}

/// Буфер обмена системы (общий с терминалом, Блокнотом, «Файлами», OBSOS).
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

pub fn cursor_of(icon: egui::CursorIcon) -> CursorShape {
    use CursorShape as S;
    use egui::CursorIcon as C;
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

/// Клавиатура и мышь окна → события egui. Общая часть главного окна и окна
/// выбора файла (`rfd`).
#[derive(Default)]
pub struct InputState {
    /// Маска модификаторов последнего события клавиатуры: 1 Shift, 2 Ctrl, 4 Alt.
    pub mask: i32,
    pub pointer: egui::Pos2,
}

/// Что окно сказало помимо событий egui.
#[derive(Default, Debug)]
pub struct Translated {
    pub close: bool,
    pub resize: Option<(u32, u32)>,
}

impl InputState {
    pub fn modifiers(&self) -> egui::Modifiers {
        modifiers(self.mask)
    }

    /// Перевести событие окна; события egui — в `out`.
    pub fn translate(&mut self, ev: &WinEvent, out: &mut Vec<egui::Event>) -> Translated {
        let mut said = Translated::default();
        match ev.kind {
            WIN_CLOSE => said.close = true,
            WIN_RESIZE => said.resize = Some((ev.x.max(1) as u32, ev.y.max(1) as u32)),
            WIN_MOVE => {
                self.pointer = egui::pos2(ev.x as f32, ev.y as f32);
                out.push(egui::Event::PointerMoved(self.pointer));
            }
            WIN_LEAVE => out.push(egui::Event::PointerGone),
            WIN_POINTER | WIN_RELEASE => {
                let button = match ev.code {
                    2 => egui::PointerButton::Secondary,
                    3 => egui::PointerButton::Middle,
                    _ => egui::PointerButton::Primary,
                };
                self.pointer = egui::pos2(ev.x as f32, ev.y as f32);
                out.push(egui::Event::PointerMoved(self.pointer));
                out.push(egui::Event::PointerButton {
                    pos: self.pointer,
                    button,
                    pressed: ev.kind == WIN_POINTER,
                    modifiers: self.modifiers(),
                });
            }
            WIN_WHEEL => out.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta: egui::vec2(0.0, ev.x as f32),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: self.modifiers(),
            }),
            WIN_KEY | WIN_KEYUP => self.key(ev, out),
            _ => {}
        }
        said
    }

    fn key(&mut self, ev: &WinEvent, out: &mut Vec<egui::Event>) {
        let pressed = ev.kind == WIN_KEY;
        let old = self.mask;
        self.mask = ev.x;
        if old != self.mask {
            out.push(egui::Event::ModifiersChanged(self.modifiers()));
        }
        let (ctrl, alt) = (self.mask & 2 != 0, self.mask & 4 != 0);
        // `y` — латинский знак физической клавиши (ядро, user-abi): по нему
        // узнаются сочетания — Ctrl+H и Backspace приходят одним кодом 8, а
        // Ctrl+буква в русской раскладке — латинской клавишей.
        let latin = char::from_u32(ev.y as u32).filter(|c| c.is_ascii_graphic()).map(|c| c.to_ascii_lowercase());
        let letter = if ctrl || alt { latin.filter(|c| c.is_ascii_alphanumeric()) } else { None };
        let modifiers = self.modifiers();
        if pressed && ctrl && !alt {
            // Буфер обмена — событиями egui, как у eframe (egui-winit).
            match letter {
                Some('c') => return out.push(egui::Event::Copy),
                Some('x') => return out.push(egui::Event::Cut),
                Some('v') => {
                    if let Some(text) = clipboard_get() {
                        out.push(egui::Event::Paste(text));
                    }
                    return;
                }
                _ => {}
            }
        }
        let key = letter.and_then(key_of_char).or_else(|| key_of_code(ev.code)).or_else(|| latin.and_then(key_of_char));
        if let Some(key) = key {
            let physical_key = latin.and_then(key_of_char);
            out.push(egui::Event::Key { key, physical_key, pressed, repeat: false, modifiers });
        }
        let printable = ev.code < WIN_KEY_NAMED && ev.code >= 32 && ev.code != 127;
        // AltGr на европейских раскладках приходит как Ctrl+Alt и печатает знак.
        if pressed && printable && (!ctrl || alt) && (!alt || ctrl) {
            if let Some(c) = char::from_u32(ev.code) {
                out.push(egui::Event::Text(c.to_string()));
            }
        }
    }
}

/// Отдельное окно со своим egui до ответа: диалоги `rfd` (выбор файла,
/// сообщение). `ui` зовётся каждый кадр, `Some` — ответ и закрытие; крестик
/// окна — `None`. Можно звать из любого потока: у окна свой контекст egui.
pub fn run_modal<R>(title: &str, width: u32, height: u32, mut ui: impl FnMut(&mut egui::Ui) -> Option<R>) -> Option<R> {
    use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
    use std::time::{Duration, Instant};

    let window = Window::open(title, width, height, true)?;
    let ctx = egui::Context::default();
    let order = if window.swap_rb { ColorFieldOrder::Rgba } else { ColorFieldOrder::Bgra };
    let mut renderer = EguiSoftwareRender::new(order);
    let mut input = InputState::default();
    let mut events = Vec::new();
    let started = Instant::now();
    let mut due = Some(Instant::now());
    let answer = 'run: loop {
        while let Some(ev) = window.next_event() {
            let said = input.translate(&ev, &mut events);
            if said.close {
                break 'run None;
            }
            if let Some((w, h)) = said.resize {
                let _ = window.resize(w, h);
            }
            due = Some(Instant::now());
        }
        if !due.is_some_and(|d| Instant::now() >= d) {
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }
        let (ww, wh) = (window.width.get(), window.height.get());
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(ww as f32, wh as f32))),
            time: Some(started.elapsed().as_secs_f64()),
            events: std::mem::take(&mut events),
            focused: true,
            ..Default::default()
        };
        let mut result = None;
        let output = ctx.run_ui(raw, |root| result = ui(root));
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                clipboard_set(text);
            }
        }
        window.set_cursor(cursor_of(output.platform_output.cursor_icon));
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        renderer.render_to_canvas(ww as usize, wh as usize, &primitives, &output.textures_delta, output.pixels_per_point);
        let pixels = window.pixels();
        pixels.fill([0x20, 0x20, 0x20, 0xff]);
        let mut buffer = BufferMutRef::new(pixels, ww as usize, wh as usize);
        renderer.blit_canvas_to_buffer(&mut buffer);
        window.commit();
        if result.is_some() {
            break 'run result;
        }
        let delay = output.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |v| v.repaint_delay);
        due = (delay < Duration::from_secs(3600)).then(|| Instant::now() + delay.max(Duration::from_millis(16)));
    };
    window.close();
    answer
}

pub fn modifiers(mask: i32) -> egui::Modifiers {
    let ctrl = mask & 2 != 0;
    egui::Modifiers { alt: mask & 4 != 0, ctrl, shift: mask & 1 != 0, mac_cmd: false, command: ctrl }
}

fn key_of_char(c: char) -> Option<egui::Key> {
    if c.is_ascii_alphanumeric() {
        return egui::Key::from_name(&c.to_ascii_uppercase().to_string());
    }
    use egui::Key as K;
    Some(match c {
        '-' => K::Minus,
        '=' => K::Equals,
        '+' => K::Plus,
        ',' => K::Comma,
        '.' => K::Period,
        ';' => K::Semicolon,
        '/' => K::Slash,
        '\\' => K::Backslash,
        '[' => K::OpenBracket,
        ']' => K::CloseBracket,
        '`' => K::Backtick,
        '\'' => K::Quote,
        ' ' => K::Space,
        _ => return None,
    })
}

fn key_of_code(code: u32) -> Option<egui::Key> {
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
            12 => K::F1,
            13 => K::F2,
            14 => K::F3,
            15 => K::F4,
            16 => K::F5,
            17 => K::F6,
            18 => K::F7,
            19 => K::F8,
            20 => K::F9,
            21 => K::F10,
            22 => K::F11,
            23 => K::F12,
            _ => return None,
        });
    }
    match code {
        8 => Some(K::Backspace),
        9 => Some(K::Tab),
        10 | 13 => Some(K::Enter),
        27 => Some(K::Escape),
        127 => Some(K::Delete),
        32 => Some(K::Space),
        _ => None,
    }
}
