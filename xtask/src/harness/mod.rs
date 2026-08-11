//! Стенд: прогон системы в QEMU без человека за клавиатурой.
//!
//! # Зачем он в репозитории
//!
//! Правило проекта — «фаза считается сделанной, когда она проверена прогоном, а
//! не осмотром». Прогон до сих пор жил в наборе сценариев PowerShell вне
//! репозитория и переписывался заново каждый раз, когда очередная фаза его
//! требовала. Переписанный стенд проверяет не то же самое, что прошлый, — а
//! значит, зелёный результат ничего не говорит о предыдущих фазах.
//!
//! # Из чего он состоит
//!
//! * [`serial`] — вывод гостя и ввод строк, через сокет;
//! * [`monitor`] — монитор QEMU: нажатия клавиш и снимки экрана;
//! * [`keys`] — раскладка US для `sendkey`;
//! * [`shot`] — перевод снимка в PNG;
//! * [`scenarios`] — собственно проверки.
//!
//! # Чем он лучше прежнего скрипта
//!
//! Тремя вещами, и каждая из них стоила отладочного дня:
//!
//! 1. **Сокеты вместо каналов.** Windows-канал съедал возврат каретки, и путь
//!    «CR как Enter» не проверялся вовсе.
//! 2. **QEMU подключается к нам.** Стенд слушает `127.0.0.1:0` и узнаёт номер
//!    порта у ядра ОС; выбирать порт заранее и надеяться, что он свободен, не
//!    приходится.
//! 3. **Одна командная строка QEMU** — [`crate::qemu::command`] — и у стенда, и
//!    у `run`. Отдельная сборка аргументов означала бы, что проверяется не та
//!    машина, которую видит человек.

mod keys;
mod monitor;
mod scenarios;
mod serial;
mod shot;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::build::{self, BuildOptions};
use crate::paths;
use crate::qemu::{self, Drive, RunOptions};
use crate::{image, util};

pub use scenarios::{Scenario, Step, Target};

/// Сколько ждать, пока QEMU подключится к сокетам стенда.
///
/// Подключение происходит при старте процесса, до всякой прошивки, поэтому
/// десяти секунд хватает с огромным запасом. Если их не хватило — QEMU не
/// запустился, и стенд обязан показать его stderr, а не молча ждать.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Пауза между нажатиями клавиш по умолчанию.
///
/// Двести пятьдесят миллисекунд — это не «на всякий случай». Клавиатура USB
/// опрашивается с интервалом из дескриптора конечной точки, а ядро разбирает
/// кольцо событий раз в квант планировщика; при более частых нажатиях символы
/// теряются, и сценарий падает на проверке введённого текста, хотя система
/// исправна.
const KEY_DELAY: Duration = Duration::from_millis(250);

pub struct TestOptions {
    /// Архитектуры прогона.
    pub arches: Vec<Arch>,
    /// Профили сборки: `false` — debug, `true` — release.
    pub profiles: Vec<bool>,
    /// Прогонять только сценарий с этим именем.
    pub only: Option<String>,
    /// Показывать окно QEMU. По умолчанию прогон идёт вслепую — снимки экрана
    /// от этого не страдают, а всплывающее окно на каждый сценарий мешает.
    pub windowed: bool,
}

/// Результат одного сценария.
struct Outcome {
    scenario: &'static str,
    arch: Arch,
    release: bool,
    error: Option<String>,
    log: PathBuf,
    shots: Vec<PathBuf>,
}

/// Напечатать список сценариев.
pub fn list() {
    println!();
    println!("сценарии стенда:");
    for scenario in scenarios::ALL {
        println!("  {:<12} {}", scenario.name, scenario.about);
        println!("               носитель: {}", scenario.target.title());
    }
    println!();
}

pub fn run(opts: &TestOptions) -> Result<()> {
    let selected: Vec<&Scenario> = match &opts.only {
        Some(name) => scenarios::ALL.iter().filter(|s| s.name == *name).collect(),
        None => scenarios::ALL.iter().collect(),
    };
    if selected.is_empty() {
        let names: Vec<&str> = scenarios::ALL.iter().map(|s| s.name).collect();
        bail!(
            "нет сценария с именем '{}'. Есть: {}",
            opts.only.as_deref().unwrap_or(""),
            names.join(", ")
        );
    }

    let mut outcomes: Vec<Outcome> = Vec::new();

    for &release in &opts.profiles {
        for &arch in &opts.arches {
            for scenario in &selected {
                if !scenario.runs_on(arch) {
                    continue;
                }
                println!();
                println!(
                    "=== {} / {arch} / {} ===",
                    scenario.name,
                    paths::profile_dir_name(release)
                );
                println!("{}", scenario.about);
                outcomes.push(run_scenario(scenario, arch, release, opts.windowed));
            }
        }
    }

    report(&outcomes)
}

fn report(outcomes: &[Outcome]) -> Result<()> {
    println!();
    println!("--- итог ---");
    let mut failed = 0usize;
    for outcome in outcomes {
        let mark = if outcome.error.is_some() { "ПРОВАЛ" } else { "ок    " };
        println!(
            "{mark}  {:<12} {:<8} {:<7}  {}",
            outcome.scenario,
            outcome.arch.name(),
            paths::profile_dir_name(outcome.release),
            outcome.log.display()
        );
        for shot in &outcome.shots {
            println!("        снимок: {}", shot.display());
        }
        if let Some(error) = &outcome.error {
            failed += 1;
            for line in error.lines() {
                println!("        {line}");
            }
        }
    }
    println!();

    if failed > 0 {
        bail!("сценариев провалено: {failed} из {}", outcomes.len());
    }
    println!("все сценарии пройдены: {}", outcomes.len());
    Ok(())
}

/// Прогнать один сценарий. Ошибка сценария не прерывает прогон — она попадает в
/// итог: результат остальных проверок нужен ровно тогда, когда одна упала.
fn run_scenario(scenario: &Scenario, arch: Arch, release: bool, windowed: bool) -> Outcome {
    let prefix = format!(
        "{}-{}-{}",
        scenario.name,
        arch.name(),
        paths::profile_dir_name(release)
    );
    let log = paths::test_dir().join(format!("{prefix}.log"));
    let mut shots = Vec::new();

    let error = match execute(scenario, arch, release, windowed, &prefix, &log, &mut shots) {
        Ok(()) => None,
        Err(err) => Some(format!("{err:#}")),
    };

    Outcome { scenario: scenario.name, arch, release, error, log, shots }
}

fn execute(
    scenario: &Scenario,
    arch: Arch,
    release: bool,
    windowed: bool,
    prefix: &str,
    log_path: &std::path::Path,
    shots: &mut Vec<PathBuf>,
) -> Result<()> {
    let built = build::build_all(&BuildOptions {
        arch,
        release,
        kernel: true,
        initrd: true,
        installer: scenario.target.needs_installer(),
    })?;
    let drives = prepare_drives(scenario.target, &built, arch)?;

    // Слушаем мы, подключается QEMU: порт 0 отдаёт свободный номер, и гонки за
    // фиксированный порт с другим процессом на машине не существует.
    let serial_listener =
        TcpListener::bind("127.0.0.1:0").context("не удалось занять порт под серийную линию")?;
    let monitor_listener =
        TcpListener::bind("127.0.0.1:0").context("не удалось занять порт под монитор")?;
    let serial_addr = serial_listener.local_addr()?;
    let monitor_addr = monitor_listener.local_addr()?;

    let mut extra: Vec<String> = scenario.qemu_args(arch).iter().map(|s| (*s).to_string()).collect();
    extra.extend(scenario.extra.iter().map(|s| (*s).to_string()));

    let opts = RunOptions {
        // Хранилище UEFI-переменных пересоздаётся перед каждым сценарием.
        // Иначе запись загрузки, оставшаяся от прошлого прогона, уводит
        // прошивку по устаревшему пути устройства — она не находит его и падает
        // в собственную оболочку, а стенд видит «система не загрузилась».
        reset_nvram: true,
        serial_only: !windowed,
        drives,
        extra,
        serial: qemu::Serial::Socket(serial_addr),
        monitor: Some(monitor_addr),
        ..RunOptions::default()
    };

    let mut cmd = qemu::command(&opts, &built)?;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    println!("> {}", util::render_command(&cmd));

    let mut child = cmd.spawn().context("не удалось запустить QEMU")?;

    let serial_stream = match monitor::accept_with_timeout(&serial_listener, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(err) => return Err(with_qemu_output(child, err)),
    };
    let mut line = match serial::SerialLine::spawn(serial_stream) {
        Ok(line) => line,
        Err(err) => return Err(with_qemu_output(child, err)),
    };
    let mut hmp = match monitor::Monitor::accept(&monitor_listener, CONNECT_TIMEOUT) {
        Ok(hmp) => hmp,
        Err(err) => return Err(with_qemu_output(child, err)),
    };

    let result = play(scenario, &mut line, &mut hmp, prefix, shots);

    // Гость не выключает машину сам: `arch::halt()` — это остановка процессора,
    // а не выключение. Процесс приходится снимать, и делать это надо в любом
    // случае — иначе провалившийся сценарий оставил бы висеть QEMU.
    child.kill().ok();
    let output = child.wait_with_output().ok();
    let text = line.finish();

    write_log(log_path, &text, output.as_ref());
    println!("журнал: {}", log_path.display());

    result
}

/// Проиграть шаги сценария.
fn play(
    scenario: &Scenario,
    line: &mut serial::SerialLine,
    hmp: &mut monitor::Monitor,
    prefix: &str,
    shots: &mut Vec<PathBuf>,
) -> Result<()> {
    let started = Instant::now();
    for (index, step) in scenario.steps.iter().enumerate() {
        let at = started.elapsed().as_millis();
        match step {
            Step::Await(needle, timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: ждём {needle:?}");
                line.wait_for(needle, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
            }
            Step::Expect(needle) => {
                println!("  [{at:>6} мс] шаг {index}: проверяем {needle:?}");
                if !line.seen(needle) {
                    bail!("шаг {index}: в выводе нет {needle:?}");
                }
            }
            Step::Absent(needle) => {
                println!("  [{at:>6} мс] шаг {index}: проверяем отсутствие {needle:?}");
                if line.seen(needle) {
                    bail!("шаг {index}: в выводе встретилось {needle:?}, чего быть не должно");
                }
            }
            Step::Key(name) => {
                println!("  [{at:>6} мс] шаг {index}: клавиша {name}");
                hmp.sendkey(name).with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Repeat(name, times) => {
                println!("  [{at:>6} мс] шаг {index}: клавиша {name} ×{times}");
                for _ in 0..*times {
                    hmp.sendkey(name).with_context(|| format!("шаг {index}"))?;
                    std::thread::sleep(KEY_DELAY);
                }
            }
            Step::Type(text) => {
                println!("  [{at:>6} мс] шаг {index}: набираем {text:?}");
                for name in keys::spell(text).with_context(|| format!("шаг {index}"))? {
                    hmp.sendkey(&name).with_context(|| format!("шаг {index}"))?;
                    std::thread::sleep(KEY_DELAY);
                }
            }
            Step::Line(text) => {
                println!("  [{at:>6} мс] шаг {index}: в линию {text:?}");
                line.write_line(text).with_context(|| format!("шаг {index}"))?;
            }
            Step::Raw(bytes) => {
                println!("  [{at:>6} мс] шаг {index}: в линию {bytes:02x?}");
                line.write_raw(bytes).with_context(|| format!("шаг {index}"))?;
            }
            Step::Wait(ms) => {
                println!("  [{at:>6} мс] шаг {index}: пауза {ms} мс");
                std::thread::sleep(Duration::from_millis(*ms));
            }
            Step::Shot(name) => {
                println!("  [{at:>6} мс] шаг {index}: снимок '{name}'");
                let ppm = paths::test_dir().join(format!("{prefix}-{name}.ppm"));
                let png = ppm.with_extension("png");
                std::fs::create_dir_all(paths::test_dir()).ok();
                hmp.screendump(&ppm).with_context(|| format!("шаг {index}"))?;
                let (w, h) = shot::ppm_to_png(&ppm, &png).with_context(|| format!("шаг {index}"))?;
                println!("             {w}x{h} -> {}", png.display());
                shots.push(png);
            }
        }
    }
    Ok(())
}

/// Носители машины для сценария.
fn prepare_drives(target: Target, built: &build::Built, arch: Arch) -> Result<Vec<Drive>> {
    let drives = match target {
        Target::Live => vec![Drive::HostDirectory(qemu::prepare_esp(built)?)],
        Target::Image => vec![Drive::Image(image::build(built, image::Kind::System)?)],
        Target::Installer => vec![
            // Порядок важен: прошивка перебирает носители в порядке подключения,
            // и загрузочный раздел на этот момент есть только у первого.
            Drive::Image(image::build(built, image::Kind::Installer)?),
            Drive::Image(image::prepare_target(arch, 1024, true)?),
        ],
        Target::Installed => {
            let disk = paths::target_disk(arch);
            if !disk.is_file() {
                bail!(
                    "нет диска, на который ставил установщик: {}\n\
                     Сценарий 'installed' идёт после 'install' и опирается на его результат.",
                    disk.display()
                );
            }
            vec![Drive::Image(disk)]
        }
    };
    Ok(drives)
}

/// Добавить к ошибке то, что успел сказать сам QEMU.
///
/// Без этого «монитор не подключился» — тупик: причина (неизвестная опция,
/// занятый файл образа, отсутствующая прошивка) остаётся в stderr процесса,
/// который стенд тут же снимает.
fn with_qemu_output(mut child: Child, err: anyhow::Error) -> anyhow::Error {
    child.kill().ok();
    let Ok(output) = child.wait_with_output() else {
        return err;
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut text = String::new();
    if !stderr.trim().is_empty() {
        text.push_str(&format!("\nQEMU stderr:\n{}", stderr.trim()));
    }
    if !stdout.trim().is_empty() {
        text.push_str(&format!("\nQEMU stdout:\n{}", stdout.trim()));
    }
    if text.is_empty() {
        return err;
    }
    err.context(text)
}

fn write_log(path: &std::path::Path, serial: &str, output: Option<&std::process::Output>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut text = String::from("=== серийная линия гостя ===\n");
    text.push_str(serial);
    if let Some(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stdout.trim().is_empty() {
            text.push_str("\n=== stdout QEMU ===\n");
            text.push_str(&stdout);
        }
        if !stderr.trim().is_empty() {
            text.push_str("\n=== stderr QEMU ===\n");
            text.push_str(&stderr);
        }
    }
    std::fs::write(path, text).ok();
}
