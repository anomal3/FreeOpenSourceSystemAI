//! Desktop icons of the system (24.11, Roman: «как „Параметры значков
//! рабочего стола“ Windows»): «Этот компьютер», the user's files, «Сеть»,
//! «Корзина», «Панель управления» (our «Параметры») and our own «Терминал»
//! and «О системе».
//!
//! Each can be shown or hidden, renamed on the desktop (F2 or «Переименовать»
//! — the name lives in `desktop.cfg`, nothing is renamed on disk) and given
//! another picture from the icon theme («Сменить значок…» of the dialog
//! `/bin/deskicon`). Lines of `desktop.cfg`:
//!
//! * `desk_icons=computer:on user:off network:off trash:on control:off terminal:on about:on`
//! * `desk_labels=computer=Мой ПК|trash=Мусор`
//! * `desk_art=computer=place-local-disk`
//!
//! The dialog talks to the desktop through `ui deskicons …` (allowed to every
//! user by `SYS_KERNEL_COMMAND`: it changes only the desktop icons):
//! `ui deskicons` prints the state, `show KEY`, `hide KEY`, `icon KEY NAME`,
//! `icon KEY default`, `label KEY default`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;

use super::window::App;

/// A system icon of the desktop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Special {
    Computer,
    User,
    Network,
    Trash,
    Control,
    Terminal,
    About,
}

/// Order of the dialog's checkboxes (Windows' five, then ours).
pub const ALL: [Special; 7] = [Special::Computer, Special::User, Special::Network, Special::Trash, Special::Control, Special::Terminal, Special::About];

/// Command of «Этот компьютер» — «Файлы» on the computer page.
pub const COMPUTER_COMMAND: &str = "/bin/files computer:";
/// Command of «Сеть»: the network page of «Параметры» (network places of
/// other machines are not browsed yet — ROADMAP).
pub const NETWORK_COMMAND: &str = "special:network";

impl Special {
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Special::Computer => "computer",
            Special::User => "user",
            Special::Network => "network",
            Special::Trash => "trash",
            Special::Control => "control",
            Special::Terminal => "terminal",
            Special::About => "about",
        }
    }

    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        ALL.into_iter().find(|special| special.key() == key)
    }

    /// Shown until the person decides (Windows shows only the trash; ours
    /// showed «Мой компьютер», «Терминал», «О системе» and the trash).
    const fn default_shown(self) -> bool {
        matches!(self, Special::Computer | Special::Trash | Special::Terminal | Special::About)
    }

    /// Caption of the dialog's checkbox.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Special::Computer => "Компьютер",
            Special::User => "Файлы пользователя",
            Special::Network => "Сеть",
            Special::Trash => "Корзина",
            Special::Control => "Панель управления",
            Special::Terminal => "Терминал",
            Special::About => "О системе",
        }
    }

    /// Name under the icon when nobody renamed it.
    #[must_use]
    pub fn default_label(self) -> String {
        String::from(match self {
            // Windows 98 called it «Мой компьютер» (classic look, 22.11).
            Special::Computer if mini_ui::theme::is_classic() => "Мой компьютер",
            Special::Computer => "Этот компьютер",
            Special::User => return user_name(),
            Special::Network => "Сеть",
            Special::Trash => super::trash::LABEL,
            Special::Control => "Панель управления",
            Special::Terminal => App::Terminal.caption(),
            Special::About => App::About.caption(),
        })
    }

    /// Picture of the icon theme when nobody chose another; `None` — the
    /// icon's own drawing (the trash: full or empty).
    #[must_use]
    pub const fn default_art(self) -> Option<&'static str> {
        match self {
            Special::Computer => Some("place-this-pc"),
            Special::User => Some("folder-user"),
            Special::Network => Some("network"),
            Special::Control => Some("settings"),
            Special::Trash | Special::Terminal | Special::About => None,
        }
    }

    /// Command of a launching icon (`icons::Kind::Program`).
    #[must_use]
    pub fn command(self) -> Option<String> {
        match self {
            Special::Computer => Some(String::from(COMPUTER_COMMAND)),
            Special::User => Some(alloc::format!("/bin/files {}", home_dir())),
            Special::Network => Some(String::from(NETWORK_COMMAND)),
            _ => None,
        }
    }
}

/// The person's home: the parent of the desktop folder.
#[must_use]
pub fn home_dir() -> String {
    let desk = super::context::desktop_dir();
    match desk.trim_end_matches('/').rsplit_once('/') {
        Some((parent, _)) if !parent.is_empty() => String::from(parent),
        _ => String::from("/"),
    }
}

/// The user's name — the last part of the home path, as Windows names the
/// folder icon.
fn user_name() -> String {
    let home = home_dir();
    match home.rsplit('/').next() {
        Some(name) if !name.is_empty() => String::from(name),
        _ => String::from("Файлы пользователя"),
    }
}

struct State {
    /// Bits by `ALL` order: decided by the person; bits: shown.
    decided: u8,
    shown: u8,
    labels: Vec<(Special, String)>,
    art: Vec<(Special, String)>,
}

static STATE: SpinLock<State> = SpinLock::new(State { decided: 0, shown: 0, labels: Vec::new(), art: Vec::new() });

fn bit(special: Special) -> u8 {
    1 << ALL.iter().position(|known| *known == special).unwrap_or(0)
}

/// Is the icon on the desktop.
#[must_use]
pub fn shown(special: Special) -> bool {
    let state = STATE.lock();
    if state.decided & bit(special) != 0 { state.shown & bit(special) != 0 } else { special.default_shown() }
}

/// Name under the icon.
#[must_use]
pub fn label(special: Special) -> String {
    STATE.lock().labels.iter().find(|(known, _)| *known == special).map(|(_, label)| label.clone()).unwrap_or_else(|| special.default_label())
}

/// Picture chosen by the person (`None` — the default).
#[must_use]
pub fn chosen_art(special: Special) -> Option<String> {
    STATE.lock().art.iter().find(|(known, _)| *known == special).map(|(_, name)| name.clone())
}

/// Picture of the icon now.
#[must_use]
pub fn art(special: Special) -> Option<String> {
    chosen_art(special).or_else(|| special.default_art().map(String::from))
}

/// Read the three lines from `desktop.cfg` (`prefs::adopt`).
pub fn adopt(text: &str) {
    let mut state = STATE.lock();
    if let Some(line) = sysconf::value(text, "desk_icons") {
        for pair in line.split_whitespace() {
            let Some((key, value)) = pair.split_once(':') else { continue };
            let Some(special) = Special::from_key(key) else { continue };
            state.decided |= bit(special);
            if value == "on" {
                state.shown |= bit(special);
            } else {
                state.shown &= !bit(special);
            }
        }
    }
    for (line_key, art) in [("desk_labels", false), ("desk_art", true)] {
        if let Some(line) = sysconf::value(text, line_key) {
            let list: Vec<(Special, String)> = line
                .split('|')
                .filter_map(|pair| pair.split_once('='))
                .filter_map(|(key, value)| Some((Special::from_key(key.trim())?, String::from(value.trim()))))
                .filter(|(_, value)| !value.is_empty())
                .collect();
            if art {
                state.art = list;
            } else {
                state.labels = list;
            }
        }
    }
}

fn store() {
    let (icons, labels, art) = {
        let state = STATE.lock();
        let icons: Vec<String> = ALL
            .iter()
            .map(|special| {
                let on = if state.decided & bit(*special) != 0 { state.shown & bit(*special) != 0 } else { special.default_shown() };
                alloc::format!("{}:{}", special.key(), if on { "on" } else { "off" })
            })
            .collect();
        let join = |list: &Vec<(Special, String)>| list.iter().map(|(special, value)| alloc::format!("{}={value}", special.key())).collect::<Vec<_>>().join("|");
        (icons.join(" "), join(&state.labels), join(&state.art))
    };
    for (key, value) in [("desk_icons", icons), ("desk_labels", labels), ("desk_art", art)] {
        if let Err(err) = super::prefs::store_line(key, &value) {
            crate::kprintln!("  desktop     : {key} not saved: {err}");
        }
    }
}

/// Show or hide the icon.
pub fn set_shown(special: Special, on: bool) {
    {
        let mut state = STATE.lock();
        state.decided |= bit(special);
        if on {
            state.shown |= bit(special);
        } else {
            state.shown &= !bit(special);
        }
    }
    store();
    crate::kprintln!("  desktop     : desktop icon '{}' ({}) {}", special.key(), special.title(), if on { "shown" } else { "hidden" });
}

/// Rename the icon (`None` or the default name — back to the default).
pub fn set_label(special: Special, name: Option<&str>) {
    let name = name.map(str::trim).filter(|name| !name.is_empty() && *name != special.default_label() && !name.contains(['|', '\n']));
    {
        let mut state = STATE.lock();
        state.labels.retain(|(known, _)| *known != special);
        if let Some(name) = name {
            state.labels.push((special, String::from(name)));
        }
    }
    store();
    crate::kprintln!("  desktop     : desktop icon '{}' named '{}'", special.key(), label(special));
}

/// Another picture (`None` — the default one).
pub fn set_art(special: Special, name: Option<&str>) {
    let name = name.filter(|name| !name.is_empty() && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-'));
    {
        let mut state = STATE.lock();
        state.art.retain(|(known, _)| *known != special);
        if let Some(name) = name {
            state.art.push((special, String::from(name)));
        }
    }
    store();
    crate::kprintln!("  desktop     : desktop icon '{}' picture {}", special.key(), art(special).unwrap_or_else(|| String::from("default")));
}

/// `ui deskicons …` (the dialog and the bench): see the head of the file.
/// Prints the state after the change: `deskicons KEY on|off art LABEL`.
pub fn command(words: &str) {
    let mut parts = words.split_whitespace();
    let verb = parts.next().unwrap_or("");
    let special = parts.next().and_then(Special::from_key);
    let rest: Vec<&str> = parts.collect();
    let changed = match (verb, special) {
        ("show", Some(special)) => {
            set_shown(special, true);
            true
        }
        ("hide", Some(special)) => {
            set_shown(special, false);
            true
        }
        ("icon", Some(special)) => {
            let name = rest.first().copied().filter(|name| *name != "default");
            set_art(special, name);
            true
        }
        ("label", Some(special)) => {
            let joined = rest.join(" ");
            set_label(special, (joined != "default").then_some(joined.as_str()));
            true
        }
        ("", _) => false,
        _ => {
            crate::shell::print(format_args!("  deskicons: show|hide|icon|label KEY …; keys: computer user network trash control terminal about\n"));
            return;
        }
    };
    if changed {
        refresh();
    }
    for special in ALL {
        crate::shell::print(format_args!(
            "deskicons {} {} {} {}\n",
            special.key(),
            if shown(special) { "on" } else { "off" },
            art(special).unwrap_or_else(|| String::from("-")),
            label(special)
        ));
    }
}

/// The desktop takes the new icons (from a task, not under the desktop's
/// lock).
fn refresh() {
    for _ in 0..500 {
        let done = super::with_desktop(|desktop| {
            desktop.reload_icons();
            desktop.repaint_all();
            desktop.present();
        });
        if done.is_some() || !super::graphics() {
            return;
        }
        crate::sched::sleep_ms(2);
    }
}

/// The special icon behind a desktop item, by its kind and command.
#[must_use]
pub fn of_item(kind: &super::icons::Kind, path: Option<&str>) -> Option<Special> {
    use super::icons::Kind;
    match kind {
        Kind::Trash(_) => Some(Special::Trash),
        Kind::App(App::Terminal) => Some(Special::Terminal),
        Kind::App(App::About) => Some(Special::About),
        Kind::App(App::Settings) => Some(Special::Control),
        Kind::Program(_) => match path? {
            COMPUTER_COMMAND => Some(Special::Computer),
            NETWORK_COMMAND => Some(Special::Network),
            command if Special::User.command().as_deref() == Some(command) => Some(Special::User),
            _ => None,
        },
        _ => None,
    }
}
