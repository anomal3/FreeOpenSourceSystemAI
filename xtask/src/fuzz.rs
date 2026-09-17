//! Долгий прогон фаззера: то же, что делает тест, но столько, сколько скажут.
//!
//! Тест рядом с крейтом `freeos-fuzz` гоняет шестьдесят итераций на цель — он
//! сторож, а не искатель: его задача не пускать назад найденное. Искать новое
//! — здесь: десятки тысяч итераций, и по одной строке на цель, чтобы видно
//! было, что прогон идёт, а не завис.
//!
//! # Куда девается найденный вход
//!
//! В `build/fuzz/<цель>-<зерно>-<итерация>.bin` — целиком, байт в байт. Отчёт
//! называет зерно и итерацию, по которым тот же вход получается снова на любой
//! машине, но файл рядом всё равно нужен: по нему разбираются отладчиком, не
//! дожидаясь, пока фаззер дойдёт до той итерации.

use std::path::PathBuf;

use anyhow::{Result, bail};

use crate::paths;

/// Прогнать фаззер.
///
/// `only` — имя одной цели или `None` для всех; `seed` — зерно; `iterations` —
/// сколько входов на цель.
pub fn run(only: Option<&str>, seed: u64, iterations: u64) -> Result<()> {
    let targets: Vec<&'static freeos_fuzz::Target> = match only {
        Some(name) => match freeos_fuzz::target(name) {
            Some(target) => vec![target],
            None => {
                let known: Vec<&str> =
                    freeos_fuzz::TARGETS.iter().map(|target| target.name).collect();
                bail!("нет такой цели: {name}\nЕсть: {}", known.join(", "));
            }
        },
        None => freeos_fuzz::TARGETS.iter().collect(),
    };

    let out = paths::workspace_root().join("build/fuzz");
    let mut reports = Vec::new();
    for target in targets {
        say!("--- {} ({} итераций, зерно {seed}): {}", target.name, iterations, target.about);
        let started = std::time::Instant::now();
        match freeos_fuzz::run(target, seed, iterations) {
            None => say!(
                "    прошло, {:.1} с ({:.0} входов в секунду)",
                started.elapsed().as_secs_f64(),
                iterations as f64 / started.elapsed().as_secs_f64().max(0.001)
            ),
            Some(report) => {
                let path = save(&out, &report)?;
                say!("    ПАДЕНИЕ\n{}\n    вход сохранён: {}", report.describe(), path.display());
                reports.push(report.target);
            }
        }
    }

    if reports.is_empty() {
        say!("фаззинг: все цели прошли");
        Ok(())
    } else {
        bail!("фаззинг: упало целей — {}: {}", reports.len(), reports.join(", "))
    }
}

/// Сохранить найденный вход рядом с прочими результатами сборки.
fn save(dir: &PathBuf, report: &freeos_fuzz::Report) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}-{}-{}.bin", report.target, report.seed, report.iteration));
    std::fs::write(&path, &report.input)?;
    Ok(path)
}
