//! Палитра и размеры рабочего стола.
//!
//! # Почему всё в одном месте
//!
//! По той же причине, по которой [`mini_ui`] существует отдельным крейтом:
//! установщик и система обязаны выглядеть как одна программа, а не как две.
//! Здесь — часть, которая относится к столу.
//!
//! # Почему цвет задан с прозрачностью, а хранится без неё
//!
//! Макет собран из полупрозрачных слоёв: окно — шестьдесят процентов над
//! обоями, панель — пятьдесят два над окном, разделительная линия — семь
//! процентов белого. Это не украшение: именно из-за прозрачности вложенные
//! поверхности отличаются друг от друга на несколько единиц яркости, а границы
//! между ними держатся на однопиксельных линиях, а не на разнице заливок.
//!
//! Поверхность в памяти прозрачности не хранит — там лежит `u32` без альфы.
//! Поэтому цвета задаются здесь так же, как в макете (цвет плюс прозрачность),
//! а при смене темы один раз **сводятся** к непрозрачным поверх подложки,
//! которая под ними окажется. Слои, лежащие прямо на обоях — панель задач, меню
//! запуска, подсветка значка, — не сводятся, а смешиваются по-настоящему при
//! отрисовке: обои считаются по формуле, и цвет под любой точкой известен.
//!
//! # Почему тем ровно две
//!
//! Потому что третья не отвечает ни на один вопрос, на который не отвечают эти
//! две, а стоит столько же, сколько первые две вместе: каждый оттенок надо
//! подобрать заново и проверить на всех поверхностях.

use core::sync::atomic::{AtomicBool, Ordering};

use mini_ui::Color;

/// Цвет вместе с прозрачностью — ровно в том виде, в каком он записан в макете.
#[derive(Clone, Copy)]
pub struct Ink {
    pub color: Color,
    /// Прозрачность в 1/255. 255 — непрозрачно.
    pub alpha: u8,
}

impl Ink {
    const fn rgba(r: u8, g: u8, b: u8, alpha: u8) -> Self {
        Self { color: Color::rgb(r, g, b), alpha }
    }

    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self::rgba(r, g, b, 255)
    }

    /// Свести к непрозрачному цвету поверх подложки.
    #[must_use]
    pub const fn over(self, under: Color) -> Color {
        under.mix(self.color, self.alpha)
    }
}

/// Набор токенов темы — имена те же, что в макете, чтобы правку можно было
/// перенести отсюда туда и обратно, не переводя словарь.
#[derive(Clone, Copy)]
pub struct Palette {
    /// Обои: верх и низ градиента и цвет точек разметки.
    pub wall_top: Color,
    pub wall_bottom: Color,
    pub wall_dot: Ink,

    /// Заливка окна и вложенных в него слоёв.
    pub win: Ink,
    pub panel: Ink,
    pub sunk: Ink,
    pub card: Ink,

    /// Разделительные линии по возрастанию заметности.
    pub line: Ink,
    pub line2: Ink,
    pub line3: Ink,

    /// Полоса заголовка: градиент сверху вниз и линия под ним.
    pub tb1: Ink,
    pub tb2: Ink,
    pub tbline: Ink,
    /// Светлая кромка по верхнему краю окна.
    pub crown: Ink,

    /// Текст по убыванию заметности.
    pub ink: Color,
    pub ink2: Color,
    pub ink3: Color,
    pub ink4: Color,
    pub ink5: Color,
    pub ink6: Color,

    /// Кнопки.
    pub btn: Ink,
    pub btnline: Ink,
    pub btnhov: Ink,
    pub ghost: Ink,

    /// Акцент: фокус, выделение, основное действие.
    pub acc: Color,
    pub acc2: Color,
    pub accline: Ink,
    pub acctint: Ink,
    pub accedge: Color,
    pub acc_ink: Color,
    pub sel1: Ink,
    pub sel2: Ink,

    /// Успех.
    pub ok: Color,
    pub ok2: Color,
    pub okbg: Ink,
    pub okline: Color,
    pub ok_ink: Color,

    /// Отказ. Он же цвет кнопки закрытия.
    pub bad: Color,
    pub bad_ink: Color,
    pub badbg: Ink,
    pub badline: Color,

    /// Предупреждение.
    pub warn: Color,
    pub warnbg: Ink,
    pub warnline: Color,

    /// Стекло: панель задач и меню запуска поверх обоев.
    pub glass: Ink,
    /// Подсветка под указателем — слабая и чуть заметнее.
    pub hover1: Ink,
    pub hover2: Ink,
    /// Тень под плавающими слоями.
    pub shadow: Ink,
}

/// Тёмная тема.
pub const DARK: Palette = Palette {
    wall_top: Color::rgb(0x16, 0x25, 0x3F),
    wall_bottom: Color::rgb(0x0A, 0x12, 0x20),
    wall_dot: Ink::rgba(0x2A, 0x3A, 0x52, 153),

    win: Ink::rgba(0x13, 0x1C, 0x2B, 153),
    panel: Ink::rgba(0x10, 0x18, 0x27, 133),
    sunk: Ink::rgba(0x08, 0x0E, 0x1A, 128),
    card: Ink::rgba(0xFF, 0xFF, 0xFF, 12),

    line: Ink::rgba(0xFF, 0xFF, 0xFF, 18),
    line2: Ink::rgba(0xFF, 0xFF, 0xFF, 26),
    line3: Ink::rgba(0xFF, 0xFF, 0xFF, 43),

    tb1: Ink::rgba(0xFF, 0xFF, 0xFF, 33),
    tb2: Ink::rgba(0xFF, 0xFF, 0xFF, 13),
    tbline: Ink::rgba(0xFF, 0xFF, 0xFF, 26),
    crown: Ink::rgba(0xFF, 0xFF, 0xFF, 85),

    ink: Color::rgb(0xEE, 0xF4, 0xFF),
    ink2: Color::rgb(0xDC, 0xE7, 0xF7),
    ink3: Color::rgb(0x96, 0xAB, 0xC6),
    ink4: Color::rgb(0x7D, 0x92, 0xAF),
    ink5: Color::rgb(0x7D, 0x92, 0xAF),
    ink6: Color::rgb(0x3C, 0x4D, 0x68),

    btn: Ink::rgba(0xFF, 0xFF, 0xFF, 23),
    btnline: Ink::rgba(0xFF, 0xFF, 0xFF, 38),
    btnhov: Ink::rgba(0xFF, 0xFF, 0xFF, 41),
    ghost: Ink::rgba(0xFF, 0xFF, 0xFF, 15),

    acc: Color::rgb(0x5A, 0xA2, 0xFF),
    acc2: Color::rgb(0x2F, 0x6E, 0xDE),
    accline: Ink::rgba(0x6F, 0xB0, 0xFF, 85),
    acctint: Ink::rgba(0x5A, 0xA2, 0xFF, 31),
    accedge: Color::rgb(0x2F, 0x4B, 0x78),
    acc_ink: Color::rgb(0x5A, 0xA2, 0xFF),
    sel1: Ink::rgba(0x5A, 0xA2, 0xFF, 61),
    sel2: Ink::rgba(0x5A, 0xA2, 0xFF, 26),

    ok: Color::rgb(0x3E, 0xCF, 0x8E),
    ok2: Color::rgb(0x1F, 0x8F, 0x5F),
    okbg: Ink::rgba(0x3E, 0xCF, 0x8E, 41),
    okline: Color::rgb(0x1F, 0x5C, 0x3A),
    ok_ink: Color::rgb(0x4F, 0xDB, 0x9C),

    bad: Color::rgb(0xFF, 0x6B, 0x5E),
    bad_ink: Color::rgb(0xFF, 0x8A, 0x80),
    badbg: Ink::rgba(0xFF, 0x6B, 0x5E, 41),
    badline: Color::rgb(0x4D, 0x2B, 0x2E),

    warn: Color::rgb(0xF5, 0xC4, 0x51),
    warnbg: Ink::rgba(0xF5, 0xC4, 0x51, 41),
    warnline: Color::rgb(0x5C, 0x4A, 0x1C),

    glass: Ink::rgba(0x10, 0x18, 0x27, 217),
    hover1: Ink::rgba(0xFF, 0xFF, 0xFF, 16),
    hover2: Ink::rgba(0xFF, 0xFF, 0xFF, 20),
    shadow: Ink::rgba(0x00, 0x05, 0x0D, 150),
};

/// Светлая тема.
///
/// Не «инвертированная тёмная»: инверсия даёт грязно-серый интерфейс, потому
/// что на светлом фоне тень читается сильнее, чем блик, и линии приходится
/// делать темнее ровно настолько, насколько в тёмной теме они были светлее.
pub const LIGHT: Palette = Palette {
    wall_top: Color::rgb(0xDC, 0xE7, 0xF7),
    wall_bottom: Color::rgb(0xEA, 0xEE, 0xF6),
    wall_dot: Ink::rgba(0xA9, 0xB6, 0xC9, 153),

    win: Ink::rgba(0xFF, 0xFF, 0xFF, 158),
    panel: Ink::rgba(0xFF, 0xFF, 0xFF, 107),
    sunk: Ink::rgba(0x10, 0x1A, 0x2B, 13),
    card: Ink::rgba(0xFF, 0xFF, 0xFF, 87),

    line: Ink::rgba(0x10, 0x1A, 0x2B, 18),
    line2: Ink::rgba(0x10, 0x1A, 0x2B, 26),
    line3: Ink::rgba(0x10, 0x1A, 0x2B, 41),

    tb1: Ink::rgba(0xFF, 0xFF, 0xFF, 199),
    tb2: Ink::rgba(0xFF, 0xFF, 0xFF, 122),
    tbline: Ink::rgba(0x10, 0x1A, 0x2B, 23),
    crown: Ink::rgb(0xFF, 0xFF, 0xFF),

    ink: Color::rgb(0x10, 0x1A, 0x2B),
    ink2: Color::rgb(0x24, 0x33, 0x49),
    ink3: Color::rgb(0x4C, 0x5B, 0x72),
    ink4: Color::rgb(0x5C, 0x6B, 0x82),
    ink5: Color::rgb(0x5C, 0x6B, 0x82),
    ink6: Color::rgb(0x9A, 0xA6, 0xB8),

    btn: Ink::rgba(0xFF, 0xFF, 0xFF, 179),
    btnline: Ink::rgba(0x10, 0x1A, 0x2B, 31),
    btnhov: Ink::rgba(0xFF, 0xFF, 0xFF, 242),
    ghost: Ink::rgba(0x10, 0x1A, 0x2B, 11),

    acc: Color::rgb(0x3B, 0x8E, 0xF0),
    acc2: Color::rgb(0x20, 0x65, 0xCF),
    accline: Ink::rgba(0x20, 0x65, 0xCF, 51),
    acctint: Ink::rgba(0x3B, 0x8E, 0xF0, 31),
    accedge: Color::rgb(0xA9, 0xCB, 0xF7),
    acc_ink: Color::rgb(0x1A, 0x5C, 0xC4),
    sel1: Ink::rgba(0x3B, 0x8E, 0xF0, 51),
    sel2: Ink::rgba(0x3B, 0x8E, 0xF0, 20),

    ok: Color::rgb(0x1C, 0x8B, 0x5A),
    ok2: Color::rgb(0x12, 0x72, 0x4A),
    okbg: Ink::rgba(0x1C, 0x8B, 0x5A, 36),
    okline: Color::rgb(0xB6, 0xE2, 0xCC),
    ok_ink: Color::rgb(0x11, 0x63, 0x40),

    bad: Color::rgb(0xE0, 0x4A, 0x3C),
    bad_ink: Color::rgb(0xC7, 0x3A, 0x2D),
    badbg: Ink::rgba(0xE0, 0x4A, 0x3C, 36),
    badline: Color::rgb(0xF5, 0xC7, 0xC2),

    warn: Color::rgb(0xA8, 0x79, 0x1A),
    warnbg: Ink::rgba(0xA8, 0x79, 0x1A, 36),
    warnline: Color::rgb(0xEC, 0xD6, 0xA6),

    glass: Ink::rgba(0xFF, 0xFF, 0xFF, 224),
    hover1: Ink::rgba(0x00, 0x00, 0x00, 8),
    hover2: Ink::rgba(0x00, 0x00, 0x00, 15),
    shadow: Ink::rgba(0x1B, 0x2A, 0x40, 90),
};

/// Какая тема сейчас.
///
/// Признак, а не индекс: тем две, и `bool` не даёт вопросу «а что, если три»
/// возникнуть раньше, чем появится третья.
static DARK_THEME: AtomicBool = AtomicBool::new(true);

/// Текущая палитра.
#[must_use]
pub fn palette() -> &'static Palette {
    if DARK_THEME.load(Ordering::Relaxed) { &DARK } else { &LIGHT }
}

/// Тёмная ли тема сейчас.
#[must_use]
pub fn is_dark() -> bool {
    DARK_THEME.load(Ordering::Relaxed)
}

/// Переключить тему. Возвращает `true`, если она действительно изменилась.
///
/// Перерисовку вызывает не эта функция: она меняет один признак, а решение
/// перерисовать всё принимает тот, кто владеет поверхностями. Иначе смена темы
/// из настроек и смена темы из меню расходились бы в том, что именно
/// обновляется.
pub fn set_dark(dark: bool) -> bool {
    DARK_THEME.swap(dark, Ordering::Relaxed) != dark
}

/// Заливка окна, сведённая к непрозрачному цвету.
///
/// Подложка — обои: окно лежит на них, и именно их оттенок просвечивает сквозь
/// шестьдесят процентов заливки. Обои берутся усреднёнными, а не в точке под
/// окном: иначе одно и то же окно меняло бы цвет при перетаскивании.
#[must_use]
pub fn window_bg() -> Color {
    let p = palette();
    p.win.over(wall_average(p))
}

/// Заливка вложенной панели поверх окна.
#[must_use]
pub fn panel_bg() -> Color {
    let p = palette();
    p.panel.over(window_bg())
}



/// Средний цвет обоев — подложка для сведения всего, что лежит на столе.
#[must_use]
pub fn wall_average(p: &Palette) -> Color {
    p.wall_top.mix(p.wall_bottom, 128)
}


// ── Размеры. Числа из макета, а не подобранные на глаз ───────────────────────

/// Скругление окна.
pub const R_WINDOW: u32 = 14;
/// Скругление панели, меню, карточки.
pub const R_CARD: u32 = 12;
/// Скругление строки списка, кнопки, поля ввода.
pub const R_ROW: u32 = 10;
/// Скругление мелкой кнопки и подложки значка.
pub const R_CHIP: u32 = 9;
/// Скругление вкладки внутри переключателя.
pub const R_TAB: u32 = 7;

/// Высота заголовка активного окна.
pub const TITLE_H: u32 = 44;
/// Сторона кнопки заголовка активного окна.
pub const TITLE_BTN: u32 = 30;
/// Зазор между кнопками заголовка.
pub const TITLE_GAP: u32 = 6;
/// Высота панели инструментов внутри окна.
pub const TOOLBAR_H: u32 = 48;
/// Высота строки состояния.
pub const STATUS_H: u32 = 30;
/// Высота строки бокового списка.
pub const SIDE_ROW_H: u32 = 34;
/// Ширина боковой колонки.
pub const SIDE_W: u32 = 240;

/// Высота панели задач.
pub const PANEL_H: u32 = 52;
/// Отступ панели задач от краёв экрана: она плавающая, а не приклеенная.
pub const PANEL_INSET: u32 = 14;
/// Высота кнопки на панели задач.
pub const PANEL_BTN_H: u32 = 34;
/// Высота строки меню запуска.
pub const MENU_ROW_H: u32 = 36;

/// Сторона плитки значка на столе.
pub const ICON_TILE: u32 = 46;
/// Ширина ячейки значка вместе с подписью.
pub const ICON_CELL_W: u32 = 110;
/// Высота ячейки значка: поле, плитка, зазор, две строки подписи, поле.
///
/// Две строки, а не одна, и у **всех** ячеек: имя файла бывает любой длины, и
/// сетка, где высота строки зависит от подписи, разъезжается на первом же
/// длинном имени.
pub const ICON_CELL_H: u32 = 100;
/// Зазор между ячейками значков.
pub const ICON_GAP: u32 = 12;
/// Отступ сетки значков от края экрана.
pub const ICON_MARGIN: u32 = 28;
/// Отступ подписи от плитки.
pub const ICON_LABEL_GAP: u32 = 9;

/// Разрядка заголовков секций, набранных прописными.
pub const CAPS_TRACKING: u32 = 1;

/// Множитель размеров для экрана такой ширины.
///
/// Разрешение перестало умножать шрифт — под каждый ряд он растеризован
/// отдельно, — но геометрию умножать по-прежнему надо: кнопка в тридцать точек
/// на экране 3840 меньше ногтя. Порог тот же, что у [`mini_ui::typeface::Tier`],
/// и это не совпадение: два разных порога развели бы шрифт и рамку вокруг него.
#[must_use]
pub const fn geometry_scale(width: u32) -> u32 {
    if width >= 2560 { 2 } else { 1 }
}
