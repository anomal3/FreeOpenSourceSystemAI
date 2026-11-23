//! Проба сети для Proton в FossAi (22.11.2026, hw-log15: Universe of Tanks под
//! Proton пишет «сервер не отвечает», на Windows работает).
//!
//! Шаги - по порядку аргументов, каждый с временем в stderr:
//!
//! * `self` - имя машины (`gethostname`) и его адрес (`getaddrinfo`): Wine
//!   пишет «Failed to resolve your host name IP», если его нет в `/etc/hosts`;
//! * `name ИМЯ` - адрес по DNS (`getaddrinfo`, как у Unity/curl);
//! * `tcp УЗЕЛ:ПОРТ` - соединение TCP (Winsock);
//! * `get URL` - запрос GET через WinHTTP (HTTPS - schannel Wine, корни -
//!   хранилище ROOT crypt32 из `/etc/ssl/certs/ca-certificates.crt`); ждём
//!   ответ 2xx/3xx/4xx;
//! * `get! URL` - то же, но запрос обязан НЕ пройти (чужой корень стенда):
//!   иначе проверка сертификата ничего не проверяет.
//!
//! У адреса-числа (`https://10.0.2.2:2003/`) имя в сертификате не сверяется
//! (сертификат стенда выписан на iPAddress, Wine сверяет dNSName), цепочка до
//! корня - сверяется.
//!
//! Итог: `netprobe: done, all N as expected` или `netprobe: done, K of N NOT as
//! expected`; код выхода - K.

use std::ffi::c_void;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::*;
use windows::Win32::Networking::WinSock::{gethostname, WSAStartup, WSADATA};

fn ms(since: Instant) -> u128 {
    since.elapsed().as_millis()
}

/// `self`: имя машины и его адрес.
fn self_name() -> bool {
    let mut buf = [0u8; 256];
    let started = Instant::now();
    if unsafe { gethostname(&mut buf) } != 0 {
        eprintln!("netprobe: gethostname failed");
        return false;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let host = String::from_utf8_lossy(&buf[..len]).into_owned();
    match (host.as_str(), 0).to_socket_addrs() {
        Ok(found) => {
            let list: Vec<String> = found.map(|a| a.ip().to_string()).collect();
            let took = ms(started);
            eprintln!("netprobe: host name '{host}' -> {} in {took} ms", list.join(" "));
            // Имя машины - из /etc/hosts, без DNS: дольше полсекунды - это DNS.
            took < 500 && !list.is_empty()
        }
        Err(err) => {
            eprintln!("netprobe: host name '{host}' not resolved after {} ms: {err}", ms(started));
            false
        }
    }
}

/// `name ИМЯ`: адрес по DNS.
fn name(host: &str) -> bool {
    let started = Instant::now();
    match (host, 443).to_socket_addrs() {
        Ok(found) => {
            let list: Vec<String> = found.map(|a| a.ip().to_string()).collect();
            eprintln!("netprobe: name {host} -> {} in {} ms", list.join(" "), ms(started));
            !list.is_empty()
        }
        Err(err) => {
            eprintln!("netprobe: name {host} not resolved after {} ms: {err}", ms(started));
            false
        }
    }
}

/// `tcp УЗЕЛ:ПОРТ`: соединение TCP со сроком 10 с.
fn tcp(target: &str) -> bool {
    let started = Instant::now();
    let addr = match target.to_socket_addrs().map(|mut found| found.next()) {
        Ok(Some(addr)) => addr,
        Ok(None) | Err(_) => {
            eprintln!("netprobe: tcp {target}: no address after {} ms", ms(started));
            return false;
        }
    };
    match TcpStream::connect_timeout(&addr, Duration::from_secs(10)) {
        Ok(stream) => {
            eprintln!("netprobe: tcp {target} ({addr}) connected in {} ms", ms(started));
            drop(stream);
            true
        }
        Err(err) => {
            eprintln!("netprobe: tcp {target} ({addr}) failed after {} ms: {err}", ms(started));
            false
        }
    }
}

/// Флаги `WINHTTP_CALLBACK_STATUS_SECURE_FAILURE` последнего запроса.
static SECURE_FAILURE: AtomicU32 = AtomicU32::new(0);

unsafe extern "system" fn status_callback(_handle: *mut c_void, _context: usize, status: u32, info: *mut c_void, len: u32) {
    if status == WINHTTP_CALLBACK_STATUS_SECURE_FAILURE && !info.is_null() && len >= 4 {
        SECURE_FAILURE.store(unsafe { *(info as *const u32) }, Ordering::Relaxed);
    }
}

/// Флаги отказа TLS словами.
fn secure_flags(flags: u32) -> String {
    let names = [
        (0x1, "revocation check failed"),
        (0x2, "invalid certificate"),
        (0x4, "certificate revoked"),
        (0x8, "untrusted root (CA)"),
        (0x10, "name mismatch"),
        (0x20, "date invalid"),
        (0x80000000, "security channel error"),
    ];
    let words: Vec<&str> = names.iter().filter(|(bit, _)| flags & bit != 0).map(|(_, word)| *word).collect();
    if words.is_empty() { format!("{flags:#x}") } else { words.join(", ") }
}

/// Ошибка WinHTTP словами.
fn winhttp_error(code: u32) -> String {
    let word = match code {
        12002 => "timeout",
        12005 => "invalid URL",
        12007 => "name not resolved",
        12029 => "cannot connect",
        12030 => "connection aborted",
        12152 => "invalid server response",
        12175 => "secure failure (TLS/certificate)",
        12157 => "TLS channel error",
        _ => "",
    };
    if word.is_empty() { format!("error {code}") } else { format!("error {code} ({word})") }
}

/// Разобрать `http(s)://узел[:порт]/путь`.
fn split_url(url: &str) -> Option<(bool, String, u16, String)> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse().ok()?),
        None => (authority, if secure { 443 } else { 80 }),
    };
    Some((secure, host.to_string(), port, path.to_string()))
}

/// Код ответа или текст ошибки, байт тела, мс до ответа.
fn fetch(url: &str) -> Result<(u32, usize, u128), String> {
    let (secure, host, port, path) = split_url(url).ok_or_else(|| String::from("bad URL"))?;
    let started = Instant::now();
    SECURE_FAILURE.store(0, Ordering::Relaxed);
    let fail = |what: &str, err: windows::core::Error| {
        let code = (err.code().0 as u32) & 0xFFFF;
        let flags = SECURE_FAILURE.load(Ordering::Relaxed);
        let tls = if flags != 0 { format!(", TLS: {}", secure_flags(flags)) } else { String::new() };
        format!("{what}: {}{tls} after {} ms", winhttp_error(code), ms(started))
    };
    unsafe {
        let session = WinHttpOpen(w!("netprobe/1.0 (FossAi)"), WINHTTP_ACCESS_TYPE_NO_PROXY, PCWSTR::null(), PCWSTR::null(), 0);
        if session.is_null() {
            return Err(fail("WinHttpOpen", windows::core::Error::from_thread()));
        }
        WinHttpSetTimeouts(session, 10_000, 10_000, 10_000, 15_000).map_err(|e| fail("WinHttpSetTimeouts", e))?;
        // TLS 1.2 и 1.3: по умолчанию WinHTTP Wine предлагает только до 1.2, а
        // сервер стенда (rustls) - только 1.3. Unity и браузеры умеют оба.
        let protocols: u32 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | 0x2000;
        let _ = WinHttpSetOption(Some(session), WINHTTP_OPTION_SECURE_PROTOCOLS, Some(&protocols.to_ne_bytes()));
        let connection = WinHttpConnect(session, &HSTRING::from(host.as_str()), port, 0);
        if connection.is_null() {
            return Err(fail("WinHttpConnect", windows::core::Error::from_thread()));
        }
        let flags = if secure { WINHTTP_FLAG_SECURE } else { WINHTTP_OPEN_REQUEST_FLAGS(0) };
        let request = WinHttpOpenRequest(connection, w!("GET"), &HSTRING::from(path.as_str()), PCWSTR::null(), PCWSTR::null(), std::ptr::null(), flags);
        if request.is_null() {
            return Err(fail("WinHttpOpenRequest", windows::core::Error::from_thread()));
        }
        WinHttpSetStatusCallback(request, Some(status_callback), WINHTTP_CALLBACK_FLAG_SECURE_FAILURE, 0);
        if secure && host.parse::<std::net::IpAddr>().is_ok() {
            let ignore: u32 = SECURITY_FLAG_IGNORE_CERT_CN_INVALID;
            WinHttpSetOption(Some(request), WINHTTP_OPTION_SECURITY_FLAGS, Some(&ignore.to_ne_bytes())).map_err(|e| fail("WinHttpSetOption", e))?;
        }
        WinHttpSendRequest(request, None, None, 0, 0, 0).map_err(|e| fail("send", e))?;
        WinHttpReceiveResponse(request, std::ptr::null_mut()).map_err(|e| fail("response", e))?;
        let took = ms(started);
        let mut status: u32 = 0;
        let mut len: u32 = 4;
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut len,
            std::ptr::null_mut(),
        )
        .map_err(|e| fail("status", e))?;
        let mut body = 0usize;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let mut read: u32 = 0;
            WinHttpReadData(request, buf.as_mut_ptr() as *mut c_void, buf.len() as u32, &mut read).map_err(|e| fail("read", e))?;
            if read == 0 {
                break;
            }
            body += read as usize;
        }
        let _ = WinHttpCloseHandle(request);
        let _ = WinHttpCloseHandle(connection);
        let _ = WinHttpCloseHandle(session);
        Ok((status, body, took))
    }
}

/// `get URL` (`expect_ok`) или `get! URL`.
fn get(url: &str, expect_ok: bool) -> bool {
    let started = Instant::now();
    match fetch(url) {
        Ok((status, body, took)) => {
            eprintln!("netprobe: GET {url} -> HTTP {status}, {body} bytes, answer in {took} ms, all in {} ms", ms(started));
            if !expect_ok {
                eprintln!("netprobe: GET {url} should have been refused (untrusted root) but passed");
            }
            expect_ok && (200..500).contains(&status)
        }
        Err(err) => {
            eprintln!("netprobe: GET {url} failed - {err}{}", if expect_ok { "" } else { " (expected)" });
            !expect_ok
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut data = WSADATA::default();
    unsafe { WSAStartup(0x0202, &mut data) };
    eprintln!("netprobe: start: {}", args.join(" "));
    let (mut total, mut bad) = (0, 0);
    let mut at = 0;
    while at < args.len() {
        let step = args[at].as_str();
        let arg = args.get(at + 1).cloned().unwrap_or_default();
        let ok = match step {
            "self" => self_name(),
            "name" => {
                at += 1;
                name(&arg)
            }
            "tcp" => {
                at += 1;
                tcp(&arg)
            }
            "get" | "get!" => {
                at += 1;
                get(&arg, step == "get")
            }
            other => {
                eprintln!("netprobe: unknown step '{other}' (self, name, tcp, get, get!)");
                false
            }
        };
        total += 1;
        if !ok {
            bad += 1;
        }
        at += 1;
    }
    if bad == 0 {
        eprintln!("netprobe: done, all {total} as expected");
    } else {
        eprintln!("netprobe: done, {bad} of {total} NOT as expected");
    }
    std::process::exit(bad);
}
