// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Фаззинг разборщиков: движок, образцы и цели.
//!
//! # Что именно проверяется
//!
//! Одно утверждение, и оно про **весь** код, читающий чужие байты: разбор
//! любой последовательности байт либо возвращает ответ, либо возвращает
//! ошибку. Он не паникует, не уходит в бесконечный цикл и не читает за
//! границей. Чужие байты — это диск, вставленный человеком, сертификат с
//! чужого сервера, дескриптор чужой мыши, пакет, скачанный по сети, и сборка
//! .NET, собранная неизвестно чем. Ни один из этих источников не обещал нам
//! правильного формата.
//!
//! Проверить это примерами нельзя: примеры пишет тот же, кто писал разборщик, и
//! он проверяет случаи, которые придумал. Фаззер придумывает за него — портит
//! правильные байты и смотрит, не обиделся ли разбор. Здесь это дёшево, потому
//! что разборщики живут в отдельных крейтах и вызываются с хоста, где есть
//! `cargo test`: цена проверки — секунды, а не прогон в эмуляторе.
//!
//! # Почему свой движок, а не `cargo-fuzz`
//!
//! `cargo-fuzz` — это libFuzzer, отдельный инструмент, который надо поставить,
//! и покрытие через инструментирование, которого нет на всякой машине (на
//! Windows с MSVC это отдельное приключение). Здесь всё внутри репозитория и
//! запускается одной командой, которая уже есть у всех: `cargo test`.
//! Воспроизводимость важнее скорости поиска — каждое падение называется
//! **зерном и номером итерации**, по которым тот же самый вход получается
//! снова, на любой машине, без файла-образца.
//!
//! Цена решения названа честно: без обратной связи по покрытию этот фаззер
//! слепой — он портит байты, но не знает, добрался ли до новой ветки кода.
//! Для формата, у которого первые же поля отвергают мусор (а такие здесь
//! все), слепая порча находит много, и находила; для поиска в глубине
//! разбора нужен libFuzzer, и это честный предел этого крейта.
//!
//! # Зависания
//!
//! Паника ловится [`std::panic::catch_unwind`]; зависание не ловится ничем,
//! поэтому цель работает в отдельном потоке, а главный смотрит на счётчик
//! итераций. Счётчик перестал расти — значит вход, лежащий рядом со
//! счётчиком, зациклил разбор, и он же попадает в отчёт. Поток при этом
//! остаётся висеть: прервать его нечем, и лучше честный отчёт с повисшим
//! потоком, чем молчание.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub mod seeds;
pub mod targets;
pub mod view;

pub use targets::{TARGETS, Target};

/// Генератор псевдослучайных чисел: `splitmix64`.
///
/// Свой и простой — здесь не нужна стойкость, нужна **воспроизводимость**:
/// одно и то же зерно обязано давать одну и ту же последовательность на любой
/// машине и в любой версии компилятора. Стандартной библиотеке такого обещания
/// давать нечем (в `std` генератора нет вовсе), а чужой крейт вправе поменять
/// алгоритм в новой версии — и отчёт «зерно 7, итерация 912» перестал бы
/// значить что-либо.
pub struct Rng(u64);

impl Rng {
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        // Ноль — законное зерно, но у splitmix64 из нуля выходит нулевое
        // первое значение; сдвиг убирает этот единственный неудачный случай.
        Self(seed ^ 0x9e37_79b9_7f4a_7c15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Число в `[0, bound)`. `bound` равный нулю даёт ноль.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        // Остаток, а не отбрасывание: смещение на краю диапазона здесь
        // безразлично, а предсказуемость числа обращений к генератору — нет.
        (self.next_u64() % bound as u64) as usize
    }

    pub fn byte(&mut self) -> u8 {
        (self.next_u64() >> 24) as u8
    }

    /// Состояние генератора — то, из чего его можно продолжить с того же места.
    ///
    /// Нужно затем, чтобы **не копировать вход**. Фаззер публикует для отчёта
    /// не сами байты, а рецепт: номер образца и состояние генератора перед
    /// порчей. По рецепту вход собирается заново — порча детерминирована, —
    /// и на горячем пути не выделяется ничего. Разница видна на btrfs, где
    /// образец весит сто двадцать восемь мегабайт.
    #[must_use]
    pub const fn state(&self) -> u64 {
        self.0
    }

    /// Продолжить с сохранённого состояния.
    #[must_use]
    pub const fn from_state(state: u64) -> Self {
        Self(state)
    }
}

/// Значения, на которых форматы ломаются чаще прочих: границы типов, нули,
/// «все единицы» и небольшие числа около них.
///
/// Слепая порча случайным байтом до такого доходит редко: вероятность собрать
/// `0xffff_ffff` в поле длины из четырёх случайных байт — одна на четыре
/// миллиарда. А именно это поле и переполняет сложение.
const INTERESTING: [u8; 12] = [0x00, 0x01, 0x02, 0x7f, 0x80, 0x81, 0xfe, 0xff, 0x20, 0x0a, 0x3d, 0x2f];

/// Испортить `bytes` от одного до восьми раз.
///
/// Портится только начало — первые `hot` байт. Для сертификата или дескриптора
/// это весь вход, а для тома ext2 — область метаданных: портить середину
/// двухмегабайтного образа, который разборщик и не читает, значит потратить
/// итерацию впустую.
pub fn mutate(rng: &mut Rng, bytes: &mut [u8], hot: usize) {
    let hot = hot.min(bytes.len());
    if hot == 0 {
        return;
    }
    let rounds = 1 + rng.below(8);
    for _ in 0..rounds {
        match rng.below(6) {
            // Перевернуть один бит: самая мелкая порча, какая бывает, и та,
            // что чаще всего проходит проверку формата и ломается глубже.
            0 => {
                let at = rng.below(hot);
                bytes[at] ^= 1 << rng.below(8);
            }
            // Случайный байт.
            1 => {
                let at = rng.below(hot);
                bytes[at] = rng.byte();
            }
            // Значение из списка интересных.
            2 => {
                let at = rng.below(hot);
                bytes[at] = INTERESTING[rng.below(INTERESTING.len())];
            }
            // Залить кусок одним байтом: так получаются «все единицы» в поле
            // длины, до которых порча по одному байту не доходит.
            3 => {
                let at = rng.below(hot);
                let len = (1 + rng.below(16)).min(hot - at);
                let value = INTERESTING[rng.below(INTERESTING.len())];
                bytes[at..at + len].fill(value);
            }
            // Переставить кусок внутри входа: поле уезжает туда, где его ждут
            // с другим смыслом.
            4 => {
                let len = 1 + rng.below(32);
                if len <= hot {
                    let from = rng.below(hot - len + 1);
                    let to = rng.below(hot - len + 1);
                    let chunk: Vec<u8> = bytes[from..from + len].to_vec();
                    bytes[to..to + len].copy_from_slice(&chunk);
                }
            }
            // Поменять местами два байта.
            _ => {
                let a = rng.below(hot);
                let b = rng.below(hot);
                bytes.swap(a, b);
            }
        }
    }
}

/// Чем кончилась порча одного входа.
pub enum Failure {
    /// Разбор паниковал. Строка — сообщение паники.
    Panic(String),
    /// Разбор не вернулся за отведённое время.
    Hang(Duration),
}

/// Отчёт о падении: всё, что нужно, чтобы получить тот же вход снова.
pub struct Report {
    pub target: &'static str,
    pub seed: u64,
    pub iteration: u64,
    pub input: Vec<u8>,
    pub failure: Failure,
}

impl Report {
    /// Текст отчёта, который человек читает и по которому повторяет прогон.
    #[must_use]
    pub fn describe(&self) -> String {
        let what = match &self.failure {
            Failure::Panic(message) => format!("разбор паниковал: {message}"),
            Failure::Hang(after) => {
                format!("разбор не вернулся за {} мс", after.as_millis())
            }
        };
        // Первые байты входа — глазами: часто по ним видно, какое поле
        // испорчено, и разбираться дальше не нужно.
        let head: Vec<String> =
            self.input.iter().take(48).map(|byte| format!("{byte:02x}")).collect();
        let tail = if self.input.len() > 48 { " ..." } else { "" };
        format!(
            "цель {}: {what}\n\
             вход {} байт, зерно {}, итерация {}\n\
             начало: {}{tail}\n\
             повторить: cargo xtask fuzz --target {} --seed {} --iterations {}",
            self.target,
            self.input.len(),
            self.seed,
            self.iteration,
            head.join(" "),
            self.target,
            self.seed,
            self.iteration + 1,
        )
    }
}

/// Сколько ждать один вход, прежде чем считать разбор зависшим.
///
/// Полторы секунды на разбор нескольких килобайт — это в сотни раз больше, чем
/// нужно самому медленному из здешних разборщиков (`fsck` по тому btrfs), и
/// запас взят под нагруженный хост: на машине, занятой сборкой, поток
/// просыпается не сразу, и сторож, срабатывающий от этого, обвинял бы
/// исправный код.
const PATIENCE: Duration = Duration::from_millis(1500);

/// Прогнать одну цель.
///
/// Возвращает `None`, если все `iterations` итераций прошли без падений.
pub fn run(target: &'static Target, seed: u64, iterations: u64) -> Option<Report> {
    let seeds = (target.seeds)();
    assert!(!seeds.is_empty(), "у цели {} нет ни одного образца", target.name);

    let seeds = Arc::new(seeds);
    // Рецепт входа, который цель разбирает **сейчас**: номер образца и
    // состояние генератора перед порчей. Главный поток читает его, когда
    // счётчик перестаёт расти, и собирает по нему те самые байты — только так
    // и узнать, на чём заело. Рецепт, а не байты, потому что публикация идёт
    // на каждой итерации, а сборка — один раз, при падении.
    let current: Arc<Mutex<(usize, u64)>> = Arc::new(Mutex::new((0, 0)));
    let done = Arc::new(AtomicU64::new(0));
    // Паника внутри цели: сообщение забирается у обработчика, иначе
    // `catch_unwind` отдаёт `Box<dyn Any>`, из которого текст достаётся не
    // всегда, и отчёт вышел бы без самого главного.
    let message: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let worker = {
        let current = Arc::clone(&current);
        let done = Arc::clone(&done);
        let message = Arc::clone(&message);
        let seeds = Arc::clone(&seeds);
        std::thread::spawn(move || -> Option<(u64, Vec<u8>, String)> {
            // Свой обработчик паники на время прогона: без него каждая
            // найденная паника печатала бы простыню в stderr, а найденная
            // паника — это ожидаемый исход, а не поломка прогона.
            {
                let message = Arc::clone(&message);
                // Прежний обработчик выбрасывается: он печатает в stderr, а
                // найденная паника — это ожидаемый исход прогона, а не его
                // поломка. Печатать её простынёй значило бы тонуть в ней при
                // долгом прогоне.
                let _ = panic::take_hook();
                panic::set_hook(Box::new(move |info| {
                    let text = info
                        .payload()
                        .downcast_ref::<&str>()
                        .map(|text| (*text).to_string())
                        .or_else(|| info.payload().downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| String::from("без сообщения"));
                    let at = info
                        .location()
                        .map(|at| format!(" ({}:{})", at.file(), at.line()))
                        .unwrap_or_default();
                    *message.lock().expect("обработчик паники") = Some(format!("{text}{at}"));
                }));
            }

            // Рабочие копии образцов: порча идёт **по месту**, а после
            // итерации испорченное окно возвращается из целого образца. Копия
            // на каждую итерацию стоила бы образа целиком, а наименьший том
            // btrfs — сто двадцать восемь мегабайт: цель шла двадцать четыре
            // входа в секунду вместо тысяч, и проверялось этим `memcpy`, а не
            // разбор.
            let mut work: Vec<Vec<u8>> = seeds.as_ref().clone();
            let mut rng = Rng::new(seed);
            let mut outcome = None;
            for iteration in 0..iterations {
                let which = rng.below(seeds.len());
                let hot = target.hot_bytes.min(seeds[which].len());
                // Рецепт публикуется **до** порчи: из этого состояния она и
                // повторится.
                *current.lock().expect("рецепт входа") = (which, rng.state());
                let input = &mut work[which];
                mutate(&mut rng, input, hot);

                let call = panic::catch_unwind(AssertUnwindSafe(|| (target.run)(input)));
                if call.is_err() {
                    let text = message
                        .lock()
                        .expect("сообщение паники")
                        .take()
                        .unwrap_or_else(|| String::from("без сообщения"));
                    // Вход собирается по рецепту: те же байты, потому что
                    // порча из того же состояния генератора повторяется в
                    // точности.
                    outcome = Some((iteration, work[which][..].to_vec(), text));
                    break;
                }
                // Испорченное окно возвращается на место: иначе следующая
                // итерация начиналась бы с уже испорченного образца, и «зерно и
                // номер» перестали бы воспроизводить вход.
                work[which][..hot].copy_from_slice(&seeds[which][..hot]);
                done.store(iteration + 1, Ordering::Release);
            }
            let _ = panic::take_hook();
            outcome
        })
    };

    // Сторож: пока счётчик растёт, ждём. Перестал — смотрим на вход.
    let mut seen = 0;
    let mut since = Instant::now();
    loop {
        if worker.is_finished() {
            break;
        }
        let now = done.load(Ordering::Acquire);
        if now != seen {
            seen = now;
            since = Instant::now();
        } else if since.elapsed() > PATIENCE {
            let (which, state) = *current.lock().expect("рецепт входа");
            let input = rebuild(&seeds, target.hot_bytes, which, state);
            return Some(Report {
                target: target.name,
                seed,
                iteration: now,
                input,
                failure: Failure::Hang(since.elapsed()),
            });
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    match worker.join() {
        Ok(Some((iteration, input, text))) => Some(Report {
            target: target.name,
            seed,
            iteration,
            input,
            failure: Failure::Panic(text),
        }),
        Ok(None) => None,
        // Поток развалился не на цели, а на самом движке — молчать об этом
        // нельзя, иначе прогон выглядел бы зелёным.
        Err(_) => {
            let (which, state) = *current.lock().expect("рецепт входа");
            Some(Report {
                target: target.name,
                seed,
                iteration: done.load(Ordering::Acquire),
                input: rebuild(&seeds, target.hot_bytes, which, state),
                failure: Failure::Panic(String::from("поток фаззера развалился сам")),
            })
        }
    }
}

/// Собрать вход по рецепту: образец под номером `which`, испорченный из
/// состояния генератора `state`.
fn rebuild(seeds: &[Vec<u8>], hot_bytes: usize, which: usize, state: u64) -> Vec<u8> {
    let Some(seed) = seeds.get(which) else {
        return Vec::new();
    };
    let mut input = seed.clone();
    let hot = hot_bytes.min(input.len());
    mutate(&mut Rng::from_state(state), &mut input, hot);
    input
}

/// Найти цель по имени.
#[must_use]
pub fn target(name: &str) -> Option<&'static Target> {
    TARGETS.iter().find(|target| target.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Зерно определяет последовательность целиком. Числа выписаны рукой: тест
    /// существует ровно затем, чтобы смена алгоритма генератора не прошла
    /// молча — иначе «зерно 7, итерация 912» в старом отчёте перестало бы
    /// воспроизводить тот вход.
    #[test]
    fn the_generator_is_reproducible() {
        let mut rng = Rng::new(1);
        assert_eq!(rng.next_u64(), 16_834_447_057_089_888_969);
        assert_eq!(rng.next_u64(), 4_048_727_598_324_417_001);
        let mut again = Rng::new(1);
        assert_eq!(again.next_u64(), 16_834_447_057_089_888_969);
        // Разные зёрна — разные последовательности.
        assert_eq!(Rng::new(2).next_u64(), 13_819_372_491_320_860_226);
    }

    /// Порча не выходит за горячую область и не меняет длину входа. Второе
    /// важнее: цели, читающие том через блочное устройство, получают образ
    /// целым сектором, и внезапно укороченный вход проверял бы не разбор, а
    /// создание устройства.
    #[test]
    fn mutation_stays_inside_the_hot_window() {
        let mut rng = Rng::new(3);
        for _ in 0..200 {
            let mut bytes = vec![0u8; 64];
            mutate(&mut rng, &mut bytes, 16);
            assert_eq!(bytes.len(), 64);
            assert!(bytes[16..].iter().all(|byte| *byte == 0), "испорчено за границей");
        }
    }

    /// Пустой вход и нулевая область порчи не роняют сам движок.
    #[test]
    fn mutation_survives_an_empty_input() {
        let mut rng = Rng::new(4);
        mutate(&mut rng, &mut [], 8);
        mutate(&mut rng, &mut [1, 2, 3], 0);
    }

    /// Паника внутри цели становится отчётом, а не падением прогона.
    #[test]
    fn a_panic_becomes_a_report() {
        static TARGET: Target = Target {
            name: "self-test-panic",
            about: "Цель, которая паникует нарочно: проверка самого движка.",
            seeds: || vec![vec![0u8; 4]],
            hot_bytes: 4,
            run: |_| panic!("нарочно"),
        };
        let report = run(&TARGET, 1, 4).expect("паника обязана стать отчётом");
        assert_eq!(report.target, "self-test-panic");
        assert_eq!(report.iteration, 0);
        match &report.failure {
            Failure::Panic(text) => assert!(text.contains("нарочно"), "{text}"),
            Failure::Hang(_) => panic!("это была паника, а не зависание"),
        }
        assert!(report.describe().contains("cargo xtask fuzz"));
    }

    /// Зависание внутри цели тоже становится отчётом — по счётчику итераций.
    /// Тест платит за себя [`PATIENCE`]: это единственная проверка в дереве,
    /// которая обязана ждать по-настоящему.
    #[test]
    fn a_hang_becomes_a_report() {
        static TARGET: Target = Target {
            name: "self-test-hang",
            about: "Цель, которая не возвращается: проверка сторожа.",
            seeds: || vec![vec![7u8; 4]],
            hot_bytes: 4,
            run: |_| {
                loop {
                    std::thread::sleep(Duration::from_millis(50));
                }
            },
        };
        let report = run(&TARGET, 1, 4).expect("зависание обязано стать отчётом");
        match &report.failure {
            Failure::Hang(_) => {}
            Failure::Panic(text) => panic!("это было зависание, а не паника: {text}"),
        }
        // Вход в отчёте — тот самый, на котором заело.
        assert_eq!(report.input.len(), 4);
    }

    /// Каждая цель называется по-разному и приносит хотя бы один образец.
    /// Цель без образцов молча проверяла бы пустой вход.
    #[test]
    fn every_target_has_a_unique_name_and_seeds() {
        for target in TARGETS {
            let seeds = (target.seeds)();
            assert!(!seeds.is_empty(), "у цели {} нет образцов", target.name);
            assert!(target.hot_bytes > 0, "у цели {} нулевая область порчи", target.name);
            assert_eq!(
                TARGETS.iter().filter(|other| other.name == target.name).count(),
                1,
                "имя цели {} занято дважды",
                target.name
            );
        }
    }
}
