//! Хранилище приложения: словарь строк в файле RON, как `FileStorage` у
//! eframe. Каталог на FreeOS — `~/.config/<приложение>` (решение для Craft:
//! настройки программ — в `.config`, как у OBSOS), на прочих Unix — как у
//! eframe (`$XDG_DATA_HOME`/`~/.local/share`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Каталог данных приложения `app_id`.
pub fn storage_dir(app_id: &str) -> Option<PathBuf> {
    let name = app_id.to_lowercase().replace(|c: char| c.is_ascii_whitespace(), "");
    let home = std::env::var_os("HOME").map(PathBuf::from).filter(|p| p.is_absolute())?;
    if cfg!(target_os = "freeos") {
        Some(home.join(".config").join(name))
    } else {
        let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|p| p.is_absolute());
        Some(data.unwrap_or_else(|| home.join(".local").join("share")).join(name))
    }
}

/// Словарь в `<каталог>/app.ron`; на диск — по `flush`.
pub struct FileStorage {
    path: PathBuf,
    kv: HashMap<String, String>,
    dirty: bool,
}

impl FileStorage {
    pub fn from_ron_filepath(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let kv = read_ron(&path).unwrap_or_default();
        Self { path, kv, dirty: false }
    }

    pub fn from_app_id(app_id: &str) -> Option<Self> {
        Some(Self::from_ron_filepath(storage_dir(app_id)?.join("app.ron")))
    }
}

impl crate::Storage for FileStorage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.kv.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        if self.kv.get(key) != Some(&value) {
            self.kv.insert(key.to_owned(), value);
            self.dirty = true;
        }
    }

    fn remove_string(&mut self, key: &str) {
        if self.kv.remove(key).is_some() {
            self.dirty = true;
        }
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match ron::ser::to_string_pretty(&self.kv, ron::ser::PrettyConfig::default()) {
            // Через временный файл и `rename`: оборванная запись не портит
            // прежние настройки.
            Ok(text) => {
                let tmp = self.path.with_extension("ron.tmp");
                if std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &self.path)).is_err() {
                    log::warn!("eframe: failed to save {}", self.path.display());
                }
            }
            Err(err) => log::warn!("eframe: failed to serialize storage: {err}"),
        }
    }
}

fn read_ron(path: &Path) -> Option<HashMap<String, String>> {
    let text = std::fs::read_to_string(path).ok()?;
    ron::from_str(&text).ok()
}
