//! Профиль OBSOS: `~/.config/obsos/<профиль>/`.
//!
//! * `servo/` — куки, хранилище сайтов (Servo);
//! * `sync/` — реплика синхронизации (`devices/<это устройство>/…`):
//!   закладки, история, общие настройки, пароли (зашифрованы, `vault.rs`);
//! * `settings.json` — настройки этого устройства (оформление, раскладка,
//!   синхронизация), `session.json` — открытые вкладки;
//! * `vault.json`, `device.key` — хранилище секретов.
//!
//! Профилей может быть несколько (`obsos --profile работа`): у каждого свои
//! куки, закладки и синхронизация.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use browser_sync::json::{self, Value};
use browser_sync::{DirStore, Replica};

use crate::vault::{Key, Login};

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Цвет `#rrggbb`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn parse(text: &str) -> Option<Rgb> {
        let t = text.trim().trim_start_matches('#');
        if t.len() != 6 {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(&t[i..i + 2], 16).ok();
        Some(Rgb(c(0)?, c(2)?, c(4)?))
    }

    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Оформление: палитра целиком задаётся пользователем (у Zervo цвета тем —
/// константы), сохраняется под своим именем и синхронизируется.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Look {
    pub name: String,
    pub dark: bool,
    pub accent: Rgb,
    pub accent2: Rgb,
    pub sidebar: Rgb,
    pub toolbar: Rgb,
    pub text: Rgb,
    /// Вкладки: слева (`true`) или полосой сверху.
    pub side_tabs: bool,
    /// Ширина боковой панели (точки).
    pub sidebar_width: u32,
    /// Насколько далеко акцент заходит в оболочку: 0 — только выделение,
    /// 1 — ещё активная вкладка, 2 — и кнопки.
    pub accent_reach: u8,
    pub compact: bool,
}

impl Look {
    pub fn dark() -> Look {
        Look {
            name: String::from("Тёмное"),
            dark: true,
            accent: Rgb(0x7a, 0x4f, 0xd8),
            accent2: Rgb(0x2b, 0xb3, 0xb1),
            sidebar: Rgb(0x17, 0x1a, 0x2e),
            toolbar: Rgb(0x20, 0x24, 0x3c),
            text: Rgb(0xe8, 0xec, 0xf8),
            side_tabs: true,
            sidebar_width: 216,
            accent_reach: 1,
            compact: false,
        }
    }

    pub fn light() -> Look {
        Look {
            name: String::from("Светлое"),
            dark: false,
            accent: Rgb(0x5b, 0x3c, 0xc4),
            accent2: Rgb(0x13, 0x8f, 0x8d),
            sidebar: Rgb(0xec, 0xee, 0xf6),
            toolbar: Rgb(0xf7, 0xf8, 0xfc),
            text: Rgb(0x1d, 0x24, 0x33),
            side_tabs: true,
            sidebar_width: 216,
            accent_reach: 1,
            compact: false,
        }
    }

    pub fn to_json(&self) -> Value {
        let s = |v: String| Value::Str(v);
        Value::Obj(vec![
            ("name".into(), s(self.name.clone())),
            ("dark".into(), Value::Bool(self.dark)),
            ("accent".into(), s(self.accent.hex())),
            ("accent2".into(), s(self.accent2.hex())),
            ("sidebar".into(), s(self.sidebar.hex())),
            ("toolbar".into(), s(self.toolbar.hex())),
            ("text".into(), s(self.text.hex())),
            ("side_tabs".into(), Value::Bool(self.side_tabs)),
            ("sidebar_width".into(), Value::Num(self.sidebar_width.to_string())),
            ("accent_reach".into(), Value::Num(self.accent_reach.to_string())),
            ("compact".into(), Value::Bool(self.compact)),
        ])
    }

    pub fn from_json(v: &Value) -> Option<Look> {
        let base = if v.get("dark") == Some(&Value::Bool(false)) { Look::light() } else { Look::dark() };
        let color = |k: &str, d: Rgb| v.str_field(k).and_then(Rgb::parse).unwrap_or(d);
        let num = |k: &str| match v.get(k) {
            Some(Value::Num(n)) => n.parse::<u32>().ok(),
            _ => None,
        };
        Some(Look {
            name: v.str_field("name")?.to_string(),
            dark: base.dark,
            accent: color("accent", base.accent),
            accent2: color("accent2", base.accent2),
            sidebar: color("sidebar", base.sidebar),
            toolbar: color("toolbar", base.toolbar),
            text: color("text", base.text),
            side_tabs: v.get("side_tabs") != Some(&Value::Bool(false)),
            sidebar_width: num("sidebar_width").unwrap_or(216).clamp(160, 420),
            accent_reach: num("accent_reach").unwrap_or(1).min(2) as u8,
            compact: v.get("compact") == Some(&Value::Bool(true)),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Search {
    DuckDuckGo,
    Yandex,
    Google,
    Startpage,
}

impl Search {
    pub const ALL: [Search; 4] = [Search::DuckDuckGo, Search::Yandex, Search::Google, Search::Startpage];

    pub fn id(self) -> &'static str {
        match self {
            Search::DuckDuckGo => "ddg",
            Search::Yandex => "yandex",
            Search::Google => "google",
            Search::Startpage => "startpage",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Search::DuckDuckGo => "DuckDuckGo",
            Search::Yandex => "Яндекс",
            Search::Google => "Google",
            Search::Startpage => "Startpage",
        }
    }

    /// Адрес поиска (без JavaScript, где у поисковика есть такая версия —
    /// Servo молод, тяжёлые страницы выдачи у него ломаются).
    pub fn url(self, query: &str) -> url::Url {
        let base = match self {
            Search::DuckDuckGo => "https://html.duckduckgo.com/html/",
            Search::Yandex => "https://yandex.ru/search/",
            Search::Google => "https://www.google.com/search",
            Search::Startpage => "https://www.startpage.com/do/search",
        };
        let mut url = url::Url::parse(base).expect("search url");
        url.query_pairs_mut().append_pair(if self == Search::Yandex { "text" } else { "q" }, query);
        url
    }

    pub fn from_id(id: &str) -> Search {
        Search::ALL.into_iter().find(|s| s.id() == id).unwrap_or(Search::DuckDuckGo)
    }
}

/// Настройки этого устройства.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub device: String,
    pub device_name: String,
    pub look: Look,
    /// Сохранённые оформления (вместе с встроенными).
    pub looks: Vec<Look>,
    pub search: Search,
    pub home: String,
    pub restore_session: bool,
    pub save_passwords: bool,
    pub autofill: bool,
    pub sync_auto: bool,
    /// Минуты между автоматическими синхронизациями.
    pub sync_every_min: u32,
    /// Отмечать каждую автоматическую синхронизацию точкой (тегом).
    pub sync_points_auto: bool,
    pub history_days: u32,
}

impl Settings {
    fn new_device() -> String {
        let r: [u8; 4] = crate::vault::random();
        format!("pc-{:02x}{:02x}{:02x}{:02x}", r[0], r[1], r[2], r[3])
    }

    pub fn default_for(device: String) -> Settings {
        Settings {
            device_name: String::from("Этот компьютер"),
            device,
            look: Look::dark(),
            looks: vec![Look::dark(), Look::light()],
            search: Search::DuckDuckGo,
            home: String::from("obsos://newtab"),
            restore_session: true,
            save_passwords: true,
            autofill: true,
            sync_auto: true,
            sync_every_min: 15,
            sync_points_auto: false,
            history_days: 90,
        }
    }

    pub fn to_text(&self) -> String {
        Value::Obj(vec![
            ("device".into(), Value::Str(self.device.clone())),
            ("device_name".into(), Value::Str(self.device_name.clone())),
            ("look".into(), self.look.to_json()),
            ("looks".into(), Value::Arr(self.looks.iter().map(Look::to_json).collect())),
            ("search".into(), Value::Str(self.search.id().into())),
            ("home".into(), Value::Str(self.home.clone())),
            ("restore_session".into(), Value::Bool(self.restore_session)),
            ("save_passwords".into(), Value::Bool(self.save_passwords)),
            ("autofill".into(), Value::Bool(self.autofill)),
            ("sync_auto".into(), Value::Bool(self.sync_auto)),
            ("sync_every_min".into(), Value::Num(self.sync_every_min.to_string())),
            ("sync_points_auto".into(), Value::Bool(self.sync_points_auto)),
            ("history_days".into(), Value::Num(self.history_days.to_string())),
        ])
        .to_text()
    }

    pub fn parse(text: &str) -> Option<Settings> {
        let v = json::parse(text).ok()?;
        let device = v.str_field("device").filter(|d| browser_sync::replica::valid_name(d))?.to_string();
        let mut s = Settings::default_for(device);
        let flag = |k: &str, d: bool| match v.get(k) {
            Some(Value::Bool(b)) => *b,
            _ => d,
        };
        let num = |k: &str, d: u32| match v.get(k) {
            Some(Value::Num(n)) => n.parse().unwrap_or(d),
            _ => d,
        };
        if let Some(name) = v.str_field("device_name") {
            s.device_name = name.to_string();
        }
        if let Some(look) = v.get("look").and_then(Look::from_json) {
            s.look = look;
        }
        if let Some(Value::Arr(items)) = v.get("looks") {
            let looks: Vec<Look> = items.iter().filter_map(Look::from_json).collect();
            if !looks.is_empty() {
                s.looks = looks;
            }
        }
        s.search = Search::from_id(v.str_field("search").unwrap_or(""));
        if let Some(home) = v.str_field("home") {
            s.home = home.to_string();
        }
        s.restore_session = flag("restore_session", s.restore_session);
        s.save_passwords = flag("save_passwords", s.save_passwords);
        s.autofill = flag("autofill", s.autofill);
        s.sync_auto = flag("sync_auto", s.sync_auto);
        s.sync_every_min = num("sync_every_min", s.sync_every_min).clamp(1, 24 * 60);
        s.sync_points_auto = flag("sync_points_auto", s.sync_points_auto);
        s.history_days = num("history_days", s.history_days).clamp(1, 3650);
        Some(s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bookmark {
    pub url: String,
    pub title: String,
    pub added_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Visit {
    pub url: String,
    pub title: String,
    pub last_ms: u64,
    pub count: u64,
}

pub const BOOKMARKS: &str = "bookmarks";
pub const HISTORY: &str = "history";
pub const SHARED: &str = "settings";
pub const PASSWORDS: &str = "passwords";
/// Коллекции, которые откатываются к точке синхронизации.
pub const SYNCED: &[&str] = &[BOOKMARKS, HISTORY, SHARED, PASSWORDS];

pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
    pub settings: Settings,
    pub replica: Replica<DirStore>,
    pub bookmarks: Vec<Bookmark>,
    pub history: Vec<Visit>,
    /// Пароли, которые удалось расшифровать (ключ записи → вход).
    pub logins: BTreeMap<String, Login>,
    /// Записей паролей, зашифрованных чужим ключом.
    pub foreign_logins: usize,
}

fn read_json_field(v: &Value, key: &str) -> String {
    v.str_field(key).unwrap_or("").to_string()
}

impl Profile {
    pub fn root() -> PathBuf {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("/home"));
        home.join(".config").join("obsos")
    }

    /// Профили, которые уже есть.
    pub fn list() -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(Self::root())
            .map(|r| r.flatten().filter(|e| e.path().join("settings.json").is_file()).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        out.sort();
        if !out.iter().any(|p| p == "default") {
            out.insert(0, String::from("default"));
        }
        out
    }

    pub fn open(name: &str) -> Profile {
        let safe: String = name.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect();
        let name = if safe.is_empty() { String::from("default") } else { safe };
        let dir = Self::root().join(&name);
        let _ = std::fs::create_dir_all(dir.join("sync"));
        let settings = std::fs::read_to_string(dir.join("settings.json"))
            .ok()
            .and_then(|t| Settings::parse(&t))
            .unwrap_or_else(|| Settings::default_for(Settings::new_device()));
        let replica = Replica::new(DirStore::new(dir.join("sync")), &settings.device);
        let mut profile = Profile {
            name,
            dir,
            settings,
            replica,
            bookmarks: Vec::new(),
            history: Vec::new(),
            logins: BTreeMap::new(),
            foreign_logins: 0,
        };
        profile.save_settings();
        profile.compact_if_large();
        profile.reload();
        profile
    }

    pub fn servo_dir(&self) -> PathBuf {
        let dir = self.dir.join("servo");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    pub fn sync_dir(&self) -> PathBuf {
        self.dir.join("sync")
    }

    pub fn save_settings(&self) {
        let path = self.dir.join("settings.json");
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, self.settings.to_text()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    /// Перечитать закладки и историю (после синхронизации — там чужие
    /// операции). Пароли — `reload_logins` (нужен ключ).
    pub fn reload(&mut self) {
        let mut bookmarks = Vec::new();
        if let Ok(view) = self.replica.load(BOOKMARKS) {
            for (url, value) in view.state.live() {
                let v = json::parse(value).unwrap_or(Value::Null);
                bookmarks.push(Bookmark {
                    url: url.to_string(),
                    title: read_json_field(&v, "title"),
                    added_ms: v.str_field("added").and_then(|a| a.parse().ok()).unwrap_or(0),
                });
            }
        }
        bookmarks.sort_by(|a, b| b.added_ms.cmp(&a.added_ms));
        self.bookmarks = bookmarks;
        let mut history = Vec::new();
        let cutoff = now_ms().saturating_sub(self.settings.history_days as u64 * 86_400_000);
        if let Ok(view) = self.replica.load(HISTORY) {
            for (url, value) in view.state.live() {
                let v = json::parse(value).unwrap_or(Value::Null);
                let last_ms = v.str_field("last").and_then(|a| a.parse().ok()).unwrap_or(0);
                if last_ms < cutoff {
                    continue;
                }
                history.push(Visit {
                    url: url.to_string(),
                    title: read_json_field(&v, "title"),
                    last_ms,
                    count: v.str_field("count").and_then(|a| a.parse().ok()).unwrap_or(1),
                });
            }
        }
        history.sort_by(|a, b| b.last_ms.cmp(&a.last_ms));
        self.history = history;
        self.apply_shared();
    }

    /// Общие настройки (из коллекции `settings`): поиск, домашняя страница,
    /// сохранённые оформления — одинаковы на всех устройствах.
    fn apply_shared(&mut self) {
        let Ok(view) = self.replica.load(SHARED) else { return };
        if let Some(search) = view.state.get("search") {
            self.settings.search = Search::from_id(search);
        }
        if let Some(home) = view.state.get("home") {
            self.settings.home = home.to_string();
        }
        for (key, value) in view.state.live() {
            if let Some(name) = key.strip_prefix("look/") {
                if let Some(look) = json::parse(value).ok().as_ref().and_then(Look::from_json) {
                    match self.settings.looks.iter_mut().find(|l| l.name == name) {
                        Some(slot) => *slot = look,
                        None => self.settings.looks.push(look),
                    }
                }
            }
        }
    }

    pub fn put_shared(&mut self, key: &str, value: &str) {
        let _ = self.replica.put(SHARED, key, value, now_ms()).map_err(write_failed);
    }

    pub fn save_look(&mut self, look: &Look) {
        match self.settings.looks.iter_mut().find(|l| l.name == look.name) {
            Some(slot) => *slot = look.clone(),
            None => self.settings.looks.push(look.clone()),
        }
        let text = look.to_json().to_text();
        self.put_shared(&format!("look/{}", look.name), &text);
        self.save_settings();
    }

    pub fn delete_look(&mut self, name: &str) {
        self.settings.looks.retain(|l| l.name != name);
        let _ = self.replica.del(SHARED, &format!("look/{name}"), now_ms()).map_err(write_failed);
        self.save_settings();
    }

    pub fn is_bookmarked(&self, url: &str) -> bool {
        self.bookmarks.iter().any(|b| b.url == url)
    }

    pub fn add_bookmark(&mut self, url: &str, title: &str) {
        let added = now_ms();
        let value = Value::Obj(vec![("title".into(), Value::Str(title.into())), ("added".into(), Value::Str(added.to_string()))]).to_text();
        if self.replica.put(BOOKMARKS, url, &value, added).map_err(write_failed).is_ok() {
            self.bookmarks.retain(|b| b.url != url);
            self.bookmarks.insert(0, Bookmark { url: url.into(), title: title.into(), added_ms: added });
        }
    }

    pub fn remove_bookmark(&mut self, url: &str) {
        if self.replica.del(BOOKMARKS, url, now_ms()).map_err(write_failed).is_ok() {
            self.bookmarks.retain(|b| b.url != url);
        }
    }

    /// Посещение (страница загрузилась). Внутренние страницы не пишутся.
    pub fn visit(&mut self, url: &str, title: &str) {
        if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://")) {
            return;
        }
        let now = now_ms();
        let count = self.history.iter().find(|v| v.url == url).map(|v| v.count + 1).unwrap_or(1);
        let value = Value::Obj(vec![
            ("title".into(), Value::Str(title.into())),
            ("last".into(), Value::Str(now.to_string())),
            ("count".into(), Value::Str(count.to_string())),
        ])
        .to_text();
        if self.replica.put(HISTORY, url, &value, now).map_err(write_failed).is_ok() {
            self.history.retain(|v| v.url != url);
            self.history.insert(0, Visit { url: url.into(), title: title.into(), last_ms: now, count });
        }
    }

    pub fn forget(&mut self, url: &str) {
        if self.replica.del(HISTORY, url, now_ms()).map_err(write_failed).is_ok() {
            self.history.retain(|v| v.url != url);
        }
    }

    pub fn clear_history(&mut self) {
        let urls: Vec<String> = self.history.iter().map(|v| v.url.clone()).collect();
        let now = now_ms();
        for url in urls {
            let _ = self.replica.del(HISTORY, &url, now).map_err(write_failed);
        }
        self.history.clear();
    }

    /// Журнал истории растёт с каждой страницей — при запуске сжимаем
    /// большие журналы в снимок.
    fn compact_if_large(&mut self) {
        for coll in SYNCED {
            let path = self.sync_dir().join("devices").join(&self.settings.device).join(format!("{coll}.log"));
            if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
                let _ = self.replica.compact(coll, now_ms());
            }
        }
    }

    // ── Пароли ──────────────────────────────────────────────────────────

    pub fn reload_logins(&mut self, key: &Key) {
        self.logins.clear();
        self.foreign_logins = 0;
        if let Ok(view) = self.replica.load(PASSWORDS) {
            for (record, sealed) in view.state.live() {
                match Login::open(key, sealed) {
                    Some(login) => {
                        self.logins.insert(record.to_string(), login);
                    }
                    None => self.foreign_logins += 1,
                }
            }
        }
    }

    pub fn logins_for(&self, origin: &str) -> Vec<Login> {
        let mut out: Vec<Login> = self.logins.values().filter(|l| l.origin == origin).cloned().collect();
        out.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms));
        out
    }

    pub fn save_login(&mut self, key: &Key, login: Login) {
        let record = login.record_key(key);
        if self.replica.put(PASSWORDS, &record, &login.seal(key), now_ms()).map_err(write_failed).is_ok() {
            self.logins.insert(record, login);
        }
    }

    pub fn delete_login(&mut self, record: &str) {
        if self.replica.del(PASSWORDS, record, now_ms()).map_err(write_failed).is_ok() {
            self.logins.remove(record);
        }
    }

    /// Новый ключ синхронизации: перешифровать свои пароли.
    pub fn rekey_logins(&mut self, old: &Key, new: &Key) {
        self.reload_logins(old);
        let logins: Vec<(String, Login)> = std::mem::take(&mut self.logins).into_iter().collect();
        let now = now_ms();
        for (record, login) in logins {
            let _ = self.replica.del(PASSWORDS, &record, now).map_err(write_failed);
            self.save_login(new, login);
        }
    }

    // ── Сессия ──────────────────────────────────────────────────────────

    pub fn save_session(&self, urls: &[String], active: usize) {
        let v = Value::Obj(vec![
            ("tabs".into(), Value::Arr(urls.iter().map(|u| Value::Str(u.clone())).collect())),
            ("active".into(), Value::Num(active.to_string())),
        ]);
        let _ = std::fs::write(self.dir.join("session.json"), v.to_text());
    }

    pub fn load_session(&self) -> Option<(Vec<String>, usize)> {
        let v = json::parse(&std::fs::read_to_string(self.dir.join("session.json")).ok()?).ok()?;
        let tabs: Vec<String> = match v.get("tabs") {
            Some(Value::Arr(items)) => items.iter().filter_map(|i| i.as_str().map(str::to_string)).collect(),
            _ => return None,
        };
        let active = match v.get("active") {
            Some(Value::Num(n)) => n.parse().unwrap_or(0),
            _ => 0,
        };
        (!tabs.is_empty()).then_some((tabs, active))
    }

    /// Вкладки — для других устройств (`devices/<я>/tabs.json`).
    pub fn publish_tabs(&mut self, tabs: &[(String, String)]) {
        let list: Vec<browser_sync::Tab> = tabs
            .iter()
            .enumerate()
            .filter(|(_, (url, _))| url.starts_with("http"))
            .map(|(i, (url, title))| browser_sync::Tab { id: format!("t{i}"), url: url.clone(), title: title.clone() })
            .collect();
        let _ = self.replica.publish_tabs(&list, now_ms());
    }

    pub fn remote_tabs(&self) -> Vec<browser_sync::DeviceTabs> {
        self.replica.remote_tabs().unwrap_or_default().into_iter().filter(|d| d.device != self.settings.device).collect()
    }
}

/// Запись в журнал профиля не удалась — сказать вслух (иначе закладка
/// «добавилась», а в списке её нет, и причину не найти).
fn write_failed(e: std::io::Error) {
    println!("browser: profile write failed: {e}");
}
