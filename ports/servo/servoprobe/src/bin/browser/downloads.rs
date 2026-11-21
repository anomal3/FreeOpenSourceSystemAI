//! Загрузки: файл качается своим HTTPS (`net::open` + `Opened::save`) в
//! отдельном потоке прямо на диск (`<имя>.part`, готовое переименовывается) —
//! в память не ложится, гигабайтные образы качаются так же, как картинки.
//! Servo загружать файлы не умеет, поэтому сюда приходят:
//! * ссылки на архивы, программы, образы (`looks_like_file`, по расширению);
//! * переходы, ответ на которые сервер велит сохранить (`Content-Disposition:
//!   attachment`) или который Servo не покажет (zip, exe, iso, pdf, видео…), —
//!   их узнаёт проверка ссылки [`Downloads::probe`]: запрос документа вкладки
//!   ждёт, пока наш GET прочтёт заголовки; файл — это соединение и становится
//!   загрузкой, страница — соединение бросается, документ грузит Servo;
//! * `<a download>` и пункты меню «Сохранить ссылку как…», «Сохранить
//!   картинку как…».
//!
//! Куда: `~/Downloads` (на живой флешке — `/root/Downloads` в памяти) или
//! системное окно «Сохранить как» (`/bin/filedlg`) — по настройке «Спрашивать,
//! куда сохранять» и всегда для «Сохранить … как…».

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Проверка ссылки вкладки: файл или страница (в списке не видна).
    Probing,
    /// Проверка решила: это страница — пусть грузит Servo.
    Page,
    /// Ждём ответа окна «Сохранить как».
    Asking,
    Running,
    Done(PathBuf),
    Failed(String),
    Cancelled,
}

#[derive(Clone)]
pub struct Item {
    pub id: u64,
    pub url: String,
    pub name: String,
    pub got: u64,
    pub total: Option<u64>,
    pub state: State,
    pub started: Instant,
    pub cancel: Arc<AtomicBool>,
    /// Скорость, байт в секунду (сглаженная).
    pub rate: f64,
    rate_at: (Instant, u64),
    /// Куда пишется (известно с начала записи).
    pub path: Option<PathBuf>,
    /// Ответ окна «Сохранить как» потоку загрузки.
    answer: Option<mpsc::Sender<Option<PathBuf>>>,
    /// Главный поток уже узнал о конце (уведомление показано).
    pub reported: bool,
    /// SHA-256 готового файла (hex).
    pub sha256: Option<String>,
}

impl Item {
    pub fn visible(&self) -> bool {
        !matches!(self.state, State::Probing | State::Page)
    }

    pub fn active(&self) -> bool {
        matches!(self.state, State::Probing | State::Asking | State::Running)
    }

    pub fn fraction(&self) -> Option<f32> {
        self.total.filter(|t| *t > 0).map(|t| (self.got as f64 / t as f64).clamp(0.0, 1.0) as f32)
    }

    /// Осталось секунд (если известны размер и скорость).
    pub fn eta(&self) -> Option<u64> {
        let total = self.total?;
        (self.rate > 1.0).then(|| (total.saturating_sub(self.got) as f64 / self.rate) as u64)
    }
}

/// Что сказать главному циклу (`Downloads::poll`); решения проверок ссылок
/// он спрашивает сам (`probe_is_file`).
pub enum Event {
    /// Поток ждёт, куда сохранять: открыть окно «Сохранить как».
    Ask { id: u64, name: String },
    Finished { name: String, ok: bool },
}

/// Запрос на загрузку.
#[derive(Clone, Default)]
pub struct Request {
    pub url: String,
    pub cookie: Option<String>,
    pub referer: Option<String>,
    /// Спросить, куда сохранять (иначе — по настройке).
    pub ask: bool,
    /// Имя, если известно заранее (картинка: имя из адреса).
    pub name: Option<String>,
}

pub struct Downloads {
    pub items: Arc<Mutex<Vec<Item>>>,
    pub dir: std::cell::RefCell<PathBuf>,
    /// Спрашивать, куда сохранять, каждый раз (настройка).
    pub ask_always: std::cell::Cell<bool>,
    next: AtomicU64,
}

pub fn default_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/home"));
    home.join("Downloads")
}

impl Downloads {
    pub fn new() -> Self {
        Downloads {
            items: Arc::new(Mutex::new(Vec::new())),
            dir: std::cell::RefCell::new(default_dir()),
            ask_always: std::cell::Cell::new(false),
            next: AtomicU64::new(1),
        }
    }

    fn add(&self, request: &Request, state: State) -> (u64, Arc<AtomicBool>, mpsc::Receiver<Option<PathBuf>>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let name = request.name.clone().unwrap_or_else(|| crate::net::name_from_url(&request.url));
        self.items.lock().expect("downloads").push(Item {
            id,
            url: request.url.clone(),
            name,
            got: 0,
            total: None,
            state,
            started: Instant::now(),
            cancel: cancel.clone(),
            rate: 0.0,
            rate_at: (Instant::now(), 0),
            path: None,
            answer: Some(tx),
            reported: false,
            sha256: None,
        });
        (id, cancel, rx)
    }

    /// Скачать адрес. Возвращает номер загрузки.
    pub fn start(&self, request: Request) -> u64 {
        let ask = request.ask || self.ask_always.get();
        let (id, cancel, rx) = self.add(&request, State::Running);
        println!("browser: download start {}", request.url);
        let items = self.items.clone();
        let dir = self.dir.borrow().clone();
        let _ = std::thread::Builder::new().name(String::from("obsos-download")).spawn(move || {
            let opened = crate::net::open(&request.url, request.cookie.as_deref(), request.referer.as_deref());
            match opened {
                Ok(o) if (200..300).contains(&o.status) => run(items, id, o, request.name, ask, dir, rx, cancel),
                Ok(o) => finish(&items, id, State::Failed(format!("сервер ответил {}", o.status))),
                Err(e) => finish(&items, id, State::Failed(e)),
            }
        });
        id
    }

    /// Проверка ссылки вкладки: GET до заголовков; файл — качать дальше по
    /// тому же соединению, страница — бросить (решение — `Event::Probed`).
    pub fn probe(&self, url: &str, cookie: Option<String>, referer: Option<String>) -> u64 {
        let request = Request { url: url.to_string(), cookie, referer, ask: false, name: None };
        let ask = self.ask_always.get();
        let (id, cancel, rx) = self.add(&request, State::Probing);
        let items = self.items.clone();
        let dir = self.dir.borrow().clone();
        let _ = std::thread::Builder::new().name(String::from("obsos-probe")).spawn(move || {
            let opened = crate::net::open(&request.url, request.cookie.as_deref(), request.referer.as_deref());
            let page = |items: &Arc<Mutex<Vec<Item>>>| set_state(items, id, State::Page);
            let o = match opened {
                Ok(o) => o,
                Err(e) => {
                    println!("browser: probe {}: {e} — page", request.url);
                    return page(&items);
                }
            };
            // Главный цикл уже отпустил документ (долго ждали) — не мешаем.
            if cancel.load(Ordering::Relaxed) {
                return page(&items);
            }
            if !(200..300).contains(&o.status) || !is_download(&o) {
                return page(&items);
            }
            println!("browser: probe {} → file {} ({}, {})", request.url, o.file_name(), o.mime(), if o.attachment() { "attachment" } else { "inline" });
            set_state(&items, id, State::Running);
            run(items, id, o, None, ask, dir, rx, cancel);
        });
        id
    }

    /// Ответ окна «Сохранить как» (`None` — отмена).
    pub fn answer(&self, id: u64, path: Option<PathBuf>) {
        if let Some(item) = self.items.lock().expect("downloads").iter().find(|i| i.id == id) {
            if let Some(tx) = &item.answer {
                let _ = tx.send(path);
            }
        }
    }

    /// События для главного цикла: решения проверок, просьбы спросить путь,
    /// концы загрузок (по разу на каждое).
    pub fn poll(&self, asked: &mut std::collections::BTreeSet<u64>) -> Vec<Event> {
        let mut out = Vec::new();
        let Ok(mut items) = self.items.lock() else { return out };
        for item in items.iter_mut() {
            match &item.state {
                State::Asking if !asked.contains(&item.id) => {
                    asked.insert(item.id);
                    out.push(Event::Ask { id: item.id, name: item.name.clone() });
                }
                State::Done(_) | State::Failed(_) | State::Cancelled if !item.reported => {
                    item.reported = true;
                    out.push(Event::Finished { name: item.name.clone(), ok: matches!(item.state, State::Done(_)) });
                }
                _ => {}
            }
        }
        // Проверки, решившие «страница», — убрать (документ отпускает
        // `Shell::poll_downloads`, `probe_is_file` для них — «не файл»).
        items.retain(|i| i.state != State::Page);
        out
    }

    /// Номера проверок, решивших «файл» (их документ отменяется).
    pub fn probe_is_file(&self, id: u64) -> Option<bool> {
        let items = self.items.lock().ok()?;
        match items.iter().find(|i| i.id == id).map(|i| &i.state) {
            None | Some(State::Page) => Some(false),
            Some(State::Probing) => None,
            Some(_) => Some(true),
        }
    }

    /// Бросить проверку: документ отпущен к Servo.
    pub fn drop_probe(&self, id: u64) {
        if let Ok(mut items) = self.items.lock() {
            if let Some(at) = items.iter().position(|i| i.id == id && i.state == State::Probing) {
                items[at].cancel.store(true, Ordering::Relaxed);
                items.remove(at);
            }
        }
    }

    pub fn snapshot(&self) -> Vec<Item> {
        self.items.lock().map(|i| i.iter().filter(|d| d.visible()).cloned().collect()).unwrap_or_default()
    }

    pub fn cancel(&self, id: u64) {
        if let Some(item) = self.items.lock().expect("downloads").iter().find(|i| i.id == id) {
            item.cancel.store(true, Ordering::Relaxed);
            if let Some(tx) = &item.answer {
                let _ = tx.send(None);
            }
        }
    }

    /// Убрать из списка (файл остаётся). Идущую — только отменив.
    pub fn remove(&self, id: u64) {
        if let Ok(mut items) = self.items.lock() {
            items.retain(|d| d.id != id || d.active());
        }
    }

    /// Убрать завершённые из списка (файлы остаются).
    pub fn clear_finished(&self) {
        if let Ok(mut items) = self.items.lock() {
            items.retain(|d| d.active());
        }
    }
}

/// Что Servo показать не может (или что сервер велел сохранить) — качать.
pub fn is_download(o: &crate::net::Opened) -> bool {
    if o.attachment() {
        return true;
    }
    !renderable_mime(&o.mime())
}

/// Типы, которые Servo показывает сам. Пустой тип — Servo угадывает по
/// содержимому, пусть грузит.
pub fn renderable_mime(mime: &str) -> bool {
    if mime.is_empty() {
        return true;
    }
    if mime.starts_with("text/") && mime != "text/csv" && mime != "text/calendar" && mime != "text/vcard" {
        return true;
    }
    if mime.starts_with("image/") {
        return matches!(
            mime,
            "image/png" | "image/jpeg" | "image/jpg" | "image/gif" | "image/webp" | "image/bmp" | "image/x-icon" | "image/vnd.microsoft.icon" | "image/svg+xml" | "image/avif"
        );
    }
    matches!(
        mime,
        "application/xhtml+xml"
            | "application/xml"
            | "application/json"
            | "application/javascript"
            | "application/ecmascript"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/manifest+json"
            | "application/ld+json"
            | "multipart/x-mixed-replace"
    )
}

fn set_state(items: &Arc<Mutex<Vec<Item>>>, id: u64, state: State) {
    if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
        item.state = state;
    }
}

fn finish(items: &Arc<Mutex<Vec<Item>>>, id: u64, state: State) {
    match &state {
        State::Done(path) => println!("browser: download done {}", path.display()),
        State::Failed(e) => println!("browser: download failed: {e}"),
        State::Cancelled => println!("browser: download cancelled"),
        _ => {}
    }
    if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
        item.state = state;
        item.answer = None;
    }
}

/// Поток загрузки после заголовков: куда писать (окно «Сохранить как» или
/// папка загрузок), запись потоком, переименование `.part` → имя.
#[allow(clippy::too_many_arguments)]
fn run(
    items: Arc<Mutex<Vec<Item>>>,
    id: u64,
    opened: crate::net::Opened,
    name: Option<String>,
    ask: bool,
    dir: PathBuf,
    answers: mpsc::Receiver<Option<PathBuf>>,
    cancel: Arc<AtomicBool>,
) {
    let name = name.and_then(|n| crate::net::clean_name(&n)).unwrap_or_else(|| opened.file_name());
    let total = opened.total();
    if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
        item.name = name.clone();
        item.total = total;
    }
    let path = if ask {
        set_state(&items, id, State::Asking);
        // Ответ окна; отмена загрузки во время вопроса тоже будит.
        let chosen = loop {
            match answers.recv_timeout(Duration::from_millis(250)) {
                Ok(answer) => break answer,
                Err(mpsc::RecvTimeoutError::Timeout) if !cancel.load(Ordering::Relaxed) => continue,
                Err(_) => break None,
            }
        };
        match chosen {
            Some(path) => path,
            None => return finish(&items, id, State::Cancelled),
        }
    } else {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return finish(&items, id, State::Failed(format!("{}: {e}", dir.display())));
        }
        crate::net::unique_path(&dir, &name)
    };
    let shown = path.file_name().and_then(|n| n.to_str()).unwrap_or("файл").to_string();
    let part = path.with_file_name(format!("{shown}.part"));
    if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
        item.name = shown.clone();
        item.path = Some(path.clone());
        item.state = State::Running;
        item.started = Instant::now();
        item.rate_at = (Instant::now(), 0);
    }
    println!("browser: download to {} ({})", path.display(), total.map(|t| format!("{t} bytes")).unwrap_or_else(|| String::from("size unknown")));
    let progress = |got: u64, total: Option<u64>| {
        if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
            let (at, before) = item.rate_at;
            let dt = at.elapsed().as_secs_f64();
            if dt >= 0.2 {
                let now = (got.saturating_sub(before)) as f64 / dt;
                item.rate = if item.rate == 0.0 { now } else { item.rate * 0.6 + now * 0.4 };
                item.rate_at = (Instant::now(), got);
            }
            item.got = got;
            item.total = total;
        }
    };
    let last_log = std::cell::Cell::new(0u64);
    let logged = |got: u64, total: Option<u64>| {
        progress(got, total);
        // Ход в журнал — по каждой сотне МиБ (стенд и журнал Романа видят,
        // что гигабайт идёт, а не стоит).
        let step = got / (100 * 1024 * 1024);
        if step > 0 && step != last_log.replace(step) {
            println!("browser: download {shown} {} MiB", got / (1024 * 1024));
        }
    };
    match opened.save(&part, &logged, &cancel) {
        Ok((bytes, sha)) => match std::fs::rename(&part, &path) {
            Ok(()) => {
                println!("browser: download saved {} ({bytes} bytes, sha256 {sha})", path.display());
                if let Some(item) = items.lock().expect("downloads").iter_mut().find(|i| i.id == id) {
                    item.sha256 = Some(sha);
                }
                finish(&items, id, State::Done(path))
            }
            Err(e) => finish(&items, id, State::Failed(format!("переименование: {e}"))),
        },
        Err(_) if cancel.load(Ordering::Relaxed) => finish(&items, id, State::Cancelled),
        Err(e) => finish(&items, id, State::Failed(e)),
    }
}

/// Похоже ли на файл для скачивания, а не на страницу (по расширению).
pub fn looks_like_file(url: &url::Url) -> bool {
    const EXT: &[&str] = &[
        "zip", "7z", "rar", "tar", "gz", "tgz", "xz", "bz2", "zst", "iso", "img", "exe", "msi", "deb", "rpm", "apk",
        "fpk", "appimage", "dmg", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
        "epub", "djvu", "mp3", "flac", "ogg", "opus", "wav", "m4a", "mp4", "mkv", "webm", "avi", "mov", "torrent",
        "bin", "jar", "whl", "nupkg", "ttf", "otf",
    ];
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    last.rsplit_once('.').is_some_and(|(_, ext)| EXT.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Адрес, по которому явно страница (проверять заголовки незачем).
pub fn surely_page(url: &url::Url) -> bool {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    if last.is_empty() && url.query().is_none() {
        return true;
    }
    last.rsplit_once('.').is_some_and(|(_, ext)| {
        matches!(ext.to_ascii_lowercase().as_str(), "html" | "htm" | "xhtml" | "shtml" | "css" | "js" | "json" | "svg" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "txt" | "md" | "xml")
    })
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["Б", "КБ", "МБ", "ГБ", "ТБ"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} {}", UNITS[0]) } else { format!("{value:.1} {}", UNITS[unit]) }
}

pub fn human_time(secs: u64) -> String {
    if secs < 60 {
        format!("{secs} с")
    } else if secs < 3600 {
        format!("{} мин {} с", secs / 60, secs % 60)
    } else {
        format!("{} ч {} мин", secs / 3600, secs % 3600 / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_rules() {
        assert!(renderable_mime("text/html"));
        assert!(renderable_mime(""));
        assert!(renderable_mime("image/png"));
        assert!(!renderable_mime("application/zip"));
        assert!(!renderable_mime("application/octet-stream"));
        assert!(!renderable_mime("application/pdf"));
        assert!(!renderable_mime("video/mp4"));
        assert!(!renderable_mime("image/x-photoshop"));
        let u = url::Url::parse("https://x.org/").unwrap();
        assert!(surely_page(&u));
        assert!(!surely_page(&url::Url::parse("https://x.org/get?id=1").unwrap()));
        assert!(!surely_page(&url::Url::parse("https://x.org/download.php").unwrap()));
    }
}
