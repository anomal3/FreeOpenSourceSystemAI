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

use alloc::string::String;
use alloc::vec::Vec;

use disk::BlockDevice as _;

use crate::block::Shared;
use crate::kprintln;
use crate::sync::SpinLock;

/// Как часто дописывать новое, мс.
const FLUSH_MS: u64 = 2000;
/// Как часто — пока окна Wayland часто показывают кадры (игра, 21.11):
/// запись на флешку делит контроллер USB с диском игры (журнал 13: D2R с
/// exFAT-SSD подвисал периодически). Паника и выключение сбрасывают всё
/// сразу ([`flush_now`]).
const FLUSH_BUSY_MS: u64 = 10_000;
/// Больше стольких показов кадров Wayland в секунду — «идёт игра».
const BUSY_COMMITS_PER_S: u64 = 20;
/// SYNCHRONIZE CACHE флешки — не чаще (кроме [`flush_now`]): запись
/// и так уходит на носитель, команда лишь торопит его внутренний кэш.
const SYNC_MS: u64 = 30_000;
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
/// До какой позиции кольца журнала снято в [`CAPTURED`].
static CAPTURED_AT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

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
    // Раскладка мониторов (16.11): на живой системе корень только для
    // чтения, и `desktop.cfg` не записать, — её место здесь же, рядом с
    // журналом, файлом заранее нужного размера.
    if LAYOUT.lock().is_none() {
        if let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"LAYOUT  TXT") {
            let mut layout = Target { device: device.clone(), runs, size, flushed: 0, fresh: false, failures: 0 };
            let text = layout.read_all();
            *LAYOUT.lock() = Some(layout);
            let text = String::from_utf8_lossy(&text);
            if let Some(list) = text.lines().find_map(|line| line.trim().strip_prefix("outputs=")) {
                if crate::display::outputs::saved().is_empty() {
                    let saved = crate::display::outputs::parse_saved(list);
                    kprintln!("  stick-log   : monitor layout for {} monitor(s) from \\FREEOS\\LAYOUT.TXT", saved.len());
                    crate::display::outputs::set_saved(saved);
                    if let Some(plan) = crate::ui::saved_layout() {
                        crate::ui::apply_layout(plan);
                    }
                }
            }
        }
    }
    // Режим экрана для загрузчика (17.11): `\FREEOS\DISPLAY.CFG` лежит на
    // загрузочном разделе флешки заранее нужного размера — загрузчик читает
    // его до ядра, а ядро переписывает на месте, как журнал.
    if DISPLAY.lock().is_none() {
        if let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"DISPLAY CFG") {
            *DISPLAY.lock() = Some(Target { device: device.clone(), runs, size, flushed: 0, fresh: false, failures: 0 });
        }
    }
    // Boot mode for the loader (24.11): `\FREEOS\BOOT.CFG`, pre-allocated the
    // same way (`normal` / `verbose`).
    if BOOT_CFG.lock().is_none() {
        if let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"BOOT    CFG") {
            *BOOT_CFG.lock() = Some(Target { device: device.clone(), runs, size, flushed: 0, fresh: false, failures: 0 });
        }
    }
    // Настройки живой системы (21.11): `\FREEOS\SETTINGS.DAT` заранее нужного
    // размера, его читает и переписывает `super::stick_settings`.
    if SETTINGS.lock().is_none() {
        if let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"SETTINGSDAT") {
            kprintln!("  stick-log   : live settings file \\FREEOS\\SETTINGS.DAT ({} KiB)", size / 1024);
            *SETTINGS.lock() = Some(Target { device: device.clone(), runs, size, flushed: 0, fresh: false, failures: 0 });
        }
    }
    if SHOTS.lock().is_none() {
        if let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"SHOTS   BIN") {
            *SHOTS.lock() = Some(Target { device: device.clone(), runs, size, flushed: 0, fresh: false, failures: 0 });
        }
    }
    if TARGET.lock().is_some() {
        return;
    }
    let Some((runs, size)) = file_runs(&fat, &mut device, dir, b"LOG     TXT") else { return };
    kprintln!("  stick-log   : writing the kernel log to \\FREEOS\\LOG.TXT ({} KiB, {} run(s))", size / 1024, runs.len());
    *TARGET.lock() = Some(Target { device, runs, size, flushed: 0, fresh: true, failures: 0 });
}

/// Сектора файла `name` в каталоге `dir`: цепочка кластеров, слитая в
/// непрерывные куски, и размер.
fn file_runs(fat: &Fat, device: &mut Shared, dir: u32, name: &[u8; 11]) -> Option<(Vec<(u64, u64)>, usize)> {
    let (first, size, _) = fat.find(device, dir, name)?;
    if first < 2 || size == 0 {
        return None;
    }
    let mut runs: Vec<(u64, u64)> = Vec::new();
    let mut cluster = first;
    let mut covered = 0u64;
    let wanted = u64::from(size).div_ceil(SECTOR as u64);
    while covered < wanted {
        let lba = fat.cluster_lba(cluster);
        match runs.last_mut() {
            Some((start, count)) if *start + *count == lba => *count += fat.per_cluster,
            _ => runs.push((lba, fat.per_cluster)),
        }
        covered += fat.per_cluster;
        if covered >= wanted {
            break;
        }
        let next = fat.next(device, cluster)?;
        cluster = next;
    }
    Some((runs, (covered.min(wanted) as usize * SECTOR).min(size as usize)))
}

/// Файл режима экрана для загрузчика на флешке, если он есть.
static DISPLAY: SpinLock<Option<Target>> = SpinLock::new(None);

/// Записать режим экрана для загрузчика на живую флешку (`WxH`). `false` —
/// файла нет или запись не удалась.
pub fn store_display(width: u32, height: u32) -> bool {
    let Some(mut target) = DISPLAY.lock().take() else { return false };
    let text = alloc::format!("{width}x{height}\n");
    let size = target.size;
    let ok = target.write_range(text.as_bytes(), 0, 0, size);
    let _ = target.device.flush();
    *DISPLAY.lock() = Some(target);
    ok
}

/// Файл режима загрузки для загрузчика на флешке, если он есть (24.11).
static BOOT_CFG: SpinLock<Option<Target>> = SpinLock::new(None);

/// Записать режим загрузки на живую флешку (`verbose` — журнал вместо
/// логотипа). `false` — файла нет (флешка старше 24.11) или запись не удалась.
pub fn store_boot_cfg(verbose: bool) -> bool {
    let Some(mut target) = BOOT_CFG.lock().take() else { return false };
    // The rest of the sector goes as spaces: the file keeps its size, the
    // loader reads the first line.
    let word: &[u8] = if verbose { b"verbose\n" } else { b"normal\n" };
    let size = target.size;
    let ok = target.write_range(word, 0, 0, size);
    let _ = target.device.flush();
    *BOOT_CFG.lock() = Some(target);
    ok
}

/// Файл раскладки мониторов на флешке, если он есть.
static LAYOUT: SpinLock<Option<Target>> = SpinLock::new(None);

/// Записать раскладку мониторов на флешку (живая система). `false` — файла
/// нет или запись не удалась.
pub fn store_layout(text: &str) -> bool {
    let Some(mut layout) = LAYOUT.lock().take() else { return false };
    let size = layout.size;
    let ok = layout.write_range(text.as_bytes(), 0, 0, size);
    let _ = layout.device.flush();
    *LAYOUT.lock() = Some(layout);
    ok
}

/// Файл снимков экрана AMD-теста на флешке (`\FREEOS\SHOTS.BIN`), если он есть.
static SHOTS: SpinLock<Option<Target>> = SpinLock::new(None);

/// Размер `\FREEOS\SHOTS.BIN`, если флешка с ним уже найдена.
pub fn shots_size() -> Option<usize> {
    SHOTS.lock().as_ref().map(|target| target.size)
}

/// Переписать кусок `\FREEOS\SHOTS.BIN` с байта `offset` (кратен сектору).
pub fn shots_write(offset: usize, bytes: &[u8]) -> bool {
    let Some(mut target) = SHOTS.lock().take() else { return false };
    let end = offset + bytes.len().div_ceil(SECTOR) * SECTOR;
    let ok = offset % SECTOR == 0 && end <= target.size && target.write_range(bytes, offset, offset, end);
    let _ = target.device.flush();
    *SHOTS.lock() = Some(target);
    ok
}

/// Файл настроек живой системы на флешке, если он есть.
static SETTINGS: SpinLock<Option<Target>> = SpinLock::new(None);

/// Размер `\FREEOS\SETTINGS.DAT`, если флешка с ним уже найдена.
pub fn settings_size() -> Option<usize> {
    SETTINGS.lock().as_ref().map(|target| target.size)
}

/// Прочитать `\FREEOS\SETTINGS.DAT` целиком.
pub fn settings_read() -> Option<Vec<u8>> {
    let mut target = SETTINGS.lock().take()?;
    let bytes = target.read_all();
    *SETTINGS.lock() = Some(target);
    Some(bytes)
}

/// Переписать кусок `\FREEOS\SETTINGS.DAT` с байта `offset` (кратен
/// сектору): `bytes`, хвост последнего сектора — пробелы. Не трогает ни FAT,
/// ни каталог; `false` — файла нет, кусок не влезает или запись не удалась.
pub fn settings_write(offset: usize, bytes: &[u8]) -> bool {
    let Some(mut target) = SETTINGS.lock().take() else { return false };
    let end = offset + bytes.len().div_ceil(SECTOR) * SECTOR;
    let ok = offset % SECTOR == 0 && end <= target.size && target.write_range(bytes, offset, offset, end);
    let _ = target.device.flush();
    *SETTINGS.lock() = Some(target);
    ok
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
    /// Прочитать файл целиком.
    fn read_all(&mut self) -> Vec<u8> {
        let mut out = alloc::vec![0u8; self.size.div_ceil(SECTOR) * SECTOR];
        for index in 0..out.len() / SECTOR {
            let Some(lba) = file_lba(&self.runs, index as u64) else { break };
            if self.device.read(lba, &mut out[index * SECTOR..(index + 1) * SECTOR]).is_err() {
                break;
            }
        }
        out.truncate(self.size);
        out
    }

    /// Записать сектора файла `[from, to)` из `bytes` (недостающее — пробелы);
    /// `bytes[0]` — байт файла номер `origin` (`origin` не больше `from`,
    /// кратен сектору: с 21.11 передаётся только хвост журнала).
    fn write_range(&mut self, bytes: &[u8], origin: usize, from: usize, to: usize) -> bool {
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
            let start = sector * SECTOR - origin;
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

/// How long the frame rate must stay below a quarter of
/// [`BUSY_COMMITS_PER_S`] before the log goes back to 2 s flushes (hw-log14).
const IDLE_BEFORE_2S_MS: u64 = 20_000;

/// Сколько кадров показал сервер Wayland с загрузки (есть только на x86_64).
fn wayland_commits() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        crate::linux::perfdiag::WL_COMMITS.load(core::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        0
    }
}

/// Задача: снимать кольцо журнала в память и дописывать на флешку.
pub fn task() {
    let mut next_flush = 0u64;
    let mut last_sync = 0u64;
    let mut commits = wayland_commits();
    let mut commits_at = crate::time::uptime_ms();
    let mut busy = false;
    // Since when the frame rate has been low while still `busy`.
    let mut quiet_since: Option<u64> = None;
    let mut chunk = alloc::vec![0u8; 4096];
    loop {
        // Снять новое с кольца.
        loop {
            // Позиция общая с `flush_now`: снимать одно и то же дважды нельзя.
            let mut captured = CAPTURED.lock();
            let position = CAPTURED_AT.load(core::sync::atomic::Ordering::Acquire);
            let (count, next) = crate::klog::read(position, &mut chunk);
            CAPTURED_AT.store(next, core::sync::atomic::Ordering::Release);
            if count == 0 {
                break;
            }
            let room = CAPTURE_MAX.saturating_sub(captured.len());
            captured.extend_from_slice(&chunk[..count.min(room)]);
        }
        let now = crate::time::uptime_ms();
        // Идёт ли игра: частота показов кадров Wayland за последние ~2 с.
        if now.saturating_sub(commits_at) >= FLUSH_MS {
            let total = wayland_commits();
            let rate = total.saturating_sub(commits) * 1000 / now.saturating_sub(commits_at).max(1);
            let was = busy;
            // Hysteresis (hw-log14): D2R at 17-21 frames/s flipped the mode
            // 7 times in a few minutes - exactly when the game was weakest,
            // flushes got more frequent. In at once above the threshold; out
            // only after `IDLE_BEFORE_2S_MS` well below it (a quarter).
            if rate > BUSY_COMMITS_PER_S {
                busy = true;
                quiet_since = None;
            } else if busy && rate * 4 > BUSY_COMMITS_PER_S {
                quiet_since = None;
            } else if busy {
                let since = *quiet_since.get_or_insert(now);
                busy = now.saturating_sub(since) < IDLE_BEFORE_2S_MS;
            }
            if busy != was {
                kprintln!(
                    "  stick-log   : {} ({rate} Wayland frames/s)",
                    if busy { "game running - log flushed every 10 s" } else { "log flushed every 2 s again" }
                );
                if busy {
                    next_flush = now + FLUSH_BUSY_MS;
                }
            }
            commits = total;
            commits_at = now;
        }
        if now >= next_flush {
            next_flush = now + if busy { FLUSH_BUSY_MS } else { FLUSH_MS };
            let sync = now.saturating_sub(last_sync) >= SYNC_MS;
            if sync {
                last_sync = now;
            }
            flush(sync);
        }
        crate::sched::sleep_ms(CAPTURE_MS);
    }
}

/// Дописать всё накопленное сейчас — перед выключением и перезагрузкой.
pub fn flush_now() {
    let mut position = CAPTURED_AT.load(core::sync::atomic::Ordering::Acquire);
    let mut chunk = [0u8; 512];
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
    CAPTURED_AT.store(position, core::sync::atomic::Ordering::Release);
    flush(true);
}

/// Дописать новое; `sync` — и SYNCHRONIZE CACHE флешки.
///
/// С 21.11 копируется только хвост журнала с последнего записанного сектора
/// (до этого — клон всех до 4 МиБ каждые 2 с), и только он уходит на флешку.
fn flush(sync: bool) {
    let Some(mut target) = TARGET.lock().take() else { return };
    let mut ok = true;
    if target.fresh {
        // Прошлая загрузка стирается целиком: иначе её хвост читался бы как
        // продолжение этой.
        let bytes = CAPTURED.lock().clone();
        let end = bytes.len().min(target.size);
        ok = target.write_range(&bytes, 0, 0, target.size);
        target.fresh = !ok;
        target.flushed = end;
    } else {
        // Последний неполный сектор переписывается: в нём дописалось.
        let from = target.flushed / SECTOR * SECTOR;
        let tail = {
            let captured = CAPTURED.lock();
            let end = captured.len().min(target.size);
            (end > target.flushed).then(|| captured[from..end].to_vec())
        };
        if let Some(tail) = tail {
            let end = from + tail.len();
            ok = target.write_range(&tail, from, from, end);
            if ok {
                target.flushed = end;
            }
        }
    }
    if ok {
        if sync {
            let _ = target.device.flush();
        }
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
