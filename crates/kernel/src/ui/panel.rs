//! Панель задач и меню запуска — то, что делает из набора окон рабочий стол.
//!
//! # Почему это не окна
//!
//! Панель и меню лежат поверх всех окон, не получают фокус ввода и не
//! закрываются. Сделать их окнами значило бы завести у окна три признака
//! («всегда сверху», «не фокусируется», «не закрывается»), каждый из которых
//! существует ради одного экземпляра. Дешевле и честнее: два отдельных слоя,
//! которые композитор выводит после окон.
//!
//! # Часы показывают время работы, а не время суток
//!
//! Часов реального времени ядро не читает: драйвера RTC нет, а прошивка своё
//! время после `ExitBootServices` больше не отдаёт. Показывать выдуманное время
//! суток нельзя — это ровно тот случай, когда интерфейс врёт; поэтому на панели
//! стоит время с момента загрузки, и подписано оно `up`.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::text::{self, GLYPH_H};
use mini_ui::{Rect, Surface};

use super::theme;
use super::window::App;

/// Надпись на кнопке меню.
const BRAND: &str = "FreeOS";

/// Что панель показывает справа.
pub struct Status {
    /// Местное время `ЧЧ:ММ`, если система его знает.
    ///
    /// Готовой строкой, а не числом: панель рисует то, что ей дали, и знать о
    /// часовых поясах ей незачем. Время работы при этом остаётся на месте — оно
    /// отвечает на другой вопрос («давно ли эта машина включена»), и заменить
    /// им часы было нельзя, как нельзя и наоборот.
    pub clock: Option<String>,
    pub uptime_ms: u64,
    pub free_mib: u64,
    pub total_mib: u64,
}

/// Панель задач вдоль нижнего края экрана.
pub struct Panel {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    damage: Rect,
    /// Кнопка меню в координатах поверхности панели.
    brand: Rect,
    /// Кнопки окон там же.
    ///
    /// Запоминаются при рисовании, а не считаются заново при попадании мышью.
    /// Две независимые раскладки — рисующая и проверяющая — расходятся молча, и
    /// расхождение выглядит как «кнопка не нажимается», хотя нажимается
    /// соседняя.
    buttons: Buttons,
}

impl Panel {
    /// Высота панели при заданном масштабе шрифта.
    #[must_use]
    pub const fn height(scale: u32) -> u32 {
        GLYPH_H * scale + theme::PADDING * 4
    }

    #[must_use]
    pub fn new(screen_w: u32, screen_h: u32, scale: u32) -> Option<Self> {
        let height = Self::height(scale);
        let surface = Surface::new(screen_w, height, theme::PANEL_BG)?;
        let rect = Rect::new(0, (screen_h - height) as i32, screen_w, height);
        Some(Self {
            surface,
            rect,
            scale,
            damage: Rect::EMPTY,
            brand: Rect::EMPTY,
            buttons: Buttons::new(),
        })
    }

    /// Во что попадает точка панели (координаты экрана).
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<PanelHit> {
        let local = (x - self.rect.x, y - self.rect.y);
        if self.brand.contains(local.0, local.1) {
            return Some(PanelHit::Menu);
        }
        for (app, rect) in &self.buttons {
            if rect.contains(local.0, local.1) {
                return Some(PanelHit::Window(*app));
            }
        }
        Some(PanelHit::Empty)
    }

    /// Перерисовать панель целиком.
    ///
    /// Целиком, а не по частям: панель — это одна строка высотой в двадцать
    /// точек, и вычисление изменившегося куска стоило бы дороже перерисовки.
    pub fn redraw(&mut self, windows: &[(App, bool)], menu_open: bool, status: &Status) {
        let scale = self.scale;
        self.buttons.clear();
        let bounds = self.surface.bounds();
        self.surface.fill(bounds, theme::PANEL_BG);
        // Светлая линия по верхнему краю: панель и фон стола оба тёмные, и без
        // неё граница между ними видна только там, где на панели есть кнопка.
        self.surface
            .fill(Rect::new(0, 0, bounds.w, 1), theme::PANEL_EDGE);

        let text_y = (bounds.h - GLYPH_H * scale) / 2;
        let pad = theme::PADDING * 2;

        // Кнопка меню.
        let brand_w = text::width_of(BRAND, scale) + pad * 2;
        let brand_rect = Rect::new(0, 1, brand_w, bounds.h - 1);
        let (brand_bg, brand_fg) = if menu_open {
            (theme::ACCENT, theme::ON_ACCENT)
        } else {
            (theme::PANEL_EDGE, theme::TEXT)
        };
        self.surface.fill(brand_rect, brand_bg);
        text::draw_text(&mut self.surface, pad, text_y, BRAND, scale, brand_fg, None);
        self.brand = brand_rect;

        // Кнопки окон.
        let mut x = brand_w + pad;
        for (app, focused) in windows {
            let label = app.title();
            let width = text::width_of(label, scale) + pad * 2;
            if x + width > bounds.w {
                break;
            }
            let button = Rect::new(x as i32, 2, width, bounds.h - 4);
            let (bg, fg) = if *focused {
                (theme::ACCENT, theme::ON_ACCENT)
            } else {
                (theme::WINDOW_BG, theme::DIM)
            };
            self.surface.fill(button, bg);
            text::draw_text(&mut self.surface, x + pad, text_y, label, scale, fg, None);
            // Промах мимо кнопки на пару точек читается как «не нажалось»,
            // поэтому попадание проверяется по всей высоте панели, а не по
            // нарисованному прямоугольнику с отступами.
            self.buttons
                .push((*app, Rect::new(x as i32, 0, width, bounds.h)));
            x += width + theme::PADDING;
        }

        // Состояние справа.
        let right = match &status.clock {
            Some(clock) => alloc::format!(
                "{clock}   up {}   mem {} of {} MiB",
                uptime_text(status.uptime_ms),
                status.free_mib,
                status.total_mib
            ),
            None => alloc::format!(
                "up {}   mem {} of {} MiB",
                uptime_text(status.uptime_ms),
                status.free_mib,
                status.total_mib
            ),
        };
        let right_w = text::width_of(&right, scale);
        if right_w + pad < bounds.w {
            let right_x = bounds.w - right_w - pad;
            // Не затираем кнопки окон: если их так много, что они дошли до
            // правого края, состояние просто не рисуется.
            if right_x > x {
                text::draw_text(
                    &mut self.surface,
                    right_x,
                    text_y,
                    &right,
                    scale,
                    theme::DIM,
                    None,
                );
            }
        }

        self.damage = bounds;
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    /// Забрать накопленные изменения.
    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }
}

/// Меню запуска.
pub struct Menu {
    surface: Surface,
    pub rect: Rect,
    scale: u32,
    selected: usize,
    open: bool,
    damage: Rect,
}

impl Menu {
    #[must_use]
    pub fn new(panel_top: i32, scale: u32) -> Option<Self> {
        let row_h = GLYPH_H * scale + theme::PADDING * 3;
        // Ширина считается по самой длинной строке, а не задана числом: строки
        // меняются вместе со списком программ, и подрезанное описание выглядит
        // как испорченный вывод.
        let mut widest = 0;
        for app in App::LAUNCHABLE {
            widest = widest.max(text::width_of(app.title(), scale));
            widest = widest.max(text::width_of(app.about(), scale.saturating_sub(1).max(1)));
        }
        let width = widest + theme::PADDING * 6;
        let height = row_h * (App::LAUNCHABLE.len() as u32 * 2 + 1) + theme::PADDING * 2;
        let surface = Surface::new(width, height, theme::WINDOW_BG)?;
        let rect = Rect::new(0, panel_top - height as i32, width, height);
        Some(Self { surface, rect, scale, selected: 0, open: false, damage: Rect::EMPTY })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Открыть или закрыть меню. Возвращает новое состояние.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        if self.open {
            self.selected = 0;
            self.redraw();
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Пункт меню под точкой (координаты экрана).
    #[must_use]
    pub fn index_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let row_h = GLYPH_H * self.scale + theme::PADDING * 3;
        let top = theme::PADDING + theme::BORDER + row_h;
        let local = (y - self.rect.y) as u32;
        if local < top {
            return None;
        }
        // Пункт занимает две строки — название и описание, — поэтому попадание
        // считается по паре: щелчок по описанию обязан открывать ту же
        // программу, что и щелчок по названию.
        let index = ((local - top) / (row_h * 2)) as usize;
        (index < App::LAUNCHABLE.len()).then_some(index)
    }

    /// Поставить выделение на пункт под указателем.
    pub fn select(&mut self, index: usize) {
        if index >= App::LAUNCHABLE.len() || index == self.selected {
            return;
        }
        self.selected = index;
        self.redraw();
    }

    pub fn move_selection(&mut self, forward: bool) {
        let count = App::LAUNCHABLE.len();
        self.selected = if forward {
            (self.selected + 1) % count
        } else {
            (self.selected + count - 1) % count
        };
        self.redraw();
    }

    /// Что выбрано сейчас.
    #[must_use]
    pub fn selection(&self) -> App {
        App::LAUNCHABLE[self.selected.min(App::LAUNCHABLE.len() - 1)]
    }

    fn redraw(&mut self) {
        let scale = self.scale;
        let small = scale.saturating_sub(1).max(1);
        let bounds = self.surface.bounds();
        self.surface.fill(bounds, theme::WINDOW_BG);
        self.surface.frame(bounds, theme::BORDER, theme::ACCENT);

        let row_h = GLYPH_H * scale + theme::PADDING * 3;
        let left = theme::PADDING * 3;
        let mut y = theme::PADDING + theme::BORDER;

        text::draw_text(&mut self.surface, left, y, "Start", scale, theme::DIM, None);
        y += row_h;

        for (index, app) in App::LAUNCHABLE.iter().enumerate() {
            let selected = index == self.selected;
            if selected {
                self.surface.fill(
                    Rect::new(
                        theme::BORDER as i32,
                        y as i32 - theme::PADDING as i32,
                        bounds.w - theme::BORDER * 2,
                        row_h * 2 - theme::PADDING,
                    ),
                    theme::SELECT_BG,
                );
            }
            let title_color = if selected { theme::ACCENT } else { theme::TEXT };
            text::draw_text(&mut self.surface, left, y, app.title(), scale, title_color, None);
            y += row_h;
            text::draw_text(&mut self.surface, left, y, app.about(), small, theme::DIM, None);
            y += row_h;
        }

        self.damage = bounds;
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }
}

/// Время работы в виде `Ч:ММ:СС`.
fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    alloc::format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// Список окон для панели: программа и признак «активно».
pub type Windows = Vec<(App, bool)>;

/// Кнопки панели вместе с их местом.
type Buttons = Vec<(App, Rect)>;

/// Во что попал указатель на панели.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelHit {
    /// Кнопка меню.
    Menu,
    /// Кнопка окна.
    Window(App),
    /// Пустое место панели: щелчок туда не должен доставаться окну под ней.
    Empty,
}
