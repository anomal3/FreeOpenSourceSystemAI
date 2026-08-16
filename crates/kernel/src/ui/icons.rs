//! Значки рабочего стола: то, что лежит на самом столе и открывается щелчком.
//!
//! # Почему значки — не окна и не поверхность
//!
//! Окно держит собственную поверхность в памяти, потому что его содержимое
//! меняется само по себе: терминал печатает, список файлов листается. Значок не
//! меняется вовсе — он рисуется поверх фона теми же примитивами, что и фон, и
//! стоит ровно столько же. Отдельная поверхность на каждый значок означала бы
//! мегабайты памяти под картинку, которую можно нарисовать шестью заливками.
//!
//! # Почему открытие по двойному щелчку
//!
//! Потому что одиночный нужен, чтобы значок выбрать, а выбранный значок —
//! единственный способ показать человеку, что система вообще заметила его
//! щелчок. Так это устроено везде, где человек уже видел рабочий стол, и
//! придумывать здесь своё — значит заставлять переучиваться ради ничего.

use mini_ui::text::{self};
use mini_ui::{Color, Rect, Screen};

use super::theme;
use super::window::App;

/// Сторона картинки значка при масштабе 1.
const ART: u32 = 32;
/// Ширина ячейки значка вместе с подписью, при масштабе 1.
const CELL_W: u32 = 96;
/// Высота ячейки значка, при масштабе 1.
const CELL_H: u32 = 64;
/// Отступ сетки значков от края экрана, при масштабе 1.
const MARGIN: u32 = 12;

/// Что лежит на столе и в каком порядке — сверху вниз.
///
/// Порядок не алфавитный и не случайный: сначала то, чем человек пользуется,
/// открыв систему впервые («здесь мои файлы»), затем инструменты. Список
/// короткий намеренно — стол, засыпанный значками, ничем не лучше пустого.
const ITEMS: [(App, &str); 4] = [
    (App::Files, "This computer"),
    (App::Terminal, "Terminal"),
    (App::Settings, "Settings"),
    (App::About, "About"),
];

/// Значки стола: их места и то, какой из них выбран.
pub struct Icons {
    /// Масштаб — тот же, что у всего стола.
    scale: u32,
    /// Выбранный значок, если человек по нему щёлкнул.
    selected: Option<App>,
}

impl Icons {
    #[must_use]
    pub const fn new(scale: u32) -> Self {
        Self { scale, selected: None }
    }

    /// Прямоугольник ячейки значка с номером `index`.
    fn cell(&self, index: usize) -> Rect {
        let scale = self.scale;
        Rect::new(
            (MARGIN * scale) as i32,
            (MARGIN * scale + index as u32 * CELL_H * scale) as i32,
            CELL_W * scale,
            CELL_H * scale,
        )
    }

    /// Все ячейки вместе — область, которую занимает сетка значков.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        let last = self.cell(ITEMS.len().saturating_sub(1));
        Rect::new(
            (MARGIN * self.scale) as i32,
            (MARGIN * self.scale) as i32,
            CELL_W * self.scale,
            (last.bottom() - (MARGIN * self.scale) as i32).max(0) as u32,
        )
    }

    /// Какой значок лежит под точкой экрана.
    #[must_use]
    pub fn at(&self, x: i32, y: i32) -> Option<App> {
        ITEMS
            .iter()
            .enumerate()
            .find(|(index, _)| self.cell(*index).contains(x, y))
            .map(|(_, (app, _))| *app)
    }

    /// Выбрать значок. Возвращает область, которую надо перерисовать.
    pub fn select(&mut self, app: Option<App>) -> Rect {
        if self.selected == app {
            return Rect::EMPTY;
        }
        let previous = self.selected;
        self.selected = app;
        // Перерисовать надо и то, что перестало быть выбранным: подсветка
        // снимается ровно так же, как ставится.
        let mut damage = Rect::EMPTY;
        for (index, (item, _)) in ITEMS.iter().enumerate() {
            if Some(*item) == previous || Some(*item) == app {
                damage = damage.union(&self.cell(index));
            }
        }
        damage
    }

    /// Нарисовать значки, попадающие в `rect`.
    ///
    /// Рисует прямо на экран, между фоном и окнами: значок — часть стола, и
    /// окно, наехавшее на него, обязано его закрывать.
    pub fn draw(&self, screen: &Screen, rect: Rect) {
        for (index, (app, label)) in ITEMS.iter().enumerate() {
            let cell = self.cell(index);
            if cell.intersect(&rect).is_empty() {
                continue;
            }
            self.draw_one(screen, cell, rect, *app, label, self.selected == Some(*app));
        }
    }

    /// Нарисовать один значок, не выходя за `clip`.
    ///
    /// Обрезка обязательна: значок рисуется поверх фона, а собирается экран
    /// прямоугольниками изменений. Нарисованный целиком ради задетого края, он
    /// лёг бы поверх окна, которое его закрывает.
    fn draw_one(
        &self,
        screen: &Screen,
        cell: Rect,
        clip: Rect,
        app: App,
        label: &str,
        selected: bool,
    ) {
        let paint = |rect: Rect, color: Color| {
            let visible = rect.intersect(&clip);
            if !visible.is_empty() {
                screen.fill(visible, color);
            }
        };
        let scale = self.scale;
        let art = ART * scale;
        let art_x = cell.x + (cell.w.saturating_sub(art) / 2) as i32;
        let art_y = cell.y + (4 * scale) as i32;

        if selected {
            // Подсветка — по всей ячейке, а не по картинке: человек целится в
            // значок вместе с подписью, и выделять надо то, во что он целился.
            paint(cell, theme::SELECT_BG);
        }

        match app {
            // Системный блок с экраном: прямоугольник, светлое «стекло» и
            // подставка. Узнаваемость здесь важнее правдоподобия — значок
            // размером в тридцать две точки не бывает похож на настоящую вещь.
            App::Files => {
                paint(
Rect::new(art_x, art_y, art, art * 3 / 4), theme::FRAME);
                paint(
Rect::new(art_x + 3 * scale as i32, art_y + 3 * scale as i32, art - 6 * scale, art * 3 / 4 - 6 * scale),
                    theme::ACCENT,
                );
                paint(
Rect::new(art_x + (art / 3) as i32, art_y + (art * 3 / 4) as i32, art / 3, 4 * scale),
                    theme::FRAME,
                );
                paint(
Rect::new(art_x + (art / 6) as i32, art_y + (art * 3 / 4 + 4 * scale) as i32, art * 2 / 3, 3 * scale),
                    theme::FRAME,
                );
            }
            // Окно терминала с приглашением: рамка и две чёрточки.
            App::Terminal => {
                paint(
Rect::new(art_x, art_y, art, art), theme::FRAME);
                paint(
Rect::new(art_x + 2 * scale as i32, art_y + (6 * scale) as i32, art - 4 * scale, art - 8 * scale),
                    Color::rgb(0x06, 0x10, 0x18),
                );
                text::draw_text_on_screen(
                    screen,
                    (art_x + 5 * scale as i32) as u32,
                    (art_y + 11 * scale as i32) as u32,
                    ">_",
                    scale,
                    theme::DIRECTORY,
                    clip,
                );
            }
            // Шестерёнка: круг из четырёх зубцов вокруг квадрата. На такой
            // сетке настоящая шестерёнка превращается в кашу, а этот силуэт
            // читается.
            App::Settings => {
                let centre = (art_x + (art / 2) as i32, art_y + (art / 2) as i32);
                let arm = (art / 3) as i32;
                let thick = 6 * scale;
                paint(
Rect::new(centre.0 - (thick / 2) as i32, centre.1 - arm, thick, arm as u32 * 2),
                    theme::DIM,
                );
                paint(
Rect::new(centre.0 - arm, centre.1 - (thick / 2) as i32, arm as u32 * 2, thick),
                    theme::DIM,
                );
                paint(
Rect::new(centre.0 - (art / 4) as i32, centre.1 - (art / 4) as i32, art / 2, art / 2),
                    theme::ACCENT,
                );
                paint(
Rect::new(centre.0 - (art / 8) as i32, centre.1 - (art / 8) as i32, art / 4, art / 4),
                    theme::DESKTOP_TOP,
                );
            }
            // Буква «i» в круге — то, чем «сведения» обозначены везде.
            _ => {
                paint(
Rect::new(art_x, art_y, art, art), theme::ACCENT);
                paint(
Rect::new(art_x + 3 * scale as i32, art_y + 3 * scale as i32, art - 6 * scale, art - 6 * scale),
                    theme::WINDOW_BG,
                );
                text::draw_text_on_screen(
                    screen,
                    (art_x + (art / 2) as i32 - (text::GLYPH_W * scale / 2) as i32) as u32,
                    (art_y + (art / 2) as i32 - (text::GLYPH_H * scale / 2) as i32) as u32,
                    "i",
                    scale,
                    theme::TEXT,
                    clip,
                );
            }
        }

        // Подпись — по центру ячейки. Длинную обрезаем, а не переносим: две
        // строки под значком сделали бы сетку неровной, а подписи у нас свои и
        // короткие.
        let text_w = text::width_of(label, scale);
        let text_x = cell.x + ((cell.w.saturating_sub(text_w)) / 2) as i32;
        let text_y = art_y + (ART * scale + 4 * scale) as i32;
        text::draw_text_on_screen(
            screen,
            text_x.max(cell.x) as u32,
            text_y as u32,
            label,
            scale,
            theme::TEXT,
            clip,
        );
    }
}
