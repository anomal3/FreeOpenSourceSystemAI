//! Google Диск (Drive API v3, вход — OAuth, область `drive.file`: программа
//! видит только созданные ею файлы).
//!
//! Путей у Диска нет — у файлов id и родители; путь `OBSOS/devices/pc1/x.log`
//! разрешается по именам, найденные id кешируются. Точка синхронизации —
//! папка `points/<имя>/devices/…` с копиями файлов (`files.copy`), подпись —
//! в описании папки точки.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::github::{arr, obj, s};
use super::{Account, Error, Http, RemoteFile, Request, Remote, SyncPoint, send, send_ok, status_error, url_encode};
use crate::json::Value;

const FOLDER: &str = "application/vnd.google-apps.folder";

pub struct GoogleDrive<'a> {
    http: &'a dyn Http,
    api: String,
    root: String,
    token: String,
    /// Путь (`OBSOS/devices`) → id.
    ids: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct Item {
    id: String,
    name: String,
    folder: bool,
    md5: String,
    description: String,
}

fn quote(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\'', "\\'")
}

impl<'a> GoogleDrive<'a> {
    pub fn new(account: &Account, http: &'a dyn Http) -> Self {
        GoogleDrive {
            http,
            api: account.host(),
            root: account.repo.trim().trim_matches('/').to_string(),
            token: account.token.clone(),
            ids: BTreeMap::new(),
        }
    }

    fn req(&self, method: &str, path: &str) -> Request {
        Request::new(method, format!("{}{}", self.api, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "OBSOS-browser")
    }

    fn query(&self, q: &str) -> Result<Vec<Item>, Error> {
        let path = format!(
            "/drive/v3/files?q={}&spaces=drive&pageSize=1000&fields={}",
            url_encode(q, b""),
            url_encode("files(id,name,mimeType,md5Checksum,description)", b"")
        );
        let v = send_ok(self.http, &self.req("GET", &path), "поиск файлов")?.json()?;
        Ok(v.get("files")
            .map(arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|f| {
                Some(Item {
                    id: f.str_field("id")?.to_string(),
                    name: f.str_field("name")?.to_string(),
                    folder: f.str_field("mimeType") == Some(FOLDER),
                    md5: f.str_field("md5Checksum").unwrap_or("").to_string(),
                    description: f.str_field("description").unwrap_or("").to_string(),
                })
            })
            .collect())
    }

    fn children(&self, parent: &str) -> Result<Vec<Item>, Error> {
        self.query(&format!("'{}' in parents and trashed=false", quote(parent)))
    }

    fn child(&self, parent: &str, name: &str) -> Result<Option<Item>, Error> {
        Ok(self.query(&format!("name='{}' and '{}' in parents and trashed=false", quote(name), quote(parent)))?.into_iter().next())
    }

    fn mkdir(&self, parent: &str, name: &str, description: &str) -> Result<String, Error> {
        let mut fields = alloc::vec![("name", s(name)), ("mimeType", s(FOLDER)), ("parents", Value::Arr(alloc::vec![s(parent)]))];
        if !description.is_empty() {
            fields.push(("description", s(description)));
        }
        let v = send_ok(self.http, &self.req("POST", "/drive/v3/files").json(&obj(fields)), "создание папки")?.json()?;
        v.str_field("id").map(str::to_string).ok_or_else(|| Error::Protocol("нет id папки".into()))
    }

    /// Id по пути от корня Диска; `create` — создать недостающие папки.
    fn resolve(&mut self, path: &str, create: bool) -> Result<Option<String>, Error> {
        if let Some(id) = self.ids.get(path) {
            return Ok(Some(id.clone()));
        }
        let mut parent = String::from("root");
        let mut walked = String::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(part);
            if let Some(id) = self.ids.get(&walked) {
                parent = id.clone();
                continue;
            }
            let id = match self.child(&parent, part)? {
                Some(item) => item.id,
                None if create => self.mkdir(&parent, part, "")?,
                None => return Ok(None),
            };
            self.ids.insert(walked.clone(), id.clone());
            parent = id;
        }
        Ok(Some(parent))
    }

    fn dir_of(&self, at: Option<&str>) -> String {
        match at {
            Some(point) => format!("{}/points/{point}/devices", self.root),
            None => format!("{}/devices", self.root),
        }
    }

    fn list_dir(&mut self, dir: &str) -> Result<Vec<Item>, Error> {
        match self.resolve(dir, false)? {
            Some(id) => self.children(&id),
            None => Ok(Vec::new()),
        }
    }

    fn upload(&mut self, path: &str, data: &str) -> Result<(), Error> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.resolve(&format!("{}/{dir}", self.root), true)?.unwrap_or_default();
        match self.child(&parent, name)? {
            Some(item) => {
                let request = self
                    .req("PATCH", &format!("/upload/drive/v3/files/{}?uploadType=media", item.id))
                    .header("Content-Type", "text/plain; charset=utf-8")
                    .body(data.as_bytes().to_vec());
                send_ok(self.http, &request, "обновление файла")?;
            }
            None => {
                let boundary = "obsos-sync-boundary-7f3a";
                let meta = obj(alloc::vec![("name", s(name)), ("parents", Value::Arr(alloc::vec![s(&parent)]))]).to_text();
                let body = format!(
                    "--{boundary}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{meta}\r\n--{boundary}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{data}\r\n--{boundary}--\r\n"
                );
                let request = self
                    .req("POST", "/upload/drive/v3/files?uploadType=multipart")
                    .header("Content-Type", format!("multipart/related; boundary={boundary}"))
                    .body(body.into_bytes());
                send_ok(self.http, &request, "загрузка файла")?;
            }
        }
        Ok(())
    }
}

impl Remote for GoogleDrive<'_> {
    fn prepare(&mut self) -> Result<(), Error> {
        let root = self.root.clone();
        self.resolve(&format!("{root}/devices"), true)?;
        self.resolve(&format!("{root}/points"), true)?;
        Ok(())
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        let dir = self.dir_of(at);
        Ok(self.list_dir(&dir)?.into_iter().filter(|i| i.folder).map(|i| i.name).collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        let dir = format!("{}/{device}", self.dir_of(at));
        Ok(self
            .list_dir(&dir)?
            .into_iter()
            .filter(|i| !i.folder)
            .map(|i| RemoteFile { name: i.name, version: i.md5 })
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        let rest = path.strip_prefix("devices/").unwrap_or(path);
        let full = format!("{}/{rest}", self.dir_of(at));
        let (dir, name) = full.rsplit_once('/').unwrap_or(("", &full));
        let Some(parent) = self.resolve(dir, false)? else { return Ok(None) };
        let Some(item) = self.child(&parent, name)? else { return Ok(None) };
        let response = send(self.http, &self.req("GET", &format!("/drive/v3/files/{}?alt=media", item.id)))?;
        match response.status {
            404 => Ok(None),
            200 => Ok(Some(response.text())),
            _ => Err(status_error("чтение файла", &response)),
        }
    }

    fn write(&mut self, files: &[(String, String)], _message: &str) -> Result<(), Error> {
        for (path, data) in files {
            self.upload(path, data)?;
        }
        Ok(())
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let root = self.root.clone();
        let points = self.resolve(&format!("{root}/points"), true)?.unwrap_or_default();
        let point = self.mkdir(&points, name, label)?;
        let devices_dir = self.mkdir(&point, "devices", "")?;
        for device in self.devices(None)? {
            let target = self.mkdir(&devices_dir, &device, "")?;
            let source = format!("{root}/devices/{device}");
            for item in self.list_dir(&source)?.into_iter().filter(|i| !i.folder) {
                let body = obj(alloc::vec![("name", s(&item.name)), ("parents", Value::Arr(alloc::vec![s(&target)]))]);
                send_ok(self.http, &self.req("POST", &format!("/drive/v3/files/{}/copy", item.id)).json(&body), "копия файла")?;
            }
        }
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let dir = format!("{}/points", self.root);
        let mut points: Vec<SyncPoint> = self
            .list_dir(&dir)?
            .into_iter()
            .filter(|i| i.folder && i.name.starts_with("obsos-"))
            .map(|i| SyncPoint { name: i.name, label: i.description })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(points)
    }
}
