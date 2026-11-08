//! Удалённые хранилища синхронизации (N10): Git-площадки (GitHub, GitLab,
//! Bitbucket — точки синхронизации там теги), облачные диски (Яндекс Диск,
//! Google Drive, любой WebDAV — точки там копии папки `devices/` в
//! `points/<имя>/`) и обычная папка (флешка, сетевой диск, тесты).
//!
//! Сеть — снаружи, за типажом [`Http`]: крейт остаётся без зависимостей, а
//! каждая площадка проверяется тестами на поддельном сервере (`fake.rs`).
//!
//! Раскладка на площадке та же, что у [`crate::Store`]: `devices/<id>/…`.
//! Каждое устройство пишет только в свой каталог, поэтому конфликтов записи
//! нет и блокировки не нужны.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{self, Value};

pub mod bitbucket;
#[cfg(feature = "std")]
pub mod folder;
pub mod gdrive;
pub mod github;
pub mod gitlab;
pub mod oauth;
pub mod webdav;

#[cfg(test)]
pub(crate) mod fake;

// ── HTTP ────────────────────────────────────────────────────────────────────

/// Запрос. Тело — байты (в API площадок всё текстовое, но WebDAV и загрузки
/// бинарные не запрещают).
#[derive(Clone, Debug, Default)]
pub struct Request {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn new(method: &str, url: impl Into<String>) -> Self {
        Request { method: method.to_string(), url: url.into(), headers: Vec::new(), body: Vec::new() }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    pub fn json(self, value: &Value) -> Self {
        self.header("Content-Type", "application/json").body(value.to_text().into_bytes())
    }

    pub fn form(self, fields: &[(&str, &str)]) -> Self {
        let body = form_encode(fields);
        self.header("Content-Type", "application/x-www-form-urlencoded").body(body.into_bytes())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn json(&self) -> Result<Value, Error> {
        json::parse(&self.text()).map_err(|e| Error::Protocol(format!("JSON at {}: {}", e.0, clip(&self.text(), 120))))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Транспорт. Обязан сам ходить по перенаправлениям (Яндекс отдаёт файлы
/// через 302) и не бросать ответ с кодом ошибки — это обычный `Response`.
pub trait Http {
    fn send(&self, request: &Request) -> Result<Response, String>;
}

// ── Ошибки ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Сеть недоступна, тайм-аут, TLS.
    Network(String),
    /// Токен не подошёл или истёк (401/403) — нужен вход заново.
    Auth(String),
    /// Нет репозитория, папки, точки.
    NotFound(String),
    /// Площадка ответила не так, как обещает её API.
    Protocol(String),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Network(s) => write!(f, "сеть: {s}"),
            Error::Auth(s) => write!(f, "вход: {s}"),
            Error::NotFound(s) => write!(f, "не найдено: {s}"),
            Error::Protocol(s) => write!(f, "ответ площадки: {s}"),
        }
    }
}

/// Ответ с кодом ошибки → `Error` (с началом тела — для журнала).
pub fn status_error(what: &str, response: &Response) -> Error {
    let detail = format!("{what}: HTTP {} {}", response.status, clip(&response.text(), 160));
    match response.status {
        401 | 403 => Error::Auth(detail),
        404 => Error::NotFound(detail),
        _ => Error::Protocol(detail),
    }
}

pub(crate) fn send(http: &dyn Http, request: &Request) -> Result<Response, Error> {
    http.send(request).map_err(Error::Network)
}

/// Отправить и потребовать 2xx.
pub(crate) fn send_ok(http: &dyn Http, request: &Request, what: &str) -> Result<Response, Error> {
    let response = send(http, request)?;
    if response.ok() { Ok(response) } else { Err(status_error(what, &response)) }
}

// ── Площадка ────────────────────────────────────────────────────────────────

/// Файл на площадке: имя в каталоге устройства и «версия» — хеш, etag или
/// дата изменения (пустая — площадка не сказала, читать всегда).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteFile {
    pub name: String,
    pub version: String,
}

/// Точка синхронизации: тег Git или копия папки на диске.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPoint {
    pub name: String,
    /// Подпись пользователя (сообщение тега); может быть пустой.
    pub label: String,
}

/// Удалённое хранилище. `at` — имя точки синхронизации: читать состояние на
/// тот момент (откат); `None` — текущее.
pub trait Remote {
    /// Создать репозиторий или папку, если их нет. Повторный вызов безвреден.
    fn prepare(&mut self) -> Result<(), Error>;
    /// Устройства — подкаталоги `devices/`.
    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error>;
    /// Файлы каталога устройства.
    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error>;
    /// Содержимое файла (`devices/<id>/<имя>`); нет — `Ok(None)`.
    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error>;
    /// Записать файлы (на Git-площадках — одним или несколькими коммитами).
    fn write(&mut self, files: &[(String, String)], message: &str) -> Result<(), Error>;
    /// Отметить текущее состояние точкой синхронизации.
    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error>;
    /// Точки синхронизации, новые первыми.
    fn points(&mut self) -> Result<Vec<SyncPoint>, Error>;
}

// ── Учётная запись ──────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    GitHub,
    GitLab,
    Bitbucket,
    YandexDisk,
    GoogleDrive,
    WebDav,
    Folder,
}

impl Provider {
    pub const ALL: [Provider; 7] = [
        Provider::GitHub,
        Provider::GitLab,
        Provider::Bitbucket,
        Provider::YandexDisk,
        Provider::GoogleDrive,
        Provider::WebDav,
        Provider::Folder,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Provider::GitHub => "github",
            Provider::GitLab => "gitlab",
            Provider::Bitbucket => "bitbucket",
            Provider::YandexDisk => "yandex",
            Provider::GoogleDrive => "gdrive",
            Provider::WebDav => "webdav",
            Provider::Folder => "folder",
        }
    }

    pub fn from_id(id: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            Provider::GitHub => "GitHub",
            Provider::GitLab => "GitLab",
            Provider::Bitbucket => "Bitbucket",
            Provider::YandexDisk => "Яндекс Диск",
            Provider::GoogleDrive => "Google Диск",
            Provider::WebDav => "WebDAV (Nextcloud и др.)",
            Provider::Folder => "Папка",
        }
    }

    /// Точки синхронизации — теги Git (а не копии папки).
    pub fn is_git(self) -> bool {
        matches!(self, Provider::GitHub | Provider::GitLab | Provider::Bitbucket)
    }

    /// Адрес API по умолчанию (поле `host` его заменяет: свой GitLab,
    /// Nextcloud).
    pub fn default_host(self) -> &'static str {
        match self {
            Provider::GitHub => "https://api.github.com",
            Provider::GitLab => "https://gitlab.com",
            Provider::Bitbucket => "https://api.bitbucket.org/2.0",
            Provider::YandexDisk => "https://webdav.yandex.ru",
            Provider::GoogleDrive => "https://www.googleapis.com",
            Provider::WebDav | Provider::Folder => "",
        }
    }

    /// Где взять токен или пароль приложения, если вход через браузер
    /// (Device Flow) не настроен.
    pub fn token_page(self) -> &'static str {
        match self {
            Provider::GitHub => "https://github.com/settings/tokens/new?scopes=repo&description=OBSOS",
            Provider::GitLab => "https://gitlab.com/-/user_settings/personal_access_tokens?name=OBSOS&scopes=api",
            Provider::Bitbucket => "https://bitbucket.org/account/settings/app-passwords/new",
            Provider::YandexDisk => "https://id.yandex.ru/security/app-passwords",
            Provider::GoogleDrive => "https://console.cloud.google.com/apis/credentials",
            Provider::WebDav | Provider::Folder => "",
        }
    }
}

/// Учётная запись синхронизации. Хранится зашифрованной (хранилище паролей
/// браузера); здесь — только данные и их текст.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub provider: Provider,
    /// Адрес API; пусто — по умолчанию площадки.
    pub host: String,
    /// Git: `владелец/репозиторий`; диски: папка (`OBSOS`); папка: путь.
    pub repo: String,
    /// Ветка Git (`main`).
    pub branch: String,
    /// Имя пользователя: Bitbucket (для пароля приложения), Яндекс, WebDAV.
    pub user: String,
    /// Токен доступа, пароль приложения.
    pub token: String,
    pub refresh_token: String,
    /// Когда истекает `token` (мс эпохи); 0 — не истекает.
    pub expires_ms: u64,
    /// OAuth-приложение для входа через браузер (Device Flow).
    pub client_id: String,
    pub client_secret: String,
}

impl Account {
    pub fn new(provider: Provider) -> Self {
        Account {
            provider,
            host: String::new(),
            repo: String::from(if provider.is_git() { "" } else { "OBSOS" }),
            branch: String::from("main"),
            user: String::new(),
            token: String::new(),
            refresh_token: String::new(),
            expires_ms: 0,
            client_id: String::new(),
            client_secret: String::new(),
        }
    }

    pub fn host(&self) -> String {
        let host = if self.host.trim().is_empty() { self.provider.default_host() } else { self.host.trim() };
        host.trim_end_matches('/').to_string()
    }

    pub fn branch(&self) -> &str {
        if self.branch.trim().is_empty() { "main" } else { self.branch.trim() }
    }

    pub fn to_json(&self) -> Value {
        let s = |v: &str| Value::Str(v.to_string());
        Value::Obj(alloc::vec![
            (String::from("provider"), s(self.provider.id())),
            (String::from("host"), s(&self.host)),
            (String::from("repo"), s(&self.repo)),
            (String::from("branch"), s(&self.branch)),
            (String::from("user"), s(&self.user)),
            (String::from("token"), s(&self.token)),
            (String::from("refresh_token"), s(&self.refresh_token)),
            (String::from("expires_ms"), Value::Num(self.expires_ms.to_string())),
            (String::from("client_id"), s(&self.client_id)),
            (String::from("client_secret"), s(&self.client_secret)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<Account> {
        let provider = Provider::from_id(v.str_field("provider")?)?;
        let f = |k: &str| v.str_field(k).unwrap_or("").to_string();
        let expires_ms = match v.get("expires_ms") {
            Some(Value::Num(n)) => n.parse().unwrap_or(0),
            _ => 0,
        };
        Some(Account {
            provider,
            host: f("host"),
            repo: f("repo"),
            branch: f("branch"),
            user: f("user"),
            token: f("token"),
            refresh_token: f("refresh_token"),
            expires_ms,
            client_id: f("client_id"),
            client_secret: f("client_secret"),
        })
    }

    /// Готово ли к синхронизации (есть всё, без чего площадка не ответит).
    pub fn check(&self) -> Result<(), String> {
        let need = |ok: bool, what: &str| if ok { Ok(()) } else { Err(what.to_string()) };
        match self.provider {
            Provider::GitHub | Provider::GitLab => {
                need(self.repo.contains('/'), "репозиторий в виде владелец/имя")?;
                need(!self.token.is_empty(), "токен (войдите)")
            }
            Provider::Bitbucket => {
                need(self.repo.contains('/'), "репозиторий в виде рабочее-пространство/имя")?;
                need(!self.token.is_empty(), "пароль приложения или токен")
            }
            Provider::YandexDisk | Provider::WebDav => {
                need(!self.repo.trim().is_empty(), "папка")?;
                need(self.provider == Provider::YandexDisk || !self.host.is_empty(), "адрес WebDAV")?;
                need(!self.user.is_empty() && !self.token.is_empty(), "имя и пароль приложения")
            }
            Provider::GoogleDrive => {
                need(!self.repo.trim().is_empty(), "папка")?;
                need(!self.token.is_empty(), "вход в Google")
            }
            Provider::Folder => need(!self.repo.trim().is_empty(), "путь к папке"),
        }
    }
}

/// Площадка для учётной записи. Папка — снаружи (ей нужна файловая система,
/// см. [`folder::FolderRemote`]); здесь — сетевые.
pub fn open<'a>(account: &Account, http: &'a dyn Http) -> Option<Box<dyn Remote + 'a>> {
    Some(match account.provider {
        Provider::GitHub => Box::new(github::GitHub::new(account, http)),
        Provider::GitLab => Box::new(gitlab::GitLab::new(account, http)),
        Provider::Bitbucket => Box::new(bitbucket::Bitbucket::new(account, http)),
        Provider::YandexDisk | Provider::WebDav => Box::new(webdav::WebDav::new(account, http)),
        Provider::GoogleDrive => Box::new(gdrive::GoogleDrive::new(account, http)),
        Provider::Folder => return None,
    })
}

// ── Мелочи: base64, кодирование адресов, имена ──────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Разбор base64; переводы строк (GitHub режет по 60 знаков) пропускаются.
pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => return None,
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Процентное кодирование: всё, кроме `A-Z a-z 0-9 - _ . ~`, и ещё `keep`.
pub fn url_encode(text: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) || keep.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(core::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn form_encode(fields: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&url_encode(k, b""));
        out.push('=');
        out.push_str(&url_encode(v, b""));
    }
    out
}

/// Разобрать `a=1&b=2` (ответ GitHub без `Accept: json`).
pub fn form_decode(text: &str) -> Vec<(String, String)> {
    text.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (url_decode(k), url_decode(v))
        })
        .collect()
}

/// Имя файла в каталоге устройства, которое мы готовы взять с площадки.
pub fn valid_file(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else { return false };
    crate::replica::valid_name(stem) && matches!(ext, "log" | "snapshot" | "json")
}

/// Имя точки синхронизации: `obsos-ГГГГММДД-ЧЧММСС[-подпись]` (подпись —
/// латиница/цифры, остальное заменено на `-`; имя тега Git и папки).
pub fn point_name(now_ms: u64, label: &str) -> String {
    let (y, mo, d, h, mi, s) = civil(now_ms / 1000);
    let mut name = format!("obsos-{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}");
    let slug: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if !slug.is_empty() {
        name.push('-');
        name.push_str(&slug.chars().take(40).collect::<String>());
    }
    name
}

/// Секунды эпохи → (год, месяц, день, час, минута, секунда), UTC.
pub fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant, days_from_civil наоборот.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (rem / 3600) as u32, (rem / 60 % 60) as u32, (rem % 60) as u32)
}

/// Обрезать текст для сообщения об ошибке.
pub fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out.replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", "Привет".as_bytes()] {
            let text = b64_encode(data);
            assert_eq!(b64_decode(&text).unwrap(), data);
        }
        assert_eq!(b64_encode(b"foobar"), "Zm9vYmFy");
        assert_eq!(b64_decode("Zm9v\nYmFy\n").unwrap(), b"foobar");
    }

    #[test]
    fn url_coding() {
        assert_eq!(url_encode("a b/ц", b"/"), "a%20b/%D1%86");
        assert_eq!(url_decode("a%20b%2Fc+d"), "a b/c d");
        assert_eq!(form_decode("access_token=x%2Fy&scope=repo"), alloc::vec![
            (String::from("access_token"), String::from("x/y")),
            (String::from("scope"), String::from("repo")),
        ]);
    }

    #[test]
    fn point_names() {
        // 2026-11-08 16:30:05 UTC
        let ms = 1_794_155_405_000;
        assert_eq!(civil(ms / 1000), (2026, 11, 8, 16, 30, 5));
        assert_eq!(point_name(ms, ""), "obsos-20261108-163005");
        assert_eq!(point_name(ms, "До переустановки! v2"), "obsos-20261108-163005-v2");
        assert_eq!(point_name(ms, "before reinstall"), "obsos-20261108-163005-before-reinstall");
    }

    #[test]
    fn account_round_trip() {
        let mut a = Account::new(Provider::GitLab);
        a.repo = String::from("roman/obsos-sync");
        a.token = String::from("glpat-\"x\"");
        a.expires_ms = 42;
        let back = Account::from_json(&json::parse(&a.to_json().to_text()).unwrap()).unwrap();
        assert_eq!(back, a);
        assert!(a.check().is_ok());
        assert!(Account::new(Provider::GitHub).check().is_err());
    }

    #[test]
    fn file_names() {
        assert!(valid_file("bookmarks.log"));
        assert!(valid_file("tabs.json"));
        assert!(!valid_file("../x.log"));
        assert!(!valid_file("evil.sh"));
    }
}
