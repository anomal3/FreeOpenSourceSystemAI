//! Окно: поверхность вместе с заголовком и содержимым.
//!
//! Украшения нарисованы в той же поверхности, что и содержимое, поэтому вывод
//! окна на экран — одна операция, а не три. Плата — перерисовка заголовка при
//! смене активного окна, то есть несколько тысяч пикселей в обычной памяти,
//! которую, в отличие от экрана, можно и читать, и переписывать сколько угодно.
//!
//! # Почему у окна нет рамки
//!
//! Рамка в две точки была единственным способом отделить окно от фона, пока
//! фон и окно отличались только яркостью заливки. Теперь окно отделяет
//! однопиксельная обводка изнутри и светлая кромка по верхнему краю — то же,
//! чем отделяет себя лист бумаги, лежащий на столе. Толстая рамка при этом
//! съедала по четыре точки с каждой стороны и делала маленькое окно ещё меньше.
//!
//! # Почему скругление рисует не окно, а композитор
//!
//! Поверхность окна прямоугольна, и прозрачности в ней нет. Скруглённый угол —
//! это место, где сквозь окно видны обои, а обои знает только композитор.
//! Поэтому окно рисует себя во всю поверхность, а углы срезаются при выводе,
//! смешиванием с уже нарисованным фоном.
//!
//! # Почему высота заголовка не зависит от фокуса
//!
//! В макете у неактивного окна полоса ниже. Сделать так значило бы, что при
//! каждом переключении окон область содержимого меняет размер, а список файлов
//! и «Параметры» пересобирают раскладку. Неактивное окно отличается тем, что
//! теряет градиент, кромку и заливку кнопок, — этого достаточно, и это стоит
//! одной полосы, а не всего окна.

use mini_ui::draw;
use mini_ui::glyphicon::Icon;
use mini_ui::text::TextGrid;
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface};

use super::files::FilesView;
use super::paint::{self, Ctx, Weight};
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

    /// Как окно называется для человека.
    ///
    /// Отдельно от [`App::title`], а не вместо него: `title` попадает в журнал,
    /// по нему стенд находит окно на экране и по нему же сверяет строки прогона.
    /// Журнал — инструмент разработчика и остаётся на латинице; заголовок окна
    /// читает человек, и он на его языке.
    #[must_use]
    pub const fn caption(self) -> &'static str {
        match self {
            App::Terminal => "Терминал",
            App::System => "Системный монитор",
            App::Files => "Файлы",
            App::About => "О системе",
            App::Settings => "Параметры",
            App::Shutdown => "Выключение",
            App::Restart => "Перезагрузка",
        }
    }

    /// Каким цветом горит значок программы.
    ///
    /// Цвет опознаёт окно раньше, чем прочитан заголовок: зелёный — терминал,
    /// красный — вопрос о выключении, синий — всё остальное.
    #[must_use]
    pub const fn tone(self) -> super::paint::Tone {
        match self {
            App::Terminal => super::paint::Tone::Ok,
            App::Shutdown | App::Restart => super::paint::Tone::Bad,
            _ => super::paint::Tone::Accent,
        }
    }

    /// Значок программы: на столе, в меню запуска и в заголовке окна.
    #[must_use]
    pub const fn icon(self) -> Icon {
        match self {
            App::Terminal => Icon::Terminal,
            App::System => Icon::Chart,
            App::Files => Icon::Folder,
            App::About => Icon::Info,
            App::Settings => Icon::Settings,
            App::Shutdown | App::Restart => Icon::Power,
        }
    }

    /// Имя окна в журнале и в прицеле стенда.
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
            App::Terminal => "оболочка и команды ядра",
            App::Files => "обзор смонтированного корня",
            App::System => "память, задачи, счётчики ввода",
            App::About => "что это за система",
            App::Settings => "экран, программы, обновление",
            App::Shutdown => "закрыть том и выключить",
            App::Restart => "закрыть том и запустить снова",
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
    /// Множитель геометрии стола: 1 на обычном экране, 2 на очень плотном.
    ///
    /// Из него же выводится размерный ряд шрифта — порог у них общий, и хранить
    /// два числа значило бы однажды нарисовать кнопку одного ряда шрифтом
    /// другого.
    scale: u32,
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
///
/// Больше прежних шестнадцати: у скруглённого окна сам угол срезан, и попасть
/// в квадрат, половина которого приходится на срез, мышью нельзя.
const GRIP: u32 = 20;

impl Window {
    /// Создать текстовое окно.
    ///
    /// `None`, если не хватило памяти под поверхность или окно слишком мало,
    /// чтобы вместить хоть одну ячейку текста. Паниковать здесь нельзя: окно —
    /// это несколько мегабайт, отказ выделения совершенно реален, и система
    /// обязана продолжить работу без этого окна.
    #[must_use]
    pub fn text(app: App, rect: Rect, scale: u32) -> Option<Self> {
        let ctx = Ctx::scaled(scale);
        let back = theme::window_bg();
        let surface = Surface::new(rect.w, rect.h, back)?;
        let grid = TextGrid::new(
            content_area(&surface, scale).shrink(ctx.px(4)),
            ctx.face(Role::Mono),
            ctx.palette.ink2,
            back,
        )?;
        let mut window = Self::wrap(app, rect, surface, Content::Text(grid), scale);
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно параметров.
    #[must_use]
    pub fn settings(rect: Rect, scale: u32, screen: (u32, u32)) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Settings(SettingsView::new(screen));
        let mut window = Self::wrap(App::Settings, rect, surface, content, scale);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    /// Создать окно файлового менеджера.
    #[must_use]
    pub fn files(rect: Rect, scale: u32) -> Option<Self> {
        let surface = Surface::new(rect.w, rect.h, theme::window_bg())?;
        let content = Content::Files(FilesView::new());
        let mut window = Self::wrap(App::Files, rect, surface, content, scale);
        window.redraw_content();
        window.draw_decorations(false);
        Some(window)
    }

    fn wrap(app: App, rect: Rect, surface: Surface, content: Content, scale: u32) -> Self {
        Self {
            app,
            rect,
            surface,
            content,
            scale,
            minimized: false,
            restore: None,
            damage: Rect::EMPTY,
        }
    }

    /// Высота полосы заголовка при заданном масштабе.
    #[must_use]
    pub const fn title_height(scale: u32) -> u32 {
        theme::TITLE_H * scale
    }

    /// Контекст отрисовки этого окна.
    fn ctx(&self) -> Ctx {
        Ctx::scaled(self.scale)
    }

    /// Кнопки заголовка справа налево: закрыть, развернуть, свернуть.
    ///
    /// Порядок как у окон, к которым человек привык: крестик крайний справа,
    /// потому что промахнуться мимо него — это закрыть окно, а не свернуть.
    fn title_button(&self, from_right: u32) -> Rect {
        let scale = self.scale;
        let size = theme::TITLE_BTN * scale;
        let gap = theme::TITLE_GAP * scale;
        let margin = 10 * scale;
        let step = size + gap;
        let right = self.surface.width() as i32 - margin as i32;
        Rect::new(
            right - (size + step * from_right) as i32,
            (Self::title_height(scale) as i32 - size as i32) / 2,
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
        let title_bottom = Self::title_height(self.scale) as i32;
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

    /// Нарисовать заголовок, кнопки и обводку окна.
    ///
    /// Полоса заголовка — градиент сверху вниз, поверх него светлая кромка в
    /// одну точку и линия под полосой. Три полосы вместо одной заливки: без них
    /// заголовок и содержимое отличаются на две единицы яркости, и граница
    /// между ними не читается вовсе.
    pub fn draw_decorations(&mut self, focused: bool) {
        let ctx = self.ctx();
        let p = ctx.palette;
        let scale = self.scale;
        let title_h = Self::title_height(scale);
        let bounds = self.surface.bounds();
        let radius = ctx.px(theme::R_WINDOW);
        let bar = Rect::new(0, 0, bounds.w, title_h);

        // Полоса — прямоугольник без скругления, и это не упущение: верхние
        // углы окна срезает композитор при выводе, потому что за ними видны
        // обои, а он один их знает. Скруглить полосу здесь значило бы срезать
        // угол дважды, а её нижние углы — ещё и там, где под ней лежит не фон,
        // а содержимое.
        //
        // Первая версия обходила это полосой на `radius` выше нужного, и она
        // молча закрашивала четырнадцать верхних точек содержимого: у
        // терминала верхняя строка оказывалась срезанной пополам.
        let top = if focused { p.tb1 } else { p.tb2 };
        let bottom = p.tb2;
        draw::rounded_gradient(
            &mut self.surface,
            bar,
            0,
            ctx.flat(top),
            ctx.flat(bottom),
            255,
        );
        draw::hline(
            &mut self.surface,
            0,
            title_h as i32 - 1,
            bounds.w,
            p.tbline.color,
            p.tbline.alpha,
        );
        if focused {
            draw::crown(&mut self.surface, bounds, radius, p.crown.color, p.crown.alpha);
        }

        // Значок программы: тот же скруглённый квадрат, что у неё на столе и в
        // меню. Он и опознаёт окно быстрее заголовка — цвет читается раньше
        // текста.
        let badge_side = ctx.px(18);
        let badge = Rect::new(
            ctx.px(16) as i32,
            (title_h as i32 - badge_side as i32) / 2,
            badge_side,
            badge_side,
        );
        let tone = if focused { self.app.tone() } else { paint::Tone::Muted };
        paint::badge(ctx, &mut self.surface, badge, tone);

        // Кнопки нарисованы всегда, а не только у активного окна: кнопка,
        // появляющаяся при наведении, потребовала бы следить за указателем и
        // перерисовывать заголовок на каждое его движение.
        //
        // Знак «развернуть» меняется вместе с состоянием: развёрнутое окно
        // предлагает вернуть прежний размер, и одинаковый значок в обоих
        // случаях означал бы, что человек нажимает наугад.
        let restore_icon = if self.restore.is_some() { Icon::Restore } else { Icon::Maximize };
        let weight = if focused { Weight::Normal } else { Weight::Ghost };
        for (button, icon, weight) in [
            (self.minimize_button(), Icon::Minimize, weight),
            (self.maximize_button(), restore_icon, weight),
            (
                self.close_button(),
                Icon::Close,
                if focused { Weight::Danger } else { Weight::Ghost },
            ),
        ] {
            paint::icon_button(ctx, &mut self.surface, button, icon, weight, false);
        }

        // Заголовок кончается там, где начинаются кнопки: подпись, заехавшая
        // под крестик, читается как ошибка отрисовки.
        let text_x = badge.right() + ctx.px(11) as i32;
        let room = (self.minimize_button().x - ctx.px(12) as i32 - text_x).max(0) as u32;
        let ink = if focused { p.ink } else { p.ink4 };
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::Title,
            text_x,
            paint::baseline(ctx, Role::Title, bar),
            room,
            self.app.caption(),
            ink,
        );
        // Обводка окна — последней: она ложится поверх и полосы заголовка, и
        // содержимого, и именно она делает из двух прямоугольников один лист.
        let edge = if focused { p.line3 } else { p.line2 };
        draw::rounded_stroke(&mut self.surface, bounds, radius, edge.color, edge.alpha);

        // Уголок размера: три точки в правом нижнем углу. Без нарисованного
        // признака за него никто не потянет — угадывать, что окно где-то
        // тянется, человек не обязан.
        //
        // Уголок рисуется, но **не** помечается изменившимся: он не зависит ни
        // от фокуса, ни от содержимого. Пометить его вместе с заголовком стоило
        // бы всей площади окна — прямоугольник изменений один, и объединение
        // верхней полосы с нижним углом накрывает окно целиком. На 1920×1080
        // это превращало переключение окон в перерисовку всего экрана: клавиша
        // Tab обрабатывалась дольше пяти секунд.
        let grip = self.resize_grip();
        let dot = ctx.px(2).max(1);
        for (dx, dy) in [(0u32, 0u32), (1, 0), (0, 1)] {
            let step = ctx.px(5) as i32;
            draw::circle(
                &mut self.surface,
                grip.right() - ctx.px(6) as i32 - dx as i32 * step,
                grip.bottom() - ctx.px(6) as i32 - dy as i32 * step,
                dot,
                p.ink6,
                255,
            );
        }

        // Изменилась только полоса сверху — её и помечаем. Пометить всё окно
        // было бы проще на одну строку и дороже на площадь окна при каждом
        // переключении фокуса.
        self.damage = self.damage.union(&Rect::new(0, 0, bounds.w, title_h));
        // Обводка идёт по всему периметру, и без её краёв неактивное окно
        // осталось бы с яркой рамкой активного. Три полосы в одну точку стоят
        // ничтожно мало по сравнению с площадью окна.
        self.damage = self.damage.union(&Rect::new(0, bounds.h as i32 - 1, bounds.w, 1));
        self.damage = self.damage.union(&Rect::new(0, 0, 1, bounds.h));
        self.damage = self.damage.union(&Rect::new(bounds.w as i32 - 1, 0, 1, bounds.h));
    }

    /// Перекрасить окно под текущую тему.
    ///
    /// Заголовок перерисует тот, кто знает, активно ли окно, — здесь только
    /// содержимое. Разделение не косметическое: фокус живёт у композитора, и
    /// окно, взявшееся угадывать его само, ошибётся на первом же переключении.
    pub fn restyle(&mut self) {
        let ctx = self.ctx();
        let back = theme::window_bg();
        match &mut self.content {
            Content::Text(grid) => {
                // Заливается вся область содержимого, а не только сетка: в
                // высоту окна редко помещается целое число строк, и остаток
                // внизу — несколько точек, а то и полтора десятка — иначе
                // остаётся полосой прежней темы под самой рамкой.
                let area = content_area(&self.surface, self.scale);
                self.surface.fill(area, back);
                grid.recolor(&mut self.surface, ctx.palette.ink2, back);
                grid.take_damage();
                self.damage = self.damage.union(&area);
            }
            Content::Files(_) | Content::Settings(_) => self.redraw_content(),
        }
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
        let area = content_area(&self.surface, self.scale);
        let ctx = self.ctx();
        // Содержимое рисуется поверх прежнего, и заливка обязательна: список,
        // ставший короче, иначе оставил бы под собой хвост предыдущего.
        self.surface.fill(area, theme::window_bg());
        match &self.content {
            Content::Files(view) => view.draw(&mut self.surface, area, ctx),
            Content::Settings(view) => view.draw(&mut self.surface, area, ctx),
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
        let area = content_area(&self.surface, self.scale);
        let ctx = self.ctx();
        let local = (x - self.rect.x, y - self.rect.y);
        let used = match &mut self.content {
            Content::Settings(view) => view.click(area, ctx, local.0, local.1),
            Content::Files(view) => view.click(area, ctx, local.0, local.1),
            Content::Text(_) => false,
        };
        if used {
            self.redraw_content();
        }
        used
    }

    /// Сменило ли содержимое тему с прошлого вопроса.
    ///
    /// Признак снимается чтением: спросить обязан ровно один — тот, кто умеет
    /// перекрасить весь стол, — и второй утвердительный ответ означал бы вторую
    /// перерисовку экрана на ровном месте.
    pub fn took_theme_change(&mut self) -> bool {
        match &mut self.content {
            Content::Settings(view) => view.take_theme_change(),
            Content::Files(_) | Content::Text(_) => false,
        }
    }

    /// Сдвинуть окно, оставив заголовок на экране.
    ///
    /// Полностью уехавшее окно нельзя ни вернуть, ни закрыть мышью, которой на
    /// этой фазе нет, — поэтому часть заголовка обязана остаться видимой.
    pub fn move_within(&mut self, dx: i32, dy: i32, screen_w: u32, bottom_limit: i32) {
        let keep = Self::title_height(self.scale) as i32 * 3;
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
        let Some(mut surface) = Surface::new(w, h, theme::window_bg()) else {
            return false;
        };
        let inner = content_area(&surface, self.scale).shrink(self.ctx().px(4));
        match &mut self.content {
            Content::Text(grid) => {
                if !grid.rebind(&mut surface, inner) {
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

    /// Поверхность окна — для сборки кадра.
    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }
}

/// Область поверхности под содержимое: всё, что ниже полосы заголовка.
///
/// Поля не отнимаются: их назначает само содержимое, и они у списка файлов и у
/// «Параметров» разные. Отнимать их здесь значило бы, что боковая колонка,
/// которая обязана доходить до края окна, до него не доходит.
fn content_area(surface: &Surface, scale: u32) -> Rect {
    let top = Window::title_height(scale);
    Rect::new(
        0,
        top as i32,
        surface.width(),
        surface.height().saturating_sub(top),
    )
}
