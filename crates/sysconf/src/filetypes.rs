//! Типы файлов, как в Windows (02.11, макет FossAi): расширение → тип
//! («Текстовый документ») и его значок, и какой программой человек велел
//! открывать файлы этого расширения.
//!
//! Таблица одна на стол (ядро) и «Файлы» (программа): прежде у каждого был
//! свой список текстовых расширений, и они разошлись — `.lua` открывался
//! блокнотом из «Файлов» и ничем со стола.
//!
//! Выбор человека хранится одной строкой `open_with=` в `desktop.cfg`:
//! `txt:/bin/notepad|html:/opt/debian/bin/bash /opt/chromium/bin/chromium`.
//! Команда — до конца записи, в ней бывают пробелы; `|` и `:` в расширении
//! не встречаются.

use alloc::string::String;

/// Тип файла: имя значка макета и подпись для человека.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Kind {
    pub icon: &'static str,
    pub name: &'static str,
}

/// Расширение имени — строчными, без точки; нет — пусто.
#[must_use]
pub fn extension(name: &str) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => ext.to_ascii_lowercase(),
        _ => String::new(),
    }
}

/// Расширения, которые открывает блокнот.
const TEXT: [&str; 22] = [
    "txt", "log", "md", "cfg", "conf", "ini", "toml", "yaml", "yml", "json", "xml", "csv", "c", "h", "rs", "py", "sh", "lua",
    "cs", "html", "css", "js",
];

/// Текстовое ли расширение — такой файл открывает блокнот. Файл без
/// расширения сюда не попадает: в `/bin` это программы, а не текст.
#[must_use]
pub fn is_text(name: &str) -> bool {
    TEXT.contains(&extension(name).as_str())
}

/// Тип записи по имени, как в Проводнике.
#[must_use]
pub fn kind_of(name: &str, directory: bool) -> Kind {
    if directory {
        let icon = match name {
            "Desktop" | "Рабочий стол" => "folder-desktop",
            "Documents" | "Документы" => "folder-documents",
            "Downloads" | "Загрузки" => "folder-downloads",
            "Music" | "Музыка" => "folder-music",
            "Pictures" | "Изображения" => "folder-pictures",
            "Videos" | "Видео" => "folder-videos",
            _ => "folder",
        };
        return Kind { icon, name: "Папка с файлами" };
    }
    let (icon, name) = match extension(name).as_str() {
        "txt" | "log" | "md" | "cfg" | "conf" | "ini" => ("doc-text", "Текстовый документ"),
        "doc" | "docx" | "odt" | "rtf" => ("doc-document", "Документ"),
        "xls" | "xlsx" | "ods" | "csv" => ("doc-spreadsheet", "Электронная таблица"),
        "ppt" | "pptx" | "odp" => ("doc-presentation", "Презентация"),
        "pdf" => ("doc-pdf", "Документ PDF"),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg" | "fimg" | "qoi" => ("doc-image", "Изображение"),
        "mp4" | "mkv" | "avi" | "webm" | "mov" => ("doc-video", "Видео"),
        "mp3" | "wav" | "ogg" | "flac" => ("doc-audio", "Звук"),
        "zip" | "tar" | "gz" | "xz" | "7z" | "rar" | "deb" => ("doc-archive", "Архив"),
        "rs" | "c" | "h" | "cpp" | "py" | "js" | "lua" | "cs" | "sh" | "html" | "css" | "json" | "toml" | "yaml" | "yml"
        | "xml" => ("doc-code", "Исходный код"),
        "exe" | "dll" | "elf" => ("doc-executable", "Приложение"),
        "fpk" => ("doc-fpk", "Пакет FossAi"),
        "img" | "iso" | "vdi" => ("doc-disk-image", "Образ диска"),
        "ttf" | "otf" => ("doc-font", "Шрифт"),
        _ => ("doc-unknown", "Файл"),
    };
    Kind { icon, name }
}

/// Программа, выбранная для расширения, из строки `open_with=`.
#[must_use]
pub fn chosen<'a>(list: &'a str, ext: &str) -> Option<&'a str> {
    list.split('|').find_map(|entry| {
        let (key, command) = entry.split_once(':')?;
        (key.trim() == ext && !command.trim().is_empty()).then(|| command.trim())
    })
}

/// Строка `open_with=` с новым выбором для расширения (прежний — заменён).
#[must_use]
pub fn with_choice(list: &str, ext: &str, command: &str) -> String {
    let mut out = String::new();
    for entry in list.split('|').filter(|entry| !entry.trim().is_empty()) {
        if entry.split_once(':').is_some_and(|(key, _)| key.trim() == ext) {
            continue;
        }
        if !out.is_empty() {
            out.push('|');
        }
        out.push_str(entry.trim());
    }
    if !out.is_empty() {
        out.push('|');
    }
    out.push_str(ext);
    out.push(':');
    out.push_str(command.trim());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_and_kinds() {
        assert_eq!(extension("Отчёт.TXT"), "txt");
        assert_eq!(extension(".bashrc"), "");
        assert_eq!(extension("README"), "");
        assert_eq!(kind_of("a.md", false).name, "Текстовый документ");
        assert_eq!(kind_of("a.lua", false).icon, "doc-code");
        assert_eq!(kind_of("Загрузки", true).icon, "folder-downloads");
        assert!(is_text("main.lua") && !is_text("Makefile") && !is_text("a.png"));
    }

    #[test]
    fn choices_round_trip() {
        let list = with_choice("", "txt", "/bin/notepad");
        assert_eq!(list, "txt:/bin/notepad");
        let list = with_choice(&list, "html", "/opt/debian/bin/bash /opt/chromium/bin/chromium");
        assert_eq!(chosen(&list, "html"), Some("/opt/debian/bin/bash /opt/chromium/bin/chromium"));
        let list = with_choice(&list, "txt", "/opt/npp/npp");
        assert_eq!(chosen(&list, "txt"), Some("/opt/npp/npp"));
        assert_eq!(list.matches("txt:").count(), 1);
        assert_eq!(chosen(&list, "md"), None);
    }
}
