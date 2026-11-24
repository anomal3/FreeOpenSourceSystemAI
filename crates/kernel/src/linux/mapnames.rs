//! Names of mapped libraries for the diagnostic lines (24.11.2026, hw-log17).
//!
//! A mapping knows its file only as a node (`user::ImageBacking`), so the
//! perf-diag `callers` line and the fault dumps could say "+0x5c821 of a
//! 765224-byte file", not which file. `openat` of a Linux program remembers
//! the path of every file that looks like code (`*.so*`, `*.dll`, `*.exe`,
//! `*.drv`, `*.sys`) by its node, weakly: the entry dies with the node, and
//! the table is bounded. Lookups are for diagnostics only (a mutex walk).

use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use crate::vfs::Node;

/// Table bound: dead entries go first, then the oldest.
const LIMIT: usize = 4096;

static NAMES: crate::sync::Mutex<Vec<(Weak<dyn Node>, String)>> = crate::sync::Mutex::new(Vec::new());

/// Is `path` worth remembering - a file that programs map as code?
#[must_use]
pub fn wanted(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = |suffix: &str| name.len() > suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix);
    name.contains(".so") || lower(".dll") || lower(".exe") || lower(".drv") || lower(".sys")
}

/// Remember that `node` was opened as `path`.
pub fn remember(node: &Arc<dyn Node>, path: &str) {
    let key = Arc::as_ptr(node) as *const ();
    let mut names = NAMES.lock();
    if let Some(entry) = names.iter_mut().find(|(live, _)| live.as_ptr() as *const () == key) {
        if entry.1 != path {
            entry.1 = String::from(path);
        }
        return;
    }
    if names.len() >= LIMIT {
        names.retain(|(live, _)| live.strong_count() > 0);
        if names.len() >= LIMIT {
            names.drain(..LIMIT / 4);
        }
    }
    names.push((Arc::downgrade(node), String::from(path)));
}

/// The path `node` was opened by, if remembered.
#[must_use]
pub fn name_of(node: &Arc<dyn Node>) -> Option<String> {
    let key = Arc::as_ptr(node) as *const ();
    NAMES.lock().iter().find(|(live, _)| live.as_ptr() as *const () == key && live.strong_count() > 0).map(|(_, path)| path.clone())
}

/// Short form for a log line: the last two path components.
#[must_use]
pub fn short(path: &str) -> &str {
    let mut cut = path.len();
    for _ in 0..2 {
        match path[..cut].rfind('/') {
            Some(at) => cut = at,
            None => return path,
        }
    }
    &path[cut + 1..]
}
