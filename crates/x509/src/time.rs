//! Даты из сертификата в секунды эпохи Unix.
//!
//! # Две записи одного и того же
//!
//! X.509 пишет срок действия то `UTCTime` (две цифры года), то
//! `GeneralizedTime` (четыре). Правило простое и записано в RFC 5280: до 2049
//! года включительно — первая запись, с 2050-го — вторая. Из этого следует
//! окно: `YY` от 50 до 99 означает девятнадцатый век двадцатого столетия
//! (1950–1999), от 00 до 49 — 2000–2049.
//!
//! # Строгость здесь дешевле снисходительности
//!
//! DER разрешает ровно одну форму: четыре (или две) цифры года, месяц, день,
//! час, минута, секунда и буква `Z`. Ни смещения часового пояса, ни дробных
//! секунд, ни пропущенных секунд в DER не бывает. Принимая их «на всякий
//! случай», получаешь разбор, который на одном сертификате читает минуты как
//! секунды.

use calendar::days_from_civil;

/// `YYMMDDHHMMSSZ`.
#[must_use]
pub fn utc_time(bytes: &[u8]) -> Option<i64> {
    if bytes.len() != 13 || bytes[12] != b'Z' {
        return None;
    }
    let two = number(&bytes[0..2])?;
    let year = if two >= 50 { 1900 + two } else { 2000 + two };
    parse_rest(year, &bytes[2..12])
}

/// `YYYYMMDDHHMMSSZ`.
#[must_use]
pub fn generalized_time(bytes: &[u8]) -> Option<i64> {
    if bytes.len() != 15 || bytes[14] != b'Z' {
        return None;
    }
    let year = number(&bytes[0..4])?;
    parse_rest(year, &bytes[4..14])
}

/// Общий хвост обеих записей: `MMDDHHMMSS`.
fn parse_rest(year: i64, bytes: &[u8]) -> Option<i64> {
    let month = number(&bytes[0..2])?;
    let day = number(&bytes[2..4])?;
    let hour = number(&bytes[4..6])?;
    let minute = number(&bytes[6..8])?;
    let second = number(&bytes[8..10])?;

    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Час 24 запрещён, а секунда 60 (високосная) в DER не пишется.
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Дата вроде 31 февраля отсекается сверкой: перевод в дни и обратно даёт
    // другое число, потому что арифметика Гаусса переносит лишние дни в
    // следующий месяц. Без этой сверки «31.02» прошло бы как 3 марта, а срок
    // действия сертификата сдвинулся бы на пару дней молча.
    let days = days_from_civil(year, month as u8, day as u8);
    let (back_year, back_month, back_day) = calendar::civil_from_days(days);
    if back_year != year || i64::from(back_month) != month || i64::from(back_day) != day {
        return None;
    }

    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Целое из цифр ASCII; любая не-цифра — отказ.
fn number(bytes: &[u8]) -> Option<i64> {
    let mut value = 0i64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::{generalized_time, utc_time};

    /// Точка отсчёта, посчитанная руками.
    #[test]
    fn the_epoch_is_zero() {
        assert_eq!(generalized_time(b"19700101000000Z"), Some(0));
        assert_eq!(utc_time(b"700101000000Z"), Some(0));
    }

    /// Окно двухзначного года: 49 — это 2049, 50 — это 1950.
    #[test]
    fn the_two_digit_year_window_is_the_one_rfc_5280_names() {
        let y2049 = utc_time(b"491231235959Z").expect("2049 год");
        let y1950 = utc_time(b"500101000000Z").expect("1950 год");
        assert!(y1950 < 0, "1950 год раньше эпохи");
        assert!(y2049 > 0);
        assert_eq!(y2049, 2_524_607_999);
    }

    /// Настоящая дата из настоящего сертификата.
    #[test]
    fn a_real_notafter_matches() {
        // 2026-08-15 07:00:00 UTC
        assert_eq!(generalized_time(b"20260815070000Z"), Some(1_786_777_200));
    }

    /// Того, чего DER не пишет, разбор не принимает.
    #[test]
    fn only_the_der_form_is_accepted() {
        // Смещение часового пояса вместо `Z`.
        assert_eq!(utc_time(b"2601010000+0300"), None);
        // Без секунд.
        assert_eq!(utc_time(b"2601010000Z"), None);
        // Тридцать первое февраля.
        assert_eq!(generalized_time(b"20260231000000Z"), None);
        // Двадцать пятый час.
        assert_eq!(generalized_time(b"20260101250000Z"), None);
        // Буквы вместо цифр.
        assert_eq!(generalized_time(b"2026010100000ZZ"), None);
    }
}
