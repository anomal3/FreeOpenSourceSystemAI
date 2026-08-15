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

mod aim;
mod keys;
mod monitor;
mod qmp;
mod scenarios;
mod serial;
mod shot;
mod sshkeys;

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::build::{self, BuildOptions};
use crate::paths;
use crate::qemu::{self, Drive, Pointer, RunOptions, UsbController};
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

/// Пауза между отчётами мыши.
///
/// Короче клавиатурной: движение — это серия отчётов, и пауза в четверть
/// секунды на каждый превратила бы проезд курсора через экран в полминуты.
/// Терять отчёты не страшно так, как терять символы: очередь указателя при
/// переполнении складывает приращения, а не выбрасывает их.
const POINTER_DELAY: Duration = Duration::from_millis(60);

/// Наибольшее приращение в одном отчёте boot-протокола мыши.
///
/// Байт на ось, знаковый. Взято с запасом от 127: пограничное значение не даёт
/// ничего, кроме шанса ошибиться на единицу.
const POINTER_STEP: i32 = 100;

/// Разбить перемещение на отчёты, помещающиеся в байт.
fn split_move(dx: i32, dy: i32) -> Vec<(i32, i32)> {
    let steps = (dx.abs().max(dy.abs()) + POINTER_STEP - 1) / POINTER_STEP;
    let steps = steps.max(1);
    let mut out = Vec::with_capacity(steps as usize);
    let (mut left_x, mut left_y) = (dx, dy);
    for index in 0..steps {
        let remaining = steps - index;
        // Деление с округлением к нулю плюс вычитание остатка: сумма шагов
        // обязана в точности равняться заказанному перемещению, иначе курсор
        // приезжает не туда, куда его звали.
        let step_x = left_x / remaining;
        let step_y = left_y / remaining;
        left_x -= step_x;
        left_y -= step_y;
        out.push((step_x, step_y));
    }
    out
}

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
    let drives = prepare_drives(scenario, &built, arch)?;

    // Слушаем мы, подключается QEMU: порт 0 отдаёт свободный номер, и гонки за
    // фиксированный порт с другим процессом на машине не существует.
    let serial_listener =
        TcpListener::bind("127.0.0.1:0").context("не удалось занять порт под серийную линию")?;
    let monitor_listener =
        TcpListener::bind("127.0.0.1:0").context("не удалось занять порт под монитор")?;
    let serial_addr = serial_listener.local_addr()?;
    let monitor_addr = monitor_listener.local_addr()?;

    // Третий сокет открывается только там, где он нужен. Лишний порт на каждый
    // прогон — это лишний способ отказать, а сценариям с мышью QMP не нужен
    // вовсе: приращения умеет и HMP.
    let qmp_listener = match scenario.tablet {
        true => Some(TcpListener::bind("127.0.0.1:0").context("не удалось занять порт под QMP")?),
        false => None,
    };
    let qmp_addr = match &qmp_listener {
        Some(listener) => Some(listener.local_addr()?),
        None => None,
    };

    // Порт под проброс выбирается тем же приёмом, что и остальные: ядро
    // операционной системы выдаёт свободный номер, сокет тут же закрывается, и
    // номер уезжает в командную строку QEMU. Окно между закрытием и запуском
    // теоретически даёт гонку с чужим процессом, но выбирать номер заранее — это
    // гонка гарантированная, а не теоретическая.
    let hostfwd = match scenario.guest_port {
        0 => None,
        guest => {
            let probe = TcpListener::bind("127.0.0.1:0")
                .context("не удалось занять порт под проброс")?;
            let port = probe.local_addr()?.port();
            drop(probe);
            Some((port, guest))
        }
    };

    // Эхо-сервер хоста поднимается до запуска гостя: гость подключается к нему
    // в первые же секунды, и сервер, поднятый позже, встретил бы его отказом.
    // Слушающий сокет живёт до конца сценария и закрывается вместе с ним.
    let _host_echo = if scenario.host_echo {
        Some(start_host_echo()?)
    } else {
        None
    };

    // Сервер обновлений — тем же приёмом и по той же причине: гость идёт к нему
    // сам, в первые же секунды после того, как поднялась сеть.
    let _host_repo = if scenario.host_repo {
        Some(start_host_repo(repo_dir(arch))?)
    } else {
        None
    };

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
        qmp: qmp_addr,
        pointer: if scenario.tablet { Pointer::Tablet } else { Pointer::Mouse },
        usb: if scenario.ohci { UsbController::Ohci } else { UsbController::Xhci },
        disk_bus: scenario.disk_bus,
        network: scenario.network,
        hostfwd,
        allow_reboot: scenario.reboots,
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
    let mut qmp = match &qmp_listener {
        Some(listener) => match qmp::Qmp::accept(listener, CONNECT_TIMEOUT) {
            Ok(qmp) => Some(qmp),
            Err(err) => return Err(with_qemu_output(child, err)),
        },
        None => None,
    };

    let result = play(
        scenario,
        &mut child,
        &mut line,
        &mut hmp,
        qmp.as_mut(),
        prefix,
        shots,
        hostfwd.map(|(host, _)| host),
    );

    // Обычно гость машину не выключает: `arch::halt()` — это остановка
    // процессора, а не снятие питания, и процесс приходится снимать. С фазы 27
    // есть и второй случай — сценарий, в котором гость гаснет сам; там к этому
    // моменту снимать уже некого, и `kill` просто не находит процесса. Звать его
    // надо всё равно: провалившийся сценарий иначе оставил бы висеть QEMU.
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
    child: &mut Child,
    line: &mut serial::SerialLine,
    hmp: &mut monitor::Monitor,
    mut qmp: Option<&mut qmp::Qmp>,
    prefix: &str,
    shots: &mut Vec<PathBuf>,
    // Порт на хосте, проброшенный в гостя, — если сценарий его просил.
    hostfwd: Option<u16>,
) -> Result<()> {
    let started = Instant::now();
    // Где сейчас указатель. Мышь относительная, абсолютных координат у неё нет,
    // поэтому «навести на цель» — это посчитать разницу и проехать её. Начальное
    // положение — середина экрана: так его ставит ядро.
    //
    // У планшета положение известно и без счёта, но храним его всё равно:
    // [`Step::Move`] задан приращением («провезти окно на столько»), и от чего
    // это приращение считать, знает только стенд.
    let mut pointer: Option<(i32, i32)> = None;

    // Число, захваченное `Step::Capture`, — например номер задачи, который
    // система назвала сама. Подставляется вместо `{}` в текст последующих шагов.
    let mut captured: Option<String> = None;
    let fill = |text: &str, captured: &Option<String>| -> String {
        match captured {
            Some(value) => text.replace("{}", value),
            None => text.to_string(),
        }
    };

    for (index, step) in scenario.steps.iter().enumerate() {
        let at = started.elapsed().as_millis();
        match step {
            Step::Await(needle, timeout_ms) => {
                let needle = fill(needle, &captured);
                println!("  [{at:>6} мс] шаг {index}: ждём {needle:?}");
                line.wait_for(&needle, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
            }
            Step::AwaitAny(needle, timeout_ms) => {
                let needle = fill(needle, &captured);
                println!("  [{at:>6} мс] шаг {index}: ждём {needle:?} где угодно в выводе");
                line.wait_seen(&needle, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
            }
            Step::Capture(prefix, timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: ждём {prefix:?} и запоминаем число за ним");
                let value = line
                    .capture_number(prefix, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                println!("  [{at:>6} мс] шаг {index}: запомнено {value:?}");
                captured = Some(value);
            }
            Step::Clock(prefix, tolerance_s, timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: сверяем часы гостя с часами хоста");
                let value = line
                    .capture_number(prefix, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let guest: i64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число секунд"))?;
                let host = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("часы хоста до 1970 года")?
                    .as_secs() as i64;
                let drift = guest - host;
                println!(
                    "             гость {guest} с, хост {host} с, расхождение {drift} с (допуск ±{tolerance_s})"
                );
                if drift.unsigned_abs() > *tolerance_s {
                    bail!(
                        "шаг {index}: часы гостя разошлись с часами хоста на {drift} с \
                         (гость {guest}, хост {host}); допуск ±{tolerance_s} с. \
                         Расхождение, кратное часу, — это ошибка в часовом поясе, а не в часах"
                    );
                }
            }
            Step::AtMost(prefix, limit, timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: ждём число за {prefix:?}, не больше {limit}");
                let value = line
                    .capture_number(prefix, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let number: u64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число"))?;
                println!("             получено {number}");
                if number > *limit {
                    bail!("шаг {index}: за {prefix:?} стоит {number}, а предел {limit}");
                }
            }
            Step::AtLeast(prefix, limit, timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: ждём число за {prefix:?}, не меньше {limit}");
                let value = line
                    .capture_number(prefix, Duration::from_millis(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let number: u64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число"))?;
                println!("             получено {number}");
                if number < *limit {
                    bail!("шаг {index}: за {prefix:?} стоит {number}, а нужно не меньше {limit}");
                }
            }
            Step::Expect(needle) => {
                let needle = fill(needle, &captured);
                println!("  [{at:>6} мс] шаг {index}: проверяем {needle:?}");
                if !line.seen(&needle) {
                    bail!("шаг {index}: в выводе нет {needle:?}");
                }
            }
            Step::Absent(needle) => {
                let needle = fill(needle, &captured);
                println!("  [{at:>6} мс] шаг {index}: проверяем отсутствие {needle:?}");
                if line.seen(&needle) {
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
                let text = fill(text, &captured);
                println!("  [{at:>6} мс] шаг {index}: в линию {text:?}");
                line.write_line(&text).with_context(|| format!("шаг {index}"))?;
            }
            Step::Raw(bytes) => {
                println!("  [{at:>6} мс] шаг {index}: в линию {bytes:02x?}");
                line.write_raw(bytes).with_context(|| format!("шаг {index}"))?;
            }
            Step::Aim(target) => {
                let log = line.text();
                let (width, height) = aim::screen(&log).with_context(|| format!("шаг {index}"))?;
                let from = *pointer.get_or_insert((width / 2, height / 2));
                let to = aim::resolve(*target, &log).with_context(|| format!("шаг {index}"))?;
                println!(
                    "  [{at:>6} мс] шаг {index}: наводим на {target:?} — {:?} -> {to:?}",
                    from
                );
                move_pointer(qmp.as_deref_mut(), hmp, from, to, width, height)
                    .with_context(|| format!("шаг {index}"))?;
                pointer = Some(to);
                // Гость разбирает отчёты в своём такте: не дав ему догнать,
                // следующий щелчок пришёлся бы по старому положению курсора.
                std::thread::sleep(KEY_DELAY);
            }
            Step::Move(dx, dy) => {
                println!("  [{at:>6} мс] шаг {index}: указатель на {dx},{dy}");
                let log = line.text();
                let (width, height) = aim::screen(&log).with_context(|| format!("шаг {index}"))?;
                let from = *pointer.get_or_insert((width / 2, height / 2));
                let to = (from.0 + dx, from.1 + dy);
                move_pointer(qmp.as_deref_mut(), hmp, from, to, width, height)
                    .with_context(|| format!("шаг {index}"))?;
                pointer = Some(to);
            }
            Step::Click => {
                println!("  [{at:>6} мс] шаг {index}: щелчок");
                press_button(qmp.as_deref_mut(), hmp, true)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(POINTER_DELAY);
                press_button(qmp.as_deref_mut(), hmp, false)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Press => {
                println!("  [{at:>6} мс] шаг {index}: кнопка нажата");
                press_button(qmp.as_deref_mut(), hmp, true)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Release => {
                println!("  [{at:>6} мс] шаг {index}: кнопка отпущена");
                press_button(qmp.as_deref_mut(), hmp, false)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Plug(spec) => {
                println!("  [{at:>6} мс] шаг {index}: подключаем {spec}");
                hmp.device_add(spec).with_context(|| format!("шаг {index}"))?;
            }
            Step::Unplug(id) => {
                println!("  [{at:>6} мс] шаг {index}: выдёргиваем {id}");
                hmp.device_del(id).with_context(|| format!("шаг {index}"))?;
            }
            Step::Wait(ms) => {
                println!("  [{at:>6} мс] шаг {index}: пауза {ms} мс");
                std::thread::sleep(Duration::from_millis(*ms));
            }
            Step::Reset => {
                println!("  [{at:>6} мс] шаг {index}: сброс машины без предупреждения гостя");
                hmp.system_reset().with_context(|| format!("шаг {index}"))?;
            }
            Step::PowerButton => {
                println!("  [{at:>6} мс] шаг {index}: кнопка питания");
                hmp.system_powerdown().with_context(|| format!("шаг {index}"))?;
            }
            Step::Exits(timeout_ms) => {
                println!("  [{at:>6} мс] шаг {index}: ждём, что QEMU завершится сам");
                let deadline = Instant::now() + Duration::from_millis(*timeout_ms);
                loop {
                    match child.try_wait().with_context(|| format!("шаг {index}"))? {
                        Some(status) => {
                            println!("             QEMU завершился сам: {status}");
                            // Ненулевой код — не «выключилось как-то не так», а
                            // отказ эмулятора: гость, снявший питание, всегда
                            // даёт ноль. Разница важна, потому что первое
                            // означало бы работающее выключение.
                            if !status.success() {
                                bail!(
                                    "шаг {index}: QEMU завершился с {status}, \
                                     а выключение гостя даёт нулевой код"
                                );
                            }
                            break;
                        }
                        None if Instant::now() >= deadline => bail!(
                            "шаг {index}: QEMU всё ещё работает через {timeout_ms} мс — \
                             машина не выключилась"
                        ),
                        None => std::thread::sleep(Duration::from_millis(100)),
                    }
                }
                // Последним словам гостя надо дать доехать: питание снято, но
                // байты ещё в сокете, а следующие шаги (`Absent`, `Expect`)
                // читают именно то, что стенд успел принять.
                std::thread::sleep(Duration::from_millis(300));
            }
            Step::TcpEcho(text, timeout_ms) => {
                let Some(port) = hostfwd else {
                    bail!("шаг {index}: сценарий не пробрасывает порт, стучаться некуда");
                };
                println!("  [{at:>6} мс] шаг {index}: с хоста на 127.0.0.1:{port} — \"{text}\"");
                let echoed = tcp_echo(port, text.as_bytes(), *timeout_ms)
                    .with_context(|| format!("шаг {index}"))?;
                if echoed != text.as_bytes() {
                    bail!(
                        "шаг {index}: вернулось не то: {:?}",
                        String::from_utf8_lossy(&echoed)
                    );
                }
                println!("             вернулось {} байт, совпало", echoed.len());
            }
            Step::TcpBulk(kilobytes, timeout_ms) => {
                let Some(port) = hostfwd else {
                    bail!("шаг {index}: сценарий не пробрасывает порт, стучаться некуда");
                };
                println!("  [{at:>6} мс] шаг {index}: {kilobytes} КиБ туда и обратно");
                // Узор, а не нули: одинаковые байты сошлись бы и при
                // перепутанном порядке сегментов, а такой — нет.
                let payload: Vec<u8> = (0..kilobytes * 1024)
                    .map(|i| (i % 251) as u8)
                    .collect();
                let echoed = tcp_echo(port, &payload, *timeout_ms)
                    .with_context(|| format!("шаг {index}"))?;
                if echoed.len() != payload.len() {
                    bail!(
                        "шаг {index}: отправлено {} байт, вернулось {}",
                        payload.len(),
                        echoed.len()
                    );
                }
                if let Some(at) = echoed.iter().zip(&payload).position(|(a, b)| a != b) {
                    bail!("шаг {index}: байт {at} вернулся изменённым");
                }
                println!("             {} байт совпали до последнего", echoed.len());
            }
            Step::Ssh(run) => {
                let Some(port) = hostfwd else {
                    bail!("шаг {index}: сценарий не пробрасывает порт, стучаться некуда");
                };
                let what = match run.command {
                    "" => "оболочка",
                    command => command,
                };
                println!("  [{at:>6} мс] шаг {index}: ssh -v на 127.0.0.1:{port} — {what}");
                let output = run_ssh(port, run, prefix, index)
                    .with_context(|| format!("шаг {index}"))?;
                for line in output.lines().filter(|line| {
                    // В отчёт попадает только то, что говорит о протоколе и о
                    // входе: полный вывод `ssh -v` — это сотня строк про файлы
                    // настроек, которых на этой машине нет.
                    line.contains("kex")
                        || line.contains("host key")
                        || line.contains("Authentications")
                        || line.contains("Server accepts")
                        || line.contains("Permission denied")
                        || line.contains("Authenticated")
                        || line.starts_with("debug1: Remote protocol")
                }) {
                    println!("             {}", line.trim());
                }
                for needle in run.expect {
                    if !output.contains(needle) {
                        bail!("шаг {index}: в выводе ssh нет \"{needle}\"");
                    }
                }
                for needle in run.absent {
                    if output.contains(needle) {
                        bail!("шаг {index}: в выводе ssh есть \"{needle}\", а его быть не должно");
                    }
                }
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

/// Позвать штатный `ssh` и вернуть всё, что он написал.
///
/// Проверка знания хоста выключена намеренно и двумя способами сразу: ключ
/// хоста у гостя новый на каждом прогоне, а файл известных хостов уводится в
/// никуда. Иначе второй прогон встречал бы предупреждение о подмене ключа и
/// отказ подключаться — то есть проверял бы аккуратность клиента, а не нас.
fn run_ssh(port: u16, run: &scenarios::SshRun, prefix: &str, index: usize) -> Result<String> {
    use std::process::Command;

    let timeout_ms = run.timeout_ms;
    let known_hosts = paths::test_dir().join("ssh-known-hosts");
    std::fs::create_dir_all(paths::test_dir()).ok();
    // Файл известных хостов пересоздаётся пустым: ключ гостя меняется от
    // прогона к прогону, и запись от прошлого раза — это гарантированный отказ.
    std::fs::write(&known_hosts, b"").ok();

    let mut cmd = Command::new("ssh");
    cmd.arg("-v")
        .arg("-p")
        .arg(port.to_string())
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
        // Пароль спрашивать некому: прогон идёт без человека, а клиент, дойдя
        // до запроса пароля, повис бы до таймаута.
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg(format!("ConnectTimeout={}", (timeout_ms / 1000).max(5)))
        .arg("-o")
        .arg("PreferredAuthentications=publickey")
        // Ни агента, ни ключей «по умолчанию». Иначе прогон зависел бы от того,
        // какие ключи лежат у разработчика в `~/.ssh`, — то есть у одного шёл
        // бы иначе, чем у другого.
        .arg("-o")
        .arg("IdentityAgent=none")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        // Псевдотерминал не запрашивается: его здесь нет, и запрос кончился бы
        // строкой про неудавшийся pty в каждом прогоне.
        .arg("-T");
    match run.identity {
        scenarios::Identity::None => {}
        scenarios::Identity::Authorized => {
            cmd.arg("-i").arg(sshkeys::authorized()?);
        }
        scenarios::Identity::Stranger => {
            cmd.arg("-i").arg(sshkeys::stranger()?);
        }
    }
    cmd.arg(format!("{}@127.0.0.1", sshkeys::ACCOUNT));
    if !run.command.is_empty() {
        cmd.arg(run.command);
    }
    // Ввод подаётся каналом, а не с терминала: терминала у прогона нет вовсе, и
    // клиент, увидев его отсутствие, сам не стал бы просить псевдотерминал.
    cmd.stdin(if run.stdin.is_empty() {
        Stdio::null()
    } else {
        Stdio::piped()
    });
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    // Запуск с ожиданием по часам, а не `output()`. У `ssh` нет предела на
    // время рукопожатия: сервер, замолчавший после согласования алгоритмов,
    // держит клиента бесконечно, и стенд вместе с ним. Ровно это и случилось
    // на первом же прогоне — прогон встал насмерть, а причина была в госте.
    let mut child = cmd
        .spawn()
        .context("не удалось запустить ssh; он нужен для этой проверки")?;

    // Ввод отдаётся сразу и целиком, после чего канал закрывается. Закрытие —
    // половина проверки: оно доезжает до гостя как `CHANNEL_EOF`, и построчный
    // сеанс обязан на нём закончиться, как заканчивается всякая оболочка на
    // `Ctrl-D`. Клиент, у которого вход остался открытым, ждал бы до таймаута.
    if !run.stdin.is_empty() {
        use std::io::Write as _;
        let mut input = child
            .stdin
            .take()
            .context("у ssh не оказалось входного канала")?;
        input
            .write_all(run.stdin.as_bytes())
            .context("не удалось отдать ssh его ввод")?;
    }

    // Журнал у каждого запуска свой: в одном сценарии их несколько, и общий
    // файл сохранил бы только последний — то есть как раз не тот, на котором
    // сценарий упал.
    let log = paths::test_dir().join(format!("{prefix}-ssh-{index}.log"));
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                child.kill().ok();
                // Даже у зависшего клиента есть что рассказать: он успел
                // напечатать всё, что понял до молчания, и именно эти строки
                // объясняют, чего он ждал.
                let output = child.wait_with_output()?;
                let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stdout));
                std::fs::create_dir_all(paths::test_dir()).ok();
                std::fs::write(&log, text.as_bytes()).ok();
                bail!(
                    "ssh не ответил за {timeout_ms} мс — гость молчит после рукопожатия;                      что успел сказать клиент: {}",
                    log.display()
                );
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    let output = child.wait_with_output()?;
    // Оба потока вместе: `ssh -v` пишет диагностику в поток ошибок, а вывод
    // удалённой команды — в обычный, и проверять приходится и то и другое.
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stdout));

    // Полный вывод клиента ложится рядом с журналом гостя. Он и есть вторая
    // половина картины: гость рассказывает, что делал он, а `ssh -v` — что из
    // этого понял тот, кто с ним разговаривал.
    std::fs::write(&log, text.as_bytes()).ok();
    println!("             вывод ssh: {}", log.display());

    Ok(text)
}

/// Порт, на котором стенд поднимает эхо-сервер для гостя.
///
/// Фиксированный, а не выданный ядром ОС: адрес и порт записаны строкой в
/// команде сценария (`echoc 10.0.2.2 2001 ...`), и подставить туда случайное
/// число нечем. Занятый порт даст внятный отказ при запуске, а не загадочное
/// поведение в середине прогона.
const HOST_ECHO_PORT: u16 = 2001;

/// Порт, на котором стенд раздаёт репозиторий обновлений.
///
/// Фиксированный, как и у эха, и по той же причине: адрес уезжает в файл
/// настроек гостя, который пишется **до** запуска, — подставить туда случайный
/// номер было бы можно, но тогда файл менялся бы от прогона к прогону, а
/// `ext2::Editor` перезаписывать не умеет.
const HOST_REPO_PORT: u16 = 2002;

/// Поднять на хосте сервер, раздающий каталог репозитория по HTTP.
///
/// Настоящий HTTP, а не заглушка, отвечающая одним и тем же: клиент в госте
/// разбирает строку состояния, заголовки и длину, и проверять его подделкой
/// значило бы проверять согласие двух наших же реализаций. Здесь сервер пишет
/// ответ по букве RFC 9112 — со строкой состояния, `Content-Length` и
/// `Connection: close`, — и отвечает `404` на то, чего нет: отказ гость обязан
/// понимать так же уверенно, как успех.
fn start_host_repo(root: PathBuf) -> Result<std::net::TcpListener> {
    let mut listener = None;
    for attempt in 0..10 {
        match std::net::TcpListener::bind(("127.0.0.1", HOST_REPO_PORT)) {
            Ok(bound) => {
                listener = Some(bound);
                break;
            }
            Err(err) if attempt == 9 => {
                return Err(err).with_context(|| {
                    format!("не удалось занять порт {HOST_REPO_PORT} под сервер обновлений")
                });
            }
            // Порт может ещё держаться в `TIME_WAIT` после прошлого прогона.
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    let listener = listener.expect("цикл выше либо занял порт, либо вернул ошибку");
    let worker = listener.try_clone().context("не удалось раздвоить слушающий сокет")?;
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};

        for stream in worker.incoming() {
            let Ok(mut stream) = stream else { break };
            // Срок на **запрос** — короткий: клиент, подключившийся и
            // замолчавший, не должен занимать сервер, который обслуживает по
            // одному.
            stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
            // А на **ответ** срока нет вовсе, и это не небрежность. Гость
            // вычитывает поток медленно: отладочное ядро под эмуляцией пишет
            // каждый кусок в ext2 и на это время не читает из сокета. Наш
            // передающий буфер тогда заполняется, `write_all` ждёт — и любой
            // срок на этом месте означает, что сервер обрывает исправную
            // загрузку. Ровно так первый прогон и упал: шестидесяти секунд
            // хватило на треть мегабайта, дальше пришёл `RST`, а выглядело это
            // как обрыв связи в госте.
            stream.set_write_timeout(None).ok();

            let mut reader = BufReader::new(match stream.try_clone() {
                Ok(clone) => clone,
                Err(_) => continue,
            });
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            // Остаток заголовка вычитывается и выбрасывается: тела у GET нет, а
            // непрочитанные байты в сокете мешают закрытию.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) if line == "\r\n" || line == "\n" => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
            }

            let mut fields = request.split_whitespace();
            let method = fields.next().unwrap_or("");
            let target = fields.next().unwrap_or("/");
            let body = if method == "GET" { file_for(&root, target) } else { None };

            let response = match &body {
                Some(data) => format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                    data.len()
                ),
                None => String::from(
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                ),
            };
            if stream.write_all(response.as_bytes()).is_err() {
                continue;
            }
            if let Some(data) = body {
                if stream.write_all(&data).is_err() {
                    continue;
                }
            }
            stream.flush().ok();
            // Гость дочитывает тело по длине и закрывается сам; наш `FIN` нужен
            // ему только затем, чтобы не ждать своего таймаута на прощание.
            stream.shutdown(std::net::Shutdown::Write).ok();
        }
    });
    println!("стенд: сервер обновлений слушает {HOST_REPO_PORT}, каталог отдаётся гостю");
    Ok(listener)
}

/// Найти файл, который просят, — и не выпустить запрос за пределы каталога.
///
/// Проверка здесь не формальность: путь приходит из гостя, а раздаётся каталог
/// на машине разработчика. `..` в запросе означал бы, что сценарий, ошибившийся
/// в одной строке, читает что угодно на хосте.
fn file_for(root: &std::path::Path, target: &str) -> Option<Vec<u8>> {
    let target = target.split('?').next().unwrap_or(target);
    let mut path = root.to_path_buf();
    for part in target.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains('\\') {
            return None;
        }
        path.push(part);
    }
    if !path.is_file() {
        return None;
    }
    std::fs::read(path).ok()
}

/// Поднять на хосте эхо-сервер, к которому будет подключаться гость.
///
/// Возвращает поток, который живёт, пока идёт сценарий, и завершается сам, когда
/// стенд закрывает слушающий сокет. Обслуживает соединения по одному: гость в
/// сценарии подключается последовательно, а параллельный сервер потребовал бы
/// пула потоков ради проверки, которой он не нужен.
fn start_host_echo() -> Result<std::net::TcpListener> {
    // Порт может быть ещё занят предыдущим прогоном: соединения, закрытые
    // секунду назад, держат его в `TIME_WAIT` на стороне хоста, и два прогона
    // подряд — обычное дело. Ждём и повторяем, а не падаем: отказ здесь
    // выглядел бы как поломка сети в госте, которой нет.
    let mut listener = None;
    for attempt in 0..10 {
        match std::net::TcpListener::bind(("127.0.0.1", HOST_ECHO_PORT)) {
            Ok(bound) => {
                listener = Some(bound);
                break;
            }
            Err(err) if attempt == 9 => {
                return Err(err).with_context(|| {
                    format!("не удалось занять порт {HOST_ECHO_PORT} под эхо-сервер хоста")
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    let listener = listener.expect("цикл выше либо занял порт, либо вернул ошибку");
    let worker = listener.try_clone().context("не удалось раздвоить слушающий сокет")?;
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in worker.incoming() {
            let Ok(mut stream) = stream else { break };
            stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
            let mut buffer = [0u8; 4096];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => {
                        if stream.write_all(&buffer[..read]).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Гость дочитывает эхо и ждёт нашего `FIN`: без него он просидит до
            // своего таймаута, и сценарий увидит «ответ не пришёл» там, где
            // ответ пришёл весь.
            stream.shutdown(std::net::Shutdown::Both).ok();
        }
    });
    Ok(listener)
}

/// Отправить гостю байты по проброшенному порту и собрать ответ.
///
/// Клиент здесь — обычный `TcpStream` стандартной библиотеки, и это главное:
/// протокол на той стороне разговаривает не сам с собой. Отправка и приём
/// разведены по двум потокам не ради скорости, а ради тупика, который иначе
/// неизбежен: эхо-сервер отвечает по ходу, и отправитель, не читающий ответ,
/// упирается в переполненное окно ровно тогда, когда получатель ждёт, пока он
/// закончит отправлять.
fn tcp_echo(port: u16, payload: &[u8], timeout_ms: u64) -> Result<Vec<u8>> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let deadline = Duration::from_millis(timeout_ms);
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, deadline)
        .with_context(|| format!("не удалось подключиться к 127.0.0.1:{port}"))?;
    stream.set_read_timeout(Some(deadline))?;
    stream.set_write_timeout(Some(deadline))?;
    // Отключаем алгоритм Нейгла: он придерживает мелкие отправки, а мы меряем
    // не пропускную способность, а то, что байты дошли.
    stream.set_nodelay(true).ok();

    let mut writer = stream.try_clone().context("не удалось раздвоить сокет")?;
    let outgoing = payload.to_vec();
    let sender = std::thread::spawn(move || -> std::io::Result<()> {
        writer.write_all(&outgoing)?;
        writer.flush()?;
        // Половина закрывается сразу после отправки: так гость узнаёт, что
        // продолжения не будет, и отвечает своим `FIN`. Без этого приём ниже
        // ждал бы до таймаута даже при исправном обмене.
        //
        // Ошибка здесь **игнорируется**, и это не небрежность: гость успевает
        // ответить эхом и закрыться раньше, чем мы дойдём до этой строки, и
        // тогда Windows отвечает `WSAENOTCONN` на закрытие уже закрытого
        // соединения. Успешность обмена проверяется сравнением байт ниже, а не
        // тем, кто первым положил трубку.
        let _ = writer.shutdown(std::net::Shutdown::Write);
        Ok(())
    });

    let mut echoed = Vec::with_capacity(payload.len());
    let mut chunk = [0u8; 4096];
    while echoed.len() < payload.len() {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => echoed.extend_from_slice(&chunk[..read]),
            Err(err) => {
                sender.join().ok();
                return Err(err).context("чтение эха оборвалось");
            }
        }
    }

    match sender.join() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => return Err(err).context("отправка оборвалась"),
        Err(_) => bail!("поток отправки упал"),
    }
    Ok(echoed)
}

/// Переместить указатель из точки в точку.
///
/// Два пути, и это не выбор между удобным и неудобным. Мышь **нельзя** поставить
/// в точку: у неё нет координат, есть только приращения, и стенд везёт курсор
/// так же, как это делает рука. Планшет, наоборот, нельзя сдвинуть: приращение,
/// посланное машине, у которой из указателей один планшет, не доходит ни до
/// кого — гипервизор доставляет относительные события только тем устройствам,
/// которые объявили, что понимают их.
fn move_pointer(
    qmp: Option<&mut qmp::Qmp>,
    hmp: &mut monitor::Monitor,
    from: (i32, i32),
    to: (i32, i32),
    width: i32,
    height: i32,
) -> Result<()> {
    if let Some(qmp) = qmp {
        return qmp.move_to(to.0, to.1, width, height);
    }
    // Движение разбивается на шаги: отчёт boot-протокола несёт знаковый байт на
    // ось, и приращение больше 127 точек за раз либо обрежется, либо
    // переполнится в обратную сторону. Крупный скачок курсора при этом и в жизни
    // не встречается — мышь шлёт отчёт каждые несколько миллисекунд.
    for (step_x, step_y) in split_move(to.0 - from.0, to.1 - from.1) {
        hmp.mouse_move(step_x, step_y)?;
        std::thread::sleep(POINTER_DELAY);
    }
    Ok(())
}

/// Нажать или отпустить левую кнопку.
///
/// Кнопки планшет как раз понимает и через HMP, но путь всё равно один: события
/// одного устройства обязаны идти одним потоком, иначе щелчок обгоняет
/// перемещение, которое его вызвало.
fn press_button(qmp: Option<&mut qmp::Qmp>, hmp: &mut monitor::Monitor, down: bool) -> Result<()> {
    match qmp {
        Some(qmp) => qmp.button(qmp::BUTTON_LEFT, down),
        None => hmp.mouse_button(if down { 1 } else { 0 }),
    }
}

/// Размер диска, который стенд подсовывает установщику.
///
/// Два гигабайта, а не один: с фазы 32 разделов четыре — ESP на полгигабайта и
/// три равные доли под два корневых слота и состояние. На гигабайтном диске
/// слоты вышли бы по полторы сотни мегабайт, то есть меньше образа системы,
/// который в них предстоит записать.
///
/// Файл разрежённый: `set_len` не занимает места, пока в него не пишут, и
/// лишний гигабайт ничего не стоит ни на диске, ни во времени прогона.
const TARGET_DISK_MIB: u64 = 2048;

/// Диск, на который ставил установщик.
fn prepare_installed_disk(arch: Arch) -> Result<PathBuf> {
    let disk = paths::target_disk(arch);
    if !disk.is_file() {
        bail!(
            "нет диска, на который ставил установщик: {}\n\
             Сценарии 'installed', 'write', 'persist' и 'ahci' идут после 'install' \
             и опираются на его результат.",
            disk.display()
        );
    }
    Ok(disk)
}

/// Каталог, который стенд раздаёт как репозиторий обновлений.
fn repo_dir(arch: Arch) -> PathBuf {
    paths::test_dir().join(format!("repo-{}", arch.name()))
}

/// Собрать репозиторий для стенда: годный и, подкаталогом, с чужой подписью.
///
/// Версия — [`NET_UPDATE_VERSION`], и она **выше** той, что несёт обновление в
/// `/media`. Иначе сценарий зависел бы от того, шёл ли перед ним `update`:
/// после него система работает под `0.2`, и предложенная `0.2` была бы отвергнута
/// запретом отката — то есть проверка сети утонула бы в проверке версий.
fn prepare_repo(arch: Arch, release: bool) -> Result<()> {
    let dir = repo_dir(arch);
    crate::repo::build(&[arch], release, NET_UPDATE_VERSION, &dir)?;
    // Репозиторий с чужой подписью лежит **внутри** годного, подкаталогом:
    // сервер раздаёт дерево, и второй каталог рядом означал бы второй адрес в
    // настройках. Путь короткий (`/x/`) намеренно — он уезжает в гостя строкой
    // по серийной линии, а длинная строка на aarch64 теряет хвост.
    crate::repo::build_untrusted(&dir, NET_UPDATE_VERSION, arch, &dir.join("x"))?;
    Ok(())
}

/// Версия, которую предлагает сервер обновлений стенда.
///
/// Заведомо новее и установленной (`0.1.<сборка>`), и той, что лежит в `/media`
/// (`package::UPDATE_VERSION`, `0.2`): сценарии, меняющие слот, идут друг за
/// другом, и обновление обязано быть новее в любом порядке.
pub const NET_UPDATE_VERSION: &str = "0.3";

/// Положить гостю `/etc/update.cfg` с адресом сервера стенда.
///
/// Гость видит хост как `10.0.2.2` — так устроена пользовательская сеть QEMU.
/// Файл пишется на раздел состояния, то есть оказывается **правкой человека**, и
/// это часть проверки: в образе лежит эталон с другим адресом, и взять систему
/// обязана правку.
fn place_update_config(disk_path: &std::path::Path) -> Result<()> {
    use disk::BlockDevice as _;

    let text = format!(
        "# Written by the harness: the update server lives on the host.\n\
         server=10.0.2.2\n\
         port={HOST_REPO_PORT}\n\
         path=/\n"
    );

    let mut dev = crate::diskfile::DiskFile::open(disk_path, 512)?;
    let table = disk::gpt::read(&mut dev)
        .map_err(|err| anyhow::anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    let state = table
        .find(disk::gpt::FREEOS_STATE_TYPE)
        .ok_or_else(|| anyhow::anyhow!("на образе нет раздела состояния"))?;

    let mut fs = ext2::Editor::open(&mut dev, state.first_lba)
        .map_err(|err| anyhow::anyhow!("раздел состояния не открывается: {err}"))?;
    fs.mark_dirty(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том используемым: {err}"))?;
    match fs.write_file_path(&mut dev, "etc/update.cfg", text.as_bytes(), 0o644, 0, 0) {
        Ok(_) => println!("стенд: гостю положен /etc/update.cfg на 10.0.2.2:{HOST_REPO_PORT}"),
        // Уже лежит с прошлого прогона, и содержимое то же самое: адрес и порт
        // здесь постоянные. Перезаписи `ext2::Editor` не умеет.
        Err(ext2::Error::Exists) => println!("стенд: /etc/update.cfg у гостя уже лежит"),
        Err(err) => bail!("не удалось записать /etc/update.cfg: {err}"),
    }
    fs.flush(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось сбросить раздел состояния: {err}"))?;
    fs.mark_clean(&mut dev)
        .map_err(|err| anyhow::anyhow!("не удалось пометить том чистым: {err}"))?;
    dev.flush()
        .map_err(|err| anyhow::anyhow!("не удалось сбросить образ: {err}"))?;
    Ok(())
}

/// Носители машины для сценария.
fn prepare_drives(
    scenario: &Scenario,
    built: &build::Built,
    arch: Arch,
) -> Result<Vec<Drive>> {
    let drives = match scenario.target {
        Target::Live => vec![Drive::HostDirectory(qemu::prepare_esp(built)?)],
        Target::Image => vec![Drive::Image(image::build(built, image::Kind::System)?)],
        Target::Installer => vec![
            // Порядок важен: прошивка перебирает носители в порядке подключения,
            // и загрузочный раздел на этот момент есть только у первого.
            Drive::Image(image::build(built, image::Kind::Installer)?),
            Drive::Image(image::prepare_target(arch, TARGET_DISK_MIB, true)?),
        ],
        Target::Iso => vec![Drive::Cdrom(image::build_iso(built, image::Kind::System)?)],
        Target::Installed => {
            let disk = prepare_installed_disk(arch)?;
            // Обновление кладётся в образ **до** запуска — так же, как человек
            // положил бы его туда с флешки. Почему не через установочный
            // носитель, сказано в заголовке `crate::diskfile`.
            if scenario.updates {
                let (Some(kernel), Some(initrd)) =
                    (built.get(crate::arch::Component::Kernel), built.initrd())
                else {
                    bail!("сценарию с обновлением нужны собранные ядро и initrd");
                };
                let programs: Vec<(&'static str, PathBuf)> = built
                    .programs()
                    .map(|(name, path)| (name, path.to_path_buf()))
                    .collect();
                crate::package::place_updates(
                    &disk,
                    arch,
                    built.release,
                    kernel,
                    initrd,
                    &programs,
                )?;
            }
            // Ключ — тем же приёмом и до запуска: так же, как человек положил бы
            // его в `~/.ssh/authorized_keys`, сидя за этой машиной. Почему не
            // через образ initrd, сказано в заголовке `sshkeys`.
            if scenario.ssh_key {
                sshkeys::place_authorized_key(&disk, &sshkeys::authorized()?)?;
            }
            // Репозиторий обновлений: собрать каталог на хосте и сказать гостю,
            // где его искать. Файл настроек ложится на раздел состояния — то
            // есть проверяется заодно и главное правило фазы: правка в `/etc`
            // читается **раньше** эталона, приехавшего с образом.
            if scenario.host_repo {
                prepare_repo(arch, built.release)?;
                place_update_config(&disk)?;
            }
            vec![Drive::Image(disk)]
        }
        // Порядок обязателен: прошивка грузится с первого носителя, а второй —
        // тот, ради которого сценарий существует.
        Target::LiveAndDisk => vec![
            Drive::HostDirectory(qemu::prepare_esp(built)?),
            Drive::Image(prepare_installed_disk(arch)?),
        ],
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
