//! Файловая служба модема — CCCI FS (05.11), вместо `ccci_fsd` Android.
//!
//! # Зачем
//!
//! Модем при загрузке читает свой NVRAM (калибровка радио, IMEI, настройки)
//! через AP: запросы по каналу 14, ответы по 15, очередь 4. На Android их
//! обслуживает закрытая программа `ccci_fsd`; протокол взят из её исходника
//! v2.3 (зеркало `Komodo-begonia/vendor_mediatek-opensource`, ветка 12,
//! `external/ccci_fsd/ccci_fsd.c`, `ccci_fsd.h`).
//!
//! # Как
//!
//! Файлы — в памяти: при первом запросе грузятся из RAM-диска
//! `/lib/modem/fs/<диск>/…` (туда `xtask phone` кладёт распакованные разделы
//! `nvdata`, `protect1`, `protect2` этого аппарата), запись меняет только
//! память — до eMMC-записи изменения модема не переживают перезагрузку.
//! Диски: `Z:` nvdata/md, `X:` protect_f/md, `Y:` protect_s/md, прочие
//! (`W V U T S`) — пустые.
//!
//! Пакет: заголовок CCCI (`reserved` — номер слота 0..4, бит 31 `data0` —
//! «будет продолжение»), `u32 op`, `u32 para_num`, затем {`u32 len`, данные с
//! выравниванием до 4}. Ответ — `op | 0xFFFF0000` и параметры так же; длинный
//! ответ режется на куски по 3456 байт.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;

const RESP: u32 = 0xFFFF_0000;
const SEND_AGAIN: u32 = 0x8000_0000;
const MAX_PKT: usize = 3456;

const NO_ERROR: i32 = 0;
const PARAM_ERROR: i32 = -2;
const DRIVE_NOT_FOUND: i32 = -4;
const TOO_MANY_FILES: i32 = -5;
const NO_MORE_FILES: i32 = -6;
const FILE_NOT_FOUND: i32 = -9;
const INVALID_HANDLE: i32 = -10;
const UNSUPPORTED: i32 = -12;
const ACCESS_DENIED: i32 = -16;
const GENERAL_FAILURE: i32 = -18;
const PATH_NOT_FOUND: i32 = -19;

const READ_ONLY: u32 = 0x100;
const CREATE: u32 = 0x1_0000;
const CREATE_ALWAYS: u32 = 0x2_0000;

const DRIVES: [char; 8] = ['Z', 'X', 'Y', 'W', 'V', 'U', 'T', 'S'];

struct Node {
    /// Имя последнего звена, как его записали (регистр сохраняется).
    name: String,
    dir: bool,
    data: Vec<u8>,
}

enum Handle {
    File { key: String, pos: usize },
    Find { names: Vec<(String, bool, usize)>, at: usize },
}

struct Fs {
    loaded: bool,
    /// Ключ — путь в нижнем регистре: `z:/nvram/nvd_data/ld0b_001`.
    nodes: BTreeMap<String, Node>,
    handles: Vec<Option<Handle>>,
    /// Недособранные запросы по слотам (бит «продолжение»).
    partial: [Vec<u8>; 5],
    stats: (u32, u32),
}

static FS: SpinLock<Fs> = SpinLock::new(Fs {
    loaded: false,
    nodes: BTreeMap::new(),
    handles: Vec::new(),
    partial: [Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new()],
    stats: (0, 0),
});

fn le32(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
}

/// Загрузить файлы из `/lib/modem/fs/<диск>` в память (один раз).
fn load(fs: &mut Fs) {
    fs.loaded = true;
    fs.handles = (0..129).map(|_| None).collect();
    let mut count = 0;
    let mut bytes = 0;
    for drive in DRIVES {
        let root = format!("{}:", drive.to_ascii_lowercase());
        fs.nodes.insert(root.clone(), Node { name: root.clone(), dir: true, data: Vec::new() });
        let mut stack = alloc::vec![(format!("/lib/modem/fs/{drive}"), root)];
        while let Some((path, key)) = stack.pop() {
            let Some(Ok(entries)) = crate::fs::list(&path) else { continue };
            for entry in entries {
                if entry.name == "." || entry.name == ".." {
                    continue;
                }
                let child_path = format!("{path}/{}", entry.name);
                let child_key = format!("{key}/{}", entry.name.to_ascii_lowercase());
                if entry.kind == crate::vfs::NodeKind::Directory {
                    fs.nodes.insert(child_key.clone(), Node { name: entry.name.clone(), dir: true, data: Vec::new() });
                    stack.push((child_path, child_key));
                } else if let Some(Ok((data, _))) = crate::fs::read(&child_path, 4 << 20) {
                    bytes += data.len();
                    count += 1;
                    fs.nodes.insert(child_key, Node { name: entry.name.clone(), dir: false, data });
                }
            }
        }
    }
    crate::kprintln!("  ccci fs     : {count} modem files ({} KiB) from /lib/modem/fs", bytes / 1024);
}

/// Путь модема (UTF-16LE, `Z:\NVRAM\…`) → ключ. `Err` — код ошибки FS.
fn key_of(wide: &[u8]) -> Result<String, i32> {
    let units: Vec<u16> =
        wide.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
    let text: String = char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect();
    let text = text.replace('\\', "/");
    if text.contains("..") {
        return Err(ACCESS_DENIED);
    }
    let mut chars = text.chars();
    let (Some(drive), Some(':')) = (chars.next(), chars.next()) else {
        return Err(PATH_NOT_FOUND);
    };
    if !DRIVES.contains(&drive.to_ascii_uppercase()) {
        return Err(PATH_NOT_FOUND);
    }
    let rest: String = chars.collect();
    let mut key = format!("{}:", drive.to_ascii_lowercase());
    for part in rest.split('/').filter(|p| !p.is_empty()) {
        key.push('/');
        key.push_str(&part.to_ascii_lowercase());
    }
    Ok(key)
}

fn name_of(wide: &[u8]) -> String {
    let units: Vec<u16> =
        wide.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
    let text: String = char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect();
    text.replace('\\', "/").rsplit('/').next().unwrap_or("").into()
}

fn parent_of(key: &str) -> &str {
    key.rfind('/').map_or(key, |at| &key[..at])
}

fn wide(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for unit in text.encode_utf16().chain(core::iter::once(0)) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

fn int(value: i32) -> Vec<u8> {
    value.to_le_bytes().to_vec()
}

/// Маска `*`/`?` без учёта регистра.
fn matches(pattern: &[u8], name: &[u8]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (Some(b'*'), _) => matches(&pattern[1..], name) || (!name.is_empty() && matches(pattern, &name[1..])),
        (Some(b'?'), Some(_)) => matches(&pattern[1..], &name[1..]),
        (Some(p), Some(n)) if p.eq_ignore_ascii_case(n) => matches(&pattern[1..], &name[1..]),
        _ => false,
    }
}

fn dos_entry(dir: bool, size: usize, name_fits: bool) -> Vec<u8> {
    let mut e = alloc::vec![0u8; 52];
    e[11] = if dir { 0x10 } else { 0 };
    e[12] = u8::from(name_fits);
    e[28..32].copy_from_slice(&(size as u32).to_le_bytes());
    e
}

fn alloc_handle(fs: &mut Fs, handle: Handle) -> i32 {
    for (i, slot) in fs.handles.iter_mut().enumerate().skip(1) {
        if slot.is_none() {
            *slot = Some(handle);
            return i as i32;
        }
    }
    TOO_MANY_FILES
}

fn open(fs: &mut Fs, path: &[u8], flags: u32) -> i32 {
    let key = match key_of(path) {
        Ok(key) => key,
        Err(err) => return err,
    };
    match fs.nodes.get_mut(&key) {
        // Каталог открывается, как `open()` Linux у `ccci_fsd`: модем так
        // проверяет, есть ли `Z:\NVRAM` (на -16 он падал в assert,
        // `nvram_drval_fat.c`). Чтение из такого дескриптора — пусто.
        Some(node) if node.dir => {}
        Some(node) => {
            if flags & CREATE_ALWAYS != 0 && flags & READ_ONLY == 0 {
                node.data.clear();
            }
        }
        None => {
            if flags & (CREATE | CREATE_ALWAYS) == 0 || flags & READ_ONLY != 0 {
                return FILE_NOT_FOUND;
            }
            if !fs.nodes.get(parent_of(&key)).is_some_and(|p| p.dir) {
                return PATH_NOT_FOUND;
            }
            fs.nodes.insert(key.clone(), Node { name: name_of(path), dir: false, data: Vec::new() });
        }
    }
    alloc_handle(fs, Handle::File { key, pos: 0 })
}

fn file(fs: &mut Fs, handle: u32) -> Option<(String, usize)> {
    match fs.handles.get(handle as usize)? {
        Some(Handle::File { key, pos }) => Some((key.clone(), *pos)),
        _ => None,
    }
}

fn set_pos(fs: &mut Fs, handle: u32, new: usize) {
    if let Some(Some(Handle::File { pos, .. })) = fs.handles.get_mut(handle as usize) {
        *pos = new;
    }
}

fn read(fs: &mut Fs, handle: u32, len: usize) -> (i32, Vec<u8>) {
    let Some((key, pos)) = file(fs, handle) else { return (INVALID_HANDLE, Vec::new()) };
    let Some(node) = fs.nodes.get(&key) else { return (FILE_NOT_FOUND, Vec::new()) };
    let end = (pos + len).min(node.data.len());
    let data = node.data.get(pos..end).unwrap_or(&[]).to_vec();
    set_pos(fs, handle, pos + data.len());
    (NO_ERROR, data)
}

fn write(fs: &mut Fs, handle: u32, data: &[u8]) -> (i32, usize) {
    let Some((key, pos)) = file(fs, handle) else { return (INVALID_HANDLE, 0) };
    let Some(node) = fs.nodes.get_mut(&key) else { return (FILE_NOT_FOUND, 0) };
    if node.data.len() < pos + data.len() {
        node.data.resize(pos + data.len(), 0);
    }
    node.data[pos..pos + data.len()].copy_from_slice(data);
    set_pos(fs, handle, pos + data.len());
    (NO_ERROR, data.len())
}

fn seek(fs: &mut Fs, handle: u32, offset: i32, whence: u32) -> i32 {
    let Some((key, pos)) = file(fs, handle) else { return INVALID_HANDLE };
    let size = fs.nodes.get(&key).map_or(0, |n| n.data.len()) as i64;
    let base = match whence {
        0 => 0,
        1 => pos as i64,
        _ => size,
    };
    let new = base + i64::from(offset);
    if new < 0 {
        return -15; // INVALID_FILE_POS
    }
    set_pos(fs, handle, new as usize);
    new as i32
}

fn close(fs: &mut Fs, handle: u32) -> i32 {
    match fs.handles.get_mut(handle as usize) {
        Some(slot @ Some(_)) => {
            *slot = None;
            NO_ERROR
        }
        _ => INVALID_HANDLE,
    }
}

fn find_first(fs: &mut Fs, pattern: &[u8], attr: u32, mask: u32, max: u32) -> Vec<Vec<u8>> {
    let key = match key_of(pattern) {
        Ok(key) => key,
        Err(err) => return alloc::vec![int(err)],
    };
    let dir = String::from(parent_of(&key));
    let mask_text = key[dir.len()..].trim_start_matches('/').as_bytes().to_vec();
    let prefix = format!("{dir}/");
    let mut names = Vec::new();
    for (k, node) in fs.nodes.range(prefix.clone()..) {
        if !k.starts_with(&prefix) {
            break;
        }
        if k[prefix.len()..].contains('/') {
            continue;
        }
        let a = if node.dir { 0x10u32 } else { 0 };
        if !matches(&mask_text, k[prefix.len()..].as_bytes()) || (a & attr) != attr || (a & mask) != 0 {
            continue;
        }
        names.push((node.name.clone(), node.dir, node.data.len()));
    }
    if names.is_empty() {
        return alloc::vec![int(NO_MORE_FILES)];
    }
    let first = names[0].clone();
    let handle = alloc_handle(fs, Handle::Find { names, at: 1 });
    if handle < 0 {
        return alloc::vec![int(handle)];
    }
    entry_reply(handle, &first, max)
}

fn entry_reply(ret: i32, entry: &(String, bool, usize), max: u32) -> Vec<Vec<u8>> {
    let fits = entry.0.encode_utf16().count() < max as usize;
    let name = if fits { wide(&entry.0) } else { Vec::new() };
    alloc::vec![int(ret), dos_entry(entry.1, entry.2, fits), name]
}

fn find_next(fs: &mut Fs, handle: u32, max: u32) -> Vec<Vec<u8>> {
    let Some(Some(Handle::Find { names, at })) = fs.handles.get_mut(handle as usize) else {
        return alloc::vec![int(INVALID_HANDLE)];
    };
    let Some(entry) = names.get(*at).cloned() else { return alloc::vec![int(NO_MORE_FILES)] };
    *at += 1;
    entry_reply(NO_ERROR, &entry, max)
}

/// Выполнить запрос; вернуть параметры ответа.
fn serve(fs: &mut Fs, op: u32, p: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let u = |i: usize| p.get(i).map_or(0, |v| le32(v, 0));
    let s = |i: usize| p.get(i).map_or(&[][..], |v| &v[..]);
    match op {
        0x1001 | 0x1011 => {
            let h = open(fs, s(0), u(1));
            if op == 0x1011 { alloc::vec![int(h), s(2).to_vec()] } else { alloc::vec![int(h)] }
        }
        0x1002 => alloc::vec![int(seek(fs, u(0), u(1) as i32, u(2)))],
        0x1003 => {
            let (ret, data) = read(fs, u(0), u(1) as usize);
            alloc::vec![int(ret), int(data.len() as i32), data]
        }
        0x1004 => {
            let n = (u(2) as usize).min(s(1).len());
            let (ret, done) = write(fs, u(0), &s(1)[..n]);
            alloc::vec![int(ret), int(done as i32)]
        }
        0x1005 => alloc::vec![int(close(fs, u(0)))],
        0x1006 => {
            for slot in &mut fs.handles {
                *slot = None;
            }
            alloc::vec![int(NO_ERROR)]
        }
        0x1007 => alloc::vec![int(match key_of(s(0)) {
            Ok(key) if fs.nodes.contains_key(&key) => GENERAL_FAILURE, // EEXIST
            Ok(key) if fs.nodes.get(parent_of(&key)).is_some_and(|n| n.dir) => {
                fs.nodes.insert(key, Node { name: name_of(s(0)), dir: true, data: Vec::new() });
                NO_ERROR
            }
            Ok(_) => FILE_NOT_FOUND,
            Err(err) => err,
        })],
        0x1008 | 0x100F | 0x1018 => alloc::vec![int(match key_of(s(0)) {
            Ok(key) => match fs.nodes.get(&key) {
                Some(n) if (op == 0x100F) == n.dir => ACCESS_DENIED,
                Some(_) => {
                    let prefix = format!("{key}/");
                    if op == 0x1018 {
                        fs.nodes.retain(|k, _| !k.starts_with(&prefix));
                    } else if fs.nodes.keys().any(|k| k.starts_with(&prefix)) {
                        return alloc::vec![int(GENERAL_FAILURE)];
                    }
                    fs.nodes.remove(&key);
                    NO_ERROR
                }
                None => FILE_NOT_FOUND,
            },
            Err(err) => err,
        })],
        0x1009 => match file(fs, u(0)) {
            Some((key, _)) => alloc::vec![int(NO_ERROR), int(fs.nodes.get(&key).map_or(0, |n| n.data.len()) as i32)],
            None => alloc::vec![int(INVALID_HANDLE), int(0)],
        },
        0x100A | 0x100D => alloc::vec![int(match key_of(s(0)) {
            Ok(key) => {
                let prefix = format!("{key}/");
                let it = fs.nodes.iter().filter(|(k, _)| k.starts_with(&prefix));
                if op == 0x100A { it.map(|(_, n)| n.data.len() as i32).sum() } else { it.count() as i32 }
            }
            Err(err) => err,
        })],
        0x100B | 0x100C => alloc::vec![int(match (key_of(s(0)), key_of(s(1))) {
            (Ok(from), Ok(to)) => match fs.nodes.remove(&from) {
                Some(mut node) => {
                    node.name = name_of(s(1));
                    let prefix = format!("{from}/");
                    let moved: Vec<String> = fs.nodes.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
                    for k in moved {
                        if let Some(n) = fs.nodes.remove(&k) {
                            fs.nodes.insert(format!("{to}{}", &k[from.len()..]), n);
                        }
                    }
                    fs.nodes.insert(to, node);
                    NO_ERROR
                }
                None => FILE_NOT_FOUND,
            },
            (Err(err), _) | (_, Err(err)) => err,
        })],
        0x100E => {
            // FS_DiskInfo: 84 байта, как у ccci_fsd — 512 Б/сектор, 8 в
            // кластере, кластеров 16384 (64 МиБ), свободно 512.
            let mut info = alloc::vec![0u8; 84];
            let words: [(usize, u32); 4] = [(48, 512), (52, 8), (56, 16384), (64, 512)];
            for (at, value) in words {
                info[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            alloc::vec![int(NO_ERROR), info]
        }
        0x1010 => alloc::vec![int(match key_of(s(0)) {
            Ok(key) => fs.nodes.get(&key).map_or(FILE_NOT_FOUND, |n| if n.dir { 0x10 } else { 0 }),
            Err(err) => err,
        })],
        0x1012 => find_first(fs, s(0), u(1) & 0xff, u(2), u(3)),
        0x1013 => find_next(fs, u(0), u(1)),
        0x1014 => alloc::vec![int(close(fs, u(0)))],
        0x1015 | 0x1019 | 0x101C => alloc::vec![int(NO_ERROR)],
        0x1016 => alloc::vec![int(1)],
        0x1017 => {
            for slot in &mut fs.handles {
                *slot = None;
            }
            Vec::new()
        }
        0x101A => alloc::vec![int(if u(0) == 4 && (1..=2).contains(&u(1)) { 0x5A } else { PARAM_ERROR })],
        0x101B => alloc::vec![int(if DRIVES.contains(&char::from(u(0) as u8)) { 4096 } else { DRIVE_NOT_FOUND })],
        0x101D | 0x1020 => alloc::vec![int(UNSUPPORTED)],
        0x101E => alloc::vec![int(UNSUPPORTED), Vec::new()],
        0x101F => alloc::vec![int(UNSUPPORTED), int(0)],
        0x1021 => alloc::vec![int(FILE_NOT_FOUND)],
        0x1023 => alloc::vec![int(PARAM_ERROR)],
        0x1022 => cmpt_read(fs, s(0), s(1)),
        0x1024 => cmpt_write(fs, s(0), s(1), s(2)),
        0x1025 => match key_of(s(0)) {
            Ok(key) if fs.nodes.contains_key(&key) => alloc::vec![int(NO_ERROR), alloc::vec![0u8; 24]],
            Ok(_) => alloc::vec![int(FILE_NOT_FOUND), alloc::vec![0u8; 24]],
            Err(err) => alloc::vec![int(err), alloc::vec![0u8; 24]],
        },
        _ => alloc::vec![int(PARAM_ERROR)],
    }
}

/// `CMPT_READ`: открыть [, размер] [, сдвиг] [, прочитать] [, закрыть] за раз.
fn cmpt_read(fs: &mut Fs, path: &[u8], para: &[u8]) -> Vec<Vec<u8>> {
    let map = le32(para, 0);
    let flag = le32(para, 12);
    let (offset, whence, length) = (le32(para, 20) as i32, le32(para, 24), le32(para, 32) as usize);
    let mut done = 0u32;
    let mut size = 0i32;
    let mut data = Vec::new();
    let mut fail = |done: u32, err: i32| -> (u32, i32) { (done, err) };
    let handle = open(fs, path, flag);
    let result = if handle < 0 {
        fail(0, handle)
    } else {
        done |= 1;
        let h = handle as u32;
        let mut err = 0;
        if map & 2 != 0 {
            size = file(fs, h).and_then(|(k, _)| fs.nodes.get(&k)).map_or(0, |n| n.data.len() as i32);
            done |= 2;
        }
        if err == 0 && map & 4 != 0 {
            let r = seek(fs, h, offset, whence);
            if r < 0 { err = r } else { done |= 4 }
        }
        if err == 0 && map & 8 != 0 {
            let (r, d) = read(fs, h, length);
            if r < 0 { err = r } else { data = d; done |= 8 }
        }
        close(fs, h);
        if err == 0 && map & 16 != 0 {
            done |= 16;
        }
        (done, err)
    };
    let mut rets = Vec::new();
    rets.extend_from_slice(&result.0.to_le_bytes());
    rets.extend_from_slice(&result.1.to_le_bytes());
    let len = data.len() as i32;
    alloc::vec![rets, int(size), int(len), data]
}

/// `CMPT_WRITE`: открыть (если `file_handle < 0`) [, сдвиг] , записать [, закрыть].
fn cmpt_write(fs: &mut Fs, path: &[u8], para: &[u8], data: &[u8]) -> Vec<Vec<u8>> {
    let map = le32(para, 0);
    let flag = le32(para, 12);
    let (offset, whence, length) = (le32(para, 16) as i32, le32(para, 20), le32(para, 28) as usize);
    let given = le32(para, 32) as i32;
    let mut done = 0u32;
    let mut err = 0;
    let mut written = 0usize;
    let handle = if given < 0 || map & 1 != 0 { open(fs, path, flag) } else { given };
    if handle < 0 {
        err = handle;
    } else {
        if map & 1 != 0 {
            done |= 1;
        }
        let h = handle as u32;
        if map & 4 != 0 {
            let r = seek(fs, h, offset, whence);
            if r < 0 { err = r } else { done |= 4 }
        }
        if err == 0 && map & 32 != 0 {
            let (r, n) = write(fs, h, &data[..length.min(data.len())]);
            if r < 0 { err = r } else { written = n; done |= 32 }
        }
        if map & 16 != 0 || err != 0 {
            close(fs, h);
            if err == 0 {
                done |= 16;
            }
        }
    }
    let mut rets = Vec::new();
    rets.extend_from_slice(&done.to_le_bytes());
    rets.extend_from_slice(&err.to_le_bytes());
    alloc::vec![rets, int(handle), int(written as i32)]
}

/// Принять пакет канала 14 (с заголовком CCCI); вернуть пакеты ответа без
/// заголовка: (`data0`, слот, полезная нагрузка с `op`). Пусто — запрос ещё
/// не собран целиком.
pub fn handle(packet: &[u8]) -> (Vec<(u32, u32, Vec<u8>)>, String) {
    let data0 = le32(packet, 0);
    let slot = le32(packet, 12) as usize;
    let mut fs = FS.lock();
    if !fs.loaded {
        load(&mut fs);
    }
    if slot >= 5 {
        return (Vec::new(), format!("fs: bad slot {slot}"));
    }
    // Тело без заголовка: `op` и параметры; куски склеиваются (у второго и
    // дальше свой `op` повторён — он отбрасывается).
    let body = packet.get(16..).unwrap_or(&[]);
    if fs.partial[slot].is_empty() {
        fs.partial[slot].extend_from_slice(body);
    } else {
        fs.partial[slot].extend_from_slice(body.get(4..).unwrap_or(&[]));
    }
    if data0 & SEND_AGAIN != 0 {
        return (Vec::new(), String::new());
    }
    let request = core::mem::take(&mut fs.partial[slot]);
    let op = le32(&request, 0);
    let count = le32(&request, 4) as usize;
    let mut params = Vec::new();
    let mut at = 8;
    let mut bad = count > 6;
    for _ in 0..count.min(6) {
        let len = le32(&request, at) as usize;
        at += 4;
        match request.get(at..at + len) {
            Some(data) if len > 0 => params.push(data.to_vec()),
            _ => {
                bad = true;
                break;
            }
        }
        at += (len + 3) & !3;
    }
    let reply = if bad { alloc::vec![int(PARAM_ERROR)] } else { serve(&mut fs, op, &params) };
    fs.stats.0 += 1;
    if reply.first().is_some_and(|r| le32(r, 0) as i32 >= 0) {
        fs.stats.1 += 1;
    }
    let note = describe(op, &params, &reply);
    drop(fs);

    // Ответ: op | RESP, para_num, {len, данные+выравнивание}; длинный — кусками.
    let mut body = Vec::new();
    body.extend_from_slice(&(reply.len() as u32).to_le_bytes());
    for p in &reply {
        body.extend_from_slice(&(p.len() as u32).to_le_bytes());
        body.extend_from_slice(p);
        while body.len() % 4 != 0 {
            body.push(0);
        }
    }
    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = if body.is_empty() { alloc::vec![&body[..]] } else { body.chunks(MAX_PKT).collect() };
    let last = chunks.len() - 1;
    for (i, chunk) in chunks.iter().enumerate() {
        let mut payload = Vec::with_capacity(4 + chunk.len());
        payload.extend_from_slice(&(op | RESP).to_le_bytes());
        payload.extend_from_slice(chunk);
        out.push((if i < last { SEND_AGAIN } else { 0 }, slot as u32, payload));
    }
    (out, note)
}

fn describe(op: u32, params: &[Vec<u8>], reply: &[Vec<u8>]) -> String {
    let path = params
        .first()
        .filter(|p| p.len() > 4 && p.len() % 2 == 0 && p[1] == 0)
        .map(|p| {
            let units: Vec<u16> =
                p.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|u| *u != 0).collect();
            char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect::<String>()
        })
        .unwrap_or_default();
    let ret = reply.first().map_or(0, |r| le32(r, 0) as i32);
    format!("fs {op:#x} {path} -> {ret}")
}

/// Счётчики: запросов всего, удачных.
pub fn stats() -> (u32, u32) {
    FS.lock().stats
}
