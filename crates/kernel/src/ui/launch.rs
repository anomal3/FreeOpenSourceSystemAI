//! Заставка запуска программы Windows (21.11, пожелание Романа 3).
//!
//! Игра под Proton открывает первое окно через десять-сорок секунд после
//! двойного щелчка: Wine поднимает префикс, `wineserver`, службы, DXVK
//! собирает конвейеры. Всё это время на экране не происходило ничего, и
//! человек щёлкал второй раз. Теперь посреди основного монитора — маленькая
//! карточка: значок программы (из её `.exe`, если он там есть), имя,
//! «Запуск…» и бегущая полоса без процентов. Она уходит сама, когда у
//! программы появилось окно; кончилась программа с ошибкой, не открыв окна, —
//! карточка говорит об этом и даёт «Подробнее» (журнал событий).
//!
//! # Почему в ядре, а не отдельной программой
//!
//! Чьё окно открылось, знает только стол, а чья это программа — только
//! таблица процессов: окно игры принадлежит не тому `bash`, которого
//! запустили, а его правнуку `wine64`. Общее у них — задание
//! (`Program::linux_job`, P4): номер процесса, с которого начался запуск из
//! системы, его наследуют все потомки. Запуск замечается в одном месте —
//! в `user::spawn_*` (меню «Пуск», ярлык на столе, двойной щелчок в «Файлах»,
//! `run` оболочки идут через него), окно — в `ui::open_window_framed`.
//!
//! # Замки
//!
//! Процессы и задачи спрашиваются **вне** замка стола (как путь программы в
//! `open_window_framed`): поток программы, держащий замок своего процесса,
//! может ждать замка стола. Поэтому состояние запусков — в своём замке
//! [`LAUNCHES`]: его обновляют [`note_spawn`], [`window_opened`] и [`poll`]
//! (из `ui::tick`), а стол только читает и рисует ([`Splash::advance`]).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::image::Image;
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};

use crate::sync::SpinLock;

/// Сколько ждать первого окна, прежде чем тихо убрать заставку: первый
/// запуск в новом префиксе под эмулятором — минута-две.
const TIMEOUT_MS: u64 = 180_000;

/// Шаг кадра полосы (мс): 30 кадров в секунду хватает глазу, а стол не
/// будится чаще, чем нужно.
const FRAME_MS: u64 = 33;

/// Период пробега полосы (мс).
const SWEEP_MS: u64 = 1_400;

/// Что с запуском.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Phase {
    /// Ждём первого окна.
    Starting,
    /// Программа кончилась, не открыв окна: текст причины.
    Failed(String),
    /// Окно открылось, программа тихо кончилась или вышел срок — убрать.
    Done,
}

/// Откуда взять значок.
#[derive(Clone)]
enum Source {
    /// `.exe` — значок из его ресурсов.
    Exe(String),
    /// Ярлык winemenubuilder: каталог ярлыков и поле `Icon=`.
    Shortcut(String, String),
    /// FIMG пакета (`/opt/<имя>/icon.fimg`).
    Fimg(String),
    None,
}

struct Launch {
    /// Задача, которую запустили; она же — задание всех её потомков.
    root: u32,
    name: String,
    at_ms: u64,
    phase: Phase,
    source: Source,
    /// Значок уже искали (найден или нет).
    looked: bool,
    icon: Option<Arc<Image>>,
    /// Человек закрыл карточку крестиком: запуск следим дальше, но не
    /// показываем.
    hidden: bool,
}

/// Запуски, за которыми следим.
static LAUNCHES: SpinLock<Vec<Launch>> = SpinLock::new(Vec::new());

/// Раскодировать `%20` и `%25` (так пути с пробелом едут в строке запуска).
fn unpct(text: &str) -> String {
    text.replace("%20", " ").replace("%25", "%")
}

/// Имя файла без каталога и расширения.
fn stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let name = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    String::from(name)
}

/// Поле `key=` файла `.desktop`.
fn desktop_field(file: &str, key: &str) -> Option<String> {
    let Some(Ok((bytes, _))) = crate::fs::read(file, 16 * 1024) else {
        return None;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .find_map(|line| line.strip_prefix(key).and_then(|rest| rest.strip_prefix('=')))
        .map(|value| String::from(value.trim()))
}

/// Запуск программы Windows ли это: имя и откуда значок. `None` — нет
/// (служебные вызовы сценариев, проверки GPU).
fn classify(line: &str) -> Option<(String, Source)> {
    let windows = ["/opt/proton/bin/fossai-proton", "/opt/wine/bin/fossai-wine", "/opt/tmnf/bin/tmnf"];
    if !windows.iter().any(|script| line.contains(script)) {
        return None;
    }
    // Проверки и пробы — без заставки: окна у них нет или его меряют.
    for quiet in ["gametest", "probe", "vkcube", "--no-splash", "--help", "--version", "--drives", "--prefix-only"] {
        if line.contains(quiet) {
            return None;
        }
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    if line.contains("/opt/tmnf/bin/tmnf") {
        return Some((String::from("TrackMania Nations Forever"), Source::Fimg(String::from("/opt/tmnf/icon.fimg"))));
    }
    for word in &words {
        if let Some(path) = word.strip_prefix("--open=") {
            let path = unpct(path);
            return Some((stem(&path), Source::Exe(path)));
        }
        if let Some(file) = word.strip_prefix("--shortcut=") {
            let file = unpct(file);
            let name = desktop_field(&file, "Name").unwrap_or_else(|| stem(&file));
            let dir = file.rsplit_once('/').map_or_else(String::new, |(dir, _)| String::from(dir));
            let icon = desktop_field(&file, "Icon").unwrap_or_default();
            return Some((name, Source::Shortcut(dir, icon)));
        }
        if word.starts_with("--d2r") {
            return Some((String::from("Diablo II: Resurrected"), Source::None));
        }
    }
    // Путь к программе Windows среди аргументов (`fossai-wine game.exe`).
    if let Some(exe) = words.iter().skip(2).find(|word| {
        let lower = word.to_ascii_lowercase();
        [".exe", ".msi", ".lnk", ".bat"].iter().any(|ext| lower.ends_with(ext))
    }) {
        let path = unpct(exe);
        return Some((stem(&path), Source::Exe(path)));
    }
    // Без аргументов — «Программы Windows» (пакет proton: настройка префикса).
    let args = words.iter().skip_while(|word| !windows.iter().any(|script| word.ends_with(script))).count();
    if args <= 1 {
        let caption = if line.contains("fossai-wine") { "Wine" } else { "Proton" };
        return Some((String::from(caption), Source::None));
    }
    None
}

/// Задачу `task` запустили строкой `line` (зовёт `user::spawn_*`). Запуск
/// программы Windows заводит заставку. Только свой замок: зовут и из-под
/// замка стола.
pub fn note_spawn(task: u32, line: &str) {
    if theme::is_mobile() {
        return;
    }
    let Some((name, source)) = classify(line) else {
        return;
    };
    crate::kprintln!("  launch      : '{name}' starting as #{task} -- splash shown");
    // A perf-diag block now and another when the window opens (24.11,
    // hw-log17: the 30 s blocks skipped the whole D2R launch).
    #[cfg(target_arch = "x86_64")]
    crate::linux::perfdiag::request_now();
    let mut launches = LAUNCHES.lock();
    if launches.try_reserve(1).is_err() {
        return;
    }
    launches.push(Launch {
        root: task,
        name,
        at_ms: crate::time::uptime_ms(),
        phase: Phase::Starting,
        source,
        looked: false,
        icon: None,
        hidden: false,
    });
    drop(launches);
    super::set_animating(true);
}

/// Задача `task` открыла окно (зовёт `ui::open_window_framed` вне замка
/// стола); `job` — задание её процесса Linux, `None` — программа не Linux.
pub fn window_opened(task: u32, job: Option<u32>) {
    let Some(job) = job else {
        return;
    };
    let now = crate::time::uptime_ms();
    let mut launches = LAUNCHES.lock();
    let mut matched = launches.iter().position(|launch| launch.phase == Phase::Starting && launch.root == job);
    // Окно программы Linux, чьё задание не наше, а ждёт ровно один запуск,
    // чей корень уже кончился (запускающий процесс отдал игру и вышел),
    // — это оно.
    if matched.is_none() {
        let waiting: Vec<usize> = (0..launches.len()).filter(|at| launches[*at].phase == Phase::Starting).collect();
        if waiting.len() == 1 && crate::sched::lookup(crate::sched::TaskId::new(launches[waiting[0]].root)).is_none() {
            matched = Some(waiting[0]);
        }
    }
    if let Some(at) = matched {
        let launch = &mut launches[at];
        launch.phase = Phase::Done;
        crate::kprintln!(
            "  launch      : '{}' opened a window (#{task}) after {} ms -- splash closed",
            launch.name,
            now.saturating_sub(launch.at_ms)
        );
        drop(launches);
        #[cfg(target_arch = "x86_64")]
        crate::linux::perfdiag::request_now();
    }
}

/// Раз в полсекунды (`ui::tick`, вне замка стола): кончилась ли программа,
/// не вышел ли срок, найти значки.
pub fn poll() {
    let jobs: Vec<(u32, Phase, bool)> = LAUNCHES.lock().iter().map(|launch| (launch.root, launch.phase.clone(), launch.looked)).collect();
    if jobs.is_empty() {
        return;
    }
    let now = crate::time::uptime_ms();
    // Icons on their own task (24.11.2026, hw-log17 "the UI freezes when a
    // game starts"): reading the PE resources of a game `.exe` on a USB disk
    // that Wine is streaming the game from took the desktop-driving task
    // along, and the splash and every frame waited for it.
    if jobs.iter().any(|(_, _, looked)| !looked) && !ICON_WORKER.swap(true, core::sync::atomic::Ordering::AcqRel) {
        if crate::sched::spawn("launch-icons", icon_worker).is_err() {
            ICON_WORKER.store(false, core::sync::atomic::Ordering::Release);
        }
    }
    for (root, phase, _) in jobs {
        if phase != Phase::Starting {
            continue;
        }
        let id = crate::sched::TaskId::new(root);
        let result = crate::sched::result_of(id);
        let gone = result.is_some() || crate::sched::lookup(id).is_none();
        let next = if gone {
            match result {
                Some(code) if code != 0 => Some(Phase::Failed(alloc::format!("Завершилась с кодом {code}, не открыв окна."))),
                // Запускающий процесс вышел: ждём, пока живы его потомки.
                _ if crate::user::job_alive(root) => None,
                _ => Some(Phase::Done),
            }
        } else {
            None
        };
        let mut launches = LAUNCHES.lock();
        let Some(launch) = launches.iter_mut().find(|launch| launch.root == root) else {
            continue;
        };
        if launch.phase != Phase::Starting {
            continue;
        }
        match next {
            Some(Phase::Failed(text)) => {
                crate::kprintln!("  launch      : '{}' failed: {text}", launch.name);
                launch.phase = Phase::Failed(text);
                launch.hidden = false;
            }
            Some(Phase::Done) => {
                crate::kprintln!("  launch      : '{}' ended without a window -- splash closed", launch.name);
                launch.phase = Phase::Done;
            }
            _ if now.saturating_sub(launch.at_ms) > TIMEOUT_MS => {
                crate::kprintln!("  launch      : '{}' no window after {} s -- splash closed", launch.name, TIMEOUT_MS / 1000);
                launch.phase = Phase::Done;
            }
            _ => {}
        }
    }
}

/// A `launch-icons` task is running (see [`icon_worker`]).
static ICON_WORKER: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Find the icons of the launches that have none yet, one by one, outside
/// the desktop and its driving task; the splash shows the icon on its next
/// frame.
fn icon_worker() {
    loop {
        let next = LAUNCHES.lock().iter_mut().find(|launch| !launch.looked).map(|launch| {
            launch.looked = true;
            (launch.root, launch.source.clone())
        });
        let Some((root, source)) = next else { break };
        let started = crate::time::uptime_ms();
        let icon = find_icon(&source);
        let spent = crate::time::uptime_ms().saturating_sub(started);
        if spent >= 50 {
            crate::kprintln!("  launch      : icon of #{root} read in {spent} ms (outside the desktop)");
        }
        if let Some(launch) = LAUNCHES.lock().iter_mut().find(|launch| launch.root == root) {
            launch.icon = icon;
        }
    }
    ICON_WORKER.store(false, core::sync::atomic::Ordering::Release);
}

/// Значок запуска.
fn find_icon(source: &Source) -> Option<Arc<Image>> {
    match source {
        // Ярлык Windows — значок его программы (21.11, `ui::winapps`).
        Source::Exe(path) if path.to_ascii_lowercase().ends_with(".lnk") => {
            let icon = super::winapps::icon_of_lnk(path);
            (!icon.is_empty()).then(|| super::appicon::image(&icon)).flatten()
        }
        Source::Exe(path) => exe_icon(path).map(Arc::new),
        Source::Shortcut(dir, icon) => {
            let fimg = super::panel::shortcut_icon(dir, icon);
            (!fimg.is_empty()).then(|| super::appicon::image(&fimg)).flatten()
        }
        Source::Fimg(path) => super::appicon::image(path),
        Source::None => None,
    }
}

// ── Значок из ресурсов PE ───────────────────────────────────────────────────

/// Значок `.exe`: группа значков (`RT_GROUP_ICON`) — первая, из её размеров
/// — ближайший к 48 точкам в формате DIB (PNG-значки 256×256 ядро не
/// разбирает: декодера PNG в нём нет, см. `mini_ui::image`). Файл читается
/// кусками по смещениям — игра весит гигабайты.
pub fn exe_icon(path: &str) -> Option<Image> {
    icon_image(path, 0)
}

/// Значок из файла `path` с номером `index`, как в поле значка ярлыка Windows
/// (21.11, `ui::winapps`): `.ico` — лучший размер файла; `.exe`/`.dll` —
/// группа значков номер `index` по порядку (отрицательный — номер ресурса).
pub fn icon_image(path: &str, index: i32) -> Option<Image> {
    let ico = path.to_ascii_lowercase().ends_with(".ico");
    crate::fs::with_fs(path, |fs, inner| {
        let node = fs.resolve(inner).ok()?;
        let read = |offset: u64, len: usize| -> Option<Vec<u8>> {
            if len > 4 * 1024 * 1024 {
                return None;
            }
            let mut buf = Vec::new();
            buf.try_reserve_exact(len).ok()?;
            buf.resize(len, 0);
            let got = node.read_at(offset, &mut buf).ok()?;
            (got == len).then_some(buf)
        };
        if ico { ico_icon(&read) } else { pe_icon(&read, index) }
    })
    .flatten()
}

/// Файл `.ico`: заголовок, записи по 16 байт (сторона, бит на точку, размер,
/// смещение) — DIB того же вида, что в ресурсах PE.
fn ico_icon(read: &dyn Fn(u64, usize) -> Option<Vec<u8>>) -> Option<Image> {
    let head = read(0, 6)?;
    if u16_at(&head, 0)? != 0 || u16_at(&head, 2)? != 1 {
        return None;
    }
    let count = usize::from(u16_at(&head, 4)?).min(64);
    let table = read(6, count * 16)?;
    let mut sizes: Vec<(u32, u16, u32, u32)> = (0..count)
        .filter_map(|i| {
            let e = i * 16;
            let side = match *table.get(e)? {
                0 => 256,
                w => u32::from(w),
            };
            Some((side, u16_at(&table, e + 6)?, u32_at(&table, e + 8)?, u32_at(&table, e + 12)?))
        })
        .collect();
    sizes.sort_by_key(|(side, bits, _, _)| best_size_key(*side, *bits));
    sizes.into_iter().find_map(|(_, _, size, offset)| {
        if size as usize > 1024 * 1024 {
            return None;
        }
        dib_to_image(&read(u64::from(offset), size as usize)?)
    })
}

/// Порядок размеров значка: ближе к 48 и глубже цветом — первым, больше 64 —
/// в конце (PNG 256×256 ядро не разбирает).
fn best_size_key(side: u32, bits: u16) -> (u32, u32) {
    ((side as i32 - 48).unsigned_abs() + if side > 64 { 1000 } else { 0 }, u32::from(32 - bits.min(32)))
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

/// Разбор PE: заголовки, секции, каталог ресурсов.
fn pe_icon(read: &dyn Fn(u64, usize) -> Option<Vec<u8>>, group_index: i32) -> Option<Image> {
    let dos = read(0, 64)?;
    if dos.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u64::from(u32_at(&dos, 0x3C)?);
    let head = read(pe, 24)?;
    if head.get(..4)? != b"PE\0\0" {
        return None;
    }
    let sections = usize::from(u16_at(&head, 6)?).min(96);
    let opt_size = usize::from(u16_at(&head, 20)?);
    let opt = read(pe + 24, opt_size)?;
    let dirs = match u16_at(&opt, 0)? {
        0x10b => 96,
        0x20b => 112,
        _ => return None,
    };
    let rsrc_rva = u32_at(&opt, dirs + 2 * 8)?;
    if rsrc_rva == 0 {
        return None;
    }
    let table = read(pe + 24 + opt_size as u64, sections * 40)?;
    // RVA → смещение в файле.
    let to_file = |rva: u32| -> Option<u64> {
        (0..sections).find_map(|index| {
            let s = index * 40;
            let size = u32_at(&table, s + 8)?.max(u32_at(&table, s + 16)?);
            let va = u32_at(&table, s + 12)?;
            let raw = u32_at(&table, s + 20)?;
            (rva >= va && rva < va.saturating_add(size)).then(|| u64::from(raw) + u64::from(rva - va))
        })
    };
    let base = to_file(rsrc_rva)?;
    // Каталог ресурсов: записи (имя/номер, смещение) — смещения от начала
    // секции ресурсов; старший бит — подкаталог.
    let entries = |offset: u32| -> Option<Vec<(u32, u32)>> {
        let dir = read(base + u64::from(offset), 16)?;
        let count = (usize::from(u16_at(&dir, 12)?) + usize::from(u16_at(&dir, 14)?)).min(4096);
        let raw = read(base + u64::from(offset) + 16, count * 8)?;
        Some((0..count).filter_map(|i| Some((u32_at(&raw, i * 8)?, u32_at(&raw, i * 8 + 4)?))).collect())
    };
    // Данные первого языка первого имени ресурса типа `kind` с номером `id`
    // (`None` — любой).
    // `nth` — номер по порядку среди имён (для группы значков ярлыка).
    let data_nth = |kind: u32, id: Option<u32>, nth: usize| -> Option<Vec<u8>> {
        let (_, types) = entries(0)?.into_iter().find(|(name, _)| *name == kind)?;
        let names = entries(types & 0x7FFF_FFFF)?;
        let (_, langs) = names.into_iter().filter(|(name, _)| id.is_none_or(|id| *name == id)).nth(nth)?;
        let mut node = langs;
        // До листа — не глубже трёх уровней.
        for _ in 0..2 {
            if node & 0x8000_0000 == 0 {
                break;
            }
            node = entries(node & 0x7FFF_FFFF)?.first()?.1;
        }
        let leaf = read(base + u64::from(node), 8)?;
        let (rva, size) = (u32_at(&leaf, 0)?, u32_at(&leaf, 4)? as usize);
        if size > 1024 * 1024 {
            return None;
        }
        read(to_file(rva)?, size)
    };
    let data = |kind: u32, id: Option<u32>| data_nth(kind, id, 0);
    const RT_ICON: u32 = 3;
    const RT_GROUP_ICON: u32 = 14;
    // Номер группы, которого нет (ярлык помнит значок прежней версии), —
    // первая группа.
    let group = match group_index {
        0 => data(RT_GROUP_ICON, None),
        n if n > 0 => data_nth(RT_GROUP_ICON, None, n as usize).or_else(|| data(RT_GROUP_ICON, None)),
        n => data(RT_GROUP_ICON, Some(n.unsigned_abs())).or_else(|| data(RT_GROUP_ICON, None)),
    }?;
    let count = usize::from(u16_at(&group, 4)?).min(64);
    // (сторона, бит на точку, номер значка)
    let mut sizes: Vec<(u32, u16, u16)> = (0..count)
        .filter_map(|i| {
            let e = 6 + i * 14;
            let side = match *group.get(e)? {
                0 => 256,
                w => u32::from(w),
            };
            Some((side, u16_at(&group, e + 6)?, u16_at(&group, e + 12)?))
        })
        .collect();
    // Ближе к 48 и глубже цветом — первым.
    sizes.sort_by_key(|(side, bits, _)| best_size_key(*side, *bits));
    sizes.into_iter().find_map(|(_, _, id)| dib_to_image(&data(RT_ICON, Some(u32::from(id)))?))
}

/// Значок DIB (BITMAPINFOHEADER, 32/24/8/4/1 бит, маска AND) в картинку.
fn dib_to_image(dib: &[u8]) -> Option<Image> {
    if dib.get(..4)? == b"\x89PNG" || u32_at(dib, 0)? != 40 {
        return None;
    }
    let w = u32_at(dib, 4)? as i32;
    let h = (u32_at(dib, 8)? as i32) / 2;
    let bits = u16_at(dib, 14)?;
    if u32_at(dib, 16)? != 0 || !(1..=256).contains(&w) || !(1..=256).contains(&h) {
        return None;
    }
    let (w, h) = (w as usize, h as usize);
    let colors = match bits {
        1 | 4 | 8 => match u32_at(dib, 32)? {
            0 => 1usize << bits,
            n => (n as usize).min(256),
        },
        24 | 32 => 0,
        _ => return None,
    };
    let palette = 40;
    let pixels = palette + colors * 4;
    let stride = (w * usize::from(bits)).div_ceil(32) * 4;
    let mask = pixels + stride * h;
    let mask_stride = w.div_ceil(32) * 4;
    let mut out = Vec::new();
    out.try_reserve_exact(8 + w * h * 4).ok()?;
    out.extend_from_slice(mini_ui::image::MAGIC);
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    let mut any_alpha = false;
    if bits == 32 {
        for y in 0..h {
            let row = pixels + (h - 1 - y) * stride;
            any_alpha |= (0..w).any(|x| dib.get(row + x * 4 + 3).is_some_and(|a| *a != 0));
        }
    }
    for y in 0..h {
        let row = pixels + (h - 1 - y) * stride;
        let mrow = mask + (h - 1 - y) * mask_stride;
        for x in 0..w {
            let masked = dib.get(mrow + x / 8).is_some_and(|byte| byte & (0x80 >> (x % 8)) != 0);
            let (b, g, r, a) = match bits {
                32 => {
                    let p = dib.get(row + x * 4..row + x * 4 + 4)?;
                    (p[0], p[1], p[2], if any_alpha { p[3] } else if masked { 0 } else { 255 })
                }
                24 => {
                    let p = dib.get(row + x * 3..row + x * 3 + 3)?;
                    (p[0], p[1], p[2], if masked { 0 } else { 255 })
                }
                _ => {
                    let per = 8 / usize::from(bits);
                    let byte = *dib.get(row + x / per)?;
                    let shift = 8 - usize::from(bits) * (x % per + 1);
                    let index = usize::from((byte >> shift) & ((1u16 << bits) - 1) as u8);
                    let c = dib.get(palette + index * 4..palette + index * 4 + 4)?;
                    (c[0], c[1], c[2], if masked { 0 } else { 255 })
                }
            };
            out.extend_from_slice(&[r, g, b, a]);
        }
    }
    Image::parse(&out)
}

// ── Карточка на столе ───────────────────────────────────────────────────────

/// Что нажали на карточке.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    /// «Подробнее» — журнал событий.
    Details,
    /// Крестик или «Закрыть».
    Close,
    Inside,
}

/// Карточка заставки — слой стола, как уведомление (`toast`).
pub struct Splash {
    /// Корень показанного запуска.
    root: Option<u32>,
    surface: Option<Surface>,
    pub rect: Rect,
    /// Кадр полосы, нарисованный последним (`now / FRAME_MS`); у ошибки — 0.
    frame: u64,
    failed: bool,
    details: Rect,
    close: Rect,
    cross: Rect,
    /// Нарисована ли карточка со значком программы (он находится позже).
    icon_seen: bool,
}

impl Default for Splash {
    fn default() -> Self {
        Self {
            root: None,
            surface: None,
            rect: Rect::EMPTY,
            frame: 0,
            failed: false,
            details: Rect::EMPTY,
            close: Rect::EMPTY,
            cross: Rect::EMPTY,
            icon_seen: false,
        }
    }
}

impl Splash {
    /// Слой: поверхность и место.
    #[must_use]
    pub fn layer(&self) -> Option<(&Surface, Rect)> {
        self.surface.as_ref().map(|surface| (surface, self.rect))
    }

    /// Обновить карточку (из `Compositor::present`, под замком стола): какой
    /// запуск показывать, кадр полосы. Возвращает прямоугольники, которые
    /// надо перерисовать, и идёт ли анимация.
    pub fn advance(&mut self, area: Rect, scale: mini_ui::Scale) -> (Vec<Rect>, bool) {
        let mut damage = Vec::new();
        let now = crate::time::uptime_ms();
        // Показывается последний живой запуск; кончившиеся — забыть.
        let shown = {
            let mut launches = LAUNCHES.lock();
            launches.retain(|launch| launch.phase != Phase::Done);
            launches
                .iter()
                .rev()
                .find(|launch| !launch.hidden)
                .map(|launch| (launch.root, launch.name.clone(), launch.icon.clone(), launch.phase.clone()))
        };
        let Some((root, name, icon, phase)) = shown else {
            if self.surface.take().is_some() {
                damage.push(self.rect);
            }
            self.root = None;
            return (damage, false);
        };
        let failed = matches!(phase, Phase::Failed(_));
        let frame = if failed { 0 } else { now / FRAME_MS };
        let same = self.root == Some(root) && self.surface.is_some() && self.failed == failed;
        if same && self.frame == frame && !self.icon_changed(&icon) {
            return (damage, !failed);
        }
        let old = self.rect;
        if self.surface.is_some() && !same {
            damage.push(old);
        }
        self.root = Some(root);
        self.failed = failed;
        self.frame = frame;
        self.icon_seen = icon.is_some();
        self.render(area, scale, &name, icon.as_deref(), &phase, now);
        damage.push(self.rect);
        (damage, !failed)
    }

    fn icon_changed(&self, icon: &Option<Arc<Image>>) -> bool {
        icon.is_some() != self.icon_seen
    }

    fn render(&mut self, area: Rect, scale: mini_ui::Scale, name: &str, icon: Option<&Image>, phase: &Phase, now: u64) {
        let ctx = Ctx::scaled(scale);
        let p = ctx.palette;
        let px = |value: u32| ctx.px(value);
        let pad = px(20);
        let icon_side = px(48);
        let failed = matches!(phase, Phase::Failed(_));
        let w = px(400).min(area.w);
        let h = if failed { pad + icon_side + px(14) + px(32) + pad } else { pad + icon_side + px(20) + px(4) + pad };
        self.rect = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
        let fill = p.tb1.over(p.tb2.color);
        let Some(mut surface) = Surface::new(w, h, fill) else {
            self.surface = None;
            return;
        };
        let ctx = ctx.on(fill);
        let s = &mut surface;
        let bounds = s.bounds();
        draw::rounded_stroke(s, bounds, px(theme::R_WINDOW), p.line3.color, p.line3.alpha);
        let left = pad as i32;
        let top = pad as i32;

        // Значок: свой из .exe или общий «Программы Windows».
        let slot = Rect::new(left, top, icon_side, icon_side);
        match icon {
            Some(image) => image.draw(s, slot),
            None => match super::appicon::design("windows-apps") {
                Some(image) => image.draw(s, slot),
                None => paint::icon_tile(ctx, s, slot, Icon::Package, paint::Tone::Accent, true),
            },
        }
        // Крестик — спрятать карточку (запуск идёт дальше).
        let cross = Rect::new(w as i32 - px(8 + 28) as i32, px(8) as i32, px(28), px(28));
        let glyph = px(10);
        glyphicon::draw(s, Icon::Close, cross.x + (cross.w as i32 - glyph as i32) / 2, cross.y + (cross.h as i32 - glyph as i32) / 2, glyph, p.ink3, 255);

        let tx = slot.right() + px(16) as i32;
        let room = (cross.x - px(4) as i32 - tx).max(0) as u32;
        let title_h = i32::from(ctx.face(Role::Strong).line);
        let sub_h = i32::from(ctx.face(Role::Body).line);
        let ty = top + (icon_side as i32 - title_h - px(4) as i32 - sub_h) / 2;
        let title = if failed { alloc::format!("Не удалось запустить «{name}»") } else { String::from(name) };
        paint::text_clipped(ctx, s, Role::Strong, tx, ty, room, &title, p.ink);
        let sub_y = ty + title_h + px(4) as i32;
        let sub_room = (w as i32 - pad as i32 - tx).max(0) as u32;
        match phase {
            Phase::Failed(text) => {
                paint::text_clipped(ctx, s, Role::Caption, tx, sub_y, sub_room, text, p.bad);
            }
            _ => {
                paint::text_clipped(ctx, s, Role::Body, tx, sub_y, sub_room, "Запуск…", p.ink3);
            }
        }

        let at = |r: Rect| r.translate(self.rect.x, self.rect.y);
        if failed {
            let by = slot.bottom() + px(14) as i32;
            let face = ctx.face(Role::Body);
            let close = Rect::new(w as i32 - pad as i32 - (face.width("Закрыть") + px(32)) as i32, by, face.width("Закрыть") + px(32), px(32));
            let details = Rect::new(close.x - px(8) as i32 - (face.width("Подробнее") + px(32)) as i32, by, face.width("Подробнее") + px(32), px(32));
            paint::button(ctx, s, details, Weight::Normal, "Подробнее", false);
            paint::button(ctx, s, close, Weight::Primary, "Закрыть", false);
            self.details = at(details);
            self.close = at(close);
        } else {
            // Полоса без процентов: отрезок в треть дорожки бежит слева
            // направо, у краёв — с замедлением (как у Windows 11).
            let track = Rect::new(left, slot.bottom() + px(20) as i32, w - pad * 2, px(4));
            draw::rounded(s, track, track.h / 2, p.sunk.over(fill), 255);
            let seg = track.w / 3;
            let t = (now % SWEEP_MS) as i64;
            // Плавный ход: smoothstep по доле пробега.
            let f = t * 1000 / SWEEP_MS as i64;
            let eased = f * f * (3000 - 2 * f) / 1_000_000;
            let travel = i64::from(track.w + seg);
            let x0 = track.x as i64 - i64::from(seg) + travel * eased / 1000;
            let a = (x0.max(i64::from(track.x))) as i32;
            let b = ((x0 + i64::from(seg)).min(i64::from(track.right()))) as i32;
            if b > a {
                draw::rounded(s, Rect::new(a, track.y, (b - a) as u32, track.h), track.h / 2, p.acc, 255);
            }
            self.details = Rect::EMPTY;
            self.close = Rect::EMPTY;
        }
        self.cross = at(cross);
        self.surface = Some(surface);
    }

    /// Что под точкой.
    #[must_use]
    pub fn hit(&self, x: i32, y: i32) -> Option<Hit> {
        self.surface.as_ref()?;
        if self.details.contains(x, y) {
            Some(Hit::Details)
        } else if self.close.contains(x, y) || self.cross.contains(x, y) {
            Some(Hit::Close)
        } else if self.rect.contains(x, y) {
            Some(Hit::Inside)
        } else {
            None
        }
    }

    /// Закрыть показанную карточку: ошибку — забыть, идущий запуск — спрятать
    /// (за ним следим дальше, окно откроется как обычно).
    pub fn dismiss(&mut self) {
        let Some(root) = self.root else { return };
        let mut launches = LAUNCHES.lock();
        if let Some(launch) = launches.iter_mut().find(|launch| launch.root == root) {
            if matches!(launch.phase, Phase::Failed(_)) {
                launch.phase = Phase::Done;
            } else {
                launch.hidden = true;
            }
            crate::kprintln!("  launch      : splash of '{}' closed by the user", launch.name);
        }
        // Перерисуется на следующем кадре.
        self.frame = u64::MAX;
        self.root = None;
    }
}
