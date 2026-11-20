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
use core::sync::atomic::{AtomicBool, Ordering};

use mini_ui::draw;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::iconset::{self, Fi};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::theme::{self, Palette};
use super::window::App;
use super::deskcfg;
use super::startapps;

/// Надпись на кнопке меню.
const BRAND: &str = "FossAi";

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
fn desk_ctx(scale: mini_ui::Scale) -> Ctx {
    Ctx::scaled(scale).on(glass_bg())
}

/// Надпись в поле дока. Она же говорит, что поле делает: открывает список
/// приложений и ищет по нему.
const SEARCH_HINT: &str = "Поиск";

/// Подпись кнопки «Пуск» на телефоне. Имя системы, а не слово «Пуск»: кнопка
/// широкая, места хватает, и человек читает, чем он пользуется.
const DOCK_BRAND: &str = "FossAi";

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

    /// Ширина кнопки меню — она же начало всего, что стоит правее.
    ///
    /// С логотипом (72b) — квадратная кнопка со знаком, как остальные
    /// кнопки панели; без него — значок и подпись «FossAi».
    fn brand_width(self) -> u32 {
        if !theme::is_mobile() && super::appicon::logo().is_some() {
            return self.btn_h + self.ctx.px(4);
        }
        self.side * 2 + self.glyph() + self.gap + self.ctx.face(Role::Title).width(BRAND)
    }

    /// Ширина кнопки окна с полной подписью.
    ///
    /// Подпись приходит строкой, а не выводится из [`App`]: у окна программы
    /// имя своё, и спросить его у перечисления нечем.
    fn button_width(self, caption: &str, state: ButtonState) -> u32 {
        self.side + self.icon() + self.gap + self.ctx.face(state.role()).width(caption)
    }

    /// Сторона значка программы на кнопке панели задач.
    const fn icon(self) -> u32 {
        self.ctx.px(24)
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
    /// Сведения для карточки «Система» домашнего экрана телефона.
    pub home: super::home::Facts,
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
    /// Звук (02.11, макет 05).
    Volume,
    Layout,
    Clock,
    /// Колокол: уведомления и быстрые настройки (02.11, макет 05).
    Bell,
}

/// Значки трея вместе с их местом на плашке.
type Tray = Vec<(TrayItem, Rect)>;

/// Панель задач: полоса вдоль стороны экрана или плавающий Dock.
///
/// С 20.11 сторона, вид, автоскрытие и размер значков берутся из
/// [`deskcfg::Taskbar`]; панель пересобирается целиком, когда они меняются
/// (см. `Compositor::rebuild_taskbar`).
pub struct Panel {
    surface: Surface,
    /// Место плашки на экране сейчас — со сдвигом автоскрытия.
    ///
    /// Поверхность — ровно она, без запаса: тень под плашкой и срез углов
    /// делает композитор, и лишняя точка поверхности стала бы прямоугольником
    /// чужого оттенка поверх обоев.
    pub rect: Rect,
    /// Множитель геометрии стола.
    scale: mini_ui::Scale,
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
    /// Настройки, под которые панель собрана (на телефоне не читаются).
    cfg: deskcfg::Taskbar,
    /// Место монитора, у стороны которого стоит панель.
    area: Rect,
    /// Где плашка стоит, когда видна целиком.
    home: Rect,
    /// Насколько плашка уехала за край (автоскрытие): 0 — видна.
    slide: u32,
    /// Где плашка стояла до смены размера — композитор стирает это место.
    vacated: Rect,
    /// Закреплённые на панели программы.
    pins: Vec<PinInfo>,
    /// Поколение списка закреплённых, под которое собран `pins`.
    pin_gen: u32,
    /// Кнопки закреплённых программ без окон: номер в `pins` и место.
    pin_hits: Vec<(usize, Rect)>,
    /// Выезд или уход плашки (автоскрытие): откуда, куда, когда начат (мс).
    anim: Option<(u32, u32, u64)>,
    /// Когда спрятать плашку, если указатель так и не вернулся (мс); 0 — нет.
    hide_at: u64,
}

/// Сколько длится выезд плашки и сколько ждать, прежде чем её спрятать.
const SLIDE_MS: u64 = 160;
const HIDE_DELAY_MS: u64 = 450;

/// Закреплённая на панели программа.
struct PinInfo {
    /// Ключ закрепления (`taskbar_pins=`).
    key: String,
    /// По нему окна программы собираются в эту кнопку.
    group: String,
    caption: String,
    art: super::appicon::Art,
    choice: Choice,
}

/// Толщина вертикальной панели (точки при масштабе 1).
const PANEL_V_W: u32 = 56;
/// Поле Dock от края экрана и поле значков внутри плашки.
const DOCK_INSET: u32 = 8;
const DOCK_PAD: u32 = 8;
/// Зазор между значками Dock.
const DOCK_GAP: u32 = 6;

impl Panel {
    /// Сколько панель с этими настройками отнимает у экрана со своей
    /// стороны (без автоскрытия). Поля плавающего Dock принадлежат ему так
    /// же, как плашка: окно, доведённое до края рабочей области, не ложится
    /// под его тень.
    #[must_use]
    pub fn thickness(cfg: deskcfg::Taskbar, scale: mini_ui::Scale) -> u32 {
        if theme::is_mobile() {
            // Док у телефона выше панели и отстоит от края дальше: под нижним
            // краем экрана лежит место жеста «домой», а по бокам — скруглённые
            // углы. Кнопка, попавшая туда, видна и не нажимается.
            return (theme::M_DOCK_H + theme::M_INSET * 2) * scale;
        }
        match cfg.mode {
            deskcfg::Mode::Panel if cfg.edge.vertical() => PANEL_V_W * scale,
            deskcfg::Mode::Panel => (theme::PANEL_H + theme::PANEL_INSET * 2) * scale,
            deskcfg::Mode::Dock => (cfg.icon + DOCK_PAD * 2 + DOCK_INSET * 2) * scale,
        }
    }

    /// Сколько панель отнимает у рабочей области и с какой стороны. Скрытая
    /// панель не отнимает ничего: окна получают весь экран.
    #[must_use]
    pub fn reserve(&self) -> (deskcfg::Edge, u32) {
        if theme::is_mobile() {
            return (deskcfg::Edge::Bottom, Self::thickness(self.cfg, self.scale));
        }
        if self.cfg.autohide {
            return (self.cfg.edge, 0);
        }
        (self.cfg.edge, Self::thickness(self.cfg, self.scale))
    }

    /// Настройки, под которые собрана панель.
    #[must_use]
    pub const fn config(&self) -> deskcfg::Taskbar {
        self.cfg
    }

    /// Где плашка стоит видимой.
    #[must_use]
    pub const fn home(&self) -> Rect {
        self.home
    }

    /// Скругление углов плашки при выводе.
    #[must_use]
    pub fn corner(&self) -> u32 {
        if theme::is_mobile() {
            return Metrics::new(desk_ctx(self.scale)).round;
        }
        match self.cfg.mode {
            deskcfg::Mode::Panel => 0,
            deskcfg::Mode::Dock => desk_ctx(self.scale).px(14),
        }
    }

    /// Сколько надо сдвинуть плашку за край, чтобы её не было видно вовсе.
    #[must_use]
    pub fn hidden_slide(&self) -> u32 {
        let extra = desk_ctx(self.scale).px(DOCK_INSET + 28);
        if self.cfg.edge.vertical() { self.home.w + extra } else { self.home.h + extra }
    }

    /// Сдвинуть плашку за край на `slide` точек (автоскрытие). Возвращает
    /// прежнее место, если плашка сдвинулась.
    pub fn set_slide(&mut self, slide: u32) -> Option<Rect> {
        if slide == self.slide {
            return None;
        }
        let before = self.rect;
        self.slide = slide;
        self.rect = slid(self.home, self.cfg.edge, slide);
        Some(before)
    }

    #[must_use]
    pub const fn slide(&self) -> u32 {
        self.slide
    }

    /// Забрать место, которое плашка освободила при смене длины (Dock).
    pub fn take_vacated(&mut self) -> Rect {
        core::mem::replace(&mut self.vacated, Rect::EMPTY)
    }

    /// Панель у стороны `area` — места основного монитора на столе (на
    /// одном мониторе это весь экран от (0, 0)). Всё внутри панели — кнопки,
    /// трей, док — в её собственных координатах, поэтому сдвиг монитора
    /// касается одного `rect`.
    #[must_use]
    pub fn new(area: Rect, scale: mini_ui::Scale) -> Option<Self> {
        let scale = scale;
        let cfg = deskcfg::taskbar();
        let home = if theme::is_mobile() {
            let inset = theme::M_INSET * scale;
            let plate_h = theme::M_DOCK_H * scale;
            let plate_w = area.w.checked_sub(inset * 2).filter(|w| *w > 0)?;
            // Плашка стоит на нижнем поле той полосы, которую панель отнимает у
            // экрана: по этому же числу нижнюю границу рабочей области считает
            // композитор.
            let top = area.bottom() - Self::thickness(cfg, scale) as i32 + inset as i32;
            Rect::new(area.x + inset as i32, top, plate_w, plate_h)
        } else {
            plate_home(area, cfg, scale, None)?
        };
        let surface = Surface::new(home.w, home.h, glass_bg())?;
        let mut panel = Self {
            surface,
            rect: home,
            scale,
            damage: Rect::EMPTY,
            brand: Rect::EMPTY,
            buttons: Buttons::new(),
            tray: Tray::new(),
            mark: None,
            dock: None,
            cfg,
            area,
            home,
            slide: 0,
            vacated: Rect::EMPTY,
            pins: Vec::new(),
            pin_gen: u32::MAX,
            pin_hits: Vec::new(),
            anim: None,
            hide_at: 0,
        };
        if cfg.autohide && !theme::is_mobile() {
            let hidden = panel.hidden_slide();
            panel.set_slide(hidden);
        }
        Some(panel)
    }

    /// Куда плашка едет (или где стоит): 0 — видна.
    fn slide_target(&self) -> u32 {
        self.anim.map_or(self.slide, |(_, to, _)| to)
    }

    /// Указатель сдвинулся (автоскрытие). `busy` — над панелью открыто её
    /// меню, «Пуск» или миниатюры: тогда она не прячется.
    pub fn autohide_pointer(&mut self, x: i32, y: i32, busy: bool, now: u64) {
        if !self.cfg.autohide || theme::is_mobile() {
            return;
        }
        let area = self.area;
        // Край — две точки у стороны экрана, по всей её длине: в край
        // попадают с размаху, целиться не надо.
        let at_edge = match self.cfg.edge {
            deskcfg::Edge::Bottom => y >= area.bottom() - 2 && x >= area.x && x < area.right(),
            deskcfg::Edge::Top => y <= area.y + 1 && x >= area.x && x < area.right(),
            deskcfg::Edge::Left => x <= area.x + 1 && y >= area.y && y < area.bottom(),
            deskcfg::Edge::Right => x >= area.right() - 2 && y >= area.y && y < area.bottom(),
        };
        let margin = desk_ctx(self.scale).px(12) as i32;
        let near = Rect::new(self.home.x - margin, self.home.y - margin, self.home.w + margin as u32 * 2, self.home.h + margin as u32 * 2);
        let shown = self.slide_target() == 0;
        if at_edge || busy || (shown && near.contains(x, y)) {
            self.hide_at = 0;
            if !shown {
                self.start_slide(0, now);
                crate::kprintln!("  desktop     : taskbar revealed");
            }
        } else if shown && self.hide_at == 0 {
            self.hide_at = now + HIDE_DELAY_MS;
        }
    }

    /// Держать плашку на экране (открыт «Пуск» или меню панели).
    pub fn hold(&mut self, now: u64) {
        if !self.cfg.autohide || theme::is_mobile() {
            return;
        }
        self.hide_at = 0;
        if self.slide_target() != 0 {
            self.start_slide(0, now);
        }
    }

    fn start_slide(&mut self, to: u32, now: u64) {
        if self.slide_target() == to {
            return;
        }
        self.anim = Some((self.slide, to, now));
    }

    /// Шаг автоскрытия перед кадром: место, которое надо перерисовать.
    pub fn autohide_tick(&mut self, now: u64) -> Option<Rect> {
        if !self.cfg.autohide || theme::is_mobile() {
            return None;
        }
        if self.hide_at != 0 && now >= self.hide_at {
            self.hide_at = 0;
            let hidden = self.hidden_slide();
            self.start_slide(hidden, now);
            crate::kprintln!("  desktop     : taskbar hidden");
        }
        let (from, to, started) = self.anim?;
        let t = (now.saturating_sub(started)).min(SLIDE_MS);
        // Плавно в конце: квадратичное замедление.
        let k = 1000 - ((SLIDE_MS - t) * (SLIDE_MS - t) * 1000) / (SLIDE_MS * SLIDE_MS);
        let value = (i64::from(from) + (i64::from(to) - i64::from(from)) * k as i64 / 1000) as u32;
        if t >= SLIDE_MS {
            self.anim = None;
        }
        let before = self.set_slide(if self.anim.is_none() { to } else { value })?;
        Some(before.union(&self.rect))
    }

    /// Ждёт ли автоскрытие кадров (едет или ждёт срока).
    #[must_use]
    pub fn autohide_pending(&self) -> bool {
        self.anim.is_some() || self.hide_at != 0
    }

    /// Видна ли плашка хоть немного.
    #[must_use]
    pub fn visible(&self) -> bool {
        self.slide < self.hidden_slide()
    }

    /// Перечитать закреплённые программы, если список сменился.
    fn sync_pins(&mut self) {
        let generation = deskcfg::task_pins_generation();
        if generation == self.pin_gen {
            return;
        }
        self.pin_gen = generation;
        self.pins.clear();
        let keys = deskcfg::task_pins_for(self.cfg.mode);
        if keys.is_empty() {
            return;
        }
        let packages = if keys.iter().any(|key| key.starts_with("pkg:")) {
            let mut packages = list_packages();
            let windows = windows_launchers(&packages);
            packages.extend(windows);
            packages
        } else {
            Vec::new()
        };
        for key in keys {
            if let Some(entry) = super::startapps::entry(&key) {
                if !entry_present(entry) {
                    continue;
                }
                self.pins.push(PinInfo {
                    key: String::from(entry.key),
                    group: String::from(if entry.group.is_empty() { entry.key } else { entry.group }),
                    caption: String::from(entry.caption),
                    art: super::startapps::art(entry),
                    choice: entry_choice(entry),
                });
            } else if let Some(name) = key.strip_prefix("pkg:") {
                if let Some(launcher) = packages.iter().find(|launcher| launcher.name == name) {
                    let program = launcher.start.split_whitespace().next().unwrap_or("");
                    self.pins.push(PinInfo {
                        key: key.clone(),
                        group: String::from(program),
                        caption: launcher.caption.clone(),
                        art: super::appicon::of_manifest(&launcher.icon),
                        choice: Choice::Command(launcher.start.clone(), launcher.rights),
                    });
                }
            }
        }
    }

    /// Закреплённая программа под кнопкой `index` (щелчок по ней без окна).
    #[must_use]
    pub fn pin_choice(&self, index: usize) -> Option<Choice> {
        self.pins.get(index).map(|pin| pin.choice.clone())
    }

    /// Ключ закреплённой кнопки — для «Открепить».
    #[must_use]
    pub fn pin_key(&self, index: usize) -> Option<String> {
        self.pins.get(index).map(|pin| pin.key.clone())
    }

    /// Подпись закреплённой кнопки — для журнала.
    #[must_use]
    pub fn pin_caption(&self, index: usize) -> &str {
        self.pins.get(index).map_or("", |pin| pin.caption.as_str())
    }

    /// Нарисовать док телефона по макету (`FreeOS-mobile`, экраны 01 и 09).
    ///
    /// Без свёрнутых окон: телефон, «FreeOS» с подписью, поиск во всю
    /// оставшуюся ширину, камера. Со свёрнутыми: телефон, «Пуск» плиткой,
    /// стопка свёрнутых во всю оставшуюся ширину, поиск плиткой, камера.
    ///
    /// Кнопок открытых окон здесь нет намеренно. На телефоне окно одно и во
    /// весь экран — переключать нечего; свёрнутые собраны в стопку.
    ///
    /// За телефоном и камерой программ пока нет, и кнопки всё равно стоят:
    /// место под них в доке — это решение о раскладке, а не обещание. Нажатие
    /// доходит до стола и называет вслух, чего именно нет.
    fn redraw_dock(&mut self, m: Metrics, plate: Rect, menu_open: bool, windows: &[Entry]) {
        let ctx = m.ctx;
        let p = ctx.palette;
        let minimized: Vec<App> =
            windows.iter().filter(|entry| entry.minimized).map(|entry| entry.app).collect();
        let layout = dock_layout(m, plate, minimized.len());
        let radius = ctx.px(16);

        for (rect, icon) in [(layout.left, Icon::Phone), (layout.right, Icon::Camera)] {
            dock_button(&mut self.surface, ctx, rect);
            centered_icon(&mut self.surface, rect, icon, ctx.px(21), p.ink2);
        }

        // «Пуск». Открытое меню переворачивает градиент — кнопка выглядит
        // вдавленной; то же правило, что у настольной панели.
        let (top, bottom) = if menu_open { (p.acc2, p.acc) } else { (p.acc, p.acc2) };
        if layout.brand_square.is_empty() {
            // Широкая: тёмная пилюля, внутри у левого края синяя плитка 40 со
            // значком, справа от неё имя системы. Прежде синей была вся кнопка
            // и подпись упиралась в её край: в макете ширина считается от
            // подписи, а не от доли дока.
            dock_button(&mut self.surface, ctx, layout.brand);
            draw::rounded_gradient(&mut self.surface, layout.chip, ctx.px(13), top, bottom, 255);
            draw::rounded_stroke(
                &mut self.surface,
                layout.chip,
                ctx.px(13),
                p.accline.color,
                p.accline.alpha,
            );
            draw_grid(&mut self.surface, layout.chip, ctx.px(18), WHITE);
            let text_x = layout.chip.right() + ctx.px(9) as i32;
            paint::text_clipped(
                ctx,
                &mut self.surface,
                Role::Title,
                text_x,
                paint::baseline(ctx, Role::Title, layout.brand),
                layout.brand.right().saturating_sub(text_x).max(0) as u32,
                DOCK_BRAND,
                p.ink,
            );
        } else {
            draw::rounded_gradient(&mut self.surface, layout.brand_square, radius, top, bottom, 255);
            draw::rounded_stroke(
                &mut self.surface,
                layout.brand_square,
                radius,
                p.accline.color,
                p.accline.alpha,
            );
            draw_grid(&mut self.surface, layout.brand_square, ctx.px(18), WHITE);
        }

        if !layout.stack.is_empty() {
            self.draw_stack(ctx, layout.stack, &minimized);
        }

        // Поиск: полем с подписью, пока места хватает, плиткой с лупой — когда
        // док делит место со стопкой.
        dock_button(&mut self.surface, ctx, layout.search);
        let lens = ctx.px(15);
        if layout.stack.is_empty() {
            let icon_x = layout.search.x + ctx.px(12) as i32;
            glyphicon::draw(
                &mut self.surface,
                Icon::Search,
                icon_x,
                layout.search.y + (layout.search.h as i32 - lens as i32) / 2,
                lens,
                p.ink4,
                255,
            );
            // Подпись — только если помещается целиком. Обрезанная до «По…»
            // она ничего не сообщает, а лупа рядом говорит то же без слов.
            let text_x = icon_x + (lens + ctx.px(7)) as i32;
            let room =
                layout.search.right().saturating_sub(text_x + ctx.px(12) as i32).max(0) as u32;
            if room >= ctx.face(Role::Body).width(SEARCH_HINT) {
                paint::text_clipped(
                    ctx,
                    &mut self.surface,
                    Role::Body,
                    text_x,
                    paint::baseline(ctx, Role::Body, layout.search),
                    room,
                    SEARCH_HINT,
                    p.ink4,
                );
            }
        } else {
            centered_icon(&mut self.surface, layout.search, Icon::Search, lens, p.ink4);
        }

        // Полоска жеста. Она лежит ниже плашки — там, где у этого экрана и так
        // ничего не помещается, — и служит меткой низа, а не кнопкой.
        draw::rounded(&mut self.surface, layout.home, layout.home.h / 2, p.ink6, 160);

        self.dock = Some(layout);
        self.brand = layout.search;
        self.buttons = Buttons::new();
        self.tray = Tray::new();
    }

    /// Стопка свёрнутых окон (макет, экран 09): утопленное поле с кольцом
    /// акцента, плитки программ внахлёст и счётчик в углу.
    ///
    /// Плитка несёт значок и цвет **своей** программы: стопка из одинаковых
    /// синих квадратов говорила бы только «что-то свёрнуто», а по ней надо
    /// узнать, что именно. Показывается не больше четырёх — пятая уже не
    /// читается, для неё есть счётчик.
    ///
    /// Поворота плиток, как в макете, пока нет: он придёт вместе с анимацией
    /// сворачивания, которой нужно то же умение.
    fn draw_stack(&mut self, ctx: Ctx, stack: Rect, apps: &[App]) {
        let p = ctx.palette;
        let radius = ctx.px(16);
        // Кольцо снаружи — мягкое, внутри — чёткое: поле выглядит местом, куда
        // «сложены» окна, а не ещё одной кнопкой.
        let glow = ctx.px(3);
        let halo = Rect::new(
            stack.x - glow as i32,
            stack.y - glow as i32,
            stack.w + glow * 2,
            stack.h + glow * 2,
        );
        draw::rounded(&mut self.surface, halo, radius + glow, p.acctint.color, p.acctint.alpha);
        draw::rounded(&mut self.surface, stack, radius, ctx.flat(p.sunk), 255);
        draw::rounded_stroke(&mut self.surface, stack, radius, p.acc, 255);

        let shown = &apps[apps.len().saturating_sub(4)..];
        let tile = ctx.px(30);
        let step = ctx.px(16);
        let total = tile + step * shown.len().saturating_sub(1) as u32;
        let base_x = stack.x + (stack.w as i32 - total as i32) / 2;
        let y = stack.y + (stack.h as i32 - tile as i32) / 2;
        let glyph = ctx.px(15);
        for (index, app) in shown.iter().enumerate() {
            let rect = Rect::new(base_x + (index as u32 * step) as i32, y, tile, tile);
            let r = ctx.px(10);
            let ink = match app.tone() {
                Tone::Ok => {
                    draw::rounded_gradient(&mut self.surface, rect, r, p.ok, p.ok2, 255);
                    WHITE
                }
                Tone::Bad => {
                    draw::rounded(&mut self.surface, rect, r, p.bad, 255);
                    WHITE
                }
                Tone::Accent if index + 1 == shown.len() => {
                    draw::rounded_gradient(&mut self.surface, rect, r, p.acc, p.acc2, 255);
                    WHITE
                }
                _ => {
                    // Подложка кнопки поверх утопленного поля — непрозрачной:
                    // иначе сквозь плитку просвечивала бы соседняя под ней.
                    draw::rounded(&mut self.surface, rect, r, ctx.flat(p.btn), 255);
                    draw::rounded_stroke(&mut self.surface, rect, r, p.btnline.color, p.btnline.alpha);
                    p.ink3
                }
            };
            let icon = if app.is_terminal() { Icon::Prompt } else { app.icon() };
            centered_icon(&mut self.surface, rect, icon, glyph, ink);
        }

        // Счётчик — в углу поля, чуть за его краем, как в макете.
        let count = alloc::format!("{}", apps.len());
        let face = ctx.face(Role::Caption);
        let badge_h = ctx.px(18);
        let badge_w = (face.width(&count) + ctx.px(10)).max(badge_h);
        let badge = Rect::new(
            stack.right() + ctx.px(5) as i32 - badge_w as i32,
            stack.y - ctx.px(5) as i32,
            badge_w,
            badge_h,
        );
        draw::rounded(&mut self.surface, badge, badge_h / 2, p.acc, 255);
        paint::text(
            ctx,
            &mut self.surface,
            Role::Caption,
            badge.x + (badge_w as i32 - face.width(&count) as i32) / 2,
            paint::baseline(ctx, Role::Caption, badge),
            &count,
            WHITE,
        );
    }

    /// Куда сворачивается окно этой программы — в координатах экрана.
    ///
    /// На телефоне это стопка (а пока её нет — «Пуск»), на столе — кнопка окна
    /// на панели задач. `None` — кнопки у окна нет (не поместилась).
    #[must_use]
    /// Где значок трея `item` на экране (72h-1: над ним — его окно).
    #[must_use]
    pub fn tray_rect(&self, item: TrayItem) -> Option<Rect> {
        self.tray.iter().find(|(known, _)| *known == item).map(|(_, rect)| rect.translate(self.rect.x, self.rect.y))
    }

    pub fn anchor(&self, app: App) -> Option<Rect> {
        let local = if let Some(dock) = self.dock {
            if !dock.stack.is_empty() {
                dock.stack
            } else if !dock.brand.is_empty() {
                dock.brand
            } else {
                dock.brand_square
            }
        } else {
            self.buttons.iter().find(|(owner, _)| *owner == app).map(|(_, rect)| *rect)?
        };
        Some(local.translate(self.rect.x, self.rect.y))
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
            if (x >= dock.brand.x && x < dock.brand.right())
                || (x >= dock.brand_square.x && x < dock.brand_square.right())
            {
                return Some(PanelHit::Menu);
            }
            if !dock.stack.is_empty() && x >= dock.stack.x && x < dock.stack.right() {
                return Some(PanelHit::Stack);
            }
            if x >= dock.search.x && x < dock.search.right() {
                return Some(PanelHit::Search);
            }
            if x >= dock.right.x && x < dock.right.right() {
                return Some(PanelHit::Missing("camera"));
            }
            return Some(PanelHit::Empty);
        }
        // Вдоль полосы — ровно как нарисовано, поперёк — на всю плашку:
        // промах мимо кнопки на пару точек читается как «не нажалось», а
        // между кнопкой и краем плашки лежит одно пустое поле, которым больше
        // никто не занят.
        let vertical = self.cfg.edge.vertical();
        let along = |rect: &Rect| if vertical { y >= rect.y && y < rect.bottom() } else { x >= rect.x && x < rect.right() };
        if along(&self.brand) {
            return Some(PanelHit::Menu);
        }
        for (app, rect) in &self.buttons {
            if along(rect) {
                return Some(PanelHit::Window(*app));
            }
        }
        for (index, rect) in &self.pin_hits {
            if along(rect) {
                return Some(PanelHit::Pin(*index));
            }
        }
        for (item, rect) in &self.tray {
            if along(rect) {
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
        if !theme::is_mobile() {
            let before = self.pin_gen;
            self.sync_pins();
            if before != self.pin_gen {
                self.mark = None;
            }
        }
        let mark = Fingerprint::of(windows, menu_open, status);
        if self.mark == Some(mark) {
            return;
        }
        self.mark = Some(mark);
        // Dock и панель у боковой стороны — своя раскладка, вдоль оси полосы.
        if !theme::is_mobile() && (self.cfg.mode == deskcfg::Mode::Dock || self.cfg.edge.vertical()) {
            self.redraw_strip(windows, menu_open, status);
            return;
        }
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        let p = ctx.palette;
        let plate = self.surface.bounds();

        // Заливка — во всю поверхность, а не по скруглённой фигуре: углы срежет
        // композитор, смешав их с обоями, и нарисованное здесь скругление
        // означало бы, что углы срезаются дважды.
        self.surface.fill(plate, ctx.under);
        if theme::is_mobile() {
            draw::rounded_stroke(&mut self.surface, plate, m.round, p.line3.color, p.line3.alpha);
            draw::crown(&mut self.surface, plate, m.round, p.crown.color, p.crown.alpha);
        } else {
            // Панель во всю ширину (72b): черта только со стороны стола, как
            // в Windows, — сверху у нижней панели, снизу у верхней.
            let y = if self.cfg.edge == deskcfg::Edge::Top { plate.h as i32 - 1 } else { 0 };
            draw::hline(&mut self.surface, 0, y, plate.w, p.line2.color, p.line2.alpha);
        }

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
        let clock = clock_line(status);
        let date = date_line(status);
        // Раскладка — как в трее Windows: «РУС» и «ENG».
        let lang = match crate::input::keymap::layout() {
            crate::input::keymap::Layout::Ru => "РУС",
            crate::input::keymap::Layout::Us => "ENG",
        };
        let mono = ctx.face(Role::Mono);
        let body = ctx.face(Role::Body);
        // Шаг между значками трея — шире зазора кнопок: значки не кнопки в
        // ряд, а три отдельных индикатора, и слипшиеся они читались бы как
        // одна надпись «RU 12:30».
        let step = ctx.px(14);
        let glyph = m.glyph();
        let caption = ctx.face(Role::Caption);
        let clock_w = body.width(&clock).max(date.as_deref().map_or(0, |date| caption.width(date)));
        let _ = mono;
        // Сеть, звук, раскладка, часы, колокол (макет 05).
        let mut status_w = (glyph + step) * 2 + body.width(lang) + step + clock_w + step + glyph;
        // Совсем узкий экран: показать всё нельзя, и тогда правого края нет
        // вовсе. Обрезанное наполовину число хуже отсутствующего — по нему не
        // понять, что именно обрезано.
        if status_w + m.brand_width() + m.plate_pad() * 2 + m.gap * 2 > plate.w {
            status_w = 0;
        }

        let layout = panel_layout(m, plate, windows, status_w, &self.pins);
        self.brand = layout.brand;
        self.buttons = layout.buttons.clone();
        self.pin_hits = layout.pin_hits.clone();

        // Кнопка меню. Открытое меню переворачивает градиент: кнопка выглядит
        // вдавленной, и это единственное, чем «меню открыто» отличается от
        // «меню закрыто» — заливка другим цветом читалась бы как другая кнопка.
        // С логотипом (фаза 72a) кнопка — знак FossAi на прозрачной плашке,
        // как «Пуск» в макете; открытое меню — плашка подсвечена.
        let logo = super::appicon::logo();
        let glyph = m.glyph();
        let ink = if logo.is_some() { p.ink } else { WHITE };
        match &logo {
            Some(logo) => {
                if menu_open {
                    draw::rounded(&mut self.surface, layout.brand, m.round_row, p.hover2.color, p.hover2.alpha);
                    draw::rounded_stroke(&mut self.surface, layout.brand, m.round_row, p.accline.color, p.accline.alpha);
                }
                let side = m.icon();
                let at = Rect::new(
                    layout.brand.x + (layout.brand.w as i32 - side as i32) / 2,
                    layout.brand.y + (layout.brand.h as i32 - side as i32) / 2,
                    side,
                    side,
                );
                logo.draw(&mut self.surface, at);
            }
            None => {
                let (top, bottom) = if menu_open { (p.acc2, p.acc) } else { (p.acc, p.acc2) };
                draw::rounded_gradient(&mut self.surface, layout.brand, m.round_row, top, bottom, 255);
                draw::rounded_stroke(&mut self.surface, layout.brand, m.round_row, p.accline.color, p.accline.alpha);
                glyphicon::draw(
                    &mut self.surface,
                    Icon::Grid,
                    layout.brand.x + m.side as i32,
                    layout.brand.y + (layout.brand.h as i32 - glyph as i32) / 2,
                    glyph,
                    WHITE,
                    255,
                );
            }
        }
        if logo.is_none() {
            paint::text(
                ctx,
                &mut self.surface,
                Role::Title,
                layout.brand.x + (m.side + glyph + m.gap) as i32,
                paint::baseline(ctx, Role::Title, layout.brand),
                BRAND,
                ink,
            );
        }

        draw::vline(
            &mut self.surface,
            layout.divider.x,
            layout.divider.y,
            layout.divider.h,
            p.line2.color,
            p.line2.alpha,
        );

        let edge = self.cfg.edge;
        for slot in &layout.slots {
            self.draw_slot(ctx, m, slot, windows, edge);
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
            // Проводная сеть — монитор, как значок Ethernet в Windows (24.10:
            // глобус с крестом читался как «нет сети» и при сети).
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

            // Звук: перечёркнутый динамик — карты нет или звук на нуле.
            let quiet = !crate::sound::present() || crate::sound::volume() == 0;
            iconset::draw(
                &mut self.surface,
                if quiet { Fi::VolumeOff } else { Fi::Volume },
                x,
                plate.y + (plate.h as i32 - glyph as i32) / 2,
                glyph,
                if crate::sound::present() { p.ink } else { p.ink3 },
                255,
            );
            self.tray.push((TrayItem::Volume, Rect::new(x - half, plate.y, glyph + step, plate.h)));
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

            // Часы и дата в две строки, прижаты вправо, как в Windows.
            let right = x + clock_w as i32;
            match &date {
                Some(date) => {
                    let line = i32::from(caption.line);
                    let top = plate.y + (plate.h as i32 - i32::from(body.line) - line) / 2;
                    paint::text_right(ctx, &mut self.surface, Role::Body, right, top, &clock, p.ink);
                    paint::text_right(ctx, &mut self.surface, Role::Caption, right, top + i32::from(body.line), date, p.ink3);
                }
                None => paint::text_right(ctx, &mut self.surface, Role::Body, right, paint::baseline(ctx, Role::Body, plate), &clock, p.ink),
            }
            self.tray.push((
                TrayItem::Clock,
                Rect::new(x - half, plate.y, clock_w + step, plate.h),
            ));
            // Колокол — крайний справа, как в макете.
            let bell_x = right + step as i32;
            iconset::draw(&mut self.surface, Fi::Bell, bell_x, plate.y + (plate.h as i32 - glyph as i32) / 2, glyph, p.ink2, 255);
            self.tray.push((TrayItem::Bell, Rect::new(bell_x - half, plate.y, glyph + step, plate.h)));
        }

        self.damage = self.surface.bounds();
    }

    /// Кнопка панели задач: значок программы (с подписью, если панель
    /// подписана) и полоска под ним, как в Windows 11 — длинная акцентная у
    /// активного окна, короткая серая у открытых и свёрнутых. Кнопка, за
    /// которой несколько окон, лежит стопкой: за ней край второй плашки.
    fn draw_slot(&mut self, ctx: Ctx, m: Metrics, slot: &Slot, windows: &[Entry], edge: deskcfg::Edge) {
        let p = ctx.palette;
        let rect = slot.rect;
        let members: Vec<&Entry> = slot.members.iter().filter_map(|index| windows.get(*index)).collect();
        // Закреплённая программа без окон — только её значок, без полоски.
        let Some(first) = members.first() else {
            let Some(pin) = slot.pin.and_then(|index| self.pins.get(index)) else {
                return;
            };
            let side = slot.icon.min(rect.w).min(rect.h);
            let icon_rect = Rect::new(rect.x + (rect.w as i32 - side as i32) / 2, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            pin.art.draw(ctx, &mut self.surface, icon_rect);
            return;
        };
        let active = members.iter().any(|entry| entry.focused && !entry.minimized);
        let all_minimized = members.iter().all(|entry| entry.minimized);
        if members.len() > 1 {
            // Стопка: край второй плашки правее (ниже у боковой полосы).
            let shift = ctx.px(3) as i32;
            let behind = if edge.vertical() { rect.translate(0, shift) } else { rect.translate(shift, 0) };
            draw::rounded(&mut self.surface, behind, m.round_row, ctx.flat(p.ghost), 255);
            draw::rounded_stroke(&mut self.surface, behind, m.round_row, p.btnline.color, p.btnline.alpha);
        }
        if active || members.len() > 1 {
            let edge_line = if active { p.btnline } else { p.line2 };
            draw::rounded(&mut self.surface, rect, m.round_row, ctx.flat(p.ghost), 255);
            draw::rounded_stroke(&mut self.surface, rect, m.round_row, edge_line.color, edge_line.alpha);
        }
        let side = slot.icon.min(rect.w).min(rect.h);
        let labelled = !slot.label.is_empty();
        let icon_x = if labelled { rect.x + m.side as i32 / 2 } else { rect.x + (rect.w as i32 - side as i32) / 2 };
        let icon_rect = Rect::new(icon_x, rect.y + (rect.h as i32 - side as i32) / 2 - ctx.px(1) as i32, side, side);
        // Окно закреплённой программы рисуется её значком: у окна программы
        // значок общий (`dotnet`, `mono`), у закрепления — свой.
        match slot.pin.and_then(|index| self.pins.get(index)) {
            Some(pin) => pin.art.draw(ctx, &mut self.surface, icon_rect),
            None => first.art.draw(ctx, &mut self.surface, icon_rect),
        }
        if labelled {
            let state = ButtonState::of(active, all_minimized);
            let role = state.role();
            let x = icon_rect.right() + m.gap as i32;
            paint::text_clipped(
                ctx,
                &mut self.surface,
                role,
                x,
                paint::baseline(ctx, role, rect),
                (rect.right() - x - (m.side / 2) as i32).max(0) as u32,
                &slot.label,
                state.ink(p),
            );
        }
        // Полоска состояния — у края кнопки, что смотрит к краю экрана.
        // Как в макете (02): у активного — акцентная 24×3, у прочих — серая
        // 6×3.
        let (length, color) = if active { (ctx.px(18).min(rect.w).min(rect.h), p.acc) } else { (ctx.px(6), p.ink5) };
        let thick = ctx.px(3);
        let inset = ctx.px(1) as i32;
        let center_x = if labelled { icon_rect.x + side as i32 / 2 } else { rect.x + rect.w as i32 / 2 };
        let center_y = rect.y + rect.h as i32 / 2;
        let bar = match edge {
            deskcfg::Edge::Bottom => Rect::new(center_x - length as i32 / 2, rect.bottom() - thick as i32 - inset, length, thick),
            deskcfg::Edge::Top => Rect::new(center_x - length as i32 / 2, rect.y + inset, length, thick),
            deskcfg::Edge::Left => Rect::new(rect.x + inset, center_y - length as i32 / 2, thick, length),
            deskcfg::Edge::Right => Rect::new(rect.right() - thick as i32 - inset, center_y - length as i32 / 2, thick, length),
        };
        draw::rounded(&mut self.surface, bar, (thick / 2).max(1), color, 255);
    }

    /// Размеры полосы вдоль оси: сторона кнопки, сторона значка, поле от
    /// концов, зазор и толщина плашки.
    fn strip_sizes(&self, m: Metrics) -> (u32, u32, u32, u32, u32) {
        let ctx = m.ctx;
        match self.cfg.mode {
            deskcfg::Mode::Dock => {
                let button = ctx.px(self.cfg.icon);
                let icon = button.saturating_sub(ctx.px(8));
                (button, icon, ctx.px(DOCK_PAD), ctx.px(DOCK_GAP), button + ctx.px(DOCK_PAD) * 2)
            }
            deskcfg::Mode::Panel => {
                let thick = ctx.px(PANEL_V_W);
                (m.btn_h, m.icon(), (thick - m.btn_h) / 2, ctx.px(4), thick)
            }
        }
    }

    /// Протяжённость значков трея вдоль полосы: сеть, звук, раскладка, часы,
    /// колокол.
    fn strip_tray(&self, m: Metrics, status: &Status) -> Vec<(TrayItem, u32)> {
        let ctx = m.ctx;
        let vertical = self.cfg.edge.vertical();
        let step = ctx.px(if vertical { 10 } else { 12 });
        let glyph = m.glyph();
        let caption = ctx.face(Role::Caption);
        let body = ctx.face(Role::Body);
        let lang = lang_tag();
        let (time, date) = short_clock(status);
        let mut out = alloc::vec![(TrayItem::Network, glyph + step), (TrayItem::Volume, glyph + step)];
        if vertical {
            out.push((TrayItem::Layout, u32::from(caption.line) + step));
            let lines = if date.is_empty() { 1 } else { 2 };
            out.push((TrayItem::Clock, u32::from(caption.line) * lines + step));
        } else {
            out.push((TrayItem::Layout, caption.width(lang) + step));
            out.push((TrayItem::Clock, body.width(&time).max(caption.width(&date)) + step));
        }
        out.push((TrayItem::Bell, glyph + step));
        out
    }

    /// Dock и панель у боковой стороны экрана: кнопка «Пуск», закреплённые и
    /// окна, черта, трей — вдоль оси полосы.
    fn redraw_strip(&mut self, windows: &[Entry], menu_open: bool, status: &Status) {
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        let p = ctx.palette;
        let vertical = self.cfg.edge.vertical();
        let dock = self.cfg.mode == deskcfg::Mode::Dock;
        let (button, icon, pad, gap, thick) = self.strip_sizes(m);
        let tray = self.strip_tray(m, status);
        let tray_len: u32 = tray.iter().map(|(_, len)| *len).sum();
        let plan = plan_slots(windows, &self.pins, false);

        // Длина Dock — по содержимому; панель — во всю сторону.
        let side_len = if vertical { self.area.h } else { self.area.w };
        let room = side_len.saturating_sub(ctx.px(DOCK_INSET) * 2);
        let fixed = pad * 2 + button + gap + gap * 2 + 1 + tray_len;
        let fits = ((room.saturating_sub(fixed)) / (button + gap)) as usize;
        let shown = plan.len().min(fits);
        if dock {
            let length = (fixed + shown as u32 * (button + gap)).min(room).max(button + pad * 2);
            let current = if vertical { self.home.h } else { self.home.w };
            if length != current {
                if let Some(home) = plate_home(self.area, self.cfg, self.scale, Some(length)) {
                    if let Some(surface) = Surface::new(home.w, home.h, glass_bg()) {
                        self.vacated = self.vacated.union(&self.rect);
                        self.surface = surface;
                        self.home = home;
                        self.rect = slid(home, self.cfg.edge, self.slide);
                    }
                }
            }
        }
        let plate = self.surface.bounds();
        let length = if vertical { plate.h } else { plate.w };
        let cross = if vertical { plate.w } else { plate.h };
        let _ = thick;
        // Прямоугольник по месту вдоль оси и протяжённости; поперёк — по
        // центру, сторона `across`.
        let place = |at: u32, len: u32, across: u32| -> Rect {
            let c = ((cross - across.min(cross)) / 2) as i32;
            if vertical { Rect::new(c, at as i32, across, len) } else { Rect::new(at as i32, c, len, across) }
        };

        self.surface.fill(plate, ctx.under);
        if dock {
            let corner = self.corner();
            draw::rounded_stroke(&mut self.surface, plate, corner, p.line3.color, p.line3.alpha);
            draw::crown(&mut self.surface, plate, corner, p.crown.color, p.crown.alpha);
        } else {
            // Черта — со стороны стола.
            match self.cfg.edge {
                deskcfg::Edge::Left => draw::vline(&mut self.surface, plate.w as i32 - 1, 0, plate.h, p.line2.color, p.line2.alpha),
                _ => draw::vline(&mut self.surface, 0, 0, plate.h, p.line2.color, p.line2.alpha),
            }
        }

        // «Пуск».
        let mut at = pad;
        let brand = place(at, button, button);
        self.brand = brand;
        if dock || menu_open {
            let fill = if menu_open { p.hover2 } else { p.btn };
            draw::rounded(&mut self.surface, brand, m.round_row, fill.color, fill.alpha);
            let line = if menu_open { p.accline } else { p.btnline };
            draw::rounded_stroke(&mut self.surface, brand, m.round_row, line.color, line.alpha);
        }
        match super::appicon::logo() {
            Some(logo) => {
                let side = if dock { button * 3 / 5 } else { m.icon() };
                logo.draw(&mut self.surface, Rect::new(brand.x + (brand.w as i32 - side as i32) / 2, brand.y + (brand.h as i32 - side as i32) / 2, side, side));
            }
            None => draw_grid(&mut self.surface, brand, m.glyph() + ctx.px(4), p.acc),
        }
        at += button + gap;
        if !dock {
            // Черта после «Пуска», как у горизонтальной панели.
            let rule = place(at, 1, ctx.px(24));
            if vertical {
                draw::hline(&mut self.surface, rule.x, rule.y, rule.w, p.line2.color, p.line2.alpha);
            }
            at += gap + 1;
        }

        // Кнопки программ.
        let mut buttons = Buttons::new();
        let mut pin_hits = Vec::new();
        let mut slots = Vec::new();
        let end = length.saturating_sub(pad + tray_len + gap * 2 + 1);
        for (members, pin, label) in plan.into_iter() {
            if at + button > end {
                break;
            }
            let rect = place(at, button, button);
            for index in &members {
                buttons.push((windows[*index].app, rect));
            }
            if members.is_empty() {
                if let Some(pin) = pin {
                    pin_hits.push((pin, rect));
                }
            }
            slots.push(Slot { rect, members, pin, label, icon });
            at += button + gap;
        }
        let edge = self.cfg.edge;
        for slot in &slots {
            self.draw_slot(ctx, m, slot, windows, edge);
        }
        self.buttons = buttons;
        self.pin_hits = pin_hits;

        // Трей — у дальнего конца: у Dock сразу за кнопками, у панели — в
        // самом конце стороны.
        let mut at = if dock { at + gap } else { length.saturating_sub(pad + tray_len) };
        let rule = place(at.saturating_sub(gap + 1), 1, ctx.px(24));
        if vertical {
            draw::hline(&mut self.surface, rule.x, rule.y, rule.w, p.line2.color, p.line2.alpha);
        } else {
            let rule = place(at.saturating_sub(gap + 1), 1, ctx.px(24));
            draw::vline(&mut self.surface, rule.x, rule.y, rule.h, p.line2.color, p.line2.alpha);
        }
        self.tray.clear();
        let glyph = m.glyph();
        let (time, date) = short_clock(status);
        for (item, len) in tray {
            let rect = place(at, len, cross);
            let center = |size: u32| (rect.x + (rect.w as i32 - size as i32) / 2, rect.y + (rect.h as i32 - size as i32) / 2);
            match item {
                TrayItem::Network => {
                    let ink = if status.net == NetState::Up { p.ink } else { p.ink3 };
                    let (x, y) = center(glyph);
                    glyphicon::draw(&mut self.surface, Icon::Network, x, y, glyph, ink, 255);
                }
                TrayItem::Volume => {
                    let quiet = !crate::sound::present() || crate::sound::volume() == 0;
                    let (x, y) = center(glyph);
                    iconset::draw(&mut self.surface, if quiet { Fi::VolumeOff } else { Fi::Volume }, x, y, glyph, if crate::sound::present() { p.ink } else { p.ink3 }, 255);
                }
                TrayItem::Bell => {
                    let (x, y) = center(glyph);
                    iconset::draw(&mut self.surface, Fi::Bell, x, y, glyph, p.ink2, 255);
                }
                TrayItem::Layout => {
                    paint::text_centered(ctx, &mut self.surface, Role::Caption, rect, lang_tag(), p.ink);
                }
                TrayItem::Clock => {
                    let line = i32::from(ctx.face(Role::Caption).line);
                    let (first, second) = if vertical { (Role::Caption, Role::Caption) } else { (Role::Body, Role::Caption) };
                    let first_line = i32::from(ctx.face(first).line);
                    let total = if date.is_empty() { first_line } else { first_line + line };
                    let top = rect.y + (rect.h as i32 - total) / 2;
                    let row = Rect::new(rect.x, top, rect.w, first_line as u32);
                    paint::text_centered(ctx, &mut self.surface, first, row, &time, p.ink);
                    if !date.is_empty() {
                        let row = Rect::new(rect.x, top + first_line, rect.w, line as u32);
                        paint::text_centered(ctx, &mut self.surface, second, row, &date, p.ink3);
                    }
                }
            }
            self.tray.push((item, rect));
            at += len;
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
    /// Закреплённые без окон: номер в списке закреплённых и место.
    pin_hits: Vec<(usize, Rect)>,
    /// Что нарисовано в каждой кнопке.
    slots: Vec<Slot>,
}

/// Кнопка панели задач: место, окна за ней (номера в списке окон),
/// закреплённая программа и подпись — пустая, когда панель без подписей.
struct Slot {
    rect: Rect,
    members: Vec<usize>,
    pin: Option<usize>,
    label: String,
    /// Сторона значка.
    icon: u32,
}

/// Что стоит в кнопках, по порядку: закреплённые программы (с их окнами),
/// затем окна, которых никто не закреплял. С подписями — кнопка на окно,
/// без них — кнопка на программу.
fn plan_slots(windows: &[Entry], pins: &[PinInfo], labels: bool) -> Vec<(Vec<usize>, Option<usize>, String)> {
    let mut taken = alloc::vec![false; windows.len()];
    let mut plan = Vec::new();
    for (pin_index, pin) in pins.iter().enumerate() {
        let members: Vec<usize> = (0..windows.len()).filter(|index| windows[*index].group == pin.group).collect();
        for index in &members {
            taken[*index] = true;
        }
        if labels && !members.is_empty() {
            for index in members {
                plan.push((alloc::vec![index], Some(pin_index), windows[index].caption.clone()));
            }
        } else {
            plan.push((members, Some(pin_index), String::new()));
        }
    }
    if labels {
        for (index, entry) in windows.iter().enumerate().filter(|(index, _)| !taken[*index]) {
            plan.push((alloc::vec![index], None, entry.caption.clone()));
        }
    } else {
        // Окна одной программы — в одной кнопке, в порядке первого окна
        // группы: кнопки не переезжают при переключении окон.
        let mut groups: Vec<(&str, Vec<usize>)> = Vec::new();
        for (index, entry) in windows.iter().enumerate().filter(|(index, _)| !taken[*index]) {
            match groups.iter_mut().find(|(key, _)| *key == entry.group.as_str()) {
                Some((_, members)) => members.push(index),
                None => groups.push((entry.group.as_str(), alloc::vec![index])),
            }
        }
        plan.extend(groups.into_iter().map(|(_, members)| (members, None, String::new())));
    }
    plan
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
    /// «Пуск» широкий, с подписью. Пустой, когда он плиткой.
    brand: Rect,
    /// Синяя плитка со значком внутри широкого «Пуска».
    chip: Rect,
    /// «Пуск» плиткой — когда док делит место со стопкой.
    brand_square: Rect,
    /// Стопка свёрнутых окон. Пустой прямоугольник — сворачивать нечего.
    stack: Rect,
    /// Поиск: полем или плиткой.
    search: Rect,
    /// Кнопка справа: камера.
    right: Rect,
    /// Полоска жеста под доком.
    home: Rect,
}

/// Числа — из макета (экраны 01 и 09) в его точках; `ctx.px` переводит в наши.
fn dock_layout(m: Metrics, plate: Rect, minimized: usize) -> DockLayout {
    let ctx = m.ctx;
    let side = ctx.px(48);
    let y = plate.y + (plate.h as i32 - side as i32) / 2;
    // Со стопкой в доке пятеро, и поля в макете уже: 12 и 8 вместо 14 и 10.
    let (pad, gap) = if minimized == 0 { (ctx.px(14), ctx.px(10)) } else { (ctx.px(12), ctx.px(8)) };

    let left = Rect::new(plate.x + pad as i32, y, side, side);
    let right = Rect::new(plate.right() - (pad + side) as i32, y, side, side);
    let start = left.right() + gap as i32;
    let end = right.x - gap as i32;

    let (brand, chip, brand_square, stack, search) = if minimized == 0 {
        // Ширина «Пуска» — от подписи: 4 слева, плитка 40, 9, подпись, 13.
        let text = ctx.face(Role::Title).width(DOCK_BRAND);
        let width = ctx.px(4 + 40 + 9 + 13) + text;
        let brand = Rect::new(start, y, width, side);
        let chip_side = ctx.px(40);
        let chip = Rect::new(
            brand.x + ctx.px(4) as i32,
            y + (side as i32 - chip_side as i32) / 2,
            chip_side,
            chip_side,
        );
        let search_x = brand.right() + gap as i32;
        let search = Rect::new(search_x, y, (end - search_x).max(side as i32) as u32, side);
        (brand, chip, Rect::EMPTY, Rect::EMPTY, search)
    } else {
        let brand_square = Rect::new(start, y, side, side);
        let search = Rect::new(end - side as i32, y, side, side);
        let stack_x = brand_square.right() + gap as i32;
        let stack_w = (search.x - gap as i32 - stack_x).max(ctx.px(92) as i32) as u32;
        let stack = Rect::new(stack_x, y, stack_w, side);
        (Rect::EMPTY, Rect::EMPTY, brand_square, stack, search)
    };

    // Полоска жеста лежит **под** доком, у самого низа экрана: это метка
    // системы, а не элемент дока, и внутри плашки она читалась бы кнопкой.
    let home_w = ctx.px(theme::M_HOME_W);
    let home_h = ctx.px(theme::M_HOME_H);
    let below = ctx.px(theme::M_INSET);
    let home = Rect::new(
        plate.x + (plate.w as i32 - home_w as i32) / 2,
        plate.bottom() + ((below as i32 - home_h as i32) / 2).max(0),
        home_w,
        home_h,
    );

    DockLayout { left, brand, chip, brand_square, stack, search, right, home }
}

/// Подложка кнопки дока: стекло кнопки и её контур — у всех кнопок одно.
fn dock_button(surface: &mut Surface, ctx: Ctx, rect: Rect) {
    let p = ctx.palette;
    let radius = ctx.px(16);
    draw::rounded(surface, rect, radius, p.btn.color, p.btn.alpha);
    draw::rounded_stroke(surface, rect, radius, p.btnline.color, p.btnline.alpha);
}

/// Значок по центру прямоугольника.
fn centered_icon(surface: &mut Surface, rect: Rect, icon: Icon, size: u32, color: Color) {
    glyphicon::draw(
        surface,
        icon,
        rect.x + (rect.w as i32 - size as i32) / 2,
        rect.y + (rect.h as i32 - size as i32) / 2,
        size,
        color,
        255,
    );
}

/// Где стоит видимая плашка ПК с этими настройками. `length` — длина
/// Dock вдоль стороны (по содержимому); `None` — пока неизвестна, берётся
/// короткая заготовка, первая перерисовка подгонит её.
fn plate_home(area: Rect, cfg: deskcfg::Taskbar, scale: mini_ui::Scale, length: Option<u32>) -> Option<Rect> {
    let thick = Panel::thickness(cfg, scale);
    if area.w <= thick || area.h <= thick {
        return None;
    }
    let rect = match cfg.mode {
        deskcfg::Mode::Panel => match cfg.edge {
            deskcfg::Edge::Bottom => Rect::new(area.x, area.bottom() - thick as i32, area.w, thick),
            deskcfg::Edge::Top => Rect::new(area.x, area.y, area.w, thick),
            deskcfg::Edge::Left => Rect::new(area.x, area.y, thick, area.h),
            deskcfg::Edge::Right => Rect::new(area.right() - thick as i32, area.y, thick, area.h),
        },
        deskcfg::Mode::Dock => {
            let inset = DOCK_INSET * scale;
            let plate = thick - inset * 2;
            let side = if cfg.edge.vertical() { area.h } else { area.w };
            let length = length.unwrap_or(plate * 4).clamp(plate, side.saturating_sub(inset * 2).max(plate));
            let along = |start: i32, side: u32| start + (side as i32 - length as i32) / 2;
            match cfg.edge {
                deskcfg::Edge::Bottom => Rect::new(along(area.x, area.w), area.bottom() - (inset + plate) as i32, length, plate),
                deskcfg::Edge::Top => Rect::new(along(area.x, area.w), area.y + inset as i32, length, plate),
                deskcfg::Edge::Left => Rect::new(area.x + inset as i32, along(area.y, area.h), plate, length),
                deskcfg::Edge::Right => Rect::new(area.right() - (inset + plate) as i32, along(area.y, area.h), plate, length),
            }
        }
    };
    Some(rect)
}

/// Плашка, уехавшая за край `edge` на `slide` точек.
fn slid(home: Rect, edge: deskcfg::Edge, slide: u32) -> Rect {
    let d = slide as i32;
    match edge {
        deskcfg::Edge::Bottom => home.translate(0, d),
        deskcfg::Edge::Top => home.translate(0, -d),
        deskcfg::Edge::Left => home.translate(-d, 0),
        deskcfg::Edge::Right => home.translate(d, 0),
    }
}

/// Раскладка в трее: как в трее Windows, «РУС» и «ENG».
fn lang_tag() -> &'static str {
    match crate::input::keymap::layout() {
        crate::input::keymap::Layout::Ru => "РУС",
        crate::input::keymap::Layout::Us => "ENG",
    }
}

/// Часы для узкого трея: время и короткая дата (`ДД.ММ`); без часов —
/// время работы.
fn short_clock(status: &Status) -> (String, String) {
    match status.clock.as_deref() {
        Some(clock) => {
            let date = if SHOW_DATE.load(Ordering::Relaxed) {
                crate::time::now_local().map(|now| alloc::format!("{:02}.{:02}", now.day, now.month)).unwrap_or_default()
            } else {
                String::new()
            };
            (String::from(clock), date)
        }
        None => {
            let seconds = status.uptime_ms / 1000;
            (alloc::format!("{}:{:02}", seconds / 3600, (seconds / 60) % 60), String::from("up"))
        }
    }
}

/// Есть ли программа строки манифеста на диске.
fn entry_present(entry: &super::startapps::Entry) -> bool {
    match entry.launch {
        super::startapps::Launch::App(_) => true,
        super::startapps::Launch::Program(file, _) => {
            exists(&alloc::format!("/bin/{file}")) && (entry.needs.is_empty() || exists(entry.needs))
        }
    }
}

/// Что запускает строка манифеста.
fn entry_choice(entry: &super::startapps::Entry) -> Choice {
    match entry.launch {
        super::startapps::Launch::App(app) => Choice::App(app),
        super::startapps::Launch::Program(file, args) => Choice::Program(file, args),
    }
}

/// Значок «Пуска» — четыре **залитые** плитки, как в макете, а не контур
/// [`Icon::Grid`]: на синей кнопке контур в полторы точки теряется.
fn draw_grid(surface: &mut Surface, area: Rect, size: u32, color: Color) {
    let cell = size * 5 / 16;
    let gap = size * 2 / 16;
    let x0 = area.x + (area.w as i32 - size as i32) / 2 + (size * 2 / 16) as i32;
    let y0 = area.y + (area.h as i32 - size as i32) / 2 + (size * 2 / 16) as i32;
    let radius = (size * 12 / 160).max(1);
    for row in 0..2u32 {
        for col in 0..2u32 {
            let rect = Rect::new(
                x0 + (col * (cell + gap)) as i32,
                y0 + (row * (cell + gap)) as i32,
                cell,
                cell,
            );
            draw::rounded(surface, rect, radius, color, 255);
        }
    }
}

fn panel_layout(m: Metrics, plate: Rect, windows: &[Entry], status_w: u32, pins: &[PinInfo]) -> PanelLayout {
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
    let mut buttons = Buttons::new();
    let mut pin_hits = Vec::new();
    let mut slots = Vec::new();
    let labels = LABELS.load(Ordering::Relaxed);
    // Уже этой кнопка с подписью не расскажет ничего: значок, поля и три-четыре
    // знака подписи.
    let least = m.side + m.icon() + m.gap + m.ctx.px(24);
    let square = m.btn_h + m.ctx.px(4);
    for (members, pin, label) in plan_slots(windows, pins, labels) {
        let room = (limit - x).max(0) as u32;
        let width = if label.is_empty() {
            square
        } else {
            if room < least {
                break;
            }
            let entry = &windows[members[0]];
            let state = ButtonState::of(entry.focused, entry.minimized);
            m.button_width(&label, state).min(room).min(m.ctx.px(220))
        };
        if width > room {
            break;
        }
        let rect = Rect::new(x, y, width, m.btn_h);
        for index in &members {
            buttons.push((windows[*index].app, rect));
        }
        if members.is_empty() {
            if let Some(pin) = pin {
                pin_hits.push((pin, rect));
            }
        }
        slots.push(Slot { rect, members, pin, label, icon: m.icon() });
        x += (width + m.gap / 2) as i32;
    }

    PanelLayout { brand, divider, buttons, pin_hits, slots }
}

/// Что выбрано в меню запуска.
///
/// Окно стола и программа из `/bin` — разные вещи, и меню обязано возвращать
/// разные ответы: окно стол открывает сам, а программу запускает.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Окно, которое умеет открыть сам стол.
    App(App),
    /// Программа с собственным окном — её имя в `/bin` и хвост команды.
    Program(&'static str, &'static str),
    /// Программа, поставленная пакетом (фаза N8), — командная строка из
    /// `start=` её манифеста.
    /// Права, которые манифест просит, едут вместе со строкой, а не
    /// спрашиваются при запуске: спросить их пришлось бы по пути к
    /// исполняемому файлу, а у программы на .NET этот путь — `/bin/dotnet`,
    /// то есть системный каталог, к пакету отношения не имеющий.
    Command(String, fpk::Rights),
    /// «Параметры» на разделе (меню пользователя в «Пуске», 20.11).
    Settings(super::settings::Section),
    /// Заблокировать (то же, что Win+L).
    Lock,
    /// Недавний файл: команда программы и путь.
    Recent(String, String),
}

/// Программа, поставленная пакетом (фаза N8): строка «Пуска» из реестра
/// `/var/lib/pkg`.
///
/// Манифест пакета — первое место, где программа говорит о себе сама, и
/// названный предел списка программ «Пуска» здесь снят: `start=` — чем её
/// запускать, `caption=` и `about=` — как её назвать. Пакет без `start=` в меню
/// не попадает: это библиотека или данные, запускать там нечего.
pub(super) struct Launcher {
    pub(super) name: String,
    pub(super) caption: String,
    about: String,
    pub(super) start: String,
    /// Что пакет просит у системы (`permissions=` в манифесте). Отсутствие
    /// строки — пусто, а не «всё»: см. [`fpk::Manifest::rights`].
    pub(super) rights: fpk::Rights,
    /// Значок на рабочем столе (`desktop=yes`, фаза 71i).
    pub(super) desktop: bool,
    /// Картинка значка по имени (`icon=`): `network`, `terminal`, `folder`…
    pub(super) icon: String,
    /// Какие файлы программа открывает (`opens=txt,log`): она появляется в
    /// «Открыть с помощью» у файлов с этими расширениями.
    pub(super) opens: String,
}

/// Где лежат записи установленных пакетов.
pub(super) const REGISTRY: &str = "/var/lib/pkg";

/// Пакеты со строкой запуска — по имени, чтобы порядок строк не зависел от
/// порядка записей в каталоге.
pub(super) fn list_packages() -> Vec<Launcher> {
    // Предел манифеста в контейнере (`fpk::MAX_MANIFEST`); запись реестра —
    // тот же манифест.
    const LIMIT: usize = fpk::MAX_MANIFEST;
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
        // Права разбираются из той же записи реестра. Непонятое имя даёт
        // пустой набор, а не «всё»: запись могла прийти от системы поновее, и
        // «не понял, значит можно» — ровно та ошибка, ради которой всё это
        // затевалось. Случай остаточный: такой пакет эта система и не
        // поставит, `pkg` проверяет манифест при установке.
        let rights = sysconf::value(text, "permissions").map_or(fpk::Rights::NONE, |list| {
            fpk::Rights::parse(list).unwrap_or(fpk::Rights::NONE)
        });
        launchers.push(Launcher {
            name: String::from(name),
            caption: String::from(sysconf::value(text, "caption").unwrap_or(name)),
            about: String::from(about),
            start: String::from(start),
            rights,
            desktop: sysconf::value(text, "desktop") == Some("yes"),
            icon: String::from(sysconf::value(text, "icon").unwrap_or("package")),
            opens: String::from(sysconf::value(text, "opens").unwrap_or("")),
        });
    }
    launchers.sort_by(|a, b| a.name.cmp(&b.name));
    launchers
}

// ── Программы Windows из префиксов Proton (P12, 18.11.2026) ─────────────────

/// Ярлык программы Windows: `.desktop`, который `winemenubuilder` установщика
/// записал в `ПРЕФИКС/drive_c/proton_shortcuts/` (так у Proton: не в
/// `~/.local/share/applications/wine`). Читает его и запускает сценарий
/// `fossai-proton --shortcut=…`; здесь — только имя, значок и путь.
pub(super) struct WinShortcut {
    pub(super) caption: String,
    /// Путь файла `.desktop`.
    pub(super) file: String,
    /// Префикс Wine — каталог с `drive_c`.
    pub(super) prefix: String,
    /// FIMG значка, если он уже сделан из PNG; иначе пусто.
    pub(super) icon: String,
    /// Ярлык удаления («Uninstall …» — его кладут Inno Setup, NSIS и подобные).
    pub(super) uninstall: bool,
}

/// Сценарий пакета `proton`.
const PROTON_SCRIPT: &str = "/opt/proton/bin/fossai-proton";

/// Где в префиксе лежат ярлыки.
const SHORTCUTS: &str = "drive_c/proton_shortcuts";

/// Командная строка `fossai-proton` с аргументами.
pub(super) fn proton_line(args: &str) -> String {
    alloc::format!("/opt/debian/bin/bash {PROTON_SCRIPT} {args}")
}

/// Путь для командной строки: ядро делит её по пробелам и кавычек не знает,
/// поэтому пробел — `%20`, `%` — `%25` (раскодируют `fossai-proton` и
/// `png2fimg`).
pub(super) fn pct(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            ' ' => out.push_str("%20"),
            '%' => out.push_str("%25"),
            _ => out.push(ch),
        }
    }
    out
}

fn exists(path: &str) -> bool {
    matches!(crate::fs::read(path, 0), Some(Ok(_)))
}

/// Ярлыки во всех префиксах дома (`~/.proton`, `~/.tmnf`, … — каталоги с
/// точкой, в которых есть `drive_c/proton_shortcuts`). Дом — того, кто вошёл,
/// и `/root` (живая флешка и `run` из оболочки ставят туда). Без пакета
/// `proton` — пусто: запускать ярлыки нечем.
pub(super) fn windows_shortcuts() -> Vec<WinShortcut> {
    const LIMIT: usize = 16 * 1024;
    let mut out = Vec::new();
    if !exists(PROTON_SCRIPT) {
        return out;
    }
    let mut homes = alloc::vec![super::context::home_dir()];
    if homes[0] != "/root" {
        homes.push(String::from("/root"));
    }
    for home in &homes {
        let Some(Ok(entries)) = crate::fs::list(home) else {
            continue;
        };
        for entry in entries {
            if entry.kind != crate::vfs::NodeKind::Directory || !entry.name.starts_with('.') || entry.name.len() < 2 {
                continue;
            }
            let prefix = alloc::format!("{home}/{}", entry.name);
            let dir = alloc::format!("{prefix}/{SHORTCUTS}");
            let Some(Ok(files)) = crate::fs::list(&dir) else {
                continue;
            };
            for file in files {
                if file.kind != crate::vfs::NodeKind::File || !file.name.ends_with(".desktop") {
                    continue;
                }
                let path = alloc::format!("{dir}/{}", file.name);
                let Some(Ok((bytes, _))) = crate::fs::read(&path, LIMIT) else {
                    continue;
                };
                let text = String::from_utf8_lossy(&bytes);
                let field = |key: &str| {
                    text.lines().find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('='))).map(str::trim)
                };
                if field("NoDisplay") == Some("true") {
                    continue;
                }
                let caption = String::from(field("Name").unwrap_or_else(|| file.name.trim_end_matches(".desktop")));
                let lower = caption.to_lowercase();
                let uninstall = lower.starts_with("uninstall") || lower.starts_with("удалить") || file.name.to_lowercase().starts_with("uninstall");
                let icon = shortcut_icon(&dir, field("Icon").unwrap_or(""));
                out.push(WinShortcut { caption, file: path, prefix: prefix.clone(), icon, uninstall });
            }
        }
    }
    out.sort_by(|a, b| a.caption.cmp(&b.caption));
    out
}

/// Префиксы Wine в доме (каталоги с точкой, в которых есть `system.reg`):
/// для «Установка и удаление программ» в «Параметрах». Путь с пробелом не
/// берётся — его не передать строкой запуска. Без пакета `proton` — пусто.
pub(super) fn windows_prefixes() -> Vec<String> {
    let mut out = Vec::new();
    if !exists(PROTON_SCRIPT) {
        return out;
    }
    let mut homes = alloc::vec![super::context::home_dir()];
    if homes[0] != "/root" {
        homes.push(String::from("/root"));
    }
    for home in &homes {
        let Some(Ok(entries)) = crate::fs::list(home) else {
            continue;
        };
        for entry in entries {
            if entry.kind != crate::vfs::NodeKind::Directory || !entry.name.starts_with('.') || entry.name.contains(' ') {
                continue;
            }
            let prefix = alloc::format!("{home}/{}", entry.name);
            if exists(&alloc::format!("{prefix}/system.reg")) {
                out.push(prefix);
            }
        }
    }
    out.sort();
    out
}

/// FIMG значка ярлыка. `Icon=` у winemenubuilder — имя картинки в
/// `proton_shortcuts/icons/` (по размерам `ШxВ/apps/`) или полный путь. PNG
/// стол не читает: если FIMG рядом ещё нет, запускается `png2fimg`, и значок
/// появится при следующем открытии меню; пока — общий значок Windows.
fn shortcut_icon(dir: &str, icon: &str) -> String {
    if icon.is_empty() {
        return String::new();
    }
    let mut candidates = Vec::new();
    if icon.starts_with('/') {
        candidates.push(if icon.ends_with(".png") { String::from(icon) } else { alloc::format!("{icon}.png") });
    } else {
        let name = icon.trim_end_matches(".png");
        for size in ["256x256", "128x128", "64x64", "48x48", "32x32"] {
            candidates.push(alloc::format!("{dir}/icons/{size}/apps/{name}.png"));
        }
        candidates.push(alloc::format!("{dir}/icons/{name}.png"));
    }
    let Some(png) = candidates.into_iter().find(|png| exists(png)) else {
        return String::new();
    };
    let fimg = alloc::format!("{}.fimg", png.trim_end_matches(".png"));
    if exists(&fimg) {
        return fimg;
    }
    convert_icon(&png, &fimg);
    String::new()
}

/// PNG, для которых `png2fimg` уже запускался в эту загрузку: не вышло
/// однажды — не запускать на каждом открытии меню.
static ICON_JOBS: crate::sync::SpinLock<Vec<String>> = crate::sync::SpinLock::new(Vec::new());

fn convert_icon(png: &str, fimg: &str) {
    {
        let mut jobs = ICON_JOBS.lock();
        if jobs.iter().any(|known| known == png) {
            return;
        }
        jobs.push(String::from(png));
    }
    let line = alloc::format!("/bin/png2fimg {} {}", pct(png), pct(fimg));
    match crate::user::spawn(&line, crate::user::session::credentials()) {
        Ok(id) => crate::kprintln!("  desktop     : started '{line}' as {id}"),
        Err(err) => crate::kprintln!("  desktop     : cannot start '{line}': {err}"),
    }
}

/// Строки «Пуска» для программ Windows: запуск ярлыка с правами пакета
/// `proton` (у него `windows files net jit`). Ярлыки удаления — не здесь, а
/// в «Параметры → Пакеты».
fn windows_launchers(packages: &[Launcher]) -> Vec<Launcher> {
    let Some(proton) = packages.iter().find(|launcher| launcher.name == "proton") else {
        return Vec::new();
    };
    windows_shortcuts()
        .into_iter()
        .filter(|shortcut| !shortcut.uninstall)
        .map(|shortcut| {
            let place = shortcut.prefix.rsplit('/').next().unwrap_or("");
            Launcher {
                name: alloc::format!("windows:{}", shortcut.file),
                about: alloc::format!("Windows · {place}"),
                start: proton_line(&alloc::format!("--shortcut={}", pct(&shortcut.file))),
                rights: proton.rights,
                desktop: false,
                icon: if shortcut.icon.is_empty() { String::from("windows-apps") } else { shortcut.icon },
                opens: String::new(),
                caption: shortcut.caption,
            }
        })
        .collect()
}

/// Строка меню: программа манифеста, программа из пакета или строка питания.
///
/// Пакет — номер в списке пакетов меню, а не ссылка: список читается с диска
/// при каждом открытии и живёт в самом меню, а `Item` копируется.
#[derive(Clone, Copy)]
enum Item {
    Entry(&'static startapps::Entry),
    Package(usize),
    /// Окно стола — только «Завершение работы» и «Перезагрузка».
    App(App),
}

impl Item {
    fn caption(self, packages: &[Launcher]) -> &str {
        match self {
            Item::App(app) => app.caption(),
            Item::Entry(entry) => entry.caption,
            Item::Package(index) => packages.get(index).map_or("", |launcher| launcher.caption.as_str()),
        }
    }

    fn about(self, packages: &[Launcher]) -> &str {
        match self {
            Item::App(app) => app.about(),
            Item::Entry(entry) => entry.about,
            Item::Package(index) => packages.get(index).map_or("", |launcher| launcher.about.as_str()),
        }
    }

    fn icon(self) -> Icon {
        match self {
            Item::App(app) => app.icon(),
            Item::Entry(entry) => entry.icon,
            // Значков из ресурсов `.exe` система не читает — у всех пакетов один.
            Item::Package(_) => Icon::Package,
        }
    }

    fn tone(self) -> Tone {
        match self {
            Item::App(app) => app.tone(),
            Item::Entry(_) | Item::Package(_) => Tone::Accent,
        }
    }

    /// Строка гасит или перезапускает машину.
    fn danger(self) -> bool {
        matches!(self, Item::App(app) if app.confirms_power().is_some())
    }

    /// Папка строки манифеста; пусто — строка верхнего уровня.
    fn folder(self) -> &'static str {
        match self {
            Item::Entry(entry) => entry.folder,
            _ => "",
        }
    }

    /// Ключ закрепления (`start_pins=`, `taskbar_pins=`).
    fn key(self, packages: &[Launcher]) -> String {
        match self {
            Item::Entry(entry) => String::from(entry.key),
            Item::Package(index) => alloc::format!("pkg:{}", packages.get(index).map_or("", |launcher| launcher.name.as_str())),
            Item::App(app) => String::from(app.title()),
        }
    }

    fn choice(self, packages: &[Launcher]) -> Option<Choice> {
        Some(match self {
            Item::App(app) => Choice::App(app),
            Item::Entry(entry) => entry_choice(entry),
            Item::Package(index) => {
                let package = packages.get(index)?;
                Choice::Command(package.start.clone(), package.rights)
            }
        })
    }
}

/// Плитка закреплённых: программа (номер в `items`) или папка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tile {
    Item(usize),
    Folder(&'static startapps::Folder),
}

/// Во что попала точка «Пуска».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Target {
    /// Плитка закреплённых (номер в `tiles`).
    Tile(usize),
    /// Строка списка или результата поиска (номер в `items`).
    Row(usize),
    /// Папка в списке «Все программы» (номер в `FOLDERS`).
    FolderRow(usize),
    /// Программа в раскрытой папке над плитками (номер в `items`).
    Child(usize),
    /// Недавний файл.
    Recent(usize),
    /// Кнопка пользователя.
    User,
    /// Маленькая кнопка внизу: диспетчер задач, устройств, параметры.
    Tool(usize),
    /// Кнопка питания (номер в `items`).
    Power(usize),
    /// Полоса прокрутки: `true` — списка, `false` — закреплённых.
    Bar(bool),
    /// Строка всплывающего меню.
    Pop(usize),
    Search,
}

/// Маленькие кнопки слева от питания: диспетчер задач, диспетчер
/// устройств, параметры (Роман, 20.11).
const TOOLS: [(&str, Fi); 3] = [("Диспетчер задач", Fi::Activity), ("Диспетчер устройств", Fi::Cpu), ("Параметры", Fi::Sliders)];

/// Что делает строка всплывающего меню.
#[derive(Clone, PartialEq, Eq, Debug)]
enum PopAct {
    Open(usize),
    PinStart(String),
    UnpinStart(String),
    PinTask(String),
    UnpinTask(String),
    First(String),
    Account,
    Lock,
    Logout,
}

/// Всплывающее над «Пуском»: меню программы, меню пользователя или
/// раскрытая папка закреплённых.
#[derive(Clone)]
enum Popup {
    Menu { at: (i32, i32), rows: Vec<(String, PopAct, bool)> },
    User,
    Folder { folder: &'static startapps::Folder, cell: Rect },
}

/// Что сделал щелчок по открытому «Пуску».
pub enum MenuClick {
    /// Запустить и закрыть меню.
    Launch(Choice),
    /// Щелчок разобран внутри меню — оно остаётся открытым.
    Stay,
    /// Щелчок мимо — закрыть.
    Close,
}

/// Меню запуска.
///
/// # Порядок строк
///
/// `items` — весь список в порядке манифеста; по нему ходят стрелки. Первые
/// четыре строки — «Терминал», «Параметры», «О системе», «Файлы» — и две
/// последние — «Завершение работы», «Перезагрузка» — стоят на своих местах с
/// фазы С3: сценарии стенда считают нажатия.
pub struct Menu {
    surface: Surface,
    pub rect: Rect,
    /// Множитель геометрии стола.
    scale: mini_ui::Scale,
    items: Vec<Item>,
    /// Выбранная строка — номер в `visible`.
    row: usize,
    open: bool,
    damage: Rect,
    /// Сколько программ лежит в `/bin` — для строки журнала при запуске стола.
    bin_programs: usize,
    /// Программы из пакетов, прочитанные при сборке меню (фаза N8).
    packages: Vec<Launcher>,
    /// Ширина колонки строк (телефон).
    width: u32,
    /// Набранное в поле поиска (72f): строки отбираются по подписи.
    query: String,
    /// Строки, видимые при этом запросе, — номера в `items`.
    visible: Vec<usize>,
    /// Закреплённые плитки.
    tiles: Vec<Tile>,
    /// Раскрытые в списке папки.
    expanded: Vec<&'static str>,
    /// Прокрутка закреплённых и списка, точки.
    scroll_pins: i32,
    scroll_list: i32,
    /// Под указателем.
    hot: Option<Target>,
    /// Всплывающее меню или папка.
    popup: Option<Popup>,
    /// Стрелками уже ходили — показать выбранную строку рамкой.
    keys_used: bool,
    cfg: deskcfg::Start,
    /// Недавние файлы (команда, путь).
    recent: Vec<(String, String)>,
    /// Поверхность прокручиваемой колонки — рисуется в ней и кладётся в
    /// карточку срезанной по краям.
    scratch: Option<Surface>,
    /// Сторона панели — с неё «Пуск» открывается.
    edge: deskcfg::Edge,
}

/// «Пуск» ПК (72f, 20.11): размеры в точках при масштабе 1.
mod start_menu {
    pub const PAD: u32 = 16;
    pub const SEARCH_H: u32 = 36;
    pub const TITLE_H: u32 = 30;
    pub const CELL_W: u32 = 88;
    pub const CELL_H: u32 = 92;
    pub const GAP: u32 = 24;
    pub const LIST_ROW: u32 = 34;
    pub const LETTER_ROW: u32 = 24;
    pub const RECENT_ROW: u32 = 44;
    pub const FOOT_H: u32 = 60;
    pub const BAR_W: u32 = 6;
    pub const POP_ROW: u32 = 34;
    /// Обычный и широкий «Пуск»: столбцов плиток, ширина списка, высота.
    pub const fn columns(wide: bool) -> u32 {
        if wide { 6 } else { 4 }
    }
    pub const fn list_w(wide: bool) -> u32 {
        if wide { 300 } else { 256 }
    }
    pub const fn max_h(wide: bool) -> u32 {
        if wide { 720 } else { 640 }
    }
}

/// Раскладка «Пуска» ПК — одна на рисование и на попадания.
struct StartLayout {
    card: Rect,
    search: Rect,
    /// Заголовок и поле плиток (без прокрутки).
    pins_title: Option<Rect>,
    pins: Option<Rect>,
    recent_title: Option<Rect>,
    list_title: Option<Rect>,
    /// Поле списка (или результатов поиска).
    list: Option<Rect>,
    foot: Rect,
    /// Высота содержимого плиток и списка.
    pins_content: u32,
    list_content: u32,
    /// Ячейки в координатах карточки, с учётом прокрутки.
    cells: Vec<(Rect, Target)>,
    /// Буквы списка: место и буква.
    letters: Vec<(Rect, String)>,
    /// Всплывающее: место и строки.
    popup: Option<(Rect, Vec<(Rect, Target)>)>,
}

impl Menu {
    /// «Пуск» у панели основного монитора: `area` — его место на столе,
    /// `panel` — место видимой панели, `cfg` — её настройки.
    #[must_use]
    pub fn new(area: Rect, panel: Rect, cfg: deskcfg::Taskbar, scale: mini_ui::Scale) -> Option<Self> {
        let scale = scale;
        let ctx = desk_ctx(scale);
        let m = Metrics::new(ctx);
        let panel_top = if panel.is_empty() { area.bottom() } else { panel.y };
        // На телефоне карточка стоит над доком: дока нет выше верха экрана.
        // На ПК панель бывает и сверху (20.11) — там место считает
        // `menu_place`.
        if theme::is_mobile() && panel_top <= area.y {
            return None;
        }

        let names = list_programs();
        let mut items = Vec::new();
        // Программа попадает в меню, только если она действительно лежит в
        // `/bin`: строка, которая ничего не запускает, хуже отсутствующей.
        for entry in &startapps::ENTRIES {
            let present = match entry.launch {
                startapps::Launch::App(_) => true,
                startapps::Launch::Program(file, _) => {
                    names.iter().any(|name| name == file) && (entry.needs.is_empty() || exists(entry.needs))
                }
            };
            if present {
                items.push(Item::Entry(entry));
            }
        }
        // Программы из пакетов — после своих и до строк питания (фаза N8).
        let mut packages = list_packages();
        // Программы Windows, поставленные в префиксы Proton (P12, 18.11).
        let windows = windows_launchers(&packages);
        packages.extend(windows);
        items.extend((0..packages.len()).map(Item::Package));
        items.extend([Item::App(App::Shutdown), Item::App(App::Restart)]);

        let start_cfg = deskcfg::start();
        let (card_w, card_h, width) = if theme::is_mobile() {
            // На телефоне «Пуск» занимает экран целиком — от строки состояния
            // до дока.
            let inset = ctx.px(theme::M_INSET);
            let top = ctx.px(theme::M_STATUS_H + theme::M_INSET);
            let full_w = area.w.saturating_sub(inset * 2).max(1);
            let full_h = (panel_top - top as i32).max(1) as u32;
            (full_w, full_h, full_w.saturating_sub(m.pad * 2))
        } else {
            use start_menu::*;
            let wide = start_cfg.wide;
            let grid = CELL_W * columns(wide);
            let left = start_cfg.pinned || start_cfg.recent;
            let w = match (left, start_cfg.all) {
                (true, true) => PAD * 2 + grid + GAP + list_w(wide),
                (true, false) => PAD * 2 + grid,
                _ => PAD * 2 + list_w(wide) + 120,
            };
            let thick = if cfg.edge.vertical() { 0 } else { panel.h };
            let room = area.h.saturating_sub(thick + ctx.px(24));
            let h = ctx.px(max_h(wide)).min(room);
            (ctx.px(w).min(area.w.saturating_sub(ctx.px(16))), h, 0)
        };

        let surface = Surface::new(card_w, card_h, glass_bg())?;
        let rect = if theme::is_mobile() {
            Rect::new(area.x + ctx.px(theme::M_INSET) as i32, panel_top - surface.height() as i32, surface.width(), surface.height())
        } else {
            menu_place(area, panel, cfg, surface.width(), surface.height(), ctx.px(8))
        };
        let visible = (0..items.len()).collect();
        let mut menu = Self {
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
            query: String::new(),
            visible,
            tiles: Vec::new(),
            expanded: Vec::new(),
            scroll_pins: 0,
            scroll_list: 0,
            hot: None,
            popup: None,
            keys_used: false,
            cfg: start_cfg,
            recent: Vec::new(),
            scratch: None,
            edge: cfg.edge,
        };
        menu.load_tiles();
        Some(menu)
    }

    /// Закреплённые плитки — из `start_pins=`. Ключ, которого нет (пакет
    /// сняли, пример не собран), молча пропускается.
    fn load_tiles(&mut self) {
        self.tiles.clear();
        for key in deskcfg::start_pins() {
            if let Some(folder) = startapps::folder(&key) {
                // Пустая папка (ни одной программы на диске) не ставится.
                if self.items.iter().any(|item| item.folder() == folder.key) {
                    self.tiles.push(Tile::Folder(folder));
                }
                continue;
            }
            if let Some(index) = self.items.iter().position(|item| !item.danger() && item.key(&self.packages) == key) {
                self.tiles.push(Tile::Item(index));
            }
        }
    }

    /// Набрать знак в поле поиска (72f). `true` — запрос изменился.
    pub fn type_char(&mut self, ch: char) -> bool {
        if theme::is_mobile() || ch.is_control() || self.query.chars().count() >= 32 {
            return false;
        }
        self.query.push(ch);
        self.refilter();
        true
    }

    /// Стереть последний знак запроса.
    pub fn backspace(&mut self) -> bool {
        if self.query.pop().is_none() {
            return false;
        }
        self.refilter();
        true
    }

    /// Отобрать строки по запросу: подпись содержит его, без учёта регистра.
    fn refilter(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = (0..self.items.len())
            .filter(|index| query.is_empty() || self.items[*index].caption(&self.packages).to_lowercase().contains(&query))
            .collect();
        self.row = 0;
        self.scroll_list = 0;
        self.popup = None;
        self.redraw();
    }

    /// Запрос в журнал.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
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
            self.query.clear();
            self.visible = (0..self.items.len()).collect();
            self.scroll_list = 0;
            self.scroll_pins = 0;
            self.popup = None;
            self.hot = None;
            self.keys_used = false;
            self.expanded.clear();
            self.recent = super::recent_files();
            self.redraw();
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
        self.popup = None;
    }

    /// Где что стоит в открытом «Пуске», строками `имя at X,Y WxH` в
    /// координатах экрана — для журнала и прицела стенда.
    #[must_use]
    pub fn spots(&self) -> Vec<String> {
        if theme::is_mobile() {
            return Vec::new();
        }
        let layout = self.start_layout();
        let at = |name: &str, rect: Rect| alloc::format!("{name} at {},{} {}x{}", rect.x + self.rect.x, rect.y + self.rect.y, rect.w, rect.h);
        let mut out = alloc::vec![at("menu", layout.card)];
        if let Some(list) = layout.list {
            out.push(at("list", list));
        }
        if let Some(pins) = layout.pins {
            out.push(at("pins", pins));
        }
        for (rect, target) in &layout.cells {
            let name = match *target {
                Target::Tile(n) => match self.tiles.get(n) {
                    Some(Tile::Item(index)) => alloc::format!("tile:{}", self.items[*index].key(&self.packages)),
                    Some(Tile::Folder(folder)) => alloc::format!("tile:{}", folder.key),
                    None => continue,
                },
                Target::FolderRow(number) => alloc::format!("row:{}", startapps::FOLDERS[number].key),
                Target::User => String::from("user"),
                Target::Tool(n) => alloc::format!("tool:{n}"),
                Target::Power(index) => alloc::format!("power:{}", self.items[index].key(&self.packages)),
                _ => continue,
            };
            out.push(at(&name, *rect));
        }
        if let Some((_, lines)) = &layout.popup {
            for (rect, target) in lines {
                match *target {
                    Target::Pop(n) => out.push(at(&alloc::format!("pop:{n}"), *rect)),
                    Target::Child(index) => out.push(at(&alloc::format!("child:{}", self.items[index].key(&self.packages)), *rect)),
                    _ => {}
                }
            }
        }
        out
    }

    /// Напечатать места строк всплывающего (стенд целится в них).
    fn log_popup(&self) {
        for spot in self.spots().into_iter().filter(|spot| spot.starts_with("pop:") || spot.starts_with("child:")) {
            crate::kprintln!("  desktop     : start spot {spot}");
        }
    }

    /// Закрыть всплывающее меню, если оно есть (Escape). `true` — было.
    pub fn dismiss_popup(&mut self) -> bool {
        if self.popup.take().is_some() {
            self.redraw();
            return true;
        }
        false
    }

    /// Перерисовать меню под текущую тему.
    pub fn restyle(&mut self) {
        self.redraw();
    }

    /// Номер строки (в `visible`) под точкой экрана — телефон.
    fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        self.layout().rows.iter().find(|row| row.rect.contains(x, y)).map(|row| row.index)
    }

    /// Что лежит под точкой (координаты экрана) — телефон.
    #[must_use]
    pub fn choice_at(&self, x: i32, y: i32) -> Option<Choice> {
        if !theme::is_mobile() {
            let (target, _) = self.target_at(x, y)?;
            return match target {
                Target::Tile(n) => match self.tiles.get(n)? {
                    Tile::Item(index) => self.items.get(*index)?.choice(&self.packages),
                    Tile::Folder(_) => None,
                },
                Target::Row(index) | Target::Child(index) | Target::Power(index) => self.items.get(index)?.choice(&self.packages),
                _ => None,
            };
        }
        let index = self.row_at(x, y)?;
        self.items.get(index).and_then(|item| item.choice(&self.packages))
    }

    /// Поставить выделение туда, где стоит указатель (телефон).
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
        let count = if theme::is_mobile() { self.items.len() } else { self.visible.len() };
        if count == 0 {
            return;
        }
        self.row = if forward { (self.row + 1) % count } else { (self.row + count - 1) % count };
        self.keys_used = true;
        self.popup = None;
        if !theme::is_mobile() {
            self.reveal_selection();
        }
        self.redraw();
    }

    /// Выбранная стрелками строка — на виду: её папка раскрыта, список
    /// прокручен до неё.
    fn reveal_selection(&mut self) {
        let Some(index) = self.visible.get(self.row).copied() else { return };
        let folder = self.items[index].folder();
        if !folder.is_empty() && self.query.is_empty() {
            if let Some(known) = startapps::folder(folder) {
                if !self.expanded.contains(&known.key) {
                    self.expanded.push(known.key);
                }
            }
        }
        let layout = self.start_layout();
        let Some(list) = layout.list else { return };
        if let Some((rect, _)) = layout.cells.iter().find(|(_, target)| *target == Target::Row(index)) {
            let top = rect.y - list.y;
            let bottom = rect.bottom() - list.y;
            if top < 0 {
                self.scroll_list += top;
            } else if bottom > list.h as i32 {
                self.scroll_list += bottom - list.h as i32;
            }
        } else if !layout.cells.iter().any(|(_, target)| *target == Target::Power(index)) {
            // Строки нет среди нарисованных (уехала за край) — посчитать её
            // место без прокрутки.
            let saved = self.scroll_list;
            self.scroll_list = 0;
            let flat = self.start_layout();
            if let Some((rect, _)) = flat.cells.iter().find(|(_, target)| *target == Target::Row(index)) {
                self.scroll_list = (rect.bottom() - list.y - list.h as i32).max(0);
            } else {
                self.scroll_list = saved;
            }
        }
        self.clamp_scroll();
    }

    /// Что выбрано сейчас.
    #[must_use]
    pub fn selection(&self) -> Option<Choice> {
        let index = if theme::is_mobile() { self.row } else { *self.visible.get(self.row)? };
        self.items.get(index).and_then(|item| item.choice(&self.packages))
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    // ── «Пуск» ПК: раскладка ────────────────────────────────────────────────

    /// Строки списка «Все программы» по алфавиту: программы верхнего уровня и
    /// папки; у раскрытой папки за ней — её программы.
    fn list_rows(&self) -> Vec<(Target, bool)> {
        let mut top: Vec<(String, Target)> = Vec::new();
        for index in 0..self.items.len() {
            let item = self.items[index];
            if item.danger() || !item.folder().is_empty() {
                continue;
            }
            top.push((item.caption(&self.packages).to_lowercase(), Target::Row(index)));
        }
        for (number, folder) in startapps::FOLDERS.iter().enumerate() {
            if self.items.iter().any(|item| item.folder() == folder.key) {
                top.push((String::from(folder.caption).to_lowercase(), Target::FolderRow(number)));
            }
        }
        top.sort_by(|a, b| a.0.cmp(&b.0));
        let mut rows = Vec::new();
        for (_, target) in top {
            rows.push((target, false));
            if let Target::FolderRow(number) = target {
                let folder = &startapps::FOLDERS[number];
                if self.expanded.contains(&folder.key) {
                    for index in 0..self.items.len() {
                        if self.items[index].folder() == folder.key {
                            rows.push((Target::Row(index), true));
                        }
                    }
                }
            }
        }
        rows
    }

    /// Подпись строки списка.
    fn target_caption(&self, target: Target) -> &str {
        match target {
            Target::Row(index) | Target::Child(index) | Target::Power(index) => self.items[index].caption(&self.packages),
            Target::FolderRow(number) => startapps::FOLDERS[number].caption,
            Target::Tile(n) => match self.tiles.get(n) {
                Some(Tile::Item(index)) => self.items[*index].caption(&self.packages),
                Some(Tile::Folder(folder)) => folder.caption,
                None => "",
            },
            _ => "",
        }
    }

    fn start_layout(&self) -> StartLayout {
        use start_menu::*;
        let ctx = desk_ctx(self.scale);
        let px = |value: u32| ctx.px(value);
        let card = self.surface.bounds();
        let search = Rect::new(px(PAD) as i32, px(PAD) as i32, card.w - px(PAD * 2), px(SEARCH_H));
        let foot = Rect::new(0, card.bottom() - px(FOOT_H) as i32, card.w, px(FOOT_H));
        let body_top = search.bottom() + px(12) as i32;
        let body_bottom = foot.y - px(8) as i32;
        let mut cells = Vec::new();
        let mut letters = Vec::new();
        let mut layout = StartLayout {
            card,
            search,
            pins_title: None,
            pins: None,
            recent_title: None,
            list_title: None,
            list: None,
            foot,
            pins_content: 0,
            list_content: 0,
            cells: Vec::new(),
            letters: Vec::new(),
            popup: None,
        };
        cells.push((search, Target::Search));

        if !self.query.is_empty() {
            // Поиск: одна колонка результатов во всю ширину.
            let title = Rect::new(px(PAD) as i32, body_top, card.w - px(PAD * 2), px(TITLE_H));
            let list = Rect::new(title.x, title.bottom(), title.w, (body_bottom - title.bottom()).max(0) as u32);
            layout.list_title = Some(title);
            layout.list = Some(list);
            let mut y = list.y - self.scroll_list;
            for index in self.visible.iter().copied().filter(|index| !self.items[*index].danger()) {
                cells.push((Rect::new(list.x, y, list.w - px(BAR_W + 4), px(LIST_ROW)), Target::Row(index)));
                y += px(LIST_ROW) as i32;
            }
            layout.list_content = (y + self.scroll_list - list.y).max(0) as u32;
        } else {
            let wide = self.cfg.wide;
            let left_on = self.cfg.pinned || self.cfg.recent;
            let grid_w = px(CELL_W * columns(wide));
            let left_x = px(PAD) as i32;
            let list_x = if left_on { left_x + (grid_w + px(GAP)) as i32 } else { left_x };
            let list_w = card.w.saturating_sub((list_x as u32) + px(PAD));

            // Левая колонка: закреплённые, под ними недавние.
            if left_on {
                let recent_rows = if self.cfg.recent { self.recent.len().clamp(1, if wide { 4 } else { 3 }) as u32 } else { 0 };
                let recent_h = if self.cfg.recent { px(TITLE_H) + px(RECENT_ROW) * recent_rows + px(4) } else { 0 };
                let recent_y = body_bottom - recent_h as i32;
                if self.cfg.pinned {
                    let title = Rect::new(left_x, body_top, grid_w, px(TITLE_H));
                    let bottom = if self.cfg.recent { recent_y - px(6) as i32 } else { body_bottom };
                    let pins = Rect::new(left_x, title.bottom(), grid_w, (bottom - title.bottom()).max(0) as u32);
                    layout.pins_title = Some(title);
                    layout.pins = Some(pins);
                    let columns = columns(wide);
                    for (n, _) in self.tiles.iter().enumerate() {
                        let (col, row) = (n as u32 % columns, n as u32 / columns);
                        let rect = Rect::new(pins.x + px(col * CELL_W) as i32, pins.y + px(row * CELL_H) as i32 - self.scroll_pins, px(CELL_W), px(CELL_H));
                        cells.push((rect, Target::Tile(n)));
                    }
                    let rows = (self.tiles.len() as u32).div_ceil(columns);
                    layout.pins_content = px(rows * CELL_H);
                }
                if self.cfg.recent {
                    let top = if self.cfg.pinned { recent_y } else { body_top };
                    let title = Rect::new(left_x, top, grid_w, px(TITLE_H));
                    layout.recent_title = Some(title);
                    for (n, _) in self.recent.iter().enumerate().take(recent_rows as usize) {
                        let rect = Rect::new(left_x, title.bottom() + (px(RECENT_ROW) * n as u32) as i32, grid_w, px(RECENT_ROW));
                        cells.push((rect, Target::Recent(n)));
                    }
                }
            }

            // Правая колонка: все программы по алфавиту с буквами.
            if self.cfg.all {
                let title = Rect::new(list_x, body_top, list_w, px(TITLE_H));
                let list = Rect::new(list_x, title.bottom(), list_w, (body_bottom - title.bottom()).max(0) as u32);
                layout.list_title = Some(title);
                layout.list = Some(list);
                let row_w = list.w.saturating_sub(px(BAR_W + 6));
                let mut y = list.y - self.scroll_list;
                let mut letter: Option<char> = None;
                for (target, child) in self.list_rows() {
                    if !child {
                        let first = self.target_caption(target).chars().next().map(|ch| ch.to_uppercase().next().unwrap_or(ch));
                        if first != letter {
                            letter = first;
                            letters.push((Rect::new(list.x + px(8) as i32, y, px(40), px(LETTER_ROW)), first.map(String::from).unwrap_or_default()));
                            y += px(LETTER_ROW) as i32;
                        }
                    }
                    let indent = if child { px(20) } else { 0 };
                    cells.push((Rect::new(list.x + indent as i32, y, row_w - indent, px(LIST_ROW)), target));
                    y += px(LIST_ROW) as i32;
                }
                layout.list_content = (y + self.scroll_list - list.y).max(0) as u32;
            }
        }

        // Низ: пользователь слева; справа маленькие кнопки и питание.
        let user_w = {
            let name = user_name();
            (px(12 + 32 + 10 + 16) + ctx.face(Role::Body).width(&name)).min(card.w / 2)
        };
        cells.push((Rect::new(px(PAD - 6) as i32, foot.y + px(10) as i32, user_w, foot.h - px(20)), Target::User));
        let side = px(36);
        let y = foot.y + (foot.h as i32 - side as i32) / 2;
        let mut x = card.right() - px(PAD) as i32 - side as i32;
        let power: Vec<usize> = (0..self.items.len()).rev().filter(|index| self.items[*index].danger()).collect();
        for index in power {
            cells.push((Rect::new(x, y, side, side), Target::Power(index)));
            x -= (side + px(6)) as i32;
        }
        // Черта между инструментами и питанием.
        x -= px(8) as i32;
        for tool in (0..TOOLS.len()).rev() {
            cells.push((Rect::new(x, y, side, side), Target::Tool(tool)));
            x -= (side + px(6)) as i32;
        }

        // Полосы прокрутки.
        if let Some(list) = layout.list.filter(|list| layout.list_content > list.h) {
            cells.push((Rect::new(list.right() - px(BAR_W + 2) as i32, list.y, px(BAR_W + 2), list.h), Target::Bar(true)));
        }
        if let Some(pins) = layout.pins.filter(|pins| layout.pins_content > pins.h) {
            cells.push((Rect::new(pins.right() - px(BAR_W) as i32, pins.y, px(BAR_W), pins.h), Target::Bar(false)));
        }

        layout.popup = self.popup_layout(&cells, card);
        layout.cells = cells;
        layout.letters = letters;
        layout
    }

    /// Место и строки всплывающего.
    fn popup_layout(&self, cells: &[(Rect, Target)], card: Rect) -> Option<(Rect, Vec<(Rect, Target)>)> {
        use start_menu::*;
        let ctx = desk_ctx(self.scale);
        let px = |value: u32| ctx.px(value);
        let face = ctx.face(Role::Body);
        let fit = |rect: Rect| -> Rect {
            let x = rect.x.clamp(px(4) as i32, (card.right() - rect.w as i32 - px(4) as i32).max(px(4) as i32));
            let y = rect.y.clamp(px(4) as i32, (card.bottom() - rect.h as i32 - px(4) as i32).max(px(4) as i32));
            Rect::new(x, y, rect.w, rect.h)
        };
        match self.popup.as_ref()? {
            Popup::Menu { at, rows } => {
                let width = rows.iter().map(|(label, _, _)| face.width(label)).max().unwrap_or(0) + px(64);
                let h = px(POP_ROW) * rows.len() as u32 + px(12);
                let rect = fit(Rect::new(at.0, at.1, width.max(px(200)), h));
                let lines = (0..rows.len()).map(|n| (Rect::new(rect.x + px(6) as i32, rect.y + px(6) as i32 + (px(POP_ROW) * n as u32) as i32, rect.w - px(12), px(POP_ROW)), Target::Pop(n))).collect();
                Some((rect, lines))
            }
            Popup::User => {
                let user = cells.iter().find(|(_, target)| *target == Target::User).map(|(rect, _)| *rect).unwrap_or(card);
                let rows = 3u32;
                let small = ctx.face(Role::Caption);
                let widest = Self::user_rows().iter().map(|(label, _, _, hint)| face.width(label) + small.width(hint) + px(24)).max().unwrap_or(0);
                let w = (widest + px(56)).max(px(280));
                let h = px(64) + px(POP_ROW) * rows + px(16);
                let rect = fit(Rect::new(user.x, user.y - h as i32 - px(6) as i32, w, h));
                let lines = (0..rows as usize).map(|n| (Rect::new(rect.x + px(6) as i32, rect.y + px(64) as i32 + (px(POP_ROW) * n as u32) as i32, rect.w - px(12), px(POP_ROW)), Target::Pop(n))).collect();
                Some((rect, lines))
            }
            Popup::Folder { folder, cell } => {
                let children: Vec<usize> = (0..self.items.len()).filter(|index| self.items[*index].folder() == folder.key).collect();
                let columns = (children.len() as u32).clamp(1, 3);
                let rows = (children.len() as u32).div_ceil(columns).max(1);
                let w = px(CELL_W) * columns + px(24);
                let h = px(TITLE_H + 10) + px(CELL_H) * rows + px(12);
                // Под плиткой папки, а если там нет места — над ней.
                let below = Rect::new(cell.x + (cell.w as i32 - w as i32) / 2, cell.bottom() - px(6) as i32, w, h);
                let rect = if below.bottom() > card.bottom() - px(FOOT_H) as i32 { Rect::new(below.x, cell.y - h as i32 + px(6) as i32, w, h) } else { below };
                let rect = fit(rect);
                let lines = children
                    .iter()
                    .enumerate()
                    .map(|(n, index)| {
                        let (col, row) = (n as u32 % columns, n as u32 / columns);
                        (Rect::new(rect.x + px(12) as i32 + px(col * CELL_W) as i32, rect.y + px(TITLE_H + 10) as i32 + px(row * CELL_H) as i32, px(CELL_W), px(CELL_H)), Target::Child(*index))
                    })
                    .collect();
                Some((rect, lines))
            }
        }
    }

    /// Что под точкой экрана: цель и её место в карточке. Всплывающее —
    /// первым; прокручиваемое — только в пределах своего поля.
    fn target_at(&self, x: i32, y: i32) -> Option<(Target, Rect)> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        let layout = self.start_layout();
        if let Some((rect, lines)) = &layout.popup {
            if rect.contains(x, y) {
                return lines.iter().find(|(line, _)| line.contains(x, y)).map(|(line, target)| (*target, *line));
            }
        }
        layout.cells.iter().rev().find(|(rect, target)| {
            if !rect.contains(x, y) {
                return false;
            }
            match target {
                Target::Tile(_) => layout.pins.is_some_and(|pins| pins.contains(x, y)),
                Target::Row(_) | Target::FolderRow(_) => layout.list.is_some_and(|list| list.contains(x, y)),
                _ => true,
            }
        }).map(|(rect, target)| (*target, *rect))
    }

    fn clamp_scroll(&mut self) {
        let layout = self.start_layout();
        let list_max = layout.list.map_or(0, |list| layout.list_content.saturating_sub(list.h)) as i32;
        let pins_max = layout.pins.map_or(0, |pins| layout.pins_content.saturating_sub(pins.h)) as i32;
        self.scroll_list = self.scroll_list.clamp(0, list_max);
        self.scroll_pins = self.scroll_pins.clamp(0, pins_max);
    }

    // ── «Пуск» ПК: мышь ─────────────────────────────────────────────────────

    /// Колесо над «Пуском»: прокрутить колонку под указателем. `true` —
    /// что-то сдвинулось.
    pub fn wheel(&mut self, x: i32, y: i32, clicks: i32) -> bool {
        if theme::is_mobile() || !self.rect.contains(x, y) {
            return false;
        }
        let (lx, ly) = (x - self.rect.x, y - self.rect.y);
        let layout = self.start_layout();
        let step = desk_ctx(self.scale).px(start_menu::LIST_ROW) as i32 * 2;
        let before = (self.scroll_list, self.scroll_pins);
        if layout.pins.is_some_and(|pins| pins.contains(lx, ly)) {
            self.scroll_pins -= clicks * step;
        } else {
            self.scroll_list -= clicks * step;
        }
        self.clamp_scroll();
        if (self.scroll_list, self.scroll_pins) == before {
            return false;
        }
        crate::kprintln!("  desktop     : start scrolled: list {}, pins {}", self.scroll_list, self.scroll_pins);
        // Всплывающая папка привязана к плитке — при прокрутке она бы
        // повисла в воздухе.
        if matches!(self.popup, Some(Popup::Folder { .. })) {
            self.popup = None;
        }
        self.hot = self.target_at(x, y).map(|(target, _)| target);
        self.redraw();
        true
    }

    /// Указатель над «Пуском»: подсветка и папка по наведению. `true` —
    /// перерисовано.
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        if theme::is_mobile() || !self.open {
            return false;
        }
        let found = self.target_at(x, y);
        let hot = found.map(|(target, _)| target);
        let mut changed = hot != self.hot;
        self.hot = hot;
        // Плитка-папка раскрывается наведением; уход и с плитки, и с
        // раскрытого закрывает её.
        match found {
            Some((Target::Tile(n), rect)) => {
                if let Some(Tile::Folder(folder)) = self.tiles.get(n).copied() {
                    let open_here = matches!(&self.popup, Some(Popup::Folder { folder: shown, .. }) if shown.key == folder.key);
                    if !open_here && !matches!(self.popup, Some(Popup::Menu { .. } | Popup::User)) {
                        self.popup = Some(Popup::Folder { folder, cell: rect });
                        crate::kprintln!("  desktop     : start folder '{}' opened", folder.caption);
                        self.log_popup();
                        changed = true;
                    }
                } else if matches!(self.popup, Some(Popup::Folder { .. })) {
                    self.popup = None;
                    changed = true;
                }
            }
            Some((Target::Child(_), _)) => {}
            _ => {
                if let Some(Popup::Folder { cell, .. }) = &self.popup {
                    let (lx, ly) = (x - self.rect.x, y - self.rect.y);
                    let keep = cell.contains(lx, ly) || self.start_layout().popup.is_some_and(|(rect, _)| rect.contains(lx, ly));
                    if !keep {
                        self.popup = None;
                        changed = true;
                    }
                }
            }
        }
        if changed {
            self.redraw();
        }
        changed
    }

    /// Щелчок левой кнопкой (координаты экрана).
    pub fn click(&mut self, x: i32, y: i32) -> MenuClick {
        if theme::is_mobile() {
            return match self.choice_at(x, y) {
                Some(choice) => {
                    self.select_at(x, y);
                    MenuClick::Launch(choice)
                }
                None => MenuClick::Close,
            };
        }
        if !self.rect.contains(x, y) {
            return MenuClick::Close;
        }
        let found = self.target_at(x, y);
        // Щелчок мимо открытого меню программы или пользователя закрывает
        // только его.
        if matches!(self.popup, Some(Popup::Menu { .. } | Popup::User)) && !matches!(found, Some((Target::Pop(_), _))) {
            self.popup = None;
            self.redraw();
            return MenuClick::Stay;
        }
        let Some((target, rect)) = found else {
            if self.popup.take().is_some() {
                self.redraw();
            }
            return MenuClick::Stay;
        };
        match target {
            Target::Tile(n) => match self.tiles.get(n).copied() {
                Some(Tile::Item(index)) => self.launch(index),
                Some(Tile::Folder(folder)) => {
                    let again = matches!(&self.popup, Some(Popup::Folder { folder: shown, .. }) if shown.key == folder.key);
                    self.popup = if again { None } else { Some(Popup::Folder { folder, cell: rect }) };
                    self.redraw();
                    MenuClick::Stay
                }
                None => MenuClick::Stay,
            },
            Target::Row(index) | Target::Child(index) | Target::Power(index) => self.launch(index),
            Target::FolderRow(number) => {
                let key = startapps::FOLDERS[number].key;
                match self.expanded.iter().position(|known| *known == key) {
                    Some(at) => {
                        self.expanded.remove(at);
                        crate::kprintln!("  desktop     : start folder '{}' collapsed", startapps::FOLDERS[number].caption);
                    }
                    None => {
                        self.expanded.push(key);
                        crate::kprintln!("  desktop     : start folder '{}' expanded", startapps::FOLDERS[number].caption);
                    }
                }
                self.clamp_scroll();
                self.redraw();
                MenuClick::Stay
            }
            Target::Recent(n) => match self.recent.get(n) {
                Some((command, path)) => MenuClick::Launch(Choice::Recent(command.clone(), path.clone())),
                None => MenuClick::Stay,
            },
            Target::User => {
                self.popup = if matches!(self.popup, Some(Popup::User)) { None } else { Some(Popup::User) };
                self.redraw();
                if self.popup.is_some() {
                    crate::kprintln!("  desktop     : start user menu opened");
                    self.log_popup();
                }
                MenuClick::Stay
            }
            Target::Tool(tool) => MenuClick::Launch(match tool {
                0 => Choice::Program("taskmgr", ""),
                1 => Choice::Program("devmgr", ""),
                _ => Choice::App(App::Settings),
            }),
            Target::Bar(list) => {
                // Щелчок по полосе — на страницу к месту щелчка.
                let layout = self.start_layout();
                let field = if list { layout.list } else { layout.pins };
                if let Some(field) = field {
                    let ly = y - self.rect.y;
                    let content = if list { layout.list_content } else { layout.pins_content };
                    let span = content.saturating_sub(field.h) as i64;
                    let at = i64::from((ly - field.y).clamp(0, field.h as i32));
                    let value = (span * at / i64::from(field.h.max(1))) as i32;
                    if list {
                        self.scroll_list = value;
                    } else {
                        self.scroll_pins = value;
                    }
                    self.clamp_scroll();
                    self.redraw();
                }
                MenuClick::Stay
            }
            Target::Pop(n) => self.popup_pick(n),
            Target::Search => MenuClick::Stay,
        }
    }

    /// Запустить строку `index`.
    fn launch(&mut self, index: usize) -> MenuClick {
        if let Some(at) = self.visible.iter().position(|known| *known == index) {
            self.row = at;
        }
        match self.items.get(index).and_then(|item| item.choice(&self.packages)) {
            Some(choice) => MenuClick::Launch(choice),
            None => MenuClick::Stay,
        }
    }

    /// Правая кнопка над «Пуском»: меню программы. `false` — точка не над
    /// «Пуском».
    pub fn context(&mut self, x: i32, y: i32) -> bool {
        if theme::is_mobile() || !self.rect.contains(x, y) {
            return false;
        }
        let at = (x - self.rect.x, y - self.rect.y);
        let target = self.target_at(x, y).map(|(target, _)| target);
        let (item, folder, in_pins) = match target {
            Some(Target::Tile(n)) => match self.tiles.get(n).copied() {
                Some(Tile::Item(index)) => (Some(index), None, true),
                Some(Tile::Folder(folder)) => (None, Some(folder), true),
                None => (None, None, false),
            },
            Some(Target::Row(index) | Target::Child(index)) => (Some(index), None, false),
            Some(Target::FolderRow(number)) => (None, Some(&startapps::FOLDERS[number]), false),
            _ => (None, None, false),
        };
        let key = match (item, folder) {
            (Some(index), _) => self.items[index].key(&self.packages),
            (None, Some(folder)) => String::from(folder.key),
            _ => {
                if self.popup.take().is_some() {
                    self.redraw();
                }
                return true;
            }
        };
        let mut rows = Vec::new();
        if let Some(index) = item {
            rows.push((String::from("Открыть"), PopAct::Open(index), true));
        }
        if deskcfg::start_pinned(&key) {
            if in_pins {
                let first = self.tiles.first().is_some_and(|tile| match tile {
                    Tile::Item(index) => Some(*index) == item,
                    Tile::Folder(shown) => folder.is_some_and(|folder| folder.key == shown.key),
                });
                if !first {
                    rows.push((String::from("Переместить в начало"), PopAct::First(key.clone()), true));
                }
            }
            rows.push((String::from("Открепить от «Пуска»"), PopAct::UnpinStart(key.clone()), true));
        } else {
            rows.push((String::from("Закрепить в «Пуске»"), PopAct::PinStart(key.clone()), true));
        }
        // Папку на панель задач не закрепить: кнопка панели запускает одну
        // программу.
        if item.is_some() {
            if deskcfg::task_pinned(&key) {
                rows.push((String::from("Открепить от панели задач"), PopAct::UnpinTask(key.clone()), true));
            } else {
                rows.push((String::from("Закрепить на панели задач"), PopAct::PinTask(key.clone()), true));
            }
        }
        crate::kprintln!("  desktop     : start context menu for '{key}'");
        self.popup = Some(Popup::Menu { at, rows });
        self.hot = None;
        self.redraw();
        self.log_popup();
        true
    }

    /// Строки меню пользователя.
    fn user_rows() -> [(&'static str, PopAct, bool, &'static str); 3] {
        [
            ("Параметры учётной записи", PopAct::Account, true, ""),
            ("Заблокировать", PopAct::Lock, true, "Win+L"),
            // Выйти из профиля пока нельзя: сеанс у стола один.
            ("Выйти из профиля", PopAct::Logout, false, "функция запланирована"),
        ]
    }

    /// Выбрана строка всплывающего.
    fn popup_pick(&mut self, n: usize) -> MenuClick {
        let act = match self.popup.as_ref() {
            Some(Popup::Menu { rows, .. }) => match rows.get(n) {
                Some((_, act, true)) => act.clone(),
                _ => return MenuClick::Stay,
            },
            Some(Popup::User) => match Self::user_rows().get(n) {
                Some((_, act, true, _)) => act.clone(),
                _ => return MenuClick::Stay,
            },
            _ => return MenuClick::Stay,
        };
        self.popup = None;
        let stay = |menu: &mut Self| {
            menu.load_tiles();
            menu.clamp_scroll();
            menu.redraw();
            MenuClick::Stay
        };
        match act {
            PopAct::Open(index) => self.launch(index),
            PopAct::PinStart(key) | PopAct::UnpinStart(key) => {
                let pinned = deskcfg::toggle_start_pin(&key);
                crate::kprintln!("  desktop     : start {} '{key}'", if pinned { "pinned" } else { "unpinned" });
                stay(self)
            }
            PopAct::First(key) => {
                deskcfg::start_pin_first(&key);
                crate::kprintln!("  desktop     : start pin '{key}' moved first");
                stay(self)
            }
            PopAct::PinTask(key) | PopAct::UnpinTask(key) => {
                let pinned = deskcfg::toggle_task_pin(&key);
                crate::kprintln!("  desktop     : taskbar {} '{key}'", if pinned { "pinned" } else { "unpinned" });
                stay(self)
            }
            PopAct::Account => MenuClick::Launch(Choice::Settings(super::settings::Section::Accounts)),
            PopAct::Lock => MenuClick::Launch(Choice::Lock),
            PopAct::Logout => MenuClick::Stay,
        }
    }

    // ── «Пуск» ПК: рисование ────────────────────────────────────────────────

    /// Нарисовать «Пуск» ПК.
    fn redraw_start(&mut self) {
        use start_menu::*;
        let ctx = desk_ctx(self.scale);
        let p = ctx.palette;
        let px = |value: u32| ctx.px(value);
        let layout = self.start_layout();
        let card = layout.card;
        let fill = p.tb1.over(p.tb2.color);
        self.surface.fill(card, fill);
        draw::rounded_stroke(&mut self.surface, card, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let ctx = ctx.on(fill);
        let hover = p.btnhov.over(fill);
        let selected = if self.keys_used { self.visible.get(self.row).copied() } else { None };

        // Поиск.
        let field = layout.search;
        draw::rounded(&mut self.surface, field, px(theme::R_ROW), p.sunk.over(fill), 255);
        let focus_line = if self.query.is_empty() { p.line3 } else { p.accline };
        draw::rounded_stroke(&mut self.surface, field, px(theme::R_ROW), focus_line.color, focus_line.alpha);
        let glyph = px(14);
        glyphicon::draw(&mut self.surface, Icon::Search, field.x + px(12) as i32, field.y + (field.h as i32 - glyph as i32) / 2, glyph, p.ink4, 255);
        let text_x = field.x + px(34) as i32;
        let room = field.right().saturating_sub(text_x + px(12) as i32).max(0) as u32;
        if self.query.is_empty() {
            paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, paint::baseline(ctx, Role::Body, field), room, "Поиск программ", p.ink4);
        } else {
            let used = paint::text_clipped(ctx, &mut self.surface, Role::Body, text_x, paint::baseline(ctx, Role::Body, field), room, &self.query, p.ink);
            let caret = Rect::new(text_x + used as i32 + 1, field.y + px(9) as i32, px(1).max(1), field.h - px(18));
            self.surface.fill(caret, p.acc);
        }

        // Заголовки.
        for (title, text) in [
            (layout.pins_title, "Закреплённые"),
            (layout.recent_title, "Недавние"),
            (layout.list_title, if self.query.is_empty() { "Все программы" } else { "Результаты поиска" }),
        ] {
            if let Some(title) = title {
                paint::text(ctx, &mut self.surface, Role::Title, title.x + px(4) as i32, paint::baseline(ctx, Role::Title, title), text, p.ink);
            }
        }
        if let (Some(title), Some(list)) = (layout.list_title, layout.list) {
            // Сколько всего программ — справа от заголовка, мелко.
            let count = self.items.iter().filter(|item| !item.danger()).count();
            let note = if self.query.is_empty() { alloc::format!("{count}") } else { alloc::format!("{}", self.visible.iter().filter(|index| !self.items[**index].danger()).count()) };
            paint::text_right(ctx, &mut self.surface, Role::Caption, list.right() - px(8) as i32, paint::baseline(ctx, Role::Caption, title), &note, p.ink4);
        }

        // Закреплённые — в своей поверхности, срезанной по полю.
        if let Some(pins) = layout.pins {
            if let Some(mut scratch) = self.take_scratch(pins.w, pins.h, fill) {
                for (rect, target) in layout.cells.iter().filter(|(_, target)| matches!(target, Target::Tile(_))) {
                    let Target::Tile(n) = *target else { continue };
                    let local = rect.translate(-pins.x, -pins.y);
                    if local.bottom() < 0 || local.y > pins.h as i32 {
                        continue;
                    }
                    let tile = self.tiles[n];
                    let open_folder = matches!((&self.popup, tile), (Some(Popup::Folder { folder, .. }), Tile::Folder(shown)) if folder.key == shown.key);
                    let chosen = matches!(tile, Tile::Item(index) if Some(index) == selected);
                    if self.hot == Some(*target) || open_folder || chosen {
                        draw::rounded(&mut scratch, local.shrink(px(2)), px(theme::R_ROW + 2), hover, 255);
                    }
                    if chosen {
                        draw::rounded_stroke(&mut scratch, local.shrink(px(2)), px(theme::R_ROW + 2), p.accline.color, p.accline.alpha);
                    }
                    let side = px(36);
                    let icon = Rect::new(local.x + (local.w as i32 - side as i32) / 2, local.y + px(10) as i32, side, side);
                    match tile {
                        Tile::Item(index) => draw_item_icon(ctx, &mut scratch, icon, self.items[index], &self.packages),
                        Tile::Folder(folder) => self.draw_folder_icon(ctx, &mut scratch, icon, folder),
                    }
                    let caption = self.target_caption(*target);
                    let label = Rect::new(local.x + px(4) as i32, icon.bottom() + px(6) as i32, local.w - px(8), px(18));
                    tile_caption(ctx, &mut scratch, label, caption, p.ink2);
                }
                if layout.pins_content > pins.h {
                    draw_bar(&mut scratch, ctx, pins.h, layout.pins_content, self.scroll_pins, self.hot == Some(Target::Bar(false)));
                }
                self.surface.blit_from(&scratch, (pins.x, pins.y), scratch.bounds());
                self.scratch = Some(scratch);
            }
            if self.tiles.is_empty() {
                paint::text(ctx, &mut self.surface, Role::Caption, pins.x + px(4) as i32, pins.y + px(8) as i32, "Правая кнопка на программе — «Закрепить в «Пуске»»", p.ink4);
            }
        }

        // Недавние.
        if layout.recent_title.is_some() {
            let rows: Vec<(Rect, usize)> = layout.cells.iter().filter_map(|(rect, target)| if let Target::Recent(n) = target { Some((*rect, *n)) } else { None }).collect();
            if self.recent.is_empty() {
                if let Some(title) = layout.recent_title {
                    let row = Rect::new(title.x + px(4) as i32, title.bottom(), title.w, px(RECENT_ROW));
                    paint::text(ctx, &mut self.surface, Role::Caption, row.x, paint::baseline(ctx, Role::Caption, row), "Здесь появятся файлы, которые вы открывали", p.ink4);
                }
            }
            for (rect, n) in rows {
                let Some((_, path)) = self.recent.get(n) else { continue };
                if self.hot == Some(Target::Recent(n)) {
                    draw::rounded(&mut self.surface, rect, px(theme::R_ROW), hover, 255);
                }
                let side = px(28);
                let icon = Rect::new(rect.x + px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
                let art = super::appicon::design("doc-text").or_else(|| super::appicon::design("doc-document"));
                match art {
                    Some(image) => image.draw(&mut self.surface, icon),
                    None => paint::icon_tile(ctx, &mut self.surface, icon, Icon::File, Tone::Accent, false),
                }
                let x = icon.right() + px(10) as i32;
                let room = (rect.right() - x - px(6) as i32).max(0) as u32;
                let name = context_base_name(path);
                let dir = path.strip_suffix(name).unwrap_or(path).trim_end_matches('/');
                let top = rect.y + px(5) as i32;
                paint::text_clipped(ctx, &mut self.surface, Role::Body, x, top, room, name, p.ink2);
                paint::text_clipped(ctx, &mut self.surface, Role::Caption, x, top + i32::from(ctx.face(Role::Body).line), room, if dir.is_empty() { "/" } else { dir }, p.ink4);
            }
        }

        // Список — в своей поверхности.
        if let Some(list) = layout.list {
            if let Some(mut scratch) = self.take_scratch(list.w, list.h, fill) {
                for (rect, mark) in &layout.letters {
                    let local = rect.translate(-list.x, -list.y);
                    if local.bottom() < 0 || local.y > list.h as i32 {
                        continue;
                    }
                    paint::text(ctx, &mut scratch, Role::Label, local.x, paint::baseline(ctx, Role::Label, local), mark, p.acc_ink);
                }
                for (rect, target) in layout.cells.iter().filter(|(_, target)| matches!(target, Target::Row(_) | Target::FolderRow(_))) {
                    let local = rect.translate(-list.x, -list.y);
                    if local.bottom() < 0 || local.y > list.h as i32 {
                        continue;
                    }
                    let chosen = matches!(target, Target::Row(index) if Some(*index) == selected);
                    if self.hot == Some(*target) || chosen {
                        draw::rounded(&mut scratch, local, px(theme::R_ROW), hover, 255);
                    }
                    if chosen {
                        draw::rounded_stroke(&mut scratch, local, px(theme::R_ROW), p.accline.color, p.accline.alpha);
                    }
                    let side = px(22);
                    let icon = Rect::new(local.x + px(8) as i32, local.y + (local.h as i32 - side as i32) / 2, side, side);
                    let caption = self.target_caption(*target);
                    match *target {
                        Target::Row(index) => draw_item_icon(ctx, &mut scratch, icon, self.items[index], &self.packages),
                        Target::FolderRow(number) => self.draw_folder_icon(ctx, &mut scratch, icon, &startapps::FOLDERS[number]),
                        _ => {}
                    }
                    let x = icon.right() + px(10) as i32;
                    let mut room = (local.right() - x - px(6) as i32).max(0) as u32;
                    if let Target::FolderRow(number) = *target {
                        // Стрелка раскрытия справа.
                        let open = self.expanded.contains(&startapps::FOLDERS[number].key);
                        let arrow = px(14);
                        let icon = if open { Icon::ChevronDown } else { Icon::ChevronRight };
                        glyphicon::draw(&mut scratch, icon, local.right() - px(10) as i32 - arrow as i32, local.y + (local.h as i32 - arrow as i32) / 2, arrow, p.ink3, 255);
                        room = room.saturating_sub(arrow + px(10));
                    }
                    paint::text_clipped(ctx, &mut scratch, Role::Body, x, paint::baseline(ctx, Role::Body, local), room, caption, p.ink2);
                }
                if layout.list_content > list.h {
                    draw_bar(&mut scratch, ctx, list.h, layout.list_content, self.scroll_list, self.hot == Some(Target::Bar(true)));
                }
                self.surface.blit_from(&scratch, (list.x, list.y), scratch.bounds());
                self.scratch = Some(scratch);
            }
            if !self.query.is_empty() && !self.visible.iter().any(|index| !self.items[*index].danger()) {
                paint::text(ctx, &mut self.surface, Role::Body, list.x + px(4) as i32, list.y + px(8) as i32, "Ничего не найдено", p.ink4);
            }
        }

        // Низ: черта, пользователь, инструменты, питание.
        let foot = layout.foot;
        draw::hline(&mut self.surface, 0, foot.y, card.w, p.line2.color, p.line2.alpha);
        let mut tip: Option<(&str, i32, bool)> = None;
        let leftmost = layout.cells.iter().filter(|(_, target)| matches!(target, Target::Tool(_) | Target::Power(_))).map(|(rect, _)| rect.x).min().unwrap_or(card.right());
        for (rect, target) in &layout.cells {
            match *target {
                Target::User => {
                    let user = user_name();
                    if self.hot == Some(Target::User) || matches!(self.popup, Some(Popup::User)) {
                        draw::rounded(&mut self.surface, *rect, px(theme::R_ROW), hover, 255);
                    }
                    let avatar = Rect::new(rect.x + px(6) as i32, rect.y + (rect.h as i32 - px(32) as i32) / 2, px(32), px(32));
                    draw_avatar(ctx, &mut self.surface, avatar, &user);
                    let name_row = Rect::new(avatar.right() + px(10) as i32, rect.y, rect.w.saturating_sub(px(48)), rect.h);
                    paint::text_clipped(ctx, &mut self.surface, Role::Body, name_row.x, paint::baseline(ctx, Role::Body, name_row), name_row.w, &user, p.ink);
                }
                Target::Tool(tool) => {
                    let hot = self.hot == Some(*target);
                    let plate = if hot { hover } else { p.btn.over(fill) };
                    draw::rounded(&mut self.surface, *rect, px(theme::R_ROW), plate, 255);
                    let glyph = px(16);
                    iconset::draw(&mut self.surface, TOOLS[tool].1, rect.x + (rect.w as i32 - glyph as i32) / 2, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, if hot { p.ink } else { p.ink2 }, 255);
                    if hot {
                        tip = Some((TOOLS[tool].0, rect.y, false));
                    }
                    // Черта между инструментами и питанием.
                    if tool == TOOLS.len() - 1 {
                        let x = rect.right() + px(7) as i32;
                        draw::vline(&mut self.surface, x, rect.y + px(8) as i32, rect.h - px(16), p.line2.color, p.line2.alpha);
                    }
                }
                Target::Power(index) => {
                    let item = self.items[index];
                    let hot = self.hot == Some(*target) || Some(index) == selected;
                    let plate = if hot { hover } else { p.btn.over(fill) };
                    draw::rounded(&mut self.surface, *rect, px(theme::R_ROW), plate, 255);
                    let glyph = px(16);
                    let icon = if matches!(item, Item::App(App::Restart)) { Icon::Update } else { Icon::Power };
                    let ink = if hot { p.bad_ink } else { p.ink2 };
                    glyphicon::draw(&mut self.surface, icon, rect.x + (rect.w as i32 - glyph as i32) / 2, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, ink, 255);
                    if hot {
                        tip = Some((item.caption(&self.packages), rect.y, true));
                    }
                }
                _ => {}
            }
        }
        // Подпись кнопки под указателем — слева от кнопок.
        if let Some((caption, y, danger)) = tip {
            let row = Rect::new(0, y, 1, px(36));
            paint::text_right(ctx, &mut self.surface, Role::Body, leftmost - px(12) as i32, paint::baseline(ctx, Role::Body, row), caption, if danger { p.bad_ink } else { p.ink2 });
        }

        // Всплывающее — поверх всего.
        if let Some((rect, lines)) = &layout.popup {
            self.draw_popup(ctx.on(fill), *rect, lines);
        }
        self.damage = self.surface.bounds();
    }

    /// Поверхность для прокручиваемой колонки нужного размера.
    fn take_scratch(&mut self, w: u32, h: u32, fill: Color) -> Option<Surface> {
        if w == 0 || h == 0 {
            return None;
        }
        match self.scratch.take() {
            Some(mut scratch) if scratch.width() == w && scratch.height() == h => {
                scratch.fill(scratch.bounds(), fill);
                Some(scratch)
            }
            _ => Surface::new(w, h, fill),
        }
    }

    /// Значок папки: плитка с уменьшенными значками первых программ.
    fn draw_folder_icon(&self, ctx: Ctx, s: &mut Surface, rect: Rect, folder: &startapps::Folder) {
        let p = ctx.palette;
        let radius = (rect.w / 4).max(2);
        draw::rounded(s, rect, radius, p.btn.color, p.btn.alpha.saturating_add(40));
        draw::rounded_stroke(s, rect, radius, p.btnline.color, p.btnline.alpha);
        let children: Vec<Item> = self.items.iter().copied().filter(|item| item.folder() == folder.key).take(4).collect();
        let inner = rect.w.saturating_sub(rect.w / 4);
        let mini = (inner / 2).saturating_sub(1).max(1);
        let x0 = rect.x + (rect.w as i32 - (mini * 2 + 2) as i32) / 2;
        let y0 = rect.y + (rect.h as i32 - (mini * 2 + 2) as i32) / 2;
        for (n, item) in children.into_iter().enumerate() {
            let at = Rect::new(x0 + ((n as u32 % 2) * (mini + 2)) as i32, y0 + ((n as u32 / 2) * (mini + 2)) as i32, mini, mini);
            draw_item_icon(ctx, s, at, item, &self.packages);
        }
    }

    /// Всплывающее меню, меню пользователя или раскрытая папка.
    fn draw_popup(&mut self, ctx: Ctx, rect: Rect, lines: &[(Rect, Target)]) {
        let p = ctx.palette;
        let px = |value: u32| ctx.px(value);
        let fill = p.tb1.over(p.tb2.color);
        // Тень — тремя ступенями прозрачной подложки вниз-вправо: композитор
        // кладёт тень только под весь «Пуск».
        for step in (1..=3u32).rev() {
            let shadow = Rect::new(rect.x + px(step) as i32 / 2, rect.y + px(step) as i32, rect.w, rect.h);
            draw::rounded(&mut self.surface, shadow, px(theme::R_WINDOW), Color::rgb(0, 0, 0), 30);
        }
        draw::rounded(&mut self.surface, rect, px(theme::R_WINDOW), fill, 255);
        draw::rounded_stroke(&mut self.surface, rect, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let ctx = ctx.on(fill);
        let hover = p.btnhov.over(fill);
        match self.popup.clone() {
            Some(Popup::Menu { rows, .. }) => {
                for ((line, target), (label, act, enabled)) in lines.iter().zip(rows.iter()) {
                    if self.hot == Some(*target) && *enabled {
                        draw::rounded(&mut self.surface, *line, px(theme::R_ROW), hover, 255);
                    }
                    let glyph = px(14);
                    let icon = match act {
                        PopAct::Open(_) => Some(Fi::ArrowRight),
                        PopAct::First(_) => Some(Fi::ArrowUp),
                        _ => Some(Fi::Pin),
                    };
                    if let Some(icon) = icon {
                        iconset::draw(&mut self.surface, icon, line.x + px(10) as i32, line.y + (line.h as i32 - glyph as i32) / 2, glyph, p.ink3, 255);
                    }
                    let ink = if *enabled { p.ink } else { p.ink4 };
                    paint::text(ctx, &mut self.surface, Role::Body, line.x + px(34) as i32, paint::baseline(ctx, Role::Body, *line), label, ink);
                }
            }
            Some(Popup::User) => {
                let user = user_name();
                let avatar = Rect::new(rect.x + px(14) as i32, rect.y + px(14) as i32, px(40), px(40));
                draw_avatar(ctx, &mut self.surface, avatar, &user);
                let x = avatar.right() + px(12) as i32;
                paint::text(ctx, &mut self.surface, Role::Title, x, avatar.y + px(2) as i32, &user, p.ink);
                paint::text(ctx, &mut self.surface, Role::Caption, x, avatar.y + px(22) as i32, "Локальная учётная запись", p.ink3);
                draw::hline(&mut self.surface, rect.x + px(10) as i32, rect.y + px(62) as i32, rect.w - px(20), p.line2.color, p.line2.alpha);
                for ((line, target), (label, act, enabled, hint)) in lines.iter().zip(Self::user_rows().iter()) {
                    if self.hot == Some(*target) && *enabled {
                        draw::rounded(&mut self.surface, *line, px(theme::R_ROW), hover, 255);
                    }
                    let glyph = px(15);
                    let icon = match act {
                        PopAct::Account => Fi::User,
                        PopAct::Lock => Fi::Lock,
                        _ => Fi::ArrowLeft,
                    };
                    let ink = if *enabled { p.ink } else { p.ink4 };
                    iconset::draw(&mut self.surface, icon, line.x + px(10) as i32, line.y + (line.h as i32 - glyph as i32) / 2, glyph, if *enabled { p.ink2 } else { p.ink4 }, 255);
                    paint::text(ctx, &mut self.surface, Role::Body, line.x + px(34) as i32, paint::baseline(ctx, Role::Body, *line), label, ink);
                    if !hint.is_empty() {
                        paint::text_right(ctx, &mut self.surface, Role::Caption, line.right() - px(10) as i32, paint::baseline(ctx, Role::Caption, *line), hint, p.ink4);
                    }
                }
            }
            Some(Popup::Folder { folder, .. }) => {
                let title = Rect::new(rect.x + px(16) as i32, rect.y + px(8) as i32, rect.w - px(32), px(start_menu::TITLE_H));
                paint::text(ctx, &mut self.surface, Role::Title, title.x, paint::baseline(ctx, Role::Title, title), folder.caption, p.ink);
                for (line, target) in lines {
                    let Target::Child(index) = *target else { continue };
                    if self.hot == Some(*target) {
                        draw::rounded(&mut self.surface, line.shrink(px(2)), px(theme::R_ROW + 2), hover, 255);
                    }
                    let side = px(36);
                    let icon = Rect::new(line.x + (line.w as i32 - side as i32) / 2, line.y + px(10) as i32, side, side);
                    draw_item_icon(ctx, &mut self.surface, icon, self.items[index], &self.packages);
                    let caption = self.items[index].caption(&self.packages);
                    let label = Rect::new(line.x + px(4) as i32, icon.bottom() + px(6) as i32, line.w - px(8), px(18));
                    tile_caption(ctx, &mut self.surface, label, caption, p.ink2);
                }
            }
            None => {}
        }
    }

    /// Раскладка карточки телефона: где шапка, где заголовок колонки, где
    /// строки.
    fn layout(&self) -> MenuLayout {
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
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
            // это соседство, в котором однажды промахиваются.
            if !ruled && item.danger() && index > 0 {
                ruled = true;
                rules.push(Rect::new(left_x + m.row_pad as i32, y + ctx.px(6) as i32, self.width.saturating_sub(m.row_pad * 2), 1));
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
        if !theme::is_mobile() {
            self.redraw_start();
            return;
        }
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        let p = ctx.palette;
        let layout = self.layout();

        let card = layout.card;
        self.surface.fill(card, ctx.under);
        draw::rounded_stroke(&mut self.surface, card, m.round, p.line3.color, p.line3.alpha);
        draw::crown(&mut self.surface, card, m.round, p.crown.color, p.crown.alpha);

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
        paint::text(ctx, &mut self.surface, Role::MonoSmall, layout.note.0, layout.note.1, crate::VERSION, p.ink4);

        paint::caps(ctx, &mut self.surface, layout.caps.0, layout.caps.1, APPS_TITLE);

        for rule in &layout.rules {
            paint::separator(ctx, &mut self.surface, rule.x, rule.y, rule.w);
        }

        for row in &layout.rows {
            let Some(item) = self.items.get(row.index).copied() else {
                continue;
            };
            let art = match item {
                Item::Package(index) => self.packages.get(index).map(|launcher| super::appicon::of_manifest(&launcher.icon)),
                Item::Entry(entry) => Some(startapps::art(entry)),
                Item::App(_) => None,
            };
            draw_row(m, &mut self.surface, row, item.caption(&self.packages), item.about(&self.packages), row.index == self.row, art.as_ref());
        }

        self.damage = self.surface.bounds();
    }
}

/// Где встаёт «Пуск» ПК: у кнопки панели, с какой бы стороны она ни стояла.
/// Панель — у начала стороны (там её кнопка), Dock — по центру.
fn menu_place(area: Rect, panel: Rect, cfg: deskcfg::Taskbar, w: u32, h: u32, gap: u32) -> Rect {
    let gap = gap as i32;
    let dock = cfg.mode == deskcfg::Mode::Dock;
    let clamp_x = |x: i32| x.clamp(area.x + gap, (area.right() - w as i32 - gap).max(area.x + gap));
    let clamp_y = |y: i32| y.clamp(area.y + gap, (area.bottom() - h as i32 - gap).max(area.y + gap));
    let (x, y) = match cfg.edge {
        deskcfg::Edge::Bottom | deskcfg::Edge::Top => {
            let x = if dock { panel.x + (panel.w as i32 - w as i32) / 2 } else { area.x + gap };
            let y = if cfg.edge == deskcfg::Edge::Bottom { panel.y - gap - h as i32 } else { panel.bottom() + gap };
            (clamp_x(x), clamp_y(y))
        }
        deskcfg::Edge::Left | deskcfg::Edge::Right => {
            let y = if dock { panel.y + (panel.h as i32 - h as i32) / 2 } else { area.y + gap };
            let x = if cfg.edge == deskcfg::Edge::Left { panel.right() + gap } else { panel.x - gap - w as i32 };
            (clamp_x(x), clamp_y(y))
        }
    };
    Rect::new(x, y, w, h)
}

/// Тонкая полоса прокрутки у правого края поверхности колонки.
fn draw_bar(s: &mut Surface, ctx: Ctx, view: u32, content: u32, scroll: i32, hot: bool) {
    let p = ctx.palette;
    let w = ctx.px(if hot { 6 } else { 4 });
    let x = s.width() as i32 - w as i32 - ctx.px(1) as i32;
    let track = Rect::new(x, 0, w, view);
    draw::rounded(s, track, w / 2, p.ink6, 60);
    let thumb_h = ((u64::from(view) * u64::from(view)) / u64::from(content.max(1))).max(u64::from(ctx.px(24))) as u32;
    let span = content.saturating_sub(view).max(1);
    let y = ((u64::from(view - thumb_h.min(view)) * scroll.max(0) as u64) / u64::from(span)) as i32;
    draw::rounded(s, Rect::new(x, y, w, thumb_h.min(view)), w / 2, p.ink4, if hot { 230 } else { 170 });
}

/// Подпись плитки «Пуска»: по центру, в две строки, если в одну не
/// помещается (перенос по пробелу, как у Windows), вторая — с многоточием.
fn tile_caption(ctx: Ctx, s: &mut Surface, label: Rect, caption: &str, ink: Color) {
    let face = ctx.face(Role::Caption);
    let line = i32::from(face.line);
    let centered = |s: &mut Surface, text: &str, y: i32| {
        let width = face.width(text).min(label.w);
        paint::text_clipped(ctx, s, Role::Caption, label.x + (label.w as i32 - width as i32) / 2, y, label.w, text, ink);
    };
    if face.width(caption) <= label.w {
        centered(s, caption, label.y);
        return;
    }
    // Самый длинный префикс по пробелу, который помещается.
    let mut split = None;
    for (at, ch) in caption.char_indices() {
        if ch == ' ' && face.width(&caption[..at]) <= label.w {
            split = Some(at);
        }
    }
    match split {
        Some(at) => {
            centered(s, &caption[..at], label.y);
            centered(s, caption[at..].trim_start(), label.y + line);
        }
        None => centered(s, caption, label.y),
    }
}

/// Круг с первой буквой имени.
fn draw_avatar(ctx: Ctx, s: &mut Surface, rect: Rect, user: &str) {
    let p = ctx.palette;
    draw::rounded_gradient(s, rect, rect.w / 2, p.acc, p.acc2, 255);
    let initial: String = user.chars().next().map(|ch| ch.to_uppercase().collect()).unwrap_or_default();
    paint::text_centered(ctx.on(p.acc), s, Role::Title, rect, &initial, WHITE);
}

/// Имя того, кто вошёл; без сеанса — `root`.
fn user_name() -> String {
    let user = crate::user::session::with_name(|name| String::from(name));
    if user.is_empty() { String::from("root") } else { user }
}

/// Имя файла из пути.
fn context_base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Значок строки «Пуска»: значок макета, если есть, иначе знак на плитке.
fn draw_item_icon(ctx: Ctx, s: &mut Surface, rect: Rect, item: Item, packages: &[Launcher]) {
    match item {
        Item::Entry(entry) => {
            startapps::art(entry).draw(ctx, s, rect);
            return;
        }
        // Пакет со своим значком (поле `icon` манифеста, 08.11.2026).
        Item::Package(index) => {
            if let Some(launcher) = packages.get(index) {
                if launcher.icon.starts_with('/') || super::appicon::design(&launcher.icon).is_some() {
                    super::appicon::of_manifest(&launcher.icon).draw(ctx, s, rect);
                    return;
                }
            }
        }
        Item::App(_) => {}
    }
    match super::appicon::design_for(item.icon()) {
        Some(image) => image.draw(s, rect),
        None => paint::icon_tile(ctx, s, rect, item.icon(), item.tone(), false),
    }
}

/// Одна строка меню.
///
/// Свободной функцией, а не методом: строке нужна поверхность и ничего больше,
/// а подпись к ней лежит в том же `self`, что и поверхность, — методу пришлось
/// бы одолжить оба поля сразу.
fn draw_row(
    m: Metrics,
    s: &mut Surface,
    row: &MenuRow,
    label: &str,
    note: &str,
    selected: bool,
    art: Option<&super::appicon::Art>,
) {
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
        match (art, super::appicon::design_for(row.icon)) {
            (Some(art), _) => art.draw(ctx, s, tile),
            (None, Some(image)) => image.draw(s, tile),
            (None, None) => paint::icon_tile(ctx, s, tile, row.icon, row.tone, selected),
        }
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
    // `vdso` — код времени программ Linux, а не программа (01.11.2026).
    const HIDDEN: [&str; 2] = ["init", "vdso"];
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

/// Показывать ли время работы рядом с часами («Параметры» → «Dock и панель
/// задач», фаза 69b). Атомики, а не поле панели: настройку читает и
/// загрузка (`prefs::adopt`), когда панели ещё нет.
static SHOW_UPTIME: AtomicBool = AtomicBool::new(false);
/// Показывать ли дату рядом с часами.
static SHOW_DATE: AtomicBool = AtomicBool::new(true);

/// Кнопки панели задач с подписями — по кнопке на окно, как «Никогда не
/// объединять» в Windows 11. Без подписей (по умолчанию) — только значки, и
/// окна одной программы в одной кнопке.
static LABELS: AtomicBool = AtomicBool::new(false);

/// Подписаны ли кнопки панели задач.
#[must_use]
pub fn labels() -> bool {
    LABELS.load(Ordering::Relaxed)
}

/// Что из двух показывает панель.
#[must_use]
pub fn shows(pref: super::settings::PanelPref) -> bool {
    match pref {
        super::settings::PanelPref::Uptime => SHOW_UPTIME.load(Ordering::Relaxed),
        super::settings::PanelPref::Date => SHOW_DATE.load(Ordering::Relaxed),
        super::settings::PanelPref::Labels => LABELS.load(Ordering::Relaxed),
        super::settings::PanelPref::Animations => super::animations_enabled(),
    }
}

pub fn set_shows(pref: super::settings::PanelPref, on: bool) {
    match pref {
        super::settings::PanelPref::Uptime => SHOW_UPTIME.store(on, Ordering::Relaxed),
        super::settings::PanelPref::Date => SHOW_DATE.store(on, Ordering::Relaxed),
        super::settings::PanelPref::Labels => LABELS.store(on, Ordering::Relaxed),
        super::settings::PanelPref::Animations => super::set_animations(on),
    }
}

/// Строка часов справа: дата, время работы и часы — что включено.
///
/// Без часов реального времени остаётся только время работы, как и было: оно
/// одно отвечает на вопрос «который час» честно.
fn clock_line(status: &Status) -> String {
    let Some(clock) = status.clock.as_deref() else {
        return alloc::format!("up {}", uptime_text(status.uptime_ms));
    };
    let mut line = String::new();
    if SHOW_UPTIME.load(Ordering::Relaxed) {
        let seconds = status.uptime_ms / 1000;
        line.push_str(&alloc::format!("up {}:{:02}  ", seconds / 3600, (seconds / 60) % 60));
    }
    line.push_str(clock);
    line
}

/// Дата под часами (72b): строкой ниже, как в трее Windows. `None` — дату не
/// показывать или она неизвестна.
fn date_line(status: &Status) -> Option<String> {
    status.clock.as_ref()?;
    if !SHOW_DATE.load(Ordering::Relaxed) {
        return None;
    }
    let now = crate::time::now_local()?;
    Some(alloc::format!("{:02}.{:02}.{}", now.day, now.month, now.year))
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
    /// Значок программы окна.
    pub art: super::appicon::Art,
    /// Окна с одним ключом — одна кнопка, когда панель без подписей.
    pub group: String,
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
            for byte in entry.group.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(0x1000_0000_01b3);
            }
            hash ^= u64::from(entry.focused) | (u64::from(entry.minimized) << 1) | (u64::from(entry.art.is_image()) << 2);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        hash ^= u64::from(LABELS.load(Ordering::Relaxed)) << 7;
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
        // Время работы на панели — с точностью до минуты; дата меняется
        // вместе с часами, её свёртка уже учтена.
        if SHOW_UPTIME.load(Ordering::Relaxed) {
            clock ^= (status.uptime_ms / 60_000).wrapping_mul(0x9E37_79B9) ^ 1;
        }
        if SHOW_DATE.load(Ordering::Relaxed) {
            clock ^= 2;
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
    /// Поле поиска в доке телефона: «Пуск» сразу с полем в фокусе.
    Search,
    /// Кнопка дока, за которой программы пока нет.
    ///
    /// Телефон и камера в доке телефона стоят, а программ за ними нет — их
    /// некому написать, пока нет ни модема, ни камеры. Кнопка при этом не
    /// обманка: нажатие доходит до стола, и стол говорит вслух, чего именно
    /// нет. Промолчать было бы хуже — неотличимо от «не нажалось».
    Missing(&'static str),
    /// Закреплённая программа без окон (номер в списке закреплённых):
    /// щелчок запускает её.
    Pin(usize),
    /// Пустое место плашки: щелчок туда не должен доставаться окну под ней.
    Empty,
}
