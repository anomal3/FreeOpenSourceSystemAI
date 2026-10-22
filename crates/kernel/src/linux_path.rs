//! Корень программ Linux (фаза 71b): где программа Linux находит `/usr/lib`.
//!
//! Программам Linux нужен свой корень — glibc, `ld-linux` и библиотеки из
//! пакетов Debian, — но и файлы системы им нужны: свой дом, `/tmp`, носитель.
//! Поэтому корень не подменяется целиком, а **перекрывает** системный, как
//! «эмуляционный путь» Linuxulator во FreeBSD: абсолютный путь сначала ищется
//! в [`LINUX_ROOT`], и только если там его нет — как есть.
//!
//! Корень приезжает пакетом `debian` (`pkg install /media/debian-12.fpk`), и
//! пакеты ставятся в `/opt/<имя>` — отсюда и путь. Нет пакета — нет и корня, и
//! программа Linux видит систему как есть (так живёт статическая
//! `/compat/linux/bin/hello`, фаза 71a).

use alloc::string::String;

use crate::vfs::perm::{Access, Credentials};

/// Корень Linux в системе.
pub const LINUX_ROOT: &str = "/opt/debian";

/// Путь, который программа Linux назвала `path` (абсолютный), — в системе.
#[must_use]
pub fn resolve(path: &str, cred: Credentials) -> String {
    if !path.starts_with('/') || path == LINUX_ROOT || path.starts_with("/opt/debian/") {
        return String::from(path);
    }
    let mut inside = String::with_capacity(LINUX_ROOT.len() + path.len());
    inside.push_str(LINUX_ROOT);
    if path != "/" {
        inside.push_str(path);
    }
    match crate::fs::resolve_as(cred, &inside, Access::NONE) {
        Some(Ok(_)) => inside,
        _ => String::from(path),
    }
}

/// Обратное: системный путь — так, как его видит программа Linux (для
/// `/proc/self/exe` и `getcwd`).
#[must_use]
pub fn shown(path: &str) -> &str {
    match path.strip_prefix(LINUX_ROOT) {
        Some("") => "/",
        Some(rest) if rest.starts_with('/') => rest,
        _ => path,
    }
}
