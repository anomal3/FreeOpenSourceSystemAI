//! Символические ссылки программ Linux (фаза 71i) — в памяти, не на диске.
//!
//! Файловые системы системы ссылок не знают. Программам Linux они нужны не
//! как способ хранить файлы, а как атомарная метка: Chromium держит профиль
//! ссылкой `SingletonLock` → «имя машины-pid» (её `symlink` либо удаётся
//! целиком, либо `EEXIST`) и рядом `SingletonSocket`, `SingletonCookie`. Без
//! неё он отказывается запускаться («Failed to create a ProcessSingleton»).
//!
//! Поэтому ссылка — запись в таблице по абсолютному пути Linux: `symlink`
//! кладёт, `readlink` и `lstat` читают, `unlink` и `rename` убирают и
//! переносят, `stat` и `open` идут по ней. До перезагрузки: метке профиля
//! это и нужно — после перезагрузки прежний браузер заведомо не жив.

use alloc::collections::BTreeMap;
use alloc::string::String;

use super::{e, linux_path, normalize, user_string};
use crate::sync::Mutex;

static LINKS: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// Таблица ссылок переживает перезагрузку (13.11): префикс Wine держится на
/// ссылках `dosdevices/c:` → `../drive_c` и `z:` → `/`, и без них после
/// перезагрузки Wine не находил ни `C:`, ни `kernel32.dll`. Таблица целиком
/// лежит в файле корня (пишет ядро, от root) и перезаписывается на каждой
/// правке — правки редки (`symlink`, `unlink`, `rename` ссылок).
const STORE: &str = "/etc/linux-links";

static LOADED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Прочитать таблицу с диска при первом обращении.
fn load() {
    if LOADED.swap(true, core::sync::atomic::Ordering::AcqRel) {
        return;
    }
    let Some(Ok((data, _))) = crate::fs::read(STORE, 4 << 20) else { return };
    let Ok(text) = core::str::from_utf8(&data) else { return };
    let mut links = LINKS.lock();
    for line in text.lines() {
        if let Some((path, target)) = line.split_once('\t') {
            if !path.is_empty() && !target.is_empty() {
                links.entry(String::from(path)).or_insert_with(|| String::from(target));
            }
        }
    }
}

/// Записать таблицу на диск.
fn save() {
    let text = {
        let links = LINKS.lock();
        let mut text = String::new();
        for (path, target) in links.iter() {
            if path.contains('\n') || path.contains('\t') || target.contains('\n') {
                continue;
            }
            text.push_str(path);
            text.push('\t');
            text.push_str(target);
            text.push('\n');
        }
        text
    };
    let root = crate::vfs::perm::Credentials::ROOT;
    let node = match crate::fs::resolve_as(root, STORE, crate::vfs::perm::Access::WRITE) {
        Some(Ok(node)) => node,
        _ => match crate::fs::create_as(root, STORE, 0o600) {
            Some(Ok(node)) => node,
            _ => return,
        },
    };
    let _ = node.truncate(0);
    let _ = node.write_at(0, text.as_bytes());
}

/// Сколько ссылок подряд проходит [`follow`] (у Linux — 40).
const MAX_HOPS: usize = 8;

/// `symlink(target, path)` / `symlinkat(target, dir, path)`.
pub fn symlink(target: usize, dir: usize, path: usize) -> i64 {
    let target = match user_string(target) {
        Ok(text) if !text.is_empty() => text,
        Ok(_) => return -e::ENOENT,
        Err(err) => return err,
    };
    let linux = match linux_path(dir, path) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if exists_on_disk(&linux) {
        return -e::EEXIST;
    }
    load();
    {
        let mut links = LINKS.lock();
        if links.contains_key(&linux) {
            return -e::EEXIST;
        }
        links.insert(linux, target);
    }
    save();
    0
}

/// Есть ли файл или каталог по пути (не ссылка).
fn exists_on_disk(linux: &str) -> bool {
    let cred = crate::user::credentials();
    let path = crate::linux_path::resolve(linux, cred);
    matches!(crate::fs::resolve_as(cred, &path, crate::vfs::perm::Access::NONE), Some(Ok(_)))
}

/// Текст ссылки по абсолютному пути; `None` — это не ссылка.
pub fn target(linux: &str) -> Option<String> {
    load();
    LINKS.lock().get(linux).cloned()
}

/// Пройти ссылки: путь, на который в итоге указывает `linux` (сам он, если
/// это не ссылка). Относительная ссылка — от её каталога.
///
/// Ссылкой может быть и каталог посреди пути (31.10.2026): `jetbrainsd` Rider
/// держит `bundles/current` → `extract/…` и запускает себя как
/// `bundles/current/jetbrainsd/bin/jetbrainsd`. Пока сверялся только путь
/// целиком, такой запуск отвечал `ENOENT`, и Rider перезапускал демон
/// десятки раз. Поэтому ищется самый длинный префикс пути (по границе
/// имён), который сам ссылка, и остаток пути дописывается к её цели.
pub fn follow(linux: &str) -> String {
    load();
    let mut path = String::from(linux);
    for _ in 0..MAX_HOPS {
        let hit = {
            let links = LINKS.lock();
            if links.is_empty() {
                return path;
            }
            let mut end = path.len();
            loop {
                if let Some(next) = links.get(&path[..end]) {
                    break Some((end, next.clone()));
                }
                match path[..end].rfind('/') {
                    Some(at) if at > 0 => end = at,
                    _ => break None,
                }
            }
        };
        let Some((end, next)) = hit else {
            break;
        };
        let mut joined = if next.starts_with('/') {
            next
        } else {
            let parent = path[..end].rsplit_once('/').map_or("", |(parent, _)| parent);
            let mut joined = String::from(parent);
            joined.push('/');
            joined.push_str(&next);
            joined
        };
        joined.push_str(&path[end..]);
        path = normalize(&joined);
    }
    path
}

/// Убрать ссылку; `false` — по пути её нет.
pub fn remove(linux: &str) -> bool {
    load();
    let removed = LINKS.lock().remove(linux).is_some();
    if removed {
        save();
    }
    removed
}

/// Перенести ссылку `old` на `new` (замещая ссылку там); `false` — `old` не
/// ссылка.
pub fn rename(old: &str, new: &str) -> bool {
    load();
    let moved = {
        let mut links = LINKS.lock();
        match links.remove(old) {
            Some(text) => {
                links.insert(String::from(new), text);
                true
            }
            None => false,
        }
    };
    if moved {
        save();
    }
    moved
}

/// `readlink` по ссылке из таблицы; `None` — не ссылка.
pub fn read(linux: &str, buf: usize, size: usize) -> Option<i64> {
    let text = target(linux)?;
    let len = text.len().min(size);
    if !crate::user::space::user_can(buf, len, crate::mm::PageFlags::WRITE) {
        return Some(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::copy_nonoverlapping(text.as_ptr(), buf as *mut u8, len) };
    Some(len as i64)
}
