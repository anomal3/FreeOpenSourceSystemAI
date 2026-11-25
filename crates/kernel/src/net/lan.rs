//! The local network neighbourhood (Roman 24.11): which computers are
//! around, what `\\NAME` means, and answering for our own name.
//!
//! # Finding computers
//!
//! Windows 10/11 with network discovery on answer WS-Discovery probes
//! (Function Discovery Resource Publication); Samba boxes answer them through
//! `wsdd` and announce `_smb._tcp` over multicast DNS (Avahi); NetBIOS name
//! service still answers on almost every Windows and Samba host. The daemon
//! sends all three every minute (and when `/net` is opened after a minute
//! of silence), collects who answered, and asks each new address for its
//! NetBIOS node status to learn the computer name and whether it serves
//! files (`<20>`).
//!
//! # Names
//!
//! [`resolve`] turns what a person typed after `\\` into an address: a
//! dotted address as is, then the hosts found, then a NetBIOS broadcast
//! query and LLMNR at once, `.local` over mDNS, and the DNS server last.
//!
//! # Being found
//!
//! While discovery is on (`Параметры → Сеть → Параметры общего доступа`,
//! registry `network/discovery`), the daemon answers NetBIOS name and node
//! status queries and LLMNR queries for our host name, so `ping FOSSAI` and
//! `nbtstat -A` work from Windows. A WS-Discovery responder (being listed in
//! Explorer's "Network") needs a file server to point at — it comes with one.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use smb::names;

use super::ipv4::Ipv4;
use crate::sched::TaskId;
use crate::sync::SpinLock;

/// Our NetBIOS/LLMNR name.
pub const NAME: &str = "FOSSAI";
/// Workgroup we claim in node status answers.
pub const WORKGROUP: &str = "WORKGROUP";

/// How often to look around while discovery is on.
const SCAN_EVERY_MS: u64 = 60_000;
/// Forget a host not heard of for this long.
const FORGET_MS: u64 = 10 * 60_000;
/// How long a scan listens for answers.
const SCAN_LISTEN_MS: u64 = 2_500;

/// A computer on the network.
#[derive(Clone, Debug)]
pub struct Host {
    /// Computer name (NetBIOS, or the mDNS host name); empty — not known yet.
    pub name: String,
    pub address: Ipv4,
    /// Known to serve files (NetBIOS `<20>`, `_smb._tcp`, or we connected).
    pub files: bool,
    pub seen_ms: u64,
    /// Node status asked (once per host).
    status_asked: bool,
}

static HOSTS: SpinLock<Vec<Host>> = SpinLock::new(Vec::new());
/// Task id of the daemon — the owner of the long-lived SMB connections too.
static OWNER: AtomicU32 = AtomicU32::new(0);
static SCAN_WANTED: AtomicBool = AtomicBool::new(true);
static LAST_SCAN_MS: AtomicU64 = AtomicU64::new(0);

/// Registry keys of the sharing page.
pub const KEY_DISCOVERY: &str = "network/discovery";
pub const KEY_PASSWORD_PROTECTED: &str = "network/passwordprotected";

fn flag(key: &str, default: bool) -> bool {
    match crate::config::registry::get_in(crate::config::registry::Scope::System, key).as_deref() {
        Some("1" | "on" | "yes" | "true") => true,
        Some("0" | "off" | "no" | "false") => false,
        _ => default,
    }
}

/// Network discovery on (default yes, as on a Windows private network).
#[must_use]
pub fn discovery_enabled() -> bool {
    flag(KEY_DISCOVERY, true)
}

/// Password-protected sharing (kept for the future server; default yes).
#[must_use]
pub fn password_protected() -> bool {
    flag(KEY_PASSWORD_PROTECTED, true)
}

/// Change one of the sharing switches (Settings window — kernel, root).
pub fn set_flag(key: &str, on: bool) {
    let _ = crate::config::registry::set(crate::config::registry::Scope::System, key, Some(if on { "1" } else { "0" }));
    if key == KEY_DISCOVERY {
        SCAN_WANTED.store(on, Ordering::Relaxed);
        if !on {
            HOSTS.lock().clear();
        }
    }
}

/// Owner of kernel-held sockets and connections: the daemon's task id.
#[must_use]
pub fn owner() -> Option<TaskId> {
    match OWNER.load(Ordering::Acquire) {
        0 => None,
        raw => Some(TaskId::new(raw)),
    }
}

/// Start the daemon (once, at boot).
pub fn start() {
    match crate::sched::spawn_daemon("lan", task) {
        Ok(id) => OWNER.store(id.as_u32(), Ordering::Release),
        Err(err) => crate::kprintln!("  lan        : daemon not started ({err})"),
    }
}

/// Computers found (with a name or an address), best known first.
#[must_use]
pub fn hosts() -> Vec<Host> {
    let now = crate::time::uptime_ms();
    // A look around when someone asks after a quiet minute.
    if discovery_enabled() && now.saturating_sub(LAST_SCAN_MS.load(Ordering::Relaxed)) > SCAN_EVERY_MS / 2 {
        SCAN_WANTED.store(true, Ordering::Relaxed);
    }
    let mut list = HOSTS.lock().clone();
    list.sort_by(|a, b| b.files.cmp(&a.files).then_with(|| a.name.cmp(&b.name)));
    list
}

/// Remember a host we reached (by name or address) — `/net` lists it.
pub fn note_host(name: &str, address: Ipv4, server_name: Option<&str>) {
    let now = crate::time::uptime_ms();
    let mut hosts = HOSTS.lock();
    let shown = server_name.filter(|n| !n.is_empty()).map(String::from).or_else(|| (Ipv4::parse(name).is_none()).then(|| name.to_uppercase()));
    if let Some(host) = hosts.iter_mut().find(|host| host.address == address) {
        host.files = true;
        host.seen_ms = now;
        if host.name.is_empty() {
            if let Some(shown) = shown {
                host.name = shown;
            }
        }
        return;
    }
    hosts.push(Host { name: shown.unwrap_or_default(), address, files: true, seen_ms: now, status_asked: true });
}

/// Ask for a scan now (opening `/net`).
pub fn rescan() {
    if discovery_enabled() {
        SCAN_WANTED.store(true, Ordering::Relaxed);
    }
}

fn learn(address: Ipv4, name: Option<&str>, files: bool) {
    let status = crate::net::status();
    if status.is_some_and(|s| s.address == address) || address.is_unspecified() || address.is_broadcast() {
        return;
    }
    let now = crate::time::uptime_ms();
    let mut hosts = HOSTS.lock();
    if let Some(host) = hosts.iter_mut().find(|host| host.address == address) {
        host.seen_ms = now;
        host.files |= files;
        if let Some(name) = name.filter(|n| !n.is_empty()) {
            host.name = String::from(name);
        }
        return;
    }
    if hosts.len() >= 256 {
        return;
    }
    hosts.push(Host { name: name.map(String::from).unwrap_or_default(), address, files, seen_ms: now, status_asked: false });
}

// ---------------------------------------------------------------------------
// Name resolution
// ---------------------------------------------------------------------------

/// The address of `\\name`, waiting at most about `timeout_ms`.
pub fn resolve(name: &str, timeout_ms: u64) -> Option<Ipv4> {
    let name = name.trim().trim_end_matches('.');
    if let Some(address) = Ipv4::parse(name) {
        return Some(address);
    }
    if name.is_empty() || name.len() > 253 {
        return None;
    }
    let short = name.strip_suffix(".local").unwrap_or(name);
    if let Some(host) = HOSTS.lock().iter().find(|host| host.name.eq_ignore_ascii_case(short)) {
        return Some(host.address);
    }
    crate::net::status().filter(|s| !s.address.is_unspecified())?;
    let found = ask_link(name, timeout_ms.min(1_500));
    if let Some(address) = found {
        learn(address, (short.len() <= 15 && !short.contains('.')).then(|| short.to_uppercase()).as_deref(), false);
        return Some(address);
    }
    crate::net::resolve(name, timeout_ms.min(2_000)).ok()
}

/// NetBIOS broadcast (`<20>` and `<00>`), LLMNR and — for `.local` — mDNS,
/// all at once from one socket of the asking task.
fn ask_link(name: &str, timeout_ms: u64) -> Option<Ipv4> {
    let owner = crate::sched::current();
    let index = crate::net::socket_open(owner).ok()?;
    let result = (|| {
        crate::net::socket_bind(owner, index, 0).ok()?;
        let mut id = [0u8; 2];
        crate::random::fill(&mut id);
        let id = u16::from_le_bytes(id) | 1;
        let mdns = name.ends_with(".local");
        let short = name.strip_suffix(".local").unwrap_or(name);
        let broadcast = broadcast_address()?;
        let mut queries: Vec<(Ipv4, u16, Vec<u8>)> = Vec::new();
        if !mdns && short.len() <= 15 && !short.contains('.') {
            queries.push((broadcast, names::NBNS_PORT, names::nb_name_query(id, short, 0x20, true)));
            queries.push((broadcast, names::NBNS_PORT, names::nb_name_query(id, short, 0x00, true)));
        }
        if mdns {
            queries.push((Ipv4::from_bytes(names::MDNS_GROUP), names::MDNS_PORT, names::dns_query(id, name, names::TYPE_A, true)));
        } else {
            queries.push((Ipv4::from_bytes(names::LLMNR_GROUP), names::LLMNR_PORT, names::dns_query(id, short, names::TYPE_A, false)));
        }
        let deadline = crate::time::uptime_ms() + timeout_ms;
        let mut next_send = 0u64;
        let mut sends = 0;
        while crate::time::uptime_ms() < deadline {
            if crate::time::uptime_ms() >= next_send && sends < 2 {
                for (to, port, query) in &queries {
                    send_to(owner, index, *to, *port, query);
                }
                sends += 1;
                next_send = crate::time::uptime_ms() + 600;
            }
            while let Ok(Some(answer)) = crate::net::socket_recv(owner, index) {
                if answer.port == names::NBNS_PORT {
                    if let Some(reply) = names::nb_parse_response(&answer.data) {
                        if reply.id == id {
                            if let Some(address) = reply.addresses.first() {
                                return Some(Ipv4::from_bytes(*address));
                            }
                        }
                    }
                } else if let Some(reply) = names::dns_parse(&answer.data) {
                    if reply.id == id || answer.port == names::MDNS_PORT {
                        for record in &reply.records {
                            if let names::Rdata::A(address) = record.data {
                                if record.name.eq_ignore_ascii_case(name) || record.name.eq_ignore_ascii_case(short) {
                                    return Some(Ipv4::from_bytes(address));
                                }
                            }
                        }
                    }
                }
            }
            crate::sched::sleep_ms(10);
        }
        None
    })();
    let _ = crate::net::socket_close(owner, index);
    result
}

fn broadcast_address() -> Option<Ipv4> {
    let status = crate::net::status()?;
    if status.address.is_unspecified() {
        return None;
    }
    Some(if status.netmask.is_unspecified() { Ipv4::BROADCAST } else { status.address.network_broadcast(status.netmask) })
}

/// One datagram to `to:port` (the socket's peer is re-pointed each time);
/// ARP in progress is retried a few times.
fn send_to(owner: TaskId, index: usize, to: Ipv4, port: u16, data: &[u8]) -> bool {
    if crate::net::socket_connect(owner, index, to, port).is_err() {
        return false;
    }
    for _ in 0..5 {
        match crate::net::socket_send(owner, index, data) {
            Ok(_) => return true,
            Err(crate::net::NetError::Pending) => crate::sched::sleep_ms(20),
            Err(_) => return false,
        }
    }
    false
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

struct Sockets {
    /// Ephemeral port: probes and their answers.
    probe: usize,
    /// UDP 137 and 5355 — questions to us.
    nbns: Option<usize>,
    llmnr: Option<usize>,
}

fn task() {
    let owner = crate::sched::current();
    OWNER.store(owner.as_u32(), Ordering::Release);
    let mut sockets: Option<Sockets> = None;
    let mut next_status_ms = 0u64;
    let mut listen_until = 0u64;
    let mut scan_id = 1u16;
    loop {
        let up = crate::net::status().is_some_and(|s| !s.address.is_unspecified());
        if !up || !discovery_enabled() {
            if let Some(open) = sockets.take() {
                close(owner, &open);
                crate::kprintln!("lan: discovery and name answers stopped");
            }
            crate::sched::sleep_ms(1_000);
            continue;
        }
        let Some(open) = sockets.as_ref() else {
            sockets = open_sockets(owner);
            if sockets.is_none() {
                crate::sched::sleep_ms(2_000);
            }
            continue;
        };
        let now = crate::time::uptime_ms();
        if SCAN_WANTED.swap(false, Ordering::Relaxed) || now.saturating_sub(LAST_SCAN_MS.load(Ordering::Relaxed)) > SCAN_EVERY_MS {
            LAST_SCAN_MS.store(now, Ordering::Relaxed);
            scan_id = scan_id.wrapping_add(2) | 1;
            scan(owner, open.probe, scan_id);
            listen_until = now + SCAN_LISTEN_MS;
            HOSTS.lock().retain(|host| now.saturating_sub(host.seen_ms) < FORGET_MS);
        }
        drain_probe(owner, open.probe);
        if let Some(index) = open.nbns {
            answer_netbios(owner, index);
        }
        if let Some(index) = open.llmnr {
            answer_llmnr(owner, index);
        }
        // Node status of new hosts, one every 100 ms.
        if now >= next_status_ms {
            next_status_ms = now + 100;
            let target = {
                let mut hosts = HOSTS.lock();
                hosts.iter_mut().find(|host| !host.status_asked).map(|host| {
                    host.status_asked = true;
                    host.address
                })
            };
            if let Some(address) = target {
                send_to(owner, open.probe, address, names::NBNS_PORT, &names::nb_node_status_query(scan_id));
            }
        }
        crate::sched::sleep_ms(if now < listen_until { 20 } else { 100 });
    }
}

fn open_sockets(owner: TaskId) -> Option<Sockets> {
    let probe = crate::net::socket_open(owner).ok()?;
    if crate::net::socket_bind(owner, probe, 0).is_err() {
        let _ = crate::net::socket_close(owner, probe);
        return None;
    }
    let bound = |port: u16| {
        let index = crate::net::socket_open(owner).ok()?;
        if crate::net::socket_bind(owner, index, port).is_ok() {
            Some(index)
        } else {
            let _ = crate::net::socket_close(owner, index);
            None
        }
    };
    let nbns = bound(names::NBNS_PORT);
    let llmnr = bound(names::LLMNR_PORT);
    crate::kprintln!(
        "lan: discovery on, answering for {NAME} (NetBIOS {}, LLMNR {})",
        if nbns.is_some() { "yes" } else { "port busy" },
        if llmnr.is_some() { "yes" } else { "port busy" }
    );
    Some(Sockets { probe, nbns, llmnr })
}

fn close(owner: TaskId, sockets: &Sockets) {
    for index in [Some(sockets.probe), sockets.nbns, sockets.llmnr].into_iter().flatten() {
        let _ = crate::net::socket_close(owner, index);
    }
}

/// Send the probes: WS-Discovery, mDNS `_smb._tcp`, NetBIOS workgroup.
fn scan(owner: TaskId, index: usize, id: u16) {
    let mut uuid = [0u8; 16];
    crate::random::fill(&mut uuid);
    send_to(owner, index, Ipv4::from_bytes(names::WSD_GROUP), names::WSD_PORT, &names::wsd_probe(&uuid));
    send_to(owner, index, Ipv4::from_bytes(names::MDNS_GROUP), names::MDNS_PORT, &names::dns_query(id, "_smb._tcp.local", names::TYPE_PTR, true));
    if let Some(broadcast) = broadcast_address() {
        // Every member of a workgroup answers for its group name.
        send_to(owner, index, broadcast, names::NBNS_PORT, &names::nb_name_query(id, WORKGROUP, 0x00, true));
        send_to(owner, index, broadcast, names::NBNS_PORT, &names::nb_name_query(id, "MSHOME", 0x00, true));
    }
}

fn drain_probe(owner: TaskId, index: usize) {
    for _ in 0..32 {
        let Ok(Some(answer)) = crate::net::socket_recv(owner, index) else {
            return;
        };
        match answer.port {
            names::WSD_PORT => {
                if names::wsd_is_computer(&answer.data) {
                    learn(answer.from, None, false);
                }
            }
            names::MDNS_PORT => {
                if let Some(reply) = names::dns_parse(&answer.data) {
                    for (host, address) in names::mdns_smb_hosts(&reply) {
                        learn(Ipv4::from_bytes(address), Some(&host.to_uppercase()), true);
                    }
                }
            }
            names::NBNS_PORT => {
                if let Some(reply) = names::nb_parse_response(&answer.data) {
                    if reply.names.is_empty() {
                        for address in &reply.addresses {
                            learn(Ipv4::from_bytes(*address), None, false);
                        }
                        learn(answer.from, None, false);
                    } else {
                        learn(answer.from, reply.computer_name(), reply.serves_files());
                    }
                }
            }
            _ => {}
        }
    }
}

fn our_address() -> Option<(Ipv4, [u8; 6])> {
    let status = crate::net::status()?;
    (!status.address.is_unspecified()).then_some((status.address, status.mac))
}

fn answer_netbios(owner: TaskId, index: usize) {
    for _ in 0..16 {
        let Ok(Some(query)) = crate::net::socket_recv(owner, index) else {
            return;
        };
        let Some((id, name, suffix, node_status)) = names::nb_parse_query(&query.data) else {
            continue;
        };
        let Some((address, mac)) = our_address() else {
            return;
        };
        let reply = if node_status {
            names::nb_node_status_response(id, &[(NAME, 0x00, false), (WORKGROUP, 0x00, true)], mac)
        } else if name.eq_ignore_ascii_case(NAME) && suffix == 0x00 {
            names::nb_name_response(id, NAME, 0x00, address.to_bytes())
        } else {
            continue;
        };
        send_to(owner, index, query.from, query.port, &reply);
    }
}

fn answer_llmnr(owner: TaskId, index: usize) {
    for _ in 0..16 {
        let Ok(Some(query)) = crate::net::socket_recv(owner, index) else {
            return;
        };
        let Some(message) = names::dns_parse(&query.data) else {
            continue;
        };
        if !names::dns_is_query(&message) {
            continue;
        }
        let Some((name, qtype)) = message.questions.first() else {
            continue;
        };
        if *qtype != names::TYPE_A || !name.eq_ignore_ascii_case(NAME) {
            continue;
        }
        let Some((address, _)) = our_address() else {
            return;
        };
        send_to(owner, index, query.from, query.port, &names::llmnr_response(message.id, name, address.to_bytes()));
    }
}
