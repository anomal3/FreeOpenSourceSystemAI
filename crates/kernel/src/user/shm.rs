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

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
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
}

impl SharedMem {
    #[must_use]
    pub fn new(size: u64) -> Arc<Self> {
        Arc::new(Self { frames: Mutex::new(BTreeMap::new()), size: AtomicU64::new(size) })
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
