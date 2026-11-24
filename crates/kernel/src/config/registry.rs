//! Settings store — «реестр» FossAi (24.11.2026).
//!
//! Roman: «что-то вроде реестра, но быстрее и проще». Keys `hive/path`, string
//! values, two scopes (system and the person), a text file per hive, parsed
//! once and kept in memory, written atomically. The format, the key names and
//! the documentation for ports are in `sysconf::store`; this module owns the
//! files, the cache and the rights.
//!
//! # Files
//!
//! * system: `/etc/registry/<hive>.reg` (root writes it; if it is missing,
//!   `/usr/share/defaults/etc/registry/<hive>.reg` of the image is read);
//! * user: `~/.config/registry/<hive>.reg` of the session user.
//!
//! A write goes to `<hive>.reg.new`, which then replaces the hive by rename;
//! a reader that finds only `.new` (power lost between the two) takes it. The
//! volume is synced at once: a choice the person just made must survive the
//! power button.
//!
//! # Cache
//!
//! Each hive file is parsed once; a cached hive is checked against the size
//! of its file at most every [`RECHECK_MS`] (someone edited it in Notepad).
//! Every change bumps [`seq`]: programs poll it (`REG_SEQ`) and re-read their
//! options only when it moved — «Файлы» and the desktop pick up «Параметры
//! папок» at once.
//!
//! # Rights
//!
//! A program may write the system scope only as root, the user scope as root
//! or as the session user. Reading is open to everyone: there are no secrets
//! here (passwords live elsewhere, `0600`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use sysconf::store;

use crate::vfs::perm::Credentials;
use crate::vfs::VfsError;

/// Which file a key lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// `/etc/registry` — for every user; packages register here.
    System,
    /// `~/.config/registry` — what the person chose.
    User,
}

impl Scope {
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "system" | "sys" | "hklm" => Some(Self::System),
            "user" | "hkcu" => Some(Self::User),
            _ => None,
        }
    }

    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
        }
    }
}

/// Why a change was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegError {
    /// Not `hive/path`, or a value too long.
    BadKey,
    /// The file system refused.
    Fs(VfsError),
}

impl core::fmt::Display for RegError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BadKey => f.write_str("not a key (hive/path) or the value is too long"),
            Self::Fs(err) => write!(f, "{err}"),
        }
    }
}

/// One parsed hive file.
struct Cached {
    path: String,
    map: Arc<BTreeMap<String, String>>,
    /// Size of the file when it was read; `None` — there was no file.
    stamp: Option<u64>,
    checked_ms: u64,
}

static CACHE: crate::sync::SpinLock<Vec<Cached>> = crate::sync::SpinLock::new(Vec::new());

/// Writers one at a time: a change is read-modify-write of a whole file.
static WRITER: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

/// Grows with every change (and with every edit seen from outside).
static SEQ: AtomicU64 = AtomicU64::new(1);

/// How often a cached hive is compared with its file.
const RECHECK_MS: u64 = 1_500;

/// Largest hive file read.
const LIMIT: usize = 1 << 20;

/// Change counter for programs that cache what they read.
#[must_use]
pub fn seq() -> u64 {
    SEQ.load(Ordering::Relaxed)
}

/// Directory of the hives of a scope.
#[must_use]
pub fn dir(scope: Scope) -> String {
    match scope {
        Scope::System => alloc::format!("{}/registry", super::ETC),
        Scope::User => alloc::format!("{}/.config/registry", crate::ui::context::home_dir()),
    }
}

fn hive_path(scope: Scope, hive: &str) -> String {
    alloc::format!("{}/{hive}.reg", dir(scope))
}

/// Size of a file, `None` — no file.
fn size_of(path: &str) -> Option<u64> {
    match crate::fs::read(path, 0) {
        Some(Ok((_, size))) => Some(size),
        _ => None,
    }
}

fn read_text(path: &str) -> Option<String> {
    match crate::fs::read(path, LIMIT) {
        Some(Ok((bytes, _))) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        _ => None,
    }
}

/// The hive as it is on the disk now: the file, its unfinished replacement,
/// or (system scope) the default of the image.
fn read_disk(scope: Scope, path: &str) -> (BTreeMap<String, String>, Option<u64>) {
    if let Some(text) = read_text(path) {
        return (store::parse(&text), Some(text.len() as u64));
    }
    if let Some(text) = read_text(&alloc::format!("{path}.new")) {
        return (store::parse(&text), None);
    }
    if scope == Scope::System {
        let name = path.rsplit('/').next().unwrap_or("");
        if let Some(text) = read_text(&alloc::format!("{}/registry/{name}", super::DEFAULTS)) {
            return (store::parse(&text), None);
        }
    }
    (BTreeMap::new(), None)
}

/// The parsed hive of a scope, from the cache when it is fresh.
fn hive(scope: Scope, hive: &str) -> Arc<BTreeMap<String, String>> {
    let path = hive_path(scope, hive);
    let now = crate::time::uptime_ms();
    {
        let mut cache = CACHE.lock();
        if let Some(entry) = cache.iter_mut().find(|entry| entry.path == path) {
            if now.saturating_sub(entry.checked_ms) < RECHECK_MS {
                return entry.map.clone();
            }
        }
    }
    let stamp = size_of(&path);
    {
        let mut cache = CACHE.lock();
        if let Some(entry) = cache.iter_mut().find(|entry| entry.path == path) {
            if entry.stamp == stamp {
                entry.checked_ms = now;
                return entry.map.clone();
            }
        }
    }
    let (map, stamp) = read_disk(scope, &path);
    let map = Arc::new(map);
    let mut cache = CACHE.lock();
    let known = cache.iter().position(|entry| entry.path == path);
    if let Some(at) = known {
        if cache[at].map != map {
            SEQ.fetch_add(1, Ordering::Relaxed);
        }
        cache.remove(at);
    }
    cache.push(Cached { path, map: map.clone(), stamp, checked_ms: now });
    map
}

/// Value of `key` in one scope.
#[must_use]
pub fn get_in(scope: Scope, key: &str) -> Option<String> {
    let (name, rest) = store::split_key(key)?;
    hive(scope, name).get(rest).cloned()
}

/// Value of `key`: the person's, else the system's, else built in.
#[must_use]
pub fn get(key: &str) -> Option<String> {
    get_in(Scope::User, key).or_else(|| get_in(Scope::System, key)).or_else(|| store::builtin(key).map(String::from))
}

/// Keys starting with `prefix` (a whole key prefix, `filetypes/.txt/`) in one
/// scope, with values; `None` scope — merged as [`get`] reads them.
#[must_use]
pub fn list(scope: Option<Scope>, prefix: &str) -> Vec<(String, String)> {
    let Some((name, rest)) = prefix.split_once('/') else {
        return Vec::new();
    };
    if !store::valid_hive(name) {
        return Vec::new();
    }
    let mut merged: BTreeMap<String, String> = BTreeMap::new();
    if scope.is_none() {
        let head = alloc::format!("{name}/{rest}");
        for (key, value) in store::BUILTIN.iter().filter(|(key, _)| key.starts_with(&head)) {
            merged.insert(String::from(*key), String::from(*value));
        }
    }
    let scopes: &[Scope] = match scope {
        Some(Scope::System) => &[Scope::System],
        Some(Scope::User) => &[Scope::User],
        None => &[Scope::System, Scope::User],
    };
    for scope in scopes {
        let map = hive(*scope, name);
        for (key, value) in map.range(String::from(rest)..).take_while(|(key, _)| key.starts_with(rest)) {
            merged.insert(alloc::format!("{name}/{key}"), value.clone());
        }
    }
    merged.into_iter().collect()
}

/// May `cred` change `scope`.
#[must_use]
pub fn may_write(cred: Credentials, scope: Scope) -> bool {
    cred.uid == 0 || (scope == Scope::User && cred.uid == crate::user::session::credentials().uid)
}

/// Set (`Some`) or remove (`None`) `key` in `scope`. The file is written
/// with the rights of the scope's owner (root, or the session user).
pub fn set(scope: Scope, key: &str, value: Option<&str>) -> Result<(), RegError> {
    let (name, rest) = store::split_key(key).ok_or(RegError::BadKey)?;
    if value.is_some_and(|value| value.len() > store::MAX_VALUE) {
        return Err(RegError::BadKey);
    }
    let _writer = WRITER.lock();
    let path = hive_path(scope, name);
    let (mut map, _) = read_disk(scope, &path);
    let changed = match value {
        Some(value) => map.insert(String::from(rest), String::from(value)).as_deref() != Some(value),
        None => map.remove(rest).is_some(),
    };
    if !changed {
        return Ok(());
    }
    let text = store::format(name, &map);
    write_atomic(scope, &path, &text).map_err(RegError::Fs)?;
    let mut cache = CACHE.lock();
    cache.retain(|entry| entry.path != path);
    cache.push(Cached { path, map: Arc::new(map), stamp: Some(text.len() as u64), checked_ms: crate::time::uptime_ms() });
    SEQ.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Who owns the files of a scope.
fn owner(scope: Scope) -> Credentials {
    match scope {
        Scope::System => Credentials::ROOT,
        Scope::User => crate::user::session::credentials(),
    }
}

/// Make `path` and its parents.
fn make_dirs(cred: Credentials, path: &str) -> Result<(), VfsError> {
    let mut built = String::new();
    for part in path.split('/').filter(|part| !part.is_empty()) {
        built.push('/');
        built.push_str(part);
        match crate::fs::mkdir_as(cred, &built, 0o755) {
            Some(Ok(())) | Some(Err(VfsError::Exists)) => {}
            Some(Err(err)) => {
                // A parent the person may not write (`/home`) exists already.
                if crate::fs::resolve_as(cred, &built, crate::vfs::perm::Access::NONE).is_some_and(|node| node.is_ok()) {
                    continue;
                }
                return Err(err);
            }
            None => return Err(VfsError::NotFound),
        }
    }
    Ok(())
}

/// Write `text` to `<path>.new`, then rename it over `path`.
fn write_atomic(scope: Scope, path: &str, text: &str) -> Result<(), VfsError> {
    let cred = owner(scope);
    let folder = path.rsplit_once('/').map_or("/", |(dir, _)| dir);
    make_dirs(cred, folder)?;
    let fresh = alloc::format!("{path}.new");
    let node = match crate::fs::resolve_as(cred, &fresh, crate::vfs::perm::Access::WRITE) {
        Some(Ok(node)) => {
            node.truncate(0)?;
            node
        }
        Some(Err(VfsError::NotFound)) => match crate::fs::create_as(cred, &fresh, 0o644) {
            Some(result) => result?,
            None => return Err(VfsError::NotFound),
        },
        Some(Err(err)) => return Err(err),
        None => return Err(VfsError::NotFound),
    };
    node.write_at(0, text.as_bytes())?;
    drop(node);
    match crate::fs::rename_as(cred, &fresh, path) {
        Some(Ok(())) => {}
        // A file system that does not replace on rename: remove, then rename
        // (the reader takes `.new` if power is lost in between).
        Some(Err(_)) => {
            let _ = crate::fs::remove_as(cred, path);
            match crate::fs::rename_as(cred, &fresh, path) {
                Some(Ok(())) => {}
                Some(Err(err)) => return Err(err),
                None => return Err(VfsError::NotFound),
            }
        }
        None => return Err(VfsError::NotFound),
    }
    let _ = crate::fs::with_fs(path, |fs, _| fs.sync());
    Ok(())
}

// ---------------------------------------------------------------------------
// File types
// ---------------------------------------------------------------------------

/// Is the program of a command line installed: its first word, if it is a
/// path, names a file (a script run by a shell — the script, 24.11 VLC).
#[must_use]
pub fn program_exists(command: &str) -> bool {
    match sysconf::store::command_program(command) {
        Some(program) if program.starts_with('/') => size_of(program).is_some(),
        Some(_) => true,
        None => false,
    }
}

/// What opens files with extension `ext` (lower case, no dot): the person's
/// choice, else the default of the system — only if its program is there.
#[must_use]
pub fn default_program(ext: &str) -> Option<String> {
    if ext.is_empty() {
        return None;
    }
    let key = store::default_key(ext);
    [get_in(Scope::User, &key), get_in(Scope::System, &key), store::builtin(&key).map(String::from)]
        .into_iter()
        .flatten()
        .find(|command| !command.trim().is_empty() && program_exists(command))
}

/// Programs registered for `ext`: id, command; only installed ones.
#[must_use]
pub fn registered_programs(ext: &str) -> Vec<(String, String)> {
    let prefix = store::openwith_prefix(ext);
    list(None, &prefix)
        .into_iter()
        .filter_map(|(key, command)| {
            let id = key.strip_prefix(&prefix)?;
            (!id.is_empty() && program_exists(&command)).then(|| (String::from(id), command))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

/// `reg` in the kernel shell: `reg get KEY`, `reg set [user|system] KEY
/// VALUE`, `reg rm [user|system] KEY`, `reg list PREFIX`, `reg path`.
/// Lines to print.
pub fn shell_command(argument: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut words = argument.split_whitespace();
    let verb = words.next().unwrap_or("");
    let scoped = |words: &mut core::str::SplitWhitespace<'_>| -> (Scope, Option<String>) {
        let first = words.next().map(String::from);
        match first.as_deref().and_then(Scope::from_word) {
            Some(scope) => (scope, words.next().map(String::from)),
            None => (Scope::User, first),
        }
    };
    match verb {
        "get" => match words.next() {
            Some(key) => match get(key) {
                Some(value) => out.push(alloc::format!("  reg: {key} = {value}")),
                None => out.push(alloc::format!("  reg: {key} is not set")),
            },
            None => out.push(alloc::format!("  usage: reg get hive/key")),
        },
        "set" => {
            let (scope, key) = scoped(&mut words);
            let value = words.collect::<Vec<_>>().join(" ");
            match key {
                Some(key) => match set(scope, &key, Some(&value)) {
                    Ok(()) => out.push(alloc::format!("  reg: {} {key} = {value}", scope.word())),
                    Err(err) => out.push(alloc::format!("  reg: cannot set {key}: {err}")),
                },
                None => out.push(alloc::format!("  usage: reg set [user|system] hive/key value")),
            }
        }
        "rm" | "remove" | "del" => {
            let (scope, key) = scoped(&mut words);
            match key {
                Some(key) => match set(scope, &key, None) {
                    Ok(()) => out.push(alloc::format!("  reg: {} {key} removed", scope.word())),
                    Err(err) => out.push(alloc::format!("  reg: cannot remove {key}: {err}")),
                },
                None => out.push(alloc::format!("  usage: reg rm [user|system] hive/key")),
            }
        }
        "list" | "ls" => {
            let prefix = words.next().unwrap_or("");
            let found = list(None, prefix);
            for (key, value) in &found {
                out.push(alloc::format!("  reg: {key} = {value}"));
            }
            out.push(alloc::format!("  reg: {} key(s) under '{prefix}'", found.len()));
        }
        "path" => out.push(alloc::format!("  reg: system {}, user {}", dir(Scope::System), dir(Scope::User))),
        _ => out.push(alloc::format!("  usage: reg get|set|rm|list|path (keys hive/path, see sysconf::store)")),
    }
    out
}
