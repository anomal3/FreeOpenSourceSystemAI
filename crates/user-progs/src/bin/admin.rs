//! «Администрирование» — центр настроек и инструментов (фаза П3, 20.11).
//!
//! # Зачем
//!
//! Это наша «Панель управления» (название выбрал Роман): одно окно, из
//! которого опытный человек находит любую настройку или инструмент системы,
//! не помня, где он лежит. Принцип плана (`docs/CONTROL_PANEL_PLAN.md`, п. 1):
//! ни одна настройка не живёт здесь второй раз. Каждый пункт либо запускает
//! программу из `/bin`, либо открывает страницу «Параметров» стола
//! (`ui open settings <страница>` через `SYS_KERNEL_COMMAND`), либо честно
//! говорит «функция запланирована».
//!
//! # Как устроено
//!
//! Слева — группы, как категории Панели управления Windows, но наши: «Система
//! и безопасность», «Оборудование и звук», «Сеть», «Программы», «Учётные
//! записи», «Оформление», «Часы и регион», «Специальные возможности»,
//! «Служебные». Справа — карточки групп с пунктами в одну-три колонки (по
//! ширине окна). Сверху поиск: набор сразу отбирает пункты по подписи,
//! пояснению и ключевым словам (в том числе латиницей и именами Windows:
//! `sound`, `services.msc`), Enter открывает выбранный.
//!
//! # Win+X
//!
//! `admin --winx` — меню опытного пользователя у кнопки «Пуск», как Win+X в
//! Windows: короткий список самого нужного (стол запускает его по Win+X).
//! Пункт открывается, меню закрывается; Esc — закрыть.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::cell::Cell;

use mini_ui::draw;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit::{self, Scroll, ScrollBar};
use mini_ui::paint::{self, Ctx, RowState, Tone};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, theme};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_RIGHT, WIN_KEY_UP, kernel_command, println, spawn,
};

const SPEC: Spec = Spec { name: "admin", title: "Administration", period_ms: 5_000 };
const WINX_SPEC: Spec = Spec { name: "admin", title: "Power User Menu", period_ms: 5_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const BACKSPACE: u32 = 0x08;
const CTRL_U: u32 = 0x15;

// Геометрия при масштабе 100 %.
const HEAD_H: u32 = 92;
const NAV_W: u32 = 248;
const NAV_ROW: u32 = 40;
/// От подписи «КАТЕГОРИИ» до первой строки списка групп.
const NAV_CAPS: u32 = 20;
const ITEM_H: u32 = 58;
const CARD_HEAD: u32 = 64;
const CARD_GAP: u32 = 16;
const COL_MIN: u32 = 290;
const MAX_QUERY: usize = 60;

// ---------------------------------------------------------------------------
// Каталог
// ---------------------------------------------------------------------------

/// Куда ведёт пункт.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Go {
    /// Программа из `/bin` с хвостом команды.
    Prog(&'static str),
    /// Страница «Параметров» стола (`ui open settings <страница>`).
    Page(&'static str),
    /// Окно стола: `terminal`, `about`.
    Window(&'static str),
    /// Ещё не сделано: почему и когда.
    Planned(&'static str),
}

struct Item {
    caption: &'static str,
    hint: &'static str,
    icon: Fi,
    go: Go,
    /// Слова для поиска сверх подписи: латиница, имена Windows.
    keys: &'static str,
}

struct Group {
    title: &'static str,
    about: &'static str,
    icon: Fi,
    tint: (u8, u8, u8),
    items: &'static [Item],
}

const fn item(caption: &'static str, hint: &'static str, icon: Fi, go: Go, keys: &'static str) -> Item {
    Item { caption, hint, icon, go, keys }
}

static GROUPS: [Group; 9] = [
    Group {
        title: "Система и безопасность",
        about: "состояние, журналы, службы, обслуживание",
        icon: Fi::ShieldCheck,
        tint: (0x80, 0xBD, 0xFB),
        items: &[
            item("Управление компьютером", "журнал, службы, устройства, диски, пользователи", Fi::Laptop, Go::Prog("compmgmt"), "compmgmt.msc computer management mmc"),
            item("Сведения о системе", "процессор, память, устройства; экспорт", Fi::Info, Go::Prog("sysinfo"), "msinfo32 system information"),
            item("Журнал событий", "загрузка, прошлый сбой, журнал на флешке", Fi::FileText, Go::Prog("eventlog"), "eventvwr event viewer log dmesg"),
            item("Службы", "службы init: состояние и перезапуск", Fi::Server, Go::Prog("services"), "services.msc init daemons"),
            item("Автозагрузка", "программы при входе и службы при запуске", Fi::Play, Go::Prog("startup"), "msconfig startup autostart autorun"),
            item("Монитор ресурсов", "процессор по ядрам, память, диск и сеть", Fi::Activity, Go::Prog("resmon"), "resmon perfmon resource monitor cpu"),
            item("Диспетчер задач", "процессы и их нагрузка", Fi::Cpu, Go::Prog("taskmgr"), "taskmgr task manager processes"),
            item("Редактор конфигурации", "файлы /etc с проверкой — вместо реестра", Fi::FileCode, Go::Prog("cfgedit"), "regedit registry config etc cfgedit"),
            item("Очистка диска", "временные файлы, корзина, кэши", Fi::Trash, Go::Prog("cleanmgr"), "cleanmgr disk cleanup"),
            item("Обновление системы", "слоты обновления и версия", Fi::Download, Go::Page("update"), "update windowsupdate"),
            item("Восстановление", "откат на прошлый слот, безопасный режим", Fi::History, Go::Page("recovery"), "recovery rollback"),
            item("Резервные копии", "копия /home и /etc на диск или флешку", Fi::Disc, Go::Planned("функция запланирована (фаза П4)"), "backup"),
            item("Шифрование тома", "шифрование данных на диске", Fi::Lock, Go::Planned("функция запланирована (фаза П4)"), "bitlocker encryption"),
        ],
    },
    Group {
        title: "Оборудование и звук",
        about: "устройства, экран, видеокарта, питание",
        icon: Fi::Speaker,
        tint: (0x51, 0xCE, 0xC7),
        items: &[
            item("Диспетчер устройств", "устройства и их драйверы", Fi::Layers, Go::Prog("devmgr"), "devmgmt.msc device manager drivers"),
            item("Видеокарта и драйверы", "NVIDIA GSP, узел NVK, Vulkan, мониторы", Fi::Monitor, Go::Prog("compmgmt --page gpu"), "gpu nvidia nvk vulkan dxdiag driver video"),
            item("Устройства и звук", "звук, мышь, подключённое", Fi::Volume, Go::Page("devices"), "sound audio mouse devices printers"),
            item("Экран", "разрешение, масштаб, мониторы", Fi::Brightness, Go::Page("display"), "display screen resolution scale monitor"),
            item("Диски и тома", "файловые системы, место, метки", Fi::Drive, Go::Prog("compmgmt --page disks"), "diskmgmt.msc disks volumes partitions exfat usb"),
            item("Электропитание", "гашение экрана, кнопка питания", Fi::Power, Go::Page("power"), "power sleep battery"),
            item("Клавиатура", "раскладки и переключение", Fi::Keyboard, Go::Page("keyboard"), "keyboard layout"),
            item("USB и съёмные носители", "флешки и их подключение", Fi::Usb, Go::Page("usb"), "usb autoplay removable"),
            item("Принтеры", "печать в системе не планируется", Fi::File, Go::Planned("печать не планируется: нет слоя CUPS"), "printers print"),
        ],
    },
    Group {
        title: "Сеть",
        about: "адрес, DNS, Wi-Fi, удалённый доступ",
        icon: Fi::Globe,
        tint: (0x6C, 0xD4, 0x9C),
        items: &[
            item("Сетевые подключения", "адаптеры: IP и DNS вручную или автоматически, отключение", Fi::NetworkWired, Go::Prog("ncpa"), "ncpa.cpl network connections adapter ethernet ip dns static dhcp"),
            item("Состояние сети", "адрес, шлюз, DNS, карта", Fi::Globe, Go::Page("network"), "network ethernet internet ip dns"),
            item("Беспроводная сеть", "Wi-Fi на странице «Сеть»", Fi::Wifi, Go::Page("network"), "wifi wireless wlan"),
            item("Сетевые инструменты", "ping, DNS, порты, соединения", Fi::Activity, Go::Planned("функция запланирована (фаза П4)"), "ping netstat tools"),
            item("Удалённый доступ", "закладки SSH/SFTP", Fi::Terminal, Go::Planned("функция запланирована (фаза П4); ssh и sftp есть в терминале"), "remote ssh sftp mstsc rdp"),
            item("Сетевой фильтр", "правила входящих портов", Fi::Shield, Go::Planned("функция запланирована: нужен фильтр пакетов"), "firewall"),
        ],
    },
    Group {
        title: "Программы",
        about: "установка, удаление, по умолчанию",
        icon: Fi::Package,
        tint: (0xB3, 0x9D, 0xF5),
        items: &[
            item("Программы и компоненты", "пакеты fpk: версии, удаление", Fi::Package, Go::Page("packages"), "appwiz.cpl programs features packages pkg uninstall"),
            item("Приложения", "установленное и его место", Fi::Apps, Go::Page("apps"), "apps applications"),
            item("Программы по умолчанию", "какая программа открывает тип файла", Fi::FolderOpen, Go::Page("defaultapps"), "default apps associations"),
            item("Автозагрузка", "что стартует при входе", Fi::Play, Go::Prog("startup"), "startup autostart"),
            item("Игры и Proton", "префиксы Wine, DXVK, журнал запуска", Fi::Image, Go::Planned("функция запланирована (фаза П3, п. 14)"), "games proton wine dxvk"),
        ],
    },
    Group {
        title: "Учётные записи",
        about: "пользователи, пароли, права",
        icon: Fi::User,
        tint: (0xF5, 0xB0, 0x6C),
        items: &[
            item("Учётные записи", "пароль, вход, другие пользователи", Fi::User, Go::Page("accounts"), "accounts users password netplwiz"),
            item("Пользователи и группы", "uid, группы, домашние папки", Fi::ShieldAdmin, Go::Prog("compmgmt --page users"), "lusrmgr.msc users groups passwd"),
            item("Связка ключей", "пароли SSH, Wi-Fi и сетевых папок", Fi::Lock, Go::Planned("функция запланирована (фаза П4)"), "credential manager keyring passwords"),
            item("Права и политики", "права пакетов, вход по SSH", Fi::Shield, Go::Planned("функция запланирована (фаза П4)"), "secpol.msc policy permissions"),
        ],
    },
    Group {
        title: "Оформление",
        about: "тема, фон, панель задач, «Пуск»",
        icon: Fi::Palette,
        tint: (0xF2, 0x8F, 0xB1),
        items: &[
            item("Персонализация", "тема и акцентный цвет", Fi::Palette, Go::Page("personal"), "personalization theme"),
            item("Фон рабочего стола", "картинка и цвет", Fi::Image, Go::Page("wallpaper"), "wallpaper background"),
            item("Цвета", "светлая и тёмная тема, акцент", Fi::Moon, Go::Page("colors"), "colors dark light accent"),
            item("Эффекты окон", "анимации, тени, прозрачность", Fi::Sliders, Go::Page("appearance"), "appearance effects animations shadows"),
            item("Панель задач и Dock", "положение, автоскрытие, значки", Fi::Dock, Go::Page("dock"), "taskbar dock panel"),
            item("Меню «Пуск»", "ширина, разделы, закреплённые", Fi::Menu, Go::Page("start"), "start menu"),
            item("Параметры проводника", "скрытые файлы, расширения, вид", Fi::Folder, Go::Page("explorer"), "folder options explorer files"),
            item("Шрифты", "установленные шрифты и предпросмотр", Fi::FileText, Go::Planned("функция запланирована (фаза П3, п. 13)"), "fonts ttf"),
        ],
    },
    Group {
        title: "Часы и регион",
        about: "дата, время, язык, раскладка",
        icon: Fi::Clock,
        tint: (0xE8, 0xC5, 0x5C),
        items: &[
            item("Дата и время", "часовой пояс, синхронизация", Fi::Calendar, Go::Page("time"), "date time timezone clock ntp"),
            item("Язык и раскладка", "раскладки клавиатуры и переключение", Fi::Keyboard, Go::Page("keyboard"), "language layout input intl.cpl"),
            item("Региональные стандарты", "форматы даты и чисел", Fi::Globe, Go::Planned("функция запланирована (фаза П3, п. 13)"), "region formats locale"),
        ],
    },
    Group {
        title: "Специальные возможности",
        about: "масштаб, крупный текст, ввод",
        icon: Fi::Eye,
        tint: (0x5C, 0xC8, 0xE8),
        items: &[
            item("Масштаб и крупный текст", "100–200 % на странице «Экран»", Fi::Maximize, Go::Page("display"), "accessibility scale zoom large text magnifier"),
            item("Эффекты и анимации", "выключить движение и прозрачность", Fi::Sliders, Go::Page("appearance"), "reduce motion animations"),
            item("Высокий контраст", "контрастная тема", Fi::Brightness, Go::Planned("функция запланирована (фаза П1, п. 4)"), "contrast"),
            item("Залипание клавиш", "сочетания по одной клавише", Fi::Keyboard, Go::Planned("функция запланирована (фаза П1, п. 4)"), "sticky keys"),
            item("Указатель мыши", "размер и скорость указателя", Fi::Cursor, Go::Planned("функция запланирована (фаза П1, п. 4)"), "mouse pointer cursor speed"),
        ],
    },
    Group {
        title: "Служебные",
        about: "терминал и мелкие утилиты",
        icon: Fi::Terminal,
        tint: (0xA8, 0xB4, 0xC4),
        items: &[
            item("Терминал", "командная строка, вкладки", Fi::Terminal, Go::Window("terminal"), "terminal cmd powershell shell console"),
            item("Выполнить", "открыть по имени (Win+R)", Fi::OpenTerminal, Go::Prog("run"), "run win+r"),
            item("Таблица символов", "знаки шрифтов, копирование", Fi::Keyboard, Go::Prog("charmap"), "charmap characters unicode"),
            item("Монитор системы", "графики счётчиков ядра", Fi::Activity, Go::Prog("sysmon"), "sysmon perfmon"),
            item("Параметры", "все настройки стола", Fi::Sliders, Go::Page("home"), "settings control"),
            item("О системе", "версия FossAi", Fi::Info, Go::Window("about"), "about winver version"),
        ],
    },
];

/// Меню Win+X: подпись и пункт каталога (по подписи), `None` — черта.
static WINX: [Option<(&str, Go)>; 17] = [
    Some(("Приложения", Go::Page("apps"))),
    Some(("Электропитание", Go::Page("power"))),
    Some(("Журнал событий", Go::Prog("eventlog"))),
    Some(("Система", Go::Prog("sysinfo"))),
    Some(("Диспетчер устройств", Go::Prog("devmgr"))),
    Some(("Сеть", Go::Page("network"))),
    Some(("Диски и тома", Go::Prog("compmgmt --page disks"))),
    Some(("Управление компьютером", Go::Prog("compmgmt"))),
    None,
    Some(("Терминал", Go::Window("terminal"))),
    Some(("Диспетчер задач", Go::Prog("taskmgr"))),
    Some(("Параметры", Go::Page("home"))),
    Some(("Файлы", Go::Prog("files"))),
    Some(("Выполнить", Go::Prog("run"))),
    None,
    Some(("Администрирование", Go::Prog("admin"))),
    Some(("Монитор ресурсов", Go::Prog("resmon"))),
];

/// Пункт по номеру группы и пункта.
fn item_at(at: (usize, usize)) -> &'static Item {
    &GROUPS[at.0].items[at.1]
}

/// Открыть пункт. `Ok` — что сделано (для строки состояния), `Err` — почему нет.
fn open(caption: &str, go: Go) -> Result<String, String> {
    match go {
        Go::Prog(command) => {
            let line = format!("/bin/{command}");
            let id = spawn(&line);
            if id < 0 {
                println(&format!("admin: cannot start '{line}': {id}"));
                return Err(match id {
                    user_abi::ERR_NOT_FOUND => format!("«{caption}»: программы {line} нет в этой сборке"),
                    user_abi::ERR_PERMISSION => format!("«{caption}»: недостаточно прав"),
                    code => format!("«{caption}»: запуск не удался, код {code}"),
                });
            }
            println(&format!("admin: open '{caption}' -> started '{line}' as {id}"));
            Ok(format!("Открыто: {caption}"))
        }
        Go::Page(page) => desktop(caption, &format!("ui open settings {page}")),
        Go::Window(window) => desktop(caption, &format!("ui open {window}")),
        Go::Planned(why) => {
            println(&format!("admin: '{caption}' is planned"));
            Err(format!("«{caption}»: {why}"))
        }
    }
}

/// Окно стола по просьбе `ui open …`.
fn desktop(caption: &str, request: &str) -> Result<String, String> {
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let got = kernel_command(request, &mut buffer);
    if got < 0 {
        println(&format!("admin: '{request}' refused: {got}"));
        return Err(format!("«{caption}»: стол не открыл окно (код {got})"));
    }
    let answer = String::from_utf8_lossy(&buffer[..got as usize]).trim().to_string();
    if answer.is_empty() || answer.contains("FAILED") {
        println(&format!("admin: '{request}' failed: {answer}"));
        return Err(format!("«{caption}»: стол не открыл окно"));
    }
    println(&format!("admin: open '{caption}' -> {answer}"));
    Ok(format!("Открыто: {caption}"))
}

fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).map(|ch| if ch == 'ё' { 'е' } else { ch }).collect()
}

// ---------------------------------------------------------------------------
// Окно-центр
// ---------------------------------------------------------------------------

/// Карточка группы на холсте (координаты от верха содержимого).
struct Card {
    group: usize,
    top: i32,
    h: u32,
    /// Пункты: номер в группе и прямоугольник (x — от левого края холста).
    items: Vec<(usize, Rect)>,
}

struct Layout {
    nav: Rect,
    head: Rect,
    search: Rect,
    body: Rect,
    status: Rect,
    cards: Vec<Card>,
    content_h: u32,
}

struct Admin {
    query: String,
    /// Выбранный пункт (группа, пункт).
    selected: Option<(usize, usize)>,
    /// Группа, на которой стоит левый список.
    group: Cell<usize>,
    hover: Option<(usize, usize)>,
    hover_nav: Option<usize>,
    scroll: Scroll,
    note: Option<(String, bool)>,
}

impl Admin {
    fn new() -> Self {
        let count: usize = GROUPS.iter().map(|group| group.items.len()).sum();
        let planned: usize = GROUPS.iter().flat_map(|group| group.items.iter()).filter(|item| matches!(item.go, Go::Planned(_))).count();
        println(&format!("admin: ready, {count} item(s) in {} group(s), {planned} planned", GROUPS.len()));
        Self { query: String::new(), selected: None, group: Cell::new(0), hover: None, hover_nav: None, scroll: Scroll::new(), note: None }
    }

    fn matches(&self, item: &Item, group: &Group) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let words: Vec<String> = lower(&self.query).split_whitespace().map(String::from).collect();
        let hay = lower(&format!("{} {} {} {}", item.caption, item.hint, item.keys, group.title));
        words.iter().all(|word| hay.contains(word.as_str()))
    }

    /// Видимые пункты по порядку обхода.
    fn visible(&self) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for (g, group) in GROUPS.iter().enumerate() {
            for (i, item) in group.items.iter().enumerate() {
                if self.matches(item, group) {
                    out.push((g, i));
                }
            }
        }
        out
    }

    fn layout(&self, ctx: Ctx, area: Rect) -> Layout {
        let nav_w = ctx.px(NAV_W).min(area.w / 3);
        let status_h = ctx.px(theme::STATUS_H);
        let nav = Rect::new(area.x, area.y, nav_w, area.h.saturating_sub(status_h));
        let status = Rect::new(area.x, area.bottom() - status_h as i32, area.w, status_h);
        let right_x = nav.right();
        let right_w = (area.right() - right_x).max(0) as u32;
        let head = Rect::new(right_x, area.y, right_w, ctx.px(HEAD_H));
        let pad = ctx.px(24) as i32;
        let search_w = ctx.px(340).min(right_w.saturating_sub(ctx.px(48)) / 2).max(ctx.px(160));
        let search = Rect::new(head.right() - pad - search_w as i32, head.y + (head.h as i32 - ctx.px(36) as i32) / 2, search_w, ctx.px(36));
        let body = Rect::new(right_x, head.bottom(), right_w, (status.y - head.bottom()).max(0) as u32);

        // Карточки: одна колонка карточек, пункты внутри — в 1..3 колонки.
        let inner_w = body.w.saturating_sub(pad as u32 * 2 + ctx.px(12));
        let cols = (inner_w / ctx.px(COL_MIN)).clamp(1, 3);
        let col_w = inner_w.saturating_sub(ctx.px(24)) / cols;
        let item_h = ctx.px(ITEM_H);
        let mut cards = Vec::new();
        let mut y = ctx.px(4) as i32;
        for (g, group) in GROUPS.iter().enumerate() {
            let shown: Vec<usize> = group.items.iter().enumerate().filter(|(_, item)| self.matches(item, group)).map(|(i, _)| i).collect();
            if shown.is_empty() {
                continue;
            }
            let rows = shown.len().div_ceil(cols as usize) as u32;
            let h = ctx.px(CARD_HEAD) + rows * item_h + ctx.px(12);
            let items = shown
                .iter()
                .enumerate()
                .map(|(n, &i)| {
                    let col = (n as u32) % cols;
                    let row = (n as u32) / cols;
                    (i, Rect::new(pad + ctx.px(12) as i32 + (col * col_w) as i32, y + (ctx.px(CARD_HEAD) + row * item_h) as i32, col_w.saturating_sub(ctx.px(8)), item_h.saturating_sub(ctx.px(4))))
                })
                .collect();
            cards.push(Card { group: g, top: y, h, items });
            y += (h + ctx.px(CARD_GAP)) as i32;
        }
        let content_h = y.max(0) as u32 + ctx.px(8);
        Layout { nav, head, search, body, status, cards, content_h }
    }

    fn set_query(&mut self, query: String) {
        self.query = query;
        self.scroll.reset();
        let visible = self.visible();
        self.selected = visible.first().copied().filter(|_| !self.query.is_empty());
        println(&format!("admin: search '{}' -> {} item(s)", self.query, visible.len()));
        if let Some(first) = self.selected {
            println(&format!("admin: first match '{}'", item_at(first).caption));
        }
        self.note = None;
    }

    fn activate(&mut self, at: (usize, usize)) {
        let item = item_at(at);
        self.selected = Some(at);
        self.note = Some(match open(item.caption, item.go) {
            Ok(text) => (text, true),
            Err(text) => (text, false),
        });
    }

    fn move_selection(&mut self, step: i32) {
        let visible = self.visible();
        if visible.is_empty() {
            return;
        }
        let now = self.selected.and_then(|at| visible.iter().position(|v| *v == at));
        let next = match now {
            None => 0,
            Some(index) => (index as i32 + step).clamp(0, visible.len() as i32 - 1) as usize,
        };
        self.selected = Some(visible[next]);
        self.group.set(visible[next].0);
    }

    fn jump_to_group(&mut self, ctx: Ctx, area: Rect, group: usize) {
        let layout = self.layout(ctx, area);
        if let Some(card) = layout.cards.iter().find(|card| card.group == group) {
            self.scroll.set(card.top - ctx.px(4) as i32);
            self.scroll.clamp(layout.body.h, layout.content_h);
            self.selected = card.items.first().map(|(i, _)| (group, *i));
            println(&format!("admin: group '{}'", GROUPS[group].title));
        }
        self.group.set(group);
    }

    fn draw_nav(&self, ctx: Ctx, s: &mut Surface, nav: Rect) {
        let p = ctx.palette;
        s.fill(nav, ctx.flat(p.panel));
        draw::vline(s, nav.right() - 1, nav.y, nav.h, p.line.color, p.line.alpha);
        let on = ctx.on(theme::panel_bg());
        let pad = ctx.px(14) as i32;
        // Знак центра.
        let tile = Rect::new(nav.x + pad, nav.y + ctx.px(20) as i32, ctx.px(40), ctx.px(40));
        draw::rounded(s, tile, ctx.px(10), p.acc, 255);
        let g = ctx.px(22);
        iconset::draw(s, Fi::Sliders, tile.x + (tile.w - g) as i32 / 2, tile.y + (tile.h - g) as i32 / 2, g, Color::rgb(0xFF, 0xFF, 0xFF), 255);
        let tx = tile.right() + ctx.px(12) as i32;
        let room = (nav.right() - tx - pad).max(0) as u32;
        paint::text_clipped(on, s, Role::Strong, tx, tile.y + ctx.px(1) as i32, room, "FossAi", p.ink);
        paint::text_clipped(on, s, Role::Caption, tx, tile.y + ctx.px(22) as i32, room, "этот компьютер", p.ink4);
        let mut y = tile.bottom() + ctx.px(22) as i32;
        paint::caps(on, s, nav.x + pad + ctx.px(6) as i32, y, "КАТЕГОРИИ");
        y += ctx.px(NAV_CAPS) as i32;
        let row_h = ctx.px(NAV_ROW);
        let current = self.group.get();
        for (g, group) in GROUPS.iter().enumerate() {
            let row = Rect::new(nav.x + ctx.px(8) as i32, y, nav.w.saturating_sub(ctx.px(16)), row_h.saturating_sub(ctx.px(2)));
            if row.bottom() > nav.bottom() {
                break;
            }
            let state = if g == current { RowState::Selected } else if self.hover_nav == Some(g) { RowState::Hover } else { RowState::Idle };
            paint::row(on, s, row, state);
            let (r, gg, b) = group.tint;
            let ic = ctx.px(18);
            iconset::draw(s, group.icon, row.x + ctx.px(12) as i32, row.y + (row.h - ic) as i32 / 2, ic, Color::rgb(r, gg, b), 255);
            let ink = if g == current { p.ink } else { p.ink2 };
            let lx = row.x + ctx.px(40) as i32;
            paint::text_clipped(on, s, Role::Body, lx, paint::baseline(on, Role::Body, row), (row.right() - lx - ctx.px(8) as i32).max(0) as u32, group.title, ink);
            y += row_h as i32;
        }
    }

    fn draw_head(&self, ctx: Ctx, s: &mut Surface, layout: &Layout) {
        let p = ctx.palette;
        let head = layout.head;
        s.fill(head, theme::window_bg());
        let pad = ctx.px(24) as i32;
        let title_room = (layout.search.x - head.x - pad * 2).max(0) as u32;
        paint::text_clipped(ctx, s, Role::Heading, head.x + pad, head.y + ctx.px(18) as i32, title_room, "Администрирование", p.ink);
        let count = self.visible().len();
        let sub = if self.query.is_empty() {
            String::from("Настройки и инструменты системы — по группам")
        } else {
            format!("Найдено: {count}")
        };
        paint::text_clipped(ctx, s, Role::Caption, head.x + pad, head.y + ctx.px(54) as i32, title_room, &sub, p.ink4);
        // Поиск.
        let field = layout.search;
        paint::sunk(ctx, s, field, ctx.px(8));
        if !self.query.is_empty() {
            draw::rounded_stroke(s, field, ctx.px(8), p.acc, 255);
        }
        let g = ctx.px(16);
        iconset::draw(s, Fi::Search, field.x + ctx.px(12) as i32, field.y + (field.h - g) as i32 / 2, g, p.ink3, 255);
        let tx = field.x + ctx.px(38) as i32;
        let room = field.w.saturating_sub(ctx.px(50));
        let base = paint::baseline(ctx, Role::Body, field);
        if self.query.is_empty() {
            paint::text_clipped(ctx, s, Role::Body, tx, base, room, "Поиск настроек и инструментов", p.ink4);
        } else {
            paint::text_clipped(ctx, s, Role::Body, tx, base, room, &self.query, p.ink);
            let caret_x = tx + ctx.face(Role::Body).width(&self.query).min(room) as i32;
            s.fill(Rect::new(caret_x + 1, field.y + ctx.px(9) as i32, ctx.px(2).max(1), field.h.saturating_sub(ctx.px(18))), p.acc);
        }
        draw::hline(s, head.x, head.bottom() - 1, head.w, p.line.color, p.line.alpha);
    }

    fn draw_cards(&self, ctx: Ctx, s: &mut Surface, layout: &Layout) {
        let p = ctx.palette;
        let body = layout.body;
        s.fill(body, theme::window_bg());
        let off = self.scroll.offset();
        let pad = ctx.px(24) as i32;
        let card_w = body.w.saturating_sub(pad as u32 * 2 + ctx.px(12));
        for card in &layout.cards {
            let top = body.y + card.top - off;
            if top > body.bottom() || top + (card.h as i32) < body.y {
                continue;
            }
            let group = &GROUPS[card.group];
            let rect = Rect::new(body.x + pad, top, card_w, card.h);
            paint::card(ctx, s, rect);
            let on = ctx.on(ctx.flat(p.card));
            // Шапка карточки: плитка, название, пояснение.
            let (r, g, b) = group.tint;
            let tint = Color::rgb(r, g, b);
            let tile = Rect::new(rect.x + ctx.px(16) as i32, top + ctx.px(14) as i32, ctx.px(36), ctx.px(36));
            draw::rounded(s, tile, ctx.px(9), tint, 48);
            draw::rounded_stroke(s, tile, ctx.px(9), tint, 110);
            let ic = ctx.px(20);
            iconset::draw(s, group.icon, tile.x + (tile.w - ic) as i32 / 2, tile.y + (tile.h - ic) as i32 / 2, ic, tint, 255);
            let tx = tile.right() + ctx.px(14) as i32;
            let room = (rect.right() - tx - ctx.px(16) as i32).max(0) as u32;
            paint::text_clipped(on, s, Role::Strong, tx, tile.y + ctx.px(1) as i32, room, group.title, p.ink);
            paint::text_clipped(on, s, Role::Caption, tx, tile.y + ctx.px(21) as i32, room, group.about, p.ink4);
            for &(i, at) in &card.items {
                let item = &group.items[i];
                let cell = Rect::new(body.x + at.x, body.y + at.y - off, at.w, at.h);
                if cell.bottom() < body.y || cell.y > body.bottom() {
                    continue;
                }
                let key = (card.group, i);
                let state = if self.selected == Some(key) { RowState::Selected } else if self.hover == Some(key) { RowState::Hover } else { RowState::Idle };
                paint::row(on, s, cell, state);
                let planned = matches!(item.go, Go::Planned(_));
                let ic = ctx.px(18);
                let icon_box = Rect::new(cell.x + ctx.px(10) as i32, cell.y + (cell.h as i32 - ctx.px(32) as i32) / 2, ctx.px(32), ctx.px(32));
                draw::rounded(s, icon_box, ctx.px(8), if planned { p.ink5 } else { tint }, if planned { 30 } else { 36 });
                iconset::draw(s, item.icon, icon_box.x + (icon_box.w - ic) as i32 / 2, icon_box.y + (icon_box.h - ic) as i32 / 2, ic, if planned { p.ink4 } else { tint }, 255);
                let lx = icon_box.right() + ctx.px(12) as i32;
                let mut room = (cell.right() - lx - ctx.px(8) as i32).max(0) as u32;
                let chip_label = "запланировано";
                if planned {
                    let chip_w = paint::chip_width(on, chip_label);
                    if room > chip_w + ctx.px(90) {
                        let chip = Rect::new(cell.right() - ctx.px(8) as i32 - chip_w as i32, cell.y + ctx.px(8) as i32, chip_w, ctx.px(20));
                        paint::chip(on, s, chip, chip_label, Tone::Muted);
                        room = room.saturating_sub(chip_w + ctx.px(8));
                    }
                }
                let line1 = cell.y + (cell.h as i32 - ctx.px(38) as i32) / 2;
                paint::text_clipped(on, s, Role::Body, lx, line1, room, item.caption, if planned { p.ink3 } else { p.ink });
                let full_room = (cell.right() - lx - ctx.px(8) as i32).max(0) as u32;
                paint::text_clipped(on, s, Role::Caption, lx, line1 + ctx.px(20) as i32, full_room, item.hint, p.ink4);
            }
        }
        if layout.cards.is_empty() {
            let y = body.y + ctx.px(40) as i32;
            paint::text(ctx, s, Role::Strong, body.x + pad, y, "Ничего не нашлось", p.ink2);
            paint::text(ctx, s, Role::Body, body.x + pad, y + ctx.px(28) as i32, "Попробуйте другое слово: «звук», «диск», «sound», «services».", p.ink4);
        }
        if let Some(bar) = ScrollBar::vertical(ctx, body, layout.content_h, &self.scroll) {
            bar.draw(ctx, s, &self.scroll);
        }
    }

    fn hit_item(&self, layout: &Layout, x: i32, y: i32) -> Option<(usize, usize)> {
        if !layout.body.contains(x, y) {
            return None;
        }
        let off = self.scroll.offset();
        for card in &layout.cards {
            for &(i, at) in &card.items {
                let cell = Rect::new(layout.body.x + at.x, layout.body.y + at.y - off, at.w, at.h);
                if cell.contains(x, y) {
                    return Some((card.group, i));
                }
            }
        }
        None
    }

    fn hit_nav(&self, ctx: Ctx, layout: &Layout, x: i32, y: i32) -> Option<usize> {
        if !layout.nav.contains(x, y) {
            return None;
        }
        let top = layout.nav.y + ctx.px(20 + 40 + 22 + NAV_CAPS) as i32;
        if y < top {
            return None;
        }
        let index = ((y - top) as u32 / ctx.px(NAV_ROW)) as usize;
        (index < GROUPS.len()).then_some(index)
    }
}

/// Последняя раскладка — для клавиш, которым нужна геометрия (PgUp/PgDn).
static LAST_AREA: [core::sync::atomic::AtomicI32; 4] = [
    core::sync::atomic::AtomicI32::new(0),
    core::sync::atomic::AtomicI32::new(0),
    core::sync::atomic::AtomicI32::new(0),
    core::sync::atomic::AtomicI32::new(0),
];
static LAST_SCALE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(100);

fn last_area() -> (Ctx, Rect) {
    use core::sync::atomic::Ordering::Relaxed;
    let ctx = Ctx::scaled(mini_ui::Scale::percent(LAST_SCALE.load(Relaxed)));
    let area = Rect::new(LAST_AREA[0].load(Relaxed), LAST_AREA[1].load(Relaxed), LAST_AREA[2].load(Relaxed) as u32, LAST_AREA[3].load(Relaxed) as u32);
    (ctx, area)
}

impl App for Admin {
    /// Smallest window (24.11, WM SDK): the 248-point section list and a page of rows.
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(640, 420)
    }

    fn key(&mut self, code: u32) -> bool {
        match code {
            ENTER => {
                let target = self.selected.or_else(|| self.visible().first().copied());
                if let Some(at) = target {
                    self.activate(at);
                }
            }
            ESCAPE if !self.query.is_empty() => self.set_query(String::new()),
            ESCAPE => return false,
            BACKSPACE => {
                let mut query = self.query.clone();
                query.pop();
                self.set_query(query);
            }
            CTRL_U => self.set_query(String::new()),
            WIN_KEY_DOWN | WIN_KEY_RIGHT => self.move_selection(1),
            WIN_KEY_UP | WIN_KEY_LEFT => self.move_selection(-1),
            WIN_KEY_HOME => {
                self.selected = self.visible().first().copied();
                self.scroll.reset();
            }
            WIN_KEY_END => self.selected = self.visible().last().copied(),
            WIN_KEY_PAGE_DOWN | WIN_KEY_PAGE_UP => {
                let (ctx, area) = last_area();
                let layout = self.layout(ctx, area);
                let pages = if code == WIN_KEY_PAGE_DOWN { 1 } else { -1 };
                self.scroll.page(pages, layout.body.h, layout.content_h, ctx.px(ITEM_H));
            }
            code if code < 0x0100_0000 => match char::from_u32(code) {
                Some(ch) if !ch.is_control() && self.query.chars().count() < MAX_QUERY => {
                    let mut query = self.query.clone();
                    query.push(ch);
                    self.set_query(query);
                }
                _ => return false,
            },
            _ => return false,
        }
        // Выбранный пункт — в видимой части.
        if let Some(at) = self.selected {
            let (ctx, area) = last_area();
            if !area.is_empty() {
                let layout = self.layout(ctx, area);
                for card in &layout.cards {
                    if let Some((_, rect)) = card.items.iter().find(|(i, _)| (card.group, *i) == at) {
                        let top = if card.items.first().is_some_and(|(i, _)| (card.group, *i) == at) { card.top } else { rect.y };
                        self.scroll.reveal(top, (rect.bottom() - top).max(0) as u32 + ctx.px(8), layout.body.h);
                        self.scroll.clamp(layout.body.h, layout.content_h);
                    }
                }
                self.group.set(at.0);
            }
        }
        true
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let layout = self.layout(ctx, area);
        if let Some(bar) = ScrollBar::vertical(ctx, layout.body, layout.content_h, &self.scroll) {
            if bar.press(&self.scroll, x, y) {
                return true;
            }
        }
        if let Some(group) = self.hit_nav(ctx, &layout, x, y) {
            self.jump_to_group(ctx, area, group);
            return true;
        }
        if layout.search.contains(x, y) {
            return false;
        }
        if let Some(at) = self.hit_item(&layout, x, y) {
            self.activate(at);
            return true;
        }
        false
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let layout = self.layout(ctx, area);
        ScrollBar::vertical(ctx, layout.body, layout.content_h, &self.scroll).is_some_and(|bar| bar.drag(&self.scroll, x, y))
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        self.scroll.release()
    }

    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let layout = self.layout(ctx, area);
        let bar = ScrollBar::vertical(ctx, layout.body, layout.content_h, &self.scroll);
        let mut changed = self.scroll.hover(bar.as_ref(), x, y);
        let hover = self.hit_item(&layout, x, y);
        let nav = self.hit_nav(ctx, &layout, x, y);
        if hover != self.hover || nav != self.hover_nav {
            self.hover = hover;
            self.hover_nav = nav;
            changed = true;
        }
        changed
    }

    fn leave(&mut self) -> bool {
        let changed = self.hover.is_some() || self.hover_nav.is_some();
        self.hover = None;
        self.hover_nav = None;
        changed
    }

    fn wheel_at(&mut self, area: Rect, ctx: Ctx, clicks: i32) -> bool {
        let layout = self.layout(ctx, area);
        let moved = self.scroll.wheel(clicks, layout.body.h, layout.content_h, ctx.px(ITEM_H) / 2);
        // Левый список следует за верхней видимой карточкой.
        let off = self.scroll.offset();
        if let Some(card) = layout.cards.iter().rev().find(|card| card.top <= off + ctx.px(8) as i32) {
            self.group.set(card.group);
        }
        moved
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Administration\x1fАдминистрирование"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        {
            use core::sync::atomic::Ordering::Relaxed;
            LAST_AREA[0].store(area.x, Relaxed);
            LAST_AREA[1].store(area.y, Relaxed);
            LAST_AREA[2].store(area.w as i32, Relaxed);
            LAST_AREA[3].store(area.h as i32, Relaxed);
            LAST_SCALE.store(ctx.scale.pct(), Relaxed);
        }
        s.fill(area, theme::window_bg());
        let layout = self.layout(ctx, area);
        self.scroll.clamp(layout.body.h, layout.content_h);
        // Тело первым: карточки, уехавшие под шапку, шапка закрывает собой.
        self.draw_cards(ctx, s, &layout);
        self.draw_head(ctx, s, &layout);
        self.draw_nav(ctx, s, layout.nav);
        let total: usize = GROUPS.iter().map(|group| group.items.len()).sum();
        let text = match &self.note {
            Some((text, _)) => text.clone(),
            None => format!("Пунктов: {total} · набирайте, чтобы искать · Enter — открыть · Win+X — короткое меню"),
        };
        kit::status_bar(ctx, s, layout.status, &text);
    }
}

// ---------------------------------------------------------------------------
// Меню Win+X
// ---------------------------------------------------------------------------

const WINX_ROW: u32 = 34;
const WINX_SEP: u32 = 13;
const WINX_PAD: u32 = 8;

struct WinX {
    selected: usize,
    hover: Option<usize>,
    done: bool,
}

impl WinX {
    fn new() -> Self {
        println(&format!("admin: win+x menu ready, {} item(s)", WINX.iter().flatten().count()));
        Self { selected: 0, hover: None, done: false }
    }

    fn rows(ctx: Ctx, area: Rect) -> Vec<(usize, Rect)> {
        let mut y = area.y + ctx.px(WINX_PAD) as i32;
        let mut out = Vec::new();
        for (index, entry) in WINX.iter().enumerate() {
            if entry.is_some() {
                let h = ctx.px(WINX_ROW);
                out.push((index, Rect::new(area.x + ctx.px(6) as i32, y, area.w.saturating_sub(ctx.px(12)), h)));
                y += h as i32;
            } else {
                y += ctx.px(WINX_SEP) as i32;
            }
        }
        out
    }

    fn height() -> u32 {
        WINX.iter().map(|entry| if entry.is_some() { WINX_ROW } else { WINX_SEP }).sum::<u32>() + WINX_PAD * 2
    }

    fn step(&mut self, forward: bool) {
        let count = WINX.len();
        let mut at = self.selected;
        for _ in 0..count {
            at = if forward { (at + 1) % count } else { (at + count - 1) % count };
            if WINX[at].is_some() {
                break;
            }
        }
        self.selected = at;
    }

    fn activate(&mut self, index: usize) {
        if let Some(Some((caption, go))) = WINX.get(index) {
            println(&format!("admin: win+x '{caption}'"));
            let _ = open(caption, *go);
            self.done = true;
        }
    }
}

impl App for WinX {
    fn key(&mut self, code: u32) -> bool {
        match code {
            ENTER => self.activate(self.selected),
            ESCAPE => {
                println("admin: win+x closed");
                self.done = true;
            }
            WIN_KEY_DOWN => self.step(true),
            WIN_KEY_UP => self.step(false),
            WIN_KEY_HOME => self.selected = 0,
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if let Some((index, _)) = Self::rows(ctx, area).into_iter().find(|(_, rect)| rect.contains(x, y)) {
            self.selected = index;
            self.activate(index);
            return true;
        }
        false
    }

    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let hover = Self::rows(ctx, area).into_iter().find(|(_, rect)| rect.contains(x, y)).map(|(index, _)| index);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    fn leave(&mut self) -> bool {
        self.hover.take().is_some()
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn quits_on_q(&self) -> bool {
        false
    }

    fn resizable(&self) -> bool {
        false
    }

    fn window_style(&self) -> u32 {
        user_abi::WIN_STYLE_DIALOG | user_abi::WIN_STYLE_AT_START
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Power User Menu\x1fБыстрое меню"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        s.fill(area, theme::window_bg());
        let mut last_bottom = area.y + ctx.px(WINX_PAD) as i32;
        for (index, rect) in Self::rows(ctx, area) {
            // Черта на месте пропуска.
            if rect.y > last_bottom + ctx.px(4) as i32 {
                let mid = last_bottom + (rect.y - last_bottom) / 2;
                draw::hline(s, area.x + ctx.px(14) as i32, mid, area.w.saturating_sub(ctx.px(28)), p.line2.color, p.line2.alpha);
            }
            last_bottom = rect.bottom();
            let Some(Some((caption, go))) = WINX.get(index) else { continue };
            let state = if index == self.selected { RowState::Selected } else if self.hover == Some(index) { RowState::Hover } else { RowState::Idle };
            paint::row(ctx, s, rect, state);
            let icon = winx_icon(*go);
            let g = ctx.px(16);
            iconset::draw(s, icon, rect.x + ctx.px(12) as i32, rect.y + (rect.h - g) as i32 / 2, g, if index == self.selected { p.acc_ink } else { p.ink3 }, 255);
            let lx = rect.x + ctx.px(40) as i32;
            paint::text_clipped(ctx, s, Role::Body, lx, paint::baseline(ctx, Role::Body, rect), (rect.right() - lx).max(0) as u32, caption, p.ink);
        }
    }
}

/// Значок пункта Win+X — тот же, что у пункта каталога с той же целью.
fn winx_icon(go: Go) -> Fi {
    GROUPS.iter().flat_map(|group| group.items.iter()).find(|item| item.go == go).map_or(
        match go {
            Go::Prog("files") => Fi::Folder,
            Go::Prog("admin") => Fi::Sliders,
            Go::Page("apps") => Fi::Apps,
            Go::Page("home") => Fi::Sliders,
            _ => Fi::Dots,
        },
        |item| item.icon,
    )
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в `_start`.
    let args = unsafe { Args::new(argc, argv) };
    let winx = (1..args.len()).any(|index| args.get(index) == Some("--winx"));
    if winx {
        let info = app::start(&WINX_SPEC);
        let scale = app::ui_scale(&info);
        let width = (300u32 * scale).min(info.screen_w);
        let height = (WinX::height() * scale).min(info.screen_h);
        app::run(&WINX_SPEC, &info, (width, height), |_, _| WinX::new())
    }
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (1080, 720));
    app::run(&SPEC, &info, size, |_, _| Admin::new())
}
