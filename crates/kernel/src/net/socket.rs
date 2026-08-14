//! Сокеты UDP: порт, очередь принятого и владелец.
//!
//! # Почему они живут рядом с картой, а не отдельно
//!
//! Потому что иначе появляется два лока и два порядка их захвата. Входящий кадр
//! разбирается под локом интерфейса и доставляется в сокет — это порядок
//! «интерфейс, потом сокеты». Программа, отправляющая датаграмму, сначала ищет
//! свой сокет, а потом отдаёт кадр карте — порядок обратный. Два таких пути,
//! встретившись, останавливают систему намертво, и ловится это раз в неделю.
//! Один лок делает встречу невозможной по построению.
//!
//! # Сокет принадлежит задаче
//!
//! И умирает вместе с ней: программа, снятая по `kill` или отказавшая, ничего
//! не закрывает сама. Без этого порт остался бы занятым до перезагрузки —
//! ровно то, что в чужих системах называется «адрес уже используется» после
//! падения программы.
//!
//! # Очередь короткая и теряет с конца
//!
//! Восемь датаграмм на сокет. Девятая отбрасывается со счётчиком, а не вытесняет
//! первую: UDP не обещает доставки, и потерять **новое**, сохранив то, что
//! программа ещё не забрала, честнее — иначе медленный читатель получал бы
//! разрозненные обрывки вместо начала обмена.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::net::ipv4::Ipv4;
use crate::sched::TaskId;

/// Сколько сокетов бывает в системе одновременно.
pub const MAX_SOCKETS: usize = 8;

/// Сколько датаграмм ждёт в очереди одного сокета.
const QUEUE_DEPTH: usize = 8;

/// Наибольшая датаграмма, которую мы согласны принять или отправить.
///
/// 1472 — это 1500 байт MTU минус заголовки IPv4 и UDP. Больше не влезет в
/// кадр, а фрагментировать мы не умеем и не собираемся.
pub const MAX_DATAGRAM: usize = 1472;

/// Первый эфемерный порт.
///
/// Диапазон 49152–65535 отведён под них IANA. Занимать что попало нельзя:
/// порт 68, например, принадлежит клиенту DHCP, и выдать его случайному
/// собеседнику значило бы отобрать у него ответы сервера.
const EPHEMERAL_FIRST: u16 = 49152;

/// Принятая датаграмма вместе с тем, от кого она.
pub struct Received {
    pub from: Ipv4,
    pub port: u16,
    pub data: Vec<u8>,
}

/// Один сокет.
struct Socket {
    owner: TaskId,
    local_port: u16,
    /// Куда отправлять, если программа не назвала адрес: результат `connect`.
    peer: Option<(Ipv4, u16)>,
    queue: VecDeque<Received>,
    /// Кто прислал последнюю **забранную** датаграмму.
    ///
    /// Запоминается здесь, а не отдаётся вместе с данными, потому что вызов
    /// приёма и так занял все три аргумента: сокет, буфер, длина. Спросить
    /// отправителя отдельным вызовом можно сразу после приёма и до следующего —
    /// так же, как это делает `recvfrom`, только в два шага.
    last_peer: Option<(Ipv4, u16)>,
    /// Сколько датаграмм не поместилось в очередь.
    dropped: u64,
}

/// Все сокеты системы.
pub struct Table {
    slots: [Option<Socket>; MAX_SOCKETS],
    /// Откуда выдавать следующий эфемерный порт.
    next_ephemeral: u16,
}

/// Почему не вышло.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketError {
    /// Все места заняты.
    TooMany,
    /// Сокета с таким номером нет — или он принадлежит другой задаче.
    BadSocket,
    /// Порт занят другим сокетом.
    PortTaken(u16),
    /// Свободных эфемерных портов не осталось.
    NoPort,
    /// Сокет не привязан к порту, а отправлять с неизвестного порта нельзя:
    /// ответ придёт в никуда.
    NotBound,
    /// Адрес получателя неизвестен: ни `connect`, ни аргумента.
    NoPeer,
    /// Датаграмма длиннее [`MAX_DATAGRAM`].
    TooLong(usize),
}

impl core::fmt::Display for SocketError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooMany => write!(f, "all {MAX_SOCKETS} sockets are in use"),
            Self::BadSocket => f.write_str("no such socket"),
            Self::PortTaken(port) => write!(f, "port {port} is already taken"),
            Self::NoPort => f.write_str("no ephemeral port is free"),
            Self::NotBound => f.write_str("the socket has no local port yet"),
            Self::NoPeer => f.write_str("nobody to send to: connect first"),
            Self::TooLong(len) => write!(f, "{len} bytes is more than {MAX_DATAGRAM}"),
        }
    }
}

impl Table {
    pub const fn new() -> Self {
        Self {
            slots: [const { None }; MAX_SOCKETS],
            next_ephemeral: EPHEMERAL_FIRST,
        }
    }

    /// Завести сокет для задачи. Возвращает его номер.
    pub fn open(&mut self, owner: TaskId) -> Result<usize, SocketError> {
        let index = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(SocketError::TooMany)?;
        self.slots[index] = Some(Socket {
            owner,
            local_port: 0,
            peer: None,
            queue: VecDeque::new(),
            last_peer: None,
            dropped: 0,
        });
        Ok(index)
    }

    /// Привязать сокет к порту. Ноль означает «дайте любой свободный».
    pub fn bind(&mut self, owner: TaskId, index: usize, port: u16) -> Result<u16, SocketError> {
        let port = if port == 0 { self.pick_ephemeral()? } else { port };
        if self.slots.iter().enumerate().any(|(other, slot)| {
            other != index && slot.as_ref().is_some_and(|socket| socket.local_port == port)
        }) {
            return Err(SocketError::PortTaken(port));
        }
        let socket = self.get_mut(owner, index)?;
        socket.local_port = port;
        Ok(port)
    }

    /// Запомнить, с кем этот сокет разговаривает.
    ///
    /// Порт при этом назначается сам, если его ещё нет: программа, которая
    /// только отправляет, не обязана думать про свой порт, а отправлять с
    /// нулевого нельзя — ответ на такую датаграмму отправить некуда.
    pub fn connect(
        &mut self,
        owner: TaskId,
        index: usize,
        address: Ipv4,
        port: u16,
    ) -> Result<u16, SocketError> {
        let local = {
            let socket = self.get_mut(owner, index)?;
            socket.peer = Some((address, port));
            socket.local_port
        };
        if local == 0 {
            return self.bind(owner, index, 0);
        }
        Ok(local)
    }

    /// Кому отправлять и с какого порта.
    pub fn route(&self, owner: TaskId, index: usize) -> Result<(u16, Ipv4, u16), SocketError> {
        let socket = self.get(owner, index)?;
        if socket.local_port == 0 {
            return Err(SocketError::NotBound);
        }
        let (address, port) = socket.peer.ok_or(SocketError::NoPeer)?;
        Ok((socket.local_port, address, port))
    }

    /// Забрать одну принятую датаграмму.
    pub fn take(&mut self, owner: TaskId, index: usize) -> Result<Option<Received>, SocketError> {
        let socket = self.get_mut(owner, index)?;
        let taken = socket.queue.pop_front();
        if let Some(received) = &taken {
            socket.last_peer = Some((received.from, received.port));
        }
        Ok(taken)
    }

    /// Кто прислал последнюю забранную датаграмму.
    pub fn last_peer(&self, owner: TaskId, index: usize) -> Result<Option<(Ipv4, u16)>, SocketError> {
        Ok(self.get(owner, index)?.last_peer)
    }

    /// Закрыть сокет.
    pub fn close(&mut self, owner: TaskId, index: usize) -> Result<(), SocketError> {
        self.get(owner, index)?;
        self.slots[index] = None;
        Ok(())
    }

    /// Закрыть все сокеты задачи — вызывается, когда она кончилась.
    ///
    /// Возвращает, сколько их было: программа, забывшая закрыть сокет, ничем не
    /// отличается от снятой посреди обмена, и в журнале это видно.
    pub fn close_owner(&mut self, owner: TaskId) -> usize {
        let mut closed = 0;
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|socket| socket.owner == owner) {
                *slot = None;
                closed += 1;
            }
        }
        closed
    }

    /// Доставить датаграмму тому, кто слушает этот порт.
    ///
    /// Возвращает `false`, если такого слушателя нет: вызывающий сосчитает это
    /// как непонятый пакет, а не промолчит.
    pub fn deliver(&mut self, port: u16, from: Ipv4, from_port: u16, data: &[u8]) -> bool {
        let Some(socket) = self
            .slots
            .iter_mut()
            .flatten()
            .find(|socket| socket.local_port == port)
        else {
            return false;
        };
        if socket.queue.len() >= QUEUE_DEPTH {
            socket.dropped += 1;
            // Датаграмма потеряна, но слушатель есть, и это правда: сосчитать её
            // как «никто не слушает» значило бы искать несуществующую ошибку в
            // настройке портов вместо настоящей — медленного читателя.
            return true;
        }
        socket.queue.push_back(Received {
            from,
            port: from_port,
            data: data.to_vec(),
        });
        true
    }

    /// Сколько сокетов открыто и сколько датаграмм они потеряли — для `ip`.
    pub fn stats(&self) -> (usize, u64) {
        let open = self.slots.iter().flatten().count();
        let dropped = self.slots.iter().flatten().map(|socket| socket.dropped).sum();
        (open, dropped)
    }

    fn get(&self, owner: TaskId, index: usize) -> Result<&Socket, SocketError> {
        self.slots
            .get(index)
            .and_then(Option::as_ref)
            // Чужой сокет — это `BadSocket`, а не «отказано»: программа не должна
            // узнавать по коду ошибки, существует ли сокет, которого ей не
            // показывали.
            .filter(|socket| socket.owner == owner)
            .ok_or(SocketError::BadSocket)
    }

    fn get_mut(&mut self, owner: TaskId, index: usize) -> Result<&mut Socket, SocketError> {
        self.slots
            .get_mut(index)
            .and_then(Option::as_mut)
            .filter(|socket| socket.owner == owner)
            .ok_or(SocketError::BadSocket)
    }

    /// Выдать свободный порт из эфемерного диапазона.
    fn pick_ephemeral(&mut self) -> Result<u16, SocketError> {
        let taken = |slots: &[Option<Socket>; MAX_SOCKETS], port: u16| {
            slots
                .iter()
                .flatten()
                .any(|socket| socket.local_port == port)
        };
        for _ in 0..=(u16::MAX - EPHEMERAL_FIRST) {
            let port = self.next_ephemeral;
            self.next_ephemeral = if port == u16::MAX { EPHEMERAL_FIRST } else { port + 1 };
            if !taken(&self.slots, port) {
                return Ok(port);
            }
        }
        Err(SocketError::NoPort)
    }
}
