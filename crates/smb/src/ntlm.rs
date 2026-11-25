//! NTLMSSP client (MS-NLMP): NEGOTIATE, CHALLENGE, AUTHENTICATE with NTLMv2,
//! key exchange and the message integrity code.
//!
//! Only NTLMv2 is spoken. LM and NTLMv1 are refused by every Windows since
//! Vista by default and by Samba since 4.5 (`ntlm auth = ntlmv2-only`).

use alloc::string::String;
use alloc::vec::Vec;

use crate::Error;
use crate::crypto::{hmac_md5, md4, rc4};
use crate::wire::{Writer, u16_at, u32_at, utf16};

pub const SIGNATURE: &[u8; 8] = b"NTLMSSP\0";

pub const NEGOTIATE_UNICODE: u32 = 0x0000_0001;
pub const REQUEST_TARGET: u32 = 0x0000_0004;
pub const NEGOTIATE_SIGN: u32 = 0x0000_0010;
pub const NEGOTIATE_NTLM: u32 = 0x0000_0200;
pub const NEGOTIATE_ANONYMOUS: u32 = 0x0000_0800;
pub const NEGOTIATE_ALWAYS_SIGN: u32 = 0x0000_8000;
pub const NEGOTIATE_EXTENDED_SESSIONSECURITY: u32 = 0x0008_0000;
pub const NEGOTIATE_TARGET_INFO: u32 = 0x0080_0000;
pub const NEGOTIATE_VERSION: u32 = 0x0200_0000;
pub const NEGOTIATE_128: u32 = 0x2000_0000;
pub const NEGOTIATE_KEY_EXCH: u32 = 0x4000_0000;
pub const NEGOTIATE_56: u32 = 0x8000_0000;

/// What we ask for — what a Windows 10 client asks for, minus sealing.
pub const CLIENT_FLAGS: u32 = NEGOTIATE_UNICODE
    | REQUEST_TARGET
    | NEGOTIATE_SIGN
    | NEGOTIATE_NTLM
    | NEGOTIATE_ALWAYS_SIGN
    | NEGOTIATE_EXTENDED_SESSIONSECURITY
    | NEGOTIATE_TARGET_INFO
    | NEGOTIATE_VERSION
    | NEGOTIATE_128
    | NEGOTIATE_KEY_EXCH
    | NEGOTIATE_56;

const AV_EOL: u16 = 0;
const AV_FLAGS: u16 = 6;
const AV_TIMESTAMP: u16 = 7;
const AV_TARGET_NAME: u16 = 9;
const AV_CHANNEL_BINDINGS: u16 = 10;
pub const AV_NB_COMPUTER: u16 = 1;
pub const AV_NB_DOMAIN: u16 = 2;
pub const AV_DNS_COMPUTER: u16 = 3;

/// Version field: "Windows 10.0 build 19041, NTLM revision 15".
const VERSION: [u8; 8] = [10, 0, 0x61, 0x4A, 0, 0, 0, 0x0F];

/// Who logs on.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    pub user: String,
    pub domain: String,
    /// MD4 of the UTF-16LE password — the only form of the password kept.
    pub nt_hash: [u8; 16],
}

impl core::fmt::Debug for Identity {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Never the hash.
        write!(f, "Identity({}\\{})", self.domain, self.user)
    }
}

/// NT hash of a password.
#[must_use]
pub fn nt_hash(password: &str) -> [u8; 16] {
    md4(&utf16(password))
}

/// Split what a person typed as the user name: `DOMAIN\user`, `user@domain`
/// or just `user` (empty domain — the server's local accounts).
#[must_use]
pub fn split_user(typed: &str) -> (String, String) {
    let typed = typed.trim();
    if let Some((domain, user)) = typed.split_once('\\') {
        return (String::from(user), String::from(domain));
    }
    if let Some((user, domain)) = typed.split_once('@') {
        return (String::from(user), String::from(domain));
    }
    (String::from(typed), String::new())
}

/// The parts of a CHALLENGE message the client uses.
#[derive(Debug, Clone)]
pub struct Challenge {
    pub flags: u32,
    pub server_challenge: [u8; 8],
    /// Raw AV pairs, EOL included.
    pub target_info: Vec<u8>,
}

impl Challenge {
    /// Parse a CHALLENGE_MESSAGE.
    pub fn parse(msg: &[u8]) -> Result<Self, Error> {
        if msg.get(..8) != Some(SIGNATURE.as_slice()) || u32_at(msg, 8) != Some(2) {
            return Err(Error::Malformed("not an NTLM challenge"));
        }
        let flags = u32_at(msg, 20).ok_or(Error::Malformed("short NTLM challenge"))?;
        let mut server_challenge = [0u8; 8];
        server_challenge.copy_from_slice(msg.get(24..32).ok_or(Error::Malformed("short NTLM challenge"))?);
        let target_info = if flags & NEGOTIATE_TARGET_INFO != 0 {
            let len = usize::from(u16_at(msg, 40).unwrap_or(0));
            let offset = u32_at(msg, 44).unwrap_or(0) as usize;
            msg.get(offset..offset + len).ok_or(Error::Malformed("NTLM target info out of bounds"))?.to_vec()
        } else {
            Vec::new()
        };
        Ok(Self { flags, server_challenge, target_info })
    }

    /// Value of an AV pair.
    #[must_use]
    pub fn av(&self, id: u16) -> Option<&[u8]> {
        av_pairs(&self.target_info).find(|(pair, _)| *pair == id).map(|(_, value)| value)
    }

    /// The server's NetBIOS computer name, if it told it.
    #[must_use]
    pub fn computer_name(&self) -> Option<String> {
        self.av(AV_NB_COMPUTER).map(crate::wire::from_utf16)
    }
}

/// Iterate `(id, value)` over AV pairs up to EOL.
fn av_pairs(info: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    let mut at = 0usize;
    core::iter::from_fn(move || {
        let id = u16_at(info, at)?;
        let len = usize::from(u16_at(info, at + 2)?);
        if id == AV_EOL {
            return None;
        }
        let value = info.get(at + 4..at + 4 + len)?;
        at += 4 + len;
        Some((id, value))
    })
}

/// NEGOTIATE_MESSAGE.
#[must_use]
pub fn negotiate(anonymous: bool) -> Vec<u8> {
    let mut w = Writer::new();
    let flags = if anonymous { CLIENT_FLAGS | NEGOTIATE_ANONYMOUS } else { CLIENT_FLAGS };
    w.bytes(SIGNATURE).u32(1).u32(flags);
    // Domain and workstation fields — empty.
    w.zeros(16);
    w.bytes(&VERSION);
    w.buf
}

/// Result of [`ntlmv2`].
pub struct V2 {
    pub nt_response: Vec<u8>,
    pub lm_response: [u8; 24],
    pub session_base_key: [u8; 16],
}

/// NTOWFv2.
#[must_use]
pub fn ntowf_v2(nt_hash: &[u8; 16], user: &str, domain: &str) -> [u8; 16] {
    let mut who = String::from(user).to_uppercase();
    who.push_str(domain);
    hmac_md5(nt_hash, &[&utf16(&who)])
}

/// The NTLMv2 responses (MS-NLMP 3.3.2). `av` is the AV pair list to embed,
/// EOL included.
#[must_use]
pub fn ntlmv2(id: &Identity, server_challenge: &[u8; 8], client_challenge: &[u8; 8], time: u64, av: &[u8]) -> V2 {
    let key = ntowf_v2(&id.nt_hash, &id.user, &id.domain);
    let mut temp = Vec::with_capacity(32 + av.len());
    temp.extend_from_slice(&[1, 1, 0, 0, 0, 0, 0, 0]);
    temp.extend_from_slice(&time.to_le_bytes());
    temp.extend_from_slice(client_challenge);
    temp.extend_from_slice(&[0; 4]);
    temp.extend_from_slice(av);
    temp.extend_from_slice(&[0; 4]);
    let proof = hmac_md5(&key, &[server_challenge, &temp]);
    let mut nt_response = Vec::with_capacity(16 + temp.len());
    nt_response.extend_from_slice(&proof);
    nt_response.extend_from_slice(&temp);
    let lm = hmac_md5(&key, &[server_challenge, client_challenge]);
    let mut lm_response = [0u8; 24];
    lm_response[..16].copy_from_slice(&lm);
    lm_response[16..].copy_from_slice(client_challenge);
    V2 { nt_response, lm_response, session_base_key: hmac_md5(&key, &[&proof]) }
}

/// AV pairs for the client's NTLMv2 blob: the server's, with the MIC flag,
/// empty channel bindings and the SPN of the target added.
fn client_av(challenge: &Challenge, spn: &str) -> Vec<u8> {
    let mut w = Writer::new();
    let mut flags_done = false;
    let mut bindings = false;
    let mut target = false;
    for (id, value) in av_pairs(&challenge.target_info) {
        match id {
            AV_FLAGS => {
                let old = u32_at(value, 0).unwrap_or(0);
                w.u16(AV_FLAGS).u16(4).u32(old | 2);
                flags_done = true;
            }
            _ => {
                bindings |= id == AV_CHANNEL_BINDINGS;
                target |= id == AV_TARGET_NAME;
                w.u16(id).u16(value.len() as u16).bytes(value);
            }
        }
    }
    if !flags_done {
        w.u16(AV_FLAGS).u16(4).u32(2);
    }
    if !bindings {
        w.u16(AV_CHANNEL_BINDINGS).u16(16).zeros(16);
    }
    if !target {
        let name = utf16(spn);
        w.u16(AV_TARGET_NAME).u16(name.len() as u16).bytes(&name);
    }
    w.u16(AV_EOL).u16(0);
    w.buf
}

/// NTLM message signature (MS-NLMP 3.4.4.2, extended session security) of
/// the client's first signed message — the SPNEGO `mechListMIC` over the
/// mechanism list. Required by MS-SPNG once the AUTHENTICATE carries a MIC;
/// Samba waits for it and answers "more processing" without.
#[must_use]
pub fn client_mic(exported: &[u8; 16], flags: u32, message: &[u8]) -> [u8; 16] {
    let mut sign_key_input = exported.to_vec();
    sign_key_input.extend_from_slice(b"session key to client-to-server signing key magic constant\0");
    let signing_key = crate::crypto::md5(&sign_key_input);
    let seal_len = if flags & NEGOTIATE_128 != 0 {
        16
    } else if flags & NEGOTIATE_56 != 0 {
        7
    } else {
        5
    };
    let mut seal_key_input = exported[..seal_len].to_vec();
    seal_key_input.extend_from_slice(b"session key to client-to-server sealing key magic constant\0");
    let sealing_key = crate::crypto::md5(&seal_key_input);
    let seq = 0u32.to_le_bytes();
    let mac = hmac_md5(&signing_key, &[&seq, message]);
    let mut checksum = [0u8; 8];
    checksum.copy_from_slice(&mac[..8]);
    if flags & NEGOTIATE_KEY_EXCH != 0 {
        rc4(&sealing_key, &mut checksum);
    }
    let mut out = [0u8; 16];
    out[..4].copy_from_slice(&1u32.to_le_bytes());
    out[4..12].copy_from_slice(&checksum);
    out[12..].copy_from_slice(&seq);
    out
}

/// AUTHENTICATE_MESSAGE, the exported session key and the negotiated flags.
///
/// `who` — `None` for an anonymous (null) session. `random` gives the client
/// challenge (8 bytes) and the random session key (16 bytes). `time` is the
/// client's FILETIME, used when the server sent no timestamp. `negotiate_msg`
/// and `challenge_msg` are the raw first two messages (for the MIC).
#[allow(clippy::too_many_arguments)]
pub fn authenticate(
    negotiate_msg: &[u8],
    challenge_msg: &[u8],
    who: Option<&Identity>,
    workstation: &str,
    spn: &str,
    time: u64,
    random: &[u8; 24],
) -> Result<(Vec<u8>, [u8; 16], u32), Error> {
    let challenge = Challenge::parse(challenge_msg)?;
    let mut flags = challenge.flags & (CLIENT_FLAGS | NEGOTIATE_ANONYMOUS);
    // Version is ours to send regardless.
    flags |= NEGOTIATE_VERSION;

    let (lm, nt, user, domain, exported, encrypted, with_mic) = match who {
        None => {
            flags |= NEGOTIATE_ANONYMOUS;
            flags &= !NEGOTIATE_KEY_EXCH;
            (alloc::vec![0u8], Vec::new(), Vec::new(), Vec::new(), [0u8; 16], Vec::new(), false)
        }
        Some(id) => {
            flags &= !NEGOTIATE_ANONYMOUS;
            let client_challenge: [u8; 8] = random[..8].try_into().map_err(|_| Error::Malformed("random"))?;
            let server_time = challenge.av(AV_TIMESTAMP).and_then(|v| crate::wire::u64_at(v, 0));
            let av = client_av(&challenge, spn);
            let v2 = ntlmv2(id, &challenge.server_challenge, &client_challenge, server_time.unwrap_or(time), &av);
            // With a server timestamp the LMv2 response is sent as zeros.
            let lm = if server_time.is_some() { alloc::vec![0u8; 24] } else { v2.lm_response.to_vec() };
            let (exported, encrypted) = if flags & NEGOTIATE_KEY_EXCH != 0 {
                let mut exported = [0u8; 16];
                exported.copy_from_slice(&random[8..24]);
                let mut encrypted = exported;
                rc4(&v2.session_base_key, &mut encrypted);
                (exported, encrypted.to_vec())
            } else {
                (v2.session_base_key, Vec::new())
            };
            (lm, v2.nt_response, utf16(&id.user), utf16(&id.domain), exported, encrypted, true)
        }
    };
    let host = utf16(workstation);

    let mut w = Writer::new();
    w.bytes(SIGNATURE).u32(3);
    let fields_at = w.len();
    w.zeros(6 * 8);
    w.u32(flags);
    w.bytes(&VERSION);
    let mic_at = w.len();
    w.zeros(16);
    let payloads: [&[u8]; 6] = [&lm, &nt, &domain, &user, &host, &encrypted];
    for (index, payload) in payloads.iter().enumerate() {
        let offset = w.len();
        let field = fields_at + index * 8;
        w.put_u16(field, payload.len() as u16);
        w.put_u16(field + 2, payload.len() as u16);
        w.put_u32(field + 4, offset as u32);
        w.bytes(payload);
    }
    let mut msg = w.buf;
    if with_mic {
        let mic = hmac_md5(&exported, &[negotiate_msg, challenge_msg, &msg]);
        msg[mic_at..mic_at + 16].copy_from_slice(&mic);
    }
    Ok((msg, exported, flags))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let clean: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
        clean.chunks(2).map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
    }

    /// MS-NLMP 4.2.4 — NTLMv2 authentication.
    #[test]
    fn ms_nlmp_ntlmv2_vectors() {
        let id = Identity { user: String::from("User"), domain: String::from("Domain"), nt_hash: nt_hash("Password") };
        assert_eq!(id.nt_hash.to_vec(), hex("a4f49c406510bdcab6824ee7c30fd852"));
        assert_eq!(ntowf_v2(&id.nt_hash, "User", "Domain").to_vec(), hex("0c868a403bfd7a93a3001ef22ef02e3f"));
        let av = hex("02000c0044006f006d00610069006e0001000c005300650072007600650072000000 0000");
        let server: [u8; 8] = hex("0123456789abcdef").try_into().unwrap();
        let v2 = ntlmv2(&id, &server, &[0xaa; 8], 0, &av);
        assert_eq!(v2.nt_response[..16].to_vec(), hex("68cd0ab851e51c96aabc927bebef6a1c"));
        assert_eq!(v2.lm_response.to_vec(), hex("86c35097ac9cec102554764a57cccc19aaaaaaaaaaaaaaaa"));
        assert_eq!(v2.session_base_key.to_vec(), hex("8de40ccadbc14a82f15cb0ad0de95ca3"));
        let mut encrypted = [0x55u8; 16];
        rc4(&v2.session_base_key, &mut encrypted);
        assert_eq!(encrypted.to_vec(), hex("c5dad2544fc9799094ce1ce90bc9d03e"));
    }

    #[test]
    fn splits_user_names() {
        assert_eq!(split_user("HOME\\roman"), (String::from("roman"), String::from("HOME")));
        assert_eq!(split_user("roman@home.lan"), (String::from("roman"), String::from("home.lan")));
        assert_eq!(split_user(" roman "), (String::from("roman"), String::new()));
    }

    fn challenge_msg(timestamp: bool) -> Vec<u8> {
        let mut info = hex("02000c0044006f006d00610069006e0001000c00530065007200760065007200");
        if timestamp {
            info.extend_from_slice(&[7, 0, 8, 0, 1, 2, 3, 4, 5, 6, 7, 8]);
        }
        info.extend_from_slice(&[0, 0, 0, 0]);
        let mut w = Writer::new();
        w.bytes(SIGNATURE).u32(2).u16(0).u16(0).u32(56).u32(CLIENT_FLAGS);
        w.bytes(&hex("0123456789abcdef")).zeros(8);
        w.u16(info.len() as u16).u16(info.len() as u16).u32(56);
        w.bytes(&VERSION);
        w.bytes(&info);
        w.buf
    }

    #[test]
    fn authenticate_layout_and_mic() {
        let neg = negotiate(false);
        let chal = challenge_msg(true);
        let id = Identity { user: String::from("User"), domain: String::from("Domain"), nt_hash: nt_hash("Password") };
        let random = [0x11u8; 24];
        let (msg, exported, _) = authenticate(&neg, &chal, Some(&id), "FOSSAI", "cifs/server", 0, &random).unwrap();
        assert_eq!(&msg[..8], SIGNATURE);
        assert_eq!(u32_at(&msg, 8), Some(3));
        // Key exchange: the exported key is the random one.
        assert_eq!(exported, [0x11; 16]);
        // LM response is zeros when the server sent a timestamp.
        let lm_len = u16_at(&msg, 12).unwrap() as usize;
        let lm_off = u32_at(&msg, 16).unwrap() as usize;
        assert_eq!(&msg[lm_off..lm_off + lm_len], &[0u8; 24]);
        // The MIC verifies over the three messages with the MIC zeroed.
        let mut zeroed = msg.clone();
        zeroed[72..88].fill(0);
        assert_eq!(&msg[72..88], &hmac_md5(&exported, &[&neg, &chal, &zeroed]));
        // The NT response embeds the server timestamp.
        let nt_off = u32_at(&msg, 24).unwrap() as usize;
        assert_eq!(&msg[nt_off + 24..nt_off + 32], &[1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn anonymous_authenticate() {
        let neg = negotiate(true);
        let chal = challenge_msg(false);
        let (msg, key, _) = authenticate(&neg, &chal, None, "FOSSAI", "cifs/server", 0, &[0; 24]).unwrap();
        assert_eq!(key, [0; 16]);
        assert_eq!(u16_at(&msg, 12), Some(1));
        assert_eq!(u16_at(&msg, 20), Some(0));
        assert_ne!(u32_at(&msg, 60).unwrap() & NEGOTIATE_ANONYMOUS, 0);
    }
}
