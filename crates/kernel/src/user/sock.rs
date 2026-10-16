//! Сокет-дескриптор (фаза 65a, `user_abi::SYS_SOCKFD`): сетевое соединение в
//! таблице файлов программы.
//!
//! Под ним — тот же стек, что у старых сетевых вызовов: сокет UDP или поток
//! TCP по номеру. Отличия два, и ради них дескриптор и заведён. Первое —
//! **ожидание**: `read`, `write`, соединение и приём ждут, как в Unix, пока
//! программа не попросила [`user_abi::SOCK_NONBLOCK`]; ждут по общему счётчику
//! событий ([`crate::io_events`]), который сетевая задача сменяет после каждого
//! прохода, — без опроса по таймеру. Второе — **жизнь по ссылке**: сокет
//! закрывается в сети, когда закрыт последний дескриптор на него (`close`,
//! `dup`, выход программы, снятие), а не по номеру задачи; так сокет, открытый
//! потоком, больше не остаётся висеть после потока.
//!
//! Ждут все функции здесь **без** замка программы: дескриптор берётся из
//! таблицы под замком, `Arc` клонируется, и замок отпускается до ожидания —
//! иначе спящий остановил бы каждого, кто заглянет в таблицу (см. `files.rs`).

use core::sync::atomic::{AtomicBool, Ordering};

use crate::mm::PageFlags;
use crate::net::ipv4::Ipv4;
use crate::net::{self, NetError, stream};
use crate::sched::TaskId;
use crate::{io_events, time};
use user_abi::{
    ERR_AGAIN, ERR_BAD_ADDRESS, ERR_CONN_REFUSED, ERR_CONN_RESET, ERR_IN_PROGRESS, ERR_UNSUPPORTED,
    SockInfo,
};

use super::space;

/// Срез ожидания: даже без события сокет просыпается так часто, чтобы
/// заметить просьбу снять программу. Событие будит раньше.
const SLICE_MS: u64 = 250;

/// Сокет-дескриптор.
pub struct Sock {
    /// Поток TCP или сокет UDP — и номер в своей таблице стека.
    tcp: bool,
    index: usize,
    /// От чьего имени сокет открыт в стеке: стек сверяет хозяина на каждом
    /// вызове. Помнится здесь, а не берётся у текущей задачи: дескриптором
    /// пользуются и потоки программы, и наследник после `dup`.
    owner: TaskId,
    nonblock: AtomicBool,
}

impl Drop for Sock {
    fn drop(&mut self) {
        // Последний дескриптор закрыт. TCP — вежливо, с FIN: соединение доживёт
        // в стеке, пока собеседник не ответит.
        let _ = if self.tcp {
            net::stream_close(self.owner, self.index)
        } else {
            net::socket_close(self.owner, self.index)
        };
    }
}

impl Sock {
    /// Открыть сокет: `tcp` — поток, иначе датаграммы.
    pub fn open(tcp: bool, nonblock: bool) -> Result<Self, i64> {
        let owner = crate::sched::current();
        let index = if tcp { net::stream_open(owner) } else { net::socket_open(owner) }
            .map_err(super::syscall::net_errno)?;
        Ok(Self { tcp, index, owner, nonblock: AtomicBool::new(nonblock) })
    }

    pub fn set_nonblock(&self, on: bool) {
        self.nonblock.store(on, Ordering::Relaxed);
    }

    fn nonblock(&self) -> bool {
        self.nonblock.load(Ordering::Relaxed)
    }

    /// `(есть что читать, можно писать, связь кончилась)` — для `SYS_POLL`.
    pub fn readiness(&self) -> (bool, bool, bool) {
        if self.tcp {
            net::stream_poll(self.owner, self.index).unwrap_or((true, true, true))
        } else {
            match net::socket_info(self.owner, self.index) {
                Ok((_, _, _, pending)) => (pending, true, false),
                Err(_) => (true, true, true),
            }
        }
    }

    /// Ждать, пока `attempt` не скажет «готово» (`Some`).
    ///
    /// Поколение событий запоминается **до** попытки: событие, пришедшее между
    /// неудачной попыткой и засыпанием, сменило его, и сон не начнётся.
    fn wait<T>(&self, mut attempt: impl FnMut() -> Option<Result<T, i64>>) -> Result<T, i64> {
        loop {
            let seen = io_events::generation();
            if let Some(done) = attempt() {
                return done;
            }
            if self.nonblock() {
                return Err(ERR_AGAIN);
            }
            // Снятие происходит на возврате в третье кольцо; ожидание без
            // срока туда не вернулось бы никогда.
            if super::kill_pending() {
                return Err(ERR_AGAIN);
            }
            io_events::wait(seen, Some(time::uptime_ms() + SLICE_MS));
        }
    }

    /// `read`: данные, ноль — конец потока, или ошибка.
    pub fn read(&self, ptr: usize, len: usize) -> Result<usize, i64> {
        self.wait(|| {
            // Буфер перепроверяется на каждой попытке: пока сокет спал, поток
            // той же программы мог отдать эту память.
            if !space::user_can(ptr, len, PageFlags::WRITE) {
                return Some(Err(ERR_BAD_ADDRESS));
            }
            // SAFETY: диапазон только что проверен по таблицам программы на
            // запись; до конца попытки ядро его не отдаёт.
            let buffer = unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len) };
            if self.tcp {
                match net::stream_recv(self.owner, self.index, buffer) {
                    Ok(0) => match net::stream_info(self.owner, self.index) {
                        // Пусто, но собеседник закрылся — это конец, ноль.
                        Ok((_, _, _, _, true, false)) => Some(Ok(0)),
                        Ok((_, _, _, _, _, true)) => Some(Err(ERR_CONN_RESET)),
                        Ok(_) => None,
                        Err(err) => Some(Err(super::syscall::net_errno(err))),
                    },
                    Ok(read) => Some(Ok(read)),
                    Err(NetError::Stream(stream::StreamError::Reset)) => Some(Err(ERR_CONN_RESET)),
                    Err(err) => Some(Err(super::syscall::net_errno(err))),
                }
            } else {
                match net::socket_recv(self.owner, self.index) {
                    // Датаграмма длиннее буфера обрезается, как у `recv` в Unix.
                    Ok(Some(datagram)) => {
                        let take = datagram.data.len().min(buffer.len());
                        buffer[..take].copy_from_slice(&datagram.data[..take]);
                        Some(Ok(take))
                    }
                    Ok(None) => None,
                    Err(err) => Some(Err(super::syscall::net_errno(err))),
                }
            }
        })
    }

    /// `write`: блокирующий отдаёт всё, неблокирующий — сколько влезло.
    pub fn write(&self, ptr: usize, len: usize) -> Result<usize, i64> {
        let mut sent = 0usize;
        self.wait(|| {
            if !space::user_can(ptr, len, PageFlags::READ) {
                return Some(Err(ERR_BAD_ADDRESS));
            }
            // SAFETY: диапазон только что проверен на чтение.
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len) };
            if !self.tcp {
                return match net::socket_send(self.owner, self.index, bytes) {
                    Ok(taken) => Some(Ok(taken)),
                    // Адрес получателя выясняется ARP — подождать.
                    Err(NetError::Pending) => None,
                    Err(err) => Some(Err(super::syscall::net_errno(err))),
                };
            }
            loop {
                match net::stream_send(self.owner, self.index, &bytes[sent..]) {
                    Ok(taken) => {
                        sent += taken;
                        if sent == bytes.len() {
                            return Some(Ok(sent));
                        }
                    }
                    Err(NetError::Stream(stream::StreamError::WouldBlock | stream::StreamError::NotConnected)) => {
                        // Отдано не всё; неблокирующему — сколько ушло.
                        if sent > 0 && self.nonblock() {
                            return Some(Ok(sent));
                        }
                        return None;
                    }
                    Err(NetError::Stream(stream::StreamError::Reset)) => {
                        return Some(if sent > 0 { Ok(sent) } else { Err(ERR_CONN_RESET) });
                    }
                    Err(err) => {
                        return Some(if sent > 0 { Ok(sent) } else { Err(super::syscall::net_errno(err)) });
                    }
                }
            }
        })
    }

    /// Занять порт; ответ — занятый порт.
    pub fn bind(&self, port: u16) -> Result<u16, i64> {
        if self.tcp {
            net::stream_bind(self.owner, self.index, port)
        } else {
            net::socket_bind(self.owner, self.index, port)
        }
        .map_err(super::syscall::net_errno)
    }

    /// Соединиться. TCP ждёт ответа собеседника, если сокет блокирующий.
    pub fn connect(&self, address: u32, port: u16) -> Result<(), i64> {
        let to = Ipv4(address);
        if !self.tcp {
            return net::socket_connect(self.owner, self.index, to, port)
                .map(|_| ())
                .map_err(super::syscall::net_errno);
        }
        net::stream_connect(self.owner, self.index, to, port).map_err(super::syscall::net_errno)?;
        if self.nonblock() {
            return Err(ERR_IN_PROGRESS);
        }
        self.wait(|| match net::stream_info(self.owner, self.index) {
            Ok((_, _, _, _, _, true)) => Some(Err(ERR_CONN_REFUSED)),
            Ok((_, _, _, state, _, _)) if state.is_open() => Some(Ok(())),
            // Попытка кончилась ничем (закрыли, пока ждали) — тоже отказ.
            Ok((_, _, _, stream::State::Closed, _, _)) => Some(Err(ERR_CONN_REFUSED)),
            Ok(_) => None,
            Err(err) => Some(Err(super::syscall::net_errno(err))),
        })
    }

    pub fn listen(&self) -> Result<(), i64> {
        if !self.tcp {
            return Err(ERR_UNSUPPORTED);
        }
        net::stream_listen(self.owner, self.index).map_err(super::syscall::net_errno)
    }

    /// Принять соединение: новый сокет того же хозяина.
    pub fn accept(&self) -> Result<Self, i64> {
        if !self.tcp {
            return Err(ERR_UNSUPPORTED);
        }
        let index = self.wait(|| match net::stream_accept(self.owner, self.index) {
            Ok(Some(index)) => Some(Ok(index)),
            Ok(None) => None,
            Err(err) => Some(Err(super::syscall::net_errno(err))),
        })?;
        // Принятое соединение стек отдаёт хозяину слушающего — тот же хозяин.
        Ok(Self { tcp: true, index, owner: self.owner, nonblock: AtomicBool::new(false) })
    }

    pub fn shutdown(&self) -> Result<(), i64> {
        if !self.tcp {
            return Err(ERR_UNSUPPORTED);
        }
        net::stream_shutdown(self.owner, self.index).map_err(super::syscall::net_errno)
    }

    /// Что известно о сокете.
    pub fn info(&self) -> Result<SockInfo, i64> {
        let mut info = SockInfo { tcp: u8::from(self.tcp), ..SockInfo::default() };
        if self.tcp {
            let (local, remote, port, state, peer_closed, reset) =
                net::stream_info(self.owner, self.index).map_err(super::syscall::net_errno)?;
            info.local_port = local;
            info.remote = remote.0;
            info.remote_port = port;
            info.open = u8::from(state.is_open());
            info.listening = u8::from(state == stream::State::Listen);
            info.peer_closed = u8::from(peer_closed);
            info.reset = u8::from(reset);
        } else {
            let (local, peer, from, _) =
                net::socket_info(self.owner, self.index).map_err(super::syscall::net_errno)?;
            info.local_port = local;
            if let Some((address, port)) = peer {
                info.remote = address.0;
                info.remote_port = port;
                info.open = 1;
            }
            if let Some((address, port)) = from {
                info.from = address.0;
                info.from_port = port;
            }
        }
        Ok(info)
    }
}
