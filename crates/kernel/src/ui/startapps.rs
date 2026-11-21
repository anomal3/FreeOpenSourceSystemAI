//! Манифест программ «Пуска» (20.11.2026).
//!
//! До этого «Пуск» знал программы по двум местам: окна стола — строками в
//! `Menu::new`, программы из `/bin` — таблицей `PROGRAMS` в `panel.rs`, а
//! панель, рабочий стол и «Параметры» спрашивали подписи и значки каждый
//! своим путём. Новая программа требовала правки во всех. Теперь всё, что
//! «Пуск» показывает, кроме пакетов, — строка здесь: ключ, подпись, папка,
//! значок и чем запускать. Пакеты (`/var/lib/pkg`) приходят своим манифестом и
//! ключом `pkg:имя`.
//!
//! Папка — тоже строка манифеста, а не свойство программы: «Системные
//! утилиты» и «SDK C#» закрепляются, открепляются и стоят в списке «Все
//! программы» как одна строка, раскрывающая своё содержимое.

use mini_ui::glyphicon::Icon;

use super::window::App;

/// Чем запускается строка.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Launch {
    /// Окно, которое открывает сам стол.
    App(App),
    /// Программа из `/bin` и хвост её команды (сборка для `mono`/`dotnet`).
    Program(&'static str, &'static str),
}

/// Программа в манифесте.
#[derive(Debug, PartialEq, Eq)]
pub struct Entry {
    /// Ключ — по нему строка закрепляется (`start_pins=`, `taskbar_pins=`).
    pub key: &'static str,
    pub caption: &'static str,
    pub about: &'static str,
    /// Папка (ключ из [`FOLDERS`]) или пусто — строка верхнего уровня.
    pub folder: &'static str,
    /// Значок макета (`assets/icons/app`), если есть.
    pub art: &'static str,
    /// Значок набора — когда значка макета нет.
    pub icon: Icon,
    pub launch: Launch,
    /// Файл, без которого строка не показывается (сборка примера). Пусто —
    /// хватает самой программы в `/bin`.
    pub needs: &'static str,
    /// Ключ группы окон на панели задач: по нему закреплённая кнопка
    /// узнаёт открытое окно своей программы.
    pub group: &'static str,
}

/// Папка «Пуска».
#[derive(Debug, PartialEq, Eq)]
pub struct Folder {
    pub key: &'static str,
    pub caption: &'static str,
    pub about: &'static str,
}

pub static FOLDERS: [Folder; 3] = [
    Folder { key: "folder:utils", caption: "Системные утилиты", about: "калькулятор, блокнот и другие" },
    Folder { key: "folder:sdk", caption: "SDK C#", about: "примеры WinForms и .NET" },
    // Как «Инструменты Windows» (20.11): журнал, службы, очистка и оба
    // диспетчера (у них есть и свои кнопки в «Пуске»).
    Folder { key: "folder:admin", caption: "Средства администрирования", about: "администрирование, журнал, службы, диски" },
];

/// Всё, что ставит в «Пуск» сама система. Порядок — порядок клавиатурного
/// обхода: первые четыре строки («Терминал», «Параметры», «О системе»,
/// «Файлы») стоят на своих местах с фазы С3 — на них считают нажатия
/// сценарии стенда.
pub static ENTRIES: [Entry; 24] = [
    Entry {
        key: "terminal",
        caption: "Терминал",
        about: "командная строка",
        folder: "",
        art: "terminal",
        icon: Icon::Terminal,
        launch: Launch::App(App::Terminal),
        needs: "",
        group: "Terminal",
    },
    Entry {
        key: "settings",
        caption: "Параметры",
        about: "система, экран, панель задач",
        folder: "",
        art: "settings",
        icon: Icon::Settings,
        launch: Launch::App(App::Settings),
        needs: "",
        group: "Settings",
    },
    Entry {
        key: "about",
        caption: "О системе",
        about: "версия и сведения",
        folder: "",
        art: "about",
        icon: Icon::Info,
        launch: Launch::App(App::About),
        needs: "",
        group: "About",
    },
    Entry {
        key: "files",
        caption: "Файлы",
        about: "папки и файлы на дисках",
        folder: "",
        art: "files",
        icon: Icon::Folder,
        launch: Launch::Program("files", ""),
        needs: "",
        group: "/bin/files",
    },
    // Диспетчер задач, а не монитор системы (фаза С5): `sysmon` остался в
    // `/bin` ради стенда, но человеку нужен тот, из которого задачу снимают.
    Entry {
        key: "taskmgr",
        caption: "Диспетчер задач",
        about: "процессы, память и службы",
        folder: "",
        art: "task-manager",
        icon: Icon::Chart,
        launch: Launch::Program("taskmgr", ""),
        needs: "",
        group: "/bin/taskmgr",
    },
    Entry {
        key: "devmgr",
        caption: "Диспетчер устройств",
        about: "устройства и их драйверы",
        folder: "",
        art: "device-manager",
        icon: Icon::Devices,
        launch: Launch::Program("devmgr", ""),
        needs: "",
        group: "/bin/devmgr",
    },
    Entry {
        key: "notepad",
        caption: "Блокнот",
        about: "простой текстовый редактор",
        folder: "folder:utils",
        art: "notepad",
        icon: Icon::File,
        launch: Launch::Program("notepad", ""),
        needs: "",
        group: "/bin/notepad",
    },
    Entry {
        key: "calc",
        caption: "Калькулятор",
        about: "обычный калькулятор",
        folder: "folder:utils",
        art: "calculator",
        icon: Icon::Grid,
        launch: Launch::Program("calc", ""),
        needs: "",
        group: "/bin/calc",
    },
    // Примеры WinForms (фазы 62a–62b) и витрина своей среды .NET. Строки нет,
    // если нет файла примера.
    Entry {
        key: "forms",
        caption: "Форма WinForms",
        about: "пример Mono: кнопка, поле, флажок",
        folder: "folder:sdk",
        art: "code-editor",
        icon: Icon::Grid,
        launch: Launch::Program("mono", "/usr/share/mono/forms.exe"),
        needs: "/usr/share/mono/forms.exe",
        group: "/bin/mono",
    },
    Entry {
        key: "popups",
        caption: "Меню и списки",
        about: "пример Mono: всплывающие окна, вопрос",
        folder: "folder:sdk",
        art: "code-editor",
        icon: Icon::Grid,
        launch: Launch::Program("mono", "/usr/share/mono/popups.exe"),
        needs: "/usr/share/mono/popups.exe",
        group: "/bin/mono",
    },
    Entry {
        key: "gallery",
        caption: "Примеры .NET",
        about: "формы WinForms своей среды",
        folder: "folder:sdk",
        art: "code-editor",
        icon: Icon::Grid,
        launch: Launch::Program("dotnet", "/usr/share/dotnet/samples/gallery.dll"),
        needs: "/usr/share/dotnet/samples/gallery.dll",
        group: "",
    },
    // Системные инструменты (фаза П1/П2, 20.11): в «Системные утилиты» —
    // то, что нужно каждому, в «Средства администрирования» — остальное.
    Entry {
        key: "run",
        caption: "Выполнить",
        about: "открыть программу, папку или файл по имени (Win+R)",
        folder: "folder:utils",
        art: "",
        icon: Icon::Prompt,
        launch: Launch::Program("run", ""),
        needs: "",
        group: "/bin/run",
    },
    Entry {
        key: "charmap",
        caption: "Таблица символов",
        about: "знаки шрифтов системы, копирование",
        folder: "folder:utils",
        art: "",
        icon: Icon::Keyboard,
        launch: Launch::Program("charmap", ""),
        needs: "",
        group: "/bin/charmap",
    },
    Entry {
        key: "sysinfo",
        caption: "Сведения о системе",
        about: "процессор, память, устройства, диски, сеть",
        folder: "folder:utils",
        art: "",
        icon: Icon::Computer,
        launch: Launch::Program("sysinfo", ""),
        needs: "",
        group: "/bin/sysinfo",
    },
    // «Администрирование» (П3, 20.11) — первой строкой папки: центр, из
    // которого открывается всё остальное (ещё кнопка у питания и Win+X).
    Entry {
        key: "admin",
        caption: "Администрирование",
        about: "все настройки и инструменты по группам",
        folder: "folder:admin",
        art: "",
        icon: Icon::Settings,
        launch: Launch::Program("admin", ""),
        needs: "",
        group: "/bin/admin",
    },
    Entry {
        key: "compmgmt",
        caption: "Управление компьютером",
        about: "журнал, службы, устройства, диски, пользователи",
        folder: "folder:admin",
        art: "",
        icon: Icon::Computer,
        launch: Launch::Program("compmgmt", ""),
        needs: "",
        group: "/bin/compmgmt",
    },
    Entry {
        key: "eventlog",
        caption: "Журнал событий",
        about: "журнал загрузки и прошлого сбоя",
        folder: "folder:admin",
        art: "",
        icon: Icon::Log,
        launch: Launch::Program("eventlog", ""),
        needs: "",
        group: "/bin/eventlog",
    },
    Entry {
        key: "services",
        caption: "Службы",
        about: "службы системы: состояние, перезапуск",
        folder: "folder:admin",
        art: "",
        icon: Icon::Service,
        launch: Launch::Program("services", ""),
        needs: "",
        group: "/bin/services",
    },
    Entry {
        key: "cleanmgr",
        caption: "Очистка диска",
        about: "временные файлы, корзина, кэши",
        folder: "folder:admin",
        art: "",
        icon: Icon::Disk,
        launch: Launch::Program("cleanmgr", ""),
        needs: "",
        group: "/bin/cleanmgr",
    },
    Entry {
        key: "resmon",
        caption: "Монитор ресурсов",
        about: "процессор по ядрам, память, диск, сеть",
        folder: "folder:admin",
        art: "",
        icon: Icon::Chart,
        launch: Launch::Program("resmon", ""),
        needs: "",
        group: "/bin/resmon",
    },
    Entry {
        key: "startup",
        caption: "Автозагрузка",
        about: "программы при входе и службы при запуске",
        folder: "folder:admin",
        art: "",
        icon: Icon::Power,
        launch: Launch::Program("startup", ""),
        needs: "",
        group: "/bin/startup",
    },
    Entry {
        key: "cfgedit",
        caption: "Редактор конфигурации",
        about: "файлы /etc с проверкой",
        folder: "folder:admin",
        art: "",
        icon: Icon::File,
        launch: Launch::Program("cfgedit", ""),
        needs: "",
        group: "/bin/cfgedit",
    },
    // Оба диспетчера — ещё и в папке (их строки верхнего уровня остаются).
    Entry {
        key: "admin:taskmgr",
        caption: "Диспетчер задач",
        about: "процессы, память и службы",
        folder: "folder:admin",
        art: "task-manager",
        icon: Icon::Chart,
        launch: Launch::Program("taskmgr", ""),
        needs: "",
        group: "/bin/taskmgr",
    },
    Entry {
        key: "admin:devmgr",
        caption: "Диспетчер устройств",
        about: "устройства и их драйверы",
        folder: "folder:admin",
        art: "device-manager",
        icon: Icon::Devices,
        launch: Launch::Program("devmgr", ""),
        needs: "",
        group: "/bin/devmgr",
    },
];

/// Закреплённое в «Пуске», пока человек ничего не менял.
/// Браузер OBSOS (`pkg:obsos`, 20.11: Роман — «браузер мне в сборку») стоит
/// плиткой и кнопкой панели, когда пакет установлен (на живой флешке — сам
/// с `\FREEOS\PKG\OBSOS.FPK`); без пакета ключ молча пропускается.
pub static DEFAULT_PINS: [&str; 9] = ["terminal", "settings", "about", "files", "pkg:obsos", "taskmgr", "folder:utils", "folder:admin", "folder:sdk"];

/// Закреплённое на панели задач по умолчанию.
pub static DEFAULT_TASK_PINS: [&str; 4] = ["files", "pkg:obsos", "terminal", "settings"];

/// Строка манифеста по ключу.
#[must_use]
pub fn entry(key: &str) -> Option<&'static Entry> {
    ENTRIES.iter().find(|entry| entry.key == key)
}

/// Папка по ключу.
#[must_use]
pub fn folder(key: &str) -> Option<&'static Folder> {
    FOLDERS.iter().find(|folder| folder.key == key)
}

/// Значок строки: картинка макета, если есть, иначе знак набора на плитке.
#[must_use]
pub fn art(entry: &Entry) -> super::appicon::Art {
    // Системные утилиты (20.11) — тем же значком, что их окна на панели.
    if let Launch::Program(program, "") = entry.launch {
        if let Some(art) = super::appicon::tool_art(program) {
            return art;
        }
    }
    match super::appicon::design(entry.art) {
        Some(image) => super::appicon::Art::Image(image),
        None => super::appicon::Art::glyph(entry.icon),
    }
}
