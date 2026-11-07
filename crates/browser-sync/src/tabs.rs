// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Открытые вкладки: снимок целиком в `devices/<id>/tabs.json` и закрытие
//! чужих вкладок удалённо.
//!
//! Вкладки меняются слишком часто для журнала, и истории у них нет — нужен
//! только «что открыто сейчас». Поэтому устройство переписывает свой файл
//! целиком, а остальные показывают его списком «Вкладки с других устройств».
//!
//! Закрыть вкладку другого устройства нельзя записью в его файл (каждый пишет
//! только своё), поэтому просьба закрыть — операция в коллекции `tab-close`
//! того, кто просит, с ключом `<устройство>/<id вкладки>`. Владелец при
//! синхронизации видит просьбы к себе (`pending_closes`), закрывает вкладки и
//! снимает просьбу (`ack_close` — `del` того же ключа), чтобы не закрыть
//! вкладку с тем же id потом. Id вкладок не повторяются (счётчик браузера).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::hlc::Hlc;
use crate::json::{self, Value};
use crate::replica::{Replica, Store, tabs_path, valid_name};

pub const CLOSE_COLL: &str = "tab-close";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: String,
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceTabs {
    pub device: String,
    /// Когда устройство записало список (для «обновлено N минут назад»).
    pub hlc: Hlc,
    pub tabs: Vec<Tab>,
}

impl DeviceTabs {
    pub fn to_text(&self) -> String {
        let tabs = self
            .tabs
            .iter()
            .map(|t| {
                Value::Obj(vec![
                    (String::from("id"), Value::Str(t.id.clone())),
                    (String::from("url"), Value::Str(t.url.clone())),
                    (String::from("title"), Value::Str(t.title.clone())),
                ])
            })
            .collect();
        let mut s = Value::Obj(vec![
            (String::from("device"), Value::Str(self.device.clone())),
            (String::from("hlc"), Value::Str(self.hlc.stamp())),
            (String::from("tabs"), Value::Arr(tabs)),
        ])
        .to_text();
        s.push('\n');
        s
    }

    pub fn parse(text: &str) -> Option<DeviceTabs> {
        let v = json::parse(text.trim()).ok()?;
        let device = v.str_field("device")?;
        let hlc = Hlc::parse_stamp(v.str_field("hlc")?, device)?;
        let mut tabs = Vec::new();
        if let Some(Value::Arr(items)) = v.get("tabs") {
            for t in items {
                // Вкладка без id или адреса пропускается, остальные — нет.
                if let (Some(id), Some(url)) = (t.str_field("id"), t.str_field("url")) {
                    let title = t.str_field("title").unwrap_or("");
                    tabs.push(Tab { id: String::from(id), url: String::from(url), title: String::from(title) });
                }
            }
        } else {
            return None;
        }
        Some(DeviceTabs { device: String::from(device), hlc, tabs })
    }
}

fn close_key(device: &str, id: &str) -> String {
    let mut k = String::from(device);
    k.push('/');
    k.push_str(id);
    k
}

impl<S: Store> Replica<S> {
    /// Записать свои открытые вкладки.
    pub fn publish_tabs(&mut self, tabs: &[Tab], now_ms: u64) -> Result<(), S::Error> {
        // Метка — от тех же часов, что и операции (не уходит назад).
        let hlc = self.tick(now_ms);
        let dt = DeviceTabs { device: String::from(self.device()), hlc, tabs: tabs.to_vec() };
        let path = tabs_path(self.device());
        self.store_mut().write(&path, &dt.to_text())
    }

    /// Вкладки других устройств (испорченные файлы и подписи не своего
    /// устройства пропускаются).
    pub fn remote_tabs(&self) -> Result<Vec<DeviceTabs>, S::Error> {
        let mut out = Vec::new();
        for dev in self.store().devices()? {
            if !valid_name(&dev) || dev == self.device() {
                continue;
            }
            if let Some(text) = self.store().read(&tabs_path(&dev))? {
                if let Some(dt) = DeviceTabs::parse(&text) {
                    if dt.device == dev {
                        out.push(dt);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Попросить устройство `device` закрыть вкладку `id`.
    pub fn close_remote_tab(&mut self, device: &str, id: &str, now_ms: u64) -> Result<(), S::Error> {
        self.put(CLOSE_COLL, &close_key(device, id), "close", now_ms).map(drop)
    }

    /// Id своих вкладок, которые просили закрыть.
    pub fn pending_closes(&mut self) -> Result<Vec<String>, S::Error> {
        let view = self.load(CLOSE_COLL)?;
        let prefix = close_key(self.device(), "");
        Ok(view.state.live().filter_map(|(k, _)| k.strip_prefix(prefix.as_str()).map(String::from)).collect())
    }

    /// Вкладка закрыта — снять просьбу.
    pub fn ack_close(&mut self, id: &str, now_ms: u64) -> Result<(), S::Error> {
        let key = close_key(self.device(), id);
        self.del(CLOSE_COLL, &key, now_ms).map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::replica::MemStore;

    fn tab(id: &str, url: &str) -> Tab {
        Tab { id: id.into(), url: url.into(), title: "Заголовок \"с кавычками\"".into() }
    }

    #[test]
    fn publish_and_close_remotely() {
        let mut pc = Replica::new(MemStore::default(), "pc");
        pc.publish_tabs(&[tab("1", "https://a/"), tab("2", "https://b/")], 1_000).unwrap();

        let mut phone = Replica::new(pc.store().clone(), "phone");
        let remote = phone.remote_tabs().unwrap();
        assert_eq!(remote.len(), 1);
        assert_eq!(remote[0].tabs[1], tab("2", "https://b/"));
        assert!(phone.remote_tabs().unwrap().iter().all(|d| d.device != "phone"));
        phone.close_remote_tab("pc", "2", 2_000).unwrap();

        let mut pc = Replica::new(phone.store().clone(), "pc");
        assert_eq!(pc.pending_closes().unwrap(), vec![String::from("2")]);
        pc.publish_tabs(&[tab("1", "https://a/")], 3_000).unwrap();
        pc.ack_close("2", 3_000).unwrap();
        assert!(pc.pending_closes().unwrap().is_empty());
        // Телефон тоже видит, что просьба снята.
        let mut phone = Replica::new(pc.store().clone(), "phone");
        assert!(phone.load(CLOSE_COLL).unwrap().state.live().next().is_none());
    }

    #[test]
    fn bad_tabs_files_are_skipped() {
        let mut s = MemStore::default();
        s.files.insert("devices/x/tabs.json".into(), "{oops".into());
        let other = DeviceTabs { device: "a".into(), hlc: Hlc::new(1, 0, "a"), tabs: vec![] };
        s.files.insert("devices/y/tabs.json".into(), other.to_text());
        let r = Replica::new(s, "me");
        assert!(r.remote_tabs().unwrap().is_empty());
    }
}
