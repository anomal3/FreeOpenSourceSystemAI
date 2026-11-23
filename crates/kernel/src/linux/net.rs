//! Сеть программ Linux (фаза 71f): сокеты `AF_INET` (TCP и UDP) поверх
//! родного стека, `/etc/resolv.conf` и `/etc/hosts` — для резолвера glibc.
//!
//! Сокет — [`Object`] в таблице дескрипторов, как сокеты `AF_UNIX` (фаза
//! 71e), и ждёт так же — `ipc::wait_io` на поколении событий. Под ним —
//! родной [`Sock`], заведённый от имени **процесса** (номер главного потока):
//! его сокеты закрываются, когда кончается процесс. `AF_INET6` — пока
//! `EAFNOSUPPORT`: стек знает только IPv4, и glibc на этот ответ переходит
//! на IPv4 сама.
//!
//! Здесь же особые файлы `/etc` для резолвера и TLS (22.11): `hosts` с именем
//! машины, `hostname`, файл корней в путях Debian/Fedora/OpenSSL; и журнал
//! сети программ (`linux-net`): чем кончилось каждое соединение TCP, каждый
//! запрос DNS и ответ на него — с временем.
//!
//! Функции разбора вызовов отвечают `None`, если за дескриптором не сокет
//! `AF_INET`, — тогда вызов уходит сокетам `AF_UNIX`.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::e;
use super::ipc::{get, put, read_user, wait_io, write_user};
use crate::sync::Mutex;
use crate::user::files::Object;
use crate::user::sock::Sock;

const AF_INET: u16 = 2;
const SOCK_STREAM: usize = 1;
const SOCK_DGRAM: usize = 2;
const SOCK_NONBLOCK: usize = 0o4000;
const SOCK_CLOEXEC: usize = 0o2_000_000;
const S_IFSOCK: u32 = 0o140_000;
const EAFNOSUPPORT: i64 = 97;
const ENOTCONN: i64 = 107;
const EISCONN: i64 = 106;
const EALREADY: i64 = 114;
const EINPROGRESS: i64 = 115;
const ENOPROTOOPT: i64 = 92;
const MSG_DONTWAIT: usize = 0x40;
/// Самая длинная датаграмма, которую принимает стек.
const MAX_DATAGRAM: usize = 64 * 1024;

/// Перевести родную ошибку сокета в `-errno` Linux.
fn errno(native: i64) -> i64 {
    super::errno(native)
}

/// Как идёт соединение.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Link {
    Idle,
    Connecting,
    Connected,
    Listening,
}

pub struct InetSocket {
    sock: Sock,
    link: Mutex<Link>,
    /// Адрес и порт собеседника — как их назвала программа (сетевой порядок
    /// не храним: числа в порядке хоста).
    peer: Mutex<Option<(u32, u16)>>,
    /// Свой порт после `bind` или соединения.
    port: AtomicU32,
    /// Ошибка неждущего соединения — для `SO_ERROR` (отдаётся один раз).
    error: Mutex<i64>,
    /// `shutdown(SHUT_WR)` уже был.
    shut: AtomicBool,
    /// Настройки `setsockopt` как их дала программа (22.11): `getsockopt`
    /// отдаёт их обратно, `SO_RCVTIMEO`/`SO_SNDTIMEO` ограничивают ожидание.
    opts: Mutex<Vec<(usize, usize, Vec<u8>)>>,
    /// Принятое при `MSG_PEEK` (22.11): следующее чтение отдаёт его первым.
    /// TCP — байты потока, UDP — одна датаграмма с отправителем.
    peeked: Mutex<Option<(Vec<u8>, Option<(u32, u16)>)>>,
    /// Когда начато соединение (мс от загрузки) — для строки журнала.
    started: core::sync::atomic::AtomicU64,
    /// Запросы DNS без ответа: номер, имя, тип, когда ушёл (журнал, 22.11).
    dns: Mutex<Vec<(u16, String, u16, u64)>>,
}

impl InetSocket {
    fn tcp(&self) -> bool {
        self.sock.is_tcp()
    }

    /// Соединение, начатое неждущим `connect`, — дошло ли.
    fn settle(&self) {
        let mut link = self.link.lock();
        if *link != Link::Connecting {
            return;
        }
        match self.sock.connected() {
            None => {}
            Some(Ok(())) => {
                *link = Link::Connected;
                drop(link);
                self.log_connect(0);
            }
            Some(Err(err)) => {
                *link = Link::Idle;
                *self.error.lock() = errno(err);
                drop(link);
                self.log_connect(errno(err));
            }
        }
    }

    fn new(sock: Sock, link: Link, peer: Option<(u32, u16)>, port: u32) -> Self {
        Self {
            sock,
            link: Mutex::new(link),
            peer: Mutex::new(peer),
            port: AtomicU32::new(port),
            error: Mutex::new(0),
            shut: AtomicBool::new(false),
            opts: Mutex::new(Vec::new()),
            peeked: Mutex::new(None),
            started: core::sync::atomic::AtomicU64::new(0),
            dns: Mutex::new(Vec::new()),
        }
    }

    /// Значение `setsockopt`, если программа его давала.
    fn opt(&self, level: usize, option: usize) -> Option<Vec<u8>> {
        self.opts.lock().iter().find(|(l, o, _)| *l == level && *o == option).map(|(_, _, value)| value.clone())
    }

    /// Срок ожидания из `SO_RCVTIMEO` (`send == false`) или `SO_SNDTIMEO`:
    /// мс от загрузки, `None` — ждать сколько угодно.
    fn deadline(&self, send: bool) -> Option<u64> {
        let value = self.opt(SOL_SOCKET, if send { SO_SNDTIMEO } else { SO_RCVTIMEO })?;
        if value.len() < 16 {
            return None;
        }
        let secs = u64::from_ne_bytes(value[..8].try_into().ok()?);
        let usecs = u64::from_ne_bytes(value[8..16].try_into().ok()?);
        let ms = secs.saturating_mul(1000).saturating_add(usecs.div_ceil(1000));
        (ms != 0).then(|| crate::time::uptime_ms().saturating_add(ms))
    }

    /// Прочитать с учётом `MSG_PEEK`: подсмотренное отдаётся первым.
    fn take(&self, buf: &mut [u8], peek: bool) -> Result<(usize, Option<(u32, u16)>), i64> {
        let mut peeked = self.peeked.lock();
        if peeked.is_none() && peek {
            // Датаграмма — целиком: подсмотр не должен её обрезать.
            let size = if self.tcp() { buf.len().clamp(1, MAX_DATAGRAM) } else { MAX_DATAGRAM };
            let mut fresh = vec![0u8; size];
            let (count, from) = self.sock.recv_now(&mut fresh).map_err(errno)?;
            if count == 0 && self.tcp() {
                return Ok((0, None));
            }
            fresh.truncate(count);
            *peeked = Some((fresh, from));
        }
        let Some((data, from)) = peeked.as_mut() else {
            drop(peeked);
            return self.sock.recv_now(buf).map_err(errno);
        };
        let from = *from;
        let count = data.len().min(buf.len());
        buf[..count].copy_from_slice(&data[..count]);
        if !peek {
            if self.tcp() && count < data.len() {
                data.drain(..count);
            } else {
                *peeked = None;
            }
        }
        Ok((count, from))
    }

    /// Строка журнала о том, чем кончилось соединение TCP (22.11): отказ и
    /// срок — всегда, удача — тоже (их немного, и в журнале железа видно,
    /// куда ходила игра). Общий предел строк — [`diag_allowed`].
    fn log_connect(&self, err: i64) {
        let started = self.started.swap(0, Ordering::Relaxed);
        if started == 0 {
            return;
        }
        let Some((address, port)) = *self.peer.lock() else { return };
        let took = crate::time::uptime_ms().saturating_sub(started);
        let [a, b, c, d] = address.to_be_bytes();
        if err == 0 {
            diag(format_args!("tcp {a}.{b}.{c}.{d}:{port} connected in {took} ms"));
        } else {
            diag(format_args!("tcp {a}.{b}.{c}.{d}:{port} failed after {took} ms: {}", errname(err)));
        }
    }

    /// Запрос DNS уходит (UDP на порт 53): запомнить имя и время.
    fn dns_sent(&self, data: &[u8]) {
        let Some((id, name, kind)) = dns_question(data) else { return };
        let now = crate::time::uptime_ms();
        let mut dns = self.dns.lock();
        if dns.len() >= 8 {
            dns.remove(0);
        }
        dns.push((id, name, kind, now));
    }

    /// Пришёл ответ DNS: строка журнала с кодом, числом ответов, первым
    /// адресом и временем.
    fn dns_answered(&self, data: &[u8]) {
        if data.len() < 12 {
            return;
        }
        let id = u16::from_be_bytes([data[0], data[1]]);
        let entry = {
            let mut dns = self.dns.lock();
            let Some(at) = dns.iter().position(|entry| entry.0 == id) else { return };
            dns.remove(at)
        };
        let (_, name, kind, sent) = entry;
        let took = crate::time::uptime_ms().saturating_sub(sent);
        let rcode = data[3] & 0xF;
        let answers = u16::from_be_bytes([data[6], data[7]]);
        let first = dns_first_address(data).map(|[a, b, c, d]| alloc::format!(", first {a}.{b}.{c}.{d}")).unwrap_or_default();
        diag(format_args!("dns {} {name}: rcode {rcode}, {answers} answer(s){first} in {took} ms", qtype_name(kind)));
    }
}

impl Drop for InetSocket {
    /// Запросы DNS, на которые ответа так и не было (резолвер закрыл сокет по
    /// сроку), — тоже в журнал.
    fn drop(&mut self) {
        let now = crate::time::uptime_ms();
        for (_, name, kind, sent) in self.dns.lock().drain(..) {
            diag(format_args!("dns {} {name}: no answer, gave up after {} ms", qtype_name(kind), now.saturating_sub(sent)));
        }
    }
}

const SOL_SOCKET: usize = 1;
const SO_RCVTIMEO: usize = 20;
const SO_SNDTIMEO: usize = 21;
const MSG_PEEK: usize = 0x2;
const DNS_PORT: u16 = 53;

/// Строк журнала сети не больше стольких за 10 с (на все программы).
const DIAG_LINES: u32 = 40;

/// Строка журнала сети Linux (22.11, hw-log15: Uot «сервер не отвечает», а
/// в журнале — ни слова о сети): `linux-net   : #задача программа: текст`.
fn diag(text: core::fmt::Arguments) {
    use core::sync::atomic::AtomicU64;
    static WINDOW: AtomicU64 = AtomicU64::new(0);
    static LINES: AtomicU32 = AtomicU32::new(0);
    static DROPPED: AtomicU32 = AtomicU32::new(0);
    let window = crate::time::uptime_ms() / 10_000;
    if WINDOW.swap(window, Ordering::Relaxed) != window {
        LINES.store(0, Ordering::Relaxed);
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped != 0 {
            crate::kprintln!("  linux-net   : {dropped} line(s) dropped (more than {DIAG_LINES} in 10 s)");
        }
    }
    if LINES.fetch_add(1, Ordering::Relaxed) >= DIAG_LINES {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    // Имя задачи, а не программы: строка бывает и из `poll` (готовность
    // соединения), где замок программы может быть уже взят.
    let tid = crate::sched::current().as_u32();
    let name = crate::sched::current_name();
    crate::kprintln!("  linux-net   : #{tid} {name}: {text}");
}

fn errname(err: i64) -> String {
    match -err {
        e::ETIMEDOUT => String::from("timed out"),
        e::ECONNREFUSED => String::from("refused"),
        e::ECONNRESET => String::from("reset"),
        e::ENETDOWN => String::from("network down"),
        other => alloc::format!("errno {other}"),
    }
}

fn qtype_name(kind: u16) -> String {
    match kind {
        1 => String::from("A"),
        28 => String::from("AAAA"),
        5 => String::from("CNAME"),
        12 => String::from("PTR"),
        33 => String::from("SRV"),
        other => alloc::format!("type {other}"),
    }
}

/// Пропустить имя DNS (метки или ссылку сжатия) с `at`; конец имени.
fn dns_skip_name(data: &[u8], mut at: usize) -> Option<usize> {
    loop {
        let len = *data.get(at)? as usize;
        if len == 0 {
            return Some(at + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(at + 2);
        }
        at += 1 + len;
    }
}

/// Номер, имя и тип первого вопроса запроса DNS.
fn dns_question(data: &[u8]) -> Option<(u16, String, u16)> {
    if data.len() < 17 || u16::from_be_bytes([data[4], data[5]]) == 0 {
        return None;
    }
    let mut name = String::new();
    let mut at = 12;
    loop {
        let len = *data.get(at)? as usize;
        if len == 0 || len & 0xC0 != 0 || name.len() > 200 {
            break;
        }
        let label = data.get(at + 1..at + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        name.extend(label.iter().map(|&byte| if byte.is_ascii_graphic() { byte as char } else { '?' }));
        at += 1 + len;
    }
    let end = dns_skip_name(data, 12)?;
    let kind = u16::from_be_bytes([*data.get(end)?, *data.get(end + 1)?]);
    Some((u16::from_be_bytes([data[0], data[1]]), name, kind))
}

/// Первый адрес IPv4 (запись A) в ответе DNS.
fn dns_first_address(data: &[u8]) -> Option<[u8; 4]> {
    let questions = u16::from_be_bytes([data[4], data[5]]);
    let answers = u16::from_be_bytes([data[6], data[7]]);
    let mut at = 12;
    for _ in 0..questions {
        at = dns_skip_name(data, at)? + 4;
    }
    for _ in 0..answers.min(32) {
        at = dns_skip_name(data, at)?;
        let kind = u16::from_be_bytes([*data.get(at)?, *data.get(at + 1)?]);
        let len = u16::from_be_bytes([*data.get(at + 8)?, *data.get(at + 9)?]) as usize;
        at += 10;
        if kind == 1 && len == 4 {
            let raw = data.get(at..at + 4)?;
            return Some([raw[0], raw[1], raw[2], raw[3]]);
        }
        at += len;
    }
    None
}

impl Object for InetSocket {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        self.settle();
        if self.tcp() && *self.link.lock() != Link::Connected {
            return Err(if *self.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
        }
        self.take(buf, false).map(|(count, _)| count)
    }
    fn write(&self, data: &[u8]) -> Result<usize, i64> {
        self.settle();
        if self.tcp() && *self.link.lock() != Link::Connected {
            return Err(if *self.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
        }
        if self.shut.load(Ordering::Relaxed) {
            return Err(-e::EPIPE);
        }
        self.sock.send_now(data).map_err(errno)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        self.settle();
        match *self.link.lock() {
            // Неждущее соединение: готовность к записи — «дошло или нет».
            Link::Connecting => (false, false, false),
            Link::Idle if self.tcp() => (false, !self.error.lock().eq(&0), true),
            _ => {
                let (read, write, hup) = self.sock.readiness();
                (read || self.peeked.lock().is_some(), write, hup)
            }
        }
    }
    fn pending(&self) -> Option<usize> {
        let peeked = self.peeked.lock().as_ref().map_or(0, |(data, _)| data.len());
        if self.tcp() { self.sock.pending().map(|count| count + peeked) } else { None }
    }
    fn mode(&self) -> u32 {
        S_IFSOCK
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn inet_of(fd: usize) -> Option<Result<(Arc<dyn Object>, bool), i64>> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    object.as_any().downcast_ref::<InetSocket>()?;
    Some(Ok((object, nonblock)))
}

fn inet(object: &Arc<dyn Object>) -> &InetSocket {
    object.as_any().downcast_ref::<InetSocket>().expect("inet_of checked the type")
}

/// `socket(AF_INET, type, protocol)`.
pub fn socket(kind: usize) -> i64 {
    let tcp = match kind & 0xf {
        SOCK_STREAM => true,
        SOCK_DGRAM => false,
        _ => return -EAFNOSUPPORT,
    };
    let owner = crate::sched::TaskId::new(crate::user::with_current(|program| program.linux_pid).unwrap_or(0));
    let sock = match Sock::open_as(tcp, owner) {
        Ok(sock) => sock,
        Err(err) => return errno(err),
    };
    let object = Arc::new(InetSocket::new(sock, Link::Idle, None, 0));
    match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, kind & SOCK_NONBLOCK != 0)?;
        program.files.set_cloexec(fd, kind & SOCK_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    }
}

/// `struct sockaddr_in` → адрес и порт (в порядке хоста).
fn address_of(addr: usize, len: usize) -> Result<(u32, u16), i64> {
    if len < 8 {
        return Err(-e::EINVAL);
    }
    let raw = read_user(addr, 8)?;
    if u16::from_ne_bytes([raw[0], raw[1]]) != AF_INET {
        return Err(-EAFNOSUPPORT);
    }
    Ok((u32::from_be_bytes([raw[4], raw[5], raw[6], raw[7]]), u16::from_be_bytes([raw[2], raw[3]])))
}

/// Записать `sockaddr_in` в `addr`, длину — в `len_at`.
fn put_address(addr: usize, len_at: usize, address: u32, port: u16) -> Result<(), i64> {
    if addr == 0 || len_at == 0 {
        return Ok(());
    }
    let room = get::<u32>(len_at)? as usize;
    let mut raw = [0u8; 16];
    raw[..2].copy_from_slice(&AF_INET.to_ne_bytes());
    raw[2..4].copy_from_slice(&port.to_be_bytes());
    raw[4..8].copy_from_slice(&address.to_be_bytes());
    write_user(addr, &raw[..room.min(16)])?;
    put(len_at, 16u32)
}

fn our_address() -> u32 {
    crate::net::status().map_or(0, |status| status.address.0)
}

pub fn connect(fd: usize, addr: usize, len: usize) -> Option<i64> {
    Some(match inet_of(fd)? {
        Err(err) => err,
        Ok((object, nonblock)) => connect_inet(inet(&object), nonblock, addr, len),
    })
}

fn connect_inet(socket: &InetSocket, nonblock: bool, addr: usize, len: usize) -> i64 {
    let (address, port) = match address_of(addr, len) {
        Ok(found) => found,
        Err(err) => return err,
    };
    if !socket.tcp() {
        // UDP: только запомнить собеседника.
        return match socket.sock.start_connect(address, port) {
            Ok(()) => {
                *socket.peer.lock() = Some((address, port));
                0
            }
            Err(err) => errno(err),
        };
    }
    match *socket.link.lock() {
        Link::Connected => return -EISCONN,
        Link::Connecting => return -EALREADY,
        Link::Listening => return -e::EINVAL,
        Link::Idle => {}
    }
    *socket.peer.lock() = Some((address, port));
    socket.started.store(crate::time::uptime_ms().max(1), Ordering::Relaxed);
    if let Err(err) = socket.sock.start_connect(address, port) {
        socket.log_connect(errno(err));
        return errno(err);
    }
    *socket.link.lock() = Link::Connecting;
    if nonblock {
        return -EINPROGRESS;
    }
    // `SO_SNDTIMEO` ограничивает и ждущий `connect` (Linux: `EINPROGRESS`).
    let deadline = socket.deadline(true);
    let done = wait_io(false, || deadline, || match socket.sock.connected() {
        None if deadline.is_some_and(|at| crate::time::uptime_ms() >= at) => Err(TIMED_OUT),
        None => Err(-e::EAGAIN),
        Some(result) => Ok(result),
    });
    match done {
        Ok(Ok(())) => {
            *socket.link.lock() = Link::Connected;
            socket.log_connect(0);
            0
        }
        Ok(Err(err)) => {
            *socket.link.lock() = Link::Idle;
            socket.log_connect(errno(err));
            errno(err)
        }
        Err(TIMED_OUT) => -EINPROGRESS,
        Err(err) => err,
    }
}

/// Знак «вышел срок `SO_RCVTIMEO`/`SO_SNDTIMEO`» внутри `wait_io`: `-EAGAIN`
/// там значит «ждать дальше», поэтому срок — отдельным числом.
const TIMED_OUT: i64 = i64::MIN;

pub fn bind(fd: usize, addr: usize, len: usize) -> Option<i64> {
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    Some(match address_of(addr, len) {
        Ok((_, port)) => match socket.sock.bind(port) {
            Ok(port) => {
                socket.port.store(u32::from(port), Ordering::Relaxed);
                0
            }
            Err(err) => errno(err),
        },
        Err(err) => err,
    })
}

pub fn listen(fd: usize) -> Option<i64> {
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    Some(match socket.sock.listen() {
        Ok(()) => {
            *socket.link.lock() = Link::Listening;
            0
        }
        Err(err) => errno(err),
    })
}

pub fn accept(fd: usize, addr: usize, len: usize, flags: usize) -> Option<i64> {
    let (object, nonblock) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    let accepted = wait_io(nonblock, || None, || socket.sock.accept_now().map_err(errno));
    let sock = match accepted {
        Ok(sock) => sock,
        Err(err) => return Some(err),
    };
    let peer = sock.info().ok().map(|info| (info.remote, info.remote_port));
    if let Some((address, port)) = peer {
        if let Err(err) = put_address(addr, len, address, port) {
            return Some(err);
        }
    }
    let object = Arc::new(InetSocket::new(sock, Link::Connected, peer, socket.port.load(Ordering::Relaxed)));
    Some(match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, flags & SOCK_NONBLOCK != 0)?;
        program.files.set_cloexec(fd, flags & SOCK_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    })
}

pub fn shutdown(fd: usize, how: usize) -> Option<i64> {
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    if how == 0 {
        return Some(0);
    }
    socket.shut.store(true, Ordering::Relaxed);
    Some(if socket.tcp() { socket.sock.shutdown().map_or_else(errno, |()| 0) } else { 0 })
}

pub fn sockname(fd: usize, addr: usize, len: usize, peer: bool) -> Option<i64> {
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    let (address, port) = if peer {
        match *socket.peer.lock() {
            Some(found) => found,
            None => return Some(-ENOTCONN),
        }
    } else {
        let port = socket.sock.info().map_or(0, |info| info.local_port);
        (our_address(), port)
    };
    Some(put_address(addr, len, address, port).map_or_else(|err| err, |()| 0))
}

pub fn getsockopt(fd: usize, level: usize, option: usize, value: usize, len_at: usize) -> Option<i64> {
    const SO_TYPE: usize = 3;
    const SO_ERROR: usize = 4;
    const SO_SNDBUF: usize = 7;
    const SO_RCVBUF: usize = 8;
    const SO_LINGER: usize = 13;
    const SO_RCVLOWAT: usize = 18;
    const SO_SNDLOWAT: usize = 19;
    const SO_ACCEPTCONN: usize = 30;
    const SO_PROTOCOL: usize = 38;
    const SO_DOMAIN: usize = 39;
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    socket.settle();
    let int = |value: i32| value.to_ne_bytes().to_vec();
    let answer: Vec<u8> = match (level, option) {
        (SOL_SOCKET, SO_TYPE) => int(if socket.tcp() { 1 } else { 2 }),
        (SOL_SOCKET, SO_ERROR) => {
            let mut error = socket.error.lock();
            let value = -*error;
            *error = 0;
            int(value as i32)
        }
        (SOL_SOCKET, SO_DOMAIN) => int(i32::from(AF_INET)),
        (SOL_SOCKET, SO_PROTOCOL) => int(if socket.tcp() { 6 } else { 17 }),
        (SOL_SOCKET, SO_ACCEPTCONN) => int(i32::from(*socket.link.lock() == Link::Listening)),
        // Что программа поставила сама — то и отдать (22.11: Wine и Mono
        // читают `SO_KEEPALIVE`, `SO_REUSEADDR`, сроки).
        _ if socket.opt(level, option).is_some() => socket.opt(level, option).unwrap_or_default(),
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => int(64 * 1024),
        (SOL_SOCKET, SO_LINGER) => vec![0; 8],
        (SOL_SOCKET, SO_RCVTIMEO | SO_SNDTIMEO) => vec![0; 16],
        (SOL_SOCKET, SO_RCVLOWAT | SO_SNDLOWAT) => int(1),
        // `SO_REUSEADDR`, `SO_KEEPALIVE`, `SO_BROADCAST`, `SO_OOBINLINE`,
        // `SO_DONTROUTE` — выключены, пока их не включили.
        (SOL_SOCKET, 2 | 5 | 6 | 9 | 10 | 15) => int(0),
        (SOL_SOCKET, _) => return Some(-ENOPROTOOPT),
        // Прочие уровни (`IPPROTO_TCP`: `TCP_NODELAY` и т. п.) — ноль.
        _ => int(0),
    };
    let room = match get::<u32>(len_at) {
        Ok(room) => room as usize,
        Err(err) => return Some(err),
    };
    let take = room.min(answer.len());
    Some(write_user(value, &answer[..take]).and_then(|()| put(len_at, take as u32)).map_or_else(|err| err, |()| 0))
}

/// `setsockopt` сокета `AF_INET` (22.11): значение запоминается — для
/// `getsockopt` и сроков `SO_RCVTIMEO`/`SO_SNDTIMEO`; действия у остальных
/// нет (стек сам решает про `TCP_NODELAY`, буферы и проверку связи).
pub fn setsockopt(fd: usize, level: usize, option: usize, value: usize, len: usize) -> Option<i64> {
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    if len > 256 {
        return Some(-e::EINVAL);
    }
    let bytes = match read_user(value, len) {
        Ok(bytes) => bytes,
        Err(err) => return Some(err),
    };
    let mut opts = socket.opts.lock();
    opts.retain(|(l, o, _)| !(*l == level && *o == option));
    opts.push((level, option, bytes));
    Some(0)
}

/// Есть ли за дескриптором сокет `AF_INET` (`setsockopt` принимает всё).
pub fn is_inet(fd: usize) -> bool {
    matches!(inet_of(fd), Some(Ok(_)))
}

/// Отправить `data`: UDP с адресом — тому, кого назвали; TCP — как `write`.
fn send(socket: &InetSocket, nonblock: bool, data: &[u8], to: Option<(u32, u16)>) -> i64 {
    if let Some((address, port)) = to.filter(|_| !socket.tcp()) {
        if let Err(err) = socket.sock.start_connect(address, port) {
            return errno(err);
        }
    }
    if !socket.tcp() && to.or(*socket.peer.lock()).is_some_and(|(_, port)| port == DNS_PORT) {
        socket.dns_sent(data);
    }
    let deadline = if nonblock { None } else { socket.deadline(true) };
    let mut done = 0;
    loop {
        let sent = wait_io(nonblock, || deadline, || {
            socket.settle();
            if socket.tcp() && *socket.link.lock() != Link::Connected {
                return Err(if *socket.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
            }
            match socket.sock.send_now(&data[done..]).map_err(errno) {
                Err(err) if err == -e::EAGAIN && deadline.is_some_and(|at| crate::time::uptime_ms() >= at) => Err(TIMED_OUT),
                other => other,
            }
        });
        let sent = if sent == Err(TIMED_OUT) { Err(-e::EAGAIN) } else { sent };
        match sent {
            Ok(count) => {
                done += count;
                if done >= data.len() || nonblock || !socket.tcp() || count == 0 {
                    return done as i64;
                }
            }
            Err(_) if done > 0 => return done as i64,
            Err(err) => return err,
        }
    }
}

/// Принять: `(байт, отправитель)`. `flags` — `MSG_PEEK` (данные остаются
/// для следующего чтения); срок ожидания — `SO_RCVTIMEO` (22.11).
fn recv(socket: &InetSocket, nonblock: bool, flags: usize, buf: &mut [u8]) -> Result<(usize, Option<(u32, u16)>), i64> {
    let deadline = if nonblock { None } else { socket.deadline(false) };
    let got = wait_io(nonblock, || deadline, || {
        socket.settle();
        if socket.tcp() && *socket.link.lock() != Link::Connected {
            return Err(if *socket.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
        }
        match socket.take(buf, flags & MSG_PEEK != 0) {
            Err(err) if err == -e::EAGAIN && deadline.is_some_and(|at| crate::time::uptime_ms() >= at) => Err(TIMED_OUT),
            other => other,
        }
    });
    if let Ok((count, Some((_, DNS_PORT)))) = got {
        if !socket.tcp() && flags & MSG_PEEK == 0 {
            socket.dns_answered(&buf[..count]);
        }
    }
    if got == Err(TIMED_OUT) { Err(-e::EAGAIN) } else { got }
}

pub fn sendto(fd: usize, buf: usize, len: usize, flags: usize, addr: usize, addr_len: usize) -> Option<i64> {
    let (object, nonblock) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let to = if addr != 0 {
        match address_of(addr, addr_len) {
            Ok(found) => Some(found),
            Err(err) => return Some(err),
        }
    } else {
        None
    };
    let data = match read_user(buf, len) {
        Ok(data) => data,
        Err(err) => return Some(err),
    };
    Some(send(inet(&object), nonblock || flags & MSG_DONTWAIT != 0, &data, to))
}

pub fn recvfrom(fd: usize, buf: usize, len: usize, flags: usize, addr: usize, len_at: usize) -> Option<i64> {
    let (object, nonblock) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    let mut buffer = vec![0u8; len.min(MAX_DATAGRAM)];
    Some(match recv(socket, nonblock || flags & MSG_DONTWAIT != 0, flags, &mut buffer) {
        Ok((count, from)) => {
            let from = from.or(*socket.peer.lock());
            let written = write_user(buf, &buffer[..count]).and_then(|()| match from {
                Some((address, port)) => put_address(addr, len_at, address, port),
                None => Ok(()),
            });
            written.map_or_else(|err| err, |()| count as i64)
        }
        Err(err) => err,
    })
}

/// `struct msghdr` x86-64: имя, длина имени, `iov`, их число, управляющие
/// данные, их длина, флаги.
struct Message {
    name: usize,
    name_len: u32,
    parts: Vec<(usize, usize)>,
}

fn message(msg: usize) -> Result<Message, i64> {
    let iov = get::<usize>(msg + 16)?;
    let count = get::<usize>(msg + 24)?;
    if count > 1024 {
        return Err(-e::EINVAL);
    }
    let mut parts = Vec::new();
    for index in 0..count {
        parts.push((get::<usize>(iov + index * 16)?, get::<usize>(iov + index * 16 + 8)?));
    }
    Ok(Message { name: get::<usize>(msg)?, name_len: get::<u32>(msg + 8)?, parts })
}

fn sendmsg_inet(socket: &InetSocket, nonblock: bool, msg: usize) -> i64 {
    let message = match message(msg) {
        Ok(message) => message,
        Err(err) => return err,
    };
    let to = if message.name != 0 {
        match address_of(message.name, message.name_len as usize) {
            Ok(found) => Some(found),
            Err(err) => return err,
        }
    } else {
        None
    };
    let mut data = Vec::new();
    for (addr, len) in message.parts {
        match read_user(addr, len) {
            Ok(part) => data.extend_from_slice(&part),
            Err(err) => return err,
        }
    }
    send(socket, nonblock, &data, to)
}

pub fn sendmsg(fd: usize, msg: usize, flags: usize) -> Option<i64> {
    let (object, nonblock) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    Some(sendmsg_inet(inet(&object), nonblock || flags & MSG_DONTWAIT != 0, msg))
}

/// `sendmmsg(fd, msgvec, n, flags)` — резолвер glibc шлёт так запросы A и
/// AAAA разом. `struct mmsghdr` — `msghdr` (56 байт) и число отправленных.
pub fn sendmmsg(fd: usize, vec: usize, count: usize, flags: usize) -> i64 {
    let (object, nonblock) = match inet_of(fd) {
        Some(Ok(found)) => found,
        Some(Err(err)) => return err,
        None => return -e::ENOTSOCK,
    };
    let mut sent = 0;
    for index in 0..count.min(1024) {
        let entry = vec + index * 64;
        let result = sendmsg_inet(inet(&object), nonblock || flags & MSG_DONTWAIT != 0, entry);
        if result < 0 {
            return if sent > 0 { sent as i64 } else { result };
        }
        if put(entry + 56, result as u32).is_err() {
            return -e::EFAULT;
        }
        sent += 1;
    }
    sent as i64
}

pub fn recvmsg(fd: usize, msg: usize, flags: usize) -> Option<i64> {
    let (object, nonblock) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    let message = match message(msg) {
        Ok(message) => message,
        Err(err) => return Some(err),
    };
    let total: usize = message.parts.iter().map(|part| part.1).sum::<usize>().min(MAX_DATAGRAM);
    let mut buffer = vec![0u8; total];
    let (count, from) = match recv(socket, nonblock || flags & MSG_DONTWAIT != 0, flags, &mut buffer) {
        Ok(got) => got,
        Err(err) => return Some(err),
    };
    let mut done = 0;
    for (addr, len) in &message.parts {
        if done >= count {
            break;
        }
        let take = (*len).min(count - done);
        if let Err(err) = write_user(*addr, &buffer[done..done + take]) {
            return Some(err);
        }
        done += take;
    }
    let from = from.or(*socket.peer.lock());
    let result = (|| -> Result<(), i64> {
        if message.name != 0 {
            match from {
                Some((address, port)) => {
                    let mut raw = [0u8; 16];
                    raw[..2].copy_from_slice(&AF_INET.to_ne_bytes());
                    raw[2..4].copy_from_slice(&port.to_be_bytes());
                    raw[4..8].copy_from_slice(&address.to_be_bytes());
                    write_user(message.name, &raw[..(message.name_len as usize).min(16)])?;
                    put(msg + 8, 16u32)?;
                }
                None => put(msg + 8, 0u32)?,
            }
        }
        put(msg + 40, 0usize)?;
        put(msg + 48, 0i32)
    })();
    Some(result.map_or_else(|err| err, |()| count as i64))
}

/// `/etc/resolv.conf` — сервер имён из DHCP; `/etc/hosts` — только сама
/// машина. glibc читает их на каждом `getaddrinfo` (фаза 71f).
pub fn special_file(linux: &str) -> Option<Vec<u8>> {
    match linux {
        "/etc/resolv.conf" => {
            let mut text = String::new();
            if let Some(dns) = crate::net::dns_server() {
                let [a, b, c, d] = dns.to_bytes();
                text = alloc::format!("nameserver {a}.{b}.{c}.{d}\n");
            }
            text.push_str("options attempts:2 timeout:3\n");
            Some(text.into_bytes())
        }
        // Имя машины (`uname`, [`HOSTNAME`]) — на `127.0.1.1`, как в Debian
        // (22.11, hw-log15): иначе `getaddrinfo(gethostname())` уходил в DNS и
        // ждал до 6 с отказа, а Wine писал «Failed to resolve your host name IP».
        "/etc/hosts" => Some(
            alloc::format!("127.0.0.1\tlocalhost\n127.0.1.1\t{HOSTNAME}\n::1\tlocalhost ip6-localhost ip6-loopback\n").into_bytes(),
        ),
        "/etc/hostname" => Some(alloc::format!("{HOSTNAME}\n").into_bytes()),
        "/etc/nsswitch.conf" => Some(b"hosts: files dns\n".to_vec()),
        // Корневые сертификаты для программ Linux (22.11, hw-log15: Uot
        // «сервер не отвечает» под Proton). Пути, где их ищут crypt32 Wine
        // (хранилище ROOT для schannel, WinHTTP и TLS Unity), GnuTLS и
        // OpenSSL Debian, Fedora и прочие.
        path if CA_BUNDLE_PATHS.contains(&path) => Some(ca_bundle()),
        _ => None,
    }
}

/// Имя машины для программ Linux — то же, что отдаёт `uname` (`linux/mod.rs`).
pub const HOSTNAME: &str = "fossai";

/// Где программы Linux ищут общий файл корней.
const CA_BUNDLE_PATHS: [&str; 6] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/ssl/cert.pem",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/tls/cert.pem",
    "/etc/ca-certificates/extracted/tls-ca-bundle.pem",
    "/usr/lib/ssl/cert.pem",
];

/// Корни Mozilla (`assets/ca/cacert.pem` с curl.se, MPL 2.0) — в ядре, чтобы
/// были и на живой флешке, и без пакетов.
static MOZILLA_ROOTS: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/ca/cacert.pem"));

/// Файл корней: корни Mozilla и следом свои корни машины — `/etc/ca.pem`
/// (правка человека; стенд кладёт туда свой корень). Эталон образа
/// (`/usr/share/defaults/etc/ca.pem`) — подмножество Mozilla, его не надо.
fn ca_bundle() -> Vec<u8> {
    let mut out = MOZILLA_ROOTS.to_vec();
    if let Some(Ok((own, _))) = crate::fs::read("/etc/ca.pem", 4 << 20) {
        out.extend_from_slice(b"\n# /etc/ca.pem of this machine\n");
        out.extend_from_slice(&own);
        out.push(b'\n');
    }
    out
}
