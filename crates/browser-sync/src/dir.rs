// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! `DirStore` — хранилище синхронизации в каталоге файловой системы.
//!
//! Годится как есть для папки, которую синхронизирует кто-то другой
//! (флешка, сетевой диск, WebDAV, смонтированный в каталог), и как локальная
//! копия репозитория для сетевых хранилищ: прочитать всё с сервера в каталог,
//! слить, отправить изменённые файлы своего устройства.
//!
//! Запись файла целиком — через временный файл и `rename`, чтобы оборванная
//! запись снимка не оставила полфайла. Дозапись журнала — обычный `append`:
//! оборванная строка журнала при чтении пропускается (`op::parse_log`).

use alloc::string::String;
use alloc::vec::Vec;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::replica::Store;

#[derive(Clone, Debug)]
pub struct DirStore {
    root: PathBuf,
}

impl DirStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        DirStore { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Путь внутри корня. Пути строит `replica` из проверенных имён, но
    /// типаж открытый — `..`, пустые и абсолютные части не пропускаются.
    fn full(&self, path: &str) -> io::Result<PathBuf> {
        let mut p = self.root.clone();
        for part in path.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') || part.contains(':') {
                return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad sync path"));
            }
            p.push(part);
        }
        Ok(p)
    }
}

impl Store for DirStore {
    type Error = io::Error;

    fn devices(&self) -> io::Result<Vec<String>> {
        let mut out = Vec::new();
        let dir = match fs::read_dir(self.root.join("devices")) {
            Ok(d) => d,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        for entry in dir {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Ok(name) = entry.file_name().into_string() {
                    out.push(name);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn read(&self, path: &str) -> io::Result<Option<String>> {
        match fs::read(self.full(path)?) {
            // Не UTF-8 — не наш файл; как испорченный журнал, а не ошибка
            // ввода-вывода, которая остановила бы синхронизацию.
            Ok(bytes) => Ok(Some(String::from_utf8(bytes).unwrap_or_default())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write(&mut self, path: &str, data: &str) -> io::Result<()> {
        let full = self.full(path)?;
        if let Some(dir) = full.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = full.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(data.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &full)
    }

    fn append(&mut self, path: &str, data: &str) -> io::Result<()> {
        let full = self.full(path)?;
        if let Some(dir) = full.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(full)?;
        f.write_all(data.as_bytes())?;
        f.sync_data()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::Replica;
    use alloc::format;
    use alloc::vec;

    fn temp_root(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("browser-sync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn two_devices_share_a_folder() {
        let root = temp_root("share");
        let mut pc = Replica::new(DirStore::new(&root), "pc");
        pc.put("bookmarks", "https://example.com/", "Example", 1_000).unwrap();
        pc.put("bookmarks", "https://servo.org/", "Servo", 1_001).unwrap();
        let mut phone = Replica::new(DirStore::new(&root), "phone");
        phone.del("bookmarks", "https://example.com/", 2_000).unwrap();
        phone.compact("bookmarks", 3_000).unwrap();
        let mut pc = Replica::new(DirStore::new(&root), "pc");
        let v = pc.load("bookmarks").unwrap();
        let live: Vec<_> = v.state.live().collect();
        assert_eq!(live, vec![("https://servo.org/", "Servo")]);
        assert_eq!(pc.store().devices().unwrap(), vec![String::from("pc"), String::from("phone")]);
        assert!(root.join("devices/phone/bookmarks.snapshot").is_file());
        assert!(!root.join("devices/phone/bookmarks.snapshot.tmp").exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rejects_escaping_paths() {
        let root = temp_root("escape");
        let mut s = DirStore::new(&root);
        for bad in ["../x", "devices/../../x", "/abs", "a//b", "c:\\x", "a\\..\\b"] {
            assert!(s.write(bad, "x").is_err(), "{bad}");
            assert!(s.read(bad).is_err(), "{bad}");
        }
        assert_eq!(s.read("devices/none/x.log").unwrap(), None);
        assert!(s.devices().unwrap().is_empty());
        let _ = fs::remove_dir_all(&root);
    }
}
