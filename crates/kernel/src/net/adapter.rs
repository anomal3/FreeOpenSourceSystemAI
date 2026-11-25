//! Per-adapter settings — «Сетевые подключения» (25.11.2026, Roman: «как в
//! Windows: Панель управления › Сеть и Интернет › Сетевые подключения»).
//!
//! # What an adapter has
//!
//! A name the person gave it («Ethernet» by default), «Включить» /
//! «Отключить», IPv4 by DHCP or by hand (address, mask, gateway), DNS servers
//! from the lease or by hand (preferred and alternate). Everything applies at
//! once, without a reboot, and is kept in the settings store, system scope:
//! `network/adapters/<mac>/{name,enabled,ipv4,address,netmask,gateway,dns,
//! dns1,dns2}` — keyed by the card's hardware address, so a stick booted on
//! another machine does not take this machine's static address with it. The
//! old `/etc/network.cfg` (one machine-wide address) is still read when the
//! store has nothing for the card, and removed on the first save.
//!
//! # How DHCP follows
//!
//! `/bin/dhcp` is a service and the kernel does not restart it. It looks at
//! `NetInfo::flags` ([`netinfo_flags`]) every second: `NETINFO_DHCP` says
//! whether it should hold a lease now, the renew counter (bits 2..7) moves on
//! «Диагностика» and on switching to DHCP — the service starts a new exchange
//! at once. A lease that arrives after the switch to a static address is
//! refused by `SYS_NETCONF` ([`dhcp_may_apply`]).
//!
//! # The window and the bench
//!
//! `/bin/ncpa` and the bench talk through the shell command `netcfg` (see
//! [`command`]): `netcfg` prints the state as `key value` lines (allowed to
//! every user), `netcfg ipv4 dhcp|static ADDR MASK [GW]`, `netcfg dns
//! auto|DNS1 [DNS2]`, `netcfg enable|disable|renew`, `netcfg rename NAME`
//! (root only). A refusal is one line `error <why, in Russian>`.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};

use super::ipv4::Ipv4;
use super::{HAS_ADDRESS, INTERFACE, LOOPBACK_NAME};
use crate::config::registry::{self, RegError, Scope};
use crate::kprintln;

/// The last DHCP lease.
#[derive(Clone, Copy)]
pub struct Lease {
    pub server: Ipv4,
    /// When it came (uptime, ms).
    pub at_ms: u64,
    pub seconds: u32,
    /// DNS servers of the lease — back in force when DNS goes «автоматически».
    pub dns: (Ipv4, Ipv4),
}

/// The adapter's settings in the running stack.
pub struct State {
    pub enabled: bool,
    pub dhcp: bool,
    pub dns_manual: bool,
    pub dns2: Ipv4,
    pub lease: Option<Lease>,
    /// When the current address came (uptime, ms) — «Длительность».
    pub since_ms: u64,
    pub name: Option<String>,
}

impl State {
    pub const fn new() -> Self {
        Self { enabled: true, dhcp: true, dns_manual: false, dns2: Ipv4::UNSPECIFIED, lease: None, since_ms: 0, name: None }
    }
}

/// Moves on «Диагностика» and on the switch to DHCP: `/bin/dhcp` starts a new
/// exchange when it sees a new value (6 bits are enough — it looks every
/// second).
static RENEW: AtomicU8 = AtomicU8::new(0);

/// What is kept in the settings store for one card.
#[derive(Clone)]
pub struct Saved {
    pub name: Option<String>,
    pub enabled: bool,
    pub dhcp: bool,
    pub address: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns_manual: bool,
    pub dns1: Ipv4,
    pub dns2: Ipv4,
}

impl Saved {
    const fn fresh() -> Self {
        Self {
            name: None,
            enabled: true,
            dhcp: true,
            address: Ipv4::UNSPECIFIED,
            netmask: Ipv4::UNSPECIFIED,
            gateway: Ipv4::UNSPECIFIED,
            dns_manual: false,
            dns1: Ipv4::UNSPECIFIED,
            dns2: Ipv4::UNSPECIFIED,
        }
    }
}

fn mac_key(mac: [u8; 6]) -> String {
    mac.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn key(mac: [u8; 6], leaf: &str) -> String {
    format!("network/adapters/{}/{leaf}", mac_key(mac))
}

fn ip_or_zero(text: Option<String>) -> Ipv4 {
    text.as_deref().and_then(Ipv4::parse).unwrap_or(Ipv4::UNSPECIFIED)
}

/// Settings of the card `mac`: the store, else the old `/etc/network.cfg`,
/// else the defaults (DHCP, switched on).
#[must_use]
pub fn load(mac: [u8; 6]) -> Saved {
    let get = |leaf: &str| registry::get_in(Scope::System, &key(mac, leaf));
    let mut saved = Saved::fresh();
    saved.name = get("name").filter(|name| !name.trim().is_empty());
    saved.enabled = get("enabled").as_deref() != Some("0");
    match get("ipv4") {
        Some(mode) => {
            saved.dhcp = mode != "static";
            saved.address = ip_or_zero(get("address"));
            saved.netmask = ip_or_zero(get("netmask"));
            saved.gateway = ip_or_zero(get("gateway"));
            saved.dns_manual = get("dns").as_deref() == Some("manual");
            saved.dns1 = ip_or_zero(get("dns1"));
            saved.dns2 = ip_or_zero(get("dns2"));
            // A static address without an address is a broken record: DHCP.
            if !saved.dhcp && saved.address.is_unspecified() {
                saved.dhcp = true;
            }
        }
        None => {
            if let Some(old) = super::persist::load_file() {
                if old.mode == super::persist::Mode::Static {
                    saved.dhcp = false;
                    saved.address = old.address;
                    saved.netmask = old.netmask;
                    saved.gateway = old.gateway;
                    saved.dns_manual = !old.dns.is_unspecified();
                    saved.dns1 = old.dns;
                }
            }
        }
    }
    saved
}

/// Write what changed (the store skips equal values).
pub fn save(mac: [u8; 6], saved: &Saved) -> Result<(), RegError> {
    let ip = |value: Ipv4| if value.is_unspecified() { None } else { Some(value.to_string()) };
    let entries: [(&str, Option<String>); 9] = [
        ("name", saved.name.clone()),
        ("enabled", Some(String::from(if saved.enabled { "1" } else { "0" }))),
        ("ipv4", Some(String::from(if saved.dhcp { "dhcp" } else { "static" }))),
        ("address", ip(saved.address)),
        ("netmask", ip(saved.netmask)),
        ("gateway", ip(saved.gateway)),
        ("dns", Some(String::from(if saved.dns_manual { "manual" } else { "auto" }))),
        ("dns1", ip(saved.dns1)),
        ("dns2", ip(saved.dns2)),
    ];
    for (leaf, value) in entries {
        registry::set(Scope::System, &key(mac, leaf), value.as_deref())?;
    }
    // The machine-wide file of old: the store speaks for the card now.
    let _ = super::persist::forget_file();
    Ok(())
}

/// The card's hardware address, if there is a real card.
fn current_mac() -> Option<[u8; 6]> {
    let guard = INTERFACE.lock();
    let iface = guard.as_ref()?;
    (iface.device.name() != LOOPBACK_NAME).then_some(iface.mac)
}

/// Flags for `NetInfo::flags` — what `/bin/dhcp` should do.
#[must_use]
pub fn netinfo_flags() -> u8 {
    let guard = INTERFACE.lock();
    let Some(iface) = guard.as_ref() else {
        return 0;
    };
    let mut flags = (RENEW.load(Ordering::Relaxed) & 0x3F) << 2;
    if iface.cfg.dhcp && iface.cfg.enabled {
        flags |= user_abi::NETINFO_DHCP;
    }
    if !iface.cfg.enabled {
        flags |= user_abi::NETINFO_DISABLED;
    }
    flags
}

/// May a lease be applied now (`SYS_NETCONF` from `/bin/dhcp`).
#[must_use]
pub fn dhcp_may_apply() -> bool {
    INTERFACE.lock().as_ref().is_some_and(|iface| iface.cfg.dhcp && iface.cfg.enabled)
}

/// The lease's details that `NetConfig` does not carry (`NetLease`).
pub fn note_lease(server: Ipv4, seconds: u32, dns1: Ipv4, dns2: Ipv4) {
    let mut guard = INTERFACE.lock();
    let Some(iface) = guard.as_mut() else {
        return;
    };
    iface.cfg.lease = Some(Lease { server, at_ms: crate::time::uptime_ms(), seconds, dns: (dns1, dns2) });
    if !iface.cfg.dns_manual {
        iface.cfg.dns2 = dns2;
    }
}

/// Apply the kept settings once, after the card came up and before the
/// services start (`/bin/dhcp` then sees whether it is wanted).
pub fn adopt() {
    let Some(mac) = current_mac() else {
        return;
    };
    let saved = load(mac);
    {
        let mut guard = INTERFACE.lock();
        let Some(iface) = guard.as_mut() else {
            return;
        };
        iface.cfg.enabled = saved.enabled;
        iface.cfg.dhcp = saved.dhcp;
        iface.cfg.dns_manual = saved.dns_manual;
        iface.cfg.name = saved.name.clone();
        if saved.dns_manual {
            iface.dns = saved.dns1;
            iface.cfg.dns2 = saved.dns2;
        }
    }
    if !saved.enabled {
        kprintln!("  network     : the adapter is switched off (settings store)");
    }
    if saved.dhcp {
        kprintln!("  network     : DHCP{}", if saved.dns_manual { ", DNS by hand" } else { "" });
        return;
    }
    match super::configure_all(saved.address, saved.netmask, saved.gateway, saved.dns1) {
        Ok(()) => kprintln!(
            "  network     : {}/{} static (settings store)",
            saved.address,
            saved.netmask.prefix().unwrap_or(0)
        ),
        Err(err) => kprintln!("  network     : the static address is rejected: {err}"),
    }
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

/// A host address one can give a card or a server.
fn check_host(what: &str, value: Ipv4) -> Result<(), String> {
    let [a, ..] = value.to_bytes();
    if value.is_unspecified() || value.is_broadcast() || a == 0 {
        return Err(format!("{what} {value} не годится"));
    }
    if value.is_loopback() || a == 127 {
        return Err(format!("{what} {value} — это петля (127.x.x.x), в сеть так не выйти"));
    }
    if a >= 224 {
        return Err(format!("{what} {value} — групповой или зарезервированный адрес"));
    }
    Ok(())
}

/// Parse a mask written as `255.255.255.0` or `/24` / `24`.
fn parse_mask(text: &str) -> Option<Ipv4> {
    let bits = text.trim_start_matches('/');
    if let Ok(bits) = bits.parse::<u32>() {
        return Ipv4::from_prefix(bits);
    }
    let mask = Ipv4::parse(text)?;
    mask.prefix().map(|_| mask)
}

/// Check a static address (also used by `/bin/ncpa` messages).
pub fn check_static(address: Ipv4, netmask: Ipv4, gateway: Ipv4) -> Result<(), String> {
    check_host("IP-адрес", address)?;
    let Some(bits) = netmask.prefix().filter(|bits| (1..=32).contains(bits)) else {
        return Err(format!("Маска подсети {netmask} не годится: единицы слева подряд, например 255.255.255.0"));
    };
    if bits <= 30 {
        let broadcast = address.network_broadcast(netmask);
        let network = Ipv4(address.0 & netmask.0);
        if address == network {
            return Err(format!("{address} — адрес самой сети {network}/{bits}, а не компьютера"));
        }
        if address == broadcast {
            return Err(format!("{address} — широковещательный адрес сети {network}/{bits}"));
        }
    }
    if !gateway.is_unspecified() {
        check_host("Основной шлюз", gateway)?;
        if gateway == address {
            return Err(String::from("Основной шлюз совпадает с IP-адресом компьютера"));
        }
        if !gateway.same_network(address, netmask) {
            return Err(format!(
                "Основной шлюз {gateway} не в подсети {}/{bits}: компьютер до него не достанет",
                Ipv4(address.0 & netmask.0)
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

type Outcome = Result<String, String>;

/// Run `change` on the live state, then save the card's settings.
fn change(what: impl FnOnce(&mut Saved) -> Result<(), String>) -> Outcome {
    let mac = current_mac().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
    let mut saved = load(mac);
    what(&mut saved)?;
    save(mac, &saved).map_err(|err| format!("Применено, но не записано в реестр: {err}"))?;
    Ok(String::from("ok"))
}

fn set_static(address: Ipv4, netmask: Ipv4, gateway: Ipv4) -> Outcome {
    check_static(address, netmask, gateway)?;
    change(|saved| {
        {
            let mut guard = INTERFACE.lock();
            let iface = guard.as_mut().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
            iface.cfg.dhcp = false;
            iface.cfg.lease = None;
            // A static address needs DNS by hand: nobody else will give it.
            if !iface.cfg.dns_manual {
                iface.cfg.dns_manual = true;
                saved.dns_manual = true;
                saved.dns1 = iface.dns;
                saved.dns2 = iface.cfg.dns2;
            }
        }
        super::configure(address, netmask, gateway).map_err(|err| format!("Адрес не принят: {err}"))?;
        saved.dhcp = false;
        saved.address = address;
        saved.netmask = netmask;
        saved.gateway = gateway;
        kprintln!("  network     : {address}/{} gw {gateway} static (Сетевые подключения)", netmask.prefix().unwrap_or(0));
        Ok(())
    })
}

fn set_dhcp() -> Outcome {
    change(|saved| {
        let was_static = {
            let mut guard = INTERFACE.lock();
            let iface = guard.as_mut().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
            let was = !iface.cfg.dhcp;
            iface.cfg.dhcp = true;
            was
        };
        if was_static {
            // The hand-set address goes; the lease brings a new one.
            let _ = super::configure(Ipv4::UNSPECIFIED, Ipv4::UNSPECIFIED, Ipv4::UNSPECIFIED);
        }
        RENEW.fetch_add(1, Ordering::Relaxed);
        saved.dhcp = true;
        kprintln!("  network     : address by DHCP (Сетевые подключения)");
        Ok(())
    })
}

fn set_dns(servers: Option<(Ipv4, Ipv4)>) -> Outcome {
    if let Some((dns1, dns2)) = servers {
        check_host("Предпочитаемый DNS-сервер", dns1)?;
        if !dns2.is_unspecified() {
            check_host("Альтернативный DNS-сервер", dns2)?;
        }
    }
    change(|saved| {
        let mut guard = INTERFACE.lock();
        let iface = guard.as_mut().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
        match servers {
            Some((dns1, dns2)) => {
                iface.cfg.dns_manual = true;
                iface.dns = dns1;
                iface.cfg.dns2 = dns2;
                saved.dns_manual = true;
                saved.dns1 = dns1;
                saved.dns2 = dns2;
                kprintln!("  network     : DNS {dns1} {dns2} by hand");
            }
            None => {
                if !iface.cfg.dhcp {
                    return Err(String::from(
                        "При IP-адресе, заданном вручную, DNS-серверы тоже задаются вручную",
                    ));
                }
                iface.cfg.dns_manual = false;
                let (dns1, dns2) = iface.cfg.lease.map_or((Ipv4::UNSPECIFIED, Ipv4::UNSPECIFIED), |lease| lease.dns);
                iface.dns = dns1;
                iface.cfg.dns2 = dns2;
                saved.dns_manual = false;
                kprintln!("  network     : DNS from DHCP ({dns1})");
            }
        }
        Ok(())
    })
}

fn set_enabled(on: bool) -> Outcome {
    change(|saved| {
        let dhcp = {
            let mut guard = INTERFACE.lock();
            let iface = guard.as_mut().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
            iface.cfg.enabled = on;
            HAS_ADDRESS.store(on && !iface.address.is_unspecified(), Ordering::Release);
            if on {
                iface.cfg.since_ms = crate::time::uptime_ms();
            }
            iface.cfg.dhcp
        };
        if on && dhcp {
            // Possibly another network now: ask for an address again.
            let _ = super::configure(Ipv4::UNSPECIFIED, Ipv4::UNSPECIFIED, Ipv4::UNSPECIFIED);
            RENEW.fetch_add(1, Ordering::Relaxed);
        }
        saved.enabled = on;
        kprintln!("  network     : adapter {}", if on { "switched on" } else { "switched off" });
        Ok(())
    })
}

fn renew() -> Outcome {
    {
        let guard = INTERFACE.lock();
        let iface = guard.as_ref().ok_or_else(|| String::from("Сетевого адаптера нет"))?;
        if !iface.cfg.enabled {
            return Err(String::from("Адаптер отключён — сначала включите его"));
        }
        if !iface.cfg.dhcp {
            return Err(String::from("IP-адрес задан вручную: DHCP не спрашивается"));
        }
    }
    RENEW.fetch_add(1, Ordering::Relaxed);
    kprintln!("  network     : DHCP renew asked (Диагностика)");
    Ok(String::from("ok"))
}

fn rename(name: &str) -> Outcome {
    let name = name.trim();
    if name.chars().count() > 48 || name.chars().any(char::is_control) {
        return Err(String::from("Имя — до 48 знаков, без управляющих"));
    }
    let name = (!name.is_empty() && name != "-").then(|| String::from(name));
    change(|saved| {
        if let Some(iface) = INTERFACE.lock().as_mut() {
            iface.cfg.name = name.clone();
        }
        saved.name = name.clone();
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// The shell command
// ---------------------------------------------------------------------------

/// Default name and what the card is.
fn describe(driver: &str) -> (&'static str, &'static str, &'static str) {
    match driver {
        "e1000" => ("Ethernet", "wired", "Intel(R) PRO/1000 Gigabit Ethernet"),
        "rtl8125" => ("Ethernet", "wired", "Realtek PCIe 2.5GbE Family Controller"),
        "atl1c" => ("Ethernet", "wired", "Qualcomm Atheros PCI-E Gigabit Ethernet"),
        "virtio-net" => ("Ethernet", "wired", "Red Hat VirtIO Ethernet Adapter"),
        "wifi" => ("Беспроводная сеть", "wireless", "MediaTek Wi-Fi"),
        name if name.starts_with("usb-") => ("Ethernet 2", "usb", "USB Ethernet / RNDIS (USB-модем)"),
        _ => ("Ethernet", "wired", "Сетевой адаптер"),
    }
}

/// The state as `key value` lines.
fn show(out: &mut Vec<String>) {
    let guard = INTERFACE.lock();
    let Some(iface) = guard.as_ref().filter(|iface| iface.device.name() != LOOPBACK_NAME) else {
        out.push(String::from("none"));
        return;
    };
    let driver = iface.device.name();
    let (default_name, kind, device) = describe(driver);
    let ip = |value: Ipv4| if value.is_unspecified() { String::from("-") } else { value.to_string() };
    let m = iface.mac;
    let now = crate::time::uptime_ms();
    out.push(format!("name {}", iface.cfg.name.as_deref().unwrap_or(default_name)));
    out.push(format!("driver {driver}"));
    out.push(format!("device {device}"));
    out.push(format!("kind {kind}"));
    out.push(format!("mac {:02X}-{:02X}-{:02X}-{:02X}-{:02X}-{:02X}", m[0], m[1], m[2], m[3], m[4], m[5]));
    out.push(format!("enabled {}", u8::from(iface.cfg.enabled)));
    let link = iface.device.link();
    out.push(format!(
        "media {}",
        match link {
            Some((true, _)) => "up",
            Some((false, _)) => "down",
            None => "unknown",
        }
    ));
    out.push(format!("speed {}", link.filter(|(up, _)| *up).map_or_else(|| String::from("-"), |(_, mbps)| mbps.to_string())));
    out.push(format!("ipv4 {}", if iface.cfg.dhcp { "dhcp" } else { "static" }));
    out.push(format!("dns {}", if iface.cfg.dns_manual { "manual" } else { "auto" }));
    out.push(format!("address {}", ip(iface.address)));
    out.push(format!("netmask {}", ip(iface.netmask)));
    out.push(format!("gateway {}", ip(iface.gateway)));
    out.push(format!("dns1 {}", ip(iface.dns)));
    out.push(format!("dns2 {}", ip(iface.cfg.dns2)));
    match iface.cfg.lease.filter(|_| iface.cfg.dhcp) {
        Some(lease) => {
            out.push(format!("dhcp_server {}", ip(lease.server)));
            out.push(format!("lease_age {}", now.saturating_sub(lease.at_ms) / 1000));
            out.push(format!("lease_seconds {}", lease.seconds));
        }
        None => out.push(String::from("dhcp_server -")),
    }
    let connected = iface.cfg.enabled && !iface.address.is_unspecified();
    out.push(format!("uptime {}", if connected { now.saturating_sub(iface.cfg.since_ms) / 1000 } else { 0 }));
    let stats = iface.device.stats();
    out.push(format!("rx_bytes {}", stats.rx_bytes));
    out.push(format!("tx_bytes {}", stats.tx_bytes));
    out.push(format!("rx_frames {}", stats.rx_frames));
    out.push(format!("tx_frames {}", stats.tx_frames));
    let mac = iface.mac;
    drop(guard);
    // What was typed by hand last time — the window offers it again.
    let saved = load(mac);
    out.push(format!("saved_address {}", ip(saved.address)));
    out.push(format!("saved_netmask {}", ip(saved.netmask)));
    out.push(format!("saved_gateway {}", ip(saved.gateway)));
    out.push(format!("saved_dns1 {}", ip(saved.dns1)));
    out.push(format!("saved_dns2 {}", ip(saved.dns2)));
}

/// An address word: `-` and an empty word are «нет».
fn ip_word(word: Option<&str>, what: &str) -> Result<Ipv4, String> {
    match word {
        None | Some("-") | Some("") => Ok(Ipv4::UNSPECIFIED),
        Some(text) => Ipv4::parse(text).ok_or_else(|| format!("{what}: «{text}» — не адрес IPv4 (четыре числа 0–255 через точку)")),
    }
}

fn run(argument: &str) -> Outcome {
    let mut words = argument.split_whitespace();
    let verb = words.next().unwrap_or("");
    match verb {
        "ipv4" => match words.next() {
            Some("dhcp" | "auto") => set_dhcp(),
            Some("static") => {
                let address = ip_word(words.next(), "IP-адрес")?;
                if address.is_unspecified() {
                    return Err(String::from("Укажите IP-адрес"));
                }
                let mask_word = words.next().ok_or_else(|| String::from("Укажите маску подсети"))?;
                let netmask = parse_mask(mask_word).ok_or_else(|| {
                    format!("Маска подсети «{mask_word}» не годится: единицы слева подряд, например 255.255.255.0")
                })?;
                let gateway = ip_word(words.next(), "Основной шлюз")?;
                set_static(address, netmask, gateway)
            }
            _ => Err(String::from("netcfg ipv4 dhcp | static ADDR MASK [GW]")),
        },
        "dns" => match words.next() {
            Some("auto" | "dhcp") => set_dns(None),
            first => {
                let dns1 = ip_word(first, "Предпочитаемый DNS-сервер")?;
                let dns2 = ip_word(words.next(), "Альтернативный DNS-сервер")?;
                if dns1.is_unspecified() {
                    return Err(String::from("Укажите предпочитаемый DNS-сервер"));
                }
                set_dns(Some((dns1, dns2)))
            }
        },
        "enable" | "on" => set_enabled(true),
        "disable" | "off" => set_enabled(false),
        "renew" => renew(),
        "rename" => rename(argument.trim_start().strip_prefix("rename").unwrap_or("")),
        _ => Err(String::from("netcfg [show] | ipv4 … | dns … | enable | disable | renew | rename NAME")),
    }
}

/// `netcfg …`: see the head of the file.
#[must_use]
pub fn command(argument: &str) -> Vec<String> {
    let argument = argument.trim();
    let mut out = Vec::new();
    if argument.is_empty() || argument == "show" {
        show(&mut out);
        return out;
    }
    match run(argument) {
        Ok(message) => out.push(message),
        Err(why) => out.push(format!("error {why}")),
    }
    out
}

/// Is the shell line a read-only `netcfg` (allowed to every user).
#[must_use]
pub fn is_read_only(line: &str) -> bool {
    matches!(line.trim(), "netcfg" | "netcfg show")
}
