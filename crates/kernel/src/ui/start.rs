// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! «Пуск» и поиск телефона (макет `FreeOS-mobile`, экран 03).
//!
//! # Что в листе
//!
//! Поле поиска, найденное (до трёх строк), закреплённые плитки и строка
//! пользователя с кнопками «Настройки» и «Питание». Нажатие на «FreeOS» в доке
//! открывает лист без клавиатуры, на «Поиск» — сразу с полем в фокусе и
//! клавиатурой.
//!
//! # Что ищется
//!
//! Три рода вещей, и строка найденного называет, что это: программы (то же, что
//! на домашнем экране, плюс «Задачи» и «О системе»), разделы «Настроек» и
//! команды оболочки. Команда открывает терминал и **набирает себя** в нём — не
//! исполняет: человек видит, что будет сделано, и сам жмёт ↵.
//!
//! Совпадение — подстрока без учёта регистра, как в макете («пак» находит
//! «Пакеты» и «pkg install» по пояснению). Нечёткого поиска нет: список короткий,
//! и «похожее» в нём чаще мешало бы, чем находило.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use super::window::App;

/// Что делает строка или плитка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// Окно ядра.
    App(App),
    /// Программа третьего кольца — командой.
    Program(&'static str),
    /// Программы нет — сказать словами (`what` — как у кнопок дока).
    Missing(&'static str),
    /// Команда оболочки: открыть терминал и набрать её.
    Command(&'static str),
}

/// Запись каталога.
struct Entry {
    title: &'static str,
    note: &'static str,
    icon: Icon,
    tone: Tone,
    /// Заголовок — моноширинным (команды).
    mono: bool,
    target: Target,
}

const fn app(title: &'static str, note: &'static str, icon: Icon, tone: Tone, target: Target) -> Entry {
    Entry { title, note, icon, tone, mono: false, target }
}

/// Всё, что находит поиск, в порядке важности: сначала программы.
const CATALOG: [Entry; 20] = [
    app("Терминал", "приложение · оболочка", Icon::Terminal, Tone::Ok, Target::App(App::Terminal)),
    app("Файлы", "приложение · диски и папки", Icon::Folder, Tone::Muted, Target::Program("/bin/files")),
    app("Настройки", "приложение", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    app("Пакеты", "приложение · установка .fpk", Icon::Package, Tone::Accent, Target::App(App::Settings)),
    app("Диски", "приложение · диспетчер устройств", Icon::Disk, Tone::Muted, Target::Program("/bin/devmgr")),
    app("Монитор", "приложение · память и процессор", Icon::Chart, Tone::Muted, Target::Program("/bin/sysmon")),
    app("Задачи", "приложение · диспетчер задач", Icon::Service, Tone::Muted, Target::Program("/bin/taskmgr")),
    app("О системе", "приложение · версия и сборка", Icon::Info, Tone::Muted, Target::App(App::About)),
    app("Телефон", "приложение · пока нет", Icon::Display, Tone::Accent, Target::Missing("phone")),
    app("Журнал", "приложение · пока нет", Icon::Log, Tone::Muted, Target::Missing("log")),
    app("Настройки → Экран", "раздел настроек", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    app("Настройки → Сеть", "раздел настроек", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    app("Настройки → Пакеты", "раздел настроек", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    app("Настройки → Оформление", "раздел настроек", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    app("Настройки → Дата и время", "раздел настроек", Icon::Settings, Tone::Muted, Target::App(App::Settings)),
    Entry { title: "pkg install", note: "команда оболочки · пакеты", icon: Icon::Terminal, tone: Tone::Muted, mono: true, target: Target::Command("pkg install ") },
    Entry { title: "free", note: "команда оболочки · память", icon: Icon::Terminal, tone: Tone::Muted, mono: true, target: Target::Command("free") },
    Entry { title: "tasks", note: "команда оболочки · задачи", icon: Icon::Terminal, tone: Tone::Muted, mono: true, target: Target::Command("tasks") },
    Entry { title: "ls", note: "команда оболочки · каталог", icon: Icon::Terminal, tone: Tone::Muted, mono: true, target: Target::Command("ls ") },
    Entry { title: "shutdown", note: "команда оболочки · питание", icon: Icon::Power, tone: Tone::Muted, mono: true, target: Target::Command("shutdown") },
];

/// Закреплённые плитки — те, что в макете.
const PINNED: [(&str, Icon, Tone, Target); 4] = [
    ("Телефон", Icon::Display, Tone::Accent, Target::Missing("phone")),
    ("Терминал", Icon::Terminal, Tone::Ok, Target::App(App::Terminal)),
    ("Файлы", Icon::Folder, Tone::Muted, Target::Program("/bin/files")),
    ("Диски", Icon::Disk, Tone::Muted, Target::Program("/bin/devmgr")),
];

/// Сколько найденного показывается.
const SHOWN: usize = 3;

/// Во что попало нажатие.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// Поле поиска: дать ему фокус.
    Field,
    Launch(Target),
    Settings,
    Power,
    Inside,
}

/// Кто пользуется телефоном — для нижней строки.
pub struct User {
    pub name: String,
    pub uid: u32,
    pub slot: Option<char>,
}

pub struct Start {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    query: String,
    focused: bool,
    user: User,
    field: Rect,
    rows: Vec<(Rect, Target)>,
    pinned: Vec<(Rect, Target)>,
    settings: Rect,
    power: Rect,
    damage: Rect,
}

/// Числа макета, в его точках.
const PAD_Y: u32 = 18;
const PAD_X: u32 = 16;
const GAP: u32 = 18;
const FIELD_H: u32 = 46;
const CAPS_H: u32 = 10;
const ROW_H: u32 = 56;
const ROW_GAP: u32 = 8;
const PIN_H: u32 = 8 + 46 + 7 + 12 + 8;
const USER_H: u32 = 12 + 40;
const HEIGHT: u32 = PAD_Y * 2
    + FIELD_H
    + GAP
    + CAPS_H
    + 8
    + ROW_H * SHOWN as u32
    + ROW_GAP * (SHOWN as u32 - 1)
    + GAP
    + CAPS_H
    + 10
    + PIN_H
    + GAP
    + USER_H;

fn bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

impl Start {
    /// Собрать лист. `bottom` — нижний край: над доком или над клавиатурой.
    #[must_use]
    pub fn open(screen_w: u32, bottom: i32, scale: u32, user: User, focused: bool) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        let side = ctx.px(8);
        let width = screen_w.checked_sub(side * 2).filter(|w| *w > 0)?;
        let height = ctx.px(HEIGHT);
        let top = bottom - height as i32;
        let surface = Surface::new(width, height, bg())?;
        let mut start = Self {
            surface,
            rect: Rect::new(side as i32, top, width, height),
            scale,
            query: String::new(),
            focused,
            user,
            field: Rect::EMPTY,
            rows: Vec::new(),
            pinned: Vec::new(),
            settings: Rect::EMPTY,
            power: Rect::EMPTY,
            damage: Rect::EMPTY,
        };
        start.redraw();
        Some(start)
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    #[must_use]
    pub const fn focused(&self) -> bool {
        self.focused
    }

    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Дать полю фокус. Возвращает `true`, если он появился.
    pub fn focus(&mut self) -> bool {
        if self.focused {
            return false;
        }
        self.focused = true;
        self.redraw();
        true
    }

    /// Переехать: лист встаёт над клавиатурой или опускается к доку.
    pub fn move_bottom(&mut self, bottom: i32) -> Rect {
        let before = self.rect;
        self.rect.y = bottom - self.rect.h as i32;
        before
    }

    /// Добавить набранный знак.
    pub fn type_char(&mut self, ch: char) {
        // Поле короткое, и строка длиннее сорока знаков в нём не видна —
        // а искать по ней в каталоге из двух десятков записей незачем.
        if self.query.chars().count() < 40 && !ch.is_control() {
            self.query.push(ch);
            self.redraw();
        }
    }

    /// Стереть последний знак.
    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.redraw();
        }
    }

    /// Что запустит ↵: первое найденное.
    #[must_use]
    pub fn first(&self) -> Option<Target> {
        self.rows.first().map(|(_, target)| *target)
    }

    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        if self.field.contains(x, y) {
            return Some(Hit::Field);
        }
        if self.settings.contains(x, y) {
            return Some(Hit::Settings);
        }
        if self.power.contains(x, y) {
            return Some(Hit::Power);
        }
        for (rect, target) in self.rows.iter().chain(self.pinned.iter()) {
            if rect.contains(x, y) {
                return Some(Hit::Launch(*target));
            }
        }
        Some(Hit::Inside)
    }

    /// Найденное по запросу — номера записей каталога.
    fn matches(&self) -> Vec<usize> {
        if self.query.is_empty() {
            // Пустой запрос показывает главное — первые программы: лист не
            // должен открываться с пустой серединой.
            return (0..SHOWN).collect();
        }
        let needle = lower(&self.query);
        (0..CATALOG.len())
            .filter(|&index| {
                let entry = &CATALOG[index];
                lower(entry.title).contains(&needle) || lower(entry.note).contains(&needle)
            })
            .collect()
    }

    fn redraw(&mut self) {
        let ctx = Ctx::scaled(self.scale).on(bg());
        let p = ctx.palette;
        let card = self.surface.bounds();
        let round = ctx.px(34);
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, round, p.crown.color, p.crown.alpha);
        let x = ctx.px(PAD_X) as i32;
        let w = card.w.saturating_sub(ctx.px(PAD_X) * 2);
        let mut y = ctx.px(PAD_Y) as i32;

        // Поле поиска: утопленное, с кольцом акцента, когда в фокусе.
        let field = Rect::new(x, y, w, ctx.px(FIELD_H));
        draw::rounded(&mut self.surface, field, ctx.px(16), ctx.flat(p.sunk), 255);
        if self.focused {
            draw::rounded_stroke(&mut self.surface, field, ctx.px(16), p.acc, 255);
        } else {
            draw::rounded_stroke(&mut self.surface, field, ctx.px(16), p.btnline.color, p.btnline.alpha);
        }
        let lens = ctx.px(16);
        glyphicon::draw(&mut self.surface, Icon::Search, field.x + ctx.px(14) as i32, field.y + (field.h as i32 - lens as i32) / 2, lens, if self.focused { p.acc_ink } else { p.ink4 }, 255);
        let text_x = field.x + ctx.px(14 + 16 + 10) as i32;
        let found = self.matches();
        let count = alloc::format!("{} {}", found.len(), matches_word(found.len()));
        let mono = ctx.face(Role::MonoSmall);
        let count_w = if self.query.is_empty() { 0 } else { mono.width(&count) };
        let room = (field.right() - ctx.px(14) as i32 - count_w as i32 - text_x - ctx.px(8) as i32).max(0) as u32;
        let (text, ink) = if self.query.is_empty() { ("Пуск или поиск", p.ink4) } else { (self.query.as_str(), p.ink) };
        let typed = paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, paint::baseline(ctx, Role::Body, field), room, text, ink);
        if self.focused {
            // Курсор — черта акцента за последним знаком.
            let caret_x = if self.query.is_empty() { text_x } else { text_x + typed as i32 + ctx.px(2) as i32 };
            let caret = Rect::new(caret_x, field.y + (field.h as i32 - ctx.px(18) as i32) / 2, ctx.px(1).max(1), ctx.px(18));
            draw::rounded(&mut self.surface, caret, 0, p.acc, 255);
        }
        if !self.query.is_empty() {
            paint::text(ctx, &mut self.surface, Role::MonoSmall, field.right() - ctx.px(14) as i32 - count_w as i32, paint::baseline(ctx, Role::MonoSmall, field), &count, p.ink4);
        }
        self.field = field;
        y += ctx.px(FIELD_H + GAP) as i32;

        // Найденное.
        paint::caps(ctx, &mut self.surface, x + ctx.px(4) as i32, y, if self.query.is_empty() { "ПРИЛОЖЕНИЯ" } else { "НАЙДЕНО" });
        y += ctx.px(CAPS_H + 8) as i32;
        self.rows.clear();
        for (slot, index) in found.iter().take(SHOWN).enumerate() {
            let entry = &CATALOG[*index];
            let row = Rect::new(x, y, w, ctx.px(ROW_H));
            let r = ctx.px(18);
            let first = slot == 0 && !self.query.is_empty();
            if first {
                draw::horizontal_gradient(&mut self.surface, row, r, ctx.flat(p.sel1), ctx.flat(p.sel2), 255);
                draw::rounded_stroke(&mut self.surface, row, r, p.accedge, 255);
            } else {
                draw::rounded(&mut self.surface, row, r, p.card.color, p.card.alpha);
                draw::rounded_stroke(&mut self.surface, row, r, p.line2.color, p.line2.alpha);
            }
            let tile = Rect::new(row.x + ctx.px(12) as i32, row.y + ctx.px(11) as i32, ctx.px(34), ctx.px(34));
            tile_art(&mut self.surface, ctx, tile, ctx.px(12), entry.icon, entry.tone, ctx.px(18));
            let tx = tile.right() + ctx.px(12) as i32;
            let troom = (row.right() - ctx.px(30) as i32 - tx).max(0) as u32;
            let title_role = if entry.mono { Role::Mono } else { Role::Title };
            let title_face = ctx.face(title_role);
            let note_face = ctx.face(Role::Caption);
            let top = row.y + (row.h as i32 - i32::from(title_face.line) - i32::from(note_face.line)) / 2;
            paint::text_clipped(ctx, &mut self.surface, title_role, tx, top, troom, entry.title, if first { p.ink } else { p.ink2 });
            paint::text_clipped(ctx, &mut self.surface, Role::Caption, tx, top + i32::from(title_face.line), troom, entry.note, p.ink3);
            if first {
                let size = ctx.px(14);
                glyphicon::draw(&mut self.surface, Icon::Enter, row.right() - ctx.px(12) as i32 - size as i32, row.y + (row.h as i32 - size as i32) / 2, size, p.acc_ink, 255);
            }
            self.rows.push((row, entry.target));
            y += ctx.px(ROW_H + ROW_GAP) as i32;
        }
        if found.is_empty() {
            let note = Rect::new(x, y, w, ctx.px(ROW_H));
            paint::text_clipped(ctx, &mut self.surface, Role::Body, x + ctx.px(8) as i32, paint::baseline(ctx, Role::Body, note), w, "Ничего не нашлось", p.ink4);
        }
        y = ctx.px(PAD_Y + FIELD_H + GAP + CAPS_H + 8 + ROW_H * SHOWN as u32 + ROW_GAP * (SHOWN as u32 - 1) + GAP) as i32;

        // Закреплённые.
        paint::caps(ctx, &mut self.surface, x + ctx.px(4) as i32, y, "ЗАКРЕПЛЕНО");
        y += ctx.px(CAPS_H + 10) as i32;
        self.pinned.clear();
        let cell_w = w / 4;
        for (index, (label, icon, tone, target)) in PINNED.iter().enumerate() {
            let cell = Rect::new(x + (index as u32 * cell_w) as i32, y, cell_w, ctx.px(PIN_H));
            let side = ctx.px(46);
            let tile = Rect::new(cell.x + (cell.w as i32 - side as i32) / 2, cell.y + ctx.px(8) as i32, side, side);
            tile_art(&mut self.surface, ctx, tile, ctx.px(15), *icon, *tone, ctx.px(22));
            let face = ctx.face(Role::Caption);
            let lw = face.width(label).min(cell.w);
            paint::text_clipped(ctx, &mut self.surface, Role::Caption, cell.x + (cell.w as i32 - lw as i32) / 2, tile.bottom() + ctx.px(7) as i32, cell.w, label, p.ink2);
            self.pinned.push((cell, *target));
        }
        y += ctx.px(PIN_H + GAP) as i32;

        // Пользователь.
        draw::hline(&mut self.surface, x, y - ctx.px(6) as i32, w, p.line.color, p.line.alpha);
        let avatar = Rect::new(x + ctx.px(4) as i32, y + ctx.px(12) as i32, ctx.px(36), ctx.px(36));
        draw::rounded_gradient(&mut self.surface, avatar, ctx.px(13), p.acc, p.acc2, 255);
        let name = if self.user.name.is_empty() { "root" } else { self.user.name.as_str() };
        let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_default();
        let strong = ctx.face(Role::Strong);
        paint::text(ctx, &mut self.surface, Role::Strong, avatar.x + (avatar.w as i32 - strong.width(&initial) as i32) / 2, paint::baseline(ctx, Role::Strong, avatar), &initial, Color::rgb(0xFF, 0xFF, 0xFF));
        let nx = avatar.right() + ctx.px(10) as i32;
        let title = ctx.face(Role::Title);
        let top = avatar.y + (avatar.h as i32 - i32::from(title.line) - i32::from(mono.line)) / 2;
        paint::text(ctx, &mut self.surface, Role::Title, nx, top, name, p.ink);
        let slot = match self.user.slot {
            Some(letter) => alloc::format!("uid {} · слот {letter}", self.user.uid),
            None => alloc::format!("uid {} · live", self.user.uid),
        };
        paint::text(ctx, &mut self.surface, Role::MonoSmall, nx, top + i32::from(title.line), &slot, p.ink4);
        let button = ctx.px(40);
        let by = avatar.y + (avatar.h as i32 - button as i32) / 2;
        self.power = Rect::new(x + w as i32 - button as i32, by, button, button);
        self.settings = Rect::new(self.power.x - ctx.px(8) as i32 - button as i32, by, button, button);
        let br = ctx.px(14);
        draw::rounded(&mut self.surface, self.settings, br, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(&mut self.surface, self.settings, br, p.btnline.color, p.btnline.alpha);
        draw::rounded(&mut self.surface, self.power, br, p.badbg.color, p.badbg.alpha);
        draw::rounded_stroke(&mut self.surface, self.power, br, p.badline, 255);
        let g = ctx.px(18);
        glyphicon::draw(&mut self.surface, Icon::Settings, self.settings.x + (button as i32 - g as i32) / 2, by + (button as i32 - g as i32) / 2, g, p.ink3, 255);
        glyphicon::draw(&mut self.surface, Icon::Power, self.power.x + (button as i32 - g as i32) / 2, by + (button as i32 - g as i32) / 2, g, p.bad_ink, 255);

        self.damage = card;
    }
}

/// Плитка со значком: залитая для акцента и «ok», стеклянная для остального.
fn tile_art(surface: &mut Surface, ctx: Ctx, tile: Rect, radius: u32, icon: Icon, tone: Tone, glyph: u32) {
    let p = ctx.palette;
    let ink = match tone {
        Tone::Accent => {
            draw::rounded_gradient(surface, tile, radius, p.acc, p.acc2, 255);
            Color::rgb(0xFF, 0xFF, 0xFF)
        }
        Tone::Ok => {
            draw::rounded_gradient(surface, tile, radius, p.ok, p.ok2, 255);
            Color::rgb(0xFF, 0xFF, 0xFF)
        }
        _ => {
            draw::rounded(surface, tile, radius, p.btn.color, p.btn.alpha);
            draw::rounded_stroke(surface, tile, radius, p.btnline.color, p.btnline.alpha);
            p.ink3
        }
    };
    glyphicon::draw(surface, icon, tile.x + (tile.w as i32 - glyph as i32) / 2, tile.y + (tile.h as i32 - glyph as i32) / 2, glyph, ink, 255);
}

/// Строка в нижнем регистре — для сравнения без учёта регистра, кириллицы тоже.
fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// «совпадение», «совпадения», «совпадений».
fn matches_word(n: usize) -> &'static str {
    match (n % 10, n % 100) {
        (1, rem) if rem != 11 => "совпадение",
        (2..=4, rem) if !(12..=14).contains(&rem) => "совпадения",
        _ => "совпадений",
    }
}
