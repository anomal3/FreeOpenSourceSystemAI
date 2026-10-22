//! Процессы программ Linux (фаза 71d): `fork`/`vfork` (сам `clone` — в
//! `thread.rs`), `execve`, `wait4`, `getppid` и учёт детей.
//!
//! # Кто чей ребёнок
//!
//! Таблица [`CHILDREN`]: пара «родитель, ребёнок» и, когда ребёнок кончился,
//! его статус для `wait4`. Запись заводят и родитель (сразу после `fork`), и
//! сам ребёнок (в начале своей задачи) — кто первым, тот и завёл: так `wait4`
//! родителя не скажет `ECHILD` про ребёнка, который ещё не начал исполняться,
//! а конец ребёнка, случившийся раньше записи родителя, не потеряется.
//!
//! Кончился родитель — его кончившиеся дети забываются, живые остаются без
//! родителя (ноль): их конец никто не ждёт, и записи уходят вместе с ними.
//! Процесса `init`, который подбирал бы сирот, нет — подбирать нечего.

use alloc::string::String;
use alloc::vec::Vec;

use super::e;
use crate::mm::PageFlags;
use crate::sync::Mutex;
use crate::user::space;
use crate::vfs::NodeKind;
use crate::vfs::perm::Access;

/// Номер `SIGCHLD`.
const SIGCHLD: usize = 17;
/// `ENOEXEC` и `ELOOP` — их нет в общем списке `e`.
const ENOEXEC: i64 = 8;
const ELOOP: i64 = 40;
const E2BIG: i64 = 7;

struct Child {
    parent: u32,
    pid: u32,
    /// Статус `wait4`, когда ребёнок кончился.
    status: Option<i32>,
    /// Ребёнок `vfork` отпустил родителя: сделал `execve` или кончился.
    released: bool,
}

static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// Записать ребёнка `pid` за `parent` (если ещё не записан).
pub fn register_child(parent: u32, pid: u32) {
    if parent == 0 {
        return;
    }
    let mut list = CHILDREN.lock();
    if list.iter().any(|child| child.pid == pid) || list.try_reserve(1).is_err() {
        return;
    }
    list.push(Child { parent, pid, status: None, released: false });
}

/// Записан ли живой процесс `pid` (ребёнок, который ещё может не дойти до
/// своей задачи).
pub fn is_starting(pid: u32) -> bool {
    CHILDREN.lock().iter().any(|child| child.pid == pid && child.status.is_none())
}

/// Статус `wait4` по коду выхода и сигналу, которым процесс снят.
fn wait_status(code: i64, signal: Option<u8>) -> i32 {
    match (signal, code) {
        (Some(sig), _) => i32::from(sig),
        // Снят ядром по просьбе или после отказа — как `SIGKILL` и `SIGSEGV`.
        (None, user_abi::EXIT_KILLED) => 9,
        (None, user_abi::EXIT_FAULTED) => 11,
        (None, code) => ((code & 0xff) as i32) << 8,
    }
}

/// Процесс `pid` кончился с кодом `code` (или снят сигналом `signal`):
/// записать статус для родителя, послать ему `SIGCHLD`, разбудить ждущих.
/// Зовётся для всякого процесса, не только Linux: у кого записей нет, тому
/// здесь делать нечего.
pub fn process_ended(pid: u32, code: i64, signal: Option<u8>) {
    let status = wait_status(code, signal);
    let parent = {
        let mut list = CHILDREN.lock();
        // Его кончившиеся дети больше никому не нужны, живые — сироты.
        list.retain(|child| !(child.parent == pid && child.status.is_some()));
        for child in list.iter_mut().filter(|child| child.parent == pid) {
            child.parent = 0;
        }
        match list.iter().position(|child| child.pid == pid) {
            Some(index) if list[index].parent == 0 => {
                list.remove(index);
                None
            }
            Some(index) => {
                list[index].status = Some(status);
                list[index].released = true;
                Some(list[index].parent)
            }
            None => None,
        }
    };
    if let Some(parent) = parent {
        crate::kprintln!("  linux       : process #{pid} ended, status {status:#x} for #{parent}");
        super::signal::send_to_process(parent, SIGCHLD, true);
    }
    crate::io_events::notify();
}

/// Ребёнок `vfork` сделал `execve`: родитель может продолжать.
pub fn exec_done(pid: u32) {
    let mut found = false;
    {
        let mut list = CHILDREN.lock();
        if let Some(child) = list.iter_mut().find(|child| child.pid == pid) {
            child.released = true;
            found = true;
        }
    }
    if found {
        crate::io_events::notify();
    }
}

/// Родитель `vfork` ждёт, пока ребёнок не сделает `execve` или не кончится.
/// Память у них не общая (копия), но порядок — тот, на который рассчитывает
/// `posix_spawn`: родитель продолжает, когда ребёнка уже нет в его образе.
pub fn wait_vfork(pid: u32) {
    loop {
        let seen = crate::io_events::generation();
        let released = CHILDREN.lock().iter().find(|child| child.pid == pid).is_none_or(|child| child.released);
        if released || crate::user::kill_pending() {
            return;
        }
        crate::io_events::wait(seen, Some(crate::time::uptime_ms() + 100));
    }
}

fn own_pid() -> u32 {
    crate::user::with_current(|program| program.linux_pid).unwrap_or(0)
}

/// `getppid`: родитель; запущенному системой — 1, как у Linux сироте.
pub fn getppid() -> i64 {
    match crate::user::with_current(|program| program.linux_ppid) {
        Some(0) | None => 1,
        Some(ppid) => i64::from(ppid),
    }
}

/// `wait4(pid, status, options, rusage)`. Группы процессов (`pid` 0 и
/// меньше -1) — как «любой ребёнок»: группа у всех одна.
pub fn wait4(pid: usize, status: usize, options: usize, rusage: usize) -> i64 {
    const WNOHANG: usize = 1;
    const RUSAGE: usize = 144;
    let pid = pid as i32;
    if status != 0 && !space::user_can(status, 4, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    if rusage != 0 && !space::user_can(rusage, RUSAGE, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let me = own_pid();
    let wanted = |child: &Child| child.parent == me && (pid <= 0 || child.pid == pid as u32);
    loop {
        let seen = crate::io_events::generation();
        let found = {
            let mut list = CHILDREN.lock();
            if !list.iter().any(wanted) {
                return -e::ECHILD;
            }
            list.iter().position(|child| wanted(child) && child.status.is_some()).map(|index| list.remove(index))
        };
        if let Some(child) = found {
            // SAFETY: оба адреса проверены на запись выше; пространство — наше.
            unsafe {
                if status != 0 {
                    core::ptr::write_unaligned(status as *mut i32, child.status.unwrap_or(0));
                }
                if rusage != 0 {
                    core::ptr::write_bytes(rusage as *mut u8, 0, RUSAGE);
                }
            }
            return i64::from(child.pid);
        }
        if options & WNOHANG != 0 {
            return 0;
        }
        if crate::user::kill_pending() || super::signal::pending() {
            return -e::EINTR;
        }
        crate::io_events::wait(seen, Some(crate::time::uptime_ms() + 100));
    }
}

/// Сколько указателей и байт строк принимает `execve` (у Linux — четверть
/// стека; здесь — сколько помещается в жадную часть стека).
const MAX_VECTOR: usize = 1900;
const MAX_VECTOR_BYTES: usize = 44 * 1024;

/// Вектор строк `argv`/`envp` из памяти программы; `NULL` — пустой.
fn user_vector(ptr: usize, bytes: &mut usize) -> Result<Vec<String>, i64> {
    let mut out = Vec::new();
    if ptr == 0 {
        return Ok(out);
    }
    for index in 0.. {
        if index >= MAX_VECTOR {
            return Err(-E2BIG);
        }
        let at = ptr + index * 8;
        if !space::user_can(at, 8, PageFlags::READ) {
            return Err(-e::EFAULT);
        }
        // SAFETY: проверено строкой выше.
        let string = unsafe { core::ptr::read_unaligned(at as *const usize) };
        if string == 0 {
            break;
        }
        let text = super::user_string(string)?;
        *bytes += text.len() + 1;
        if *bytes > MAX_VECTOR_BYTES {
            return Err(-E2BIG);
        }
        out.try_reserve(1).map_err(|_| -e::ENOMEM)?;
        out.push(text);
    }
    Ok(out)
}

/// `execve(path, argv, envp)`. При успехе не возвращается в старую
/// программу: `frame` уже указывает на вход новой.
pub fn execve(frame: &mut crate::arch::TrapFrame, path: usize, argv: usize, envp: usize) -> i64 {
    let linux = match super::linux_path(super::AT_FDCWD, path) {
        Ok(path) => path,
        Err(err) => return err,
    };
    // `/proc/self/exe` — файл самой программы (так перезапускают себя).
    let linux = if linux == "/proc/self/exe" {
        crate::user::with_current(|program| String::from(crate::linux_path::shown(&program.path))).unwrap_or(linux)
    } else {
        linux
    };
    let mut bytes = 0;
    let args = match user_vector(argv, &mut bytes) {
        Ok(args) => args,
        Err(err) => return err,
    };
    let env = match user_vector(envp, &mut bytes) {
        Ok(env) => env,
        Err(err) => return err,
    };
    exec_linux(frame, linux, args, &env, 0)
}

/// `execve` по пути Linux: сценарий с `#!` — через его интерпретатор,
/// остальное — в ядро ([`crate::user::exec`]).
fn exec_linux(frame: &mut crate::arch::TrapFrame, linux: String, args: Vec<String>, env: &[String], depth: usize) -> i64 {
    let cred = crate::user::credentials();
    let native = crate::linux_path::resolve(&linux, cred);
    let node = match crate::fs::resolve_as(cred, &native, Access::SEARCH) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return super::vfs_errno(err),
        None => return -e::ENOENT,
    };
    if node.metadata().kind == NodeKind::Directory {
        return -e::EACCES;
    }
    let mut head = [0u8; 256];
    let read = node.read_at(0, &mut head).unwrap_or(0);
    drop(node);
    if read >= 2 && head[..2] == *b"#!" {
        if depth >= 4 {
            return -ELOOP;
        }
        let Some(end) = head[..read].iter().position(|byte| *byte == b'\n') else {
            return -ENOEXEC;
        };
        let Ok(line) = core::str::from_utf8(&head[2..end]) else {
            return -ENOEXEC;
        };
        let line = line.trim();
        // Linux: интерпретатор — до первого пробела, остаток строки — один
        // аргумент.
        let (interp, extra) = match line.find([' ', '\t']) {
            Some(at) => (&line[..at], Some(line[at..].trim())),
            None => (line, None),
        };
        if interp.is_empty() {
            return -ENOEXEC;
        }
        let mut next = Vec::new();
        if next.try_reserve(args.len() + 3).is_err() {
            return -e::ENOMEM;
        }
        next.push(String::from(interp));
        if let Some(extra) = extra.filter(|extra| !extra.is_empty()) {
            next.push(String::from(extra));
        }
        next.push(linux);
        next.extend(args.into_iter().skip(1));
        return exec_linux(frame, String::from(interp), next, env, depth + 1);
    }

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let envp: Vec<&str> = env.iter().map(String::as_str).collect();
    match crate::user::exec(frame, &native, &argv, &envp) {
        Ok(()) => {
            exec_done(own_pid());
            0
        }
        Err(crate::user::ExecError::Busy) => -e::EAGAIN,
        Err(crate::user::ExecError::Load(err)) => {
            use crate::user::Error;
            match err {
                Error::Read(err) => super::vfs_errno(err),
                Error::NoFilesystem => -e::ENOENT,
                Error::OutOfMemory | Error::Map(_) => -e::ENOMEM,
                Error::TooManyTasks | Error::AlreadyRunning => -e::EAGAIN,
                Error::Elf(_) | Error::WriteExecute(_) | Error::TooManySegments => -ENOEXEC,
            }
        }
    }
}
