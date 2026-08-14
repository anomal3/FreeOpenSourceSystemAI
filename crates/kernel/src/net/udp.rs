//! UDP: восемь байт заголовка и датаграмма как она есть.
//!
//! # Контрольная сумма считается не по тому, что лежит в пакете
//!
//! Это третья сумма в стеке и третье правило её вычисления. У IPv4 она покрывает
//! заголовок, у ICMP — сообщение целиком, а у UDP — сообщение **плюс
//! псевдозаголовок**: адреса отправителя и получателя, номер протокола и длина,
//! которых в самой датаграмме нет и которые берутся из IP-пакета вокруг неё.
//!
//! Смысл в том, что иначе датаграмма, доставленная не тому адресату (ошибка
//! маршрутизации, порча адреса в пути), сошлась бы по сумме и была бы принята.
//! Псевдозаголовок привязывает содержимое к адресам, а цена — необходимость
//! знать их здесь, на уровне, который вообще-то про порты.
//!
//! # Ноль в поле суммы
//!
//! В IPv4 контрольная сумма UDP необязательна: ноль означает «не считалась».
//! Мы считаем всегда, а вот приходящий ноль принимаем — так требует RFC 768, и
//! отвергать такие датаграммы значило бы ломать связь с теми, кто в своём праве.
//! Отдельная тонкость: если посчитанная сумма вышла нулём, на провод уходит
//! `0xFFFF` — иначе получатель прочтёт её как «не считалась». Обе величины
//! означают одно и то же в арифметике дополнения до единицы, и разница между
//! ними существует ровно ради этого случая.

use crate::net::ipv4::{self, Ipv4};

/// Длина заголовка.
pub const HEADER: usize = 8;

/// Разобранная датаграмма.
pub struct Datagram<'a> {
    pub source_port: u16,
    pub destination_port: u16,
    pub payload: &'a [u8],
}

/// Разобрать датаграмму, проверив сумму по псевдозаголовку.
pub fn parse<'a>(source: Ipv4, destination: Ipv4, message: &'a [u8]) -> Option<Datagram<'a>> {
    if message.len() < HEADER {
        return None;
    }
    let length = usize::from(u16::from_be_bytes([message[4], message[5]]));
    // Длина в заголовке считается вместе с ним самим и не может быть меньше.
    // Больше принятого — значит датаграмму обрезали в пути.
    if length < HEADER || length > message.len() {
        return None;
    }
    let message = &message[..length];

    let sum = u16::from_be_bytes([message[6], message[7]]);
    if sum != 0 && checksum(source, destination, message) != 0 {
        return None;
    }

    Some(Datagram {
        source_port: u16::from_be_bytes([message[0], message[1]]),
        destination_port: u16::from_be_bytes([message[2], message[3]]),
        payload: &message[HEADER..],
    })
}

/// Записать датаграмму в буфер и вернуть её длину.
pub fn write(
    buffer: &mut [u8],
    source: Ipv4,
    destination: Ipv4,
    source_port: u16,
    destination_port: u16,
    payload: &[u8],
) -> usize {
    let length = HEADER + payload.len();
    buffer[0..2].copy_from_slice(&source_port.to_be_bytes());
    buffer[2..4].copy_from_slice(&destination_port.to_be_bytes());
    buffer[4..6].copy_from_slice(&(length as u16).to_be_bytes());
    // Сумма считается по датаграмме, в которой её поле — нули.
    buffer[6..8].copy_from_slice(&[0, 0]);
    buffer[HEADER..length].copy_from_slice(payload);

    let sum = checksum(source, destination, &buffer[..length]);
    // Ноль на проводе означает «не считалась», поэтому вместо него уходит
    // `0xFFFF` — то же самое число в арифметике дополнения до единицы.
    let sum = if sum == 0 { 0xFFFF } else { sum };
    buffer[6..8].copy_from_slice(&sum.to_be_bytes());
    length
}

/// Сумма по псевдозаголовку и датаграмме.
fn checksum(source: Ipv4, destination: Ipv4, message: &[u8]) -> u16 {
    let length = (message.len() as u16).to_be_bytes();
    let pseudo = [
        source.to_bytes()[0],
        source.to_bytes()[1],
        source.to_bytes()[2],
        source.to_bytes()[3],
        destination.to_bytes()[0],
        destination.to_bytes()[1],
        destination.to_bytes()[2],
        destination.to_bytes()[3],
        0,
        ipv4::PROTOCOL_UDP,
        length[0],
        length[1],
    ];
    ipv4::checksum(&[&pseudo, message])
}
