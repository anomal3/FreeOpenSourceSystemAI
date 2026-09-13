//! Файлы `ключ=значение` из `/etc`: чтение, правка и разбор известных ключей.
//!
//! # Почему отдельный крейт, а не модуль ядра
//!
//! Из-за проверок. Разбор настроек — чистые функции над строкой: на входе текст,
//! на выходе число или признак. Ошибка в них выглядит как «система не запомнила
//! часовой пояс», то есть требует загрузки, чтобы её заметить, — а стоит она
//! секунды, если проверять на хосте. Но ядро под хост не собирается вовсе
//! (`#![no_std]`, `#![no_main]`, ассемблер под архитектуру), и `#[cfg(test)]`
//! внутри него — это тесты, которых никто никогда не запустит. Хуже, чем их
//! отсутствие: они выглядят как проверенное место.
//!
//! Поэтому разбор живёт здесь, рядом со своими тестами, а ядро его зовёт.
//!
//! # Что здесь есть и чего нет
//!
//! Есть общее чтение `ключ=значение` и правка одной строки. Нет ничего, что
//! требует знать систему: адреса разбирает тот, у кого есть `Ipv4`, файлы читает
//! тот, у кого есть файловая система. Здесь только текст.
//!
//! # Про формат
//!
//! `ключ=значение`, по строке на пару, `#` в начале строки — примечание. Формат
//! придумали не мы и не здесь: так устроены `system.cfg` установщика,
//! `update.cfg` и `services`. Разбор терпимый — файл правят руками, и непонятная
//! строка обязана быть пропущена, а не уронить загрузку.

#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::String;

/// Значение ключа — первое, если их несколько.
///
/// Первое, а не последнее: файл читают несколько разных программ, и «первое
/// побеждает» — единственное правило, которое одинаково очевидно всем. Дубликат
/// при этом не ошибка: его вносит человек, дописавший строку вместо правки, и
/// отказ разбирать такой файл оставил бы машину без настроек целиком.
#[must_use]
pub fn value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, found)) = line.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return Some(found.trim());
        }
    }
    None
}

/// Заменить одну строку `ключ=значение`, не тронув остальные.
///
/// В `system.cfg` лежат язык, раскладка, пояс, имя пользователя и его дом — их
/// пишет установщик. Переписать файл целиком ради одного ключа значило бы
/// стереть то, чего пишущий не знает, а не знает он ровно того, что добавит
/// следующая фаза.
///
/// Порядок строк сохраняется, отсутствующий ключ дописывается в конец, а
/// повторы выбрасываются: файл с двумя разными значениями одного ключа читается
/// по-разному разными разборщиками, и это хуже, чем потеря дубликата.
#[must_use]
pub fn replace_key(text: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in text.lines() {
        let is_this_key = line
            .trim()
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == key);
        if is_this_key {
            if !replaced {
                out.push_str(&format!("{key}={value}\n"));
                replaced = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

/// Убрать ключ вовсе.
#[must_use]
pub fn remove_key(text: &str, key: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let is_this_key = line
            .trim()
            .split_once('=')
            .is_some_and(|(name, _)| name.trim() == key);
        if !is_this_key {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Известные ключи
// ---------------------------------------------------------------------------

/// Смещение часового пояса в минутах из строки `timezone=UTC+03:00`.
///
/// Принимаются три написания, и все три встречаются: `UTC+03:00` пишет
/// установщик, `+3` пишет человек, `-05:30` — человек в Индии, если ему когда-
/// нибудь предложат такой пояс. Часы проверяются пределом 14 — больше не бывает
/// ни у одного пояса на Земле, — минуты пределом 60.
#[must_use]
pub fn timezone_minutes(text: &str) -> Option<i32> {
    let raw = value(text, "timezone")?;
    parse_offset(raw)
}

/// Разобрать одно смещение: `UTC+03:00`, `+3`, `-05:30`.
#[must_use]
pub fn parse_offset(raw: &str) -> Option<i32> {
    let raw = raw.trim();
    let raw = raw.strip_prefix("UTC").unwrap_or(raw);
    let raw = raw.strip_prefix("utc").unwrap_or(raw);
    let (negative, rest) = match raw.as_bytes().first()? {
        b'-' => (true, &raw[1..]),
        b'+' => (false, &raw[1..]),
        _ => (false, raw),
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None => (rest, "0"),
    };
    let hours: i32 = hours.trim().parse().ok()?;
    let minutes: i32 = minutes.trim().parse().ok()?;
    if !(0..=14).contains(&hours) || !(0..60).contains(&minutes) {
        return None;
    }
    let total = hours * 60 + minutes;
    Some(if negative { -total } else { total })
}

/// Смещение в том виде, в каком оно пишется в файл: `UTC+03:00`.
#[must_use]
pub fn offset_text(minutes: i32) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let absolute = minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", absolute / 60, absolute % 60)
}

/// Тёмная ли тема по строке `theme=dark`.
///
/// `None` — ключа нет либо значение непонятное. Непонятное не считается тёмной
/// темой намеренно: опечатка в файле не обязана менять вид системы.
#[must_use]
pub fn theme_dark(text: &str) -> Option<bool> {
    match value(text, "theme")? {
        "dark" => Some(true),
        "light" => Some(false),
        _ => None,
    }
}

/// Откуда брать сетевой адрес: строка `mode=static`.
///
/// `true` — постоянный адрес. Всё остальное, включая отсутствие ключа, — DHCP:
/// машина без настроек обязана попытаться получить адрес, а не остаться без
/// сети.
#[must_use]
pub fn network_is_static(text: &str) -> bool {
    value(text, "mode").is_some_and(|mode| mode.eq_ignore_ascii_case("static"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_is_found_and_trimmed() {
        let text = "# заметка\nlanguage=ru\n  timezone = UTC+03:00  \n";
        assert_eq!(value(text, "language"), Some("ru"));
        assert_eq!(value(text, "timezone"), Some("UTC+03:00"));
        assert_eq!(value(text, "keyboard"), None);
    }

    /// Первое вхождение побеждает, и это правило, а не случайность разбора.
    #[test]
    fn the_first_value_wins() {
        assert_eq!(value("a=1\na=2\n", "a"), Some("1"));
    }

    /// Соседние строки переживают правку — ради этого функция и существует.
    #[test]
    fn replacing_a_key_leaves_the_neighbours_alone() {
        let text = "language=ru\ntimezone=UTC+03:00\nuser=roman\n";
        let out = replace_key(text, "timezone", "UTC+05:00");
        assert_eq!(out, "language=ru\ntimezone=UTC+05:00\nuser=roman\n");
    }

    /// Отсутствующий ключ дописывается, а не теряется.
    #[test]
    fn a_missing_key_is_appended() {
        assert_eq!(replace_key("a=1\n", "b", "2"), "a=1\nb=2\n");
        assert_eq!(replace_key("", "b", "2"), "b=2\n");
    }

    /// Повтор схлопывается: файл с двумя значениями одного ключа читается
    /// по-разному разными разборщиками.
    #[test]
    fn duplicates_collapse_into_one() {
        assert_eq!(replace_key("a=1\nb=0\na=2\n", "a", "9"), "a=9\nb=0\n");
    }

    #[test]
    fn removing_a_key_keeps_the_rest() {
        assert_eq!(remove_key("a=1\nb=2\n", "a"), "b=2\n");
        assert_eq!(remove_key("a=1\n", "zzz"), "a=1\n");
    }

    /// Три написания смещения, все встречающиеся в живых файлах.
    #[test]
    fn every_written_form_of_an_offset_parses() {
        assert_eq!(parse_offset("UTC+03:00"), Some(180));
        assert_eq!(parse_offset("+3"), Some(180));
        assert_eq!(parse_offset("-05:30"), Some(-330));
        assert_eq!(parse_offset("UTC+00:00"), Some(0));
    }

    /// Пояса больше четырнадцати часов и минут больше пятидесяти девяти не
    /// бывает. Принять такое — значит показывать человеку неверное время и не
    /// уметь объяснить почему.
    #[test]
    fn impossible_offsets_are_refused() {
        assert_eq!(parse_offset("+15:00"), None);
        assert_eq!(parse_offset("+03:99"), None);
        assert_eq!(parse_offset("завтра"), None);
        assert_eq!(parse_offset(""), None);
    }

    /// Записанное смещение читается обратно тем же числом. Проверка круга: у
    /// знака здесь два места, где его можно потерять.
    #[test]
    fn an_offset_survives_a_round_trip() {
        for minutes in [-330, -180, 0, 180, 330, 14 * 60] {
            assert_eq!(parse_offset(&offset_text(minutes)), Some(minutes), "{minutes}");
        }
    }

    #[test]
    fn the_theme_reads_both_ways_and_ignores_nonsense() {
        assert_eq!(theme_dark("theme=dark\n"), Some(true));
        assert_eq!(theme_dark("theme=light\n"), Some(false));
        assert_eq!(theme_dark("theme=purple\n"), None);
        assert_eq!(theme_dark(""), None);
    }

    /// Машина без настроек идёт за адресом в сеть, а не остаётся без него.
    #[test]
    fn network_defaults_to_dhcp() {
        assert!(!network_is_static(""));
        assert!(!network_is_static("mode=dhcp\n"));
        assert!(network_is_static("mode=static\n"));
        assert!(network_is_static("mode=STATIC\n"));
    }
}
