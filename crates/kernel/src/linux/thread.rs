//! Потоки программ Linux (фаза 71c): `clone`, `clone3`, выход потока и
//! процесса, `prctl`.

use super::e;
use crate::mm::PageFlags;
use crate::sched::{self, TaskId};
use crate::user::space;

const CLONE_VM: usize = 0x100;
const CLONE_THREAD: usize = 0x10000;
const CLONE_SETTLS: usize = 0x80000;
const CLONE_PARENT_SETTID: usize = 0x100000;
const CLONE_CHILD_CLEARTID: usize = 0x200000;
const CLONE_CHILD_SETTID: usize = 0x1000000;

const CLONE_VFORK: usize = 0x4000;

/// `clone(flags, stack, parent_tid, child_tid, tls)` — порядок аргументов
/// x86-64. Поток — `CLONE_VM | CLONE_THREAD`; без `CLONE_THREAD` — новый
/// процесс (фаза 71d): `fork`, `vfork`, `posix_spawn`.
pub fn clone(frame: &crate::arch::TrapFrame, flags: usize, stack: usize, parent_tid: usize, child_tid: usize, tls: usize) -> i64 {
    if flags & CLONE_THREAD == 0 {
        return clone_process(frame, flags, stack, parent_tid, child_tid, tls);
    }
    if flags & CLONE_VM == 0 {
        return -e::EINVAL;
    }
    let mut regs = *frame;
    regs.rax = 0;
    if stack != 0 {
        regs.rsp = stack as u64;
    }
    let tls = if flags & CLONE_SETTLS != 0 { tls as u64 } else { crate::arch::user_tls() };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 };
    let id = match crate::user::spawn_linux_thread(regs, tls, clear) {
        Ok(id) => id,
        Err(crate::user::ThreadError::TooManyTasks) => return -e::EAGAIN,
        Err(_) => return -e::ENOMEM,
    };
    let tid = id.as_u32();
    // Новый поток может успеть побежать раньше этих строк: glibc читает
    // `pd->tid` в нём самом только через `THREAD_SELF`, а ждут номера родитель
    // (`pthread_create`) и `pthread_join` — им и пишется здесь.
    for (flag, at) in [(CLONE_PARENT_SETTID, parent_tid), (CLONE_CHILD_SETTID, child_tid)] {
        if flags & flag != 0 && at != 0 && space::user_can(at, 4, PageFlags::WRITE) {
            // SAFETY: адрес проверен на запись; пространство общее у потоков.
            unsafe { core::ptr::write_volatile(at as *mut u32, tid) };
        }
    }
    i64::from(tid)
}

/// Новый процесс — копия текущего (фаза 71d). `CLONE_VM` без `CLONE_THREAD`
/// (`vfork`, `posix_spawn` glibc) общей памяти не даёт — ребёнок получает
/// копию, — а `CLONE_VFORK` держит родителя, пока ребёнок не сделает
/// `execve` или не кончится: этого порядка `posix_spawn` и ждёт. Сигнал
/// конца (младший байт флагов) — всегда `SIGCHLD`.
fn clone_process(frame: &crate::arch::TrapFrame, flags: usize, stack: usize, parent_tid: usize, child_tid: usize, tls: usize) -> i64 {
    let mut regs = *frame;
    regs.rax = 0;
    if stack != 0 {
        regs.rsp = stack as u64;
    }
    let tls = if flags & CLONE_SETTLS != 0 { tls as u64 } else { crate::arch::user_tls() };
    let set = if flags & CLONE_CHILD_SETTID != 0 { child_tid } else { 0 };
    let clear = if flags & CLONE_CHILD_CLEARTID != 0 { child_tid } else { 0 };
    let id = match crate::user::spawn_linux_process(regs, tls, set, clear) {
        Ok(id) => id,
        Err(crate::user::ThreadError::TooManyTasks) => return -e::EAGAIN,
        Err(_) => return -e::ENOMEM,
    };
    let pid = id.as_u32();
    if flags & CLONE_PARENT_SETTID != 0 && parent_tid != 0 && space::user_can(parent_tid, 4, PageFlags::WRITE) {
        // SAFETY: адрес проверен на запись в пространстве родителя.
        unsafe { core::ptr::write_volatile(parent_tid as *mut u32, pid) };
    }
    if flags & CLONE_VFORK != 0 {
        super::process::wait_vfork(pid);
    }
    i64::from(pid)
}

/// `clone3(args, size)`: то же, что `clone`, аргументы — структурой.
pub fn clone3(frame: &crate::arch::TrapFrame, args: usize, size: usize) -> i64 {
    const MIN_SIZE: usize = 64;
    if size < MIN_SIZE || !space::user_can(args, MIN_SIZE, PageFlags::READ) {
        return -e::EINVAL;
    }
    // SAFETY: первые 64 байта `struct clone_args` проверены на чтение.
    let field = |index: usize| unsafe { core::ptr::read_unaligned((args + index * 8) as *const u64) } as usize;
    let (flags, child_tid, parent_tid, stack, stack_size, tls) = (field(0), field(2), field(3), field(5), field(6), field(7));
    let top = if stack != 0 { stack + stack_size } else { 0 };
    clone(frame, flags, top, parent_tid, child_tid, tls)
}

/// Главный ли это поток процесса.
fn is_main() -> bool {
    crate::user::with_current(|program| program.linux_pid) == Some(sched::current().as_u32())
}

/// `exit`: у потока — конец потока (ядро обнулит его `clear_tid`), у главного
/// — конец процесса.
pub fn exit(code: usize) -> i64 {
    // SAFETY: из системного вызова третьего кольца — кадр входа цел.
    unsafe { crate::arch::return_to_kernel(code as i64) }
}

/// `exit_group`: конец процесса из любого потока. Из потока — процессу
/// заказывается конец с этим кодом, главный поток будится и уходит с ним на
/// ближайшем возврате.
pub fn exit_group(code: usize) -> i64 {
    if !is_main() {
        let main = crate::user::with_current(|program| {
            program.linux_exit = Some(code as i64);
            program.kill_requested = true;
            program.linux_pid
        });
        if let Some(main) = main {
            sched::wake(TaskId::new(main));
        }
    }
    // SAFETY: из системного вызова третьего кольца — кадр входа цел.
    unsafe { crate::arch::return_to_kernel(code as i64) }
}

/// `prctl`: имя потока и флаги, которых у ядра нет, — принимаются.
pub fn prctl(option: usize, arg: usize) -> i64 {
    const PR_SET_PDEATHSIG: usize = 1;
    const PR_GET_DUMPABLE: usize = 3;
    const PR_SET_DUMPABLE: usize = 4;
    const PR_SET_NAME: usize = 15;
    const PR_GET_NAME: usize = 16;
    const PR_CAPBSET_READ: usize = 23;
    const PR_SET_NO_NEW_PRIVS: usize = 38;
    const PR_GET_NO_NEW_PRIVS: usize = 39;
    match option {
        PR_SET_PDEATHSIG | PR_SET_DUMPABLE | PR_SET_NAME | PR_SET_NO_NEW_PRIVS => 0,
        PR_GET_DUMPABLE | PR_GET_NO_NEW_PRIVS => i64::from(option == PR_GET_DUMPABLE),
        PR_CAPBSET_READ => 0,
        PR_GET_NAME => {
            if !space::user_can(arg, 16, PageFlags::WRITE) {
                return -e::EFAULT;
            }
            let name = b"linux\0";
            // SAFETY: проверено строкой выше.
            unsafe { core::ptr::copy_nonoverlapping(name.as_ptr(), arg as *mut u8, name.len()) };
            0
        }
        _ => -e::EINVAL,
    }
}
