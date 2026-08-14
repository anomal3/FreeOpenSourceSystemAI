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

/// Шестнадцать цветов терминала — те самые, которые ANSI нумерует от 30 до 37 и
/// от 90 до 97.
///
/// Значения взяты не из спецификации (её в этой части нет вовсе — цвета там
/// названы словами), а подобраны под тёмный фон системы: «чёрный» чуть светлее
/// фона окна, иначе текст им напечатанный исчезал бы совсем, а яркая половина
/// действительно ярче тусклой, а не просто другая.
pub const PALETTE: [Color; 16] = [
    Color::rgb(0x14, 0x20, 0x2C), // 0 чёрный
    Color::rgb(0xC0, 0x4C, 0x50), // 1 красный
    Color::rgb(0x5E, 0xA8, 0x74), // 2 зелёный
    Color::rgb(0xC0, 0xA0, 0x50), // 3 жёлтый
    Color::rgb(0x3C, 0x8C, 0xC8), // 4 синий
    Color::rgb(0xA0, 0x70, 0xB8), // 5 пурпурный
    Color::rgb(0x50, 0xA8, 0xB0), // 6 голубой
    Color::rgb(0xD8, 0xE2, 0xEC), // 7 белый
    Color::rgb(0x4A, 0x5C, 0x70), // 8 яркий чёрный (серый)
    Color::rgb(0xE0, 0x70, 0x74), // 9 яркий красный
    Color::rgb(0x86, 0xD0, 0x9C), // 10 яркий зелёный
    Color::rgb(0xE8, 0xC8, 0x78), // 11 яркий жёлтый
    Color::rgb(0x6C, 0xB4, 0xE8), // 12 яркий синий
    Color::rgb(0xC8, 0x98, 0xE0), // 13 яркий пурпурный
    Color::rgb(0x7C, 0xD0, 0xD8), // 14 яркий голубой
    Color::rgb(0xFF, 0xFF, 0xFF), // 15 яркий белый
];

/// Атрибут ячейки, означающий «цвета окна, а не палитры».
///
/// Отдельное значение, а не пара индексов, потому что цвет окна задаётся темой и
/// не обязан совпадать ни с одним из шестнадцати. Ячейка, которой никто не
/// назначал цвета, обязана перекрашиваться вместе с темой, а не остаться серой
/// навсегда.
const ATTR_DEFAULT: u8 = 0xFF;

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
    /// Цвет каждой ячейки: индекс палитры в младших четырёх битах для текста, в
    /// старших — для фона, либо [`ATTR_DEFAULT`].
    ///
    /// Байт на ячейку, как и у символа: терминалу шестнадцати цветов больше не
    /// нужно, а хранить два `Color` на ячейку значило бы платить шесть байт за
    /// экран, где почти все ячейки одного цвета.
    attrs: Vec<u8>,
    /// Цвет, которым печатается следующий символ.
    attr: u8,
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
        let mut attrs = Vec::new();
        attrs.try_reserve_exact(len).ok()?;
        attrs.resize(len, ATTR_DEFAULT);

        Some(Self {
            origin: (area.x as u32, area.y as u32),
            cols,
            rows,
            scale,
            cells,
            attrs,
            attr: ATTR_DEFAULT,
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
        self.attrs.fill(ATTR_DEFAULT);
        self.col = 0;
        self.row = 0;
        self.cursor_drawn = false;
        let area = self.area();
        surface.fill(area, self.bg);
        self.mark(area);
    }

    /// Цвета ячейки по её атрибуту: текст и фон.
    fn colors(&self, attr: u8) -> (Color, Color) {
        if attr == ATTR_DEFAULT {
            return (self.fg, self.bg);
        }
        (
            PALETTE[(attr & 0x0F) as usize],
            PALETTE[(attr >> 4) as usize],
        )
    }

    /// Поставить цвет текста для последующего вывода.
    ///
    /// Индекс — из шестнадцати цветов [`PALETTE`]. Фон при этом сохраняется тот,
    /// что был: `ESC [ 31 m` в терминале означает «красный текст», а не
    /// «красный текст на чёрном».
    pub fn set_fg(&mut self, index: u8) {
        let bg = if self.attr == ATTR_DEFAULT {
            // У ячейки, не имевшей цвета, фон был цветом окна; ближайший к нему
            // в палитре — «чёрный», он для того и подобран.
            0
        } else {
            self.attr >> 4
        };
        self.attr = (index & 0x0F) | (bg << 4);
    }

    /// Поставить цвет фона для последующего вывода.
    pub fn set_bg(&mut self, index: u8) {
        let fg = if self.attr == ATTR_DEFAULT { 7 } else { self.attr & 0x0F };
        self.attr = fg | ((index & 0x0F) << 4);
    }

    /// Вернуть цвета окна — то, что делает `ESC [ 0 m`.
    pub fn reset_attr(&mut self) {
        self.attr = ATTR_DEFAULT;
    }

    /// Где стоит курсор: строка и столбец, считая с нуля.
    #[must_use]
    pub const fn cursor_at(&self) -> (u32, u32) {
        (self.row, self.col)
    }

    /// Поставить курсор в заданную ячейку.
    ///
    /// Координаты за пределами сетки прижимаются к её краю, а не отвергаются:
    /// так же ведёт себя всякий терминал, а программа, попросившая
    /// восьмидесятую строку у окна из двадцати четырёх, обычно просто не знала
    /// размера.
    pub fn move_to(&mut self, surface: &mut Surface, row: u32, col: u32) {
        let had_cursor = self.cursor_drawn;
        self.erase_cursor(surface);
        self.row = row.min(self.rows - 1);
        self.col = col.min(self.cols - 1);
        if had_cursor || self.cursor_enabled {
            self.draw_cursor(surface);
        }
    }

    /// Сдвинуть курсор на заданное число строк и столбцов.
    pub fn move_by(&mut self, surface: &mut Surface, rows: i32, cols: i32) {
        let row = (self.row as i32 + rows).max(0) as u32;
        let col = (self.col as i32 + cols).max(0) as u32;
        self.move_to(surface, row, col);
    }

    /// Стереть часть экрана: `0` — от курсора вниз, `1` — сверху до курсора,
    /// `2` — весь.
    ///
    /// Курсор при этом **не** двигается — в отличие от [`TextGrid::clear`]. Это
    /// не тонкость: программы очищают экран последовательностью из двух команд,
    /// вторая из которых ставит курсор, и очистка, двигающая его сама, ломала бы
    /// вывод тех, кто обходится одной.
    pub fn erase_display(&mut self, surface: &mut Surface, mode: u8) {
        let position = (self.row * self.cols + self.col) as usize;
        let total = self.cells.len();
        let (from, to) = match mode {
            0 => (position, total),
            1 => (0, position + 1),
            _ => (0, total),
        };
        self.erase_range(surface, from, to.min(total));
    }

    /// Стереть часть строки: `0` — от курсора вправо, `1` — слева до курсора,
    /// `2` — всю строку.
    pub fn erase_line(&mut self, surface: &mut Surface, mode: u8) {
        let start = (self.row * self.cols) as usize;
        let position = start + self.col as usize;
        let end = start + self.cols as usize;
        let (from, to) = match mode {
            0 => (position, end),
            1 => (start, position + 1),
            _ => (start, end),
        };
        self.erase_range(surface, from, to.min(end));
    }

    /// Залить диапазон ячеек пробелами цвета окна.
    fn erase_range(&mut self, surface: &mut Surface, from: usize, to: usize) {
        let had_cursor = self.cursor_drawn;
        self.erase_cursor(surface);
        for index in from..to {
            let col = (index as u32) % self.cols;
            let row = (index as u32) / self.cols;
            if self.cells[index] == b' ' && self.attrs[index] == ATTR_DEFAULT {
                continue;
            }
            self.cells[index] = b' ';
            self.attrs[index] = ATTR_DEFAULT;
            self.draw_cell(surface, col, row, b' ');
        }
        if had_cursor || self.cursor_enabled {
            self.draw_cursor(surface);
        }
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
        let (fg, bg) = self.colors(self.attrs[(row * self.cols + col) as usize]);
        draw_glyph(
            surface,
            cell.x as u32,
            cell.y as u32,
            char::from(byte),
            self.scale,
            fg,
            Some(bg),
        );
        self.mark(cell);
    }

    fn put_cell(&mut self, surface: &mut Surface, col: u32, row: u32, byte: u8) {
        let index = (row * self.cols + col) as usize;
        // Сравнивается и символ, и цвет: ячейка, перекрашенная под тем же
        // символом, изменилась ровно так же, как ячейка с новым символом.
        if self.cells[index] == byte && self.attrs[index] == self.attr {
            return;
        }
        self.cells[index] = byte;
        self.attrs[index] = self.attr;
        self.draw_cell(surface, col, row, byte);
    }

    /// Сдвинуть содержимое на строку вверх.
    fn scroll(&mut self, surface: &mut Surface) {
        let cols = self.cols as usize;
        self.cells.copy_within(cols.., 0);
        self.attrs.copy_within(cols.., 0);
        let last = (self.rows as usize - 1) * cols;
        self.cells[last..].fill(b' ');
        // Освободившаяся строка получает цвета окна, а не последний
        // назначенный: `scroll_up` заливает её именно фоном окна, и разойтись с
        // ним значило бы, что теневой буфер описывает не то, что на экране.
        self.attrs[last..].fill(ATTR_DEFAULT);

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
