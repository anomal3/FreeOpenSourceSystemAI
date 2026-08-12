//! Проверки календаря на хосте.
//!
//! Значения эпохи взяты не из этой реализации, а из общеизвестных: начало
//! эпохи, «миллиард секунд», дата, которую любят приводить в примерах, и два
//! года у границы правила високосности. Сверять реализацию с самой собой
//! бессмысленно — она согласуется с собой по построению.

use crate::{DateTime, civil_from_days, days_from_civil};

/// Известные моменты: секунды эпохи и то, что они означают.
const KNOWN: &[(i64, DateTime)] = &[
    (0, DateTime::new(1970, 1, 1, 0, 0, 0)),
    (1, DateTime::new(1970, 1, 1, 0, 0, 1)),
    (86_399, DateTime::new(1970, 1, 1, 23, 59, 59)),
    (86_400, DateTime::new(1970, 1, 2, 0, 0, 0)),
    (946_684_800, DateTime::new(2000, 1, 1, 0, 0, 0)),
    (1_000_000_000, DateTime::new(2001, 9, 9, 1, 46, 40)),
    (1_234_567_890, DateTime::new(2009, 2, 13, 23, 31, 30)),
    (1_700_000_000, DateTime::new(2023, 11, 14, 22, 13, 20)),
];

#[test]
fn known_moments_convert_both_ways() {
    for (seconds, date) in KNOWN {
        assert_eq!(DateTime::from_unix(*seconds), *date, "разбор {seconds}");
        assert_eq!(date.to_unix(), *seconds, "сборка {date}");
    }
}

/// 2000 год високосный, 1900 и 2100 — нет. Это то самое правило, ради которого
/// в алгоритме есть деление на 100 и на 400, и единственное место, где
/// григорианский календарь отличается от юлианского.
#[test]
fn century_leap_rule() {
    // 2000-02-28 → 29 февраля существует.
    assert_eq!(
        DateTime::from_unix(DateTime::new(2000, 2, 28, 0, 0, 0).to_unix() + 86_400),
        DateTime::new(2000, 2, 29, 0, 0, 0)
    );
    // 1900-02-28 → сразу март.
    assert_eq!(
        DateTime::from_unix(DateTime::new(1900, 2, 28, 0, 0, 0).to_unix() + 86_400),
        DateTime::new(1900, 3, 1, 0, 0, 0)
    );
    // 2100-02-28 → тоже сразу март.
    assert_eq!(
        DateTime::from_unix(DateTime::new(2100, 2, 28, 0, 0, 0).to_unix() + 86_400),
        DateTime::new(2100, 3, 1, 0, 0, 0)
    );
    // Обычный високосный год правилу веков не подчиняется.
    assert_eq!(
        DateTime::from_unix(DateTime::new(2024, 2, 28, 0, 0, 0).to_unix() + 86_400),
        DateTime::new(2024, 2, 29, 0, 0, 0)
    );
}

/// До эпохи. Проверка существует потому, что часы, которых никогда не ставили,
/// вполне могут отдать время до 1970 года, и целочисленное деление в Rust
/// округляет к нулю, а не вниз — наивная реализация ошибается здесь на сутки.
#[test]
fn before_the_epoch() {
    assert_eq!(DateTime::from_unix(-1), DateTime::new(1969, 12, 31, 23, 59, 59));
    assert_eq!(DateTime::from_unix(-86_400), DateTime::new(1969, 12, 31, 0, 0, 0));
    assert_eq!(DateTime::new(1969, 12, 31, 23, 59, 59).to_unix(), -1);
    assert_eq!(DateTime::new(1, 1, 1, 0, 0, 0).to_unix(), -62_135_596_800);
}

/// Каждые сутки с 1800 по 2200 год переводятся туда и обратно без потерь, а
/// день следует за днём без пропусков и повторов. Точечные значения показывают,
/// что реализация не разъехалась с календарём в известных местах; этот проход —
/// что она не разъезжается и в остальных ста сорока тысячах.
#[test]
fn round_trip_over_four_centuries() {
    let first = days_from_civil(1800, 1, 1);
    let last = days_from_civil(2200, 1, 1);
    let mut previous = None;

    for day in first..last {
        let (year, month, date) = civil_from_days(day);
        assert_eq!(days_from_civil(year, month, date), day, "день {day}");
        assert!((1..=12).contains(&month), "месяц {month} на дне {day}");
        assert!((1..=31).contains(&date), "число {date} на дне {day}");

        // Дата обязана строго возрастать: пропуск или повтор суток здесь
        // означал бы ошибку в длине месяца, которую точечные проверки могли бы
        // и не задеть.
        let current = (year, month, date);
        if let Some(previous) = previous {
            assert!(previous < current, "{previous:?} не раньше {current:?}");
        }
        previous = Some(current);
    }
}

/// Время суток восстанавливается из остатка, а не из даты — проверяется
/// отдельно, потому что предыдущий проход шёл ровно по полуночам.
#[test]
fn time_of_day_survives_the_round_trip() {
    let midnight = DateTime::new(2026, 8, 12, 0, 0, 0).to_unix();
    for second in [0_i64, 1, 59, 60, 3599, 3600, 43_200, 86_399] {
        let restored = DateTime::from_unix(midnight + second);
        assert_eq!(restored.to_unix(), midnight + second, "секунда {second}");
    }
}
