//! История команд оболочки (терминал T2): ↑ и ↓, поиск по началу строки,
//! Ctrl+R, команда `history`, файл `~/.fossai_history`.
//!
//! # Как листается
//!
//! Как `HistorySearchBackward` в PowerShell и `history-search-backward` в
//! readline: если перед первым ↑ в строке что-то набрано, листаются только
//! команды, которые с этого начинаются; пустая строка — все подряд. Набранное
//! запоминается и возвращается, когда ↓ уходит дальше самой новой команды.
//! Повторы подряд не хранятся, а повтор старой команды переносит её в конец —
//! история остаётся списком разных команд, а не протоколом нажатий.
//!
//! # Где хранится
//!
//! В домашнем каталоге того, кто вошёл, по строке на команду, не больше
//! [`MAX`] последних. Файл переписывается целиком после каждой команды: он
//! небольшой, а дописывание в конец оставляло бы в нём повторы, которые
//! пришлось бы чистить при чтении.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::vfs::perm::Access;
use crate::vfs::VfsError;

/// Сколько команд помнить.
pub const MAX: usize = 1000;

/// Самая длинная строка в файле истории, которую стоит читать.
const FILE_LIMIT: usize = 256 * 1024;

pub struct History {
    items: Vec<String>,
    /// Какая команда сейчас показана в строке; `None` — набранная человеком.
    index: Option<usize>,
    /// Набранное до первого ↑ — вернётся после последнего ↓.
    draft: String,
}

impl History {
    #[must_use]
    pub const fn new() -> Self {
        Self { items: Vec::new(), index: None, draft: String::new() }
    }

    /// Файл истории того, кто вошёл.
    fn path() -> String {
        alloc::format!("{}/.fossai_history", crate::ui::context::home_dir())
    }

    /// Прочитать файл истории (его может не быть — это не ошибка).
    pub fn load(&mut self) {
        self.items.clear();
        if let Some(Ok((bytes, _))) = fs::read(&Self::path(), FILE_LIMIT) {
            if let Ok(text) = core::str::from_utf8(&bytes) {
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    self.push(line);
                }
            }
        }
        self.index = None;
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        self.items.get(index).map(String::as_str)
    }

    fn push(&mut self, line: &str) {
        self.items.retain(|item| item != line);
        if self.items.try_reserve(1).is_err() {
            return;
        }
        self.items.push(String::from(line));
        if self.items.len() > MAX {
            let extra = self.items.len() - MAX;
            self.items.drain(..extra);
        }
    }

    /// Запомнить выполненную команду и сохранить файл.
    pub fn add(&mut self, line: &str) {
        self.index = None;
        self.draft.clear();
        let line = line.trim_end();
        if line.trim().is_empty() {
            return;
        }
        self.push(line);
        self.save();
    }

    /// Строку правят руками: листание начинается заново от неё.
    pub fn reset(&mut self) {
        self.index = None;
    }

    /// ↑: предыдущая команда, начинающаяся с набранного до первого ↑.
    pub fn older(&mut self, current: &str) -> Option<&str> {
        if self.index.is_none() {
            self.draft = String::from(current);
        }
        let start = self.index.unwrap_or(self.items.len());
        let found = (0..start).rev().find(|&index| self.matches(index, current))?;
        self.index = Some(found);
        self.get(found)
    }

    /// ↓: следующая; за самой новой — набранное до листания.
    pub fn newer(&mut self, current: &str) -> Option<String> {
        let start = self.index? + 1;
        match (start..self.items.len()).find(|&index| self.matches(index, current)) {
            Some(found) => {
                self.index = Some(found);
                self.get(found).map(String::from)
            }
            None => {
                self.index = None;
                Some(core::mem::take(&mut self.draft))
            }
        }
    }

    /// Подходит ли команда к листанию: начинается с набранного до первого ↑ и
    /// не совпадает с тем, что уже в строке.
    fn matches(&self, index: usize, current: &str) -> bool {
        let item = &self.items[index];
        item.starts_with(self.draft.as_str()) && item != current
    }

    /// Ctrl+R: самая новая команда старше `before`, содержащая `query`.
    #[must_use]
    pub fn search(&self, query: &str, before: usize) -> Option<(usize, &str)> {
        (0..before.min(self.items.len())).rev().find(|&index| self.items[index].contains(query)).map(|index| (index, self.items[index].as_str()))
    }

    /// Самая новая команда, начинающаяся с `prefix` и длиннее его, — серая
    /// подсказка в строке (как Predictive IntelliSense в PowerShell).
    #[must_use]
    pub fn suggest(&self, prefix: &str) -> Option<&str> {
        if prefix.trim().is_empty() {
            return None;
        }
        self.items.iter().rev().find(|item| item.len() > prefix.len() && item.starts_with(prefix)).map(String::as_str)
    }

    /// Переписать файл истории. Не вышло — история остаётся в памяти, а
    /// говорить об этом на каждой команде незачем.
    fn save(&self) {
        let path = Self::path();
        let cred = crate::user::session::credentials();
        let node = match fs::resolve_as(cred, &path, Access::WRITE) {
            Some(Ok(node)) => match node.truncate(0) {
                Ok(()) => node,
                Err(_) => return,
            },
            Some(Err(VfsError::NotFound)) => match fs::create_as(cred, &path, 0o600) {
                Some(Ok(node)) => node,
                _ => return,
            },
            _ => return,
        };
        let mut text = String::new();
        for item in &self.items {
            text.push_str(item);
            text.push('\n');
        }
        let _ = node.write_at(0, text.as_bytes());
    }
}
