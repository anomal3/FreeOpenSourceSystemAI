//! Прогон проверок по маркеру `RADV.TXT` с уведомлениями на столе.

use super::*;

/// Сообщить и в журнал (латиницей - его читают с флешки), и уведомлением на столе.
pub(super) fn announce(title: &str, text: &str, ms: u64) {
    crate::kprintln!("==== AMD TEST: {title} - {text} ====");
    crate::ui::system_notice(title, text, ms);
}

/// Прогнать `gpu gametest hw -- {what}` и дождаться; `Some(true)` - всё прошло, `None` - не дождались.
pub(super) fn run_gametest(what: &str, limit_s: u32) -> Option<bool> {
    let _ = super::super::nvidia_gametest::drain();
    crate::kprintln!("  amdgpu     : radv: {}", super::super::nvidia_gametest::start(&format!("hw amd -- {what}")));
    let mut ticks = 0u32;
    let (mut last_commits, mut shot) = (wayland_commits(), false);
    while super::super::nvidia_gametest::running() {
        crate::sched::sleep_ms(1000);
        ticks += 1;
        // Один снимок экрана на шаг: когда пошли кадры.
        let commits = wayland_commits();
        let busy = commits.wrapping_sub(last_commits) >= 5 && ticks >= 3;
        if !shot && busy {
            shots::burst(what);
            shot = true;
        }
        last_commits = commits;
        if ticks % 30 == 0 {
            crate::kprintln!("  amdgpu     : radv: '{what}' still running ({ticks} s of up to {limit_s})");
        }
        if ticks > limit_s {
            return None;
        }
    }
    let lines = super::super::nvidia_gametest::drain();
    Some(lines.iter().any(|line| line.contains("gametest: done in") && line.contains(", 0 failed")))
}

/// Ждать, пока флешка поставит пакеты в память, и пройти проверки: Vulkan, скорость Direct3D 9 (DXVK) и игра.
/// Остальное (Direct3D 11/12, VLC, OBS, 7-Zip) проверено на RX 590 (0.8.37) и из прогона убрано, чтобы он не тянулся;
/// запускается вручную: `gpu gametest hw amd -- d3d11|d3d12|apps`. Каждый шаг - уведомлением на столе и строками `gametest|` в `LOG.TXT`.
pub(super) fn radv_auto() {
    // Тест идёт долго и без мыши: гашение экрана прятало бы ход и итог, а снимки выходили чёрными.
    crate::power::inhibit(true);
    let present = |path: &str| matches!(crate::fs::list(path), Some(Ok(_)));
    // До 25 минут: пакеты с флешки - гигабайты.
    let started = crate::time::uptime_ms();
    let mut last_line = 0u64;
    loop {
        let waited = (crate::time::uptime_ms() - started) / 1000;
        let ready = present("/opt/proton/bin") && !crate::usb::storage::live_busy();
        if ready || waited > 1500 {
            break;
        }
        if waited >= last_line + 30 || last_line == 0 {
            last_line = waited.max(1);
            announce("Копируются программы", &format!("Прошло {waited} с из 2-8 мин. Не выключайте."), 40_000);
        }
        crate::sched::sleep_ms(2000);
    }
    if !present("/opt/proton/bin") {
        announce("Тест AMD остановлен", "Нет Proton в /opt. Можно выключать.", 1_800_000);
        crate::usb::stick_log::flush_now();
        return;
    }
    // Названия короткие: карточка уведомления узкая, длинное обрезается.
    let mut steps: Vec<(&str, &str, u32)> = alloc::vec![
        ("vk", "Vulkan", 600),
        // Первым запуском Wine создаётся префикс.
        ("vsync", "скорость", 900),
    ];
    if present("/opt/tmnf/bin") {
        steps.push(("tmnf", "Trackmania", 1900));
    }
    let total = steps.len();
    let mut results: Vec<String> = Vec::new();
    let mut failed = 0u32;
    let mut record = |name: &str, verdict: Option<bool>, results: &mut Vec<String>| {
        if verdict != Some(true) {
            failed += 1;
        }
        results.push(format!("{name} {}", match verdict {
            Some(true) => "PASS",
            Some(false) => "FAIL",
            None => "TIMEOUT",
        }));
    };
    for (number, (what, title, limit)) in steps.into_iter().enumerate() {
        let previous = results.last().map_or(String::new(), |last| format!(" Было: {last}"));
        announce(&format!("Шаг {}/{total}: {title}", number + 1), &format!("Не выключайте.{previous}"), 600_000);
        let verdict = run_gametest(what, limit);
        record(what, verdict, &mut results);
        crate::usb::stick_log::flush_now();
        // Прежняя проверка не закончилась - новую не запускать поверх.
        if super::super::nvidia_gametest::running() {
            break;
        }
    }
    crate::kprintln!("==== AMD TEST: summary: {} ====", results.join(", "));
    let bad: Vec<&str> = results.iter().filter(|line| !line.ends_with(" PASS")).map(String::as_str).collect();
    let text = if failed == 0 { String::from("Всё PASS. Можно выключать.") } else { format!("Выключайте. {}", bad.join(", ")) };
    announce(if failed == 0 { "Тест AMD пройден" } else { "Тест AMD: есть ошибки" }, &text, 1_800_000);
    crate::usb::stick_log::flush_now();
}

/// Сколько кадров показал сервер Wayland с загрузки.
pub(super) fn wayland_commits() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        crate::linux::perfdiag::WL_COMMITS.load(core::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}
