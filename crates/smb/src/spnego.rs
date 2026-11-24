//! SPNEGO (RFC 4178) just wide enough to carry NTLMSSP: the client's
//! NegTokenInit with one mechanism, NegTokenResp with the next token, and
//! the server's NegTokenResp (state and token).

use alloc::vec::Vec;

use crate::Error;

const SPNEGO_OID: [u8; 6] = [0x2b, 0x06, 0x01, 0x05, 0x05, 0x02];
const NTLMSSP_OID: [u8; 10] = [0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a];

/// negState of NegTokenResp.
pub const ACCEPT_COMPLETED: u8 = 0;
pub const ACCEPT_INCOMPLETE: u8 = 1;
pub const REJECT: u8 = 2;

fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 6);
    out.push(tag);
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else if len < 0x1_0000 {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    } else {
        out.extend_from_slice(&[0x83, (len >> 16) as u8, (len >> 8) as u8, len as u8]);
    }
    out.extend_from_slice(body);
    out
}

/// DER of the mechanism list we offer (`MechTypeList`) — what `mechListMIC`
/// signs.
#[must_use]
pub fn mech_types() -> Vec<u8> {
    tlv(0x30, &tlv(0x06, &NTLMSSP_OID))
}

/// The first token: NegTokenInit offering NTLMSSP with its NEGOTIATE.
#[must_use]
pub fn init(ntlm_negotiate: &[u8]) -> Vec<u8> {
    let mech_types = tlv(0xa0, &mech_types());
    let mech_token = tlv(0xa2, &tlv(0x04, ntlm_negotiate));
    let mut seq = mech_types;
    seq.extend_from_slice(&mech_token);
    let neg_token_init = tlv(0xa0, &tlv(0x30, &seq));
    let mut body = tlv(0x06, &SPNEGO_OID);
    body.extend_from_slice(&neg_token_init);
    tlv(0x60, &body)
}

/// A later token: NegTokenResp carrying the next NTLMSSP message and,
/// when there are keys, the `mechListMIC`.
#[must_use]
pub fn response(ntlm_message: &[u8], mic: Option<&[u8]>) -> Vec<u8> {
    let mut seq = tlv(0xa2, &tlv(0x04, ntlm_message));
    if let Some(mic) = mic {
        seq.extend_from_slice(&tlv(0xa3, &tlv(0x04, mic)));
    }
    tlv(0xa1, &tlv(0x30, &seq))
}

/// Read one DER TLV at the start of `buf`: (tag, body, rest).
fn read_tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let tag = *buf.first()?;
    let first = *buf.get(1)?;
    let (len, head) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..count {
            len = (len << 8) | usize::from(*buf.get(2 + i)?);
        }
        (len, 2 + count)
    };
    let body = buf.get(head..head + len)?;
    Some((tag, body, &buf[head + len..]))
}

/// What the server answered: negState (if present) and its token.
#[derive(Debug, Default)]
pub struct Answer<'a> {
    pub state: Option<u8>,
    pub token: Option<&'a [u8]>,
}

/// Parse the server's security blob: NegTokenResp (`a1`), or — some servers
/// answer the first leg so — a NegTokenInit wrapped in `60`. A raw NTLMSSP
/// message (no SPNEGO at all) is accepted too.
pub fn parse(blob: &[u8]) -> Result<Answer<'_>, Error> {
    if blob.starts_with(b"NTLMSSP\0") {
        return Ok(Answer { state: None, token: Some(blob) });
    }
    let bad = Error::Malformed("bad SPNEGO token");
    let (tag, body, _) = read_tlv(blob).ok_or(bad)?;
    let inner = match tag {
        0xa1 => body,
        0x60 => {
            // OID, then [0] NegTokenInit.
            let (_, _, rest) = read_tlv(body).ok_or(bad)?;
            let (_, init, _) = read_tlv(rest).ok_or(bad)?;
            init
        }
        _ => return Err(bad),
    };
    let (seq_tag, mut fields, _) = read_tlv(inner).ok_or(bad)?;
    if seq_tag != 0x30 {
        return Err(bad);
    }
    let mut answer = Answer::default();
    while !fields.is_empty() {
        let (tag, body, rest) = read_tlv(fields).ok_or(bad)?;
        match tag {
            // negState in NegTokenResp; in NegTokenInit [0] is the mechanism
            // list (a SEQUENCE, not ENUMERATED) and is skipped.
            0xa0 => {
                if let Some((0x0a, value, _)) = read_tlv(body) {
                    answer.state = value.first().copied();
                }
            }
            0xa2 => {
                if let Some((0x04, value, _)) = read_tlv(body) {
                    answer.token = Some(value);
                }
            }
            _ => {}
        }
        fields = rest;
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_wraps_ntlm() {
        let blob = init(b"NTLMSSP\0\x01\x00\x00\x00");
        assert_eq!(blob[0], 0x60);
        assert_eq!(usize::from(blob[1]), blob.len() - 2);
        assert!(blob.windows(NTLMSSP_OID.len()).any(|w| w == NTLMSSP_OID));
        assert!(blob.ends_with(b"NTLMSSP\0\x01\x00\x00\x00"));
    }

    #[test]
    fn long_tokens_use_long_lengths() {
        let token = alloc::vec![7u8; 300];
        let blob = response(&token, None);
        assert_eq!(&blob[..2], &[0xa1, 0x82]);
        assert!(blob.ends_with(&token));
    }

    #[test]
    fn parses_server_response() {
        // a1 { 30 { a0 { 0a 01 01 } a1 { 06 oid } a2 { 04 token } } }
        let token = b"NTLMSSP\0\x02\x00\x00\x00";
        let mut seq = tlv(0xa0, &tlv(0x0a, &[ACCEPT_INCOMPLETE]));
        seq.extend_from_slice(&tlv(0xa1, &tlv(0x06, &NTLMSSP_OID)));
        seq.extend_from_slice(&tlv(0xa2, &tlv(0x04, token)));
        let blob = tlv(0xa1, &tlv(0x30, &seq));
        let answer = parse(&blob).unwrap();
        assert_eq!(answer.state, Some(ACCEPT_INCOMPLETE));
        assert_eq!(answer.token, Some(token.as_slice()));
        let done = tlv(0xa1, &tlv(0x30, &tlv(0xa0, &tlv(0x0a, &[ACCEPT_COMPLETED]))));
        let answer = parse(&done).unwrap();
        assert_eq!(answer.state, Some(ACCEPT_COMPLETED));
        assert!(answer.token.is_none());
    }
}
