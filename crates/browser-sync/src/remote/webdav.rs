//! WebDAV: Яндекс Диск (`https://webdav.yandex.ru`, логин и пароль
//! приложения — регистрировать своё OAuth-приложение не нужно), Nextcloud,
//! ownCloud, любой сервер WebDAV.
//!
//! Папка `<repo>/devices/<id>/…`; точка синхронизации — копия `devices/` в
//! `<repo>/points/<имя>/devices/` (метод `COPY`, сервер копирует сам) и
//! подпись в `points/<имя>/label.txt`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::{Account, Error, Http, RemoteFile, Request, Remote, SyncPoint, b64_encode, send, status_error, url_decode, url_encode};

pub struct WebDav<'a> {
    http: &'a dyn Http,
    /// `https://webdav.yandex.ru` — без завершающей `/`.
    base: String,
    /// Путь папки на сервере: `/remote.php/dav/files/roman/OBSOS`.
    root: String,
    auth: String,
}

/// Запись ответа PROPFIND.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DavEntry {
    pub href: String,
    pub dir: bool,
    pub version: String,
}

/// Разбор ответа `PROPFIND` (multistatus). Префиксы пространств имён у
/// серверов разные (`d:`, `D:`, без префикса), поэтому сравниваем по
/// локальному имени тега.
pub(crate) fn parse_multistatus(xml: &str) -> Vec<DavEntry> {
    let mut out = Vec::new();
    for block in split_elements(xml, "response") {
        let Some(href) = element_text(block, "href") else { continue };
        let dir = find_tag(block, "collection").is_some();
        let version = element_text(block, "getetag")
            .or_else(|| element_text(block, "getlastmodified"))
            .unwrap_or_default()
            .trim_matches('"')
            .to_string();
        out.push(DavEntry { href: url_decode(href.trim()), dir, version });
    }
    out
}

/// Начало открывающего тега с локальным именем `name` (`<d:name`, `<name`).
fn find_tag(text: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(pos) = text[from..].find('<') {
        let start = from + pos;
        let rest = &text[start + 1..];
        let tag_end = rest.find(|c: char| c == '>' || c == '/' || c.is_whitespace()).unwrap_or(rest.len());
        let tag = &rest[..tag_end];
        if !tag.starts_with('/') && (tag == name || tag.rsplit(':').next() == Some(name)) {
            return Some(start);
        }
        from = start + 1;
    }
    None
}

/// Текст первого элемента `name`.
fn element_text<'t>(text: &'t str, name: &str) -> Option<&'t str> {
    let start = find_tag(text, name)?;
    let open_end = start + text[start..].find('>')? + 1;
    if text[..open_end].ends_with("/>") {
        return Some("");
    }
    let close = text[open_end..].find("</")?;
    Some(&text[open_end..open_end + close])
}

/// Куски текста между открывающими тегами `name` (каждый — один элемент).
fn split_elements<'t>(text: &'t str, name: &str) -> Vec<&'t str> {
    let mut starts = Vec::new();
    let mut from = 0;
    while let Some(pos) = find_tag(&text[from..], name) {
        starts.push(from + pos);
        from += pos + 1;
    }
    starts.iter().enumerate().map(|(i, &s)| &text[s..*starts.get(i + 1).unwrap_or(&text.len())]).collect()
}

impl<'a> WebDav<'a> {
    pub fn new(account: &Account, http: &'a dyn Http) -> Self {
        let host = account.host();
        // Адрес может нести путь (Nextcloud: …/remote.php/dav/files/<user>).
        let (base, prefix) = match host.find("://").and_then(|i| host[i + 3..].find('/').map(|j| i + 3 + j)) {
            Some(cut) => (host[..cut].to_string(), host[cut..].trim_end_matches('/').to_string()),
            None => (host.clone(), String::new()),
        };
        let folder = account.repo.trim().trim_matches('/');
        WebDav {
            http,
            base,
            root: format!("{prefix}/{folder}"),
            auth: format!("Basic {}", b64_encode(format!("{}:{}", account.user, account.token).as_bytes())),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, url_encode(&format!("{}/{}", self.root, path.trim_start_matches('/')), b"/"))
    }

    fn req(&self, method: &str, path: &str) -> Request {
        Request::new(method, self.url(path)).header("Authorization", self.auth.clone()).header("User-Agent", "OBSOS-browser")
    }

    fn mkcol(&self, path: &str) -> Result<(), Error> {
        let response = send(self.http, &self.req("MKCOL", &format!("{}/", path.trim_end_matches('/'))))?;
        // 405 — уже есть.
        if response.ok() || response.status == 405 {
            Ok(())
        } else {
            Err(status_error("создание папки", &response))
        }
    }

    /// Подпапки и файлы каталога (без него самого).
    fn list(&self, dir: &str) -> Result<Vec<DavEntry>, Error> {
        let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?><d:propfind xmlns:d=\"DAV:\"><d:prop><d:resourcetype/><d:getetag/><d:getlastmodified/></d:prop></d:propfind>";
        let request = self
            .req("PROPFIND", &format!("{}/", dir.trim_end_matches('/')))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body.as_bytes().to_vec());
        let response = send(self.http, &request)?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("список файлов", &response));
        }
        let own = format!("{}/{}", self.root, dir.trim_matches('/'));
        Ok(parse_multistatus(&response.text())
            .into_iter()
            .filter(|e| {
                let path = e.href.split("://").nth(1).map(|r| r.find('/').map(|i| &r[i..]).unwrap_or("")).unwrap_or(&e.href);
                path.trim_end_matches('/') != own.trim_end_matches('/')
            })
            .collect())
    }

    fn prefix(at: Option<&str>) -> String {
        match at {
            Some(point) => format!("points/{point}/devices"),
            None => String::from("devices"),
        }
    }
}

fn entry_name(href: &str) -> String {
    href.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()
}

impl Remote for WebDav<'_> {
    fn prepare(&mut self) -> Result<(), Error> {
        // Папка может лежать глубже: создаём путь по частям. Части префикса
        // сервера (Nextcloud: /remote.php/dav/files/<user>) уже есть — на них
        // сервер ответит 405, это не ошибка.
        let mut path = String::new();
        for part in self.root.split('/').filter(|p| !p.is_empty()) {
            path.push('/');
            path.push_str(part);
            let url = format!("{}{}/", self.base, url_encode(&path, b"/"));
            let request = Request::new("MKCOL", url).header("Authorization", self.auth.clone()).header("User-Agent", "OBSOS-browser");
            let response = send(self.http, &request)?;
            // Промежуточные части чужого префикса могут отвечать чем угодно
            // (403 на /remote.php) — важна только сама папка.
            if path == self.root && !(response.ok() || response.status == 405) {
                return Err(status_error("создание папки", &response));
            }
        }
        self.mkcol("devices")?;
        self.mkcol("points")
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        Ok(self.list(&Self::prefix(at))?.into_iter().filter(|e| e.dir).map(|e| entry_name(&e.href)).collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        Ok(self
            .list(&format!("{}/{device}", Self::prefix(at)))?
            .into_iter()
            .filter(|e| !e.dir)
            .map(|e| RemoteFile { name: entry_name(&e.href), version: e.version })
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        let path = match (at, path.strip_prefix("devices/")) {
            (Some(point), Some(rest)) => format!("points/{point}/devices/{rest}"),
            _ => path.to_string(),
        };
        let response = send(self.http, &self.req("GET", &path))?;
        match response.status {
            404 => Ok(None),
            200..=299 => Ok(Some(response.text())),
            _ => Err(status_error("чтение файла", &response)),
        }
    }

    fn write(&mut self, files: &[(String, String)], _message: &str) -> Result<(), Error> {
        let mut made: Vec<String> = Vec::new();
        for (path, data) in files {
            if let Some((dir, _)) = path.rsplit_once('/') {
                if !made.iter().any(|d| d == dir) {
                    self.mkcol(dir)?;
                    made.push(dir.to_string());
                }
            }
            let response = send(self.http, &self.req("PUT", path).body(data.as_bytes().to_vec()))?;
            if !response.ok() {
                return Err(status_error("запись файла", &response));
            }
        }
        Ok(())
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let dir = format!("points/{name}");
        self.mkcol(&dir)?;
        let request = self
            .req("COPY", "devices/")
            .header("Destination", self.url(&format!("{dir}/devices/")))
            .header("Depth", "infinity")
            .header("Overwrite", "F");
        let response = send(self.http, &request)?;
        if !response.ok() {
            return Err(status_error("копия папки", &response));
        }
        if !label.is_empty() {
            let response = send(self.http, &self.req("PUT", &format!("{dir}/label.txt")).body(label.as_bytes().to_vec()))?;
            if !response.ok() {
                return Err(status_error("подпись точки", &response));
            }
        }
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let mut points: Vec<SyncPoint> = self
            .list("points")?
            .into_iter()
            .filter(|e| e.dir)
            .map(|e| entry_name(&e.href))
            .filter(|n| n.starts_with("obsos-"))
            .map(|name| SyncPoint { name, label: String::new() })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        // Подписи — у последних тридцати (каждая — отдельный запрос).
        for point in points.iter_mut().take(30) {
            let response = send(self.http, &self.req("GET", &format!("points/{}/label.txt", point.name)))?;
            if response.ok() {
                point.label = response.text().trim().to_string();
            }
        }
        Ok(points)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multistatus_any_prefix() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:">
<d:response><d:href>/OBSOS/devices/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop></d:propstat></d:response>
<d:response><d:href>/OBSOS/devices/pc%201/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype><d:getetag>"abc"</d:getetag></d:prop></d:propstat></d:response>
<d:response><d:href>/OBSOS/devices/x.log</d:href><d:propstat><d:prop><d:resourcetype/><d:getetag>"e1"</d:getetag></d:prop></d:propstat></d:response>
</d:multistatus>"#;
        let entries = parse_multistatus(xml);
        assert_eq!(entries.len(), 3);
        assert!(entries[1].dir && entries[1].href == "/OBSOS/devices/pc 1/" && entries[1].version == "abc");
        assert!(!entries[2].dir && entries[2].version == "e1");
        let upper = xml.replace("d:", "D:");
        assert_eq!(parse_multistatus(&upper), entries);
        let bare = xml.replace("d:", "");
        assert_eq!(parse_multistatus(&bare), entries);
    }
}
