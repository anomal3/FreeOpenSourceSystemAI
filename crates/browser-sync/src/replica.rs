// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Реплика устройства над хранилищем файлов (`Store`).
//!
//! Раскладка в репозитории:
//!
//! ```text
//! devices/<id>/<коллекция>.log       журнал устройства, только дописывается
//! devices/<id>/<коллекция>.snapshot  снимок: заголовок + сжатые операции
//! devices/<id>/tabs.json             открытые вкладки целиком
//! ```
//!
//! **Каждое устройство пишет только в свой каталог**, поэтому два устройства
//! никогда не спорят за один файл (на GitHub это значит — нет конфликтов
//! правок по `sha`). Читают все — всё.
//!
//! # Снимок и сборка надгробий
//!
//! `compact` переписывает снимок устройства: только **свои** операции, которые
//! всё ещё побеждают в общем слиянии (свои, перебитые чужими, выбрасываются), и
//! обнуляет журнал. В заголовке снимка — вектор `seen`: по каждому устройству
//! самая большая его метка, прочитанная при сжатии. Метки одного устройства
//! растут по журналу, поэтому `seen[D] >= T` значит «операцию `T` устройства
//! `D` это устройство прочитало и всё своё, что она перебила, уже выбросило».
//!
//! Своё надгробие `T` выбрасывается при сжатии, только если ему больше
//! `tombstone_ttl_ms` (90 дней) **и** у каждого устройства, которое вообще
//! пишет эту коллекцию, есть снимок с `seen[я] >= T`. Устройство без снимка
//! (давно не включалось) держит надгробия — иначе его старый `put` вернул бы
//! удалённое. Новые операции такого устройства после того, как оно прочитает
//! общее, получат метку больше `T` (часы `observe`) и честно победят.
//!
//! Сбой между записью снимка и обнулением журнала не страшен: операции из
//! журнала совпадают с уже записанными в снимок, а слияние идемпотентно.
//! Одна реплика на устройство (одна программа пишет его каталог).

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::hlc::{Clock, Hlc};
use crate::json::{self, Value};
use crate::op::{self, Op, State};

/// 90 дней — срок жизни надгробия до того, как его можно выбросить.
pub const TOMBSTONE_TTL_MS: u64 = 90 * 24 * 3600 * 1000;

/// Хранилище файлов: GitHub Contents API, Gitea/Forgejo, WebDAV, каталог на
/// диске или память (`MemStore`, тесты). Пути — с `/`, относительно корня
/// репозитория синхронизации.
pub trait Store {
    type Error;
    /// Id устройств — имена подкаталогов `devices/`.
    fn devices(&self) -> Result<Vec<String>, Self::Error>;
    /// Содержимое файла; нет файла — `Ok(None)`.
    fn read(&self, path: &str) -> Result<Option<String>, Self::Error>;
    /// Записать файл целиком (создать, если нет).
    fn write(&mut self, path: &str, data: &str) -> Result<(), Self::Error>;
    /// Дописать в конец (создать, если нет). Хранилищу без дозаписи хватит
    /// `read` + `write`.
    fn append(&mut self, path: &str, data: &str) -> Result<(), Self::Error>;
}

/// Хранилище в памяти: тесты и образец для настоящих.
#[derive(Clone, Debug, Default)]
pub struct MemStore {
    pub files: BTreeMap<String, String>,
}

impl Store for MemStore {
    type Error = core::convert::Infallible;

    fn devices(&self) -> Result<Vec<String>, Self::Error> {
        let mut set = BTreeSet::new();
        for path in self.files.keys() {
            if let Some(rest) = path.strip_prefix("devices/") {
                if let Some((dev, _)) = rest.split_once('/') {
                    set.insert(String::from(dev));
                }
            }
        }
        Ok(set.into_iter().collect())
    }

    fn read(&self, path: &str) -> Result<Option<String>, Self::Error> {
        Ok(self.files.get(path).cloned())
    }

    fn write(&mut self, path: &str, data: &str) -> Result<(), Self::Error> {
        self.files.insert(String::from(path), String::from(data));
        Ok(())
    }

    fn append(&mut self, path: &str, data: &str) -> Result<(), Self::Error> {
        self.files.entry(String::from(path)).or_default().push_str(data);
        Ok(())
    }
}

/// Имя устройства или коллекции: латиница, цифры, `-`, `_`, до 64 знаков.
/// Имена каталогов приходят из чужого репозитория — без этой проверки
/// `../` в имени устройства увёл бы запись за пределы своего каталога.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 64 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub fn log_path(device: &str, coll: &str) -> String {
    format!("devices/{device}/{coll}.log")
}

pub fn snapshot_path(device: &str, coll: &str) -> String {
    format!("devices/{device}/{coll}.snapshot")
}

pub fn tabs_path(device: &str) -> String {
    format!("devices/{device}/tabs.json")
}

/// Снимок коллекции одного устройства.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// По каждому устройству — его самая большая прочитанная метка.
    pub seen: BTreeMap<String, Hlc>,
    pub ops: Vec<Op>,
}

impl Snapshot {
    pub fn to_text(&self) -> String {
        let seen = self.seen.iter().map(|(d, h)| (d.clone(), Value::Str(h.stamp()))).collect();
        let header = Value::Obj(vec![
            (String::from("snapshot"), Value::Num(String::from("1"))),
            (String::from("seen"), Value::Obj(seen)),
        ]);
        let mut s = header.to_text();
        s.push('\n');
        for op in &self.ops {
            s.push_str(&op.to_line());
        }
        s
    }

    /// Разбор; второе — число испорченных строк операций. Без читаемого
    /// заголовка снимок не принимается целиком: такой файл не наш.
    pub fn parse(text: &str) -> Option<(Snapshot, usize)> {
        let (head, rest) = text.split_once('\n').unwrap_or((text, ""));
        let header = json::parse(head.trim_end_matches('\r')).ok()?;
        if header.get("snapshot") != Some(&Value::Num(String::from("1"))) {
            return None;
        }
        let mut seen = BTreeMap::new();
        if let Some(Value::Obj(fields)) = header.get("seen") {
            for (dev, v) in fields {
                if let Some(h) = v.as_str().and_then(|s| Hlc::parse_stamp(s, dev)) {
                    seen.insert(dev.clone(), h);
                }
            }
        }
        let mut ops = Vec::new();
        let bad = op::parse_log(rest, &mut ops);
        Some((Snapshot { seen, ops }, bad))
    }
}

/// Слитый вид одной коллекции.
#[derive(Clone, Debug, Default)]
pub struct View {
    pub state: State,
    /// По каждому устройству — самая большая его метка в прочитанном.
    pub seen: BTreeMap<String, Hlc>,
    /// Вектор `seen` из снимка каждого устройства (у кого снимок есть).
    pub snapshot_seen: BTreeMap<String, BTreeMap<String, Hlc>>,
    /// Устройства, у которых есть журнал или снимок этой коллекции.
    pub writers: BTreeSet<String>,
    /// Испорченные строки и файлы — для журнала программы.
    pub bad_lines: usize,
    /// Операции с чужим `device` в моём каталоге и подобное — отброшены.
    pub foreign_ops: usize,
}

/// Итог сжатия.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compacted {
    pub kept: usize,
    pub dropped_superseded: usize,
    pub dropped_tombstones: usize,
}

pub struct Replica<S: Store> {
    store: S,
    clock: Clock,
    /// Коллекции, чей журнал уже прочитан — часы учли свои прошлые метки.
    loaded: BTreeSet<String>,
    pub tombstone_ttl_ms: u64,
}

impl<S: Store> Replica<S> {
    /// `device` должен проходить `valid_name` (иначе — паника: это ошибка
    /// программы, а не данных).
    pub fn new(store: S, device: &str) -> Self {
        assert!(valid_name(device), "bad device id {device:?}");
        Replica { store, clock: Clock::new(device), loaded: BTreeSet::new(), tombstone_ttl_ms: TOMBSTONE_TTL_MS }
    }

    pub fn device(&self) -> &str {
        self.clock.device()
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub(crate) fn tick(&mut self, now_ms: u64) -> Hlc {
        self.clock.tick(now_ms)
    }

    fn devices(&self) -> Result<Vec<String>, S::Error> {
        let mut list: Vec<String> = self.store.devices()?.into_iter().filter(|d| valid_name(d)).collect();
        if !list.iter().any(|d| d == self.device()) {
            list.push(String::from(self.device()));
        }
        list.sort();
        list.dedup();
        Ok(list)
    }

    /// Прочитать снимки и журналы всех устройств и слить.
    pub fn load(&mut self, coll: &str) -> Result<View, S::Error> {
        assert!(valid_name(coll), "bad collection {coll:?}");
        let mut view = View::default();
        for dev in self.devices()? {
            let mut ops = Vec::new();
            let mut present = false;
            if let Some(text) = self.store.read(&snapshot_path(&dev, coll))? {
                present = true;
                match Snapshot::parse(&text) {
                    Some((snap, bad)) => {
                        view.bad_lines += bad;
                        view.snapshot_seen.insert(dev.clone(), snap.seen);
                        ops.extend(snap.ops);
                    }
                    None => view.bad_lines += 1,
                }
            }
            if let Some(text) = self.store.read(&log_path(&dev, coll))? {
                present = true;
                view.bad_lines += op::parse_log(&text, &mut ops);
            }
            if present {
                view.writers.insert(dev.clone());
            }
            for op in ops {
                // Устройство пишет только свои операции; чужая подпись в его
                // каталоге — подделка или ошибка, и она сломала бы `seen`.
                if op.hlc.device != dev {
                    view.foreign_ops += 1;
                    continue;
                }
                self.clock.observe(&op.hlc);
                match view.seen.get(&dev) {
                    Some(h) if *h >= op.hlc => {}
                    _ => {
                        view.seen.insert(dev.clone(), op.hlc.clone());
                    }
                }
                view.state.apply(op);
            }
        }
        self.loaded.insert(String::from(coll));
        Ok(view)
    }

    fn record(&mut self, coll: &str, make: impl FnOnce(Hlc) -> Op, now_ms: u64) -> Result<Op, S::Error> {
        if !self.loaded.contains(coll) {
            // Без этого после перезапуска с отставшими часами новая операция
            // могла бы получить метку меньше своей же прошлой.
            self.load(coll)?;
        }
        let op = make(self.clock.tick(now_ms));
        let path = log_path(self.device(), coll);
        self.store.append(&path, &op.to_line())?;
        Ok(op)
    }

    pub fn put(&mut self, coll: &str, key: &str, value: &str, now_ms: u64) -> Result<Op, S::Error> {
        self.record(coll, |h| Op::put(key, value, h), now_ms)
    }

    pub fn del(&mut self, coll: &str, key: &str, now_ms: u64) -> Result<Op, S::Error> {
        self.record(coll, |h| Op::del(key, h), now_ms)
    }

    /// Сжать свой журнал коллекции в снимок (см. описание модуля).
    pub fn compact(&mut self, coll: &str, now_ms: u64) -> Result<Compacted, S::Error> {
        let view = self.load(coll)?;
        let me = String::from(self.device());
        let mut own_total = 0usize;
        if let Some(text) = self.store.read(&snapshot_path(&me, coll))? {
            if let Some((snap, _)) = Snapshot::parse(&text) {
                own_total += snap.ops.len();
            }
        }
        if let Some(text) = self.store.read(&log_path(&me, coll))? {
            let mut ops = Vec::new();
            op::parse_log(&text, &mut ops);
            own_total += ops.len();
        }

        let mut out = Compacted::default();
        let mut keep = Vec::new();
        for op in view.state.winners().filter(|op| op.hlc.device == me) {
            if op.value().is_none() && self.tombstone_expired(op, &view, now_ms) {
                out.dropped_tombstones += 1;
            } else {
                keep.push(op.clone());
            }
        }
        out.kept = keep.len();
        out.dropped_superseded = own_total.saturating_sub(out.kept + out.dropped_tombstones);
        let snap = Snapshot { seen: view.seen, ops: keep };
        self.store.write(&snapshot_path(&me, coll), &snap.to_text())?;
        self.store.write(&log_path(&me, coll), "")?;
        Ok(out)
    }

    fn tombstone_expired(&self, op: &Op, view: &View, now_ms: u64) -> bool {
        if now_ms.saturating_sub(op.hlc.wall) < self.tombstone_ttl_ms {
            return false;
        }
        view.writers.iter().filter(|d| **d != op.hlc.device).all(|d| {
            view.snapshot_seen.get(d).and_then(|seen| seen.get(&op.hlc.device)).is_some_and(|h| *h >= op.hlc)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 3600 * 1000;

    /// Общий репозиторий, к которому по очереди подключаются устройства.
    fn replica(store: &MemStore, dev: &str) -> Replica<MemStore> {
        Replica::new(store.clone(), dev)
    }

    fn live(r: &mut Replica<MemStore>, coll: &str) -> Vec<(String, String)> {
        let v = r.load(coll).unwrap();
        v.state.live().map(|(k, v)| (String::from(k), String::from(v))).collect()
    }

    #[test]
    fn roman_bookmark_case() {
        // Добавил example.com на одном ПК, удалил на другом — исчезло везде,
        // в том числе на телефоне, который всё это время был выключен.
        let mut repo = MemStore::default();
        let mut pc1 = replica(&repo, "pc1");
        pc1.put("bookmarks", "https://example.com/", "Example", 10_000_000).unwrap();
        repo = pc1.store().clone();

        let mut pc2 = replica(&repo, "pc2");
        assert_eq!(live(&mut pc2, "bookmarks").len(), 1);
        // У второго ПК часы отстают на час — удаление всё равно позже.
        pc2.del("bookmarks", "https://example.com/", 10_000_000 - 3_600_000).unwrap();
        repo = pc2.store().clone();

        for dev in ["pc1", "pc2", "phone"] {
            let mut r = replica(&repo, dev);
            assert!(live(&mut r, "bookmarks").is_empty(), "{dev}");
        }

        // Повторное добавление позже — возвращает закладку везде.
        let mut phone = replica(&repo, "phone");
        phone.put("bookmarks", "https://example.com/", "Example", 20_000_000).unwrap();
        repo = phone.store().clone();
        let mut pc1 = replica(&repo, "pc1");
        assert_eq!(live(&mut pc1, "bookmarks"), vec![("https://example.com/".into(), "Example".into())]);
    }

    #[test]
    fn delete_vs_concurrent_edit() {
        // Два устройства не видели друг друга: побеждает более поздняя метка,
        // и одинаково на всех.
        let repo = MemStore::default();
        let mut a = replica(&repo, "a");
        a.put("settings", "home", "x", 10).unwrap();
        let mut b = replica(&a.store().clone(), "b");
        b.load("settings").unwrap();
        // Без синхронизации: a правит, b удаляет чуть позже по часам.
        a.put("settings", "home", "y", 20).unwrap();
        b.del("settings", "home", 21).unwrap();
        let mut merged = a.store().clone();
        merged.files.extend(b.store().files.iter().filter(|(k, _)| k.starts_with("devices/b/")).map(|(k, v)| (k.clone(), v.clone())));
        for dev in ["a", "b", "c"] {
            let mut r = replica(&merged, dev);
            assert!(live(&mut r, "settings").is_empty(), "{dev}");
        }
    }

    #[test]
    fn restart_with_clock_behind_still_wins() {
        let repo = MemStore::default();
        let mut a = replica(&repo, "a");
        a.put("s", "k", "1", 1_000_000).unwrap();
        // Перезапуск: новая реплика, часы ушли назад — put без load.
        let mut a2 = replica(a.store(), "a");
        let op = a2.put("s", "k", "2", 5).unwrap();
        assert!(op.hlc > Hlc::new(1_000_000, 0, "a"));
        assert_eq!(live(&mut a2, "s"), vec![("k".into(), "2".into())]);
    }

    #[test]
    fn forged_and_bad_input_is_ignored() {
        let mut repo = MemStore::default();
        // Устройство «evil» пишет операцию от имени «a», плюс мусор.
        let forged = Op::put("k", "evil", Hlc::new(99, 0, "a")).to_line();
        repo.write("devices/evil/s.log", &format!("{forged}garbage\n")).unwrap();
        repo.write("devices/../x/s.log", &forged).unwrap();
        let mut r = replica(&repo, "a");
        let v = r.load("s").unwrap();
        assert_eq!(v.state.get("k"), None);
        assert_eq!((v.foreign_ops, v.bad_lines), (1, 1));
    }

    #[test]
    fn compaction_keeps_view_and_drops_superseded() {
        let repo = MemStore::default();
        let mut a = replica(&repo, "a");
        for i in 0..10 {
            a.put("h", "k", &format!("{i}"), 100 + i).unwrap();
        }
        a.put("h", "gone", "x", 200).unwrap();
        let mut b = replica(a.store(), "b");
        b.del("h", "gone", 300).unwrap();
        let mut a = replica(b.store(), "a");
        let before = live(&mut a, "h");
        let c = a.compact("h", 400).unwrap();
        assert_eq!(c, Compacted { kept: 1, dropped_superseded: 10, dropped_tombstones: 0 });
        assert_eq!(a.store().files["devices/a/h.log"], "");
        assert_eq!(live(&mut a, "h"), before);
        // Дальнейшая запись после сжатия.
        a.put("h", "k2", "v", 500).unwrap();
        assert_eq!(live(&mut a, "h").len(), 2);
    }

    #[test]
    fn tombstone_gc_waits_for_every_writer() {
        let repo = MemStore::default();
        let mut a = replica(&repo, "a");
        a.put("b", "x", "1", DAY).unwrap();
        let mut b = replica(a.store(), "b");
        b.load("b").unwrap();
        b.del("b", "x", 2 * DAY).unwrap();
        // Устройство c писало до удаления (видело только a) и с тех пор
        // выключено: в его журнале старый put того же ключа.
        let mut c = replica(a.store(), "c");
        c.put("b", "x", "old", DAY / 2).unwrap(); // метка > a, но < удаления b
        let mut repo = b.store().clone();
        repo.files.extend(c.store().files.iter().filter(|(k, _)| k.starts_with("devices/c/")).map(|(k, v)| (k.clone(), v.clone())));
        let mut b = replica(&repo, "b");
        assert!(live(&mut b, "b").is_empty());

        // 100 дней спустя b сжимает: у a и c нет снимков — надгробие живёт.
        let late = 100 * DAY;
        assert_eq!(b.compact("b", late).unwrap().dropped_tombstones, 0);
        // a сжимается (видит удаление), c — нет.
        let mut a = replica(b.store(), "a");
        a.compact("b", late).unwrap();
        let mut b = replica(a.store(), "b");
        assert_eq!(b.compact("b", late).unwrap().dropped_tombstones, 0);
        // c включился и сжался — теперь все видели удаление.
        let mut c = replica(b.store(), "c");
        c.compact("b", late).unwrap();
        let mut b = replica(c.store(), "b");
        assert_eq!(b.compact("b", late).unwrap().dropped_tombstones, 1);
        // И ничего не воскресло ни у кого.
        for dev in ["a", "b", "c", "d"] {
            let mut r = replica(b.store(), dev);
            assert!(live(&mut r, "b").is_empty(), "{dev}");
        }
        assert_eq!(b.store().files["devices/b/b.snapshot"].lines().count(), 1);
    }

    #[test]
    fn young_tombstone_is_kept() {
        let repo = MemStore::default();
        let mut a = replica(&repo, "a");
        a.put("b", "x", "1", DAY).unwrap();
        a.del("b", "x", 2 * DAY).unwrap();
        assert_eq!(a.compact("b", 3 * DAY).unwrap().dropped_tombstones, 0);
        assert_eq!(a.compact("b", 93 * DAY).unwrap().dropped_tombstones, 1);
    }

    /// Простой ГПСЧ для перебора: зависимостей нет и не нужно.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
    }

    #[test]
    fn random_histories_converge() {
        let devs = ["a", "b", "c"];
        for seed in 0..60u64 {
            let mut rng = Lcg(seed);
            // У каждого устройства своя копия репозитория; синхронизация —
            // копирование чужих каталогов в случайные моменты.
            let mut stores: Vec<MemStore> = devs.iter().map(|_| MemStore::default()).collect();
            let skew = [0u64, 50_000, 7];
            for step in 0..80u64 {
                let i = rng.next(3) as usize;
                let mut r = Replica::new(stores[i].clone(), devs[i]);
                let now = 1_000_000 + step * 10 + skew[i] - rng.next(20) * 1_000;
                let key = format!("k{}", rng.next(5));
                match rng.next(6) {
                    0..=2 =>drop(r.put("c", &key, &format!("{seed}-{step}"), now).unwrap()),
                    3 => drop(r.del("c", &key, now).unwrap()),
                    4 => drop(r.compact("c", now).unwrap()),
                    _ => {
                        let j = rng.next(3) as usize;
                        let from = stores[j].clone();
                        let prefix = format!("devices/{}/", devs[j]);
                        r.store_mut().files.extend(from.files.into_iter().filter(|(k, _)| k.starts_with(&prefix)));
                    }
                }
                stores[i] = r.store().clone();
            }
            // Полный обмен — все видят одно и то же.
            let mut all = MemStore::default();
            for (i, s) in stores.iter().enumerate() {
                let prefix = format!("devices/{}/", devs[i]);
                all.files.extend(s.files.iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(k, v)| (k.clone(), v.clone())));
            }
            let expect = live(&mut Replica::new(all.clone(), "z"), "c");
            for d in devs {
                assert_eq!(live(&mut Replica::new(all.clone(), d), "c"), expect, "seed {seed}");
            }
            // Сжатие каждого (и надгробия старше срока) не меняет вида.
            let mut cur = all;
            for d in devs {
                let mut r = Replica::new(cur, d);
                r.compact("c", 1_000_000 + 200 * DAY).unwrap();
                cur = r.store().clone();
            }
            for d in devs {
                let mut r = Replica::new(cur.clone(), d);
                r.compact("c", 1_000_000 + 200 * DAY).unwrap();
                cur = r.store().clone();
            }
            assert_eq!(live(&mut Replica::new(cur, "z"), "c"), expect, "seed {seed} after gc");
        }
    }
}
