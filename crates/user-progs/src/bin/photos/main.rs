//! «Фотографии» (24.11.2026, Роман: «стандартный просмотрщик фото»).
//!
//! Like Windows «Фотографии»: open a picture (a double click in «Файлы» or on
//! the desktop — the system default for pictures — or «Открыть…»), walk the
//! folder with ←/→ (Home/End), zoom with the wheel around the pointer, +/−,
//! «Вписать» and «1:1» (a double click toggles them), pan by dragging or with
//! the arrows when zoomed in, rotate (R, Shift+R; the view only — the file is
//! not rewritten), full screen (F11, Esc leaves), delete to the recycle bin
//! (Delete, with the question «Файлов»), «Сделать фоном рабочего стола»
//! (Ctrl+B, as in Windows) and «Сведения» (I): name, folder, size, format,
//! date, zoom.
//!
//! Formats — see [`decode`]; how it stays fast on 2560×1440 — [`render`].
//! The background goes to the desktop through the settings store: the key
//! `desktop/background_request` of the user scope, taken by the desktop on
//! its next tick (`ui::wallpaper`), which converts it with `/bin/wallconv`
//! like «Параметры → Персонализация → Фон». Files `wallconv` does not read
//! (GIF, interlaced PNG, a rotated view) go as a BMP copy in
//! `~/.local/share/photos`.
//!
//! The bench: `photos --samples DIR` writes a PNG with transparency, a GIF,
//! a BMP and a copy of a gallery JPEG; `photos --probe DIR` decodes every
//! picture of a folder and says how it went (scenario `photos`).

#![no_std]
#![no_main]

extern crate alloc;

mod decode;
mod render;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use mini_ui::glyphicon::Icon;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit;
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw, theme};
use user_progs::app::{self, App, SizeLimits, Spec};
use user_progs::filedlg::{Dialog, Filter, Options, Outcome};
use user_progs::{
    Args, Dirent, KIND_DIRECTORY, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN,
    WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP, close, exit, file_size, heap_size, mkdir, mmap, monotonic_ms,
    munmap, open, open_write, println, read, readdir_raw, remove, rename, time_now, write,
};

use decode::{Decoded, Packer};
use user_abi::{WIN_KEY_F1, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use render::{ONE, Pyramid, Scaled};

const SPEC: Spec = Spec { name: "photos", title: "Photos", period_ms: 5_000 };

const ESCAPE: u32 = 0x1B;
const ENTER: u32 = '\n' as u32;
const TAB: u32 = '\t' as u32;
const F11: u32 = WIN_KEY_F1 + 10;

/// Biggest file read.
const FILE_LIMIT: usize = 256 * 1024 * 1024;

/// Extensions shown when walking a folder.
const KNOWN: [&str; 8] = ["jpg", "jpeg", "jpe", "png", "bmp", "gif", "qoi", "webp"];

/// WebP samples of the bench (the test files of `crates/webp`): a lossy
/// 32×24 and an animation of two 8×8 frames (100 and 200 ms).
const SAMPLE_WEBP: &[u8] = include_bytes!("../../../../webp/tests/data/lossy.webp");
const SAMPLE_WEBP_ANIM: &[u8] = include_bytes!("../../../../webp/tests/data/anim.webp");

/// Most zoom: 3200 %.
const MAX_ZOOM: u64 = 32 * ONE;

/// Double click: time and distance.
const DOUBLE_MS: u64 = 450;

/// A note at the bottom lives this long.
const NOTE_MS: u64 = 3_500;

/// Key the desktop takes the background from (`ui::wallpaper`).
const BACKGROUND_KEY: &str = "desktop/background_request";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Open,
    Prev,
    Next,
    ZoomOut,
    ZoomIn,
    Fit,
    Actual,
    RotateLeft,
    RotateRight,
    Delete,
    Background,
    Info,
    Full,
}

impl Btn {
    fn tip(self) -> &'static str {
        match self {
            Btn::Open => "Открыть… (Ctrl+O)",
            Btn::Prev => "Предыдущее (←)",
            Btn::Next => "Следующее (→)",
            Btn::ZoomOut => "Уменьшить (−)",
            Btn::ZoomIn => "Увеличить (+)",
            Btn::Fit => "Вписать в окно (0)",
            Btn::Actual => "Реальный размер (1)",
            Btn::RotateLeft => "Повернуть влево (Shift+R)",
            Btn::RotateRight => "Повернуть вправо (R)",
            Btn::Delete => "Удалить в корзину (Delete)",
            Btn::Background => "Сделать фоном рабочего стола (Ctrl+B)",
            Btn::Info => "Сведения (I)",
            Btn::Full => "Во весь экран (F11)",
        }
    }
}

/// A file of the folder.
#[derive(Clone)]
struct Entry {
    name: String,
    size: u64,
    mtime: u32,
}

/// The picture on screen.
struct Shown {
    path: String,
    pyramid: Pyramid,
    src_w: usize,
    src_h: usize,
    format: &'static str,
    wall_ok: bool,
    frames: usize,
    /// Quarter turns clockwise of the view.
    turns: u8,
    took_ms: u64,
    /// Animated GIF: all frames, the one on screen and when the next is due.
    anim: Option<Anim>,
    /// EXIF orientation the photo was turned by (1 — none).
    orientation: u8,
}

/// Frames of an animation being played (27.11).
struct Anim {
    frames: Vec<decode::Picture>,
    delays: Vec<u32>,
    at: usize,
    due: u64,
    /// Rounds played (the bench waits for the first).
    rounds: u32,
}

enum State {
    /// Nothing opened.
    Empty,
    /// Asked; the next step paints «Загрузка…», the one after decodes.
    Loading { painted: bool },
    Ready(Shown),
    Failed(String),
}

/// What the delete question offers.
struct Question {
    forever: bool,
    focus: usize,
}

struct Photos {
    packer: Packer,
    budget: usize,
    dir: String,
    files: Vec<Entry>,
    index: usize,
    state: State,
    /// The picture before, kept for an instant way back.
    previous: Option<Shown>,
    fit: bool,
    zoom: u64,
    /// Image point (16.16, base pixels) at the centre of the view.
    centre: (i64, i64),
    scaled: RefCell<Option<Scaled>>,
    info: bool,
    full: bool,
    hover: Option<Btn>,
    pointer: (i32, i32),
    drag: Option<((i32, i32), (i64, i64))>,
    last_click: (u64, i32, i32),
    ask: Option<Question>,
    dialog: Option<Dialog>,
    note: Option<(String, u64)>,
    view: Cell<Rect>,
}

/// Where things are.
struct Plan {
    toolbar: Rect,
    status: Rect,
    view: Rect,
    info: Rect,
    buttons: Vec<(Btn, Rect)>,
    zoom_label: Rect,
}

fn plan(ctx: Ctx, area: Rect, full: bool, info: bool) -> Plan {
    let (toolbar, status, body) = if full {
        (Rect::new(area.x, area.y, area.w, 0), Rect::new(area.x, area.bottom(), area.w, 0), area)
    } else {
        let frame = kit::Frame::new(ctx, area);
        (frame.toolbar, frame.status, frame.body)
    };
    let info_w = if info { ctx.px(300).min(body.w / 2) } else { 0 };
    let info_rect = Rect::new(body.right() - info_w as i32, body.y, info_w, body.h);
    let view = Rect::new(body.x, body.y, body.w - info_w, body.h);
    let mut buttons = Vec::new();
    let mut zoom_label = Rect::new(0, 0, 0, 0);
    if !full {
        let bh = ctx.px(34);
        let by = toolbar.y + (toolbar.h as i32 - bh as i32) / 2;
        let icon_w = ctx.px(38);
        let gap = ctx.px(2) as i32;
        let sep = ctx.px(14) as i32;
        let face = ctx.face(Role::Body);
        let text_w = |label: &str| face.width(label) + ctx.px(24);
        let mut x = toolbar.x + ctx.px(10) as i32;
        let push = |btn: Btn, w: u32, x: &mut i32, buttons: &mut Vec<(Btn, Rect)>| {
            buttons.push((btn, Rect::new(*x, by, w, bh)));
            *x += w as i32 + gap;
        };
        push(Btn::Open, icon_w, &mut x, &mut buttons);
        x += sep;
        push(Btn::Prev, icon_w, &mut x, &mut buttons);
        push(Btn::Next, icon_w, &mut x, &mut buttons);
        x += sep;
        push(Btn::ZoomOut, icon_w, &mut x, &mut buttons);
        zoom_label = Rect::new(x, by, ctx.px(64), bh);
        x += zoom_label.w as i32 + gap;
        push(Btn::ZoomIn, icon_w, &mut x, &mut buttons);
        x += gap * 2;
        push(Btn::Fit, text_w("Вписать"), &mut x, &mut buttons);
        push(Btn::Actual, text_w("1:1"), &mut x, &mut buttons);
        x += sep;
        push(Btn::RotateLeft, icon_w, &mut x, &mut buttons);
        push(Btn::RotateRight, icon_w, &mut x, &mut buttons);
        // The right group from the right edge.
        let mut rx = toolbar.right() - ctx.px(10) as i32;
        for btn in [Btn::Full, Btn::Info, Btn::Background, Btn::Delete] {
            rx -= icon_w as i32;
            if rx < x + sep {
                break;
            }
            buttons.push((btn, Rect::new(rx, by, icon_w, bh)));
            rx -= gap;
        }
    }
    Plan { toolbar, status, view, info: info_rect, buttons, zoom_label }
}

impl Photos {
    fn new(packer: Packer, budget: usize) -> Self {
        Self {
            packer,
            budget,
            dir: String::new(),
            files: Vec::new(),
            index: 0,
            state: State::Empty,
            previous: None,
            fit: true,
            zoom: ONE,
            centre: (0, 0),
            scaled: RefCell::new(None),
            info: false,
            full: false,
            hover: None,
            pointer: (-1, -1),
            drag: None,
            last_click: (0, 0, 0),
            ask: None,
            dialog: None,
            note: None,
            view: Cell::new(Rect::new(0, 0, 1, 1)),
        }
    }

    fn shown(&self) -> Option<&Shown> {
        match &self.state {
            State::Ready(shown) => Some(shown),
            _ => None,
        }
    }

    fn current_path(&self) -> Option<String> {
        self.files.get(self.index).map(|entry| join(&self.dir, &entry.name))
    }

    fn say(&mut self, text: &str) {
        self.note = Some((String::from(text), monotonic_ms() + NOTE_MS));
    }

    // ── Opening ───────────────────────────────────────────────────────────

    /// Open `path`: list its folder, show it.
    fn open_path(&mut self, path: &str) {
        let (dir, name) = match path.rfind('/') {
            Some(0) => (String::from("/"), String::from(&path[1..])),
            Some(at) => (String::from(&path[..at]), String::from(&path[at + 1..])),
            None => (String::from("."), String::from(path)),
        };
        let mut files = list_pictures(&dir);
        let index = match files.iter().position(|entry| entry.name == name) {
            Some(index) => index,
            None => {
                // Not a known extension (or the folder is unreadable): show it
                // anyway, alone or first.
                let size = file_len(path);
                files.insert(0, Entry { name: name.clone(), size, mtime: 0 });
                0
            }
        };
        println(&format!("photos: folder {dir} holds {} picture(s), opening '{name}'", files.len()));
        self.dir = dir;
        self.files = files;
        self.previous = None;
        self.go(index);
    }

    /// Show file number `index` of the folder (from the slot of the previous
    /// picture if it is that one).
    fn go(&mut self, index: usize) {
        if self.files.is_empty() {
            self.state = State::Empty;
            return;
        }
        let index = index.min(self.files.len() - 1);
        let want = join(&self.dir, &self.files[index].name);
        let old = core::mem::replace(&mut self.state, State::Loading { painted: false });
        let old = match old {
            State::Ready(shown) => Some(shown),
            _ => None,
        };
        self.index = index;
        self.fit = true;
        self.scaled.replace(None);
        if let Some(prev) = self.previous.take() {
            if prev.path == want {
                self.previous = old;
                println(&format!("photos: shown {} {}x{} {} from memory", self.files[index].name, prev.pyramid.base().w, prev.pyramid.base().h, prev.format));
                self.state = State::Ready(prev);
                self.reset_view();
                return;
            }
        }
        // Keep the one we leave only if it is not huge.
        self.previous = old.filter(|shown| shown.pyramid.base().w * shown.pyramid.base().h <= self.budget / 3);
    }

    fn reset_view(&mut self) {
        self.fit = true;
        self.zoom = ONE;
        if let Some(shown) = self.shown() {
            let base = shown.pyramid.base();
            self.centre = ((base.w as i64) << 15, (base.h as i64) << 15);
        }
        self.scaled.replace(None);
    }

    /// Decode the asked file (from `step`).
    fn load(&mut self) {
        let Some(path) = self.current_path() else {
            self.state = State::Empty;
            return;
        };
        let name = self.files[self.index].name.clone();
        let started = monotonic_ms();
        // A big file: free what is kept before decoding.
        if file_len(&path) > 4 * 1024 * 1024 {
            self.previous = None;
        }
        let result = read_file(&path).and_then(|data| decode::decode(&data, self.budget, self.packer));
        match result {
            Ok(Decoded { picture, src_w, src_h, format, wall_ok, frames, animation, orientation }) => {
                let took = monotonic_ms().saturating_sub(started);
                let (w, h) = (picture.w, picture.h);
                println(&format!(
                    "photos: shown {name} {w}x{h} {format} in {took} ms{}",
                    if (w, h) != (src_w, src_h) { format!(" (shrunk from {src_w}x{src_h})") } else { String::new() }
                ));
                if orientation > 1 {
                    println(&format!("photos: {name}: EXIF orientation {orientation} applied"));
                }
                let anim = animation.map(|animation| {
                    println(&format!("photos: {name}: animation of {} frame(s) playing", animation.frames.len()));
                    let first = animation.delays.first().copied().unwrap_or(100);
                    Anim { frames: animation.frames, delays: animation.delays, at: 0, due: monotonic_ms() + u64::from(first), rounds: 0 }
                });
                if anim.is_none() && frames > 1 {
                    println(&format!("photos: {name}: {frames} frame(s), too big to play — the first shown"));
                }
                self.state = State::Ready(Shown {
                    path,
                    pyramid: Pyramid::new(picture),
                    src_w,
                    src_h,
                    format,
                    wall_ok,
                    frames,
                    turns: 0,
                    took_ms: took,
                    anim,
                    orientation,
                });
                self.reset_view();
            }
            Err(why) => {
                println(&format!("photos: cannot show {name}: {why}"));
                self.state = State::Failed(String::from(why));
            }
        }
    }

    fn step_to(&mut self, delta: i32) -> bool {
        if self.files.len() < 2 {
            return false;
        }
        let n = self.files.len() as i32;
        let next = (self.index as i32 + delta).rem_euclid(n) as usize;
        self.go(next);
        true
    }

    fn start_dialog(&mut self) {
        if self.dialog.is_some() {
            return;
        }
        let mut options = Options::open();
        options.title = Some(String::from("Открыть фото"));
        let pictures = format!("{}/Pictures", user_progs::filedlg::home_dir());
        options.dir = Some(if self.dir.is_empty() { pictures } else { self.dir.clone() });
        options.filters = alloc::vec![Filter::new("Изображения", &KNOWN), Filter::all()];
        match Dialog::start(&options, "photos") {
            Ok(dialog) => self.dialog = Some(dialog),
            Err(why) => {
                println(&format!("photos: file dialog: {why}"));
                self.say("Окно «Открыть» не запустилось");
            }
        }
    }

    // ── View ──────────────────────────────────────────────────────────────

    fn base_size(&self) -> Option<(usize, usize)> {
        self.shown().map(|shown| (shown.pyramid.base().w, shown.pyramid.base().h))
    }

    /// Fit zoom: the whole picture in the view, never above 100 %.
    fn fit_zoom(&self, view: Rect) -> u64 {
        let Some((w, h)) = self.base_size() else { return ONE };
        let zx = (u64::from(view.w) << 16) / w.max(1) as u64;
        let zy = (u64::from(view.h) << 16) / h.max(1) as u64;
        zx.min(zy).min(ONE).max(1)
    }

    fn zoom_in(&self, view: Rect) -> u64 {
        if self.fit { self.fit_zoom(view) } else { self.zoom }
    }

    /// Top-left of the picture in view coordinates, clamped so that no empty
    /// band shows while the picture is larger than the view.
    fn offsets(&self, view: Rect) -> (i64, i64) {
        let Some((w, h)) = self.base_size() else { return (0, 0) };
        let zoom = self.zoom_in(view);
        let (dw, dh) = render::scaled_size(w, h, zoom);
        let axis = |v: u32, d: usize, c: i64| -> i64 {
            let (v, d) = (i64::from(v), d as i64);
            if d <= v {
                (v - d) / 2
            } else {
                let o = v / 2 - ((c as i128 * zoom as i128) >> 32) as i64;
                o.clamp(v - d, 0)
            }
        };
        (axis(view.w, dw, self.centre.0), axis(view.h, dh, self.centre.1))
    }

    /// Put the centre back from offsets (then clamped through [`Self::offsets`]).
    fn set_offsets(&mut self, view: Rect, ox: i64, oy: i64) {
        let zoom = self.zoom_in(view).max(1);
        let back = |v: u32, o: i64| -> i64 { ((((i64::from(v) / 2 - o) as i128) << 32) / zoom as i128) as i64 };
        self.centre = (back(view.w, ox), back(view.h, oy));
        let (ox, oy) = self.offsets(view);
        self.centre = (back(view.w, ox), back(view.h, oy));
    }

    /// Zoom to `zoom` keeping the picture point under `(px, py)` (view
    /// coordinates) in place.
    fn zoom_at(&mut self, view: Rect, zoom: u64, px: i32, py: i32) {
        let Some(_) = self.base_size() else { return };
        let old = self.zoom_in(view).max(1);
        let fit = self.fit_zoom(view);
        let lowest = (fit / 2).min(ONE / 10).max(ONE / 100);
        let mut zoom = zoom.clamp(lowest, MAX_ZOOM);
        // Near the fit — snap to it.
        if zoom.abs_diff(fit) * 100 <= fit * 3 {
            zoom = fit;
        }
        let (ox, oy) = self.offsets(view);
        let (px, py) = (i64::from(px - view.x), i64::from(py - view.y));
        // Picture point under the pointer, 16.16.
        let ix = (((px - ox) as i128) << 32) / old as i128;
        let iy = (((py - oy) as i128) << 32) / old as i128;
        self.fit = zoom == fit;
        self.zoom = zoom;
        let nox = px - ((ix * zoom as i128) >> 32) as i64;
        let noy = py - ((iy * zoom as i128) >> 32) as i64;
        let back = |v: u32, o: i64| -> i64 { ((((i64::from(v) / 2 - o) as i128) << 32) / zoom as i128) as i64 };
        self.centre = (back(view.w, nox), back(view.h, noy));
        let (ox, oy) = self.offsets(view);
        self.centre = (back(view.w, ox), back(view.h, oy));
        println(&format!("photos: zoom {}%{}", (zoom * 100 + ONE / 2) >> 16, if self.fit { " (fit)" } else { "" }));
    }

    fn zoom_step(&mut self, view: Rect, steps: i32, at: Option<(i32, i32)>) {
        let mut zoom = self.zoom_in(view);
        for _ in 0..steps.unsigned_abs() {
            let next = if steps > 0 { zoom * 5 / 4 } else { zoom * 4 / 5 };
            // Crossing 100 % — stop on it.
            zoom = if (zoom < ONE && next > ONE) || (zoom > ONE && next < ONE) { ONE } else { next.max(1) };
        }
        let (px, py) = at.filter(|&(x, y)| view.contains(x, y)).unwrap_or((view.x + view.w as i32 / 2, view.y + view.h as i32 / 2));
        self.zoom_at(view, zoom, px, py);
    }

    fn set_fit(&mut self) {
        self.reset_view();
        println("photos: zoom fit");
    }

    fn set_actual(&mut self, view: Rect, at: Option<(i32, i32)>) {
        let (px, py) = at.filter(|&(x, y)| view.contains(x, y)).unwrap_or((view.x + view.w as i32 / 2, view.y + view.h as i32 / 2));
        self.zoom_at(view, ONE, px, py);
    }

    fn pan(&mut self, view: Rect, dx: i64, dy: i64) {
        let (ox, oy) = self.offsets(view);
        self.set_offsets(view, ox + dx, oy + dy);
    }

    /// Is the picture larger than the view (arrows pan instead of walking).
    fn zoomed_in(&self, view: Rect) -> bool {
        let Some((w, h)) = self.base_size() else { return false };
        let (dw, dh) = render::scaled_size(w, h, self.zoom_in(view));
        !self.fit && (dw as u32 > view.w || dh as u32 > view.h)
    }

    /// Next frame of an animated GIF when its time comes (`true` — repaint).
    /// The frame replaces the shown picture, turned like the view.
    fn animate(&mut self) -> bool {
        let State::Ready(shown) = &mut self.state else { return false };
        let Some(anim) = shown.anim.as_mut() else { return false };
        let now = monotonic_ms();
        if now < anim.due || anim.frames.len() < 2 {
            return false;
        }
        anim.at = (anim.at + 1) % anim.frames.len();
        if anim.at == 0 {
            anim.rounds += 1;
            if anim.rounds == 1 {
                println(&format!("photos: animation went round ({} frame(s))", anim.frames.len()));
            }
        }
        let delay = u64::from(anim.delays.get(anim.at).copied().unwrap_or(100));
        // Late (a slow paint): the next frame from now, not a burst to catch up.
        anim.due = if now > anim.due + delay { now + delay } else { anim.due + delay };
        let Ok(frame) = anim.frames[anim.at].try_clone() else { return false };
        let mut pyramid = Pyramid::new(frame);
        for _ in 0..shown.turns {
            if pyramid.rotate(true).is_err() {
                return false;
            }
        }
        shown.pyramid = pyramid;
        self.scaled.replace(None);
        true
    }

    fn rotate(&mut self, clockwise: bool) {
        let State::Ready(shown) = &mut self.state else { return };
        let started = monotonic_ms();
        match shown.pyramid.rotate(clockwise) {
            Ok(()) => {
                shown.turns = (shown.turns + if clockwise { 1 } else { 3 }) % 4;
                println(&format!("photos: rotated {} ({} quarter turn(s)) in {} ms", if clockwise { "right" } else { "left" }, shown.turns, monotonic_ms() - started));
                self.reset_view();
            }
            Err(why) => self.say(&format!("Не повернуть: {why}")),
        }
    }

    // ── Actions ───────────────────────────────────────────────────────────

    fn act(&mut self, btn: Btn, view: Rect) {
        match btn {
            Btn::Open => self.start_dialog(),
            Btn::Prev => {
                self.step_to(-1);
            }
            Btn::Next => {
                self.step_to(1);
            }
            Btn::ZoomOut => self.zoom_step(view, -1, None),
            Btn::ZoomIn => self.zoom_step(view, 1, None),
            Btn::Fit => self.set_fit(),
            Btn::Actual => self.set_actual(view, None),
            Btn::RotateLeft => self.rotate(false),
            Btn::RotateRight => self.rotate(true),
            Btn::Delete => self.ask_delete(),
            Btn::Background => self.set_background(),
            Btn::Info => self.toggle_info(),
            Btn::Full => self.toggle_full(),
        }
    }

    fn toggle_info(&mut self) {
        self.info = !self.info;
        println(&format!("photos: info {}", if self.info { "shown" } else { "hidden" }));
    }

    fn toggle_full(&mut self) {
        self.full = !self.full;
        self.scaled.replace(None);
        println(&format!("photos: fullscreen {}", if self.full { "on" } else { "off" }));
    }

    fn ask_delete(&mut self) {
        if self.current_path().is_none() || matches!(self.state, State::Loading { .. }) {
            return;
        }
        let forever = !trash_reachable(&self.dir);
        self.ask = Some(Question { forever, focus: 0 });
    }

    fn do_delete(&mut self) {
        let Some(path) = self.current_path() else { return };
        let forever = self.ask.as_ref().is_some_and(|ask| ask.forever);
        self.ask = None;
        let name = self.files[self.index].name.clone();
        let result = if forever {
            let code = remove(&path);
            if code < 0 { Err(code) } else { Ok(()) }
        } else {
            move_to_trash(&path).map(|_| ())
        };
        match result {
            Ok(()) => {
                println(&format!("photos: {} '{name}'", if forever { "deleted" } else { "moved to the trash" }));
                self.files.remove(self.index);
                if self.previous.as_ref().is_some_and(|prev| prev.path == path) {
                    self.previous = None;
                }
                self.state = State::Empty;
                if self.files.is_empty() {
                    self.say(&format!("«{name}» удалено — в папке больше нет фото"));
                } else {
                    let index = self.index.min(self.files.len() - 1);
                    self.go(index);
                    self.say(&format!("«{name}» {}", if forever { "удалено" } else { "перемещено в корзину" }));
                }
            }
            Err(code) => {
                println(&format!("photos: cannot delete '{name}': {code}"));
                self.say(&format!("Не удалось удалить «{name}» (ошибка {code})"));
            }
        }
    }

    /// «Сделать фоном рабочего стола».
    fn set_background(&mut self) {
        let Some(shown) = self.shown() else { return };
        let target = if shown.wall_ok && shown.turns == 0 {
            shown.path.clone()
        } else {
            // A copy the converter reads: BMP of what is on screen.
            let dir = format!("{}/.local/share/photos", user_progs::filedlg::home_dir());
            make_dirs(&dir);
            let out = format!("{dir}/wallpaper-{:016x}-{}.bmp", fnv1a64(shown.path.as_bytes()), shown.turns);
            let base = shown.pyramid.base();
            let mut rgb = Vec::new();
            if rgb.try_reserve_exact(base.w * base.h * 3).is_err() {
                self.say("Недостаточно памяти для фона");
                return;
            }
            for &p in &base.px {
                rgb.extend_from_slice(&self.packer.rgb(p));
            }
            let bytes = decode::write_bmp(base.w, base.h, &rgb);
            drop(rgb);
            if write_file(&out, &bytes).is_err() {
                self.say("Не удалось подготовить картинку для фона");
                return;
            }
            out
        };
        match user_progs::reg_set("user", BACKGROUND_KEY, &target) {
            Ok(()) => {
                println(&format!("photos: background requested {target}"));
                self.say("Фон рабочего стола меняется…");
            }
            Err(code) => {
                println(&format!("photos: cannot request the background: {code}"));
                self.say("Фон не сменить: нет доступа к настройкам");
            }
        }
    }

    // ── Drawing ───────────────────────────────────────────────────────────

    fn view_bg(&self) -> Color {
        if self.full {
            Color::rgb(0, 0, 0)
        } else if theme::is_dark() {
            Color::rgb(0x1A, 0x1B, 0x1F)
        } else {
            Color::rgb(0xEC, 0xED, 0xF0)
        }
    }

    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let bar = kit::toolbar(ctx, s, plan.toolbar);
        let p = bar.palette;
        let have = self.shown().is_some();
        let many = self.files.len() > 1;
        for &(btn, rect) in &plan.buttons {
            let enabled = match btn {
                Btn::Open | Btn::Full | Btn::Info => true,
                Btn::Prev | Btn::Next => many,
                Btn::Delete => self.current_path().is_some(),
                _ => have,
            };
            let hover = enabled && self.hover == Some(btn);
            let on = (btn == Btn::Info && self.info) || (btn == Btn::Fit && self.fit && have);
            let ink = if !enabled { p.ink5 } else if on { p.acc } else { p.ink2 };
            match btn {
                Btn::Fit | Btn::Actual => {
                    let label = if btn == Btn::Fit { "Вписать" } else { "1:1" };
                    if hover || on {
                        draw::rounded(s, rect, ctx.px(theme::R_ROW), bar.flat(p.ghost), 255);
                    }
                    paint::text_centered(bar, s, Role::Body, rect, label, ink);
                }
                _ => {
                    if hover || on {
                        draw::rounded(s, rect, ctx.px(theme::R_ROW), bar.flat(p.ghost), 255);
                    }
                    let fi = match btn {
                        Btn::Open => Fi::FolderOpen,
                        Btn::Prev => Fi::ArrowLeft,
                        Btn::Next => Fi::ArrowRight,
                        Btn::ZoomIn => Fi::Plus,
                        Btn::RotateLeft => Fi::Undo,
                        Btn::RotateRight => Fi::Refresh,
                        Btn::Delete => Fi::Trash,
                        Btn::Background => Fi::Monitor,
                        Btn::Info => Fi::Info,
                        Btn::Full => Fi::Maximize,
                        _ => Fi::Minimize,
                    };
                    let g = ctx.px(18);
                    let (gx, gy) = (rect.x + (rect.w as i32 - g as i32) / 2, rect.y + (rect.h as i32 - g as i32) / 2);
                    if btn == Btn::ZoomOut {
                        // A minus: the set has none.
                        let t = ctx.px(2).max(1);
                        draw::rounded(s, Rect::new(gx + ctx.px(3) as i32, gy + (g as i32 - t as i32) / 2, g - ctx.px(6), t), t / 2, ink, 255);
                    } else {
                        iconset::draw(s, fi, gx, gy, g, ink, 255);
                    }
                }
            }
        }
        if !plan.zoom_label.is_empty() {
            let text = if have { format!("{} %", (self.zoom_in(plan.view) * 100 + ONE / 2) >> 16) } else { String::from("—") };
            paint::text_centered(bar, s, Role::Body, plan.zoom_label, &text, if have { p.ink2 } else { p.ink5 });
        }
    }

    fn status_text(&self, plan: &Plan) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(hover) = self.hover {
            return String::from(hover.tip());
        }
        if !self.files.is_empty() {
            parts.push(format!("{} из {}", self.index + 1, self.files.len()));
        }
        if let Some(shown) = self.shown() {
            let base = shown.pyramid.base();
            parts.push(format!("{} × {}", base.w, base.h));
            parts.push(String::from(shown.format));
            parts.push(format!("{} %", (self.zoom_in(plan.view) * 100 + ONE / 2) >> 16));
        }
        if let Some(entry) = self.files.get(self.index) {
            parts.push(kit::size_text(entry.size));
        }
        if parts.is_empty() {
            return String::from("Откройте фото: Ctrl+O");
        }
        parts.join("  ·  ")
    }

    fn draw_view(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let view = plan.view;
        self.view.set(view);
        s.fill(view, self.view_bg());
        let p = ctx.palette;
        let on_view = ctx.on(self.view_bg());
        let ink = if self.full || theme::is_dark() { Color::rgb(0xD8, 0xD8, 0xDC) } else { p.ink3 };
        match &self.state {
            State::Empty => {
                let g = ctx.px(64);
                iconset::draw(s, Fi::Image, view.x + (view.w as i32 - g as i32) / 2, view.y + view.h as i32 / 2 - g as i32 - ctx.px(16) as i32, g, p.ink4, 255);
                let line = Rect::new(view.x, view.y + view.h as i32 / 2, view.w, ctx.px(30));
                paint::text_centered(on_view, s, Role::Title, line, "Нет открытого фото", ink);
                let hint = Rect::new(view.x, line.bottom(), view.w, ctx.px(26));
                paint::text_centered(on_view, s, Role::Body, hint, "«Открыть» (Ctrl+O) или двойной щелчок по картинке в «Файлах»", p.ink4);
            }
            State::Loading { .. } => {
                let line = Rect::new(view.x, view.y + view.h as i32 / 2 - ctx.px(15) as i32, view.w, ctx.px(30));
                let name = self.files.get(self.index).map_or("", |entry| entry.name.as_str());
                paint::text_centered(on_view, s, Role::Title, line, &format!("Загрузка «{name}»…"), ink);
            }
            State::Failed(why) => {
                let g = ctx.px(56);
                iconset::draw(s, Fi::Warning, view.x + (view.w as i32 - g as i32) / 2, view.y + view.h as i32 / 2 - g as i32 - ctx.px(16) as i32, g, p.bad_ink, 255);
                let name = self.files.get(self.index).map_or("", |entry| entry.name.as_str());
                let line = Rect::new(view.x, view.y + view.h as i32 / 2, view.w, ctx.px(30));
                paint::text_centered(on_view, s, Role::Title, line, &format!("Не удалось открыть «{name}»"), ink);
                let hint = Rect::new(view.x, line.bottom(), view.w, ctx.px(26));
                paint::text_centered(on_view, s, Role::Body, hint, why, p.ink4);
            }
            State::Ready(shown) => {
                let zoom = self.zoom_in(view);
                let (ox, oy) = self.offsets(view);
                if zoom < ONE {
                    let mut cache = self.scaled.borrow_mut();
                    if cache.as_ref().is_none_or(|scaled| scaled.zoom != zoom) {
                        cache.take();
                        *cache = render::make_scaled(&shown.pyramid, zoom);
                    }
                    render::paint(s, view, shown.pyramid.base(), cache.as_ref(), zoom, ox, oy);
                } else {
                    render::paint(s, view, shown.pyramid.base(), None, zoom, ox, oy);
                }
            }
        }
        // Arrows over the picture edges, as in Windows «Фотографии».
        if self.files.len() > 1 && self.pointer.0 >= view.x && view.contains(self.pointer.0, self.pointer.1) {
            for (left, fi) in [(true, Fi::ChevronLeft), (false, Fi::ChevronRight)] {
                let r = arrow_rect(ctx, view, left);
                draw::blend_rect(s, r, Color::rgb(0, 0, 0), if r.contains(self.pointer.0, self.pointer.1) { 150 } else { 90 });
                let g = ctx.px(22);
                iconset::draw(s, fi, r.x + (r.w as i32 - g as i32) / 2, r.y + (r.h as i32 - g as i32) / 2, g, Color::rgb(0xFF, 0xFF, 0xFF), 255);
            }
        }
    }

    fn draw_info(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let rect = plan.info;
        if rect.is_empty() {
            return;
        }
        let p = ctx.palette;
        s.fill(rect, ctx.flat(p.panel));
        draw::vline(s, rect.x, rect.y, rect.h, p.line.color, p.line.alpha);
        let panel = ctx.on(theme::panel_bg());
        let pad = ctx.px(18) as i32;
        let x = rect.x + pad;
        let w = rect.w.saturating_sub(pad as u32 * 2);
        let mut y = rect.y + pad;
        paint::text_clipped(panel, s, Role::Title, x, y, w, "Сведения", p.ink);
        y += i32::from(ctx.face(Role::Title).line) + ctx.px(14) as i32;
        let mut rows: Vec<(&str, String)> = Vec::new();
        if let Some(entry) = self.files.get(self.index) {
            rows.push(("Имя", entry.name.clone()));
            rows.push(("Папка", self.dir.clone()));
            rows.push(("Размер файла", format!("{} ({} байт)", kit::size_text(entry.size), group_digits(entry.size))));
            if entry.mtime != 0 {
                rows.push(("Изменён", long_date(entry.mtime)));
            }
        }
        if let Some(shown) = self.shown() {
            rows.push(("Размеры", format!("{} × {} точек", shown.src_w, shown.src_h)));
            let mp = (shown.src_w * shown.src_h + 50_000) / 100_000;
            rows.push(("Мегапиксели", format!("{},{} Мп", mp / 10, mp % 10)));
            rows.push((
                "Формат",
                match (&shown.anim, shown.frames) {
                    (Some(anim), _) => format!("{} (анимация, кадров: {})", shown.format, anim.frames.len()),
                    (None, frames) if frames > 1 => format!("{} (анимация, кадров: {}; велика для памяти — показан первый)", shown.format, frames),
                    _ => String::from(shown.format),
                },
            ));
            if shown.orientation > 1 {
                let how = match shown.orientation {
                    2 => "отражено по горизонтали",
                    3 => "повёрнуто на 180°",
                    4 => "отражено по вертикали",
                    5 => "отражено и повёрнуто на 90° влево",
                    6 => "повёрнуто на 90° вправо",
                    7 => "отражено и повёрнуто на 90° вправо",
                    _ => "повёрнуто на 90° влево",
                };
                rows.push(("Ориентация (EXIF)", String::from(how)));
            }
            let base = shown.pyramid.base();
            if (base.w, base.h) != (shown.src_w, shown.src_h) && shown.turns % 2 == 0 {
                rows.push(("Показано", format!("{} × {} (уменьшено для памяти)", base.w, base.h)));
            }
            if shown.turns != 0 {
                rows.push(("Поворот", format!("{}° (только просмотр)", u32::from(shown.turns) * 90)));
            }
            rows.push(("Масштаб", format!("{} %", (self.zoom_in(plan.view) * 100 + ONE / 2) >> 16)));
            rows.push(("Открыто за", format!("{} мс", shown.took_ms)));
        }
        let label_h = i32::from(ctx.face(Role::Caption).line);
        let body_h = i32::from(ctx.face(Role::Body).line);
        for (label, value) in rows {
            if y + label_h + body_h > rect.bottom() {
                break;
            }
            paint::text_clipped(panel, s, Role::Caption, x, y, w, label, p.ink4);
            y += label_h + ctx.px(2) as i32;
            // Long values (a path) wrap by characters into up to three lines.
            let face = ctx.face(Role::Body);
            let mut line = String::new();
            let mut lines = 0;
            for ch in value.chars() {
                line.push(ch);
                if face.width(&line) > w {
                    line.pop();
                    paint::text_clipped(panel, s, Role::Body, x, y, w, &line, p.ink);
                    y += body_h;
                    lines += 1;
                    line.clear();
                    line.push(ch);
                    if lines == 2 {
                        break;
                    }
                }
            }
            if !line.is_empty() {
                paint::text_clipped(panel, s, Role::Body, x, y, w, &line, p.ink);
                y += body_h;
            }
            y += ctx.px(10) as i32;
        }
    }

    fn draw_note(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let Some((text, _)) = &self.note else { return };
        let face = ctx.face(Role::Body);
        let w = (face.width(text) + ctx.px(36)).min(plan.view.w.saturating_sub(ctx.px(24)));
        let h = ctx.px(38);
        let r = Rect::new(plan.view.x + (plan.view.w as i32 - w as i32) / 2, plan.view.bottom() - h as i32 - ctx.px(18) as i32, w, h);
        draw::rounded(s, r, ctx.px(theme::R_ROW), Color::rgb(0x20, 0x22, 0x28), 235);
        paint::text_centered(ctx.on(Color::rgb(0x20, 0x22, 0x28)), s, Role::Body, r, text, Color::rgb(0xF2, 0xF2, 0xF4));
    }

    fn question(&self) -> Option<(kit::Ask<'_>, usize)> {
        let ask = self.ask.as_ref()?;
        let name = self.files.get(self.index).map_or("", |entry| entry.name.as_str());
        Some((
            kit::Ask {
                icon: Icon::Close,
                tone: if ask.forever { Tone::Bad } else { Tone::Accent },
                title: if ask.forever { "Удалить без возможности восстановления?" } else { "Удалить это фото?" },
                detail: name,
                text: if ask.forever {
                    "На этом томе нет корзины: файл будет стёрт безвозвратно."
                } else {
                    "Файл будет перемещён в корзину. Его можно будет восстановить, пока корзина не очищена."
                },
                action: if ask.forever { "Удалить навсегда" } else { "Удалить" },
                weight: if ask.forever { Weight::Danger } else { Weight::Primary },
            },
            ask.focus,
        ))
    }

    fn hit_button(&self, ctx: Ctx, area: Rect, x: i32, y: i32) -> Option<Btn> {
        plan(ctx, area, self.full, self.info).buttons.iter().find(|(_, rect)| rect.contains(x, y)).map(|(btn, _)| *btn)
    }
}

fn arrow_rect(ctx: Ctx, view: Rect, left: bool) -> Rect {
    let (w, h) = (ctx.px(44), ctx.px(64));
    let y = view.y + (view.h as i32 - h as i32) / 2;
    let x = if left { view.x + ctx.px(12) as i32 } else { view.right() - ctx.px(12) as i32 - w as i32 };
    Rect::new(x, y, w, h)
}

impl App for Photos {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let plan = plan(ctx, area, self.full, self.info);
        if !self.full {
            self.draw_toolbar(s, ctx, &plan);
        }
        self.draw_view(s, ctx, &plan);
        self.draw_info(s, ctx, &plan);
        if !self.full {
            kit::status_bar(ctx, s, plan.status, &self.status_text(&plan));
        }
        self.draw_note(s, ctx, &plan);
        if let Some((ask, focus)) = self.question() {
            kit::draw_ask(ctx, s, area, &ask, focus);
        }
    }

    fn title(&self) -> Option<String> {
        match self.files.get(self.index) {
            Some(entry) if !matches!(self.state, State::Empty) => Some(format!("{}\x1f{} — Фотографии", SPEC.title, entry.name)),
            _ => Some(format!("{}\x1fФотографии", SPEC.title)),
        }
    }

    fn size_limits(&self) -> SizeLimits {
        SizeLimits::min(640, 420)
    }

    fn fullscreen(&self) -> bool {
        self.full
    }

    fn quits_on_q(&self) -> bool {
        self.ask.is_none()
    }

    fn step(&mut self) -> bool {
        let mut dirty = false;
        if let Some(dialog) = self.dialog.as_mut() {
            if let Some(outcome) = dialog.poll() {
                self.dialog = None;
                if let Outcome::Chosen(path) = outcome {
                    self.open_path(&path);
                }
                dirty = true;
            }
        }
        if let Some((_, until)) = &self.note {
            if monotonic_ms() >= *until {
                self.note = None;
                dirty = true;
            }
        }
        match self.state {
            State::Loading { painted: false } => {
                self.state = State::Loading { painted: true };
                dirty = true;
            }
            State::Loading { painted: true } => {
                self.load();
                dirty = true;
            }
            State::Ready(_) => dirty |= self.animate(),
            _ => {}
        }
        dirty
    }

    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let view = self.view.get();
        let ctrl = mods & WIN_MOD_CTRL != 0;
        let shift = mods & WIN_MOD_SHIFT != 0;
        let letter = char::from_u32(latin).map(|c| c.to_ascii_lowercase());
        if let Some(focus) = self.ask.as_ref().map(|ask| ask.focus) {
            match code {
                ESCAPE => self.ask = None,
                ENTER if focus == 0 => self.do_delete(),
                ENTER => self.ask = None,
                TAB | WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                    if let Some(ask) = self.ask.as_mut() {
                        ask.focus = 1 - focus;
                    }
                }
                _ => return false,
            }
            return true;
        }
        if ctrl {
            match letter {
                Some('o') => self.start_dialog(),
                Some('b') => self.set_background(),
                // Ctrl+R as in Windows «Фотографии».
                Some('r') => self.rotate(!shift),
                Some('0') => self.set_fit(),
                Some('1') => self.set_actual(view, None),
                Some('=') | Some('+') => self.zoom_step(view, 1, None),
                Some('-') => self.zoom_step(view, -1, None),
                _ => return false,
            }
            return true;
        }
        match code {
            ESCAPE if self.full => self.toggle_full(),
            ESCAPE if self.info => self.toggle_info(),
            F11 => self.toggle_full(),
            WIN_KEY_DELETE => self.ask_delete(),
            WIN_KEY_HOME => self.go(0),
            WIN_KEY_END => self.go(self.files.len().saturating_sub(1)),
            WIN_KEY_PAGE_UP => return self.step_to(-1),
            WIN_KEY_PAGE_DOWN | 0x20 => return self.step_to(1),
            WIN_KEY_LEFT | WIN_KEY_RIGHT | WIN_KEY_UP | WIN_KEY_DOWN if self.zoomed_in(view) => {
                let step = i64::from(view.w.min(view.h) / 8).max(16);
                let (dx, dy) = match code {
                    WIN_KEY_LEFT => (step, 0),
                    WIN_KEY_RIGHT => (-step, 0),
                    WIN_KEY_UP => (0, step),
                    _ => (0, -step),
                };
                self.pan(view, dx, dy);
            }
            WIN_KEY_LEFT | WIN_KEY_UP => return self.step_to(-1),
            WIN_KEY_RIGHT | WIN_KEY_DOWN => return self.step_to(1),
            _ => match char::from_u32(code) {
                Some('+') | Some('=') => self.zoom_step(view, 1, None),
                Some('-') | Some('_') => self.zoom_step(view, -1, None),
                Some('0') => self.set_fit(),
                Some('1') => self.set_actual(view, None),
                Some('r') if !shift => self.rotate(true),
                Some('R') | Some('r') => self.rotate(false),
                Some('i') | Some('I') => self.toggle_info(),
                Some('f') | Some('F') => self.toggle_full(),
                _ => return false,
            },
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if self.ask.is_some() {
            let answer = self.question().and_then(|(ask, _)| kit::ask_hit(ctx, area, &ask, x, y));
            match answer {
                Some(true) => self.do_delete(),
                Some(false) => self.ask = None,
                None => {}
            }
            return true;
        }
        let plan = plan(ctx, area, self.full, self.info);
        if let Some(btn) = self.hit_button(ctx, area, x, y) {
            self.act(btn, plan.view);
            return true;
        }
        if !plan.view.contains(x, y) {
            return false;
        }
        if self.files.len() > 1 {
            for (left, delta) in [(true, -1), (false, 1)] {
                if arrow_rect(ctx, plan.view, left).contains(x, y) {
                    return self.step_to(delta);
                }
            }
        }
        let now = monotonic_ms();
        let (t, lx, ly) = self.last_click;
        if now.saturating_sub(t) <= DOUBLE_MS && (x - lx).abs() <= 6 && (y - ly).abs() <= 6 {
            self.last_click = (0, 0, 0);
            self.drag = None;
            if self.fit || self.zoom_in(plan.view) != ONE {
                self.set_actual(plan.view, Some((x, y)));
            } else {
                self.set_fit();
            }
            return true;
        }
        self.last_click = (now, x, y);
        self.drag = Some(((x, y), self.offsets(plan.view)));
        false
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.pointer = (x, y);
        let Some(((sx, sy), (ox, oy))) = self.drag else { return false };
        let view = plan(ctx, area, self.full, self.info).view;
        if !self.zoomed_in(view) && self.fit {
            return false;
        }
        self.set_offsets(view, ox + i64::from(x - sx), oy + i64::from(y - sy));
        true
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.drag = None;
        false
    }

    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let before = (self.hover, self.arrow_zone(ctx, area, self.pointer));
        self.pointer = (x, y);
        self.hover = self.hit_button(ctx, area, x, y);
        before != (self.hover, self.arrow_zone(ctx, area, (x, y)))
    }

    fn leave(&mut self) -> bool {
        self.pointer = (-1, -1);
        self.hover = None;
        true
    }

    fn wheel_at(&mut self, area: Rect, ctx: Ctx, clicks: i32) -> bool {
        if self.shown().is_none() || self.ask.is_some() {
            return false;
        }
        let view = plan(ctx, area, self.full, self.info).view;
        self.zoom_step(view, clicks.clamp(-5, 5), Some(self.pointer));
        true
    }
}

impl Photos {
    /// Which edge arrow (or none, or the view) the pointer is over — the
    /// arrows show only while the pointer is over the picture.
    fn arrow_zone(&self, ctx: Ctx, area: Rect, at: (i32, i32)) -> u8 {
        let view = plan(ctx, area, self.full, self.info).view;
        if !view.contains(at.0, at.1) {
            return 0;
        }
        if arrow_rect(ctx, view, true).contains(at.0, at.1) {
            2
        } else if arrow_rect(ctx, view, false).contains(at.0, at.1) {
            3
        } else {
            1
        }
    }
}

// ── Files ───────────────────────────────────────────────────────────────────

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

fn extension(name: &str) -> String {
    match name.rfind('.') {
        Some(at) if at > 0 => name[at + 1..].to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Pictures of a folder by name (case-insensitive, numbers by value).
fn list_pictures(dir: &str) -> Vec<Entry> {
    let fd = open(dir);
    if fd < 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir_raw(fd, &mut entry) > 0 {
        if entry.kind == KIND_DIRECTORY {
            continue;
        }
        let len = (entry.name_len as usize).min(entry.name.len());
        let Ok(name) = core::str::from_utf8(&entry.name[..len]) else { continue };
        if name.starts_with('.') || !KNOWN.contains(&extension(name).as_str()) || name.ends_with(".thumb.qoi") {
            continue;
        }
        out.push(Entry { name: name.to_string(), size: entry.size, mtime: entry.mtime });
    }
    close(fd);
    out.sort_by(|a, b| natural(&a.name, &b.name));
    out
}

/// «file2» before «file10».
fn natural(a: &str, b: &str) -> core::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return core::cmp::Ordering::Equal,
            (None, _) => return core::cmp::Ordering::Less,
            (_, None) => return core::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let n = |it: &mut core::iter::Peekable<core::str::Chars<'_>>| {
                    let mut v: u64 = 0;
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        v = v.saturating_mul(10).saturating_add(u64::from(c as u8 - b'0'));
                        it.next();
                    }
                    v
                };
                let (va, vb) = (n(&mut a), n(&mut b));
                if va != vb {
                    return va.cmp(&vb);
                }
            }
            (Some(x), Some(y)) => {
                let (lx, ly) = (x.to_lowercase().next().unwrap_or(x), y.to_lowercase().next().unwrap_or(y));
                if lx != ly {
                    return lx.cmp(&ly);
                }
                a.next();
                b.next();
            }
        }
    }
}

fn file_len(path: &str) -> u64 {
    let fd = open(path);
    if fd < 0 {
        return 0;
    }
    let size = file_size(fd);
    close(fd);
    size.max(0) as u64
}

fn read_file(path: &str) -> Result<Vec<u8>, &'static str> {
    let fd = open(path);
    if fd < 0 {
        return Err("файл не открывается");
    }
    let size = file_size(fd);
    if size <= 0 || size as usize > FILE_LIMIT {
        close(fd);
        return Err(if size <= 0 { "файл пуст" } else { "файл больше 256 МБ" });
    }
    let mut data = Vec::new();
    if data.try_reserve_exact(size as usize).is_err() {
        close(fd);
        return Err("недостаточно памяти");
    }
    data.resize(size as usize, 0);
    let mut got = 0;
    while got < data.len() {
        let step = read(fd, &mut data[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    data.truncate(got);
    Ok(data)
}

fn write_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let fd = open_write(path, true, true);
    if fd < 0 {
        return Err(fd);
    }
    let mut sent = 0;
    while sent < data.len() {
        let step = write(fd, &data[sent..]);
        if step <= 0 {
            close(fd);
            return Err(if step < 0 { step } else { -1 });
        }
        sent += step as usize;
    }
    close(fd);
    Ok(())
}

fn make_dirs(path: &str) {
    let mut built = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        built.push('/');
        built.push_str(part);
        let fd = open(&built);
        if fd >= 0 {
            close(fd);
            continue;
        }
        let _ = mkdir(&built, 0o755);
    }
}

fn exists(path: &str) -> bool {
    let fd = open(path);
    if fd >= 0 {
        close(fd);
    }
    fd >= 0
}

fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn trash_dir() -> String {
    format!("{}/.local/share/Trash", user_progs::filedlg::home_dir())
}

/// Mount points (first field of `SYS_MOUNTS` lines).
fn mount_points() -> Vec<String> {
    let mut buffer = alloc::vec![0u8; 16 * 1024];
    let got = user_progs::mounts(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("");
    text.lines().filter_map(|line| line.split('\t').next()).filter(|point| !point.is_empty()).map(String::from).collect()
}

/// Is the recycle bin on the volume of `dir` (moving there is a rename).
fn trash_reachable(dir: &str) -> bool {
    let points = mount_points();
    let mount = |path: &str| points.iter().filter(|point| *point == "/" || path == point.as_str() || path.starts_with(&format!("{point}/"))).max_by_key(|point| point.len()).cloned();
    mount(dir) == mount(&trash_dir())
}

/// Path in the percent form of `.trashinfo` (freedesktop, as «Файлы»).
fn percent_encode(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Move `path` to the recycle bin the way «Файлы» does.
fn move_to_trash(path: &str) -> Result<String, i64> {
    let trash = trash_dir();
    let (files, info) = (format!("{trash}/files"), format!("{trash}/info"));
    make_dirs(&files);
    make_dirs(&info);
    let base = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = match base.rfind('.') {
        Some(at) if at > 0 => (&base[..at], &base[at..]),
        _ => (base, ""),
    };
    let name = (1..=1000u32)
        .map(|attempt| if attempt == 1 { base.to_string() } else { format!("{stem} ({attempt}){ext}") })
        .find(|candidate| !exists(&join(&files, candidate)) && !exists(&format!("{info}/{candidate}.trashinfo")))
        .ok_or(user_abi::ERR_EXISTS)?;
    let info_path = format!("{info}/{name}.trashinfo");
    let mut text = format!("[Trash Info]\nPath={}\n", percent_encode(path));
    let now = time_now();
    if now > 0 {
        let t = calendar::DateTime::from_unix(now as i64 + i64::from(local_offset()) * 60);
        text.push_str(&format!("DeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}\n", t.year, t.month, t.day, t.hour, t.minute, t.second));
    }
    write_file(&info_path, text.as_bytes())?;
    let code = rename(path, &join(&files, &name));
    if code < 0 {
        remove(&info_path);
        return Err(code);
    }
    Ok(name)
}

/// Minutes east of UTC from `/etc/system.cfg`.
fn local_offset() -> i32 {
    let fd = open("/etc/system.cfg");
    if fd < 0 {
        return 0;
    }
    let mut buffer = [0u8; 2048];
    let got = read(fd, &mut buffer);
    close(fd);
    if got <= 0 {
        return 0;
    }
    let text = core::str::from_utf8(&buffer[..got as usize]).unwrap_or("");
    sysconf::value(text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

fn long_date(mtime: u32) -> String {
    const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    let local = calendar::DateTime::from_unix(i64::from(mtime) + i64::from(local_offset()) * 60);
    let month = MONTHS.get(usize::from(local.month).saturating_sub(1)).copied().unwrap_or("");
    format!("{} {month} {} г., {:02}:{:02}", local.day, local.year, local.hour, local.minute)
}

fn group_digits(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i > 0 && (text.len() - i) % 3 == 0 {
            out.push('\u{A0}');
        }
        out.push(ch);
    }
    out
}

/// `%XX` — byte XX (paths passed with `%20` for spaces).
fn unpct(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            if let Some(byte) = text.get(at + 1..at + 3).and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| String::from(text))
}

// ── Bench helpers ───────────────────────────────────────────────────────────

/// `--samples DIR`: a PNG with transparency, a GIF, a BMP and a gallery JPEG.
fn samples(dir: &str) -> i64 {
    make_dirs(dir);
    let mut written = 0;
    // A gallery photo if there is one (a big JPEG).
    if let Some(entry) = list_pictures("/usr/share/wallpapers").into_iter().find(|entry| extension(&entry.name) == "jpg") {
        if let Ok(data) = read_file(&join("/usr/share/wallpapers", &entry.name)) {
            if write_file(&join(dir, "01-photo.jpg"), &data).is_ok() {
                written += 1;
            }
            // The same photo «from a phone held upright»: EXIF orientation 6
            // (turn 90° right to view).
            if data.starts_with(&[0xFF, 0xD8]) && write_file(&join(dir, "05-exif-6.jpg"), &decode::with_exif_orientation(&data, 6)).is_ok() {
                written += 1;
            }
        }
    }
    let (w, h) = (800usize, 600usize);
    let mut rgba = alloc::vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as i64 - 400, y as i64 - 300);
            let inside = dx * dx + dy * dy < 250 * 250;
            let px = &mut rgba[(y * w + x) * 4..][..4];
            px.copy_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 200, if inside { 255 } else { (x * 255 / w) as u8 / 2 }]);
        }
    }
    if write_file(&join(dir, "02-alpha.png"), &decode::write_png(w, h, &rgba)).is_ok() {
        written += 1;
    }
    let (gw, gh) = (480usize, 320usize);
    let mut palette = [[0u8; 3]; 256];
    for (i, c) in palette.iter_mut().enumerate() {
        *c = [(i & 0xE0) as u8, ((i & 0x1C) << 3) as u8, ((i & 3) << 6) as u8];
    }
    let mut indices = alloc::vec![0u8; gw * gh];
    for y in 0..gh {
        for x in 0..gw {
            let stripe = ((x / 40) + (y / 40)) % 2 == 0;
            indices[y * gw + x] = if stripe { ((x * 7 / gw) << 5 | (y * 7 / gh) << 2) as u8 } else { 0x03 };
        }
    }
    if write_file(&join(dir, "03-palette.gif"), &decode::write_gif(gw, gh, &palette, &indices)).is_ok() {
        written += 1;
    }
    // Animated: a red, green and blue ball crossing the frame, 0,4 s each.
    let (aw, ah) = (240usize, 160usize);
    let anim: Vec<Vec<u8>> = [(0xE0u8, 40usize), (0x1C, 120), (0x03, 200)]
        .iter()
        .map(|&(colour, cx)| {
            let mut frame = alloc::vec![0xFFu8; aw * ah];
            for y in 0..ah {
                for x in 0..aw {
                    let (dx, dy) = (x as i64 - cx as i64, y as i64 - 80);
                    if dx * dx + dy * dy < 30 * 30 {
                        frame[y * aw + x] = colour;
                    }
                }
            }
            frame
        })
        .collect();
    if write_file(&join(dir, "06-anim.gif"), &decode::write_gif_anim(aw, ah, &palette, &anim, 40)).is_ok() {
        written += 1;
    }
    for (name, data) in [("07-lossy.webp", SAMPLE_WEBP), ("08-anim.webp", SAMPLE_WEBP_ANIM)] {
        if write_file(&join(dir, name), data).is_ok() {
            written += 1;
        }
    }
    let (bw, bh) = (1024usize, 768usize);
    let mut rgb = alloc::vec![0u8; bw * bh * 3];
    for y in 0..bh {
        for x in 0..bw {
            let px = &mut rgb[(y * bw + x) * 3..][..3];
            let ring = ((((x as i64 - 512).pow(2) + (y as i64 - 384).pow(2)) as u64).isqrt() / 24) % 2 == 0;
            px.copy_from_slice(&if ring { [230, 120, 40] } else { [30, 60, (y * 200 / bh) as u8 + 40] });
        }
    }
    if write_file(&join(dir, "04-rings.bmp"), &decode::write_bmp(bw, bh, &rgb)).is_ok() {
        written += 1;
    }
    println(&format!("photos: samples written {written} in {dir}"));
    if written >= 3 { 0 } else { 1 }
}

/// `--probe DIR`: decode every picture of a folder.
fn probe(dir: &str, budget: usize) -> i64 {
    let packer = Packer::new();
    let (mut ok, mut failed) = (0, 0);
    for entry in list_pictures(dir) {
        let started = monotonic_ms();
        match read_file(&join(dir, &entry.name)).and_then(|data| decode::decode(&data, budget, packer)) {
            Ok(decoded) => {
                ok += 1;
                // A pixel in the middle, as R G B — the decoders agree with the samples.
                let p = &decoded.picture;
                let mid = packer.rgb(p.px[(p.h / 2) * p.w + p.w / 2]);
                println(&format!(
                    "photos: probe {} {} {}x{} frames {} centre {:02x}{:02x}{:02x} in {} ms ok",
                    entry.name,
                    decoded.format,
                    decoded.src_w,
                    decoded.src_h,
                    decoded.frames,
                    mid[0],
                    mid[1],
                    mid[2],
                    monotonic_ms() - started
                ));
                if decoded.orientation > 1 {
                    println(&format!("photos: probe {} exif orientation {} shown {}x{}", entry.name, decoded.orientation, p.w, p.h));
                }
                if let Some(animation) = &decoded.animation {
                    let total: u32 = animation.delays.iter().sum();
                    println(&format!("photos: probe {} animation {} frame(s) {} ms", entry.name, animation.frames.len(), total));
                }
            }
            Err(why) => {
                failed += 1;
                println(&format!("photos: probe {} FAILED: {why}", entry.name));
            }
        }
    }
    println(&format!("photos: probe done, {ok} ok, {failed} failed"));
    if failed == 0 && ok > 0 { 0 } else { 1 }
}

/// Take as much heap as the machine gives (one piece, before any
/// allocation): photos are large.
fn take_heap() -> usize {
    for mib in [384usize, 256, 192, 128, 96, 64] {
        let bytes = mib * 1024 * 1024;
        let base = mmap(bytes, 0);
        if base >= 0 {
            munmap(base as usize, bytes);
            if heap_size(bytes) {
                return bytes;
            }
        }
    }
    32 * 1024 * 1024
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: values from the kernel per the `_start` contract.
    let args = unsafe { Args::new(argc, argv) };
    let heap = take_heap();
    // Pixels of one picture: base + pyramid (4/3) + decoding buffers.
    let budget = heap / 16;
    match args.get(1) {
        Some("--samples") => exit(samples(&args.get(2).map(unpct).unwrap_or_else(|| String::from("/root/Pictures/photos-test")))),
        Some("--probe") => {
            mini_ui::use_raw_format(mini_ui::PIXEL_RGB);
            exit(probe(&args.get(2).map(unpct).unwrap_or_else(|| String::from(".")), budget))
        }
        _ => {}
    }
    // A path with spaces may come in pieces (the kernel splits by spaces)
    // or with `%20`.
    let path = (args.len() > 1).then(|| {
        let joined: Vec<&str> = (1..args.len()).filter_map(|i| args.get(i)).collect();
        let joined = joined.join(" ");
        let joined = joined.trim_matches('"');
        if exists(joined) { String::from(joined) } else { unpct(joined) }
    });
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (1100, 760));
    app::run(&SPEC, &info, size, move |_, _| {
        let mut photos = Photos::new(Packer::new(), budget);
        println(&format!("photos: heap {} MiB, picture budget {} Mpx", heap >> 20, budget / 1_000_000));
        match path {
            Some(path) => photos.open_path(&path),
            None => println("photos: ready, nothing opened"),
        }
        photos
    })
}
