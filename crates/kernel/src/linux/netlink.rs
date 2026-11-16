//! Сокеты `AF_NETLINK` / `NETLINK_ROUTE` программ Linux (16.11.2026): список
//! интерфейсов и адресов для glibc — `getifaddrs`, `if_nameindex`,
//! `getaddrinfo` с `AI_ADDRCONFIG`. На них стоит `nsiproxy.sys` Wine 11: сам
//! Wine netlink не открывает, но таблицу интерфейсов берёт у этих функций.
//!
//! Ядра за сокетом нет: ответ собирается прямо в `sendto`/`sendmsg` из одного
//! снимка [`crate::net::interfaces`] и кладётся в очередь датаграмм сокета, а
//! `recvmsg` её разбирает. Поэтому на каждый запрос приходит хотя бы одна
//! датаграмма (ответ, `NLMSG_DONE` или `NLMSG_ERROR`), и ждущий `recvmsg`
//! после запроса не виснет. Подписки (`nl_groups`) принимаются и молчат:
//! событий об изменении сети нет — их никто из glibc и Wine 11 не ждёт.
//!
//! Раскладка байт — как у Linux (`include/uapi/linux/{netlink,rtnetlink,
//! if_link,if_addr}.h`), всё в порядке хоста, кроме адресов IPv4 (сетевой).
//! Две ловушки glibc: адрес, чей `ifa_index` не встретился в списке
//! интерфейсов, и флаг `NLM_F_DUMP_INTR` заставляют `getifaddrs` начинать
//! заново без конца — поэтому оба списка из одного снимка, а флага нет вовсе.
//!
//! Функции разбора вызовов отвечают `None`, если за дескриптором не сокет
//! netlink, — тогда вызов уходит сокетам `AF_INET` и `AF_UNIX`.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use super::e;
use super::ipc::{get, put, read_user, wait_io, write_user};
use crate::net::ipv4::Ipv4;
use crate::net::IfInfo;
use crate::sync::{Mutex, SpinLock};
use crate::user::files::Object;

pub const AF_NETLINK: u16 = 16;
const SOCK_DGRAM: usize = 2;
const SOCK_RAW: usize = 3;
const SOCK_NONBLOCK: usize = 0o4000;
const SOCK_CLOEXEC: usize = 0o2_000_000;
const S_IFSOCK: u32 = 0o140_000;
const NETLINK_ROUTE: usize = 0;

const EPERM: i64 = 1;
const ENODEV: i64 = 19;
const ENOBUFS: i64 = 105;
const ENOPROTOOPT: i64 = 92;
const EPROTONOSUPPORT: i64 = 93;
const ESOCKTNOSUPPORT: i64 = 94;
const EOPNOTSUPP: i64 = 95;
const EADDRINUSE: i64 = 98;
const ECONNREFUSED: i64 = 111;

const MSG_PEEK: usize = 0x2;
const MSG_TRUNC: usize = 0x20;
const MSG_DONTWAIT: usize = 0x40;

// Заголовок `nlmsghdr` и служебные типы.
const NLMSG_HDRLEN: usize = 16;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_MULTI: u16 = 2;
const NLM_F_ACK: u16 = 4;
/// `NLM_F_ROOT | NLM_F_MATCH`: любой из двух — «выдать список».
const NLM_F_DUMP: u16 = 0x300;
/// В `NLMSG_ERROR`: копия запроса урезана до заголовка.
const NLM_F_CAPPED: u16 = 0x100;

// rtnetlink: типы сообщений.
const RTM_BASE: u16 = 16;
const RTM_NEWLINK: u16 = 16;
const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const RTM_NEWROUTE: u16 = 24;
const RTM_GETROUTE: u16 = 26;
/// Последний тип, меняющий адреса, интерфейсы и маршруты (`RTM_SETLINK` …
/// `RTM_DELROUTE` и до `RTM_GETNEIGH`): на них — `EPERM`.
const RTM_LAST_SET: u16 = 31;

const AF_UNSPEC: u8 = 0;
const AF_INET: u8 = 2;

// Атрибуты интерфейса (`IFLA_*`), адреса (`IFA_*`) и маршрута (`RTA_*`).
const IFLA_ADDRESS: u16 = 1;
const IFLA_BROADCAST: u16 = 2;
const IFLA_IFNAME: u16 = 3;
const IFLA_MTU: u16 = 4;
const IFLA_OPERSTATE: u16 = 16;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFA_BROADCAST: u16 = 4;
const RTA_DST: u16 = 1;
const RTA_OIF: u16 = 4;
const RTA_GATEWAY: u16 = 5;
const RTA_PREFSRC: u16 = 7;
const RTA_TABLE: u16 = 15;

const IFA_F_PERMANENT: u8 = 0x80;
const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_SCOPE_LINK: u8 = 253;
const RT_SCOPE_HOST: u8 = 254;
const RT_TABLE_MAIN: u8 = 254;
const RTPROT_KERNEL: u8 = 2;
const RTPROT_BOOT: u8 = 3;
const RTN_UNICAST: u8 = 1;
const IF_OPER_UNKNOWN: u8 = 0;
const IF_OPER_DOWN: u8 = 2;
const IF_OPER_UP: u8 = 6;

/// Датаграмма ответа не длиннее этого: glibc читает в буфер 8 КиБ (старая —
/// по размеру из `MSG_PEEK|MSG_TRUNC`) и на `MSG_TRUNC` в ответе сдаётся.
const DATAGRAM_MAX: usize = 4000;
/// Сколько датаграмм ждёт в очереди сокета, пока их не прочитали: запросы без
/// чтения ответов — ошибка программы, память ядра на неё не тратим.
const QUEUE_MAX: usize = 256;

/// Занятые номера (`nl_pid`) сокетов netlink — по всей системе, как у Linux.
static PIDS: SpinLock<Vec<u32>> = SpinLock::new(Vec::new());
/// Следующий номер для второго и дальнейших сокетов процесса: первый
/// получает номер процесса, остальные — из верхней половины, где номеров
/// процессов не бывает.
static NEXT_PID: AtomicU32 = AtomicU32::new(0x8000_1000);

pub struct NetlinkSocket {
    /// `nl_pid`: ноль — ещё не привязан (`bind` или первый `sendto`).
    pid: AtomicU32,
    /// Подписки `nl_groups`: хранятся для `getsockname`, событий не будет.
    groups: AtomicU32,
    /// `SOCK_RAW` или `SOCK_DGRAM` — для `SO_TYPE`.
    kind: u32,
    /// Ответы — каждая запись одна датаграмма.
    queue: Mutex<VecDeque<Vec<u8>>>,
}

impl Drop for NetlinkSocket {
    fn drop(&mut self) {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid != 0 {
            PIDS.lock().retain(|other| *other != pid);
        }
    }
}

impl NetlinkSocket {
    /// Привязать к номеру, если ещё не привязан («autobind» Linux): первый
    /// сокет процесса — номер процесса, следующие — свободный из верхних.
    fn autobind(&self) -> u32 {
        let current = self.pid.load(Ordering::Acquire);
        if current != 0 {
            return current;
        }
        let tgid = crate::user::with_current(|program| program.linux_pid).unwrap_or(0);
        let mut pids = PIDS.lock();
        let mut pid = tgid;
        while pid == 0 || pids.contains(&pid) {
            pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        }
        // Два потока привязывают один сокет разом: побеждает первый.
        match self.pid.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                pids.push(pid);
                pid
            }
            Err(other) => other,
        }
    }

    /// Разобрать запрос и положить ответы в очередь.
    fn request(&self, data: &[u8]) -> Result<(), i64> {
        let pid = self.autobind();
        let replies = handle_request(data, pid)?;
        if replies.is_empty() {
            return Ok(());
        }
        let mut queue = self.queue.lock();
        if queue.len() + replies.len() > QUEUE_MAX {
            return Err(-ENOBUFS);
        }
        queue.extend(replies);
        drop(queue);
        // Готовность к чтению сменилась — разбудить `poll` из другого потока.
        crate::io_events::notify();
        Ok(())
    }

    /// Первая датаграмма очереди: вынуть или (`peek`) только скопировать.
    fn take(&self, peek: bool) -> Result<Vec<u8>, i64> {
        let mut queue = self.queue.lock();
        let datagram = if peek { queue.front().cloned() } else { queue.pop_front() };
        datagram.ok_or(-e::EAGAIN)
    }
}

impl Object for NetlinkSocket {
    /// `read(2)`: датаграмма целиком или её начало (остальное теряется, как у
    /// `SOCK_DGRAM`).
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        let datagram = self.take(false)?;
        let count = datagram.len().min(buf.len());
        buf[..count].copy_from_slice(&datagram[..count]);
        Ok(count)
    }
    /// `write(2)`: тот же запрос, что `send`.
    fn write(&self, data: &[u8]) -> Result<usize, i64> {
        self.request(data)?;
        Ok(data.len())
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (!self.queue.lock().is_empty(), true, false)
    }
    fn pending(&self) -> Option<usize> {
        Some(self.queue.lock().front().map_or(0, Vec::len))
    }
    fn mode(&self) -> u32 {
        S_IFSOCK
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn netlink_of(fd: usize) -> Option<(Arc<dyn Object>, bool)> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    object.as_any().downcast_ref::<NetlinkSocket>()?;
    Some((object, nonblock))
}

fn netlink(object: &Arc<dyn Object>) -> &NetlinkSocket {
    object.as_any().downcast_ref::<NetlinkSocket>().expect("netlink_of checked the type")
}

/// Есть ли за дескриптором сокет netlink.
pub fn is_netlink(fd: usize) -> bool {
    netlink_of(fd).is_some()
}

/// `socket(AF_NETLINK, type, protocol)`: только `NETLINK_ROUTE`. Прочие
/// (`NETLINK_KOBJECT_UEVENT` libudev, `NETLINK_GENERIC`, `NETLINK_AUDIT`) —
/// `EPROTONOSUPPORT`: их пользователи тогда честно обходятся без них, как
/// раньше на `EAFNOSUPPORT`.
pub fn socket(kind: usize, protocol: usize) -> i64 {
    let base = kind & 0xf;
    if base != SOCK_RAW && base != SOCK_DGRAM {
        return -ESOCKTNOSUPPORT;
    }
    if protocol != NETLINK_ROUTE {
        return -EPROTONOSUPPORT;
    }
    let object = Arc::new(NetlinkSocket {
        pid: AtomicU32::new(0),
        groups: AtomicU32::new(0),
        kind: base as u32,
        queue: Mutex::new(VecDeque::new()),
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

/// `struct sockaddr_nl` (12 байт): семейство, пусто, `nl_pid`, `nl_groups`.
fn address_of(addr: usize, len: usize) -> Result<(u32, u32), i64> {
    if len < 12 {
        return Err(-e::EINVAL);
    }
    let raw = read_user(addr, 12)?;
    if u16::from_ne_bytes([raw[0], raw[1]]) != AF_NETLINK {
        return Err(-e::EINVAL);
    }
    Ok((
        u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]),
        u32::from_ne_bytes([raw[8], raw[9], raw[10], raw[11]]),
    ))
}

fn sockaddr(pid: u32, groups: u32) -> [u8; 12] {
    let mut raw = [0u8; 12];
    raw[..2].copy_from_slice(&AF_NETLINK.to_ne_bytes());
    raw[4..8].copy_from_slice(&pid.to_ne_bytes());
    raw[8..12].copy_from_slice(&groups.to_ne_bytes());
    raw
}

/// Записать `sockaddr_nl` в `addr` (не больше, чем там места), длину 12 — в
/// `len_at`.
fn put_address(addr: usize, len_at: usize, pid: u32, groups: u32) -> Result<(), i64> {
    if addr == 0 || len_at == 0 {
        return Ok(());
    }
    let room = get::<u32>(len_at)? as usize;
    write_user(addr, &sockaddr(pid, groups)[..room.min(12)])?;
    put(len_at, 12u32)
}

pub fn bind(fd: usize, addr: usize, len: usize) -> Option<i64> {
    let (object, _) = netlink_of(fd)?;
    let socket = netlink(&object);
    let (pid, groups) = match address_of(addr, len) {
        Ok(found) => found,
        Err(err) => return Some(err),
    };
    if pid != 0 {
        let current = socket.pid.load(Ordering::Acquire);
        if current != pid {
            if current != 0 {
                return Some(-e::EINVAL);
            }
            let mut pids = PIDS.lock();
            if pids.contains(&pid) {
                return Some(-EADDRINUSE);
            }
            if socket.pid.compare_exchange(0, pid, Ordering::AcqRel, Ordering::Acquire).is_err() {
                return Some(-e::EINVAL);
            }
            pids.push(pid);
        }
    } else {
        socket.autobind();
    }
    socket.groups.store(groups, Ordering::Relaxed);
    Some(0)
}

/// `connect`: собеседник у netlink один — ядро; запомнить нечего.
pub fn connect(fd: usize, addr: usize, len: usize) -> Option<i64> {
    let (object, _) = netlink_of(fd)?;
    Some(match address_of(addr, len) {
        Ok(_) => {
            netlink(&object).autobind();
            0
        }
        Err(err) => err,
    })
}

/// `getsockname` — свой номер (ноль до привязки, как у Linux); `getpeername`
/// — ядро: `{AF_NETLINK, 0, 0}`. glibc (`__netlink_assert_response`)
/// проверяет здесь семейство и без `AF_NETLINK` падает.
pub fn sockname(fd: usize, addr: usize, len_at: usize, peer: bool) -> Option<i64> {
    let (object, _) = netlink_of(fd)?;
    let socket = netlink(&object);
    let (pid, groups) = if peer {
        (0, 0)
    } else {
        (socket.pid.load(Ordering::Acquire), socket.groups.load(Ordering::Relaxed))
    };
    Some(put_address(addr, len_at, pid, groups).map_or_else(|err| err, |()| 0))
}

pub fn getsockopt(fd: usize, level: usize, option: usize, value: usize, len_at: usize) -> Option<i64> {
    const SOL_SOCKET: usize = 1;
    const SO_TYPE: usize = 3;
    const SO_ERROR: usize = 4;
    const SO_SNDBUF: usize = 7;
    const SO_RCVBUF: usize = 8;
    const SO_PASSCRED: usize = 16;
    const SO_PROTOCOL: usize = 38;
    const SO_DOMAIN: usize = 39;
    const SOL_NETLINK: usize = 270;
    let (object, _) = netlink_of(fd)?;
    let socket = netlink(&object);
    let answer: i32 = match (level, option) {
        (SOL_SOCKET, SO_TYPE) => socket.kind as i32,
        (SOL_SOCKET, SO_ERROR | SO_PASSCRED) => 0,
        (SOL_SOCKET, SO_SNDBUF | SO_RCVBUF) => 212_992,
        (SOL_SOCKET, SO_PROTOCOL) => NETLINK_ROUTE as i32,
        (SOL_SOCKET, SO_DOMAIN) => i32::from(AF_NETLINK),
        (SOL_SOCKET, _) => return Some(-ENOPROTOOPT),
        // Флаги `SOL_NETLINK` (`NETLINK_EXT_ACK`, `NETLINK_GET_STRICT_CHK` …)
        // — выключены.
        (SOL_NETLINK, _) => 0,
        _ => return Some(-ENOPROTOOPT),
    };
    let room = match get::<u32>(len_at) {
        Ok(room) => room as usize,
        Err(err) => return Some(err),
    };
    let bytes = answer.to_ne_bytes();
    let take = room.min(4);
    Some(write_user(value, &bytes[..take]).and_then(|()| put(len_at, take as u32)).map_or_else(|err| err, |()| 0))
}

/// Отправить запрос: ответы ложатся в очередь до возврата. Адрес, если есть,
/// должен быть ядром (`nl_pid` 0): других сокетов netlink, кому слать, у нас
/// не бывает.
fn send(socket: &NetlinkSocket, data: &[u8], to: Option<(u32, u32)>) -> i64 {
    if let Some((pid, _)) = to {
        if pid != 0 {
            return -ECONNREFUSED;
        }
    }
    match socket.request(data) {
        Ok(()) => data.len() as i64,
        Err(err) => err,
    }
}

pub fn sendto(fd: usize, buf: usize, len: usize, _flags: usize, addr: usize, addr_len: usize) -> Option<i64> {
    let (object, _) = netlink_of(fd)?;
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
    Some(send(netlink(&object), &data, to))
}

/// `struct msghdr` x86-64: имя @0, длина имени @8, `iov` @16, их число @24,
/// управляющие данные @32, их длина @40, флаги @48.
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

pub fn sendmsg(fd: usize, msg: usize, _flags: usize) -> Option<i64> {
    let (object, _) = netlink_of(fd)?;
    let message = match message(msg) {
        Ok(message) => message,
        Err(err) => return Some(err),
    };
    let to = if message.name != 0 {
        match address_of(message.name, message.name_len as usize) {
            Ok(found) => Some(found),
            Err(err) => return Some(err),
        }
    } else {
        None
    };
    let mut data = Vec::new();
    for (addr, len) in message.parts {
        match read_user(addr, len) {
            Ok(part) => data.extend_from_slice(&part),
            Err(err) => return Some(err),
        }
    }
    Some(send(netlink(&object), &data, to))
}

/// Дождаться датаграммы (ждущий сокет без `MSG_DONTWAIT` ждёт, как Linux,
/// хоть вечно; неждущий — `EAGAIN`).
fn receive(socket: &NetlinkSocket, nonblock: bool, flags: usize) -> Result<Vec<u8>, i64> {
    wait_io(nonblock || flags & MSG_DONTWAIT != 0, || None, || socket.take(flags & MSG_PEEK != 0))
}

/// Что вернуть из `recv*`: с `MSG_TRUNC` во флагах — длину датаграммы, даже
/// если она не влезла (так старая glibc узнаёт размер буфера), иначе —
/// сколько скопировано.
fn received(datagram: &[u8], copied: usize, flags: usize) -> i64 {
    if flags & MSG_TRUNC != 0 { datagram.len() as i64 } else { copied as i64 }
}

pub fn recvfrom(fd: usize, buf: usize, len: usize, flags: usize, addr: usize, len_at: usize) -> Option<i64> {
    let (object, nonblock) = netlink_of(fd)?;
    let datagram = match receive(netlink(&object), nonblock, flags) {
        Ok(datagram) => datagram,
        Err(err) => return Some(err),
    };
    let copied = datagram.len().min(len);
    let written = write_user(buf, &datagram[..copied]).and_then(|()| put_address(addr, len_at, 0, 0));
    Some(written.map_or_else(|err| err, |()| received(&datagram, copied, flags)))
}

pub fn recvmsg(fd: usize, msg: usize, flags: usize) -> Option<i64> {
    let (object, nonblock) = netlink_of(fd)?;
    let message = match message(msg) {
        Ok(message) => message,
        Err(err) => return Some(err),
    };
    let datagram = match receive(netlink(&object), nonblock, flags) {
        Ok(datagram) => datagram,
        Err(err) => return Some(err),
    };
    let mut done = 0;
    for (addr, len) in &message.parts {
        if done >= datagram.len() {
            break;
        }
        let take = (*len).min(datagram.len() - done);
        if let Err(err) = write_user(*addr, &datagram[done..done + take]) {
            return Some(err);
        }
        done += take;
    }
    let result = (|| -> Result<(), i64> {
        if message.name != 0 {
            // Отправитель — ядро: `{AF_NETLINK, 0, nl_pid 0, nl_groups 0}`;
            // ответ с другим `nl_pid` glibc выбрасывает.
            write_user(message.name, &sockaddr(0, 0)[..(message.name_len as usize).min(12)])?;
            put(msg + 8, 12u32)?;
        }
        put(msg + 40, 0usize)?;
        let truncated = if done < datagram.len() { MSG_TRUNC as i32 } else { 0 };
        put(msg + 48, truncated)
    })();
    Some(result.map_or_else(|err| err, |()| received(&datagram, done, flags)))
}

/// `shutdown`, `listen`, `accept`: у netlink нечего закрывать наполовину и
/// нечего слушать.
pub fn shutdown(fd: usize) -> Option<i64> {
    netlink_of(fd).map(|_| 0)
}

pub fn listen(fd: usize) -> Option<i64> {
    netlink_of(fd).map(|_| -EOPNOTSUPP)
}

pub fn accept(fd: usize) -> Option<i64> {
    netlink_of(fd).map(|_| -EOPNOTSUPP)
}

// ---- разбор запросов ------------------------------------------------------------

const fn align4(len: usize) -> usize {
    (len + 3) & !3
}

/// Сообщение ответа: заголовок пишется в [`Builder::finish`], когда длина
/// известна.
struct Builder {
    buf: Vec<u8>,
}

impl Builder {
    fn new(ty: u16, flags: u16, seq: u32, pid: u32) -> Self {
        let mut buf = Vec::with_capacity(128);
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(&ty.to_ne_bytes());
        buf.extend_from_slice(&flags.to_ne_bytes());
        buf.extend_from_slice(&seq.to_ne_bytes());
        buf.extend_from_slice(&pid.to_ne_bytes());
        Self { buf }
    }
    fn bytes(&mut self, data: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(data);
        self
    }
    /// `rtattr`: длина без выравнивания (4 + данные), затем нули до 4 байт —
    /// они в длину атрибута не входят, а в длину сообщения входят.
    fn attr(&mut self, ty: u16, payload: &[u8]) -> &mut Self {
        let len = 4 + payload.len();
        self.buf.extend_from_slice(&(len as u16).to_ne_bytes());
        self.buf.extend_from_slice(&ty.to_ne_bytes());
        self.buf.extend_from_slice(payload);
        self.buf.resize(align4(self.buf.len()), 0);
        self
    }
    fn finish(&mut self) -> Vec<u8> {
        self.buf.resize(align4(self.buf.len()), 0);
        let len = self.buf.len() as u32;
        self.buf[..4].copy_from_slice(&len.to_ne_bytes());
        core::mem::take(&mut self.buf)
    }
}

/// Имя с нулём в конце — для `IFLA_IFNAME`/`IFA_LABEL` (не длиннее 16:
/// длиннее glibc считает поломкой и зовёт `abort`).
fn name_bytes(name: &str) -> Vec<u8> {
    let mut out: Vec<u8> = name.bytes().take(15).collect();
    out.push(0);
    out
}

/// `RTM_NEWLINK` об интерфейсе.
fn link_message(info: &IfInfo, flags: u16, seq: u32, pid: u32) -> Vec<u8> {
    let mut message = Builder::new(RTM_NEWLINK, flags, seq, pid);
    // `ifinfomsg`: семейство, пусто, тип ARPHRD, номер, флаги IFF, маска смены.
    message
        .bytes(&[AF_UNSPEC, 0])
        .bytes(&info.hwtype.to_ne_bytes())
        .bytes(&(info.index as i32).to_ne_bytes())
        .bytes(&info.flags.to_ne_bytes())
        .bytes(&u32::MAX.to_ne_bytes());
    let broadcast = if info.loopback { [0u8; 6] } else { [0xFF; 6] };
    let operstate = if info.loopback {
        IF_OPER_UNKNOWN
    } else if info.running() {
        IF_OPER_UP
    } else {
        IF_OPER_DOWN
    };
    message
        .attr(IFLA_IFNAME, &name_bytes(info.name))
        .attr(IFLA_ADDRESS, &info.mac)
        .attr(IFLA_BROADCAST, &broadcast)
        .attr(IFLA_MTU, &info.mtu.to_ne_bytes())
        .attr(IFLA_OPERSTATE, &[operstate]);
    message.finish()
}

/// `RTM_NEWADDR` об адресе IPv4 интерфейса (если он есть).
fn address_message(info: &IfInfo, seq: u32, pid: u32) -> Option<Vec<u8>> {
    let (address, prefix) = info.address?;
    let scope = if address.is_loopback() { RT_SCOPE_HOST } else { RT_SCOPE_UNIVERSE };
    let mut message = Builder::new(RTM_NEWADDR, NLM_F_MULTI, seq, pid);
    // `ifaddrmsg`: семейство, длина префикса, флаги, область, номер.
    message.bytes(&[AF_INET, prefix, IFA_F_PERMANENT, scope]).bytes(&info.index.to_ne_bytes());
    message.attr(IFA_ADDRESS, &address.to_bytes()).attr(IFA_LOCAL, &address.to_bytes()).attr(IFA_LABEL, &name_bytes(info.name));
    if !info.loopback {
        message.attr(IFA_BROADCAST, &info.broadcast().to_bytes());
    }
    Some(message.finish())
}

/// `RTM_NEWROUTE`: `rtmsg` (семейство, длины назначения и источника, TOS,
/// таблица, протокол, область, тип, флаги) и атрибуты.
fn route_message(seq: u32, pid: u32, dst_len: u8, protocol: u8, scope: u8, attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut message = Builder::new(RTM_NEWROUTE, NLM_F_MULTI, seq, pid);
    message.bytes(&[AF_INET, dst_len, 0, 0, RT_TABLE_MAIN, protocol, scope, RTN_UNICAST]).bytes(&0u32.to_ne_bytes());
    message.attr(RTA_TABLE, &u32::from(RT_TABLE_MAIN).to_ne_bytes());
    for (ty, payload) in attrs {
        message.attr(*ty, payload);
    }
    message.finish()
}

/// Маршруты главной таблицы: путь по умолчанию через шлюз и сеть каждого
/// интерфейса с адресом (кроме петли — её путь в таблице `local`).
fn route_messages(interfaces: &[IfInfo], seq: u32, pid: u32) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for info in interfaces.iter().filter(|info| !info.loopback) {
        let Some((address, prefix)) = info.address else { continue };
        let oif = info.index.to_ne_bytes();
        if !info.gateway.is_unspecified() {
            let gateway = info.gateway.to_bytes();
            out.push(route_message(seq, pid, 0, RTPROT_BOOT, RT_SCOPE_UNIVERSE, &[(RTA_GATEWAY, &gateway), (RTA_OIF, &oif)]));
        }
        let mask = Ipv4::from_prefix(u32::from(prefix)).unwrap_or(Ipv4::UNSPECIFIED);
        let network = Ipv4(address.0 & mask.0).to_bytes();
        let source = address.to_bytes();
        out.push(route_message(
            seq,
            pid,
            prefix,
            RTPROT_KERNEL,
            RT_SCOPE_LINK,
            &[(RTA_DST, &network), (RTA_OIF, &oif), (RTA_PREFSRC, &source)],
        ));
    }
    out
}

fn done_message(seq: u32, pid: u32) -> Vec<u8> {
    let mut message = Builder::new(NLMSG_DONE, NLM_F_MULTI, seq, pid);
    message.bytes(&0i32.to_ne_bytes());
    message.finish()
}

/// `NLMSG_ERROR`: код (`-errno`, ноль — подтверждение) и копия заголовка
/// запроса.
fn error_message(seq: u32, pid: u32, code: i64, request: &[u8]) -> Vec<u8> {
    // Linux к ошибке прикладывает и тело запроса; мы — только заголовок и
    // говорим об этом флагом, как Linux с `NETLINK_CAP_ACK`.
    let flags = if code != 0 && request.len() > NLMSG_HDRLEN { NLM_F_CAPPED } else { 0 };
    let mut message = Builder::new(NLMSG_ERROR, flags, seq, pid);
    message.bytes(&(code as i32).to_ne_bytes()).bytes(&request[..NLMSG_HDRLEN]);
    message.finish()
}

/// Уложить сообщения в датаграммы не длиннее [`DATAGRAM_MAX`].
fn pack(messages: Vec<Vec<u8>>, out: &mut Vec<Vec<u8>>) {
    let mut current: Vec<u8> = Vec::new();
    for message in messages {
        if !current.is_empty() && current.len() + message.len() > DATAGRAM_MAX {
            out.push(core::mem::take(&mut current));
        }
        current.extend_from_slice(&message);
    }
    if !current.is_empty() {
        out.push(current);
    }
}

/// Ответить на запрос (одно или несколько сообщений подряд): датаграммы
/// ответа в порядке запросов.
fn handle_request(data: &[u8], pid: u32) -> Result<Vec<Vec<u8>>, i64> {
    if data.len() < NLMSG_HDRLEN {
        return Err(-e::EINVAL);
    }
    let mut out = Vec::new();
    let mut offset = 0;
    while offset + NLMSG_HDRLEN <= data.len() {
        let rest = &data[offset..];
        let len = u32::from_ne_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
        if len < NLMSG_HDRLEN || len > rest.len() {
            if offset == 0 {
                return Err(-e::EINVAL);
            }
            break;
        }
        handle_message(&rest[..len], pid, &mut out);
        // glibc шлёт 17 байт без выравнивания: шаг — до 4, но не за конец.
        offset += align4(len).min(rest.len());
    }
    Ok(out)
}

fn handle_message(request: &[u8], pid: u32, out: &mut Vec<Vec<u8>>) {
    let ty = u16::from_ne_bytes([request[4], request[5]]);
    let flags = u16::from_ne_bytes([request[6], request[7]]);
    let seq = u32::from_ne_bytes([request[8], request[9], request[10], request[11]]);
    let body = &request[NLMSG_HDRLEN..];
    // Служебные (`NOOP`, `ERROR`, `DONE`, `OVERRUN`) от программы — не запросы.
    if ty < RTM_BASE || flags & NLM_F_REQUEST == 0 {
        return;
    }
    // Тип rtnetlink: четвёрки NEW, DEL, GET, SET.
    let is_get = (ty - RTM_BASE) % 4 == 2;
    if is_get && flags & NLM_F_DUMP != 0 {
        // Семейство — первый байт тела (`rtgenmsg`, `ifinfomsg`, `ifaddrmsg`).
        let family = body.first().copied().unwrap_or(AF_UNSPEC);
        // Один снимок на ответ: номера в адресах — те же, что в интерфейсах.
        let interfaces = crate::net::interfaces();
        let messages: Vec<Vec<u8>> = match ty {
            // Интерфейсы Linux отдаёт при любом семействе запроса.
            RTM_GETLINK => interfaces.iter().map(|info| link_message(info, NLM_F_MULTI, seq, pid)).collect(),
            RTM_GETADDR if family == AF_UNSPEC || family == AF_INET => {
                interfaces.iter().filter_map(|info| address_message(info, seq, pid)).collect()
            }
            RTM_GETROUTE if family == AF_UNSPEC || family == AF_INET => route_messages(&interfaces, seq, pid),
            // Прочие списки (соседи, правила, IPv6 …) — пустые: кто читает до
            // `DONE`, тот закончит.
            _ => Vec::new(),
        };
        let mut messages = messages;
        messages.push(done_message(seq, pid));
        pack(messages, out);
        return;
    }
    if ty == RTM_GETLINK {
        // Один интерфейс — по номеру из `ifinfomsg` или по `IFLA_IFNAME`.
        let interfaces = crate::net::interfaces();
        let index = (body.len() >= 8).then(|| i32::from_ne_bytes([body[4], body[5], body[6], body[7]])).unwrap_or(0);
        let name = if body.len() > 16 { attr_name(&body[16..]) } else { None };
        let found = interfaces.iter().find(|info| {
            if index > 0 { info.index as i32 == index } else { name.as_deref() == Some(info.name.as_bytes()) }
        });
        match found {
            Some(info) => {
                out.push(link_message(info, 0, seq, pid));
                if flags & NLM_F_ACK != 0 {
                    out.push(error_message(seq, pid, 0, request));
                }
            }
            None => out.push(error_message(seq, pid, -ENODEV, request)),
        }
        return;
    }
    // Смена интерфейсов, адресов и маршрутов — не программам; остальное — не
    // поддержано (так отвечает rtnetlink Linux на незнакомое).
    let code = if !is_get && ty <= RTM_LAST_SET { -EPERM } else { -EOPNOTSUPP };
    out.push(error_message(seq, pid, code, request));
}

/// `IFLA_IFNAME` среди атрибутов — имя без нуля.
fn attr_name(mut attrs: &[u8]) -> Option<Vec<u8>> {
    while attrs.len() >= 4 {
        let len = u16::from_ne_bytes([attrs[0], attrs[1]]) as usize;
        let ty = u16::from_ne_bytes([attrs[2], attrs[3]]);
        if len < 4 || len > attrs.len() {
            return None;
        }
        if ty == IFLA_IFNAME {
            let payload = &attrs[4..len];
            let end = payload.iter().position(|byte| *byte == 0).unwrap_or(payload.len());
            return Some(payload[..end].to_vec());
        }
        attrs = &attrs[align4(len).min(attrs.len())..];
    }
    None
}
