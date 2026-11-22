//! Классика — элементы Windows 98 (22.11, Роман: набор «Упрощённый» должен
//! менять весь интерфейс, а не одну полосу заголовка).
//!
//! # Как устроена кромка
//!
//! Объём у Windows 98 держится на четырёх цветах и двух рядах точек:
//!
//! * выпуклая (кнопка, панель задач, окно): снаружи сверху-слева светло-серый
//!   `HI`, внутри — белый `LIGHT`; снаружи снизу-справа чёрный `DARK`, внутри —
//!   серый `SHADE`;
//! * вдавленная (поле ввода, список, трей): то же наоборот — снаружи серый и
//!   белый, внутри чёрный и светло-серый;
//! * нажатая кнопка — чёрная рамка и серая внутри, надпись сдвинута на точку.
//!
//! Кромка в точку не умножается на масштаб до 150 %: у Windows 98 она
//! всегда в одну точку, и на 125 % двухточечная кромка выглядит грубо.
//! С 150 % — две точки на ряд, иначе на 2560 при 200 % кромку не видно.
//!
//! Все функции рисуют **внутри** прямоугольника — ни точки наружу.

use crate::theme::win98::{DARK, FACE, HI, LIGHT, NAVY, SHADE};
use crate::{Color, Rect, Scale, Surface, draw};

/// Толщина одного ряда кромки при таком масштабе.
#[must_use]
pub fn unit(scale: Scale) -> u32 {
    if scale.pct() >= 150 { 2 } else { 1 }
}

/// Рамка из двух рядов: `tl_out`/`br_out` — внешний, `tl_in`/`br_in` —
/// внутренний.
fn two_rows(s: &mut Surface, rect: Rect, t: u32, tl_out: Color, br_out: Color, tl_in: Color, br_in: Color) {
    if rect.w < 4 * t || rect.h < 4 * t {
        s.fill(rect, br_out);
        return;
    }
    edge(s, rect, t, tl_out, br_out);
    edge(s, rect.shrink(t), t, tl_in, br_in);
}

/// Один ряд: верх и лево цветом `tl`, низ и право — `br`. Угол снизу-слева и
/// сверху-справа — у `br` (как у Windows).
pub fn edge(s: &mut Surface, rect: Rect, t: u32, tl: Color, br: Color) {
    if rect.is_empty() {
        return;
    }
    s.fill(Rect::new(rect.x, rect.y, rect.w, t), tl);
    s.fill(Rect::new(rect.x, rect.y, t, rect.h), tl);
    s.fill(Rect::new(rect.x, rect.bottom() - t as i32, rect.w, t), br);
    s.fill(Rect::new(rect.right() - t as i32, rect.y, t, rect.h), br);
}

/// Выпуклая кромка (кнопка, окно, панель задач).
pub fn raised(s: &mut Surface, rect: Rect, t: u32) {
    two_rows(s, rect, t, HI, DARK, LIGHT, SHADE);
}

/// Выпуклая кромка кнопки: внешний ряд белый (у кнопок Windows 98 —
/// `LIGHT` снаружи, `HI` внутри).
pub fn button_edge(s: &mut Surface, rect: Rect, t: u32) {
    two_rows(s, rect, t, LIGHT, DARK, HI, SHADE);
}

/// Вдавленная кромка (поле ввода, список).
pub fn sunken(s: &mut Surface, rect: Rect, t: u32) {
    two_rows(s, rect, t, SHADE, LIGHT, DARK, HI);
}

/// Нажатая кнопка: чёрная рамка, внутри — серая.
pub fn pressed(s: &mut Surface, rect: Rect, t: u32) {
    two_rows(s, rect, t, DARK, DARK, SHADE, SHADE);
}

/// Мелкая вдавленность в один ряд (трей панели задач, строка состояния).
pub fn shallow(s: &mut Surface, rect: Rect, t: u32) {
    edge(s, rect, t, SHADE, LIGHT);
}

/// Мелкая выпуклость в один ряд (полоса меню, группа).
pub fn bump(s: &mut Surface, rect: Rect, t: u32) {
    edge(s, rect, t, LIGHT, SHADE);
}

/// Кнопка целиком: грань, кромка; нажатая — кромкой «вдавлено».
pub fn button(s: &mut Surface, rect: Rect, t: u32, down: bool) {
    s.fill(rect, FACE);
    if down { pressed(s, rect, t) } else { button_edge(s, rect, t) }
}

/// Кнопка «по умолчанию» (Enter): лишняя чёрная рамка снаружи.
pub fn default_button(s: &mut Surface, rect: Rect, t: u32) {
    s.fill(rect, FACE);
    edge(s, rect, t, DARK, DARK);
    button_edge(s, rect.shrink(t), t);
}

/// Поле ввода: белое, вдавленное.
pub fn field(s: &mut Surface, rect: Rect, t: u32) {
    s.fill(rect, Color::rgb(0xFF, 0xFF, 0xFF));
    sunken(s, rect, t);
}

/// Окно-карточка (меню, всплывающее): грань и выпуклая кромка.
pub fn panel(s: &mut Surface, rect: Rect, t: u32) {
    s.fill(rect, FACE);
    raised(s, rect, t);
}

/// Бороздка-разделитель (меню, панель инструментов): серая линия и белая
/// под ней.
pub fn groove_h(s: &mut Surface, x: i32, y: i32, w: u32, t: u32) {
    s.fill(Rect::new(x, y, w, t), SHADE);
    s.fill(Rect::new(x, y + t as i32, w, t), LIGHT);
}

/// Вертикальная бороздка.
pub fn groove_v(s: &mut Surface, x: i32, y: i32, h: u32, t: u32) {
    s.fill(Rect::new(x, y, t, h), SHADE);
    s.fill(Rect::new(x + t as i32, y, t, h), LIGHT);
}

/// Выделение строки: тёмно-синяя заливка.
pub fn selection(s: &mut Surface, rect: Rect) {
    s.fill(rect, NAVY);
}

/// Пунктир фокуса (через точку, чёрным) внутри `rect`.
pub fn focus_dots(s: &mut Surface, rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let pixel = DARK.pixel();
    let bounds = s.bounds();
    let r = rect.intersect(&bounds);
    if r.is_empty() {
        return;
    }
    for x in r.x..r.right() {
        if (x + rect.y) & 1 == 0 && rect.y >= 0 && rect.y < bounds.h as i32 {
            s.put(x as u32, rect.y as u32, pixel);
        }
        let b = rect.bottom() - 1;
        if (x + b) & 1 == 0 && b >= 0 && b < bounds.h as i32 {
            s.put(x as u32, b as u32, pixel);
        }
    }
    for y in r.y..r.bottom() {
        if (rect.x + y) & 1 == 0 && rect.x >= 0 && rect.x < bounds.w as i32 {
            s.put(rect.x as u32, y as u32, pixel);
        }
        let rx = rect.right() - 1;
        if (rx + y) & 1 == 0 && rx >= 0 && rx < bounds.w as i32 {
            s.put(rx as u32, y as u32, pixel);
        }
    }
}

/// Заголовок с горизонтальным градиентом (активный — синий, неактивный —
/// серый): часть `part` полосы `bar`.
pub fn caption(s: &mut Surface, part: Rect, bar: Rect, focused: bool) {
    use crate::theme::win98::{CAPTION2, GREY_CAPTION, GREY_CAPTION2};
    let (from, to) = if focused { (NAVY, CAPTION2) } else { (GREY_CAPTION, GREY_CAPTION2) };
    let part = part.intersect(&bar).intersect(&s.bounds());
    if part.is_empty() {
        return;
    }
    let span = bar.w.max(2) - 1;
    for x in part.x..part.right() {
        let weight = ((x - bar.x).max(0) as u32 * 255 / span).min(255) as u8;
        s.fill(Rect::new(x, part.y, 1, part.h), from.mix(to, weight));
    }
}

/// Полоса прокрутки Windows 98: дорожка в шахматку, кнопки-стрелки на концах
/// и выпуклый бегунок. `track` — вся полоса, `thumb` — бегунок (внутри неё).
pub fn scrollbar(s: &mut Surface, track: Rect, thumb: Rect, vertical: bool, t: u32) {
    // Дорожка: шахматка белого и серого — так выглядит «пустая» часть.
    let (a, b) = (LIGHT.pixel(), FACE.pixel());
    let r = track.intersect(&s.bounds());
    for y in r.y..r.bottom() {
        let row = s.row_mut(y as u32);
        for x in r.x..r.right() {
            row[x as usize] = if (x + y) & 1 == 0 { a } else { b };
        }
    }
    let side = if vertical { track.w } else { track.h };
    let (first, last) = if vertical {
        (Rect::new(track.x, track.y, track.w, side.min(track.h / 2)), Rect::new(track.x, track.bottom() - side.min(track.h / 2) as i32, track.w, side.min(track.h / 2)))
    } else {
        (Rect::new(track.x, track.y, side.min(track.w / 2), track.h), Rect::new(track.right() - side.min(track.w / 2) as i32, track.y, side.min(track.w / 2), track.h))
    };
    for (index, arrow) in [first, last].into_iter().enumerate() {
        button(s, arrow, t, false);
        triangle(s, arrow, vertical, index == 1, DARK);
    }
    if !thumb.is_empty() {
        button(s, thumb, t, false);
    }
}

/// Чёрный треугольник-стрелка в середине `rect`: вверх/влево (`forward`
/// ложно) или вниз/вправо.
pub fn triangle(s: &mut Surface, rect: Rect, vertical: bool, forward: bool, color: Color) {
    let size = (rect.w.min(rect.h) / 4).max(2) as i32;
    let cx = rect.x + rect.w as i32 / 2;
    let cy = rect.y + rect.h as i32 / 2;
    for i in 0..size {
        let len = (2 * i + 1) as u32;
        if vertical {
            let y = if forward { cy - size / 2 + (size - 1 - i) } else { cy - size / 2 + i };
            s.fill(Rect::new(cx - i, y, len, 1), color);
        } else {
            let x = if forward { cx - size / 2 + (size - 1 - i) } else { cx - size / 2 + i };
            s.fill(Rect::new(x, cy - i, 1, len), color);
        }
    }
}

/// Флажок: белое вдавленное поле, внутри — галочка.
pub fn checkbox(s: &mut Surface, rect: Rect, t: u32, checked: bool) {
    field(s, rect, t);
    if checked {
        let inner = rect.shrink(2 * t + t);
        let w = inner.w.max(3) as i32;
        let h = inner.h.max(3) as i32;
        // Галочка — три точки вниз и вверх, толщиной в три строки.
        let thick = (h / 4).max(1);
        for i in 0..w {
            let y = if i < w / 3 { inner.y + h / 3 + i } else { inner.y + h / 3 + w / 3 - (i - w / 3) * (h / 2) / (w - w / 3).max(1) };
            s.fill(Rect::new(inner.x + i, y, 1, thick as u32), DARK);
        }
    }
}

/// Переключатель «вкл/выкл» в классике — тот же флажок слева, без «пилюли».
pub fn toggle(s: &mut Surface, rect: Rect, t: u32, on: bool) {
    let side = rect.h.min(rect.w).min(13 * t.max(1) + 2);
    let box_rect = Rect::new(rect.x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
    checkbox(s, box_rect, t, on);
}

/// Залить и обвести «вдавленным» дорожку полосы хода; `filled` — сколько
/// закрасить синими кубиками.
pub fn progress(s: &mut Surface, rect: Rect, t: u32, done: u32, total: u32) {
    s.fill(rect, FACE);
    shallow(s, rect, t);
    if total == 0 || done == 0 {
        return;
    }
    let inner = rect.shrink(2 * t);
    if inner.is_empty() {
        return;
    }
    let filled = (u64::from(inner.w) * u64::from(done.min(total)) / u64::from(total)) as u32;
    let block = (inner.h * 2 / 3).max(4);
    let gap = (block / 4).max(1);
    let mut x = 0;
    while x < filled {
        let w = block.min(filled - x);
        s.fill(Rect::new(inner.x + x as i32, inner.y, w, inner.h), NAVY);
        x += block + gap;
    }
}

/// Прямоугольник-рамка в одну точку цветом `color` — для заметной обводки
/// (фокус по умолчанию у кнопок, рамка «Пуска»).
pub fn outline(s: &mut Surface, rect: Rect, t: u32, color: Color) {
    edge(s, rect, t, color, color);
}

/// Сгладить «мягкий» элемент в классику: залить грань и выпуклую кромку —
/// для мест, где в современном виде была полупрозрачная карточка.
pub fn card(s: &mut Surface, rect: Rect, t: u32) {
    draw::blend_rect(s, rect, FACE, 255);
    raised(s, rect, t);
}
