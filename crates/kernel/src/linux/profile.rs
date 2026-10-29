//! Счёт системных вызовов Linux: сколько раз и сколько времени в сумме —
//! команда оболочки `lxstat` (можно и по SSH: `kernel lxstat`).
//!
//! Нужен, чтобы отвечать на «почему медленно» числами, а не догадками: Rider
//! в VirtualBox стартовал три минуты при простаивающих процессорах (27.10), и
//! по одному журналу программы не видно, на чём именно она стоит — на файлах,
//! на отображениях или на ожидании потоков.
//!
//! Время — от входа в вызов до выхода, то есть вместе со сном: у `futex` и
//! `poll` это ожидание, у `read` и `openat` — диск и файловая система. Без
//! замков: два атомика на номер вызова.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// Номера вызовов x86-64 укладываются в 512 с запасом (последний — 462).
const SLOTS: usize = 512;

static CALLS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
static NANOS: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];

/// Учесть вызов `number`, длившийся `nanos`.
pub fn count(number: usize, nanos: u64) {
    if number < SLOTS {
        CALLS[number].fetch_add(1, Ordering::Relaxed);
        NANOS[number].fetch_add(nanos, Ordering::Relaxed);
    }
}

/// Вызовы, которые были, — `(номер, сколько раз, наносекунд)`, по убыванию
/// времени. `reset` — обнулить счёт после чтения.
pub fn report(reset: bool) -> Vec<(usize, u64, u64)> {
    let mut rows: Vec<(usize, u64, u64)> = (0..SLOTS)
        .filter_map(|number| {
            let calls = if reset { CALLS[number].swap(0, Ordering::Relaxed) } else { CALLS[number].load(Ordering::Relaxed) };
            let nanos = if reset { NANOS[number].swap(0, Ordering::Relaxed) } else { NANOS[number].load(Ordering::Relaxed) };
            (calls > 0).then_some((number, calls, nanos))
        })
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    rows
}

/// Имя вызова для таблицы — самые частые; остальные — номером.
#[must_use]
pub fn name(number: usize) -> &'static str {
    match number {
        0 => "read",
        1 => "write",
        3 => "close",
        4 => "stat",
        5 => "fstat",
        6 => "lstat",
        7 => "poll",
        8 => "lseek",
        9 => "mmap",
        10 => "mprotect",
        11 => "munmap",
        12 => "brk",
        13 => "rt_sigaction",
        14 => "rt_sigprocmask",
        16 => "ioctl",
        17 => "pread64",
        18 => "pwrite64",
        19 => "readv",
        20 => "writev",
        21 => "access",
        23 => "select",
        24 => "sched_yield",
        25 => "mremap",
        26 => "msync",
        28 => "madvise",
        35 => "nanosleep",
        39 => "getpid",
        41 => "socket",
        42 => "connect",
        43 => "accept",
        44 => "sendto",
        45 => "recvfrom",
        46 => "sendmsg",
        47 => "recvmsg",
        56 => "clone",
        57 => "fork",
        59 => "execve",
        61 => "wait4",
        72 => "fcntl",
        74 => "fsync",
        75 => "fdatasync",
        76 => "truncate",
        77 => "ftruncate",
        78 => "getdents",
        79 => "getcwd",
        82 => "rename",
        83 => "mkdir",
        87 => "unlink",
        89 => "readlink",
        96 => "gettimeofday",
        186 => "gettid",
        202 => "futex",
        217 => "getdents64",
        228 => "clock_gettime",
        230 => "clock_nanosleep",
        232 => "epoll_wait",
        257 => "openat",
        262 => "newfstatat",
        263 => "unlinkat",
        270 => "pselect6",
        271 => "ppoll",
        281 => "epoll_pwait",
        285 => "fallocate",
        288 => "accept4",
        290 => "eventfd2",
        293 => "pipe2",
        318 => "getrandom",
        332 => "statx",
        _ => "",
    }
}
