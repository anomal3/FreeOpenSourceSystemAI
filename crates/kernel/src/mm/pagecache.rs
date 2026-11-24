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

/// Есть ли страница в кэше (для упреждающего чтения: читать её незачем).
#[must_use]
pub fn has(file: (usize, u64), offset: u64, want: usize) -> bool {
    CACHE.lock().contains_key(&(file, offset, want))
}

/// Есть ли у кэша место ещё под `pages` страниц.
#[must_use]
pub fn roomy(pages: usize) -> bool {
    let total = crate::mm::frame::stats().total;
    CACHE.lock().len() + pages < total / SHARE_OF_MEMORY
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

/// Pages handled per hold of the cache lock (hw-log14/17: `forget` of a
/// file with thousands of cached pages, and `shrink` walking the whole
/// cache, kept interrupts off for 7-11 ms - serial input lost characters).
const BATCH: usize = 64;
/// Entries `shrink` looks at per hold of the lock.
const SCAN: usize = 1024;

/// Забыть страницы файла: его переписали, усекли или удалили.
///
/// In batches of [`BATCH`]: the pages leave the map under the lock, and
/// their frames are released after it (nobody can `take` a page that is
/// no longer in the map).
pub fn forget(file: (usize, u64)) {
    let range = (file, 0, 0)..=(file, u64::MAX, usize::MAX);
    loop {
        let mut gone = [PhysAddr::new(0); BATCH];
        let mut count = 0;
        {
            let mut cache = CACHE.lock();
            while count < BATCH {
                let Some(key) = cache.range(range.clone()).next().map(|(key, _)| *key) else {
                    break;
                };
                if let Some(frame) = cache.remove(&key) {
                    gone[count] = frame;
                    count += 1;
                }
            }
        }
        if count == 0 {
            return;
        }
        crate::mm::frame::with(|frames| {
            for frame in &gone[..count] {
                // SAFETY: кэш — один из владельцев кадра; `free` снимает его,
                // и кадр уходит в пул, только если других владельцев нет.
                unsafe { frames.free(*frame) };
            }
        });
        if count < BATCH {
            return;
        }
    }
}

/// Забыть страницы всех файлов тома: его память о незафиксированном пропала
/// (писатель btrfs открыт заново после отказа).
pub fn forget_volume(volume: usize) {
    let numbers: alloc::vec::Vec<u64> = {
        let cache = CACHE.lock();
        let mut numbers: alloc::vec::Vec<u64> =
            cache.range(((volume, 0), 0, 0)..=((volume, u64::MAX), u64::MAX, usize::MAX)).map(|(key, _)| key.0.1).collect();
        numbers.dedup();
        numbers
    };
    for number in numbers {
        forget((volume, number));
    }
}

/// Отдать до `want` кадров, которые держит только кэш (память на исходе).
/// Возвращает, сколько отдано.
///
/// Looks at [`SCAN`] entries per hold of the lock (it used to `retain`
/// over the whole map at once - millions of entries on a big machine).
pub fn shrink(want: usize) -> usize {
    use core::ops::Bound;
    let mut freed = 0;
    let mut from: Bound<Key> = Bound::Unbounded;
    while freed < want {
        let mut cache = CACHE.lock();
        let mut victims = [((0usize, 0u64), 0u64, 0usize); BATCH];
        let mut count = 0;
        let mut seen = 0;
        let mut last = None;
        crate::mm::frame::with(|frames| {
            for (key, frame) in cache.range((from, Bound::Unbounded)) {
                last = Some(*key);
                seen += 1;
                if !frames.is_shared(*frame) {
                    victims[count] = *key;
                    count += 1;
                }
                if count == BATCH || freed + count >= want || seen >= SCAN {
                    break;
                }
            }
            for key in &victims[..count] {
                if let Some(frame) = cache.remove(key) {
                    // SAFETY: владелец один — кэш (проверено под этим же
                    // замком); кадр не отображён никем.
                    unsafe { frames.free(frame) };
                }
            }
        });
        drop(cache);
        freed += count;
        match last {
            Some(key) => from = Bound::Excluded(key),
            None => break,
        }
    }
    freed
}

/// Сколько страниц в кэше.
#[must_use]
pub fn pages() -> usize {
    CACHE.lock().len()
}
