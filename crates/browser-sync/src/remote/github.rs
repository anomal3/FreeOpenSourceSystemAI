//! GitHub: закрытый репозиторий `владелец/имя`, ветка `main`.
//!
//! Запись — Git Data API: все изменённые файлы одним коммитом (дерево поверх
//! прошлого, коммит, сдвиг ветки). Если ветку сдвинуло другое устройство
//! между чтением и записью, GitHub отвечает 422 — повторяем с нового конца.
//! Точка синхронизации — аннотированный тег `obsos-…` (сообщение — подпись).

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::{Account, Error, Http, RemoteFile, Request, Remote, SyncPoint, b64_encode, send, send_ok, status_error, url_encode};
use crate::json::Value;

pub struct GitHub<'a> {
    http: &'a dyn Http,
    api: String,
    repo: String,
    branch: String,
    token: String,
}

pub(crate) fn obj(fields: Vec<(&str, Value)>) -> Value {
    Value::Obj(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

pub(crate) fn s(text: &str) -> Value {
    Value::Str(text.to_string())
}

pub(crate) fn arr(v: &Value) -> &[Value] {
    match v {
        Value::Arr(items) => items,
        _ => &[],
    }
}

impl<'a> GitHub<'a> {
    pub fn new(account: &Account, http: &'a dyn Http) -> Self {
        GitHub {
            http,
            api: account.host(),
            repo: account.repo.trim().trim_matches('/').to_string(),
            branch: account.branch().to_string(),
            token: account.token.clone(),
        }
    }

    fn req(&self, method: &str, path: &str) -> Request {
        Request::new(method, format!("{}{}", self.api, path))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "OBSOS-browser")
    }

    fn repo_path(&self, rest: &str) -> String {
        format!("/repos/{}{}", self.repo, rest)
    }

    fn reference(&self, at: Option<&str>) -> String {
        url_encode(at.unwrap_or(&self.branch), b"")
    }

    /// Содержимое каталога; нет каталога (или пустой репозиторий) — пусто.
    fn list(&self, dir: &str, at: Option<&str>) -> Result<Vec<Value>, Error> {
        let path = self.repo_path(&format!("/contents/{}?ref={}", url_encode(dir, b"/"), self.reference(at)));
        let response = send(self.http, &self.req("GET", &path))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("список файлов", &response));
        }
        Ok(arr(&response.json()?).to_vec())
    }

    fn head(&self) -> Result<String, Error> {
        let path = self.repo_path(&format!("/git/ref/heads/{}", url_encode(&self.branch, b"")));
        let v = send_ok(self.http, &self.req("GET", &path), "конец ветки")?.json()?;
        v.get("object").and_then(|o| o.str_field("sha")).map(str::to_string).ok_or_else(|| Error::Protocol("нет object.sha".into()))
    }

    fn commit_once(&self, files: &[(String, String)], message: &str) -> Result<bool, Error> {
        let parent = self.head()?;
        let commit = send_ok(self.http, &self.req("GET", &self.repo_path(&format!("/git/commits/{parent}"))), "коммит")?.json()?;
        let base_tree = commit
            .get("tree")
            .and_then(|t| t.str_field("sha"))
            .ok_or_else(|| Error::Protocol("нет tree.sha".into()))?
            .to_string();
        let entries = files
            .iter()
            .map(|(path, data)| {
                obj(alloc::vec![("path", s(path)), ("mode", s("100644")), ("type", s("blob")), ("content", s(data))])
            })
            .collect();
        let tree_body = obj(alloc::vec![("base_tree", s(&base_tree)), ("tree", Value::Arr(entries))]);
        let tree = send_ok(self.http, &self.req("POST", &self.repo_path("/git/trees")).json(&tree_body), "дерево")?.json()?;
        let tree_sha = tree.str_field("sha").ok_or_else(|| Error::Protocol("нет sha дерева".into()))?;
        let commit_body = obj(alloc::vec![
            ("message", s(message)),
            ("tree", s(tree_sha)),
            ("parents", Value::Arr(alloc::vec![s(&parent)])),
        ]);
        let new = send_ok(self.http, &self.req("POST", &self.repo_path("/git/commits")).json(&commit_body), "коммит")?.json()?;
        let new_sha = new.str_field("sha").ok_or_else(|| Error::Protocol("нет sha коммита".into()))?;
        let move_body = obj(alloc::vec![("sha", s(new_sha)), ("force", Value::Bool(false))]);
        let path = self.repo_path(&format!("/git/refs/heads/{}", url_encode(&self.branch, b"")));
        let response = send(self.http, &self.req("PATCH", &path).json(&move_body))?;
        match response.status {
            200..=299 => Ok(true),
            // Ветку сдвинули — не перемотка вперёд.
            409 | 422 => Ok(false),
            _ => Err(status_error("сдвиг ветки", &response)),
        }
    }
}

impl GitHub<'_> {
    /// Ветка синхронизации должна быть: у пустого репозитория (создан без
    /// README — GitHub отвечает 409 «Git Repository is empty») Git Data API не
    /// работает вовсе, первый коммит делает Contents API; ветки нет, а
    /// репозиторий не пуст — она ответвляется от ветки по умолчанию.
    fn ensure_branch(&self, repo: &Value) -> Result<(), Error> {
        let path = self.repo_path(&format!("/git/ref/heads/{}", url_encode(&self.branch, b"")));
        let response = send(self.http, &self.req("GET", &path))?;
        if response.ok() {
            return Ok(());
        }
        let default = repo.str_field("default_branch").unwrap_or("main").to_string();
        let base = if response.status == 409 {
            None
        } else if response.status == 404 {
            let path = self.repo_path(&format!("/git/ref/heads/{}", url_encode(&default, b"")));
            let found = send(self.http, &self.req("GET", &path))?;
            match found.status {
                200..=299 => found.json()?.get("object").and_then(|o| o.str_field("sha")).map(str::to_string),
                // Ветки по умолчанию тоже нет — репозиторий пуст.
                404 | 409 => None,
                _ => return Err(status_error("ветка по умолчанию", &found)),
            }
        } else {
            return Err(status_error("конец ветки", &response));
        };
        match base {
            Some(sha) => {
                let body = obj(alloc::vec![("ref", s(&format!("refs/heads/{}", self.branch))), ("sha", s(&sha))]);
                send_ok(self.http, &self.req("POST", &self.repo_path("/git/refs")).json(&body), "создание ветки")?;
            }
            None => {
                let text = "# OBSOS\n\nЗакладки, история и настройки браузера OBSOS (FreeOS): журналы устройств в `devices/`.\n";
                let body = obj(alloc::vec![
                    ("message", s("OBSOS: начало синхронизации")),
                    ("content", s(&b64_encode(text.as_bytes()))),
                    ("branch", s(&self.branch)),
                ]);
                send_ok(self.http, &self.req("PUT", &self.repo_path("/contents/README.md")).json(&body), "первый коммит")?;
            }
        }
        Ok(())
    }
}

impl Remote for GitHub<'_> {
    fn prepare(&mut self) -> Result<(), Error> {
        let response = send(self.http, &self.req("GET", &self.repo_path("")))?;
        if response.ok() {
            return self.ensure_branch(&response.json()?);
        }
        if response.status != 404 {
            return Err(status_error("репозиторий", &response));
        }
        let (owner, name) = self.repo.split_once('/').ok_or_else(|| Error::NotFound("репозиторий владелец/имя".into()))?;
        let me = send_ok(self.http, &self.req("GET", "/user"), "пользователь")?.json()?;
        let login = me.str_field("login").unwrap_or("");
        let path = if login.eq_ignore_ascii_case(owner) { String::from("/user/repos") } else { format!("/orgs/{owner}/repos") };
        let body = obj(alloc::vec![
            ("name", s(name)),
            ("private", Value::Bool(true)),
            ("auto_init", Value::Bool(true)),
            ("description", s("OBSOS: закладки, история, настройки (синхронизация браузера)")),
        ]);
        send_ok(self.http, &self.req("POST", &path).json(&body), "создание репозитория")?;
        Ok(())
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        Ok(self
            .list("devices", at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("dir"))
            .filter_map(|e| e.str_field("name").map(str::to_string))
            .collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        Ok(self
            .list(&format!("devices/{device}"), at)?
            .iter()
            .filter(|e| e.str_field("type") == Some("file"))
            .filter_map(|e| {
                Some(RemoteFile { name: e.str_field("name")?.to_string(), version: e.str_field("sha").unwrap_or("").to_string() })
            })
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        let url = self.repo_path(&format!("/contents/{}?ref={}", url_encode(path, b"/"), self.reference(at)));
        let request = self.req("GET", &url).header("Accept", "application/vnd.github.raw+json");
        let response = send(self.http, &request)?;
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
        for _ in 0..4 {
            if self.commit_once(files, message)? {
                return Ok(());
            }
        }
        Err(Error::Protocol("ветку всё время сдвигают другие устройства".into()))
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let commit = self.head()?;
        let message = if label.is_empty() { name } else { label };
        let tag_body = obj(alloc::vec![("tag", s(name)), ("message", s(message)), ("object", s(&commit)), ("type", s("commit"))]);
        let tag = send_ok(self.http, &self.req("POST", &self.repo_path("/git/tags")).json(&tag_body), "тег")?.json()?;
        let tag_sha = tag.str_field("sha").ok_or_else(|| Error::Protocol("нет sha тега".into()))?;
        let ref_body = obj(alloc::vec![("ref", s(&format!("refs/tags/{name}"))), ("sha", s(tag_sha))]);
        send_ok(self.http, &self.req("POST", &self.repo_path("/git/refs")).json(&ref_body), "ссылка тега")?;
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let response = send(self.http, &self.req("GET", &self.repo_path("/tags?per_page=100")))?;
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if !response.ok() {
            return Err(status_error("теги", &response));
        }
        let mut points: Vec<SyncPoint> = arr(&response.json()?)
            .iter()
            .filter_map(|t| t.str_field("name"))
            .filter(|n| n.starts_with("obsos-"))
            .map(|n| SyncPoint { name: n.to_string(), label: String::new() })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(points)
    }
}
