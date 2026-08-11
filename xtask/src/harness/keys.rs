//! Перевод текста в последовательность имён клавиш QEMU.
//!
//! Стенд печатает текст (`Step::Type`), а `sendkey` принимает имя клавиши по
//! раскладке US со списком модификаторов через дефис. Таблица ниже — и есть эта
//! раскладка, записанная один раз.
//!
//! Символы вне таблицы отвергаются, а не пропускаются молча: пропущенная буква
//! превращает проверку «в поле введено `freeos`» в «введено `freos`», и искать
//! причину придётся в системе, которая ни в чём не виновата.

use anyhow::{Result, bail};

/// Имя клавиши для символа, вместе с необходимостью Shift.
fn key_for(ch: char) -> Option<(&'static str, bool)> {
    let name = match ch {
        'a'..='z' => return Some((letter(ch), false)),
        'A'..='Z' => return Some((letter(ch.to_ascii_lowercase()), true)),
        '0' => ("0", false),
        '1' => ("1", false),
        '2' => ("2", false),
        '3' => ("3", false),
        '4' => ("4", false),
        '5' => ("5", false),
        '6' => ("6", false),
        '7' => ("7", false),
        '8' => ("8", false),
        '9' => ("9", false),
        ')' => ("0", true),
        '!' => ("1", true),
        '@' => ("2", true),
        '#' => ("3", true),
        '$' => ("4", true),
        '%' => ("5", true),
        '^' => ("6", true),
        '&' => ("7", true),
        '*' => ("8", true),
        '(' => ("9", true),
        ' ' => ("spc", false),
        '\n' => ("ret", false),
        '\t' => ("tab", false),
        '-' => ("minus", false),
        '_' => ("minus", true),
        '=' => ("equal", false),
        '+' => ("equal", true),
        '[' => ("bracket_left", false),
        '{' => ("bracket_left", true),
        ']' => ("bracket_right", false),
        '}' => ("bracket_right", true),
        ';' => ("semicolon", false),
        ':' => ("semicolon", true),
        '\'' => ("apostrophe", false),
        '"' => ("apostrophe", true),
        '`' => ("grave_accent", false),
        '~' => ("grave_accent", true),
        '\\' => ("backslash", false),
        '|' => ("backslash", true),
        ',' => ("comma", false),
        '<' => ("comma", true),
        '.' => ("dot", false),
        '>' => ("dot", true),
        '/' => ("slash", false),
        '?' => ("slash", true),
        _ => return None,
    };
    Some(name)
}

/// Имя буквенной клавиши. Таблица, а не `&ch.to_string()`: имена клавиш живут
/// столько же, сколько программа, а временная строка — нет.
const fn letter(ch: char) -> &'static str {
    match ch {
        'a' => "a", 'b' => "b", 'c' => "c", 'd' => "d", 'e' => "e", 'f' => "f",
        'g' => "g", 'h' => "h", 'i' => "i", 'j' => "j", 'k' => "k", 'l' => "l",
        'm' => "m", 'n' => "n", 'o' => "o", 'p' => "p", 'q' => "q", 'r' => "r",
        's' => "s", 't' => "t", 'u' => "u", 'v' => "v", 'w' => "w", 'x' => "x",
        'y' => "y", _ => "z",
    }
}

/// Разложить текст в аргументы `sendkey`.
pub fn spell(text: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let Some((name, shift)) = key_for(ch) else {
            bail!(
                "символ {ch:?} нельзя набрать через sendkey: его нет в таблице раскладки US.\n\
                 Добавьте его в xtask/src/harness/keys.rs или наберите строку через серийную линию."
            );
        };
        out.push(if shift { format!("shift-{name}") } else { name.to_string() });
    }
    Ok(out)
}
