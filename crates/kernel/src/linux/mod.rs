//! Системные вызовы программ Linux (фаза 71a): таблица перевода в родные.
//!
//! # Как сюда попадают
//!
//! Программа Linux входит в ядро инструкцией `syscall` (x86-64), родная —
//! `int 0x80`; заглушка входа складывает тот же кадр, что у `int 0x80`, и
//! `dispatch` отдаёт его сюда. Номер — в RAX, аргументы — RDI, RSI, RDX, R10,
//! R8, R9; ответ — в RAX, ошибка — отрицательный `errno` **Linux**.
//!
//! # Как устроен перевод
//!
//! Где смысл совпадает, вызов уходит в родной обработчик
//! ([`crate::user::syscall::handle`]) с теми же аргументами, а его код ошибки
//! переводится в `errno` Linux ([`errno`]). Где у Linux своя раскладка
//! структуры (`stat`, `utsname`, `termios`), ответ перекладывается здесь.
//! Чего нет вовсе — `-ENOSYS`, и номер вызова печатается в журнал один раз на
//! программу: по этому списку и растёт таблица. Притворяться, что вызов
//! удался, можно только там, где у Linux он и правда ничего не меняет для
//! однопоточной программы без сигналов (`rt_sigaction`, `sigaltstack`), — и это
//! названо у каждого такого вызова.
//!
//! Родные программы сюда не попадают: `syscall` у программы, не помеченной
//! как программа Linux, — `-ENOSYS`.

use core::mem::size_of;

use user_abi::{
    CLOCK_MONOTONIC, CLOCK_REALTIME, MAP_LAZY, SYS_CLOCK, SYS_CLOSE, SYS_EXIT, SYS_FSTAT, SYS_FUTEX_WAIT,
    SYS_FUTEX_WAKE, SYS_GETGID, SYS_GETPID, SYS_GETUID, SYS_ISATTY, SYS_MMAP, SYS_MPROTECT, SYS_MUNMAP,
    SYS_NANOSLEEP, SYS_OPEN, SYS_RANDOM, SYS_READ, SYS_SEEK, SYS_SET_TLS, SYS_STAT, SYS_WRITE, SYS_YIELD,
};

use crate::kprintln;
use crate::mm::PageFlags;
use crate::user::space;

/// Номера вызовов Linux x86-64 (`arch/x86/entry/syscalls/syscall_64.tbl`).
mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const OPEN: usize = 2;
    pub const CLOSE: usize = 3;
    pub const STAT: usize = 4;
    pub const FSTAT: usize = 5;
    pub const LSTAT: usize = 6;
    pub const LSEEK: usize = 8;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const BRK: usize = 12;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const IOCTL: usize = 16;
    pub const WRITEV: usize = 20;
    pub const SCHED_YIELD: usize = 24;
    pub const MADVISE: usize = 28;
    pub const NANOSLEEP: usize = 35;
    pub const GETPID: usize = 39;
    pub const EXIT: usize = 60;
    pub const UNAME: usize = 63;
    pub const GETUID: usize = 102;
    pub const GETGID: usize = 104;
    pub const GETEUID: usize = 107;
    pub const GETEGID: usize = 108;
    pub const GETPPID: usize = 110;
    pub const SIGALTSTACK: usize = 131;
    pub const ARCH_PRCTL: usize = 158;
    pub const GETTID: usize = 186;
    pub const FUTEX: usize = 202;
    pub const SCHED_GETAFFINITY: usize = 204;
    pub const SET_TID_ADDRESS: usize = 218;
    pub const CLOCK_GETTIME: usize = 228;
    pub const EXIT_GROUP: usize = 231;
    pub const OPENAT: usize = 257;
    pub const NEWFSTATAT: usize = 262;
    pub const SET_ROBUST_LIST: usize = 273;
    pub const PRLIMIT64: usize = 302;
    pub const GETRANDOM: usize = 318;
    pub const RSEQ: usize = 334;
}

/// `errno` Linux, которые здесь отдаются.
mod e {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const ESRCH: i64 = 3;
    pub const EIO: i64 = 5;
    pub const EBADF: i64 = 9;
    pub const EAGAIN: i64 = 11;
    pub const ENOMEM: i64 = 12;
    pub const EACCES: i64 = 13;
    pub const EFAULT: i64 = 14;
    pub const EEXIST: i64 = 17;
    pub const ENOTDIR: i64 = 20;
    pub const EINVAL: i64 = 22;
    pub const EMFILE: i64 = 24;
    pub const ENOTTY: i64 = 25;
    pub const EPIPE: i64 = 32;
    pub const ENOSYS: i64 = 38;
    pub const ENOTEMPTY: i64 = 39;
    pub const ENOTSOCK: i64 = 88;
    pub const ENETDOWN: i64 = 100;
    pub const ECONNRESET: i64 = 104;
    pub const ETIMEDOUT: i64 = 110;
    pub const ECONNREFUSED: i64 = 111;
    pub const EINPROGRESS: i64 = 115;
}

/// Перевести ответ родного обработчика: неотрицательное — как есть,
/// отрицательное — родной код ошибки в `-errno` Linux.
fn errno(native: i64) -> i64 {
    if native >= 0 {
        return native;
    }
    let code = match native {
        user_abi::ERR_NO_SYSCALL => e::ENOSYS,
        user_abi::ERR_BAD_ADDRESS => e::EFAULT,
        user_abi::ERR_NOT_FOUND | user_abi::ERR_NO_FILESYSTEM => e::ENOENT,
        user_abi::ERR_PERMISSION => e::EACCES,
        user_abi::ERR_BAD_FD => e::EBADF,
        user_abi::ERR_TOO_MANY_FILES => e::EMFILE,
        user_abi::ERR_IO => e::EIO,
        user_abi::ERR_EXISTS => e::EEXIST,
        user_abi::ERR_NOT_EMPTY => e::ENOTEMPTY,
        // Родной «места нет» отвечает и на нехватку кадров, и на полный диск;
        // для `mmap` и `brk` — а это главный путь сюда — это память.
        user_abi::ERR_NO_SPACE | user_abi::ERR_LIMIT => e::ENOMEM,
        user_abi::ERR_AGAIN | user_abi::ERR_TOO_MANY_TASKS => e::EAGAIN,
        user_abi::ERR_NO_TASK | user_abi::ERR_NO_PROGRAM => e::ESRCH,
        user_abi::ERR_NO_NETWORK => e::ENETDOWN,
        user_abi::ERR_BAD_SOCKET => e::ENOTSOCK,
        user_abi::ERR_BROKEN_PIPE => e::EPIPE,
        user_abi::ERR_UPDATE_REFUSED => e::EPERM,
        user_abi::ERR_CONN_RESET => e::ECONNRESET,
        user_abi::ERR_CONN_REFUSED => e::ECONNREFUSED,
        user_abi::ERR_IN_PROGRESS => e::EINPROGRESS,
        _ => e::EINVAL,
    };
    -code
}

/// Позвать родной обработчик.
///
/// # Safety
///
/// Как у [`crate::user::syscall::handle`]: из ловушки третьего кольца.
unsafe fn native(number: usize, a0: usize, a1: usize, a2: usize) -> i64 {
    // SAFETY: контракт функции.
    unsafe { crate::user::syscall::handle(number, a0, a1, a2) }
}

/// Разобрать вызов программы Linux.
///
/// # Safety
///
/// Только из обработчика ловушки, пришедшей из третьего кольца: `exit` не
/// возвращается.
pub unsafe fn syscall(number: usize, args: [usize; 6]) -> i64 {
    let [a0, a1, a2, a3, a4, _a5] = args;
    match crate::user::with_current(|program| program.linux) {
        Some(true) => {}
        // `syscall` из родной программы: таблицы Linux ей не положено.
        _ => return -e::ENOSYS,
    }
    // SAFETY (для всех `native` ниже): контракт функции.
    unsafe {
        match number {
            nr::READ => errno(native(SYS_READ, a0, a1, a2)),
            nr::WRITE => errno(native(SYS_WRITE, a0, a1, a2)),
            nr::WRITEV => writev(a0, a1, a2),
            nr::CLOSE => errno(native(SYS_CLOSE, a0, 0, 0)),
            nr::OPEN => open_at(AT_FDCWD, a0, a1),
            nr::OPENAT => open_at(a0, a1, a2),
            nr::STAT | nr::LSTAT => stat_path(AT_FDCWD, a0, a1),
            nr::NEWFSTATAT => {
                // `AT_EMPTY_PATH` с пустым путём — это `fstat` дескриптора.
                if a3 & AT_EMPTY_PATH != 0 && c_string_len(a1) == Some(0) {
                    fstat(a0, a2)
                } else {
                    stat_path(a0, a1, a2)
                }
            }
            nr::FSTAT => fstat(a0, a1),
            nr::LSEEK => errno(native(SYS_SEEK, a0, a1, a2)),
            nr::MMAP => mmap(a0, a1, a2, a3, a4, args[5]),
            nr::MPROTECT => errno(native(SYS_MPROTECT, a0, a1, a2)),
            nr::MUNMAP => errno(native(SYS_MUNMAP, a0, a1, 0)),
            // `brk` отвечает новым концом, а отказ — прежним; ошибки у него нет.
            nr::BRK => crate::user::linux_brk(a0) as i64,
            // Сигналов в системе пока нет (фаза 71c): обработчик запоминать
            // некому, и ни один сигнал не придёт. Отказ здесь уронил бы glibc
            // на старте без всякой пользы.
            nr::RT_SIGACTION | nr::RT_SIGPROCMASK | nr::SIGALTSTACK => 0,
            nr::IOCTL => ioctl(a0, a1, a2),
            nr::SCHED_YIELD => errno(native(SYS_YIELD, 0, 0, 0)),
            // Совет о памяти — совет: не принять его можно всегда.
            nr::MADVISE => 0,
            nr::NANOSLEEP => nanosleep(a0),
            nr::GETPID | nr::GETTID => errno(native(SYS_GETPID, 0, 0, 0)),
            nr::GETPPID => 1,
            nr::GETUID | nr::GETEUID => errno(native(SYS_GETUID, 0, 0, 0)),
            nr::GETGID | nr::GETEGID => errno(native(SYS_GETGID, 0, 0, 0)),
            nr::UNAME => uname(a0),
            nr::ARCH_PRCTL => arch_prctl(a0, a1),
            nr::FUTEX => futex(a0, a1, a2, a3),
            nr::SCHED_GETAFFINITY => affinity(a1, a2),
            // Поток один (фаза 71c добавит `clone`): адрес очистки при выходе
            // потока помнить незачем, ответ — номер потока.
            nr::SET_TID_ADDRESS => errno(native(SYS_GETPID, 0, 0, 0)),
            nr::SET_ROBUST_LIST => 0,
            nr::CLOCK_GETTIME => clock_gettime(a0, a1),
            nr::PRLIMIT64 => prlimit(a1, a2, a3),
            nr::GETRANDOM => errno(native(SYS_RANDOM, a0, a1, 0)),
            nr::EXIT | nr::EXIT_GROUP => native(SYS_EXIT, a0, 0, 0),
            // `rseq` — оптимизация glibc; `ENOSYS` она понимает и обходится без.
            nr::RSEQ => -e::ENOSYS,
            _ => unknown(number),
        }
    }
}

/// Неизвестный вызов: `-ENOSYS` и строка в журнал — по ней видно, чего
/// программе не хватило.
fn unknown(number: usize) -> i64 {
    let path = crate::user::with_current(|program| program.path.clone()).unwrap_or_default();
    kprintln!("  linux       : {path}: system call {number} is not implemented");
    -e::ENOSYS
}

const AT_FDCWD: usize = -100isize as usize;
const AT_EMPTY_PATH: usize = 0x1000;

/// Длина строки C в памяти программы — не больше [`MAX_PATH`]; `None` — адрес
/// не читается или строка длиннее.
fn c_string_len(ptr: usize) -> Option<usize> {
    const MAX_PATH: usize = 255;
    for len in 0..=MAX_PATH {
        if !space::user_can(ptr + len, 1, PageFlags::READ) {
            return None;
        }
        // SAFETY: байт проверен на чтение строкой выше; пространство
        // программы активно — мы в её системном вызове.
        if unsafe { core::ptr::read_volatile((ptr + len) as *const u8) } == 0 {
            return Some(len);
        }
    }
    None
}

/// Путь от каталога `dir`: пока только абсолютный и от текущего (`AT_FDCWD`) —
/// текущий каталог у программы Linux один, корень. `None` — непонятный путь.
fn path_of(dir: usize, ptr: usize) -> Result<usize, i64> {
    let len = c_string_len(ptr).ok_or(-e::EFAULT)?;
    if len == 0 {
        return Err(-e::ENOENT);
    }
    // SAFETY: байт проверен в `c_string_len`.
    let first = unsafe { core::ptr::read_volatile(ptr as *const u8) };
    if first != b'/' && dir != AT_FDCWD {
        // Путь от дескриптора каталога — фаза 71b.
        return Err(-e::ENOTDIR);
    }
    if first != b'/' {
        // Без текущего каталога относительный путь — от корня; родной `open`
        // примет его только абсолютным.
        return Err(-e::ENOENT);
    }
    Ok(len)
}

/// `openat(dir, path, flags)`.
unsafe fn open_at(dir: usize, ptr: usize, flags: usize) -> i64 {
    const O_ACCMODE: usize = 3;
    const O_CREAT: usize = 0o100;
    const O_TRUNC: usize = 0o1000;
    let len = match path_of(dir, ptr) {
        Ok(len) => len,
        Err(err) => return err,
    };
    let mut native_flags = 0;
    if flags & O_ACCMODE != 0 {
        native_flags |= user_abi::O_WRITE;
    }
    if flags & O_CREAT != 0 {
        native_flags |= user_abi::O_CREATE;
    }
    if flags & O_TRUNC != 0 {
        native_flags |= user_abi::O_TRUNC;
    }
    // SAFETY: из ловушки третьего кольца — контракт `syscall`.
    errno(unsafe { native(SYS_OPEN, ptr, len, native_flags) })
}

/// `struct stat` Linux x86-64 — 144 байта.
#[repr(C)]
#[derive(Default)]
struct LinuxStat {
    dev: u64,
    ino: u64,
    nlink: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    _pad0: u32,
    rdev: u64,
    size: i64,
    blksize: i64,
    blocks: i64,
    atime: [i64; 2],
    mtime: [i64; 2],
    ctime: [i64; 2],
    _reserved: [i64; 3],
}

/// Родной ответ `stat`/`fstat` (`user_abi::Stat`) — он короче линуксового и
/// пишется в тот же буфер программы, после чего перекладывается.
fn relay_stat(out: usize, code: i64) -> i64 {
    if code < 0 {
        return errno(code);
    }
    // SAFETY: родной обработчик только что записал сюда `Stat`, проверив
    // адрес на выравнивание и запись; буфер Linux длиннее.
    let native = unsafe { core::ptr::read(out as *const user_abi::Stat) };
    let kind = match native.kind {
        user_abi::KIND_DIRECTORY => 0o040_000,
        user_abi::KIND_PIPE => 0o010_000,
        user_abi::KIND_TERMINAL => 0o020_000,
        user_abi::KIND_SOCKET => 0o140_000,
        _ => 0o100_000,
    };
    let value = LinuxStat {
        // Номер узла — не ноль: glibc и утилиты сравнивают пары (dev, ino), и
        // нули у всех сделали бы все файлы одним. Настоящих номеров ядро не
        // отдаёт; размер в нём — лучше, чем ничего, пока их нет (фаза 71b).
        ino: native.size.max(1),
        nlink: 1,
        mode: kind | (native.mode & 0o7777),
        uid: native.uid,
        gid: native.gid,
        size: native.size as i64,
        blksize: 4096,
        blocks: native.size.div_ceil(512) as i64,
        ..LinuxStat::default()
    };
    // SAFETY: буфер проверен вызывающим на запись `LinuxStat` целиком.
    unsafe { core::ptr::write_unaligned(out as *mut LinuxStat, value) };
    0
}

unsafe fn fstat(fd: usize, out: usize) -> i64 {
    if out % 8 != 0 || !space::user_can(out, size_of::<LinuxStat>(), PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // SAFETY: контракт `syscall`.
    relay_stat(out, unsafe { native(SYS_FSTAT, fd, out, 0) })
}

unsafe fn stat_path(dir: usize, ptr: usize, out: usize) -> i64 {
    if out % 8 != 0 || !space::user_can(out, size_of::<LinuxStat>(), PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let len = match path_of(dir, ptr) {
        Ok(len) => len,
        Err(err) => return err,
    };
    // SAFETY: контракт `syscall`.
    relay_stat(out, unsafe { native(SYS_STAT, ptr, len, out) })
}

/// `writev` — по куску родным `write`.
unsafe fn writev(fd: usize, iov: usize, count: usize) -> i64 {
    if count > 1024 || !space::user_can(iov, count * 16, PageFlags::READ) {
        return -e::EINVAL;
    }
    let mut total = 0i64;
    for index in 0..count {
        // SAFETY: массив `iovec` проверен на чтение выше.
        let (base, len) = unsafe {
            let entry = (iov + index * 16) as *const usize;
            (core::ptr::read_unaligned(entry), core::ptr::read_unaligned(entry.add(1)))
        };
        if len == 0 {
            continue;
        }
        // SAFETY: контракт `syscall`.
        let wrote = unsafe { native(SYS_WRITE, fd, base, len) };
        if wrote < 0 {
            return if total > 0 { total } else { errno(wrote) };
        }
        total += wrote;
        if (wrote as usize) < len {
            break;
        }
    }
    total
}

/// `mmap`: безымянная память и файл только на чтение — то, что умеет ядро.
/// Адрес — подсказка, и ею пренебрегают; `MAP_FIXED` — фаза 71b.
unsafe fn mmap(_addr: usize, len: usize, prot: usize, flags: usize, fd: usize, offset: usize) -> i64 {
    const MAP_FIXED: usize = 0x10;
    const MAP_ANONYMOUS: usize = 0x20;
    if flags & MAP_FIXED != 0 {
        return -e::EINVAL;
    }
    if flags & MAP_ANONYMOUS != 0 {
        // SAFETY: контракт `syscall`.
        let base = unsafe { native(SYS_MMAP, len, MAP_LAZY, 0) };
        if base < 0 {
            return errno(base);
        }
        if prot != user_abi::PROT_READ | user_abi::PROT_WRITE {
            // SAFETY: контракт `syscall`.
            let done = unsafe { native(SYS_MPROTECT, base as usize, len, prot) };
            if done < 0 {
                // SAFETY: контракт `syscall`.
                unsafe { native(SYS_MUNMAP, base as usize, len, 0) };
                return errno(done);
            }
        }
        return base;
    }
    if prot & user_abi::PROT_WRITE != 0 {
        // Запись в отображение файла — фаза 71b (копия при записи).
        return -e::EACCES;
    }
    // SAFETY: контракт `syscall`.
    errno(unsafe { native(user_abi::SYS_MMAP_FILE, fd, offset, len) })
}

/// `ioctl`: терминалу — `TCGETS` и размер окна, остальным — «не терминал».
unsafe fn ioctl(fd: usize, request: usize, arg: usize) -> i64 {
    const TCGETS: usize = 0x5401;
    const TIOCGWINSZ: usize = 0x5413;
    // SAFETY: контракт `syscall`.
    let tty = unsafe { native(SYS_ISATTY, fd, 0, 0) };
    if tty < 0 {
        return errno(tty);
    }
    if tty == 0 {
        return -e::ENOTTY;
    }
    match request {
        TCGETS => {
            // `struct termios` — 60 байт. Нули — «сырой» режим без флагов;
            // программе этого хватает, чтобы узнать, что перед ней терминал.
            if !space::user_can(arg, 60, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::write_bytes(arg as *mut u8, 0, 60) };
            0
        }
        TIOCGWINSZ => {
            if !space::user_can(arg, 8, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            // `struct winsize`: строки, столбцы, пиксели — 25×80 терминала.
            let size: [u16; 4] = [25, 80, 0, 0];
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::write_unaligned(arg as *mut [u16; 4], size) };
            0
        }
        _ => -e::ENOTTY,
    }
}

/// `nanosleep(req, rem)`: `timespec` Linux — секунды и наносекунды по 64 бита.
unsafe fn nanosleep(req: usize) -> i64 {
    if !space::user_can(req, 16, PageFlags::READ) {
        return -e::EFAULT;
    }
    // SAFETY: проверено строкой выше.
    let [seconds, nanos] = unsafe { core::ptr::read_unaligned(req as *const [u64; 2]) };
    // SAFETY: контракт `syscall`.
    errno(unsafe { native(SYS_NANOSLEEP, seconds as usize, nanos as usize, 0) })
}

/// `clock_gettime`: родной `Timespec` (секунды `u64`, наносекунды `u32` и
/// нулевой `u32`) побайтно совпадает с `timespec` Linux x86-64, и номера часов
/// у обоих одни: 0 — время суток, 1 — монотонные.
unsafe fn clock_gettime(clock: usize, out: usize) -> i64 {
    /// `CLOCK_MONOTONIC_RAW`, `_COARSE`, `BOOTTIME` — те же монотонные.
    const MONOTONIC_LIKE: [usize; 3] = [4, 6, 7];
    /// `CLOCK_REALTIME_COARSE`.
    const REALTIME_COARSE: usize = 5;
    let clock = match clock {
        CLOCK_REALTIME | REALTIME_COARSE => CLOCK_REALTIME,
        CLOCK_MONOTONIC => CLOCK_MONOTONIC,
        other if MONOTONIC_LIKE.contains(&other) => CLOCK_MONOTONIC,
        _ => return -e::EINVAL,
    };
    // SAFETY: контракт `syscall`.
    errno(unsafe { native(SYS_CLOCK, clock, out, 0) })
}

/// `uname`: шесть полей по 65 байт.
fn uname(out: usize) -> i64 {
    const FIELD: usize = 65;
    if !space::user_can(out, FIELD * 6, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // Версия ядра — такая, какую glibc примет за достаточно новую: она
    // сверяет её со своим минимумом и иначе отказывается работать.
    let fields: [&[u8]; 6] = [b"Linux", b"fossai", b"6.1.0-fossai", b"#1 FossAi", b"x86_64", b"(none)"];
    for (index, field) in fields.iter().enumerate() {
        let at = out + index * FIELD;
        // SAFETY: весь буфер проверен на запись выше.
        unsafe {
            core::ptr::write_bytes(at as *mut u8, 0, FIELD);
            core::ptr::copy_nonoverlapping(field.as_ptr(), at as *mut u8, field.len());
        }
    }
    0
}

/// `arch_prctl`: база FS — хранилище потока glibc.
unsafe fn arch_prctl(code: usize, addr: usize) -> i64 {
    const ARCH_SET_GS: usize = 0x1001;
    const ARCH_SET_FS: usize = 0x1002;
    const ARCH_GET_FS: usize = 0x1003;
    const ARCH_GET_GS: usize = 0x1004;
    match code {
        // SAFETY: контракт `syscall`.
        ARCH_SET_FS => errno(unsafe { native(SYS_SET_TLS, addr, 0, 0) }),
        ARCH_GET_FS => {
            if !space::user_can(addr, 8, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            let base = crate::arch::user_tls();
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::write_unaligned(addr as *mut u64, base as u64) };
            0
        }
        // GS ядро отдаёт заглушке входа `syscall` (`swapgs`); своей базы GS
        // программе Linux пока не положено.
        ARCH_SET_GS | ARCH_GET_GS => -e::EINVAL,
        _ => -e::EINVAL,
    }
}

/// `futex`: ожидание и пробуждение — то, на чём стоят замки glibc.
unsafe fn futex(addr: usize, op: usize, value: usize, timeout: usize) -> i64 {
    const FUTEX_WAIT: usize = 0;
    const FUTEX_WAKE: usize = 1;
    const FUTEX_PRIVATE: usize = 128;
    const FUTEX_CLOCK_REALTIME: usize = 256;
    match op & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME) {
        FUTEX_WAIT => {
            // Срок Linux — `timespec`; родной — миллисекунды, ноль — без срока.
            let ms = if timeout == 0 {
                0
            } else {
                if !space::user_can(timeout, 16, PageFlags::READ) {
                    return -e::EFAULT;
                }
                // SAFETY: проверено строкой выше.
                let [seconds, nanos] = unsafe { core::ptr::read_unaligned(timeout as *const [u64; 2]) };
                (seconds.saturating_mul(1000).saturating_add(nanos.div_ceil(1_000_000))).max(1) as usize
            };
            // SAFETY: контракт `syscall`.
            let code = unsafe { native(SYS_FUTEX_WAIT, addr, value, ms) };
            match code {
                // Родные «значение уже другое» и «срок вышел» — положительные
                // ответы; у Linux это `EAGAIN` и `ETIMEDOUT`.
                user_abi::FUTEX_CHANGED => -e::EAGAIN,
                user_abi::FUTEX_TIMED_OUT => -e::ETIMEDOUT,
                code => errno(code),
            }
        }
        // SAFETY: контракт `syscall`.
        FUTEX_WAKE => errno(unsafe { native(SYS_FUTEX_WAKE, addr, value, 0) }),
        _ => -e::ENOSYS,
    }
}

/// `sched_getaffinity`: маска из всех процессоров машины.
fn affinity(len: usize, out: usize) -> i64 {
    let cpus = crate::smp::online().max(1);
    let bytes = cpus.div_ceil(8).max(8);
    if len < bytes || !space::user_can(out, bytes, PageFlags::WRITE) {
        return -e::EINVAL;
    }
    for index in 0..bytes {
        let bits = cpus.saturating_sub(index * 8).min(8);
        let byte = if bits == 8 { 0xFF } else { (1u8 << bits) - 1 };
        // SAFETY: буфер проверен на запись выше.
        unsafe { core::ptr::write((out + index) as *mut u8, byte) };
    }
    bytes as i64
}

/// `prlimit64(pid, resource, new, old)`: на чтение — «без предела», кроме
/// стека (8 МиБ, как у Linux по умолчанию) и числа файлов; на запись —
/// принимается молча: пределов, которые можно было бы поставить, у задачи нет.
fn prlimit(resource: usize, _new: usize, old: usize) -> i64 {
    const RLIMIT_STACK: usize = 3;
    const RLIMIT_NOFILE: usize = 7;
    const INFINITY: u64 = u64::MAX;
    if old != 0 {
        if !space::user_can(old, 16, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        let value = match resource {
            RLIMIT_STACK => [8 << 20, INFINITY],
            RLIMIT_NOFILE => [1024, 1024],
            _ => [INFINITY, INFINITY],
        };
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::write_unaligned(old as *mut [u64; 2], value) };
    }
    0
}
