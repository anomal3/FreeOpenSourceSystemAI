//! Файловые вызовы, которых потребовал Chromium (фаза 71h): каталоги,
//! переименование, запись с места, `fallocate`, и несколько ответов «как у
//! Linux» на вопросы, у которых в системе ответа нет (`flock`, `fsync`,
//! права, время).

use alloc::string::String;

use super::{AT_FDCWD, e, file_errno, linux_path, vfs_errno};
use crate::mm::PageFlags;
use crate::user::space;

/// Куда в системе ляжет **новый** путь Linux `linux`: в корень Debian, если
/// там есть его родительский каталог, иначе — как есть. `linux_path::resolve`
/// для этого не годится: он смотрит, есть ли сам путь, а его ещё нет.
pub(super) fn place(linux: &str) -> String {
    let cred = crate::user::credentials();
    // Путь уже есть — там, где он есть (22.11, hw-log15): `mkdir -p
    // /root/.proton` делает и `mkdir /root`, родитель `/` есть в корне Debian,
    // и вместо EEXIST появлялся пустой двойник `/opt/debian/root`. С ним
    // `resolve` уводил весь дом в корень Debian: префикс Wine лёг в
    // `/opt/debian/root/.proton`, а стол искал его в `/root` и не видел
    // программ Windows («Пуск», ярлык на столе).
    let existing = crate::linux_path::resolve(linux, cred);
    if matches!(crate::fs::resolve_as(cred, &existing, crate::vfs::perm::Access::NONE), Some(Ok(_))) {
        return existing;
    }
    let (parent, name) = match linux.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => return String::from(linux),
    };
    // Ссылки посреди пути — пройти (07.11): Wine создаёт каталоги через
    // `dosdevices/c:` → `../drive_c`, и `mkdir` по такому пути отвечал
    // `ENOENT` («Failed to create directory C:\\Program Files»). Последний
    // компонент не трогается — как у Linux для `mkdir`, `unlink`, `rename`.
    let parent = super::links::follow(parent);
    let mut out = crate::linux_path::resolve(&parent, cred);
    if !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(name);
    out
}

/// `chmod`/`fchmodat` (фаза 74; до неё — «успех» без дела). Rider
/// распаковывает свои программы и ставит им `+x`; без этого их запуск
/// получал `EACCES`, и Rider не поднимал свою половину на .NET.
pub fn chmod(dir: usize, ptr: usize, mode: usize) -> i64 {
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let cred = crate::user::credentials();
    let path = crate::linux_path::resolve(&linux, cred);
    match crate::fs::chmod_as(cred, &path, (mode & 0o7777) as u16) {
        Some(Ok(())) => 0,
        // Тому, что не умеет права (FAT, корень только для чтения), — как раньше.
        Some(Err(crate::vfs::VfsError::Unsupported | crate::vfs::VfsError::ReadOnly)) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `fchmod`.
pub fn fchmod(fd: usize, mode: usize) -> i64 {
    let node = match crate::user::with_current(|program| program.files.file_or_dir(fd)) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return file_errno(err),
        None => return -e::ESRCH,
    };
    match crate::fs::chmod_node(crate::user::credentials(), &*node, (mode & 0o7777) as u16) {
        Ok(()) | Err(crate::vfs::VfsError::Unsupported | crate::vfs::VfsError::ReadOnly) => 0,
        Err(err) => vfs_errno(err),
    }
}

/// `utimensat`/`futimens` (27.11; before — «success» doing nothing): `cp -p`,
/// `tar`, `rsync`, `touch`, 7-Zip and Wine set the times of files they make.
/// `times` — two `timespec` (access, modification) or 0 for «now»; a
/// `tv_nsec` of `UTIME_NOW`/`UTIME_OMIT` — now / keep. The file systems keep
/// whole seconds (`vfs::Times`), the nanoseconds are dropped. A null path
/// (glibc `futimens`) or `AT_EMPTY_PATH` — the descriptor `dir` itself.
pub fn utimensat(dir: usize, ptr: usize, times: usize, flags: usize) -> i64 {
    const UTIME_NOW: i64 = (1 << 30) - 1;
    const UTIME_OMIT: i64 = (1 << 30) - 2;
    const AT_SYMLINK_NOFOLLOW: usize = 0x100;
    const AT_EMPTY_PATH: usize = 0x1000;
    let wanted: [[i64; 2]; 2] = if times == 0 {
        [[0, UTIME_NOW], [0, UTIME_NOW]]
    } else {
        if !space::user_can(times, 32, PageFlags::READ) {
            return -e::EFAULT;
        }
        // SAFETY: 32 readable bytes of the program (checked above).
        unsafe { core::ptr::read_unaligned(times as *const [[i64; 2]; 2]) }
    };
    let now = crate::time::now_unix_u32();
    let mut picked = [None; 2];
    for (slot, [seconds, nanos]) in picked.iter_mut().zip(wanted) {
        *slot = match nanos {
            UTIME_OMIT => None,
            UTIME_NOW => Some(now),
            nanos if !(0..1_000_000_000).contains(&nanos) => return -e::EINVAL,
            // 32-bit seconds on disk (ext2): before 1970 or after 2106 clamped.
            _ => Some(seconds.clamp(0, i64::from(u32::MAX)) as u32),
        };
    }
    let [atime, mtime] = picked;
    if atime.is_none() && mtime.is_none() {
        return 0;
    }
    let cred = crate::user::credentials();
    let by_fd = ptr == 0 || (flags & AT_EMPTY_PATH != 0 && space::user_can(ptr, 1, PageFlags::READ) && {
        // SAFETY: one readable byte (checked just before).
        unsafe { core::ptr::read(ptr as *const u8) == 0 }
    });
    let node = if by_fd {
        if dir == AT_FDCWD {
            return -e::EFAULT;
        }
        match crate::user::with_current(|program| program.files.file_or_dir(dir)) {
            Some(Ok(node)) => node,
            Some(Err(err)) => return file_errno(err),
            None => return -e::ESRCH,
        }
    } else {
        let linux = match linux_path(dir, ptr) {
            Ok(path) => path,
            Err(err) => return err,
        };
        // A link of the layer keeps no times of its own: `lutimes` on it is
        // a quiet success, as on file systems without link times.
        if flags & AT_SYMLINK_NOFOLLOW != 0 && super::links::target(&linux).is_some() {
            return 0;
        }
        let linux = super::links::follow(&linux);
        let path = crate::linux_path::resolve(&linux, cred);
        match crate::fs::resolve_as(cred, &path, crate::vfs::perm::Access::NONE) {
            Some(Ok(node)) => alloc::sync::Arc::from(node),
            Some(Err(err)) => return vfs_errno(err),
            None => return -e::ENOENT,
        }
    };
    match crate::fs::set_times_node(cred, &*node, atime, mtime) {
        Ok(()) => 0,
        // No stored times (FAT read-only, special files) or a read-only
        // root — as before, `chmod` answers the same.
        Err(crate::vfs::VfsError::Unsupported | crate::vfs::VfsError::ReadOnly) => 0,
        Err(err) => vfs_errno(err),
    }
}

/// `mkdir`/`mkdirat`.
pub fn mkdir(dir: usize, ptr: usize, mode: usize) -> i64 {
    let linux = match linux_path(dir, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let path = place(&linux);
    match crate::fs::mkdir_as(crate::user::credentials(), &path, (mode as u16) & 0o777) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `rename`/`renameat`/`renameat2` (флаги `renameat2` — кроме нуля — отказ).
pub fn rename(old_dir: usize, old: usize, new_dir: usize, new: usize, flags: usize) -> i64 {
    if flags != 0 {
        return -e::EINVAL;
    }
    let (from, to) = match (linux_path(old_dir, old), linux_path(new_dir, new)) {
        (Ok(from), Ok(to)) => (from, to),
        (Err(err), _) | (_, Err(err)) => return err,
    };
    if super::links::rename(&from, &to) {
        return 0;
    }
    let cred = crate::user::credentials();
    let from = crate::linux_path::resolve(&from, cred);
    let existing = crate::linux_path::resolve(&to, cred);
    // Цель уже есть — Linux заменяет её; наше переименование отказывает,
    // поэтому сначала снимаем её (файл — не каталог).
    if matches!(crate::fs::resolve_as(cred, &existing, crate::vfs::perm::Access::NONE), Some(Ok(_))) && existing != from {
        let _ = crate::fs::remove_as(cred, &existing);
    }
    let to = place(&to);
    match crate::fs::rename_as(cred, &from, &to) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `rmdir`.
pub fn rmdir(ptr: usize) -> i64 {
    let linux = match linux_path(AT_FDCWD, ptr) {
        Ok(path) => path,
        Err(err) => return err,
    };
    let cred = crate::user::credentials();
    let path = crate::linux_path::resolve(&linux, cred);
    match crate::fs::remove_as(cred, &path) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => -e::ENOENT,
    }
}

/// `pwrite64(fd, buf, count, offset)` — запись с места, не двигая позицию.
pub fn pwrite(fd: usize, buf: usize, count: usize, offset: usize) -> i64 {
    const CHUNK: usize = 64 * 1024;
    if count == 0 {
        return 0;
    }
    if !space::user_can(buf, count, PageFlags::READ) {
        return -e::EFAULT;
    }
    let node = match crate::user::with_current(|program| program.files.node(fd)) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return file_errno(err),
        None => return -e::ESRCH,
    };
    let mut chunk = alloc::vec::Vec::new();
    if chunk.try_reserve_exact(CHUNK.min(count)).is_err() {
        return -e::ENOMEM;
    }
    let mut done = 0;
    while done < count {
        let take = (count - done).min(CHUNK);
        chunk.clear();
        // SAFETY: весь буфер программы проверен на чтение выше.
        chunk.extend_from_slice(unsafe { core::slice::from_raw_parts((buf + done) as *const u8, take) });
        match node.write_at((offset + done) as u64, &chunk) {
            Ok(0) => break,
            Ok(wrote) => done += wrote,
            Err(err) if done == 0 => return vfs_errno(err),
            Err(_) => break,
        }
    }
    done as i64
}

/// `fallocate(fd, mode, offset, len)`: место под файл — то есть длина не
/// меньше `offset + len` (режим 0). Дыры и прочие режимы — `EOPNOTSUPP`, как
/// у файловой системы, которая их не умеет.
pub fn fallocate(fd: usize, mode: usize, offset: usize, len: usize) -> i64 {
    const EOPNOTSUPP: i64 = 95;
    const FALLOC_FL_KEEP_SIZE: usize = 1;
    if mode & !FALLOC_FL_KEEP_SIZE != 0 {
        return -EOPNOTSUPP;
    }
    let node = match crate::user::with_current(|program| program.files.node(fd)) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return file_errno(err),
        None => return -e::ESRCH,
    };
    let want = (offset + len) as u64;
    if mode & FALLOC_FL_KEEP_SIZE != 0 || node.metadata().size >= want {
        return 0;
    }
    match node.truncate(want) {
        Ok(()) => 0,
        Err(err) => vfs_errno(err),
    }
}

/// `statfs(path, buf)` / `fstatfs(fd, buf)`: `struct statfs` x86-64, 120 байт.
/// `/dev/shm` и разделяемая память — tmpfs, остальное — ext2. Числа места —
/// правдоподобные, а не сосчитанные: спрашивают их, чтобы решить, где
/// держать, а не сколько.
pub fn statfs(path: Option<usize>, fd: usize, out: usize) -> i64 {
    const TMPFS_MAGIC: u64 = 0x0102_1994;
    const EXT2_MAGIC: u64 = 0xEF53;
    // sysfs (24.11.2026, gamepads): libudev refuses a device whose directory
    // is not on sysfs (`fd_is_fs_type(fd, SYSFS_MAGIC)` in `device_set_syspath`).
    const SYSFS_MAGIC: u64 = 0x6265_6572;
    let in_sys = |linux: &str| linux == "/sys" || linux.starts_with("/sys/");
    let (tmpfs, sysfs) = match path {
        Some(ptr) => match linux_path(AT_FDCWD, ptr) {
            Ok(linux) => (linux == "/dev/shm" || linux.starts_with("/dev/shm/"), in_sys(&linux)),
            Err(err) => return err,
        },
        None => {
            let sysfs = super::dir_path(fd).is_some_and(|linux| in_sys(&linux));
            match crate::user::with_current(|program| program.files.file_or_dir(fd)) {
                Some(Ok(node)) => (node.shared_memory().is_some(), sysfs),
                Some(Err(err)) => return file_errno(err),
                None => return -e::ESRCH,
            }
        }
    };
    if !space::user_can(out, 120, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let frames = crate::mm::frame::stats();
    let blocks = frames.total as u64;
    let free = frames.free as u64;
    // type, bsize, blocks, bfree, bavail, files, ffree, fsid, namelen, frsize,
    // flags, spare[4].
    let fields: [u64; 15] = [
        if tmpfs {
            TMPFS_MAGIC
        } else if sysfs {
            SYSFS_MAGIC
        } else {
            EXT2_MAGIC
        },
        4096,
        blocks,
        free,
        free,
        1 << 20,
        1 << 19,
        0,
        255,
        4096,
        0,
        0,
        0,
        0,
        0,
    ];
    // SAFETY: проверено выше.
    unsafe { core::ptr::write_unaligned(out as *mut [u64; 15], fields) };
    0
}

/// `clock_getres`: точность часов — наносекунда. Отрицательный номер — часы
/// процессорного времени потока или процесса (`pthread_getcpuclockid`): их
/// понимает и `clock_gettime`, и Java спрашивает о них при запуске.
pub fn clock_getres(clock: usize, out: usize) -> i64 {
    if clock > 11 && (clock as i32) >= 0 {
        return -e::EINVAL;
    }
    if out != 0 {
        if !space::user_can(out, 16, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::write_unaligned(out as *mut [u64; 2], [0, 1]) };
    }
    0
}

/// `getresuid`/`getresgid`: все три — те же.
pub fn getres(real: usize, effective: usize, saved: usize, group: bool) -> i64 {
    let cred = crate::user::credentials();
    let id = if group { cred.gid } else { cred.uid };
    for at in [real, effective, saved] {
        if !space::user_can(at, 4, PageFlags::WRITE) {
            return -e::EFAULT;
        }
        // SAFETY: проверено строкой выше.
        unsafe { core::ptr::write_unaligned(at as *mut u32, id) };
    }
    0
}
