//! Настройки живой флешки между перезагрузками (21.11).
//!
//! # Зачем
//!
//! Роман проверяет систему только с живой флешки. Там `/etc` и `/root` —
//! тома в памяти ([`crate::fs::ramvol`]), и всё, что он настроил (громкость,
//! панель и «Пуск», закрепления, тема, масштаб, раскладка мониторов, ярлыки
//! на столе), пропадало с каждой перезагрузкой.
//!
//! # Как
//!
//! На разделе журнала флешки лежит `\FREEOS\SETTINGS.DAT` — 256 КиБ, их
//! кладёт `xtask image`. Ядро переписывает его сектора на месте, как журнал
//! ([`super::stick_log`]): ни FAT, ни каталог, ни размер не меняются.
//!
//! В файле две половины по 128 КиБ — два снимка небольших файлов настроек
//! (заголовок: метка, номер поколения, длина, FNV-1a содержимого). Пишется
//! половина со старым снимком, так что питание, пропавшее посреди записи,
//! оставляет целым предыдущий. При загрузке берётся целая половина с бо́льшим
//! поколением; испорченная (или пробелы новой флешки) молча пропускается.
//!
//! Что входит в снимок (белый список, всё — файлы до 64 КиБ):
//! * `/etc/**` — всё, что изменил человек (`desktop.cfg`, `system.cfg`,
//!   `autostart`, `linux-links`…), кроме файлов только для владельца (`0600`:
//!   пароли Wi-Fi, ключ SSH) — FAT флешки читает любой компьютер;
//! * `/root/.config/*` — настройки программ (Блокнот, «Выполнить», диалог
//!   файлов), без подкаталогов (профиль OBSOS туда не влезет);
//! * `/root/Desktop/*.lnk|.desktop|.url` — ярлыки на столе.
//!
//! Снимок сверяется с записанным раз в 2 с и пишется, когда перестал меняться
//! (ползунок громкости не пишет флешку на каждый шаг), но не реже раза в 10 с
//! при непрерывных изменениях; и всегда — при выключении и перезагрузке
//! ([`save_now`] из [`crate::power`]).
//!
//! Флешка находится позже, чем поднимается стол: восстановленный
//! `desktop.cfg` стол перечитывает и применяет сразу
//! ([`crate::ui::settings_restored`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::kprintln;
use crate::vfs::perm::{Access, Credentials};
use crate::vfs::{NodeKind, VfsError};

const MAGIC: &[u8; 8] = b"FOSSCFG1";
const HEADER: usize = 32;
/// Половина файла под один снимок.
const SLOT: usize = 128 * 1024;
/// Больше — не настройка.
const MAX_FILE: usize = 64 * 1024;
/// Как часто сверять снимок, мс.
const POLL_MS: u64 = 2000;
/// Дольше этого изменения не ждут затишья, мс.
const MAX_DELAY_MS: u64 = 10_000;
/// Дом живой системы: учётных записей нет, работает root.
const HOME: &str = "/root";

/// Попытка восстановить уже сделана (флешка найдена).
static RESTORED: AtomicBool = AtomicBool::new(false);
/// FNV-1a последнего записанного (или восстановленного) снимка.
static SAVED_HASH: AtomicU64 = AtomicU64::new(0);
/// Поколение последнего записанного снимка.
static GENERATION: AtomicU32 = AtomicU32::new(0);
/// Запись идёт (задача или выключение) — вторая ждёт.
static BUSY: AtomicBool = AtomicBool::new(false);

struct Entry {
    path: String,
    mode: u16,
    data: Vec<u8>,
}

fn fnv(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Путь из белого списка (и при чтении снимка: испорченный или чужой путь
/// не восстанавливается).
fn allowed(path: &str) -> bool {
    if path.contains("/../") || path.contains("/./") || path.ends_with("/..") || path.contains("//") {
        return false;
    }
    if let Some(rest) = path.strip_prefix("/etc/") {
        return !rest.is_empty();
    }
    if let Some(name) = path.strip_prefix("/root/.config/") {
        return !name.is_empty() && !name.contains('/');
    }
    if let Some(name) = path.strip_prefix("/root/Desktop/") {
        let lower = name.to_ascii_lowercase();
        return !name.contains('/') && (lower.ends_with(".lnk") || lower.ends_with(".desktop") || lower.ends_with(".url"));
    }
    false
}

/// Файлы каталога `dir` в снимок; `depth` — сколько ещё уровней вглубь.
fn collect_dir(dir: &str, depth: u32, out: &mut Vec<Entry>) {
    let Some(Ok(list)) = crate::fs::list(dir) else { return };
    for entry in list {
        if entry.name == "." || entry.name == ".." || entry.name == "lost+found" {
            continue;
        }
        let path = format!("{dir}/{}", entry.name);
        match entry.kind {
            NodeKind::Directory if depth > 0 => collect_dir(&path, depth - 1, out),
            NodeKind::File if (entry.size as usize) <= MAX_FILE && entry.mode & 0o077 != 0 && allowed(&path) => {
                if let Some(Ok((data, _))) = crate::fs::read(&path, MAX_FILE) {
                    out.push(Entry { path, mode: entry.mode & 0o777, data });
                }
            }
            _ => {}
        }
    }
}

/// Снимок: файлы белого списка, по порядку путей.
fn collect() -> Vec<Entry> {
    let mut out = Vec::new();
    collect_dir("/etc", 3, &mut out);
    collect_dir(&format!("{HOME}/.config"), 0, &mut out);
    collect_dir(&format!("{HOME}/Desktop"), 0, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Содержимое снимка (без заголовка) и число файлов; что не влезло в
/// половину — пропущено (`log` — сказать об этом в журнал).
fn encode(entries: &[Entry], log: bool) -> (Vec<u8>, usize) {
    let mut payload = Vec::new();
    let mut count = 0;
    for entry in entries {
        let size = 8 + entry.path.len() + entry.data.len();
        if HEADER + payload.len() + size > SLOT || entry.path.len() > 1024 {
            if log {
                kprintln!("  stick-cfg   : {} left out ({} bytes, the settings file is full)", entry.path, entry.data.len());
            }
            continue;
        }
        payload.extend_from_slice(&(entry.path.len() as u16).to_le_bytes());
        payload.extend_from_slice(&entry.mode.to_le_bytes());
        payload.extend_from_slice(&(entry.data.len() as u32).to_le_bytes());
        payload.extend_from_slice(entry.path.as_bytes());
        payload.extend_from_slice(&entry.data);
        count += 1;
    }
    (payload, count)
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Разобрать половину файла: поколение, хеш и файлы. `None` — нет снимка
/// или он испорчен.
fn decode(slot: &[u8]) -> Option<(u32, Vec<Entry>)> {
    if slot.len() < HEADER || &slot[..8] != MAGIC {
        return None;
    }
    let generation = le32(slot, 8);
    let len = le32(slot, 12) as usize;
    let hash = u64::from_le_bytes(slot[16..24].try_into().ok()?);
    let count = le32(slot, 24) as usize;
    let payload = slot.get(HEADER..HEADER.checked_add(len)?)?;
    if fnv(payload) != hash {
        return None;
    }
    let mut out = Vec::new();
    let mut at = 0;
    while at < payload.len() {
        let head = payload.get(at..at + 8)?;
        let path_len = usize::from(le16(head, 0));
        let mode = le16(head, 2);
        let data_len = le32(head, 4) as usize;
        let path = core::str::from_utf8(payload.get(at + 8..at + 8 + path_len)?).ok()?;
        let data = payload.get(at + 8 + path_len..at + 8 + path_len + data_len)?;
        at += 8 + path_len + data_len;
        if allowed(path) && data.len() <= MAX_FILE {
            out.push(Entry { path: String::from(path), mode: mode & 0o777, data: data.to_vec() });
        }
    }
    (out.len() == count).then_some((generation, out))
}

/// Создать каталоги до файла `path`.
fn make_parents(path: &str) {
    let mut at = 1;
    while let Some(next) = path[at..].find('/') {
        let dir = &path[..at + next];
        let _ = crate::fs::mkdir_as(Credentials::ROOT, dir, 0o755);
        at += next + 1;
    }
}

/// Записать файл целиком (как `config::write`: усечь или создать).
fn write_file(entry: &Entry) -> Result<(), VfsError> {
    make_parents(&entry.path);
    let mode = if entry.mode == 0 { 0o644 } else { entry.mode };
    let node = match crate::fs::resolve_as(Credentials::ROOT, &entry.path, Access::WRITE) {
        Some(Ok(node)) => {
            node.truncate(0)?;
            node
        }
        Some(Err(VfsError::NotFound)) => match crate::fs::create_as(Credentials::ROOT, &entry.path, mode) {
            Some(result) => result?,
            None => return Err(VfsError::NotFound),
        },
        Some(Err(err)) => return Err(err),
        None => return Err(VfsError::NotFound),
    };
    node.write_at(0, &entry.data)?;
    Ok(())
}

/// Флешка с файлом настроек нашлась: вернуть снимок в `/etc` и `/root`.
fn restore() {
    let Some(bytes) = super::stick_log::settings_read() else { return };
    let best = [0usize, SLOT]
        .iter()
        .filter_map(|&offset| bytes.get(offset..(offset + SLOT).min(bytes.len())).and_then(decode))
        .max_by_key(|(generation, _)| *generation);
    let Some((generation, entries)) = best else {
        kprintln!("  stick-cfg   : no saved settings on the stick yet");
        return;
    };
    GENERATION.store(generation, Ordering::Relaxed);
    let mut written = 0;
    let mut size = 0;
    for entry in &entries {
        match write_file(entry) {
            Ok(()) => {
                written += 1;
                size += entry.data.len();
            }
            Err(err) => kprintln!("  stick-cfg   : {} not restored: {err}", entry.path),
        }
    }
    kprintln!(
        "  stick-cfg   : restored {written} file(s), {} KiB, generation {generation} from \\FREEOS\\SETTINGS.DAT",
        size.div_ceil(1024)
    );
    for entry in &entries {
        kprintln!("  stick-cfg   :   {} ({} bytes)", entry.path, entry.data.len());
    }
    // То, что читается один раз при загрузке, — заново.
    if entries.iter().any(|entry| entry.path == "/etc/system.cfg") {
        crate::time::adopt_timezone();
        crate::input::keymap::adopt();
    }
    crate::ui::settings_restored();
}

/// Записать снимок, если он отличается от записанного. `true` — записан.
fn save(entries: &[Entry]) -> bool {
    let (payload, count) = encode(entries, true);
    let hash = fnv(&payload);
    if hash == SAVED_HASH.load(Ordering::Relaxed) {
        return false;
    }
    let generation = GENERATION.load(Ordering::Relaxed).wrapping_add(1);
    let mut slot = Vec::with_capacity(HEADER + payload.len());
    slot.extend_from_slice(MAGIC);
    slot.extend_from_slice(&generation.to_le_bytes());
    slot.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    slot.extend_from_slice(&hash.to_le_bytes());
    slot.extend_from_slice(&(count as u32).to_le_bytes());
    slot.resize(HEADER, 0);
    slot.extend_from_slice(&payload);
    let offset = (generation as usize % 2) * SLOT;
    if super::stick_log::settings_write(offset, &slot) {
        GENERATION.store(generation, Ordering::Relaxed);
        SAVED_HASH.store(hash, Ordering::Relaxed);
        kprintln!(
            "  stick-cfg   : saved {count} file(s), {} KiB, generation {generation} to \\FREEOS\\SETTINGS.DAT",
            slot.len().div_ceil(1024)
        );
        true
    } else {
        kprintln!("  stick-cfg   : could not write \\FREEOS\\SETTINGS.DAT");
        // Не повторять ту же неудачу каждые 2 с: до следующего изменения.
        SAVED_HASH.store(hash, Ordering::Relaxed);
        false
    }
}

/// Хеш текущего снимка — тот же, что у [`save`].
fn snapshot_hash(entries: &[Entry]) -> u64 {
    fnv(&encode(entries, false).0)
}

fn lock() -> bool {
    BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).is_ok()
}

fn unlock() {
    BUSY.store(false, Ordering::Release);
}

/// Задача живой системы: дождаться флешки, восстановить, дальше сохранять.
pub fn task() {
    // Изменение, ждущее затишья: (хеш, когда замечено впервые).
    let mut pending: Option<(u64, u64)> = None;
    loop {
        // Пока флешки нет — чаще: стол уже стоит с настройками по умолчанию.
        let restored = RESTORED.load(Ordering::Acquire);
        crate::sched::sleep_ms(if restored { POLL_MS } else { 250 });
        if !restored {
            if super::stick_log::settings_size().is_none() || !lock() {
                continue;
            }
            restore();
            // Отправная точка — то, что лежит сейчас (с восстановленным).
            SAVED_HASH.store(snapshot_hash(&collect()), Ordering::Relaxed);
            RESTORED.store(true, Ordering::Release);
            unlock();
            continue;
        }
        let entries = collect();
        let hash = snapshot_hash(&entries);
        let now = crate::time::uptime_ms();
        if hash == SAVED_HASH.load(Ordering::Relaxed) {
            pending = None;
            continue;
        }
        let due = match pending {
            // Не менялось с прошлой сверки — или меняется слишком долго.
            Some((seen, since)) => seen == hash || now.saturating_sub(since) >= MAX_DELAY_MS,
            None => false,
        };
        if !due {
            let since = pending.map_or(now, |(_, since)| since);
            pending = Some((hash, since));
            continue;
        }
        if lock() {
            save(&entries);
            unlock();
            pending = None;
        }
    }
}

/// Записать изменения сейчас — перед выключением и перезагрузкой. Ничего не
/// делает, пока флешка не найдена (иначе пустые настройки затёрли бы
/// сохранённые).
pub fn save_now() {
    if !RESTORED.load(Ordering::Acquire) {
        return;
    }
    // Задача может как раз писать — подождать её, но не вечно.
    let mut tries = 0;
    while !lock() {
        tries += 1;
        if tries > 100 {
            return;
        }
        crate::sched::sleep_ms(20);
    }
    save(&collect());
    unlock();
}
