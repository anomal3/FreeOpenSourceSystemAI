//! IPv4: адрес, заголовок и контрольная сумма.
//!
//! # Про контрольную сумму
//!
//! Она здесь одна на весь модуль, а считается в двух разных местах и по разным
//! правилам — это первая ловушка, которую ставит IP-стек. У **IPv4** сумма
//! покрывает только заголовок, у **ICMP** — всё сообщение целиком. Написать
//! одну функцию и вызвать её от разных срезов правильно; написать одну функцию
//! и вызвать её дважды от одного и того же среза — это стек, который
//! отправляет пакеты, а в ответ не получает ничего, и понять почему нельзя,
//! пока не посмотришь на провод чужими глазами.
//!
//! Сама сумма — дополнение до единицы к сумме 16-битных слов в дополнительном
//! коде. Считается она с полем суммы, заполненным нулями, а проверяется по
//! готовому пакету: правильный пакет даёт `0xFFFF` до дополнения, то есть ноль
//! после.

use core::fmt;

/// Длина заголовка без параметров.
pub const HEADER: usize = 20;

/// Протокол: ICMP.
pub const PROTOCOL_ICMP: u8 = 1;
/// Протокол: UDP. Появится в фазе 35; здесь он ради разбора входящего.
#[allow(dead_code)]
pub const PROTOCOL_UDP: u8 = 17;
/// Протокол: TCP.
#[allow(dead_code)]
pub const PROTOCOL_TCP: u8 = 6;

/// Время жизни исходящих пакетов.
///
/// 64 — то же, что у большинства систем. Значение попадает в ответы, которые
/// увидит чужая машина, и выделяться здесь незачем.
const TTL: u8 = 64;

/// Адрес IPv4.
///
/// Хранится числом в машинном порядке, а не массивом байт: сравнение с маской
/// и вычисление сети — это арифметика, и делать её над байтами значит писать
/// цикл там, где достаточно одной операции.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ipv4(pub u32);

impl Ipv4 {
    /// Адрес, которого нет: «не настроен» и одновременно источник в DHCP.
    pub const UNSPECIFIED: Ipv4 = Ipv4(0);
    /// Широковещательный адрес подсети целиком.
    pub const BROADCAST: Ipv4 = Ipv4(0xFFFF_FFFF);

    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        Self(u32::from_be_bytes(bytes))
    }

    pub const fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    pub const fn is_unspecified(self) -> bool {
        self.0 == 0
    }

    pub const fn is_broadcast(self) -> bool {
        self.0 == Self::BROADCAST.0
    }

    /// Лежит ли адрес в той же сети, что и `self`, при данной маске.
    pub const fn same_network(self, other: Ipv4, mask: Ipv4) -> bool {
        self.0 & mask.0 == other.0 & mask.0
    }

    /// Широковещательный адрес сети, к которой принадлежит адрес.
    pub const fn network_broadcast(self, mask: Ipv4) -> Ipv4 {
        Ipv4((self.0 & mask.0) | !mask.0)
    }

    /// Разобрать запись вида `10.0.2.15`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut bytes = [0u8; 4];
        let mut seen = 0usize;
        for (index, part) in text.split('.').enumerate() {
            if index >= 4 || part.is_empty() {
                return None;
            }
            bytes[index] = part.parse::<u8>().ok()?;
            seen = index + 1;
        }
        if seen != 4 {
            return None;
        }
        Some(Self::from_bytes(bytes))
    }

    /// Разобрать маску, записанную длиной префикса: `24` — это `255.255.255.0`.
    pub fn from_prefix(bits: u32) -> Option<Self> {
        match bits {
            0 => Some(Ipv4(0)),
            1..=32 => Some(Ipv4(u32::MAX << (32 - bits))),
            _ => None,
        }
    }

    /// Длина префикса, если маска сплошная; иначе `None`.
    pub fn prefix(self) -> Option<u32> {
        let ones = self.0.leading_ones();
        if ones == 32 || self.0 << ones == 0 {
            Some(ones)
        } else {
            None
        }
    }
}

impl fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.to_bytes();
        write!(f, "{a}.{b}.{c}.{d}")
    }
}

/// Сумма 16-битных слов с переносом в младший разряд, дополненная до единицы.
///
/// Нечётный хвост дополняется нулевым байтом — так требует RFC 1071, и это не
/// то же самое, что «пропустить последний байт»: пропуск даёт сумму, которая
/// сходится у нас и не сходится у всех остальных.
pub fn checksum(parts: &[&[u8]]) -> u16 {
    let mut sum: u32 = 0;
    // Хвост предыдущего среза: слово может начаться в одном куске и кончиться в
    // другом, и складывать каждый кусок по отдельности значило бы выровнять
    // слова заново на каждой границе.
    let mut pending: Option<u8> = None;

    for part in parts {
        let mut bytes = part.iter().copied();
        if let Some(high) = pending.take() {
            if let Some(low) = bytes.next() {
                sum += u32::from(u16::from_be_bytes([high, low]));
            } else {
                pending = Some(high);
            }
        }
        loop {
            let Some(high) = bytes.next() else { break };
            match bytes.next() {
                Some(low) => sum += u32::from(u16::from_be_bytes([high, low])),
                None => {
                    pending = Some(high);
                    break;
                }
            }
        }
    }
    if let Some(high) = pending {
        sum += u32::from(u16::from_be_bytes([high, 0]));
    }

    while sum >> 16 != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Разобранный заголовок IPv4 и то, что за ним.
pub struct Packet<'a> {
    pub source: Ipv4,
    pub destination: Ipv4,
    pub protocol: u8,
    pub payload: &'a [u8],
}

/// Разобрать пакет.
///
/// Отвергается всё, чему мы не в состоянии верить: чужая версия, обрезанный
/// заголовок, длина, обещающая больше данных, чем приехало, и неверная
/// контрольная сумма. Фрагменты тоже отвергаются — собирать их мы не умеем, а
/// разобрать первый фрагмент как целый пакет значит выдать половину сообщения
/// за сообщение.
pub fn parse(datagram: &[u8]) -> Option<Packet<'_>> {
    if datagram.len() < HEADER {
        return None;
    }
    let version = datagram[0] >> 4;
    let header_len = usize::from(datagram[0] & 0x0F) * 4;
    if version != 4 || header_len < HEADER || datagram.len() < header_len {
        return None;
    }

    let total = usize::from(u16::from_be_bytes([datagram[2], datagram[3]]));
    if total < header_len || total > datagram.len() {
        // Кадр Ethernet короче обещанного пакета — значит его обрезали, и
        // содержимое неполное. Кадр *длиннее* — обычное дело: короткие кадры
        // дополняются нулями до 60 байт, поэтому сравнение только в одну сторону.
        return None;
    }

    // Флаг «ещё будут фрагменты» либо ненулевое смещение — это фрагмент.
    let flags_and_offset = u16::from_be_bytes([datagram[6], datagram[7]]);
    if flags_and_offset & 0x2000 != 0 || flags_and_offset & 0x1FFF != 0 {
        return None;
    }

    if checksum(&[&datagram[..header_len]]) != 0 {
        return None;
    }

    let mut source = [0u8; 4];
    let mut destination = [0u8; 4];
    source.copy_from_slice(&datagram[12..16]);
    destination.copy_from_slice(&datagram[16..20]);

    Some(Packet {
        source: Ipv4::from_bytes(source),
        destination: Ipv4::from_bytes(destination),
        protocol: datagram[9],
        payload: &datagram[header_len..total],
    })
}

/// Записать заголовок исходящего пакета.
///
/// `payload_len` — длина того, что ляжет за заголовком; `id` — идентификатор
/// пакета, который пригодился бы при фрагментации. Фрагментировать мы не умеем
/// и просим этого не делать (`Don't Fragment`), но идентификатор всё равно
/// обязан меняться: одинаковый у всех пакетов сбивает с толку тех, кто их
/// разбирает.
pub fn write_header(
    buffer: &mut [u8],
    source: Ipv4,
    destination: Ipv4,
    protocol: u8,
    payload_len: usize,
    id: u16,
) {
    let total = (HEADER + payload_len) as u16;
    buffer[0] = 0x45; // версия 4, длина заголовка 5 слов
    buffer[1] = 0; // без указаний о приоритете
    buffer[2..4].copy_from_slice(&total.to_be_bytes());
    buffer[4..6].copy_from_slice(&id.to_be_bytes());
    buffer[6..8].copy_from_slice(&0x4000u16.to_be_bytes()); // Don't Fragment
    buffer[8] = TTL;
    buffer[9] = protocol;
    // Сумма считается по заголовку, в котором её поле — нули. Порядок здесь
    // обязателен, а не удобен.
    buffer[10..12].copy_from_slice(&[0, 0]);
    buffer[12..16].copy_from_slice(&source.to_bytes());
    buffer[16..20].copy_from_slice(&destination.to_bytes());
    let sum = checksum(&[&buffer[..HEADER]]);
    buffer[10..12].copy_from_slice(&sum.to_be_bytes());
}
