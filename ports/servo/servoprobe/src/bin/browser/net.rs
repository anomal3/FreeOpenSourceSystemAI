//! HTTP(S) для синхронизации: HTTP/1.1 по одному запросу на соединение
//! (`Connection: close`), TLS — rustls (тот же, что у Servo, с теми же
//! встроенными корнями Mozilla `webpki-roots`), сокеты — `std::net`.
//!
//! Без tokio и hyper: поток синхронизации и так отдельный, запросов мало, а
//! синхронный код проще проверить. Перенаправления — до пяти; на чужой
//! сервер заголовок `Authorization` не уходит (Яндекс отдаёт файлы через 302
//! на другой адрес, и токен туда не нужен).

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use browser_sync::remote::{Http, Request, Response};

pub struct Client;

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("TLS versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

struct Target {
    tls: bool,
    host: String,
    port: u16,
    path: String,
}

fn parse_target(url: &str) -> Result<Target, String> {
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("адрес без схемы: {url}"))?;
    let tls = match scheme {
        "https" => true,
        "http" => false,
        _ => return Err(format!("схема {scheme} не поддерживается")),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) if !h.contains(']') || h.ends_with(']') => (h, p.parse().map_err(|_| format!("порт: {p}"))?),
        _ => (authority, if tls { 443 } else { 80 }),
    };
    Ok(Target { tls, host: host.trim_matches(|c| c == '[' || c == ']').to_string(), port, path: path.to_string() })
}

fn connect(target: &Target) -> Result<TcpStream, String> {
    let addrs: Vec<_> = (target.host.as_str(), target.port)
        .to_socket_addrs()
        .map_err(|e| format!("DNS {}: {e}", target.host))?
        .collect();
    let mut last = String::from("нет адресов");
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, Duration::from_secs(15)) {
            Ok(stream) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(60)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(60)));
                return Ok(stream);
            }
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(format!("соединение с {}: {last}", target.host))
}

/// Прочитать всё до закрытия. TLS-сервер, закрывший соединение без
/// `close_notify`, — обычное дело; полученное к этому моменту годится.
fn read_all(stream: &mut dyn Read) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !out.is_empty() => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("чтение ответа: {e}")),
        }
        if out.len() > 64 * 1024 * 1024 {
            return Err(String::from("ответ больше 64 МиБ"));
        }
    }
    Ok(out)
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = body.windows(2).position(|w| w == b"\r\n").ok_or("chunked: нет конца строки размера")?;
        let size_text = std::str::from_utf8(&body[..line_end]).map_err(|_| "chunked: размер")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "chunked: размер")?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            return Err(String::from("chunked: оборванный кусок"));
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or(&[]);
    }
}

fn parse_response(raw: &[u8]) -> Result<Response, String> {
    let head_end = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("нет заголовков ответа")?;
    let head = String::from_utf8_lossy(&raw[..head_end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| format!("строка статуса: {status_line}"))?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    let mut body = raw[head_end + 4..].to_vec();
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str());
    if header("Transfer-Encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        body = dechunk(&body)?;
    } else if let Some(len) = header("Content-Length").and_then(|v| v.parse::<usize>().ok()) {
        body.truncate(len);
    }
    Ok(Response { status, headers, body })
}

fn once(request: &Request) -> Result<Response, String> {
    let target = parse_target(&request.url)?;
    let mut head = format!("{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n", request.method, target.path, target.host);
    let mut has_length = false;
    for (k, v) in &request.headers {
        if k.eq_ignore_ascii_case("content-length") {
            has_length = true;
        }
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if !has_length && (!request.body.is_empty() || matches!(request.method.as_str(), "POST" | "PUT" | "PATCH")) {
        head.push_str(&format!("Content-Length: {}\r\n", request.body.len()));
    }
    if !request.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding")) {
        head.push_str("Accept-Encoding: identity\r\n");
    }
    head.push_str("\r\n");
    let tcp = connect(&target)?;
    let raw = if target.tls {
        let name = rustls::pki_types::ServerName::try_from(target.host.clone()).map_err(|e| format!("имя сервера: {e}"))?;
        let conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| format!("TLS: {e}"))?;
        let mut stream = rustls::StreamOwned::new(conn, tcp);
        stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&request.body)).and_then(|_| stream.flush()).map_err(|e| format!("TLS {}: {e}", target.host))?;
        read_all(&mut stream)?
    } else {
        let mut stream = tcp;
        stream.write_all(head.as_bytes()).and_then(|_| stream.write_all(&request.body)).map_err(|e| format!("отправка: {e}"))?;
        read_all(&mut stream)?
    };
    parse_response(&raw)
}

fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    rest.split('/').next().unwrap_or("")
}

impl Http for Client {
    fn send(&self, request: &Request) -> Result<Response, String> {
        let mut current = request.clone();
        for _ in 0..6 {
            let response = once(&current)?;
            if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let Some(location) = response.header("Location") else { return Ok(response) };
            let next = if location.starts_with("http://") || location.starts_with("https://") {
                location.to_string()
            } else {
                let scheme_host = &current.url[..current.url.find("://").map(|i| i + 3).unwrap_or(0) + host_of(&current.url).len()];
                format!("{scheme_host}{}", if location.starts_with('/') { location.to_string() } else { format!("/{location}") })
            };
            if host_of(&next) != host_of(&current.url) {
                current.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
            }
            if response.status == 303 || (matches!(response.status, 301 | 302) && current.method == "POST") {
                current.method = String::from("GET");
                current.body.clear();
                current.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            }
            current.url = next;
        }
        Err(String::from("слишком много перенаправлений"))
    }
}

/// Скачать файл в каталог (загрузки браузера): потоком, с ходом
/// `progress(получено, всего)`, отменой `cancel`. Имя — из
/// `Content-Disposition`, иначе из адреса; пишется в `<имя>.part`, готовое
/// переименовывается. Возвращает путь готового файла.
pub fn download(
    url: &str,
    dir: &std::path::Path,
    progress: &dyn Fn(u64, Option<u64>, &str),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::path::PathBuf, String> {
    use std::sync::atomic::Ordering;
    let mut current = url.to_string();
    for _ in 0..6 {
        let target = parse_target(&current)?;
        let head = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept-Encoding: identity\r\nUser-Agent: Mozilla/5.0 (X11; FreeOS) OBSOS/0.1\r\nAccept: */*\r\n\r\n",
            target.path, target.host
        );
        let tcp = connect(&target)?;
        let mut stream: Box<dyn Read> = if target.tls {
            let name = rustls::pki_types::ServerName::try_from(target.host.clone()).map_err(|e| format!("имя сервера: {e}"))?;
            let conn = rustls::ClientConnection::new(tls_config(), name).map_err(|e| format!("TLS: {e}"))?;
            let mut s = rustls::StreamOwned::new(conn, tcp);
            s.write_all(head.as_bytes()).and_then(|_| s.flush()).map_err(|e| format!("TLS {}: {e}", target.host))?;
            Box::new(s)
        } else {
            let mut s = tcp;
            s.write_all(head.as_bytes()).map_err(|e| format!("отправка: {e}"))?;
            Box::new(s)
        };
        // Заголовки.
        let mut raw = Vec::new();
        let mut buf = [0u8; 32 * 1024];
        let head_end = loop {
            if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break i;
            }
            let n = read_some(&mut *stream, &mut buf)?;
            if n == 0 {
                return Err(String::from("сервер закрыл соединение до заголовков"));
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > 256 * 1024 {
                return Err(String::from("заголовки ответа слишком длинные"));
            }
        };
        let head_text = String::from_utf8_lossy(&raw[..head_end]).to_string();
        let mut rest = raw[head_end + 4..].to_vec();
        let mut lines = head_text.split("\r\n");
        let status: u16 = lines.next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).ok_or("строка статуса")?;
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))).collect();
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = header("location").ok_or("перенаправление без адреса")?;
            current = if location.starts_with("http://") || location.starts_with("https://") {
                location
            } else {
                let base = url::Url::parse(&current).map_err(|e| e.to_string())?;
                base.join(&location).map_err(|e| e.to_string())?.to_string()
            };
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!("сервер ответил {status}"));
        }
        let total = header("content-length").and_then(|v| v.parse::<u64>().ok());
        let chunked = header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let name = header("content-disposition").and_then(|d| disposition_name(&d)).unwrap_or_else(|| name_from_url(&current));
        let _ = std::fs::create_dir_all(dir);
        let path = unique_path(dir, &name);
        let shown = path.file_name().and_then(|n| n.to_str()).unwrap_or("файл").to_string();
        let part = path.with_file_name(format!("{shown}.part"));
        let mut file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut got = 0u64;
        let mut last_report = std::time::Instant::now();
        let mut dechunker = Dechunker::default();
        let mut write = |data: &[u8], got: &mut u64| -> Result<(), String> {
            file.write_all(data).map_err(|e| format!("запись: {e}"))?;
            *got += data.len() as u64;
            Ok(())
        };
        progress(0, total, &shown);
        let mut finished = false;
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = std::fs::remove_file(&part);
                return Err(String::from("отменено"));
            }
            if !rest.is_empty() {
                if chunked {
                    let mut out = Vec::new();
                    finished = dechunker.feed(&rest, &mut out)?;
                    write(&out, &mut got)?;
                } else {
                    let take = total.map(|t| (t - got).min(rest.len() as u64) as usize).unwrap_or(rest.len());
                    write(&rest[..take], &mut got)?;
                    finished = total.is_some_and(|t| got >= t);
                }
                rest.clear();
            }
            if last_report.elapsed() > Duration::from_millis(250) {
                progress(got, total, &shown);
                last_report = std::time::Instant::now();
            }
            if finished {
                break;
            }
            let n = read_some(&mut *stream, &mut buf)?;
            if n == 0 {
                if chunked || total.is_some_and(|t| got < t) {
                    let _ = std::fs::remove_file(&part);
                    return Err(format!("соединение оборвалось на {got} байт"));
                }
                break;
            }
            rest.extend_from_slice(&buf[..n]);
        }
        drop(file);
        std::fs::rename(&part, &path).map_err(|e| format!("переименование: {e}"))?;
        progress(got, Some(got), &shown);
        return Ok(path);
    }
    Err(String::from("слишком много перенаправлений"))
}

fn read_some(stream: &mut dyn Read, buf: &mut [u8]) -> Result<usize, String> {
    loop {
        match stream.read(buf) {
            Ok(n) => return Ok(n),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(0),
            Err(e) => return Err(format!("чтение: {e}")),
        }
    }
}

/// Разбор `Transfer-Encoding: chunked` потоком (куски приходят как попало).
#[derive(Default)]
struct Dechunker {
    pending: Vec<u8>,
    /// Сколько байт данных текущего куска ещё впереди; `None` — ждём строку размера.
    left: Option<usize>,
}

impl Dechunker {
    /// `true` — встретился последний (нулевой) кусок.
    fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<bool, String> {
        self.pending.extend_from_slice(data);
        loop {
            match self.left {
                None => {
                    let Some(end) = self.pending.windows(2).position(|w| w == b"\r\n") else { return Ok(false) };
                    let text = std::str::from_utf8(&self.pending[..end]).map_err(|_| "chunked: размер")?;
                    let text = text.split(';').next().unwrap_or("").trim();
                    if text.is_empty() {
                        // Пустая строка — хвост прошлого куска.
                        self.pending.drain(..end + 2);
                        continue;
                    }
                    let size = usize::from_str_radix(text, 16).map_err(|_| "chunked: размер")?;
                    self.pending.drain(..end + 2);
                    if size == 0 {
                        return Ok(true);
                    }
                    self.left = Some(size);
                }
                Some(left) => {
                    if self.pending.is_empty() {
                        return Ok(false);
                    }
                    let take = left.min(self.pending.len());
                    out.extend_from_slice(&self.pending[..take]);
                    self.pending.drain(..take);
                    self.left = if take == left { None } else { Some(left - take) };
                }
            }
        }
    }
}

/// `attachment; filename="a b.zip"` / `filename*=UTF-8''%D1%84.txt`.
fn disposition_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        if let Some(v) = part.strip_prefix("filename*=") {
            let v = v.trim_matches('"');
            let encoded = v.split_once("''").map(|(_, e)| e).unwrap_or(v);
            let decoded = url::form_urlencoded::parse(format!("x={}", encoded.replace('+', "%2B")).as_bytes()).next().map(|(_, v)| v.into_owned());
            if let Some(name) = decoded.and_then(|n| clean_name(&n)) {
                return Some(name);
            }
        } else if let Some(v) = part.strip_prefix("filename=") {
            plain = clean_name(v.trim_matches('"'));
        }
    }
    plain
}

fn name_from_url(url: &str) -> String {
    let parsed = url::Url::parse(url).ok();
    let last = parsed
        .as_ref()
        .and_then(|u| u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)))
        .filter(|s| !s.is_empty())
        .map(|s| url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes()).next().map(|(_, v)| v.into_owned()).unwrap_or(s));
    last.and_then(|n| clean_name(&n)).unwrap_or_else(|| String::from("download"))
}

/// Имя без каталогов и управляющих знаков.
fn clean_name(name: &str) -> Option<String> {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name = name.trim().trim_start_matches('.').to_string();
    (!name.is_empty() && name.len() <= 200).then_some(name)
}

/// `файл.zip`, а если занято — `файл (1).zip`, `файл (2).zip`…
fn unique_path(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..1000).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap_or(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_and_length() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n5;x=y\r\npedia\r\n0\r\n\r\n";
        assert_eq!(parse_response(raw).unwrap().body, b"Wikipedia");
        let raw = b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nnoEXTRA";
        let r = parse_response(raw).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (404, &b"no"[..]));
        let t = parse_target("https://webdav.yandex.ru/OBSOS/x?y=1").unwrap();
        assert_eq!((t.tls, t.host.as_str(), t.port, t.path.as_str()), (true, "webdav.yandex.ru", 443, "/OBSOS/x?y=1"));
        let t = parse_target("http://10.0.2.2:8080").unwrap();
        assert_eq!((t.port, t.path.as_str()), (8080, "/"));
    }

    #[test]
    fn streaming_dechunk_and_names() {
        let mut d = Dechunker::default();
        let mut out = Vec::new();
        assert!(!d.feed(b"4\r\nWi", &mut out).unwrap());
        assert!(!d.feed(b"ki\r\n5;x\r\npedia\r", &mut out).unwrap());
        assert!(d.feed(b"\n0\r\n\r\n", &mut out).unwrap());
        assert_eq!(out, b"Wikipedia");
        assert_eq!(disposition_name("attachment; filename=\"a b.zip\"").as_deref(), Some("a b.zip"));
        assert_eq!(disposition_name("attachment; filename*=UTF-8''%D1%84.txt").as_deref(), Some("ф.txt"));
        assert_eq!(name_from_url("https://x.org/dl/../setup%20v2.exe?x=1"), "setup v2.exe");
        assert_eq!(name_from_url("https://x.org/"), "download");
        assert_eq!(clean_name("../../etc/passwd").as_deref(), Some("passwd"));
    }
}
