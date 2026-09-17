//! Короткий прогон каждой цели — тот, что гоняется вместе со всеми тестами.
//!
//! # Почему коротко
//!
//! Потому что этот тест платит за себя при **каждом** `cargo xtask check`, а
//! такой проверкой пользуются десятки раз в день. Его задача — не искать новое,
//! а не дать вернуться найденному: зерно и число итераций здесь постоянные, и
//! всё, что однажды падало на них, падает снова.
//!
//! Искать новое — дело долгого прогона: `cargo xtask fuzz --iterations 200000
//! --seed <любое>`. Он же и запускается, когда меняют разборщик.

use std::collections::BTreeSet;

/// Сколько итераций на цель в коротком прогоне.
///
/// Шестьдесят — это секунды на все цели вместе, включая тома ext2 и btrfs, где
/// одна итерация стоит копию двухмегабайтного образа. Число выбрано по времени,
/// а не по желанию: тест, из-за которого проверка идёт минуту, начинают
/// пропускать.
const ITERATIONS: u64 = 60;

/// Зерно короткого прогона. Постоянное, и в этом весь смысл.
const SEED: u64 = 20_260_918;

#[test]
fn no_parser_panics_on_corrupted_input() {
    let mut failed = Vec::new();
    for target in freeos_fuzz::TARGETS {
        if let Some(report) = freeos_fuzz::run(target, SEED, ITERATIONS) {
            failed.push(report.describe());
        }
    }
    assert!(
        failed.is_empty(),
        "разбор чужих байт обязан возвращать ошибку, а не падать:\n\n{}",
        failed.join("\n\n")
    );
}

/// Образцы получаются одинаковыми при каждом запуске.
///
/// Без этого «зерно 7, итерация 912» ничего не воспроизводит: порча
/// детерминирована, а образец — нет, и вход выходит другой. Проверяется
/// сравнением двух построений подряд, потому что источник недетерминизма здесь
/// один и тот же у всех — время, попавшее в образ.
#[test]
fn seeds_are_byte_for_byte_reproducible() {
    for target in freeos_fuzz::TARGETS {
        let first = (target.seeds)();
        let again = (target.seeds)();
        assert_eq!(first, again, "образцы цели {} различаются между запусками", target.name);
    }
}

/// Цели называют разные вещи: два имени, дающие одни и те же образцы и один и
/// тот же вызов, — это опечатка в списке, а не две проверки.
#[test]
fn targets_do_not_repeat_each_other() {
    let names: BTreeSet<&str> = freeos_fuzz::TARGETS.iter().map(|target| target.name).collect();
    assert_eq!(names.len(), freeos_fuzz::TARGETS.len());
    // И каждая цель находится по своему имени — иначе `--target` не сработает.
    for target in freeos_fuzz::TARGETS {
        assert!(freeos_fuzz::target(target.name).is_some(), "цель {} не ищется", target.name);
    }
}
