// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! Лист «Свёрнутые программы» телефона (макет `FreeOS-mobile`, экран 09).
//!
//! # Что это
//!
//! Нажатие на стопку в доке открывает над ней список свёрнутых окон: значок и
//! цвет программы, имя, пояснение, сколько окно лежит свёрнутым, крестик
//! закрытия. Нажатие на строку разворачивает окно, «Развернуть все» — все.
//!
//! # Почему снимок, а не живой список
//!
//! Строки собираются в миг открытия и больше не меняются, а стол закрывает
//! лист при любом изменении окон. Живой список, перестраивающийся под пальцем,
//! означал бы, что строка, в которую целились, уехала за миг до нажатия и
//! нажатие досталось соседней — развернуть или закрыть не то окно.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use super::window::App;

/// Строка листа — то, что о свёрнутом окне известно в миг открытия.
pub struct Row {
    pub app: App,
    pub title: String,
    pub note: String,
    /// Сколько окно лежит свёрнутым, словами: «сейчас», «2 мин».
    pub age: String,
}

/// Во что попало нажатие.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Развернуть это окно.
    Restore(App),
    /// Закрыть это окно.
    Close(App),
    /// Развернуть все.
    RestoreAll,
    /// Мимо строк, но внутри листа — ничего не делать.
    Inside,
}

/// Числа макета, в его точках.
const PAD: u32 = 16;
const HEADER_H: u32 = 20;
const GAP: u32 = 14;
const ROW_H: u32 = 60;
const ROW_GAP: u32 = 8;
const NOTE_H: u32 = 56;
const HANDLE_H: u32 = 4;
/// Сколько лист отстоит от низа экрана: док и поле под ним.
const ABOVE_BOTTOM: u32 = 100;

/// Что сказано внизу листа — дословно из макета: это ответ на вопрос «а что
/// с программой, пока окно свёрнуто», который человек задаёт первым.
const NOTE: &str = "Свёрнутое окно не перерисовывается: задача остаётся в памяти, но композитор её не опрашивает.";

/// То же место в переключателе программ (жест «вверх и задержать», 04.11):
/// там человек пришёл выбирать, и ему нужно знать, как.
const SWITCH_NOTE: &str = "Нажмите на карточку, чтобы открыть программу; смахните её вверх, чтобы закрыть.";

/// Числа карточек переключателя (05.11), в точках макета.
const CARD_HEAD: u32 = 34;
const CARD_PAD: u32 = 8;
const CARD_GAP: u32 = 12;
/// Ниже этой высоты снимка карточка уже ничего не показывает: лишние окна
/// в переключатель не попадают (самые давние), счётчик говорит, сколько всех.
const SHOT_MIN_H: u32 = 70;
const FOOT_H: u32 = 40;

/// Карточка переключателя программ: окно, его имя и значок, и снимок —
/// поверхность самого окна, которую переключатель уменьшит.
pub struct Card<'a> {
    pub app: App,
    pub title: String,
    pub art: super::appicon::Art,
    pub shot: &'a Surface,
}

pub struct Sheet {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    rows: Vec<(Rect, Rect, App)>,
    restore_all: Rect,
    /// Переключатель программ, а не лист свёрнутых: другие заголовок и
    /// пояснение, строки те же.
    switcher: bool,
}

fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

impl Sheet {
    /// Собрать лист над доком. `None` — строк нет или не хватило памяти.
    /// `switcher` — это переключатель программ (жест полоски, 04.11).
    #[must_use]
    pub fn open(screen_w: u32, screen_h: u32, scale: u32, rows: &[Row], switcher: bool) -> Option<Self> {
        if rows.is_empty() {
            return None;
        }
        let ctx = Ctx::scaled(scale);
        let inset = ctx.px(theme::M_INSET);
        let width = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let count = rows.len() as u32;
        let height = ctx.px(
            PAD * 2 + HEADER_H + GAP + ROW_H * count + ROW_GAP * (count - 1) + GAP + NOTE_H + GAP + HANDLE_H,
        );
        let bottom = screen_h.checked_sub(ctx.px(ABOVE_BOTTOM))?;
        let top = bottom.checked_sub(height)?;
        let surface = Surface::new(width, height, bg())?;
        let mut sheet = Self {
            surface,
            rect: Rect::new(inset as i32, top as i32, width, height),
            scale,
            rows: Vec::new(),
            restore_all: Rect::EMPTY,
            switcher,
        };
        sheet.draw(rows);
        Some(sheet)
    }

    /// Собрать переключатель программ (05.11, как «недавние» у Android и
    /// iPhone): карточки с уменьшенными снимками самих окон, по две в ряд, над
    /// доком, не выше строки состояния. Первая — окно, ушедшее с экрана
    /// последним.
    /// Нажатие на карточку открывает программу, смахивание вверх закрывает
    /// (разбирает стол, `ui::card_end`), крестик в заголовке карточки — тоже.
    ///
    /// Снимки уменьшаются здесь, один раз: лист — снимок мига открытия (см.
    /// начало файла), и пересчитывать миниатюры в каждом кадре незачем.
    /// `None` — карточек нет, экран слишком мал или не хватило памяти.
    #[must_use]
    pub fn cards(screen_w: u32, screen_h: u32, scale: u32, cards: &[Card]) -> Option<Self> {
        if cards.is_empty() {
            return None;
        }
        let ctx = Ctx::scaled(scale);
        let inset = ctx.px(theme::M_INSET);
        let width = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let top = ctx.px(theme::M_STATUS_H) + ctx.px(4);
        let bottom = screen_h.checked_sub(ctx.px(ABOVE_BOTTOM))?;
        let fixed = ctx.px(PAD * 2 + HEADER_H + GAP + FOOT_H);
        let room = bottom.checked_sub(top).filter(|h| *h > fixed)?;
        // Лист по карточкам, над доком, как прочие листы: две карточки не
        // оставляют под собой полэкрана пустого стекла.
        let grid = Grid::new(ctx, width, room - fixed, (screen_w, screen_h), cards.len());
        let height = (fixed + grid.height()).min(room);
        let surface = Surface::new(width, height, bg())?;
        let mut sheet = Self {
            surface,
            rect: Rect::new(inset as i32, (bottom - height) as i32, width, height),
            scale,
            rows: Vec::new(),
            restore_all: Rect::EMPTY,
            switcher: true,
        };
        sheet.draw_cards(cards, grid);
        Some(sheet)
    }

    /// Нарисовать карточки переключателя по раскладке `grid`.
    fn draw_cards(&mut self, cards: &[Card], grid: Grid) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let sheet = self.surface.bounds();
        let round = ctx.px(30);
        self.surface.fill(sheet, ctx.under);
        draw::rounded_stroke(&mut self.surface, sheet, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, sheet, round, p.crown.color, p.crown.alpha);

        let pad = ctx.px(PAD);
        let inner_w = sheet.w.saturating_sub(pad * 2);
        // Заголовок и счётчик всех открытых, а не только показанных.
        let header = Rect::new((pad + ctx.px(4)) as i32, pad as i32, inner_w.saturating_sub(ctx.px(8)), ctx.px(HEADER_H));
        let title = "Открытые программы";
        let title_w = paint::text(ctx, &mut self.surface, Role::Title, header.x, paint::baseline(ctx, Role::Title, header), title, p.ink);
        let count = alloc::format!("{}", cards.len());
        let mono = ctx.face(Role::MonoSmall);
        let pill = Rect::new(header.x + (title_w + ctx.px(10)) as i32, header.y, mono.width(&count) + ctx.px(16), ctx.px(20));
        draw::rounded(&mut self.surface, pill, pill.h / 2, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(&mut self.surface, pill, pill.h / 2, p.btnline.color, p.btnline.alpha);
        paint::text(ctx, &mut self.surface, Role::MonoSmall, pill.x + ctx.px(8) as i32, paint::baseline(ctx, Role::MonoSmall, pill), &count, p.ink3);

        let grid_top = pad + ctx.px(HEADER_H + GAP);
        let grid_bottom = sheet.h.saturating_sub(pad + ctx.px(FOOT_H));
        let Grid { shown, card_w, card_h, gap } = grid;
        let (head, cpad) = (ctx.px(CARD_HEAD), ctx.px(CARD_PAD));
        let shot_w = card_w.saturating_sub(cpad * 2).max(1);

        self.rows.clear();
        for (index, card) in cards.iter().take(shown).enumerate() {
            let (col, row) = ((index % 2) as u32, (index / 2) as u32);
            // Одна карточка — посередине, а не у левого края.
            let x = if shown == 1 { pad + (inner_w.saturating_sub(card_w)) / 2 } else { pad + col * (card_w + gap) };
            let y = grid_top + row * (card_h + gap);
            let rect = Rect::new(x as i32, y as i32, card_w, card_h);
            let r = ctx.px(18);
            draw::rounded(&mut self.surface, rect, r, p.card.color, p.card.alpha);
            if index == 0 {
                draw::rounded_stroke(&mut self.surface, rect, r, p.accedge, 255);
            } else {
                draw::rounded_stroke(&mut self.surface, rect, r, p.line2.color, p.line2.alpha);
            }

            // Заголовок карточки: значок, имя, крестик.
            let icon_side = ctx.px(20);
            let icon = Rect::new(rect.x + ctx.px(10) as i32, rect.y + (head as i32 - icon_side as i32) / 2, icon_side, icon_side);
            card.art.draw(ctx, &mut self.surface, icon);
            let side = ctx.px(24);
            let close = Rect::new(rect.right() - (ctx.px(8) + side) as i32, rect.y + (head as i32 - side as i32) / 2, side, side);
            let cr = ctx.px(8);
            draw::rounded(&mut self.surface, close, cr, p.badbg.color, p.badbg.alpha);
            draw::rounded_stroke(&mut self.surface, close, cr, p.badline, 255);
            let cross = ctx.px(10);
            glyphicon::draw(
                &mut self.surface,
                Icon::Close,
                close.x + (close.w as i32 - cross as i32) / 2,
                close.y + (close.h as i32 - cross as i32) / 2,
                cross,
                p.bad_ink,
                255,
            );
            let text_x = icon.right() + ctx.px(8) as i32;
            let room = (close.x - ctx.px(6) as i32 - text_x).max(0) as u32;
            let head_rect = Rect::new(rect.x, rect.y, rect.w, head);
            paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, paint::baseline(ctx, Role::Body, head_rect), room, &card.title, p.ink);

            // Снимок: окно, вписанное в карточку с сохранением пропорций.
            let shot = Rect::new(rect.x + cpad as i32, rect.y + head as i32, shot_w, card_h.saturating_sub(head + cpad).max(1));
            self.surface.fill(shot, p.sunk.over(ctx.under));
            draw_shot(&mut self.surface, card.shot, shot);
            draw::rounded_stroke(&mut self.surface, shot, ctx.px(4), p.line2.color, p.line2.alpha);

            // Попадание в крестик — чуть шире его самого: промах на пару точек
            // иначе открывал бы окно, которое хотели закрыть.
            let close_hit = Rect::new(close.x - ctx.px(6) as i32, rect.y, close.w + ctx.px(14), head);
            self.rows.push((rect, close_hit, card.app));
        }

        // Пояснение внизу.
        let foot = Rect::new(pad as i32, grid_bottom as i32, inner_w, ctx.px(FOOT_H));
        let caption = ctx.face(Role::Caption);
        let lines = wrap_px(SWITCH_NOTE, foot.w, |text| caption.width(text));
        let lines_h = i32::from(caption.line) * lines.len() as i32;
        let mut line_y = foot.y + (foot.h as i32 - lines_h) / 2;
        for line in lines {
            let line_w = caption.width(line);
            let line_x = foot.x + (foot.w as i32 - line_w as i32) / 2;
            paint::text(ctx, &mut self.surface, Role::Caption, line_x, line_y, line, p.ink4);
            line_y += i32::from(caption.line);
        }
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Во что попала точка экрана. `None` — мимо листа.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        if self.restore_all.contains(x, y) {
            return Some(Hit::RestoreAll);
        }
        for (row, close, app) in &self.rows {
            // Крестик проверяется раньше строки: он лежит внутри неё.
            if close.contains(x, y) {
                return Some(Hit::Close(*app));
            }
            if row.contains(x, y) {
                return Some(Hit::Restore(*app));
            }
        }
        Some(Hit::Inside)
    }

    fn draw(&mut self, rows: &[Row]) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(30);
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);

        let pad = ctx.px(PAD) as i32;
        let inner_w = card.w.saturating_sub(ctx.px(PAD) * 2);
        let mut y = pad;

        // Заголовок: имя, счётчик, «Развернуть все».
        let header = Rect::new(pad + ctx.px(4) as i32, y, inner_w.saturating_sub(ctx.px(8)), ctx.px(HEADER_H));
        let title = if self.switcher { "Открытые программы" } else { "Свёрнутые программы" };
        let title_w = paint::text(ctx, &mut self.surface, Role::Title, header.x, paint::baseline(ctx, Role::Title, header), title, p.ink);
        let count = alloc::format!("{}", rows.len());
        let mono = ctx.face(Role::MonoSmall);
        let pill_w = mono.width(&count) + ctx.px(16);
        let pill = Rect::new(header.x + (title_w + ctx.px(10)) as i32, header.y, pill_w, ctx.px(20));
        draw::rounded(&mut self.surface, pill, pill.h / 2, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(&mut self.surface, pill, pill.h / 2, p.btnline.color, p.btnline.alpha);
        paint::text(ctx, &mut self.surface, Role::MonoSmall, pill.x + ctx.px(8) as i32, paint::baseline(ctx, Role::MonoSmall, pill), &count, p.ink3);
        let all = "Развернуть все";
        let all_w = ctx.face(Role::Label).width(all);
        let all_x = header.right() - all_w as i32;
        paint::text(ctx, &mut self.surface, Role::Label, all_x, paint::baseline(ctx, Role::Label, header), all, p.acc_ink);
        // Попадание шире надписи: в неё целятся пальцем.
        self.restore_all = Rect::new(all_x - ctx.px(8) as i32, header.y - ctx.px(8) as i32, all_w + ctx.px(16), header.h + ctx.px(16));
        y += ctx.px(HEADER_H + GAP) as i32;

        // Строки. Первая — окно, свёрнутое последним, — подсвечена: его
        // скорее всего и ищут.
        self.rows.clear();
        for (index, row) in rows.iter().enumerate() {
            let rect = Rect::new(pad, y, inner_w, ctx.px(ROW_H));
            let r = ctx.px(18);
            if index == 0 {
                draw::horizontal_gradient(&mut self.surface, rect, r, ctx.flat(p.sel1), ctx.flat(p.sel2), 255);
                draw::rounded_stroke(&mut self.surface, rect, r, p.accedge, 255);
            } else {
                draw::rounded(&mut self.surface, rect, r, p.card.color, p.card.alpha);
                draw::rounded_stroke(&mut self.surface, rect, r, p.line2.color, p.line2.alpha);
            }

            let tile = Rect::new(rect.x + ctx.px(12) as i32, rect.y + ctx.px(12) as i32, ctx.px(36), ctx.px(36));
            let tr = ctx.px(12);
            let ink = match row.app.tone() {
                Tone::Ok => {
                    draw::rounded_gradient(&mut self.surface, tile, tr, p.ok, p.ok2, 255);
                    Color::rgb(0xFF, 0xFF, 0xFF)
                }
                Tone::Bad => {
                    draw::rounded(&mut self.surface, tile, tr, p.bad, 255);
                    Color::rgb(0xFF, 0xFF, 0xFF)
                }
                _ => {
                    draw::rounded(&mut self.surface, tile, tr, p.btn.color, p.btn.alpha);
                    draw::rounded_stroke(&mut self.surface, tile, tr, p.btnline.color, p.btnline.alpha);
                    p.ink3
                }
            };
            let glyph = ctx.px(15);
            let icon = if row.app == App::Terminal { Icon::Prompt } else { row.app.icon() };
            glyphicon::draw(
                &mut self.surface,
                icon,
                tile.x + (tile.w as i32 - glyph as i32) / 2,
                tile.y + (tile.h as i32 - glyph as i32) / 2,
                glyph,
                ink,
                255,
            );

            // Справа: крестик и возраст.
            let close = Rect::new(rect.right() - ctx.px(12 + 28) as i32, rect.y + ctx.px(16) as i32, ctx.px(28), ctx.px(28));
            let cr = ctx.px(10);
            draw::rounded(&mut self.surface, close, cr, p.badbg.color, p.badbg.alpha);
            draw::rounded_stroke(&mut self.surface, close, cr, p.badline, 255);
            let cross = ctx.px(12);
            glyphicon::draw(
                &mut self.surface,
                Icon::Close,
                close.x + (close.w as i32 - cross as i32) / 2,
                close.y + (close.h as i32 - cross as i32) / 2,
                cross,
                p.bad_ink,
                255,
            );
            let age_w = mono.width(&row.age);
            let age_x = close.x - ctx.px(9) as i32 - age_w as i32;
            paint::text(ctx, &mut self.surface, Role::MonoSmall, age_x, paint::baseline(ctx, Role::MonoSmall, rect), &row.age, p.ink4);

            // Имя и пояснение между плиткой и возрастом.
            let text_x = tile.right() + ctx.px(12) as i32;
            let room = (age_x - ctx.px(9) as i32 - text_x).max(0) as u32;
            let title_face = ctx.face(Role::Title);
            let lines_h = i32::from(title_face.line) + i32::from(mono.line);
            let top = rect.y + (rect.h as i32 - lines_h) / 2;
            paint::text_clipped(ctx, &mut self.surface, Role::Title, text_x, top, room, &row.title, p.ink);
            paint::text_clipped(ctx, &mut self.surface, Role::MonoSmall, text_x, top + i32::from(title_face.line), room, &row.note, p.ink4);

            // Попадание в строку — вся строка, в крестик — чуть шире самого
            // крестика: промах мимо него на пару точек иначе разворачивал бы
            // окно, которое хотели закрыть.
            let close_hit = Rect::new(close.x - ctx.px(6) as i32, rect.y, close.w + ctx.px(12), rect.h);
            self.rows.push((rect, close_hit, row.app));
            y += ctx.px(ROW_H + ROW_GAP) as i32;
        }
        y += ctx.px(GAP - ROW_GAP) as i32;

        // Пояснение.
        let note = Rect::new(pad, y, inner_w, ctx.px(NOTE_H));
        draw::rounded(&mut self.surface, note, ctx.px(16), p.card.color, p.card.alpha);
        draw::rounded_stroke(&mut self.surface, note, ctx.px(16), p.line2.color, p.line2.alpha);
        let area = note.shrink(ctx.px(12));
        let caption = ctx.face(Role::Caption);
        let mut line_y = area.y;
        let text = if self.switcher { SWITCH_NOTE } else { NOTE };
        for line in wrap_px(text, area.w, |text| caption.width(text)) {
            paint::text(ctx, &mut self.surface, Role::Caption, area.x, line_y, line, p.ink4);
            line_y += i32::from(caption.line);
        }
        y += ctx.px(NOTE_H + GAP) as i32;

        // Ручка.
        let handle = Rect::new(card.w as i32 / 2 - ctx.px(22) as i32, y, ctx.px(44), ctx.px(HANDLE_H));
        draw::rounded(&mut self.surface, handle, handle.h / 2, p.ink6, 180);
    }
}

/// Раскладка карточек переключателя: две колонки, рядов сколько влезает.
/// Всё насыщающими действиями — лист бывает и меньше, чем просят числа макета.
#[derive(Clone, Copy)]
struct Grid {
    /// Сколько карточек показано (самые давние окна не влезли).
    shown: usize,
    card_w: u32,
    card_h: u32,
    gap: u32,
}

impl Grid {
    /// `width` — ширина листа, `room` — высота под сетку, `screen` —
    /// пропорции снимка (окна телефона во весь экран), `count` — карточек.
    fn new(ctx: Ctx, width: u32, room: u32, screen: (u32, u32), count: usize) -> Self {
        let inner_w = width.saturating_sub(ctx.px(PAD) * 2);
        let gap = ctx.px(CARD_GAP);
        let card_w = inner_w.saturating_sub(gap) / 2;
        let (head, cpad) = (ctx.px(CARD_HEAD), ctx.px(CARD_PAD));
        let shot_w = card_w.saturating_sub(cpad * 2).max(1);
        let natural_shot = (u64::from(shot_w) * u64::from(screen.1.max(1)) / u64::from(screen.0.max(1))) as u32;
        let least = head + ctx.px(SHOT_MIN_H) + cpad;
        let max_rows = ((room + gap) / (least + gap)).max(1) as usize;
        let shown = count.min(max_rows * 2);
        let rows = shown.div_ceil(2) as u32;
        let per_row = room.saturating_sub(gap * rows.saturating_sub(1)) / rows.max(1);
        let card_h = natural_shot.saturating_add(head + cpad).min(per_row).max(head + cpad + 1);
        Self { shown, card_w, card_h, gap }
    }

    /// Высота сетки целиком.
    fn height(&self) -> u32 {
        let rows = self.shown.div_ceil(2) as u32;
        self.card_h * rows + self.gap * rows.saturating_sub(1)
    }
}

/// Вписать `src` в `into` поверхности `dst` с сохранением пропорций, по
/// центру, ближайшей точкой (как миниатюры панели ПК, `thumbs`). Окно шире
/// или выше места — уменьшается, никогда не вылезает: все счёты в `u64`, а
/// запись — только в пределах `into` и `dst`.
fn draw_shot(dst: &mut Surface, src: &Surface, into: Rect) {
    let (sw, sh) = (src.width().max(1), src.height().max(1));
    if into.is_empty() {
        return;
    }
    let (dw, dh) = if u64::from(sw) * u64::from(into.h) > u64::from(sh) * u64::from(into.w) {
        (into.w, (u64::from(sh) * u64::from(into.w) / u64::from(sw)).max(1) as u32)
    } else {
        ((u64::from(sw) * u64::from(into.h) / u64::from(sh)).max(1) as u32, into.h)
    };
    let (dw, dh) = (dw.min(into.w), dh.min(into.h));
    let ox = into.x + (into.w - dw) as i32 / 2;
    let oy = into.y + (into.h - dh) as i32 / 2;
    let columns: Vec<usize> = (0..dw).map(|tx| (u64::from(tx) * u64::from(sw) / u64::from(dw)).min(u64::from(sw) - 1) as usize).collect();
    for ty in 0..dh {
        let target = oy + ty as i32;
        if target < 0 {
            continue;
        }
        let sy = (u64::from(ty) * u64::from(sh) / u64::from(dh)).min(u64::from(sh) - 1) as u32;
        let line = src.row(sy);
        let row = dst.row_mut(target as u32);
        for (tx, &column) in columns.iter().enumerate() {
            let x = ox + tx as i32;
            if x >= 0 && (x as usize) < row.len() {
                if let Some(&pixel) = line.get(column) {
                    row[x as usize] = pixel;
                }
            }
        }
    }
}

/// Сколько окно лежит свёрнутым, словами.
#[must_use]
pub fn age_text(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes == 0 {
        String::from("сейчас")
    } else if minutes < 60 {
        alloc::format!("{minutes} мин")
    } else {
        alloc::format!("{} ч", minutes / 60)
    }
}

/// Перенос по словам по ширине в точках: сколько слов влезает в `width`.
///
/// Свой, а не `widget::wrap`: тот считает знаки, а шрифт у нас
/// пропорциональный, и строка из «ш» вдвое шире строки из «і».
pub fn wrap_px(text: &str, width: u32, measure: impl Fn(&str) -> u32) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut last_fit = 0;
    for (at, _) in text.match_indices(' ').chain(core::iter::once((text.len(), ""))) {
        if measure(&text[start..at]) > width && last_fit > start {
            lines.push(&text[start..last_fit]);
            start = last_fit + 1;
        }
        last_fit = at;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}
