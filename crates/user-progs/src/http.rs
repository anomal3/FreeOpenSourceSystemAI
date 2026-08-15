//! Клиент HTTP/1.1: ровно столько, сколько нужно, чтобы забрать файл.
//!
//! # Почему в программе, а не в ядре
//!
//! Потому что качать умеет и третье кольцо: у него есть сокеты, файлы и время.
//! В ядре HTTP оказался бы разбором чужого текста в самом привилегированном
//! месте системы — ради задачи, которая не требует ни одного права ядра.
//!
//! # Что этот клиент умеет и чего не умеет
//!
//! Умеет: `GET`, разбор строки состояния, `Content-Length`, тело потоком —
//! кусками, наружу, не держа его в памяти (образ системы — десятки мегабайт, а
//! кучи у программы нет вовсе).
//!
//! Не умеет — и говорит об этом вслух, а не молчит:
//!
//! * **Разбиение на куски** (`Transfer-Encoding: chunked`). Наш сервер отдаёт
//!   файл с длиной; чужой, отдавший кусками, получит внятный отказ вместо
//!   тела, в котором посреди данных лежат шестнадцатеричные числа.
//! * **Переадресацию**. Код `301` — это ответ «файл в другом месте», и пойти
//!   туда автоматически значит позволить серверу увести клиента куда угодно.
//!   Человеку сообщается код и адрес из `Location`.
//! * **HTTPS**. Его нет по решению фазы 39: доверие даёт подпись, а не канал.
//!   TLS придёт фазой 39a, и тогда у этого модуля появится второй транспорт, а
//!   не второй разбор.
//!
//! # Соединение закрывается сервером
//!
//! В запросе стоит `Connection: close`: одно соединение — один файл. Держать его
//! живым имеет смысл там, где запросов десятки; здесь их три, а поддержка
//! `keep-alive` означала бы уметь понимать, где кончился ответ, ещё одним
//! способом.

use crate::{
    close_socket, connect, recv, send_waiting, shutdown, sleep_ms, stream, stream_state,
    uptime_ms, wait_connected,
};

/// Сколько ждать установления связи.
const CONNECT_TIMEOUT_MS: u64 = 15_000;

/// Сколько ждать **первого** байта ответа.
///
/// Отдельно от общего срока: сервер, который не ответил вовсе, и сервер,
/// который медленно отдаёт сто мегабайт, — разные неисправности, и говорить о
/// них надо разными словами.
const HEADER_TIMEOUT_MS: u64 = 20_000;

/// Сколько ждать продолжения тела, если оно перестало идти.
const BODY_STALL_MS: u64 = 30_000;

/// Сколько байт заголовка ответа клиент согласен прочитать.
///
/// Два килобайта при десятке нужных строк — запас на чужой сервер, который
/// любит рассказывать о себе. Предел существует потому, что заголовок приходит
/// **до** того, как стало известно хоть что-нибудь: без него сервер, отдающий
/// бесконечный поток строк, съел бы память программы, ничего ей не сообщив.
const MAX_HEADER: usize = 2 * 1024;

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
        }
    }
}

/// Что сказал сервер и сколько байт тела приехало.
#[derive(Debug, Clone, Copy)]
pub struct Response {
    pub status: u16,
    pub length: u64,
}

/// Забрать файл по адресу и отдать тело кусками.
///
/// `sink` возвращает `false`, если принять кусок не удалось, — тогда загрузка
/// прекращается с [`Error::SinkFailed`]. Так сюда попадает единственная
/// настоящая ошибка потребителя: на разделе состояния кончилось место.
///
/// `scratch` — рабочий буфер вызывающего. Своего у модуля нет намеренно: кучи
/// нет, а статик внутри библиотеки означал бы, что две загрузки в одной
/// программе портят друг другу данные.
pub fn get(
    address: u32,
    port: u16,
    host: &str,
    path: &str,
    scratch: &mut [u8],
    sink: &mut impl FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    let socket = stream();
    if socket < 0 {
        return Err(Error::NoSocket);
    }
    let result = exchange(socket, address, port, host, path, scratch, sink);
    close_socket(socket);
    result
}

fn exchange(
    socket: i64,
    address: u32,
    port: u16,
    host: &str,
    path: &str,
    scratch: &mut [u8],
    sink: &mut impl FnMut(&[u8]) -> bool,
) -> Result<Response, Error> {
    if connect(socket, address, port) < 0 {
        return Err(Error::NoAnswer);
    }
    if !wait_connected(socket, CONNECT_TIMEOUT_MS) {
        return Err(Error::NoAnswer);
    }

    // Запрос уходит кусками, а не собирается в буфер: собирать его пришлось бы
    // форматированием, которого у программы без кучи нет, а TCP всё равно
    // склеит их в один сегмент — или не склеит, и сервер прочитает столько,
    // сколько приехало. Разницы для протокола нет.
    for part in [
        "GET ",
        path,
        " HTTP/1.1\r\nHost: ",
        host,
        "\r\nUser-Agent: FreeOS-sysupdate/1\r\nAccept: */*\r\nConnection: close\r\n\r\n",
    ] {
        write_all(socket, part.as_bytes())?;
    }
    // Сказать «я всё» нельзя: половина закрывается вместе с возможностью
    // получить ответ у собеседников, читающих до `FIN`. HTTP этого и не требует
    // — конец запроса виден по пустой строке.

    let mut header = [0u8; MAX_HEADER];
    let (header_len, body_start, filled) = read_header(socket, &mut header)?;
    let head = core::str::from_utf8(&header[..header_len]).map_err(|_| Error::NotHttp)?;
    let status = status_of(head)?;
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
        let read = recv(socket, scratch);
        if read > 0 {
            let chunk = &scratch[..read as usize];
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
        // Приёмная очередь пуста — только теперь имеет смысл смотреть на
        // состояние: закрытая половина при непустой очереди означает «дочитай
        // то, что уже пришло», а не «данных больше не будет».
        match stream_state(socket) {
            Some(state) if state.reset != 0 => return Err(Error::Reset),
            Some(state) if state.peer_closed != 0 => {
                return Err(Error::Short { got: done, want: length });
            }
            _ => {}
        }
        if uptime_ms().saturating_sub(quiet_since) > BODY_STALL_MS {
            return Err(Error::Timeout);
        }
        sleep_ms(2);
    }

    // Своя половина закрывается в конце: сервер уже сказал всё, что собирался, и
    // `FIN` от нас — это единственный способ дать ему закрыть соединение, не
    // дожидаясь своего таймаута.
    shutdown(socket);
    Ok(Response { status, length: done })
}

/// Отправить всё, повторяя попытки, пока выясняется аппаратный адрес.
///
/// «Ещё не сейчас» отделено от «оборвалось» намеренно: первое означает, что ARP
/// не ответил, второе — что соединения больше нет, и человеку с этими двумя
/// сообщениями идти в разные стороны.
fn write_all(socket: i64, mut data: &[u8]) -> Result<(), Error> {
    while !data.is_empty() {
        let wrote = send_waiting(socket, data, 200);
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

/// Прочитать заголовок целиком и вернуть его длину и место, где начинается тело.
///
/// Тело почти всегда начинается **в том же буфере**: сервер отдаёт заголовок и
/// первые килобайты файла одним сегментом.
fn read_header(
    socket: i64,
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
        let read = recv(socket, &mut header[filled..]);
        if read > 0 {
            filled += read as usize;
            continue;
        }
        match stream_state(socket) {
            Some(state) if state.reset != 0 => return Err(Error::Reset),
            // Собеседник закрылся, не дописав заголовка: это не «ответ кончился»,
            // а «ответа не было».
            Some(state) if state.peer_closed != 0 && read == 0 => return Err(Error::NotHttp),
            _ => {}
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

/// Есть ли такой заголовок (имя пишется строчными, со двоеточием).
fn has_header(head: &str, name: &str) -> bool {
    head.lines().skip(1).any(|line| starts_with_ignoring_case(line, name))
}

/// Значение `Content-Length`.
fn content_length(head: &str) -> Option<u64> {
    for line in head.lines().skip(1) {
        if starts_with_ignoring_case(line, "content-length:") {
            let (_, value) = line.split_once(':')?;
            return value.trim().parse::<u64>().ok();
        }
    }
    None
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
