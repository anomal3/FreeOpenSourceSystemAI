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
//! # Почему панель плавает, а не приклеена к нижнему краю
//!
//! Приклеенная полоса делит экран пополам линией во всю ширину, и эта линия —
//! самая заметная деталь стола, хотя не рассказывает ровно ничего. У плашки с
//! полями по краям такой линии нет: у неё есть форма, и глаз читает её как
//! предмет, лежащий на обоях, а не как границу экрана.
//!
//! # Почему углы и тень рисует не панель, а композитор
//!
//! Поверхность панели прямоугольна, и прозрачности в ней нет. Скруглённый угол
//! — это место, где сквозь плашку видны обои, а обои знает только композитор:
//! это уже не вертикальный градиент, а световое пятно с цветными кругами, и
//! повторить его у себя нельзя — прямоугольник чуть другого оттенка виден
//! сразу. Поэтому панель рисует себя во всю поверхность, а углы срезаются при
//! выводе; тем же выводом кладётся и тень под плашкой. Всё, что остаётся
//! панели, — заливка, сведённая к непрозрачному цвету поверх средних обоев.
//!
//! # Часы показывают время работы, а не время суток
//!
//! Часов реального времени ядро не читает: драйвера RTC нет, а прошивка своё
//! время после `ExitBootServices` больше не отдаёт. Показывать выдуманное время
//! суток нельзя — это ровно тот случай, когда интерфейс врёт; поэтому, пока
//! настоящего времени нет, последним справа стоит время с момента загрузки, и
//! подписано оно `up`.

use alloc::string::String;
use alloc::vec::Vec;

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::theme::{self, Palette};
use super::window::App;

/// Надпись на кнопке меню.
const BRAND: &str = "FreeOS";

/// Заголовок колонки меню.
const APPS_TITLE: &str = "ПРИЛОЖЕНИЯ";

/// Белый — цвет надписи на акценте.
///
/// В палитре его нет намеренно: белый на акценте одинаков в обеих темах, и
/// токен на него завёл бы вопрос «а какой белый в светлой».
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

/// Контекст стола: панель и меню лежат на обоях, а не в окне.
///
/// Подложка — стекло поверх обоев: плитка значка и подложка выбранной строки
/// сводятся к ней, и возьми они подложку окна, строка меню оказалась бы светлее
/// карточки, на которой стоит.
fn desk_ctx(scale: u32) -> Ctx {
    Ctx::scaled(scale).on(glass_bg())
}

/// Надпись в поле дока. Она же говорит, что поле делает: открывает список
/// приложений и ищет по нему.
const SEARCH_HINT: &str = "Поиск";

/// Подпись кнопки «Пуск» на телефоне. Имя системы, а не слово «Пуск»: кнопка
/// широкая, места хватает, и человек читает, чем он пользуется.
const DOCK_BRAND: &str = "FreeOS";

/// Заливка плавающего слоя, сведённая к непрозрачному цвету.
///
/// Обои берутся усреднёнными, а не в точке под плашкой: под ней их не один
/// оттенок, а световое пятно, и «настоящий» цвет пришлось бы считать заново на
/// каждой перерисовке — ради разницы, которой не видно под непрозрачным
/// стеклом.
fn glass_bg() -> Color {
    let p = theme::palette();
    p.glass.over(theme::wall_average(p))
}

/// Размеры, которыми пользуются и панель, и меню.
///
/// Собраны в одном месте не ради краткости: раскладку считают две стороны —
/// рисующая и разбирающая щелчок, — и любое число, посчитанное в каждой из них
/// отдельно, однажды разойдётся.
#[derive(Clone, Copy)]
struct Metrics {
    ctx: Ctx,
    /// Отступ плавающего слоя от краёв экрана.
    inset: u32,
    /// Скругление плашки и карточки.
    round: u32,
    /// Скругление кнопки и строки списка.
    round_row: u32,
    /// Высота кнопки на панели.
    btn_h: u32,
    /// Отступ содержимого от края кнопки.
    side: u32,
    /// Обычный зазор между соседями.
    gap: u32,
    /// Высота строки меню и шаг между строками вместе с зазором.
    row_h: u32,
    pitch: u32,
    /// Сторона плитки значка в строке меню.
    tile: u32,
    /// Поле строки меню слева и справа.
    row_pad: u32,
    /// Поле карточки меню.
    pad: u32,
}

impl Metrics {
    fn new(ctx: Ctx) -> Self {
        Self {
            ctx,
            inset: ctx.px(theme::PANEL_INSET),
            // Док у телефона скруглён сильнее плашки: он лежит у самого края
            // экрана, а экран там сам скруглён по радиусу вчетверо большему.
            // Прямой угол рядом со скруглённым краем читается как обрезанный.
            round: ctx.px(if theme::is_mobile() { theme::M_R_DOCK } else { theme::R_WINDOW }),
            // Скругление кнопки внутри дока — тоже крупнее: кнопка 48 точек со
            // скруглением 10 выглядит квадратом с обточенными углами, а в
            // макете это почти круг.
            round_row: ctx.px(if theme::is_mobile() { theme::M_R_TILE } else { theme::R_ROW }),
            btn_h: ctx.px(if theme::is_mobile() {
                theme::M_DOCK_BTN
            } else {
                theme::PANEL_BTN_H
            }),
            side: ctx.px(15),
            gap: ctx.px(8),
            // Строка «Пуска» на телефоне выше: в неё попадают пальцем, и
            // тридцать шесть точек — это половина подушечки.
            row_h: ctx.px(if theme::is_mobile() {
                theme::M_MENU_ROW_H
            } else {
                theme::MENU_ROW_H
            }),
            pitch: ctx.px(if theme::is_mobile() {
                theme::M_MENU_ROW_H
            } else {
                theme::MENU_ROW_H
            }) + ctx.px(if theme::is_mobile() { 6 } else { 2 }),
            tile: ctx.px(20),
            row_pad: ctx.px(8),
            pad: ctx.px(10),
        }
    }

    /// Поле плашки: столько остаётся сверху и снизу от кнопки высотой
    /// [`theme::PANEL_BTN_H`] в полосе высотой [`theme::PANEL_H`]. Оно же берётся
    /// слева и справа — иначе кнопка стояла бы в плашке не по центру, а
    /// «примерно по центру».
    const fn plate_pad(self) -> u32 {
        (self.ctx.px(theme::PANEL_H) - self.btn_h) / 2
    }

    /// Сторона значка внутри кнопки панели.
    const fn glyph(self) -> u32 {
        self.ctx.px(14)
    }

    /// Кружок состояния окна.
    const fn dot(self) -> u32 {
        self.ctx.px(8)
    }

    /// Ширина кнопки меню — она же начало всего, что стоит правее.
    fn brand_width(self) -> u32 {
        self.side * 2 + self.glyph() + self.gap + self.ctx.face(Role::Title).width(BRAND)
    }

    /// Ширина кнопки окна с полной подписью.
    ///
    /// Подпись приходит строкой, а не выводится из [`App`]: у окна программы
    /// имя своё, и спросить его у перечисления нечем.
    fn button_width(self, caption: &str, state: ButtonState) -> u32 {
        self.side * 2 + self.dot() + self.gap + self.ctx.face(state.role()).width(caption)
    }

    /// Сторона плитки в шапке меню.
    const fn head(self) -> u32 {
        self.ctx.px(34)
    }

    /// Высота строки заголовка столбца.
    const fn caps(self) -> u32 {
        self.ctx.px(16)
    }

    /// Высота всего, что стоит в карточке меню выше первой строки.
    fn head_height(self) -> u32 {
        self.pad + self.head() + self.gap + self.caps()
    }

    /// Высота черты перед питанием вместе с полями сверху и снизу.
    const fn rule_height(self) -> u32 {
        self.ctx.px(6) * 2 + 1
    }

    /// Отступ подписи от плитки в строке меню.
    const fn label_gap(self) -> u32 {
        self.ctx.px(10)
    }

    /// Ширина строки меню с подписью такой ширины.
    const fn row_width(self, text: u32) -> u32 {
        self.row_pad * 2 + self.tile + self.label_gap() + text
    }

    /// Ширина шапки меню: плитка, зазор и самая длинная из двух строк.
    fn header_width(self) -> u32 {
        let name = self.ctx.face(Role::Title).width(BRAND);
        let note = self.ctx.face(Role::MonoSmall).width(crate::VERSION);
        self.head() + self.gap + name.max(note)
    }
}

/// Как выглядит кнопка окна.
///
/// Перечисление, а не два признака: «активно» и «свёрнуто» вместе дают четыре
/// сочетания, из которых существуют три, а четвёртое — активное свёрнутое окно
/// — бессмысленно. Свести их к одному состоянию здесь дешевле, чем ловить это
/// сочетание в каждой из трёх строк, где выбирается цвет.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ButtonState {
    /// Окно на экране и в фокусе.
    Active,
    /// Окно на экране, но не в фокусе.
    Idle,
    /// Окно свёрнуто: его нет на экране, но оно есть в панели.
    Minimized,
}

impl ButtonState {
    /// Свёрнутое окно фокуса не имеет: пока оно свёрнуто, ввод достаётся не
    /// ему, и подсвечивать его как активное значило бы врать о том, куда
    /// уходят нажатия.
    const fn of(focused: bool, minimized: bool) -> Self {
        if minimized {
            Self::Minimized
        } else if focused {
            Self::Active
        } else {
            Self::Idle
        }
    }

    /// Активное окно набрано плотнее прочих.
    const fn role(self) -> Role {
        match self {
            Self::Active => Role::Label,
            _ => Role::Body,
        }
    }

    /// Цвет подписи.
    const fn ink(self, p: &Palette) -> Color {
        match self {
            Self::Active => p.ink2,
            Self::Idle => p.ink3,
            Self::Minimized => p.ink5,
        }
    }

    /// Цвет кружка состояния.
    const fn dot(self, p: &Palette) -> Color {
        match self {
            Self::Active => p.ok,
            _ => p.ink6,
        }
    }
}

/// Что панель показывает справа — в трее.
///
/// Трей, а не счётчики: до фазы С2 справа стояли свободная память и время
/// работы, и Роман спросил, зачем они там. Человеку с Windows справа нужны
/// язык, сеть и часы — то, что он проверяет взглядом десять раз в день. Память
/// осталась в «О системе» и в разделе «Система» Параметров.
pub struct Status {
    /// Местное время `ЧЧ:ММ`, если система его знает.
    ///
    /// Готовой строкой, а не числом: панель рисует то, что ей дали, и знать о
    /// часовых поясах ей незачем. Без часов трей показывает время работы —
    /// хоть какое-то время лучше пустого места.
    pub clock: Option<String>,
    pub uptime_ms: u64,
    /// Есть ли сеть. Считается снаружи, до захвата замка стола: у сети свой
    /// замок, и брать его под замком стола нельзя — см. [`super::status_now`].
    pub net: NetState,
}

/// Состояние сети для значка в трее.
///
/// Раскладка сюда не входит намеренно: её панель спрашивает сама у
/// [`crate::input::keymap::layout`] — это атомик без замка, и панель, которую
/// перерисовали сразу после сочетания клавиш, показывает новую раскладку, а не
/// ту, что была в момент подсчёта `Status`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NetState {
    /// Сетевой карты нет.
    Absent,
    /// Карта есть, адреса нет.
    NoAddress,
    /// Адрес назначен.
    Up,
}

/// Значок трея, в который попал указатель.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayItem {
    Network,
    Layout,
    Clock,
}

/// Значки трея вместе с их местом на плашке.
type Tray = Vec<(TrayItem, Rect)>;

/// Панель задач: плавающая плашка вдоль нижнего края экрана.
pub struct Panel {
    surface: Surface,
    /// Место плашки на экране: отступ 14 от левого, правого и нижнего краёв.
    ///
    /// Поверхность — ровно она, без запаса: тень под плашкой и срез углов
    /// делает композитор, и лишняя точка поверхности стала бы прямоугольником
    /// чужого оттенка поверх обоев.
    pub rect: Rect,
    /// Множитель геометрии стола.
    scale: u32,
    damage: Rect,
    /// Кнопка меню в координатах поверхности.
    brand: Rect,
    /// Кнопки окон там же.
    ///
    /// Запоминаются при рисовании, а не считаются заново при попадании мышью.
    /// Две независимые раскладки — рисующая и проверяющая — расходятся молча, и
    /// расхождение выглядит как «кнопка не нажимается», хотя нажимается
    /// соседняя.
    buttons: Buttons,
    /// Значки трея там же и по той же причине.
    tray: Tray,
    /// Отпечаток нарисованного: по нему видно, надо ли рисовать заново.
    ///
    /// `None` — панель ещё ни разу не рисовали.
    mark: Option<Fingerprint>,
    /// Раскладка дока — только на телефоне (см. [`dock_layout`]).
    ///
    /// Запоминается при рисовании, как и кнопки окон: две независимые
    /// раскладки расходятся молча, и расхождение выглядит как «кнопка не
    /// нажимается», хотя нажимается соседняя.
    dock: Option<DockLayout>,
}

impl Panel {
    /// Сколько панель отнимает у экрана снизу.
    ///
    /// Не высота плашки: плашка плавает, и поля над ней и под ней принадлежат
    /// ей так же, как она сама. Окно, доведённое до низа рабочей области,
    /// обязано остановиться над верхним полем, а не над самой плашкой — иначе
    /// оно ляжет под её тень и подсветится ею снизу.
    #[must_use]
    pub fn height(scale: u32) -> u32 {
        if theme::is_mobile() {
            // Док у телефона выше панели и отстоит от края дальше: под нижним
            // краем экрана лежит место жеста «домой», а по бокам — скруглённые
            // углы. Кнопка, попавшая туда, видна и не нажимается.
            return (theme::M_DOCK_H + theme::M_INSET * 2) * scale;
        }
        (theme::PANEL_H + theme::PANEL_INSET * 2) * scale
    }

    #[must_use]
    pub fn new(screen_w: u32, screen_h: u32, scale: u32) -> Option<Self> {
        let scale = scale.max(1);
        // Оба числа обязаны быть теми же, из которых сложена [`Panel::height`]:
        // по ней композитор считает нижнюю границу рабочей области. Разойдись
        // они — и плашка встанет не на своё поле, а окна улягутся ей под тень.
        let (inset, plate) = if theme::is_mobile() {
            (theme::M_INSET, theme::M_DOCK_H)
        } else {
            (theme::PANEL_INSET, theme::PANEL_H)
        };
        let inset = inset * scale;
        let plate_h = plate * scale;
        let plate_w = screen_w.checked_sub(inset * 2).filter(|w| *w > 0)?;
        let surface = Surface::new(plate_w, plate_h, glass_bg())?;
        // Плашка стоит на нижнем поле той полосы, которую панель отнимает у
        // экрана. Считается через [`Panel::height`], а не своим сложением: по
        // этому же числу нижнюю границу рабочей области считает композитор, и
        // два независимых выражения одного числа однажды разойдутся на поле.
        let top = screen_h as i32 - Self::height(scale) as i32 + inset as i32;
        Some(Self {
            surface,
            rect: Rect::new(inset as i32, top, plate_w, plate_h),
            scale,
            damage: Rect::EMPTY,
            brand: Rect::EMPTY,
            buttons: Buttons::new(),
            tray: Tray::new(),
            mark: None,
            dock: None,
        })
    }

    /// Нарисовать док телефона: телефон, поиск, камера.
    ///
    /// Кнопок открытых окон здесь нет намеренно. На телефоне окно одно и во
    /// весь экран — переключать нечего, а список окон занял бы место того, чем
    /// пользуются каждый день.
    ///
    /// За телефоном и камерой программ пока нет, и кнопки всё равно стоят:
    /// место под них в доке — это решение о раскладке, а не обещание. Нажатие
    /// доходит до стола и называет вслух, чего именно нет.
    fn redraw_dock(&mut self, m: Metrics, plate: Rect, menu_open: bool, windows: &[Entry]) {
        let ctx = m.ctx;
        let p = ctx.palette;
        let minimized = windows.iter().filter(|entry| entry.minimized).count();
        let layout = dock_layout(m, plate, minimized);
        let glyph = ctx.px(20);

        // Боковые кнопки: подложка кнопки, значок по центру. Того, за чем нет
        // программы, это не отличает — отличать нечем, и притворяться, что
        // кнопка «выключена», было бы неправдой: нажать её можно.
        for (rect, icon) in [(layout.left, Icon::Devices), (layout.right, Icon::Display)] {
            draw::rounded(&mut self.surface, rect, m.round_row, p.btn.color, p.btn.alpha);
            draw::rounded_stroke(
                &mut self.surface,
                rect,
                m.round_row,
                p.btnline.color,
                p.btnline.alpha,
            );
            glyphicon::draw(
                &mut self.surface,
                icon,
                rect.x + (rect.w as i32 - glyph as i32) / 2,
                rect.y + (rect.h as i32 - glyph as i32) / 2,
                glyph,
                p.ink2,
                255,
            );
        }

        // Кнопка «Пуск». Открытое меню переворачивает градиент — кнопка
        // выглядит вдавленной; то же правило, что у настольной панели.
        let (top, bottom) = if menu_open { (p.acc2, p.acc) } else { (p.acc, p.acc2) };
        draw::rounded_gradient(&mut self.surface, layout.brand, m.round_row, top, bottom, 255);
        draw::rounded_stroke(
            &mut self.surface,
            layout.brand,
            m.round_row,
            p.accline.color,
            p.accline.alpha,
        );
        // Широкая кнопка подписана именем системы, узкая — только значком:
        // подпись, обрезанная до «Fr…», хуже её отсутствия.
        let wide = layout.brand.w > m.btn_h + ctx.px(20);
        let icon_x = if wide {
            layout.brand.x + m.side as i32
        } else {
            layout.brand.x + (layout.brand.w as i32 - glyph as i32) / 2
        };
        glyphicon::draw(
            &mut self.surface,
            Icon::Grid,
            icon_x,
            layout.brand.y + (layout.brand.h as i32 - glyph as i32) / 2,
            glyph,
            WHITE,
            255,
        );
        if wide {
            let text_x = icon_x + glyph as i32 + m.gap as i32;
            paint::text_clipped(
                ctx,
                &mut self.surface,
                Role::Title,
                text_x,
                paint::baseline(ctx, Role::Title, layout.brand),
                layout.brand.right().saturating_sub(text_x).max(0) as u32,
                DOCK_BRAND,
                WHITE,
            );
        }

        // Стопка свёрнутых окон: плитки внахлёст со сдвигом, чтобы из-под
        // верхней выглядывали края нижних. Так видно, что окон несколько, не
        // называя их числом.
        if !layout.stack.is_empty() {
            draw::rounded(
                &mut self.surface,
                layout.stack,
                m.round_row,
                p.btn.color,
                p.btn.alpha,
            );
            draw::rounded_stroke(
                &mut self.surface,
                layout.stack,
                m.round_row,
                p.btnline.color,
                p.btnline.alpha,
            );
            let tile = ctx.px(22);
            let step = ctx.px(6);
            let count = minimized.min(3);
            let total = tile + step * (count.saturating_sub(1)) as u32;
            let base_x = layout.stack.x + (layout.stack.w as i32 - total as i32) / 2;
            let base_y = layout.stack.y + (layout.stack.h as i32 - tile as i32) / 2;
            for index in 0..count {
                // Нижние плитки приподняты и сдвинуты: поворота у нас нет, а
                // разнобой нужен — ровная стопка читается как одна плитка.
                let lift = ((count - index - 1) as u32 * ctx.px(2)) as i32;
                let rect = Rect::new(
                    base_x + (index as u32 * step) as i32,
                    base_y - lift,
                    tile,
                    tile,
                );
                let last = index + 1 == count;
                if last {
                    draw::rounded_gradient(&mut self.surface, rect, ctx.px(7), p.acc, p.acc2, 255);
                } else {
                    draw::rounded(&mut self.surface, rect, ctx.px(7), ctx.flat(p.ghost), 255);
                }
                draw::rounded_stroke(
                    &mut self.surface,
                    rect,
                    ctx.px(7),
                    p.btnline.color,
                    p.btnline.alpha,
                );
            }
        }

        // Поле поиска.
        draw::rounded(&mut self.surface, layout.search, m.round_row, p.btn.color, p.btn.alpha);
        draw::rounded_stroke(
            &mut self.surface,
            layout.search,
            m.round_row,
            p.btnline.color,
            p.btnline.alpha,
        );
        let search_icon = ctx.px(16);
        let icon_x = layout.search.x + m.gap as i32;
        glyphicon::draw(
            &mut self.surface,
            Icon::Search,
            icon_x,
            layout.search.y + (layout.search.h as i32 - search_icon as i32) / 2,
            search_icon,
            p.ink3,
            255,
        );
        let text_x = icon_x + search_icon as i32 + m.gap as i32;
        paint::text_clipped(
            ctx,
            &mut self.surface,
            Role::Body,
            text_x,
            paint::baseline(ctx, Role::Body, layout.search),
            layout.search.right().saturating_sub(text_x).max(0) as u32,
            SEARCH_HINT,
            p.ink3,
        );

        // Полоска жеста. Она лежит ниже плашки — там, где у этого экрана и так
        // ничего не помещается, — и служит меткой низа, а не кнопкой.
        draw::rounded(
            &mut self.surface,
            layout.home,
            layout.home.h / 2,
            p.ink6,
            160,
        );

        self.dock = Some(layout);
        self.brand = layout.search;
        self.buttons = Buttons::new();
        self.tray = Tray::new();
    }

    /// Забыть, что было нарисовано: следующий [`Panel::redraw`] нарисует заново.
    ///
    /// Нужен там, где меняются цвета, а не содержимое: тема, акцент, обои. Ни
    /// одно из этого в отпечаток не входит и входить не должно — иначе он
    /// превратился бы в список всего на свете, — но плашка от них меняется, и
    /// без сброса она осталась бы в прежних цветах.
    pub fn forget(&mut self) {
        self.mark = None;
    }

    /// Во что попадает точка панели (координаты экрана).
    ///
    /// `None` за пределами плашки: поля вокруг неё — это обои, и щелчок по ним
    /// обязан достаться столу, а не быть съеденным панелью.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<PanelHit> {
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        if !self.surface.bounds().contains(x, y) {
            return None;
        }
        // Док разбирается первым и отдельно: у него своя раскладка, и кнопок
        // окон в нём нет вовсе.
        if let Some(dock) = self.dock {
            // По высоте — на всю плашку, как и у настольных кнопок: промах на
            // пару точек читается как «не нажалось», а пустое поле вокруг
            // кнопки больше ничем не занято.
            if x >= dock.left.x && x < dock.left.right() {
                return Some(PanelHit::Missing("phone"));
            }
            if x >= dock.brand.x && x < dock.brand.right() {
                return Some(PanelHit::Menu);
            }
            if !dock.stack.is_empty() && x >= dock.stack.x && x < dock.stack.right() {
                return Some(PanelHit::Stack);
            }
            if x >= dock.search.x && x < dock.search.right() {
                return Some(PanelHit::Menu);
            }
            if x >= dock.right.x && x < dock.right.right() {
                return Some(PanelHit::Missing("camera"));
            }
            return Some(PanelHit::Empty);
        }
        if self.brand.contains(x, y) {
            return Some(PanelHit::Menu);
        }
        for (app, rect) in &self.buttons {
            // По ширине — ровно как нарисовано, по высоте — на всю плашку:
            // промах мимо кнопки на пару точек читается как «не нажалось», а
            // между кнопкой и краем плашки лежит одно пустое поле, которым
            // больше никто не занят.
            if x >= rect.x && x < rect.right() {
                return Some(PanelHit::Window(*app));
            }
        }
        for (item, rect) in &self.tray {
            if x >= rect.x && x < rect.right() {
                return Some(PanelHit::Tray(*item));
            }
        }
        Some(PanelHit::Empty)
    }

    /// Перерисовать панель целиком.
    ///
    /// Целиком, а не по частям: панель — это одна плашка в полсотни точек
    /// высотой, и вычисление изменившегося куска стоило бы дороже перерисовки.
    /// Она же перекрашивает панель после смены темы — отдельного пути для этого
    /// нет и не нужно.
    pub fn redraw(&mut self, windows: &[Entry], menu_open: bool, status: &Status) {
        // Перерисовывать только то, что изменилось. Панель просят обновиться на
        // **каждое** событие ввода, а у тачскрина их шестьдесят пять в секунду;
        // при этом на ней почти всегда ровно то же, что было. Измерено на
        // телефоне: верхние слои съедали 20 мс из 32 на кадр — это и была
        // перерисовка плашки 720×180 со скруглением, обводкой и короной,
        // делавшаяся впустую по шестьдесят пять раз в секунду.
        let mark = Fingerprint::of(windows, menu_open, status);
        if self.mark == Some(mark) {
            return;
        }
        self.mark = Some(mark);
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        let p = ctx.palette;
        let plate = self.surface.bounds();

        // Заливка — во всю поверхность, а не по скруглённой фигуре: углы срежет
        // композитор, смешав их с обоями, и нарисованное здесь скругление
        // означало бы, что углы срезаются дважды.
        self.surface.fill(plate, ctx.under);
        draw::rounded_stroke(&mut self.surface, plate, m.round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, plate, m.round, p.crown.color, p.crown.alpha);

        // Телефон: док вместо панели задач. Кнопок окон здесь нет и быть не
        // может — окно на телефоне одно и во весь экран, переключать нечего.
        // Вместо них то, чем пользуются: приложения, поиск, телефон и камера.
        if theme::is_mobile() {
            self.redraw_dock(m, plate, menu_open, windows);
            self.damage = plate;
            return;
        }

        // Правый край считается раньше кнопок: место под трей занято всегда, а
        // кнопкам достаётся то, что осталось. Наоборот было бы хуже — десяток
        // открытых окон вытеснил бы часы за край экрана.
        let clock = match status.clock.as_deref() {
            Some(clock) => String::from(clock),
            None => alloc::format!("up {}", uptime_text(status.uptime_ms)),
        };
        let lang = crate::input::keymap::layout().label();
        let mono = ctx.face(Role::Mono);
        let body = ctx.face(Role::Body);
        // Шаг между значками трея — шире зазора кнопок: значки не кнопки в
        // ряд, а три отдельных индикатора, и слипшиеся они читались бы как
        // одна надпись «RU 12:30».
        let step = ctx.px(14);
        let glyph = m.glyph();
        let mut status_w = glyph + step + body.width(lang) + step + mono.width(&clock);
        // Совсем узкий экран: показать всё нельзя, и тогда правого края нет
        // вовсе. Обрезанное наполовину число хуже отсутствующего — по нему не
        // понять, что именно обрезано.
        if status_w + m.brand_width() + m.plate_pad() * 2 + m.gap * 2 > plate.w {
            status_w = 0;
        }

        let layout = panel_layout(m, plate, windows, status_w);
        self.brand = layout.brand;
        self.buttons = layout.buttons;

        // Кнопка меню. Открытое меню переворачивает градиент: кнопка выглядит
        // вдавленной, и это единственное, чем «меню открыто» отличается от
        // «меню закрыто» — заливка другим цветом читалась бы как другая кнопка.
        let (top, bottom) = if menu_open { (p.acc2, p.acc) } else { (p.acc, p.acc2) };
        draw::rounded_gradient(&mut self.surface, layout.brand, m.round_row, top, bottom, 255);
        draw::rounded_stroke(
            &mut self.surface,
            layout.brand,
            m.round_row,
            p.accline.color,
            p.accline.alpha,
        );
        let glyph = m.glyph();
        glyphicon::draw(
            &mut self.surface,
            Icon::Grid,
            layout.brand.x + m.side as i32,
            layout.brand.y + (layout.brand.h as i32 - glyph as i32) / 2,
            glyph,
            WHITE,
            255,
        );
        paint::text(
            ctx,
            &mut self.surface,
            Role::Title,
            layout.brand.x + (m.side + glyph + m.gap) as i32,
            paint::baseline(ctx, Role::Title, layout.brand),
            BRAND,
            WHITE,
        );

        draw::vline(
            &mut self.surface,
            layout.divider.x,
            layout.divider.y,
            layout.divider.h,
            p.line2.color,
            p.line2.alpha,
        );

        for (entry, (_, rect)) in windows.iter().zip(self.buttons.iter()) {
            let rect = *rect;
            let state = ButtonState::of(entry.focused, entry.minimized);
            if state == ButtonState::Active {
                draw::rounded(&mut self.surface, rect, m.round_row, p.ghost.color, p.ghost.alpha);
                draw::rounded_stroke(
                    &mut self.surface,
                    rect,
                    m.round_row,
                    p.btnline.color,
                    p.btnline.alpha,
                );
            }
            // Кружок — это состояние окна, а не украшение: зелёный у активного,
            // тусклый у прочих. Он же держит подписи на одной вертикали,
            // сколько бы кнопок ни стояло рядом.
            let dot = m.dot();
            draw::circle(
                &mut self.surface,
                rect.x + (m.side + dot / 2) as i32,
                rect.y + rect.h as i32 / 2,
                dot / 2,
                state.dot(p),
                255,
            );
            let role = state.role();
            paint::text_clipped(
                ctx,
                &mut self.surface,
                role,
                rect.x + (m.side + dot + m.gap) as i32,
                paint::baseline(ctx, role, rect),
                rect.w.saturating_sub(m.side * 2 + dot + m.gap),
                &entry.caption,
                state.ink(p),
            );
        }

        self.tray.clear();
        if status_w > 0 {
            let half = (step / 2) as i32;
            let mut x = plate.right() - (m.plate_pad() + status_w) as i32;

            // Сеть: значок в полную силу, когда адрес есть, и приглушённый,
            // когда карты нет или адрес не получен. Приглушённый, а не другой
            // значок: «нет сети» в наборе значков нет, а чужой значок сказал бы
            // не то. Разница между «нет карты» и «нет адреса» — в меню трея.
            let net_ink = match status.net {
                NetState::Up => p.ink,
                NetState::NoAddress | NetState::Absent => p.ink3,
            };
            glyphicon::draw(
                &mut self.surface,
                Icon::Network,
                x,
                plate.y + (plate.h as i32 - glyph as i32) / 2,
                glyph,
                net_ink,
                255,
            );
            // Область попадания шире значка на полшага в обе стороны: значок в
            // четырнадцать точек — цель для меткого, а трей должен нажиматься
            // с первого раза.
            self.tray.push((
                TrayItem::Network,
                Rect::new(x - half, plate.y, glyph + step, plate.h),
            ));
            x += (glyph + step) as i32;

            let width = paint::text(
                ctx,
                &mut self.surface,
                Role::Body,
                x,
                paint::baseline(ctx, Role::Body, plate),
                lang,
                p.ink,
            );
            self.tray.push((TrayItem::Layout, Rect::new(x - half, plate.y, width + step, plate.h)));
            x += (width + step) as i32;

            let width = paint::text(
                ctx,
                &mut self.surface,
                Role::Mono,
                x,
                paint::baseline(ctx, Role::Mono, plate),
                &clock,
                p.ink,
            );
            self.tray.push((
                TrayItem::Clock,
                Rect::new(x - half, plate.y, width + m.plate_pad(), plate.h),
            ));
        }

        self.damage = self.surface.bounds();
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

/// Раскладка панели.
///
/// Считается одной функцией, а не двумя — рисующей и проверяющей попадание:
/// вторая раскладка расходится с первой молча, и расхождение видно только
/// руками, когда кнопка перестаёт нажиматься.
struct PanelLayout {
    brand: Rect,
    /// Разделитель между кнопкой меню и кнопками окон.
    divider: Rect,
    buttons: Buttons,
}

/// Раскладка дока на телефоне: две кнопки по краям и поиск посередине.
///
/// Считается отдельной функцией, а не внутри рисования, по той же причине, что
/// и настольная: раскладку спрашивают двое — тот, кто рисует, и тот, кто
/// разбирает нажатие, — и посчитанная дважды она однажды разойдётся.
#[derive(Clone, Copy)]
struct DockLayout {
    /// Кнопка слева: телефон.
    left: Rect,
    /// Кнопка «Пуск»: широкая с подписью, пока нет свёрнутых окон, и плитка,
    /// когда они появились.
    brand: Rect,
    /// Стопка свёрнутых окон. Пустой прямоугольник — сворачивать нечего.
    stack: Rect,
    /// Поле поиска.
    search: Rect,
    /// Плитка со значком внутри поля поиска.
    chip: Rect,
    /// Кнопка справа: камера.
    right: Rect,
    /// Полоска жеста под доком.
    home: Rect,
}

fn dock_layout(m: Metrics, plate: Rect, minimized: usize) -> DockLayout {
    let side = m.btn_h;
    let pad = (plate.h.saturating_sub(side)) / 2;
    let y = plate.y + pad as i32;
    let gap = m.ctx.px(10);

    let left = Rect::new(plate.x + pad as i32, y, side, side);
    let right = Rect::new(plate.right() - (pad + side) as i32, y, side, side);
    let mut search_x = left.right() + gap as i32;
    let room = (right.x - gap as i32 - search_x).max(side as i32) as u32;

    // Пока свёрнутых окон нет, «Пуск» широкий и подписан именем системы, а
    // поиск занимает остаток. Как только окна сворачиваются, между ними встаёт
    // стопка, и «Пуск» ужимается до плитки: место на экране одно, и делить его
    // приходится с тем, что появилось.
    let (brand_w, stack) = if minimized == 0 {
        (room * 2 / 5, Rect::EMPTY)
    } else {
        let stack_w = side + m.ctx.px(18);
        let brand = side;
        let stack = Rect::new(search_x + brand as i32 + gap as i32, y, stack_w, side);
        (brand, stack)
    };
    let brand = Rect::new(search_x, y, brand_w, side);
    search_x = brand.right() + gap as i32;
    if !stack.is_empty() {
        search_x = stack.right() + gap as i32;
    }
    let search_w = (right.x - gap as i32 - search_x).max(m.ctx.px(40) as i32) as u32;
    let search = Rect::new(search_x, y, search_w, side);

    // Плитка внутри поля — на четыре точки меньше него со всех сторон: в
    // макете она вложена с полем, а не вписана в край.
    let inner = m.ctx.px(4);
    let chip_side = side.saturating_sub(inner * 2);
    let chip = Rect::new(search.x + inner as i32, search.y + inner as i32, chip_side, chip_side);

    // Полоска жеста лежит **под** доком, у самого низа экрана: это метка
    // системы, а не элемент дока, и внутри плашки она читалась бы кнопкой.
    let home_w = m.ctx.px(theme::M_HOME_W);
    let home_h = m.ctx.px(theme::M_HOME_H);
    let home = Rect::new(
        plate.x + (plate.w as i32 - home_w as i32) / 2,
        plate.bottom() + ((pad as i32 - home_h as i32) / 2).max(0),
        home_w,
        home_h,
    );

    DockLayout { left, brand, stack, search, chip, right, home }
}

fn panel_layout(m: Metrics, plate: Rect, windows: &[Entry], status_w: u32) -> PanelLayout {
    let pad = m.plate_pad();
    let y = plate.y + pad as i32;
    let brand = Rect::new(plate.x + pad as i32, y, m.brand_width(), m.btn_h);

    // Разделитель стоит вплотную к обоим соседям: это не отступ между блоками,
    // а черта внутри одного блока, и широкие поля вокруг неё превратили бы её в
    // третий столбец панели.
    let rule = m.ctx.px(4);
    let rule_h = m.ctx.px(24);
    let divider = Rect::new(
        brand.right() + rule as i32,
        plate.y + (plate.h as i32 - rule_h as i32) / 2,
        1,
        rule_h,
    );

    let mut x = divider.right() + rule as i32;
    let limit =
        plate.right() - (pad + status_w) as i32 - if status_w > 0 { m.gap as i32 } else { 0 };
    // Кнопка уже этой не расскажет ничего: кружок, поля и три-четыре знака
    // подписи. Всё, что короче, — не сокращённая кнопка, а мусор в строке.
    let least = m.side * 2 + m.dot() + m.gap + m.ctx.px(24);
    let mut buttons = Buttons::new();
    for entry in windows {
        let room = (limit - x).max(0) as u32;
        if room < least {
            break;
        }
        let state = ButtonState::of(entry.focused, entry.minimized);
        let width = m.button_width(&entry.caption, state).min(room);
        buttons.push((entry.app, Rect::new(x, y, width, m.btn_h)));
        x += (width + m.gap) as i32;
    }

    PanelLayout { brand, divider, buttons }
}

/// Что выбрано в меню запуска.
///
/// Окно стола и программа из `/bin` — разные вещи, и меню обязано возвращать
/// разные ответы: окно стол открывает сам, а программу запускает.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Окно, которое умеет открыть сам стол.
    App(App),
    /// Программа с собственным окном — её имя в `/bin`.
    Program(&'static str),
    /// Программа, поставленная пакетом (фаза N8), — командная строка из
    /// `start=` её манифеста.
    Command(String),
}

/// Программа, поставленная пакетом (фаза N8): строка «Пуска» из реестра
/// `/var/lib/pkg`.
///
/// Манифест пакета — первое место, где программа говорит о себе сама, и
/// названный предел списка [`PROGRAMS`] здесь снят: `start=` — чем её
/// запускать, `caption=` и `about=` — как её назвать. Пакет без `start=` в меню
/// не попадает: это библиотека или данные, запускать там нечего.
struct Launcher {
    name: String,
    caption: String,
    about: String,
    start: String,
}

/// Пакеты со строкой запуска — по имени, чтобы порядок строк не зависел от
/// порядка записей в каталоге.
fn list_packages() -> Vec<Launcher> {
    const REGISTRY: &str = "/var/lib/pkg";
    // Предел манифеста в контейнере — 32 КиБ (`fpk::MAX_MANIFEST`); запись
    // реестра — тот же манифест.
    const LIMIT: usize = 32 * 1024;
    let Some(Ok(entries)) = crate::fs::list(REGISTRY) else {
        return Vec::new();
    };
    let mut launchers = Vec::new();
    for entry in entries {
        if entry.kind != crate::vfs::NodeKind::File {
            continue;
        }
        let Some(name) = entry.name.strip_suffix(".pkg") else {
            continue;
        };
        let path = alloc::format!("{REGISTRY}/{}", entry.name);
        // Запись, которую не прочитать или не разобрать, — не повод терять
        // меню целиком: строки просто не будет.
        let Some(Ok((bytes, _))) = crate::fs::read(&path, LIMIT) else {
            continue;
        };
        let Ok(text) = core::str::from_utf8(&bytes) else {
            continue;
        };
        let Some(start) = sysconf::value(text, "start").filter(|line| !line.is_empty()) else {
            continue;
        };
        let about = sysconf::value(text, "about").or_else(|| sysconf::value(text, "summary")).unwrap_or("");
        launchers.push(Launcher {
            name: String::from(name),
            caption: String::from(sysconf::value(text, "caption").unwrap_or(name)),
            about: String::from(about),
            start: String::from(start),
        });
    }
    launchers.sort_by(|a, b| a.name.cmp(&b.name));
    launchers
}

/// Программа из `/bin`, у которой есть окно, — то, что стоит в «Пуске».
///
/// Список, а не признак в самом файле, и это названный предел: исполняемый
/// файл пока не умеет сказать о себе ни имени для человека, ни значка, ни того,
/// откроет ли он окно. Пока формата для этого нет, такие программы названы
/// здесь поимённо. `winshow` среди них нет намеренно: это проверка договора об
/// окнах, а не программа для человека.
struct Program {
    file: &'static str,
    caption: &'static str,
    about: &'static str,
    icon: Icon,
}

static PROGRAMS: [Program; 3] = [
    Program {
        file: "files",
        caption: "Файлы",
        about: "папки и файлы на дисках",
        icon: Icon::Folder,
    },
    // Диспетчер задач, а не монитор системы (фаза С5): `sysmon` остался в
    // `/bin` ради стенда, но человеку из меню нужен тот, из которого задачу
    // можно снять.
    Program {
        file: "taskmgr",
        caption: "Диспетчер задач",
        about: "процессы, память и службы",
        icon: Icon::Chart,
    },
    // Диспетчер устройств (фаза С7): что стоит в машине и чем обслуживается.
    Program {
        file: "devmgr",
        caption: "Диспетчер устройств",
        about: "устройства и их драйверы",
        icon: Icon::Devices,
    },
];

/// Строка меню: окно стола, программа или программа из пакета.
///
/// Пакет — номер в списке пакетов меню, а не ссылка: список читается с диска
/// при каждом открытии и живёт в самом меню, а `Item` копируется.
#[derive(Clone, Copy)]
enum Item {
    App(App),
    Program(&'static Program),
    Package(usize),
}

impl Item {
    fn caption(self, packages: &[Launcher]) -> &str {
        match self {
            Item::App(app) => app.caption(),
            Item::Program(program) => program.caption,
            Item::Package(index) => packages.get(index).map_or("", |launcher| launcher.caption.as_str()),
        }
    }

    fn about(self, packages: &[Launcher]) -> &str {
        match self {
            Item::App(app) => app.about(),
            Item::Program(program) => program.about,
            Item::Package(index) => packages.get(index).map_or("", |launcher| launcher.about.as_str()),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Item::App(app) => app.icon(),
            Item::Program(program) => program.icon,
            // Значков из ресурсов `.exe` система не читает — у всех пакетов один.
            Item::Package(_) => Icon::Package,
        }
    }

    fn tone(self) -> Tone {
        match self {
            Item::App(app) => app.tone(),
            Item::Program(_) | Item::Package(_) => Tone::Accent,
        }
    }

    /// Строка гасит или перезапускает машину.
    fn danger(self) -> bool {
        matches!(self, Item::App(app) if app.confirms_power().is_some())
    }

    fn choice(self, packages: &[Launcher]) -> Option<Choice> {
        Some(match self {
            Item::App(app) => Choice::App(app),
            Item::Program(program) => Choice::Program(program.file),
            Item::Package(index) => Choice::Command(packages.get(index)?.start.clone()),
        })
    }
}

/// Меню запуска: плавающая карточка над кнопкой «FreeOS».
///
/// # Одна колонка, и в ней только то, у чего есть окно
///
/// До фазы С3 справа от окон стола стояло всё содержимое `/bin` — три десятка
/// имён, среди которых `ls`, `fetch` и `dhcp`. Человек, открывший «Пуск», ищет
/// в нём программы, а не команды, которые без терминала ничего не показывают.
/// Команды перечисляет `help`, и запускаются они там же, где виден их ответ.
///
/// Порядок первых трёх строк — «Терминал», «Параметры», «О системе» — прежний,
/// и это не лень: сценарии стенда ходят по меню клавишами и считают строки
/// нажатиями.
pub struct Menu {
    surface: Surface,
    pub rect: Rect,
    /// Множитель геометрии стола.
    scale: u32,
    items: Vec<Item>,
    /// Выбранная строка.
    row: usize,
    open: bool,
    damage: Rect,
    /// Сколько программ лежит в `/bin` — для строки журнала при запуске стола.
    bin_programs: usize,
    /// Программы из пакетов, прочитанные при сборке меню (фаза N8).
    packages: Vec<Launcher>,
    /// Ширина колонки строк.
    width: u32,
}

impl Menu {
    #[must_use]
    pub fn new(panel_top: i32, scale: u32, screen_w: u32) -> Option<Self> {
        let scale = scale.max(1);
        let ctx = desk_ctx(scale);
        let m = Metrics::new(ctx);
        if panel_top <= 0 {
            return None;
        }

        let names = list_programs();
        let mut items = Vec::new();
        items.extend([Item::App(App::Terminal), Item::App(App::Settings), Item::App(App::About)]);
        // Программа попадает в меню, только если она действительно лежит в
        // `/bin`: строка, которая ничего не запускает, хуже отсутствующей.
        for program in &PROGRAMS {
            if names.iter().any(|name| name == program.file) {
                items.push(Item::Program(program));
            }
        }
        // Программы из пакетов — после своих и до строк питания (фаза N8).
        let packages = list_packages();
        items.extend((0..packages.len()).map(Item::Package));
        items.extend([Item::App(App::Shutdown), Item::App(App::Restart)]);

        // Ширина — по самой длинной строке, а не заданным числом: подрезанная
        // подпись выглядит как испорченный вывод.
        let title = ctx.face(Role::Title);
        let note = ctx.face(Role::Caption);
        let mut widest = ctx.face(Role::MonoCaps).width(APPS_TITLE);
        for item in &items {
            widest = widest.max(title.width(item.caption(&packages))).max(note.width(item.about(&packages)));
        }
        let width = m.row_width(widest);
        let card_w = m.pad * 2 + width.max(m.header_width());

        // Высота — по содержимому: карточка кончается там, где кончились
        // строки, а не там, где кончился экран.
        let rows_h = items.len() as u32 * m.pitch + m.rule_height();
        let body_h = rows_h.saturating_sub(m.pitch - m.row_h);
        let card_h = (m.head_height() + body_h + m.pad).min(panel_top as u32);

        // На телефоне «Пуск» занимает экран целиком — от строки состояния до
        // дока. Карточка по содержимому здесь не годится: строк десяток, они
        // крупные, и посчитанная по ним карточка либо не влезет, либо встанет
        // узкой колонкой у левого края, оставив две трети экрана пустыми.
        let (card_w, card_h, width) = if theme::is_mobile() {
            let inset = ctx.px(theme::M_INSET);
            let top = ctx.px(theme::M_STATUS_H + theme::M_INSET);
            let full_w = screen_w.saturating_sub(inset * 2).max(1);
            let full_h = (panel_top - top as i32).max(1) as u32;
            (full_w, full_h, full_w.saturating_sub(m.pad * 2))
        } else {
            (card_w, card_h, width)
        };

        let surface = Surface::new(card_w, card_h, glass_bg())?;
        // Левый край карточки — ровно левый край плашки панели: одна вертикаль
        // на весь стол читается как порядок, две почти совпадающие — как ошибка
        // отрисовки. На телефоне поле своё — под скруглённые углы экрана.
        let left = if theme::is_mobile() { ctx.px(theme::M_INSET) } else { m.inset };
        let rect = Rect::new(
            left as i32,
            panel_top - surface.height() as i32,
            surface.width(),
            surface.height(),
        );
        Some(Self {
            surface,
            rect,
            scale,
            items,
            row: 0,
            open: false,
            damage: Rect::EMPTY,
            bin_programs: names.len(),
            packages,
            width,
        })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Сколько строк в меню.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Сколько программ лежит в `/bin`.
    #[must_use]
    pub const fn bin_programs(&self) -> usize {
        self.bin_programs
    }

    /// Имена пакетов, стоящих в меню, через запятую — для журнала (фаза N8).
    #[must_use]
    pub fn package_names(&self) -> String {
        let names: Vec<&str> = self.packages.iter().map(|launcher| launcher.name.as_str()).collect();
        names.join(", ")
    }

    /// Открыть или закрыть меню. Возвращает новое состояние.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        if self.open {
            self.row = 0;
            self.redraw();
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Перерисовать меню под текущую тему.
    ///
    /// Перерисовывается и закрытое меню: тема могла смениться, пока оно
    /// закрыто, и открыться оно обязано уже в новых цветах.
    pub fn restyle(&mut self) {
        self.redraw();
    }

    /// Номер строки под точкой экрана.
    ///
    /// Та же раскладка, что и при рисовании, — не пересчитанная заново, а
    /// ровно та же функция: щелчок обязан попадать туда, куда нарисовано.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        self.layout().rows.iter().find(|row| row.rect.contains(x, y)).map(|row| row.index)
    }

    /// Что лежит под точкой (координаты экрана).
    #[must_use]
    pub fn choice_at(&self, x: i32, y: i32) -> Option<Choice> {
        let index = self.row_at(x, y)?;
        self.items.get(index).and_then(|item| item.choice(&self.packages))
    }

    /// Поставить выделение туда, где стоит указатель.
    pub fn select_at(&mut self, x: i32, y: i32) {
        if let Some(index) = self.row_at(x, y) {
            if index != self.row {
                self.row = index;
                self.redraw();
            }
        }
    }

    /// Сдвинуть выбор, по кругу.
    pub fn move_selection(&mut self, forward: bool) {
        let count = self.items.len();
        if count == 0 {
            return;
        }
        self.row = if forward { (self.row + 1) % count } else { (self.row + count - 1) % count };
        self.redraw();
    }

    /// Что выбрано сейчас.
    #[must_use]
    pub fn selection(&self) -> Option<Choice> {
        self.items.get(self.row).and_then(|item| item.choice(&self.packages))
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Раскладка карточки: где шапка, где заголовок колонки, где строки.
    fn layout(&self) -> MenuLayout {
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        // Карточка — это вся поверхность: тень под ней и срез углов делает
        // композитор.
        let card = self.surface.bounds();

        let badge = Rect::new(card.x + m.pad as i32, card.y + m.pad as i32, m.head(), m.head());
        let name_x = badge.right() + m.gap as i32;
        let name_line = i32::from(ctx.face(Role::Title).line);
        let lines = name_line + i32::from(ctx.face(Role::MonoSmall).line);
        let name_y = badge.y + (badge.h as i32 - lines) / 2;

        let caps_y = card.y + (m.pad + m.head() + m.gap) as i32;
        let left_x = card.x + m.pad as i32;

        let mut rows = Vec::new();
        let mut rules = Vec::new();
        let mut y = caps_y + m.caps() as i32;
        let mut ruled = false;
        for (index, item) in self.items.iter().enumerate() {
            // Питание отделяется чертой: «выключить» рядом с «открыть файлы» —
            // это соседство, в котором однажды промахиваются. Черта одна на
            // обе строки питания.
            if !ruled && item.danger() && index > 0 {
                ruled = true;
                rules.push(Rect::new(
                    left_x + m.row_pad as i32,
                    y + ctx.px(6) as i32,
                    self.width.saturating_sub(m.row_pad * 2),
                    1,
                ));
                y += m.rule_height() as i32;
            }
            rows.push(MenuRow {
                rect: Rect::new(left_x, y, self.width, m.row_h),
                index,
                icon: item.icon(),
                tone: item.tone(),
                danger: item.danger(),
            });
            y += m.pitch as i32;
        }

        MenuLayout {
            card,
            badge,
            name: (name_x, name_y),
            note: (name_x, name_y + name_line),
            caps: (left_x + m.row_pad as i32, caps_y),
            rows,
            rules,
        }
    }

    fn redraw(&mut self) {
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        let p = ctx.palette;
        let layout = self.layout();

        let card = layout.card;
        // Во всю поверхность и без скругления — по той же причине, что и у
        // плашки: углы срезает композитор, смешивая их с обоями.
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, m.round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, m.round, p.crown.color, p.crown.alpha);

        // Шапка: имя системы и версия. Знак в плитке — тот же, что на кнопке
        // «FreeOS» под ней.
        paint::badge(ctx, &mut self.surface, layout.badge, Tone::Accent);
        let head_glyph = layout.badge.w * 4 / 7;
        glyphicon::draw(
            &mut self.surface,
            Icon::Grid,
            layout.badge.x + (layout.badge.w as i32 - head_glyph as i32) / 2,
            layout.badge.y + (layout.badge.h as i32 - head_glyph as i32) / 2,
            head_glyph,
            WHITE,
            255,
        );
        paint::text(ctx, &mut self.surface, Role::Title, layout.name.0, layout.name.1, BRAND, p.ink);
        paint::text(
            ctx,
            &mut self.surface,
            Role::MonoSmall,
            layout.note.0,
            layout.note.1,
            crate::VERSION,
            p.ink4,
        );

        paint::caps(ctx, &mut self.surface, layout.caps.0, layout.caps.1, APPS_TITLE);

        for rule in &layout.rules {
            paint::separator(ctx, &mut self.surface, rule.x, rule.y, rule.w);
        }

        for row in &layout.rows {
            let Some(item) = self.items.get(row.index).copied() else {
                continue;
            };
            draw_row(
                m,
                &mut self.surface,
                row,
                item.caption(&self.packages),
                item.about(&self.packages),
                row.index == self.row,
            );
        }

        self.damage = self.surface.bounds();
    }
}

/// Одна строка меню.
///
/// Свободной функцией, а не методом: строке нужна поверхность и ничего больше,
/// а подпись к ней лежит в том же `self`, что и поверхность, — методу пришлось
/// бы одолжить оба поля сразу.
fn draw_row(m: Metrics, s: &mut Surface, row: &MenuRow, label: &str, note: &str, selected: bool) {
    let ctx = m.ctx;
    let p = ctx.palette;
    let tile = Rect::new(
        row.rect.x + m.row_pad as i32,
        row.rect.y + (row.rect.h as i32 - m.tile as i32) / 2,
        m.tile,
        m.tile,
    );

    if row.danger {
        // Выключение подсвечивается своим цветом, а не общим: строка, которая
        // гасит машину, обязана отличаться от строки, которая открывает окно,
        // ещё до того, как её прочитали.
        if selected {
            draw::rounded(s, row.rect, m.round_row, p.badbg.color, p.badbg.alpha);
            draw::rounded_stroke(s, row.rect, m.round_row, p.badline, 255);
        }
        let r = ctx.px(theme::R_TAB);
        draw::rounded(s, tile, r, p.badbg.color, p.badbg.alpha);
        draw::rounded_stroke(s, tile, r, p.badline, 255);
        let side = m.tile * 4 / 7;
        glyphicon::draw(
            s,
            row.icon,
            tile.x + (m.tile as i32 - side as i32) / 2,
            tile.y + (m.tile as i32 - side as i32) / 2,
            side,
            p.bad_ink,
            255,
        );
    } else {
        if selected {
            paint::row(ctx, s, row.rect, RowState::Selected);
        }
        paint::icon_tile(ctx, s, tile, row.icon, row.tone, selected);
    }

    let role = if selected { Role::Title } else { Role::Body };
    let ink = if row.danger {
        p.bad_ink
    } else if selected {
        p.ink
    } else {
        p.ink2
    };
    let x = tile.right() + m.label_gap() as i32;
    let room = row.rect.w.saturating_sub((x - row.rect.x) as u32 + m.row_pad);
    let name_line = i32::from(ctx.face(role).line);
    let note_line = i32::from(ctx.face(Role::Caption).line);
    if note.is_empty() || (name_line + note_line) as u32 > row.rect.h {
        // Пояснения нет или ему не хватает высоты — подпись стоит по центру
        // строки. Втискивать вторую строку впритык нельзя: она сольётся с
        // соседней строкой списка, и список перестанет читаться как список.
        paint::text_clipped(ctx, s, role, x, paint::baseline(ctx, role, row.rect), room, label, ink);
        return;
    }
    let top = row.rect.y + (row.rect.h as i32 - name_line - note_line) / 2;
    paint::text_clipped(ctx, s, role, x, top, room, label, ink);
    paint::text_clipped(
        ctx,
        s,
        Role::Caption,
        x,
        top + name_line,
        room,
        note,
        if selected { p.ink3 } else { p.ink4 },
    );
}

/// Строка меню и её место в карточке.
struct MenuRow {
    rect: Rect,
    /// Номер строки — по нему же считается выделение.
    index: usize,
    icon: Icon,
    /// Цвет плитки, когда строка выбрана.
    tone: Tone,
    /// Строка гасит или перезапускает машину.
    danger: bool,
}

/// Всё, что нарисовано в карточке меню, посчитанное один раз.
struct MenuLayout {
    card: Rect,
    badge: Rect,
    name: (i32, i32),
    note: (i32, i32),
    caps: (i32, i32),
    rows: Vec<MenuRow>,
    rules: Vec<Rect>,
}

/// Имена программ из `/bin`, по алфавиту, без `init`.
///
/// Нужны меню дважды: чтобы не поставить строку программы, которой на диске
/// нет, и чтобы назвать в журнале, сколько программ осталось терминалу. `init`
/// не считается: это надзиратель за службами, он уже работает, и запускать его
/// по имени незачем.
fn list_programs() -> Vec<String> {
    const HIDDEN: [&str; 1] = ["init"];
    let Some(Ok(entries)) = crate::fs::list("/bin") else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.kind == crate::vfs::NodeKind::File)
        .map(|entry| entry.name)
        .filter(|name| !HIDDEN.contains(&name.as_str()))
        .collect();
    names.sort();
    names
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

/// Одно окно в глазах панели задач.
///
/// Структура, а не кортеж, и это перемена фазы 47a: подпись перестала
/// выводиться из [`App`] — у окна программы имя своё, — а кортеж из четырёх
/// полей на месте применения читается загадкой.
pub struct Entry {
    pub app: App,
    /// Как окно называется. Собственная строка, а не ссылка на окно: список
    /// переживает выход стола из-под замка, а окно за это время может
    /// закрыться.
    pub caption: String,
    pub focused: bool,
    pub minimized: bool,
}

/// Список окон для панели.
pub type Windows = Vec<Entry>;

/// Отпечаток того, что панель нарисовала в прошлый раз.
///
/// Сравнивается целиком: любое поле, попавшее сюда, — это то, что видно на
/// плашке. Забытое поле означает панель, застывшую на старом виде, поэтому
/// брать надо всё, что рисуется, и ничего сверх того.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    /// Свёртка списка окон: имена, фокус, свёрнутость.
    windows: u64,
    menu_open: bool,
    /// Часы и время работы — то, что видно в трее.
    clock: u64,
    /// Состояние сети и раскладка — остальные значки трея.
    net: u8,
    layout: u8,
    /// Тема могла смениться, а вместе с ней все цвета плашки.
    dark: bool,
}

impl Fingerprint {
    fn of(windows: &[Entry], menu_open: bool, status: &Status) -> Self {
        // Свёртка, а не копия списка: панель обновляют на каждое событие ввода,
        // и заводить вектор строк по шестьдесят пять раз в секунду ради
        // сравнения — это та же работа, от которой мы уходим.
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for entry in windows {
            for byte in entry.caption.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
            hash ^= u64::from(entry.focused) | (u64::from(entry.minimized) << 1);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        // Часы показываются с точностью до минуты, время работы — до секунды;
        // и то и другое приходит готовой строкой, поэтому сворачивается так же.
        let mut clock = 0xcbf2_9ce4_8422_2325u64;
        match status.clock.as_deref() {
            Some(text) => {
                for byte in text.as_bytes() {
                    clock ^= u64::from(*byte);
                    clock = clock.wrapping_mul(0x1000_0000_01b3);
                }
            }
            None => clock ^= status.uptime_ms / 1000,
        }
        Self {
            windows: hash,
            menu_open,
            clock,
            net: status.net as u8,
            layout: crate::input::keymap::layout() as u8,
            dark: theme::is_dark(),
        }
    }
}

/// Кнопки панели вместе с их местом.
type Buttons = Vec<(App, Rect)>;

/// Во что попал указатель на панели.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelHit {
    /// Кнопка меню.
    Menu,
    /// Кнопка окна.
    Window(App),
    /// Значок трея.
    Tray(TrayItem),
    /// Стопка свёрнутых окон в доке: тап показывает их списком.
    Stack,
    /// Кнопка дока, за которой программы пока нет.
    ///
    /// Телефон и камера в доке телефона стоят, а программ за ними нет — их
    /// некому написать, пока нет ни модема, ни камеры. Кнопка при этом не
    /// обманка: нажатие доходит до стола, и стол говорит вслух, чего именно
    /// нет. Промолчать было бы хуже — неотличимо от «не нажалось».
    Missing(&'static str),
    /// Пустое место плашки: щелчок туда не должен доставаться окну под ней.
    Empty,
}
