//! Композитор: слои рабочего стола поверх фреймбуфера.
//!
//! Слоёв четыре, и порядок их вывода — это и есть весь рабочий стол:
//!
//! ```text
//!   меню запуска      (если открыто)
//!   панель задач      (всегда сверху окон)
//!   окна              снизу вверх, последнее — активное
//!   фон               градиент с разметкой, рисуется без единого байта памяти
//! ```
//!
//! # Учёт изменённого никуда не делся
//!
//! На экран выводится не всё, а прямоугольники, которые действительно
//! изменились. Это не оптимизация ради оптимизации: фреймбуфер — память
//! устройства, запись в него на порядок дороже записи в обычную память, а набор
//! одной строки в терминале меняет одну строку ячеек, а не полтора миллиона
//! пикселей. Перетаскивание окна порождает два прямоугольника (откуда ушло и
//! куда пришло), и именно поэтому их список длиннее одного.
//!
//! # Почему фон не хранится поверхностью
//!
//! Поверхность размером с экран — это четыре мегабайта при куче в шестнадцать.
//! Градиент считается по номеру строки, а разметка — по остатку от деления
//! координаты, поэтому фон рисуется из ничего, и его хватает на любой экран,
//! который отдаст прошивка.

use alloc::vec::Vec;

use mini_ui::{Rect, Screen, Surface};

use super::panel::{Menu, Panel, PanelHit, Status};
use super::pointer::Pointer;
use super::theme;
use super::window::{App, Hit, Window};

/// Сколько прямоугольников изменений композитор согласен помнить.
///
/// Двенадцать, а не восемь, как было до появления стола: перетаскивание окна
/// даёт сразу два прямоугольника, панель — третий, и запаса на обычную работу
/// (строка терминала, окно состояния) должно оставаться. Переполнение не
/// ошибка — оно стоит одной перерисовки экрана.
const MAX_DAMAGE: usize = 12;

/// Шаг разметки на фоне рабочего стола.
const DOT_STEP: u32 = 48;

/// Размер точки разметки.
const DOT_SIZE: u32 = 2;

pub struct Compositor {
    screen: Screen,
    /// Снизу вверх: последнее окно поверх остальных.
    windows: Vec<Window>,
    /// Индекс активного окна.
    focus: usize,
    panel: Option<Panel>,
    menu: Option<Menu>,
    /// Указатель мыши — верхний слой кадра.
    pointer: Pointer,
    /// Окно, которое сейчас тащат за заголовок.
    ///
    /// Программа, а не индекс: порядок окон меняется при поднятии, и индекс,
    /// запомненный до щелчка, после него указывал бы на соседнее окно.
    drag: Option<App>,
    /// Масштаб глифа для новых окон.
    scale: u32,
    damage: [Rect; MAX_DAMAGE],
    damage_count: usize,
    /// Изменений накопилось больше, чем помещается: проще перерисовать всё.
    damage_overflow: bool,
    frames: u64,
    rects: u64,
}

impl Compositor {
    pub fn new(screen: Screen, scale: u32) -> Self {
        let pointer = Pointer::new(screen.width(), screen.height());
        let mut compositor = Self {
            screen,
            windows: Vec::new(),
            focus: 0,
            panel: None,
            menu: None,
            pointer,
            drag: None,
            scale,
            damage: [Rect::EMPTY; MAX_DAMAGE],
            damage_count: 0,
            damage_overflow: true,
            frames: 0,
            rects: 0,
        };
        compositor.panel = Panel::new(
            compositor.screen.width(),
            compositor.screen.height(),
            scale.min(2),
        );
        let panel_top = compositor.work_bottom();
        compositor.menu = Menu::new(panel_top, scale.min(2));
        compositor
    }

    /// Нижняя граница области, в которой живут окна: верх панели.
    pub fn work_bottom(&self) -> i32 {
        match self.panel.as_ref() {
            Some(panel) => panel.rect.y,
            None => self.screen.height() as i32,
        }
    }

    pub const fn screen_width(&self) -> u32 {
        self.screen.width()
    }

    pub const fn screen_height(&self) -> u32 {
        self.screen.height()
    }

    pub const fn scale(&self) -> u32 {
        self.scale
    }

    // -----------------------------------------------------------------------
    // Окна
    // -----------------------------------------------------------------------

    /// Добавить окно наверх и сделать активным.
    pub fn push(&mut self, window: Window) {
        let rect = window.rect;
        self.windows.push(window);
        self.focus = self.windows.len() - 1;
        self.refresh_decorations();
        // Помечается площадь нового окна, а не весь экран: остальное как было,
        // так и осталось. Разница не косметическая — перерисовка экрана целиком
        // задерживает ввод настолько, что успевает измениться порядок событий.
        self.mark(rect);
    }

    pub fn find(&mut self, app: App) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.app == app)
    }

    /// Где стоит окно программы.
    #[must_use]
    pub fn rect_of(&self, app: App) -> Option<Rect> {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .map(|window| window.rect)
    }

    /// Какая программа живёт в окне с этим номером.
    #[must_use]
    pub fn app_at(&self, index: usize) -> Option<App> {
        self.windows.get(index).map(|window| window.app)
    }

    /// Номер окна программы в порядке по глубине.
    pub fn index_of(&self, app: App) -> Option<usize> {
        self.windows.iter().position(|window| window.app == app)
    }

    pub fn focused_app(&self) -> Option<App> {
        self.windows.get(self.focus).map(|window| window.app)
    }

    pub fn focused_mut(&mut self) -> Option<&mut Window> {
        self.windows.get_mut(self.focus)
    }

    /// Кнопки панели задач: порядок создания, а не порядок по глубине.
    ///
    /// Именно порядок создания: кнопка, переезжающая с места на место при каждом
    /// переключении окон, — это кнопка, в которую нельзя попасть.
    pub fn buttons(&self) -> super::panel::Windows {
        self.windows
            .iter()
            .enumerate()
            .map(|(index, window)| (window.app, index == self.focus))
            .collect()
    }

    /// Поднять окно с номером `index` наверх и сделать активным.
    pub fn raise(&mut self, index: usize) {
        if index >= self.windows.len() {
            return;
        }
        let window = self.windows.remove(index);
        let rect = window.rect;
        self.windows.push(window);
        self.focus = self.windows.len() - 1;
        self.refresh_decorations();
        // Поднятое окно — единственное место, где порядок по глубине
        // изменился; всё остальное на экране осталось прежним.
        self.mark(rect);
    }

    /// Поднять нижнее окно наверх — обход по кругу.
    pub fn focus_next(&mut self) {
        if self.windows.len() < 2 {
            return;
        }
        self.raise(0);
    }

    /// Закрыть активное окно.
    pub fn close_focused(&mut self) -> Option<App> {
        if self.windows.is_empty() {
            return None;
        }
        let window = self.windows.remove(self.focus);
        self.focus = self.windows.len().saturating_sub(1);
        self.refresh_decorations();
        // На месте закрытого окна снова виден фон и то, что было под ним.
        self.mark(window.rect);
        Some(window.app)
    }

    /// Сдвинуть активное окно.
    pub fn move_focused(&mut self, dx: i32, dy: i32) {
        let width = self.screen.width();
        let bottom = self.work_bottom();
        let Some(window) = self.windows.get_mut(self.focus) else {
            return;
        };
        let before = window.rect;
        window.move_within(dx, dy, width, bottom);
        if window.rect == before {
            return;
        }
        let after = window.rect;
        // Два прямоугольника, а не один объединяющий: при большом сдвиге
        // объединение — это почти весь экран, тогда как настоящих изменений два
        // куска по краям.
        self.mark(before);
        self.mark(after);
    }

    // -----------------------------------------------------------------------
    // Указатель
    // -----------------------------------------------------------------------

    /// Сдвинуть указатель и пометить изменившееся.
    ///
    /// Помечаются **два** прямоугольника — откуда стрелка ушла и куда пришла.
    /// Один объединяющий при быстром движении накрыл бы полэкрана, тогда как
    /// настоящих изменений два пятна размером с курсор.
    pub fn move_pointer(&mut self, dx: i32, dy: i32) {
        let (width, height) = (self.screen.width(), self.screen.height());
        let before = self.pointer.rect();
        let moved = self.pointer.move_by(dx, dy, width, height);
        let appeared = self.pointer.show();
        if !moved && !appeared {
            return;
        }
        if moved {
            self.mark(before);
        }
        let after = self.pointer.rect();
        self.mark(after);
    }

    #[must_use]
    pub const fn pointer_position(&self) -> (i32, i32) {
        self.pointer.position()
    }

    #[must_use]
    pub const fn pointer_visible(&self) -> bool {
        self.pointer.is_visible()
    }

    /// Верхнее окно под точкой и то, во что она попала.
    #[must_use]
    pub fn window_at(&self, x: i32, y: i32) -> Option<(usize, Hit)> {
        // Сверху вниз: перекрытое окно щелчок получать не должно.
        self.windows
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, window)| window.hit(x, y).map(|hit| (index, hit)))
    }

    /// Попадание в панель задач.
    #[must_use]
    pub fn panel_at(&self, x: i32, y: i32) -> Option<PanelHit> {
        let panel = self.panel.as_ref()?;
        if !panel.rect.contains(x, y) {
            return None;
        }
        panel.hit(x, y)
    }

    /// Начать или закончить перетаскивание окна.
    pub fn set_drag(&mut self, app: Option<App>) {
        self.drag = app;
    }

    #[must_use]
    pub const fn dragging(&self) -> Option<App> {
        self.drag
    }

    /// Сдвинуть окно, которое тащат.
    pub fn drag_by(&mut self, dx: i32, dy: i32) {
        let Some(app) = self.drag else {
            return;
        };
        let Some(index) = self.index_of(app) else {
            // Окно закрыли, не отпустив кнопку. Такое бывает, и тащить дальше
            // нечего.
            self.drag = None;
            return;
        };
        let width = self.screen.width();
        let bottom = self.work_bottom();
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let before = window.rect;
        window.move_within(dx, dy, width, bottom);
        if window.rect == before {
            return;
        }
        let after = window.rect;
        self.mark(before);
        self.mark(after);
    }

    /// Закрыть окно программы. Возвращает `true`, если оно было.
    pub fn close(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let window = self.windows.remove(index);
        if self.focus >= self.windows.len() {
            self.focus = self.windows.len().saturating_sub(1);
        }
        self.refresh_decorations();
        self.mark(window.rect);
        if self.drag == Some(app) {
            self.drag = None;
        }
        true
    }

    /// Перерисовать украшения всех окон по текущему фокусу.
    fn refresh_decorations(&mut self) {
        let focus = self.focus;
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.draw_decorations(index == focus);
        }
    }

    // -----------------------------------------------------------------------
    // Панель и меню
    // -----------------------------------------------------------------------

    pub fn menu_mut(&mut self) -> Option<&mut Menu> {
        self.menu.as_mut()
    }

    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(Menu::is_open)
    }

    /// Обновить панель задач.
    pub fn refresh_panel(&mut self, status: &Status) {
        let buttons = self.buttons();
        let menu_open = self.menu_open();
        if let Some(panel) = self.panel.as_mut() {
            panel.redraw(&buttons, menu_open, status);
        }
    }

    // -----------------------------------------------------------------------
    // Сборка кадра
    // -----------------------------------------------------------------------

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

    /// Перенести накопленные слоями изменения в общий список.
    fn collect(&mut self) {
        for index in 0..self.windows.len() {
            let Some(window) = self.windows.get_mut(index) else {
                continue;
            };
            let damage = core::mem::replace(&mut window.damage, Rect::EMPTY);
            if damage.is_empty() {
                continue;
            }
            let origin = (window.rect.x, window.rect.y);
            self.mark(damage.translate(origin.0, origin.1));
        }

        if let Some(panel) = self.panel.as_mut() {
            let damage = panel.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(panel.rect.x, panel.rect.y);
                self.mark(rect);
            }
        }

        if let Some(menu) = self.menu.as_mut() {
            let damage = menu.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(menu.rect.x, menu.rect.y);
                self.mark(rect);
            }
        }
    }

    /// Пометить область меню как изменившуюся — при закрытии его надо стереть.
    pub fn mark_menu_area(&mut self) {
        if let Some(menu) = self.menu.as_ref() {
            let rect = menu.rect;
            self.mark(rect);
        }
    }

    /// Собрать кадр: вывести на экран всё, что изменилось.
    pub fn present(&mut self) {
        self.collect();
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

    /// Нарисовать один прямоугольник экрана: фон, окна снизу вверх, панель,
    /// меню.
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
    /// несколько тысяч записей в фреймбуфер на нажатие клавиши.
    fn compose(&self, rect: Rect) {
        self.draw_background(rect);
        for window in &self.windows {
            let overlap = window.rect.intersect(&rect);
            if !overlap.is_empty() {
                self.blit(window.surface(), window.rect, overlap);
            }
        }
        if let Some(panel) = self.panel.as_ref() {
            let overlap = panel.rect.intersect(&rect);
            if !overlap.is_empty() {
                self.blit(panel.surface(), panel.rect, overlap);
            }
        }
        if let Some(menu) = self.menu.as_ref() {
            if menu.is_open() {
                let overlap = menu.rect.intersect(&rect);
                if !overlap.is_empty() {
                    self.blit(menu.surface(), menu.rect, overlap);
                }
            }
        }
        // Курсор — последним и без проверки пересечения: он мал, а обрезка
        // однобитной картинки уже сделана внутри `draw_bitmap`. Проверка
        // «попадает ли он в прямоугольник» стоила бы больше, чем экономила.
        self.pointer.draw(&self.screen);
    }

    /// Вывести часть поверхности слоя, попадающую в `rect` (координаты экрана).
    fn blit(&self, surface: &Surface, placed: Rect, rect: Rect) {
        let src = rect.translate(-placed.x, -placed.y);
        self.screen.blit(surface, (rect.x, rect.y), src);
    }

    /// Фон рабочего стола: вертикальный градиент и точки разметки.
    fn draw_background(&self, rect: Rect) {
        let height = self.screen.height().max(1);
        for y in rect.y..rect.bottom() {
            if y < 0 {
                continue;
            }
            // Вес смешивания — доля пройденной высоты экрана. Считается от
            // экрана, а не от прямоугольника: иначе каждый кусок фона имел бы
            // собственный градиент, и границы кусков были бы видны.
            let weight = ((y as u32).min(height - 1) * 255 / height) as u8;
            let color = theme::DESKTOP_TOP.mix(theme::DESKTOP_BOTTOM, weight);
            self.screen.fill(Rect::new(rect.x, y, rect.w, 1), color);
        }

        // Разметка: редкая сетка точек. Она даёт глазу опору — на однородной
        // заливке перетаскиваемое окно кажется стоящим на месте.
        let first_x = align_up(rect.x, DOT_STEP);
        let first_y = align_up(rect.y, DOT_STEP);
        let mut y = first_y;
        while y < rect.bottom() {
            let mut x = first_x;
            while x < rect.right() {
                self.screen
                    .fill(Rect::new(x, y, DOT_SIZE, DOT_SIZE), theme::DESKTOP_DOT);
                x += DOT_STEP as i32;
            }
            y += DOT_STEP as i32;
        }
    }

    /// Сколько кадров собрано, сколько прямоугольников выведено, сколько окон.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, usize) {
        (self.frames, self.rects, self.windows.len())
    }
}

/// Ближайшая сверху координата, кратная шагу разметки.
fn align_up(value: i32, step: u32) -> i32 {
    let step = step as i32;
    // Округление к большему работает и для отрицательных: `div_euclid`
    // округляет вниз в математическом смысле, а не в сторону нуля.
    (value + step - 1).div_euclid(step) * step
}
