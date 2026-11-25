//! `gpu gametest` (20.11.2026): одна команда для утренней проверки игры на
//! видеокарте — Роман тестирует на железе раз в день, и этот раз должен
//! ответить на всё сразу.
//!
//! # Что делает
//!
//! В фоне (задача ядра `gametest`), каждая ступень — строка `gametest:` с
//! PASS/FAIL и временем, всё в журнал ядра (и на флешку, `G:\FREEOS\LOG.TXT`):
//!
//! 1. GSP-RM работает (нет — попытка поднять его, как после `gpu unload`).
//! 2. Мониторы горят (`nvidia_disp::describe`).
//! 3. `gpu nvktest` — копия через CE на своём runlist (жетон звонка!) и
//!    канал GR (golden context, 3D, compute, copy) — веха M2.
//! 4. Узел `/dev/dri/renderD128` настоящей карты (`nvk hw on`).
//! 5. Сценарий `/opt/proton/bin/fossai-gametest` (пакет Proton): vkprobe на
//!    NVK, vulkaninfo, vkcube 600 кадров, Direct3D 9 через DXVK на NVK
//!    (d3dprobe), TMNF через DXVK на NVK с замером к/с. Его вывод идёт через
//!    канал в журнал ядра построчно (`gametest| …`): вывод программ иначе
//!    остаётся в окне терминала и на флешку не попадает (hw-log10: `+fps`
//!    Wine в журнале не было).
//!
//! `gpu gametest force` — дальше ступени 3 даже при FAIL канала GR (смотреть,
//! что скажет NVK); `gpu gametest kernel` — только ступени 1–4; `gpu gametest
//! soft` — вместо карты программная SoftGpu (`nvk soft on`, для QEMU:
//! проверяет сценарий и путь Mesa NVK → DRM → кадры dmabuf без железа).
//! После `--` — аргументы сценария (`vk`, `d3d`, `tmnf`, `all`). Оболочка
//! ждёт конца и печатает ход в окно терминала; `gpu gametest bg` — не ждать.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::kprintln;
use crate::sync::SpinLock;

/// Сценарий пользовательской части (пакет Proton).
const SCRIPT: &str = "/opt/proton/bin/fossai-gametest";
const BASH: &str = "/opt/debian/bin/bash";
/// Дольше этого сценарий не ждём (установка TMNF при первом запуске —
/// пара минут, замер игры — минута, запуск Wine — десятки секунд).
const SCRIPT_LIMIT_MS: u64 = 40 * 60 * 1000;

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Строки для окна терминала: оболочка (`gpu gametest` без `bg`) ждёт
/// проверку и печатает их по мере появления — Роман видит ход на экране.
static ECHO: SpinLock<Vec<String>> = SpinLock::new(Vec::new());

/// Строка проверки: в журнал (и на флешку) и в окно терминала.
fn say(line: String) {
    kprintln!("  {line}");
    let mut echo = ECHO.lock();
    if echo.len() < 4000 {
        echo.push(line);
    }
}

/// Новые строки для терминала.
pub fn drain() -> Vec<String> {
    core::mem::take(&mut *ECHO.lock())
}

/// Идёт ли проверка.
#[must_use]
pub fn running() -> bool {
    RUNNING.load(Ordering::Acquire)
}
/// 0 — всё, 1 — `force`, 2 — `kernel` (без сценария), 3 — `soft`.
static MODE: AtomicU8 = AtomicU8::new(0);
/// `soft amd`: программный узел AMD (RADV) вместо NVIDIA (NVK).
static SOFT_AMD: AtomicBool = AtomicBool::new(false);
/// `hw`: узел AMD на настоящей карте (`amdnode hw on`), а не на программной.
static HW_AMD: AtomicBool = AtomicBool::new(false);
/// Аргументы сценария (`gpu gametest -- …`).
static SCRIPT_ARGS: SpinLock<String> = SpinLock::new(String::new());

/// Запустить проверку в фоне. Возвращает строку для оболочки.
pub fn start(argument: &str) -> String {
    if RUNNING.swap(true, Ordering::AcqRel) {
        return "gametest is already running (lines 'gametest:' in the log)".into();
    }
    ECHO.lock().clear();
    let (ours, script) = argument.split_once("--").map_or((argument, ""), |(a, b)| (a, b.trim()));
    let words: Vec<&str> = ours.split_whitespace().collect();
    // `hw`: настоящая карта AMD (кольцо `amd ring` уже работает), `soft amd`: программная.
    HW_AMD.store(words.contains(&"hw"), Ordering::Relaxed);
    SOFT_AMD.store(words.contains(&"amd") || words.contains(&"hw"), Ordering::Relaxed);
    MODE.store(
        if words.contains(&"soft") || words.contains(&"hw") {
            3
        } else if words.contains(&"kernel") {
            2
        } else if words.contains(&"force") {
            1
        } else {
            0
        },
        Ordering::Relaxed,
    );
    *SCRIPT_ARGS.lock() = String::from(script);
    match crate::sched::spawn_daemon("gametest", task) {
        Ok(id) => format!("gametest started as task {id}; results are the 'gametest:' lines in the log (G:\\FREEOS\\LOG.TXT)"),
        Err(why) => {
            RUNNING.store(false, Ordering::Release);
            format!("gametest could not start: {why:?}")
        }
    }
}

struct Run {
    started: u64,
    passed: Vec<&'static str>,
    failed: Vec<&'static str>,
}

impl Run {
    fn step(&mut self, name: &'static str, ok: bool, t0: u64, detail: &str) -> bool {
        let ms = crate::time::uptime_ms() - t0;
        say(format!("gametest: {} {name} in {ms} ms{}{detail}", if ok { "PASS" } else { "FAIL" }, if detail.is_empty() { "" } else { " - " }));
        if ok {
            self.passed.push(name);
        } else {
            self.failed.push(name);
        }
        crate::usb::stick_log::flush_now();
        ok
    }
}

fn task() {
    let mut run = Run { started: crate::time::uptime_ms(), passed: Vec::new(), failed: Vec::new() };
    let mode = MODE.load(Ordering::Relaxed);
    say(format!("gametest: start ({}), mode {}", crate::version_text(), ["full", "force", "kernel", "soft"][mode as usize % 4]));
    let go_on = if mode == 3 { soft_steps(&mut run) } else { kernel_steps(&mut run, mode) };
    if go_on && mode != 2 {
        script_step(&mut run);
    }
    say(format!(
        "gametest: done in {} s: {} passed {:?}, {} failed {:?}",
        (crate::time::uptime_ms() - run.started) / 1000,
        run.passed.len(),
        run.passed,
        run.failed.len(),
        run.failed
    ));
    crate::usb::stick_log::flush_now();
    RUNNING.store(false, Ordering::Release);
}

/// Узел AMD: на настоящей карте (`hw`) или программный. Узел amdgpu есть только на x86-64.
#[cfg(target_arch = "x86_64")]
fn amd_node(hw: bool) -> bool {
    if hw {
        // Настоящая карта: кольцо и микрокод поднимает цепочка `amd smc start`/`ucode`/`ring`.
        for line in drm::amdgpu::set_hw(true) {
            say(format!("gametest: amd hw: {line}"));
        }
        crate::display::amd::hw::ready()
    } else {
        drm::amdgpu::set_soft(true);
        true
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn amd_node(_hw: bool) -> bool {
    false
}

/// Вместо ступеней 1–4 — программная видеокарта (QEMU).
fn soft_steps(run: &mut Run) -> bool {
    let t = crate::time::uptime_ms();
    let amd = SOFT_AMD.load(Ordering::Relaxed);
    let hw = HW_AMD.load(Ordering::Relaxed);
    let ok = if amd { amd_node(hw) } else { drm::set_soft(true) };
    let node = drm::node_visible();
    let kind = if hw { "AMD on the card" } else if amd { "soft AMD" } else { "SoftGpu" };
    run.step("soft-node", ok && node, t, &format!("{kind} {}, /dev/dri/renderD128 {}", if ok { "on" } else { "not available" }, if node { "present" } else { "absent" }))
}

/// Ступени 1–4. `false` — дальше идти незачем.
fn kernel_steps(run: &mut Run, mode: u8) -> bool {
    // 1. GSP-RM.
    let t = crate::time::uptime_ms();
    if super::nvidia::card().is_none() {
        run.step("gsp", false, t, "no NVIDIA card");
        return false;
    }
    if !super::nvidia_gsp::running() {
        say(format!("gametest: GSP-RM is not running - starting it"));
        let _ = super::nvidia_gsp::restart("gametest");
    }
    if !run.step("gsp", super::nvidia_gsp::running(), t, if super::nvidia_gsp::running() { "GSP-RM running" } else { "GSP-RM did not start (see nvidia-gsp lines)" }) {
        return false;
    }
    // 2. Мониторы.
    let t = crate::time::uptime_ms();
    let disp = super::nvidia_disp::describe();
    let lit = super::nvidia_disp::primary_size().is_some();
    run.step("display", lit, t, &disp);
    // 3. Самопроверки M1/M2 (каналы CE и GR через RM).
    let t = crate::time::uptime_ms();
    let info = super::nvidia_rm_chan::info_report();
    run.step("m1-info", info.iter().any(|l| l.contains("SELFTEST m1 PASS")), t, "");
    let t = crate::time::uptime_ms();
    let test = super::nvidia_rm_chan::nvktest("");
    let ce = test.iter().any(|l| l.contains("SELFTEST m2 ce-copy AsyncCopy PASS"));
    let gr = test.iter().any(|l| l.contains("SELFTEST m2 ce-copy Gr PASS"));
    let rescue = test.iter().find(|l| l.contains("doorbell rescue ->")).cloned().unwrap_or_default();
    run.step("m2-copy-engine", ce, t, &rescue);
    run.step("m2-gr-channel", gr, t, "");
    if !gr && mode != 1 {
        say(format!("gametest: the GR channel does not work - NVK cannot draw; stopping ('gpu gametest force' goes on anyway)"));
        return false;
    }
    // 4. Узел DRM настоящей карты.
    let t = crate::time::uptime_ms();
    drm::set_hw(true);
    let (real, _) = drm::backend_state();
    let node = drm::node_visible();
    run.step("drm-node", node && real, t, &format!("/dev/dri/renderD128 {}, real backend {}", if node { "present" } else { "absent" }, if real { "registered" } else { "none" }))
}

/// Ступень 5: сценарий пакета Proton, вывод — в журнал построчно.
fn script_step(run: &mut Run) {
    let t = crate::time::uptime_ms();
    let (reader, writer) = match crate::user::pipe::create() {
        Ok(ends) => ends,
        Err(_) => {
            run.step("script", false, t, "no memory for a pipe");
            return;
        }
    };
    let args = SCRIPT_ARGS.lock().clone();
    let line = if args.is_empty() { format!("{BASH} {SCRIPT}") } else { format!("{BASH} {SCRIPT} {args}") };
    let cred = crate::user::session::credentials();
    match crate::user::spawn_streams(&line, cred, false, None, Some(writer)) {
        Ok(id) => say(format!("gametest: {line} started as {id}")),
        Err(why) => {
            run.step("script", false, t, &format!("{line}: {why} (is the Proton package installed?)"));
            return;
        }
    }
    let deadline = t + SCRIPT_LIMIT_MS;
    let mut pending: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 512];
    let mut summary = None;
    loop {
        match reader.read(&mut buffer, false) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&buffer[..n]);
                while let Some(end) = pending.iter().position(|b| *b == b'\n') {
                    let text = String::from_utf8_lossy(&pending[..end]).into_owned();
                    pending.drain(..=end);
                    let text = text.trim_end_matches('\r');
                    say(format!("gametest| {text}"));
                    // Первые вызовы NVK к карте — в журнал (vkCreateDevice:
                    // каналы, объекты, первые EXEC): если споткнётся, видно где.
                    if text.starts_with("gametest: run vkprobe") {
                        drm::trace_next(800);
                    } else if text.starts_with("gametest: PASS vkprobe") || text.starts_with("gametest: FAIL vkprobe") {
                        drm::trace_next(0);
                    }
                    if text.starts_with("gametest: summary") {
                        summary = Some(String::from(text));
                    }
                }
                if pending.len() > 4096 {
                    say(format!("gametest| {}", String::from_utf8_lossy(&pending)));
                    pending.clear();
                }
            }
            Err(_) => {
                if crate::time::uptime_ms() > deadline {
                    say(format!("gametest: the script is still running after {} min - not waiting any more", SCRIPT_LIMIT_MS / 60_000));
                    break;
                }
                crate::sched::sleep_ms(100);
            }
        }
        // Сводка пришла, а писатели не закрылись (оставшийся wineserver
        // держит вывод) — ждать дальше незачем.
        if summary.is_some() && !reader.has_data() {
            break;
        }
    }
    if !pending.is_empty() {
        say(format!("gametest| {}", String::from_utf8_lossy(&pending)));
    }
    let ok = summary.as_deref().is_some_and(|s| s.contains(" 0 failed"));
    run.step("script", ok, t, summary.as_deref().unwrap_or("no summary line from the script"));
}

/// Слой DRM живёт в `crate::linux`, который собирается только на x86_64.
#[cfg(target_arch = "x86_64")]
use crate::linux::drm;

/// На aarch64 слоя Linux нет: узла DRM нет, ступени честно проваливаются.
#[cfg(not(target_arch = "x86_64"))]
mod drm {
    pub fn set_soft(_on: bool) -> bool {
        false
    }
    pub fn set_hw(_on: bool) {}
    pub fn backend_state() -> (bool, bool) {
        (false, false)
    }
    pub fn node_visible() -> bool {
        false
    }
    pub fn trace_next(_ioctls: u64) {}
}
