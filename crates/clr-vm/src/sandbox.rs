//! Хост с настоящими файлами в каталоге-песочнице (фаза N5a) — для
//! `cargo xtask clr-check` и тестов `clr-vm` на машине разработчика.
//!
//! Полный путь программы `/a/b` становится `<корень>/a/b`: программа видит
//! песочницу корнем и текущим каталогом. Так и вывод, и дерево файлов, которое
//! программа оставила, сравниваются с тем, что сделал настоящий `dotnet`,
//! запущенный в таком же пустом каталоге.

use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::PathBuf;
use std::string::String;
use std::vec::Vec;

use crate::{FileKind, Host, IoError};

pub struct Sandbox {
    root: PathBuf,
    /// Всё, что программа напечатала.
    pub output: String,
    /// Точка отсчёта монотонных часов (фаза N5b).
    started: std::time::Instant,
}

impl Sandbox {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), output: String::new(), started: std::time::Instant::now() }
    }

    fn place(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }
}

fn error(failure: &std::io::Error) -> IoError {
    match failure.kind() {
        ErrorKind::NotFound => IoError::NotFound,
        ErrorKind::AlreadyExists => IoError::Exists,
        ErrorKind::DirectoryNotEmpty => IoError::NotEmpty,
        ErrorKind::PermissionDenied => IoError::Denied,
        ErrorKind::StorageFull => IoError::NoSpace,
        ErrorKind::IsADirectory | ErrorKind::NotADirectory => IoError::WrongKind,
        _ => IoError::Other,
    }
}

impl Host for Sandbox {
    fn write_out(&mut self, text: &str) {
        self.output.push_str(text);
    }

    fn read_file(&mut self, path: &str) -> Result<Vec<u8>, IoError> {
        let place = self.place(path);
        if place.is_dir() {
            return Err(IoError::WrongKind);
        }
        fs::read(place).map_err(|failure| error(&failure))
    }

    fn write_file(&mut self, path: &str, data: &[u8], append: bool) -> Result<(), IoError> {
        let place = self.place(path);
        if place.is_dir() {
            return Err(IoError::WrongKind);
        }
        let mut options = fs::OpenOptions::new();
        options.create(true);
        if append {
            options.append(true);
        } else {
            options.write(true).truncate(true);
        }
        let mut file = options.open(place).map_err(|failure| error(&failure))?;
        file.write_all(data).map_err(|failure| error(&failure))
    }

    fn remove_file(&mut self, path: &str) -> Result<(), IoError> {
        let place = self.place(path);
        if place.is_dir() {
            return Err(IoError::WrongKind);
        }
        fs::remove_file(place).map_err(|failure| error(&failure))
    }

    fn create_dir(&mut self, path: &str) -> Result<(), IoError> {
        fs::create_dir(self.place(path)).map_err(|failure| error(&failure))
    }

    fn remove_dir(&mut self, path: &str) -> Result<(), IoError> {
        let place = self.place(path);
        if place.is_file() {
            return Err(IoError::WrongKind);
        }
        fs::remove_dir(place).map_err(|failure| error(&failure))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), IoError> {
        fs::rename(self.place(from), self.place(to)).map_err(|failure| error(&failure))
    }

    fn stat(&mut self, path: &str) -> Result<(FileKind, u64), IoError> {
        let metadata = fs::metadata(self.place(path)).map_err(|failure| error(&failure))?;
        Ok((if metadata.is_dir() { FileKind::Directory } else { FileKind::File }, metadata.len()))
    }

    fn list_dir(&mut self, path: &str) -> Result<Vec<String>, IoError> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.place(path)).map_err(|failure| error(&failure))? {
            let entry = entry.map_err(|failure| error(&failure))?;
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn utc_now(&mut self) -> i64 {
        let since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        (since.as_nanos() / 100) as i64
    }

    // Часового пояса у std нет, и песочница живёт по UTC. Образцы печатают про
    // местное время только проверки, верные при любом смещении.

    fn monotonic_nanos(&mut self) -> u64 {
        self.started.elapsed().as_nanos() as u64
    }

    fn sleep(&mut self, milliseconds: u32) {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(milliseconds)));
    }

    fn processor_count(&mut self) -> u32 {
        std::thread::available_parallelism().map_or(1, |count| count.get() as u32)
    }
}
