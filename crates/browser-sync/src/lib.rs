// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Синхронизация браузера через свой репозиторий (ROADMAP, этап N10).
//!
//! Закладки, история, настройки — журналы операций устройств
//! (`devices/<id>/<коллекция>.log`, `put`/`del` с гибридной меткой времени),
//! слитые правилом «побеждает последняя запись» (LWW-набор, CRDT): результат
//! одинаков на любом устройстве и при любом порядке чтения, удаление —
//! «надгробие», которое живёт, пока его не увидели все. Открытые вкладки —
//! снимком `devices/<id>/tabs.json`, закрытие чужих — просьбой в коллекции
//! `tab-close`.
//!
//! Здесь только логика: сеть (GitHub Contents API, Gitea/Forgejo, WebDAV) —
//! реализация типажа [`Store`] снаружи; пароли (`passwords.enc`, Argon2id →
//! XChaCha20-Poly1305) и вход по GitHub Device Flow — в оболочке браузера.
//!
//! ```
//! use browser_sync::{MemStore, Replica};
//! let mut pc = Replica::new(MemStore::default(), "pc1");
//! pc.put("bookmarks", "https://example.com/", "Example", 1_000).unwrap();
//! let mut other = Replica::new(pc.store().clone(), "pc2");
//! other.del("bookmarks", "https://example.com/", 2_000).unwrap();
//! let mut phone = Replica::new(other.store().clone(), "phone");
//! assert!(phone.load("bookmarks").unwrap().state.live().next().is_none());
//! ```

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

pub mod hlc;
#[cfg(feature = "std")]
pub mod dir;
pub mod json;
pub mod op;
pub mod replica;
pub mod tabs;

#[cfg(feature = "std")]
pub use dir::DirStore;
pub use hlc::{Clock, Hlc};
pub use op::{Kind, Op, State};
pub use replica::{Compacted, MemStore, Replica, Snapshot, Store, TOMBSTONE_TTL_MS, View};
pub use tabs::{DeviceTabs, Tab};
