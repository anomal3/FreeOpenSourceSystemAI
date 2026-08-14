//! Сетевой стек: Ethernet, ARP, IPv4 и ICMP поверх virtio-net.
//!
//! # Что здесь есть и чего нет
//!
//! Есть ровно столько, сколько нужно, чтобы система сказала «я здесь» и
//! получила ответ: разбор кадров, таблица ARP, приём и отправка IPv4, эхо. Нет
//! ни UDP, ни TCP, ни настройки адреса по сети — это следующие фазы, и
//! появятся они службами, а не здесь.
//!
//! # Почему приём — в задаче, а не в обработчике прерывания
//!
//! Потому что кадр надо разобрать, а разбор — это чужие данные, произвольная
//! длина и решения, которые иногда заканчиваются отправкой ответа. Делать это
//! в обработчике значит держать прерывания запрещёнными столько, сколько занял
//! разбор, и отвечать на ARP из контекста, где нельзя ни подождать, ни занять
//! лок, который держит кто-то другой. Задача [`service_task`] опрашивает
//! очередь несколько сотен раз в секунду и работает на общих правах — её можно
//! вытеснить, и от этого ничего не сломается.
//!
//! # Одно состояние под одним локом
//!
//! Карта, адрес, таблица ARP и счётчики лежат в одной структуре под одним
//! [`SpinLock`]. Раздельные локи выглядели бы аккуратнее ровно до первого
//! входящего кадра, на который надо ответить: обработка держала бы лок
//! состояния и брала бы лок устройства, а отправка из оболочки — наоборот.
//! Один лок делает такой порядок невозможным по построению; критические секции
//! здесь короткие, и единственное, что делается под ним долго, — это ожидание
//! свободного дескриптора в передающей очереди.

pub mod arp;
pub mod dns;
pub mod eth;
pub mod icmp;
pub mod ipv4;
pub mod socket;
pub mod stream;
pub mod tcp;
pub mod udp;

use crate::sched::TaskId;
use crate::sync::SpinLock;
use crate::virtio::net::{FRAME_MAX, Stats, VirtioNet};
use crate::virtio::VirtioError;
use eth::Mac;
use ipv4::Ipv4;

/// Сколько миллисекунд задача-приёмник спит между обходами очереди.
///
/// Пять — это компромисс, который видно на глаз: столько же добавляется к
/// времени отклика `ping`, и столько же ядро не тратит на пустые обходы. При
/// шестнадцати приёмных буферах этого хватает, чтобы не терять кадры на
/// эмулируемой карте; настоящее железо потребует прерываний, и это честно
/// названо в дорожной карте, а не спрятано.
const POLL_INTERVAL_MS: u64 = 5;

/// Идентификатор, которым система подписывает свои эхо-запросы.
///
/// Постоянный, а не случайный: случайности в ядре пока взять негде (её
/// источник появится вместе с ключом хоста SSH), а различать надо не наши
/// запросы между собой, а наши от чужих.
const ECHO_ID: u16 = 0x4F53; // 'OS'

/// Почему не вышло.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Сетевой карты в системе нет.
    NoDevice,
    /// Адрес не настроен: отправлять не от чьего имени.
    NoAddress,
    /// Адрес не в нашей сети, а шлюза нет.
    NoRoute,
    /// Аппаратный адрес получателя ещё неизвестен, запрос ARP отправлен.
    Pending,
    /// Ответа не дождались.
    Timeout,
    /// Устройство отказалось отправлять.
    Device(VirtioError),
    /// Не получилось с сокетом.
    Socket(socket::SocketError),
    /// Не получилось с соединением.
    Stream(stream::StreamError),
    /// Имя не годится: пустое, слишком длинное или с пустой меткой.
    BadName,
    /// Сервер имён ответил, и ответ — «такого имени нет».
    ///
    /// Отдельно от [`NetError::Timeout`] намеренно: молчание сервера означает
    /// «спросите ещё раз», а этот ответ — «перестаньте спрашивать». Слить их в
    /// одно значило бы превратить опечатку в имени в загадочную неисправность
    /// сети.
    NoSuchName,
}

impl core::fmt::Display for NetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoDevice => f.write_str("no network card"),
            Self::NoAddress => f.write_str("no address configured, use: ip <addr>/<bits> [gateway]"),
            Self::NoRoute => f.write_str("the address is outside our network and there is no gateway"),
            Self::Pending => f.write_str("the hardware address is not known yet"),
            Self::Timeout => f.write_str("no answer"),
            Self::Device(err) => write!(f, "the card refused the frame: {err}"),
            Self::Socket(err) => write!(f, "{err}"),
            Self::Stream(err) => write!(f, "{err}"),
            Self::BadName => f.write_str("that name cannot be asked about"),
            Self::NoSuchName => f.write_str("the name server says there is no such name"),
        }
    }
}

/// Счётчики стека — то, что нельзя посмотреть у карты.
#[derive(Debug, Clone, Copy, Default)]
pub struct Counters {
    pub arp_requests_in: u64,
    pub arp_replies_in: u64,
    pub arp_requests_out: u64,
    pub arp_replies_out: u64,
    pub ipv4_in: u64,
    /// Кадры, разобрать которые не удалось или которые адресованы не нам.
    pub ignored: u64,
    pub echo_requests_in: u64,
    pub echo_replies_out: u64,
    pub echo_requests_out: u64,
    pub echo_replies_in: u64,
    pub udp_in: u64,
    pub udp_out: u64,
    /// Датаграммы, пришедшие на порт, который никто не слушает.
    pub udp_no_listener: u64,
    pub tcp_in: u64,
    pub tcp_out: u64,
    /// Сегменты, отправленные повторно, — мера того, сколько теряется.
    pub tcp_retransmits: u64,
    /// Соединения, оборванные молчанием собеседника.
    pub tcp_timeouts: u64,
    pub tcp_resets: u64,
}

/// Пришедший эхо-ответ: кто ответил и когда.
#[derive(Clone, Copy)]
struct Reply {
    from: Ipv4,
    id: u16,
    sequence: u16,
    at_ms: u64,
}

/// Всё состояние сети.
struct Interface {
    device: VirtioNet,
    mac: Mac,
    address: Ipv4,
    netmask: Ipv4,
    gateway: Ipv4,
    table: arp::Table,
    sockets: socket::Table,
    streams: stream::Streams,
    counters: Counters,
    /// Адрес сервера имён — от DHCP или заданный руками.
    dns: Ipv4,
    /// Последний пришедший эхо-ответ. Одно место, а не очередь: `ping` в
    /// системе один и спрашивает про конкретный номер.
    last_reply: Option<Reply>,
    /// Счётчик исходящих пакетов IPv4 — он же их идентификатор.
    next_id: u16,
}

static INTERFACE: SpinLock<Option<Interface>> = SpinLock::new(None);

/// Найти сетевую карту и поднять её.
///
/// # Safety
///
/// Ядро должно исполняться на собственных таблицах страниц, а `rsdp` — быть
/// адресом таблиц ACPI из хэндоффа (ноль означает «таблиц нет»).
pub unsafe fn init(rsdp: u64) {
    crate::kprintln!();
    crate::kprintln!("---- network ----------------------------------------------------");

    if rsdp == 0 {
        crate::kprintln!("  network     : no ACPI tables, so no PCI: no network on this machine");
        return;
    }

    // SAFETY: контракт функции.
    let root = match unsafe { crate::pci::Root::discover(rsdp) } {
        Ok(root) => root,
        Err(err) => {
            crate::kprintln!("  network     : no PCI at all ({err}): no network");
            return;
        }
    };

    // SAFETY: см. выше.
    let device = match unsafe { VirtioNet::probe(&root) } {
        Ok(device) => device,
        Err(VirtioError::NoCapabilities) => {
            // Карты нет — это обычное состояние машины, запущенной без сети, а
            // не поломка. Сказать об этом надо, чтобы «сеть не работает» не
            // приходилось выяснять расспросами.
            crate::kprintln!("  network     : no virtio-net card attached to this machine");
            return;
        }
        Err(err) => {
            crate::kprintln!("  network     : the card did not come up: {err}");
            return;
        }
    };

    let mac = device.mac();
    crate::kprintln!("  network     : virtio-net, hardware address {}", eth::Display(mac));

    *INTERFACE.lock() = Some(Interface {
        device,
        mac,
        address: Ipv4::UNSPECIFIED,
        netmask: Ipv4::UNSPECIFIED,
        gateway: Ipv4::UNSPECIFIED,
        table: arp::Table::new(),
        sockets: socket::Table::new(),
        streams: stream::Streams::new(),
        counters: Counters::default(),
        dns: Ipv4::UNSPECIFIED,
        last_reply: None,
        next_id: 1,
    });

    // Задача-приёмник заводится сразу и служебной: она работает, пока работает
    // система, но сама по себе поводом ей работать не является — иначе `exit` в
    // оболочке перестал бы останавливать машину.
    match crate::sched::spawn_daemon("net", service_task) {
        Ok(id) => crate::kprintln!("  network     : receive task {id}, polling every {POLL_INTERVAL_MS} ms"),
        Err(err) => crate::kprintln!("  network     : the receive task did not start: {err}"),
    }
    crate::kprintln!("  network     : no address yet; set one with `ip <addr>/<bits> [gateway]`");
}

/// Есть ли в системе сетевая карта.
pub fn is_present() -> bool {
    INTERFACE.lock().is_some()
}

/// Настройка интерфейса — то, что показывает команда `ip`.
#[derive(Clone, Copy)]
pub struct Status {
    pub mac: Mac,
    pub address: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
    pub link: Stats,
    pub counters: Counters,
    /// Сколько сокетов открыто и сколько датаграмм они потеряли.
    pub sockets: (usize, u64),
    /// Сколько соединений живо и сколько из них установлено.
    pub streams: (usize, usize),
}

pub fn status() -> Option<Status> {
    let guard = INTERFACE.lock();
    let iface = guard.as_ref()?;
    Some(Status {
        mac: iface.mac,
        address: iface.address,
        netmask: iface.netmask,
        gateway: iface.gateway,
        dns: iface.dns,
        link: iface.device.stats(),
        counters: iface.counters,
        sockets: iface.sockets.stats(),
        streams: iface.streams.summary(),
    })
}

/// Задать адрес, маску и шлюз.
///
/// Шлюз проверяется на принадлежность нашей сети: шлюз, до которого нельзя
/// достучаться напрямую, — это опечатка, и обнаружить её лучше здесь, чем по
/// молчащему `ping` через минуту.
pub fn configure(address: Ipv4, netmask: Ipv4, gateway: Ipv4) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    if !gateway.is_unspecified() && !gateway.same_network(address, netmask) {
        return Err(NetError::NoRoute);
    }
    iface.address = address;
    iface.netmask = netmask;
    iface.gateway = gateway;
    Ok(())
}

/// Задать настройки целиком — то, что приносит клиент DHCP.
///
/// Отличается от [`configure`] только сервером имён: ноль в нём означает «не
/// менять», потому что аренда без предложенного сервера имён — обычное дело, а
/// стереть уже известный из-за этого было бы потерей.
pub fn configure_all(
    address: Ipv4,
    netmask: Ipv4,
    gateway: Ipv4,
    dns: Ipv4,
) -> Result<(), NetError> {
    configure(address, netmask, gateway)?;
    if !dns.is_unspecified() {
        let mut guard = INTERFACE.lock();
        guard.as_mut().ok_or(NetError::NoDevice)?.dns = dns;
    }
    Ok(())
}

/// Адрес сервера имён, если он известен.
pub fn dns_server() -> Option<Ipv4> {
    let guard = INTERFACE.lock();
    let dns = guard.as_ref()?.dns;
    (!dns.is_unspecified()).then_some(dns)
}

// ---------------------------------------------------------------------------
// Сокеты
// ---------------------------------------------------------------------------

/// Завести сокет для задачи.
pub fn socket_open(owner: TaskId) -> Result<usize, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.sockets.open(owner).map_err(NetError::Socket)
}

/// Привязать сокет к порту; ноль — «любой свободный».
pub fn socket_bind(owner: TaskId, index: usize, port: u16) -> Result<u16, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.sockets.bind(owner, index, port).map_err(NetError::Socket)
}

/// Запомнить собеседника.
pub fn socket_connect(
    owner: TaskId,
    index: usize,
    address: Ipv4,
    port: u16,
) -> Result<u16, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface
        .sockets
        .connect(owner, index, address, port)
        .map_err(NetError::Socket)
}

/// Отправить датаграмму собеседнику.
///
/// [`NetError::Pending`] означает, что ушёл запрос ARP и повторить стоит через
/// несколько миллисекунд. Ждать здесь нельзя: лок держит прерывания
/// запрещёнными, а ответ придёт задаче-приёмнику, которой для этого надо дать
/// поработать.
pub fn socket_send(owner: TaskId, index: usize, data: &[u8]) -> Result<usize, NetError> {
    if data.len() > socket::MAX_DATAGRAM {
        return Err(NetError::Socket(socket::SocketError::TooLong(data.len())));
    }
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    let (local_port, destination, remote_port) =
        iface.sockets.route(owner, index).map_err(NetError::Socket)?;

    let source = iface.address;
    let mut message = [0u8; udp::HEADER + socket::MAX_DATAGRAM];
    let len = udp::write(
        &mut message,
        source,
        destination,
        local_port,
        remote_port,
        data,
    );
    send_ipv4(iface, destination, ipv4::PROTOCOL_UDP, &message[..len])?;
    iface.counters.udp_out += 1;
    Ok(data.len())
}

/// Забрать принятую датаграмму, если она есть.
pub fn socket_recv(owner: TaskId, index: usize) -> Result<Option<socket::Received>, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.sockets.take(owner, index).map_err(NetError::Socket)
}

// ---------------------------------------------------------------------------
// Соединения TCP — то, чем пользуются программы
// ---------------------------------------------------------------------------

/// Завести соединение (пока ни к кому не подключённое).
pub fn stream_open(owner: TaskId) -> Result<usize, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.streams.open(owner).map_err(NetError::Stream)
}

/// Привязать к порту.
pub fn stream_bind(owner: TaskId, index: usize, port: u16) -> Result<u16, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.streams.bind(owner, index, port).map_err(NetError::Stream)
}

/// Начать слушать входящие соединения.
pub fn stream_listen(owner: TaskId, index: usize) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.streams.listen(owner, index).map_err(NetError::Stream)
}

/// Забрать установленное соединение из очереди слушающего.
///
/// Не ждёт: пустая очередь — это `None`, и программа сама решает, спать ей или
/// заняться другим.
pub fn stream_accept(owner: TaskId, index: usize) -> Result<Option<usize>, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.streams.accept(owner, index).map_err(NetError::Stream)
}

/// Начать соединение.
pub fn stream_connect(
    owner: TaskId,
    index: usize,
    address: Ipv4,
    port: u16,
) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    if iface.address.is_unspecified() {
        return Err(NetError::NoAddress);
    }

    let now = crate::time::uptime_ms();
    // Локальный порт назначается из эфемерного диапазона тем же счётчиком, что
    // и у датаграмм: два диапазона на одну машину означали бы, что один и тот же
    // номер занят дважды разными протоколами — законно, но объяснять это себе
    // через полгода никому не нужно.
    let local = iface.sockets.pick_port().map_err(NetError::Socket)?;

    {
        let conn = iface.streams.get_mut(owner, index).map_err(NetError::Stream)?;
        let iss = stream::initial_sequence(now);
        conn.local_port = local;
        conn.remote = address;
        conn.remote_port = port;
        conn.snd_una = iss;
        conn.snd_nxt = iss;
        // Окно собеседника ещё неизвестно; до его `SYN` мы всё равно ничего не
        // отправим, кроме самого `SYN`.
        conn.snd_wnd = 0;
        conn.state = stream::State::SynSent;
    }
    tcp_pump(iface, index);
    Ok(())
}

/// В каком состоянии соединение.
pub fn stream_state(owner: TaskId, index: usize) -> Result<(stream::State, bool, bool), NetError> {
    let guard = INTERFACE.lock();
    let iface = guard.as_ref().ok_or(NetError::NoDevice)?;
    let conn = iface.streams.get(owner, index).map_err(NetError::Stream)?;
    Ok((conn.state, conn.peer_closed, conn.reset))
}

/// Отдать байты на отправку. Возвращает, сколько принято.
pub fn stream_send(owner: TaskId, index: usize, data: &[u8]) -> Result<usize, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    let taken = {
        let conn = iface.streams.get_mut(owner, index).map_err(NetError::Stream)?;
        if conn.reset {
            return Err(NetError::Stream(stream::StreamError::Reset));
        }
        if !conn.state.is_open() {
            return Err(NetError::Stream(stream::StreamError::NotConnected));
        }
        if conn.closing {
            return Err(NetError::Stream(stream::StreamError::Closed));
        }
        // Буфер отправки — это ещё и окно повторной передачи: пока байт не
        // подтверждён, он обязан лежать здесь. Поэтому его размер и есть предел
        // того, сколько программа может отдать вперёд.
        let room = stream::SEND_BUFFER.saturating_sub(conn.send.len());
        if room == 0 {
            return Err(NetError::Stream(stream::StreamError::WouldBlock));
        }
        let taken = data.len().min(room);
        conn.send.extend(&data[..taken]);
        taken
    };
    tcp_pump(iface, index);
    Ok(taken)
}

/// Забрать принятые байты.
///
/// Ноль означает «пока ничего»; конец потока программа узнаёт по
/// [`stream_state`], где `peer_closed` — это чужой `FIN`.
pub fn stream_recv(owner: TaskId, index: usize, out: &mut [u8]) -> Result<usize, NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    let (taken, need_window_update) = {
        let conn = iface.streams.get_mut(owner, index).map_err(NetError::Stream)?;
        if conn.reset && conn.recv.is_empty() {
            return Err(NetError::Stream(stream::StreamError::Reset));
        }
        let was_full = conn.window() == 0;
        let mut taken = 0;
        while taken < out.len() {
            match conn.recv.pop_front() {
                Some(byte) => {
                    out[taken] = byte;
                    taken += 1;
                }
                None => break,
            }
        }
        // Освободившееся место надо объявить: собеседник, которому мы когда-то
        // сказали «окно ноль», сам не догадается, что оно открылось, и будет
        // молчать до своего таймера.
        (taken, was_full && taken > 0)
    };
    if need_window_update {
        if let Some(conn) = iface.streams.at(index) {
            conn.need_ack = true;
        }
        tcp_pump(iface, index);
    }
    Ok(taken)
}

/// Закрыть свою половину: отправить `FIN`, когда кончатся данные.
pub fn stream_shutdown(owner: TaskId, index: usize) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    {
        let conn = iface.streams.get_mut(owner, index).map_err(NetError::Stream)?;
        conn.closing = true;
    }
    tcp_pump(iface, index);
    Ok(())
}

/// Закрыть соединение и освободить слот.
pub fn stream_close(owner: TaskId, index: usize) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    // Проверка владельца обязательна и здесь: закрыть чужое соединение —
    // это разорвать чужую связь.
    iface.streams.get(owner, index).map_err(NetError::Stream)?;
    {
        let conn = iface.streams.get_mut(owner, index).map_err(NetError::Stream)?;
        conn.closing = true;
    }
    tcp_pump(iface, index);

    // Слот освобождается сразу, а не после завершения рукопожатия закрытия:
    // держать его до `TIME_WAIT` при восьми слотах на систему означало бы, что
    // три закрытых подряд соединения исчерпывают её.
    let finished = iface.streams.at(index).is_some_and(|conn| {
        matches!(
            conn.state,
            stream::State::Closed | stream::State::TimeWait | stream::State::Listen
        ) || conn.reset
    });
    if finished {
        iface.streams.close(index);
    }
    Ok(())
}

/// Что показывает команда оболочки.
pub fn stream_table() -> alloc::vec::Vec<(usize, &'static str, u16, Ipv4, u16, usize, usize)> {
    let guard = INTERFACE.lock();
    match guard.as_ref() {
        Some(iface) => iface
            .streams
            .describe()
            .map(|(index, state, local, remote, port, send, recv)| {
                (index, state.name(), local, remote, port, send, recv)
            })
            .collect(),
        None => alloc::vec::Vec::new(),
    }
}

/// Кто прислал последнюю забранную датаграмму.
pub fn socket_peer(owner: TaskId, index: usize) -> Result<Option<(Ipv4, u16)>, NetError> {
    let guard = INTERFACE.lock();
    let iface = guard.as_ref().ok_or(NetError::NoDevice)?;
    iface.sockets.last_peer(owner, index).map_err(NetError::Socket)
}

/// Закрыть сокет.
pub fn socket_close(owner: TaskId, index: usize) -> Result<(), NetError> {
    let mut guard = INTERFACE.lock();
    let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
    iface.sockets.close(owner, index).map_err(NetError::Socket)
}

/// Закрыть всё, что осталось от задачи.
///
/// Вызывается на **всех** путях выхода программы, включая снятие по отказу:
/// порт, оставшийся занятым после падения, не освободится уже никогда.
pub fn close_owner(owner: TaskId) -> usize {
    let mut guard = INTERFACE.lock();
    match guard.as_mut() {
        // Соединения TCP закрываются здесь же и грубо — слот освобождается без
        // прощального `FIN`. Это правильно: программы, которая могла бы
        // договориться о закрытии, больше нет, а собеседник узнает об обрыве по
        // `RST` на первый же свой сегмент.
        Some(iface) => iface.sockets.close_owner(owner) + iface.streams.close_owner(owner),
        None => 0,
    }
}

/// Перебрать живые записи таблицы ARP.
///
/// Возвращает копию, а не итератор по таблице: итератор держал бы лок ровно
/// столько, сколько печатает вызывающий, — то есть пока задача-приёмник ждёт
/// своей очереди обработать кадр.
pub fn arp_table() -> alloc::vec::Vec<(Ipv4, Mac, u64)> {
    let now = crate::time::uptime_ms();
    let guard = INTERFACE.lock();
    match guard.as_ref() {
        Some(iface) => iface.table.iter(now).collect(),
        None => alloc::vec::Vec::new(),
    }
}

/// Задача, забирающая кадры у карты.
///
/// Лок берётся на каждый кадр отдельно, а не на весь обход: обработка одного
/// кадра — это микросекунды, а очередь, забитая под завязку, иначе означала бы
/// шестнадцать кадров подряд с запрещёнными прерываниями.
pub fn service_task() {
    let mut frame = [0u8; FRAME_MAX];
    loop {
        loop {
            let mut guard = INTERFACE.lock();
            let Some(iface) = guard.as_mut() else {
                // Карты не стало — такого сегодня не бывает, но задача, которая
                // в этом случае крутилась бы вхолостую, была бы хуже.
                return;
            };
            let Some(len) = iface.device.receive(&mut frame) else {
                break;
            };
            // Кадр разбирается под тем же локом: ответ на него уйдёт через ту
            // же карту, и отпускать лок между «поняли, что это ARP-запрос» и
            // «ответили на него» незачем.
            handle(iface, &frame[..len]);
        }

        // Таймеры TCP крутятся здесь же, а не отдельной задачей: повторная
        // передача — это отправка, и делать её из второго места значило бы
        // второй лок или второй порядок его захвата.
        {
            let mut guard = INTERFACE.lock();
            if let Some(iface) = guard.as_mut() {
                tcp_tick(iface);
            }
        }

        crate::sched::sleep_ms(POLL_INTERVAL_MS);
    }
}

/// Разобрать принятый кадр и, если он того требует, ответить.
fn handle(iface: &mut Interface, frame: &[u8]) {
    let Some(parsed) = eth::parse(frame) else {
        iface.counters.ignored += 1;
        return;
    };

    // Кадр, адресованный не нам и не всем, приходить не должен: карта фильтрует
    // по своему адресу сама. Проверка здесь потому, что «не должен» — это
    // свойство сегодняшней карты, а не закон природы: в неразборчивом режиме или
    // на чужом железе такие кадры пойдут, и разбирать их как свои значит
    // отвечать за чужой адрес.
    if parsed.destination != iface.mac && parsed.destination != eth::BROADCAST {
        iface.counters.ignored += 1;
        return;
    }

    match parsed.kind {
        eth::TYPE_ARP => handle_arp(iface, parsed.payload),
        eth::TYPE_IPV4 => handle_ipv4(iface, parsed.payload),
        _ => iface.counters.ignored += 1,
    }
}

fn handle_arp(iface: &mut Interface, payload: &[u8]) {
    let Some(packet) = arp::parse(payload) else {
        iface.counters.ignored += 1;
        return;
    };
    let now = crate::time::uptime_ms();

    // Отправитель запоминается в любом случае — и у запроса, и у ответа: тот,
    // кто нас спрашивает, и есть тот, кому мы сейчас будем отвечать.
    if !packet.sender_ip.is_unspecified() {
        iface.table.remember(packet.sender_ip, packet.sender_mac, now);
    }

    match packet.operation {
        arp::REQUEST => {
            iface.counters.arp_requests_in += 1;
            // Отвечаем только за свой адрес и только если он у нас есть.
            // Система без адреса, отвечающая на ARP, — это машина, которая
            // объявляет себя владельцем чужого адреса.
            if iface.address.is_unspecified() || packet.target_ip != iface.address {
                return;
            }
            let mut buffer = [0u8; eth::HEADER + arp::PACKET];
            eth::write_header(&mut buffer, packet.sender_mac, iface.mac, eth::TYPE_ARP);
            arp::write(
                &mut buffer[eth::HEADER..],
                arp::REPLY,
                iface.mac,
                iface.address,
                packet.sender_mac,
                packet.sender_ip,
            );
            if iface.device.send(&buffer).is_ok() {
                iface.counters.arp_replies_out += 1;
            }
        }
        arp::REPLY => {
            iface.counters.arp_replies_in += 1;
            // Соединения, которые упёрлись в неизвестный аппаратный адрес,
            // будятся сразу, а не по своему таймеру. Разница — десятки
            // миллисекунд на каждом первом обращении к новому собеседнику, и
            // она видна: без этого первый же сегмент к нему всегда «теряется».
            let waiting: alloc::vec::Vec<usize> = iface
                .streams
                .indices()
                .filter(|index| {
                    iface
                        .streams
                        .peek(*index)
                        .is_some_and(|conn| conn.remote == packet.sender_ip)
                })
                .collect();
            for index in waiting {
                tcp_pump(iface, index);
            }
        }
        _ => iface.counters.ignored += 1,
    }
}

fn handle_ipv4(iface: &mut Interface, payload: &[u8]) {
    let Some(packet) = ipv4::parse(payload) else {
        iface.counters.ignored += 1;
        return;
    };
    iface.counters.ipv4_in += 1;

    // Свой адрес, широковещательный адрес сети и «всем» — три случая, в которых
    // пакет наш. Остальное не наше: маршрутизацией мы не занимаемся, и молча
    // обрабатывать чужой пакет значило бы притворяться маршрутизатором.
    let ours = packet.destination == iface.address
        || packet.destination.is_broadcast()
        || (!iface.netmask.is_unspecified()
            && packet.destination == iface.address.network_broadcast(iface.netmask))
        // Пока адреса нет, «наш» — это любой пакет, который карта нам отдала:
        // она уже отфильтровала кадры по аппаратному адресу, а сервер DHCP
        // вправе ответить на предлагаемый адрес, которого у нас ещё нет.
        // Отбросить такой ответ значит никогда не получить адрес и остаться в
        // этом состоянии навсегда.
        || iface.address.is_unspecified();
    if !ours {
        iface.counters.ignored += 1;
        return;
    }

    if packet.protocol == ipv4::PROTOCOL_UDP {
        handle_udp(iface, packet.source, packet.destination, packet.payload);
        return;
    }

    if packet.protocol == ipv4::PROTOCOL_TCP {
        // Сегмент копируется на стек: разбор одалживает кадр приёмного буфера,
        // а ответ на этот же сегмент уйдёт через тот же буфер, и держать
        // заимствование до отправки нельзя.
        let mut segment = [0u8; FRAME_MAX];
        let len = packet.payload.len().min(FRAME_MAX);
        segment[..len].copy_from_slice(&packet.payload[..len]);
        let source = packet.source;
        let destination = packet.destination;
        handle_tcp(iface, source, destination, len, &segment);
        return;
    }

    if packet.protocol != ipv4::PROTOCOL_ICMP {
        iface.counters.ignored += 1;
        return;
    }

    let Some(echo) = icmp::parse(packet.payload) else {
        iface.counters.ignored += 1;
        return;
    };

    match echo.kind {
        icmp::ECHO_REQUEST => {
            iface.counters.echo_requests_in += 1;
            // Отвечать от имени адреса, которого нет, нельзя: ответ уйдёт с
            // источником 0.0.0.0 и будет отброшен на той стороне.
            if iface.address.is_unspecified() {
                return;
            }
            // Данные возвращаются как есть — на этом держится вся проверка:
            // отвечающий обязан вернуть их без изменений.
            let mut message = [0u8; FRAME_MAX];
            let len = icmp::write(
                &mut message,
                icmp::ECHO_REPLY,
                echo.id,
                echo.sequence,
                echo.data,
            );
            if send_ipv4(iface, packet.source, ipv4::PROTOCOL_ICMP, &message[..len]).is_ok() {
                iface.counters.echo_replies_out += 1;
            }
        }
        icmp::ECHO_REPLY => {
            iface.counters.echo_replies_in += 1;
            iface.last_reply = Some(Reply {
                from: packet.source,
                id: echo.id,
                sequence: echo.sequence,
                at_ms: crate::time::uptime_ms(),
            });
        }
        _ => iface.counters.ignored += 1,
    }
}

fn handle_udp(iface: &mut Interface, source: Ipv4, destination: Ipv4, payload: &[u8]) {
    let Some(datagram) = udp::parse(source, destination, payload) else {
        iface.counters.ignored += 1;
        return;
    };
    iface.counters.udp_in += 1;

    if !iface.sockets.deliver(
        datagram.destination_port,
        source,
        datagram.source_port,
        datagram.payload,
    ) {
        // Никто не слушает. Полагалось бы ответить ICMP «порт недостижим», и
        // когда-нибудь мы это сделаем; пока — счётчик, потому что молчание в
        // ответ на датаграмму и молчание из-за поломки выглядят одинаково.
        iface.counters.udp_no_listener += 1;
    }
}

// ---------------------------------------------------------------------------
// TCP
// ---------------------------------------------------------------------------

/// Разобрать сегмент и продвинуть автомат соединения.
fn handle_tcp(
    iface: &mut Interface,
    source: Ipv4,
    destination: Ipv4,
    len: usize,
    buffer: &[u8; FRAME_MAX],
) {
    let Some(segment) = tcp::parse(source, destination, &buffer[..len]) else {
        iface.counters.ignored += 1;
        return;
    };
    iface.counters.tcp_in += 1;

    let now = crate::time::uptime_ms();
    let existing = iface
        .streams
        .lookup(segment.destination_port, source, segment.source_port);

    let Some(index) = existing else {
        // Соединения нет. Либо это `SYN` кому-то слушающему, либо сегмент,
        // адресованный никому.
        if segment.has(tcp::SYN) && !segment.has(tcp::ACK) {
            if let Some(listener) = iface.streams.listener_for(segment.destination_port) {
                let iss = stream::initial_sequence(now);
                if let Some(index) = iface.streams.accept_syn(
                    listener,
                    source,
                    segment.source_port,
                    segment.sequence,
                    segment.window,
                    segment.mss,
                    iss,
                ) {
                    tcp_pump(iface, index);
                    return;
                }
                // Очередь слушателя переполнена или слотов не осталось. Отказ
                // сегментом `RST` честнее молчания: клиент узнает сразу, а не
                // будет повторять `SYN` до своего таймаута.
            }
        }
        if !segment.has(tcp::RST) {
            tcp_reject(iface, source, &segment);
        }
        return;
    };

    // Сегмент, оборвавший связь, — это конец без переписки: ни подтверждений,
    // ни ответного `RST`, иначе два стека забросают друг друга ими.
    if segment.has(tcp::RST) {
        if let Some(conn) = iface.streams.at(index) {
            conn.reset = true;
            conn.state = stream::State::Closed;
        }
        return;
    }

    let mut hand_over = false;
    let mut send_reset = false;

    if let Some(conn) = iface.streams.at(index) {
        conn.snd_wnd = segment.window;
        if let Some(mss) = segment.mss {
            conn.peer_mss = mss.clamp(536, tcp::MSS as u16);
        }

        match conn.state {
            stream::State::SynSent => {
                if segment.has(tcp::SYN) && segment.has(tcp::ACK) {
                    // Подтверждение обязано покрывать наш `SYN` и ничего сверх:
                    // всё прочее означает собеседника, отвечающего не нам.
                    if segment.acknowledgement == conn.snd_nxt {
                        conn.rcv_nxt = segment.sequence.wrapping_add(1);
                        conn.acknowledge(segment.acknowledgement);
                        conn.state = stream::State::Established;
                        conn.need_ack = true;
                    } else {
                        send_reset = true;
                    }
                } else if segment.has(tcp::SYN) {
                    // Одновременное открытие: оба послали `SYN`. Редкость, но
                    // законная, и без этой ветки соединение зависло бы.
                    conn.rcv_nxt = segment.sequence.wrapping_add(1);
                    conn.state = stream::State::SynReceived;
                    conn.need_ack = true;
                }
            }
            stream::State::SynReceived => {
                if segment.has(tcp::ACK) && segment.acknowledgement == conn.snd_nxt {
                    conn.acknowledge(segment.acknowledgement);
                    conn.state = stream::State::Established;
                    hand_over = conn.listener.is_some();
                    // Данные могли приехать вместе с подтверждением.
                    conn.accept_data(segment.sequence, segment.payload);
                }
            }
            stream::State::Established
            | stream::State::FinWait1
            | stream::State::FinWait2
            | stream::State::CloseWait
            | stream::State::Closing
            | stream::State::LastAck => {
                if segment.has(tcp::ACK) {
                    conn.acknowledge(segment.acknowledgement);
                }
                conn.accept_data(segment.sequence, segment.payload);

                // `FIN` считается принятым, только если он ровно на границе
                // потока: закрытие, признанное раньше пропущенных данных, — это
                // потерянный хвост файла.
                let fin_here = segment.has(tcp::FIN)
                    && segment.sequence.wrapping_add(segment.payload.len() as u32)
                        == conn.rcv_nxt;
                if fin_here && !conn.peer_closed {
                    conn.peer_closed = true;
                    conn.rcv_nxt = conn.rcv_nxt.wrapping_add(1);
                    conn.need_ack = true;
                }

                // Наш `FIN` подтверждён, если подтверждение дошло до `snd_nxt`,
                // а `FIN` занимает последний номер.
                let our_fin_acked = conn.fin_sent && conn.snd_una == conn.snd_nxt;

                conn.state = match (conn.state, fin_here || conn.peer_closed, our_fin_acked) {
                    (stream::State::Established, true, _) => stream::State::CloseWait,
                    (stream::State::FinWait1, true, true) => {
                        conn.expires_at = stream::time_wait_until(now);
                        stream::State::TimeWait
                    }
                    (stream::State::FinWait1, true, false) => stream::State::Closing,
                    (stream::State::FinWait1, false, true) => stream::State::FinWait2,
                    (stream::State::FinWait2, true, _) => {
                        conn.expires_at = stream::time_wait_until(now);
                        stream::State::TimeWait
                    }
                    (stream::State::Closing, _, true) => {
                        conn.expires_at = stream::time_wait_until(now);
                        stream::State::TimeWait
                    }
                    (stream::State::LastAck, _, true) => stream::State::Closed,
                    (state, _, _) => state,
                };
            }
            stream::State::TimeWait => {
                // Запоздавший сегмент старого соединения: подтверждаем и
                // продлеваем ожидание, чтобы собеседник успел закончить.
                conn.need_ack = true;
                conn.expires_at = stream::time_wait_until(now);
            }
            stream::State::Closed | stream::State::Listen => {}
        }
    }

    if send_reset {
        tcp_reject(iface, source, &segment);
        iface.streams.close(index);
        return;
    }
    if hand_over {
        iface.streams.hand_to_listener(index);
    }
    tcp_pump(iface, index);

    // Закрытое соединение, которое никто не забирал (его не отдали программе),
    // убирается сразу: слот дороже истории.
    let orphan = iface
        .streams
        .at(index)
        .is_some_and(|conn| conn.state == stream::State::Closed && conn.listener.is_some());
    if orphan {
        iface.streams.close(index);
    }
}

/// Ответить `RST` на сегмент, которому некуда прийти.
fn tcp_reject(iface: &mut Interface, source: Ipv4, segment: &tcp::Segment<'_>) {
    // Номера в отказе выбираются по правилу RFC 793: если подтверждения не
    // было, отказ несёт нулевой номер и подтверждает всё, что пришло; иначе
    // берётся номер из подтверждения. Промах здесь означает `RST`, который
    // собеседник законно проигнорирует.
    let (sequence, acknowledgement, flags) = if segment.has(tcp::ACK) {
        (segment.acknowledgement, 0, tcp::RST)
    } else {
        (
            0,
            segment.sequence.wrapping_add(segment.span()),
            tcp::RST | tcp::ACK,
        )
    };

    let out = tcp::Outgoing {
        source_port: segment.destination_port,
        destination_port: segment.source_port,
        sequence,
        acknowledgement,
        flags,
        window: 0,
        with_mss: false,
        payload: &[],
    };
    let mut buffer = [0u8; tcp::HEADER + 4];
    let len = tcp::write(&mut buffer, iface.address, source, &out);
    if send_ipv4(iface, source, ipv4::PROTOCOL_TCP, &buffer[..len]).is_ok() {
        iface.counters.tcp_out += 1;
        iface.counters.tcp_resets += 1;
    }
}

/// Отправить всё, что соединение задолжало: `SYN`, данные, `FIN`, подтверждение.
fn tcp_pump(iface: &mut Interface, index: usize) {
    let now = crate::time::uptime_ms();

    loop {
        // Решение принимается по копии полей: дальше начнётся отправка, а она
        // одалживает интерфейс целиком.
        let Some(conn) = iface.streams.at(index) else { return };
        if conn.reset {
            return;
        }

        let state = conn.state;
        let mut flags = 0u8;
        let mut with_mss = false;
        let mut take = 0usize;
        let mut fin = false;

        match state {
            stream::State::SynSent if conn.snd_nxt == conn.snd_una => {
                flags = tcp::SYN;
                with_mss = true;
            }
            stream::State::SynReceived if conn.snd_nxt == conn.snd_una => {
                flags = tcp::SYN | tcp::ACK;
                with_mss = true;
            }
            stream::State::Established | stream::State::CloseWait => {
                take = conn.sendable();
                if take > 0 {
                    flags = tcp::ACK | tcp::PSH;
                } else if conn.closing && !conn.fin_sent && conn.in_flight() == 0 {
                    flags = tcp::ACK | tcp::FIN;
                    fin = true;
                } else if conn.need_ack {
                    flags = tcp::ACK;
                } else {
                    return;
                }
            }
            stream::State::FinWait1
            | stream::State::Closing
            | stream::State::LastAck
            | stream::State::FinWait2
            | stream::State::TimeWait => {
                if conn.closing && !conn.fin_sent && conn.in_flight() == 0 {
                    flags = tcp::ACK | tcp::FIN;
                    fin = true;
                } else if conn.need_ack {
                    flags = tcp::ACK;
                } else {
                    return;
                }
            }
            _ => return,
        }

        // Данные берутся из очереди отправки начиная с того места, докуда уже
        // дошли: `send` хранит всё от `snd_una`, а отправлять надо от `snd_nxt`.
        let mut payload = [0u8; tcp::MSS];
        let offset = conn.in_flight() as usize;
        for (at, byte) in conn.send.iter().skip(offset).take(take).enumerate() {
            payload[at] = *byte;
        }

        let out_seq = conn.snd_nxt;
        let out_ack = conn.rcv_nxt;
        let window = conn.window();
        let local_port = conn.local_port;
        let remote = conn.remote;
        let remote_port = conn.remote_port;

        let out = tcp::Outgoing {
            source_port: local_port,
            destination_port: remote_port,
            sequence: out_seq,
            acknowledgement: out_ack,
            flags,
            window,
            with_mss,
            payload: &payload[..take],
        };
        let mut buffer = [0u8; tcp::HEADER + 4 + tcp::MSS];
        let source = iface.address;
        let len = tcp::write(&mut buffer, source, remote, &out);

        match send_ipv4(iface, remote, ipv4::PROTOCOL_TCP, &buffer[..len]) {
            Ok(()) => {}
            Err(NetError::Pending) => {
                // Адрес соседа ещё выясняется. Ничего не потеряно: таймер
                // повторной передачи пришлёт нас сюда снова.
                if let Some(conn) = iface.streams.at(index) {
                    conn.arm(now);
                    if conn.retransmit_at == 0 {
                        conn.retransmit_at = now + 50;
                    }
                }
                return;
            }
            Err(_) => return,
        }
        iface.counters.tcp_out += 1;

        let Some(conn) = iface.streams.at(index) else { return };
        conn.need_ack = false;
        let mut span = take as u32;
        if flags & tcp::SYN != 0 || fin {
            span += 1;
        }
        conn.snd_nxt = conn.snd_nxt.wrapping_add(span);
        if fin {
            conn.fin_sent = true;
            conn.state = match conn.state {
                stream::State::CloseWait => stream::State::LastAck,
                stream::State::Established => stream::State::FinWait1,
                state => state,
            };
        }
        if span > 0 {
            conn.arm(now);
        }
        // Больше отправлять нечего — выходим; иначе цикл продолжит выгребать
        // очередь по сегменту за виток.
        if span == 0 {
            return;
        }
    }
}

/// Тик таймеров: повторная передача и `TIME_WAIT`.
fn tcp_tick(iface: &mut Interface) {
    let now = crate::time::uptime_ms();
    let indices: alloc::vec::Vec<usize> = iface.streams.indices().collect();

    for index in indices {
        if iface.streams.expired(index, now) {
            iface.streams.close(index);
            continue;
        }

        let mut retransmit = false;
        let mut dead = false;
        // Повтор ли это на самом деле. Сегмент, который **не ушёл** (адрес
        // соседа выяснялся, и отправка вернула `Pending`), приходит сюда по
        // тому же таймеру, но повторной передачей не является: первая попытка
        // просто состоится сейчас. Считать её повтором значит завести счётчик,
        // который на исправной сети никогда не ноль, — то есть бесполезный.
        let mut counted = false;
        if let Some(conn) = iface.streams.at(index) {
            if conn.timed_out(now) {
                if conn.gave_up() {
                    dead = true;
                } else {
                    counted = conn.snd_nxt != conn.snd_una;
                    conn.back_off(now);
                    // Go-Back-N: всё неподтверждённое отправляется заново с
                    // головы окна. Выборочного повтора у нас нет, потому что
                    // нет и выборочных подтверждений.
                    conn.snd_nxt = conn.snd_una;
                    conn.fin_sent = false;
                    retransmit = true;
                }
            }
        }

        if dead {
            // Собеседник молчит столько, сколько мы согласны ждать. Соединение
            // объявляется оборванным, а не остаётся висеть: программа должна
            // получить отказ, а не ждать вечно.
            if let Some(conn) = iface.streams.at(index) {
                conn.reset = true;
                conn.state = stream::State::Closed;
            }
            iface.counters.tcp_timeouts += 1;
            continue;
        }
        if retransmit {
            if counted {
                iface.counters.tcp_retransmits += 1;
            }
            tcp_pump(iface, index);
        }
    }
}

/// Отправить пакет IPv4.
///
/// Возвращает [`NetError::Pending`], если аппаратный адрес получателя ещё
/// неизвестен: запрос ARP при этом уже отправлен, и повторить попытку имеет
/// смысл через несколько миллисекунд. Придерживать пакет до ответа мы не
/// будем — см. заголовок [`arp`].
fn send_ipv4(
    iface: &mut Interface,
    destination: Ipv4,
    protocol: u8,
    payload: &[u8],
) -> Result<(), NetError> {
    // Отправлять, не имея адреса, вообще-то нельзя: ответ пришёл бы на
    // `0.0.0.0`. Ровно одно исключение — широковещательный пакет, и оно не
    // придумано для удобства: клиент DHCP обязан спросить адрес **до** того, как
    // адрес у него появится, и делает это с `0.0.0.0` на `255.255.255.255`. Так
    // написано в RFC 2131, и без этого исключения аренду взять невозможно.
    if iface.address.is_unspecified() && !destination.is_broadcast() {
        return Err(NetError::NoAddress);
    }

    // Куда отдавать кадр: получателю напрямую, если он в нашей сети, и шлюзу
    // во всех остальных случаях. Это и есть вся таблица маршрутов, какая
    // сегодня существует.
    let next_hop = if destination.same_network(iface.address, iface.netmask)
        || destination.is_broadcast()
    {
        destination
    } else if !iface.gateway.is_unspecified() {
        iface.gateway
    } else {
        return Err(NetError::NoRoute);
    };

    let target_mac = if next_hop.is_broadcast()
        || (!iface.netmask.is_unspecified()
            && next_hop == iface.address.network_broadcast(iface.netmask))
    {
        eth::BROADCAST
    } else {
        let now = crate::time::uptime_ms();
        match iface.table.lookup(next_hop, now) {
            Some(mac) => mac,
            None => {
                request_arp(iface, next_hop);
                return Err(NetError::Pending);
            }
        }
    };

    let total = eth::HEADER + ipv4::HEADER + payload.len();
    if total > FRAME_MAX {
        return Err(NetError::Device(VirtioError::TooLong(total)));
    }

    let id = iface.next_id;
    iface.next_id = iface.next_id.wrapping_add(1);

    let mut frame = [0u8; FRAME_MAX];
    eth::write_header(&mut frame, target_mac, iface.mac, eth::TYPE_IPV4);
    ipv4::write_header(
        &mut frame[eth::HEADER..],
        iface.address,
        destination,
        protocol,
        payload.len(),
        id,
    );
    frame[eth::HEADER + ipv4::HEADER..total].copy_from_slice(payload);

    iface.device.send(&frame[..total]).map_err(NetError::Device)
}

/// Спросить, у кого этот адрес.
fn request_arp(iface: &mut Interface, target: Ipv4) {
    let mut buffer = [0u8; eth::HEADER + arp::PACKET];
    eth::write_header(&mut buffer, eth::BROADCAST, iface.mac, eth::TYPE_ARP);
    arp::write(
        &mut buffer[eth::HEADER..],
        arp::REQUEST,
        iface.mac,
        iface.address,
        // Кого ищем — тот и неизвестен: поле аппаратного адреса получателя в
        // запросе заполняется нулями.
        [0u8; 6],
        target,
    );
    if iface.device.send(&buffer).is_ok() {
        iface.counters.arp_requests_out += 1;
    }
}

/// Спросить у сервера имён адрес.
///
/// Работает поверх тех же сокетов, что и программы, и от имени той задачи,
/// которая спрашивает: отдельного пути «для ядра» здесь нет намеренно — сокет,
/// который никому не принадлежит, некому закрыть, если спрашивающего снимут
/// посреди ожидания.
pub fn resolve(name: &str, timeout_ms: u64) -> Result<Ipv4, NetError> {
    /// Сколько раз переспросить, если ответа нет. UDP не обещает доставки, и
    /// один потерянный запрос не повод объявлять имя несуществующим.
    const ATTEMPTS: u32 = 3;

    // Имя, записанное адресом, не спрашивают: `ping 10.0.2.2` не должен зависеть
    // от того, поднялся ли сервер имён.
    if let Some(address) = Ipv4::parse(name) {
        return Ok(address);
    }
    if name.is_empty() || name.len() > dns::MAX_NAME {
        return Err(NetError::BadName);
    }

    let server = dns_server().ok_or(NetError::NoAddress)?;
    let owner = crate::sched::current();
    let index = socket_open(owner)?;

    // Дальше — только через `finish`: сокет обязан закрыться на любом пути,
    // включая отказ, иначе эфемерный порт останется занятым до конца работы
    // задачи.
    let result = resolve_with(owner, index, server, name, timeout_ms, ATTEMPTS);
    let _ = socket_close(owner, index);
    result
}

fn resolve_with(
    owner: TaskId,
    index: usize,
    server: Ipv4,
    name: &str,
    timeout_ms: u64,
    attempts: u32,
) -> Result<Ipv4, NetError> {
    socket_bind(owner, index, 0)?;
    socket_connect(owner, index, server, dns::PORT)?;

    // Идентификатор запроса берётся из часов: случайности в системе пока нет, а
    // постоянный идентификатор означал бы, что ответ на прошлый вопрос сходит за
    // ответ на новый.
    let id = (crate::time::uptime_ms() as u16) | 1;

    let mut query = [0u8; dns::MAX_NAME + 32];
    let len = dns::write_query(&mut query, id, name).ok_or(NetError::BadName)?;

    for _ in 0..attempts {
        // Отправка может не удаться, пока выясняется адрес шлюза.
        let deadline = crate::time::uptime_ms() + timeout_ms;
        loop {
            match socket_send(owner, index, &query[..len]) {
                Ok(_) => break,
                Err(NetError::Pending) if crate::time::uptime_ms() < deadline => {
                    crate::sched::sleep_ms(POLL_INTERVAL_MS);
                }
                Err(err) => return Err(err),
            }
        }

        while crate::time::uptime_ms() < deadline {
            if let Some(answer) = socket_recv(owner, index)? {
                // Ответ не от того, кого спрашивали, не считается — на
                // эфемерный порт может прийти что угодно.
                if answer.from == server && answer.port == dns::PORT {
                    if let Some(address) = dns::parse_answer(&answer.data, id) {
                        return Ok(address);
                    }
                    // Ответ разобран и говорит «нет такого имени» — переспрашивать
                    // бессмысленно, это ответ, а не потеря.
                    return Err(NetError::NoSuchName);
                }
            }
            crate::sched::sleep_ms(2);
        }
    }
    Err(NetError::Timeout)
}

/// Отправить эхо-запрос и дождаться ответа.
///
/// Возвращает время в миллисекундах, за которое ответ вернулся. Лок берётся
/// только на сами операции с картой: ожидание идёт со сном, а спать под
/// [`SpinLock`] нельзя — он держит прерывания запрещёнными.
pub fn ping(target: Ipv4, sequence: u16, timeout_ms: u64) -> Result<u64, NetError> {
    /// Данные эхо-запроса: их отвечающий обязан вернуть без изменений.
    const PAYLOAD: &[u8] = b"FreeOS says hello, please send it back";

    let started = crate::time::uptime_ms();
    let deadline = started + timeout_ms;

    // Отправка может не удаться с первого раза: аппаратный адрес получателя
    // сначала надо спросить, а ответ придёт не мгновенно.
    loop {
        let attempt = {
            let mut guard = INTERFACE.lock();
            let iface = guard.as_mut().ok_or(NetError::NoDevice)?;
            let mut message = [0u8; icmp::HEADER + PAYLOAD.len()];
            let len = icmp::write(&mut message, icmp::ECHO_REQUEST, ECHO_ID, sequence, PAYLOAD);
            // Прошлый ответ забывается перед отправкой, а не после: иначе ответ
            // на предыдущий запрос сошёл бы за ответ на этот.
            iface.last_reply = None;
            let result = send_ipv4(iface, target, ipv4::PROTOCOL_ICMP, &message[..len]);
            if result.is_ok() {
                iface.counters.echo_requests_out += 1;
            }
            result
        };
        match attempt {
            Ok(()) => break,
            Err(NetError::Pending) => {
                if crate::time::uptime_ms() >= deadline {
                    return Err(NetError::Timeout);
                }
                crate::sched::sleep_ms(POLL_INTERVAL_MS);
            }
            Err(err) => return Err(err),
        }
    }

    let sent_at = crate::time::uptime_ms();
    loop {
        {
            let guard = INTERFACE.lock();
            let iface = guard.as_ref().ok_or(NetError::NoDevice)?;
            if let Some(reply) = iface.last_reply {
                // Сверяются все три поля: чужой ответ, пришедший в это же
                // окно, не должен засчитаться за наш.
                if reply.id == ECHO_ID && reply.sequence == sequence && reply.from == target {
                    return Ok(reply.at_ms.saturating_sub(sent_at));
                }
            }
        }
        if crate::time::uptime_ms() >= deadline {
            return Err(NetError::Timeout);
        }
        crate::sched::sleep_ms(1);
    }
}
