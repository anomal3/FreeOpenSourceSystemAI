//! «Демонстрация экрана» (28.11): the desktop's own picker of what a program
//! shows to others — a monitor or a window, like the share dialogs of
//! Windows and GNOME.
//!
//! # Who asks
//!
//! The ScreenCast portal (`libc/linux/fossai-portal.c`, Chromium's
//! getDisplayMedia, Rupor's screen share) on `Start`: it opens
//! `/dev/fossai-capture` and asks with `FOSSAI_CAP_PICK` ([`ask`]) — the
//! source types the program allows (monitor, window), whether the pointer is
//! wanted, and the asking program's name. The desktop shows the window at its
//! next tick ([`tick`]); the portal keeps serving its bus and polls
//! (`FOSSAI_CAP_PICK_POLL`, [`poll`]) until the answer is there: the chosen
//! rectangle (a monitor, or the place of a window), its monitor number and
//! name, and the pointer switch. «Отмена», Esc or the window's cross answer
//! «cancelled» (the portal answers the program with response 1). The portal
//! closing its descriptor, or `FOSSAI_CAP_PICK_CANCEL` (the program closed
//! its request), takes the window away ([`cancel`]).
//!
//! # What it shows
//!
//! Cards with miniatures: every monitor (its number of «Параметры → Дисплей»,
//! size, «основной»), then the open windows top first (icon and title). The
//! miniatures are drawn once when the window opens by the compositor itself
//! ([`Compositor::snapshot`]) — the desktop at that moment, averaged down —
//! so they need no running capture. One picker at a time; a second program
//! asking while it is open gets `EBUSY` and tries again.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use crate::input::KeyCode;
use crate::sync::SpinLock;

use super::appicon::Art;
use super::compositor::Compositor;
use super::window::{App, Window};

/// Source types (the portal's `types` bit mask).
pub const TYPE_MONITOR: u32 = 1;
pub const TYPE_WINDOW: u32 = 2;

/// [`CapPick::flags`]: in — the pointer is on at first; out — it was chosen.
pub const PICK_POINTER: u32 = 1;

/// [`CapPick::state`].
pub const STATE_WAITING: u32 = 0;
pub const STATE_CHOSEN: u32 = 1;
pub const STATE_CANCELLED: u32 = 2;

/// Argument of `FOSSAI_CAP_PICK` / answer of `FOSSAI_CAP_PICK_POLL`
/// (`struct cap_pick` of fossai-portal.c).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CapPick {
    /// In: `TYPE_*` allowed (0 — monitors).
    pub types: u32,
    /// In / out: [`PICK_POINTER`].
    pub flags: u32,
    /// Out: `STATE_*`.
    pub state: u32,
    /// Out: `TYPE_MONITOR` or `TYPE_WINDOW`.
    pub kind: u32,
    /// Out: the chosen rectangle, desktop coordinates.
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
    /// Out: the monitor's number (from 1; for a window — its monitor's).
    pub monitor: u32,
    /// Out of `PICK`: the request's number.
    pub id: u32,
    /// In: who asks (UTF-8, zero padded).
    pub title: [u8; 64],
    /// Out: what was chosen («Экран 2», the window's title).
    pub name: [u8; 96],
}

impl Default for CapPick {
    fn default() -> Self {
        Self { types: 0, flags: 0, state: 0, kind: 0, x: 0, y: 0, w: 0, h: 0, monitor: 0, id: 0, title: [0; 64], name: [0; 96] }
    }
}

/// What the person chose.
#[derive(Clone)]
pub struct Choice {
    pub kind: u32,
    pub rect: Rect,
    pub monitor: u32,
    pub name: String,
    pub pointer: bool,
}

#[derive(Clone)]
enum Phase {
    /// Asked, the desktop has not shown it yet.
    Asked,
    /// On the screen.
    Shown,
    Chosen(Choice),
    Cancelled,
    /// The asker went away: the window is to be closed.
    Dropped,
}

struct Pick {
    id: u32,
    types: u32,
    pointer: bool,
    title: String,
    phase: Phase,
}

static PICK: SpinLock<Option<Pick>> = SpinLock::new(None);
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// A program asks (`FOSSAI_CAP_PICK`): the request's number, or
/// [`user_abi::ERR_EXISTS`] while another picker is open.
pub fn ask(types: u32, pointer: bool, title: &str) -> Result<u32, i64> {
    let mut pick = PICK.lock();
    if pick.as_ref().is_some_and(|pick| matches!(pick.phase, Phase::Asked | Phase::Shown | Phase::Dropped)) {
        return Err(user_abi::ERR_EXISTS);
    }
    let types = if types & (TYPE_MONITOR | TYPE_WINDOW) == 0 { TYPE_MONITOR } else { types & (TYPE_MONITOR | TYPE_WINDOW) };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let title: String = title.chars().filter(|c| !c.is_control()).take(40).collect();
    *pick = Some(Pick { id, types, pointer, title, phase: Phase::Asked });
    drop(pick);
    Ok(id)
}

/// The state of request `id`; `None` — no such request (answered and taken
/// by a newer one).
#[must_use]
pub fn poll(id: u32) -> Option<CapPick> {
    let pick = PICK.lock();
    let pick = pick.as_ref().filter(|pick| pick.id == id)?;
    let mut out = CapPick { id, ..CapPick::default() };
    match &pick.phase {
        Phase::Asked | Phase::Shown => out.state = STATE_WAITING,
        Phase::Cancelled | Phase::Dropped => out.state = STATE_CANCELLED,
        Phase::Chosen(choice) => {
            out.state = STATE_CHOSEN;
            out.kind = choice.kind;
            out.x = choice.rect.x;
            out.y = choice.rect.y;
            out.w = choice.rect.w;
            out.h = choice.rect.h;
            out.monitor = choice.monitor;
            out.flags = if choice.pointer { PICK_POINTER } else { 0 };
            let bytes = choice.name.as_bytes();
            let mut n = bytes.len().min(out.name.len() - 1);
            while n > 0 && !choice.name.is_char_boundary(n) {
                n -= 1;
            }
            out.name[..n].copy_from_slice(&bytes[..n]);
        }
    }
    Some(out)
}

/// The asker does not wait any more (closed its request or descriptor): an
/// open window goes away at the next tick.
pub fn cancel(id: u32) {
    let mut pick = PICK.lock();
    if let Some(pick) = pick.as_mut().filter(|pick| pick.id == id) {
        match pick.phase {
            Phase::Asked => pick.phase = Phase::Cancelled,
            Phase::Shown => {
                pick.phase = Phase::Dropped;
            }
            _ => {}
        }
    }
}

/// Desktop tick: show an asked picker, notice one closed by its cross, close
/// one whose asker went away.
pub fn tick(desktop: &mut Compositor) {
    let phase = {
        let pick = PICK.lock();
        match pick.as_ref() {
            Some(pick) => (pick.id, pick.types, pick.pointer, pick.title.clone(), pick.phase.clone()),
            None => return,
        }
    };
    let (id, types, pointer, title, phase) = phase;
    match phase {
        Phase::Asked => {
            show(desktop, id, types, pointer, &title);
        }
        Phase::Shown => {
            if desktop.find(App::SharePick).is_none() {
                set_phase(id, Phase::Cancelled);
                crate::kprintln!("  share       : picker closed, nothing shared");
            }
        }
        Phase::Dropped => {
            if desktop.close(App::SharePick) {
                crate::kprintln!("  share       : picker closed (the program stopped asking)");
            }
            set_phase(id, Phase::Cancelled);
        }
        Phase::Chosen(_) | Phase::Cancelled => {}
    }
}

fn set_phase(id: u32, phase: Phase) {
    if let Some(pick) = PICK.lock().as_mut().filter(|pick| pick.id == id) {
        pick.phase = phase;
    }
}

/// The answer of the window (`ui::answer_dialog`): «Поделиться» or not.
pub fn answer(desktop: &mut Compositor, confirmed: bool) {
    let choice = if confirmed { desktop.find(App::SharePick).and_then(|window| window.share_choice()) } else { None };
    if desktop.close(App::SharePick) {
        crate::kprintln!("  desktop     : closed '{}'", App::SharePick.title());
    }
    let mut pick = PICK.lock();
    let Some(pick) = pick.as_mut() else { return };
    if !matches!(pick.phase, Phase::Shown) {
        return;
    }
    match choice {
        Some(choice) => {
            crate::kprintln!(
                "  share       : chosen {} '{}' {}x{} at {},{} (monitor {}, pointer {})",
                if choice.kind == TYPE_WINDOW { "window" } else { "monitor" },
                choice.name,
                choice.rect.w,
                choice.rect.h,
                choice.rect.x,
                choice.rect.y,
                choice.monitor,
                if choice.pointer { "on" } else { "off" }
            );
            pick.phase = Phase::Chosen(choice);
        }
        None => {
            crate::kprintln!("  share       : cancelled");
            pick.phase = Phase::Cancelled;
        }
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// Window width in points.
const WIN_W: u32 = 720;
/// Margins (as the other dialogs) and gaps.
const PAD: u32 = 24;
const GAP: u32 = 12;
/// A card: the miniature's box and the caption under it.
const CARD_W: u32 = 216;
const SHOT_W: u32 = 200;
const SHOT_H: u32 = 112;
const CAPTION_H: u32 = 22;
const CARD_H: u32 = 8 + SHOT_H + 6 + CAPTION_H + 6;
/// Header: the tile and two lines of text.
const HEAD_H: u32 = 64;
/// A section label («ЭКРАНЫ», «ОКНА»).
const LABEL_H: u32 = 26;
/// The button band at the bottom (button 32 + margins, line above).
const FOOT_H: u32 = PAD + 32 + PAD + 16;
/// Window cards shown at once (rows), the rest scroll with the wheel.
const WINDOW_ROWS: u32 = 2;
/// Windows listed at most.
const MAX_WINDOWS: usize = 24;

/// One thing to share.
pub struct Source {
    kind: u32,
    rect: Rect,
    monitor: u32,
    name: String,
    /// Second caption part (size, «основной»).
    note: String,
    thumb: Option<Surface>,
    art: Option<Art>,
}

/// Contents of the picker window.
pub struct SharePick {
    title: String,
    sources: Vec<Source>,
    /// Monitors are the first `monitors` sources.
    monitors: usize,
    selected: usize,
    pointer: bool,
    /// First shown row of window cards.
    scroll: u32,
}

/// Cards per row in a content area of this width.
fn columns(ctx: Ctx, area: Rect) -> usize {
    let room = area.w.saturating_sub(ctx.px(PAD) * 2) + ctx.px(GAP);
    (room / ctx.px(CARD_W + GAP)).max(1) as usize
}

/// Window height for these numbers of monitors and windows.
fn window_height(monitors: usize, windows: usize) -> u32 {
    let cols = ((WIN_W - PAD * 2 + GAP) / (CARD_W + GAP)).max(1) as usize;
    let mut h = PAD + HEAD_H + 8;
    if monitors > 0 {
        h += LABEL_H + monitors.div_ceil(cols) as u32 * (CARD_H + GAP);
    }
    if windows > 0 {
        h += LABEL_H + (windows.div_ceil(cols) as u32).min(WINDOW_ROWS) * (CARD_H + GAP);
    }
    h + FOOT_H
}

impl SharePick {
    /// Places of everything, in surface coordinates: cards (index into
    /// `sources`, rectangle; scrolled-out window cards are left out), the
    /// section labels, the pointer switch.
    fn layout(&self, area: Rect, ctx: Ctx) -> Layout {
        let pad = ctx.px(PAD) as i32;
        let x0 = area.x + pad;
        let cols = columns(ctx, area);
        let (cw, ch, gap) = (ctx.px(CARD_W), ctx.px(CARD_H), ctx.px(GAP) as i32);
        let mut y = area.y + pad + ctx.px(HEAD_H + 8) as i32;
        let mut cards = Vec::new();
        let mut labels = Vec::new();
        if self.monitors > 0 {
            labels.push((y, "ЭКРАНЫ"));
            y += ctx.px(LABEL_H) as i32;
            for index in 0..self.monitors {
                let (row, col) = (index / cols, index % cols);
                cards.push((index, Rect::new(x0 + col as i32 * (cw as i32 + gap), y + row as i32 * (ch as i32 + gap), cw, ch)));
            }
            y += self.monitors.div_ceil(cols) as i32 * (ch as i32 + gap);
        }
        let windows = self.sources.len() - self.monitors;
        let foot = area.bottom() - ctx.px(FOOT_H - 16) as i32;
        let mut window_box = Rect::EMPTY;
        if windows > 0 {
            labels.push((y, "ОКНА"));
            y += ctx.px(LABEL_H) as i32;
            let room = (foot - ctx.px(16) as i32 - y).max(0) as u32;
            let rows = (room / (ch + gap as u32)).clamp(1, WINDOW_ROWS);
            window_box = Rect::new(x0, y, area.w.saturating_sub(ctx.px(PAD) * 2), rows * (ch + gap as u32));
            for index in 0..windows {
                let row = (index / cols) as u32;
                if row < self.scroll || row >= self.scroll + rows {
                    continue;
                }
                let col = index % cols;
                cards.push((
                    self.monitors + index,
                    Rect::new(x0 + col as i32 * (cw as i32 + gap), y + (row - self.scroll) as i32 * (ch as i32 + gap), cw, ch),
                ));
            }
        }
        let check = Rect::new(x0, foot, ctx.px(260), ctx.px(32));
        Layout { cards, labels, check, window_box, cols }
    }

    /// Rows of window cards visible now.
    fn visible_rows(&self, area: Rect, ctx: Ctx) -> u32 {
        let layout = self.layout(area, ctx);
        let ch = ctx.px(CARD_H + GAP);
        (layout.window_box.h / ch.max(1)).max(1)
    }

    /// Keep the selected window card on the screen.
    fn reveal(&mut self, area: Rect, ctx: Ctx) {
        if self.selected < self.monitors {
            return;
        }
        let cols = columns(ctx, area);
        let row = ((self.selected - self.monitors) / cols) as u32;
        let rows = self.visible_rows(area, ctx);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + rows {
            self.scroll = row + 1 - rows;
        }
    }

    /// Keys: arrows choose, Space switches the pointer. `true` — used.
    pub fn key(&mut self, code: KeyCode, area: Rect, ctx: Ctx) -> bool {
        let count = self.sources.len();
        if count == 0 {
            return false;
        }
        let cols = columns(ctx, area);
        let last = count - 1;
        match code {
            KeyCode::Right => self.selected = (self.selected + 1).min(last),
            KeyCode::Left => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => self.selected = (self.selected + cols).min(last),
            KeyCode::Up => self.selected = self.selected.saturating_sub(cols),
            KeyCode::Space => self.pointer = !self.pointer,
            _ => return false,
        }
        self.reveal(area, ctx);
        true
    }

    /// A click: a card is chosen, the switch flips. `true` — used.
    pub fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let layout = self.layout(area, ctx);
        if let Some(&(index, _)) = layout.cards.iter().find(|(_, rect)| rect.contains(x, y)) {
            self.selected = index;
            return true;
        }
        if layout.check.contains(x, y) {
            self.pointer = !self.pointer;
            return true;
        }
        false
    }

    /// The wheel scrolls the windows. `true` — moved.
    pub fn wheel(&mut self, clicks: i32, area: Rect, ctx: Ctx) -> bool {
        let windows = self.sources.len() - self.monitors;
        let cols = columns(ctx, area);
        let rows = windows.div_ceil(cols) as u32;
        let shown = self.visible_rows(area, ctx);
        let top = rows.saturating_sub(shown);
        let next = if clicks > 0 { self.scroll.saturating_sub(clicks as u32) } else { (self.scroll + clicks.unsigned_abs()).min(top) };
        if next == self.scroll {
            return false;
        }
        self.scroll = next;
        true
    }

    /// The chosen source.
    #[must_use]
    pub fn choice(&self) -> Option<Choice> {
        let source = self.sources.get(self.selected)?;
        Some(Choice { kind: source.kind, rect: source.rect, monitor: source.monitor, name: source.name.clone(), pointer: self.pointer })
    }

    /// Names and places (surface coordinates) of the cards and the switch,
    /// for the log the test stand aims by.
    #[must_use]
    pub fn spots(&self, area: Rect, ctx: Ctx) -> Vec<(String, Rect)> {
        let layout = self.layout(area, ctx);
        let mut out = Vec::new();
        for (index, rect) in layout.cards {
            let source = &self.sources[index];
            let name = if source.kind == TYPE_MONITOR { format!("monitor-{}", source.monitor) } else { format!("window-{}", index - self.monitors + 1) };
            out.push((name, rect));
        }
        out.push((String::from("pointer"), layout.check));
        out
    }

    /// Draw the contents above the buttons.
    pub fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        let pad = ctx.px(PAD) as i32;
        // Header: the tile, what is asked and by whom.
        let tile = Rect::new(area.x + pad, area.y + pad, ctx.px(44), ctx.px(44));
        match super::appicon::design("display") {
            Some(image) => image.draw(s, tile),
            None => paint::icon_tile(ctx, s, tile, Icon::Display, Tone::Accent, true),
        }
        let x = tile.right() + ctx.px(16) as i32;
        let width = (area.right() - pad - x).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Heading, x, tile.y, width, "Что показать?", p.ink);
        let who = if self.title.is_empty() { String::from("Программа") } else { format!("«{}»", self.title) };
        let text = format!("{who} просит показать ваш экран. Выберите экран или окно — его увидят те, кому вы его показываете.");
        let mut y = tile.y + line(ctx, Role::Heading) + ctx.px(4) as i32;
        paragraph(ctx, s, Role::Caption, x, &mut y, width, &text, p.ink3);

        let layout = self.layout(area, ctx);
        for &(y, label) in &layout.labels {
            paint::caps(ctx, s, area.x + pad, y + ctx.px(4) as i32, label);
        }
        for &(index, rect) in &layout.cards {
            self.draw_card(s, ctx, &self.sources[index], rect, index == self.selected);
        }
        // More window rows than fit: a thin scroll mark at the right.
        let windows = self.sources.len() - self.monitors;
        let rows = windows.div_ceil(layout.cols) as u32;
        let shown = layout.window_box.h / ctx.px(CARD_H + GAP).max(1);
        if rows > shown && shown > 0 {
            let track = Rect::new(area.right() - pad / 2 - ctx.px(2) as i32, layout.window_box.y, ctx.px(4), layout.window_box.h.saturating_sub(ctx.px(GAP)));
            draw::rounded(s, track, ctx.px(2), p.line3.color, 120);
            let thumb_h = (track.h * shown / rows).max(ctx.px(16));
            let thumb_y = track.y + ((track.h - thumb_h) * self.scroll / (rows - shown).max(1)) as i32;
            draw::rounded(s, Rect::new(track.x, thumb_y, track.w, thumb_h), ctx.px(2), p.ink4, 255);
        }
        // The pointer switch, left of the buttons.
        let check = layout.check;
        let side = ctx.px(14);
        let bx = Rect::new(check.x, check.y + (check.h as i32 - side as i32) / 2, side, side);
        if self.pointer {
            draw::rounded(s, bx, ctx.px(3), p.acc, 255);
            glyphicon::draw(s, Icon::Check, bx.x, bx.y, bx.w, p.acc_ink, 255);
        } else {
            draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
        }
        let label_x = bx.right() + ctx.px(10) as i32;
        let label_w = (check.right() - label_x).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Body, label_x, paint::baseline(ctx, Role::Body, check), label_w, "Показывать указатель мыши", p.ink2);
    }

    fn draw_card(&self, s: &mut Surface, ctx: Ctx, source: &Source, rect: Rect, selected: bool) {
        let p = ctx.palette;
        let r = ctx.px(8);
        if selected {
            draw::rounded(s, rect, r, p.acc, 50);
            draw::rounded_stroke(s, rect, r, p.acc, 255);
        } else {
            draw::rounded_stroke(s, rect, r, p.line3.color, p.line3.alpha);
        }
        let shot = Rect::new(rect.x + ctx.px(8) as i32, rect.y + ctx.px(8) as i32, ctx.px(SHOT_W), ctx.px(SHOT_H));
        s.fill(shot, p.sunk.over(mini_ui::theme::window_bg()));
        if let Some(thumb) = &source.thumb {
            let at = (shot.x + (shot.w as i32 - thumb.width() as i32) / 2, shot.y + (shot.h as i32 - thumb.height() as i32) / 2);
            s.blit_from(thumb, at, thumb.bounds());
        }
        let cap = Rect::new(rect.x + ctx.px(8) as i32, shot.bottom() + ctx.px(6) as i32, rect.w.saturating_sub(ctx.px(16)), ctx.px(CAPTION_H));
        let mut text_x = cap.x;
        if let Some(art) = &source.art {
            let icon = Rect::new(cap.x, cap.y + (cap.h as i32 - ctx.px(16) as i32) / 2, ctx.px(16), ctx.px(16));
            art.draw(ctx, s, icon);
            text_x = icon.right() + ctx.px(6) as i32;
        }
        let room = (cap.right() - text_x).max(0) as u32;
        let base = paint::baseline(ctx, Role::Body, cap);
        let used = paint::text_clipped(ctx, s, Role::Body, text_x, base, room, &source.name, p.ink);
        if !source.note.is_empty() && used + ctx.px(8) < room {
            paint::text_clipped(ctx, s, Role::Caption, text_x + (used + ctx.px(6)) as i32, base + ctx.px(1) as i32, room - used - ctx.px(6), &source.note, p.ink4);
        }
    }
}

struct Layout {
    cards: Vec<(usize, Rect)>,
    labels: Vec<(i32, &'static str)>,
    check: Rect,
    window_box: Rect,
    cols: usize,
}

fn line(ctx: Ctx, role: Role) -> i32 {
    i32::from(ctx.face(role).line)
}

/// A paragraph wrapped by words; `y` moves down by its height.
#[allow(clippy::too_many_arguments)]
fn paragraph(ctx: Ctx, s: &mut Surface, role: Role, x: i32, y: &mut i32, width: u32, text: &str, color: Color) {
    let face = ctx.face(role);
    let mut current = String::new();
    for word in text.split(' ') {
        let candidate = if current.is_empty() { String::from(word) } else { format!("{current} {word}") };
        if face.width(&candidate) > width && !current.is_empty() {
            paint::text(ctx, s, role, x, *y, &current, color);
            *y += line(ctx, role);
            current = String::from(word);
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        paint::text(ctx, s, role, x, *y, &current, color);
        *y += line(ctx, role);
    }
}

/// Fit `w`x`h` into the miniature box keeping the proportions.
fn fit(w: u32, h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    let (w, h) = (w.max(1), h.max(1));
    if u64::from(w) * u64::from(box_h) > u64::from(h) * u64::from(box_w) {
        (box_w, ((u64::from(h) * u64::from(box_w) / u64::from(w)) as u32).max(1))
    } else {
        (((u64::from(w) * u64::from(box_h) / u64::from(h)) as u32).max(1), box_h)
    }
}

/// Even sides (video encoders want them) inside the desktop.
fn even_inside(rect: Rect, union: Rect) -> Rect {
    let r = rect.intersect(&union);
    Rect::new(r.x, r.y, r.w & !1, r.h & !1)
}

/// Build the window and put it on the desktop.
fn show(desktop: &mut Compositor, id: u32, types: u32, pointer: bool, title: &str) {
    let started = crate::time::uptime_ns();
    let scale = desktop.scale();
    let ctx = Ctx::scaled(scale);
    let (box_w, box_h) = (ctx.px(SHOT_W), ctx.px(SHOT_H));
    let outputs: Vec<Rect> = desktop.output_rects().to_vec();
    let union = outputs.iter().fold(Rect::EMPTY, |acc, r| acc.union(r));
    let primary = desktop.primary_rect();
    let mut sources = Vec::new();
    if types & TYPE_MONITOR != 0 {
        for (index, rect) in outputs.iter().enumerate() {
            let (tw, th) = fit(rect.w, rect.h, box_w, box_h);
            let main = *rect == primary;
            sources.push(Source {
                kind: TYPE_MONITOR,
                rect: even_inside(*rect, union),
                monitor: index as u32 + 1,
                name: format!("Экран {}", index + 1),
                note: if main { format!("{}×{}, основной", rect.w, rect.h) } else { format!("{}×{}", rect.w, rect.h) },
                thumb: desktop.snapshot(*rect, None, tw, th),
                art: None,
            });
        }
    }
    let monitors = sources.len();
    if types & TYPE_WINDOW != 0 {
        let mut index = 0;
        let mut apps = Vec::new();
        while let Some(app) = desktop.app_at(index) {
            apps.push(app);
            index += 1;
        }
        for app in apps.into_iter().rev() {
            if sources.len() - monitors >= MAX_WINDOWS || app == App::SharePick || desktop.is_minimized(app) {
                continue;
            }
            let (Some(rect), Some(name)) = (desktop.rect_of(app), desktop.caption_of(app)) else { continue };
            let place = even_inside(rect, union);
            if place.w < 16 || place.h < 16 {
                continue;
            }
            let (tw, th) = fit(rect.w, rect.h, box_w, box_h);
            let thumb = desktop.snapshot(rect, Some(app), tw, th);
            let art = desktop.find(app).map(|window| window.icon_art());
            let centre = (place.x + place.w as i32 / 2, place.y + place.h as i32 / 2);
            let monitor = outputs.iter().position(|r| r.contains(centre.0, centre.1)).map_or(1, |i| i as u32 + 1);
            sources.push(Source { kind: TYPE_WINDOW, rect: place, monitor, name, note: String::new(), thumb, art });
        }
    }
    let windows = sources.len() - monitors;
    // The primary monitor (or the first window) is chosen at first.
    let selected = sources.iter().position(|source| source.kind == TYPE_MONITOR && source.rect == even_inside(primary, union)).unwrap_or(0);
    let pick = SharePick { title: String::from(title), sources, monitors, selected, pointer, scroll: 0 };
    let work = desktop.work_at(None);
    let title_h = Window::title_height(scale);
    let w = scale.px(WIN_W).min(work.w);
    let h = (scale.px(window_height(monitors, windows)) + title_h).min(work.h);
    let rect = Rect::new(work.x + (work.w as i32 - w as i32) / 2, work.y + (work.h as i32 - h as i32) / 2, w, h);
    let Some(window) = Window::dialog(App::SharePick, rect, scale, super::dialog::Dialog::SharePick(pick)) else {
        crate::kprintln!("  share       : no memory for the picker");
        set_phase(id, Phase::Cancelled);
        return;
    };
    let spots = window.share_spots();
    let buttons = window.dialog_buttons();
    desktop.push(window);
    set_phase(id, Phase::Shown);
    crate::kprintln!(
        "  share       : picker for '{title}' shown with {monitors} monitor(s), {windows} window(s), types {types}, pointer {} in {} us",
        if pointer { "on" } else { "off" },
        crate::time::uptime_ns().wrapping_sub(started) / 1000
    );
    for (name, r) in spots.iter().chain(buttons.iter()) {
        crate::kprintln!("  share spot {name} at {},{} {}x{}", r.x, r.y, r.w, r.h);
    }
}
