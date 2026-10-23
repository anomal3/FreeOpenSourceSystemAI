//! Дополнение по Tab (терминал T3).
//!
//! Что дополняется:
//!
//! - первое слово — встроенные команды оболочки и программы из `/bin` (их
//!   оболочка запускает и без `run`);
//! - слово с `/` или после первого — путь: каталоги получают `/` в конце,
//!   файлы — пробел;
//! - подкоманды известных команд: `pkg install|remove|list`, а после
//!   `pkg install` — пакеты `.fpk` с носителя `/media`, после `pkg remove` —
//!   установленные пакеты.
//!
//! Как в bash: единственный вариант подставляется целиком, у нескольких —
//! общее начало, а второй Tab подряд показывает их список. Различать регистр
//! незачем: имена в системе — латиница в нижнем регистре.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::vfs::NodeKind;

/// Встроенные команды оболочки — то, что разбирает `run_command`.
pub const BUILTINS: [&str; 46] = [
    "acpidump", "arp", "beep", "cat", "cd", "clear", "date", "dmesg", "echo", "exit", "free", "fsck", "guard",
    "help", "history", "input", "intel", "ip", "irq", "kill", "lastpanic", "ls", "mem", "mkdir", "mounts",
    "panic", "pci", "ping", "power", "poweroff", "pwd", "quit", "reboot", "resolve", "restart", "rm", "run",
    "shutdown", "slots", "stat", "sysupdate", "tasks", "tcp", "ui", "uptime", "usb",
];

/// Варианты для слова под курсором.
pub struct Completion {
    /// Где слово начинается (байт в строке до курсора).
    pub start: usize,
    /// Слово целиком, как его заменить, и каталог ли это.
    pub candidates: Vec<(String, bool)>,
}

impl Completion {
    /// Общее начало всех вариантов.
    #[must_use]
    pub fn common_prefix(&self) -> String {
        let mut iter = self.candidates.iter().map(|(text, _)| text.as_str());
        let Some(first) = iter.next() else {
            return String::new();
        };
        let mut end = first.len();
        for other in iter {
            end = first
                .char_indices()
                .zip(other.chars())
                .take_while(|((_, a), b)| a == b)
                .map(|((at, ch), _)| at + ch.len_utf8())
                .last()
                .unwrap_or(0)
                .min(end);
        }
        String::from(&first[..end])
    }
}

/// Варианты для набранного до курсора; `cwd` — текущий каталог оболочки.
#[must_use]
pub fn complete(line: &str, cwd: &str) -> Completion {
    let start = line.rfind(' ').map_or(0, |at| at + 1);
    let word = &line[start..];
    let before: Vec<&str> = line[..start].split_whitespace().collect();
    let mut candidates = Vec::new();

    match before.as_slice() {
        // Первое слово: команда или программа.
        [] if !word.contains('/') => {
            for name in BUILTINS.iter().filter(|name| name.starts_with(word)) {
                candidates.push((String::from(*name), false));
            }
            if let Some(Ok(entries)) = fs::list("/bin") {
                for entry in entries.iter().filter(|entry| entry.name.starts_with(word)) {
                    if !candidates.iter().any(|(name, _)| *name == entry.name) {
                        candidates.push((entry.name.clone(), false));
                    }
                }
            }
        }
        ["pkg"] | ["run", "/bin/pkg"] => {
            for sub in ["install", "remove", "list"].iter().filter(|sub| sub.starts_with(word)) {
                candidates.push((String::from(*sub), false));
            }
        }
        // Пакет с носителя — сразу полным путём, если начала пути нет.
        ["pkg", "install"] | ["run", "/bin/pkg", "install"] if !word.contains('/') => {
            if let Some(Ok(entries)) = fs::list("/media") {
                for entry in entries.iter().filter(|entry| entry.name.ends_with(".fpk")) {
                    let full = alloc::format!("/media/{}", entry.name);
                    if full.starts_with(word) || entry.name.starts_with(word) {
                        candidates.push((full, false));
                    }
                }
            }
        }
        ["pkg", "remove"] | ["run", "/bin/pkg", "remove"] => {
            if let Some(Ok(entries)) = fs::list("/var/lib/pkg") {
                for entry in &entries {
                    if let Some(name) = entry.name.strip_suffix(".pkg").filter(|name| name.starts_with(word)) {
                        candidates.push((String::from(name), false));
                    }
                }
            }
        }
        _ => paths(word, cwd, &mut candidates),
    }
    candidates.sort();
    candidates.dedup();
    Completion { start, candidates }
}

/// Пути, начинающиеся с `word`: относительный — от `cwd`.
fn paths(word: &str, cwd: &str, out: &mut Vec<(String, bool)>) {
    let (dir_part, name_part) = match word.rfind('/') {
        Some(at) => (&word[..=at], &word[at + 1..]),
        None => ("", word),
    };
    let dir = if dir_part.starts_with('/') {
        String::from(dir_part)
    } else if cwd.ends_with('/') {
        alloc::format!("{cwd}{dir_part}")
    } else {
        alloc::format!("{cwd}/{dir_part}")
    };
    let listed = if dir.len() > 1 { dir.trim_end_matches('/') } else { "/" };
    let Some(Ok(entries)) = fs::list(listed) else {
        return;
    };
    for entry in entries {
        if entry.name == "." || entry.name == ".." || !entry.name.starts_with(name_part) {
            continue;
        }
        // Скрытые — только если их начали набирать с точки.
        if entry.name.starts_with('.') && !name_part.starts_with('.') {
            continue;
        }
        out.push((alloc::format!("{dir_part}{}", entry.name), entry.kind == NodeKind::Directory));
    }
}
