//! FreeOS / FossAi: picolibc + `libc/freeos` (https://github.com/anomal3/FreeOpenSourceSystemAI).
//!
//! Types and struct layouts mirror the C headers of the FreeOS sysroot
//! (`build/toolchain/sysroot/<arch>/include` + `libc/freeos/include`); they
//! were taken from the preprocessed headers and checked with sizeof/offsetof
//! probes on x86_64 and aarch64. Constants below the marker are generated
//! from the same headers (`ports/rust-std/gen-libc.py`) — do not edit by hand.

use crate::prelude::*;

pub type blkcnt_t = i64;
pub type blksize_t = i32;
pub type clock_t = c_ulong;
pub type clockid_t = c_ulong;
pub type dev_t = u64;
pub type fsblkcnt_t = u64;
pub type fsfilcnt_t = u64;
pub type id_t = u32;
pub type ino_t = u64;
pub type key_t = c_int;
pub type loff_t = i64;
pub type mode_t = u32;
pub type nfds_t = c_ulong;
pub type nlink_t = u64;
pub type off_t = i64;
pub type pthread_key_t = c_uint;
pub type pthread_t = c_ulong;
pub type rlim_t = u64;
pub type sa_family_t = u16;
pub type sigset_t = c_ulong;
pub type socklen_t = u32;
pub type speed_t = c_uint;
pub type suseconds_t = i32;
pub type tcflag_t = c_uint;
pub type time_t = i64;
pub type useconds_t = u32;
#[cfg(target_arch = "x86_64")]
pub type wchar_t = i32;
#[cfg(not(target_arch = "x86_64"))]
pub type wchar_t = u32;
pub type nl_item = c_int;
pub type __fd_mask = c_ulong;

// libc/freeos/include/sys/filio.h (not in the generated set)
pub const FIONREAD: c_ulong = 0x541B;
pub const FIONBIO: c_ulong = 0x5421;
// poll.h: these bits are accepted and ignored by libc/freeos poll() (Linux
// numbers); mio asks for them next to POLLIN/POLLOUT.
pub const POLLRDNORM: c_short = 0x040;
pub const POLLRDBAND: c_short = 0x080;
pub const POLLWRNORM: c_short = 0x100;
pub const POLLWRBAND: c_short = 0x200;

// Peer credentials of a unix socket (tokio asks; getsockopt answers
// ENOPROTOOPT). Linux number and layout.
pub const SO_PEERCRED: c_int = 17;

// libc/freeos/include/netinet/in.h, added for std (Linux numbers)
pub const IP_TOS: c_int = 1;
pub const IP_TTL: c_int = 2;
pub const IPV6_ADD_MEMBERSHIP: c_int = 20;
pub const IPV6_DROP_MEMBERSHIP: c_int = 21;

pub const STDIN_FILENO: c_int = 0;
pub const STDOUT_FILENO: c_int = 1;
pub const STDERR_FILENO: c_int = 2;

s! {
    pub struct stat {
        pub st_dev: dev_t,
        pub st_ino: ino_t,
        pub st_mode: mode_t,
        pub st_nlink: nlink_t,
        pub st_uid: crate::uid_t,
        pub st_gid: crate::gid_t,
        pub st_rdev: dev_t,
        pub st_size: off_t,
        pub st_blksize: blksize_t,
        pub st_blocks: blkcnt_t,
        pub st_atim: crate::timespec,
        pub st_mtim: crate::timespec,
        pub st_ctim: crate::timespec,
    }

    pub struct dirent {
        pub d_ino: ino_t,
        pub d_type: u8,
        pub d_name: [c_char; 256],
    }

    pub struct sockaddr {
        pub sa_family: sa_family_t,
        pub sa_data: [c_char; 14],
    }

    pub struct sockaddr_in {
        pub sin_family: sa_family_t,
        pub sin_port: crate::in_port_t,
        pub sin_addr: crate::in_addr,
        pub sin_zero: [u8; 8],
    }

    pub struct sockaddr_in6 {
        pub sin6_family: sa_family_t,
        pub sin6_port: crate::in_port_t,
        pub sin6_flowinfo: u32,
        pub sin6_addr: crate::in6_addr,
        pub sin6_scope_id: u32,
    }

    pub struct sockaddr_un {
        pub sun_family: sa_family_t,
        pub sun_path: [c_char; 108],
    }

    pub struct sockaddr_storage {
        pub ss_family: sa_family_t,
        __ss_padding: [c_char; 118],
        __ss_align: c_ulong,
    }

    pub struct in_addr {
        pub s_addr: crate::in_addr_t,
    }

    pub struct ip_mreq {
        pub imr_multiaddr: in_addr,
        pub imr_interface: in_addr,
    }

    pub struct addrinfo {
        pub ai_flags: c_int,
        pub ai_family: c_int,
        pub ai_socktype: c_int,
        pub ai_protocol: c_int,
        pub ai_addrlen: socklen_t,
        pub ai_addr: *mut sockaddr,
        pub ai_canonname: *mut c_char,
        pub ai_next: *mut addrinfo,
    }

    pub struct msghdr {
        pub msg_name: *mut c_void,
        pub msg_namelen: socklen_t,
        pub msg_iov: *mut crate::iovec,
        pub msg_iovlen: size_t,
        pub msg_control: *mut c_void,
        pub msg_controllen: size_t,
        pub msg_flags: c_int,
    }

    pub struct sigaction {
        pub sa_sigaction: crate::sighandler_t,
        pub sa_mask: sigset_t,
        pub sa_flags: c_int,
    }

    // <signal.h> picolibc (N5, 08.11.2026): нужен signal-hook-registry
    // (tokio/Servo). Сигналы ядро не доставляет — тип только для раскладки.
    pub struct siginfo_t {
        pub si_signo: c_int,
        pub si_code: c_int,
        pub si_errno: c_int,
        pub si_pid: crate::pid_t,
        pub si_uid: crate::uid_t,
        pub si_addr: *mut c_void,
        pub si_status: c_int,
        pub si_band: c_long,
        pub si_value: crate::sigval,
    }

    pub struct stack_t {
        pub ss_sp: *mut c_void,
        pub ss_flags: c_int,
        pub ss_size: size_t,
    }

    pub struct tm {
        pub tm_sec: c_int,
        pub tm_min: c_int,
        pub tm_hour: c_int,
        pub tm_mday: c_int,
        pub tm_mon: c_int,
        pub tm_year: c_int,
        pub tm_wday: c_int,
        pub tm_yday: c_int,
        pub tm_isdst: c_int,
    }

    pub struct utsname {
        pub sysname: [c_char; 65],
        pub nodename: [c_char; 65],
        pub release: [c_char; 65],
        pub version: [c_char; 65],
        pub machine: [c_char; 65],
    }

    pub struct passwd {
        pub pw_name: *mut c_char,
        pub pw_uid: crate::uid_t,
        pub pw_gid: crate::gid_t,
        pub pw_dir: *mut c_char,
        pub pw_shell: *mut c_char,
    }

    pub struct group {
        pub gr_name: *mut c_char,
        pub gr_gid: crate::gid_t,
        pub gr_mem: *mut *mut c_char,
    }

    pub struct rusage {
        pub ru_utime: crate::timeval,
        pub ru_stime: crate::timeval,
    }

    pub struct winsize {
        pub ws_row: c_ushort,
        pub ws_col: c_ushort,
    }

    pub struct ucred {
        pub pid: crate::pid_t,
        pub uid: crate::uid_t,
        pub gid: crate::gid_t,
    }

    pub struct sched_param {
        pub sched_priority: c_int,
    }

    pub struct termios {
        pub c_iflag: tcflag_t,
        pub c_oflag: tcflag_t,
        pub c_cflag: tcflag_t,
        pub c_lflag: tcflag_t,
        pub c_cc: [crate::cc_t; 11],
        pub c_ispeed: speed_t,
        pub c_ospeed: speed_t,
    }

    pub struct flock {
        pub l_type: c_short,
        pub l_whence: c_short,
        pub l_start: c_long,
        pub l_len: c_long,
        pub l_pid: c_short,
        pub l_xxx: c_short,
    }

    pub struct itimerspec {
        pub it_interval: crate::timespec,
        pub it_value: crate::timespec,
    }

    pub struct timezone {
        pub tz_minuteswest: c_int,
        pub tz_dsttime: c_int,
    }

    pub struct fd_set {
        fds_bits: [__fd_mask; 1],
    }

    pub struct lconv {
        pub decimal_point: *mut c_char,
        pub thousands_sep: *mut c_char,
        pub grouping: *mut c_char,
        pub int_curr_symbol: *mut c_char,
        pub currency_symbol: *mut c_char,
        pub mon_decimal_point: *mut c_char,
        pub mon_thousands_sep: *mut c_char,
        pub mon_grouping: *mut c_char,
        pub positive_sign: *mut c_char,
        pub negative_sign: *mut c_char,
        pub int_frac_digits: c_char,
        pub frac_digits: c_char,
        pub p_cs_precedes: c_char,
        pub p_sep_by_space: c_char,
        pub n_cs_precedes: c_char,
        pub n_sep_by_space: c_char,
        pub p_sign_posn: c_char,
        pub n_sign_posn: c_char,
        pub int_n_cs_precedes: c_char,
        pub int_n_sep_by_space: c_char,
        pub int_n_sign_posn: c_char,
        pub int_p_cs_precedes: c_char,
        pub int_p_sep_by_space: c_char,
        pub int_p_sign_posn: c_char,
    }

    // libc/freeos/pthread.h
    pub struct pthread_attr_t {
        __stacksize: size_t,
        __detachstate: c_int,
        __stackaddr: *mut c_void,
    }

    pub struct pthread_mutex_t {
        __state: c_uint,
        __type: c_int,
        __owner: pthread_t,
        __count: c_uint,
    }

    pub struct pthread_mutexattr_t {
        __type: c_int,
    }

    pub struct pthread_cond_t {
        __seq: c_uint,
        __clock: c_int,
    }

    pub struct pthread_condattr_t {
        __clock: c_int,
    }

    pub struct pthread_rwlock_t {
        __lock: pthread_mutex_t,
        __readable: pthread_cond_t,
        __writable: pthread_cond_t,
        __readers: c_int,
        __writer: c_int,
        __waiting_writers: c_int,
    }

    pub struct pthread_rwlockattr_t {
        __unused: c_int,
    }

    pub struct pthread_once_t {
        __state: c_uint,
    }

    pub struct sem_t {
        __value: c_uint,
        __waiters: c_uint,
    }
}

pub const PTHREAD_MUTEX_INITIALIZER: pthread_mutex_t = pthread_mutex_t {
    __state: 0,
    __type: 0,
    __owner: 0,
    __count: 0,
};
pub const PTHREAD_COND_INITIALIZER: pthread_cond_t = pthread_cond_t {
    __seq: 0,
    __clock: CLOCK_REALTIME as c_int,
};
pub const PTHREAD_RWLOCK_INITIALIZER: pthread_rwlock_t = pthread_rwlock_t {
    __lock: PTHREAD_MUTEX_INITIALIZER,
    __readable: PTHREAD_COND_INITIALIZER,
    __writable: PTHREAD_COND_INITIALIZER,
    __readers: 0,
    __writer: 0,
    __waiting_writers: 0,
};
pub const PTHREAD_ONCE_INIT: pthread_once_t = pthread_once_t { __state: 0 };

f! {
    pub unsafe fn FD_CLR(fd: c_int, set: *mut fd_set) -> () {
        let bits = size_of::<__fd_mask>() * 8;
        let fd = fd as usize;
        (*set).fds_bits[fd / bits] &= !(1 << (fd % bits));
    }

    pub unsafe fn FD_ISSET(fd: c_int, set: *const fd_set) -> bool {
        let bits = size_of::<__fd_mask>() * 8;
        let fd = fd as usize;
        ((*set).fds_bits[fd / bits] & (1 << (fd % bits))) != 0
    }

    pub unsafe fn FD_SET(fd: c_int, set: *mut fd_set) -> () {
        let bits = size_of::<__fd_mask>() * 8;
        let fd = fd as usize;
        (*set).fds_bits[fd / bits] |= 1 << (fd % bits);
    }

    pub unsafe fn FD_ZERO(set: *mut fd_set) -> () {
        (*set).fds_bits.fill(0);
    }
}

safe_f! {
    pub const safe fn WIFEXITED(status: c_int) -> bool {
        (status & 0xff) == 0
    }

    pub const safe fn WEXITSTATUS(status: c_int) -> c_int {
        (status >> 8) & 0xff
    }

    pub const safe fn WIFSIGNALED(status: c_int) -> bool {
        (status & 0x7f) > 0 && (status & 0x7f) < 0x7f
    }

    pub const safe fn WTERMSIG(status: c_int) -> c_int {
        status & 0x7f
    }

    pub const safe fn WIFCONTINUED(status: c_int) -> bool {
        status == 0xffff
    }

    pub const safe fn WCOREDUMP(status: c_int) -> bool {
        (status & 0x80) != 0
    }

    pub const safe fn WIFSTOPPED(status: c_int) -> bool {
        (status & 0xff) == 0x7f
    }

    pub const safe fn WSTOPSIG(status: c_int) -> c_int {
        (status >> 8) & 0xff
    }
}

extern "C" {
    pub fn __errno() -> *mut c_int;

    pub fn pthread_create(
        native: *mut pthread_t,
        attr: *const pthread_attr_t,
        f: extern "C" fn(*mut c_void) -> *mut c_void,
        value: *mut c_void,
    ) -> c_int;
    pub fn dirfd(dirp: *mut crate::DIR) -> c_int;
    pub fn setgroups(ngroups: c_int, ptr: *const crate::gid_t) -> c_int;

    pub fn getrlimit(resource: c_int, rlim: *mut crate::rlimit) -> c_int;
    pub fn setrlimit(resource: c_int, rlim: *const crate::rlimit) -> c_int;
    pub fn strerror_r(errnum: c_int, buf: *mut c_char, buflen: size_t) -> c_int;

    pub fn sem_destroy(sem: *mut sem_t) -> c_int;
    pub fn sem_init(sem: *mut sem_t, pshared: c_int, value: c_uint) -> c_int;

    pub fn bind(fd: c_int, addr: *const sockaddr, len: socklen_t) -> c_int;
    pub fn clock_settime(clock_id: clockid_t, tp: *const crate::timespec) -> c_int;
    pub fn clock_gettime(clock_id: clockid_t, tp: *mut crate::timespec) -> c_int;
    pub fn clock_getres(clock_id: clockid_t, res: *mut crate::timespec) -> c_int;
    pub fn clock_nanosleep(
        clock_id: clockid_t,
        flags: c_int,
        rqtp: *const crate::timespec,
        rmtp: *mut crate::timespec,
    ) -> c_int;
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn recvfrom(
        fd: c_int,
        buf: *mut c_void,
        n: size_t,
        flags: c_int,
        addr: *mut sockaddr,
        addr_len: *mut socklen_t,
    ) -> ssize_t;
    pub fn getnameinfo(
        sa: *const sockaddr,
        salen: socklen_t,
        host: *mut c_char,
        hostlen: socklen_t,
        serv: *mut c_char,
        servlen: socklen_t,
        flags: c_int,
    ) -> c_int;
    pub fn memalign(align: size_t, size: size_t) -> *mut c_void;
    pub fn gettimeofday(tp: *mut crate::timeval, tz: *mut c_void) -> c_int;
    pub fn sigaltstack(ss: *const stack_t, oss: *mut stack_t) -> c_int;
    pub fn pthread_sigmask(how: c_int, set: *const sigset_t, oldset: *mut sigset_t) -> c_int;
    pub fn pthread_kill(thread: pthread_t, sig: c_int) -> c_int;
    pub fn pthread_getattr_np(thread: pthread_t, attr: *mut pthread_attr_t) -> c_int;
    pub fn pthread_attr_getstack(
        attr: *const pthread_attr_t,
        stackaddr: *mut *mut c_void,
        stacksize: *mut size_t,
    ) -> c_int;
    pub fn pthread_condattr_setclock(attr: *mut pthread_condattr_t, clock_id: clockid_t) -> c_int;
    pub fn pthread_getthreadid_np() -> c_int;
    pub fn getpwuid_r(
        uid: crate::uid_t,
        pwd: *mut passwd,
        buf: *mut c_char,
        buflen: size_t,
        result: *mut *mut passwd,
    ) -> c_int;
    pub fn uname(buf: *mut utsname) -> c_int;
    pub fn arc4random_buf(buf: *mut c_void, nbytes: size_t);
    pub fn getentropy(buf: *mut c_void, buflen: size_t) -> c_int;
    pub fn accept4(
        fd: c_int,
        addr: *mut sockaddr,
        len: *mut socklen_t,
        flg: c_int,
    ) -> c_int;
    pub fn pipe2(fds: *mut c_int, flags: c_int) -> c_int;
    pub fn sendmsg(fd: c_int, msg: *const msghdr, flags: c_int) -> ssize_t;
    pub fn recvmsg(fd: c_int, msg: *mut msghdr, flags: c_int) -> ssize_t;
    pub fn dup3(oldfd: c_int, newfd: c_int, flags: c_int) -> c_int;
    pub fn sched_yield() -> c_int;
    pub fn memrchr(cx: *const c_void, c: c_int, n: size_t) -> *mut c_void;
    pub fn writev(fd: c_int, iov: *const crate::iovec, iovcnt: c_int) -> ssize_t;
    pub fn readv(fd: c_int, iov: *const crate::iovec, iovcnt: c_int) -> ssize_t;
    pub fn pread(fd: c_int, buf: *mut c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn pwrite(fd: c_int, buf: *const c_void, count: size_t, offset: off_t) -> ssize_t;
    pub fn futimens(fd: c_int, times: *const crate::timespec) -> c_int;
    pub fn utimensat(
        dirfd: c_int,
        path: *const c_char,
        times: *const crate::timespec,
        flag: c_int,
    ) -> c_int;
}

// Not provided by FreeOS headers; declared so the common unix module
// compiles. `statvfs()`/`dladdr()` are not in libc/freeos (link would fail).
s! {
    pub struct statvfs {
        pub f_bsize: c_ulong,
        pub f_frsize: c_ulong,
        pub f_blocks: fsblkcnt_t,
        pub f_bfree: fsblkcnt_t,
        pub f_bavail: fsblkcnt_t,
        pub f_files: fsfilcnt_t,
        pub f_ffree: fsfilcnt_t,
        pub f_favail: fsfilcnt_t,
        pub f_fsid: c_ulong,
        pub f_flag: c_ulong,
        pub f_namemax: c_ulong,
    }

    pub struct Dl_info {
        pub dli_fname: *const c_char,
        pub dli_fbase: *mut c_void,
        pub dli_sname: *const c_char,
        pub dli_saddr: *mut c_void,
    }
}

// ---- generated constants below (gen-libc.py) ----
pub const ACCESSPERMS: crate::mode_t = 511;
pub const AF_INET: c_int = 2;
pub const AF_INET6: c_int = 10;
pub const AF_LOCAL: c_int = 1;
pub const AF_UNIX: c_int = 1;
pub const AF_UNSPEC: c_int = 0;
pub const AI_ADDRCONFIG: c_int = 32;
pub const AI_CANONNAME: c_int = 2;
pub const AI_NUMERICHOST: c_int = 4;
pub const AI_NUMERICSERV: c_int = 1024;
pub const AI_PASSIVE: c_int = 1;
pub const ALLPERMS: crate::mode_t = 4095;
pub const ARG_MAX: c_int = 65536;
pub const AT_EACCESS: c_int = 1;
pub const AT_FDCWD: c_int = -2;
pub const AT_REMOVEDIR: c_int = 8;
pub const AT_SYMLINK_FOLLOW: c_int = 4;
pub const AT_SYMLINK_NOFOLLOW: c_int = 2;
pub const B0: crate::speed_t = 0;
pub const B1000000: crate::speed_t = 1000000;
pub const B110: crate::speed_t = 110;
pub const B115200: crate::speed_t = 115200;
pub const B1152000: crate::speed_t = 1152000;
pub const B1200: crate::speed_t = 1200;
pub const B134: crate::speed_t = 134;
pub const B14400: crate::speed_t = 14400;
pub const B150: crate::speed_t = 150;
pub const B1500000: crate::speed_t = 1500000;
pub const B153600: crate::speed_t = 153600;
pub const B1800: crate::speed_t = 1800;
pub const B19200: crate::speed_t = 19200;
pub const B200: crate::speed_t = 200;
pub const B2000000: crate::speed_t = 2000000;
pub const B230400: crate::speed_t = 230400;
pub const B2400: crate::speed_t = 2400;
pub const B2500000: crate::speed_t = 2500000;
pub const B28800: crate::speed_t = 28800;
pub const B300: crate::speed_t = 300;
pub const B3000000: crate::speed_t = 3000000;
pub const B307200: crate::speed_t = 307200;
pub const B3500000: crate::speed_t = 3500000;
pub const B38400: crate::speed_t = 38400;
pub const B4000000: crate::speed_t = 4000000;
pub const B460800: crate::speed_t = 460800;
pub const B4800: crate::speed_t = 4800;
pub const B50: crate::speed_t = 50;
pub const B500000: crate::speed_t = 500000;
pub const B57600: crate::speed_t = 57600;
pub const B576000: crate::speed_t = 576000;
pub const B600: crate::speed_t = 600;
pub const B614400: crate::speed_t = 614400;
pub const B7200: crate::speed_t = 7200;
pub const B75: crate::speed_t = 75;
pub const B76800: crate::speed_t = 76800;
pub const B921600: crate::speed_t = 921600;
pub const B9600: crate::speed_t = 9600;
pub const BIG_ENDIAN: c_int = 4321;
pub const BRKINT: crate::tcflag_t = 1;
pub const BS0: crate::tcflag_t = 0;
pub const BS1: crate::tcflag_t = 4096;
pub const BSDLY: crate::tcflag_t = 4096;
pub const BUFSIZ: c_uint = 512;
pub const BYTE_ORDER: usize = 1234;
pub const CHAR_BIT: usize = 8;
pub const CHILD_MAX: c_int = 40;
pub const CLOCAL: crate::tcflag_t = 512;
pub const CLOCKS_PER_SEC: c_int = 1000000;
pub const CLOCK_MONOTONIC: crate::clockid_t = 4;
pub const CLOCK_REALTIME: crate::clockid_t = 1;
pub const COLL_WEIGHTS_MAX: c_int = 0;
pub const CR0: crate::tcflag_t = 0;
pub const CR1: crate::tcflag_t = 256;
pub const CR2: crate::tcflag_t = 512;
pub const CR3: crate::tcflag_t = 768;
pub const CRDLY: crate::tcflag_t = 768;
pub const CREAD: crate::tcflag_t = 32;
pub const CS5: crate::tcflag_t = 5;
pub const CS6: crate::tcflag_t = 6;
pub const CS7: crate::tcflag_t = 7;
pub const CS8: crate::tcflag_t = 8;
pub const CSIZE: crate::tcflag_t = 15;
pub const CSTOPB: crate::tcflag_t = 16;
pub const DEFFILEMODE: crate::mode_t = 438;
pub const DST_AUST: c_int = 2;
pub const DST_CAN: c_int = 6;
pub const DST_EET: c_int = 5;
pub const DST_MET: c_int = 4;
pub const DST_NONE: c_int = 0;
pub const DST_USA: c_int = 1;
pub const DST_WET: c_int = 3;
pub const DT_BLK: u8 = 6;
pub const DT_CHR: u8 = 2;
pub const DT_DIR: u8 = 4;
pub const DT_FIFO: u8 = 1;
pub const DT_LNK: u8 = 10;
pub const DT_REG: u8 = 8;
pub const DT_SOCK: u8 = 12;
pub const DT_UNKNOWN: u8 = 0;
pub const DT_WHT: c_uchar = 14;
pub const E2BIG: c_int = 7;
pub const EACCES: c_int = 13;
pub const EADDRINUSE: c_int = 112;
pub const EADDRNOTAVAIL: c_int = 125;
pub const EADV: c_int = 68;
pub const EAFNOSUPPORT: c_int = 106;
pub const EAGAIN: c_int = 11;
pub const EAI_AGAIN: c_int = -3;
pub const EAI_BADFLAGS: c_int = -1;
pub const EAI_FAIL: c_int = -4;
pub const EAI_FAMILY: c_int = -6;
pub const EAI_MEMORY: c_int = -10;
pub const EAI_NONAME: c_int = -2;
pub const EAI_SERVICE: c_int = -8;
pub const EAI_SOCKTYPE: c_int = -7;
pub const EAI_SYSTEM: c_int = -11;
pub const EALREADY: c_int = 120;
pub const EBADE: c_int = 50;
pub const EBADF: c_int = 9;
pub const EBADFD: c_int = 81;
pub const EBADMSG: c_int = 77;
pub const EBADR: c_int = 51;
pub const EBADRQC: c_int = 54;
pub const EBADSLT: c_int = 55;
pub const EBFONT: c_int = 57;
pub const EBUSY: c_int = 16;
pub const ECANCELED: c_int = 140;
pub const ECHILD: c_int = 10;
pub const ECHO: crate::tcflag_t = 1;
pub const ECHOE: crate::tcflag_t = 2;
pub const ECHOK: crate::tcflag_t = 4;
pub const ECHONL: crate::tcflag_t = 8;
pub const ECHRNG: c_int = 37;
pub const ECOMM: c_int = 70;
pub const ECONNABORTED: c_int = 113;
pub const ECONNREFUSED: c_int = 111;
pub const ECONNRESET: c_int = 104;
pub const EDEADLK: c_int = 45;
pub const EDEADLOCK: c_int = 56;
pub const EDESTADDRREQ: c_int = 121;
pub const EDOM: c_int = 33;
pub const EDOTDOT: c_int = 76;
pub const EDQUOT: c_int = 132;
pub const EEXIST: c_int = 17;
pub const EFAULT: c_int = 14;
pub const EFBIG: c_int = 27;
pub const EFTYPE: c_int = 79;
pub const EHOSTDOWN: c_int = 117;
pub const EHOSTUNREACH: c_int = 118;
pub const EHWPOISON: c_int = 144;
pub const EIDRM: c_int = 36;
pub const EILSEQ: c_int = 138;
pub const EINPROGRESS: c_int = 119;
pub const EINTR: c_int = 4;
pub const EINVAL: c_int = 22;
pub const EIO: c_int = 5;
pub const EISCONN: c_int = 127;
pub const EISDIR: c_int = 21;
pub const EISNAM: c_int = 145;
pub const EKEYEXPIRED: c_int = 146;
pub const EKEYREJECTED: c_int = 147;
pub const EKEYREVOKED: c_int = 148;
pub const EL2HLT: c_int = 44;
pub const EL2NSYNC: c_int = 38;
pub const EL3HLT: c_int = 39;
pub const EL3RST: c_int = 40;
pub const ELBIN: i32 = 75;
pub const ELIBACC: c_int = 83;
pub const ELIBBAD: c_int = 84;
pub const ELIBEXEC: c_int = 87;
pub const ELIBMAX: c_int = 86;
pub const ELIBSCN: c_int = 85;
pub const ELNRNG: c_int = 41;
pub const ELOOP: c_int = 92;
pub const EMFILE: c_int = 24;
pub const EMLINK: c_int = 31;
pub const EMSGSIZE: c_int = 122;
pub const EMULTIHOP: c_int = 74;
pub const ENAMETOOLONG: c_int = 91;
pub const ENETDOWN: c_int = 115;
pub const ENETRESET: c_int = 126;
pub const ENETUNREACH: c_int = 114;
pub const ENFILE: c_int = 23;
pub const ENOANO: c_int = 53;
pub const ENOBUFS: c_int = 105;
pub const ENOCSI: c_int = 43;
pub const ENODATA: c_int = 61;
pub const ENODEV: c_int = 19;
pub const ENOENT: c_int = 2;
pub const ENOEXEC: c_int = 8;
pub const ENOLCK: c_int = 46;
pub const ENOLINK: c_int = 67;
pub const ENOMEDIUM: c_int = 135;
pub const ENOMEM: c_int = 12;
pub const ENOMSG: c_int = 35;
pub const ENONET: c_int = 64;
pub const ENOPKG: c_int = 65;
pub const ENOPROTOOPT: c_int = 109;
pub const ENOSPC: c_int = 28;
pub const ENOSR: c_int = 63;
pub const ENOSTR: c_int = 60;
pub const ENOSYS: c_int = 88;
pub const ENOTBLK: c_int = 15;
pub const ENOTCONN: c_int = 128;
pub const ENOTDIR: c_int = 20;
pub const ENOTEMPTY: c_int = 90;
pub const ENOTRECOVERABLE: c_int = 141;
pub const ENOTSOCK: c_int = 108;
pub const ENOTSUP: c_int = 134;
pub const ENOTTY: c_int = 25;
pub const ENOTUNIQ: c_int = 80;
pub const ENXIO: c_int = 6;
pub const EOF: c_int = -1;
pub const EOPNOTSUPP: c_int = 95;
pub const EOVERFLOW: c_int = 139;
pub const EOWNERDEAD: c_int = 142;
pub const EPERM: c_int = 1;
pub const EPFNOSUPPORT: c_int = 96;
pub const EPIPE: c_int = 32;
pub const EPROCLIM: c_int = 130;
pub const EPROTO: c_int = 71;
pub const EPROTONOSUPPORT: c_int = 123;
pub const EPROTOTYPE: c_int = 107;
pub const ERANGE: c_int = 34;
pub const EREMCHG: c_int = 82;
pub const EREMOTE: c_int = 66;
pub const EROFS: c_int = 30;
pub const ESHUTDOWN: c_int = 110;
pub const ESOCKTNOSUPPORT: c_int = 124;
pub const ESPIPE: c_int = 29;
pub const ESRCH: c_int = 3;
pub const ESRMNT: c_int = 69;
pub const ESTALE: c_int = 133;
pub const ESTRPIPE: c_int = 143;
pub const ETIME: c_int = 62;
pub const ETIMEDOUT: c_int = 116;
pub const ETOOMANYREFS: c_int = 129;
pub const ETXTBSY: c_int = 26;
pub const EUNATCH: c_int = 42;
pub const EUSERS: c_int = 131;
pub const EWOULDBLOCK: c_int = 11;
pub const EXDEV: c_int = 18;
pub const EXFULL: c_int = 52;
pub const EXIT_FAILURE: c_int = 1;
pub const EXIT_SUCCESS: c_int = 0;
pub const EXPR_NEST_MAX: c_int = 32;
pub const FAPPEND: c_int = 1024;
pub const FASYNC: c_int = 8;
pub const FCREAT: c_int = 64;
pub const FD_CLOEXEC: c_int = 1;
pub const FD_SETSIZE: usize = 64;
pub const FEXCL: c_int = 2048;
pub const FF0: crate::tcflag_t = 0;
pub const FF1: crate::tcflag_t = 16384;
pub const FFDLY: crate::tcflag_t = 16384;
pub const FILENAME_MAX: c_uint = 1024;
pub const FNDELAY: c_int = 16384;
pub const FNM_NOESCAPE: c_int = 1;
pub const FNM_NOMATCH: c_int = 1;
pub const FNM_PATHNAME: c_int = 2;
pub const FNM_PERIOD: c_int = 4;
pub const FNOCTTY: c_int = 32768;
pub const FNONBLOCK: c_int = 16384;
pub const FOPEN_MAX: c_uint = 32;
pub const FREAD: c_int = 1;
pub const FSYNC: c_int = 8192;
pub const FTRUNC: c_int = 512;
pub const FWRITE: c_int = 2;
pub const F_CNVT: c_int = 12;
pub const F_DUPFD: c_int = 0;
pub const F_DUPFD_CLOEXEC: c_int = 14;
pub const F_GETFD: c_int = 1;
pub const F_GETFL: c_int = 3;
pub const F_GETLK: c_int = 7;
pub const F_GETOWN: c_int = 5;
pub const F_LOCK: c_int = 1;
pub const F_OK: c_int = 0;
pub const F_RDLCK: c_int = 1;
pub const F_RGETLK: c_int = 10;
pub const F_RSETLK: c_int = 11;
pub const F_RSETLKW: c_int = 13;
pub const F_SETFD: c_int = 2;
pub const F_SETFL: c_int = 4;
pub const F_SETLK: c_int = 8;
pub const F_SETLKW: c_int = 9;
pub const F_SETOWN: c_int = 6;
pub const F_TEST: c_int = 3;
pub const F_TLOCK: c_int = 2;
pub const F_ULOCK: c_int = 0;
pub const F_UNLCK: c_int = 3;
pub const F_WRLCK: c_int = 2;
pub const HOST_NOT_FOUND: c_int = 1;
pub const HUPCL: crate::tcflag_t = 256;
pub const ICANON: crate::tcflag_t = 16;
pub const ICRNL: crate::tcflag_t = 2;
pub const IEXTEN: crate::tcflag_t = 32;
pub const IFF_BROADCAST: c_int = 2;
pub const IFF_LOOPBACK: c_int = 8;
pub const IFF_MULTICAST: c_int = 4096;
pub const IFF_RUNNING: c_int = 64;
pub const IFF_UP: c_int = 1;
pub const IFNAMSIZ: size_t = 16;
pub const IF_NAMESIZE: size_t = 16;
pub const IGNBRK: crate::tcflag_t = 4;
pub const IGNCR: crate::tcflag_t = 8;
pub const IGNPAR: crate::tcflag_t = 16;
pub const INADDR_ANY: crate::in_addr_t = 0;
pub const INADDR_BROADCAST: crate::in_addr_t = 4294967295;
pub const INADDR_LOOPBACK: crate::in_addr_t = 2130706433;
pub const INADDR_NONE: crate::in_addr_t = 4294967295;
pub const INET6_ADDRSTRLEN: usize = 46;
pub const INET_ADDRSTRLEN: usize = 16;
pub const INLCR: crate::tcflag_t = 32;
pub const INPCK: crate::tcflag_t = 64;
pub const INT16_MAX: i16 = 32767;
pub const INT16_MIN: i16 = -32768;
pub const INT32_MAX: i32 = 2147483647;
pub const INT32_MIN: i32 = -2147483648;
pub const INT8_MAX: i8 = 127;
pub const INT8_MIN: i8 = -128;
pub const INT_FAST16_MAX: c_int = 32767;
pub const INT_FAST16_MIN: c_int = -32768;
pub const INT_FAST32_MAX: c_int = 2147483647;
pub const INT_FAST32_MIN: c_int = -2147483648;
pub const INT_FAST8_MAX: c_int = 127;
pub const INT_FAST8_MIN: c_int = -128;
pub const INT_LEAST16_MAX: c_int = 32767;
pub const INT_LEAST16_MIN: c_int = -32768;
pub const INT_LEAST32_MAX: c_int = 2147483647;
pub const INT_LEAST32_MIN: c_int = -2147483648;
pub const INT_LEAST8_MAX: c_int = 127;
pub const INT_LEAST8_MIN: c_int = -128;
pub const IOV_MAX: c_int = 1024;
pub const IPPROTO_ICMP: c_int = 1;
pub const IPPROTO_IP: c_int = 0;
pub const IPPROTO_IPV6: c_int = 41;
pub const IPPROTO_RAW: c_int = 255;
pub const IPPROTO_TCP: c_int = 6;
pub const IPPROTO_UDP: c_int = 17;
pub const IPV6_JOIN_GROUP: c_int = 20;
pub const IPV6_LEAVE_GROUP: c_int = 21;
pub const IPV6_MULTICAST_HOPS: c_int = 18;
pub const IPV6_MULTICAST_IF: c_int = 17;
pub const IPV6_MULTICAST_LOOP: c_int = 19;
pub const IPV6_UNICAST_HOPS: c_int = 16;
pub const IPV6_V6ONLY: c_int = 26;
pub const IP_ADD_MEMBERSHIP: c_int = 35;
pub const IP_DROP_MEMBERSHIP: c_int = 36;
pub const IP_MULTICAST_IF: c_int = 32;
pub const IP_MULTICAST_LOOP: c_int = 34;
pub const IP_MULTICAST_TTL: c_int = 33;
pub const ISIG: crate::tcflag_t = 64;
pub const ISTRIP: crate::tcflag_t = 128;
pub const ITIMER_PROF: c_int = 2;
pub const ITIMER_REAL: c_int = 0;
pub const ITIMER_VIRTUAL: c_int = 1;
pub const IXANY: crate::tcflag_t = 256;
pub const IXOFF: crate::tcflag_t = 512;
pub const IXON: crate::tcflag_t = 1024;
pub const LC_ALL: c_int = 0;
pub const LC_ALL_MASK: c_int = 126;
pub const LC_COLLATE: c_int = 1;
pub const LC_COLLATE_MASK: c_int = 2;
pub const LC_CTYPE: c_int = 2;
pub const LC_CTYPE_MASK: c_int = 4;
pub const LC_GLOBAL_LOCALE: c_int = -1;
pub const LC_MESSAGES: c_int = 6;
pub const LC_MESSAGES_MASK: c_int = 64;
pub const LC_MONETARY: c_int = 3;
pub const LC_MONETARY_MASK: c_int = 8;
pub const LC_NUMERIC: c_int = 4;
pub const LC_NUMERIC_MASK: c_int = 16;
pub const LC_TIME: c_int = 5;
pub const LC_TIME_MASK: c_int = 32;
pub const LINE_MAX: c_int = 2048;
pub const LITTLE_ENDIAN: c_int = 1234;
pub const LOCK_EX: c_int = 2;
pub const LOCK_NB: c_int = 4;
pub const LOCK_SH: c_int = 1;
pub const LOCK_UN: c_int = 8;
pub const LOG_ALERT: c_int = 1;
pub const LOG_CONS: c_int = 2;
pub const LOG_CRIT: c_int = 2;
pub const LOG_DAEMON: c_int = 24;
pub const LOG_DEBUG: c_int = 7;
pub const LOG_EMERG: c_int = 0;
pub const LOG_ERR: c_int = 3;
pub const LOG_INFO: c_int = 6;
pub const LOG_NDELAY: c_int = 8;
pub const LOG_NOTICE: c_int = 5;
pub const LOG_PID: c_int = 1;
pub const LOG_USER: c_int = 8;
pub const LOG_WARNING: c_int = 4;
pub const MAP_ANONYMOUS: c_int = 32;
pub const MAP_FAILED: *mut c_void = !0 as *mut c_void;
pub const MAP_FIXED: c_int = 16;
pub const MAP_JIT: c_int = 2048;
pub const MAP_PRIVATE: c_int = 2;
pub const MAP_SHARED: c_int = 1;
pub const MAX_CANON: c_int = 255;
pub const MAX_INPUT: c_int = 255;
pub const MCL_CURRENT: c_int = 1;
pub const MCL_FUTURE: c_int = 2;
pub const MINSIGSTKSZ: size_t = 2048;
pub const MSG_DONTROUTE: c_int = 4;
pub const MSG_DONTWAIT: c_int = 64;
pub const MSG_NOSIGNAL: c_int = 16384;
pub const MSG_OOB: c_int = 1;
pub const MSG_PEEK: c_int = 2;
pub const MSG_WAITALL: c_int = 256;
pub const MS_ASYNC: c_int = 1;
pub const MS_INVALIDATE: c_int = 2;
pub const MS_SYNC: c_int = 4;
pub const NAME_MAX: c_int = 255;
pub const NCCS: usize = 11;
pub const NI_MAXHOST: crate::socklen_t = 1025;
pub const NI_MAXSERV: crate::socklen_t = 32;
pub const NI_NUMERICHOST: c_int = 1;
pub const NI_NUMERICSERV: c_int = 2;
pub const NL0: crate::tcflag_t = 0;
pub const NL1: crate::tcflag_t = 128;
pub const NLDLY: crate::tcflag_t = 128;
pub const NOFLSH: crate::tcflag_t = 128;
pub const NO_DATA: c_int = 4;
pub const NO_RECOVERY: c_int = 3;
pub const NSIG: c_int = 32;
pub const OCRNL: crate::tcflag_t = 4;
pub const OFDEL: crate::tcflag_t = 32;
pub const OFILL: crate::tcflag_t = 64;
pub const ONLCR: crate::tcflag_t = 2;
pub const ONLRET: crate::tcflag_t = 16;
pub const ONOCR: crate::tcflag_t = 8;
pub const OPEN_MAX: c_int = 64;
pub const OPOST: crate::tcflag_t = 1;
pub const O_ACCMODE: c_int = 3;
pub const O_APPEND: c_int = 1024;
pub const O_ASYNC: c_int = 8;
pub const O_CLOEXEC: c_int = 262144;
pub const O_CREAT: c_int = 64;
pub const O_DIRECT: c_int = 524288;
pub const O_DIRECTORY: c_int = 2097152;
pub const O_DSYNC: c_int = 8192;
pub const O_EXCL: c_int = 2048;
pub const O_EXEC: c_int = 4194304;
pub const O_NOCTTY: c_int = 32768;
pub const O_NOFOLLOW: c_int = 1048576;
pub const O_NONBLOCK: c_int = 16384;
pub const O_RDONLY: c_int = 0;
pub const O_RDWR: c_int = 2;
pub const O_RSYNC: c_int = 8192;
pub const O_SEARCH: c_int = 4194304;
pub const O_SYNC: c_int = 8192;
pub const O_TRUNC: c_int = 512;
pub const O_WRONLY: c_int = 1;
pub const PARENB: crate::tcflag_t = 64;
pub const PARMRK: crate::tcflag_t = 2048;
pub const PARODD: crate::tcflag_t = 128;
pub const PATH_MAX: c_int = 1024;
pub const PDP_ENDIAN: c_int = 3412;
pub const PF_INET: c_int = 2;
pub const PF_INET6: c_int = 10;
pub const PF_UNIX: c_int = 1;
pub const PF_UNSPEC: c_int = 0;
pub const PIPE_BUF: usize = 512;
pub const POLLERR: c_short = 8;
pub const POLLHUP: c_short = 16;
pub const POLLIN: c_short = 1;
pub const POLLNVAL: c_short = 32;
pub const POLLOUT: c_short = 4;
pub const POLLPRI: c_short = 2;
pub const POSIX_MADV_DONTNEED: c_int = 4;
pub const POSIX_MADV_NORMAL: c_int = 0;
pub const POSIX_MADV_RANDOM: c_int = 1;
pub const POSIX_MADV_SEQUENTIAL: c_int = 2;
pub const POSIX_MADV_WILLNEED: c_int = 3;
pub const PRIO_PGRP: c_int = 1;
pub const PRIO_PROCESS: c_int = 0;
pub const PRIO_USER: c_int = 2;
pub const PROT_EXEC: c_int = 4;
pub const PROT_NONE: c_int = 0;
pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const PTHREAD_CREATE_DETACHED: c_int = 1;
pub const PTHREAD_CREATE_JOINABLE: c_int = 0;
pub const PTHREAD_KEYS_MAX: usize = 128;
pub const PTHREAD_MUTEX_DEFAULT: c_int = 0;
pub const PTHREAD_MUTEX_ERRORCHECK: c_int = 2;
pub const PTHREAD_MUTEX_NORMAL: c_int = 0;
pub const PTHREAD_MUTEX_RECURSIVE: c_int = 1;
pub const PTHREAD_PROCESS_PRIVATE: c_int = 0;
pub const PTHREAD_PROCESS_SHARED: c_int = 1;
pub const PTHREAD_STACK_MIN: size_t = 16384;
pub const RAND_MAX: c_int = 2147483647;
pub const RE_DUP_MAX: c_int = 255;
pub const RLIMIT_AS: c_int = 6;
pub const RLIMIT_CORE: c_int = 4;
pub const RLIMIT_CPU: c_int = 0;
pub const RLIMIT_DATA: c_int = 2;
pub const RLIMIT_FSIZE: c_int = 1;
pub const RLIMIT_NOFILE: c_int = 5;
pub const RLIMIT_STACK: c_int = 3;
pub const RLIM_SAVED_CUR: crate::rlim_t = 9223372036854775807;
// <sys/resource.h> picolibc: RLIM_INFINITY = INT64_MAX (servo-script).
pub const RLIM_INFINITY: crate::rlim_t = 9223372036854775807;
pub const RLIM_SAVED_MAX: crate::rlim_t = 9223372036854775807;
pub const RTLD_DEFAULT: *mut c_void = 0 as *mut c_void;
pub const RTLD_GLOBAL: c_int = 256;
pub const RTLD_LAZY: c_int = 1;
pub const RTLD_LOCAL: c_int = 0;
pub const RTLD_NOW: c_int = 2;
pub const RUSAGE_CHILDREN: c_int = -1;
pub const RUSAGE_SELF: c_int = 0;
pub const R_OK: c_int = 4;
pub const SA_NOCLDSTOP: c_int = 1;
pub const SA_NOCLDWAIT: c_int = 32;
pub const SA_NODEFER: c_int = 64;
pub const SA_ONSTACK: c_int = 2;
pub const SA_RESETHAND: c_int = 4;
pub const SA_RESTART: c_int = 8;
pub const SA_SIGINFO: c_int = 16;
pub const SCHED_FIFO: c_int = 1;
pub const SCHED_OTHER: c_int = 0;
pub const SCHED_RR: c_int = 2;
pub const SEEK_CUR: c_int = 1;
pub const SEEK_DATA: c_int = 3;
pub const SEEK_END: c_int = 2;
pub const SEEK_HOLE: c_int = 4;
pub const SEEK_SET: c_int = 0;
pub const SEM_FAILED: c_int = 0;
pub const SHUT_RD: c_int = 0;
pub const SHUT_RDWR: c_int = 2;
pub const SHUT_WR: c_int = 1;
pub const SIGABRT: c_int = 6;
pub const SIGALRM: c_int = 14;
pub const SIGBUS: c_int = 10;
pub const SIGCHLD: c_int = 20;
pub const SIGCLD: c_int = 20;
pub const SIGCONT: c_int = 19;
pub const SIGEMT: c_int = 7;
pub const SIGEV_NONE: c_int = 1;
pub const SIGEV_SIGNAL: c_int = 2;
pub const SIGEV_THREAD: c_int = 3;
pub const SIGFPE: c_int = 8;
pub const SIGHUP: c_int = 1;
pub const SIGILL: c_int = 4;
pub const SIGINT: c_int = 2;
pub const SIGIO: c_int = 23;
pub const SIGIOT: c_int = 6;
pub const SIGKILL: c_int = 9;
pub const SIGLOST: c_int = 29;
pub const SIGPIPE: c_int = 13;
pub const SIGPOLL: c_int = 23;
pub const SIGPROF: c_int = 27;
pub const SIGQUIT: c_int = 3;
pub const SIGSEGV: c_int = 11;
pub const SIGSTKSZ: size_t = 8192;
pub const SIGSTOP: c_int = 17;
pub const SIGSYS: c_int = 12;
pub const SIGTERM: c_int = 15;
pub const SIGTRAP: c_int = 5;
pub const SIGTSTP: c_int = 18;
pub const SIGTTIN: c_int = 21;
pub const SIGTTOU: c_int = 22;
pub const SIGURG: c_int = 16;
pub const SIGUSR1: c_int = 30;
pub const SIGUSR2: c_int = 31;
pub const SIGVTALRM: c_int = 26;
pub const SIGWINCH: c_int = 28;
pub const SIGXCPU: c_int = 24;
pub const SIGXFSZ: c_int = 25;
pub const SIG_ATOMIC_MAX: c_int = 2147483647;
pub const SIG_ATOMIC_MIN: c_int = -2147483648;
pub const SIG_BLOCK: c_int = 1;
pub const SIG_SETMASK: c_int = 0;
pub const SIG_UNBLOCK: c_int = 2;
pub const SIZE_MAX: usize = (-1i64) as usize;
pub const SI_ASYNCIO: c_int = 4;
pub const SI_MESGQ: c_int = 5;
pub const SI_QUEUE: c_int = 2;
pub const SI_TIMER: c_int = 3;
pub const SI_USER: c_int = 1;
pub const SOCK_CLOEXEC: c_int = 524288;
pub const SOCK_DGRAM: c_int = 2;
pub const SOCK_NONBLOCK: c_int = 2048;
pub const SOCK_RAW: c_int = 3;
pub const SOCK_SEQPACKET: c_int = 5;
pub const SOCK_STREAM: c_int = 1;
pub const SOL_SOCKET: c_int = 1;
pub const SOL_TCP: c_int = 6;
pub const SOMAXCONN: c_int = 128;
pub const SO_ACCEPTCONN: c_int = 30;
pub const SO_BROADCAST: c_int = 6;
pub const SO_DEBUG: c_int = 1;
pub const SO_DONTROUTE: c_int = 5;
pub const SO_ERROR: c_int = 4;
pub const SO_KEEPALIVE: c_int = 9;
pub const SO_LINGER: c_int = 13;
pub const SO_OOBINLINE: c_int = 10;
pub const SO_RCVBUF: c_int = 8;
pub const SO_RCVLOWAT: c_int = 18;
pub const SO_RCVTIMEO: c_int = 20;
pub const SO_REUSEADDR: c_int = 2;
pub const SO_REUSEPORT: c_int = 15;
pub const SO_SNDBUF: c_int = 7;
pub const SO_SNDLOWAT: c_int = 19;
pub const SO_SNDTIMEO: c_int = 21;
pub const SO_TYPE: c_int = 3;
pub const SS_DISABLE: c_int = 2;
pub const SS_ONSTACK: c_int = 1;
pub const S_BLKSIZE: crate::mode_t = 512;
pub const S_ENFMT: crate::mode_t = 1024;
pub const S_IEXEC: crate::mode_t = 64;
pub const S_IFBLK: crate::mode_t = 24576;
pub const S_IFCHR: crate::mode_t = 8192;
pub const S_IFDIR: crate::mode_t = 16384;
pub const S_IFIFO: crate::mode_t = 4096;
pub const S_IFLNK: crate::mode_t = 40960;
pub const S_IFMT: crate::mode_t = 61440;
pub const S_IFREG: crate::mode_t = 32768;
pub const S_IFSOCK: crate::mode_t = 49152;
pub const S_IREAD: crate::mode_t = 256;
pub const S_IRGRP: crate::mode_t = 32;
pub const S_IROTH: crate::mode_t = 4;
pub const S_IRUSR: crate::mode_t = 256;
pub const S_IRWXG: crate::mode_t = 56;
pub const S_IRWXO: crate::mode_t = 7;
pub const S_IRWXU: crate::mode_t = 448;
pub const S_ISGID: crate::mode_t = 1024;
pub const S_ISUID: crate::mode_t = 2048;
pub const S_ISVTX: crate::mode_t = 512;
pub const S_IWGRP: crate::mode_t = 16;
pub const S_IWOTH: crate::mode_t = 2;
pub const S_IWRITE: crate::mode_t = 128;
pub const S_IWUSR: crate::mode_t = 128;
pub const S_IXGRP: crate::mode_t = 8;
pub const S_IXOTH: crate::mode_t = 1;
pub const S_IXUSR: crate::mode_t = 64;
pub const TAB0: crate::tcflag_t = 0;
pub const TAB1: crate::tcflag_t = 1024;
pub const TAB2: crate::tcflag_t = 2048;
pub const TAB3: crate::tcflag_t = 3072;
pub const TABDLY: crate::tcflag_t = 3072;
pub const TCIFLUSH: c_int = 1;
pub const TCIOFF: c_int = 0;
pub const TCIOFLUSH: c_int = 3;
pub const TCION: c_int = 1;
pub const TCOFLUSH: c_int = 2;
pub const TCOOFF: c_int = 2;
pub const TCOON: c_int = 3;
pub const TCP_KEEPCNT: c_int = 6;
pub const TCP_KEEPIDLE: c_int = 4;
pub const TCP_KEEPINTVL: c_int = 5;
pub const TCP_MAXSEG: c_int = 2;
pub const TCP_NODELAY: c_int = 1;
pub const TCSADRAIN: c_int = 1;
pub const TCSAFLUSH: c_int = 2;
pub const TCSANOW: c_int = 0;
pub const TIMER_ABSTIME: c_int = 4;
pub const TIME_UTC: c_int = 1;
pub const TIOCGWINSZ: c_int = 21505;
pub const TIOCLINUX: c_int = 21507;
pub const TIOCSWINSZ: c_int = 21506;
pub const TMP_MAX: c_uint = 32;
pub const TOSTOP: crate::tcflag_t = 256;
pub const TRY_AGAIN: c_int = 2;
pub const UINT16_MAX: u16 = 65535;
pub const UINT32_MAX: u32 = 4294967295;
pub const UINT8_MAX: u8 = 255;
pub const UINTPTR_MAX: usize = (-1i64) as usize;
pub const UINT_FAST16_MAX: c_int = 65535;
pub const UINT_FAST8_MAX: c_int = 255;
pub const UINT_LEAST16_MAX: c_int = 65535;
pub const UINT_LEAST8_MAX: c_int = 255;
pub const VEOF: usize = 0;
pub const VEOL: usize = 1;
pub const VERASE: usize = 2;
pub const VINTR: usize = 3;
pub const VKILL: usize = 4;
pub const VMIN: usize = 5;
pub const VQUIT: usize = 6;
pub const VSTART: usize = 7;
pub const VSTOP: usize = 8;
pub const VSUSP: usize = 9;
pub const VT0: crate::tcflag_t = 0;
pub const VT1: crate::tcflag_t = 8192;
pub const VTDLY: crate::tcflag_t = 8192;
pub const VTIME: usize = 10;
pub const WNOHANG: c_int = 1;
pub const WUNTRACED: c_int = 2;
pub const W_OK: c_int = 2;
pub const X_OK: c_int = 1;
pub const _CS_LFS_CFLAGS: c_int = 21;
pub const _CS_LFS_LDFLAGS: c_int = 22;
pub const _CS_LFS_LIBS: c_int = 23;
pub const _CS_LFS_LINTFLAGS: c_int = 24;
pub const _CS_PATH: c_int = 0;
pub const _CS_POSIX_V6_ILP32_OFF32_CFLAGS: c_int = 1;
pub const _CS_POSIX_V6_ILP32_OFF32_LDFLAGS: c_int = 2;
pub const _CS_POSIX_V6_ILP32_OFF32_LIBS: c_int = 3;
pub const _CS_POSIX_V6_ILP32_OFFBIG_CFLAGS: c_int = 5;
pub const _CS_POSIX_V6_ILP32_OFFBIG_LDFLAGS: c_int = 6;
pub const _CS_POSIX_V6_ILP32_OFFBIG_LIBS: c_int = 7;
pub const _CS_POSIX_V6_LP64_OFF64_CFLAGS: c_int = 9;
pub const _CS_POSIX_V6_LP64_OFF64_LDFLAGS: c_int = 10;
pub const _CS_POSIX_V6_LP64_OFF64_LIBS: c_int = 11;
pub const _CS_POSIX_V6_LPBIG_OFFBIG_CFLAGS: c_int = 13;
pub const _CS_POSIX_V6_LPBIG_OFFBIG_LDFLAGS: c_int = 14;
pub const _CS_POSIX_V6_LPBIG_OFFBIG_LIBS: c_int = 15;
pub const _CS_POSIX_V6_WIDTH_RESTRICTED_ENVS: c_int = 17;
pub const _CS_POSIX_V7_ILP32_OFF32_CFLAGS: c_int = 1;
pub const _CS_POSIX_V7_ILP32_OFF32_LDFLAGS: c_int = 2;
pub const _CS_POSIX_V7_ILP32_OFF32_LIBS: c_int = 3;
pub const _CS_POSIX_V7_ILP32_OFFBIG_CFLAGS: c_int = 5;
pub const _CS_POSIX_V7_ILP32_OFFBIG_LDFLAGS: c_int = 6;
pub const _CS_POSIX_V7_ILP32_OFFBIG_LIBS: c_int = 7;
pub const _CS_POSIX_V7_LP64_OFF64_CFLAGS: c_int = 9;
pub const _CS_POSIX_V7_LP64_OFF64_LDFLAGS: c_int = 10;
pub const _CS_POSIX_V7_LP64_OFF64_LIBS: c_int = 11;
pub const _CS_POSIX_V7_LPBIG_OFFBIG_CFLAGS: c_int = 13;
pub const _CS_POSIX_V7_LPBIG_OFFBIG_LDFLAGS: c_int = 14;
pub const _CS_POSIX_V7_LPBIG_OFFBIG_LIBS: c_int = 15;
pub const _CS_POSIX_V7_WIDTH_RESTRICTED_ENVS: c_int = 17;
pub const _CS_V6_ENV: c_int = 20;
pub const _CS_V7_ENV: c_int = 20;
pub const _CS_XBS5_ILP32_OFF32_CFLAGS: c_int = 1;
pub const _CS_XBS5_ILP32_OFF32_LDFLAGS: c_int = 2;
pub const _CS_XBS5_ILP32_OFF32_LIBS: c_int = 3;
pub const _CS_XBS5_ILP32_OFF32_LINTFLAGS: c_int = 4;
pub const _CS_XBS5_ILP32_OFFBIG_CFLAGS: c_int = 5;
pub const _CS_XBS5_ILP32_OFFBIG_LDFLAGS: c_int = 6;
pub const _CS_XBS5_ILP32_OFFBIG_LIBS: c_int = 7;
pub const _CS_XBS5_ILP32_OFFBIG_LINTFLAGS: c_int = 8;
pub const _CS_XBS5_LP64_OFF64_CFLAGS: c_int = 9;
pub const _CS_XBS5_LP64_OFF64_LDFLAGS: c_int = 10;
pub const _CS_XBS5_LP64_OFF64_LIBS: c_int = 11;
pub const _CS_XBS5_LP64_OFF64_LINTFLAGS: c_int = 12;
pub const _CS_XBS5_LPBIG_OFFBIG_CFLAGS: c_int = 13;
pub const _CS_XBS5_LPBIG_OFFBIG_LDFLAGS: c_int = 14;
pub const _CS_XBS5_LPBIG_OFFBIG_LIBS: c_int = 15;
pub const _CS_XBS5_LPBIG_OFFBIG_LINTFLAGS: c_int = 16;
pub const _IOFBF: c_int = 0;
pub const _IOLBF: c_int = 1;
pub const _IONBF: c_int = 2;
pub const _NSIG: usize = 32;
pub const _PC_2_SYMLINKS: c_int = 7;
pub const _PC_ALLOC_SIZE_MIN: c_int = 8;
pub const _PC_ASYNC_IO: c_int = 18;
pub const _PC_CHOWN_RESTRICTED: c_int = 15;
pub const _PC_FILESIZEBITS: c_int = 0;
pub const _PC_LINK_MAX: c_int = 1;
pub const _PC_MAX_CANON: c_int = 2;
pub const _PC_MAX_INPUT: c_int = 3;
pub const _PC_NAME_MAX: c_int = 4;
pub const _PC_NO_TRUNC: c_int = 16;
pub const _PC_PATH_MAX: c_int = 5;
pub const _PC_PIPE_BUF: c_int = 6;
pub const _PC_PRIO_IO: c_int = 20;
pub const _PC_REC_INCR_XFER_SIZE: c_int = 9;
pub const _PC_REC_MAX_XFER_SIZE: c_int = 10;
pub const _PC_REC_MIN_XFER_SIZE: c_int = 11;
pub const _PC_REC_XFER_ALIGN: c_int = 12;
pub const _PC_SYMLINK_MAX: c_int = 13;
pub const _PC_SYNC_IO: c_int = 21;
pub const _PC_TIMESTAMP_RESOLUTION: c_int = 22;
pub const _PC_VDISABLE: c_int = 17;
pub const _POSIX2_BC_BASE_MAX: c_int = 99;
pub const _POSIX2_BC_DIM_MAX: c_int = 2048;
pub const _POSIX2_BC_SCALE_MAX: c_int = 99;
pub const _POSIX2_BC_STRING_MAX: c_int = 1000;
pub const _POSIX2_CHARCLASS_NAME_MAX: c_int = 14;
pub const _POSIX2_COLL_WEIGHTS_MAX: c_int = 2;
pub const _POSIX2_EXPR_NEST_MAX: c_int = 32;
pub const _POSIX2_LINE_MAX: c_int = 2048;
pub const _POSIX2_RE_DUP_MAX: c_int = 255;
pub const _POSIX_AIO_LISTIO_MAX: usize = 2;
pub const _POSIX_AIO_MAX: usize = 1;
pub const _POSIX_ARG_MAX: c_int = 4096;
pub const _POSIX_CHILD_MAX: usize = 6;
pub const _POSIX_DELAYTIMER_MAX: usize = 32;
pub const _POSIX_HOST_NAME_MAX: usize = 255;
pub const _POSIX_LINK_MAX: c_int = 8;
pub const _POSIX_LOGIN_NAME_MAX: usize = 9;
pub const _POSIX_MAX_CANON: c_int = 255;
pub const _POSIX_MAX_INPUT: c_int = 255;
pub const _POSIX_MQ_OPEN_MAX: usize = 8;
pub const _POSIX_MQ_PRIO_MAX: usize = 32;
pub const _POSIX_NAME_MAX: c_int = 14;
pub const _POSIX_NGROUPS_MAX: usize = 0;
pub const _POSIX_OPEN_MAX: usize = 16;
pub const _POSIX_PATH_MAX: usize = 255;
pub const _POSIX_PIPE_BUF: c_int = 512;
pub const _POSIX_RE_DUP_MAX: usize = 255;
pub const _POSIX_RTSIG_MAX: usize = 8;
pub const _POSIX_SEM_NSEMS_MAX: usize = 256;
pub const _POSIX_SEM_VALUE_MAX: usize = 32767;
pub const _POSIX_SIGQUEUE_MAX: usize = 32;
pub const _POSIX_SSIZE_MAX: c_int = 32767;
pub const _POSIX_STREAM_MAX: c_int = 8;
pub const _POSIX_SYMLINK_MAX: usize = 255;
pub const _POSIX_SYMLOOP_MAX: usize = 8;
pub const _POSIX_THREAD_DESTRUCTOR_ITERATIONS: usize = 4;
pub const _POSIX_THREAD_KEYS_MAX: usize = 128;
pub const _POSIX_THREAD_THREADS_MAX: usize = 64;
pub const _POSIX_TIMER_MAX: usize = 32;
pub const _POSIX_TTY_NAME_MAX: usize = 9;
pub const _POSIX_TZNAME_MAX: usize = 3;
pub const _SC_2_CHAR_TERM: c_int = 104;
pub const _SC_2_C_BIND: c_int = 105;
pub const _SC_2_C_DEV: c_int = 106;
pub const _SC_2_FORT_DEV: c_int = 107;
pub const _SC_2_FORT_RUN: c_int = 108;
pub const _SC_2_LOCALEDEF: c_int = 109;
pub const _SC_2_PBS: c_int = 110;
pub const _SC_2_PBS_ACCOUNTING: c_int = 111;
pub const _SC_2_PBS_CHECKPOINT: c_int = 112;
pub const _SC_2_PBS_LOCATE: c_int = 113;
pub const _SC_2_PBS_MESSAGE: c_int = 114;
pub const _SC_2_PBS_TRACK: c_int = 115;
pub const _SC_2_SW_DEV: c_int = 116;
pub const _SC_2_UPE: c_int = 117;
pub const _SC_2_VERSION: c_int = 118;
pub const _SC_ADVISORY_INFO: c_int = 50;
pub const _SC_AIO_LISTIO_MAX: c_int = 30;
pub const _SC_AIO_MAX: c_int = 31;
pub const _SC_AIO_PRIO_DELTA_MAX: c_int = 32;
pub const _SC_ARG_MAX: c_int = 0;
pub const _SC_ASYNCHRONOUS_IO: c_int = 17;
pub const _SC_ATEXIT_MAX: c_int = 51;
pub const _SC_AVPHYS_PAGES: c_int = 141;
pub const _SC_BARRIERS: c_int = 52;
pub const _SC_BC_BASE_MAX: c_int = 53;
pub const _SC_BC_DIM_MAX: c_int = 54;
pub const _SC_BC_SCALE_MAX: c_int = 55;
pub const _SC_BC_STRING_MAX: c_int = 56;
pub const _SC_CHARCLASS_NAME_MAX: c_int = 57;
pub const _SC_CHILD_MAX: c_int = 1;
pub const _SC_CLK_TCK: c_int = 2;
pub const _SC_CLOCK_SELECTION: c_int = 58;
pub const _SC_COLL_WEIGHTS_MAX: c_int = 59;
pub const _SC_CPUTIME: c_int = 60;
pub const _SC_DELAYTIMER_MAX: c_int = 33;
pub const _SC_EXPR_NEST_MAX: c_int = 61;
pub const _SC_FSYNC: c_int = 18;
pub const _SC_GETGR_R_SIZE_MAX: c_int = 46;
pub const _SC_GETPW_R_SIZE_MAX: c_int = 47;
pub const _SC_HOST_NAME_MAX: c_int = 62;
pub const _SC_IOV_MAX: c_int = 63;
pub const _SC_IPV6: c_int = 64;
pub const _SC_JOB_CONTROL: c_int = 5;
pub const _SC_LEVEL1_DCACHE_ASSOC: c_int = 126;
pub const _SC_LEVEL1_DCACHE_LINESIZE: c_int = 127;
pub const _SC_LEVEL1_DCACHE_SIZE: c_int = 125;
pub const _SC_LEVEL1_ICACHE_ASSOC: c_int = 123;
pub const _SC_LEVEL1_ICACHE_LINESIZE: c_int = 124;
pub const _SC_LEVEL1_ICACHE_SIZE: c_int = 122;
pub const _SC_LEVEL2_CACHE_ASSOC: c_int = 129;
pub const _SC_LEVEL2_CACHE_LINESIZE: c_int = 130;
pub const _SC_LEVEL2_CACHE_SIZE: c_int = 128;
pub const _SC_LEVEL3_CACHE_ASSOC: c_int = 132;
pub const _SC_LEVEL3_CACHE_LINESIZE: c_int = 133;
pub const _SC_LEVEL3_CACHE_SIZE: c_int = 131;
pub const _SC_LEVEL4_CACHE_ASSOC: c_int = 135;
pub const _SC_LEVEL4_CACHE_LINESIZE: c_int = 136;
pub const _SC_LEVEL4_CACHE_SIZE: c_int = 134;
pub const _SC_LINE_MAX: c_int = 65;
pub const _SC_LOGIN_NAME_MAX: c_int = 48;
pub const _SC_MAPPED_FILES: c_int = 19;
pub const _SC_MEMLOCK: c_int = 20;
pub const _SC_MEMLOCK_RANGE: c_int = 21;
pub const _SC_MEMORY_PROTECTION: c_int = 22;
pub const _SC_MESSAGE_PASSING: c_int = 23;
pub const _SC_MONOTONIC_CLOCK: c_int = 66;
pub const _SC_MQ_OPEN_MAX: c_int = 9;
pub const _SC_MQ_PRIO_MAX: c_int = 10;
pub const _SC_NGROUPS_MAX: c_int = 3;
pub const _SC_NPROCESSORS_CONF: c_int = 138;
pub const _SC_NPROCESSORS_ONLN: c_int = 139;
pub const _SC_OPEN_MAX: c_int = 4;
pub const _SC_PAGESIZE: c_int = 8;
pub const _SC_PAGE_SIZE: c_int = 8;
pub const _SC_PHYS_PAGES: c_int = 140;
pub const _SC_PRIORITIZED_IO: c_int = 24;
pub const _SC_PRIORITY_SCHEDULING: c_int = 98;
pub const _SC_RAW_SOCKETS: c_int = 67;
pub const _SC_READER_WRITER_LOCKS: c_int = 68;
pub const _SC_REALTIME_SIGNALS: c_int = 25;
pub const _SC_REGEXP: c_int = 69;
pub const _SC_RE_DUP_MAX: c_int = 70;
pub const _SC_RTSIG_MAX: c_int = 11;
pub const _SC_SAVED_IDS: c_int = 6;
pub const _SC_SEMAPHORES: c_int = 26;
pub const _SC_SEM_NSEMS_MAX: c_int = 12;
pub const _SC_SEM_VALUE_MAX: c_int = 13;
pub const _SC_SHARED_MEMORY_OBJECTS: c_int = 27;
pub const _SC_SHELL: c_int = 71;
pub const _SC_SIGQUEUE_MAX: c_int = 14;
pub const _SC_SPAWN: c_int = 72;
pub const _SC_SPIN_LOCKS: c_int = 73;
pub const _SC_SPORADIC_SERVER: c_int = 74;
pub const _SC_SS_REPL_MAX: c_int = 75;
pub const _SC_STREAM_MAX: c_int = 97;
pub const _SC_SYMLOOP_MAX: c_int = 76;
pub const _SC_SYNCHRONIZED_IO: c_int = 28;
pub const _SC_THREADS: c_int = 38;
pub const _SC_THREAD_ATTR_STACKADDR: c_int = 39;
pub const _SC_THREAD_ATTR_STACKSIZE: c_int = 40;
pub const _SC_THREAD_CPUTIME: c_int = 77;
pub const _SC_THREAD_DESTRUCTOR_ITERATIONS: c_int = 49;
pub const _SC_THREAD_KEYS_MAX: c_int = 34;
pub const _SC_THREAD_PRIORITY_SCHEDULING: c_int = 41;
pub const _SC_THREAD_PRIO_CEILING: c_int = 43;
pub const _SC_THREAD_PRIO_INHERIT: c_int = 42;
pub const _SC_THREAD_PRIO_PROTECT: c_int = 43;
pub const _SC_THREAD_PROCESS_SHARED: c_int = 44;
pub const _SC_THREAD_ROBUST_PRIO_INHERIT: c_int = 119;
pub const _SC_THREAD_ROBUST_PRIO_PROTECT: c_int = 120;
pub const _SC_THREAD_SAFE_FUNCTIONS: c_int = 45;
pub const _SC_THREAD_SPORADIC_SERVER: c_int = 78;
pub const _SC_THREAD_STACK_MIN: c_int = 35;
pub const _SC_THREAD_THREADS_MAX: c_int = 36;
pub const _SC_TIMEOUTS: c_int = 79;
pub const _SC_TIMERS: c_int = 29;
pub const _SC_TIMER_MAX: c_int = 15;
pub const _SC_TRACE: c_int = 80;
pub const _SC_TRACE_EVENT_FILTER: c_int = 81;
pub const _SC_TRACE_EVENT_NAME_MAX: c_int = 82;
pub const _SC_TRACE_INHERIT: c_int = 83;
pub const _SC_TRACE_LOG: c_int = 84;
pub const _SC_TRACE_NAME_MAX: c_int = 85;
pub const _SC_TRACE_SYS_MAX: c_int = 86;
pub const _SC_TRACE_USER_EVENT_MAX: c_int = 87;
pub const _SC_TTY_NAME_MAX: c_int = 37;
pub const _SC_TYPED_MEMORY_OBJECTS: c_int = 88;
pub const _SC_TZNAME_MAX: c_int = 16;
pub const _SC_V6_ILP32_OFF32: c_int = 89;
pub const _SC_V6_ILP32_OFFBIG: c_int = 90;
pub const _SC_V6_LP64_OFF64: c_int = 91;
pub const _SC_V6_LPBIG_OFFBIG: c_int = 92;
pub const _SC_V7_ILP32_OFF32: c_int = 89;
pub const _SC_V7_ILP32_OFFBIG: c_int = 90;
pub const _SC_V7_LP64_OFF64: c_int = 91;
pub const _SC_V7_LPBIG_OFFBIG: c_int = 92;
pub const _SC_VERSION: c_int = 7;
pub const _SC_XBS5_ILP32_OFF32: c_int = 89;
pub const _SC_XBS5_ILP32_OFFBIG: c_int = 90;
pub const _SC_XBS5_LP64_OFF64: c_int = 91;
pub const _SC_XBS5_LPBIG_OFFBIG: c_int = 92;
pub const _SC_XOPEN_CRYPT: c_int = 93;
pub const _SC_XOPEN_ENH_I18N: c_int = 94;
pub const _SC_XOPEN_LEGACY: c_int = 95;
pub const _SC_XOPEN_REALTIME: c_int = 96;
pub const _SC_XOPEN_REALTIME_THREADS: c_int = 99;
pub const _SC_XOPEN_SHM: c_int = 100;
pub const _SC_XOPEN_STREAMS: c_int = 101;
pub const _SC_XOPEN_UNIX: c_int = 102;
pub const _SC_XOPEN_UUCP: c_int = 121;
pub const _SC_XOPEN_VERSION: c_int = 103;

// ── Память: mprotect/msync/madvise (N5, 08.11.2026) ─────────────────────────
// Нужны memmap2 (Servo: шрифты, кеш). Функции — libc/freeos (posix.c,
// syscalls.c), значения — libc/freeos/include/sys/mman.h.
extern "C" {
    pub fn mprotect(addr: *mut c_void, len: size_t, prot: c_int) -> c_int;
    pub fn msync(addr: *mut c_void, len: size_t, flags: c_int) -> c_int;
    pub fn madvise(addr: *mut c_void, len: size_t, advice: c_int) -> c_int;
    pub fn posix_madvise(addr: *mut c_void, len: size_t, advice: c_int) -> c_int;
    // picolibc <malloc.h>; servo-allocator (учёт памяти), как у Linux.
    pub fn malloc_usable_size(ptr: *mut c_void) -> size_t;
}

// rustix (ветки Redox, 08.11.2026): renameat2 — libc/freeos/files.c (флаги —
// EINVAL); номер устройства — как <sys/stat.h> picolibc: старшая половина
// dev_t (64 бита) — major, младшая — minor.
extern "C" {
    pub fn renameat2(olddirfd: c_int, oldpath: *const c_char, newdirfd: c_int, newpath: *const c_char, flags: c_uint) -> c_int;
}
pub const RENAME_NOREPLACE: c_uint = 1;
pub const RENAME_EXCHANGE: c_uint = 2;
pub const RENAME_WHITEOUT: c_uint = 4;
pub const fn makedev(major: c_uint, minor: c_uint) -> dev_t {
    ((major as dev_t) << 32) | minor as dev_t
}
pub const fn major(dev: dev_t) -> c_uint {
    (dev >> 32) as c_uint
}
pub const fn minor(dev: dev_t) -> c_uint {
    (dev & 0xffff_ffff) as c_uint
}
pub const MAP_ANON: c_int = MAP_ANONYMOUS;
pub const MADV_NORMAL: c_int = 0;
pub const MADV_RANDOM: c_int = 1;
pub const MADV_SEQUENTIAL: c_int = 2;
pub const MADV_WILLNEED: c_int = 3;
pub const MADV_DONTNEED: c_int = 4;
pub const MADV_FREE: c_int = 8;
pub const MADV_REMOVE: c_int = 9;
pub const MADV_DONTFORK: c_int = 10;
pub const MADV_DOFORK: c_int = 11;
pub const MADV_MERGEABLE: c_int = 12;
pub const MADV_UNMERGEABLE: c_int = 13;
pub const MADV_HUGEPAGE: c_int = 14;
pub const MADV_NOHUGEPAGE: c_int = 15;
pub const MADV_DONTDUMP: c_int = 16;
pub const MADV_DODUMP: c_int = 17;
pub const MADV_WIPEONFORK: c_int = 18;
pub const MADV_KEEPONFORK: c_int = 19;
pub const MADV_COLD: c_int = 20;
pub const MADV_PAGEOUT: c_int = 21;
pub const MADV_POPULATE_READ: c_int = 22;
pub const MADV_POPULATE_WRITE: c_int = 23;
