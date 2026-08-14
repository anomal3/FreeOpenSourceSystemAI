//! ARP: «у кого этот адрес» и таблица ответов.
//!
//! # Зачем таблица, если можно спрашивать каждый раз
//!
//! Потому что спрашивать — это широковещательный кадр, который получают все, и
//! ответ, которого надо ждать. Пакет, отправляемый по адресу, для которого ещё
//! нет записи, поэтому не отправляется вовсе: сначала запрос, потом ожидание,
//! потом повтор. Пытаться придержать пакет до ответа мы не будем — очередь
//! отложенных пакетов это вторая жизнь и второй источник ошибок, а всё, что
//! ходит поверх IP, и так умеет переспрашивать.
//!
//! # Про срок годности
//!
//! Запись живёт минуту. Не вечно — потому что адрес переезжает между машинами
//! (замена сетевой карты, переезд шлюза), и таблица без срока годности означает
//! систему, которая шлёт кадры в пустоту до перезагрузки. Не секунду — потому
//! что запрос на каждый пакет превратил бы обмен в две операции вместо одной.

use crate::net::eth::{self, Mac};
use crate::net::ipv4::Ipv4;

/// Длина пакета ARP для Ethernet и IPv4.
pub const PACKET: usize = 28;

/// Операция: запрос.
pub const REQUEST: u16 = 1;
/// Операция: ответ.
pub const REPLY: u16 = 2;

/// Тип аппаратной сети: Ethernet.
const HARDWARE_ETHERNET: u16 = 1;

/// Сколько живёт запись таблицы.
const LIFETIME_MS: u64 = 60_000;

/// Сколько адресов помнится одновременно.
///
/// Восьми хватает: в подсети, где живёт эта система, собеседников считанные
/// единицы — шлюз, сервер имён, машина человека. Таблица переполняется
/// вытеснением самой старой записи, а не отказом.
const ENTRIES: usize = 8;

/// Разобранный пакет ARP.
pub struct Packet {
    pub operation: u16,
    pub sender_mac: Mac,
    pub sender_ip: Ipv4,
    pub target_ip: Ipv4,
}

/// Разобрать пакет.
///
/// Всё, что не «Ethernet поверх IPv4», отвергается: другие сочетания
/// существуют, но их поля лежат по другим смещениям, и разбирать их по этой
/// раскладке значит читать чужие байты как адреса.
pub fn parse(payload: &[u8]) -> Option<Packet> {
    if payload.len() < PACKET {
        return None;
    }
    let hardware = u16::from_be_bytes([payload[0], payload[1]]);
    let protocol = u16::from_be_bytes([payload[2], payload[3]]);
    if hardware != HARDWARE_ETHERNET || protocol != eth::TYPE_IPV4 {
        return None;
    }
    if payload[4] != 6 || payload[5] != 4 {
        return None;
    }

    let mut sender_mac = [0u8; 6];
    sender_mac.copy_from_slice(&payload[8..14]);
    let mut sender_ip = [0u8; 4];
    sender_ip.copy_from_slice(&payload[14..18]);
    let mut target_ip = [0u8; 4];
    target_ip.copy_from_slice(&payload[24..28]);

    Some(Packet {
        operation: u16::from_be_bytes([payload[6], payload[7]]),
        sender_mac,
        sender_ip: Ipv4::from_bytes(sender_ip),
        target_ip: Ipv4::from_bytes(target_ip),
    })
}

/// Записать пакет ARP в буфер.
pub fn write(
    buffer: &mut [u8],
    operation: u16,
    sender_mac: Mac,
    sender_ip: Ipv4,
    target_mac: Mac,
    target_ip: Ipv4,
) {
    buffer[0..2].copy_from_slice(&HARDWARE_ETHERNET.to_be_bytes());
    buffer[2..4].copy_from_slice(&eth::TYPE_IPV4.to_be_bytes());
    buffer[4] = 6;
    buffer[5] = 4;
    buffer[6..8].copy_from_slice(&operation.to_be_bytes());
    buffer[8..14].copy_from_slice(&sender_mac);
    buffer[14..18].copy_from_slice(&sender_ip.to_bytes());
    // В запросе адрес получателя неизвестен по определению — там нули, а не
    // широковещательный адрес: широковещательным будет заголовок кадра, а поле
    // пакета означает «кого ищем», и заполнять его единицами значит искать
    // всех сразу.
    buffer[18..24].copy_from_slice(&target_mac);
    buffer[24..28].copy_from_slice(&target_ip.to_bytes());
}

/// Одна запись таблицы.
#[derive(Clone, Copy)]
struct Entry {
    ip: Ipv4,
    mac: Mac,
    /// Время работы системы, после которого запись перестаёт годиться.
    expires_ms: u64,
}

/// Таблица соответствий «адрес IP — адрес на проводе».
pub struct Table {
    entries: [Option<Entry>; ENTRIES],
}

impl Table {
    pub const fn new() -> Self {
        Self { entries: [None; ENTRIES] }
    }

    /// Найти адрес, если он известен и запись ещё не протухла.
    pub fn lookup(&self, ip: Ipv4, now_ms: u64) -> Option<Mac> {
        self.entries
            .iter()
            .flatten()
            .find(|entry| entry.ip == ip && entry.expires_ms > now_ms)
            .map(|entry| entry.mac)
    }

    /// Запомнить соответствие.
    ///
    /// Запоминается **всё**, что приходит в пакетах ARP, а не только ответы на
    /// наши запросы: тот, кто нас спрашивает, обычно и есть тот, кому мы сейчас
    /// будем отвечать, и второй запрос ради того же адреса — лишний кадр.
    pub fn remember(&mut self, ip: Ipv4, mac: Mac, now_ms: u64) {
        let expires_ms = now_ms + LIFETIME_MS;
        // Известный адрес обновляется на месте: две записи об одном адресе
        // означали бы, что после переезда машины таблица какое-то время
        // возвращает то старый ответ, то новый.
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|slot| matches!(slot, Some(entry) if entry.ip == ip))
        {
            *slot = Some(Entry { ip, mac, expires_ms });
            return;
        }
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|slot| slot.is_none() || slot.is_some_and(|entry| entry.expires_ms <= now_ms))
        {
            *slot = Some(Entry { ip, mac, expires_ms });
            return;
        }
        // Свободных нет и протухших нет — вытесняется та запись, которой
        // осталось жить меньше всех.
        if let Some(slot) = self
            .entries
            .iter_mut()
            .min_by_key(|slot| slot.map_or(0, |entry| entry.expires_ms))
        {
            *slot = Some(Entry { ip, mac, expires_ms });
        }
    }

    /// Перебрать живые записи — для команды `arp` в оболочке.
    pub fn iter(&self, now_ms: u64) -> impl Iterator<Item = (Ipv4, Mac, u64)> + '_ {
        self.entries
            .iter()
            .flatten()
            .filter(move |entry| entry.expires_ms > now_ms)
            .map(move |entry| (entry.ip, entry.mac, entry.expires_ms - now_ms))
    }
}
