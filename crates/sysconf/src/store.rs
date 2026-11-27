//! Settings store («реестр», 24.11.2026): the text of one hive and the names
//! of the well-known keys.
//!
//! Roman: «что-то вроде реестра, но быстрее и проще». The store itself lives
//! in the kernel (`config::registry`): it keeps the parsed hives in memory and
//! writes them atomically; programs reach it through `SYS_REGISTRY`
//! (`user_progs::reg_*`), the kernel shell through `reg`. This module is the
//! part both sides share and the host can test: the file format and the keys.
//!
//! # Keys
//!
//! A key is `hive/path`: the first segment names the hive (one file), the rest
//! is free text without `=` and control characters — `filetypes/.txt/default`,
//! `explorer/show_hidden`. Two scopes hold the same keys:
//!
//! * **system** — `/etc/registry/<hive>.reg` (image defaults may ship in
//!   `/usr/share/defaults/etc/registry/`), written by root: packages register
//!   their programs here;
//! * **user** — `~/.config/registry/<hive>.reg`: what the person chose.
//!
//! Reading a key looks at the user scope, then the system scope, then the
//! built-in defaults of the kernel ([`BUILTIN`]).
//!
//! # File format
//!
//! One `key=value` per line (the key without its hive), sorted, `#` starts a
//! comment line. Values are one line: `\` is written `\\`, a line break `\n`.
//!
//! # File types (`filetypes` hive) — the API for ports (7-Zip, VLC, …)
//!
//! * `filetypes/.<ext>/default` = command line (the file path is appended) —
//!   what a double click opens. In the user scope it is the person's «always
//!   open with» choice; in the system scope — the default a package set.
//! * `filetypes/.<ext>/openwith/<id>` = command line — a program that opens
//!   this type: it is offered first in «Открыть с помощью» (a package manifest
//!   line `opens=ext,ext` does the same for packages).
//! * `apps/<id>/caption`, `apps/<id>/icon` — how such a program is shown
//!   (icon: a `.fimg` path or an icon name of the manifest kind).
//!
//! A port registers itself at install time, e.g. 7-Zip:
//! `reg set system filetypes/.7z/openwith/7zip /opt/7zip/bin/7zfm`,
//! `reg set system filetypes/.7z/default /opt/7zip/bin/7zfm`,
//! `reg set system apps/7zip/caption 7-Zip` (or `user_progs::reg_set` from a
//! program running as root). A missing program is skipped, so a removed
//! package leaves no dead line in the menu.
//!
//! # Folder options (`explorer` hive)
//!
//! See [`FolderOptions`]: «Параметры папок» of «Файлы», read by the desktop
//! as well.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// Longest hive name.
pub const MAX_HIVE: usize = 32;
/// Longest key (without the hive) and value.
pub const MAX_KEY: usize = 256;
pub const MAX_VALUE: usize = 4096;

/// Is `name` a hive name: `a-z`, `0-9`, `_`, `-`.
#[must_use]
pub fn valid_hive(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_HIVE && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Split `hive/rest` and check both parts. `None` — not a key.
#[must_use]
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    let (hive, rest) = key.split_once('/')?;
    (valid_hive(hive) && valid_rest(rest)).then_some((hive, rest))
}

/// The part of a key after its hive: not empty, no `=`, no control characters.
#[must_use]
pub fn valid_rest(rest: &str) -> bool {
    !rest.is_empty() && rest.len() <= MAX_KEY && !rest.contains('=') && !rest.chars().any(char::is_control) && rest.trim() == rest
}

/// `\` → `\\`, line break → `\n`, carriage return → `\r`.
#[must_use]
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

/// Back from [`escape`]; an unknown escape keeps both characters.
#[must_use]
pub fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Parse the text of a hive file. A line that is not `key=value` is skipped:
/// the file may be edited by hand, and one typo must not lose the rest.
#[must_use]
pub fn parse(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        if !valid_rest(key) {
            continue;
        }
        out.entry(String::from(key)).or_insert_with(|| unescape(value));
    }
    out
}

/// Text of a hive file: a comment line, then the keys in order.
#[must_use]
pub fn format(hive: &str, map: &BTreeMap<String, String>) -> String {
    let mut out = alloc::format!("# FossAi settings hive '{hive}': key=value per line (reg get/set/rm in the shell)\n");
    for (key, value) in map {
        out.push_str(key);
        out.push('=');
        out.push_str(&escape(value));
        out.push('\n');
    }
    out
}

/// Built-in defaults (the lowest layer, under the system scope): the OBSOS
/// browser opens web pages, pictures in SVG and Internet shortcuts (Roman,
/// 24.11). A default whose program is not installed is skipped by the reader.
pub const BUILTIN: &[(&str, &str)] = &[
    ("apps/obsos/caption", "OBSOS (BETA) — Браузер"),
    ("apps/obsos/icon", "/opt/obsos/icon.fimg"),
    ("filetypes/.html/default", OBSOS),
    ("filetypes/.html/openwith/obsos", OBSOS),
    ("filetypes/.htm/default", OBSOS),
    ("filetypes/.htm/openwith/obsos", OBSOS),
    ("filetypes/.xhtml/default", OBSOS),
    ("filetypes/.xhtml/openwith/obsos", OBSOS),
    ("filetypes/.svg/default", OBSOS),
    ("filetypes/.svg/openwith/obsos", OBSOS),
    ("filetypes/.url/default", OBSOS),
    ("filetypes/.url/openwith/obsos", OBSOS),
    // «Архиватор 7-Zip» (`/bin/archiver` над `/bin/7zz`, 24.11): архивы —
    // `sysconf::filetypes::ARCHIVES`; `.iso` остаётся «Образом диска».
    ("apps/archiver/caption", "Архиватор 7-Zip"),
    ("apps/archiver/icon", "doc-archive"),
    ("filetypes/.7z/default", ARCHIVER),
    ("filetypes/.7z/openwith/archiver", ARCHIVER),
    ("filetypes/.zip/default", ARCHIVER),
    ("filetypes/.zip/openwith/archiver", ARCHIVER),
    ("filetypes/.rar/default", ARCHIVER),
    ("filetypes/.rar/openwith/archiver", ARCHIVER),
    ("filetypes/.tar/default", ARCHIVER),
    ("filetypes/.tar/openwith/archiver", ARCHIVER),
    ("filetypes/.gz/default", ARCHIVER),
    ("filetypes/.gz/openwith/archiver", ARCHIVER),
    ("filetypes/.tgz/default", ARCHIVER),
    ("filetypes/.tgz/openwith/archiver", ARCHIVER),
    ("filetypes/.bz2/default", ARCHIVER),
    ("filetypes/.bz2/openwith/archiver", ARCHIVER),
    ("filetypes/.tbz2/default", ARCHIVER),
    ("filetypes/.tbz2/openwith/archiver", ARCHIVER),
    ("filetypes/.xz/default", ARCHIVER),
    ("filetypes/.xz/openwith/archiver", ARCHIVER),
    ("filetypes/.txz/default", ARCHIVER),
    ("filetypes/.txz/openwith/archiver", ARCHIVER),
    ("filetypes/.zst/default", ARCHIVER),
    ("filetypes/.zst/openwith/archiver", ARCHIVER),
    ("filetypes/.lzma/default", ARCHIVER),
    ("filetypes/.lzma/openwith/archiver", ARCHIVER),
    ("filetypes/.cab/default", ARCHIVER),
    ("filetypes/.cab/openwith/archiver", ARCHIVER),
    ("filetypes/.arj/default", ARCHIVER),
    ("filetypes/.arj/openwith/archiver", ARCHIVER),
    ("filetypes/.lzh/default", ARCHIVER),
    ("filetypes/.lzh/openwith/archiver", ARCHIVER),
    ("filetypes/.wim/default", ARCHIVER),
    ("filetypes/.wim/openwith/archiver", ARCHIVER),
    ("filetypes/.cpio/default", ARCHIVER),
    ("filetypes/.cpio/openwith/archiver", ARCHIVER),
    ("filetypes/.rpm/default", ARCHIVER),
    ("filetypes/.rpm/openwith/archiver", ARCHIVER),
    ("filetypes/.deb/default", ARCHIVER),
    ("filetypes/.deb/openwith/archiver", ARCHIVER),
    ("filetypes/.z/default", ARCHIVER),
    ("filetypes/.z/openwith/archiver", ARCHIVER),
    ("filetypes/.001/default", ARCHIVER),
    ("filetypes/.001/openwith/archiver", ARCHIVER),
    // Образ диска 7-Zip тоже открывает — только в «Открыть с помощью».
    ("filetypes/.iso/openwith/archiver", ARCHIVER),
    // «Фотографии» (`/bin/photos`, 24.11): pictures its decoders read.
    ("apps/photos/caption", "Фотографии"),
    ("apps/photos/icon", "doc-image"),
    ("filetypes/.jpg/default", PHOTOS),
    ("filetypes/.jpg/openwith/photos", PHOTOS),
    ("filetypes/.jpeg/default", PHOTOS),
    ("filetypes/.jpeg/openwith/photos", PHOTOS),
    ("filetypes/.jpe/default", PHOTOS),
    ("filetypes/.jpe/openwith/photos", PHOTOS),
    ("filetypes/.png/default", PHOTOS),
    ("filetypes/.png/openwith/photos", PHOTOS),
    ("filetypes/.bmp/default", PHOTOS),
    ("filetypes/.bmp/openwith/photos", PHOTOS),
    ("filetypes/.gif/default", PHOTOS),
    ("filetypes/.gif/openwith/photos", PHOTOS),
    ("filetypes/.qoi/default", PHOTOS),
    ("filetypes/.qoi/openwith/photos", PHOTOS),
    // WebP since 27.11 (`crates/webp`).
    ("filetypes/.webp/default", PHOTOS),
    ("filetypes/.webp/openwith/photos", PHOTOS),
    // «Видеоплеер VLC» (пакет `vlc`, 24.11): video and sound files; skipped
    // while the package is not installed (`command_program`).
    ("filetypes/.mp4/default", VLC),
    ("filetypes/.m4v/default", VLC),
    ("filetypes/.mkv/default", VLC),
    ("filetypes/.webm/default", VLC),
    ("filetypes/.avi/default", VLC),
    ("filetypes/.mov/default", VLC),
    ("filetypes/.wmv/default", VLC),
    ("filetypes/.flv/default", VLC),
    ("filetypes/.ts/default", VLC),
    ("filetypes/.m2ts/default", VLC),
    ("filetypes/.mpg/default", VLC),
    ("filetypes/.mpeg/default", VLC),
    ("filetypes/.ogv/default", VLC),
    ("filetypes/.3gp/default", VLC),
    ("filetypes/.mp3/default", VLC),
    ("filetypes/.flac/default", VLC),
    ("filetypes/.ogg/default", VLC),
    ("filetypes/.oga/default", VLC),
    ("filetypes/.opus/default", VLC),
    ("filetypes/.wav/default", VLC),
    ("filetypes/.m4a/default", VLC),
    ("filetypes/.aac/default", VLC),
    ("filetypes/.wma/default", VLC),
    ("filetypes/.mka/default", VLC),
    // Context-menu verbs (25.11, see [`verbs`]): «Расположение файла» on
    // files always; with Shift — «Копировать как путь» on files and
    // «Удалить содержимое папки» on folders.
    ("shell/file/location/caption", "Расположение файла"),
    ("shell/file/location/command", VERB_LOCATION),
    ("shell/file/location/icon", "folder"),
    ("shell/file/copyaspath/caption", "Копировать как путь"),
    ("shell/file/copyaspath/command", VERB_COPY_AS_PATH),
    ("shell/file/copyaspath/extended", "1"),
    ("shell/folder/emptyfolder/caption", "Удалить содержимое папки"),
    ("shell/folder/emptyfolder/command", VERB_EMPTY_FOLDER),
    ("shell/folder/emptyfolder/extended", "1"),
];

// ---------------------------------------------------------------------------
// Context-menu verbs (`shell` hive, 25.11)
// ---------------------------------------------------------------------------
//
// Roman 25.11: items of the right-click menus come from the store, so a
// package (or the person) adds one without code — like `HKCR\*\shell\<verb>`.
//
// * `shell/<type>/<verb>/caption` — the text of the item (required);
// * `shell/<type>/<verb>/command` — what it runs (required): a command line,
//   `%1` is replaced by the path (no `%1` — the path is appended), or one of
//   the built-in actions [`VERB_LOCATION`], [`VERB_COPY_AS_PATH`],
//   [`VERB_EMPTY_FOLDER`];
// * `shell/<type>/<verb>/icon` — an icon name (optional);
// * `shell/<type>/<verb>/extended` = `1` — shown only when Shift is held
//   while the menu opens (Windows «extended verbs»);
// * `shell/<type>/<verb>/hidden` = `1` — switched off (in the user scope it
//   hides a built-in verb).
//
// `<type>`: `file`, `folder`, `desktop` (the empty desktop), `shortcut`
// (a program shortcut), `*` (files and folders), or `.ext` (files of that
// extension). Used by the desktop (kernel) and «Файлы».

/// Built-in action: open «Файлы» at the containing folder with the item
/// selected («Расположение файла»).
pub const VERB_LOCATION: &str = "@location";
/// Built-in action: the full path, quoted, to the clipboard («Копировать как
/// путь»).
pub const VERB_COPY_AS_PATH: &str = "@copy-as-path";
/// Built-in action: ask, then move everything inside the folder to the
/// recycle bin («Удалить содержимое папки»).
pub const VERB_EMPTY_FOLDER: &str = "@empty-folder";

/// Most verbs a menu shows (the menu surfaces are sized for it).
pub const MAX_VERBS: usize = 6;

/// One context-menu item from the store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verb {
    pub id: String,
    pub caption: String,
    pub command: String,
    pub icon: Option<String>,
    pub extended: bool,
}

impl Verb {
    /// The command line for `path`: `%1` replaced, or the path appended.
    /// Built-in actions (`@…`) come back as they are.
    #[must_use]
    pub fn command_for(&self, path: &str) -> String {
        if self.command.starts_with('@') {
            return self.command.clone();
        }
        if self.command.contains("%1") {
            return self.command.replace("%1", path);
        }
        alloc::format!("{} {path}", self.command)
    }
}

/// The types of an item for [`verbs`], most specific first.
#[must_use]
pub fn verb_types(name: &str, directory: bool) -> Vec<String> {
    let mut out = Vec::new();
    if directory {
        out.push(String::from("folder"));
    } else {
        if let Some((_, ext)) = name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
            out.push(alloc::format!(".{}", ext.to_ascii_lowercase()));
        }
        out.push(String::from("file"));
    }
    out.push(String::from("*"));
    out
}

/// Verbs for an item of `types` ([`verb_types`], or `["desktop"]`,
/// `["shortcut"]`) from the merged keys under `shell/` (`reg_list("shell/")`,
/// `config::registry::list(None, "shell/")` — built-ins included). A verb id
/// met under a more specific type wins; extended verbs only with `shift`;
/// at most [`MAX_VERBS`], in key order.
#[must_use]
pub fn verbs(pairs: &[(String, String)], types: &[&str], shift: bool) -> Vec<Verb> {
    let mut out: Vec<Verb> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for kind in types {
        let head = alloc::format!("shell/{kind}/");
        // Verb ids of this type, in key order.
        let mut ids: Vec<&str> = Vec::new();
        for (key, _) in pairs {
            if let Some(rest) = key.strip_prefix(head.as_str()) {
                if let Some((id, _)) = rest.split_once('/') {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        for id in ids {
            if seen.iter().any(|known| known == id) {
                continue;
            }
            seen.push(String::from(id));
            let field = |name: &str| {
                let key = alloc::format!("{head}{id}/{name}");
                pairs.iter().find(|(known, _)| *known == key).map(|(_, value)| value.clone())
            };
            if field("hidden").is_some_and(|value| value.trim() == "1") {
                continue;
            }
            let (Some(caption), Some(command)) = (field("caption"), field("command")) else { continue };
            if caption.trim().is_empty() || command.trim().is_empty() {
                continue;
            }
            let extended = field("extended").is_some_and(|value| value.trim() == "1");
            if extended && !shift {
                continue;
            }
            out.push(Verb { id: String::from(id), caption, command, icon: field("icon").filter(|icon| !icon.is_empty()), extended });
            if out.len() == MAX_VERBS {
                break;
            }
        }
        if out.len() == MAX_VERBS {
            break;
        }
    }
    // The everyday verbs first, the extended ones (Shift) after them — the
    // menu without Shift is the same menu, only shorter.
    out.sort_by_key(|verb| verb.extended);
    out
}

/// The photo viewer of the system («Фотографии», 24.11).
pub const PHOTOS: &str = "/bin/photos";

/// The video player of the system (VLC from Debian 13, package `vlc`,
/// 24.11): a bash script, see [`command_program`].
pub const VLC: &str = "/opt/debian/bin/bash /opt/vlc/bin/fossai-vlc";

/// The file a command line runs: its first word, and for a script handed to
/// a shell (`/opt/debian/bin/bash /opt/vlc/bin/fossai-vlc`) the script — the
/// shell is there as long as Debian is, the script only with its package, so
/// a default of a removed package is skipped.
#[must_use]
pub fn command_program(command: &str) -> Option<&str> {
    let mut words = command.split_whitespace();
    let first = words.next()?;
    if first.ends_with("/bash") || first.ends_with("/sh") {
        if let Some(script) = words.next().filter(|word| word.starts_with('/')) {
            return Some(script);
        }
    }
    Some(first)
}

/// The browser of the system.
pub const OBSOS: &str = "/opt/obsos/bin/obsos";

/// The archiver of the system (7-Zip, 24.11).
pub const ARCHIVER: &str = "/bin/archiver";

/// Built-in value of `key`.
#[must_use]
pub fn builtin(key: &str) -> Option<&'static str> {
    BUILTIN.iter().find(|(known, _)| *known == key).map(|(_, value)| *value)
}

/// Key of the program chosen (or set by default) for extension `ext`
/// (lower case, without the dot).
#[must_use]
pub fn default_key(ext: &str) -> String {
    alloc::format!("filetypes/.{ext}/default")
}

/// Prefix of the programs registered for extension `ext`.
#[must_use]
pub fn openwith_prefix(ext: &str) -> String {
    alloc::format!("filetypes/.{ext}/openwith/")
}

/// The address an Internet shortcut (`.url`, `[InternetShortcut]` /
/// `URL=…`) points to.
#[must_use]
pub fn url_of_shortcut(text: &str) -> Option<String> {
    text.lines().map(str::trim).find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim().eq_ignore_ascii_case("url") && !value.trim().is_empty()).then(|| String::from(value.trim()))
    })
}

// ---------------------------------------------------------------------------
// Folder options
// ---------------------------------------------------------------------------

/// «Параметры папок» (24.11, like Windows → «Вид»): only what the system
/// really does. Kept in the `explorer` hive of the user scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FolderOptions {
    /// Show hidden files and folders (names starting with a dot).
    pub show_hidden: bool,
    /// Hide extensions of known file types.
    pub hide_known_ext: bool,
    /// Full path in the title of the window.
    pub full_path_title: bool,
    /// A double click on a folder opens it in a new window.
    pub new_window: bool,
    /// Status bar at the bottom of «Файлы».
    pub status_bar: bool,
    /// Tooltips with the type, size and date of an item under the pointer.
    pub info_tips: bool,
    /// The tooltip of a folder counts its size and what it holds.
    pub folder_size_tips: bool,
}

impl FolderOptions {
    /// What «Восстановить значения по умолчанию» returns to. Extensions stay
    /// visible: FossAi has always shown them, and a known type is easier to
    /// hide than an unexpected `.exe` to notice.
    pub const DEFAULT: Self = Self {
        show_hidden: false,
        hide_known_ext: false,
        full_path_title: false,
        new_window: false,
        status_bar: true,
        info_tips: true,
        folder_size_tips: true,
    };

    /// Keys of the options in the order of the dialog, with their captions.
    pub const ROWS: [(&'static str, &'static str); 7] = [
        ("explorer/show_hidden", "Показывать скрытые файлы и папки"),
        ("explorer/hide_known_ext", "Скрывать расширения для известных типов файлов"),
        ("explorer/full_path_title", "Выводить полный путь в заголовке окна"),
        ("explorer/open_in_new_window", "Открывать каждую папку в отдельном окне"),
        ("explorer/status_bar", "Показывать строку состояния"),
        ("explorer/info_tips", "Отображать описание для папок и элементов во всплывающих подсказках"),
        ("explorer/folder_size_tips", "Отображать сведения о размере папок во всплывающих подсказках"),
    ];

    /// Option number `index` of [`Self::ROWS`].
    #[must_use]
    pub const fn get(&self, index: usize) -> bool {
        match index {
            0 => self.show_hidden,
            1 => self.hide_known_ext,
            2 => self.full_path_title,
            3 => self.new_window,
            4 => self.status_bar,
            5 => self.info_tips,
            _ => self.folder_size_tips,
        }
    }

    pub fn set(&mut self, index: usize, on: bool) {
        match index {
            0 => self.show_hidden = on,
            1 => self.hide_known_ext = on,
            2 => self.full_path_title = on,
            3 => self.new_window = on,
            4 => self.status_bar = on,
            5 => self.info_tips = on,
            _ => self.folder_size_tips = on,
        }
    }

    /// Read through `get` (key → value): `1`/`0`; anything else — default.
    #[must_use]
    pub fn read(get: impl Fn(&str) -> Option<String>) -> Self {
        let mut out = Self::DEFAULT;
        for (index, (key, _)) in Self::ROWS.iter().enumerate() {
            match get(key).as_deref().map(str::trim) {
                Some("1" | "on" | "yes" | "true") => out.set(index, true),
                Some("0" | "off" | "no" | "false") => out.set(index, false),
                _ => {}
            }
        }
        out
    }

    /// Pairs to write: every option, `1` or `0`.
    #[must_use]
    pub fn pairs(&self) -> Vec<(&'static str, &'static str)> {
        Self::ROWS.iter().enumerate().map(|(index, (key, _))| (*key, if self.get(index) { "1" } else { "0" })).collect()
    }
}

/// Folders pinned to the quick access column of «Файлы»: paths split by `|`.
pub const PINNED_KEY: &str = "explorer/pinned";

/// The pinned list from its value.
#[must_use]
pub fn pinned_list(value: &str) -> Vec<String> {
    value.split('|').map(str::trim).filter(|path| path.starts_with('/')).map(String::from).collect()
}

/// The value of a pinned list.
#[must_use]
pub fn pinned_value(list: &[String]) -> String {
    let mut out = String::new();
    for (index, path) in list.iter().enumerate() {
        if index > 0 {
            out.push('|');
        }
        out.push_str(path);
    }
    out
}

/// The name shown for `name` when extensions of known types are hidden:
/// `Отчёт.txt` → `Отчёт`, `архив.tar` → `архив`; unknown types, folders and
/// names that start with a dot keep the whole name.
#[must_use]
pub fn shown_name(name: &str, directory: bool, hide_known_ext: bool) -> &str {
    if !hide_known_ext || directory {
        return name;
    }
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() && crate::filetypes::kind_of(name, false).icon != "doc-unknown" => stem,
        _ => name,
    }
}

/// Characters of the name without its extension — what a rename selects
/// (Windows: F2 and a new «Новый текстовый документ.txt» select «Новый
/// текстовый документ»). Folders and names without a stem — the whole name.
#[must_use]
pub fn stem_chars(name: &str, directory: bool) -> usize {
    if directory {
        return name.chars().count();
    }
    match name.rfind('.') {
        Some(at) if at > 0 => name[..at].chars().count(),
        _ => name.chars().count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_verbs() {
        let mut pairs: Vec<(String, String)> = BUILTIN.iter().filter(|(key, _)| key.starts_with("shell/")).map(|(k, v)| (String::from(*k), String::from(*v))).collect();
        pairs.push((String::from("shell/.txt/edit/caption"), String::from("Изменить")));
        pairs.push((String::from("shell/.txt/edit/command"), String::from("/bin/notepad %1")));
        let types = verb_types("a.TXT", false);
        assert_eq!(types, ["\u{2e}txt", "file", "*"]);
        let refs: Vec<&str> = types.iter().map(String::as_str).collect();
        let plain = verbs(&pairs, &refs, false);
        assert_eq!(plain.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["edit", "location"]);
        assert_eq!(plain[0].command_for("/x/a.txt"), "/bin/notepad /x/a.txt");
        let shifted = verbs(&pairs, &refs, true);
        assert_eq!(shifted.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["edit", "location", "copyaspath"]);
        let folder = verbs(&pairs, &["folder", "*"], false);
        assert!(folder.is_empty());
        assert_eq!(verbs(&pairs, &["folder", "*"], true)[0].command, VERB_EMPTY_FOLDER);
        pairs.push((String::from("shell/file/location/hidden"), String::from("1")));
        assert!(verbs(&pairs, &["file"], false).is_empty());
    }

    #[test]
    fn hive_round_trip() {
        let mut map = BTreeMap::new();
        map.insert(String::from(".txt/default"), String::from("/bin/notepad"));
        map.insert(String::from("note"), String::from("a\\b\nc"));
        let text = format("filetypes", &map);
        assert!(text.starts_with('#'));
        assert_eq!(parse(&text), map);
        assert_eq!(parse("bad line\n=x\nk=v\nk=w\n"), BTreeMap::from([(String::from("k"), String::from("v"))]));
    }

    #[test]
    fn keys() {
        assert_eq!(split_key("filetypes/.txt/default"), Some(("filetypes", ".txt/default")));
        assert_eq!(split_key("Bad/x"), None);
        assert_eq!(split_key("explorer/"), None);
        assert_eq!(split_key("explorer/a=b"), None);
        assert_eq!(split_key("nohive"), None);
        assert_eq!(builtin("filetypes/.html/default"), Some(OBSOS));
        assert_eq!(builtin("filetypes/.7z/default"), Some(ARCHIVER));
        assert_eq!(builtin("filetypes/.iso/default"), None);
        for ext in crate::filetypes::ARCHIVES {
            assert_eq!(builtin(&default_key(ext)), Some(ARCHIVER), "{ext}");
            assert_eq!(builtin(&alloc::format!("{}archiver", openwith_prefix(ext))), Some(ARCHIVER), "{ext}");
        }
        assert_eq!(default_key("svg"), "filetypes/.svg/default");
    }

    #[test]
    fn folder_options() {
        let options = FolderOptions::read(|key| (key == "explorer/show_hidden").then(|| String::from("1")));
        assert!(options.show_hidden && options.status_bar && !options.hide_known_ext);
        let back = FolderOptions::read(|key| options.pairs().iter().find(|(k, _)| *k == key).map(|(_, v)| String::from(*v)));
        assert_eq!(back, options);
        assert_eq!(shown_name("Отчёт.txt", false, true), "Отчёт");
        assert_eq!(shown_name("data.xyz", false, true), "data.xyz");
        assert_eq!(shown_name("Отчёт.txt", false, false), "Отчёт.txt");
        assert_eq!(shown_name(".bashrc", false, true), ".bashrc");
        assert_eq!(stem_chars("Новый текстовый документ.txt", false), 24);
        assert_eq!(stem_chars("Новая папка", true), 11);
        assert_eq!(stem_chars(".bashrc", false), 7);
        assert_eq!(url_of_shortcut("[InternetShortcut]\r\nURL=https://example.org/\r\n").as_deref(), Some("https://example.org/"));
        assert_eq!(pinned_list(&pinned_value(&[String::from("/a"), String::from("/b c")])), [String::from("/a"), String::from("/b c")]);
    }
}
