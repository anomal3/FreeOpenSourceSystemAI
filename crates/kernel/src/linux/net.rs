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
            Some(Ok(())) => *link = Link::Connected,
            Some(Err(err)) => {
                *link = Link::Idle;
                *self.error.lock() = errno(err);
            }
        }
    }
}

impl Object for InetSocket {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        self.settle();
        if self.tcp() && *self.link.lock() != Link::Connected {
            return Err(if *self.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
        }
        self.sock.recv_now(buf).map(|(count, _)| count).map_err(errno)
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
            _ => self.sock.readiness(),
        }
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
    let object = Arc::new(InetSocket {
        sock,
        link: Mutex::new(Link::Idle),
        peer: Mutex::new(None),
        port: AtomicU32::new(0),
        error: Mutex::new(0),
        shut: AtomicBool::new(false),
    });
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
    if let Err(err) = socket.sock.start_connect(address, port) {
        return errno(err);
    }
    *socket.peer.lock() = Some((address, port));
    *socket.link.lock() = Link::Connecting;
    if nonblock {
        return -EINPROGRESS;
    }
    let done = wait_io(false, || None, || match socket.sock.connected() {
        None => Err(-e::EAGAIN),
        Some(result) => Ok(result),
    });
    match done {
        Ok(Ok(())) => {
            *socket.link.lock() = Link::Connected;
            0
        }
        Ok(Err(err)) => {
            *socket.link.lock() = Link::Idle;
            errno(err)
        }
        Err(err) => err,
    }
}

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
    let object = Arc::new(InetSocket {
        sock,
        link: Mutex::new(Link::Connected),
        peer: Mutex::new(peer),
        port: AtomicU32::new(socket.port.load(Ordering::Relaxed)),
        error: Mutex::new(0),
        shut: AtomicBool::new(false),
    });
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
    const SOL_SOCKET: usize = 1;
    const SO_TYPE: usize = 3;
    const SO_ERROR: usize = 4;
    const SO_SNDBUF: usize = 7;
    const SO_RCVBUF: usize = 8;
    const SO_DOMAIN: usize = 39;
    let (object, _) = match inet_of(fd)? {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    let socket = inet(&object);
    socket.settle();
    let answer: i32 = match (level, option) {
        (SOL_SOCKET, SO_TYPE) => if socket.tcp() { 1 } else { 2 },
        (SOL_SOCKET, SO_ERROR) => {
            let mut error = socket.error.lock();
            let value = -*error;
            *error = 0;
            value as i32
        }
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => 64 * 1024,
        (SOL_SOCKET, SO_DOMAIN) => i32::from(AF_INET),
        // Прочие уровни (`IPPROTO_TCP`: `TCP_NODELAY` и т. п.) — ноль.
        (SOL_SOCKET, _) => return Some(-ENOPROTOOPT),
        _ => 0,
    };
    let room = match get::<u32>(len_at) {
        Ok(room) => room as usize,
        Err(err) => return Some(err),
    };
    let bytes = answer.to_ne_bytes();
    let take = room.min(4);
    Some(write_user(value, &bytes[..take]).and_then(|()| put(len_at, take as u32)).map_or_else(|err| err, |()| 0))
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
    let mut done = 0;
    loop {
        let sent = wait_io(nonblock, || None, || {
            socket.settle();
            if socket.tcp() && *socket.link.lock() != Link::Connected {
                return Err(if *socket.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
            }
            socket.sock.send_now(&data[done..]).map_err(errno)
        });
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

/// Принять: `(байт, отправитель)`.
fn recv(socket: &InetSocket, nonblock: bool, buf: &mut [u8]) -> Result<(usize, Option<(u32, u16)>), i64> {
    wait_io(nonblock, || None, || {
        socket.settle();
        if socket.tcp() && *socket.link.lock() != Link::Connected {
            return Err(if *socket.link.lock() == Link::Connecting { -e::EAGAIN } else { -ENOTCONN });
        }
        socket.sock.recv_now(buf).map_err(errno)
    })
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
    Some(match recv(socket, nonblock || flags & MSG_DONTWAIT != 0, &mut buffer) {
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
    let (count, from) = match recv(socket, nonblock || flags & MSG_DONTWAIT != 0, &mut buffer) {
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
        "/etc/hosts" => Some(b"127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost\n".to_vec()),
        "/etc/nsswitch.conf" => Some(b"hosts: files dns\n".to_vec()),
        _ => None,
    }
}
