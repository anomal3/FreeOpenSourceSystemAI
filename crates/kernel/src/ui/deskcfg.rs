//! Настройки панели задач и «Пуска» одним местом (20.11.2026).
//!
//! # Зачем отдельный модуль
//!
//! Роман попросил «свой SDK» для Dock и панели: один набор чисел, из которого
//! и панель, и композитор, и «Параметры» берут, где стоит полоса, прячется ли
//! она и какого размера значки. Пока это были разрозненные атомики в
//! `panel.rs`, каждое новое свойство тянуло за собой правку в трёх местах и
//! своё имя строки в `desktop.cfg`. Здесь — одна структура, одна строка файла
//! на панель (`taskbar=`), одна на «Пуск» (`start=`) и два списка закреплённых
//! (`start_pins=`, `taskbar_pins=`).
//!
//! Формат строки — пары `ключ:значение` через пробел: незнакомая пара
//! пропускается, недостающая берёт умолчание. Так файл, записанный системой
//! поновее, не ломает старую, а старый файл без строки даёт прежний вид.

use alloc::string::String;
use alloc::vec::Vec;

use core::sync::atomic::{AtomicU32, Ordering};

use crate::sync::SpinLock;

/// Вид полосы.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Панель во всю сторону экрана: кнопка «Пуск», кнопки окон, трей.
    Panel,
    /// Плавающая плашка по центру стороны: значки программ, часы.
    Dock,
}

/// Сторона экрана, у которой стоит полоса.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Bottom,
    Top,
    Left,
    Right,
}

impl Edge {
    pub const ALL: [Edge; 4] = [Edge::Bottom, Edge::Top, Edge::Left, Edge::Right];

    /// Полоса стоит вдоль боковой стороны — раскладка столбцом.
    #[must_use]
    pub const fn vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }

    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Edge::Bottom => "bottom",
            Edge::Top => "top",
            Edge::Left => "left",
            Edge::Right => "right",
        }
    }

    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Edge::Bottom => "Снизу",
            Edge::Top => "Сверху",
            Edge::Left => "Слева",
            Edge::Right => "Справа",
        }
    }

    #[must_use]
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|edge| edge.tag() == tag)
    }
}

/// Размеры значков дока, точки при масштабе 1.
pub const ICON_SIZES: [u32; 4] = [32, 40, 48, 56];

/// Настройки полосы.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Taskbar {
    pub mode: Mode,
    pub edge: Edge,
    /// Прятать полосу, пока указатель не коснётся её края экрана. Окна тогда
    /// получают весь экран.
    pub autohide: bool,
    /// Сторона значка в доке (одна из [`ICON_SIZES`]).
    pub icon: u32,
}

impl Taskbar {
    pub const DEFAULT: Self = Self { mode: Mode::Panel, edge: Edge::Bottom, autohide: false, icon: 40 };

    fn parse(text: &str) -> Self {
        let mut out = Self::DEFAULT;
        for pair in text.split_whitespace() {
            let Some((key, value)) = pair.split_once(':') else { continue };
            match key {
                "mode" => {
                    out.mode = match value {
                        "dock" => Mode::Dock,
                        _ => Mode::Panel,
                    }
                }
                "edge" => out.edge = Edge::from_tag(value).unwrap_or(Edge::Bottom),
                "autohide" => out.autohide = value == "on",
                "icon" => {
                    if let Ok(size) = value.parse::<u32>() {
                        out.icon = size.clamp(ICON_SIZES[0], ICON_SIZES[ICON_SIZES.len() - 1]);
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn format(self) -> String {
        alloc::format!(
            "mode:{} edge:{} autohide:{} icon:{}",
            if self.mode == Mode::Dock { "dock" } else { "panel" },
            self.edge.tag(),
            if self.autohide { "on" } else { "off" },
            self.icon
        )
    }
}

/// Настройки «Пуска».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Start {
    /// Широкий «Пуск»: больше столбцов закреплённых и шире список.
    pub wide: bool,
    pub pinned: bool,
    pub all: bool,
    pub recent: bool,
}

impl Start {
    pub const DEFAULT: Self = Self { wide: false, pinned: true, all: true, recent: true };

    fn parse(text: &str) -> Self {
        let mut out = Self::DEFAULT;
        for pair in text.split_whitespace() {
            let Some((key, value)) = pair.split_once(':') else { continue };
            let on = value == "on";
            match key {
                "width" => out.wide = value == "wide",
                "pinned" => out.pinned = on,
                "all" => out.all = on,
                "recent" => out.recent = on,
                _ => {}
            }
        }
        // Пустой «Пуск» не бывает: без обеих колонок остаётся список.
        if !out.pinned && !out.all {
            out.all = true;
        }
        out
    }

    fn format(self) -> String {
        let flag = |on: bool| if on { "on" } else { "off" };
        alloc::format!(
            "width:{} pinned:{} all:{} recent:{}",
            if self.wide { "wide" } else { "normal" },
            flag(self.pinned),
            flag(self.all),
            flag(self.recent)
        )
    }
}

static TASKBAR: SpinLock<Taskbar> = SpinLock::new(Taskbar::DEFAULT);
static START: SpinLock<Start> = SpinLock::new(Start::DEFAULT);
/// Закреплённое в «Пуске» — ключи манифеста ([`super::startapps`]) и
/// `pkg:имя` пакетов, по порядку. `None` — человек ничего не менял, берётся
/// [`super::startapps::DEFAULT_PINS`].
static START_PINS: SpinLock<Option<Vec<String>>> = SpinLock::new(None);
/// Закреплённое на панели задач — те же ключи.
static TASK_PINS: SpinLock<Option<Vec<String>>> = SpinLock::new(None);

#[must_use]
pub fn taskbar() -> Taskbar {
    *TASKBAR.lock()
}

#[must_use]
pub fn start() -> Start {
    *START.lock()
}

/// Закреплённое в «Пуске».
#[must_use]
pub fn start_pins() -> Vec<String> {
    START_PINS.lock().clone().unwrap_or_else(|| super::startapps::DEFAULT_PINS.iter().map(|key| String::from(*key)).collect())
}

/// Закреплённое на панели задач.
#[must_use]
pub fn task_pins() -> Vec<String> {
    TASK_PINS.lock().clone().unwrap_or_else(|| super::startapps::DEFAULT_TASK_PINS.iter().map(|key| String::from(*key)).collect())
}

/// Закреплённое на панели в этом виде. Пока человек ничего не закреплял,
/// у панели закреплённых нет (кнопки окон стоят там же, где стояли до
/// 20.11), а у Dock — набор по умолчанию: пустой Dock — это одна кнопка.
#[must_use]
pub fn task_pins_for(mode: Mode) -> Vec<String> {
    // Список копируется отдельной строкой: замок, взятый прямо в `match`,
    // живёт до конца всего `match`, и ветка Dock (`task_pins` берёт тот же
    // замок) вешала стол намертво — «Панель → Dock» без своих закреплённых
    // (железо, 20.11).
    let saved = TASK_PINS.lock().clone();
    match (saved, mode) {
        (Some(list), _) => list,
        (None, Mode::Dock) => task_pins(),
        (None, Mode::Panel) => Vec::new(),
    }
}

/// Закреплена ли программа на панели в этом виде.
#[must_use]
pub fn task_pinned(key: &str) -> bool {
    task_pins_for(taskbar().mode).iter().any(|known| known == key)
}

/// Закреплена ли программа в «Пуске».
#[must_use]
pub fn start_pinned(key: &str) -> bool {
    start_pins().iter().any(|known| known == key)
}

/// Поколение списка закреплённых на панели: панель перечитывает его, когда
/// число сменилось.
static PIN_GEN: AtomicU32 = AtomicU32::new(0);

#[must_use]
pub fn task_pins_generation() -> u32 {
    PIN_GEN.load(Ordering::Relaxed)
}

fn parse_list(text: &str) -> Vec<String> {
    text.split('|').map(str::trim).filter(|key| !key.is_empty()).map(String::from).collect()
}

/// Прочитать из текста `desktop.cfg` (зовёт [`super::prefs::adopt`]).
pub fn adopt(text: &str, path: &str) {
    if let Some(line) = sysconf::value(text, "taskbar") {
        let parsed = Taskbar::parse(line);
        *TASKBAR.lock() = parsed;
        crate::kprintln!("  desktop     : taskbar {} from {path}", parsed.format());
    }
    if let Some(line) = sysconf::value(text, "start") {
        let parsed = Start::parse(line);
        *START.lock() = parsed;
        crate::kprintln!("  desktop     : start {} from {path}", parsed.format());
    }
    if let Some(line) = sysconf::value(text, "start_pins") {
        *START_PINS.lock() = Some(parse_list(line));
    }
    if let Some(line) = sysconf::value(text, "taskbar_pins") {
        *TASK_PINS.lock() = Some(parse_list(line));
        PIN_GEN.fetch_add(1, Ordering::Relaxed);
    }
}

/// Сменить настройки полосы и запомнить. Ошибка записи не отменяет смену:
/// живая система с корнем только для чтения работает и без файла.
pub fn set_taskbar(value: Taskbar) -> Result<(), crate::vfs::VfsError> {
    *TASKBAR.lock() = value;
    // Закреплённые по умолчанию у панели и Dock разные.
    PIN_GEN.fetch_add(1, Ordering::Relaxed);
    super::prefs::store_line("taskbar", &value.format())
}

pub fn set_start(value: Start) -> Result<(), crate::vfs::VfsError> {
    let value = Start::parse(&value.format());
    *START.lock() = value;
    super::prefs::store_line("start", &value.format())
}

pub fn set_start_pins(list: Vec<String>) -> Result<(), crate::vfs::VfsError> {
    let line = list.join("|");
    *START_PINS.lock() = Some(list);
    super::prefs::store_line("start_pins", &line)
}

pub fn set_task_pins(list: Vec<String>) -> Result<(), crate::vfs::VfsError> {
    let line = list.join("|");
    *TASK_PINS.lock() = Some(list);
    PIN_GEN.fetch_add(1, Ordering::Relaxed);
    super::prefs::store_line("taskbar_pins", &line)
}

/// Строка состояния для журнала (`taskbar ...`) — её ищут сценарии.
#[must_use]
pub fn describe() -> String {
    taskbar().format()
}

/// То же для «Пуска».
#[must_use]
pub fn describe_start() -> String {
    start().format()
}

/// Закрепить или открепить программу в «Пуске». `true` — теперь закреплена.
pub fn toggle_start_pin(key: &str) -> bool {
    let mut list = start_pins();
    let pinned = if let Some(at) = list.iter().position(|known| known == key) {
        list.remove(at);
        false
    } else {
        list.push(String::from(key));
        true
    };
    if let Err(err) = set_start_pins(list) {
        crate::kprintln!("  desktop     : start pins not saved: {err}");
    }
    pinned
}

/// Поставить закреплённую в «Пуске» программу первой.
pub fn start_pin_first(key: &str) {
    let mut list = start_pins();
    if let Some(at) = list.iter().position(|known| known == key) {
        let item = list.remove(at);
        list.insert(0, item);
        if let Err(err) = set_start_pins(list) {
            crate::kprintln!("  desktop     : start pins not saved: {err}");
        }
    }
}

/// Закрепить или открепить программу на панели задач. `true` — закреплена.
pub fn toggle_task_pin(key: &str) -> bool {
    let mut list = task_pins_for(taskbar().mode);
    let pinned = if let Some(at) = list.iter().position(|known| known == key) {
        list.remove(at);
        false
    } else {
        list.push(String::from(key));
        true
    };
    if let Err(err) = set_task_pins(list) {
        crate::kprintln!("  desktop     : taskbar pins not saved: {err}");
    }
    pinned
}

/// Что меняют «Параметры» на странице «Dock и панель задач».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskbarDeed {
    Mode(Mode),
    Edge(Edge),
    Autohide(bool),
    Icon(u32),
    ClearPins,
}

/// Что меняют «Параметры» на странице «Пуск».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StartDeed {
    Wide(bool),
    Pinned(bool),
    All(bool),
    Recent(bool),
    ResetPins,
}

/// Применить и запомнить; строка — что сказать человеку.
pub fn apply_taskbar(deed: TaskbarDeed) -> (String, Result<(), crate::vfs::VfsError>) {
    let mut value = taskbar();
    let text = match deed {
        TaskbarDeed::Mode(mode) => {
            value.mode = mode;
            String::from(if mode == Mode::Dock { "панель стала Dock" } else { "Dock стал панелью" })
        }
        TaskbarDeed::Edge(edge) => {
            value.edge = edge;
            alloc::format!("панель — {}", edge.title().to_lowercase())
        }
        TaskbarDeed::Autohide(on) => {
            value.autohide = on;
            String::from(if on { "панель прячется, пока к краю не подвели указатель" } else { "панель видна всегда" })
        }
        TaskbarDeed::Icon(size) => {
            value.icon = size;
            alloc::format!("значки Dock — {size} точек")
        }
        TaskbarDeed::ClearPins => {
            return (String::from("закреплённые откреплены"), set_task_pins(Vec::new()));
        }
    };
    (text, set_taskbar(value))
}

pub fn apply_start(deed: StartDeed) -> (String, Result<(), crate::vfs::VfsError>) {
    let mut value = start();
    let text = match deed {
        StartDeed::Wide(wide) => {
            value.wide = wide;
            String::from(if wide { "«Пуск» широкий" } else { "«Пуск» обычной ширины" })
        }
        StartDeed::Pinned(on) => {
            value.pinned = on;
            String::from(if on { "закреплённые показываются" } else { "закреплённые скрыты" })
        }
        StartDeed::All(on) => {
            value.all = on;
            String::from(if on { "список программ показывается" } else { "список программ скрыт" })
        }
        StartDeed::Recent(on) => {
            value.recent = on;
            String::from(if on { "недавние показываются" } else { "недавние скрыты" })
        }
        StartDeed::ResetPins => {
            let list = super::startapps::DEFAULT_PINS.iter().map(|key| String::from(*key)).collect();
            return (String::from("закреплённые — как было"), set_start_pins(list));
        }
    };
    (text, set_start(value))
}

/// `ui taskbar …` оболочки (стенд, 20.11): слова `panel`/`dock`,
/// `bottom`/`top`/`left`/`right`, `autohide`/`show`, число — размер значков.
pub fn command(words: &str) -> Taskbar {
    let mut value = taskbar();
    for word in words.split_whitespace() {
        match word {
            "panel" => value.mode = Mode::Panel,
            "dock" => value.mode = Mode::Dock,
            "autohide" => value.autohide = true,
            "show" => value.autohide = false,
            other => {
                if let Some(edge) = Edge::from_tag(other) {
                    value.edge = edge;
                } else if let Ok(size) = other.parse::<u32>() {
                    value.icon = size.clamp(ICON_SIZES[0], ICON_SIZES[ICON_SIZES.len() - 1]);
                }
            }
        }
    }
    if let Err(err) = set_taskbar(value) {
        crate::kprintln!("  desktop     : taskbar not saved: {err}");
    }
    value
}

/// `ui start …`: `wide`/`normal`, `pinned`/`nopinned`, `all`/`noall`,
/// `recent`/`norecent`.
pub fn start_command(words: &str) {
    let mut value = start();
    for word in words.split_whitespace() {
        match word {
            "wide" => value.wide = true,
            "normal" => value.wide = false,
            "pinned" => value.pinned = true,
            "nopinned" => value.pinned = false,
            "all" => value.all = true,
            "noall" => value.all = false,
            "recent" => value.recent = true,
            "norecent" => value.recent = false,
            _ => {}
        }
    }
    if let Err(err) = set_start(value) {
        crate::kprintln!("  desktop     : start not saved: {err}");
    }
    crate::kprintln!("  desktop     : start {}", describe_start());
}
