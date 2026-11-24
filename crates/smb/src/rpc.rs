//! DCE/RPC over a named pipe, just for `srvsvc` NetrShareEnum (level 1) —
//! the call Windows Explorer makes to show `\\host`.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Error;
use crate::wire::{Writer, from_utf16, u16_at, u32_at, utf16};

const PTYPE_REQUEST: u8 = 0;
const PTYPE_RESPONSE: u8 = 2;
const PTYPE_FAULT: u8 = 3;
const PTYPE_BIND: u8 = 11;
const PTYPE_BIND_ACK: u8 = 12;
const FIRST_FRAG: u8 = 0x01;
const LAST_FRAG: u8 = 0x02;

/// srvsvc interface 4b324fc8-1670-01d3-1278-5a47bf6ee188 v3.0.
const SRVSVC: [u8; 16] = [0xc8, 0x4f, 0x32, 0x4b, 0x70, 0x16, 0xd3, 0x01, 0x12, 0x78, 0x5a, 0x47, 0xbf, 0x6e, 0xe1, 0x88];
/// NDR transfer syntax 8a885d04-1ceb-11c9-9fe8-08002b104860 v2.
const NDR: [u8; 16] = [0x04, 0x5d, 0x88, 0x8a, 0xeb, 0x1c, 0xc9, 0x11, 0x9f, 0xe8, 0x08, 0x00, 0x2b, 0x10, 0x48, 0x60];

const MAX_FRAG: u16 = 4280;
const NETR_SHARE_ENUM: u16 = 15;

/// Share types (`STYPE_*`).
pub const STYPE_DISKTREE: u32 = 0;
pub const STYPE_PRINTQ: u32 = 1;
pub const STYPE_IPC: u32 = 3;
pub const STYPE_SPECIAL: u32 = 0x8000_0000;

/// One share of `NetrShareEnum`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    pub name: String,
    pub kind: u32,
    pub remark: String,
}

impl Share {
    /// A folder a person would open: a disk share that is not hidden (`C$`,
    /// `ADMIN$`) — what Explorer shows.
    #[must_use]
    pub fn is_visible_disk(&self) -> bool {
        self.kind & 0x0FFF_FFFF == STYPE_DISKTREE && self.kind & STYPE_SPECIAL == 0 && !self.name.ends_with('$')
    }
}

fn header(w: &mut Writer, ptype: u8, call_id: u32) {
    w.u8(5).u8(0).u8(ptype).u8(FIRST_FRAG | LAST_FRAG);
    w.bytes(&[0x10, 0, 0, 0]); // little-endian, ASCII, IEEE
    w.u16(0); // frag length, patched
    w.u16(0); // auth length
    w.u32(call_id);
}

fn finish(mut w: Writer) -> Vec<u8> {
    let len = w.len() as u16;
    w.put_u16(8, len);
    w.buf
}

/// BIND to srvsvc.
#[must_use]
pub fn bind(call_id: u32) -> Vec<u8> {
    let mut w = Writer::new();
    header(&mut w, PTYPE_BIND, call_id);
    w.u16(MAX_FRAG).u16(MAX_FRAG).u32(0);
    w.u8(1).u8(0).u16(0); // one context
    w.u16(0).u8(1).u8(0); // context 0, one transfer syntax
    w.bytes(&SRVSVC).u16(3).u16(0);
    w.bytes(&NDR).u32(2);
    finish(w)
}

/// Check a BIND_ACK: the first context accepted.
pub fn check_bind_ack(pdu: &[u8]) -> Result<(), Error> {
    if pdu.get(2) != Some(&PTYPE_BIND_ACK) {
        return Err(Error::Malformed("RPC bind refused"));
    }
    // Secondary address (u16 length + string), aligned to 4, then results.
    let sec_len = usize::from(u16_at(pdu, 24).ok_or(Error::Malformed("short bind ack"))?);
    let mut at = 26 + sec_len;
    at = (at + 3) & !3;
    let count = *pdu.get(at).ok_or(Error::Malformed("short bind ack"))?;
    let result = u16_at(pdu, at + 4).ok_or(Error::Malformed("short bind ack"))?;
    if count == 0 || result != 0 {
        return Err(Error::Malformed("RPC context rejected"));
    }
    Ok(())
}

/// NDR conformant varying string (with the terminating NUL).
fn ndr_string(w: &mut Writer, text: &str) {
    let mut units = utf16(text);
    units.extend_from_slice(&[0, 0]);
    let count = (units.len() / 2) as u32;
    w.u32(count).u32(0).u32(count).bytes(&units).align(4);
}

/// REQUEST: NetrShareEnum(\\server, level 1).
#[must_use]
pub fn share_enum_request(call_id: u32, server: &str) -> Vec<u8> {
    let mut stub = Writer::new();
    // ServerName: unique pointer + string.
    stub.u32(0x0002_0000);
    let mut name = String::from("\\\\");
    name.push_str(server);
    ndr_string(&mut stub, &name);
    // InfoStruct: Level 1, union switch 1, pointer to an empty container.
    stub.u32(1).u32(1).u32(0x0002_0004).u32(0).u32(0);
    // PreferedMaximumLength, ResumeHandle (unique pointer to 0).
    stub.u32(0xFFFF_FFFF).u32(0x0002_0008).u32(0);

    let mut w = Writer::new();
    header(&mut w, PTYPE_REQUEST, call_id);
    w.u32(stub.len() as u32).u16(0).u16(NETR_SHARE_ENUM);
    w.bytes(&stub.buf);
    finish(w)
}

/// Collect the stub of a (possibly fragmented) RESPONSE: `pdus` is the raw
/// pipe data, one or more PDUs back to back. Returns `None` while the last
/// fragment has not arrived.
pub fn response_stub(pdus: &[u8]) -> Result<Option<Vec<u8>>, Error> {
    let mut stub = Vec::new();
    let mut at = 0usize;
    while at < pdus.len() {
        let Some(frag_len) = u16_at(pdus, at + 8).map(usize::from) else {
            return Ok(None);
        };
        if frag_len < 24 {
            return Err(Error::Malformed("RPC fragment too short"));
        }
        let Some(pdu) = pdus.get(at..at + frag_len) else {
            return Ok(None);
        };
        match pdu[2] {
            PTYPE_RESPONSE => {}
            PTYPE_FAULT => {
                return Err(Error::Status(u32_at(pdu, 24).unwrap_or(0)));
            }
            _ => return Err(Error::Malformed("unexpected RPC packet")),
        }
        let auth = usize::from(u16_at(pdu, 10).unwrap_or(0));
        let end = frag_len.saturating_sub(if auth > 0 { auth + 8 } else { 0 });
        stub.extend_from_slice(pdu.get(24..end).ok_or(Error::Malformed("RPC fragment"))?);
        at += frag_len;
        if pdu[3] & LAST_FRAG != 0 {
            return Ok(Some(stub));
        }
    }
    Ok(None)
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn u32(&mut self) -> Result<u32, Error> {
        self.at = (self.at + 3) & !3;
        let v = u32_at(self.buf, self.at).ok_or(Error::Malformed("short NetrShareEnum reply"))?;
        self.at += 4;
        Ok(v)
    }

    fn string(&mut self) -> Result<String, Error> {
        let _max = self.u32()?;
        let _offset = self.u32()?;
        let count = self.u32()? as usize;
        let bytes = self.buf.get(self.at..self.at + count * 2).ok_or(Error::Malformed("short NetrShareEnum string"))?;
        self.at += count * 2;
        let mut text = from_utf16(bytes);
        while text.ends_with('\0') {
            text.pop();
        }
        Ok(text)
    }
}

/// Parse the NetrShareEnum level-1 reply stub.
pub fn parse_share_enum(stub: &[u8]) -> Result<Vec<Share>, Error> {
    let mut r = Reader { buf: stub, at: 0 };
    let _level = r.u32()?;
    let _switch = r.u32()?;
    let container = r.u32()?;
    let mut shares = Vec::new();
    if container != 0 {
        let count = r.u32()? as usize;
        let array = r.u32()?;
        if array != 0 {
            let max = r.u32()? as usize;
            if max < count || count > 4096 {
                return Err(Error::Malformed("NetrShareEnum count"));
            }
            let mut fixed = Vec::with_capacity(count);
            for _ in 0..count {
                let name = r.u32()?;
                let kind = r.u32()?;
                let remark = r.u32()?;
                fixed.push((name, kind, remark));
            }
            for (name, kind, remark) in fixed {
                let name = if name != 0 { r.string()? } else { String::new() };
                let remark = if remark != 0 { r.string()? } else { String::new() };
                shares.push(Share { name, kind, remark });
            }
        }
    }
    let _total = r.u32()?;
    let resume = r.u32()?;
    if resume != 0 {
        let _ = r.u32()?;
    }
    let werror = r.u32()?;
    if werror != 0 {
        return Err(Error::Status(werror));
    }
    Ok(shares)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_layout() {
        let pdu = bind(1);
        assert_eq!(pdu.len(), 72);
        assert_eq!(u16_at(&pdu, 8), Some(72));
        assert_eq!(pdu[2], PTYPE_BIND);
    }

    #[test]
    fn share_enum_round_trip() {
        // Build a reply the way a server marshals it.
        let mut w = Writer::new();
        w.u32(1).u32(1).u32(0x2_0000).u32(3).u32(0x2_0004).u32(3);
        w.u32(0x2_0008).u32(STYPE_DISKTREE).u32(0x2_000c);
        w.u32(0x2_0010).u32(STYPE_IPC | STYPE_SPECIAL).u32(0x2_0014);
        w.u32(0x2_0018).u32(STYPE_DISKTREE | STYPE_SPECIAL).u32(0);
        ndr_string(&mut w, "Общая");
        ndr_string(&mut w, "Файлы");
        ndr_string(&mut w, "IPC$");
        ndr_string(&mut w, "Remote IPC");
        ndr_string(&mut w, "C$");
        w.u32(3).u32(0x2_001c).u32(0).u32(0);
        let shares = parse_share_enum(&w.buf).unwrap();
        assert_eq!(shares.len(), 3);
        assert_eq!(shares[0].name, "Общая");
        assert_eq!(shares[0].remark, "Файлы");
        assert!(shares[0].is_visible_disk());
        assert!(!shares[1].is_visible_disk());
        assert_eq!(shares[2].remark, "");
        assert!(!shares[2].is_visible_disk());

        // Wrapped into two fragments.
        let stub = &w.buf;
        let mut pdus = Vec::new();
        for (index, part) in stub.chunks(stub.len() / 2 + 1).enumerate() {
            let mut f = Writer::new();
            header(&mut f, PTYPE_RESPONSE, 2);
            f.buf[3] = if index == 0 { FIRST_FRAG } else { LAST_FRAG };
            f.u32(stub.len() as u32).u16(0).u8(0).u8(0).bytes(part);
            pdus.extend_from_slice(&finish(f));
        }
        assert_eq!(response_stub(&pdus[..pdus.len() - 3]).unwrap(), None);
        assert_eq!(response_stub(&pdus).unwrap().unwrap(), *stub);
    }

    #[test]
    fn request_has_opnum_15() {
        let pdu = share_enum_request(2, "NAS");
        assert_eq!(pdu[2], PTYPE_REQUEST);
        assert_eq!(u16_at(&pdu, 22), Some(15));
        assert_eq!(u16_at(&pdu, 8).map(usize::from), Some(pdu.len()));
    }
}
