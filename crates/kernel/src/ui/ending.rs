//! «Завершение работы» / «Перезагрузка» screen (24.11): black on every
//! monitor, white words and a running circle of dots, like Windows (clip of
//! Roman's, with a black background instead of blue).
//!
//! The screen is shown while the graceful shutdown (`power::graceful`) asks
//! programs to close and waits for them; it closes everything, the pointer
//! too, and takes no input. The spinner is a small picture redrawn on every
//! tick of the shutdown task and copied into the middle of each monitor.
//!
//! No floating point in the kernel: the circle is a table of 64 points
//! (cos, sin × 1024), the easing — integer smoothstep.

use alloc::string::String;

use mini_ui::paint::{self, Ctx};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

/// The circle: 64 points from the top clockwise, ×1024.
const CIRCLE: [(i32, i32); 64] = [
    (0, -1024), (100, -1019), (200, -1004), (297, -980), (392, -946), (483, -903), (569, -851), (650, -792), (724, -724), (792, -650), (851, -569),
    (903, -483), (946, -392), (980, -297), (1004, -200), (1019, -100), (1024, 0), (1019, 100), (1004, 200), (980, 297), (946, 392), (903, 483),
    (851, 569), (792, 650), (724, 724), (650, 792), (569, 851), (483, 903), (392, 946), (297, 980), (200, 1004), (100, 1019), (0, 1024),
    (-100, 1019), (-200, 1004), (-297, 980), (-392, 946), (-483, 903), (-569, 851), (-650, 792), (-724, 724), (-792, 650), (-851, 569),
    (-903, 483), (-946, 392), (-980, 297), (-1004, 200), (-1019, 100), (-1024, 0), (-1019, -100), (-1004, -200), (-980, -297), (-946, -392),
    (-903, -483), (-851, -569), (-792, -650), (-724, -724), (-650, -792), (-569, -851), (-483, -903), (-392, -946), (-297, -980), (-200, -1004),
    (-100, -1019),
];

/// One turn of a dot, ms, and the lag between dots.
const PERIOD_MS: u64 = 1800;
const LAG_MS: u64 = 110;
const DOTS: u64 = 5;

/// Picture size, points at scale 1: the circle above, the words below.
const ART_W: u32 = 420;
const ART_H: u32 = 150;
const RING: u32 = 24;
const DOT: u32 = 7;

pub struct Screen {
    restart: bool,
    started: u64,
    /// What is being waited for, under the words («Закрываются программы: 2»).
    note: String,
    art: Option<Surface>,
    scale: mini_ui::Scale,
}

impl Screen {
    #[must_use]
    pub fn new(restart: bool, scale: mini_ui::Scale) -> Self {
        let mut screen = Self { restart, started: crate::time::uptime_ms(), note: String::new(), art: Surface::new(ART_W * scale, ART_H * scale, Color::rgb(0, 0, 0)), scale };
        screen.redraw();
        screen
    }

    /// The words of the screen.
    #[must_use]
    pub const fn title(&self) -> &'static str {
        if self.restart { "Перезагрузка" } else { "Завершение работы" }
    }

    /// Next frame of the spinner and, maybe, a new note.
    pub fn tick(&mut self, note: Option<&str>) {
        if let Some(note) = note {
            if note != self.note {
                self.note = String::from(note);
            }
        }
        self.redraw();
    }

    fn redraw(&mut self) {
        let scale = self.scale;
        let now = crate::time::uptime_ms().saturating_sub(self.started);
        let title = self.title();
        let note = self.note.clone();
        let Some(art) = self.art.as_mut() else { return };
        let black = Color::rgb(0, 0, 0);
        let white = Color::rgb(0xFF, 0xFF, 0xFF);
        let bounds = art.bounds();
        art.fill(bounds, black);
        let ctx = Ctx::scaled(scale).on(black);
        let px = |value: u32| value * scale;
        let (cx, cy) = ((bounds.w / 2) as i32, px(RING + DOT + 4) as i32);
        let radius = px(RING) as i32;
        let side = px(DOT);
        for dot in 0..DOTS {
            let Some(t) = (now + PERIOD_MS * 4).checked_sub(dot * LAG_MS) else { continue };
            // Smoothstep over one turn: dots slow at the top, rush round the
            // bottom and bunch up — the Windows rhythm.
            let u = (t % PERIOD_MS) * 1024 / PERIOD_MS;
            let eased = u * u * (3 * 1024 - 2 * u) / (1024 * 1024);
            let (x, y) = CIRCLE[(eased * 64 / 1024) as usize % 64];
            let (px, py) = (cx + x * radius / 1024 - side as i32 / 2, cy + y * radius / 1024 - side as i32 / 2);
            draw::rounded(art, Rect::new(px, py, side, side), side / 2, white, 255);
        }
        let words = Rect::new(0, px(RING * 2 + DOT * 2 + 18) as i32, bounds.w, px(36));
        paint::text_centered(ctx, art, Role::Heading, words, title, white);
        if !note.is_empty() {
            let line = Rect::new(0, words.bottom(), bounds.w, px(26));
            paint::text_centered(ctx, art, Role::Body, line, &note, Color::rgb(0xB0, 0xB0, 0xB0));
        }
    }

    /// Where the picture stands on the monitor `output`.
    fn art_at(&self, output: Rect) -> Rect {
        let (w, h) = (ART_W * self.scale, ART_H * self.scale);
        Rect::new(output.x + (output.w as i32 - w as i32) / 2, output.y + (output.h as i32 - h as i32) / 2, w, h)
    }

    /// The pictures on all monitors — what changes from frame to frame.
    pub fn damage(&self, outputs: &[Rect]) -> impl Iterator<Item = Rect> + '_ {
        outputs.iter().map(|output| self.art_at(*output)).collect::<alloc::vec::Vec<_>>().into_iter()
    }

    /// Compose the band `band` (desktop coordinates) into `back` at `dy`.
    pub fn compose(&self, back: &mut Surface, band: Rect, dy: i32, outputs: &[Rect]) {
        back.fill(Rect::new(band.x, band.y + dy, band.w, band.h), Color::rgb(0, 0, 0));
        let Some(art) = self.art.as_ref() else { return };
        for output in outputs {
            let at = self.art_at(*output);
            let part = band.intersect(&at);
            if !part.is_empty() {
                back.blit_from(art, (part.x, part.y + dy), part.translate(-at.x, -at.y));
            }
        }
    }
}
