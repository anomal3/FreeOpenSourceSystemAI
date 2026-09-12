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

use super::paint::{self, Ctx, RowState, Tone};
use super::theme::{self, Palette};
use super::window::App;

/// Надпись на кнопке меню.
const BRAND: &str = "FreeOS";

/// Заголовок столбца окон.
const APPS_TITLE: &str = "СИСТЕМА";

/// Заголовок столбца программ.
const PROGRAMS_TITLE: &str = "ПРОГРАММЫ";

/// Белый — цвет надписи на акценте.
///
/// В палитре его нет намеренно: белый на акценте одинаков в обеих темах, и
/// токен на него завёл бы вопрос «а какой белый в светлой».
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);

/// Больше двух столбцов программ не бывает.
///
/// Не потому, что не поместится, а потому, что три узких столбца имён — это уже
/// не меню, а вывод `ls`: по нему не выбирают, в нём ищут. Предел выражен
/// числом столбцов, а не шириной экрана, ровно поэтому — он про чтение, а не
/// про машину.
const MAX_PROGRAM_COLUMNS: usize = 2;

/// Контекст стола: панель и меню лежат на обоях, а не в окне.
///
/// Подложка — стекло поверх обоев: плитка значка и подложка выбранной строки
/// сводятся к ней, и возьми они подложку окна, строка меню оказалась бы светлее
/// карточки, на которой стоит.
fn desk_ctx(scale: u32) -> Ctx {
    Ctx::scaled(scale).on(glass_bg())
}

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
            round: ctx.px(theme::R_WINDOW),
            round_row: ctx.px(theme::R_ROW),
            btn_h: ctx.px(theme::PANEL_BTN_H),
            side: ctx.px(15),
            gap: ctx.px(8),
            row_h: ctx.px(theme::MENU_ROW_H),
            pitch: ctx.px(theme::MENU_ROW_H) + ctx.px(2),
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
}

impl Panel {
    /// Сколько панель отнимает у экрана снизу.
    ///
    /// Не высота плашки: плашка плавает, и поля над ней и под ней принадлежат
    /// ей так же, как она сама. Окно, доведённое до низа рабочей области,
    /// обязано остановиться над верхним полем, а не над самой плашкой — иначе
    /// оно ляжет под её тень и подсветится ею снизу.
    #[must_use]
    pub const fn height(scale: u32) -> u32 {
        (theme::PANEL_H + theme::PANEL_INSET * 2) * scale
    }

    #[must_use]
    pub fn new(screen_w: u32, screen_h: u32, scale: u32) -> Option<Self> {
        let scale = scale.max(1);
        let inset = theme::PANEL_INSET * scale;
        let plate_h = theme::PANEL_H * scale;
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
        })
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
        Some(PanelHit::Empty)
    }

    /// Перерисовать панель целиком.
    ///
    /// Целиком, а не по частям: панель — это одна плашка в полсотни точек
    /// высотой, и вычисление изменившегося куска стоило бы дороже перерисовки.
    /// Она же перекрашивает панель после смены темы — отдельного пути для этого
    /// нет и не нужно.
    pub fn redraw(&mut self, windows: &[Entry], menu_open: bool, status: &Status) {
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

        // Правый край считается раньше кнопок: место под часы и счётчики занято
        // всегда, а кнопкам достаётся то, что осталось. Наоборот было бы хуже —
        // десяток открытых окон вытеснил бы часы за край экрана.
        let mem = alloc::format!("mem {} / {} МиБ", status.free_mib, status.total_mib);
        let up = alloc::format!("up {}", uptime_text(status.uptime_ms));
        let mono = ctx.face(Role::Mono);
        let step = ctx.px(16);
        let mut status_w = mono.width(&mem) + step + mono.width(&up);
        if let Some(clock) = status.clock.as_deref() {
            status_w += step + mono.width(clock);
        }
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

        if status_w > 0 {
            let y = paint::baseline(ctx, Role::Mono, plate);
            let mut x = plate.right() - (m.plate_pad() + status_w) as i32;
            x += paint::text(ctx, &mut self.surface, Role::Mono, x, y, &mem, p.ink3) as i32;
            x += step as i32;
            // Пока настоящих часов нет, время работы и есть часы — и набрано
            // оно тогда цветом часов, а не счётчиков. Иначе на панели не
            // остаётся ни одной строки в полную силу, и правый край читается
            // как сплошная серая сноска.
            let up_ink = if status.clock.is_some() { p.ink3 } else { p.ink };
            x += paint::text(ctx, &mut self.surface, Role::Mono, x, y, &up, up_ink) as i32;
            if let Some(clock) = status.clock.as_deref() {
                x += step as i32;
                paint::text(ctx, &mut self.surface, Role::Mono, x, y, clock, p.ink);
            }
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
/// Программа ядра и программа из `/bin` — разные вещи, и меню обязано
/// возвращать разные ответы. Одно перечисление на двоих потребовало бы завести
/// у `App` вариант «какая-нибудь программа с именем», то есть строку внутри
/// перечисления, которое существует ровно затем, чтобы строк не было.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Окно, которое умеет открыть сам стол.
    App(App),
    /// Программа третьего кольца — её имя в `/bin`.
    Program(String),
}

/// Меню запуска: плавающая карточка над кнопкой «FreeOS».
///
/// # Почему два столбца, а не один список
///
/// Потому что в `/bin` два с половиной десятка программ, а у каждой строки
/// левого столбца есть пояснение под названием. Одним списком меню выходит выше
/// экрана: на 1280×720 над панелью 654 точки, а тридцать три строки по сорок —
/// тысяча триста. Второй столбец стоит ширины, которой на экране хватает, и не
/// стоит ни прокрутки, ни вложенных подменю, каждое из которых пришлось бы
/// открывать и закрывать. По той же причине столбец программ, не поместившись
/// сам, разбивается надвое: потерять половину `/bin` ради одной ровной колонки
/// — плохая мена.
pub struct Menu {
    surface: Surface,
    pub rect: Rect,
    /// Множитель геометрии стола.
    scale: u32,
    /// Выбранная строка левого столбца.
    app_row: usize,
    /// Выбранная строка правого столбца.
    program_row: usize,
    /// Выбор стоит в правом столбце.
    on_programs: bool,
    open: bool,
    damage: Rect,
    /// Имена программ из `/bin`, по алфавиту.
    programs: Vec<String>,
    /// Сколько программ в список **не** поместилось.
    ///
    /// Ноль почти всегда, и именно поэтому поле нужно. Список обрезается по
    /// высоте экрана, и обрезался он молча: программа, не влезшая в последнюю
    /// строку, просто переставала существовать для того, кто пользуется мышью.
    /// Один раз это уже случилось — из меню пропала `wc`, и нашли её не сразу.
    /// Теперь обрезка считается вместе с раскладкой, но сказать о ней всё равно
    /// есть чем.
    dropped: usize,
    /// Ширина столбца окон.
    left_w: u32,
    /// Ширина одного столбца программ.
    col_w: u32,
    /// Сколько строк помещается в столбце по высоте.
    per_col: usize,
}

impl Menu {
    #[must_use]
    pub fn new(panel_top: i32, scale: u32) -> Option<Self> {
        let scale = scale.max(1);
        let ctx = desk_ctx(scale);
        let m = Metrics::new(ctx);
        if panel_top <= 0 {
            return None;
        }
        // Над панелью — всё, что есть: карточка растёт снизу вверх от границы
        // рабочей области, а зазор между нею и плашкой — это верхнее поле
        // самой панели, уже отложенное тем, кто эту границу посчитал.
        let card_max_h = panel_top as u32;
        let per_col = (card_max_h.saturating_sub(m.head_height() + m.pad) / m.pitch.max(1)) as usize;
        if per_col == 0 {
            return None;
        }

        // Ширина столбцов — по самой длинной строке, а не заданным числом:
        // строки меняются вместе со списком программ, и подрезанное имя
        // выглядит как испорченный вывод.
        let title = ctx.face(Role::Title);
        let note = ctx.face(Role::Caption);
        let mut widest = ctx.face(Role::MonoCaps).width(APPS_TITLE);
        for app in App::LAUNCHABLE {
            widest = widest.max(title.width(app.caption())).max(note.width(app.about()));
        }
        let left_w = m.row_width(widest);

        let all = list_programs();
        let mut widest_program = ctx.face(Role::MonoCaps).width(PROGRAMS_TITLE);
        for name in &all {
            widest_program = widest_program.max(title.width(name));
        }
        // Имя длиннее этого столбец не растягивает: имя файла ограничено сотнями
        // знаков, и одна такая строка в `/bin` вытолкнула бы меню за край
        // экрана. Ей отрежут хвост многоточием — так же, как подписи кнопки.
        let col_w = m.row_width(widest_program.min(ctx.px(180)));

        let limit = per_col.saturating_mul(MAX_PROGRAM_COLUMNS);
        let dropped = all.len().saturating_sub(limit);
        let mut programs = all;
        programs.truncate(limit);
        let cols = programs.len().div_ceil(per_col);
        // Столбцы уравниваются по длине: двадцать пять имён — это не
        // «четырнадцать и одиннадцать», а «тринадцать и двенадцать». Разница не
        // в красоте, а в высоте карточки: она считается по длинному столбцу.
        let per_col = if cols > 0 { programs.len().div_ceil(cols) } else { per_col };
        let cols = cols as u32;

        let body_w = if cols == 0 {
            left_w
        } else {
            left_w + (m.gap + col_w) * cols
        };
        let card_w = m.pad * 2 + body_w.max(m.header_width());

        // Высота — по самому длинному столбцу: карточка кончается там, где
        // кончилось содержимое, а не там, где кончился экран.
        let left_h = App::LAUNCHABLE.len() as u32 * m.pitch + m.rule_height();
        let right_h = programs.len().min(per_col) as u32 * m.pitch;
        let body_h = left_h.max(right_h).saturating_sub(m.pitch - m.row_h);
        let card_h = (m.head_height() + body_h + m.pad).min(card_max_h);

        let surface = Surface::new(card_w, card_h, glass_bg())?;
        // Левый край карточки — ровно левый край плашки панели: одна вертикаль
        // на весь стол читается как порядок, две почти совпадающие — как ошибка
        // отрисовки.
        let rect = Rect::new(
            m.inset as i32,
            panel_top - surface.height() as i32,
            surface.width(),
            surface.height(),
        );
        Some(Self {
            surface,
            rect,
            scale,
            app_row: 0,
            program_row: 0,
            on_programs: false,
            open: false,
            damage: Rect::EMPTY,
            programs,
            dropped,
            left_w,
            col_w,
            per_col,
        })
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Сколько программ меню показывает.
    #[must_use]
    pub fn program_count(&self) -> usize {
        self.programs.len()
    }

    /// Сколько программ не поместилось в список.
    #[must_use]
    pub fn dropped_programs(&self) -> usize {
        self.dropped
    }

    /// Открыть или закрыть меню. Возвращает новое состояние.
    pub fn toggle(&mut self) -> bool {
        self.open = !self.open;
        if self.open {
            self.app_row = 0;
            self.program_row = 0;
            self.on_programs = false;
            self.redraw();
        }
        self.open
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Перерисовать меню под текущую тему.
    ///
    /// Отдельно от [`Menu::toggle`], потому что смена темы не меняет ни выбора,
    /// ни того, открыто ли меню: она меняет только цвета. Перерисовывается и
    /// закрытое меню — тема могла смениться, пока оно закрыто, и открыться оно
    /// обязано уже в новых цветах, а не в старых до первого движения мышью.
    pub fn restyle(&mut self) {
        self.redraw();
    }

    /// Что лежит под точкой (координаты экрана).
    #[must_use]
    pub fn choice_at(&self, x: i32, y: i32) -> Option<Choice> {
        if !self.rect.contains(x, y) {
            return None;
        }
        let (x, y) = (x - self.rect.x, y - self.rect.y);
        // Та же раскладка, что и при рисовании, — не пересчитанная заново, а
        // ровно та же функция: щелчок обязан попадать туда, куда нарисовано.
        for row in self.layout().rows {
            if row.rect.contains(x, y) {
                return self.choice_of(&row);
            }
        }
        None
    }

    /// Поставить выделение туда, где стоит указатель.
    pub fn select_at(&mut self, x: i32, y: i32) {
        let Some(choice) = self.choice_at(x, y) else {
            return;
        };
        let (on_programs, row) = match &choice {
            Choice::App(app) => (
                false,
                App::LAUNCHABLE.iter().position(|item| item == app).unwrap_or(0),
            ),
            Choice::Program(name) => (
                true,
                self.programs.iter().position(|item| item == name).unwrap_or(0),
            ),
        };
        if self.on_programs == on_programs
            && (if on_programs { self.program_row } else { self.app_row }) == row
        {
            return;
        }
        self.on_programs = on_programs;
        if on_programs {
            self.program_row = row;
        } else {
            self.app_row = row;
        }
        self.redraw();
    }

    /// Сдвинуть выбор внутри столбца, по кругу.
    pub fn move_selection(&mut self, forward: bool) {
        let count = if self.on_programs {
            self.programs.len()
        } else {
            App::LAUNCHABLE.len()
        };
        if count == 0 {
            return;
        }
        let row = if self.on_programs {
            &mut self.program_row
        } else {
            &mut self.app_row
        };
        *row = if forward {
            (*row + 1) % count
        } else {
            (*row + count - 1) % count
        };
        self.redraw();
    }

    /// Перейти в другой столбец. Возвращает `true`, если переход состоялся.
    ///
    /// Столбцы переключаются стрелками влево-вправо, а не общим обходом сверху
    /// вниз: список программ длиннее списка окон в три раза, и обход по кругу
    /// означал бы двадцать нажатий, чтобы вернуться к «Терминалу».
    pub fn switch_column(&mut self, to_programs: bool) -> bool {
        if to_programs && self.programs.is_empty() {
            return false;
        }
        if self.on_programs == to_programs {
            return false;
        }
        self.on_programs = to_programs;
        self.redraw();
        true
    }

    /// Что выбрано сейчас.
    #[must_use]
    pub fn selection(&self) -> Option<Choice> {
        if self.on_programs {
            self.programs.get(self.program_row).cloned().map(Choice::Program)
        } else {
            App::LAUNCHABLE.get(self.app_row).copied().map(Choice::App)
        }
    }

    #[must_use]
    pub const fn surface(&self) -> &Surface {
        &self.surface
    }

    pub fn take_damage(&mut self) -> Rect {
        core::mem::replace(&mut self.damage, Rect::EMPTY)
    }

    /// Куда ведёт строка.
    fn choice_of(&self, row: &MenuRow) -> Option<Choice> {
        if row.program {
            self.programs.get(row.index).cloned().map(Choice::Program)
        } else {
            App::LAUNCHABLE.get(row.index).copied().map(Choice::App)
        }
    }

    /// Раскладка карточки: где шапка, где заголовки столбцов, где строки.
    fn layout(&self) -> MenuLayout {
        let ctx = desk_ctx(self.scale);
        let m = Metrics::new(ctx);
        // Карточка — это вся поверхность: тень под ней и срез углов делает
        // композитор. Так раскладка сходится с той, под которую считали место в
        // [`Menu::new`], без единого повторённого числа.
        let card = self.surface.bounds();

        let badge = Rect::new(card.x + m.pad as i32, card.y + m.pad as i32, m.head(), m.head());
        let name_x = badge.right() + m.gap as i32;
        let name_line = i32::from(ctx.face(Role::Title).line);
        let lines = name_line + i32::from(ctx.face(Role::MonoSmall).line);
        let name_y = badge.y + (badge.h as i32 - lines) / 2;

        let caps_y = card.y + (m.pad + m.head() + m.gap) as i32;
        let top = caps_y + m.caps() as i32;
        let left_x = card.x + m.pad as i32;

        let mut rows = Vec::new();
        let mut rules = Vec::new();
        let mut y = top;
        let mut ruled = false;
        for (index, app) in App::LAUNCHABLE.iter().enumerate() {
            // Питание отделяется чертой: «выключить» рядом с «открыть терминал»
            // — это соседство, в котором однажды промахиваются. Черта одна на
            // обе строки питания, а не по черте на каждую.
            if !ruled && app.confirms_power().is_some() && index > 0 {
                ruled = true;
                let rule = ctx.px(6);
                rules.push(Rect::new(
                    left_x + m.row_pad as i32,
                    y + rule as i32,
                    self.left_w.saturating_sub(m.row_pad * 2),
                    1,
                ));
                y += m.rule_height() as i32;
            }
            rows.push(MenuRow {
                rect: Rect::new(left_x, y, self.left_w, m.row_h),
                index,
                program: false,
                icon: app.icon(),
                tone: app.tone(),
                danger: app.confirms_power().is_some(),
            });
            y += m.pitch as i32;
        }

        let programs_x = left_x + (self.left_w + m.gap) as i32;
        let per_col = self.per_col.max(1);
        for index in 0..self.programs.len() {
            rows.push(MenuRow {
                rect: Rect::new(
                    programs_x + ((index / per_col) as u32 * (self.col_w + m.gap)) as i32,
                    top + ((index % per_col) as u32 * m.pitch) as i32,
                    self.col_w,
                    m.row_h,
                ),
                index,
                program: true,
                // Программа о себе не рассказывает ничего, пока её не
                // запустишь, и своего значка у неё взяться неоткуда. Но она
                // всё-таки программа, а не файл: значок оболочки говорит
                // «это запустится», и это единственное, что о ней известно
                // наверняка.
                icon: Icon::Terminal,
                tone: Tone::Accent,
                danger: false,
            });
        }

        MenuLayout {
            card,
            badge,
            name: (name_x, name_y),
            note: (name_x, name_y + name_line),
            apps_caps: (left_x + m.row_pad as i32, caps_y),
            programs_caps: (programs_x + m.row_pad as i32, caps_y),
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

        // Шапка. Имени пользователя система не знает — учётных записей у неё
        // нет, — поэтому в строке имени стоит то же, что стояло и раньше: имя
        // самой системы и её версия. Раскладка при этом уже та, которая нужна
        // имени, когда оно появится.
        //
        // Знак в плитке — тот же, что на кнопке «FreeOS» под ней: пока это имя
        // системы, а не человека, лицо в кружке обещало бы учётную запись,
        // которой нет.
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
        paint::text(
            ctx,
            &mut self.surface,
            Role::Title,
            layout.name.0,
            layout.name.1,
            BRAND,
            p.ink,
        );
        paint::text(
            ctx,
            &mut self.surface,
            Role::MonoSmall,
            layout.note.0,
            layout.note.1,
            crate::VERSION,
            p.ink4,
        );

        paint::caps(ctx, &mut self.surface, layout.apps_caps.0, layout.apps_caps.1, APPS_TITLE);
        if !self.programs.is_empty() {
            paint::caps(
                ctx,
                &mut self.surface,
                layout.programs_caps.0,
                layout.programs_caps.1,
                PROGRAMS_TITLE,
            );
        }

        for rule in &layout.rules {
            paint::separator(ctx, &mut self.surface, rule.x, rule.y, rule.w);
        }

        for row in &layout.rows {
            let selected = row.program == self.on_programs
                && row.index == if row.program { self.program_row } else { self.app_row };
            let (label, note) = if row.program {
                (self.programs.get(row.index).map_or("", String::as_str), "")
            } else {
                match App::LAUNCHABLE.get(row.index) {
                    Some(app) => (app.caption(), app.about()),
                    None => continue,
                }
            };
            draw_row(m, &mut self.surface, row, label, note, selected);
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
    /// Номер в своём столбце — по нему же считается выделение.
    index: usize,
    /// Строка правого столбца.
    program: bool,
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
    apps_caps: (i32, i32),
    programs_caps: (i32, i32),
    rows: Vec<MenuRow>,
    rules: Vec<Rect>,
}

/// Имена программ из `/bin`, по алфавиту.
///
/// # Чего здесь нет и почему
///
/// `init` в списке нет: это надзиратель за службами, он уже работает, и вторая
/// его копия, запущенная человеком из меню, взялась бы поднимать те же службы
/// заново. Всё остальное в списке есть, включая то, что падает нарочно
/// (`crash`, `svcbad`): оболочка запускает их по имени и сейчас, и прятать в
/// меню то, что можно набрать руками, значило бы делать вид, что этого нет.
///
/// Обрезкой список не занимается: сколько имён поместится, знает раскладка
/// меню, и решать это в двух местах — верный способ однажды показать одно, а
/// открыть другое.
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

/// Кнопки панели вместе с их местом.
type Buttons = Vec<(App, Rect)>;

/// Во что попал указатель на панели.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelHit {
    /// Кнопка меню.
    Menu,
    /// Кнопка окна.
    Window(App),
    /// Пустое место плашки: щелчок туда не должен доставаться окну под ней.
    Empty,
}
