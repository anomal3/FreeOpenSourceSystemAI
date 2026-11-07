// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Операция журнала и слияние «побеждает последняя запись» (LWW-набор).
//!
//! Строка журнала — один объект JSON:
//! `{"op":"put","key":"…","value":"…","hlc":"<wall>.<count>","device":"…"}`,
//! у `del` поля `value` нет. Значение — строка; что в ней (адрес и заголовок
//! закладки, значение настройки) решает тот, кто пишет: синхронизации нужна
//! только равенство.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Ordering;

use crate::hlc::Hlc;
use crate::json::{self, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Put(String),
    /// «Надгробие»: ключ удалён. Хранится, пока его не увидели все устройства
    /// (`crate::replica`), иначе старый `put` с другого устройства вернул бы
    /// удалённое.
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    pub key: String,
    pub kind: Kind,
    pub hlc: Hlc,
}

impl Op {
    pub fn put(key: &str, value: &str, hlc: Hlc) -> Self {
        Op { key: String::from(key), kind: Kind::Put(String::from(value)), hlc }
    }

    pub fn del(key: &str, hlc: Hlc) -> Self {
        Op { key: String::from(key), kind: Kind::Del, hlc }
    }

    pub fn value(&self) -> Option<&str> {
        match &self.kind {
            Kind::Put(v) => Some(v),
            Kind::Del => None,
        }
    }

    /// Полный порядок операций над одним ключом: сначала метка; при равной
    /// метке (одно устройство записало два раза в одну метку — так быть не
    /// должно, но журнал мог прийти чужой или испорченный) `del` старше
    /// `put`, а два `put` — по значению. Итог не зависит от порядка чтения.
    pub fn cmp_lww(&self, other: &Op) -> Ordering {
        self.hlc.cmp(&other.hlc).then_with(|| match (&self.kind, &other.kind) {
            (Kind::Del, Kind::Del) => Ordering::Equal,
            (Kind::Del, Kind::Put(_)) => Ordering::Greater,
            (Kind::Put(_), Kind::Del) => Ordering::Less,
            (Kind::Put(a), Kind::Put(b)) => a.cmp(b),
        })
    }

    pub fn to_json(&self) -> Value {
        let mut f = vec![
            (String::from("op"), Value::Str(String::from(if self.value().is_some() { "put" } else { "del" }))),
            (String::from("key"), Value::Str(self.key.clone())),
        ];
        if let Kind::Put(v) = &self.kind {
            f.push((String::from("value"), Value::Str(v.clone())));
        }
        f.push((String::from("hlc"), Value::Str(self.hlc.stamp())));
        f.push((String::from("device"), Value::Str(self.hlc.device.clone())));
        Value::Obj(f)
    }

    pub fn to_line(&self) -> String {
        let mut s = self.to_json().to_text();
        s.push('\n');
        s
    }

    pub fn from_json(v: &Value) -> Option<Op> {
        let key = v.str_field("key")?;
        let device = v.str_field("device")?;
        let hlc = Hlc::parse_stamp(v.str_field("hlc")?, device)?;
        let kind = match v.str_field("op")? {
            "put" => Kind::Put(String::from(v.str_field("value")?)),
            "del" => Kind::Del,
            _ => return None,
        };
        Some(Op { key: String::from(key), kind, hlc })
    }

    pub fn parse_line(line: &str) -> Option<Op> {
        Op::from_json(&json::parse(line).ok()?)
    }
}

/// Разобрать журнал (строки JSON). Пустые строки пропускаются; испорченные
/// (оборванная дозапись, правка руками) — тоже, но считаются: синхронизация
/// не должна вставать из-за одной плохой строки.
pub fn parse_log(text: &str, out: &mut Vec<Op>) -> usize {
    let mut bad = 0;
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        match Op::parse_line(line) {
            Some(op) => out.push(op),
            None => bad += 1,
        }
    }
    bad
}

/// Слитое состояние одной коллекции: по каждому ключу — победившая операция.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    entries: BTreeMap<String, Op>,
}

impl State {
    pub fn new() -> Self {
        State::default()
    }

    /// Учесть операцию. Слияние коммутативно, ассоциативно и идемпотентно:
    /// результат не зависит ни от порядка, ни от повторов.
    pub fn apply(&mut self, op: Op) {
        match self.entries.get(&op.key) {
            Some(cur) if op.cmp_lww(cur) != Ordering::Greater => {}
            _ => {
                self.entries.insert(op.key.clone(), op);
            }
        }
    }

    pub fn apply_all<I: IntoIterator<Item = Op>>(&mut self, ops: I) {
        for op in ops {
            self.apply(op);
        }
    }

    /// Значение ключа (нет ключа или удалён — `None`).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).and_then(Op::value)
    }

    /// Победившая операция по ключу, включая надгробия.
    pub fn winner(&self, key: &str) -> Option<&Op> {
        self.entries.get(key)
    }

    /// Живые записи по порядку ключей.
    pub fn live(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().filter_map(|(k, op)| op.value().map(|v| (k.as_str(), v)))
    }

    /// Все победители, включая надгробия.
    pub fn winners(&self) -> impl Iterator<Item = &Op> {
        self.entries.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(w: u64, c: u32, d: &str) -> Hlc {
        Hlc::new(w, c, d)
    }

    #[test]
    fn line_roundtrip() {
        let ops = [
            Op::put("https://example.com/", "{\"title\":\"Пример\"}\n", h(1, 2, "pc")),
            Op::del("k", h(3, 0, "телефон")),
        ];
        for op in ops {
            let line = op.to_line();
            assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
            assert_eq!(Op::parse_line(line.trim_end()), Some(op));
        }
    }

    #[test]
    fn log_skips_bad_lines() {
        let text = "{\"op\":\"put\",\"key\":\"a\",\"value\":\"1\",\"hlc\":\"5.0\",\"device\":\"x\"}\r\n\n\
                    {\"op\":\"put\",\"key\":\"b\"\n\
                    {\"op\":\"zap\",\"key\":\"a\",\"hlc\":\"6.0\",\"device\":\"x\"}\n\
                    {\"op\":\"del\",\"key\":\"a\",\"hlc\":\"7.0\",\"device\":\"x\"}";
        let mut ops = Vec::new();
        assert_eq!(parse_log(text, &mut ops), 2);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[1], Op::del("a", h(7, 0, "x")));
    }

    #[test]
    fn delete_then_readd() {
        let mut s = State::new();
        s.apply(Op::put("e", "1", h(1, 0, "a")));
        s.apply(Op::del("e", h(2, 0, "b")));
        assert_eq!(s.get("e"), None);
        s.apply(Op::put("e", "2", h(3, 0, "a")));
        assert_eq!(s.get("e"), Some("2"));
        // Опоздавший старый `put` ничего не меняет.
        s.apply(Op::put("e", "0", h(0, 5, "c")));
        assert_eq!(s.get("e"), Some("2"));
    }

    #[test]
    fn same_stamp_is_deterministic() {
        let a = Op::put("k", "a", h(1, 0, "x"));
        let b = Op::put("k", "b", h(1, 0, "x"));
        let d = Op::del("k", h(1, 0, "x"));
        let mut s1 = State::new();
        s1.apply_all([a.clone(), b.clone(), d.clone()]);
        let mut s2 = State::new();
        s2.apply_all([d, b, a]);
        assert_eq!(s1, s2);
        assert_eq!(s1.get("k"), None);
    }
}
