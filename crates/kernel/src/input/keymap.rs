//! Раскладка: перевод позиции клавиши в символ.
//!
//! Единственное место в ядре, которое знает, что на клавише с кодом
//! [`KeyCode::Digit2`] нарисовано `2` и `@`. Драйверы об этом не знают
//! принципиально — см. заголовок [`super`].
//!
//! # Что реализовано
//!
//! US QWERTY и только он. Причина не в лени: русская раскладка требует не второй
//! таблицы, а решения двух вопросов, которых на этой фазе не существует, —
//! как переключаться между раскладками и в какой кодировке ядро держит текст
//! (ASCII в текущей экранной консоли не покрывает кириллицу вовсе, см. таблицу
//! глифов в [`crate::console`]). Добавить вторую таблицу к готовому решению
//! легко; выбрать решение задним числом, когда таблиц уже три, — нет.
//!
//! # Чего здесь нет намеренно
//!
//! Управляющих клавиш ([`KeyCode::Backspace`], [`KeyCode::Escape`], стрелок).
//! Они не символы, и выдавать для них `\u{8}` или `\u{1b}` значило бы стирать
//! разницу между «пользователь нажал Backspace» и «в поток пришёл байт 0x08».
//! Потребитель разбирает такие клавиши по [`KeyCode`] — так у него остаётся
//! возможность повести себя по-разному, а не только удалить символ.

use super::{KeyCode, KeyEvent, Modifiers};

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

    if let Some((lower, upper)) = letter(code) {
        // Caps и Shift складываются по XOR, а не по OR: при залипшем Caps
        // нажатый Shift даёт строчную букву. Это не тонкость реализации, а то,
        // как ведёт себя любая клавиатура.
        let upper_case = shift != mods.contains(Modifiers::CAPS);
        return Some(if upper_case { upper } else { lower });
    }

    if let Some((plain, shifted)) = printable(code) {
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
