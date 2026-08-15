//! Клиент HTTP/1.1 и HTTPS: ровно столько, сколько нужно, чтобы забрать файл.
//!
//! # Почему в программе, а не в ядре
//!
//! Потому что качать умеет и третье кольцо: у него есть сокеты, файлы и время.
//! В ядре HTTP оказался бы разбором чужого текста в самом привилегированном
//! месте системы — ради задачи, которая не требует ни одного права ядра. То же
//! и с TLS: проверка цепочки сертификатов — это сотни строк разбора чужого DER,
//! и место им там же, где HTTP.
//!
//! # Что этот клиент умеет и чего не умеет
//!
//! Умеет: `GET`, разбор строки состояния, `Content-Length`, тело потоком —
//! кусками, наружу, не держа его в памяти (образ системы — десятки мегабайт, а
//! кучи у программы нет вовсе). Умеет `https://` — фаза 39a, крейт `tls`.
//! Умеет **переадресацию**, ограниченно (см. ниже).
//!
//! Не умеет — и говорит об этом вслух, а не молчит:
//!
//! * **Разбиение на куски** (`Transfer-Encoding: chunked`). Наш сервер отдаёт
//!   файл с длиной; чужой, отдавший кусками, получит внятный отказ вместо
//!   тела, в котором посреди данных лежат шестнадцатеричные числа.
//! * **HTTP/2**. В `ClientHello` мы прямо называем `http/1.1` расширением ALPN,
//!   чтобы сервер не выбрал то, чего мы не разберём.
//!
//! # Переадресация: почему теперь идём, хотя раньше отказывались
//!
//! В фазе 39 переадресация была запрещена, и довод звучал так: пойти по
//! `Location` автоматически — значит позволить серверу увести клиента куда
//! угодно. Довод остаётся верным ровно до тех пор, пока доверие держится на
//! канале. У нас оно держится на **подписи**: индекс подписан Ed25519, у образа
//! в подписанном индексе записан SHA-256, а контейнер проверяет ядро ключом из
//! `/os-keys`. Сервер, уведший нас в чужое место, добьётся только того, что
//! приедет файл, который не сойдётся с подписью, — и не встанет.
//!
//! А без переадресации не работает то, ради чего фаза и делается: GitHub
//! отдаёт ассеты релиза **только** через два перехода — `releases/latest` на
//! `releases/download/<тег>`, и оттуда на CDN с подписанным адресом.
//!
//! Ограничения всё же есть, и они не про доверие, а про здравый смысл:
//! переходов не больше [`MAX_REDIRECTS`], адрес обязан быть абсолютным, и
//! **с `https://` на `http://` мы не спускаемся**. Последнее — единственное,
//! что здесь про безопасность: понижение канала посреди разговора всегда
//! означает, что кто-то вмешался.
//!
//! # Соединение закрывается сервером
//!
//! В запросе стоит `Connection: close`: одно соединение — один файл. Держать его
//! живым имеет смысл там, где запросов десятки; здесь их три, а поддержка
//! `keep-alive` означала бы уметь понимать, где кончился ответ, ещё одним
//! способом.

use crate::{
    close_socket, connect, random, recv, resolve, send_waiting, shutdown, sleep_ms, stream,
    stream_state, uptime_ms, wait_connected,
};

/// Сколько ждать установления связи.
const CONNECT_TIMEOUT_MS: u64 = 15_000;

/// Сколько ждать **первого** байта ответа.
///
/// Отдельно от общего срока: сервер, который не ответил вовсе, и сервер,
/// который медленно отдаёт сто мегабайт, — разные неисправности, и говорить о
/// них надо разными словами.
const HEADER_TIMEOUT_MS: u64 = 20_000;

/// Сколько ждать окончания рукопожатия TLS.
///
/// Больше, чем на заголовок: в рукопожатии четыре пересылки, а проверка цепочки
/// из трёх сертификатов RSA на отладочном ядре под эмуляцией занимает секунды —
/// длинная арифметика на четырёх килобитах считается медленно и считается
/// честно.
const HANDSHAKE_TIMEOUT_MS: u64 = 60_000;

/// Сколько ждать продолжения тела, если оно перестало идти.
const BODY_STALL_MS: u64 = 30_000;

/// Сколько байт заголовка ответа клиент согласен прочитать.
///
/// Четыре килобайта при десятке нужных строк — запас на чужой сервер, который
/// любит рассказывать о себе, и на длинный `Location`: адрес ассета на CDN
/// GitHub занимает под тысячу знаков вместе с подписью. Предел существует
/// потому, что заголовок приходит **до** того, как стало известно хоть
/// что-нибудь.
const MAX_HEADER: usize = 4 * 1024;

/// Сколько переходов по `Location` клиент готов сделать.
///
/// Четыре при двух нужных для GitHub. Предел существует потому, что цепочку
/// переадресаций выбирает сервер, и без предела она бывает бесконечной.
pub const MAX_REDIRECTS: usize = 4;

/// Сколько знаков помещается в адрес.
pub const MAX_URL: usize = 1024;

/// Чем кончилась попытка забрать файл.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Сокет не завести: их предел на программу.
    NoSocket,
    /// Соединение не установилось: адрес не отвечает.
    NoAnswer,
    /// Связь оборвалась посреди обмена.
    Reset,
    /// Ответ не начинается на `HTTP/1.`.
    NotHttp,
    /// Ответ пришёл, но код не `200`.
    Status(u16),
    /// Заголовок длиннее [`MAX_HEADER`] или без пустой строки в конце.
    HugeHeader,
    /// Сервер отдаёт кусками — этот клиент так не умеет.
    Chunked,
    /// В ответе нет `Content-Length`, а без него неизвестно, дошло ли всё.
    NoLength,
    /// Тело кончилось раньше объявленной длины.
    Short { got: u64, want: u64 },
    /// Тело оказалось длиннее объявленного.
    Long,
    /// Никто ничего не присылает дольше отведённого.
    Timeout,
    /// Тот, кому мы отдаём тело, отказался его принимать (место кончилось).
    SinkFailed,
    /// Адрес не разбирается.
    BadUrl,
    /// Имя не превращается в адрес.
    NoAddress,
    /// Сервер водит по кругу.
    TooManyRedirects,
    /// Сервер переадресует, но не говорит куда.
    NoLocation,
    /// Сервер переадресует с `https://` на `http://`.
    Downgrade,
    /// Адрес `https://`, а доверенных корней программе не дали.
    NoTrust,
    /// Ядро не дало случайных байт: без них ключ соединения не сделать.
    NoRandom,
    /// TLS не договорился.
    Tls(tls::Error),
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NoSocket => "cannot open a socket",
            Self::NoAnswer => "the server did not answer",
            Self::Reset => "the connection was reset",
            Self::NotHttp => "that answer is not HTTP",
            Self::Status(_) => "the server refused the request",
            Self::HugeHeader => "the answer header is too long to be one",
            Self::Chunked => "the server sends chunks, and this client cannot read them",
            Self::NoLength => "the answer has no Content-Length",
            Self::Short { .. } => "the answer ended early",
            Self::Long => "the answer is longer than it said",
            Self::Timeout => "the server went quiet",
            Self::SinkFailed => "there is nowhere to put what was downloaded",
            Self::BadUrl => "that is not an address this client understands",
            Self::NoAddress => "that name has no address",
            Self::TooManyRedirects => "the server sends us round in circles",
            Self::NoLocation => "the server redirects and does not say where",
            Self::Downgrade => "the server redirects from https to http; refusing",
            Self::NoTrust => "https was asked for and this system was given no trusted roots",
            Self::NoRandom => "the kernel gave no random bytes; refusing to make a key without them",
            Self::Tls(inner) => inner.text(),
        }
    }
}

/// Что сказал сервер и сколько байт тела приехало.
#[derive(Debug, Clone, Copy)]
pub struct Response {
    pub status: u16,
    pub length: u64,
}

/// Разобранный адрес.
#[derive(Debug, Clone, Copy)]
pub struct Url<'a> {
    pub secure: bool,
    pub host: &'a str,
    pub port: u16,
    /// Путь вместе с запросом; всегда начинается с косой черты.
    pub path: &'a str,
}

/// Разобрать `http://host[:port]/path` или `https://...`.
///
/// Возвращает `None` на всём, что не начинается со схемы: адрес без схемы — это
/// не «по умолчанию HTTP», а строка, о которой неизвестно, что она такое.
#[must_use]
pub fn parse_url(text: &str) -> Option<Url<'_>> {
    let (secure, rest) = if let Some(rest) = text.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = text.strip_prefix("http://") {
        (false, rest)
    } else {
        return None;
    };

    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return None;
    }
    // Имя пользователя перед адресом (`user@host`) мы не поддерживаем: оно
    // ничего не значит для GET и служит только тем, кто прячет настоящее имя
    // сервера за похожим на него началом строки.
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some(Url { secure, host, port, path })
}

/// Строка адреса на стеке — то, во что складывается `Location`.
pub struct Location {
    buffer: [u8; MAX_URL],
    len: usize,
}

impl Default for Location {
    fn default() -> Self {
        Self::new()
    }
}

impl Location {
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: [0; MAX_URL], len: 0 }
    }

    pub fn set(&mut self, text: &str) -> bool {
        if text.len() > self.buffer.len() {
            return false;
        }
        self.buffer[..text.len()].copy_from_slice(text.as_bytes());
        self.len = text.len();
        true
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY: в буфер попадают только байты из `&str`, то есть UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// То, что нужно, чтобы поговорить по HTTPS.
///
/// Буферы приходят снаружи, из статика программы: их почти шестьдесят
/// килобайт, а стека у программы шестьдесят четыре.
pub struct Trust<'a> {
    pub buffers: &'a mut tls::Buffers,
    pub roots: x509::Store<'a>,
    /// Секунды эпохи Unix; ноль — «часы неизвестны, сроки не проверять».
    pub now: i64,
}

/// Рабочие буферы загрузки.
///
/// Их два, и это не расточительность. При HTTPS в первом лежат байты **с
/// провода** (шифрованные), а во втором — расшифрованное: пока первый
/// скармливается TLS, второй наполняется, и одним буфером тут не обойтись.
/// При обычном HTTP первый не используется вовсе.
pub struct Buffers<'a> {
    pub wire: &'a mut [u8],
    pub body: &'a mut [u8],
}

/// Забрать файл по адресу и отдать тело кусками.
///
/// `sink` возвращает `false`, если принять кусок не удалось, — тогда загрузка
/// прекращается с [`Error::SinkFailed`]. Так сюда попадает единственная
/// настоящая ошибка потребителя: на разделе состояния кончилось место.
pub fn get(
    url: &str,
    trust: Option<&mut Trust<'_>>,
    io: &mut Buffers<'_>,
    sink: &mut impl FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    let mut current = Location::new();
    if !current.set(url) {
        return Err(Error::BadUrl);
    }
    let mut trust = trust;
    let mut was_secure = false;

    for _ in 0..=MAX_REDIRECTS {
        let parsed = parse_url(current.as_str()).ok_or(Error::BadUrl)?;
        // Понижение канала — единственный переход, который мы запрещаем.
        if was_secure && !parsed.secure {
            return Err(Error::Downgrade);
        }
        was_secure = parsed.secure;

        let address = match parse_ip(parsed.host) {
            Some(address) => address,
            None => resolve(parsed.host).ok_or(Error::NoAddress)?,
        };

        let mut next = Location::new();
        let outcome = if parsed.secure {
            match trust.as_mut() {
                Some(trust) => once(parsed, address, Some(&mut **trust), io, sink, &mut next)?,
                None => return Err(Error::NoTrust),
            }
        } else {
            once(parsed, address, None, io, sink, &mut next)?
        };
        match outcome {
            Some(response) => return Ok(response),
            None => {
                if next.is_empty() {
                    return Err(Error::NoLocation);
                }
                current = next;
            }
        }
    }
    Err(Error::TooManyRedirects)
}

/// Одна попытка: соединиться, спросить, прочитать.
///
/// Возвращает `Ok(Some(..))` — ответ получен; `Ok(None)` — сервер переадресует,
/// и новый адрес записан в `next`.
fn once(
    url: Url<'_>,
    address: u32,
    trust: Option<&mut Trust<'_>>,
    io: &mut Buffers<'_>,
    sink: &mut impl FnMut(&[u8]) -> bool,
    next: &mut Location,
) -> Result<Option<Response>, Error> {
    let socket = stream();
    if socket < 0 {
        return Err(Error::NoSocket);
    }
    let result = exchange(socket, url, address, trust, io, sink, next);
    close_socket(socket);
    result
}

fn exchange(
    socket: i64,
    url: Url<'_>,
    address: u32,
    trust: Option<&mut Trust<'_>>,
    io: &mut Buffers<'_>,
    sink: &mut impl FnMut(&[u8]) -> bool,
    next: &mut Location,
) -> Result<Option<Response>, Error> {
    if connect(socket, address, url.port) < 0 {
        return Err(Error::NoAnswer);
    }
    if !wait_connected(socket, CONNECT_TIMEOUT_MS) {
        return Err(Error::NoAnswer);
    }

    let mut session = match trust {
        Some(trust) => {
            let mut seed = [0u8; 96];
            if !random(&mut seed) {
                return Err(Error::NoRandom);
            }
            let session =
                tls::Session::new(&mut *trust.buffers, trust.roots, url.host, trust.now, &seed)
                    .map_err(Error::Tls)?;
            Some(session)
        }
        None => None,
    };
    let mut wire = Wire { socket, session: session.as_mut() };
    wire.handshake(io.wire)?;

    // Запрос уходит кусками, а не собирается в буфер: собирать его пришлось бы
    // форматированием, которого у программы без кучи нет. По обычному HTTP их
    // всё равно склеит TCP; по TLS каждый кусок стал бы отдельной записью с
    // двадцатью байтами накладных, поэтому там запрос собирается в одну.
    let parts = [
        "GET ",
        url.path,
        " HTTP/1.1\r\nHost: ",
        url.host,
        "\r\nUser-Agent: FreeOS-sysupdate/1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
    ];
    wire.request(&parts, io.wire)?;
    // Сказать «я всё» нельзя: половина закрывается вместе с возможностью
    // получить ответ у собеседников, читающих до `FIN`. HTTP этого и не требует
    // — конец запроса виден по пустой строке.

    let mut header = [0u8; MAX_HEADER];
    let (header_len, body_start, filled) = read_header(&mut wire, io, &mut header)?;
    let head = core::str::from_utf8(&header[..header_len]).map_err(|_| Error::NotHttp)?;
    let status = status_of(head)?;

    if is_redirect(status) {
        let target = header_value(head, "location:").ok_or(Error::NoLocation)?;
        // Относительный адрес (`/other/path`) достраивается до полного: сервер
        // вправе его прислать, и отказ выглядел бы как «файл не качается».
        if target.starts_with('/') {
            let mut full = Location::new();
            let scheme = if url.secure { "https://" } else { "http://" };
            if !push_all(&mut full, &[scheme, url.host, target]) {
                return Err(Error::BadUrl);
            }
            *next = full;
        } else if !next.set(target) {
            return Err(Error::BadUrl);
        }
        return Ok(None);
    }
    if status != 200 {
        return Err(Error::Status(status));
    }
    if has_header(head, "transfer-encoding:") {
        return Err(Error::Chunked);
    }
    let length = content_length(head).ok_or(Error::NoLength)?;

    // Хвост, приехавший вместе с заголовком, — это уже тело. Потерять его —
    // классическая ошибка такого разбора: файл оказывается короче ровно на то,
    // что поместилось в первый сегмент.
    let mut done = 0u64;
    if filled > body_start {
        let chunk = &header[body_start..filled];
        if chunk.len() as u64 > length {
            return Err(Error::Long);
        }
        if !sink(chunk) {
            return Err(Error::SinkFailed);
        }
        done += chunk.len() as u64;
    }

    let mut quiet_since = uptime_ms();
    while done < length {
        let read = wire.read(io.wire, io.body)?;
        if read > 0 {
            let chunk = &io.body[..read];
            if done + chunk.len() as u64 > length {
                return Err(Error::Long);
            }
            if !sink(chunk) {
                return Err(Error::SinkFailed);
            }
            done += chunk.len() as u64;
            quiet_since = uptime_ms();
            continue;
        }
        if wire.finished() {
            return Err(Error::Short { got: done, want: length });
        }
        if uptime_ms().saturating_sub(quiet_since) > BODY_STALL_MS {
            return Err(Error::Timeout);
        }
        sleep_ms(2);
    }

    // Своя половина закрывается в конце: сервер уже сказал всё, что собирался, и
    // `FIN` от нас — это единственный способ дать ему закрыть соединение, не
    // дожидаясь своего таймаута.
    wire.close();
    shutdown(socket);
    Ok(Some(Response { status, length: done }))
}

/// Провод: сокет и, если разговор защищённый, сеанс TLS поверх него.
struct Wire<'a, 'b> {
    socket: i64,
    session: Option<&'a mut tls::Session<'b>>,
}

impl Wire<'_, '_> {
    /// Довести рукопожатие до конца. Для обычного HTTP не делает ничего.
    fn handshake(&mut self, scratch: &mut [u8]) -> Result<(), Error> {
        if self.session.is_none() {
            return Ok(());
        }
        let deadline = uptime_ms() + HANDSHAKE_TIMEOUT_MS;
        loop {
            self.flush()?;
            if self.session.as_ref().is_some_and(|session| session.ready()) {
                return Ok(());
            }
            let cap = {
                let session = self.session.as_ref().expect("проверено выше");
                scratch.len().min(session.room())
            };
            let read = recv(self.socket, &mut scratch[..cap]);
            if read > 0 {
                let session = self.session.as_mut().expect("проверено выше");
                let mut at = 0usize;
                while at < read as usize {
                    let used = session.feed(&scratch[at..read as usize]).map_err(Error::Tls)?;
                    if used == 0 {
                        break;
                    }
                    at += used;
                }
                continue;
            }
            match stream_state(self.socket) {
                Some(state) if state.reset != 0 => return Err(Error::Reset),
                Some(state) if state.peer_closed != 0 => return Err(Error::Reset),
                _ => {}
            }
            if uptime_ms() > deadline {
                return Err(Error::Timeout);
            }
            sleep_ms(2);
        }
    }

    /// Отправить запрос: по TLS — одной записью, по HTTP — как есть.
    fn request(&mut self, parts: &[&str], scratch: &mut [u8]) -> Result<(), Error> {
        match self.session {
            None => {
                for part in parts {
                    self.write_socket(part.as_bytes())?;
                }
                Ok(())
            }
            Some(_) => {
                let mut at = 0usize;
                for part in parts {
                    if at + part.len() > scratch.len() {
                        return Err(Error::BadUrl);
                    }
                    scratch[at..at + part.len()].copy_from_slice(part.as_bytes());
                    at += part.len();
                }
                let session = self.session.as_mut().expect("проверено выше");
                session.send(&scratch[..at]).map_err(Error::Tls)?;
                self.flush()
            }
        }
    }

    /// Вытолкнуть в сокет всё, что TLS поставил в очередь.
    fn flush(&mut self) -> Result<(), Error> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        while !session.outgoing().is_empty() {
            let wrote = send_waiting(self.socket, session.outgoing(), 200);
            if wrote == crate::ERR_AGAIN {
                return Err(Error::Timeout);
            }
            if wrote <= 0 {
                return Err(Error::Reset);
            }
            session.consume_outgoing(wrote as usize);
        }
        Ok(())
    }

    /// Прочитать сколько-нибудь содержимого в `out`.
    ///
    /// Ноль означает ровно одно: **в сокете пусто**. Различие принципиальное и
    /// стоило отладочного дня. Запись TLS приезжает несколькими сегментами, и
    /// первый ответ «пока нечего» на полуприехавшей записи вызывающий читает
    /// как «данных больше не будет» — а собеседник к этому моменту уже прислал
    /// `FIN`, потому что отдал всё. Загрузка обрывается на «ответ кончился
    /// раньше времени» посреди исправного файла.
    ///
    /// Поэтому здесь цикл: из сокета вычитывается всё, пока не появится
    /// расшифрованное или пока сокет не опустеет.
    fn read(&mut self, scratch: &mut [u8], out: &mut [u8]) -> Result<usize, Error> {
        if self.session.is_none() {
            let read = recv(self.socket, out);
            return Ok(read.max(0) as usize);
        }
        loop {
            let session = self.session.as_mut().expect("проверено выше");
            // Сначала — разобрать то, что уже лежит в приёмнике, и только потом
            // идти в сокет. Порядок именно этот, и он стоил отладочного дня:
            // последняя запись ответа приезжает, когда расшифрованное ещё не
            // забрано, и остаётся в приёмнике неразобранной. Сокет к этому
            // времени пуст, собеседник закрылся — и «ничего не пришло»
            // читается как «файл кончился». Не хватало ровно последней записи:
            // «приехало 524186 из 524288».
            //
            // Ровно та же ошибка, что и на другом конце этого разговора, в
            // HTTPS-сервере стенда: сначала разобрать прочитанное, потом читать.
            session.feed(&[]).map_err(Error::Tls)?;
            // Расшифрованное отдаётся раньше нового чтения: пока оно не забрано,
            // разбирать новые записи некуда.
            let ready = session.plaintext().len();
            if ready > 0 {
                let take = ready.min(out.len());
                out[..take].copy_from_slice(&session.plaintext()[..take]);
                session.consume_plaintext(take);
                return Ok(take);
            }
            // Больше, чем приёмник готов взять, читать нельзя: остаток пришлось
            // бы где-то держать, а держать его негде.
            let cap = scratch.len().min(session.room());
            let read = recv(self.socket, &mut scratch[..cap]);
            if read <= 0 {
                return Ok(0);
            }
            let mut at = 0usize;
            while at < read as usize {
                let used = session.feed(&scratch[at..read as usize]).map_err(Error::Tls)?;
                if used == 0 {
                    break;
                }
                at += used;
            }
            // Ответ на `KeyUpdate` мог встать в очередь на отправку.
            self.flush()?;
        }
    }

    /// Данных больше не будет: собеседник закрылся.
    fn finished(&self) -> bool {
        if let Some(session) = self.session.as_ref() {
            if session.closed() && session.plaintext().is_empty() {
                return true;
            }
        }
        match stream_state(self.socket) {
            Some(state) if state.reset != 0 => true,
            // Закрытая половина при непустой очереди означает «дочитай то, что
            // уже пришло», а не «данных больше не будет», — но здесь мы уже
            // спросили и не получили ничего.
            Some(state) => state.peer_closed != 0,
            None => false,
        }
    }

    fn close(&mut self) {
        if let Some(session) = self.session.as_mut() {
            session.close();
        }
        let _ = self.flush();
    }

    /// Отправить всё, повторяя попытки, пока выясняется аппаратный адрес.
    ///
    /// «Ещё не сейчас» отделено от «оборвалось» намеренно: первое означает, что
    /// ARP не ответил, второе — что соединения больше нет, и человеку с этими
    /// двумя сообщениями идти в разные стороны.
    fn write_socket(&mut self, mut data: &[u8]) -> Result<(), Error> {
        while !data.is_empty() {
            let wrote = send_waiting(self.socket, data, 200);
            if wrote == crate::ERR_AGAIN {
                return Err(Error::Timeout);
            }
            if wrote <= 0 {
                return Err(Error::Reset);
            }
            data = &data[wrote as usize..];
        }
        Ok(())
    }
}

/// Прочитать заголовок целиком и вернуть его длину и место, где начинается тело.
///
/// Тело почти всегда начинается **в том же буфере**: сервер отдаёт заголовок и
/// первые килобайты файла одним сегментом.
fn read_header(
    wire: &mut Wire<'_, '_>,
    io: &mut Buffers<'_>,
    header: &mut [u8; MAX_HEADER],
) -> Result<(usize, usize, usize), Error> {
    let deadline = uptime_ms() + HEADER_TIMEOUT_MS;
    let mut filled = 0usize;
    loop {
        if let Some((end, body)) = find_blank_line(&header[..filled]) {
            return Ok((end, body, filled));
        }
        if filled == header.len() {
            return Err(Error::HugeHeader);
        }
        let read = wire.read(io.wire, &mut header[filled..])?;
        if read > 0 {
            filled += read;
            continue;
        }
        // Собеседник закрылся, не дописав заголовка: это не «ответ кончился», а
        // «ответа не было».
        if wire.finished() {
            return Err(Error::NotHttp);
        }
        if uptime_ms() > deadline {
            return Err(Error::Timeout);
        }
        sleep_ms(2);
    }
}

/// Найти пустую строку: конец заголовка и начало тела.
///
/// Возвращает длину заголовка (без пустой строки) и смещение тела. Понимает и
/// `\r\n\r\n`, и `\n\n`: второй вариант встречается у самодельных серверов, а
/// отказ разобрать его выглядел бы как «файл не качается».
fn find_blank_line(bytes: &[u8]) -> Option<(usize, usize)> {
    for at in 0..bytes.len() {
        if bytes[at..].starts_with(b"\r\n\r\n") {
            return Some((at, at + 4));
        }
        if bytes[at..].starts_with(b"\n\n") {
            return Some((at, at + 2));
        }
    }
    None
}

/// Код из строки состояния `HTTP/1.1 200 OK`.
fn status_of(head: &str) -> Result<u16, Error> {
    let line = head.lines().next().ok_or(Error::NotHttp)?;
    if !line.starts_with("HTTP/1.") {
        return Err(Error::NotHttp);
    }
    let mut fields = line.split_whitespace();
    let _ = fields.next();
    fields
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or(Error::NotHttp)
}

/// Код, который означает «файл в другом месте».
///
/// `303` в этом списке потому, что сервер вправе ответить им и на `GET`; `307`
/// и `308` — потому, что ими GitHub переадресует на CDN.
const fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Есть ли такой заголовок (имя пишется строчными, со двоеточием).
fn has_header(head: &str, name: &str) -> bool {
    head.lines().skip(1).any(|line| starts_with_ignoring_case(line, name))
}

/// Значение заголовка по имени.
fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    for line in head.lines().skip(1) {
        if starts_with_ignoring_case(line, name) {
            let (_, value) = line.split_once(':')?;
            return Some(value.trim());
        }
    }
    None
}

/// Значение `Content-Length`.
fn content_length(head: &str) -> Option<u64> {
    header_value(head, "content-length:")?.parse::<u64>().ok()
}

/// Имена заголовков нечувствительны к регистру — это требование RFC 9110, а не
/// вежливость: `Content-Length` и `content-length` пишут разные серверы.
fn starts_with_ignoring_case(line: &str, lowercase: &str) -> bool {
    let line = line.as_bytes();
    let want = lowercase.as_bytes();
    if line.len() < want.len() {
        return false;
    }
    line[..want.len()]
        .iter()
        .zip(want)
        .all(|(have, want)| have.to_ascii_lowercase() == *want)
}

/// Сложить строку из кусков; `false`, если не поместилось.
fn push_all(out: &mut Location, parts: &[&str]) -> bool {
    let total: usize = parts.iter().map(|part| part.len()).sum();
    if total > out.buffer.len() {
        return false;
    }
    out.len = 0;
    for part in parts {
        out.buffer[out.len..out.len + part.len()].copy_from_slice(part.as_bytes());
        out.len += part.len();
    }
    true
}

/// Разобрать адрес вида `10.0.2.2`.
#[must_use]
pub fn parse_ip(text: &str) -> Option<u32> {
    let mut bytes = [0u8; 4];
    let mut seen = 0;
    for (index, part) in text.split('.').enumerate() {
        if index >= 4 {
            return None;
        }
        bytes[index] = part.parse::<u8>().ok()?;
        seen = index + 1;
    }
    (seen == 4).then(|| u32::from_be_bytes(bytes))
}
