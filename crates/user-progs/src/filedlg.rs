//! Окно «Открыть» / «Сохранить как» для программ (20.11): системная сторона.
//!
//! Само окно — `mini_ui::filedlg` (рисует и разбирает ввод, системных вызовов
//! не знает). Здесь [`SysHost`] — всё, что окну нужно от системы: папки, тома,
//! создание папки, значки макета, дата по часовому поясу, память о последней
//! папке (`~/.config/filedlg.cfg`). Как подключить окно к программе — в
//! заголовке `mini_ui::filedlg`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;

pub use mini_ui::filedlg::{COMPUTER, FileDialog, Filter, Host, Item, Mode, Options, Outcome, Volume, join, parent_of};
use mini_ui::filedlg as dlg;
use mini_ui::image::Image;
use mini_ui::{Rect, Surface};
use user_abi::{Dirent, KIND_DIRECTORY, Stat};

use crate::{close, monotonic_ms, mounts, open, open_write, println, read, readdir_raw, stat, write};

// Коды клавиш окна повторены в `mini_ui::filedlg` (тот крейт не знает
// договора); расходиться им нельзя.
const _: () = {
    assert!(dlg::KEY_LEFT == user_abi::WIN_KEY_LEFT);
    assert!(dlg::KEY_RIGHT == user_abi::WIN_KEY_RIGHT);
    assert!(dlg::KEY_UP == user_abi::WIN_KEY_UP);
    assert!(dlg::KEY_DOWN == user_abi::WIN_KEY_DOWN);
    assert!(dlg::KEY_HOME == user_abi::WIN_KEY_HOME);
    assert!(dlg::KEY_END == user_abi::WIN_KEY_END);
    assert!(dlg::KEY_PAGE_UP == user_abi::WIN_KEY_PAGE_UP);
    assert!(dlg::KEY_PAGE_DOWN == user_abi::WIN_KEY_PAGE_DOWN);
    assert!(dlg::KEY_DELETE == user_abi::WIN_KEY_DELETE);
    assert!(dlg::KEY_F1 == user_abi::WIN_KEY_F1);
    assert!(dlg::MOD_SHIFT == user_abi::WIN_MOD_SHIFT);
    assert!(dlg::MOD_CTRL == user_abi::WIN_MOD_CTRL);
    assert!(dlg::MOD_ALT == user_abi::WIN_MOD_ALT);
};

/// Значки макета (FIMG 64), которые окно показывает: папки, документы, тома.
/// Те же файлы, что у «Файлов» и стола (`assets/icons/file`).
static ICONS: [(&str, &[u8]); 17] = [
    ("folder", include_bytes!("../../../assets/icons/file/folder.fimg").as_slice()),
    ("folder-desktop", include_bytes!("../../../assets/icons/file/folder-desktop.fimg").as_slice()),
    ("folder-documents", include_bytes!("../../../assets/icons/file/folder-documents.fimg").as_slice()),
    ("folder-downloads", include_bytes!("../../../assets/icons/file/folder-downloads.fimg").as_slice()),
    ("folder-music", include_bytes!("../../../assets/icons/file/folder-music.fimg").as_slice()),
    ("folder-pictures", include_bytes!("../../../assets/icons/file/folder-pictures.fimg").as_slice()),
    ("folder-videos", include_bytes!("../../../assets/icons/file/folder-videos.fimg").as_slice()),
    ("doc-text", include_bytes!("../../../assets/icons/file/doc-text.fimg").as_slice()),
    ("doc-code", include_bytes!("../../../assets/icons/file/doc-code.fimg").as_slice()),
    ("doc-image", include_bytes!("../../../assets/icons/file/doc-image.fimg").as_slice()),
    ("doc-archive", include_bytes!("../../../assets/icons/file/doc-archive.fimg").as_slice()),
    ("doc-unknown", include_bytes!("../../../assets/icons/file/doc-unknown.fimg").as_slice()),
    ("place-this-pc", include_bytes!("../../../assets/icons/file/place-this-pc.fimg").as_slice()),
    ("place-system-disk", include_bytes!("../../../assets/icons/file/place-system-disk.fimg").as_slice()),
    ("place-local-disk", include_bytes!("../../../assets/icons/file/place-local-disk.fimg").as_slice()),
    ("place-usb-stick", include_bytes!("../../../assets/icons/file/place-usb-stick.fimg").as_slice()),
    ("place-external-drive", include_bytes!("../../../assets/icons/file/place-external-drive.fimg").as_slice()),
];

/// Самое большее записей одной папки.
const MAX_ENTRIES: usize = 4096;
/// Буфер ответа `mounts`.
const MOUNTS_LIMIT: usize = 2048;
/// Где помнится последняя папка (в доме).
const STATE_FILE: &str = ".config/filedlg.cfg";

/// Система для окна выбора файла.
pub struct SysHost {
    /// Как программа подписывает строки журнала: `notepad`.
    tag: &'static str,
    home: String,
    /// Смещение местного времени, минуты.
    offset: i32,
    /// Разобранные значки по имени (разбираются при первом показе).
    icons: RefCell<Vec<(String, Option<Image>)>>,
}

impl SysHost {
    /// `tag` — имя программы в журнале.
    #[must_use]
    pub fn new(tag: &'static str) -> Self {
        Self { tag, home: home_dir(), offset: local_offset(), icons: RefCell::new(Vec::new()) }
    }
}

impl Host for SysHost {
    fn list(&self, dir: &str) -> Result<Vec<Item>, String> {
        let fd = open(dir);
        if fd < 0 {
            return Err(error_text(fd));
        }
        let mut out = Vec::new();
        let mut entry = Dirent::default();
        while readdir_raw(fd, &mut entry) > 0 {
            let len = (entry.name_len as usize).min(entry.name.len());
            let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            out.push(Item { name: name.to_string(), dir: entry.kind == KIND_DIRECTORY, size: entry.size, mtime: entry.mtime });
            if out.len() >= MAX_ENTRIES {
                break;
            }
        }
        close(fd);
        Ok(out)
    }

    fn stat(&self, path: &str) -> Option<bool> {
        let mut info = Stat::default();
        (stat(path, &mut info) >= 0).then_some(info.kind == KIND_DIRECTORY)
    }

    fn make_dir(&self, path: &str) -> Result<(), String> {
        let code = crate::mkdir(path, 0o755);
        if code < 0 { Err(error_text(code)) } else { Ok(()) }
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let code = crate::rename(from, to);
        if code < 0 { Err(error_text(code)) } else { Ok(()) }
    }

    fn home(&self) -> String {
        self.home.clone()
    }

    fn now_ms(&self) -> u64 {
        monotonic_ms()
    }

    fn volumes(&self) -> Vec<Volume> {
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(MOUNTS_LIMIT).is_err() {
            return Vec::new();
        }
        buffer.resize(MOUNTS_LIMIT, 0u8);
        let got = mounts(&mut buffer);
        let text = if got > 0 { core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("") } else { "" };
        let mut out: Vec<Volume> = Vec::new();
        for line in text.lines() {
            // `<точка>\t<ФС>[\t<всего>\t<свободно>]`.
            let mut fields = line.split('\t');
            let (Some(point), Some(fs)) = (fields.next(), fields.next()) else {
                continue;
            };
            let total = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            let free = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            // Служебные файловые системы — не диски.
            if matches!(fs, "proc" | "sysfs" | "devfs" | "devtmpfs" | "tmpfs" | "devpts" | "pipefs")
                || ["/proc", "/sys", "/dev", "/run", "/tmp"].iter().any(|skip| point == *skip || point.starts_with(&format!("{skip}/")))
                || out.iter().any(|known| known.path == point)
            {
                continue;
            }
            let removable = point.starts_with("/usb") || point.starts_with("/media");
            let title = match point {
                "/" => String::from("Системный диск"),
                "/data" => String::from("Данные (/data)"),
                _ if removable => format!("USB-накопитель ({point})"),
                _ => format!("Диск ({point})"),
            };
            let icon = if point == "/" {
                "place-system-disk"
            } else if removable {
                "place-usb-stick"
            } else {
                "place-local-disk"
            };
            out.push(Volume { path: point.to_string(), title, icon, total, free });
        }
        if !out.iter().any(|volume| volume.path == "/") {
            out.insert(0, Volume { path: String::from("/"), title: String::from("Системный диск"), icon: "place-system-disk", total: 0, free: 0 });
        }
        // Корень — первым, остальные по алфавиту.
        out.sort_by(|a, b| (a.path != "/").cmp(&(b.path != "/")).then_with(|| a.path.cmp(&b.path)));
        out
    }

    fn kind(&self, name: &str, dir: bool) -> (&'static str, &'static str) {
        let kind = sysconf::filetypes::kind_of(name, dir);
        (kind.icon, kind.name)
    }

    fn date(&self, mtime: u32) -> String {
        if mtime == 0 {
            return String::new();
        }
        let local = calendar::DateTime::from_unix(i64::from(mtime) + i64::from(self.offset) * 60);
        format!("{:02}.{:02}.{} {:02}:{:02}", local.day, local.month, local.year, local.hour, local.minute)
    }

    fn draw_icon(&self, s: &mut Surface, rect: Rect, name: &str) -> bool {
        let mut cache = self.icons.borrow_mut();
        let at = match cache.iter().position(|(known, _)| known == name) {
            Some(at) => at,
            None => {
                // Нет в наборе окна — та же папка или «неизвестный документ».
                let fallback = if name.starts_with("folder") { "folder" } else { "doc-unknown" };
                let bytes = ICONS.iter().find(|(known, _)| *known == name).or_else(|| ICONS.iter().find(|(known, _)| *known == fallback)).map(|(_, bytes)| *bytes);
                cache.push((name.to_string(), bytes.and_then(Image::parse)));
                cache.len() - 1
            }
        };
        match &cache[at].1 {
            Some(image) => {
                image.draw(s, rect);
                true
            }
            None => false,
        }
    }

    fn last_dir(&self) -> Option<String> {
        let text = read_small(&dlg::join(&self.home, STATE_FILE))?;
        text.lines().find_map(|line| line.strip_prefix("last_dir=")).map(|dir| dir.trim().to_string()).filter(|dir| !dir.is_empty())
    }

    fn remember_dir(&self, dir: &str) {
        let config = dlg::join(&self.home, ".config");
        if self.stat(&config).is_none() {
            let _ = crate::mkdir(&config, 0o755);
        }
        let path = dlg::join(&self.home, STATE_FILE);
        let fd = open_write(&path, true, true);
        if fd >= 0 {
            let line = format!("last_dir={dir}\n");
            let _ = write(fd, line.as_bytes());
            close(fd);
        }
    }

    fn log(&self, line: &str) {
        println(&format!("{}: {line}", self.tag));
    }
}

/// Домашняя папка: `/root` у нулевого uid, иначе своя в `/home`.
#[must_use]
pub fn home_dir() -> String {
    let me = crate::uid();
    if me == 0 {
        return String::from("/root");
    }
    let fd = open("/home");
    if fd >= 0 {
        let mut entry = Dirent::default();
        while readdir_raw(fd, &mut entry) > 0 {
            let len = (entry.name_len as usize).min(entry.name.len());
            let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
                continue;
            };
            if name.starts_with('.') || entry.kind != KIND_DIRECTORY {
                continue;
            }
            let path = format!("/home/{name}");
            let mut info = Stat::default();
            if stat(&path, &mut info) >= 0 && info.uid == me {
                close(fd);
                return path;
            }
        }
        close(fd);
    }
    String::from("/home")
}

/// Смещение местного времени, минуты (из `/etc/system.cfg`, как у ядра).
fn local_offset() -> i32 {
    let Some(text) = read_small("/etc/system.cfg") else {
        return 0;
    };
    sysconf::value(&text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

/// Прочитать небольшой текстовый файл (до 4 КБ).
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
    core::str::from_utf8(&buffer[..got as usize]).ok().map(ToString::to_string)
}

fn error_text(code: i64) -> String {
    match code {
        user_abi::ERR_NOT_FOUND => String::from("нет такого файла или папки"),
        user_abi::ERR_PERMISSION => String::from("нет прав"),
        user_abi::ERR_EXISTS => String::from("такое имя уже есть"),
        user_abi::ERR_NO_SPACE => String::from("на диске нет места"),
        user_abi::ERR_UNSUPPORTED => String::from("этот том не умеет записи"),
        _ => format!("код {code}"),
    }
}
