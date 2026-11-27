//! Screen capture (22.11 recorder `/bin/screenrec`, `SYS_SCREENCAP`; 23.11
//! Linux programs through `/dev/fossai-capture`; 26.11 sessions per
//! rectangle).
//!
//! # Sessions
//!
//! A **session** is one captured rectangle (a monitor, the whole desktop, a
//! window's place) with its own mirror at its own size. Everybody capturing
//! the same rectangle shares the session — OBS's sources on monitor 1 and on
//! monitor 2 are two sessions of 1280x800 each (a monitor at its native
//! size), a second source on monitor 1 or the recorder on it join the first
//! one and cost no second copy. Each consumer keeps its own frame number
//! (`have`) and gets the newest frame of the session.
//!
//! The recorder (`/bin/screenrec`) is the one consumer through the native
//! system call (one recording at a time); every open of
//! `/dev/fossai-capture` may hold one session ([`open_session`]).
//!
//! # How a frame gets here
//!
//! The compositor already copies every composed band from its back buffer
//! into the outputs ([`super::compositor`]: `compose`, the direct copy of a
//! fullscreen window and the page-flip path). While captures run, the same
//! pixels are also copied into the mirror of every session they touch
//! ([`put`]) — only the damaged parts, at present time. A grab
//! ([`grab_with`]) is then one copy of the mirror into the consumer's buffer;
//! nothing reads the scanout memory (on NVIDIA that is VRAM behind a BAR, and
//! reading it costs tens of milliseconds per frame).
//!
//! When every consumer of a session samples a fullscreen game's own buffer
//! (OBS, zero copy, [`set_client`]) the desktop does not copy that game into
//! the session's mirror at all ([`put_direct`]).
//!
//! # The pointer
//!
//! When the compositor draws it into the frame (software pointer, no directly
//! shown window) it is already in the mirror. A pointer drawn by the display
//! (hardware cursor, page flip, direct copy) is either patched into grabbed
//! frames from a cached picture (consumers that ask for it, [`CAP_POINTER`]:
//! the recorder) or handed over separately (26.11, [`cursor`]: picture,
//! hotspot and place per frame — OBS draws it as its own quad over the frame,
//! also over a game's buffer that never has it).
//!
//! # Who stops it
//!
//! The recording program ([`user_abi::CAP_STOP`]). Win+Alt+R, the tray
//! indicator and the recorder window only *ask* ([`request_stop`]); the
//! program sees it in [`user_abi::CAP_STATUS`], finishes the file and stops.
//! A recorder that died without stopping is noticed by [`tick`]: no grab for
//! [`ORPHAN_MS`] ends its capture. A session of `/dev/fossai-capture` ends
//! when its descriptor is closed (the device's `Drop`).
//!
//! [`CAP_POINTER`]: user_abi::CAP_POINTER

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use mini_ui::{Rect, Surface};
use user_abi::{CapCursor, CapInfo, CapSpec, CapStatus, ERR_AGAIN, ERR_BAD_ADDRESS, ERR_EXISTS, ERR_LIMIT, ERR_NOT_FOUND, ERR_UNSUPPORTED};

use crate::mm::PageFlags;
use crate::sync::SpinLock;
use crate::user::space;

use super::compositor::Compositor;

/// A recording without a grab for this long belongs to a program that is gone.
const ORPHAN_MS: u64 = 4_000;

/// Rows copied per hold of the lock in [`grab_with`] and [`put`]: the other
/// side waits for it at most this many rows (spin lock, interrupts off).
const GRAB_ROWS: u32 = 32;

/// Largest side of a capture.
const MAX_SIDE: u32 = 16_384;

/// Sessions at once (different rectangles captured at the same time).
const MAX_SESSIONS: usize = 8;

/// Side of the pointer picture handed over / patched in.
const POINTER_SIDE: u32 = 48;

/// Fast check for the compositor hooks — no lock when nothing is captured.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Start time (ms of uptime) of the running recording, for the tray; 0 — none.
static STARTED_MS: AtomicU64 = AtomicU64::new(0);

/// Somebody asked the recorder to stop (hotkey, tray, window).
static STOP_ASKED: AtomicBool = AtomicBool::new(false);

/// Some session wants the compositor to repaint its rectangle once (fills
/// the mirror at start, or after the game-buffer mode left it stale).
static REPAINT: AtomicBool = AtomicBool::new(false);

/// A saved file to announce («Запись сохранена»), shown by [`tick`].
static SAVED: SpinLock<Option<String>> = SpinLock::new(None);

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

static SESSIONS: SpinLock<Vec<Session>> = SpinLock::new(Vec::new());

/// The recorder's session (`SYS_SCREENCAP`).
static RECORDER: SpinLock<Option<Recorder>> = SpinLock::new(None);

/// The pointer as the display shows it.
static POINTER: SpinLock<PointerShot> = SpinLock::new(PointerShot::NONE);

#[derive(Clone, Copy)]
struct Recorder {
    session: u32,
    pointer: bool,
}

struct Session {
    id: u32,
    /// Captured rectangle in desktop coordinates.
    rect: Rect,
    mirror: Vec<u32>,
    /// Bumped by every change of the mirror or of a patched pointer.
    seq: u64,
    /// Consumers of the session, those that have the pointer patched into
    /// their frames, and those that sample the game buffer ([`set_client`]).
    users: u32,
    pointer_users: u32,
    clients: u32,
    /// The compositor should repaint the rectangle once.
    repaint: bool,
    last_grab_ms: u64,
    grabs: u64,
    started_ms: u64,
    /// 25.11: the captured rectangle is an output shown straight from a
    /// contiguous client buffer - its physical address and pitch
    /// ([`note_direct`]).
    direct: Option<(u64, u32)>,
}

impl Session {
    /// Every consumer samples the game's buffer: the mirror is not kept.
    fn all_clients(&self) -> bool {
        self.clients > 0 && self.clients >= self.users
    }
}

/// The pointer as the display shows it, for patching into grabbed frames and
/// for [`cursor`].
struct PointerShot {
    /// Already drawn into the composed frames by the compositor.
    drawn: bool,
    visible: bool,
    x: i32,
    y: i32,
    key: usize,
    hot: (u32, u32),
    side: u32,
    /// Number of the picture (bumped with every new picture; 0 — none yet).
    serial: u32,
    /// `0xAARRGGBB` points, `side`×`side`.
    argb: Vec<u32>,
}

impl PointerShot {
    const NONE: Self = Self { drawn: true, visible: false, x: 0, y: 0, key: usize::MAX, hot: (0, 0), side: 0, serial: 0, argb: Vec::new() };

    /// Where the picture is on the desktop.
    fn rect(&self) -> Rect {
        Rect::new(self.x - self.hot.0 as i32, self.y - self.hot.1 as i32, self.side, self.side)
    }

    /// Shown by the display, not in the composed frame.
    fn separate(&self) -> bool {
        self.visible && !self.drawn && !self.argb.is_empty()
    }
}

/// Is a capture running (cheap).
#[inline]
#[must_use]
pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Does the screen recorder (`/bin/screenrec`) record now. OBS's sources
/// (`/dev/fossai-capture`) capture for their preview whether OBS records or
/// not — they are not a recording of ours (24.11: no red tray indicator).
#[must_use]
pub fn recording() -> bool {
    STARTED_MS.load(Ordering::Relaxed) != 0
}

/// Seconds since the start of the running recording (tray indicator);
/// `None` — no recording.
#[must_use]
pub fn elapsed_s() -> Option<u64> {
    let started = STARTED_MS.load(Ordering::Relaxed);
    (started != 0).then(|| crate::time::uptime_ms().saturating_sub(started) / 1000)
}

/// Ask the recording program to stop (Win+Alt+R, tray, recorder window).
pub fn request_stop() {
    if recording() {
        STOP_ASKED.store(true, Ordering::Relaxed);
        crate::kprintln!("  capture     : stop requested");
    }
}

/// Compositor hook: the rectangle to repaint once so new mirrors fill.
pub fn take_repaint() -> Option<Rect> {
    if !REPAINT.swap(false, Ordering::Relaxed) {
        return None;
    }
    let mut out = Rect::EMPTY;
    for session in SESSIONS.lock().iter_mut().filter(|session| session.repaint) {
        session.repaint = false;
        out = out.union(&session.rect);
    }
    (!out.is_empty()).then_some(out)
}

/// Compositor hook: `part` (desktop coordinates) now shows the pixels of
/// `src` at `(sx, sy)` — a composed band.
pub fn put(part: Rect, src: &Surface, sx: i32, sy: i32) {
    put_into(part, src, sx, sy, false);
}

/// Compositor hook: `part` shows a directly shown (fullscreen) window's
/// pixels. Sessions whose consumers all sample that window's buffer
/// themselves skip the copy (25.11/26.11).
pub fn put_direct(part: Rect, src: &Surface, sx: i32, sy: i32) {
    put_into(part, src, sx, sy, true);
}

/// Does any session need the directly shown windows copied (cheap check
/// before [`put_direct`]).
#[must_use]
pub fn mirror_direct() -> bool {
    SESSIONS.lock().iter().any(|session| !session.all_clients())
}

fn put_into(part: Rect, src: &Surface, sx: i32, sy: i32, direct: bool) {
    let mut targets = [(0u32, Rect::EMPTY); MAX_SESSIONS];
    let mut count = 0;
    for session in SESSIONS.lock().iter() {
        if count < MAX_SESSIONS && !(direct && session.all_clients()) && !part.intersect(&session.rect).is_empty() {
            targets[count] = (session.id, session.rect);
            count += 1;
        }
    }
    for &(id, rect) in &targets[..count] {
        copy_into(id, rect, part, src, sx, sy);
    }
}

/// The copy goes [`GRAB_ROWS`] rows per hold of the lock (24.11): a whole
/// 2560x1440 frame of a fullscreen game under one hold kept interrupts off
/// for 2.4-3.1 ms every frame (log 17 of Roman's PC). Between holds the
/// session may have ended — then the rest is dropped.
fn copy_into(id: u32, rect: Rect, part: Rect, src: &Surface, sx: i32, sy: i32) {
    let clip = part.intersect(&rect);
    if clip.is_empty() {
        return;
    }
    let (dx, dy) = (clip.x - part.x, clip.y - part.y);
    let (sx, sy) = (sx + dx, sy + dy);
    if sx < 0 || sy < 0 || (sx as u32).saturating_add(clip.w) > src.width() || (sy as u32).saturating_add(clip.h) > src.height() {
        return;
    }
    let width = rect.w as usize;
    let col = (clip.x - rect.x) as usize;
    let mut row = 0;
    while row < clip.h {
        let rows = GRAB_ROWS.min(clip.h - row);
        let mut sessions = SESSIONS.lock();
        let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
            return;
        };
        for row in row..row + rows {
            let from = &src.row(sy as u32 + row)[sx as usize..sx as usize + clip.w as usize];
            let at = (clip.y - rect.y) as usize + row as usize;
            session.mirror[at * width + col..at * width + col + clip.w as usize].copy_from_slice(from);
        }
        if row + rows >= clip.h {
            session.seq += 1;
        }
        drop(sessions);
        row += rows;
    }
}

/// Compositor hook, once per presented frame: where the pointer is, its
/// picture and whether the compositor drew it into the frame itself.
pub fn note_pointer(pointer: &super::pointer::Pointer, drawn: bool) {
    let (x, y) = pointer.position();
    let visible = pointer.is_visible();
    // The picture changes with the shape and the size («Крупный указатель»).
    let key = pointer.image_key() ^ ((pointer.rect().w as usize) << 20);
    let stale = {
        let shot = POINTER.lock();
        shot.argb.is_empty() || shot.key != key
    };
    // The picture is made outside the lock (an allocation).
    let picture = stale.then(|| pointer.image(POINTER_SIDE));
    let (before, after, moved, old) = {
        let mut shot = POINTER.lock();
        let before = shot.separate().then(|| shot.rect());
        let moved = shot.x != x || shot.y != y || shot.visible != visible || shot.key != key || shot.drawn != drawn;
        let mut old = None;
        if let Some((argb, hot)) = picture {
            old = Some(core::mem::replace(&mut shot.argb, argb));
            shot.hot = hot;
            shot.side = POINTER_SIDE;
            shot.serial = shot.serial.wrapping_add(1).max(1);
        }
        shot.key = key;
        shot.x = x;
        shot.y = y;
        shot.visible = visible;
        shot.drawn = drawn;
        let after = shot.separate().then(|| shot.rect());
        (before, after, moved, old)
    };
    drop(old);
    if !moved || (before.is_none() && after.is_none()) {
        return;
    }
    // Sessions with the pointer patched into their frames have a new frame
    // when the pointer moved over them.
    for session in SESSIONS.lock().iter_mut().filter(|session| session.pointer_users > 0) {
        let over = |spot: Option<Rect>| spot.is_some_and(|spot| !spot.intersect(&session.rect).is_empty());
        if over(before) || over(after) {
            session.seq += 1;
        }
    }
}

/// Desktop tick (every half second): forget a recording whose program is
/// gone, show «Запись сохранена».
pub fn tick(desktop: &mut Compositor) {
    if active() {
        let recorder = *RECORDER.lock();
        let stale = recorder.is_some_and(|recorder| {
            SESSIONS.lock().iter().find(|session| session.id == recorder.session).is_none_or(|session| {
                crate::time::uptime_ms().saturating_sub(session.last_grab_ms) > ORPHAN_MS
            })
        });
        if stale {
            crate::kprintln!("  capture     : the recorder stopped grabbing, capture ended");
            end_recorder();
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

/// End the recorder's session (stop, orphan); its grab count.
fn end_recorder() -> u64 {
    let recorder = RECORDER.lock().take();
    STARTED_MS.store(0, Ordering::Relaxed);
    STOP_ASKED.store(false, Ordering::Relaxed);
    let Some(recorder) = recorder else {
        return 0;
    };
    // The session goes on when OBS captures the same monitor (26.11).
    close_session(recorder.session, recorder.pointer, false).unwrap_or_else(|| {
        crate::kprintln!("  capture     : the recording left session {}, its other consumers go on", recorder.session);
        0
    })
}

// ---------------------------------------------------------------------------
// Sessions (26.11)
// ---------------------------------------------------------------------------

/// Start capturing `spec` (or join the session already capturing exactly
/// that rectangle): the session number. `CAP_POINTER` in the flags — this
/// consumer has the pointer patched into its frames.
pub fn open_session(spec: &CapSpec) -> Result<u32, i64> {
    if spec.w == 0 || spec.h == 0 || spec.w > MAX_SIDE || spec.h > MAX_SIDE {
        return Err(ERR_LIMIT);
    }
    let rect = Rect::new(spec.x, spec.y, spec.w, spec.h);
    let pointer = spec.flags & user_abi::CAP_POINTER != 0;
    if let Some(joined) = join(rect, pointer) {
        return Ok(joined);
    }
    // A new mirror, made outside the lock.
    let points = spec.w as usize * spec.h as usize;
    let mut mirror = Vec::new();
    if mirror.try_reserve_exact(points).is_err() {
        return Err(ERR_LIMIT);
    }
    mirror.resize(points, mini_ui::Color::rgb(0, 0, 0).pixel());
    let now = crate::time::uptime_ms();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut sessions = SESSIONS.lock();
        if !sessions.iter().any(|session| session.rect == rect) {
            if sessions.len() >= MAX_SESSIONS {
                drop(sessions);
                return Err(ERR_LIMIT);
            }
            sessions.push(Session {
                id,
                rect,
                mirror,
                seq: 1,
                users: 1,
                pointer_users: u32::from(pointer),
                clients: 0,
                repaint: true,
                last_grab_ms: now,
                grabs: 0,
                started_ms: now,
                direct: None,
            });
            let count = sessions.len();
            drop(sessions);
            REPAINT.store(true, Ordering::Relaxed);
            ACTIVE.store(true, Ordering::Relaxed);
            crate::kprintln!(
                "  capture     : started {}x{} at {},{} ({} fps, pointer {}), session {id} of {count}",
                spec.w,
                spec.h,
                spec.x,
                spec.y,
                spec.fps,
                if pointer { "on" } else { "off" }
            );
            return Ok(id);
        }
    }
    // Somebody started the same rectangle meanwhile.
    drop(mirror);
    join(rect, pointer).ok_or(ERR_NOT_FOUND)
}

/// Join the session of `rect` if there is one.
fn join(rect: Rect, pointer: bool) -> Option<u32> {
    let mut sessions = SESSIONS.lock();
    let session = sessions.iter_mut().find(|session| session.rect == rect)?;
    let stale = session.all_clients();
    session.users += 1;
    if pointer {
        session.pointer_users += 1;
        session.seq += 1;
    }
    // The mirror was not kept while only game-buffer consumers used it.
    if stale {
        session.repaint = true;
        REPAINT.store(true, Ordering::Relaxed);
    }
    let (id, users) = (session.id, session.users);
    drop(sessions);
    crate::kprintln!("  capture     : joined {}x{} at {},{} (session {id}, {users} consumers, no second copy)", rect.w, rect.h, rect.x, rect.y);
    Some(id)
}

/// A consumer leaves the session (`pointer`, `client` — as it was in it).
/// `Some(grabs)` — it was the last one and the session ended.
pub fn close_session(id: u32, pointer: bool, client: bool) -> Option<u64> {
    let mut sessions = SESSIONS.lock();
    let at = sessions.iter().position(|session| session.id == id)?;
    let session = &mut sessions[at];
    let stale = session.all_clients();
    session.users = session.users.saturating_sub(1);
    if pointer {
        session.pointer_users = session.pointer_users.saturating_sub(1);
    }
    if client {
        session.clients = session.clients.saturating_sub(1);
    }
    if session.users > 0 {
        if stale && !session.all_clients() {
            session.repaint = true;
            REPAINT.store(true, Ordering::Relaxed);
        }
        return None;
    }
    let gone = sessions.swap_remove(at);
    ACTIVE.store(!sessions.is_empty(), Ordering::Relaxed);
    drop(sessions);
    let grabs = gone.grabs;
    drop(gone);
    Some(grabs)
}

/// Size of the session; `None` - it is gone.
#[must_use]
pub fn size(id: u32) -> Option<(u32, u32)> {
    SESSIONS.lock().iter().find(|session| session.id == id).map(|session| (session.rect.w, session.rect.h))
}

/// The consumer is alive (a frame was handed without a grab).
pub fn touch(id: u32) {
    if let Some(session) = SESSIONS.lock().iter_mut().find(|session| session.id == id) {
        session.last_grab_ms = crate::time::uptime_ms();
    }
}

/// The session's state for its consumer (`started_ms` — when that consumer
/// started).
pub fn session_status(id: u32, started_ms: u64, out: usize) -> i64 {
    let answer = SESSIONS.lock().iter().find(|session| session.id == id).map_or_else(CapStatus::default, |session| CapStatus {
        active: 1,
        stop_requested: 0,
        w: session.rect.w,
        h: session.rect.h,
        elapsed_ms: crate::time::uptime_ms().saturating_sub(started_ms.max(session.started_ms)),
        seq: session.seq,
    });
    write_out(out, answer)
}

// ---------------------------------------------------------------------------
// SYS_SCREENCAP
// ---------------------------------------------------------------------------

/// `SYS_SCREENCAP` (see the contract in `user_abi`): the recorder.
pub fn syscall(action: usize, a1: usize, a2: usize) -> i64 {
    let recorder = || *RECORDER.lock();
    match action {
        user_abi::CAP_INFO => info(a1),
        user_abi::CAP_START => start(a1),
        user_abi::CAP_GRAB => match recorder() {
            Some(recorder) => grab(recorder.session, a1, a2 as u64, recorder.pointer),
            None => ERR_NOT_FOUND,
        },
        user_abi::CAP_STOP => stop(a1, a2),
        user_abi::CAP_STATUS => status(a1),
        user_abi::CAP_REQUEST_STOP => {
            if !recording() {
                return ERR_NOT_FOUND;
            }
            request_stop();
            0
        }
        user_abi::CAP_WINDOWS => windows(a1, a2),
        // 24.11: «Определить» — number plates on the monitors.
        user_abi::CAP_IDENTIFY => {
            super::identify_monitors(a1 as u64);
            0
        }
        user_abi::CAP_CURSOR => match recorder() {
            Some(recorder) => cursor_at(recorder.session, a1),
            None => ERR_NOT_FOUND,
        },
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

/// Read a plain `#[repr(C)]` argument from the program.
fn read_in<T: Copy>(ptr: usize) -> Result<T, i64> {
    if ptr % align_of::<T>() != 0 || !space::user_can(ptr, size_of::<T>(), PageFlags::READ) {
        return Err(ERR_BAD_ADDRESS);
    }
    // SAFETY: aligned and readable, checked above.
    Ok(unsafe { core::ptr::read(ptr as *const T) })
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

/// The recorder starts.
fn start(ptr: usize) -> i64 {
    let spec = match read_in::<CapSpec>(ptr) {
        Ok(spec) => spec,
        Err(err) => return err,
    };
    if RECORDER.lock().is_some() {
        return ERR_EXISTS;
    }
    let session = match open_session(&spec) {
        Ok(session) => session,
        Err(err) => return err,
    };
    let pointer = spec.flags & user_abi::CAP_POINTER != 0;
    {
        let mut recorder = RECORDER.lock();
        if recorder.is_some() {
            drop(recorder);
            close_session(session, pointer, false);
            return ERR_EXISTS;
        }
        *recorder = Some(Recorder { session, pointer });
    }
    STOP_ASKED.store(false, Ordering::Relaxed);
    STARTED_MS.store(crate::time::uptime_ms().max(1), Ordering::Relaxed);
    // The panel shows the red indicator and the mirror fills at the next
    // desktop tick (half a second at most).
    0
}

/// Where a grab puts the frame: the program's buffer ([`grab`]) or a slot of
/// the dma-buf ring of `/dev/fossai-capture` (25.11, OBS on the GPU).
pub trait FrameSink {
    /// `points` are rows `first..` of the frame, `width` points each.
    fn put_rows(&mut self, first: u32, width: u32, points: &[u32]);
    /// The point at `(x, y)` of the frame (the pointer patch).
    fn point(&mut self, x: u32, y: u32) -> &mut u32;
}

/// The program's own buffer, rows packed.
struct SliceSink<'a> {
    out: &'a mut [u32],
    width: usize,
}

impl FrameSink for SliceSink<'_> {
    fn put_rows(&mut self, first: u32, width: u32, points: &[u32]) {
        let from = first as usize * width as usize;
        self.out[from..from + points.len()].copy_from_slice(points);
    }
    fn point(&mut self, x: u32, y: u32) -> &mut u32 {
        &mut self.out[y as usize * self.width + x as usize]
    }
}

/// Copy the session's mirror into the program's buffer `buf`, the pointer
/// patched in when `pointer`.
pub fn grab(id: u32, buf: usize, have: u64, pointer: bool) -> i64 {
    grab_with(id, have, pointer, |w, h| {
        let bytes = w as usize * h as usize * 4;
        if buf % 4 != 0 || !space::user_can(buf, bytes, PageFlags::WRITE) {
            return Err(ERR_BAD_ADDRESS);
        }
        // SAFETY: the whole buffer is the program's, writable, aligned (checked).
        let out = unsafe { core::slice::from_raw_parts_mut(buf as *mut u32, w as usize * h as usize) };
        Ok(SliceSink { out, width: w as usize })
    })
}

/// A grab of session `id` into any sink: nothing when the mirror still has
/// frame `have` (the answer is that number), else the mirror goes to the
/// sink `make` builds for the size, the pointer patched in when `pointer`.
/// The answer is the frame number or a native error.
pub fn grab_with<S: FrameSink>(id: u32, have: u64, pointer: bool, make: impl FnOnce(u32, u32) -> Result<S, i64>) -> i64 {
    let (rect, seq) = {
        let mut sessions = SESSIONS.lock();
        let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
            return ERR_NOT_FOUND;
        };
        session.last_grab_ms = crate::time::uptime_ms();
        (session.rect, session.seq)
    };
    if seq == have {
        return seq as i64;
    }
    let (w, h) = (rect.w, rect.h);
    let mut out = match make(w, h) {
        Ok(out) => out,
        Err(err) => return err,
    };
    let mut row = 0;
    while row < h {
        let rows = GRAB_ROWS.min(h - row);
        let sessions = SESSIONS.lock();
        let Some(session) = sessions.iter().find(|session| session.id == id) else {
            return ERR_NOT_FOUND;
        };
        let from = row as usize * w as usize;
        let to = from + rows as usize * w as usize;
        out.put_rows(row, w, &session.mirror[from..to]);
        drop(sessions);
        row += rows;
    }
    if let Some(session) = SESSIONS.lock().iter_mut().find(|session| session.id == id) {
        session.grabs += 1;
    }
    if pointer {
        patch_pointer(&mut out, rect);
    }
    seq as i64
}

/// The pointer shown by the display, drawn into a grabbed frame of `rect`.
fn patch_pointer<S: FrameSink>(out: &mut S, rect: Rect) {
    let shot = POINTER.lock();
    if !shot.separate() {
        return;
    }
    let left = shot.x - shot.hot.0 as i32 - rect.x;
    let top = shot.y - shot.hot.1 as i32 - rect.y;
    let side = shot.side as i32;
    for py in 0..side {
        let y = top + py;
        if y < 0 || y >= rect.h as i32 {
            continue;
        }
        for px in 0..side {
            let x = left + px;
            if x < 0 || x >= rect.w as i32 {
                continue;
            }
            let argb = shot.argb[(py * side + px) as usize];
            let alpha = (argb >> 24) as u8;
            if alpha == 0 {
                continue;
            }
            let color = mini_ui::Color::rgb((argb >> 16) as u8, (argb >> 8) as u8, argb as u8).pixel();
            let at = out.point(x as u32, y as u32);
            *at = if alpha == 255 { color } else { blend(*at, color, alpha) };
        }
    }
}

/// The pointer over session `id` (26.11): place relative to the captured
/// rectangle, hotspot, flags, and the picture copied into `args.image` when
/// the caller's `have_serial` is old and there is room.
pub fn cursor(id: u32, args: &mut CapCursor) -> i64 {
    let Some((rx, ry)) = SESSIONS.lock().iter().find(|session| session.id == id).map(|session| (session.rect.x, session.rect.y)) else {
        return ERR_NOT_FOUND;
    };
    let shot = POINTER.lock();
    let mut flags = 0;
    if shot.visible {
        flags |= user_abi::CAP_CURSOR_VISIBLE;
    }
    if shot.drawn {
        flags |= user_abi::CAP_CURSOR_IN_FRAME;
    }
    args.serial = shot.serial;
    args.x = shot.x - shot.hot.0 as i32 - rx;
    args.y = shot.y - shot.hot.1 as i32 - ry;
    args.hot_x = shot.hot.0;
    args.hot_y = shot.hot.1;
    args.width = shot.side;
    args.height = shot.side;
    let bytes = shot.argb.len() * 4;
    let image = args.image as usize;
    if image != 0 && bytes > 0 && args.have_serial != shot.serial && args.image_len as usize >= bytes {
        if image % 4 != 0 || !space::user_can(image, bytes, PageFlags::WRITE) {
            return ERR_BAD_ADDRESS;
        }
        // SAFETY: the program's writable range of `bytes`, checked above.
        unsafe { core::ptr::copy_nonoverlapping(shot.argb.as_ptr(), image as *mut u32, shot.argb.len()) };
        flags |= user_abi::CAP_CURSOR_IMAGE;
    }
    args.flags = flags;
    0
}

/// [`cursor`] with the argument in the program (`SYS_SCREENCAP`).
fn cursor_at(id: u32, ptr: usize) -> i64 {
    let mut args = match read_in::<CapCursor>(ptr) {
        Ok(args) => args,
        Err(err) => return err,
    };
    let answer = cursor(id, &mut args);
    if answer < 0 {
        return answer;
    }
    write_out(ptr, args)
}

// ---------------------------------------------------------------------------
// 25.11: a fullscreen game's own buffer for OBS (zero copy)
// ---------------------------------------------------------------------------

/// Desktop hook, every presented frame while captures run: the outputs
/// shown straight from a client buffer - `(output rect, physical address,
/// pitch)`. A session whose rectangle is exactly one of them remembers it.
pub fn note_direct(sources: &[(Rect, u64, u32)]) {
    for session in SESSIONS.lock().iter_mut() {
        session.direct = sources.iter().find(|(rect, _, _)| *rect == session.rect).map(|&(_, phys, pitch)| (phys, pitch));
    }
}

/// The session's rectangle is an output shown straight from one contiguous
/// client buffer now: its physical address and pitch.
#[must_use]
pub fn direct_source(id: u32) -> Option<(u64, u32)> {
    SESSIONS.lock().iter().find(|session| session.id == id).and_then(|session| session.direct)
}

/// A consumer of session `id` samples the client buffer ([`direct_source`])
/// from now on, or stops sampling it (the caller sends changes only). When
/// all consumers sample it the desktop does not copy that window into the
/// mirror; when one stops, the mirror is repainted whole once.
pub fn set_client(id: u32, on: bool) {
    let mut sessions = SESSIONS.lock();
    let Some(session) = sessions.iter_mut().find(|session| session.id == id) else {
        return;
    };
    let was = session.all_clients();
    if on {
        session.clients += 1;
    } else {
        session.clients = session.clients.saturating_sub(1);
    }
    let now = session.all_clients();
    if was && !now {
        session.repaint = true;
        REPAINT.store(true, Ordering::Relaxed);
    }
    drop(sessions);
    crate::kprintln!(
        "  capture     : {} (session {id})",
        if on { "the source samples the fullscreen window's buffer (no copy)" } else { "the source takes composed frames again" }
    );
}

// ---------------------------------------------------------------------------
// 27.11 (hw-log20): the game buffer a source samples is not the game's to draw
// into until the source moves on
// ---------------------------------------------------------------------------

/// Game buffers handed to a source by `FRAME`: `(consumer key, physical
/// address, length, ms of uptime when handed)`. One per consumer.
static GAME_HELD: SpinLock<Vec<(usize, u64, u64, u64)>> = SpinLock::new(Vec::new());

/// A held buffer is let go after this long without the next `FRAME` (a
/// source that stopped asking must not keep a game's buffer forever).
const GAME_HOLD_MS: u64 = 250;

/// Consumer `key` samples the game buffer `(phys, len)` from now on (`None`
/// - no game buffer any more). Until its next `FRAME` the Wayland server
/// does not give that buffer back to the game ([`game_held`]).
///
/// hw-log20 (D2R at 160-180 fps under OBS, «картинка трясётся»): the
/// display refused flips, the desktop showed the game by a copy and gave
/// every buffer back as soon as the next one came; OBS sampled «the game's
/// buffer» 30 times a second while the game was already drawing a newer
/// frame into it - frames out of order and half drawn in the preview and
/// the recording.
pub fn hold_game(key: usize, buffer: Option<(u64, u64)>) {
    let now = crate::time::uptime_ms();
    let mut held = GAME_HELD.lock();
    held.retain(|&(k, _, _, _)| k != key);
    if let Some((phys, len)) = buffer {
        held.push((key, phys, len, now));
    }
}

/// May a source still sample the client buffer at `phys` (`len` bytes)?
/// Held by [`hold_game`], or it is the directly shown buffer of a session
/// whose source samples game buffers (the desktop notes the new buffer of
/// the window only with its next frame - a `FRAME` in between hands the one
/// noted). The Wayland server keeps such a buffer from the client.
#[must_use]
pub fn game_held(phys: u64, len: u64) -> bool {
    if !active() {
        return false;
    }
    let overlaps = |p: u64, l: u64| phys < p.saturating_add(l) && p < phys.saturating_add(len);
    let now = crate::time::uptime_ms();
    if GAME_HELD.lock().iter().any(|&(_, p, l, since)| now.saturating_sub(since) < GAME_HOLD_MS && overlaps(p, l)) {
        return true;
    }
    SESSIONS.lock().iter().any(|session| {
        session.clients > 0 && session.direct.is_some_and(|(p, pitch)| overlaps(p, u64::from(pitch) * u64::from(session.rect.h)))
    })
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

/// The recorder stops (`ptr`/`len` — the saved file).
fn stop(ptr: usize, len: usize) -> i64 {
    if !recording() {
        return ERR_NOT_FOUND;
    }
    let grabs = end_recorder();
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

/// The recorder's state.
fn status(out: usize) -> i64 {
    let recorder = *RECORDER.lock();
    let Some(recorder) = recorder else {
        return write_out(out, CapStatus::default());
    };
    let answer = SESSIONS.lock().iter().find(|session| session.id == recorder.session).map_or_else(CapStatus::default, |session| CapStatus {
        active: 1,
        stop_requested: u32::from(STOP_ASKED.load(Ordering::Relaxed)),
        w: session.rect.w,
        h: session.rect.h,
        elapsed_ms: crate::time::uptime_ms().saturating_sub(STARTED_MS.load(Ordering::Relaxed)),
        seq: session.seq,
    });
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
