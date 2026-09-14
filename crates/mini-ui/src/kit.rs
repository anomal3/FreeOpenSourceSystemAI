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

use alloc::vec::Vec;

use crate::glyphicon::{self, Icon};
use crate::paint::{self, Ctx, RowState};
use crate::typeface::Role;
use crate::{Rect, Surface, draw, theme};

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
        draw::hline(s, rect.x, rect.bottom() - 1, rect.w, p.line.color, p.line.alpha);
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
    draw::hline(s, rect.x, rect.y, rect.w, p.line2.color, p.line2.alpha);
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
            paint::separator(inner, s, rect.x + ctx.px(12) as i32, line_y, rect.w.saturating_sub(ctx.px(24)));
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
                if entry.danger { p.bad_ink } else { p.ink3 },
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
