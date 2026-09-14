//! Псевдографика, нарисованная по ячейке, а не взятая из шрифта (фаза С9).
//!
//! # Зачем рисовать, когда в шрифте есть `─│┌┐`
//!
//! Глиф шрифта рисуется внутри своей рамки: у JetBrains Mono вертикальная черта
//! кончается на пару точек раньше края строки, потому что высота строки больше
//! кегля. Рамка, собранная из таких глифов, выходит пунктиром — щель на каждом
//! стыке строк, и выглядит это как испорченный экран, а не как рамка. Двойных
//! линий (`═║╔╗`), без которых нет Far, в наборе знаков нет вовсе.
//!
//! Поэтому знаки из блоков `U+2500–U+257F` и `U+2580–U+259F` рисуются
//! прямоугольниками от края до края ячейки — так же поступает всякий
//! современный терминал. Рамка при этом сходится при любом кегле, а таблица
//! шрифта не растёт ни на байт.
//!
//! # Как описан знак
//!
//! Четырьмя «плечами» от центра к краям: вверх, вправо, вниз, влево, и у
//! каждого — вид линии. Так описывается весь блок линий без единого
//! нарисованного руками знака, а стыки получаются из правил, а не из таблицы
//! картинок. Пунктирные варианты (`┄┆╌`) рисуются сплошными: отличие видно
//! только на крупном кегле, и заводить ради него третий способ рисования не
//! стоило — это названное упрощение.
//!
//! # Двойные линии
//!
//! Двойное плечо — две параллельные линии толщиной `t` с просветом `t`. Где
//! линии кончаются, решает соседство: у `╔` внешняя пара смыкается углом
//! снаружи, внутренняя — внутри, у `╬` все четыре угла внутренние. Правила
//! записаны у [`double_arms`] и проверены тестами на `╔`, `╬`, `╟`.

use crate::draw;
use crate::{Color, Rect, Surface};

/// Вид одного плеча.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Line {
    None,
    Light,
    Heavy,
    Double,
}

use Line::{Double as D, Heavy as H, Light as L, None as N};

/// Плечи знака: вверх, вправо, вниз, влево.
type Arms = [Line; 4];

const UP: usize = 0;
const RIGHT: usize = 1;
const DOWN: usize = 2;
const LEFT: usize = 3;

/// Нарисовать знак псевдографики в ячейке.
///
/// Возвращает `false`, если знак не из тех, что рисуются здесь, — тогда его
/// рисует шрифт. Фон ячейки уже залит вызывающим: здесь кладутся только линии.
pub fn draw(surface: &mut Surface, cell: Rect, ch: char, color: Color) -> bool {
    if cell.w == 0 || cell.h == 0 {
        return false;
    }
    if let Some(arms) = arms(ch) {
        draw_arms(surface, cell, arms, color);
        return true;
    }
    block(surface, cell, ch, color)
}

/// Рисуется ли знак здесь, а не шрифтом.
#[must_use]
pub fn covers(ch: char) -> bool {
    arms(ch).is_some() || matches!(ch as u32, 0x2580..=0x259F)
}

/// Плечи знака из блока линий.
#[allow(clippy::match_same_arms)]
fn arms(ch: char) -> Option<Arms> {
    let arms = match ch as u32 {
        0x2500 | 0x2504 | 0x2508 | 0x254C => [N, L, N, L],
        0x2501 | 0x2505 | 0x2509 | 0x254D => [N, H, N, H],
        0x2502 | 0x2506 | 0x250A | 0x254E => [L, N, L, N],
        0x2503 | 0x2507 | 0x250B | 0x254F => [H, N, H, N],
        0x250C => [N, L, L, N],
        0x250D => [N, H, L, N],
        0x250E => [N, L, H, N],
        0x250F => [N, H, H, N],
        0x2510 => [N, N, L, L],
        0x2511 => [N, N, L, H],
        0x2512 => [N, N, H, L],
        0x2513 => [N, N, H, H],
        0x2514 => [L, L, N, N],
        0x2515 => [L, H, N, N],
        0x2516 => [H, L, N, N],
        0x2517 => [H, H, N, N],
        0x2518 => [L, N, N, L],
        0x2519 => [L, N, N, H],
        0x251A => [H, N, N, L],
        0x251B => [H, N, N, H],
        0x251C => [L, L, L, N],
        0x251D => [L, H, L, N],
        0x251E => [H, L, L, N],
        0x251F => [L, L, H, N],
        0x2520 => [H, L, H, N],
        0x2521 => [H, H, L, N],
        0x2522 => [L, H, H, N],
        0x2523 => [H, H, H, N],
        0x2524 => [L, N, L, L],
        0x2525 => [L, N, L, H],
        0x2526 => [H, N, L, L],
        0x2527 => [L, N, H, L],
        0x2528 => [H, N, H, L],
        0x2529 => [H, N, L, H],
        0x252A => [L, N, H, H],
        0x252B => [H, N, H, H],
        0x252C => [N, L, L, L],
        0x252D => [N, L, L, H],
        0x252E => [N, H, L, L],
        0x252F => [N, H, L, H],
        0x2530 => [N, L, H, L],
        0x2531 => [N, L, H, H],
        0x2532 => [N, H, H, L],
        0x2533 => [N, H, H, H],
        0x2534 => [L, L, N, L],
        0x2535 => [L, L, N, H],
        0x2536 => [L, H, N, L],
        0x2537 => [L, H, N, H],
        0x2538 => [H, L, N, L],
        0x2539 => [H, L, N, H],
        0x253A => [H, H, N, L],
        0x253B => [H, H, N, H],
        0x253C => [L, L, L, L],
        0x253D => [L, L, L, H],
        0x253E => [L, H, L, L],
        0x253F => [L, H, L, H],
        0x2540 => [H, L, L, L],
        0x2541 => [L, L, H, L],
        0x2542 => [H, L, H, L],
        0x2543 => [H, L, L, H],
        0x2544 => [H, H, L, L],
        0x2545 => [L, L, H, H],
        0x2546 => [L, H, H, L],
        0x2547 => [H, H, L, H],
        0x2548 => [L, H, H, H],
        0x2549 => [H, L, H, H],
        0x254A => [H, H, H, L],
        0x254B => [H, H, H, H],
        0x2550 => [N, D, N, D],
        0x2551 => [D, N, D, N],
        0x2552 => [N, D, L, N],
        0x2553 => [N, L, D, N],
        0x2554 => [N, D, D, N],
        0x2555 => [N, N, L, D],
        0x2556 => [N, N, D, L],
        0x2557 => [N, N, D, D],
        0x2558 => [L, D, N, N],
        0x2559 => [D, L, N, N],
        0x255A => [D, D, N, N],
        0x255B => [L, N, N, D],
        0x255C => [D, N, N, L],
        0x255D => [D, N, N, D],
        0x255E => [L, D, L, N],
        0x255F => [D, L, D, N],
        0x2560 => [D, D, D, N],
        0x2561 => [L, N, L, D],
        0x2562 => [D, N, D, L],
        0x2563 => [D, N, D, D],
        0x2564 => [N, D, L, D],
        0x2565 => [N, L, D, L],
        0x2566 => [N, D, D, D],
        0x2567 => [L, D, N, D],
        0x2568 => [D, L, N, L],
        0x2569 => [D, D, N, D],
        0x256A => [L, D, L, D],
        0x256B => [D, L, D, L],
        0x256C => [D, D, D, D],
        // Скруглённые углы рисуются прямыми: скругление радиусом в полклетки
        // на кегле в семь точек неотличимо от угла.
        0x256D => [N, L, L, N],
        0x256E => [N, N, L, L],
        0x256F => [L, N, N, L],
        0x2570 => [L, L, N, N],
        0x2574 => [N, N, N, L],
        0x2575 => [L, N, N, N],
        0x2576 => [N, L, N, N],
        0x2577 => [N, N, L, N],
        0x2578 => [N, N, N, H],
        0x2579 => [H, N, N, N],
        0x257A => [N, H, N, N],
        0x257B => [N, N, H, N],
        0x257C => [N, H, N, L],
        0x257D => [L, N, H, N],
        0x257E => [N, L, N, H],
        0x257F => [H, N, L, N],
        _ => return None,
    };
    Some(arms)
}

/// Геометрия ячейки, общая для всех плеч.
struct Geometry {
    cell: Rect,
    /// Толщина тонкой линии.
    t: u32,
}

impl Geometry {
    fn new(cell: Rect) -> Self {
        // Толщина растёт с кеглем: на ячейке 7×17 линия в точку, на 16×40 — в
        // две. Считается от меньшей стороны, иначе высокая узкая ячейка дала бы
        // линию толще половины своей ширины.
        let t = (cell.w.min(cell.h) / 8).max(1);
        Self { cell, t }
    }

    fn thickness(&self, line: Line) -> u32 {
        match line {
            Line::Heavy => self.t * 2,
            _ => self.t,
        }
    }

    /// Левая точка вертикальной линии заданной толщины, стоящей по центру.
    fn cx(&self, th: u32) -> i32 {
        self.cell.x + (self.cell.w.saturating_sub(th) / 2) as i32
    }

    /// Верхняя точка горизонтальной линии заданной толщины, стоящей по центру.
    fn cy(&self, th: u32) -> i32 {
        self.cell.y + (self.cell.h.saturating_sub(th) / 2) as i32
    }

    /// Левая и правая линии двойной вертикали.
    fn double_x(&self) -> (i32, i32) {
        let c = self.cx(self.t);
        (c - self.t as i32, c + self.t as i32)
    }

    /// Верхняя и нижняя линии двойной горизонтали.
    fn double_y(&self) -> (i32, i32) {
        let c = self.cy(self.t);
        (c - self.t as i32, c + self.t as i32)
    }
}

fn draw_arms(surface: &mut Surface, cell: Rect, arms: Arms, color: Color) {
    let g = Geometry::new(cell);
    single_arms(surface, &g, arms, color);
    double_arms(surface, &g, arms, color);
}

/// Вертикальный отрезок `[y0, y1)` толщиной `th` с левой точкой `x`.
fn vseg(surface: &mut Surface, x: i32, th: u32, y0: i32, y1: i32, color: Color) {
    if y1 > y0 {
        draw::blend_rect(surface, Rect::new(x, y0, th, (y1 - y0) as u32), color, 255);
    }
}

/// Горизонтальный отрезок `[x0, x1)` толщиной `th` с верхней точкой `y`.
fn hseg(surface: &mut Surface, y: i32, th: u32, x0: i32, x1: i32, color: Color) {
    if x1 > x0 {
        draw::blend_rect(surface, Rect::new(x0, y, (x1 - x0) as u32, th), color, 255);
    }
}

/// Тонкие и толстые плечи.
///
/// Плечо идёт от края до центра **с заходом** на свою толщину: у `┌` горизонталь
/// и вертикаль иначе встретились бы углом в одну точку, и на стыке осталась бы
/// выщербина. Если поперёк стоит двойная линия, одинарное плечо кончается на
/// ней: у `╟` — на ближней линии, у `╒` (двойная только с одной стороны) — на
/// дальней, чтобы угол сомкнулся.
fn single_arms(surface: &mut Surface, g: &Geometry, arms: Arms, color: Color) {
    let cell = g.cell;
    let t = g.t as i32;
    let (a, b) = g.double_x();
    let (p, q) = g.double_y();
    let both_v = arms[UP] == D && arms[DOWN] == D;
    let one_v = arms[UP] == D || arms[DOWN] == D;
    let both_h = arms[LEFT] == D && arms[RIGHT] == D;
    let one_h = arms[LEFT] == D || arms[RIGHT] == D;

    if matches!(arms[UP], L | H) {
        let th = g.thickness(arms[UP]);
        let end = if both_h {
            p
        } else if one_h {
            q + t
        } else {
            g.cy(th) + th as i32
        };
        vseg(surface, g.cx(th), th, cell.y, end, color);
    }
    if matches!(arms[DOWN], L | H) {
        let th = g.thickness(arms[DOWN]);
        let start = if both_h {
            q + t
        } else if one_h {
            p
        } else {
            g.cy(th)
        };
        vseg(surface, g.cx(th), th, start, cell.bottom(), color);
    }
    if matches!(arms[LEFT], L | H) {
        let th = g.thickness(arms[LEFT]);
        let end = if both_v {
            a
        } else if one_v {
            b + t
        } else {
            g.cx(th) + th as i32
        };
        hseg(surface, g.cy(th), th, cell.x, end, color);
    }
    if matches!(arms[RIGHT], L | H) {
        let th = g.thickness(arms[RIGHT]);
        let start = if both_v {
            b + t
        } else if one_v {
            a
        } else {
            g.cx(th)
        };
        hseg(surface, g.cy(th), th, start, cell.right(), color);
    }
}

/// Двойные плечи.
///
/// Каждая из двух линий плеча кончается там, где встречает линию соседнего
/// двойного плеча: со стороны соседа — на внутренней линии (`p + t` у верхнего
/// плеча, если двойное есть слева), с противоположной — на внешней (`q + t`).
/// Без двойных соседей линия доходит до центра и сливается с продолжением.
/// Проверка на глаз — `╔`: внешняя пара сходится в верхнем левом углу,
/// внутренняя — на точку правее и ниже.
fn double_arms(surface: &mut Surface, g: &Geometry, arms: Arms, color: Color) {
    let cell = g.cell;
    let t = g.t;
    let ti = t as i32;
    let (a, b) = g.double_x();
    let (p, q) = g.double_y();
    let c_x = g.cx(t);
    let c_y = g.cy(t);
    let left = arms[LEFT] == D;
    let right = arms[RIGHT] == D;
    let up = arms[UP] == D;
    let down = arms[DOWN] == D;

    if up {
        let end_a = if left { p + ti } else if right { q + ti } else { c_y + ti };
        let end_b = if right { p + ti } else if left { q + ti } else { c_y + ti };
        vseg(surface, a, t, cell.y, end_a, color);
        vseg(surface, b, t, cell.y, end_b, color);
    }
    if down {
        let start_a = if left { q } else if right { p } else { c_y };
        let start_b = if right { q } else if left { p } else { c_y };
        vseg(surface, a, t, start_a, cell.bottom(), color);
        vseg(surface, b, t, start_b, cell.bottom(), color);
    }
    if left {
        let end_p = if up { a + ti } else if down { b + ti } else { c_x + ti };
        let end_q = if down { a + ti } else if up { b + ti } else { c_x + ti };
        hseg(surface, p, t, cell.x, end_p, color);
        hseg(surface, q, t, cell.x, end_q, color);
    }
    if right {
        let start_p = if up { b } else if down { a } else { c_x };
        let start_q = if down { b } else if up { a } else { c_x };
        hseg(surface, p, t, start_p, cell.right(), color);
        hseg(surface, q, t, start_q, cell.right(), color);
    }
}

/// Блоки `U+2580–U+259F`: половины, восьмушки, четверти и тени.
fn block(surface: &mut Surface, cell: Rect, ch: char, color: Color) -> bool {
    let (x, y, w, h) = (cell.x, cell.y, cell.w, cell.h);
    // Доля высоты или ширины в восьмых; округление вниз, но не до нуля — иначе
    // «нижняя восьмушка» на ячейке ниже восьми точек исчезла бы совсем.
    let eighths = |total: u32, n: u32| ((total * n) / 8).max(1);
    let fill = |surface: &mut Surface, rect: Rect, alpha: u8| {
        draw::blend_rect(surface, rect, color, alpha);
    };
    let half_w = w / 2;
    let half_h = h / 2;
    let code = ch as u32;
    match code {
        0x2580 => fill(surface, Rect::new(x, y, w, half_h), 255),
        0x2581..=0x2587 => {
            let part = eighths(h, code - 0x2580);
            fill(surface, Rect::new(x, y + (h - part) as i32, w, part), 255);
        }
        0x2588 => fill(surface, cell, 255),
        0x2589..=0x258F => {
            let part = eighths(w, 0x2590 - code);
            fill(surface, Rect::new(x, y, part, h), 255);
        }
        0x2590 => fill(surface, Rect::new(x + half_w as i32, y, w - half_w, h), 255),
        // Тени — прозрачностью, а не узором в шахматку: узор из точек на
        // ячейке в семь точек шириной рябит и меняется при каждом сдвиге окна.
        0x2591 => fill(surface, cell, 64),
        0x2592 => fill(surface, cell, 128),
        0x2593 => fill(surface, cell, 191),
        0x2594 => fill(surface, Rect::new(x, y, w, eighths(h, 1)), 255),
        0x2595 => {
            let part = eighths(w, 1);
            fill(surface, Rect::new(x + (w - part) as i32, y, part, h), 255);
        }
        0x2596..=0x259F => {
            // Четверти: бит на каждую — верхняя левая, верхняя правая, нижняя
            // левая, нижняя правая.
            let mask = match code {
                0x2596 => 0b0100,
                0x2597 => 0b1000,
                0x2598 => 0b0001,
                0x2599 => 0b1101,
                0x259A => 0b1001,
                0x259B => 0b0111,
                0x259C => 0b1011,
                0x259D => 0b0010,
                0x259E => 0b0110,
                _ => 0b1110,
            };
            let quads = [
                Rect::new(x, y, half_w, half_h),
                Rect::new(x + half_w as i32, y, w - half_w, half_h),
                Rect::new(x, y + half_h as i32, half_w, h - half_h),
                Rect::new(x + half_w as i32, y + half_h as i32, w - half_w, h - half_h),
            ];
            for (bit, rect) in quads.iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    fill(surface, *rect, 255);
                }
            }
        }
        _ => return false,
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const INK: Color = Color::rgb(0xFF, 0xFF, 0xFF);
    const BACK: Color = Color::rgb(0, 0, 0);

    /// Ячейка терминала обычного ряда: 7×17.
    ///
    /// Формат пикселя ставится явно: без него [`Color::pixel`] отвечает нулём на
    /// любой цвет, и «закрашено» от «пусто» не отличить.
    fn render(ch: char) -> Surface {
        crate::use_raw_format(crate::PIXEL_RGB);
        let mut surface = Surface::new(7, 17, BACK).expect("surface");
        assert!(draw(&mut surface, Rect::new(0, 0, 7, 17), ch, INK));
        surface
    }

    fn lit(surface: &Surface, x: u32, y: u32) -> bool {
        surface.get(x, y) == INK.pixel()
    }

    /// Ради этого модуль и написан: вертикаль касается и верхнего, и нижнего
    /// края, иначе рамка из строк выйдет пунктиром.
    #[test]
    fn vertical_line_reaches_both_edges() {
        let s = render('│');
        assert!(lit(&s, 3, 0));
        assert!(lit(&s, 3, 16));
        assert!(!lit(&s, 0, 8));
    }

    #[test]
    fn horizontal_line_reaches_both_edges() {
        let s = render('─');
        assert!(lit(&s, 0, 8));
        assert!(lit(&s, 6, 8));
        assert!(!lit(&s, 3, 0));
    }

    /// Угол `┌` сомкнут: точка стыка закрашена, наружу ничего не торчит.
    #[test]
    fn light_corner_is_closed() {
        let s = render('┌');
        assert!(lit(&s, 3, 8));
        assert!(lit(&s, 6, 8));
        assert!(lit(&s, 3, 16));
        assert!(!lit(&s, 3, 0));
        assert!(!lit(&s, 0, 8));
    }

    /// `╔`: внешняя пара сходится в точке (2, 7), внутренняя — в (4, 9), а
    /// между ними просвет.
    #[test]
    fn double_corner_has_outer_and_inner_lines() {
        let s = render('╔');
        // Внешний угол.
        assert!(lit(&s, 2, 7));
        assert!(lit(&s, 6, 7));
        assert!(lit(&s, 2, 16));
        // Внутренний угол.
        assert!(lit(&s, 4, 9));
        assert!(lit(&s, 6, 9));
        assert!(lit(&s, 4, 16));
        // Просвет между линиями и отсутствие хвостов наружу.
        assert!(!lit(&s, 3, 8));
        assert!(!lit(&s, 3, 16));
        assert!(!lit(&s, 1, 7));
        assert!(!lit(&s, 2, 6));
    }

    /// `╬`: четыре внутренних угла, центр пуст.
    #[test]
    fn double_cross_leaves_the_centre_empty() {
        let s = render('╬');
        assert!(!lit(&s, 3, 8));
        assert!(lit(&s, 2, 0));
        assert!(lit(&s, 4, 16));
        assert!(lit(&s, 0, 7));
        assert!(lit(&s, 6, 9));
        // Внутренние линии не пересекают друг друга.
        assert!(!lit(&s, 3, 7));
        assert!(!lit(&s, 3, 9));
    }

    /// `╟` — разделитель Far внутри двойной рамки: вертикали сплошные,
    /// одинарная линия начинается от правой из них.
    #[test]
    fn double_tee_with_single_arm() {
        let s = render('╟');
        for y in 0..17 {
            assert!(lit(&s, 2, y), "левая вертикаль прервана на {y}");
            assert!(lit(&s, 4, y), "правая вертикаль прервана на {y}");
        }
        assert!(lit(&s, 5, 8));
        assert!(!lit(&s, 3, 8));
    }

    #[test]
    fn blocks_fill_their_share() {
        let s = render('▀');
        assert!(lit(&s, 0, 0));
        assert!(!lit(&s, 0, 16));
        let s = render('█');
        assert!(lit(&s, 6, 16));
    }

    #[test]
    fn shade_is_between_back_and_ink() {
        let s = render('▒');
        let pixel = Color::from_pixel(s.get(3, 8));
        assert!(pixel != INK && pixel != BACK);
    }

    #[test]
    fn letters_are_left_to_the_font() {
        let mut surface = Surface::new(7, 17, BACK).expect("surface");
        assert!(!draw(&mut surface, Rect::new(0, 0, 7, 17), 'A', INK));
        assert!(!covers('Я'));
        assert!(covers('═'));
    }
}
