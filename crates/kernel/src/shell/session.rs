//! Вкладки терминала: у каждой — своя оболочка (задача), свой ввод.
//!
//! Первая вкладка (сессия 0) — оболочка, что была всегда: она же разбирает
//! весь ввод стола и раздаёт клавиши видимой вкладке. Остальные — задачи
//! `shell`, заведённые по Ctrl+Shift+T или «+»: каждая читает только свою
//! очередь клавиш, а её каталог, история, программа переднего плана и
//! терминал — свои (`sched::session`, `tty`).

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::input::KeyEvent;
use crate::sched::{self, MAX_SESSIONS};
use crate::sync::SpinLock;

/// Сколько клавиш вкладка держит непрочитанными.
const KEYS_MAX: usize = 64;

static KEYS: [SpinLock<VecDeque<KeyEvent>>; MAX_SESSIONS] = [const { SpinLock::new(VecDeque::new()) }; MAX_SESSIONS];
static SEQ: [AtomicU64; MAX_SESSIONS] = [const { AtomicU64::new(0) }; MAX_SESSIONS];
/// Оболочка вкладки работает.
static ALIVE: [AtomicBool; MAX_SESSIONS] = [const { AtomicBool::new(false) }; MAX_SESSIONS];
/// Вкладку попросили закрыть.
static CLOSING: [AtomicBool; MAX_SESSIONS] = [const { AtomicBool::new(false) }; MAX_SESSIONS];
/// Вкладка заведена, а её задача ещё не забрала номер.
static STARTING: [AtomicBool; MAX_SESSIONS] = [const { AtomicBool::new(false) }; MAX_SESSIONS];

/// Отдать клавишу оболочке вкладки `session`.
pub fn push(session: usize, event: KeyEvent) {
    {
        let mut keys = KEYS[session].lock();
        if keys.len() < KEYS_MAX && keys.try_reserve(1).is_ok() {
            keys.push_back(event);
        }
    }
    SEQ[session].fetch_add(1, Ordering::Release);
    crate::power::note_input();
    sched::wake_input();
}

/// Следующая клавиша вкладки `session`.
pub fn next(session: usize) -> Option<KeyEvent> {
    KEYS[session].lock().pop_front()
}

/// Счётчик клавиш вкладки: изменился — есть что разбирать.
#[must_use]
pub fn seq(session: usize) -> u64 {
    SEQ[session].load(Ordering::Acquire)
}

/// Работает ли оболочка вкладки.
#[must_use]
pub fn alive(session: usize) -> bool {
    session < MAX_SESSIONS && ALIVE[session].load(Ordering::Acquire)
}

/// Попросили ли вкладку закрыться.
#[must_use]
pub fn closing(session: usize) -> bool {
    CLOSING[session].load(Ordering::Acquire)
}

/// Открыть вкладку: завести её номер, сетку в окне и задачу оболочки.
pub fn open() {
    let Some(session) = (1..MAX_SESSIONS).find(|&session| !ALIVE[session].load(Ordering::Acquire) && !STARTING[session].load(Ordering::Acquire)) else {
        crate::kprintln!("  terminal    : no room for another tab ({MAX_SESSIONS} at most)");
        return;
    };
    KEYS[session].lock().clear();
    CLOSING[session].store(false, Ordering::Release);
    ALIVE[session].store(true, Ordering::Release);
    if !crate::ui::add_tab(session) {
        ALIVE[session].store(false, Ordering::Release);
        return;
    }
    STARTING[session].store(true, Ordering::Release);
    if let Err(err) = sched::spawn("shell", tab_task) {
        crate::kprintln!("  terminal    : cannot start the tab shell: {err}");
        STARTING[session].store(false, Ordering::Release);
        ALIVE[session].store(false, Ordering::Release);
        crate::ui::remove_tab(session);
    }
}

/// Закрыть вкладку `session`: её программа снимается, оболочка уходит.
pub fn close(session: usize) {
    if session == 0 || !alive(session) {
        return;
    }
    CLOSING[session].store(true, Ordering::Release);
    if let Some(task) = crate::tty::foreground_of(session) {
        let _ = crate::user::request_kill(task);
    }
    SEQ[session].fetch_add(1, Ordering::Release);
    sched::wake_input();
}

/// Закрыть все вкладки, кроме первой (сеанс кончается).
pub fn close_all() {
    for session in 1..MAX_SESSIONS {
        close(session);
    }
}

/// Тело задачи оболочки вкладки.
fn tab_task() {
    let Some(session) = (1..MAX_SESSIONS).find(|&session| STARTING[session].swap(false, Ordering::AcqRel)) else {
        return;
    };
    sched::set_session(session);
    super::run(false);
    ALIVE[session].store(false, Ordering::Release);
    KEYS[session].lock().clear();
    crate::ui::remove_tab(session);
}
