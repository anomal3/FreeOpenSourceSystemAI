//! Обработка системных вызовов — единственная дверь из программы в ядро.
//!
//! # Что здесь главное
//!
//! Проверка аргументов. Всё, что приходит сюда, выбрала программа, а она —
//! недоверенная сторона: номер вызова может быть любым, указатель может
//! указывать в ядро, длина может переполнять сложение. Ни одно из этих значений
//! не должно приводить ни к чему, кроме кода ошибки.
//!
//! Особенно указатели, и особенно те, **в которые ядро пишет**. Прочитать
//! память ядра программа не может сама — ей не дадут страницы, — но может
//! попросить об этом ядро, передав чужой адрес в `write`. А передав в `read`
//! адрес собственного кода, она заставила бы ядро писать в страницу, которую
//! сама изменить не вправе: ядру-то запись разрешена, и отказ пришёл бы уже в
//! кольце ноль, то есть остановил бы машину. Поэтому каждый адрес проверяется
//! по таблицам страниц самой программы ([`space::user_can`]) — правом,
//! спрошенным у той же записи, у которой его спросил бы процессор.
//!
//! # Права файлов
//!
//! Проверка `mode`/`uid`/`gid` живёт не здесь, а в [`crate::fs::resolve_as`],
//! и вызывается отсюда с личностью сеанса ([`super::session`]). Здесь остаётся
//! перевод отказов в числа договора: ядро не рассказывает программе, какая
//! именно структура на диске ей не понравилась.

use user_abi::{
    ERR_BAD_ADDRESS, ERR_BAD_FD, ERR_BAD_PATH, ERR_IO, ERR_NOT_FOUND, ERR_NO_FILESYSTEM,
    ERR_NO_PROGRAM, ERR_NO_SYSCALL, ERR_PERMISSION, ERR_TOO_MANY_FILES, ERR_UNSUPPORTED, FD_STDOUT,
    KIND_DIRECTORY, KIND_FILE, SYS_CLOSE, SYS_EXIT, SYS_GETGID, SYS_GETPID, SYS_GETUID, SYS_OPEN,
    ERR_EXISTS, ERR_NOT_EMPTY, ERR_NO_SPACE, SYS_MKDIR, SYS_READ, SYS_REMOVE, SYS_SLEEP,
    SYS_STAT, SYS_UPTIME, SYS_WRITE, SYS_YIELD, Stat,
};

use crate::mm::PageFlags;
use crate::vfs::perm::Access;
use crate::vfs::{NodeKind, VfsError};
use crate::{irq, sched};

use super::files::FileError;
use super::space;

/// Самый длинный путь, который ядро согласно принять от программы.
///
/// Не свойство файловой системы, а предел на разбор: путь копируется в буфер на
/// стеке ядра, и брать его размер из длины, которую выбрала программа, значило
/// бы отдать ей глубину ядерного стека.
const MAX_PATH: usize = 255;

/// Разобрать системный вызов. Возвращает то, что уедет в регистр результата.
///
/// # Safety
///
/// Вызывать только из обработчика ловушки, пришедшей из пользовательского
/// режима: [`SYS_EXIT`] не возвращается, а уходит в точку запуска программы.
pub unsafe fn handle(number: usize, a0: usize, a1: usize, a2: usize) -> i64 {
    match number {
        SYS_WRITE => write(a0, a1, a2),
        SYS_EXIT => {
            // SAFETY: контракт функции.
            unsafe { crate::arch::return_to_kernel(a0 as i64) }
        }
        SYS_SLEEP => {
            // Спящая программа выходит из ротации до срока. Ограничения на
            // длительность нет намеренно: программа, попросившая проспать год,
            // не занимает ничего, кроме своего слота в таблице, — а слот у неё
            // и так есть.
            sched::sleep_ms(a0 as u64);
            0
        }
        SYS_YIELD => {
            // Уступка оставляет программу готовой к исполнению, в отличие от
            // `SYS_SLEEP`: она отдаёт очередь, а не выходит из неё. Своего
            // адресного пространства это не касается — с Phase 13a его
            // переставляет само переключение задач.
            sched::yield_now();
            0
        }
        SYS_UPTIME => irq::uptime_ms() as i64,
        SYS_OPEN => open(a0, a1, a2),
        SYS_READ => read(a0, a1, a2),
        SYS_CLOSE => close(a0),
        SYS_STAT => stat(a0, a1, a2),
        SYS_MKDIR => mkdir(a0, a1, a2),
        SYS_REMOVE => remove(a0, a1),
        SYS_GETUID => i64::from(super::session::credentials().uid),
        SYS_GETGID => i64::from(super::session::credentials().gid),
        // Программа — это задача, и её номер тот же, что видит `tasks` в
        // оболочке. Отдельного пространства номеров процессов не заводится:
        // второе пространство имён для тех же объектов пришлось бы всё время
        // сопоставлять с первым.
        SYS_GETPID => i64::from(sched::current().as_u32()),
        _ => ERR_NO_SYSCALL,
    }
}

/// `write(fd, ptr, len)`.
fn write(fd: usize, ptr: usize, len: usize) -> i64 {
    if len == 0 {
        return 0;
    }
    if !space::user_can(ptr, len, PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }

    // SAFETY: диапазон проверен по таблицам самой программы: каждая его
    // страница отображена и доступна ей на чтение. Ядро и программа исполняются
    // на одном процессоре по очереди, поэтому изменить эти байты во время
    // чтения некому.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };

    if fd != FD_STDOUT {
        // В файл уходят байты как есть: это данные, а не текст, и требовать от
        // них UTF-8 значило бы запретить программе сохранить что угодно, кроме
        // строки.
        return match super::with_current(|program| program.files.write(fd, bytes)) {
            Some(Ok(written)) => written as i64,
            Some(Err(err)) => errno(err),
            None => ERR_NO_PROGRAM,
        };
    }

    // Двоичный мусор в окно оболочки не выводится: управляющие байты испортили
    // бы и сетку символов, и терминал на другом конце линии. Это ограничение
    // вывода, а не проверка программы, — поэтому не ошибка.
    match core::str::from_utf8(bytes) {
        Ok(text) => {
            crate::shell::print(format_args!("{text}"));
            len as i64
        }
        Err(_) => ERR_BAD_ADDRESS,
    }
}

/// `open(ptr, len, flags) -> fd`.
fn open(ptr: usize, len: usize, flags: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };

    let cred = super::session::credentials();
    match super::with_current(|program| program.files.open(cred, path, flags)) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `mkdir(ptr, len, mode) -> 0`.
fn mkdir(ptr: usize, len: usize, mode: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };
    // Права обрезаются до девяти бит: тип узла задаёт ядро, и программа,
    // приславшая в этом аргументе что угодно, не должна получить каталог,
    // притворяющийся устройством.
    let mode = (mode as u16) & 0o777;
    match crate::fs::mkdir_as(super::session::credentials(), path, mode) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => ERR_NO_FILESYSTEM,
    }
}

/// `remove(ptr, len) -> 0`.
fn remove(ptr: usize, len: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };
    match crate::fs::remove_as(super::session::credentials(), path) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => ERR_NO_FILESYSTEM,
    }
}

/// `read(fd, ptr, len) -> сколько прочитано`.
fn read(fd: usize, ptr: usize, len: usize) -> i64 {
    if len == 0 {
        return 0;
    }
    // Именно `WRITE`: сюда ядро пишет. Страница, которую программа отдала на
    // запись только себе на чтение, — это отказ в кольце ноль, см. заголовок.
    if !space::user_can(ptr, len, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }

    // SAFETY: диапазон проверен по таблицам программы и доступен ей на запись;
    // пока исполняется этот вызов, отображение не меняется — менять его может
    // только ядро, а оно сейчас здесь.
    let buffer = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };

    match super::with_current(|program| program.files.read(fd, buffer)) {
        Some(Ok(read)) => read as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `close(fd)`.
fn close(fd: usize) -> i64 {
    match super::with_current(|program| program.files.close(fd)) {
        Some(Ok(())) => 0,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `stat(ptr, len, out)`.
fn stat(ptr: usize, len: usize, out: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };

    // Выравнивание проверяется до всего остального: невыровненная запись
    // структуры — это отказ на AArch64 и молчаливая потеря скорости на x86-64,
    // и оба варианта хуже внятной ошибки.
    if out % align_of::<Stat>() != 0 {
        return ERR_BAD_ADDRESS;
    }
    if !space::user_can(out, size_of::<Stat>(), PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }

    // Прав на сам файл не требуется — только проход по каталогам пути. Так же
    // устроен `stat` в Unix, и разница осмысленная: узнать размер файла и
    // прочитать его содержимое — разные вещи. Что скрывает каталог, тем не
    // менее скрыто: [`crate::fs::resolve_as`] спрашивает право пройти у каждого
    // каталога на пути.
    let node = match crate::fs::resolve_as(super::session::credentials(), path, Access::NONE) {
        Some(Ok(node)) => node,
        Some(Err(err)) => return vfs_errno(err),
        None => return ERR_NO_FILESYSTEM,
    };

    let meta = node.metadata();
    let value = Stat {
        size: meta.size,
        mode: u32::from(meta.mode),
        uid: meta.uid,
        gid: meta.gid,
        kind: match meta.kind {
            NodeKind::File => KIND_FILE,
            NodeKind::Directory => KIND_DIRECTORY,
        },
    };

    // SAFETY: адрес проверен на выравнивание и на то, что вся структура лежит в
    // страницах, доступных программе на запись.
    unsafe { core::ptr::write(out as *mut Stat, value) };
    0
}

/// Скопировать путь из памяти программы в буфер ядра.
///
/// Копия, а не срез поверх её памяти: путь уезжает в файловую систему, где
/// проживёт дольше одного обращения, а память программы всё это время остаётся
/// её памятью.
fn copy_path<'a>(ptr: usize, len: usize, buffer: &'a mut [u8; MAX_PATH]) -> Result<&'a str, i64> {
    if len == 0 || len > MAX_PATH {
        return Err(ERR_BAD_PATH);
    }
    if !space::user_can(ptr, len, PageFlags::READ) {
        return Err(ERR_BAD_ADDRESS);
    }
    // SAFETY: диапазон проверен по таблицам программы и доступен ей на чтение.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    buffer[..len].copy_from_slice(bytes);
    core::str::from_utf8(&buffer[..len]).map_err(|_| ERR_BAD_PATH)
}

/// Перевести отказ файловой системы в код договора.
fn errno(err: FileError) -> i64 {
    match err {
        FileError::NoFilesystem => ERR_NO_FILESYSTEM,
        FileError::BadFd => ERR_BAD_FD,
        FileError::TooManyFiles => ERR_TOO_MANY_FILES,
        FileError::Vfs(err) => vfs_errno(err),
    }
}

fn vfs_errno(err: VfsError) -> i64 {
    match err {
        VfsError::NotFound => ERR_NOT_FOUND,
        VfsError::PermissionDenied => ERR_PERMISSION,
        VfsError::BadPath => ERR_BAD_PATH,
        VfsError::WrongKind | VfsError::Unsupported => ERR_UNSUPPORTED,
        VfsError::Exists => ERR_EXISTS,
        VfsError::NotEmpty => ERR_NOT_EMPTY,
        VfsError::NoSpace => ERR_NO_SPACE,
        // Испорченная структура на диске, чтение за концом устройства и отказ
        // самого устройства для программы — одно и то же: носитель не отдал
        // данные. Подробности ушли в журнал ядра, где им и место.
        VfsError::Corrupt | VfsError::OutOfBounds | VfsError::Io | VfsError::OutOfMemory => ERR_IO,
    }
}
