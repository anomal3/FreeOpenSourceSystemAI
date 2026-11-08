//! Поддельные площадки для тестов: та часть API GitHub, GitLab, Bitbucket,
//! WebDAV и Google Drive, которой пользуется синхронизация, — в памяти, со
//! своими правилами (не та ветка — 422, `create` существующего — 400, нет
//! родительской папки — 409, чужой токен — 401). Клиенты проверяются на них
//! от запроса до разбора ответа.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use super::github::{arr, obj, s};
use super::{Http, Request, Response, b64_encode, form_decode, url_decode};
use crate::json::{self, Value};
use crate::sync::content_hash;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    GitHub,
    GitLab,
    Bitbucket,
    WebDav,
    GoogleDrive,
}

#[derive(Clone, Default)]
struct Commit {
    files: BTreeMap<String, String>,
    parent: Option<usize>,
}

#[derive(Default)]
struct Repo {
    exists: bool,
    commits: Vec<Commit>,
    head: Option<usize>,
    /// Имя → (коммит, сообщение).
    tags: BTreeMap<String, (usize, String)>,
    /// GitHub: деревья и объекты тегов до ссылки.
    trees: Vec<BTreeMap<String, String>>,
    tag_objects: Vec<(String, usize, String)>,
    /// WebDAV: папки и файлы (пути без `%`).
    dirs: BTreeSet<String>,
    files: BTreeMap<String, String>,
    /// Google Drive: элементы.
    items: Vec<DriveItem>,
}

#[derive(Clone)]
struct DriveItem {
    id: String,
    name: String,
    parent: String,
    folder: bool,
    content: String,
    description: String,
}

pub struct FakeServer {
    flavor: Flavor,
    repo: RefCell<Repo>,
    bad_auth: Cell<bool>,
}

fn resp(status: u16, body: impl Into<Vec<u8>>) -> Response {
    Response { status, headers: Vec::new(), body: body.into() }
}

fn json_resp(status: u16, v: Value) -> Response {
    resp(status, v.to_text().into_bytes())
}

fn split_url(url: &str) -> (String, BTreeMap<String, String>) {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path = rest.find('/').map(|i| &rest[i..]).unwrap_or("/");
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    (path.to_string(), form_decode(query).into_iter().collect())
}

fn body_json(req: &Request) -> Value {
    json::parse(&String::from_utf8_lossy(&req.body)).unwrap_or(Value::Null)
}

/// Непосредственное содержимое каталога в наборе файлов: (файлы, папки).
fn children(files: &BTreeMap<String, String>, dir: &str) -> (Vec<(String, String)>, Vec<String>) {
    let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
    let (mut f, mut d) = (Vec::new(), BTreeSet::new());
    for (path, data) in files {
        if let Some(rest) = path.strip_prefix(&prefix) {
            match rest.split_once('/') {
                Some((sub, _)) => {
                    d.insert(sub.to_string());
                }
                None => f.push((rest.to_string(), data.clone())),
            }
        }
    }
    (f, d.into_iter().collect())
}

impl FakeServer {
    pub fn new(flavor: Flavor) -> Self {
        let mut repo = Repo::default();
        if flavor == Flavor::WebDav {
            for d in ["/", "/remote.php", "/remote.php/dav", "/remote.php/dav/files", "/remote.php/dav/files/roman"] {
                repo.dirs.insert(d.to_string());
            }
        }
        FakeServer { flavor, repo: RefCell::new(repo), bad_auth: Cell::new(false) }
    }

    /// Все запросы шли с верным заголовком входа.
    pub fn auth_ok(&self) -> bool {
        !self.bad_auth.get()
    }

    fn expected_auth(&self) -> String {
        match self.flavor {
            Flavor::Bitbucket | Flavor::WebDav => format!("Basic {}", b64_encode(b"roman:t0ken")),
            _ => String::from("Bearer t0ken"),
        }
    }

    fn snapshot(repo: &Repo, reference: &str) -> Option<BTreeMap<String, String>> {
        if let Some((commit, _)) = repo.tags.get(reference) {
            return Some(repo.commits[*commit].files.clone());
        }
        if reference == "main" {
            return repo.head.map(|h| repo.commits[h].files.clone());
        }
        None
    }

    fn commit(repo: &mut Repo, files: BTreeMap<String, String>) -> usize {
        let parent = repo.head;
        repo.commits.push(Commit { files, parent });
        let id = repo.commits.len() - 1;
        repo.head = Some(id);
        id
    }

    fn init_repo(repo: &mut Repo, with_readme: bool) {
        repo.exists = true;
        if with_readme {
            let mut files = BTreeMap::new();
            files.insert(String::from("README.md"), String::from("# sync\n"));
            Self::commit(repo, files);
        }
    }

    fn github(&self, req: &Request, path: &str, q: &BTreeMap<String, String>) -> Response {
        let mut repo = self.repo.borrow_mut();
        let base = "/repos/roman/obsos-sync";
        if path == "/user" {
            return json_resp(200, obj(alloc::vec![("login", s("roman"))]));
        }
        if req.method == "POST" && path == "/user/repos" {
            Self::init_repo(&mut repo, body_json(req).get("auto_init") == Some(&Value::Bool(true)));
            return json_resp(201, obj(alloc::vec![]));
        }
        let Some(rest) = path.strip_prefix(base) else { return resp(404, "no route") };
        if !repo.exists {
            return resp(404, "Not Found");
        }
        match (req.method.as_str(), rest) {
            ("GET", "") => json_resp(200, obj(alloc::vec![("private", Value::Bool(true))])),
            ("GET", r) if r.starts_with("/contents/") => {
                let target = url_decode(&r["/contents/".len()..]);
                let Some(files) = Self::snapshot(&repo, q.get("ref").map(String::as_str).unwrap_or("main")) else {
                    return resp(404, "No commit found for the ref");
                };
                if let Some(data) = files.get(&target) {
                    return resp(200, data.clone());
                }
                let (f, d) = children(&files, &target);
                if f.is_empty() && d.is_empty() {
                    return resp(404, "Not Found");
                }
                let mut entries: Vec<Value> = d.iter().map(|n| obj(alloc::vec![("type", s("dir")), ("name", s(n))])).collect();
                entries.extend(f.iter().map(|(n, data)| {
                    obj(alloc::vec![("type", s("file")), ("name", s(n)), ("sha", s(&content_hash(data)))])
                }));
                json_resp(200, Value::Arr(entries))
            }
            ("GET", "/git/ref/heads/main") => match repo.head {
                Some(h) => json_resp(200, obj(alloc::vec![("object", obj(alloc::vec![("sha", s(&format!("c{h}")))]))])),
                None => resp(409, "Git Repository is empty."),
            },
            ("GET", r) if r.starts_with("/git/commits/c") => {
                let idx: usize = r["/git/commits/c".len()..].parse().unwrap_or(usize::MAX);
                if idx >= repo.commits.len() {
                    return resp(404, "no commit");
                }
                let files = repo.commits[idx].files.clone();
                repo.trees.push(files);
                let tree = repo.trees.len() - 1;
                json_resp(200, obj(alloc::vec![("tree", obj(alloc::vec![("sha", s(&format!("t{tree}")))]))]))
            }
            ("POST", "/git/trees") => {
                let body = body_json(req);
                let base_id: usize = body.str_field("base_tree").and_then(|t| t[1..].parse().ok()).unwrap_or(usize::MAX);
                let Some(mut files) = repo.trees.get(base_id).cloned() else { return resp(422, "bad base_tree") };
                for e in body.get("tree").map(arr).unwrap_or(&[]) {
                    if e.str_field("mode") != Some("100644") || e.str_field("type") != Some("blob") {
                        return resp(422, "bad tree entry");
                    }
                    files.insert(e.str_field("path").unwrap_or("").to_string(), e.str_field("content").unwrap_or("").to_string());
                }
                repo.trees.push(files);
                json_resp(201, obj(alloc::vec![("sha", s(&format!("t{}", repo.trees.len() - 1)))]))
            }
            ("POST", "/git/commits") => {
                let body = body_json(req);
                let tree: usize = body.str_field("tree").and_then(|t| t[1..].parse().ok()).unwrap_or(usize::MAX);
                let parent: Option<usize> = body.get("parents").map(arr).and_then(|p| p.first()).and_then(|p| p.as_str()).and_then(|p| p[1..].parse().ok());
                let Some(files) = repo.trees.get(tree).cloned() else { return resp(422, "bad tree") };
                repo.commits.push(Commit { files, parent });
                json_resp(201, obj(alloc::vec![("sha", s(&format!("c{}", repo.commits.len() - 1)))]))
            }
            ("PATCH", "/git/refs/heads/main") => {
                let sha: usize = body_json(req).str_field("sha").and_then(|t| t[1..].parse().ok()).unwrap_or(usize::MAX);
                match repo.commits.get(sha) {
                    Some(c) if c.parent == repo.head => {
                        repo.head = Some(sha);
                        json_resp(200, obj(alloc::vec![]))
                    }
                    Some(_) => resp(422, "Update is not a fast forward"),
                    None => resp(422, "bad sha"),
                }
            }
            ("POST", "/git/tags") => {
                let body = body_json(req);
                let commit: usize = body.str_field("object").and_then(|t| t[1..].parse().ok()).unwrap_or(usize::MAX);
                let tag = (body.str_field("tag").unwrap_or("").to_string(), commit, body.str_field("message").unwrap_or("").to_string());
                repo.tag_objects.push(tag);
                json_resp(201, obj(alloc::vec![("sha", s(&format!("g{}", repo.tag_objects.len() - 1)))]))
            }
            ("POST", "/git/refs") => {
                let body = body_json(req);
                let name = body.str_field("ref").and_then(|r| r.strip_prefix("refs/tags/")).unwrap_or("").to_string();
                let id: usize = body.str_field("sha").and_then(|t| t[1..].parse().ok()).unwrap_or(usize::MAX);
                let Some((tag, commit, message)) = repo.tag_objects.get(id).cloned() else { return resp(422, "bad tag") };
                if tag != name || repo.tags.contains_key(&name) {
                    return resp(422, "Reference already exists");
                }
                repo.tags.insert(name, (commit, message));
                json_resp(201, obj(alloc::vec![]))
            }
            ("GET", "/tags") => json_resp(200, Value::Arr(repo.tags.keys().map(|n| obj(alloc::vec![("name", s(n))])).collect())),
            _ => resp(404, format!("no route {} {rest}", req.method)),
        }
    }

    fn gitlab(&self, req: &Request, path: &str, q: &BTreeMap<String, String>) -> Response {
        let mut repo = self.repo.borrow_mut();
        if req.method == "POST" && path == "/api/v4/projects" {
            let body = body_json(req);
            if body.str_field("visibility") != Some("private") {
                return resp(400, "must be private in this test");
            }
            Self::init_repo(&mut repo, body.get("initialize_with_readme") == Some(&Value::Bool(true)));
            return json_resp(201, obj(alloc::vec![]));
        }
        let Some(rest) = path.strip_prefix("/api/v4/projects/roman%2Fobsos-sync") else { return resp(404, "404 Project Not Found") };
        if !repo.exists {
            return resp(404, "404 Project Not Found");
        }
        let reference = q.get("ref").map(String::as_str).unwrap_or("main");
        match (req.method.as_str(), rest) {
            ("GET", "") => json_resp(200, obj(alloc::vec![])),
            ("GET", "/repository/tree") => {
                let Some(files) = Self::snapshot(&repo, reference) else { return resp(404, "404 Tree Not Found") };
                let (f, d) = children(&files, q.get("path").map(String::as_str).unwrap_or(""));
                let mut entries: Vec<Value> = d.iter().map(|n| obj(alloc::vec![("type", s("tree")), ("name", s(n))])).collect();
                entries.extend(f.iter().map(|(n, data)| obj(alloc::vec![("type", s("blob")), ("name", s(n)), ("id", s(&content_hash(data)))])));
                json_resp(200, Value::Arr(entries))
            }
            ("GET", r) if r.starts_with("/repository/files/") && r.ends_with("/raw") => {
                let file = url_decode(&r["/repository/files/".len()..r.len() - 4]);
                match Self::snapshot(&repo, reference).and_then(|f| f.get(&file).cloned()) {
                    Some(data) => resp(200, data),
                    None => resp(404, "404 File Not Found"),
                }
            }
            ("POST", "/repository/commits") => {
                let body = body_json(req);
                if body.str_field("branch") != Some("main") {
                    return resp(400, "bad branch");
                }
                let mut files = repo.head.map(|h| repo.commits[h].files.clone()).unwrap_or_default();
                for a in body.get("actions").map(arr).unwrap_or(&[]) {
                    let path = a.str_field("file_path").unwrap_or("").to_string();
                    match a.str_field("action") {
                        Some("create") if files.contains_key(&path) => return resp(400, "A file with this name already exists"),
                        Some("update") if !files.contains_key(&path) => return resp(400, "A file with this name doesn't exist"),
                        Some("create" | "update") => {
                            files.insert(path, a.str_field("content").unwrap_or("").to_string());
                        }
                        _ => return resp(400, "bad action"),
                    }
                }
                Self::commit(&mut repo, files);
                json_resp(201, obj(alloc::vec![]))
            }
            ("POST", "/repository/tags") => {
                let body = body_json(req);
                let (Some(name), Some(head)) = (body.str_field("tag_name"), repo.head) else { return resp(400, "bad tag") };
                repo.tags.insert(name.to_string(), (head, body.str_field("message").unwrap_or("").to_string()));
                json_resp(201, obj(alloc::vec![]))
            }
            ("GET", "/repository/tags") => json_resp(
                200,
                Value::Arr(repo.tags.iter().map(|(n, (_, m))| obj(alloc::vec![("name", s(n)), ("message", s(m))])).collect()),
            ),
            _ => resp(404, format!("no route {} {rest}", req.method)),
        }
    }

    fn bitbucket(&self, req: &Request, path: &str) -> Response {
        let mut repo = self.repo.borrow_mut();
        let Some(rest) = path.strip_prefix("/2.0/repositories/roman/obsos-sync") else { return resp(404, "no route") };
        match (req.method.as_str(), rest) {
            ("GET", "") => return if repo.exists { json_resp(200, obj(alloc::vec![])) } else { resp(404, "not found") },
            ("POST", "") => {
                Self::init_repo(&mut repo, false);
                return json_resp(200, obj(alloc::vec![]));
            }
            _ => {}
        }
        if !repo.exists {
            return resp(404, "not found");
        }
        match (req.method.as_str(), rest) {
            ("POST", "/src") => {
                let fields = form_decode(&String::from_utf8_lossy(&req.body));
                if !fields.iter().any(|(k, v)| k == "branch" && v == "main") {
                    return resp(400, "no branch");
                }
                let mut files = repo.head.map(|h| repo.commits[h].files.clone()).unwrap_or_default();
                for (k, v) in fields {
                    if k != "message" && k != "branch" {
                        files.insert(k.trim_start_matches('/').to_string(), v);
                    }
                }
                Self::commit(&mut repo, files);
                resp(201, "")
            }
            ("GET", r) if r.starts_with("/src/") => {
                let r = &r["/src/".len()..];
                let (reference, file) = r.split_once('/').unwrap_or((r, ""));
                let Some(files) = Self::snapshot(&repo, &url_decode(reference)) else { return resp(404, "no commit") };
                let file = url_decode(file);
                if !file.ends_with('/') {
                    return match files.get(&file) {
                        Some(d) => resp(200, d.clone()),
                        None => resp(404, "no file"),
                    };
                }
                let dir = file.trim_end_matches('/');
                let (f, d) = children(&files, dir);
                if f.is_empty() && d.is_empty() {
                    return resp(404, "no dir");
                }
                let mut values: Vec<Value> =
                    d.iter().map(|n| obj(alloc::vec![("type", s("commit_directory")), ("path", s(&format!("{dir}/{n}")))])).collect();
                values.extend(f.iter().map(|(n, data)| {
                    obj(alloc::vec![
                        ("type", s("commit_file")),
                        ("path", s(&format!("{dir}/{n}"))),
                        ("size", Value::Num(data.len().to_string())),
                    ])
                }));
                json_resp(200, obj(alloc::vec![("values", Value::Arr(values))]))
            }
            ("GET", "/refs/branches/main") => match repo.head {
                Some(h) => json_resp(200, obj(alloc::vec![("target", obj(alloc::vec![("hash", s(&format!("c{h}")))]))])),
                None => resp(404, "no branch"),
            },
            ("POST", "/refs/tags") => {
                let body = body_json(req);
                let hash: usize = body.get("target").and_then(|t| t.str_field("hash")).and_then(|h| h[1..].parse().ok()).unwrap_or(usize::MAX);
                if hash >= repo.commits.len() {
                    return resp(400, "bad hash");
                }
                repo.tags.insert(body.str_field("name").unwrap_or("").to_string(), (hash, body.str_field("message").unwrap_or("").to_string()));
                json_resp(201, obj(alloc::vec![]))
            }
            ("GET", "/refs/tags") => json_resp(
                200,
                obj(alloc::vec![(
                    "values",
                    Value::Arr(repo.tags.iter().map(|(n, (_, m))| obj(alloc::vec![("name", s(n)), ("message", s(m))])).collect())
                )]),
            ),
            _ => resp(404, format!("no route {} {rest}", req.method)),
        }
    }

    fn webdav(&self, req: &Request, path: &str) -> Response {
        let mut repo = self.repo.borrow_mut();
        let path = url_decode(path);
        let clean = path.trim_end_matches('/').to_string();
        let clean = if clean.is_empty() { String::from("/") } else { clean };
        let parent = clean.rsplit_once('/').map(|(p, _)| if p.is_empty() { "/" } else { p }).unwrap_or("/").to_string();
        match req.method.as_str() {
            "MKCOL" => {
                if repo.dirs.contains(&clean) {
                    resp(405, "exists")
                } else if !repo.dirs.contains(&parent) {
                    resp(409, "no parent")
                } else {
                    repo.dirs.insert(clean);
                    resp(201, "")
                }
            }
            "PUT" => {
                if !repo.dirs.contains(&parent) {
                    return resp(409, "no parent");
                }
                repo.files.insert(clean, String::from_utf8_lossy(&req.body).into_owned());
                resp(201, "")
            }
            "GET" => match repo.files.get(&clean) {
                Some(d) => resp(200, d.clone()),
                None => resp(404, "not found"),
            },
            "PROPFIND" => {
                if !repo.dirs.contains(&clean) {
                    return resp(404, "not found");
                }
                let mut xml = String::from("<?xml version=\"1.0\"?><d:multistatus xmlns:d=\"DAV:\">");
                let mut entry = |href: &str, dir: bool, etag: &str| {
                    xml.push_str(&format!(
                        "<d:response><d:href>{}</d:href><d:propstat><d:prop><d:resourcetype>{}</d:resourcetype><d:getetag>\"{etag}\"</d:getetag></d:prop></d:propstat></d:response>",
                        super::url_encode(href, b"/"),
                        if dir { "<d:collection/>" } else { "" }
                    ));
                };
                entry(&format!("{clean}/"), true, "dir");
                let prefix = format!("{clean}/");
                for d in repo.dirs.iter().filter(|d| d.strip_prefix(&prefix).is_some_and(|r| !r.is_empty() && !r.contains('/'))) {
                    entry(&format!("{d}/"), true, "dir");
                }
                for (f, data) in repo.files.iter().filter(|(f, _)| f.strip_prefix(&prefix).is_some_and(|r| !r.contains('/'))) {
                    entry(f, false, &content_hash(data));
                }
                xml.push_str("</d:multistatus>");
                resp(207, xml)
            }
            "COPY" => {
                let Some(dest) = req.headers.iter().find(|(k, _)| k == "Destination").map(|(_, v)| v.clone()) else {
                    return resp(400, "no Destination");
                };
                let (dest, _) = split_url(&dest);
                let dest = url_decode(dest.trim_end_matches('/'));
                if !repo.dirs.contains(&clean) {
                    return resp(404, "no source");
                }
                let src_prefix = format!("{clean}/");
                let dirs: Vec<String> = repo.dirs.iter().filter(|d| d.starts_with(&src_prefix)).cloned().collect();
                repo.dirs.insert(dest.clone());
                for d in dirs {
                    repo.dirs.insert(format!("{dest}/{}", &d[src_prefix.len()..]));
                }
                let files: Vec<(String, String)> = repo.files.iter().filter(|(f, _)| f.starts_with(&src_prefix)).map(|(f, d)| (f.clone(), d.clone())).collect();
                for (f, d) in files {
                    repo.files.insert(format!("{dest}/{}", &f[src_prefix.len()..]), d);
                }
                resp(201, "")
            }
            _ => resp(405, "method"),
        }
    }

    fn gdrive(&self, req: &Request, path: &str, q: &BTreeMap<String, String>) -> Response {
        let mut repo = self.repo.borrow_mut();
        let item_json = |i: &DriveItem| {
            obj(alloc::vec![
                ("id", s(&i.id)),
                ("name", s(&i.name)),
                ("mimeType", s(if i.folder { "application/vnd.google-apps.folder" } else { "text/plain" })),
                ("md5Checksum", s(&content_hash(&i.content))),
                ("description", s(&i.description)),
            ])
        };
        let new_id = |repo: &Repo| format!("id{}", repo.items.len() + 1);
        match (req.method.as_str(), path) {
            ("GET", "/drive/v3/files") => {
                let query = q.get("q").cloned().unwrap_or_default();
                let unquote = |from: usize| -> String {
                    let mut out = String::new();
                    let mut chars = query[from..].chars();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => out.extend(chars.next()),
                            '\'' => break,
                            c => out.push(c),
                        }
                    }
                    out
                };
                let name = query.find("name='").map(|i| unquote(i + 6));
                let parent = query.find(" in parents").and_then(|end| {
                    let start = query[..end].rfind("'")?;
                    let open = query[..start].rfind("'")?;
                    Some(unquote(open + 1))
                });
                let files: Vec<Value> = repo
                    .items
                    .iter()
                    .filter(|i| parent.as_deref().is_none_or(|p| i.parent == p) && name.as_deref().is_none_or(|n| i.name == n))
                    .map(item_json)
                    .collect();
                json_resp(200, obj(alloc::vec![("files", Value::Arr(files))]))
            }
            ("POST", "/drive/v3/files") => {
                let body = body_json(req);
                let id = new_id(&repo);
                repo.items.push(DriveItem {
                    id: id.clone(),
                    name: body.str_field("name").unwrap_or("").to_string(),
                    parent: body.get("parents").map(arr).and_then(|p| p.first()).and_then(|p| p.as_str()).unwrap_or("root").to_string(),
                    folder: body.str_field("mimeType") == Some("application/vnd.google-apps.folder"),
                    content: String::new(),
                    description: body.str_field("description").unwrap_or("").to_string(),
                });
                json_resp(200, obj(alloc::vec![("id", s(&id))]))
            }
            ("POST", "/upload/drive/v3/files") => {
                let text = String::from_utf8_lossy(&req.body).into_owned();
                let parts: Vec<&str> = text.split("--obsos-sync-boundary-7f3a").collect();
                if parts.len() < 3 || q.get("uploadType").map(String::as_str) != Some("multipart") {
                    return resp(400, "bad multipart");
                }
                let body_of = |p: &str| p.split_once("\r\n\r\n").map(|(_, b)| b.trim_end_matches("\r\n").to_string()).unwrap_or_default();
                let meta = json::parse(&body_of(parts[1])).unwrap_or(Value::Null);
                let id = new_id(&repo);
                repo.items.push(DriveItem {
                    id: id.clone(),
                    name: meta.str_field("name").unwrap_or("").to_string(),
                    parent: meta.get("parents").map(arr).and_then(|p| p.first()).and_then(|p| p.as_str()).unwrap_or("root").to_string(),
                    folder: false,
                    content: body_of(parts[2]),
                    description: String::new(),
                });
                json_resp(200, obj(alloc::vec![("id", s(&id))]))
            }
            ("PATCH", p) if p.starts_with("/upload/drive/v3/files/") => {
                let id = &p["/upload/drive/v3/files/".len()..];
                match repo.items.iter_mut().find(|i| i.id == id) {
                    Some(item) => {
                        item.content = String::from_utf8_lossy(&req.body).into_owned();
                        json_resp(200, obj(alloc::vec![("id", s(id))]))
                    }
                    None => resp(404, "File not found"),
                }
            }
            ("GET", p) if p.starts_with("/drive/v3/files/") && q.get("alt").map(String::as_str) == Some("media") => {
                let id = &p["/drive/v3/files/".len()..];
                match repo.items.iter().find(|i| i.id == id) {
                    Some(item) => resp(200, item.content.clone()),
                    None => resp(404, "File not found"),
                }
            }
            ("POST", p) if p.starts_with("/drive/v3/files/") && p.ends_with("/copy") => {
                let id = &p["/drive/v3/files/".len()..p.len() - 5];
                let Some(src) = repo.items.iter().find(|i| i.id == id).cloned() else { return resp(404, "File not found") };
                let body = body_json(req);
                let copy = DriveItem {
                    id: new_id(&repo),
                    name: body.str_field("name").unwrap_or(&src.name).to_string(),
                    parent: body.get("parents").map(arr).and_then(|p| p.first()).and_then(|p| p.as_str()).unwrap_or("root").to_string(),
                    ..src
                };
                let id = copy.id.clone();
                repo.items.push(copy);
                json_resp(200, obj(alloc::vec![("id", s(&id))]))
            }
            _ => resp(404, format!("no route {} {path}", req.method)),
        }
    }
}

impl Http for FakeServer {
    fn send(&self, req: &Request) -> Result<Response, String> {
        let auth = req.headers.iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v.as_str()).unwrap_or("");
        if auth != self.expected_auth() {
            self.bad_auth.set(true);
            return Ok(resp(401, "Bad credentials"));
        }
        let (path, q) = split_url(&req.url);
        Ok(match self.flavor {
            Flavor::GitHub => self.github(req, &path, &q),
            Flavor::GitLab => self.gitlab(req, &path, &q),
            Flavor::Bitbucket => self.bitbucket(req, &path),
            Flavor::WebDav => self.webdav(req, &path),
            Flavor::GoogleDrive => self.gdrive(req, &path, &q),
        })
    }
}
