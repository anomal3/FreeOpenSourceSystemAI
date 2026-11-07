//! Трассировка системных вызовов по имени программы (`lxtrace <имя>`).
//!
//! Началась с программ Linux (фаза 74, `linux::recent`); с N4 браузера
//! (08.11.2026) печатает и **родные** вызовы — так нашлось, что SpiderMonkey
//! в `JS_Init` вечно открывал `/dev/urandom`. Здесь — то, что общее для обоих
//! слоёв и есть на обеих архитектурах: имя, включатель и запас строк.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::sched;

/// Чьи вызовы печатать: конец пути программы, несколько — через запятую.
static NAME: crate::sync::SpinLock<String> = crate::sync::SpinLock::new(String::new());
static ON: AtomicBool = AtomicBool::new(false);
/// Сколько строк ещё можно напечатать.
static LEFT: AtomicUsize = AtomicUsize::new(0);

/// Включить трассировку программ с именем `name` (`off` или пусто — выключить).
pub fn set(name: &str) {
    let name = name.trim();
    let on = !name.is_empty() && name != "off";
    *NAME.lock() = if on { String::from(name) } else { String::new() };
    LEFT.store(20_000, Ordering::Relaxed);
    ON.store(on, Ordering::Relaxed);
}

/// Включена ли трассировка вообще (дёшево — до любых замков).
pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Печатать ли вызов текущей программы: имя совпало и запас строк не кончился
/// (строка при этом списывается).
pub fn wanted() -> bool {
    if !on() {
        return false;
    }
    let Some(path) = super::with_current(|program| program.path.clone()) else { return false };
    let matches = {
        let name = NAME.lock();
        name.split(',').any(|one| !one.is_empty() && path.ends_with(one))
    };
    matches && LEFT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |left| left.checked_sub(1)).is_ok()
}

/// Родной вызов (`user_abi::SYS_*`): у `open`/`stat` и прочих с путём
/// (`ptr, len`) печатается и путь. Вывод программы в терминал не печатается —
/// он и так в журнале.
pub fn native(number: usize, args: [usize; 3], result: i64) {
    if !on() || (number == user_abi::SYS_WRITE && args[0] <= 2) || !wanted() {
        return;
    }
    let [a0, a1, a2] = args;
    let shown = match number {
        user_abi::SYS_OPEN | user_abi::SYS_STAT | user_abi::SYS_CREATE | user_abi::SYS_MKDIR | user_abi::SYS_REMOVE
            if a1 <= 255 && super::space::user_can(a0, a1, crate::mm::PageFlags::READ) =>
        {
            // SAFETY: проверено условием ветки.
            let bytes = unsafe { core::slice::from_raw_parts(a0 as *const u8, a1) };
            String::from(core::str::from_utf8(bytes).unwrap_or("?"))
        }
        _ => String::new(),
    };
    crate::kprintln!("  ntrace {} {number}({a0:#x}, {a1:#x}, {a2:#x}) = {result} {shown}", sched::current());
}
