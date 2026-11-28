//! Автозапуск драйвера при загрузке.

use super::*;

// --- Auto start of the SMC (no keyboard needed) -----------------------------
//
// The USB keyboard of the live stick is not always recognised at boot (19-21.11),
// so `amd smc start` can not always be typed. A stick built with
// FOSSAI_AMD_SMC_START=1 carries the marker FREEOS/AMD/START.TXT: this task waits
// for the stick, and only if the marker is there runs the same start as the
// command. Without the marker nothing is written to the card.

/// True if a Polaris card was found and its register window is mapped.
#[must_use]
pub fn probed() -> bool {
    POLARIS_MMIO.lock().is_some()
}

/// Автозапуск драйвера при обычной загрузке (25.11): прошивки с флешки (`FREEOS/AMD/*.BIN`) -> SMC ->
/// микрокод -> кольцо GFX -> GART -> узел DRM на карте (`/dev/dri/renderD128`, RADV). Никакие маркеры для этого
/// не нужны; файл `FREEOS/AMD/NOAUTO.TXT` отключает запуск. Остальные маркеры добавляют проверки:
/// `HW.TXT` - самотест железа, `RADV.TXT` - прогон RADV, игр и программ (`radv_auto`); ход виден уведомлениями стола.
/// `SYNC.TXT` - ждать GPU в каждой отправке, как до асинхронной очереди (для сравнения и отладки).
/// Если ступень не удалась, дальше не идём: экран и система работают без видеокарты.
pub fn auto_task() {
    if super::super::firmware_only() {
        crate::kprintln!("  amdgpu     : auto start skipped - firmware display asked in the boot menu");
        return;
    }
    // The stick appears some seconds after the kernel starts.
    let deadline = crate::time::uptime_ms() + 90_000;
    while firmware_file("SMC.BIN").is_err() {
        if crate::time::uptime_ms() > deadline {
            crate::kprintln!("  amdgpu     : auto start: no FREEOS/AMD/SMC.BIN found in 90 s - the card stays with the firmware picture");
            return;
        }
        crate::sched::sleep_ms(1000);
    }
    if firmware_file("NOAUTO.TXT").is_ok() {
        crate::kprintln!("  amdgpu     : auto start is off (FREEOS/AMD/NOAUTO.TXT); 'amd smc start' by hand");
        return;
    }
    let testing = ["HW.TXT", "RADV.TXT"].iter().any(|name| firmware_file(name).is_ok());
    #[cfg(target_arch = "x86_64")]
    crate::linux::drm::amdgpu::set_perf_log(testing);
    if testing {
        crate::kprintln!("==== AMD TEST: started. Stages: SMC, microcode, ring, GART, then the checks asked for by the markers. ====");
    }
    let has = |lines: &[String], what: &str| lines.iter().any(|line| line.contains(what));
    let failed = |stage: &str| {
        crate::kprintln!("  amdgpu     : auto start STOPPED at {stage}: see the lines above; the system keeps the firmware picture");
        crate::usb::stick_log::flush_now();
    };
    let _ = polaris_smc_start();
    let smc = polaris_smc();
    for line in &smc {
        crate::kprintln!("  amdgpu     : auto start: smc: {line}");
    }
    if !has(&smc, "SMC is RUNNING") {
        return failed("SMC");
    }
    if !has(&polaris_ucode(), "ALL loaded") {
        return failed("microcode");
    }
    if !has(&polaris_ring(), "TEST 2 PASSED") {
        return failed("the GFX ring");
    }
    if !has(&polaris_gart(), "TEST PASSED") {
        return failed("GART");
    }
    // Самотест по маркеру.
    if firmware_file("HW.TXT").is_ok() {
        let lines = hw::selftest();
        let verdict = |what: &str| lines.iter().find(|l| l.contains(what)).map_or("no result", |l| if l.contains("PASSED") { "PASSED" } else { "FAILED" });
        let checks = ["HW RESULT", "HW SHADER RESULT", "HW VRAM RESULT", "HW ASYNC RESULT"];
        crate::kprintln!(
            "==== AMD TEST: hw backend: {}, shader in VMID 1: {}, VRAM buffers: {}, queued submissions: {} ====",
            verdict(checks[0]),
            verdict(checks[1]),
            verdict(checks[2]),
            verdict(checks[3])
        );
        let all = checks.iter().all(|what| verdict(what) == "PASSED");
        announce("Тест AMD начался", if all { "Железо PASSED. Не выключайте." } else { "Железо: есть FAILED. Не выключайте." }, 120_000);
        if let Some(speed) = lines.iter().find(|l| l.contains("HW VRAM SPEED")) {
            crate::kprintln!("==== AMD TEST: {speed} ====");
        }
        crate::usb::stick_log::flush_now();
    }
    #[cfg(target_arch = "x86_64")]
    if firmware_file("SYNC.TXT").is_ok() {
        crate::linux::drm::amdgpu::set_async(false);
    }
    // Узел DRM на карте - то, ради чего всё: RADV и DXVK берут его сами.
    if !driver_ready() {
        return failed("the DRM backend (VMID 1)");
    }
    crate::usb::stick_log::flush_now();
    if firmware_file("RADV.TXT").is_ok() {
        crate::kprintln!("  amdgpu     : auto start: RADV.TXT found, waiting for Proton and running RADV on the card");
        radv_auto();
    }
}

/// Включить узел amdgpu на карте; `false` - не вышло.
pub(super) fn driver_ready() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        for line in crate::linux::drm::amdgpu::set_hw(true) {
            crate::kprintln!("  amdgpu     : {line}");
        }
        if hw::ready() {
            crate::kprintln!("  amdgpu     : READY - /dev/dri/renderD128 is the card; Vulkan programs use RADV, Windows games DXVK");
            return true;
        }
        false
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}
