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
pub mod eth;
pub mod icmp;
pub mod ipv4;

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
    counters: Counters,
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
        counters: Counters::default(),
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
    pub link: Stats,
    pub counters: Counters,
}

pub fn status() -> Option<Status> {
    let guard = INTERFACE.lock();
    let iface = guard.as_ref()?;
    Some(Status {
        mac: iface.mac,
        address: iface.address,
        netmask: iface.netmask,
        gateway: iface.gateway,
        link: iface.device.stats(),
        counters: iface.counters,
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
        arp::REPLY => iface.counters.arp_replies_in += 1,
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
            && packet.destination == iface.address.network_broadcast(iface.netmask));
    if !ours {
        iface.counters.ignored += 1;
        return;
    }

    if packet.protocol != ipv4::PROTOCOL_ICMP {
        // UDP и TCP появятся в следующих фазах. Пока честнее сосчитать пакет
        // как непонятый, чем промолчать.
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
    if iface.address.is_unspecified() {
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
