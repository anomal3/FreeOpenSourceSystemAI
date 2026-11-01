//! Корзина (02.11, макет FossAi): «Удалить» на столе переносит, а не стирает.
//!
//! Раскладка — как у freedesktop (её же знают Linux-программы и их файловые
//! диалоги): `~/.local/share/Trash/files/<имя>` — само удалённое, рядом
//! `info/<имя>.trashinfo` — откуда оно и когда:
//!
//! ```text
//! [Trash Info]
//! Path=/home/roman/Desktop/%D0%9E%D1%82%D1%87%D1%91%D1%82
//! DeletionDate=2026-11-02T14:05:31
//! ```
//!
//! Перенос — `rename`, то есть мгновенный и целиком, с каталогом вместе со
//! всем содержимым. С другого тома перенести нельзя (`rename` между томами не
//! бывает) — тогда отказ словами, а не тихое стирание. Восстанавливают
//! «Файлы» (`bin/files.rs`): там корзина открывается окном.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::vfs::NodeKind;
use crate::vfs::perm::{Access, Credentials};

/// Подпись значка и окна.
pub const LABEL: &str = "Корзина";

/// Каталог корзины того, кто вошёл.
#[must_use]
pub fn dir() -> String {
    alloc::format!("{}/.local/share/Trash", super::context::home_dir())
}

/// Где лежит удалённое.
#[must_use]
pub fn files_dir() -> String {
    alloc::format!("{}/files", dir())
}

fn info_dir() -> String {
    alloc::format!("{}/info", dir())
}

fn cred() -> Credentials {
    crate::user::session::credentials()
}

fn exists(path: &str) -> bool {
    crate::fs::resolve_as(cred(), path, Access::NONE).is_some_and(|result| result.is_ok())
}

/// Имена в корзине (без «.» и «..»).
#[must_use]
pub fn names() -> Vec<String> {
    match crate::fs::list(&files_dir()) {
        Some(Ok(entries)) => entries.into_iter().map(|entry| entry.name).filter(|name| name != "." && name != "..").collect(),
        _ => Vec::new(),
    }
}

/// Пуста ли корзина — от этого зависит значок на столе.
#[must_use]
pub fn is_empty() -> bool {
    names().is_empty()
}

/// Перенести `path` в корзину. Ответ — имя в корзине.
pub fn trash(path: &str) -> Result<String, String> {
    let files = files_dir();
    let info = info_dir();
    if path == dir() || path.starts_with(&alloc::format!("{}/", dir())) {
        return Err("it is already in the trash".to_string());
    }
    super::context::ensure_dir(&files)?;
    super::context::ensure_dir(&info)?;
    let base = super::context::base_name(path);
    // Свободное имя и в `files`, и в `info`: два «Отчёт.txt» с разных мест
    // лежат рядом как «Отчёт.txt» и «Отчёт (2).txt».
    let (stem, extension) = match base.rfind('.') {
        Some(at) if at > 0 => (&base[..at], &base[at..]),
        _ => (base, ""),
    };
    let mut name = None;
    for attempt in 1..=1000u32 {
        let candidate = if attempt == 1 { base.to_string() } else { alloc::format!("{stem} ({attempt}){extension}") };
        if !exists(&alloc::format!("{files}/{candidate}")) && !exists(&alloc::format!("{info}/{candidate}.trashinfo")) {
            name = Some(candidate);
            break;
        }
    }
    let name = name.ok_or_else(|| "too many entries with that name in the trash".to_string())?;
    // Сначала запись о том, откуда: перенос без неё — файл, который нельзя
    // вернуть на место.
    let info_path = alloc::format!("{info}/{name}.trashinfo");
    write_file(&info_path, info_text(path).as_bytes())?;
    match crate::fs::rename_as(cred(), path, &alloc::format!("{files}/{name}")) {
        Some(Ok(())) => Ok(name),
        failed => {
            let _ = crate::fs::remove_as(cred(), &info_path);
            Err(match failed {
                Some(Err(crate::vfs::VfsError::Unsupported)) => "the trash is on another volume".to_string(),
                Some(Err(err)) => alloc::format!("{err}"),
                _ => "no filesystem is mounted".to_string(),
            })
        }
    }
}

/// Очистить корзину: стереть всё безвозвратно. Ответ — сколько стёрто.
pub fn empty() -> Result<usize, String> {
    let (files, info) = (files_dir(), info_dir());
    let mut count = 0;
    let mut last_error = None;
    for name in names() {
        match remove_tree(&alloc::format!("{files}/{name}"), 0) {
            Ok(()) => {
                count += 1;
                let _ = crate::fs::remove_as(cred(), &alloc::format!("{info}/{name}.trashinfo"));
            }
            Err(err) => last_error = Some(err),
        }
    }
    // Записи без файла — остатки прерванного переноса.
    if let Some(Ok(entries)) = crate::fs::list(&info) {
        for entry in entries {
            if let Some(name) = entry.name.strip_suffix(".trashinfo") {
                if !exists(&alloc::format!("{files}/{name}")) {
                    let _ = crate::fs::remove_as(cred(), &alloc::format!("{info}/{}", entry.name));
                }
            }
        }
    }
    match last_error {
        Some(err) if count == 0 => Err(err),
        _ => Ok(count),
    }
}

/// Сколько объектов в корзине и сколько они занимают (байт).
#[must_use]
pub fn usage() -> (usize, u64) {
    let files = files_dir();
    let names = names();
    let bytes = names.iter().map(|name| tree_size(&alloc::format!("{files}/{name}"), 0)).sum();
    (names.len(), bytes)
}

/// Стереть файл или каталог со всем содержимым. Только для корзины: на
/// столе рекурсивного удаления нет намеренно: удалённое уходит сюда целиком.
fn remove_tree(path: &str, depth: u32) -> Result<(), String> {
    if depth > 64 {
        return Err("folders are nested too deep".to_string());
    }
    if let Some(Ok(entries)) = crate::fs::list(path) {
        for entry in entries.into_iter().filter(|entry| entry.name != "." && entry.name != "..") {
            let child = alloc::format!("{path}/{}", entry.name);
            if entry.kind == NodeKind::Directory {
                remove_tree(&child, depth + 1)?;
            } else {
                remove(&child)?;
            }
        }
    }
    remove(path)
}

fn remove(path: &str) -> Result<(), String> {
    match crate::fs::remove_as(cred(), path) {
        Some(Ok(())) => Ok(()),
        Some(Err(err)) => Err(alloc::format!("{err}")),
        None => Err("no filesystem is mounted".to_string()),
    }
}

fn tree_size(path: &str, depth: u32) -> u64 {
    let Some(Ok(entries)) = crate::fs::list(path) else {
        // Не каталог — размер файла из записи родителя.
        let parent = match path.rfind('/') {
            Some(at) => &path[..at],
            None => return 0,
        };
        let name = super::context::base_name(path);
        return match crate::fs::list(parent) {
            Some(Ok(entries)) => entries.iter().find(|entry| entry.name == name).map_or(0, |entry| entry.size),
            _ => 0,
        };
    };
    if depth > 64 {
        return 0;
    }
    entries
        .into_iter()
        .filter(|entry| entry.name != "." && entry.name != "..")
        .map(|entry| {
            if entry.kind == NodeKind::Directory {
                tree_size(&alloc::format!("{path}/{}", entry.name), depth + 1)
            } else {
                entry.size
            }
        })
        .sum()
}

fn write_file(path: &str, data: &[u8]) -> Result<(), String> {
    let node = match crate::fs::create_as(cred(), path, 0o600) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return Err(alloc::format!("{err}")),
        None => return Err("no filesystem is mounted".to_string()),
    };
    node.write_at(0, data).map(|_| ()).map_err(|err| alloc::format!("{err}"))
}

/// Текст `.trashinfo`: путь — в процентной записи, как требует freedesktop;
/// дата — местное время без пояса.
fn info_text(path: &str) -> String {
    let mut text = String::from("[Trash Info]\nPath=");
    text.push_str(&encode(path));
    text.push('\n');
    if let Some(now) = crate::time::now_local() {
        text.push_str(&alloc::format!(
            "DeletionDate={:04}-{:02}-{:02}T{:02}:{:02}:{:02}\n",
            now.year,
            now.month,
            now.day,
            now.hour,
            now.minute,
            now.second
        ));
    }
    text
}

/// Процентная запись пути: буквы, цифры, `/` и `-._~` — как есть.
fn encode(path: &str) -> String {
    let mut out = String::new();
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&alloc::format!("%{byte:02X}"));
        }
    }
    out
}
