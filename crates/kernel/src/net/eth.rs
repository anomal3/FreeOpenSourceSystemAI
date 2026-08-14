//! Ethernet: четырнадцать байт перед всем остальным.
//!
//! Контрольной суммы кадра (FCS) здесь нет и быть не должно: её считает и
//! проверяет само устройство, а до драйвера кадр доезжает уже без неё. Кадр,
//! в конец которого дописали четыре байта «на всякий случай», сеть примет — и
//! отбросит на другой стороне.

/// Аппаратный адрес.
pub type Mac = [u8; 6];

/// Широковещательный адрес: кадр получают все.
pub const BROADCAST: Mac = [0xFF; 6];

/// Длина заголовка.
pub const HEADER: usize = 14;

/// Тип содержимого: IPv4.
pub const TYPE_IPV4: u16 = 0x0800;
/// Тип содержимого: ARP.
pub const TYPE_ARP: u16 = 0x0806;

/// Разобранный заголовок и то, что за ним лежит.
///
/// Адреса отправителя здесь нет намеренно. Он в кадре, конечно, есть, но
/// пользоваться им нельзя: единственное осмысленное применение — запомнить его
/// в таблице ARP, а адрес отправителя кадра и адрес владельца IP-адреса внутри
/// пакета совпадают не всегда. Соответствия берутся только из ARP, где они
/// заявлены явно.
pub struct Frame<'a> {
    pub destination: Mac,
    pub kind: u16,
    pub payload: &'a [u8],
}

/// Разобрать кадр.
///
/// Возвращает `None`, если байт меньше, чем один заголовок: кадр короче
/// четырнадцати байт не бывает, и попытка прочитать из него тип содержимого
/// означала бы чтение за концом среза.
pub fn parse(frame: &[u8]) -> Option<Frame<'_>> {
    if frame.len() < HEADER {
        return None;
    }
    let mut destination = [0u8; 6];
    destination.copy_from_slice(&frame[0..6]);
    Some(Frame {
        destination,
        kind: u16::from_be_bytes([frame[12], frame[13]]),
        payload: &frame[HEADER..],
    })
}

/// Записать заголовок в начало буфера.
///
/// Порядок байт в поле типа — сетевой (старший первым), как и во всём
/// остальном, что уходит на провод.
pub fn write_header(buffer: &mut [u8], destination: Mac, source: Mac, kind: u16) {
    buffer[0..6].copy_from_slice(&destination);
    buffer[6..12].copy_from_slice(&source);
    buffer[12..14].copy_from_slice(&kind.to_be_bytes());
}

/// Напечатать адрес в привычном виде `52:54:00:12:34:56`.
pub struct Display(pub Mac);

impl core::fmt::Display for Display {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for (index, byte) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(":")?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
