//! Соединения TCP: состояние, буферы и то, что решает автомат.
//!
//! Отправкой здесь никто не занимается — за неё отвечает [`super`], у которого
//! есть карта. Разделение не косметическое: автомат состояний это чистая
//! логика, и её можно читать, не держа в голове ни очередей virtio, ни
//! заимствований.
//!
//! # Что реализовано, а что нет
//!
//! Реализовано то, без чего связь неправильна: полный набор состояний,
//! установление и закрытие с обеих сторон, накопительное подтверждение,
//! ограничение окном собеседника, повторная передача по таймеру и `TIME_WAIT`.
//!
//! **Не** реализовано и названо здесь, а не спрятано:
//!
//! * **Сегменты не по порядку отбрасываются.** Пришедший раньше своего времени
//!   сегмент не откладывается, а выбрасывается, и собеседник пришлёт его снова.
//!   Это законно (TCP обязан переживать потери) и стоит пропускной способности
//!   на сети с перестановкой пакетов. Очередь пересборки — заметный кусок кода
//!   и заметный кусок памяти на соединение, и заводить его до того, как
//!   появится, чем измерить выигрыш, значит писать вслепую.
//! * **Нет управления перегрузкой** — ни медленного старта, ни `cwnd`.
//!   Ограничение одно: окно, которое объявил собеседник. В локальной сети и в
//!   эмуляторе это то же самое; на настоящем интернет-канале это означает, что
//!   мы не умеем притормаживать, и честно сказать об этом важнее, чем сделать
//!   вид, что умеем.
//! * **Нет выборочных подтверждений и масштаба окна.** Мы их не объявляем,
//!   поэтому и получить не должны.
//!
//! # Почему `TIME_WAIT` короткий
//!
//! Стандарт велит ждать две максимальные жизни сегмента — на практике это
//! десятки секунд. Смысл ожидания в том, чтобы запоздавший сегмент старого
//! соединения не попал в новое с той же парой портов. Здесь стоит две секунды,
//! и это осознанная замена: соединений у нас единицы, порты эфемерные выдаются
//! по кругу из шестнадцати тысяч, а держать слот занятым минуту при восьми
//! слотах означало бы, что три подряд закрытых соединения исчерпывают систему.

use alloc::collections::VecDeque;

use crate::net::ipv4::Ipv4;
use crate::net::tcp;
use crate::sched::TaskId;

/// Сколько соединений живёт одновременно, считая слушающие.
pub const MAX_STREAMS: usize = 8;

/// Сколько байт помещается в приёмный буфер соединения.
///
/// Это же значение объявляется собеседнику как окно: обещать больше, чем можем
/// сложить, — верный способ потерять данные, которые нам разрешили прислать.
pub const RECEIVE_BUFFER: usize = 8192;

/// Сколько байт программа может отдать на отправку, не дожидаясь подтверждений.
pub const SEND_BUFFER: usize = 8192;

/// Сколько ждать до первой повторной передачи.
const RTO_MS: u64 = 300;

/// Предел, до которого растёт задержка между повторами.
const RTO_MAX_MS: u64 = 4_000;

/// Сколько раз повторять, прежде чем признать соединение мёртвым.
const MAX_RETRIES: u32 = 6;

/// Сколько держать соединение в `TIME_WAIT`.
const TIME_WAIT_MS: u64 = 2_000;

/// Сколько соединений ждут в очереди слушающего сокета.
const BACKLOG: usize = 4;

/// Состояние соединения — те самые одиннадцать из RFC 793.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    /// Мы закрылись первыми, наш `FIN` ещё не подтверждён.
    FinWait1,
    /// Наш `FIN` подтверждён, ждём чужой.
    FinWait2,
    /// Оба закрылись одновременно.
    Closing,
    /// Всё кончилось, ждём, пока в сети умрут запоздавшие сегменты.
    TimeWait,
    /// Закрылся собеседник; мы ещё можем отправлять.
    CloseWait,
    /// Мы ответили своим `FIN`, ждём подтверждения.
    LastAck,
}

impl State {
    /// Можно ли программе читать и писать в это соединение.
    pub fn is_open(self) -> bool {
        matches!(self, State::Established | State::CloseWait)
    }

    pub fn name(self) -> &'static str {
        match self {
            State::Closed => "closed",
            State::Listen => "listen",
            State::SynSent => "syn-sent",
            State::SynReceived => "syn-received",
            State::Established => "established",
            State::FinWait1 => "fin-wait-1",
            State::FinWait2 => "fin-wait-2",
            State::Closing => "closing",
            State::TimeWait => "time-wait",
            State::CloseWait => "close-wait",
            State::LastAck => "last-ack",
        }
    }
}

/// Одно соединение или один слушающий сокет.
pub struct Connection {
    pub owner: TaskId,
    pub state: State,
    pub local_port: u16,
    pub remote: Ipv4,
    pub remote_port: u16,

    /// Самый ранний неподтверждённый байт.
    pub snd_una: u32,
    /// Номер, с которого пойдут следующие отправленные данные.
    pub snd_nxt: u32,
    /// Сколько собеседник готов принять сверх `snd_una`.
    pub snd_wnd: u16,
    /// Наибольший сегмент, который согласен принять собеседник.
    pub peer_mss: u16,

    /// Номер, который мы ждём следующим.
    pub rcv_nxt: u32,

    /// Байты, отданные программой: от `snd_una` и дальше.
    ///
    /// Подтверждённое снимается с головы, поэтому очередь и есть окно
    /// повторной передачи: всё, что в ней лежит, может понадобиться послать
    /// заново.
    pub send: VecDeque<u8>,
    /// Принятое, чего программа ещё не забрала.
    pub recv: VecDeque<u8>,

    /// Когда повторить неподтверждённое; ноль — таймер не заведён.
    pub retransmit_at: u64,
    pub rto_ms: u64,
    pub retries: u32,
    /// Когда закончится `TIME_WAIT`.
    pub expires_at: u64,

    /// Надо отправить подтверждение — данными или отдельным сегментом.
    pub need_ack: bool,
    /// Программа попросила закрыть отправку; `FIN` уйдёт, когда кончатся данные.
    pub closing: bool,
    /// Наш `FIN` уже в пути (и занимает один номер).
    pub fin_sent: bool,
    /// Собеседник прислал `FIN`: читать больше нечего.
    pub peer_closed: bool,
    /// Соединение оборвано — `RST` или исчерпанные повторы.
    pub reset: bool,

    /// Готовые соединения слушающего сокета.
    pub backlog: VecDeque<usize>,
    /// Для принятого соединения — слот слушающего, который его породил.
    pub listener: Option<usize>,
}

impl Connection {
    fn new(owner: TaskId, state: State, local_port: u16) -> Self {
        Self {
            owner,
            state,
            local_port,
            remote: Ipv4::UNSPECIFIED,
            remote_port: 0,
            snd_una: 0,
            snd_nxt: 0,
            snd_wnd: 0,
            peer_mss: tcp::MSS as u16,
            rcv_nxt: 0,
            send: VecDeque::new(),
            recv: VecDeque::new(),
            retransmit_at: 0,
            rto_ms: RTO_MS,
            retries: 0,
            expires_at: 0,
            need_ack: false,
            closing: false,
            fin_sent: false,
            peer_closed: false,
            reset: false,
            backlog: VecDeque::new(),
            listener: None,
        }
    }

    /// Сколько мы готовы принять — оно же объявляемое окно.
    pub fn window(&self) -> u16 {
        (RECEIVE_BUFFER - self.recv.len().min(RECEIVE_BUFFER)) as u16
    }

    /// Сколько байт отправлено, но не подтверждено.
    pub fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    /// Сколько байт можно отправить прямо сейчас.
    ///
    /// Ограничений три, и меньшее из них побеждает: сколько лежит в буфере,
    /// сколько разрешил собеседник своим окном и сколько влезает в сегмент.
    pub fn sendable(&self) -> usize {
        if !matches!(self.state, State::Established | State::CloseWait) {
            return 0;
        }
        let unsent = self.send.len().saturating_sub(self.in_flight() as usize);
        let window = usize::from(self.snd_wnd).saturating_sub(self.in_flight() as usize);
        unsent.min(window).min(usize::from(self.peer_mss).min(tcp::MSS))
    }

    /// Снять подтверждённое с головы очереди отправки.
    pub fn acknowledge(&mut self, ack: u32) {
        if !tcp::after(ack, self.snd_una) {
            return;
        }
        let mut acked = ack.wrapping_sub(self.snd_una) as usize;
        // `FIN` занимает номер, но байтом в очереди не лежит: подтверждение
        // нашего `FIN` не должно снимать с очереди чужой байт.
        if self.fin_sent && ack == self.snd_nxt && acked > 0 {
            acked = acked.saturating_sub(1);
        }
        for _ in 0..acked.min(self.send.len()) {
            self.send.pop_front();
        }
        self.snd_una = ack;
        self.retries = 0;
        self.rto_ms = RTO_MS;
        // Таймер повторной передачи заводится заново, только если ещё есть что
        // подтверждать. Таймер на пустую очередь — это повтор в никуда.
        self.retransmit_at = 0;
    }

    /// Положить принятые байты, если они те самые, которых мы ждём.
    ///
    /// Возвращает `true`, если данные приняты. Сегмент не по порядку
    /// отбрасывается — см. заголовок модуля.
    pub fn accept_data(&mut self, sequence: u32, data: &[u8]) -> bool {
        if data.is_empty() {
            return true;
        }
        if sequence != self.rcv_nxt {
            // Подтверждение всё равно отправим: собеседник узнает, чего мы
            // ждём, и пришлёт именно это.
            self.need_ack = true;
            return false;
        }
        let room = RECEIVE_BUFFER - self.recv.len().min(RECEIVE_BUFFER);
        let taken = data.len().min(room);
        self.recv.extend(&data[..taken]);
        self.rcv_nxt = self.rcv_nxt.wrapping_add(taken as u32);
        self.need_ack = true;
        taken == data.len()
    }

    /// Завести таймер повторной передачи, если он ещё не заведён.
    pub fn arm(&mut self, now: u64) {
        if self.retransmit_at == 0 && self.in_flight() > 0 {
            self.retransmit_at = now + self.rto_ms;
        }
    }

    /// Снять таймер повтора: сторожить нечего.
    ///
    /// Нужен там, где таймер остался заведённым, а неподтверждённых байт уже не
    /// осталось. Так бывает у **скачивающего**: он отправил запрос, тот был
    /// подтверждён вместе с первым же куском ответа, и дальше сторону-получателя
    /// никто ни о чём не спрашивает. Таймер при этом продолжает срабатывать, и
    /// каждое срабатывание увеличивало счётчик неудач, пока соединение не
    /// объявлялось мёртвым — на исправной связи, посреди работающей загрузки.
    pub fn disarm(&mut self) {
        self.retransmit_at = 0;
        self.rto_ms = RTO_MS;
        self.retries = 0;
    }

    /// Пора ли повторять.
    pub fn timed_out(&self, now: u64) -> bool {
        self.retransmit_at != 0 && now >= self.retransmit_at
    }

    /// Отложить следующую попытку, удвоив задержку.
    ///
    /// Удвоение обязательно: повтор с постоянным периодом в сеть, которая и так
    /// не справляется, — это не настойчивость, а вклад в затор.
    pub fn back_off(&mut self, now: u64) {
        self.retries += 1;
        self.rto_ms = (self.rto_ms * 2).min(RTO_MAX_MS);
        self.retransmit_at = now + self.rto_ms;
    }

    pub fn gave_up(&self) -> bool {
        self.retries >= MAX_RETRIES
    }
}

/// Все соединения системы.
pub struct Streams {
    slots: [Option<Connection>; MAX_STREAMS],
}

/// Почему не вышло.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamError {
    TooMany,
    BadStream,
    PortTaken(u16),
    /// Соединение ещё не установлено.
    NotConnected,
    /// Соединение оборвано собеседником или потеряно.
    Reset,
    /// Сокет не слушает, а `accept` спрашивают у него.
    NotListening,
    /// Отправлять нечего и некуда: связь закрыта с нашей стороны.
    Closed,
    /// Буфер отправки полон.
    WouldBlock,
}

impl core::fmt::Display for StreamError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooMany => write!(f, "all {MAX_STREAMS} connections are in use"),
            Self::BadStream => f.write_str("no such connection"),
            Self::PortTaken(port) => write!(f, "port {port} is already taken"),
            Self::NotConnected => f.write_str("the connection is not established yet"),
            Self::Reset => f.write_str("the connection was reset"),
            Self::NotListening => f.write_str("this socket does not listen"),
            Self::Closed => f.write_str("the connection is closed for sending"),
            Self::WouldBlock => f.write_str("the send buffer is full"),
        }
    }
}

impl Streams {
    pub const fn new() -> Self {
        Self { slots: [const { None }; MAX_STREAMS] }
    }

    pub fn open(&mut self, owner: TaskId) -> Result<usize, StreamError> {
        let index = self.free_slot().ok_or(StreamError::TooMany)?;
        self.slots[index] = Some(Connection::new(owner, State::Closed, 0));
        Ok(index)
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(Option::is_none)
    }

    pub fn get(&self, owner: TaskId, index: usize) -> Result<&Connection, StreamError> {
        self.slots
            .get(index)
            .and_then(Option::as_ref)
            .filter(|conn| conn.owner == owner)
            .ok_or(StreamError::BadStream)
    }

    pub fn get_mut(&mut self, owner: TaskId, index: usize) -> Result<&mut Connection, StreamError> {
        self.slots
            .get_mut(index)
            .and_then(Option::as_mut)
            .filter(|conn| conn.owner == owner)
            .ok_or(StreamError::BadStream)
    }

    /// Доступ без проверки владельца — для внутренностей стека.
    pub fn at(&mut self, index: usize) -> Option<&mut Connection> {
        self.slots.get_mut(index).and_then(Option::as_mut)
    }

    /// То же, но только на чтение.
    pub fn peek(&self, index: usize) -> Option<&Connection> {
        self.slots.get(index).and_then(Option::as_ref)
    }

    pub fn exists(&self, index: usize) -> bool {
        self.slots.get(index).is_some_and(Option::is_some)
    }

    pub fn bind(&mut self, owner: TaskId, index: usize, port: u16) -> Result<u16, StreamError> {
        if self.slots.iter().enumerate().any(|(other, slot)| {
            other != index
                && slot
                    .as_ref()
                    .is_some_and(|conn| conn.local_port == port && conn.state == State::Listen)
        }) {
            return Err(StreamError::PortTaken(port));
        }
        let conn = self.get_mut(owner, index)?;
        conn.local_port = port;
        Ok(port)
    }

    pub fn listen(&mut self, owner: TaskId, index: usize) -> Result<(), StreamError> {
        let conn = self.get_mut(owner, index)?;
        if conn.local_port == 0 {
            // Слушать на порту, который назначили случайно, бессмысленно: никто
            // не знает, куда стучаться.
            return Err(StreamError::NotConnected);
        }
        conn.state = State::Listen;
        Ok(())
    }

    /// Забрать готовое соединение из очереди слушающего.
    pub fn accept(&mut self, owner: TaskId, index: usize) -> Result<Option<usize>, StreamError> {
        let conn = self.get_mut(owner, index)?;
        if conn.state != State::Listen {
            return Err(StreamError::NotListening);
        }
        Ok(conn.backlog.pop_front())
    }

    /// Найти соединение по четвёрке адресов.
    ///
    /// Сначала точное совпадение, и только потом слушающий сокет: иначе новый
    /// `SYN` от того же собеседника отдавался бы слушателю, у которого уже есть
    /// это соединение.
    pub fn lookup(
        &self,
        local_port: u16,
        remote: Ipv4,
        remote_port: u16,
    ) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.as_ref().is_some_and(|conn| {
                conn.state != State::Listen
                    && conn.local_port == local_port
                    && conn.remote == remote
                    && conn.remote_port == remote_port
            })
        })
    }

    pub fn listener_for(&self, local_port: u16) -> Option<usize> {
        self.slots.iter().position(|slot| {
            slot.as_ref()
                .is_some_and(|conn| conn.state == State::Listen && conn.local_port == local_port)
        })
    }

    /// Завести соединение, рождённое пришедшим `SYN`.
    pub fn accept_syn(
        &mut self,
        listener: usize,
        remote: Ipv4,
        remote_port: u16,
        sequence: u32,
        window: u16,
        mss: Option<u16>,
        iss: u32,
    ) -> Option<usize> {
        let (owner, local_port, waiting) = {
            let conn = self.slots.get(listener)?.as_ref()?;
            (conn.owner, conn.local_port, conn.backlog.len())
        };
        // Очередь ожидающих не безразмерна: соединение, которое никто не
        // забирает, — это память и слот, а слотов восемь на всю систему.
        if waiting >= BACKLOG {
            return None;
        }
        let index = self.free_slot()?;

        let mut conn = Connection::new(owner, State::SynReceived, local_port);
        conn.remote = remote;
        conn.remote_port = remote_port;
        conn.rcv_nxt = sequence.wrapping_add(1);
        conn.snd_una = iss;
        conn.snd_nxt = iss;
        conn.snd_wnd = window;
        if let Some(mss) = mss {
            conn.peer_mss = mss.clamp(536, tcp::MSS as u16);
        }
        conn.listener = Some(listener);
        conn.need_ack = true;
        self.slots[index] = Some(conn);
        Some(index)
    }

    /// Сообщить слушателю, что соединение готово.
    pub fn hand_to_listener(&mut self, index: usize) {
        let listener = self.slots.get(index).and_then(Option::as_ref).and_then(|conn| conn.listener);
        let Some(listener) = listener else { return };
        if let Some(Some(conn)) = self.slots.get_mut(listener) {
            conn.backlog.push_back(index);
        }
    }

    pub fn close(&mut self, index: usize) {
        // Соединение уходит вместе со своим местом в очереди слушателя:
        // индекс, оставшийся в чужой очереди, при следующем `accept` указал бы
        // на слот, где уже живёт кто-то другой.
        let listener = self.slots.get(index).and_then(Option::as_ref).and_then(|conn| conn.listener);
        if let Some(listener) = listener {
            if let Some(Some(conn)) = self.slots.get_mut(listener) {
                conn.backlog.retain(|waiting| *waiting != index);
            }
        }
        if let Some(slot) = self.slots.get_mut(index) {
            *slot = None;
        }
    }

    /// Закрыть всё, что принадлежит задаче. Возвращает, сколько закрыто.
    pub fn close_owner(&mut self, owner: TaskId) -> usize {
        let mut closed = 0;
        for index in 0..MAX_STREAMS {
            if self.slots[index].as_ref().is_some_and(|conn| conn.owner == owner) {
                self.close(index);
                closed += 1;
            }
        }
        closed
    }

    /// Сколько соединений открыто, и сводка по состояниям — для `ip`.
    pub fn summary(&self) -> (usize, usize) {
        let total = self.slots.iter().flatten().count();
        let established = self
            .slots
            .iter()
            .flatten()
            .filter(|conn| conn.state == State::Established)
            .count();
        (total, established)
    }

    /// Перебрать соединения для команды оболочки.
    pub fn describe(&self) -> impl Iterator<Item = (usize, State, u16, Ipv4, u16, usize, usize)> + '_ {
        self.slots.iter().enumerate().filter_map(|(index, slot)| {
            let conn = slot.as_ref()?;
            Some((
                index,
                conn.state,
                conn.local_port,
                conn.remote,
                conn.remote_port,
                conn.send.len(),
                conn.recv.len(),
            ))
        })
    }

    /// Сколько времени осталось до ближайшего срабатывания таймера.
    pub fn indices(&self) -> impl Iterator<Item = usize> + '_ {
        (0..MAX_STREAMS).filter(|index| self.slots[*index].is_some())
    }

    /// Истёк ли `TIME_WAIT`.
    pub fn expired(&self, index: usize, now: u64) -> bool {
        self.slots[index]
            .as_ref()
            .is_some_and(|conn| conn.state == State::TimeWait && now >= conn.expires_at)
    }
}

/// Начальный номер последовательности.
///
/// Берётся из часов, а не из нуля и не из константы. Постоянный начальный номер
/// означает, что запоздавший сегмент прошлого соединения с той же парой портов
/// попадает в новое и выглядит там законным. Часы — не источник случайности и
/// от подбора не защищают; защищает от подбора то, что этой системы нет в
/// интернете без посредника, а вот от собственного эха защищают именно они.
pub fn initial_sequence(now_ms: u64) -> u32 {
    // Классическая формула: счётчик, растущий примерно на 250 тысяч в секунду.
    (now_ms.wrapping_mul(250_000) & 0xFFFF_FFFF) as u32
}

/// Через сколько истекает `TIME_WAIT`.
pub fn time_wait_until(now: u64) -> u64 {
    now + TIME_WAIT_MS
}
