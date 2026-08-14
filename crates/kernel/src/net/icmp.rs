//! ICMP: пока только эхо — то, что называется `ping`.
//!
//! Сообщение состоит из четырёх байт заголовка (тип, код, контрольная сумма),
//! четырёх байт, специфичных для эха (идентификатор и номер), и данных, которые
//! отвечающий обязан вернуть без изменений. Именно на этом «без изменений»
//! проверка и держится: совпавшие данные означают, что пакет прошёл туда и
//! обратно целиком, а не что кто-то в пути сочинил правдоподобный ответ.

use crate::net::ipv4;

/// Тип: эхо-ответ.
pub const ECHO_REPLY: u8 = 0;
/// Тип: эхо-запрос.
pub const ECHO_REQUEST: u8 = 8;

/// Длина заголовка эха: тип, код, сумма, идентификатор, номер.
pub const HEADER: usize = 8;

/// Разобранное эхо.
pub struct Echo<'a> {
    pub kind: u8,
    pub id: u16,
    pub sequence: u16,
    pub data: &'a [u8],
}

/// Разобрать сообщение ICMP, если это эхо.
///
/// Контрольная сумма проверяется по **всему** сообщению — в отличие от IPv4,
/// где она покрывает только заголовок. Это разные правила для двух сумм,
/// лежащих в одном пакете в двадцати байтах друг от друга.
pub fn parse(message: &[u8]) -> Option<Echo<'_>> {
    if message.len() < HEADER {
        return None;
    }
    let kind = message[0];
    if kind != ECHO_REQUEST && kind != ECHO_REPLY {
        return None;
    }
    if ipv4::checksum(&[message]) != 0 {
        return None;
    }
    Some(Echo {
        kind,
        id: u16::from_be_bytes([message[4], message[5]]),
        sequence: u16::from_be_bytes([message[6], message[7]]),
        data: &message[HEADER..],
    })
}

/// Собрать эхо в буфер и вернуть его длину.
///
/// Буфер обязан вмещать заголовок и данные; вызывающий это знает, потому что
/// сам его и выделял.
pub fn write(buffer: &mut [u8], kind: u8, id: u16, sequence: u16, data: &[u8]) -> usize {
    let len = HEADER + data.len();
    buffer[0] = kind;
    buffer[1] = 0; // код: у эха он всегда ноль
    buffer[2..4].copy_from_slice(&[0, 0]);
    buffer[4..6].copy_from_slice(&id.to_be_bytes());
    buffer[6..8].copy_from_slice(&sequence.to_be_bytes());
    buffer[HEADER..len].copy_from_slice(data);
    let sum = ipv4::checksum(&[&buffer[..len]]);
    buffer[2..4].copy_from_slice(&sum.to_be_bytes());
    len
}
