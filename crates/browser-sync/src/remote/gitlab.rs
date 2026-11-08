//! GitLab (gitlab.com или свой сервер — поле `host`): проект `группа/имя`.
//!
//! Запись — Commits API: все файлы одним коммитом (`create` или `update` по
//! списку файлов каталога). Точка синхронизации — тег с сообщением-подписью.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::github::{arr, obj, s};
use super::{Account, Error, Http, RemoteFile, Request, Remote, SyncPoint, send, send_ok, status_error, url_encode};
use crate::json::Value;

pub struct GitLab<'a> {
    http: &'a dyn Http,
    api: String,
    repo: String,
    branch: String,
    token: String,
}

impl<'a> GitLab<'a> {
    pub fn new(account: &Account, http: &'a dyn Http) -> Self {
        GitLab {
            http,
            api: format!("{}/api/v4", account.host()),
            repo: account.repo.trim().trim_matches('/').to_string(),
            branch: account.branch().to_string(),
            token: account.token.clone(),
        }
    }

    fn req(&self, method: &str, path: &str) -> Request {
        Request::new(method, format!("{}/projects/{}{}", self.api, url_encode(&self.repo, b""), path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "OBSOS-browser")
    }

    fn tree(&self, dir: &str, at: Option<&str>) -> Result<Vec<Value>, Error> {
        let path = format!(
            "/repository/tree?path={}&ref={}&per_page=100",
            url_encode(dir, b""),
            url_encode(at.unwrap_or(&self.branch), b"")
        );
        let response = send(self.http, &self.req("GET", &path))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("список файлов", &response));
        }
        Ok(arr(&response.json()?).to_vec())
    }
}

impl Remote for GitLab<'_> {
    fn prepare(&mut self) -> Result<(), Error> {
        let response = send(self.http, &self.req("GET", ""))?;
        if response.ok() {
            return Ok(());
        }
        if response.status != 404 {
            return Err(status_error("проект", &response));
        }
        let name = self.repo.rsplit('/').next().unwrap_or(&self.repo).to_string();
        let body = obj(alloc::vec![
            ("name", s(&name)),
            ("path", s(&name)),
            ("visibility", s("private")),
            ("initialize_with_readme", Value::Bool(true)),
            ("default_branch", s(&self.branch)),
        ]);
        let request = Request::new("POST", format!("{}/projects", self.api))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("User-Agent", "OBSOS-browser")
            .json(&body);
        send_ok(self.http, &request, "создание проекта")?;
        Ok(())
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        Ok(self
            .tree("devices", at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("tree"))
            .filter_map(|e| e.str_field("name").map(str::to_string))
            .collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        Ok(self
            .tree(&format!("devices/{device}"), at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("blob"))
            .filter_map(|e| Some(RemoteFile { name: e.str_field("name")?.to_string(), version: e.str_field("id").unwrap_or("").to_string() }))
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        let url = format!(
            "/repository/files/{}/raw?ref={}",
            url_encode(path, b""),
            url_encode(at.unwrap_or(&self.branch), b"")
        );
        let response = send(self.http, &self.req("GET", &url))?;
        match response.status {
            404 => Ok(None),
            200 => Ok(Some(response.text())),
            _ => Err(status_error("чтение файла", &response)),
        }
    }

    fn write(&mut self, files: &[(String, String)], message: &str) -> Result<(), Error> {
        if files.is_empty() {
            return Ok(());
        }
        // Какие файлы уже есть — по каталогам, которые трогаем.
        let mut existing: Vec<String> = Vec::new();
        let mut dirs: Vec<&str> = files.iter().filter_map(|(p, _)| p.rsplit_once('/').map(|(d, _)| d)).collect();
        dirs.sort();
        dirs.dedup();
        for dir in dirs {
            for e in self.tree(dir, None)? {
                if let Some(name) = e.str_field("name") {
                    existing.push(format!("{dir}/{name}"));
                }
            }
        }
        let actions = files
            .iter()
            .map(|(path, data)| {
                let action = if existing.contains(path) { "update" } else { "create" };
                obj(alloc::vec![("action", s(action)), ("file_path", s(path)), ("content", s(data))])
            })
            .collect();
        let body = obj(alloc::vec![("branch", s(&self.branch)), ("commit_message", s(message)), ("actions", Value::Arr(actions))]);
        send_ok(self.http, &self.req("POST", "/repository/commits").json(&body), "коммит")?;
        Ok(())
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let body = obj(alloc::vec![("tag_name", s(name)), ("ref", s(&self.branch)), ("message", s(label))]);
        send_ok(self.http, &self.req("POST", "/repository/tags").json(&body), "тег")?;
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let response = send(self.http, &self.req("GET", "/repository/tags?per_page=100"))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("теги", &response));
        }
        let mut points: Vec<SyncPoint> = arr(&response.json()?)
            .iter()
            .filter_map(|t| {
                let name = t.str_field("name")?;
                name.starts_with("obsos-")
                    .then(|| SyncPoint { name: name.to_string(), label: t.str_field("message").unwrap_or("").to_string() })
            })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(points)
    }
}
