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
//! и вызывается отсюда с личностью **программы** ([`super::credentials`]).
//! Здесь остаётся перевод отказов в числа договора: ядро не рассказывает
//! программе, какая именно структура на диске ей не понравилась.
//!
//! Личность программы, а не сеанса, — с Phase 33: службу запускает супервизор
//! от root, а исполняется она от своего пользователя, и общая на всех личность
//! сеанса означала бы, что описание службы врёт о том, от чьего имени она
//! работает.

use user_abi::{
    ERR_AGAIN, ERR_BAD_ADDRESS, ERR_BAD_FD, ERR_BAD_PATH, ERR_IO, ERR_NOT_FOUND,
    ERR_NO_FILESYSTEM, ERR_NO_PROGRAM, ERR_NO_SYSCALL, ERR_NO_TASK, ERR_PERMISSION,
    ERR_TOO_MANY_FILES, ERR_TOO_MANY_TASKS, ERR_UNSUPPORTED, FD_STDERR, FD_STDIN, FD_STDOUT,
    KIND_DIRECTORY, KIND_FILE, SYS_CLOSE, SYS_CREATE, SYS_EXIT, SYS_GETGID, SYS_GETPID,
    SYS_GETUID, SYS_OPEN, Dirent, ERR_EXISTS, ERR_NOT_EMPTY, ERR_NO_SPACE, MAX_NAME, SEEK_CUR,
    SEEK_END, SEEK_SET, SPAWN_INHERIT, SYS_MKDIR, SYS_READ, SYS_READDIR, SYS_REMOVE, SYS_RENAME,
    SYS_SEEK, SYS_SLEEP, SYS_SPAWN, SYS_STAT, SYS_TIME, SYS_TTYMODE, SYS_UPTIME, SYS_WAIT,
    SYS_WINSIZE, SYS_WRITE, SYS_YIELD, Stat, TTY_RAW, WAIT_NOHANG,
};
use user_abi::{ERR_BROKEN_PIPE, LAUNCH_KEEP, Launch, SYS_LAUNCH, SYS_PIPE};
use user_abi::{ERR_UPDATE_REFUSED, SYS_UPDATE};
use user_abi::{
    ERR_BAD_SOCKET, ERR_NO_NETWORK, NetConfig, NetInfo, Peer, SOCK_TCP, SOCK_UDP, STREAM_FIRST,
    StreamState, SYS_ACCEPT, SYS_BIND, SYS_CLOSE_SOCKET, SYS_CONNECT, SYS_LISTEN, SYS_NETCONF,
    SYS_NETINFO, SYS_PEER, SYS_RANDOM, SYS_RECV, SYS_RESOLVE, SYS_SEND, SYS_SHUTDOWN,
    SYS_SOCKET, SYS_STREAMSTATE,
};

use crate::net::{self, NetError};
use crate::net::ipv4::Ipv4;

use crate::mm::PageFlags;
use crate::vfs::perm::Access;
use crate::vfs::{NodeKind, VfsError};
use crate::{sched, time};

use super::files::{self, FileError};
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
        SYS_UPTIME => time::uptime_ms() as i64,
        // Ноль — это «времени суток система не знает», а не 1970 год. Разница
        // видна только тому, кто её проверяет, поэтому она названа в договоре.
        SYS_TIME => time::now_unix().unwrap_or(0) as i64,
        SYS_OPEN => open(a0, a1, a2),
        SYS_CREATE => create(a0, a1, a2),
        SYS_SPAWN => spawn(a0, a1, a2),
        SYS_PIPE => make_pipe(),
        SYS_LAUNCH => launch(a0),
        SYS_WAIT => wait(a0, a1),
        SYS_READ => read(a0, a1, a2),
        SYS_SEEK => seek(a0, a1 as i64, a2),
        SYS_READDIR => readdir(a0, a1, a2),
        SYS_CLOSE => close(a0),
        SYS_STAT => stat(a0, a1, a2),
        SYS_MKDIR => mkdir(a0, a1, a2),
        SYS_REMOVE => remove(a0, a1),
        SYS_RENAME => rename(a0, a1, a2),
        // Размер окна — два числа в одном: столбцы в старшей половине, строки в
        // младшей. Нули означают, что окна нет, и это честный ответ, а не
        // ошибка: система работает и в серийной консоли.
        SYS_WINSIZE => {
            let (cols, rows) = crate::ui::shell_size();
            ((i64::from(cols)) << 32) | i64::from(rows)
        }
        SYS_TTYMODE => {
            crate::tty::set_raw(a0 == TTY_RAW);
            0
        }
        SYS_GETUID => i64::from(super::credentials().uid),
        SYS_GETGID => i64::from(super::credentials().gid),
        // Программа — это задача, и её номер тот же, что видит `tasks` в
        // оболочке. Отдельного пространства номеров процессов не заводится:
        // второе пространство имён для тех же объектов пришлось бы всё время
        // сопоставлять с первым.
        SYS_GETPID => i64::from(sched::current().as_u32()),
        SYS_SOCKET => socket(a0),
        SYS_BIND => bind(a0, a1),
        SYS_CONNECT => connect(a0, a1, a2),
        SYS_SEND => send(a0, a1, a2),
        SYS_RECV => recv(a0, a1, a2),
        SYS_PEER => peer(a0, a1),
        SYS_LISTEN => listen(a0),
        SYS_ACCEPT => accept(a0),
        SYS_SHUTDOWN => shutdown(a0),
        SYS_STREAMSTATE => streamstate(a0, a1),
        SYS_CLOSE_SOCKET => close_socket(a0),
        SYS_NETCONF => netconf(a0, a1),
        SYS_NETINFO => netinfo(a0, a1),
        SYS_RESOLVE => resolve(a0, a1, a2),
        SYS_RANDOM => random(a0, a1),
        SYS_UPDATE => update(a0, a1),
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

    if fd != FD_STDOUT && fd != FD_STDERR {
        // В файл уходят байты как есть: это данные, а не текст, и требовать от
        // них UTF-8 значило бы запретить программе сохранить что угодно, кроме
        // строки.
        return match super::with_current(|program| program.files.write(fd, bytes)) {
            Some(Ok(written)) => written as i64,
            Some(Err(err)) => errno(err),
            None => ERR_NO_PROGRAM,
        };
    }

    // Диагностика уходит в журнал, минуя окно. Это не оформление, а разделение
    // каналов: программа, рисующая весь экран, обязана иметь место, куда сказать
    // слово, не испортив картинку, — а проверить её снаружи можно только по
    // журналу.
    if fd == FD_STDERR {
        return match core::str::from_utf8(bytes) {
            Ok(text) => {
                if crate::ui::is_active() {
                    crate::serial::_print(format_args!("{text}"));
                } else {
                    // Графики нет — значит и картинки, которую надо беречь, нет
                    // тоже, а экранная консоль остаётся единственным местом, где
                    // человек это увидит.
                    crate::kprint!("{text}");
                }
                len as i64
            }
            Err(_) => ERR_BAD_ADDRESS,
        };
    }

    // Вывод перенаправлен в канал — значит уходит туда целиком и как есть.
    // Байты, а не текст: на другом конце может стоять программа, которой нужны
    // именно байты, и требовать от них UTF-8 значило бы запретить `cat`
    // двоичный файл.
    //
    // Писатель берётся копией и лок программы отпускается до записи: запись в
    // полный канал **ждёт**, а ждать, удерживая таблицу программ, значит
    // остановить всех, кто в неё заглянет, — включая того, кто должен этот
    // канал вычерпать.
    if let Some(Some(stdout)) = super::with_current(|program| program.stdout.clone()) {
        return match stdout.write(bytes, true) {
            Ok(written) => written as i64,
            // Читателя не стало: программа пишет в никуда. Это тот самый
            // `EPIPE`, и молчать о нём нельзя — иначе вывод исчезает, а
            // программа считает, что напечатала.
            Err(_) => ERR_BROKEN_PIPE,
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

    let cred = super::credentials();
    match super::with_current(|program| program.files.open(cred, path, flags)) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `create(ptr, len, mode) -> fd`.
fn create(ptr: usize, len: usize, mode: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };

    let cred = super::credentials();
    match super::with_current(|program| program.files.create(cred, path, mode as u16)) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `spawn(ptr, len, кто) -> номер задачи`.
///
/// # Почему проверка прав здесь, а не в [`super::spawn_with`]
///
/// Потому что здесь известно, **кто просит**. Понижение прав разрешено всем,
/// повышение — никому, кроме root, и вопрос «а кто такой этот root» имеет
/// смысл только на границе третьего кольца: внутри ядра эту функцию вызывает и
/// оболочка, которой проверять нечего.
fn spawn(ptr: usize, len: usize, who: usize) -> i64 {
    let mut buffer = [0u8; MAX_PATH];
    let line = match copy_path(ptr, len, &mut buffer) {
        Ok(line) => line,
        Err(err) => return err,
    };

    let cred = match requested_credentials(who) {
        Ok(cred) => cred,
        Err(err) => return err,
    };

    // Служба, запущенная службой, — тоже служба. Иначе перезапущенный
    // супервизором демон стал бы обычной задачей и начал бы удерживать систему
    // от остановки, хотя его родитель этого не делал.
    let daemon = sched::is_daemon();

    match super::spawn_with(line, cred, daemon) {
        Ok(id) => i64::from(id.as_u32()),
        Err(super::Error::TooManyTasks) => ERR_TOO_MANY_TASKS,
        Err(super::Error::OutOfMemory) => ERR_NO_SPACE,
        // Всё остальное — отказ разбора строки: она длиннее предела либо пуста.
        Err(_) => ERR_BAD_PATH,
    }
}

/// `pipe() -> (читающий << 32) | пишущий`.
///
/// Два дескриптора одним вызовом: канал с одним концом — это не канал, а
/// программа, которая успела получить только половину, не смогла бы даже
/// закрыть вторую.
fn make_pipe() -> i64 {
    let (reader, writer) = match super::pipe::create() {
        Ok(ends) => ends,
        Err(_) => return ERR_NO_SPACE,
    };
    // Оба места занимаются под одним взятием лока: между двумя вызовами
    // программу могут вытеснить, и второй мог бы не найти места — а первый уже
    // отдал бы дескриптор на конец канала, у которого нет пары.
    let result = super::with_current(|program| {
        let read_fd = program.files.install_read(reader)?;
        match program.files.install_write(writer) {
            Ok(write_fd) => Ok((read_fd, write_fd)),
            Err(err) => {
                // Место под первый конец возвращается: полканала в таблице —
                // это утечка, которую программе нечем даже заметить.
                let _ = program.files.close(read_fd);
                Err(err)
            }
        }
    });
    match result {
        Some(Ok((read_fd, write_fd))) => ((read_fd as i64) << 32) | write_fd as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `launch(ptr) -> номер задачи`.
///
/// # Почему структурой, а не аргументами
///
/// Потому что назвать надо пять вещей — строку, её длину, личность и два
/// дескриптора, — а аргументов у системного вызова три на обеих архитектурах.
/// Расширять соглашение ради одного вызова пришлось бы в четырёх местах, из
/// них два — вставки на ассемблере.
fn launch(ptr: usize) -> i64 {
    let size = core::mem::size_of::<Launch>();
    if !space::user_can(ptr, size, PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: диапазон проверен по таблицам самой программы и доступен ей на
    // чтение; `Launch` — `repr(C)` из полей по восемь байт, поэтому чтение
    // невыровненным быть не может, а любое содержимое для него законно.
    let request = unsafe { (ptr as *const Launch).read_unaligned() };

    let Ok(command) = usize::try_from(request.command) else {
        return ERR_BAD_ADDRESS;
    };
    let Ok(command_len) = usize::try_from(request.command_len) else {
        return ERR_BAD_PATH;
    };
    let mut buffer = [0u8; MAX_PATH];
    let line = match copy_path(command, command_len, &mut buffer) {
        Ok(line) => line,
        Err(err) => return err,
    };

    let cred = match requested_credentials(request.who as usize) {
        Ok(cred) => cred,
        Err(err) => return err,
    };

    // Концы берутся копиями: дескрипторы остаются у того, кто запускает, и
    // закрывает их он сам. См. договор `SYS_PIPE` — там же сказано, чем грозит
    // забывчивость.
    let ends = super::with_current(|program| {
        let stdin = if request.stdin == LAUNCH_KEEP {
            Ok(None)
        } else {
            program.files.read_end(request.stdin as usize).map(Some)
        };
        let stdout = if request.stdout == LAUNCH_KEEP {
            Ok(None)
        } else {
            program.files.write_end(request.stdout as usize).map(Some)
        };
        stdin.and_then(|stdin| stdout.map(|stdout| (stdin, stdout)))
    });
    let (stdin, stdout) = match ends {
        Some(Ok(ends)) => ends,
        Some(Err(err)) => return errno(err),
        None => return ERR_NO_PROGRAM,
    };

    match super::spawn_streams(line, cred, sched::is_daemon(), stdin, stdout) {
        Ok(id) => i64::from(id.as_u32()),
        Err(super::Error::TooManyTasks) => ERR_TOO_MANY_TASKS,
        Err(super::Error::OutOfMemory) => ERR_NO_SPACE,
        Err(_) => ERR_BAD_PATH,
    }
}

/// От чьего имени запускать: разбор поля `кто` у [`SYS_SPAWN`] и [`SYS_LAUNCH`].
///
/// Правило одно на оба вызова и живёт в одном месте намеренно: два места, где
/// решается «можно ли этому uid», — это два места, где можно ошибиться, и одно
/// из них станет дырой.
fn requested_credentials(who: usize) -> Result<crate::vfs::perm::Credentials, i64> {
    let mine = super::credentials();
    if who == SPAWN_INHERIT {
        return Ok(mine);
    }
    let asked = crate::vfs::perm::Credentials::new((who >> 32) as u32, who as u32);
    // Тот же uid — не «повышение», даже если группа другая: сменить себе
    // группу вправе кто угодно, потому что чужих прав это не даёт.
    if !mine.is_root() && asked.uid != mine.uid {
        return Err(ERR_PERMISSION);
    }
    Ok(asked)
}

/// `wait(id, флаги) -> код возврата`.
///
/// # Почему ожидание не блокирует ничего, кроме спрашивающего
///
/// Потому что оно устроено сном планировщика, а не циклом: задача выходит из
/// очереди и возвращается в неё, когда ждущаяся закончится. Программа,
/// уснувшая на `wait`, не занимает процессор — в отличие от той, что спрашивала
/// бы `WAIT_NOHANG` в цикле.
fn wait(id: usize, flags: usize) -> i64 {
    let Ok(raw) = u32::try_from(id) else {
        return ERR_NO_TASK;
    };
    let task = sched::TaskId::new(raw);

    if flags & WAIT_NOHANG != 0 {
        return match sched::lookup(task) {
            Some((_, sched::TaskState::Finished)) => {
                sched::result_of(task).unwrap_or(ERR_NO_TASK)
            }
            Some(_) => ERR_AGAIN,
            None => ERR_NO_TASK,
        };
    }

    // Программу могли попросить остановиться, пока она ждала чужую. Проверка
    // до сна, а не после: снятие происходит на возврате в третье кольцо, а
    // ожидание задачи, которая не кончится, туда не возвращается никогда.
    if super::kill_pending() {
        return ERR_NO_TASK;
    }
    sched::wait(task).unwrap_or(ERR_NO_TASK)
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
    match crate::fs::mkdir_as(super::credentials(), path, mode) {
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
    match crate::fs::remove_as(super::credentials(), path) {
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

    if fd == FD_STDIN {
        // Ввод перенаправлен в канал — читаем оттуда и **ждём**, как ждал бы
        // терминал. Ноль означает конец: писателей у канала не осталось.
        if let Some(Some(stdin)) = super::with_current(|program| program.stdin.clone()) {
            return match stdin.read(buffer, true) {
                Ok(read) => read as i64,
                Err(_) => 0,
            };
        }
        return read_input(buffer);
    }

    match super::with_current(|program| program.files.read(fd, buffer)) {
        Some(Ok(read)) => read as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `read(0, …)`: чтение с терминала.
///
/// # Почему здесь есть цикл со сном
///
/// Потому что у ввода, в отличие от файла, нет содержимого, которое можно
/// отдать прямо сейчас. Программа, спросившая строку, обязана остановиться до
/// тех пор, пока человек её не наберёт, — и остановиться **правильно**: выйти из
/// очереди планировщика, а не крутиться, спрашивая. Уступка в цикле занимала бы
/// процессор ровно столько же, сколько занимал бы счёт.
///
/// Буфер наполняет задача оболочки (см. [`crate::tty`]), она же и будит эту.
/// Гонка «байт пришёл раньше, чем задача уснула» закрыта тем же счётчиком
/// событий, что и в оболочке: [`sched::block_on_input`] проверяет условие под
/// своим локом.
fn read_input(buffer: &mut [u8]) -> i64 {
    loop {
        let read = crate::tty::read(buffer);
        if read > 0 {
            return read as i64;
        }
        // Ноль байт при законченном вводе — это конец, а не «подожди ещё».
        if crate::tty::at_eof() {
            return 0;
        }
        // Программу могли попросить остановиться, пока она спала. Проверка
        // именно здесь: снимает её [`super::check_kill`] на возврате в третье
        // кольцо, а до возврата надо ещё дойти — то есть выйти из этого цикла.
        if super::kill_pending() {
            return 0;
        }

        // Срок у сна есть всегда, и это не перестраховка: событие ввода кладёт
        // в очередь обработчик прерывания, а он не имеет права ждать лока
        // планировщика — то есть пробуждение можно потерять. Со сроком потеря
        // стоит десятой доли секунды, без срока — всей программы.
        let deadline = crate::irq::ticks() + u64::from(crate::irq::TIMER_HZ) / 10;
        sched::block_on_input(deadline, crate::tty::ready);
    }
}

/// `rename(ptr, old_len, total_len)`.
///
/// Оба пути приходят одним буфером — см. `SYS_RENAME` в договоре.
fn rename(ptr: usize, old_len: usize, total: usize) -> i64 {
    if old_len == 0 || old_len >= total || total > 2 * MAX_PATH {
        return ERR_BAD_PATH;
    }
    if !space::user_can(ptr, total, PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: диапазон проверен по таблицам программы и доступен ей на чтение.
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, total) };

    let mut buffer = [0u8; 2 * MAX_PATH];
    buffer[..total].copy_from_slice(bytes);
    let (Ok(old), Ok(new)) = (
        core::str::from_utf8(&buffer[..old_len]),
        core::str::from_utf8(&buffer[old_len..total]),
    ) else {
        return ERR_BAD_PATH;
    };

    match crate::fs::rename_as(super::credentials(), old, new) {
        Some(Ok(())) => 0,
        Some(Err(err)) => vfs_errno(err),
        None => ERR_NO_FILESYSTEM,
    }
}

/// `seek(fd, offset, whence)`.
///
/// Возвращает новую позицию. Смещение знаковое: назад двигаться можно, и
/// именно это отличает `seek` от «пропустить вперёд, читая в никуда».
fn seek(fd: usize, offset: i64, whence: usize) -> i64 {
    let whence = match whence {
        SEEK_SET => files::Whence::Set,
        SEEK_CUR => files::Whence::Current,
        SEEK_END => files::Whence::End,
        // Неизвестное значение — не «возьмём начало по умолчанию»: программа
        // просила не то, что получила бы, и молчаливая подстановка превратила
        // бы её ошибку в тихо неверные данные.
        _ => return ERR_UNSUPPORTED,
    };

    match super::with_current(|program| program.files.seek(fd, offset, whence)) {
        Some(Ok(position)) => position as i64,
        Some(Err(err)) => errno(err),
        None => ERR_NO_PROGRAM,
    }
}

/// `readdir(fd, ptr, len) -> 1 | 0`.
///
/// Единица — запись положена по `ptr`, ноль — каталог кончился.
fn readdir(fd: usize, ptr: usize, len: usize) -> i64 {
    // Длина проверяется до всего остального: программа, приславшая буфер
    // меньше структуры, получит отказ, а не запись, обрезанную по чужой памяти.
    if len < size_of::<Dirent>() {
        return ERR_BAD_ADDRESS;
    }
    // Именно `WRITE`: сюда пишет ядро. И именно по таблицам программы — тем же
    // правом, которое спросил бы процессор, если бы писала она сама.
    if !space::user_can(ptr, size_of::<Dirent>(), PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }

    let entry = match super::with_current(|program| program.files.next_entry(fd)) {
        Some(Ok(Some(entry))) => entry,
        // Каталог кончился. Ноль, а не ошибка: конец перечисления — обычное
        // событие, ровно как ноль байт в конце файла.
        Some(Ok(None)) => return 0,
        Some(Err(err)) => return errno(err),
        None => return ERR_NO_PROGRAM,
    };

    let mut out = Dirent {
        size: entry.size,
        mtime: entry.mtime,
        mode: u32::from(entry.mode),
        uid: entry.uid,
        gid: entry.gid,
        kind: match entry.kind {
            NodeKind::Directory => KIND_DIRECTORY,
            NodeKind::File => KIND_FILE,
        },
        name_len: 0,
        name: [0; MAX_NAME],
    };

    // Имя длиннее буфера обрезается, а не роняет вызов: предел здесь тот же,
    // что у имени в ext2, поэтому обрезать на самом деле нечего — но полагаться
    // на это в коде, который пишет в чужую память, нельзя.
    let name = entry.name.as_bytes();
    let copy = name.len().min(MAX_NAME);
    out.name[..copy].copy_from_slice(&name[..copy]);
    out.name_len = copy as u32;

    // Запись невыровненная — в отличие от `stat`, который требует выравнивания и
    // отказывает без него. Разница в том, что `Dirent` вчетверо крупнее и
    // программа, скорее всего, держит его в массиве или на стеке, где
    // выравнивание получится само; требовать его отдельно значило бы отказывать
    // из-за того, с чем ядро прекрасно справляется одной инструкцией.
    //
    // SAFETY: диапазон проверен по таблицам программы и доступен ей на запись;
    // пока исполняется вызов, отображение менять некому — ядро сейчас здесь.
    unsafe { core::ptr::write_unaligned(ptr as *mut Dirent, out) };
    1
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
    let node = match crate::fs::resolve_as(super::credentials(), path, Access::NONE) {
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

/// `update(ptr, len) -> номер слота`.
///
/// Вся работа — в [`crate::slot::apply`]: та же функция, которую зовёт команда
/// оболочки, с той же проверкой подписи внутри. Здесь только два решения,
/// которых у оболочки нет.
///
/// Первое: **только root**. Обновление — это запись в чужой раздел и смена
/// того, с чего машина загрузится; программа, работающая от имени человека, не
/// вправе такого заказывать, даже если файл ей читать разрешено.
///
/// Второе: причина отказа печатается **здесь**, а программе уходит один код.
/// Причину знает ядро, и знает целиком — вплоть до имени куска, чей хеш не
/// сошёлся; превращать её в число значило бы завести второй словарь отказов
/// ради того, чтобы программа перевела его обратно в слова, но хуже.
fn update(ptr: usize, len: usize) -> i64 {
    if !super::credentials().is_root() {
        return ERR_PERMISSION;
    }
    let mut buffer = [0u8; MAX_PATH];
    let path = match copy_path(ptr, len, &mut buffer) {
        Ok(path) => path,
        Err(err) => return err,
    };
    match crate::slot::apply(path) {
        // Номер, а не буква: через регистр результата уезжает число. Обратно в
        // букву его переводит программа — тем же соответствием, что записано в
        // договоре.
        Ok(slots::Slot::A) => 0,
        Ok(slots::Slot::B) => 1,
        Err(err) => {
            crate::kprintln!("  sysupdate   : {err}");
            ERR_UPDATE_REFUSED
        }
    }
}

/// Перевести отказ файловой системы в код договора.
// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------

/// Номер соединения TCP, если это он.
fn as_stream(index: usize) -> Option<usize> {
    index.checked_sub(STREAM_FIRST)
}

/// `socket(kind) -> номер сокета`.
fn socket(kind: usize) -> i64 {
    match kind {
        SOCK_UDP => match net::socket_open(sched::current()) {
            Ok(index) => index as i64,
            Err(err) => net_errno(err),
        },
        SOCK_TCP => match net::stream_open(sched::current()) {
            Ok(index) => (index + STREAM_FIRST) as i64,
            Err(err) => net_errno(err),
        },
        _ => ERR_UNSUPPORTED,
    }
}

/// `bind(сокет, порт) -> назначенный порт`.
fn bind(index: usize, port: usize) -> i64 {
    let Ok(port) = u16::try_from(port) else {
        return ERR_BAD_ADDRESS;
    };
    let result = match as_stream(index) {
        Some(stream) => net::stream_bind(sched::current(), stream, port),
        None => net::socket_bind(sched::current(), index, port),
    };
    match result {
        Ok(port) => i64::from(port),
        Err(err) => net_errno(err),
    }
}

/// `connect(сокет, адрес, порт) -> локальный порт или 0`.
fn connect(index: usize, address: usize, port: usize) -> i64 {
    let (Ok(address), Ok(port)) = (u32::try_from(address), u16::try_from(port)) else {
        return ERR_BAD_ADDRESS;
    };
    if let Some(stream) = as_stream(index) {
        // У потока `connect` только начинает рукопожатие: установления связи
        // придётся подождать, и узнать о нём — через `SYS_STREAMSTATE`.
        // Возвращать «готово» здесь значило бы соврать.
        return match net::stream_connect(sched::current(), stream, Ipv4(address), port) {
            Ok(()) => 0,
            Err(err) => net_errno(err),
        };
    }
    match net::socket_connect(sched::current(), index, Ipv4(address), port) {
        Ok(local) => i64::from(local),
        Err(err) => net_errno(err),
    }
}

/// `listen(сокет) -> 0`.
fn listen(index: usize) -> i64 {
    let Some(stream) = as_stream(index) else {
        // Датаграммы не слушают: у них нет соединений, которые можно было бы
        // принимать.
        return ERR_UNSUPPORTED;
    };
    match net::stream_listen(sched::current(), stream) {
        Ok(()) => 0,
        Err(err) => net_errno(err),
    }
}

/// `accept(сокет) -> номер нового соединения`.
fn accept(index: usize) -> i64 {
    let Some(stream) = as_stream(index) else {
        return ERR_UNSUPPORTED;
    };
    match net::stream_accept(sched::current(), stream) {
        Ok(Some(accepted)) => (accepted + STREAM_FIRST) as i64,
        Ok(None) => ERR_AGAIN,
        Err(err) => net_errno(err),
    }
}

/// `shutdown(сокет) -> 0`.
fn shutdown(index: usize) -> i64 {
    let Some(stream) = as_stream(index) else {
        return ERR_UNSUPPORTED;
    };
    match net::stream_shutdown(sched::current(), stream) {
        Ok(()) => 0,
        Err(err) => net_errno(err),
    }
}

/// `streamstate(сокет, out) -> 0`.
fn streamstate(index: usize, out: usize) -> i64 {
    let Some(stream) = as_stream(index) else {
        return ERR_UNSUPPORTED;
    };
    if !space::user_can(out, size_of::<StreamState>(), PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    let (state, peer_closed, reset) = match net::stream_state(sched::current(), stream) {
        Ok(state) => state,
        Err(err) => return net_errno(err),
    };
    let value = StreamState {
        open: u8::from(state.is_open()),
        peer_closed: u8::from(peer_closed),
        reset: u8::from(reset),
        _reserved: 0,
    };
    // SAFETY: диапазон проверен на запись и вмещает структуру целиком.
    unsafe { core::ptr::write_unaligned(out as *mut StreamState, value) };
    0
}

/// `send(сокет, ptr, len) -> len`.
fn send(index: usize, ptr: usize, len: usize) -> i64 {
    if !space::user_can(ptr, len, PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: диапазон проверен по таблицам самой программы; ядро и программа
    // исполняются по очереди на одном процессоре, поэтому менять эти байты во
    // время чтения некому.
    let data = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
    let result = match as_stream(index) {
        Some(stream) => net::stream_send(sched::current(), stream, data),
        None => net::socket_send(sched::current(), index, data),
    };
    match result {
        Ok(sent) => sent as i64,
        Err(err) => net_errno(err),
    }
}

/// `recv(сокет, ptr, len) -> сколько байт`.
fn recv(index: usize, ptr: usize, len: usize) -> i64 {
    if !space::user_can(ptr, len, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }

    if let Some(stream) = as_stream(index) {
        // SAFETY: диапазон проверен по таблицам программы на запись.
        let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
        return match net::stream_recv(sched::current(), stream, out) {
            // Ноль означает «пока ничего»; конец потока программа узнаёт
            // отдельным вызовом, потому что это другое утверждение.
            Ok(0) => ERR_AGAIN,
            Ok(taken) => taken as i64,
            Err(err) => net_errno(err),
        };
    }

    let received = match net::socket_recv(sched::current(), index) {
        // Пустая очередь — это «ещё не сейчас», а не ошибка: программа сама
        // решит, спать ей или сдаться.
        Ok(None) => return ERR_AGAIN,
        Ok(Some(received)) => received,
        Err(err) => return net_errno(err),
    };

    // Датаграмма, не поместившаяся в буфер, **обрезается**, а не откладывается:
    // границы у неё есть, и вторая половина без первой не значит ничего. Так же
    // ведёт себя `recvfrom` везде, где он есть.
    let copied = received.data.len().min(len);
    // SAFETY: диапазон проверен по таблицам программы на запись; копируется не
    // больше, чем в него влезает.
    unsafe {
        core::ptr::copy_nonoverlapping(received.data.as_ptr(), ptr as *mut u8, copied);
    }
    copied as i64
}

/// `peer(сокет, out) -> 0`.
fn peer(index: usize, out: usize) -> i64 {
    if !space::user_can(out, size_of::<Peer>(), PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    let peer = match net::socket_peer(sched::current(), index) {
        Ok(Some(peer)) => peer,
        // Ни одной датаграммы ещё не забрано — спрашивать не о ком.
        Ok(None) => return ERR_AGAIN,
        Err(err) => return net_errno(err),
    };
    let value = Peer { address: peer.0.0, port: peer.1, _reserved: 0 };
    // SAFETY: диапазон проверен на запись и вмещает структуру целиком.
    unsafe { core::ptr::write_unaligned(out as *mut Peer, value) };
    0
}

/// `close_socket(сокет) -> 0`.
fn close_socket(index: usize) -> i64 {
    let result = match as_stream(index) {
        Some(stream) => net::stream_close(sched::current(), stream),
        None => net::socket_close(sched::current(), index),
    };
    match result {
        Ok(()) => 0,
        Err(err) => net_errno(err),
    }
}

/// `netconf(ptr, len) -> 0`.
///
/// Только root: адрес интерфейса — решение о машине целиком, а не о программе,
/// которая его сообщает. Клиент DHCP работает от root именно поэтому, и это
/// записано в описании службы, а не подразумевается.
fn netconf(ptr: usize, len: usize) -> i64 {
    if super::credentials().uid != 0 {
        return ERR_PERMISSION;
    }
    if len != size_of::<NetConfig>() || !space::user_can(ptr, len, PageFlags::READ) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: диапазон проверен по таблицам программы и вмещает структуру.
    let config = unsafe { core::ptr::read_unaligned(ptr as *const NetConfig) };
    match net::configure_all(
        Ipv4(config.address),
        Ipv4(config.netmask),
        Ipv4(config.gateway),
        Ipv4(config.dns),
    ) {
        Ok(()) => 0,
        Err(err) => net_errno(err),
    }
}

/// `netinfo(ptr, len) -> 0`.
///
/// Читать состояние сети вправе любая программа: аппаратный адрес карты и так
/// написан в каждом кадре, который она отправляет, а прятать от программы то,
/// что видно всей подсети, значит усложнять без выгоды.
fn netinfo(ptr: usize, len: usize) -> i64 {
    if len != size_of::<NetInfo>() || !space::user_can(ptr, len, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    let info = match net::status() {
        Some(status) => NetInfo {
            mac: status.mac,
            present: 1,
            _reserved: 0,
            address: status.address.0,
            netmask: status.netmask.0,
            gateway: status.gateway.0,
            dns: status.dns.0,
        },
        // Карты нет — это не ошибка вызова, а ответ на него: программа обязана
        // уметь отличить «сети нет» от «спросить не получилось».
        None => NetInfo::default(),
    };
    // SAFETY: диапазон проверен на запись и вмещает структуру целиком.
    unsafe { core::ptr::write_unaligned(ptr as *mut NetInfo, info) };
    0
}

/// `resolve(ptr, len, out) -> 0`.
fn resolve(ptr: usize, len: usize, out: usize) -> i64 {
    /// Сколько ждать ответа сервера имён.
    const TIMEOUT_MS: u64 = 3_000;

    let mut buffer = [0u8; MAX_PATH];
    let name = match copy_path(ptr, len, &mut buffer) {
        Ok(name) => name,
        Err(err) => return err,
    };
    if !space::user_can(out, 4, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    match net::resolve(name, TIMEOUT_MS) {
        Ok(address) => {
            // SAFETY: четыре байта проверены на запись.
            unsafe {
                core::ptr::copy_nonoverlapping(address.to_bytes().as_ptr(), out as *mut u8, 4);
            }
            0
        }
        Err(err) => net_errno(err),
    }
}

/// `random(ptr, len) -> len`.
///
/// Ограничения на длину нет: программа, попросившая мегабайт случайности,
/// потратит его сама, а пул от этого не истощается — он не расходуемый запас,
/// а состояние, которое прокручивается.
fn random(ptr: usize, len: usize) -> i64 {
    if len == 0 {
        return 0;
    }
    if !space::user_can(ptr, len, PageFlags::WRITE) {
        return ERR_BAD_ADDRESS;
    }
    // SAFETY: диапазон проверен по таблицам программы на запись.
    let out = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
    crate::random::fill(out);
    len as i64
}

fn net_errno(err: NetError) -> i64 {
    match err {
        NetError::NoDevice | NetError::NoAddress | NetError::NoRoute => ERR_NO_NETWORK,
        // «Адрес получателя ещё выясняется» и «в очереди пусто» для программы —
        // одно и то же указание: подождать и спросить снова.
        NetError::Pending => ERR_AGAIN,
        NetError::Timeout => ERR_AGAIN,
        NetError::BadName => ERR_BAD_PATH,
        NetError::NoSuchName => ERR_NOT_FOUND,
        NetError::Device(_) => ERR_IO,
        NetError::Stream(err) => match err {
            crate::net::stream::StreamError::TooMany => ERR_TOO_MANY_FILES,
            crate::net::stream::StreamError::BadStream => ERR_BAD_SOCKET,
            crate::net::stream::StreamError::PortTaken(_) => ERR_EXISTS,
            // «Ещё не установлено», «буфер полон» и «нечего принимать» — это
            // всё «спросите позже», и программа поступает с ними одинаково.
            crate::net::stream::StreamError::NotConnected
            | crate::net::stream::StreamError::WouldBlock => ERR_AGAIN,
            crate::net::stream::StreamError::Reset => ERR_IO,
            crate::net::stream::StreamError::NotListening
            | crate::net::stream::StreamError::Closed => ERR_UNSUPPORTED,
        },
        NetError::Socket(err) => match err {
            crate::net::socket::SocketError::TooMany => ERR_TOO_MANY_FILES,
            crate::net::socket::SocketError::BadSocket => ERR_BAD_SOCKET,
            crate::net::socket::SocketError::PortTaken(_) => ERR_EXISTS,
            crate::net::socket::SocketError::NoPort => ERR_TOO_MANY_FILES,
            crate::net::socket::SocketError::NotBound
            | crate::net::socket::SocketError::NoPeer
            | crate::net::socket::SocketError::TooLong(_) => ERR_BAD_ADDRESS,
        },
    }
}

fn errno(err: FileError) -> i64 {
    match err {
        FileError::NoFilesystem => ERR_NO_FILESYSTEM,
        FileError::BadFd => ERR_BAD_FD,
        FileError::TooManyFiles => ERR_TOO_MANY_FILES,
        FileError::Vfs(err) => vfs_errno(err),
        FileError::BadOffset => ERR_BAD_ADDRESS,
        FileError::NotSeekable => ERR_UNSUPPORTED,
        FileError::Pipe(super::pipe::PipeError::Broken) => ERR_BROKEN_PIPE,
        FileError::Pipe(super::pipe::PipeError::WouldBlock) => ERR_AGAIN,
        FileError::Pipe(super::pipe::PipeError::OutOfMemory) => ERR_NO_SPACE,
    }
}

fn vfs_errno(err: VfsError) -> i64 {
    match err {
        VfsError::NotFound => ERR_NOT_FOUND,
        VfsError::PermissionDenied => ERR_PERMISSION,
        VfsError::BadPath => ERR_BAD_PATH,
        // «Том только на чтение» для программы — тот же отказ, что и «эта ФС
        // так не умеет»: сделать с этим она всё равно ничего не может, а
        // объяснение, почему именно, уже напечатано ядром при загрузке.
        VfsError::WrongKind | VfsError::Unsupported | VfsError::ReadOnly => ERR_UNSUPPORTED,
        VfsError::Exists => ERR_EXISTS,
        VfsError::NotEmpty => ERR_NOT_EMPTY,
        VfsError::NoSpace => ERR_NO_SPACE,
        // Испорченная структура на диске, чтение за концом устройства и отказ
        // самого устройства для программы — одно и то же: носитель не отдал
        // данные. Подробности ушли в журнал ядра, где им и место.
        VfsError::Corrupt | VfsError::OutOfBounds | VfsError::Io | VfsError::OutOfMemory => ERR_IO,
    }
}
