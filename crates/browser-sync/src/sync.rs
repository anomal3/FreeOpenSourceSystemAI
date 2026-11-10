//! Синхронизация локальной реплики с площадкой ([`crate::remote`]).
//!
//! Реплика всегда работает с локальной папкой (без сети браузер помнит всё),
//! а синхронизация:
//! 1. отправляет свои файлы `devices/<я>/…`, изменившиеся с прошлого раза;
//! 2. забирает чужие `devices/<другое>/…`, чья версия на площадке сменилась;
//! 3. по желанию отмечает состояние точкой синхронизации (тег Git или копия
//!    папки) — с подписью пользователя или без.
//!
//! Откат к точке — не перезапись истории (она общая для всех устройств), а
//! новые операции этого устройства: всё, чего не было в точке, удаляется,
//! всё, что было, возвращается с прежним значением. Новые метки времени
//! побеждают, и после следующей синхронизации откат видят все устройства.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::json::{self, Value};
use crate::remote::{self, Error, Remote, SyncPoint, valid_file};
use crate::replica::{MemStore, Replica, Store, valid_name};

/// Хранилище, которое умеет перечислить файлы каталога устройства (нужно,
/// чтобы знать, что отправлять).
pub trait ListFiles: Store {
    fn device_files(&self, device: &str) -> Result<Vec<String>, Self::Error>;
}

impl ListFiles for MemStore {
    fn device_files(&self, device: &str) -> Result<Vec<String>, Self::Error> {
        let prefix = format!("devices/{device}/");
        Ok(self.files.keys().filter_map(|p| p.strip_prefix(&prefix)).filter(|n| !n.contains('/')).map(str::to_string).collect())
    }
}

#[cfg(feature = "std")]
impl ListFiles for crate::dir::DirStore {
    fn device_files(&self, device: &str) -> Result<Vec<String>, Self::Error> {
        let dir = self.root().join("devices").join(device);
        let mut out = Vec::new();
        match std::fs::read_dir(&dir) {
            Ok(read) => {
                for entry in read.flatten() {
                    if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        if valid_file(&name) {
                            out.push(name);
                        }
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        out.sort();
        Ok(out)
    }
}

/// Что синхронизация помнит между запусками (файл `sync-state.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncState {
    /// Свой файл → хеш отправленного содержимого.
    pub pushed: BTreeMap<String, String>,
    /// Чужой файл → версия на площадке, которую уже забрали.
    pub pulled: BTreeMap<String, String>,
    pub last_sync_ms: u64,
    pub last_point: String,
    pub last_error: String,
}

impl SyncState {
    pub fn to_text(&self) -> String {
        let map = |m: &BTreeMap<String, String>| Value::Obj(m.iter().map(|(k, v)| (k.clone(), Value::Str(v.clone()))).collect());
        Value::Obj(alloc::vec![
            (String::from("pushed"), map(&self.pushed)),
            (String::from("pulled"), map(&self.pulled)),
            (String::from("last_sync_ms"), Value::Num(self.last_sync_ms.to_string())),
            (String::from("last_point"), Value::Str(self.last_point.clone())),
            (String::from("last_error"), Value::Str(self.last_error.clone())),
        ])
        .to_text()
    }

    pub fn parse(text: &str) -> SyncState {
        let Ok(v) = json::parse(text) else { return SyncState::default() };
        let map = |key: &str| -> BTreeMap<String, String> {
            match v.get(key) {
                Some(Value::Obj(fields)) => {
                    fields.iter().filter_map(|(k, val)| Some((k.clone(), val.as_str()?.to_string()))).collect()
                }
                _ => BTreeMap::new(),
            }
        };
        SyncState {
            pushed: map("pushed"),
            pulled: map("pulled"),
            last_sync_ms: match v.get("last_sync_ms") {
                Some(Value::Num(n)) => n.parse().unwrap_or(0),
                _ => 0,
            },
            last_point: v.str_field("last_point").unwrap_or("").to_string(),
            last_error: v.str_field("last_error").unwrap_or("").to_string(),
        }
    }
}

/// Итог синхронизации.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub pushed: usize,
    pub pulled: usize,
    pub devices: usize,
    /// Имя созданной точки синхронизации.
    pub point: Option<String>,
}

/// Хеш содержимого (FNV-1a 64) — только «изменилось ли», не защита.
pub fn content_hash(data: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in data.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}:{}", data.len())
}

fn local<E: core::fmt::Debug>(e: E) -> Error {
    Error::Protocol(format!("локальная папка: {e:?}"))
}

/// Синхронизировать. `point` — `Some(подпись)`: после обмена отметить
/// состояние точкой (подпись может быть пустой). Ошибка на полпути не портит
/// ничего: отправленное отмечается только после успешной записи.
pub fn sync<L: ListFiles>(
    local_store: &mut L,
    remote: &mut dyn Remote,
    device: &str,
    state: &mut SyncState,
    point: Option<&str>,
    now_ms: u64,
) -> Result<Report, Error>
where
    L::Error: core::fmt::Debug,
{
    let mut report = Report::default();
    remote.prepare()?;

    // 1. Свои файлы.
    let mut outgoing: Vec<(String, String)> = Vec::new();
    let mut hashes: Vec<(String, String)> = Vec::new();
    for name in local_store.device_files(device).map_err(local)? {
        let path = format!("devices/{device}/{name}");
        let data = local_store.read(&path).map_err(local)?.unwrap_or_default();
        let hash = content_hash(&data);
        if state.pushed.get(&path) != Some(&hash) {
            outgoing.push((path.clone(), data));
            hashes.push((path, hash));
        }
    }
    if !outgoing.is_empty() {
        let (y, mo, d, h, mi, _) = remote::civil(now_ms / 1000);
        let message = format!("OBSOS {device}: {} файл(ов), {y:04}-{mo:02}-{d:02} {h:02}:{mi:02} UTC", outgoing.len());
        remote.write(&outgoing, &message)?;
        report.pushed = outgoing.len();
        state.pushed.extend(hashes);
    }

    // 2. Чужие файлы.
    for other in remote.devices(None)? {
        if other == device || !valid_name(&other) {
            continue;
        }
        report.devices += 1;
        for file in remote.files(&other, None)? {
            if !valid_file(&file.name) {
                continue;
            }
            let path = format!("devices/{other}/{}", file.name);
            if !file.version.is_empty() && state.pulled.get(&path) == Some(&file.version) {
                continue;
            }
            if let Some(data) = remote.read(&path, None)? {
                let old = local_store.read(&path).map_err(local)?;
                if old.as_deref() != Some(data.as_str()) {
                    local_store.write(&path, &data).map_err(local)?;
                    report.pulled += 1;
                }
                if !file.version.is_empty() {
                    state.pulled.insert(path, file.version);
                }
            }
        }
    }

    // 3. Точка синхронизации.
    if let Some(label) = point {
        let name = remote::point_name(now_ms, label);
        remote.create_point(&name, label.trim())?;
        state.last_point = name.clone();
        report.point = Some(name);
    }
    state.last_sync_ms = now_ms;
    state.last_error.clear();
    Ok(report)
}

/// Точки синхронизации площадки (новые первыми).
pub fn points(remote: &mut dyn Remote) -> Result<Vec<SyncPoint>, Error> {
    remote.points()
}

/// Что изменит откат одной коллекции.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rollback {
    pub restored: usize,
    pub removed: usize,
}

/// Откатить коллекции к точке `point`: прочитать с площадки состояние на тот
/// момент и записать в свою реплику операции, которые к нему приводят. Потом
/// нужна обычная синхронизация, чтобы откат ушёл на площадку.
pub fn rollback<S: Store>(
    replica: &mut Replica<S>,
    remote: &mut dyn Remote,
    point: &str,
    collections: &[&str],
    now_ms: u64,
) -> Result<Rollback, Error>
where
    S::Error: core::fmt::Debug,
{
    let then = fetch_point(remote, point)?;
    apply_rollback(replica, then, collections, now_ms)
}

/// Файлы всех устройств на момент точки — в памяти (сетевая часть отката;
/// браузер делает её в потоке синхронизации).
pub fn fetch_point(remote: &mut dyn Remote, point: &str) -> Result<MemStore, Error> {
    let mut then = MemStore::default();
    for dev in remote.devices(Some(point))? {
        if !valid_name(&dev) {
            continue;
        }
        for file in remote.files(&dev, Some(point))? {
            if !valid_file(&file.name) {
                continue;
            }
            let path = format!("devices/{dev}/{}", file.name);
            if let Some(data) = remote.read(&path, Some(point))? {
                then.files.insert(path, data);
            }
        }
    }
    Ok(then)
}

/// Записать в реплику операции, возвращающие коллекции к состоянию `then`
/// (из [`fetch_point`]).
pub fn apply_rollback<S: Store>(
    replica: &mut Replica<S>,
    then: MemStore,
    collections: &[&str],
    now_ms: u64,
) -> Result<Rollback, Error>
where
    S::Error: core::fmt::Debug,
{
    let mut past = Replica::new(then, replica.device());
    let mut result = Rollback::default();
    for coll in collections {
        let old = past.load(coll).map_err(local)?.state;
        let now = replica.load(coll).map_err(local)?.state;
        let current: Vec<(String, String)> = now.live().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        for (key, _) in &current {
            if old.get(key).is_none() {
                replica.del(coll, key, now_ms).map_err(local)?;
                result.removed += 1;
            }
        }
        for (key, value) in old.live() {
            if now.get(key) != Some(value) {
                replica.put(coll, key, value, now_ms).map_err(local)?;
                result.restored += 1;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote::fake::{FakeServer, Flavor};
    use crate::remote::{Account, Provider};

    const COLLS: &[&str] = &["bookmarks"];

    fn account(provider: Provider) -> Account {
        let mut a = Account::new(provider);
        a.repo = String::from(if provider.is_git() { "roman/obsos-sync" } else { "OBSOS" });
        a.user = String::from("roman");
        a.token = String::from("t0ken");
        if provider == Provider::WebDav {
            a.host = String::from("https://dav.example/remote.php/dav/files/roman");
        }
        a
    }

    /// Два устройства через площадку: закладка с одного, удаление с другого,
    /// точка с подписью, откат к ней — сходится везде.
    fn two_devices(provider: Provider) {
        let flavor = match provider {
            Provider::GitHub => Flavor::GitHub,
            Provider::GitLab => Flavor::GitLab,
            Provider::Bitbucket => Flavor::Bitbucket,
            Provider::YandexDisk | Provider::WebDav => Flavor::WebDav,
            Provider::GoogleDrive => Flavor::GoogleDrive,
            Provider::Folder => unreachable!(),
        };
        let server = FakeServer::new(flavor);
        let acc = account(provider);
        let mut pc = Replica::new(MemStore::default(), "pc1");
        let mut laptop = Replica::new(MemStore::default(), "laptop");
        let (mut s_pc, mut s_laptop) = (SyncState::default(), SyncState::default());

        pc.put("bookmarks", "https://example.com/", "Example", 1_000).unwrap();
        pc.put("bookmarks", "https://servo.org/", "Servo", 1_001).unwrap();
        let r = sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut s_pc, Some("first"), 1_794_155_405_000)
            .unwrap_or_else(|e| panic!("{provider:?} sync pc: {e}"));
        assert_eq!(r.pushed, 1, "{provider:?}");
        let point = r.point.clone().unwrap();
        assert_eq!(point, "obsos-20261108-163005-first");

        // Второе устройство получает и удаляет одну закладку.
        let r = sync(laptop.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "laptop", &mut s_laptop, None, 1_794_155_406_000).unwrap();
        assert_eq!((r.pulled, r.devices), (1, 1), "{provider:?}");
        assert_eq!(laptop.load("bookmarks").unwrap().state.live().count(), 2);
        laptop.del("bookmarks", "https://example.com/", 2_000).unwrap();
        sync(laptop.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "laptop", &mut s_laptop, None, 1_794_155_407_000).unwrap();

        // Первое видит удаление; повторная синхронизация ничего не гоняет.
        let r = sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut s_pc, None, 1_794_155_408_000).unwrap();
        assert_eq!(r.pulled, 1, "{provider:?}");
        let live: Vec<String> = pc.load("bookmarks").unwrap().state.live().map(|(k, _)| k.to_string()).collect();
        assert_eq!(live, alloc::vec![String::from("https://servo.org/")]);
        let r = sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut s_pc, None, 1_794_155_409_000).unwrap();
        assert_eq!((r.pushed, r.pulled), (0, 0), "{provider:?}: idle sync must not transfer");

        // Точки видны; откат к первой возвращает удалённую закладку.
        let pts = points(&mut *remote::open(&acc, &server).unwrap()).unwrap();
        assert_eq!(pts.len(), 1, "{provider:?}");
        assert_eq!(pts[0].name, point);
        let rb = rollback(&mut pc, &mut *remote::open(&acc, &server).unwrap(), &point, COLLS, 3_000).unwrap();
        assert_eq!(rb, Rollback { restored: 1, removed: 0 }, "{provider:?}");
        sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut s_pc, None, 1_794_155_410_000).unwrap();
        sync(laptop.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "laptop", &mut s_laptop, None, 1_794_155_411_000).unwrap();
        assert_eq!(laptop.load("bookmarks").unwrap().state.get("https://example.com/"), Some("Example"), "{provider:?}");
        // Сервер ни разу не видел чужого токена.
        assert!(server.auth_ok(), "{provider:?}: bad auth header");
    }

    #[test]
    fn github() {
        two_devices(Provider::GitHub);
    }

    #[test]
    fn gitlab() {
        two_devices(Provider::GitLab);
    }

    #[test]
    fn bitbucket() {
        two_devices(Provider::Bitbucket);
    }

    #[test]
    fn yandex_webdav() {
        two_devices(Provider::YandexDisk);
    }

    #[test]
    fn nextcloud_webdav() {
        two_devices(Provider::WebDav);
    }

    #[test]
    fn google_drive() {
        two_devices(Provider::GoogleDrive);
    }

    /// Репозиторий создан на сайте пустым (без README): Git Data API на нём
    /// отвечает 409 — первый коммит делает Contents API, дальше как обычно.
    #[test]
    fn github_empty_repository() {
        let server = FakeServer::new(Flavor::GitHub).with_empty_repo();
        let acc = account(Provider::GitHub);
        let mut pc = Replica::new(MemStore::default(), "pc1");
        pc.put("bookmarks", "https://example.com/", "Example", 1_000).unwrap();
        let r = sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut SyncState::default(), Some("111"), 1_794_155_405_000)
            .unwrap_or_else(|e| panic!("empty repo: {e}"));
        assert_eq!(r.pushed, 1);
        assert!(r.point.is_some());
    }

    #[test]
    fn bad_token_is_auth_error() {
        let server = FakeServer::new(Flavor::GitHub);
        let mut acc = account(Provider::GitHub);
        acc.token = String::from("wrong");
        let mut pc = Replica::new(MemStore::default(), "pc1");
        pc.put("bookmarks", "a", "b", 1).unwrap();
        let err = sync(pc.store_mut(), &mut *remote::open(&acc, &server).unwrap(), "pc1", &mut SyncState::default(), None, 5).unwrap_err();
        assert!(matches!(err, Error::Auth(_)), "{err:?}");
    }

    #[test]
    fn state_round_trip() {
        let mut s = SyncState::default();
        s.pushed.insert(String::from("devices/pc1/bookmarks.log"), content_hash("x"));
        s.pulled.insert(String::from("devices/pc2/bookmarks.log"), String::from("sha1"));
        s.last_sync_ms = 77;
        s.last_point = String::from("obsos-1");
        assert_eq!(SyncState::parse(&s.to_text()), s);
        assert_eq!(SyncState::parse("garbage"), SyncState::default());
    }

    #[cfg(feature = "std")]
    #[test]
    fn folder_remote_and_dirstore() {
        use crate::dir::DirStore;
        use crate::remote::folder::FolderRemote;
        let base = std::env::temp_dir().join(format!("obsos-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mut a = Replica::new(DirStore::new(base.join("a")), "pc1");
        let mut b = Replica::new(DirStore::new(base.join("b")), "pc2");
        let mut shared = FolderRemote::new(base.join("usb"));
        a.put("bookmarks", "k", "v", 1).unwrap();
        let (mut sa, mut sb) = (SyncState::default(), SyncState::default());
        let r = sync(a.store_mut(), &mut shared, "pc1", &mut sa, Some("usb"), 1_794_155_405_000).unwrap();
        assert_eq!(r.pushed, 1);
        sync(b.store_mut(), &mut shared, "pc2", &mut sb, None, 1_794_155_406_000).unwrap();
        assert_eq!(b.load("bookmarks").unwrap().state.get("k"), Some("v"));
        assert_eq!(points(&mut shared).unwrap()[0].label, "usb");
        let _ = std::fs::remove_dir_all(&base);
    }
}
