//! Сеть по USB: RNDIS и CDC ECM (фаза 68p).
//!
//! # Зачем
//!
//! Универсального драйвера сетевой карты не бывает: у Realtek, Intel и Atheros
//! свои регистры, а сетевой драйвер прошивки умирает вместе с её службами. Но
//! у USB есть **классы** сетевых функций, и одного драйвера класса хватает
//! на всё, что под него подходит:
//!
//! * **RNDIS** — так раздаёт интернет по кабелю любой Android («USB-модем»);
//! * **CDC ECM** — USB-Ethernet адаптеры (RTL8153 во второй конфигурации и
//!   подобные), модемы.
//!
//! Телефон есть у каждого, и с ним сеть появляется на любом ПК с xHCI — в том
//! числе на том, чья встроенная карта нам неизвестна.
//!
//! # Как
//!
//! xHCI поднимает функцию до настроенных bulk-точек и колец приёма. Здесь —
//! рукопожатие (RNDIS: `INITIALIZE`, адрес `OID_802_3_PERMANENT_ADDRESS`,
//! фильтр приёма; ECM: адрес из строки `iMACAddress`, фильтр) и обёртка
//! кадров: у RNDIS — заголовок `PACKET_MSG` в 44 байта, у ECM — кадр как есть.
//! Готовая карта уходит в сетевой стек ([`crate::net::attach_usb`]).
//!
//! Рукопожатие отложено, как у накопителя: подключение замечается, пока
//! контроллеры вынуты из списка, а управляющие передачи во время работы идут
//! через список (`xhci::net_control`).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;

use crate::kprintln;
use crate::net::card::{Card, CardError, FRAME_MAX, Stats};
use crate::sync::SpinLock;
use crate::usb::xhci::{self, NET_SLOT, NetError, NetProtocol, UsbHandle};

/// Подключённые, но ещё не поднятые функции: ссылка, протокол, управляющий
/// интерфейс и строка адреса.
static ARRIVED: SpinLock<Vec<(UsbHandle, NetProtocol, u8, u8)>> = SpinLock::new(Vec::new());

/// Сетевая функция подключена; поднимет её [`settle`].
pub fn arrived(handle: UsbHandle, protocol: NetProtocol, control: u8, mac_string: u8) {
    ARRIVED.lock().push((handle, protocol, control, mac_string));
}

/// Функция выдернута. Карта узнает это сама — передачи ответят «нет
/// устройства», — а стек сменит её, когда воткнут новую.
pub fn gone(_handle: UsbHandle) {
    kprintln!("  usb-net     : the network adapter is gone");
}

/// Поднять подключённые функции и отдать их сетевому стеку. Зовётся, когда
/// контроллеры xHCI в списке, не под локом.
pub fn settle() {
    loop {
        let next = ARRIVED.lock().pop();
        let Some((handle, protocol, control, mac_string)) = next else { break };
        let card = match protocol {
            NetProtocol::Rndis => UsbNet::rndis(handle, control),
            NetProtocol::Ecm => UsbNet::ecm(handle, control, mac_string),
        };
        match card {
            Ok(card) => {
                xhci::net_start(handle);
                kprintln!(
                    "  usb-net     : {} up, hardware address {}",
                    card.name(),
                    crate::net::eth::Display(card.mac)
                );
                crate::net::attach_usb(Box::new(card));
            }
            Err(err) => kprintln!("  usb-net     : the adapter did not come up: {err}"),
        }
    }
}

// --- RNDIS (MS-RNDIS) ---------------------------------------------------------

const RNDIS_PACKET_MSG: u32 = 0x0000_0001;
const RNDIS_INITIALIZE_MSG: u32 = 0x0000_0002;
const RNDIS_QUERY_MSG: u32 = 0x0000_0004;
const RNDIS_SET_MSG: u32 = 0x0000_0005;
const RNDIS_INITIALIZE_CMPLT: u32 = 0x8000_0002;
const RNDIS_QUERY_CMPLT: u32 = 0x8000_0004;
const RNDIS_SET_CMPLT: u32 = 0x8000_0005;
const OID_802_3_PERMANENT_ADDRESS: u32 = 0x0101_0101;
const OID_GEN_CURRENT_PACKET_FILTER: u32 = 0x0001_010E;
/// Свои кадры, широковещательные и все групповые.
const RNDIS_FILTER: u32 = 0x0000_000D;
/// Заголовок `PACKET_MSG`.
const RNDIS_HEADER: usize = 44;
/// Сколько раз спрашивать ответ на команду, прежде чем сдаться.
const RESPONSE_ATTEMPTS: u32 = 100;

/// Почему функция не поднялась.
#[derive(Clone, Copy, Debug)]
enum Error {
    Usb(NetError),
    /// Ответа на команду так и не пришло.
    NoResponse(u32),
    /// Ответ пришёл, но с ошибкой или не тот.
    Refused(u32, u32),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Usb(err) => write!(f, "USB transfer failed: {err:?}"),
            Self::NoResponse(kind) => write!(f, "no answer to RNDIS message {kind:#x}"),
            Self::Refused(kind, status) => write!(f, "RNDIS message {kind:#x} refused with status {status:#x}"),
        }
    }
}

impl From<NetError> for Error {
    fn from(err: NetError) -> Self {
        Self::Usb(err)
    }
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    bytes.get(at..at + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// Сетевая функция USB как карта стека.
struct UsbNet {
    handle: UsbHandle,
    protocol: NetProtocol,
    control: u8,
    mac: [u8; 6],
    stats: Stats,
    request: u32,
    /// Кадры, разобранные из пришедшего, но ещё не отданные стеку: RNDIS
    /// вправе сложить несколько кадров в одну передачу.
    pending: VecDeque<Vec<u8>>,
    buffer: Vec<u8>,
}

impl UsbNet {
    fn new(handle: UsbHandle, protocol: NetProtocol, control: u8) -> Self {
        Self {
            handle,
            protocol,
            control,
            mac: [0; 6],
            stats: Stats::default(),
            request: 0,
            pending: VecDeque::new(),
            buffer: alloc::vec![0; NET_SLOT],
        }
    }

    /// RNDIS: `INITIALIZE`, адрес, фильтр.
    fn rndis(handle: UsbHandle, control: u8) -> Result<Self, Error> {
        let mut card = Self::new(handle, NetProtocol::Rndis, control);
        // Наибольшая передача от устройства к нам — ячейка приёма: так кадры
        // приходят по одному и всегда помещаются.
        let init = card.rndis_command(RNDIS_INITIALIZE_MSG, &[1, 0, NET_SLOT as u32])?;
        if u32_at(&init, 0) != RNDIS_INITIALIZE_CMPLT {
            return Err(Error::Refused(RNDIS_INITIALIZE_MSG, u32_at(&init, 0)));
        }
        // QUERY: OID, длина и смещение буфера (пусто), VcHandle.
        let reply = card.rndis_command(RNDIS_QUERY_MSG, &[OID_802_3_PERMANENT_ADDRESS, 0, 0, 0])?;
        if u32_at(&reply, 0) != RNDIS_QUERY_CMPLT {
            return Err(Error::Refused(RNDIS_QUERY_MSG, u32_at(&reply, 0)));
        }
        let (len, offset) = (u32_at(&reply, 16) as usize, u32_at(&reply, 20) as usize);
        let at = 8 + offset;
        if len >= 6 && reply.len() >= at + 6 {
            card.mac.copy_from_slice(&reply[at..at + 6]);
        }
        // SET: OID, длина буфера 4, смещение 20 (от поля RequestId), VcHandle,
        // сам фильтр.
        let reply = card.rndis_command(RNDIS_SET_MSG, &[OID_GEN_CURRENT_PACKET_FILTER, 4, 20, 0, RNDIS_FILTER])?;
        if u32_at(&reply, 0) != RNDIS_SET_CMPLT {
            return Err(Error::Refused(RNDIS_SET_MSG, u32_at(&reply, 0)));
        }
        Ok(card)
    }

    /// ECM: адрес из строки `iMACAddress`, фильтр приёма.
    fn ecm(handle: UsbHandle, control: u8, mac_string: u8) -> Result<Self, Error> {
        let mut card = Self::new(handle, NetProtocol::Ecm, control);
        if mac_string != 0 {
            // GET_DESCRIPTOR(STRING, индекс, язык 0x0409).
            let text = xhci::net_control(handle, [0x80, 6, mac_string, 3, 0x09, 0x04, 255, 0], &[], 255)?;
            // Двенадцать шестнадцатеричных цифр в UTF-16LE после заголовка.
            let digits: Vec<u8> =
                text.get(2..).unwrap_or(&[]).chunks(2).filter_map(|pair| hex(pair[0])).collect();
            if digits.len() >= 12 {
                for (index, byte) in card.mac.iter_mut().enumerate() {
                    *byte = (digits[index * 2] << 4) | digits[index * 2 + 1];
                }
            }
        }
        if card.mac == [0; 6] {
            // Адреса нет — местный (бит 1 первого байта), из номера подъёма.
            card.mac = [0x02, 0x46, 0x4F, 0x53, 0x00, 0x01];
        }
        // SET_ETHERNET_PACKET_FILTER: свои, широковещательные, все групповые.
        let _ = xhci::net_control(handle, [0x21, 0x43, 0x0E, 0, control, 0, 0, 0], &[], 0);
        Ok(card)
    }

    /// Команда RNDIS: `SEND_ENCAPSULATED_COMMAND` и ответ из
    /// `GET_ENCAPSULATED_RESPONSE`. `fields` — слова после `RequestId`.
    fn rndis_command(&mut self, kind: u32, fields: &[u32]) -> Result<Vec<u8>, Error> {
        self.request = self.request.wrapping_add(1);
        let mut message = Vec::with_capacity(12 + fields.len() * 4);
        message.extend_from_slice(&kind.to_le_bytes());
        message.extend_from_slice(&((12 + fields.len() * 4) as u32).to_le_bytes());
        message.extend_from_slice(&self.request.to_le_bytes());
        for field in fields {
            message.extend_from_slice(&field.to_le_bytes());
        }
        let length = (message.len() as u16).to_le_bytes();
        xhci::net_control(self.handle, [0x21, 0x00, 0, 0, self.control, 0, length[0], length[1]], &message, 0)?;
        // Ответ спрашивается опросом: уведомление RESPONSE_AVAILABLE по точке
        // прерываний говорит лишь «спроси», и спрашивать можно и без него.
        for _ in 0..RESPONSE_ATTEMPTS {
            let reply = xhci::net_control(self.handle, [0xA1, 0x01, 0, 0, self.control, 0, 0x00, 0x04], &[], 1024)?;
            if reply.len() >= 16 && u32_at(&reply, 8) == self.request {
                let status = u32_at(&reply, 12);
                if status != 0 {
                    return Err(Error::Refused(kind, status));
                }
                return Ok(reply);
            }
            crate::usb::sleep_ms(5);
        }
        Err(Error::NoResponse(kind))
    }

    /// Разобрать пришедшую передачу в кадры.
    fn unpack(&mut self, data: &[u8]) {
        match self.protocol {
            NetProtocol::Ecm => self.pending.push_back(data.to_vec()),
            NetProtocol::Rndis => {
                let mut at = 0;
                while at + RNDIS_HEADER <= data.len() {
                    let kind = u32_at(data, at);
                    let length = u32_at(data, at + 4) as usize;
                    if length < RNDIS_HEADER || at + length > data.len() {
                        self.stats.rx_dropped += 1;
                        break;
                    }
                    if kind == RNDIS_PACKET_MSG {
                        let offset = u32_at(data, at + 8) as usize;
                        let len = u32_at(data, at + 12) as usize;
                        let start = at + 8 + offset;
                        if start + len <= at + length && len <= FRAME_MAX {
                            self.pending.push_back(data[start..start + len].to_vec());
                        } else {
                            self.stats.rx_dropped += 1;
                        }
                    }
                    at += length;
                }
            }
        }
    }
}

fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

impl Card for UsbNet {
    fn name(&self) -> &'static str {
        match self.protocol {
            NetProtocol::Rndis => "usb-rndis",
            NetProtocol::Ecm => "usb-ecm",
        }
    }

    fn mac(&self) -> [u8; 6] {
        self.mac
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), CardError> {
        let result = match self.protocol {
            NetProtocol::Ecm => xhci::net_send(self.handle, frame, true),
            NetProtocol::Rndis => {
                let mut message = alloc::vec![0u8; RNDIS_HEADER + frame.len()];
                message[0..4].copy_from_slice(&RNDIS_PACKET_MSG.to_le_bytes());
                let total = message.len() as u32;
                message[4..8].copy_from_slice(&total.to_le_bytes());
                // DataOffset — от поля DataOffset, то есть 44 - 8.
                message[8..12].copy_from_slice(&((RNDIS_HEADER - 8) as u32).to_le_bytes());
                message[12..16].copy_from_slice(&(frame.len() as u32).to_le_bytes());
                message[RNDIS_HEADER..].copy_from_slice(frame);
                xhci::net_send(self.handle, &message, false)
            }
        };
        match result {
            Ok(()) => {
                self.stats.tx_frames += 1;
                self.stats.tx_bytes += frame.len() as u64;
                Ok(())
            }
            Err(NetError::Busy) => {
                self.stats.tx_dropped += 1;
                Err(CardError::Busy)
            }
            Err(NetError::TooLong) => Err(CardError::TooLong(frame.len())),
            Err(NetError::Gone) => Err(CardError::Stopped),
        }
    }

    fn receive(&mut self, frame: &mut [u8; FRAME_MAX]) -> Option<usize> {
        if self.pending.is_empty() {
            let mut buffer = core::mem::take(&mut self.buffer);
            if let Ok(Some(len)) = xhci::net_receive(self.handle, &mut buffer) {
                self.unpack(&buffer[..len]);
            }
            self.buffer = buffer;
        }
        let next = self.pending.pop_front()?;
        let len = next.len().min(FRAME_MAX);
        frame[..len].copy_from_slice(&next[..len]);
        self.stats.rx_frames += 1;
        self.stats.rx_bytes += len as u64;
        Some(len)
    }

    fn stats(&self) -> Stats {
        self.stats
    }
}
