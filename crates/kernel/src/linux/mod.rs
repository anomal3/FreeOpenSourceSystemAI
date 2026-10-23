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

use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;

use user_abi::{
    CLOCK_MONOTONIC, CLOCK_REALTIME, SYS_CLOCK, SYS_CLOSE, SYS_FUTEX_WAIT, SYS_FUTEX_WAKE, SYS_GETGID,
    SYS_GETPID, SYS_GETUID, SYS_ISATTY, SYS_MPROTECT, SYS_MUNMAP, SYS_NANOSLEEP, SYS_RANDOM, SYS_READ, SYS_SEEK,
    SYS_SET_TLS, SYS_WRITE, SYS_YIELD,
};

mod poll;
mod fsops;
mod ipc;
pub mod recent;
mod net;
mod process;
mod signal;
mod special;
mod thread;

pub use process::{process_ended, register_child};

use crate::kprintln;
use crate::mm::PageFlags;
use crate::user::files::FdKind;
use crate::user::space;
use crate::vfs::NodeKind;
use crate::vfs::perm::Access;

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
    pub const PREAD64: usize = 17;
    pub const ACCESS: usize = 21;
    pub const DUP: usize = 32;
    pub const DUP2: usize = 33;
    pub const WAIT4: usize = 61;
    pub const KILL: usize = 62;
    pub const FCNTL: usize = 72;
    pub const GETCWD: usize = 79;
    pub const CHDIR: usize = 80;
    pub const READLINK: usize = 89;
    pub const UMASK: usize = 95;
    pub const GETTIMEOFDAY: usize = 96;
    pub const GETRUSAGE: usize = 98;
    pub const SYSINFO: usize = 99;
    pub const TIMES: usize = 100;
    pub const SETPGID: usize = 109;
    pub const GETPGRP: usize = 111;
    pub const SETSID: usize = 112;
    pub const GETGROUPS: usize = 115;
    pub const GETPGID: usize = 121;
    pub const GETSID: usize = 124;
    pub const TIME: usize = 201;
    pub const GETDENTS64: usize = 217;
    pub const FADVISE64: usize = 221;
    pub const TGKILL: usize = 234;
    pub const READLINKAT: usize = 267;
    pub const FACCESSAT: usize = 269;
    pub const DUP3: usize = 292;
    pub const STATX: usize = 332;
    pub const STATFS: usize = 137;
    pub const FSTATFS: usize = 138;
    pub const FACCESSAT2: usize = 439;
    pub const POLL: usize = 7;
    pub const RT_SIGRETURN: usize = 15;
    pub const PIPE: usize = 22;
    pub const CLONE: usize = 56;
    pub const GETPRIORITY: usize = 140;
    pub const SETPRIORITY: usize = 141;
    pub const SCHED_GETSCHEDULER: usize = 145;
    pub const PRCTL: usize = 157;
    pub const TKILL: usize = 200;
    pub const SCHED_SETAFFINITY: usize = 203;
    pub const CLOCK_NANOSLEEP: usize = 230;
    pub const PPOLL: usize = 271;
    pub const PIPE2: usize = 293;
    pub const MEMBARRIER: usize = 324;
    pub const CLONE3: usize = 435;
    pub const FORK: usize = 57;
    pub const VFORK: usize = 58;
    pub const EXECVE: usize = 59;
    pub const READV: usize = 19;
    pub const SOCKET: usize = 41;
    pub const CONNECT: usize = 42;
    pub const ACCEPT: usize = 43;
    pub const SENDTO: usize = 44;
    pub const RECVFROM: usize = 45;
    pub const SENDMSG: usize = 46;
    pub const RECVMSG: usize = 47;
    pub const SHUTDOWN: usize = 48;
    pub const BIND: usize = 49;
    pub const LISTEN: usize = 50;
    pub const GETSOCKNAME: usize = 51;
    pub const GETPEERNAME: usize = 52;
    pub const SOCKETPAIR: usize = 53;
    pub const SETSOCKOPT: usize = 54;
    pub const GETSOCKOPT: usize = 55;
    pub const FTRUNCATE: usize = 77;
    pub const UNLINK: usize = 87;
    pub const EPOLL_CREATE: usize = 213;
    pub const EPOLL_WAIT: usize = 232;
    pub const EPOLL_CTL: usize = 233;
    pub const UNLINKAT: usize = 263;
    pub const EPOLL_PWAIT: usize = 281;
    pub const TIMERFD_CREATE: usize = 283;
    pub const EVENTFD: usize = 284;
    pub const TIMERFD_SETTIME: usize = 286;
    pub const TIMERFD_GETTIME: usize = 287;
    pub const ACCEPT4: usize = 288;
    pub const EVENTFD2: usize = 290;
    pub const EPOLL_CREATE1: usize = 291;
    pub const MEMFD_CREATE: usize = 319;
    pub const SENDMMSG: usize = 307;
    pub const PWRITE64: usize = 18;
    pub const FLOCK: usize = 73;
    pub const FSYNC: usize = 74;
    pub const FDATASYNC: usize = 75;
    pub const RENAME: usize = 82;
    pub const MKDIR: usize = 83;
    pub const RMDIR: usize = 84;
    pub const LINK: usize = 86;
    pub const SYMLINK: usize = 88;
    pub const CHMOD: usize = 90;
    pub const FCHMOD: usize = 91;
    pub const FCHOWN: usize = 93;
    pub const GETRESUID: usize = 118;
    pub const GETRESGID: usize = 120;
    pub const SCHED_SETPARAM: usize = 142;
    pub const SCHED_GETPARAM: usize = 143;
    pub const SCHED_SETSCHEDULER: usize = 144;
    pub const SCHED_GET_PRIORITY_MAX: usize = 146;
    pub const SCHED_GET_PRIORITY_MIN: usize = 147;
    pub const CLOCK_GETRES: usize = 229;
    pub const MKDIRAT: usize = 258;
    pub const RENAMEAT: usize = 264;
    pub const LINKAT: usize = 265;
    pub const SYMLINKAT: usize = 266;
    pub const FCHMODAT: usize = 268;
    pub const SYNC_FILE_RANGE: usize = 277;
    pub const UTIMENSAT: usize = 280;
    pub const FALLOCATE: usize = 285;
    pub const RENAMEAT2: usize = 316;
}

/// `errno` Linux, которые здесь отдаются.
mod e {
    pub const EPERM: i64 = 1;
    pub const ENOENT: i64 = 2;
    pub const EINTR: i64 = 4;
    pub const ECHILD: i64 = 10;
    pub const EISDIR: i64 = 21;
    pub const ERANGE: i64 = 34;
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
pub unsafe fn syscall(frame: &mut crate::arch::TrapFrame) {
    match crate::user::with_current(|program| program.linux) {
        Some(true) => {}
        // `syscall` из родной программы: таблицы Linux ей не положено.
        _ => {
            frame.rax = (-e::ENOSYS) as u64;
            return;
        }
    }
    let number = frame.rax as usize;
    // `rt_sigreturn` возвращает не ответ, а прерванные регистры целиком.
    if number == nr::RT_SIGRETURN {
        signal::sigreturn(frame);
        return;
    }
    let args = [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9].map(|value| value as usize);
    // SAFETY: контракт функции.
    let result = unsafe { dispatch(number, args, frame) };
    recent::record(number, args, result);
    frame.rax = result as u64;
    // Пришедшие сигналы доставляются на выходе из вызова (фаза 71c).
    signal::deliver(frame);
}

/// Разобрать вызов по номеру.
///
/// # Safety
///
/// Как у [`syscall`].
unsafe fn dispatch(number: usize, args: [usize; 6], frame: &mut crate::arch::TrapFrame) -> i64 {
    let [a0, a1, a2, a3, a4, a5] = args;
    // SAFETY (для всех `native` ниже): контракт функции.
    unsafe {
        match number {
            // Объекты слоя Linux (фаза 71e) читаются и пишутся здесь, всё
            // остальное — родным путём.
            nr::READ => ipc::object_read(a0, a1, a2).unwrap_or_else(|| errno(native(SYS_READ, a0, a1, a2))),
            nr::WRITE => ipc::object_write(a0, a1, a2).unwrap_or_else(|| errno(native(SYS_WRITE, a0, a1, a2))),
            nr::WRITEV => ipc::object_writev(a0, a1, a2).unwrap_or_else(|| writev(a0, a1, a2)),
            nr::READV => ipc::object_readv(a0, a1, a2).unwrap_or_else(|| readv(a0, a1, a2)),
            // `AF_INET` — родной стек (фаза 71f), остальное — `AF_UNIX`.
            nr::SOCKET if a0 == 2 => net::socket(a1),
            nr::SOCKET => ipc::socket(a0, a1),
            nr::SOCKETPAIR => ipc::socketpair(a0, a1, a3),
            nr::BIND => net::bind(a0, a1, a2).unwrap_or_else(|| ipc::bind(a0, a1, a2)),
            nr::LISTEN => net::listen(a0).unwrap_or_else(|| ipc::listen(a0)),
            nr::CONNECT => net::connect(a0, a1, a2).unwrap_or_else(|| ipc::connect(a0, a1, a2)),
            nr::ACCEPT => net::accept(a0, a1, a2, 0).unwrap_or_else(|| ipc::accept(a0, a1, a2, 0)),
            nr::ACCEPT4 => net::accept(a0, a1, a2, a3).unwrap_or_else(|| ipc::accept(a0, a1, a2, a3)),
            nr::SHUTDOWN => net::shutdown(a0, a1).unwrap_or_else(|| ipc::shutdown(a0, a1)),
            nr::GETSOCKNAME => net::sockname(a0, a1, a2, false).unwrap_or_else(|| ipc::sockname(a0, a1, a2, false)),
            nr::GETPEERNAME => net::sockname(a0, a1, a2, true).unwrap_or_else(|| ipc::sockname(a0, a1, a2, true)),
            nr::GETSOCKOPT => net::getsockopt(a0, a1, a2, a3, a4).unwrap_or_else(|| ipc::getsockopt(a0, a1, a2, a3, a4)),
            nr::SETSOCKOPT if net::is_inet(a0) => 0,
            nr::SETSOCKOPT => match ipc::getsockopt(a0, 1, 3, 0, 0) {
                // Проверка, что это сокет; самих настроек у сокета нет.
                err if err == -e::EBADF || err == -e::ENOTSOCK => err,
                _ => 0,
            },
            nr::SENDMSG => net::sendmsg(a0, a1, a2).unwrap_or_else(|| ipc::sendmsg(a0, a1, a2)),
            nr::SENDMMSG => net::sendmmsg(a0, a1, a2, a3),
            nr::RECVMSG => net::recvmsg(a0, a1, a2).unwrap_or_else(|| ipc::recvmsg(a0, a1, a2)),
            nr::SENDTO => net::sendto(a0, a1, a2, a3, a4, args[5]).unwrap_or_else(|| ipc::sendto(a0, a1, a2, a3)),
            nr::RECVFROM => net::recvfrom(a0, a1, a2, a3, a4, args[5]).unwrap_or_else(|| ipc::recvfrom(a0, a1, a2, a3, a4, args[5])),
            nr::EVENTFD => ipc::eventfd(a0, 0),
            nr::EVENTFD2 => ipc::eventfd(a0, a1),
            nr::TIMERFD_CREATE => ipc::timerfd_create(a0, a1),
            nr::TIMERFD_SETTIME => ipc::timerfd_settime(a0, a1, a2, a3),
            nr::TIMERFD_GETTIME => ipc::timerfd_gettime(a0, a1),
            nr::EPOLL_CREATE => ipc::epoll_create(0),
            nr::EPOLL_CREATE1 => ipc::epoll_create(a0),
            nr::EPOLL_CTL => ipc::epoll_ctl(a0, a1, a2, a3),
            nr::EPOLL_WAIT | nr::EPOLL_PWAIT => ipc::epoll_wait(a0, a1, a2, a3 as i32 as i64),
            nr::MEMFD_CREATE => ipc::memfd_create(a0, a1),
            nr::FTRUNCATE => ftruncate(a0, a1),
            // Фаза 71h — то, чего потребовал Chromium.
            nr::PWRITE64 => fsops::pwrite(a0, a1, a2, a3),
            nr::MKDIR => fsops::mkdir(AT_FDCWD, a0, a1),
            nr::MKDIRAT => fsops::mkdir(a0, a1, a2),
            nr::RENAME => fsops::rename(AT_FDCWD, a0, AT_FDCWD, a1, 0),
            nr::RENAMEAT => fsops::rename(a0, a1, a2, a3, 0),
            nr::RENAMEAT2 => fsops::rename(a0, a1, a2, a3, a4),
            nr::RMDIR => fsops::rmdir(a0),
            nr::FALLOCATE => fsops::fallocate(a0, a1, a2, a3),
            nr::CLOCK_GETRES => fsops::clock_getres(a0, a1),
            nr::GETRESUID => fsops::getres(a0, a1, a2, false),
            nr::GETRESGID => fsops::getres(a0, a1, a2, true),
            // Жёстких и символических ссылок файловая система не знает —
            // ответ, как у FAT: «нельзя».
            nr::LINK | nr::LINKAT | nr::SYMLINK | nr::SYMLINKAT => -e::EPERM,
            // Данные пишутся сразу, блокировок между процессами одного
            // пользователя нет, прав и времени файла программа Linux не меняет:
            // согласиться — честный ответ.
            nr::FSYNC | nr::FDATASYNC | nr::SYNC_FILE_RANGE | nr::FLOCK => 0,
            nr::CHMOD | nr::FCHMOD | nr::FCHMODAT | nr::FCHOWN | nr::UTIMENSAT => 0,
            nr::SCHED_GET_PRIORITY_MAX | nr::SCHED_GET_PRIORITY_MIN => 0,
            // Приоритет `SCHED_OTHER` — ноль; поменять его нечем.
            nr::SCHED_GETPARAM => match ipc::put(a1, 0i32) {
                Ok(()) => 0,
                Err(err) => err,
            },
            nr::SCHED_SETPARAM | nr::SCHED_SETSCHEDULER => 0,
            nr::UNLINK => unlink(AT_FDCWD, a0),
            nr::UNLINKAT => unlink(a0, a1),
            nr::CLOSE => {
                crate::user::with_current(|program| program.linux_dirs.retain(|(fd, _)| *fd != a0));
                errno(native(SYS_CLOSE, a0, 0, 0))
            }
            nr::OPEN => open_at(AT_FDCWD, a0, a1),
            nr::OPENAT => open_at(a0, a1, a2),
            nr::STAT | nr::LSTAT => stat_path(AT_FDCWD, a0, a1),
            nr::PREAD64 => pread(a0, a1, a2, a3),
            nr::ACCESS => access(AT_FDCWD, a0, a1),
            nr::FACCESSAT | nr::FACCESSAT2 => access(a0, a1, a2),
            nr::GETDENTS64 => getdents(a0, a1, a2),
            nr::GETCWD => getcwd(a0, a1),
            nr::CHDIR => chdir(a0),
            nr::READLINK => readlink(AT_FDCWD, a0, a1, a2),
            nr::READLINKAT => readlink(a0, a1, a2, a3),
            nr::FCNTL => fcntl(a0, a1, a2),
            nr::DUP => errno(native(user_abi::SYS_DUP, a0, user_abi::DUP_ANY, 0)),
            nr::DUP2 => dup3(a0, a1, 0, true),
            nr::DUP3 => dup3(a0, a1, a2, false),
            nr::WAIT4 => process::wait4(a0, a1, a2, a3),
            nr::FORK => thread::clone(frame, SIGCHLD, 0, 0, 0, 0),
            nr::VFORK => thread::clone(frame, CLONE_VM | CLONE_VFORK | SIGCHLD, 0, 0, 0, 0),
            nr::EXECVE => process::execve(frame, a0, a1, a2),
            nr::UMASK => 0o022,
            nr::GETTIMEOFDAY => gettimeofday(a0),
            nr::TIME => time(a0),
            nr::GETRUSAGE => zero_out(a1, 144),
            nr::SYSINFO => sysinfo(a0),
            nr::TIMES => times(a0),
            // Группы процессов и сеансы: процесс один и сам себе группа.
            nr::SETPGID => 0,
            nr::GETPGRP | nr::SETSID | nr::GETPGID | nr::GETSID => errno(native(SYS_GETPID, 0, 0, 0)),
            nr::GETGROUPS => 0,
            // `statx` glibc пробует первым и, получив `ENOSYS`, берёт
            // `newfstatat` — его здесь и хватает.
            nr::STATX => -e::ENOSYS,
            // Сведения о томе (фаза 71h): Chromium по `statvfs("/dev/shm")`
            // решает, где держать разделяемую память. libselinux ищет так
            // `selinuxfs` — по типу тома его не находит и работает без него.
            nr::STATFS => fsops::statfs(Some(a0), 0, a1),
            nr::FSTATFS => fsops::statfs(None, a0, a1),
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
            nr::RT_SIGACTION => signal::sigaction(a0, a1, a2, a3),
            nr::RT_SIGPROCMASK => signal::sigprocmask(a0, a1, a2, a3),
            nr::SIGALTSTACK => signal::sigaltstack(a0, a1),
            nr::KILL => signal::kill(a0, a1),
            nr::TKILL => signal::tgkill(0, a0, a1),
            nr::TGKILL => signal::tgkill(a0, a1, a2),
            nr::CLONE => thread::clone(frame, a0, a1, a2, a3, a4),
            nr::CLONE3 => thread::clone3(frame, a0, a1),
            nr::POLL => poll::poll(a0, a1, a2 as i32 as i64),
            nr::PPOLL => poll::ppoll(a0, a1, a2),
            nr::PIPE => poll::pipe(a0, 0),
            nr::PIPE2 => poll::pipe(a0, a1),
            nr::PRCTL => thread::prctl(a0, a1),
            nr::SCHED_SETAFFINITY | nr::SETPRIORITY => 0,
            nr::GETPRIORITY => 20,
            nr::SCHED_GETSCHEDULER => 0,
            nr::CLOCK_NANOSLEEP => clock_nanosleep(a0, a1, a2),
            nr::MEMBARRIER => -e::ENOSYS,
            nr::IOCTL => ioctl(a0, a1, a2),
            nr::SCHED_YIELD => errno(native(SYS_YIELD, 0, 0, 0)),
            // Совет о памяти или о чтении файла — совет: не принять его можно
            // всегда.
            nr::MADVISE | nr::FADVISE64 => 0,
            nr::NANOSLEEP => nanosleep(a0),
            nr::GETPID => crate::user::with_current(|program| program.linux_pid).unwrap_or(1) as i64,
            nr::GETTID => errno(native(SYS_GETPID, 0, 0, 0)),
            nr::GETPPID => process::getppid(),
            nr::GETUID | nr::GETEUID => errno(native(SYS_GETUID, 0, 0, 0)),
            nr::GETGID | nr::GETEGID => errno(native(SYS_GETGID, 0, 0, 0)),
            nr::UNAME => uname(a0),
            nr::ARCH_PRCTL => arch_prctl(a0, a1),
            nr::FUTEX => futex(a0, a1, a2, a3, a4, a5),
            nr::SCHED_GETAFFINITY => affinity(a1, a2),
            // Адрес, который ядро обнулит при выходе потока (фаза 71c).
            nr::SET_TID_ADDRESS => {
                crate::user::LINUX_CLEAR_TID[crate::sched::current_slot()].store(a0, core::sync::atomic::Ordering::Relaxed);
                errno(native(SYS_GETPID, 0, 0, 0))
            }
            nr::SET_ROBUST_LIST => 0,
            nr::CLOCK_GETTIME => clock_gettime(a0, a1),
            nr::PRLIMIT64 => prlimit(a1, a2, a3),
            nr::GETRANDOM => errno(native(SYS_RANDOM, a0, a1, 0)),
            nr::EXIT => thread::exit(a0),
            nr::EXIT_GROUP => thread::exit_group(a0),
            // `rseq` — оптимизация glibc; `ENOSYS` она понимает и обходится без.
            nr::RSEQ => -e::ENOSYS,
            _ => unknown(number),
        }
    }
}

const SIGCHLD: usize = 17;
const CLONE_VM: usize = 0x100;
const CLONE_VFORK: usize = 0x4000;
const O_CLOEXEC: usize = 0o2_000_000;

/// `dup2`/`dup3`: копия на заданный номер; `dup3` с `O_CLOEXEC` ставит
/// `FD_CLOEXEC` копии. `dup2` сам в себя — проверка, что номер открыт;
/// `dup3` сам в себя — `EINVAL`.
fn dup3(fd: usize, to: usize, flags: usize, dup2: bool) -> i64 {
    if fd == to {
        if !dup2 {
            return -e::EINVAL;
        }
        return match crate::user::with_current(|program| program.files.is_open(fd)) {
            Some(true) => fd as i64,
            _ => -e::EBADF,
        };
    }
    let result = crate::user::with_current(|program| {
        let fd = program.files.dup(fd, to)?;
        program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
        Ok(fd)
    });
    match result {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => file_errno(err),
        None => -e::ESRCH,
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

/// Самый длинный путь, который здесь принимается (`PATH_MAX` Linux — 4096).
const PATH_MAX: usize = 4095;

/// Длина строки C в памяти программы — не больше [`PATH_MAX`]; `None` — адрес
/// не читается или строка длиннее.
fn c_string_len(ptr: usize) -> Option<usize> {
    for len in 0..=PATH_MAX {
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

/// Путь из памяти программы — строкой ядра.
fn user_string(ptr: usize) -> Result<String, i64> {
    let len = c_string_len(ptr).ok_or(-e::EFAULT)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(len).map_err(|_| -e::ENOMEM)?;
    for at in 0..len {
        // SAFETY: строка проверена на чтение в `c_string_len`.
        bytes.push(unsafe { core::ptr::read_volatile((ptr + at) as *const u8) });
    }
    String::from_utf8(bytes).map_err(|_| -e::ENOENT)
}

/// Свернуть `.` и `..` в абсолютном пути. Ссылок в системе нет, и путь по
/// тексту — это путь по диску.
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = String::with_capacity(path.len() + 1);
    for part in &parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Путь `ptr` от каталога `dir` — так, как его видит программа Linux:
/// абсолютный, свёрнутый. Относительный — от её текущего каталога; от
/// дескриптора каталога — пока отказ (`ENOTDIR`): программы, которым это
/// нужно, придут со своими сценариями.
fn linux_path(dir: usize, ptr: usize) -> Result<String, i64> {
    let path = user_string(ptr)?;
    if path.is_empty() {
        return Err(-e::ENOENT);
    }
    if path.starts_with('/') {
        return Ok(normalize(&path));
    }
    // От дескриптора каталога — по пути, под которым каталог открыт (фаза
    // 71c); от текущего — по текущему каталогу.
    let base = if dir == AT_FDCWD {
        crate::user::with_current(|program| program.linux_cwd.clone()).unwrap_or_else(|| String::from("/"))
    } else {
        match crate::user::with_current(|program| {
            program.linux_dirs.iter().find(|(fd, _)| *fd == dir).map(|(_, path)| path.clone())
        }) {
            Some(Some(path)) => path,
            _ => return Err(-e::ENOTDIR),
        }
    };
    let mut joined = base;
    joined.push('/');
    joined.push_str(&path);
    Ok(normalize(&joined))
}

fn file_errno(err: crate::user::files::FileError) -> i64 {
    errno(crate::user::syscall::errno(err))
}

fn vfs_errno(err: crate::vfs::VfsError) -> i64 {
    errno(crate::user::syscall::vfs_errno(err))
}

/// `openat(dir, path, flags)`.
fn open_at(dir: usize, ptr: usize, flags: usize) -> i64 {
    const O_ACCMODE: usize = 3;
    const O_CREAT: usize = 0o100;
    const O_EXCL: usize = 0o200;
    const O_TRUNC: usize = 0o1000;
    const O_DIRECTORY: usize = 0o200_000;
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if let Some(answer) = ipc::open_shm(&linux, flags) {
        return answer;
    }
    if let Some((node, writable)) = special::node(&linux) {
        let directory = node.metadata().kind == NodeKind::Directory;
        return match crate::user::with_current(|program| {
            let fd = program.files.install_node(node, writable)?;
            program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
            // Каталог `/proc` (фаза 71h) — путь для `openat` от него.
            if directory {
                program.linux_dirs.retain(|(other, _)| *other != fd);
                program.linux_dirs.push((fd, linux.clone()));
            }
            Ok(fd)
        }) {
            Some(Ok(fd)) => fd as i64,
            Some(Err(err)) => file_errno(err),
            None => -e::ESRCH,
        };
    }
    // `/proc/self/exe` — файл самой программы.
    let linux = if linux == "/proc/self/exe" {
        crate::user::with_current(|program| String::from(crate::linux_path::shown(&program.path))).unwrap_or(linux)
    } else {
        linux
    };
    let path = crate::linux_path::resolve(&linux, crate::user::credentials());
    let cred = crate::user::credentials();
    if flags & O_CREAT != 0 && flags & O_EXCL != 0 && matches!(crate::fs::resolve_as(cred, &path, Access::NONE), Some(Ok(_))) {
        return -e::EEXIST;
    }
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
    let fd = match crate::user::with_current(|program| program.files.open(cred, &path, native_flags)) {
        Some(Ok(fd)) => fd,
        Some(Err(err)) => return file_errno(err),
        None => return -e::ESRCH,
    };
    let kind = crate::user::with_current(|program| program.files.describe(fd).map(|info| info.kind));
    let directory = matches!(kind, Some(Ok(FdKind::Directory)));
    if flags & O_DIRECTORY != 0 && !directory {
        crate::user::with_current(|program| program.files.close(fd));
        return -e::ENOTDIR;
    }
    if flags & O_CLOEXEC != 0 {
        crate::user::with_current(|program| program.files.set_cloexec(fd, true));
    }
    if directory {
        crate::user::with_current(|program| {
            program.linux_dirs.retain(|(other, _)| *other != fd);
            program.linux_dirs.push((fd, linux));
        });
    }
    fd as i64
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

/// Биты вида файла в `st_mode`.
const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFIFO: u32 = 0o010_000;
const S_IFCHR: u32 = 0o020_000;
const S_IFSOCK: u32 = 0o140_000;

/// Записать `struct stat` в память программы.
fn put_stat(out: usize, kind: u32, mode: u16, uid: u32, gid: u32, size: u64, mtime: u32, ino: u64) -> i64 {
    if out % 8 != 0 || !space::user_can(out, size_of::<LinuxStat>(), PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let time = [i64::from(mtime), 0];
    let value = LinuxStat {
        // Номер узла — не ноль: утилиты сравнивают пары (dev, ino), и нули у
        // всех сделали бы все файлы одним. Настоящих номеров ядро не отдаёт;
        // вместо них — свёртка пути (`ino_of`).
        dev: 1,
        ino: ino.max(1),
        nlink: if kind == S_IFDIR { 2 } else { 1 },
        mode: kind | (u32::from(mode) & 0o7777),
        uid,
        gid,
        size: size as i64,
        blksize: 4096,
        blocks: size.div_ceil(512) as i64,
        atime: time,
        mtime: time,
        ctime: time,
        ..LinuxStat::default()
    };
    // SAFETY: буфер проверен на запись `LinuxStat` целиком.
    unsafe { core::ptr::write(out as *mut LinuxStat, value) };
    0
}

/// «Номер узла» по пути: FNV-1a. Одинаков у одного файла при каждом вызове,
/// разный у разных — большего утилитам от него не нужно.
fn ino_of(path: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

fn stat_path(dir: usize, ptr: usize, out: usize) -> i64 {
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    // `/proc/self/fd/N` — ссылка на открытое; `stat` идёт по ней, то есть
    // это `fstat` дескриптора (фаза 71h: песочница Chromium так проверяет
    // открытые дескрипторы). `/proc/self/task/<tid>` — каталог потока.
    if let Some(number) = linux.strip_prefix("/proc/self/fd/") {
        return match number.parse::<usize>() {
            Ok(fd) => fstat(fd, out),
            Err(_) => -e::ENOENT,
        };
    }
    if let Some(tid) = linux.strip_prefix("/proc/self/task/") {
        // Только живой поток этого процесса: песочница Chromium, остановив
        // поток, ждёт, когда его каталог исчезнет.
        return match tid.parse::<u32>() {
            Ok(tid) if crate::user::thread_alive(tid) => put_stat(out, S_IFDIR, 0o555, 0, 0, 0, 0, ino_of(&linux)),
            _ => -e::ENOENT,
        };
    }
    if ipc::shm_exists(&linux) {
        return put_stat(out, S_IFREG, 0o600, 0, 0, 0, 0, ino_of(&linux));
    }
    if special::exists(&linux) {
        let kind = if linux.starts_with("/dev/") { S_IFCHR } else { S_IFREG };
        return put_stat(out, kind, 0o666, 0, 0, 0, 0, ino_of(&linux));
    }
    if special::is_directory(&linux) || matches!(linux.as_str(), "/dev" | "/sys" | "/dev/shm") {
        let done = put_stat(out, S_IFDIR, 0o555, 0, 0, 0, 0, ino_of(&linux));
        // `/proc/self/task`: ссылок — «.», «..» и по одной на поток. Песочница
        // Chromium считает потоки именно так (фаза 71h).
        if done == 0 && linux == "/proc/self/task" {
            let links = 2 + crate::user::thread_count() as u64;
            // SAFETY: `put_stat` только что записал сюда `struct stat`
            // целиком; `st_nlink` — третье слово.
            unsafe { core::ptr::write_unaligned((out + 16) as *mut u64, links) };
        }
        return done;
    }
    let path = crate::linux_path::resolve(&linux, crate::user::credentials());
    let node = match crate::fs::resolve_as(crate::user::credentials(), &path, Access::NONE) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return vfs_errno(err),
        None => return -e::ENOENT,
    };
    let meta = node.metadata();
    let kind = match meta.kind {
        NodeKind::Directory => S_IFDIR,
        NodeKind::File => S_IFREG,
    };
    put_stat(out, kind, meta.mode, meta.uid, meta.gid, meta.size, meta.mtime, ino_of(&path))
}

fn fstat(fd: usize, out: usize) -> i64 {
    // Объект слоя Linux (фаза 71e): сокет или «анонимный узел».
    if let Some(Some((object, _))) = crate::user::with_current(|program| program.files.object(fd)) {
        let ino = alloc::sync::Arc::as_ptr(&object) as *const () as usize as u64;
        return put_stat(out, object.mode(), 0o600, 0, 0, 0, 0, ino);
    }
    // Стандартный поток — на каком бы номере он ни стоял (фаза 71d): канал
    // запускающего или терминал.
    let standard = crate::user::with_current(|program| {
        program.files.std_target(fd).map(|stream| {
            let piped = match stream {
                0 => program.stdin.is_some(),
                1 => program.stdout.is_some(),
                _ => false,
            };
            (stream, piped)
        })
    });
    if let Some(Some((stream, piped))) = standard {
        return if piped {
            put_stat(out, S_IFIFO, 0o600, 0, 0, 0, 0, 0x100 + stream as u64)
        } else {
            put_stat(out, S_IFCHR, 0o620, 0, 0, 0, 0, 0x100 + stream as u64)
        };
    }
    match crate::user::with_current(|program| program.files.describe(fd)) {
        Some(Ok(info)) => {
            let kind = match info.kind {
                FdKind::File => S_IFREG,
                FdKind::Directory => S_IFDIR,
                FdKind::Pipe => S_IFIFO,
                FdKind::Socket => S_IFSOCK,
                // Объекты отвечают выше; сюда не доходят.
                FdKind::Object => 0,
            };
            // Номер узла — по самому открытому файлу, а не по номеру
            // дескриптора: `ld.so` узнаёт уже загруженную библиотеку по паре
            // (dev, ino), и `libc.so.6`, открытая под тем же номером 3, что до
            // неё `libselinux.so.1`, сошла бы за неё (первый прогон `ls`, 71b).
            let ino = match crate::user::with_current(|program| program.files.node(fd)) {
                // Разделяемая память — по самому объекту памяти (фаза 71h):
                // Chromium открывает `/dev/shm/имя` дважды и сверяет номера.
                Some(Ok(node)) => match node.shared_memory() {
                    Some(mem) => alloc::sync::Arc::as_ptr(&mem) as usize as u64,
                    None => alloc::sync::Arc::as_ptr(&node) as *const () as usize as u64,
                },
                _ => 0x1000 + fd as u64,
            };
            put_stat(out, kind, info.mode, info.uid, info.gid, info.size, 0, ino)
        }
        Some(Err(err)) => file_errno(err),
        None => -e::ESRCH,
    }
}

/// `pread64(fd, buf, count, offset)` — чтение с места, не двигая позицию.
fn pread(fd: usize, buf: usize, count: usize, offset: usize) -> i64 {
    const CHUNK: usize = 64 * 1024;
    if !space::user_can(buf, count, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let node = match crate::user::with_current(|program| program.files.node(fd)) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return file_errno(err),
        None => return -e::ESRCH,
    };
    if node.metadata().kind == NodeKind::Directory {
        return -e::EISDIR;
    }
    let mut chunk = Vec::new();
    if chunk.try_reserve_exact(CHUNK.min(count)).is_err() {
        return -e::ENOMEM;
    }
    chunk.resize(CHUNK.min(count), 0);
    let mut done = 0;
    while done < count {
        let want = (count - done).min(CHUNK);
        let got = match node.read_at((offset + done) as u64, &mut chunk[..want]) {
            Ok(got) => got,
            Err(err) if done == 0 => return vfs_errno(err),
            Err(_) => break,
        };
        // SAFETY: весь буфер программы проверен на запись выше.
        unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), (buf + done) as *mut u8, got) };
        done += got;
        if got < want {
            break;
        }
    }
    done as i64
}

/// `access(path, mode)`: `F_OK` — есть ли путь; `R_OK`, `W_OK`, `X_OK` —
/// по праву каждое.
fn access(dir: usize, ptr: usize, mode: usize) -> i64 {
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if special::exists(&linux) {
        return 0;
    }
    let path = crate::linux_path::resolve(&linux, crate::user::credentials());
    let cred = crate::user::credentials();
    let mut wants = [Access::NONE; 4];
    let mut count = 1;
    for (bit, want) in [(4, Access::READ), (2, Access::WRITE), (1, Access::SEARCH)] {
        if mode & bit != 0 {
            wants[count] = want;
            count += 1;
        }
    }
    for want in &wants[..count] {
        match crate::fs::resolve_as(cred, &path, *want) {
            Some(Ok(_)) => {}
            Some(Err(err)) => return vfs_errno(err),
            None => return -e::ENOENT,
        }
    }
    0
}

/// `getdents64(fd, buf, count)`: записи `linux_dirent64` по одной из
/// перечисления каталога, пока помещается ещё одна самая длинная.
fn getdents(fd: usize, buf: usize, count: usize) -> i64 {
    /// Самая длинная запись: 19 байт заголовка, имя до 255 и ноль, по 8.
    const LONGEST: usize = 280;
    if !space::user_can(buf, count, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    if count < LONGEST {
        return -e::EINVAL;
    }
    let mut written = 0;
    while count - written >= LONGEST {
        let entry = match crate::user::with_current(|program| program.files.next_entry(fd)) {
            Some(Ok(Some(entry))) => entry,
            Some(Ok(None)) => break,
            Some(Err(err)) if written == 0 => return file_errno(err),
            _ => break,
        };
        let name = entry.name.as_bytes();
        let name = &name[..name.len().min(255)];
        let reclen = (19 + name.len() + 1).next_multiple_of(8);
        let kind: u8 = match entry.kind {
            NodeKind::Directory => 4,
            NodeKind::File => 8,
        };
        let at = buf + written;
        // SAFETY: место под запись проверено (`LONGEST` в остатке буфера).
        unsafe {
            core::ptr::write_bytes(at as *mut u8, 0, reclen);
            core::ptr::write_unaligned(at as *mut u64, ino_of(&entry.name));
            core::ptr::write_unaligned((at + 8) as *mut i64, (written + reclen) as i64);
            core::ptr::write_unaligned((at + 16) as *mut u16, reclen as u16);
            core::ptr::write((at + 18) as *mut u8, kind);
            core::ptr::copy_nonoverlapping(name.as_ptr(), (at + 19) as *mut u8, name.len());
        }
        written += reclen;
    }
    written as i64
}

/// `getcwd(buf, size)`: ответ — длина с завершающим нулём.
fn getcwd(buf: usize, size: usize) -> i64 {
    let cwd = crate::user::with_current(|program| program.linux_cwd.clone()).unwrap_or_else(|| String::from("/"));
    if size < cwd.len() + 1 {
        return -e::ERANGE;
    }
    if !space::user_can(buf, cwd.len() + 1, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // SAFETY: проверено строкой выше.
    unsafe {
        core::ptr::copy_nonoverlapping(cwd.as_ptr(), buf as *mut u8, cwd.len());
        core::ptr::write((buf + cwd.len()) as *mut u8, 0);
    }
    (cwd.len() + 1) as i64
}

fn chdir(ptr: usize) -> i64 {
    let path = match linux_path(AT_FDCWD, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let full = crate::linux_path::resolve(&path, crate::user::credentials());
    match crate::fs::resolve_as(crate::user::credentials(), &full, Access::SEARCH) {
        Some(Ok(node)) if node.metadata().kind == NodeKind::Directory => {
            crate::user::with_current(|program| program.linux_cwd = path);
            0
        }
        Some(Ok(_)) => -e::ENOTDIR,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `readlink`: ссылок в системе нет, кроме `/proc/self/exe` — путь программы.
fn readlink(dir: usize, ptr: usize, buf: usize, size: usize) -> i64 {
    let path = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if path == "/proc/self/exe" {
        let exe = crate::user::with_current(|program| String::from(crate::linux_path::shown(&program.path)))
            .unwrap_or_default();
        let len = exe.len().min(size);
        if !space::user_can(buf, len, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::copy_nonoverlapping(exe.as_ptr(), buf as *mut u8, len) };
        return len as i64;
    }
    let full = crate::linux_path::resolve(&path, crate::user::credentials());
    match crate::fs::resolve_as(crate::user::credentials(), &full, Access::NONE) {
        // Есть, но не ссылка — так и отвечает Linux.
        Some(Ok(_)) => -e::EINVAL,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `fcntl`: копия дескриптора, `FD_CLOEXEC` (фаза 71d) и «не ждать».
unsafe fn fcntl(fd: usize, cmd: usize, arg: usize) -> i64 {
    const F_DUPFD: usize = 0;
    const F_GETFD: usize = 1;
    const F_SETFD: usize = 2;
    const F_GETFL: usize = 3;
    const F_SETFL: usize = 4;
    const F_DUPFD_CLOEXEC: usize = 1030;
    const F_ADD_SEALS: usize = 1033;
    const F_GET_SEALS: usize = 1034;
    const FD_CLOEXEC: usize = 1;
    const O_NONBLOCK: usize = 0o4000;
    let result = crate::user::with_current(|program| {
        let files = &mut program.files;
        match cmd {
            F_DUPFD | F_DUPFD_CLOEXEC => {
                let copy = files.dup_from(fd, arg)?;
                files.set_cloexec(copy, cmd == F_DUPFD_CLOEXEC);
                Ok(copy as i64)
            }
            F_GETFD => Ok(if files.cloexec(fd)? { FD_CLOEXEC as i64 } else { 0 }),
            F_SETFD => {
                files.cloexec(fd)?;
                files.set_cloexec(fd, arg & FD_CLOEXEC != 0);
                Ok(0)
            }
            // Чтение и запись — `O_RDWR`; у ждущего канала — без `O_NONBLOCK`.
            // Режим доступа — как открыт (фаза 71h: Chromium проверяет, что
            // дескриптор «только на чтение» разделяемой памяти и правда такой).
            F_GETFL => {
                files.cloexec(fd)?;
                let nonblock = files.describe(fd).is_ok_and(|info| info.kind == FdKind::Pipe) && files.nonblock(fd)?;
                let access = files.access_mode(fd)? as i64;
                Ok(if nonblock { access | O_NONBLOCK as i64 } else { access })
            }
            // Печати `memfd`: принимаются и не держатся — запрет, которого
            // никто не нарушает, ничего не стоит не проверять.
            F_ADD_SEALS => files.cloexec(fd).map(|_| 0),
            F_GET_SEALS => files.cloexec(fd).map(|_| 0),
            F_SETFL => {
                files.cloexec(fd)?;
                files.set_nonblock(fd, arg & O_NONBLOCK != 0)?;
                Ok(0)
            }
            _ => Ok(-e::EINVAL),
        }
    });
    match result {
        Some(Ok(answer)) => answer,
        Some(Err(err)) => file_errno(err),
        None => -e::ESRCH,
    }
}

fn zero_out(ptr: usize, len: usize) -> i64 {
    if !space::user_can(ptr, len, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::write_bytes(ptr as *mut u8, 0, len) };
    0
}

fn gettimeofday(out: usize) -> i64 {
    if out == 0 {
        return 0;
    }
    if !space::user_can(out, 16, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let seconds = crate::time::now_unix().unwrap_or(0);
    let micros = (crate::time::uptime_ns() % 1_000_000_000) / 1000;
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::write_unaligned(out as *mut [u64; 2], [seconds, micros]) };
    0
}

fn time(out: usize) -> i64 {
    let seconds = crate::time::now_unix().unwrap_or(0);
    if out != 0 {
        if !space::user_can(out, 8, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::write_unaligned(out as *mut u64, seconds) };
    }
    seconds as i64
}

/// `sysinfo`: время работы и память машины.
fn sysinfo(out: usize) -> i64 {
    const SIZE: usize = 112;
    if zero_out(out, SIZE) != 0 {
        return -e::EFAULT;
    }
    let frames = crate::mm::frame::stats();
    let uptime = crate::time::uptime_ms() / 1000;
    // SAFETY: буфер проверен и обнулён `zero_out`; смещения — по `struct
    // sysinfo` x86-64: uptime 0, totalram 32, freeram 40, procs 80,
    // mem_unit 104.
    unsafe {
        core::ptr::write_unaligned(out as *mut u64, uptime);
        core::ptr::write_unaligned((out + 32) as *mut u64, (frames.total * crate::mm::PAGE_SIZE) as u64);
        core::ptr::write_unaligned((out + 40) as *mut u64, (frames.free * crate::mm::PAGE_SIZE) as u64);
        core::ptr::write_unaligned((out + 80) as *mut u16, 1);
        core::ptr::write_unaligned((out + 104) as *mut u32, 1);
    }
    0
}

/// `times`: процессорного времени по процессу ядро не считает — нули; ответ —
/// время работы в тиках по 10 мс (`AT_CLKTCK` = 100).
fn times(out: usize) -> i64 {
    if out != 0 && zero_out(out, 32) != 0 {
        return -e::EFAULT;
    }
    (crate::time::uptime_ms() / 10) as i64
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

/// `mmap`: безымянная память или кусок файла с любыми правами, кроме записи
/// и исполнения сразу; `MAP_FIXED` — ровно по адресу (фаза 71b). Общая
/// запись в файл (`MAP_SHARED` с `PROT_WRITE`) — отказ: в файл отображение не
/// пишет.
fn mmap(addr: usize, len: usize, prot: usize, flags: usize, fd: usize, offset: usize) -> i64 {
    // `int fd` — младшие 32 бита регистра: старшие компилятор вправе оставить
    // любыми (Chromium кладёт туда соседнее значение, фаза 71h).
    let fd = fd as u32 as usize;
    let flags = flags as u32 as usize;
    let prot = prot as u32 as usize;
    const MAP_SHARED: usize = 0x01;
    const MAP_FIXED: usize = 0x10;
    const MAP_ANONYMOUS: usize = 0x20;
    const MAP_FIXED_NOREPLACE: usize = 0x10_0000;
    let fixed = flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0;
    let shared_flag = flags & MAP_SHARED != 0;
    let mut shared = None;
    let file = if flags & MAP_ANONYMOUS != 0 {
        // Общая безымянная (фаза 71e): видна и ребёнку после `fork`.
        if shared_flag {
            shared = Some((crate::user::shm::SharedMem::new(len as u64), 0));
        }
        None
    } else {
        let node = match crate::user::with_current(|program| program.files.node(fd)) {
            Some(Ok(node)) => node,
            Some(Err(err)) => return file_errno(err),
            None => return -e::ESRCH,
        };
        // `memfd` и `/dev/shm` — общими кадрами (фаза 71e).
        match node.shared_memory() {
            Some(mem) if shared_flag => {
                shared = Some((mem, offset as u64));
                None
            }
            _ => {
                if shared_flag && prot & user_abi::PROT_WRITE != 0 {
                    return -e::EACCES;
                }
                Some((node, offset as u64))
            }
        }
    };
    match crate::user::with_current(|program| program.linux_mmap(addr, len, prot, fixed, file, shared)) {
        Some(Ok(base)) => base as i64,
        Some(Err(err)) => errno(crate::user::syscall::mmap_errno(err)),
        None => -e::ESRCH,
    }
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
            // `struct termios` **ядра** Linux — 36 байт: четыре слова флагов,
            // `c_line` и 19 управляющих символов. У glibc своя, на 60, и
            // перекладывает она в неё сама: записать 60 значило бы затереть её
            // буфер на стеке вместе с канарейкой (так и было в первом прогоне
            // 71b — «stack smashing detected» у `echo`). Нули — «сырой» режим
            // без флагов; узнать, что перед ней терминал, программе хватает.
            const KERNEL_TERMIOS: usize = 36;
            if !space::user_can(arg, KERNEL_TERMIOS, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::write_bytes(arg as *mut u8, 0, KERNEL_TERMIOS) };
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
    /// `CLOCK_PROCESS_CPUTIME_ID`, `CLOCK_THREAD_CPUTIME_ID`.
    const CPUTIME: [usize; 2] = [2, 3];
    let clock = match clock {
        CLOCK_REALTIME | REALTIME_COARSE => CLOCK_REALTIME,
        CLOCK_MONOTONIC => CLOCK_MONOTONIC,
        other if MONOTONIC_LIKE.contains(&other) => CLOCK_MONOTONIC,
        // Процессорное время (фаза 71h; Chromium меряет им свои задачи) —
        // своего потока; часы другого потока (`pthread_getcpuclockid`,
        // отрицательный номер) и процесса — тоже его: точнее ядро не считает.
        other if CPUTIME.contains(&other) || (other as i32) < 0 => {
            if !space::user_can(out, 16, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            let ms = crate::sched::cpu_ms(crate::sched::current()).unwrap_or(0);
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::write_unaligned(out as *mut [u64; 2], [ms / 1000, (ms % 1000) * 1_000_000]) };
            return 0;
        }
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

/// `futex`: ожидание и пробуждение — то, на чём стоят замки, условные
/// переменные и `pthread_join` glibc.
///
/// `WAIT_BITSET` и `WAKE_BITSET` — как `WAIT` и `WAKE` без маски: лишнее
/// пробуждение законно, код вокруг `futex` перепроверяет условие сам. Срок
/// `WAIT_BITSET` — **абсолютный** (по монотонным часам или, с
/// `FUTEX_CLOCK_REALTIME`, по времени суток). `REQUEUE` и `CMP_REQUEUE`
/// будят и тех, кого просили переложить: перекладывать очередь ядро не умеет,
/// а разбуженный перепроверит и уснёт снова.
unsafe fn futex(addr: usize, op: usize, value: usize, timeout: usize, addr2: usize, value3: usize) -> i64 {
    const FUTEX_WAIT: usize = 0;
    const FUTEX_WAKE: usize = 1;
    const FUTEX_REQUEUE: usize = 3;
    const FUTEX_CMP_REQUEUE: usize = 4;
    const FUTEX_WAKE_OP: usize = 5;
    const FUTEX_WAIT_BITSET: usize = 9;
    const FUTEX_WAKE_BITSET: usize = 10;
    const FUTEX_PRIVATE: usize = 128;
    const FUTEX_CLOCK_REALTIME: usize = 256;
    let command = op & !(FUTEX_PRIVATE | FUTEX_CLOCK_REALTIME);
    match command {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            // Срок Linux — `timespec`; родной — миллисекунды, ноль — без срока.
            let ms = if timeout == 0 {
                0
            } else {
                if !space::user_can(timeout, 16, PageFlags::READ) {
                    return -e::EFAULT;
                }
                // SAFETY: проверено строкой выше.
                let [seconds, nanos] = unsafe { core::ptr::read_unaligned(timeout as *const [u64; 2]) };
                let wanted = u128::from(seconds) * 1_000_000_000 + u128::from(nanos);
                let left = if command == FUTEX_WAIT_BITSET {
                    let now = if op & FUTEX_CLOCK_REALTIME != 0 {
                        u128::from(crate::time::now_unix().unwrap_or(0)) * 1_000_000_000
                            + u128::from(crate::time::uptime_ns() % 1_000_000_000)
                    } else {
                        u128::from(crate::time::uptime_ns())
                    };
                    if wanted <= now {
                        return -e::ETIMEDOUT;
                    }
                    wanted - now
                } else {
                    wanted
                };
                (left.div_ceil(1_000_000) as usize).max(1)
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
        FUTEX_WAKE | FUTEX_WAKE_BITSET => errno(unsafe { native(SYS_FUTEX_WAKE, addr, value, 0) }),
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            if command == FUTEX_CMP_REQUEUE {
                if !space::user_can(addr, 4, PageFlags::READ) {
                    return -e::EFAULT;
                }
                // SAFETY: проверено строкой выше.
                if unsafe { core::ptr::read_volatile(addr as *const u32) } as usize != value3 {
                    return -e::EAGAIN;
                }
            }
            // SAFETY: контракт `syscall`.
            let woken = unsafe { native(SYS_FUTEX_WAKE, addr, usize::MAX, 0) };
            // SAFETY: контракт `syscall`.
            let moved = unsafe { native(SYS_FUTEX_WAKE, addr2, usize::MAX, 0) };
            errno(woken.max(0) + moved.max(0))
        }
        FUTEX_WAKE_OP => {
            // `*addr2 = op(*addr2)`, разбудить на `addr` и — если сравнение
            // сошлось — на `addr2`. Здесь будятся оба: лишнее пробуждение законно.
            let _ = (timeout, value3);
            // SAFETY: контракт `syscall`.
            let woken = unsafe { native(SYS_FUTEX_WAKE, addr, value, 0) };
            // SAFETY: контракт `syscall`.
            let other = unsafe { native(SYS_FUTEX_WAKE, addr2, usize::MAX, 0) };
            errno(woken.max(0) + other.max(0))
        }
        _ => -e::ENOSYS,
    }
}

/// `clock_nanosleep(clock, flags, req)`: `TIMER_ABSTIME` — срок абсолютный.
unsafe fn clock_nanosleep(clock: usize, flags: usize, req: usize) -> i64 {
    const TIMER_ABSTIME: usize = 1;
    if !space::user_can(req, 16, PageFlags::READ) {
        return -e::EFAULT;
    }
    // SAFETY: проверено строкой выше.
    let [seconds, nanos] = unsafe { core::ptr::read_unaligned(req as *const [u64; 2]) };
    let (mut seconds, mut nanos) = (seconds, nanos);
    if flags & TIMER_ABSTIME != 0 {
        let wanted = u128::from(seconds) * 1_000_000_000 + u128::from(nanos);
        let now = if clock == CLOCK_REALTIME {
            u128::from(crate::time::now_unix().unwrap_or(0)) * 1_000_000_000
                + u128::from(crate::time::uptime_ns() % 1_000_000_000)
        } else {
            u128::from(crate::time::uptime_ns())
        };
        let left = wanted.saturating_sub(now);
        seconds = (left / 1_000_000_000) as u64;
        nanos = (left % 1_000_000_000) as u64;
    }
    // SAFETY: контракт `syscall`.
    errno(unsafe { native(SYS_NANOSLEEP, seconds as usize, nanos as usize, 0) })
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

/// `readv` — по куску родным `read`.
unsafe fn readv(fd: usize, iov: usize, count: usize) -> i64 {
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
        let got = unsafe { native(SYS_READ, fd, base, len) };
        if got < 0 {
            return if total > 0 { total } else { errno(got) };
        }
        total += got;
        if (got as usize) < len {
            break;
        }
    }
    total
}

/// `ftruncate(fd, length)` — длина открытого файла (и `memfd`, фаза 71e).
fn ftruncate(fd: usize, length: usize) -> i64 {
    match crate::user::with_current(|program| program.files.node(fd)) {
        Some(Ok(node)) => match node.truncate(length as u64) {
            Ok(()) => 0,
            Err(err) => vfs_errno(err),
        },
        Some(Err(err)) => file_errno(err),
        None => -e::ESRCH,
    }
}

/// `unlink`/`unlinkat`: `/dev/shm` — имя разделяемой памяти, остальное —
/// файл на диске (фаза 71e).
fn unlink(dir: usize, ptr: usize) -> i64 {
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    if let Some(answer) = ipc::unlink_shm(&linux) {
        return answer;
    }
    let cred = crate::user::credentials();
    let path = crate::linux_path::resolve(&linux, cred);
    match crate::fs::remove_as(cred, &path) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}
