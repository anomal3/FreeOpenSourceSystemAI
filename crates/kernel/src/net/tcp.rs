//! TCP: формат сегмента. Автомат состояний живёт в [`super::stream`].
//!
//! # Что в этом заголовке важно понимать
//!
//! Двадцать байт, и почти каждое поле — про одно и то же: **где мы в потоке**.
//! Номер последовательности говорит, каким по счёту байтом начинается этот
//! сегмент; номер подтверждения — какой байт мы ждём следующим от собеседника.
//! Не «сколько получено» и не «какой сегмент», а именно номер **байта**: TCP не
//! знает о сегментах ничего, они существуют только на проводе.
//!
//! Отсюда же следует, что `SYN` и `FIN` занимают по одному номеру, хотя байтов
//! данных не несут. Это не причуда формата: без этого подтвердить их было бы
//! нечем, и установление связи не отличалось бы от потерянного пакета.
//!
//! # Про арифметику номеров
//!
//! Номера — 32-битные и переполняются, поэтому сравнивать их обычным `<`
//! нельзя: поток длиной больше четырёх гигабайт перескакивает через ноль, и
//! «меньше» перестаёт значить «раньше». Правильное сравнение — вычитание со
//! знаком, и оно вынесено в [`before`] с [`after`], чтобы никому не пришло в
//! голову написать `a < b` там, где имелось в виду «a раньше b».
//!
//! # Контрольная сумма — как у UDP
//!
//! По псевдозаголовку с адресами и по всему сегменту. В отличие от UDP, здесь
//! она **обязательна**: ноль в этом поле означает не «не считалась», а
//! испорченный сегмент.

use crate::net::ipv4::{self, Ipv4};

/// Длина заголовка без опций.
pub const HEADER: usize = 20;

/// Наибольшая порция данных в одном сегменте.
///
/// 1460 — это 1500 байт MTU минус заголовки IPv4 и TCP. Значение объявляется
/// собеседнику опцией MSS при установлении связи; чужое объявление мы читаем и
/// уважаем — сегмент длиннее того, что готов принять получатель, он вправе
/// отбросить целиком.
pub const MSS: usize = 1460;

// --- флаги -------------------------------------------------------------------

pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// Опция: наибольший размер сегмента.
const OPTION_MSS: u8 = 2;
/// Опция: конец списка.
const OPTION_END: u8 = 0;
/// Опция: пропуск (выравнивание).
const OPTION_NOP: u8 = 1;

/// Разобранный сегмент.
pub struct Segment<'a> {
    pub source_port: u16,
    pub destination_port: u16,
    pub sequence: u32,
    pub acknowledgement: u32,
    pub flags: u8,
    pub window: u16,
    /// Что объявил собеседник опцией MSS, если объявил.
    pub mss: Option<u16>,
    pub payload: &'a [u8],
}

impl Segment<'_> {
    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// Сколько номеров занимает сегмент: данные плюс по одному за `SYN` и `FIN`.
    pub fn span(&self) -> u32 {
        let mut span = self.payload.len() as u32;
        if self.has(SYN) {
            span += 1;
        }
        if self.has(FIN) {
            span += 1;
        }
        span
    }
}

/// Разобрать сегмент, проверив сумму по псевдозаголовку.
pub fn parse<'a>(source: Ipv4, destination: Ipv4, message: &'a [u8]) -> Option<Segment<'a>> {
    if message.len() < HEADER {
        return None;
    }
    let offset = usize::from(message[12] >> 4) * 4;
    if offset < HEADER || offset > message.len() {
        return None;
    }
    if checksum(source, destination, message) != 0 {
        return None;
    }

    // Опции разбираются только ради MSS: всё остальное, что там бывает
    // (масштаб окна, выборочные подтверждения, отметки времени), мы не
    // объявляли и потому получить не должны — а если получим, честно
    // пропустим, а не сделаем вид, что поняли.
    let mut mss = None;
    let mut at = HEADER;
    while at < offset {
        match message[at] {
            OPTION_END => break,
            OPTION_NOP => at += 1,
            kind => {
                if at + 1 >= offset {
                    break;
                }
                let length = usize::from(message[at + 1]);
                if length < 2 || at + length > offset {
                    break;
                }
                if kind == OPTION_MSS && length == 4 {
                    mss = Some(u16::from_be_bytes([message[at + 2], message[at + 3]]));
                }
                at += length;
            }
        }
    }

    Some(Segment {
        source_port: u16::from_be_bytes([message[0], message[1]]),
        destination_port: u16::from_be_bytes([message[2], message[3]]),
        sequence: u32::from_be_bytes([message[4], message[5], message[6], message[7]]),
        acknowledgement: u32::from_be_bytes([message[8], message[9], message[10], message[11]]),
        flags: message[13],
        window: u16::from_be_bytes([message[14], message[15]]),
        mss,
        payload: &message[offset..],
    })
}

/// Что нужно, чтобы собрать сегмент.
pub struct Outgoing<'a> {
    pub source_port: u16,
    pub destination_port: u16,
    pub sequence: u32,
    pub acknowledgement: u32,
    pub flags: u8,
    pub window: u16,
    /// Объявить свой MSS. Делается только в сегментах с `SYN` — в остальных
    /// опция не имеет смысла и будет отброшена собеседником.
    pub with_mss: bool,
    pub payload: &'a [u8],
}

/// Собрать сегмент в буфер и вернуть его длину.
pub fn write(buffer: &mut [u8], source: Ipv4, destination: Ipv4, out: &Outgoing<'_>) -> usize {
    let options = if out.with_mss { 4 } else { 0 };
    let offset = HEADER + options;
    let length = offset + out.payload.len();

    buffer[0..2].copy_from_slice(&out.source_port.to_be_bytes());
    buffer[2..4].copy_from_slice(&out.destination_port.to_be_bytes());
    buffer[4..8].copy_from_slice(&out.sequence.to_be_bytes());
    buffer[8..12].copy_from_slice(&out.acknowledgement.to_be_bytes());
    // Старшие четыре бита — длина заголовка в 32-битных словах. Ошибка здесь
    // означает, что собеседник прочтёт часть данных как заголовок.
    buffer[12] = ((offset / 4) as u8) << 4;
    buffer[13] = out.flags;
    buffer[14..16].copy_from_slice(&out.window.to_be_bytes());
    buffer[16..18].copy_from_slice(&[0, 0]);
    // Указатель срочных данных. Срочных данных у нас не бывает, и флага `URG`
    // мы не ставим, поэтому поле всегда ноль.
    buffer[18..20].copy_from_slice(&[0, 0]);

    if out.with_mss {
        buffer[HEADER] = OPTION_MSS;
        buffer[HEADER + 1] = 4;
        buffer[HEADER + 2..HEADER + 4].copy_from_slice(&(MSS as u16).to_be_bytes());
    }
    buffer[offset..length].copy_from_slice(out.payload);

    let sum = checksum(source, destination, &buffer[..length]);
    buffer[16..18].copy_from_slice(&sum.to_be_bytes());
    length
}

/// Сумма по псевдозаголовку и сегменту.
fn checksum(source: Ipv4, destination: Ipv4, message: &[u8]) -> u16 {
    let source = source.to_bytes();
    let destination = destination.to_bytes();
    let length = (message.len() as u16).to_be_bytes();
    let pseudo = [
        source[0],
        source[1],
        source[2],
        source[3],
        destination[0],
        destination[1],
        destination[2],
        destination[3],
        0,
        ipv4::PROTOCOL_TCP,
        length[0],
        length[1],
    ];
    ipv4::checksum(&[&pseudo, message])
}

/// Раньше ли `a`, чем `b`, в кольцевой арифметике номеров.
pub fn before(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

/// Позже ли `a`, чем `b`.
pub fn after(a: u32, b: u32) -> bool {
    before(b, a)
}

/// Лежит ли `value` в полуинтервале `[low, high)` кольцевой арифметики.
pub fn between(value: u32, low: u32, high: u32) -> bool {
    !before(value, low) && before(value, high)
}
