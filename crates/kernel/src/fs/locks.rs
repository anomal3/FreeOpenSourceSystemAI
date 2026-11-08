//! Рекомендательные блокировки файлов (08.11.2026): одна таблица на всю
//! систему — для родных программ (`SYS_FLOCK`) и для слоя Linux (`fcntl`
//! `F_*LK`, `flock`).
//!
//! Логика диапазонов — крейт `filelocks` (проверяется на хосте). Здесь —
//! замок вокруг таблицы и пробуждение ждущих (`F_SETLKW`, `flock` без
//! `LOCK_NB`): они спят на [`crate::io_events`] и перепроверяют таблицу после
//! каждого снятия.
//!
//! Ключ файла — `Node::file_id()`: том (адрес его `Arc`) и номер inode. Он
//! один у всех, кто открыл файл, под любым именем. У файловых систем без
//! номеров (FAT, initrd) блокировок нет — ответ «не поддерживается».
//!
//! Порядок замков: замок процесса → эта таблица; обратно — никогда, и под
//! этой таблицей ничего не ждут.

use crate::sync::Mutex;

pub use filelocks::{Kind, Lock, Owner, Space, EOF};

/// Ключ файла: том и номер inode.
pub type Key = (usize, u64);

static TABLE: Mutex<filelocks::Table<Key>> = Mutex::new(filelocks::Table::new());

/// Первая чужая блокировка, мешающая взять `kind` на `[start, end)`.
pub fn conflict(key: Key, owner: Owner, space: Space, kind: Kind, start: u64, end: u64) -> Option<Lock> {
    TABLE.lock().conflict(&key, owner, space, kind, start, end)
}

/// Взять или снять (`kind == None`). `Err` — чужая конфликтующая блокировка.
pub fn set(key: Key, owner: Owner, space: Space, kind: Option<Kind>, start: u64, end: u64, pid: i32) -> Result<(), Lock> {
    let result = TABLE.lock().set(&key, owner, space, kind, start, end, pid);
    if result.is_ok() && kind != Some(Kind::Write) {
        // Снятие или понижение до читающей могло освободить кого-то ждущего.
        crate::io_events::notify();
    }
    result
}

/// `close` дескриптора: POSIX-блокировки процесса на этом файле.
pub fn release_file(key: Key, owner: Owner) {
    let mut table = TABLE.lock();
    let before = table.locks(&key).len();
    table.release_file(&key, owner, Some(Space::Record));
    let changed = table.locks(&key).len() != before;
    drop(table);
    if changed {
        crate::io_events::notify();
    }
}

/// Всё, что держал владелец: выход процесса, последняя копия открытого файла.
pub fn release(owner: Owner) {
    let changed = TABLE.lock().release(owner);
    if changed {
        crate::io_events::notify();
    }
}
