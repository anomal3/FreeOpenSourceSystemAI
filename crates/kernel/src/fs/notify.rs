//! Наблюдение за каталогами (20.11): кто-то создал, удалил, переименовал или
//! записал файл — наблюдающие узнают об этом сразу.
//!
//! # Зачем
//!
//! Роман (20.11): файл, сохранённый Блокнотом на Рабочий стол, не появлялся на
//! столе, пока не нажать «Обновить». Стол, «Файлы» и окно выбора файла
//! перечитывали каталог только по просьбе человека. Опрашивать каталоги по
//! таймеру — это читать диск впустую раз в секунду в каждой открытой папке;
//! правильнее, чтобы о переменах говорил тот, кто их делает, — файловый слой.
//!
//! # Как устроено
//!
//! Очередь событий — [`Queue`]: у неё свои наблюдения (каталог и маска) и свой
//! список событий. Всякое изменение имени в каталоге проходит через
//! `fs::create_as`, `mkdir_as`, `remove_as`, `rename_as`, а запись файла
//! кончается закрытием открытого на запись — там и зовётся [`emit`]. Очереди
//! держат программы (дескриптор [`user_abi::SYS_FSWATCH`] и `inotify` Linux —
//! один и тот же объект) и само ядро (значки стола).
//!
//! События — о **именах в каталоге**, как у `inotify`: наблюдение за `/a`
//! видит `/a/b`, но не `/a/b/c`. Пути — те, которыми назвал файл вызывающий,
//! приведённые к виду без `//`, `.` и `/` в конце; двух имён у одного
//! каталога (символьные ссылки, второй точки монтирования) наблюдение не
//! видит — это TODO.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::sync::Mutex;
use crate::user::files::Object;

pub use user_abi::{
    FSN_ATTRIB as ATTRIB, FSN_CLOSE_WRITE as CLOSE_WRITE, FSN_CREATE as CREATE, FSN_DELETE as DELETE, FSN_DELETE_SELF as DELETE_SELF,
    FSN_ISDIR as ISDIR, FSN_MODIFY as MODIFY, FSN_MOVED_FROM as MOVED_FROM, FSN_MOVED_TO as MOVED_TO, FSN_OVERFLOW as OVERFLOW,
};

/// Самое большее событий в одной очереди; дальше — одно [`OVERFLOW`].
const MAX_EVENTS: usize = 512;
/// Самое большее наблюдений у одной очереди.
const MAX_WATCHES: usize = 256;
/// Самое большее очередей в системе.
const MAX_QUEUES: usize = 256;

/// Событие.
#[derive(Clone, Debug)]
pub struct Event {
    pub wd: i32,
    pub mask: u32,
    pub cookie: u32,
    /// Имя в каталоге; пустое — событие о самом каталоге.
    pub name: String,
}

struct Watch {
    wd: i32,
    path: String,
    mask: u32,
}

#[derive(Default)]
struct Inner {
    watches: Vec<Watch>,
    next_wd: i32,
    events: VecDeque<Event>,
    overflowed: bool,
}

/// Очередь событий со своими наблюдениями.
pub struct Queue {
    inner: Mutex<Inner>,
}

/// Все живые очереди. Слабые ссылки: очередь живёт, пока её держит
/// дескриптор или ядро, и уходит отсюда при следующем [`emit`].
static QUEUES: Mutex<Vec<Weak<Queue>>> = Mutex::new(Vec::new());

/// Связка «переименовано отсюда — сюда» (`cookie` у `inotify`).
static COOKIE: AtomicU32 = AtomicU32::new(1);

impl Queue {
    /// Новая очередь. `None` — очередей в системе слишком много.
    #[must_use]
    pub fn new() -> Option<Arc<Self>> {
        let queue = Arc::new(Self { inner: Mutex::new(Inner { next_wd: 1, ..Inner::default() }) });
        let mut all = QUEUES.lock();
        all.retain(|weak| weak.strong_count() > 0);
        if all.len() >= MAX_QUEUES {
            return None;
        }
        all.push(Arc::downgrade(&queue));
        Some(queue)
    }

    /// Следить за каталогом `path`. Тот же каталог второй раз — тот же номер
    /// с новой маской (как `inotify_add_watch`). Ноль в маске — всё.
    pub fn add(&self, path: &str, mask: u32) -> Option<i32> {
        let path = normalize(path);
        let mask = if mask == 0 { u32::MAX } else { mask };
        let mut inner = self.inner.lock();
        if let Some(watch) = inner.watches.iter_mut().find(|watch| watch.path == path) {
            watch.mask = mask;
            return Some(watch.wd);
        }
        if inner.watches.len() >= MAX_WATCHES {
            return None;
        }
        let wd = inner.next_wd;
        inner.next_wd += 1;
        inner.watches.push(Watch { wd, path, mask });
        Some(wd)
    }

    /// Снять наблюдение. `false` — такого нет.
    pub fn remove(&self, wd: i32) -> bool {
        let mut inner = self.inner.lock();
        let before = inner.watches.len();
        inner.watches.retain(|watch| watch.wd != wd);
        inner.watches.len() != before
    }

    /// Снять все наблюдения (стол сменил каталог).
    pub fn clear(&self) {
        let mut inner = self.inner.lock();
        inner.watches.clear();
        inner.events.clear();
        inner.overflowed = false;
    }

    /// Есть ли что читать.
    #[must_use]
    pub fn ready(&self) -> bool {
        !self.inner.lock().events.is_empty()
    }

    /// Забрать все события разом.
    pub fn drain(&self) -> Vec<Event> {
        let mut inner = self.inner.lock();
        inner.overflowed = false;
        inner.events.drain(..).collect()
    }

    /// Разложить события в `buf` записями `struct inotify_event` (имя с нулём,
    /// до кратного 16). Сколько байт; ноль — нечего или первая запись не
    /// помещается (`Err` с нужным размером).
    pub fn read_into(&self, buf: &mut [u8]) -> Result<usize, usize> {
        let mut inner = self.inner.lock();
        let mut used = 0usize;
        while let Some(event) = inner.events.front() {
            let name_len = if event.name.is_empty() { 0 } else { (event.name.len() + 1).div_ceil(16) * 16 };
            let need = 16 + name_len;
            if used + need > buf.len() {
                if used == 0 {
                    return Err(need);
                }
                break;
            }
            let out = &mut buf[used..used + need];
            out[..4].copy_from_slice(&event.wd.to_ne_bytes());
            out[4..8].copy_from_slice(&event.mask.to_ne_bytes());
            out[8..12].copy_from_slice(&event.cookie.to_ne_bytes());
            out[12..16].copy_from_slice(&(name_len as u32).to_ne_bytes());
            out[16..].fill(0);
            out[16..16 + event.name.len()].copy_from_slice(event.name.as_bytes());
            used += need;
            inner.events.pop_front();
        }
        if inner.events.is_empty() {
            inner.overflowed = false;
        }
        Ok(used)
    }

    /// Положить событие тем наблюдениям, которые смотрят на `dir`. `true` —
    /// хоть одно положено.
    fn offer(&self, dir: &str, name: &str, mask: u32, cookie: u32) -> bool {
        let mut inner = self.inner.lock();
        let matched: Vec<i32> = inner
            .watches
            .iter()
            .filter(|watch| watch.path == dir && watch.mask & mask & !ISDIR != 0)
            .map(|watch| watch.wd)
            .collect();
        for wd in &matched {
            if inner.overflowed {
                break;
            }
            // Запись кусками даёт подряд одинаковые события — одно.
            if inner.events.back().is_some_and(|last| last.wd == *wd && last.mask == mask && last.name == name && mask & (MODIFY | ATTRIB) != 0) {
                continue;
            }
            if inner.events.len() >= MAX_EVENTS {
                inner.overflowed = true;
                inner.events.push_back(Event { wd: -1, mask: OVERFLOW, cookie: 0, name: String::new() });
                break;
            }
            inner.events.push_back(Event { wd: *wd, mask, cookie, name: String::from(name) });
        }
        !matched.is_empty()
    }
}

/// Объект за дескриптором: `SYS_FSWATCH` родных программ и `inotify` Linux.
pub struct Watcher {
    pub queue: Arc<Queue>,
}

impl Object for Watcher {
    fn read(&self, buf: &mut [u8]) -> Result<usize, i64> {
        // -EAGAIN / -EINVAL Linux: объект читают и программы Linux.
        match self.queue.read_into(buf) {
            Ok(0) => Err(-11),
            Ok(used) => Ok(used),
            Err(_) => Err(-22),
        }
    }
    fn write(&self, _data: &[u8]) -> Result<usize, i64> {
        Err(-22)
    }
    fn readiness(&self) -> (bool, bool, bool) {
        (self.queue.ready(), false, false)
    }
    fn mode(&self) -> u32 {
        0
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Путь без `//`, `.`, `..` и `/` в конце.
#[must_use]
pub fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let mut out = String::with_capacity(path.len() + 1);
    for part in parts {
        out.push('/');
        out.push_str(part);
    }
    if out.is_empty() {
        out.push('/');
    }
    out
}

/// Каталог и имя пути.
fn split(path: &str) -> (String, String) {
    let path = normalize(path);
    match path.rsplit_once('/') {
        Some(("", name)) => (String::from("/"), String::from(name)),
        Some((dir, name)) => (String::from(dir), String::from(name)),
        None => (String::from("/"), path),
    }
}

/// Сказать наблюдающим о событии `mask` с именем `name` в каталоге `dir`
/// (путь уже приведён). Пустое `name` — событие о самом каталоге.
fn deliver(dir: &str, name: &str, mask: u32, cookie: u32) {
    let queues: Vec<Arc<Queue>> = {
        let mut all = QUEUES.lock();
        all.retain(|weak| weak.strong_count() > 0);
        all.iter().filter_map(Weak::upgrade).collect()
    };
    let mut any = false;
    for queue in queues {
        any |= queue.offer(dir, name, mask, cookie);
    }
    if any {
        crate::io_events::notify();
    }
}

/// Что-то случилось с файлом или каталогом `path`.
pub fn emit(path: &str, mask: u32) {
    if QUEUES.lock().is_empty() {
        return;
    }
    let (dir, name) = split(path);
    deliver(&dir, &name, mask, 0);
    // Удалён сам наблюдаемый каталог — тем, кто смотрит на него.
    if mask & DELETE != 0 && mask & ISDIR != 0 {
        deliver(&normalize(path), "", DELETE_SELF, 0);
    }
}

/// Переименование: два события с общей связкой.
pub fn emit_rename(from: &str, to: &str, dir: bool) {
    if QUEUES.lock().is_empty() {
        return;
    }
    let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
    let isdir = if dir { ISDIR } else { 0 };
    let (from_dir, from_name) = split(from);
    let (to_dir, to_name) = split(to);
    deliver(&from_dir, &from_name, MOVED_FROM | isdir, cookie);
    deliver(&to_dir, &to_name, MOVED_TO | isdir, cookie);
}
