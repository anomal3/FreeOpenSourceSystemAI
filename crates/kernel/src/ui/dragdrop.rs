//! Drag and drop between program windows and the desktop (27.11, Roman on
//! image 0.9.1121, hw-log20).
//!
//! Two complaints: a file dragged from «Файлы» onto the desktop was not
//! copied (between windows it was - the windows of «Файлы» agree through a
//! mailbox, and the desktop, drawn by the kernel, never read it), and nothing
//! showed what was dragged once the pointer left the window (the program can
//! draw only inside its own surface).
//!
//! The contract, both through `SYS_CLIPBOARD`:
//!
//! * `CLIP_DRAG` - the program drags things: the count and up to three names
//!   (`d`/`f` and the name, a line each). The desktop shows a half-transparent
//!   copy of their icons under the pointer over all layers until the left
//!   button is released (or the program says an empty text). Asked while
//!   the button is not held by a program window, the request is dropped:
//!   a late one must not leave a ghost on the screen.
//! * `CLIP_DROP` - the program asks where its drag ended. The desktop
//!   remembers the place of the last left button release of a program
//!   window (bare desktop: the desktop folder and the cell under the
//!   pointer; a folder icon: that folder). The answer is the folder; the
//!   program copies or moves there itself, with its own progress window, and
//!   the new icons take the cell they were dropped on when they appear.

use alloc::string::String;
use alloc::vec::Vec;

use crate::kprintln;
use crate::sync::SpinLock;

use super::compositor::Compositor;

/// A ghost asked by a program: `Some(None)` - take it away.
static REQUEST: SpinLock<Option<Option<(Vec<(bool, String)>, usize)>>> = SpinLock::new(None);

/// Where the last left button release of a program window happened, if
/// on the desktop: when (ms), the folder, the cell (bare desktop only).
static LAST_DROP: SpinLock<Option<(u64, String, Option<usize>)>> = SpinLock::new(None);

/// How long the program has to ask after the release.
const DROP_FRESH_MS: u64 = 3_000;

/// Most names in a ghost (the stack shows three).
const GHOST_NAMES: usize = 3;

/// `CLIP_DRAG`: the wire `count\n` then `d name` / `f name` lines; empty -
/// take the ghost away. `false` - not understood.
pub fn request_ghost(wire: &str) -> bool {
    if wire.is_empty() {
        *REQUEST.lock() = Some(None);
        return true;
    }
    let mut lines = wire.split('\n');
    let Some(count) = lines.next().and_then(|line| line.trim().parse::<usize>().ok()) else {
        return false;
    };
    let items: Vec<(bool, String)> = lines
        .filter_map(|line| {
            let (kind, name) = line.split_once(' ')?;
            Some((kind == "d", String::from(name)))
        })
        .filter(|(_, name)| !name.is_empty())
        .take(GHOST_NAMES)
        .collect();
    if items.is_empty() || count == 0 {
        return false;
    }
    *REQUEST.lock() = Some(Some((items, count.max(1))));
    true
}

/// Apply a ghost request (every pointer event, before it is handled).
pub fn apply(desktop: &mut Compositor) {
    let Some(request) = REQUEST.lock().take() else {
        return;
    };
    match request {
        Some((items, count)) => {
            // Only while a program window holds the left button.
            if !desktop.captured().is_some_and(|(_, buttons)| buttons & 1 != 0) {
                return;
            }
            if desktop.show_program_ghost(&items, count) {
                kprintln!("  desktop     : drag ghost of {count} item(s) from a program");
            }
        }
        None => {
            if desktop.program_ghost() {
                desktop.set_drag_ghost(None, true);
            }
        }
    }
}

/// The left button of program window was released at `x, y` (before the
/// window hears it): the ghost goes, and a release on the desktop is
/// remembered for `CLIP_DROP`.
pub fn released(desktop: &mut Compositor, x: i32, y: i32) {
    let had_ghost = desktop.program_ghost();
    if had_ghost {
        desktop.set_drag_ghost(None, true);
    }
    *REQUEST.lock() = None;
    if !desktop.bare_desktop_at(x, y) {
        *LAST_DROP.lock() = None;
        return;
    }
    // On a folder icon - into the folder; on another icon or the bare
    // desktop - into the desktop folder, at the cell under the pointer.
    let folder = desktop
        .icon_at(x, y)
        .filter(|index| matches!(desktop.icon_kind(*index), Some(super::icons::Kind::Folder)))
        .and_then(|index| desktop.icon_path(index));
    let (dir, slot) = match folder {
        Some(path) => (path, None),
        None => {
            let dir = super::context::desktop_dir();
            let _ = super::context::ensure_dir(&dir);
            (dir, desktop.icon_slot_at(x, y))
        }
    };
    if had_ghost {
        kprintln!("  desktop     : drag released over the desktop at {x},{y} ({dir}{})", slot.map_or_else(String::new, |slot| alloc::format!(", cell {slot}")));
    }
    *LAST_DROP.lock() = Some((crate::time::uptime_ms(), dir, slot));
}

/// `CLIP_DROP`: the program's drag of `paths` ended - on the desktop? The
/// folder to put them in, `None` - not on the desktop (or too long ago).
pub fn take_drop(paths: &[String]) -> Option<String> {
    let (at, dir, slot) = LAST_DROP.lock().take()?;
    if crate::time::uptime_ms().saturating_sub(at) > DROP_FRESH_MS || paths.is_empty() {
        return None;
    }
    if let Some(slot) = slot {
        let targets: Vec<String> = paths
            .iter()
            .map(|path| {
                let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or(path);
                alloc::format!("{}/{name}", dir.trim_end_matches('/'))
            })
            .collect();
        super::icons::expect_at(&targets, slot);
    }
    kprintln!(
        "  desktop     : {} item(s) dropped on the desktop into {dir}{}",
        paths.len(),
        slot.map_or_else(String::new, |slot| alloc::format!(" at cell {slot}"))
    );
    Some(dir)
}

/// Desktop icons released over a program window (27.11, ROADMAP: "desktop
/// icons cannot be dropped into Files"): when (ms) and the wire for
/// `CLIP_TAKE` - `c` (Ctrl: copy), `M` (Shift: move) or `m` (the program
/// decides: the same volume moves, another copies), then a path a line.
static HANDOFF: SpinLock<Option<(u64, String)>> = SpinLock::new(None);

/// How long the window under the pointer has to take the icons.
const HANDOFF_FRESH_MS: u64 = 1_500;

/// The desktop's things `paths` were released over a program window; the
/// window takes them with `CLIP_TAKE` when it hears the pointer.
pub fn hand_off(paths: &[String], mode: char) {
    let mut wire = String::new();
    wire.push(mode);
    for path in paths {
        wire.push('\n');
        wire.push_str(path);
    }
    *HANDOFF.lock() = Some((crate::time::uptime_ms(), wire));
}

/// `CLIP_TAKE`: things the desktop dropped on a window, if fresh (taken
/// once).
pub fn take_handoff() -> Option<String> {
    let (at, wire) = HANDOFF.lock().take()?;
    (crate::time::uptime_ms().saturating_sub(at) <= HANDOFF_FRESH_MS).then_some(wire)
}
