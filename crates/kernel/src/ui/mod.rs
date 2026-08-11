//! Композитор: окна как прямоугольные поверхности поверх фреймбуфера.
//!
//! # Чем это не является
//!
//! Ни X11, ни Wayland. Здесь нет ни протокола, ни клиентов, ни серверa: окно —
//! это структура в том же ядре, у неё есть поверхность в памяти и место на
//! экране. Смысл всей затеи ровно в этом: оконная система, которая помещается в
//! голову целиком, а не в несколько сотен тысяч строк.
//!
//! # Что делает композитор
//!
//! Три вещи, и все три — следствие того, что экран дорог на запись и невозможен
//! на чтение (см. [`crate::gfx`]):
//!
//! * **порядок**: окна лежат списком снизу вверх, и то, что выше, закрывает то,
//!   что ниже;
//! * **учёт изменённого**: на экран выводится не всё, а прямоугольники, которые
//!   действительно изменились. Набор одной строки в терминале — это одна строка
//!   ячеек, а не полтора миллиона пикселей;
//! * **сборка**: для каждого изменённого прямоугольника рисуется фон и то, что
//!   его перекрывает, снизу вверх.
//!
//! # Почему нет мыши
//!
//! Потому что нет драйвера мыши. Программный курсор — это ещё один слой поверх
//! окон, и написать его без источника событий значит написать код, который
//! невозможно ни проверить, ни отладить. USB HID отдаёт мышь тем же
//! boot-протоколом, что и клавиатуру ([`crate::usb::hid`]), так что появление
//! курсора — это ещё один разбор отчёта и один слой в [`Compositor::present`].
//!
//! Пока же порядок окон переключается с клавиатуры (Tab), и это не заглушка: до
//! появления мыши так работали все оконные системы, а «поднять окно наверх» —
//! ровно та операция, которая проверяет, что композитор действительно
//! композитор, а не один буфер на весь экран.

use alloc::vec::Vec;

use crate::gfx::text::{self, TextGrid};
use crate::gfx::{Color, Rect, Screen, Surface};
use crate::sync::SpinLock;

// ---------------------------------------------------------------------------
// Палитра
// ---------------------------------------------------------------------------

/// Фон рабочего стола.
const DESKTOP: Color = Color::rgb(0x0A, 0x14, 0x1E);
/// Фон окна.
const WINDOW_BG: Color = Color::rgb(0x0A, 0x1C, 0x2E);
/// Текст.
const TEXT: Color = Color::rgb(0xD8, 0xE2, 0xEC);
/// Рамка и заголовок активного окна.
const ACCENT: Color = Color::rgb(0x3C, 0x8C, 0xC8);
/// Текст в заголовке активного окна.
const TITLE_TEXT: Color = Color::rgb(0x06, 0x10, 0x18);

/// Толщина рамки окна.
const BORDER: u32 = 2;
/// Отступ содержимого от рамки.
const PADDING: u32 = 4;

/// Сколько прямоугольников изменений композитор согласен помнить.
///
/// Восемь — компромисс, а не число из воздуха: при обычной работе их один-два
/// (строка терминала и, изредка, окно статистики), а переполнение обходится
/// одной перерисовкой экрана. Список длиннее стоил бы больше, чем экономил: на
/// каждый прямоугольник приходится проход по всем окнам.
const MAX_DAMAGE: usize = 8;

// ---------------------------------------------------------------------------
// Окно
// ---------------------------------------------------------------------------

/// Окно: поверхность вместе с рамкой и заголовком.
///
/// Украшения нарисованы в той же поверхности, что и содержимое, поэтому вывод
/// окна на экран — одна операция, а не три. Плата — перерисовка заголовка при
/// смене активного окна, то есть несколько тысяч пикселей в обычной памяти.
pub struct Window {
    title: &'static str,
    /// Место на экране, включая украшения.
    rect: Rect,
    surface: Surface,
    /// Сетка символов, если окно текстовое.
    grid: Option<TextGrid>,
    /// Что изменилось в поверхности с прошлой сборки, в координатах поверхности.
    damage: Rect,
}

impl Window {
    /// Создать текстовое окно.
    ///
    /// Возвращает `None`, если не хватило памяти под поверхность или окно
    /// слишком мало, чтобы вместить хоть одну ячейку текста.
    fn new(title: &'static str, rect: Rect, scale: u32) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, WINDOW_BG)?;
        let content = Self::content_area(&surface, scale);
        let grid = TextGrid::new(content, scale, TEXT, WINDOW_BG)?;
        let mut window = Self { title, rect, surface, grid: Some(grid), damage: Rect::EMPTY };
        window.draw_decorations(false);
        Some(window)
    }

    /// Область поверхности под содержимое: всё, кроме рамки и заголовка.
    fn content_area(surface: &Surface, scale: u32) -> Rect {
        let title_h = Self::title_height(scale);
        let top = BORDER + title_h + PADDING;
        Rect::new(
            (BORDER + PADDING) as i32,
            top as i32,
            surface.width().saturating_sub((BORDER + PADDING) * 2),
            surface.height().saturating_sub(top + BORDER + PADDING),
        )
    }

    const fn title_height(scale: u32) -> u32 {
        text::GLYPH_H * scale + PADDING * 2
    }

    /// Нарисовать рамку и заголовок.
    fn draw_decorations(&mut self, focused: bool) {
        let scale = 1;
        let title_h = Self::title_height(scale);
        let bounds = self.surface.bounds();

        // Неактивное окно получает тот же цвет, приглушённый к фону: так рамка
        // остаётся видимой, но перестаёт спорить за внимание с активной.
        let accent = if focused { ACCENT } else { ACCENT.mix(DESKTOP, 160) };

        self.surface.frame(bounds, BORDER, accent);
        let title_bar = Rect::new(
            BORDER as i32,
            BORDER as i32,
            bounds.w.saturating_sub(BORDER * 2),
            title_h,
        );
        self.surface.fill(title_bar, accent);
        let title_color = if focused { TITLE_TEXT } else { TEXT };
        text::draw_text(
            &mut self.surface,
            BORDER + PADDING,
            BORDER + PADDING,
            self.title,
            scale,
            title_color,
            None,
        );
        self.damage = self.damage.union(&bounds);
    }

    /// Напечатать в окно.
    pub fn write_str(&mut self, text: &str) {
        let Some(mut grid) = self.grid.take() else {
            return;
        };
        grid.write_str(&mut self.surface, text);
        self.damage = self.damage.union(&grid.take_damage());
        self.grid = Some(grid);
    }

    /// Очистить содержимое окна.
    pub fn clear(&mut self) {
        let Some(mut grid) = self.grid.take() else {
            return;
        };
        grid.clear(&mut self.surface);
        self.damage = self.damage.union(&grid.take_damage());
        self.grid = Some(grid);
    }

    /// Показывать ли курсор.
    pub fn set_cursor(&mut self, visible: bool) {
        let Some(mut grid) = self.grid.take() else {
            return;
        };
        grid.set_cursor(&mut self.surface, visible);
        self.damage = self.damage.union(&grid.take_damage());
        self.grid = Some(grid);
    }

    /// Размер сетки в символах.
    #[must_use]
    pub fn size_in_cells(&self) -> (u32, u32) {
        match self.grid.as_ref() {
            Some(grid) => (grid.cols(), grid.rows()),
            None => (0, 0),
        }
    }
}

// ---------------------------------------------------------------------------
// Композитор
// ---------------------------------------------------------------------------

/// Окна и экран.
pub struct Compositor {
    screen: Screen,
    /// Снизу вверх: последнее окно поверх остальных.
    windows: Vec<Window>,
    /// Индекс активного окна в [`Compositor::windows`].
    focus: usize,
    damage: [Rect; MAX_DAMAGE],
    damage_count: usize,
    /// Изменений накопилось больше, чем помещается: проще перерисовать всё.
    damage_overflow: bool,
    /// Сколько раз собирался кадр — диагностика.
    frames: u64,
    /// Сколько прямоугольников выведено на экран.
    rects: u64,
}

impl Compositor {
    /// Создать композитор и залить экран фоном.
    fn new(screen: Screen) -> Self {
        screen.fill(screen.bounds(), DESKTOP);
        Self {
            screen,
            windows: Vec::new(),
            focus: 0,
            damage: [Rect::EMPTY; MAX_DAMAGE],
            damage_count: 0,
            damage_overflow: false,
            frames: 0,
            rects: 0,
        }
    }

    /// Добавить окно наверх. Возвращает его индекс в порядке создания.
    fn push(&mut self, window: Window) {
        self.windows.push(window);
        self.damage_overflow = true;
    }

    fn mark(&mut self, rect: Rect) {
        if rect.is_empty() || self.damage_overflow {
            return;
        }
        if self.damage_count == MAX_DAMAGE {
            self.damage_overflow = true;
            return;
        }
        self.damage[self.damage_count] = rect;
        self.damage_count += 1;
    }

    /// Перенести накопленные окном изменения в общий список.
    fn collect(&mut self, index: usize) {
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let damage = core::mem::replace(&mut window.damage, Rect::EMPTY);
        if damage.is_empty() {
            return;
        }
        let origin = (window.rect.x, window.rect.y);
        self.mark(damage.translate(origin.0, origin.1));
    }

    /// Собрать кадр: вывести на экран всё, что изменилось.
    pub fn present(&mut self) {
        for index in 0..self.windows.len() {
            self.collect(index);
        }
        if !self.damage_overflow && self.damage_count == 0 {
            return;
        }
        self.frames += 1;

        if self.damage_overflow {
            let all = self.screen.bounds();
            self.compose(all);
            self.rects += 1;
            self.damage_overflow = false;
            self.damage_count = 0;
            return;
        }

        for index in 0..self.damage_count {
            let rect = self.damage[index].intersect(&self.screen.bounds());
            if !rect.is_empty() {
                self.compose(rect);
                self.rects += 1;
            }
        }
        self.damage_count = 0;
    }

    /// Нарисовать один прямоугольник экрана.
    /// Нарисовать один прямоугольник экрана: фон, затем все окна снизу вверх.
    ///
    /// # Почему здесь нет короткого пути
    ///
    /// Он был: если верхнее окно закрывает прямоугольник целиком, можно вывести
    /// только его и не трогать ни фон, ни окна под ним. Выглядит бесспорно —
    /// набор текста меняет ячейку внутри окна, и это самый частый случай.
    ///
    /// На практике он давал неверное перекрытие: содержимое нижнего окна
    /// оказывалось поверх верхнего. Причину найти не удалось — трассировка
    /// показывала правильный порядок вывода, а на экране был обратный, — и
    /// оптимизация убрана целиком. Оптимизация, работающая не всегда, хуже её
    /// отсутствия: она превращает картинку в лотерею, а сэкономленное здесь —
    /// это несколько тысяч записей в фреймбуфер на нажатие клавиши, то есть
    /// ничто по сравнению с ценой невоспроизводимого дефекта.
    ///
    /// Учёт изменённого при этом остался и остаётся главным: на экран выводятся
    /// прямоугольники, а не весь экран.
    fn compose(&self, rect: Rect) {
        self.screen.fill(rect, DESKTOP);
        for window in &self.windows {
            let overlap = window.rect.intersect(&rect);
            if !overlap.is_empty() {
                self.blit_window(window, overlap);
            }
        }
    }

    /// Вывести часть окна, попадающую в `rect` (координаты экрана).
    fn blit_window(&self, window: &Window, rect: Rect) {
        let src = rect.translate(-window.rect.x, -window.rect.y);
        self.screen.blit(&window.surface, (rect.x, rect.y), src);
    }

    /// Поднять нижнее окно наверх и подсветить его как активное.
    ///
    /// Циклический сдвиг порядка, а не переключение фокуса ввода, и разница
    /// существенная: ввод по-прежнему уходит в окно оболочки, потому что у окна
    /// состояния обработчика ввода нет вовсе. Делать вид, что фокус ввода
    /// переключается, значило бы обещать поведение, которого нет.
    ///
    /// Смысл операции в другом: поднятие — единственное действие, которое
    /// проверяет, что порядок окон и сборка по частям действительно работают.
    /// После него перекрытие меняется местами, и это видно на экране.
    pub fn focus_next(&mut self) {
        if self.windows.len() < 2 {
            return;
        }
        let raised = self.windows.remove(0);
        self.windows.push(raised);
        self.focus = self.windows.len() - 1;
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.draw_decorations(index == self.focus);
        }
        self.damage_overflow = true;
    }

    /// Окно с заданным заголовком.
    fn by_title(&mut self, title: &str) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.title == title)
    }

    /// Сколько кадров собрано и сколько прямоугольников выведено.
    #[must_use]
    pub const fn stats(&self) -> (u64, u64, usize) {
        (self.frames, self.rects, self.windows.len())
    }
}

// ---------------------------------------------------------------------------
// Глобальное состояние
// ---------------------------------------------------------------------------

/// Заголовок окна, в которое печатает оболочка.
pub const SHELL_WINDOW: &str = "shell";
/// Заголовок окна состояния системы.
pub const STATUS_WINDOW: &str = "system";

static COMPOSITOR: SpinLock<Option<Compositor>> = SpinLock::new(None);

/// Поднять композитор на этом фреймбуфере.
///
/// Возвращает `false`, если экрана нет или памяти под окна не хватило. Ядро в
/// этом случае продолжает работать с оболочкой в серийной консоли — графика не
/// является условием работы системы.
pub fn init(fb: &boot_info::Framebuffer) -> bool {
    let Some(screen) = Screen::new(fb) else {
        return false;
    };

    // Масштаб глифа: 8×8 на экране шириной 1600 читается плохо.
    let scale = if screen.width() >= 1600 {
        3
    } else if screen.width() >= 1024 {
        2
    } else {
        1
    };

    let (width, height) = (screen.width(), screen.height());
    let mut compositor = Compositor::new(screen);

    // Раскладка считается от размера экрана, а не задана числами: прошивка
    // вправе дать и 800×600, и 1920×1080, и окно, не помещающееся на экран,
    // выглядело бы как испорченная графика.
    let margin = width / 24;
    let shell_rect = Rect::new(margin as i32, (height / 16) as i32, width * 3 / 4, height * 3 / 4);

    // Окно состояния сознательно перекрывает окно оболочки — без перекрытия
    // композитор не отличить от двух независимых прямоугольников. Но перекрывает
    // так, чтобы правая часть его заголовка осталась видна из-под верхнего окна:
    // безымянный прямоугольник, торчащий из-за края, читается как испорченная
    // картинка, а не как второе окно.
    let status_x = shell_rect.x + (shell_rect.w * 3 / 4) as i32;
    let status_w = width.saturating_sub(status_x as u32 + margin);
    let status_rect =
        Rect::new(status_x, (height / 3) as i32, status_w, height / 2);

    // Окно оболочки обязательно, окно состояния — нет: без второго система
    // работает, поэтому отказ выделения памяти под него не повод отказываться от
    // графики целиком.
    let shell = Window::new(SHELL_WINDOW, shell_rect, scale);
    let status = Window::new(STATUS_WINDOW, status_rect, 1);

    let Some(shell) = shell else {
        return false;
    };
    // Порядок добавления и есть порядок по глубине: окно состояния уходит вниз,
    // оболочка кладётся поверх него. Так при запуске видно то, с чем работают, а
    // Tab поднимает состояние наверх.
    if let Some(status) = status {
        compositor.push(status);
    }
    compositor.push(shell);

    compositor.focus = compositor.windows.len() - 1;
    let focus = compositor.focus;
    for (index, window) in compositor.windows.iter_mut().enumerate() {
        window.draw_decorations(index == focus);
    }
    compositor.present();

    for (index, window) in compositor.windows.iter().enumerate() {
        crate::kprintln!(
            "  window {index}    : '{}' at {},{} {}x{} {}",
            window.title,
            window.rect.x,
            window.rect.y,
            window.rect.w,
            window.rect.h,
            if index == focus { "(focused)" } else { "" }
        );
    }

    *COMPOSITOR.lock() = Some(compositor);
    true
}

/// Работает ли графика.
#[must_use]
pub fn is_active() -> bool {
    COMPOSITOR.lock().is_some()
}

/// Напечатать в окно оболочки и собрать кадр.
pub fn write(text: &str) {
    let mut guard = COMPOSITOR.lock();
    let Some(compositor) = guard.as_mut() else {
        return;
    };
    if let Some(window) = compositor.by_title(SHELL_WINDOW) {
        window.write_str(text);
    }
    compositor.present();
}

/// Заменить содержимое окна состояния.
pub fn set_status(text: &str) {
    let mut guard = COMPOSITOR.lock();
    let Some(compositor) = guard.as_mut() else {
        return;
    };
    if let Some(window) = compositor.by_title(STATUS_WINDOW) {
        window.clear();
        window.write_str(text);
    }
    compositor.present();
}

/// Очистить окно оболочки.
pub fn clear_shell() {
    let mut guard = COMPOSITOR.lock();
    let Some(compositor) = guard.as_mut() else {
        return;
    };
    if let Some(window) = compositor.by_title(SHELL_WINDOW) {
        window.clear();
    }
    compositor.present();
}

/// Показывать ли курсор в активном окне.
pub fn set_cursor(visible: bool) {
    let mut guard = COMPOSITOR.lock();
    let Some(compositor) = guard.as_mut() else {
        return;
    };
    if let Some(window) = compositor.by_title(SHELL_WINDOW) {
        window.set_cursor(visible);
    }
    compositor.present();
}

/// Поднять следующее окно наверх.
pub fn focus_next() {
    let mut guard = COMPOSITOR.lock();
    let Some(compositor) = guard.as_mut() else {
        return;
    };
    compositor.focus_next();
    compositor.present();
}

/// Размер окна оболочки в символах.
#[must_use]
pub fn shell_size() -> (u32, u32) {
    let mut guard = COMPOSITOR.lock();
    match guard.as_mut().and_then(|c| c.by_title(SHELL_WINDOW)) {
        Some(window) => window.size_in_cells(),
        None => (0, 0),
    }
}

/// Кадры, прямоугольники и число окон.
#[must_use]
pub fn stats() -> (u64, u64, usize) {
    match COMPOSITOR.lock().as_ref() {
        Some(compositor) => compositor.stats(),
        None => (0, 0, 0),
    }
}
