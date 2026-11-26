//! Заставка (22.11): что показывает экран, пока за машиной никого нет.
//!
//! # Как это устроено
//!
//! Сроки считает `power::idle_tick` — там же, где гашение экрана: заставка
//! появляется через [`after`] секунд без ввода, а когда истекает срок
//! отключения экрана, она уходит и мониторы гаснут по-настоящему
//! (`display::set_power`). Fullscreen window or a Wayland idle inhibitor
//! (`zwp_idle_inhibit_manager_v1`) hold both back ([`inhibited`]).
//!
//! Картинка — слой стола поверх всего, и экрана блокировки тоже
//! ([`Shown`], `Compositor::saver_set`): фон каждого монитора готовится один
//! раз (фото, вписанное по выбору, притушенное, или ровный цвет), часы с
//! датой — маской, которая медленно плывёт по экрану, чтобы ничего не
//! выжигалось. Первая клавиша или щелчок только убирают заставку: до программ
//! они не доходят ([`swallow_key`], для мыши — `WAKE_TOUCH` стола).
//!
//! # Фото
//!
//! Любые JPEG/PNG/BMP/QOI: ядро само читает только QOI, остальное переводит
//! та же программа, что и обои (`/bin/wallconv conv`), в тот же кэш
//! (`ui::wallpaper::cache_dir`, подкаталог `saver/`) — под размер каждого
//! монитора. Переводит заранее, в фоне ([`prepare_tick`]): сразу после смены
//! настроек, иначе — когда за машиной [`AHEAD_IDLE_MS`] никого нет, и пока
//! заставка на экране (следующие фото показа). Кэш ограничен окном
//! [`WINDOW`] фото от текущего: лишнее удаляется. Не переведённое ещё фото
//! заменяет уже готовое из списка, а нет ни одного — обои FossAi из ядра.
//! Без своего выбора — галерея обоев [`GALLERY`]. Смена фото — плавная
//! ([`FADE_MS`]).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use mini_ui::paint::{self, Ctx};
use mini_ui::qoi::Picture;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw};

use crate::input::KeyEvent;
use crate::kprintln;
use crate::sync::SpinLock;

/// Галерея по умолчанию (её наполняют пакеты обоев).
pub const GALLERY: &str = "/usr/share/wallpapers";

const BLACK: Color = Color::rgb(0, 0, 0);
const WHITE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
/// Насколько притушено фото (из 255): часы читаются на любом.
const DIM: u8 = 80;
/// Clock ink (of 255): not full white - kinder to OLED and burn-in.
const INK: u32 = 225;
/// Clock moves one step (1/1000 of the free room) this often.
const DRIFT_MS: u64 = 250;
/// Самый большой файл фото, который стоит читать.
const MAX_PHOTO: usize = 96 << 20;
/// Сколько фото показа (от текущего) держать переведёнными.
const WINDOW: usize = 8;
/// Без срочности переводить, только когда никого нет столько (мс).
const AHEAD_IDLE_MS: u64 = 5_000;
/// Сколько длится смена фото (мс).
const FADE_MS: u64 = 900;

// ---------------------------------------------------------------------------
// Настройки (desktop.cfg)
// ---------------------------------------------------------------------------

/// Что за фон.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Photo,
    Color,
}

/// Как фото ложится на экран.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// Закрыть экран целиком, лишнее по краям срезать.
    Fill,
    /// Целиком, с полями.
    Fit,
    Stretch,
    Center,
}

impl Fit {
    pub const ALL: [Fit; 4] = [Fit::Fill, Fit::Fit, Fit::Stretch, Fit::Center];

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Fit::Fill => "fill",
            Fit::Fit => "fit",
            Fit::Stretch => "stretch",
            Fit::Center => "center",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Fit::Fill => "Заполнить",
            Fit::Fit => "Вписать",
            Fit::Stretch => "Растянуть",
            Fit::Center => "По центру",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|fit| fit.tag() == tag)
    }
}

/// Сроки появления, которые предлагают «Параметры» (с).
pub const AFTER_OPTIONS: [u32; 5] = [60, 180, 300, 600, 900];
/// Как часто сменять фото (с).
pub const EVERY_OPTIONS: [u32; 4] = [10, 30, 60, 300];
/// Цвета фона: (подпись, 0xRRGGBB).
pub const COLORS: [(&str, u32); 5] =
    [("Ночь", 0x0B_12_20), ("Графит", 0x20_21_24), ("Синий", 0x0F_2A_4A), ("Зелёный", 0x0E_3B_2E), ("Бордо", 0x3B_0E_1E)];

pub const DEFAULT_AFTER: u32 = 180;

static ENABLED: AtomicBool = AtomicBool::new(false);
static AFTER: AtomicU32 = AtomicU32::new(DEFAULT_AFTER);
static PHOTO: AtomicBool = AtomicBool::new(true);
static FIT: AtomicU8 = AtomicU8::new(0);
static EVERY: AtomicU32 = AtomicU32::new(30);
static COLOR: AtomicU32 = AtomicU32::new(0x0B_12_20);
static CLOCK: AtomicBool = AtomicBool::new(true);
static INFO: AtomicBool = AtomicBool::new(false);
static LOCK: AtomicBool = AtomicBool::new(false);
/// Папка или один файл картинки; пусто — [`GALLERY`] или обои FossAi.
static SOURCE: SpinLock<String> = SpinLock::new(String::new());

/// Всё сразу — для страницы «Параметров».
#[derive(Clone)]
pub struct Settings {
    pub enabled: bool,
    pub after: u32,
    pub kind: Kind,
    pub fit: Fit,
    pub every: u32,
    pub color: u32,
    pub clock: bool,
    pub info: bool,
    pub lock: bool,
    /// Откуда фото — словами.
    pub source: String,
    /// Выбрана галерея обоев (своего выбора нет).
    pub gallery: bool,
    /// Своё — папка (`false` — одна картинка).
    pub folder: bool,
    /// Что с переводом фото — словами; `None` — всё готово.
    pub preparing: Option<String>,
}

#[must_use]
pub fn settings() -> Settings {
    Settings {
        enabled: ENABLED.load(Ordering::Relaxed),
        after: AFTER.load(Ordering::Relaxed),
        kind: if PHOTO.load(Ordering::Relaxed) { Kind::Photo } else { Kind::Color },
        fit: fit(),
        every: EVERY.load(Ordering::Relaxed),
        color: COLOR.load(Ordering::Relaxed),
        clock: CLOCK.load(Ordering::Relaxed),
        info: INFO.load(Ordering::Relaxed),
        lock: LOCK.load(Ordering::Relaxed),
        source: playlist().1,
        gallery: {
            let source = SOURCE.lock();
            source.is_empty() || source.trim_end_matches('/') == GALLERY
        },
        folder: {
            let source = SOURCE.lock().clone();
            !source.is_empty() && matches!(crate::fs::list(&source), Some(Ok(_)))
        },
        preparing: preparing(),
    }
}

fn fit() -> Fit {
    Fit::ALL[usize::from(FIT.load(Ordering::Relaxed)).min(3)]
}

/// Через сколько секунд без ввода появляется заставка; ноль — выключена.
#[must_use]
pub fn after() -> u32 {
    if ENABLED.load(Ordering::Relaxed) { AFTER.load(Ordering::Relaxed) } else { 0 }
}

/// Срок словами: «30 с», «3 мин».
#[must_use]
pub fn after_text(seconds: u32) -> String {
    if seconds < 60 || seconds % 60 != 0 { format!("{seconds} с") } else { format!("{} мин", seconds / 60) }
}

/// Прочитать настройки из `desktop.cfg` (`prefs::adopt`).
pub fn adopt(text: &str, path: &str) {
    let mut seen = false;
    let on = |key: &str| sysconf::value(text, key).map(|value| value.trim() == "on");
    if let Some(value) = on("saver") {
        ENABLED.store(value, Ordering::Relaxed);
        seen = true;
    }
    if let Some(seconds) = sysconf::value(text, "saver_after").and_then(|v| v.trim().parse::<u32>().ok()).filter(|s| *s > 0) {
        AFTER.store(seconds, Ordering::Relaxed);
    }
    if let Some(kind) = sysconf::value(text, "saver_kind") {
        PHOTO.store(kind.trim() != "color", Ordering::Relaxed);
    }
    if let Some(fit) = sysconf::value(text, "saver_fit").and_then(|v| Fit::from_tag(v.trim())) {
        FIT.store(Fit::ALL.iter().position(|f| *f == fit).unwrap_or(0) as u8, Ordering::Relaxed);
    }
    if let Some(seconds) = sysconf::value(text, "saver_every").and_then(|v| v.trim().parse::<u32>().ok()).filter(|s| *s > 0) {
        EVERY.store(seconds, Ordering::Relaxed);
    }
    if let Some(rgb) = sysconf::value(text, "saver_color").and_then(|v| u32::from_str_radix(v.trim().trim_start_matches('#'), 16).ok()) {
        COLOR.store(rgb & 0xFF_FFFF, Ordering::Relaxed);
    }
    if let Some(value) = on("saver_clock") {
        CLOCK.store(value, Ordering::Relaxed);
    }
    if let Some(value) = on("saver_info") {
        INFO.store(value, Ordering::Relaxed);
    }
    if let Some(value) = on("saver_lock") {
        LOCK.store(value, Ordering::Relaxed);
    }
    if let Some(source) = sysconf::value(text, "saver_source") {
        *SOURCE.lock() = String::from(source.trim());
    }
    // Фото переведутся заранее, при первом простое.
    mark(false);
    if seen {
        kprintln!("  saver       : {} from {path}", describe());
    }
}

/// Одной строкой — для журнала и команды `power`.
#[must_use]
pub fn describe() -> String {
    let s = settings();
    if !s.enabled {
        return String::from("off");
    }
    let what = match s.kind {
        Kind::Photo => format!("photo {} ({}, every {} s)", s.source, s.fit.tag(), s.every),
        Kind::Color => format!("color #{:06x}", s.color),
    };
    format!(
        "after {} s, {what}, clock {}, info {}, lock on exit {}",
        s.after,
        if s.clock { "on" } else { "off" },
        if s.info { "on" } else { "off" },
        if s.lock { "on" } else { "off" }
    )
}

fn store(key: &str, value: &str) -> Result<(), String> {
    super::prefs::store_line(key, value).map_err(|err| format!("{err}"))
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// Что нажали на странице «Питание».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Deed {
    Enable(bool),
    After(u32),
    Kind(Kind),
    /// Фото — галерея обоев.
    Gallery,
    /// Выбрать папку с фото в окне «Открыть» (`/bin/filedlg`).
    PickFolder,
    /// Выбрать одну картинку в окне «Открыть».
    PickPicture,
    Fit(Fit),
    Every(u32),
    Color(u32),
    Clock(bool),
    Info(bool),
    Lock(bool),
    Preview,
}

/// Применить и запомнить. `Ok` — что сказать человеку, `Err` — что не вышло.
pub fn apply(deed: Deed) -> Result<String, String> {
    let (saved, said) = match deed {
        Deed::Enable(on) => {
            ENABLED.store(on, Ordering::Relaxed);
            mark(true);
            (store("saver", on_off(on)), String::from(if on { "заставка включена" } else { "заставка выключена" }))
        }
        Deed::After(seconds) => {
            AFTER.store(seconds.max(1), Ordering::Relaxed);
            (store("saver_after", &format!("{seconds}")), format!("заставка появится через {}", after_text(seconds)))
        }
        Deed::Kind(kind) => {
            PHOTO.store(kind == Kind::Photo, Ordering::Relaxed);
            mark(true);
            (store("saver_kind", if kind == Kind::Photo { "photo" } else { "color" }), String::from("вид заставки запомнен"))
        }
        Deed::Gallery => {
            SOURCE.lock().clear();
            mark(true);
            (store("saver_source", ""), format!("фото: {}", playlist().1.to_lowercase()))
        }
        Deed::PickFolder | Deed::PickPicture => {
            let folder = deed == Deed::PickFolder;
            return start_choose(folder).map(|()| {
                String::from(if folder { "выберите папку с фото в окне «Открыть»" } else { "выберите фото в окне «Открыть»" })
            });
        }
        Deed::Fit(fit) => {
            FIT.store(Fit::ALL.iter().position(|f| *f == fit).unwrap_or(0) as u8, Ordering::Relaxed);
            (store("saver_fit", fit.tag()), format!("фото: {}", fit.title().to_lowercase()))
        }
        Deed::Every(seconds) => {
            EVERY.store(seconds.max(5), Ordering::Relaxed);
            (store("saver_every", &format!("{seconds}")), format!("фото сменяется каждые {}", after_text(seconds)))
        }
        Deed::Color(rgb) => {
            COLOR.store(rgb, Ordering::Relaxed);
            (store("saver_color", &format!("{rgb:06x}")), String::from("цвет заставки запомнен"))
        }
        Deed::Clock(on) => {
            CLOCK.store(on, Ordering::Relaxed);
            (store("saver_clock", on_off(on)), String::from(if on { "часы на заставке" } else { "заставка без часов" }))
        }
        Deed::Info(on) => {
            INFO.store(on, Ordering::Relaxed);
            (store("saver_info", on_off(on)), String::from("сведения о системе запомнены"))
        }
        Deed::Lock(on) => {
            LOCK.store(on, Ordering::Relaxed);
            (
                store("saver_lock", on_off(on)),
                String::from(if on { "после заставки — экран блокировки" } else { "после заставки — сразу стол" }),
            )
        }
        Deed::Preview => {
            crate::power::request_saver_preview();
            return Ok(String::from("заставка сейчас появится; любое движение её уберёт"));
        }
    };
    kprintln!("  settings    : saver {}", describe());
    // The displays go dark before the saver is due: it would never show
    // (hw-log18). Say it right away, not only on the page.
    let off = crate::power::screen_off_after();
    let said = if after() != 0 && off != 0 && off <= after() {
        kprintln!("  saver       : screen off after {off} s comes before the saver ({} s) - it will not show", after());
        format!("{said}; но экран гаснет раньше (через {}) — заставка не появится", after_text(off))
    } else {
        said
    };
    match saved {
        Ok(()) => Ok(said),
        Err(err) => Err(format!("{said}, но не запомнено: {err}")),
    }
}

/// Команда `power saver …` (сценарии стенда и серийная линия).
pub fn command(words: &str) {
    let mut it = words.split_whitespace();
    let word = it.next().unwrap_or("");
    let arg = it.next();
    let flag = |arg: Option<&str>| arg.is_none_or(|a| a == "on");
    let result = match word {
        "" => {
            kprintln!("  saver       : {}", describe());
            kprintln!("  usage: power saver on [s]|off|after <s>|photo [path|gallery]|choose folder|file|cache|color [rrggbb]|fit fill|fit|stretch|center|every <s>|clock on|off|info on|off|lock on|off|preview");
            return;
        }
        "on" => {
            if let Some(seconds) = arg.and_then(|a| a.parse::<u32>().ok()) {
                let _ = apply(Deed::After(seconds));
            }
            apply(Deed::Enable(true))
        }
        "off" => apply(Deed::Enable(false)),
        "after" => match arg.and_then(|a| a.parse::<u32>().ok()) {
            Some(seconds) => apply(Deed::After(seconds)),
            None => Err(String::from("after <seconds>")),
        },
        "photo" => {
            if let Some(path) = arg {
                *SOURCE.lock() = String::from(if path == "default" || path == "gallery" { "" } else { path });
                let _ = store("saver_source", &SOURCE.lock().clone());
            }
            apply(Deed::Kind(Kind::Photo))
        }
        "choose" => apply(if arg == Some("file") { Deed::PickPicture } else { Deed::PickFolder }),
        "cache" => {
            kprintln!("  saver       : cache {} ({})", saver_dir(), preparing().unwrap_or_else(|| String::from("all photos ready")));
            for (name, bytes) in cache_files() {
                kprintln!("  saver       :   {name} {} KiB", bytes / 1024);
            }
            return;
        }
        "color" => {
            if let Some(rgb) = arg.and_then(|a| u32::from_str_radix(a.trim_start_matches('#'), 16).ok()) {
                let _ = apply(Deed::Color(rgb & 0xFF_FFFF));
            }
            apply(Deed::Kind(Kind::Color))
        }
        "fit" => match arg.and_then(Fit::from_tag) {
            Some(fit) => apply(Deed::Fit(fit)),
            None => Err(String::from("fit fill|fit|stretch|center")),
        },
        "every" => match arg.and_then(|a| a.parse::<u32>().ok()) {
            Some(seconds) => apply(Deed::Every(seconds)),
            None => Err(String::from("every <seconds>")),
        },
        "clock" => apply(Deed::Clock(flag(arg))),
        "info" => apply(Deed::Info(flag(arg))),
        "lock" => apply(Deed::Lock(flag(arg))),
        "preview" => apply(Deed::Preview),
        other => Err(format!("unknown '{other}'")),
    };
    match result {
        Ok(_) => kprintln!("  saver       : {} (saved)", describe()),
        Err(err) => kprintln!("  saver       : {} ({err})", describe()),
    }
}

// ---------------------------------------------------------------------------
// Откуда фото
// ---------------------------------------------------------------------------

/// Фото, которое переведёт `wallconv`: JPEG, PNG, BMP, QOI — но не
/// уменьшенная копия галереи (`*.thumb.qoi` рядом с JPEG — для значков
/// выбора обоев).
fn is_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".jpg", ".jpeg", ".png", ".bmp", ".qoi"].iter().any(|ext| lower.ends_with(ext)) && !lower.ends_with(".thumb.qoi")
}

/// Фото каталога по имени; не каталог — пусто.
fn pictures_in(dir: &str) -> Vec<String> {
    let Some(Ok(entries)) = crate::fs::list(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .into_iter()
        .filter(|entry| entry.kind == crate::vfs::NodeKind::File && is_picture(&entry.name))
        .map(|entry| if dir.ends_with('/') { format!("{dir}{}", entry.name) } else { format!("{dir}/{}", entry.name) })
        .collect();
    out.sort();
    out
}

/// Фото по порядку и откуда они — словами. Пустой список — обои FossAi.
fn playlist() -> (Vec<String>, String) {
    let source = SOURCE.lock().clone();
    if source.is_empty() {
        let list = pictures_in(GALLERY);
        return if list.is_empty() { (list, String::from("Обои FossAi")) } else { (list, String::from("Галерея обоев")) };
    }
    if is_picture(&source) && crate::fs::list(&source).is_none_or(|r| r.is_err()) {
        return (alloc::vec![source.clone()], source);
    }
    let list = pictures_in(&source);
    if list.is_empty() { (list, format!("{source} (пусто — обои FossAi)")) } else { (list, source) }
}

// ---------------------------------------------------------------------------
// Перевод фото заранее (`/bin/wallconv`, кэш обоев)
// ---------------------------------------------------------------------------

/// Что делает запущенная программа.
#[derive(Clone)]
enum Work {
    /// Перевод `source` в `output` (размер монитора).
    Convert { source: String, output: String },
    /// Окно выбора: путь окажется в записке.
    Choose { note: String },
}

struct Prep {
    job: Option<(crate::sched::TaskId, Work, u64)>,
    /// Что перевести: источник, файл кэша, размер.
    queue: Vec<(String, String, u32, u32)>,
    /// Пересобрать очередь; `urgent` — не ждать простоя (настройки сменили).
    dirty: bool,
    urgent: bool,
    /// Файлы кэша, которые в эту загрузку не получились: не пытаться снова.
    failed: Vec<String>,
    /// Размеры мониторов, под которые собрана очередь.
    sizes: Vec<(u32, u32)>,
    checked_ms: u64,
    /// Каталог кэша, когда уже создан.
    dir: Option<String>,
}

static PREP: SpinLock<Prep> = SpinLock::new(Prep {
    job: None,
    queue: Vec::new(),
    dirty: false,
    urgent: false,
    failed: Vec::new(),
    sizes: Vec::new(),
    checked_ms: 0,
    dir: None,
});

/// Пересобрать очередь перевода: `urgent` — сразу, иначе при простое.
fn mark(urgent: bool) {
    let mut prep = PREP.lock();
    prep.dirty = true;
    prep.urgent |= urgent;
}

/// Каталог переведённых фото заставки — подкаталог кэша обоев, открытый
/// программе сеанса на запись, как и он.
fn saver_dir() -> String {
    if let Some(dir) = PREP.lock().dir.clone() {
        return dir;
    }
    let dir = format!("{}/saver", super::wallpaper::cache_dir());
    let root = crate::vfs::perm::Credentials::ROOT;
    let _ = crate::fs::mkdir_as(root, &dir, 0o1777);
    let _ = crate::fs::chmod_as(root, &dir, 0o1777);
    PREP.lock().dir = Some(dir.clone());
    dir
}

/// Размер, под который переводится фото монитора (`wallconv` больше 4K не
/// делает).
fn size_key(w: u32, h: u32) -> (u32, u32) {
    (w.min(3840), h.min(2160))
}

fn cache_file(source: &str, w: u32, h: u32) -> String {
    let (w, h) = size_key(w, h);
    format!("{}/{:016x}-{w}x{h}.qoi", saver_dir(), super::wallpaper::fnv1a64(source.as_bytes()))
}

fn exists(path: &str) -> bool {
    matches!(crate::fs::read(path, 16), Some(Ok(_)))
}

/// Готовый QOI фото `source` под монитор `w`×`h`: свой кэш, обои того же
/// размера или сам файл, если он уже QOI.
fn ready_file(source: &str, w: u32, h: u32) -> Option<String> {
    let own = cache_file(source, w, h);
    if exists(&own) {
        return Some(own);
    }
    let (kw, kh) = size_key(w, h);
    let wall = format!("{}/{:016x}-{kw}x{kh}.qoi", super::wallpaper::cache_dir(), super::wallpaper::fnv1a64(source.as_bytes()));
    if exists(&wall) {
        return Some(wall);
    }
    source.to_ascii_lowercase().ends_with(".qoi").then(|| String::from(source))
}

/// Файлы кэша заставки и их размер.
fn cache_files() -> Vec<(String, usize)> {
    let dir = saver_dir();
    let Some(Ok(entries)) = crate::fs::list(&dir) else {
        return Vec::new();
    };
    entries
        .into_iter()
        .filter(|entry| entry.kind == crate::vfs::NodeKind::File)
        .map(|entry| {
            let size = entry.size as usize;
            (format!("{dir}/{}", entry.name), size)
        })
        .collect()
}

/// Сколько фото ещё переводится — словами; `None` — всё готово.
fn preparing() -> Option<String> {
    let prep = PREP.lock();
    let converting = matches!(prep.job, Some((_, Work::Convert { .. }, _)));
    let left = prep.queue.len() + usize::from(converting);
    (left > 0).then(|| format!("готовится фото: осталось {left}"))
}

/// Размеры мониторов стола; `None` — стол занят или его нет.
fn monitor_sizes() -> Option<Vec<(u32, u32)>> {
    let rects = super::with_desktop(|desktop| desktop.output_rects().to_vec())?;
    let mut sizes: Vec<(u32, u32)> = rects.iter().filter(|r| r.w > 0 && r.h > 0).map(|r| size_key(r.w, r.h)).collect();
    sizes.sort_unstable();
    sizes.dedup();
    (!sizes.is_empty()).then_some(sizes)
}

/// Собрать очередь: [`WINDOW`] фото от текущего под каждый размер монитора;
/// всё остальное в кэше заставки — удалить (кэш ограничен окном).
fn plan() {
    let (list, _) = playlist();
    let sizes = PREP.lock().sizes.clone();
    if sizes.is_empty() {
        return;
    }
    let index = INDEX.load(Ordering::Relaxed);
    let count = list.len().min(WINDOW);
    let mut wanted: Vec<String> = Vec::new();
    let mut queue = Vec::new();
    let failed = PREP.lock().failed.clone();
    for i in 0..count {
        let source = &list[(index + i) % list.len()];
        for &(w, h) in &sizes {
            let output = cache_file(source, w, h);
            wanted.push(output.clone());
            if ready_file(source, w, h).is_none() && !failed.contains(&output) {
                queue.push((source.clone(), output, w, h));
            }
        }
    }
    let busy = match &PREP.lock().job {
        Some((_, Work::Convert { output, .. }, _)) => Some(output.clone()),
        _ => None,
    };
    let mut removed = 0;
    for (path, _) in cache_files() {
        // Недописанные файлы программы и то, что она пишет сейчас, — её.
        if wanted.contains(&path) || busy.as_deref().is_some_and(|b| path.starts_with(b)) || !path.ends_with(".qoi") {
            continue;
        }
        if crate::fs::remove_as(crate::vfs::perm::Credentials::ROOT, &path).is_some_and(|r| r.is_ok()) {
            removed += 1;
        }
    }
    queue.retain(|(_, output, _, _)| busy.as_deref() != Some(output.as_str()));
    let mut prep = PREP.lock();
    if !queue.is_empty() || removed > 0 {
        kprintln!("  saver       : {} photo(s) to prepare for {:?}, {removed} stale removed from the cache", queue.len(), sizes);
    }
    if queue.is_empty() && prep.job.is_none() {
        prep.urgent = false;
    }
    prep.queue = queue;
    prep.dirty = false;
}

/// Перерисовать «Параметры», если открыты (сменился выбор или ход перевода).
fn refresh_settings() {
    super::with_desktop(|desktop| {
        if let Some(window) = desktop.find(super::App::Settings) {
            window.refresh_settings();
            desktop.present();
        }
    });
}

fn start_choose(folder: bool) -> Result<(), String> {
    if matches!(PREP.lock().job, Some((_, Work::Choose { .. }, _))) {
        return Err(String::from("окно выбора уже открыто"));
    }
    let note = format!("{}/chosen.txt", saver_dir());
    let _ = crate::fs::remove_as(crate::vfs::perm::Credentials::ROOT, &note);
    let current = SOURCE.lock().clone();
    // Начать там, откуда фото сейчас: папка — она, картинка — её папка.
    let start = if current.is_empty() {
        String::from(GALLERY)
    } else if matches!(crate::fs::list(&current), Some(Ok(_))) {
        current
    } else {
        current.rsplit_once('/').map_or(current.clone(), |(dir, _)| String::from(dir))
    };
    let line = format!("/bin/wallconv choose {} {} {}", if folder { "folder" } else { "file" }, super::panel::pct(&note), super::panel::pct(&start));
    match crate::user::spawn(&line, crate::user::session::credentials()) {
        Ok(task) => {
            kprintln!("  saver       : started '{line}' as {task}");
            let mut prep = PREP.lock();
            // Перевод, если шёл, доведёт свою программу сам; очередь подождёт.
            if let Some((old, Work::Convert { source, .. }, _)) = prep.job.take() {
                kprintln!("  saver       : conversion of {source} ({old}) left running in the background");
                prep.dirty = true;
            }
            prep.job = Some((task, Work::Choose { note }, crate::time::uptime_ms()));
            Ok(())
        }
        Err(err) => {
            kprintln!("  saver       : cannot start '{line}': {err}");
            Err(format!("окно выбора не открылось: {err}"))
        }
    }
}

/// Перевод фото, пока за машиной никого нет (зовёт `power::idle_tick`, вне
/// замка стола): запустить следующую программу, дождаться её, принять выбор
/// из окна «Открыть».
pub fn prepare_tick(idle_ms: u64) {
    let now = crate::time::uptime_ms();
    let job = PREP.lock().job.clone();
    if let Some((task, work, started)) = job {
        let running = matches!(crate::sched::lookup(task), Some((_, state)) if state != crate::sched::TaskState::Finished);
        if running {
            return;
        }
        PREP.lock().job = None;
        finish(work, now.saturating_sub(started));
    }
    if !ENABLED.load(Ordering::Relaxed) || !PHOTO.load(Ordering::Relaxed) {
        return;
    }
    let up = is_up();
    let (check, urgent) = {
        let prep = PREP.lock();
        (now >= prep.checked_ms + 3000 || prep.sizes.is_empty(), prep.urgent)
    };
    if check {
        PREP.lock().checked_ms = now;
        if let Some(sizes) = monitor_sizes() {
            let mut prep = PREP.lock();
            if prep.sizes != sizes {
                prep.sizes = sizes;
                prep.dirty = true;
            }
        }
    }
    if !(urgent || up || idle_ms >= AHEAD_IDLE_MS) {
        return;
    }
    if PREP.lock().dirty {
        plan();
    }
    // Обои переводятся — их программа первая (обе держат по сотне мегабайт).
    if PREP.lock().job.is_some() || super::wallpaper::busy() {
        return;
    }
    let next = {
        let mut prep = PREP.lock();
        if prep.queue.is_empty() { None } else { Some(prep.queue.remove(0)) }
    };
    let Some((source, output, w, h)) = next else {
        return;
    };
    let line = format!("/bin/wallconv conv {} {} {w} {h}", super::panel::pct(&source), super::panel::pct(&output));
    match crate::user::spawn(&line, crate::user::session::credentials()) {
        Ok(task) => {
            kprintln!("  saver       : started '{line}' as {task}");
            PREP.lock().job = Some((task, Work::Convert { source, output }, now));
        }
        Err(err) => {
            kprintln!("  saver       : cannot start '{line}': {err}");
            PREP.lock().failed.push(output);
        }
    }
}

/// Программа закончила.
fn finish(work: Work, took: u64) {
    match work {
        Work::Convert { source, output } => {
            if exists(&output) {
                kprintln!("  saver       : photo {source} prepared in {took} ms");
                // Заставка показывает замену — поставить настоящее фото.
                if is_up() && FALLBACK.load(Ordering::Relaxed) {
                    REBUILD.store(true, Ordering::Relaxed);
                }
            } else {
                kprintln!("  saver       : photo {source} did not convert");
                PREP.lock().failed.push(output);
            }
            // The guard must be gone before `refresh_settings`: redrawing
            // Settings asks `preparing()`, which takes PREP again. With more
            // photos queued the guard used to live on into the redraw - a
            // self-deadlock on the spin lock with the desktop lock held, the
            // whole OS hung right after the first photo was prepared
            // (hw-log18: «Заставка» switched on with Settings open on «Питание»).
            let all_done = {
                let mut prep = PREP.lock();
                let done = prep.queue.is_empty() && !prep.dirty;
                if done {
                    prep.urgent = false;
                }
                done
            };
            if all_done {
                kprintln!("  saver       : all photos prepared");
            }
            refresh_settings();
        }
        Work::Choose { note } => {
            let chosen = match crate::fs::read(&note, 4096) {
                Some(Ok((bytes, _))) => String::from_utf8(bytes).ok().map(|text| String::from(text.trim())).filter(|text| !text.is_empty()),
                _ => None,
            };
            let _ = crate::fs::remove_as(crate::vfs::perm::Credentials::ROOT, &note);
            match chosen {
                Some(path) => {
                    kprintln!("  saver       : photos from {path} (chosen in {took} ms)");
                    *SOURCE.lock() = path.clone();
                    INDEX.store(0, Ordering::Relaxed);
                    PREP.lock().failed.clear();
                    if let Err(err) = store("saver_source", &path) {
                        kprintln!("  saver       : not saved: {err}");
                    }
                    PHOTO.store(true, Ordering::Relaxed);
                    let _ = store("saver_kind", "photo");
                }
                None => kprintln!("  saver       : choice cancelled"),
            }
            mark(true);
            refresh_settings();
        }
    }
}

// ---------------------------------------------------------------------------
// Картинка
// ---------------------------------------------------------------------------

/// Альфа-маска надписи (белое по чёрному → яркость).
struct Mask {
    w: u32,
    h: u32,
    alpha: Vec<u8>,
}

impl Mask {
    const fn empty() -> Self {
        Self { w: 0, h: 0, alpha: Vec::new() }
    }

    fn at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return 0;
        }
        self.alpha[(y as u32 * self.w + x as u32) as usize]
    }

    /// Строка текста, увеличенная в `zoom` раз билинейно (шрифта крупнее
    /// заголовка в системе нет — как у часов экрана блокировки).
    fn text(ctx: Ctx, role: Role, text: &str, zoom: u32) -> Option<Self> {
        let face = ctx.face(role);
        let (w, h) = (face.width(text).max(1) + 2, u32::from(face.line).max(1));
        let mut surface = Surface::new(w, h, BLACK)?;
        paint::text(ctx.on(BLACK), &mut surface, role, 1, 0, text, WHITE);
        let src: Vec<u8> = (0..h).flat_map(|row| surface.row(row).iter().map(|p| Color::from_pixel(*p).g).collect::<Vec<_>>()).collect();
        let zoom = zoom.max(1);
        if zoom == 1 {
            return Some(Self { w, h, alpha: src });
        }
        let (zw, zh) = (w * zoom, h * zoom);
        let mut alpha = Vec::new();
        alpha.try_reserve_exact((zw * zh) as usize).ok()?;
        let at = |cx: u32, cy: u32| -> u32 { u32::from(src[(cy.min(h - 1) * w + cx.min(w - 1)) as usize]) };
        for ty in 0..zh {
            for tx in 0..zw {
                let fx = (tx * 256 + 128) / zoom;
                let fy = (ty * 256 + 128) / zoom;
                let (x0, y0) = (fx.saturating_sub(128) / 256, fy.saturating_sub(128) / 256);
                let (wx, wy) = (fx.saturating_sub(128) % 256, fy.saturating_sub(128) % 256);
                let top = at(x0, y0) * (256 - wx) + at(x0 + 1, y0) * wx;
                let bottom = at(x0, y0 + 1) * (256 - wx) + at(x0 + 1, y0 + 1) * wx;
                let a = (top * (256 - wy) + bottom * wy) >> 16;
                // A bilinear zoom smears an edge over `zoom` points; a steeper
                // ramp around the middle gives the big digits a crisp edge
                // that is still antialiased (one or two points wide).
                let a = if zoom >= 3 { (a.saturating_sub(64) * 255 / 128).min(255) } else { a };
                alpha.push(a as u8);
            }
        }
        Some(Self { w: zw, h: zh, alpha })
    }

    /// Line of text rasterized by the TrueType font at `px` - its own size,
    /// not a zoomed heading (25.11, Roman: the floating clock is blurry while
    /// the lock-screen clock is crisp).
    fn ttf(font: &ttf::Font, text: &str, px: f32) -> Option<Self> {
        let metrics = font.line_metrics(px);
        let ascent = ceil(metrics.ascent) as i32;
        let h = ceil(metrics.ascent + metrics.descent).max(1) as u32;
        let glyphs: Vec<ttf::Bitmap> = text.chars().map(|ch| font.rasterize(ch, px)).collect();
        let advance: f32 = glyphs.iter().map(|g| g.advance).sum();
        let w = (ceil(advance).max(0) as u32 + 4).max(1);
        let mut alpha = Vec::new();
        alpha.try_reserve_exact((w * h) as usize).ok()?;
        alpha.resize((w * h) as usize, 0u8);
        let mut pen = 2.0f32;
        for glyph in &glyphs {
            let x0 = (pen + 0.5) as i32 + glyph.left;
            let y0 = ascent + glyph.top;
            for gy in 0..glyph.height {
                let y = y0 + gy as i32;
                if y < 0 || y >= h as i32 {
                    continue;
                }
                for gx in 0..glyph.width {
                    let x = x0 + gx as i32;
                    if x < 0 || x >= w as i32 {
                        continue;
                    }
                    let cover = glyph.coverage[(gy * glyph.width + gx) as usize];
                    let at = (y as u32 * w + x as u32) as usize;
                    alpha[at] = alpha[at].max(cover);
                }
            }
            pen += glyph.advance;
        }
        Some(Self { w, h, alpha })
    }

    /// Без пустых строк сверху и снизу: межстрочный запас шрифта, увеличенный
    /// в разы, разводил часы и дату слишком далеко.
    fn trimmed(self) -> Self {
        let w = self.w as usize;
        let blank = |row: u32| self.alpha[row as usize * w..(row as usize + 1) * w].iter().all(|a| *a < 16);
        let top = (0..self.h).find(|r| !blank(*r)).unwrap_or(0);
        let bottom = (0..self.h).rev().find(|r| !blank(*r)).map_or(self.h, |r| r + 1);
        if top == 0 && bottom == self.h || bottom <= top {
            return self;
        }
        let alpha = self.alpha[top as usize * w..bottom as usize * w].to_vec();
        Self { w: self.w, h: bottom - top, alpha }
    }

    /// Строки одна под другой, по левому краю.
    fn stack(parts: Vec<Self>, gap: u32) -> Self {
        let parts: Vec<Self> = parts.into_iter().filter(|p| p.w > 0).map(Self::trimmed).collect();
        if parts.is_empty() {
            return Self::empty();
        }
        let w = parts.iter().map(|p| p.w).max().unwrap_or(0);
        let h = parts.iter().map(|p| p.h).sum::<u32>() + gap * (parts.len() as u32 - 1);
        let mut alpha = Vec::new();
        if alpha.try_reserve_exact((w * h) as usize).is_err() {
            return Self::empty();
        }
        alpha.resize((w * h) as usize, 0);
        let mut top = 0;
        for part in &parts {
            for y in 0..part.h {
                let from = (y * part.w) as usize;
                let to = ((top + y) * w) as usize;
                alpha[to..to + part.w as usize].copy_from_slice(&part.alpha[from..from + part.w as usize]);
            }
            top += part.h + gap;
        }
        Self { w, h, alpha }
    }
}

/// Один монитор заставки.
struct Out {
    rect: Rect,
    /// Готовый фон; `None` — ровный `color`.
    base: Option<Surface>,
    color: Color,
}

/// Заставка на экране — слой стола (`Compositor::saver_set`).
pub struct Shown {
    outs: Vec<Out>,
    mask: Mask,
    /// Место часов в свободном поле монитора, 0..=1000 по каждой оси, и шаг.
    pos: (i32, i32),
    vel: (i32, i32),
    preview: bool,
    /// Прежние фоны, пока идёт смена фото, и сколько нового видно (из 255).
    old: Option<Vec<Out>>,
    fade: u32,
}

/// Смещение тени надписи.
const SHADOW: i32 = 3;

fn blend(pixel: u32, target: u32, alpha: u32) -> u32 {
    let mut out = pixel & 0xFF00_0000;
    for shift in [0, 8, 16] {
        let c = ((pixel >> shift) & 0xFF) as i32;
        let t = ((target >> shift) & 0xFF) as i32;
        let v = c + (t - c) * alpha as i32 / 255;
        out |= (v.clamp(0, 255) as u32) << shift;
    }
    out
}

impl Shown {
    /// Где надпись на мониторе `out`.
    fn overlay(&self, out: Rect) -> Rect {
        let margin = out.h as i32 / 14;
        let room_x = (out.w as i32 - self.mask.w as i32 - 2 * margin).max(0);
        let room_y = (out.h as i32 - self.mask.h as i32 - 2 * margin).max(0);
        Rect::new(
            out.x + margin + room_x * self.pos.0 / 1000,
            out.y + margin + room_y * self.pos.1 / 1000,
            self.mask.w,
            self.mask.h,
        )
    }

    /// То, что надпись занимает вместе с тенью, на всех мониторах.
    fn overlay_damage(&self) -> Vec<Rect> {
        if self.mask.w == 0 {
            return Vec::new();
        }
        self.outs
            .iter()
            .map(|out| {
                let at = self.overlay(out.rect);
                Rect::new(at.x, at.y, at.w + SHADOW as u32, at.h + SHADOW as u32)
            })
            .collect()
    }

    /// Сдвинуть надпись на шаг; что перерисовать.
    fn drift(&mut self) -> Vec<Rect> {
        let mut damage = self.overlay_damage();
        for (pos, vel) in [(&mut self.pos.0, &mut self.vel.0), (&mut self.pos.1, &mut self.vel.1)] {
            *pos += *vel;
            if *pos <= 0 || *pos >= 1000 {
                *vel = -*vel;
                *pos = (*pos).clamp(0, 1000);
            }
        }
        damage.extend(self.overlay_damage());
        damage
    }

    /// Собрать полосу кадра `band` (координаты стола) в `back`, строка `y`
    /// стола — строка `y + dy` буфера.
    pub fn compose(&self, back: &mut Surface, band: Rect, dy: i32) {
        back.fill(Rect::new(band.x, band.y + dy, band.w, band.h), BLACK);
        for out in &self.outs {
            let part = band.intersect(&out.rect);
            if part.is_empty() {
                continue;
            }
            match &out.base {
                Some(base) => back.blit_from(base, (part.x, part.y + dy), part.translate(-out.rect.x, -out.rect.y)),
                None => back.fill(Rect::new(part.x, part.y + dy, part.w, part.h), out.color),
            }
            // Смена фото: прежний фон поверх, всё прозрачнее.
            if let Some(old) = self.old.as_ref().and_then(|old| old.iter().find(|o| o.rect == out.rect)) {
                let alpha = 255 - self.fade.min(255);
                if alpha > 0 {
                    let color = old.color.pixel();
                    for y in part.y..part.bottom() {
                        let from = old.base.as_ref().map(|base| &base.row((y - out.rect.y) as u32)[(part.x - out.rect.x) as usize..]);
                        let row = back.row_mut((y + dy) as u32);
                        let Some(line) = row.get_mut(part.x as usize..part.right() as usize) else {
                            continue;
                        };
                        for (i, pixel) in line.iter_mut().enumerate() {
                            let was = from.and_then(|f| f.get(i).copied()).unwrap_or(color);
                            *pixel = blend(*pixel, was, alpha);
                        }
                    }
                }
            }
            if self.mask.w == 0 {
                continue;
            }
            let at = self.overlay(out.rect);
            let area = Rect::new(at.x, at.y, at.w + SHADOW as u32, at.h + SHADOW as u32).intersect(&part);
            for y in area.y..area.bottom() {
                let row = back.row_mut((y + dy) as u32);
                for x in area.x..area.right() {
                    let Some(pixel) = row.get_mut(x as usize) else {
                        continue;
                    };
                    let (mx, my) = (x - at.x, y - at.y);
                    let shadow = u32::from(self.mask.at(mx - SHADOW, my - SHADOW));
                    let ink = u32::from(self.mask.at(mx, my));
                    if shadow == 0 && ink == 0 {
                        continue;
                    }
                    let mut p = *pixel;
                    if shadow > 0 {
                        p = blend(p, 0, shadow * 3 / 5);
                    }
                    if ink > 0 {
                        p = blend(p, 0x00FF_FFFF, ink * INK / 255);
                    }
                    *pixel = p;
                }
            }
        }
    }
}

/// Растянуть `src` целиком в прямоугольник `to` поверхности `dst`
/// (билинейно, в неподвижной точке 16.16: ядро без FPU).
fn scale_into(src: &Surface, dst: &mut Surface, to: Rect) {
    let (sw, sh) = (u64::from(src.width()), u64::from(src.height()));
    if to.w == 0 || to.h == 0 || sw == 0 || sh == 0 {
        return;
    }
    let step_x = (sw << 16) / u64::from(to.w);
    let step_y = (sh << 16) / u64::from(to.h);
    let (max_x, max_y) = ((sw - 1) << 16, (sh - 1) << 16);
    let mut cols = Vec::new();
    if cols.try_reserve_exact(to.w as usize).is_err() {
        return;
    }
    for x in 0..u64::from(to.w) {
        cols.push((step_x * x + step_x / 2).saturating_sub(1 << 15).min(max_x));
    }
    for ty in 0..to.h {
        let y = to.y + ty as i32;
        if y < 0 || y >= dst.height() as i32 {
            continue;
        }
        let fy = (step_y * u64::from(ty) + step_y / 2).saturating_sub(1 << 15).min(max_y);
        let (r0, wy) = ((fy >> 16) as u32, ((fy & 0xFFFF) >> 8) as u32);
        let r1 = (r0 + 1).min(sh as u32 - 1);
        let (row0, row1) = (src.row(r0), src.row(r1));
        let line = dst.row_mut(y as u32);
        for (i, fx) in cols.iter().enumerate() {
            let x = to.x + i as i32;
            if x < 0 || x as usize >= line.len() {
                continue;
            }
            let (c0, wx) = ((fx >> 16) as usize, ((fx & 0xFFFF) >> 8) as u32);
            let c1 = (c0 + 1).min(sw as usize - 1);
            let (a, b, c, d) = (row0[c0], row0[c1], row1[c0], row1[c1]);
            let mut out = a & 0xFF00_0000;
            for shift in [0, 8, 16] {
                let ch = |p: u32| (p >> shift) & 0xFF;
                let top = ch(a) * (256 - wx) + ch(b) * wx;
                let bottom = ch(c) * (256 - wx) + ch(d) * wx;
                out |= ((top * (256 - wy) + bottom * wy) >> 16) << shift;
            }
            line[x as usize] = out;
        }
    }
}

/// Нарисовать фото из файла в `out` по способу `fit`. `false` — не вышло.
fn paint_file(path: &str, out: &mut Surface, fit: Fit) -> bool {
    let bytes = match crate::fs::read(path, MAX_PHOTO) {
        Some(Ok((bytes, _))) => bytes,
        _ => {
            kprintln!("  saver       : {path} unreadable");
            return false;
        }
    };
    let Some(picture) = Picture::decode(&bytes) else {
        kprintln!("  saver       : {path} is not a QOI picture (or no memory)");
        return false;
    };
    drop(bytes);
    if fit == Fit::Fill {
        picture.cover(out);
        return true;
    }
    // Same size: `cover` is a plain copy - the picture as it is, to scale.
    let Some(mut raw) = Surface::new(picture.width, picture.height, BLACK) else {
        return false;
    };
    picture.cover(&mut raw);
    drop(picture);
    let (w, h) = (out.width(), out.height());
    match fit {
        Fit::Stretch => scale_into(&raw, out, out.bounds()),
        Fit::Fit => {
            let (pw, ph) = (u64::from(raw.width()), u64::from(raw.height()));
            let (dw, dh) = if pw * u64::from(h) > ph * u64::from(w) {
                (w, (ph * u64::from(w) / pw) as u32)
            } else {
                ((pw * u64::from(h) / ph) as u32, h)
            };
            scale_into(&raw, out, Rect::new(((w - dw) / 2) as i32, ((h - dh) / 2) as i32, dw, dh));
        }
        Fit::Center | Fit::Fill => {
            out.blit_from(&raw, ((w as i32 - raw.width() as i32) / 2, (h as i32 - raw.height() as i32) / 2), raw.bounds());
        }
    }
    true
}

/// Фоны мониторов: `index` — с какого фото начать (у каждого монитора своё).
fn build_outs(rects: &[Rect], index: usize) -> (Vec<Out>, Option<String>) {
    let settings = settings();
    let color = Color::rgb((settings.color >> 16) as u8, (settings.color >> 8) as u8, settings.color as u8);
    let list = if settings.kind == Kind::Photo { Some(playlist().0) } else { None };
    let mut first = None;
    let mut fallback = false;
    let outs = rects
        .iter()
        .enumerate()
        .map(|(n, rect)| {
            let base = list.as_ref().and_then(|list| {
                let mut surface = Surface::new(rect.w, rect.h, BLACK)?;
                // Своё фото монитора, а не переведено ещё — любое готовое из
                // списка (по порядку от него).
                let len = list.len();
                let ready = (0..len.min(WINDOW)).find_map(|k| {
                    let source = &list[(index + n + k) % len];
                    ready_file(source, rect.w, rect.h).map(|file| (source.clone(), file, k))
                });
                if len > 0 && ready.as_ref().is_none_or(|(_, _, k)| *k > 0) {
                    fallback = true;
                }
                if n == 0 {
                    first = ready.as_ref().map(|(source, _, _)| source.clone());
                }
                let painted = ready.is_some_and(|(_, file, _)| paint_file(&file, &mut surface, settings.fit)) || super::wallart::paint(&mut surface);
                if !painted {
                    return None;
                }
                let whole = surface.bounds();
                draw::blend_rect(&mut surface, whole, BLACK, DIM);
                Some(surface)
            });
            Out { rect: *rect, base, color: if list.is_some() { Color::rgb(0x10, 0x18, 0x28) } else { color } }
        })
        .collect();
    FALLBACK.store(fallback, Ordering::Relaxed);
    (outs, first)
}

/// «суббота, 22 ноября».
fn date_text() -> Option<String> {
    const MONTHS: [&str; 12] =
        ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    const DAYS: [&str; 7] = ["понедельник", "вторник", "среда", "четверг", "пятница", "суббота", "воскресенье"];
    let now = crate::time::now_local()?;
    let days = calendar::days_from_civil(i64::from(now.year), now.month, now.day);
    Some(format!("{}, {} {}", DAYS[(days + 3).rem_euclid(7) as usize], now.day, MONTHS[usize::from(now.month.max(1) - 1) % 12]))
}

/// Прошлый замер загрузки: (мс, тики простоя).
static CPU_SAMPLE: SpinLock<(u64, u64)> = SpinLock::new((0, 0));

/// «ЦП 3 %   Память 1,2 из 7,8 ГБ».
fn info_text() -> String {
    let now = crate::time::uptime_ms();
    let idle = crate::sched::idle_ticks_total();
    let (then, idle_then) = core::mem::replace(&mut *CPU_SAMPLE.lock(), (now, idle));
    let span = now.saturating_sub(then);
    let cpus = crate::smp::online().max(1) as u64;
    let ticks = span * u64::from(crate::irq::TIMER_HZ) / 1000 * cpus;
    let busy = if then == 0 || ticks == 0 { 0 } else { 100 - (idle.saturating_sub(idle_then) * 100 / ticks).min(100) };
    let frames = crate::mm::frame::stats();
    let total = frames.total_bytes() as u64;
    let used = total.saturating_sub(frames.free_bytes() as u64);
    let size = |bytes: u64| {
        if total < 4 << 30 {
            return format!("{} МБ", bytes >> 20);
        }
        let tenth = bytes * 10 / (1 << 30);
        format!("{},{} ГБ", tenth / 10, tenth % 10)
    };
    format!("ЦП {busy} %    Память {} из {}", size(used), size(total))
}

/// The saver's TrueType face (Inter, as the interface), loaded while the
/// saver is up and dropped with it (876 KiB is not worth keeping for a
/// screen that is shown now and then). `Some(None)` - tried, not there.
static FONT: SpinLock<Option<Option<Box<ttf::Font>>>> = SpinLock::new(None);

/// Where the face lives (`cbuild::SYSTEM_FONTS`).
const FONT_PATH: &str = "/usr/share/fonts/fossai/Inter.ttf";

/// Take the saver's face out of [`FONT`] (loading it the first time) - out
/// of the spin lock, so rasterizing does not run with interrupts off. Put
/// it back with [`put_font`].
fn take_font() -> Option<Box<ttf::Font>> {
    let known = FONT.lock().take();
    match known {
        Some(font) => font,
        None => {
            let font = match crate::fs::read(FONT_PATH, 4 << 20) {
                Some(Ok((bytes, _))) => ttf::Font::parse(bytes).map(Box::new),
                _ => None,
            };
            kprintln!("  saver       : clock face {}", if font.is_some() { "Inter (TrueType, at the monitor's size)" } else { "built-in (no Inter.ttf)" });
            font
        }
    }
}

fn put_font(font: Option<Box<ttf::Font>>) {
    *FONT.lock() = Some(font);
}

/// `f32::ceil` without `std`.
fn ceil(x: f32) -> i32 {
    let t = x as i32;
    if (t as f32) < x { t + 1 } else { t }
}

/// Надпись заставки для монитора высотой `height` и ключ её содержимого.
/// `known` - the key of the shown one: the same - `None`, nothing to build
/// (the clock is checked every second, and rasterizing it anew each time
/// costs for nothing).
fn build_mask(height: u32, known: &str) -> Option<(Mask, String)> {
    let clock = CLOCK.load(Ordering::Relaxed);
    let info = INFO.load(Ordering::Relaxed);
    let clock_text = clock.then(|| crate::time::clock_text().unwrap_or_else(|| String::from("FossAi")));
    let date = if clock { date_text() } else { None };
    let info_line = info.then(info_text);
    let key = format!("{clock_text:?}|{date:?}|{info_line:?}|{height}");
    if key == known {
        return None;
    }
    let font = take_font();
    let mask = font.as_deref().map_or_else(
        || zoomed_mask(height, clock_text.as_deref(), date.as_deref(), info_line.as_deref()),
        |font| {
            // Sizes as the zoomed text had (a fifth of the monitor for the
            // clock), now in real pixels of this monitor.
            let h = height as f32;
            let mut parts = Vec::new();
            if let Some(text) = &clock_text {
                parts.extend(Mask::ttf(font, text, h / 5.0));
            }
            if let Some(text) = &date {
                parts.extend(Mask::ttf(font, text, (h / 22.0).max(18.0)));
            }
            if let Some(text) = &info_line {
                parts.extend(Mask::ttf(font, text, (h / 40.0).max(14.0)));
            }
            Mask::stack(parts, height / 45)
        },
    );
    put_font(font);
    Some((mask, key))
}

/// The inscription from the built-in faces zoomed (no TrueType face).
fn zoomed_mask(height: u32, clock_text: Option<&str>, date: Option<&str>, info_line: Option<&str>) -> Mask {
    let scale = if height >= 1000 { mini_ui::Scale::TWO } else { mini_ui::Scale::ONE };
    let ctx = Ctx::scaled(scale);
    let heading = u32::from(ctx.face(Role::Heading).line).max(1);
    let body = u32::from(ctx.face(Role::Body).line).max(1);
    let mut parts = Vec::new();
    // Clock and date from the largest face there is: the less zoom, the
    // sharper they are.
    let big = Ctx::scaled(mini_ui::Scale::TWO);
    let big_line = u32::from(big.face(Role::Heading).line).max(1);
    if let Some(text) = clock_text {
        parts.extend(Mask::text(big, Role::Heading, text, (height / 5 / big_line).clamp(2, 8)));
    }
    if let Some(text) = date {
        let (face, zoom) = if height >= 600 { (big, (height / 22 / big_line).clamp(1, 3)) } else { (ctx, (height / 20 / heading).clamp(1, 3)) };
        parts.extend(Mask::text(face, Role::Heading, text, zoom));
    }
    if let Some(text) = info_line {
        parts.extend(Mask::text(ctx, Role::Body, text, (height / 34 / body).clamp(1, 2)));
    }
    Mask::stack(parts, height / 45)
}

// ---------------------------------------------------------------------------
// Показ (зовёт `power::idle_tick`, вне замка стола)
// ---------------------------------------------------------------------------

/// Заставка на экране.
static UP: AtomicBool = AtomicBool::new(false);
/// Мониторы, под которые собрана заставка, и высота надписи.
static RECTS: SpinLock<Vec<Rect>> = SpinLock::new(Vec::new());

/// Показана ли заставка.
#[must_use]
pub fn is_up() -> bool {
    UP.load(Ordering::Relaxed)
}
/// Ключ показанной надписи (сменился — пересобрать).
static KEY: SpinLock<String> = SpinLock::new(String::new());
static INDEX: AtomicUsize = AtomicUsize::new(0);
static NEXT_PHOTO_MS: AtomicU64 = AtomicU64::new(0);
static MOVED_MS: AtomicU64 = AtomicU64::new(0);
static CHECKED_MS: AtomicU64 = AtomicU64::new(0);
/// На экране замена (фото ещё не переведено): готово — пересобрать фоны.
static FALLBACK: AtomicBool = AtomicBool::new(false);
static REBUILD: AtomicBool = AtomicBool::new(false);
/// Когда началась смена фото (мс), 0 — не идёт.
static FADE_AT: AtomicU64 = AtomicU64::new(0);

/// Показать заставку. `false` — не вышло (стола нет или он занят): срок
/// проверится снова на следующем витке.
pub fn start(preview: bool) -> bool {
    let started = crate::time::uptime_ms();
    let Some(rects) = super::with_desktop(|desktop| desktop.output_rects().to_vec()) else {
        return false;
    };
    if rects.is_empty() {
        return false;
    }
    let height = rects.iter().map(|r| r.h).min().unwrap_or(720);
    let index = INDEX.load(Ordering::Relaxed);
    let (outs, first) = build_outs(&rects, index);
    let Some((mask, key)) = build_mask(height, "") else {
        return false;
    };
    let shown = Shown { outs, mask, pos: (120, 700), vel: (1, -1), preview, old: None, fade: 255 };
    FADE_AT.store(0, Ordering::Relaxed);
    REBUILD.store(false, Ordering::Relaxed);
    let count = rects.len();
    let installed = super::with_desktop(move |desktop| {
        desktop.saver_set(Some(shown));
        desktop.present();
    })
    .is_some();
    if !installed {
        return false;
    }
    UP.store(true, Ordering::Relaxed);
    mark(false);
    *RECTS.lock() = rects;
    *KEY.lock() = key;
    let now = crate::time::uptime_ms();
    NEXT_PHOTO_MS.store(now + u64::from(EVERY.load(Ordering::Relaxed)) * 1000, Ordering::Relaxed);
    MOVED_MS.store(now, Ordering::Relaxed);
    CHECKED_MS.store(now, Ordering::Relaxed);
    let s = settings();
    let what = match s.kind {
        Kind::Photo => format!("photo {} from {}, {}", first.as_deref().unwrap_or("fossai"), s.source, s.fit.tag()),
        Kind::Color => format!("color #{:06x}", s.color),
    };
    kprintln!(
        "  saver       : shown{} ({what}, clock {}) on {count} monitor(s) in {} ms",
        if preview { " as a preview" } else { "" },
        on_off(s.clock),
        now.saturating_sub(started)
    );
    true
}

/// Убрать заставку; с «экраном блокировки при выходе» — закрыть стол им.
/// `false` — стол занят, попробовать на следующем витке.
pub fn stop() -> bool {
    super::with_desktop(|desktop| {
        end_on(desktop);
        desktop.present();
    })
    .is_some()
}

/// Убрать заставку со стола `desktop` — сразу, на том же вводе, что её
/// закончил (`ui::dispatch_on`, `pointer_on`): следующая клавиша уже идёт
/// экрану блокировки или программе, а не в никуда.
pub fn end_on(desktop: &mut super::compositor::Compositor) {
    UP.store(false, Ordering::Relaxed);
    let Some(old) = desktop.saver_set(None) else {
        return;
    };
    let locks = LOCK.load(Ordering::Relaxed) && !old.preview;
    drop(old);
    let font = FONT.lock().take();
    drop(font);
    if locks && !desktop.locked() {
        desktop.lock_screen();
    }
    kprintln!("  saver       : ended{}", if locks { ", the lock screen is up" } else { "" });
    if locks {
        kprintln!("  desktop     : locked by the screensaver");
    }
}

/// Виток показа: сменить фото, обновить часы, сдвинуть надпись.
pub fn tick() {
    let now = crate::time::uptime_ms();
    // Слайд-шоу: фоны собираются вне замка стола (распаковка — десятки мс).
    let next = PHOTO.load(Ordering::Relaxed) && now >= NEXT_PHOTO_MS.load(Ordering::Relaxed);
    // Фото перевелось, а на экране замена — поставить его, не дожидаясь срока.
    let rebuild = PHOTO.load(Ordering::Relaxed) && REBUILD.swap(false, Ordering::Relaxed);
    if next || rebuild {
        NEXT_PHOTO_MS.store(now + u64::from(EVERY.load(Ordering::Relaxed)) * 1000, Ordering::Relaxed);
        let rects = RECTS.lock().clone();
        let count = playlist().0.len();
        if (rebuild || count > 1) && !rects.is_empty() {
            let index = if rebuild { INDEX.load(Ordering::Relaxed) } else { INDEX.fetch_add(rects.len(), Ordering::Relaxed) + rects.len() };
            // Окно переведённых фото сдвинулось: следующие — заранее.
            mark(false);
            let (outs, first) = build_outs(&rects, index);
            let swapped = super::with_desktop(move |desktop| {
                let Some(shown) = desktop.saver_mut() else {
                    return false;
                };
                // Прежний фон уходит плавно (FADE_MS); идущая смена — сразу.
                shown.old = Some(core::mem::replace(&mut shown.outs, outs));
                shown.fade = 0;
                desktop.repaint_all();
                desktop.present();
                true
            });
            if swapped == Some(true) {
                FADE_AT.store(now.max(1), Ordering::Relaxed);
                kprintln!("  saver       : {} picture {}", if rebuild { "prepared" } else { "next" }, first.unwrap_or_default());
            }
        }
    }
    // Смена фото: шаг затухания прежнего на каждом витке.
    let fade_at = FADE_AT.load(Ordering::Relaxed);
    if fade_at != 0 {
        let elapsed = now.saturating_sub(fade_at);
        let fade = if elapsed >= FADE_MS { 255 } else { (elapsed * 255 / FADE_MS) as u32 };
        if fade >= 255 {
            FADE_AT.store(0, Ordering::Relaxed);
        }
        super::with_desktop(move |desktop| {
            let Some(shown) = desktop.saver_mut() else {
                return;
            };
            shown.fade = fade;
            if fade >= 255 {
                shown.old = None;
            }
            desktop.repaint_all();
            desktop.present();
        });
    }
    let mut mask = None;
    if now >= CHECKED_MS.load(Ordering::Relaxed) + 1000 {
        CHECKED_MS.store(now, Ordering::Relaxed);
        let height = RECTS.lock().iter().map(|r| r.h).min().unwrap_or(720);
        let known = KEY.lock().clone();
        if let Some((fresh, key)) = build_mask(height, &known) {
            *KEY.lock() = key;
            mask = Some(fresh);
        }
    }
    let drift = now >= MOVED_MS.load(Ordering::Relaxed) + DRIFT_MS;
    if drift {
        MOVED_MS.store(now, Ordering::Relaxed);
    }
    if mask.is_none() && !drift {
        return;
    }
    super::with_desktop(move |desktop| {
        let Some(shown) = desktop.saver_mut() else {
            return;
        };
        let mut damage = Vec::new();
        if let Some(mask) = mask {
            damage.extend(shown.overlay_damage());
            shown.mask = mask;
            damage.extend(shown.overlay_damage());
        }
        if drift {
            damage.extend(shown.drift());
        }
        for rect in damage {
            desktop.saver_damage(rect);
        }
        desktop.present();
    });
}

/// Почему заставке и гашению сейчас нельзя: `Some(Some(причина))` —
/// держат; `Some(None)` — можно; `None` — стол занят, спросить позже.
#[must_use]
pub fn inhibited() -> Option<Option<&'static str>> {
    if crate::power::inhibitors() > 0 {
        return Some(Some("a Wayland client holds an idle inhibitor"));
    }
    super::with_desktop(|desktop| desktop.fullscreen_front().then_some("a fullscreen window is in front"))
}

/// Нажатые клавиши, разбудившие экран: их повтор и отпускание тоже не
/// доходят до программ.
static SWALLOWED: SpinLock<Vec<crate::input::KeyCode>> = SpinLock::new(Vec::new());

/// Съесть ли клавишу: `dark` — экран погашен или показана заставка.
pub fn swallow_key(dark: bool, event: KeyEvent) -> bool {
    let mut list = SWALLOWED.lock();
    let known = list.iter().position(|code| *code == event.code);
    if event.pressed {
        if known.is_some() {
            return true;
        }
        if dark {
            list.push(event.code);
            kprintln!("  saver       : key swallowed (it only wakes the screen)");
            return true;
        }
        return false;
    }
    match known {
        Some(at) => {
            list.remove(at);
            true
        }
        None => dark,
    }
}
