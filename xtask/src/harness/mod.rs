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
mod tlskeys;

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
    /// Сколько прогонов вести одновременно. Ноль — выбрать по числу ядер.
    pub jobs: usize,
}

/// Результат одного сценария.
struct Outcome {
    /// Место в порядке, в котором прогон вывел бы результаты, будь он
    /// последовательным. Воркеры заканчивают вразнобой, а итоговая таблица
    /// обязана читаться одинаково — иначе два прогона не сравнить глазами.
    order: usize,
    scenario: &'static str,
    arch: Arch,
    release: bool,
    error: Option<String>,
    log: PathBuf,
    shots: Vec<PathBuf>,
    elapsed: Duration,
}

/// Один прогон: сценарий на конкретной машине с конкретным профилем.
struct Run {
    order: usize,
    scenario: &'static Scenario,
    arch: Arch,
    release: bool,
}

/// Пачка прогонов, которую обязан отработать **один** воркер и **подряд**.
///
/// Единица планирования здесь не сценарий, а пачка, и вот почему. Половина
/// сценариев работает с диском, на который записал установщик: `install` его
/// пишет, `installed`, `write`, `ssh-shell` и ещё дюжина с него грузятся,
/// `rollback` и `update` меняют на нём активный слот, `install4k` пересоздаёт
/// его с другим размером сектора. Разложить их по разным воркерам нельзя ни в
/// каком порядке: диск у воркера свой, и сценарий, приехавший к соседу,
/// прочитал бы либо чужой диск, либо пустоту.
struct Unit {
    arch: Arch,
    release: bool,
    /// Чем эта пачка занята — для строки о начале работы.
    what: &'static str,
    runs: Vec<Run>,
}

/// Напечатать список сценариев.
pub fn list() {
    say!();
    say!("сценарии стенда:");
    for scenario in scenarios::ALL {
        say!("  {:<12} {}", scenario.name, scenario.about);
        say!("               носитель: {}", scenario.target.title());
    }
    say!();
}

/// Сколько прогонов вести одновременно, если человек не сказал.
///
/// Половина ядер, но не больше трёх. Половина — потому что один прогон это не
/// один поток: у QEMU под TCG есть поток процессора и поток ввода-вывода, а
/// рядом работает сам стенд.
///
/// # Почему потолок именно три, а не «сколько ядер выдержит»
///
/// Выигрыша выше трёх почти нет: цепочка сценариев установленной системы
/// неделима и она же самая длинная, так что общее время упирается в неё, а не
/// в число воркеров. Полный прогон в четыре потока уложил 142 минуты работы в
/// 59 минут по часам — и то же самое сделал бы третий воркер, потому что
/// одиночные сценарии кончаются задолго до цепочек.
///
/// А вот **цена** четвёртого измерима, и она не в процентах. При четырёх
/// гостях сразу два прогона из восьмидесяти девяти упали не по делу: прошивка
/// ARM не дождалась ответа NVMe (`NvmExpressPassThru: Timeout occurs`) и
/// сбросила машину сторожевым таймером посреди установки, а на x86-64 оболочка
/// гостя закончила сеанс по своему двадцатисекундному пределу простоя —
/// часы гостя идут по часам хоста и тогда, когда гостю не досталось процессора.
/// Ни то ни другое не говорит ничего о системе; и то и другое означает, что
/// прогон надо повторять. Три воркера дают то же время и не платят этим.
pub fn default_jobs() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 3)
}

pub fn run(opts: &TestOptions) -> Result<()> {
    let selected: Vec<&'static Scenario> = match &opts.only {
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

    let plan = schedule(&selected, opts)?;
    let total: usize = plan.iter().map(|unit| unit.runs.len()).sum();
    if total == 0 {
        bail!(
            "сценарий '{}' не идёт ни на одной из выбранных архитектур",
            opts.only.as_deref().unwrap_or("")
        );
    }

    // Воркеров не больше, чем пачек: пять потоков на четыре цепочки — это пятый
    // каталог сборки, заведённый ради того, чтобы простоять весь прогон пустым.
    let jobs = opts.jobs.clamp(1, plan.len());

    // Всё общее готовится **до** развилки и дальше только читается. Собери
    // воркеры это сами — и они делали бы одну и ту же работу вчетвером, а на
    // общих файлах (образ initrd, ключи, счётчик сборок) ещё и мешали бы друг
    // другу.
    let built = prebuild(&plan)?;
    if jobs > 1 {
        prepare_shared_material(&plan)?;
    }

    let started = Instant::now();
    let outcomes = if jobs == 1 {
        let mut outcomes = Vec::with_capacity(total);
        for unit in &plan {
            play_unit(unit, &built, opts, 1, &mut outcomes);
        }
        outcomes
    } else {
        say!();
        say!(
            "стенд: {total} прогон(ов) в {jobs} потока(ов); пачек {}, \
             у каждого воркера свой каталог сборки",
            plan.len()
        );
        run_parallel(plan, &built, opts, jobs)
    };

    report(&outcomes, started.elapsed(), jobs)
}

/// Разложить выбранные сценарии по пачкам.
///
/// Порядок — тот же, в котором их гнал бы последовательный прогон
/// (профиль, архитектура, порядок объявления), и это не только ради читаемого
/// итога: цепочка установленного диска держится именно на порядке объявления.
fn schedule(selected: &[&'static Scenario], opts: &TestOptions) -> Result<Vec<Unit>> {
    // Порты серверов обновлений закреплены за конфигурацией, а не за воркером
    // (почему — в [`HostPorts`]), и держится это на одном свойстве: с этими
    // серверами работают только сценарии установленного диска, то есть все они
    // попадают в одну цепочку. Сценарий, которому сервер понадобится вне
    // цепочки, это свойство сломает — и сломает молчаливо: два прогона одной
    // конфигурации возьмутся за один порт, а выглядеть это будет как
    // «репозиторий вдруг отдаёт чужую архитектуру». Поэтому — вслух и сразу.
    if let Some(stray) = selected
        .iter()
        .find(|scenario| scenario.host_repo && !scenario.target.shares_target_disk())
    {
        bail!(
            "сценарий '{}' просит сервер обновлений, но не работает с установленным диском.\n\
             Порт сервера закреплён за конфигурацией и достаётся цепочке; вне её \
             два прогона возьмутся за один номер.\n\
             Либо переведите сценарий на Target::Installed, либо раздайте порты иначе \
             (см. HostPorts в xtask/src/harness/mod.rs).",
            stray.name
        );
    }

    let mut chains: Vec<Unit> = Vec::new();
    let mut singles: Vec<Unit> = Vec::new();
    let mut order = 0usize;

    for &release in &opts.profiles {
        for &arch in &opts.arches {
            let mut chain = Vec::new();
            for scenario in selected {
                if !scenario.runs_on(arch) {
                    continue;
                }
                let run = Run { order, scenario, arch, release };
                order += 1;
                if scenario.target.shares_target_disk() {
                    chain.push(run);
                } else {
                    singles.push(Unit {
                        arch,
                        release,
                        what: scenario.name,
                        runs: vec![run],
                    });
                }
            }
            if !chain.is_empty() {
                chains.push(Unit { arch, release, what: "цепочка установленной системы", runs: chain });
            }
        }
    }

    // Цепочки — в начало очереди, и это единственное место, где расписание
    // вообще что-то решает. Цепочка длиннее любого одиночного сценария в
    // двадцать раз; начатая последней, она одна доработала бы уже после того,
    // как остальные воркеры разошлись.
    chains.extend(singles);
    Ok(chains)
}

/// Собрать всё, что понадобится прогону, — по разу на конфигурацию.
///
/// Раньше это делал каждый сценарий сам, и в последовательном прогоне разницы
/// не было: cargo на втором вызове отвечает «пересобирать нечего» за секунду.
/// В параллельном разница принципиальная. Во-первых, `cargo build` берёт замок
/// на каталог `target/`, и четыре воркера выстроились бы в очередь на ровном
/// месте. Во-вторых — и это важнее — образ initrd собирается **в файл**, и
/// воркер, пересобирающий его посреди чужого прогона, подменил бы систему,
/// которую сосед в этот момент грузит.
fn prebuild(plan: &[Unit]) -> Result<std::collections::HashMap<(Arch, bool), build::Built>> {
    use std::collections::{HashMap, HashSet};

    // Установщик собирается только там, где он кому-то нужен: это отдельный
    // крейт под UEFI, и в прогоне из одного сценария платить за него незачем.
    let mut needed: Vec<(Arch, bool)> = Vec::new();
    let mut with_installer: HashSet<(Arch, bool)> = HashSet::new();
    for unit in plan {
        let key = (unit.arch, unit.release);
        if !needed.contains(&key) {
            needed.push(key);
        }
        if unit.runs.iter().any(|run| run.scenario.target.needs_installer()) {
            with_installer.insert(key);
        }
    }

    let mut built = HashMap::new();
    for (arch, release) in needed {
        say!();
        say!(
            "=== сборка {arch} / {} ===",
            paths::profile_dir_name(release)
        );
        built.insert(
            (arch, release),
            build::build_all(&BuildOptions {
                arch,
                release,
                kernel: true,
                initrd: true,
                installer: with_installer.contains(&(arch, release)),
            })?,
        );
    }
    Ok(built)
}

/// Завести общий материал прогона: ключи SSH и комплекты TLS.
///
/// Тоже до развилки, и по причине, которая дороже времени: и то и другое
/// заводится «если файла ещё нет», а два воркера, не нашедшие файла
/// одновременно, зовут `ssh-keygen` на одно и то же имя. Один из них получит
/// отказ, а второй — пару, половина которой уже уехала в чужой образ.
fn prepare_shared_material(plan: &[Unit]) -> Result<()> {
    let needs_ssh = plan
        .iter()
        .flat_map(|unit| &unit.runs)
        .any(|run| run.scenario.ssh_key || run.scenario.guest_port != 0);
    let needs_tls = plan
        .iter()
        .flat_map(|unit| &unit.runs)
        .any(|run| run.scenario.host_repo);

    if needs_ssh {
        sshkeys::authorized()?;
        sshkeys::stranger()?;
    }
    if needs_tls {
        tlskeys::trusted()?;
        tlskeys::stranger()?;
    }
    // Номер сборки считается по всему дереву исходников и кладётся в имена
    // образов. Посчитанный один раз здесь, он гарантированно один на прогон.
    crate::version::build_number()?;
    Ok(())
}

/// Раздать пачки воркерам и дождаться всех.
fn run_parallel(
    plan: Vec<Unit>,
    built: &std::collections::HashMap<(Arch, bool), build::Built>,
    opts: &TestOptions,
    jobs: usize,
) -> Vec<Outcome> {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    let queue: Mutex<VecDeque<Unit>> = Mutex::new(plan.into());
    let collected: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());

    std::thread::scope(|scope| {
        for index in 1..=jobs {
            let queue = &queue;
            let collected = &collected;
            scope.spawn(move || {
                // Два признака воркера, и оба — свойства потока: каталог, в
                // который он строит, и метка, с которой он говорит.
                paths::set_worker(index as u16);
                crate::out::set_tag(format!("[w{index}] "));

                loop {
                    let Some(unit) = queue
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .pop_front()
                    else {
                        break;
                    };
                    let mut mine = Vec::with_capacity(unit.runs.len());
                    play_unit(&unit, built, opts, jobs as u16, &mut mine);
                    collected
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .extend(mine);
                }
            });
        }
    });

    let mut outcomes = collected.into_inner().unwrap_or_else(|poisoned| poisoned.into_inner());
    outcomes.sort_by_key(|outcome| outcome.order);
    outcomes
}

/// Отработать пачку целиком, по сценарию за раз.
fn play_unit(
    unit: &Unit,
    built: &std::collections::HashMap<(Arch, bool), build::Built>,
    opts: &TestOptions,
    workers: u16,
    outcomes: &mut Vec<Outcome>,
) {
    let profile = paths::profile_dir_name(unit.release);
    say!();
    say!(
        "=== пачка: {} / {} / {profile} ({} шт.) ===",
        unit.what,
        unit.arch,
        unit.runs.len()
    );

    for run in &unit.runs {
        say!();
        say!("=== {} / {} / {profile} ===", run.scenario.name, run.arch);
        say!("{}", run.scenario.about);

        let Some(built) = built.get(&(run.arch, run.release)) else {
            // Случиться не может: набор собранного строится по тому же плану.
            // Но молчаливый пропуск сценария хуже внятного провала.
            outcomes.push(Outcome {
                order: run.order,
                scenario: run.scenario.name,
                arch: run.arch,
                release: run.release,
                error: Some(String::from("для этой конфигурации ничего не собрано")),
                log: paths::test_dir().join("нет"),
                shots: Vec::new(),
                elapsed: Duration::ZERO,
            });
            continue;
        };

        let started = Instant::now();
        let mut outcome = run_scenario(run, built, opts.windowed, workers);
        outcome.elapsed = started.elapsed();
        say!(
            "--- {} / {} / {profile}: {} за {}",
            run.scenario.name,
            run.arch,
            match outcome.error {
                Some(_) => "ПРОВАЛ",
                None => "ок",
            },
            render_duration(outcome.elapsed)
        );
        outcomes.push(outcome);
    }
}

/// Время в виде «7 мин 12 с» — секунды в четырёхзначных числах не читаются.
fn render_duration(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    if seconds < 60 {
        return format!("{seconds} с");
    }
    format!("{} мин {:02} с", seconds / 60, seconds % 60)
}

fn report(outcomes: &[Outcome], wall: Duration, jobs: usize) -> Result<()> {
    say!();
    say!("--- итог ---");
    let mut failed = 0usize;
    let mut work = Duration::ZERO;
    for outcome in outcomes {
        work += outcome.elapsed;
        let mark = if outcome.error.is_some() { "ПРОВАЛ" } else { "ок    " };
        say!(
            "{mark}  {:<12} {:<8} {:<7} {:>10}  {}",
            outcome.scenario,
            outcome.arch.name(),
            paths::profile_dir_name(outcome.release),
            render_duration(outcome.elapsed),
            outcome.log.display()
        );
        for shot in &outcome.shots {
            say!("        снимок: {}", shot.display());
        }
        if let Some(error) = &outcome.error {
            failed += 1;
            for line in error.lines() {
                say!("        {line}");
            }
        }
    }
    say!();
    if jobs > 1 {
        // Отношение — единственное честное число про выигрыш: «во столько раз
        // прогон короче того же прогона в один поток» посчитать нельзя, не
        // прогнав его в один поток, а вот сколько работы уложилось в это
        // время — видно прямо здесь.
        say!(
            "работы {} в {jobs} потока(ов), по часам {}",
            render_duration(work),
            render_duration(wall)
        );
    } else {
        say!("по часам {}", render_duration(wall));
    }

    if failed > 0 {
        bail!("сценариев провалено: {failed} из {}", outcomes.len());
    }
    say!("все сценарии пройдены: {}", outcomes.len());
    Ok(())
}

/// Сценарии, которым нужна машина целиком.
///
/// Список короткий и, надеюсь, таким останется. Попадают в него не «медленные»
/// сценарии — медленных много, и они прекрасно уживаются, — а те, чей отказ под
/// нагрузкой приходит **не от нашего срока**.
///
/// `install4k` ставит систему на диск с сектором 4096, а такой диск умеет
/// изображать только `nvme`, и добирается до него прошивка своим драйвером. Срок
/// ожидания у этого драйвера внутри edk2: при трёх гостях сразу он не
/// дожидается ответа (`NvmExpressPassThru: Timeout occurs`), объявляет отказ
/// блочного уровня, и сторожевой таймер сбрасывает машину посреди установки.
/// Поднять этот срок из стенда нельзя ничем — можно только не мешать. В
/// одиночку сценарий проходит за полминуты, так что стоит это ожидание
/// недорого, а без него падают три конфигурации из четырёх и утаскивают за
/// собой `sector4k`, которому нужен записанный ими диск.
const ALONE: &[&str] = &["install4k"];

/// Пропуск на прогон: обычные сценарии проходят вместе, одиночные — одни.
static GATE: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Прогнать один сценарий. Ошибка сценария не прерывает прогон — она попадает в
/// итог: результат остальных проверок нужен ровно тогда, когда одна упала.
fn run_scenario(run: &Run, built: &build::Built, windowed: bool, workers: u16) -> Outcome {
    let prefix = format!(
        "{}-{}-{}",
        run.scenario.name,
        run.arch.name(),
        paths::profile_dir_name(run.release)
    );
    let log = paths::test_dir().join(format!("{prefix}.log"));
    let mut shots = Vec::new();

    // Пропуск берётся на всё время прогона: одиночный сценарий ждёт, пока
    // разойдутся остальные, и не пускает новых, пока идёт сам.
    let alone = ALONE.contains(&run.scenario.name);
    let _pass: Box<dyn std::any::Any> = if alone {
        say!("ждём, пока освободится машина: {} идёт один", run.scenario.name);
        Box::new(GATE.write().unwrap_or_else(|poisoned| poisoned.into_inner()))
    } else {
        Box::new(GATE.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
    };

    let error = match execute(run.scenario, built, windowed, workers, &prefix, &log, &mut shots) {
        Ok(()) => None,
        Err(err) => Some(format!("{err:#}")),
    };

    Outcome {
        order: run.order,
        scenario: run.scenario.name,
        arch: run.arch,
        release: run.release,
        error,
        log,
        shots,
        elapsed: Duration::ZERO,
    }
}

fn execute(
    scenario: &Scenario,
    built: &build::Built,
    windowed: bool,
    workers: u16,
    prefix: &str,
    log_path: &std::path::Path,
    shots: &mut Vec<PathBuf>,
) -> Result<()> {
    let arch = built.arch;
    // Порты серверов хоста: три закреплены за конфигурацией, четвёртый
    // (эхо-сервер) появится ниже, когда его выдаст ядро ОС.
    let mut ports = HostPorts::for_config(arch, built.release);
    let drives = prepare_drives(scenario, built, arch, ports)?;

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
        let (listener, port) = start_host_echo()?;
        ports.echo = port;
        Some(listener)
    } else {
        None
    };

    // Сервер обновлений — тем же приёмом и по той же причине: гость идёт к нему
    // сам, в первые же секунды после того, как поднялась сеть.
    //
    // Их сразу три, и это не расточительство. Обычный HTTP — то, чем машина
    // обновляется сегодня; HTTPS с корнем, который гость знает, — запасной
    // канал фазы 39a; HTTPS с корнем, которого он не знает, — единственное,
    // что отличает «мы соединились по TLS» от «мы проверили, с кем». Клиент,
    // принимающий любой сертификат, проходит первые две проверки так же
    // успешно, как правильный.
    let _host_repo = if scenario.host_repo {
        let root = paths::test_repo_dir(arch, built.release);
        Some((
            start_host_repo(root.clone(), ports.repo)?,
            start_host_tls_repo(root.clone(), ports.tls, tlskeys::trusted()?)?,
            start_host_tls_repo(root, ports.stranger, tlskeys::stranger()?)?,
        ))
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

    let mut cmd = qemu::command(&opts, built)?;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    say!("> {}", util::render_command(&cmd));

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
        ports,
        workers,
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
    say!("журнал: {}", log_path.display());

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
    ports: HostPorts,
    // Сколько прогонов идёт одновременно: от этого зависят сроки ожидания.
    workers: u16,
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
    //
    // Тем же приёмом в текст попадают номера портов хоста (`{echo}`, `{repo}`,
    // `{tls}`, `{stranger}`). Писать их в сценарии числом было можно, пока
    // прогон был один: теперь у каждого воркера своя четвёрка, и `2002`,
    // набранное в команде гостю, увело бы его к соседу — к серверу, который
    // раздаёт образы **другой** архитектуры. Ошибка при этом выглядела бы как
    // испорченный образ обновления.
    // Сколько ждать на самом деле.
    //
    // Все сроки в сценариях написаны для машины, на которой один гость. Они
    // отвечают на вопрос «не повисло ли», и запас в них есть — но запас,
    // рассчитанный на одного. При трёх гостях сразу всё, что делает гость,
    // идёт втрое дольше, и сроки начинают срабатывать на исправной работе:
    // `kill 11` не успел напечатать ответ за пятнадцать секунд, потому что
    // оболочка в этот момент перерисовывала окно после очередной попытки
    // `init` поднять `sshd`.
    //
    // Поэтому терпение растёт вместе с числом воркеров. Цена честная и
    // названа вслух: настоящее зависание будет объявлено втрое позже. Ложный
    // провал на исправной системе стоит дороже — его нельзя отличить от
    // настоящего, не прогнав сценарий заново.
    let patience = |ms: u64| Duration::from_millis(ms * u64::from(workers.max(1)));

    let mut captured: Option<String> = None;
    let mut captured2: Option<String> = None;
    let fill = move |text: &str, captured: &Option<String>, second: &Option<String>| -> String {
        let text = match captured {
            Some(value) => text.replace("{}", value),
            None => text.to_string(),
        };
        let text = match second {
            Some(value) => text.replace("{2}", value),
            None => text,
        };
        if !text.contains('{') {
            return text;
        }
        text.replace("{echo}", &ports.echo.to_string())
            .replace("{repo}", &ports.repo.to_string())
            .replace("{tls}", &ports.tls.to_string())
            .replace("{stranger}", &ports.stranger.to_string())
    };

    for (index, step) in scenario.steps.iter().enumerate() {
        let at = started.elapsed().as_millis();
        match step {
            Step::Await(needle, timeout_ms) => {
                let needle = fill(needle, &captured, &captured2);
                say!("  [{at:>6} мс] шаг {index}: ждём {needle:?}");
                line.wait_for(&needle, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
            }
            Step::AwaitAny(needle, timeout_ms) => {
                let needle = fill(needle, &captured, &captured2);
                say!("  [{at:>6} мс] шаг {index}: ждём {needle:?} где угодно в выводе");
                line.wait_seen(&needle, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
            }
            Step::Capture(prefix, timeout_ms) => {
                say!("  [{at:>6} мс] шаг {index}: ждём {prefix:?} и запоминаем число за ним");
                let value = line
                    .capture_number(prefix, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                say!("  [{at:>6} мс] шаг {index}: запомнено {value:?}");
                captured = Some(value);
            }
            Step::Capture2(prefix, timeout_ms) => {
                say!("  [{at:>6} мс] шаг {index}: ждём {prefix:?} и запоминаем второе число");
                let value = line
                    .capture_number(prefix, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                say!("  [{at:>6} мс] шаг {index}: запомнено вторым {value:?}");
                captured2 = Some(value);
            }
            Step::Clock(prefix, tolerance_s, timeout_ms) => {
                say!("  [{at:>6} мс] шаг {index}: сверяем часы гостя с часами хоста");
                let value = line
                    .capture_number(prefix, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let guest: i64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число секунд"))?;
                let host = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .context("часы хоста до 1970 года")?
                    .as_secs() as i64;
                let drift = guest - host;
                say!(
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
                say!("  [{at:>6} мс] шаг {index}: ждём число за {prefix:?}, не больше {limit}");
                let value = line
                    .capture_number(prefix, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let number: u64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число"))?;
                say!("             получено {number}");
                if number > *limit {
                    bail!("шаг {index}: за {prefix:?} стоит {number}, а предел {limit}");
                }
            }
            Step::AtLeast(prefix, limit, timeout_ms) => {
                say!("  [{at:>6} мс] шаг {index}: ждём число за {prefix:?}, не меньше {limit}");
                let value = line
                    .capture_number(prefix, patience(*timeout_ms))
                    .with_context(|| format!("шаг {index}"))?;
                let number: u64 = value
                    .parse()
                    .with_context(|| format!("шаг {index}: {value:?} — не число"))?;
                say!("             получено {number}");
                if number < *limit {
                    bail!("шаг {index}: за {prefix:?} стоит {number}, а нужно не меньше {limit}");
                }
            }
            Step::Expect(needle) => {
                let needle = fill(needle, &captured, &captured2);
                say!("  [{at:>6} мс] шаг {index}: проверяем {needle:?}");
                if !line.seen(&needle) {
                    bail!("шаг {index}: в выводе нет {needle:?}");
                }
            }
            Step::Absent(needle) => {
                let needle = fill(needle, &captured, &captured2);
                say!("  [{at:>6} мс] шаг {index}: проверяем отсутствие {needle:?}");
                if line.seen(&needle) {
                    bail!("шаг {index}: в выводе встретилось {needle:?}, чего быть не должно");
                }
            }
            Step::Key(name) => {
                say!("  [{at:>6} мс] шаг {index}: клавиша {name}");
                hmp.sendkey(name).with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Repeat(name, times) => {
                say!("  [{at:>6} мс] шаг {index}: клавиша {name} ×{times}");
                for _ in 0..*times {
                    hmp.sendkey(name).with_context(|| format!("шаг {index}"))?;
                    std::thread::sleep(KEY_DELAY);
                }
            }
            Step::Type(text) => {
                say!("  [{at:>6} мс] шаг {index}: набираем {text:?}");
                for name in keys::spell(text).with_context(|| format!("шаг {index}"))? {
                    hmp.sendkey(&name).with_context(|| format!("шаг {index}"))?;
                    std::thread::sleep(KEY_DELAY);
                }
            }
            Step::Line(text) => {
                let text = fill(text, &captured, &captured2);
                say!("  [{at:>6} мс] шаг {index}: в линию {text:?}");
                line.write_line(&text).with_context(|| format!("шаг {index}"))?;
            }
            Step::Raw(bytes) => {
                say!("  [{at:>6} мс] шаг {index}: в линию {bytes:02x?}");
                line.write_raw(bytes).with_context(|| format!("шаг {index}"))?;
            }
            Step::Aim(target) => {
                let log = line.text();
                let (width, height) = aim::screen(&log).with_context(|| format!("шаг {index}"))?;
                let from = *pointer.get_or_insert((width / 2, height / 2));
                let to = aim::resolve(*target, &log).with_context(|| format!("шаг {index}"))?;
                say!(
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
                say!("  [{at:>6} мс] шаг {index}: указатель на {dx},{dy}");
                let log = line.text();
                let (width, height) = aim::screen(&log).with_context(|| format!("шаг {index}"))?;
                let from = *pointer.get_or_insert((width / 2, height / 2));
                let to = (from.0 + dx, from.1 + dy);
                move_pointer(qmp.as_deref_mut(), hmp, from, to, width, height)
                    .with_context(|| format!("шаг {index}"))?;
                pointer = Some(to);
            }
            Step::Click => {
                say!("  [{at:>6} мс] шаг {index}: щелчок");
                press_button(qmp.as_deref_mut(), hmp, true)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(POINTER_DELAY);
                press_button(qmp.as_deref_mut(), hmp, false)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::RightClick => {
                say!("  [{at:>6} мс] шаг {index}: щелчок правой");
                press_right(qmp.as_deref_mut(), hmp, true)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(POINTER_DELAY);
                press_right(qmp.as_deref_mut(), hmp, false)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Press => {
                say!("  [{at:>6} мс] шаг {index}: кнопка нажата");
                press_button(qmp.as_deref_mut(), hmp, true)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Release => {
                say!("  [{at:>6} мс] шаг {index}: кнопка отпущена");
                press_button(qmp.as_deref_mut(), hmp, false)
                    .with_context(|| format!("шаг {index}"))?;
                std::thread::sleep(KEY_DELAY);
            }
            Step::Plug(spec) => {
                say!("  [{at:>6} мс] шаг {index}: подключаем {spec}");
                hmp.device_add(spec).with_context(|| format!("шаг {index}"))?;
            }
            Step::Unplug(id) => {
                say!("  [{at:>6} мс] шаг {index}: выдёргиваем {id}");
                hmp.device_del(id).with_context(|| format!("шаг {index}"))?;
            }
            Step::Wait(ms) => {
                say!("  [{at:>6} мс] шаг {index}: пауза {ms} мс");
                std::thread::sleep(Duration::from_millis(*ms));
            }
            Step::Reset => {
                say!("  [{at:>6} мс] шаг {index}: сброс машины без предупреждения гостя");
                hmp.system_reset().with_context(|| format!("шаг {index}"))?;
            }
            Step::PowerButton => {
                say!("  [{at:>6} мс] шаг {index}: кнопка питания");
                hmp.system_powerdown().with_context(|| format!("шаг {index}"))?;
            }
            Step::Exits(timeout_ms) => {
                say!("  [{at:>6} мс] шаг {index}: ждём, что QEMU завершится сам");
                let deadline = Instant::now() + patience(*timeout_ms);
                loop {
                    match child.try_wait().with_context(|| format!("шаг {index}"))? {
                        Some(status) => {
                            say!("             QEMU завершился сам: {status}");
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
                say!("  [{at:>6} мс] шаг {index}: с хоста на 127.0.0.1:{port} — \"{text}\"");
                let echoed = tcp_echo(port, text.as_bytes(), patience(*timeout_ms).as_millis() as u64)
                    .with_context(|| format!("шаг {index}"))?;
                if echoed != text.as_bytes() {
                    bail!(
                        "шаг {index}: вернулось не то: {:?}",
                        String::from_utf8_lossy(&echoed)
                    );
                }
                say!("             вернулось {} байт, совпало", echoed.len());
            }
            Step::TcpBulk(kilobytes, timeout_ms) => {
                let Some(port) = hostfwd else {
                    bail!("шаг {index}: сценарий не пробрасывает порт, стучаться некуда");
                };
                say!("  [{at:>6} мс] шаг {index}: {kilobytes} КиБ туда и обратно");
                // Узор, а не нули: одинаковые байты сошлись бы и при
                // перепутанном порядке сегментов, а такой — нет.
                let payload: Vec<u8> = (0..kilobytes * 1024)
                    .map(|i| (i % 251) as u8)
                    .collect();
                let echoed = tcp_echo(port, &payload, patience(*timeout_ms).as_millis() as u64)
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
                say!("             {} байт совпали до последнего", echoed.len());
            }
            Step::Ssh(run) => {
                let Some(port) = hostfwd else {
                    bail!("шаг {index}: сценарий не пробрасывает порт, стучаться некуда");
                };
                let what = match run.command {
                    "" => "оболочка",
                    command => command,
                };
                say!("  [{at:>6} мс] шаг {index}: ssh -v на 127.0.0.1:{port} — {what}");
                let output = run_ssh(port, run, prefix, index, patience(run.timeout_ms))
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
                    say!("             {}", line.trim());
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
                say!("  [{at:>6} мс] шаг {index}: снимок '{name}'");
                let ppm = paths::test_dir().join(format!("{prefix}-{name}.ppm"));
                let png = ppm.with_extension("png");
                std::fs::create_dir_all(paths::test_dir()).ok();
                hmp.screendump(&ppm).with_context(|| format!("шаг {index}"))?;
                let (w, h) = shot::ppm_to_png(&ppm, &png).with_context(|| format!("шаг {index}"))?;
                say!("             {w}x{h} -> {}", png.display());
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
fn run_ssh(
    port: u16,
    run: &scenarios::SshRun,
    prefix: &str,
    index: usize,
    // Срок — уже с поправкой на число воркеров (см. `patience` в `play`).
    timeout: Duration,
) -> Result<String> {
    use std::process::Command;

    let timeout_ms = timeout.as_millis() as u64;
    // Файл известных хостов — свой у каждого прогона, а не один на стенд:
    // прогонов теперь может идти несколько сразу, и общий файл они бы
    // пересоздавали друг у друга под ногами.
    let known_hosts = paths::test_dir().join(format!("{prefix}-known-hosts"));
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
    say!("             вывод ssh: {}", log.display());

    Ok(text)
}

/// Порты, которые стенд занимает на хосте под один прогон.
///
/// # Три из них закреплены за конфигурацией
///
/// Адреса серверов обновлений уезжают гостю в `/etc/update.cfg` — файл на
/// разделе состояния, который пишется **до** запуска машины и переживает
/// запуск команды. Спросить свободный номер у ядра ОС, как это делается для
/// серийной линии, здесь негде: к моменту, когда номер стал бы известен, файл
/// уже записан. Номер, привязанный к **воркеру**, тоже не годится — при
/// следующем запуске цепочка достанется другому воркеру, а на диске останется
/// прежний адрес, и гость пошёл бы туда, где никто не слушает.
///
/// Поэтому номер закреплён за конфигурацией: `x86_64/debug` получает 2002–2004
/// (те же, что были всегда), `x86_64/release` — 2012–2014, и так далее. Занять
/// их одновременно два прогона не могут: с сервером обновлений работают только
/// сценарии, читающие установленный диск, а они собраны в одну неделимую
/// цепочку, и цепочка у конфигурации одна.
///
/// # Четвёртый выдаёт ядро ОС
///
/// Эхо-серверу такой привязки не нужно: его адрес попадает в гостя строкой
/// команды, которую стенд набирает **во время** прогона (`echoc 10.0.2.2
/// {echo} ...`), и подставить туда можно что угодно. Значит и незачем: порт,
/// выданный ядром из свободных, не конфликтует ни с чем и никогда.
#[derive(Clone, Copy)]
struct HostPorts {
    /// Эхо-сервер, к которому подключается гость. Ноль — сценарию он не нужен.
    echo: u16,
    /// Репозиторий обновлений по обычному HTTP.
    repo: u16,
    /// Он же по HTTPS, с корнем, который гость знает.
    tls: u16,
    /// Он же по HTTPS, с корнем, которого гость не знает.
    stranger: u16,
}

impl HostPorts {
    fn for_config(arch: Arch, release: bool) -> Self {
        let slot = match arch {
            Arch::X86_64 => 0,
            Arch::Aarch64 => 2,
        } + u16::from(release);
        let base = 2000 + 10 * slot;
        Self {
            echo: 0,
            repo: base + 2,
            tls: base + 3,
            stranger: base + 4,
        }
    }
}

/// Сервер стенда, живущий ровно столько, сколько идёт сценарий.
///
/// # Зачем понадобилась остановка
///
/// Раньше сервер запускался потоком, которому отдавали **копию** слушающего
/// сокета, и поток крутился в `incoming()` до конца процесса. Пока сценарии с
/// сервером обновлений гоняли по одному, это сходило с рук: процесс кончался
/// вместе с прогоном. Первый же полный прогон показал цену — `update-tls`
/// пришёл следом за `update-net`, порт 2002 всё ещё держал поток предыдущего
/// сценария, и проверка HTTPS упала на «адрес занят», не начавшись.
///
/// Закрыть сокет, уронив свою копию, нельзя: копия в потоке держит его сама, а
/// `accept` в ней спит. Поэтому сокет здесь неблокирующий, поток просыпается
/// раз в полсотни миллисекунд и смотрит на флаг, а [`Drop`] флаг поднимает и
/// дожидается потока — то есть возврат из сценария означает освобождённый порт,
/// а не «когда-нибудь освободится».
struct HostServer {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Drop for HostServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Обслуживать соединения, пока сценарий не кончится.
///
/// Соединения обслуживаются по одному и намеренно: гость подключается
/// последовательно, а пул потоков ради этого — лишний способ отказать.
fn serve<F>(listener: std::net::TcpListener, handle: F) -> Result<HostServer>
where
    F: Fn(std::net::TcpStream) + Send + 'static,
{
    use std::sync::atomic::{AtomicBool, Ordering};

    listener
        .set_nonblocking(true)
        .context("не удалось сделать слушающий сокет неблокирующим")?;
    let stop = std::sync::Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let worker = std::thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((stream, _)) => {
                    // Принятый сокет наследует режим слушающего, а весь обмен
                    // ниже написан на блокирующих чтениях: без этой строки
                    // сервер отвечал бы «данных пока нет» на каждый запрос.
                    if stream.set_nonblocking(false).is_err() {
                        continue;
                    }
                    handle(stream);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => break,
            }
        }
        // Сокет уезжает вместе с потоком и закрывается здесь — в этом весь
        // смысл: порт свободен ровно тогда, когда поток кончился.
    });
    Ok(HostServer { stop, worker: Some(worker) })
}

/// Поднять на хосте тот же репозиторий, но по HTTPS.
///
/// Реализация TLS здесь **чужая** (`rustls`), и это главное свойство проверки:
/// наш клиент, проверенный нашим же сервером, доказывал бы только то, что две
/// половины одной ошибки согласны друг с другом. Расписание ключей, разбор
/// расширений, порядок сообщений — всё это `rustls` делает по стандарту, а не
/// так, как поняли его мы.
fn start_host_tls_repo(
    root: PathBuf,
    port: u16,
    material: tlskeys::Material,
) -> Result<HostServer> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.leaf_der)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(material.key_der)),
        )
        .context("rustls не принял сертификат стенда")?;
    let config = Arc::new(config);

    let listener = bind_with_patience(port, "сервер обновлений по HTTPS")?;
    let server = serve(listener, move |stream| {
        use std::io::{Read, Write};

        {
            stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
            // Срока на запись нет по той же причине, что у обычного сервера:
            // гость вычитывает поток медленно, и любой срок здесь означает, что
            // сервер обрывает исправную загрузку.
            stream.set_write_timeout(None).ok();

            let Ok(mut connection) = rustls::ServerConnection::new(config.clone()) else {
                return;
            };
            let mut stream = stream;

            // Обмен ведётся руками, а не через `StreamOwned`, и это не вкусовое
            // решение — на нём стенд встал на двадцать секунд.
            //
            // Дефект виден только в записи трафика: клиент присылает `Finished`
            // и запрос двумя сегментами через три миллисекунды, а сервер
            // успевает прочитать оба **одним** `read_tls`. Рукопожатие при этом
            // заканчивается, `complete_io` возвращается — и следующий вызов
            // снова идёт читать сокет, хотя запись с запросом уже лежит
            // разобранной в буфере. Читать нечего, и сервер стоит, пока клиент
            // не закроет соединение по своему сроку. Со стороны это выглядит
            // как «сервер не отвечает», и искать причину идут в клиента.
            //
            // Лекарство — порядок: **сначала разобрать то, что уже прочитано**,
            // и только потом блокироваться на сокете.
            let mut request = Vec::new();
            let mut broken = false;
            loop {
                if let Err(err) = connection.process_new_packets() {
                    eprintln!("[tls:{port}] разбор не удался: {err}");
                    broken = true;
                    break;
                }
                while connection.wants_write() {
                    if connection.write_tls(&mut stream).is_err() {
                        broken = true;
                        break;
                    }
                }
                if broken {
                    break;
                }
                let mut chunk = [0u8; 4096];
                match connection.reader().read(&mut chunk) {
                    Ok(0) => {}
                    Ok(got) => request.extend_from_slice(&chunk[..got]),
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        broken = true;
                        break;
                    }
                }
                // Запрос кончается пустой строкой; тела у GET нет.
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
                match connection.read_tls(&mut stream) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(err) => {
                        eprintln!("[tls:{port}] чтение оборвалось: {err}");
                        broken = true;
                        break;
                    }
                }
            }
            if broken {
                return;
            }

            let request = String::from_utf8_lossy(&request).to_string();
            let first = request.lines().next().unwrap_or("");
            let mut fields = first.split_whitespace();
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
            let mut answer = response.into_bytes();
            if let Some(data) = body {
                answer.extend_from_slice(&data);
            }
            let mut sent = 0usize;
            let mut failed = false;
            while sent < answer.len() {
                let took = match connection.writer().write(&answer[sent..]) {
                    Ok(0) => break,
                    Ok(took) => took,
                    Err(_) => {
                        failed = true;
                        break;
                    }
                };
                sent += took;
                while connection.wants_write() {
                    if connection.write_tls(&mut stream).is_err() {
                        failed = true;
                        break;
                    }
                }
                if failed {
                    break;
                }
            }
            // `close_notify` и `FIN`: гость дочитывает тело по длине, но сказать
            // «это конец, а не обрыв» обязан именно TLS — иначе оборванное
            // соединение неотличимо от законченного.
            connection.send_close_notify();
            while connection.wants_write() {
                if connection.write_tls(&mut stream).is_err() {
                    break;
                }
            }
            stream.flush().ok();
            stream.shutdown(std::net::Shutdown::Write).ok();
        }
    })?;
    say!("стенд: сервер обновлений по HTTPS слушает {port}");
    Ok(server)
}

/// Занять порт, повторяя попытки: он мог остаться в `TIME_WAIT`.
fn bind_with_patience(port: u16, what: &str) -> Result<std::net::TcpListener> {
    for attempt in 0..10 {
        match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(bound) => return Ok(bound),
            Err(err) if attempt == 9 => {
                return Err(err).with_context(|| format!("не удалось занять порт {port} под {what}"));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
    unreachable!("цикл выше либо занял порт, либо вернул ошибку")
}

/// Поднять на хосте сервер, раздающий каталог репозитория по HTTP.
///
/// Настоящий HTTP, а не заглушка, отвечающая одним и тем же: клиент в госте
/// разбирает строку состояния, заголовки и длину, и проверять его подделкой
/// значило бы проверять согласие двух наших же реализаций. Здесь сервер пишет
/// ответ по букве RFC 9112 — со строкой состояния, `Content-Length` и
/// `Connection: close`, — и отвечает `404` на то, чего нет: отказ гость обязан
/// понимать так же уверенно, как успех.
fn start_host_repo(root: PathBuf, port: u16) -> Result<HostServer> {
    // Порт может ещё держаться в `TIME_WAIT` после прошлого прогона — ждём и
    // повторяем, а не падаем.
    let listener = bind_with_patience(port, "сервер обновлений")?;
    let server = serve(listener, move |stream| {
        use std::io::{BufRead, BufReader, Write};

        {
            let mut stream = stream;
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
                Err(_) => return,
            });
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                return;
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
                return;
            }
            if let Some(data) = body {
                if stream.write_all(&data).is_err() {
                    return;
                }
            }
            stream.flush().ok();
            // Гость дочитывает тело по длине и закрывается сам; наш `FIN` нужен
            // ему только затем, чтобы не ждать своего таймаута на прощание.
            stream.shutdown(std::net::Shutdown::Write).ok();
        }
    })?;
    say!("стенд: сервер обновлений слушает {port}, каталог отдаётся гостю");
    Ok(server)
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
/// Возвращает сервер, живущий столько же, сколько сценарий (см. [`HostServer`]).
///
/// Номер порта возвращается вторым: его выбирает ядро ОС, и узнать его можно
/// только после того, как сокет занят. В команду гостю он попадает подстановкой
/// `{echo}` — этим и снята прежняя необходимость держать здесь константу,
/// которую нельзя было занять дважды.
fn start_host_echo() -> Result<(HostServer, u16)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .context("не удалось занять порт под эхо-сервер хоста")?;
    let port = listener.local_addr()?.port();
    let server = serve(listener, |stream| {
        use std::io::{Read, Write};
        {
            let mut stream = stream;
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
    })?;
    say!("стенд: эхо-сервер хоста слушает {port}");
    Ok((server, port))
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

/// Правая кнопка — тем же путём, что и левая.
///
/// Отдельная функция, а не флаг у [`press_button`]: у HMP правая кнопка это
/// другая маска, и перепутать их значит проверять контекстное меню щелчком,
/// который его не открывает.
fn press_right(qmp: Option<&mut qmp::Qmp>, hmp: &mut monitor::Monitor, down: bool) -> Result<()> {
    match qmp {
        Some(qmp) => qmp.button(qmp::BUTTON_RIGHT, down),
        None => hmp.mouse_button(if down { 2 } else { 0 }),
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
fn prepare_installed_disk(arch: Arch, release: bool) -> Result<PathBuf> {
    let disk = paths::target_disk(arch, release);
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

/// Собрать репозиторий для стенда: годный и, подкаталогом, с чужой подписью.
///
/// Версия — [`NET_UPDATE_VERSION`], и она **выше** той, что несёт обновление в
/// `/media`. Иначе сценарий зависел бы от того, шёл ли перед ним `update`:
/// после него система работает под `0.2`, и предложенная `0.2` была бы отвергнута
/// запретом отката — то есть проверка сети утонула бы в проверке версий.
fn prepare_repo(built: &build::Built) -> Result<()> {
    let (arch, release) = (built.arch, built.release);
    let dir = paths::test_repo_dir(arch, release);
    crate::repo::build(&[built], NET_UPDATE_VERSION, &dir)?;
    // Репозиторий с чужой подписью лежит **внутри** годного, подкаталогом:
    // сервер раздаёт дерево, и второй каталог рядом означал бы второй адрес в
    // настройках. Путь короткий (`/x/`) намеренно — он уезжает в гостя строкой
    // по серийной линии, а длинная строка на aarch64 теряет хвост.
    crate::repo::build_untrusted(&dir, NET_UPDATE_VERSION, arch, &dir.join("x"))?;

    // Файл под проверку объёмной загрузки. Полмегабайта, а не образ системы: по
    // TLS в отладочной сборке под эмуляцией семьдесят семь мегабайт означали бы
    // сценарий на полдня, а доказать надо другое — что через рукопожатие
    // проходит не одна запись, а сотни: со сменой счётчика записей и с
    // границами записей, которые не совпадают ни с границами кусков TCP, ни с
    // границами чтений программы.
    //
    // Содержимое считаемое, а не случайное: сценарий сверяет длину, и файл,
    // меняющийся от прогона к прогону, не дал бы ничего.
    let blob: Vec<u8> = (0..BLOB_SIZE).map(|index| (index % 251) as u8).collect();
    let path = dir.join("blob");
    std::fs::write(&path, &blob)
        .with_context(|| format!("не удалось записать {}", path.display()))?;
    Ok(())
}

/// Размер файла, который сценарий качает по HTTPS.
pub const BLOB_SIZE: usize = 512 * 1024;

/// Версия, которую предлагает сервер обновлений стенда.
///
/// Заведомо новее и установленной (`0.3.<сборка>`), и той, что лежит в `/media`
/// (`package::UPDATE_VERSION`, `0.4`): сценарии, меняющие слот, идут друг за
/// другом, и обновление обязано быть новее в любом порядке.
pub const NET_UPDATE_VERSION: &str = "0.5";

/// Положить гостю `/etc/update.cfg` с адресом сервера стенда.
///
/// Гость видит хост как `10.0.2.2` — так устроена пользовательская сеть QEMU.
/// Файл пишется на раздел состояния, то есть оказывается **правкой человека**, и
/// это часть проверки: в образе лежит эталон с другим адресом, и взять систему
/// обязана правку.
fn place_update_config(disk_path: &std::path::Path, ports: HostPorts) -> Result<()> {
    use disk::BlockDevice as _;

    let text = format!(
        "# Written by the harness: the update servers live on the host.\n\
         #\n\
         # Three of them, and the order is the point. The first is plain HTTP --\n\
         # what a machine updates over today. The second is HTTPS with a\n\
         # certificate from a root this guest has never heard of, and it has to\n\
         # be refused. The third is HTTPS with the root in /etc/ca.pem.\n\
         server=10.0.2.2\n\
         port={}\n\
         path=/\n\
         server=https://10.0.2.2:{}/\n\
         server=https://10.0.2.2:{}/\n",
        ports.repo, ports.stranger, ports.tls
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
        Ok(_) => say!("стенд: гостю положен /etc/update.cfg на 10.0.2.2:{}", ports.repo),
        // Уже лежит с прошлого прогона, и содержимое то же самое: адрес и порт
        // здесь постоянные. Перезаписи `ext2::Editor` не умеет.
        Err(ext2::Error::Exists) => say!("стенд: /etc/update.cfg у гостя уже лежит"),
        Err(err) => bail!("не удалось записать /etc/update.cfg: {err}"),
    }

    // Корень, которому гость будет доверять. Кладётся правкой человека, в
    // `/etc`, а не в образ: корень стенда выписан на машине разработчика, и в
    // выпущенном ISO ему делать нечего — ровно то же рассуждение, что у ключа
    // SSH (см. заголовок `sshkeys`).
    let ca = tlskeys::trusted()?.root_pem;
    match fs.write_file_path(&mut dev, "etc/ca.pem", ca.as_bytes(), 0o644, 0, 0) {
        Ok(_) => say!("стенд: гостю положен /etc/ca.pem с корнем стенда"),
        Err(ext2::Error::Exists) => say!("стенд: /etc/ca.pem у гостя уже лежит"),
        Err(err) => bail!("не удалось записать /etc/ca.pem: {err}"),
    }

    fs.flush_everywhere(&mut dev)
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
    ports: HostPorts,
) -> Result<Vec<Drive>> {
    let drives = match scenario.target {
        Target::Live => vec![Drive::HostDirectory(qemu::prepare_esp(built)?)],
        Target::Image => vec![Drive::Image(image::build(built, image::Kind::System)?)],
        Target::Installer => vec![
            // Порядок важен: прошивка перебирает носители в порядке подключения,
            // и загрузочный раздел на этот момент есть только у первого.
            Drive::Image(image::build(built, image::Kind::Installer)?),
            Drive::Image(image::prepare_target(arch, built.release, TARGET_DISK_MIB, true)?),
        ],
        Target::Iso => vec![Drive::Cdrom(image::build_iso(built, image::Kind::System)?)],
        Target::Installed => {
            let disk = prepare_installed_disk(arch, built.release)?;
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
                prepare_repo(built)?;
                place_update_config(&disk, ports)?;
            }
            vec![Drive::Image(disk)]
        }
        // Порядок обязателен: прошивка грузится с первого носителя, а второй —
        // тот, ради которого сценарий существует.
        Target::LiveAndDisk => vec![
            Drive::HostDirectory(qemu::prepare_esp(built)?),
            Drive::Image(prepare_installed_disk(arch, built.release)?),
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
