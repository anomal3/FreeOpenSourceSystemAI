// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Голова сообщения HTTP/1.1: строка запроса, строка ответа и поля заголовка.
//!
//! # Почему отдельный крейт
//!
//! Потому что читателей у этого разбора трое, и живут они по разные стороны
//! системы: сервер `/bin/httpd` разбирает **запрос**, клиент `sysupdate` и тот
//! же сервер в роли обратного прокси разбирают **ответ**, а фаззер зовёт и то,
//! и другое с испорченными байтами. Две реализации одного разбора разошлись бы
//! молча — и разошлись бы именно там, где это опаснее всего: в проксировании,
//! где сообщение разбирают дважды, мы и тот, кому мы его передаём.
//!
//! Здесь только текст. Ни сокетов, ни файлов, ни времени: всё, что требует
//! знать систему, делает тот, кто зовёт. Поэтому разбор проверяется `cargo
//! test` за секунды и фаззится на хосте, а не в эмуляторе.
//!
//! # Строгость — не придирчивость, а защита от расхождения
//!
//! Обычный сервер может позволить себе быть терпимым к кривому запросу: он сам
//! себе последняя инстанция. Прокси — не может. Если мы посчитали границу
//! сообщения в одном месте, а тот, кому мы его передали, — в другом, то между
//! нами появляется «лишний» запрос, которого не посылал никто: это и есть
//! контрабанда запросов (request smuggling). Поэтому:
//!
//! * строки кончаются **только** `\r\n`; одинокий `\n` — отказ, а не «ну
//!   понятно же». Именно на разном отношении к одинокому `\n` реализации
//!   расходятся чаще всего;
//! * `Content-Length` дважды — отказ, даже если числа совпадают;
//! * `Content-Length` вместе с `Transfer-Encoding` — отказ;
//! * пробел перед двоеточием (`Name : value`) — отказ;
//! * продолжение поля со следующей строки (obs-fold) — отказ;
//! * управляющие знаки внутри значения — отказ.
//!
//! Цена названа вслух: клиент, посылающий одинокий `\n`, получит `400` вместо
//! страницы. Такого клиента не существует — `\r\n` пишут все, — а вот
//! расхождение с прокси существует и стоит дорого.
//!
//! # Чего здесь нет
//!
//! Тела. Совсем: этот крейт разбирает только голову и говорит, **как** тело
//! ограничено ([`Body`]); читает его тот, у кого есть сокет. Нет и разбиения на
//! куски (`Transfer-Encoding: chunked`) — оно опознаётся и отвергается, потому
//! что ни наш сервер, ни наш клиент так не отдают и так не читают.

#![no_std]

pub mod mime;
pub mod path;

#[cfg(test)]
mod tests;

/// Чем кончился разбор головы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// В голове байты, не складывающиеся в UTF-8.
    NotText,
    /// Строка кончилась не на `\r\n`.
    BadLineEnd,
    /// Первая строка не разбирается: не три части, не та форма.
    BadStart,
    /// Версия не `HTTP/1.0` и не `HTTP/1.1`.
    BadVersion,
    /// Код ответа не трёхзначное число.
    BadStatus,
    /// Поле заголовка без двоеточия, с пробелом перед ним или со свёрткой.
    BadField,
    /// `Content-Length` не число или не помещается.
    BadLength,
    /// Два `Content-Length`, либо длина вместе с `Transfer-Encoding`.
    Conflicting,
}

impl Error {
    /// Что сказать человеку. По-английски: это уезжает в журнал.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::NotText => "the head is not text",
            Self::BadLineEnd => "a line does not end with CRLF",
            Self::BadStart => "the first line is not a request or an answer",
            Self::BadVersion => "that HTTP version is not 1.0 or 1.1",
            Self::BadStatus => "the status code is not three digits",
            Self::BadField => "a header field is malformed",
            Self::BadLength => "Content-Length is not a number",
            Self::Conflicting => "the message says its length in two different ways",
        }
    }
}

/// Версия протокола. Больше двух нам взять неоткуда и незачем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Http10,
    Http11,
}

impl Version {
    fn parse(text: &str) -> Result<Self, Error> {
        match text {
            "HTTP/1.1" => Ok(Self::Http11),
            "HTTP/1.0" => Ok(Self::Http10),
            _ => Err(Error::BadVersion),
        }
    }

    /// Как эта версия пишется в сообщении.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Http10 => "HTTP/1.0",
            Self::Http11 => "HTTP/1.1",
        }
    }
}

/// Метод запроса.
///
/// По имени названы те два, которые сервер выполняет; остальные существуют как
/// [`Method::Other`] — ответ на них один и тот же (`405`), и различать их
/// значило бы делать вид, что мы их умеем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Other,
}

/// Как ограничено тело сообщения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// Тела нет.
    None,
    /// Тело длиной в столько байт.
    Length(u64),
    /// Тело кусками. Мы так не умеем — но опознаём и говорим об этом.
    Chunked,
    /// Тело кончается закрытием соединения (так отвечает `HTTP/1.0`).
    ToClose,
}

/// Где кончается голова сообщения.
///
/// Возвращает длину головы вместе с завершающей пустой строкой, то есть
/// смещение первого байта тела. `None` — головы ещё нет целиком, и это не
/// ошибка отправителя, а «подожди ещё»: сообщение приезжает сегментами, и в
/// первом из них голова редко помещается целиком.
#[must_use]
pub fn head_end(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4 {
        return None;
    }
    bytes.windows(4).position(|window| window == b"\r\n\r\n").map(|at| at + 4)
}

/// Голова сообщения: первая строка и поля.
#[derive(Debug, Clone, Copy)]
pub struct Head<'a> {
    first: &'a str,
    /// Поля, разделённые `\r\n`; завершающей пустой строки уже нет.
    fields: &'a str,
}

impl<'a> Head<'a> {
    /// Разобрать голову — то, что отрезал [`head_end`].
    ///
    /// Терпит и отсутствие завершающей пустой строки: так удобнее звать из
    /// проверок. Всё остальное — строго.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let text = core::str::from_utf8(bytes).map_err(|_| Error::NotText)?;
        let text = text
            .strip_suffix("\r\n\r\n")
            .or_else(|| text.strip_suffix("\r\n"))
            .unwrap_or(text);
        let (first, fields) = match text.split_once("\r\n") {
            Some((first, rest)) => (first, rest),
            None => (text, ""),
        };
        if first.is_empty() {
            return Err(Error::BadStart);
        }
        let head = Self { first, fields };
        head.check()?;
        Ok(head)
    }

    /// Проверить форму всех строк один раз, при разборе.
    ///
    /// Один раз — чтобы читающий поле не был обязан помнить о проверке: к
    /// моменту, когда у [`Head`] что-то спрашивают, она уже прошла.
    fn check(&self) -> Result<(), Error> {
        if self.first.contains(['\r', '\n']) {
            return Err(Error::BadLineEnd);
        }
        // Сообщение без единого поля законно: `split` по пустой строке отдал бы
        // одну пустую строку, и такое сообщение было бы отвергнуто ни за что.
        if self.fields.is_empty() {
            return Ok(());
        }
        for line in self.fields.split("\r\n") {
            // Пустая строка посреди полей означала бы, что голова кончилась
            // раньше, чем думает вызывающий, — то есть что дальше идёт тело,
            // которое мы вот-вот разберём как заголовки.
            if line.is_empty() {
                return Err(Error::BadField);
            }
            if line.contains(['\r', '\n']) {
                return Err(Error::BadLineEnd);
            }
            // Строка, начинающаяся с пробела, — это продолжение предыдущего
            // поля (obs-fold). Стандарт объявил его устаревшим, а прокси на нём
            // расходятся: одни склеивают, другие считают новым полем.
            if line.starts_with([' ', '\t']) {
                return Err(Error::BadField);
            }
            let Some((name, value)) = line.split_once(':') else {
                return Err(Error::BadField);
            };
            if !is_token(name) {
                return Err(Error::BadField);
            }
            if value.bytes().any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f) {
                return Err(Error::BadField);
            }
        }
        Ok(())
    }

    /// Первая строка целиком.
    #[must_use]
    pub const fn first_line(&self) -> &'a str {
        self.first
    }

    /// Все поля по порядку: имя как написано, значение без окружающих пробелов.
    pub fn fields(&self) -> impl Iterator<Item = (&'a str, &'a str)> {
        self.fields.split("\r\n").filter(|line| !line.is_empty()).filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name, value.trim_matches([' ', '\t'])))
        })
    }

    /// Значение поля по имени; регистр имени не важен.
    ///
    /// Первое, если полей с таким именем несколько. Там, где дубликат опасен
    /// (длина тела), он ловится отдельно — см. [`Head::content_length`].
    #[must_use]
    pub fn value(&self, name: &str) -> Option<&'a str> {
        self.fields()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    }

    /// Сколько раз поле названо.
    #[must_use]
    pub fn count(&self, name: &str) -> usize {
        self.fields().filter(|(field, _)| field.eq_ignore_ascii_case(name)).count()
    }

    /// Есть ли в списке через запятую (`Connection: keep-alive, upgrade`) такое
    /// слово.
    #[must_use]
    pub fn lists(&self, name: &str, token: &str) -> bool {
        self.fields()
            .filter(|(field, _)| field.eq_ignore_ascii_case(name))
            .flat_map(|(_, value)| value.split(','))
            .any(|item| item.trim_matches([' ', '\t']).eq_ignore_ascii_case(token))
    }

    /// Объявленная длина тела.
    ///
    /// `Ok(None)` — поля нет. Два поля или не число — отказ: посчитать длину
    /// «как-нибудь» здесь означает разойтись с тем, кому мы сообщение передаём.
    pub fn content_length(&self) -> Result<Option<u64>, Error> {
        match self.count("content-length") {
            0 => Ok(None),
            1 => {
                let value = self.value("content-length").expect("поле посчитано выше");
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(Error::BadLength);
                }
                value.parse::<u64>().map(Some).map_err(|_| Error::BadLength)
            }
            _ => Err(Error::Conflicting),
        }
    }

    /// Сказано ли, что тело идёт кусками.
    #[must_use]
    pub fn is_chunked(&self) -> bool {
        self.lists("transfer-encoding", "chunked")
    }

    /// Останется ли соединение живым после этого сообщения.
    #[must_use]
    pub fn keep_alive(&self, version: Version) -> bool {
        if self.lists("connection", "close") {
            return false;
        }
        match version {
            Version::Http11 => true,
            Version::Http10 => self.lists("connection", "keep-alive"),
        }
    }

    /// Как ограничено тело, если о методе ничего не известно.
    fn framing(&self) -> Result<Body, Error> {
        let chunked = self.is_chunked();
        let length = self.content_length()?;
        if chunked && length.is_some() {
            return Err(Error::Conflicting);
        }
        if chunked {
            return Ok(Body::Chunked);
        }
        match length {
            Some(0) | None => Ok(Body::None),
            Some(length) => Ok(Body::Length(length)),
        }
    }
}

/// Разобранный запрос.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub method: Method,
    /// Цель запроса как прислана: путь вместе с запросом после `?`.
    pub target: &'a str,
    pub version: Version,
    pub head: Head<'a>,
}

impl<'a> Request<'a> {
    /// Разобрать запрос — то, что отрезал [`head_end`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let head = Head::parse(bytes)?;
        let mut parts = head.first_line().split(' ');
        let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(Error::BadStart);
        };
        // Ровно три части. Четвёртая означает пробел внутри цели — а цель с
        // пробелом это уже не одна строка запроса, а две разных: наша и та,
        // которую прочитает тот, кому мы её передадим.
        if parts.next().is_some() {
            return Err(Error::BadStart);
        }
        if !is_token(method) {
            return Err(Error::BadStart);
        }
        // Только origin-form (`/путь`). Форма `http://хост/путь` существует для
        // сквозного прокси, которым мы не являемся: обратный прокси сам решает,
        // куда идти, и позволить собеседнику называть место назначения значило
        // бы открыть свою сеть наружу.
        if !target.starts_with('/') {
            return Err(Error::BadStart);
        }
        let version = Version::parse(version)?;
        let method = match method {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            _ => Method::Other,
        };
        Ok(Self { method, target, version, head })
    }

    /// Как ограничено тело запроса.
    pub fn body(&self) -> Result<Body, Error> {
        self.head.framing()
    }

    /// Путь и строка запроса порознь.
    #[must_use]
    pub fn split_target(&self) -> (&'a str, &'a str) {
        path::split_target(self.target)
    }

    /// Останется ли соединение живым после ответа на этот запрос.
    #[must_use]
    pub fn keep_alive(&self) -> bool {
        self.head.keep_alive(self.version)
    }
}

/// Разобранный ответ.
#[derive(Debug, Clone, Copy)]
pub struct Response<'a> {
    pub version: Version,
    pub status: u16,
    /// Пояснение к коду; бывает пустым, и это законно.
    pub reason: &'a str,
    pub head: Head<'a>,
}

impl<'a> Response<'a> {
    /// Разобрать ответ — то, что отрезал [`head_end`].
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let head = Head::parse(bytes)?;
        let line = head.first_line();
        let (version, rest) = line.split_once(' ').ok_or(Error::BadStart)?;
        let version = Version::parse(version)?;
        let (code, reason) = match rest.split_once(' ') {
            Some((code, reason)) => (code, reason),
            None => (rest, ""),
        };
        if code.len() != 3 || !code.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Error::BadStatus);
        }
        let status = code.parse::<u16>().map_err(|_| Error::BadStatus)?;
        Ok(Self { version, status, reason, head })
    }

    /// Как ограничено тело ответа на запрос методом `method`.
    ///
    /// Метод нужен потому, что тело ограничено не только тем, что написано в
    /// ответе: у ответа на `HEAD` тела нет никогда, сколько бы ни было
    /// объявлено в `Content-Length`. Прокси, забывший об этом, зависает на
    /// чтении тела, которого не будет.
    pub fn body(&self, method: Method) -> Result<Body, Error> {
        if matches!(method, Method::Head) || no_body_status(self.status) {
            return Ok(Body::None);
        }
        match self.head.framing()? {
            // Ни длины, ни кусков: тело кончается закрытием. Так отвечает
            // `HTTP/1.0` и так же выглядит ответ сервера, который решил длину
            // не считать. Соединение после такого ответа не переиспользуют.
            Body::None if self.head.content_length()?.is_none() => Ok(Body::ToClose),
            other => Ok(other),
        }
    }

    /// Останется ли соединение живым после этого ответа.
    #[must_use]
    pub fn keep_alive(&self) -> bool {
        self.head.keep_alive(self.version)
    }
}

/// У этих кодов тела нет по определению, что бы ни было написано в полях.
#[must_use]
pub const fn no_body_status(status: u16) -> bool {
    matches!(status, 100..=199 | 204 | 304)
}

/// Слово протокола: имя поля, метод. Знаки перечислены стандартом; пробела,
/// двоеточия и управляющих среди них нет — на этом и держится разбор.
#[must_use]
pub fn is_token(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | 0x27
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}
