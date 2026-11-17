//! `craftupd` — обновление приложений Craft на FossAi без форка и без ручной
//! пересборки образа (ROADMAP: «Приложения Craft», обновления).
//!
//! Ночная сборка (`.github/workflows/craft.yml`, `cargo xtask craft --latest`)
//! выкладывает в выпуск `craft-packages` репозитория FossAi пакеты
//! `<арх>-<app>-<версия>.fpk` и индекс `<арх>-craft-index.json` с подписью
//! `.sig` (Ed25519 ключом выпуска по `osupdate::index::digest`). Здесь:
//!
//! 1. индекс и подпись — по HTTPS (или из папки: `--from <каталог>`);
//! 2. подпись — ключами `/os-keys` (теми же, что у обновлений системы); не
//!    сошлась — ничего не ставится;
//! 3. установленная версия — из реестра `pkg` (`/var/lib/pkg/<имя>.pkg`);
//! 4. новее — скачать, сверить размер и SHA-256 с индексом, проверить, что
//!    пакет подписан (подпись пакета сверит `pkg install` по `/os-keys`),
//!    `pkg remove` + `pkg install`.
//!
//! `craftupd` — проверить и обновить установленные; `craftupd --check` —
//! только сказать; `craftupd gridcraft …` — поставить/обновить названные;
//! `craftupd --all` — все из индекса.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

const RELEASE: &str = "https://github.com/anomal3/FreeOpenSourceSystemAI/releases/download/craft-packages";
const REGISTRY: &str = "/var/lib/pkg";

#[derive(Debug, Clone)]
struct Entry {
    name: String,
    version: String,
    file: String,
    size: u64,
    sha256: String,
    url: String,
}

fn arch() -> &'static str {
    if cfg!(target_arch = "aarch64") { "aarch64" } else { "x86_64" }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut check = false;
    let mut all = false;
    let mut from: Option<String> = None;
    let mut base = String::from(RELEASE);
    let mut wanted: Vec<String> = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--check" | "-n" => check = true,
            "--all" => all = true,
            "--from" => from = it.next(),
            "--url" => base = it.next().unwrap_or(base),
            "-h" | "--help" => {
                println!("usage: craftupd [--check] [--all] [--from DIR | --url BASE] [app...]");
                return;
            }
            name => wanted.push(name.to_string()),
        }
    }
    std::process::exit(match run(check, all, from.as_deref(), &base, &wanted) {
        Ok(()) => 0,
        Err(e) => {
            println!("craftupd: {e}");
            1
        }
    });
}

fn run(check: bool, all: bool, from: Option<&str>, base: &str, wanted: &[String]) -> Result<(), String> {
    let index_name = format!("{}-craft-index.json", arch());
    let (index, sig) = match from {
        Some(dir) => {
            // Из папки — так, как кладёт `cargo xtask craft` (без приставки арх.).
            let read = |n: &str| std::fs::read(format!("{dir}/{n}")).map_err(|e| format!("{dir}/{n}: {e}"));
            (read("craft-index.json").or_else(|_| read(&index_name))?, read("craft-index.json.sig").or_else(|_| read(&format!("{index_name}.sig")))?)
        }
        None => (get(&format!("{base}/{index_name}"))?, get(&format!("{base}/{index_name}.sig"))?),
    };
    verify_index(&index, &sig)?;
    let text = String::from_utf8(index).map_err(|_| "index is not UTF-8".to_string())?;
    let entries = parse_index(&text);
    println!("craftupd: index ok, {} package(s), signature verified", entries.len());
    let mut failures = 0;
    for e in &entries {
        let installed = installed_version(&e.name);
        let asked = wanted.iter().any(|w| *w == e.name);
        let newer = installed.as_deref().is_none_or(|v| version_key(&e.version) > version_key(v));
        match (&installed, newer) {
            (Some(v), false) => println!("craftupd: {} {v} is up to date", e.name),
            (Some(v), true) => println!("craftupd: {} {v} -> {} available", e.name, e.version),
            (None, _) => println!("craftupd: {} {} available (not installed)", e.name, e.version),
        }
        let act = newer && (asked || all || installed.is_some());
        if check || !act {
            continue;
        }
        if let Err(err) = update(e, from, installed.is_some()) {
            println!("craftupd: {} failed: {err}", e.name);
            failures += 1;
        }
    }
    if failures > 0 { Err(format!("{failures} package(s) failed")) } else { Ok(()) }
}

fn update(e: &Entry, from: Option<&str>, installed: bool) -> Result<(), String> {
    let bytes = match from {
        Some(dir) => std::fs::read(format!("{dir}/{}", e.file)).map_err(|err| format!("{dir}/{}: {err}", e.file))?,
        None => get(&e.url)?,
    };
    if bytes.len() as u64 != e.size {
        return Err(format!("size {} != {} from the index", bytes.len(), e.size));
    }
    let mut hasher = fpk::Hasher::new();
    hasher.update(&bytes);
    if hex(&hasher.finish()) != e.sha256 {
        return Err("SHA-256 does not match the index".into());
    }
    let header = fpk::Header::parse(&bytes).map_err(|err| format!("not a package: {err:?}"))?;
    if !header.is_signed() {
        return Err("the package is not signed".into());
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home".into());
    let dir = format!("{home}/Downloads");
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/{}", e.file);
    std::fs::write(&path, &bytes).map_err(|err| format!("{path}: {err}"))?;
    println!("craftupd: {} {} downloaded to {path}", e.name, e.version);
    if installed {
        shell(&format!("/bin/pkg remove {}", e.name))?;
    }
    // Подпись пакета `pkg install` сверяет с /os-keys сам и отказывает чужой.
    shell(&format!("/bin/pkg install {path}"))?;
    println!("craftupd: {} {} installed", e.name, e.version);
    Ok(())
}

unsafe extern "C" {
    fn system(command: *const std::ffi::c_char) -> std::ffi::c_int;
}

/// Команда системы (`std::process` у родных программ FreeOS пока нет).
fn shell(command: &str) -> Result<(), String> {
    let c = std::ffi::CString::new(command).map_err(|_| "bad command".to_string())?;
    // SAFETY: строка C живёт до возврата.
    let code = unsafe { system(c.as_ptr()) };
    if code == 0 { Ok(()) } else { Err(format!("`{command}` exited with {code}")) }
}

fn verify_index(index: &[u8], sig: &[u8]) -> Result<(), String> {
    let sig = String::from_utf8_lossy(sig);
    let sig = unhex(sig.trim()).ok_or("bad signature file")?;
    let keys = std::fs::read_to_string("/os-keys").map_err(|e| format!("/os-keys: {e}"))?;
    let trusted = osupdate::keys::Trusted::parse(&keys);
    if trusted.verifies(&osupdate::index::digest(index), &sig) {
        Ok(())
    } else {
        Err("the index is not signed by a key this system trusts (/os-keys); nothing installed".into())
    }
}

fn installed_version(name: &str) -> Option<String> {
    let text = std::fs::read_to_string(format!("{REGISTRY}/{name}.pkg")).ok()?;
    text.lines().find_map(|l| l.strip_prefix("version=").map(|v| v.trim().to_string()))
}

fn version_key(v: &str) -> Vec<u64> {
    v.split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0)).collect()
}

/// Индекс пишет `cargo xtask craft` — по объекту на строку; разбор по ключам.
fn parse_index(text: &str) -> Vec<Entry> {
    let field = |line: &str, key: &str| -> Option<String> {
        let at = line.find(&format!("\"{key}\":"))? + key.len() + 3;
        let rest = line[at..].trim_start();
        if let Some(s) = rest.strip_prefix('"') {
            Some(s[..s.find('"')?].to_string())
        } else {
            Some(rest.chars().take_while(|c| c.is_ascii_digit()).collect())
        }
    };
    text.lines()
        .filter(|l| l.contains("\"name\":"))
        .filter_map(|l| {
            Some(Entry {
                name: field(l, "name")?,
                version: field(l, "version")?,
                file: field(l, "file")?,
                size: field(l, "size")?.parse().ok()?,
                sha256: field(l, "sha256")?,
                url: field(l, "url")?,
            })
        })
        .filter(|e| e.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

// ── HTTPS GET (HTTP/1.1, переадресации, chunked) ───────────────────────────

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("TLS versions")
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

fn get(url: &str) -> Result<Vec<u8>, String> {
    let config = tls_config();
    let mut url = url.to_string();
    for _ in 0..8 {
        let (status, headers, body) = request(&config, &url)?;
        if (300..400).contains(&status) {
            let location = header(&headers, "location").ok_or("redirect without Location")?;
            url = if location.starts_with("http") { location } else { format!("https://{}{location}", host_of(&url)?.0) };
            continue;
        }
        if status != 200 {
            return Err(format!("{url}: HTTP {status}"));
        }
        return Ok(body);
    }
    Err("too many redirects".into())
}

fn host_of(url: &str) -> Result<(String, String), String> {
    let rest = url.strip_prefix("https://").ok_or_else(|| format!("{url}: only https"))?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    Ok((host.to_string(), path))
}

fn header(headers: &str, name: &str) -> Option<String> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
    })
}

fn request(config: &Arc<rustls::ClientConfig>, url: &str) -> Result<(u16, String, Vec<u8>), String> {
    let (host, path) = host_of(url)?;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| format!("{host}: {e}"))?;
    let mut conn = rustls::ClientConnection::new(config.clone(), name).map_err(|e| e.to_string())?;
    let mut sock = TcpStream::connect((host.as_str(), 443)).map_err(|e| format!("{host}: {e}"))?;
    let _ = sock.set_read_timeout(Some(Duration::from_secs(60)));
    let mut tls = rustls::Stream::new(&mut conn, &mut sock);
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: fossai-craftupd/0.1\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    tls.write_all(req.as_bytes()).map_err(|e| format!("{host}: {e}"))?;
    let mut raw = Vec::new();
    match tls.read_to_end(&mut raw) {
        Ok(_) => {}
        // Сервер закрыл без close_notify — тело уже у нас (Connection: close).
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => return Err(format!("{host}: {e}")),
    }
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no HTTP header")?;
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = raw[split + 4..].to_vec();
    let status: u16 = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or("bad status line")?;
    if header(&head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        body = dechunk(&body)?;
    }
    Ok((status, head, body))
}

fn dechunk(mut data: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let line_end = data.windows(2).position(|w| w == b"\r\n").ok_or("bad chunk")?;
        let size_text = String::from_utf8_lossy(&data[..line_end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16).map_err(|_| "bad chunk size")?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if data.len() < size {
            return Err("short chunk".into());
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn index_lines_parse() {
        let text = r#"{ "format": 1, "arch": "x86_64", "packages": [
    { "name": "gridcraft", "version": "0.19.1", "file": "gridcraft-0.19.1.fpk", "size": 123, "sha256": "ab", "url": "https://x/y" }
  ] }"#;
        let e = super::parse_index(text);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].size, 123);
        assert_eq!(e[0].url, "https://x/y");
        assert!(super::version_key("0.20.0") > super::version_key("0.19.10"));
        assert_eq!(super::dechunk(b"3\r\nabc\r\n0\r\n\r\n").unwrap(), b"abc");
    }
}
