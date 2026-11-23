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
/// Кто последним делал вызов (слот задачи) — чтобы найти, кто крутится.
static LAST: [AtomicU64; SLOTS] = [const { AtomicU64::new(0) }; SLOTS];
/// Последний готовый ответ `poll`: дескриптор и `revents` (диагностика
/// пустых циклов опроса).
pub static POLL_LAST: AtomicU64 = AtomicU64::new(0);

/// Кто последним делал вызов `number`.
#[must_use]
pub fn last_caller(number: usize) -> u64 {
    LAST.get(number).map_or(0, |last| last.load(Ordering::Relaxed))
}

/// Учесть вызов `number`, длившийся `nanos`.
pub fn count(number: usize, nanos: u64) {
    if number < SLOTS {
        CALLS[number].fetch_add(1, Ordering::Relaxed);
        LAST[number].store(crate::sched::running_slot() as u64, Ordering::Relaxed);
        NANOS[number].fetch_add(nanos, Ordering::Relaxed);
        // The longest call of the perf-diag window (hw-log14), without the
        // calls that exist to sleep; read first so the common short call does
        // not write a shared line.
        if nanos > WORST_NS.load(Ordering::Relaxed)
            && !matches!(number, 7 | 23 | 34 | 35 | 61 | 202 | 230 | 232 | 247 | 270 | 271 | 281 | 441 | 449 | 130 | 128)
            && WORST_NS.fetch_max(nanos, Ordering::Relaxed) < nanos
        {
            WORST_NR.store(number as u64, Ordering::Relaxed);
        }
    }
}

// ---- who makes the calls (hw-log15) ------------------------------------------
//
// D2R made 5-12 M `getrusage` and half as many `sched_yield` per 30 s - one
// thread spinning - and nothing in the log said whose loop it was. Every
// 256th call of a number samples where it came from: the user RIP (in the
// libc wrapper), the return address on the user stack (the caller - libc
// wrappers of these calls are leaf functions), the file mapped there (offset
// and file length: the library is recognisable by its length), and the task
// slot. 32 cells for the window, by (number, return address).

/// Sample cells: number + 1 (0 - empty), return address, RIP, image offset
/// of the return address, image file length, hits, last slot.
const SITES: usize = 32;
static SITE_NR: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_RET: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_RIP: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_IMG_OFF: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_IMG_LEN: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_HITS: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];
static SITE_SLOT: [AtomicU64; SITES] = [const { AtomicU64::new(0) }; SITES];

/// Sample one call of `number` (every 256th of its calls): `rip` and `rsp`
/// of the user at the `syscall` instruction.
pub fn sample(number: usize, rip: u64, rsp: u64) {
    if number >= SLOTS || CALLS[number].load(Ordering::Relaxed) % 256 != 0 {
        return;
    }
    let ret = if crate::user::space::user_can(rsp as usize, 8, crate::mm::PageFlags::READ) {
        // SAFETY: checked by the line above.
        unsafe { core::ptr::read_unaligned(rsp as *const u64) }
    } else {
        0
    };
    let key = number as u64 + 1;
    let found = (0..SITES).find(|&i| SITE_NR[i].load(Ordering::Relaxed) == key && SITE_RET[i].load(Ordering::Relaxed) == ret);
    let cell = match found {
        Some(cell) => cell,
        None => {
            let Some(cell) = (0..SITES).find(|&i| SITE_NR[i].compare_exchange(0, key, Ordering::AcqRel, Ordering::Relaxed).is_ok())
            else {
                return;
            };
            let (off, len) = crate::user::with_current(|program| program.image_at(ret as usize))
                .flatten()
                .unwrap_or((0, 0));
            SITE_RET[cell].store(ret, Ordering::Relaxed);
            SITE_RIP[cell].store(rip, Ordering::Relaxed);
            SITE_IMG_OFF[cell].store(off, Ordering::Relaxed);
            SITE_IMG_LEN[cell].store(len, Ordering::Relaxed);
            cell
        }
    };
    SITE_HITS[cell].fetch_add(1, Ordering::Relaxed);
    SITE_SLOT[cell].store(crate::sched::running_slot() as u64, Ordering::Relaxed);
}

/// A sampled call site: `(number, samples, return address, rip, image
/// offset of the return address, image file length, last task slot)`.
pub type Site = (usize, u64, u64, u64, u64, u64, usize);

/// The sampled sites of the window, most samples first - and forget them.
pub fn take_sites() -> Vec<Site> {
    let mut sites: Vec<Site> = (0..SITES)
        .filter_map(|i| {
            let key = SITE_NR[i].load(Ordering::Acquire);
            if key == 0 {
                return None;
            }
            let site = (
                key as usize - 1,
                SITE_HITS[i].swap(0, Ordering::Relaxed),
                SITE_RET[i].load(Ordering::Relaxed),
                SITE_RIP[i].load(Ordering::Relaxed),
                SITE_IMG_OFF[i].load(Ordering::Relaxed),
                SITE_IMG_LEN[i].load(Ordering::Relaxed),
                SITE_SLOT[i].load(Ordering::Relaxed) as usize,
            );
            SITE_NR[i].store(0, Ordering::Release);
            Some(site)
        })
        .collect();
    sites.sort_by(|a, b| b.1.cmp(&a.1));
    sites
}

// ---- ioctl by request (hw-log15) -------------------------------------------
//
// `ioctl` averaged 291-719 us in D2R (40-80 thread-seconds per 30 s) and only
// part of it was fence waits: per request number - count, ns, worst.

const IOCTLS: usize = 48;
static IOCTL_REQ: [AtomicU64; IOCTLS] = [const { AtomicU64::new(0) }; IOCTLS];
static IOCTL_CALLS: [AtomicU64; IOCTLS] = [const { AtomicU64::new(0) }; IOCTLS];
static IOCTL_NS: [AtomicU64; IOCTLS] = [const { AtomicU64::new(0) }; IOCTLS];
static IOCTL_WORST: [AtomicU64; IOCTLS] = [const { AtomicU64::new(0) }; IOCTLS];

/// Count one `ioctl` with request `request` that took `nanos`.
pub fn count_ioctl(request: u32, nanos: u64) {
    // 0 marks an empty cell: request 0 is counted as 1<<32.
    let key = if request == 0 { 1 << 32 } else { u64::from(request) };
    let mut cell = None;
    for i in 0..IOCTLS {
        let now = IOCTL_REQ[i].load(Ordering::Relaxed);
        if now == key {
            cell = Some(i);
            break;
        }
        if now == 0 {
            match IOCTL_REQ[i].compare_exchange(0, key, Ordering::AcqRel, Ordering::Relaxed) {
                Ok(_) => {
                    cell = Some(i);
                    break;
                }
                Err(other) if other == key => {
                    cell = Some(i);
                    break;
                }
                Err(_) => {}
            }
        }
    }
    let Some(i) = cell else { return };
    IOCTL_CALLS[i].fetch_add(1, Ordering::Relaxed);
    IOCTL_NS[i].fetch_add(nanos, Ordering::Relaxed);
    IOCTL_WORST[i].fetch_max(nanos, Ordering::Relaxed);
}

/// `ioctl`s of the window by request: `(request, calls, ns, worst ns)`, the
/// most time first - and start a new window.
pub fn take_ioctls() -> Vec<(u32, u64, u64, u64)> {
    let mut rows: Vec<(u32, u64, u64, u64)> = (0..IOCTLS)
        .filter_map(|i| {
            let key = IOCTL_REQ[i].load(Ordering::Acquire);
            if key == 0 {
                return None;
            }
            let calls = IOCTL_CALLS[i].swap(0, Ordering::Relaxed);
            let ns = IOCTL_NS[i].swap(0, Ordering::Relaxed);
            let worst = IOCTL_WORST[i].swap(0, Ordering::Relaxed);
            (calls > 0).then_some((key as u32, calls, ns, worst))
        })
        .collect();
    rows.sort_by(|a, b| b.2.cmp(&a.2));
    rows
}

/// Name of an `ioctl` request for perf-diag: DRM core and nouveau numbers,
/// dma-buf, terminal; otherwise empty.
#[must_use]
pub fn ioctl_name(request: u32) -> &'static str {
    let kind = (request >> 8) & 0xff;
    let nr = request & 0xff;
    match (kind, nr) {
        (0x64, 0x00) => "DRM_VERSION",
        (0x64, 0x09) => "GEM_CLOSE",
        (0x64, 0x0c) => "GET_CAP",
        (0x64, 0x2d) => "PRIME_HANDLE_TO_FD",
        (0x64, 0x2e) => "PRIME_FD_TO_HANDLE",
        (0x64, 0xbf) => "SYNCOBJ_CREATE",
        (0x64, 0xc0) => "SYNCOBJ_DESTROY",
        (0x64, 0xc1) => "SYNCOBJ_HANDLE_TO_FD",
        (0x64, 0xc2) => "SYNCOBJ_FD_TO_HANDLE",
        (0x64, 0xc3) => "SYNCOBJ_WAIT",
        (0x64, 0xc4) => "SYNCOBJ_RESET",
        (0x64, 0xc5) => "SYNCOBJ_SIGNAL",
        (0x64, 0xca) => "SYNCOBJ_TIMELINE_WAIT",
        (0x64, 0xcb) => "SYNCOBJ_QUERY",
        (0x64, 0xcc) => "SYNCOBJ_TRANSFER",
        (0x64, 0xcd) => "SYNCOBJ_TIMELINE_SIGNAL",
        (0x64, 0x40) => "NOUVEAU_GETPARAM",
        (0x64, 0x42) => "NOUVEAU_CHANNEL_ALLOC",
        (0x64, 0x43) => "NOUVEAU_CHANNEL_FREE",
        (0x64, 0x47) => "NOUVEAU_NVIF",
        (0x64, 0x50) => "NOUVEAU_VM_INIT",
        (0x64, 0x51) => "NOUVEAU_VM_BIND",
        (0x64, 0x52) => "NOUVEAU_EXEC",
        (0x64, 0x80) => "NOUVEAU_GEM_NEW",
        (0x64, 0x81) => "NOUVEAU_GEM_PUSHBUF",
        (0x64, 0x82) => "NOUVEAU_GEM_CPU_PREP",
        (0x64, 0x83) => "NOUVEAU_GEM_CPU_FINI",
        (0x64, 0x84) => "NOUVEAU_GEM_INFO",
        (0x62, 0x00) => "DMA_BUF_SYNC",
        (0x62, 0x02) => "DMA_BUF_EXPORT_SYNC_FILE",
        (0x62, 0x03) => "DMA_BUF_IMPORT_SYNC_FILE",
        (0x54, _) => "tty",
        (0x89, _) => "socket",
        _ => "",
    }
}

/// The longest single call since the last [`take_worst`] and its number.
static WORST_NS: AtomicU64 = AtomicU64::new(0);
static WORST_NR: AtomicU64 = AtomicU64::new(0);

/// `(number, ns)` of the longest single call since the last question (sleeps
/// included, like the totals) - and forget it.
#[must_use]
pub fn take_worst() -> Option<(usize, u64)> {
    let ns = WORST_NS.swap(0, Ordering::Relaxed);
    (ns != 0).then(|| (WORST_NR.load(Ordering::Relaxed) as usize, ns))
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

/// Число слотов счёта (номера вызовов и виды отказов страниц).
pub const COUNT: usize = SLOTS;

/// Снимок счёта без сброса — `(раз, наносекунд)` на слот (`perf-diag`
/// считает разность с прошлым снимком и `lxstat` не трогает).
pub fn snapshot(out: &mut [(u64, u64)]) {
    for (number, cell) in out.iter_mut().enumerate().take(SLOTS) {
        *cell = (CALLS[number].load(Ordering::Relaxed), NANOS[number].load(Ordering::Relaxed));
    }
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
        98 => "getrusage",
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
        326 => "copy_file_range",
        332 => "statx",
        40 => "sendfile",
        233 => "epoll_ctl",
        234 => "tgkill",
        299 => "recvmmsg",
        435 => "clone3",
        441 => "epoll_pwait2",
        449 => "futex_waitv",
        // Не вызовы — отказы страниц (`user::fault_in`).
        500 => "#pf missing",
        501 => "#pf write (cow/jit)",
        502 => "#pf exec (jit)",
        503 => "#pf shared",
        504 => "#pf page cache",
        505 => "#pf anon",
        506 => "#pf file read",
        _ => "",
    }
}
