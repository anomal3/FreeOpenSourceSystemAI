//! Устройство на шине: перечисление и две объёмные точки.
//!
//! # Чем притворяемся и почему именно этим
//!
//! Загрузчиком в режиме fastboot — `18D1:D00D`. Не из хитрости: под Windows
//! любое устройство требует драйвера, и написать его — отдельная задача с
//! подписью и установкой. Драйвер для этой пары на машине уже стоит и проверен
//! работой, а программа `fastboot` уже лежит рядом с `adb`. То есть весь
//! хостовой конец задачи уже решён, и остаётся только устройство.
//!
//! Побочная выгода крупнее основной: `fastboot devices` показывает аппарат, ещё
//! не отправив ему ни одной команды. Это первый признак жизни, и он не требует
//! ни одной нашей строчки сверх дескрипторов.
//!
//! # Опрос вместо прерываний
//!
//! Весь обмен ведётся из [`Gadget::poll`], который вызывают часто. Хост терпит:
//! пока мы не ответили, контроллер сам отвечает «занят», и запрос повторяется.
//! Единственное место, где это важно, — назначение адреса, и там ожидание
//! оговорено отдельно.

use super::fastboot::Fastboot;
use super::musb::{Musb, reg};
use super::musb::{
    CSR0_DATAEND, CSR0_RXPKTRDY, CSR0_SENDSTALL, CSR0_SETUPEND, CSR0_SVDRXPKTRDY,
    CSR0_SVDSETUPEND, CSR0_TXPKTRDY, DEVCTL_SESSION, INTR_RESET, POWER_HSENAB, POWER_HSMODE,
    POWER_SOFTCONN, RXCSR_CLRDATATOG, RXCSR_FLUSHFIFO, RXCSR_RXPKTRDY, TXCSR_CLRDATATOG,
    TXCSR_FLUSHFIFO, TXCSR_MODE, TXCSR_TXPKTRDY,
};

/// Наибольший пакет нулевой точки. Не настраивается — так устроен контроллер.
const EP0_MAX: usize = 64;
/// Номер объёмной точки. Одна пара: приём и передача под общим номером.
pub const BULK: u8 = 1;
/// Адрес передающей точки в дескрипторе (старший бит — «к хосту»).
const EP_IN: u8 = 0x80 | BULK;
/// Адрес принимающей.
const EP_OUT: u8 = BULK;

/// Кем мы называемся. Числа выбраны за уже существующий на хосте драйвер.
const VENDOR: u16 = 0x18d1;
const PRODUCT: u16 = 0xd00d;

/// Строки, на которые ссылаются дескрипторы. Порядок задаёт номера.
const STRINGS: [&str; 3] = ["FreeOS", "FreeOS phone", "dandelion"];

/// Наибольший ответ на нулевой точке.
///
/// Самый длинный из наших — дескриптор настройки, тридцать два байта. Запас
/// нужен не им, а строкам: каждая буква занимает два байта, и «FreeOS phone»
/// уже двадцать шесть.
const REPLY_MAX: usize = 128;

/// Незавершённый ответ хосту на нулевой точке.
struct Reply {
    data: [u8; REPLY_MAX],
    len: usize,
    sent: usize,
    /// Ответ оказался короче запрошенного, но кратен размеру пакета. Хост
    /// не может отличить «это всё» от «продолжение будет» иначе как по пакету
    /// короче полного, и такой пакет придётся отправить пустым.
    zero_packet: bool,
}

/// Состояние устройства на шине.
pub struct Gadget {
    musb: Musb,
    /// Шина договорилась на высокую скорость. Известно только после сброса.
    high_speed: bool,
    /// Хост выбрал настройку — с этого мига объёмные точки работают.
    configured: bool,
    /// Адрес, назначенный хостом и ещё не применённый. См. [`Gadget::poll`].
    pending_address: Option<u8>,
    reply: Option<Reply>,
    /// Протокол поверх объёмных точек.
    fastboot: Fastboot,
    /// Признаки шины, замеченные с запуска. Только для отчёта на экране: по
    /// ним видно, дошло ли до нас хоть что-нибудь от хоста.
    pub seen: u8,
}

impl Gadget {
    pub fn new(musb: Musb) -> Self {
        Self {
            musb,
            high_speed: false,
            configured: false,
            pending_address: None,
            reply: None,
            fastboot: Fastboot::new(),
            seen: 0,
        }
    }

    /// Настроить точки и подключиться к шине.
    ///
    /// Порядок обязателен, и он ровно обратный интуиции: подтяжка линии данных
    /// ставится **последней**. Она означает «я здесь, спрашивайте», и заявить
    /// это раньше, чем точки готовы, значит получить запрос, на который нечем
    /// ответить, — а неотвеченное устройство хост объявляет неисправным, и
    /// второй попытки уже не будет.
    pub fn attach(&mut self) {
        self.configure_endpoints();
        self.musb.write8(reg::DEVCTL, DEVCTL_SESSION);
        let power = self.musb.read8(reg::POWER);
        self.musb.write8(reg::POWER, power | POWER_HSENAB);
        self.musb
            .write8(reg::POWER, power | POWER_HSENAB | POWER_SOFTCONN);
    }

    /// Один проход обмена. Вызывать часто; ничего не ждёт.
    pub fn poll(&mut self) {
        let events = self.musb.read8(reg::INTRUSB);
        self.seen |= events;
        if events & INTR_RESET != 0 {
            self.on_reset();
        }

        self.poll_control();

        if self.configured {
            self.fastboot.poll(&mut Bulk { musb: self.musb });
        }
    }

    /// Хост сбросил шину: адрес забыт, настройка забыта, скорость известна.
    ///
    /// Скорость читается **здесь и только здесь**. До сброса разряд `HSMODE` —
    /// это наше пожелание, а не итог переговоров, и настроенные по нему точки
    /// на полной скорости ждали бы пакетов, которых не бывает.
    fn on_reset(&mut self) {
        self.musb.write8(reg::FADDR, 0);
        self.high_speed = self.musb.read8(reg::POWER) & POWER_HSMODE != 0;
        self.configured = false;
        self.pending_address = None;
        self.reply = None;
        self.fastboot.reset();
        self.configure_endpoints();
    }

    fn poll_control(&mut self) {
        self.musb.select(0);
        let csr = self.musb.read16(reg::CSR0);

        // Адрес применяется только когда завершающая стадия позади. Хост
        // запрашивает её по **старому** адресу, и поменять его раньше — значит
        // не ответить на собственное подтверждение: устройство назначенного
        // адреса не получит, а прежнего у него уже нет.
        if let Some(address) = self.pending_address {
            if csr & (CSR0_RXPKTRDY | CSR0_TXPKTRDY) == 0 {
                self.musb.write8(reg::FADDR, address);
                self.pending_address = None;
            }
        }

        if csr & CSR0_SETUPEND != 0 {
            // Хост передумал посреди запроса. Незаконченный ответ выбрасывается
            // целиком: досылать его хвост некуда.
            self.musb.write16(reg::CSR0, CSR0_SVDSETUPEND);
            self.reply = None;
            return;
        }

        if self.reply.is_some() {
            if csr & CSR0_TXPKTRDY == 0 {
                self.send_next(false);
            }
            return;
        }

        if csr & CSR0_RXPKTRDY == 0 {
            return;
        }

        let count = usize::from(self.musb.read16(reg::COUNT0));
        if count != 8 {
            // Восемь байт — длина запроса, другой здесь не бывает. Что бы это
            // ни было, память точки надо освободить, иначе она встанет.
            let mut discard = [0u8; EP0_MAX];
            let take = count.min(EP0_MAX);
            self.musb.read_fifo(0, &mut discard[..take]);
            self.musb.write16(reg::CSR0, CSR0_SVDRXPKTRDY);
            return;
        }

        let mut setup = [0u8; 8];
        self.musb.read_fifo(0, &mut setup);
        self.handle_setup(&setup);
    }

    fn handle_setup(&mut self, setup: &[u8; 8]) {
        // Коды запросов уникальны только внутри своего вида. Классовый или
        // изготовительский запрос с кодом `0x06` — не «дай дескриптор», и
        // ответить на него дескриптором значит отдать хосту не то, что он
        // просил, вместо честного отказа. Мы объявлены как устройство без
        // класса, и своих запросов у нас нет: всё, кроме стандартных, — чужое.
        if setup[0] & 0x60 != 0 {
            self.stall();
            return;
        }

        let direction_in = setup[0] & 0x80 != 0;
        let request = setup[1];
        let value = u16::from_le_bytes([setup[2], setup[3]]);
        let length = usize::from(u16::from_le_bytes([setup[6], setup[7]]));

        let mut data = [0u8; REPLY_MAX];

        match (direction_in, request) {
            // GET_DESCRIPTOR
            (true, 0x06) => {
                let kind = (value >> 8) as u8;
                let number = (value & 0xff) as u8;
                match self.descriptor(kind, number, &mut data) {
                    Some(len) => self.start_reply(&data, len, length),
                    None => self.stall(),
                }
            }
            // GET_STATUS: питаемся от шины, удалённого пробуждения не умеем.
            (true, 0x00) => self.start_reply(&[0, 0], 2, length),
            // GET_CONFIGURATION
            (true, 0x08) => {
                let current = u8::from(self.configured);
                self.start_reply(&[current], 1, length);
            }
            // GET_INTERFACE: настройка одна, других значений не бывает.
            (true, 0x0a) => self.start_reply(&[0], 1, length),
            // SET_ADDRESS
            (false, 0x05) => {
                self.pending_address = Some((value & 0x7f) as u8);
                self.ack();
            }
            // SET_CONFIGURATION
            (false, 0x09) => {
                let was_configured = self.configured;
                self.configured = value != 0;
                self.configure_endpoints();
                self.fastboot.reset();
                // Единственная строка, которую стоит сказать вслух на экране:
                // до неё человек не знает, заработал кабель или нет, и узнать
                // это может только тем самым снимком экрана, ради отмены
                // которого всё и писалось.
                if self.configured && !was_configured {
                    let speed = if self.high_speed { "high" } else { "full" };
                    crate::kprintln!(
                        "  usb         : the host enumerated us at {speed} speed -- `fastboot oem log` works now"
                    );
                }
                self.ack();
            }
            // SET_INTERFACE и снятие признаков: соглашаемся, делать нечего.
            (false, 0x01 | 0x03 | 0x0b) => self.ack(),
            _ => self.stall(),
        }
    }

    /// Собрать дескриптор в `out`, вернув его длину.
    fn descriptor(&self, kind: u8, number: u8, out: &mut [u8; REPLY_MAX]) -> Option<usize> {
        match kind {
            1 => Some(self.device_descriptor(out)),
            2 => Some(self.configuration_descriptor(out)),
            3 => string_descriptor(number, out),
            // Дескриптор «на другой скорости». Устройство с высокой скоростью
            // обязано на него отвечать: без ответа хост считает нас
            // неисправными и на части машин отказывается перечислять.
            6 => {
                out[..10].copy_from_slice(&[10, 6, 0x00, 0x02, 0, 0, 0, EP0_MAX as u8, 1, 0]);
                Some(10)
            }
            _ => None,
        }
    }

    fn device_descriptor(&self, out: &mut [u8; REPLY_MAX]) -> usize {
        let descriptor = [
            18,
            1,
            0x00,
            0x02, // договор версии 2.0
            0,
            0,
            0, // класс называет интерфейс, а не устройство
            EP0_MAX as u8,
            VENDOR as u8,
            (VENDOR >> 8) as u8,
            PRODUCT as u8,
            (PRODUCT >> 8) as u8,
            0x00,
            0x01, // версия устройства
            1,
            2,
            3, // изготовитель, изделие, серийный номер
            1, // одна настройка
        ];
        out[..18].copy_from_slice(&descriptor);
        18
    }

    fn configuration_descriptor(&self, out: &mut [u8; REPLY_MAX]) -> usize {
        // Наибольший пакет объёмной точки задаёт скорость, и написать здесь
        // 512 на полной скорости значит объявить невозможное: хост будет слать
        // по 64 и ждать подтверждения на 512, которого не будет никогда.
        let max = self.bulk_max_packet();
        let descriptor = [
            // настройка
            9,
            2,
            32,
            0, // полная длина вместе с интерфейсом и точками
            1,
            1,
            0,
            0x80, // питаемся от шины
            0xfa, // 500 мА — столько просит и заводской загрузчик
            // интерфейс
            9,
            4,
            0,
            0,
            2,
            0xff,
            0x42,
            0x03, // класс, подкласс и протокол fastboot
            0,
            // точка к хосту
            7,
            5,
            EP_IN,
            0x02,
            max as u8,
            (max >> 8) as u8,
            0,
            // точка от хоста
            7,
            5,
            EP_OUT,
            0x02,
            max as u8,
            (max >> 8) as u8,
            0,
        ];
        out[..32].copy_from_slice(&descriptor);
        32
    }

    fn bulk_max_packet(&self) -> u16 {
        if self.high_speed { 512 } else { 64 }
    }

    /// Настроить объёмные точки под текущую скорость.
    fn configure_endpoints(&self) {
        let max = self.bulk_max_packet();
        self.musb.select(BULK);

        self.musb.write16(reg::TXMAXP, max);
        // Сброс дважды: память точки может быть двойной, и одна очистка
        // оставила бы во второй половине пакет от прошлого подключения.
        self.musb
            .write16(reg::TXCSR, TXCSR_MODE | TXCSR_CLRDATATOG | TXCSR_FLUSHFIFO);
        self.musb.write16(reg::TXCSR, TXCSR_MODE | TXCSR_FLUSHFIFO);
        self.musb.write16(reg::TXCSR, TXCSR_MODE);

        self.musb.write16(reg::RXMAXP, max);
        self.musb
            .write16(reg::RXCSR, RXCSR_CLRDATATOG | RXCSR_FLUSHFIFO);
        self.musb.write16(reg::RXCSR, RXCSR_FLUSHFIFO);
        self.musb.write16(reg::RXCSR, 0);

        // Раздел памяти точек. Размер задаётся показателем степени: ноль — это
        // восемь байт, и до нужных 512 их шесть удвоений. Начало считается в
        // восьмибайтовых единицах и идёт сразу за памятью нулевой точки.
        let size_code = match max {
            512 => 6,
            _ => 3,
        };
        let start = (EP0_MAX / 8) as u16;
        self.musb.write8(reg::TXFIFOSZ, size_code);
        self.musb.write16(reg::TXFIFOADD, start);
        self.musb.write8(reg::RXFIFOSZ, size_code);
        self.musb.write16(reg::RXFIFOADD, start + max / 8);

        self.musb.select(0);
    }

    /// Начать ответ на запрос, читающий данные.
    fn start_reply(&mut self, data: &[u8], len: usize, requested: usize) {
        let len = len.min(requested).min(REPLY_MAX);
        if len == 0 {
            self.ack();
            return;
        }
        let mut buffer = [0u8; REPLY_MAX];
        buffer[..len].copy_from_slice(&data[..len]);
        self.reply = Some(Reply {
            data: buffer,
            len,
            sent: 0,
            zero_packet: len < requested && len % EP0_MAX == 0,
        });
        self.send_next(true);
    }

    /// Отправить очередную порцию ответа.
    ///
    /// `first` означает, что это ещё и подтверждение самого запроса: его надо
    /// снять ровно один раз, иначе контроллер примет следующий запрос поверх
    /// незаконченного.
    fn send_next(&mut self, first: bool) {
        let Some(reply) = self.reply.as_mut() else {
            return;
        };
        let chunk = (reply.len - reply.sent).min(EP0_MAX);
        let from = reply.sent;
        reply.sent += chunk;
        let done = reply.sent == reply.len && !reply.zero_packet;
        if chunk == 0 {
            // Это и есть тот самый пустой пакет.
            reply.zero_packet = false;
        }
        let data = reply.data;

        self.musb.write_fifo(0, &data[from..from + chunk]);

        let mut csr = CSR0_TXPKTRDY;
        if first {
            csr |= CSR0_SVDRXPKTRDY;
        }
        if done || chunk == 0 {
            csr |= CSR0_DATAEND;
            self.reply = None;
        }
        self.musb.write16(reg::CSR0, csr);
    }

    /// Согласиться с запросом, у которого нет данных.
    fn ack(&self) {
        self.musb.write16(reg::CSR0, CSR0_SVDRXPKTRDY | CSR0_DATAEND);
    }

    /// Отказать. Хост снимет отказ сам, следующим запросом.
    fn stall(&self) {
        self.musb
            .write16(reg::CSR0, CSR0_SVDRXPKTRDY | CSR0_SENDSTALL);
    }

    /// Дошло ли дело до работы: хост перечислил устройство и выбрал настройку.
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.configured
    }

    /// На какой скорости договорились.
    #[must_use]
    pub fn is_high_speed(&self) -> bool {
        self.high_speed
    }
}

/// Собрать строковый дескриптор.
///
/// Нулевой номер — не строка, а список языков: без него хост не спросит ни
/// одной остальной. `0x0409` — английский США, единственный, на котором наши
/// строки и написаны.
fn string_descriptor(number: u8, out: &mut [u8; REPLY_MAX]) -> Option<usize> {
    if number == 0 {
        out[..4].copy_from_slice(&[4, 3, 0x09, 0x04]);
        return Some(4);
    }
    let text = STRINGS.get(usize::from(number) - 1)?;
    // Буква занимает две ячейки: договор требует UTF-16, и хотя все наши буквы
    // латинские, старший байт всё равно обязан быть.
    let len = 2 + text.len() * 2;
    if len > REPLY_MAX {
        return None;
    }
    out[0] = len as u8;
    out[1] = 3;
    for (at, byte) in text.bytes().enumerate() {
        out[2 + at * 2] = byte;
        out[3 + at * 2] = 0;
    }
    Some(len)
}

/// Объёмные точки — то немногое, что нужно протоколу поверх них.
pub struct Bulk {
    musb: Musb,
}

impl Bulk {
    /// Забрать пакет от хоста, если он пришёл.
    ///
    /// Пакет длиннее приёмника дочитывается и выбрасывается: оставить хвост в
    /// памяти точки — значит выдать его за начало следующей команды.
    pub fn receive(&mut self, out: &mut [u8]) -> Option<usize> {
        self.musb.select(BULK);
        let csr = self.musb.read16(reg::RXCSR);
        if csr & RXCSR_RXPKTRDY == 0 {
            return None;
        }
        let count = usize::from(self.musb.read16(reg::RXCOUNT));
        let taken = count.min(out.len());
        self.musb.read_fifo(BULK, &mut out[..taken]);
        let mut discard = [0u8; 64];
        let mut left = count - taken;
        while left > 0 {
            let chunk = left.min(discard.len());
            self.musb.read_fifo(BULK, &mut discard[..chunk]);
            left -= chunk;
        }
        self.musb.write16(reg::RXCSR, csr & !RXCSR_RXPKTRDY);
        Some(taken)
    }

    /// Отдать пакет хосту. `false` — точка ещё занята прошлым, попробовать
    /// позже; терять при этом нечего, потому что ничего не записано.
    pub fn send(&mut self, data: &[u8]) -> bool {
        self.musb.select(BULK);
        if self.musb.read16(reg::TXCSR) & TXCSR_TXPKTRDY != 0 {
            return false;
        }
        self.musb.write_fifo(BULK, data);
        self.musb.write16(reg::TXCSR, TXCSR_MODE | TXCSR_TXPKTRDY);
        true
    }
}
