//! Последние системные вызовы потока Linux (фаза 71h) — для журнала, когда
//! поток падает.
//!
//! Большие программы (Chromium) падают на собственных проверках молча:
//! сообщение проверки в выпускной сборке вырезано, остаётся `ud2`. Что
//! программа спросила у ядра перед падением и что получила, — почти всегда
//! ответ на вопрос «почему». Кольцо по слоту задачи, без замков: пишет только
//! сам поток, читает — он же, когда падает.

use alloc::string::String;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sched;

/// Сколько вызовов помнить.
const DEPTH: usize = 12;

/// Запись: номер, шесть аргументов, ответ.
struct Entry([AtomicU64; 8]);

static RING: [[Entry; DEPTH]; sched::MAX_TASKS] =
    [const { [const { Entry([const { AtomicU64::new(0) }; 8]) }; DEPTH] }; sched::MAX_TASKS];
static NEXT: [AtomicUsize; sched::MAX_TASKS] = [const { AtomicUsize::new(0) }; sched::MAX_TASKS];

/// Сколько неудачных вызовов уже напечатано (на всю систему).
static FAILED_SHOWN: AtomicUsize = AtomicUsize::new(0);
/// Больше не печатать: журнал нужен для первых отказов, а не для потока.
const FAILED_LIMIT: usize = 300;

/// Отказы, которые печатать незачем: обычный ход программы — «нет такого
/// файла», «пока пусто», «прервано», «срок», «уже есть», «не терминал»,
/// «нет детей», «нет процесса»; «нет вызова» печатается отдельно.
fn routine(errno: i64) -> bool {
    matches!(errno, 2 | 3 | 4 | 10 | 11 | 17 | 25 | 38 | 110)
}

/// Номер аргумента-пути у вызовов с путём.
fn path_arg(number: usize) -> Option<usize> {
    match number {
        2 | 4 | 6 | 21 | 82 | 83 | 84 | 85 | 87 => Some(0),
        257 | 258 | 262 | 263 | 264 | 269 | 316 | 439 => Some(1),
        _ => None,
    }
}

/// Напечатать необычный отказ (фаза 71i: так нашлось, на чём спотыкается
/// профиль Chromium). Первые [`FAILED_LIMIT`] на всю систему.
fn show_failure(number: usize, args: [usize; 6], result: i64) {
    // `readlink` не ссылки — `EINVAL`: так .NET и glibc спрашивают «ссылка
    // ли это» на каждом шаге пути, и эти строки вытесняли остальные.
    if result >= 0 || routine(-result) || (number == 89 && result == -22) || (number == 9 && result == -12) || FAILED_SHOWN.fetch_add(1, Ordering::Relaxed) >= FAILED_LIMIT {
        return;
    }
    let [a0, a1, a2, a3, ..] = args;
    let path = path_arg(number).and_then(|index| super::user_string(args[index]).ok()).unwrap_or_default();
    crate::kprintln!("  linux       : {} {number}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) = {result} {path}", sched::current());
}

/// Трассировка (`lxtrace <имя>`, фаза 74): печатать каждый вызов программ,
/// чей файл кончается на это имя. Имя, включатель и запас строк — общие с
/// родными вызовами ([`crate::user::trace`]).
fn trace(number: usize, args: [usize; 6], result: i64) {
    // Удачные `mprotect`, `madvise`, `clock_gettime` и `sched_yield` JIT
    // делает тысячами — они съедали весь запас строк.
    if result >= 0 && matches!(number, 10 | 24 | 28 | 228) {
        return;
    }
    if !crate::user::trace::wanted() {
        return;
    }
    // Пути — у вызовов с путём и у `rename`/`renameat`/`mkdir`/`unlinkat`.
    let path_of = |index: usize| super::user_string(args[index]).ok().unwrap_or_default();
    let shown = match number {
        82 => alloc::format!("{} -> {}", path_of(0), path_of(1)),
        264 | 316 => alloc::format!("{} -> {}", path_of(1), path_of(3)),
        83 => path_of(0),
        258 | 263 => path_of(1),
        // `poll`/`ppoll`: первые записи `pollfd` — `fd/events/revents`; по ним
        // видно, что именно «готово» у программы, крутящей опрос.
        7 | 271 if crate::user::space::user_can(args[0], args[1].min(4) * 8, crate::mm::PageFlags::READ) => {
            let mut text = String::new();
            for index in 0..args[1].min(4) {
                // SAFETY: проверено условием ветки.
                let at = args[0] + index * 8;
                let (fd, events, revents) = unsafe {
                    (
                        core::ptr::read_unaligned(at as *const i32),
                        core::ptr::read_unaligned((at + 4) as *const u16),
                        core::ptr::read_unaligned((at + 6) as *const u16),
                    )
                };
                text.push_str(&alloc::format!("fd {fd} ev {events:#x} rev {revents:#x}; "));
            }
            text
        }
        _ => path_arg(number).map(path_of).unwrap_or_default(),
    };
    let [a0, a1, a2, a3, ..] = args;
    crate::kprintln!("  trace {} {number}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) = {result} {shown}", sched::current());
}

/// Запомнить вызов текущего потока.
pub fn record(number: usize, args: [usize; 6], result: i64) {
    show_failure(number, args, result);
    if crate::user::trace::on() {
        trace(number, args, result);
    }
    let slot = sched::current_slot();
    let at = NEXT[slot].fetch_add(1, Ordering::Relaxed) % DEPTH;
    let [a0, a1, a2, a3, a4, a5] = args.map(|arg| arg as u64);
    let values = [number as u64, a0, a1, a2, a3, a4, a5, result as u64];
    for (cell, value) in RING[slot][at].0.iter().zip(values) {
        cell.store(value, Ordering::Relaxed);
    }
}

/// Последние `count` вызовов потока слота `slot` (для `tasks`, фаза 74): у
/// зависшего потока видно, что он делал перед тем, как встать.
pub fn dump_slot(slot: usize, count: usize) {
    let Some(next) = NEXT.get(slot).map(|next| next.load(Ordering::Relaxed)) else { return };
    for index in next.saturating_sub(count.min(DEPTH))..next {
        let entry = &RING[slot][index % DEPTH].0;
        let [number, a0, a1, a2, a3, _, _, result] = [0, 1, 2, 3, 4, 5, 6, 7].map(|field| entry[field].load(Ordering::Relaxed));
        crate::kprintln!("         was {number}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) = {}", result as i64);
    }
}

/// Последние вызовы текущего потока без путей — для мест под замками, где
/// читать память программы нельзя.
pub fn dump_plain() {
    let slot = sched::current_slot();
    let next = NEXT[slot].load(Ordering::Relaxed);
    for index in next.saturating_sub(DEPTH)..next {
        let entry = &RING[slot][index % DEPTH].0;
        let [number, a0, a1, a2, a3, _, _, result] = [0, 1, 2, 3, 4, 5, 6, 7].map(|field| entry[field].load(Ordering::Relaxed));
        crate::kprintln!("    {number:>3}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) = {}", result as i64);
    }
}

/// Напечатать последние вызовы текущего потока — от старых к новым.
pub fn dump() {
    let slot = sched::current_slot();
    let next = NEXT[slot].load(Ordering::Relaxed);
    let count = next.min(DEPTH);
    if count == 0 {
        return;
    }
    crate::kprintln!("  linux       : last {count} system calls of task {}:", sched::current());
    for index in next - count..next {
        let entry = &RING[slot][index % DEPTH].0;
        let [number, a0, a1, a2, a3, a4, a5, result] = [0, 1, 2, 3, 4, 5, 6, 7].map(|field| entry[field].load(Ordering::Relaxed));
        // Путь — из памяти программы: она ещё на месте, поток падает внутри неё.
        let path = path_arg(number as usize)
            .and_then(|index| super::user_string([a0, a1, a2, a3, a4, a5][index] as usize).ok())
            .unwrap_or_default();
        crate::kprintln!("    {number:>3}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}, {a4:#x}, {a5:#x}) = {} {path}", result as i64);
    }
}

/// Вызов, который поток выполняет сейчас (номер), — для журнала событий
/// внутри вызова: «последний писатель канала ушёл — каким вызовом».
static CURRENT: [AtomicU64; sched::MAX_TASKS] = [const { AtomicU64::new(u64::MAX) }; sched::MAX_TASKS];
/// Первые четыре аргумента текущего вызова — для `tasks`.
static CURRENT_ARGS: [[AtomicU64; 4]; sched::MAX_TASKS] =
    [const { [const { AtomicU64::new(0) }; 4] }; sched::MAX_TASKS];

/// Поток входит в вызов `number` (`usize::MAX` — выходит).
pub fn begin(number: usize, args: &[usize; 6]) {
    let slot = sched::current_slot();
    for (cell, value) in CURRENT_ARGS[slot].iter().zip(args) {
        cell.store(*value as u64, Ordering::Relaxed);
    }
    CURRENT[slot].store(number as u64, Ordering::Relaxed);
}

/// Вызов, в котором стоит поток слота `slot`: номер и четыре аргумента.
#[must_use]
pub fn in_call(slot: usize) -> Option<[u64; 5]> {
    let number = CURRENT.get(slot)?.load(Ordering::Relaxed);
    if number == u64::MAX {
        return None;
    }
    let [a0, a1, a2, a3] = [0, 1, 2, 3].map(|index| CURRENT_ARGS[slot][index].load(Ordering::Relaxed));
    Some([number, a0, a1, a2, a3])
}

/// Номер вызова, который выполняет текущий поток (`u64::MAX` — вне вызова
/// Linux).
#[must_use]
pub fn current() -> u64 {
    CURRENT[sched::current_slot()].load(Ordering::Relaxed)
}

/// Забыть вызовы слота — новый поток или процесс начинает с чистого кольца.
pub fn reset(slot: usize) {
    NEXT[slot].store(0, Ordering::Relaxed);
    CURRENT[slot].store(u64::MAX, Ordering::Relaxed);
}
