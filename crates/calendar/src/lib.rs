//! Григорианский календарь: секунды эпохи Unix ↔ гражданская дата.
//!
//! # Зачем свой крейт, когда есть `time` и `chrono`
//!
//! Нужны ровно два преобразования в обе стороны и печать даты. Оба крейта
//! умеют работать без `std`, но взамен пришлось бы тянуть зависимость в четыре
//! двоичных файла на трёх целях — включая ядро, где каждая внешняя зависимость
//! это ещё и вопрос «а что она делает при нехватке памяти». Здесь же всё
//! содержимое — тридцать строк целочисленной арифметики без ветвлений и
//! выделений, которые проверяются на хосте обычным `cargo test`.
//!
//! Настоящая причина, по которой этот код вынесен из установщика: считать дату
//! теперь нужно и ядру. Установщик умел это с самого начала — он ставит время
//! на файлы, которые пишет, — и копия того же алгоритма в ядре разошлась бы с
//! оригиналом ровно в той мере, в какой её потом правили бы порознь.
//!
//! # Что этот крейт не делает
//!
//! Не знает о часовых поясах, переходе на летнее время и високосных секундах.
//! Часовой пояс — это смещение, которое прибавляет вызывающий (в системе оно
//! приходит из `/etc/system.cfg`), а не свойство календаря. Високосных секунд в
//! эпохе Unix не существует по определению: в сутках ровно 86 400 секунд.
//!
//! Отрицательные годы (до нашей эры) арифметика выдерживает, но датам ОС они
//! не нужны; о них сказано лишь потому, что алгоритм честно работает и там.

#![no_std]

// Тесты живут на хосте. Объявление обязательно: в `no_std`-крейте `std` не
// подключается сам даже там, где он доступен.
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use core::fmt;

/// Секунд в сутках. В эпохе Unix это константа, а не среднее значение:
/// високосная секунда в ней не представлена вовсе.
const SECONDS_PER_DAY: i64 = 86_400;

/// Гражданская дата и время.
///
/// Крейт не различает UTC и местное время: это одна и та же арифметика, а
/// какой момент описывают поля — знает тот, кто прибавлял смещение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i32,
    /// 1–12.
    pub month: u8,
    /// 1–31.
    pub day: u8,
    /// 0–23.
    pub hour: u8,
    /// 0–59.
    pub minute: u8,
    /// 0–59.
    pub second: u8,
}

impl DateTime {
    /// Начало эпохи Unix.
    pub const EPOCH: Self =
        Self { year: 1970, month: 1, day: 1, hour: 0, minute: 0, second: 0 };

    #[must_use]
    pub const fn new(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self { year, month, day, hour, minute, second }
    }

    /// Сколько секунд прошло от эпохи Unix до этого момента.
    #[must_use]
    pub const fn to_unix(&self) -> i64 {
        let days = days_from_civil(self.year as i64, self.month, self.day);
        days * SECONDS_PER_DAY
            + (self.hour as i64) * 3600
            + (self.minute as i64) * 60
            + (self.second as i64)
    }

    /// Разобрать секунды эпохи Unix в дату.
    ///
    /// Деление здесь именно с округлением вниз, а не к нулю: до 1970 года
    /// остаток от `/` в Rust отрицателен, и наивное `seconds / 86400` дало бы
    /// «31 декабря 1969, 00:00» вместо любого времени того дня.
    #[must_use]
    pub const fn from_unix(seconds: i64) -> Self {
        let days = seconds.div_euclid(SECONDS_PER_DAY);
        let rest = seconds.rem_euclid(SECONDS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Self {
            year: year as i32,
            month,
            day,
            hour: (rest / 3600) as u8,
            minute: ((rest % 3600) / 60) as u8,
            second: (rest % 60) as u8,
        }
    }
}

/// Печатается так, как система показывает время везде: `2026-08-12 20:47:51`.
///
/// Формат выбран не за красоту: он сортируется как текст, одинаково читается
/// человеком и разбирается стендом, и в нём нет ни местных названий месяцев, ни
/// неоднозначного порядка дня и месяца.
impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

/// Число дней от 1970-01-01 до заданной даты.
///
/// Алгоритм Говарда Хиннанта (`days_from_civil`): год сдвигается так, чтобы
/// март стал первым месяцем, и високосный день оказывается в конце года — после
/// чего длины месяцев описываются одной формулой, а правило високосности
/// сводится к арифметике над номером года. Ветвлений и таблиц нет, поэтому
/// ошибиться в конкретном месяце тут негде: неверен либо весь календарь, либо
/// ничего.
#[must_use]
pub const fn days_from_civil(year: i64, month: u8, day: u8) -> i64 {
    let month = month as i64;
    let day = day as i64;
    let year = if month <= 2 { year - 1 } else { year };
    // Эра — период в 400 лет, через который григорианский календарь полностью
    // повторяется (146 097 дней).
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Обратное преобразование: год, месяц и день по числу дней от 1970-01-01.
#[must_use]
pub const fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u8, day as u8)
}
