//! Папка как площадка: флешка, сетевой диск, общий каталог двух профилей
//! (и сквозная проверка синхронизации в системе без сети). Точка
//! синхронизации — копия `devices/` в `points/<имя>/devices/` и `label.txt`.

use std::fs;
use std::path::{Path, PathBuf};
use std::string::{String, ToString};
use std::vec::Vec;

use super::{Error, RemoteFile, Remote, SyncPoint};

pub struct FolderRemote {
    root: PathBuf,
}

fn io(what: &str, e: std::io::Error) -> Error {
    Error::Network(std::format!("{what}: {e}"))
}

impl FolderRemote {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FolderRemote { root: root.into() }
    }

    fn base(&self, at: Option<&str>) -> PathBuf {
        match at {
            Some(point) => self.root.join("points").join(point),
            None => self.root.clone(),
        }
    }

    fn entries(dir: &Path, dirs: bool) -> Result<Vec<(String, String)>, Error> {
        let Ok(read) = fs::read_dir(dir) else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        for entry in read.flatten() {
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() != dirs {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            // Версия файла — размер и время изменения.
            let modified = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis()).unwrap_or(0);
            out.push((name, std::format!("{}:{modified}", meta.len())));
        }
        out.sort();
        Ok(out)
    }
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)?.flatten() {
        let target = to.join(entry.file_name());
        if entry.metadata()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Remote for FolderRemote {
    fn prepare(&mut self) -> Result<(), Error> {
        fs::create_dir_all(self.root.join("devices")).map_err(|e| io("папка devices", e))?;
        fs::create_dir_all(self.root.join("points")).map_err(|e| io("папка points", e))
    }

    fn devices(&mut self, at: Option<&str>) -> Result<Vec<String>, Error> {
        Ok(Self::entries(&self.base(at).join("devices"), true)?.into_iter().map(|(n, _)| n).collect())
    }

    fn files(&mut self, device: &str, at: Option<&str>) -> Result<Vec<RemoteFile>, Error> {
        Ok(Self::entries(&self.base(at).join("devices").join(device), false)?
            .into_iter()
            .map(|(name, version)| RemoteFile { name, version })
            .collect())
    }

    fn read(&mut self, path: &str, at: Option<&str>) -> Result<Option<String>, Error> {
        match fs::read(self.base(at).join(path)) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io("чтение", e)),
        }
    }

    fn write(&mut self, files: &[(String, String)], _message: &str) -> Result<(), Error> {
        for (path, data) in files {
            let target = self.root.join(path);
            if let Some(dir) = target.parent() {
                fs::create_dir_all(dir).map_err(|e| io("папка", e))?;
            }
            let tmp = target.with_extension("tmp");
            fs::write(&tmp, data.as_bytes()).map_err(|e| io("запись", e))?;
            fs::rename(&tmp, &target).map_err(|e| io("запись", e))?;
        }
        Ok(())
    }

    fn create_point(&mut self, name: &str, label: &str) -> Result<(), Error> {
        let dir = self.root.join("points").join(name);
        copy_tree(&self.root.join("devices"), &dir.join("devices")).map_err(|e| io("копия", e))?;
        if !label.is_empty() {
            fs::write(dir.join("label.txt"), label).map_err(|e| io("подпись", e))?;
        }
        Ok(())
    }

    fn points(&mut self) -> Result<Vec<SyncPoint>, Error> {
        let mut points: Vec<SyncPoint> = Self::entries(&self.root.join("points"), true)?
            .into_iter()
            .filter(|(n, _)| n.starts_with("obsos-"))
            .map(|(name, _)| {
                let label = fs::read_to_string(self.root.join("points").join(&name).join("label.txt")).unwrap_or_default();
                SyncPoint { name, label: label.trim().to_string() }
            })
            .collect();
        points.sort_by(|a, b| b.name.cmp(&a.name));
        Ok(points)
    }
}
