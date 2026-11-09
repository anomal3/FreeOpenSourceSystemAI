//! Загрузки: файл качается своим HTTPS (`net::download`) в отдельном потоке
//! в `~/Downloads` («Загрузки» в «Файлах»). Servo загружать файлы не умеет —
//! ссылки на архивы, программы, видео и страницы «неизвестный тип»
//! отдаются сюда (`shell.rs`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Running,
    Done(PathBuf),
    Failed(String),
}

#[derive(Clone)]
pub struct Item {
    pub url: String,
    pub name: String,
    pub got: u64,
    pub total: Option<u64>,
    pub state: State,
    pub started: Instant,
    pub cancel: Arc<AtomicBool>,
}

impl Item {
    /// Скорость, байт в секунду.
    pub fn speed(&self) -> f64 {
        self.got as f64 / self.started.elapsed().as_secs_f64().max(0.1)
    }
}

pub struct Downloads {
    pub items: Arc<Mutex<Vec<Item>>>,
    pub dir: PathBuf,
}

impl Downloads {
    pub fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/home"));
        Downloads { items: Arc::new(Mutex::new(Vec::new())), dir: home.join("Downloads") }
    }

    pub fn start(&self, url: &str) {
        let cancel = Arc::new(AtomicBool::new(false));
        let index = {
            let mut items = self.items.lock().expect("downloads");
            items.push(Item {
                url: url.to_string(),
                name: url.rsplit('/').next().unwrap_or(url).to_string(),
                got: 0,
                total: None,
                state: State::Running,
                started: Instant::now(),
                cancel: cancel.clone(),
            });
            items.len() - 1
        };
        println!("browser: download start {url}");
        let items = self.items.clone();
        let dir = self.dir.clone();
        let url = url.to_string();
        let _ = std::thread::Builder::new().name(String::from("obsos-download")).spawn(move || {
            let progress = |got: u64, total: Option<u64>, name: &str| {
                if let Some(item) = items.lock().expect("downloads").get_mut(index) {
                    item.got = got;
                    item.total = total;
                    item.name = name.to_string();
                }
            };
            let result = crate::net::download(&url, &dir, &progress, &cancel);
            let state = match result {
                Ok(path) => {
                    println!("browser: download done {}", path.display());
                    State::Done(path)
                }
                Err(e) => {
                    println!("browser: download failed: {e}");
                    State::Failed(e)
                }
            };
            if let Some(item) = items.lock().expect("downloads").get_mut(index) {
                item.state = state;
            }
        });
    }

    pub fn snapshot(&self) -> Vec<Item> {
        self.items.lock().map(|i| i.clone()).unwrap_or_default()
    }

    pub fn cancel(&self, index: usize) {
        if let Some(item) = self.items.lock().expect("downloads").get(index) {
            item.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Убрать завершённые из списка (файлы остаются).
    pub fn clear_finished(&self) {
        if let Ok(mut items) = self.items.lock() {
            // Поток загрузки помнит свой номер в списке — чистим, только
            // когда ни один не бежит, иначе номера съедут.
            if items.iter().all(|d| d.state != State::Running) {
                items.clear();
            }
        }
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
