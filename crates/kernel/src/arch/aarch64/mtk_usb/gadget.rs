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
use crate::arch::aarch64::timer;
use super::musb::{
    CSR0_DATAEND, CSR0_RXPKTRDY, CSR0_SENDSTALL, CSR0_SETUPEND, CSR0_SVDRXPKTRDY,
    CSR0_SVDSETUPEND, CSR0_TXPKTRDY, DCM_DISABLE, DEVCTL_SESSION, INTRUSBE_RESET, INTR_RESET,
    INTRTX_EP0, POWER_HSENAB, POWER_HSMODE, POWER_SOFTCONN, RXCSR_CLRDATATOG, RXCSR_FLUSHFIFO,
    RXCSR_RXPKTRDY, TXCSR_CLRDATATOG, TXCSR_FLUSHFIFO, TXCSR_MODE, TXCSR_TXPKTRDY,
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

/// Мгновенный снимок регистров для отчёта на экране.
///
/// Существует потому, что у этой машины один канал наружу и он шириной в
/// фотографию. Каждое лишнее число здесь бесплатно, каждое недостающее стоит
/// перезагрузки, снимка и получаса.
pub struct Snapshot {
    pub power: u8,
    pub csr0: u16,
    pub count0: u8,
    pub faddr: u8,
    pub intrtx: u16,
    pub swrst: u8,
}

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
    /// Подтяжка поднята. См. [`Gadget::prepare`].
    announced: bool,
    /// Сколько раз хост сбрасывал шину.
    pub resets: u16,
    /// Сколько ответов мы отправили хосту.
    ///
    /// Вместе с [`Gadget::setups`] это вторая половина того же вопроса:
    /// «спросили» и «ответили» — разные числа, и расхождение между ними
    /// показывает, на чём именно обрывается разговор.
    pub replies: u16,
    /// Сколько запросов пришло на нулевую точку.
    ///
    /// Счётчики существуют ради одного вопроса, на который нельзя ответить
    /// снаружи: доходят ли до нас запросы вообще. Ноль здесь и «устройство не
    /// отвечает» на хосте — это неисправность в шине; ненулевое здесь и то же
    /// самое на хосте — неисправность в наших ответах. Одно число разделяет два
    /// расследования, каждое из которых стоит вечера.
    pub setups: u16,
    /// Код последнего запроса и его старший байт значения — то есть какой
    /// именно дескриптор просили последним, прежде чем всё встало.
    pub last: (u8, u8),
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
            announced: false,
            resets: 0,
            replies: 0,
            setups: 0,
            last: (0, 0),
        }
    }

    /// Настроить точки и открыть сессию — но **не** заявлять о себе шине.
    ///
    /// Подтяжка линии данных поднимается отдельно, из [`Gadget::poll`], и это
    /// главный урок первой проверки на аппарате. Она означает «я здесь,
    /// спрашивайте», и хост начинает спрашивать немедленно. Поднятая здесь, она
    /// заставала ядро занятым: сразу после USB поднимается тачскрин, а это
    /// обход семи шин, ручной обмен по битам и чтения — секунды, в которые
    /// отвечать было некому. Windows показывала «Device Descriptor Request
    /// Failed», и второй попытки хост не делает.
    ///
    /// Теперь заявление о себе и готовность слушать — один и тот же миг.
    pub fn prepare(&mut self) {
        self.quiesce();

        // Контроллеру запрещается гасить собственные такты. Строка взята из
        // `musb_start` изготовителя, где она стоит без объяснений; смысл её,
        // однако, виден по последствиям — блок, засыпающий по своему
        // усмотрению, отвечает шине через раз, а перечисление через раз не
        // проходит вовсе: хост спрашивает трижды и уходит навсегда.
        let dcm = self.musb.read32(reg::DCM);
        self.musb.write32(reg::DCM, dcm | DCM_DISABLE);

        self.configure_endpoints();
        self.musb.write8(reg::DEVCTL, DEVCTL_SESSION);

        // Из признаков шины в режиме устройства слушается только сброс — так
        // же, как у изготовителя.
        self.musb.write8(reg::INTRUSBE, INTRUSBE_RESET);

        // Скорость объявляется записью **целиком**, а не поверх прочитанного.
        // Прочитанное содержало разряд «высокая скорость состоялась» — тот,
        // что контроллер выставляет сам и который мы не вправе ему сообщать.
        self.musb.write8(reg::POWER, POWER_HSENAB);
    }

    /// Привести контроллер в известное состояние.
    ///
    /// Загрузчик пользовался этим блоком для своего fastboot и оставил его как
    /// придётся: с разрешёнными признаками и неразобранной очередью событий.
    /// Начинать поверх чужого состояния — значит получить чужое событие вместо
    /// первого своего.
    fn quiesce(&self) {
        self.musb.write8(reg::INTRUSBE, 0);
        self.musb.write16(reg::INTRTXE, 0);
        self.musb.write16(reg::INTRRXE, 0);
        // Признаки сбрасываются записью единиц: всё, что накопилось до нас,
        // объявляется прочитанным.
        self.musb.write16(reg::INTRTX, 0xffff);
        self.musb.write16(reg::INTRRX, 0xffff);
        self.musb.write8(reg::INTRUSB, 0xef);
    }

    /// Поднять подтяжку. Вызывается один раз, первым же проходом опроса.
    fn announce(&mut self) {
        self.musb
            .write8(reg::POWER, POWER_HSENAB | POWER_SOFTCONN);
        self.announced = true;
    }

    /// Один проход обмена. Вызывать часто; ничего не ждёт.
    pub fn poll(&mut self) {
        if !self.announced {
            self.announce();
        }

        let events = self.musb.read8(reg::INTRUSB);
        if events != 0 {
            // Признаки снимаются **записью** прочитанного, а не самим чтением.
            //
            // Разница стоила вечера и выглядела как неисправность где угодно,
            // только не здесь. Незанятый разряд «хост сбросил шину» остаётся
            // взведённым навсегда, а значит сброс обрабатывается заново каждую
            // миллисекунду: адрес обнуляется, настройка объявляется забытой,
            // точки перенастраиваются. Перечисление разваливалось ровно в тот
            // миг, когда начинало получаться, — хост успевал спросить
            // дескриптор и даже получить ответ, после чего устройство теряло
            // о себе всё. Счётчик сбросов, дойдя до предела разрядной сетки,
            // это и показал.
            self.musb.write8(reg::INTRUSB, events);
            self.seen |= events;
            if events & INTR_RESET != 0 {
                self.on_reset();
            }
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
        self.resets = self.resets.saturating_add(1);
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
        //
        // Признак завершения — разряд нулевой точки в регистре передающих
        // точек, и ничто другое. Прежняя редакция смотрела на регистр
        // состояния самой точки, а он чист **и до завершающей стадии, и
        // после**: разницы между «ещё не ответили» и «уже ответили» в нём нет.
        // Адрес поэтому записывался на миллисекунду раньше времени, и хост
        // сообщал ровно то, что происходило, — «не удалось назначить адрес».
        if let Some(address) = self.pending_address {
            if self.musb.read16(reg::INTRTX) & INTRTX_EP0 != 0 {
                self.musb.write16(reg::INTRTX, INTRTX_EP0);
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
                self.send_next();
            }
            return;
        }

        if csr & CSR0_RXPKTRDY == 0 {
            return;
        }

        // Восемь разрядов, а не шестнадцать. См. [`reg::COUNT0`].
        let count = usize::from(self.musb.read8(reg::COUNT0));
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
        self.setups = self.setups.saturating_add(1);
        self.last = (setup[1], setup[3]);
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
                // Признак нулевой точки снимается **до** подтверждения: ждать
                // предстоит именно ту завершающую стадию, которая начнётся
                // сейчас, а не остаток предыдущей.
                self.musb.write16(reg::INTRTX, INTRTX_EP0);
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

        // Запрос подтверждается **отдельной записью**, до того как ответ
        // ляжет в память точки, — и дальше контроллеру дают время развернуть
        // точку с приёма на передачу.
        //
        // Это не осторожность, а условие работы, и стоило оно вечера.
        // Подтверждение, отправленное вместе с данными, означает, что данные
        // кладутся в память, ещё стоящую на приём: запись проходит, ошибки
        // нет, ответ не уходит никуда. Снаружи это выглядит как устройство,
        // которое видно на шине и молчит на первый же вопрос. Изготовитель
        // делает так же и объясняет тем, что «контроллеру нужен миг на смену
        // режима».
        self.musb.write16(reg::CSR0, CSR0_SVDRXPKTRDY);
        self.replies = self.replies.saturating_add(1);
        self.await_receive_cleared();

        self.send_next();
    }

    /// Дождаться, пока контроллер снимет признак принятого пакета.
    ///
    /// Предел ожидания короткий и намеренно: это происходит за микросекунды, а
    /// затянувшееся ожидание означает не «сейчас получится», а «уже не
    /// получится» — и висеть в нём означало бы не ответить и на все следующие
    /// запросы тоже.
    fn await_receive_cleared(&self) {
        for _ in 0..1000 {
            if self.musb.read16(reg::CSR0) & CSR0_RXPKTRDY == 0 {
                return;
            }
            timer::delay_us(10);
        }
    }

    /// Отправить очередную порцию ответа.
    ///
    /// Подтверждения запроса здесь нет: оно отдано отдельно и раньше, в
    /// [`Gadget::start_reply`], — см. объяснение там.
    fn send_next(&mut self) {
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

    /// Подключение и состояние нулевой точки прямо сейчас.
    ///
    /// Второе разделяет два очень разных отказа, снаружи неразличимых: разряд
    /// «пришёл пакет» во взведённом состоянии означает, что хост нас спросил, а
    /// мы не ответили; его отсутствие — что не спросил.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.musb.select(0);
        Snapshot {
            power: self.musb.read8(reg::POWER),
            csr0: self.musb.read16(reg::CSR0),
            count0: self.musb.read8(reg::COUNT0),
            faddr: self.musb.read8(reg::FADDR),
            intrtx: self.musb.read16(reg::INTRTX),
            swrst: self.musb.read8(reg::BUSPERF3),
        }
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

    /// Выбросить пакет, который хост так и не забрал.
    ///
    /// Нужно ровно в одном случае: собеседник ушёл посреди разговора. Пакет,
    /// оставшийся в памяти точки, ждёт вечно, и следующий разговор начинается с
    /// чужого хвоста — а точнее, не начинается вовсе.
    pub fn flush(&mut self) {
        self.musb.select(BULK);
        self.musb
            .write16(reg::TXCSR, TXCSR_MODE | TXCSR_FLUSHFIFO);
        self.musb.write16(reg::TXCSR, TXCSR_MODE);
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
