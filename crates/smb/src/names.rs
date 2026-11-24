//! Finding computers on the local network: NetBIOS name service (RFC 1002),
//! LLMNR (RFC 4795), multicast DNS (RFC 6762) and WS-Discovery probes —
//! packets only, the sockets are the caller's.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub const NBNS_PORT: u16 = 137;
pub const LLMNR_PORT: u16 = 5355;
pub const MDNS_PORT: u16 = 5353;
pub const WSD_PORT: u16 = 3702;
pub const LLMNR_GROUP: [u8; 4] = [224, 0, 0, 252];
pub const MDNS_GROUP: [u8; 4] = [224, 0, 0, 251];
pub const WSD_GROUP: [u8; 4] = [239, 255, 255, 250];

pub const TYPE_A: u16 = 1;
pub const TYPE_PTR: u16 = 12;
pub const TYPE_SRV: u16 = 33;
const TYPE_NB: u16 = 0x20;
const TYPE_NBSTAT: u16 = 0x21;

fn be16(buf: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*buf.get(at)?, *buf.get(at + 1)?]))
}

// ---------------------------------------------------------------------------
// NetBIOS name service
// ---------------------------------------------------------------------------

/// First-level encoding of a NetBIOS name (`name` padded to 15 with spaces,
/// `suffix` as the 16th byte), as a 34-byte DNS-style label.
#[must_use]
pub fn nb_encode(name: &str, suffix: u8) -> [u8; 34] {
    let mut raw = [b' '; 16];
    if name == "*" {
        raw = [0; 16];
        raw[0] = b'*';
    } else {
        for (slot, byte) in raw.iter_mut().zip(name.bytes().take(15)) {
            *slot = byte.to_ascii_uppercase();
        }
    }
    raw[15] = suffix;
    let mut out = [0u8; 34];
    out[0] = 32;
    for (i, byte) in raw.iter().enumerate() {
        out[1 + i * 2] = b'A' + (byte >> 4);
        out[2 + i * 2] = b'A' + (byte & 0x0f);
    }
    out
}

/// Decode a first-level encoded name at `at`: (name without padding,
/// suffix, offset after the name).
fn nb_decode(msg: &[u8], at: usize) -> Option<(String, u8, usize)> {
    if *msg.get(at)? != 32 {
        return None;
    }
    let mut raw = [0u8; 16];
    for (i, slot) in raw.iter_mut().enumerate() {
        let hi = msg.get(at + 1 + i * 2)?.checked_sub(b'A')?;
        let lo = msg.get(at + 2 + i * 2)?.checked_sub(b'A')?;
        *slot = (hi << 4) | (lo & 0x0f);
    }
    // Scope labels up to the root.
    let mut end = at + 33;
    while *msg.get(end)? != 0 {
        end += 1 + usize::from(*msg.get(end)?);
    }
    let name = String::from_utf8_lossy(&raw[..15]).trim_end_matches([' ', '\0']).into();
    Some((name, raw[15], end + 1))
}

fn nb_header(id: u16, flags: u16, questions: u16, answers: u16) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for word in [id, flags, questions, answers, 0, 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    out
}

/// NAME QUERY REQUEST for `name<20>` (file server) or another suffix.
#[must_use]
pub fn nb_name_query(id: u16, name: &str, suffix: u8, broadcast: bool) -> Vec<u8> {
    let mut out = nb_header(id, if broadcast { 0x0110 } else { 0x0100 }, 1, 0);
    out.extend_from_slice(&nb_encode(name, suffix));
    out.extend_from_slice(&TYPE_NB.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

/// NODE STATUS REQUEST (`nbtstat -A`): the names a host has registered.
#[must_use]
pub fn nb_node_status_query(id: u16) -> Vec<u8> {
    let mut out = nb_header(id, 0, 1, 0);
    out.extend_from_slice(&nb_encode("*", 0));
    out.extend_from_slice(&TYPE_NBSTAT.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

/// A NetBIOS reply: positive name query answers (addresses) or a node
/// status (registered names).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NbAnswer {
    pub id: u16,
    pub name: String,
    pub addresses: Vec<[u8; 4]>,
    /// Node status: `(name, suffix, group)`.
    pub names: Vec<(String, u8, bool)>,
}

impl NbAnswer {
    /// The computer name from a node status: the unique `<00>` name.
    #[must_use]
    pub fn computer_name(&self) -> Option<&str> {
        self.names.iter().find(|(_, suffix, group)| *suffix == 0 && !group).map(|(name, _, _)| name.as_str())
    }

    /// The workgroup or domain: the group `<00>` name.
    #[must_use]
    pub fn workgroup(&self) -> Option<&str> {
        self.names.iter().find(|(_, suffix, group)| *suffix == 0 && *group).map(|(name, _, _)| name.as_str())
    }

    /// Does the host run a file server (`<20>`).
    #[must_use]
    pub fn serves_files(&self) -> bool {
        self.names.iter().any(|(_, suffix, _)| *suffix == 0x20)
    }
}

/// Parse a NetBIOS response.
#[must_use]
pub fn nb_parse_response(msg: &[u8]) -> Option<NbAnswer> {
    let id = be16(msg, 0)?;
    let flags = be16(msg, 2)?;
    if flags & 0x8000 == 0 || flags & 0x000f != 0 || be16(msg, 6)? == 0 {
        return None;
    }
    let (name, _, at) = nb_decode(msg, 12)?;
    let rtype = be16(msg, at)?;
    let rdlen = usize::from(be16(msg, at + 8)?);
    let rdata = msg.get(at + 10..at + 10 + rdlen)?;
    let mut answer = NbAnswer { id, name, ..NbAnswer::default() };
    match rtype {
        TYPE_NB => {
            for entry in rdata.chunks_exact(6) {
                answer.addresses.push([entry[2], entry[3], entry[4], entry[5]]);
            }
        }
        TYPE_NBSTAT => {
            let count = usize::from(*rdata.first()?);
            for index in 0..count {
                let entry = rdata.get(1 + index * 18..1 + index * 18 + 18)?;
                let name = String::from_utf8_lossy(&entry[..15]).trim_end_matches([' ', '\0']).into();
                let group = entry[16] & 0x80 != 0;
                answer.names.push((name, entry[15], group));
            }
        }
        _ => return None,
    }
    Some(answer)
}

/// A query someone sent us: (id, name, suffix, type is node status).
#[must_use]
pub fn nb_parse_query(msg: &[u8]) -> Option<(u16, String, u8, bool)> {
    let id = be16(msg, 0)?;
    let flags = be16(msg, 2)?;
    // Requests only, opcode 0 (query).
    if flags & 0xf800 != 0 || be16(msg, 4)? != 1 {
        return None;
    }
    let (name, suffix, at) = nb_decode(msg, 12)?;
    let qtype = be16(msg, at)?;
    match qtype {
        TYPE_NB => Some((id, name, suffix, false)),
        TYPE_NBSTAT => Some((id, name, suffix, true)),
        _ => None,
    }
}

/// POSITIVE NAME QUERY RESPONSE: `name<suffix>` is at `address`.
#[must_use]
pub fn nb_name_response(id: u16, name: &str, suffix: u8, address: [u8; 4]) -> Vec<u8> {
    let mut out = nb_header(id, 0x8500, 0, 1);
    out.extend_from_slice(&nb_encode(name, suffix));
    out.extend_from_slice(&TYPE_NB.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&300_000u32.to_be_bytes());
    out.extend_from_slice(&6u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes()); // B-node, unique
    out.extend_from_slice(&address);
    out
}

/// NODE STATUS RESPONSE with our names.
#[must_use]
pub fn nb_node_status_response(id: u16, names: &[(&str, u8, bool)], mac: [u8; 6]) -> Vec<u8> {
    let mut out = nb_header(id, 0x8400, 0, 1);
    out.extend_from_slice(&nb_encode("*", 0));
    out.extend_from_slice(&TYPE_NBSTAT.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    let rdlen = 1 + names.len() * 18 + 46;
    out.extend_from_slice(&(rdlen as u16).to_be_bytes());
    out.push(names.len() as u8);
    for (name, suffix, group) in names {
        let mut raw = [b' '; 15];
        for (slot, byte) in raw.iter_mut().zip(name.bytes()) {
            *slot = byte.to_ascii_uppercase();
        }
        out.extend_from_slice(&raw);
        out.push(*suffix);
        // Active, B-node; group bit.
        out.extend_from_slice(&(0x0400u16 | if *group { 0x8000 } else { 0 }).to_be_bytes());
    }
    let mut stats = [0u8; 46];
    stats[..6].copy_from_slice(&mac);
    out.extend_from_slice(&stats);
    out
}

// ---------------------------------------------------------------------------
// DNS-format messages: LLMNR and multicast DNS
// ---------------------------------------------------------------------------

/// A resource record of interest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rdata {
    A([u8; 4]),
    Ptr(String),
    Srv { port: u16, target: String },
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub name: String,
    pub rtype: u16,
    pub data: Rdata,
}

/// Parsed DNS message.
#[derive(Debug, Clone, Default)]
pub struct DnsMessage {
    pub id: u16,
    pub flags: u16,
    pub questions: Vec<(String, u16)>,
    /// Answers, authority and additional records together.
    pub records: Vec<Record>,
}

fn write_dns_name(out: &mut Vec<u8>, name: &str) {
    for label in name.trim_end_matches('.').split('.').filter(|label| !label.is_empty()) {
        let bytes = label.as_bytes();
        out.push(bytes.len().min(63) as u8);
        out.extend_from_slice(&bytes[..bytes.len().min(63)]);
    }
    out.push(0);
}

/// Read a (possibly compressed) DNS name at `at`: (name, offset after it).
fn read_dns_name(msg: &[u8], at: usize) -> Option<(String, usize)> {
    let mut name = String::new();
    let mut pos = at;
    let mut end = None;
    for _ in 0..64 {
        let len = *msg.get(pos)?;
        if len & 0xc0 == 0xc0 {
            let target = (usize::from(len & 0x3f) << 8) | usize::from(*msg.get(pos + 1)?);
            end.get_or_insert(pos + 2);
            pos = target;
            continue;
        }
        if len == 0 {
            return Some((name, end.unwrap_or(pos + 1)));
        }
        let label = msg.get(pos + 1..pos + 1 + usize::from(len))?;
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&String::from_utf8_lossy(label));
        pos += 1 + usize::from(len);
    }
    None
}

/// A one-question query. `unicast` sets the mDNS "QU" bit (answer to the
/// asking port directly).
#[must_use]
pub fn dns_query(id: u16, name: &str, qtype: u16, unicast: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for word in [id, 0, 1, 0, 0, 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    write_dns_name(&mut out, name);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&(if unicast { 0x8001u16 } else { 1 }).to_be_bytes());
    out
}

/// Parse a DNS-format message.
#[must_use]
pub fn dns_parse(msg: &[u8]) -> Option<DnsMessage> {
    let mut parsed = DnsMessage { id: be16(msg, 0)?, flags: be16(msg, 2)?, ..DnsMessage::default() };
    let questions = be16(msg, 4)?;
    let records = u32::from(be16(msg, 6)?) + u32::from(be16(msg, 8)?) + u32::from(be16(msg, 10)?);
    let mut at = 12;
    for _ in 0..questions {
        let (name, next) = read_dns_name(msg, at)?;
        parsed.questions.push((name, be16(msg, next)?));
        at = next + 4;
    }
    for _ in 0..records.min(256) {
        let (name, next) = read_dns_name(msg, at)?;
        let rtype = be16(msg, next)?;
        let rdlen = usize::from(be16(msg, next + 8)?);
        let rdata_at = next + 10;
        let rdata = msg.get(rdata_at..rdata_at + rdlen)?;
        let data = match rtype {
            TYPE_A if rdlen == 4 => Rdata::A([rdata[0], rdata[1], rdata[2], rdata[3]]),
            TYPE_PTR => Rdata::Ptr(read_dns_name(msg, rdata_at)?.0),
            TYPE_SRV if rdlen >= 7 => Rdata::Srv { port: be16(rdata, 4)?, target: read_dns_name(msg, rdata_at + 6)?.0 },
            _ => Rdata::Other,
        };
        parsed.records.push(Record { name, rtype, data });
        at = rdata_at + rdlen;
    }
    Some(parsed)
}

/// LLMNR answer: `name` has address `address` (question echoed).
#[must_use]
pub fn llmnr_response(id: u16, name: &str, address: [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    for word in [id, 0x8000, 1, 1, 0, 0] {
        out.extend_from_slice(&word.to_be_bytes());
    }
    write_dns_name(&mut out, name);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    write_dns_name(&mut out, name);
    out.extend_from_slice(&TYPE_A.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&30u32.to_be_bytes());
    out.extend_from_slice(&4u16.to_be_bytes());
    out.extend_from_slice(&address);
    out
}

/// Is a DNS message a query (not a response).
#[must_use]
pub fn dns_is_query(msg: &DnsMessage) -> bool {
    msg.flags & 0x8000 == 0
}

/// Hosts that answered an mDNS `_smb._tcp.local` browse: `(host name,
/// address)` from SRV targets and their A records.
#[must_use]
pub fn mdns_smb_hosts(msg: &DnsMessage) -> Vec<(String, [u8; 4])> {
    let mut out: Vec<(String, [u8; 4])> = Vec::new();
    for record in &msg.records {
        if let Rdata::Srv { target, .. } = &record.data {
            if let Some(address) = msg.records.iter().find_map(|r| match &r.data {
                Rdata::A(address) if r.name.eq_ignore_ascii_case(target) => Some(*address),
                _ => None,
            }) {
                let host = target.trim_end_matches(".local").trim_end_matches(".local.");
                if !out.iter().any(|(_, a)| *a == address) {
                    out.push((String::from(host), address));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// WS-Discovery
// ---------------------------------------------------------------------------

/// `urn:uuid:` text of 16 random bytes (version 4 bits set).
#[must_use]
pub fn uuid_text(bytes: &[u8; 16]) -> String {
    let mut b = *bytes;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// A WS-Discovery Probe for devices — Windows answers it while network
/// discovery is on (Function Discovery Resource Publication), and so does
/// `wsdd` next to Samba.
#[must_use]
pub fn wsd_probe(message_id: &[u8; 16]) -> Vec<u8> {
    format!(
        concat!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>",
            "<soap:Envelope xmlns:soap=\"http://www.w3.org/2003/05/soap-envelope\" ",
            "xmlns:wsa=\"http://schemas.xmlsoap.org/ws/2004/08/addressing\" ",
            "xmlns:wsd=\"http://schemas.xmlsoap.org/ws/2005/04/discovery\" ",
            "xmlns:wsdp=\"http://schemas.xmlsoap.org/ws/2006/02/devprof\">",
            "<soap:Header><wsa:To>urn:schemas-xmlsoap-org:ws:2005:04:discovery</wsa:To>",
            "<wsa:Action>http://schemas.xmlsoap.org/ws/2005/04/discovery/Probe</wsa:Action>",
            "<wsa:MessageID>urn:uuid:{}</wsa:MessageID></soap:Header>",
            "<soap:Body><wsd:Probe><wsd:Types>wsdp:Device</wsd:Types></wsd:Probe></soap:Body></soap:Envelope>"
        ),
        uuid_text(message_id)
    )
    .into_bytes()
}

/// Is this a ProbeMatches answer from a computer (not a printer or a
/// media renderer).
#[must_use]
pub fn wsd_is_computer(xml: &[u8]) -> bool {
    let text = String::from_utf8_lossy(xml);
    text.contains("ProbeMatches") && (text.contains("Computer") || !text.contains("Print"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn netbios_name_encoding() {
        // RFC 1001 example: "FRED" padded with spaces, suffix 0x20.
        let encoded = nb_encode("fred", 0x20);
        assert_eq!(&encoded[1..33], b"EGFCEFEECACACACACACACACACACACACA");
        let (name, suffix, end) = nb_decode(&encoded, 0).unwrap();
        assert_eq!((name.as_str(), suffix, end), ("FRED", 0x20, 34));
        assert_eq!(&nb_encode("*", 0)[1..5], b"CKAA");
    }

    #[test]
    fn netbios_query_and_answer() {
        let query = nb_name_query(0x1234, "asdk-bckp", 0x20, true);
        let (id, name, suffix, status) = nb_parse_query(&query).unwrap();
        assert_eq!((id, name.as_str(), suffix, status), (0x1234, "ASDK-BCKP", 0x20, false));
        let answer = nb_name_response(0x1234, "ASDK-BCKP", 0x20, [192, 168, 88, 246]);
        let parsed = nb_parse_response(&answer).unwrap();
        assert_eq!(parsed.addresses, alloc::vec![[192, 168, 88, 246]]);
        // Our own query is not taken for an answer.
        assert!(nb_parse_response(&query).is_none());
    }

    #[test]
    fn netbios_node_status() {
        let reply = nb_node_status_response(7, &[("FOSSAI", 0, false), ("WORKGROUP", 0, true), ("FOSSAI", 0x20, false)], [1, 2, 3, 4, 5, 6]);
        let parsed = nb_parse_response(&reply).unwrap();
        assert_eq!(parsed.computer_name(), Some("FOSSAI"));
        assert_eq!(parsed.workgroup(), Some("WORKGROUP"));
        assert!(parsed.serves_files());
        let (_, name, _, status) = nb_parse_query(&nb_node_status_query(9)).unwrap();
        assert_eq!((name.as_str(), status), ("*", true));
    }

    #[test]
    fn dns_round_trip() {
        let q = dns_query(5, "_smb._tcp.local", TYPE_PTR, true);
        let parsed = dns_parse(&q).unwrap();
        assert!(dns_is_query(&parsed));
        assert_eq!(parsed.questions, alloc::vec![(String::from("_smb._tcp.local"), TYPE_PTR)]);
        let answer = llmnr_response(5, "fossai", [10, 0, 2, 15]);
        let parsed = dns_parse(&answer).unwrap();
        assert!(!dns_is_query(&parsed));
        assert_eq!(parsed.records[0].data, Rdata::A([10, 0, 2, 15]));
    }

    #[test]
    fn mdns_browse_answer() {
        // PTR _smb._tcp.local -> nas._smb._tcp.local, SRV -> nas.local:445,
        // A nas.local -> 192.168.88.246, with compression pointers.
        let mut m = Vec::new();
        for word in [0u16, 0x8400, 0, 1, 0, 2] {
            m.extend_from_slice(&word.to_be_bytes());
        }
        let svc_at = m.len();
        write_dns_name(&mut m, "_smb._tcp.local");
        m.extend_from_slice(&[0, 12, 0, 1, 0, 0, 0, 10]);
        let mut rdata = alloc::vec![3, b'n', b'a', b's', 0xc0, svc_at as u8];
        m.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
        let inst_at = m.len();
        m.append(&mut rdata);
        m.extend_from_slice(&[0xc0, inst_at as u8, 0, 33, 0x80, 1, 0, 0, 0, 10]);
        let mut srv = alloc::vec![0, 0, 0, 0, 1, 189];
        write_dns_name(&mut srv, "nas.local");
        m.extend_from_slice(&(srv.len() as u16).to_be_bytes());
        m.extend_from_slice(&srv);
        write_dns_name(&mut m, "nas.local");
        m.extend_from_slice(&[0, 1, 0x80, 1, 0, 0, 0, 10, 0, 4, 192, 168, 88, 246]);
        let parsed = dns_parse(&m).unwrap();
        assert_eq!(mdns_smb_hosts(&parsed), alloc::vec![(String::from("nas"), [192, 168, 88, 246])]);
    }

    #[test]
    fn wsd_probe_text() {
        let probe = wsd_probe(&[0; 16]);
        let text = core::str::from_utf8(&probe).unwrap();
        assert!(text.contains("urn:uuid:00000000-0000-4000-8000-000000000000"));
        assert!(wsd_is_computer(b"<wsd:ProbeMatches><wsd:Types>wsdp:Device pub:Computer</wsd:Types>"));
        assert!(!wsd_is_computer(b"<wsd:Probe>"));
    }
}
