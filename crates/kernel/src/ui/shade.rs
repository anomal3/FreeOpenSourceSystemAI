// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Шторка телефона: то, что открывается сверху.
//!
//! # Что в ней есть
//!
//! Состояние машины и переключатели к нему: связь, отладка, яркость, звук,
//! уведомления. На настольной машине то же самое живёт в трее и в «Параметрах»,
//! и шторки там нет вовсе — она открывается жестом, которого мышью не сделать.
//!
//! # Что нажимается (03.11)
//!
//! Роман на телефоне: «шторка открывается без анимации и ни на что не
//! реагирует». Теперь каждая плитка отвечает на нажатие — делом или честным
//! ответом: Wi-Fi открывает свою страницу «Параметров» (там видно, что с
//! блоком связи), «Не беспокоить» переключается, «Заблокировать» ставит
//! экран блокировки, Bluetooth говорит на плитке, что драйвера нет. Полоса
//! звука тянется пальцем; яркость — когда появится регулятор подсветки.
//! Шторка выезжает сверху и уезжает обратно ([`OPEN_NS`]).
//!
//! # Как она устроена внутри
//!
//! Тем же способом, что меню запуска и меню стола: своя поверхность, своё место
//! на экране, свой прямоугольник изменений. Композитор кладёт её поверх окон,
//! срезает углы и рисует тень — шторка об этом не знает.

use alloc::format;

use mini_ui::paint::{self, Ctx};
use mini_ui::glyphicon::{self, Icon};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

/// Плитка состояния: значок, подпись, пояснение под ней.
struct Tile {
    icon: Icon,
    label: &'static str,
    note: alloc::string::String,
    /// Включено ли то, о чём плитка: включённая залита акцентом.
    on: bool,
}

/// Что сделать по нажатию в шторку — решает стол, шторка только называет.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Плитка Wi-Fi: страница «Wi-Fi» в «Параметрах».
    Wifi,
    /// Плитка «Заблокировать».
    Lock,
    /// Полоса звука, новая громкость в процентах.
    Volume(u8),
    /// Нажатие в ручку внизу — закрыть шторку.
    Close,
}

/// «Не беспокоить» — пока только состояние на плитке (уведомлений, которые
/// оно глушило бы, у системы ещё нет).
static DO_NOT_DISTURB: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Сколько выезжает шторка.
pub const OPEN_NS: u64 = 220_000_000;

/// Плитки в том порядке, в каком они стоят в макете.
fn tiles() -> [Tile; 4] {
    let quiet = DO_NOT_DISTURB.load(core::sync::atomic::Ordering::Relaxed);
    [
        Tile {
            icon: Icon::Network,
            label: "Wi-Fi",
            note: crate::wifi::state().note().into(),
            on: crate::wifi::state() == crate::wifi::State::FirmwareUp,
        },
        Tile { icon: Icon::Devices, label: "Bluetooth", note: "драйвера нет".into(), on: false },
        Tile { icon: Icon::Clock, label: "Не беспокоить", note: if quiet { "включено" } else { "выключено" }.into(), on: quiet },
        Tile { icon: Icon::Shield, label: "Заблокировать", note: "экран и вход".into(), on: false },
    ]
}

/// Сколько плиток в ряду.
const COLUMNS: u32 = 2;

/// Высота плитки.
const TILE_H: u32 = 64;

/// Высота строки с полосой (яркость, звук).
const SLIDER_H: u32 = 44;

/// Шторка: слой поверх стола.
pub struct Shade {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    open: bool,
    damage: Rect,
    /// Идёт выезд или уход: когда начался и куда (`true` — открывается).
    motion: Option<(u64, bool)>,
    /// Где шторка была нарисована прошлым кадром — его надо стереть.
    shown: Rect,
    /// Места плиток и полос внутри поверхности — для нажатий.
    tile_rects: [Rect; 4],
    volume_rect: Rect,
    handle_rect: Rect,
}

impl Shade {
    /// Собрать шторку под этот экран.
    ///
    /// `None` — не хватило памяти под поверхность. Это не отказ стола: шторки
    /// просто не будет, а всё остальное работает.
    #[must_use]
    pub fn new(screen_w: u32, work_bottom: i32, scale: u32) -> Option<Self> {
        if !theme::is_mobile() || work_bottom <= 0 {
            return None;
        }
        let scale = scale.max(1);
        let ctx = Ctx::scaled(scale);
        // Макет (экран 02): от верха экрана, с полями по 8, и строка времени —
        // своя, внутри шторки: она закрывает строку состояния, а не лежит под ней.
        let side = ctx.px(8);
        let width = screen_w.saturating_sub(side * 2).max(1);
        let height = ctx
            .px(14 + 30 + 16 + TILE_H * 2 + 9 + 16 + SLIDER_H * 2 + 10 + 16 + 10 + 16 + 64 + 16 + 4 + 18)
            .min(work_bottom.max(1) as u32);
        let surface = Surface::new(width, height, bg())?;
        Some(Self {
            surface,
            rect: Rect::new(side as i32, side as i32, width, height),
            scale,
            open: false,
            damage: Rect::EMPTY,
            motion: None,
            shown: Rect::EMPTY,
            tile_rects: [Rect::EMPTY; 4],
            volume_rect: Rect::EMPTY,
            handle_rect: Rect::EMPTY,
        })
    }

    /// Открыта ли (по смыслу: уезжающая — уже закрыта).
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Видна ли на экране: открыта или ещё уезжает.
    #[must_use]
    pub const fn is_visible(&self) -> bool {
        self.open || self.motion.is_some()
    }

    /// Идёт ли выезд или уход.
    #[must_use]
    pub const fn is_moving(&self) -> bool {
        self.motion.is_some()
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Открыть или закрыть. Возвращает новое состояние.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        if self.open {
            self.redraw();
        }
        // Выезд продолжается с того места, где был: шторку, закрытую на
        // полпути, не дёргает обратно на край.
        let now = crate::time::uptime_ns();
        let start = match self.motion {
            Some((started, _)) => now.saturating_sub(OPEN_NS.saturating_sub(now.saturating_sub(started).min(OPEN_NS))),
            None => now,
        };
        self.motion = Some((start, self.open));
        self.open
    }

    pub fn close(&mut self) {
        if self.open {
            self.toggle();
        }
    }

    /// Где шторка сейчас, с учётом выезда, и закончился ли он.
    ///
    /// Сглаживание — «плавно в начале и в конце» (smoothstep), как у полёта
    /// окон; путь — от-за верхнего края до своего места.
    pub fn advance(&mut self, now_ns: u64) -> (Rect, Rect) {
        let was = self.shown;
        let travel = self.rect.bottom().max(1);
        let rect = match self.motion {
            None if self.open => self.rect,
            None => Rect::EMPTY,
            Some((started, opening)) => {
                let t = (now_ns.saturating_sub(started).min(OPEN_NS) * 1024 / OPEN_NS) as i64;
                let eased = t * t * (3 * 1024 - 2 * t) / (1024 * 1024);
                let shown = if opening { eased } else { 1024 - eased };
                let dy = -(i64::from(travel) * (1024 - shown) / 1024) as i32;
                if t >= 1024 {
                    self.motion = None;
                    if opening { self.rect } else { Rect::EMPTY }
                } else {
                    self.rect.translate(0, dy)
                }
            }
        };
        self.shown = rect;
        (was, rect)
    }

    /// Где шторка нарисована сейчас.
    #[must_use]
    pub const fn shown(&self) -> Rect {
        self.shown
    }

    /// Попадает ли точка экрана в шторку.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.open && self.rect.contains(x, y)
    }

    /// Нажатие в открытую шторку: что сделать. Переключатели, которые
    /// шторка ведёт сама («Не беспокоить»), переключаются здесь же.
    pub fn press(&mut self, x: i32, y: i32) -> Option<Action> {
        if !self.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        if self.handle_rect.contains(x, y) {
            return Some(Action::Close);
        }
        if self.volume_rect.contains(x, y) {
            let into = (x - self.volume_rect.x).clamp(0, self.volume_rect.w as i32) as u32;
            let percent = (into * 100 + self.volume_rect.w / 2) / self.volume_rect.w.max(1);
            return Some(Action::Volume(percent.min(100) as u8));
        }
        let hit = self.tile_rects.iter().position(|rect| rect.contains(x, y))?;
        match hit {
            0 => Some(Action::Wifi),
            2 => {
                DO_NOT_DISTURB.fetch_xor(true, core::sync::atomic::Ordering::Relaxed);
                self.redraw();
                None
            }
            3 => Some(Action::Lock),
            // Bluetooth: драйвера нет, и это написано на самой плитке.
            _ => None,
        }
    }

    /// Громкость под пальцем, который тянут по полосе звука (`x` — экран).
    #[must_use]
    pub fn volume_at(&self, x: i32) -> Option<u8> {
        if !self.open || self.volume_rect.w == 0 {
            return None;
        }
        let into = (x - self.rect.x - self.volume_rect.x).clamp(0, self.volume_rect.w as i32) as u32;
        Some(((into * 100 + self.volume_rect.w / 2) / self.volume_rect.w).min(100) as u8)
    }

    /// Перерисовать содержимое снаружи (громкость сменилась).
    pub fn refresh(&mut self) {
        if self.open {
            self.redraw();
        }
    }

    /// Забрать накопленные изменения.
    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Перерисовать содержимое.
    fn redraw(&mut self) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(38);

        // Заливка во всю поверхность без скругления: углы срезает композитор,
        // смешивая их с тем, что под ними. Скругление здесь означало бы, что
        // углы срезаются дважды.
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);

        let left = card.x + ctx.px(16) as i32;
        let room = card.w.saturating_sub(ctx.px(32));
        let mut y = card.y + ctx.px(14) as i32;

        // Строка времени. Даты у телефона нет — часов реального времени тоже,
        // — поэтому рядом со временем сказано, что это время работы.
        let head = Rect::new(left + ctx.px(6) as i32, y, room.saturating_sub(ctx.px(12)), ctx.px(30));
        let time = super::clock_or_uptime();
        let time_w = paint::text(ctx, &mut self.surface, Role::Mono, head.x, paint::baseline(ctx, Role::Mono, head), &time, p.ink);
        let note = if crate::time::clock_text().is_some() { "" } else { "время работы" };
        paint::text(ctx, &mut self.surface, Role::Label, head.x + (time_w + ctx.px(8)) as i32, paint::baseline(ctx, Role::Label, head), note, p.ink4);
        y += ctx.px(30 + 16) as i32;

        // Плитки: два ряда по две.
        let gap = ctx.px(9);
        let tile_w = (room.saturating_sub(gap * (COLUMNS - 1))) / COLUMNS;
        let tile_h = ctx.px(TILE_H);
        for (index, tile) in tiles().iter().enumerate() {
            let column = index as u32 % COLUMNS;
            let row = index as u32 / COLUMNS;
            let rect = Rect::new(
                left + (column * (tile_w + gap)) as i32,
                y + (row * (tile_h + gap)) as i32,
                tile_w,
                tile_h,
            );
            self.draw_tile(ctx, rect, tile);
            self.tile_rects[index] = rect;
        }
        y += (tile_h * 2 + gap) as i32 + ctx.px(16) as i32;

        // Полосы: яркость и звук. Звук — настоящий (`crate::sound`) и
        // тянется пальцем; яркость — когда будет регулятор подсветки.
        let volume = u32::from(crate::sound::volume());
        for (label, icon, percent, accent, live) in
            [("Яркость", Icon::Sun, 100u32, true, false), ("Звук", Icon::Info, volume, false, true)]
        {
            let rect = Rect::new(left, y, room, ctx.px(SLIDER_H));
            self.draw_slider(ctx, rect, label, icon, percent, accent);
            if live {
                self.volume_rect = rect;
            }
            y += ctx.px(SLIDER_H + 10) as i32;
        }
        y += ctx.px(6) as i32;

        // Уведомлений у системы пока нет вовсе, и это сказано прямо. Пустой
        // раздел без строки читался бы как не дорисованный экран.
        paint::caps(ctx, &mut self.surface, left + ctx.px(6) as i32, y, "УВЕДОМЛЕНИЯ · 0");
        y += ctx.px(10 + 16) as i32;
        let empty = Rect::new(left, y, room, ctx.px(64));
        draw::rounded(&mut self.surface, empty, ctx.px(20), p.card.color, p.card.alpha);
        draw::rounded_stroke(&mut self.surface, empty, ctx.px(20), p.line2.color, p.line2.alpha);
        paint::text_clipped(ctx, &mut self.surface, Role::Body, empty.x + ctx.px(14) as i32, paint::baseline(ctx, Role::Body, empty), empty.w, "Уведомлений пока нет", p.ink3);
        y += ctx.px(64 + 16) as i32;

        let handle = Rect::new(card.w as i32 / 2 - ctx.px(22) as i32, y, ctx.px(44), ctx.px(4));
        draw::rounded(&mut self.surface, handle, handle.h / 2, p.ink6, 180);
        // Ручку нажимают пальцем, а не попадают в четыре точки: поле — во всю
        // ширину и до низа шторки.
        self.handle_rect = Rect::new(0, y - ctx.px(14) as i32, card.w, (card.bottom() - y + ctx.px(14) as i32).max(0) as u32);

        self.damage = card;
    }

    /// Одна плитка состояния: значок слева, имя и состояние справа. Включённая
    /// залита акцентом.
    fn draw_tile(&mut self, ctx: Ctx, rect: Rect, tile: &Tile) {
        let p = ctx.palette;
        let round = ctx.px(20);
        let ink = if tile.on {
            draw::rounded_gradient(&mut self.surface, rect, round, p.acc, p.acc2, 255);
            draw::rounded_stroke(&mut self.surface, rect, round, p.accline.color, p.accline.alpha);
            Color::rgb(0xFF, 0xFF, 0xFF)
        } else {
            draw::rounded(&mut self.surface, rect, round, p.btn.color, p.btn.alpha);
            draw::rounded_stroke(&mut self.surface, rect, round, p.btnline.color, p.btnline.alpha);
            p.ink2
        };
        let glyph = ctx.px(20);
        let icon_x = rect.x + ctx.px(14) as i32;
        glyphicon::draw(&mut self.surface, tile.icon, icon_x, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, ink, 255);
        let text_x = icon_x + (glyph + ctx.px(11)) as i32;
        let room = (rect.right() - ctx.px(10) as i32 - text_x).max(0) as u32;
        let title = ctx.face(Role::Title);
        let small = ctx.face(Role::MonoSmall);
        let top = rect.y + (rect.h as i32 - i32::from(title.line) - i32::from(small.line)) / 2;
        paint::text_clipped(ctx, &mut self.surface, Role::Title, text_x, top, room, tile.label, ink);
        let note_ink = if tile.on { ink } else { p.ink4 };
        paint::text_clipped(ctx, &mut self.surface, Role::MonoSmall, text_x, top + i32::from(title.line), room, &tile.note, note_ink);
    }

    /// Полоса: кнопка во всю ширину, внутри — заливка на долю и подпись.
    fn draw_slider(&mut self, ctx: Ctx, rect: Rect, label: &str, icon: Icon, percent: u32, accent: bool) {
        let p = ctx.palette;
        let round = ctx.px(16);
        draw::rounded(&mut self.surface, rect, round, p.btn.color, p.btn.alpha);
        let filled = rect.w * percent.min(100) / 100;
        if filled > 0 {
            let part = Rect::new(rect.x, rect.y, filled, rect.h);
            if accent {
                draw::horizontal_gradient(&mut self.surface, part, round, p.acc2, p.acc, 140);
            } else {
                draw::rounded(&mut self.surface, part, round, p.ink6, 115);
            }
        }
        draw::rounded_stroke(&mut self.surface, rect, round, p.btnline.color, p.btnline.alpha);
        let glyph = ctx.px(18);
        let icon_x = rect.x + ctx.px(16) as i32;
        glyphicon::draw(&mut self.surface, icon, icon_x, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, p.ink, 255);
        let text = format!("{label} {percent} %");
        let text_x = icon_x + (glyph + ctx.px(10)) as i32;
        paint::text_clipped(ctx, &mut self.surface, Role::Label, text_x, paint::baseline(ctx, Role::Label, rect), rect.w, &text, p.ink);
    }
}

/// Подложка шторки — та же, что у прочих плавающих слоёв стола.
fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}
