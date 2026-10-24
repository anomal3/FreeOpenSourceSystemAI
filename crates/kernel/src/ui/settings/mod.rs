//! Окно «Параметры»: то, что человек настраивает, не набирая команд.
//!
//! # Почему это окно, а не набор команд оболочки
//!
//! Все разделы доступны и из терминала: версию печатает `about`, пакеты —
//! `pkg list`, обновление — `sysupdate`. Разница не в возможностях, а в том,
//! что человеку не приходится знать имена команд. Окно ничего не умеет сверх
//! того, что есть в системе, — и это осознанно: строка, за которой ничего не
//! стоит, — обещание, которого окно не выполнит. Поэтому разделы макета
//! Романа (фаза 69b) заполнены только тем, что действительно работает; чего
//! нет, того нет и в окне.
//!
//! # Как устроено с фазы 69b
//!
//! Слева — карточка сеанса, поле «Найти параметр» и тринадцать разделов с
//! цветными значками; справа — страница. Страница — это «герой» сверху
//! (карточка машины, образцы фона, круг обновления) и группы строк-карточек:
//! значок, подпись, пояснение, справа значение, переключатель, кнопка или
//! стрелка в подраздел. У подраздела над заголовком — хлебная крошка к
//! родителю. Содержимое прокручивается колесом и само едет за клавиатурным
//! фокусом.
//!
//! # Почему рисование и разбор щелчка — один проход
//!
//! Раскладка, посчитанная отдельно для отрисовки и отдельно для попадания,
//! расходится в первый же день. Поэтому окно проходится ровно один раз
//! ([`kit::Pass`]), и проход этот либо рисует, либо только запоминает, куда
//! можно нажать и в каком порядке по пунктам ходит клавиатура.

mod kit;
mod pages;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;

use mini_ui::iconset::Fi;
use mini_ui::paint::{self, Ctx, Tone, Weight};
use mini_ui::theme;
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface};

use crate::input::KeyCode;
use crate::kprintln;

use kit::{Pass, Spot, line_h};

/// Страницы окна: тринадцать разделов бокового списка и их подразделы.
///
/// Открыт наружу ради трея и меню стола: «Параметры сети» обязаны открыть
/// окно сразу на сети.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Section {
    Home,
    System,
    Display,
    Power,
    Storage,
    About,
    Devices,
    Keyboard,
    Usb,
    Network,
    Personal,
    Wallpaper,
    Colors,
    Dock,
    Explorer,
    Apps,
    Packages,
    Services,
    Accounts,
    Time,
    Privacy,
    Update,
    Recovery,
}

/// Боковой список — порядок макета. «Сеть» в макете не было (там нет
/// сетевых настроек вовсе); в системе она есть, и стоит сразу за
/// устройствами, как «Сеть и Интернет» в Windows.
const NAV: [Section; 13] = [
    Section::Home,
    Section::System,
    Section::Devices,
    Section::Network,
    Section::Personal,
    Section::Dock,
    Section::Explorer,
    Section::Apps,
    Section::Accounts,
    Section::Time,
    Section::Privacy,
    Section::Update,
    Section::Recovery,
];

/// Все страницы — для поиска.
const ALL: [Section; 23] = [
    Section::Home,
    Section::System,
    Section::Display,
    Section::Power,
    Section::Storage,
    Section::About,
    Section::Devices,
    Section::Keyboard,
    Section::Usb,
    Section::Network,
    Section::Personal,
    Section::Wallpaper,
    Section::Colors,
    Section::Dock,
    Section::Explorer,
    Section::Apps,
    Section::Packages,
    Section::Services,
    Section::Accounts,
    Section::Time,
    Section::Privacy,
    Section::Update,
    Section::Recovery,
];

impl Section {
    /// Имя в боковом списке и в журнале.
    pub const fn title(self) -> &'static str {
        match self {
            Section::Home => "Главная",
            Section::System => "Система",
            Section::Display => "Дисплей",
            Section::Power => "Питание",
            Section::Storage => "Хранилище",
            Section::About => "О системе",
            Section::Devices => "Устройства",
            Section::Keyboard => "Клавиатура",
            Section::Usb => "USB-устройства",
            Section::Network => "Сеть",
            Section::Personal => "Персонализация",
            Section::Wallpaper => "Фон",
            Section::Colors => "Цвета",
            Section::Dock => "Dock и панель задач",
            Section::Explorer => "Проводник",
            Section::Apps => "Приложения",
            Section::Packages => "Пакеты",
            Section::Services => "Службы",
            Section::Accounts => "Учётные записи",
            Section::Time => "Время и язык",
            Section::Privacy => "Конфиденциальность",
            Section::Update => "Обновление системы",
            Section::Recovery => "Восстановление",
        }
    }

    /// Заголовок страницы — у одной он длиннее строки списка.
    const fn heading(self) -> &'static str {
        match self {
            Section::Privacy => "Конфиденциальность и безопасность",
            other => other.title(),
        }
    }

    /// Чей это подраздел.
    pub const fn parent(self) -> Option<Section> {
        match self {
            Section::Display | Section::Power | Section::Storage | Section::About => {
                Some(Section::System)
            }
            Section::Keyboard | Section::Usb => Some(Section::Devices),
            Section::Wallpaper | Section::Colors => Some(Section::Personal),
            Section::Packages | Section::Services => Some(Section::Apps),
            _ => None,
        }
    }

    /// Раздел бокового списка, к которому относится страница.
    const fn top(self) -> Section {
        match self.parent() {
            Some(parent) => parent,
            None => self,
        }
    }

    pub const fn icon(self) -> Fi {
        match self {
            Section::Home => Fi::Home,
            Section::System => Fi::Cpu,
            Section::Display => Fi::Monitor,
            Section::Power => Fi::Power,
            Section::Storage => Fi::Drive,
            Section::About => Fi::Info,
            Section::Devices | Section::Keyboard => Fi::Keyboard,
            Section::Usb => Fi::Usb,
            Section::Network => Fi::Globe,
            Section::Personal | Section::Colors => Fi::Palette,
            Section::Wallpaper => Fi::Image,
            Section::Dock => Fi::Dock,
            Section::Explorer => Fi::Folder,
            Section::Apps => Fi::Apps,
            Section::Packages => Fi::Package,
            Section::Services => Fi::Server,
            Section::Accounts => Fi::User,
            Section::Time => Fi::Clock,
            Section::Privacy => Fi::Shield,
            Section::Update => Fi::Refresh,
            Section::Recovery => Fi::History,
        }
    }

    /// Цвет значка в боковом списке: `oklch(0.78 0.11 h)` макета на тёмной
    /// теме и `oklch(0.55 0.13 h)` на светлой — светлый пастельный значок на
    /// белом не читается.
    fn tint(self) -> Color {
        let (dark, light) = match self.top() {
            Section::Home => ((0x80, 0xBD, 0xFB), (0x2A, 0x75, 0xBA)),
            Section::System => ((0x59, 0xC8, 0xE8), (0x00, 0x81, 0xA5)),
            Section::Devices => ((0x51, 0xCE, 0xC7), (0x00, 0x88, 0x82)),
            Section::Network => ((0x6C, 0xCE, 0xA6), (0x00, 0x88, 0x5E)),
            Section::Personal => ((0xD7, 0xA0, 0xE3), (0x92, 0x56, 0xA0)),
            Section::Dock => ((0xA9, 0xB0, 0xFD), (0x64, 0x66, 0xBB)),
            Section::Explorer => ((0xE5, 0xAB, 0x66), (0xA1, 0x61, 0x00)),
            Section::Apps => ((0x82, 0xCB, 0x92), (0x29, 0x86, 0x46)),
            Section::Accounts => ((0xF6, 0x9B, 0x94), (0xB1, 0x4E, 0x49)),
            Section::Time => ((0xC8, 0xB9, 0x62), (0x84, 0x72, 0x00)),
            Section::Privacy => ((0xF1, 0x99, 0xB4), (0xAC, 0x4C, 0x6E)),
            Section::Update => ((0x6B, 0xC3, 0xF4), (0x00, 0x7B, 0xB2)),
            _ => ((0xF2, 0xA0, 0x7C), (0xAE, 0x55, 0x28)),
        };
        let (r, g, b) = if theme::is_dark() { dark } else { light };
        Color::rgb(r, g, b)
    }

    /// Одной строкой — пояснение в строке-ссылке и в результатах поиска.
    pub const fn blurb(self) -> &'static str {
        match self {
            Section::Home => "Обзор машины и быстрые настройки",
            Section::System => "Дисплей, питание, хранилище, сведения",
            Section::Display => "Разрешение экрана и масштаб",
            Section::Power => "Гашение экрана, выключение, перезагрузка",
            Section::Storage => "Диски, тома, проверка файловых систем",
            Section::About => "Версия, архитектура, оборудование",
            Section::Devices => "Клавиатура и USB",
            Section::Keyboard => "Раскладка и горячие клавиши",
            Section::Usb => "Подключённые устройства и контроллеры",
            Section::Network => "Адрес: по DHCP или постоянный",
            Section::Personal => "Фон, цвета, тема",
            Section::Wallpaper => "Градиент рабочего стола",
            Section::Colors => "Тема, акцентный цвет, заголовок окна",
            Section::Dock => "Что показывает панель задач",
            Section::Explorer => "Файловый менеджер",
            Section::Apps => "Пакеты и службы",
            Section::Packages => "Установленные .fpk в /opt",
            Section::Services => "Что запускает /bin/init",
            Section::Accounts => "Кто может входить в систему",
            Section::Time => "Часовой пояс, раскладка, язык",
            Section::Privacy => "Что система делает с вашими данными",
            Section::Update => "Слоты A/B, серверы, проверка",
            Section::Recovery => "Проверка тома и меню загрузки",
        }
    }

    /// Слова, по которым страницу находит поиск, — то, что на ней написано.
    const fn keywords(self) -> &'static str {
        match self {
            Section::Home => "главная обзор",
            Section::System => "система уведомления",
            Section::Display => "дисплей экран разрешение масштаб режим монитор",
            Section::Power => "питание выключить перезагрузить гасить экран сон",
            Section::Storage => "хранилище диск том проверка fsck смонтировано место",
            Section::About => "о системе версия архитектура процессор память время работы сборка",
            Section::Devices => "устройства",
            Section::Keyboard => "клавиатура раскладка горячие клавиши alt shift",
            Section::Usb => "usb xhci ehci ohci контроллер мышь планшет",
            Section::Network => "сеть адрес dhcp шлюз ip mac dns",
            Section::Personal => "персонализация оформление",
            Section::Wallpaper => "фон обои градиент",
            Section::Colors => "цвета тема тёмная светлая акцент заголовок",
            Section::Dock => "панель задач dock аптайм дата часы",
            Section::Explorer => "проводник файлы папки files",
            Section::Apps => "приложения программы",
            Section::Packages => "пакеты fpk установить удалить pkg",
            Section::Services => "службы init services",
            Section::Accounts => "учётные записи пользователь uid root",
            Section::Time => "время дата часовой пояс язык",
            Section::Privacy => "конфиденциальность безопасность телеметрия права w^x",
            Section::Update => "обновление sysupdate слот сервер откат",
            Section::Recovery => "восстановление безопасный режим меню загрузки fsck",
        }
    }
}

/// Разрешения, которые можно попросить у прошивки.
pub const MODES: [(u32, u32); 5] = [(1024, 768), (1280, 720), (1280, 800), (1600, 900), (1920, 1080)];

/// Что панель задач показывает рядом с часами (фаза 69b).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanelPref {
    /// Время работы рядом с часами.
    Uptime,
    /// Дата рядом с часами.
    Date,
}

/// Чего окно просит у стола: сделать это само оно не может.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// Открыть вопрос «Завершить работу?».
    Shutdown,
    /// Открыть вопрос «Перезагрузить?».
    Restart,
    /// Перерисовать панель задач: сменилось то, что она показывает.
    Panel,
}

/// Что делает пункт, по которому нажали.
#[derive(Clone, PartialEq, Eq)]
enum Deed {
    /// Перейти на страницу (строка со стрелкой, результат поиска).
    Open(Section),
    Mode(u32, u32),
    Theme(bool),
    Accent(theme::Accent),
    Wallpaper(theme::Wallpaper),
    TitleBar(u32),
    ScreenOff(u32),
    Panel(PanelPref, bool),
    Layout(crate::input::keymap::Layout),
    Timezone(i32),
    NetDhcp,
    NetStatic,
    NetEdit(Field),
    NetApply,
    CheckDisks,
    CheckUpdates,
    ChooseFile,
    Install(String),
    AskRemove(String),
    Remove(String),
    BackToList,
    Shutdown,
    Restart,
    /// Запустить программу (проводник).
    Launch(&'static str),
}

/// Какое из двух полей раздела «Сеть» правится.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Address,
    Gateway,
}

/// Поля ввода раздела «Сеть».
///
/// Ввод ограничен цифрами, точкой и косой: эти клавиши на обеих раскладках
/// стоят на одном месте, и адрес из них набирается всегда.
#[derive(Clone)]
struct NetDraft {
    address: String,
    gateway: String,
    /// В каком поле каретка. `None` — ни в каком.
    editing: Option<Field>,
}

impl NetDraft {
    /// Начальное содержимое — то, что у машины сейчас.
    fn current() -> Self {
        let (address, gateway) = match crate::net::status() {
            Some(status) if !status.address.is_unspecified() => (
                format!("{}/{}", status.address, status.netmask.prefix().unwrap_or(24)),
                if status.gateway.is_unspecified() { String::new() } else { format!("{}", status.gateway) },
            ),
            _ => (String::new(), String::new()),
        };
        Self { address, gateway, editing: None }
    }

    fn text(&self, field: Field) -> &str {
        match field {
            Field::Address => &self.address,
            Field::Gateway => &self.gateway,
        }
    }

    fn text_mut(&mut self, field: Field) -> &mut String {
        match field {
            Field::Address => &mut self.address,
            Field::Gateway => &mut self.gateway,
        }
    }
}

/// Наибольшая длина того, что можно набрать в поле адреса.
const FIELD_LIMIT: usize = 36;

/// Наибольшая длина запроса поиска.
const SEARCH_LIMIT: usize = 32;

/// Чем занят раздел «Пакеты».
#[derive(Clone, PartialEq, Eq)]
enum Programs {
    List,
    /// Выбор файла — вместе с тем, что нашлось (искать при каждой
    /// перерисовке — обход трёх каталогов внутри разбора ввода).
    Choose(Vec<String>),
    Confirm(String),
}

/// Чем кончилось последнее действие.
struct Report {
    ok: bool,
    text: String,
}

impl Report {
    fn ok(text: String) -> Self {
        Self { ok: true, text }
    }

    fn bad(text: String) -> Self {
        Self { ok: false, text }
    }
}

/// То, что дорого спрашивать на каждой перерисовке: обход шины PCI, чтение
/// записи о слотах с ESP, реестр пакетов. Собирается при открытии страницы и
/// после действия, которое его меняет.
#[derive(Default)]
struct Cache {
    /// Перепись устройств (`devices::report`).
    devices: String,
    /// Запись о слотах.
    slots: Option<slots::State>,
    /// Установленные пакеты.
    packages: Option<Result<Vec<String>, String>>,
    /// Место на томах: точка, всего, свободно.
    space: Vec<(&'static str, u64, u64)>,
}

/// Высота и ширина окна, которые оно видело последними: клавиатуре нужна
/// раскладка, а клавиша приходит без области окна.
#[derive(Clone, Copy)]
struct Seen {
    area: Rect,
    ctx: Ctx,
}

pub struct SettingsView {
    page: Section,
    /// Выбранный пункт справа — номер в порядке прохода.
    action: usize,
    /// Фокус на левом списке разделов, а не на пунктах справа.
    on_sections: bool,
    /// На сколько точек содержимое прокручено вниз.
    scroll: i32,
    /// Что набрано в «Найти параметр».
    search: String,
    report: Option<Report>,
    screen: (u32, u32),
    programs: Programs,
    net: NetDraft,
    theme_changed: bool,
    title_changed: bool,
    mode_request: Option<(u32, u32)>,
    remembered: Option<Result<(), String>>,
    request: Option<Request>,
    /// Телефон: открыта страница, а не список разделов.
    page_open: bool,
    cache: Cache,
    seen: Cell<Option<Seen>>,
}

/// Что вернул проход.
struct Layout {
    hits: Vec<kit::Hit>,
    order: Vec<(Deed, Rect)>,
    top: i32,
    limit: i32,
    extent: i32,
    /// Прокрутка, с которой сделан проход.
    scroll: i32,
}

impl SettingsView {
    #[must_use]
    pub fn new(screen: (u32, u32)) -> Self {
        let mut view = Self {
            page: Section::Home,
            action: 0,
            on_sections: true,
            scroll: 0,
            search: String::new(),
            report: None,
            screen,
            programs: Programs::List,
            net: NetDraft::current(),
            theme_changed: false,
            title_changed: false,
            mode_request: None,
            remembered: None,
            request: None,
            page_open: false,
            cache: Cache::default(),
            seen: Cell::new(None),
        };
        view.refresh();
        view
    }

    /// Перейти на страницу — так окно открывают меню стола и трей.
    pub fn show(&mut self, section: Section) {
        self.page_open = true;
        self.on_sections = true;
        self.search.clear();
        self.go(section);
    }

    pub fn take_theme_change(&mut self) -> bool {
        core::mem::take(&mut self.theme_changed)
    }

    pub fn take_title_change(&mut self) -> bool {
        core::mem::take(&mut self.title_changed)
    }

    pub fn take_mode_request(&mut self) -> Option<(u32, u32)> {
        self.mode_request.take()
    }

    /// Чего окно просит у стола с прошлого вопроса.
    pub fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// Стол сказал, переключил ли он экран.
    pub fn mode_applied(&mut self, requested: (u32, u32), outcome: Result<(), String>) {
        let (width, height) = requested;
        let remembered = self.remembered.take().unwrap_or(Ok(()));
        if outcome.is_ok() {
            self.screen = requested;
        }
        self.report = Some(match (outcome, remembered) {
            (Ok(()), Ok(())) => Report::ok(format!("{width}×{height} включено и запомнено")),
            (Ok(()), Err(err)) => {
                Report::bad(format!("{width}×{height} включено до перезагрузки: не записано ({err})"))
            }
            (Err(why), Ok(())) => Report::ok(format!(
                "{width}×{height} — со следующего запуска: на ходу не переключить ({why})"
            )),
            (Err(why), Err(err)) => Report::bad(format!("режим не сменён ({why}) и не сохранён ({err})")),
        });
    }

    /// Открыть страницу: прокрутка в начало, разговоры «Пакетов» сброшены.
    fn go(&mut self, section: Section) {
        self.page = section;
        self.action = 0;
        self.scroll = 0;
        self.report = None;
        self.programs = Programs::List;
        self.net.editing = None;
        self.refresh();
    }

    /// Пересобрать то, что дорого спрашивать, — только для этой страницы.
    fn refresh(&mut self) {
        match self.page {
            Section::Home | Section::Packages | Section::Apps => {
                self.cache.packages = Some(pages::packages());
                if self.page == Section::Home {
                    self.cache.space = crate::fs::space();
                }
            }
            Section::Storage => {
                self.cache.devices = crate::devices::report();
                self.cache.space = crate::fs::space();
            }
            Section::Usb | Section::About => self.cache.devices = crate::devices::report(),
            Section::Update | Section::Recovery => self.cache.slots = crate::slot::state(),
            _ => {}
        }
    }

    /// Нажатие ведёт по разделам бокового списка.
    fn nav_index(&self) -> usize {
        let top = self.page.top();
        NAV.iter().position(|other| *other == top).unwrap_or(0)
    }

    /// Пройти окно без рисования — где что лежит.
    fn layout(&self) -> Layout {
        let seen = self.seen.get().unwrap_or(Seen {
            area: Rect::new(0, 0, self.screen.0 * 7 / 8, self.screen.1 * 8 / 10),
            ctx: Ctx::scaled(1),
        });
        self.run(seen.ctx, seen.area, None)
    }

    /// Пункты текущей страницы в порядке прохода.
    fn deeds(&self) -> Vec<Deed> {
        self.layout().order.into_iter().map(|(deed, _)| deed).collect()
    }

    /// Прокрутить так, чтобы пункт с фокусом был виден целиком.
    fn follow_focus(&mut self) {
        let layout = self.layout();
        if self.on_sections {
            return;
        }
        let Some((_, rect)) = layout.order.get(self.action) else {
            return;
        };
        let margin = 12;
        if rect.y < layout.top {
            self.scroll -= layout.top - rect.y + margin;
        } else if rect.bottom() > layout.limit {
            self.scroll += rect.bottom() - layout.limit + margin;
        }
        self.clamp_scroll(&layout);
    }

    fn clamp_scroll(&mut self, layout: &Layout) {
        // `extent` посчитан при прежней прокрутке: полная высота — это он плюс
        // то, на сколько уже прокручено.
        let full = layout.extent + layout.scroll;
        let room = layout.limit - layout.top;
        let content = full - layout.top;
        let most = (content - room + 16).max(0);
        self.scroll = self.scroll.clamp(0, most);
    }

    /// Колесо мыши над окном. `true` — содержимое сдвинулось.
    pub fn wheel(&mut self, clicks: i32) -> bool {
        let before = self.scroll;
        let step = self.seen.get().map_or(1, |seen| seen.ctx.scale) as i32 * 64;
        let layout = self.layout();
        // Положительное — от себя: содержимое едет вниз, то есть к началу.
        self.scroll -= clicks * step;
        self.clamp_scroll(&layout);
        self.scroll != before
    }

    /// Разобрать клавишу. `ch` — символ, который она даёт в текущей раскладке.
    /// `true` — окно её использовало.
    pub fn handle(&mut self, code: KeyCode, ch: Option<char>) -> bool {
        if let Some(field) = self.net.editing {
            if self.type_into(field, code) {
                return true;
            }
        }
        let used = self.key(code, ch);
        if used {
            self.follow_focus();
        }
        used
    }

    fn key(&mut self, code: KeyCode, ch: Option<char>) -> bool {
        // Набранная буква — в поиск, как в «Параметрах» Windows: начать
        // печатать можно сразу, не целясь в поле.
        if let Some(ch) = ch {
            let printable = !ch.is_control() && (ch.is_alphanumeric() || ch == ' ' || ch == '-' || ch == '+');
            let keys = matches!(code, KeyCode::Enter | KeyCode::Tab | KeyCode::Backspace | KeyCode::Escape);
            if printable && !keys && !(ch == ' ' && self.search.is_empty()) {
                if self.search.chars().count() < SEARCH_LIMIT {
                    self.search.push(ch);
                }
                self.searched();
                return true;
            }
        }
        if code == KeyCode::Escape && theme::is_mobile() && self.page_open {
            self.page_open = false;
            self.programs = Programs::List;
            return true;
        }
        match code {
            KeyCode::Backspace | KeyCode::Escape if !self.search.is_empty() => {
                if code == KeyCode::Escape {
                    self.search.clear();
                } else {
                    self.search.pop();
                }
                self.searched();
                true
            }
            KeyCode::Backspace | KeyCode::Escape => self.back(),
            KeyCode::Up => {
                if self.on_sections {
                    let index = self.nav_index().saturating_sub(1);
                    self.go(NAV[index]);
                } else {
                    self.action = self.action.saturating_sub(1);
                    self.report = None;
                }
                true
            }
            KeyCode::Down => {
                if self.on_sections {
                    let index = (self.nav_index() + 1).min(NAV.len() - 1);
                    self.search.clear();
                    self.go(NAV[index]);
                } else {
                    self.action = (self.action + 1).min(self.deeds().len().saturating_sub(1));
                    self.report = None;
                }
                true
            }
            KeyCode::Right | KeyCode::Tab => {
                let count = self.deeds().len();
                if count > 0 {
                    self.on_sections = false;
                    self.action = self.action.min(count - 1);
                }
                true
            }
            KeyCode::Left => {
                self.on_sections = true;
                true
            }
            KeyCode::Enter => {
                if self.on_sections {
                    if !self.deeds().is_empty() {
                        self.on_sections = false;
                        self.action = 0;
                    }
                } else {
                    self.activate();
                }
                true
            }
            _ => false,
        }
    }

    /// Поиск изменился: страница результатов с начала.
    fn searched(&mut self) {
        self.action = 0;
        self.scroll = 0;
        self.on_sections = true;
        self.report = None;
        if !self.search.is_empty() {
            kprintln!("  settings    : search '{}'", self.search);
        }
    }

    /// Назад к родителю. Фокус встаёт на строку, которая этот подраздел
    /// открыла: человек возвращается туда, откуда ушёл.
    fn back(&mut self) -> bool {
        let Some(parent) = self.page.parent() else {
            return false;
        };
        let child = self.page;
        self.go(parent);
        self.on_sections = false;
        if let Some(index) = self.deeds().iter().position(|deed| *deed == Deed::Open(child)) {
            self.action = index;
        }
        kprintln!("  settings    : back to '{}'", parent.title());
        true
    }

    /// Набрать знак в поле адреса. `true` — клавиша использована полем.
    fn type_into(&mut self, field: Field, code: KeyCode) -> bool {
        let digit = match code {
            KeyCode::Digit0 => Some('0'),
            KeyCode::Digit1 => Some('1'),
            KeyCode::Digit2 => Some('2'),
            KeyCode::Digit3 => Some('3'),
            KeyCode::Digit4 => Some('4'),
            KeyCode::Digit5 => Some('5'),
            KeyCode::Digit6 => Some('6'),
            KeyCode::Digit7 => Some('7'),
            KeyCode::Digit8 => Some('8'),
            KeyCode::Digit9 => Some('9'),
            KeyCode::Period => Some('.'),
            KeyCode::Slash => Some('/'),
            _ => None,
        };
        if let Some(ch) = digit {
            let text = self.net.text_mut(field);
            if text.len() < FIELD_LIMIT {
                text.push(ch);
            }
            self.report = None;
            return true;
        }
        match code {
            KeyCode::Backspace => {
                self.net.text_mut(field).pop();
                self.report = None;
                true
            }
            // Enter из поля — «поле готово», а не «применить»: адрес меняется
            // под тем, кто сидит по сети, и лишний Enter не должен обрывать ему
            // соединение.
            KeyCode::Enter | KeyCode::Escape => {
                self.net.editing = None;
                true
            }
            // Буквы полю не нужны и в поиск из поля не уходят.
            _ => crate::input::keymap::latin(code).is_some(),
        }
    }

    /// Щелчок по окну: координаты внутри поверхности окна.
    pub fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.seen.set(Some(Seen { area, ctx }));
        let layout = self.run(ctx, area, None);
        let Some(hit) = layout.hits.into_iter().find(|hit| hit.rect.contains(x, y)) else {
            return false;
        };
        match hit.spot {
            Spot::Back => {
                if theme::is_mobile() {
                    self.page_open = false;
                    self.programs = Programs::List;
                    self.report = None;
                    kprintln!("  settings    : back to the list");
                } else {
                    self.back();
                }
                true
            }
            Spot::Nav(section) => {
                if theme::is_mobile() {
                    self.page_open = true;
                }
                self.search.clear();
                self.on_sections = true;
                self.go(section);
                kprintln!("  settings    : opened '{}'", section.title());
                true
            }
            Spot::Search => {
                self.on_sections = true;
                true
            }
            Spot::Deed(deed) => {
                self.on_sections = false;
                if let Some(index) = self.deeds().iter().position(|other| *other == deed) {
                    self.action = index;
                }
                self.perform(deed);
                true
            }
        }
    }

    /// Применить набранный адрес и записать его — в таком порядке: адрес с
    /// опечаткой отказывает сразу, а не после перезагрузки.
    fn apply_network(&mut self) {
        use crate::net::ipv4::Ipv4;
        use crate::net::persist::{Mode, Settings};

        let address = self.net.address.trim().to_string();
        let Some((host, bits)) = address.split_once('/') else {
            self.report = Some(Report::bad(String::from("адрес пишется с длиной префикса: 10.0.2.15/24")));
            return;
        };
        let (Some(host), Ok(bits)) = (Ipv4::parse(host), bits.parse::<u32>()) else {
            self.report = Some(Report::bad(format!("это не адрес: {address}")));
            return;
        };
        let Some(netmask) = Ipv4::from_prefix(bits) else {
            self.report = Some(Report::bad(format!("префикс — от 0 до 32 бит, а не {bits}")));
            return;
        };
        let gateway_text = self.net.gateway.trim().to_string();
        let gateway = if gateway_text.is_empty() {
            Ipv4::UNSPECIFIED
        } else {
            match Ipv4::parse(&gateway_text) {
                Some(gateway) => gateway,
                None => {
                    self.report = Some(Report::bad(format!("это не шлюз: {gateway_text}")));
                    return;
                }
            }
        };
        if let Err(err) = crate::net::configure(host, netmask, gateway) {
            self.report = Some(Report::bad(format!("адрес не принят: {err}")));
            return;
        }
        let dns = crate::net::dns_server().unwrap_or(Ipv4::UNSPECIFIED);
        let settings = Settings { mode: Mode::Static, address: host, netmask, gateway, dns };
        self.net.editing = None;
        self.report = Some(match crate::net::persist::store(&settings) {
            Ok(()) => {
                kprintln!("  settings    : address {host}/{bits} static, saved");
                Report::ok(format!("адрес {host}/{bits} применён и запомнен"))
            }
            Err(err) => Report::bad(format!("{host}/{bits} применён, но не записан: {err}")),
        });
    }

    fn activate(&mut self) {
        let Some(deed) = self.deeds().into_iter().nth(self.action) else {
            return;
        };
        self.perform(deed);
    }

    fn perform(&mut self, deed: Deed) {
        match deed {
            Deed::Open(section) => {
                kprintln!("  settings    : opened '{}'", section.title());
                self.search.clear();
                self.go(section);
                self.on_sections = false;
                if theme::is_mobile() {
                    self.page_open = true;
                }
            }
            Deed::Mode(width, height) => {
                self.remembered = Some(crate::slot::request_screen_mode(width, height));
                self.mode_request = Some((width, height));
            }
            Deed::Theme(dark) => {
                if theme::set_dark(dark) {
                    self.theme_changed = true;
                }
                self.report = Some(match super::prefs::store_theme(dark) {
                    Ok(()) => {
                        kprintln!("  settings    : theme {} saved", if dark { "dark" } else { "light" });
                        Report::ok(String::from("тема запомнена"))
                    }
                    Err(err) => Report::bad(format!("тема применена, но не запомнена: {err}")),
                });
            }
            Deed::Accent(accent) => {
                if theme::set_accent(accent) {
                    self.theme_changed = true;
                }
                self.report = Some(match super::prefs::store_accent(accent) {
                    Ok(()) => {
                        kprintln!("  settings    : accent {} saved", accent.tag());
                        Report::ok(format!("акцент «{}» запомнен", accent.title()))
                    }
                    Err(err) => Report::bad(format!("акцент применён, но не запомнен: {err}")),
                });
            }
            Deed::Wallpaper(wall) => {
                if theme::set_wallpaper(wall) {
                    self.theme_changed = true;
                }
                self.report = Some(match super::prefs::store_wallpaper(wall) {
                    Ok(()) => {
                        kprintln!("  settings    : wallpaper {} saved", wall.tag());
                        Report::ok(format!("обои «{}» запомнены", wall.title()))
                    }
                    Err(err) => Report::bad(format!("обои применены, но не запомнены: {err}")),
                });
            }
            Deed::TitleBar(height) => {
                if theme::set_title_h(height) {
                    self.title_changed = true;
                }
                self.report = Some(match super::prefs::store_title_bar(height) {
                    Ok(()) => {
                        kprintln!("  settings    : title bar {height} saved");
                        Report::ok(format!("высота заголовка {height} запомнена"))
                    }
                    Err(err) => Report::bad(format!("высота применена, но не запомнена: {err}")),
                });
            }
            Deed::ScreenOff(seconds) => {
                crate::power::set_screen_off_after(seconds);
                self.report = Some(match super::prefs::store_screen_off(seconds) {
                    Ok(()) => {
                        if seconds == 0 {
                            kprintln!("  settings    : screen off never saved");
                            Report::ok(String::from("экран не будет гаснуть сам"))
                        } else {
                            kprintln!("  settings    : screen off after {seconds} s saved");
                            Report::ok(format!("экран будет гаснуть через {}", pages::screen_off_text(seconds)))
                        }
                    }
                    Err(err) => Report::bad(format!("срок применён, но не запомнен: {err}")),
                });
            }
            Deed::Panel(pref, on) => {
                super::panel::set_shows(pref, on);
                self.request = Some(Request::Panel);
                let tag = match pref {
                    PanelPref::Uptime => "uptime",
                    PanelPref::Date => "date",
                };
                self.report = Some(match super::prefs::store_panel(pref, on) {
                    Ok(()) => {
                        kprintln!("  settings    : panel {tag} {} saved", if on { "on" } else { "off" });
                        Report::ok(String::from("панель запомнила"))
                    }
                    Err(err) => Report::bad(format!("применено, но не запомнено: {err}")),
                });
            }
            Deed::Layout(layout) => {
                crate::input::keymap::set_layout(layout);
                self.request = Some(Request::Panel);
                self.report = Some(match crate::input::keymap::store_layout(layout) {
                    Ok(()) => {
                        kprintln!("  settings    : keyboard {} saved", layout.tag());
                        Report::ok(format!("раскладка «{}» запомнена", layout.name()))
                    }
                    Err(err) => Report::bad(format!("раскладка включена, но не запомнена: {err}")),
                });
            }
            Deed::Timezone(minutes) => {
                let text = crate::time::zone_text(minutes);
                self.report = Some(match crate::time::set_timezone(minutes) {
                    Ok(()) => Report::ok(format!("часовой пояс {text} применён и запомнен")),
                    Err(err) => Report::bad(format!("{text} не записан: {err}")),
                });
                self.request = Some(Request::Panel);
            }
            Deed::NetDhcp => {
                self.report = Some(match crate::net::persist::forget() {
                    Ok(true) => {
                        kprintln!("  settings    : address back to DHCP, /etc/network.cfg removed");
                        Report::ok(String::from("адрес будет получаться по DHCP со следующего запуска"))
                    }
                    Ok(false) => Report::ok(String::from("адрес и так получается по DHCP")),
                    Err(err) => Report::bad(format!("не записано: {err}")),
                });
            }
            Deed::NetStatic => {
                self.net = NetDraft::current();
                self.net.editing = Some(Field::Address);
                self.report = None;
            }
            Deed::NetEdit(field) => {
                self.net.editing = Some(field);
                self.report = None;
            }
            Deed::NetApply => self.apply_network(),
            Deed::CheckDisks => {
                // Только проверка, без починки: чинить том под работающим
                // редактором нельзя — см. `fsck` в `shell.rs`.
                let mut problems = 0usize;
                let mut volumes = 0usize;
                for (point, result) in crate::fs::check_all() {
                    match result {
                        Some(Ok(summary)) => {
                            volumes += 1;
                            problems += summary.problems.len() + summary.dropped;
                            kprintln!(
                                "  settings    : fsck {point}: {} problem(s)",
                                summary.problems.len() + summary.dropped
                            );
                        }
                        Some(Err(err)) => {
                            volumes += 1;
                            problems += 1;
                            kprintln!("  settings    : fsck {point}: {err}");
                        }
                        None => {}
                    }
                }
                self.report = Some(if problems == 0 {
                    Report::ok(format!("проверено томов: {volumes}, замечаний нет"))
                } else {
                    Report::bad(format!("замечаний: {problems}. Починить можно только при загрузке"))
                });
            }
            Deed::CheckUpdates => self.report = Some(pages::run("/bin/sysupdate", "sysupdate")),
            Deed::ChooseFile => {
                let found = pages::available();
                let empty = found.is_empty();
                if self.page != Section::Packages {
                    self.go(Section::Packages);
                }
                self.programs = Programs::Choose(found);
                self.action = 0;
                self.report =
                    empty.then(|| Report::bad("файлов .fpk не нашлось ни в /media, ни дома".to_string()));
            }
            Deed::Install(path) => {
                // Пакет с носителя — по имени: так `pkg` сам ставит и его
                // зависимости (имя файла до последнего `-`, как его ищет `pkg`).
                let by_name = path
                    .strip_prefix("/media/")
                    .and_then(|file| file.strip_suffix(".fpk"))
                    .and_then(|stem| stem.rsplit_once('-').map(|(name, _)| name))
                    .filter(|name| !name.is_empty() && !name.contains('/'));
                let line = match by_name {
                    Some(name) => format!("/bin/pkg install {name}"),
                    None => format!("/bin/pkg install {path}"),
                };
                self.report = Some(pages::run(&line, "pkg install"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::AskRemove(name) => {
                self.programs = Programs::Confirm(name);
                self.action = 0;
                self.report = None;
            }
            Deed::Remove(name) => {
                let line = format!("/bin/pkg remove {name}");
                self.report = Some(pages::run(&line, "pkg remove"));
                self.programs = Programs::List;
                self.action = 0;
            }
            Deed::BackToList => {
                self.programs = Programs::List;
                self.action = 0;
                self.report = None;
                self.cache.packages = Some(pages::packages());
            }
            Deed::Shutdown => self.request = Some(Request::Shutdown),
            Deed::Restart => self.request = Some(Request::Restart),
            Deed::Launch(path) => self.report = Some(pages::run(path, path)),
        }
        self.scroll = self.scroll.max(0);
    }

    /// Нарисовать окно целиком.
    pub fn draw(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        self.seen.set(Some(Seen { area, ctx }));
        self.run(ctx, area, Some(surface));
    }

    // ── Один проход ──────────────────────────────────────────────────────────

    fn run(&self, ctx: Ctx, area: Rect, surface: Option<&mut Surface>) -> Layout {
        let mut pass = Pass {
            ctx,
            surface,
            hits: Vec::new(),
            focus: (!self.on_sections).then_some(self.action),
            order: Vec::new(),
            top: area.y,
            limit: area.bottom(),
            extent: area.y,
        };
        let mut top = area.y;
        if area.w != 0 && area.h != 0 {
            let background = ctx.under;
            pass.on(|s| s.fill(area, background));
            if theme::is_mobile() {
                if self.page_open {
                    let header_h = ctx.px(56);
                    self.mobile_header(&mut pass, Rect::new(area.x, area.y, area.w, header_h));
                    let body = Rect::new(area.x, area.y + header_h as i32, area.w, area.h.saturating_sub(header_h));
                    top = self.content(&mut pass, body);
                } else {
                    self.mobile_list(&mut pass, area);
                }
            } else {
                let side_w = ctx.px(270).min(area.w / 2);
                self.sidebar(&mut pass, Rect::new(area.x, area.y, side_w, area.h));
                let body = Rect::new(area.x + side_w as i32, area.y, area.w.saturating_sub(side_w), area.h);
                top = self.content(&mut pass, body);
            }
        }
        Layout { hits: pass.hits, order: pass.order, top, limit: pass.limit, extent: pass.extent, scroll: self.scroll }
    }

    /// Телефон: заголовок открытой страницы — стрелка назад и имя.
    fn mobile_header(&self, pass: &mut Pass, area: Rect) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let pad = ctx.px(16) as i32;
        let size = ctx.px(36);
        let back = Rect::new(area.x + pad, area.y + (area.h as i32 - size as i32) / 2, size, size);
        let radius = ctx.px(12);
        pass.on(|s| {
            mini_ui::draw::rounded(s, back, radius, p.btn.color, p.btn.alpha);
            mini_ui::draw::rounded_stroke(s, back, radius, p.btnline.color, p.btnline.alpha);
        });
        let glyph = ctx.px(18);
        pass.icon(Fi::ChevronLeft, back.x + (size - glyph) as i32 / 2, back.y + (size - glyph) as i32 / 2, glyph, p.ink2);
        pass.hit(Rect::new(area.x, area.y, ctx.px(160).min(area.w), area.h), Spot::Back);
        let text_x = back.right() + ctx.px(12) as i32;
        let room = (area.right() - pad - text_x).max(0) as u32;
        let y = paint::baseline(ctx, Role::Title, area);
        pass.text(Role::Title, text_x, y, room, self.page.title(), p.ink);
        let edge = p.line;
        pass.on(|s| mini_ui::draw::hline(s, area.x, area.bottom() - 1, area.w, edge.color, edge.alpha));
    }

    /// Телефон: список разделов — карточка сеанса и две группы.
    fn mobile_list(&self, pass: &mut Pass, area: Rect) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let pad = ctx.px(16) as i32;
        let x = area.x + pad;
        let w = area.w.saturating_sub(ctx.px(32));
        let mut y = area.y + ctx.px(18) as i32;

        pass.text(Role::Heading, x + ctx.px(4) as i32, y, w, "Настройки", p.ink);
        y += line_h(ctx, Role::Heading) + ctx.px(16) as i32;

        let card = Rect::new(x, y, w, ctx.px(64));
        let radius = ctx.px(18);
        pass.on(|s| {
            mini_ui::draw::horizontal_gradient(s, card, radius, ctx.flat(p.sel1), ctx.flat(p.sel2), 255);
            mini_ui::draw::rounded_stroke(s, card, radius, p.accedge, 255);
        });
        let avatar = Rect::new(card.x + ctx.px(14) as i32, card.y + ctx.px(14) as i32, ctx.px(36), ctx.px(36));
        let name = pages::session_name();
        let uid = crate::user::session::credentials().uid;
        pages::avatar(pass, avatar, &name);
        let text_x = avatar.right() + ctx.px(12) as i32;
        let room = (card.right() - ctx.px(36) as i32 - text_x).max(0) as u32;
        let lines = line_h(ctx, Role::Title) + line_h(ctx, Role::MonoSmall);
        let top = card.y + (card.h as i32 - lines) / 2;
        pass.text(Role::Title, text_x, top, room, &name, p.ink);
        let sub = format!("uid {uid} \u{00b7} {} {}", crate::arch::ARCH_NAME, crate::version_text());
        pass.text(Role::MonoSmall, text_x, top + line_h(ctx, Role::Title), room, &sub, p.ink4);
        pass.hit(card, Spot::Nav(Section::Accounts));
        y = card.bottom() + ctx.px(18) as i32;

        // Геометрия та же, что до 69b: на неё стоит прицел сценария `phone`.
        let groups: [(&str, &[Section]); 2] = [
            ("УСТРОЙСТВО", &[Section::Display, Section::Time, Section::Network]),
            (
                "СИСТЕМА",
                &[Section::Storage, Section::Packages, Section::Update, Section::Colors, Section::Power, Section::About],
            ),
        ];
        let row_h = ctx.px(46);
        for (caption, sections) in groups {
            pass.on(|s| {
                paint::caps(ctx, s, x + ctx.px(4) as i32, y, caption);
            });
            y += line_h(ctx, Role::MonoCaps) + ctx.px(10) as i32;
            for section in sections.iter().copied() {
                let rect = Rect::new(x, y, w, row_h);
                if !pass.visible(rect) {
                    break;
                }
                let r = ctx.px(16);
                pass.on(|s| {
                    mini_ui::draw::rounded(s, rect, r, p.card.color, p.card.alpha);
                    mini_ui::draw::rounded_stroke(s, rect, r, p.line2.color, p.line2.alpha);
                });
                let glyph = ctx.px(20);
                let tint = section.tint();
                pass.icon(section.icon(), rect.x + ctx.px(16) as i32, rect.y + (row_h - glyph) as i32 / 2, glyph, tint);
                let text_x = rect.x + ctx.px(54) as i32;
                let room = (rect.right() - ctx.px(36) as i32 - text_x).max(0) as u32;
                let lines = line_h(ctx, Role::Title) + line_h(ctx, Role::Caption);
                let top = rect.y + (rect.h as i32 - lines) / 2;
                pass.text(Role::Title, text_x, top, room, section.title(), p.ink);
                pass.text(Role::Caption, text_x, top + line_h(ctx, Role::Title), room, section.blurb(), p.ink4);
                let chev = ctx.px(16);
                pass.icon(Fi::ChevronRight, rect.right() - ctx.px(30) as i32, rect.y + (row_h - chev) as i32 / 2, chev, p.ink4);
                pass.hit(rect, Spot::Nav(section));
                y += (row_h + ctx.px(4)) as i32;
            }
            y += ctx.px(10) as i32;
        }
    }

    /// Боковая колонка: карточка сеанса, поиск, разделы.
    fn sidebar(&self, pass: &mut Pass, area: Rect) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let fill = ctx.flat(p.panel);
        pass.on(|s| s.fill(area, fill));
        let edge = p.line;
        let edge_x = area.right() - 1;
        pass.on(|s| mini_ui::draw::vline(s, edge_x, area.y, area.h, edge.color, edge.alpha));
        let inner = ctx.on(fill);

        let pad = ctx.px(12) as i32;
        let x = area.x + pad;
        let w = area.w.saturating_sub(ctx.px(24));

        // ── Сеанс ────────────────────────────────────────────────────────────
        let avatar_d = ctx.px(48);
        let avatar = Rect::new(x + ctx.px(6) as i32, area.y + ctx.px(20) as i32, avatar_d, avatar_d);
        let name = pages::session_name();
        pages::avatar(pass, avatar, &name);
        let text_x = avatar.right() + ctx.px(14) as i32;
        let room = (area.right() - pad - text_x).max(0) as u32;
        let lines = line_h(ctx, Role::Strong) + ctx.px(2) as i32 + line_h(ctx, Role::Caption);
        let ty = avatar.y + (avatar_d as i32 - lines) / 2;
        pass.text(Role::Strong, text_x, ty, room, &name, p.ink);
        let sub = if crate::user::session::credentials().uid == 0 {
            "Администратор системы"
        } else {
            "Локальная учётная запись"
        };
        pass.text(Role::Caption, text_x, ty + line_h(ctx, Role::Strong) + ctx.px(2) as i32, room, sub, p.ink4);
        pass.hit(Rect::new(x, avatar.y, w, avatar_d), Spot::Nav(Section::Accounts));

        // ── Найти параметр ───────────────────────────────────────────────────
        let field = Rect::new(x, avatar.bottom() + ctx.px(18) as i32, w, ctx.px(36));
        let radius = ctx.px(6);
        let searching = !self.search.is_empty();
        pass.on(|s| {
            mini_ui::draw::rounded(s, field, radius, inner.flat(p.sunk), 255);
            mini_ui::draw::rounded_stroke(s, field, radius, p.line2.color, p.line2.alpha);
            let ink = if searching || self.on_sections { p.acc } else { p.ink5 };
            mini_ui::draw::hline(s, field.x + 2, field.bottom() - 2, field.w.saturating_sub(4), ink, 255);
            mini_ui::draw::hline(s, field.x + 2, field.bottom() - 1, field.w.saturating_sub(4), ink, 255);
        });
        let glyph = ctx.px(16);
        pass.icon(Fi::Search, field.x + ctx.px(12) as i32, field.y + (field.h - glyph) as i32 / 2, glyph, p.ink4);
        let tx = field.x + ctx.px(38) as i32;
        let room = (field.right() - tx - ctx.px(8) as i32).max(0) as u32;
        if searching {
            pass.text(Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, &self.search, p.ink);
            let width = ctx.face(Role::Body).width(&self.search).min(room);
            let caret = Rect::new(tx + width as i32 + 1, field.y + ctx.px(9) as i32, ctx.px(2), field.h.saturating_sub(ctx.px(18)));
            pass.on(|s| s.fill(caret, p.acc));
        } else {
            pass.text(Role::Body, tx, paint::baseline(ctx, Role::Body, field), room, "Найти параметр", p.ink5);
        }
        pass.hit(field, Spot::Search);

        // ── Разделы ──────────────────────────────────────────────────────────
        let bottom = area.bottom() - ctx.px(8) as i32;
        let mut y = field.bottom() + ctx.px(14) as i32;
        // Тринадцать строк обязаны поместиться все: прокрутки у колонки нет, и
        // раздел, уехавший под нижний край, не открыть ни мышью, ни глазами.
        // На экране в 720 точек строка сжимается с 38 до того, что влезает.
        let room = (bottom - y).max(0) as u32;
        let row_h = (room / NAV.len() as u32).saturating_sub(ctx.px(2)).clamp(ctx.px(26), ctx.px(38));
        let current = (!searching).then(|| self.page.top());
        for section in NAV {
            let rect = Rect::new(x, y, w, row_h);
            y += (row_h + ctx.px(2)) as i32;
            if rect.bottom() > bottom {
                continue;
            }
            let chosen = current == Some(section);
            if chosen {
                let r = kit::nav_radius(ctx);
                pass.on(|s| {
                    mini_ui::draw::rounded(s, rect, r, inner.flat(p.btn), 255);
                    if self.on_sections {
                        mini_ui::draw::rounded_stroke(s, rect, r, p.accedge, 255);
                    }
                    let mark = Rect::new(rect.x, rect.y + ctx.px(11) as i32, ctx.px(3), rect.h.saturating_sub(ctx.px(22)));
                    mini_ui::draw::rounded(s, mark, ctx.px(2), p.acc, 255);
                });
            }
            let glyph = ctx.px(20);
            let tint = section.tint();
            pass.icon(section.icon(), rect.x + ctx.px(14) as i32, rect.y + (row_h - glyph) as i32 / 2, glyph, tint);
            let text_x = rect.x + ctx.px(46) as i32;
            let room = (rect.right() - ctx.px(8) as i32 - text_x).max(0) as u32;
            let ink = if chosen { p.ink } else { p.ink2 };
            pass.text(Role::Body, text_x, paint::baseline(ctx, Role::Body, rect), room, section.title(), ink);
            pass.hit(rect, Spot::Nav(section));
        }
    }

    /// Правая часть: заголовок, страница, низ. Возвращает верх прокрутки.
    fn content(&self, pass: &mut Pass, area: Rect) -> i32 {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let pad = ctx.px(32);
        if area.w <= pad * 2 || area.h <= pad * 2 {
            return area.y;
        }
        let main = Rect::new(area.x + pad as i32, area.y, area.w - pad * 2, area.h);

        // ── Заголовок (не прокручивается) ────────────────────────────────────
        let mut y = main.y + ctx.px(22) as i32;
        if !theme::is_mobile() {
            let heading = self.heading();
            let mut x = main.x;
            if let (Some(parent), true) = (self.page.parent(), self.search.is_empty()) {
                // Хлебная крошка: «Система › Дисплей». Щелчок по родителю — назад.
                let pw = ctx.face(Role::Heading).width(parent.heading());
                pass.text(Role::Heading, x, y, pw, parent.heading(), p.ink4);
                pass.hit(Rect::new(x, y, pw, line_h(ctx, Role::Heading) as u32), Spot::Back);
                x += pw as i32 + ctx.px(8) as i32;
                let glyph = ctx.px(20);
                pass.icon(Fi::ChevronRight, x, y + (line_h(ctx, Role::Heading) - glyph as i32) / 2, glyph, p.ink4);
                x += (glyph + ctx.px(8)) as i32;
            }
            let room = (main.right() - x).max(0) as u32;
            pass.text(Role::Heading, x, y, room, &heading, p.ink);
            y += line_h(ctx, Role::Heading) + ctx.px(14) as i32;
        } else {
            y = main.y + ctx.px(12) as i32;
        }

        // ── Низ: ответ последнего действия и кнопки разговора ────────────────
        let buttons = self.buttons();
        let bottom_h = if buttons.is_empty() && self.report.is_none() { ctx.px(8) } else { ctx.px(58) };
        let top = y;
        pass.top = top;
        pass.limit = main.bottom() - bottom_h as i32;

        // ── Страница ─────────────────────────────────────────────────────────
        let w = main.w.min(ctx.px(900));
        let start = top - self.scroll;
        if self.search.is_empty() {
            self.page_body(pass, main.x, start, w);
        } else {
            self.search_results(pass, main.x, start, w);
        }
        let extent = pass.extent;

        // Полоса прокрутки — тонкая, у правого края, только когда есть куда.
        let room = pass.limit - top;
        let full = extent + self.scroll - top;
        if full > room && room > 0 {
            let track = Rect::new(area.right() - ctx.px(8) as i32, top, ctx.px(4), room as u32);
            let thumb_h = ((room as i64 * room as i64) / full as i64).max(ctx.px(24) as i64) as u32;
            let thumb_y = top + ((self.scroll as i64 * (room as i64 - thumb_h as i64)) / (full - room).max(1) as i64) as i32;
            let thumb = Rect::new(track.x, thumb_y, track.w, thumb_h);
            pass.on(|s| mini_ui::draw::rounded(s, thumb, ctx.px(2), p.line3.over(ctx.under), 255));
        }

        let limit = pass.limit;
        pass.top = area.y;
        pass.limit = main.bottom();
        self.draw_footer(pass, Rect::new(main.x, limit, main.w, bottom_h), &buttons);
        pass.extent = extent;
        pass.limit = limit;
        top
    }

    /// Заголовок страницы. В «Пакетах» он говорит, о чём спрашивают сейчас.
    fn heading(&self) -> String {
        if !self.search.is_empty() {
            return format!("Результаты: «{}»", self.search);
        }
        match (self.page, &self.programs) {
            (Section::Packages, Programs::Choose(_)) => "Установка пакета".to_string(),
            (Section::Packages, Programs::Confirm(name)) => format!("Удалить {name}?"),
            (section, _) => section.heading().to_string(),
        }
    }

    /// Результаты поиска: страницы, в названии или словах которых есть запрос.
    fn search_results(&self, pass: &mut Pass, x: i32, y: i32, w: u32) {
        let query = self.search.to_lowercase();
        let query = query.trim();
        let mut y = y;
        let mut found = 0;
        // Сначала страницы, в чьём имени есть запрос, потом — те, где он
        // нашёлся в пояснении и словах: «usb» обязано первым дать
        // «USB-устройства», а не «Устройства» с их «Клавиатура и USB».
        let named = ALL.into_iter().filter(|section| section.title().to_lowercase().contains(query));
        let rest = ALL.into_iter().filter(|section| {
            !section.title().to_lowercase().contains(query)
                && format!("{} {}", section.blurb(), section.keywords()).to_lowercase().contains(query)
        });
        for section in named.chain(rest) {
            found += 1;
            let desc = match section.parent() {
                Some(parent) => format!("{} › {}", parent.title(), section.blurb()),
                None => section.blurb().to_string(),
            };
            y = kit::row(pass, x, y, w, Some(section.icon()), section.title(), &desc, kit::Ctl::Nav, Some(Deed::Open(section)));
        }
        if found == 0 {
            pass.note(x, y, w, "Ничего не нашлось. Esc — очистить поиск.");
        }
    }

    /// Кнопки, прижатые к низу окна, — только там, где есть что подтверждать.
    fn buttons(&self) -> Vec<(Deed, &'static str, Weight)> {
        let mut out = Vec::new();
        if !self.search.is_empty() {
            return out;
        }
        if self.page == Section::Network && crate::net::is_present() {
            out.push((Deed::NetApply, "Применить", Weight::Primary));
            return out;
        }
        if self.page != Section::Packages {
            return out;
        }
        match &self.programs {
            Programs::List => {}
            Programs::Choose(_) => out.push((Deed::BackToList, "Вернуться к списку", Weight::Normal)),
            Programs::Confirm(name) => {
                out.push((Deed::Remove(name.clone()), "Удалить", Weight::Danger));
                out.push((Deed::BackToList, "Оставить", Weight::Normal));
            }
        }
        out
    }

    /// Низ окна: чем кончилось действие и кнопки разговора.
    fn draw_footer(&self, pass: &mut Pass, band: Rect, buttons: &[(Deed, &'static str, Weight)]) {
        let ctx = pass.ctx;
        let p = ctx.palette;
        let button_h = ctx.px(34);
        let top = band.y + (band.h as i32 - button_h as i32) / 2;
        if band.h < button_h {
            return;
        }
        pass.on(|s| paint::separator(ctx, s, band.x, band.y, band.w));

        let mut total = 0;
        let widths: Vec<u32> = buttons.iter().map(|(_, label, _)| kit::button_width(ctx, label)).collect();
        for width in &widths {
            total += width;
        }
        if !buttons.is_empty() {
            total += ctx.px(10) * (buttons.len() as u32 - 1);
        }
        let mut x = band.right() - total as i32;
        for ((deed, label, weight), width) in buttons.iter().zip(widths) {
            let rect = Rect::new(x, top, width, button_h);
            x += (width + ctx.px(10)) as i32;
            let focused = pass.deed(rect, deed.clone());
            if !pass.visible(rect) {
                continue;
            }
            let weight = *weight;
            pass.on(|s| paint::button(ctx, s, rect, weight, label, focused));
        }

        let Some(report) = &self.report else {
            return;
        };
        let (label, tone) = if report.ok { ("ГОТОВО", Tone::Ok) } else { ("ОШИБКА", Tone::Bad) };
        let chip_h = ctx.px(26);
        let chip = Rect::new(band.x, top + (button_h as i32 - chip_h as i32) / 2, paint::chip_width(ctx, label), chip_h);
        pass.on(|s| paint::chip(ctx, s, chip, label, tone));
        let text_x = chip.right() + ctx.px(10) as i32;
        let right = band.right() - total as i32 - ctx.px(16) as i32;
        let room = (right - text_x).max(0) as u32;
        let baseline = paint::baseline(ctx, Role::Caption, chip);
        pass.text(Role::Caption, text_x, baseline, room, &report.text, p.ink4);
    }
}
