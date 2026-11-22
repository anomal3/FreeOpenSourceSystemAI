//! Окно «Открыть» / «Сохранить как» / «Выбор папки» для программ (20.11):
//! системная сторона.
//!
//! Само окно — `mini_ui::filedlg` (рисует и разбирает ввод, системных вызовов
//! не знает). Здесь [`SysHost`] — всё, что окну нужно от системы: папки, тома,
//! создание папки, значки макета, дата по часовому поясу, память о последней
//! папке (`~/.config/filedlg.cfg`).
//!
//! # Как программе спросить файл
//!
//! Окно — отдельная программа `/bin/filedlg` со своим окном стола: заголовок,
//! перетаскивание, края, модальность к окну программы (стол не отдаёт
//! владельцу ввод, ставит окно посередине него, кнопка на панели задач — одна).
//! Роман (20.11): окно, нарисованное внутри Блокнота, выглядело наложением.
//! Программе — [`Dialog`]:
//!
//! ```ignore
//! let options = Options::save("Без имени.txt").filters(vec![Filter::new("Текстовые файлы", &["txt"]), Filter::all()]);
//! self.dialog = Dialog::start(&options, "notepad").ok();   // окно открылось над нашим
//! // В App::step (каждые ~30 мс):
//! if let Some(outcome) = self.dialog.as_mut().and_then(Dialog::poll) { self.dialog = None; /* Chosen(path) | Cancelled */ }
//! ```
//!
//! Договор с `/bin/filedlg`: запрос — строки `ключ\tзначение` в стандартный
//! ввод ([`request_text`], [`parse_request`]), ответ — строка
//! [`ANSWER`]`ok\t<путь>` или [`ANSWER`]`cancel` в стандартный вывод; прочие
//! строки вывода — журнал окна, [`Dialog`] печатает их от себя.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use mini_ui::filedlg::{COMPUTER, FileDialog, Filter, Host, Item, Mode, Options, Outcome, Volume, join, parent_of};
use mini_ui::filedlg as dlg;
use mini_ui::{Rect, Surface};
use user_abi::{Dirent, KIND_DIRECTORY, Stat};

use crate::{close, monotonic_ms, mounts, open, open_write, println, read, readdir_raw, stat, write};

// Коды клавиш окна повторены в `mini_ui::filedlg` (тот крейт не знает
// договора); расходиться им нельзя.
const _: () = {
    assert!(dlg::KEY_LEFT == user_abi::WIN_KEY_LEFT);
    assert!(dlg::KEY_RIGHT == user_abi::WIN_KEY_RIGHT);
    assert!(dlg::KEY_UP == user_abi::WIN_KEY_UP);
    assert!(dlg::KEY_DOWN == user_abi::WIN_KEY_DOWN);
    assert!(dlg::KEY_HOME == user_abi::WIN_KEY_HOME);
    assert!(dlg::KEY_END == user_abi::WIN_KEY_END);
    assert!(dlg::KEY_PAGE_UP == user_abi::WIN_KEY_PAGE_UP);
    assert!(dlg::KEY_PAGE_DOWN == user_abi::WIN_KEY_PAGE_DOWN);
    assert!(dlg::KEY_DELETE == user_abi::WIN_KEY_DELETE);
    assert!(dlg::KEY_F1 == user_abi::WIN_KEY_F1);
    assert!(dlg::MOD_SHIFT == user_abi::WIN_MOD_SHIFT);
    assert!(dlg::MOD_CTRL == user_abi::WIN_MOD_CTRL);
    assert!(dlg::MOD_ALT == user_abi::WIN_MOD_ALT);
};

/// Самое большее записей одной папки.
const MAX_ENTRIES: usize = 4096;
/// Буфер ответа `mounts`.
const MOUNTS_LIMIT: usize = 2048;
/// Где помнится последняя папка (в доме).
const STATE_FILE: &str = ".config/filedlg.cfg";

/// Система для окна выбора файла.
pub struct SysHost {
    /// Как программа подписывает строки журнала: `notepad`.
    tag: &'static str,
    home: String,
    /// Смещение местного времени, минуты.
    offset: i32,
}

impl SysHost {
    /// `tag` — имя программы в журнале.
    #[must_use]
    pub fn new(tag: &'static str) -> Self {
        Self { tag, home: home_dir(), offset: local_offset() }
    }
}

impl Host for SysHost {
    fn list(&self, dir: &str) -> Result<Vec<Item>, String> {
        let fd = open(dir);
        if fd < 0 {
            return Err(error_text(fd));
        }
        let mut out = Vec::new();
        let mut entry = Dirent::default();
        while readdir_raw(fd, &mut entry) > 0 {
            let len = (entry.name_len as usize).min(entry.name.len());
            let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            out.push(Item { name: name.to_string(), dir: entry.kind == KIND_DIRECTORY, size: entry.size, mtime: entry.mtime });
            if out.len() >= MAX_ENTRIES {
                break;
            }
        }
        close(fd);
        Ok(out)
    }

    fn stat(&self, path: &str) -> Option<bool> {
        let mut info = Stat::default();
        (stat(path, &mut info) >= 0).then_some(info.kind == KIND_DIRECTORY)
    }

    fn make_dir(&self, path: &str) -> Result<(), String> {
        let code = crate::mkdir(path, 0o755);
        if code < 0 { Err(error_text(code)) } else { Ok(()) }
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), String> {
        let code = crate::rename(from, to);
        if code < 0 { Err(error_text(code)) } else { Ok(()) }
    }

    fn home(&self) -> String {
        self.home.clone()
    }

    fn now_ms(&self) -> u64 {
        monotonic_ms()
    }

    fn volumes(&self) -> Vec<Volume> {
        let mut buffer = Vec::new();
        if buffer.try_reserve_exact(MOUNTS_LIMIT).is_err() {
            return Vec::new();
        }
        buffer.resize(MOUNTS_LIMIT, 0u8);
        let got = mounts(&mut buffer);
        let text = if got > 0 { core::str::from_utf8(&buffer[..(got as usize).min(buffer.len())]).unwrap_or("") } else { "" };
        let mut out: Vec<Volume> = Vec::new();
        for line in text.lines() {
            // `<точка>\t<ФС>[\t<всего>\t<свободно>]`.
            let mut fields = line.split('\t');
            let (Some(point), Some(fs)) = (fields.next(), fields.next()) else {
                continue;
            };
            let total = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            let free = fields.next().and_then(|value| value.parse().ok()).unwrap_or(0);
            // Служебные файловые системы — не диски.
            if matches!(fs, "proc" | "sysfs" | "devfs" | "devtmpfs" | "tmpfs" | "devpts" | "pipefs")
                || ["/proc", "/sys", "/dev", "/run", "/tmp"].iter().any(|skip| point == *skip || point.starts_with(&format!("{skip}/")))
                || out.iter().any(|known| known.path == point)
            {
                continue;
            }
            let removable = point.starts_with("/usb") || point.starts_with("/media");
            let title = match point {
                "/" => String::from("Системный диск"),
                "/data" => String::from("Данные (/data)"),
                _ if removable => format!("USB-накопитель ({point})"),
                _ => format!("Диск ({point})"),
            };
            let icon = if point == "/" {
                "place-system-disk"
            } else if removable {
                "place-usb-stick"
            } else {
                "place-local-disk"
            };
            out.push(Volume { path: point.to_string(), title, icon, total, free });
        }
        if !out.iter().any(|volume| volume.path == "/") {
            out.insert(0, Volume { path: String::from("/"), title: String::from("Системный диск"), icon: "place-system-disk", total: 0, free: 0 });
        }
        // Корень — первым, остальные по алфавиту.
        out.sort_by(|a, b| (a.path != "/").cmp(&(b.path != "/")).then_with(|| a.path.cmp(&b.path)));
        out
    }

    fn kind(&self, name: &str, dir: bool) -> (&'static str, &'static str) {
        let kind = sysconf::filetypes::kind_of(name, dir);
        (kind.icon, kind.name)
    }

    fn date(&self, mtime: u32) -> String {
        if mtime == 0 {
            return String::new();
        }
        let local = calendar::DateTime::from_unix(i64::from(mtime) + i64::from(self.offset) * 60);
        format!("{:02}.{:02}.{} {:02}:{:02}", local.day, local.month, local.year, local.hour, local.minute)
    }

    fn draw_icon(&self, s: &mut Surface, rect: Rect, name: &str) -> bool {
        // Значки темы (22.11, `crate::icons`); нет такого — та же папка или
        // «неизвестный документ».
        let fallback = if name.starts_with("folder") { "folder" } else { "doc-unknown" };
        crate::icons::draw(s, rect, name) || crate::icons::draw(s, rect, fallback)
    }

    fn last_dir(&self) -> Option<String> {
        let text = read_small(&dlg::join(&self.home, STATE_FILE))?;
        text.lines().find_map(|line| line.strip_prefix("last_dir=")).map(|dir| dir.trim().to_string()).filter(|dir| !dir.is_empty())
    }

    fn remember_dir(&self, dir: &str) {
        let config = dlg::join(&self.home, ".config");
        if self.stat(&config).is_none() {
            let _ = crate::mkdir(&config, 0o755);
        }
        let path = dlg::join(&self.home, STATE_FILE);
        let fd = open_write(&path, true, true);
        if fd >= 0 {
            let line = format!("last_dir={dir}\n");
            let _ = write(fd, line.as_bytes());
            close(fd);
        }
    }

    fn log(&self, line: &str) {
        println(&format!("{}: {line}", self.tag));
    }
}

/// Домашняя папка: `/root` у нулевого uid, иначе своя в `/home`.
#[must_use]
pub fn home_dir() -> String {
    let me = crate::uid();
    if me == 0 {
        return String::from("/root");
    }
    let fd = open("/home");
    if fd >= 0 {
        let mut entry = Dirent::default();
        while readdir_raw(fd, &mut entry) > 0 {
            let len = (entry.name_len as usize).min(entry.name.len());
            let Ok(name) = core::str::from_utf8(&entry.name[..len]) else {
                continue;
            };
            if name.starts_with('.') || entry.kind != KIND_DIRECTORY {
                continue;
            }
            let path = format!("/home/{name}");
            let mut info = Stat::default();
            if stat(&path, &mut info) >= 0 && info.uid == me {
                close(fd);
                return path;
            }
        }
        close(fd);
    }
    String::from("/home")
}

/// Смещение местного времени, минуты (из `/etc/system.cfg`, как у ядра).
fn local_offset() -> i32 {
    let Some(text) = read_small("/etc/system.cfg") else {
        return 0;
    };
    sysconf::value(&text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

/// Прочитать небольшой текстовый файл (до 4 КБ).
fn read_small(path: &str) -> Option<String> {
    let fd = open(path);
    if fd < 0 {
        return None;
    }
    let mut buffer = [0u8; 4096];
    let got = read(fd, &mut buffer);
    close(fd);
    if got <= 0 {
        return None;
    }
    core::str::from_utf8(&buffer[..got as usize]).ok().map(ToString::to_string)
}

// ---------------------------------------------------------------------------
// Окно отдельной программой: `/bin/filedlg`
// ---------------------------------------------------------------------------

/// Начало строки ответа `/bin/filedlg` в его стандартном выводе.
pub const ANSWER: &str = "\x1efiledlg-answer\t";

/// Где лежит программа окна.
pub const PROGRAM: &str = "/bin/filedlg";

/// Запрос окну: строки `ключ\tзначение`.
#[must_use]
pub fn request_text(options: &Options, owner: i64, tag: &str) -> String {
    let clean = |text: &str| text.replace(['\n', '\r', '\t'], " ");
    let mode = match options.mode {
        Mode::Open => "open",
        Mode::Save => "save",
        Mode::Folder => "folder",
    };
    let mut out = format!("mode\t{mode}\nowner\t{owner}\ntag\t{}\n", clean(tag));
    if let Some(title) = &options.title {
        out.push_str(&format!("title\t{}\n", clean(title)));
    }
    if let Some(dir) = &options.dir {
        out.push_str(&format!("dir\t{}\n", clean(dir)));
    }
    if !options.name.is_empty() {
        out.push_str(&format!("name\t{}\n", clean(&options.name)));
    }
    for filter in &options.filters {
        // Подпись — без масок: `Filter::new` допишет их заново.
        let label = filter.label.split(" (").next().unwrap_or(&filter.label);
        out.push_str(&format!("filter\t{}\t{}\n", clean(label), filter.exts.join(";")));
    }
    out.push_str(&format!("index\t{}\nextension\t{}\n", options.filter, u8::from(options.add_extension)));
    out
}

/// Разобрать запрос: параметры окна, номер окна-владельца и имя программы.
#[must_use]
pub fn parse_request(text: &str) -> (Options, i64, String) {
    let mut options = Options::open();
    let mut owner = 0i64;
    let mut tag = String::from("filedlg");
    let mut filters: Vec<Filter> = Vec::new();
    let mut extension = None;
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(key), Some(value)) = (fields.next(), fields.next()) else {
            continue;
        };
        match key {
            "mode" => {
                options.mode = match value {
                    "save" => Mode::Save,
                    "folder" => Mode::Folder,
                    _ => Mode::Open,
                };
            }
            "owner" => owner = value.trim().parse().unwrap_or(0),
            "tag" => tag = value.to_string(),
            "title" if !value.trim().is_empty() => options.title = Some(value.to_string()),
            "dir" if !value.trim().is_empty() => options.dir = Some(value.to_string()),
            "name" => options.name = value.to_string(),
            "filter" => {
                let exts: Vec<&str> = fields.next().unwrap_or("").split([';', ',']).map(|ext| ext.trim().trim_start_matches("*.").trim_start_matches('.')).filter(|ext| !ext.is_empty() && *ext != "*").collect();
                filters.push(if exts.is_empty() { Filter::all() } else { Filter::new(value, &exts) });
            }
            "index" => options.filter = value.trim().parse().unwrap_or(0),
            "extension" => extension = Some(value.trim() == "1"),
            _ => {}
        }
    }
    if !filters.is_empty() {
        options.filters = filters;
    }
    options.add_extension = extension.unwrap_or(options.mode == Mode::Save);
    (options, owner, tag)
}

/// Окно выбора файла, открытое отдельной программой (см. заголовок модуля).
pub struct Dialog {
    task: i64,
    out: i64,
    pending: Vec<u8>,
    tag: String,
}

impl Dialog {
    /// Открыть окно над окном этой программы (`app::window_id`). `tag` —
    /// имя программы в журнале окна.
    pub fn start(options: &Options, tag: &str) -> Result<Self, String> {
        Self::start_owned(options, crate::app::window_id(), tag)
    }

    /// То же, владелец назван явно (ноль — без владельца): `dotnet` держит
    /// окна сам.
    pub fn start_owned(options: &Options, owner: i64, tag: &str) -> Result<Self, String> {
        let (in_read, in_write) = crate::pipe().map_err(error_text)?;
        let (out_read, out_write) = match crate::pipe() {
            Ok(pair) => pair,
            Err(code) => {
                close(in_read);
                close(in_write);
                return Err(error_text(code));
            }
        };
        let task = crate::launch(&format!("{PROGRAM} --stdin"), None, in_read, out_write);
        // Свои копии концов, отданных окну, — закрыть сразу: иначе конца
        // файла не будет ни у него, ни у нас.
        close(in_read);
        close(out_write);
        if task < 0 {
            close(in_write);
            close(out_read);
            return Err(format!("{PROGRAM} не запускается: {}", error_text(task)));
        }
        let request = request_text(options, owner, tag);
        let mut sent = 0usize;
        let bytes = request.as_bytes();
        let deadline = monotonic_ms() + 2_000;
        while sent < bytes.len() && monotonic_ms() < deadline {
            let wrote = write(in_write, &bytes[sent..]);
            if wrote > 0 {
                sent += wrote as usize;
            } else if wrote != user_abi::ERR_AGAIN {
                break;
            } else {
                crate::nanosleep(0, 5_000_000);
            }
        }
        close(in_write);
        println(&format!("{tag}: file dialog #{task} started"));
        Ok(Self { task, out: out_read, pending: Vec::new(), tag: tag.to_string() })
    }

    /// Номер задачи окна.
    #[must_use]
    pub const fn task(&self) -> i64 {
        self.task
    }

    /// Чем кончилось — не дожидаясь: `None` — окно ещё открыто. Строки
    /// журнала окна печатаются от имени программы.
    pub fn poll(&mut self) -> Option<Outcome> {
        let mut buffer = [0u8; 1024];
        let ended = loop {
            let got = read(self.out, &mut buffer);
            if got > 0 {
                self.pending.extend_from_slice(&buffer[..got as usize]);
                continue;
            }
            break got == 0 || (got < 0 && got != user_abi::ERR_AGAIN);
        };
        let mut answer = None;
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if let Some(rest) = line.strip_prefix(ANSWER) {
                answer = Some(match rest.split_once('\t') {
                    Some(("ok", path)) if !path.is_empty() => Outcome::Chosen(path.to_string()),
                    _ => Outcome::Cancelled,
                });
            } else if !line.is_empty() {
                println(&line);
            }
        }
        if answer.is_some() {
            return answer;
        }
        if ended {
            println(&format!("{}: file dialog #{} ended without an answer", self.tag, self.task));
            return Some(Outcome::Cancelled);
        }
        None
    }

    /// Дождаться ответа (программа со своим циклом событий зовёт [`Self::poll`]
    /// сама; это — для тех, кому окно своё перерисовывать не нужно).
    pub fn wait(mut self) -> Outcome {
        loop {
            if let Some(outcome) = self.poll() {
                return outcome;
            }
            crate::nanosleep(0, 20_000_000);
        }
    }
}

impl Drop for Dialog {
    fn drop(&mut self) {
        close(self.out);
    }
}

fn error_text(code: i64) -> String {
    match code {
        user_abi::ERR_NOT_FOUND => String::from("нет такого файла или папки"),
        user_abi::ERR_PERMISSION => String::from("нет прав"),
        user_abi::ERR_EXISTS => String::from("такое имя уже есть"),
        user_abi::ERR_NO_SPACE => String::from("на диске нет места"),
        user_abi::ERR_UNSUPPORTED => String::from("этот том не умеет записи"),
        _ => format!("код {code}"),
    }
}
