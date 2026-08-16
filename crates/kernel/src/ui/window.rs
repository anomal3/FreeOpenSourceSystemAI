//! Окно: поверхность вместе с рамкой, заголовком и содержимым.
//!
//! Украшения нарисованы в той же поверхности, что и содержимое, поэтому вывод
//! окна на экран — одна операция, а не три. Плата — перерисовка заголовка при
//! смене активного окна, то есть несколько тысяч пикселей в обычной памяти,
//! которую, в отличие от экрана, можно и читать, и переписывать сколько угодно.

use mini_ui::text::{self, TextGrid};
use mini_ui::{Rect, Surface};

use super::files::FilesView;
use super::settings::SettingsView;
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
    /// Параметры: сведения о системе, экран, программы, обновление.
    Settings,
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
    pub const LAUNCHABLE: [App; 7] = [
        App::Terminal,
        App::Files,
        App::Settings,
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
            App::Settings => "Settings",
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
            App::Settings => "screen, programs, updates",
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
    /// Кнопка «свернуть».
    Minimize,
    /// Кнопка «развернуть» — она же «вернуть прежний размер».
    Maximize,
    /// Уголок в правом нижнем углу: за него окно тянут за размер.
    Resize,
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
    /// Окно параметров: разделы слева, содержимое справа.
    Settings(SettingsView),
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
    /// Окно свёрнуто: его нет на экране, но оно есть в панели задач.
    ///
    /// Свёрнутое окно сохраняет и поверхность, и содержимое: свернуть — это
    /// «убрать с глаз», а не «закрыть», и вернувшееся окно обязано показать то
    /// же, что показывало до этого.
    pub minimized: bool,
    /// Куда вернуть окно, если оно развёрнуто. `None` — окно обычного размера.
    restore: Option<Rect>,
    /// Что изменилось в поверхности с прошлой сборки, в координатах поверхности.
    pub damage: Rect,
}

/// Наименьший размер окна в пикселях.
///
/// Меньше — это окно, в котором не помещается ни строки текста, ни трёх кнопок
/// заголовка; вернуть его к рабочему размеру мышью человек уже не сможет.
const MIN_W: u32 = 220;
const MIN_H: u32 = 120;

/// Сторона уголка, за который тянут размер.
const GRIP: u32 = 16;

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

    /// Создать окно параметров.
    #[must_use]
    pub fn settings(rect: Rect, scale: u32, screen: (u32, u32)) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::WINDOW_BG)?;
        let title_scale = scale.min(2);
        let content = Content::Settings(SettingsView::new(screen));
        let mut window = Self::wrap(App::Settings, rect, surface, content, scale, title_scale);
        window.redraw_content();
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
            minimized: false,
            restore: None,
            damage: Rect::EMPTY,
        }
    }

    /// Высота полосы заголовка при заданном масштабе.
    #[must_use]
    pub const fn title_height(title_scale: u32) -> u32 {
        text::GLYPH_H * title_scale + theme::PADDING * 2
    }

    /// Кнопки заголовка справа налево: закрыть, развернуть, свернуть.
    ///
    /// Порядок как у окон, к которым человек привык: крестик крайний справа,
    /// потому что промахнуться мимо него — это закрыть окно, а не свернуть.
    fn title_button(&self, from_right: u32) -> Rect {
        let size = Self::title_height(self.title_scale);
        Rect::new(
            self.surface.width() as i32 - (theme::BORDER + size * (from_right + 1)) as i32,
            theme::BORDER as i32,
            size,
            size,
        )
    }

    /// Кнопка закрытия в координатах поверхности.
    fn close_button(&self) -> Rect {
        self.title_button(0)
    }

    /// Кнопка «развернуть» в координатах поверхности.
    fn maximize_button(&self) -> Rect {
        self.title_button(1)
    }

    /// Кнопка «свернуть» в координатах поверхности.
    fn minimize_button(&self) -> Rect {
        self.title_button(2)
    }

    /// Уголок изменения размера в координатах поверхности.
    fn resize_grip(&self) -> Rect {
        Rect::new(
            self.surface.width().saturating_sub(GRIP) as i32,
            self.surface.height().saturating_sub(GRIP) as i32,
            GRIP,
            GRIP,
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
        if self.maximize_button().contains(local.0, local.1) {
            return Some(Hit::Maximize);
        }
        if self.minimize_button().contains(local.0, local.1) {
            return Some(Hit::Minimize);
        }
        let title_bottom = (theme::BORDER + Self::title_height(self.title_scale)) as i32;
        if local.1 < title_bottom {
            return Some(Hit::Title);
        }
        // Уголок проверяется после заголовка: у окна ростом с полосу заголовка
        // они пересекаются, и таскать такое окно важнее, чем тянуть его за
        // размер.
        if self.resize_grip().contains(local.0, local.1) {
            return Some(Hit::Resize);
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

        // Кнопки нарисованы всегда, а не только у активного окна: кнопка,
        // появляющаяся при наведении, потребовала бы следить за указателем и
        // перерисовывать заголовок на каждое его движение.
        let glyph_w = text::GLYPH_W * self.title_scale;
        let glyph_h = text::GLYPH_H * self.title_scale;
        // Знак «развернуть» меняется вместе с состоянием: развёрнутое окно
        // предлагает вернуть прежний размер, и одинаковый значок в обоих
        // случаях означал бы, что человек нажимает наугад.
        let restore_glyph = if self.restore.is_some() { "-" } else { "[" };
        for (button, glyph, fill) in [
            (self.minimize_button(), "_", accent),
            (self.maximize_button(), restore_glyph, accent),
            (self.close_button(), "x", theme::CLOSE),
        ] {
            self.surface.fill(button, fill);
            text::draw_text(
                &mut self.surface,
                button.x as u32 + button.w.saturating_sub(glyph_w) / 2,
                button.y as u32 + button.h.saturating_sub(glyph_h) / 2,
                glyph,
                self.title_scale,
                if fill == theme::CLOSE || focused {
                    theme::ON_ACCENT
                } else {
                    theme::TEXT
                },
                None,
            );
        }

        // Уголок размера: три косые чёрточки в правом нижнем углу. Без
        // нарисованного признака за него никто не потянет — угадывать, что окно
        // где-то тянется, человек не обязан.
        // Уголок рисуется, но **не** помечается изменившимся: он не зависит ни
        // от фокуса, ни от содержимого. Пометить его вместе с заголовком стоило
        // бы всей площади окна — прямоугольник изменений один, и объединение
        // верхней полосы с нижним углом накрывает окно целиком. На 1920×1080
        // это превращало переключение окон в перерисовку всего экрана: клавиша
        // Tab обрабатывалась дольше пяти секунд.
        let grip = self.resize_grip();
        for step in 0..3u32 {
            let offset = (step * 5 + 3) as i32;
            let from_x = grip.x + grip.w as i32 - 2;
            let from_y = grip.y + grip.h as i32 - offset;
            for along in 0..offset.min(grip.w as i32 - 2) {
                self.surface.fill(
                    Rect::new(from_x - along, from_y + along, 2, 2),
                    theme::FRAME,
                );
            }
        }

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

    /// Поставить курсор в заданную ячейку. Нумерация с нуля.
    ///
    /// Дальше идут пять методов, которыми пользуется разбор управляющих
    /// последовательностей ([`super::term`]). Каждый — одна строка делегирования,
    /// и это не бесполезная прослойка: содержимое окна бывает не только текстом,
    /// а разбор ANSI обязан молча ничего не делать над списком файлов, а не
    /// разбираться, что там внутри.
    pub fn term_move_to(&mut self, row: u32, col: u32) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.move_to(&mut self.surface, row, col);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Сдвинуть курсор на заданное число строк и столбцов.
    pub fn term_move_by(&mut self, rows: i32, cols: i32) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.move_by(&mut self.surface, rows, cols);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Стереть часть экрана: `0` — вниз от курсора, `1` — вверх, `2` — весь.
    pub fn term_erase_display(&mut self, mode: u8) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.erase_display(&mut self.surface, mode);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Стереть часть строки.
    pub fn term_erase_line(&mut self, mode: u8) {
        let Content::Text(grid) = &mut self.content else {
            return;
        };
        grid.erase_line(&mut self.surface, mode);
        self.damage = self.damage.union(&grid.take_damage());
    }

    /// Цвет текста для последующего вывода.
    pub fn term_set_fg(&mut self, index: u8) {
        if let Content::Text(grid) = &mut self.content {
            grid.set_fg(index);
        }
    }

    /// Цвет фона для последующего вывода.
    pub fn term_set_bg(&mut self, index: u8) {
        if let Content::Text(grid) = &mut self.content {
            grid.set_bg(index);
        }
    }

    /// Вернуть цвета окна.
    pub fn term_reset_attr(&mut self) {
        if let Content::Text(grid) = &mut self.content {
            grid.reset_attr();
        }
    }

    /// Размер сетки в символах. Для нетекстовых окон — нули.
    #[must_use]
    pub fn size_in_cells(&self) -> (u32, u32) {
        match &self.content {
            Content::Text(grid) => (grid.cols(), grid.rows()),
            Content::Files(_) | Content::Settings(_) => (0, 0),
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
            Content::Settings(view) => {
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
        let area = content_area(&self.surface, self.title_scale);
        match &self.content {
            Content::Files(view) => view.draw(&mut self.surface, area, self.scale),
            Content::Settings(view) => view.draw(&mut self.surface, area, self.scale),
            Content::Text(_) => return,
        }
        self.damage = self.damage.union(&area);
    }

    /// Показать в файловом менеджере то, что открыли значком со стола.
    pub fn reveal(&mut self, path: &str, directory: bool) {
        if let Content::Files(view) = &mut self.content {
            view.reveal(path, directory);
            self.redraw_content();
        }
    }

    /// Перечитать открытый каталог.
    pub fn refresh_files(&mut self) {
        if let Content::Files(view) = &mut self.content {
            view.refresh();
            self.redraw_content();
        }
    }

    /// Показать в «Параметрах» раздел экрана.
    ///
    /// Нужно меню стола: пункт «Display settings» обязан открывать окно уже на
    /// нужном разделе, иначе он всего лишь синоним значка «Settings».
    pub fn show_display_settings(&mut self) {
        if let Content::Settings(view) = &mut self.content {
            view.show_display();
            self.redraw_content();
        }
    }

    /// Отдать щелчок содержимому окна. Координаты — в точках экрана.
    ///
    /// `true` — содержимое им воспользовалось и окно перерисовано. Текстовые
    /// окна щелчков не разбирают: в них нечего выбирать мышью.
    pub fn handle_click(&mut self, x: i32, y: i32) -> bool {
        let area = content_area(&self.surface, self.title_scale);
        let local = (x - self.rect.x, y - self.rect.y);
        let scale = self.scale;
        let used = match &mut self.content {
            Content::Settings(view) => view.click(area, scale, local.0, local.1),
            Content::Files(view) => view.click(area, scale, local.0, local.1),
            Content::Text(_) => false,
        };
        if used {
            self.redraw_content();
        }
        used
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

    /// Задать окну новый размер, сохранив содержимое.
    ///
    /// Поверхность создаётся заново: она хранит пиксели, и растянуть её нечем.
    /// Отказ выделения — это `false` и **прежнее** окно, а не половина нового:
    /// окно без поверхности нечем рисовать, и потерять на изменении размера
    /// работающий терминал было бы хуже, чем не изменить размер.
    pub fn resize(&mut self, w: u32, h: u32) -> bool {
        let w = w.max(MIN_W);
        let h = h.max(MIN_H);
        if w == self.rect.w && h == self.rect.h {
            return true;
        }
        let Some(mut surface) = Surface::new(w, h, theme::WINDOW_BG) else {
            return false;
        };
        let area = content_area(&surface, self.title_scale);
        match &mut self.content {
            Content::Text(grid) => {
                if !grid.rebind(&mut surface, area) {
                    return false;
                }
            }
            // Список файлов и «Параметры» рисуют себя от размера области, и
            // переносить в них нечего: содержимое соберётся заново.
            Content::Files(_) | Content::Settings(_) => {}
        }

        self.surface = surface;
        self.rect.w = w;
        self.rect.h = h;
        self.redraw_content();
        self.draw_decorations(false);
        self.damage = Rect::new(0, 0, w, h);
        true
    }

    /// Развернуть окно на всю рабочую область — или вернуть прежний размер.
    ///
    /// Возвращает `true`, если состояние изменилось. Прежний прямоугольник
    /// запоминается целиком, вместе с местом: развёрнутое окно, вернувшееся не
    /// туда, откуда его развернули, выглядит как потерянное.
    pub fn toggle_maximize(&mut self, screen_w: u32, work_bottom: i32) -> bool {
        match self.restore.take() {
            Some(rect) => {
                let (x, y) = (rect.x, rect.y);
                if !self.resize(rect.w, rect.h) {
                    self.restore = Some(rect);
                    return false;
                }
                self.rect.x = x;
                self.rect.y = y;
                true
            }
            None => {
                let previous = self.rect;
                let height = work_bottom.max(0) as u32;
                if !self.resize(screen_w, height) {
                    return false;
                }
                self.rect.x = 0;
                self.rect.y = 0;
                self.restore = Some(previous);
                true
            }
        }
    }

    /// Развёрнуто ли окно во весь экран.
    #[must_use]
    pub const fn maximized(&self) -> bool {
        self.restore.is_some()
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
