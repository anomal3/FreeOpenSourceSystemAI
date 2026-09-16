//! Раскладка: перевод позиции клавиши в символ.
//!
//! Единственное место в ядре, которое знает, что на клавише с кодом
//! [`KeyCode::Digit2`] нарисовано `2` и `@`. Драйверы об этом не знают
//! принципиально — см. заголовок [`super`].
//!
//! # Раскладки
//!
//! Две: US QWERTY и русская ЙЦУКЕН. Действующая — одно состояние на всё ядро
//! ([`layout`]), а не свойство окна: клавиатура одна, и человек, переключивший
//! её в терминале, ждёт кириллицы и в поле имени файла. Переключается сочетанием
//! Alt+Shift (см. [`observe`]) и Win+Пробел на столе — ровно теми, к которым
//! привык человек с Windows, — а при загрузке берётся из `/etc/system.cfg`,
//! куда её записал установщик ([`adopt`]).
//!
//! Русская таблица — та, что в Windows: `ё` на клавише слева от единицы, точка
//! и запятая на клавише `/`, `№` на Shift+3. Ctrl-сочетания при этом считаются
//! по латинской позиции клавиши: Ctrl+C — это 0x03 независимо от раскладки,
//! иначе прервать программу в русской раскладке было бы нечем.
//!
//! Кириллица доезжает до экрана как UTF-8: сетка терминала хранит символы, а
//! не байты, и шрифт стола содержит русский алфавит. Текстовая консоль без
//! графики знает только ASCII и печатает вместо буквы `?` — это ограничение
//! консоли, а не раскладки.
//!
//! # Чего здесь нет намеренно
//!
//! Управляющих клавиш ([`KeyCode::Backspace`], [`KeyCode::Escape`], стрелок).
//! Они не символы, и выдавать для них `\u{8}` или `\u{1b}` значило бы стирать
//! разницу между «пользователь нажал Backspace» и «в поток пришёл байт 0x08».
//! Потребитель разбирает такие клавиши по [`KeyCode`] — так у него остаётся
//! возможность повести себя по-разному, а не только удалить символ.

use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use super::{KeyCode, KeyEvent, Modifiers};

/// Раскладка клавиатуры.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// Латиница, US QWERTY.
    Us,
    /// Кириллица, ЙЦУКЕН.
    Ru,
}

impl Layout {
    /// Как раскладка записана в `/etc/system.cfg` — теми же словами, что у
    /// установщика.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Layout::Us => "us",
            Layout::Ru => "ru",
        }
    }

    /// Две буквы для трея — язык, а не раскладка: так подписан индикатор
    /// в Windows, и так его читают, не задумываясь.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Layout::Us => "EN",
            Layout::Ru => "RU",
        }
    }

    /// Имя для меню и журнала.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Layout::Us => "English (US)",
            Layout::Ru => "Русская",
        }
    }

    /// Другая из двух.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Layout::Us => Layout::Ru,
            Layout::Ru => Layout::Us,
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "us" | "en" => Some(Layout::Us),
            "ru" => Some(Layout::Ru),
            _ => None,
        }
    }

    const fn from_bits(bits: u8) -> Self {
        match bits {
            1 => Layout::Ru,
            _ => Layout::Us,
        }
    }
}

/// Действующая раскладка. Атомик, а не замок: читается на каждую клавишу, в
/// том числе из мест, где замок брать нельзя.
static LAYOUT: AtomicU8 = AtomicU8::new(0);

/// Сочетание Alt+Shift уже сработало и клавиши ещё не отпущены.
///
/// Без этого автоповтор удерживаемого Shift переключал бы раскладку десять
/// раз в секунду — туда и обратно, и в момент отпускания она оказывалась бы
/// случайной.
static CHORD_HELD: AtomicBool = AtomicBool::new(false);

/// Какая раскладка действует сейчас.
#[must_use]
pub fn layout() -> Layout {
    Layout::from_bits(LAYOUT.load(Ordering::Relaxed))
}

/// Переключить раскладку и назвать новую в журнале.
///
/// Строка журнала — то, по чему стенд узнаёт, что сочетание дошло: на снимке
/// экрана переключение видно только по двум буквам в трее.
pub fn set_layout(layout: Layout) {
    LAYOUT.store(layout as u8, Ordering::Relaxed);
    crate::kprintln!("  keyboard    : layout {} ({})", layout.label(), layout.tag());
}

/// Переключить на другую из двух. Возвращает новую.
pub fn toggle_layout() -> Layout {
    let next = layout().other();
    set_layout(next);
    next
}

/// Посмотреть на событие: не сочетание ли это переключения раскладки.
///
/// `true` — было сочетание Alt+Shift, и раскладка **уже переключена**.
/// Зовёт тот, кто разбирает клавиши первым: стол в графическом режиме, редактор
/// строки без графики. Один потребитель на событие — иначе сочетание
/// переключало бы дважды и оставляло раскладку прежней.
///
/// Работает по нажатию второй клавиши сочетания, какая бы из двух ни была
/// второй: Alt, потом Shift — или Shift, потом Alt. Так ведёт себя Windows, и
/// человек не помнит, в каком порядке нажимает.
pub fn observe(event: KeyEvent) -> bool {
    let both = event.mods.contains(Modifiers::ALT) && event.mods.contains(Modifiers::SHIFT);
    if !both {
        CHORD_HELD.store(false, Ordering::Relaxed);
        return false;
    }
    let chord_key = matches!(
        event.code,
        KeyCode::LeftShift | KeyCode::RightShift | KeyCode::LeftAlt | KeyCode::RightAlt
    );
    if !event.pressed || !chord_key {
        return false;
    }
    if CHORD_HELD.swap(true, Ordering::Relaxed) {
        return false;
    }
    toggle_layout();
    true
}

/// Прочитать раскладку из `/etc/system.cfg`.
///
/// Вызывается один раз после монтирования корня, там же, где часовой пояс. Нет
/// файла или ключа — остаётся US, молча: на загруженной с носителя системе
/// настроек нет, и это не событие.
pub fn adopt() {
    const CONFIG: &str = "system.cfg";
    const LIMIT: usize = 4096;
    let Some((bytes, source)) = crate::config::read(CONFIG, LIMIT) else {
        return;
    };
    let text = core::str::from_utf8(&bytes).unwrap_or("");
    let Some(layout) = sysconf::value(text, "keyboard").and_then(Layout::from_tag) else {
        return;
    };
    LAYOUT.store(layout as u8, Ordering::Relaxed);
    crate::kprintln!(
        "  keyboard    : layout {} ({}) from {}",
        layout.label(),
        layout.tag(),
        crate::config::path(CONFIG, source)
    );
}

/// Символ, который даёт это событие, или `None`, если события символ не даёт.
///
/// Отпускание клавиши символа не даёт никогда: текст порождается нажатием.
/// Функция сделана press-only сознательно — иначе каждый потребитель обязан был
/// бы помнить про фильтр, и один забытый `if event.pressed` даёт удвоение
/// каждого набранного символа.
#[must_use]
pub fn char_for(event: KeyEvent) -> Option<char> {
    if !event.pressed {
        return None;
    }
    char_for_code(event.code, event.mods)
}

/// Латинская буква клавиши: что она даёт в раскладке US без Shift.
///
/// Нужна сочетаниям (фаза N7h, [`user_abi::WinEvent::y`]): `Ctrl+O` называет
/// клавишу, а не символ, и в русской раскладке остаётся `Ctrl+O`. Раскладка
/// здесь не читается нарочно — ровно в этом и смысл.
#[must_use]
pub fn latin(code: KeyCode) -> Option<char> {
    letter(code).map(|(lower, _)| lower).or_else(|| printable(code).map(|(plain, _)| plain))
}

/// Символ клавиши при заданных модификаторах, без учёта нажатия/отпускания.
#[must_use]
pub fn char_for_code(code: KeyCode, mods: Modifiers) -> Option<char> {
    // Ctrl обрабатывается раньше раскладки: Ctrl+C — это байт 0x03, а не символ
    // `c` с флажком. Порядок именно такой, потому что комбинация с Ctrl
    // перекрывает и Shift, и Caps.
    if mods.contains(Modifiers::CTRL) {
        return control_char(code);
    }

    let shift = mods.contains(Modifiers::SHIFT);
    let cyrillic = layout() == Layout::Ru;

    if let Some((lower, upper)) = if cyrillic { ru_letter(code) } else { letter(code) } {
        // Caps и Shift складываются по XOR, а не по OR: при залипшем Caps
        // нажатый Shift даёт строчную букву. Это не тонкость реализации, а то,
        // как ведёт себя любая клавиатура.
        let upper_case = shift != mods.contains(Modifiers::CAPS);
        return Some(if upper_case { upper } else { lower });
    }

    // Знаки в русской раскладке лежат иначе только на верхнем ряду и двух
    // клавишах справа; всё, чего нет в её таблице, берётся из общей — цифровой
    // блок и минус с равно у обеих раскладок одни.
    let pair = if cyrillic { ru_printable(code) } else { None };
    if let Some((plain, shifted)) = pair.or_else(|| printable(code)) {
        return Some(if shift { shifted } else { plain });
    }

    match code {
        KeyCode::Space => Some(' '),
        KeyCode::Enter | KeyCode::KeypadEnter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        // Цифровой блок даёт цифры только при включённом Num Lock. Без него
        // клавиши работают как навигационные, и подставлять цифру значило бы
        // печатать `4` там, где пользователь нажал «влево».
        _ if mods.contains(Modifiers::NUM_LOCK) => keypad_digit(code),
        _ => None,
    }
}

/// Клавиши, дающие знаки, — в них ищет [`code_for`].
const PRINTING: [KeyCode; 48] = [
    KeyCode::A, KeyCode::B, KeyCode::C, KeyCode::D, KeyCode::E, KeyCode::F, KeyCode::G,
    KeyCode::H, KeyCode::I, KeyCode::J, KeyCode::K, KeyCode::L, KeyCode::M, KeyCode::N,
    KeyCode::O, KeyCode::P, KeyCode::Q, KeyCode::R, KeyCode::S, KeyCode::T, KeyCode::U,
    KeyCode::V, KeyCode::W, KeyCode::X, KeyCode::Y, KeyCode::Z,
    KeyCode::Digit0, KeyCode::Digit1, KeyCode::Digit2, KeyCode::Digit3, KeyCode::Digit4,
    KeyCode::Digit5, KeyCode::Digit6, KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9,
    KeyCode::Minus, KeyCode::Equal, KeyCode::LeftBracket, KeyCode::RightBracket,
    KeyCode::Backslash, KeyCode::Semicolon, KeyCode::Apostrophe, KeyCode::Grave,
    KeyCode::Comma, KeyCode::Period, KeyCode::Slash, KeyCode::Space,
];

/// Знак клавиши в **заданной** раскладке — без Caps, Ctrl и Num Lock.
fn char_in(layout: Layout, code: KeyCode, shift: bool) -> Option<char> {
    let cyrillic = layout == Layout::Ru;
    if let Some((lower, upper)) = if cyrillic { ru_letter(code) } else { letter(code) } {
        return Some(if shift { upper } else { lower });
    }
    let pair = if cyrillic { ru_printable(code) } else { None };
    if let Some((plain, shifted)) = pair.or_else(|| printable(code)) {
        return Some(if shift { shifted } else { plain });
    }
    (code == KeyCode::Space).then_some(' ')
}

/// Какой клавишей, и с Shift ли, набирается знак в этой раскладке. `None` —
/// в ней его нет: латиницы в русской, кириллицы в английской.
///
/// Нужна экранной клавиатуре. Она посылает коды клавиш, как USB-клавиатура,
/// а знак из кода делает раскладка — поэтому «/» в русской раскладке набирается
/// не той клавишей, что в английской (там на ней точка).
#[must_use]
pub fn code_for(ch: char, layout: Layout) -> Option<(KeyCode, bool)> {
    PRINTING.iter().find_map(|&code| {
        [false, true]
            .into_iter()
            .find(|&shift| char_in(layout, code, shift) == Some(ch))
            .map(|shift| (code, shift))
    })
}

/// Буква: строчный и заглавный варианты.
const fn letter(code: KeyCode) -> Option<(char, char)> {
    let pair = match code {
        KeyCode::A => ('a', 'A'),
        KeyCode::B => ('b', 'B'),
        KeyCode::C => ('c', 'C'),
        KeyCode::D => ('d', 'D'),
        KeyCode::E => ('e', 'E'),
        KeyCode::F => ('f', 'F'),
        KeyCode::G => ('g', 'G'),
        KeyCode::H => ('h', 'H'),
        KeyCode::I => ('i', 'I'),
        KeyCode::J => ('j', 'J'),
        KeyCode::K => ('k', 'K'),
        KeyCode::L => ('l', 'L'),
        KeyCode::M => ('m', 'M'),
        KeyCode::N => ('n', 'N'),
        KeyCode::O => ('o', 'O'),
        KeyCode::P => ('p', 'P'),
        KeyCode::Q => ('q', 'Q'),
        KeyCode::R => ('r', 'R'),
        KeyCode::S => ('s', 'S'),
        KeyCode::T => ('t', 'T'),
        KeyCode::U => ('u', 'U'),
        KeyCode::V => ('v', 'V'),
        KeyCode::W => ('w', 'W'),
        KeyCode::X => ('x', 'X'),
        KeyCode::Y => ('y', 'Y'),
        KeyCode::Z => ('z', 'Z'),
        _ => return None,
    };
    Some(pair)
}

/// Буква русской раскладки: строчная и заглавная.
///
/// Букв здесь больше, чем в латинской: `ё`, `х`, `ъ`, `ж`, `э`, `б`, `ю` стоят
/// на клавишах, где в US лежат знаки. Это буквы, и Caps Lock действует на них
/// так же, как на остальные, — потому они здесь, а не в [`ru_printable`].
const fn ru_letter(code: KeyCode) -> Option<(char, char)> {
    let pair = match code {
        KeyCode::Q => ('й', 'Й'),
        KeyCode::W => ('ц', 'Ц'),
        KeyCode::E => ('у', 'У'),
        KeyCode::R => ('к', 'К'),
        KeyCode::T => ('е', 'Е'),
        KeyCode::Y => ('н', 'Н'),
        KeyCode::U => ('г', 'Г'),
        KeyCode::I => ('ш', 'Ш'),
        KeyCode::O => ('щ', 'Щ'),
        KeyCode::P => ('з', 'З'),
        KeyCode::LeftBracket => ('х', 'Х'),
        KeyCode::RightBracket => ('ъ', 'Ъ'),
        KeyCode::A => ('ф', 'Ф'),
        KeyCode::S => ('ы', 'Ы'),
        KeyCode::D => ('в', 'В'),
        KeyCode::F => ('а', 'А'),
        KeyCode::G => ('п', 'П'),
        KeyCode::H => ('р', 'Р'),
        KeyCode::J => ('о', 'О'),
        KeyCode::K => ('л', 'Л'),
        KeyCode::L => ('д', 'Д'),
        KeyCode::Semicolon => ('ж', 'Ж'),
        KeyCode::Apostrophe => ('э', 'Э'),
        KeyCode::Z => ('я', 'Я'),
        KeyCode::X => ('ч', 'Ч'),
        KeyCode::C => ('с', 'С'),
        KeyCode::V => ('м', 'М'),
        KeyCode::B => ('и', 'И'),
        KeyCode::N => ('т', 'Т'),
        KeyCode::M => ('ь', 'Ь'),
        KeyCode::Comma => ('б', 'Б'),
        KeyCode::Period => ('ю', 'Ю'),
        KeyCode::Grave => ('ё', 'Ё'),
        _ => return None,
    };
    Some(pair)
}

/// Знаки русской раскладки, отличающиеся от US: обычный и с Shift.
const fn ru_printable(code: KeyCode) -> Option<(char, char)> {
    let pair = match code {
        KeyCode::Digit2 => ('2', '"'),
        KeyCode::Digit3 => ('3', '№'),
        KeyCode::Digit4 => ('4', ';'),
        KeyCode::Digit6 => ('6', ':'),
        KeyCode::Digit7 => ('7', '?'),
        KeyCode::Backslash => ('\\', '/'),
        KeyCode::Slash => ('.', ','),
        _ => return None,
    };
    Some(pair)
}

/// Печатный символ, не являющийся буквой: обычный и с Shift.
const fn printable(code: KeyCode) -> Option<(char, char)> {
    let pair = match code {
        KeyCode::Digit1 => ('1', '!'),
        KeyCode::Digit2 => ('2', '@'),
        KeyCode::Digit3 => ('3', '#'),
        KeyCode::Digit4 => ('4', '$'),
        KeyCode::Digit5 => ('5', '%'),
        KeyCode::Digit6 => ('6', '^'),
        KeyCode::Digit7 => ('7', '&'),
        KeyCode::Digit8 => ('8', '*'),
        KeyCode::Digit9 => ('9', '('),
        KeyCode::Digit0 => ('0', ')'),
        KeyCode::Minus => ('-', '_'),
        KeyCode::Equal => ('=', '+'),
        KeyCode::LeftBracket => ('[', '{'),
        KeyCode::RightBracket => (']', '}'),
        KeyCode::Backslash => ('\\', '|'),
        KeyCode::Semicolon => (';', ':'),
        KeyCode::Apostrophe => ('\'', '"'),
        KeyCode::Grave => ('`', '~'),
        KeyCode::Comma => (',', '<'),
        KeyCode::Period => ('.', '>'),
        KeyCode::Slash => ('/', '?'),
        // Цифровой блок: Shift на нём ничего не меняет.
        KeyCode::KeypadSlash => ('/', '/'),
        KeyCode::KeypadAsterisk => ('*', '*'),
        KeyCode::KeypadMinus => ('-', '-'),
        KeyCode::KeypadPlus => ('+', '+'),
        _ => return None,
    };
    Some(pair)
}

/// Цифра на дополнительном блоке при включённом Num Lock.
const fn keypad_digit(code: KeyCode) -> Option<char> {
    let ch = match code {
        KeyCode::Keypad0 => '0',
        KeyCode::Keypad1 => '1',
        KeyCode::Keypad2 => '2',
        KeyCode::Keypad3 => '3',
        KeyCode::Keypad4 => '4',
        KeyCode::Keypad5 => '5',
        KeyCode::Keypad6 => '6',
        KeyCode::Keypad7 => '7',
        KeyCode::Keypad8 => '8',
        KeyCode::Keypad9 => '9',
        KeyCode::KeypadPeriod => '.',
        _ => return None,
    };
    Some(ch)
}

/// Управляющий символ для комбинации с Ctrl.
///
/// Отображение историческое и стандартное: Ctrl снимает у ASCII-кода буквы
/// старшие биты, оставляя 1..26. Именно поэтому Ctrl+C — это 0x03 (ETX,
/// «прервать»), а Ctrl+D — 0x04 (EOT, «конец ввода»); терминалы, драйверы tty и
/// программы вроде оболочки полагаются ровно на эти числа.
const fn control_char(code: KeyCode) -> Option<char> {
    if let Some((lower, _)) = letter(code) {
        // 'a' = 0x61, и 0x61 & 0x1F = 1. Арифметика вместо таблицы — потому что
        // это и есть определение управляющего символа, а не совпадение.
        let value = (lower as u32) & 0x1F;
        return char::from_u32(value);
    }
    let ch = match code {
        // Продолжение того же правила за пределами букв: коды 0x1B..0x1F.
        KeyCode::LeftBracket => '\u{1b}', // Ctrl+[ = Escape
        KeyCode::Backslash => '\u{1c}',
        KeyCode::RightBracket => '\u{1d}',
        KeyCode::Digit6 => '\u{1e}',
        KeyCode::Minus => '\u{1f}',
        // Ctrl+Space традиционно даёт NUL — им пользуются редакторы.
        KeyCode::Space => '\0',
        // Enter и Tab с Ctrl дают то же, что и без него: их собственные коды
        // и так лежат в управляющем диапазоне.
        KeyCode::Enter | KeyCode::KeypadEnter => '\n',
        KeyCode::Tab => '\t',
        _ => return None,
    };
    Some(ch)
}
