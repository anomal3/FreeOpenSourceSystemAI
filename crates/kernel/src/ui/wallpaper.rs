//! Фон стола (22.11.2026): галерея картинок, своя картинка, сплошной цвет и
//! расположение — «Параметры → Персонализация → Фон», как в любой системе.
//!
//! # Кто что разбирает
//!
//! Галерея (`/usr/share/wallpapers`) — JPEG: пять картинок 2816×1536 весят
//! в JPEG 3,4 МиБ, а в QOI — 25; RAM-диск и флешка этого не стоят. Ядро же
//! читает только QOI (`mini_ui::qoi`): JPEG и PNG разбирает программа
//! `/bin/wallconv`, и уменьшает так, чтобы картинка закрывала 4K, не больше.
//! Результат ложится в кэш ([`cache_dir`]) под ключом — FNV-1a пути
//! источника; есть файл — второй раз программа не нужна.
//!
//! # Когда считается картинка
//!
//! Только при выборе и при смене размера монитора, а не на кадре: готовые обои
//! каждого монитора стол хранит сам (`Compositor::wall`), а ключ этого кэша
//! включает [`generation`] — номер выбора. Масштаб — усреднение по площади
//! целыми блоками, пока картинка больше вдвое, затем билинейный (см.
//! [`scale_into`]).
//!
//! # Что переживает перезагрузку
//!
//! Строки `background=`, `background_fit=`, `background_color=` в
//! `/etc/desktop.cfg` — на живой флешке их увозит `SETTINGS.DAT`. Кэш — нет
//! (`/var` флешки в памяти): после перезагрузки `wallconv` переводит картинку
//! заново, секунду-две стол показывает прежние обои FossAi. Своя картинка
//! хранится путём к файлу: лежала она в `/root` живой флешки — после
//! перезагрузки её нет, и стол молча возвращается к FossAi (ROADMAP).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use mini_ui::qoi::Picture;
use mini_ui::{Color, Rect, Surface};

use crate::kprintln;
use crate::sync::SpinLock;

/// Каталог галереи: картинки, `<имя>.thumb.qoi` и `index.txt` с подписями.
pub const GALLERY: &str = "/usr/share/wallpapers";

/// Как картинка ложится на монитор — пять способов Windows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fit {
    /// Закрыть экран целиком, лишнее по краям срезать.
    Fill,
    /// Вписать целиком, по краям — поля цвета фона.
    Fit,
    /// Растянуть на весь экран без сохранения пропорций.
    Stretch,
    /// Замостить в натуральную величину от левого верхнего угла.
    Tile,
    /// В натуральную величину посередине, вокруг — цвет фона.
    Center,
}

impl Fit {
    pub const ALL: [Fit; 5] = [Fit::Fill, Fit::Fit, Fit::Stretch, Fit::Tile, Fit::Center];

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Fit::Fill => "fill",
            Fit::Fit => "fit",
            Fit::Stretch => "stretch",
            Fit::Tile => "tile",
            Fit::Center => "center",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Fit::Fill => "Заполнение",
            Fit::Fit => "По размеру",
            Fit::Stretch => "Растянуть",
            Fit::Tile => "Замостить",
            Fit::Center => "По центру",
        }
    }

    fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|fit| fit.tag() == tag)
    }
}

/// Что на фоне.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    /// Обои темы (`mini_ui::theme::Wallpaper`): FossAi или градиент.
    Preset,
    /// Картинка: путь к источнику (галерея или своя).
    Image(String),
    /// Сплошной цвет [`State::color`].
    Color,
}

/// Что меняют на странице «Фон».
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum WallDeed {
    /// Картинка — путь к источнику.
    Image(String),
    /// «Обзор…» — своя картинка через окно выбора файла.
    Browse,
    /// Сплошной цвет `0xRRGGBB`.
    Color(u32),
    Fit(Fit),
    /// «Классический бирюзовый фон» (22.11, «Оформление» в классике):
    /// включить — сплошной #008080, прежний фон запоминается; выключить —
    /// вернуть прежний.
    ClassicTeal(bool),
    /// «Также для экрана блокировки» (24.11): `true` — экран блокировки
    /// с тем же фоном, что и стол; `false` — свой ([`LockDeed`]).
    LockSame(bool),
    /// Свой фон экрана блокировки.
    Lock(LockDeed),
}

/// Что выбирают на странице «Экран блокировки» (24.11).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LockDeed {
    /// Обои FossAi.
    Fossai,
    /// Картинка — путь к источнику.
    Image(String),
    /// «Обзор…».
    Browse,
    /// Сплошной цвет `0xRRGGBB`.
    Color(u32),
}

/// Бирюзовый стол Windows 95/98.
pub const CLASSIC_TEAL: u32 = 0x008080;

/// Цвета «Сплошного цвета» (и полей у «По размеру» / «По центру»).
pub const COLORS: [u32; 10] = [
    0x0F172A, 0x1E3A5F, 0x0E4D64, 0x14532D, 0x3B0764, 0x701A75, 0x7C2D12, 0x374151, 0x000000, 0xCBD5E1,
];

/// Перевод картинки программой.
struct Job {
    task: crate::sched::TaskId,
    /// `None` — «Обзор…» (что выбрано, скажет `picked.txt`).
    source: Option<String>,
    /// Какой файл кэша она обязана оставить.
    output: String,
    started: u64,
    /// «Обзор…» на странице «Экран блокировки»: выбранное — фону экрана
    /// блокировки, не стола.
    for_lock: bool,
}

struct State {
    choice: Choice,
    fit: Fit,
    /// Цвет фона `0xRRGGBB`: сплошной и поля.
    color: u32,
    /// Распакованная картинка и путь, которому она принадлежит.
    picture: Option<(String, Arc<Picture>)>,
    /// Перевести этот источник при ближайшем [`poll`].
    wanted: Option<String>,
    job: Option<Job>,
    /// Источники, которые в эту загрузку не перевелись: не пытаться снова.
    failed: Vec<String>,
    /// Что сказать на странице «Фон» (готовится, не вышло).
    note: Option<String>,
    /// Каталог кэша, когда уже найден.
    cache: Option<&'static str>,
    /// Фон до «Классического бирюзового» (строка `background=` и цвет):
    /// его возвращает выключение (`background_saved=` в `desktop.cfg`).
    saved: Option<(String, u32)>,
    /// Экран блокировки с фоном стола (24.11, по умолчанию так).
    lock_same: bool,
    /// Свой фон экрана блокировки: `Preset` — обои FossAi.
    lock_choice: Choice,
    /// Цвет своего фона экрана блокировки.
    lock_color: u32,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    lock_same: true,
    lock_choice: Choice::Preset,
    lock_color: COLORS[0],
    choice: Choice::Preset,
    fit: Fit::Fill,
    color: COLORS[0],
    picture: None,
    wanted: None,
    job: None,
    failed: Vec::new(),
    note: None,
    cache: None,
    saved: None,
});

/// Номер выбора: входит в ключ готовых обоев стола.
static GENERATION: AtomicU32 = AtomicU32::new(1);

/// Миниатюры «Фона»: ключ (путь или `fossai`), размер и готовая картинка.
static THUMBS: SpinLock<Vec<(String, u32, u32, Surface)>> = SpinLock::new(Vec::new());

/// Номер выбора — сменился, значит готовые обои стола устарели.
#[must_use]
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

fn bump() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
}

#[must_use]
pub fn choice() -> Choice {
    STATE.lock().choice.clone()
}

#[must_use]
pub fn fit() -> Fit {
    STATE.lock().fit
}

#[must_use]
pub fn color() -> u32 {
    STATE.lock().color
}

/// Строка состояния для страницы «Фон».
#[must_use]
pub fn note() -> Option<String> {
    STATE.lock().note.clone()
}

/// Идёт ли перевод картинки.
#[must_use]
pub fn busy() -> bool {
    let state = STATE.lock();
    state.job.is_some() || state.wanted.is_some()
}

fn rgb(color: u32) -> Color {
    Color::rgb((color >> 16) as u8, (color >> 8) as u8, color as u8)
}

/// Подписи на столе белые, с тенью: фон — картинка или тёмный цвет.
#[must_use]
pub fn white_labels() -> bool {
    let state = STATE.lock();
    match state.choice {
        Choice::Image(_) => true,
        Choice::Color => {
            let c = rgb(state.color);
            u32::from(c.r) * 299 + u32::from(c.g) * 587 + u32::from(c.b) * 114 < 140_000
        }
        Choice::Preset => mini_ui::theme::wallpaper() == mini_ui::theme::Wallpaper::Fossai,
    }
}

/// Прочитать выбор из `desktop.cfg` (при загрузке и после возврата настроек
/// живой флешки).
pub fn adopt(text: &str, path: &str) {
    let mut state = STATE.lock();
    if let Some(color) = sysconf::value(text, "background_color").and_then(|hex| u32::from_str_radix(hex.trim().trim_start_matches('#'), 16).ok()) {
        state.color = color & 0xFF_FFFF;
    }
    if let Some(fit) = sysconf::value(text, "background_fit").and_then(|tag| Fit::from_tag(tag.trim())) {
        state.fit = fit;
    }
    // `background_saved=<background>;<цвет>` — пусто, когда сохранять нечего.
    state.saved = sysconf::value(text, "background_saved").and_then(|line| {
        let (value, hex) = line.trim().rsplit_once(';')?;
        Some((value.to_string(), u32::from_str_radix(hex.trim(), 16).ok()? & 0xFF_FFFF))
    });
    if let Some(value) = sysconf::value(text, "background") {
        let value = value.trim();
        state.choice = if let Some(image) = value.strip_prefix("image:") {
            Choice::Image(image.to_string())
        } else if value == "color" {
            Choice::Color
        } else {
            Choice::Preset
        };
        kprintln!("  desktop     : background {} ({}, #{:06x}) from {path}", value, state.fit.tag(), state.color);
    }
    // Экран блокировки (24.11): `same`, `fossai`, `color`, `image:<путь>`.
    if let Some(color) = sysconf::value(text, "lock_background_color").and_then(|hex| u32::from_str_radix(hex.trim().trim_start_matches('#'), 16).ok()) {
        state.lock_color = color & 0xFF_FFFF;
    }
    if let Some(value) = sysconf::value(text, "lock_background_same") {
        state.lock_same = value.trim() != "off";
    }
    if let Some(value) = sysconf::value(text, "lock_background") {
        let value = value.trim();
        state.lock_choice = if let Some(image) = value.strip_prefix("image:") {
            Choice::Image(image.to_string())
        } else if value == "color" {
            Choice::Color
        } else {
            Choice::Preset
        };
        kprintln!("  desktop     : lock screen background {value} ({}) from {path}", if state.lock_same { "not used: same as the desktop" } else { "own" });
    }
    drop(state);
    bump();
    bump_lock();
}

/// Выбраны обои темы (FossAi или градиент): картинка и цвет больше не главные.
/// `true` — фон действительно сменился.
pub fn use_preset() -> bool {
    let mut state = STATE.lock();
    if state.choice == Choice::Preset {
        return false;
    }
    state.choice = Choice::Preset;
    state.note = None;
    drop(state);
    bump();
    let _ = super::prefs::store_line("background", "preset");
    true
}

/// Выполнить действие страницы «Фон»: что сказать и записалось ли.
pub fn apply(deed: WallDeed) -> (String, Result<(), crate::vfs::VfsError>) {
    match deed {
        WallDeed::Image(path) => {
            {
                let mut state = STATE.lock();
                state.choice = Choice::Image(path.clone());
                state.failed.retain(|known| *known != path);
                state.note = None;
            }
            bump();
            kprintln!("  settings    : background image {path}");
            let result = super::prefs::store_line("background", &format!("image:{path}"));
            let ready = STATE.lock().picture.as_ref().is_some_and(|(of, _)| *of == path) || cached(&path).is_some();
            let text = if ready { String::from("картинка применена и запомнена") } else { String::from("картинка запомнена, ставится на стол…") };
            (text, result)
        }
        WallDeed::Browse => {
            STATE.lock().wanted = None;
            let text = match start_pick(false) {
                Ok(()) => String::from("выберите картинку в окне «Открыть»"),
                Err(why) => why,
            };
            (text, Ok(()))
        }
        WallDeed::Color(color) => {
            {
                let mut state = STATE.lock();
                state.choice = Choice::Color;
                state.color = color;
                state.note = None;
            }
            bump();
            kprintln!("  settings    : background color #{color:06x}");
            let hex = format!("{color:06x}");
            let result = super::prefs::store_line("background_color", &hex).and_then(|()| super::prefs::store_line("background", "color"));
            (format!("цвет #{} применён и запомнен", hex.to_uppercase()), result)
        }
        WallDeed::Fit(fit) => {
            STATE.lock().fit = fit;
            bump();
            kprintln!("  settings    : background fit {}", fit.tag());
            (format!("расположение «{}» запомнено", fit.title()), super::prefs::store_line("background_fit", fit.tag()))
        }
        WallDeed::ClassicTeal(on) => classic_teal(on),
        WallDeed::LockSame(same) => {
            STATE.lock().lock_same = same;
            bump_lock();
            kprintln!("  settings    : lock screen background {}", if same { "same as the desktop" } else { "own" });
            let text = String::from(if same { "экран блокировки — с фоном рабочего стола" } else { "у экрана блокировки свой фон: «Экран блокировки»" });
            (text, super::prefs::store_line("lock_background_same", if same { "on" } else { "off" }))
        }
        WallDeed::Lock(deed) => apply_lock(deed),
    }
}

/// Строка `lock_background=` своего фона.
fn lock_line() -> String {
    let state = STATE.lock();
    match &state.lock_choice {
        Choice::Preset => String::from("fossai"),
        other => choice_line(other),
    }
}

fn apply_lock(deed: LockDeed) -> (String, Result<(), crate::vfs::VfsError>) {
    let text = match deed {
        LockDeed::Browse => {
            STATE.lock().wanted = None;
            let text = match start_pick(true) {
                Ok(()) => String::from("выберите картинку в окне «Открыть»"),
                Err(why) => why,
            };
            return (text, Ok(()));
        }
        LockDeed::Fossai => {
            STATE.lock().lock_choice = Choice::Preset;
            String::from("экран блокировки: обои FossAi")
        }
        LockDeed::Image(path) => {
            let mut state = STATE.lock();
            state.failed.retain(|known| *known != path);
            state.lock_choice = Choice::Image(path);
            String::from("картинка экрана блокировки запомнена")
        }
        LockDeed::Color(color) => {
            let mut state = STATE.lock();
            state.lock_choice = Choice::Color;
            state.lock_color = color & 0xFF_FFFF;
            format!("экран блокировки: цвет #{:06X}", color & 0xFF_FFFF)
        }
    };
    STATE.lock().lock_same = false;
    bump_lock();
    let line = lock_line();
    kprintln!("  settings    : lock screen background {line}");
    let color = format!("{:06x}", STATE.lock().lock_color);
    let result = super::prefs::store_line("lock_background_color", &color)
        .and_then(|()| super::prefs::store_line("lock_background", &line))
        .and_then(|()| super::prefs::store_line("lock_background_same", "off"));
    (text, result)
}

// ── Экран блокировки (24.11) ────────────────────────────────────────────────

/// Номер выбора фона экрана блокировки: сменился — готовый фон устарел.
static LOCK_GENERATION: AtomicU32 = AtomicU32::new(1);

/// Готовый фон экрана блокировки: номер выбора, размер, картинка. Живёт,
/// пока экран заблокирован ([`lock_done`]).
static LOCK_WALL: SpinLock<Option<(u32, u32, u32, Surface)>> = SpinLock::new(None);

fn bump_lock() {
    LOCK_GENERATION.fetch_add(1, Ordering::Relaxed);
}

/// Тот же ли фон у экрана блокировки, что и у стола.
#[must_use]
pub fn lock_same() -> bool {
    STATE.lock().lock_same
}

/// Свой фон экрана блокировки: выбор и цвет.
#[must_use]
pub fn lock_choice() -> (Choice, u32) {
    let state = STATE.lock();
    (state.lock_choice.clone(), state.lock_color)
}

/// Нарисовать свой фон экрана блокировки в `surface` целиком. `false` —
/// фон тот же, что у стола, или своя картинка ещё не готова: рисует стол.
pub fn paint_lock(surface: &mut Surface) -> bool {
    let (same, choice, color) = {
        let state = STATE.lock();
        (state.lock_same, state.lock_choice.clone(), state.lock_color)
    };
    if same {
        return false;
    }
    let generation = LOCK_GENERATION.load(Ordering::Relaxed);
    let (w, h) = (surface.width(), surface.height());
    let bounds = surface.bounds();
    {
        let cache = LOCK_WALL.lock();
        if let Some((made, cw, ch, picture)) = cache.as_ref() {
            if *made == generation && *cw == w && *ch == h {
                surface.blit_from(picture, (0, 0), bounds);
                return true;
            }
        }
    }
    let render = |target: &mut Surface| -> bool {
        match &choice {
            Choice::Color => {
                let all = target.bounds();
                target.fill(all, rgb(color));
                true
            }
            Choice::Preset => super::wallart::paint(target),
            Choice::Image(source) => match lock_picture(source) {
                Some(decoded) => {
                    place(&decoded, target, Fit::Fill, rgb(color));
                    true
                }
                None => false,
            },
        }
    };
    // The ready copy costs a whole screen of kernel heap; without room for
    // it (64 MiB heap, fragmented) the background is drawn straight into the
    // lock screen on every redraw instead.
    let Some(mut picture) = Surface::new(w, h, rgb(color)) else {
        let drawn = render(surface);
        if drawn {
            kprintln!("  desktop     : lock screen background {} {w}x{h} (no room for a ready copy)", choice_line(&choice));
        }
        return drawn;
    };
    if !render(&mut picture) {
        return false;
    }
    kprintln!("  desktop     : lock screen background {} {w}x{h}", choice_line(&choice));
    surface.blit_from(&picture, (0, 0), bounds);
    *LOCK_WALL.lock() = Some((generation, w, h, picture));
    true
}

/// `ui lockbg same|fossai|color RRGGBB|image PATH` — фон экрана блокировки
/// из оболочки (стенд, серийная линия), то же, что страница «Экран блокировки».
pub fn lock_command(words: &str) {
    let mut it = words.split_whitespace();
    let deed = match (it.next(), it.next()) {
        (Some("same"), _) => Some(WallDeed::LockSame(true)),
        (Some("fossai"), _) => Some(WallDeed::Lock(LockDeed::Fossai)),
        (Some("color"), Some(hex)) => u32::from_str_radix(hex.trim_start_matches('#'), 16).ok().map(|c| WallDeed::Lock(LockDeed::Color(c))),
        (Some("image"), Some(path)) => Some(WallDeed::Lock(LockDeed::Image(path.to_string()))),
        _ => None,
    };
    let Some(deed) = deed else {
        kprintln!("  usage: ui lockbg same|fossai|color RRGGBB|image PATH");
        return;
    };
    let (text, result) = apply(deed);
    match result {
        Ok(()) => kprintln!("  lockbg      : {text}"),
        Err(err) => kprintln!("  lockbg      : {text} (not saved: {err})"),
    }
}

/// Экран разблокирован: готовый фон больше не нужен (десяток мегабайт).
pub fn lock_done() {
    *LOCK_WALL.lock() = None;
}

/// Картинка своего фона экрана блокировки из кэша; нет — заказать перевод.
/// Не запоминается: распакованная живёт, пока готовится фон.
fn lock_picture(source: &str) -> Option<Arc<Picture>> {
    {
        let state = STATE.lock();
        if let Some((of, picture)) = state.picture.as_ref() {
            if of == source {
                return Some(picture.clone());
            }
        }
        if state.failed.iter().any(|known| known == source) {
            return None;
        }
    }
    let path = cache_path(source);
    let bytes = match crate::fs::read(&path, 64 * 1024 * 1024) {
        Some(Ok((bytes, _))) => bytes,
        _ => {
            let mut state = STATE.lock();
            if state.job.as_ref().and_then(|job| job.source.as_deref()) != Some(source) {
                state.wanted = Some(source.to_string());
            }
            return None;
        }
    };
    Picture::decode(&bytes).map(Arc::new)
}

/// Стоит ли «Классический бирюзовый фон».
#[must_use]
pub fn classic_teal_on() -> bool {
    let state = STATE.lock();
    state.choice == Choice::Color && state.color == CLASSIC_TEAL
}

/// Строка `background=` для выбора.
fn choice_line(choice: &Choice) -> String {
    match choice {
        Choice::Preset => String::from("preset"),
        Choice::Color => String::from("color"),
        Choice::Image(path) => format!("image:{path}"),
    }
}

/// Включить или выключить бирюзовый стол Windows 98 (22.11): включение
/// запоминает прежний фон, выключение его возвращает — обои FossAi, свою
/// картинку или цвет — со всеми строками `desktop.cfg`.
fn classic_teal(on: bool) -> (String, Result<(), crate::vfs::VfsError>) {
    if on {
        let (line, color) = {
            let mut state = STATE.lock();
            if state.choice == Choice::Color && state.color == CLASSIC_TEAL {
                return (String::from("бирюзовый фон уже стоит"), Ok(()));
            }
            let saved = (choice_line(&state.choice), state.color);
            state.saved = Some(saved.clone());
            state.choice = Choice::Color;
            state.color = CLASSIC_TEAL;
            state.note = None;
            saved
        };
        bump();
        kprintln!("  settings    : background classic teal on (was {line}, #{color:06x})");
        let result = super::prefs::store_line("background_saved", &format!("{line};{color:06x}"))
            .and_then(|()| super::prefs::store_line("background_color", &format!("{CLASSIC_TEAL:06x}")))
            .and_then(|()| super::prefs::store_line("background", "color"));
        return (String::from("классический бирюзовый фон включён; выключите — вернутся прежние обои"), result);
    }
    let saved = STATE.lock().saved.take();
    let (line, color) = saved.unwrap_or_else(|| (String::from("preset"), COLORS[0]));
    {
        let mut state = STATE.lock();
        state.color = color;
        state.choice = if let Some(path) = line.strip_prefix("image:") {
            Choice::Image(path.to_string())
        } else if line == "color" {
            Choice::Color
        } else {
            Choice::Preset
        };
        state.note = None;
    }
    bump();
    kprintln!("  settings    : background classic teal off (back to {line}, #{color:06x})");
    let result = super::prefs::store_line("background_saved", "")
        .and_then(|()| super::prefs::store_line("background_color", &format!("{color:06x}")))
        .and_then(|()| super::prefs::store_line("background", &line));
    (String::from("прежний фон возвращён"), result)
}

// ── Кэш и программа перевода ────────────────────────────────────────────────

/// Каталог кэша переведённых картинок: `/var/cache/wallpaper`, а где `/var`
/// нет (запуск стендом без установки) — `/tmp/wallpaper`. Писать туда должна
/// программа сеанса, поэтому каталог открыт всем на запись, как `/tmp`.
pub(super) fn cache_dir() -> &'static str {
    if let Some(dir) = STATE.lock().cache {
        return dir;
    }
    let root = crate::vfs::perm::Credentials::ROOT;
    let mut found = "/tmp/wallpaper";
    for (parent, dir) in [("/var/cache", "/var/cache/wallpaper"), ("/tmp", "/tmp/wallpaper")] {
        let _ = crate::fs::mkdir_as(root, parent, 0o755);
        let _ = crate::fs::mkdir_as(root, dir, 0o1777);
        let _ = crate::fs::chmod_as(root, dir, 0o1777);
        if matches!(crate::fs::list(dir), Some(Ok(_))) {
            found = dir;
            break;
        }
    }
    STATE.lock().cache = Some(found);
    found
}

/// FNV-1a, 64 бита (тот же счёт у `wallconv pick`).
pub(super) fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Самый большой монитор, на который фон уже рисовался (`ш << 32 | в`).
static LARGEST: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Под какой размер переводить картинку: самый большой монитор (основной или
/// уже нарисованный), не больше 4K. Ядру не держать лишнего: на машине до
/// 1 ГБ его куча — 64 МиБ, а 4K в точках — 25.
fn target() -> (u32, u32) {
    let (sw, sh) = super::screen_size();
    let seen = LARGEST.load(Ordering::Relaxed);
    let (w, h) = (sw.max((seen >> 32) as u32), sh.max(seen as u32));
    if w == 0 || h == 0 {
        return (1920, 1080);
    }
    (w.min(3840), h.min(2160))
}

fn cache_path(source: &str) -> String {
    let (w, h) = target();
    format!("{}/{:016x}-{w}x{h}.qoi", cache_dir(), fnv1a64(source.as_bytes()))
}

/// Переведённая картинка источника, если она уже есть: путь к ней.
fn cached(source: &str) -> Option<String> {
    let path = cache_path(source);
    matches!(crate::fs::read(&path, 16), Some(Ok(_))).then_some(path)
}

/// Путь в строке запуска: пробел — `%20`, `%` — `%25`.
fn pct(path: &str) -> String {
    super::panel::pct(path)
}

fn start_pick(for_lock: bool) -> Result<(), String> {
    if STATE.lock().job.is_some() {
        return Err(String::from("подождите: предыдущая картинка ещё готовится"));
    }
    let dir = cache_dir();
    let _ = crate::fs::remove_as(crate::vfs::perm::Credentials::ROOT, &format!("{dir}/picked.txt"));
    let (w, h) = target();
    let line = format!("/bin/wallconv pick {} {w} {h}", pct(dir));
    match crate::user::spawn(&line, crate::user::session::credentials()) {
        Ok(task) => {
            kprintln!("  desktop     : started '{line}' as {task}");
            let mut state = STATE.lock();
            state.job = Some(Job { task, source: None, output: String::new(), started: crate::time::uptime_ms(), for_lock });
            state.note = Some(String::from("Выберите картинку в окне «Открыть»…"));
            Ok(())
        }
        Err(err) => {
            kprintln!("  desktop     : cannot start '{line}': {err}");
            Err(format!("окно выбора не открылось: {err}"))
        }
    }
}

fn start_convert(source: &str) {
    let output = cache_path(source);
    let (w, h) = target();
    let line = format!("/bin/wallconv conv {} {} {w} {h}", pct(source), pct(&output));
    match crate::user::spawn(&line, crate::user::session::credentials()) {
        Ok(task) => {
            kprintln!("  desktop     : started '{line}' as {task}");
            let mut state = STATE.lock();
            state.job = Some(Job { task, source: Some(source.to_string()), output, started: crate::time::uptime_ms(), for_lock: false });
            state.note = Some(String::from("Картинка готовится…"));
        }
        Err(err) => {
            kprintln!("  desktop     : cannot start '{line}': {err}");
            let mut state = STATE.lock();
            state.failed.push(source.to_string());
            state.note = Some(format!("Картинку не перевести: {err}"));
        }
    }
}

/// «Сделать фоном рабочего стола» from a program (24.11, «Фотографии»): the
/// path of the picture in this key of the user scope of the settings store.
/// The desktop takes it on its next tick, removes the key and applies the
/// picture as «Параметры → Фон» would.
pub const REQUEST_KEY: &str = "desktop/background_request";

/// Store change counter last looked at for [`REQUEST_KEY`].
static REQUEST_SEQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Take a background asked by a program. `true` — the background changed.
fn take_request() -> bool {
    use crate::config::registry::{self, Scope};
    let seq = registry::seq();
    if REQUEST_SEQ.swap(seq, Ordering::Relaxed) == seq {
        return false;
    }
    let Some(path) = registry::get_in(Scope::User, REQUEST_KEY) else {
        return false;
    };
    let _ = registry::set(Scope::User, REQUEST_KEY, None);
    REQUEST_SEQ.store(registry::seq(), Ordering::Relaxed);
    let path = path.trim();
    if !path.starts_with('/') || !matches!(crate::fs::read(path, 16), Some(Ok(_))) {
        kprintln!("  desktop     : background asked for '{path}': no such file");
        return false;
    }
    kprintln!("  desktop     : background asked by a program: {path}");
    let (_, result) = apply(WallDeed::Image(String::from(path)));
    if let Err(err) = result {
        kprintln!("  desktop     : background {path} applied, not stored: {err}");
    }
    true
}

/// Работа вне кадра (из `ui::tick`): запустить перевод, дождаться его. `true`
/// — фон сменился или сменилась строка состояния: перерисовать стол и «Фон».
pub fn poll() -> bool {
    if take_request() {
        return true;
    }
    let (wanted, job) = {
        let mut state = STATE.lock();
        let wanted = if state.job.is_none() { state.wanted.take() } else { None };
        let job = state.job.as_ref().map(|job| (job.task, job.source.clone(), job.output.clone(), job.started, job.for_lock));
        (wanted, job)
    };
    if let Some(source) = wanted {
        start_convert(&source);
        return true;
    }
    let Some((task, source, output, started, for_lock)) = job else {
        return false;
    };
    let running = matches!(crate::sched::lookup(task), Some((_, state)) if state != crate::sched::TaskState::Finished);
    if running {
        return false;
    }
    let took = crate::time::uptime_ms().saturating_sub(started);
    // A converted picture may be the lock screen's: its background redraws.
    bump_lock();
    let mut state = STATE.lock();
    state.job = None;
    match source {
        Some(source) => {
            drop(state);
            if matches!(crate::fs::read(&output, 16), Some(Ok(_))) {
                kprintln!("  desktop     : wallpaper {source} converted in {took} ms");
                STATE.lock().note = None;
            } else {
                kprintln!("  desktop     : wallpaper {source} did not convert");
                let mut state = STATE.lock();
                state.failed.push(source);
                state.note = Some(String::from("Эту картинку прочитать не удалось — показаны прежние обои"));
            }
        }
        None => {
            drop(state);
            let note = format!("{}/picked.txt", cache_dir());
            let picked = match crate::fs::read(&note, 4096) {
                Some(Ok((bytes, _))) => String::from_utf8(bytes).ok().map(|text| text.trim().to_string()).filter(|text| !text.is_empty()),
                _ => None,
            };
            match picked {
                Some(path) => {
                    kprintln!("  desktop     : wallpaper picked {path} in {took} ms{}", if for_lock { " for the lock screen" } else { "" });
                    let deed = if for_lock { WallDeed::Lock(LockDeed::Image(path)) } else { WallDeed::Image(path) };
                    let (_, result) = apply(deed);
                    STATE.lock().note = match result {
                        Ok(()) => None,
                        Err(err) => Some(format!("Картинка применена, но не запомнена: {err}")),
                    };
                }
                None => {
                    kprintln!("  desktop     : wallpaper pick cancelled");
                    STATE.lock().note = None;
                }
            }
        }
    }
    bump();
    true
}

// ── Рисование ───────────────────────────────────────────────────────────────

/// Распакованная картинка выбранного источника — из кэша. Нет в кэше —
/// заказать перевод ([`poll`]) и ответить `None`.
fn picture_for(source: &str) -> Option<Arc<Picture>> {
    {
        let state = STATE.lock();
        if let Some((of, picture)) = state.picture.as_ref() {
            if of == source {
                return Some(picture.clone());
            }
        }
        if state.failed.iter().any(|known| known == source) {
            return None;
        }
        // Прежняя картинка остаётся на столе, пока новая готовится; уходит,
        // когда новая распакуется.
    }
    let path = cache_path(source);
    let started = crate::time::uptime_ms();
    // Есть ли файл — сначала дёшево: читать целиком только готовый.
    if !matches!(crate::fs::read(&path, 16), Some(Ok(_))) {
        let mut state = STATE.lock();
        if state.job.as_ref().and_then(|job| job.source.as_deref()) != Some(source) {
            state.wanted = Some(source.to_string());
        }
        return None;
    }
    // Новая картинка распакуется рядом со старой — старую отдать заранее.
    STATE.lock().picture = None;
    let bytes = match crate::fs::read(&path, 64 * 1024 * 1024) {
        Some(Ok((bytes, _))) => bytes,
        _ => {
            let mut state = STATE.lock();
            if state.job.as_ref().and_then(|job| job.source.as_deref()) != Some(source) {
                state.wanted = Some(source.to_string());
            }
            return None;
        }
    };
    let Some(picture) = Picture::decode(&bytes) else {
        kprintln!("  desktop     : wallpaper cache {path} did not unpack; converting again");
        let _ = crate::fs::remove_as(crate::vfs::perm::Credentials::ROOT, &path);
        STATE.lock().wanted = Some(source.to_string());
        return None;
    };
    kprintln!(
        "  desktop     : wallpaper {source} {}x{} unpacked in {} ms",
        picture.width,
        picture.height,
        crate::time::uptime_ms().saturating_sub(started)
    );
    let picture = Arc::new(picture);
    STATE.lock().picture = Some((source.to_string(), picture.clone()));
    Some(picture)
}

/// Нарисовать фон монитора в `surface` целиком (готовые обои монитора).
/// `false` — рисовать нечего: стол нарисует градиент темы.
pub fn paint(surface: &mut Surface) -> bool {
    let seen = LARGEST.load(Ordering::Relaxed);
    let (w, h) = (surface.width().max((seen >> 32) as u32), surface.height().max(seen as u32));
    LARGEST.store((u64::from(w) << 32) | u64::from(h), Ordering::Relaxed);
    let (choice, fit, color) = {
        let state = STATE.lock();
        (state.choice.clone(), state.fit, state.color)
    };
    match choice {
        Choice::Color => {
            let bounds = surface.bounds();
            surface.fill(bounds, rgb(color));
            true
        }
        Choice::Image(source) => match picture_for(&source) {
            Some(picture) => {
                let started = crate::time::uptime_ms();
                place(&picture, surface, fit, rgb(color));
                kprintln!(
                    "  desktop     : wallpaper {}x{} {} -> {}x{} in {} ms",
                    picture.width,
                    picture.height,
                    fit.tag(),
                    surface.width(),
                    surface.height(),
                    crate::time::uptime_ms().saturating_sub(started)
                );
                true
            }
            // Пока картинка готовится — прежняя картинка, а нет её — обои
            // темы.
            None => match STATE.lock().picture.as_ref().map(|(_, picture)| picture.clone()) {
                Some(previous) => {
                    place(&previous, surface, fit, rgb(color));
                    true
                }
                None => preset(surface),
            },
        },
        Choice::Preset => preset(surface),
    }
}

fn preset(surface: &mut Surface) -> bool {
    mini_ui::theme::wallpaper() == mini_ui::theme::Wallpaper::Fossai && super::wallart::paint(surface)
}

/// Положить картинку на монитор способом `fit`, поля — цветом `back`.
fn place(picture: &Picture, surface: &mut Surface, fit: Fit, back: Color) {
    let (sw, sh) = (u64::from(picture.width), u64::from(picture.height));
    let (dw, dh) = (u64::from(surface.width()), u64::from(surface.height()));
    if dw == 0 || dh == 0 {
        return;
    }
    let centered = |w: u64, h: u64| Rect::new(((dw as i64 - w as i64) / 2) as i32, ((dh as i64 - h as i64) / 2) as i32, w.max(1) as u32, h.max(1) as u32);
    match fit {
        Fit::Fill => {
            // Меньший из множителей, при котором закрыты обе стороны.
            let (w, h) = if sw * dh < sh * dw { (dw, (sh * dw).div_ceil(sw)) } else { ((sw * dh).div_ceil(sh), dh) };
            scale_into(picture, surface, centered(w, h));
        }
        Fit::Fit => {
            let (w, h) = if sw * dh > sh * dw { (dw, (sh * dw / sw).max(1)) } else { ((sw * dh / sh).max(1), dh) };
            let bounds = surface.bounds();
            surface.fill(bounds, back);
            scale_into(picture, surface, centered(w, h));
        }
        Fit::Stretch => {
            let bounds = surface.bounds();
            scale_into(picture, surface, bounds);
        }
        Fit::Center => {
            let bounds = surface.bounds();
            surface.fill(bounds, back);
            scale_into(picture, surface, centered(sw, sh));
        }
        Fit::Tile => {
            let mut y = 0;
            while y < dh {
                let mut x = 0;
                while x < dw {
                    scale_into(picture, surface, Rect::new(x as i32, y as i32, sw as u32, sh as u32));
                    x += sw;
                }
                y += sh;
            }
        }
    }
}

/// Уменьшить в `k` раз по каждой стороне усреднением блоков `k×k`.
fn reduce(picture: &Picture, k: u32) -> Option<Picture> {
    let (w, h) = (picture.width / k, picture.height / k);
    if w == 0 || h == 0 {
        return None;
    }
    let src = picture.rgb();
    let stride = picture.width as usize * 3;
    let mut out = Vec::new();
    out.try_reserve_exact((w * h * 3) as usize).ok()?;
    let area = k * k;
    let mut sums: Vec<u32> = Vec::new();
    sums.try_reserve_exact(w as usize * 3).ok()?;
    sums.resize(w as usize * 3, 0);
    for y in 0..h as usize {
        sums.iter_mut().for_each(|sum| *sum = 0);
        for row in 0..k as usize {
            let line = &src[(y * k as usize + row) * stride..][..stride];
            for x in 0..w as usize {
                let at = x * k as usize * 3;
                for dx in 0..k as usize {
                    let px = &line[at + dx * 3..at + dx * 3 + 3];
                    sums[x * 3] += u32::from(px[0]);
                    sums[x * 3 + 1] += u32::from(px[1]);
                    sums[x * 3 + 2] += u32::from(px[2]);
                }
            }
        }
        out.extend(sums.iter().map(|sum| ((sum + area / 2) / area) as u8));
    }
    Picture::from_rgb(w, h, out)
}

/// Нарисовать картинку целиком в прямоугольник `dst` поверхности (может
/// выходить за её края — нарисуется видимое). Уменьшение больше чем вдвое —
/// сначала усреднением блоков (иначе билинейная выборка пропускает точки и
/// мелкие детали рябят), остаток — билинейно, в неподвижной точке.
pub fn scale_into(picture: &Picture, surface: &mut Surface, dst: Rect) {
    let visible = dst.intersect(&surface.bounds());
    if visible.is_empty() || dst.w == 0 || dst.h == 0 {
        return;
    }
    let k = (picture.width / dst.w).min(picture.height / dst.h);
    let reduced;
    let source = if k >= 2 {
        match reduce(picture, k) {
            Some(small) => {
                reduced = small;
                &reduced
            }
            None => picture,
        }
    } else {
        picture
    };
    let (sw, sh) = (u64::from(source.width), u64::from(source.height));
    let (dw, dh) = (u64::from(dst.w), u64::from(dst.h));
    let rgb = source.rgb();
    let stride = source.width as usize * 3;
    // Один к одному — просто переложить точки.
    if sw == dw && sh == dh {
        for y in visible.y..visible.bottom() {
            let sy = (y - dst.y) as usize;
            let line = surface.row_mut(y as u32);
            for x in visible.x..visible.right() {
                let at = sy * stride + (x - dst.x) as usize * 3;
                line[x as usize] = Color::rgb(rgb[at], rgb[at + 1], rgb[at + 2]).pixel();
            }
        }
        return;
    }
    let step_x = (sw << 16) / dw;
    let step_y = (sh << 16) / dh;
    let max_x = (sw - 1) << 16;
    let max_y = (sh - 1) << 16;
    let mut columns: Vec<(u32, u32, u32)> = Vec::new();
    if columns.try_reserve_exact(visible.w as usize).is_err() {
        return;
    }
    for x in visible.x..visible.right() {
        let fx = ((x - dst.x) as u64 * step_x + step_x / 2).saturating_sub(1 << 15).min(max_x);
        let col0 = (fx >> 16) as u32;
        let col1 = (col0 + 1).min(source.width - 1);
        columns.push((col0 * 3, col1 * 3, ((fx & 0xFFFF) >> 8) as u32));
    }
    for y in visible.y..visible.bottom() {
        let fy = ((y - dst.y) as u64 * step_y + step_y / 2).saturating_sub(1 << 15).min(max_y);
        let row0 = (fy >> 16) as usize;
        let row1 = (row0 + 1).min(source.height as usize - 1);
        let wy = ((fy & 0xFFFF) >> 8) as u32;
        let (top, bottom) = (&rgb[row0 * stride..][..stride], &rgb[row1 * stride..][..stride]);
        let line = surface.row_mut(y as u32);
        for (slot, &(c0, c1, wx)) in line[visible.x as usize..visible.right() as usize].iter_mut().zip(columns.iter()) {
            let (c0, c1) = (c0 as usize, c1 as usize);
            let mut out = [0u8; 3];
            for channel in 0..3 {
                let a = u32::from(top[c0 + channel]) * (256 - wx) + u32::from(top[c1 + channel]) * wx;
                let b = u32::from(bottom[c0 + channel]) * (256 - wx) + u32::from(bottom[c1 + channel]) * wx;
                out[channel] = ((a * (256 - wy) + b * wy) >> 16) as u8;
            }
            *slot = Color::rgb(out[0], out[1], out[2]).pixel();
        }
    }
}

// ── Галерея и миниатюры ─────────────────────────────────────────────────────

/// Картинка галереи: путь и подпись.
#[derive(Clone)]
pub struct Item {
    pub path: String,
    pub title: String,
}

static GALLERY_LIST: SpinLock<Option<Vec<Item>>> = SpinLock::new(None);

/// Картинки галереи по `index.txt` (читается один раз за загрузку).
#[must_use]
pub fn gallery() -> Vec<Item> {
    if let Some(list) = GALLERY_LIST.lock().as_ref() {
        return list.clone();
    }
    let mut list = Vec::new();
    if let Some(Ok((bytes, _))) = crate::fs::read(&format!("{GALLERY}/index.txt"), 16 * 1024) {
        let text = String::from_utf8_lossy(&bytes);
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (file, title) = line.split_once(' ').unwrap_or((line, line));
            let path = format!("{GALLERY}/{file}");
            if matches!(crate::fs::read(&path, 16), Some(Ok(_))) {
                list.push(Item { path, title: title.trim().to_string() });
            }
        }
    }
    *GALLERY_LIST.lock() = Some(list.clone());
    list
}

/// Ключ миниатюры обоев FossAi.
pub const FOSSAI_THUMB: &str = "fossai";

/// Нарисовать миниатюру `key` (путь источника или [`FOSSAI_THUMB`]) в `rect`.
/// `false` — миниатюры нет (своя картинка ещё не переведена).
pub fn draw_thumb(s: &mut Surface, rect: Rect, key: &str) -> bool {
    if rect.w == 0 || rect.h == 0 {
        return false;
    }
    {
        let thumbs = THUMBS.lock();
        if let Some((_, _, _, thumb)) = thumbs.iter().find(|(of, w, h, _)| of == key && *w == rect.w && *h == rect.h) {
            s.blit_from(thumb, (rect.x, rect.y), Rect::new(0, 0, rect.w, rect.h));
            return true;
        }
    }
    let gallery_thumb = || {
        let stem = key.rsplit_once('.').map_or(key, |(stem, _)| stem);
        match crate::fs::read(&format!("{stem}.thumb.qoi"), 4 * 1024 * 1024) {
            Some(Ok((bytes, _))) => Picture::decode(&bytes).map(Arc::new),
            _ => None,
        }
    };
    let picture: Option<Arc<Picture>> = if key == FOSSAI_THUMB {
        super::wallart::picture().map(Arc::new)
    } else if let Some(thumb) = key.starts_with(GALLERY).then(gallery_thumb).flatten() {
        Some(thumb)
    } else {
        // Своя картинка: из памяти, если она выбрана, иначе из кэша.
        let loaded = STATE.lock().picture.as_ref().filter(|(of, _)| of == key).map(|(_, picture)| picture.clone());
        loaded.or_else(|| {
            let path = cached(key)?;
            match crate::fs::read(&path, 64 * 1024 * 1024) {
                Some(Ok((bytes, _))) => Picture::decode(&bytes).map(Arc::new),
                _ => None,
            }
        })
    };
    let Some(picture) = picture else {
        return false;
    };
    let Some(mut thumb) = Surface::new(rect.w, rect.h, Color::rgb(0, 0, 0)) else {
        return false;
    };
    place(&picture, &mut thumb, Fit::Fill, Color::rgb(0, 0, 0));
    s.blit_from(&thumb, (rect.x, rect.y), Rect::new(0, 0, rect.w, rect.h));
    let mut thumbs = THUMBS.lock();
    // Миниатюры другого размера (сменился масштаб) больше не нужны.
    thumbs.retain(|(_, w, h, _)| *w == rect.w && *h == rect.h);
    thumbs.push((key.to_string(), rect.w, rect.h, thumb));
    true
}
