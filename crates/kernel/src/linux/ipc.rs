//! Межпроцессное для программ Linux (фаза 71e): сокеты `AF_UNIX` (пары,
//! имена, `SCM_RIGHTS`), `eventfd`, `timerfd`, `epoll`, `memfd_create`.
//!
//! # Как устроено
//!
//! Каждый из них — [`Object`] в таблице дескрипторов: ссылка, общая у копий
//! (`dup`, `fork`, `SCM_RIGHTS`). Объект ничего не ждёт: «пока нечего» —
//! `-EAGAIN`; ждёт [`wait_io`] — вне всех замков, на поколении событий
//! ввода-вывода (`io_events`), которое объект сменяет, когда у него что-то
//! стало готово.
//!
//! Сокет `AF_UNIX` — два направления, у каждого очередь пакетов
//! ([`Channel`]). У потокового сокета граница пакета видна только там, где к
//! нему приложены дескрипторы: чтение на ней останавливается, чтобы права
//! пришли вместе со своими байтами; у пакетного (`SOCK_DGRAM`,
//! `SOCK_SEQPACKET`) пакет читается целиком и по одному.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use super::e;
use crate::mm::PageFlags;
use crate::sync::Mutex;
use crate::user::files::{Handle, Object};
use crate::user::space;

const EMSGSIZE: i64 = 90;
const ENOPROTOOPT: i64 = 92;
const EOPNOTSUPP: i64 = 95;
const EAFNOSUPPORT: i64 = 97;
const EADDRINUSE: i64 = 98;
const EISCONN: i64 = 106;
const ENOTCONN: i64 = 107;

const AF_UNIX: usize = 1;
const SOCK_STREAM: u32 = 1;
const SOCK_DGRAM: u32 = 2;
const SOCK_SEQPACKET: u32 = 5;
const SOCK_NONBLOCK: usize = 0o4000;
const SOCK_CLOEXEC: usize = 0o2_000_000;
const S_IFSOCK: u32 = 0o140_000;

/// Сколько байт ждёт в одном направлении сокета (у Linux — 208 КиБ по
/// умолчанию).
const CHANNEL_BYTES: usize = 256 * 1024;
/// Сколько дескрипторов в одном сообщении (`SCM_MAX_FD`).
const MAX_RIGHTS: usize = 253;

// ---- память программы --------------------------------------------------------

fn get<T: Copy>(addr: usize) -> Result<T, i64> {
    if !space::user_can(addr, size_of::<T>(), PageFlags::READ) {
        return Err(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше; пространство программы активно.
    Ok(unsafe { core::ptr::read_unaligned(addr as *const T) })
}

fn put<T: Copy>(addr: usize, value: T) -> Result<(), i64> {
    if !space::user_can(addr, size_of::<T>(), PageFlags::WRITE) {
        return Err(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::write_unaligned(addr as *mut T, value) };
    Ok(())
}

fn read_user(addr: usize, len: usize) -> Result<Vec<u8>, i64> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(|_| -e::ENOMEM)?;
    if len == 0 {
        return Ok(out);
    }
    if !space::user_can(addr, len, PageFlags::READ) {
        return Err(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше.
    out.extend_from_slice(unsafe { core::slice::from_raw_parts(addr as *const u8, len) });
    Ok(out)
}

fn write_user(addr: usize, data: &[u8]) -> Result<(), i64> {
    if data.is_empty() {
        return Ok(());
    }
    if !space::user_can(addr, data.len(), PageFlags::WRITE) {
        return Err(-e::EFAULT);
    }
    // SAFETY: проверено строкой выше.
    unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), addr as *mut u8, data.len()) };
    Ok(())
}

/// `struct iovec[]` — пары «адрес, длина».
fn iovecs(iov: usize, count: usize) -> Result<Vec<(usize, usize)>, i64> {
    if count > 1024 {
        return Err(-e::EINVAL);
    }
    (0..count).map(|index| Ok((get::<usize>(iov + index * 16)?, get::<usize>(iov + index * 16 + 8)?))).collect()
}

// ---- ожидание ------------------------------------------------------------------

/// Повторять `op`, пока он отвечает `-EAGAIN` (если дескриптор ждущий): между
/// попытками — сон до смены поколения событий, не дольше 100 мс и не дольше
/// `deadline`. Сигнал или снятие — `-EINTR`.
fn wait_io<T>(nonblock: bool, deadline: impl Fn() -> Option<u64>, mut op: impl FnMut() -> Result<T, i64>) -> Result<T, i64> {
    loop {
        let seen = crate::io_events::generation();
        match op() {
            Err(err) if err == -e::EAGAIN && !nonblock => {}
            other => return other,
        }
        if crate::user::kill_pending() || super::signal::pending() {
            return Err(-e::EINTR);
        }
        let mut until = crate::time::uptime_ms() + 100;
        if let Some(at) = deadline() {
            until = until.min(at.max(crate::time::uptime_ms() + 1));
        }
        crate::io_events::wait(seen, Some(until));
    }
}

fn object_of(fd: usize) -> Result<(Arc<dyn Object>, bool), i64> {
    match crate::user::with_current(|program| (program.files.object(fd), program.files.is_open(fd))) {
        Some((Some(found), _)) => Ok(found),
        Some((None, true)) => Err(-e::EINVAL),
        _ => Err(-e::EBADF),
    }
}

fn install(object: Arc<dyn Object>, nonblock: bool, cloexec: bool) -> i64 {
    match crate::user::with_current(|program| {
        let fd = program.files.install_object(object, nonblock)?;
        program.files.set_cloexec(fd, cloexec);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    }
}

/// `read`/`write`/`readv`/`writev` на объект — `None`, если за `fd` не объект
/// (тогда работает родной путь).
pub fn object_read(fd: usize, buf: usize, len: usize) -> Option<i64> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    Some(read_into(&object, nonblock, &[(buf, len)]))
}

pub fn object_write(fd: usize, buf: usize, len: usize) -> Option<i64> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    Some(write_from(&object, nonblock, &[(buf, len)]))
}

pub fn object_readv(fd: usize, iov: usize, count: usize) -> Option<i64> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    Some(match iovecs(iov, count) {
        Ok(parts) => read_into(&object, nonblock, &parts),
        Err(err) => err,
    })
}

pub fn object_writev(fd: usize, iov: usize, count: usize) -> Option<i64> {
    let (object, nonblock) = crate::user::with_current(|program| program.files.object(fd)).flatten()?;
    Some(match iovecs(iov, count) {
        Ok(parts) => write_from(&object, nonblock, &parts),
        Err(err) => err,
    })
}

fn scatter(parts: &[(usize, usize)], data: &[u8]) -> Result<(), i64> {
    let mut done = 0;
    for &(addr, len) in parts {
        if done >= data.len() {
            break;
        }
        let take = len.min(data.len() - done);
        write_user(addr, &data[done..done + take])?;
        done += take;
    }
    Ok(())
}

fn gather(parts: &[(usize, usize)]) -> Result<Vec<u8>, i64> {
    let mut data = Vec::new();
    for &(addr, len) in parts {
        data.extend_from_slice(&read_user(addr, len)?);
    }
    Ok(data)
}

fn read_into(object: &Arc<dyn Object>, nonblock: bool, parts: &[(usize, usize)]) -> i64 {
    let total: usize = parts.iter().map(|part| part.1).sum::<usize>().min(CHANNEL_BYTES);
    let mut buffer = vec![0u8; total];
    let read = wait_io(nonblock, || object.deadline(), || object.read(&mut buffer));
    match read {
        Ok(count) => match scatter(parts, &buffer[..count]) {
            Ok(()) => count as i64,
            Err(err) => err,
        },
        Err(err) => err,
    }
}

fn write_from(object: &Arc<dyn Object>, nonblock: bool, parts: &[(usize, usize)]) -> i64 {
    let data = match gather(parts) {
        Ok(data) => data,
        Err(err) => return err,
    };
    let mut done = 0;
    loop {
        let wrote = wait_io(nonblock, || None, || object.write(&data[done..]));
        match wrote {
            Ok(count) => {
                done += count;
                // Ждущий потоковый — пока не уйдёт всё, как `write` в канал.
                if done >= data.len() || nonblock || count == 0 {
                    return done as i64;
                }
            }
            Err(_) if done > 0 => return done as i64,
            Err(err) => return err,
        }
    }
}

// ---- сокеты AF_UNIX --------------------------------------------------------------

struct Packet {
    data: Vec<u8>,
    /// Сколько байт пакета уже прочитано (потоковый сокет).
    used: usize,
    rights: Vec<Handle>,
}

#[derive(Default)]
struct Queue {
    packets: VecDeque<Packet>,
    bytes: usize,
    /// Пишущих не осталось (конец файла у читающего).
    no_writer: bool,
    /// Читающих не осталось (`EPIPE` у пишущего).
    no_reader: bool,
}

/// Одно направление сокета.
struct Channel(Mutex<Queue>);

impl Default for Channel {
    fn default() -> Self {
        Self(Mutex::new(Queue::default()))
    }
}

/// Один конец соединённой пары: читает из `rx`, пишет в `tx`.
struct Endpoint {
    rx: Arc<Channel>,
    tx: Arc<Channel>,
    /// Пакетный (`SOCK_DGRAM`, `SOCK_SEQPACKET`) — границы сообщений
    /// сохраняются.
    packets: bool,
    /// Процесс по ту сторону (`SO_PEERCRED`).
    peer_pid: AtomicU32,
}

impl Endpoint {
    fn pair(packets: bool, left_pid: u32, right_pid: u32) -> (Self, Self) {
        let (one, two) = (Arc::new(Channel::default()), Arc::new(Channel::default()));
        (
            Self { rx: Arc::clone(&one), tx: Arc::clone(&two), packets, peer_pid: AtomicU32::new(right_pid) },
            Self { rx: two, tx: one, packets, peer_pid: AtomicU32::new(left_pid) },
        )
    }

    fn send(&self, data: &[u8], rights: Vec<Handle>) -> Result<usize, i64> {
        let mut queue = self.tx.0.lock();
        if queue.no_reader || queue.no_writer {
            return Err(-e::EPIPE);
        }
        if self.packets && data.len() > CHANNEL_BYTES {
            return Err(-EMSGSIZE);
        }
        let room = CHANNEL_BYTES.saturating_sub(queue.bytes);
        let take = if self.packets {
            if data.len() > room && queue.bytes > 0 {
                return Err(-e::EAGAIN);
            }
            data.len()
        } else {
            if room == 0 {
                return Err(-e::EAGAIN);
            }
            data.len().min(room)
        };
        if take == 0 && rights.is_empty() && !self.packets {
            return Ok(0);
        }
        queue.packets.try_reserve(1).map_err(|_| -e::ENOMEM)?;
        queue.bytes += take;
        queue.packets.push_back(Packet { data: data[..take].to_vec(), used: 0, rights });
        drop(queue);
        crate::io_events::notify();
        Ok(take)
    }

    /// Прочитать: `(сколько байт, дескрипторы, пакет обрезан)`.
    fn recv(&self, buf: &mut [u8], peek: bool) -> Result<(usize, Vec<Handle>, bool), i64> {
        let mut queue = self.rx.0.lock();
        if queue.packets.is_empty() {
            return if queue.no_writer { Ok((0, Vec::new(), false)) } else { Err(-e::EAGAIN) };
        }
        if self.packets {
            let packet = if peek {
                let front = queue.packets.front().ok_or(-e::EAGAIN)?;
                let take = front.data.len().min(buf.len());
                buf[..take].copy_from_slice(&front.data[..take]);
                return Ok((take, Vec::new(), front.data.len() > take));
            } else {
                queue.packets.pop_front().ok_or(-e::EAGAIN)?
            };
            queue.bytes -= packet.data.len();
            drop(queue);
            crate::io_events::notify();
            let take = packet.data.len().min(buf.len());
            buf[..take].copy_from_slice(&packet.data[..take]);
            return Ok((take, packet.rights, packet.data.len() > take));
        }
        let mut done = 0;
        let mut rights = Vec::new();
        let mut index = 0;
        while done < buf.len() && index < queue.packets.len() {
            // Дескрипторы едут со своими байтами: пакет с ними начинает новое
            // чтение.
            if done > 0 && !queue.packets[index].rights.is_empty() {
                break;
            }
            let packet = &mut queue.packets[index];
            if !peek {
                rights.append(&mut packet.rights);
            }
            let left = packet.data.len() - packet.used;
            let take = left.min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&packet.data[packet.used..packet.used + take]);
            done += take;
            if peek {
                index += 1;
                continue;
            }
            packet.used += take;
            if packet.used == packet.data.len() {
                queue.packets.pop_front();
            }
            queue.bytes -= take;
        }
        drop(queue);
        if !peek {
            crate::io_events::notify();
        }
        Ok((done, rights, false))
    }

    fn readiness(&self) -> (bool, bool, bool) {
        let (readable, closed) = {
            let queue = self.rx.0.lock();
            (!queue.packets.is_empty() || queue.no_writer, queue.no_writer)
        };
        let writable = {
            let queue = self.tx.0.lock();
            queue.bytes < CHANNEL_BYTES || queue.no_reader
        };
        (readable, writable, closed)
    }

    fn shutdown(&self, read: bool, write: bool) {
        if read {
            self.rx.0.lock().no_reader = true;
        }
        if write {
            self.tx.0.lock().no_writer = true;
        }
        crate::io_events::notify();
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        self.shutdown(true, true);
    }
}

/// Слушающий сокет: очередь пришедших соединений.
struct Listener {
    queue: Mutex<VecDeque<Arc<UnixSocket>>>,
}

/// Имена сокетов: и пути, и абстрактные (с нулём в начале). Ссылка слабая —
/// закрытый сокет уходит из имён сам.
static NAMES: Mutex<Vec<(Vec<u8>, Weak<Listener>)>> = Mutex::new(Vec::new());

enum State {
    Fresh,
    Listening(Arc<Listener>),
    Connected(Endpoint),
}

pub struct UnixSocket {
    kind: u32,
    state: Mutex<State>,
    name: Mutex<Option<Vec<u8>>>,
    peer_name: Mutex<Option<Vec<u8>>>,
}

impl UnixSocket {
    fn new(kind: u32, state: State) -> Self {
        Self { kind, state: Mutex::new(state), name: Mutex::new(None), peer_name: Mutex::new(None) }
    }

    fn with_endpoint<R>(&self, f: impl FnOnce(&Endpoint) -> Result<R, i64>) -> Result<R, i64> {
        match &*self.state.lock() {
            State::Connected(endpoint) => f(endpoint),
            State::Listening(_) => Err(-e::EINVAL),
            State::Fresh => Err(-ENOTCONN),
        }
    }
}

impl Object for UnixSocket {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        self.with_endpoint(|endpoint| endpoint.recv(buf, false).map(|(count, _, _)| count))
    }
    fn write(&self, data: &[u8]) -> Result<usize, i64> {
        self.with_endpoint(|endpoint| endpoint.send(data, Vec::new()))
    }
    fn readiness(&self) -> (bool, bool, bool) {
        match &*self.state.lock() {
            State::Connected(endpoint) => endpoint.readiness(),
            State::Listening(listener) => (!listener.queue.lock().is_empty(), false, false),
            State::Fresh => (false, false, true),
        }
    }
    fn mode(&self) -> u32 {
        S_IFSOCK
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn socket_of(fd: usize) -> Result<(Arc<dyn Object>, bool), i64> {
    let (object, nonblock) = match crate::user::with_current(|program| (program.files.object(fd), program.files.is_open(fd))) {
        Some((Some(found), _)) => found,
        Some((None, true)) => return Err(-e::ENOTSOCK),
        _ => return Err(-e::EBADF),
    };
    if object.as_any().downcast_ref::<UnixSocket>().is_none() {
        return Err(-e::ENOTSOCK);
    }
    Ok((object, nonblock))
}

fn unix(object: &Arc<dyn Object>) -> &UnixSocket {
    // Проверено в `socket_of`.
    object.as_any().downcast_ref::<UnixSocket>().expect("socket_of checked the type")
}

fn own_pid() -> u32 {
    crate::user::with_current(|program| program.linux_pid).unwrap_or(0)
}

fn kind_of(kind: usize) -> Result<u32, i64> {
    match (kind & 0xf) as u32 {
        kind @ (SOCK_STREAM | SOCK_DGRAM | SOCK_SEQPACKET) => Ok(kind),
        _ => Err(-EOPNOTSUPP),
    }
}

/// `socket(domain, type, protocol)`: только `AF_UNIX` (сеть — фаза 71f).
pub fn socket(domain: usize, kind: usize) -> i64 {
    if domain != AF_UNIX {
        return -EAFNOSUPPORT;
    }
    let sock = match kind_of(kind) {
        Ok(sock) => sock,
        Err(err) => return err,
    };
    install(Arc::new(UnixSocket::new(sock, State::Fresh)), kind & SOCK_NONBLOCK != 0, kind & SOCK_CLOEXEC != 0)
}

/// `socketpair(AF_UNIX, type, 0, sv)`.
pub fn socketpair(domain: usize, kind: usize, out: usize) -> i64 {
    if domain != AF_UNIX {
        return -EAFNOSUPPORT;
    }
    let sock = match kind_of(kind) {
        Ok(sock) => sock,
        Err(err) => return err,
    };
    if !space::user_can(out, 8, PageFlags::WRITE) {
        return -e::EFAULT;
    }
    let pid = own_pid();
    let (left, right) = Endpoint::pair(sock != SOCK_STREAM, pid, pid);
    let (nonblock, cloexec) = (kind & SOCK_NONBLOCK != 0, kind & SOCK_CLOEXEC != 0);
    let first = install(Arc::new(UnixSocket::new(sock, State::Connected(left))), nonblock, cloexec);
    if first < 0 {
        return first;
    }
    let second = install(Arc::new(UnixSocket::new(sock, State::Connected(right))), nonblock, cloexec);
    if second < 0 {
        crate::user::with_current(|program| program.files.close(first as usize));
        return second;
    }
    match put(out, [first as i32, second as i32]) {
        Ok(()) => 0,
        Err(err) => err,
    }
}

/// Имя из `struct sockaddr_un`: путь или абстрактное (с нулём в начале).
fn name_of(addr: usize, len: usize) -> Result<Vec<u8>, i64> {
    if !(3..=110).contains(&len) {
        return Err(-e::EINVAL);
    }
    let raw = read_user(addr, len)?;
    if usize::from(u16::from_ne_bytes([raw[0], raw[1]])) != AF_UNIX {
        return Err(-EAFNOSUPPORT);
    }
    let path = &raw[2..];
    if path[0] == 0 {
        return Ok(path.to_vec());
    }
    let end = path.iter().position(|byte| *byte == 0).unwrap_or(path.len());
    Ok(path[..end].to_vec())
}

/// Записать `sockaddr_un` с именем `name` (или пустым) в `addr`/`len`.
fn put_name(addr: usize, len_at: usize, name: Option<&[u8]>) -> Result<(), i64> {
    if addr == 0 || len_at == 0 {
        return Ok(());
    }
    let room = get::<u32>(len_at)? as usize;
    let name = name.unwrap_or(&[]);
    let mut raw = Vec::with_capacity(2 + name.len() + 1);
    raw.extend_from_slice(&(AF_UNIX as u16).to_ne_bytes());
    raw.extend_from_slice(name);
    let mut full = raw.len();
    if !name.is_empty() && name[0] != 0 {
        raw.push(0);
        full += 1;
    }
    write_user(addr, &raw[..raw.len().min(room)])?;
    put(len_at, full as u32)
}

pub fn bind(fd: usize, addr: usize, len: usize) -> i64 {
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let name = match name_of(addr, len) {
        Ok(name) => name,
        Err(err) => return err,
    };
    {
        let names = NAMES.lock();
        if names.iter().any(|(other, live)| *other == name && live.strong_count() > 0) {
            return -EADDRINUSE;
        }
    }
    *unix(&object).name.lock() = Some(name);
    0
}

pub fn listen(fd: usize) -> i64 {
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let socket = unix(&object);
    let Some(name) = socket.name.lock().clone() else {
        return -e::EINVAL;
    };
    let mut state = socket.state.lock();
    match &*state {
        State::Listening(_) => return 0,
        State::Connected(_) => return -EISCONN,
        State::Fresh => {}
    }
    let listener = Arc::new(Listener { queue: Mutex::new(VecDeque::new()) });
    {
        let mut names = NAMES.lock();
        names.retain(|(_, live)| live.strong_count() > 0);
        names.push((name, Arc::downgrade(&listener)));
    }
    *state = State::Listening(listener);
    0
}

pub fn connect(fd: usize, addr: usize, len: usize) -> i64 {
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let name = match name_of(addr, len) {
        Ok(name) => name,
        Err(err) => return err,
    };
    let listener = NAMES.lock().iter().find(|(other, _)| *other == name).and_then(|(_, live)| live.upgrade());
    let Some(listener) = listener else {
        return -e::ECONNREFUSED;
    };
    let socket = unix(&object);
    let mut state = socket.state.lock();
    if !matches!(*state, State::Fresh) {
        return -EISCONN;
    }
    let pid = own_pid();
    let (mine, theirs) = Endpoint::pair(socket.kind != SOCK_STREAM, pid, 0);
    let server = Arc::new(UnixSocket::new(socket.kind, State::Connected(theirs)));
    *server.name.lock() = Some(name.clone());
    if listener.queue.lock().try_reserve(1).is_err() {
        return -e::ENOMEM;
    }
    listener.queue.lock().push_back(server);
    *state = State::Connected(mine);
    drop(state);
    *socket.peer_name.lock() = Some(name);
    crate::io_events::notify();
    0
}

pub fn accept(fd: usize, addr: usize, len: usize, flags: usize) -> i64 {
    let (object, nonblock) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let socket = unix(&object);
    let listener = match &*socket.state.lock() {
        State::Listening(listener) => Arc::clone(listener),
        _ => return -e::EINVAL,
    };
    let got = wait_io(nonblock, || None, || listener.queue.lock().pop_front().ok_or(-e::EAGAIN));
    let server = match got {
        Ok(server) => server,
        Err(err) => return err,
    };
    if let Err(err) = put_name(addr, len, None) {
        return err;
    }
    install(server, flags & SOCK_NONBLOCK != 0, flags & SOCK_CLOEXEC != 0)
}

pub fn shutdown(fd: usize, how: usize) -> i64 {
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    match unix(&object).with_endpoint(|endpoint| {
        endpoint.shutdown(how != 1, how != 0);
        Ok(0)
    }) {
        Ok(answer) | Err(answer) => answer,
    }
}

pub fn sockname(fd: usize, addr: usize, len: usize, peer: bool) -> i64 {
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let socket = unix(&object);
    if peer && !matches!(*socket.state.lock(), State::Connected(_)) {
        return -ENOTCONN;
    }
    let name = if peer { socket.peer_name.lock().clone() } else { socket.name.lock().clone() };
    match put_name(addr, len, name.as_deref()) {
        Ok(()) => 0,
        Err(err) => err,
    }
}

pub fn getsockopt(fd: usize, level: usize, option: usize, value: usize, len_at: usize) -> i64 {
    const SOL_SOCKET: usize = 1;
    const SO_TYPE: usize = 3;
    const SO_ERROR: usize = 4;
    const SO_SNDBUF: usize = 7;
    const SO_RCVBUF: usize = 8;
    const SO_PASSCRED: usize = 16;
    const SO_PEERCRED: usize = 17;
    const SO_DOMAIN: usize = 39;
    let (object, _) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    if level != SOL_SOCKET {
        return -ENOPROTOOPT;
    }
    let socket = unix(&object);
    let answer: Vec<u8> = match option {
        SO_TYPE => socket.kind.to_ne_bytes().to_vec(),
        SO_ERROR | SO_PASSCRED => 0u32.to_ne_bytes().to_vec(),
        SO_SNDBUF | SO_RCVBUF => (CHANNEL_BYTES as u32).to_ne_bytes().to_vec(),
        SO_DOMAIN => (AF_UNIX as u32).to_ne_bytes().to_vec(),
        SO_PEERCRED => {
            let pid = match &*socket.state.lock() {
                State::Connected(endpoint) => endpoint.peer_pid.load(Ordering::Relaxed),
                _ => 0,
            };
            let cred = crate::user::credentials();
            let mut raw = pid.to_ne_bytes().to_vec();
            raw.extend_from_slice(&cred.uid.to_ne_bytes());
            raw.extend_from_slice(&cred.gid.to_ne_bytes());
            raw
        }
        _ => return -ENOPROTOOPT,
    };
    let room = match get::<u32>(len_at) {
        Ok(room) => room as usize,
        Err(err) => return err,
    };
    let take = answer.len().min(room);
    if let Err(err) = write_user(value, &answer[..take]).and_then(|()| put(len_at, take as u32)) {
        return err;
    }
    0
}

/// `sendmsg(fd, msg, flags)` и `sendto` без адреса.
pub fn sendmsg(fd: usize, msg: usize, flags: usize) -> i64 {
    const MSG_DONTWAIT: usize = 0x40;
    let (object, nonblock) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let parsed = (|| -> Result<(Vec<u8>, Vec<Handle>), i64> {
        let iov = get::<usize>(msg + 16)?;
        let count = get::<usize>(msg + 24)?;
        let control = get::<usize>(msg + 32)?;
        let control_len = get::<usize>(msg + 40)?;
        let data = gather(&iovecs(iov, count)?)?;
        let rights = rights_from(control, control_len)?;
        Ok((data, rights))
    })();
    let (data, rights) = match parsed {
        Ok(parsed) => parsed,
        Err(err) => return err,
    };
    send(&object, nonblock || flags & MSG_DONTWAIT != 0, &data, rights)
}

pub fn sendto(fd: usize, buf: usize, len: usize, flags: usize) -> i64 {
    const MSG_DONTWAIT: usize = 0x40;
    let (object, nonblock) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let data = match read_user(buf, len) {
        Ok(data) => data,
        Err(err) => return err,
    };
    send(&object, nonblock || flags & MSG_DONTWAIT != 0, &data, Vec::new())
}

fn send(object: &Arc<dyn Object>, nonblock: bool, data: &[u8], rights: Vec<Handle>) -> i64 {
    let socket = unix(object);
    let mut rights = Some(rights);
    let mut done = 0;
    loop {
        let sent = wait_io(nonblock, || None, || {
            socket.with_endpoint(|endpoint| {
                let attached = rights.clone().unwrap_or_default();
                let count = endpoint.send(&data[done..], attached)?;
                rights = None;
                Ok(count)
            })
        });
        match sent {
            Ok(count) => {
                done += count;
                if done >= data.len() || nonblock || socket.kind != SOCK_STREAM || count == 0 {
                    return done as i64;
                }
            }
            Err(_) if done > 0 => return done as i64,
            Err(err) => return err,
        }
    }
}

/// Дескрипторы `SCM_RIGHTS` из управляющих данных — ссылками.
fn rights_from(control: usize, len: usize) -> Result<Vec<Handle>, i64> {
    const SOL_SOCKET: i32 = 1;
    const SCM_RIGHTS: i32 = 1;
    let mut out = Vec::new();
    let mut at = 0;
    while control != 0 && at + 16 <= len {
        let cmsg_len = get::<usize>(control + at)?;
        let level = get::<i32>(control + at + 8)?;
        let kind = get::<i32>(control + at + 12)?;
        if cmsg_len < 16 || at + cmsg_len > len {
            return Err(-e::EINVAL);
        }
        if level == SOL_SOCKET && kind == SCM_RIGHTS {
            let count = (cmsg_len - 16) / 4;
            if out.len() + count > MAX_RIGHTS {
                return Err(-e::EINVAL);
            }
            for index in 0..count {
                let fd = get::<i32>(control + at + 16 + index * 4)?;
                let handle = crate::user::with_current(|program| program.files.handle(fd as usize));
                match handle {
                    Some(Ok(handle)) => out.push(handle),
                    _ => return Err(-e::EBADF),
                }
            }
        }
        at += cmsg_len.next_multiple_of(8);
    }
    Ok(out)
}

/// `recvmsg(fd, msg, flags)`.
pub fn recvmsg(fd: usize, msg: usize, flags: usize) -> i64 {
    const MSG_PEEK: usize = 2;
    const MSG_CTRUNC: i32 = 8;
    const MSG_TRUNC: i32 = 0x20;
    const MSG_DONTWAIT: usize = 0x40;
    const MSG_CMSG_CLOEXEC: usize = 0x4000_0000;
    let (object, nonblock) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let fields = (|| -> Result<_, i64> {
        Ok((get::<usize>(msg + 16)?, get::<usize>(msg + 24)?, get::<usize>(msg + 32)?, get::<usize>(msg + 40)?))
    })();
    let (iov, count, control, control_len) = match fields {
        Ok(fields) => fields,
        Err(err) => return err,
    };
    let parts = match iovecs(iov, count) {
        Ok(parts) => parts,
        Err(err) => return err,
    };
    let total: usize = parts.iter().map(|part| part.1).sum::<usize>().min(CHANNEL_BYTES);
    let mut buffer = vec![0u8; total];
    let socket = unix(&object);
    let got = wait_io(nonblock || flags & MSG_DONTWAIT != 0, || None, || {
        socket.with_endpoint(|endpoint| endpoint.recv(&mut buffer, flags & MSG_PEEK != 0))
    });
    let (read, rights, truncated) = match got {
        Ok(got) => got,
        Err(err) => return err,
    };
    if let Err(err) = scatter(&parts, &buffer[..read]) {
        return err;
    }
    let mut msg_flags = if truncated { MSG_TRUNC } else { 0 };
    // Дескрипторы — в таблицу и в управляющие данные, сколько поместится.
    let mut fds: Vec<i32> = Vec::new();
    let room = if control == 0 { 0 } else { control_len.saturating_sub(16) / 4 };
    for (index, handle) in rights.into_iter().enumerate() {
        if index >= room {
            msg_flags |= MSG_CTRUNC;
            continue;
        }
        let installed = crate::user::with_current(|program| {
            let fd = program.files.install_handle(handle)?;
            program.files.set_cloexec(fd, flags & MSG_CMSG_CLOEXEC != 0);
            Ok::<usize, crate::user::files::FileError>(fd)
        });
        match installed {
            Some(Ok(fd)) => fds.push(fd as i32),
            _ => msg_flags |= MSG_CTRUNC,
        }
    }
    let written = (|| -> Result<usize, i64> {
        if fds.is_empty() {
            return Ok(0);
        }
        let cmsg_len = 16 + fds.len() * 4;
        put(control, cmsg_len)?;
        put(control + 8, 1i32)?;
        put(control + 12, 1i32)?;
        for (index, fd) in fds.iter().enumerate() {
            put(control + 16 + index * 4, *fd)?;
        }
        Ok(cmsg_len.next_multiple_of(8).min(control_len))
    })();
    let written = match written {
        Ok(written) => written,
        Err(err) => return err,
    };
    let result = (|| -> Result<(), i64> {
        // Имени у отправителя нет: `msg_namelen` = 0.
        if get::<usize>(msg)? != 0 {
            put(msg + 8, 0u32)?;
        }
        put(msg + 40, written)?;
        put(msg + 48, msg_flags)
    })();
    match result {
        Ok(()) => read as i64,
        Err(err) => err,
    }
}

pub fn recvfrom(fd: usize, buf: usize, len: usize, flags: usize, addr: usize, len_at: usize) -> i64 {
    const MSG_PEEK: usize = 2;
    const MSG_DONTWAIT: usize = 0x40;
    let (object, nonblock) = match socket_of(fd) {
        Ok(found) => found,
        Err(err) => return err,
    };
    let mut buffer = vec![0u8; len.min(CHANNEL_BYTES)];
    let socket = unix(&object);
    let got = wait_io(nonblock || flags & MSG_DONTWAIT != 0, || None, || {
        socket.with_endpoint(|endpoint| endpoint.recv(&mut buffer, flags & MSG_PEEK != 0))
    });
    match got {
        Ok((read, _, _)) => match write_user(buf, &buffer[..read]).and_then(|()| put_name(addr, len_at, None)) {
            Ok(()) => read as i64,
            Err(err) => err,
        },
        Err(err) => err,
    }
}

// ---- eventfd -------------------------------------------------------------------

struct EventFd {
    count: Mutex<u64>,
    semaphore: bool,
}

impl Object for EventFd {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        if buf.len() < 8 {
            return Err(-e::EINVAL);
        }
        let mut count = self.count.lock();
        if *count == 0 {
            return Err(-e::EAGAIN);
        }
        let value = if self.semaphore { 1 } else { *count };
        *count -= value;
        drop(count);
        buf[..8].copy_from_slice(&value.to_ne_bytes());
        crate::io_events::notify();
        Ok(8)
    }
    fn write(&self, data: &[u8]) -> Result<usize, i64> {
        if data.len() < 8 {
            return Err(-e::EINVAL);
        }
        let value = u64::from_ne_bytes(data[..8].try_into().unwrap_or([0; 8]));
        if value == u64::MAX {
            return Err(-e::EINVAL);
        }
        let mut count = self.count.lock();
        if u64::MAX - 1 - *count < value {
            return Err(-e::EAGAIN);
        }
        *count += value;
        drop(count);
        crate::io_events::notify();
        Ok(8)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        let count = *self.count.lock();
        (count > 0, count < u64::MAX - 1, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `eventfd2(initval, flags)`.
pub fn eventfd(initial: usize, flags: usize) -> i64 {
    const EFD_SEMAPHORE: usize = 1;
    const EFD_NONBLOCK: usize = 0o4000;
    const EFD_CLOEXEC: usize = 0o2_000_000;
    let object = Arc::new(EventFd { count: Mutex::new(initial as u32 as u64), semaphore: flags & EFD_SEMAPHORE != 0 });
    install(object, flags & EFD_NONBLOCK != 0, flags & EFD_CLOEXEC != 0)
}

// ---- timerfd -------------------------------------------------------------------

const CLOCK_REALTIME: usize = 0;

/// Часы таймера в наносекундах.
fn clock_ns(clock: usize) -> u64 {
    if clock == CLOCK_REALTIME {
        crate::time::now_unix().unwrap_or(0) * 1_000_000_000 + crate::time::uptime_ns() % 1_000_000_000
    } else {
        crate::time::uptime_ns()
    }
}

#[derive(Default)]
struct Timer {
    /// Следующее срабатывание по часам таймера, нс; ноль — не взведён.
    next: u64,
    interval: u64,
    /// Срабатывания, ещё не прочитанные.
    expired: u64,
}

struct TimerFd {
    clock: usize,
    timer: Mutex<Timer>,
}

impl TimerFd {
    /// Учесть срабатывания, наступившие к этому мигу.
    fn update(&self, timer: &mut Timer) {
        if timer.next == 0 {
            return;
        }
        let now = clock_ns(self.clock);
        if now < timer.next {
            return;
        }
        if timer.interval == 0 {
            timer.expired += 1;
            timer.next = 0;
        } else {
            let count = 1 + (now - timer.next) / timer.interval;
            timer.expired += count;
            timer.next += count * timer.interval;
        }
    }

    /// Сколько осталось до срабатывания, нс; `None` — не взведён.
    fn remaining(&self, timer: &Timer) -> Option<u64> {
        (timer.next != 0).then(|| timer.next.saturating_sub(clock_ns(self.clock)))
    }
}

impl Object for TimerFd {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        if buf.len() < 8 {
            return Err(-e::EINVAL);
        }
        let mut timer = self.timer.lock();
        self.update(&mut timer);
        if timer.expired == 0 {
            return Err(-e::EAGAIN);
        }
        buf[..8].copy_from_slice(&timer.expired.to_ne_bytes());
        timer.expired = 0;
        Ok(8)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        let mut timer = self.timer.lock();
        self.update(&mut timer);
        (timer.expired > 0, false, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn deadline(&self) -> Option<u64> {
        let timer = self.timer.lock();
        self.remaining(&timer).map(|left| crate::time::uptime_ms() + left.div_ceil(1_000_000))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// `timerfd_create(clock, flags)`.
pub fn timerfd_create(clock: usize, flags: usize) -> i64 {
    const TFD_NONBLOCK: usize = 0o4000;
    const TFD_CLOEXEC: usize = 0o2_000_000;
    if clock > 7 {
        return -e::EINVAL;
    }
    let object = Arc::new(TimerFd { clock, timer: Mutex::new(Timer::default()) });
    install(object, flags & TFD_NONBLOCK != 0, flags & TFD_CLOEXEC != 0)
}

fn timespec_ns(addr: usize) -> Result<u64, i64> {
    let [seconds, nanos] = get::<[u64; 2]>(addr)?;
    if nanos >= 1_000_000_000 {
        return Err(-e::EINVAL);
    }
    Ok(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
}

fn put_timespec(addr: usize, ns: u64) -> Result<(), i64> {
    put(addr, [ns / 1_000_000_000, ns % 1_000_000_000])
}

fn timer_of(fd: usize) -> Result<Arc<dyn Object>, i64> {
    let (object, _) = object_of(fd)?;
    if object.as_any().downcast_ref::<TimerFd>().is_none() {
        return Err(-e::EINVAL);
    }
    Ok(object)
}

fn put_itimerspec(object: &TimerFd, timer: &Timer, out: usize) -> Result<(), i64> {
    put_timespec(out, timer.interval)?;
    put_timespec(out + 16, object.remaining(timer).unwrap_or(0))
}

/// `timerfd_settime(fd, flags, new, old)`.
pub fn timerfd_settime(fd: usize, flags: usize, new: usize, old: usize) -> i64 {
    const TFD_TIMER_ABSTIME: usize = 1;
    let object = match timer_of(fd) {
        Ok(object) => object,
        Err(err) => return err,
    };
    let timer_fd = object.as_any().downcast_ref::<TimerFd>().expect("timer_of checked the type");
    let parsed = (|| -> Result<(u64, u64), i64> { Ok((timespec_ns(new)?, timespec_ns(new + 16)?)) })();
    let (interval, value) = match parsed {
        Ok(parsed) => parsed,
        Err(err) => return err,
    };
    let mut timer = timer_fd.timer.lock();
    if old != 0 {
        timer_fd.update(&mut timer);
        if let Err(err) = put_itimerspec(timer_fd, &timer, old) {
            return err;
        }
    }
    timer.expired = 0;
    timer.interval = interval;
    timer.next = if value == 0 {
        0
    } else if flags & TFD_TIMER_ABSTIME != 0 {
        value.max(1)
    } else {
        clock_ns(timer_fd.clock) + value
    };
    drop(timer);
    crate::io_events::notify();
    0
}

/// `timerfd_gettime(fd, out)`.
pub fn timerfd_gettime(fd: usize, out: usize) -> i64 {
    let object = match timer_of(fd) {
        Ok(object) => object,
        Err(err) => return err,
    };
    let timer_fd = object.as_any().downcast_ref::<TimerFd>().expect("timer_of checked the type");
    let mut timer = timer_fd.timer.lock();
    timer_fd.update(&mut timer);
    match put_itimerspec(timer_fd, &timer, out) {
        Ok(()) => 0,
        Err(err) => err,
    }
}

// ---- epoll ---------------------------------------------------------------------

const EPOLLIN: u32 = 0x1;
const EPOLLOUT: u32 = 0x4;
const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLRDHUP: u32 = 0x2000;
const EPOLLONESHOT: u32 = 1 << 30;
const EPOLLET: u32 = 1 << 31;

struct Interest {
    fd: usize,
    events: u32,
    data: u64,
    /// `EPOLLONESHOT` сработал — молчит до `EPOLL_CTL_MOD`.
    disabled: bool,
    /// Что было готово при прошлом опросе (`EPOLLET`: сообщается только
    /// то, что стало готово с тех пор).
    last: u32,
}

/// Набор `epoll`. Дескрипторы — номерами в таблице процесса, а не открытыми
/// файлами, как у Linux: закрытый номер просто перестаёт отвечать.
struct Epoll {
    interests: Mutex<Vec<Interest>>,
}

impl Object for Epoll {
    fn read(&self, _buf: &mut [u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-e::EINVAL)
    }
    /// Вложенный `epoll` не опрашивается: готовность чужих дескрипторов
    /// спрашивается под замком программы, а здесь он уже взят.
    fn readiness(&self) -> (bool, bool, bool) {
        (false, false, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub fn epoll_create(flags: usize) -> i64 {
    const EPOLL_CLOEXEC: usize = 0o2_000_000;
    install(Arc::new(Epoll { interests: Mutex::new(Vec::new()) }), false, flags & EPOLL_CLOEXEC != 0)
}

fn epoll_of(fd: usize) -> Result<Arc<dyn Object>, i64> {
    let (object, _) = object_of(fd)?;
    if object.as_any().downcast_ref::<Epoll>().is_none() {
        return Err(-e::EINVAL);
    }
    Ok(object)
}

/// `epoll_ctl(epfd, op, fd, event)`; `struct epoll_event` x86-64 — 12 байт,
/// упакованная: `u32 events`, `u64 data`.
pub fn epoll_ctl(epfd: usize, op: usize, fd: usize, event: usize) -> i64 {
    const ADD: usize = 1;
    const DEL: usize = 2;
    const MOD: usize = 3;
    let object = match epoll_of(epfd) {
        Ok(object) => object,
        Err(err) => return err,
    };
    if fd == epfd {
        return -e::EINVAL;
    }
    if !crate::user::with_current(|program| program.files.is_open(fd)).unwrap_or(false) {
        return -e::EBADF;
    }
    let epoll = object.as_any().downcast_ref::<Epoll>().expect("epoll_of checked the type");
    let (events, data) = if op == DEL {
        (0, 0)
    } else {
        match (get::<u32>(event), get::<u64>(event + 4)) {
            (Ok(events), Ok(data)) => (events, data),
            (Err(err), _) | (_, Err(err)) => return err,
        }
    };
    let mut interests = epoll.interests.lock();
    let at = interests.iter().position(|interest| interest.fd == fd);
    match (op, at) {
        (ADD, Some(_)) => return -e::EEXIST,
        (ADD, None) => {
            if interests.try_reserve(1).is_err() {
                return -e::ENOMEM;
            }
            interests.push(Interest { fd, events, data, disabled: false, last: 0 });
        }
        (MOD, Some(index)) => {
            interests[index] = Interest { fd, events, data, disabled: false, last: 0 };
        }
        (DEL, Some(index)) => {
            interests.remove(index);
        }
        (MOD | DEL, None) => return -e::ENOENT,
        _ => return -e::EINVAL,
    }
    drop(interests);
    crate::io_events::notify();
    0
}

/// `epoll_wait(epfd, events, max, timeout_ms)` и `epoll_pwait` (маска сигналов
/// не меняется: доставка и так только на выходе из вызова).
pub fn epoll_wait(epfd: usize, out: usize, max: usize, timeout: i64) -> i64 {
    let object = match epoll_of(epfd) {
        Ok(object) => object,
        Err(err) => return err,
    };
    if max == 0 || max > 4096 || !space::user_can(out, max * 12, PageFlags::WRITE) {
        return -e::EINVAL;
    }
    let epoll = object.as_any().downcast_ref::<Epoll>().expect("epoll_of checked the type");
    let deadline = (timeout >= 0).then(|| crate::time::uptime_ms() + timeout as u64);
    loop {
        let seen = crate::io_events::generation();
        let fds: Vec<(usize, u32)> = epoll.interests.lock().iter().map(|interest| (interest.fd, interest.events)).collect();
        // Готовность — без замка набора: `readiness_of` берёт замок
        // программы, а объекты — свои.
        let mut ready: Vec<(usize, u32)> = Vec::new();
        let mut soonest: Option<u64> = None;
        for (fd, events) in fds {
            let mut wanted = 0;
            if events & EPOLLIN != 0 {
                wanted |= user_abi::POLL_IN;
            }
            if events & EPOLLOUT != 0 {
                wanted |= user_abi::POLL_OUT;
            }
            let got = crate::user::syscall::readiness_of(fd, wanted);
            if got & user_abi::POLL_BAD != 0 {
                continue;
            }
            let mut bits = 0;
            if got & user_abi::POLL_IN != 0 {
                bits |= EPOLLIN;
            }
            if got & user_abi::POLL_OUT != 0 {
                bits |= EPOLLOUT;
            }
            if got & user_abi::POLL_HUP != 0 {
                bits |= EPOLLHUP | (events & EPOLLRDHUP);
            }
            ready.push((fd, bits));
            if let Some(at) = crate::user::with_current(|program| program.files.deadline(fd)).flatten() {
                soonest = Some(soonest.map_or(at, |now: u64| now.min(at)));
            }
        }
        let mut found: Vec<(u32, u64)> = Vec::new();
        {
            let mut interests = epoll.interests.lock();
            for (fd, bits) in ready {
                let Some(interest) = interests.iter_mut().find(|interest| interest.fd == fd) else {
                    continue;
                };
                let mut report = bits & (interest.events | EPOLLHUP | EPOLLERR);
                if interest.events & EPOLLET != 0 {
                    let fresh = report & !interest.last;
                    interest.last = report;
                    report = fresh;
                }
                if report == 0 || interest.disabled || found.len() >= max {
                    continue;
                }
                if interest.events & EPOLLONESHOT != 0 {
                    interest.disabled = true;
                }
                found.push((report, interest.data));
            }
        }
        if !found.is_empty() {
            for (index, (events, data)) in found.iter().enumerate() {
                let at = out + index * 12;
                if put(at, *events).and_then(|()| put(at + 4, *data)).is_err() {
                    return -e::EFAULT;
                }
            }
            return found.len() as i64;
        }
        let now = crate::time::uptime_ms();
        if deadline.is_some_and(|deadline| now >= deadline) {
            return 0;
        }
        if crate::user::kill_pending() || super::signal::pending() {
            return -e::EINTR;
        }
        let mut until = now + 100;
        for limit in [deadline, soonest].into_iter().flatten() {
            until = until.min(limit.max(now + 1));
        }
        crate::io_events::wait(seen, Some(until));
    }
}

// ---- memfd ---------------------------------------------------------------------

/// `memfd_create(name, flags)`: файл в памяти — разделяемая память, которую
/// можно отобразить в нескольких процессах.
pub fn memfd_create(name: usize, flags: usize) -> i64 {
    const MFD_CLOEXEC: usize = 1;
    if super::user_string(name).is_err() {
        return -e::EFAULT;
    }
    let node = Arc::new(crate::user::shm::ShmNode(crate::user::shm::SharedMem::new(0)));
    match crate::user::with_current(|program| {
        let fd = program.files.install_node(node, true)?;
        program.files.set_cloexec(fd, flags & MFD_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    }
}

/// Открыть `/dev/shm/имя` (`shm_open`); `None` — путь не оттуда.
pub fn open_shm(linux: &str, flags: usize) -> Option<i64> {
    const O_CREAT: usize = 0o100;
    const O_EXCL: usize = 0o200;
    const O_TRUNC: usize = 0o1000;
    const O_CLOEXEC: usize = 0o2_000_000;
    let name = linux.strip_prefix("/dev/shm/")?;
    if name.is_empty() || name.contains('/') {
        return Some(-e::EINVAL);
    }
    if flags & O_CREAT != 0 && flags & O_EXCL != 0 && crate::user::shm::named(name, false).is_some() {
        return Some(-e::EEXIST);
    }
    let Some(mem) = crate::user::shm::named(name, flags & O_CREAT != 0) else {
        return Some(-e::ENOENT);
    };
    let node = crate::user::shm::ShmNode(mem);
    if flags & O_TRUNC != 0 {
        let _ = crate::vfs::Node::truncate(&node, 0);
    }
    Some(match crate::user::with_current(|program| {
        let fd = program.files.install_node(Arc::new(node), true)?;
        program.files.set_cloexec(fd, flags & O_CLOEXEC != 0);
        Ok(fd)
    }) {
        Some(Ok(fd)) => fd as i64,
        Some(Err(err)) => super::file_errno(err),
        None => -e::ESRCH,
    })
}

/// `unlink` в `/dev/shm`; `None` — путь не оттуда.
pub fn unlink_shm(linux: &str) -> Option<i64> {
    let name = linux.strip_prefix("/dev/shm/")?;
    Some(if crate::user::shm::unlink(name) { 0 } else { -e::ENOENT })
}

/// Имена в `/dev/shm` (для `stat` — существует ли).
#[must_use]
pub fn shm_exists(linux: &str) -> bool {
    linux.strip_prefix("/dev/shm/").is_some_and(|name| crate::user::shm::names().iter().any(|other| other == name))
}
