//! Окно: поверхность вместе с рамкой, заголовком и содержимым.
//!
//! Украшения нарисованы в той же поверхности, что и содержимое, поэтому вывод
//! окна на экран — одна операция, а не три. Плата — перерисовка заголовка при
//! смене активного окна, то есть несколько тысяч пикселей в обычной памяти,
//! которую, в отличие от экрана, можно и читать, и переписывать сколько угодно.

use mini_ui::text::{self, TextGrid};
use mini_ui::{Rect, Surface};

use super::files::FilesView;
use super::theme;
use crate::input::KeyCode;

/// Какая программа живёт в окне.
///
/// Перечисление, а не строка-заголовок: по нему ищут окно, по нему же меню
/// решает, что запускать, и опечатка в имени становится ошибкой компиляции.
/// Пользовательского пространства ещё нет, поэтому «программа» — это модуль
/// ядра; граница проведена там, где она пройдёт и потом.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
    /// Оболочка.
    Terminal,
    /// Счётчики системы.
    System,
    /// Файловый менеджер.
    Files,
    /// Что это за система.
    About,
    /// Подтверждение выключения.
    ///
    /// Окно, а не отдельная сущность: подтверждение ведёт себя как всё
    /// остальное на столе — его видно в панели задач, его можно отодвинуть и
    /// закрыть привычным Ctrl+W, и «закрыть» здесь означает «передумал». Это
    /// дешевле собственного модального слоя и понятнее человеку, который уже
    /// знает, как закрываются окна.
    Shutdown,
    /// Подтверждение перезагрузки.
    Restart,
}

impl App {
    /// Порядок в меню запуска.
    pub const LAUNCHABLE: [App; 6] = [
        App::Terminal,
        App::Files,
        App::System,
        App::About,
        App::Shutdown,
        App::Restart,
    ];

    /// Спрашивает ли это окно «точно?» — и о чём именно.
    ///
    /// `Some(true)` — перезагрузка, `Some(false)` — выключение, `None` — обычное
    /// окно. Один ответ на оба вопроса, а не два предиката: «спрашивает ли» и
    /// «о чём именно» нельзя разнести так, чтобы вызывающий проверил первое и
    /// забыл второе, — а перепутать выключение с перезагрузкой дороже, чем
    /// набрать лишний `match`.
    #[must_use]
    pub const fn confirms_power(self) -> Option<bool> {
        match self {
            App::Shutdown => Some(false),
            App::Restart => Some(true),
            _ => None,
        }
    }

    /// Заголовок окна и подпись в панели задач.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            App::Terminal => "Terminal",
            App::System => "System",
            App::Files => "Files",
            App::About => "About",
            App::Shutdown => "Shut down",
            App::Restart => "Restart",
        }
    }

    /// Строка меню: что эта программа делает.
    #[must_use]
    pub const fn about(self) -> &'static str {
        match self {
            App::Terminal => "shell and kernel commands",
            App::Files => "browse the mounted root",
            App::System => "memory, tasks, input counters",
            App::About => "what this system is",
            App::Shutdown => "close the volume and switch off",
            App::Restart => "close the volume and start again",
        }
    }
}

/// Во что попадает указатель внутри окна.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// Кнопка закрытия.
    Close,
    /// Полоса заголовка: за неё окно таскают.
    Title,
    /// Всё остальное.
    Body,
}

/// Содержимое окна.
pub enum Content {
    /// Сетка символов: терминал и всё, что печатает строки.
    Text(TextGrid),
    /// Список файлов, который рисует себя сам.
    Files(FilesView),
}

pub struct Window {
    pub app: App,
    pub rect: Rect,
    surface: Surface,
    content: Content,
    /// Масштаб глифа содержимого.
    scale: u32,
    /// Масштаб глифа заголовка. Ограничен двойкой: на экране 1920 заголовок
    /// втрое крупнее обычного съел бы половину окна.
    title_scale: u32,
    /// Что изменилось в поверхности с прошлой сборки, в координатах поверхности.
    pub damage: Rect,
}

impl Window {
    /// Создать текстовое окно.
    ///
    /// `None`, если не хватило памяти под поверхность или окно слишком мало,
    /// чтобы вместить хоть одну ячейку текста. Паниковать здесь нельзя: окно —
    /// это несколько мегабайт, отказ выделения совершенно реален, и система
    /// обязана продолжить работу без этого окна.
    #[must_use]
    pub fn text(app: App, rect: Rect, scale: u32) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::WINDOW_BG)?;
        let title_scale = scale.min(2);
        let grid = TextGrid::new(
            content_area(&surface, title_scale),
            scale,
            theme::TEXT,
            theme::WINDOW_BG,
        )?;
        let mut window = Self::wrap(app, rect, surface, Content::Text(grid), scale, title_scale);
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно файлового менеджера.
    #[must_use]
    pub fn files(rect: Rect, scale: u32) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::WINDOW_BG)?;
        let title_scale = scale.min(2);
        let content = Content::Files(FilesView::new());
        let mut window = Self::wrap(App::Files, rect, surface, content, scale, title_scale);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    fn wrap(
        app: App,
        rect: Rect,
        surface: Surface,
        content: Content,
        scale: u32,
        title_scale: u32,
    ) -> Self {
        Self {
            app,
            rect,
            surface,
            content,
            scale,
            title_scale,
            damage: Rect::EMPTY,
        }
    }

    /// Высота полосы заголовка при заданном масштабе.
    #[must_use]
    pub const fn title_height(title_scale: u32) -> u32 {
        text::GLYPH_H * title_scale + theme::PADDING * 2
    }

    /// Кнопка закрытия в координатах поверхности.
    fn close_button(&self) -> Rect {
        let size = Self::title_height(self.title_scale);
        Rect::new(
            self.surface.width() as i32 - (theme::BORDER + size) as i32,
            theme::BORDER as i32,
            size,
            size,
        )
    }

    /// Во что попадает точка экрана. `None` — мимо окна.
    ///
    /// Кнопка закрытия проверяется первой: она лежит внутри полосы заголовка, и
    /// обратный порядок означал бы, что окно за неё таскают, а не закрывается.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let local = (x - self.rect.x, y - self.rect.y);
        if self.close_button().contains(local.0, local.1) {
            return Some(Hit::Close);
        }
        let title_bottom = (theme::BORDER + Self::title_height(self.title_scale)) as i32;
        if local.1 < title_bottom {
            return Some(Hit::Title);
        }
        Some(Hit::Body)
    }

    /// Нарисовать рамку, заголовок и кнопку закрытия.
    pub fn draw_decorations(&mut self, focused: bool) {
        let title_h = Self::title_height(self.title_scale);
        let bounds = self.surface.bounds();

        // Неактивное окно получает тот же цвет, приглушённый к фону: заголовок
        // остаётся видимым, но перестаёт спорить за внимание с активным.
        let accent = if focused {
            theme::ACCENT
        } else {
            theme::inactive(theme::ACCENT)
        };

        self.surface.frame(bounds, theme::BORDER, theme::FRAME);
        let title_bar = Rect::new(
            theme::BORDER as i32,
            theme::BORDER as i32,
            bounds.w.saturating_sub(theme::BORDER * 2),
            title_h,
        );
        self.surface.fill(title_bar, accent);

        let title_color = if focused { theme::ON_ACCENT } else { theme::TEXT };
        text::draw_text(
            &mut self.surface,
            theme::BORDER + theme::PADDING,
            theme::BORDER + theme::PADDING,
            self.app.title(),
            self.title_scale,
            title_color,
            None,
        );

        // Кнопка закрытия нарисована всегда, а не только у активного окна:
        // кнопка, появляющаяся при наведении, потребовала бы мыши, а её на этой
        // фазе нет.
        let button = self.close_button();
        self.surface.fill(button, theme::CLOSE);
        let glyph_w = text::GLYPH_W * self.title_scale;
        let glyph_h = text::GLYPH_H * self.title_scale;
        text::draw_text(
            &mut self.surface,
            button.x as u32 + button.w.saturating_sub(glyph_w) / 2,
            button.y as u32 + button.h.saturating_sub(glyph_h) / 2,
            "x",
            self.title_scale,
            theme::ON_ACCENT,
            None,
        );

        // Изменилась только полоса сверху — её и помечаем. Пометить всё окно
        // было бы проще на одну строку и дороже на площадь окна при каждом
        // переключении фокуса.
        self.damage = self
            .damage
            .union(&Rect::new(0, 0, bounds.w, theme::BORDER + title_h));
    }

    /// Напечатать в окно. Действует только на текстовые окна.
    pub fn write_str(&mut self, text: &str) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.write_str(&mut self.surface, text);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Очистить содержимое.
    pub fn clear(&mut self) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.clear(&mut self.surface);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Показывать ли курсор.
    pub fn set_cursor(&mut self, visible: bool) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.set_cursor(&mut self.surface, visible);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Размер сетки в символах. Для нетекстовых окон — нули.
    #[must_use]
    pub fn size_in_cells(&self) -> (u32, u32) {
        match &self.content {
            Content::Text(grid) => (grid.cols(), grid.rows()),
            Content::Files(_) => (0, 0),
        }
    }

    /// Передать клавишу содержимому. `true` — окно её обработало.
    pub fn handle_key(&mut self, code: KeyCode) -> bool {
        match &mut self.content {
            Content::Files(view) => {
                if !view.handle(code) {
                    return false;
                }
                self.redraw_content();
                true
            }
            // Терминал получает события не здесь: их разбирает редактор строки
            // в задаче оболочки, потому что набираемая строка — состояние
            // оболочки, а не окна.
            Content::Text(_) => false,
        }
    }

    /// Перерисовать содержимое, которое рисует себя само.
    pub fn redraw_content(&mut self) {
        let Content::Files(view) = &self.content else {
            return;
        };
        let area = content_area(&self.surface, self.title_scale);
        view.draw(&mut self.surface, area, self.scale);
        self.damage = self.damage.union(&area);
    }

    /// Сдвинуть окно, оставив заголовок на экране.
    ///
    /// Полностью уехавшее окно нельзя ни вернуть, ни закрыть мышью, которой на
    /// этой фазе нет, — поэтому часть заголовка обязана остаться видимой.
    pub fn move_within(&mut self, dx: i32, dy: i32, screen_w: u32, bottom_limit: i32) {
        let keep = Self::title_height(self.title_scale) as i32 * 3;
        let min_x = keep - self.rect.w as i32;
        let max_x = screen_w as i32 - keep;
        let max_y = (bottom_limit - keep).max(0);
        self.rect.x = self.rect.x.saturating_add(dx).clamp(min_x, max_x.max(min_x));
        self.rect.y = self.rect.y.saturating_add(dy).clamp(0, max_y);
    }

    /// Поверхность окна — для сборки кадра.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

}

/// Область поверхности под содержимое: всё, кроме рамки и заголовка.
fn content_area(surface: &Surface, title_scale: u32) -> Rect {
    let top = theme::BORDER + Window::title_height(title_scale) + theme::PADDING;
    Rect::new(
        (theme::BORDER + theme::PADDING) as i32,
        top as i32,
        surface
            .width()
            .saturating_sub((theme::BORDER + theme::PADDING) * 2),
        surface
            .height()
            .saturating_sub(top + theme::BORDER + theme::PADDING),
    )
}
