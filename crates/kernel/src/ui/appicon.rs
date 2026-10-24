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
            Self::Glyph(icon, tone, filled) => paint::icon_tile(ctx, surface, rect, *icon, *tone, *filled),
            Self::Image(image) => image.draw(surface, rect),
        }
    }

    /// Картинка ли это (у картинки нет плитки, и рисуется она крупнее).
    #[must_use]
    pub const fn is_image(&self) -> bool {
        matches!(self, Self::Image(_))
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
