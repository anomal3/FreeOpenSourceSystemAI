//! «Сетевые подключения» — ncpa.cpl (25.11.2026, Roman: «Панель управления ›
//! Сеть и Интернет › Сетевые подключения», с его снимками Windows).
//!
//! # What it is
//!
//! The adapters as tiles — name, state or address, the device — with a red
//! cross when there is no connection and a grey picture when the adapter is
//! switched off; network cards without a driver in this kernel are tiles too
//! («Нет драйвера»). The command bar and the context menu: «Отключить» /
//! «Включить», «Состояние», «Диагностика», «Переименовать», «Свойства».
//! «Состояние» — the Windows dialog (IPv4, media, duration, speed, bytes sent
//! and received) with «Сведения…» (MAC, DHCP, address, mask, lease, gateway,
//! DHCP server, DNS); «Свойства» — the components list; «IP версии 4
//! (TCP/IPv4)» — «Получить IP-адрес автоматически» / «Использовать следующий
//! IP-адрес», DNS automatically or by hand. Dialogs are panels over the
//! window, one over another, as Windows stacks them.
//!
//! # How it talks to the kernel
//!
//! The shell command `netcfg` through `SYS_KERNEL_COMMAND` (see
//! `kernel::net::adapter`): reading is open to everyone, changes are root's;
//! a refusal comes back as a line `error <why>` and is shown as it is. Changes
//! apply at once and are kept in the settings store (system scope).
//!
//! # Arguments
//!
//! `--status`, `--details`, `--props`, `--ipv4`, `--diag`, `--rename` open
//! that dialog at start (the bench and «Параметры › Сеть»).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use mini_ui::filedlg::Line;
use mini_ui::glyphicon::Icon;
use mini_ui::iconset::{self, Fi};
use mini_ui::kit::{self, Entry};
use mini_ui::paint::{self, Ctx, Weight};
use mini_ui::typeface::Role;
use mini_ui::{Color, Rect, Surface, draw, theme};
use user_progs::app::{self, App, SizeLimits, Spec};
use user_abi::{WIN_KEY_DOWN, WIN_KEY_F1, WIN_KEY_LEFT, WIN_KEY_MENU, WIN_KEY_RIGHT, WIN_KEY_UP, WIN_MOD_ALT, WIN_MOD_CTRL, WIN_MOD_SHIFT};
use user_progs::{Args, SysInfo, devices, kernel_command, monotonic_ms, println};

const SPEC: Spec = Spec { name: "ncpa", title: "Network Connections", period_ms: 1_000 };

const ENTER: u32 = '\n' as u32;
const ESCAPE: u32 = 0x1B;
const TAB: u32 = '\t' as u32;
const F2: u32 = WIN_KEY_F1 + 1;
const F5: u32 = WIN_KEY_F1 + 4;

const ADDR_H: u32 = 40;
const CMD_H: u32 = 44;
const STATUS_H: u32 = 28;
const TILE_W: u32 = 310;
const TILE_H: u32 = 66;
const DOUBLE_MS: u64 = 450;

// ---------------------------------------------------------------------------
// What the kernel says
// ---------------------------------------------------------------------------

/// `netcfg` answer: `key value` lines.
#[derive(Clone, Default)]
struct Info {
    pairs: Vec<(String, String)>,
}

impl Info {
    fn parse(text: &str) -> Option<Self> {
        if text.trim() == "none" || text.trim().is_empty() {
            return None;
        }
        let pairs = text
            .lines()
            .filter_map(|line| line.trim().split_once(' ').map(|(k, v)| (String::from(k), String::from(v.trim()))))
            .collect();
        Some(Self { pairs })
    }

    fn get(&self, key: &str) -> &str {
        self.pairs.iter().find(|(k, _)| k == key).map_or("", |(_, v)| v.as_str())
    }

    /// An address, `None` for «-».
    fn ip(&self, key: &str) -> Option<&str> {
        Some(self.get(key)).filter(|v| !v.is_empty() && *v != "-")
    }

    fn num(&self, key: &str) -> u64 {
        self.get(key).parse().unwrap_or(0)
    }

    fn enabled(&self) -> bool {
        self.get("enabled") == "1"
    }

    fn dhcp(&self) -> bool {
        self.get("ipv4") == "dhcp"
    }

    fn cable_out(&self) -> bool {
        self.get("media") == "down"
    }

    fn connected(&self) -> bool {
        self.enabled() && self.ip("address").is_some()
    }

    /// The second line of the tile.
    fn state_text(&self) -> String {
        if !self.enabled() {
            String::from("Отключено")
        } else if self.cable_out() {
            String::from(if self.get("kind") == "wireless" { "Нет подключения" } else { "Сетевой кабель не подключен" })
        } else if let Some(address) = self.ip("address") {
            format!("Подключено · {address}")
        } else if self.dhcp() {
            String::from("Получение сетевого адреса…")
        } else {
            String::from("Нет адреса IPv4")
        }
    }

    /// «IPv4-подключение» of «Состояние».
    fn ipv4_text(&self) -> &'static str {
        if !self.connected() {
            "Без доступа к сети"
        } else if self.ip("gateway").is_some() && self.ip("dns1").is_some() {
            "Интернет"
        } else {
            "Без доступа к Интернету"
        }
    }
}

/// Run `netcfg …`. `Err` — the refusal in words.
fn netcfg(args: &str) -> Result<String, String> {
    let line = if args.is_empty() { String::from("netcfg") } else { format!("netcfg {args}") };
    let mut buffer = vec![0u8; user_abi::KERNEL_COMMAND_OUT];
    let mut got = kernel_command(&line, &mut buffer);
    // The kernel shell is busy for a moment — ask again.
    for _ in 0..5 {
        if got != user_abi::ERR_AGAIN {
            break;
        }
        user_progs::sleep_ms(60);
        got = kernel_command(&line, &mut buffer);
    }
    if got < 0 {
        return Err(match got {
            user_abi::ERR_PERMISSION => String::from("Изменять параметры сети может только администратор (root)"),
            code => format!("Команда не выполнена: код {code}"),
        });
    }
    let text = String::from_utf8_lossy(&buffer[..got as usize]).to_string();
    if let Some(why) = text.lines().find_map(|line| line.trim().strip_prefix("error ")) {
        return Err(String::from(why));
    }
    Ok(text)
}

/// A network card on the PCI bus that no driver of this kernel serves.
#[derive(Clone)]
struct Undriven {
    place: String,
    id: String,
    what: String,
    /// Driver there, card not used (the stack runs one card).
    idle: bool,
}

fn undriven() -> Vec<Undriven> {
    let mut buffer = vec![0u8; 16 * 1024];
    let got = devices(&mut buffer);
    if got <= 0 {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buffer[..(got as usize).min(buffer.len())]).to_string();
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 6 || f[0] != "pci" || !(f[3].contains("ethernet") || f[3].contains("network")) {
                return None;
            }
            match f[5] {
                "none" => Some(Undriven { place: f[1].to_string(), id: f[2].to_string(), what: f[3].to_string(), idle: false }),
                "idle" => Some(Undriven { place: f[1].to_string(), id: f[2].to_string(), what: f[3].to_string(), idle: true }),
                _ => None,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Words
// ---------------------------------------------------------------------------

fn speed_text(info: &Info) -> String {
    match info.get("speed").parse::<u64>() {
        Ok(mbps) if mbps >= 1000 => format!("{}.{} Гбит/с", mbps / 1000, mbps % 1000 / 100),
        Ok(mbps) if mbps > 0 => format!("{mbps} Мбит/с"),
        _ => String::from("нет данных"),
    }
}

fn duration_text(seconds: u64) -> String {
    let (d, h, m, s) = (seconds / 86_400, seconds / 3600 % 24, seconds / 60 % 60, seconds % 60);
    if d > 0 { format!("{d} дн. {h:02}:{m:02}:{s:02}") } else { format!("{h:02}:{m:02}:{s:02}") }
}

fn group_digits(value: u64) -> String {
    let text = value.to_string();
    let mut out = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i > 0 && (text.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Minutes east of UTC from `/etc/system.cfg`.
fn local_offset() -> i32 {
    let fd = user_progs::open("/etc/system.cfg");
    if fd < 0 {
        return 0;
    }
    let mut buffer = [0u8; 2048];
    let got = user_progs::read(fd, &mut buffer);
    user_progs::close(fd);
    if got <= 0 {
        return 0;
    }
    let text = core::str::from_utf8(&buffer[..got as usize]).unwrap_or("");
    sysconf::value(text, "timezone").and_then(sysconf::parse_offset).unwrap_or(0)
}

/// «25 ноября 2026 г. 13:40:10» — `unix` seconds, local time.
fn long_date(unix: i64) -> String {
    const MONTHS: [&str; 12] =
        ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
    let local = calendar::DateTime::from_unix(unix + i64::from(local_offset()) * 60);
    let month = MONTHS.get(usize::from(local.month).saturating_sub(1)).copied().unwrap_or("");
    format!("{} {month} {} г. {:02}:{:02}:{:02}", local.day, local.year, local.hour, local.minute, local.second)
}

/// Is the text an IPv4 address (four numbers 0–255).
fn ipv4_ok(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|n| n <= 255))
}

fn ipv4_value(text: &str) -> u32 {
    text.split('.').fold(0u32, |acc, p| (acc << 8) | p.parse::<u32>().unwrap_or(0))
}

fn mask_ok(text: &str) -> bool {
    if !ipv4_ok(text) {
        return false;
    }
    let value = ipv4_value(text);
    value != 0 && value.leading_ones() + value.trailing_zeros() == 32
}

/// The mask Windows proposes for an address (by its class).
fn class_mask(address: &str) -> &'static str {
    match address.split('.').next().and_then(|first| first.parse::<u32>().ok()) {
        Some(0..=127) => "255.0.0.0",
        Some(128..=191) => "255.255.0.0",
        _ => "255.255.255.0",
    }
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

/// «Свойства: IP версии 4 (TCP/IPv4)».
struct Ipv4Form {
    auto_ip: bool,
    auto_dns: bool,
    /// address, mask, gateway, dns1, dns2.
    fields: [Line; 5],
    focus: usize,
    error: Option<String>,
}

impl Ipv4Form {
    fn of(info: &Info) -> Self {
        let pick = |live: &str, saved: &str| {
            let value = info.ip(live).or_else(|| info.ip(saved)).unwrap_or("");
            Line::new(value)
        };
        let auto_ip = info.dhcp();
        let auto_dns = info.get("dns") != "manual";
        // Hand-typed values come back as they were; with DHCP — what the
        // lease gave, so «Использовать следующий» starts from it.
        let fields = [
            pick("address", "saved_address"),
            pick("netmask", "saved_netmask"),
            pick("gateway", "saved_gateway"),
            if auto_dns { Line::new(info.ip("saved_dns1").unwrap_or("")) } else { pick("dns1", "saved_dns1") },
            if auto_dns { Line::new(info.ip("saved_dns2").unwrap_or("")) } else { pick("dns2", "saved_dns2") },
        ];
        Self { auto_ip, auto_dns, fields, focus: 0, error: None }
    }

    fn field_enabled(&self, index: usize) -> bool {
        if index < 3 { !self.auto_ip } else { !self.auto_dns }
    }

    /// Check what is typed; `Err` — the Windows-like message.
    fn check(&self) -> Result<(), String> {
        let text = |i: usize| self.fields[i].text.trim();
        if !self.auto_ip {
            if !ipv4_ok(text(0)) {
                return Err(format!("«{}» — не IP-адрес. Введите четыре числа от 0 до 255 через точку.", text(0)));
            }
            if !mask_ok(text(1)) {
                return Err(String::from("Маска подсети недопустима: единицы слева подряд, например 255.255.255.0."));
            }
            if !text(2).is_empty() {
                if !ipv4_ok(text(2)) {
                    return Err(format!("«{}» — не адрес основного шлюза.", text(2)));
                }
                let (a, m, g) = (ipv4_value(text(0)), ipv4_value(text(1)), ipv4_value(text(2)));
                if a & m != g & m {
                    return Err(String::from("Основной шлюз не в той же подсети, что IP-адрес и маска."));
                }
            }
        }
        if !self.auto_dns {
            if !ipv4_ok(text(3)) {
                return Err(String::from("Укажите предпочитаемый DNS-сервер: четыре числа от 0 до 255 через точку."));
            }
            if !text(4).is_empty() && !ipv4_ok(text(4)) {
                return Err(format!("«{}» — не адрес альтернативного DNS-сервера.", text(4)));
            }
        }
        Ok(())
    }
}

/// «Диагностика сетей»: steps one per frame.
struct Diag {
    stage: u8,
    lines: Vec<(String, bool)>,
    next_at: u64,
    deadline: u64,
    done: bool,
}

enum Dialog {
    Status,
    Details,
    Props { selected: usize },
    Ipv4(Ipv4Form),
    Diag(Diag),
}

impl Dialog {
    /// Size in layout points.
    fn size(&self) -> (u32, u32) {
        match self {
            Dialog::Status => (380, 452),
            Dialog::Details => (400, 476),
            Dialog::Props { .. } => (400, 520),
            Dialog::Ipv4(_) => (450, 520),
            Dialog::Diag(_) => (450, 330),
        }
    }
}

/// The components of «Свойства» (name, checked, can be opened, about).
const COMPONENTS: [(&str, bool, &str); 5] = [
    ("IP версии 4 (TCP/IPv4)", true, "Протокол TCP/IP — сетевой протокол по умолчанию. Адрес, маска, шлюз и DNS — кнопкой «Свойства»."),
    ("IP версии 6 (TCP/IPv6)", false, "IPv6 в FossAi пока не поддерживается: стек ядра говорит только IPv4."),
    ("Клиент для сетей SMB", true, "Доступ к общим папкам компьютеров Windows (\\\\имя\\папка) в «Файлах»."),
    ("Ответчик обнаружения (LLMNR, mDNS)", true, "Компьютер виден в сети по имени и сам находит соседей."),
    ("Служба времени SNTP", true, "Время из сети: спрашивается после получения адреса по DHCP."),
];

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Act {
    Toggle,
    Status,
    Diag,
    Rename,
    Props,
    DevMgr,
}

struct Menu {
    at: (i32, i32),
    acts: Vec<Act>,
}

struct Ncpa {
    info: Option<Info>,
    cards: Vec<Undriven>,
    /// 0 — the adapter (when there is one), then the cards without a driver.
    selected: Option<usize>,
    dialogs: Vec<Dialog>,
    menu: Option<Menu>,
    rename: Option<Line>,
    message: Option<(String, bool)>,
    root: bool,
    last_click: (u64, usize),
    /// Where the keyboard's context menu opens: under the selected tile
    /// (remembered when drawn).
    anchor: core::cell::Cell<(i32, i32)>,
    done: bool,
}

/// Window parts.
struct Plan {
    addr: Rect,
    cmd: Rect,
    refresh: Rect,
    buttons: Vec<(Rect, Act)>,
    tiles: Vec<Rect>,
    status: Rect,
}

impl Ncpa {
    fn new(open: Option<&str>) -> Self {
        let mut app = Self {
            info: netcfg("").ok().and_then(|text| Info::parse(&text)),
            cards: undriven(),
            selected: None,
            dialogs: Vec::new(),
            menu: None,
            rename: None,
            message: None,
            root: user_progs::uid() == 0,
            last_click: (0, usize::MAX),
            anchor: core::cell::Cell::new((140, 160)),
            done: false,
        };
        if app.info.is_some() || !app.cards.is_empty() {
            app.selected = Some(0);
        }
        match &app.info {
            Some(info) => println(&format!(
                "ncpa: adapter '{}' {} enabled={} ipv4={} address={} media={}",
                info.get("name"),
                info.get("driver"),
                info.get("enabled"),
                info.get("ipv4"),
                info.get("address"),
                info.get("media")
            )),
            None => println("ncpa: no adapter in the stack"),
        }
        for card in &app.cards {
            println(&format!("ncpa: card without a driver {} {} {}", card.place, card.id, card.what));
        }
        match open {
            Some("--status") => app.open_status(),
            Some("--details") => {
                app.open_status();
                app.push(Dialog::Details);
            }
            Some("--props") => app.open_props(),
            Some("--ipv4") => {
                app.open_props();
                app.open_ipv4();
            }
            Some("--diag") => app.start_diag(),
            Some("--rename") => app.start_rename(),
            _ => {}
        }
        app
    }

    fn tile_count(&self) -> usize {
        usize::from(self.info.is_some()) + self.cards.len()
    }

    /// Is the selected tile the adapter (not a card without a driver).
    fn adapter_selected(&self) -> bool {
        self.info.is_some() && self.selected == Some(0)
    }

    fn reload(&mut self) {
        self.info = netcfg("").ok().and_then(|text| Info::parse(&text));
    }

    fn push(&mut self, dialog: Dialog) {
        let name = match &dialog {
            Dialog::Status => "status",
            Dialog::Details => "details",
            Dialog::Props { .. } => "properties",
            Dialog::Ipv4(_) => "ipv4",
            Dialog::Diag(_) => "diagnostics",
        };
        println(&format!("ncpa: dialog {name} opened"));
        self.dialogs.push(dialog);
    }

    fn open_status(&mut self) {
        if self.info.as_ref().is_some_and(Info::enabled) {
            self.push(Dialog::Status);
        } else {
            self.message = Some((String::from("Адаптер отключён: состояние появится, когда он будет включён"), false));
        }
    }

    fn open_props(&mut self) {
        if self.info.is_some() {
            self.push(Dialog::Props { selected: 0 });
        }
    }

    fn open_ipv4(&mut self) {
        if let Some(info) = &self.info {
            let form = Ipv4Form::of(info);
            self.push(Dialog::Ipv4(form));
        }
    }

    fn start_diag(&mut self) {
        if self.info.is_some() {
            let now = monotonic_ms();
            self.push(Dialog::Diag(Diag { stage: 0, lines: Vec::new(), next_at: now + 300, deadline: 0, done: false }));
        }
    }

    fn start_rename(&mut self) {
        if let Some(info) = &self.info {
            self.selected = Some(0);
            self.rename = Some(Line::selected(info.get("name")));
            println("ncpa: renaming");
        }
    }

    /// Run a change and say what came of it.
    fn change(&mut self, args: &str, ok: &str) -> bool {
        let result = netcfg(args);
        println(&format!("ncpa: netcfg {args}: {}", result.as_ref().map_or_else(|why| format!("error {why}"), |_| String::from("ok"))));
        self.reload();
        match result {
            Ok(_) => {
                self.message = Some((String::from(ok), true));
                true
            }
            Err(why) => {
                self.message = Some((why, false));
                false
            }
        }
    }

    fn toggle(&mut self) {
        let Some(on) = self.info.as_ref().map(Info::enabled) else {
            return;
        };
        if on {
            self.change("disable", "Адаптер отключён");
            // The status of a switched-off adapter makes no sense.
            self.dialogs.retain(|dialog| !matches!(dialog, Dialog::Status | Dialog::Details));
        } else {
            self.change("enable", "Адаптер включён");
        }
    }

    fn act(&mut self, act: Act) {
        self.menu = None;
        match act {
            Act::Toggle => self.toggle(),
            Act::Status => self.open_status(),
            Act::Diag => self.start_diag(),
            Act::Rename => self.start_rename(),
            Act::Props => self.open_props(),
            Act::DevMgr => {
                user_progs::spawn("/bin/devmgr");
            }
        }
    }

    fn acts(&self) -> Vec<Act> {
        if self.adapter_selected() {
            vec![Act::Toggle, Act::Status, Act::Diag, Act::Rename, Act::Props]
        } else if self.selected.is_some() {
            vec![Act::DevMgr]
        } else {
            Vec::new()
        }
    }

    fn act_title(&self, act: Act) -> &'static str {
        let on = self.info.as_ref().is_some_and(Info::enabled);
        match act {
            Act::Toggle if on => "Отключить",
            Act::Toggle => "Включить",
            Act::Status => "Состояние",
            Act::Diag => "Диагностика",
            Act::Rename => "Переименовать",
            Act::Props => "Свойства",
            Act::DevMgr => "Диспетчер устройств",
        }
    }

    fn act_enabled(&self, act: Act) -> bool {
        let on = self.info.as_ref().is_some_and(Info::enabled);
        match act {
            Act::Status | Act::Diag => on,
            _ => true,
        }
    }

    fn plan(&self, ctx: Ctx, area: Rect) -> Plan {
        let px = |v: u32| ctx.px(v);
        let addr = Rect::new(area.x, area.y, area.w, px(ADDR_H));
        let cmd = Rect::new(area.x, addr.bottom(), area.w, px(CMD_H));
        let status = Rect::new(area.x, area.bottom() - px(STATUS_H) as i32, area.w, px(STATUS_H));
        let refresh = Rect::new(addr.right() - px(44) as i32, addr.y + px(6) as i32, px(34), px(28));
        let face = ctx.face(Role::Body);
        let mut x = cmd.x + px(10) as i32;
        let mut buttons = Vec::new();
        for act in self.acts() {
            let w = face.width(self.act_title(act)) + px(16 + 8 + 24);
            buttons.push((Rect::new(x, cmd.y + px(7) as i32, w, px(30)), act));
            x += w as i32 + px(4) as i32;
        }
        let body = Rect::new(area.x + px(14) as i32, cmd.bottom() + px(12) as i32, area.w.saturating_sub(px(28)), 0);
        let per_row = (body.w / px(TILE_W + 8)).max(1);
        let tiles = (0..self.tile_count())
            .map(|n| {
                let (col, row) = (n as u32 % per_row, n as u32 / per_row);
                Rect::new(body.x + (col * px(TILE_W + 8)) as i32, body.y + (row * px(TILE_H + 6)) as i32, px(TILE_W), px(TILE_H))
            })
            .collect();
        Plan { addr, cmd, refresh, buttons, tiles, status }
    }

    fn menu_entries(&self, acts: &[Act]) -> Vec<Entry<'static>> {
        acts.iter()
            .map(|act| {
                let entry = Entry::new(self.act_title(*act)).disabled(!self.act_enabled(*act));
                match act {
                    Act::Toggle => entry.icon(Icon::Shield),
                    Act::Status => entry.bold(),
                    Act::Diag => entry.icon(Icon::Search),
                    Act::Rename => entry.group(1),
                    Act::Props => entry.icon(Icon::Shield).group(2),
                    Act::DevMgr => entry.icon(Icon::Devices),
                }
            })
            .collect()
    }

    fn open_menu(&mut self, at: (i32, i32)) {
        let acts = self.acts();
        if !acts.is_empty() {
            println("ncpa: context menu");
            self.menu = Some(Menu { at, acts });
        }
    }

    // ── Dialog geometry ─────────────────────────────────────────────────────

    fn dialog_rect(&self, ctx: Ctx, area: Rect, depth: usize) -> Rect {
        let (w, h) = self.dialogs[depth].size();
        let (w, h) = (ctx.px(w).min(area.w.saturating_sub(ctx.px(8))), ctx.px(h).min(area.h.saturating_sub(ctx.px(8))));
        let shift = ctx.px(22) as i32 * depth as i32;
        let x = (area.x + (area.w as i32 - w as i32) / 2 + shift).min(area.right() - w as i32).max(area.x);
        let y = (area.y + (area.h as i32 - h as i32) / 2 + shift).min(area.bottom() - h as i32).max(area.y);
        Rect::new(x, y, w, h)
    }

    fn adapter_name(&self) -> String {
        self.info.as_ref().map_or_else(|| String::from("Ethernet"), |info| String::from(info.get("name")))
    }

    // ── Applying the IPv4 form ──────────────────────────────────────────────

    fn apply_ipv4(&mut self) -> bool {
        let Some(Dialog::Ipv4(form)) = self.dialogs.last() else {
            return false;
        };
        if let Err(why) = form.check() {
            if let Some(Dialog::Ipv4(form)) = self.dialogs.last_mut() {
                form.error = Some(why);
            }
            return false;
        }
        let t = |i: usize| String::from(form.fields[i].text.trim());
        let ip_line = if form.auto_ip {
            String::from("ipv4 dhcp")
        } else {
            let gateway = if t(2).is_empty() { String::from("-") } else { t(2) };
            format!("ipv4 static {} {} {gateway}", t(0), t(1))
        };
        let dns_line = if form.auto_dns { String::from("dns auto") } else { format!("dns {} {}", t(3), if t(4).is_empty() { String::from("-") } else { t(4) }) };
        let (was_dhcp, was_auto_dns) = self.info.as_ref().map_or((true, true), |info| (info.dhcp(), info.get("dns") != "manual"));
        // Order: DNS by hand may be set only once the address is by hand,
        // and DNS «автоматически» only once it is DHCP again.
        let mut steps = Vec::new();
        if form.auto_ip {
            steps.push(ip_line);
            steps.push(dns_line);
        } else {
            steps.push(dns_line);
            steps.push(ip_line);
        }
        let mut failed = None;
        for step in &steps {
            // Nothing to send for what did not change.
            if (step == "ipv4 dhcp" && was_dhcp) || (step == "dns auto" && was_auto_dns && was_dhcp) {
                continue;
            }
            let result = netcfg(step);
            println(&format!("ncpa: netcfg {step}: {}", result.as_ref().map_or_else(|why| format!("error {why}"), |_| String::from("ok"))));
            if let Err(why) = result {
                failed = Some(why);
                break;
            }
        }
        self.reload();
        match failed {
            Some(why) => {
                if let Some(Dialog::Ipv4(form)) = self.dialogs.last_mut() {
                    form.error = Some(why);
                }
                false
            }
            None => {
                self.message = Some((String::from("Параметры IPv4 применены"), true));
                true
            }
        }
    }

    // ── Diagnostics ─────────────────────────────────────────────────────────

    fn diag_step(&mut self) -> bool {
        let now = monotonic_ms();
        let Some(Dialog::Diag(diag)) = self.dialogs.last() else {
            return false;
        };
        if diag.done || now < diag.next_at {
            return false;
        }
        let stage = diag.stage;
        let deadline = diag.deadline;
        self.reload();
        let info = self.info.clone().unwrap_or_default();
        let mut lines: Vec<(String, bool)> = Vec::new();
        let mut next = stage + 1;
        let mut done = false;
        let mut new_deadline = deadline;
        match stage {
            0 => {
                if info.enabled() {
                    lines.push((String::from("Адаптер включён"), true));
                } else {
                    let ok = netcfg("enable").is_ok();
                    println(&format!("ncpa: diagnostics enabled the adapter: {ok}"));
                    lines.push((String::from(if ok { "Адаптер был отключён — включён" } else { "Адаптер отключён, а включить его может только администратор" }), ok));
                    done = !ok;
                }
            }
            1 => {
                if info.cable_out() {
                    lines.push((String::from("Сетевой кабель не подключён: проверьте кабель и порт коммутатора"), false));
                    done = true;
                } else {
                    let speed = speed_text(&info);
                    lines.push((format!("Связь есть ({speed})"), true));
                }
            }
            2 => {
                if info.dhcp() {
                    match netcfg("renew") {
                        Ok(_) => lines.push((String::from("Запрошен новый адрес у сервера DHCP…"), true)),
                        Err(why) => lines.push((why, false)),
                    }
                    new_deadline = now + 15_000;
                } else {
                    next = 3;
                    lines.push((String::from("IP-адрес задан вручную"), true));
                }
            }
            3 => {
                if let Some(address) = info.ip("address") {
                    let server = info.ip("dhcp_server").map(|s| format!(" от {s}")).unwrap_or_default();
                    lines.push((format!("Адрес IPv4 {address}{server}"), true));
                } else if info.dhcp() && now < deadline {
                    // Still waiting for the lease.
                    next = 3;
                } else {
                    lines.push((String::from("Адрес IPv4 не получен: сервер DHCP не ответил"), false));
                    done = true;
                }
            }
            4 => match info.ip("gateway") {
                Some(gateway) => lines.push((format!("Основной шлюз {gateway}"), true)),
                None => lines.push((String::from("Основной шлюз не задан: доступна только своя подсеть"), false)),
            },
            5 => match info.ip("dns1") {
                Some(dns) => {
                    let found = user_progs::resolve("ya.ru");
                    match found {
                        Some(address) => {
                            let b = address.to_be_bytes();
                            lines.push((format!("DNS-сервер {dns} отвечает: ya.ru — {}.{}.{}.{}", b[0], b[1], b[2], b[3]), true));
                            lines.push((String::from("Проблем не обнаружено"), true));
                        }
                        None => lines.push((format!("DNS-сервер {dns} не ответил на вопрос о ya.ru"), false)),
                    }
                    done = true;
                }
                None => {
                    lines.push((String::from("DNS-сервер не задан: имена сайтов не разрешаются"), false));
                    done = true;
                }
            },
            _ => done = true,
        }
        for (line, ok) in &lines {
            println(&format!("ncpa: diagnostics {}: {line}", if *ok { "ok" } else { "problem" }));
        }
        if done {
            println("ncpa: diagnostics finished");
        }
        if let Some(Dialog::Diag(diag)) = self.dialogs.last_mut() {
            diag.lines.extend(lines);
            diag.stage = next;
            diag.deadline = new_deadline;
            diag.done = done;
            diag.next_at = now + if next == 3 && stage == 3 { 500 } else { 350 };
        }
        true
    }

    // ── Input on dialogs ────────────────────────────────────────────────────

    fn close_top(&mut self) {
        self.dialogs.pop();
    }

    fn dialog_click(&mut self, ctx: Ctx, area: Rect, x: i32, y: i32) -> bool {
        let depth = self.dialogs.len() - 1;
        let rect = self.dialog_rect(ctx, area, depth);
        let frame = frame_layout(ctx, rect);
        if frame.close.contains(x, y) {
            self.close_top();
            return true;
        }
        if !rect.contains(x, y) {
            return false;
        }
        match self.dialogs.last_mut() {
            Some(Dialog::Status) => {
                let l = status_layout(ctx, frame.body);
                if l.details.contains(x, y) {
                    self.push(Dialog::Details);
                } else if l.props.contains(x, y) {
                    self.open_props();
                } else if l.toggle.contains(x, y) {
                    self.toggle();
                } else if l.diag.contains(x, y) {
                    self.start_diag();
                } else if l.close.contains(x, y) {
                    self.close_top();
                }
            }
            Some(Dialog::Details) => {
                if details_layout(ctx, frame.body).close.contains(x, y) {
                    self.close_top();
                }
            }
            Some(Dialog::Props { selected }) => {
                let l = props_layout(ctx, frame.body);
                if let Some(n) = l.rows.iter().position(|r| r.contains(x, y)) {
                    let now = monotonic_ms();
                    let double = *selected == n && now.saturating_sub(self.last_click.0) <= DOUBLE_MS && self.last_click.1 == 1000 + n;
                    *selected = n;
                    self.last_click = (now, 1000 + n);
                    if double && n == 0 {
                        self.open_ipv4();
                    }
                } else if l.properties.contains(x, y) {
                    if *selected == 0 {
                        self.open_ipv4();
                    }
                } else if l.configure.contains(x, y) {
                    user_progs::spawn("/bin/devmgr");
                } else if l.ok.contains(x, y) || l.cancel.contains(x, y) {
                    self.close_top();
                }
            }
            Some(Dialog::Ipv4(form)) => {
                let l = ipv4_layout(ctx, frame.body);
                if l.auto_ip.contains(x, y) {
                    form.auto_ip = true;
                    form.auto_dns = form.auto_dns || form.auto_ip;
                    form.error = None;
                } else if l.manual_ip.contains(x, y) {
                    form.auto_ip = false;
                    // A hand-set address needs DNS by hand (as in Windows).
                    form.auto_dns = false;
                    form.focus = 0;
                    form.error = None;
                } else if l.auto_dns.contains(x, y) && form.auto_ip {
                    form.auto_dns = true;
                    form.error = None;
                } else if l.manual_dns.contains(x, y) {
                    form.auto_dns = false;
                    form.focus = 3;
                    form.error = None;
                } else if let Some(n) = l.fields.iter().position(|r| r.contains(x, y)) {
                    if form.field_enabled(n) {
                        form.focus = n;
                        let inner = field_inner(ctx, l.fields[n]);
                        form.fields[n].click(ctx.face(Role::Body), inner.w, x - inner.x);
                    }
                } else if l.ok.contains(x, y) {
                    if self.apply_ipv4() {
                        self.close_top();
                    }
                } else if l.cancel.contains(x, y) {
                    println("ncpa: ipv4 cancelled");
                    self.close_top();
                }
            }
            Some(Dialog::Diag(_)) => {
                if diag_layout(ctx, frame.body).close.contains(x, y) {
                    self.close_top();
                }
            }
            None => {}
        }
        true
    }

    fn dialog_key(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        if code == ESCAPE {
            println("ncpa: dialog closed (Esc)");
            self.close_top();
            return true;
        }
        match self.dialogs.last_mut() {
            Some(Dialog::Ipv4(form)) => {
                // Accelerators of the Windows dialog: Alt+O «Получить
                // IP-адрес автоматически», Alt+S «Использовать следующий»,
                // Alt+B DNS automatically, Alt+E DNS by hand.
                if mods & WIN_MOD_ALT != 0 {
                    match char::from_u32(latin).map(|c| c.to_ascii_lowercase()) {
                        Some('o') => {
                            form.auto_ip = true;
                        }
                        Some('s') => {
                            form.auto_ip = false;
                            form.auto_dns = false;
                            form.focus = 0;
                        }
                        Some('b') if form.auto_ip => form.auto_dns = true,
                        Some('e') => {
                            form.auto_dns = false;
                            form.focus = 3;
                        }
                        _ => return false,
                    }
                    form.error = None;
                    println(&format!("ncpa: ipv4 form auto_ip={} auto_dns={}", form.auto_ip, form.auto_dns));
                    return true;
                }
                match code {
                    ENTER => {
                        if self.apply_ipv4() {
                            self.close_top();
                        }
                    }
                    TAB => {
                        let back = mods & WIN_MOD_SHIFT != 0;
                        for _ in 0..5 {
                            form.focus = if back { (form.focus + 4) % 5 } else { (form.focus + 1) % 5 };
                            if form.field_enabled(form.focus) {
                                break;
                            }
                        }
                    }
                    _ => {
                        let focus = form.focus;
                        if !form.field_enabled(focus) {
                            return false;
                        }
                        // Digits and dots only, as in the address control.
                        if mods & WIN_MOD_CTRL == 0 && code < 0x100 && !(code as u8 as char).is_ascii_digit() && code != '.' as u32 && code >= 0x20 && code != 0x7F {
                            return true;
                        }
                        let changed = form.fields[focus].key(code, mods, latin).unwrap_or(false);
                        // The mask Windows proposes on leaving the address.
                        if changed && focus == 0 && form.fields[1].text.is_empty() && ipv4_ok(form.fields[0].text.trim()) {
                            form.fields[1] = Line::new(class_mask(form.fields[0].text.trim()));
                        }
                        form.error = None;
                    }
                }
                true
            }
            Some(Dialog::Props { selected }) => {
                match code {
                    WIN_KEY_UP => *selected = selected.saturating_sub(1),
                    WIN_KEY_DOWN => *selected = (*selected + 1).min(COMPONENTS.len() - 1),
                    ENTER if *selected == 0 => self.open_ipv4(),
                    ENTER => self.close_top(),
                    _ => return false,
                }
                true
            }
            Some(_) if code == ENTER => {
                self.close_top();
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Layouts
// ---------------------------------------------------------------------------

struct Frame {
    title: Rect,
    close: Rect,
    body: Rect,
}

fn frame_layout(ctx: Ctx, rect: Rect) -> Frame {
    let px = |v: u32| ctx.px(v);
    let title = Rect::new(rect.x, rect.y, rect.w, px(40));
    let close = Rect::new(rect.right() - px(40) as i32, rect.y + px(4) as i32, px(34), px(32));
    let body = Rect::new(rect.x + px(16) as i32, title.bottom() + px(6) as i32, rect.w.saturating_sub(px(32)), rect.h.saturating_sub(px(40 + 6 + 14)));
    Frame { title, close, body }
}

/// Bottom-right buttons of a dialog body: `count` of them, the last rightmost.
fn bottom_buttons(ctx: Ctx, body: Rect, count: usize) -> Vec<Rect> {
    let (bw, bh, gap) = (ctx.px(96), ctx.px(30), ctx.px(8) as i32);
    let y = body.bottom() - bh as i32;
    (0..count)
        .map(|n| {
            let from_right = (count - 1 - n) as i32;
            Rect::new(body.right() - bw as i32 - from_right * (bw as i32 + gap), y, bw, bh)
        })
        .collect()
}

struct StatusLayout {
    rows_y: i32,
    details: Rect,
    activity_y: i32,
    props: Rect,
    toggle: Rect,
    diag: Rect,
    close: Rect,
}

fn status_layout(ctx: Ctx, body: Rect) -> StatusLayout {
    let px = |v: u32| ctx.px(v);
    let rows_y = body.y + px(30) as i32;
    let details = Rect::new(body.x + px(12) as i32, rows_y + px(5 * 24 + 6) as i32, px(110), px(30));
    let activity_y = details.bottom() + px(16) as i32;
    let row_y = activity_y + px(116) as i32;
    let props = Rect::new(body.x, row_y, px(108), px(30));
    let toggle = Rect::new(props.right() + px(8) as i32, row_y, px(108), px(30));
    let diag = Rect::new(toggle.right() + px(8) as i32, row_y, px(108), px(30));
    let close = bottom_buttons(ctx, body, 1)[0];
    StatusLayout { rows_y, details, activity_y, props, toggle, diag, close }
}

struct DetailsLayout {
    table: Rect,
    close: Rect,
}

fn details_layout(ctx: Ctx, body: Rect) -> DetailsLayout {
    let close = bottom_buttons(ctx, body, 1)[0];
    let table = Rect::new(body.x, body.y + ctx.px(26) as i32, body.w, (close.y - ctx.px(12) as i32 - body.y - ctx.px(26) as i32).max(0) as u32);
    DetailsLayout { table, close }
}

struct PropsLayout {
    device: Rect,
    configure: Rect,
    list: Rect,
    rows: Vec<Rect>,
    install: Rect,
    remove: Rect,
    properties: Rect,
    about: Rect,
    ok: Rect,
    cancel: Rect,
}

fn props_layout(ctx: Ctx, body: Rect) -> PropsLayout {
    let px = |v: u32| ctx.px(v);
    let device = Rect::new(body.x, body.y + px(24) as i32, body.w, px(34));
    let configure = Rect::new(body.right() - px(120) as i32, device.bottom() + px(8) as i32, px(120), px(30));
    let list = Rect::new(body.x, configure.bottom() + px(30) as i32, body.w, px(COMPONENTS.len() as u32 * 28 + 8));
    let rows = (0..COMPONENTS.len())
        .map(|n| Rect::new(list.x + px(4) as i32, list.y + px(4) as i32 + (n as u32 * px(28)) as i32, list.w.saturating_sub(px(8)), px(28)))
        .collect();
    let by = list.bottom() + px(10) as i32;
    let bw = (body.w.saturating_sub(px(16))) / 3;
    let install = Rect::new(body.x, by, bw, px(30));
    let remove = Rect::new(install.right() + px(8) as i32, by, bw, px(30));
    let properties = Rect::new(remove.right() + px(8) as i32, by, bw, px(30));
    let buttons = bottom_buttons(ctx, body, 2);
    let about = Rect::new(body.x, properties.bottom() + px(12) as i32, body.w, (buttons[0].y - px(12) as i32 - properties.bottom() - px(12) as i32).max(px(40) as i32) as u32);
    PropsLayout { device, configure, list, rows, install, remove, properties, about, ok: buttons[0], cancel: buttons[1] }
}

struct Ipv4Layout {
    intro_y: i32,
    auto_ip: Rect,
    manual_ip: Rect,
    ip_group: Rect,
    auto_dns: Rect,
    manual_dns: Rect,
    dns_group: Rect,
    fields: [Rect; 5],
    error_y: i32,
    ok: Rect,
    cancel: Rect,
}

fn ipv4_layout(ctx: Ctx, body: Rect) -> Ipv4Layout {
    let px = |v: u32| ctx.px(v);
    let intro_y = body.y + px(4) as i32;
    let auto_ip = Rect::new(body.x + px(8) as i32, intro_y + px(70) as i32, body.w.saturating_sub(px(16)), px(26));
    let manual_ip = Rect::new(auto_ip.x, auto_ip.bottom() + px(2) as i32, auto_ip.w, px(26));
    let ip_group = Rect::new(body.x, manual_ip.y + px(13) as i32, body.w, px(3 * 34 + 26));
    let field_w = px(170);
    let fx = body.right() - px(14) as i32 - field_w as i32;
    let row = |top: i32, n: u32| Rect::new(fx, top + (n * px(34)) as i32, field_w, px(28));
    let ip_top = manual_ip.bottom() + px(8) as i32;
    let auto_dns = Rect::new(auto_ip.x, ip_group.bottom() + px(14) as i32, auto_ip.w, px(26));
    let manual_dns = Rect::new(auto_ip.x, auto_dns.bottom() + px(2) as i32, auto_ip.w, px(26));
    let dns_group = Rect::new(body.x, manual_dns.y + px(13) as i32, body.w, px(2 * 34 + 26));
    let dns_top = manual_dns.bottom() + px(8) as i32;
    let fields = [row(ip_top, 0), row(ip_top, 1), row(ip_top, 2), row(dns_top, 0), row(dns_top, 1)];
    let buttons = bottom_buttons(ctx, body, 2);
    let error_y = dns_group.bottom() + px(8) as i32;
    Ipv4Layout { intro_y, auto_ip, manual_ip, ip_group, auto_dns, manual_dns, dns_group, fields, error_y, ok: buttons[0], cancel: buttons[1] }
}

struct DiagLayout {
    lines_y: i32,
    close: Rect,
}

fn diag_layout(ctx: Ctx, body: Rect) -> DiagLayout {
    DiagLayout { lines_y: body.y + ctx.px(34) as i32, close: bottom_buttons(ctx, body, 1)[0] }
}

fn field_inner(ctx: Ctx, rect: Rect) -> Rect {
    Rect::new(rect.x + ctx.px(8) as i32, rect.y, rect.w.saturating_sub(ctx.px(16)), rect.h)
}

// ---------------------------------------------------------------------------
// Drawing helpers
// ---------------------------------------------------------------------------

fn white() -> Color {
    Color::rgb(0xFF, 0xFF, 0xFF)
}

/// A disabled look over something already drawn.
fn grey_out(ctx: Ctx, s: &mut Surface, rect: Rect, bg: Color) {
    draw::rounded(s, rect, ctx.px(6), bg, 150);
}

fn button(ctx: Ctx, s: &mut Surface, rect: Rect, weight: Weight, label: &str, enabled: bool, bg: Color) {
    paint::button(ctx, s, rect, weight, label, false);
    if !enabled {
        grey_out(ctx, s, rect, bg);
    }
}

/// A button with the administrator shield before the label (as Windows marks
/// what needs elevation).
fn shield_button(ctx: Ctx, s: &mut Surface, rect: Rect, label: &str, enabled: bool, bg: Color) {
    let p = ctx.palette;
    paint::button(ctx, s, rect, Weight::Normal, "", false);
    let g = ctx.px(16);
    let face = ctx.face(Role::Body);
    let total = g + ctx.px(6) + face.width(label);
    let x = rect.x + (rect.w as i32 - total as i32).max(0) / 2;
    iconset::draw(s, Fi::ShieldAdmin, x, rect.y + (rect.h as i32 - g as i32) / 2, g, p.acc, 255);
    paint::text(ctx, s, Role::Body, x + (g + ctx.px(6)) as i32, paint::baseline(ctx, Role::Body, rect), label, p.ink);
    if !enabled {
        grey_out(ctx, s, rect, bg);
    }
}

fn radio(ctx: Ctx, s: &mut Surface, rect: Rect, on: bool, label: &str, enabled: bool) {
    let p = ctx.palette;
    let r = ctx.px(8);
    let cx = rect.x + r as i32 + ctx.px(1) as i32;
    let cy = rect.y + rect.h as i32 / 2;
    let ink = if enabled { p.ink } else { p.ink5 };
    draw::circle_outline(s, cx, cy, r, ctx.px(1).max(1), if on { p.acc } else { p.ink4 }, 255);
    if on {
        draw::circle(s, cx, cy, r / 2, if enabled { p.acc } else { p.ink5 }, 255);
    }
    paint::text(ctx, s, Role::Body, cx + r as i32 + ctx.px(8) as i32, paint::baseline(ctx, Role::Body, rect), label, ink);
}

fn checkbox(ctx: Ctx, s: &mut Surface, x: i32, rect: Rect, on: bool, enabled: bool, bg: Color) -> i32 {
    let p = ctx.palette;
    let side = ctx.px(16);
    let mark = Rect::new(x, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
    if on {
        draw::rounded(s, mark, ctx.px(3), if enabled { p.acc } else { p.ink5 }, 255);
        iconset::draw(s, Fi::Check, mark.x + ctx.px(2) as i32, mark.y + ctx.px(2) as i32, ctx.px(12), white(), 255);
    } else {
        draw::rounded(s, mark, ctx.px(3), p.sunk.over(bg), 255);
        draw::rounded_stroke(s, mark, ctx.px(3), p.line3.color, p.line3.alpha.saturating_add(60));
    }
    mark.right() + ctx.px(8) as i32
}

/// A group title with a rule after it, as in Windows dialogs.
fn group_title(ctx: Ctx, s: &mut Surface, x: i32, y: i32, w: u32, title: &str) {
    let p = ctx.palette;
    let width = paint::text(ctx, s, Role::Strong, x, y, title, p.ink2);
    let line_y = y + i32::from(ctx.face(Role::Strong).line) / 2;
    let from = x + width as i32 + ctx.px(8) as i32;
    draw::hline(s, from, line_y, (x + w as i32 - from).max(0) as u32, p.line3.color, p.line3.alpha);
}

fn text_field(ctx: Ctx, s: &mut Surface, rect: Rect, line: &Line, focused: bool, enabled: bool, bg: Color) {
    let p = ctx.palette;
    let fill = if enabled { p.sunk.over(bg) } else { bg };
    draw::rounded(s, rect, ctx.px(4), fill, 255);
    let edge = if focused { p.acc } else { p.line3.color };
    draw::rounded_stroke(s, rect, ctx.px(4), edge, if focused { 255 } else { p.line3.alpha.saturating_add(50) });
    let inner = field_inner(ctx, rect);
    if enabled {
        line.draw(ctx.on(fill), s, inner, focused, p.ink);
    } else {
        // Windows shows the dots of an empty disabled address control.
        let shown = if line.text.is_empty() { "   .     .     ." } else { line.text.as_str() };
        paint::text_clipped(ctx.on(fill), s, Role::Body, inner.x, paint::baseline(ctx, Role::Body, inner), inner.w, shown, p.ink5);
    }
}

/// `label ……… value` row.
fn pair_row(ctx: Ctx, s: &mut Surface, x: i32, right: i32, y: i32, label: &str, value: &str) {
    let p = ctx.palette;
    paint::text(ctx, s, Role::Body, x, y, label, p.ink2);
    paint::text_right(ctx, s, Role::Body, right, y, value, p.ink);
}

/// The adapter picture: the theme icon, grey when off, a red cross when not
/// connected.
fn adapter_art(ctx: Ctx, s: &mut Surface, rect: Rect, art: &str, grey: bool, cross: bool, bg: Color) {
    let p = ctx.palette;
    if !user_progs::icons::draw(s, rect, art) {
        let fi = if art == "wifi" { Fi::Wifi } else { Fi::NetworkWired };
        iconset::draw(s, fi, rect.x, rect.y, rect.w, p.acc, 255);
    }
    if grey {
        draw::rounded(s, rect, 0, bg, 140);
    }
    if cross {
        let side = rect.w * 9 / 24;
        let (x, y) = (rect.x, rect.bottom() - side as i32);
        let weight = ctx.px(3).max(2);
        let red = Color::rgb(0xE8, 0x11, 0x23);
        draw::line(s, (x, y), (x + side as i32, y + side as i32), weight, red, 255);
        draw::line(s, (x + side as i32, y), (x, y + side as i32), weight, red, 255);
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

impl App for Ncpa {
    fn key_with(&mut self, code: u32, mods: i32, latin: u32) -> bool {
        if let Some(menu) = &self.menu {
            match code {
                ESCAPE => self.menu = None,
                ENTER => {
                    let first = menu.acts.iter().copied().find(|act| self.act_enabled(*act));
                    if let Some(act) = first {
                        self.act(act);
                    }
                }
                _ => return false,
            }
            return true;
        }
        if !self.dialogs.is_empty() {
            return self.dialog_key(code, mods, latin);
        }
        if let Some(line) = &mut self.rename {
            match code {
                ENTER => {
                    let name = String::from(line.text.trim());
                    self.rename = None;
                    self.change(&format!("rename {}", if name.is_empty() { "-" } else { name.as_str() }), "Подключение переименовано");
                }
                ESCAPE => {
                    println("ncpa: rename cancelled");
                    self.rename = None;
                }
                _ => {
                    line.key(code, mods, latin);
                }
            }
            return true;
        }
        let count = self.tile_count();
        match code {
            ENTER if mods & WIN_MOD_ALT != 0 => self.open_props(),
            ENTER => match self.info.as_ref().map(Info::enabled) {
                Some(true) if self.adapter_selected() => self.open_status(),
                Some(false) if self.adapter_selected() => self.toggle(),
                _ if self.selected.is_some() => self.act(Act::DevMgr),
                _ => {}
            },
            F2 if self.adapter_selected() => self.start_rename(),
            F5 => {
                self.reload();
                self.cards = undriven();
            }
            WIN_KEY_MENU => self.open_menu(self.anchor.get()),
            // Shift+F10 — the context menu, as in Windows.
            code if code == WIN_KEY_F1 + 9 && mods & WIN_MOD_SHIFT != 0 => self.open_menu(self.anchor.get()),
            WIN_KEY_LEFT | WIN_KEY_UP if count > 0 => self.selected = Some(self.selected.map_or(0, |n| n.saturating_sub(1))),
            WIN_KEY_RIGHT | WIN_KEY_DOWN if count > 0 => self.selected = Some(self.selected.map_or(0, |n| (n + 1).min(count - 1))),
            ESCAPE if self.message.is_some() => self.message = None,
            _ => return false,
        }
        true
    }

    fn click(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if let Some(menu) = self.menu.take() {
            let entries = self.menu_entries(&menu.acts);
            let rect = kit::place(menu.at, kit::menu_size(ctx, &entries), area);
            if let Some(n) = kit::menu_hit(ctx, rect, &entries, x, y) {
                let act = menu.acts[n];
                if self.act_enabled(act) {
                    println(&format!("ncpa: menu '{}'", self.act_title(act)));
                    self.act(act);
                } else {
                    self.menu = Some(menu);
                }
            }
            return true;
        }
        if !self.dialogs.is_empty() {
            return self.dialog_click(ctx, area, x, y);
        }
        if self.rename.is_some() {
            let name = self.rename.take().map(|line| String::from(line.text.trim())).unwrap_or_default();
            self.change(&format!("rename {}", if name.is_empty() { "-" } else { name.as_str() }), "Подключение переименовано");
        }
        let plan = self.plan(ctx, area);
        if plan.refresh.contains(x, y) {
            self.reload();
            self.cards = undriven();
            return true;
        }
        if let Some((_, act)) = plan.buttons.iter().find(|(rect, _)| rect.contains(x, y)) {
            let act = *act;
            if self.act_enabled(act) {
                println(&format!("ncpa: button '{}'", self.act_title(act)));
                self.act(act);
            }
            return true;
        }
        match plan.tiles.iter().position(|rect| rect.contains(x, y)) {
            Some(n) => {
                let now = monotonic_ms();
                let double = self.last_click.1 == n && now.saturating_sub(self.last_click.0) <= DOUBLE_MS;
                self.selected = Some(n);
                self.last_click = (now, n);
                if double {
                    self.last_click = (0, usize::MAX);
                    self.key_with(ENTER, 0, 0);
                }
            }
            None => self.selected = None,
        }
        true
    }

    fn click_right(&mut self, area: Rect, ctx: Ctx, x: i32, y: i32) -> bool {
        if !self.dialogs.is_empty() || self.rename.is_some() {
            return false;
        }
        let plan = self.plan(ctx, area);
        match plan.tiles.iter().position(|rect| rect.contains(x, y)) {
            Some(n) => {
                self.selected = Some(n);
                self.open_menu((x, y));
            }
            None => {
                self.menu = None;
                self.selected = None;
            }
        }
        true
    }

    fn step(&mut self) -> bool {
        self.diag_step()
    }

    fn tick(&mut self, _info: &SysInfo) -> bool {
        // The form being typed is not touched; everything else follows the
        // kernel (a lease came, the cable went).
        self.reload();
        true
    }

    fn wants_quit(&self) -> bool {
        self.done
    }

    fn quits_on_q(&self) -> bool {
        self.dialogs.is_empty() && self.rename.is_none() && self.menu.is_none()
    }

    fn size_limits(&self) -> SizeLimits {
        SizeLimits::min(560, 560)
    }

    fn title(&self) -> Option<String> {
        Some(String::from("Network Connections\x1fСетевые подключения"))
    }

    fn draw(&self, s: &mut Surface, area: Rect, ctx: Ctx) {
        let p = ctx.palette;
        let px = |v: u32| ctx.px(v);
        let bg = theme::window_bg();
        s.fill(area, bg);
        let plan = self.plan(ctx, area);

        // Address bar: where we are, as in Explorer.
        let bar = kit::toolbar(ctx, s, plan.addr);
        let mut x = plan.addr.x + px(14) as i32;
        let g = px(18);
        iconset::draw(s, Fi::NetworkWired, x, plan.addr.y + (plan.addr.h as i32 - g as i32) / 2, g, p.acc, 255);
        x += (g + px(10)) as i32;
        let base = paint::baseline(bar, Role::Body, plan.addr);
        for (index, part) in ["Панель управления", "Сеть и Интернет", "Сетевые подключения"].iter().enumerate() {
            if index > 0 {
                let a = px(12);
                iconset::draw(s, Fi::ChevronRight, x, plan.addr.y + (plan.addr.h as i32 - a as i32) / 2, a, p.ink4, 255);
                x += (a + px(8)) as i32;
            }
            x += paint::text(bar, s, if index == 2 { Role::Strong } else { Role::Body }, x, base, part, if index == 2 { p.ink } else { p.ink3 }) as i32;
            x += px(8) as i32;
        }
        paint::button(bar, s, plan.refresh, Weight::Ghost, "", false);
        iconset::draw(s, Fi::Refresh, plan.refresh.x + (plan.refresh.w as i32 - px(16) as i32) / 2, plan.refresh.y + (plan.refresh.h as i32 - px(16) as i32) / 2, px(16), p.ink2, 255);

        // Command bar: what can be done with the selected tile.
        let cmd_fill = ctx.flat(p.panel);
        s.fill(plan.cmd, cmd_fill);
        draw::hline(s, plan.cmd.x, plan.cmd.bottom() - 1, plan.cmd.w, p.line.color, p.line.alpha);
        let cmd = ctx.on(cmd_fill);
        if plan.buttons.is_empty() {
            paint::text(cmd, s, Role::Body, plan.cmd.x + px(16) as i32, paint::baseline(cmd, Role::Body, plan.cmd), "Выберите подключение", p.ink4);
        }
        for (rect, act) in &plan.buttons {
            let enabled = self.act_enabled(*act);
            paint::button(cmd, s, *rect, Weight::Ghost, "", false);
            let icon = match act {
                Act::Toggle => Fi::ShieldAdmin,
                Act::Status => Fi::Info,
                Act::Diag => Fi::Search,
                Act::Rename => Fi::Rename,
                Act::Props => Fi::ShieldAdmin,
                Act::DevMgr => Fi::Layers,
            };
            let ink = if enabled { p.ink } else { p.ink5 };
            let ix = rect.x + px(10) as i32;
            iconset::draw(s, icon, ix, rect.y + (rect.h as i32 - px(16) as i32) / 2, px(16), if enabled { p.ink2 } else { p.ink5 }, 255);
            paint::text(cmd, s, Role::Body, ix + px(24) as i32, paint::baseline(cmd, Role::Body, *rect), self.act_title(*act), ink);
        }

        // Tiles.
        let mut tiles: Vec<(String, String, String, &'static str, bool, bool)> = Vec::new();
        if let Some(info) = &self.info {
            let art = if info.get("kind") == "wireless" { "wifi" } else { "network" };
            let cross = info.enabled() && !info.connected();
            tiles.push((String::from(info.get("name")), info.state_text(), String::from(info.get("device")), art, !info.enabled(), cross));
        }
        for card in &self.cards {
            let state = if card.idle { "Не используется: работает другая карта" } else { "Нет драйвера" };
            tiles.push((format!("Сетевой адаптер {}", card.id), String::from(state), format!("PCI {} · {}", card.place, card.what), "network", card.idle, !card.idle));
        }
        for (index, (rect, (name, state, device, art, grey, cross))) in plan.tiles.iter().zip(tiles.iter()).enumerate() {
            let on = self.selected == Some(index);
            if on {
                paint::row(ctx, s, *rect, paint::RowState::Selected);
                self.anchor.set((rect.x + px(60) as i32, rect.bottom()));
            }
            let side = px(48);
            let pic = Rect::new(rect.x + px(8) as i32, rect.y + (rect.h as i32 - side as i32) / 2, side, side);
            adapter_art(ctx, s, pic, art, *grey, *cross, bg);
            let tx = pic.right() + px(10) as i32;
            let room = (rect.right() - tx - px(6) as i32).max(0) as u32;
            let line = i32::from(ctx.face(Role::Body).line);
            let top = rect.y + (rect.h as i32 - line * 3) / 2;
            if index == 0 && self.info.is_some() && self.rename.is_some() {
                let field = Rect::new(tx - px(4) as i32, top - px(3) as i32, room, (line + px(6) as i32) as u32);
                if let Some(edit) = &self.rename {
                    text_field(ctx, s, field, edit, true, true, bg);
                }
            } else {
                paint::text_clipped(ctx, s, Role::Strong, tx, top, room, name, p.ink);
            }
            paint::text_clipped(ctx, s, Role::Body, tx, top + line, room, state, if *cross { p.ink3 } else { p.ink2 });
            paint::text_clipped(ctx, s, Role::Body, tx, top + line * 2, room, device, p.ink4);
        }
        if self.tile_count() == 0 {
            paint::text(ctx, s, Role::Body, area.x + px(20) as i32, plan.cmd.bottom() + px(24) as i32, "Сетевых адаптеров нет: ни одной карты, которую знает ядро", p.ink4);
        }

        // Status bar.
        let status = match &self.message {
            Some((text, _)) => text.clone(),
            None => {
                let mut text = format!("Элементов: {}", self.tile_count());
                if self.selected.is_some() {
                    text.push_str(" · Выбран 1 элемент");
                }
                if !self.root {
                    text.push_str(" · Изменять параметры может только администратор (root)");
                }
                text
            }
        };
        kit::status_bar(ctx, s, plan.status, &status);

        // Dialogs, one over another.
        if !self.dialogs.is_empty() {
            draw::blend_rect(s, area, Color::rgb(0, 0, 0), if theme::is_dark() { 90 } else { 50 });
        }
        for depth in 0..self.dialogs.len() {
            let rect = self.dialog_rect(ctx, area, depth);
            self.draw_dialog(ctx, s, rect, depth);
        }

        if let Some(menu) = &self.menu {
            let entries = self.menu_entries(&menu.acts);
            let rect = kit::place(menu.at, kit::menu_size(ctx, &entries), area);
            kit::draw_menu(ctx, s, rect, &entries, usize::MAX);
        }
    }
}

impl Ncpa {
    fn draw_dialog(&self, ctx: Ctx, s: &mut Surface, rect: Rect, depth: usize) {
        let inner = kit::card(ctx, s, rect);
        let p = inner.palette;
        let bg = inner.under;
        let frame = frame_layout(ctx, rect);
        let name = self.adapter_name();
        let title = match &self.dialogs[depth] {
            Dialog::Status => format!("Состояние — {name}"),
            Dialog::Details => String::from("Сведения о сетевом подключении"),
            Dialog::Props { .. } => format!("{name}: свойства"),
            Dialog::Ipv4(_) => String::from("Свойства: IP версии 4 (TCP/IPv4)"),
            Dialog::Diag(_) => String::from("Диагностика сетей"),
        };
        let g = ctx.px(16);
        iconset::draw(s, Fi::NetworkWired, frame.title.x + ctx.px(14) as i32, frame.title.y + (frame.title.h as i32 - g as i32) / 2, g, p.acc, 255);
        paint::text_clipped(inner, s, Role::Strong, frame.title.x + ctx.px(38) as i32, paint::baseline(inner, Role::Strong, frame.title), frame.title.w.saturating_sub(ctx.px(84)), &title, p.ink);
        iconset::draw(s, Fi::X, frame.close.x + (frame.close.w as i32 - g as i32) / 2, frame.close.y + (frame.close.h as i32 - g as i32) / 2, g, p.ink3, 255);
        draw::hline(s, rect.x, frame.title.bottom(), rect.w, p.line.color, p.line.alpha);
        let info = self.info.clone().unwrap_or_default();
        let body = frame.body;
        let line_h = i32::from(ctx.face(Role::Body).line) + ctx.px(6) as i32;
        match &self.dialogs[depth] {
            Dialog::Status => {
                let l = status_layout(ctx, body);
                group_title(inner, s, body.x, body.y + ctx.px(4) as i32, body.w, "Подключение");
                let media = match info.get("media") {
                    "down" => "Кабель не подключён",
                    _ if info.enabled() => "Подключено",
                    _ => "Отключено",
                };
                let rows = [
                    ("IPv4-подключение:", String::from(info.ipv4_text())),
                    ("IPv6-подключение:", String::from("Не поддерживается")),
                    ("Состояние среды:", String::from(media)),
                    ("Длительность:", duration_text(info.num("uptime"))),
                    ("Скорость:", speed_text(&info)),
                ];
                for (n, (label, value)) in rows.iter().enumerate() {
                    pair_row(inner, s, body.x + ctx.px(12) as i32, body.right() - ctx.px(8) as i32, l.rows_y + n as i32 * ctx.px(24) as i32, label, value);
                }
                button(inner, s, l.details, Weight::Normal, "Сведения…", true, bg);
                group_title(inner, s, body.x, l.activity_y, body.w, "Активность");
                let mid = body.x + body.w as i32 / 2;
                let ay = l.activity_y + ctx.px(32) as i32;
                paint::text_right(inner, s, Role::Body, mid - ctx.px(40) as i32, ay, "Отправлено", p.ink2);
                paint::text(inner, s, Role::Body, mid + ctx.px(40) as i32, ay, "Принято", p.ink2);
                let pic = ctx.px(48);
                adapter_art(inner, s, Rect::new(mid - pic as i32 / 2, ay - ctx.px(14) as i32, pic, pic), "network", false, false, bg);
                let by = ay + ctx.px(50) as i32;
                paint::text(inner, s, Role::Body, body.x + ctx.px(12) as i32, by, "Байт:", p.ink2);
                paint::text_right(inner, s, Role::Body, mid - ctx.px(14) as i32, by, &group_digits(info.num("tx_bytes")), p.ink);
                draw::vline(s, mid, by - ctx.px(2) as i32, ctx.px(18), p.line3.color, p.line3.alpha);
                paint::text(inner, s, Role::Body, mid + ctx.px(14) as i32, by, &group_digits(info.num("rx_bytes")), p.ink);
                shield_button(inner, s, l.props, "Свойства", true, bg);
                shield_button(inner, s, l.toggle, if info.enabled() { "Отключить" } else { "Включить" }, true, bg);
                button(inner, s, l.diag, Weight::Normal, "Диагностика", true, bg);
                button(inner, s, l.close, Weight::Normal, "Закрыть", true, bg);
            }
            Dialog::Details => {
                let l = details_layout(ctx, body);
                paint::text(inner, s, Role::Body, body.x, body.y + ctx.px(2) as i32, "Сведения о подключении к сети:", p.ink2);
                let table_bg = p.sunk.over(bg);
                draw::rounded(s, l.table, ctx.px(4), table_bg, 255);
                draw::rounded_stroke(s, l.table, ctx.px(4), p.line3.color, p.line3.alpha);
                let t = inner.on(table_bg);
                let col = l.table.x + (l.table.w as i32 * 42 / 100);
                let lx = l.table.x + ctx.px(10) as i32;
                let mut y = l.table.y + ctx.px(8) as i32;
                paint::text(t, s, Role::Strong, lx, y, "Свойство", p.ink2);
                paint::text(t, s, Role::Strong, col, y, "Значение", p.ink2);
                y += line_h;
                draw::hline(s, l.table.x + ctx.px(4) as i32, y - ctx.px(3) as i32, l.table.w.saturating_sub(ctx.px(8)), p.line.color, p.line.alpha);
                let dash = |v: Option<&str>| String::from(v.unwrap_or(""));
                let now = user_progs::time_now() as i64;
                let (got, ends) = if info.dhcp() && info.ip("dhcp_server").is_some() && now > 0 {
                    let got = now - info.num("lease_age") as i64;
                    (long_date(got), long_date(got + info.num("lease_seconds") as i64))
                } else {
                    (String::new(), String::new())
                };
                let mut rows: Vec<(&str, String)> = vec![
                    ("DNS-суффикс подключения", String::new()),
                    ("Описание", String::from(info.get("device"))),
                    ("Физический адрес", String::from(info.get("mac"))),
                    ("DHCP включен", String::from(if info.dhcp() { "Да" } else { "Нет" })),
                    ("Адрес IPv4", dash(info.ip("address"))),
                    ("Маска подсети IPv4", dash(info.ip("netmask"))),
                ];
                if info.dhcp() {
                    rows.push(("Аренда получена", got));
                    rows.push(("Аренда истекает", ends));
                }
                rows.push(("Шлюз по умолчанию IPv4", dash(info.ip("gateway"))));
                if info.dhcp() {
                    rows.push(("DHCP-сервер IPv4", dash(info.ip("dhcp_server"))));
                }
                rows.push(("DNS-серверы IPv4", dash(info.ip("dns1"))));
                if let Some(dns2) = info.ip("dns2") {
                    rows.push(("", String::from(dns2)));
                }
                rows.push(("WINS-сервер IPv4", String::new()));
                rows.push(("Служба NetBIOS через TCP/IP", String::from("Нет")));
                let room = (l.table.right() - col - ctx.px(8) as i32).max(0) as u32;
                for (label, value) in rows {
                    if y + line_h > l.table.bottom() {
                        break;
                    }
                    paint::text_clipped(t, s, Role::Body, lx, y, (col - lx - ctx.px(8) as i32).max(0) as u32, label, p.ink2);
                    paint::text_clipped(t, s, Role::Body, col, y, room, &value, p.ink);
                    y += line_h - ctx.px(2) as i32;
                }
                button(inner, s, l.close, Weight::Normal, "Закрыть", true, bg);
            }
            Dialog::Props { selected } => {
                let l = props_layout(ctx, body);
                paint::text(inner, s, Role::Body, body.x, body.y + ctx.px(2) as i32, "Подключение через:", p.ink2);
                let dev_bg = p.sunk.over(bg);
                draw::rounded(s, l.device, ctx.px(4), dev_bg, 255);
                iconset::draw(s, Fi::NetworkWired, l.device.x + ctx.px(10) as i32, l.device.y + (l.device.h as i32 - g as i32) / 2, g, p.acc, 255);
                paint::text_clipped(inner.on(dev_bg), s, Role::Body, l.device.x + ctx.px(34) as i32, paint::baseline(inner, Role::Body, l.device), l.device.w.saturating_sub(ctx.px(40)), info.get("device"), p.ink);
                button(inner, s, l.configure, Weight::Normal, "Настроить…", true, bg);
                paint::text(inner, s, Role::Body, body.x, l.list.y - ctx.px(24) as i32, "Отмеченные компоненты используются этим подключением:", p.ink2);
                let list_bg = p.sunk.over(bg);
                draw::rounded(s, l.list, ctx.px(4), list_bg, 255);
                draw::rounded_stroke(s, l.list, ctx.px(4), p.line3.color, p.line3.alpha);
                let lc = inner.on(list_bg);
                for (n, (row, (title, checked, _))) in l.rows.iter().zip(COMPONENTS.iter()).enumerate() {
                    if n == *selected {
                        paint::row(lc, s, *row, paint::RowState::Selected);
                    }
                    let tx = checkbox(lc, s, row.x + ctx.px(6) as i32, *row, *checked, n != 1, list_bg);
                    iconset::draw(s, Fi::Layers, tx, row.y + (row.h as i32 - ctx.px(14) as i32) / 2, ctx.px(14), p.ok, 255);
                    paint::text_clipped(lc, s, Role::Body, tx + ctx.px(20) as i32, paint::baseline(lc, Role::Body, *row), (row.right() - tx - ctx.px(24) as i32).max(0) as u32, title, if n == 1 { p.ink4 } else { p.ink });
                }
                button(inner, s, l.install, Weight::Normal, "Установить…", false, bg);
                button(inner, s, l.remove, Weight::Normal, "Удалить", false, bg);
                button(inner, s, l.properties, Weight::Normal, "Свойства", *selected == 0, bg);
                group_title(inner, s, l.about.x, l.about.y, l.about.w, "Описание");
                let about = COMPONENTS.get(*selected).map_or("", |c| c.2);
                let mut y = l.about.y + line_h;
                for chunk in wrap(inner, about, l.about.w.saturating_sub(ctx.px(16))) {
                    paint::text(inner, s, Role::Body, l.about.x + ctx.px(8) as i32, y, &chunk, p.ink3);
                    y += line_h - ctx.px(2) as i32;
                }
                button(inner, s, l.ok, Weight::Primary, "ОК", true, bg);
                button(inner, s, l.cancel, Weight::Normal, "Отмена", true, bg);
            }
            Dialog::Ipv4(form) => {
                let l = ipv4_layout(ctx, body);
                let intro = "Параметры IP можно назначать автоматически, если сеть поддерживает эту возможность. В противном случае узнайте параметры IP у сетевого администратора.";
                let mut y = l.intro_y;
                for chunk in wrap(inner, intro, body.w) {
                    paint::text(inner, s, Role::Body, body.x, y, &chunk, p.ink2);
                    y += line_h - ctx.px(3) as i32;
                }
                radio(inner, s, l.auto_ip, form.auto_ip, "Получить IP-адрес автоматически", true);
                draw::rounded_stroke(s, l.ip_group, ctx.px(6), p.line3.color, p.line3.alpha);
                s.fill(Rect::new(l.manual_ip.x - ctx.px(4) as i32, l.manual_ip.y, ctx.face(Role::Body).width("Использовать следующий IP-адрес:") + ctx.px(34), l.manual_ip.h), bg);
                radio(inner, s, l.manual_ip, !form.auto_ip, "Использовать следующий IP-адрес:", true);
                radio(inner, s, l.auto_dns, form.auto_dns, "Получить адрес DNS-сервера автоматически", form.auto_ip);
                draw::rounded_stroke(s, l.dns_group, ctx.px(6), p.line3.color, p.line3.alpha);
                s.fill(Rect::new(l.manual_dns.x - ctx.px(4) as i32, l.manual_dns.y, ctx.face(Role::Body).width("Использовать следующие адреса DNS-серверов:") + ctx.px(34), l.manual_dns.h), bg);
                radio(inner, s, l.manual_dns, !form.auto_dns, "Использовать следующие адреса DNS-серверов:", true);
                let labels = ["IP-адрес:", "Маска подсети:", "Основной шлюз:", "Предпочитаемый DNS-сервер:", "Альтернативный DNS-сервер:"];
                for (n, (rect, label)) in l.fields.iter().zip(labels.iter()).enumerate() {
                    let enabled = form.field_enabled(n);
                    paint::text(inner, s, Role::Body, body.x + ctx.px(26) as i32, paint::baseline(inner, Role::Body, *rect), label, if enabled { p.ink2 } else { p.ink5 });
                    text_field(inner, s, *rect, &form.fields[n], enabled && form.focus == n, enabled, bg);
                }
                if let Some(error) = &form.error {
                    let mut y = l.error_y;
                    for chunk in wrap(inner, error, body.w.saturating_sub(ctx.px(24))) {
                        iconset::draw(s, Fi::Warning, body.x, y, ctx.px(16), p.bad, 255);
                        paint::text(inner, s, Role::Body, body.x + ctx.px(22) as i32, y, &chunk, p.bad_ink);
                        y += line_h - ctx.px(2) as i32;
                    }
                } else if !self.root {
                    paint::text(inner, s, Role::Caption, body.x, l.error_y, "Изменить параметры может только администратор (root).", p.ink4);
                }
                button(inner, s, l.ok, Weight::Primary, "ОК", true, bg);
                button(inner, s, l.cancel, Weight::Normal, "Отмена", true, bg);
            }
            Dialog::Diag(diag) => {
                let l = diag_layout(ctx, body);
                let head = if diag.done { "Диагностика завершена" } else { "Обнаружение проблем…" };
                paint::text(inner, s, Role::Strong, body.x, body.y + ctx.px(4) as i32, head, p.ink);
                let mut y = l.lines_y;
                for (text, ok) in &diag.lines {
                    let (icon, ink) = if *ok { (Fi::Check, p.ok) } else { (Fi::Warning, p.bad) };
                    iconset::draw(s, icon, body.x, y, ctx.px(16), ink, 255);
                    for chunk in wrap(inner, text, body.w.saturating_sub(ctx.px(26))) {
                        paint::text(inner, s, Role::Body, body.x + ctx.px(24) as i32, y, &chunk, p.ink);
                        y += line_h - ctx.px(2) as i32;
                    }
                }
                if !diag.done {
                    paint::progress(inner, s, Rect::new(body.x, l.close.y - ctx.px(20) as i32, body.w, ctx.px(4)), u32::from(diag.stage) + 1, 7, paint::Tone::Accent);
                }
                button(inner, s, l.close, if diag.done { Weight::Primary } else { Weight::Normal }, if diag.done { "Закрыть" } else { "Отмена" }, true, bg);
            }
        }
        if depth + 1 < self.dialogs.len() {
            // Covered by the next dialog: dimmed, as an inactive window.
            draw::blend_rect(s, rect, bg, 60);
        }
    }
}

/// Split a text into lines that fit `room` points (by words).
fn wrap(ctx: Ctx, text: &str, room: u32) -> Vec<String> {
    let face = ctx.face(Role::Body);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        let candidate = if line.is_empty() { String::from(word) } else { format!("{line} {word}") };
        if face.width(&candidate) <= room || line.is_empty() {
            line = candidate;
        } else {
            out.push(core::mem::take(&mut line));
            line = String::from(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: the kernel lays `argc` pointers to strings living as long as
    // the program.
    let args = unsafe { Args::new(argc, argv) };
    let open = args.get(1).map(String::from);
    let info = app::start(&SPEC);
    let size = app::window_size(&info, (900, 600));
    app::run(&SPEC, &info, size, move |_, _| Ncpa::new(open.as_deref()))
}
