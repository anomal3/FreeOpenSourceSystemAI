//! Последние системные вызовы потока Linux (фаза 71h) — для журнала, когда
//! поток падает.
//!
//! Большие программы (Chromium) падают на собственных проверках молча:
//! сообщение проверки в выпускной сборке вырезано, остаётся `ud2`. Что
//! программа спросила у ядра перед падением и что получила, — почти всегда
//! ответ на вопрос «почему». Кольцо по слоту задачи, без замков: пишет только
//! сам поток, читает — он же, когда падает.

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
    if result >= 0 || routine(-result) || FAILED_SHOWN.fetch_add(1, Ordering::Relaxed) >= FAILED_LIMIT {
        return;
    }
    let [a0, a1, a2, a3, ..] = args;
    let path = path_arg(number).and_then(|index| super::user_string(args[index]).ok()).unwrap_or_default();
    crate::kprintln!("  linux       : {} {number}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}) = {result} {path}", sched::current());
}

/// Запомнить вызов текущего потока.
pub fn record(number: usize, args: [usize; 6], result: i64) {
    show_failure(number, args, result);
    let slot = sched::current_slot();
    let at = NEXT[slot].fetch_add(1, Ordering::Relaxed) % DEPTH;
    let [a0, a1, a2, a3, a4, a5] = args.map(|arg| arg as u64);
    let values = [number as u64, a0, a1, a2, a3, a4, a5, result as u64];
    for (cell, value) in RING[slot][at].0.iter().zip(values) {
        cell.store(value, Ordering::Relaxed);
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

/// Забыть вызовы слота — новый поток или процесс начинает с чистого кольца.
pub fn reset(slot: usize) {
    NEXT[slot].store(0, Ordering::Relaxed);
}
