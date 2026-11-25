//! Страница «Персонализация → Фон» (22.11.2026): галерея картинок с
//! миниатюрами, «Обзор…», расположение, градиенты темы и сплошной цвет.
//! Состояние и рисование фона — `ui::wallpaper`.

use alloc::string::String;

use mini_ui::draw;
use mini_ui::iconset::Fi;
use mini_ui::paint::{self, Weight};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect};

use super::super::wallpaper::{self, COLORS, Choice, FOSSAI_THUMB, Fit, LockDeed, WallDeed};
use super::Deed;
use super::kit::{Pass, button_width, line_h};

/// Плитка галереи: что нарисовать и что нажать.
struct Tile {
    key: String,
    title: String,
    deed: Deed,
    current: bool,
}

pub(super) fn page(pass: &mut Pass, x: i32, y: i32, w: u32) {
    let choice = wallpaper::choice();
    let preset = theme::wallpaper();

    // ── Картинка ─────────────────────────────────────────────────────────
    let mut y = pass.group(x, y, w, "Картинка");
    let mut tiles = alloc::vec![Tile {
        key: String::from(FOSSAI_THUMB),
        title: String::from("FossAi"),
        deed: Deed::Wallpaper(theme::Wallpaper::Fossai),
        current: choice == Choice::Preset && preset == theme::Wallpaper::Fossai,
    }];
    let gallery = wallpaper::gallery();
    for item in &gallery {
        tiles.push(Tile {
            key: item.path.clone(),
            title: item.title.clone(),
            deed: Deed::Background(WallDeed::Image(item.path.clone())),
            current: choice == Choice::Image(item.path.clone()),
        });
    }
    // Своя картинка — плиткой рядом с галереей, пока она выбрана.
    if let Choice::Image(path) = &choice {
        if !gallery.iter().any(|item| item.path == *path) {
            let name = path.rsplit('/').next().unwrap_or(path);
            tiles.push(Tile { key: path.clone(), title: String::from(name), deed: Deed::Background(WallDeed::Image(path.clone())), current: true });
        }
    }
    y = tile_grid(pass, x, y, w, &tiles, Deed::Background(WallDeed::Browse));
    let status = wallpaper::note().unwrap_or_else(|| String::from("Свои картинки — JPEG, PNG, BMP или QOI: «Обзор…» откроет окно выбора файла."));
    y = pass.note(x, y, w, &status);

    // ── Экран блокировки (24.11) ─────────────────────────────────────────
    let same = wallpaper::lock_same();
    y = super::kit::row(pass, x, y, w, Some(Fi::Lock), "Также для экрана блокировки", "Снимите флажок, чтобы выбрать экрану блокировки свой фон", super::kit::Ctl::Check(same), Some(Deed::Background(WallDeed::LockSame(!same))));
    if !same {
        y = super::kit::row(pass, x, y, w, Some(Fi::Image), "Фон экрана блокировки", "Свой: картинка, обои FossAi или цвет", super::kit::Ctl::Nav, Some(Deed::Open(super::Section::LockScreen)));
    }
    rest(pass, x, y, w, choice, preset);
}

/// «Экран блокировки» (24.11): тот же фон, что у стола, или свой — обои
/// FossAi, картинка галереи, своя («Обзор…») или цвет. Возвращает `y` под ним.
pub(super) fn lock_page(pass: &mut Pass, x: i32, y: i32, w: u32) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let same = wallpaper::lock_same();
    // Образец: фон экрана блокировки с часами, как в Windows.
    let pw = w.min(ctx.px(320));
    let preview = Rect::new(x, y + ctx.px(8) as i32, pw, pw * 9 / 16);
    if pass.visible(preview) {
        let (choice, color) = if same { (wallpaper::choice(), wallpaper::color()) } else { wallpaper::lock_choice() };
        let preset = if same { theme::wallpaper() } else { theme::Wallpaper::Fossai };
        let clock = crate::time::clock_text().unwrap_or_default();
        pass.on(|s| {
            thumb(s, preview, ctx.px(8), &choice, color, preset);
            draw::blend_rect(s, preview, Color::rgb(0x04, 0x08, 0x14), 70);
            ring(s, preview, ctx.px(8), p.line3.color, p.line3.alpha, 0, 1);
            paint::text(ctx, s, Role::Heading, preview.x + ctx.px(16) as i32, preview.y + preview.h as i32 / 6, &clock, Color::rgb(0xFF, 0xFF, 0xFF));
        });
    }
    let mut y = preview.bottom() + ctx.px(4) as i32;
    y = pass.group(x, y, w, "Фон экрана блокировки");
    y = super::kit::row(pass, x, y, w, Some(Fi::Image), "Как на рабочем столе", "Тот же фон, что выбран в «Фоне» (флажок «Также для экрана блокировки»)", super::kit::Ctl::Check(same), Some(Deed::Background(WallDeed::LockSame(!same))));
    if same {
        return y;
    }
    let (choice, color_now) = wallpaper::lock_choice();
    let lock = |deed: LockDeed| Deed::Background(WallDeed::Lock(deed));
    let mut tiles = alloc::vec![Tile { key: String::from(FOSSAI_THUMB), title: String::from("FossAi"), deed: lock(LockDeed::Fossai), current: choice == Choice::Preset }];
    let gallery = wallpaper::gallery();
    for item in &gallery {
        tiles.push(Tile { key: item.path.clone(), title: item.title.clone(), deed: lock(LockDeed::Image(item.path.clone())), current: choice == Choice::Image(item.path.clone()) });
    }
    if let Choice::Image(path) = &choice {
        if !gallery.iter().any(|item| item.path == *path) {
            let name = path.rsplit('/').next().unwrap_or(path);
            tiles.push(Tile { key: path.clone(), title: String::from(name), deed: lock(LockDeed::Image(path.clone())), current: true });
        }
    }
    y = tile_grid(pass, x, y + ctx.px(4) as i32, w, &tiles, lock(LockDeed::Browse));
    y = pass.group(x, y, w, "Сплошной цвет");
    let dot = ctx.px(36);
    let dot_gap = ctx.px(12);
    let per_line = ((w + dot_gap) / (dot + dot_gap)).max(1);
    y += ctx.px(4) as i32;
    for (index, color) in COLORS.into_iter().enumerate() {
        let (col, line) = (index as u32 % per_line, index as u32 / per_line);
        let rect = Rect::new(x + ctx.px(4) as i32 + (col * (dot + dot_gap)) as i32, y + (line * (dot + dot_gap)) as i32, dot, dot);
        let focused = pass.deed(rect, lock(LockDeed::Color(color)));
        let current = choice == Choice::Color && color_now == color;
        if pass.visible(Rect::new(rect.x - 4, rect.y - 4, rect.w + 8, rect.h + 8)) {
            let fill = Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8);
            pass.on(|s| {
                let r = rect.w / 2;
                draw::rounded(s, rect, r, fill, 255);
                draw::rounded_stroke(s, rect, r, p.line3.color, p.line3.alpha);
                if current {
                    ring(s, rect, r, p.acc, 255, 2, 3);
                }
                if focused {
                    ring(s, rect, r, p.ink, 255, 6, 1);
                }
            });
        }
    }
    y += (COLORS.len() as u32).div_ceil(per_line) as i32 * (dot + dot_gap) as i32;
    pass.note(x, y, w, "Картинка закрывает экран целиком («Заполнение»); часы и поле пароля — поверх.")
}

/// Плитки картинок с подписями и «Обзор…» последней. Возвращает `y` под ними.
fn tile_grid(pass: &mut Pass, x: i32, y: i32, w: u32, tiles: &[Tile], browse: Deed) -> i32 {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let gap = ctx.px(12);
    let min_w = ctx.px(150);
    let count = tiles.len() as u32 + 1;
    let cols = ((w + gap) / (min_w + gap)).clamp(2, count.max(2));
    let tile_w = (w.saturating_sub(gap * (cols - 1)) / cols).min(ctx.px(220));
    let tile_h = tile_w * 9 / 16;
    let label_h = line_h(ctx, Role::Caption) as u32 + ctx.px(6);
    let cell_h = tile_h + label_h + ctx.px(10);
    let radius = ctx.px(6);
    let busy = wallpaper::busy();
    for (index, tile) in tiles.iter().enumerate() {
        let (col, line) = (index as u32 % cols, index as u32 / cols);
        let rect = Rect::new(x + (col * (tile_w + gap)) as i32, y + (line * cell_h) as i32, tile_w, tile_h);
        let focused = pass.deed_in(rect, Rect::new(rect.x, rect.y, rect.w, rect.h + label_h), tile.deed.clone());
        if !pass.visible(Rect::new(rect.x, rect.y, rect.w, rect.h + label_h)) {
            continue;
        }
        let key = tile.key.clone();
        pass.on(|s| {
            if !wallpaper::draw_thumb(s, rect, &key) {
                draw::rounded(s, rect, radius, p.sunk.color, 255);
                let face = ctx.face(Role::Caption);
                let text = if busy { "Готовится…" } else { "Нет миниатюры" };
                paint::text_centered(ctx, s, Role::Caption, Rect::new(rect.x, rect.y + (rect.h as i32 - i32::from(face.line)) / 2, rect.w, u32::from(face.line)), text, p.ink3);
            }
            ring(s, rect, radius, p.line3.color, p.line3.alpha, 0, 1);
            if tile.current {
                ring(s, rect, radius, p.acc, 255, 1, 3);
            }
            if focused {
                ring(s, rect, radius, p.ink, 255, 4, 2);
            }
        });
        let ink = if tile.current { p.ink } else { p.ink3 };
        pass.text(Role::Caption, rect.x, rect.bottom() + ctx.px(6) as i32, rect.w, &tile.title, ink);
    }
    // «Обзор…» — последняя плитка.
    {
        let index = tiles.len() as u32;
        let (col, line) = (index % cols, index / cols);
        let rect = Rect::new(x + (col * (tile_w + gap)) as i32, y + (line * cell_h) as i32, tile_w, tile_h);
        let focused = pass.deed_in(rect, Rect::new(rect.x, rect.y, rect.w, rect.h + label_h), browse);
        if pass.visible(Rect::new(rect.x, rect.y, rect.w, rect.h + label_h)) {
            let icon = ctx.px(28);
            pass.on(|s| {
                draw::rounded(s, rect, radius, p.sunk.color, 255);
                ring(s, rect, radius, p.line3.color, p.line3.alpha, 0, 1);
                if focused {
                    ring(s, rect, radius, p.ink, 255, 4, 2);
                }
            });
            pass.icon(Fi::FolderOpen, rect.x + (rect.w as i32 - icon as i32) / 2, rect.y + (rect.h as i32 - icon as i32) / 2, icon, p.ink2);
            pass.text(Role::Caption, rect.x, rect.bottom() + ctx.px(6) as i32, rect.w, "Обзор…", p.ink3);
        }
    }
    y + (count.div_ceil(cols) * cell_h) as i32
}

/// Расположение, градиенты темы и сплошной цвет — под картинками «Фона».
fn rest(pass: &mut Pass, x: i32, y: i32, w: u32, choice: Choice, preset: theme::Wallpaper) {
    let ctx = pass.ctx;
    let p = ctx.palette;
    let gap = ctx.px(12);
    let label_h = line_h(ctx, Role::Caption) as u32 + ctx.px(6);
    let radius = ctx.px(6);
    // ── Расположение ─────────────────────────────────────────────────────
    let mut y = pass.group(x, y, w, "Расположение картинки");
    let current_fit = wallpaper::fit();
    let button_h = ctx.px(34);
    let mut bx = x;
    for fit in Fit::ALL {
        let bw = button_width(ctx, fit.title());
        if bx + bw as i32 > x + w as i32 && bx > x {
            bx = x;
            y += (button_h + ctx.px(8)) as i32;
        }
        let rect = Rect::new(bx, y, bw, button_h);
        let focused = pass.deed(rect, Deed::Background(WallDeed::Fit(fit)));
        if pass.visible(rect) {
            let weight = if fit == current_fit { Weight::Primary } else { Weight::Normal };
            pass.on(|s| paint::button(ctx, s, rect, weight, fit.title(), focused));
        }
        bx += (bw + ctx.px(8)) as i32;
    }
    y += (button_h + ctx.px(8)) as i32;
    y = pass.note(x, y, w, "«По размеру» и «По центру» оставляют поля — они цвета, выбранного ниже.");

    // ── Градиент темы ────────────────────────────────────────────────────
    y = pass.group(x, y, w, "Градиент");
    let grads: alloc::vec::Vec<theme::Wallpaper> = theme::Wallpaper::ALL.into_iter().filter(|wall| *wall != theme::Wallpaper::Fossai).collect();
    let gcount = grads.len() as u32;
    let gw = (w.saturating_sub(gap * (gcount - 1)) / gcount).min(ctx.px(150));
    let gh = gw * 9 / 16;
    for (index, wall) in grads.into_iter().enumerate() {
        let rect = Rect::new(x + (index as u32 * (gw + gap)) as i32, y, gw, gh);
        let focused = pass.deed_in(rect, Rect::new(rect.x, rect.y, rect.w, rect.h + label_h), Deed::Wallpaper(wall));
        let current = choice == Choice::Preset && preset == wall;
        if pass.visible(Rect::new(rect.x, rect.y, rect.w, rect.h + label_h)) {
            let (top, bottom) = theme::wallpaper_colors(wall);
            pass.on(|s| {
                draw::rounded_gradient(s, rect, radius, top, bottom, 255);
                ring(s, rect, radius, p.line3.color, p.line3.alpha, 0, 1);
                if current {
                    ring(s, rect, radius, p.acc, 255, 1, 3);
                }
                if focused {
                    ring(s, rect, radius, p.ink, 255, 4, 2);
                }
            });
            let ink = if current { p.ink } else { p.ink3 };
            pass.text(Role::Caption, rect.x, rect.bottom() + ctx.px(6) as i32, rect.w, wall.title(), ink);
        }
    }
    y += (gh + label_h + ctx.px(10)) as i32;

    // ── Сплошной цвет ────────────────────────────────────────────────────
    y = pass.group(x, y, w, "Сплошной цвет");
    let dot = ctx.px(36);
    let dot_gap = ctx.px(12);
    let per_line = ((w + dot_gap) / (dot + dot_gap)).max(1);
    let color_now = wallpaper::color();
    y += ctx.px(4) as i32;
    for (index, color) in COLORS.into_iter().enumerate() {
        let (col, line) = (index as u32 % per_line, index as u32 / per_line);
        let rect = Rect::new(x + ctx.px(4) as i32 + (col * (dot + dot_gap)) as i32, y + (line * (dot + dot_gap)) as i32, dot, dot);
        let focused = pass.deed(rect, Deed::Background(WallDeed::Color(color)));
        let current = choice == Choice::Color && color_now == color;
        if pass.visible(Rect::new(rect.x - 4, rect.y - 4, rect.w + 8, rect.h + 8)) {
            let fill = Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8);
            pass.on(|s| {
                let r = rect.w / 2;
                draw::rounded(s, rect, r, fill, 255);
                draw::rounded_stroke(s, rect, r, p.line3.color, p.line3.alpha);
                if current || (color_now == color && choice != Choice::Color) {
                    // Выбранный цвет полей — тонкое кольцо, выбранный фон — толстое.
                    let width = if current { 3 } else { 1 };
                    for i in 0..width {
                        let ring = Rect::new(rect.x - 2 - i, rect.y - 2 - i, rect.w + 4 + 2 * i as u32, rect.h + 4 + 2 * i as u32);
                        draw::rounded_stroke(s, ring, r + 2 + i as u32, p.acc, 255);
                    }
                }
                if focused {
                    let ring = Rect::new(rect.x - 6, rect.y - 6, rect.w + 12, rect.h + 12);
                    draw::rounded_stroke(s, ring, r + 6, p.ink, 255);
                }
            });
        }
    }
    y += (COLORS.len() as u32).div_ceil(per_line) as i32 * (dot + dot_gap) as i32;
    pass.note(x, y, w, "Один фон на все мониторы; картинка уменьшается под каждый один раз, при выборе.");
}

/// Миниатюра фона: картинка, цвет или обои темы (`preset` при `Preset`).
pub(super) fn thumb(s: &mut mini_ui::Surface, rect: Rect, radius: u32, choice: &Choice, color: u32, preset: theme::Wallpaper) {
    let drawn = match choice {
        Choice::Image(path) => wallpaper::draw_thumb(s, rect, path),
        Choice::Color => {
            draw::rounded(s, rect, radius, Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8), 255);
            true
        }
        Choice::Preset if preset == theme::Wallpaper::Fossai => wallpaper::draw_thumb(s, rect, FOSSAI_THUMB),
        Choice::Preset => false,
    };
    if !drawn {
        let (top, bottom) = theme::wallpaper_colors(preset);
        draw::rounded_gradient(s, rect, radius, top, bottom, 255);
    }
}

/// Рамка вокруг `rect` толщиной `width` точек, отступив наружу на `out`.
fn ring(s: &mut mini_ui::Surface, rect: Rect, radius: u32, color: Color, alpha: u8, out: i32, width: i32) {
    for i in 0..width {
        let grow = out + i;
        let ring = Rect::new(rect.x - grow, rect.y - grow, rect.w + 2 * grow as u32, rect.h + 2 * grow as u32);
        draw::rounded_stroke(s, ring, radius + grow as u32, color, alpha);
    }
}
