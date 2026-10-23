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
    let mut links = LINKS.lock();
    if links.contains_key(&linux) {
        return -e::EEXIST;
    }
    links.insert(linux, target);
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
    LINKS.lock().get(linux).cloned()
}

/// Пройти ссылки: путь, на который в итоге указывает `linux` (сам он, если
/// это не ссылка). Относительная ссылка — от её каталога.
pub fn follow(linux: &str) -> String {
    let mut path = String::from(linux);
    for _ in 0..MAX_HOPS {
        let Some(next) = target(&path) else {
            break;
        };
        path = if next.starts_with('/') {
            normalize(&next)
        } else {
            let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
            let mut joined = String::from(parent);
            joined.push('/');
            joined.push_str(&next);
            normalize(&joined)
        };
    }
    path
}

/// Убрать ссылку; `false` — по пути её нет.
pub fn remove(linux: &str) -> bool {
    LINKS.lock().remove(linux).is_some()
}

/// Перенести ссылку `old` на `new` (замещая ссылку там); `false` — `old` не
/// ссылка.
pub fn rename(old: &str, new: &str) -> bool {
    let mut links = LINKS.lock();
    match links.remove(old) {
        Some(text) => {
            links.insert(String::from(new), text);
            true
        }
        None => false,
    }
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
