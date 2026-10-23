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

/// Запомнить вызов текущего потока.
pub fn record(number: usize, args: [usize; 6], result: i64) {
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
        crate::kprintln!("    {number:>3}({a0:#x}, {a1:#x}, {a2:#x}, {a3:#x}, {a4:#x}, {a5:#x}) = {}", result as i64);
    }
}

/// Забыть вызовы слота — новый поток или процесс начинает с чистого кольца.
pub fn reset(slot: usize) {
    NEXT[slot].store(0, Ordering::Relaxed);
}
