//! Screen capture for the recorder (22.11, `/bin/screenrec`, `SYS_SCREENCAP`).
//!
//! # How a frame gets here
//!
//! The compositor already copies every composed band from its back buffer
//! into the outputs ([`super::compositor`]: `compose`, the direct copy of a
//! fullscreen window and the page-flip path). While a capture runs, the same
//! pixels are also copied into a **mirror** of the captured rectangle in
//! ordinary memory ([`put`]) — only the damaged parts, at present time. A
//! grab ([`grab`]) is then one copy of the mirror into the recorder's buffer;
//! nothing reads the scanout memory (on NVIDIA that is VRAM behind a BAR, and
//! reading it costs tens of milliseconds per frame).
//!
//! The pointer: when the compositor draws it into the frame (software
//! pointer, no directly shown window) it is already in the mirror. A pointer
//! drawn by the display (hardware cursor, page flip, direct copy) is patched
//! into the grabbed frame from a cached picture ([`note_pointer`]).
//!
//! # Who stops it
//!
//! The recording program ([`user_abi::CAP_STOP`]). Win+Alt+R, the tray
//! indicator and the recorder window only *ask* ([`request_stop`]); the
//! program sees it in [`user_abi::CAP_STATUS`], finishes the file and stops.
//! A program that died without stopping is noticed by [`tick`]: no grab for
//! [`ORPHAN_MS`] ends the capture.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use mini_ui::{Rect, Surface};
use user_abi::{CapInfo, CapSpec, CapStatus, ERR_AGAIN, ERR_BAD_ADDRESS, ERR_EXISTS, ERR_LIMIT, ERR_NOT_FOUND, ERR_UNSUPPORTED};

use crate::mm::PageFlags;
use crate::sync::SpinLock;
use crate::user::space;

use super::compositor::Compositor;

/// A capture without a grab for this long belongs to a program that is gone.
const ORPHAN_MS: u64 = 4_000;

/// Rows copied per hold of the lock in [`grab`]: the compositor waits for it
/// at most this many rows (spin lock, interrupts off).
const GRAB_ROWS: u32 = 32;

/// Largest side of a capture.
const MAX_SIDE: u32 = 16_384;

/// Fast check for the compositor hooks — no lock when nothing is captured.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Start time (ms of uptime) of the running capture, for the tray; 0 — none.
static STARTED_MS: AtomicU64 = AtomicU64::new(0);

/// Somebody asked the recorder to stop (hotkey, tray, window).
static STOP_ASKED: AtomicBool = AtomicBool::new(false);

/// The compositor should repaint the captured rectangle once (fills the
/// mirror at start).
static REPAINT: AtomicBool = AtomicBool::new(false);

/// A saved file to announce («Запись сохранена»), shown by [`tick`].
static SAVED: SpinLock<Option<String>> = SpinLock::new(None);

static STATE: SpinLock<Option<Capture>> = SpinLock::new(None);

struct Capture {
    /// Captured rectangle in desktop coordinates.
    rect: Rect,
    mirror: Vec<u32>,
    /// Bumped by every change of the mirror or of a patched pointer.
    seq: u64,
    pointer_wanted: bool,
    pointer: PointerShot,
    last_grab_ms: u64,
    grabs: u64,
}

/// The pointer as the display shows it, for patching into grabbed frames.
struct PointerShot {
    /// Already drawn into the mirror by the compositor.
    drawn: bool,
    visible: bool,
    x: i32,
    y: i32,
    key: usize,
    hot: (u32, u32),
    side: u32,
    /// Points in screen format and their alpha.
    image: Vec<(u32, u8)>,
}

/// Is a capture running (cheap).
#[inline]
#[must_use]
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Seconds since the start of the running capture (tray indicator).
#[must_use]
pub fn elapsed_s() -> Option<u64> {
    let started = STARTED_MS.load(Ordering::Relaxed);
    (started != 0).then(|| crate::time::uptime_ms().saturating_sub(started) / 1000)
}

/// Ask the recording program to stop (Win+Alt+R, tray, recorder window).
pub fn request_stop() {
    if active() {
        STOP_ASKED.store(true, Ordering::Relaxed);
        crate::kprintln!("  capture     : stop requested");
    }
}

/// Compositor hook: the rectangle to repaint once so the mirror fills.
pub fn take_repaint() -> Option<Rect> {
    if !REPAINT.swap(false, Ordering::Relaxed) {
        return None;
    }
    STATE.lock().as_ref().map(|capture| capture.rect)
}

/// Compositor hook: `part` (desktop coordinates) now shows the pixels of
/// `src` at `(sx, sy)`.
pub fn put(part: Rect, src: &Surface, sx: i32, sy: i32) {
    let mut state = STATE.lock();
    let Some(capture) = state.as_mut() else {
        return;
    };
    let clip = part.intersect(&capture.rect);
    if clip.is_empty() {
        return;
    }
    let (dx, dy) = (clip.x - part.x, clip.y - part.y);
    let (sx, sy) = (sx + dx, sy + dy);
    if sx < 0 || sy < 0 || (sx as u32).saturating_add(clip.w) > src.width() || (sy as u32).saturating_add(clip.h) > src.height() {
        return;
    }
    let width = capture.rect.w as usize;
    let col = (clip.x - capture.rect.x) as usize;
    for row in 0..clip.h {
        let from = &src.row(sy as u32 + row)[sx as usize..sx as usize + clip.w as usize];
        let at = (clip.y - capture.rect.y) as usize + row as usize;
        capture.mirror[at * width + col..at * width + col + clip.w as usize].copy_from_slice(from);
    }
    capture.seq += 1;
}

/// Compositor hook, once per presented frame: where the pointer is and
/// whether the compositor drew it into the frame itself.
pub fn note_pointer(pointer: &super::pointer::Pointer, drawn: bool) {
    let mut state = STATE.lock();
    let Some(capture) = state.as_mut() else {
        return;
    };
    let (x, y) = pointer.position();
    let visible = pointer.is_visible();
    let shot = &mut capture.pointer;
    let key = pointer.image_key();
    let moved = shot.x != x || shot.y != y || shot.visible != visible || shot.key != key || shot.drawn != drawn;
    if !drawn && capture.pointer_wanted && (shot.image.is_empty() || shot.key != key) {
        let side = 48;
        let (argb, hot) = pointer.image(side);
        shot.image = argb
            .iter()
            .map(|p| {
                let color = mini_ui::Color::rgb((p >> 16) as u8, (p >> 8) as u8, *p as u8);
                (color.pixel(), (p >> 24) as u8)
            })
            .collect();
        shot.side = side;
        shot.hot = hot;
    }
    shot.key = key;
    shot.x = x;
    shot.y = y;
    shot.visible = visible;
    shot.drawn = drawn;
    if moved && !drawn && capture.pointer_wanted {
        capture.seq += 1;
    }
}

/// Desktop tick (every half second): forget a capture whose program is gone,
/// show «Запись сохранена».
pub fn tick(desktop: &mut Compositor) {
    if active() {
        let stale = STATE
            .lock()
            .as_ref()
            .is_some_and(|capture| crate::time::uptime_ms().saturating_sub(capture.last_grab_ms) > ORPHAN_MS);
        if stale {
            crate::kprintln!("  capture     : the recorder stopped grabbing, capture ended");
            end();
        }
    }
    let saved = SAVED.lock().take();
    if let Some(path) = saved {
        show_saved(desktop, &path);
    }
}

fn show_saved(desktop: &mut Compositor, path: &str) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let art = super::appicon::tool_art("screenrec").unwrap_or(super::appicon::Art::Glyph(mini_ui::glyphicon::Icon::Camera, mini_ui::paint::Tone::Bad, true));
    let (area, panel_top) = desktop.toast_place();
    let start = alloc::format!("/bin/files {path}");
    if let Some(toast) = super::toast::Toast::notice(
        "Запись экрана",
        "Запись сохранена",
        name,
        "Открыть папку",
        &art,
        start,
        fpk::Rights::ALL,
        area,
        panel_top,
        desktop.scale(),
    ) {
        desktop.show_toast(toast);
        crate::kprintln!("  desktop     : notification 'Запись сохранена' ({path})");
    }
}

fn end() {
    *STATE.lock() = None;
    ACTIVE.store(false, Ordering::Relaxed);
    STARTED_MS.store(0, Ordering::Relaxed);
    STOP_ASKED.store(false, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------
// SYS_SCREENCAP
// ---------------------------------------------------------------------------

/// `SYS_SCREENCAP` (see the contract in `user_abi`).
pub fn syscall(action: usize, a1: usize, a2: usize) -> i64 {
    match action {
        user_abi::CAP_INFO => info(a1),
        user_abi::CAP_START => start(a1),
        user_abi::CAP_GRAB => grab(a1, a2 as u64),
        user_abi::CAP_STOP => stop(a1, a2),
        user_abi::CAP_STATUS => status(a1),
        user_abi::CAP_REQUEST_STOP => {
            if !active() {
                return ERR_NOT_FOUND;
            }
            request_stop();
            0
        }
        user_abi::CAP_WINDOWS => windows(a1, a2),
        _ => ERR_UNSUPPORTED,
    }
}

/// Write a plain `#[repr(C)]` answer into the program.
fn write_out<T: Copy>(ptr: usize, value: T) -> i64 {
    if ptr % align_of::<T>() != 0 || !space::user_can(ptr, size_of::<T>(), PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: aligned and checked against the program's page tables for write.
    unsafe { core::ptr::write(ptr as *mut T, value) };
    0
}

fn info(out: usize) -> i64 {
    let Some(rects) = super::with_desktop(|desktop| desktop.output_rects().to_vec()) else {
        return ERR_AGAIN;
    };
    let mut answer = CapInfo { count: rects.len().min(8) as u32, format: mini_ui::format_code(), ..CapInfo::default() };
    let mut union = Rect::EMPTY;
    for (index, rect) in rects.iter().take(8).enumerate() {
        answer.outputs[index] = [rect.x, rect.y, rect.w as i32, rect.h as i32];
        union = if union.is_empty() { *rect } else { union.union(rect) };
    }
    answer.desk_w = union.right().max(0) as u32;
    answer.desk_h = union.bottom().max(0) as u32;
    write_out(out, answer)
}

fn start(ptr: usize) -> i64 {
    if ptr % align_of::<CapSpec>() != 0 || !space::user_can(ptr, size_of::<CapSpec>(), PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: aligned and readable, checked above.
    let spec = unsafe { core::ptr::read(ptr as *const CapSpec) };
    if spec.w == 0 || spec.h == 0 || spec.w > MAX_SIDE || spec.h > MAX_SIDE {
        return ERR_LIMIT;
    }
    if active() {
        return ERR_EXISTS;
    }
    let points = spec.w as usize * spec.h as usize;
    let mut mirror = Vec::new();
    if mirror.try_reserve_exact(points).is_err() {
        return ERR_LIMIT;
    }
    mirror.resize(points, mini_ui::Color::rgb(0, 0, 0).pixel());
    let now = crate::time::uptime_ms();
    let capture = Capture {
        rect: Rect::new(spec.x, spec.y, spec.w, spec.h),
        mirror,
        seq: 1,
        pointer_wanted: spec.flags & user_abi::CAP_POINTER != 0,
        pointer: PointerShot { drawn: true, visible: false, x: 0, y: 0, key: usize::MAX, hot: (0, 0), side: 0, image: Vec::new() },
        last_grab_ms: now,
        grabs: 0,
    };
    {
        let mut state = STATE.lock();
        if state.is_some() {
            return ERR_EXISTS;
        }
        *state = Some(capture);
    }
    STOP_ASKED.store(false, Ordering::Relaxed);
    STARTED_MS.store(now.max(1), Ordering::Relaxed);
    REPAINT.store(true, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    crate::kprintln!(
        "  capture     : started {}x{} at {},{} ({} fps, pointer {})",
        spec.w,
        spec.h,
        spec.x,
        spec.y,
        spec.fps,
        if spec.flags & user_abi::CAP_POINTER != 0 { "on" } else { "off" }
    );
    // The panel shows the red indicator and the mirror fills at the next
    // desktop tick (half a second at most).
    0
}

/// Copy the mirror into the program's buffer, the pointer patched in.
fn grab(buf: usize, have: u64) -> i64 {
    let (w, h, seq) = {
        let mut state = STATE.lock();
        let Some(capture) = state.as_mut() else {
            return ERR_NOT_FOUND;
        };
        capture.last_grab_ms = crate::time::uptime_ms();
        (capture.rect.w, capture.rect.h, capture.seq)
    };
    if seq == have {
        return seq as i64;
    }
    let bytes = w as usize * h as usize * 4;
    if buf % 4 != 0 || !space::user_can(buf, bytes, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: the whole buffer is the program's, writable, aligned (checked).
    let out = unsafe { core::slice::from_raw_parts_mut(buf as *mut u32, w as usize * h as usize) };
    let mut row = 0;
    while row < h {
        let rows = GRAB_ROWS.min(h - row);
        let state = STATE.lock();
        let Some(capture) = state.as_ref() else {
            return ERR_NOT_FOUND;
        };
        if capture.rect.w != w || capture.rect.h != h {
            return ERR_NOT_FOUND;
        }
        let from = row as usize * w as usize;
        let to = from + rows as usize * w as usize;
        out[from..to].copy_from_slice(&capture.mirror[from..to]);
        drop(state);
        row += rows;
    }
    let mut state = STATE.lock();
    let Some(capture) = state.as_mut() else {
        return ERR_NOT_FOUND;
    };
    capture.grabs += 1;
    let shot = &capture.pointer;
    if capture.pointer_wanted && !shot.drawn && shot.visible && !shot.image.is_empty() {
        let left = shot.x - shot.hot.0 as i32 - capture.rect.x;
        let top = shot.y - shot.hot.1 as i32 - capture.rect.y;
        let side = shot.side as i32;
        for py in 0..side {
            let y = top + py;
            if y < 0 || y >= h as i32 {
                continue;
            }
            for px in 0..side {
                let x = left + px;
                if x < 0 || x >= w as i32 {
                    continue;
                }
                let (color, alpha) = shot.image[(py * side + px) as usize];
                if alpha == 0 {
                    continue;
                }
                let at = y as usize * w as usize + x as usize;
                out[at] = if alpha == 255 { color } else { blend(out[at], color, alpha) };
            }
        }
    }
    seq as i64
}

/// `over` on `under` with `alpha` (channel-wise, format-agnostic).
fn blend(under: u32, over: u32, alpha: u8) -> u32 {
    let a = u32::from(alpha);
    let mut out = 0xFF00_0000;
    for shift in [0, 8, 16] {
        let u = (under >> shift) & 0xFF;
        let o = (over >> shift) & 0xFF;
        out |= ((o * a + u * (255 - a)) / 255) << shift;
    }
    out
}

fn stop(ptr: usize, len: usize) -> i64 {
    if !active() {
        return ERR_NOT_FOUND;
    }
    let grabs = STATE.lock().as_ref().map_or(0, |capture| capture.grabs);
    end();
    if len > 0 && len <= 4096 && space::user_can(ptr, len, PageFlags::READ) {
        // SAFETY: readable range of the program, checked above.
        let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
        if let Ok(path) = core::str::from_utf8(bytes) {
            crate::kprintln!("  capture     : stopped after {grabs} grab(s), saved {path}");
            *SAVED.lock() = Some(String::from(path));
        }
    } else {
        crate::kprintln!("  capture     : stopped after {grabs} grab(s)");
    }
    0
}

fn status(out: usize) -> i64 {
    let answer = {
        let state = STATE.lock();
        match state.as_ref() {
            Some(capture) => CapStatus {
                active: 1,
                stop_requested: u32::from(STOP_ASKED.load(Ordering::Relaxed)),
                w: capture.rect.w,
                h: capture.rect.h,
                elapsed_ms: crate::time::uptime_ms().saturating_sub(STARTED_MS.load(Ordering::Relaxed)),
                seq: capture.seq,
            },
            None => CapStatus::default(),
        }
    };
    write_out(out, answer)
}

/// Windows top first: `x\ty\tw\th\tname\n`.
fn windows(buf: usize, len: usize) -> i64 {
    if len == 0 || !space::user_can(buf, len, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    let Some(text) = super::with_desktop(|desktop| {
        let mut text = String::new();
        let mut index = 0;
        let mut apps = Vec::new();
        while let Some(app) = desktop.app_at(index) {
            apps.push(app);
            index += 1;
        }
        for app in apps.into_iter().rev() {
            if desktop.is_minimized(app) {
                continue;
            }
            let (Some(rect), Some(name)) = (desktop.rect_of(app), desktop.caption_of(app)) else {
                continue;
            };
            let name = name.replace(['\t', '\n'], " ");
            text.push_str(&alloc::format!("{}\t{}\t{}\t{}\t{name}\n", rect.x, rect.y, rect.w, rect.h));
        }
        text
    }) else {
        return ERR_AGAIN;
    };
    let n = text.len().min(len);
    // SAFETY: writable range of the program, checked above.
    unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), buf as *mut u8, n) };
    n as i64
}
