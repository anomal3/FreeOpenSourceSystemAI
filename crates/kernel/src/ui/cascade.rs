// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Виталий Ардашов (gerzoid), Роман Кощеев (anomal3)

//! «Пуск» в классике (22.11.2026): каскадное меню Windows 98.
//!
//! # Что в нём
//!
//! Слева — синяя полоса с повёрнутой надписью «FossAi», справа — строки с
//! крупными значками: «Программы ▸», «Документы ▸», «Параметры ▸», «Поиск»,
//! «Справка», «Выполнить…», черта, «Завершение работы…». Строка со стрелкой
//! раскрывает подменю справа от себя при наведении (или стрелкой вправо и
//! Enter); у подменю мелкие значки, папки «Программ» — свои подменю
//! («Программы Windows», «Системные утилиты»…). Не помещается по высоте —
//! столбцами, как у Windows 98.
//!
//! # Слои
//!
//! Корень рисуется в поверхности самого [`Menu`] (её место — `Menu::rect`),
//! каждое подменю — своей поверхностью ([`Sub`]): композитор кладёт их поверх
//! (`Menu::layers`), а места открытых и закрытых подменю отдаёт
//! `Menu::take_sub_damage`. Современный «Пуск» этот модуль не трогает: всё
//! здесь работает, только когда меню собрано в классике (`Cascade::on`).
//!
//! # Клавиатура
//!
//! ↑/↓ — по строкам уровня, → и Enter — раскрыть подменю (Enter на
//! программе — запустить), ← и Escape — закрыть подменю (Escape в корне —
//! «Пуск»), буква — к строке на эту букву (единственная такая — сразу
//! выбрать, как ускоритель Windows).

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::classic as c98;
use mini_ui::glyphicon::Icon;
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::theme::win98;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use super::super::appicon::Art;
use super::super::settings::Section;
use super::super::window::App;
use super::{Choice, Menu, MenuClick, desk_ctx, draw_item_icon, startapps};

/// Строка корня, её значок и полоса слева (точки при масштабе 1).
const ROOT_ROW: u32 = 36;
const ROOT_ICON: u32 = 32;
const BANNER_W: u32 = 24;
/// Строка подменю и её значок.
const SUB_ROW: u32 = 22;
const SUB_ICON: u32 = 16;
/// Черта-разделитель.
const SEP_H: u32 = 8;
/// Кромка карточки и поле внутри неё.
const PAD: u32 = 3;
/// Место под стрелку подменю справа.
const ARROW_W: u32 = 20;
/// Сколько недавних файлов в «Документах».
const RECENT_MAX: usize = 15;

/// Строки «Параметров ▸»: подпись, значок темы, знак набора.
const SETTINGS: [(&str, &str, Icon); 7] = [
    ("Панель управления", "settings", Icon::Settings),
    ("Панель задач и меню «Пуск»", "folder-tools", Icon::Grid),
    ("Оформление", "display", Icon::Display),
    ("Фон рабочего стола", "doc-image", Icon::Display),
    ("Администрирование", "admin", Icon::Settings),
    ("Диспетчер задач", "task-manager", Icon::Chart),
    ("Диспетчер устройств", "device-manager", Icon::Devices),
];

/// Строка классического «Пуска» или его подменю.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Node {
    Programs,
    Documents,
    Settings,
    Find,
    Help,
    Run,
    Shutdown,
    /// Папка «Программ» (номер в `startapps::FOLDERS`).
    Folder(usize),
    /// Программа (номер в `Menu::items`).
    Item(usize),
    /// Недавний файл (номер в `Menu::recent`).
    Recent(usize),
    /// Строка «Параметров ▸» (номер в [`SETTINGS`]).
    Setting(usize),
    /// «(пусто)» — серая, не выбирается.
    Empty,
    Sep,
}

impl Node {
    /// Раскрывает ли строка подменю.
    const fn opens(self) -> bool {
        matches!(self, Node::Programs | Node::Documents | Node::Settings | Node::Folder(_))
    }

    /// Выбирается ли строка (черта и «пусто» — нет).
    const fn selectable(self) -> bool {
        !matches!(self, Node::Sep | Node::Empty)
    }
}

/// Открытое подменю.
pub(super) struct Sub {
    /// Чьё оно (строка-родитель).
    node: Node,
    rows: Vec<Node>,
    /// Места строк в его поверхности.
    places: Vec<Rect>,
    /// Место на экране.
    rect: Rect,
    surface: Surface,
    hot: Option<usize>,
}

/// Состояние классического «Пуска».
pub(super) struct Cascade {
    /// Меню собрано в классике.
    pub(super) on: bool,
    /// Рабочая область монитора (без панели) — подменю не вылезают за неё.
    work: Rect,
    root: Vec<Node>,
    root_places: Vec<Rect>,
    root_hot: Option<usize>,
    subs: Vec<Sub>,
    /// Уровень, по которому ходят стрелки: 0 — корень, k — `subs[k-1]`.
    level: usize,
    /// Места появившихся и ушедших подменю (экран).
    damage: Rect,
}

impl Cascade {
    /// Выключенный каскад — у современного «Пуска» и телефона.
    pub(super) const fn off() -> Self {
        Self { on: false, work: Rect::EMPTY, root: Vec::new(), root_places: Vec::new(), root_hot: None, subs: Vec::new(), level: 0, damage: Rect::EMPTY }
    }

    /// Каскад для «Пуска» в классике: `work` — рабочая область монитора.
    pub(super) fn new(work: Rect) -> Self {
        let root = alloc::vec![Node::Programs, Node::Documents, Node::Settings, Node::Find, Node::Help, Node::Run, Node::Sep, Node::Shutdown];
        Self { on: true, work, root, ..Self::off() }
    }

    /// Размер корня (карточки «Пуска») при масштабе `ctx`.
    pub(super) fn root_size(&self, ctx: Ctx) -> (u32, u32) {
        let face = ctx.face(Role::Body);
        let text = self.root.iter().map(|node| face.width(root_caption(*node))).max().unwrap_or(0);
        let w = ctx.px(PAD * 2 + 2 + ROOT_ICON + 10 + ARROW_W + 6) + banner_w(ctx) + text;
        let h = self.root.iter().map(|node| row_h(ctx, *node, true)).sum::<u32>() + ctx.px(PAD * 2);
        (w.max(ctx.px(180)), h)
    }

    /// Закрыть все подменю, снять подсветку (меню открыли заново).
    pub(super) fn reset(&mut self) {
        for sub in self.subs.drain(..) {
            self.damage = self.damage.union(&sub.rect);
        }
        self.root_hot = None;
        self.level = 0;
    }

    /// Место корня и всех подменю.
    pub(super) fn span(&self, root: Rect) -> Rect {
        self.subs.iter().fold(root, |acc, sub| acc.union(&sub.rect))
    }

    pub(super) fn layers(&self) -> impl Iterator<Item = (&Surface, Rect)> {
        self.subs.iter().map(|sub| (&sub.surface, sub.rect))
    }

    pub(super) fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }
}

/// Ширина синей полосы: по высоте строки надписи.
fn banner_w(ctx: Ctx) -> u32 {
    ctx.px(BANNER_W).max(u32::from(ctx.face(Role::Heading).line) + ctx.px(4))
}

fn row_h(ctx: Ctx, node: Node, root: bool) -> u32 {
    match (node, root) {
        (Node::Sep, _) => ctx.px(SEP_H),
        (_, true) => ctx.px(ROOT_ROW),
        (_, false) => ctx.px(SUB_ROW),
    }
}

const fn root_caption(node: Node) -> &'static str {
    match node {
        Node::Programs => "Программы",
        Node::Documents => "Документы",
        Node::Settings => "Параметры",
        Node::Find => "Поиск",
        Node::Help => "Справка",
        Node::Run => "Выполнить…",
        Node::Shutdown => "Завершение работы…",
        _ => "",
    }
}

/// Что сделала клавиша в классическом «Пуске».
pub enum ClassicKey {
    Stay,
    Launch(Choice),
    Close,
    /// Клавиша не меню.
    Ignored,
}

impl Menu {
    /// Подпись строки.
    fn c_caption(&self, node: Node) -> String {
        match node {
            Node::Folder(n) => String::from(startapps::FOLDERS[n].caption),
            Node::Item(index) => String::from(self.items[index].caption(&self.packages)),
            Node::Recent(n) => self.recent.get(n).map_or_else(String::new, |(_, path)| String::from(path.rsplit('/').next().unwrap_or(path))),
            Node::Setting(n) => String::from(SETTINGS[n].0),
            Node::Empty => String::from("(пусто)"),
            other => String::from(root_caption(other)),
        }
    }

    /// Что запускает строка (`None` — подменю, черта, «пусто»).
    fn c_choice(&self, node: Node) -> Option<Choice> {
        Some(match node {
            // «Поиск» — «Этот компьютер» в «Файлах»: там поле поиска, как
            // «Найти → Файлы и папки» у Windows 98.
            Node::Find => Choice::Program("files", "computer:"),
            // Справочной программы нет — «О системе» (версия, сборка, ссылки).
            Node::Help => Choice::App(App::About),
            Node::Run => Choice::Program("run", ""),
            // Вопрос «Завершение работы» задаёт стол (`App::confirms_power`).
            Node::Shutdown => Choice::App(App::Shutdown),
            Node::Item(index) => return self.items.get(index)?.choice(&self.packages),
            Node::Recent(n) => {
                let (command, path) = self.recent.get(n)?;
                Choice::Recent(command.clone(), path.clone())
            }
            Node::Setting(n) => match n {
                0 => Choice::App(App::Settings),
                1 => Choice::Settings(Section::Dock),
                2 => Choice::Settings(Section::Appearance),
                3 => Choice::Settings(Section::Wallpaper),
                4 => Choice::Program("admin", ""),
                5 => Choice::Program("taskmgr", ""),
                _ => Choice::Program("devmgr", ""),
            },
            _ => return None,
        })
    }

    /// Имя строки в журнале (`start spot c:<имя>`): по нему целится стенд.
    fn c_key(&self, node: Node) -> String {
        match node {
            Node::Folder(n) => alloc::format!("c:{}", startapps::FOLDERS[n].key),
            Node::Item(index) => match self.windows_file(index) {
                Some(_) => alloc::format!("c:win:{}", self.items[index].caption(&self.packages)),
                None => alloc::format!("c:{}", self.items[index].key(&self.packages)),
            },
            Node::Recent(n) => alloc::format!("c:recent:{n}"),
            Node::Setting(n) => alloc::format!("c:setting:{n}"),
            Node::Programs => String::from("c:programs"),
            Node::Documents => String::from("c:documents"),
            Node::Settings => String::from("c:settings"),
            Node::Find => String::from("c:find"),
            Node::Help => String::from("c:help"),
            Node::Run => String::from("c:run"),
            Node::Shutdown => String::from("c:shutdown"),
            Node::Empty => String::from("c:empty"),
            Node::Sep => String::from("c:sep"),
        }
    }

    /// Строки подменю строки `node`.
    fn c_rows(&self, node: Node) -> Vec<Node> {
        let by_caption = |menu: &Self, list: &mut Vec<Node>| {
            list.sort_by_cached_key(|node| menu.c_caption(*node).to_lowercase());
        };
        let mut rows = Vec::new();
        match node {
            Node::Programs => {
                // Папки сверху, под ними программы — как у Windows 98.
                let mut folders: Vec<Node> = (0..startapps::FOLDERS.len()).filter(|n| self.homes.iter().any(|home| *home == startapps::FOLDERS[*n].key)).map(Node::Folder).collect();
                by_caption(self, &mut folders);
                let mut programs: Vec<Node> = (0..self.items.len()).filter(|index| !self.items[*index].danger() && self.home(*index).is_empty()).map(Node::Item).collect();
                by_caption(self, &mut programs);
                rows.extend(folders);
                rows.extend(programs);
            }
            Node::Folder(n) => {
                let key = startapps::FOLDERS[n].key;
                rows = (0..self.items.len()).filter(|index| self.home(*index) == key).map(Node::Item).collect();
                by_caption(self, &mut rows);
            }
            Node::Documents => rows = (0..self.recent.len().min(RECENT_MAX)).map(Node::Recent).collect(),
            Node::Settings => rows = alloc::vec![Node::Setting(0), Node::Setting(1), Node::Setting(2), Node::Setting(3), Node::Sep, Node::Setting(4), Node::Setting(5), Node::Setting(6)],
            _ => {}
        }
        if rows.is_empty() {
            rows.push(Node::Empty);
        }
        rows
    }

    /// Значок строки в квадрат `rect`.
    fn c_icon(&self, ctx: Ctx, s: &mut Surface, rect: Rect, node: Node) {
        let named = |name: &'static str, icon: Icon| Art::Named(name, icon, Tone::Accent);
        let art = match node {
            Node::Programs => named("folder-full", Icon::Folder),
            Node::Documents => named("folder-documents", Icon::Folder),
            Node::Settings => named("settings", Icon::Settings),
            Node::Find => named("search", Icon::Search),
            Node::Help => named("dlg-question", Icon::Info),
            Node::Run => named("run", Icon::Prompt),
            Node::Shutdown => named("power", Icon::Power),
            Node::Folder(n) => {
                if startapps::FOLDERS[n].key == startapps::WINDOWS_FOLDER {
                    named("windows-apps", Icon::Folder)
                } else {
                    named("folder", Icon::Folder)
                }
            }
            Node::Item(index) => {
                draw_item_icon(ctx, s, rect, self.items[index], &self.packages);
                return;
            }
            Node::Recent(n) => {
                let name = self.recent.get(n).map_or("", |(_, path)| path.rsplit('/').next().unwrap_or(path));
                match super::super::appicon::file_icon(name, false) {
                    Some(image) => image.draw(s, rect),
                    None => named("doc-text", Icon::File).draw(ctx, s, rect),
                }
                return;
            }
            Node::Setting(n) => named(SETTINGS[n].1, SETTINGS[n].2),
            Node::Empty | Node::Sep => return,
        };
        art.draw(ctx, s, rect);
    }

    /// Разложить строки по столбцам высотой не больше `room` (внутри кромки).
    /// Возвращает места строк и размер карточки.
    fn c_place(&self, ctx: Ctx, rows: &[Node], room: u32) -> (Vec<Rect>, u32, u32) {
        let face = ctx.face(Role::Body);
        let pad = ctx.px(PAD);
        let col_w = rows.iter().map(|node| face.width(&self.c_caption(*node))).max().unwrap_or(0) + ctx.px(4 + SUB_ICON + 8 + ARROW_W + 8);
        let col_w = col_w.max(ctx.px(120)).min(ctx.px(420));
        let mut places = Vec::with_capacity(rows.len());
        let (mut col, mut y, mut tallest) = (0u32, 0u32, 0u32);
        for node in rows {
            let h = row_h(ctx, *node, false);
            if y + h > room && y > 0 {
                col += 1;
                y = 0;
            }
            places.push(Rect::new((pad + col * col_w) as i32, (pad + y) as i32, col_w, h));
            y += h;
            tallest = tallest.max(y);
        }
        (places, pad * 2 + col_w * (col + 1), pad * 2 + tallest)
    }

    /// Места строк корня.
    fn c_root_places(&self, ctx: Ctx) -> Vec<Rect> {
        let pad = ctx.px(PAD);
        let x = (pad + banner_w(ctx)) as i32;
        let w = self.surface.width().saturating_sub(pad * 2 + banner_w(ctx));
        let mut y = pad as i32;
        let mut out = Vec::with_capacity(self.cascade.root.len());
        for node in &self.cascade.root {
            let h = row_h(ctx, *node, true);
            out.push(Rect::new(x, y, w, h));
            y += h as i32;
        }
        out
    }

    /// Нарисовать весь классический «Пуск»: корень и открытые подменю.
    pub(super) fn c_redraw(&mut self) {
        self.c_draw_root();
        for k in 0..self.cascade.subs.len() {
            self.c_redraw_sub(k);
        }
    }

    /// Нарисовать корень.
    fn c_draw_root(&mut self) {
        let ctx = desk_ctx(self.scale).on(win98::FACE);
        self.cascade.root_places = self.c_root_places(ctx);
        let Some(placeholder) = Surface::new(1, 1, win98::FACE) else { return };
        let mut surface = core::mem::replace(&mut self.surface, placeholder);
        let card = surface.bounds();
        let t = c98::unit(self.scale);
        c98::panel(&mut surface, card, t);
        // Полоса слева: сверху вниз от тёмно-синего к синему, надпись снизу
        // вверх.
        let pad = ctx.px(PAD);
        let banner = Rect::new(pad as i32, pad as i32, banner_w(ctx), card.h.saturating_sub(pad * 2));
        for row in 0..banner.h {
            let weight = (row * 255 / banner.h.saturating_sub(1).max(1)).min(255) as u8;
            surface.fill(Rect::new(banner.x, banner.y + row as i32, banner.w, 1), win98::NAVY.mix(win98::CAPTION2, weight));
        }
        banner_text(ctx, &mut surface, banner);
        let open = self.cascade.subs.first().map(|sub| sub.node);
        for (n, (node, place)) in self.cascade.root.iter().zip(self.cascade.root_places.iter()).enumerate() {
            let hot = self.cascade.root_hot == Some(n) || (open == Some(*node) && node.opens());
            let caption = self.c_caption(*node);
            self.c_row(ctx, &mut surface, *place, *node, &caption, hot, true);
        }
        self.surface = surface;
        self.damage = card;
    }

    /// Нарисовать подменю `k`.
    fn c_redraw_sub(&mut self, k: usize) {
        let ctx = desk_ctx(self.scale).on(win98::FACE);
        let t = c98::unit(self.scale);
        let Some(placeholder) = Surface::new(1, 1, win98::FACE) else { return };
        let Some(sub) = self.cascade.subs.get_mut(k) else { return };
        let mut surface = core::mem::replace(&mut sub.surface, placeholder);
        let rows = sub.rows.clone();
        let places = sub.places.clone();
        let hot = sub.hot;
        let rect = sub.rect;
        let next = self.cascade.subs.get(k + 1).map(|sub| sub.node);
        let card = surface.bounds();
        c98::panel(&mut surface, card, t);
        for (n, (node, place)) in rows.iter().zip(places.iter()).enumerate() {
            let lit = hot == Some(n) || (next == Some(*node) && node.opens());
            let caption = self.c_caption(*node);
            self.c_row(ctx, &mut surface, *place, *node, &caption, lit, false);
        }
        self.cascade.subs[k].surface = surface;
        self.cascade.damage = self.cascade.damage.union(&rect);
    }

    /// Одна строка: подсветка, значок, подпись, стрелка подменю.
    #[allow(clippy::too_many_arguments)]
    fn c_row(&self, ctx: Ctx, s: &mut Surface, place: Rect, node: Node, caption: &str, hot: bool, root: bool) {
        let t = c98::unit(self.scale);
        if node == Node::Sep {
            c98::groove_h(s, place.x + ctx.px(2) as i32, place.y + (place.h as i32 / 2) - t as i32, place.w.saturating_sub(ctx.px(4)), t);
            return;
        }
        let lit = hot && node.selectable();
        if lit {
            c98::selection(s, place);
        }
        let side = ctx.px(if root { ROOT_ICON } else { SUB_ICON });
        let icon = Rect::new(place.x + ctx.px(if root { 4 } else { 3 }) as i32, place.y + (place.h as i32 - side as i32) / 2, side, side);
        self.c_icon(ctx.on(if lit { win98::NAVY } else { win98::FACE }), s, icon, node);
        let x = icon.right() + ctx.px(if root { 10 } else { 6 }) as i32;
        let room = (place.right() - x - ctx.px(ARROW_W) as i32).max(0) as u32;
        let ink = if !node.selectable() {
            win98::SHADE
        } else if lit {
            Color::rgb(0xFF, 0xFF, 0xFF)
        } else {
            Color::rgb(0, 0, 0)
        };
        let tctx = ctx.on(if lit { win98::NAVY } else { win98::FACE });
        if !node.selectable() {
            // Серая надпись Windows 98 — с белым оттиском на точку ниже.
            paint::text_clipped(tctx, s, Role::Body, x + 1, paint::baseline(tctx, Role::Body, place) + 1, room, caption, win98::LIGHT);
        }
        paint::text_clipped(tctx, s, Role::Body, x, paint::baseline(tctx, Role::Body, place), room, caption, ink);
        if node.opens() {
            let side = ctx.px(16);
            let arrow = Rect::new(place.right() - ctx.px(4) as i32 - side as i32, place.y + (place.h as i32 - side as i32) / 2, side, side);
            c98::triangle(s, arrow, false, true, ink);
        }
    }

    /// Открыть подменю строки `row` уровня `level` (0 — корень). Уже открытое
    /// той же строкой — оставить.
    fn c_open(&mut self, level: usize, row: usize) {
        let ctx = desk_ctx(self.scale);
        let (node, parent) = if level == 0 {
            let Some(node) = self.cascade.root.get(row).copied() else { return };
            let Some(place) = self.cascade.root_places.get(row).copied() else { return };
            (node, place.translate(self.rect.x, self.rect.y))
        } else {
            let Some(sub) = self.cascade.subs.get(level - 1) else { return };
            let Some(node) = sub.rows.get(row).copied() else { return };
            (node, sub.places[row].translate(sub.rect.x, sub.rect.y))
        };
        if !node.opens() {
            self.c_close_from(level);
            return;
        }
        if self.cascade.subs.get(level).is_some_and(|sub| sub.node == node) {
            return;
        }
        self.c_close_from(level);
        let rows = self.c_rows(node);
        let work = self.cascade.work;
        let room = work.h.saturating_sub(ctx.px(PAD * 2));
        let (places, w, h) = self.c_place(ctx, &rows, room);
        // Правее родителя, первой строкой вровень с ним; не влезает справа —
        // левее; снизу — подняться.
        let owner = if level == 0 { self.rect } else { self.cascade.subs[level - 1].rect };
        let mut x = owner.right() - ctx.px(3) as i32;
        if x + w as i32 > work.right() {
            x = (owner.x - w as i32 + ctx.px(3) as i32).max(work.x);
        }
        let mut y = parent.y - ctx.px(PAD) as i32;
        if y + h as i32 > work.bottom() {
            y = work.bottom() - h as i32;
        }
        y = y.max(work.y);
        let Some(surface) = Surface::new(w, h, win98::FACE) else { return };
        let rect = Rect::new(x, y, w, h);
        crate::kprintln!("  desktop     : start submenu {} '{}' at {},{} {}x{}", level + 1, self.c_caption(node), x, y, w, h);
        self.cascade.subs.push(Sub { node, rows, places, rect, surface, hot: None });
        let k = self.cascade.subs.len() - 1;
        self.c_redraw_sub(k);
        if level > 0 {
            self.c_redraw_sub(level - 1);
        }
        self.c_log_sub(k);
    }

    /// Закрыть подменю начиная с `level` (0 — все).
    fn c_close_from(&mut self, level: usize) {
        while self.cascade.subs.len() > level {
            if let Some(sub) = self.cascade.subs.pop() {
                self.cascade.damage = self.cascade.damage.union(&sub.rect);
            }
        }
        if self.cascade.level > level {
            self.cascade.level = level;
        }
    }

    /// Напечатать места строк подменю `k` (стенд целится в них).
    fn c_log_sub(&self, k: usize) {
        let Some(sub) = self.cascade.subs.get(k) else { return };
        for (node, place) in sub.rows.iter().zip(sub.places.iter()) {
            if node.selectable() {
                let at = place.translate(sub.rect.x, sub.rect.y);
                crate::kprintln!("  desktop     : start spot {} at {},{} {}x{}", self.c_key(*node), at.x, at.y, at.w, at.h);
            }
        }
    }

    /// Места строк корня — для журнала при открытии.
    pub(super) fn c_spots(&self) -> Vec<String> {
        let ctx = desk_ctx(self.scale);
        let mut out = alloc::vec![alloc::format!("menu at {},{} {}x{}", self.rect.x, self.rect.y, self.rect.w, self.rect.h)];
        for (node, place) in self.cascade.root.iter().zip(self.c_root_places(ctx).iter()) {
            if node.selectable() {
                let at = place.translate(self.rect.x, self.rect.y);
                out.push(alloc::format!("{} at {},{} {}x{}", self.c_key(*node), at.x, at.y, at.w, at.h));
            }
        }
        out
    }

    /// Что под точкой экрана: уровень (0 — корень) и строка, если попали в
    /// строку. Подменю — первыми (они лежат поверх).
    fn c_hit(&self, x: i32, y: i32) -> Option<(usize, Option<usize>)> {
        for (k, sub) in self.cascade.subs.iter().enumerate().rev() {
            if sub.rect.contains(x, y) {
                let (lx, ly) = (x - sub.rect.x, y - sub.rect.y);
                return Some((k + 1, sub.places.iter().position(|place| place.contains(lx, ly))));
            }
        }
        if self.rect.contains(x, y) {
            let (lx, ly) = (x - self.rect.x, y - self.rect.y);
            return Some((0, self.cascade.root_places.iter().position(|place| place.contains(lx, ly))));
        }
        None
    }

    fn c_node(&self, level: usize, row: usize) -> Option<Node> {
        if level == 0 { self.cascade.root.get(row).copied() } else { self.cascade.subs.get(level - 1)?.rows.get(row).copied() }
    }

    fn c_hot(&self, level: usize) -> Option<usize> {
        if level == 0 { self.cascade.root_hot } else { self.cascade.subs.get(level - 1)?.hot }
    }

    /// Подсветить строку `row` уровня `level`; `true` — изменилось.
    fn c_set_hot(&mut self, level: usize, row: Option<usize>) -> bool {
        if self.c_hot(level) == row {
            return false;
        }
        if level == 0 {
            self.cascade.root_hot = row;
        } else if let Some(sub) = self.cascade.subs.get_mut(level - 1) {
            sub.hot = row;
        }
        true
    }

    /// Перерисовать уровень `level`.
    fn c_repaint(&mut self, level: usize) {
        if level == 0 {
            self.c_draw_root();
        } else {
            self.c_redraw_sub(level - 1);
        }
    }

    /// Указатель над классическим «Пуском».
    pub(super) fn c_hover(&mut self, x: i32, y: i32) -> bool {
        let Some((level, row)) = self.c_hit(x, y) else { return false };
        let Some(row) = row else { return false };
        let Some(node) = self.c_node(level, row) else { return false };
        if !node.selectable() {
            return false;
        }
        let mut changed = self.c_set_hot(level, Some(row));
        self.cascade.level = level;
        let had = self.cascade.subs.len();
        let shown = self.cascade.subs.get(level).map(|sub| sub.node);
        if node.opens() {
            if shown != Some(node) {
                // Подсветка в глубине, оставленной указателем, гаснет.
                self.c_open(level, row);
                changed = true;
            }
        } else if had > level {
            self.c_close_from(level);
            changed = true;
        }
        if changed {
            self.c_repaint(level);
        }
        changed
    }

    /// Щелчок по классическому «Пуску».
    pub(super) fn c_click(&mut self, x: i32, y: i32) -> MenuClick {
        let Some((level, row)) = self.c_hit(x, y) else { return MenuClick::Close };
        let Some(node) = row.and_then(|row| self.c_node(level, row)) else { return MenuClick::Stay };
        if node.opens() {
            if let Some(row) = row {
                self.c_set_hot(level, Some(row));
                self.c_open(level, row);
                self.c_repaint(level);
            }
            return MenuClick::Stay;
        }
        match self.c_choice(node) {
            Some(choice) => {
                crate::kprintln!("  desktop     : start item '{}' chosen", self.c_caption(node));
                MenuClick::Launch(choice)
            }
            None => MenuClick::Stay,
        }
    }

    /// Клавиша в классическом «Пуске».
    pub fn classic_key(&mut self, code: crate::input::KeyCode, ch: Option<char>) -> ClassicKey {
        use crate::input::KeyCode;
        if !self.cascade.on || !self.open {
            return ClassicKey::Ignored;
        }
        let level = self.cascade.level.min(self.cascade.subs.len());
        match code {
            KeyCode::Up | KeyCode::Down => {
                self.c_step(level, code == KeyCode::Down);
                ClassicKey::Stay
            }
            KeyCode::Right => {
                self.c_enter(level, false);
                ClassicKey::Stay
            }
            KeyCode::Enter => self.c_enter(level, true),
            KeyCode::Left | KeyCode::Escape if level > 0 => {
                self.c_close_from(level - 1);
                self.cascade.level = level - 1;
                self.c_repaint(level - 1);
                ClassicKey::Stay
            }
            KeyCode::Left => ClassicKey::Stay,
            KeyCode::Escape | KeyCode::LeftMeta | KeyCode::RightMeta | KeyCode::F1 => ClassicKey::Close,
            _ => match ch.filter(|ch| !ch.is_control() && *ch != ' ') {
                Some(ch) => self.c_letter(level, ch),
                None => ClassicKey::Ignored,
            },
        }
    }

    fn c_rows_at(&self, level: usize) -> Vec<Node> {
        if level == 0 { self.cascade.root.clone() } else { self.cascade.subs.get(level - 1).map_or_else(Vec::new, |sub| sub.rows.clone()) }
    }

    /// ↑/↓ по строкам уровня, по кругу, мимо черт.
    fn c_step(&mut self, level: usize, down: bool) {
        let rows = self.c_rows_at(level);
        let count = rows.len();
        if count == 0 || !rows.iter().any(|node| node.selectable()) {
            return;
        }
        let mut at = match self.c_hot(level) {
            Some(row) => row,
            None if down => count - 1,
            None => 0,
        };
        loop {
            at = if down { (at + 1) % count } else { (at + count - 1) % count };
            if rows[at].selectable() {
                break;
            }
        }
        self.c_set_hot(level, Some(at));
        // Подменю глубже выбранного закрываются: стрелки ушли с его строки.
        self.c_close_from(level);
        self.cascade.level = level;
        self.c_repaint(level);
    }

    /// → / Enter: раскрыть подменю (и встать на его первую строку); Enter на
    /// программе — запустить.
    fn c_enter(&mut self, level: usize, launch: bool) -> ClassicKey {
        let Some(row) = self.c_hot(level) else {
            if launch {
                return ClassicKey::Stay;
            }
            self.c_step(level, true);
            return ClassicKey::Stay;
        };
        let Some(node) = self.c_node(level, row) else { return ClassicKey::Stay };
        if node.opens() {
            self.c_open(level, row);
            if let Some(sub) = self.cascade.subs.get_mut(level) {
                sub.hot = sub.rows.iter().position(|node| node.selectable());
            }
            self.cascade.level = (level + 1).min(self.cascade.subs.len());
            self.c_repaint(level);
            if self.cascade.subs.len() > level {
                self.c_redraw_sub(level);
            }
            return ClassicKey::Stay;
        }
        if !launch {
            return ClassicKey::Stay;
        }
        match self.c_choice(node) {
            Some(choice) => {
                crate::kprintln!("  desktop     : start item '{}' chosen", self.c_caption(node));
                ClassicKey::Launch(choice)
            }
            None => ClassicKey::Stay,
        }
    }

    /// Буква: к следующей строке уровня на неё; единственная — выбрать.
    fn c_letter(&mut self, level: usize, ch: char) -> ClassicKey {
        let rows = self.c_rows_at(level);
        let wanted: String = ch.to_lowercase().collect();
        let starts = |node: &Node| node.selectable() && self.c_caption(*node).to_lowercase().starts_with(wanted.as_str());
        let matches: Vec<usize> = rows.iter().enumerate().filter(|(_, node)| starts(node)).map(|(n, _)| n).collect();
        let Some(first) = matches.first().copied() else { return ClassicKey::Stay };
        let after = self.c_hot(level).and_then(|hot| matches.iter().copied().find(|n| *n > hot)).unwrap_or(first);
        self.c_set_hot(level, Some(after));
        self.c_close_from(level);
        self.cascade.level = level;
        self.c_repaint(level);
        if matches.len() == 1 {
            return self.c_enter(level, true);
        }
        ClassicKey::Stay
    }
}

/// «FossAi» на синей полосе снизу вверх: «Foss» полужирным белым, «Ai» —
/// светлее, как «Windows98» у Windows 98. Надпись набирается вдоль во
/// вспомогательной поверхности (белым по чёрному) и переносится поворотом
/// на четверть оборота против часовой, яркость — как непрозрачность.
fn banner_text(ctx: Ctx, s: &mut Surface, banner: Rect) {
    let bold = ctx.face(Role::Heading);
    let thin = ctx.face(Role::Title);
    let (first, second) = ("Foss", "Ai");
    let len = bold.width(first) + thin.width(second) + ctx.px(2);
    let line = u32::from(bold.line);
    let Some(mut strip) = Surface::new(len.max(1), line.max(1), Color::rgb(0, 0, 0)) else { return };
    let black = ctx.on(Color::rgb(0, 0, 0));
    let used = paint::text(black, &mut strip, Role::Heading, 0, 0, first, Color::rgb(0xFF, 0xFF, 0xFF));
    let thin_top = (i32::from(bold.line) - i32::from(thin.line)) / 2 + ctx.px(1) as i32;
    paint::text(black, &mut strip, Role::Title, used as i32 + ctx.px(2) as i32, thin_top.max(0), second, Color::rgb(0xC0, 0xC0, 0xC0));
    // Низ надписи — у низа полосы с полем в 6 точек; верх букв смотрит влево.
    let left = banner.x + (banner.w as i32 - line as i32) / 2;
    let bottom = banner.bottom() - ctx.px(6) as i32;
    let bounds = s.bounds();
    for sx in 0..strip.width() {
        let y = bottom - 1 - sx as i32;
        if y < banner.y || y < 0 || y >= bounds.h as i32 {
            continue;
        }
        for sy in 0..strip.height() {
            let x = left + sy as i32;
            if x < banner.x || x >= banner.right() || x < 0 || x >= bounds.w as i32 {
                continue;
            }
            let src = Color::from_pixel(strip.get(sx, sy));
            let alpha = src.r.max(src.g).max(src.b);
            if alpha == 0 {
                continue;
            }
            let under = Color::from_pixel(s.get(x as u32, y as u32));
            let lit = Color::rgb(src.r, src.g, src.b);
            s.put(x as u32, y as u32, under.mix(lit, alpha).pixel());
        }
    }
}
