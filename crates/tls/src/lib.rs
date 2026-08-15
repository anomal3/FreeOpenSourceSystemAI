//! Клиент TLS 1.3: один набор шифров, одна группа, никаких билетов.
//!
//! # Зачем он этой системе
//!
//! Обновления идут по HTTP с подписью Ed25519 (фаза 39), и это решение в силе:
//! доверие даёт подпись под содержимым, а не канал. TLS понадобился по
//! приземлённой причине — **GitHub отдаёт только по HTTPS**, а запасной канал
//! обновлений живёт там. Система с одним сервером обновлений — это обещание,
//! которое держится на одной машине.
//!
//! # Что он умеет и чего не умеет — вслух
//!
//! Умеет: TLS 1.3 (и только 1.3), `TLS_CHACHA20_POLY1305_SHA256`, X25519,
//! проверку цепочки сертификатов с именем и сроками, `KeyUpdate` в обе стороны.
//!
//! Не умеет и не будет:
//!
//! * **AES-GCM.** ChaCha20 и Poly1305 уже есть от SSH; AES означал бы второй
//!   шифр, который нечем проверить, кроме чужого сервера. Сказать прямо: сервер,
//!   не умеющий ChaCha20, нам не ответит. На 2026 год таких среди тех, кто нам
//!   нужен, нет.
//! * **Возобновление сеанса и 0-RTT.** Билет — это состояние, которое надо где-то
//!   хранить между запусками, и 0-RTT — это данные, отправленные до того, как
//!   собеседник назвался. Трёх соединений за обновление это не ускорит.
//! * **Клиентские сертификаты.** Их у системы нет; запрос на них — внятный
//!   отказ, а не тишина.
//! * **HelloRetryRequest.** Мы предлагаем ровно одну группу, поэтому просьба
//!   выбрать другую означает «нам не по пути». Отказ обязан быть внятным, и он
//!   есть.
//! * **Проверку отзыва** (CRL, OCSP) — см. `x509::chain`, там сказано почему.
//!
//! # Как этим пользоваться
//!
//! Ввода-вывода внутри нет: сокет принадлежит вызывающему. Обмен идёт так —
//! [`Session::outgoing`] отдаёт байты, которые надо отправить, [`Session::feed`]
//! принимает пришедшие, [`Session::plaintext`] отдаёт расшифрованное.
//!
//! ```text
//!   пока не готово:  отправить outgoing() → прочитать сокет → feed()
//!   потом:           send(запрос) → outgoing() → feed() → plaintext()
//! ```

#![no_std]

pub mod hkdf;
pub mod record;
pub mod wire;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use hkdf::{HASH_LEN, derive_empty, derive_secret, expand_label, extract, hmac};
use record::{HEADER_LEN, Keys, MAX_CIPHERTEXT, MAX_PLAINTEXT, content};
use wire::{Reader, Writer};
use x509::cert::{Algorithm, Certificate, PublicKey as CertKey};
use x509::chain;
use x509::ecdsa::Curve;
use x509::hash::Hash;
use x509::store::Store;

/// Единственный набор шифров, который мы предлагаем.
const TLS_CHACHA20_POLY1305_SHA256: u16 = 0x1303;

/// Единственная группа обмена ключами.
const X25519: u16 = 0x001D;

/// Номера расширений, которые мы пишем и читаем.
mod extension {
    pub const SERVER_NAME: u16 = 0;
    pub const SUPPORTED_GROUPS: u16 = 10;
    pub const SIGNATURE_ALGORITHMS: u16 = 13;
    pub const ALPN: u16 = 16;
    pub const SIGNATURE_ALGORITHMS_CERT: u16 = 50;
    pub const SUPPORTED_VERSIONS: u16 = 43;
    pub const KEY_SHARE: u16 = 51;
}

/// Типы сообщений рукопожатия.
mod handshake {
    pub const CLIENT_HELLO: u8 = 1;
    pub const SERVER_HELLO: u8 = 2;
    pub const NEW_SESSION_TICKET: u8 = 4;
    pub const ENCRYPTED_EXTENSIONS: u8 = 8;
    pub const CERTIFICATE: u8 = 11;
    pub const CERTIFICATE_REQUEST: u8 = 13;
    pub const CERTIFICATE_VERIFY: u8 = 15;
    pub const FINISHED: u8 = 20;
    pub const KEY_UPDATE: u8 = 24;
}

/// Схемы подписи, которые мы называем в `ClientHello` и умеем проверять.
mod scheme {
    pub const ECDSA_SECP256R1_SHA256: u16 = 0x0403;
    pub const ECDSA_SECP384R1_SHA384: u16 = 0x0503;
    pub const RSA_PSS_RSAE_SHA256: u16 = 0x0804;
    pub const RSA_PSS_RSAE_SHA384: u16 = 0x0805;
    pub const RSA_PSS_RSAE_SHA512: u16 = 0x0806;
    pub const RSA_PKCS1_SHA256: u16 = 0x0401;
    pub const RSA_PKCS1_SHA384: u16 = 0x0501;
    pub const RSA_PKCS1_SHA512: u16 = 0x0601;
}

/// Хеш от строки `"HelloRetryRequest"` — им сервер помечает просьбу начать
/// заново с другой группой. Значение фиксировано стандартом (RFC 8446, §4.1.3).
const HELLO_RETRY_MAGIC: [u8; 32] = [
    0xCF, 0x21, 0xAD, 0x74, 0xE5, 0x9A, 0x61, 0x11, 0xBE, 0x1D, 0x8C, 0x02, 0x1E, 0x65, 0xB8, 0x91,
    0xC2, 0xA2, 0x11, 0x16, 0x7A, 0xBB, 0x8C, 0x5E, 0x07, 0x9E, 0x09, 0xE2, 0xC8, 0xA8, 0x33, 0x9C,
];

/// Строка, которую подписывает сервер в `CertificateVerify`.
///
/// Подписывается **не** хеш стенограммы: сначала 64 пробела, потом эта строка,
/// потом нулевой байт, и только потом хеш. Ошибка здесь выглядит как «сервер
/// прислал чужой сертификат», и искать её будут в разборе X.509.
const CERTIFICATE_VERIFY_CONTEXT: &[u8] = b"TLS 1.3, server CertificateVerify";

/// Сколько байт с провода помещается в приёмник: одна запись целиком.
const INBOX: usize = HEADER_LEN + MAX_CIPHERTEXT;

/// Сколько расшифрованного ждёт вызывающего.
const PLAIN: usize = MAX_PLAINTEXT;

/// Сколько байт можно поставить в очередь на отправку.
///
/// Запросу HTTP хватает и трёхсот байт; четыре килобайта — запас на длинный
/// адрес с подписью, которым GitHub отдаёт файлы (там подпись в запросе длиной
/// под тысячу знаков).
pub const MAX_WRITE: usize = 4 * 1024;
const OUTBOX: usize = HEADER_LEN + MAX_WRITE + 1 + record::TAG_LEN + 256;

/// Сколько места под сборку сообщений рукопожатия.
///
/// Сообщение `Certificate` с цепочкой из трёх сертификатов RSA занимает около
/// четырёх килобайт и приходит **не** одной записью: границы записей и границы
/// сообщений в TLS не совпадают, и клиент, читающий сообщение из одной записи,
/// работает ровно до первой длинной цепочки.
const HANDSHAKE: usize = 16 * 1024;

/// Сколько места под копию сертификата сервера.
const LEAF: usize = 4 * 1024;

/// Чем кончилась попытка договориться.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Сообщение пришло не тогда, когда его ждали.
    OutOfOrder,
    /// Байты не складываются в сообщение TLS.
    Malformed,
    /// Сервер прислал предупреждение или отказ.
    Alert(u8),
    /// Сервер выбрал не тот набор шифров или не ту версию.
    NotTls13,
    /// Сервер просит начать заново с другой группой; у нас она одна.
    HelloRetry,
    /// Сервер не прислал свою половину ключа X25519.
    NoKeyShare,
    /// Подпись под записью не сходится: ключи разошлись или запись подделана.
    BadRecord,
    /// `Finished` не сходится: собеседник не тот, за кого себя выдаёт.
    BadFinished,
    /// Подпись в `CertificateVerify` не сходится.
    BadSignature,
    /// Схема подписи, которую мы не проверяем.
    UnknownScheme,
    /// С цепочкой сертификатов что-то не так.
    Chain(chain::Error),
    /// Сертификат не разбирается.
    Certificate(x509::cert::Error),
    /// Сервер просит клиентский сертификат; у этой системы его нет.
    WantsClientCertificate,
    /// Сообщение длиннее, чем эта система готова собрать.
    TooLong,
    /// Записывать нечего: рукопожатие ещё не кончилось или уже закрыто.
    NotReady,
    /// Соединение закрыто собеседником.
    Closed,
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::OutOfOrder => "the server said things in an order TLS does not allow",
            Self::Malformed => "that is not a TLS message",
            Self::Alert(_) => "the server closed the conversation with an alert",
            Self::NotTls13 => "the server does not speak TLS 1.3 with the cipher we offer",
            Self::HelloRetry => "the server wants another key exchange group; this client has one",
            Self::NoKeyShare => "the server sent no key share",
            Self::BadRecord => "a record did not decrypt; the keys do not agree",
            Self::BadFinished => "the server cannot prove it holds the key it just used",
            Self::BadSignature => "the server did not sign the handshake with the key in its certificate",
            Self::UnknownScheme => "the server signed with a scheme this system cannot check",
            Self::Chain(inner) => inner.text(),
            Self::Certificate(inner) => inner.text(),
            Self::WantsClientCertificate => "the server asks for a client certificate; this system has none",
            Self::TooLong => "the server sent a longer message than this system will hold",
            Self::NotReady => "the connection is not ready to carry data",
            Self::Closed => "the connection is closed",
        }
    }
}

/// Буферы соединения.
///
/// Отдельной структурой, а не полями сеанса, ровно затем, чтобы её можно было
/// положить в статик: почти шестьдесят килобайт на стеке в шестьдесят четыре —
/// это переполнение с первого же шага.
pub struct Buffers {
    inbox: [u8; INBOX],
    inbox_start: usize,
    inbox_len: usize,
    plain: [u8; PLAIN],
    plain_start: usize,
    plain_len: usize,
    outbox: [u8; OUTBOX],
    outbox_start: usize,
    outbox_len: usize,
    handshake: [u8; HANDSHAKE],
    handshake_len: usize,
    leaf: [u8; LEAF],
    leaf_len: usize,
}

impl Default for Buffers {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffers {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            inbox: [0; INBOX],
            inbox_start: 0,
            inbox_len: 0,
            plain: [0; PLAIN],
            plain_start: 0,
            plain_len: 0,
            outbox: [0; OUTBOX],
            outbox_start: 0,
            outbox_len: 0,
            handshake: [0; HANDSHAKE],
            handshake_len: 0,
            leaf: [0; LEAF],
            leaf_len: 0,
        }
    }
}

/// На каком шаге рукопожатия мы находимся.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    ServerHello,
    EncryptedExtensions,
    Certificate,
    CertificateVerify,
    Finished,
    Ready,
    Closed,
}

/// Имя сервера — копия внутри сеанса.
///
/// Копия, а не ссылка, потому что имя приходит из разобранного файла настроек,
/// а живёт сеанс дольше: чужая строка, оставленная ссылкой, — это ровно тот
/// случай, когда компилятор согласен, а человек ошибается.
struct Host {
    buffer: [u8; 255],
    len: usize,
}

impl Host {
    fn as_str(&self) -> &str {
        // SAFETY: в буфер копируются байты из `&str`, то есть заведомо UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }
}

/// Соединение TLS.
pub struct Session<'a> {
    io: &'a mut Buffers,
    roots: Store<'a>,
    host: Host,
    /// Секунды эпохи Unix; ноль — «часы неизвестны».
    now: i64,
    state: State,
    /// Стенограмма рукопожатия: хеш всех сообщений подряд.
    transcript: Sha256,
    secret: Option<StaticSecret>,
    client_random: [u8; 32],
    session_id: [u8; 32],
    handshake_secret: [u8; HASH_LEN],
    client_handshake: [u8; HASH_LEN],
    server_handshake: [u8; HASH_LEN],
    read: Option<Keys>,
    write: Option<Keys>,
    /// Собеседник прислал `close_notify`: данных больше не будет.
    peer_closed: bool,
}

impl<'a> Session<'a> {
    /// Начать соединение: сложить `ClientHello` в очередь на отправку.
    ///
    /// `random` — 96 байт от системы: тридцать два на `client_random`,
    /// тридцать два на идентификатор сеанса и тридцать два на закрытый ключ
    /// X25519. Своего источника случайности у крейта нет намеренно: он в ядре, и
    /// брать его отсюда значило бы завести второй.
    pub fn new(
        io: &'a mut Buffers,
        roots: Store<'a>,
        host: &str,
        now: i64,
        random: &[u8; 96],
    ) -> Result<Self, Error> {
        let mut name = Host { buffer: [0; 255], len: host.len().min(255) };
        name.buffer[..name.len].copy_from_slice(&host.as_bytes()[..name.len]);

        let mut client_random = [0u8; 32];
        let mut session_id = [0u8; 32];
        let mut private = [0u8; 32];
        client_random.copy_from_slice(&random[..32]);
        session_id.copy_from_slice(&random[32..64]);
        private.copy_from_slice(&random[64..]);

        io.inbox_start = 0;
        io.inbox_len = 0;
        io.plain_start = 0;
        io.plain_len = 0;
        io.outbox_start = 0;
        io.outbox_len = 0;
        io.handshake_len = 0;
        io.leaf_len = 0;

        let mut session = Self {
            io,
            roots,
            host: name,
            now,
            state: State::ServerHello,
            transcript: Sha256::new(),
            secret: Some(StaticSecret::from(private)),
            client_random,
            session_id,
            handshake_secret: [0; HASH_LEN],
            client_handshake: [0; HASH_LEN],
            server_handshake: [0; HASH_LEN],
            read: None,
            write: None,
            peer_closed: false,
        };
        session.send_client_hello()?;
        Ok(session)
    }

    /// Байты, которые надо отправить собеседнику.
    #[must_use]
    pub fn outgoing(&self) -> &[u8] {
        &self.io.outbox[self.io.outbox_start..self.io.outbox_start + self.io.outbox_len]
    }

    /// Отметить, что столько байт ушло в сокет.
    pub fn consume_outgoing(&mut self, count: usize) {
        let count = count.min(self.io.outbox_len);
        self.io.outbox_start += count;
        self.io.outbox_len -= count;
        if self.io.outbox_len == 0 {
            self.io.outbox_start = 0;
        }
    }

    /// Сколько байт с провода приёмник готов взять прямо сейчас.
    ///
    /// Нужно тому, кто читает сокет: скормить больше, чем помещается, значит
    /// оставить остаток без места. Ноль здесь невозможен, пока расшифрованное
    /// забрано: приёмник хранит только недоехавшую запись, а она короче его
    /// самого.
    ///
    /// Считается **от длины, а не от курсора**, и это не мелочь. Курсор ползёт
    /// вперёд по мере разбора записей, а сдвигается к началу только в
    /// [`Self::feed`], перед самым копированием. Ответ «места нет», данный по
    /// курсору, привёл бы вызывающего к чтению нуля байт, а ноль для него
    /// означает «сокет пуст» — то есть «файл кончился». Загрузка обрывалась на
    /// «ответ кончился раньше времени» посреди исправного файла, и увидеть это
    /// удалось только по записи трафика: все байты приезжали.
    #[must_use]
    pub const fn room(&self) -> usize {
        INBOX - self.io.inbox_len
    }

    /// Расшифрованные данные приложения, которые уже пришли.
    #[must_use]
    pub fn plaintext(&self) -> &[u8] {
        &self.io.plain[self.io.plain_start..self.io.plain_start + self.io.plain_len]
    }

    /// Отметить, что столько расшифрованных байт забрано.
    pub fn consume_plaintext(&mut self, count: usize) {
        let count = count.min(self.io.plain_len);
        self.io.plain_start += count;
        self.io.plain_len -= count;
        if self.io.plain_len == 0 {
            self.io.plain_start = 0;
        }
    }

    /// Закончилось ли рукопожатие.
    #[must_use]
    pub const fn ready(&self) -> bool {
        matches!(self.state, State::Ready)
    }

    /// Сказал ли собеседник, что данных больше не будет.
    #[must_use]
    pub const fn closed(&self) -> bool {
        self.peer_closed
    }

    /// Принять байты с провода. Возвращает, сколько из них взято.
    ///
    /// Взято может быть меньше, чем дали: расшифрованное складывается в буфер, и
    /// пока вызывающий его не забрал, дальше разбирать некуда. Это не ошибка, а
    /// обратное давление — то самое, без которого загрузка в семьдесят семь
    /// мегабайт означала бы семьдесят семь мегабайт в памяти.
    pub fn feed(&mut self, data: &[u8]) -> Result<usize, Error> {
        let mut used = 0usize;
        loop {
            self.drain()?;
            if used == data.len() {
                return Ok(used);
            }
            self.compact_inbox();
            let room = self.io.inbox.len() - self.io.inbox_start - self.io.inbox_len;
            if room == 0 {
                return Ok(used);
            }
            let take = room.min(data.len() - used);
            let at = self.io.inbox_start + self.io.inbox_len;
            self.io.inbox[at..at + take].copy_from_slice(&data[used..used + take]);
            self.io.inbox_len += take;
            used += take;
        }
    }

    /// Зашифровать данные приложения и поставить их в очередь на отправку.
    pub fn send(&mut self, data: &[u8]) -> Result<(), Error> {
        if !self.ready() {
            return Err(Error::NotReady);
        }
        if data.len() > MAX_WRITE {
            return Err(Error::TooLong);
        }
        self.emit(content::APPLICATION_DATA, data)
    }

    /// Сказать собеседнику, что мы закончили.
    pub fn close(&mut self) {
        if self.state == State::Ready {
            // `close_notify`: уровень «предупреждение», описание 0.
            let _ = self.emit(content::ALERT, &[1, 0]);
            self.state = State::Closed;
        }
    }

    // --- рукопожатие -------------------------------------------------------

    fn send_client_hello(&mut self) -> Result<(), Error> {
        let public = PublicKey::from(self.secret.as_ref().ok_or(Error::OutOfOrder)?);
        let mut body = [0u8; 1024];
        let hello = build_client_hello(
            &mut body,
            &self.client_random,
            &self.session_id,
            self.host.as_str(),
            public.as_bytes(),
        )?;
        self.transcript.update(hello);
        self.queue_plain(content::HANDSHAKE, hello)?;
        // Пустая запись `change_cipher_spec` следом — дань посредникам, которые
        // рвут соединение, не увидев смены шифра. Стандарт разрешает её
        // игнорировать, и мы игнорируем встречную; отправляем свою потому, что
        // без неё часть сетей просто не пропускает рукопожатие.
        self.queue_plain(content::CHANGE_CIPHER_SPEC, &[1])?;
        Ok(())
    }

    /// Разобрать всё, что уже лежит в приёмнике.
    fn drain(&mut self) -> Result<(), Error> {
        loop {
            let available = self.io.inbox_len;
            if available < HEADER_LEN {
                return Ok(());
            }
            let start = self.io.inbox_start;
            let (kind, length) =
                record::header(&self.io.inbox[start..start + HEADER_LEN]).ok_or(Error::Malformed)?;
            if length > MAX_CIPHERTEXT {
                return Err(Error::TooLong);
            }
            if available < HEADER_LEN + length {
                // Запись не приехала целиком. Если она не помещается в буфер
                // даже теоретически — это отказ, а не ожидание.
                if HEADER_LEN + length > self.io.inbox.len() {
                    return Err(Error::TooLong);
                }
                return Ok(());
            }
            // Место под расшифрованное должно быть **до** расшифровки: иначе
            // запись пришлось бы расшифровывать дважды. Считается по верхней
            // оценке — длина записи без подписи и байта типа: настоящая длина
            // содержимого известна только после расшифровки, а решать надо
            // раньше. Проверка «лишь бы что-то было свободно» здесь не годится:
            // запись в шестнадцать килобайт при половине занятого буфера
            // расшифровалась бы в никуда.
            if self.io.plain_len + length.saturating_sub(record::TAG_LEN + 1) > PLAIN {
                return Ok(());
            }

            let body_at = start + HEADER_LEN;
            let consumed = HEADER_LEN + length;
            let (kind, plain_range) = if self.read.is_some() && kind != content::CHANGE_CIPHER_SPEC {
                let mut header = [0u8; HEADER_LEN];
                header.copy_from_slice(&self.io.inbox[start..body_at]);
                let keys = self.read.as_mut().expect("проверено выше");
                let (len, real) = keys
                    .open(&header, &mut self.io.inbox[body_at..body_at + length])
                    .ok_or(Error::BadRecord)?;
                (real, body_at..body_at + len)
            } else {
                (kind, body_at..body_at + length)
            };

            self.io.inbox_start += consumed;
            self.io.inbox_len -= consumed;
            self.handle(kind, plain_range)?;
        }
    }

    /// Обработать содержимое одной записи. Диапазон указывает в приёмник.
    fn handle(&mut self, kind: u8, range: core::ops::Range<usize>) -> Result<(), Error> {
        match kind {
            // Смена шифра — пережиток, который стандарт велит пропускать молча.
            content::CHANGE_CIPHER_SPEC => Ok(()),
            content::ALERT => {
                let body = &self.io.inbox[range];
                let description = *body.get(1).unwrap_or(&0);
                // `close_notify` (0) — это не ошибка, а конец разговора.
                if description == 0 {
                    self.peer_closed = true;
                    return Ok(());
                }
                Err(Error::Alert(description))
            }
            content::HANDSHAKE => {
                let length = range.len();
                if self.io.handshake_len + length > HANDSHAKE {
                    return Err(Error::TooLong);
                }
                // Копия обязательна: сообщение рукопожатия дробится по записям
                // как попало, и собирать его надо отдельно от них.
                let at = self.io.handshake_len;
                for index in 0..length {
                    self.io.handshake[at + index] = self.io.inbox[range.start + index];
                }
                self.io.handshake_len += length;
                self.handshake_messages()
            }
            content::APPLICATION_DATA => {
                if !self.ready() {
                    return Err(Error::OutOfOrder);
                }
                self.stash_plaintext(range)
            }
            _ => Err(Error::Malformed),
        }
    }

    /// Разобрать из буфера все сообщения, которые приехали целиком.
    fn handshake_messages(&mut self) -> Result<(), Error> {
        let mut at = 0usize;
        while self.io.handshake_len - at >= 4 {
            let head = &self.io.handshake[at..at + 4];
            let kind = head[0];
            let length =
                (usize::from(head[1]) << 16) | (usize::from(head[2]) << 8) | usize::from(head[3]);
            if self.io.handshake_len - at < 4 + length {
                break;
            }
            let start = at;
            at += 4 + length;
            self.message(kind, start, 4 + length)?;
        }
        // Разобранное убирается из начала буфера.
        if at != 0 {
            self.io.handshake.copy_within(at..self.io.handshake_len, 0);
            self.io.handshake_len -= at;
        }
        Ok(())
    }

    /// Одно сообщение рукопожатия: `[start, start + len)` в буфере сборки.
    fn message(&mut self, kind: u8, start: usize, len: usize) -> Result<(), Error> {
        let body_at = start + 4;
        let body_len = len - 4;

        match (self.state, kind) {
            (State::ServerHello, handshake::SERVER_HELLO) => {
                self.server_hello(body_at, body_len)?;
                self.add_to_transcript(start, len);
                self.derive_handshake_keys()?;
                self.state = State::EncryptedExtensions;
            }
            (State::EncryptedExtensions, handshake::ENCRYPTED_EXTENSIONS) => {
                self.add_to_transcript(start, len);
                self.state = State::Certificate;
            }
            (State::Certificate, handshake::CERTIFICATE_REQUEST) => {
                return Err(Error::WantsClientCertificate);
            }
            (State::Certificate, handshake::CERTIFICATE) => {
                self.certificate(body_at, body_len)?;
                self.add_to_transcript(start, len);
                self.state = State::CertificateVerify;
            }
            (State::CertificateVerify, handshake::CERTIFICATE_VERIFY) => {
                // Хеш стенограммы берётся **до** этого сообщения: сервер
                // подписывал то, что было сказано до него, включая свой
                // сертификат и не включая свою подпись.
                let transcript = self.transcript_hash();
                self.certificate_verify(body_at, body_len, &transcript)?;
                self.add_to_transcript(start, len);
                self.state = State::Finished;
            }
            (State::Finished, handshake::FINISHED) => {
                let transcript = self.transcript_hash();
                self.check_finished(body_at, body_len, &transcript)?;
                self.add_to_transcript(start, len);
                self.finish()?;
                self.state = State::Ready;
            }
            // Билеты приходят уже после рукопожатия и приходят почти всегда.
            // Молча выбросить их — единственно верное: хранить сеанс между
            // запусками программы негде.
            (State::Ready, handshake::NEW_SESSION_TICKET) => {}
            (State::Ready, handshake::KEY_UPDATE) => {
                self.key_update(body_at, body_len)?;
            }
            _ => return Err(Error::OutOfOrder),
        }
        Ok(())
    }

    fn add_to_transcript(&mut self, start: usize, len: usize) {
        self.transcript.update(&self.io.handshake[start..start + len]);
    }

    fn transcript_hash(&self) -> [u8; HASH_LEN] {
        self.transcript.clone().finalize().into()
    }

    fn server_hello(&mut self, at: usize, len: usize) -> Result<(), Error> {
        let body = &self.io.handshake[at..at + len];
        let mut reader = Reader::new(body);
        let _legacy_version = reader.u16().ok_or(Error::Malformed)?;
        let random = reader.take(32).ok_or(Error::Malformed)?;
        if random == HELLO_RETRY_MAGIC {
            return Err(Error::HelloRetry);
        }
        let _session_id = reader.vector8().ok_or(Error::Malformed)?;
        let suite = reader.u16().ok_or(Error::Malformed)?;
        let _compression = reader.u8().ok_or(Error::Malformed)?;
        if suite != TLS_CHACHA20_POLY1305_SHA256 {
            return Err(Error::NotTls13);
        }

        let mut version_ok = false;
        let mut share: Option<&[u8]> = None;
        let mut extensions = Reader::new(reader.vector16().ok_or(Error::Malformed)?);
        while !extensions.is_empty() {
            let kind = extensions.u16().ok_or(Error::Malformed)?;
            let body = extensions.vector16().ok_or(Error::Malformed)?;
            match kind {
                extension::SUPPORTED_VERSIONS => {
                    version_ok = body == [0x03, 0x04];
                }
                extension::KEY_SHARE => {
                    let mut reader = Reader::new(body);
                    let group = reader.u16().ok_or(Error::Malformed)?;
                    let key = reader.vector16().ok_or(Error::Malformed)?;
                    if group == X25519 && key.len() == 32 {
                        share = Some(key);
                    }
                }
                _ => {}
            }
        }
        if !version_ok {
            // Сервер согласился на 1.2 или ниже. Отступать некуда: старые версии
            // мы не умеем и не собираемся.
            return Err(Error::NotTls13);
        }
        let share = share.ok_or(Error::NoKeyShare)?;

        let mut theirs = [0u8; 32];
        theirs.copy_from_slice(share);
        let secret = self.secret.take().ok_or(Error::OutOfOrder)?;
        let shared = secret.diffie_hellman(&PublicKey::from(theirs));
        if !shared.was_contributory() {
            // Общий секрет из одних нулей: собеседник прислал точку малого
            // порядка. Продолжать значило бы шифровать на ключе, который знает
            // кто угодно.
            return Err(Error::NoKeyShare);
        }
        // Общий секрет кладётся туда же, где будет `handshake_secret`, — до
        // первого вызова `derive_handshake_keys` он и есть входной материал.
        self.handshake_secret = shared.to_bytes();
        Ok(())
    }

    fn derive_handshake_keys(&mut self) -> Result<(), Error> {
        let shared = self.handshake_secret;
        let early = extract(&[0u8; HASH_LEN], &[0u8; HASH_LEN]);
        let derived = derive_empty(&early, "derived");
        let handshake = extract(&derived, &shared);
        let transcript = self.transcript_hash();
        self.client_handshake = derive_secret(&handshake, "c hs traffic", &transcript);
        self.server_handshake = derive_secret(&handshake, "s hs traffic", &transcript);
        self.handshake_secret = handshake;
        self.read = Some(Keys::new(&self.server_handshake));
        self.write = Some(Keys::new(&self.client_handshake));
        Ok(())
    }

    fn certificate(&mut self, at: usize, len: usize) -> Result<(), Error> {
        let body = &self.io.handshake[at..at + len];
        let mut reader = Reader::new(body);
        let _context = reader.vector8().ok_or(Error::Malformed)?;
        let mut list = Reader::new(reader.vector24().ok_or(Error::Malformed)?);

        let mut chain_buffer: [Option<Certificate<'_>>; chain::MAX_CHAIN] = [None; chain::MAX_CHAIN];
        let mut count = 0usize;
        let mut leaf_der: &[u8] = &[];
        while !list.is_empty() {
            let der = list.vector24().ok_or(Error::Malformed)?;
            let _extensions = list.vector16().ok_or(Error::Malformed)?;
            if count == chain::MAX_CHAIN {
                return Err(Error::Chain(chain::Error::TooLong));
            }
            let certificate = Certificate::parse(der).map_err(Error::Certificate)?;
            if count == 0 {
                leaf_der = der;
            }
            chain_buffer[count] = Some(certificate);
            count += 1;
        }

        let mut chain_list = [chain_buffer[0].ok_or(Error::Chain(chain::Error::Empty))?; chain::MAX_CHAIN];
        for index in 1..count {
            chain_list[index] = chain_buffer[index].ok_or(Error::Malformed)?;
        }
        chain::verify(&chain_list[..count], &self.roots, self.host.as_str(), self.now)
            .map_err(Error::Chain)?;

        // Лист копируется: `CertificateVerify` придёт следующим сообщением, а к
        // тому времени буфер сборки уже сдвинется.
        if leaf_der.len() > LEAF {
            return Err(Error::TooLong);
        }
        let length = leaf_der.len();
        for index in 0..length {
            self.io.leaf[index] = leaf_der[index];
        }
        self.io.leaf_len = length;
        Ok(())
    }

    fn certificate_verify(
        &mut self,
        at: usize,
        len: usize,
        transcript: &[u8; HASH_LEN],
    ) -> Result<(), Error> {
        let body = &self.io.handshake[at..at + len];
        let mut reader = Reader::new(body);
        let scheme = reader.u16().ok_or(Error::Malformed)?;
        let signature = reader.vector16().ok_or(Error::Malformed)?;
        if !reader.is_empty() {
            return Err(Error::Malformed);
        }

        let leaf = Certificate::parse(&self.io.leaf[..self.io.leaf_len])
            .map_err(Error::Certificate)?;
        let algorithm = match (scheme, &leaf.key) {
            (scheme::ECDSA_SECP256R1_SHA256, CertKey::Ec { curve: Curve::P256, .. }) => {
                Algorithm::Ecdsa(Hash::Sha256)
            }
            (scheme::ECDSA_SECP384R1_SHA384, CertKey::Ec { curve: Curve::P384, .. }) => {
                Algorithm::Ecdsa(Hash::Sha384)
            }
            (scheme::RSA_PSS_RSAE_SHA256, CertKey::Rsa(_)) => Algorithm::RsaPss(Hash::Sha256),
            (scheme::RSA_PSS_RSAE_SHA384, CertKey::Rsa(_)) => Algorithm::RsaPss(Hash::Sha384),
            (scheme::RSA_PSS_RSAE_SHA512, CertKey::Rsa(_)) => Algorithm::RsaPss(Hash::Sha512),
            // Схемы PKCS#1 v1.5 в этом месте запрещены стандартом, и отказ здесь
            // — не наша строгость, а буква RFC 8446, §4.4.3.
            _ => return Err(Error::UnknownScheme),
        };

        // Шестьдесят четыре пробела впереди — не украшение: они делают
        // подписанную строку заведомо не похожей ни на один сертификат,
        // подписанный тем же ключом.
        let padding = [0x20u8; 64];
        let signed: [&[u8]; 4] =
            [&padding, CERTIFICATE_VERIFY_CONTEXT, &[0x00], transcript];
        if !algorithm.verify(&leaf.key, &signed, signature) {
            return Err(Error::BadSignature);
        }
        Ok(())
    }

    fn check_finished(
        &mut self,
        at: usize,
        len: usize,
        transcript: &[u8; HASH_LEN],
    ) -> Result<(), Error> {
        let body = &self.io.handshake[at..at + len];
        let mut key = [0u8; HASH_LEN];
        expand_label(&self.server_handshake, "finished", &[], &mut key);
        let expected = hmac(&key, &[transcript]);
        if body.len() != expected.len() {
            return Err(Error::BadFinished);
        }
        let mut difference = 0u8;
        for (a, b) in body.iter().zip(expected.iter()) {
            difference |= a ^ b;
        }
        if difference != 0 {
            return Err(Error::BadFinished);
        }
        Ok(())
    }

    /// Ответить своим `Finished` и перейти на ключи приложения.
    fn finish(&mut self) -> Result<(), Error> {
        // Стенограмма здесь уже включает `Finished` сервера — так требует
        // стандарт и для нашего `Finished`, и для секретов приложения.
        let transcript = self.transcript_hash();

        let mut key = [0u8; HASH_LEN];
        expand_label(&self.client_handshake, "finished", &[], &mut key);
        let verify = hmac(&key, &[&transcript]);
        let mut message = [0u8; 4 + HASH_LEN];
        message[0] = handshake::FINISHED;
        message[3] = HASH_LEN as u8;
        message[4..].copy_from_slice(&verify);
        // Отправляется **на ключах рукопожатия**: смена ключей происходит после.
        self.emit(content::HANDSHAKE, &message)?;

        let derived = derive_empty(&self.handshake_secret, "derived");
        let master = extract(&derived, &[0u8; HASH_LEN]);
        let client = derive_secret(&master, "c ap traffic", &transcript);
        let server = derive_secret(&master, "s ap traffic", &transcript);
        self.read = Some(Keys::new(&server));
        self.write = Some(Keys::new(&client));
        Ok(())
    }

    fn key_update(&mut self, at: usize, len: usize) -> Result<(), Error> {
        if len != 1 {
            return Err(Error::Malformed);
        }
        let request = self.io.handshake[at];
        self.read.as_mut().ok_or(Error::OutOfOrder)?.update();
        if request == 1 {
            // Сервер просит обновиться и нас. Отказ выглядел бы как молчание, а
            // сервер после просьбы вправе перестать понимать старые ключи.
            let message = [handshake::KEY_UPDATE, 0, 0, 1, 0];
            self.emit(content::HANDSHAKE, &message)?;
            self.write.as_mut().ok_or(Error::OutOfOrder)?.update();
        }
        Ok(())
    }

    // --- буферы ------------------------------------------------------------

    /// Положить расшифрованное в очередь для вызывающего.
    fn stash_plaintext(&mut self, range: core::ops::Range<usize>) -> Result<(), Error> {
        let length = range.len();
        if self.io.plain_start + self.io.plain_len + length > PLAIN {
            self.io.plain.copy_within(
                self.io.plain_start..self.io.plain_start + self.io.plain_len,
                0,
            );
            self.io.plain_start = 0;
        }
        if self.io.plain_len + length > PLAIN {
            return Err(Error::TooLong);
        }
        let at = self.io.plain_start + self.io.plain_len;
        for index in 0..length {
            self.io.plain[at + index] = self.io.inbox[range.start + index];
        }
        self.io.plain_len += length;
        Ok(())
    }

    fn compact_inbox(&mut self) {
        if self.io.inbox_start == 0 {
            return;
        }
        self.io
            .inbox
            .copy_within(self.io.inbox_start..self.io.inbox_start + self.io.inbox_len, 0);
        self.io.inbox_start = 0;
    }

    /// Записать в очередь на отправку то, что шифровать не надо.
    fn queue_plain(&mut self, kind: u8, body: &[u8]) -> Result<(), Error> {
        self.make_room(HEADER_LEN + body.len())?;
        let at = self.io.outbox_start + self.io.outbox_len;
        self.io.outbox[at] = kind;
        self.io.outbox[at + 1] = 0x03;
        self.io.outbox[at + 2] = if kind == content::HANDSHAKE { 0x01 } else { 0x03 };
        let length = (body.len() as u16).to_be_bytes();
        self.io.outbox[at + 3..at + 5].copy_from_slice(&length);
        self.io.outbox[at + HEADER_LEN..at + HEADER_LEN + body.len()].copy_from_slice(body);
        self.io.outbox_len += HEADER_LEN + body.len();
        Ok(())
    }

    /// Зашифровать и записать в очередь на отправку.
    fn emit(&mut self, kind: u8, body: &[u8]) -> Result<(), Error> {
        let total = HEADER_LEN + body.len() + 1 + record::TAG_LEN;
        self.make_room(total)?;
        let at = self.io.outbox_start + self.io.outbox_len;
        let keys = self.write.as_mut().ok_or(Error::NotReady)?;
        let written = keys
            .seal(kind, body, &mut self.io.outbox[at..at + total])
            .ok_or(Error::TooLong)?;
        self.io.outbox_len += written;
        Ok(())
    }

    fn make_room(&mut self, want: usize) -> Result<(), Error> {
        if self.io.outbox_start + self.io.outbox_len + want > OUTBOX {
            self.io.outbox.copy_within(
                self.io.outbox_start..self.io.outbox_start + self.io.outbox_len,
                0,
            );
            self.io.outbox_start = 0;
        }
        if self.io.outbox_len + want > OUTBOX {
            return Err(Error::TooLong);
        }
        Ok(())
    }
}

/// Собрать `ClientHello` целиком.
fn build_client_hello<'a>(
    buffer: &'a mut [u8],
    client_random: &[u8; 32],
    session_id: &[u8; 32],
    host: &str,
    key_share: &[u8; 32],
) -> Result<&'a [u8], Error> {
    let mut writer = Writer::new(buffer);
    let full = |_: wire::Full| Error::TooLong;

    writer.u8(handshake::CLIENT_HELLO).map_err(full)?;
    let message = writer.open(3).map_err(full)?;

    // Версия в заголовке — всегда 1.2, настоящая едет расширением. Так решили в
    // RFC 8446, чтобы рукопожатие проходило через посредников, знающих только
    // старые версии.
    writer.u16(0x0303).map_err(full)?;
    writer.bytes(client_random).map_err(full)?;
    writer.u8(32).map_err(full)?;
    writer.bytes(session_id).map_err(full)?;

    let suites = writer.open(2).map_err(full)?;
    writer.u16(TLS_CHACHA20_POLY1305_SHA256).map_err(full)?;
    writer.close(suites);

    writer.u8(1).map_err(full)?;
    writer.u8(0).map_err(full)?;

    let extensions = writer.open(2).map_err(full)?;

    // server_name: без него виртуальный хостинг отдаст чужой сертификат.
    writer.u16(extension::SERVER_NAME).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    writer.u8(0).map_err(full)?;
    let name = writer.open(2).map_err(full)?;
    writer.bytes(host.as_bytes()).map_err(full)?;
    writer.close(name);
    writer.close(list);
    writer.close(body);

    // supported_versions: единственная запись — 1.3.
    writer.u16(extension::SUPPORTED_VERSIONS).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    writer.u8(2).map_err(full)?;
    writer.u16(0x0304).map_err(full)?;
    writer.close(body);

    // supported_groups.
    writer.u16(extension::SUPPORTED_GROUPS).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    writer.u16(X25519).map_err(full)?;
    writer.close(list);
    writer.close(body);

    // key_share: сразу с половиной ключа, чтобы обошлось одним кругом.
    writer.u16(extension::KEY_SHARE).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    writer.u16(X25519).map_err(full)?;
    let key = writer.open(2).map_err(full)?;
    writer.bytes(key_share).map_err(full)?;
    writer.close(key);
    writer.close(list);
    writer.close(body);

    // signature_algorithms — то, чем сервер вправе подписать `CertificateVerify`.
    // PKCS#1 v1.5 здесь нет намеренно: в этом сообщении он запрещён.
    writer.u16(extension::SIGNATURE_ALGORITHMS).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    for value in [
        scheme::ECDSA_SECP256R1_SHA256,
        scheme::ECDSA_SECP384R1_SHA384,
        scheme::RSA_PSS_RSAE_SHA256,
        scheme::RSA_PSS_RSAE_SHA384,
        scheme::RSA_PSS_RSAE_SHA512,
    ] {
        writer.u16(value).map_err(full)?;
    }
    writer.close(list);
    writer.close(body);

    // signature_algorithms_cert — а вот чем подписаны сами сертификаты. Здесь
    // PKCS#1 v1.5 обязателен: им подписано почти всё, что выписано до сих пор.
    writer.u16(extension::SIGNATURE_ALGORITHMS_CERT).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    for value in [
        scheme::ECDSA_SECP256R1_SHA256,
        scheme::ECDSA_SECP384R1_SHA384,
        scheme::RSA_PKCS1_SHA256,
        scheme::RSA_PKCS1_SHA384,
        scheme::RSA_PKCS1_SHA512,
        scheme::RSA_PSS_RSAE_SHA256,
        scheme::RSA_PSS_RSAE_SHA384,
    ] {
        writer.u16(value).map_err(full)?;
    }
    writer.close(list);
    writer.close(body);

    // ALPN: говорим прямо, что умеем HTTP/1.1. Без этого сервер вправе выбрать
    // HTTP/2, а разбирать его нам нечем.
    writer.u16(extension::ALPN).map_err(full)?;
    let body = writer.open(2).map_err(full)?;
    let list = writer.open(2).map_err(full)?;
    writer.u8(8).map_err(full)?;
    writer.bytes(b"http/1.1").map_err(full)?;
    writer.close(list);
    writer.close(body);

    writer.close(extensions);
    writer.close(message);
    Ok(writer.finish())
}
