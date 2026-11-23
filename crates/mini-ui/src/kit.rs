//! Общий набор элементов программ с окнами (фаза С8).
//!
//! # Откуда он взялся
//!
//! «Файлы», диспетчер задач и диспетчер устройств писались по очереди, и каждый
//! следующий начинался с копии предыдущего: панель сверху, строка состояния
//! снизу, таблица со столбцами, которые узкое окно теряет справа, карточка
//! контекстного меню с тенью и разделителями групп, переключатель вкладок в
//! лунке. Копии уже начали расходиться — меню «Файлов» и диспетчера задач было
//! разной ширины при одинаковых пунктах, а пометку о строках за краем получил
//! только диспетчер устройств. Здесь то, что было общим по смыслу, стало общим
//! по коду.
//!
//! # Что это и чего здесь нет
//!
//! Тот же немедленный режим, что у [`crate::paint`]: функции кладут элемент на
//! поверхность и считают геометрию, а состояния у элементов нет — выбранная
//! строка, открытое меню и вкладка живут в программе. Поэтому и дерева
//! элементов, и обработчиков событий здесь нет: попадание щелчком — такая же
//! функция от прямоугольника и точки, как отрисовка.
//!
//! Системных вызовов тоже нет: модуль рисует, а окно и цикл событий — у
//! библиотеки программ (`user_progs::app`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;

use crate::glyphicon::{self, Icon};
use crate::iconset::{self, Fi};
use crate::paint::{self, Ctx, RowState};
use crate::typeface::Role;
use crate::{Rect, Surface, classic, draw, theme};

// ---------------------------------------------------------------------------
// Панели
// ---------------------------------------------------------------------------

/// Окно, разрезанное на панель сверху, тело и строку состояния снизу.
#[derive(Clone, Copy)]
pub struct Frame {
    pub toolbar: Rect,
    pub body: Rect,
    pub status: Rect,
}

impl Frame {
    #[must_use]
    pub fn new(ctx: Ctx, area: Rect) -> Self {
        let toolbar_h = ctx.px(theme::TOOLBAR_H).min(area.h);
        let toolbar = Rect::new(area.x, area.y, area.w, toolbar_h);
        let status_h = ctx.px(theme::STATUS_H);
        let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
        let status = Rect::new(area.x, status_y, area.w, status_h);
        let body = Rect::new(area.x, toolbar.bottom(), area.w, (status.y - toolbar.bottom()).max(0) as u32);
        Self { toolbar, body, status }
    }
}

/// Залить панель и провести под ней линию. Возвращает контекст **поверх
/// панели**: всё, что лежит на ней, сводится с её цветом, а не с цветом окна —
/// разница в один-два уровня на снимке видна как кнопка чуть другого оттенка.
pub fn toolbar(ctx: Ctx, s: &mut Surface, rect: Rect) -> Ctx {
    let p = ctx.palette;
    if !rect.is_empty() {
        s.fill(rect, ctx.flat(p.panel));
        if theme::is_classic() {
            // Панель инструментов Windows 98 — бороздка снизу.
            let t = classic::unit(ctx.scale);
            classic::groove_h(s, rect.x, rect.bottom() - 2 * t as i32, rect.w, t);
        } else {
            draw::hline(s, rect.x, rect.bottom() - 1, rect.w, p.line.color, p.line.alpha);
        }
    }
    ctx.on(theme::panel_bg())
}

/// Заголовок панели слева.
pub fn toolbar_title(bar: Ctx, s: &mut Surface, rect: Rect, title: &str) {
    let pad = bar.px(16);
    paint::text_clipped(
        bar,
        s,
        Role::Title,
        rect.x + pad as i32,
        paint::baseline(bar, Role::Title, rect),
        rect.w / 2,
        title,
        bar.palette.ink2,
    );
}

/// Строка состояния: линия сверху и одна фраза.
pub fn status_bar(ctx: Ctx, s: &mut Surface, rect: Rect, text: &str) {
    let p = ctx.palette;
    if rect.is_empty() {
        return;
    }
    s.fill(rect, ctx.flat(p.panel));
    if theme::is_classic() {
        let t = classic::unit(ctx.scale);
        classic::shallow(s, Rect::new(rect.x + 2 * t as i32, rect.y + 2 * t as i32, rect.w.saturating_sub(4 * t), rect.h.saturating_sub(3 * t)), t);
    } else {
        draw::hline(s, rect.x, rect.y, rect.w, p.line2.color, p.line2.alpha);
    }
    let bar = ctx.on(theme::panel_bg());
    let pad = ctx.px(14);
    paint::text_clipped(
        bar,
        s,
        Role::MonoSmall,
        rect.x + pad as i32,
        paint::baseline(bar, Role::MonoSmall, rect),
        rect.w.saturating_sub(pad * 2),
        text,
        p.ink4,
    );
}

// ---------------------------------------------------------------------------
// Вкладки
// ---------------------------------------------------------------------------

/// Переключатель вкладок: лунка на панели и вкладки в ней.
pub struct Tabs {
    pub frame: Rect,
    pub tabs: Vec<Rect>,
}

impl Tabs {
    /// Разложить вкладки заданной ширины у левого края `toolbar`, начиная с `x`.
    #[must_use]
    pub fn new(ctx: Ctx, toolbar: Rect, x: i32, widths: &[u32]) -> Self {
        let tab_h = ctx.px(26);
        let box_h = ctx.px(32);
        let box_w = widths.iter().sum::<u32>() + ctx.px(6);
        let box_y = toolbar.y + (toolbar.h as i32 - box_h as i32) / 2;
        let frame = Rect::new(x, box_y, box_w, box_h);
        let tab_y = box_y + (box_h as i32 - tab_h as i32) / 2;
        let mut tabs = Vec::with_capacity(widths.len());
        let mut left = x + ctx.px(3) as i32;
        for w in widths {
            tabs.push(Rect::new(left, tab_y, *w, tab_h));
            left += *w as i32;
        }
        Self { frame, tabs }
    }

    /// Какая вкладка под точкой.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<usize> {
        self.tabs.iter().position(|rect| rect.contains(x, y))
    }

    /// Нарисовать поверх панели (`bar` — контекст от [`toolbar`]).
    pub fn draw(&self, bar: Ctx, s: &mut Surface, labels: &[&str], active: usize) {
        let p = bar.palette;
        if theme::is_classic() {
            // Переключатель — ряд кнопок; выбранная вдавлена (как
            // кнопки-переключатели панели инструментов Windows 98).
            let t = classic::unit(bar.scale);
            for (index, (rect, label)) in self.tabs.iter().zip(labels).enumerate() {
                let on = index == active;
                classic::button(s, *rect, t, on);
                let area = if on { rect.translate(t as i32, t as i32) } else { *rect };
                paint::text_centered(bar, s, Role::Caption, area, label, p.ink);
            }
            return;
        }
        paint::sunk(bar, s, self.frame, bar.px(theme::R_ROW));
        for (index, (rect, label)) in self.tabs.iter().zip(labels).enumerate() {
            let on = index == active;
            if on {
                draw::rounded(s, *rect, bar.px(theme::R_TAB), bar.flat(p.btn), 255);
                draw::rounded_stroke(s, *rect, bar.px(theme::R_TAB), p.btnline.color, p.btnline.alpha);
            }
            paint::text_centered(bar, s, Role::Caption, *rect, label, if on { p.ink2 } else { p.ink4 });
        }
    }
}

// ---------------------------------------------------------------------------
// Таблица
// ---------------------------------------------------------------------------

/// Таблица: шапка столбцов и строки под ней.
#[derive(Clone, Copy)]
pub struct Table {
    pub header: Rect,
    pub list: Rect,
    pub row_h: u32,
}

impl Table {
    /// Таблица на всё `body`.
    #[must_use]
    pub fn new(ctx: Ctx, body: Rect) -> Self {
        let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(body.h);
        let header = Rect::new(body.x, body.y, body.w, header_h);
        let list = Rect::new(body.x, header.bottom(), body.w, body.h.saturating_sub(header_h));
        Self { header, list, row_h: ctx.px(32) }
    }

    /// Та же таблица с другой высотой строки.
    #[must_use]
    pub const fn with_row(mut self, row_h: u32) -> Self {
        self.row_h = row_h;
        self
    }

    /// Та же таблица, но с полосой снизу под кнопки высотой `h`.
    #[must_use]
    pub fn leave_bottom(mut self, h: u32) -> Self {
        self.list.h = self.list.h.saturating_sub(h);
        self
    }

    /// Сколько строк помещается.
    #[must_use]
    pub fn visible(&self) -> usize {
        (self.list.h / self.row_h.max(1)) as usize
    }

    /// Сколько строк показывается из `rows`: когда все не помещаются, последняя
    /// отдана пометке о скрытых ([`Self::draw_overflow`]).
    #[must_use]
    pub fn shown(&self, rows: usize) -> usize {
        let visible = self.visible();
        if rows > visible { visible.saturating_sub(1) } else { visible }
    }

    /// Прямоугольник строки номер `offset` от первой показанной.
    #[must_use]
    pub fn row_rect(&self, ctx: Ctx, offset: usize) -> Rect {
        let pad = ctx.px(12);
        Rect::new(
            self.list.x + pad as i32,
            self.list.y + (offset as u32 * self.row_h) as i32,
            self.list.w.saturating_sub(pad * 2),
            self.row_h.saturating_sub(ctx.px(2)),
        )
    }

    /// Номер показанной строки под точкой, если она среди `shown` первых.
    #[must_use]
    pub fn offset_at(&self, x: i32, y: i32, shown: usize) -> Option<usize> {
        if !self.list.contains(x, y) {
            return None;
        }
        let offset = ((y - self.list.y) / self.row_h.max(1) as i32).max(0) as usize;
        (offset < shown).then_some(offset)
    }

    /// Столбцы: первый тянется, остальные — своей ширины.
    ///
    /// Узкое окно теряет столбцы **справа налево**, а не сжимает все разом:
    /// сжатые столбцы обрезают текст в каждом, потерянный — ни в одном. Поэтому
    /// `widths` перечисляются по важности: то, без чего окно бессмысленно, —
    /// левее. `widths[0]` не читается — первый столбец получает остаток, но не
    /// меньше `first_min`.
    #[must_use]
    pub fn columns(&self, ctx: Ctx, widths: &[u32], first_min: u32) -> Vec<Rect> {
        let left = self.list.x + ctx.px(24) as i32;
        let right = self.list.right() - ctx.px(22) as i32;
        let mut used = 0u32;
        let mut shown = 1;
        for width in widths.iter().skip(1) {
            if left + (first_min + used + width) as i32 > right {
                break;
            }
            used += width;
            shown += 1;
        }
        let mut out = Vec::with_capacity(widths.len());
        let mut x = right - used as i32;
        out.push(Rect::new(left, self.list.y, (x - left).max(0) as u32, self.list.h));
        for width in &widths[1..] {
            if out.len() < shown {
                out.push(Rect::new(x, self.list.y, *width, self.list.h));
                x += *width as i32;
            } else {
                out.push(Rect::EMPTY);
            }
        }
        out
    }

    /// Шапка: подписи столбцов и линия под ними.
    pub fn draw_header(&self, ctx: Ctx, s: &mut Surface, columns: &[Rect], titles: &[&str]) {
        let p = ctx.palette;
        let y = paint::baseline(ctx, Role::MonoCaps, self.header);
        if theme::is_classic() {
            // Шапка списка Windows 98: ряд выпуклых кнопок-столбцов.
            let t = classic::unit(ctx.scale);
            let shown: Vec<Rect> = columns.iter().copied().filter(|rect| rect.w > 0).collect();
            for (index, rect) in shown.iter().enumerate() {
                let left = if index == 0 { self.header.x } else { rect.x - ctx.px(8) as i32 };
                let right = shown.get(index + 1).map_or(self.header.right(), |next| next.x - ctx.px(8) as i32);
                let cell = Rect::new(left, self.header.y, (right - left).max(0) as u32, self.header.h);
                classic::button(s, cell, t, false);
            }
            for (rect, title) in columns.iter().zip(titles) {
                if rect.w > 0 {
                    paint::text(ctx, s, Role::Caption, rect.x, paint::baseline(ctx, Role::Caption, self.header), title, p.ink);
                }
            }
            return;
        }
        for (rect, title) in columns.iter().zip(titles) {
            if rect.w > 0 {
                paint::caps(ctx, s, rect.x, y, title);
            }
        }
        draw::hline(
            s,
            self.header.x + ctx.px(12) as i32,
            self.header.bottom() - 1,
            self.header.w.saturating_sub(ctx.px(24)),
            p.line.color,
            p.line.alpha,
        );
    }

    /// Пометка о строках за краем — в строке номер `shown`.
    ///
    /// Строки за краем названы, а не молча отрезаны: на снимке диспетчера
    /// устройств заголовок группы «ДИСКИ» стоял последней видимой строкой, а
    /// сам диск — под краем, и окно читалось как «диск не найден».
    pub fn draw_overflow(&self, ctx: Ctx, s: &mut Surface, rows: usize, first: usize, shown: usize) {
        if rows <= self.visible() {
            return;
        }
        let below = rows.saturating_sub(first + shown);
        let rect = self.row_rect(ctx, shown);
        let text = alloc::format!("строк выше: {first}, ниже: {below} — стрелки, PageUp, PageDown");
        paint::text_clipped(
            ctx,
            s,
            Role::Caption,
            rect.x + ctx.px(6) as i32,
            paint::baseline(ctx, Role::Caption, rect),
            rect.w,
            &text,
            ctx.palette.ink3,
        );
    }
}

/// Первая показанная строка, при которой строка `selected` видна.
#[must_use]
pub const fn first_visible(selected: usize, shown: usize) -> usize {
    if shown == 0 || selected < shown { 0 } else { selected + 1 - shown }
}

/// Текст ячейки, обрезанный по столбцу.
pub fn cell(ctx: Ctx, s: &mut Surface, column: Rect, x: i32, row: Rect, role: Role, text: &str, ink: crate::Color) {
    if column.w == 0 {
        return;
    }
    let room = (column.right() - x - ctx.px(8) as i32).max(0) as u32;
    paint::text_clipped(ctx, s, role, x, paint::baseline(ctx, role, row), room, text, ink);
}

// ---------------------------------------------------------------------------
// Контекстное меню
// ---------------------------------------------------------------------------

/// Пункт меню.
#[derive(Clone, Copy)]
pub struct Entry<'a> {
    pub title: &'a str,
    pub icon: Option<Icon>,
    /// Опасное действие — красным: удалить, снять задачу.
    pub danger: bool,
    /// Номер группы. Между группами — разделитель.
    pub group: u8,
}

const MENU_PAD: u32 = 6;
const MENU_SEPARATOR: u32 = 9;

/// Размер карточки меню.
#[must_use]
pub fn menu_size(ctx: Ctx, entries: &[Entry<'_>]) -> (u32, u32) {
    let widest = entries.iter().map(|entry| ctx.face(Role::Body).width(entry.title)).max().unwrap_or(0);
    let mut h = ctx.px(MENU_PAD) * 2;
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 && entry.group != entries[index - 1].group {
            h += ctx.px(MENU_SEPARATOR);
        }
        h += ctx.px(theme::MENU_ROW_H);
    }
    (widest + ctx.px(14 + 14 + 10 + 12) + ctx.px(12), h)
}

/// Поставить карточку размером `size` у точки `at`, не выпуская из `area`.
#[must_use]
pub fn place(at: (i32, i32), size: (u32, u32), area: Rect) -> Rect {
    let x = at.0.clamp(area.x, (area.right() - size.0 as i32).max(area.x));
    let y = at.1.clamp(area.y, (area.bottom() - size.1 as i32).max(area.y));
    Rect::new(x, y, size.0, size.1)
}

/// Карточка поверх окна: тень, скругление, обводка. Возвращает контекст
/// поверх карточки — на ней рисуются и пункты меню, и поле переименования.
pub fn card(ctx: Ctx, s: &mut Surface, rect: Rect) -> Ctx {
    let p = ctx.palette;
    if theme::is_classic() {
        classic::panel(s, rect, classic::unit(ctx.scale));
        return ctx.on(theme::panel_bg());
    }
    draw::shadow(s, rect, ctx.px(theme::R_CARD), ctx.px(18), crate::Color::rgb(0, 0, 0), 90);
    draw::rounded(s, rect, ctx.px(theme::R_CARD), ctx.flat(p.panel), 255);
    draw::rounded_stroke(s, rect, ctx.px(theme::R_CARD), p.line3.color, p.line3.alpha);
    ctx.on(theme::panel_bg())
}

/// Прямоугольник пункта `index` в карточке `rect`.
fn entry_rect(ctx: Ctx, rect: Rect, entries: &[Entry<'_>], index: usize) -> Rect {
    let pad = ctx.px(MENU_PAD);
    let row_h = ctx.px(theme::MENU_ROW_H);
    let mut y = rect.y + pad as i32;
    for (at, entry) in entries.iter().enumerate().take(index + 1) {
        if at > 0 && entry.group != entries[at - 1].group {
            y += ctx.px(MENU_SEPARATOR) as i32;
        }
        if at < index {
            y += row_h as i32;
        }
    }
    Rect::new(rect.x + pad as i32, y, rect.w.saturating_sub(pad * 2), row_h)
}

/// Нарисовать меню с выбранным пунктом `selected`.
pub fn draw_menu(ctx: Ctx, s: &mut Surface, rect: Rect, entries: &[Entry<'_>], selected: usize) {
    let inner = card(ctx, s, rect);
    let p = inner.palette;
    for (index, entry) in entries.iter().enumerate() {
        let row = entry_rect(inner, rect, entries, index);
        if index > 0 && entry.group != entries[index - 1].group {
            let line_y = row.y - ctx.px(MENU_SEPARATOR) as i32 + ctx.px(4) as i32;
            if theme::is_classic() {
                classic::groove_h(s, rect.x + ctx.px(4) as i32, line_y, rect.w.saturating_sub(ctx.px(8)), classic::unit(ctx.scale));
            } else {
                paint::separator(inner, s, rect.x + ctx.px(12) as i32, line_y, rect.w.saturating_sub(ctx.px(24)));
            }
        }
        let on = index == selected;
        paint::row(inner, s, row, if on { RowState::Selected } else { RowState::Idle });
        let side = ctx.px(14);
        if let Some(icon) = entry.icon {
            glyphicon::draw(
                s,
                icon,
                row.x + ctx.px(14) as i32,
                row.y + (row.h as i32 - side as i32) / 2,
                side,
                if on && theme::is_classic() {
                    crate::Color::rgb(0xFF, 0xFF, 0xFF)
                } else if entry.danger {
                    p.bad_ink
                } else {
                    p.ink3
                },
                255,
            );
        }
        let ink = if entry.danger {
            p.bad_ink
        } else if on {
            p.ink
        } else {
            p.ink2
        };
        let text_x = row.x + ctx.px(14 + 14 + 10) as i32;
        paint::text_clipped(
            inner,
            s,
            Role::Body,
            text_x,
            paint::baseline(inner, Role::Body, row),
            (row.right() - text_x - ctx.px(12) as i32).max(0) as u32,
            entry.title,
            ink,
        );
    }
}

/// Какой пункт под точкой.
#[must_use]
pub fn menu_hit(ctx: Ctx, rect: Rect, entries: &[Entry<'_>], x: i32, y: i32) -> Option<usize> {
    if !rect.contains(x, y) {
        return None;
    }
    (0..entries.len()).find(|index| entry_rect(ctx, rect, entries, *index).contains(x, y))
}

// ---------------------------------------------------------------------------
// Единый стиль окон программ (20.11)
// ---------------------------------------------------------------------------
//
// Роман, 20.11: «окна открываются кто во что горазд» и у каждого свои
// отступы. Числа, которые раньше жили по копии в каждой программе, — здесь,
// в точках макета при множителе 1; [`Metrics`] отдаёт их уже умноженными.

/// Поле содержимого от краёв окна и панели.
pub const PAD: u32 = 14;
/// Высота строки списка или таблицы.
pub const ROW_H: u32 = 32;
/// Зазор между строками списка.
pub const ROW_GAP: u32 = 2;
/// Высота шапки столбцов таблицы.
pub const HEAD_H: u32 = 30;
/// Толщина полосы прокрутки (дорожка; бегунок внутри тоньше).
pub const SCROLLBAR_W: u32 = 12;
/// Высота полосы вкладок над панелью (вкладки «Файлов», 20.11).
pub const TAB_STRIP_H: u32 = 38;
/// Самый короткий бегунок: за меньший не ухватиться.
const THUMB_MIN: u32 = 28;
/// Строк за щелчок колеса — как у Проводника.
pub const WHEEL_LINES: i32 = 3;

/// Размеры единого стиля, умноженные на множитель геометрии.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub pad: u32,
    pub row_h: u32,
    pub row_gap: u32,
    pub head_h: u32,
    pub toolbar_h: u32,
    pub status_h: u32,
    pub scrollbar_w: u32,
    pub tab_strip_h: u32,
}

impl Metrics {
    #[must_use]
    pub fn of(ctx: Ctx) -> Self {
        Self {
            pad: ctx.px(PAD),
            row_h: ctx.px(ROW_H),
            row_gap: ctx.px(ROW_GAP),
            head_h: ctx.px(HEAD_H),
            toolbar_h: ctx.px(theme::TOOLBAR_H),
            status_h: ctx.px(theme::STATUS_H),
            scrollbar_w: ctx.px(SCROLLBAR_W),
            tab_strip_h: ctx.px(TAB_STRIP_H),
        }
    }

    /// Шаг строк: строка и зазор.
    #[must_use]
    pub const fn step(&self) -> u32 {
        self.row_h + self.row_gap
    }
}

// ---------------------------------------------------------------------------
// Прокрутка
// ---------------------------------------------------------------------------
//
// # Почему отдельно от выделения
//
// До 20.11 список «прокручивался» выделением: колесо двигало выбранную
// строку, а видимая часть ехала за ней. Роман: «при прокрутке выделение
// прыгает по папкам». В Windows колесо двигает **вид**, а выделение меняют
// только щелчок и клавиши; клавиша же, сдвинув выделение за край, подтягивает
// вид к нему. [`Scroll`] — это смещение вида в точках плюс отложенная просьба
// «показать строку N»: клавиатура о высоте строк и окна не знает, а
// отрисовка знает и исполняет просьбу ([`Scroll::sync`]).
//
// Состояние — в `Cell`: отрисовка получает программу по `&self`, а привести
// смещение к новому размеру окна (окно сузили — дальше конца листать нельзя)
// надо именно там.

/// Смещение прокручиваемого вида, точки от начала содержимого.
#[derive(Clone, Default, Debug)]
pub struct Scroll {
    at: Cell<i32>,
    follow: Cell<Option<usize>>,
    /// Бегунок тянут: где в нём ухватили (точки от его начала).
    grab: Cell<Option<i32>>,
    /// Указатель над полосой — она шире и ярче.
    hot: Cell<bool>,
}

impl Scroll {
    #[must_use]
    pub const fn new() -> Self {
        Self { at: Cell::new(0), follow: Cell::new(None), grab: Cell::new(None), hot: Cell::new(false) }
    }

    /// Смещение вида.
    #[must_use]
    pub fn offset(&self) -> i32 {
        self.at.get()
    }

    /// Поставить смещение (без проверки границ — их проверит [`Self::clamp`]).
    pub fn set(&self, at: i32) {
        self.at.set(at.max(0));
    }

    /// В начало: другой каталог, другой раздел.
    pub fn reset(&self) {
        self.at.set(0);
        self.follow.set(None);
        self.grab.set(None);
    }

    /// Показать элемент `index` при следующей раскладке — его выбрали
    /// клавишей.
    pub fn follow(&self, index: usize) {
        self.follow.set(Some(index));
    }

    /// Тянут ли бегунок.
    #[must_use]
    pub fn dragging(&self) -> bool {
        self.grab.get().is_some()
    }

    /// Подсвечена ли полоса (под указателем или её тянут).
    #[must_use]
    pub fn hot(&self) -> bool {
        self.hot.get() || self.dragging()
    }

    /// Самое большое смещение при таком виде и содержимом.
    #[must_use]
    pub const fn limit(view: u32, content: u32) -> i32 {
        content.saturating_sub(view) as i32
    }

    /// Не дальше конца и не раньше начала.
    pub fn clamp(&self, view: u32, content: u32) {
        self.at.set(self.at.get().clamp(0, Self::limit(view, content)));
    }

    /// Сдвинуть вид так, чтобы отрезок `[top, top + h)` содержимого был виден.
    pub fn reveal(&self, top: i32, h: u32, view: u32) {
        let at = self.at.get();
        if top < at {
            self.at.set(top.max(0));
        } else if top + h as i32 > at + view as i32 {
            self.at.set(top + h as i32 - view as i32);
        }
    }

    /// Привести в порядок перед раскладкой списка с одинаковыми строками шага
    /// `step`: исполнить просьбу «показать строку» и не выйти за края.
    pub fn sync(&self, view: u32, content: u32, step: u32) {
        if let Some(index) = self.follow.take() {
            self.reveal(index as i32 * step as i32, step, view);
        }
        self.clamp(view, content);
    }

    /// То же для строк разной высоты: `item(index)` — верх и высота элемента.
    pub fn sync_with(&self, view: u32, content: u32, item: impl Fn(usize) -> Option<(i32, u32)>) {
        if let Some((top, h)) = self.follow.take().and_then(item) {
            self.reveal(top, h, view);
        }
        self.clamp(view, content);
    }

    /// Колесо: `clicks` больше нуля — от себя, вид едет вверх. `line` — высота
    /// строки (шаг колеса — [`WHEEL_LINES`] строк). `true` — вид сдвинулся.
    pub fn wheel(&self, clicks: i32, view: u32, content: u32, line: u32) -> bool {
        let before = self.at.get();
        self.follow.set(None);
        self.at.set(before - clicks * WHEEL_LINES * line.max(1) as i32);
        self.clamp(view, content);
        self.at.get() != before
    }

    /// На `pages` страниц (минус — вверх). Страница — вид без одной строки
    /// `line`, чтобы край прежней страницы остался перед глазами.
    pub fn page(&self, pages: i32, view: u32, content: u32, line: u32) -> bool {
        let before = self.at.get();
        let page = view.saturating_sub(line).max(line).max(1) as i32;
        self.at.set(before + pages * page);
        self.clamp(view, content);
        self.at.get() != before
    }

    /// Строки шага `step`, хоть краем попадающие в вид высоты `view`.
    #[must_use]
    pub fn rows(&self, view: u32, step: u32, count: usize) -> core::ops::Range<usize> {
        let step = step.max(1) as i32;
        let at = self.at.get().max(0);
        let first = (at / step) as usize;
        let last = ((at + view as i32 + step - 1) / step) as usize;
        first.min(count)..last.min(count)
    }

    /// Номер строки шага `step` под точкой `y` вида, начинающегося с `top`.
    #[must_use]
    pub fn row_at(&self, y: i32, top: i32, step: u32) -> Option<usize> {
        let inside = y - top + self.at.get();
        (inside >= 0).then(|| (inside / step.max(1) as i32) as usize)
    }

    /// Указатель отпустили: бросить бегунок. `true` — его тянули.
    pub fn release(&self) -> bool {
        self.grab.take().is_some()
    }

    /// Указатель над полосой или нет (`bar` — `None`, если полосы нет).
    /// `true` — подсветка сменилась, надо перерисовать.
    pub fn hover(&self, bar: Option<&ScrollBar>, x: i32, y: i32) -> bool {
        let hot = bar.is_some_and(|bar| bar.track.contains(x, y));
        self.hot.replace(hot) != hot
    }
}

/// Полоса прокрутки: дорожка и бегунок, длина которого — доля видимого.
///
/// Считается заново на каждый кадр и на каждый щелчок от той же раскладки —
/// как всё в этом модуле. Состояние (смещение, захват) — в [`Scroll`].
#[derive(Clone, Copy, Debug)]
pub struct ScrollBar {
    pub track: Rect,
    pub thumb: Rect,
    view: u32,
    content: u32,
    vertical: bool,
}

impl ScrollBar {
    /// Вертикальная полоса у правого края вида `view` (прямоугольник видимой
    /// части) для содержимого высоты `content`. `None` — всё и так видно.
    #[must_use]
    pub fn vertical(ctx: Ctx, view: Rect, content: u32, scroll: &Scroll) -> Option<Self> {
        if content <= view.h || view.h == 0 {
            return None;
        }
        let w = ctx.px(SCROLLBAR_W).min(view.w);
        let track = Rect::new(view.right() - w as i32, view.y, w, view.h);
        let (start, len) = Self::thumb_span(ctx, track.h, view.h, content, scroll.offset());
        Some(Self { track, thumb: Rect::new(track.x, track.y + start, w, len), view: view.h, content, vertical: true })
    }

    /// Горизонтальная полоса у нижнего края вида шириной `view.w` для
    /// содержимого ширины `content`.
    #[must_use]
    pub fn horizontal(ctx: Ctx, view: Rect, content: u32, scroll: &Scroll) -> Option<Self> {
        if content <= view.w || view.w == 0 {
            return None;
        }
        let h = ctx.px(SCROLLBAR_W).min(view.h);
        let track = Rect::new(view.x, view.bottom() - h as i32, view.w, h);
        let (start, len) = Self::thumb_span(ctx, track.w, view.w, content, scroll.offset());
        Some(Self { track, thumb: Rect::new(track.x + start, track.y, len, h), view: view.w, content, vertical: false })
    }

    fn thumb_span(ctx: Ctx, track: u32, view: u32, content: u32, at: i32) -> (i32, u32) {
        let len = ((u64::from(view) * u64::from(track) / u64::from(content.max(1))) as u32).max(ctx.px(THUMB_MIN)).min(track);
        let room = track - len;
        let limit = Scroll::limit(view, content).max(1);
        let start = (i64::from(at.clamp(0, limit)) * i64::from(room) / i64::from(limit)) as i32;
        (start, len)
    }

    /// Точка вдоль полосы.
    const fn along(&self, x: i32, y: i32) -> i32 {
        if self.vertical { y } else { x }
    }

    const fn start(rect: Rect, vertical: bool) -> i32 {
        if vertical { rect.y } else { rect.x }
    }

    const fn length(rect: Rect, vertical: bool) -> u32 {
        if vertical { rect.h } else { rect.w }
    }

    /// Попадает ли точка в полосу.
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.track.contains(x, y)
    }

    /// Нажатие: по бегунку — ухватить его, по дорожке — страница в сторону
    /// точки (как в Windows). `false` — мимо полосы.
    pub fn press(&self, scroll: &Scroll, x: i32, y: i32) -> bool {
        if !self.track.contains(x, y) {
            return false;
        }
        let at = self.along(x, y);
        let thumb_start = Self::start(self.thumb, self.vertical);
        let thumb_len = Self::length(self.thumb, self.vertical) as i32;
        if at >= thumb_start && at < thumb_start + thumb_len {
            scroll.grab.set(Some(at - thumb_start));
        } else {
            let line = (self.view / 10).max(1);
            scroll.page(if at < thumb_start { -1 } else { 1 }, self.view, self.content, line);
        }
        true
    }

    /// Указатель с зажатой кнопкой: бегунок едет за ним. `false` — бегунок
    /// не ухвачен.
    pub fn drag(&self, scroll: &Scroll, x: i32, y: i32) -> bool {
        let Some(grab) = scroll.grab.get() else {
            return false;
        };
        let track_start = Self::start(self.track, self.vertical);
        let room = Self::length(self.track, self.vertical).saturating_sub(Self::length(self.thumb, self.vertical));
        let pos = (self.along(x, y) - grab - track_start).clamp(0, room as i32);
        let limit = Scroll::limit(self.view, self.content);
        let at = if room == 0 { 0 } else { (i64::from(pos) * i64::from(limit) / i64::from(room)) as i32 };
        scroll.at.set(at);
        true
    }

    /// Нарисовать. Покойная полоса — тонкий бегунок без дорожки (Windows 11);
    /// под указателем и когда тянут — дорожка и бегунок пошире.
    pub fn draw(&self, ctx: Ctx, s: &mut Surface, scroll: &Scroll) {
        let p = ctx.palette;
        if theme::is_classic() {
            // Windows 98: дорожка в шахматку, стрелки на концах, выпуклый
            // бегунок — всегда, а не только под указателем.
            classic::scrollbar(s, self.track, self.thumb, self.vertical, classic::unit(ctx.scale));
            return;
        }
        let hot = scroll.hot();
        if hot {
            draw::rounded(s, self.track, ctx.px(theme::R_ROW), p.line2.color, p.line2.alpha);
        }
        let thick = if hot { ctx.px(8) } else { ctx.px(4) };
        let inset = ctx.px(2);
        let rect = if self.vertical {
            Rect::new(
                self.thumb.x + (self.thumb.w as i32 - thick as i32) / 2,
                self.thumb.y + inset as i32,
                thick,
                self.thumb.h.saturating_sub(inset * 2),
            )
        } else {
            Rect::new(
                self.thumb.x + inset as i32,
                self.thumb.y + (self.thumb.h as i32 - thick as i32) / 2,
                self.thumb.w.saturating_sub(inset * 2),
                thick,
            )
        };
        let ink = if scroll.dragging() {
            p.ink2
        } else if hot {
            p.ink3
        } else {
            p.ink5
        };
        draw::rounded(s, rect, thick / 2, ink, 255);
    }
}

// ---------------------------------------------------------------------------
// Строка состояния по образцу Проводника
// ---------------------------------------------------------------------------

/// «Элементов: 30».
#[must_use]
pub fn items_text(count: usize) -> String {
    format!("Элементов: {count}")
}

/// «Выбрано 5 элем.: 1,90 МБ»; размер — если он известен и не ноль.
#[must_use]
pub fn selected_text(count: usize, bytes: u64) -> String {
    if bytes == 0 { format!("Выбрано {count} элем.") } else { format!("Выбрано {count} элем.: {}", size_text(bytes)) }
}

/// Размер как в Проводнике: три значащие цифры — «1,90 МБ», «19,0 МБ»,
/// «190 МБ»; меньше килобайта — в байтах.
#[must_use]
pub fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["КБ", "МБ", "ГБ", "ТБ"];
    if bytes < 1024 {
        return format!("{bytes} байт");
    }
    let mut unit = 0;
    let mut scaled = bytes as u128 * 100 / 1024;
    while scaled >= 1024 * 100 && unit + 1 < UNITS.len() {
        scaled /= 1024;
        unit += 1;
    }
    // `scaled` — сотые доли единицы.
    let whole = scaled / 100;
    if whole >= 100 {
        format!("{whole} {}", UNITS[unit])
    } else if whole >= 10 {
        format!("{whole},{} {}", (scaled % 100) / 10, UNITS[unit])
    } else {
        format!("{whole},{:02} {}", scaled % 100, UNITS[unit])
    }
}

/// Места под `count` переключателей вида у правого края строки состояния —
/// справа налево в том порядке, в каком их перечисляют слева направо.
#[must_use]
pub fn status_toggles(ctx: Ctx, rect: Rect, count: usize) -> Vec<Rect> {
    let w = ctx.px(28);
    let h = rect.h.saturating_sub(ctx.px(8));
    let gap = ctx.px(4);
    let y = rect.y + (rect.h as i32 - h as i32) / 2;
    let total = count as u32 * w + count.saturating_sub(1) as u32 * gap;
    let mut x = rect.right() - ctx.px(PAD) as i32 - total as i32;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(Rect::new(x, y, w, h));
        x += (w + gap) as i32;
    }
    out
}

/// Переключатель вида в строке состояния: значок, у включённого — подложка.
pub fn status_toggle(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Icon, on: bool) {
    let p = ctx.palette;
    if on && theme::is_classic() {
        classic::pressed(s, rect, classic::unit(ctx.scale));
    } else if on {
        draw::rounded(s, rect, ctx.px(theme::R_ROW), ctx.flat(p.btn), 255);
        draw::rounded_stroke(s, rect, ctx.px(theme::R_ROW), p.btnline.color, p.btnline.alpha);
    }
    let glyph = ctx.px(12);
    glyphicon::draw(
        s,
        icon,
        rect.x + (rect.w as i32 - glyph as i32) / 2,
        rect.y + (rect.h as i32 - glyph as i32) / 2,
        glyph,
        if on { p.ink } else { p.ink4 },
        255,
    );
}

/// Строка состояния единого вида: слева части через черту («Элементов: 30 |
/// Выбрано 5 элем.: 1,90 МБ»), справа — `right` текстом, левее места под
/// `toggles` переключателей ([`status_toggles`]).
pub fn status_line(ctx: Ctx, s: &mut Surface, rect: Rect, parts: &[&str], right: &str, toggles: usize) {
    let p = ctx.palette;
    if rect.is_empty() {
        return;
    }
    s.fill(rect, ctx.flat(p.panel));
    if theme::is_classic() {
        // Строка состояния Windows 98 — мелкая вдавленность.
        let t = classic::unit(ctx.scale);
        classic::shallow(s, Rect::new(rect.x + 2 * t as i32, rect.y + 2 * t as i32, rect.w.saturating_sub(4 * t), rect.h.saturating_sub(3 * t)), t);
    } else {
        draw::hline(s, rect.x, rect.y, rect.w, p.line2.color, p.line2.alpha);
    }
    let bar = ctx.on(theme::panel_bg());
    let pad = ctx.px(PAD) as i32;
    let role = Role::Caption;
    let y = paint::baseline(bar, role, rect);
    let slot = status_toggles(ctx, rect, toggles).first().map_or(rect.right() - pad, |first| first.x - ctx.px(10) as i32);
    let right_w = if right.is_empty() { 0 } else { ctx.face(role).width(right) as i32 };
    if right_w > 0 {
        paint::text_right(bar, s, role, slot, y, right, p.ink4);
    }
    let limit = slot - right_w - ctx.px(16) as i32;
    let mut x = rect.x + pad;
    let sep_h = rect.h.saturating_sub(ctx.px(14));
    for (index, part) in parts.iter().filter(|part| !part.is_empty()).enumerate() {
        if index > 0 {
            let gap = ctx.px(10) as i32;
            if x + gap * 2 >= limit {
                break;
            }
            draw::vline(s, x + gap, rect.y + (rect.h as i32 - sep_h as i32) / 2, sep_h, p.line3.color, p.line3.alpha);
            x += gap * 2 + 1;
        }
        let room = (limit - x).max(0) as u32;
        if room == 0 {
            break;
        }
        x += paint::text_clipped(bar, s, role, x, y, room, part, p.ink3) as i32;
    }
}

// ---------------------------------------------------------------------------
// Полоса вкладок (вкладки «Файлов», 20.11)
// ---------------------------------------------------------------------------

/// Куда попали в полосе вкладок.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabHit {
    /// Сама вкладка.
    Tab(usize),
    /// Её крестик.
    Close(usize),
    /// «+» — новая вкладка.
    New,
    /// Пустое место полосы.
    Strip,
}

/// Полоса вкладок как у Проводника Windows 11: вкладки слева направо, у
/// каждой крестик, за последней «+». Активная — цвета панели под ней и
/// сливается с ней.
pub struct TabStrip {
    pub rect: Rect,
    pub tabs: Vec<Rect>,
    pub closes: Vec<Rect>,
    pub plus: Rect,
}

impl TabStrip {
    /// Разложить `count` вкладок в полосе `rect`: по 240 точек, теснее, если
    /// не помещаются, но не уже 72.
    #[must_use]
    pub fn new(ctx: Ctx, rect: Rect, count: usize) -> Self {
        let pad = ctx.px(8) as i32;
        let plus_w = ctx.px(36);
        let room = (rect.w as i32 - pad * 2 - plus_w as i32 - ctx.px(8) as i32).max(0) as u32;
        let count_u = count.max(1) as u32;
        let tab_w = (room / count_u).clamp(ctx.px(72), ctx.px(240));
        let tab_h = rect.h.saturating_sub(ctx.px(6));
        let y = rect.bottom() - tab_h as i32;
        let mut tabs = Vec::with_capacity(count);
        let mut closes = Vec::with_capacity(count);
        let mut x = rect.x + pad;
        let close = ctx.px(22);
        for _ in 0..count {
            let tab = Rect::new(x, y, tab_w, tab_h);
            closes.push(Rect::new(tab.right() - close as i32 - ctx.px(6) as i32, y + (tab_h as i32 - close as i32) / 2, close, close));
            tabs.push(tab);
            x += tab_w as i32;
        }
        let plus_h = ctx.px(28).min(tab_h);
        let plus = Rect::new(x + ctx.px(4) as i32, y + (tab_h as i32 - plus_h as i32) / 2, plus_w, plus_h);
        Self { rect, tabs, closes, plus }
    }

    /// Что под точкой. `None` — мимо полосы.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<TabHit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        if let Some(index) = self.closes.iter().position(|rect| rect.contains(x, y)) {
            return Some(TabHit::Close(index));
        }
        if let Some(index) = self.tabs.iter().position(|rect| rect.contains(x, y)) {
            return Some(TabHit::Tab(index));
        }
        if self.plus.contains(x, y) {
            return Some(TabHit::New);
        }
        Some(TabHit::Strip)
    }

    /// Нарисовать: `titles` — подписи вкладок, `active` — открытая.
    pub fn draw(&self, ctx: Ctx, s: &mut Surface, titles: &[&str], active: usize, icon: Option<Icon>) {
        self.draw_hover(ctx, s, titles, active, icon, None);
    }

    /// То же с подсветкой под указателем `hover` (20.11): у неоткрытой
    /// вкладки — подложка, у крестика и «+» — своя плашка, как у Проводника.
    pub fn draw_hover(&self, ctx: Ctx, s: &mut Surface, titles: &[&str], active: usize, icon: Option<Icon>, hover: Option<TabHit>) {
        let p = ctx.palette;
        if self.rect.is_empty() {
            return;
        }
        if theme::is_classic() {
            self.draw_classic(ctx, s, titles, active, icon);
            return;
        }
        s.fill(self.rect, theme::window_bg());
        let panel = ctx.flat(p.panel);
        let r = ctx.px(theme::R_CARD);
        let hot_tab = match hover {
            Some(TabHit::Tab(index) | TabHit::Close(index)) => Some(index),
            _ => None,
        };
        for (index, (tab, title)) in self.tabs.iter().zip(titles).enumerate() {
            let on = index == active;
            if !on && hot_tab == Some(index) {
                let inset = ctx.px(3);
                let plate = Rect::new(tab.x + inset as i32, tab.y + inset as i32, tab.w.saturating_sub(inset * 2), tab.h.saturating_sub(inset * 2));
                draw::rounded(s, plate, ctx.px(theme::R_ROW), p.hover1.color, p.hover1.alpha);
            }
            if on {
                // Верх скруглён, низ уходит в панель под полосой.
                draw::rounded(s, *tab, r, panel, 255);
                s.fill(Rect::new(tab.x, tab.bottom() - r as i32, tab.w, r), panel);
                draw::rounded_stroke(s, Rect::new(tab.x, tab.y, tab.w, tab.h + r), r, p.line2.color, p.line2.alpha);
                s.fill(Rect::new(tab.x + 1, tab.bottom() - 1, tab.w.saturating_sub(2), 1), panel);
            } else if index + 1 != active && index + 1 < self.tabs.len() {
                let h = tab.h.saturating_sub(ctx.px(16));
                draw::vline(s, tab.right() - 1, tab.y + (tab.h as i32 - h as i32) / 2, h, p.line3.color, p.line3.alpha);
            }
            let inner = if on { ctx.on(panel) } else { ctx.on(theme::window_bg()) };
            let mut x = tab.x + ctx.px(12) as i32;
            if let Some(icon) = icon {
                let g = ctx.px(14);
                glyphicon::draw(s, icon, x, tab.y + (tab.h as i32 - g as i32) / 2, g, if on { p.acc } else { p.ink4 }, 255);
                x += (g + ctx.px(8)) as i32;
            }
            let close = self.closes[index];
            let room = (close.x - x - ctx.px(4) as i32).max(0) as u32;
            paint::text_clipped(inner, s, Role::Body, x, paint::baseline(inner, Role::Body, *tab), room, title, if on { p.ink } else { p.ink3 });
            let g = ctx.px(10);
            let close_hot = hover == Some(TabHit::Close(index));
            if close_hot {
                draw::rounded(s, close, ctx.px(theme::R_ROW), p.hover2.color, p.hover2.alpha);
            }
            iconset::draw(s, Fi::X, close.x + (close.w as i32 - g as i32) / 2, close.y + (close.h as i32 - g as i32) / 2, g, if on || close_hot { p.ink2 } else { p.ink4 }, 255);
        }
        // Линия под полосой — кроме места под активной вкладкой.
        let line_y = self.rect.bottom() - 1;
        match self.tabs.get(active) {
            Some(tab) => {
                draw::hline(s, self.rect.x, line_y, (tab.x - self.rect.x).max(0) as u32, p.line2.color, p.line2.alpha);
                draw::hline(s, tab.right(), line_y, (self.rect.right() - tab.right()).max(0) as u32, p.line2.color, p.line2.alpha);
            }
            None => draw::hline(s, self.rect.x, line_y, self.rect.w, p.line2.color, p.line2.alpha),
        }
        let g = ctx.px(14);
        if hover == Some(TabHit::New) {
            draw::rounded(s, self.plus, ctx.px(theme::R_ROW), p.hover1.color, p.hover1.alpha);
        }
        iconset::draw(s, Fi::Plus, self.plus.x + (self.plus.w as i32 - g as i32) / 2, self.plus.y + (self.plus.h as i32 - g as i32) / 2, g, if hover == Some(TabHit::New) { p.ink } else { p.ink3 }, 255);
    }

    /// Вкладки Windows 98 (22.11, классика): выпуклые «ушки», открытая —
    /// выше остальных и сливается с панелью под полосой.
    fn draw_classic(&self, ctx: Ctx, s: &mut Surface, titles: &[&str], active: usize, icon: Option<Icon>) {
        use crate::theme::win98::{DARK, FACE, LIGHT, SHADE};
        let p = ctx.palette;
        let t = classic::unit(ctx.scale);
        let ti = t as i32;
        s.fill(self.rect, FACE);
        let base = self.rect.bottom() - ti;
        s.fill(Rect::new(self.rect.x, base, self.rect.w, t), LIGHT);
        for (index, (tab, title)) in self.tabs.iter().zip(titles).enumerate() {
            let on = index == active;
            let lift = if on { 0 } else { 2 * ti };
            let ear = Rect::new(tab.x - if on { 2 * ti } else { 0 }, tab.y + lift, tab.w + if on { 4 * t } else { 0 }, (base - tab.y - lift).max(0) as u32 + if on { t } else { 0 });
            s.fill(ear, FACE);
            s.fill(Rect::new(ear.x, ear.y + ti, t, ear.h.saturating_sub(t)), LIGHT);
            s.fill(Rect::new(ear.x + ti, ear.y, ear.w.saturating_sub(3 * t), t), LIGHT);
            s.fill(Rect::new(ear.right() - 2 * ti, ear.y + ti, t, ear.h.saturating_sub(t)), SHADE);
            s.fill(Rect::new(ear.right() - ti, ear.y + 2 * ti, t, ear.h.saturating_sub(2 * t)), DARK);
            let inner = ctx.on(FACE);
            let mut x = tab.x + ctx.px(8) as i32;
            if let Some(icon) = icon {
                let g = ctx.px(14);
                glyphicon::draw(s, icon, x, tab.y + lift + ((tab.h as i32 - lift) - g as i32) / 2, g, p.ink, 255);
                x += (g + ctx.px(6)) as i32;
            }
            let close = self.closes[index];
            let room = (close.x - x - ctx.px(4) as i32).max(0) as u32;
            let band = Rect::new(tab.x, tab.y + lift, tab.w, (tab.h as i32 - lift).max(0) as u32);
            paint::text_clipped(inner, s, Role::Body, x, paint::baseline(inner, Role::Body, band), room, title, p.ink);
            let g = ctx.px(10);
            iconset::draw(s, Fi::X, close.x + (close.w as i32 - g as i32) / 2, close.y + lift / 2 + (close.h as i32 - g as i32) / 2, g, p.ink, 255);
        }
        // «+» — маленькая кнопка.
        let plus = Rect::new(self.plus.x, self.plus.y + 2 * ti, self.plus.w, self.plus.h.saturating_sub(4 * t));
        classic::button(s, plus, t, false);
        let g = ctx.px(12);
        iconset::draw(s, Fi::Plus, plus.x + (plus.w as i32 - g as i32) / 2, plus.y + (plus.h as i32 - g as i32) / 2, g, p.ink, 255);
    }
}

// ---------------------------------------------------------------------------
// Вопрос поверх окна
// ---------------------------------------------------------------------------

/// Вопрос «Удалить?», «Очистить корзину?» поверх окна программы (21.11).
///
/// Тот же вид, что у окон-вопросов ядра (`ui::dialog`: «Удаление файла»,
/// «Выключение»): плитка со значком слева, заголовок, пояснение, черта и
/// кнопки в правом нижнем углу — действие, потом «Отмена»; кнопка, которую
/// нажмёт Enter, обведена. Окно под вопросом притушено: отвечать надо ему.
/// Состояния нет, как и у остального здесь: выбранную кнопку держит
/// программа, клавиши (Enter, Esc, Tab, стрелки) разбирает она же.
pub struct Ask<'a> {
    pub icon: Icon,
    pub tone: paint::Tone,
    pub title: &'a str,
    /// Что именно спрашивается — имя файла, число объектов; строкой между
    /// заголовком и пояснением. Пусто — строки нет.
    pub detail: &'a str,
    pub text: &'a str,
    /// Подпись и вес кнопки действия; вторая кнопка — «Отмена».
    pub action: &'a str,
    pub weight: paint::Weight,
}

/// Где карточка вопроса и её кнопки: `[действие, отмена]`.
#[derive(Clone, Copy)]
pub struct AskLayout {
    pub card: Rect,
    pub buttons: [Rect; 2],
}

/// Поля карточки вопроса.
const ASK_PAD: u32 = 24;
/// Плитка со значком.
const ASK_TILE: u32 = 44;
/// Высота кнопки.
const ASK_BUTTON_H: u32 = 32;
/// Самая узкая кнопка.
const ASK_BUTTON_MIN_W: u32 = 104;

/// Строки пояснения, разбитые по словам под ширину `width`.
fn ask_lines(ctx: Ctx, text: &str, width: u32) -> Vec<String> {
    let face = ctx.face(Role::Body);
    let space = face.width(" ");
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_w = 0;
    for word in text.split(' ') {
        let word_w = face.width(word);
        if !current.is_empty() && current_w + space + word_w > width {
            out.push(core::mem::take(&mut current));
            current_w = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_w += space;
        }
        current.push_str(word);
        current_w += word_w;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Раскладка вопроса посреди `area` — одна для отрисовки и для щелчка.
#[must_use]
pub fn ask_layout(ctx: Ctx, area: Rect, ask: &Ask<'_>) -> AskLayout {
    let pad = ctx.px(ASK_PAD);
    // Заголовок не обрезается: карточка шире, если он длинный.
    let fit = ctx.face(Role::Heading).width(ask.title) + pad * 2 + ctx.px(ASK_TILE + 16 + 4);
    let w = fit.max(ctx.px(460)).min(area.w.saturating_sub(ctx.px(24)));
    let text_w = w.saturating_sub(pad * 2 + ctx.px(ASK_TILE + 16));
    let lines = ask_lines(ctx, ask.text, text_w).len().max(1) as u32;
    let heading = u32::from(ctx.face(Role::Heading).line);
    let body = u32::from(ctx.face(Role::Body).line);
    let detail = if ask.detail.is_empty() { 0 } else { body + ctx.px(4) };
    let top = (heading + ctx.px(8) + detail + body * lines).max(ctx.px(ASK_TILE));
    let h = pad + top + ctx.px(28) + ctx.px(ASK_BUTTON_H) + pad;
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
    let face = ctx.face(Role::Label);
    let width = |label: &str| (face.width(label) + ctx.px(36)).max(ctx.px(ASK_BUTTON_MIN_W));
    let (yes_w, no_w) = (width(ask.action), width("Отмена"));
    let gap = ctx.px(8);
    let by = card.bottom() - (pad + ctx.px(ASK_BUTTON_H)) as i32;
    let no = Rect::new(card.right() - (pad + no_w) as i32, by, no_w, ctx.px(ASK_BUTTON_H));
    let yes = Rect::new(no.x - (gap + yes_w) as i32, by, yes_w, ctx.px(ASK_BUTTON_H));
    AskLayout { card, buttons: [yes, no] }
}

/// Нарисовать вопрос поверх `area`; `focus` — кнопка под Enter (0 — действие).
pub fn draw_ask(ctx: Ctx, s: &mut Surface, area: Rect, ask: &Ask<'_>, focus: usize) {
    let lay = ask_layout(ctx, area, ask);
    draw::blend_rect(s, area, crate::Color::rgb(0, 0, 0), 70);
    let inner = card(ctx, s, lay.card);
    let p = inner.palette;
    let pad = ctx.px(ASK_PAD) as i32;
    let tile = Rect::new(lay.card.x + pad, lay.card.y + pad, ctx.px(ASK_TILE), ctx.px(ASK_TILE));
    paint::icon_tile(inner, s, tile, ask.icon, ask.tone, true);
    let x = tile.right() + ctx.px(16) as i32;
    let width = (lay.card.right() - pad - x).max(0) as u32;
    let mut y = lay.card.y + pad;
    paint::text_clipped(inner, s, Role::Heading, x, y, width, ask.title, p.ink);
    y += i32::from(ctx.face(Role::Heading).line) + ctx.px(8) as i32;
    if !ask.detail.is_empty() {
        paint::text_clipped(inner, s, Role::Body, x, y, width, ask.detail, p.ink2);
        y += i32::from(ctx.face(Role::Body).line) + ctx.px(4) as i32;
    }
    for line in ask_lines(ctx, ask.text, width) {
        paint::text_clipped(inner, s, Role::Body, x, y, width, &line, p.ink3);
        y += i32::from(ctx.face(Role::Body).line);
    }
    paint::separator(inner, s, lay.card.x, lay.buttons[0].y - ctx.px(16) as i32, lay.card.w);
    let specs = [(ask.weight, ask.action), (paint::Weight::Normal, "Отмена")];
    for (index, (rect, (weight, label))) in lay.buttons.iter().zip(specs).enumerate() {
        paint::button(inner, s, *rect, weight, label, false);
        if index == focus {
            let ring = ctx.px(3);
            let around = Rect::new(rect.x - ring as i32, rect.y - ring as i32, rect.w + ring * 2, rect.h + ring * 2);
            draw::rounded_stroke(s, around, ctx.px(theme::R_ROW) + ring, p.accedge, 255);
        }
    }
}

/// Кнопка вопроса под точкой: `Some(true)` — действие, `Some(false)` — отмена.
#[must_use]
pub fn ask_hit(ctx: Ctx, area: Rect, ask: &Ask<'_>, x: i32, y: i32) -> Option<bool> {
    let lay = ask_layout(ctx, area, ask);
    lay.buttons.iter().position(|rect| rect.contains(x, y)).map(|index| index == 0)
}
