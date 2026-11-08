//! Bitbucket Cloud: репозиторий `рабочее-пространство/имя`.
//!
//! Вход — пароль приложения или API-токен с именем пользователя (Basic) либо
//! токен доступа без имени (Bearer): у Bitbucket нет Device Flow. Запись —
//! `POST /src` формой «путь = содержимое»: все файлы одним коммитом. Точка
//! синхронизации — тег на конце ветки.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::github::{arr, obj, s};
use super::{Account, Error, Http, RemoteFile, Request, Remote, SyncPoint, b64_encode, send, send_ok, status_error, url_encode};
use crate::json::Value;

pub struct Bitbucket<'a> {
    http: &'a dyn Http,
    api: String,
    repo: String,
    branch: String,
    auth: String,
}

impl<'a> Bitbucket<'a> {
    pub fn new(account: &Account, http: &'a dyn Http) -> Self {
        let auth = if account.user.is_empty() {
            format!("Bearer {}", account.token)
        } else {
            format!("Basic {}", b64_encode(format!("{}:{}", account.user, account.token).as_bytes()))
        };
        Bitbucket {
            http,
            api: account.host(),
            repo: account.repo.trim().trim_matches('/').to_string(),
            branch: account.branch().to_string(),
            auth,
        }
    }

    fn req(&self, method: &str, path: &str) -> Request {
        Request::new(method, format!("{}/repositories/{}{}", self.api, self.repo, path))
            .header("Authorization", self.auth.clone())
            .header("User-Agent", "OBSOS-browser")
    }

    fn src(&self, path: &str, at: Option<&str>) -> String {
        format!("/src/{}/{}", url_encode(at.unwrap_or(&self.branch), b""), url_encode(path, b"/"))
    }

    fn listing(&self, dir: &str, at: Option<&str>) -> Result<Vec<Value>, Error> {
        let path = format!("{}/?pagelen=100", self.src(dir, at));
        let response = send(self.http, &self.req("GET", &path))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("список файлов", &response));
        }
        let v = response.json()?;
        Ok(v.get("values").map(|a| arr(a).to_vec()).unwrap_or_default())
    }
}

fn base_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or(path)
}

impl Remote for Bitbucket<'_> {
    fn prepare(&mut self) -> Result<(), Error> {
        let response = send(self.http, &self.req("GET", ""))?;
        if response.ok() {
            return Ok(());
        }
        if response.status != 404 {
            return Err(status_error("репозиторий", &response));
        }
        let body = obj(alloc::vec![("scm", s("git")), ("is_private", Value::Bool(true))]);
        send_ok(self.http, &self.req("POST", "").json(&body), "создание репозитория")?;
        Ok(())
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        Ok(self
            .listing("devices", at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("commit_directory"))
            .filter_map(|e| e.str_field("path").map(|p| base_name(p).to_string()))
            .collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        Ok(self
            .listing(&format!("devices/{device}"), at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("commit_file"))
            .filter_map(|e| {
                let name = base_name(e.str_field("path")?).to_string();
                // Хеша файла в списке нет; размер ловит дозапись в журнал.
                let version = match e.get("size") {
                    Some(Value::Num(n)) => format!("size:{n}"),
                    _ => String::new(),
                };
                Some(RemoteFile { name, version })
            })
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        let response = send(self.http, &self.req("GET", &self.src(path, at)))?;
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
        let mut fields: Vec<(&str, &str)> = alloc::vec![("message", message), ("branch", self.branch.as_str())];
        for (path, data) in files {
            fields.push((path.as_str(), data.as_str()));
        }
        send_ok(self.http, &self.req("POST", "/src").form(&fields), "коммит")?;
        Ok(())
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let path = format!("/refs/branches/{}", url_encode(&self.branch, b""));
        let branch = send_ok(self.http, &self.req("GET", &path), "конец ветки")?.json()?;
        let hash = branch
            .get("target")
            .and_then(|t| t.str_field("hash"))
            .ok_or_else(|| Error::Protocol("нет target.hash".into()))?
            .to_string();
        let body = obj(alloc::vec![("name", s(name)), ("message", s(label)), ("target", obj(alloc::vec![("hash", s(&hash))]))]);
        send_ok(self.http, &self.req("POST", "/refs/tags").json(&body), "тег")?;
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let response = send(self.http, &self.req("GET", "/refs/tags?pagelen=100&q=name~%22obsos-%22"))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("теги", &response));
        }
        let v = response.json()?;
        let mut points: Vec<SyncPoint> = v
            .get("values")
            .map(arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|t| {
                let name = t.str_field("name")?;
                name.starts_with("obsos-")
                    .then(|| SyncPoint { name: name.to_string(), label: t.str_field("message").unwrap_or("").trim().to_string() })
            })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(points)
    }
}
