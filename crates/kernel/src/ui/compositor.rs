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
//!
//! # Кадр собирается в памяти и выводится целиком
//!
//! Слои складываются не на экран, а в буфер, и на экран уходит уже готовая
//! картинка — одним копированием строк. Это даёт три разные вещи, и каждая
//! стоила бы отдельной работы:
//!
//! 1. **Ничего не мелькает.** Раньше прямоугольник сначала заливался фоном, а
//!    окно ложилось поверх — и между этими двумя действиями фон было видно.
//! 2. **В экран пишут один раз.** Фреймбуфер — память устройства; фон, значок и
//!    окно, легшие на одно место, стоили трёх записей в неё, а теперь одной.
//! 3. **Буфер можно читать.** На этом стоит сглаживание шрифта: полутон на
//!    ступеньке буквы смешивается с тем, что под ней, а прочитать «что под
//!    ней» на экране невозможно — чтение write-combining памяти сбрасывает
//!    буфер записи.
//!
//! Буфер — **полоса**, а не целый экран: экран 1920×1080 в четырёх байтах на
//! точку — это восемь мегабайт из шестнадцати, то есть половина кучи под одну
//! картинку. Полоса в полмегабайта даёт то же самое, а прямоугольник выше её
//! просто выводится в несколько заходов.

use alloc::vec::Vec;

use mini_ui::{Color, Rect, Screen, Surface, draw};

use super::context::{Action, ContextMenu, Reply};
use super::icons::{Icons, Kind};
use super::panel::{Menu, Panel, PanelHit, Status};
use super::pointer::Pointer;
use super::shade::Shade;
use mini_ui::theme;
use super::window::{App, Hit, Window};

/// Сколько прямоугольников изменений композитор согласен помнить.
///
/// Двенадцать, а не восемь, как было до появления стола: перетаскивание окна
/// даёт сразу два прямоугольника, панель — третий, и запаса на обычную работу
/// (строка терминала, окно состояния) должно оставаться. Переполнение не
/// ошибка — оно стоит одной перерисовки экрана.
const MAX_DAMAGE: usize = 12;

/// Шаг разметки на фоне рабочего стола.
const DOT_STEP: u32 = 24;

/// Размер точки разметки.
const DOT_SIZE: u32 = 1;

/// На сколько тень плавающего слоя выходит за его края.
///
/// Тень — единственное, что отделяет панель задач от обоев там, где обои
/// светлые: однопиксельной обводки на светлой теме не видно, а без отделения
/// панель читается как полоса, нарисованная прямо на фоне.
const SHADOW_SPREAD: u32 = 18;

/// На сколько тень смещена вниз относительно самого слоя.
const SHADOW_DROP: u32 = 10;

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
    /// Значки на самом столе — слой между фоном и окнами.
    icons: Icons,
    /// Меню по правому щелчку. Верхний слой, как и меню запуска.
    context: Option<ContextMenu>,
    /// Шторка телефона — слой поверх окон (см. [`super::shade`]).
    ///
    /// `None` на настольной машине: там её нет вовсе, а не «есть и закрыта».
    shade: Option<Shade>,
    /// Окно, которое сейчас тащат за заголовок.
    ///
    /// Программа, а не индекс: порядок окон меняется при поднятии, и индекс,
    /// запомненный до щелчка, после него указывал бы на соседнее окно.
    drag: Option<App>,
    /// Где стояло окно в тот миг, когда его взяли.
    ///
    /// Нужно одному: отличить перетаскивание от щелчка по заголовку. Щелчок
    /// тоже проходит через захват и отпускание, и без этой точки в журнале
    /// появляется строка «окно переехало туда, где оно и было». Она не просто
    /// лишняя: стенд ждёт переезда по этой строке и принимает за него щелчок,
    /// после чего целится в окно по старому месту.
    drag_from: (i32, i32),
    /// Захват меняет размер окна, а не его место.
    drag_resizes: bool,
    /// Окно программы, над содержимым которого указатель был в прошлый раз
    /// (фаза N7h).
    ///
    /// Нужно одному: сказать окну, что указатель ушёл. Узнать это из текущего
    /// движения нельзя — оно достаётся уже другому окну или столу.
    hovered: Option<App>,
    /// Масштаб глифа для новых окон.
    scale: u32,
    /// Буфер, в котором собирается кадр, — полоса во всю ширину экрана.
    back: Surface,
    damage: [Rect; MAX_DAMAGE],
    damage_count: usize,
    /// Изменений накопилось больше, чем помещается: проще перерисовать всё.
    damage_overflow: bool,
    frames: u64,
    rects: u64,
    /// Кадр не собирать: за этим событием в очереди ввода уже лежит следующее.
    ///
    /// Изменения при этом не теряются — они копятся в прямоугольниках окон и в
    /// [`Self::damage`] и уходят на экран ближайшим несложенным кадром.
    deferred: bool,
}

/// Сколько памяти отдаётся под полосу, в которой собирается кадр.
///
/// Полмегабайта — это сотня строк на экране 1280 точек и семь десятков на 1920.
/// Больше не нужно: полоса влияет только на число заходов, а не на то, что
/// получится; меньше — и на каждый прямоугольник изменений приходилось бы по
/// десятку выводов на экран, каждый со своим обходом всех слоёв.
const BAND_BYTES: u32 = 512 * 1024;

/// Наименьшая и наибольшая высота полосы в строках.
const BAND_MIN: u32 = 16;
const BAND_MAX: u32 = 256;

/// Слои стола, посчитанные от размера экрана, — собранные заранее под новый
/// режим (фаза С6a). См. [`Compositor::prepare_screen`].
pub struct Layers {
    back: Surface,
    panel: Option<Panel>,
    menu: Option<Menu>,
    context: Option<ContextMenu>,
}

impl Compositor {
    /// Поднять композитор на этом экране.
    ///
    /// `None`, если не хватило памяти под буфер кадра. Это не отказ системы:
    /// ядро в таком случае работает с оболочкой в серийной линии — ровно как
    /// тогда, когда прошивка не дала фреймбуфера вовсе. Отдельного пути «рисуем
    /// прямо на экран, без буфера» здесь намеренно нет: он означал бы две
    /// сборки кадра, из которых вторая проверялась бы только на машине, где не
    /// хватило памяти, — то есть никогда.
    pub fn new(screen: Screen, scale: u32) -> Option<Self> {
        let pointer = Pointer::new(screen.width(), screen.height());
        let rows = (BAND_BYTES / (screen.width().max(1) * 4)).clamp(BAND_MIN, BAND_MAX);
        let wall = theme::palette().wall_top;
        let back = Surface::new(screen.width(), rows.min(screen.height().max(1)), wall)?;
        let mut compositor = Self {
            back,
            screen,
            windows: Vec::new(),
            focus: 0,
            panel: None,
            menu: None,
            pointer,
            icons: Icons::new(scale),
            context: ContextMenu::new(scale.min(2)),
            shade: None,
            drag: None,
            drag_from: (0, 0),
            drag_resizes: false,
            hovered: None,
            scale,
            damage: [Rect::EMPTY; MAX_DAMAGE],
            damage_count: 0,
            damage_overflow: true,
            frames: 0,
            rects: 0,
            deferred: false,
        };
        compositor.panel = Panel::new(
            compositor.screen.width(),
            compositor.screen.height(),
            scale.min(2),
        );
        let panel_top = compositor.work_bottom();
        compositor.menu = Menu::new(panel_top, scale.min(2), compositor.screen.width());
        // Шторка заводится только на телефоне — [`Shade::new`] сам вернёт
        // `None` на настольной машине.
        compositor.shade = Shade::new(compositor.screen.width(), panel_top, scale);
        // Длина столбца значков считается от рабочей области, а не от экрана:
        // ячейка, заехавшая под панель задач, щёлкается панелью, а не значком.
        compositor.icons.set_area(panel_top);
        // Ширина экрана и верх сетки — телефону (см. [`Icons::set_span`]). На
        // столе вызов безвреден: мобильная раскладка спрашивает эти числа, а
        // настольная о них не знает.
        compositor.icons.set_span(
            compositor.screen.width(),
            mini_ui::paint::Ctx::scaled(scale).px(theme::M_STATUS_H + theme::M_INSET),
        );
        compositor.icons.reload();
        Some(compositor)
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
        self.mark_layer(rect);
    }

    pub fn find(&mut self, app: App) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.app == app)
    }

    /// Окно программы, над **содержимым** которого стоит точка, и эта точка
    /// внутри его поверхности. Заголовок и рамка — не содержимое: программа их
    /// не рисует и событий о них не ждёт.
    #[must_use]
    pub fn program_under(&self, x: i32, y: i32) -> Option<(App, i32, i32)> {
        let (index, hit) = self.window_at(x, y)?;
        let window = self.windows.get(index)?;
        if hit != Hit::Body || !window.is_program() {
            return None;
        }
        let top = window.rect.y + Window::title_height(self.scale) as i32;
        Some((window.app, x - window.rect.x, y - top))
    }

    /// См. [`Self::hovered`].
    pub const fn hovered(&self) -> Option<App> {
        self.hovered
    }

    pub fn set_hovered(&mut self, app: Option<App>) {
        self.hovered = app;
    }

    /// Где стоит окно программы.
    #[must_use]
    pub fn rect_of(&self, app: App) -> Option<Rect> {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .map(|window| window.rect)
    }

    /// Как называется окно этой программы.
    ///
    /// Нужно журналу: у окна программы имя своё, и [`App::title`] о нём не
    /// знает. Строка собственная, а не заимствованная, по той же причине, что и
    /// в [`Compositor::buttons`], — стол выходит из-под замка, окно может
    /// закрыться.
    #[must_use]
    pub fn caption_of(&self, app: App) -> Option<alloc::string::String> {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .map(|window| window.caption().into())
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

    /// Активное окно — то, которому достаётся ввод.
    ///
    /// Свёрнутое окно активным **не бывает**, и это не придирка к слову. Ввод
    /// уходит туда, что вернёт эта функция ([`super::route`]), а `None` означает
    /// «в оболочку». Свёрнутое окно, названное активным, означает клавиши,
    /// уходящие туда, где их некому показать: строка, набранная человеком,
    /// пропадает без следа, и выглядит это как «система перестала принимать
    /// команды».
    ///
    /// Куплено красным прогоном 48a: сценарий `installed` сворачивал оба окна и
    /// после этого посылал команду в серийную линию. Она уезжала в свёрнутое
    /// окно монитора.
    pub fn focused_app(&self) -> Option<App> {
        let window = self.windows.get(self.focus)?;
        (!window.minimized).then_some(window.app)
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
            .map(|(index, window)| super::panel::Entry {
                app: window.app,
                caption: window.caption().into(),
                focused: index == self.focus,
                minimized: window.minimized,
            })
            .collect()
    }

    /// Поднять окно с номером `index` наверх и сделать активным.
    pub fn raise(&mut self, index: usize) {
        if index >= self.windows.len() {
            return;
        }
        // Кто теряет фокус — запоминается до перестановки: индексы после неё
        // съедут, а перекрасить надо ровно два заголовка. Перекрашивать все
        // окна здесь дорого не «в принципе», а измеримо: на 1920×1080 в
        // отладочной сборке переключение окон занимало столько, что второе
        // нажатие Tab не успевало быть прочитанным с клавиатуры и терялось.
        let losing = self.windows.get(self.focus).map(|window| window.app);
        // Перерисовать надо не всё поднятое окно, а только то, что его
        // закрывало: пиксели, видные и до, и после поднятия, не изменились.
        // Разница измерима — на 1920×1080 в отладочной сборке перерисовка окна
        // целиком занимала больше четверти секунды, и следующее нажатие
        // клавиши терялось: клавиатуру некому было опросить.
        let mut uncovered = [Rect::EMPTY; MAX_DAMAGE];
        let mut uncovered_count = 0;
        {
            let raised = self.windows[index].rect;
            for above in self.windows[index + 1..].iter().filter(|w| !w.minimized) {
                let overlap = raised.intersect(&above.rect);
                if !overlap.is_empty() && uncovered_count < MAX_DAMAGE {
                    uncovered[uncovered_count] = overlap;
                    uncovered_count += 1;
                }
            }
        }
        let window = self.windows.remove(index);
        self.windows.push(window);
        self.focus = self.windows.len() - 1;
        if let Some(app) = losing {
            if let Some(previous) = self.windows.iter_mut().find(|window| window.app == app) {
                previous.draw_decorations(false);
            }
        }
        if let Some(window) = self.windows.last_mut() {
            window.draw_decorations(true);
        }
        // Поднятое окно — единственное место, где порядок по глубине
        // изменился; всё остальное на экране осталось прежним.
        for index in 0..uncovered_count {
            self.mark_layer(uncovered[index]);
        }
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
        self.mark_layer(window.rect);
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
        self.mark_layer(before);
        self.mark_layer(after);
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

    /// Поставить указатель туда, куда показало устройство, и пометить
    /// изменившееся. Возвращает **фактическое** приращение положения.
    ///
    /// Приращение возвращается не для симметрии: за курсором тащится окно, а
    /// планшет о приращениях ничего не сообщает — единственный, кто их знает,
    /// это тот, кто хранит прежнее положение. Заодно из ответа исчезает та
    /// часть движения, которую съел край экрана.
    pub fn move_pointer_to(&mut self, x_fraction: u16, y_fraction: u16) -> (i32, i32) {
        let (width, height) = (self.screen.width(), self.screen.height());
        let (from_x, from_y) = self.pointer.position();
        let before = self.pointer.rect();

        let moved = self.pointer.move_to(
            scale_fraction(x_fraction, width),
            scale_fraction(y_fraction, height),
            width,
            height,
        );
        let appeared = self.pointer.show();
        if !moved && !appeared {
            return (0, 0);
        }
        if moved {
            self.mark(before);
        }
        let after = self.pointer.rect();
        self.mark(after);

        let (to_x, to_y) = self.pointer.position();
        (to_x - from_x, to_y - from_y)
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
        // Сверху вниз: перекрытое окно щелчок получать не должно. Свёрнутого
        // окна на экране нет — щелчок проходит сквозь его прежнее место.
        self.windows
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, window)| !window.minimized)
            .find_map(|(index, window)| window.hit(x, y).map(|hit| (index, hit)))
    }

    /// Значок стола под точкой — если она не накрыта окном.
    ///
    /// Проверка «не накрыта окном» здесь, а не у вызывающего: значок лежит на
    /// столе, то есть ниже всех окон, и щелчок сквозь окно по значку под ним —
    /// это ровно та ошибка, которую пользователь заметит первой.
    #[must_use]
    pub fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        if self.window_at(x, y).is_some() {
            return None;
        }
        self.icons.at(x, y)
    }

    /// Что за значок стоит на этом месте сетки.
    #[must_use]
    pub fn icon_kind(&self, index: usize) -> Option<Kind> {
        self.icons.item(index).map(|item| item.kind.clone())
    }

    /// Подпись значка.
    #[must_use]
    pub fn icon_label(&self, index: usize) -> Option<alloc::string::String> {
        self.icons.item(index).map(|item| item.label.clone())
    }

    /// Путь к тому, что за значком, — только у файлов и каталогов стола.
    #[must_use]
    pub fn icon_path(&self, index: usize) -> Option<alloc::string::String> {
        self.icons.item(index).and_then(|item| item.path.clone())
    }

    /// Какой значок выбран сейчас.
    #[must_use]
    pub fn icon_selection(&self) -> Option<usize> {
        self.icons.selection()
    }

    /// Сколько значков на столе и сколько из них — файлы и каталоги.
    #[must_use]
    pub fn icon_counts(&self) -> (usize, usize) {
        self.icons.counts()
    }

    /// Выделить значок по пути — тем, кто только что переименовал файл.
    pub fn select_icon_path(&mut self, path: &str) {
        let index = self.icons.index_of_path(path);
        self.select_icon(index);
    }

    /// Выделить значок (или снять выделение).
    pub fn select_icon(&mut self, index: Option<usize>) {
        let damage = self.icons.select(index);
        if !damage.is_empty() {
            self.mark(damage);
        }
    }

    /// Перечитать каталог стола и показать его заново.
    ///
    /// Помечается объединение сетки **до** и **после**: удалённый файл
    /// освобождает ячейку, которую иначе никто не стёр бы, и она осталась бы на
    /// экране до первой чужой перерисовки.
    pub fn reload_icons(&mut self) {
        let before = self.icons.bounds();
        self.icons.reload();
        let after = self.icons.bounds();
        self.mark_layer(before.union(&after));
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
        if let Some(app) = app {
            self.drag_from = self.rect_of(app).map_or((0, 0), |rect| (rect.x, rect.y));
        } else {
            self.drag_resizes = false;
        }
    }

    /// Переехало ли окно с тех пор, как его взяли.
    #[must_use]
    pub fn drag_moved(&self) -> bool {
        match self.drag.and_then(|app| self.rect_of(app)) {
            Some(rect) => (rect.x, rect.y) != self.drag_from,
            None => false,
        }
    }

    /// Начать изменение размера окна: тот же захват, другое действие.
    pub fn set_resize_drag(&mut self, app: App) {
        self.drag = Some(app);
        self.drag_from = self.rect_of(app).map_or((0, 0), |rect| (rect.x, rect.y));
        self.drag_resizes = true;
    }

    #[must_use]
    pub const fn dragging(&self) -> Option<App> {
        self.drag
    }

    /// Сдвинуть окно, которое тащат, — или изменить его размер.
    pub fn drag_by(&mut self, dx: i32, dy: i32) {
        let Some(app) = self.drag else {
            return;
        };
        let Some(index) = self.index_of(app) else {
            // Окно закрыли, не отпустив кнопку. Такое бывает, и тащить дальше
            // нечего.
            self.drag = None;
            self.drag_resizes = false;
            return;
        };
        let width = self.screen.width();
        let bottom = self.work_bottom();
        let resizes = self.drag_resizes;
        let Some(window) = self.windows.get_mut(index) else {
            return;
        };
        let before = window.rect;
        if resizes {
            // Размер считается от места окна до указателя, а не приращением к
            // прежнему: окно, упёршееся в наименьший размер, иначе продолжало бы
            // «копить» движение мыши и отставало бы от неё на обратном ходу.
            let w = (before.w as i32 + dx).max(0) as u32;
            let h = (before.h as i32 + dy).max(0) as u32;
            let limit_w = width.saturating_sub(before.x.max(0) as u32);
            let limit_h = (bottom - before.y).max(0) as u32;
            if !window.resize(w.min(limit_w), h.min(limit_h)) {
                return;
            }
        } else {
            window.move_within(dx, dy, width, bottom);
        }
        if window.rect == before {
            return;
        }
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
    }

    /// Свернуть окно: убрать с экрана, оставив в панели задач.
    pub fn minimize(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        if window.minimized {
            return false;
        }
        window.minimized = true;
        let rect = window.rect;
        self.mark_layer(rect);
        // Фокус переезжает на **верхнее из оставшихся видимых**. Прежде здесь
        // стоял `focus_next`, а тот поднимает нулевое окно, не спрашивая, не
        // свёрнуто ли оно: свернув последнее видимое, система делала активным
        // соседнее свёрнутое. Ввод после этого уходил в никуда.
        //
        // Если видимых не осталось, фокус остаётся на этом окне, и это верно:
        // [`Compositor::focused_app`] назовёт его неактивным, то есть ввод
        // достанется оболочке — единственному месту, где команду ещё можно
        // набрать.
        if self.focus == index {
            if let Some(next) = self.windows.iter().rposition(|window| !window.minimized) {
                self.raise(next);
            }
        }
        true
    }

    /// Вернуть свёрнутое окно на экран.
    pub fn restore(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        if !window.minimized {
            return false;
        }
        window.minimized = false;
        // Поверхность цела, но экран под окном за это время перерисовали —
        // значит показать надо всё окно целиком, а не то, что в нём изменилось.
        window.damage = Rect::new(0, 0, window.rect.w, window.rect.h);
        let rect = window.rect;
        // Вернувшееся окно приносит с собой и тень: без неё вокруг него
        // осталась бы светлая рамка от того, что лежало здесь, пока оно было
        // свёрнуто.
        self.mark_layer(rect);
        true
    }

    /// Свёрнуто ли окно этой программы.
    #[must_use]
    pub fn is_minimized(&self, app: App) -> bool {
        self.windows
            .iter()
            .find(|window| window.app == app)
            .is_some_and(|window| window.minimized)
    }

    /// Развернуть окно на всю рабочую область или вернуть прежний размер.
    pub fn toggle_maximize(&mut self, app: App) -> bool {
        let Some(index) = self.index_of(app) else {
            return false;
        };
        let width = self.screen.width();
        let bottom = self.work_bottom();
        let Some(window) = self.windows.get_mut(index) else {
            return false;
        };
        let before = window.rect;
        if !window.toggle_maximize(width, bottom) {
            return false;
        }
        let after = window.rect;
        self.mark_layer(before);
        self.mark_layer(after);
        true
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
        self.mark_layer(window.rect);
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

    /// Сколько строк в меню запуска.
    #[must_use]
    pub fn menu_items(&self) -> usize {
        self.menu.as_ref().map_or(0, Menu::item_count)
    }

    /// Сколько программ лежит в `/bin`.
    #[must_use]
    pub fn menu_bin_programs(&self) -> usize {
        self.menu.as_ref().map_or(0, Menu::bin_programs)
    }

    pub fn menu_open(&self) -> bool {
        self.menu.as_ref().is_some_and(Menu::is_open)
    }

    /// Пересобрать закрытое меню запуска (фаза N8): строки пакетов читаются из
    /// реестра, а пакет могли поставить после запуска стола.
    ///
    /// Нехватка памяти под новую карточку оставляет прежнюю — меню без нового
    /// пакета лучше, чем меню, которое не открылось.
    pub fn reload_menu(&mut self) {
        if self.menu_open() {
            return;
        }
        if let Some(menu) = Menu::new(self.work_bottom(), self.scale.min(2), self.screen.width()) {
            self.menu = Some(menu);
        }
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
            // Переполнение — отдельный счётчик: «весь экран перерисован» имеет
            // две причины, и лечатся они по-разному. Двенадцать накопившихся
            // прямоугольников — это учёт, который не справляется; смена режима
            // или темы — это честная полная перерисовка, которой так и надо.
            super::note_overflow();
            return;
        }
        self.damage[self.damage_count] = rect;
        self.damage_count += 1;
    }

    /// Пометить изменившимся **слой целиком**: вместе с его тенью.
    ///
    /// Отдельно от [`Compositor::mark`], а не вместо него, из-за цены. Тень
    /// выходит за границы слоя, и уехавшее окно оставило бы за собой её след, —
    /// но расширять так каждый прямоугольник значило бы платить за это и на
    /// каждой напечатанной в терминале букве. Буква тени не отбрасывает; слой
    /// отбрасывает. Поэтому расширяются только те прямоугольники, которые
    /// описывают появление, исчезновение или переезд слоя.
    fn mark_layer(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        // Ореол вокруг слоя — место под его тень: стирая слой, стереть надо и
        // её. На телефоне тени нет (см. [`Self::drop_shadow`]), и ореол там
        // означал бы прямоугольник на 56 точек шире нужного с каждой стороны —
        // два таких накрывают экран целиком, учёт изменённого переполняется, и
        // кадр перерисовывает всё.
        let halo = if theme::is_mobile() {
            0
        } else {
            (SHADOW_SPREAD + SHADOW_DROP) * self.scale
        };
        self.mark(Rect::new(
            rect.x - halo as i32,
            rect.y - halo as i32,
            rect.w + halo * 2,
            rect.h + halo * 2,
        ));
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

        if let Some(menu) = self.context.as_mut() {
            let damage = menu.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(menu.rect.x, menu.rect.y);
                self.mark(rect);
            }
        }

        if let Some(shade) = self.shade.as_mut() {
            let damage = shade.take_damage();
            if !damage.is_empty() {
                let rect = damage.translate(shade.rect.x, shade.rect.y);
                self.mark(rect);
            }
        }
    }

    /// Открыть или закрыть шторку. Возвращает новое состояние.
    ///
    /// `None` — шторки на этой машине нет (см. [`Shade::new`]).
    pub fn toggle_shade(&mut self) -> Option<bool> {
        let rect = self.shade.as_ref()?.rect;
        let open = self.shade.as_mut()?.toggle();
        // Закрытую шторку надо стереть: под ней стол и окна, которые никто не
        // перерисовывал, — они не «изменились», но их снова видно.
        if !open {
            self.mark_layer(rect);
        }
        Some(open)
    }

    #[must_use]
    pub fn shade_open(&self) -> bool {
        self.shade.as_ref().is_some_and(Shade::is_open)
    }

    /// Попадает ли точка в открытую шторку.
    #[must_use]
    pub fn shade_contains(&self, x: i32, y: i32) -> bool {
        self.shade.as_ref().is_some_and(|shade| shade.contains(x, y))
    }

    /// Меню стола: открыть в точке, закрыть, спросить пункт под указателем.
    pub fn open_context(&mut self, x: i32, y: i32, items: &[Action]) {
        let screen = (self.screen.width(), self.screen.height());
        let bottom = self.work_bottom();
        if let Some(menu) = self.context.as_mut() {
            menu.open_at(x, y, screen, bottom, items);
        }
    }

    pub fn context_open(&self) -> bool {
        self.context.as_ref().is_some_and(ContextMenu::is_open)
    }

    /// Занято ли меню набором имени или вопросом об удалении.
    pub fn context_editing(&self) -> bool {
        self.context
            .as_ref()
            .is_some_and(|menu| menu.is_open() && menu.is_editing())
    }

    /// Где стоит меню стола — если оно открыто.
    pub fn context_rect(&self) -> Option<Rect> {
        self.context
            .as_ref()
            .filter(|menu| menu.is_open())
            .map(|menu| menu.rect)
    }

    /// Накрывает ли меню стола эту точку.
    pub fn context_contains(&self, x: i32, y: i32) -> bool {
        self.context
            .as_ref()
            .is_some_and(|menu| menu.is_open() && menu.rect.contains(x, y))
    }

    pub fn context_action_at(&self, x: i32, y: i32) -> Option<Action> {
        self.context.as_ref().and_then(|menu| menu.action_at(x, y))
    }

    /// Отдать клавишу меню стола.
    pub fn context_key(&mut self, event: crate::input::KeyEvent) -> Reply {
        match self.context.as_mut() {
            Some(menu) => menu.handle_key(event),
            None => Reply::Ignored,
        }
    }

    /// Перевести меню в набор нового имени.
    pub fn context_rename(&mut self, name: &str) {
        let before = self.context.as_ref().map(|menu| menu.rect);
        if let Some(menu) = self.context.as_mut() {
            menu.start_rename(name);
        }
        self.mark_shrunk(before);
    }

    /// Спросить в меню, точно ли удалять.
    pub fn context_confirm(&mut self, name: &str) {
        let before = self.context.as_ref().map(|menu| menu.rect);
        if let Some(menu) = self.context.as_mut() {
            menu.start_confirm(name);
        }
        self.mark_shrunk(before);
    }

    /// Стереть то, что осталось от прежнего, большего меню.
    ///
    /// Карточка меняет высоту вместе с тем, что в ней: список пунктов выше
    /// поля ввода имени. Меню сообщает об изменившемся в собственных
    /// координатах, а их композитор сдвигает на **новый** прямоугольник — и
    /// освободившаяся полоса под ним остаётся на экране куском прошлой
    /// карточки. Помечается разница, а не всё подряд: обычно она пуста.
    fn mark_shrunk(&mut self, before: Option<Rect>) {
        let Some(before) = before else { return };
        let Some(after) = self.context.as_ref().map(|menu| menu.rect) else {
            return;
        };
        if before == after {
            return;
        }
        self.mark_layer(before.union(&after));
    }

    /// Подсветить в меню пункт, по которому щёлкнули.
    pub fn context_select(&mut self, action: Action) {
        if let Some(menu) = self.context.as_mut() {
            menu.select_action(action);
        }
    }

    /// Показать в меню ответ действия — оно остаётся открытым.
    pub fn context_note(&mut self, note: &str) {
        if let Some(menu) = self.context.as_mut() {
            menu.set_note(note);
        }
    }

    /// Закрыть меню стола и стереть его с экрана.
    pub fn close_context(&mut self) {
        let rect = self.context.as_ref().map(|menu| menu.rect);
        if let Some(menu) = self.context.as_mut() {
            menu.close();
        }
        if let Some(rect) = rect {
            self.mark_layer(rect);
        }
    }

    /// Перекрасить стол в другую тему.
    ///
    /// Перекрашивается всё и сразу: тема — это цвет каждой точки, и оставить
    /// хоть один слой в прежней палитре значит показать человеку наполовину
    /// перекрашенный стол, который он примет за поломку.
    ///
    /// Поверхности при этом не пересоздаются: размеры от темы не зависят, а
    /// пересоздание стоило бы отказа выделения там, где ничего выделять не
    /// нужно.
    pub fn restyle(&mut self, status: &Status) {
        let focus = self.focus;
        for (index, window) in self.windows.iter_mut().enumerate() {
            window.restyle();
            window.draw_decorations(index == focus);
        }
        self.icons.restyle();
        if let Some(menu) = self.context.as_mut() {
            menu.restyle();
        }
        // Панель рисует себя заново только при изменившемся содержимом (см.
        // [`Panel::redraw`]), а здесь меняются цвета — их отпечаток не видит.
        if let Some(panel) = self.panel.as_mut() {
            panel.forget();
        }
        self.refresh_panel(status);
        if let Some(menu) = self.menu.as_mut() {
            menu.restyle();
        }
        self.repaint_all();
    }

    /// Пересобрать все окна под новую высоту заголовка и перекрасить стол.
    ///
    /// Окно, которому после этого не хватило рабочей области, поджимается к
    /// её низу: заголовок вырос, и нижняя кромка иначе ушла бы под панель.
    pub fn retitle_all(&mut self, status: &Status) {
        let bottom = self.work_bottom();
        for window in self.windows.iter_mut() {
            window.retitle();
            let overflow = window.rect.bottom() - bottom;
            if overflow > 0 {
                window.rect.y = (window.rect.y - overflow).max(0);
            }
        }
        self.restyle(status);
    }

    /// Собрать слои под экран другого размера, ничего пока не меняя.
    ///
    /// Отдельно от [`Self::adopt_screen`], и порядок важен: память спрашивается
    /// **до** того, как переключён адаптер. Не хватило — `None`, адаптер не
    /// тронут, экран прежний. В обратном порядке отказ означал бы экран нового
    /// размера и стол, которому нечем на нём рисовать.
    pub fn prepare_screen(&self, width: u32, height: u32) -> Option<Layers> {
        let rows = (BAND_BYTES / (width.max(1) * 4)).clamp(BAND_MIN, BAND_MAX);
        let back = Surface::new(width, rows.min(height.max(1)), theme::palette().wall_top)?;
        let panel = Panel::new(width, height, self.scale.min(2));
        let top = panel.as_ref().map_or(height as i32, |panel| panel.rect.y);
        let menu = Menu::new(top, self.scale.min(2), width);
        let context = ContextMenu::new(self.scale.min(2));
        Some(Layers { back, panel, menu, context })
    }

    /// Перейти на экран нового размера — смена режима на ходу.
    ///
    /// Всё, что посчитано от размера экрана, заменяется собранным в
    /// [`Self::prepare_screen`]. Окна переезжают, а не строятся заново: в них
    /// состояние — строки терминала, память программы, — и пересоздать окно
    /// значило бы его потерять. Окно, не влезающее в новый экран, поджимается,
    /// если умеет менять размер, и в любом случае прижимается внутрь: окно
    /// программы размера не меняет (см. `Window::rebuild`).
    ///
    /// Масштаб интерфейса остаётся прежним. Смена масштаба пересобрала бы сетку
    /// терминала, и строки, уже набранные человеком, переехали бы — это другая
    /// работа, и в эту фазу она не входит.
    pub fn adopt_screen(&mut self, screen: Screen, layers: Layers, status: &Status) {
        let (width, height) = (screen.width(), screen.height());
        self.screen = screen;
        self.back = layers.back;
        self.panel = layers.panel;
        self.menu = layers.menu;
        self.context = layers.context;
        self.pointer = Pointer::new(width, height);
        self.drag = None;
        let bottom = self.work_bottom();
        self.icons.set_area(bottom);
        self.icons.reload();
        for window in self.windows.iter_mut() {
            let fit_w = window.rect.w.min(width);
            let fit_h = window.rect.h.min(bottom.max(1) as u32);
            if (fit_w, fit_h) != (window.rect.w, window.rect.h) {
                // Отказ — окно программы или нехватка памяти: окно остаётся
                // своего размера и только прижимается ниже.
                let _ = window.resize(fit_w, fit_h);
            }
            let max_x = (width as i32 - window.rect.w as i32).max(0);
            let max_y = (bottom - window.rect.h as i32).max(0);
            window.rect.x = window.rect.x.clamp(0, max_x);
            window.rect.y = window.rect.y.clamp(0, max_y);
        }
        self.restyle(status);
        self.damage_count = 0;
        self.damage_overflow = true;
    }

    /// Перерисовать весь экран — то, что делает пункт «Refresh».
    pub fn repaint_all(&mut self) {
        let all = self.screen.bounds();
        self.mark(all);
    }

    /// Пометить область меню как изменившуюся — при закрытии его надо стереть.
    pub fn mark_menu_area(&mut self) {
        if let Some(menu) = self.menu.as_ref() {
            let rect = menu.rect;
            self.mark_layer(rect);
        }
    }

    /// Откладывать ли сборку кадров — см. [`Self::deferred`].
    pub fn set_deferred(&mut self, deferred: bool) {
        self.deferred = deferred;
    }

    /// Собрать кадр: вывести на экран всё, что изменилось.
    pub fn present(&mut self) {
        if self.deferred {
            return;
        }
        self.collect();
        if !self.damage_overflow && self.damage_count == 0 {
            return;
        }
        self.frames += 1;
        // И наружу, в атомик: среднее на кадр считает тот, кто спрашивает по
        // кабелю, а стол к тому времени уже занят (см. [`super::timing`]).
        super::note_frame();

        if self.damage_overflow {
            let all = self.screen.bounds();
            super::note_full_frame();
            self.compose(all);
            self.rects += 1;
            self.damage_overflow = false;
            self.damage_count = 0;
            return;
        }
        super::note_partial_frame(self.damage_count as u64);

        for index in 0..self.damage_count {
            let rect = self.damage[index].intersect(&self.screen.bounds());
            if !rect.is_empty() {
                super::note_rect(u64::from(rect.w) * u64::from(rect.h));
                self.compose(rect);
                self.rects += 1;
            }
        }
        self.damage_count = 0;
    }

    /// Собрать прямоугольник экрана и вывести его целиком.
    ///
    /// Прямоугольник выше полосы буфера разрезается на несколько: каждая полоса
    /// собирается со всеми слоями и уходит на экран одним выводом.
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
    /// отсутствия: она превращает картинку в лотерею.
    fn compose(&mut self, rect: Rect) {
        // Буфер вынимается на время сборки — иначе он занят изменяемой
        // ссылкой, пока читаются окна, значки и панель, то есть весь остальной
        // композитор. Тот же приём, что и у самого стола под замком.
        let mut back = core::mem::replace(&mut self.back, Surface::EMPTY);
        let band_h = back.height().max(1);
        let mut top = rect.y;
        while top < rect.bottom() {
            let height = band_h.min((rect.bottom() - top) as u32);
            let band = Rect::new(rect.x, top, rect.w, height);

            // Сборка и вывод меряются **порознь**, и это не любопытство.
            // Медленная сборка и медленный вывод лечатся противоположным:
            // первая — рисующим кодом, второй — числом точек, уезжающих в
            // некэшируемую память панели. Одна общая цифра не различает их
            // вовсе, а различить надо прежде, чем что-то менять.
            let t0 = crate::time::uptime_ns();
            self.compose_band(&mut back, band);
            let t1 = crate::time::uptime_ns();
            // Буфер во всю ширину экрана, поэтому по горизонтали координаты
            // совпадают, и сдвигать надо только начало по вертикали.
            self.screen.blit(
                &back,
                (band.x, band.y),
                Rect::new(band.x, 0, band.w, band.h),
            );
            let t2 = crate::time::uptime_ns();
            // Наружу, в атомики: спрашивают эти числа по кабелю, пока стол
            // занят, а занятый стол вынут из-под замка целиком (см.
            // [`super::timing`]).
            super::note_band(
                t1.wrapping_sub(t0),
                t2.wrapping_sub(t1),
                u64::from(band.w) * u64::from(band.h),
            );

            top += height as i32;
        }
        self.back = back;
    }


    /// Сложить все слои одной полосы в буфер.
    ///
    /// Порядок — снизу вверх, и он же делает обрезку: значок, попавший под
    /// окно, просто перекрывается им в буфере. Пока слои шли прямо на экран,
    /// каждый рисующий обязан был обрезать себя сам по прямоугольнику
    /// изменений — иначе значок, задетый краем, ложился поверх закрывающего его
    /// окна.
    ///
    /// Тень слоя рисуется **перед** самим слоем и по тем же правилам порядка:
    /// тень окна ложится на всё, что ниже него, и не задевает то, что выше.
    fn compose_band(&self, back: &mut Surface, band: Rect) {
        let dy = -band.y;
        let radius = theme::R_WINDOW * self.scale;
        // Разложение сборки по слоям. Общее число уже сказало, что виновата
        // сборка, а не вывод; теперь надо знать, какой её кусок, — иначе
        // правка снова окажется догадкой.
        // Нижние слои пропускаются, когда полосу целиком накрывает окно: они
        // всё равно будут затёрты им же через несколько строк.
        //
        // Это измерено, а не придумано: с «Параметрами» во весь экран обои
        // стоили 4,9 мс, а значки 11,2 мс из 22 мс кадра — и всё это ложилось
        // под окно. Шестнадцать миллисекунд из двадцати двух уходили в мусор.
        //
        // Отсечение сделано **не** тем способом, что когда-то был убран. Тогда
        // короткий путь пропускал вывод самих окон, и порядок их наложения
        // ломался (см. комментарий к [`Self::compose`]). Здесь порядок слоёв не
        // меняется вовсе: окна выводятся ровно как раньше, отменяется только
        // рисование того, что под ними.
        let covered = self.band_covered(band);
        let t_start = crate::time::uptime_ns();
        if !covered {
            self.draw_background(back, band, dy);
        }
        let t_wall = crate::time::uptime_ns();
        // Строка состояния — часть стола и лежит под окнами: она рисуется сразу
        // после обоев, до значков. На настольной машине не рисуется вовсе (см.
        // [`super::statusbar`]).
        if !covered {
            super::statusbar::draw(back, band, dy, self.screen.width(), self.scale);
            self.icons.draw(back, band, dy);
        }
        let t_icons = crate::time::uptime_ns();
        let mut shadow_ns = 0u64;
        for window in self.windows.iter().filter(|window| !window.minimized) {
            let t0 = crate::time::uptime_ns();
            self.drop_shadow(back, window.rect, band, dy, radius);
            shadow_ns += crate::time::uptime_ns().wrapping_sub(t0);
            self.stack(back, window.surface(), window.rect, band, dy, radius);
        }
        let t_windows = crate::time::uptime_ns();
        if let Some(panel) = self.panel.as_ref() {
            // Полосы, которых док не задевает, отсекаются здесь, до всякой
            // работы. `stack` проверяет то же самое, но уже внутри — а нам
            // нужно знать, сколько раз он вызывается вхолостую: счётчик
            // `top` растёт и при тапе в верх экрана, где дока нет вовсе.
            if !panel.rect.intersect(&band).is_empty() {
                super::note_dock_band();
                let t0 = crate::time::uptime_ns();
                self.drop_shadow(back, panel.rect, band, dy, radius);
                self.stack(back, panel.surface(), panel.rect, band, dy, radius);
                // Док считается отдельно от прочих верхних слоёв: он попадает
                // в девять-двенадцать полос, а `top` стоит восемь миллисекунд,
                // и одно с другим не сходится. Пока время не разведено, любое
                // объяснение — догадка.
                super::note_dock_ns(crate::time::uptime_ns().wrapping_sub(t0));
            }
        }
        if let Some(menu) = self.menu.as_ref() {
            if menu.is_open() {
                self.drop_shadow(back, menu.rect, band, dy, radius);
                self.stack(back, menu.surface(), menu.rect, band, dy, radius);
            }
        }
        if let Some(menu) = self.context.as_ref() {
            if menu.is_open() {
                let r = theme::R_CARD * self.scale;
                self.drop_shadow(back, menu.rect, band, dy, r);
                self.stack(back, menu.surface(), menu.rect, band, dy, r);
            }
        }
        // Шторка — поверх всего, включая док: она и открывается затем, чтобы
        // закрыть собой то, что под ней.
        if let Some(shade) = self.shade.as_ref() {
            if shade.is_open() {
                let r = theme::M_R_CARD * self.scale;
                self.drop_shadow(back, shade.rect, band, dy, r);
                self.stack(back, shade.surface(), shade.rect, band, dy, r);
            }
        }
        // Курсор — последним и без проверки пересечения: он мал, а обрезка
        // однобитной картинки уже сделана внутри `draw_bitmap`. Проверка
        // «попадает ли он в полосу» стоила бы больше, чем экономила.
        //
        // На телефоне он не рисуется вовсе. Стрелка показывает, куда попадёт
        // следующее нажатие, — а палец уже стоит там, куда попадёт: он сам
        // виден человеку, и второй указатель на то же место означает, что
        // система показывает ему его собственный палец с опозданием на кадр.
        if !theme::is_mobile() {
            self.pointer.draw(back, dy);
        }
        let t_end = crate::time::uptime_ns();
        super::note_layers(
            t_wall.wrapping_sub(t_start),
            t_icons.wrapping_sub(t_wall),
            t_windows.wrapping_sub(t_icons),
            shadow_ns,
            t_end.wrapping_sub(t_windows),
        );
    }

    /// Фон для телефона: градиент по строкам и разметка, без световых пятен.
    ///
    /// # Почему он другой
    ///
    /// Не потому, что на телефоне некрасиво, а потому, что там он не виден.
    /// Окно на этом экране занимает его целиком, и весь счёт трёх эллипсов
    /// уходит под окно. Сам счёт недёшев: три умножения и до трёх смешиваний на
    /// каждую из миллиона с лишним точек, и всё это на каждом кадре.
    ///
    /// Здесь на точку остаётся одно обращение к таблице. Цвет строки считается
    /// один раз на строку, а разметка ставится там же, где и на столе, — она
    /// нужна и здесь: на однородной заливке движение не читается.
    fn draw_background_flat(&self, back: &mut Surface, rect: Rect, dy: i32) {
        let p = theme::palette();
        let height = self.screen.height().max(1) as i32;

        // Готовые цвета основы: двести пятьдесят шесть оттенков перехода, и
        // цвет точки — это выбор из массива, а не смешивание.
        let mut base = [0u32; 256];
        for (weight, slot) in base.iter_mut().enumerate() {
            *slot = p.wall_top.mix(p.wall_bottom, weight as u8).pixel();
        }

        for y in rect.y.max(0)..rect.bottom() {
            // Вес перехода — доля высоты экрана, посчитанная один раз на строку.
            let weight = (y.clamp(0, height) * 255 / height) as usize;
            let pixel = base[weight.min(255)];
            let target = (y + dy) as u32;
            let from = rect.x.max(0) as usize;
            let to = (rect.right().max(0) as usize).min(back.width() as usize);
            let row = back.row_mut(target);
            if from >= to || row.len() < to {
                continue;
            }
            row[from..to].fill(pixel);
        }

        // Разметка — тем же способом, что у настольного фона: редкие точки,
        // по которым глаз видит, что окно поехало.
        let step = DOT_STEP * self.scale;
        let size = DOT_SIZE * self.scale;
        let mut y = align_up(rect.y, step);
        while y < rect.bottom() {
            let mut x = align_up(rect.x, step);
            while x < rect.right() {
                draw::blend_rect(
                    back,
                    Rect::new(x, y + dy, size, size),
                    p.wall_dot.color,
                    p.wall_dot.alpha,
                );
                x += step as i32;
            }
            y += step as i32;
        }
    }

    /// Мягкая тень под слоем.
    ///
    /// Она не украшение: на светлой теме однопиксельная обводка панели по
    /// светлым обоям не видна вовсе, и без тени панель читается как полоса,
    /// нарисованная прямо на фоне, а не как лежащая на нём плашка.
    fn drop_shadow(&self, back: &mut Surface, placed: Rect, band: Rect, dy: i32, radius: u32) {
        // На телефоне тени нет, и это измерено, а не выбрано на глаз. Кадр там
        // стоил 114 мс, из них 37 мс — тень окна и ещё десятки — тени дока,
        // меню и шторки: расстояние до скруглённого прямоугольника считается
        // на каждую точку рамки в 36 точек по всему периметру слоя, и заново
        // для каждой из полос, на которые режется кадр.
        //
        // Отказаться можно без потери: тень заведена, чтобы плавающая плашка
        // не сливалась со светлыми обоями настольной темы. На телефоне слои
        // занимают экран целиком и лежат на тёмном фоне — отделять нечего.
        if theme::is_mobile() {
            return;
        }
        let spread = SHADOW_SPREAD * self.scale;
        let drop = SHADOW_DROP * self.scale;
        if !Self::touches(placed.translate(0, drop as i32), band, spread) {
            return;
        }
        let ink = theme::palette().shadow;
        let halo = placed.translate(0, drop as i32 + dy);
        draw::shadow(back, halo, radius, spread, ink.color, ink.alpha);
    }

    /// Накрыто ли всё, что в этой полосе, каким-нибудь одним окном.
    ///
    /// Строго: окно должно быть видимым, непрозрачным и покрывать полосу по
    /// обеим сторонам **с запасом на скруглённые углы**. Углы срезает
    /// композитор при выводе ([`Self::stack`]), и в полосе, куда попал угол,
    /// сквозь него видно то, что лежит ниже, — такую полосу накрытой считать
    /// нельзя.
    ///
    /// Достаточно одного окна: два окна, вместе закрывающих полосу, но по
    /// отдельности — нет, здесь не рассматриваются. Случай редкий, а проверка
    /// на объединение прямоугольников стоила бы дороже, чем экономит.
    fn band_covered(&self, band: Rect) -> bool {
        // Радиус берётся полный, а [`Self::stack`] режет углы по
        // `min(radius, w/2, h/2)` — то есть настоящая угловая зона **не больше**
        // этой. Ошибка здесь возможна только в сторону «посчитали полосу
        // незакрытой, хотя она закрыта», и это лишняя отрисовка, а не дырка в
        // картинке. Обратная ошибка стоила бы угла, сквозь который видно
        // мусор, и такой запас брать нельзя.
        let radius = (theme::R_WINDOW * self.scale) as i32;
        self.windows.iter().filter(|window| !window.minimized).any(|window| {
            let rect = window.rect;
            // Полоса целиком внутри окна по горизонтали и по вертикали, и при
            // этом не задевает ни верхний, ни нижний угол.
            rect.x <= band.x
                && rect.right() >= band.right()
                && rect.y + radius <= band.y
                && rect.bottom() - radius >= band.bottom()
        })
    }

    /// Задевает ли слой полосу с учётом того, на сколько его тень выходит наружу.
    fn touches(placed: Rect, band: Rect, margin: u32) -> bool {
        let grown = Rect::new(
            placed.x - margin as i32,
            placed.y - margin as i32,
            placed.w + margin * 2,
            placed.h + margin * 2,
        );
        !grown.intersect(&band).is_empty()
    }

    /// Положить в полосу ту часть слоя, которая в неё попадает, срезав углы.
    ///
    /// Скругление живёт здесь, а не в самом слое, по одной причине: угол — это
    /// место, где сквозь окно видны обои, а обои к этому моменту уже лежат в
    /// буфере. Слой рисует себя прямоугольником и о срезе не знает.
    ///
    /// Строки, проходящие мимо углов, копируются целиком, как и раньше: срез
    /// касается четырёх квадратов со стороной в радиус, то есть восьмисот
    /// точек на окно любого размера.
    fn stack(
        &self,
        back: &mut Surface,
        surface: &Surface,
        placed: Rect,
        band: Rect,
        dy: i32,
        radius: u32,
    ) {
        let overlap = placed.intersect(&band);
        if overlap.is_empty() {
            return;
        }
        let radius = radius.min(placed.w / 2).min(placed.h / 2) as i32;
        for row in 0..overlap.h {
            let screen_y = overlap.y + row as i32;
            let local_y = screen_y - placed.y;
            // Насколько строка углублена в угловую зону сверху или снизу.
            // `None` — строка проходит мимо углов и копируется целиком.
            let corner_y = if local_y < radius {
                Some(radius - local_y)
            } else if local_y >= placed.h as i32 - radius {
                Some(local_y - (placed.h as i32 - radius) + 1)
            } else {
                None
            };
            let Some(depth_y) = corner_y else {
                let src = Rect::new(overlap.x - placed.x, local_y, overlap.w, 1);
                back.blit_from(surface, (overlap.x, screen_y + dy), src);
                continue;
            };
            for column in 0..overlap.w {
                let screen_x = overlap.x + column as i32;
                let local_x = screen_x - placed.x;
                let depth_x = if local_x < radius {
                    radius - local_x
                } else if local_x >= placed.w as i32 - radius {
                    local_x - (placed.w as i32 - radius) + 1
                } else {
                    0
                };
                let pixel = surface.get(local_x as u32, local_y as u32);
                let target = (screen_x as u32, (screen_y + dy) as u32);
                if depth_x == 0 {
                    back.put(target.0, target.1, pixel);
                    continue;
                }
                let coverage = corner_coverage(depth_x, depth_y, radius);
                if coverage == 0 {
                    continue;
                }
                let under = Color::from_pixel(back.get(target.0, target.1));
                let over = Color::from_pixel(pixel);
                back.put(target.0, target.1, under.mix(over, coverage).pixel());
            }
        }
    }

    /// Обои рабочего стола.
    ///
    /// # Почему всё считается, а не хранится картинкой
    ///
    /// Картинка размером с экран — четыре мегабайта при куче в шестнадцать.
    /// Здесь же цвет любой точки — функция от её координат.
    ///
    /// # Из чего они состоят
    ///
    /// Эллиптическое световое пятно у верхнего края, два размытых цветных круга
    /// по противоположным углам и редкая сетка точек. Это не украшение: на
    /// однородной заливке перетаскиваемое окно кажется стоящим на месте, и
    /// глазу нужна опора, чтобы увидеть движение.
    ///
    /// # Почему считается именно так
    ///
    /// Стол перерисовывает фон на каждом переезде окна, а машина под ним —
    /// эмулятор без ускорения, где деление стоит десятки тактов. Первая версия
    /// делала на точку шесть делений и корень, и картинка отставала от мыши на
    /// секунду: окно на снимке стояло там, откуда его уже утащили.
    ///
    /// Поэтому здесь нет ни одного деления в цикле по точкам. Полуоси
    /// превращены в множители один раз, вклад строки посчитан один раз на
    /// строку, корень заменён таблицей на тысячу значений, а цвет основы —
    /// таблицей на двести пятьдесят шесть готовых точек. В цикле остаются
    /// умножение, сдвиг и обращение к таблице.
    fn draw_background(&self, back: &mut Surface, rect: Rect, dy: i32) {
        // На телефоне фон считается дешёвым способом, и это не упрощение ради
        // упрощения. Три световых пятна стоят на точку трёх умножений и до трёх
        // смешиваний; экран 720×1600 — это миллион с лишним точек, а окно на
        // телефоне занимает его целиком, то есть весь этот счёт уходит под
        // окно, где его никто не увидит. Глазом это выглядит как окно,
        // проявляющееся сверху вниз.
        if theme::is_mobile() {
            self.draw_background_flat(back, rect, dy);
            return;
        }
        let p = theme::palette();
        let width = self.screen.width().max(1) as i32;
        let height = self.screen.height().max(1) as i32;

        // Числа повторяют макет: эллипс шириной в три четверти экрана с центром
        // на трети ширины у самого верха, и два круга по противоположным углам.
        let glow = Ellipse::new(width * 3 / 10, 0, width * 3 / 4, height * 7 / 10);
        let cool = Ellipse::new(-width / 12, -height / 8, width * 5 / 8, height * 7 / 10);
        let warm = Ellipse::new(
            width + width / 8,
            height + height / 6,
            width * 5 / 7,
            height * 3 / 4,
        );
        let cool_ink = Color::rgb(0x5A, 0xA2, 0xFF);
        let warm_ink = Color::rgb(0xC7, 0x7D, 0xFF);
        // Цветные круги на светлой теме приглушены вчетверо: то, что на тёмном
        // фоне читается как подсветка, на светлом становится пятном краски.
        let tint = if theme::is_dark() { 1 } else { 4 };
        let cool_peak = 92 / tint;
        let warm_peak = 74 / tint;

        // Таблица перехода: квадрат расстояния → вес смешивания. Корень берётся
        // тысячу раз вместо миллиона.
        let mut ramp = [0u8; RAMP + 1];
        let full = UNIT * 65 / 100;
        for (index, slot) in ramp.iter_mut().enumerate() {
            let d2 = (index as i64) * UNIT / (RAMP as i64);
            let distance = isqrt((d2 * UNIT) as u64) as i64;
            *slot = (distance * 255 / full).min(255) as u8;
        }
        // Готовые точки основы: их всего двести пятьдесят шесть, и без круга
        // над ними цвет точки — это одно обращение к массиву.
        let mut base = [0u32; 256];
        for (weight, slot) in base.iter_mut().enumerate() {
            *slot = p.wall_top.mix(p.wall_bottom, weight as u8).pixel();
        }

        for y in rect.y.max(0)..rect.bottom() {
            let glow_row = glow.row(y);
            let cool_row = cool.row(y);
            let warm_row = warm.row(y);
            // Строка целиком вне круга отсекается один раз, а не на каждой её
            // точке.
            let cool_here = cool_row < UNIT && cool_peak > 0;
            let warm_here = warm_row < UNIT && warm_peak > 0;
            let target = (y + dy) as u32;
            let from = rect.x.max(0) as usize;
            let to = (rect.right().max(0) as usize).min(back.width() as usize);
            let row = back.row_mut(target);
            if from >= to || row.len() < to {
                continue;
            }
            for (offset, slot) in row[from..to].iter_mut().enumerate() {
                let x = from as i32 + offset as i32;
                let d2 = glow.at(x, glow_row).clamp(0, UNIT);
                let weight = ramp[(d2 * (RAMP as i64) / UNIT) as usize];
                let mut pixel = base[weight as usize];
                if cool_here {
                    let blue = cool.glow(x, cool_row, cool_peak);
                    if blue > 0 {
                        pixel = Color::from_pixel(pixel).mix(cool_ink, blue).pixel();
                    }
                }
                if warm_here {
                    let violet = warm.glow(x, warm_row, warm_peak);
                    if violet > 0 {
                        pixel = Color::from_pixel(pixel).mix(warm_ink, violet).pixel();
                    }
                }
                *slot = pixel;
            }
        }

        // Разметка: редкая сетка точек. Вторая опора для глаза — по ней видно,
        // что окно едет, даже когда оно едет по однотонному месту.
        let step = DOT_STEP * self.scale;
        let size = DOT_SIZE * self.scale;
        let mut y = align_up(rect.y, step);
        while y < rect.bottom() {
            let mut x = align_up(rect.x, step);
            while x < rect.right() {
                draw::blend_rect(
                    back,
                    Rect::new(x, y + dy, size, size),
                    p.wall_dot.color,
                    p.wall_dot.alpha,
                );
                x += step as i32;
            }
            y += step as i32;
        }
    }

    /// Сколько кадров собрано, сколько прямоугольников выведено, сколько окон.
    #[must_use]
    pub fn stats(&self) -> (u64, u64, usize) {
        (self.frames, self.rects, self.windows.len())
    }
}

/// Доля экрана (0..65535) → точка на нём.
///
/// Считается по **последней** точке, а не по размеру: доля 65535 обязана давать
/// правый край, а не первую точку за ним. Ошибка на единицу здесь означала бы
/// недостижимый последний столбец экрана — то, что человек замечает первым,
/// когда не может попасть в кнопку закрытия развёрнутого окна.
fn scale_fraction(fraction: u16, extent: u32) -> i32 {
    let last = extent.saturating_sub(1);
    ((u64::from(fraction) * u64::from(last)) / u64::from(u16::MAX)) as i32
}

/// Ближайшая сверху координата, кратная шагу разметки.
fn align_up(value: i32, step: u32) -> i32 {
    let step = step as i32;
    // Округление к большему работает и для отрицательных: `div_euclid`
    // округляет вниз в математическом смысле, а не в сторону нуля.
    (value + step - 1).div_euclid(step) * step
}


/// Эллипс, по которому считается пятно на обоях.
///
/// Полуоси хранятся не длинами, а множителями: вклад точки в нормированное
/// расстояние — это `d² · k`, и деление, которое иначе пришлось бы делать на
/// каждую точку кадра, сделано здесь один раз.
struct Ellipse {
    cx: i32,
    cy: i32,
    kx: i64,
    ky: i64,
}

/// Единица нормированного расстояния в неподвижной точке.
const UNIT: i64 = 4096;

/// Сколько знаков после запятой хранит множитель полуоси.
///
/// Двадцать: при полуоси в тысячу точек множитель получается около пяти
/// миллионов, и произведение с квадратом расстояния остаётся далеко внутри
/// шестидесяти четырёх бит, а точность нормированного расстояния — доли
/// единицы из четырёх тысяч.
const AXIS_BITS: u32 = 20;

/// Сколько ступеней в таблице перехода.
const RAMP: usize = 1024;

impl Ellipse {
    fn new(cx: i32, cy: i32, a: i32, b: i32) -> Self {
        let a = i64::from(a.max(1));
        let b = i64::from(b.max(1));
        Self {
            cx,
            cy,
            kx: (UNIT << AXIS_BITS) / (a * a),
            ky: (UNIT << AXIS_BITS) / (b * b),
        }
    }

    /// Вклад строки в квадрат нормированного расстояния.
    fn row(&self, y: i32) -> i64 {
        let dy = i64::from(y - self.cy);
        (dy * dy * self.ky) >> AXIS_BITS
    }

    /// Квадрат нормированного расстояния до центра: [`UNIT`] на самом эллипсе.
    fn at(&self, x: i32, row: i64) -> i64 {
        let dx = i64::from(x - self.cx);
        row + ((dx * dx * self.kx) >> AXIS_BITS)
    }

    /// Насыщенность цветного круга в этой точке, 0 за его краем.
    fn glow(&self, x: i32, row: i64, peak: i64) -> u8 {
        let d2 = self.at(x, row);
        if d2 >= UNIT {
            return 0;
        }
        let fade = UNIT - d2;
        (peak * fade / UNIT * fade / UNIT).clamp(0, 255) as u8
    }
}

/// Доля точки, накрытая скруглённым углом.
///
/// `depth_x` и `depth_y` — на сколько точка углублена в угловой квадрат,
/// считая от его внешнего края внутрь. Центр дуги при этом стоит в точке
/// `(radius, radius)` того же квадрата, и задача сводится к расстоянию до него.
fn corner_coverage(depth_x: i32, depth_y: i32, radius: i32) -> u8 {
    if radius <= 0 {
        return 255;
    }
    // Расстояние считается до центра точки, поэтому половина точки прибавлена
    // сразу: без неё край дуги смещается на полточки внутрь, и скругление у
    // соседних окон выглядит разной толщины.
    let dx = i64::from(depth_x) * 256 - 128;
    let dy = i64::from(depth_y) * 256 - 128;
    let distance = isqrt((dx * dx + dy * dy) as u64) as i64;
    let edge = i64::from(radius) * 256;
    if distance + 128 <= edge {
        return 255;
    }
    if distance >= edge + 128 {
        return 0;
    }
    ((edge + 128 - distance) * 255 / 256).clamp(0, 255) as u8
}

/// Целочисленный квадратный корень.
///
/// Свой, а не из ядра языка: плавающую точку в ядре трогать нельзя — на AArch64
/// её регистры в обработчике прерывания не сохраняются, и одно умножение на
/// `f32` посреди отрисовки портит состояние прерванной задачи.
fn isqrt(value: u64) -> u64 {
    if value < 2 {
        return value;
    }
    let mut guess = 1u64 << ((65 - value.leading_zeros()) / 2);
    loop {
        let next = (guess + value / guess) / 2;
        if next >= guess {
            return guess;
        }
        guess = next;
    }
}
