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

use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::arch::Arch;
use crate::build::{self, BuildOptions};
use crate::paths;
use crate::qemu::{self, Drive, Pointer, RunOptions};
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
    let drives = prepare_drives(scenario.target, &built, arch)?;

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

    let result = play(scenario, &mut line, &mut hmp, qmp.as_mut(), prefix, shots);

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
    mut qmp: Option<&mut qmp::Qmp>,
    prefix: &str,
    shots: &mut Vec<PathBuf>,
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
        Target::Iso => vec![Drive::Cdrom(image::build_iso(built, image::Kind::System)?)],
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
