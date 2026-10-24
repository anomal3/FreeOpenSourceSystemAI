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
        }
    }

    /// Картинка ли это (у картинки нет плитки, и рисуется она крупнее).
    #[must_use]
    pub fn is_image(&self) -> bool {
        match self {
            Self::Image(_) => true,
            Self::Glyph(icon, ..) => design_for(*icon).is_some(),
        }
    }
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

/// Значок по полю `icon` манифеста: путь к FIMG или имя значка набора.
#[must_use]
pub fn of_manifest(icon: &str) -> Art {
    if icon.starts_with('/') {
        if let Some(image) = image(icon) {
            return Art::Image(image);
        }
        return Art::glyph(Icon::Package);
    }
    Art::glyph(named(icon))
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
    }
    let program = path.rsplit('/').next().unwrap_or(path);
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

// ── Значки макета (фаза 72d) ────────────────────────────────────────────────

/// Значки программ из макета Романа (`assets/icons/app`, FIMG 64): цветная
/// плитка с градиентом и знаком. Рисуются вместо значка набора на плитке.
static APP_ICONS: [(&str, &[u8]); 20] = [
    ("about", include_bytes!("../../../../assets/icons/app/about.fimg").as_slice()),
    ("archiver", include_bytes!("../../../../assets/icons/app/archiver.fimg").as_slice()),
    ("browser", include_bytes!("../../../../assets/icons/app/browser.fimg").as_slice()),
    ("calculator", include_bytes!("../../../../assets/icons/app/calculator.fimg").as_slice()),
    ("code-editor", include_bytes!("../../../../assets/icons/app/code-editor.fimg").as_slice()),
    ("device-manager", include_bytes!("../../../../assets/icons/app/device-manager.fimg").as_slice()),
    ("files", include_bytes!("../../../../assets/icons/app/files.fimg").as_slice()),
    ("generic", include_bytes!("../../../../assets/icons/app/generic.fimg").as_slice()),
    ("help", include_bytes!("../../../../assets/icons/app/help.fimg").as_slice()),
    ("image-viewer", include_bytes!("../../../../assets/icons/app/image-viewer.fimg").as_slice()),
    ("linux-apps", include_bytes!("../../../../assets/icons/app/linux-apps.fimg").as_slice()),
    ("media-player", include_bytes!("../../../../assets/icons/app/media-player.fimg").as_slice()),
    ("notepad", include_bytes!("../../../../assets/icons/app/notepad.fimg").as_slice()),
    ("packages", include_bytes!("../../../../assets/icons/app/packages.fimg").as_slice()),
    ("settings", include_bytes!("../../../../assets/icons/app/settings.fimg").as_slice()),
    ("system-monitor", include_bytes!("../../../../assets/icons/app/system-monitor.fimg").as_slice()),
    ("system-update", include_bytes!("../../../../assets/icons/app/system-update.fimg").as_slice()),
    ("task-manager", include_bytes!("../../../../assets/icons/app/task-manager.fimg").as_slice()),
    ("terminal", include_bytes!("../../../../assets/icons/app/terminal.fimg").as_slice()),
    ("windows-apps", include_bytes!("../../../../assets/icons/app/windows-apps.fimg").as_slice()),
];

/// Значки файлов, папок и мест (`assets/icons/file`, FIMG 64).
static FILE_ICONS: [(&str, &[u8]); 39] = [
    ("doc-archive", include_bytes!("../../../../assets/icons/file/doc-archive.fimg").as_slice()),
    ("doc-audio", include_bytes!("../../../../assets/icons/file/doc-audio.fimg").as_slice()),
    ("doc-code", include_bytes!("../../../../assets/icons/file/doc-code.fimg").as_slice()),
    ("doc-disk-image", include_bytes!("../../../../assets/icons/file/doc-disk-image.fimg").as_slice()),
    ("doc-document", include_bytes!("../../../../assets/icons/file/doc-document.fimg").as_slice()),
    ("doc-executable", include_bytes!("../../../../assets/icons/file/doc-executable.fimg").as_slice()),
    ("doc-font", include_bytes!("../../../../assets/icons/file/doc-font.fimg").as_slice()),
    ("doc-fpk", include_bytes!("../../../../assets/icons/file/doc-fpk.fimg").as_slice()),
    ("doc-image", include_bytes!("../../../../assets/icons/file/doc-image.fimg").as_slice()),
    ("doc-pdf", include_bytes!("../../../../assets/icons/file/doc-pdf.fimg").as_slice()),
    ("doc-presentation", include_bytes!("../../../../assets/icons/file/doc-presentation.fimg").as_slice()),
    ("doc-spreadsheet", include_bytes!("../../../../assets/icons/file/doc-spreadsheet.fimg").as_slice()),
    ("doc-text", include_bytes!("../../../../assets/icons/file/doc-text.fimg").as_slice()),
    ("doc-unknown", include_bytes!("../../../../assets/icons/file/doc-unknown.fimg").as_slice()),
    ("doc-video", include_bytes!("../../../../assets/icons/file/doc-video.fimg").as_slice()),
    ("folder", include_bytes!("../../../../assets/icons/file/folder.fimg").as_slice()),
    ("folder-desktop", include_bytes!("../../../../assets/icons/file/folder-desktop.fimg").as_slice()),
    ("folder-documents", include_bytes!("../../../../assets/icons/file/folder-documents.fimg").as_slice()),
    ("folder-downloads", include_bytes!("../../../../assets/icons/file/folder-downloads.fimg").as_slice()),
    ("folder-empty", include_bytes!("../../../../assets/icons/file/folder-empty.fimg").as_slice()),
    ("folder-full", include_bytes!("../../../../assets/icons/file/folder-full.fimg").as_slice()),
    ("folder-music", include_bytes!("../../../../assets/icons/file/folder-music.fimg").as_slice()),
    ("folder-open", include_bytes!("../../../../assets/icons/file/folder-open.fimg").as_slice()),
    ("folder-pictures", include_bytes!("../../../../assets/icons/file/folder-pictures.fimg").as_slice()),
    ("folder-videos", include_bytes!("../../../../assets/icons/file/folder-videos.fimg").as_slice()),
    ("overlay-error", include_bytes!("../../../../assets/icons/file/overlay-error.fimg").as_slice()),
    ("overlay-lock", include_bytes!("../../../../assets/icons/file/overlay-lock.fimg").as_slice()),
    ("overlay-shared", include_bytes!("../../../../assets/icons/file/overlay-shared.fimg").as_slice()),
    ("overlay-shortcut-arrow", include_bytes!("../../../../assets/icons/file/overlay-shortcut-arrow.fimg").as_slice()),
    ("overlay-sync", include_bytes!("../../../../assets/icons/file/overlay-sync.fimg").as_slice()),
    ("place-external-drive", include_bytes!("../../../../assets/icons/file/place-external-drive.fimg").as_slice()),
    ("place-local-disk", include_bytes!("../../../../assets/icons/file/place-local-disk.fimg").as_slice()),
    ("place-network-folder", include_bytes!("../../../../assets/icons/file/place-network-folder.fimg").as_slice()),
    ("place-optical", include_bytes!("../../../../assets/icons/file/place-optical.fimg").as_slice()),
    ("place-system-disk", include_bytes!("../../../../assets/icons/file/place-system-disk.fimg").as_slice()),
    ("place-this-pc", include_bytes!("../../../../assets/icons/file/place-this-pc.fimg").as_slice()),
    ("place-trash-empty", include_bytes!("../../../../assets/icons/file/place-trash-empty.fimg").as_slice()),
    ("place-trash-full", include_bytes!("../../../../assets/icons/file/place-trash-full.fimg").as_slice()),
    ("place-usb-stick", include_bytes!("../../../../assets/icons/file/place-usb-stick.fimg").as_slice()),
];

/// Разобранные значки макета: имя и картинка. Разбирается по требованию.
static DESIGN: SpinLock<Vec<(&'static str, Option<Arc<Image>>)>> = SpinLock::new(Vec::new());

/// Значок макета по имени (`files`, `doc-text`, `place-trash-empty`…).
#[must_use]
pub fn design(name: &str) -> Option<Arc<Image>> {
    let mut cache = DESIGN.lock();
    if let Some((_, image)) = cache.iter().find(|(known, _)| *known == name) {
        return image.clone();
    }
    let (known, bytes) = APP_ICONS.iter().chain(FILE_ICONS.iter()).find(|(known, _)| *known == name)?;
    let image = Image::parse(bytes).map(Arc::new);
    cache.push((known, image.clone()));
    image
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
        _ => return None,
    };
    design(name)
}

/// Значок файла по имени: папка или тип по расширению, как в Проводнике.
#[must_use]
pub fn file_icon(name: &str, directory: bool) -> Option<Arc<Image>> {
    if directory {
        let kind = match name {
            "Desktop" | "Рабочий стол" => "folder-desktop",
            "Documents" | "Документы" => "folder-documents",
            "Downloads" | "Загрузки" => "folder-downloads",
            "Music" | "Музыка" => "folder-music",
            "Pictures" | "Изображения" => "folder-pictures",
            "Videos" | "Видео" => "folder-videos",
            _ => "folder",
        };
        return design(kind);
    }
    let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()).unwrap_or_default();
    let kind = match ext.as_str() {
        "txt" | "log" | "md" | "cfg" | "conf" | "ini" => "doc-text",
        "doc" | "docx" | "odt" | "rtf" => "doc-document",
        "xls" | "xlsx" | "ods" | "csv" => "doc-spreadsheet",
        "ppt" | "pptx" | "odp" => "doc-presentation",
        "pdf" => "doc-pdf",
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "fimg" | "qoi" => "doc-image",
        "mp4" | "mkv" | "avi" | "webm" | "mov" => "doc-video",
        "mp3" | "wav" | "ogg" | "flac" => "doc-audio",
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" | "deb" => "doc-archive",
        "rs" | "c" | "h" | "cpp" | "py" | "js" | "lua" | "cs" | "sh" | "html" | "css" | "json" => "doc-code",
        "exe" | "dll" | "elf" => "doc-executable",
        "fpk" => "doc-fpk",
        "img" | "iso" | "vdi" => "doc-disk-image",
        "ttf" | "otf" => "doc-font",
        _ => "doc-unknown",
    };
    design(kind)
}
