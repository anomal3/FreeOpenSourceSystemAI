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

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use mini_ui::glyphicon::{self, Icon};
use mini_ui::paint::{self, Ctx, RowState, Tone, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Rect, Surface, draw, theme};
use user_abi::{Dirent, KIND_DIRECTORY, Stat};
use user_progs::{
    Args, SYSINFO_DARK, SysInfo, WIN_CLOSE, WIN_KEY, WIN_KEY_DELETE, WIN_KEY_DOWN, WIN_KEY_END,
    WIN_KEY_HOME, WIN_KEY_LEFT, WIN_KEY_MENU, WIN_KEY_NAMED, WIN_KEY_PAGE_DOWN, WIN_KEY_PAGE_UP,
    WIN_KEY_RIGHT, WIN_KEY_UP, WIN_POINTER, Window, close, create, exit, mkdir, monotonic_ms,
    mounts, nanosleep, open, println, read, readdir_raw, remove, rename, stat, sysinfo,
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

/// Пауза между опросами очереди событий.
const POLL_NS: u32 = 30_000_000;

/// Как часто спрашивать систему о теме.
///
/// События «тема изменилась» договор не знает — своего состояния стола у
/// программы нет, — поэтому признак перечитывается. Две секунды: человек
/// переключает тему руками и замечает задержку в две секунды как «сработало», а
/// не как «не сработало».
const THEME_PERIOD_MS: u64 = 2_000;

/// Сколько всего ждать окна при запуске.
///
/// Полминуты, как у монитора системы, и по той же причине: при загрузке система
/// много печатает, а каждая строка в окне оболочки — перерисовка, на которую
/// стол берут целиком.
const OPEN_WAIT_MS: u64 = 30_000;

/// Сколько ждать графики при запуске.
const WAIT_GRAPHICS_MS: u64 = 10_000;

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
    uid: u32,
    gid: u32,
    size: u64,
    /// Для тома на странице «Этот компьютер» — его файловая система; у
    /// записи каталога пусто. Одна структура на обе страницы: выделение,
    /// клавиши и оба вида списка тогда работают одинаково.
    volume: String,
}

impl Row {
    /// Запись каталога.
    fn entry(name: &str, directory: bool, program: bool, entry: &Dirent) -> Self {
        Self {
            name: name.to_string(),
            directory,
            program,
            mode: entry.mode,
            uid: entry.uid,
            gid: entry.gid,
            size: entry.size,
            volume: String::new(),
        }
    }

    /// Том: имя — точка монтирования.
    fn volume(point: &str, kind: &str) -> Self {
        Self {
            name: point.to_string(),
            directory: true,
            program: false,
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            volume: kind.to_string(),
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
    Rename,
    Delete,
    /// Буфера обмена в системе нет, и пункт говорит об этом словами, а не
    /// пропадает: правило вехи v0.7b — нереализованное показывать с пометкой.
    CopyPath,
    Properties,
    NewFolder,
    NewTextFile,
    Refresh,
}

impl MenuItem {
    /// Пункты на записи каталога.
    const ON_ENTRY: [MenuItem; 5] = [
        MenuItem::Open,
        MenuItem::Rename,
        MenuItem::Delete,
        MenuItem::CopyPath,
        MenuItem::Properties,
    ];

    /// Пункты на пустом месте списка.
    const ON_FOLDER: [MenuItem; 3] = [MenuItem::NewFolder, MenuItem::NewTextFile, MenuItem::Refresh];

    /// Пункты на томе.
    const ON_VOLUME: [MenuItem; 2] = [MenuItem::Open, MenuItem::Properties];

    fn title(self) -> &'static str {
        match self {
            MenuItem::Open => "Открыть",
            MenuItem::Rename => "Переименовать",
            MenuItem::Delete => "Удалить",
            MenuItem::CopyPath => "Копировать путь (функция запланирована)",
            MenuItem::Properties => "Свойства",
            MenuItem::NewFolder => "Создать папку",
            MenuItem::NewTextFile => "Создать текстовый файл",
            MenuItem::Refresh => "Обновить",
        }
    }

    fn icon(self) -> Option<Icon> {
        match self {
            MenuItem::Open => Some(Icon::ChevronRight),
            MenuItem::Rename | MenuItem::CopyPath => None,
            MenuItem::Delete => Some(Icon::Close),
            MenuItem::Properties => Some(Icon::Info),
            MenuItem::NewFolder => Some(Icon::Folder),
            MenuItem::NewTextFile => Some(Icon::File),
            MenuItem::Refresh => Some(Icon::Update),
        }
    }

    /// Группа — между разными группами черта.
    const fn group(self) -> u8 {
        match self {
            MenuItem::Open | MenuItem::Rename | MenuItem::Delete => 0,
            MenuItem::CopyPath | MenuItem::Properties => 1,
            MenuItem::NewFolder | MenuItem::NewTextFile => 2,
            MenuItem::Refresh => 3,
        }
    }

    const fn danger(self) -> bool {
        matches!(self, MenuItem::Delete)
    }
}

/// Чем занято меню.
enum MenuMode {
    /// Показывает пункты.
    Items,
    /// Просит новое имя.
    Rename(String),
    /// Спрашивает, удалять ли.
    Confirm,
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
}

/// Свойства записи — карточка на месте списка, как просмотр файла.
struct Props {
    name: String,
    path: String,
    rows: Vec<(&'static str, String)>,
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
    /// Плиткой, а не таблицей.
    tiles: bool,
    /// Последний щелчок по строке: её номер и время — для двойного щелчка.
    last_click: Option<(usize, u64)>,
    menu: Option<Menu>,
    props: Option<Props>,
    /// Ответ последнего действия — в строке состояния, пока не будет
    /// следующего действия.
    note: Option<String>,
}

impl Files {
    fn new(path: String, home: String) -> Self {
        let mut view = Self {
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
            tiles: false,
            last_click: None,
            menu: None,
            props: None,
            note: None,
        };
        view.reload();
        view
    }

    /// На странице «Этот компьютер»?
    fn on_computer(&self) -> bool {
        self.path == COMPUTER
    }

    /// Перечитать текущий каталог — или список томов.
    fn reload(&mut self) {
        self.rows.clear();
        self.selected = 0;
        self.dropped = 0;
        self.error = None;

        if self.on_computer() {
            self.rows = list_volumes();
            return;
        }

        match list_dir(&self.path) {
            Ok((rows, dropped)) => {
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
                // Каталоги наверх, дальше по имени: порядок записей в ext2 —
                // это порядок вставки, то есть для человека случайный.
                self.rows.sort_by(|a, b| {
                    b.directory
                        .cmp(&a.directory)
                        .then_with(|| a.name.cmp(&b.name))
                });
            }
            Err(text) => self.error = Some(text),
        }
    }

    /// Обработать клавишу. `true` — картинку надо перерисовать.
    fn key(&mut self, code: u32) -> bool {
        if self.menu.is_some() {
            return self.key_menu(code);
        }
        if self.preview.is_some() {
            return self.key_preview(code);
        }
        if self.props.is_some() {
            if matches!(code, ESCAPE | BACKSPACE | WIN_KEY_LEFT) {
                self.props = None;
                return true;
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
                if self.rows.get(self.selected).is_some_and(|row| !row.is_volume()) {
                    self.open_menu(None, Some(self.selected));
                    if let Some(menu) = self.menu.as_mut() {
                        menu.mode = MenuMode::Confirm;
                    }
                    return true;
                }
                false
            }
            TILES => {
                self.tiles = !self.tiles;
                println(&format!("files: view {}", if self.tiles { "tiles" } else { "table" }));
                true
            }
            WIN_KEY_UP => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            WIN_KEY_DOWN => {
                if self.selected + 1 < self.rows.len() {
                    self.selected += 1;
                }
                true
            }
            WIN_KEY_HOME => {
                self.selected = 0;
                true
            }
            WIN_KEY_END => {
                self.selected = self.rows.len().saturating_sub(1);
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
        if !self.friendly {
            return &row.name;
        }
        match sysconf::winpath::label(&join(&self.path, &row.name)) {
            Some(label) => label,
            None => &row.name,
        }
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

    /// Войти в каталог, открыть том или файл на просмотр.
    fn open_selected(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected) else {
            return false;
        };
        let target = if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) };
        if row.directory {
            self.go_to(target);
            return true;
        }
        self.preview = Some(read_preview(&row.name, &target));
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
            Some(_) => &MenuItem::ON_ENTRY,
            None if self.on_computer() => &MenuItem::ON_VOLUME[1..],
            None => &MenuItem::ON_FOLDER,
        };
        let (x, y) = at.unwrap_or((0, 0));
        self.menu = Some(Menu {
            x,
            y,
            items: items.to_vec(),
            selected: 0,
            mode: MenuMode::Items,
            target,
        });
        self.note = None;
        let what = match target.and_then(|index| self.rows.get(index)) {
            Some(row) => format!("'{}'", row.name),
            None => String::from("the folder"),
        };
        println(&format!("files: menu opened for {what}"));
    }

    /// Открыть меню у выбранной строки — клавишей.
    fn open_menu_for_selection(&mut self) {
        let target = self.rows.get(self.selected).map(|_| self.selected);
        // Место посчитает отрисовка: у клавиатуры точки нет, и меню встаёт у
        // строки, к которой относится.
        self.open_menu(None, target);
    }

    fn close_menu(&mut self) {
        if self.menu.take().is_some() {
            println("files: menu closed");
        }
    }

    /// Клавиша, пока открыто меню.
    fn key_menu(&mut self, code: u32) -> bool {
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
            MenuMode::Rename(name) => match code {
                ENTER => {
                    let name = name.clone();
                    self.finish_rename(&name);
                    true
                }
                ESCAPE => {
                    self.close_menu();
                    true
                }
                BACKSPACE => {
                    name.pop();
                    true
                }
                // Ctrl+U — очистить, как в редакторе строки оболочки.
                0x15 => {
                    name.clear();
                    true
                }
                code if code < WIN_KEY_NAMED => {
                    // Косая — разделитель пути, а не знак имени; управляющие
                    // знаки — не текст.
                    match char::from_u32(code) {
                        Some(ch) if ch >= ' ' && ch != '/' && ch != '\u{7F}' => {
                            if name.len() + ch.len_utf8() <= 200 {
                                name.push(ch);
                            }
                            true
                        }
                        _ => false,
                    }
                }
                _ => false,
            },
            MenuMode::Confirm => match code {
                ENTER | YES => {
                    self.finish_delete();
                    true
                }
                ESCAPE | NO => {
                    self.close_menu();
                    true
                }
                _ => false,
            },
        }
    }

    /// Выполнить пункт меню.
    fn run_menu_item(&mut self, item: MenuItem) {
        let target = self.menu.as_ref().and_then(|menu| menu.target);
        match item {
            MenuItem::Open => {
                self.close_menu();
                if let Some(index) = target {
                    self.selected = index;
                    self.open_selected();
                }
            }
            MenuItem::Rename => {
                let current = target.and_then(|index| self.rows.get(index)).map(|row| row.name.clone());
                match (self.menu.as_mut(), current) {
                    (Some(menu), Some(name)) => menu.mode = MenuMode::Rename(name),
                    _ => self.close_menu(),
                }
            }
            MenuItem::Delete => {
                if let Some(menu) = self.menu.as_mut() {
                    menu.mode = MenuMode::Confirm;
                }
            }
            MenuItem::CopyPath => {
                self.close_menu();
                println("files: copy path is planned, there is no clipboard yet");
                self.note = Some(String::from("Копировать путь: функция запланирована, буфера обмена ещё нет"));
            }
            MenuItem::Properties => {
                self.close_menu();
                match target {
                    Some(index) => self.show_props(index),
                    None => self.show_folder_props(),
                }
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
        }
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

    /// Удалить строку меню.
    fn finish_delete(&mut self) {
        let Some(index) = self.menu.as_ref().and_then(|menu| menu.target) else {
            self.close_menu();
            return;
        };
        let Some(row) = self.rows.get(index) else {
            self.close_menu();
            return;
        };
        let name = row.name.clone();
        let path = join(&self.path, &name);
        let code = remove(&path);
        self.close_menu();
        if code < 0 {
            println(&format!("files: delete of '{name}' failed: {code}"));
            self.note = Some(format!("Не удалось удалить: {}", error_text(code)));
            return;
        }
        println(&format!("files: deleted '{name}'"));
        self.note = Some(format!("Удалено «{name}»"));
        let keep = self.selected.min(index);
        self.reload();
        self.selected = keep.min(self.rows.len().saturating_sub(1));
    }

    /// Создать папку или пустой текстовый файл в текущем каталоге.
    ///
    /// Имя — как в Windows: «Новая папка», и если такая есть — «Новая папка
    /// (2)» и дальше. Переименовать её человек может тут же, из меню.
    fn create_new(&mut self, directory: bool) {
        let base = if directory { "Новая папка" } else { "Новый текстовый файл" };
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

    /// Показать свойства строки.
    fn show_props(&mut self, index: usize) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let path = if row.is_volume() { row.name.clone() } else { join(&self.path, &row.name) };
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if row.is_volume() {
            rows.push(("Тип", String::from("Том")));
            rows.push(("Точка монтирования", row.name.clone()));
            rows.push(("Файловая система", row.volume.clone()));
            rows.push(("Путь в стиле Windows", sysconf::winpath::to_windows(&row.name)));
        } else {
            let kind = if row.directory {
                "Папка"
            } else if row.name.ends_with(".fpk") {
                "Пакет"
            } else if row.mode & 0o111 != 0 && row.program {
                "Программа"
            } else {
                "Файл"
            };
            rows.push(("Тип", String::from(kind)));
            rows.push(("Расположение", self.path.clone()));
            rows.push(("Путь в стиле Windows", sysconf::winpath::to_windows(&path)));
            if !row.directory {
                rows.push(("Размер", format!("{} байт", row.size)));
            }
            rows.push(("Права", format!("{:04o}", row.mode & 0o7777)));
            rows.push(("Владелец", format!("uid {} · gid {}", row.uid, row.gid)));
        }
        println(&format!("files: properties of '{}'", row.name));
        self.props = Some(Props { name: row.name.clone(), path, rows });
    }

    /// Свойства самого каталога.
    fn show_folder_props(&mut self) {
        let mut rows: Vec<(&'static str, String)> = Vec::new();
        if self.on_computer() {
            rows.push(("Тип", String::from("Этот компьютер")));
            rows.push(("Томов", format!("{}", self.rows.len())));
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
        self.props = Some(Props { name, path: self.path.clone(), rows });
    }

    /// Перейти в каталог, запомнив, откуда пришли.
    ///
    /// Переход вперёд обнуляет список «вперёд» — как в любом обозревателе: путь,
    /// с которого свернули, перестаёт существовать.
    fn go_to(&mut self, path: String) {
        if path == self.path {
            return;
        }
        self.back.push(core::mem::replace(&mut self.path, path));
        self.forward.clear();
        self.preview = None;
        // Ответ прошлого действия относился к прошлому каталогу.
        self.note = None;
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
        self.reload();
        true
    }

    /// Щелчок по окну: координаты внутри поверхности.
    ///
    /// Кнопки навигации, строки быстрого доступа и строки списка ищутся по той
    /// же [`layout`], по которой рисуются, — иначе они разъедутся при первом же
    /// изменении размера окна, и попасть в них будет можно только наугад.
    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);

        // Открытое меню получает щелчок первым: внутри — пункт, снаружи —
        // закрытие, и щелчок мимо на этом заканчивается. Так ведёт себя всякое
        // меню, которое человек видел.
        if let Some(menu) = self.menu.as_ref() {
            if let Some(card) = self.menu_rect(ctx, area) {
                if matches!(menu.mode, MenuMode::Items) && card.contains(x, y) {
                    let row_h = ctx.px(theme::MENU_ROW_H) as i32;
                    let mut top = card.y + ctx.px(6) as i32;
                    let mut hit = None;
                    for (index, item) in menu.items.iter().enumerate() {
                        if index > 0 && item.group() != menu.items[index - 1].group() {
                            top += ctx.px(9) as i32;
                        }
                        if y >= top && y < top + row_h {
                            hit = Some(*item);
                            break;
                        }
                        top += row_h;
                    }
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

        if plan.toolbar.contains(x, y) {
            if plan.nav[0].contains(x, y) {
                return self.go_back();
            }
            if plan.nav[1].contains(x, y) {
                return self.go_forward();
            }
            if plan.nav[2].contains(x, y) {
                return self.go_up();
            }
            if plan.view[0].contains(x, y) || plan.view[1].contains(x, y) {
                let tiles = plan.view[0].contains(x, y);
                if tiles != self.tiles {
                    self.tiles = tiles;
                    println(&format!("files: view {}", if self.tiles { "tiles" } else { "table" }));
                    return true;
                }
            }
            return false;
        }

        if let Some(side) = plan.side {
            if side.contains(x, y) {
                let inner = ctx.on(theme::panel_bg());
                for slot in place_slots(inner, side, &self.home) {
                    if slot.rect.contains(x, y) {
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

        // В просмотре и в свойствах списка нет, и щёлкать в нём не по чему:
        // «попал в невидимую строку» — это выбор вслепую.
        if self.preview.is_some() || self.props.is_some() || !plan.list.contains(x, y) {
            return false;
        }
        let Some(index) = self.row_at(&plan, ctx, x, y) else {
            // Щелчок по пустому месту списка снимает выделение с мысли, но не
            // с экрана: выбранная строка остаётся, как в Windows остаётся
            // курсор. Нужен он для того, чтобы двойной щелчок не открыл
            // строку, по которой попали один раз.
            self.last_click = None;
            return false;
        };
        // Одиночный щелчок выбирает, двойной открывает. Двойного щелчка окно
        // не получает — приходят два обычных, — и «двойной» здесь значит
        // «второй по той же строке не позже [`DOUBLE_CLICK_MS`]».
        let now = monotonic_ms();
        let double = matches!(self.last_click, Some((last, at)) if last == index && now.saturating_sub(at) <= DOUBLE_CLICK_MS);
        self.selected = index;
        if double {
            self.last_click = None;
            return self.open_selected();
        }
        self.last_click = Some((index, now));
        true
    }

    /// Правая кнопка: меню для строки под указателем или для каталога.
    fn click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        let plan = layout(ctx, area);
        if self.menu.is_some() {
            self.close_menu();
            return true;
        }
        if self.preview.is_some() || self.props.is_some() || !plan.list.contains(x, y) {
            return false;
        }
        let target = self.row_at(&plan, ctx, x, y);
        if let Some(index) = target {
            self.selected = index;
        }
        self.open_menu(Some((x, y)), target);
        true
    }

    /// Строка списка под точкой — в таблице или в плитке.
    fn row_at(&self, plan: &Plan, ctx: Ctx, x: i32, y: i32) -> Option<usize> {
        if self.tiles {
            let grid = plan.grid(ctx);
            if grid.cols == 0 || grid.rows == 0 {
                return None;
            }
            let col = ((x - plan.list.x - grid.margin as i32) / grid.cell_w as i32).max(0) as usize;
            let row = ((y - plan.list.y - grid.margin as i32) / grid.cell_h as i32).max(0) as usize;
            if col >= grid.cols || row >= grid.rows {
                return None;
            }
            let index = self.first_tile(&grid) + row * grid.cols + col;
            let cell = plan.tile_rect(&grid, self.first_tile(&grid), index)?;
            if !cell.contains(x, y) {
                return None;
            }
            return (index < self.rows.len()).then_some(index);
        }
        let visible = plan.visible();
        let step = (plan.row_h + plan.row_gap).max(1) as i32;
        let offset = ((y - plan.list.y) / step).max(0) as usize;
        if offset >= visible {
            return None;
        }
        let index = self.first_visible(visible) + offset;
        (index < self.rows.len()).then_some(index)
    }

    /// Первая показанная плитка при такой сетке.
    fn first_tile(&self, grid: &Grid) -> usize {
        let per_page = grid.cols * grid.rows;
        if per_page == 0 || self.selected < per_page {
            return 0;
        }
        // Прокрутка целыми рядами: страница начинается с начала ряда, иначе
        // плитки съезжали бы на одну при каждом шаге выделения.
        let row = self.selected / grid.cols;
        let first_row = row + 1 - grid.rows;
        first_row * grid.cols
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
                Some(index) if self.tiles => {
                    let grid = plan.grid(ctx);
                    let first = self.first_tile(&grid);
                    plan.tile_rect(&grid, first, index)
                        .map_or((plan.list.x, plan.list.y), |r| (r.x + r.w as i32 / 2, r.bottom()))
                }
                Some(index) => {
                    let visible = plan.visible();
                    let first = self.first_visible(visible);
                    let offset = index.saturating_sub(first);
                    let r = plan.row_rect(ctx, offset);
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
        let row_h = ctx.px(theme::MENU_ROW_H);
        match &menu.mode {
            MenuMode::Items => {
                let mut widest = 0;
                for item in &menu.items {
                    widest = widest.max(ctx.face(Role::Body).width(item.title()));
                }
                let mut h = pad * 2;
                for (index, item) in menu.items.iter().enumerate() {
                    if index > 0 && item.group() != menu.items[index - 1].group() {
                        h += ctx.px(9);
                    }
                    h += row_h;
                }
                (widest + ctx.px(14 + 10 + 12) * 2, h)
            }
            MenuMode::Rename(_) | MenuMode::Confirm => {
                let hint = ctx.face(Role::Caption).width(RENAME_HINT).max(ctx.face(Role::Caption).width(CONFIRM_HINT));
                (hint.max(ctx.px(320)) + ctx.px(28), ctx.px(40) + ctx.px(32) + ctx.px(28) + pad * 2)
            }
        }
    }

    /// Нарисовать окно целиком.
    ///
    /// Фон заливается здесь, а не приходит готовым: до переезда область
    /// заливало окно ядра, а у программы поверхность своя и заливать её больше
    /// некому.
    fn draw(&self, surface: &mut Surface, area: Rect, ctx: Ctx) {
        surface.fill(area, theme::window_bg());
        let plan = layout(ctx, area);
        self.draw_toolbar(surface, ctx, &plan);
        if let Some(side) = plan.side {
            self.draw_side(surface, ctx, side);
        }
        if let Some(preview) = self.preview.as_ref() {
            self.draw_preview(surface, ctx, &plan, preview);
        } else if let Some(props) = self.props.as_ref() {
            self.draw_props(surface, ctx, &plan, props);
        } else if self.tiles {
            self.draw_tiles(surface, ctx, &plan);
        } else {
            self.draw_list(surface, ctx, &plan);
        }
        self.draw_status(surface, ctx, &plan);
        self.draw_menu(surface, ctx, area);
    }

    /// Карточка свойств на месте списка.
    fn draw_props(&self, s: &mut Surface, ctx: Ctx, plan: &Plan, props: &Props) {
        let p = ctx.palette;
        let area = plan.list.union(&plan.header);
        if area.is_empty() {
            return;
        }
        let pad = ctx.px(20);
        let card = Rect::new(
            area.x + pad as i32,
            area.y + pad as i32,
            area.w.saturating_sub(pad * 2).min(ctx.px(560)),
            area.h.saturating_sub(pad * 2),
        );
        paint::card(ctx, s, card);
        let inner = ctx.px(18);
        let mut y = card.y + inner as i32;
        let x = card.x + inner as i32;
        let room = card.w.saturating_sub(inner * 2);
        paint::text_clipped(ctx, s, Role::Title, x, y, room, &props.name, p.ink);
        y += i32::from(ctx.face(Role::Title).line) + ctx.px(4) as i32;
        paint::text_clipped(ctx, s, Role::Mono, x, y, room, &props.path, p.ink4);
        y += i32::from(ctx.face(Role::Mono).line) + ctx.px(12) as i32;
        paint::separator(ctx, s, x, y, room);
        y += ctx.px(12) as i32;
        let label_w = ctx.px(190);
        let step = i32::from(ctx.face(Role::Body).line) + ctx.px(10) as i32;
        for (label, value) in &props.rows {
            if y + step > card.bottom() - inner as i32 {
                break;
            }
            paint::text_clipped(ctx, s, Role::Caption, x, y, label_w, label, p.ink4);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                x + label_w as i32,
                y,
                room.saturating_sub(label_w),
                value,
                p.ink2,
            );
            y += step;
        }
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
        let first = self.first_tile(&grid);
        let per_page = grid.cols * grid.rows;
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
            let Some(cell) = plan.tile_rect(&grid, first, index) else {
                break;
            };
            let selected = index == self.selected;
            if selected {
                draw::rounded(s, cell, ctx.px(theme::R_CARD), p.hover1.color, p.hover1.alpha.max(40));
                draw::rounded_stroke(s, cell, ctx.px(theme::R_CARD), p.accline.color, p.accline.alpha);
            }
            let side = ctx.px(theme::ICON_TILE);
            let tile = Rect::new(
                cell.x + (cell.w as i32 - side as i32) / 2,
                cell.y + ctx.px(10) as i32,
                side,
                side,
            );
            let (icon, tone, filled) = row.icon();
            paint::icon_tile(ctx, s, tile, icon, tone, filled);
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
            return volume_title(&row.name);
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
            "Диспетчер устройств — функция запланирована",
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
        let p = ctx.palette;
        draw::shadow(s, card, ctx.px(theme::R_CARD), ctx.px(18), mini_ui::Color::rgb(0, 0, 0), 90);
        draw::rounded(s, card, ctx.px(theme::R_CARD), ctx.flat(p.panel), 255);
        draw::rounded_stroke(s, card, ctx.px(theme::R_CARD), p.line3.color, p.line3.alpha);
        let inner = ctx.on(theme::panel_bg());
        let pad = ctx.px(6);
        match &menu.mode {
            MenuMode::Items => {
                let row_h = ctx.px(theme::MENU_ROW_H);
                let mut y = card.y + pad as i32;
                for (index, item) in menu.items.iter().enumerate() {
                    if index > 0 && item.group() != menu.items[index - 1].group() {
                        let line_y = y + ctx.px(4) as i32;
                        paint::separator(inner, s, card.x + ctx.px(12) as i32, line_y, card.w.saturating_sub(ctx.px(24)));
                        y += ctx.px(9) as i32;
                    }
                    let row = Rect::new(card.x + pad as i32, y, card.w.saturating_sub(pad * 2), row_h);
                    let selected = index == menu.selected;
                    paint::row(inner, s, row, if selected { RowState::Selected } else { RowState::Idle });
                    let icon_side = ctx.px(14);
                    let text_x = row.x + ctx.px(14 + 14 + 10) as i32;
                    if let Some(icon) = item.icon() {
                        glyphicon::draw(
                            s,
                            icon,
                            row.x + ctx.px(14) as i32,
                            row.y + (row.h as i32 - icon_side as i32) / 2,
                            icon_side,
                            if item.danger() { p.bad_ink } else { p.ink3 },
                            255,
                        );
                    }
                    let ink = if item.danger() {
                        p.bad_ink
                    } else if selected {
                        p.ink
                    } else {
                        p.ink2
                    };
                    paint::text_clipped(
                        inner,
                        s,
                        Role::Body,
                        text_x,
                        paint::baseline(inner, Role::Body, row),
                        (row.right() - text_x - ctx.px(12) as i32).max(0) as u32,
                        item.title(),
                        ink,
                    );
                    y += row_h as i32;
                }
            }
            MenuMode::Rename(name) => {
                let x = card.x + ctx.px(14) as i32;
                let room = card.w.saturating_sub(ctx.px(28));
                let mut y = card.y + ctx.px(12) as i32;
                paint::text_clipped(inner, s, Role::Caption, x, y, room, "Новое имя", p.ink4);
                y += ctx.px(22) as i32;
                let field = Rect::new(x, y, room, ctx.px(32));
                paint::sunk(inner, s, field, ctx.px(theme::R_ROW));
                let shown = format!("{name}_");
                paint::text_clipped(
                    inner,
                    s,
                    Role::Mono,
                    field.x + ctx.px(10) as i32,
                    paint::baseline(inner, Role::Mono, field),
                    field.w.saturating_sub(ctx.px(20)),
                    &shown,
                    p.ink,
                );
                y += field.h as i32 + ctx.px(10) as i32;
                paint::text_clipped(inner, s, Role::Caption, x, y, room, RENAME_HINT, p.ink4);
            }
            MenuMode::Confirm => {
                let x = card.x + ctx.px(14) as i32;
                let room = card.w.saturating_sub(ctx.px(28));
                let mut y = card.y + ctx.px(12) as i32;
                let name = menu
                    .target
                    .and_then(|index| self.rows.get(index))
                    .map_or(String::new(), |row| row.name.clone());
                paint::text_clipped(inner, s, Role::Body, x, y, room, &format!("Удалить «{name}»?"), p.ink);
                y += ctx.px(30) as i32;
                let btn_w = ctx.px(120);
                let btn_h = ctx.px(30);
                paint::button(inner, s, Rect::new(x, y, btn_w, btn_h), Weight::Danger, "Удалить", false);
                paint::button(
                    inner,
                    s,
                    Rect::new(x + btn_w as i32 + ctx.px(8) as i32, y, btn_w, btn_h),
                    Weight::Normal,
                    "Отмена",
                    false,
                );
                y += btn_h as i32 + ctx.px(10) as i32;
                paint::text_clipped(inner, s, Role::Caption, x, y, room, CONFIRM_HINT, p.ink4);
            }
        }
    }

    /// Панель инструментов: три стрелки и строка пути.
    fn draw_toolbar(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.toolbar.is_empty() {
            return;
        }
        s.fill(plan.toolbar, ctx.flat(p.panel));
        draw::hline(
            s,
            plan.toolbar.x,
            plan.toolbar.bottom() - 1,
            plan.toolbar.w,
            p.line.color,
            p.line.alpha,
        );
        // Всё, что лежит на панели, сводится поверх **панели**, а не поверх
        // окна: разница в один-два уровня яркости глазом не ловится, а на
        // снимке видна как кнопка чуть другого оттенка, чем соседняя.
        let bar = ctx.on(theme::panel_bg());

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

        // Переключатель вида: две вкладки в одной лунке, как в эскизе.
        if !plan.view_box.is_empty() {
            paint::sunk(bar, s, plan.view_box, ctx.px(theme::R_ROW));
            for (rect, (label, active)) in plan
                .view
                .iter()
                .zip([("Плитка", self.tiles), ("Таблица", !self.tiles)])
            {
                if active {
                    draw::rounded(s, *rect, ctx.px(theme::R_TAB), ctx.flat(p.btn), 255);
                    draw::rounded_stroke(s, *rect, ctx.px(theme::R_TAB), p.btnline.color, p.btnline.alpha);
                }
                paint::text_centered(
                    bar,
                    s,
                    Role::Caption,
                    *rect,
                    label,
                    if active { p.ink2 } else { p.ink4 },
                );
            }
        }

        if plan.path.is_empty() {
            return;
        }
        paint::sunk(bar, s, plan.path, ctx.px(theme::R_ROW));
        // Путь виден только здесь, поэтому он рисуется и в просмотре файла:
        // иначе, открыв файл, человек перестаёт понимать, где находится.
        let place = if self.on_computer() {
            String::from("/  Этот компьютер")
        } else {
            self.path.clone()
        };
        let shown = match self.preview.as_ref() {
            Some(preview) => format!("{place}  ·  {}", preview.name),
            None => place,
        };
        let pad = ctx.px(12);
        let mut x = plan.path.x + pad as i32;
        let mut room = plan.path.w.saturating_sub(pad * 2);
        let y = paint::baseline(bar, Role::Mono, plan.path);
        // Первая косая — акцентом: она отмечает корень, от которого читается
        // всё остальное, и без неё путь сливается в одну серую строку.
        if let Some(rest) = shown.strip_prefix('/') {
            let used = paint::text(bar, s, Role::Mono, x, y, "/", p.acc_ink);
            x += used as i32;
            room = room.saturating_sub(used);
            paint::text_clipped(bar, s, Role::Mono, x, y, room, rest, p.ink3);
        } else {
            paint::text_clipped(bar, s, Role::Mono, x, y, room, &shown, p.ink3);
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
        let pad = ctx.px(12);

        for slot in place_slots(inner, side, &self.home) {
            if let Some(head) = slot.head {
                paint::caps(inner, s, side.x + ctx.px(16) as i32, slot.head_y, head);
            }
            let state = if slot.path == self.path {
                RowState::Selected
            } else {
                RowState::Idle
            };
            paint::row(inner, s, slot.rect, state);
            // В боковой колонке подпись уместна больше всего: это ровно те
            // места, у которых знакомое имя есть, и человек ищет их глазами, а
            // не читает путь.
            let shown = place_label(&slot.path, self.friendly);
            paint::text_clipped(
                inner,
                s,
                Role::Mono,
                slot.rect.x + pad as i32,
                paint::baseline(inner, Role::Mono, slot.rect),
                slot.rect.w.saturating_sub(pad * 2),
                shown,
                paint::row_ink(inner, state),
            );
        }
    }

    /// Список файлов: заголовок столбцов и строки.
    fn draw_list(&self, s: &mut Surface, ctx: Ctx, plan: &Plan) {
        let p = ctx.palette;
        if plan.list.is_empty() {
            return;
        }

        if !plan.header.is_empty() {
            let x = plan.header.x + ctx.px(24) as i32;
            let y = paint::baseline(ctx, Role::MonoCaps, plan.header);
            paint::caps(ctx, s, x, y, "ИМЯ");
            let tail = ctx
                .face(Role::MonoCaps)
                .width_tracked("РАЗМЕР", theme::CAPS_TRACKING * ctx.scale);
            paint::caps(
                ctx,
                s,
                plan.header.right() - ctx.px(22) as i32 - tail as i32,
                y,
                "РАЗМЕР",
            );
            draw::hline(
                s,
                plan.header.x + ctx.px(12) as i32,
                plan.header.bottom() - 1,
                plan.header.w.saturating_sub(ctx.px(24)),
                p.line.color,
                p.line.alpha,
            );
        }

        let visible = plan.visible();
        if visible == 0 {
            return;
        }

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
            return;
        }
        if self.rows.is_empty() {
            let rect = plan.row_rect(ctx, 0);
            paint::text_clipped(
                ctx,
                s,
                Role::Body,
                rect.x + ctx.px(10) as i32,
                paint::baseline(ctx, Role::Body, rect),
                rect.w.saturating_sub(ctx.px(20)),
                "Пусто",
                p.ink5,
            );
            return;
        }

        // Прокрутка считается здесь, а не хранится: сколько строк помещается,
        // знает только тот, кто рисует, а размер окна может измениться.
        let first = self.first_visible(visible);
        for (offset, row) in self.rows.iter().skip(first).take(visible).enumerate() {
            let rect = plan.row_rect(ctx, offset);
            let selected = first + offset == self.selected;
            let state = if selected { RowState::Selected } else { RowState::Idle };
            paint::row(ctx, s, rect, state);

            let tile_side = ctx.px(26);
            let tile = Rect::new(
                rect.x + ctx.px(6) as i32,
                rect.y + (rect.h as i32 - tile_side as i32) / 2,
                tile_side,
                tile_side,
            );
            let (icon, tone, filled) = row.icon();
            paint::icon_tile(ctx, s, tile, icon, tone, filled);

            // Размер и права прижаты к правому краю: выровненные по левому краю
            // случайной длины имени, столбцы чисел не читаются вовсе.
            let mut right = rect.right() - ctx.px(10) as i32;
            let small = paint::baseline(ctx, Role::MonoSmall, rect);
            let size = if row.is_volume() {
                row.volume.clone()
            } else if row.directory {
                String::from("—")
            } else {
                size_text(row.size)
            };
            paint::text_right(ctx, s, Role::MonoSmall, right, small, &size, p.ink4);
            right -= ctx.px(76) as i32;
            if rect.w > ctx.px(360) && !row.is_volume() {
                let meta = format!("{:04o} {}:{}", row.mode & 0o7777, row.uid, row.gid);
                paint::text_right(ctx, s, Role::MonoSmall, right, small, &meta, p.ink4);
                right -= ctx.px(120) as i32;
            }

            let name_x = tile.right() + ctx.px(10) as i32;
            let room = (right - name_x).max(0) as u32;
            let (role, ink) = if selected {
                (Role::Title, p.ink)
            } else if row.directory {
                (Role::Label, p.ink2)
            } else {
                (Role::Body, p.ink3)
            };
            let shown = if row.is_volume() {
                volume_title(&row.name)
            } else {
                self.shown_name(row).to_string()
            };
            paint::text_clipped(
                ctx,
                s,
                role,
                name_x,
                paint::baseline(ctx, role, rect),
                room,
                &shown,
                ink,
            );
        }
        if self.on_computer() {
            let grid = Grid {
                cols: 1,
                rows: visible,
                cell_w: plan.list.w,
                cell_h: plan.row_h + plan.row_gap,
                margin: 0,
            };
            self.draw_computer_facts(s, ctx, plan, &grid);
        }
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
        let p = ctx.palette;
        if plan.status.is_empty() {
            return;
        }
        s.fill(plan.status, ctx.flat(p.panel));
        draw::hline(
            s,
            plan.status.x,
            plan.status.y,
            plan.status.w,
            p.line2.color,
            p.line2.alpha,
        );
        let bar = ctx.on(theme::panel_bg());
        let text = if let Some(note) = self.note.as_ref() {
            note.clone()
        } else if self.preview.is_some() {
            String::from("Стрелки — листать    Esc — назад к списку")
        } else if self.props.is_some() {
            String::from("Esc — назад к списку")
        } else if self.error.is_some() {
            String::from("Стрелка влево — назад    Backspace — вверх")
        } else if self.dropped != 0 {
            format!(
                "{} из {} объектов    показаны не все",
                self.rows.len(),
                self.rows.len() + self.dropped
            )
        } else if self.on_computer() {
            format!("{} томов    двойной щелчок — открыть    правая кнопка — меню", self.rows.len())
        } else {
            format!(
                "{} объектов    двойной щелчок — открыть    правая кнопка — меню    Backspace — вверх    V — {}",
                self.rows.len(),
                if self.friendly { "настоящие имена" } else { "знакомый вид" }
            )
        };
        let pad = ctx.px(14);
        paint::text_clipped(
            bar,
            s,
            Role::MonoSmall,
            plan.status.x + pad as i32,
            paint::baseline(bar, Role::MonoSmall, plan.status),
            plan.status.w.saturating_sub(pad * 2),
            &text,
            p.ink4,
        );
    }

    /// Первая показанная строка при таком числе видимых.
    fn first_visible(&self, visible: usize) -> usize {
        if visible == 0 || self.selected < visible {
            0
        } else {
            self.selected + 1 - visible
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
/// Ответы на вопрос об удалении.
const YES: u32 = 'y' as u32;
const NO: u32 = 'n' as u32;
/// Подсказки под полем имени и под вопросом.
const RENAME_HINT: &str = "Enter — переименовать    Esc — отмена    Ctrl+U — очистить";
const CONFIRM_HINT: &str = "Enter или Y — удалить    Esc или N — оставить";

/// Обновить список. `r` — потому что F-ряд договор программам не отдаёт.
const REFRESH: u32 = 'r' as u32;

/// Переключить знакомый вид на настоящие имена и обратно.
const VIEW: u32 = 'v' as u32;
/// Закрыть окно.
const QUIT: u32 = 'q' as u32;

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

struct Plan {
    toolbar: Rect,
    /// «Назад», «вперёд», «вверх» — в этом порядке.
    nav: [Rect; 3],
    path: Rect,
    /// Лунка переключателя вида и две его вкладки: плитка, таблица.
    view_box: Rect,
    view: [Rect; 2],
    side: Option<Rect>,
    header: Rect,
    list: Rect,
    status: Rect,
    row_h: u32,
    row_gap: u32,
}

impl Plan {
    /// Сколько строк списка помещается.
    fn visible(&self) -> usize {
        let step = (self.row_h + self.row_gap).max(1);
        ((self.list.h + self.row_gap) / step) as usize
    }

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

    /// Плитка с таким номером при такой первой показанной.
    fn tile_rect(&self, grid: &Grid, first: usize, index: usize) -> Option<Rect> {
        if grid.cols == 0 || index < first {
            return None;
        }
        let area = self.list.union(&self.header);
        let offset = index - first;
        let row = offset / grid.cols;
        let col = offset % grid.cols;
        if row >= grid.rows {
            return None;
        }
        let top = if self.list.y > area.y { self.list.y - area.y } else { 0 } as u32;
        Some(Rect::new(
            area.x + (grid.margin + col as u32 * grid.cell_w) as i32,
            area.y + (top.max(grid.margin) + row as u32 * grid.cell_h) as i32,
            grid.cell_w.saturating_sub(ctx_gap()),
            grid.cell_h.saturating_sub(ctx_gap()),
        ))
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

/// Посчитать раскладку окна.
///
/// Одна функция на отрисовку и на щелчок — см. заголовок модуля.
fn layout(ctx: Ctx, area: Rect) -> Plan {
    let pad = ctx.px(14);
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

    let path_h = ctx.px(32);
    let path_x = x + ctx.px(4) as i32;

    // Переключатель вида справа; на узком окне его нет — путь важнее.
    let tab_w = ctx.px(72);
    let tab_h = ctx.px(26);
    let box_w = tab_w * 2 + ctx.px(6);
    let (view_box, view, path_right) = if area.w > ctx.px(560) {
        let box_x = area.right() - pad as i32 - box_w as i32;
        let box_y = toolbar.y + (toolbar_h as i32 - path_h as i32) / 2;
        let view_box = Rect::new(box_x, box_y, box_w, path_h);
        let tab_y = box_y + (path_h as i32 - tab_h as i32) / 2;
        let tabs = [
            Rect::new(box_x + ctx.px(3) as i32, tab_y, tab_w, tab_h),
            Rect::new(box_x + (ctx.px(3) + tab_w) as i32, tab_y, tab_w, tab_h),
        ];
        (view_box, tabs, box_x - ctx.px(8) as i32)
    } else {
        (Rect::EMPTY, [Rect::EMPTY; 2], area.right() - pad as i32)
    };

    let path_w = (path_right - path_x).max(0) as u32;
    let path = Rect::new(
        path_x,
        toolbar.y + (toolbar_h as i32 - path_h as i32) / 2,
        path_w,
        path_h,
    );

    let status_h = ctx.px(theme::STATUS_H);
    let status_y = (area.bottom() - status_h as i32).max(toolbar.bottom());
    let status = Rect::new(area.x, status_y, area.w, status_h);

    let top = toolbar.bottom();
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
        toolbar,
        nav,
        path,
        view_box,
        view,
        side,
        header,
        list,
        status,
        row_h: ctx.px(32),
        row_gap: ctx.px(2),
    }
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
}

/// Куда ведёт быстрый доступ.
///
/// Список короткий и составлен из того, что в системе точно есть: «Этот
/// компьютер», корень, дом, стол и два системных каталога. Закладок человек
/// пока не заводит — заводить их некуда, файла настроек у стола нет.
fn places(home: &str) -> [(Option<&'static str>, String); 6] {
    [
        (Some("МЕСТА"), String::from(COMPUTER)),
        (None, String::from("/")),
        (None, home.to_string()),
        (None, format!("{home}/Desktop")),
        (Some("СИСТЕМА"), String::from("/bin")),
        (None, String::from("/etc")),
    ]
}

/// Как называется место в боковой колонке.
///
/// Корень здесь — «Локальный диск (C:)», а не «Этот компьютер»: тем именем
/// подписана страница томов строкой выше, и два одинаковых имени подряд
/// читались бы как одно место, нарисованное дважды.
fn place_label(path: &str, friendly: bool) -> &str {
    if path == COMPUTER {
        return "Этот компьютер";
    }
    if !friendly {
        return path;
    }
    if path == "/" {
        return "Локальный диск (C:)";
    }
    sysconf::winpath::label(path).unwrap_or(path)
}

/// Имя тома на странице «Этот компьютер».
fn volume_title(point: &str) -> String {
    match point {
        "/" => String::from("Локальный диск (C:)"),
        "/data" => String::from("Данные (/data)"),
        other => other.to_string(),
    }
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
            let (point, kind) = line.split_once('\t')?;
            Some(Row::volume(point, kind))
        })
        .collect()
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
fn place_slots(ctx: Ctx, side: Rect, home: &str) -> Vec<PlaceSlot> {
    let padx = ctx.px(16);
    let row_h = ctx.px(32);
    let gap = ctx.px(2);
    let caps_h = u32::from(ctx.face(Role::MonoCaps).line) + ctx.px(12);
    let mut y = side.y + ctx.px(10) as i32;
    let mut out = Vec::new();

    for (head, path) in places(home) {
        let head_y = if head.is_some() {
            let at = y + ctx.px(4) as i32;
            y += caps_h as i32;
            at
        } else {
            0
        };
        let rect = Rect::new(
            side.x + padx as i32,
            y,
            side.w.saturating_sub(padx * 2),
            row_h,
        );
        y += (row_h + gap) as i32;
        // Не поместилось — не рисуем: строка, наполовину заехавшая под список,
        // выглядит как испорченная отрисовка, а не как «здесь кончилось место».
        if rect.bottom() > side.bottom() {
            break;
        }
        out.push(PlaceSlot { head, head_y, path, rect });
    }
    out
}

// ---------------------------------------------------------------------------
// Файловая система
// ---------------------------------------------------------------------------

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

/// Размер в виде, который читается с одного взгляда.
fn size_text(bytes: u64) -> String {
    if bytes < 10 * 1024 {
        format!("{bytes}")
    } else if bytes < 10 * 1024 * 1024 {
        format!("{}K", bytes / 1024)
    } else {
        format!("{}M", bytes / (1024 * 1024))
    }
}

/// Прочитать каталог. Возвращает строки и сколько записей не поместилось.
///
/// Ошибка — текстом, а не кодом: показывать её человеку всё равно строкой, а
/// перевод кода в слова в одном месте лучше, чем в трёх.
fn list_dir(path: &str) -> Result<(Vec<Row>, usize), String> {
    let fd = open(path);
    if fd < 0 {
        return Err(format!("cannot open {path}: error {fd}"));
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
            return Err(format!("cannot read {path}: error {step}"));
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

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли из `_start` ровно в том виде, в каком их положило
    // ядро, — это и есть единственный допустимый источник по контракту `Args`.
    let args = unsafe { Args::new(argc, argv) };

    let Some(info) = wait_for_graphics() else {
        // Машина без графики — это не сбой: система работает в серийной линии,
        // и показывать каталог просто негде.
        println("files: no graphics on this machine, nothing to show");
        exit(0)
    };

    // Формат точки и тема — первое, что нужно сделать, и сделать до всякой
    // отрисовки. Оба счётчика у программы свои: адресное пространство своё, и
    // заполненные ядром у себя ей не видны. Без формата окно вышло бы сплошь
    // чёрным при совершенно исправной отрисовке, без темы — светлым на тёмном
    // столе; обе ошибки глазами ищут долго.
    mini_ui::use_raw_format(info.pixel_format);
    theme::set_dark(info.flags & SYSINFO_DARK != 0);

    let start = starting_dir(args.get(1));
    let home = if user_progs::uid() == 0 { "/root" } else { "/home" };

    let (width, height) = window_size(&info);
    let scale = theme::geometry_scale(info.screen_w.max(1));

    let Some(mut window) = open_patiently(width, height) else {
        println("files: FAILED the desktop never freed up; no window");
        exit(1)
    };

    let base = window.pixels().as_mut_ptr();
    // SAFETY: ядро отобразило ровно `width * height` точек по этому адресу и
    // держит их, пока живо окно. Второй ссылки на них нет — `window` больше
    // пикселей никому не отдаёт.
    let Some(mut surface) = (unsafe { Surface::from_raw(base, width, height) }) else {
        println("files: FAILED the surface the kernel gave makes no sense");
        exit(1)
    };

    let area = Rect::new(0, 0, width, height);
    let ctx = Ctx::scaled(scale);
    let mut view = Files::new(start, home.to_string());

    println(&format!("files: window '{TITLE}' opened, {width}x{height}"));
    // Что именно прочитано — в журнал, по разу на каждый каталог. Нарисованное
    // на экране снаружи не проверить, а строка проверяется: она и отличает
    // «список показан» от «окно нарисовано пустым».
    view.report();

    let mut dark = info.flags & SYSINFO_DARK != 0;
    let mut next_theme = monotonic_ms() + THEME_PERIOD_MS;
    let mut dirty = true;
    let reason;

    'live: loop {
        while let Some(event) = window.next_event() {
            match event.kind {
                // Просьба закрыться — крестиком или Ctrl+W. Соглашаемся сразу:
                // несохранённого у менеджера нет.
                WIN_CLOSE => {
                    reason = "request";
                    break 'live;
                }
                WIN_KEY if event.code == QUIT && view.preview.is_none() => {
                    reason = "'q'";
                    break 'live;
                }
                WIN_KEY => {
                    if view.key(event.code) {
                        view.report_if_moved();
                        dirty = true;
                    }
                }
                WIN_POINTER => {
                    // Двойка — правая кнопка, единица — левая.
                    let handled = if event.code == 2 {
                        view.click_right(area, ctx, event.x, event.y)
                    } else {
                        view.click(area, ctx, event.x, event.y)
                    };
                    if handled {
                        view.report_if_moved();
                        dirty = true;
                    }
                }
                _ => {}
            }
        }

        let now = monotonic_ms();
        if now >= next_theme {
            next_theme = now + THEME_PERIOD_MS;
            if let Some(fresh) = sysinfo() {
                let fresh_dark = fresh.flags & SYSINFO_DARK != 0;
                if fresh_dark != dark {
                    dark = fresh_dark;
                    theme::set_dark(dark);
                    println(if dark {
                        "files: repainted for the dark theme"
                    } else {
                        "files: repainted for the light theme"
                    });
                    dirty = true;
                }
            }
        }

        if dirty {
            // Контекст пересобирается на каждый кадр, а не хранится: он держит
            // палитру и сведённую подложку, а обе меняются вместе с темой.
            view.draw(&mut surface, area, Ctx::scaled(scale));
            // Отказ здесь — **не** сбой, и выходить из-за него нельзя. Занятый
            // стол отвечает `ERR_AGAIN`, а пропущенный кадр ничего не стоит:
            // следующий виток нарисует то же самое. Ровно на этом монитор
            // системы падал и поднимался супервизором по кругу.
            if window.commit() >= 0 {
                dirty = false;
            }
        }

        nanosleep(0, POLL_NS);
    }

    println(&format!("files: closing on {reason}"));
    window.close();
    exit(0)
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
            None if self.on_computer() => println(&format!(
                "files: this computer has {} volume(s), selected '{}'",
                self.rows.len(),
                self.selected_name()
            )),
            None => println(&format!(
                "files: {} has {} entries, selected '{}'",
                self.path,
                self.rows.len(),
                self.selected_name()
            )),
        }
    }

    /// Имя выбранной строки, либо прочерк, если выбирать не из чего.
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
/// Две трети экрана — ровно столько занимало это окно, пока его раскладку
/// считало ядро. Считается от экрана, а не задано числом: окно в 850 точек на
/// экране 3840 выглядит маркой на конверте, а на 800×600 не помещается вовсе.
fn window_size(info: &SysInfo) -> (u32, u32) {
    let w = (info.screen_w * 2 / 3).clamp(320, info.screen_w.max(320));
    let h = (info.screen_h * 2 / 3).clamp(240, info.screen_h.max(240));
    (w, h)
}

/// Попросить окно столько раз, сколько нужно.
fn open_patiently(width: u32, height: u32) -> Option<Window> {
    let deadline = monotonic_ms() + OPEN_WAIT_MS;
    loop {
        match Window::open(TITLE, width, height) {
            Ok(window) => return Some(window),
            Err(code) => {
                // Всё, кроме «попробуйте ещё», окончательно: окна такого
                // размера не дадут никогда, сколько ни проси.
                if code != user_progs::ERR_AGAIN {
                    println(&format!("files: FAILED opening the window: {code}"));
                    return None;
                }
            }
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}

/// Дождаться, пока система скажет формат точки, — это и значит «графика есть».
fn wait_for_graphics() -> Option<SysInfo> {
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    loop {
        let info = sysinfo()?;
        if info.pixel_format != 0 && info.screen_w != 0 {
            return Some(info);
        }
        if monotonic_ms() >= deadline {
            return None;
        }
        nanosleep(0, POLL_NS);
    }
}
