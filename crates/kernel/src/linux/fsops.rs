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
    let (parent, name) = match linux.rsplit_once('/') {
        Some(("", name)) => ("/", name),
        Some((parent, name)) => (parent, name),
        None => return String::from(linux),
    };
    let mut out = crate::linux_path::resolve(parent, cred);
    if !out.ends_with('/') {
        out.push('/');
    }
    out.push_str(name);
    out
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
    let tmpfs = match path {
        Some(ptr) => match linux_path(AT_FDCWD, ptr) {
            Ok(linux) => linux == "/dev/shm" || linux.starts_with("/dev/shm/"),
            Err(err) => return err,
        },
        None => match crate::user::with_current(|program| program.files.node(fd)) {
            Some(Ok(node)) => node.shared_memory().is_some(),
            Some(Err(err)) => return file_errno(err),
            None => return -e::ESRCH,
        },
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
        if tmpfs { TMPFS_MAGIC } else { EXT2_MAGIC },
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

/// `clock_getres`: точность часов — наносекунда.
pub fn clock_getres(clock: usize, out: usize) -> i64 {
    if clock > 11 {
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
