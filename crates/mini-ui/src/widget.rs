//! Элементы интерфейса установщика: заголовок, абзац, список, поле ввода,
//! полоса хода работ.
//!
//! # Почему это не «библиотека виджетов»
//!
//! Здесь нет ни дерева элементов, ни обработчиков событий, ни разметки. Экран
//! установщика перерисовывается целиком на каждое нажатие клавиши, а элементы —
//! это функции, которые кладут прямоугольник на поверхность и говорят, сколько
//! места заняли.
//!
//! Так сделано намеренно. Немедленный режим (immediate mode) исключает целый
//! класс дефектов: состояние элемента невозможно рассинхронизировать с
//! состоянием программы, потому что у элемента нет состояния — оно всё в
//! структуре, из которой экран рисуется. Плата — перерисовка: несколько
//! миллионов операций в обычной памяти на нажатие, что на порядок дешевле
//! одного вывода на экран, ради которого всё и делается.
//!
//! # Масштаб
//!
//! Прошивка вправе дать и 800×600, и 1920×1080. Все размеры считаются от
//! [`Metrics`], который выводится из ширины экрана: интерфейс, заданный
//! пикселями, на одном разрешении был бы нечитаемым, а на другом не помещался
//! бы.

use alloc::vec::Vec;

use crate::text::{self, GLYPH_H, GLYPH_W};
use crate::{Color, Rect, Surface};

/// Палитра.
///
/// Та же, что у композитора ядра, и это не совпадение: установщик и система
/// показывают человеку одну вещь, и разная палитра выглядела бы как две разные
/// программы.
#[derive(Clone, Copy)]
pub struct Theme {
    /// Фон экрана.
    pub background: Color,
    /// Фон панели поверх фона экрана.
    pub panel: Color,
    /// Основной текст.
    pub text: Color,
    /// Пояснения и подсказки.
    pub dim: Color,
    /// Выделение: активная строка списка, рамка активного поля.
    pub accent: Color,
    /// Текст поверх выделения.
    pub accent_text: Color,
    /// То, о чём нужно предупредить (потеря данных).
    pub warning: Color,
    /// Успех.
    pub success: Color,
}

/// Тёмная палитра — единственная. Светлую заводить не из чего: выбирать её
/// человеку негде, а неиспользуемая ветка кода в интерфейсе установщика
/// означала бы, что её никто не видел.
pub const DARK: Theme = Theme {
    background: Color::rgb(0x0A, 0x14, 0x1E),
    panel: Color::rgb(0x0A, 0x1C, 0x2E),
    text: Color::rgb(0xD8, 0xE2, 0xEC),
    dim: Color::rgb(0x7C, 0x8C, 0x9C),
    accent: Color::rgb(0x3C, 0x8C, 0xC8),
    accent_text: Color::rgb(0x06, 0x10, 0x18),
    warning: Color::rgb(0xE0, 0x9C, 0x40),
    success: Color::rgb(0x60, 0xC0, 0x80),
};

/// Размеры, посчитанные от разрешения экрана.
#[derive(Clone, Copy)]
pub struct Metrics {
    /// Во сколько раз увеличен глиф основного текста.
    pub scale: u32,
    /// Масштаб заголовка.
    pub title_scale: u32,
    /// Высота строки вместе с межстрочным просветом.
    pub line: u32,
    /// Внутренний отступ панелей.
    pub padding: u32,
}

impl Metrics {
    /// Подобрать размеры под экран.
    #[must_use]
    pub fn for_screen(width: u32, height: u32) -> Self {
        // 8×8 без увеличения на экране в 1280 точек — это текст высотой в
        // полтора миллиметра. Порог по высоте нужен отдельно: экран 1920×480
        // существует (панель в машине), и втрое увеличенный текст на нём
        // оставил бы место под три строки.
        let scale = if width >= 1600 && height >= 900 {
            3
        } else if width >= 1000 && height >= 600 {
            2
        } else {
            1
        };
        Self {
            scale,
            title_scale: scale + 1,
            line: GLYPH_H * scale + scale * 2,
            padding: GLYPH_W * scale,
        }
    }

    /// Сколько символов помещается в ширину `width`.
    #[must_use]
    pub const fn columns(&self, width: u32) -> u32 {
        width / (GLYPH_W * self.scale)
    }

    /// Высота строки списка.
    ///
    /// Больше строки текста: у выделенной строки фон залит на всю высоту, и
    /// при межстрочном просвете в два пикселя заливка вплотную подходит к
    /// соседним строкам — на экране это читается как слипшийся текст.
    #[must_use]
    pub const fn row(&self) -> u32 {
        self.line + self.scale * 4
    }
}

/// Курсор вертикальной раскладки.
///
/// Элементы кладутся сверху вниз, каждый сообщает, сколько занял. Ни один
/// экран установщика не требует большего, а полноценная раскладка — это ещё
/// один слой, который некому проверять.
pub struct Canvas<'a> {
    surface: &'a mut Surface,
    theme: Theme,
    metrics: Metrics,
    /// Область, внутри которой идёт раскладка.
    area: Rect,
    /// Текущая позиция по вертикали в координатах поверхности.
    y: i32,
}

impl<'a> Canvas<'a> {
    #[must_use]
    pub fn new(surface: &'a mut Surface, area: Rect, theme: Theme, metrics: Metrics) -> Self {
        let y = area.y;
        Self { surface, theme, metrics, area, y }
    }

    #[must_use]
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    #[must_use]
    pub const fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Сколько места по вертикали осталось.
    #[must_use]
    pub const fn remaining(&self) -> u32 {
        let left = self.area.bottom() - self.y;
        if left > 0 { left as u32 } else { 0 }
    }

    /// Отступ на `lines` строк.
    pub fn gap(&mut self, lines: u32) {
        self.y += (self.metrics.line * lines) as i32;
    }

    /// Строка текста заданным цветом.
    pub fn line(&mut self, text: &str, color: Color) {
        self.draw_line(text, color, self.metrics.scale);
    }

    /// Строка основным цветом.
    pub fn body(&mut self, text: &str) {
        self.line(text, self.theme.text);
    }

    /// Пояснение — приглушённым цветом.
    pub fn hint(&mut self, text: &str) {
        self.line(text, self.theme.dim);
    }

    fn draw_line(&mut self, line: &str, color: Color, scale: u32) {
        if self.y < self.area.y || self.y + (GLYPH_H * scale) as i32 > self.area.bottom() {
            // Молча не рисуем то, что не помещается: обрезка по границе лучше,
            // чем текст, наползающий на подсказки внизу экрана.
            self.y += (GLYPH_H * scale + scale * 2) as i32;
            return;
        }
        text::draw_text(
            self.surface,
            self.area.x as u32,
            self.y as u32,
            line,
            scale,
            color,
            None,
        );
        self.y += (GLYPH_H * scale + scale * 2) as i32;
    }

    /// Абзац с переносом по словам.
    pub fn paragraph(&mut self, text: &str, color: Color) {
        let columns = self.metrics.columns(self.area.w).max(1);
        for line in wrap(text, columns as usize) {
            self.line(line, color);
        }
    }

    /// Список с выделенной строкой.
    ///
    /// Выделение — залитая полоса на всю ширину, а не рамка вокруг текста:
    /// строки списка разной длины, и рамка по тексту прыгала бы туда-сюда при
    /// движении по списку.
    pub fn list(&mut self, items: &[&str], selected: usize) {
        let height = self.metrics.row();
        // Глиф ставится по центру строки, а не к её верху: иначе выделение
        // выглядит съехавшим вниз относительно надписи.
        let inset = (height - GLYPH_H * self.metrics.scale) / 2;
        for (index, item) in items.iter().enumerate() {
            let row = Rect::new(self.area.x, self.y, self.area.w, height);
            let color = if index == selected {
                self.surface.fill(row, self.theme.accent);
                self.theme.accent_text
            } else {
                self.theme.text
            };
            text::draw_text(
                self.surface,
                (self.area.x + (self.metrics.scale * 2) as i32) as u32,
                (self.y + inset as i32) as u32,
                item,
                self.metrics.scale,
                color,
                None,
            );
            self.y += height as i32;
        }
    }

    /// Поле ввода.
    ///
    /// `masked` заменяет содержимое звёздочками, но длину сохраняет: человеку
    /// надо видеть, что нажатие вообще дошло.
    pub fn field(&mut self, label: &str, value: &str, masked: bool, focused: bool) {
        self.line(label, if focused { self.theme.text } else { self.theme.dim });

        let height = GLYPH_H * self.metrics.scale + self.metrics.scale * 4;
        let box_rect = Rect::new(self.area.x, self.y, self.area.w, height);
        self.surface.fill(box_rect, self.theme.panel);
        self.surface.frame(
            box_rect,
            self.metrics.scale.max(1),
            if focused { self.theme.accent } else { self.theme.dim },
        );

        let inner_x = (self.area.x + (self.metrics.scale * 2) as i32) as u32;
        let inner_y = (self.y + (self.metrics.scale * 2) as i32) as u32;
        let mut cursor = inner_x;
        for ch in value.chars() {
            let shown = if masked { '*' } else { ch };
            text::draw_glyph(
                self.surface,
                cursor,
                inner_y,
                shown,
                self.metrics.scale,
                self.theme.text,
                None,
            );
            cursor += GLYPH_W * self.metrics.scale;
        }
        // Курсор — подчёркивание после последнего символа, как в терминале.
        if focused {
            let underline = Rect::new(
                cursor as i32,
                (inner_y + GLYPH_H * self.metrics.scale) as i32,
                GLYPH_W * self.metrics.scale,
                self.metrics.scale,
            );
            self.surface.fill(underline, self.theme.accent);
        }

        self.y += (height + self.metrics.line / 2) as i32;
    }

    /// Полоса хода работ с подписью.
    pub fn progress(&mut self, done: u32, total: u32, label: &str) {
        let height = GLYPH_H * self.metrics.scale;
        let bar = Rect::new(self.area.x, self.y, self.area.w, height);
        self.surface.fill(bar, self.theme.panel);

        let total = total.max(1);
        let filled = (u64::from(bar.w) * u64::from(done.min(total)) / u64::from(total)) as u32;
        if filled > 0 {
            self.surface
                .fill(Rect::new(bar.x, bar.y, filled, height), self.theme.accent);
        }
        self.surface.frame(bar, self.metrics.scale.max(1), self.theme.dim);
        self.y += (height + self.metrics.line / 2) as i32;

        self.hint(label);
    }

    /// Горизонтальная черта — разделитель разделов экрана.
    pub fn rule(&mut self) {
        let thickness = self.metrics.scale.max(1);
        self.surface.fill(
            Rect::new(self.area.x, self.y, self.area.w, thickness),
            self.theme.dim,
        );
        self.y += (self.metrics.line) as i32;
    }
}

/// Нарисовать общую рамку экрана: фон, заголовок сверху, подсказки снизу.
///
/// Возвращает область, оставшуюся под содержимое.
pub fn frame(
    surface: &mut Surface,
    theme: Theme,
    metrics: Metrics,
    title: &str,
    step: &str,
    footer: &str,
) -> Rect {
    let bounds = surface.bounds();
    surface.fill(bounds, theme.background);

    let header_h = GLYPH_H * metrics.title_scale + metrics.padding * 2;
    let header = Rect::new(0, 0, bounds.w, header_h);
    surface.fill(header, theme.panel);
    surface.fill(
        Rect::new(0, header_h as i32 - metrics.scale as i32, bounds.w, metrics.scale),
        theme.accent,
    );
    text::draw_text(
        surface,
        metrics.padding,
        metrics.padding,
        title,
        metrics.title_scale,
        theme.text,
        None,
    );
    // Номер шага прижат к правому краю: слева он спорил бы с заголовком за
    // место, а длина заголовка от экрана к экрану разная.
    let step_w = text::width_of(step, metrics.scale);
    if step_w + metrics.padding * 2 < bounds.w {
        text::draw_text(
            surface,
            bounds.w - metrics.padding - step_w,
            metrics.padding + (GLYPH_H * metrics.title_scale - GLYPH_H * metrics.scale) / 2,
            step,
            metrics.scale,
            theme.dim,
            None,
        );
    }

    let footer_h = GLYPH_H * metrics.scale + metrics.padding * 2;
    let footer_rect = Rect::new(
        0,
        (bounds.h - footer_h) as i32,
        bounds.w,
        footer_h,
    );
    surface.fill(footer_rect, theme.panel);
    text::draw_text(
        surface,
        metrics.padding,
        bounds.h - footer_h + metrics.padding,
        footer,
        metrics.scale,
        theme.dim,
        None,
    );

    Rect::new(
        metrics.padding as i32 * 2,
        (header_h + metrics.padding) as i32,
        bounds.w - metrics.padding * 4,
        bounds.h - header_h - footer_h - metrics.padding * 2,
    )
}

/// Перенос текста по словам.
///
/// Слово длиннее строки разрывается по границе строки: альтернатива — вывести
/// его за край экрана, то есть потерять.
#[must_use]
pub fn wrap(text: &str, columns: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    if columns == 0 {
        return lines;
    }

    for paragraph in text.split('\n') {
        let mut rest = paragraph;
        loop {
            if rest.chars().count() <= columns {
                lines.push(rest);
                break;
            }
            // Байтовая граница символа, следующего за последним помещающимся.
            // Считать приходится по символам, а резать по байтам: в UTF-8 это
            // разные величины, и кириллица расходится с ASCII ровно вдвое.
            let edge = rest
                .char_indices()
                .nth(columns)
                .map_or(rest.len(), |(at, _)| at);
            let head = &rest[..edge];

            rest = if rest[edge..].starts_with(' ') {
                // Слово кончилось ровно на границе: пробел не переносим.
                lines.push(head);
                &rest[edge + 1..]
            } else if let Some(space) = head.rfind(' ') {
                lines.push(&rest[..space]);
                &rest[space + 1..]
            } else {
                // Слово длиннее строки. Разрыв посреди слова — плохо, но
                // вывести его за край экрана значит потерять целиком.
                lines.push(head);
                &rest[edge..]
            };
        }
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_on_spaces() {
        assert_eq!(
            wrap("one two three four", 9),
            alloc::vec!["one two", "three", "four"]
        );
    }

    #[test]
    fn wrap_splits_words_longer_than_the_line() {
        let lines = wrap("abcdefghijkl", 5);
        assert!(lines.iter().all(|line| line.chars().count() <= 5), "{lines:?}");
        let joined: alloc::string::String = lines.concat();
        assert_eq!(joined, "abcdefghijkl");
    }

    #[test]
    fn wrap_keeps_explicit_line_breaks() {
        assert_eq!(wrap("a\n\nb", 10), alloc::vec!["a", "", "b"]);
    }

    /// Переполнение ровно на пробеле — тот случай, на котором прежняя
    /// реализация сходила с ума: она бралась резать строку по индексу, уже
    /// оставшемуся позади, и падала на срезе с началом больше конца. На экране
    /// установщика это выглядело как мгновенная перезагрузка машины.
    #[test]
    fn wrap_survives_a_break_exactly_at_the_limit() {
        assert_eq!(wrap("abc def", 3), alloc::vec!["abc", "def"]);
        assert_eq!(wrap("ab cd ef", 5), alloc::vec!["ab cd", "ef"]);
        // Длинный текст со множеством пробелов: любая ошибка на границе
        // проявится либо паникой, либо потерянными словами. Ширина берётся от
        // самого длинного слова — на более узкой строке слова рвутся, и счёт
        // слов перестаёт быть мерой сохранности текста.
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let longest = text.split(' ').map(str::len).max().expect("текст не пуст");
        for columns in longest..30 {
            let joined = wrap(text, columns).join(" ");
            assert_eq!(joined.split_whitespace().count(), 12, "columns = {columns}");
            assert!(
                wrap(text, columns)
                    .iter()
                    .all(|line| line.chars().count() <= columns),
                "columns = {columns}"
            );
        }
    }

    /// Перенос обязан считать символы, а не байты: кириллица в UTF-8
    /// двухбайтовая, и строка «уехала бы» вдвое раньше.
    #[test]
    fn wrap_counts_characters_not_bytes() {
        let lines = wrap("привет мир", 6);
        assert_eq!(lines, alloc::vec!["привет", "мир"]);
    }

    #[test]
    fn metrics_scale_with_the_screen() {
        assert_eq!(Metrics::for_screen(800, 600).scale, 1);
        assert_eq!(Metrics::for_screen(1280, 800).scale, 2);
        assert_eq!(Metrics::for_screen(1920, 1080).scale, 3);
        // Широкая, но низкая панель не должна получить крупный шрифт.
        assert_eq!(Metrics::for_screen(1920, 480).scale, 1);
    }
}
