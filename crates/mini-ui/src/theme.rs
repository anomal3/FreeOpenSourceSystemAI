//! Палитра и размеры рабочего стола.
//!
//! # Почему всё в одном месте
//!
//! По той же причине, по которой этот крейт существует отдельно: установщик,
//! система и программы обязаны выглядеть как одна программа, а не как три.
//! Здесь — часть, которая относится к столу.
//!
//! # Почему модуль живёт здесь, а не в ядре
//!
//! Он и жил в ядре — до фазы 47c, пока окна были его частью. Когда окно стало
//! делом программы, палитра понадобилась по обе стороны границы привилегий:
//! файловый менеджер рисует список теми же цветами, что и стол, иначе он
//! выглядит чужим. Зависимостей от ядра у палитры не было ни одной — это
//! константы и один признак, — так что переезд оказался переносом файла.
//!
//! **Признак темы у каждой стороны свой.** Адресные пространства разные, и
//! [`set_dark`] в ядре не виден программе; та узнаёт тему из `SYS_SYSINFO`
//! (бит `SYSINFO_DARK`) и выставляет признак себе сама — ровно так же, как
//! поступает с форматом точки. Ошибиться этим легко, и выглядит ошибка
//! убедительно: окно нарисовано правильно, только в другой теме.
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

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

use crate::Color;

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

    // С фазы 72b — цвета `tokens.json` макета Романа: поверхности сплошные
    // (окно, заголовок, поле ввода, кнопка), прозрачны только линии, подсветка
    // и выделение.
    win: Ink::rgb(0x1A, 0x1F, 0x2B),
    panel: Ink::rgb(0x17, 0x1C, 0x27),
    sunk: Ink::rgb(0x11, 0x15, 0x1D),
    card: Ink::rgba(0xFF, 0xFF, 0xFF, 10),

    line: Ink::rgba(0xFF, 0xFF, 0xFF, 15),
    line2: Ink::rgba(0xFF, 0xFF, 0xFF, 20),
    line3: Ink::rgba(0xFF, 0xFF, 0xFF, 31),

    tb1: Ink::rgb(0x21, 0x28, 0x36),
    tb2: Ink::rgb(0x1A, 0x1F, 0x2B),
    tbline: Ink::rgba(0xFF, 0xFF, 0xFF, 20),
    crown: Ink::rgba(0xFF, 0xFF, 0xFF, 0),

    ink: Color::rgb(0xED, 0xF0, 0xF6),
    ink2: Color::rgb(0xDD, 0xE2, 0xEA),
    ink3: Color::rgb(0xB4, 0xBC, 0xCB),
    ink4: Color::rgb(0x8A, 0x93, 0xA5),
    ink5: Color::rgb(0x8A, 0x93, 0xA5),
    ink6: Color::rgb(0x4A, 0x52, 0x63),

    btn: Ink::rgb(0x2A, 0x31, 0x40),
    btnline: Ink::rgba(0xFF, 0xFF, 0xFF, 31),
    btnhov: Ink::rgb(0x32, 0x3A, 0x4B),
    ghost: Ink::rgba(0xFF, 0xFF, 0xFF, 12),

    acc: Color::rgb(0x3B, 0x82, 0xF6),
    acc2: Color::rgb(0x25, 0x63, 0xEB),
    accline: Ink::rgba(0x60, 0xA5, 0xFA, 110),
    acctint: Ink::rgba(0x3B, 0x82, 0xF6, 40),
    accedge: Color::rgb(0x1E, 0x3A, 0x8A),
    acc_ink: Color::rgb(0x60, 0xA5, 0xFA),
    sel1: Ink::rgba(0x3B, 0x82, 0xF6, 61),
    sel2: Ink::rgba(0x3B, 0x82, 0xF6, 32),

    ok: Color::rgb(0x4A, 0xDE, 0x80),
    ok2: Color::rgb(0x22, 0xA0, 0x55),
    okbg: Ink::rgba(0x4A, 0xDE, 0x80, 36),
    okline: Color::rgb(0x1F, 0x5C, 0x3A),
    ok_ink: Color::rgb(0x4A, 0xDE, 0x80),

    bad: Color::rgb(0xC4, 0x2B, 0x1C),
    bad_ink: Color::rgb(0xF8, 0x71, 0x71),
    badbg: Ink::rgba(0xF8, 0x71, 0x71, 41),
    badline: Color::rgb(0x5A, 0x2A, 0x2A),

    warn: Color::rgb(0xFB, 0xBF, 0x24),
    warnbg: Ink::rgba(0xFB, 0xBF, 0x24, 36),
    warnline: Color::rgb(0x5C, 0x4A, 0x1C),

    glass: Ink::rgba(0x16, 0x1B, 0x26, 250),
    hover1: Ink::rgba(0xFF, 0xFF, 0xFF, 18),
    hover2: Ink::rgba(0xFF, 0xFF, 0xFF, 27),
    shadow: Ink::rgba(0x00, 0x00, 0x00, 115),
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

    // Тема «лён» макета (72b): тёплая бумага и сланец.
    win: Ink::rgb(0xFB, 0xFA, 0xF6),
    panel: Ink::rgb(0xF6, 0xF3, 0xEC),
    sunk: Ink::rgb(0xFF, 0xFF, 0xFF),
    card: Ink::rgba(0x1F, 0x29, 0x37, 8),

    line: Ink::rgba(0x1F, 0x29, 0x37, 18),
    line2: Ink::rgba(0x1F, 0x29, 0x37, 23),
    line3: Ink::rgba(0x1F, 0x29, 0x37, 41),

    tb1: Ink::rgb(0xEE, 0xEA, 0xE1),
    tb2: Ink::rgb(0xF6, 0xF3, 0xEC),
    tbline: Ink::rgba(0x1F, 0x29, 0x37, 23),
    crown: Ink::rgba(0xFF, 0xFF, 0xFF, 0),

    ink: Color::rgb(0x1F, 0x29, 0x37),
    ink2: Color::rgb(0x2B, 0x35, 0x45),
    ink3: Color::rgb(0x47, 0x54, 0x67),
    ink4: Color::rgb(0x66, 0x70, 0x85),
    ink5: Color::rgb(0x66, 0x70, 0x85),
    ink6: Color::rgb(0xA5, 0xAC, 0xB8),

    btn: Ink::rgb(0xEC, 0xE8, 0xDE),
    btnline: Ink::rgba(0x1F, 0x29, 0x37, 41),
    btnhov: Ink::rgb(0xE4, 0xE0, 0xD4),
    ghost: Ink::rgba(0x1F, 0x29, 0x37, 10),

    acc: Color::rgb(0x3B, 0x82, 0xF6),
    acc2: Color::rgb(0x1D, 0x4E, 0xD8),
    accline: Ink::rgba(0x1D, 0x4E, 0xD8, 70),
    acctint: Ink::rgba(0x3B, 0x82, 0xF6, 31),
    accedge: Color::rgb(0xBF, 0xDB, 0xFE),
    acc_ink: Color::rgb(0x1D, 0x4E, 0xD8),
    sel1: Ink::rgba(0x3B, 0x82, 0xF6, 41),
    sel2: Ink::rgba(0x3B, 0x82, 0xF6, 20),

    ok: Color::rgb(0x15, 0x80, 0x3D),
    ok2: Color::rgb(0x12, 0x6A, 0x33),
    okbg: Ink::rgba(0x15, 0x80, 0x3D, 31),
    okline: Color::rgb(0xB6, 0xE2, 0xCC),
    ok_ink: Color::rgb(0x15, 0x80, 0x3D),

    bad: Color::rgb(0xC4, 0x2B, 0x1C),
    bad_ink: Color::rgb(0xB9, 0x1C, 0x1C),
    badbg: Ink::rgba(0xB9, 0x1C, 0x1C, 31),
    badline: Color::rgb(0xF5, 0xC7, 0xC2),

    warn: Color::rgb(0xB4, 0x53, 0x09),
    warnbg: Ink::rgba(0xB4, 0x53, 0x09, 31),
    warnline: Color::rgb(0xEC, 0xD6, 0xA6),

    glass: Ink::rgba(0xF1, 0xEE, 0xE6, 250),
    hover1: Ink::rgba(0x1F, 0x29, 0x37, 10),
    hover2: Ink::rgba(0x1F, 0x29, 0x37, 18),
    shadow: Ink::rgba(0x1F, 0x29, 0x37, 60),
};

/// Какая тема сейчас.
///
/// Признак, а не индекс: тем две, и `bool` не даёт вопросу «а что, если три»
/// возникнуть раньше, чем появится третья.
static DARK_THEME: AtomicBool = AtomicBool::new(true);

// ── Персонализация (фаза С6) ─────────────────────────────────────────────────
//
// Акцент, обои и высота заголовка окна — настройки времени выполнения, как и
// тема. Палитра при этом остаётся `&'static`: все сочетания темы, акцента и
// обоев посчитаны на этапе компиляции в одну таблицу, и [`palette`] лишь
// выбирает из неё. Считать палитру на лету значило бы держать её под замком —
// а замок в крейте рисования брать некому и незачем.

/// Акцентный цвет: фокус, выделение и главное действие.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accent {
    Blue,
    Violet,
    Green,
    Orange,
    Rose,
}

impl Accent {
    pub const ALL: [Accent; 5] =
        [Accent::Blue, Accent::Violet, Accent::Green, Accent::Orange, Accent::Rose];

    /// Слово в `desktop.cfg`.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Accent::Blue => "blue",
            Accent::Violet => "violet",
            Accent::Green => "green",
            Accent::Orange => "orange",
            Accent::Rose => "rose",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Accent::Blue => "Синий",
            Accent::Violet => "Фиолетовый",
            Accent::Green => "Зелёный",
            Accent::Orange => "Оранжевый",
            Accent::Rose => "Розовый",
        }
    }

    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|accent| accent.tag() == tag)
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn from_index(index: usize) -> Self {
        match index {
            1 => Accent::Violet,
            2 => Accent::Green,
            3 => Accent::Orange,
            4 => Accent::Rose,
            _ => Accent::Blue,
        }
    }
}

/// Обои: два цвета градиента и цвет точек разметки, свои для каждой темы.
///
/// [`Wallpaper::Fossai`] — картинка из макета Романа (фаза 72a), обои по
/// умолчанию; её цвета здесь — средние по картинке, для того, что лежит на
/// обоях полупрозрачным (меню, док).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wallpaper {
    Fossai,
    Night,
    Ocean,
    Sunset,
    Graphite,
}

impl Wallpaper {
    pub const ALL: [Wallpaper; 5] =
        [Wallpaper::Fossai, Wallpaper::Night, Wallpaper::Ocean, Wallpaper::Sunset, Wallpaper::Graphite];

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Wallpaper::Fossai => "fossai",
            Wallpaper::Night => "night",
            Wallpaper::Ocean => "ocean",
            Wallpaper::Sunset => "sunset",
            Wallpaper::Graphite => "graphite",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Wallpaper::Fossai => "FossAi",
            Wallpaper::Night => "Ночь",
            Wallpaper::Ocean => "Океан",
            Wallpaper::Sunset => "Закат",
            Wallpaper::Graphite => "Графит",
        }
    }

    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|wall| wall.tag() == tag)
    }

    const fn index(self) -> usize {
        self as usize
    }

    const fn from_index(index: usize) -> Self {
        match index {
            1 => Wallpaper::Night,
            2 => Wallpaper::Ocean,
            3 => Wallpaper::Sunset,
            4 => Wallpaper::Graphite,
            _ => Wallpaper::Fossai,
        }
    }
}

/// Цвета акцента: для тёмной и для светлой темы отдельно — на тёмном фоне
/// акцент светлее, на светлом темнее, и одна пара на обе темы не читалась бы.
struct AccentColors {
    /// Основной, тёмный край градиента, кромка выделения, цвет текста.
    dark: (Color, Color, Color, Color),
    light: (Color, Color, Color, Color),
}

const ACCENTS: [AccentColors; 5] = [
    AccentColors {
        // Синий — акцент макета (72b): `accent.base` #3B82F6, текст #60A5FA.
        dark: (Color::rgb(0x3B, 0x82, 0xF6), Color::rgb(0x25, 0x63, 0xEB), Color::rgb(0x1E, 0x3A, 0x8A), Color::rgb(0x60, 0xA5, 0xFA)),
        light: (Color::rgb(0x3B, 0x82, 0xF6), Color::rgb(0x1D, 0x4E, 0xD8), Color::rgb(0xBF, 0xDB, 0xFE), Color::rgb(0x1D, 0x4E, 0xD8)),
    },
    AccentColors {
        dark: (Color::rgb(0xA7, 0x8B, 0xFA), Color::rgb(0x7C, 0x3A, 0xED), Color::rgb(0x4C, 0x3A, 0x80), Color::rgb(0xB8, 0xA3, 0xFF)),
        light: (Color::rgb(0x8B, 0x5C, 0xF6), Color::rgb(0x6D, 0x28, 0xD9), Color::rgb(0xD6, 0xC7, 0xFB), Color::rgb(0x5B, 0x21, 0xB6)),
    },
    AccentColors {
        dark: (Color::rgb(0x34, 0xD3, 0x99), Color::rgb(0x05, 0x96, 0x69), Color::rgb(0x24, 0x5C, 0x48), Color::rgb(0x5E, 0xE0, 0xB0)),
        light: (Color::rgb(0x10, 0xB9, 0x81), Color::rgb(0x04, 0x78, 0x57), Color::rgb(0xB4, 0xEB, 0xD4), Color::rgb(0x04, 0x78, 0x57)),
    },
    AccentColors {
        dark: (Color::rgb(0xFB, 0x92, 0x3C), Color::rgb(0xEA, 0x58, 0x0C), Color::rgb(0x6B, 0x3A, 0x1E), Color::rgb(0xFF, 0xA9, 0x5E)),
        light: (Color::rgb(0xF9, 0x73, 0x16), Color::rgb(0xC2, 0x41, 0x0C), Color::rgb(0xFB, 0xD5, 0xB5), Color::rgb(0xB4, 0x53, 0x09)),
    },
    AccentColors {
        dark: (Color::rgb(0xFB, 0x71, 0x85), Color::rgb(0xE1, 0x1D, 0x48), Color::rgb(0x6B, 0x2D, 0x3A), Color::rgb(0xFF, 0x8F, 0xA0)),
        light: (Color::rgb(0xF4, 0x3F, 0x5E), Color::rgb(0xBE, 0x12, 0x3C), Color::rgb(0xFB, 0xC4, 0xCF), Color::rgb(0xBE, 0x12, 0x3C)),
    },
];

/// Цвета обоев: верх, низ, точки — для тёмной и светлой темы.
struct WallColors {
    dark: (Color, Color, Ink),
    light: (Color, Color, Ink),
}

const WALLS: [WallColors; 5] = [
    // Обои FossAi: середина картинки и низ у волн. Светлой темы у картинки
    // нет — она одна на обе.
    WallColors {
        dark: (Color::rgb(0x0C, 0x2A, 0x7E), Color::rgb(0x06, 0x14, 0x46), Ink::rgba(0x2A, 0x4A, 0x9A, 0)),
        light: (Color::rgb(0x0C, 0x2A, 0x7E), Color::rgb(0x06, 0x14, 0x46), Ink::rgba(0x2A, 0x4A, 0x9A, 0)),
    },
    WallColors {
        dark: (Color::rgb(0x16, 0x25, 0x3F), Color::rgb(0x0A, 0x12, 0x20), Ink::rgba(0x2A, 0x3A, 0x52, 153)),
        light: (Color::rgb(0xDC, 0xE7, 0xF7), Color::rgb(0xEA, 0xEE, 0xF6), Ink::rgba(0xA9, 0xB6, 0xC9, 153)),
    },
    WallColors {
        dark: (Color::rgb(0x0F, 0x3A, 0x3A), Color::rgb(0x08, 0x1A, 0x22), Ink::rgba(0x2A, 0x5A, 0x5A, 153)),
        light: (Color::rgb(0xD6, 0xF0, 0xEE), Color::rgb(0xE8, 0xF5, 0xF3), Ink::rgba(0x9C, 0xC9, 0xC4, 153)),
    },
    WallColors {
        dark: (Color::rgb(0x3A, 0x1E, 0x3F), Color::rgb(0x1A, 0x0E, 0x22), Ink::rgba(0x5A, 0x3A, 0x5E, 153)),
        light: (Color::rgb(0xF7, 0xE3, 0xDC), Color::rgb(0xF3, 0xE9, 0xF6), Ink::rgba(0xC9, 0xAF, 0xB6, 153)),
    },
    WallColors {
        dark: (Color::rgb(0x23, 0x27, 0x2E), Color::rgb(0x12, 0x14, 0x18), Ink::rgba(0x3A, 0x3F, 0x48, 153)),
        light: (Color::rgb(0xE6, 0xE8, 0xEC), Color::rgb(0xF0, 0xF1, 0xF4), Ink::rgba(0xB6, 0xBA, 0xC2, 153)),
    },
];

/// Палитра темы с таким акцентом и такими обоями.
const fn styled(dark: bool, accent: usize, wall: usize) -> Palette {
    let mut p = if dark { DARK } else { LIGHT };
    let (acc, acc2, edge, ink) = if dark { ACCENTS[accent].dark } else { ACCENTS[accent].light };
    p.acc = acc;
    p.acc2 = acc2;
    p.accedge = edge;
    p.acc_ink = ink;
    // Прозрачности — те же, что у синего в каждой теме: они подобраны под
    // фон, а не под оттенок.
    p.accline = if dark { Ink::rgba(acc.r, acc.g, acc.b, 85) } else { Ink::rgba(acc2.r, acc2.g, acc2.b, 51) };
    p.acctint = Ink::rgba(acc.r, acc.g, acc.b, 31);
    p.sel1 = Ink::rgba(acc.r, acc.g, acc.b, if dark { 61 } else { 51 });
    p.sel2 = Ink::rgba(acc.r, acc.g, acc.b, if dark { 26 } else { 20 });
    let (top, bottom, dot) = if dark { WALLS[wall].dark } else { WALLS[wall].light };
    p.wall_top = top;
    p.wall_bottom = bottom;
    p.wall_dot = dot;
    p
}

const fn build_palettes() -> [[[Palette; 5]; 5]; 2] {
    let mut out = [[[DARK; 5]; 5]; 2];
    let mut theme = 0;
    while theme < 2 {
        let mut accent = 0;
        while accent < 5 {
            let mut wall = 0;
            while wall < 5 {
                out[theme][accent][wall] = styled(theme == 0, accent, wall);
                wall += 1;
            }
            accent += 1;
        }
        theme += 1;
    }
    out
}

/// Все сочетания: `[тёмная|светлая][акцент][обои]`.
static PALETTES: [[[Palette; 5]; 5]; 2] = build_palettes();

static ACCENT: AtomicU8 = AtomicU8::new(0);
static WALLPAPER: AtomicU8 = AtomicU8::new(0);
/// Высота заголовка окна в точках при масштабе 1. Из [`TITLE_HEIGHTS`].
static TITLE_BAR: AtomicU32 = AtomicU32::new(TITLE_H);

/// Допустимые высоты заголовка: компактный, обычный (макет), крупный.
pub const TITLE_HEIGHTS: [u32; 3] = [28, 32, 40];

#[must_use]
pub fn accent() -> Accent {
    Accent::from_index(ACCENT.load(Ordering::Relaxed) as usize)
}

/// Основной цвет такого акцента в нынешней теме — для образцов в «Параметрах».
#[must_use]
pub fn accent_color(accent: Accent) -> Color {
    let colors = &ACCENTS[accent.index()];
    if is_dark() { colors.dark.0 } else { colors.light.0 }
}

/// Верх и низ таких обоев в нынешней теме — для образцов в «Параметрах».
#[must_use]
pub fn wallpaper_colors(wall: Wallpaper) -> (Color, Color) {
    let colors = &WALLS[wall.index()];
    let (top, bottom, _) = if is_dark() { colors.dark } else { colors.light };
    (top, bottom)
}

/// `true` — акцент действительно сменился.
pub fn set_accent(accent: Accent) -> bool {
    ACCENT.swap(accent.index() as u8, Ordering::Relaxed) != accent.index() as u8
}

#[must_use]
pub fn wallpaper() -> Wallpaper {
    Wallpaper::from_index(WALLPAPER.load(Ordering::Relaxed) as usize)
}

pub fn set_wallpaper(wall: Wallpaper) -> bool {
    WALLPAPER.swap(wall.index() as u8, Ordering::Relaxed) != wall.index() as u8
}

/// Высота заголовка окна при масштабе 1.
#[must_use]
pub fn title_h() -> u32 {
    TITLE_BAR.load(Ordering::Relaxed)
}

/// Задать высоту заголовка. Принимается только из [`TITLE_HEIGHTS`]: заголовок
/// в три точки — это не настройка, а испорченный файл. `true` — изменилась.
pub fn set_title_h(height: u32) -> bool {
    if !TITLE_HEIGHTS.contains(&height) {
        return false;
    }
    TITLE_BAR.swap(height, Ordering::Relaxed) != height
}

/// Текущая палитра: тема, акцент и обои.
#[must_use]
pub fn palette() -> &'static Palette {
    let theme = if DARK_THEME.load(Ordering::Relaxed) { 0 } else { 1 };
    let accent = (ACCENT.load(Ordering::Relaxed) as usize).min(4);
    let wall = (WALLPAPER.load(Ordering::Relaxed) as usize).min(4);
    &PALETTES[theme][accent][wall]
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
pub const R_WINDOW: u32 = 8;
/// Скругление панели, меню, карточки.
pub const R_CARD: u32 = 8;
/// Скругление строки списка, кнопки, поля ввода.
pub const R_ROW: u32 = 4;
/// Скругление мелкой кнопки и подложки значка.
pub const R_CHIP: u32 = 4;
/// Скругление вкладки внутри переключателя.
pub const R_TAB: u32 = 4;

/// Высота заголовка активного окна.
pub const TITLE_H: u32 = 32;
/// Ширина кнопки заголовка (72b, `windowControlWidth`): кнопки плоские и во
/// всю высоту полосы, как в Windows.
pub const TITLE_BTN: u32 = 46;
/// Зазор между кнопками заголовка.
pub const TITLE_GAP: u32 = 0;
/// Высота панели инструментов внутри окна.
pub const TOOLBAR_H: u32 = 48;
/// Высота строки состояния.
pub const STATUS_H: u32 = 30;
/// Высота строки бокового списка.
pub const SIDE_ROW_H: u32 = 34;
/// Ширина боковой колонки.
pub const SIDE_W: u32 = 240;

/// Высота панели задач.
pub const PANEL_H: u32 = 48;
/// Отступ панели задач от краёв экрана. С 72b — ноль: панель во всю ширину
/// и приклеена к низу, как в макете и в Windows.
pub const PANEL_INSET: u32 = 0;
/// Высота кнопки на панели задач.
pub const PANEL_BTN_H: u32 = 40;
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

// ── Телефон: те же числа, но из мобильного макета ────────────────────────────

/// Высота строки состояния наверху экрана телефона: часы, связь, питание.
pub const M_STATUS_H: u32 = 46;
/// Поле от краёв экрана.
///
/// У телефона край — не край: углы скруглены, сверху вырез камеры. Всё, что
/// ложится под них, человек не видит и, что хуже, не может нажать.
pub const M_INSET: u32 = 14;
/// Сторона плитки приложения.
pub const M_TILE: u32 = 56;
/// Скругление плитки приложения.
pub const M_R_TILE: u32 = 18;
/// Скругление карточки.
pub const M_R_CARD: u32 = 24;
/// Скругление дока.
pub const M_R_DOCK: u32 = 26;
/// Высота дока внизу экрана.
pub const M_DOCK_H: u32 = 76;
/// Сколько плиток в ряду.
pub const M_COLUMNS: u32 = 4;
/// Высота ячейки приложения: плитка, зазор, подпись.
pub const M_CELL_H: u32 = 92;
/// Зазор между ячейками.
pub const M_CELL_GAP: u32 = 6;
/// Сторона кнопки в доке.
pub const M_DOCK_BTN: u32 = 48;
/// Высота строки «Пуска» на телефоне.
///
/// Вдвое выше настольной: в строку попадают пальцем, а не курсором, и
/// тридцать шесть точек — это половина подушечки.
pub const M_MENU_ROW_H: u32 = 62;
/// Скругление плитки внутри строки поиска.
pub const M_R_CHIP: u32 = 13;
/// Ширина полоски жеста под доком.
pub const M_HOME_W: u32 = 110;
/// Толщина полоски жеста.
pub const M_HOME_H: u32 = 4;

/// Какого рода машина перед человеком.
///
/// Не «телефон или ПК»: форма — это про то, чем в экран тыкают и сколько на нём
/// места, а не про то, что за железо внутри. Планшет с тачем и монитор с мышью
/// отличаются тем же самым.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    /// Курсор, большой экран, окна, которые двигают.
    Desktop,
    /// Палец, экран в ладони, одно окно во весь экран.
    Mobile,
}

static FORM: AtomicU8 = AtomicU8::new(0);

/// Форма этой машины.
#[must_use]
pub fn form() -> Form {
    if FORM.load(Ordering::Relaxed) == 1 { Form::Mobile } else { Form::Desktop }
}

/// Коротко то же самое — читается в условиях чаще, чем сравнение.
#[must_use]
pub fn is_mobile() -> bool {
    matches!(form(), Form::Mobile)
}

/// Запомнить форму. `true` — она изменилась.
pub fn set_form(form: Form) -> bool {
    let value = u8::from(matches!(form, Form::Mobile));
    FORM.swap(value, Ordering::Relaxed) != value
}

/// Какая форма у экрана такого размера.
///
/// Признак — **портрет и узкая сторона**, а не модель аппарата и не наличие
/// тача. Так это и есть по сути: экран выше, чем шире, и меньше девятисот точек
/// поперёк — это экран, который держат в руке. Тач признаком быть не может:
/// его шлёт и планшет USB, воткнутый в настольную машину.
#[must_use]
pub const fn form_for(width: u32, height: u32) -> Form {
    if height > width && width <= 900 { Form::Mobile } else { Form::Desktop }
}

/// Множитель размеров для экрана такого размера.
///
/// Разрешение перестало умножать шрифт — под каждый ряд он растеризован
/// отдельно, — но геометрию умножать по-прежнему надо: кнопка в тридцать точек
/// на экране 3840 меньше ногтя. Порог тот же, что у [`mini_ui::typeface::Tier`],
/// и это не совпадение: два разных порога развели бы шрифт и рамку вокруг него.
///
/// Второе правило добавлено телефоном, и оно про плотность, а не про число
/// точек. У экрана 720×1600 в ладони точек мало, а на дюйм их втрое больше, чем
/// у монитора: заголовок в 44 точки выходит вдвое мельче нужного, и попасть в
/// него пальцем нельзя. Мерить плотность нам нечем — прошивка её не сообщает, —
/// но форма экрана о ней говорит достаточно: портретный экран уже девятисот
/// точек не бывает редким.
#[must_use]
pub const fn geometry_scale(width: u32, height: u32) -> u32 {
    if width >= 2560 {
        return 2;
    }
    if matches!(form_for(width, height), Form::Mobile) {
        return 2;
    }
    1
}
