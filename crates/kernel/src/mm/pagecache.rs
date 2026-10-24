//! Кэш страниц файлов программ: одна копия страницы на все процессы.
//!
//! # Зачем
//!
//! До этого модуля каждая программа читала страницы своего исполняемого файла
//! и библиотек с диска сама, по обращению. Пока программа запускалась одна,
//! это было незаметно. Chromium запускает около восьми процессов из одного и
//! того же файла в 250 МБ, и под VirtualBox до первого окна ядро прочитало с
//! диска 157 тысяч страниц — 600 МБ одного и того же (24.10). Windows и Linux
//! держат страницу файла в памяти один раз и отображают её всем.
//!
//! # Как
//!
//! Чистая (только на чтение) страница файла, прочитанная по отказу, остаётся
//! здесь: кэш — ещё один владелец её кадра (счётчик владельцев — тот же, что
//! у копирования при записи, `BitmapFrameAllocator::share`). Следующий отказ
//! на той же странице того же файла берёт этот кадр, а не читает диск.
//! Программа, снимая страницу, снимает своего владельца — кадр остаётся кэшу.
//!
//! Ключ — файл (`Node::cache_key`: у ext2 — том и номер inode), смещение и
//! сколько байт страницы из файла (хвост сегмента за файлом — нули, и та же
//! страница с другой длиной — другое содержимое). Запись в файл, его усечение
//! и удаление забывают его страницы (`forget`): отображённые программами
//! кадры остаются у них, а следующий отказ прочтёт файл заново.
//!
//! Памяти кэш не отнимает: когда её мало, `shrink` отдаёт кадры, которые
//! держит только он.

use alloc::collections::BTreeMap;

use crate::mm::{FrameAllocator, PhysAddr};
use crate::sync::SpinLock;

/// Файл, смещение страницы, байт из файла.
type Key = ((usize, u64), u64, usize);

static CACHE: SpinLock<BTreeMap<Key, PhysAddr>> = SpinLock::new(BTreeMap::new());

/// Больше скольких страниц не держать — доля всей памяти.
const SHARE_OF_MEMORY: usize = 4;

/// Кадр страницы из кэша — уже с владельцем для вызывающего. `None` —
/// страницы нет (или кадр больше нельзя делить: тогда её читают заново).
#[must_use]
pub fn take(file: (usize, u64), offset: u64, want: usize) -> Option<PhysAddr> {
    let cache = CACHE.lock();
    let frame = *cache.get(&(file, offset, want))?;
    crate::mm::frame::with(|frames| frames.share(frame)).unwrap_or(false).then_some(frame)
}

/// Положить прочитанную страницу: кэш становится ещё одним владельцем
/// кадра. Страница уже есть (её прочитал соседний процесс) — не кладётся.
pub fn keep(file: (usize, u64), offset: u64, want: usize, frame: PhysAddr) {
    let total = crate::mm::frame::stats().total;
    let mut cache = CACHE.lock();
    if cache.contains_key(&(file, offset, want)) || cache.len() >= total / SHARE_OF_MEMORY {
        return;
    }
    if crate::mm::frame::with(|frames| frames.share(frame)).unwrap_or(false) {
        cache.insert((file, offset, want), frame);
    }
}

/// Забыть страницы файла: его переписали, усекли или удалили.
pub fn forget(file: (usize, u64)) {
    let mut cache = CACHE.lock();
    let gone: alloc::vec::Vec<Key> = cache.range((file, 0, 0)..=(file, u64::MAX, usize::MAX)).map(|(key, _)| *key).collect();
    if gone.is_empty() {
        return;
    }
    crate::mm::frame::with(|frames| {
        for key in &gone {
            if let Some(frame) = cache.remove(key) {
                // SAFETY: кэш — один из владельцев кадра; `free` снимает его,
                // и кадр уходит в пул, только если других владельцев нет.
                unsafe { frames.free(frame) };
            }
        }
    });
}

/// Отдать до `want` кадров, которые держит только кэш (память на исходе).
/// Возвращает, сколько отдано.
pub fn shrink(want: usize) -> usize {
    let mut cache = CACHE.lock();
    let mut freed = 0;
    crate::mm::frame::with(|frames| {
        cache.retain(|_, frame| {
            if freed >= want || frames.is_shared(*frame) {
                return true;
            }
            // SAFETY: владелец один — кэш; кадр не отображён никем.
            unsafe { frames.free(*frame) };
            freed += 1;
            false
        });
    });
    freed
}

/// Сколько страниц в кэше.
#[must_use]
pub fn pages() -> usize {
    CACHE.lock().len()
}
