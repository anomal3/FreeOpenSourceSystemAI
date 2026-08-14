//! `dhcp` — клиент DHCP. Первая настоящая служба в системе.
//!
//! # Почему это программа, а не часть ядра
//!
//! Потому что она может сломаться, и когда она сломается, система обязана
//! остаться. Аренда адреса — это разбор чужого пакета с двадцатью
//! необязательными полями, ожидание ответа, который может не прийти, и таймеры
//! на часы вперёд. Всё это живёт в третьем кольце под присмотром супервизора
//! (фаза 33): упала — поднимут, падает по кругу — остановят и скажут.
//!
//! # Что здесь делается и в каком порядке
//!
//! Четыре шага, названные в RFC 2131 и придуманные не нами:
//!
//! ```text
//!   клиент  --DISCOVER-->  всем       «есть кто раздающий?»
//!   сервер  --OFFER----->  клиенту    «предлагаю 10.0.2.15»
//!   клиент  --REQUEST--->  всем       «беру 10.0.2.15 у этого сервера»
//!   сервер  --ACK------->  клиенту    «твой, вот маска, шлюз и имена»
//! ```
//!
//! Третий шаг широковещательный, хотя адрес сервера уже известен, и это не
//! оплошность: так остальные серверы в сети узнают, что их предложения
//! отклонены, и не держат адреса зарезервированными.
//!
//! # Про флаг «отвечайте всем»
//!
//! Мы его выставляем. Свой адрес клиент узнаёт только из ответа, то есть до
//! ответа отвечать ему на этот адрес некуда: карта отбросит кадр, адресованный
//! адресу, которого у неё ещё нет. Флаг просит сервер отвечать
//! широковещательно, и именно так делают все клиенты, у которых нет отдельного
//! пути для «сырых» кадров.

#![no_std]
#![no_main]

use user_progs::{
    NetConfig, NetInfo, bind, close_socket, connect, error, error_num, exit, netconf, netinfo,
    recv_waiting, send_waiting, sleep_ms, socket, uptime_ms,
};

/// Порт клиента.
const PORT_CLIENT: u16 = 68;
/// Порт сервера.
const PORT_SERVER: u16 = 67;
/// Куда идут DISCOVER и REQUEST: адреса сервера мы ещё не знаем, а свой адрес
/// нам ещё не дали.
const BROADCAST: u32 = 0xFFFF_FFFF;

/// Длина неизменной части пакета BOOTP, до опций.
const FIXED: usize = 236;
/// Метка, с которой начинаются опции: «дальше DHCP, а не голый BOOTP».
const COOKIE: [u8; 4] = [99, 130, 83, 99];

/// Наибольший пакет, который мы согласны собрать или разобрать.
///
/// 576 байт — это минимум, который обязан принять любой узел IPv4, и ровно
/// столько отводит под свои пакеты сам DHCP. Ответ длиннее означал бы сервер,
/// который прислал больше опций, чем предусмотрено протоколом.
const PACKET: usize = 576;

/// Опция: тип сообщения.
const OPT_MESSAGE_TYPE: u8 = 53;
/// Опция: маска подсети.
const OPT_SUBNET_MASK: u8 = 1;
/// Опция: шлюз.
const OPT_ROUTER: u8 = 3;
/// Опция: серверы имён.
const OPT_DNS: u8 = 6;
/// Опция: запрашиваемый адрес.
const OPT_REQUESTED_IP: u8 = 50;
/// Опция: срок аренды в секундах.
const OPT_LEASE_TIME: u8 = 51;
/// Опция: какой сервер отвечает.
const OPT_SERVER_ID: u8 = 54;
/// Опция: список того, что клиент хочет узнать.
const OPT_PARAMETER_LIST: u8 = 55;
/// Конец списка опций.
const OPT_END: u8 = 255;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

/// Сколько ждать ответа на каждый шаг.
const REPLY_TIMEOUT_MS: u64 = 4_000;

/// Сколько раз повторить шаг, прежде чем начать сначала.
const ATTEMPTS: u32 = 3;

/// Сколько ждать перед новой попыткой, если сервер не отозвался вовсе.
///
/// Десять секунд, а не полсекунды: сети может не быть вовсе, и служба, которая
/// в этом случае шлёт широковещательные пакеты без остановки, — это не
/// настойчивость, а помеха для всех остальных в подсети.
const RETRY_MS: u64 = 10_000;

/// Аренда обновляется на половине срока — так велит RFC 2131, и причина
/// практическая: если сервер к тому моменту не ответит, останется ещё столько
/// же времени на попытки, прежде чем адрес перестанет быть нашим.
fn renew_after(lease_seconds: u32) -> u64 {
    let half = u64::from(lease_seconds) * 1000 / 2;
    // Аренда на пять секунд — это либо опечатка сервера, либо сеть, которой
    // лучше не доверять. Минута снизу защищает от превращения службы в
    // источник пакетов.
    half.max(60_000)
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    let mut info = NetInfo::default();
    if netinfo(&mut info) != 0 || info.present == 0 {
        // Карты нет — это не поломка службы, а свойство машины. Сказать и уйти
        // с нулём: супервизор не должен считать это падением и поднимать нас по
        // кругу.
        error("dhcp: no network card, nothing to configure\n");
        exit(0);
    }

    error("dhcp: asking on ");
    print_mac(info.mac);
    error("\n");

    loop {
        match lease(&info) {
            Some(lease) => {
                apply(&lease);
                sleep_ms(renew_after(lease.seconds));
                error("dhcp: renewing the lease\n");
            }
            None => {
                error("dhcp: no answer, trying again later\n");
                sleep_ms(RETRY_MS);
            }
        }
    }
}

/// Что приехало в ACK.
struct Lease {
    address: u32,
    netmask: u32,
    gateway: u32,
    dns: u32,
    seconds: u32,
    server: u32,
}

/// Пройти четыре шага и получить аренду.
fn lease(info: &NetInfo) -> Option<Lease> {
    let socket = socket();
    if socket < 0 {
        error("dhcp: cannot open a socket\n");
        return None;
    }
    let result = exchange(info, socket);
    // Сокет закрывается на любом пути: порт 68 один на систему, и оставить его
    // за собой значит не дать себе же перезапуститься.
    close_socket(socket);
    result
}

fn exchange(info: &NetInfo, socket: i64) -> Option<Lease> {
    if bind(socket, PORT_CLIENT) < 0 {
        error("dhcp: port 68 is taken\n");
        return None;
    }
    if connect(socket, BROADCAST, PORT_SERVER) < 0 {
        error("dhcp: cannot address the broadcast\n");
        return None;
    }

    // Идентификатор сделки берётся из часов. Случайности у программы нет, а
    // постоянное число означало бы, что ответ на прошлую попытку сходит за
    // ответ на эту.
    let xid = (uptime_ms() as u32) | 1;

    let mut packet = [0u8; PACKET];
    let mut reply = [0u8; PACKET];

    // --- DISCOVER ---------------------------------------------------------
    let offer = {
        let len = build(&mut packet, info.mac, xid, DISCOVER, 0, 0);
        let mut found = None;
        for _ in 0..ATTEMPTS {
            if send_waiting(socket, &packet[..len], 40) < 0 {
                continue;
            }
            if let Some(offer) = wait_for(socket, &mut reply, xid, OFFER) {
                found = Some(offer);
                break;
            }
        }
        found?
    };

    error("dhcp: offer ");
    print_ip(offer.address);
    error(" from ");
    print_ip(offer.server);
    error("\n");

    // --- REQUEST ----------------------------------------------------------
    let len = build(&mut packet, info.mac, xid, REQUEST, offer.address, offer.server);
    for _ in 0..ATTEMPTS {
        if send_waiting(socket, &packet[..len], 40) < 0 {
            continue;
        }
        if let Some(ack) = wait_for(socket, &mut reply, xid, ACK) {
            return Some(ack);
        }
    }
    error("dhcp: the server never confirmed the address\n");
    None
}

/// Дождаться ответа нужного типа.
fn wait_for(socket: i64, buffer: &mut [u8; PACKET], xid: u32, want: u8) -> Option<Lease> {
    let deadline = uptime_ms() + REPLY_TIMEOUT_MS;
    while uptime_ms() < deadline {
        let got = recv_waiting(socket, buffer, REPLY_TIMEOUT_MS);
        if got <= 0 {
            return None;
        }
        let Some(parsed) = parse(&buffer[..got as usize], xid) else {
            // Чужой разговор в той же подсети: широковещательные ответы видят
            // все, и отбрасывать не свои обязан каждый.
            continue;
        };
        if parsed.1 == NAK {
            error("dhcp: the server refused the address\n");
            return None;
        }
        if parsed.1 == want {
            return Some(parsed.0);
        }
    }
    None
}

/// Собрать пакет и вернуть его длину.
fn build(
    packet: &mut [u8; PACKET],
    mac: [u8; 6],
    xid: u32,
    kind: u8,
    requested: u32,
    server: u32,
) -> usize {
    packet.fill(0);
    packet[0] = 1; // это запрос от клиента
    packet[1] = 1; // сеть Ethernet
    packet[2] = 6; // длина аппаратного адреса
    packet[4..8].copy_from_slice(&xid.to_be_bytes());
    // Флаг «отвечайте широковещательно» — см. заголовок модуля.
    packet[10..12].copy_from_slice(&0x8000u16.to_be_bytes());
    packet[28..34].copy_from_slice(&mac);
    packet[FIXED..FIXED + 4].copy_from_slice(&COOKIE);

    let mut at = FIXED + 4;
    at = put(packet, at, OPT_MESSAGE_TYPE, &[kind]);
    if requested != 0 {
        at = put(packet, at, OPT_REQUESTED_IP, &requested.to_be_bytes());
    }
    if server != 0 {
        at = put(packet, at, OPT_SERVER_ID, &server.to_be_bytes());
    }
    // Спрашиваем ровно то, чем умеем распорядиться. Просить больше — значит
    // получать в ответ опции, которые придётся молча выбрасывать.
    at = put(
        packet,
        at,
        OPT_PARAMETER_LIST,
        &[OPT_SUBNET_MASK, OPT_ROUTER, OPT_DNS],
    );
    packet[at] = OPT_END;
    at + 1
}

fn put(packet: &mut [u8; PACKET], at: usize, option: u8, value: &[u8]) -> usize {
    packet[at] = option;
    packet[at + 1] = value.len() as u8;
    packet[at + 2..at + 2 + value.len()].copy_from_slice(value);
    at + 2 + value.len()
}

/// Разобрать ответ. Возвращает аренду и тип сообщения.
fn parse(message: &[u8], xid: u32) -> Option<(Lease, u8)> {
    if message.len() < FIXED + 4 {
        return None;
    }
    // Ответ от сервера, наша сделка, наша метка — три проверки, и все три
    // обязательны: широковещательный ответ видят все, кто есть в подсети.
    if message[0] != 2 {
        return None;
    }
    if u32::from_be_bytes([message[4], message[5], message[6], message[7]]) != xid {
        return None;
    }
    if message[FIXED..FIXED + 4] != COOKIE {
        return None;
    }

    let mut lease = Lease {
        address: u32::from_be_bytes([message[16], message[17], message[18], message[19]]),
        netmask: 0,
        gateway: 0,
        dns: 0,
        seconds: 0,
        server: 0,
    };
    let mut kind = 0u8;

    let mut at = FIXED + 4;
    while at < message.len() {
        let option = message[at];
        if option == OPT_END {
            break;
        }
        // Опция 0 — заполнитель, у неё нет ни длины, ни значения.
        if option == 0 {
            at += 1;
            continue;
        }
        if at + 1 >= message.len() {
            break;
        }
        let length = usize::from(message[at + 1]);
        let value = message.get(at + 2..at + 2 + length)?;
        match (option, length) {
            (OPT_MESSAGE_TYPE, 1) => kind = value[0],
            (OPT_SUBNET_MASK, 4) => lease.netmask = be32(value),
            // Маршрутизаторов может быть несколько; берём первый — таблицы
            // маршрутов у нас всё равно одна строка.
            (OPT_ROUTER, len) if len >= 4 => lease.gateway = be32(value),
            (OPT_DNS, len) if len >= 4 => lease.dns = be32(value),
            (OPT_LEASE_TIME, 4) => lease.seconds = be32(value),
            (OPT_SERVER_ID, 4) => lease.server = be32(value),
            _ => {}
        }
        at += 2 + length;
    }

    if lease.address == 0 || kind == 0 {
        return None;
    }
    Some((lease, kind))
}

fn be32(value: &[u8]) -> u32 {
    u32::from_be_bytes([value[0], value[1], value[2], value[3]])
}

/// Отдать полученное ядру и рассказать об этом.
fn apply(lease: &Lease) {
    // Маска, которой сервер не прислал, — обычное дело для сетей класса C:
    // берём /24, потому что адрес без маски бесполезен, а угадать её здесь
    // можно только так.
    let netmask = if lease.netmask == 0 { 0xFFFF_FF00 } else { lease.netmask };
    let config = NetConfig {
        address: lease.address,
        netmask,
        gateway: lease.gateway,
        dns: lease.dns,
    };
    let result = netconf(&config);
    if result != 0 {
        error("dhcp: the kernel refused the configuration, code ");
        error_num(result);
        error("\n");
        return;
    }

    error("dhcp: lease ");
    print_ip(lease.address);
    error("/");
    error_num(i64::from(prefix(netmask)));
    error(" gw ");
    print_ip(lease.gateway);
    error(" dns ");
    print_ip(lease.dns);
    error(" for ");
    error_num(i64::from(lease.seconds));
    error(" s\n");
}

/// Длина префикса маски.
fn prefix(netmask: u32) -> u32 {
    netmask.leading_ones()
}

fn print_ip(address: u32) {
    let bytes = address.to_be_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            error(".");
        }
        error_num(i64::from(*byte));
    }
}

fn print_mac(mac: [u8; 6]) {
    for (index, byte) in mac.iter().enumerate() {
        if index > 0 {
            error(":");
        }
        print_hex(*byte);
    }
}

fn print_hex(byte: u8) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let text = [DIGITS[usize::from(byte >> 4)], DIGITS[usize::from(byte & 0x0F)]];
    // SAFETY: обе цифры из таблицы выше — ASCII.
    error(unsafe { core::str::from_utf8_unchecked(&text) });
}
