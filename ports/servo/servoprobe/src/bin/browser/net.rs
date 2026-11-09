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
}
