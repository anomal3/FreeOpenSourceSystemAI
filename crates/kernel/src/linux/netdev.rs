//! Интерфейсы сети для программ Linux вне netlink (16.11.2026): ioctl
//! `SIOCGIF*` на любом сокете, `/proc/net/*`, `/proc/sys/net/*` и
//! `/sys/class/net/<имя>/*`.
//!
//! Читает это `nsiproxy.sys` Wine 11 (`ndis.c`, `ip.c`): MAC — через
//! `SIOCGIFHWADDR` на сокете `AF_INET`, MTU и флаги — тоже ioctl, счётчики —
//! `/proc/net/dev`, наличие кабеля — `/sys/class/net/<имя>/carrier`, таблица
//! путей — `/proc/net/route`, соседи — `/proc/net/arp`, статистика —
//! `/proc/net/snmp`. Без ответа Wine оставлял таблицу интерфейсов пустой или с
//! ошибкой устройства. glibc зовёт `SIOCGIFINDEX`/`SIOCGIFNAME` в
//! `if_nametoindex`/`if_indextoname`.
//!
//! Источник у всего один — [`crate::net::interfaces`], тот же, что у netlink:
//! номера и имена везде совпадают (`lo` — 1, `eth0` — 2).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::e;
use super::ipc::{get, put, read_user, write_user};
use crate::net::ipv4::Ipv4;
use crate::net::IfInfo;
use crate::vfs::NodeKind;

const S_IFSOCK: u32 = 0o140_000;
const S_IFMT: u32 = 0o170_000;
const ENODEV: i64 = 19;
const EADDRNOTAVAIL: i64 = 99;

const SIOCGIFNAME: usize = 0x8910;
const SIOCGIFCONF: usize = 0x8912;
const SIOCGIFFLAGS: usize = 0x8913;
const SIOCGIFADDR: usize = 0x8915;
const SIOCGIFBRDADDR: usize = 0x8919;
const SIOCGIFNETMASK: usize = 0x891b;
const SIOCGIFMTU: usize = 0x8921;
const SIOCGIFHWADDR: usize = 0x8927;
const SIOCGIFINDEX: usize = 0x8933;
const SIOCGIWNAME: usize = 0x8B01;

/// `struct ifreq`: имя (16 байт) и объединение на 24.
const IFREQ: usize = 40;
const IFNAMSIZ: usize = 16;
const AF_INET: u16 = 2;

/// Сокет ли за дескриптором (любой: `AF_INET`, `AF_UNIX`, netlink) — ioctl
/// интерфейсов Linux принимает на всяком.
fn is_socket(fd: usize) -> bool {
    crate::user::with_current(|program| program.files.object(fd))
        .flatten()
        .is_some_and(|(object, _)| object.mode() & S_IFMT == S_IFSOCK)
}

/// ioctl интерфейсов сети; `None` — не наш запрос или не сокет.
pub fn ioctl(fd: usize, request: usize, arg: usize) -> Option<i64> {
    if !matches!(
        request,
        SIOCGIFNAME
            | SIOCGIFCONF
            | SIOCGIFFLAGS
            | SIOCGIFADDR
            | SIOCGIFBRDADDR
            | SIOCGIFNETMASK
            | SIOCGIFMTU
            | SIOCGIFHWADDR
            | SIOCGIFINDEX
            | SIOCGIWNAME
    ) || !is_socket(fd)
    {
        return None;
    }
    Some(match answer(request, arg) {
        Ok(()) => 0,
        Err(err) => err,
    })
}

/// Имя из `ifr_name` (до нуля).
fn request_name(arg: usize) -> Result<String, i64> {
    let raw = read_user(arg, IFNAMSIZ)?;
    let end = raw.iter().position(|byte| *byte == 0).unwrap_or(IFNAMSIZ);
    Ok(String::from_utf8_lossy(&raw[..end]).into_owned())
}

/// `struct sockaddr_in` (16 байт) с адресом и нулевым портом.
fn sockaddr_in(address: Ipv4) -> [u8; 16] {
    let mut raw = [0u8; 16];
    raw[..2].copy_from_slice(&AF_INET.to_ne_bytes());
    raw[4..8].copy_from_slice(&address.to_bytes());
    raw
}

fn answer(request: usize, arg: usize) -> Result<(), i64> {
    let interfaces = crate::net::interfaces();
    match request {
        SIOCGIFCONF => return ifconf(&interfaces, arg),
        SIOCGIFNAME => {
            // По номеру в `ifr_ifindex` — имя в `ifr_name`.
            let index = get::<i32>(arg + IFNAMSIZ)?;
            let info = interfaces.iter().find(|info| info.index as i32 == index).ok_or(-ENODEV)?;
            let mut name = [0u8; IFNAMSIZ];
            for (slot, byte) in name.iter_mut().zip(info.name.bytes().take(IFNAMSIZ - 1)) {
                *slot = byte;
            }
            return write_user(arg, &name);
        }
        _ => {}
    }
    let name = request_name(arg)?;
    let info = interfaces.iter().find(|info| info.name == name).ok_or(-ENODEV)?;
    let field = arg + IFNAMSIZ;
    match request {
        // Беспроводных у нас нет: `ndis.c` на отказ оставляет Ethernet.
        SIOCGIWNAME => Err(-ENODEV),
        // `ifr_flags` — short: `IFF_LOWER_UP` (бит 16) в него не входит и у
        // Linux.
        SIOCGIFFLAGS => put(field, info.flags as u16 as i16),
        SIOCGIFMTU => put(field, info.mtu as i32),
        SIOCGIFINDEX => put(field, info.index as i32),
        SIOCGIFHWADDR => {
            // `ifr_hwaddr`: `sa_family` = тип ARPHRD, затем адрес.
            let mut raw = [0u8; 16];
            raw[..2].copy_from_slice(&info.hwtype.to_ne_bytes());
            raw[2..8].copy_from_slice(&info.mac);
            write_user(field, &raw)
        }
        SIOCGIFADDR | SIOCGIFNETMASK | SIOCGIFBRDADDR => {
            let (address, prefix) = info.address.ok_or(-EADDRNOTAVAIL)?;
            let value = match request {
                SIOCGIFADDR => address,
                SIOCGIFNETMASK => Ipv4::from_prefix(u32::from(prefix)).unwrap_or(Ipv4::UNSPECIFIED),
                // У петли широковещательного нет — Linux отвечает нулём.
                _ if info.loopback => Ipv4::UNSPECIFIED,
                _ => info.broadcast(),
            };
            write_user(field, &sockaddr_in(value))
        }
        _ => Err(-e::EINVAL),
    }
}

/// `SIOCGIFCONF`: `struct ifconf { int ifc_len; char *ifc_buf; }` — по
/// `ifreq` с адресом на каждый интерфейс с адресом IPv4. Без буфера — сколько
/// байт понадобится.
fn ifconf(interfaces: &[IfInfo], arg: usize) -> Result<(), i64> {
    let room = get::<i32>(arg)?.max(0) as usize;
    let buf = get::<usize>(arg + 8)?;
    let with_address: Vec<&IfInfo> = interfaces.iter().filter(|info| info.address.is_some()).collect();
    if buf == 0 {
        return put(arg, (with_address.len() * IFREQ) as i32);
    }
    let mut written = 0;
    for info in with_address {
        if written + IFREQ > room {
            break;
        }
        let mut entry = [0u8; IFREQ];
        for (slot, byte) in entry[..IFNAMSIZ - 1].iter_mut().zip(info.name.bytes()) {
            *slot = byte;
        }
        let (address, _) = info.address.expect("filtered above");
        entry[IFNAMSIZ..IFNAMSIZ + 16].copy_from_slice(&sockaddr_in(address));
        write_user(buf + written, &entry)?;
        written += IFREQ;
    }
    put(arg, written as i32)
}

// ---- файлы ----------------------------------------------------------------------

/// Адрес IPv4 так, как его пишет `/proc/net/route`: байты в сетевом порядке,
/// прочтённые как число машины (`0101A8C0` — это 192.168.1.1).
fn route_hex(address: Ipv4) -> String {
    format!("{:08X}", u32::from_le_bytes(address.to_bytes()))
}

fn mac_text(mac: &[u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
}

/// Строка таблицы Linux, добитая пробелами до 127 знаков (`seq_setwidth`).
fn padded(line: &str) -> String {
    format!("{line:<127}\n")
}

/// Содержимое файла сети Linux; `None` — путь не наш. Сначала — дешёвая
/// проверка префикса: `text` особых файлов зовут на каждый `stat`, а снимок
/// интерфейсов берёт замок сети.
pub fn special_file(path: &str) -> Option<String> {
    if !(path.starts_with("/proc/net/") || path.starts_with("/proc/sys/net/") || path.starts_with("/sys/class/net/")) {
        return None;
    }
    if let Some(rest) = path.strip_prefix("/proc/sys/net/") {
        return sysctl(rest);
    }
    let interfaces = crate::net::interfaces();
    if let Some(rest) = path.strip_prefix("/sys/class/net/") {
        let (name, file) = rest.split_once('/')?;
        let info = interfaces.iter().find(|info| info.name == name)?;
        return sys_class_file(info, file);
    }
    match path {
        "/proc/net/dev" => {
            let mut out = String::from(
                "Inter-|   Receive                                                |  Transmit\n \
                 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n",
            );
            for info in &interfaces {
                let stats = &info.stats;
                out.push_str(&format!(
                    "{:>6}: {:>7} {:>7} {:>4} {:>4} {:>4} {:>5} {:>10} {:>9} {:>8} {:>7} {:>4} {:>4} {:>4} {:>5} {:>7} {:>10}\n",
                    info.name,
                    stats.rx_bytes,
                    stats.rx_frames,
                    0,
                    stats.rx_dropped,
                    0,
                    0,
                    0,
                    0,
                    stats.tx_bytes,
                    stats.tx_frames,
                    0,
                    stats.tx_dropped,
                    0,
                    0,
                    0,
                    0
                ));
            }
            Some(out)
        }
        "/proc/net/route" => {
            let mut out = padded("Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT");
            for info in interfaces.iter().filter(|info| !info.loopback) {
                let Some((address, prefix)) = info.address else { continue };
                let mask = Ipv4::from_prefix(u32::from(prefix)).unwrap_or(Ipv4::UNSPECIFIED);
                // Флаги: 1 — RTF_UP, 2 — RTF_GATEWAY.
                if !info.gateway.is_unspecified() {
                    out.push_str(&padded(&format!(
                        "{}\t{}\t{}\t0003\t0\t0\t0\t{}\t0\t0\t0",
                        info.name,
                        route_hex(Ipv4::UNSPECIFIED),
                        route_hex(info.gateway),
                        route_hex(Ipv4::UNSPECIFIED)
                    )));
                }
                out.push_str(&padded(&format!(
                    "{}\t{}\t{}\t0001\t0\t0\t0\t{}\t0\t0\t0",
                    info.name,
                    route_hex(Ipv4(address.0 & mask.0)),
                    route_hex(Ipv4::UNSPECIFIED),
                    route_hex(mask)
                )));
            }
            Some(out)
        }
        "/proc/net/arp" => {
            let mut out = String::from("IP address       HW type     Flags       HW address            Mask     Device\n");
            // Таблица ARP у стека одна — на карте; у петли соседей нет.
            if let Some(card) = interfaces.iter().find(|info| !info.loopback) {
                for (address, mac, _) in crate::net::arp_table() {
                    // Флаги: 0x2 — ATF_COM (запись полная).
                    out.push_str(&format!(
                        "{:<16} 0x{:<10x}0x{:<10x}{:<17}     {:<8} {}\n",
                        format!("{address}"),
                        1,
                        2,
                        mac_text(&mac),
                        "*",
                        card.name
                    ));
                }
            }
            Some(out)
        }
        "/proc/net/snmp" => Some(snmp()),
        // IPv6 у стека нет: пустая таблица, а не отсутствующий файл.
        "/proc/net/if_inet6" => Some(String::new()),
        _ => None,
    }
}

/// `/sys/class/net/<имя>/<файл>`.
fn sys_class_file(info: &IfInfo, file: &str) -> Option<String> {
    const IFF_RUNNING: u32 = 0x40;
    const IFF_LOWER_UP: u32 = 0x1_0000;
    Some(match file {
        "carrier" => String::from(if info.running() { "1\n" } else { "0\n" }),
        "operstate" => String::from(if info.loopback {
            "unknown\n"
        } else if info.running() {
            "up\n"
        } else {
            "down\n"
        }),
        "address" => format!("{}\n", mac_text(&info.mac)),
        "broadcast" => format!("{}\n", mac_text(&if info.loopback { [0; 6] } else { [0xFF; 6] })),
        "mtu" => format!("{}\n", info.mtu),
        "type" => format!("{}\n", info.hwtype),
        "ifindex" | "iflink" => format!("{}\n", info.index),
        // `dev->flags` Linux: без RUNNING и LOWER_UP — они там от состояния.
        "flags" => format!("{:#x}\n", info.flags & !(IFF_RUNNING | IFF_LOWER_UP)),
        "tx_queue_len" => String::from("1000\n"),
        _ => return None,
    })
}

/// Файлы в `/sys/class/net/<имя>`.
const SYS_CLASS_FILES: [&str; 10] =
    ["carrier", "operstate", "address", "broadcast", "mtu", "type", "ifindex", "iflink", "flags", "tx_queue_len"];

/// `/proc/sys/net/*` — значения Linux по умолчанию (`ip.c` Wine читает их и
/// без файла берёт те же).
fn sysctl(rest: &str) -> Option<String> {
    let value = match rest {
        "ipv4/ip_default_ttl" | "ipv6/conf/default/hop_limit" | "ipv6/conf/all/hop_limit" => "64",
        "ipv4/ip_forward" | "ipv4/conf/default/forwarding" | "ipv4/conf/all/forwarding" => "0",
        "ipv6/conf/default/forwarding" | "ipv6/conf/all/forwarding" => "0",
        "ipv4/ip_local_port_range" => "32768\t60999",
        _ => {
            // По интерфейсу: `ipv4/neigh/<имя>/base_reachable_time_ms`,
            // `ipv6/conf/<имя>/dad_transmits`.
            let (name, file) = if let Some(tail) = rest.strip_prefix("ipv4/neigh/") {
                tail.split_once('/')?
            } else if let Some(tail) = rest.strip_prefix("ipv6/conf/") {
                tail.split_once('/')?
            } else {
                return None;
            };
            if name != "default" && !crate::net::interfaces().iter().any(|info| info.name == name) {
                return None;
            }
            match file {
                "base_reachable_time_ms" => "30000",
                "retrans_time_ms" => "1000",
                "dad_transmits" => "1",
                "hop_limit" => "64",
                "forwarding" => "0",
                _ => return None,
            }
        }
    };
    Some(format!("{value}\n"))
}

/// `/proc/net/snmp`: пары строк «имена» / «значения» по протоколам, как у
/// Linux 6.1 (Wine разбирает значения по местам). Чего стек не считает — нули.
fn snmp() -> String {
    let counters = crate::net::status().map(|status| (status.counters, status.streams.1)).unwrap_or_default();
    let (c, established) = counters;
    let mut out = String::new();
    let mut table = |name: &str, fields: &str, values: &[i64]| {
        out.push_str(&format!("{name}: {fields}\n{name}:"));
        for value in values {
            out.push_str(&format!(" {value}"));
        }
        out.push('\n');
    };
    let n = |value: u64| value.min(i64::MAX as u64) as i64;
    let outgoing = n(c.udp_out + c.tcp_out + c.echo_requests_out + c.echo_replies_out);
    table(
        "Ip",
        "Forwarding DefaultTTL InReceives InHdrErrors InAddrErrors ForwDatagrams InUnknownProtos InDiscards \
         InDelivers OutRequests OutDiscards OutNoRoutes ReasmTimeout ReasmReqds ReasmOKs ReasmFails FragOKs FragFails FragCreates",
        &[2, 64, n(c.ipv4_in), 0, 0, 0, 0, 0, n(c.ipv4_in), outgoing, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    let echo_in = n(c.echo_requests_in + c.echo_replies_in);
    let echo_out = n(c.echo_requests_out + c.echo_replies_out);
    table(
        "Icmp",
        "InMsgs InErrors InCsumErrors InDestUnreachs InTimeExcds InParmProbs InSrcQuenchs InRedirects InEchos \
         InEchoReps InTimestamps InTimestampReps InAddrMasks InAddrMaskReps OutMsgs OutErrors OutDestUnreachs \
         OutTimeExcds OutParmProbs OutSrcQuenchs OutRedirects OutEchos OutEchoReps OutTimestamps OutTimestampReps \
         OutAddrMasks OutAddrMaskReps",
        &[
            echo_in,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            n(c.echo_requests_in),
            n(c.echo_replies_in),
            0,
            0,
            0,
            0,
            echo_out,
            0,
            0,
            0,
            0,
            0,
            0,
            n(c.echo_requests_out),
            n(c.echo_replies_out),
            0,
            0,
            0,
            0,
        ],
    );
    table(
        "Tcp",
        "RtoAlgorithm RtoMin RtoMax MaxConn ActiveOpens PassiveOpens AttemptFails EstabResets CurrEstab InSegs \
         OutSegs RetransSegs InErrs OutRsts InCsumErrors",
        &[1, 200, 120_000, -1, 0, 0, 0, n(c.tcp_resets), established as i64, n(c.tcp_in), n(c.tcp_out), n(c.tcp_retransmits), 0, 0, 0],
    );
    let udp_fields = "InDatagrams NoPorts InErrors OutDatagrams RcvbufErrors SndbufErrors InCsumErrors IgnoredMulti MemErrors";
    table("Udp", udp_fields, &[n(c.udp_in), n(c.udp_no_listener), 0, n(c.udp_out), 0, 0, 0, 0, 0]);
    table("UdpLite", udp_fields, &[0, 0, 0, 0, 0, 0, 0, 0, 0]);
    out
}

/// Особые каталоги сети: `/proc/net`, `/sys/class`, `/sys/class/net` и по
/// каталогу на интерфейс.
pub fn directory(path: &str) -> Option<Vec<(String, NodeKind)>> {
    let file = |name: &str| (String::from(name), NodeKind::File);
    match path {
        "/proc/net" => Some(["dev", "route", "arp", "snmp", "if_inet6"].iter().map(|name| file(name)).collect()),
        "/sys/class" => Some(alloc::vec![(String::from("net"), NodeKind::Directory)]),
        "/sys/class/net" => {
            Some(crate::net::interfaces().iter().map(|info| (String::from(info.name), NodeKind::Directory)).collect())
        }
        _ => {
            let name = path.strip_prefix("/sys/class/net/")?;
            crate::net::interfaces().iter().any(|info| info.name == name).then(|| SYS_CLASS_FILES.iter().map(|name| file(name)).collect())
        }
    }
}
