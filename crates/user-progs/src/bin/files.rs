//! Файловый менеджер: содержимое смонтированного корня в окне программы.
//!
//! # Что здесь изменилось по сравнению с прошлой фазой
//!
//! Это окно было **частью ядра** — модуль `ui::files`, девятьсот строк, которые
//! читали файловую систему прямым вызовом и рисовали прямо в поверхность окна,
//! заведённого композитором. Теперь окно просит программа, каталог читается
//! системными вызовами, а рисует она сама — тем же `mini-ui`, которым рисует
//! себя стол.
//!
//! Смысл переезда не в красоте. Менеджер — это разбор чужих имён, чужих прав и
//! чужого содержимого; падать он обязан вместе со своим окном, а не вместе с
//! машиной. Это последнее большое окно, которое ядро рисовало само.
//!
//! # Что он доказывает
//!
//! Что цепочка «virtio-blk → GPT → ext2 → VFS → системный вызов» работает не
//! только в выводе команды: права, владелец и размер в окне взяты из inode, а
//! просмотр файла читает его блоки по-настоящему — и всё это из третьего
//! кольца, через `SYS_READDIR` и `SYS_READ`.
//!
//! # Почему раскладка считается одной функцией
//!
//! Потому что нарисованное и нажимаемое обязаны совпадать. Пока кнопка «назад»
//! рисовалась одной формулой, а искалась под указателем другой, они сходились
//! ровно до первой правки отступа — и расхождение выглядело не как ошибка
//! раскладки, а как «мышь не работает». [`layout`] отвечает на вопрос «где что
//! лежит» один раз, а [`Files::draw`] и [`Files::click`] её спрашивают.
//!
//! # Аргументы: `files [путь]`
//!
//! Путь — каталог, с которого начать. Его передаёт стол, когда человек открыл
//! значок папки: до переезда то же самое делал вызов `reveal` внутри ядра.
//! Файл в аргументе тоже годится — менеджер откроет его каталог и покажет
//! содержимое, потому что показать файл, не показав, где он лежит, значит
//! оставить человека без единственного способа выйти из просмотра куда-то,
//! кроме корня.
//!
//! # Чего программа не знает и знать пока неоткуда
//!
//! **Имени вошедшего.** Оно есть у ядра (`/etc/passwd` читает оно, а права на
//! этот файл — `0640 root`), и до программ не доходит ничем. Поэтому домашний
//! каталог в боковой колонке — это `/root` для нулевого uid и `/home` для
//! остальных: первое верно, второе честно. Гадать имя по uid нечем, а
//! показывать `/home/roman` всем подряд — хуже, чем показать общий каталог.

#![no_std]
#![no_main]

extern crate alloc;

/// Окно «Свойства» (24.11, `files --properties`). Файл — в `files/`:
/// `src/bin/` сборка считает программами.
#[path = "files/props.rs"]
mod props;
/// «Сеть» (24.11): `/net`, shared folders over SMB — `files/net.rs`.
#[path = "files/net.rs"]
mod net;
use net::{is_net, net_level, net_title, volume_art};

use alloc::collections::BTreeSet;
use alloc::format;
use core::cell::Cell;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::filedlg::Line;
use mini_ui::glyphicon::{self, Icon};
use mini_ui::iconset::{self, Fi};
use mini_ui::kit::{self, Entry};
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_abi::{Dirent, KIND_DIRECTORY, Stat, WIN_MOD_ALT, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use user_progs::app::{self, App, Spec};
use user_progs::{
    Args, SysInfo, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END, WIN_KEY_HOME, WIN_KEY_LEFT,
    WIN_KEY_MENU, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP, WIN_KEY_RIGHT, WIN_KEY_UP,
    close, create, mkdir, monotonic_ms, mounts, open, open_write, println, read, readdir_raw, remove,
    clipboard_get, clipboard_set, rename, spawn, stat, sysinfo, time_now, write,
};

/// Имя окна.
///
/// Латиницей и именно это слово: по нему автоматический стенд наводит мышь
/// (`Aim::Close("Files")`), и оно же стояло у окна, пока оно было частью ядра.
/// Переезд не должен быть заметен снаружи — в этом половина его проверки.
const TITLE: &str = "Files";

/// Два щелчка по одной строке не дальше этого срока — двойной щелчок.
///
/// Полсекунды, как в Windows по умолчанию. До фазы С4 открывал **второй
/// щелчок по уже выбранной строке**, сколько бы времени ни прошло, — и человек,
/// щёлкнувший строку через минуту, чтобы вернуть на неё взгляд, попадал внутрь
/// каталога. Роман назвал это «ужас, не интуитивно», и это было верно.
const DOUBLE_CLICK_MS: u64 = 500;

/// «Путь» страницы «Этот компьютер».
///
/// Страница живёт в той же навигации, что каталоги: в неё ведёт «вверх» из
/// корня и первая строка быстрого доступа, из неё «назад» возвращает в
/// каталог. Двоеточие в имени — чтобы ни один настоящий путь не совпал.
const COMPUTER: &str = "computer:";

/// Сколько байт отводится под список томов.
const MOUNTS_LIMIT: usize = 2048;

/// Сколько байт файла показывает просмотр.
///
/// Предел не косметический: размер файла приходит с носителя, и окно, в которое
/// вывалили сорок мегабайт, — это заполненная куча и остановка программы.
const PREVIEW_LIMIT: usize = 8 * 1024;

/// Сколько строк файла показывается.
const PREVIEW_LINES: usize = 256;

/// С какой ширины окна появляется боковая колонка.
///
/// Колонка в 240 точек съедает у списка треть окна шириной 700; ниже этого
/// порога быстрый доступ мешает тому, ради чего окно открыли.
const SIDE_FROM: u32 = 700;

/// Сколько записей каталога читается.
///
/// Предел здесь по той же причине, по которой он есть у просмотра: число
/// записей приходит с носителя. Тысяча строк — это больше, чем помещается на
/// любой экран, умноженное на запас; каталог длиннее показывается не целиком, и
/// об этом написано в строке состояния, а не умалчивается.
const MAX_ROWS: usize = 1024;

/// Паспорт окна для каркаса программ (фаза С8).
///
/// Период — как часто спрашивать систему о теме. События «тема изменилась»
/// договор не знает, и признак перечитывается; две секунды человек замечает
/// как «сработало», а не как «не сработало».
const SPEC: Spec = Spec { name: "files", title: TITLE, period_ms: 2_000 };

// ---------------------------------------------------------------------------
// Данные
// ---------------------------------------------------------------------------

/// Одна строка списка.
struct Row {
    name: String,
    directory: bool,
    /// Лежит ли запись там, где живут программы.
    ///
    /// Признак строки, а не вопрос к пути на каждой отрисовке: путь у всех
    /// строк списка один, и спрашивать его двадцать раз подряд значит двадцать
    /// раз ответить одно и то же.
    program: bool,
    mode: u32,
    size: u64,
    /// Время изменения, секунды эпохи Unix; ноль — неизвестно.
    mtime: u32,
    /// Для тома на странице «Этот компьютер» — его файловая система; у
    /// записи каталога пусто. Одна структура на обе страницы: выделение,
    /// клавиши и оба вида списка тогда работают одинаково.
    volume: String,
    /// У тома — свободно, байт (`size` — всего); ноль у `size` — неизвестно.
    free: u64,
    /// У тома — метка (`Z7S` у exFAT-диска, 20.11); пусто — нет.
    label: String,
}

impl Row {
    /// Запись каталога.
    fn entry(name: &str, directory: bool, program: bool, entry: &Dirent) -> Self {
        Self {
            name: name.to_string(),
            directory,
            program,
            mode: entry.mode,
            size: entry.size,
            mtime: entry.mtime,
            volume: String::new(),
            free: 0,
            label: String::new(),
        }
    }

    /// Том: имя — точка монтирования; место — всего и свободно (ноль — неизвестно).
    fn volume(point: &str, kind: &str, total: u64, free: u64) -> Self {
        Self {
            name: point.to_string(),
            directory: true,
            program: false,
            mode: 0,
            size: total,
            mtime: 0,
            volume: kind.to_string(),
            free,
            label: String::new(),
        }
    }

    const fn is_volume(&self) -> bool {
        !self.volume.is_empty()
    }

    /// Значок строки: каталог, пакет, программа или обычный файл.
    ///
    /// Исполняемый бит один в признак программы не годится: в образе initrd он
    /// стоит у **всего**, включая `README.TXT`, и список получался из одних
    /// коробок. Поэтому пакет узнаётся по расширению, программа — по
    /// исполняемому биту вместе с каталогом, где программам и место, а всё
    /// остальное остаётся файлом.
    fn icon(&self) -> (Icon, Tone, bool) {
        if self.is_volume() {
            (Icon::Disk, Tone::Accent, true)
        } else if self.directory {
            (Icon::Folder, Tone::Accent, true)
        } else if self.name.ends_with(".fpk") {
            (Icon::Package, Tone::Accent, false)
        } else if self.mode & 0o111 != 0 && self.program {
            (Icon::Terminal, Tone::Ok, false)
        } else {
            (Icon::File, Tone::Muted, false)
        }
    }
}

/// Разобранные значки: имя и картинка. Разбираются при первом показе.
type IconCache = core::cell::RefCell<Vec<(&'static str, Option<mini_ui::image::Image>)>>;

/// Нарисовать значок макета `name` в `rect`. `false` — такого нет.
fn draw_design_icon(cache: &IconCache, s: &mut Surface, rect: Rect, name: &str) -> bool {
    // Значки темы (22.11): одни на систему, `/usr/share/icons/<тема>.pak`.
    let _ = cache;
    user_progs::icons::draw(s, rect, name)
}

/// Значок и тип файла по имени, как в Проводнике (таблица — общая со
/// столом, `sysconf::filetypes`).
fn file_kind(name: &str, directory: bool) -> (&'static str, &'static str) {
    let kind = sysconf::filetypes::kind_of(name, directory);
    (kind.icon, kind.name)
}

/// Размер по-человечески: «48 КБ», «1,2 МБ».
fn human_size(bytes: u64) -> String {
    const K: u64 = 1024;
    if bytes < K {
        format!("{bytes} Б")
    } else if bytes < K * K {
        format!("{} КБ", bytes.div_ceil(K))
    } else if bytes < K * K * K {
        let tenths = bytes * 10 / (K * K);
        if tenths < 100 { format!("{},{} МБ", tenths / 10, tenths % 10) } else { format!("{} МБ", tenths / 10) }
    } else {
        let tenths = bytes * 10 / (K * K * K);
        format!("{},{} ГБ", tenths / 10, tenths % 10)
    }
}

/// Смещение местного времени, минуты (из `/etc/system.cfg`, как у ядра).
fn local_offset() -> i32 {
    let fd = open("/etc/system.cfg");
    if fd < 0 {
        return 0;
    }
    let mut buffer = [0u8; 2048];
    let got = read(fd, &mut buffer);
    close(fd);
    if got <= 0 {
        return 0;
    }
    let text = core::str::from_utf8(&buffer[..got as usize]).unwrap_or("");
    sysconf::value(text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

/// «дд.мм.гггг чч:мм»; ноль — пусто.
fn date_text(mtime: u32, offset: i32) -> String {
    if mtime == 0 {
        return String::new();
    }
    let local = calendar::DateTime::from_unix(i64::from(mtime) + i64::from(offset) * 60);
    format!("{:02}.{:02}.{} {:02}:{:02}", local.day, local.month, local.year, local.hour, local.minute)
}

/// Просмотр файла.
struct Preview {
    name: String,
    lines: Vec<String>,
    /// Пояснение под текстом: сколько показано и почему не всё.
    note: String,
    /// Сколько строк пролистано.
    scroll: usize,
}

/// Пункт контекстного меню.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Open,
    /// «Открыть с помощью» (22.11): программы для файла, и программы Windows
    /// из префиксов Wine (Notepad++ для `.txt`), и «Выбрать другую…».
    OpenWith,
    /// Папку — в новой вкладке (20.11); средняя кнопка делает то же.
    OpenTab,
    /// Папку — в новом окне.
    OpenWindow,
    Rename,
    Delete,
    /// Буфера обмена в системе нет, и пункт говорит об этом словами, а не
    /// пропадает: правило вехи v0.7b — нереализованное показывать с пометкой.
    CopyPath,
    Properties,
    NewFolder,
    NewTextFile,
    Refresh,
    /// Вернуть из корзины туда, откуда удалили.
    Restore,
    /// Стереть всё содержимое корзины.
    EmptyTrash,
    /// Вставить вырезанное или скопированное в эту папку (21.11).
    Paste,
    /// «Закрепить на панели быстрого доступа» / «Открепить…» (24.11).
    Pin,
    Unpin,
    /// «7-Zip ▸» (25.11, Roman: as the 7-Zip shell extension in Windows) —
    /// one cascading item in the upper block; its submenu is [`ZipItem`].
    SevenZip,
    /// Verb `n` of the settings store (`shell` hive, 25.11): see [`VERBS`].
    Verb(u8),
}

impl MenuItem {
    /// Пункты на записи каталога.
    const ON_ENTRY: [MenuItem; 6] = [
        MenuItem::Open,
        MenuItem::OpenWith,
        MenuItem::Rename,
        MenuItem::Delete,
        MenuItem::CopyPath,
        MenuItem::Properties,
    ];

    /// Пункты на папке (20.11): как у записи, и ещё «в новой вкладке» и «в
    /// новом окне» — сразу под «Открыть», как в Проводнике; за ними (24.11)
    /// «Закрепить на панели быстрого доступа» (у закреплённой — «Открепить»).
    const ON_DIR: [MenuItem; 8] = [
        MenuItem::Open,
        MenuItem::OpenTab,
        MenuItem::OpenWindow,
        MenuItem::Pin,
        MenuItem::Rename,
        MenuItem::Delete,
        MenuItem::CopyPath,
        MenuItem::Properties,
    ];

    /// Пункты на месте колонки быстрого доступа (24.11).
    const ON_PLACE: [MenuItem; 4] = [MenuItem::Open, MenuItem::OpenWindow, MenuItem::Pin, MenuItem::Properties];

    /// Пункты на пустом месте списка.
    const ON_FOLDER: [MenuItem; 4] = [MenuItem::NewFolder, MenuItem::NewTextFile, MenuItem::Refresh, MenuItem::Paste];

    /// Пункты на томе.
    const ON_VOLUME: [MenuItem; 2] = [MenuItem::Open, MenuItem::Properties];

    /// Пункты на удалённом — в корзине (макет 12): вернуть, стереть.
    const ON_TRASHED: [MenuItem; 3] = [MenuItem::Restore, MenuItem::Delete, MenuItem::Properties];

    /// Пункты на пустом месте корзины.
    const ON_TRASH: [MenuItem; 2] = [MenuItem::EmptyTrash, MenuItem::Refresh];

    fn title(self) -> &'static str {
        match self {
            MenuItem::Open => "Открыть",
            MenuItem::OpenWith => "Открыть с помощью…",
            MenuItem::OpenTab => "Открыть в новой вкладке",
            MenuItem::OpenWindow => "Открыть в новом окне",
            MenuItem::Rename => "Переименовать",
            MenuItem::Delete => "Удалить",
            MenuItem::CopyPath => "Копировать путь",
            MenuItem::Properties => "Свойства",
            MenuItem::NewFolder => "Создать папку",
            MenuItem::NewTextFile => "Создать текстовый файл",
            MenuItem::Refresh => "Обновить",
            MenuItem::Restore => "Восстановить",
            MenuItem::EmptyTrash => "Очистить корзину",
            MenuItem::Paste => "Вставить",
            MenuItem::Pin => "Закрепить на панели быстрого доступа",
            MenuItem::Unpin => "Открепить от панели быстрого доступа",
            MenuItem::SevenZip => "7-Zip",
            MenuItem::Verb(index) => verb_caption(index),
        }
    }

    fn icon(self) -> Option<Icon> {
        match self {
            MenuItem::Open => Some(Icon::ChevronRight),
            MenuItem::OpenTab | MenuItem::OpenWindow | MenuItem::OpenWith => None,
            MenuItem::Rename | MenuItem::CopyPath => None,
            MenuItem::Delete => Some(Icon::Close),
            MenuItem::Properties => Some(Icon::Info),
            MenuItem::NewFolder => Some(Icon::Folder),
            MenuItem::NewTextFile => Some(Icon::File),
            MenuItem::Refresh => Some(Icon::Update),
            MenuItem::Restore => Some(Icon::Back),
            MenuItem::EmptyTrash => Some(Icon::Close),
            MenuItem::Paste | MenuItem::Pin | MenuItem::Unpin => None,
            MenuItem::SevenZip => Some(Icon::Package),
            MenuItem::Verb(index) => verb_icon(index),
        }
    }

    /// Группа — между разными группами черта.
    const fn group(self) -> u8 {
        match self {
            MenuItem::Open
            | MenuItem::OpenWith
            | MenuItem::OpenTab
            | MenuItem::OpenWindow
            | MenuItem::Pin
            | MenuItem::Unpin
            | MenuItem::Rename
            | MenuItem::Delete
            | MenuItem::Restore
            | MenuItem::EmptyTrash => 0,
            MenuItem::CopyPath | MenuItem::Properties => 1,
            MenuItem::NewFolder | MenuItem::NewTextFile => 2,
            MenuItem::Refresh | MenuItem::Paste => 3,
            MenuItem::SevenZip => 4,
            MenuItem::Verb(_) => 5,
        }
    }

    const fn danger(self) -> bool {
        matches!(self, MenuItem::Delete | MenuItem::EmptyTrash)
    }
}

/// Context-menu verbs of the settings store for the open menu (25.11,
/// `sysconf::store::verbs`): the verb, its caption kept for the program's
/// life (menu titles are `&'static str`), and the path it acts on.
struct VerbTable(core::cell::UnsafeCell<Vec<(sysconf::store::Verb, &'static str, String)>>);
// SAFETY: «Файлы» run on one thread; the table is touched only from the
// event loop.
unsafe impl Sync for VerbTable {}
static VERBS: VerbTable = VerbTable(core::cell::UnsafeCell::new(Vec::new()));
/// Captions given out so far — each distinct one is leaked once.
static VERB_CAPTIONS: VerbTable = VerbTable(core::cell::UnsafeCell::new(Vec::new()));

fn verbs() -> &'static mut Vec<(sysconf::store::Verb, &'static str, String)> {
    // SAFETY: see `VerbTable` — one thread, no reference outlives a call.
    unsafe { &mut *VERBS.0.get() }
}

/// The caption, as `&'static str`, leaked once per distinct text.
fn intern_caption(text: &str) -> &'static str {
    // SAFETY: as above.
    let known = unsafe { &mut *VERB_CAPTIONS.0.get() };
    if let Some((_, caption, _)) = known.iter().find(|(_, caption, _)| *caption == text) {
        return caption;
    }
    let caption: &'static str = alloc::boxed::Box::leak(String::from(text).into_boxed_str());
    known.push((sysconf::store::Verb { id: String::new(), caption: String::new(), command: String::new(), icon: None, extended: false }, caption, String::new()));
    caption
}

fn verb_caption(index: u8) -> &'static str {
    verbs().get(usize::from(index)).map_or("…", |(_, caption, _)| caption)
}

fn verb_icon(index: u8) -> Option<Icon> {
    match verbs().get(usize::from(index)).and_then(|(verb, _, _)| verb.icon.as_deref()) {
        Some("folder") => Some(Icon::Folder),
        Some("file") => Some(Icon::File),
        Some("terminal") => Some(Icon::Terminal),
        Some("delete" | "trash" | "close") => Some(Icon::Close),
        Some("info") => Some(Icon::Info),
        Some("package" | "archive") => Some(Icon::Package),
        _ => None,
    }
}

/// An item of «7-Zip ▸» (25.11, as the 7-Zip shell extension of Windows):
/// `/bin/archiver` with the picked rows ([`ZipItem::command`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ZipItem {
    Open,
    /// «Открыть архив ▸ тип» — index in [`ZIP_TYPES`].
    OpenAs(u8),
    Extract,
    ExtractHere,
    ExtractTo,
    Test,
    Add,
    Add7z,
    AddZip,
    /// «CRC SHA ▸ метод» — index in [`ZIP_HASHES`].
    Hash(u8),
    /// «Открыть архив ▸» and «CRC SHA ▸» — rows with a submenu.
    OpenAsMenu,
    HashMenu,
}

/// Types of «Открыть архив ▸» (7-Zip: `*` — any, `#` — parser).
const ZIP_TYPES: [&str; 7] = ["*", "#", "7z", "zip", "tar", "rar", "iso"];

/// «CRC SHA ▸»: menu words and `-scrc` methods.
const ZIP_HASHES: [(&str, &str); 5] = [("CRC-32", "CRC32"), ("CRC-64", "CRC64"), ("SHA-1", "SHA1"), ("SHA-256", "SHA256"), ("*", "*")];

impl ZipItem {
    /// `/bin/archiver` arguments before the paths.
    fn command(self) -> String {
        match self {
            ZipItem::Open | ZipItem::OpenAsMenu | ZipItem::HashMenu => String::new(),
            ZipItem::OpenAs(index) => format!("--type {}", ZIP_TYPES.get(usize::from(index)).copied().unwrap_or("*")),
            ZipItem::Extract => String::from("--extract"),
            ZipItem::ExtractHere => String::from("--extract-here"),
            ZipItem::ExtractTo => String::from("--extract-to"),
            ZipItem::Test => String::from("--test"),
            ZipItem::Add => String::from("--add"),
            ZipItem::Add7z => String::from("--7z"),
            ZipItem::AddZip => String::from("--zip"),
            ZipItem::Hash(index) => format!("--hash {}", ZIP_HASHES.get(usize::from(index)).map_or("CRC32", |(_, key)| *key)),
        }
    }

    const fn submenu(self) -> bool {
        matches!(self, ZipItem::OpenAsMenu | ZipItem::HashMenu)
    }
}

/// An open level of «7-Zip ▸»: its rows, their words, the lit row, the row
/// of the parent card it hangs from, whether the arrows are here.
struct ZipLevel {
    items: Vec<ZipItem>,
    titles: Vec<String>,
    selected: usize,
    parent: usize,
    focus: bool,
}

impl ZipLevel {
    fn entries(&self) -> Vec<Entry<'_>> {
        self.items.iter().zip(&self.titles).map(|(item, title)| Entry { title, submenu: item.submenu(), ..Entry::NONE }).collect()
    }
}

/// Entries of the context menu with «7-Zip» marked as a submenu (25.11).
fn zip_entries(items: &[MenuItem]) -> Vec<Entry<'static>> {
    let mut list = entries(items);
    for (entry, item) in list.iter_mut().zip(items) {
        if *item == MenuItem::SevenZip {
            entry.submenu = true;
        }
    }
    list
}

/// Пункты для общего меню (`mini_ui::kit`, фаза С8).
fn entries(items: &[MenuItem]) -> Vec<Entry<'static>> {
    items
        .iter()
        .map(|item| Entry {
            title: item.title(),
            icon: item.icon(),
            danger: item.danger(),
            group: item.group(),
            // 25.11: the default item bold, shortcuts on the right (Windows 11).
            bold: *item == MenuItem::Open,
            keys: match item {
                MenuItem::Rename => Some("F2"),
                MenuItem::Delete => Some("Del"),
                MenuItem::Refresh => Some("F5"),
                MenuItem::Paste => Some("Ctrl+V"),
                _ => None,
            },
            ..Entry::NONE
        })
        .collect()
}

/// Чем занято меню.
enum MenuMode {
    /// Показывает пункты.
    Items,
    /// Просит новое имя: поле (24.11 — с выделением имени без расширения,
    /// как F2 в Проводнике) и скрытое расширение, которое допишется (когда
    /// расширения известных типов не показываются).
    Rename(Line, String),
    /// Спрашивает, удалять ли, — вопросом посреди окна (`kit::draw_ask`,
    /// 21.11); число — кнопка под Enter: 0 — «Удалить», 1 — «Отмена».
    Confirm(usize),
    /// «Открыть с помощью» (22.11): все программы для файла (`SYS_OPEN_WITH`)
    /// и выбранная строка; карточка показывает подходящие и последней строкой
    /// «Выбрать другую программу…».
    OpenWith(Vec<Prog>, usize),
    /// «Выбрать другую программу…» — окно посреди «Файлов».
    Choose(Chooser),
}

/// Программа из «Открыть с помощью» (22.11).
struct Prog {
    caption: String,
    /// Командная строка без файла (файл допишет [`with_line`]).
    command: String,
    icon: Option<mini_ui::image::Image>,
    /// `d` — выбрана человеком, `f` — подходит к типу, `o` — прочие.
    mark: u8,
}

impl Prog {
    fn fits(&self) -> bool {
        self.mark == b'd' || self.mark == b'f'
    }

    fn windows(&self) -> bool {
        self.command.contains("fossai-proton")
    }
}

/// Окно «Выбрать другую программу…».
struct Chooser {
    progs: Vec<Prog>,
    selected: usize,
    /// Первая показанная строка (список длиннее окна).
    first: usize,
    /// «Всегда использовать эту программу для файлов .ext».
    always: bool,
}

/// Программы для файла `path` — у ядра (`SYS_OPEN_WITH`), со значками.
fn load_progs(path: &str) -> Vec<Prog> {
    let text = user_progs::open_with_list(path);
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let mark = parts.next()?.bytes().next()?;
            let caption = String::from(parts.next()?);
            let command = String::from(parts.next()?);
            let icon = user_progs::open_with_icon(parts.next().unwrap_or("")).and_then(|bytes| mini_ui::image::Image::parse(&bytes));
            (!command.is_empty()).then_some(Prog { caption, command, icon, mark })
        })
        .collect()
}

/// Строка запуска `command` с файлом `path`: `fossai-proton` получает путь с
/// `%20` вместо пробела (ядро делит строку по пробелам).
fn with_line(command: &str, path: &str) -> String {
    // Ярлык интернета (`.url`, 24.11): программе, зарегистрированной для
    // `.url` (браузеру), — адрес из него, а не сам файл.
    if path.to_ascii_lowercase().ends_with(".url") && user_progs::reg_list("filetypes/.url/openwith/").iter().any(|(_, known)| known == command) {
        if let Some(url) = read_small(path).and_then(|text| sysconf::store::url_of_shortcut(&text)).filter(|url| !url.contains(char::is_whitespace)) {
            return format!("{command} {url}");
        }
    }
    if command.contains("fossai-proton") {
        format!("{command} {}", pct(path))
    } else if path.contains(char::is_whitespace) && !path.contains('"') {
        // A path with a space goes in quotes - the kernel hands it over as
        // one word (`user::launch_words`), as the desktop does (24.11).
        format!("{command} \"{path}\"")
    } else {
        format!("{command} {path}")
    }
}

/// Путь для строки запуска: ядро делит её по пробелам, поэтому пробел —
/// `%20`, `%` — `%25` (раскодируют `fossai-proton` и `files --properties`).
fn pct(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for ch in path.chars() {
        match ch {
            ' ' => encoded.push_str("%20"),
            '%' => encoded.push_str("%25"),
            _ => encoded.push(ch),
        }
    }
    encoded
}

/// Обратно из [`pct`].
fn unpct(text: &str) -> String {
    text.replace("%20", " ").replace("%25", "%")
}

/// Строк в карточке «Открыть с помощью»: подходящие и «Выбрать другую…».
fn openwith_rows(progs: &[Prog]) -> usize {
    progs.iter().filter(|prog| prog.fits()).count() + 1
}

/// Высота строки карточки и окна выбора.
const PROG_ROW_H: u32 = 34;
const CHOOSER_ROW_H: u32 = 44;
const CHOOSER_ROWS: usize = 7;

/// Раскладка окна выбора программы: карточка, строки, флажок, кнопки.
struct ChooserPlan {
    card: Rect,
    rows: Vec<Rect>,
    check: Rect,
    ok: Rect,
    cancel: Rect,
}

fn chooser_plan(ctx: Ctx, area: Rect, count: usize) -> ChooserPlan {
    let pad = ctx.px(20);
    let w = ctx.px(520).min(area.w.saturating_sub(ctx.px(24)));
    let shown = count.min(CHOOSER_ROWS).max(1) as u32;
    let head = ctx.px(58);
    let h = pad + head + shown * ctx.px(CHOOSER_ROW_H + 2) + ctx.px(12) + ctx.px(28) + ctx.px(20) + ctx.px(32) + pad;
    let h = h.min(area.h.saturating_sub(ctx.px(16)));
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
    let x = card.x + pad as i32;
    let inner_w = w.saturating_sub(pad * 2);
    let mut y = card.y + (pad + head) as i32;
    let mut rows = Vec::new();
    for _ in 0..shown {
        rows.push(Rect::new(x, y, inner_w, ctx.px(CHOOSER_ROW_H)));
        y += ctx.px(CHOOSER_ROW_H + 2) as i32;
    }
    y += ctx.px(12) as i32;
    let check = Rect::new(x, y, inner_w, ctx.px(28));
    let face = ctx.face(Role::Label);
    let bw = |label: &str| (face.width(label) + ctx.px(36)).max(ctx.px(104));
    let by = card.bottom() - (pad + ctx.px(32)) as i32;
    let cancel = Rect::new(card.right() - (pad + bw("Отмена")) as i32, by, bw("Отмена"), ctx.px(32));
    let ok = Rect::new(cancel.x - (ctx.px(8) + bw("Открыть")) as i32, by, bw("Открыть"), ctx.px(32));
    ChooserPlan { card, rows, check, ok, cancel }
}

/// Окно «Выбрать другую программу» (22.11) для файла `name`. `remember` —
/// из «Свойств» (24.11): выбор только запоминается для типа — флажка нет,
/// кнопка «ОК».
fn draw_chooser_card(ctx: Ctx, s: &mut Surface, area: Rect, chooser: &Chooser, name: &str, remember: bool) {
    let cp = chooser_plan(ctx, area, chooser.progs.len());
    draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 70);
    let inner = kit::card(ctx, s, cp.card);
    let p = inner.palette;
    let pad = ctx.px(20) as i32;
    let x = cp.card.x + pad;
    let width = cp.card.w.saturating_sub(ctx.px(40));
    paint::text_clipped(inner, s, Role::Heading, x, cp.card.y + pad, width, "Выбрать другую программу", p.ink);
    let ext = sysconf::filetypes::extension(name);
    let what = if remember { format!("Чем открывать файлы .{ext}?") } else { format!("Чем открыть «{name}»?") };
    paint::text_clipped(inner, s, Role::Caption, x, cp.card.y + pad + i32::from(ctx.face(Role::Heading).line) + ctx.px(6) as i32, width, &what, p.ink3);
    for (slot, row) in cp.rows.iter().enumerate() {
        let index = chooser.first + slot;
        let Some(prog) = chooser.progs.get(index) else { break };
        let on = index == chooser.selected;
        paint::row(inner, s, *row, if on { RowState::Selected } else { RowState::Idle });
        let side = ctx.px(28);
        let icon = Rect::new(row.x + ctx.px(10) as i32, row.y + (row.h as i32 - side as i32) / 2, side, side);
        draw_prog_icon(inner, s, icon, prog);
        let tx = icon.right() + ctx.px(12) as i32;
        let tw = (row.right() - tx - ctx.px(10) as i32).max(0) as u32;
        let note = match (prog.mark, prog.windows()) {
            (b'd', _) => "выбрана для этого типа",
            (b'f', true) => "рекомендуется · программа Windows",
            (b'f', false) => "рекомендуется",
            (_, true) => "программа Windows",
            _ => "программа FossAi",
        };
        let top = row.y + (row.h as i32 - i32::from(ctx.face(Role::Body).line) - i32::from(ctx.face(Role::Caption).line)) / 2;
        paint::text_clipped(inner, s, Role::Body, tx, top, tw, &prog.caption, if on { p.ink } else { p.ink2 });
        paint::text_clipped(inner, s, Role::Caption, tx, top + i32::from(ctx.face(Role::Body).line), tw, note, p.ink4);
    }
    // Список длиннее окна — сколько ещё ниже и выше.
    if chooser.progs.len() > cp.rows.len() {
        let more = format!("{}–{} из {}", chooser.first + 1, (chooser.first + cp.rows.len()).min(chooser.progs.len()), chooser.progs.len());
        if let Some(last) = cp.rows.last() {
            paint::text_right(inner, s, Role::Caption, last.right(), cp.check.y + ctx.px(6) as i32, &more, p.ink4);
        }
    }
    if remember {
        paint::separator(inner, s, cp.card.x, cp.ok.y - ctx.px(14) as i32, cp.card.w);
        paint::button(inner, s, cp.ok, Weight::Primary, "ОК", false);
        paint::button(inner, s, cp.cancel, Weight::Normal, "Отмена", false);
        return;
    }
    let side = ctx.px(16);
    let bx = Rect::new(cp.check.x, cp.check.y + (cp.check.h as i32 - side as i32) / 2, side, side);
    if chooser.always {
        draw::rounded(s, bx, ctx.px(3), p.acc, 255);
        glyphicon::draw(s, Icon::Check, bx.x, bx.y, bx.w, p.acc_ink, 255);
    } else {
        draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
    }
    let label = if ext.is_empty() { String::from("Всегда использовать эту программу") } else { format!("Всегда использовать для файлов .{ext}") };
    let lx = bx.right() + ctx.px(10) as i32;
    paint::text_clipped(inner, s, Role::Body, lx, paint::baseline(inner, Role::Body, cp.check), (cp.check.right() - lx - ctx.px(90) as i32).max(0) as u32, &label, p.ink2);
    paint::separator(inner, s, cp.card.x, cp.ok.y - ctx.px(14) as i32, cp.card.w);
    paint::button(inner, s, cp.ok, Weight::Primary, "Открыть", false);
    paint::button(inner, s, cp.cancel, Weight::Normal, "Отмена", false);
}

/// Раскладка окна «Параметры папок» (24.11): карточка, строки флажков,
/// «Восстановить значения по умолчанию», «ОК», «Отмена», «Применить».
struct OptionsPlan {
    card: Rect,
    rows: Vec<Rect>,
    restore: Rect,
    ok: Rect,
    cancel: Rect,
    apply: Rect,
}

fn options_plan(ctx: Ctx, area: Rect) -> OptionsPlan {
    let pad = ctx.px(20);
    let count = sysconf::store::FolderOptions::ROWS.len() as u32;
    let row_h = ctx.px(32);
    let w = ctx.px(640).min(area.w.saturating_sub(ctx.px(24)));
    let head = ctx.px(78);
    let h = (pad + head + count * row_h + ctx.px(12) + ctx.px(32) + ctx.px(28) + ctx.px(32) + pad).min(area.h.saturating_sub(ctx.px(16)));
    let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
    let x = card.x + pad as i32;
    let inner_w = w.saturating_sub(pad * 2);
    let mut y = card.y + (pad + head) as i32;
    let mut rows = Vec::new();
    for _ in 0..count {
        rows.push(Rect::new(x, y, inner_w, row_h));
        y += row_h as i32;
    }
    y += ctx.px(12) as i32;
    let face = ctx.face(Role::Label);
    let bw = |label: &str| (face.width(label) + ctx.px(36)).max(ctx.px(104));
    let restore_w = bw("Восстановить значения по умолчанию");
    let restore = Rect::new(card.right() - pad as i32 - restore_w as i32, y, restore_w, ctx.px(32));
    let by = card.bottom() - (pad + ctx.px(32)) as i32;
    let apply = Rect::new(card.right() - (pad + bw("Применить")) as i32, by, bw("Применить"), ctx.px(32));
    let cancel = Rect::new(apply.x - (ctx.px(8) + bw("Отмена")) as i32, by, bw("Отмена"), ctx.px(32));
    let ok = Rect::new(cancel.x - (ctx.px(8) + bw("ОК")) as i32, by, bw("ОК"), ctx.px(32));
    OptionsPlan { card, rows, restore, ok, cancel, apply }
}

/// Флажок: отмеченный — залит акцентом с галочкой.
fn draw_check(ctx: Ctx, s: &mut Surface, rect: Rect, on: bool) {
    let p = ctx.palette;
    let side = ctx.px(16);
    let bx = Rect::new(rect.x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
    if on {
        draw::rounded(s, bx, ctx.px(3), p.acc, 255);
        glyphicon::draw(s, Icon::Check, bx.x, bx.y, bx.w, p.acc_ink, 255);
    } else {
        draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
    }
}

/// Последняя строка карточки «Открыть с помощью».
const OTHER_PROGRAM: &str = "Выбрать другую программу…";

impl Chooser {
    /// Держать выбранную строку в окне.
    fn follow(&mut self) {
        if self.selected < self.first {
            self.first = self.selected;
        } else if self.selected >= self.first + CHOOSER_ROWS {
            self.first = self.selected + 1 - CHOOSER_ROWS;
        }
    }
}

/// Строка `row` карточки «Открыть с помощью» (`fits` подходящих; строка
/// `fits` — «Выбрать другую…», за чертой).
fn openwith_row_rect(ctx: Ctx, card: Rect, row: usize, fits: usize) -> Rect {
    let pad = ctx.px(6);
    let mut y = card.y + pad as i32 + (row as u32 * ctx.px(PROG_ROW_H)) as i32;
    if row == fits && fits > 0 {
        y += ctx.px(9) as i32;
    }
    Rect::new(card.x + pad as i32, y, card.w.saturating_sub(pad * 2), ctx.px(PROG_ROW_H))
}

/// Карточка «Открыть с помощью»: подходящие программы со значками и
/// «Выбрать другую программу…».
fn draw_openwith(ctx: Ctx, s: &mut Surface, card: Rect, progs: &[Prog], selected: usize) {
    let inner = kit::card(ctx, s, card);
    let p = inner.palette;
    let fits: Vec<&Prog> = progs.iter().filter(|prog| prog.fits()).collect();
    for row in 0..=fits.len() {
        let rect = openwith_row_rect(ctx, card, row, fits.len());
        if row == fits.len() && row > 0 {
            paint::separator(inner, s, card.x + ctx.px(12) as i32, rect.y - ctx.px(5) as i32, card.w.saturating_sub(ctx.px(24)));
        }
        let on = row == selected;
        paint::row(inner, s, rect, if on { RowState::Selected } else { RowState::Idle });
        let side = ctx.px(20);
        let icon = Rect::new(rect.x + ctx.px(10) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
        let tx = icon.right() + ctx.px(10) as i32;
        let tw = (rect.right() - tx - ctx.px(8) as i32).max(0) as u32;
        let ink = if on { p.ink } else { p.ink2 };
        match fits.get(row) {
            Some(prog) => {
                draw_prog_icon(inner, s, icon, prog);
                let label = if prog.mark == b'd' { format!("{} (по умолчанию)", prog.caption) } else { prog.caption.clone() };
                paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(inner, Role::Body, rect), tw, &label, ink);
            }
            None => {
                paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(inner, Role::Body, rect), tw, OTHER_PROGRAM, ink);
            }
        }
    }
}

/// Значок программы в квадрат `rect`: её картинка или общий знак.
fn draw_prog_icon(ctx: Ctx, s: &mut Surface, rect: Rect, prog: &Prog) {
    match prog.icon.as_ref() {
        Some(image) => image.draw(s, rect),
        None => paint::icon_tile(ctx, s, rect, if prog.windows() { Icon::Package } else { Icon::File }, Tone::Accent, false),
    }
}


/// Вопрос об удалении словами — для `kit::Ask` (21.11).
struct Question {
    title: String,
    text: &'static str,
    /// Что именно удаляется — строкой над пояснением; пусто — не нужно.
    detail: String,
    action: &'static str,
    /// Стирается безвозвратно: кнопка красная, значок — тревожный.
    forever: bool,
}

impl Question {
    fn as_ask(&self) -> kit::Ask<'_> {
        kit::Ask {
            icon: Icon::Close,
            tone: if self.forever { Tone::Bad } else { Tone::Accent },
            title: &self.title,
            detail: &self.detail,
            text: self.text,
            action: self.action,
            weight: if self.forever { Weight::Danger } else { Weight::Primary },
        }
    }
}

/// Контекстное меню — карточка поверх окна.
///
/// Рисует его сама программа, в своей поверхности: у ядра меню стола есть, но
/// оно знает про значки стола, а не про строки чужого окна. Договор окон
/// отдаёт правую кнопку двойкой в [`WIN_POINTER`] — и всё остальное здесь.
struct Menu {
    /// Верхний левый угол в координатах поверхности.
    x: i32,
    y: i32,
    items: Vec<MenuItem>,
    selected: usize,
    mode: MenuMode,
    /// Строка, к которой относится меню; `None` — к самому каталогу.
    target: Option<usize>,
    /// Меню места колонки быстрого доступа (24.11): его путь.
    place: Option<String>,
}

/// «Параметры папок» (24.11, как «Вид» у Проводника): что отмечено в окне
/// (применится по «ОК» или «Применить») и строка под клавиатурой — флажки,
/// затем «Восстановить значения по умолчанию», «ОК», «Отмена», «Применить».
struct OptionsDialog {
    draft: sysconf::store::FolderOptions,
    focus: usize,
}

/// Всплывающая подсказка у строки под указателем (24.11): строка, с какого
/// времени указатель над ней, и готовый текст, когда пора показать.
struct Tip {
    index: usize,
    since: u64,
    lines: Option<Vec<String>>,
}

/// Место колонки быстрого доступа, которое тянут (24.11): номер в списке
/// закреплённых, где нажали, куда встанет, началось ли.
struct PinDrag {
    from: usize,
    start: i32,
    to: usize,
    active: bool,
    path: String,
}

/// Свойства записи — карточка на месте списка, как просмотр файла.
struct Props {
    name: String,
    rows: Vec<(&'static str, String)>,
    /// Вкладка «Подробно» (макет 14): владелец, группа, права.
    details: Vec<(&'static str, String)>,
    /// Права для таблицы «Владелец / Группа / Остальные»; `None` — нет вкладки.
    mode: Option<u32>,
    /// Открытая вкладка: 0 — «Общие», 1 — «Подробно».
    tab: u8,
}

/// Состояние менеджера.
struct Files {
    path: String,
    /// Куда можно вернуться кнопкой «назад» — стек посещённых каталогов.
    ///
    /// Стек, а не одно «предыдущее место»: человек, зашедший на три уровня
    /// вниз, ждёт, что «назад» проведёт его тем же путём обратно, а не швырнёт
    /// в начало.
    back: Vec<String>,
    /// Куда можно пойти «вперёд» — то, откуда вернулись назад.
    forward: Vec<String>,
    rows: Vec<Row>,
    selected: usize,
    /// Сколько записей каталога не поместилось в [`MAX_ROWS`].
    dropped: usize,
    /// Ошибка чтения каталога вместо списка.
    error: Option<String>,
    preview: Option<Preview>,
    /// Домашний каталог для боковой колонки.
    home: String,
    /// Знакомый вид: подписи вместо имён, служебные деревья свёрнуты.
    ///
    /// По умолчанию включён, и это не мелочь: система, которую человек видит
    /// впервые, встречает его словами «Программы» и «Настройки», а не `/bin` и
    /// `/etc`. Выключается клавишей — тем же способом, каким везде показывают
    /// скрытые файлы, — и тогда видно настоящие имена и все каталоги.
    ///
    /// Настоящий путь при этом виден **всегда**, в адресной строке: подпись
    /// украшает, а не прячет. Подпись, вытеснившая путь, превращает «нет такого
    /// файла» в загадку.
    friendly: bool,
    /// Вид списка: таблица, «Список» (имена столбиками) или плитка;
    /// помнится в `~/.config/files.cfg` (21.11).
    view: ViewMode,
    /// Отметка, от которой считает Shift+щелчок (21.11, как в Проводнике):
    /// последняя строка, по которой щёлкнули без Shift.
    anchor: usize,
    /// Выделения нет вовсе (21.11): щёлкнули по пустому месту или обвели
    /// рамкой пустоту. Строка `selected` остаётся курсором клавиатуры, но
    /// не подсвечена и действиям не достаётся.
    no_cursor: bool,
    /// Модификаторы нажатия, которое сейчас обрабатывается (`click_with`).
    press_mods: i32,
    /// «Удалить содержимое папки» asked (25.11): the folder and how many
    /// objects are in it.
    emptying: Option<(String, usize)>,
    /// Ctrl зажат — перетаскивание копирует (нажатие и `WIN_KEYUP`).
    ctrl_held: bool,
    /// Рамка выделения по пустому месту (21.11).
    band: Option<Band>,
    /// Ширины столбцов «Дата», «Тип», «Размер» в точках макета (21.11:
    /// тянутся за границу в шапке, помнятся в настройках).
    cols: [u32; 3],
    /// Границу столбцов тянут: какую (0 — имя|дата, 1 — дата|тип, 2 —
    /// тип|размер), откуда и ширины до начала.
    col_drag: Option<(usize, i32, [u32; 3])>,
    /// Ширина столбца вида «Список» при этом масштабе (по самому длинному
    /// имени; считается раз на папку).
    list_w: Cell<Option<(u32, u32)>>,
    /// Строк в столбце вида «Список» в последнем кадре — шаг стрелок вбок.
    list_rows: Cell<usize>,
    /// Строки вынесены за окно и отпущены (21.11): предложены другому окну
    /// «Файлов» тем же ящиком, что вкладки, — метка, срок и что было в
    /// буфере обмена.
    item_offer: Option<(String, u64, Option<String>, usize)>,
    /// Указатель вошёл в окно: до этого времени смотреть, не бросили ли
    /// сюда строки другого окна.
    item_watch: Option<u64>,
    /// Последний щелчок по строке: её номер и время — для двойного щелчка.
    last_click: Option<(usize, u64)>,
    menu: Option<Menu>,
    props: Option<Props>,
    /// Ответ последнего действия — в строке состояния, пока не будет
    /// следующего действия.
    note: Option<String>,
    /// Значки макета (72g).
    icons: IconCache,
    /// Поиск в текущей папке (72g): набранное и стоит ли в поле курсор.
    query: String,
    searching: bool,
    /// Смещение местного времени для дат, минуты.
    offset: i32,
    /// Каталог корзины (`~/.local/share/Trash`, как у freedesktop; её же
    /// пополняет «Удалить» на столе).
    trash: String,
    /// Сортировка: по чему и в обратную ли сторону (папки всегда сверху).
    sort: SortKey,
    descending: bool,
    /// Показывать скрытые файлы (имя с точки).
    hidden: bool,
    /// Отмеченные строки — выделение нескольких (Shift+стрелки, Ctrl+A,
    /// пробел). Пусто — действует выбранная строка `selected`.
    picked: BTreeSet<usize>,
    /// Выпавшее меню панели команд («Сортировка», «Вид») и его пункт.
    pop: Option<(Command, usize)>,
    /// Вырезанное или скопированное: полные пути и «вырезано ли».
    clip: Option<(Vec<String>, bool)>,
    /// Идущее копирование.
    job: Option<CopyJob>,
    /// Перетаскивание строк мышью (02.11): откуда, где сейчас, куда бросить.
    drag: Option<Drag>,
    /// Прокрутка списка и плитки (20.11): колесо и полоса двигают вид, а не
    /// выделение.
    scroll: kit::Scroll,
    /// Прокрутка таблицы вбок, когда столбцы в окно не помещаются.
    hscroll: kit::Scroll,
    /// Сколько строк помещалось в последнем кадре — шаг PageUp и PageDown.
    page_rows: Cell<usize>,
    /// Вкладки (20.11). Ячейка открытой — пустая: её состояние живёт в
    /// полях выше (`path`, `back`, `forward`, `selected`, `scroll`).
    tabs: Vec<Tab>,
    /// Открытая вкладка.
    tab: usize,
    /// Вкладку тянут мышью.
    tab_drag: Option<TabDrag>,
    /// Меню вкладки (правая кнопка): какой вкладки, где, выбранный пункт.
    tab_menu: Option<(usize, i32, i32, usize)>,
    /// Open levels of «7-Zip ▸» of the context menu (25.11): the submenu and
    /// «CRC SHA ▸» / «Открыть архив ▸» in it.
    zip_menu: Vec<ZipLevel>,
    /// Закрыли последнюю вкладку — окно закрывается.
    quit: bool,
    /// Где указатель над окном (20.11, ядро присылает движения без кнопок):
    /// строка, место слева и вкладка под ним подсвечиваются. `None` —
    /// указатель ушёл с окна.
    pointer: Option<(i32, i32)>,
    /// Что подсвечено сейчас: окно перерисовывается только при смене.
    hot: Hot,
    /// Вкладку вынесли из полосы и отпустили (20.11): она предложена другому
    /// окну «Файлов» ([`TAB_DROP`]) — какая, метка предложения и до какого
    /// времени ждать ответа. Не взяли — уходит в своё окно.
    offer: Option<(usize, String, u64)>,
    /// Предложение лежит в буфере обмена: что там было до него.
    offer_clip: Option<String>,
    /// Указатель вошёл в окно над полосой вкладок: до этого времени
    /// проверять, не бросили ли сюда вкладку другого окна.
    drop_watch: Option<u64>,
    /// Наблюдение за открытой папкой (20.11, `SYS_FSWATCH`): очередь, что
    /// наблюдается (путь, номер) и пришло ли событие, которое ещё не
    /// показано (меню или копирование — подождут).
    watch: i64,
    watched: Option<(String, i64)>,
    changed: bool,
    /// Адрес правят текстом (21.11, как в Проводнике): щелчок справа от
    /// звеньев, Ctrl+L, Alt+D. Строка ввода — та же, что у окна выбора файла.
    address: Option<Line>,
    /// Выпавший у шеврона звена список подпапок (21.11).
    crumb_pop: Option<CrumbPop>,
    /// «Параметры папок» (24.11) — из хранилища настроек (`explorer/…`),
    /// перечитываются, когда счётчик хранилища сдвинулся.
    opts: sysconf::store::FolderOptions,
    reg_seq: u64,
    /// Закреплённые папки быстрого доступа (24.11, `explorer/pinned`).
    pinned: Vec<String>,
    /// Окно «Параметры папок».
    options: Option<OptionsDialog>,
    /// Всплывающая подсказка у строки (24.11).
    tip: Option<Tip>,
    /// Закреплённое место тянут, чтобы переставить (24.11).
    pin_drag: Option<PinDrag>,
    /// A network folder is being read by a child (24.11, `files/net.rs`).
    net_load: Option<net::NetLoad>,
    /// `netlogon` asks for the account of the open network folder.
    net_login: Option<net::NetLogin>,
}

/// Что под указателем подсвечено.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
enum Hot {
    #[default]
    Nothing,
    /// Строка списка или плитка.
    Row(usize),
    /// Место в боковой колонке.
    Place(usize),
    /// Вкладка, её крестик или «+».
    Tab(kit::TabHit),
    /// Звено адреса, его шеврон, «…» спрятанных звеньев (21.11).
    Crumb(usize),
    Chevron(usize),
    CrumbMore,
    /// Граница столбцов в шапке таблицы (21.11).
    Grip(usize),
}

/// Почтовый ящик переноса вкладок между окнами «Файлов» (20.11).
///
/// Окна — разные задачи, а указатель, пока тянут вкладку, захвачен окном,
/// из которого её тянут: второе окно не знает ни о нажатии, ни о
/// перетаскивании. Знает оно одно — после отпускания стол присылает окну под
/// указателем движение ([`App::hover`]). Поэтому так: окно-источник,
/// отпустив вынесенную вкладку, кладёт сюда предложение `offer <метка>
/// <время> <путь>` и ждёт [`TAB_DROP_WAIT_MS`]; окно, в которое указатель
/// вошёл над полосой вкладок, находит свежее чужое предложение, открывает
/// путь новой вкладкой и пишет `taken <метка>`. Источник видит ответ и
/// закрывает свою вкладку; не дождался — выносит её в новое окно, как раньше.
///
/// Где писать некуда (живая система: корень — FAT только для чтения, `/tmp`
/// не завести), ящик — буфер обмена стола с меткой [`TAB_DROP_TAG`];
/// источник потом возвращает в него то, что там было.
const TAB_DROP: &str = "/tmp/.files-tab-drop";

/// Метка письма в буфере обмена.
const TAB_DROP_TAG: &str = "\u{1f}files-tab ";

/// Положить письмо в ящик. `Some(true)` — через буфер обмена, `None` — некуда.
fn mailbox_write(text: &str) -> Option<bool> {
    let _ = make_dirs("/tmp");
    if write_file(TAB_DROP, format!("{text}\n").as_bytes()).is_ok() {
        return Some(false);
    }
    clipboard_set(&format!("{TAB_DROP_TAG}{text}")).then_some(true)
}

/// Письма в ящике: из файла и из буфера обмена.
fn mailbox_read() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(text) = read_small(TAB_DROP) {
        out.push(String::from(text.trim_end()));
    }
    if let Some(text) = clipboard_get().strip_prefix(TAB_DROP_TAG) {
        out.push(String::from(text));
    }
    out
}

/// Сколько окно-источник ждёт, что вкладку возьмут.
const TAB_DROP_WAIT_MS: u64 = 450;

/// Сколько окно, в которое вошёл указатель, высматривает предложение.
const TAB_DROP_WATCH_MS: u64 = 400;

/// Вкладка «Файлов» (20.11, как у Проводника Windows 11): своя папка, своя
/// история «назад/вперёд», своё выделение и прокрутка.
#[derive(Default)]
struct Tab {
    path: String,
    back: Vec<String>,
    forward: Vec<String>,
    selected: usize,
    scroll: i32,
}

/// Вкладку тянут: какую, где указатель и ушла ли она из полосы (тогда
/// отпускание выносит её в своё окно).
struct TabDrag {
    index: usize,
    at: (i32, i32),
    out: bool,
}

/// Меню вкладки.
const TAB_ENTRIES: [Entry<'static>; 4] = [
    Entry { title: "Новая вкладка", icon: None, danger: false, group: 0, ..Entry::NONE },
    Entry { title: "Переместить в новое окно", icon: None, danger: false, group: 1, ..Entry::NONE },
    Entry { title: "Закрыть вкладку", icon: Some(Icon::Close), danger: false, group: 2, ..Entry::NONE },
    Entry { title: "Закрыть другие вкладки", icon: None, danger: false, group: 2, ..Entry::NONE },
];

/// Самая узкая таблица, в которой видны все столбцы: имя не уже 160 точек,
/// дата, тип и размер. Окно уже этого (20.11) не теряет столбцы, а листает
/// таблицу вбок.
const TABLE_MIN_W: u32 = 600;

/// Строки, которые тянут мышью.
struct Drag {
    from: usize,
    start: (i32, i32),
    at: (i32, i32),
    /// Сдвинулись дальше порога — это перетаскивание, а не щелчок.
    active: bool,
    /// Папка под указателем, куда отпустить: путь и где она на экране.
    target: Option<(String, Rect)>,
    /// Что сделать, если кнопку отпустили, не сдвинув (21.11, Проводник):
    /// Ctrl по отмеченной строке снимает отметку при отпускании (иначе
    /// Ctrl+перетаскивание отмеченных было бы невозможно), щелчок внутри
    /// выделения оставляет одну эту строку.
    after: ClickAfter,
    /// Указатель за окном: отпускание предложит строки другому окну.
    out: bool,
    /// Точки монтирования: перенос между томами по умолчанию — копия.
    points: Vec<String>,
}

/// Что делает щелчок без перетаскивания при отпускании.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClickAfter {
    Nothing,
    Unpick,
    Collapse,
}

/// Рамка выделения: где нажали, где указатель, что было отмечено до неё
/// (Ctrl — рамка добавляет к нему) и началась ли.
struct Band {
    start: (i32, i32),
    at: (i32, i32),
    base: BTreeSet<usize>,
    active: bool,
}

/// Вид списка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ViewMode {
    Table,
    List,
    Tiles,
}

impl ViewMode {
    const ALL: [ViewMode; 3] = [ViewMode::Table, ViewMode::List, ViewMode::Tiles];

    const fn tag(self) -> &'static str {
        match self {
            ViewMode::Table => "table",
            ViewMode::List => "list",
            ViewMode::Tiles => "tiles",
        }
    }

    const fn title(self) -> &'static str {
        match self {
            ViewMode::Table => "Таблица",
            ViewMode::List => "Список",
            ViewMode::Tiles => "Плитка",
        }
    }

    const fn icon(self) -> Icon {
        match self {
            ViewMode::Table => Icon::Log,
            ViewMode::List => Icon::List,
            ViewMode::Tiles => Icon::Grid,
        }
    }
}

/// Ширины столбцов «Дата изменения», «Тип», «Размер» по умолчанию и
/// наименьшие, точки макета.
const COLS_DEFAULT: [u32; 3] = [135, 140, 80];
const COLS_MIN: [u32; 3] = [70, 60, 56];
const COL_MAX: u32 = 420;

/// Высота строки вида «Список», точки макета.
const LIST_ROW_H: u32 = 26;

/// Где помнятся вид и столбцы — от домашнего каталога.
const SETTINGS: &str = ".config/files.cfg";

/// По чему сортировать список.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    Name,
    Date,
    Kind,
    Size,
}

impl SortKey {
    const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Date, SortKey::Kind, SortKey::Size];

    const fn title(self) -> &'static str {
        match self {
            SortKey::Name => "Имя",
            SortKey::Date => "Дата изменения",
            SortKey::Kind => "Тип",
            SortKey::Size => "Размер",
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Date => "date",
            SortKey::Kind => "type",
            SortKey::Size => "size",
        }
    }
}

/// Шаг копирования: завести папку или скопировать файл.
enum Op {
    Dir(String),
    File { src: String, dst: String, size: u64 },
}

/// Копирование (или перенос между томами) кусками — по шагу на виток цикла
/// окна ([`App::step`]): ход виден, «Пауза» и «Отмена» нажимаются.
struct CopyJob {
    ops: Vec<Op>,
    next: usize,
    /// Открытые файлы текущего шага: откуда и куда.
    files: Option<(i64, i64)>,
    total: u64,
    done: u64,
    started: u64,
    paused: bool,
    /// Перенос: что стереть, когда всё скопировано.
    remove_after: Vec<String>,
    items: usize,
    from: String,
    to: String,
    /// Папка, куда копируется (полный путь): в журнал по окончании.
    dest: String,
    current: String,
    error: Option<String>,
}

/// Сколько байт копировать за виток цикла.
const COPY_STEP: usize = 512 * 1024;

impl Files {
    fn new(path: String, home: String) -> Self {
        let trash = format!("{home}/.local/share/Trash");
        let mut view = Self {
            trash,
            sort: SortKey::Name,
            descending: false,
            hidden: false,
            picked: BTreeSet::new(),
            pop: None,
            clip: None,
            job: None,
            drag: None,
            scroll: kit::Scroll::new(),
            hscroll: kit::Scroll::new(),
            page_rows: Cell::new(10),
            tabs: alloc::vec![Tab::default()],
            tab: 0,
            tab_drag: None,
            tab_menu: None,
            zip_menu: Vec::new(),
            address: None,
            crumb_pop: None,
            quit: false,
            pointer: None,
            hot: Hot::Nothing,
            offer: None,
            offer_clip: None,
            drop_watch: None,
            path,
            back: Vec::new(),
            forward: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            dropped: 0,
            error: None,
            preview: None,
            home,
            friendly: true,
            view: ViewMode::Table,
            anchor: 0,
            no_cursor: false,
            press_mods: 0,
            emptying: None,
            ctrl_held: false,
            band: None,
            cols: COLS_DEFAULT,
            col_drag: None,
            list_w: Cell::new(None),
            list_rows: Cell::new(1),
            item_offer: None,
            item_watch: None,
            last_click: None,
            menu: None,
            props: None,
            note: None,
            icons: IconCache::default(),
            query: String::new(),
            searching: false,
            offset: local_offset(),
            watch: user_progs::fswatch_open(),
            watched: None,
            changed: false,
            opts: sysconf::store::FolderOptions::DEFAULT,
            reg_seq: u64::MAX,
            pinned: Vec::new(),
            options: None,
            tip: None,
            pin_drag: None,
            net_load: None,
            net_login: None,
        };
        view.load_settings();
        view.load_options();
        view.reload();
        view
    }

    /// «Параметры папок» и закреплённые папки — из хранилища настроек
    /// (24.11), если оно менялось с прошлого раза. `true` — что-то сменилось.
    fn load_options(&mut self) -> bool {
        let seq = user_progs::reg_seq();
        if seq == self.reg_seq {
            return false;
        }
        self.reg_seq = seq;
        let opts = sysconf::store::FolderOptions::read(user_progs::reg_get);
        let pinned = match user_progs::reg_get(sysconf::store::PINNED_KEY) {
            Some(value) => sysconf::store::pinned_list(&value),
            None => default_pins(&self.home),
        };
        let changed = opts != self.opts || pinned != self.pinned;
        if opts != self.opts {
            println(&format!(
                "files: folder options hidden {} ext {} path {} window {} status {} tips {} sizes {}",
                u8::from(opts.show_hidden),
                u8::from(!opts.hide_known_ext),
                u8::from(opts.full_path_title),
                u8::from(opts.new_window),
                u8::from(opts.status_bar),
                u8::from(opts.info_tips),
                u8::from(opts.folder_size_tips)
            ));
        }
        self.opts = opts;
        self.hidden = opts.show_hidden;
        self.pinned = pinned;
        STATUS_BAR.store(opts.status_bar, core::sync::atomic::Ordering::Relaxed);
        changed
    }

    /// Перечитать папку, сохранив выделение по имени.
    fn reload_keep(&mut self) {
        let keep = self.rows.get(self.selected).map(|row| row.name.clone());
        let offset = self.scroll.offset();
        self.reload();
        if let Some(at) = keep.and_then(|name| self.rows.iter().position(|row| row.name == name)) {
            self.selected = at;
        }
        self.scroll.set(offset);
    }

    /// Записать одну настройку человека. `false` — не записалось.
    fn store_option(key: &str, value: &str) -> bool {
        match user_progs::reg_set("user", key, value) {
            Ok(()) => true,
            Err(code) => {
                println(&format!("files: cannot store {key}: {code}"));
                false
            }
        }
    }

    /// Закрепить папку (или открепить) на панели быстрого доступа.
    fn set_pinned(&mut self, path: &str, pin: bool) {
        let mut list = self.pinned.clone();
        list.retain(|known| known != path);
        if pin {
            list.push(path.to_string());
        }
        self.save_pins(list);
        println(&format!("files: {} '{path}'", if pin { "pinned" } else { "unpinned" }));
        self.note = Some(format!("{} «{}»", if pin { "Закреплено" } else { "Откреплено" }, place_title(path, &self.home)));
    }

    fn save_pins(&mut self, list: Vec<String>) {
        if Self::store_option(sysconf::store::PINNED_KEY, &sysconf::store::pinned_value(&list)) {
            self.reg_seq = user_progs::reg_seq();
        }
        self.pinned = list;
    }

    /// Открыть «Параметры папок».
    fn open_options(&mut self) {
        self.close_menu();
        self.pop = None;
        self.options = Some(OptionsDialog { draft: self.opts, focus: 0 });
        println("files: folder options opened");
    }

    /// Записать то, что отмечено в «Параметрах папок».
    fn apply_options(&mut self) {
        let Some(draft) = self.options.as_ref().map(|dialog| dialog.draft) else { return };
        let old = self.opts;
        let mut ok = true;
        for (index, (key, value)) in draft.pairs().into_iter().enumerate() {
            if old.get(index) != draft.get(index) {
                ok &= Self::store_option(key, value);
            }
        }
        println(&format!("files: folder options saved{}", if ok { "" } else { " (not all)" }));
        if !ok {
            self.note = Some(String::from("Не все параметры удалось сохранить"));
        }
        if self.load_options() {
            self.reload_keep();
        }
    }

    /// Вид и ширины столбцов из `~/.config/files.cfg` (21.11).
    fn load_settings(&mut self) {
        let Some(text) = read_small(&format!("{}/{SETTINGS}", self.home)) else {
            return;
        };
        if let Some(mode) = sysconf::value(&text, "view").and_then(|tag| ViewMode::ALL.into_iter().find(|mode| mode.tag() == tag.trim())) {
            self.view = mode;
        }
        if let Some(list) = sysconf::value(&text, "columns") {
            let widths: Vec<u32> = list.split(',').filter_map(|part| part.trim().parse().ok()).collect();
            if let [date, kind, size] = widths[..] {
                for (slot, (width, min)) in self.cols.iter_mut().zip([date, kind, size].into_iter().zip(COLS_MIN)) {
                    *slot = width.clamp(min, COL_MAX);
                }
            }
        }
        println(&format!("files: settings view {} columns {},{},{}", self.view.tag(), self.cols[0], self.cols[1], self.cols[2]));
    }

    /// Запомнить вид и столбцы. Где писать некуда (живая система без
    /// записываемого дома) — молча: вид всё равно сменился.
    fn save_settings(&self) {
        let _ = make_dirs(&format!("{}/.config", self.home));
        let text = format!("view={}\ncolumns={},{},{}\n", self.view.tag(), self.cols[0], self.cols[1], self.cols[2]);
        if write_file(&format!("{}/{SETTINGS}", self.home), text.as_bytes()).is_err() {
            println("files: settings not saved");
        }
    }

    /// Сменить вид (меню «Вид», кнопки в строке состояния, `T`, `L`).
    fn set_view(&mut self, mode: ViewMode) -> bool {
        if mode == self.view {
            return false;
        }
        self.view = mode;
        self.scroll.reset();
        self.hscroll.reset();
        self.scroll.follow(self.selected);
        println(&format!("files: view {}", mode.tag()));
        self.save_settings();
        true
    }

    /// Подсвечена ли строка как выбранная: отмеченные, а без отметок —
    /// курсор (если выделение не снято щелчком по пустому месту).
    fn is_marked(&self, index: usize) -> bool {
        if self.picked.is_empty() { index == self.selected && !self.no_cursor } else { self.picked.contains(&index) }
    }

    /// Строка, к которой относятся действия над одной записью
    /// (переименовать, свойства, Enter): курсор, а если он не отмечен при
    /// отмеченных других — первая отмеченная. `None` — выделения нет.
    fn focus(&self) -> Option<usize> {
        let index = if !self.picked.is_empty() && !self.picked.contains(&self.selected) {
            *self.picked.iter().next()?
        } else if self.picked.is_empty() && self.no_cursor {
            return None;
        } else {
            self.selected
        };
        (index < self.rows.len()).then_some(index)
    }

    /// Папку открытой вкладки поменял кто-то другой (20.11: терминал создал
    /// файл, Блокнот сохранил) — перечитать, сохранив выделение и прокрутку.
    /// Зовётся на каждом витке цикла; события за виток сливаются в одно
    /// перечитывание.
    fn watch_step(&mut self) -> bool {
        let want = (!self.on_computer()).then(|| self.path.clone());
        if want.as_deref() != self.watched.as_ref().map(|(path, _)| path.as_str()) {
            if let Some((_, wd)) = self.watched.take() {
                user_progs::fswatch_remove(self.watch, wd);
            }
            if let Some(path) = want {
                let wd = user_progs::fswatch_add(self.watch, &path, 0);
                if wd >= 0 {
                    self.watched = Some((path, wd));
                }
            }
            // Только что перешли — список и так свежий.
            let _ = user_progs::fswatch_read(self.watch);
            self.changed = false;
            return false;
        }
        if !user_progs::fswatch_read(self.watch).is_empty() {
            self.changed = true;
        }
        let idle = self.menu.is_none() && self.drag.is_none() && self.band.is_none() && self.props.is_none() && self.job.is_none() && self.pop.is_none();
        if !self.changed || !idle {
            return false;
        }
        self.changed = false;
        let keep = self.rows.get(self.selected).map(|row| row.name.clone());
        let marked: Vec<String> = self.picked.iter().filter_map(|index| self.rows.get(*index)).map(|row| row.name.clone()).collect();
        let offset = self.scroll.offset();
        self.reload();
        if let Some(at) = keep.and_then(|name| self.rows.iter().position(|row| row.name == name)) {
            self.selected = at;
        }
        for name in marked {
            if let Some(at) = self.rows.iter().position(|row| row.name == name) {
                self.picked.insert(at);
            }
        }
        self.scroll.set(offset);
        println(&format!("files: folder changed, {} item(s) in {}", self.rows.len(), self.path));
        true
    }

    /// На странице «Этот компьютер»?
    fn on_computer(&self) -> bool {
        self.path == COMPUTER
    }

    /// Перечитать текущий каталог — или список томов.
    fn reload(&mut self) {
        self.rows.clear();
        self.selected = 0;
        self.anchor = 0;
        self.no_cursor = false;
        self.band = None;
        self.list_w.set(None);
        self.picked.clear();
        self.dropped = 0;
        self.error = None;

        if self.on_computer() {
            self.rows = computer_rows();
            return;
        }
        // A network folder is read in the background (24.11): connecting to
        // a computer can take seconds.
        if is_net(&self.path) {
            self.start_net_load();
            return;
        }

        match list_dir(&self.path) {
            Ok((rows, dropped)) => self.adopt_rows(rows, dropped),
            Err(text) => self.error = Some(text),
        }
    }

    /// Rows of the open folder, as read: folded trees, hidden files, order,
    /// search.
    fn adopt_rows(&mut self, rows: Vec<Row>, dropped: usize) {
        // Служебные деревья сворачиваются здесь, а не при отрисовке:
        // иначе стрелка вниз ходила бы по невидимым строкам, и выделение
        // пропадало бы на ровном месте.
        self.rows = if self.friendly {
            rows.into_iter()
                .filter(|row| !sysconf::winpath::is_folded(&join(&self.path, &row.name)))
                .collect()
        } else {
            rows
        };
        self.dropped = dropped;
        if !self.hidden {
            self.rows.retain(|row| !row.name.starts_with('.'));
        }
        // Каталоги наверх, дальше — как выбрано (по умолчанию по имени):
        // порядок записей в ext2 — это порядок вставки, то есть для
        // человека случайный.
        let (key, descending) = (self.sort, self.descending);
        self.rows.sort_by(|a, b| {
            let order = match key {
                SortKey::Name => a.name.cmp(&b.name),
                SortKey::Date => a.mtime.cmp(&b.mtime).then_with(|| a.name.cmp(&b.name)),
                SortKey::Kind => file_kind(&a.name, a.directory).1.cmp(file_kind(&b.name, b.directory).1).then_with(|| a.name.cmp(&b.name)),
                SortKey::Size => a.size.cmp(&b.size).then_with(|| a.name.cmp(&b.name)),
            };
            b.directory.cmp(&a.directory).then(if descending { order.reverse() } else { order })
        });
        // Поиск (72g): остаются записи, в имени которых есть набранное.
        if !self.query.is_empty() {
            let query = self.query.to_lowercase();
            let path = self.path.clone();
            let friendly = self.friendly;
            self.rows.retain(|row| {
                let shown = if friendly { sysconf::winpath::label(&join(&path, &row.name)).unwrap_or(&row.name) } else { &row.name };
                row.name.to_lowercase().contains(&query) || shown.to_lowercase().contains(&query)
            });
        }
    }

    /// Клавиша, пока курсор в поле поиска.
    fn key_search(&mut self, code: u32) -> bool {
        match code {
            ESCAPE => {
                self.searching = false;
                if !self.query.is_empty() {
                    self.query.clear();
                    self.reload();
                }
                true
            }
            ENTER => {
                self.searching = false;
                true
            }
            BACKSPACE => {
                if self.query.pop().is_some() {
                    self.reload();
                    println(&format!("files: search '{}'", self.query));
                }
                true
            }
            WIN_KEY_UP | WIN_KEY_DOWN => {
                self.searching = false;
                self.on_key(code)
            }
            _ => match char::from_u32(code) {
                Some(ch) if !ch.is_control() && self.query.chars().count() < 40 => {
                    self.query.push(ch);
                    self.reload();
                    println(&format!("files: search '{}'", self.query));
                    true
                }
                _ => false,
            },
        }
    }

    /// Кнопка панели команд (72g, макет 14).
    fn run_command(&mut self, command: Command) -> bool {
        let focus = self.focus();
        let row = focus.and_then(|index| self.rows.get(index)).filter(|row| !row.is_volume()).map(|row| row.name.clone());
        let focus = focus.unwrap_or(self.selected);
        match command {
            Command::NewFolder => {
                self.create_new(true);
            }
            Command::Cut => return self.to_clip(true),
            Command::Copy => return self.to_clip(false),
            Command::Paste => return self.paste(),
            Command::Sort | Command::View => {
                self.pop = if self.pop.is_some_and(|(open, _)| open == command) { None } else { Some((command, 0)) };
            }
            Command::Rename => {
                if row.is_none() {
                    return false;
                }
                self.start_rename(focus);
            }
            Command::Delete => {
                if row.is_none() {
                    return false;
                }
                self.open_menu(None, Some(focus));
                if let Some(menu) = self.menu.as_mut() {
                    menu.mode = MenuMode::Confirm(0);
                }
            }
        }
        true
    }

    /// Строки, к которым относится действие: отмеченные, иначе выбранная.
    fn targets(&self) -> Vec<usize> {
        if self.picked.is_empty() {
            if self.no_cursor {
                return Vec::new();
            }
            self.rows.get(self.selected).filter(|row| !row.is_volume()).map(|_| self.selected).into_iter().collect()
        } else {
            self.picked.iter().copied().filter(|index| *index < self.rows.len()).collect()
        }
    }

    /// Вырезать или копировать выделенное — в буфер «Файлов» и путями в
    /// общий буфер обмена стола (в Блокнот вставятся пути).
    fn to_clip(&mut self, cut: bool) -> bool {
        if self.on_computer() {
            return false;
        }
        let paths: Vec<String> = self.targets().into_iter().map(|index| join(&self.path, &self.rows[index].name)).collect();
        if paths.is_empty() {
            return false;
        }
        // Общий буфер обмена системы (24.11): списком файлов — его видят
        // другие окна «Файлов», стол и программы Wine (`text/uri-list`);
        // текстом — пути (в Блокнот вставятся они).
        if !user_progs::clipboard_set_files(&paths, cut) {
            self.note = Some(String::from("Не удалось положить в буфер обмена"));
            return true;
        }
        println(&format!("files: {} {} item(s)", if cut { "cut" } else { "copied" }, paths.len()));
        self.note = Some(format!("{}: объектов {}", if cut { "Вырезано" } else { "Скопировано" }, paths.len()));
        true
    }

    /// Вставить файлы из буфера обмена системы в открытую папку.
    fn paste(&mut self) -> bool {
        if self.on_computer() {
            return false;
        }
        let dir = self.path.clone();
        if self.job.is_some() {
            return false;
        }
        let Some((paths, cut)) = user_progs::clipboard_get_files() else {
            self.note = Some(String::from("В буфере обмена нет файлов: сначала «Копировать» или «Вырезать»"));
            return true;
        };
        println(&format!("files: pasting {} item(s) from the clipboard{}", paths.len(), if cut { " (cut)" } else { "" }));
        self.clip = Some((paths, cut));
        let done = self.paste_into(dir);
        self.clip = None;
        // Вырезанное перенесено — в буфере его больше нет (как в Windows).
        if cut {
            let _ = clipboard_set("");
        }
        done
    }

    /// Вставить буфер «Файлов» в папку `dir`.
    fn paste_into(&mut self, dir: String) -> bool {
        if self.job.is_some() {
            return false;
        }
        let Some((paths, cut)) = self.clip.clone() else {
            self.note = Some(String::from("Буфер пуст: сначала «Копировать» или «Вырезать»"));
            return true;
        };
        let mut ops = Vec::new();
        let mut remove_after = Vec::new();
        let mut moved = 0usize;
        for src in &paths {
            let name = src.rsplit('/').next().unwrap_or(src).to_string();
            if dir == *src || dir.starts_with(&format!("{src}/")) {
                self.note = Some(format!("Нельзя вставить «{name}» в саму себя"));
                return true;
            }
            // Перенос в ту же папку — ничего не делать; копия — «— копия».
            if cut && parent_of(src) == dir {
                continue;
            }
            let dst = unique_name(&dir, &name);
            // Перенос на том же томе — переименование, мгновенно.
            if cut && rename(src, &dst) >= 0 {
                moved += 1;
                continue;
            }
            expand(src, &dst, &mut ops, 0);
            if cut {
                remove_after.push(src.clone());
            }
        }
        if cut {
            self.clip = None;
        }
        if ops.is_empty() {
            println(&format!("files: pasted {moved} item(s) into {dir}"));
            self.note = Some(format!("Перенесено объектов: {moved}"));
            self.reload();
            return true;
        }
        let total = ops.iter().map(|op| if let Op::File { size, .. } = op { *size } else { 0 }).sum();
        let home = self.home.clone();
        let label = |path: &str| -> String { place_title(path, &home).rsplit('/').next().unwrap_or(path).to_string() };
        let from = paths.first().map(|path| label(&parent_of(path))).unwrap_or_default();
        let to = label(&dir);
        println(&format!("files: copying {} item(s), {total} byte(s) into {dir}", paths.len()));
        self.job = Some(CopyJob {
            ops,
            next: 0,
            files: None,
            total,
            done: 0,
            started: monotonic_ms(),
            paused: false,
            remove_after,
            items: paths.len(),
            from,
            to,
            dest: dir.clone(),
            current: String::new(),
            error: None,
        });
        true
    }

    /// Шаг копирования. `true` — перерисовать.
    fn copy_step(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else {
            return false;
        };
        if job.paused || job.error.is_some() {
            return false;
        }
        let mut budget = COPY_STEP;
        let mut buffer = alloc::vec![0u8; 64 * 1024];
        while budget > 0 {
            let Some(op) = job.ops.get(job.next) else {
                break;
            };
            match op {
                Op::Dir(dst) => {
                    let code = mkdir(dst, 0o755);
                    if code < 0 && code != user_abi::ERR_EXISTS {
                        job.error = Some(format!("Не создать папку {dst}: {}", error_text(code)));
                        return true;
                    }
                    job.next += 1;
                }
                Op::File { src, dst, .. } => {
                    let (from, to) = match job.files {
                        Some(pair) => pair,
                        None => {
                            job.current = src.rsplit('/').next().unwrap_or(src).to_string();
                            let from = open(src);
                            if from < 0 {
                                job.error = Some(format!("Не открыть {src}: {}", error_text(from)));
                                return true;
                            }
                            let to = open_write(dst, true, true);
                            if to < 0 {
                                close(from);
                                job.error = Some(format!("Не записать {dst}: {}", error_text(to)));
                                return true;
                            }
                            job.files = Some((from, to));
                            (from, to)
                        }
                    };
                    let got = read(from, &mut buffer[..budget.min(64 * 1024)]);
                    if got < 0 {
                        job.error = Some(format!("Ошибка чтения {}: {}", job.current, error_text(got)));
                        return true;
                    }
                    if got == 0 {
                        close(from);
                        close(to);
                        job.files = None;
                        job.next += 1;
                        continue;
                    }
                    let mut written = 0usize;
                    while written < got as usize {
                        let step = write(to, &buffer[written..got as usize]);
                        if step <= 0 {
                            job.error = Some(format!("Ошибка записи {}: {}", job.current, error_text(step)));
                            return true;
                        }
                        written += step as usize;
                    }
                    job.done += got as u64;
                    budget = budget.saturating_sub(got as usize);
                }
            }
        }
        if job.next >= job.ops.len() {
            let remove_after = core::mem::take(&mut job.remove_after);
            let items = job.items;
            let dir = job.dest.clone();
            for src in &remove_after {
                remove_tree(src, 0);
            }
            self.job = None;
            println(&format!("files: pasted {items} item(s) into {dir}"));
            self.note = Some(format!("Готово: объектов {items}"));
            self.reload();
        }
        true
    }

    /// Остановить копирование: открытое закрыть, недописанный файл стереть.
    fn cancel_copy(&mut self) {
        if let Some(job) = self.job.take() {
            if let Some((from, to)) = job.files {
                close(from);
                close(to);
                if let Some(Op::File { dst, .. }) = job.ops.get(job.next) {
                    remove(dst);
                }
            }
            println("files: copying cancelled");
            self.note = Some(String::from("Копирование отменено"));
            self.reload();
        }
    }

    /// Пункты выпавшего меню панели команд.
    fn pop_entries(&self, command: Command) -> Vec<(String, u8)> {
        let mark = |on: bool, text: &str| format!("{}{text}", if on { "● " } else { "   " });
        let check = |on: bool, text: &str| format!("{}{text}", if on { "✓ " } else { "   " });
        match command {
            Command::Sort => {
                let mut out: Vec<(String, u8)> = SortKey::ALL.iter().map(|key| (mark(*key == self.sort, key.title()), 0)).collect();
                out.push((mark(!self.descending, "По возрастанию"), 1));
                out.push((mark(self.descending, "По убыванию"), 1));
                out
            }
            _ => {
                let mut out: Vec<(String, u8)> = ViewMode::ALL.iter().map(|mode| (mark(*mode == self.view, mode.title()), 0)).collect();
                out.push((check(!self.friendly, "Показывать системные файлы"), 1));
                out.push((check(self.hidden, "Показывать скрытые файлы"), 1));
                out.push((String::from("   Параметры папок…"), 2));
                out
            }
        }
    }

    fn run_pop(&mut self, command: Command, index: usize) {
        self.pop = None;
        match (command, index) {
            (Command::Sort, 0..=3) => self.set_sort(SortKey::ALL[index], self.descending),
            (Command::Sort, 4) => self.set_sort(self.sort, false),
            (Command::Sort, _) => self.set_sort(self.sort, true),
            (_, 0..=2) => {
                self.set_view(ViewMode::ALL[index]);
            }
            (_, 3) => {
                self.friendly = !self.friendly;
                println(&format!("files: system files {}", if self.friendly { "hidden" } else { "shown" }));
                self.reload();
            }
            (_, 4) => {
                // С 24.11 — настройка «Параметров папок» (хранится и у стола).
                self.hidden = !self.hidden;
                println(&format!("files: hidden files {}", if self.hidden { "shown" } else { "hidden" }));
                Self::store_option(sysconf::store::FolderOptions::ROWS[0].0, if self.hidden { "1" } else { "0" });
                self.load_options();
                self.reload();
            }
            _ => self.open_options(),
        }
    }

    /// Клавиша в «Параметрах папок»: стрелки и Tab — по флажкам и кнопкам,
    /// пробел — флажок или кнопка, Enter — «ОК», Esc — «Отмена».
    fn key_options(&mut self, code: u32) -> bool {
        let rows = sysconf::store::FolderOptions::ROWS.len();
        let last = rows + 3;
        let Some(dialog) = self.options.as_mut() else { return false };
        match code {
            WIN_KEY_UP => dialog.focus = dialog.focus.saturating_sub(1),
            WIN_KEY_DOWN | 0x09 => dialog.focus = (dialog.focus + 1).min(last),
            0x20 => {
                let focus = dialog.focus;
                return self.press_options(focus);
            }
            ENTER => {
                let focus = dialog.focus;
                return self.press_options(if focus >= rows { focus } else { rows + 1 });
            }
            ESCAPE => {
                self.options = None;
                println("files: folder options cancelled");
            }
            _ => {}
        }
        true
    }

    /// Флажок `index` или кнопка (за флажками: «Восстановить значения по
    /// умолчанию», «ОК», «Отмена», «Применить»).
    fn press_options(&mut self, index: usize) -> bool {
        let rows = sysconf::store::FolderOptions::ROWS.len();
        let Some(dialog) = self.options.as_mut() else { return false };
        dialog.focus = index;
        match index.checked_sub(rows) {
            None => {
                let on = !dialog.draft.get(index);
                dialog.draft.set(index, on);
                println(&format!("files: folder option {} {}", sysconf::store::FolderOptions::ROWS[index].0, if on { "on" } else { "off" }));
            }
            Some(0) => {
                dialog.draft = sysconf::store::FolderOptions::DEFAULT;
                println("files: folder options back to the defaults");
            }
            Some(1) => {
                self.apply_options();
                self.options = None;
            }
            Some(2) => {
                self.options = None;
                println("files: folder options cancelled");
            }
            _ => self.apply_options(),
        }
        true
    }

    fn set_sort(&mut self, key: SortKey, descending: bool) {
        let keep = self.rows.get(self.selected).map(|row| row.name.clone());
        self.sort = key;
        self.descending = descending;
        self.reload();
        if let Some(name) = keep {
            self.selected = self.rows.iter().position(|row| row.name == name).unwrap_or(0);
        }
        println(&format!("files: sorted by {} {}", key.tag(), if descending { "desc" } else { "asc" }));
    }

    /// Обработать клавишу. `true` — картинку надо перерисовать.
    fn on_key(&mut self, code: u32) -> bool {
        if self.menu.is_some() {
            return self.key_menu(code);
        }
        if self.searching && self.preview.is_none() && self.props.is_none() {
            return self.key_search(code);
        }
        if self.preview.is_some() {
            return self.key_preview(code);
        }
        if self.props.is_some() {
            if matches!(code, ESCAPE | BACKSPACE | ENTER) {
                self.props = None;
                return true;
            }
            // Вкладки — Tab и стрелками.
            if let Some(props) = self.props.as_mut().filter(|props| props.mode.is_some()) {
                if matches!(code, 0x09 | WIN_KEY_LEFT | WIN_KEY_RIGHT) {
                    props.tab = 1 - props.tab.min(1);
                    return true;
                }
            }
            return false;
        }
        match code {
            // Клавиша меню — то же, что правая кнопка по выбранной строке. `M`
            // — для клавиатур без неё: F-ряд до программ не доходит, и Shift+F10
            // здесь не сделать.
            WIN_KEY_MENU | MENU => {
                self.open_menu_for_selection();
                true
            }
            WIN_KEY_DELETE => {
                if let Some(focus) = self.focus().filter(|index| self.rows.get(*index).is_some_and(|row| !row.is_volume())) {
                    self.open_menu(None, Some(focus));
                    if let Some(menu) = self.menu.as_mut() {
                        menu.mode = MenuMode::Confirm(0);
                    }
                    return true;
                }
                false
            }
            // `T` — плитка или таблица, `L` — «Список» или таблица.
            TILES => self.set_view(if self.view == ViewMode::Tiles { ViewMode::Table } else { ViewMode::Tiles }),
            LIST => self.set_view(if self.view == ViewMode::List { ViewMode::Table } else { ViewMode::List }),
            // В «Списке» строки идут столбцами (21.11): стрелки вбок — в
            // соседний столбец, как в Проводнике.
            WIN_KEY_LEFT | WIN_KEY_RIGHT if self.view == ViewMode::List && !self.rows.is_empty() && !self.on_computer() => {
                let per = self.list_rows.get().max(1);
                let last = self.rows.len() - 1;
                if code == WIN_KEY_LEFT {
                    self.selected = self.selected.saturating_sub(per);
                } else if self.selected / per < last / per {
                    self.selected = (self.selected + per).min(last);
                }
                self.scroll.follow(self.selected);
                true
            }
            ENTER if self.focus().is_none() => false,
            // Клавиши двигают выделение, а вид едет за ним (20.11); колесо —
            // наоборот, см. `wheel_at`.
            WIN_KEY_UP => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll.follow(self.selected);
                true
            }
            WIN_KEY_DOWN => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                self.scroll.follow(self.selected);
                true
            }
            WIN_KEY_PAGE_UP => {
                self.selected = self.selected.saturating_sub(self.page_rows.get().max(1));
                self.scroll.follow(self.selected);
                true
            }
            WIN_KEY_PAGE_DOWN => {
                self.selected = (self.selected + self.page_rows.get().max(1)).min(self.rows.len().saturating_sub(1));
                self.scroll.follow(self.selected);
                true
            }
            WIN_KEY_HOME => {
                self.selected = 0;
                self.scroll.follow(self.selected);
                true
            }
            WIN_KEY_END => {
                self.selected = self.rows.len().saturating_sub(1);
                self.scroll.follow(self.selected);
                true
            }
            // Enter приезжает символом: у него он есть, и договор отдаёт
            // символ раньше имени.
            ENTER | WIN_KEY_RIGHT => self.open_selected(),
            BACKSPACE => self.go_up(),
            // Влево — «назад», как у стрелки на панели: подниматься наверх
            // умеет Backspace, и две клавиши на одно действие ничего не дают.
            WIN_KEY_LEFT => self.go_back() || self.go_up(),
            // Обновить. В ядре этого пункта не было и не требовалось: меню
            // стола само звало `refresh_files` у открытого окна. Через границу
            // привилегий такого вызова нет, и чинить это извещением о смене
            // каталога — работа не этой фазы. Поэтому клавиша, и она названа в
            // строке состояния, а не оставлена на угадывание.
            REFRESH => {
                self.reload();
                true
            }
            // Переключить вид. Выделение сбрасывается вместе с перечитыванием
            // списка — иначе номер строки указывал бы на другую запись: в
            // обычном виде каталогов больше.
            VIEW => {
                self.friendly = !self.friendly;
                self.reload();
                println(&format!(
                    "files: view {}",
                    if self.friendly { "friendly" } else { "plain" }
                ));
                true
            }
            _ => false,
        }
    }

    /// Как называется эта запись на экране.
    ///
    /// Подпись есть у немногих каталогов и только на верхних уровнях — таблица в
    /// `sysconf::winpath`. У остальных возвращается настоящее имя: сочинять
    /// перевод для `/usr/lib` значило бы показывать человеку слово, которого он
    /// нигде больше не увидит.
    fn shown_name<'a>(&self, row: &'a Row) -> &'a str {
        if self.friendly {
            if let Some(label) = sysconf::winpath::label(&join(&self.path, &row.name)) {
                return label;
            }
        }
        // «Скрывать расширения для известных типов файлов» (24.11).
        sysconf::store::shown_name(&row.name, row.directory, self.opts.hide_known_ext)
    }

    fn key_preview(&mut self, code: u32) -> bool {
        let Some(preview) = self.preview.as_mut() else {
            return false;
        };
        let page = PREVIEW_PAGE;
        match code {
            ESCAPE | BACKSPACE | WIN_KEY_LEFT => {
                self.preview = None;
                true
            }
            WIN_KEY_DOWN => {
                if preview.scroll + 1 < preview.lines.len() {
                    preview.scroll += 1;
                }
                true
            }
            WIN_KEY_UP => {
                preview.scroll = preview.scroll.saturating_sub(1);
                true
            }
            WIN_KEY_PAGE_DOWN => {
                preview.scroll = (preview.scroll + page).min(preview.lines.len().saturating_sub(1));
                true
            }
            WIN_KEY_PAGE_UP => {
                preview.scroll = preview.scroll.saturating_sub(page);
                true
            }
            WIN_KEY_HOME => {
                preview.scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// Войти в каталог, открыть том, запустить .NET-программу или открыть файл
    /// на просмотр.
    fn open_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else {
            return false;
        };
        let target = if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) };
        if row.directory {
            // «Открывать каждую папку в отдельном окне» (24.11).
            if self.opts.new_window && !row.is_volume() && !self.in_trash() {
                return self.open_window(&target);
            }
            self.go_to(target);
            return true;
        }
        let name = row.name.clone();
        // Фаза N8: .NET-программу открыть — значит запустить, а не показать её
        // байты. Ждать её «Файлы» не станут: у программы своё окно.
        if let Some(line) = dotnet_command(&target) {
            let id = spawn(&line);
            if id < 0 {
                println(&format!("files: cannot start '{line}': error {id}"));
                self.note = Some(format!("Не удалось запустить «{name}»: {}", error_text(id)));
            } else {
                println(&format!("files: started '{line}' as #{id}"));
                self.note = Some(format!("Запущено «{name}»"));
            }
            return true;
        }
        // P12 (18.11): программа Windows — запустить её, а не показать байты.
        if let Some(line) = windows_command(&target) {
            let id = spawn(&line);
            if id < 0 {
                println(&format!("files: cannot start '{line}': error {id}"));
                self.note = Some(format!("Не удалось запустить «{name}»: {}", error_text(id)));
            } else {
                println(&format!("files: started '{line}' as #{id}"));
                self.note = Some(format!("Запущено «{name}» (Windows)"));
            }
            return true;
        }
        // Выбор человека («Всегда открывать этой программой») — первым.
        if let Some(command) = chosen_program(&name) {
            let line = with_line(&command, &target);
            let id = spawn(&line);
            if id >= 0 {
                println(&format!("files: started '{line}' as #{id}"));
                self.note = Some(format!("Открыто «{name}»"));
                return true;
            }
            println(&format!("files: cannot start '{line}': error {id}"));
        }
        // Фаза 70b: текст открывается в блокноте — его там можно и поправить.
        // Всё прочее — на просмотр, как раньше.
        if is_text(&name) {
            let line = format!("/bin/notepad {target}");
            let id = spawn(&line);
            if id >= 0 {
                println(&format!("files: started '{line}' as #{id}"));
                self.note = Some(format!("«{name}» открыт в блокноте"));
                return true;
            }
            println(&format!("files: cannot start '{line}': error {id}"));
        }
        self.preview = Some(read_preview(&name, &target));
        true
    }

    /// Путь выбранной записи.
    fn selected_path(&self) -> Option<String> {
        let row = self.rows.get(self.selected)?;
        Some(if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) })
    }

    // -----------------------------------------------------------------------
    // Контекстное меню
    // -----------------------------------------------------------------------

    /// Открыть меню в точке; `target` — строка, к которой оно относится.
    fn open_menu(&mut self, at: Option<(i32, i32)>, target: Option<usize>) {
        let items: &[MenuItem] = match target.and_then(|index| self.rows.get(index)) {
            Some(row) if row.is_volume() => &MenuItem::ON_VOLUME,
            Some(_) if self.in_trash() => &MenuItem::ON_TRASHED,
            Some(row) if row.directory => &MenuItem::ON_DIR,
            Some(_) => &MenuItem::ON_ENTRY,
            None if self.in_trash() => &MenuItem::ON_TRASH,
            None if self.on_computer() => &MenuItem::ON_VOLUME[1..],
            None => &MenuItem::ON_FOLDER,
        };
        let (x, y) = at.unwrap_or((0, 0));
        let mut items = items.to_vec();
        // Закреплённая папка — «Открепить» (24.11).
        if let Some(path) = target.and_then(|index| self.rows.get(index)).filter(|row| row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name)) {
            if self.pinned.contains(&path) {
                for item in &mut items {
                    if *item == MenuItem::Pin {
                        *item = MenuItem::Unpin;
                    }
                }
            }
        }
        // 25.11: verbs of the settings store (`shell` hive) — before
        // «Свойства»; extended ones only with Shift held at the right click.
        self.add_store_verbs(&mut items, at.is_some(), target);
        // «7-Zip ▸» (25.11): in the upper block, under the open items, where
        // the 7-Zip shell extension stands in Windows.
        self.zip_menu.clear();
        if target.and_then(|index| self.rows.get(index)).is_some_and(|row| !row.is_volume() && !self.in_trash()) && exists("/bin/archiver") && exists("/bin/7zz") {
            let at = items.iter().position(|item| *item == MenuItem::Rename).unwrap_or(items.len());
            items.insert(at, MenuItem::SevenZip);
        }
        self.menu = Some(Menu {
            x,
            y,
            items,
            selected: 0,
            mode: MenuMode::Items,
            target,
            place: None,
        });
        self.note = None;
        let what = match target.and_then(|index| self.rows.get(index)) {
            Some(row) => format!("'{}'", row.name),
            None => String::from("the folder"),
        };
        println(&format!("files: menu opened for {what}"));
    }

    /// Verbs of the settings store for the row `target` (a file or a
    /// folder; none on volumes and in the recycle bin) into `items`, before
    /// «Свойства». `by_pointer` — opened by a right click (its Shift counts).
    fn add_store_verbs(&mut self, items: &mut Vec<MenuItem>, by_pointer: bool, target: Option<usize>) {
        verbs().clear();
        let Some(row) = target.and_then(|index| self.rows.get(index)).filter(|row| !row.is_volume() && !self.in_trash()) else {
            return;
        };
        let path = self.row_path(row);
        let types = sysconf::store::verb_types(&row.name, row.directory);
        let refs: Vec<&str> = types.iter().map(String::as_str).collect();
        let shift = by_pointer && app::right_click_mods() & WIN_MOD_SHIFT != 0;
        let found = sysconf::store::verbs(&user_progs::reg_list("shell/"), &refs, shift);
        println(&format!(
            "files: menu verbs{} [{}]",
            if shift { " (shift)" } else { "" },
            found.iter().map(|verb| verb.id.as_str()).collect::<Vec<_>>().join(", ")
        ));
        let at = items.iter().position(|item| *item == MenuItem::Properties).unwrap_or(items.len());
        for (index, verb) in found.into_iter().enumerate() {
            let caption = intern_caption(&verb.caption);
            verbs().push((verb, caption, path.clone()));
            items.insert(at + index, MenuItem::Verb(index as u8));
        }
    }

    /// Run verb `index` of the open menu (25.11).
    fn run_verb(&mut self, index: u8) {
        let Some((verb, _, path)) = verbs().get(usize::from(index)).cloned() else {
            self.close_menu();
            return;
        };
        println(&format!("files: verb '{}' on '{path}'", verb.id));
        match verb.command.as_str() {
            sysconf::store::VERB_LOCATION => {
                self.close_menu();
                // A new window on the folder with the item selected.
                self.open_window(&path);
            }
            sysconf::store::VERB_COPY_AS_PATH => {
                self.close_menu();
                let quoted = format!("\"{path}\"");
                if clipboard_set(&quoted) {
                    println(&format!("files: copied as path {quoted}"));
                    self.note = Some(format!("Скопировано: {quoted}"));
                }
            }
            sysconf::store::VERB_EMPTY_FOLDER => {
                let count = list_dir(&path).map_or(0, |(rows, _)| rows.len());
                if count == 0 {
                    self.close_menu();
                    self.note = Some(String::from("Папка уже пуста"));
                    return;
                }
                self.emptying = Some((path, count));
                if let Some(menu) = self.menu.as_mut() {
                    menu.mode = MenuMode::Confirm(0);
                }
            }
            command if command.starts_with('@') => self.close_menu(),
            _ => {
                self.close_menu();
                let line = verb.command_for(&path);
                let id = spawn(&line);
                if id < 0 {
                    println(&format!("files: cannot start '{line}': error {id}"));
                    self.note = Some(format!("Не удалось запустить: {}", error_text(id)));
                }
            }
        }
    }

    /// «Удалить содержимое папки» confirmed (25.11): everything inside to the
    /// recycle bin.
    fn empty_folder(&mut self, dir: &str) {
        let names: Vec<String> = list_dir(dir).map(|(rows, _)| rows.into_iter().map(|row| row.name).collect()).unwrap_or_default();
        let (mut moved, mut failed) = (0usize, 0usize);
        for name in &names {
            match self.move_to_trash(&join(dir, name)) {
                Ok(_) => moved += 1,
                Err(code) => {
                    println(&format!("files: empty folder: '{name}' failed: {code}"));
                    failed += 1;
                }
            }
        }
        println(&format!("files: emptied '{dir}': {moved} to the trash, {failed} failed"));
        self.note = Some(if failed == 0 { format!("Содержимое перемещено в корзину: {moved}") } else { format!("В корзину: {moved}, не удалось: {failed}") });
        self.reload();
    }

    /// Открыть меню у выбранной строки — клавишей.
    fn open_menu_for_selection(&mut self) {
        let target = self.focus();
        // Место посчитает отрисовка: у клавиатуры точки нет, и меню встаёт у
        // строки, к которой относится.
        self.open_menu(None, target);
    }

    fn close_menu(&mut self) {
        self.zip_menu.clear();
        self.emptying = None;
        if self.menu.take().is_some() {
            println("files: menu closed");
        }
    }

    /// Клавиша, пока открыто меню.
    fn key_menu(&mut self, code: u32) -> bool {
        // «7-Zip ▸» takes the arrows while it is open (25.11).
        if let Some(done) = self.key_zip(code) {
            return done;
        }
        let Some(menu) = self.menu.as_mut() else {
            return false;
        };
        match &mut menu.mode {
            MenuMode::Items => match code {
                WIN_KEY_UP => {
                    menu.selected = menu.selected.saturating_sub(1);
                    true
                }
                WIN_KEY_DOWN => {
                    if menu.selected + 1 < menu.items.len() {
                        menu.selected += 1;
                    }
                    true
                }
                ENTER => {
                    let item = menu.items[menu.selected.min(menu.items.len() - 1)];
                    self.run_menu_item(item);
                    true
                }
                ESCAPE | WIN_KEY_MENU | MENU => {
                    self.close_menu();
                    true
                }
                _ => false,
            },
            MenuMode::Rename(..) => self.key_rename(code, 0, 0),
            MenuMode::OpenWith(progs, selected) => {
                let rows = openwith_rows(progs);
                match code {
                    WIN_KEY_UP => {
                        *selected = selected.saturating_sub(1);
                        true
                    }
                    WIN_KEY_DOWN => {
                        if *selected + 1 < rows {
                            *selected += 1;
                        }
                        true
                    }
                    ENTER => {
                        let row = *selected;
                        self.pick_open_with(row);
                        true
                    }
                    ESCAPE | WIN_KEY_MENU | MENU => {
                        self.close_menu();
                        true
                    }
                    _ => false,
                }
            }
            // Окно выбора — модальное: стрелки, пробел (флажок), Enter, Esc.
            MenuMode::Choose(chooser) => match code {
                WIN_KEY_UP => {
                    chooser.selected = chooser.selected.saturating_sub(1);
                    chooser.follow();
                    true
                }
                WIN_KEY_DOWN => {
                    if chooser.selected + 1 < chooser.progs.len() {
                        chooser.selected += 1;
                    }
                    chooser.follow();
                    true
                }
                0x20 => {
                    chooser.always = !chooser.always;
                    true
                }
                ENTER => {
                    self.finish_choose();
                    true
                }
                ESCAPE => {
                    self.close_menu();
                    true
                }
                _ => true,
            },
            // Как у окон-вопросов ядра: Enter — выбранная кнопка, Tab и
            // стрелки переводят выбор, Esc — отмена; `y`/`n` остались.
            MenuMode::Confirm(focus) => match code {
                0x09 | WIN_KEY_LEFT | WIN_KEY_RIGHT => {
                    *focus = 1 - (*focus).min(1);
                    true
                }
                ENTER if *focus == 1 => {
                    self.close_menu();
                    true
                }
                ENTER | YES => {
                    self.finish_delete();
                    true
                }
                ESCAPE | NO => {
                    self.close_menu();
                    true
                }
                // Вопрос модальный: прочие клавиши не уходят списку под ним.
                _ => true,
            },
        }
    }

    /// Клавиша в поле нового имени (24.11 — строка ввода набора: выделение,
    /// каретка, Shift+стрелки, Ctrl+A/C/X/V).
    fn key_rename(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        let Some(MenuMode::Rename(line, suffix)) = self.menu.as_mut().map(|menu| &mut menu.mode) else {
            return false;
        };
        match code {
            ENTER => {
                let name = format!("{}{suffix}", line.text);
                self.finish_rename(&name);
                true
            }
            ESCAPE => {
                self.close_menu();
                true
            }
            // Ctrl+U — очистить, как в редакторе строки оболочки.
            0x15 => {
                *line = Line::new("");
                true
            }
            // Косая — разделитель пути, а не знак имени.
            0x2F => true,
            _ => {
                line.key(code, mods, latin);
                if line.text.len() > 200 {
                    let mut cut = 200;
                    while !line.text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    line.text.truncate(cut);
                    *line = Line::new(&line.text.clone());
                }
                true
            }
        }
    }

    /// Выполнить пункт меню.
    fn run_menu_item(&mut self, item: MenuItem) {
        let target = self.menu.as_ref().and_then(|menu| menu.target);
        // Меню места колонки быстрого доступа (24.11).
        if let Some(place) = self.menu.as_ref().and_then(|menu| menu.place.clone()) {
            self.close_menu();
            match item {
                MenuItem::Open => self.go_to(place),
                MenuItem::OpenWindow => {
                    self.open_window(&place);
                }
                MenuItem::Pin => self.set_pinned(&place, true),
                MenuItem::Unpin => self.set_pinned(&place, false),
                MenuItem::Properties => self.open_properties(&[place]),
                _ => {}
            }
            return;
        }
        match item {
            MenuItem::Pin | MenuItem::Unpin => {
                self.close_menu();
                let path = target.and_then(|index| self.rows.get(index)).filter(|row| row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name));
                if let Some(path) = path {
                    self.set_pinned(&path, item == MenuItem::Pin);
                }
            }
            MenuItem::Open => {
                self.close_menu();
                if let Some(index) = target {
                    self.selected = index;
                    self.open_selected();
                }
            }
            MenuItem::OpenWith => {
                let path = target.and_then(|index| self.rows.get(index)).filter(|row| !row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name));
                let Some(path) = path else {
                    self.close_menu();
                    return;
                };
                let progs = load_progs(&path);
                println(&format!(
                    "files: open with for '{path}': {} program(s), {} fit: {}",
                    progs.len(),
                    progs.iter().filter(|prog| prog.fits()).count(),
                    progs.iter().filter(|prog| prog.fits()).map(|prog| prog.caption.as_str()).collect::<Vec<_>>().join(", ")
                ));
                if let Some(menu) = self.menu.as_mut() {
                    // Ничего не подходит — сразу окно выбора.
                    menu.mode = if progs.iter().any(Prog::fits) {
                        MenuMode::OpenWith(progs, 0)
                    } else {
                        MenuMode::Choose(Chooser { progs, selected: 0, first: 0, always: false })
                    };
                }
            }
            MenuItem::OpenTab | MenuItem::OpenWindow => {
                self.close_menu();
                let path = target.and_then(|index| self.rows.get(index)).filter(|row| row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name));
                if let Some(path) = path {
                    if item == MenuItem::OpenTab {
                        self.open_tab(path, false);
                    } else {
                        self.open_window(&path);
                    }
                }
            }
            MenuItem::Rename => match target {
                Some(index) => self.start_rename(index),
                None => self.close_menu(),
            },
            MenuItem::Delete => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.mode = MenuMode::Confirm(0);
                }
            }
            MenuItem::CopyPath => {
                self.close_menu();
                if let Some(row) = target.and_then(|index| self.rows.get(index)) {
                    let path = if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) };
                    if clipboard_set(&path) {
                        println(&format!("files: path copied '{path}'"));
                        self.note = Some(format!("Путь скопирован: {path}"));
                    }
                }
            }
            MenuItem::Properties => {
                self.close_menu();
                self.properties_of_selection(target);
            }
            MenuItem::NewFolder => {
                self.close_menu();
                self.create_new(true);
            }
            MenuItem::NewTextFile => {
                self.close_menu();
                self.create_new(false);
            }
            MenuItem::Refresh => {
                self.close_menu();
                self.reload();
            }
            MenuItem::Paste => {
                self.close_menu();
                self.paste();
            }
            MenuItem::Restore => {
                self.close_menu();
                if let Some(index) = target {
                    self.restore(index);
                }
            }
            MenuItem::EmptyTrash => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.target = None;
                    menu.mode = MenuMode::Confirm(0);
                }
            }
            // Enter or a click on «7-Zip» opens its submenu (25.11).
            MenuItem::SevenZip => {
                let _ = target;
                self.open_zip(0, true);
            }
            MenuItem::Verb(index) => self.run_verb(index),
        }
    }

    // -----------------------------------------------------------------------
    // «7-Zip ▸» (25.11)
    // -----------------------------------------------------------------------

    /// The rows the 7-Zip items act on: the picked ones (when the menu row is
    /// among them), else the menu row; and whether it is one folder.
    fn zip_paths(&self) -> (Vec<String>, bool) {
        let target = self.menu.as_ref().and_then(|menu| menu.target);
        let rows: Vec<&Row> = match target {
            Some(index) if self.picked.len() > 1 && self.picked.contains(&index) => self.picked.iter().filter_map(|index| self.rows.get(*index)).collect(),
            Some(index) => self.rows.get(index).into_iter().collect(),
            None => Vec::new(),
        };
        let single_dir = rows.len() == 1 && rows[0].directory;
        (rows.iter().map(|row| self.row_path(row)).collect(), single_dir)
    }

    /// Rows and words of level `level` of «7-Zip ▸» (0 — the submenu; 1 — the
    /// submenu of its row `parent`).
    fn zip_level(&self, level: usize, parent: usize) -> Option<ZipLevel> {
        let (paths, single_dir) = self.zip_paths();
        if paths.is_empty() {
            return None;
        }
        let items: Vec<ZipItem> = if level == 0 {
            let archive = !single_dir && paths.iter().all(|path| sysconf::filetypes::is_archive(path));
            if archive {
                alloc::vec![
                    ZipItem::Open,
                    ZipItem::OpenAsMenu,
                    ZipItem::Extract,
                    ZipItem::ExtractHere,
                    ZipItem::ExtractTo,
                    ZipItem::Test,
                    ZipItem::Add,
                    ZipItem::Add7z,
                    ZipItem::AddZip,
                    ZipItem::HashMenu,
                ]
            } else {
                alloc::vec![ZipItem::Add, ZipItem::Add7z, ZipItem::AddZip, ZipItem::HashMenu]
            }
        } else {
            match self.zip_menu.first().and_then(|first| first.items.get(parent)) {
                Some(ZipItem::OpenAsMenu) => (0..ZIP_TYPES.len()).map(|index| ZipItem::OpenAs(index as u8)).collect(),
                Some(ZipItem::HashMenu) => (0..ZIP_HASHES.len()).map(|index| ZipItem::Hash(index as u8)).collect(),
                _ => return None,
            }
        };
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let named = |ext: &str| {
            let target = sysconf::filetypes::add_to_target(&refs, single_dir, ext);
            target.rsplit('/').next().unwrap_or("").to_string()
        };
        let titles = items
            .iter()
            .map(|item| match item {
                ZipItem::Open | ZipItem::OpenAsMenu => String::from("Открыть архив"),
                ZipItem::OpenAs(index) => String::from(ZIP_TYPES[usize::from(*index)]),
                ZipItem::Extract => String::from("Распаковать"),
                ZipItem::ExtractHere => String::from("Распаковать здесь"),
                ZipItem::ExtractTo => format!("Распаковать в \"{}\\\"", sysconf::filetypes::archive_stem(paths[0].rsplit('/').next().unwrap_or(""))),
                ZipItem::Test => String::from("Тестировать"),
                ZipItem::Add => String::from("Добавить к архиву…"),
                ZipItem::Add7z => format!("Добавить к \"{}\"", named("7z")),
                ZipItem::AddZip => format!("Добавить к \"{}\"", named("zip")),
                ZipItem::Hash(index) => String::from(ZIP_HASHES[usize::from(*index)].0),
                ZipItem::HashMenu => String::from("CRC SHA"),
            })
            .collect();
        Some(ZipLevel { items, titles, selected: usize::MAX, parent, focus: false })
    }

    /// Open level `level` (0 — «7-Zip ▸» itself, from the menu row; 1 — the
    /// submenu of the lit row of level 0). `keys` — opened with the keyboard:
    /// the arrows go into it, its first row lit.
    fn open_zip(&mut self, level: usize, keys: bool) {
        let parent = if level == 0 {
            match self.menu.as_ref().and_then(|menu| menu.items.iter().position(|item| *item == MenuItem::SevenZip)) {
                Some(at) => at,
                None => return,
            }
        } else {
            match self.zip_menu.get(level - 1) {
                Some(up) if up.items.get(up.selected).is_some_and(|item| item.submenu()) => up.selected,
                _ => return,
            }
        };
        if self.zip_menu.get(level).is_some_and(|open| open.parent == parent) {
            if keys {
                self.zip_menu.truncate(level + 1);
                for (at, open) in self.zip_menu.iter_mut().enumerate() {
                    open.focus = at == level;
                }
                if let Some(open) = self.zip_menu.get_mut(level) {
                    open.selected = 0;
                }
            }
            return;
        }
        let Some(mut opened) = self.zip_level(level, parent) else { return };
        if keys {
            opened.selected = 0;
            opened.focus = true;
            for open in &mut self.zip_menu {
                open.focus = false;
            }
        }
        self.zip_menu.truncate(level);
        if level == 0 {
            if let Some(menu) = self.menu.as_mut() {
                menu.selected = parent;
            }
        }
        println(&format!(
            "files: 7-Zip submenu {} opened: {}",
            level + 1,
            opened.titles.iter().map(String::as_str).collect::<Vec<_>>().join(" | ")
        ));
        self.zip_menu.push(opened);
    }

    /// Cards of the open levels, in order.
    fn zip_rects(&self, ctx: Ctx, area: Rect) -> Vec<Rect> {
        let mut out = Vec::new();
        let (Some(menu), Some(card)) = (self.menu.as_ref(), self.menu_rect(ctx, area)) else { return out };
        if !matches!(menu.mode, MenuMode::Items) {
            return out;
        }
        let mut parent_rect = card;
        let mut parent_entries = zip_entries(&menu.items);
        for open in &self.zip_menu {
            let entries = open.entries();
            let rect = kit::submenu_rect(ctx, parent_rect, &parent_entries, open.parent, &entries, area);
            out.push(rect);
            parent_rect = rect;
            parent_entries = entries.iter().map(|entry| Entry { title: "", submenu: entry.submenu, group: entry.group, ..Entry::NONE }).collect();
        }
        out
    }

    fn draw_zip(&self, ctx: Ctx, s: &mut Surface, area: Rect) {
        for (open, rect) in self.zip_menu.iter().zip(self.zip_rects(ctx, area)) {
            // The row whose submenu is open stays lit.
            let lit = if open.selected < open.items.len() { open.selected } else { usize::MAX };
            kit::draw_menu(ctx, s, rect, &open.entries(), lit);
        }
    }

    /// Pointer over the menu (25.11): «7-Zip» opens its submenu, a row with a
    /// submenu opens it, other rows close the deeper levels. `true` — handled
    /// (redraw).
    fn hover_zip(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) -> bool {
        let Some(card) = self.menu_rect(ctx, area) else { return false };
        if !self.menu.as_ref().is_some_and(|menu| matches!(menu.mode, MenuMode::Items)) {
            return false;
        }
        let rects = self.zip_rects(ctx, area);
        for level in (0..rects.len()).rev() {
            if !rects[level].contains(x, y) {
                continue;
            }
            let row = kit::menu_hit(ctx, rects[level], &self.zip_menu[level].entries(), x, y);
            let open = &mut self.zip_menu[level];
            let changed = row.is_some_and(|row| row != open.selected);
            if let Some(row) = row {
                open.selected = row;
                if open.items[row].submenu() {
                    self.open_zip(level + 1, false);
                } else {
                    self.zip_menu.truncate(level + 1);
                }
            }
            return changed || row.is_some();
        }
        let Some(menu) = self.menu.as_ref() else { return false };
        match kit::menu_hit(ctx, card, &zip_entries(&menu.items), x, y) {
            Some(row) if menu.items[row] == MenuItem::SevenZip => {
                let was = self.zip_menu.len();
                self.open_zip(0, false);
                was != self.zip_menu.len()
            }
            Some(_) if !self.zip_menu.is_empty() => {
                self.zip_menu.clear();
                false
            }
            _ => false,
        }
    }

    /// Click (25.11): a row of «7-Zip ▸» runs (or opens its submenu); a
    /// click on «7-Zip» opens it. `None` — not here.
    fn click_zip(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) -> Option<bool> {
        let rects = self.zip_rects(ctx, area);
        for level in (0..rects.len()).rev() {
            if !rects[level].contains(x, y) {
                continue;
            }
            if let Some(row) = kit::menu_hit(ctx, rects[level], &self.zip_menu[level].entries(), x, y) {
                self.zip_menu[level].selected = row;
                let item = self.zip_menu[level].items[row];
                if item.submenu() {
                    self.open_zip(level + 1, false);
                } else {
                    self.run_zip(item);
                }
            }
            return Some(true);
        }
        None
    }

    /// Keys while the menu is open (25.11): → on «7-Zip» or on a row with a
    /// submenu enters it, ← and Esc leave, ↑↓ move, Enter runs. `None` — the
    /// key is for the card.
    fn key_zip(&mut self, code: u32) -> Option<bool> {
        let menu = self.menu.as_ref()?;
        if !matches!(menu.mode, MenuMode::Items) {
            return None;
        }
        let Some(level) = self.zip_menu.iter().position(|open| open.focus) else {
            // On the card: → or Enter on «7-Zip» enters its submenu.
            if matches!(code, WIN_KEY_RIGHT | ENTER) && menu.items.get(menu.selected) == Some(&MenuItem::SevenZip) {
                self.open_zip(0, true);
                return Some(true);
            }
            return None;
        };
        let open = &mut self.zip_menu[level];
        let count = open.items.len().max(1);
        match code {
            WIN_KEY_UP => open.selected = if open.selected >= count { count - 1 } else { (open.selected + count - 1) % count },
            WIN_KEY_DOWN => open.selected = if open.selected >= count { 0 } else { (open.selected + 1) % count },
            WIN_KEY_RIGHT | ENTER if open.items.get(open.selected).is_some_and(|item| item.submenu()) => self.open_zip(level + 1, true),
            ENTER => {
                if let Some(item) = open.items.get(open.selected).copied() {
                    self.run_zip(item);
                }
            }
            WIN_KEY_LEFT | ESCAPE => {
                self.zip_menu.truncate(level);
                if let Some(up) = self.zip_menu.last_mut() {
                    up.focus = true;
                }
            }
            _ => return Some(false),
        }
        Some(true)
    }

    /// Run an item of «7-Zip ▸» (25.11): `/bin/archiver <command>` with the
    /// picked rows (an archive command — with the first); a long list goes
    /// as a file (the launch line is at most 255 bytes).
    fn run_zip(&mut self, item: ZipItem) {
        let (paths, _) = self.zip_paths();
        self.close_menu();
        let one = matches!(item, ZipItem::Open | ZipItem::OpenAs(_) | ZipItem::Extract);
        let paths: Vec<String> = if one { paths.into_iter().take(1).collect() } else { paths };
        if paths.is_empty() {
            return;
        }
        let command = item.command();
        let head = if command.is_empty() { String::from("/bin/archiver") } else { format!("/bin/archiver {command}") };
        let mut line = head.clone();
        for path in &paths {
            line.push(' ');
            line.push_str(&pct(path));
        }
        if line.len() > 250 {
            let file = format!("/tmp/.files-7zip-{}", monotonic_ms());
            let _ = make_dirs("/tmp");
            if write_file(&file, paths.join("\n").as_bytes()).is_ok() {
                line = format!("{head} @{file}");
            }
        }
        let id = spawn(&line);
        println(&format!("files: 7-Zip {item:?} for {} item(s) ({id})", paths.len()));
        if id < 0 {
            self.note = Some(format!("Не удалось запустить архиватор: {}", error_text(id)));
        }
    }

    /// Начать переименование строки `index` (24.11, как F2 в Проводнике):
    /// поле с именем, выделено имя без расширения; скрытое расширение
    /// известного типа в поле не показывается и допишется само.
    fn start_rename(&mut self, index: usize) {
        let Some(row) = self.rows.get(index).filter(|row| !row.is_volume()) else {
            self.close_menu();
            return;
        };
        let (name, directory) = (row.name.clone(), row.directory);
        if self.menu.as_ref().map(|menu| menu.target) != Some(Some(index)) {
            self.open_menu(None, Some(index));
        }
        let shown = sysconf::store::shown_name(&name, directory, self.opts.hide_known_ext);
        let (text, suffix) = if shown.len() < name.len() { (shown.to_string(), name[shown.len()..].to_string()) } else { (name.clone(), String::new()) };
        let stem = if suffix.is_empty() { sysconf::store::stem_chars(&text, directory) } else { text.chars().count() };
        if let Some(menu) = self.menu.as_mut() {
            menu.mode = MenuMode::Rename(Line::with_selection(&text, stem), suffix);
        }
        println(&format!("files: renaming '{name}', {stem} character(s) selected"));
    }

    /// Путь строки: у тома — точка монтирования.
    fn row_path(&self, row: &Row) -> String {
        if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) }
    }

    /// «Свойства» (24.11): отмеченного, строки `target` или открытой папки —
    /// отдельным окном; у «Этого компьютера» — сведения о системе.
    fn properties_of_selection(&mut self, target: Option<usize>) {
        let paths: Vec<String> = match target {
            Some(index) if self.picked.len() > 1 && self.picked.contains(&index) => {
                self.picked.iter().filter_map(|index| self.rows.get(*index)).map(|row| self.row_path(row)).collect()
            }
            Some(index) => self.rows.get(index).map(|row| self.row_path(row)).into_iter().collect(),
            None if self.on_computer() => {
                self.show_folder_props();
                return;
            }
            None => alloc::vec![self.path.clone()],
        };
        self.open_properties(&paths);
    }

    /// Окно «Свойства» для `paths` — `files --properties` (24.11). Строка
    /// запуска не длиннее 255 байт: длинный список уходит файлом в `/tmp`.
    fn open_properties(&mut self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let mut line = String::from("/bin/files --properties");
        for path in paths {
            line.push(' ');
            line.push_str(&pct(path));
        }
        if line.len() > 250 {
            let file = format!("/tmp/.files-props-{}", monotonic_ms());
            let _ = make_dirs("/tmp");
            line = if write_file(&file, paths.join("\n").as_bytes()).is_ok() { format!("/bin/files --properties @{file}") } else { format!("/bin/files --properties {}", pct(&paths[0])) };
        }
        let id = spawn(&line);
        if id < 0 {
            println(&format!("files: cannot open properties: {id}"));
            self.note = Some(format!("Не удалось открыть свойства: {}", error_text(id)));
        } else {
            println(&format!("files: properties window for {} item(s)", paths.len()));
        }
    }

    /// Файл, для которого открыто меню: путь и имя.
    fn menu_target_file(&self) -> Option<(String, String)> {
        let index = self.menu.as_ref()?.target?;
        let row = self.rows.get(index)?;
        (!row.directory && !row.is_volume()).then(|| (join(&self.path, &row.name), row.name.clone()))
    }

    /// Строка `row` карточки «Открыть с помощью»: программа или (последняя)
    /// «Выбрать другую программу…».
    fn pick_open_with(&mut self, row: usize) {
        let Some(menu) = self.menu.as_mut() else { return };
        let MenuMode::OpenWith(progs, _) = &mut menu.mode else { return };
        let fits: Vec<usize> = (0..progs.len()).filter(|index| progs[*index].fits()).collect();
        match fits.get(row) {
            Some(&index) => {
                let (command, caption) = (progs[index].command.clone(), progs[index].caption.clone());
                self.launch_with(&command, &caption);
            }
            None => {
                let progs = core::mem::take(progs);
                println(&format!("files: choose another program ({} listed)", progs.len()));
                menu.mode = MenuMode::Choose(Chooser { progs, selected: 0, first: 0, always: false });
            }
        }
    }

    /// «Открыть» в окне выбора: запомнить, если отмечено «всегда», и открыть.
    fn finish_choose(&mut self) {
        let Some(MenuMode::Choose(chooser)) = self.menu.as_ref().map(|menu| &menu.mode) else { return };
        let Some(prog) = chooser.progs.get(chooser.selected) else {
            self.close_menu();
            return;
        };
        let (command, caption, always) = (prog.command.clone(), prog.caption.clone(), chooser.always);
        if always {
            if let Some((_, name)) = self.menu_target_file() {
                let ext = sysconf::filetypes::extension(&name);
                if !ext.is_empty() {
                    if user_progs::open_with_remember(&ext, &command) {
                        println(&format!("files: .{ext} files open with '{command}'"));
                    } else {
                        println(&format!("files: cannot remember the program for .{ext}"));
                    }
                }
            }
        }
        self.launch_with(&command, &caption);
    }

    /// Открыть файл меню программой `command` (22.11).
    fn launch_with(&mut self, command: &str, caption: &str) {
        let target = self.menu_target_file();
        self.close_menu();
        let Some((path, name)) = target else { return };
        let line = with_line(command, &path);
        let id = spawn(&line);
        if id < 0 {
            println(&format!("files: cannot start '{line}': error {id}"));
            self.note = Some(format!("Не удалось открыть «{name}» программой «{caption}»: {}", error_text(id)));
        } else {
            println(&format!("files: started '{line}' as #{id}"));
            self.note = Some(format!("«{name}» открыт программой «{caption}»"));
        }
    }

    /// Что спрашивает вопрос об удалении: очистка корзины, стирание из неё
    /// или перенос в неё (макет 18) — те же слова, что у окна ядра.
    fn ask(&self) -> Question {
        // «Удалить содержимое папки» (25.11).
        if let Some((dir, count)) = &self.emptying {
            return Question {
                title: format!("Удалить содержимое папки «{}»?", dir.rsplit('/').next().unwrap_or(dir)),
                text: "Всё, что лежит в папке, будет перемещено в корзину. Сама папка останется.",
                detail: format!("Объектов: {count}"),
                action: "Удалить",
                forever: false,
            };
        }
        let target = self.menu.as_ref().and_then(|menu| menu.target);
        let name = target.and_then(|index| self.rows.get(index)).map_or(String::new(), |row| row.name.clone());
        let many = self.picked.len() > 1;
        match (target, self.in_trash()) {
            (None, _) => Question {
                title: format!("Очистить корзину? Объектов: {}", self.rows.len()),
                text: "Объекты будут стёрты безвозвратно. Восстановить их из корзины будет нельзя.",
                detail: String::new(),
                action: "Удалить навсегда",
                forever: true,
            },
            (Some(_), true) => Question {
                title: if many { format!("Удалить навсегда объекты ({})?", self.picked.len()) } else { format!("Удалить «{name}» навсегда?") },
                text: "Объект будет стёрт безвозвратно. Восстановить его будет нельзя.",
                detail: String::new(),
                action: "Удалить навсегда",
                forever: true,
            },
            // Том без корзины (`/data`, флешка): `rename` в корзину между
            // томами не бывает, и удаление стирает целиком — так и спросить.
            (Some(_), false) if !self.trash_reachable() => Question {
                title: String::from("Удалить без возможности восстановления?"),
                text: if many { "На этом томе нет корзины: отмеченные объекты будут стёрты безвозвратно." } else { "На этом томе нет корзины: объект будет стёрт безвозвратно." },
                detail: if many { format!("Объектов: {}", self.picked.len()) } else { format!("«{name}»") },
                action: "Удалить навсегда",
                forever: true,
            },
            (Some(_), false) => Question {
                title: if many { format!("Удалить объекты ({})?", self.picked.len()) } else { format!("Удалить «{name}»?") },
                text: "Файл будет перемещён в корзину. Его можно будет восстановить, пока корзина не очищена.",
                detail: String::new(),
                action: "Удалить",
                forever: false,
            },
        }
    }

    /// Лежит ли открытая папка на том же томе, что и корзина: перенос в
    /// корзину — `rename`, а между томами его нет (тогда удаление стирает).
    fn trash_reachable(&self) -> bool {
        let points = mount_points();
        let mount = |path: &str| {
            points
                .iter()
                .filter(|point| *point == "/" || path == point.as_str() || path.starts_with(&format!("{point}/")))
                .max_by_key(|point| point.len())
                .cloned()
        };
        mount(&self.path) == mount(&self.trash)
    }

    /// Открыта ли корзина.
    fn in_trash(&self) -> bool {
        self.path == format!("{}/files", self.trash)
    }

    /// Перенести `path` в корзину. `Err` — код отказа переноса.
    fn move_to_trash(&self, path: &str) -> Result<String, i64> {
        let files = format!("{}/files", self.trash);
        let info = format!("{}/info", self.trash);
        make_dirs(&files)?;
        make_dirs(&info)?;
        let base = path.rsplit('/').next().unwrap_or(path);
        let (stem, extension) = match base.rfind('.') {
            Some(at) if at > 0 => (&base[..at], &base[at..]),
            _ => (base, ""),
        };
        let mut name = None;
        for attempt in 1..=1000u32 {
            let candidate = if attempt == 1 { base.to_string() } else { format!("{stem} ({attempt}){extension}") };
            if !exists(&join(&files, &candidate)) && !exists(&format!("{info}/{candidate}.trashinfo")) {
                name = Some(candidate);
                break;
            }
        }
        let name = name.ok_or(user_abi::ERR_EXISTS)?;
        let info_path = format!("{info}/{name}.trashinfo");
        let mut text = format!("[Trash Info]\nPath={}\n", percent_encode(path));
        let now = time_now();
        if now > 0 {
            let t = calendar::DateTime::from_unix(now as i64 + i64::from(self.offset) * 60);
            text.push_str(&format!(
                "DeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}\n",
                t.year, t.month, t.day, t.hour, t.minute, t.second
            ));
        }
        write_file(&info_path, text.as_bytes())?;
        let code = rename(path, &join(&files, &name));
        if code < 0 {
            remove(&info_path);
            return Err(code);
        }
        Ok(name)
    }

    /// Вернуть строку корзины на место, откуда её удалили.
    fn restore(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let name = row.name.clone();
        let info_path = format!("{}/info/{name}.trashinfo", self.trash);
        let Some(original) = read_small(&info_path).and_then(|text| trash_origin(&text)) else {
            println(&format!("files: no record where '{name}' came from"));
            self.note = Some(format!("Неизвестно, откуда удалено «{name}»"));
            return;
        };
        let parent = parent_of(&original);
        let _ = make_dirs(&parent);
        let base = original.rsplit('/').next().unwrap_or(&original).to_string();
        let (stem, extension) = match base.rfind('.') {
            Some(at) if at > 0 => (base[..at].to_string(), base[at..].to_string()),
            _ => (base.clone(), String::new()),
        };
        // Место занято — рядом под свободным именем, как в Windows.
        let mut target = join(&parent, &base);
        for attempt in 2..=1000u32 {
            if !exists(&target) {
                break;
            }
            target = join(&parent, &format!("{stem} ({attempt}){extension}"));
        }
        let code = rename(&join(&self.path, &name), &target);
        if code < 0 {
            println(&format!("files: restore of '{name}' failed: {code}"));
            self.note = Some(format!("Не удалось восстановить: {}", error_text(code)));
            return;
        }
        remove(&info_path);
        println(&format!("files: restored '{name}' to '{target}'"));
        self.note = Some(format!("Восстановлено в {target}"));
        let keep = self.selected.min(index);
        self.reload();
        self.selected = keep.min(self.rows.len().saturating_sub(1));
    }

    /// Стереть всё содержимое корзины.
    fn empty_trash(&mut self) {
        let files = format!("{}/files", self.trash);
        let names: Vec<String> = match list_dir(&files) {
            Ok((rows, _)) => rows.into_iter().map(|row| row.name).collect(),
            Err(_) => Vec::new(),
        };
        let mut count = 0;
        let mut failed = None;
        for name in &names {
            let code = remove_tree(&join(&files, name), 0);
            if code < 0 {
                failed = Some(code);
                continue;
            }
            remove(&format!("{}/info/{name}.trashinfo", self.trash));
            count += 1;
        }
        println(&format!("files: emptied the trash, {count} items"));
        self.note = Some(match failed {
            Some(code) => format!("Стёрто {count}, не удалось: {}", error_text(code)),
            None => format!("Корзина очищена: стёрто объектов {count}"),
        });
        self.reload();
    }

    /// Переименовать строку меню в `name`.
    fn finish_rename(&mut self, name: &str) {
        let Some(index) = self.menu.as_ref().and_then(|menu| menu.target) else {
            self.close_menu();
            return;
        };
        let Some(row) = self.rows.get(index) else {
            self.close_menu();
            return;
        };
        let old_name = row.name.clone();
        let name = name.trim();
        if name.is_empty() || name == old_name {
            self.close_menu();
            return;
        }
        let old = join(&self.path, &old_name);
        let new = join(&self.path, name);
        let code = rename(&old, &new);
        self.close_menu();
        if code < 0 {
            println(&format!("files: rename of '{old_name}' failed: {code}"));
            self.note = Some(format!("Не удалось переименовать: {}", error_text(code)));
            return;
        }
        println(&format!("files: renamed '{old_name}' to '{name}'"));
        self.note = Some(format!("Переименовано в «{name}»"));
        self.reload();
        if let Some(at) = self.rows.iter().position(|row| row.name == name) {
            self.selected = at;
        }
    }

    /// Удалить строку меню; без строки (в корзине) — очистить корзину.
    fn finish_delete(&mut self) {
        if let Some((dir, _)) = self.emptying.take() {
            self.close_menu();
            self.empty_folder(&dir);
            return;
        }
        let Some(index) = self.menu.as_ref().and_then(|menu| menu.target) else {
            let empty = self.in_trash();
            self.close_menu();
            if empty {
                self.empty_trash();
            }
            return;
        };
        // Отмечено несколько — удаляются все отмеченные.
        let names: Vec<String> = if self.picked.len() > 1 {
            self.picked.iter().filter_map(|index| self.rows.get(*index)).map(|row| row.name.clone()).collect()
        } else {
            self.rows.get(index).map(|row| row.name.clone()).into_iter().collect()
        };
        self.close_menu();
        for name in names {
            self.delete_named(&name);
        }
        let keep = self.selected.min(index);
        self.reload();
        self.selected = keep.min(self.rows.len().saturating_sub(1));
    }

    /// Удалить одну запись папки (в корзину или навсегда).
    fn delete_named(&mut self, name: &str) {
        let name = name.to_string();
        let path = join(&self.path, &name);
        // В корзине «Удалить» стирает навсегда, вне её — переносит в
        // корзину. С тома, где корзины нет (другой диск, `rename` между
        // томами не бывает), — стирается, как раньше, и об этом сказано.
        let trashed = if self.in_trash() || path.starts_with(&format!("{}/", self.trash)) { None } else { Some(self.move_to_trash(&path)) };
        match trashed {
            Some(Ok(as_name)) => {
                println(&format!("files: deleted '{name}' to the trash as '{as_name}'"));
                self.note = Some(format!("«{name}» — в корзине"));
            }
            Some(Err(code)) if code != user_abi::ERR_UNSUPPORTED && code != user_abi::ERR_PERMISSION => {
                println(&format!("files: delete of '{name}' failed: {code}"));
                self.note = Some(format!("Не удалось удалить: {}", error_text(code)));
                return;
            }
            _ => {
                // Другой том (корзины нет) — стирается целиком, с содержимым.
                let code = remove_tree(&path, 0);
                if code < 0 {
                    println(&format!("files: delete of '{name}' failed: {code}"));
                    self.note = Some(format!("Не удалось удалить: {}", error_text(code)));
                    return;
                }
                if self.in_trash() {
                    remove(&format!("{}/info/{name}.trashinfo", self.trash));
                }
                println(&format!("files: deleted '{name}'"));
                self.note = Some(format!("Удалено навсегда «{name}»"));
            }
        }
    }

    /// Создать папку или пустой текстовый файл в текущем каталоге.
    ///
    /// Имя — как в Windows: «Новая папка», и если такая есть — «Новая папка
    /// (2)» и дальше. Переименовать её человек может тут же, из меню.
    fn create_new(&mut self, directory: bool) {
        let base = if directory { "Новая папка" } else { "Новый текстовый документ" };
        let ext = if directory { "" } else { ".txt" };
        let mut chosen = None;
        for attempt in 1..10 {
            let name = if attempt == 1 {
                format!("{base}{ext}")
            } else {
                format!("{base} ({attempt}){ext}")
            };
            let path = join(&self.path, &name);
            let mut info = Stat::default();
            if stat(&path, &mut info) >= 0 {
                continue;
            }
            let code = if directory {
                mkdir(&path, 0o755)
            } else {
                let fd = create(&path, 0o644);
                if fd >= 0 {
                    close(fd);
                }
                fd
            };
            chosen = Some((name, code));
            break;
        }
        match chosen {
            Some((name, code)) if code >= 0 => {
                println(&format!("files: created '{name}' in {}", self.path));
                self.note = Some(format!("Создано «{name}»"));
                self.reload();
                if let Some(at) = self.rows.iter().position(|row| row.name == name) {
                    self.selected = at;
                    self.picked.clear();
                    self.no_cursor = false;
                    self.scroll.follow(at);
                    // Как в Windows (24.11): новое сразу просит имя, выделено
                    // имя без расширения — Enter оставляет предложенное.
                    self.start_rename(at);
                }
            }
            Some((name, code)) => {
                println(&format!("files: cannot create '{name}': {code}"));
                self.note = Some(format!("Не удалось создать: {}", error_text(code)));
            }
            None => {
                self.note = Some(String::from("Не удалось создать: слишком много одноимённых"));
            }
        }
    }

    /// Свойства самого каталога.
    fn show_folder_props(&mut self) {
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if self.on_computer() {
            rows.push(("Тип", String::from("Этот компьютер")));
            rows.push(("Томов", format!("{}", self.volume_count())));
            if let Some(info) = sysinfo() {
                rows.push((
                    "Память",
                    format!(
                        "{} МиБ свободно из {}",
                        info.frames_free / (1024 * 1024),
                        info.frames_total / (1024 * 1024)
                    ),
                ));
                rows.push(("Задач", format!("{}", info.tasks_alive)));
            }
        } else {
            rows.push(("Тип", String::from("Папка")));
            rows.push(("Объектов", format!("{}", self.rows.len() + self.dropped)));
            rows.push(("Путь в стиле Windows", sysconf::winpath::to_windows(&self.path)));
        }
        let name = if self.on_computer() {
            String::from("Этот компьютер")
        } else {
            self.path.clone()
        };
        println(&format!("files: properties of '{name}'"));
        self.props = Some(Props { name, rows, details: Vec::new(), mode: None, tab: 0 });
    }

    /// Перейти в каталог, запомнив, откуда пришли.
    ///
    /// Переход вперёд обнуляет список «вперёд» — как в любом обозревателе: путь,
    /// с которого свернули, перестаёт существовать.
    fn go_to(&mut self, path: String) {
        self.address = None;
        self.crumb_pop = None;
        if path == self.path {
            return;
        }
        self.back.push(core::mem::replace(&mut self.path, path));
        self.forward.clear();
        self.query.clear();
        self.searching = false;
        self.preview = None;
        // Ответ прошлого действия относился к прошлому каталогу.
        self.note = None;
        self.scroll.reset();
        self.hscroll.reset();
        self.reload();
        // Отдельная строка, а не «список перечитан»: «выделение переехало» и
        // «мы вошли внутрь» печатают одно и то же — путь и число строк, — и
        // отличить одно от другого снаружи было бы нечем.
        println(&format!("files: entered '{}'", self.path));
    }

    /// Подняться на уровень выше. Из корня — на страницу «Этот компьютер»,
    /// как из `C:\` в Windows.
    fn go_up(&mut self) -> bool {
        if self.on_computer() {
            return false;
        }
        if self.path == "/" {
            self.go_to(String::from(COMPUTER));
            return true;
        }
        let parent = parent_of(&self.path);
        self.go_to(parent);
        true
    }

    /// Вернуться туда, откуда пришли.
    fn go_back(&mut self) -> bool {
        let Some(previous) = self.back.pop() else {
            return false;
        };
        self.forward.push(core::mem::replace(&mut self.path, previous));
        self.preview = None;
        self.scroll.reset();
        self.reload();
        true
    }

    /// Пойти обратно вперёд — туда, откуда вернулись назад.
    fn go_forward(&mut self) -> bool {
        let Some(next) = self.forward.pop() else {
            return false;
        };
        self.back.push(core::mem::replace(&mut self.path, next));
        self.preview = None;
        self.scroll.reset();
        self.reload();
        true
    }

    /// Щелчок по окну: координаты внутри поверхности.
    ///
    /// Кнопки навигации, строки быстрого доступа и строки списка ищутся по той
    /// же [`layout`], по которой рисуются, — иначе они разъедутся при первом же
    /// изменении размера окна, и попасть в них будет можно только наугад.
    fn on_click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        self.tip = None;

        // «Параметры папок» — модальное окно (24.11).
        if self.options.is_some() {
            let op = options_plan(ctx, area);
            let rows = op.rows.len();
            let hit = op
                .rows
                .iter()
                .position(|rect| rect.contains(x, y))
                .or_else(|| [op.restore, op.ok, op.cancel, op.apply].iter().position(|rect| rect.contains(x, y)).map(|at| rows + at));
            return match hit {
                Some(index) => self.press_options(index),
                None => false,
            };
        }

        // Окно выбора программы — модальное (22.11).
        if let Some(MenuMode::Choose(chooser)) = self.menu.as_ref().map(|menu| &menu.mode) {
            let cp = chooser_plan(ctx, area, chooser.progs.len());
            enum Act {
                Open,
                Cancel,
                Toggle,
                Pick(usize),
                Nothing,
            }
            let act = if cp.ok.contains(x, y) {
                Act::Open
            } else if cp.cancel.contains(x, y) {
                Act::Cancel
            } else if cp.check.contains(x, y) {
                Act::Toggle
            } else if let Some(at) = cp.rows.iter().position(|row| row.contains(x, y)) {
                let index = chooser.first + at;
                if index >= chooser.progs.len() {
                    Act::Nothing
                } else if index == chooser.selected {
                    Act::Open
                } else {
                    Act::Pick(index)
                }
            } else {
                Act::Nothing
            };
            match act {
                Act::Open => self.finish_choose(),
                Act::Cancel => self.close_menu(),
                Act::Toggle | Act::Pick(_) => {
                    if let Some(MenuMode::Choose(chooser)) = self.menu.as_mut().map(|menu| &mut menu.mode) {
                        match act {
                            Act::Toggle => chooser.always = !chooser.always,
                            Act::Pick(index) => chooser.selected = index,
                            _ => {}
                        }
                    }
                }
                Act::Nothing => {}
            }
            return true;
        }

        // Вопрос об удалении — модальный (21.11): отвечают его кнопками,
        // щелчок мимо них ничего не делает.
        if let Some(MenuMode::Confirm(_)) = self.menu.as_ref().map(|menu| &menu.mode) {
            match kit::ask_hit(ctx, area, &self.ask().as_ask(), x, y) {
                Some(true) => self.finish_delete(),
                Some(false) => self.close_menu(),
                None => return false,
            }
            return true;
        }

        // Меню вкладки — первым, как всякое открытое меню.
        if self.tab_menu.is_some() {
            return self.click_tab_menu(ctx, area, x, y);
        }
        // Список подпапок у шеврона адреса (21.11).
        if self.crumb_pop.is_some() {
            return self.click_crumb_pop(ctx, area, &plan, x, y);
        }
        // Адрес правят текстом: щелчок в поле ставит каретку, мимо — отмена.
        if self.address.is_some() {
            if plan.path.contains(x, y) {
                let text = address_text(ctx, &plan);
                if let Some(line) = self.address.as_mut() {
                    line.click(ctx.face(Role::Body), text.w, x - text.x);
                }
                return true;
            }
            self.address = None;
        }
        // Полоса вкладок (20.11).
        if let Some(hit) = self.tab_strip(ctx, &plan).hit(x, y) {
            return self.click_tab(hit);
        }

        // Идёт копирование — его окно отвечает первым.
        if self.job.is_some() {
            let (card, pause, cancel) = Self::job_plan(ctx, &plan);
            if pause.contains(x, y) {
                if let Some(job) = self.job.as_mut() {
                    job.paused = !job.paused;
                    if job.error.is_some() {
                        // После ошибки «Продолжить» — пропустить файл.
                        job.error = None;
                        job.files = None;
                        job.next += 1;
                    }
                }
                return true;
            }
            if cancel.contains(x, y) {
                self.cancel_copy();
                return true;
            }
            if card.contains(x, y) {
                return false;
            }
        }
        // Выпавшее меню «Сортировка» / «Вид».
        if let Some((command, _, rect, items)) = self.pop_layout(ctx, area, &plan) {
            let entries: Vec<Entry<'_>> = items.iter().map(|(title, group)| Entry { title, icon: None, danger: false, group: *group, ..Entry::NONE }).collect();
            let hit = kit::menu_hit(ctx, rect, &entries, x, y);
            drop(entries);
            match hit {
                Some(index) => self.run_pop(command, index),
                None => {
                    // Щелчок по той же кнопке закрывает, по другой — откроет её.
                    self.pop = None;
                    if let Some((other, _)) = plan.commands.iter().find(|(_, r)| r.contains(x, y)) {
                        if *other != command {
                            return self.run_command(*other);
                        }
                    }
                }
            }
            return true;
        }
        // Заголовок столбца — сортировка по нему; второй щелчок — обратно.
        if self.view == ViewMode::Table && plan.header.contains(x, y) && !self.on_computer() && self.preview.is_none() && self.props.is_none() {
            // Граница столбцов (21.11) — тянуть ширину, а не сортировать.
            if let Some(boundary) = self.col_grip(ctx, &plan, x) {
                self.col_drag = Some((boundary, x, self.cols));
                return true;
            }
            let (date_x, type_x, size_x, _) = self.columns(ctx, self.table_list(ctx, &plan));
            let key = if x >= size_x {
                SortKey::Size
            } else if x >= type_x {
                SortKey::Kind
            } else if x >= date_x {
                SortKey::Date
            } else {
                SortKey::Name
            };
            let descending = key == self.sort && !self.descending;
            self.set_sort(key, descending);
            return true;
        }

        // «7-Zip ▸» and its submenus (25.11) — over the menu card.
        if let Some(done) = self.click_zip(ctx, area, x, y) {
            return done;
        }
        // Открытое меню получает щелчок первым: внутри — пункт, снаружи —
        // закрытие, и щелчок мимо на этом заканчивается. Так ведёт себя всякое
        // меню, которое человек видел.
        if let Some(menu) = self.menu.as_ref() {
            if let Some(card) = self.menu_rect(ctx, area) {
                if let MenuMode::OpenWith(progs, _) = &menu.mode {
                    if card.contains(x, y) {
                        let fits = openwith_rows(progs) - 1;
                        if let Some(row) = (0..=fits).find(|row| openwith_row_rect(ctx, card, *row, fits).contains(x, y)) {
                            self.pick_open_with(row);
                        }
                        return true;
                    }
                }
                if matches!(menu.mode, MenuMode::Items) && card.contains(x, y) {
                    // Та же геометрия пунктов, что у отрисовки: считает её `kit`.
                    let hit = kit::menu_hit(ctx, card, &entries(&menu.items), x, y).map(|index| menu.items[index]);
                    if let Some(item) = hit {
                        self.run_menu_item(item);
                    }
                    return true;
                }
                if card.contains(x, y) {
                    return false;
                }
            }
            self.close_menu();
            return true;
        }

        // Полосы прокрутки (20.11): бегунок тянут, по дорожке листают
        // страницами.
        if self.preview.is_none() && self.props.is_none() && self.press_bars(ctx, &plan, x, y) {
            self.last_click = None;
            return true;
        }

        // Вид переехал в строку состояния (макет 14): две кнопки справа.
        if let Some(at) = plan.view.iter().position(|rect| rect.contains(x, y)) {
            return self.set_view(ViewMode::ALL[at]);
        }
        if let Some((command, _)) = plan.commands.iter().find(|(_, rect)| rect.contains(x, y)) {
            self.searching = false;
            return self.run_command(*command);
        }
        if plan.toolbar.contains(x, y) {
            let was = self.searching;
            self.searching = plan.search.contains(x, y);
            if self.searching {
                return !was;
            }
            if plan.nav[0].contains(x, y) {
                return self.go_back();
            }
            if plan.nav[1].contains(x, y) {
                return self.go_forward();
            }
            if plan.nav[2].contains(x, y) {
                return self.go_up();
            }
            if plan.refresh.contains(x, y) {
                self.reload();
                return true;
            }
            // Звено адреса — переход в него, шеврон — список подпапок, «…» —
            // спрятанные звенья, пустое место справа — адрес текстом (21.11,
            // как в Проводнике).
            if plan.path.contains(x, y) {
                let (slots, lead) = self.crumb_slots(ctx, &plan);
                if lead.contains(x, y) {
                    return self.open_crumb_pop(ctx, &plan, None);
                }
                for (index, slot) in slots.iter().enumerate() {
                    if slot.rect.contains(x, y) {
                        if slot.path != self.path {
                            self.go_to(slot.path.clone());
                            return true;
                        }
                        return false;
                    }
                    if slot.chevron.contains(x, y) {
                        return self.open_crumb_pop(ctx, &plan, Some(index));
                    }
                }
                self.edit_address();
                return true;
            }
            return was;
        }
        self.searching = false;

        if let Some(side) = plan.side {
            if side.contains(x, y) {
                let inner = ctx.on(theme::panel_bg());
                for slot in place_slots(inner, side, &self.home, &self.pinned) {
                    if slot.rect.contains(x, y) {
                        // Закреплённую папку можно переставить (24.11):
                        // переход — по отпусканию, если её не тянули.
                        if let Some(from) = slot.pinned {
                            self.pin_drag = Some(PinDrag { from, start: y, to: from, active: false, path: slot.path });
                            return false;
                        }
                        if slot.path == self.path {
                            return false;
                        }
                        self.go_to(slot.path);
                        return true;
                    }
                }
                return false;
            }
        }

        // Окно свойств: вкладки и «ОК»; мимо карточки — закрыть.
        if self.props.is_some() && self.preview.is_none() {
            let (card, tabs, ok) = Self::props_plan(ctx, &plan);
            if ok.contains(x, y) || !card.contains(x, y) {
                self.props = None;
                return true;
            }
            if let (Some(index), Some(props)) = (tabs.iter().position(|tab| tab.contains(x, y)), self.props.as_mut()) {
                if index == 0 || props.mode.is_some() {
                    props.tab = index as u8;
                    return true;
                }
            }
            return false;
        }
        // В просмотре и в свойствах списка нет, и щёлкать в нём не по чему:
        // «попал в невидимую строку» — это выбор вслепую.
        // Таблица — под шапкой, плитка и «Список» — вместе с местом шапки.
        let body = if self.on_computer() || self.view != ViewMode::Table { plan.list.union(&plan.header) } else { plan.list };
        if self.preview.is_some() || self.props.is_some() || !body.contains(x, y) {
            return false;
        }
        let Some(index) = self.row_at(&plan, ctx, x, y) else {
            // Пустое место (21.11, как в Проводнике): щелчок снимает
            // выделение, нажатие с протяжкой — рамка выделения; с Ctrl рамка
            // добавляет к отмеченному. Строка `selected` остаётся курсором
            // клавиатуры, но не подсвечена.
            self.last_click = None;
            if self.on_computer() || self.error.is_some() {
                return false;
            }
            let ctrl = self.press_mods & WIN_MOD_CTRL != 0;
            let had = !self.picked.is_empty() || !self.no_cursor;
            if !ctrl {
                self.picked.clear();
                self.no_cursor = true;
            }
            self.band = Some(Band { start: (x, y), at: (x, y), base: self.picked.clone(), active: false });
            return had && !ctrl;
        };
        // Одиночный щелчок выбирает, двойной открывает. Двойного щелчка окно
        // не получает — приходят два обычных, — и «двойной» здесь значит
        // «второй по той же строке не позже [`DOUBLE_CLICK_MS`]».
        let now = monotonic_ms();
        let double = matches!(self.last_click, Some((last, at)) if last == index && now.saturating_sub(at) <= DOUBLE_CLICK_MS);
        // Простой щелчок снимает отметки — как в Проводнике. Но щелчок по
        // строке внутри выделения его не трогает до отпускания: может быть,
        // выделение сейчас потащат (21.11).
        let inside = self.picked.len() > 1 && self.picked.contains(&index);
        if !inside {
            self.picked.clear();
        }
        self.selected = index;
        self.anchor = index;
        self.no_cursor = false;
        // Строка, видная краем, выезжает целиком.
        self.scroll.follow(index);
        if double {
            self.last_click = None;
            self.picked.clear();
            return self.open_selected();
        }
        self.last_click = Some((index, now));
        // Нажатие на строке — возможно, начало перетаскивания.
        self.start_drag(index, x, y, if inside { ClickAfter::Collapse } else { ClickAfter::Nothing });
        true
    }

    /// Нажали на строке: может быть, её (или выделение) сейчас потащат.
    fn start_drag(&mut self, index: usize, x: i32, y: i32, after: ClickAfter) {
        if !self.on_computer() && self.rows.get(index).is_some_and(|row| !row.is_volume()) {
            self.drag = Some(Drag { from: index, start: (x, y), at: (x, y), active: false, target: None, after, out: false, points: Vec::new() });
        }
    }

    /// Строки, которые тянут: выделение, если нажали на отмеченную, иначе
    /// одна строка нажатия.
    fn dragged(&self, drag: &Drag) -> Vec<usize> {
        if self.picked.contains(&drag.from) {
            self.picked.iter().copied().filter(|index| *index < self.rows.len()).collect()
        } else {
            alloc::vec![drag.from]
        }
    }

    /// Копирует ли перетаскивание в `dir` (а не переносит): с Ctrl — всегда,
    /// без него — между разными томами, как в Проводнике.
    fn drag_copies(&self, drag: &Drag, dir: &str) -> bool {
        self.ctrl_held || mount_of(&drag.points, dir) != mount_of(&drag.points, &self.path)
    }

    /// Указатель с зажатой кнопкой: тянуть строку, подсветить папку под ним.
    fn on_drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        // Выделение мышью в поле адреса (24.11).
        if self.address.as_ref().is_some_and(Line::held) {
            let text = address_text(ctx, &layout(ctx, area));
            return self.address.as_mut().is_some_and(|line| line.drag(ctx.face(Role::Body), text.w, x - text.x));
        }
        if self.tab_drag.is_some() {
            return self.drag_tab(ctx, area, x, y);
        }
        // Закреплённое место тянут вверх или вниз (24.11).
        if self.pin_drag.is_some() {
            let plan = layout(ctx, area);
            let slots = plan.side.map(|side| place_slots(ctx.on(theme::panel_bg()), side, &self.home, &self.pinned)).unwrap_or_default();
            let Some(drag) = self.pin_drag.as_mut() else { return false };
            if !drag.active && (y - drag.start).abs() > 6 {
                drag.active = true;
                println(&format!("files: moving pinned '{}'", drag.path));
            }
            if !drag.active {
                return false;
            }
            let pinned: Vec<&PlaceSlot> = slots.iter().filter(|slot| slot.pinned.is_some()).collect();
            let to = match pinned.iter().find(|slot| y >= slot.rect.y && y < slot.rect.bottom()) {
                Some(slot) => slot.pinned.unwrap_or(drag.from),
                None if pinned.first().is_some_and(|slot| y < slot.rect.y) => pinned.first().and_then(|slot| slot.pinned).unwrap_or(drag.from),
                None => pinned.last().and_then(|slot| slot.pinned).unwrap_or(drag.from),
            };
            let changed = to != drag.to;
            drag.to = to;
            return changed;
        }
        if self.scroll.dragging() || self.hscroll.dragging() {
            let plan = layout(ctx, area);
            let (v, h) = self.bars(ctx, &plan);
            return v.is_some_and(|bar| bar.drag(&self.scroll, x, y)) | h.is_some_and(|bar| bar.drag(&self.hscroll, x, y));
        }
        if let Some((boundary, from, widths)) = self.col_drag {
            return self.drag_column(boundary, x - from, widths, ctx);
        }
        if self.band.is_some() {
            return self.drag_band(area, ctx, x, y);
        }
        if self.drag.is_none() {
            return false;
        }
        let plan = layout(ctx, area);
        let dragged = self.drag.as_ref().map(|drag| self.dragged(drag)).unwrap_or_default();
        // Куда бросить: папка в списке (не та, что тянут), место слева,
        // звено адреса, другая вкладка (21.11).
        let mut target = None;
        let body = if self.view == ViewMode::Table { plan.list } else { plan.list.union(&plan.header) };
        if body.contains(x, y) {
            if let Some(index) = self.row_at(&plan, ctx, x, y) {
                if !dragged.contains(&index) && self.rows.get(index).is_some_and(|row| row.directory && !row.is_volume()) {
                    target = Some((join(&self.path, &self.rows[index].name), self.item_rect(ctx, &plan, index)));
                }
            }
        } else if let Some(side) = plan.side.filter(|side| side.contains(x, y)) {
            let inner = ctx.on(theme::panel_bg());
            if let Some(slot) = place_slots(inner, side, &self.home, &self.pinned).into_iter().find(|slot| slot.rect.contains(x, y)) {
                if slot.path != COMPUTER && slot.path != self.path {
                    target = Some((slot.path, slot.rect));
                }
            }
        } else if plan.path.contains(x, y) && self.address.is_none() {
            let (slots, _) = self.crumb_slots(ctx, &plan);
            if let Some(slot) = slots.into_iter().find(|slot| slot.rect.contains(x, y)) {
                if slot.path != COMPUTER && slot.path != self.path {
                    target = Some((slot.path, slot.rect));
                }
            }
        } else if let Some(kit::TabHit::Tab(index) | kit::TabHit::Close(index)) = self.tab_strip(ctx, &plan).hit(x, y) {
            let path = self.tabs.get(index).map(|tab| tab.path.clone()).filter(|path| index != self.tab && path != COMPUTER && *path != self.path);
            if let Some(path) = path {
                target = Some((path, Rect::EMPTY));
            }
        }
        // Сюда не бросить: в тянущуюся папку и внутрь неё.
        if let Some((dir, _)) = &target {
            let inside = dragged.iter().filter_map(|index| self.rows.get(*index)).any(|row| {
                let src = join(&self.path, &row.name);
                *dir == src || dir.starts_with(&format!("{src}/"))
            });
            if inside {
                target = None;
            }
        }
        let Some(drag) = self.drag.as_mut() else {
            return false;
        };
        drag.at = (x, y);
        if !drag.active && ((x - drag.start.0).abs() > 8 || (y - drag.start.1).abs() > 8) {
            drag.active = true;
            drag.points = mount_points();
            println(&format!("files: dragging {} item(s)", dragged.len()));
        }
        drag.out = !area.contains(x, y);
        drag.target = target;
        drag.active
    }

    /// Где строка `index` на экране — в том виде, что открыт.
    fn item_rect(&self, ctx: Ctx, plan: &Plan, index: usize) -> Rect {
        match self.view {
            ViewMode::Tiles => plan.tile_rect(&plan.grid(ctx), index, self.scroll.offset()).unwrap_or(Rect::EMPTY),
            ViewMode::List => {
                let list = self.list_plan(ctx, plan);
                self.list_item_rect(&list, index, self.scroll.offset())
            }
            ViewMode::Table => {
                let table = self.table_list(ctx, plan);
                let pad = ctx.px(12);
                Rect::new(table.x + pad as i32, table.y + (index as u32 * (plan.row_h + plan.row_gap)) as i32 - self.scroll.offset(), table.w.saturating_sub(pad * 2), plan.row_h)
            }
        }
    }

    /// Рамку выделения тянут: отмечено то, что она задевает, плюс то, что
    /// было отмечено до неё с Ctrl.
    fn drag_band(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        let Some(band) = self.band.as_mut() else {
            return false;
        };
        band.at = (x, y);
        if !band.active && (x - band.start.0).abs() + (y - band.start.1).abs() < 6 {
            return false;
        }
        band.active = true;
        let frame = band_rect(band.start, band.at);
        let mut picked = band.base.clone();
        // Рамка не выходит за список.
        let visible = if self.view == ViewMode::Table { plan.list } else { plan.list.union(&plan.header) };
        let frame = frame.intersect(&visible);
        if let Some(band) = self.band.as_mut() {
            band.at = (band.at.0.clamp(visible.x, visible.right() - 1), band.at.1.clamp(visible.y, visible.bottom() - 1));
        }
        let mut first = None;
        for index in 0..self.rows.len() {
            let rect = self.item_rect(ctx, &plan, index);
            if rect.is_empty() || rect.intersect(&visible).is_empty() {
                continue;
            }
            if !rect.intersect(&frame).is_empty() {
                picked.insert(index);
                first.get_or_insert(index);
            }
        }
        if let Some(index) = first {
            self.selected = index;
            self.no_cursor = false;
        } else if picked.is_empty() {
            self.no_cursor = true;
        }
        self.picked = picked;
        // Рамка рисуется на каждом движении, даже без смены отметок.
        true
    }

    /// Границу столбцов тянут на `dx` точек от нажатия.
    fn drag_column(&mut self, boundary: usize, dx: i32, start: [u32; 3], ctx: Ctx) -> bool {
        let dx = dx * 100 / ctx.px(100).max(1) as i32;
        let grow = |width: u32, by: i32, min: u32| (width as i32 + by).clamp(min as i32, COL_MAX as i32) as u32;
        let mut cols = start;
        match boundary {
            // Имя | дата: имя тянется за счёт даты.
            0 => cols[0] = grow(start[0], -dx, COLS_MIN[0]),
            // Дата | тип и тип | размер: левый шире — правый уже.
            1 | 2 => {
                let (left, right) = (boundary - 1, boundary);
                let moved = (grow(start[left], dx, COLS_MIN[left]) as i32 - start[left] as i32).min(start[right] as i32 - COLS_MIN[right] as i32);
                cols[left] = (start[left] as i32 + moved) as u32;
                cols[right] = (start[right] as i32 - moved) as u32;
            }
            _ => {}
        }
        let changed = cols != self.cols;
        self.cols = cols;
        changed
    }

    /// Отпустили: рамку — убрать, границу столбца — запомнить, строки на
    /// папке — перенести или скопировать туда, за окном — предложить
    /// другому окну «Файлов».
    fn on_release(&mut self) -> bool {
        if let Some(line) = self.address.as_mut().filter(|line| line.held()) {
            line.release();
            return true;
        }
        if let Some(drag) = self.pin_drag.take() {
            if !drag.active {
                // Щелчок по закреплённой папке — переход в неё.
                if drag.path != self.path {
                    self.go_to(drag.path);
                }
                return true;
            }
            if drag.to != drag.from && drag.from < self.pinned.len() && drag.to < self.pinned.len() {
                let mut list = self.pinned.clone();
                let moved = list.remove(drag.from);
                list.insert(drag.to, moved);
                self.save_pins(list);
                println(&format!("files: pinned '{}' moved to place {}", drag.path, drag.to + 1));
            }
            return true;
        }
        if self.col_drag.take().is_some() {
            println(&format!("files: columns {},{},{}", self.cols[0], self.cols[1], self.cols[2]));
            self.save_settings();
            return true;
        }
        if let Some(band) = self.band.take() {
            if band.active {
                println(&format!("files: {} item(s) marked by the frame", self.picked.len()));
            }
            return true;
        }
        let Some(drag) = self.drag.take() else {
            return false;
        };
        if !drag.active {
            // Щелчок без перетаскивания: Ctrl по отмеченной — снять отметку,
            // щелчок внутри выделения — оставить одну строку.
            return match drag.after {
                ClickAfter::Unpick => {
                    self.picked.remove(&drag.from);
                    println(&format!("files: {} item(s) marked", self.picked.len()));
                    true
                }
                ClickAfter::Collapse => {
                    self.picked.clear();
                    true
                }
                ClickAfter::Nothing => false,
            };
        }
        let paths: Vec<String> = self.dragged(&drag).iter().filter_map(|index| self.rows.get(*index)).map(|row| join(&self.path, &row.name)).collect();
        let Some((dir, _)) = drag.target.clone() else {
            if drag.out && !paths.is_empty() {
                let copy = self.ctrl_held;
                return self.offer_items(&paths, copy);
            }
            return true;
        };
        let copy = self.drag_copies(&drag, &dir);
        println(&format!("files: dropped {} item(s) on {dir}{}", paths.len(), if copy { " (copy)" } else { "" }));
        self.drop_paths(paths, dir, copy);
        true
    }

    /// Перенести (или скопировать) `paths` в `dir` — тем же путём, что
    /// «Вырезать»/«Копировать» и «Вставить»; буфер человека не трогается.
    fn drop_paths(&mut self, paths: Vec<String>, dir: String, copy: bool) {
        let saved = self.clip.replace((paths, !copy));
        self.paste_into(dir);
        self.clip = saved;
    }

    /// Что тянут — подпись у указателя и рамка папки, куда бросить.
    fn draw_drag(&self, s: &mut Surface, ctx: Ctx) {
        if let Some(band) = self.band.as_ref().filter(|band| band.active) {
            let p = ctx.palette;
            let frame = band_rect(band.start, band.at);
            draw::rounded(s, frame, 0, p.acc, 48);
            draw::rounded_stroke(s, frame, 0, p.acc, 200);
        }
        let Some(drag) = self.drag.as_ref().filter(|drag| drag.active) else {
            return;
        };
        let p = ctx.palette;
        if let Some((_, rect)) = &drag.target {
            if !rect.is_empty() {
                draw::rounded_stroke(s, *rect, ctx.px(theme::R_ROW), p.acc, 255);
            }
        }
        let count = self.dragged(drag).len();
        let label = if count > 1 { format!("Объектов: {count}") } else { self.rows.get(drag.from).map(|row| row.name.clone()).unwrap_or_default() };
        let label = match &drag.target {
            Some((dir, _)) => {
                let verb = if self.drag_copies(drag, dir) { "+ Копировать в" } else { "→ Переместить в" };
                format!("{verb} «{}»: {label}", place_title(dir, &self.home).rsplit('/').next().unwrap_or(dir))
            }
            None if drag.out => format!("{label} — в другое окно «Файлов»{}", if self.ctrl_held { " (копия)" } else { "" }),
            None => label,
        };
        let w = ctx.face(Role::Body).width(&label) + ctx.px(20);
        let chip = Rect::new(drag.at.0 + ctx.px(14) as i32, drag.at.1 + ctx.px(10) as i32, w, ctx.px(28));
        let inner = kit::card(ctx, s, chip);
        paint::text(inner, s, Role::Body, chip.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, chip), &label, p.ink);
    }

    /// Правая кнопка: меню для строки под указателем или для каталога.
    fn on_click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        if self.tab_menu.take().is_some() {
            return true;
        }
        // Правая кнопка по вкладке — её меню (20.11).
        if let Some(kit::TabHit::Tab(index) | kit::TabHit::Close(index)) = self.tab_strip(ctx, &plan).hit(x, y) {
            self.close_menu();
            self.pop = None;
            self.tab_menu = Some((index, x, y, 0));
            println(&format!("files: tab menu opened for tab {}", index + 1));
            return true;
        }
        if self.menu.is_some() {
            self.close_menu();
            return true;
        }
        if self.options.is_some() {
            return false;
        }
        // Место колонки быстрого доступа (24.11): открыть, закрепить или
        // открепить, свойства.
        if let Some(side) = plan.side.filter(|side| side.contains(x, y)) {
            let slot = place_slots(ctx.on(theme::panel_bg()), side, &self.home, &self.pinned).into_iter().find(|slot| slot.rect.contains(x, y));
            if let Some(slot) = slot.filter(|slot| slot.path != COMPUTER && !slot.path.ends_with("/.local/share/Trash/files")) {
                let pinned = slot.pinned.is_some();
                let mut items = MenuItem::ON_PLACE.to_vec();
                if pinned {
                    items[2] = MenuItem::Unpin;
                } else if slot.path == self.home || USB_PLACES.contains(&slot.path.as_str()) || slot.path == "/" {
                    items.remove(2);
                }
                println(&format!("files: menu opened for place '{}'", slot.path));
                self.menu = Some(Menu { x, y, items, selected: 0, mode: MenuMode::Items, target: None, place: Some(slot.path) });
                return true;
            }
            return false;
        }
        let body = if self.on_computer() || self.view != ViewMode::Table { plan.list.union(&plan.header) } else { plan.list };
        if self.preview.is_some() || self.props.is_some() || !body.contains(x, y) {
            return false;
        }
        let target = self.row_at(&plan, ctx, x, y);
        // Как в Проводнике: правая кнопка по строке вне выделения выбирает
        // её одну; по отмеченной — меню для всех отмеченных.
        match target {
            Some(index) => {
                if !self.picked.contains(&index) {
                    self.picked.clear();
                }
                self.selected = index;
                self.no_cursor = false;
            }
            None if !self.on_computer() => {
                self.picked.clear();
                self.no_cursor = true;
            }
            None => {}
        }
        self.open_menu(Some((x, y)), target);
        true
    }

    /// Строка списка под точкой — в таблице или в плитке.
    fn row_at(&self, plan: &Plan, ctx: Ctx, x: i32, y: i32) -> Option<usize> {
        if self.on_computer() {
            let (cards, _, _) = self.computer_layout(ctx, plan);
            return cards.iter().position(|card| card.contains(x, y));
        }
        if self.view == ViewMode::List {
            let list = self.list_plan(ctx, plan);
            if !list.area.contains(x, y) {
                return None;
            }
            let inside_x = x - list.area.x - list.pad + self.scroll.offset();
            let inside_y = y - list.area.y - list.top;
            if inside_x < 0 || inside_y < 0 {
                return None;
            }
            let row = (inside_y / list.step.max(1) as i32) as usize;
            if row >= list.per_col {
                return None;
            }
            let index = (inside_x / list.col_w.max(1) as i32) as usize * list.per_col + row;
            if index >= self.rows.len() || !self.list_item_rect(&list, index, self.scroll.offset()).contains(x, y) {
                return None;
            }
            return Some(index);
        }
        if self.view == ViewMode::Tiles {
            let grid = plan.grid(ctx);
            let area = plan.list.union(&plan.header);
            if grid.cols == 0 || !area.contains(x, y) {
                return None;
            }
            let col = ((x - area.x - grid.margin as i32) / grid.cell_w as i32).max(0) as usize;
            let inside = y - area.y - plan.tiles_top(&grid) as i32 + self.scroll.offset();
            if col >= grid.cols || inside < 0 {
                return None;
            }
            let index = (inside / grid.cell_h.max(1) as i32) as usize * grid.cols + col;
            let cell = plan.tile_rect(&grid, index, self.scroll.offset())?;
            if !cell.contains(x, y) {
                return None;
            }
            return (index < self.rows.len()).then_some(index);
        }
        // Строки едут по точкам (20.11): номер — от начала списка, а не от
        // первой видимой строки.
        if !plan.list.contains(x, y) {
            return None;
        }
        let index = self.scroll.row_at(y, plan.list.y, plan.row_h + plan.row_gap)?;
        (index < self.rows.len()).then_some(index)
    }

    /// Где стоит карточка меню: у точки щелчка, либо у строки, для которой
    /// его открыли клавишей; в окно вписывается всегда.
    fn menu_rect(&self, ctx: Ctx, area: Rect) -> Option<Rect> {
        let menu = self.menu.as_ref()?;
        let (w, h) = self.menu_size(ctx);
        let (mut x, mut y) = if menu.x != 0 || menu.y != 0 {
            (menu.x, menu.y)
        } else {
            // У клавиатуры точки нет: карточка встаёт у строки.
            let plan = layout(ctx, area);
            match menu.target {
                Some(index) if self.view == ViewMode::Tiles => {
                    let grid = plan.grid(ctx);
                    plan.tile_rect(&grid, index, self.scroll.offset())
                        .map_or((plan.list.x, plan.list.y), |r| (r.x + r.w as i32 / 2, r.bottom()))
                }
                Some(index) if self.view == ViewMode::List => {
                    let r = self.item_rect(ctx, &plan, index);
                    (r.x + ctx.px(32) as i32, r.bottom())
                }
                Some(index) => {
                    let r = plan.row_rect_at(ctx, index, self.scroll.offset());
                    (r.x + ctx.px(48) as i32, r.bottom())
                }
                None => (plan.list.x + ctx.px(24) as i32, plan.list.y + ctx.px(24) as i32),
            }
        };
        let max_x = (area.right() - w as i32).max(area.x);
        let max_y = (area.bottom() - h as i32).max(area.y);
        x = x.clamp(area.x, max_x);
        y = y.clamp(area.y, max_y);
        Some(Rect::new(x, y, w, h))
    }

    /// Размер карточки меню в её нынешнем режиме.
    fn menu_size(&self, ctx: Ctx) -> (u32, u32) {
        let Some(menu) = self.menu.as_ref() else {
            return (0, 0);
        };
        let pad = ctx.px(6);
        match &menu.mode {
            MenuMode::Items => kit::menu_size(ctx, &entries(&menu.items)),
            MenuMode::OpenWith(progs, _) => {
                let face = ctx.face(Role::Body);
                let widest = progs
                    .iter()
                    .filter(|prog| prog.fits())
                    .map(|prog| face.width(&prog.caption) + if prog.mark == b'd' { face.width(" (по умолчанию)") } else { 0 })
                    .chain(core::iter::once(face.width(OTHER_PROGRAM)))
                    .max()
                    .unwrap_or(0);
                let fits = openwith_rows(progs) as u32 - 1;
                (
                    (widest + ctx.px(10 + 20 + 10 + 16) + pad * 2).max(ctx.px(240)),
                    pad * 2 + ctx.px(PROG_ROW_H) * (fits + 1) + if fits > 0 { ctx.px(9) } else { 0 },
                )
            }
            MenuMode::Choose(_) => (ctx.px(10), ctx.px(10)),
            MenuMode::Rename(..) | MenuMode::Confirm(_) => {
                let hint = ctx.face(Role::Caption).width(RENAME_HINT);
                (hint.max(ctx.px(320)) + ctx.px(28), ctx.px(40) + ctx.px(32) + ctx.px(28) + pad * 2)
            }
        }
    }

    /// Нарисовать окно целиком.
    ///
    /// Фон заливается здесь, а не приходит готовым: до переезда область
    /// заливало окно ядра, а у программы поверхность своя и заливать её больше
    /// некому.
    fn render(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        surface.fill(area, theme::window_bg());
        let plan = layout(ctx, area);
        // Классика (22.11): область списка белая, как у Проводника Windows 98.
        if theme::is_classic() {
            surface.fill(plan.list.union(&plan.header), ctx.flat(ctx.palette.sunk));
        }
        // Список — первым (20.11). Строки едут по точкам, и край строки,
        // ушедшей под шапку, панель или боковую колонку, закрывают они сами,
        // нарисованные поверх: отсечения у поверхности нет.
        if let Some(preview) = self.preview.as_ref() {
            self.draw_preview(surface, ctx, &plan, preview);
        } else if self.on_computer() && self.error.is_none() {
            self.draw_computer(surface, ctx, &plan);
        } else if self.draw_net_state(surface, ctx, &plan) {
            // «Сеть»: loading, the password window, nobody found (24.11).
        } else if self.view == ViewMode::Tiles {
            self.draw_tiles(surface, ctx, &plan);
        } else if self.view == ViewMode::List {
            self.draw_list_view(surface, ctx, &plan);
        } else {
            self.draw_list(surface, ctx, &plan);
        }
        if let Some(side) = plan.side {
            self.draw_side(surface, ctx, side);
        }
        // Полоса вкладок — до панели: обводка активной вкладки уходит вниз и
        // закрывается панелью.
        self.draw_tabs(surface, ctx, &plan);
        self.draw_toolbar(surface, ctx, &plan);
        self.draw_status(surface, ctx, &plan);
        if self.preview.is_none() {
            self.draw_bars(surface, ctx, &plan);
        }
        // Свойства — окном поверх списка (макет 14).
        if let (None, Some(props)) = (self.preview.as_ref(), self.props.as_ref()) {
            self.draw_props(surface, ctx, &plan, props);
        }
        self.draw_tip(surface, ctx, area);
        self.draw_menu(surface, ctx, area);
        self.draw_pop(surface, ctx, area, &plan);
        self.draw_crumb_pop(surface, ctx, area, &plan);
        self.draw_job(surface, ctx, &plan);
        self.draw_drag(surface, ctx);
        self.draw_tab_extras(surface, ctx, area);
        self.draw_options(surface, ctx, area);
    }

    /// «Этот компьютер» (24.11, по снимку Романа — как в Windows 11):
    /// раздел «Устройства и диски» — плитки томов с полосой заполнения, под
    /// ним «Сетевые расположения». Плитки, верх заголовка дисков и сети.
    fn computer_layout(&self, ctx: Ctx, plan: &Plan) -> (Vec<Rect>, i32, i32) {
        let area = plan.list.union(&plan.header);
        let pad = ctx.px(16) as i32;
        let disks_head = area.y + ctx.px(10) as i32;
        let top = disks_head + ctx.px(32) as i32;
        let room = (area.w as i32 - pad * 2).max(0) as u32;
        let gap = ctx.px(10);
        let per_row = ((room + gap) / (ctx.px(250) + gap)).max(1);
        let tile_w = (room.saturating_sub(gap * (per_row - 1)) / per_row).min(ctx.px(330));
        let tile_h = ctx.px(60);
        // «Сеть» (24.11) is the last row and stands under its own heading.
        let disks = self.rows.iter().filter(|row| row.name != sysconf::winpath::NET).count();
        let mut cards: Vec<Rect> = (0..disks)
            .map(|index| {
                let (column, line) = (index as u32 % per_row, index as u32 / per_row);
                Rect::new(area.x + pad + (column * (tile_w + gap)) as i32, top + (line * (tile_h + gap)) as i32, tile_w, tile_h)
            })
            .collect();
        let lines = (disks as u32).div_ceil(per_row).max(1);
        let net_head = top + (lines * (tile_h + gap)) as i32 + ctx.px(12) as i32;
        for index in 0..self.rows.len() - disks {
            cards.push(Rect::new(area.x + pad + (index as u32 * (tile_w + gap)) as i32, net_head + ctx.px(32) as i32, tile_w, tile_h));
        }
        (cards, disks_head, net_head)
    }

    /// Заголовок раздела «Этого компьютера»: шеврон и подпись.
    fn section_head(ctx: Ctx, s: &mut Surface, x: i32, y: i32, title: &str) {
        let p = ctx.palette;
        let glyph = ctx.px(12);
        glyphicon::draw(s, Icon::ChevronDown, x, y + ctx.px(5) as i32, glyph, p.ink3, 255);
        paint::text(ctx, s, Role::Strong, x + ctx.px(20) as i32, y, title, p.ink);
    }

    fn draw_computer(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        let (cards, disks_head, net_head) = self.computer_layout(ctx, plan);
        let x = area.x + ctx.px(16) as i32;
        Self::section_head(ctx, s, x, disks_head, "Устройства и диски");
        let hot = self.hot_now(ctx, plan);
        for (index, (row, card)) in self.rows.iter().zip(&cards).enumerate() {
            if card.y >= area.bottom() {
                break;
            }
            let state = if self.is_marked(index) {
                RowState::Selected
            } else if hot == Hot::Row(index) {
                RowState::Hover
            } else {
                RowState::Idle
            };
            paint::row(ctx, s, *card, state);
            let side = ctx.px(44);
            let icon = Rect::new(card.x + ctx.px(8) as i32, card.y + (card.h as i32 - side as i32) / 2, side, side);
            let art = volume_art(&row.name);
            if !draw_design_icon(&self.icons, s, icon, art) {
                paint::icon_tile(ctx, s, icon, Icon::Disk, Tone::Accent, true);
            }
            let tx = icon.right() + ctx.px(10) as i32;
            let room = (card.right() - tx - ctx.px(10) as i32).max(0) as u32;
            let mut ty = card.y + ctx.px(6) as i32;
            paint::text_clipped(ctx, s, Role::Body, tx, ty, room, &volume_title(&row.name, &row.label), p.ink);
            ty += i32::from(ctx.face(Role::Body).line) + ctx.px(3) as i32;
            if row.name == sysconf::winpath::NET {
                paint::text_clipped(ctx, s, Role::Caption, tx, ty, room, "Компьютеры и общие папки (SMB)", p.ink3);
            } else if row.size > 0 {
                let used = row.size.saturating_sub(row.free);
                let percent = (used * 100 / row.size.max(1)).min(100);
                let bar = Rect::new(tx, ty, room, ctx.px(12));
                s.fill(bar, ctx.flat(p.sunk));
                draw::rounded_stroke(s, bar, 0, p.line.color, p.line.alpha);
                let fill = (u64::from(bar.w) * percent / 100) as u32;
                if fill > 0 {
                    // Свободно меньше десятой части — красным, как в Windows.
                    let low = row.free.saturating_mul(10) < row.size;
                    s.fill(Rect::new(bar.x + 1, bar.y + 1, fill.saturating_sub(2).max(1), bar.h.saturating_sub(2)), if low { p.bad } else { p.acc });
                }
                ty += bar.h as i32 + ctx.px(3) as i32;
                let facts = format!("{} свободно из {}", human_size(row.free), human_size(row.size));
                paint::text_clipped(ctx, s, Role::Caption, tx, ty, room, &facts, p.ink3);
            } else {
                paint::text_clipped(ctx, s, Role::Caption, tx, ty, room, &format!("{} · место не считается", row.volume), p.ink4);
            }
        }
        // Сетевые расположения (24.11): «Сеть» — `/net`, компьютеры и их
        // общие папки (SMB); без неё (ядро без сети) — честно пусто.
        let has_net = self.rows.iter().any(|row| row.name == sysconf::winpath::NET);
        if has_net && net_head < area.bottom() {
            Self::section_head(ctx, s, x, net_head, "Сетевые расположения");
        } else if net_head + ctx.px(60) as i32 <= area.bottom() {
            Self::section_head(ctx, s, x, net_head, "Сетевые расположения");
            let y = net_head + ctx.px(32) as i32;
            let room = area.w.saturating_sub(ctx.px(56));
            paint::text_clipped(ctx, s, Role::Body, x + ctx.px(20) as i32, y, room, "Сетевых расположений нет", p.ink3);
            paint::text_clipped(ctx, s, Role::Caption, x + ctx.px(20) as i32, y + ctx.px(22) as i32, room, "Общие папки в сети (SMB) появятся здесь, когда их можно будет подключать.", p.ink4);
        }
    }

    /// Выпавшее меню «Сортировка» / «Вид»: где и какие пункты.
    fn pop_layout(&self, ctx: Ctx, area: Rect, plan: &Plan) -> Option<(Command, usize, Rect, Vec<(String, u8)>)> {
        let (command, selected) = self.pop?;
        let button = plan.commands.iter().find(|(c, _)| *c == command)?.1;
        let items = self.pop_entries(command);
        let entries: Vec<Entry<'_>> = items.iter().map(|(title, group)| Entry { title, icon: None, danger: false, group: *group, ..Entry::NONE }).collect();
        let size = kit::menu_size(ctx, &entries);
        let rect = kit::place((button.x, button.bottom() + ctx.px(2) as i32), size, area);
        Some((command, selected, rect, items))
    }

    fn draw_pop(&self, s: &mut Surface, ctx: Ctx, area: Rect, plan: &Plan) {
        let Some((_, selected, rect, items)) = self.pop_layout(ctx, area, plan) else {
            return;
        };
        let entries: Vec<Entry<'_>> = items.iter().map(|(title, group)| Entry { title, icon: None, danger: false, group: *group, ..Entry::NONE }).collect();
        kit::draw_menu(ctx, s, rect, &entries, selected);
    }

    /// Окно копирования (макет 14): ход, скорость, сколько осталось.
    fn job_plan(ctx: Ctx, plan: &Plan) -> (Rect, Rect, Rect) {
        let area = plan.list.union(&plan.header);
        let w = ctx.px(520).min(area.w.saturating_sub(ctx.px(24)));
        let h = ctx.px(210);
        let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 3, w, h);
        let pad = ctx.px(20) as i32;
        let bw = ctx.px(110);
        let by = card.bottom() - pad - ctx.px(32) as i32;
        let cancel = Rect::new(card.right() - pad - bw as i32, by, bw, ctx.px(32));
        let pause = Rect::new(cancel.x - ctx.px(8) as i32 - bw as i32, by, bw, ctx.px(32));
        (card, pause, cancel)
    }

    fn draw_job(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let Some(job) = &self.job else {
            return;
        };
        let p = ctx.palette;
        let (card, pause, cancel) = Self::job_plan(ctx, plan);
        let inner = kit::card(ctx, s, card);
        let pad = ctx.px(20) as i32;
        let percent = if job.total == 0 { 100 } else { job.done * 100 / job.total };
        let x = card.x + pad;
        let room = card.w - ctx.px(40);
        let mut y = card.y + pad;
        paint::text(inner, s, Role::Title, x, y, &format!("Копирование: {percent}%"), p.ink);
        y += ctx.px(30) as i32;
        let what = format!("Копирование {} элементов из «{}» в «{}»", job.items, job.from, job.to);
        paint::text_clipped(inner, s, Role::Body, x, y, room, &what, p.ink2);
        y += ctx.px(28) as i32;
        let bar = Rect::new(x, y, room, ctx.px(10));
        draw::rounded(s, bar, ctx.px(5), p.line.color, p.line.alpha);
        let fill = (u64::from(bar.w) * percent.min(100) / 100) as u32;
        if fill > 0 {
            draw::rounded(s, Rect::new(bar.x, bar.y, fill, bar.h), ctx.px(5), p.acc, 255);
        }
        y += ctx.px(20) as i32;
        let seconds = monotonic_ms().saturating_sub(job.started).max(1);
        let speed = job.done * 1000 / seconds;
        let left = if speed == 0 { String::from("—") } else { format!("{} с", job.total.saturating_sub(job.done) / speed.max(1)) };
        let facts = match &job.error {
            Some(error) => error.clone(),
            None => format!("Осталось {left} · {}/с · {} из {}", human_size(speed), human_size(job.done), human_size(job.total)),
        };
        paint::text_clipped(inner, s, Role::Caption, x, y, room, &facts, if job.error.is_some() { p.bad_ink } else { p.ink3 });
        y += ctx.px(18) as i32;
        paint::text_clipped(inner, s, Role::Caption, x, y, room, &format!("Сейчас: {}", job.current), p.ink4);
        paint::button(inner, s, pause, Weight::Normal, if job.paused { "Продолжить" } else { "Пауза" }, false);
        paint::button(inner, s, cancel, Weight::Normal, "Отмена", false);
    }

    /// Окно свойств (макет 14): где карточка, вкладки и кнопка «ОК».
    fn props_plan(ctx: Ctx, plan: &Plan) -> (Rect, [Rect; 2], Rect) {
        let area = plan.list.union(&plan.header);
        let w = ctx.px(440).min(area.w.saturating_sub(ctx.px(24)));
        let h = ctx.px(500).min(area.h.saturating_sub(ctx.px(16)));
        let card = Rect::new(area.x + (area.w as i32 - w as i32) / 2, area.y + (area.h as i32 - h as i32) / 2, w, h);
        let pad = ctx.px(20) as i32;
        let tab_y = card.y + ctx.px(52) as i32;
        let face = ctx.face(Role::Body);
        let general = Rect::new(card.x + pad, tab_y, face.width("Общие") + ctx.px(24), ctx.px(32));
        let details = Rect::new(general.right() + ctx.px(4) as i32, tab_y, face.width("Подробно") + ctx.px(24), ctx.px(32));
        let ok = Rect::new(card.right() - pad - ctx.px(110) as i32, card.bottom() - pad - ctx.px(32) as i32, ctx.px(110), ctx.px(32));
        (card, [general, details], ok)
    }

    fn draw_props(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, props: &Props) {
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        if area.is_empty() {
            return;
        }
        let (card, tabs, ok) = Self::props_plan(ctx, plan);
        let inner = kit::card(ctx, s, card);
        let pad = ctx.px(20) as i32;
        let x = card.x + pad;
        let room = card.w.saturating_sub(ctx.px(40));
        paint::text_clipped(inner, s, Role::Title, x, card.y + pad, room, &format!("Свойства: {}", props.name), p.ink);
        // Вкладки — с подчёркиванием акцентом у открытой.
        let with_details = props.mode.is_some();
        for (index, (rect, label)) in tabs.iter().zip(["Общие", "Подробно"]).enumerate() {
            if index == 1 && !with_details {
                continue;
            }
            let active = props.tab as usize == index;
            paint::text(inner, s, Role::Body, rect.x + ctx.px(12) as i32, paint::baseline(inner, Role::Body, *rect), label, if active { p.ink } else { p.ink3 });
            if active {
                s.fill(Rect::new(rect.x + ctx.px(8) as i32, rect.bottom() - ctx.px(2) as i32, rect.w - ctx.px(16), ctx.px(2)), p.acc);
            }
        }
        draw::hline(s, card.x + pad, tabs[0].bottom(), room, p.line.color, p.line.alpha);
        let mut y = tabs[0].bottom() + ctx.px(16) as i32;
        let label_w = ctx.px(112);
        // Шаг строк — по месту: в низком окне строки плотнее, а не вылезают.
        let free = (ok.y - y) as u32;
        let wanted = if props.tab == 0 { props.rows.len() as u32 + 2 } else { props.details.len() as u32 + 6 };
        let line = u32::from(ctx.face(Role::Body).line);
        let step = (free / wanted.max(1)).clamp(line + ctx.px(2), line + ctx.px(12)) as i32;
        let lines = if props.tab == 0 { &props.rows } else { &props.details };
        if props.tab == 0 {
            // Шапка: значок и имя.
            let side = ctx.px(32);
            let icon = Rect::new(x, y, side, side);
            let kind = props.rows.iter().find(|(label, _)| *label == "Тип файла").map(|(_, value)| value.as_str()).unwrap_or("");
            let art = file_kind(&props.name, kind.starts_with("Папка")).0;
            draw_design_icon(&self.icons, s, icon, art);
            paint::text_clipped(inner, s, Role::Strong, icon.right() + ctx.px(12) as i32, y + ctx.px(6) as i32, room - side - ctx.px(12), &props.name, p.ink);
            y += side as i32 + ctx.px(16) as i32;
        } else {
            paint::text(inner, s, Role::Strong, x, y, "Владелец и права доступа", p.ink2);
            y += step;
        }
        for (label, value) in lines {
            if y + step > ok.y - ctx.px(8) as i32 {
                break;
            }
            paint::text_clipped(inner, s, Role::Caption, x, y + ctx.px(2) as i32, label_w, label, p.ink4);
            paint::text_clipped(inner, s, Role::Body, x + label_w as i32, y, room.saturating_sub(label_w), value, p.ink2);
            y += step;
        }
        // Таблица прав: строки — кто, столбцы — что можно.
        if let (1, Some(mode)) = (props.tab, props.mode) {
            y += ctx.px(6) as i32;
            let col_w = ctx.px(80) as i32;
            for (index, head) in ["Чтение", "Запись", "Запуск"].iter().enumerate() {
                paint::text(inner, s, Role::Caption, x + label_w as i32 + index as i32 * col_w, y, head, p.ink4);
            }
            y += step;
            for (row, who) in ["Владелец", "Группа", "Остальные"].iter().enumerate() {
                if y + step > ok.y {
                    break;
                }
                paint::text(inner, s, Role::Body, x, y, who, p.ink2);
                for column in 0..3 {
                    let bit = 1u32 << (8 - (row * 3 + column));
                    let bx = Rect::new(x + label_w as i32 + column as i32 * col_w, y + ctx.px(2) as i32, ctx.px(14), ctx.px(14));
                    if mode & bit != 0 {
                        draw::rounded(s, bx, ctx.px(3), p.acc, 255);
                        glyphicon::draw(s, Icon::Check, bx.x, bx.y, bx.w, p.acc_ink, 255);
                    } else {
                        draw::rounded_stroke(s, bx, ctx.px(3), p.ink4, 255);
                    }
                }
                y += step;
            }
            if y + step <= ok.y {
                paint::text_clipped(inner, s, Role::Caption, x, y, (ok.x - x - ctx.px(8) as i32).max(0) as u32, "Этот раздел нужен тем, кто знает, зачем он нужен.", p.ink4);
            }
        }
        paint::button(inner, s, ok, Weight::Primary, "ОК", false);
    }

    /// Плитки: значок и подпись, как значки на столе.
    fn draw_tiles(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.list.is_empty() {
            return;
        }
        let area = plan.list.union(&plan.header);
        if let Some(error) = &self.error {
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                area.x + ctx.px(24) as i32,
                area.y + ctx.px(24) as i32,
                area.w.saturating_sub(ctx.px(48)),
                error,
                p.bad_ink,
            );
            return;
        }
        if self.on_computer() {
            let x = area.x + ctx.px(28) as i32;
            paint::caps(ctx, s, x, area.y + ctx.px(14) as i32, "ТОМА");
        }
        let grid = plan.grid(ctx);
        // Прокрутка рядами по точкам (20.11), как у списка.
        let (view, content, _) = self.view_metrics(ctx, plan);
        let top = plan.tiles_top(&grid) as i32;
        let cols = grid.cols.max(1);
        let cell_h = grid.cell_h.max(1);
        self.scroll.sync_with(view.h, content, |index| Some((top + (index / cols) as i32 * cell_h as i32, cell_h)));
        self.page_rows.set(cols * (view.h / cell_h).max(1) as usize);
        let at = self.scroll.offset();
        let first = ((at - top).max(0) as u32 / cell_h) as usize * cols;
        let per_page = (view.h / cell_h + 2) as usize * cols;
        let hot = self.hot_now(ctx, plan);
        if self.rows.is_empty() {
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                area.x + ctx.px(28) as i32,
                area.y + ctx.px(28) as i32,
                area.w.saturating_sub(ctx.px(56)),
                "Пусто",
                p.ink5,
            );
        }
        for (index, row) in self.rows.iter().enumerate().skip(first).take(per_page) {
            let Some(cell) = plan.tile_rect(&grid, index, at) else {
                break;
            };
            let selected = self.is_marked(index);
            if selected {
                draw::rounded(s, cell, ctx.px(theme::R_CARD), p.hover1.color, p.hover1.alpha.max(40));
                draw::rounded_stroke(s, cell, ctx.px(theme::R_CARD), p.accline.color, p.accline.alpha);
            } else if hot == Hot::Row(index) {
                draw::rounded(s, cell, ctx.px(theme::R_CARD), p.hover1.color, p.hover1.alpha);
            }
            let side = ctx.px(theme::ICON_TILE);
            let tile = Rect::new(
                cell.x + (cell.w as i32 - side as i32) / 2,
                cell.y + ctx.px(10) as i32,
                side,
                side,
            );
            // Картинка темы без плитки (22.11, Роман: «синий квадрат с синей
            // папкой» не нужен); плитка — только если картинки нет.
            let kind_icon = self.row_art(row).0;
            if !draw_design_icon(&self.icons, s, tile, kind_icon) {
                let (icon, tone, filled) = row.icon();
                paint::icon_tile(ctx, s, tile, icon, tone, filled);
            }
            let label = Rect::new(
                cell.x + ctx.px(4) as i32,
                tile.bottom() + ctx.px(theme::ICON_LABEL_GAP) as i32,
                cell.w.saturating_sub(ctx.px(8)),
                u32::from(ctx.face(Role::Caption).line),
            );
            let ink = if selected { p.ink } else { p.ink2 };
            let shown = self.tile_name(row);
            let width = ctx.face(Role::Caption).width(&shown);
            if width <= label.w {
                paint::text_centered(ctx, s, Role::Caption, label, &shown, ink);
            } else {
                paint::text_clipped(ctx, s, Role::Caption, label.x, label.y, label.w, &shown, ink);
            }
            if row.is_volume() {
                let sub = Rect::new(label.x, label.bottom() + ctx.px(2) as i32, label.w, label.h);
                paint::text_centered(ctx, s, Role::MonoSmall, sub, &row.volume, p.ink4);
            }
        }
        if self.on_computer() {
            self.draw_computer_facts(s, ctx, plan, &grid);
        }
    }

    /// Подпись плитки.
    fn tile_name(&self, row: &Row) -> String {
        if row.is_volume() {
            return volume_title(&row.name, &row.label);
        }
        self.shown_name(row).to_string()
    }

    /// Под томами на странице «Этот компьютер»: память, задачи и честная
    /// строка про оборудование.
    fn draw_computer_facts(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, grid: &Grid) {
        let p = ctx.palette;
        let rows_used = self.rows.len().div_ceil(grid.cols.max(1)).min(grid.rows);
        let mut y = plan.list.y + (grid.margin + rows_used as u32 * grid.cell_h) as i32 + ctx.px(18) as i32;
        let x = plan.list.x + ctx.px(28) as i32;
        let room = plan.list.w.saturating_sub(ctx.px(56));
        if y + ctx.px(90) as i32 > plan.list.bottom() {
            return;
        }
        paint::caps(ctx, s, x, y, "СИСТЕМА");
        y += ctx.px(22) as i32;
        let step = i32::from(ctx.face(Role::Body).line) + ctx.px(6) as i32;
        if let Some(info) = sysinfo() {
            let facts = [
                format!(
                    "Память: {} МиБ свободно из {}",
                    info.frames_free / (1024 * 1024),
                    info.frames_total / (1024 * 1024)
                ),
                format!("Время работы: {}", uptime_text(info.uptime_ms)),
                format!("Задач: {}, окон: {}", info.tasks_alive, info.windows),
            ];
            for fact in facts {
                if y + step > plan.list.bottom() {
                    return;
                }
                paint::text_clipped(ctx, s, Role::Body, x, y, room, &fact, p.ink3);
                y += step;
            }
        }
        y += ctx.px(12) as i32;
        if y + ctx.px(50) as i32 > plan.list.bottom() {
            return;
        }
        paint::caps(ctx, s, x, y, "ОБОРУДОВАНИЕ");
        y += ctx.px(22) as i32;
        paint::text_clipped(
            ctx,
            s,
            Role::Body,
            x,
            y,
            room,
            "Устройства и драйверы — «Диспетчер устройств» в «Пуске»",
            p.ink4,
        );
    }

    /// Карточка меню поверх всего.
    fn draw_menu(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let Some(menu) = self.menu.as_ref() else {
            return;
        };
        let Some(card) = self.menu_rect(ctx, area) else {
            return;
        };
        // Пункты рисует общее меню; поле имени и вопрос об удалении — своё
        // содержимое на той же карточке (фаза С8).
        if matches!(menu.mode, MenuMode::Items) {
            kit::draw_menu(ctx, s, card, &zip_entries(&menu.items), menu.selected);
            self.draw_zip(ctx, s, area);
            return;
        }
        if let MenuMode::Confirm(focus) = menu.mode {
            kit::draw_ask(ctx, s, area, &self.ask().as_ask(), focus);
            return;
        }
        if let MenuMode::Choose(chooser) = &menu.mode {
            self.draw_chooser(s, ctx, area, chooser);
            return;
        }
        if let MenuMode::OpenWith(progs, selected) = &menu.mode {
            draw_openwith(ctx, s, card, progs, *selected);
            return;
        }
        let inner = kit::card(ctx, s, card);
        let p = inner.palette;
        match &menu.mode {
            MenuMode::Items => {}
            MenuMode::Rename(line, suffix) => {
                let x = card.x + ctx.px(14) as i32;
                let room = card.w.saturating_sub(ctx.px(28));
                let mut y = card.y + ctx.px(12) as i32;
                let caption = if suffix.is_empty() { String::from("Новое имя") } else { format!("Новое имя (расширение {suffix} сохранится)") };
                paint::text_clipped(inner, s, Role::Caption, x, y, room, &caption, p.ink4);
                y += ctx.px(22) as i32;
                let field = Rect::new(x, y, room, ctx.px(32));
                paint::sunk(inner, s, field, ctx.px(theme::R_ROW));
                draw::rounded_stroke(s, field, ctx.px(theme::R_ROW), p.acc, 255);
                // Строка ввода набора (24.11): выделенное имя — на заливке.
                let text = Rect::new(field.x + ctx.px(10) as i32, field.y, field.w.saturating_sub(ctx.px(20)), field.h);
                line.draw(inner, s, text, true, p.ink);
                y += field.h as i32 + ctx.px(10) as i32;
                paint::text_clipped(inner, s, Role::Caption, x, y, room, RENAME_HINT, p.ink4);
            }
            MenuMode::Confirm(_) | MenuMode::OpenWith(..) | MenuMode::Choose(_) => {}
        }
    }

    /// Подсказке пора показаться (24.11): указатель стоит на строке
    /// [`TIP_DELAY_MS`], и ничего не открыто. `true` — перерисовать.
    fn tip_step(&mut self) -> bool {
        let idle = self.menu.is_none() && self.options.is_none() && self.pop.is_none() && self.props.is_none() && self.preview.is_none() && self.drag.is_none() && self.band.is_none() && self.job.is_none();
        let Some(tip) = self.tip.as_ref() else { return false };
        if tip.lines.is_some() || monotonic_ms().saturating_sub(tip.since) < TIP_DELAY_MS {
            return false;
        }
        if !idle {
            self.tip = None;
            return false;
        }
        let index = tip.index;
        let lines = self.tip_lines(index);
        if let Some(row) = self.rows.get(index) {
            println(&format!("files: tip for '{}': {}", row.name, lines.join(" | ")));
        }
        if let Some(tip) = self.tip.as_mut() {
            tip.lines = Some(lines);
        }
        true
    }

    /// Что сказать о строке `index` в подсказке: тип, размер, дата; у папки
    /// (если так выбрано) — размер содержимого и что в ней лежит.
    fn tip_lines(&self, index: usize) -> Vec<String> {
        let Some(row) = self.rows.get(index) else { return Vec::new() };
        let mut out = Vec::new();
        if row.is_volume() {
            out.push(format!("Файловая система: {}", row.volume));
            if row.size > 0 {
                out.push(format!("Свободно: {} из {}", human_size(row.free), human_size(row.size)));
            }
            return out;
        }
        if row.directory {
            if row.mtime != 0 {
                out.push(format!("Дата изменения: {}", date_text(row.mtime, self.offset)));
            }
            // Computers and shared folders (24.11): their kind, and no walk —
            // over the network it could hold the window.
            if let Some(level @ (0 | 1)) = net_level(&self.path) {
                out.push(String::from(if level == 0 { "Компьютер в сети" } else { "Общая папка" }));
                out.push(net_title(&join(&self.path, &row.name)));
                return out;
            }
            if self.opts.folder_size_tips && !is_net(&self.path) {
                let summary = folder_summary(&join(&self.path, &row.name), TIP_WALK_LIMIT);
                if summary.files + summary.dirs == 0 && summary.complete {
                    out.push(String::from("Пустая папка"));
                } else {
                    out.push(format!("Размер: {}{}", if summary.complete { "" } else { "более " }, human_size(summary.bytes)));
                    if !summary.first_dirs.is_empty() {
                        out.push(format!("Папки: {}", summary.first_dirs.join(", ")));
                    }
                    if !summary.first_files.is_empty() {
                        out.push(format!("Файлы: {}", summary.first_files.join(", ")));
                    }
                }
            }
            return out;
        }
        let kind = if row.program && row.mode & 0o111 != 0 { "Приложение" } else { file_kind(&row.name, false).1 };
        out.push(format!("Тип: {kind}"));
        out.push(format!("Размер: {}", human_size(row.size)));
        if row.mtime != 0 {
            out.push(format!("Дата изменения: {}", date_text(row.mtime, self.offset)));
        }
        out
    }

    /// Подсказка у указателя.
    fn draw_tip(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let (Some(tip), Some((px, py))) = (self.tip.as_ref(), self.pointer) else { return };
        let Some(lines) = tip.lines.as_ref().filter(|lines| !lines.is_empty()) else { return };
        let face = ctx.face(Role::Caption);
        let step = u32::from(face.line) + ctx.px(2);
        let max_w = ctx.px(420);
        let w = (lines.iter().map(|line| face.width(line)).max().unwrap_or(0) + ctx.px(20)).min(max_w);
        let h = step * lines.len() as u32 + ctx.px(14);
        let mut rect = Rect::new(px + ctx.px(12) as i32, py + ctx.px(20) as i32, w, h);
        if rect.right() > area.right() {
            rect.x = (area.right() - w as i32).max(area.x);
        }
        if rect.bottom() > area.bottom() {
            rect.y = (py - h as i32 - ctx.px(6) as i32).max(area.y);
        }
        let inner = kit::card(ctx, s, rect);
        let mut y = rect.y + ctx.px(7) as i32;
        for line in lines {
            paint::text_clipped(inner, s, Role::Caption, rect.x + ctx.px(10) as i32, y, w.saturating_sub(ctx.px(20)), line, inner.palette.ink2);
            y += step as i32;
        }
    }

    /// Окно «Параметры папок» (24.11) поверх «Файлов».
    fn draw_options(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        let Some(dialog) = self.options.as_ref() else { return };
        let plan = options_plan(ctx, area);
        draw::blend_rect(s, area, mini_ui::Color::rgb(0, 0, 0), 70);
        let inner = kit::card(ctx, s, plan.card);
        let p = inner.palette;
        let pad = ctx.px(20) as i32;
        let x = plan.card.x + pad;
        let width = plan.card.w.saturating_sub(ctx.px(40));
        let mut y = plan.card.y + pad;
        paint::text_clipped(inner, s, Role::Heading, x, y, width, "Параметры папок", p.ink);
        y += i32::from(ctx.face(Role::Heading).line) + ctx.px(4) as i32;
        paint::text_clipped(inner, s, Role::Caption, x, y, width, "Вид папок в «Файлах» и значков на рабочем столе", p.ink3);
        y += i32::from(ctx.face(Role::Caption).line) + ctx.px(10) as i32;
        paint::text_clipped(inner, s, Role::Strong, x, y, width, "Дополнительные параметры:", p.ink2);
        let hot = self.pointer;
        for (index, (rect, (_, caption))) in plan.rows.iter().zip(sysconf::store::FolderOptions::ROWS.iter()).enumerate() {
            if dialog.focus == index {
                paint::row(inner, s, *rect, RowState::Selected);
            } else if hot.is_some_and(|(hx, hy)| rect.contains(hx, hy)) {
                paint::row(inner, s, *rect, RowState::Hover);
            }
            draw_check(inner, s, Rect::new(rect.x + ctx.px(8) as i32, rect.y, ctx.px(16), rect.h), dialog.draft.get(index));
            let tx = rect.x + ctx.px(34) as i32;
            paint::text_clipped(inner, s, Role::Body, tx, paint::baseline(inner, Role::Body, *rect), (rect.right() - tx).max(0) as u32, caption, p.ink2);
        }
        let rows = plan.rows.len();
        let focus_ring = |s: &mut Surface, rect: Rect, index: usize| {
            if dialog.focus == index {
                draw::rounded_stroke(s, Rect::new(rect.x - 2, rect.y - 2, rect.w + 4, rect.h + 4), ctx.px(6), p.acc, 255);
            }
        };
        paint::button(inner, s, plan.restore, Weight::Normal, "Восстановить значения по умолчанию", false);
        focus_ring(s, plan.restore, rows);
        paint::separator(inner, s, plan.card.x, plan.ok.y - ctx.px(14) as i32, plan.card.w);
        paint::button(inner, s, plan.ok, Weight::Primary, "ОК", false);
        focus_ring(s, plan.ok, rows + 1);
        paint::button(inner, s, plan.cancel, Weight::Normal, "Отмена", false);
        focus_ring(s, plan.cancel, rows + 2);
        // «Применить» гаснет, пока менять нечего.
        let dirty = dialog.draft != self.opts;
        paint::button(inner, s, plan.apply, if dirty { Weight::Normal } else { Weight::Ghost }, "Применить", false);
        focus_ring(s, plan.apply, rows + 3);
    }

    /// Окно «Выбрать другую программу…» посреди «Файлов» (22.11).
    fn draw_chooser(&self, s: &mut Surface, ctx: Ctx, area: Rect, chooser: &Chooser) {
        let name = self.menu_target_file().map(|(_, name)| name).unwrap_or_default();
        draw_chooser_card(ctx, s, area, chooser, &name, false);
    }

    /// Панель инструментов: три стрелки и строка пути.
    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.toolbar.is_empty() {
            return;
        }
        // Панель и контекст поверх неё — общие (`kit`, фаза С8).
        let bar = kit::toolbar(ctx, s, plan.toolbar);

        // Недоступная кнопка гаснет, а не пропадает: «назад» из первого же
        // каталога не должно выглядеть как неисправность.
        let states = [
            (Icon::Back, !self.back.is_empty()),
            (Icon::Forward, !self.forward.is_empty()),
            (Icon::Up, !self.on_computer()),
        ];
        for (rect, (icon, enabled)) in plan.nav.iter().zip(states) {
            nav_button(bar, s, *rect, icon, enabled);
        }
        nav_button(bar, s, plan.refresh, Icon::Update, true);

        // Адрес звеньями, как в Проводнике: «Этот компьютер › Системный диск
        // › bin».
        if !plan.path.is_empty() {
            self.draw_address(s, bar, plan);
        }

        // Поиск: «Поиск: <папка>», с курсором, пока в поле печатают.
        if !plan.search.is_empty() {
            paint::sunk(bar, s, plan.search, ctx.px(theme::R_ROW));
            if self.searching {
                draw::rounded_stroke(s, plan.search, ctx.px(theme::R_ROW), p.acc, 255);
            }
            let glyph = ctx.px(13);
            glyphicon::draw(s, Icon::Search, plan.search.x + ctx.px(10) as i32, plan.search.y + (plan.search.h as i32 - glyph as i32) / 2, glyph, p.ink4, 255);
            let x = plan.search.x + ctx.px(30) as i32;
            let room = (plan.search.right() - x - ctx.px(8) as i32).max(0) as u32;
            let y = paint::baseline(bar, Role::Body, plan.search);
            if self.query.is_empty() && !self.searching {
                let here = breadcrumb(&self.path, &self.home).last().map(|(label, _)| label.clone()).unwrap_or_default();
                paint::text_clipped(bar, s, Role::Body, x, y, room, &format!("Поиск: {here}"), p.ink4);
            } else {
                let used = paint::text_clipped(bar, s, Role::Body, x, y, room, &self.query, p.ink);
                if self.searching {
                    s.fill(Rect::new(x + used as i32 + 1, plan.search.y + ctx.px(8) as i32, 1, plan.search.h - ctx.px(16)), p.acc);
                }
            }
        }

        // Панель команд (макет 14): значок и подпись.
        if let Some(first) = plan.commands.first() {
            let row = Rect::new(plan.toolbar.x, first.1.y - ctx.px(4) as i32, plan.toolbar.w, first.1.h + ctx.px(8));
            s.fill(row, ctx.flat(p.panel));
            draw::hline(s, row.x, row.bottom() - 1, row.w, p.line.color, p.line.alpha);
            let inner = ctx.on(theme::panel_bg());
            let mut previous: Option<Command> = None;
            for (command, rect) in &plan.commands {
                if previous.is_some_and(|before| before.group() != command.group()) {
                    draw::vline(s, rect.x - ctx.px(7) as i32, rect.y + ctx.px(6) as i32, rect.h.saturating_sub(ctx.px(12)), p.line.color, p.line.alpha);
                }
                previous = Some(*command);
                let (icon, label) = command.look();
                let has_target = !self.targets().is_empty() && !self.on_computer();
                let enabled = match command {
                    Command::Rename | Command::Delete | Command::Cut | Command::Copy => has_target,
                    Command::Paste => !self.on_computer() && user_progs::clipboard_get_files().is_some(),
                    Command::NewFolder => !self.on_computer(),
                    _ => true,
                };
                if self.pop.is_some_and(|(open, _)| open == *command) {
                    draw::rounded(s, *rect, ctx.px(theme::R_ROW), ctx.flat(p.btn), 255);
                }
                let ink = if enabled { p.ink2 } else { p.ink5 };
                let glyph = ctx.px(18);
                let gx = if label.is_empty() { rect.x + (rect.w as i32 - glyph as i32) / 2 } else { rect.x + ctx.px(8) as i32 };
                iconset::draw(s, icon, gx, rect.y + (rect.h as i32 - glyph as i32) / 2, glyph, ink, 255);
                if !label.is_empty() {
                    paint::text(inner, s, Role::Body, rect.x + ctx.px(32) as i32, paint::baseline(inner, Role::Body, *rect), label, ink);
                }
            }
        }
    }

    /// Боковая колонка быстрого доступа.
    fn draw_side(&self, s: &mut Surface, ctx: Ctx, side: Rect) {
        let p = ctx.palette;
        if side.is_empty() {
            return;
        }
        s.fill(side, ctx.flat(p.panel));
        draw::vline(s, side.right() - 1, side.y, side.h, p.line.color, p.line.alpha);
        let inner = ctx.on(theme::panel_bg());
        let pad = ctx.px(10);

        // Колонка не прокручивается — подсвеченное запомнено верно.
        let hot = if self.pointer.is_some_and(|(x, y)| side.contains(x, y)) { self.hot } else { Hot::Nothing };
        for (index, slot) in place_slots(inner, side, &self.home, &self.pinned).into_iter().enumerate() {
            if slot.head.is_some() {
                draw::hline(s, slot.rect.x, slot.head_y + ctx.px(6) as i32, slot.rect.w, p.line2.color, p.line2.alpha);
            }
            let state = if slot.path == self.path {
                RowState::Selected
            } else if hot == Hot::Place(index) {
                RowState::Hover
            } else {
                RowState::Idle
            };
            paint::row(inner, s, slot.rect, state);
            let side_icon = ctx.px(18);
            let icon = Rect::new(slot.rect.x + pad as i32, slot.rect.y + (slot.rect.h as i32 - side_icon as i32) / 2, side_icon, side_icon);
            draw_design_icon(&self.icons, s, icon, place_icon(&slot.path, &self.home));
            let shown = place_title(&slot.path, &self.home);
            let x = icon.right() + ctx.px(10) as i32;
            // Закреплённая папка (24.11) — с булавкой справа, как в Проводнике.
            let pin_w = if slot.pinned.is_some() { ctx.px(18) } else { 0 };
            paint::text_clipped(
                inner,
                s,
                Role::Body,
                x,
                paint::baseline(inner, Role::Body, slot.rect),
                (slot.rect.right() - x - pad as i32 - pin_w as i32).max(0) as u32,
                shown,
                paint::row_ink(inner, state),
            );
            if slot.pinned.is_some() {
                let cx = slot.rect.right() - pad as i32 - ctx.px(6) as i32;
                let cy = slot.rect.y + slot.rect.h as i32 / 2;
                draw_pin(s, ctx, cx, cy, p.ink4);
            }
            // Закреплённое место тянут: черта там, куда оно встанет.
            if let Some(drag) = self.pin_drag.as_ref().filter(|drag| drag.active) {
                if slot.pinned == Some(drag.to) && drag.to != drag.from {
                    let y = if drag.to < drag.from { slot.rect.y - 1 } else { slot.rect.bottom() };
                    s.fill(Rect::new(slot.rect.x, y, slot.rect.w, ctx.px(2)), p.acc);
                }
            }
        }
    }

    /// Список файлов (72g, макет 14): «Имя», «Дата изменения», «Тип»,
    /// «Размер»; значки — из макета, по типу файла.
    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.list.is_empty() {
            return;
        }
        // Узкое окно (20.11) не теряет столбцы, а листает таблицу вбок: она
        // раскладывается в свою ширину и сдвигается на прокрутку.
        let table = self.table_list(ctx, plan);
        let (date_x, type_x, size_x, size_right) = self.columns(ctx, table);
        // Шапка — после строк: строка, наполовину ушедшая вверх, уходит под неё.
        let header = |s: &mut Surface| self.draw_list_header(s, ctx, plan, table.x, [date_x, type_x, size_x], size_right);

        if self.draw_no_rows(s, ctx, plan) {
            header(s);
            return;
        }

        // Прокрутка (20.11) — смещение в точках; просьбу «показать выбранную»
        // исполняет раскладка: высоту вида знает только тот, кто рисует.
        let step = plan.row_h + plan.row_gap;
        let (view, content, _) = self.view_metrics(ctx, plan);
        self.scroll.sync(view.h, content, step);
        self.page_rows.set((view.h / step.max(1)).max(1) as usize);
        let at = self.scroll.offset();
        let pad = ctx.px(12);
        let hot = self.hot_now(ctx, plan);
        for index in self.scroll.rows(view.h, step, self.rows.len()) {
            let row = &self.rows[index];
            let rect = Rect::new(table.x + pad as i32, table.y + (index as u32 * step) as i32 - at, table.w.saturating_sub(pad * 2), plan.row_h);
            let selected = self.is_marked(index);
            let state = if selected {
                RowState::Selected
            } else if hot == Hot::Row(index) {
                RowState::Hover
            } else {
                RowState::Idle
            };
            paint::row(ctx, s, rect, state);

            let side = ctx.px(18);
            let icon = Rect::new(rect.x + ctx.px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            let (kind_icon, kind_name) = self.row_art(row);
            if !draw_design_icon(&self.icons, s, icon, kind_icon) {
                let (glyph, tone, filled) = row.icon();
                paint::icon_tile(ctx, s, icon, glyph, tone, filled);
            }

            let small = paint::baseline(ctx, Role::Body, rect);
            let size = if row.is_volume() {
                row.volume.clone()
            } else if row.directory {
                String::new()
            } else {
                human_size(row.size)
            };
            paint::text_right(ctx, s, Role::Body, size_right, small, &size, p.ink3);
            // Программа в `/bin` — «Приложение», что бы ни было в имени.
            let kind = if !row.directory && row.program && row.mode & 0o111 != 0 { "Приложение" } else { kind_name };
            paint::text_clipped(ctx, s, Role::Body, type_x, small, (size_x - ctx.px(10) as i32 - type_x).max(0) as u32, kind, p.ink3);
            paint::text_clipped(ctx, s, Role::Body, date_x, small, (type_x - ctx.px(10) as i32 - date_x).max(0) as u32, &date_text(row.mtime, self.offset), p.ink3);
            let right = date_x - ctx.px(12) as i32;

            let name_x = icon.right() + ctx.px(10) as i32;
            let room = (right - name_x).max(0) as u32;
            let ink = if selected { p.ink } else { p.ink2 };
            let shown = if row.is_volume() {
                volume_title(&row.name, &row.label)
            } else {
                self.shown_name(row).to_string()
            };
            paint::text_clipped(ctx, s, Role::Body, name_x, small, room, &shown, ink);
        }
        header(s);
    }

    /// Шапка таблицы: подписи столбцов (у столбца сортировки — стрелка) и
    /// линия под ними. Фон заливается: под шапку уходят строки.
    fn draw_list_header(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, left: i32, bounds: [i32; 3], size_right: i32) {
        let p = ctx.palette;
        if plan.header.is_empty() {
            return;
        }
        let [date_x, type_x, _] = bounds;
        s.fill(plan.header, theme::window_bg());
        let y = paint::baseline(ctx, Role::Caption, plan.header);
        // Столбец сортировки — со стрелкой (↑ по возрастанию, ↓ по убыванию).
        let arrow = if self.descending { " ↓" } else { " ↑" };
        let title = |key: SortKey| if key == self.sort && !self.on_computer() { format!("{}{arrow}", key.title()) } else { String::from(key.title()) };
        paint::text(ctx, s, Role::Caption, left + ctx.px(18) as i32, y, &title(SortKey::Name), p.ink4);
        paint::text(ctx, s, Role::Caption, date_x, y, &title(SortKey::Date), p.ink4);
        paint::text(ctx, s, Role::Caption, type_x, y, &title(SortKey::Kind), p.ink4);
        paint::text_right(ctx, s, Role::Caption, size_right, y, &title(SortKey::Size), p.ink4);
        // Границы столбцов (21.11): тонкая черта, под указателем и когда
        // тянут — ярче; за неё тянут ширину.
        if !self.on_computer() {
            let grip = self.col_drag.map(|(boundary, ..)| boundary).or(match self.hot_now(ctx, plan) {
                Hot::Grip(boundary) => Some(boundary),
                _ => None,
            });
            let h = plan.header.h.saturating_sub(ctx.px(14));
            for (index, x) in bounds.iter().enumerate() {
                let x = x - ctx.px(8) as i32;
                let (ink, w) = if grip == Some(index) { (p.acc, ctx.px(2)) } else { (p.line.color, 1) };
                let alpha = if grip == Some(index) { 255 } else { p.line.alpha };
                draw::blend_rect(s, Rect::new(x, plan.header.y + ctx.px(7) as i32, w, h), ink, alpha);
            }
        }
        draw::hline(
            s,
            plan.header.x + ctx.px(12) as i32,
            plan.header.bottom() - 1,
            plan.header.w.saturating_sub(ctx.px(24)),
            p.line.color,
            p.line.alpha,
        );
    }

    /// Содержимое открытого файла на месте списка.
    fn draw_preview(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, preview: &Preview) {
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        if area.is_empty() {
            return;
        }
        let step = u32::from(ctx.face(Role::Mono).line) + ctx.px(2);
        let pad = ctx.px(16);
        let room = area.w.saturating_sub(pad * 2);
        let note_h = if preview.note.is_empty() { 0 } else { step * 2 };
        let body = (area.h.saturating_sub(note_h) / step.max(1)) as usize;

        for (offset, line) in preview.lines.iter().skip(preview.scroll).take(body).enumerate() {
            paint::text_clipped(
                ctx,
                s,
                Role::Mono,
                area.x + pad as i32,
                area.y + (offset as u32 * step) as i32,
                room,
                line,
                p.ink3,
            );
        }
        if !preview.note.is_empty() {
            paint::text_clipped(
                ctx,
                s,
                Role::Caption,
                area.x + pad as i32,
                area.bottom() - step as i32,
                room,
                &preview.note,
                p.ink4,
            );
        }
    }

    /// Строка состояния: без неё стрелки и Enter — это то, что надо угадать.
    fn draw_status(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        if plan.status.is_empty() {
            return;
        }
        // Как в Проводнике (20.11, снимок Романа): «Элементов: 30 | Выбрано
        // 5 элем.: 1,90 МБ», справа — переключатели вида.
        let mut parts: Vec<String> = Vec::new();
        if self.preview.is_some() {
            parts.push(String::from("Стрелки — листать    Esc — назад к списку"));
        } else if self.props.is_some() {
            parts.push(String::from("Esc — назад к списку"));
        } else if self.error.is_some() {
            parts.push(String::from("Стрелка влево — назад    Backspace — вверх"));
        } else if self.on_computer() {
            parts.push(format!("Томов: {}", self.volume_count()));
        } else {
            parts.push(kit::items_text(self.rows.len()));
            let targets = self.targets();
            if !targets.is_empty() {
                let bytes: u64 = targets.iter().filter_map(|index| self.rows.get(*index)).filter(|row| !row.directory).map(|row| row.size).sum();
                parts.push(kit::selected_text(targets.len(), bytes));
            }
            if self.dropped != 0 {
                parts.push(format!("показаны не все: ещё {}", self.dropped));
            }
        }
        if let Some(note) = self.note.as_ref() {
            parts.push(note.clone());
        }
        let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
        kit::status_line(ctx, s, plan.status, &parts, "", plan.view.len());
        // Вид — кнопки справа (макет 14): таблица, «Список», плитка.
        for (rect, mode) in plan.view.iter().zip(ViewMode::ALL) {
            if !rect.is_empty() {
                kit::status_toggle(ctx, s, *rect, mode.icon(), mode == self.view);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Клавиши, у которых символ есть
// ---------------------------------------------------------------------------

/// Enter. Договор отдаёт символ раньше имени, и у этой клавиши он есть.
const ENTER: u32 = '\n' as u32;
/// Backspace — `0x08`, его собственный код в ASCII.
const BACKSPACE: u32 = 0x08;
/// Escape — `0x1B`, тоже собственный.
const ESCAPE: u32 = 0x1B;
/// `M` — меню выбранной строки, для клавиатур без клавиши меню.
const MENU: u32 = 'm' as u32;
/// `T` — плитка или таблица.
const TILES: u32 = 't' as u32;
/// `L` — «Список» или таблица (21.11).
const LIST: u32 = 'l' as u32;
/// Ответы на вопрос об удалении.
const YES: u32 = 'y' as u32;
const NO: u32 = 'n' as u32;
/// Подсказки под полем имени и под вопросом.
const RENAME_HINT: &str = "Enter — переименовать    Esc — отмена    Ctrl+U — очистить";

/// Обновить список. `r` — потому что F-ряд договор программам не отдаёт.
const REFRESH: u32 = 'r' as u32;

/// Переключить знакомый вид на настоящие имена и обратно.
const VIEW: u32 = 'v' as u32;

/// На сколько строк прокручивает просмотр страница.
const PREVIEW_PAGE: usize = 20;

// ---------------------------------------------------------------------------
// Раскладка
// ---------------------------------------------------------------------------

/// Где что лежит в окне менеджера.
///
/// Все прямоугольники — в координатах поверхности окна, те же, в которых
/// приходит щелчок.
/// Сетка плиток.
struct Grid {
    cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
    margin: u32,
}

/// Раскладка вида «Список»: область, поля, шаг и высота строки, зазор
/// между столбцами, строк в столбце, ширина столбца и всего содержимого.
struct ListPlan {
    area: Rect,
    pad: i32,
    top: i32,
    step: u32,
    row_h: u32,
    gap: u32,
    per_col: usize,
    col_w: u32,
    content: u32,
}

struct Plan {
    /// Полоса вкладок над панелью (20.11).
    tabs: Rect,
    toolbar: Rect,
    /// «Назад», «вперёд», «вверх» — в этом порядке.
    nav: [Rect; 3],
    path: Rect,
    /// «Обновить» и поле поиска (72g).
    refresh: Rect,
    search: Rect,
    /// Панель команд под адресом.
    commands: Vec<(Command, Rect)>,
    /// Лунка переключателя вида и его кнопки: таблица, «Список», плитка (в
    /// строке состояния справа).
    view_box: Rect,
    view: [Rect; 3],
    side: Option<Rect>,
    header: Rect,
    list: Rect,
    status: Rect,
    row_h: u32,
    row_gap: u32,
}

impl Plan {
    /// Сетка плиток в области списка.
    fn grid(&self, ctx: Ctx) -> Grid {
        let area = self.list.union(&self.header);
        let margin = ctx.px(theme::ICON_MARGIN) / 2;
        let cell_w = ctx.px(theme::ICON_CELL_W) + ctx.px(theme::ICON_GAP);
        let cell_h = ctx.px(theme::ICON_CELL_H) + ctx.px(theme::ICON_GAP);
        let cols = (area.w.saturating_sub(margin * 2) / cell_w.max(1)) as usize;
        let rows = (area.h.saturating_sub(margin * 2) / cell_h.max(1)) as usize;
        Grid { cols, rows, cell_w, cell_h, margin }
    }

    /// Отступ первого ряда плиток от верха их области.
    fn tiles_top(&self, grid: &Grid) -> u32 {
        let area = self.list.union(&self.header);
        ((self.list.y - area.y).max(0) as u32).max(grid.margin)
    }

    /// Плитка с таким номером при смещении прокрутки `scroll` (20.11: ряды
    /// едут по точкам; видна ли плитка, решает тот, кто рисует).
    fn tile_rect(&self, grid: &Grid, index: usize, scroll: i32) -> Option<Rect> {
        if grid.cols == 0 {
            return None;
        }
        let area = self.list.union(&self.header);
        let row = index / grid.cols;
        let col = index % grid.cols;
        Some(Rect::new(
            area.x + (grid.margin + col as u32 * grid.cell_w) as i32,
            area.y + (self.tiles_top(grid) + row as u32 * grid.cell_h) as i32 - scroll,
            grid.cell_w.saturating_sub(ctx_gap()),
            grid.cell_h.saturating_sub(ctx_gap()),
        ))
    }

    /// Строка списка номер `index` от начала при смещении прокрутки `scroll`.
    fn row_rect_at(&self, ctx: Ctx, index: usize, scroll: i32) -> Rect {
        let pad = ctx.px(12);
        Rect::new(
            self.list.x + pad as i32,
            self.list.y + (index as u32 * (self.row_h + self.row_gap)) as i32 - scroll,
            self.list.w.saturating_sub(pad * 2),
            self.row_h,
        )
    }

    /// Строка списка с таким номером сверху.
    fn row_rect(&self, ctx: Ctx, offset: usize) -> Rect {
        let pad = ctx.px(12);
        Rect::new(
            self.list.x + pad as i32,
            self.list.y + (offset as u32 * (self.row_h + self.row_gap)) as i32,
            self.list.w.saturating_sub(pad * 2),
            self.row_h,
        )
    }
}

/// Показывать ли строку состояния (24.11, «Параметры папок»): раскладку
/// спрашивают из многих мест, и признак у неё — общий на окно.
static STATUS_BAR: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(true);

/// Посчитать раскладку окна.
///
/// Одна функция на отрисовку и на щелчок — см. заголовок модуля.
fn layout(ctx: Ctx, area: Rect) -> Plan {
    // Полоса вкладок над панелью (20.11), как у Проводника Windows 11.
    let strip_h = ctx.px(kit::TAB_STRIP_H).min(area.h);
    let tabs = Rect::new(area.x, area.y, area.w, strip_h);
    let area = Rect::new(area.x, tabs.bottom(), area.w, area.h.saturating_sub(strip_h));
    let pad = ctx.px(kit::PAD);
    let toolbar_h = ctx.px(theme::TOOLBAR_H).min(area.h);
    let toolbar = Rect::new(area.x, area.y, area.w, toolbar_h);

    let btn = ctx.px(30);
    let gap = ctx.px(6);
    let btn_y = toolbar.y + (toolbar_h as i32 - btn as i32) / 2;
    let mut x = area.x + pad as i32;
    let mut nav = [Rect::EMPTY; 3];
    for slot in &mut nav {
        *slot = Rect::new(x, btn_y, btn, btn);
        x += (btn + gap) as i32;
    }
    let refresh = Rect::new(x, btn_y, btn, btn);
    x += (btn + gap) as i32;

    let path_h = ctx.px(32);
    let path_x = x + ctx.px(4) as i32;
    let field_y = toolbar.y + (toolbar_h as i32 - path_h as i32) / 2;

    // Поиск справа; на узком окне его нет — адрес важнее.
    let (search, path_right) = if area.w > ctx.px(560) {
        let width = ctx.px(220);
        let x = area.right() - pad as i32 - width as i32;
        (Rect::new(x, field_y, width, path_h), x - ctx.px(8) as i32)
    } else {
        (Rect::EMPTY, area.right() - pad as i32)
    };
    let path = Rect::new(path_x, field_y, (path_right - path_x).max(0) as u32, path_h);

    // Панель команд: «Создать папку», «Переименовать», «Удалить»,
    // «Свойства».
    let command_h = ctx.px(theme::COMMAND_H).min(area.h.saturating_sub(toolbar_h));
    let mut commands = Vec::new();
    if command_h > 0 {
        let face = ctx.face(Role::Body);
        let mut cx = area.x + pad as i32 - ctx.px(6) as i32;
        let cy = toolbar.bottom() + ctx.px(4) as i32;
        let mut group = 0;
        for command in Command::ALL {
            // Между группами — место под черту.
            if command.group() != group {
                group = command.group();
                cx += ctx.px(13) as i32;
            }
            let label = command.look().1;
            let width = if label.is_empty() { ctx.px(32) } else { ctx.px(40) + face.width(label) };
            let rect = Rect::new(cx, cy, width, command_h.saturating_sub(ctx.px(8)));
            if rect.right() > area.right() - pad as i32 {
                break;
            }
            commands.push((command, rect));
            cx += (width + ctx.px(2)) as i32;
        }
    }

    // Строку состояния можно убрать (24.11, «Параметры папок»).
    let status_h = if STATUS_BAR.load(core::sync::atomic::Ordering::Relaxed) { ctx.px(theme::STATUS_H) } else { 0 };
    let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
    let status = Rect::new(area.x, status_y, area.w, status_h);
    // Переключатели вида — места строки состояния единого вида (`kit`).
    let toggles = kit::status_toggles(ctx, status, 3);
    let view = [toggles[0], toggles[1], toggles[2]];

    let top = toolbar.bottom() + command_h as i32;
    let bottom = status.y;
    let body_h = (bottom - top).max(0) as u32;

    // Колонка есть только там, где после неё остаётся окно, а не щель.
    let side = if area.w > ctx.px(SIDE_FROM) && body_h > 0 {
        Some(Rect::new(area.x, top, ctx.px(theme::SIDE_W), body_h))
    } else {
        None
    };
    let body_x = side.map_or(area.x, |side| side.right());
    let body_w = (area.right() - body_x).max(0) as u32;

    let header_h = (u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(14)).min(body_h);
    let header = Rect::new(body_x, top, body_w, header_h);
    let list = Rect::new(
        body_x,
        top + header_h as i32,
        body_w,
        body_h.saturating_sub(header_h),
    );

    Plan {
        tabs,
        toolbar,
        nav,
        path,
        refresh,
        search,
        commands,
        view_box: Rect::EMPTY,
        view,
        side,
        header,
        list,
        status,
        row_h: ctx.px(32),
        row_gap: ctx.px(2),
    }
}

/// Кнопки панели команд «Файлов» (макет 14): «Создать» | вырезать,
/// копировать, вставить, переименовать, удалить — значками | «Сортировка»,
/// «Вид».
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    NewFolder,
    Cut,
    Copy,
    Paste,
    Rename,
    Delete,
    Sort,
    View,
}

impl Command {
    const ALL: [Command; 8] =
        [Command::NewFolder, Command::Cut, Command::Copy, Command::Paste, Command::Rename, Command::Delete, Command::Sort, Command::View];

    /// Значок и подпись (у кнопок-значков подписи нет — она в подсказке).
    const fn look(self) -> (Fi, &'static str) {
        match self {
            Command::NewFolder => (Fi::Plus, "Создать"),
            Command::Cut => (Fi::Cut, ""),
            Command::Copy => (Fi::Copy, ""),
            Command::Paste => (Fi::Paste, ""),
            Command::Rename => (Fi::Rename, ""),
            Command::Delete => (Fi::Trash, ""),
            Command::Sort => (Fi::List, "Сортировка"),
            Command::View => (Fi::Apps, "Вид"),
        }
    }

    /// Группа: между группами — черта.
    const fn group(self) -> u8 {
        match self {
            Command::NewFolder => 0,
            Command::Sort | Command::View => 2,
            _ => 1,
        }
    }
}

/// Поле подписи звена адреса слева и справа и ширина его шеврона (21.11:
/// у каждого — своя подсветка, как в Проводнике).
const CRUMB_PAD: u32 = 8;
const CRUMB_CHEVRON: u32 = 22;
/// Сколько подпапок показывает список у шеврона.
const CRUMB_POP_MAX: usize = 24;

/// Звенья адреса: подпись и куда ведёт (72g). Дом — «Главная», корень —
/// «Этот компьютер › Системный диск».
fn breadcrumb(path: &str, home: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if path == COMPUTER {
        out.push((String::from("Этот компьютер"), String::from(COMPUTER)));
        return out;
    }
    // «Сеть › HOST › папка › …» (24.11).
    if let Some(rest) = path.strip_prefix(sysconf::winpath::NET).filter(|rest| rest.is_empty() || rest.starts_with('/')) {
        out.push((String::from("Сеть"), String::from(sysconf::winpath::NET)));
        let mut current = String::from(sysconf::winpath::NET);
        for part in rest.split('/').filter(|part| !part.is_empty()) {
            current = format!("{current}/{part}");
            out.push((part.to_string(), current.clone()));
        }
        return out;
    }
    let (mut current, rest) = if path == home || path.starts_with(&format!("{home}/")) {
        out.push((String::from("Главная"), home.to_string()));
        (home.to_string(), &path[home.len()..])
    } else {
        out.push((String::from("Этот компьютер"), String::from(COMPUTER)));
        out.push((String::from("Системный диск"), String::from("/")));
        (String::new(), path)
    };
    for part in rest.split('/').filter(|part| !part.is_empty()) {
        current = format!("{current}/{part}");
        let label = match part {
            "Desktop" => "Рабочий стол",
            "Downloads" => "Загрузки",
            "Documents" => "Документы",
            "Pictures" => "Изображения",
            "Music" => "Музыка",
            "Videos" => "Видео",
            _ => sysconf::winpath::label(&current).unwrap_or(part),
        };
        out.push((label.to_string(), current.clone()));
    }
    out
}

/// Кнопка навигации: доступная — с подложкой, недоступная — одним значком.
///
/// Рисуется здесь, а не через [`paint::icon_button`]: у той кнопки состояния
/// «под указателем», а у этой — «есть куда идти», и цвет значка в них меняется
/// по-разному.
fn nav_button(ctx: Ctx, s: &mut Surface, rect: Rect, icon: Icon, enabled: bool) {
    let p = ctx.palette;
    let r = ctx.px(theme::R_CHIP);
    if enabled {
        draw::rounded(s, rect, r, ctx.flat(p.ghost), 255);
        draw::rounded_stroke(s, rect, r, p.line2.color, p.line2.alpha);
    }
    // Та же доля, что у значка в кнопке заголовка: 11 точек в кнопке 30.
    let side = rect.w.min(rect.h) * 11 / 30;
    let x = rect.x + (rect.w as i32 - side as i32) / 2;
    let y = rect.y + (rect.h as i32 - side as i32) / 2;
    let ink = if enabled { p.ink3 } else { p.ink6 };
    glyphicon::draw(s, icon, x, y, side, ink, 255);
}

/// Строка быстрого доступа вместе с местом, которое она занимает.
struct PlaceSlot {
    /// Заголовок группы над строкой, если строка её открывает.
    head: Option<&'static str>,
    head_y: i32,
    path: String,
    rect: Rect,
    /// Закреплённая папка (24.11): её номер в списке закреплённых.
    pinned: Option<usize>,
}

/// Папки дома, закреплённые по умолчанию (24.11): те из «Рабочего стола»,
/// «Загрузок», «Документов», «Изображений», «Музыки» и «Видео», что есть.
fn default_pins(home: &str) -> Vec<String> {
    ["Desktop", "Downloads", "Documents", "Pictures", "Music", "Videos"]
        .iter()
        .map(|name| format!("{home}/{name}"))
        .filter(|path| is_directory(path))
        .collect()
}

/// Куда ведёт быстрый доступ.
///
/// Как в Проводнике (72g, макет 14; 24.11 — по снимку Романа): «Главная» |
/// закреплённые папки (по умолчанию — папки дома; закрепляют и открепляют
/// из меню, порядок — перетаскиванием) | «Этот компьютер», под ним диски (с
/// отступом), «Корзина». Закреплённая папка, которой нет, не показывается.
fn places(home: &str, pinned: &[String]) -> Vec<(Option<&'static str>, String, Option<usize>)> {
    let mut out = alloc::vec![(None, home.to_string(), None)];
    let mut first = true;
    for (index, path) in pinned.iter().enumerate() {
        if is_directory(path) {
            out.push((if first { Some("—") } else { None }, path.clone(), Some(index)));
            first = false;
        }
    }
    out.push((Some("—"), String::from(COMPUTER), None));
    out.push((None, String::from("/"), None));
    for disk in USB_PLACES {
        let mut info = Stat::default();
        if stat(disk, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
            out.push((None, String::from(disk), None));
        }
    }
    // «Сеть» (24.11), as in Explorer's navigation pane.
    if is_directory(sysconf::winpath::NET) {
        out.push((None, String::from(sysconf::winpath::NET), None));
    }
    out.push((None, format!("{home}/.local/share/Trash/files"), None));
    out
}

/// Диски в боковой колонке под «Этим компьютером»: данные и съёмные
/// (`/usb`, `/usb1`… — второй накопитель, например exFAT-диск, 20.11).
const USB_PLACES: [&str; 5] = ["/data", "/usb", "/usb1", "/usb2", "/usb3"];

/// Подпись места в боковой колонке (72g).
fn place_title<'a>(path: &'a str, home: &str) -> &'a str {
    if path == home {
        return "Главная";
    }
    match path.rsplit('/').next().unwrap_or(path) {
        _ if path == COMPUTER => "Этот компьютер",
        _ if path == "/" => "Системный диск",
        _ if path == "/usb" => "USB-накопитель",
        _ if path == "/usb1" => "USB-накопитель 2",
        _ if path == "/usb2" => "USB-накопитель 3",
        _ if path == "/usb3" => "USB-накопитель 4",
        _ if path == "/data" => "Данные",
        _ if path == sysconf::winpath::NET => "Сеть",
        _ if path.ends_with("/.local/share/Trash/files") => "Корзина",
        "Desktop" => "Рабочий стол",
        "Downloads" => "Загрузки",
        "Documents" => "Документы",
        "Pictures" => "Изображения",
        "Music" => "Музыка",
        "Videos" => "Видео",
        // Закреплённая папка (24.11) — её имя.
        last => last,
    }
}

/// Значок места в боковой колонке (72g).
fn place_icon(path: &str, home: &str) -> &'static str {
    if path == home {
        return "folder";
    }
    match path {
        COMPUTER => "place-this-pc",
        "/" => "place-system-disk",
        "/usb" | "/usb1" | "/usb2" | "/usb3" => "place-usb-stick",
        "/data" => "place-local-disk",
        sysconf::winpath::NET => "network",
        _ if path.ends_with("/.local/share/Trash/files") => {
            if dir_has_entries(path) { "place-trash-full" } else { "place-trash-empty" }
        }
        _ => file_kind(path.rsplit('/').next().unwrap_or(path), true).0,
    }
}

/// Имя тома на странице «Этот компьютер»: у съёмного с меткой — как в
/// Проводнике, «Z7S (/usb1)».
fn volume_title(point: &str, label: &str) -> String {
    match point {
        "/" => String::from("Системный диск"),
        sysconf::winpath::NET => String::from("Сеть"),
        "/data" => String::from("Данные (/data)"),
        other if !label.is_empty() => format!("{label} ({other})"),
        other if other.starts_with("/usb") => format!("USB-накопитель ({other})"),
        other => other.to_string(),
    }
}

/// Булавка закреплённой папки (24.11) с центром в `(cx, cy)`: головка,
/// упор и игла.
fn draw_pin(s: &mut Surface, ctx: Ctx, cx: i32, cy: i32, ink: mini_ui::Color) {
    let r = ctx.px(3);
    let head = cy - ctx.px(3) as i32;
    draw::circle(s, cx, head, r, ink, 255);
    draw::hline(s, cx - ctx.px(4) as i32, head + r as i32, ctx.px(9), ink, 255);
    draw::vline(s, cx, head + r as i32, ctx.px(6), ink, 255);
}

/// Прямоугольник рамки выделения между двумя точками.
fn band_rect(a: (i32, i32), b: (i32, i32)) -> Rect {
    Rect::new(a.0.min(b.0), a.1.min(b.1), (a.0 - b.0).unsigned_abs().max(1), (a.1 - b.1).unsigned_abs().max(1))
}

/// Том, на котором лежит `path`: самая длинная точка монтирования, которой
/// путь начинается.
fn mount_of<'a>(points: &'a [String], path: &str) -> Option<&'a str> {
    points
        .iter()
        .filter(|point| *point == "/" || path == point.as_str() || path.starts_with(&format!("{point}/")))
        .max_by_key(|point| point.len())
        .map(String::as_str)
}

/// Точки монтирования томов (`SYS_MOUNTS`, первое поле строки).
fn mount_points() -> Vec<String> {
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(MOUNTS_LIMIT).is_err() {
        return Vec::new();
    }
    buffer.resize(MOUNTS_LIMIT, 0u8);
    let got = mounts(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("");
    text.lines().filter_map(|line| line.split('\t').next()).filter(|point| !point.is_empty()).map(String::from).collect()
}

/// Тома — из ядра, по строке на каждый.
fn list_volumes() -> Vec<Row> {
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(MOUNTS_LIMIT).is_err() {
        return Vec::new();
    }
    buffer.resize(MOUNTS_LIMIT, 0u8);
    let got = mounts(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("");
    text.lines()
        .filter_map(|line| {
            // `<точка>\t<ФС>[\t<всего>\t<свободно>]` (место — с 02.11).
            let mut fields = line.split('\t');
            let point = fields.next()?;
            let kind = fields.next()?;
            let total = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            let free = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            let mut row = Row::volume(point, kind, total, free);
            // Пятое поле — метка тома (20.11).
            row.label = fields.next().unwrap_or("").to_string();
            Some(row)
        })
        .fold(Vec::new(), |mut out: Vec<Row>, row| {
            // Не диски (21.11): тома в памяти, которые ядро заводит живой
            // системе под `/tmp`, `/root`, `/etc`, `/opt`, `/var` (метки
            // `scratch-*`, `live-*`), — «Этот компьютер» показывал их
            // тремя-пятью «дисками»; и один раздел состояния, смонтированный
            // ветками (`/etc`, `/home`, `/var`…), — это один том, а не пять.
            if row.label.starts_with("scratch-") || row.label.starts_with("live-") {
                return out;
            }
            let same = |other: &Row| other.size != 0 && other.size == row.size && other.free == row.free && other.volume == row.volume && other.label == row.label;
            if !out.iter().any(same) {
                out.push(row);
            }
            out
        })
}

/// «Этот компьютер»: the disks, then «Сеть» (24.11) — `/net` is not a disk
/// tile but the network place, last, under «Сетевые расположения».
fn computer_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = list_volumes().into_iter().filter(|row| row.name != sysconf::winpath::NET).collect();
    if is_directory(sysconf::winpath::NET) {
        rows.push(Row::volume(sysconf::winpath::NET, "SMB", 0, 0));
    }
    rows
}

/// Время работы в виде `Ч:ММ:СС`.
fn uptime_text(ms: u64) -> String {
    let seconds = ms / 1000;
    format!("{}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}

/// Что означает код отказа — словами.
fn error_text(code: i64) -> String {
    match code {
        user_abi::ERR_NOT_FOUND => String::from("нет такого файла"),
        user_abi::ERR_PERMISSION => String::from("нет прав"),
        user_abi::ERR_EXISTS => String::from("такое имя уже есть"),
        user_abi::ERR_UNSUPPORTED => String::from("этот том не умеет записи"),
        user_abi::ERR_NOT_EMPTY => String::from("папка не пуста"),
        user_abi::ERR_NO_SPACE => String::from("нет места"),
        _ => format!("код {code}"),
    }
}

/// Зазор между плитками.
fn ctx_gap() -> u32 {
    theme::ICON_GAP
}

/// Разложить быстрый доступ по колонке.
fn place_slots(ctx: Ctx, side: Rect, home: &str, pinned: &[String]) -> Vec<PlaceSlot> {
    let padx = ctx.px(16);
    let row_h = ctx.px(32);
    let gap = ctx.px(2);
    let caps_h = ctx.px(14);
    let mut y = side.y + ctx.px(10) as i32;
    let mut out = Vec::new();

    for (head, path, pin) in places(home, pinned) {
        let head_y = if head.is_some() {
            let at = y + ctx.px(4) as i32;
            y += caps_h as i32;
            at
        } else {
            0
        };
        // Диски — под «Этим компьютером», с отступом.
        let indent = if matches!(path.as_str(), "/" | "/data" | "/usb" | "/usb1" | "/usb2" | "/usb3") { ctx.px(16) } else { 0 };
        let rect = Rect::new(
            side.x + (padx + indent) as i32,
            y,
            side.w.saturating_sub(padx * 2 + indent),
            row_h,
        );
        y += (row_h + gap) as i32;
        // Не поместилось — не рисуем: строка, наполовину заехавшая под список,
        // выглядит как испорченная отрисовка, а не как «здесь кончилось место».
        if rect.bottom() > side.bottom() {
            break;
        }
        out.push(PlaceSlot { head, head_y, path, rect, pinned: pin });
    }
    out
}

// ---------------------------------------------------------------------------
// Файловая система
// ---------------------------------------------------------------------------

/// Сколько указатель стоит на строке, прежде чем появится подсказка.
const TIP_DELAY_MS: u64 = 700;

/// Сколько записей обходит подсказка папки — дальше «более N».
const TIP_WALK_LIMIT: usize = 3000;

/// Что лежит в папке (24.11, подсказка и «Свойства»): байт, файлов,
/// папок, обошли ли всё и первые имена.
#[derive(Default)]
struct Summary {
    bytes: u64,
    files: u64,
    dirs: u64,
    complete: bool,
    first_dirs: Vec<String>,
    first_files: Vec<String>,
}

/// Записи одного каталога: имя, папка ли, размер (без «.» и «..»).
fn dir_entries(path: &str) -> Option<Vec<(String, bool, u64)>> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut out = Vec::new();
    let mut entry = Dirent::default();
    while readdir_raw(fd, &mut entry) > 0 {
        let len = (entry.name_len as usize).min(entry.name.len());
        let name = String::from_utf8_lossy(&entry.name[..len]).into_owned();
        if name == "." || name == ".." {
            continue;
        }
        out.push((name, entry.kind == KIND_DIRECTORY, entry.size));
    }
    close(fd);
    Some(out)
}

/// Обойти папку не дальше `limit` записей.
fn folder_summary(path: &str, limit: usize) -> Summary {
    let mut out = Summary { complete: true, ..Summary::default() };
    let mut stack = alloc::vec![(String::from(path), 0u32)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Some(entries) = dir_entries(&dir) else { continue };
        for (name, directory, size) in entries {
            seen += 1;
            if seen > limit {
                out.complete = false;
                return out;
            }
            if directory {
                out.dirs += 1;
                if depth == 0 && out.first_dirs.len() < 4 {
                    out.first_dirs.push(name.clone());
                }
                if depth < 32 {
                    stack.push((join(&dir, &name), depth + 1));
                }
            } else {
                out.files += 1;
                out.bytes += size;
                if depth == 0 && out.first_files.len() < 4 {
                    out.first_files.push(name);
                }
            }
        }
    }
    out
}

/// Число с пробелами между разрядами: «2 048».
fn group_digits(value: u64) -> String {
    let digits = format!("{value}");
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// «23 октября 2026 г., 22:47».
fn long_date(mtime: u32, offset: i32) -> String {
    const MONTHS: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    let local = calendar::DateTime::from_unix(i64::from(mtime) + i64::from(offset) * 60);
    let month = MONTHS.get(usize::from(local.month).saturating_sub(1)).copied().unwrap_or("");
    format!("{} {month} {} г., {:02}:{:02}", local.day, local.year, local.hour, local.minute)
}

/// Имя по номеру из `/etc/passwd` (`/etc/group` для групп).
fn name_of_id(file: &str, id: u32) -> String {
    if id == 0 {
        return String::from("root");
    }
    let text = read_small_limit(file, 64 * 1024).unwrap_or_default();
    text.lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let number: u32 = fields.nth(1)?.parse().ok()?;
            (number == id).then(|| name.to_string())
        })
        .unwrap_or_else(|| format!("{id}"))
}

fn user_name(uid: u32) -> String {
    name_of_id("/etc/passwd", uid)
}

fn group_name(gid: u32) -> String {
    name_of_id("/etc/group", gid)
}

/// Права строкой, как у `ls -l`: «-rw-r--r--».
fn rwx(mode: u32, directory: bool) -> String {
    let mut out = String::from(if directory { "d" } else { "-" });
    for shift in [6u32, 3, 0] {
        let bits = (mode >> shift) & 7;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Свободное имя в папке для копии: «Отчёт.txt», иначе «Отчёт — копия.txt»,
/// «Отчёт — копия (2).txt».
fn unique_name(dir: &str, name: &str) -> String {
    let first = join(dir, name);
    if !exists(&first) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(at) if at > 0 => (&name[..at], &name[at..]),
        _ => (name, ""),
    };
    for attempt in 1..1000u32 {
        let candidate = if attempt == 1 { format!("{stem} — копия{ext}") } else { format!("{stem} — копия ({attempt}){ext}") };
        let path = join(dir, &candidate);
        if !exists(&path) {
            return path;
        }
    }
    first
}

/// Разложить копирование `src` в `dst` на шаги: папки — заведением и
/// содержимым, файлы — с размером.
fn expand(src: &str, dst: &str, ops: &mut Vec<Op>, depth: u32) {
    let mut info = Stat::default();
    if stat(src, &mut info) < 0 {
        return;
    }
    if info.kind == KIND_DIRECTORY {
        ops.push(Op::Dir(dst.to_string()));
        if depth > 64 {
            return;
        }
        if let Ok((rows, _)) = list_dir(src) {
            for row in rows {
                expand(&join(src, &row.name), &join(dst, &row.name), ops, depth + 1);
            }
        }
    } else {
        ops.push(Op::File { src: src.to_string(), dst: dst.to_string(), size: info.size });
    }
}

/// Собрать путь к записи внутри каталога.
fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Каталог, в котором лежит этот путь.
fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => path[..index].to_string(),
    }
}


/// Домашний каталог: `/root` у нулевого uid, иначе каталог в `/home`,
/// принадлежащий этому uid (`/home`, если такого нет).
fn home_dir() -> String {
    let me = user_progs::uid();
    if me == 0 {
        return String::from("/root");
    }
    if let Ok((rows, _)) = list_dir("/home") {
        for row in rows {
            let path = join("/home", &row.name);
            let mut info = Stat::default();
            if stat(&path, &mut info) >= 0 && info.kind == KIND_DIRECTORY && info.uid == me {
                return path;
            }
        }
    }
    String::from("/home")
}

fn exists(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0
}

/// Есть ли в каталоге хоть одна запись, кроме «.» и «..».
fn dir_has_entries(path: &str) -> bool {
    let fd = open(path);
    if fd < 0 {
        return false;
    }
    let mut entry = Dirent::default();
    let mut found = false;
    while readdir_raw(fd, &mut entry) > 0 {
        let len = (entry.name_len as usize).min(entry.name.len());
        if &entry.name[..len] != b"." && &entry.name[..len] != b".." {
            found = true;
            break;
        }
    }
    close(fd);
    found
}

/// Создать каталог вместе с недостающими родителями.
fn make_dirs(path: &str) -> Result<(), i64> {
    let mut built = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        built.push('/');
        built.push_str(part);
        if exists(&built) {
            continue;
        }
        let code = mkdir(&built, 0o755);
        if code < 0 && code != user_abi::ERR_EXISTS {
            return Err(code);
        }
    }
    Ok(())
}

/// Записать маленький файл целиком.
fn write_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let fd = open_write(path, true, true);
    if fd < 0 {
        return Err(fd);
    }
    let code = write(fd, data);
    close(fd);
    if code < 0 { Err(code) } else { Ok(()) }
}

/// Прочитать текстовый файл до `limit` байт.
fn read_small_limit(path: &str, limit: usize) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = Vec::new();
    if buffer.try_reserve_exact(limit).is_err() {
        close(fd);
        return None;
    }
    buffer.resize(limit, 0u8);
    let mut got = 0usize;
    while got < limit {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    String::from_utf8(buffer[..got].to_vec()).ok()
}

/// Прочитать маленький текстовый файл (до 4 КиБ).
fn read_small(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = [0u8; 4096];
    let got = read(fd, &mut buffer);
    close(fd);
    if got <= 0 {
        return None;
    }
    core::str::from_utf8(&buffer[..got as usize]).ok().map(String::from)
}

/// Стереть файл или каталог со всем содержимым — только в корзине.
fn remove_tree(path: &str, depth: u32) -> i64 {
    let mut info = Stat::default();
    if stat(path, &mut info) < 0 {
        return user_abi::ERR_NOT_FOUND;
    }
    if info.kind == KIND_DIRECTORY && depth < 64 {
        if let Ok((rows, _)) = list_dir(path) {
            for row in rows {
                let code = remove_tree(&join(path, &row.name), depth + 1);
                if code < 0 {
                    return code;
                }
            }
        }
    }
    remove(path)
}

/// Путь в процентной записи `.trashinfo` (freedesktop).
fn percent_encode(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Откуда удалено — `Path=` из `.trashinfo`.
fn trash_origin(text: &str) -> Option<String> {
    let value = text.lines().find_map(|line| line.trim().strip_prefix("Path="))?.trim();
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            out.push(u8::from_str_radix(value.get(at + 1..at + 3)?, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    let path = String::from_utf8(out).ok()?;
    path.starts_with('/').then_some(path)
}

/// Прочитать каталог. Возвращает строки и сколько записей не поместилось.
///
/// Ошибка — текстом, а не кодом: показывать её человеку всё равно строкой, а
/// перевод кода в слова в одном месте лучше, чем в трёх.
fn list_dir(path: &str) -> Result<(Vec<Row>, usize), String> {
    list_dir_code(path).map_err(|(code, opening)| {
        if opening { format!("cannot open {path}: error {code}") } else { format!("cannot read {path}: error {code}") }
    })
}

/// [`list_dir`] with the refusal as a code (and whether it was the open):
/// `ERR_PERMISSION` under `/net` means «ask for a password» (24.11).
fn list_dir_code(path: &str) -> Result<(Vec<Row>, usize), (i64, bool)> {
    let fd = open(path);
    if fd < 0 {
        return Err((fd, true));
    }
    let program = path == "/bin" || path.ends_with("/bin");
    let mut rows = Vec::new();
    let mut dropped = 0usize;
    let mut entry = Dirent::default();

    loop {
        let step = readdir_raw(fd, &mut entry);
        if step == 0 {
            break;
        }
        if step < 0 {
            close(fd);
            return Err((step, false));
        }
        // Имя пришло из-за границы доверия: длина — поле структуры, а байты —
        // содержимое носителя. И то и другое проверяется, а не берётся на веру:
        // длина за пределом массива увела бы срез в чужую память, а не-UTF-8
        // прошёл бы в отрисовку и вышел мусором на экране.
        let len = (entry.name_len as usize).min(entry.name.len());
        let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
            continue;
        };
        // «.» и «..» приходят от ext2 как настоящие записи. Своя навигация уже
        // есть (Backspace), а две строки, ведущие «сюда же» и «наверх», в
        // списке только мешают.
        if name == "." || name == ".." {
            continue;
        }
        if rows.len() >= MAX_ROWS {
            dropped += 1;
            continue;
        }
        rows.push(Row::entry(name, entry.kind == KIND_DIRECTORY, program, &entry));
    }

    close(fd);
    Ok((rows, dropped))
}

/// Командная строка, которой запускается .NET-программа, если `path` — она.
///
/// Признак — `<имя>.runtimeconfig.json` рядом: `dotnet build` оставляет его у
/// каждой программы и не оставляет у библиотек, так что `.dll` без него —
/// библиотека, запускать которую нечего. У `.exe` вдобавок обязана лежать
/// `<имя>.dll`: `.exe` из `dotnet build` — машинный код Windows, который
/// запускает ту самую `.dll`, и исполнять его здесь нечем, а сама `.dll` —
/// можно.
///
/// Путь с пробелом не запускается, и это названный предел: ядро делит
/// командную строку по пробелам, и программа получила бы полпути. Такой файл
/// откроется на просмотр, как любой другой.
fn dotnet_command(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    if !(lower.ends_with(".dll") || lower.ends_with(".exe")) || path.contains(' ') {
        return None;
    }
    let stem = &path[..path.len() - 4];
    let dll = format!("{stem}.dll");
    let mut info = Stat::default();
    if stat(&format!("{stem}.runtimeconfig.json"), &mut info) < 0 {
        return None;
    }
    if stat(&dll, &mut info) < 0 || info.kind == KIND_DIRECTORY {
        return None;
    }
    Some(format!("/bin/dotnet {dll}"))
}

/// Запуск Proton из пакета `proton` (`xtask/src/proton.rs`).
const PROTON: &str = "/opt/proton/bin/fossai-proton";
/// Запуск Wine из пакета `wine` (`xtask/src/wine.rs`).
const WINE: &str = "/opt/wine/bin/fossai-wine";
/// Оболочка Debian, которой исполняются оба сценария.
const BASH: &str = "/opt/debian/bin/bash";

/// Командная строка, которой запускается программа Windows, если `path` — она
/// (P12, 18.11): `.exe` без заголовка CLI (машинный код Windows), `.msi`
/// (установщик — `msiexec /i`), `.lnk`, `.bat`, `.cmd`.
///
/// Чем: Proton, если стоит пакет `proton` (`fossai-proton --open=ПУТЬ`, общий
/// префикс `~/.proton`); иначе — Wine из пакета `wine`. Сборка .NET Framework
/// (заголовок CLI есть, `runtimeconfig.json` нет — её ловит [`dotnet_command`])
/// уходит в `/bin/mono`, если он есть: Mono в префиксах Proton выключен.
///
/// Путь с пробелом: ядро делит строку по пробелам, поэтому Proton получает
/// путь с `%20` и раскодирует его сам. У Wine такого разбора нет — путь с
/// пробелом им не запускается (названный предел, как у [`dotnet_command`]).
fn windows_command(path: &str) -> Option<String> {
    let lower = path.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, ext)| ext);
    if !matches!(ext, "exe" | "msi" | "lnk" | "bat" | "cmd") {
        return None;
    }
    let mut info = Stat::default();
    let has = |file: &str, info: &mut Stat| stat(file, info) >= 0;
    if ext == "exe" && pe_has_cli(path) && !path.contains(' ') && has("/bin/mono", &mut info) {
        return Some(format!("/bin/mono {path}"));
    }
    if has(PROTON, &mut info) {
        let mut encoded = String::with_capacity(path.len());
        for ch in path.chars() {
            match ch {
                ' ' => encoded.push_str("%20"),
                '%' => encoded.push_str("%25"),
                _ => encoded.push(ch),
            }
        }
        return Some(format!("{BASH} {PROTON} --open={encoded}"));
    }
    if has(WINE, &mut info) && !path.contains(' ') {
        return Some(match ext {
            "msi" => format!("{BASH} {WINE} msiexec /i {path}"),
            "exe" => format!("{BASH} {WINE} {path}"),
            _ => format!("{BASH} {WINE} start /unix {path}"),
        });
    }
    None
}

/// Есть ли у PE-файла заголовок CLI (каталог данных 14, `COM_DESCRIPTOR`) —
/// сборка .NET, а не машинный код. Не PE или не прочиталось — `false`.
fn pe_has_cli(path: &str) -> bool {
    let fd = open(path);
    if fd < 0 {
        return false;
    }
    let mut head = [0u8; 1024];
    let got = read(fd, &mut head);
    close(fd);
    if got < 64 || &head[..2] != b"MZ" {
        return false;
    }
    let got = got as usize;
    let u16_at = |at: usize| head.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |at: usize| head.get(at..at + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    let Some(pe) = u32_at(0x3c).map(|v| v as usize).filter(|&pe| pe + 24 < got) else {
        return false;
    };
    if head.get(pe..pe + 4) != Some(b"PE\0\0".as_slice()) {
        return false;
    }
    let optional = pe + 24;
    // Каталоги данных: у PE32 с 96-го байта необязательного заголовка, у PE32+ — со 112-го.
    let dirs = match u16_at(optional) {
        Some(0x10b) => optional + 96,
        Some(0x20b) => optional + 112,
        _ => return false,
    };
    let count = u32_at(optional + if dirs == optional + 96 { 92 } else { 108 }).unwrap_or(0);
    count > 14 && u32_at(dirs + 14 * 8).is_some_and(|rva| rva != 0)
}

/// Прочитать файл для просмотра.
fn read_preview(name: &str, path: &str) -> Preview {
    let mut lines = Vec::new();
    let mut note = String::new();

    let fd = open(path);
    if fd < 0 {
        return Preview {
            name: name.to_string(),
            lines,
            note: format!("cannot read: error {fd}"),
            scroll: 0,
        };
    }

    // Полный размер спрашивается отдельно: прочитано будет не больше предела, а
    // сказать «показано не всё» можно только зная, сколько всего.
    let mut info = Stat::default();
    let total = if fstat_ok(fd, &mut info) { info.size } else { 0 };

    let mut buffer = Vec::new();
    // `try_reserve` вместо `vec![]`: отказ аллокатора обязан вернуться ошибкой,
    // а не уронить программу. Восемь килобайт есть почти всегда — «почти» здесь
    // и означает, что проверка нужна.
    if buffer.try_reserve_exact(PREVIEW_LIMIT).is_err() {
        close(fd);
        return Preview {
            name: name.to_string(),
            lines,
            note: String::from("not enough memory to preview this file"),
            scroll: 0,
        };
    }
    buffer.resize(PREVIEW_LIMIT, 0u8);

    let read_bytes = read(fd, &mut buffer);
    close(fd);
    if read_bytes < 0 {
        return Preview {
            name: name.to_string(),
            lines,
            note: format!("cannot read: error {read_bytes}"),
            scroll: 0,
        };
    }
    let got = (read_bytes as usize).min(buffer.len());

    match core::str::from_utf8(&buffer[..got]) {
        Ok(text) => {
            for line in text.lines().take(PREVIEW_LINES) {
                // Табуляции и управляющие байты испортили бы разметку строки:
                // рисование текста не знает про них ничего.
                lines.push(line.replace('\t', "    "));
            }
            if total > got as u64 {
                note = format!("... {got} of {total} bytes shown");
            }
        }
        // Двоичный файл не показывается вовсе, а не показывается мусором: из
        // «шрифт нарисовал непечатное» никто не сделает вывода, что файл
        // двоичный.
        Err(_) => note = format!("binary file, {got} bytes"),
    }

    Preview { name: name.to_string(), lines, note, scroll: 0 }
}

/// `fstat`, у которого ответ — «получилось или нет».
fn fstat_ok(fd: i64, out: &mut Stat) -> bool {
    user_progs::fstat(fd, out) >= 0
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

/// Heap of the Files window (`--paste`, `--netlist` and `--properties` keep
/// the default megabyte).
const FILES_HEAP: usize = 4 * 1024 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно в том виде, в каком их положило
    // ядро, — это и есть единственный допустимый источник по контракту `Args`.
    let args = unsafe { Args::new(argc, argv) };

    // `files --properties ПУТЬ…` (24.11): окно «Свойства».
    if args.get(1) == Some("--properties") {
        props::run(&args, 2);
    }

    // `files --netlist OUT PATH` (24.11): a network folder read for a
    // window, in the background (`files/net.rs`).
    if args.get(1) == Some("--netlist") {
        net::netlist_main(&args);
    }

    // `files --paste DIR` (24.11): файлы из буфера обмена — в DIR без окна.
    // Так вставляет стол (Ctrl+V, «Вставить»): копирование идёт отдельной
    // задачей и стол не держит.
    if args.get(1) == Some("--paste") {
        let dir = (2..args.len()).filter_map(|index| args.get(index)).collect::<Vec<_>>().join(" ");
        user_progs::exit(paste_headless(dir));
    }

    // The window wants more than the default megabyte (24.11): at 200 % on
    // 2560×1440 theme icons of 96–128 points, the font and the rows ran it
    // out («user: panic», exit 101). Asked before the first allocation.
    user_progs::heap_size(FILES_HEAP);
    // Графика, формат точки и тема — у каркаса (`user_progs::app`, фаза С8).
    let info = app::start(&SPEC);
    // Путь с пробелом («Новая папка») приезжает несколькими аргументами:
    // строку запуска ядро режет по пробелам.
    let path = (1..args.len()).filter_map(|index| args.get(index)).collect::<Vec<_>>().join(" ");
    let start = starting_dir((!path.is_empty()).then_some(path.as_str()));
    // Путь к файлу (20.11, «Показать в папке» из загрузок браузера): папка
    // открывается с этим файлом выделенным.
    let reveal = (!path.is_empty() && start != path && parent_of(&path) == start).then(|| path.rsplit('/').next().unwrap_or("").to_string());
    let home = home_dir();
    let size = window_size(&info);

    app::run(&SPEC, &info, size, move |_, _| {
        let mut view = Files::new(start, home);
        if let Some(at) = reveal.as_deref().and_then(|name| view.rows.iter().position(|row| row.name == name)) {
            view.selected = at;
            view.scroll.follow(at);
        }
        // Что именно прочитано — в журнал, по разу на каждый каталог.
        // Нарисованное на экране снаружи не проверить, а строка проверяется:
        // она и отличает «список показан» от «окно нарисовано пустым».
        view.report();
        view
    })
}

/// Вставить файлы из буфера обмена в `dir` без окна (`files --paste`).
/// Код выхода — ноль, если всё перенесено или скопировано.
fn paste_headless(dir: String) -> i64 {
    if dir.is_empty() {
        println("files: --paste needs a folder");
        return 2;
    }
    let mut view = Files::new(dir.clone(), home_dir());
    if user_progs::clipboard_get_files().is_none() {
        println("files: paste: no files on the clipboard");
        return 1;
    }
    view.paste();
    while view.job.is_some() {
        view.copy_step();
        if let Some(error) = view.job.as_ref().and_then(|job| job.error.clone()) {
            println(&format!("files: paste into {dir} failed: {error}"));
            view.cancel_copy();
            return 1;
        }
    }
    0
}

/// Окно и цикл — у каркаса; здесь только то, чем «Файлы» отвечают на ввод.
///
/// Собственные методы названы иначе (`render`, `on_key`, `on_click`), чем
/// методы трейта: при одинаковых именах вызов из реализации трейта держался бы
/// на правиле «собственный метод важнее», и одна опечатка превратила бы его в
/// тихую бесконечную рекурсию.
impl App for Files {
    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        self.render(s, area, ctx);
    }

    /// Smallest window (24.11, WM SDK): tabs, the command bar, the address
    /// row, the side list and a table (wider ones scroll sideways).
    fn size_limits(&self) -> app::SizeLimits {
        app::SizeLimits::min(560, 380)
    }

    fn key(&mut self, code: u32) -> bool {
        self.on_key(code)
    }

    /// Сочетания (макет 14): Ctrl+X/C/V/A, Alt+Enter — свойства, Shift+стрелки
    /// и пробел — отметить несколько. Остальное — [`Files::on_key`].
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        // Сам Ctrl (21.11): посреди перетаскивания он переключает «перенести»
        // на «копировать»; больше ничего не делает.
        if code == user_abi::WIN_KEY_CTRL {
            self.ctrl_held = true;
            return self.drag.as_ref().is_some_and(|drag| drag.active);
        }
        if matches!(code, user_abi::WIN_KEY_SHIFT | user_abi::WIN_KEY_ALT) {
            return false;
        }
        self.tip = None;
        // «Параметры папок» (24.11) — модальное окно.
        if self.options.is_some() {
            return self.key_options(code);
        }
        // Поле нового имени — строка ввода с выделением (24.11).
        if matches!(self.menu.as_ref().map(|menu| &menu.mode), Some(MenuMode::Rename(..))) {
            return self.key_rename(code, mods, latin);
        }
        // F2 — переименовать (24.11, как в Проводнике).
        if code == user_abi::WIN_KEY_F1 + 1 && self.menu.is_none() && self.preview.is_none() && self.props.is_none() && !self.searching && self.address.is_none() {
            return self.run_command(Command::Rename);
        }
        // Меню вкладки: стрелки, Enter, всё прочее закрывает.
        if let Some((index, x, y, selected)) = self.tab_menu {
            match code {
                WIN_KEY_UP => self.tab_menu = Some((index, x, y, selected.saturating_sub(1))),
                WIN_KEY_DOWN => self.tab_menu = Some((index, x, y, (selected + 1).min(TAB_ENTRIES.len() - 1))),
                ENTER => return self.run_tab_menu(index, selected),
                _ => self.tab_menu = None,
            }
            return true;
        }
        // Адрес и список подпапок у шеврона (21.11) — раньше всего прочего:
        // буквы здесь — набор, а не сочетания.
        if let Some(handled) = self.key_address(code, mods, latin) {
            return handled;
        }
        // Вкладки (20.11): Ctrl+T — новая, Ctrl+W — закрыть, Ctrl+Tab и
        // Ctrl+Shift+Tab — соседняя, Ctrl+Enter — папку в новой вкладке.
        if mods & WIN_MOD_CTRL != 0 && self.menu.is_none() {
            match char::from_u32(latin).map(|ch| ch.to_ascii_lowercase()) {
                Some('t') => {
                    let start = self.new_tab_path();
                    self.open_tab(start, true);
                    return true;
                }
                Some('w') => return self.close_tab(self.tab),
                _ => {}
            }
            if code == u32::from(b'\t') && self.tabs.len() > 1 {
                let count = self.tabs.len();
                let next = if mods & WIN_MOD_SHIFT != 0 { (self.tab + count - 1) % count } else { (self.tab + 1) % count };
                return self.switch_tab(next);
            }
            if code == ENTER && self.preview.is_none() && !self.searching {
                let path = self.rows.get(self.selected).filter(|row| row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name));
                if let Some(path) = path {
                    self.open_tab(path, false);
                    return true;
                }
            }
        }
        // Alt+V — меню «Вид» (24.11, как Alt+буква меню в Windows): в нём и
        // «Параметры папок…».
        if mods & WIN_MOD_ALT != 0 && char::from_u32(latin).is_some_and(|ch| ch.eq_ignore_ascii_case(&'v')) && self.menu.is_none() {
            self.pop = if self.pop.is_some_and(|(open, _)| open == Command::View) { None } else { Some((Command::View, 0)) };
            println("files: view menu opened");
            return true;
        }
        if let Some((command, selected)) = self.pop {
            let count = self.pop_entries(command).len();
            match code {
                WIN_KEY_UP => self.pop = Some((command, selected.saturating_sub(1))),
                WIN_KEY_DOWN => self.pop = Some((command, (selected + 1).min(count.saturating_sub(1)))),
                ENTER => self.run_pop(command, selected),
                _ => self.pop = None,
            }
            return true;
        }
        let busy = self.menu.is_some() || self.searching || self.preview.is_some() || self.props.is_some();
        if !busy {
            let letter = char::from_u32(latin).map(|ch| ch.to_ascii_lowercase());
            if mods & WIN_MOD_CTRL != 0 {
                match letter {
                    Some('x') => return self.run_command(Command::Cut),
                    Some('c') => return self.run_command(Command::Copy),
                    Some('v') => return self.run_command(Command::Paste),
                    Some('a') => {
                        self.picked = (0..self.rows.len()).filter(|index| !self.rows[*index].is_volume()).collect();
                        return true;
                    }
                    _ => {}
                }
            }
            if mods & WIN_MOD_ALT != 0 && code == ENTER {
                // Окно «Свойства» (24.11): выделенного, иначе открытой папки.
                let focus = self.focus();
                self.properties_of_selection(focus);
                return true;
            }
            if mods & WIN_MOD_SHIFT != 0 && matches!(code, WIN_KEY_UP | WIN_KEY_DOWN) && !self.rows.is_empty() {
                self.no_cursor = false;
                self.picked.insert(self.selected);
                let changed = self.on_key(code);
                self.picked.insert(self.selected);
                return changed;
            }
            if code == u32::from(b' ') && !self.rows.is_empty() {
                if !self.picked.remove(&self.selected) {
                    self.picked.insert(self.selected);
                }
                return true;
            }
            // Стрелка без Shift снимает отметки — как в Проводнике.
            if matches!(code, WIN_KEY_UP | WIN_KEY_DOWN | WIN_KEY_HOME | WIN_KEY_END | WIN_KEY_PAGE_UP | WIN_KEY_PAGE_DOWN) {
                self.picked.clear();
                self.no_cursor = false;
            }
            if self.view == ViewMode::List && matches!(code, WIN_KEY_LEFT | WIN_KEY_RIGHT) {
                self.picked.clear();
                self.no_cursor = false;
            }
        }
        let changed = self.on_key(code);
        // Курсор, сдвинутый клавишей, — новый якорь Shift+щелчка.
        if !busy && mods & WIN_MOD_SHIFT == 0 && matches!(code, WIN_KEY_UP | WIN_KEY_DOWN | WIN_KEY_HOME | WIN_KEY_END | WIN_KEY_PAGE_UP | WIN_KEY_PAGE_DOWN | WIN_KEY_LEFT | WIN_KEY_RIGHT) {
            self.anchor = self.selected;
        }
        changed
    }

    fn drag(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.on_drag(area, ctx, x, y)
    }

    fn release(&mut self, _area: Rect, _ctx: Ctx, _x: i32, _y: i32) -> bool {
        // Бегунок полосы прокрутки — бросить.
        if self.scroll.release() | self.hscroll.release() {
            return true;
        }
        // Вкладку, вытянутую из полосы, — другому окну «Файлов», если её
        // бросили на него, иначе в своё окно (20.11).
        if let Some(drag) = self.tab_drag.take() {
            return if drag.out { self.offer_tab(drag.index) } else { false };
        }
        self.on_release()
    }

    /// Копирование кусками — на каждом витке цикла окна; перенос вкладки
    /// между окнами.
    fn step(&mut self) -> bool {
        // «Параметры папок» сменили в другом окне или на столе (24.11).
        let options = if self.load_options() {
            self.reload_keep();
            true
        } else {
            false
        };
        options | self.net_step() | self.tip_step() | self.copy_step() | self.offer_step() | self.watch_drop() | self.item_offer_step() | self.watch_items() | self.watch_step()
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.on_click(area, ctx, x, y)
    }

    /// Ctrl+щелчок — отметить строку или снять отметку; Shift+щелчок —
    /// отметить всё от выбранной до этой (как в Проводнике).
    fn click_with(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32, mods: i32) -> bool {
        self.press_mods = mods;
        self.ctrl_held = mods & WIN_MOD_CTRL != 0;
        let idle = self.menu.is_none() && self.pop.is_none() && self.job.is_none() && self.props.is_none() && self.preview.is_none();
        if mods & (WIN_MOD_CTRL | WIN_MOD_SHIFT) != 0 && idle && !self.on_computer() {
            let plan = layout(ctx, area);
            let body = if self.view == ViewMode::Table { plan.list } else { plan.list.union(&plan.header) };
            let bar = { let (v, h) = self.bars(ctx, &plan); v.is_some_and(|bar| bar.contains(x, y)) || h.is_some_and(|bar| bar.contains(x, y)) };
            if body.contains(x, y) && !bar {
                if let Some(index) = self.row_at(&plan, ctx, x, y) {
                    if mods & WIN_MOD_SHIFT != 0 {
                        // Shift — от отметки-якоря до этой строки (21.11:
                        // заменяет отмеченное, с Ctrl — добавляет), как в
                        // Проводнике.
                        let anchor = if self.anchor < self.rows.len() { self.anchor } else { self.selected };
                        if mods & WIN_MOD_CTRL == 0 {
                            self.picked.clear();
                        }
                        self.picked.extend(anchor.min(index)..=anchor.max(index));
                        self.selected = index;
                        self.no_cursor = false;
                    } else {
                        // Ctrl — отметить. Уже отмеченная снимается при
                        // отпускании: Ctrl+перетаскивание её копирует.
                        if self.picked.is_empty() && self.selected != index && !self.no_cursor && self.selected < self.rows.len() {
                            self.picked.insert(self.selected);
                        }
                        let was = !self.picked.insert(index);
                        self.selected = index;
                        self.anchor = index;
                        self.no_cursor = false;
                        self.start_drag(index, x, y, if was { ClickAfter::Unpick } else { ClickAfter::Nothing });
                    }
                    self.last_click = None;
                    println(&format!("files: {} item(s) marked", self.picked.len()));
                    return true;
                }
            }
        }
        self.on_click(area, ctx, x, y)
    }

    /// Ctrl отпустили — перетаскивание снова переносит.
    fn key_up(&mut self, code: u32) -> bool {
        if code == user_abi::WIN_KEY_CTRL {
            self.ctrl_held = false;
            return self.drag.as_ref().is_some_and(|drag| drag.active);
        }
        false
    }

    fn click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        self.on_click_right(area, ctx, x, y)
    }

    /// Колесо — три строки за щелчок, как у Проводника. С 20.11 оно двигает
    /// **вид**, а выделение остаётся, где было (Роман: «при прокрутке
    /// выделение прыгает по папкам»). В просмотре листает текст; в открытом
    /// меню и в поле переименования ничего не делает.
    fn wheel_at(&mut self, area: Rect, ctx: Ctx, clicks: i32) -> bool {
        // Окно выбора программы листает свой список (22.11).
        if let Some(MenuMode::Choose(chooser)) = self.menu.as_mut().map(|menu| &mut menu.mode) {
            let last = chooser.progs.len().saturating_sub(CHOOSER_ROWS);
            let step = clicks.unsigned_abs() as usize;
            chooser.first = if clicks > 0 { chooser.first.saturating_sub(step) } else { (chooser.first + step).min(last) };
            return true;
        }
        if self.menu.is_some() || self.tab_menu.is_some() || clicks == 0 {
            return false;
        }
        if self.preview.is_some() {
            let key = if clicks > 0 { WIN_KEY_UP } else { WIN_KEY_DOWN };
            let mut changed = false;
            for _ in 0..clicks.unsigned_abs().saturating_mul(3).min(60) {
                changed |= self.on_key(key);
            }
            return changed;
        }
        let plan = layout(ctx, area);
        let (view, content, line) = self.view_metrics(ctx, &plan);
        // «Список» колесо листает вбок — столбец за щелчок.
        let span = if self.view == ViewMode::List && !self.on_computer() { view.w } else { view.h };
        let moved = self.scroll.wheel(clicks, span, content, line);
        if moved {
            println(&format!("files: scrolled to {} of {}", self.scroll.offset(), kit::Scroll::limit(span, content)));
        }
        moved
    }

    /// Указатель над окном без нажатых кнопок (20.11): полоса прокрутки под
    /// ним шире и ярче, строка, место слева и вкладка подсвечены. Окно
    /// перерисовывается, только когда подсвеченное сменилось.
    fn hover(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        // «7-Zip ▸» opens and its rows light under the pointer (25.11).
        if self.hover_zip(ctx, area, x, y) {
            return true;
        }
        let plan = layout(ctx, area);
        let (v, h) = self.bars(ctx, &plan);
        let bars = self.scroll.hover(v.as_ref(), x, y) | self.hscroll.hover(h.as_ref(), x, y);
        // Указатель только что вошёл в окно над полосой вкладок — не бросили
        // ли сюда вкладку другого окна (см. [`TAB_DROP`]).
        if self.pointer.is_none() && plan.tabs.union(&plan.toolbar).contains(x, y) {
            self.drop_watch = Some(monotonic_ms() + TAB_DROP_WATCH_MS);
        }
        // Вошёл куда угодно — не бросили ли сюда строки другого окна (21.11).
        if self.pointer.is_none() {
            self.item_watch = Some(monotonic_ms() + TAB_DROP_WATCH_MS);
        }
        self.pointer = Some((x, y));
        let hot = self.hot_at(ctx, &plan, x, y);
        let changed = hot != self.hot;
        self.hot = hot;
        // Всплывающая подсказка (24.11): строка под указателем — ждать, не
        // уйдёт ли он с неё; ушёл — подсказку убрать.
        let mut tip_gone = false;
        if changed {
            tip_gone = self.tip.as_ref().is_some_and(|tip| tip.lines.is_some());
            self.tip = match hot {
                Hot::Row(index) if self.opts.info_tips => Some(Tip { index, since: monotonic_ms(), lines: None }),
                _ => None,
            };
        }
        // Окно «Параметры папок»: подсветка строки под указателем.
        let options = self.options.is_some();
        // Подсветка звеньев адреса — строкой для стенда (21.11).
        if changed {
            match hot {
                Hot::Crumb(index) => println(&format!("files: hover crumb {index}")),
                Hot::Chevron(index) => println(&format!("files: hover chevron {index}")),
                _ => {}
            }
        }
        let menus = self.hover_menus(ctx, area, &plan, x, y);
        bars | changed | tip_gone | options | menus | self.hover_crumb_pop(ctx, area, &plan, x, y)
    }

    fn leave(&mut self) -> bool {
        self.pointer = None;
        self.tip = None;
        let changed = self.hot != Hot::Nothing;
        self.hot = Hot::Nothing;
        self.scroll.hover(None, 0, 0) | self.hscroll.hover(None, 0, 0) | changed
    }

    /// Средняя кнопка (20.11), как в Проводнике: по папке — открыть её в
    /// новой вкладке фоном, по вкладке — закрыть вкладку.
    fn click_middle(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        if let Some(kit::TabHit::Tab(index) | kit::TabHit::Close(index)) = self.tab_strip(ctx, &plan).hit(x, y) {
            return self.close_tab(index);
        }
        if self.preview.is_some() || self.props.is_some() || self.on_computer() {
            return false;
        }
        let path = self.row_at(&plan, ctx, x, y).and_then(|index| self.rows.get(index)).filter(|row| row.directory).map(|row| join(&self.path, &row.name));
        match path {
            Some(path) => {
                self.open_tab(path, false);
                true
            }
            None => false,
        }
    }

    /// Имя окна (20.11) — «Files\x1fпапка»: первая часть — имя окна для
    /// журнала и стенда (`focus 'Files'`), вторая — подпись, которую стол
    /// показывает в заголовке, на панели задач и в миниатюрах. Так два окна
    /// «Файлов» различимы.
    fn title(&self) -> Option<String> {
        // «Выводить полный путь в заголовке окна» (24.11).
        if self.opts.full_path_title && !self.on_computer() {
            return Some(format!("{TITLE}\x1f{}", self.path));
        }
        Some(format!("{TITLE}\x1f{}", folder_title(&self.path, &self.home)))
    }

    fn wants_quit(&self) -> bool {
        self.quit
    }

    fn after_input(&mut self) {
        self.report_if_moved();
    }

    /// `q` закрывает окно только тогда, когда это не буква. До фазы С8
    /// проверялся лишь просмотр файла, и имя с буквой «q», набранное в поле
    /// переименования, закрывало окно на полуслове.
    fn quits_on_q(&self) -> bool {
        self.preview.is_none() && self.menu.is_none() && self.tab_menu.is_none() && self.address.is_none() && self.crumb_pop.is_none()
    }
}

impl Files {
    /// Сказать в журнал, что показано сейчас.
    ///
    /// Печатается то, чего снаружи не видно иначе: путь, число строк и имя
    /// выбранной. Снимок экрана доказательством не считается — это правило
    /// дома, — а по этой строке стенд проверяет и переход по каталогам, и то,
    /// что стрелка действительно двигает выделение.
    fn report(&self) {
        match &self.error {
            Some(text) => println(&format!("files: {} failed: {text}", self.path)),
            // «Сеть» (`/net`) is a row here but not a volume: it stands under
            // «Сетевые расположения» and is named after the selection.
            None if self.on_computer() => println(&format!(
                "files: this computer has {} volume(s), selected '{}'{}",
                self.volume_count(),
                self.selected_name(),
                if self.volume_count() < self.rows.len() { ", network place 'Сеть'" } else { "" }
            )),
            // The row number too (24.11): «the wheel keeps the selection on
            // the first row» was checked by the first name, which every new
            // program in /bin could change.
            None => println(&format!(
                "files: {} has {} entries, selected '{}' (row {})",
                self.path,
                self.rows.len(),
                self.selected_name(),
                self.selected + 1
            )),
        }
    }

    /// Имя выбранной строки, либо прочерк, если выбирать не из чего.
    /// Volumes of «Этот компьютер»: its rows without «Сеть» (24.11).
    fn volume_count(&self) -> usize {
        self.rows.iter().filter(|row| row.name != sysconf::winpath::NET).count()
    }

    fn selected_name(&self) -> &str {
        match self.rows.get(self.selected) {
            Some(row) => &row.name,
            None => "-",
        }
    }

    /// Сказать в журнал после действия, которое могло сменить каталог, строку
    /// или открыть просмотр.
    fn report_if_moved(&self) {
        // Пока открыто меню или свойства, список не менялся, и повторять его
        // незачем: строка стенда о меню уже напечатана там, где оно открылось.
        if self.menu.is_some() || self.props.is_some() {
            return;
        }
        match self.preview.as_ref() {
            Some(preview) => println(&format!(
                "files: preview '{}' has {} line(s)",
                preview.name,
                preview.lines.len()
            )),
            None => self.report(),
        }
    }
}

/// Подпись папки — последнее звено адреса: «Загрузки», «bin», «Этот
/// компьютер». Ею подписаны вкладка и окно.
fn folder_title(path: &str, home: &str) -> String {
    breadcrumb(path, home).last().map_or_else(|| String::from(TITLE), |(label, _)| label.clone())
}

// ---------------------------------------------------------------------------
// Вкладки и прокрутка (20.11)
// ---------------------------------------------------------------------------

impl Files {
    /// С чего начинается новая вкладка: «Главная», как в Проводнике, а
    /// если домашнего каталога нет (живая система) — «Этот компьютер».
    fn new_tab_path(&self) -> String {
        if exists(&self.home) { self.home.clone() } else { String::from(COMPUTER) }
    }

    fn tab_strip(&self, ctx: Ctx, plan: &Plan) -> kit::TabStrip {
        kit::TabStrip::new(ctx, plan.tabs, self.tabs.len())
    }

    /// Подписи вкладок: у открытой — её папка сейчас.
    fn tab_titles(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| folder_title(if index == self.tab { &self.path } else { &tab.path }, &self.home))
            .collect()
    }

    /// Запомнить открытую вкладку в её ячейке.
    fn save_tab(&mut self) {
        let tab = Tab {
            path: self.path.clone(),
            back: self.back.clone(),
            forward: self.forward.clone(),
            selected: self.selected,
            scroll: self.scroll.offset(),
        };
        if let Some(slot) = self.tabs.get_mut(self.tab) {
            *slot = tab;
        }
    }

    /// Открыть вкладку `index`: её папку, историю, выделение и прокрутку.
    fn load_tab(&mut self, index: usize) {
        let tab = core::mem::take(&mut self.tabs[index]);
        self.tab = index;
        self.path = tab.path;
        self.back = tab.back;
        self.forward = tab.forward;
        self.preview = None;
        self.props = None;
        self.menu = None;
        self.pop = None;
        self.drag = None;
        self.note = None;
        self.query.clear();
        self.searching = false;
        self.reload();
        self.selected = tab.selected.min(self.rows.len().saturating_sub(1));
        self.scroll.reset();
        self.scroll.set(tab.scroll);
        self.hscroll.reset();
    }

    fn switch_tab(&mut self, index: usize) -> bool {
        if index == self.tab || index >= self.tabs.len() {
            return false;
        }
        self.save_tab();
        self.load_tab(index);
        println(&format!("files: tab {} of {} is '{}'", index + 1, self.tabs.len(), self.path));
        true
    }

    /// Новая вкладка с папкой `path` в конце полосы. `focus` — сразу в неё
    /// (Ctrl+T, «+»); иначе фоном, как средняя кнопка в Проводнике.
    fn open_tab(&mut self, path: String, focus: bool) {
        let title = folder_title(&path, &self.home);
        println(&format!("files: tab {} opened '{path}'", self.tabs.len() + 1));
        self.tabs.push(Tab { path, ..Tab::default() });
        if focus {
            self.switch_tab(self.tabs.len() - 1);
        } else {
            self.note = Some(format!("«{title}» открыта в новой вкладке"));
        }
    }

    /// Закрыть вкладку. Последнюю — вместе с окном, как Ctrl+W в Проводнике.
    fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return false;
        }
        if self.tabs.len() == 1 {
            println("files: last tab closed");
            self.quit = true;
            return true;
        }
        self.tabs.remove(index);
        if index == self.tab {
            self.load_tab(index.min(self.tabs.len() - 1));
        } else if index < self.tab {
            self.tab -= 1;
        }
        println(&format!("files: tab {} closed, {} left", index + 1, self.tabs.len()));
        true
    }

    /// Открыть папку в новом окне — новой задачей «Файлов».
    fn open_window(&mut self, path: &str) -> bool {
        let line = format!("/bin/files {path}");
        let id = spawn(&line);
        if id < 0 {
            println(&format!("files: cannot start '{line}': error {id}"));
            self.note = Some(format!("Не удалось открыть окно: {}", error_text(id)));
            return false;
        }
        println(&format!("files: new window for '{path}' as #{id}"));
        true
    }

    /// Вынести вкладку в своё окно (перетащили из полосы или «Переместить в
    /// новое окно»). Брошенная на другое окно «Файлов» переезжает туда —
    /// [`Self::offer_tab`].
    fn tear_off(&mut self, index: usize) -> bool {
        if self.tabs.len() < 2 {
            self.note = Some(String::from("Единственная вкладка уже в своём окне"));
            return true;
        }
        let path = if index == self.tab { self.path.clone() } else { self.tabs[index].path.clone() };
        if self.open_window(&path) {
            println(&format!("files: tab {} moved to a new window", index + 1));
            self.close_tab(index);
        }
        true
    }

    /// Вынесенную и отпущенную вкладку предложить окну «Файлов» под
    /// указателем (20.11, [`TAB_DROP`]); ответ ждёт [`Self::offer_step`].
    fn offer_tab(&mut self, index: usize) -> bool {
        if self.tabs.len() < 2 || index >= self.tabs.len() {
            return self.tear_off(index);
        }
        let path = if index == self.tab { self.path.clone() } else { self.tabs[index].path.clone() };
        let now = monotonic_ms();
        let token = format!("{now}.{}", self.tabs.len());
        // Буфер обмена, если писать некуда, — вернуть потом как был.
        let saved = clipboard_get();
        let Some(by_clipboard) = mailbox_write(&format!("offer {token} {now} {path}")) else {
            return self.tear_off(index);
        };
        self.offer = Some((index, token, now + TAB_DROP_WAIT_MS));
        self.offer_clip = by_clipboard.then_some(saved);
        true
    }

    /// Ждать, возьмёт ли вкладку другое окно: взяли — закрыть её здесь, не
    /// дождались — вынести в своё окно.
    fn offer_step(&mut self) -> bool {
        let Some((index, token, deadline)) = self.offer.clone() else {
            return false;
        };
        let answer = format!("taken {token}");
        let taken = mailbox_read().iter().any(|text| *text == answer);
        if !taken && monotonic_ms() < deadline {
            return false;
        }
        self.offer = None;
        let _ = remove(TAB_DROP);
        if let Some(saved) = self.offer_clip.take() {
            clipboard_set(&saved);
        }
        if index >= self.tabs.len() {
            return true;
        }
        if taken {
            println(&format!("files: tab {} moved to another window", index + 1));
            return self.close_tab(index);
        }
        self.tear_off(index)
    }

    /// Указатель вошёл в окно над полосой вкладок: свежее чужое предложение
    /// вкладки — открыть её путь новой вкладкой и ответить «взята».
    fn watch_drop(&mut self) -> bool {
        let Some(until) = self.drop_watch else {
            return false;
        };
        let now = monotonic_ms();
        if now > until {
            self.drop_watch = None;
            return false;
        }
        for text in mailbox_read() {
            let mut parts = text.splitn(4, ' ');
            let (Some("offer"), Some(token), Some(at), Some(path)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if self.offer.as_ref().is_some_and(|(_, own, _)| own == token) {
                continue;
            }
            if !at.parse::<u64>().is_ok_and(|at| now.saturating_sub(at) <= TAB_DROP_WAIT_MS) {
                continue;
            }
            if mailbox_write(&format!("taken {token}")).is_none() {
                return false;
            }
            self.drop_watch = None;
            println(&format!("files: tab '{path}' taken from another window"));
            self.open_tab(String::from(path), true);
            return true;
        }
        false
    }

    /// Строки вынесли за окно и отпустили (21.11): предложить их окну
    /// «Файлов» под указателем тем же ящиком, что вкладки ([`TAB_DROP`]):
    /// `items <метка> <время> <c|m> <пути через \x1e>`. Взявшее окно само
    /// переносит (или копирует) их в свою папку.
    fn offer_items(&mut self, paths: &[String], copy: bool) -> bool {
        let now = monotonic_ms();
        let token = format!("{now}.i{}", paths.len());
        let saved = clipboard_get();
        let text = format!("items {token} {now} {} {}", if copy { 'c' } else { 'm' }, paths.join("\u{1e}"));
        let Some(by_clipboard) = mailbox_write(&text) else {
            self.note = Some(String::from("Перетаскивать в другое окно некуда: нет записываемого /tmp"));
            return true;
        };
        println(&format!("files: offered {} item(s) to another window", paths.len()));
        self.item_offer = Some((token, now + TAB_DROP_WAIT_MS, by_clipboard.then_some(saved), paths.len()));
        true
    }

    /// Ждать, возьмёт ли строки другое окно.
    fn item_offer_step(&mut self) -> bool {
        let Some((token, deadline, _, count)) = self.item_offer.clone() else {
            return false;
        };
        let answer = format!("taken {token}");
        let taken = mailbox_read().iter().any(|text| *text == answer);
        if !taken && monotonic_ms() < deadline {
            return false;
        }
        if let Some((_, _, Some(saved), _)) = self.item_offer.take() {
            clipboard_set(&saved);
        }
        let _ = remove(TAB_DROP);
        if taken {
            println(&format!("files: {count} item(s) taken by another window"));
            self.note = Some(format!("Объектов передано в другое окно: {count}"));
        } else {
            // Стол брошенное не принимает (его значки рисует ядро, договора
            // «бросить файл на стол» нет) — на «Рабочий стол» слева можно.
            println(&format!("files: nobody took {count} item(s)"));
            self.note = Some(String::from("Отпустите над папкой, вкладкой или окном «Файлов»"));
        }
        true
    }

    /// Указатель вошёл в окно: свежие строки другого окна — перенести или
    /// скопировать в папку под указателем (или в открытую) и ответить
    /// «взяты».
    fn watch_items(&mut self) -> bool {
        let Some(until) = self.item_watch else {
            return false;
        };
        let now = monotonic_ms();
        if now > until {
            self.item_watch = None;
            return false;
        }
        for text in mailbox_read() {
            let mut parts = text.splitn(5, ' ');
            let (Some("items"), Some(token), Some(at), Some(mode), Some(list)) = (parts.next(), parts.next(), parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            if self.item_offer.as_ref().is_some_and(|(own, ..)| own == token) {
                continue;
            }
            if !at.parse::<u64>().is_ok_and(|at| now.saturating_sub(at) <= TAB_DROP_WAIT_MS) {
                continue;
            }
            // Некуда положить: «Этот компьютер», просмотр, идёт копирование.
            if self.on_computer() || self.preview.is_some() || self.job.is_some() || self.error.is_some() {
                continue;
            }
            let dir = match self.hot {
                Hot::Row(index) => self.rows.get(index).filter(|row| row.directory && !row.is_volume()).map(|row| join(&self.path, &row.name)),
                _ => None,
            }
            .unwrap_or_else(|| self.path.clone());
            let paths: Vec<String> = list.split('\u{1e}').filter(|path| !path.is_empty()).map(String::from).collect();
            if mailbox_write(&format!("taken {token}")).is_none() {
                return false;
            }
            self.item_watch = None;
            let copy = mode == "c" || paths.first().is_some_and(|path| mount_of(&mount_points(), path) != mount_of(&mount_points(), &dir));
            println(&format!("files: took {} item(s) from another window into {dir}{}", paths.len(), if copy { " (copy)" } else { "" }));
            self.drop_paths(paths, dir, copy);
            return true;
        }
        false
    }

    /// Что подсвечено под указателем `x, y`. Пока открыто меню, свойства или
    /// что-то тянут — ничего.
    fn hot_at(&self, ctx: Ctx, plan: &Plan, x: i32, y: i32) -> Hot {
        let busy = self.menu.is_some()
            || self.tab_menu.is_some()
            || self.pop.is_some()
            || self.crumb_pop.is_some()
            || self.props.is_some()
            || self.job.is_some()
            || self.tab_drag.is_some()
            || self.drag.as_ref().is_some_and(|drag| drag.active)
            || self.band.is_some()
            || self.col_drag.is_some()
            || self.scroll.dragging()
            || self.hscroll.dragging();
        if busy {
            return Hot::Nothing;
        }
        if plan.header.contains(x, y) {
            if let Some(boundary) = self.col_grip(ctx, plan, x) {
                return Hot::Grip(boundary);
            }
        }
        if let Some(hit) = self.tab_strip(ctx, plan).hit(x, y) {
            return if hit == kit::TabHit::Strip { Hot::Nothing } else { Hot::Tab(hit) };
        }
        // Звенья адреса и их шевроны (21.11).
        if plan.path.contains(x, y) {
            if self.address.is_some() {
                return Hot::Nothing;
            }
            let (slots, lead) = self.crumb_slots(ctx, plan);
            if lead.contains(x, y) {
                return Hot::CrumbMore;
            }
            for (index, slot) in slots.iter().enumerate() {
                if slot.rect.contains(x, y) {
                    return Hot::Crumb(index);
                }
                if slot.chevron.contains(x, y) {
                    return Hot::Chevron(index);
                }
            }
            return Hot::Nothing;
        }
        if let Some(side) = plan.side.filter(|side| side.contains(x, y)) {
            let inner = ctx.on(theme::panel_bg());
            return place_slots(inner, side, &self.home, &self.pinned).iter().position(|slot| slot.rect.contains(x, y)).map_or(Hot::Nothing, Hot::Place);
        }
        if self.preview.is_some() || self.error.is_some() {
            return Hot::Nothing;
        }
        let (v, h) = self.bars(ctx, plan);
        if v.as_ref().is_some_and(|bar| bar.contains(x, y)) || h.as_ref().is_some_and(|bar| bar.contains(x, y)) {
            return Hot::Nothing;
        }
        self.row_at(plan, ctx, x, y).map_or(Hot::Nothing, Hot::Row)
    }

    /// Подсвеченное сейчас — от места указателя, а не от запомненного: после
    /// прокрутки колесом под указателем уже другая строка.
    fn hot_now(&self, ctx: Ctx, plan: &Plan) -> Hot {
        self.pointer.map_or(Hot::Nothing, |(x, y)| self.hot_at(ctx, plan, x, y))
    }

    fn click_tab(&mut self, hit: kit::TabHit) -> bool {
        self.pop = None;
        self.close_menu();
        match hit {
            kit::TabHit::Tab(index) => {
                self.tab_drag = Some(TabDrag { index, at: (0, 0), out: false });
                self.switch_tab(index);
                true
            }
            kit::TabHit::Close(index) => self.close_tab(index),
            kit::TabHit::New => {
                let start = self.new_tab_path();
                self.open_tab(start, true);
                true
            }
            kit::TabHit::Strip => false,
        }
    }

    /// Вкладку тянут: в полосе — переставить, ниже полосы или за окно —
    /// приготовить к выносу в своё окно.
    fn drag_tab(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        let strip = self.tab_strip(ctx, &plan);
        let several = self.tabs.len() > 1;
        let Some(drag) = self.tab_drag.as_mut() else {
            return false;
        };
        drag.at = (x, y);
        let was = drag.out;
        drag.out = several && (y > plan.tabs.bottom() + ctx.px(24) as i32 || y < area.y - ctx.px(8) as i32 || x < area.x || x >= area.right());
        if !drag.out {
            let mid = plan.tabs.y + plan.tabs.h as i32 / 2;
            if let Some(kit::TabHit::Tab(over) | kit::TabHit::Close(over)) = strip.hit(x, mid) {
                let from = drag.index;
                if over != from {
                    drag.index = over;
                    self.tabs.swap(from, over);
                    if self.tab == from {
                        self.tab = over;
                    } else if self.tab == over {
                        self.tab = from;
                    }
                    return true;
                }
            }
        }
        drag.out || was
    }

    /// 25.11 (Roman: the item under the pointer must be lit): hover moves
    /// the lit row of the open menus — the context menu, the tab menu and the
    /// «Сортировка» / «Вид» drop-downs — the same row the arrows move.
    fn hover_menus(&mut self, ctx: Ctx, area: Rect, plan: &Plan, x: i32, y: i32) -> bool {
        if let Some(card) = self.menu_rect(ctx, area) {
            if let Some(menu) = self.menu.as_mut() {
                if matches!(menu.mode, MenuMode::Items) {
                    if let Some(index) = kit::menu_hit(ctx, card, &entries(&menu.items), x, y) {
                        let changed = index != menu.selected;
                        menu.selected = index;
                        return changed;
                    }
                }
            }
            return false;
        }
        if let Some((index, mx, my, selected)) = self.tab_menu {
            if let Some(item) = self.tab_menu_rect(ctx, area).and_then(|rect| kit::menu_hit(ctx, rect, &TAB_ENTRIES, x, y)) {
                self.tab_menu = Some((index, mx, my, item));
                return item != selected;
            }
            return false;
        }
        if let Some((command, selected, rect, items)) = self.pop_layout(ctx, area, plan) {
            let entries: Vec<Entry<'_>> = items.iter().map(|(title, group)| Entry { title, icon: None, danger: false, group: *group, ..Entry::NONE }).collect();
            if let Some(index) = kit::menu_hit(ctx, rect, &entries, x, y) {
                self.pop = Some((command, index));
                return index != selected;
            }
        }
        false
    }

    fn tab_menu_rect(&self, ctx: Ctx, area: Rect) -> Option<Rect> {
        let (_, x, y, _) = self.tab_menu?;
        Some(kit::place((x, y), kit::menu_size(ctx, &TAB_ENTRIES), area))
    }

    fn click_tab_menu(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) -> bool {
        let Some((index, ..)) = self.tab_menu else {
            return false;
        };
        match self.tab_menu_rect(ctx, area).and_then(|rect| kit::menu_hit(ctx, rect, &TAB_ENTRIES, x, y)) {
            Some(item) => self.run_tab_menu(index, item),
            None => {
                self.tab_menu = None;
                true
            }
        }
    }

    fn run_tab_menu(&mut self, index: usize, item: usize) -> bool {
        self.tab_menu = None;
        match item {
            0 => {
                let start = self.new_tab_path();
                self.open_tab(start, true);
                true
            }
            1 => self.tear_off(index),
            2 => self.close_tab(index),
            _ => {
                self.switch_tab(index);
                let mut at = self.tabs.len();
                while at > 0 {
                    at -= 1;
                    if at != self.tab {
                        self.tabs.remove(at);
                        if at < self.tab {
                            self.tab -= 1;
                        }
                    }
                }
                println("files: other tabs closed");
                true
            }
        }
    }

    fn draw_tabs(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let titles = self.tab_titles();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let hover = match self.hot_now(ctx, plan) {
            Hot::Tab(hit) => Some(hit),
            _ => None,
        };
        self.tab_strip(ctx, plan).draw_hover(ctx, s, &titles, self.tab, Some(Icon::Folder), hover);
    }

    /// Вытянутая вкладка у указателя и меню вкладки.
    fn draw_tab_extras(&self, s: &mut Surface, ctx: Ctx, area: Rect) {
        if let Some(drag) = self.tab_drag.as_ref().filter(|drag| drag.out) {
            let title = self.tab_titles().get(drag.index).cloned().unwrap_or_default();
            let label = format!("Переместить: {title}");
            let w = ctx.face(Role::Body).width(&label) + ctx.px(20);
            let chip = Rect::new(drag.at.0 + ctx.px(14) as i32, drag.at.1 + ctx.px(10) as i32, w, ctx.px(28));
            let inner = kit::card(ctx, s, chip);
            paint::text(inner, s, Role::Body, chip.x + ctx.px(10) as i32, paint::baseline(ctx, Role::Body, chip), &label, ctx.palette.ink);
        }
        if let (Some(rect), Some((.., selected))) = (self.tab_menu_rect(ctx, area), self.tab_menu) {
            kit::draw_menu(ctx, s, rect, &TAB_ENTRIES, selected);
        }
    }

    /// Нужна ли таблице прокрутка вбок.
    fn hbar_needed(&self, ctx: Ctx, plan: &Plan) -> bool {
        self.view == ViewMode::Table && !self.on_computer() && self.preview.is_none() && self.error.is_none() && plan.list.w < ctx.px(self.table_min_w())
    }

    /// Самая узкая таблица со всеми столбцами: имя не уже 160 точек плюс
    /// столбцы, как их растянули (по умолчанию — [`TABLE_MIN_W`]).
    fn table_min_w(&self) -> u32 {
        TABLE_MIN_W - COLS_DEFAULT.iter().sum::<u32>() + self.cols.iter().sum::<u32>()
    }

    /// Прямоугольник таблицы: в узком окне — шире окна и сдвинут на прокрутку.
    fn table_list(&self, ctx: Ctx, plan: &Plan) -> Rect {
        if !self.hbar_needed(ctx, plan) {
            return plan.list;
        }
        let width = ctx.px(self.table_min_w());
        self.hscroll.clamp(plan.list.w, width);
        Rect::new(plan.list.x - self.hscroll.offset(), plan.list.y, width, plan.list.h)
    }

    /// Левые края столбцов «Дата», «Тип», «Размер» и правый край размера в
    /// таблице `table` — по ширинам `cols` (21.11).
    fn columns(&self, ctx: Ctx, table: Rect) -> (i32, i32, i32, i32) {
        let size_right = table.right() - ctx.px(16) as i32;
        let size_x = size_right - ctx.px(self.cols[2]) as i32;
        let type_x = size_x - ctx.px(self.cols[1]) as i32;
        let date_x = type_x - ctx.px(self.cols[0]) as i32;
        (date_x, type_x, size_x, size_right)
    }

    /// Граница столбцов под `x` в шапке таблицы: 0 — имя|дата, 1 —
    /// дата|тип, 2 — тип|размер. Черта рисуется на 8 точек левее края
    /// столбца; хватать можно в 5 точках от неё.
    fn col_grip(&self, ctx: Ctx, plan: &Plan, x: i32) -> Option<usize> {
        if self.view != ViewMode::Table || self.on_computer() || self.preview.is_some() || self.error.is_some() {
            return None;
        }
        let (date_x, type_x, size_x, _) = self.columns(ctx, self.table_list(ctx, plan));
        let reach = ctx.px(5) as i32;
        [date_x, type_x, size_x].iter().position(|edge| (x - (edge - ctx.px(8) as i32)).abs() <= reach)
    }

    /// Раскладка вида «Список» (21.11): имена с маленьким значком идут
    /// сверху вниз, столбец за столбцом, а вид листается вбок.
    fn list_plan(&self, ctx: Ctx, plan: &Plan) -> ListPlan {
        let area = plan.list.union(&plan.header);
        let step = ctx.px(LIST_ROW_H + 2);
        let top = ctx.px(8);
        let usable = area.h.saturating_sub(top + ctx.px(kit::SCROLLBAR_W) + ctx.px(6));
        let per_col = (usable / step.max(1)).max(1) as usize;
        let col_w = self.list_col_w(ctx);
        let cols = self.rows.len().div_ceil(per_col) as u32;
        let pad = ctx.px(12);
        ListPlan { area, pad: pad as i32, top: top as i32, step, row_h: ctx.px(LIST_ROW_H), gap: ctx.px(6), per_col, col_w, content: pad * 2 + cols * col_w }
    }

    /// Ширина столбца «Списка»: по самому длинному имени папки, но не уже
    /// 160 и не шире 360 точек — как у Проводника.
    fn list_col_w(&self, ctx: Ctx) -> u32 {
        let key = ctx.px(100);
        if let Some((known, width)) = self.list_w.get() {
            if known == key {
                return width;
            }
        }
        let face = ctx.face(Role::Body);
        let widest = self.rows.iter().map(|row| face.width(self.shown_name(row))).max().unwrap_or(0);
        let width = (widest + ctx.px(8 + 16 + 10 + 18)).clamp(ctx.px(160), ctx.px(360));
        self.list_w.set(Some((key, width)));
        width
    }

    /// Место строки `index` в «Списке» при прокрутке `scroll`.
    fn list_item_rect(&self, list: &ListPlan, index: usize, scroll: i32) -> Rect {
        let col = index / list.per_col.max(1);
        let row = index % list.per_col.max(1);
        Rect::new(
            list.area.x + list.pad + (col as u32 * list.col_w) as i32 - scroll,
            list.area.y + list.top + (row as u32 * list.step) as i32,
            list.col_w.saturating_sub(list.gap),
            list.row_h,
        )
    }

    /// Ошибка чтения или пустая папка вместо строк. `true` — нарисовано.
    fn draw_no_rows(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) -> bool {
        let p = ctx.palette;
        if let Some(error) = &self.error {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                rect.x + ctx.px(10) as i32,
                paint::baseline(ctx, Role::Body, rect),
                rect.w.saturating_sub(ctx.px(20)),
                error,
                p.bad_ink,
            );
            return true;
        }
        if !self.rows.is_empty() {
            return false;
        }
        // Пустая папка (макет 14): по центру значок, надпись и подсказка.
        let side = ctx.px(64);
        let cx = plan.list.x + plan.list.w as i32 / 2;
        let top = plan.list.y + plan.list.h as i32 / 3 - side as i32;
        let icon = Rect::new(cx - side as i32 / 2, top.max(plan.list.y), side, side);
        draw_design_icon(&self.icons, s, icon, "folder");
        let title = "Эта папка пуста";
        let hint = "Перетащите файлы сюда или нажмите «Создать»";
        let y = icon.bottom() + ctx.px(14) as i32;
        let tw = ctx.face(Role::Strong).width(title) as i32;
        paint::text(ctx, s, Role::Strong, cx - tw / 2, y, title, p.ink2);
        let hw = ctx.face(Role::Body).width(hint) as i32;
        paint::text(ctx, s, Role::Body, cx - hw / 2, y + ctx.px(26) as i32, hint, p.ink4);
        true
    }

    /// Вид «Список» (21.11): значок 16 и имя, столбцами; выделение,
    /// отметки и подсветка — как у таблицы.
    fn draw_list_view(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        let list = self.list_plan(ctx, plan);
        if list.area.is_empty() || self.draw_no_rows(s, ctx, plan) {
            return;
        }
        let per = list.per_col.max(1);
        let col_w = list.col_w.max(1);
        let pad = list.pad;
        self.scroll.sync_with(list.area.w, list.content, |index| Some((pad + ((index / per) as u32 * col_w) as i32, col_w)));
        self.page_rows.set(per);
        self.list_rows.set(per);
        let at = self.scroll.offset();
        let first_col = ((at - pad).max(0) as u32 / col_w) as usize;
        let shown_cols = (list.area.w / col_w) as usize + 2;
        let hot = self.hot_now(ctx, plan);
        let end = ((first_col + shown_cols) * per).min(self.rows.len());
        for index in (first_col * per).min(end)..end {
            let row = &self.rows[index];
            let rect = self.list_item_rect(&list, index, at);
            if rect.right() <= list.area.x || rect.x >= list.area.right() {
                continue;
            }
            let selected = self.is_marked(index);
            let state = if selected {
                RowState::Selected
            } else if hot == Hot::Row(index) {
                RowState::Hover
            } else {
                RowState::Idle
            };
            paint::row(ctx, s, rect, state);
            let side = ctx.px(16);
            let icon = Rect::new(rect.x + ctx.px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            let (kind_icon, _) = self.row_art(row);
            if !draw_design_icon(&self.icons, s, icon, kind_icon) {
                let (glyph, tone, filled) = row.icon();
                paint::icon_tile(ctx, s, icon, glyph, tone, filled);
            }
            let name_x = icon.right() + ctx.px(8) as i32;
            // Край вида справа режет имя, как у Проводника.
            let right = rect.right().min(list.area.right() - ctx.px(4) as i32) - ctx.px(6) as i32;
            let ink = if selected { p.ink } else { p.ink2 };
            paint::text_clipped(ctx, s, Role::Body, name_x, paint::baseline(ctx, Role::Body, rect), (right - name_x).max(0) as u32, self.shown_name(row), ink);
        }
    }

    /// Видимая часть, высота всего содержимого и шаг колеса — для прокрутки
    /// списка (или плитки). У «Списка» — ширина всего и шаг в треть столбца.
    fn view_metrics(&self, ctx: Ctx, plan: &Plan) -> (Rect, u32, u32) {
        let step = plan.row_h + plan.row_gap;
        if self.on_computer() || self.error.is_some() {
            return (plan.list, 0, step);
        }
        if self.view == ViewMode::List {
            let list = self.list_plan(ctx, plan);
            return (list.area, list.content, (list.col_w / 3).max(1));
        }
        if self.view == ViewMode::Tiles {
            let grid = plan.grid(ctx);
            let area = plan.list.union(&plan.header);
            let rows = self.rows.len().div_ceil(grid.cols.max(1)) as u32;
            return (area, plan.tiles_top(&grid) + rows * grid.cell_h + grid.margin, grid.cell_h);
        }
        let mut view = plan.list;
        if self.hbar_needed(ctx, plan) {
            view.h = view.h.saturating_sub(ctx.px(kit::SCROLLBAR_W));
        }
        (view, self.rows.len() as u32 * step + ctx.px(4), step)
    }

    /// Полосы прокрутки: вертикальная — когда строки не помещаются,
    /// горизонтальная — когда столбцы.
    fn bars(&self, ctx: Ctx, plan: &Plan) -> (Option<kit::ScrollBar>, Option<kit::ScrollBar>) {
        if self.preview.is_some() {
            return (None, None);
        }
        let (view, content, _) = self.view_metrics(ctx, plan);
        // «Список» листается вбок той же прокруткой `scroll`: его полоса —
        // в первом месте, где её ищут нажатие, перетаскивание и подсветка.
        if self.view == ViewMode::List && !self.on_computer() && self.error.is_none() {
            return (kit::ScrollBar::horizontal(ctx, view, content, &self.scroll), None);
        }
        let vertical = kit::ScrollBar::vertical(ctx, view, content, &self.scroll);
        let horizontal = if self.hbar_needed(ctx, plan) { kit::ScrollBar::horizontal(ctx, plan.list, ctx.px(self.table_min_w()), &self.hscroll) } else { None };
        (vertical, horizontal)
    }

    fn draw_bars(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let (vertical, horizontal) = self.bars(ctx, plan);
        if let Some(bar) = vertical {
            bar.draw(ctx, s, &self.scroll);
        }
        if let Some(bar) = horizontal {
            bar.draw(ctx, s, &self.hscroll);
        }
    }

    /// Нажатие по полосе прокрутки. `true` — попали в неё.
    fn press_bars(&mut self, ctx: Ctx, plan: &Plan, x: i32, y: i32) -> bool {
        let (vertical, horizontal) = self.bars(ctx, plan);
        if let Some(bar) = vertical.filter(|bar| bar.contains(x, y)) {
            bar.press(&self.scroll, x, y);
            println(&format!("files: scrollbar pressed, view at {}", self.scroll.offset()));
            return true;
        }
        if let Some(bar) = horizontal.filter(|bar| bar.contains(x, y)) {
            bar.press(&self.hscroll, x, y);
            return true;
        }
        false
    }
}

/// Текстовый ли файл — по расширению: открывать его в блокноте (фаза 70b).
fn is_text(name: &str) -> bool {
    sysconf::filetypes::is_text(name)
}

/// Программа, которой открываются файлы этого расширения: выбор человека
/// (24.11 — хранилище настроек, `filetypes/.ext/default`; прежняя строка
/// `open_with=` в `/etc/desktop.cfg`), иначе умолчание системы (OBSOS для
/// `.html`). Программа, которой нет, пропускается.
fn chosen_program(name: &str) -> Option<String> {
    let ext = sysconf::filetypes::extension(name);
    if ext.is_empty() {
        return None;
    }
    let key = sysconf::store::default_key(&ext);
    let installed = |command: &String| match sysconf::store::command_program(command) {
        Some(program) if program.starts_with('/') => exists(program),
        Some(_) => true,
        None => false,
    };
    if let Some(command) = user_progs::reg_get_in("user", &key).filter(installed) {
        return Some(command);
    }
    let legacy = read_small_limit("/etc/desktop.cfg", 16 * 1024)
        .and_then(|text| sysconf::value(&text, "open_with").and_then(|list| sysconf::filetypes::chosen(list, &ext)).map(String::from));
    if legacy.is_some() {
        return legacy;
    }
    user_progs::reg_get(&key).filter(installed)
}

/// С какого каталога начать.
///
/// Аргумент бывает и файлом: значок на столе указывает на файл ровно так же,
/// как на папку, и требовать от стола различать их значило бы завести две
/// команды запуска вместо одной.
fn starting_dir(argument: Option<&str>) -> String {
    let Some(path) = argument else {
        return String::from("/");
    };
    if path.is_empty() {
        return String::from("/");
    }
    // «Этот компьютер» — вкладку с ним выносят в новое окно (20.11); без
    // этой строки окно открывалось на корне.
    if path == COMPUTER {
        return String::from(COMPUTER);
    }
    // `\\host\share` and `/net/…` (24.11): opened as is — the window reads
    // it in the background, asks for a password if needed.
    let path = &*sysconf::winpath::to_unix(path);
    if is_net(path) {
        return path.trim_end_matches('/').to_string();
    }
    let mut info = Stat::default();
    if stat(path, &mut info) >= 0 && info.kind == KIND_DIRECTORY {
        return path.to_string();
    }
    // Не каталог или его вовсе нет — показываем то место, где он должен был бы
    // лежать. Пустое окно с сообщением «нет такого пути» человеку бесполезно:
    // он открыл менеджер, чтобы смотреть файлы, а не чтобы читать отказ.
    parent_of(path)
}

/// Какого размера просить окно.
///
/// До 20.11 — две трети экрана, как когда раскладку считало ядро. Доля
/// экрана растёт вместе с экраном, а содержимое — нет, и на 2560×1440 окно
/// выходило вдвое больше нужного; теперь размер — по правилу каркаса
/// ([`app::window_size`]).
fn window_size(info: &SysInfo) -> (u32, u32) {
    // С 20.11 — общее правило каркаса: желаемый размер в точках макета,
    // умноженный на масштаб и ограниченный рабочей областью.
    // Но не уже, чем нужно, чтобы таблица со всеми столбцами встала рядом с
    // боковой колонкой (240 + TABLE_MIN_W + поля, 850 точек макета): на 2560
    // при 200 % доля в 56 % (20.11) прятала «Размер» за прокруткой вбок.
    let (w, h) = app::window_size(info, app::size::FILES);
    let scale = app::ui_scale(info);
    let fit = (850u32 * scale).min(info.screen_w * 85 / 100);
    (w.max(fit), h)
}


// ---------------------------------------------------------------------------
// Адрес (21.11): звенья с подсветкой, шевроны со списком подпапок, правка
// текстом — как в Проводнике Windows 11 и в окне выбора файла.
// ---------------------------------------------------------------------------

/// Звено адреса на панели: подпись, путь, место подписи и шеврона.
struct CrumbSlot {
    label: String,
    path: String,
    rect: Rect,
    chevron: Rect,
}

/// Список у шеврона: чьего звена (`None` — у «…», спрятанные звенья), где
/// его левый край, пункты (подпись, путь; пустой путь — пояснение) и
/// выбранный пункт.
struct CrumbPop {
    which: Option<usize>,
    at: i32,
    items: Vec<(String, String)>,
    selected: usize,
}

/// Место текста в поле адреса, когда его правят.
fn address_text(ctx: Ctx, plan: &Plan) -> Rect {
    let pad = ctx.px(12);
    Rect::new(plan.path.x + pad as i32, plan.path.y, plan.path.w.saturating_sub(pad * 2), plan.path.h)
}

/// Куда ведёт набранный адрес: `~`, `~/…`, абсолютный путь, путь от
/// открытой папки, «Этот компьютер»; `.` и `..` разобраны.
fn resolve_address(text: &str, here: &str, home: &str) -> Option<String> {
    if text.is_empty() {
        return None;
    }
    if text == "Этот компьютер" || text == COMPUTER {
        return Some(String::from(COMPUTER));
    }
    if text == "Сеть" {
        return Some(String::from(sysconf::winpath::NET));
    }
    // `\\host\share` → `/net/host/share`, `C:\…` → `/…` (24.11).
    let text = &*sysconf::winpath::to_unix(text);
    let raw = if text == "~" {
        home.to_string()
    } else if let Some(rest) = text.strip_prefix("~/") {
        join(home, rest)
    } else if text.starts_with('/') {
        text.to_string()
    } else if here == COMPUTER {
        format!("/{text}")
    } else {
        join(here, text)
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Some(if parts.is_empty() { String::from("/") } else { format!("/{}", parts.join("/")) })
}

/// Каталог ли это.
fn is_directory(path: &str) -> bool {
    let mut info = Stat::default();
    stat(path, &mut info) >= 0 && info.kind == KIND_DIRECTORY
}

impl Files {
    /// Звенья адреса, которые помещаются: у каждого — кнопка подписи и
    /// шеврон справа. Не влезают — уходят первые, на их месте «…» (второе
    /// значение — его место, пусто — нет).
    fn crumb_slots(&self, ctx: Ctx, plan: &Plan) -> (Vec<CrumbSlot>, Rect) {
        let field = plan.path;
        if field.is_empty() {
            return (Vec::new(), Rect::EMPTY);
        }
        let face = ctx.face(Role::Body);
        let links = breadcrumb(&self.path, &self.home);
        let pad = ctx.px(CRUMB_PAD) as i32;
        let chev = ctx.px(CRUMB_CHEVRON) as i32;
        let start = field.x + ctx.px(4) as i32;
        let end = field.right() - ctx.px(4) as i32;
        let widths: Vec<i32> = links.iter().map(|(label, _)| face.width(label) as i32 + pad * 2).collect();
        let ellipsis = face.width("…") as i32 + pad * 2;
        let mut first = 0;
        loop {
            let lead = if first > 0 { ellipsis + chev } else { 0 };
            let total: i32 = lead + widths[first..].iter().map(|w| w + chev).sum::<i32>();
            if start + total <= end || first + 1 >= links.len() {
                break;
            }
            first += 1;
        }
        let y = field.y + ctx.px(3) as i32;
        let h = field.h.saturating_sub(ctx.px(6));
        let mut x = start;
        let lead = if first > 0 {
            let rect = Rect::new(x, y, ellipsis as u32, h);
            x += ellipsis;
            rect
        } else {
            Rect::EMPTY
        };
        let mut out = Vec::new();
        for ((label, path), w) in links.into_iter().zip(widths).skip(first) {
            let w = w.min((end - x).max(0));
            let rect = Rect::new(x, y, w as u32, h);
            x += w;
            let cw = chev.min((end - x).max(0));
            let chevron = Rect::new(x, y, cw as u32, h);
            x += cw;
            out.push(CrumbSlot { label, path, rect, chevron });
        }
        (out, lead)
    }

    /// Поле адреса: звенья с подсветкой под указателем или строка правки.
    fn draw_address(&self, s: &mut Surface, bar: Ctx, plan: &Plan) {
        let ctx = bar;
        let p = ctx.palette;
        let radius = ctx.px(theme::R_ROW);
        paint::sunk(bar, s, plan.path, radius);
        let field = bar.on(bar.flat(p.sunk));
        if let Some(line) = &self.address {
            draw::rounded_stroke(s, plan.path, radius, p.acc, 255);
            line.draw(field, s, address_text(ctx, plan), true, p.ink);
            return;
        }
        let (slots, lead) = self.crumb_slots(ctx, plan);
        let y = paint::baseline(field, Role::Body, plan.path);
        let pad = ctx.px(CRUMB_PAD) as i32;
        let open = self.crumb_pop.as_ref().map(|pop| pop.which);
        if !lead.is_empty() {
            if self.hot == Hot::CrumbMore || open == Some(None) {
                paint::row(field, s, lead, RowState::Hover);
            }
            paint::text(field, s, Role::Body, lead.x + pad, y, "…", p.ink3);
        }
        let last = slots.len().saturating_sub(1);
        let mut end = plan.path.x + ctx.px(12) as i32;
        for (index, slot) in slots.iter().enumerate() {
            if self.hot == Hot::Crumb(index) {
                paint::row(field, s, slot.rect, RowState::Hover);
            }
            let open_here = open == Some(Some(index));
            if self.hot == Hot::Chevron(index) || open_here {
                paint::row(field, s, slot.chevron, RowState::Hover);
            }
            let room = slot.rect.w.saturating_sub(pad as u32 * 2);
            paint::text_clipped(field, s, Role::Body, slot.rect.x + pad, y, room, &slot.label, if index == last { p.ink } else { p.ink3 });
            // Разделитель — значок «›» (в шрифте такого знака нет); у
            // раскрытого списка — «⌄», как в Проводнике.
            let glyph = ctx.px(10);
            if slot.chevron.w >= glyph {
                let icon = if open_here { Icon::ChevronDown } else { Icon::ChevronRight };
                let gx = slot.chevron.x + (slot.chevron.w as i32 - glyph as i32) / 2;
                glyphicon::draw(s, icon, gx, plan.path.y + (plan.path.h as i32 - glyph as i32) / 2, glyph, p.ink4, 255);
            }
            end = slot.chevron.right();
        }
        if let Some(preview) = self.preview.as_ref() {
            let right = plan.path.right() - ctx.px(8) as i32;
            paint::text_clipped(field, s, Role::Body, end, y, (right - end).max(0) as u32, &format!(" ·  {}", preview.name), p.ink4);
        }
        // Список у шеврона рисуется последним — поверх списка файлов
        // (см. [`Files::draw_crumb_pop`]).
    }

    /// Начать правку адреса текстом: весь путь выделен, как в Проводнике.
    fn edit_address(&mut self) {
        let text = if self.on_computer() {
            String::from("Этот компьютер")
        } else if is_net(&self.path) {
            net_title(&self.path)
        } else {
            self.path.clone()
        };
        self.address = Some(Line::selected(&text));
        self.crumb_pop = None;
        self.searching = false;
        println(&format!("files: address editing '{text}'"));
    }

    /// Enter в поле адреса: перейти. Файл — открыть его папку и выбрать его.
    fn commit_address(&mut self) -> bool {
        let text = self.address.as_ref().map(|line| line.text.trim().to_string()).unwrap_or_default();
        let Some(target) = resolve_address(&text, &self.path, &self.home) else {
            self.address = None;
            return true;
        };
        // A network path is not asked first (24.11): that can take seconds or
        // want a password — the folder answers when it is read.
        if target == COMPUTER || is_net(&target) || is_directory(&target) {
            self.address = None;
            println(&format!("files: address -> '{target}'"));
            if target == self.path {
                self.reload();
            } else {
                self.go_to(target);
            }
            return true;
        }
        if exists(&target) {
            let name = target.rsplit('/').next().unwrap_or("").to_string();
            self.address = None;
            println(&format!("files: address -> file '{target}'"));
            self.go_to(parent_of(&target));
            if let Some(index) = self.rows.iter().position(|row| row.name == name) {
                self.selected = index;
            }
            return true;
        }
        self.note = Some(format!("Путь «{text}» не найден"));
        println(&format!("files: address '{text}' not found"));
        true
    }

    /// Клавиши адреса и списка у шеврона. `None` — не их.
    fn key_address(&mut self, code: u32, mods: i32, latin: u32) -> Option<bool> {
        if let Some(pop) = self.crumb_pop.as_mut() {
            match code {
                WIN_KEY_UP => pop.selected = pop.selected.saturating_sub(1),
                WIN_KEY_DOWN => pop.selected = (pop.selected + 1).min(pop.items.len().saturating_sub(1)),
                ENTER => {
                    let path = pop.items.get(pop.selected).map(|(_, path)| path.clone()).filter(|path| !path.is_empty());
                    self.crumb_pop = None;
                    if let Some(path) = path {
                        self.go_to(path);
                    }
                }
                _ => self.crumb_pop = None,
            }
            return Some(true);
        }
        if self.address.is_some() {
            match code {
                ESCAPE => {
                    self.address = None;
                    println("files: address editing cancelled");
                }
                ENTER | 0x0D => return Some(self.commit_address()),
                _ => {
                    if let Some(line) = self.address.as_mut() {
                        line.key(code, mods, latin);
                    }
                }
            }
            return Some(true);
        }
        let letter = char::from_u32(latin).map(|ch| ch.to_ascii_lowercase());
        let idle = self.menu.is_none() && self.props.is_none() && self.preview.is_none() && self.tab_menu.is_none();
        if idle && ((mods & WIN_MOD_CTRL != 0 && letter == Some('l')) || (mods & WIN_MOD_ALT != 0 && letter == Some('d'))) {
            self.edit_address();
            return Some(true);
        }
        None
    }

    /// Подпапки `path` для списка у шеврона; у «Этого компьютера» — тома.
    fn subfolders(&self, path: &str) -> Vec<(String, String)> {
        if path == COMPUTER {
            return computer_rows().into_iter().map(|row| (volume_title(&row.name, &row.label), row.name.clone())).collect();
        }
        // Under `/net` only what is already read (24.11): another listing
        // here could hold the window for seconds.
        if is_net(path) {
            if path != self.path {
                return Vec::new();
            }
            return self.rows.iter().filter(|row| row.directory).map(|row| (row.name.clone(), join(path, &row.name))).collect();
        }
        let Ok((rows, _)) = list_dir(path) else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = rows
            .into_iter()
            .filter(|row| row.directory && (self.hidden || !row.name.starts_with('.')))
            .map(|row| (row.name.clone(), join(path, &row.name)))
            .collect();
        out.sort_by_key(|(name, _)| name.to_lowercase());
        out
    }

    /// Открыть список у шеврона звена `which` (`None` — у «…»).
    fn open_crumb_pop(&mut self, ctx: Ctx, plan: &Plan, which: Option<usize>) -> bool {
        let (slots, lead) = self.crumb_slots(ctx, plan);
        let (at, mut items) = match which {
            Some(index) => {
                let Some(slot) = slots.get(index) else { return false };
                (slot.chevron.x, self.subfolders(&slot.path))
            }
            None => {
                let links = breadcrumb(&self.path, &self.home);
                let hidden = links.len().saturating_sub(slots.len());
                (lead.x, links.into_iter().take(hidden).collect())
            }
        };
        let total = items.len();
        items.truncate(CRUMB_POP_MAX);
        if total > CRUMB_POP_MAX {
            items.push((format!("… ещё {}", total - CRUMB_POP_MAX), String::new()));
        }
        if items.is_empty() {
            items.push((String::from("Нет вложенных папок"), String::new()));
        }
        // Выбран пункт, ведущий туда, где мы сейчас (или глубже).
        let selected = items.iter().position(|(_, path)| !path.is_empty() && (self.path == *path || self.path.starts_with(&format!("{path}/")))).unwrap_or(0);
        println(&format!("files: crumb list {} item(s)", total));
        self.crumb_pop = Some(CrumbPop { which, at, items, selected });
        self.pop = None;
        true
    }

    /// Место списка у шеврона.
    fn crumb_pop_rect(&self, ctx: Ctx, area: Rect, plan: &Plan) -> Option<Rect> {
        let pop = self.crumb_pop.as_ref()?;
        let entries: Vec<Entry<'_>> = pop.items.iter().map(|(title, _)| Entry { title, icon: None, danger: false, group: 0, ..Entry::NONE }).collect();
        let size = kit::menu_size(ctx, &entries);
        Some(kit::place((pop.at, plan.path.bottom() + ctx.px(2) as i32), size, area))
    }

    fn draw_crumb_pop(&self, s: &mut Surface, ctx: Ctx, area: Rect, plan: &Plan) {
        let (Some(pop), Some(rect)) = (self.crumb_pop.as_ref(), self.crumb_pop_rect(ctx, area, plan)) else {
            return;
        };
        let entries: Vec<Entry<'_>> = pop.items.iter().map(|(title, _)| Entry { title, icon: None, danger: false, group: 0, ..Entry::NONE }).collect();
        kit::draw_menu(ctx, s, rect, &entries, pop.selected);
    }

    /// Щелчок при открытом списке: пункт — перейти; мимо — закрыть (по
    /// другому шеврону — открыть его список).
    fn click_crumb_pop(&mut self, ctx: Ctx, area: Rect, plan: &Plan, x: i32, y: i32) -> bool {
        let rect = self.crumb_pop_rect(ctx, area, plan);
        let Some(pop) = self.crumb_pop.take() else { return false };
        if let Some(rect) = rect {
            let entries: Vec<Entry<'_>> = pop.items.iter().map(|(title, _)| Entry { title, icon: None, danger: false, group: 0, ..Entry::NONE }).collect();
            if let Some(index) = kit::menu_hit(ctx, rect, &entries, x, y) {
                drop(entries);
                let path = pop.items[index].1.clone();
                if !path.is_empty() {
                    self.go_to(path);
                } else {
                    self.crumb_pop = Some(pop);
                }
                return true;
            }
            if rect.contains(x, y) {
                self.crumb_pop = Some(pop);
                return false;
            }
        }
        if plan.path.contains(x, y) {
            let (slots, lead) = self.crumb_slots(ctx, plan);
            if lead.contains(x, y) && pop.which.is_some() {
                return self.open_crumb_pop(ctx, plan, None);
            }
            if let Some(index) = slots.iter().position(|slot| slot.chevron.contains(x, y)) {
                if pop.which != Some(index) {
                    return self.open_crumb_pop(ctx, plan, Some(index));
                }
            }
        }
        true
    }

    /// Указатель над списком у шеврона — пункт под ним выбран.
    fn hover_crumb_pop(&mut self, ctx: Ctx, area: Rect, plan: &Plan, x: i32, y: i32) -> bool {
        let Some(rect) = self.crumb_pop_rect(ctx, area, plan) else { return false };
        let Some(pop) = self.crumb_pop.as_mut() else { return false };
        let entries: Vec<Entry<'_>> = pop.items.iter().map(|(title, _)| Entry { title, icon: None, danger: false, group: 0, ..Entry::NONE }).collect();
        let hit = kit::menu_hit(ctx, rect, &entries, x, y);
        drop(entries);
        match hit {
            Some(index) if index != pop.selected => {
                pop.selected = index;
                true
            }
            _ => false,
        }
    }
}
