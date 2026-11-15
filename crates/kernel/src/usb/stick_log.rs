//! Журнал ядра на флешку (15.11).
//!
//! # Зачем
//!
//! На настоящем ПК без сети и без последовательной линии журнал загрузки можно
//! было только сфотографировать с экрана. Роман: «сделай так, чтобы на флешку
//! логи писались». Живой носитель несёт `\FREEOS\LOG.TXT` — файл нужного
//! размера из пробелов (его кладёт `xtask image`), и ядро переписывает его
//! сектора: сначала всё, что накопилось с загрузки, дальше — новые строки раз
//! в пару секунд.
//!
//! # Почему так, а не запись в FAT
//!
//! FAT в ядре только читает, и учить её выделять кластеры ради журнала — это
//! риск испортить загрузочную флешку на машине, где её больше нечем починить.
//! Здесь меняются только данные одного файла: ни таблица FAT, ни каталог, ни
//! размер файла не трогаются — сектора берутся из цепочки кластеров, которую
//! записал `xtask`.

use alloc::vec::Vec;

use disk::BlockDevice as _;

use crate::block::Shared;
use crate::kprintln;
use crate::sync::SpinLock;

/// Как часто дописывать новое, мс.
const FLUSH_MS: u64 = 2000;
/// Как часто снимать кольцо журнала в память, мс: кольцо — 32 КиБ, загрузка
/// столько за полсекунды не печатает.
const CAPTURE_MS: u64 = 250;
/// Больше файла всё равно не записать.
const CAPTURE_MAX: usize = 4 * 1024 * 1024;
const SECTOR: usize = 512;

/// Куда писать: носитель и сектора файла по порядку (начало, сколько).
struct Target {
    device: Shared,
    runs: Vec<(u64, u64)>,
    /// Размер файла в байтах — дальше него не пишется ничего.
    size: usize,
    /// Сколько байт журнала уже на флешке.
    flushed: usize,
    /// Файл ещё не очищен от прошлой загрузки.
    fresh: bool,
    /// Неудачных записей подряд: флешку могли вынуть, а могли просто занять
    /// перечислением портов — сдаёмся не с первой.
    failures: u32,
}

static TARGET: SpinLock<Option<Target>> = SpinLock::new(None);
/// Всё, что ядро напечатало с запуска (насколько успели снять с кольца).
static CAPTURED: SpinLock<Vec<u8>> = SpinLock::new(Vec::new());

fn le16(b: &[u8], at: usize) -> u32 {
    u32::from(u16::from_le_bytes([b[at], b[at + 1]]))
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Раскладка тома FAT32 — ровно то, что нужно для обхода цепочек.
struct Fat {
    fat_lba: u64,
    data_lba: u64,
    per_cluster: u64,
}

impl Fat {
    fn cluster_lba(&self, cluster: u32) -> u64 {
        self.data_lba + u64::from(cluster - 2) * self.per_cluster
    }

    fn next(&self, device: &mut Shared, cluster: u32) -> Option<u32> {
        let offset = u64::from(cluster) * 4;
        let mut sector = [0u8; SECTOR];
        device.read(self.fat_lba + offset / SECTOR as u64, &mut sector).ok()?;
        let next = le32(&sector, (offset % SECTOR as u64) as usize) & 0x0fff_ffff;
        (2..0x0fff_fff8).contains(&next).then_some(next)
    }

    /// Найти запись с именем 8.3 в каталоге: (первый кластер, размер, атрибуты).
    fn find(&self, device: &mut Shared, dir: u32, name: &[u8; 11]) -> Option<(u32, u32, u8)> {
        let mut cluster = dir;
        let mut sector = [0u8; SECTOR];
        for _ in 0..4096 {
            for index in 0..self.per_cluster {
                device.read(self.cluster_lba(cluster) + index, &mut sector).ok()?;
                for entry in sector.chunks_exact(32) {
                    match entry[0] {
                        0 => return None,
                        0xe5 => continue,
                        _ => {}
                    }
                    if entry[11] == 0x0f || &entry[..11] != name {
                        continue;
                    }
                    let first = (le16(entry, 20) << 16) | le16(entry, 26);
                    return Some((first, le32(entry, 28), entry[11]));
                }
            }
            cluster = self.next(device, cluster)?;
        }
        None
    }
}

/// Том смонтирован: если на нём `\FREEOS\LOG.TXT`, писать журнал туда.
///
/// Зовётся из [`super::storage`] для каждого открытого тома FAT32.
pub fn offer(device: &Shared, first_lba: u64) {
    if TARGET.lock().is_some() {
        return;
    }
    let mut device = device.clone();
    if device.sector_size() as usize != SECTOR {
        return;
    }
    let mut boot = [0u8; SECTOR];
    if device.read(first_lba, &mut boot).is_err() || le16(&boot, 11) as usize != SECTOR {
        return;
    }
    let per_cluster = u64::from(boot[13]);
    let reserved = u64::from(le16(&boot, 14));
    let fats = u64::from(boot[16]);
    let fat_size = u64::from(le32(&boot, 36));
    let root = le32(&boot, 44);
    if per_cluster == 0 || fats == 0 || fat_size == 0 || root < 2 {
        return;
    }
    let fat = Fat { fat_lba: first_lba + reserved, data_lba: first_lba + reserved + fats * fat_size, per_cluster };
    let Some((dir, _, attr)) = fat.find(&mut device, root, b"FREEOS     ") else { return };
    if attr & 0x10 == 0 {
        return;
    }
    let Some((first, size, _)) = fat.find(&mut device, dir, b"LOG     TXT") else { return };
    if first < 2 || size == 0 {
        return;
    }
    // Сектора файла: цепочка кластеров, слитая в непрерывные куски.
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut cluster = first;
    let mut covered = 0u64;
    let wanted = u64::from(size).div_ceil(SECTOR as u64);
    while covered < wanted {
        let lba = fat.cluster_lba(cluster);
        match runs.last_mut() {
            Some((start, count)) if *start + *count == lba => *count += per_cluster,
            _ => runs.push((lba, per_cluster)),
        }
        covered += per_cluster;
        if covered >= wanted {
            break;
        }
        let Some(next) = fat.next(&mut device, cluster) else { break };
        cluster = next;
    }
    let size = (covered.min(wanted) as usize * SECTOR).min(size as usize);
    kprintln!("  stick-log   : writing the kernel log to \\FREEOS\\LOG.TXT ({} KiB, {} run(s))", size / 1024, runs.len());
    *TARGET.lock() = Some(Target { device, runs, size, flushed: 0, fresh: true, failures: 0 });
}

/// Сектор файла по номеру сектора внутри файла.
fn file_lba(runs: &[(u64, u64)], mut index: u64) -> Option<u64> {
    for (start, count) in runs {
        if index < *count {
            return Some(start + index);
        }
        index -= count;
    }
    None
}

impl Target {
    /// Записать сектора файла `[from, to)` из `bytes` (недостающее — пробелы).
    fn write_range(&mut self, bytes: &[u8], from: usize, to: usize) -> bool {
        let mut sector = from / SECTOR;
        let last = to.div_ceil(SECTOR);
        let mut buffer = alloc::vec![b' '; SECTOR * 16];
        while sector < last {
            // Кусок подряд идущих секторов — одной командой, до 8 КиБ.
            let Some(lba) = file_lba(&self.runs, sector as u64) else { return false };
            let mut count = 1;
            while count < 16
                && sector + count < last
                && file_lba(&self.runs, (sector + count) as u64) == Some(lba + count as u64)
            {
                count += 1;
            }
            let chunk = &mut buffer[..count * SECTOR];
            chunk.fill(b' ');
            let start = sector * SECTOR;
            if start < bytes.len() {
                let end = (start + chunk.len()).min(bytes.len());
                chunk[..end - start].copy_from_slice(&bytes[start..end]);
            }
            if self.device.write(lba, chunk).is_err() {
                return false;
            }
            sector += count;
        }
        true
    }
}

/// Задача: снимать кольцо журнала в память и дописывать на флешку.
pub fn task() {
    let mut position = 0u64;
    let mut next_flush = 0u64;
    let mut chunk = alloc::vec![0u8; 4096];
    loop {
        // Снять новое с кольца.
        loop {
            let (count, next) = crate::klog::read(position, &mut chunk);
            position = next;
            if count == 0 {
                break;
            }
            let mut captured = CAPTURED.lock();
            let room = CAPTURE_MAX.saturating_sub(captured.len());
            captured.extend_from_slice(&chunk[..count.min(room)]);
        }
        let now = crate::time::uptime_ms();
        if now >= next_flush {
            next_flush = now + FLUSH_MS;
            flush();
        }
        crate::sched::sleep_ms(CAPTURE_MS);
    }
}

fn flush() {
    let Some(mut target) = TARGET.lock().take() else { return };
    let bytes = CAPTURED.lock().clone();
    let end = bytes.len().min(target.size);
    let mut ok = true;
    if target.fresh {
        // Прошлая загрузка стирается целиком: иначе её хвост читался бы как
        // продолжение этой.
        ok = target.write_range(&bytes, 0, target.size);
        target.fresh = !ok;
        target.flushed = end;
    } else if end > target.flushed {
        // Последний неполный сектор переписывается: в нём дописалось.
        let from = target.flushed / SECTOR * SECTOR;
        ok = target.write_range(&bytes, from, end);
        if ok {
            target.flushed = end;
        }
    }
    if ok {
        let _ = target.device.flush();
        target.failures = 0;
        *TARGET.lock() = Some(target);
    } else {
        target.failures += 1;
        if target.failures < 5 {
            *TARGET.lock() = Some(target);
        } else {
            kprintln!("  stick-log   : writing to the stick keeps failing - the log stays in memory only");
        }
    }
}
