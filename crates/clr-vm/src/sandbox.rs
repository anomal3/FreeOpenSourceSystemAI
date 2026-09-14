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

use crate::{FileKind, Host, IoError, WindowEvent, WindowRect};

pub struct Sandbox {
    root: PathBuf,
    /// Всё, что программа напечатала.
    pub output: String,
    /// Точка отсчёта монотонных часов (фаза N5b).
    started: std::time::Instant,
    /// Окна программы — картинки в памяти (фаза N6a).
    windows: Vec<Option<Frame>>,
}

/// Окно песочницы. Событий у него нет: ни мыши, ни человека. Программа, которая
/// сама не закрылась, получает просьбу закрыть окно после [`IDLE_POLLS`]
/// пустых опросов — иначе `Application.Run` крутился бы вечно.
struct Frame {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    idle: u32,
}

/// Сколько пустых опросов событий окно терпит до просьбы закрыться: у формы
/// оборот цикла — 20 мс сна, так что это около двух секунд.
const IDLE_POLLS: u32 = 100;

impl Sandbox {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into(), output: String::new(), started: std::time::Instant::now(), windows: Vec::new() }
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

    fn window_open(&mut self, title: &str, width: u32, height: u32) -> Option<u32> {
        let _ = title;
        let count = usize::try_from(u64::from(width) * u64::from(height)).ok()?;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(count).ok()?;
        pixels.resize(count, 0);
        self.windows.push(Some(Frame { width, height, pixels, idle: 0 }));
        u32::try_from(self.windows.len() - 1).ok()
    }

    fn window_fill(&mut self, window: u32, area: WindowRect, argb: u32) {
        let Some(Some(frame)) = self.windows.get_mut(window as usize) else { return };
        let left = area.x.max(0) as u32;
        let top = area.y.max(0) as u32;
        let right = (i64::from(area.x) + i64::from(area.width)).clamp(0, i64::from(frame.width)) as u32;
        let bottom = (i64::from(area.y) + i64::from(area.height)).clamp(0, i64::from(frame.height)) as u32;
        for y in top..bottom {
            let row = (y * frame.width) as usize;
            for x in left..right {
                frame.pixels[row + x as usize] = argb;
            }
        }
    }

    fn window_event(&mut self, window: u32) -> Option<WindowEvent> {
        let Some(Some(frame)) = self.windows.get_mut(window as usize) else { return None };
        frame.idle += 1;
        (frame.idle > IDLE_POLLS).then_some(WindowEvent::Close)
    }

    fn window_close(&mut self, window: u32) {
        if let Some(slot) = self.windows.get_mut(window as usize) {
            *slot = None;
        }
    }
}
