//! Monitor a launch came from (25.11, Roman on two 2560x1440 monitors: a
//! program started on the right monitor opened on the left, primary one).
//!
//! A new window opens on the monitor where it was asked for, as in Windows:
//!
//! * a click (double-click on a desktop icon or in «Файлы», an item of
//!   «Пуск» or a taskbar button) - the monitor of the click, if the launch
//!   follows it within [`CLICK_FRESH_MS`];
//! * otherwise the monitor of the focused window (Win+R, the Run dialog,
//!   the keyboard in «Пуск» or in «Файлы»);
//! * otherwise the primary monitor.
//!
//! The point is taken when the program is **spawned** (`launch::note_spawn`,
//! all `user::spawn_*` go there) and kept for that task: a game under Proton
//! opens its first window half a minute later, when the pointer and the
//! focus may be anywhere. Linux programs are found by their job (the root
//! process every descendant inherits), so the window of `wine64`, great-
//! grandchild of the started `bash`, goes where `bash` was started.
//! A program's next windows (dialogs, a second window) open over its
//! existing window instead (`ui::open_window_framed`).
//!
//! Only own spin locks here: `note_spawn` is called from under the desktop.

use alloc::vec::Vec;

use crate::sync::SpinLock;

/// A click older than this is not the launch's (ms).
const CLICK_FRESH_MS: u64 = 5_000;

/// How long a spawned task remembers where it was started (ms): a first
/// start of a game in a new Wine prefix under emulation takes minutes.
const TASK_KEEP_MS: u64 = 15 * 60_000;

/// How many started tasks are remembered.
const TASKS_MAX: usize = 48;

/// Last click (button pressed) on the desktop: point and time.
static CLICK: SpinLock<Option<((i32, i32), u64)>> = SpinLock::new(None);

/// Centre of the focused window (`None` - no window has the focus).
static FOCUS: SpinLock<Option<(i32, i32)>> = SpinLock::new(None);

/// Started tasks: task, point, time.
static TASKS: SpinLock<Vec<(u32, (i32, i32), u64)>> = SpinLock::new(Vec::new());

/// A button went down at `(x, y)` (desktop coordinates).
pub fn note_click(x: i32, y: i32) {
    *CLICK.lock() = Some(((x, y), crate::time::uptime_ms()));
}

/// The focused window is centred at this point now (`None` - nothing is
/// focused). Called whenever the desktop is given back.
pub fn note_focus(point: Option<(i32, i32)>) {
    *FOCUS.lock() = point;
}

/// Where a launch happening now comes from; `None` - the primary monitor.
#[must_use]
pub fn current() -> Option<(i32, i32)> {
    let now = crate::time::uptime_ms();
    if let Some((point, at)) = *CLICK.lock() {
        if now.saturating_sub(at) <= CLICK_FRESH_MS {
            return Some(point);
        }
    }
    *FOCUS.lock()
}

/// Task `task` was spawned: remember where from.
pub fn note_spawn(task: u32) {
    let Some(point) = current() else {
        return;
    };
    let now = crate::time::uptime_ms();
    let mut tasks = TASKS.lock();
    tasks.retain(|(_, _, at)| now.saturating_sub(*at) <= TASK_KEEP_MS);
    if tasks.len() >= TASKS_MAX {
        tasks.remove(0);
    }
    if tasks.try_reserve(1).is_ok() {
        tasks.push((task, point, now));
    }
}

/// Where the program of task `task` (job `job` for a Linux process) was
/// started; not remembered - where a launch happening now comes from.
#[must_use]
pub fn of_task(task: u32, job: Option<u32>) -> Option<(i32, i32)> {
    let now = crate::time::uptime_ms();
    let found = {
        let tasks = TASKS.lock();
        let fresh = |id: u32| tasks.iter().rev().find(|(t, _, at)| *t == id && now.saturating_sub(*at) <= TASK_KEEP_MS).map(|(_, point, _)| *point);
        fresh(task).or_else(|| job.and_then(fresh))
    };
    found.or_else(current)
}
