//! Значок программы — один и тот же на столе, в меню и на панели задач.
//!
//! У пакета он свой: картинка FIMG, которую `xtask` сделал из значка
//! программы (у Chromium — его логотип из `hicolor`), путь — в поле `icon`
//! манифеста. У системных программ и пакетов без картинки — значок набора на
//! цветной плитке, как был.
//!
//! Окна группируются на панели по программе: ключ — путь исполняемого файла,
//! из которого открыто окно. Три окна «Файлов» — одна кнопка, как в Windows 11.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use mini_ui::glyphicon::Icon;
use mini_ui::image::Image;
use mini_ui::paint::{self, Ctx, Tone};
use mini_ui::{Rect, Surface};

use crate::sync::SpinLock;

/// Как нарисовать значок.
#[derive(Clone)]
pub enum Art {
    /// Значок набора на плитке: картинка, оттенок, залита ли плитка.
    Glyph(Icon, Tone, bool),
    /// Картинка программы.
    Image(Arc<Image>),
    /// Залитая цветом плитка со знаком набора — всегда, без замены картинкой
    /// макета (20.11, системные утилиты: у `Icon::Log` картинка макета —
    /// редактор кода, а журналу нужен свой значок).
    #[allow(dead_code)]
    Tile(Icon, Tone),
    /// Значок темы по имени (22.11) — берётся при каждом рисовании: тему
    /// значков меняют на ходу, и кнопка панели не должна держать старую
    /// картинку. Нет такого значка — плитка со знаком.
    Named(&'static str, Icon, Tone),
}

impl Art {
    /// Значок набора на плитке с оттенком акцента.
    #[must_use]
    pub const fn glyph(icon: Icon) -> Self {
        Self::Glyph(icon, Tone::Accent, false)
    }

    /// Нарисовать в квадрат `rect`.
    pub fn draw(&self, ctx: Ctx, surface: &mut Surface, rect: Rect) {
        match self {
            // С 72d — значок программы из макета, если он есть.
            Self::Glyph(icon, tone, filled) => match design_for(*icon) {
                Some(image) => image.draw(surface, rect),
                None => paint::icon_tile(ctx, surface, rect, *icon, *tone, *filled),
            },
            Self::Image(image) => image.draw(surface, rect),
            Self::Tile(icon, tone) => paint::icon_tile(ctx, surface, rect, *icon, *tone, true),
            Self::Named(name, icon, tone) => match design(name) {
                Some(image) => image.draw(surface, rect),
                None => paint::icon_tile(ctx, surface, rect, *icon, *tone, true),
            },
        }
    }

    /// Картинка ли это (у картинки нет плитки, и рисуется она крупнее).
    #[must_use]
    pub fn is_image(&self) -> bool {
        match self {
            Self::Image(_) => true,
            Self::Glyph(icon, ..) => design_for(*icon).is_some(),
            Self::Tile(..) => false,
            Self::Named(name, ..) => design(name).is_some(),
        }
    }
}

/// Значок системной утилиты (20.11) по имени программы в `/bin`: один и тот
/// же в «Пуске», на панели задач и в заголовке окна. `None` — не утилита.
#[must_use]
pub fn tool_art(program: &str) -> Option<Art> {
    // Имя значка темы — то же, что имя программы.
    let program: &'static str = match program {
        "run" => "run",
        "charmap" => "charmap",
        "sysinfo" => "sysinfo",
        "eventlog" => "eventlog",
        "services" => "services",
        "cleanmgr" => "cleanmgr",
        "admin" => "admin",
        "compmgmt" => "compmgmt",
        "resmon" => "resmon",
        "startup" => "startup",
        "cfgedit" => "cfgedit",
        "screenrec" => "screenrec",
        // «Архиватор 7-Zip» (24.11): значок архива темы.
        "archiver" => "doc-archive",
        // «Фотографии» (24.11): the picture-document icon of the theme.
        "photos" => "doc-image",
        // «Сетевые подключения» (25.11): the network icon of the theme.
        "ncpa" => "network",
        _ => return None,
    };
    let (icon, tone) = match program {
        "run" => (Icon::Prompt, Tone::Accent),
        "charmap" => (Icon::Keyboard, Tone::Ok),
        "sysinfo" => (Icon::Computer, Tone::Accent),
        "eventlog" => (Icon::Log, Tone::Warn),
        "services" => (Icon::Service, Tone::Ok),
        "cleanmgr" => (Icon::Disk, Tone::Bad),
        // П3 (20.11): «Администрирование» и его инструменты.
        "admin" => (Icon::Settings, Tone::Accent),
        "compmgmt" => (Icon::Computer, Tone::Ok),
        "resmon" => (Icon::Chart, Tone::Warn),
        "startup" => (Icon::Power, Tone::Ok),
        "cfgedit" => (Icon::File, Tone::Accent),
        // Запись экрана (22.11).
        "screenrec" => (Icon::Camera, Tone::Bad),
        "doc-archive" => (Icon::Package, Tone::Warn),
        "doc-image" => (Icon::Camera, Tone::Accent),
        "network" => (Icon::Network, Tone::Accent),
        _ => return None,
    };
    // Картинка темы значков (22.11), плитка — если в теме такой нет.
    Some(Art::Named(program, icon, tone))
}

/// Прочитанные картинки: путь и то, что вышло (`None` — файла нет или он не
/// FIMG; второй раз не читаем).
static CACHE: SpinLock<Vec<(String, Option<Arc<Image>>)>> = SpinLock::new(Vec::new());

/// Самый большой файл значка, который стоит читать.
const LIMIT: usize = 512 * 1024;

/// Картинка по пути — из кэша или с диска.
#[must_use]
pub fn image(path: &str) -> Option<Arc<Image>> {
    if let Some((_, image)) = CACHE.lock().iter().find(|(known, _)| known == path) {
        return image.clone();
    }
    // Читается без замка кэша: чтение файла может ждать диска.
    let image = match crate::fs::read(path, LIMIT) {
        Some(Ok((bytes, _))) => Image::parse(&bytes).map(Arc::new),
        _ => None,
    };
    let mut cache = CACHE.lock();
    if !cache.iter().any(|(known, _)| known == path) {
        cache.push((String::from(path), image.clone()));
    }
    image
}

/// Логотип FossAi — знак и знак на плитке (фаза 72a), FIMG 128×128 из
/// `assets/logo`: «Пуск», «О системе». Вшит в ядро, как и обои.
static LOGO: &[u8] = include_bytes!("../../../../assets/logo/fossai-logo.fimg");
static LOGO_TILE: &[u8] = include_bytes!("../../../../assets/logo/fossai-logo-tile.fimg");

/// Разобранные логотипы: распаковываются один раз.
static LOGOS: SpinLock<Option<(Arc<Image>, Arc<Image>)>> = SpinLock::new(None);

fn logos() -> Option<(Arc<Image>, Arc<Image>)> {
    let mut logos = LOGOS.lock();
    if logos.is_none() {
        *logos = Image::parse(LOGO).map(Arc::new).zip(Image::parse(LOGO_TILE).map(Arc::new));
    }
    logos.clone()
}

/// Знак FossAi с прозрачностью.
#[must_use]
pub fn logo() -> Option<Arc<Image>> {
    logos().map(|(mark, _)| mark)
}

/// Знак FossAi на плитке — значок «О системе».
#[must_use]
pub fn logo_tile() -> Option<Arc<Image>> {
    logos().map(|(_, tile)| tile)
}

/// Забыть прочитанное: пакет сняли или поставили заново.
pub fn forget() {
    CACHE.lock().clear();
}

/// Положить в кэш картинку, которой нет на диске, под условным путём
/// (21.11: значки программ Windows из их `.exe`, `ui::winapps`).
pub fn remember(path: &str, image: Arc<Image>) {
    let mut cache = CACHE.lock();
    match cache.iter_mut().find(|(known, _)| known == path) {
        Some((_, slot)) => *slot = Some(image),
        None => cache.push((String::from(path), Some(image))),
    }
}

/// Значок по полю `icon` манифеста: путь к FIMG или имя значка набора.
#[must_use]
pub fn of_manifest(icon: &str) -> Art {
    if icon.starts_with('/') {
        if let Some(image) = image(icon) {
            return Art::Image(image);
        }
        return Art::glyph(Icon::Package);
    }
    // Значок макета по имени (`windows-apps` — программы Windows, 18.11).
    if let Some(image) = design(icon) {
        return Art::Image(image);
    }
    Art::glyph(named(icon))
}

/// Свой значок программы из `/opt/<имя>/…` — `/opt/<имя>/icon.fimg`, если
/// он есть, даже без установленного пакета (OBSOS на телефоне лежит томом
/// раздела `cache`, а не пакетом).
#[must_use]
pub fn opt_icon(path: &str) -> Option<Arc<Image>> {
    let name = path.strip_prefix("/opt/")?.split('/').next().filter(|n| !n.is_empty())?;
    image(&alloc::format!("/opt/{name}/icon.fimg"))
}

/// Значок набора по имени из манифеста.
#[must_use]
pub fn named(name: &str) -> Icon {
    match name {
        "network" => Icon::Network,
        "terminal" => Icon::Terminal,
        "folder" => Icon::Folder,
        "chart" => Icon::Chart,
        "display" => Icon::Display,
        "settings" => Icon::Settings,
        _ => Icon::Package,
    }
}

/// Значок и ключ группы окна программы, запущенной из `path` (окно с
/// заголовком `title`).
///
/// Порядок: пакет, которому принадлежит файл (`/opt/<имя>/…`), — его значок;
/// системная программа из `/bin` — по таблице; прочее — коробка.
#[must_use]
pub fn of_program(path: &str, title: &str) -> (Art, String) {
    // У программ на .NET исполняемый файл один на всех — `/bin/dotnet`;
    // различает их только окно.
    let group = if path == "/bin/dotnet" { alloc::format!("dotnet:{title}") } else { String::from(path) };
    if let Some(rest) = path.strip_prefix("/opt/") {
        let name = rest.split('/').next().unwrap_or("");
        if let Some(launcher) = super::panel::list_packages().into_iter().find(|launcher| launcher.name == name) {
            return (of_manifest(&launcher.icon), group);
        }
        if let Some(image) = opt_icon(path) {
            return (Art::Image(image), group);
        }
    }
    let program = path.rsplit('/').next().unwrap_or(path);
    if let Some(art) = tool_art(program) {
        return (art, group);
    }
    let art = match program {
        // «Файлы» на столе залиты акцентом — и на панели так же.
        "files" | "mc" => Art::Glyph(Icon::Folder, Tone::Accent, true),
        "taskmgr" | "sysmon" => Art::glyph(Icon::Chart),
        "devmgr" => Art::glyph(Icon::Devices),
        "notepad" => Art::glyph(Icon::File),
        "winshow" | "cdraw" => Art::glyph(Icon::Display),
        "calc" => Art::glyph(Icon::Grid),
        _ => Art::glyph(Icon::Package),
    };
    (art, group)
}

/// Значок и ключ группы окна задачи `task` — по пути её программы.
#[must_use]
pub fn of_task(task: u32, title: &str) -> (Art, String) {
    match crate::user::path_of_task(task) {
        Some(path) => of_program(&path, title),
        None => (Art::glyph(Icon::Package), alloc::format!("task:{task}")),
    }
}

// ── Значки макета (фаза 72d) и темы значков (22.11) ─────────────────────────

/// Пачка темы по умолчанию (`mini_ui::image::ICON_THEMES[0]`) — вшита: стол
/// рисует значки и тогда, когда корень ещё не прочитан или пачки на нём нет.
/// Остальные темы читаются с диска, `/usr/share/icons/<тема>.pak`.
static BUILTIN: &[u8] = include_bytes!("../../../../initrd/usr/share/icons/glossy.pak");

/// Самая большая пачка темы, которую стоит читать.
const PACK_LIMIT: usize = 16 * 1024 * 1024;

/// Прочитанные с диска пачки: номер темы и байты (`None` — файла нет).
static PACKS: SpinLock<Vec<(u8, Option<Arc<Vec<u8>>>)>> = SpinLock::new(Vec::new());

/// Разобранные значки: тема, имя и картинка. Разбираются по требованию.
static DESIGN: SpinLock<Vec<(u8, String, Option<Arc<Image>>)>> = SpinLock::new(Vec::new());

/// Байты значка `name` темы `theme`.
fn pack_bytes(theme: u8, name: &str) -> Option<Vec<u8>> {
    if theme == 0 {
        return mini_ui::image::pack_entry(BUILTIN, name).map(<[u8]>::to_vec);
    }
    let known = PACKS.lock().iter().find(|(t, _)| *t == theme).map(|(_, pack)| pack.clone());
    let pack = match known {
        Some(pack) => pack,
        None => {
            let id = mini_ui::image::ICON_THEMES.get(usize::from(theme))?.0;
            let path = mini_ui::image::icon_pack_path(id);
            // Читается без замка: чтение файла может ждать диска.
            let pack = match crate::fs::read(&path, PACK_LIMIT) {
                Some(Ok((bytes, _))) if mini_ui::image::pack_index_end(&bytes).is_some() => Some(Arc::new(bytes)),
                _ => {
                    crate::kprintln!("  desktop     : icon theme '{id}': no pack at {path}, built-in icons instead");
                    None
                }
            };
            PACKS.lock().push((theme, pack.clone()));
            pack
        }
    };
    mini_ui::image::pack_entry(pack.as_ref()?, name).map(<[u8]>::to_vec)
}

/// Образцы темы на странице «Оформление» (`settings::pages`).
pub const PREVIEW: [&str; 6] = ["folder", "files", "doc-text", "terminal", "settings", "place-this-pc"];

/// Значок `name` темы `theme` (и для образцов в «Параметрах»).
#[must_use]
pub fn theme_design(theme: u8, name: &str) -> Option<Arc<Image>> {
    if let Some((.., image)) = DESIGN.lock().iter().find(|(t, known, _)| *t == theme && known == name) {
        return image.clone();
    }
    let image = pack_bytes(theme, name).and_then(|bytes| Image::parse(&bytes)).map(Arc::new);
    let mut cache = DESIGN.lock();
    if !cache.iter().any(|(t, known, _)| *t == theme && known == name) {
        cache.push((theme, String::from(name), image.clone()));
    }
    image
}

/// Значок макета по имени (`files`, `doc-text`, `place-trash-empty`…) в
/// текущей теме; нет в ней — из темы по умолчанию.
#[must_use]
pub fn design(name: &str) -> Option<Arc<Image>> {
    if name.is_empty() {
        return None;
    }
    let theme = mini_ui::image::icon_theme();
    theme_design(theme, name).or_else(|| if theme == 0 { None } else { theme_design(0, name) })
}

/// Сменить тему значков (номер в `ICON_THEMES`). `true` — сменилась.
pub fn set_theme(theme: u8) -> bool {
    let changed = mini_ui::image::set_icon_theme(theme);
    if changed {
        // Разобранный значок со всеми размерами — под сто килобайт: значки
        // прежних тем не держать (кроме образцов страницы «Оформление»).
        let current = mini_ui::image::icon_theme();
        DESIGN.lock().retain(|(t, name, _)| *t == current || PREVIEW.contains(&name.as_str()));
        crate::kprintln!("  desktop     : icon theme {}", mini_ui::image::icon_theme_id());
    }
    changed
}

/// Прочитать `icon_theme=` из `desktop.cfg` (зовёт `prefs::adopt`).
pub fn adopt(text: &str, path: &str) {
    if let Some(id) = sysconf::value(text, "icon_theme") {
        match mini_ui::image::icon_theme_index(id.trim()) {
            Some(theme) => {
                set_theme(theme);
                crate::kprintln!("  desktop     : icon theme {} from {path}", id.trim());
            }
            None => crate::kprintln!("  desktop     : unknown icon theme '{}' in {path}", id.trim()),
        }
    }
}

/// Запомнить тему значков.
pub fn store() -> Result<(), crate::vfs::VfsError> {
    super::prefs::store_line("icon_theme", mini_ui::image::icon_theme_id())
}

/// `ui icons glossy|crisp` — то же, что «Параметры → Оформление → Стиль
/// значков», для стенда; без слова — какая сейчас.
pub fn command(words: &str) {
    let word = words.trim();
    if !word.is_empty() {
        let Some(theme) = mini_ui::image::icon_theme_index(word) else {
            let ids: Vec<&str> = mini_ui::image::ICON_THEMES.iter().map(|(id, ..)| *id).collect();
            crate::kprintln!("  desktop     : ui icons: {}", ids.join(", "));
            return;
        };
        set_theme(theme);
        if let Err(err) = store() {
            crate::kprintln!("  desktop     : icon theme not saved: {err}");
        }
        let status = super::status_now();
        super::with_desktop(|desktop| {
            desktop.restyle(&status);
            desktop.present();
        });
    }
    let theme = mini_ui::image::icon_theme();
    let missing = ["folder", "files", "terminal", "doc-text", "place-this-pc", "dlg-warning"]
        .iter()
        .filter(|name| theme_design(theme, name).is_none())
        .count();
    crate::kprintln!("  desktop     : icon theme now {} ({missing} of 6 probes missing)", mini_ui::image::icon_theme_id());
}

/// Значок программы из макета для значка набора — того, которым программа
/// рисовалась до 72d. `None` — замены нет, остаётся плитка со знаком.
#[must_use]
pub fn design_for(icon: Icon) -> Option<Arc<Image>> {
    let name = match icon {
        Icon::Folder => "files",
        Icon::Terminal => "terminal",
        Icon::Settings => "settings",
        Icon::Info => "about",
        Icon::Chart => "task-manager",
        Icon::Devices => "device-manager",
        Icon::File => "notepad",
        Icon::Grid => "calculator",
        Icon::Package => "packages",
        Icon::Update => "system-update",
        Icon::Network => "browser",
        Icon::Display => "image-viewer",
        Icon::Log => "code-editor",
        Icon::Computer => "place-this-pc",
        Icon::Disk => "place-local-disk",
        Icon::Keyboard => "keyboard",
        Icon::Clock => "clock",
        Icon::User => "user",
        Icon::Power => "power",
        Icon::Shield => "dlg-shield",
        Icon::Search => "search",
        Icon::Service => "services",
        _ => return None,
    };
    design(name)
}

/// Значок файла по имени: папка или тип по расширению, как в Проводнике
/// (таблица — общая с «Файлами», `sysconf::filetypes`).
#[must_use]
pub fn file_icon(name: &str, directory: bool) -> Option<Arc<Image>> {
    design(sysconf::filetypes::kind_of(name, directory).icon)
}
