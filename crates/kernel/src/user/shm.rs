//! Разделяемая память (фаза 71e): `memfd_create`, `/dev/shm`,
//! `MAP_SHARED | MAP_ANONYMOUS`.
//!
//! # Чьи кадры
//!
//! Кадры принадлежат объекту [`SharedMem`], а не программе: их видят все
//! процессы, отобразившие его, и после `fork` — оба. Поэтому область такой
//! памяти у программы ([`super::Source::Shared`]) снимается **без** возврата
//! кадров в пул (`Space::unmap_range_keep`) — и до того, как разберут
//! пространство, иначе разбор вернул бы в пул чужие кадры. Вернёт их сам
//! объект, когда уйдёт последняя ссылка: из дескриптора, из области, из
//! `/dev/shm`.
//!
//! Кадры приходят по первому касанию — отказом страницы в любом из процессов
//! или записью через дескриптор, — и до конца жизни объекта не уходят.
//!
//! # Файл на диске, отображённый на запись (фаза 71i)
//!
//! `MAP_SHARED | PROT_WRITE` обычного файла (индекс кэша Chromium, SQLite) —
//! тоже такой объект: [`SharedMem::of_file`] заполняет его куском файла, а
//! записанное уходит обратно в файл, когда объект уходит (снято последнее
//! отображение) или по [`SharedMem::flush`]. Видно это всем, кто отобразил
//! тот же объект (и детям после `fork`); другому `mmap` того же файла —
//! только после записи обратно.
//!
//! # Один объект на файл (фаза 71j)
//!
//! Wine держит общую память сервера и клиентов на файлах: `wineserver`
//! создаёт файл, отображает его на запись и передаёт дескриптор клиенту
//! (`SCM_RIGHTS`), клиент отображает тот же файл — и оба обязаны видеть одни
//! кадры (`KUSER_SHARED_DATA`, общее состояние окон). Поэтому объект файла
//! заводится по номеру узла ([`Node::file_id`]) один на всех
//! ([`SharedMem::for_file`]): смещение в объекте — смещение в файле, кусок
//! дочитывается, когда отображают дальше.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::mm::{FrameAllocator, PAGE_SIZE, PhysAddr};
use crate::sync::Mutex;
use crate::vfs::{DirEntry, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Кусок разделяемой памяти.
pub struct SharedMem {
    /// Кадры по номеру страницы — только те, которых касались.
    frames: Mutex<BTreeMap<usize, PhysAddr>>,
    /// Длина в байтах (`ftruncate`).
    size: AtomicU64,
    /// Файл, кусок которого отображён на запись: куда возвращать записанное.
    file: Option<FileBack>,
}

/// Кусок файла `[start, end)` за объектом.
struct FileBack {
    node: Arc<dyn Node>,
    start: u64,
    /// Конец прочитанного; у общего объекта файла растёт.
    end: AtomicU64,
    /// Отпечаток каждой страницы, какой она лежит в файле: при записи обратно
    /// страница с тем же отпечатком пропускается. Без этого назад писалось
    /// всё, чего касались, — и IntelliJ (Rider), отображающая индексы на
    /// запись, а пишущая в них мало, при выходе десятками тысяч записей по
    /// 4 КБ занимала файловую систему на минуты: система «висла» (27.10).
    clean: Mutex<BTreeMap<usize, u64>>,
}

/// Общие объекты файлов по номеру узла (фаза 71j, [`SharedMem::for_file`]).
static FILES: Mutex<Vec<((usize, u64), Weak<SharedMem>)>> = Mutex::new(Vec::new());

/// Страница-буфер для чтения куска файла.
fn vec_page() -> Vec<u8> {
    alloc::vec![0u8; PAGE_SIZE]
}

/// Отпечаток страницы: FNV-1a по 64 битам. Не защита, а сравнение «та же ли
/// страница», и столкновение здесь — один не записанный кусок, который
/// программа и так меняла бы под `msync`.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for chunk in bytes.chunks_exact(8) {
        hash ^= u64::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7]]);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Сколько читать или писать за раз у куска файла: подряд, а не по странице.
const FILE_RUN: usize = 256 * 1024;

/// Больше этого отображать на запись не возьмёмся: кусок читается целиком.
const MAX_FILE_MAP: usize = 64 << 20;

impl SharedMem {
    #[must_use]
    pub fn new(size: u64) -> Arc<Self> {
        Arc::new(Self { frames: Mutex::new(BTreeMap::new()), size: AtomicU64::new(size), file: None })
    }

    /// Кусок файла `[offset, offset + len)` для `MAP_SHARED` на запись:
    /// прочитан сразу, записанное вернётся в файл. `None` — велик или не
    /// прочитался.
    pub fn of_file(node: Arc<dyn Node>, offset: u64, len: usize) -> Option<Arc<Self>> {
        if len > MAX_FILE_MAP {
            return None;
        }
        let end = offset.checked_add(len as u64)?;
        let size = node.metadata().size;
        let mem = Arc::new(Self {
            frames: Mutex::new(BTreeMap::new()),
            size: AtomicU64::new(size),
            file: Some(FileBack { node: Arc::clone(&node), start: offset, end: AtomicU64::new(end), clean: Mutex::new(BTreeMap::new()) }),
        });
        // Кусками по [`FILE_RUN`]: одно чтение файловой системы вместо
        // шестидесяти четырёх.
        let mut chunk = alloc::vec![0u8; FILE_RUN];
        let mut at = offset;
        while at < end.min(size) {
            let take = ((end.min(size) - at) as usize).min(FILE_RUN);
            let got = node.read_at(at, &mut chunk[..take]).ok()?;
            if got == 0 {
                break;
            }
            let data = &chunk[..got];
            mem.copy(at, got, |frame, done, step| {
                // SAFETY: `done + step <= got`; кадр — см. `copy`.
                unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(done), frame, step) };
            })
            .ok()?;
            at += got as u64;
        }
        mem.remember_clean();
        Some(mem)
    }

    /// Общий объект файла (фаза 71j): один на узел для всех процессов, с
    /// дочитанным `[offset, offset + len)`. Узел без номера — по самому
    /// объекту узла. `existing_only` — только если объект уже есть
    /// (общее отображение на чтение: см. `linux::mmap`).
    pub fn for_file(node: Arc<dyn Node>, offset: u64, len: usize, existing_only: bool) -> Option<Arc<Self>> {
        // Узел без номера (`/tmp` в памяти — там wineserver держит файл
        // сессии, 07.11): ключ — сам объект узла. Клиент получает тот же
        // объект через `SCM_RIGHTS`, так что и ключ у него тот же.
        let id = node.file_id().unwrap_or((usize::MAX, alloc::sync::Arc::as_ptr(&node) as *const () as usize as u64));
        let end = offset.checked_add(len as u64)?;
        let mut files = FILES.lock();
        files.retain(|(_, live)| live.strong_count() > 0);
        let found = files.iter().find(|(other, _)| *other == id).and_then(|(_, live)| live.upgrade());
        let mem = match found {
            Some(mem) => mem,
            None if existing_only => return None,
            None => {
                if len > MAX_FILE_MAP {
                    return None;
                }
                let mem = Arc::new(Self {
                    frames: Mutex::new(BTreeMap::new()),
                    size: AtomicU64::new(node.metadata().size),
                    file: Some(FileBack { node: Arc::clone(&node), start: 0, end: AtomicU64::new(0), clean: Mutex::new(BTreeMap::new()) }),
                });
                files.push((id, Arc::downgrade(&mem)));
                mem
            }
        };
        drop(files);
        mem.load(offset, end)?;
        Some(mem)
    }

    /// Дочитать из файла страницы `[offset, end)`, которых объект ещё не
    /// касался (тронутые — уже свои, их не перезаписывать).
    fn load(&self, offset: u64, end: u64) -> Option<()> {
        let back = self.file.as_ref()?;
        let size = back.node.metadata().size;
        self.size.fetch_max(size, Ordering::Relaxed);
        let first = (offset as usize) / PAGE_SIZE;
        let last = (end.min(size) as usize).div_ceil(PAGE_SIZE);
        let mut page = vec_page();
        for index in first..last {
            if self.frames.lock().contains_key(&index) {
                continue;
            }
            let at = (index * PAGE_SIZE) as u64;
            let take = ((size - at) as usize).min(PAGE_SIZE);
            let got = back.node.read_at(at, &mut page[..take]).ok()?;
            let frame = self.frame(index)?;
            // SAFETY: кадр только что выдан объекту (или выдан раньше и пуст:
            // страницы не касались); прямое отображение, внутри страницы.
            unsafe { core::ptr::copy_nonoverlapping(page.as_ptr(), super::frame_bytes(frame), got) };
            let bytes = unsafe { core::slice::from_raw_parts(super::frame_bytes(frame), PAGE_SIZE) };
            back.clean.lock().insert(index, fingerprint(bytes));
        }
        back.end.fetch_max(end, Ordering::Relaxed);
        Some(())
    }

    /// Запомнить отпечатки всех страниц — такими они лежат в файле.
    fn remember_clean(&self) {
        let Some(back) = &self.file else {
            return;
        };
        let pages: Vec<(usize, PhysAddr)> = self.frames.lock().iter().map(|(page, frame)| (*page, *frame)).collect();
        let mut clean = back.clean.lock();
        for (page, frame) in pages {
            // SAFETY: кадр принадлежит объекту и жив, пока жив он.
            let bytes = unsafe { core::slice::from_raw_parts(super::frame_bytes(frame), PAGE_SIZE) };
            clean.insert(page, fingerprint(bytes));
        }
    }

    /// Вернуть записанное в файл (`msync`, снятие отображения): только
    /// страницы, которые изменились с тех пор, как были прочитаны или
    /// записаны (см. [`FileBack::clean`]), — и подряд идущие одним вызовом.
    pub fn flush(&self) {
        let Some(back) = &self.file else {
            return;
        };
        let size = back.node.metadata().size;
        let end = back.end.load(Ordering::Relaxed).min(size);
        let pages: Vec<(usize, PhysAddr)> = self.frames.lock().iter().map(|(page, frame)| (*page, *frame)).collect();
        let mut clean = back.clean.lock();
        // Изменившиеся страницы — `(страница, кадр, отпечаток)`.
        let mut dirty: Vec<(usize, PhysAddr, u64)> = Vec::new();
        for (page, frame) in pages {
            // SAFETY: кадр принадлежит объекту и жив, пока жив он.
            let bytes = unsafe { core::slice::from_raw_parts(super::frame_bytes(frame), PAGE_SIZE) };
            let print = fingerprint(bytes);
            if clean.get(&page) != Some(&print) {
                dirty.push((page, frame, print));
            }
        }
        let mut run: Vec<u8> = Vec::new();
        let mut index = 0;
        while index < dirty.len() {
            // Отрезок подряд идущих страниц, не длиннее [`FILE_RUN`].
            let mut last = index;
            while last + 1 < dirty.len() && dirty[last + 1].0 == dirty[last].0 + 1 && (last + 2 - index) * PAGE_SIZE <= FILE_RUN {
                last += 1;
            }
            run.clear();
            let mut run_from = 0u64;
            for &(page, frame, _) in &dirty[index..=last] {
                let from = ((page * PAGE_SIZE) as u64).max(back.start);
                let to = ((page * PAGE_SIZE + PAGE_SIZE) as u64).min(end);
                if from >= to {
                    continue;
                }
                if run.is_empty() {
                    run_from = from;
                }
                let inside = (from % PAGE_SIZE as u64) as usize;
                // SAFETY: кадр принадлежит объекту и жив, пока жив он.
                let bytes = unsafe { core::slice::from_raw_parts(super::frame_bytes(frame).add(inside), (to - from) as usize) };
                run.extend_from_slice(bytes);
            }
            if !run.is_empty() && back.node.write_at(run_from, &run).is_ok() {
                for &(page, _, print) in &dirty[index..=last] {
                    clean.insert(page, print);
                }
            }
            index = last + 1;
        }
    }

    #[must_use]
    pub fn size(&self) -> u64 {
        self.size.load(Ordering::Relaxed)
    }

    /// Кадр страницы `page`: есть — его, нет — новый, с нулями. `None` —
    /// кадров не осталось (с учётом запаса ядра).
    pub fn frame(&self, page: usize) -> Option<PhysAddr> {
        let mut frames = self.frames.lock();
        if let Some(frame) = frames.get(&page) {
            return Some(*frame);
        }
        let frame = crate::mm::frame::with(|pool| {
            if pool.stats().free <= super::RESERVE_FRAMES {
                return None;
            }
            pool.allocate()
        })
        .flatten()?;
        frames.insert(page, frame);
        Some(frame)
    }

    /// Прочитать `buf.len()` байт с `offset` — без оглядки на длину файла
    /// (сервер Wayland читает буфер в пределах пула, который ему объявили).
    /// `false` — кадров не осталось.
    pub fn read(&self, offset: u64, buf: &mut [u8]) -> bool {
        let out = buf.as_mut_ptr();
        self.copy(offset, buf.len(), |frame, done, take| {
            // SAFETY: `done + take <= buf.len()`; кадр — см. `copy`.
            unsafe { core::ptr::copy_nonoverlapping(frame, out.add(done), take) };
        })
        .is_ok()
    }

    fn copy(&self, offset: u64, len: usize, mut each: impl FnMut(*mut u8, usize, usize)) -> VfsResult<()> {
        let mut done = 0;
        while done < len {
            let at = offset as usize + done;
            let page = at / PAGE_SIZE;
            let inside = at % PAGE_SIZE;
            let take = (PAGE_SIZE - inside).min(len - done);
            let frame = self.frame(page).ok_or(VfsError::NoSpace)?;
            // SAFETY: кадр принадлежит объекту и жив, пока жив он; виден через
            // прямое отображение, смещение и длина — внутри страницы.
            each(unsafe { super::frame_bytes(frame).add(inside) }, done, take);
            done += take;
        }
        Ok(())
    }
}

impl Drop for SharedMem {
    fn drop(&mut self) {
        self.flush();
        let frames = core::mem::take(&mut *self.frames.lock());
        crate::mm::frame::with(|pool| {
            for frame in frames.into_values() {
                // SAFETY: кадры выданы этим же пулом под этот объект; ссылок на
                // него не осталось — значит, и отображений, которые держат его
                // кадры (каждая область держит `Arc`).
                unsafe { pool.free(frame) };
            }
        });
    }
}

/// Файл над разделяемой памятью: дескриптор `memfd` и `/dev/shm/имя`.
pub struct ShmNode(pub Arc<SharedMem>);

impl Node for ShmNode {
    fn metadata(&self) -> Metadata {
        Metadata { kind: NodeKind::File, size: self.0.size(), mode: 0o600, uid: 0, gid: 0, mtime: 0 }
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let size = self.0.size();
        if offset >= size {
            return Ok(0);
        }
        let len = buf.len().min((size - offset) as usize);
        let out = buf.as_mut_ptr();
        self.0.copy(offset, len, |frame, done, take| {
            // SAFETY: `done + take <= len <= buf.len()`; кадр — см. `copy`.
            unsafe { core::ptr::copy_nonoverlapping(frame, out.add(done), take) };
        })?;
        Ok(len)
    }
    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        self.0.copy(offset, data.len(), |frame, done, take| {
            // SAFETY: см. `read_at`.
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr().add(done), frame, take) };
        })?;
        let end = offset + data.len() as u64;
        self.0.size.fetch_max(end, Ordering::Relaxed);
        Ok(data.len())
    }
    /// Кадры за новым концом не возвращаются: их может держать чья-то
    /// область. Уйдут вместе с объектом.
    fn truncate(&self, size: u64) -> VfsResult<()> {
        self.0.size.store(size, Ordering::Relaxed);
        Ok(())
    }
    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        Err(VfsError::WrongKind)
    }
    fn lookup(&self, _name: &str) -> VfsResult<Box<dyn Node>> {
        Err(VfsError::WrongKind)
    }
    fn shared_memory(&self) -> Option<Arc<SharedMem>> {
        Some(Arc::clone(&self.0))
    }
}

/// `/dev/shm`: именованные куски (`shm_open` glibc открывает файл здесь).
static NAMED: Mutex<Vec<(String, Arc<SharedMem>)>> = Mutex::new(Vec::new());

/// Найти кусок `/dev/shm/name`; `create` — завести, если нет.
pub fn named(name: &str, create: bool) -> Option<Arc<SharedMem>> {
    let mut list = NAMED.lock();
    if let Some((_, mem)) = list.iter().find(|(other, _)| other == name) {
        return Some(Arc::clone(mem));
    }
    if !create || list.try_reserve(1).is_err() {
        return None;
    }
    let mem = SharedMem::new(0);
    list.push((String::from(name), Arc::clone(&mem)));
    Some(mem)
}

/// Убрать имя из `/dev/shm`; память живёт, пока её держат.
pub fn unlink(name: &str) -> bool {
    let mut list = NAMED.lock();
    let before = list.len();
    list.retain(|(other, _)| other != name);
    list.len() != before
}

/// Имена `/dev/shm` — для перечисления каталога.
#[must_use]
pub fn names() -> Vec<String> {
    NAMED.lock().iter().map(|(name, _)| name.clone()).collect()
}
