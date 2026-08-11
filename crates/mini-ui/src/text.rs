//! Текст на поверхности: глифы и сетка символов с прокруткой.
//!
//! Растровый шрифт — [`crate::font`]: массив из восьми байт на глиф, по байту
//! на строку. Рисуется он не в фреймбуфер, а в [`Surface`], и из этого следует
//! всё остальное — прокрутку можно сделать сдвигом пикселей, потому что
//! поверхность, в отличие от экрана, читается.
//!
//! # Учёт изменённого
//!
//! [`TextGrid`] запоминает, какая область поверхности изменилась с прошлого
//! [`TextGrid::take_damage`]. Это не преждевременная оптимизация: набор одной
//! строки меняет одну строку ячеек, и вывести на экран 900×16 пикселей вместо
//! 900×520 — разница в тридцать раз на каждое нажатие клавиши. Разница
//! чувствуется руками: экран — память устройства, и запись в него стоит дорого.

use alloc::vec::Vec;

use crate::font;
use crate::{Color, Rect, Surface};

/// Размер глифа в таблице шрифта.
pub const GLYPH_W: u32 = font::GLYPH_W;
pub const GLYPH_H: u32 = font::GLYPH_H;

/// Ширина строки в пикселях при заданном масштабе.
///
/// Считается по символам, а не по байтам: в UTF-8 кириллическая буква занимает
/// два байта, и надпись, отцентрованная по длине строки в байтах, уехала бы
/// ровно вдвое.
#[must_use]
pub fn width_of(text: &str, scale: u32) -> u32 {
    text.chars().count() as u32 * GLYPH_W * scale
}

/// Нарисовать один глиф.
///
/// `bg` равное `None` означает прозрачный фон: рисуются только точки самого
/// глифа. Нужно для надписей поверх уже нарисованного (заголовок окна), тогда
/// как ячейка терминала обязана закрашивать фон — иначе прежний символ
/// просвечивал бы сквозь новый.
pub fn draw_glyph(
    surface: &mut Surface,
    x: u32,
    y: u32,
    ch: char,
    scale: u32,
    fg: Color,
    bg: Option<Color>,
) {
    let glyph = font::glyph(ch);
    let fg = fg.pixel();
    let bg = bg.map(Color::pixel);

    for (gy, bits) in glyph.iter().copied().enumerate() {
        for gx in 0..GLYPH_W {
            // Младший бит байта — самый ЛЕВЫЙ пиксель строки (формат
            // унаследован от C-заголовка font8x8_basic.h), поэтому сдвигаем
            // вправо на номер столбца, а не на 7 - столбец.
            let lit = (bits >> gx) & 1 != 0;
            let pixel = if lit {
                fg
            } else {
                match bg {
                    Some(bg) => bg,
                    None => continue,
                }
            };
            for sy in 0..scale {
                for sx in 0..scale {
                    surface.put(x + gx * scale + sx, y + gy as u32 * scale + sy, pixel);
                }
            }
        }
    }
}

/// Нарисовать строку. Возвращает занятую ею область.
pub fn draw_text(
    surface: &mut Surface,
    x: u32,
    y: u32,
    text: &str,
    scale: u32,
    fg: Color,
    bg: Option<Color>,
) -> Rect {
    let mut cursor = x;
    for ch in text.chars() {
        draw_glyph(surface, cursor, y, ch, scale, fg, bg);
        cursor += GLYPH_W * scale;
    }
    Rect::new(x as i32, y as i32, cursor - x, GLYPH_H * scale)
}

/// Сетка символов внутри поверхности: то, что делает из окна терминал.
pub struct TextGrid {
    /// Где в поверхности начинается сетка.
    origin: (u32, u32),
    cols: u32,
    rows: u32,
    scale: u32,
    /// Что сейчас в каждой ячейке. Нужно для прокрутки и для восстановления
    /// того, что было под курсором.
    ///
    /// Байты, а не символы: сетку заполняет терминал, а он работает с потоком
    /// ASCII. Байт на ячейку вместо четырёх — это не экономия ради экономии, а
    /// разница в теневом буфере на экран 200×60.
    cells: Vec<u8>,
    col: u32,
    row: u32,
    fg: Color,
    bg: Color,
    /// Нарисован ли курсор прямо сейчас.
    cursor_drawn: bool,
    /// Показывать ли курсор вообще.
    cursor_enabled: bool,
    /// Что изменилось с прошлого [`TextGrid::take_damage`].
    damage: Rect,
}

/// Шаг табуляции в символах.
const TAB_STOP: u32 = 8;

impl TextGrid {
    /// Создать сетку, занимающую область `area` поверхности.
    ///
    /// Возвращает `None`, если в область не помещается ни одна ячейка или не
    /// хватило памяти под теневой буфер.
    #[must_use]
    pub fn new(area: Rect, scale: u32, fg: Color, bg: Color) -> Option<Self> {
        let scale = scale.max(1);
        let cell_w = GLYPH_W * scale;
        let cell_h = GLYPH_H * scale;
        let cols = area.w / cell_w;
        let rows = area.h / cell_h;
        if cols == 0 || rows == 0 || area.x < 0 || area.y < 0 {
            return None;
        }

        let len = (cols as usize).checked_mul(rows as usize)?;
        let mut cells = Vec::new();
        cells.try_reserve_exact(len).ok()?;
        cells.resize(len, b' ');

        Some(Self {
            origin: (area.x as u32, area.y as u32),
            cols,
            rows,
            scale,
            cells,
            col: 0,
            row: 0,
            fg,
            bg,
            cursor_drawn: false,
            cursor_enabled: false,
            damage: Rect::EMPTY,
        })
    }

    #[must_use]
    pub const fn cols(&self) -> u32 {
        self.cols
    }

    #[must_use]
    pub const fn rows(&self) -> u32 {
        self.rows
    }

    /// Область поверхности, занимаемая сеткой.
    #[must_use]
    pub const fn area(&self) -> Rect {
        Rect {
            x: self.origin.0 as i32,
            y: self.origin.1 as i32,
            w: self.cols * GLYPH_W * self.scale,
            h: self.rows * GLYPH_H * self.scale,
        }
    }

    /// Забрать накопленную область изменений и начать копить заново.
    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    fn mark(&mut self, rect: Rect) {
        self.damage = self.damage.union(&rect);
    }

    fn cell_rect(&self, col: u32, row: u32) -> Rect {
        Rect::new(
            (self.origin.0 + col * GLYPH_W * self.scale) as i32,
            (self.origin.1 + row * GLYPH_H * self.scale) as i32,
            GLYPH_W * self.scale,
            GLYPH_H * self.scale,
        )
    }

    /// Очистить сетку и залить её область фоном.
    pub fn clear(&mut self, surface: &mut Surface) {
        self.cells.fill(b' ');
        self.col = 0;
        self.row = 0;
        self.cursor_drawn = false;
        let area = self.area();
        surface.fill(area, self.bg);
        self.mark(area);
    }

    /// Показывать или не показывать курсор.
    pub fn set_cursor(&mut self, surface: &mut Surface, visible: bool) {
        self.cursor_enabled = visible;
        if visible {
            self.draw_cursor(surface);
        } else {
            self.erase_cursor(surface);
        }
    }

    /// Курсор — подчёркивание под текущей ячейкой.
    ///
    /// Подчёркивание, а не заливка ячейки: блок скрыл бы символ под собой, а
    /// курсор стоит именно там, где только что напечатан символ.
    fn draw_cursor(&mut self, surface: &mut Surface) {
        if !self.cursor_enabled
            || self.cursor_drawn
            || self.col >= self.cols
            || self.row >= self.rows
        {
            return;
        }
        let cell = self.cell_rect(self.col, self.row);
        let line = Rect::new(cell.x, cell.bottom() - self.scale as i32, cell.w, self.scale);
        surface.fill(line, self.fg);
        self.mark(line);
        self.cursor_drawn = true;
    }

    fn erase_cursor(&mut self, surface: &mut Surface) {
        if !self.cursor_drawn {
            return;
        }
        self.cursor_drawn = false;
        if self.col >= self.cols || self.row >= self.rows {
            return;
        }
        // Перерисовываем ячейку целиком из теневого буфера: след курсора при
        // этом исчезает вместе с фоном, и знать, что именно было под ним, не
        // требуется.
        let byte = self.cells[(self.row * self.cols + self.col) as usize];
        self.draw_cell(surface, self.col, self.row, byte);
    }

    fn draw_cell(&mut self, surface: &mut Surface, col: u32, row: u32, byte: u8) {
        let cell = self.cell_rect(col, row);
        draw_glyph(
            surface,
            cell.x as u32,
            cell.y as u32,
            char::from(byte),
            self.scale,
            self.fg,
            Some(self.bg),
        );
        self.mark(cell);
    }

    fn put_cell(&mut self, surface: &mut Surface, col: u32, row: u32, byte: u8) {
        let index = (row * self.cols + col) as usize;
        if self.cells[index] == byte {
            return;
        }
        self.cells[index] = byte;
        self.draw_cell(surface, col, row, byte);
    }

    /// Сдвинуть содержимое на строку вверх.
    fn scroll(&mut self, surface: &mut Surface) {
        let cols = self.cols as usize;
        self.cells.copy_within(cols.., 0);
        let last = (self.rows as usize - 1) * cols;
        self.cells[last..].fill(b' ');

        // Пиксели сдвигаются сдвигом, а не перерисовкой глифов: копирование
        // внутри обычной памяти на порядок дешевле, чем нарисовать заново
        // несколько тысяч ячеек.
        let area = self.area();
        surface.scroll_up(area, GLYPH_H * self.scale, self.bg);
        self.mark(area);

        self.col = 0;
        self.row = self.rows - 1;
    }

    fn newline(&mut self, surface: &mut Surface) {
        self.col = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
        } else {
            self.scroll(surface);
        }
    }

    fn tab(&mut self, surface: &mut Surface) {
        let stop = (self.col / TAB_STOP + 1) * TAB_STOP;
        if stop >= self.cols {
            self.newline(surface);
            return;
        }
        while self.col < stop {
            self.put_cell(surface, self.col, self.row, b' ');
            self.col += 1;
        }
    }

    /// Напечатать строку.
    pub fn write_str(&mut self, surface: &mut Surface, text: &str) {
        let had_cursor = self.cursor_drawn;
        self.erase_cursor(surface);
        for ch in text.chars() {
            self.write_char(surface, ch);
        }
        if had_cursor || self.cursor_enabled {
            self.draw_cursor(surface);
        }
    }

    fn write_char(&mut self, surface: &mut Surface, ch: char) {
        match ch {
            '\n' => {
                self.newline(surface);
                return;
            }
            '\r' => {
                self.col = 0;
                return;
            }
            '\t' => {
                self.tab(surface);
                return;
            }
            // Возврат на позицию. Символ не стирается — так же, как в любом
            // терминале: стирание делает последовательность «возврат, пробел,
            // возврат», и решает это тот, кто печатает.
            '\u{8}' => {
                self.col = self.col.saturating_sub(1);
                return;
            }
            _ => {}
        }
        // Перенос по правому краю: строка длиннее сетки продолжается снизу и
        // может утащить за собой прокрутку, поэтому проверка идёт до вывода.
        if self.col >= self.cols {
            self.newline(surface);
        }
        let byte = if (0x20..0x7F).contains(&(ch as u32)) { ch as u8 } else { b'?' };
        self.put_cell(surface, self.col, self.row, byte);
        self.col += 1;
    }
}
