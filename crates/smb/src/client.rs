//! The SMB2/3 client: one TCP connection, one session, requests one at a
//! time.
//!
//! Blocking and transport-agnostic: the kernel gives it a TCP stream of its
//! own stack, the host tests a `std::net::TcpStream`. Everything a request
//! needs (credits, message ids, signing, the 3.1.1 pre-authentication hash)
//! lives here.

use alloc::string::String;
use alloc::vec::Vec;

use crate::crypto::{aes_cmac, hmac_sha256, sha512_chain, smb3_kdf};
use crate::smb2::{self, Header, status};
use crate::wire::{Writer, filetime_to_unix, from_utf16, u16_at, u32_at, u64_at, utf16};
use crate::{Error, ntlm, rpc, spnego};

/// What the client needs from the outside world.
pub trait Transport {
    /// Send all of `data`.
    fn send(&mut self, data: &[u8]) -> Result<(), Error>;
    /// Fill all of `buf` (blocking, with the transport's own time limit).
    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), Error>;
    /// Cryptographically random bytes.
    fn random(&mut self, out: &mut [u8]);
    /// Current time as a Windows FILETIME (100 ns since 1601), 0 if unknown.
    fn filetime(&self) -> u64;
}

/// Largest message accepted from a server (a 64 KiB read plus headers).
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
/// Largest READ/WRITE/QUERY_DIRECTORY transfer requested at once — one
/// credit (without LARGE_MTU).
pub const MAX_IO: u32 = 65_536;

/// File handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId(pub [u8; 16]);

/// A connected share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tree {
    pub id: u32,
    pub share_type: u8,
    pub maximal_access: u32,
}

/// What a directory listing or an open tells about a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub name: String,
    pub size: u64,
    pub attributes: u32,
    /// Last write, Unix seconds (0 unknown).
    pub mtime: u64,
}

impl FileInfo {
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.attributes & smb2::ATTR_DIRECTORY != 0
    }
}

/// Result of an open.
#[derive(Debug, Clone)]
pub struct Opened {
    pub id: FileId,
    pub info: FileInfo,
}

/// Who to log on as.
#[derive(Debug, Clone)]
pub enum Auth {
    /// Null session — enough to list shares on servers that allow it.
    Anonymous,
    /// Account name with an empty password, for "guest ok" shares.
    Guest,
    User(ntlm::Identity),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signer {
    None,
    HmacSha256([u8; 16]),
    Cmac([u8; 16]),
}

/// The connection and its one session.
pub struct Client<T: Transport> {
    t: T,
    pub dialect: u16,
    server_security: u16,
    pub max_read: u32,
    pub max_write: u32,
    pub max_transact: u32,
    message_id: u64,
    credits: u32,
    session_id: u64,
    pub session_flags: u16,
    signer: Signer,
    sign_requests: bool,
    preauth: [u8; 64],
    client_guid: [u8; 16],
    /// The server's NetBIOS name from the NTLM challenge (for logs and `/net`).
    pub server_name: Option<String>,
    /// Name the server is known by (`\\host\` in tree paths).
    host: String,
    call_id: u32,
}

impl<T: Transport> Client<T> {
    /// NEGOTIATE over a fresh transport. `host` is the name the server was
    /// reached by (used in tree paths and the NTLM target name).
    pub fn connect(mut t: T, host: &str) -> Result<Self, Error> {
        let mut client_guid = [0u8; 16];
        t.random(&mut client_guid);
        let mut client = Self {
            t,
            dialect: 0,
            server_security: 0,
            max_read: MAX_IO,
            max_write: MAX_IO,
            max_transact: MAX_IO,
            message_id: 0,
            credits: 1,
            session_id: 0,
            session_flags: 0,
            signer: Signer::None,
            sign_requests: false,
            preauth: [0; 64],
            client_guid,
            server_name: None,
            host: String::from(host),
            call_id: 1,
        };
        client.negotiate()?;
        Ok(client)
    }

    /// The transport, for callers that keep per-connection state there.
    pub fn transport(&self) -> &T {
        &self.t
    }

    fn negotiate(&mut self) -> Result<(), Error> {
        let mut body = Writer::new();
        body.u16(36).u16(smb2::DIALECTS.len() as u16).u16(smb2::SIGNING_ENABLED).u16(0);
        body.u32(0); // capabilities
        body.bytes(&self.client_guid);
        let context_offset_at = body.len();
        body.u32(0).u16(2).u16(0);
        for dialect in smb2::DIALECTS {
            body.u16(dialect);
        }
        // Negotiate contexts (3.1.1), 8-aligned from the header start.
        while (smb2::HEADER + body.len()) % 8 != 0 {
            body.u8(0);
        }
        let offset = (smb2::HEADER + body.len()) as u32;
        body.put_u32(context_offset_at, offset);
        let mut salt = [0u8; 32];
        self.t.random(&mut salt);
        // PREAUTH_INTEGRITY_CAPABILITIES: SHA-512.
        body.u16(1).u16(38).u32(0).u16(1).u16(32).u16(1).bytes(&salt);
        while (smb2::HEADER + body.len()) % 8 != 0 {
            body.u8(0);
        }
        // SIGNING_CAPABILITIES: AES-CMAC only (we have no GMAC).
        body.u16(8).u16(4).u32(0).u16(1).u16(1);

        let request = self.build(smb2::NEGOTIATE, 0, &body.buf, false);
        self.preauth = sha512_chain(&[0; 64], &request);
        let (header, response) = self.exchange(request)?;
        if header.status != status::SUCCESS {
            return Err(Error::Status(header.status));
        }
        let b = &response[smb2::HEADER..];
        self.server_security = u16_at(b, 2).ok_or(Error::Malformed("short negotiate"))?;
        self.dialect = u16_at(b, 4).ok_or(Error::Malformed("short negotiate"))?;
        if !smb2::DIALECTS.contains(&self.dialect) {
            return Err(Error::Unsupported("server offered no SMB2 dialect we speak"));
        }
        self.max_transact = u32_at(b, 28).unwrap_or(MAX_IO).min(MAX_IO);
        self.max_read = u32_at(b, 32).unwrap_or(MAX_IO).min(MAX_IO);
        self.max_write = u32_at(b, 36).unwrap_or(MAX_IO).min(MAX_IO);
        if self.dialect == smb2::DIALECT_311 {
            self.preauth = sha512_chain(&self.preauth, &response);
        }
        Ok(())
    }

    /// Does the server insist on signing.
    #[must_use]
    pub fn server_requires_signing(&self) -> bool {
        self.server_security & smb2::SIGNING_REQUIRED != 0
    }

    /// SESSION_SETUP rounds with NTLMSSP in SPNEGO.
    pub fn login(&mut self, auth: &Auth, workstation: &str) -> Result<(), Error> {
        let (who, anonymous) = match auth {
            Auth::Anonymous => (None, true),
            Auth::Guest => (Some(ntlm::Identity { user: String::from("Guest"), domain: String::new(), nt_hash: ntlm::nt_hash("") }), false),
            Auth::User(id) => (Some(id.clone()), false),
        };
        let negotiate = ntlm::negotiate(anonymous);
        let mut session_hash = self.preauth;
        let first = self.session_setup_request(&spnego::init(&negotiate));
        if self.dialect == smb2::DIALECT_311 {
            session_hash = sha512_chain(&session_hash, &first);
        }
        let (header, response) = self.exchange(first)?;
        if header.status != status::MORE_PROCESSING_REQUIRED {
            return Err(if header.status == status::SUCCESS { Error::Malformed("session set up without a challenge") } else { Error::Status(header.status) });
        }
        self.session_id = header.session_id;
        if self.dialect == smb2::DIALECT_311 {
            session_hash = sha512_chain(&session_hash, &response);
        }
        let blob = security_buffer(&response, 4)?;
        let answer = spnego::parse(blob)?;
        let challenge = answer.token.ok_or(Error::Malformed("no NTLM challenge in SPNEGO"))?.to_vec();
        if let Ok(parsed) = ntlm::Challenge::parse(&challenge) {
            self.server_name = parsed.computer_name();
        }

        let mut random = [0u8; 24];
        self.t.random(&mut random);
        let mut spn = String::from("cifs/");
        spn.push_str(&self.host);
        let time = self.t.filetime();
        let (auth_msg, session_key, flags) = ntlm::authenticate(&negotiate, &challenge, who.as_ref(), workstation, &spn, time, &random)?;
        let mic = who.as_ref().map(|_| ntlm::client_mic(&session_key, flags, &spnego::mech_types()));
        let second = self.session_setup_request(&spnego::response(&auth_msg, mic.as_ref().map(<[u8; 16]>::as_slice)));
        if self.dialect == smb2::DIALECT_311 {
            session_hash = sha512_chain(&session_hash, &second);
        }
        let (header, response) = self.exchange(second)?;
        if header.status != status::SUCCESS {
            self.session_id = 0;
            return Err(Error::Status(header.status));
        }
        self.session_flags = u16_at(&response[smb2::HEADER..], 2).unwrap_or(0);
        if self.session_flags & smb2::SESSION_ENCRYPT_DATA != 0 {
            return Err(Error::Unsupported("the server requires SMB3 encryption"));
        }
        let null = anonymous || self.session_flags & (smb2::SESSION_IS_NULL | smb2::SESSION_IS_GUEST) != 0;
        self.signer = if null {
            Signer::None
        } else {
            match self.dialect {
                smb2::DIALECT_202 | smb2::DIALECT_210 => Signer::HmacSha256(session_key),
                smb2::DIALECT_311 => Signer::Cmac(smb3_kdf(&session_key, b"SMBSigningKey\0", &session_hash)),
                _ => Signer::Cmac(smb3_kdf(&session_key, b"SMB2AESCMAC\0", b"SmbSign\0")),
            }
        };
        // The final response of an authenticated session is signed: checking
        // it proves the key derivation before anything else depends on it.
        if header.flags & smb2::FLAG_SIGNED != 0 && self.signer != Signer::None && !self.verify(&response) {
            return Err(Error::Signature);
        }
        self.sign_requests = !null && (self.server_requires_signing() || self.dialect == smb2::DIALECT_311);
        Ok(())
    }

    fn session_setup_request(&mut self, token: &[u8]) -> Vec<u8> {
        let mut body = Writer::new();
        body.u16(25).u8(0).u8(smb2::SIGNING_ENABLED as u8).u32(0).u32(0);
        body.u16((smb2::HEADER + 24) as u16).u16(token.len() as u16).u64(0);
        body.bytes(token);
        self.build(smb2::SESSION_SETUP, 0, &body.buf, false)
    }

    /// Is the session guest or null.
    #[must_use]
    pub fn is_guest_or_null(&self) -> bool {
        self.session_flags & (smb2::SESSION_IS_GUEST | smb2::SESSION_IS_NULL) != 0 || self.signer == Signer::None
    }

    /// LOGOFF (best effort).
    pub fn logoff(&mut self) {
        let request = self.build(smb2::LOGOFF, 0, &[4, 0, 0, 0], self.sign_requests);
        let _ = self.exchange(request);
    }

    /// ECHO — is the connection alive.
    pub fn echo(&mut self) -> Result<(), Error> {
        let request = self.build(smb2::ECHO, 0, &[4, 0, 0, 0], false);
        let (header, _) = self.exchange(request)?;
        ok(header.status)
    }

    /// TREE_CONNECT to `\\host\share`.
    pub fn tree_connect(&mut self, share: &str) -> Result<Tree, Error> {
        let mut path = String::from("\\\\");
        path.push_str(&self.host);
        path.push('\\');
        path.push_str(share);
        let name = utf16(&path);
        let mut body = Writer::new();
        body.u16(9).u16(0).u16((smb2::HEADER + 8) as u16).u16(name.len() as u16).bytes(&name);
        let request = self.build(smb2::TREE_CONNECT, 0, &body.buf, self.sign_requests);
        let (header, response) = self.exchange(request)?;
        ok(header.status)?;
        let b = &response[smb2::HEADER..];
        let share_type = *b.get(2).ok_or(Error::Malformed("short tree connect"))?;
        let flags = u32_at(b, 4).unwrap_or(0);
        if flags & smb2::SHAREFLAG_ENCRYPT_DATA != 0 {
            let tree = Tree { id: header.tree_id, share_type, maximal_access: 0 };
            let _ = self.tree_disconnect(tree);
            return Err(Error::Unsupported("the share requires SMB3 encryption"));
        }
        Ok(Tree { id: header.tree_id, share_type, maximal_access: u32_at(b, 12).unwrap_or(0) })
    }

    pub fn tree_disconnect(&mut self, tree: Tree) -> Result<(), Error> {
        let request = self.build(smb2::TREE_DISCONNECT, tree.id, &[4, 0, 0, 0], self.sign_requests);
        let (header, _) = self.exchange(request)?;
        ok(header.status)
    }

    /// CREATE. `path` is relative to the share root, `\`-separated, empty
    /// for the root.
    pub fn create(&mut self, tree: Tree, path: &str, access: u32, disposition: u32, options: u32, attributes: u32) -> Result<Opened, Error> {
        let name = utf16(path);
        let mut body = Writer::new();
        body.u16(57).u8(0).u8(0).u32(2).u64(0).u64(0);
        body.u32(access).u32(attributes).u32(smb2::SHARE_ALL).u32(disposition).u32(options);
        body.u16((smb2::HEADER + 56) as u16).u16(name.len() as u16).u32(0).u32(0);
        if name.is_empty() {
            body.u8(0);
        } else {
            body.bytes(&name);
        }
        let request = self.build(smb2::CREATE, tree.id, &body.buf, self.sign_requests);
        let (header, response) = self.exchange(request)?;
        ok(header.status)?;
        let b = &response[smb2::HEADER..];
        let mut id = [0u8; 16];
        id.copy_from_slice(b.get(64..80).ok_or(Error::Malformed("short create"))?);
        let leaf = path.rsplit('\\').next().unwrap_or("");
        Ok(Opened {
            id: FileId(id),
            info: FileInfo {
                name: String::from(leaf),
                size: u64_at(b, 48).unwrap_or(0),
                attributes: u32_at(b, 56).unwrap_or(0),
                mtime: filetime_to_unix(u64_at(b, 24).unwrap_or(0)),
            },
        })
    }

    pub fn close(&mut self, tree: Tree, file: FileId) -> Result<(), Error> {
        let mut body = Writer::new();
        body.u16(24).u16(0).u32(0).bytes(&file.0);
        let request = self.build(smb2::CLOSE, tree.id, &body.buf, self.sign_requests);
        let (header, _) = self.exchange(request)?;
        ok(header.status)
    }

    /// Open and close at once: attributes of `path`.
    pub fn stat(&mut self, tree: Tree, path: &str) -> Result<FileInfo, Error> {
        let opened = self.create(tree, path, smb2::FILE_READ_ATTRIBUTES | smb2::SYNCHRONIZE, smb2::FILE_OPEN, 0, 0)?;
        let _ = self.close(tree, opened.id);
        Ok(opened.info)
    }

    /// READ up to `len` bytes (≤ [`Client::max_read`]). Empty at the end of
    /// the file. For pipes, `more` tells that the message continues.
    pub fn read(&mut self, tree: Tree, file: FileId, offset: u64, len: u32) -> Result<(Vec<u8>, bool), Error> {
        let len = len.min(self.max_read);
        let mut body = Writer::new();
        body.u16(49).u8(0x50).u8(0).u32(len).u64(offset).bytes(&file.0);
        body.u32(0).u32(0).u32(0).u16(0).u16(0).u8(0);
        let request = self.build(smb2::READ, tree.id, &body.buf, self.sign_requests);
        let (header, response) = self.exchange(request)?;
        let more = header.status == status::BUFFER_OVERFLOW;
        if header.status == status::END_OF_FILE {
            return Ok((Vec::new(), false));
        }
        if !more {
            ok(header.status)?;
        }
        let b = &response[smb2::HEADER..];
        let data_offset = usize::from(*b.get(2).ok_or(Error::Malformed("short read"))?);
        let data_len = u32_at(b, 4).unwrap_or(0) as usize;
        let data = response.get(data_offset..data_offset + data_len).ok_or(Error::Malformed("read data out of bounds"))?;
        Ok((data.to_vec(), more))
    }

    /// WRITE `data` (≤ [`Client::max_write`]); returns how much was written.
    pub fn write(&mut self, tree: Tree, file: FileId, offset: u64, data: &[u8]) -> Result<usize, Error> {
        let data = &data[..data.len().min(self.max_write as usize)];
        let mut body = Writer::new();
        body.u16(49).u16((smb2::HEADER + 48) as u16).u32(data.len() as u32).u64(offset).bytes(&file.0);
        body.u32(0).u32(0).u16(0).u16(0).u32(0);
        body.bytes(data);
        let request = self.build(smb2::WRITE, tree.id, &body.buf, self.sign_requests);
        let (header, response) = self.exchange(request)?;
        ok(header.status)?;
        Ok(u32_at(&response[smb2::HEADER..], 4).unwrap_or(0) as usize)
    }

    /// All entries of an open directory (without `.` and `..`).
    pub fn list(&mut self, tree: Tree, dir: FileId) -> Result<Vec<FileInfo>, Error> {
        let mut out = Vec::new();
        let mut first = true;
        loop {
            let pattern = utf16("*");
            let mut body = Writer::new();
            body.u16(33).u8(smb2::FILE_DIRECTORY_INFORMATION).u8(u8::from(first)).u32(0).bytes(&dir.0);
            body.u16((smb2::HEADER + 32) as u16).u16(pattern.len() as u16).u32(self.max_transact.min(MAX_IO));
            body.bytes(&pattern);
            first = false;
            let request = self.build(smb2::QUERY_DIRECTORY, tree.id, &body.buf, self.sign_requests);
            let (header, response) = self.exchange(request)?;
            if header.status == status::NO_MORE_FILES {
                return Ok(out);
            }
            ok(header.status)?;
            let b = &response[smb2::HEADER..];
            let offset = usize::from(u16_at(b, 2).unwrap_or(0));
            let len = u32_at(b, 4).unwrap_or(0) as usize;
            let data = response.get(offset..offset + len).ok_or(Error::Malformed("directory data out of bounds"))?;
            let before = out.len();
            parse_directory(data, &mut out)?;
            if out.len() == before && len == 0 {
                return Ok(out);
            }
            if out.len() > 100_000 {
                return Err(Error::Unsupported("directory too large"));
            }
        }
    }

    fn set_info(&mut self, tree: Tree, file: FileId, class: u8, data: &[u8]) -> Result<(), Error> {
        let mut body = Writer::new();
        body.u16(33).u8(smb2::INFO_FILE).u8(class).u32(data.len() as u32).u16((smb2::HEADER + 32) as u16).u16(0).u32(0);
        body.bytes(&file.0).bytes(data);
        let request = self.build(smb2::SET_INFO, tree.id, &body.buf, self.sign_requests);
        let (header, _) = self.exchange(request)?;
        ok(header.status)
    }

    /// Change the length of an open file.
    pub fn set_size(&mut self, tree: Tree, file: FileId, size: u64) -> Result<(), Error> {
        self.set_info(tree, file, smb2::FILE_END_OF_FILE_INFORMATION, &size.to_le_bytes())
    }

    /// Delete a file or an empty directory.
    pub fn delete(&mut self, tree: Tree, path: &str, directory: bool) -> Result<(), Error> {
        let options = if directory { smb2::FILE_DIRECTORY_FILE } else { smb2::FILE_NON_DIRECTORY_FILE };
        let opened = self.create(tree, path, smb2::DELETE | smb2::FILE_READ_ATTRIBUTES | smb2::SYNCHRONIZE, smb2::FILE_OPEN, options | smb2::FILE_OPEN_REPARSE_POINT, 0)?;
        let result = self.set_info(tree, opened.id, smb2::FILE_DISPOSITION_INFORMATION, &[1]);
        let closed = self.close(tree, opened.id);
        result.and(closed)
    }

    /// Rename within the share (`to` relative to the share root).
    pub fn rename(&mut self, tree: Tree, from: &str, to: &str) -> Result<(), Error> {
        let opened = self.create(tree, from, smb2::DELETE | smb2::FILE_READ_ATTRIBUTES | smb2::SYNCHRONIZE, smb2::FILE_OPEN, 0, 0)?;
        let name = utf16(to);
        let mut data = Writer::new();
        data.u8(0).zeros(7).u64(0).u32(name.len() as u32).bytes(&name);
        let result = self.set_info(tree, opened.id, smb2::FILE_RENAME_INFORMATION, &data.buf);
        let closed = self.close(tree, opened.id);
        result.and(closed)
    }

    /// Size of the volume behind the share: (total, free) bytes.
    pub fn space(&mut self, tree: Tree) -> Result<(u64, u64), Error> {
        let root = self.create(tree, "", smb2::FILE_READ_ATTRIBUTES | smb2::SYNCHRONIZE, smb2::FILE_OPEN, smb2::FILE_DIRECTORY_FILE, 0)?;
        let mut body = Writer::new();
        body.u16(41).u8(smb2::INFO_FILESYSTEM).u8(smb2::FS_SIZE_INFORMATION).u32(64).u16(0).u16(0).u32(0).u32(0).u32(0);
        body.bytes(&root.id.0).u8(0);
        let request = self.build(smb2::QUERY_INFO, tree.id, &body.buf, self.sign_requests);
        let answer = self.exchange(request);
        let _ = self.close(tree, root.id);
        let (header, response) = answer?;
        ok(header.status)?;
        let b = &response[smb2::HEADER..];
        let offset = usize::from(u16_at(b, 2).unwrap_or(0));
        let info = response.get(offset..offset + 24).ok_or(Error::Malformed("short fs info"))?;
        let total = u64_at(info, 0).unwrap_or(0);
        let free = u64_at(info, 8).unwrap_or(0);
        let unit = u64::from(u32_at(info, 16).unwrap_or(0)) * u64::from(u32_at(info, 20).unwrap_or(0));
        Ok((total * unit, free * unit))
    }

    /// The shares of the server (srvsvc NetrShareEnum over `IPC$`).
    pub fn shares(&mut self) -> Result<Vec<rpc::Share>, Error> {
        let ipc = self.tree_connect("IPC$")?;
        let result = self.shares_on(ipc);
        let _ = self.tree_disconnect(ipc);
        result
    }

    fn shares_on(&mut self, ipc: Tree) -> Result<Vec<rpc::Share>, Error> {
        let pipe = self.create(ipc, "srvsvc", smb2::ACCESS_READ | smb2::FILE_WRITE_DATA | smb2::FILE_APPEND_DATA, smb2::FILE_OPEN, 0, 0)?;
        let result = (|| {
            let call = self.next_call();
            let ack = self.transact(ipc, pipe.id, &rpc::bind(call))?;
            rpc::check_bind_ack(&ack)?;
            let call = self.next_call();
            let host = self.host.clone();
            let mut reply = self.transact(ipc, pipe.id, &rpc::share_enum_request(call, &host))?;
            loop {
                if let Some(stub) = rpc::response_stub(&reply)? {
                    return rpc::parse_share_enum(&stub);
                }
                // Fragments beyond the first message.
                let (more, _) = self.read(ipc, pipe.id, 0, MAX_IO)?;
                if more.is_empty() {
                    return Err(Error::Malformed("RPC reply cut short"));
                }
                reply.extend_from_slice(&more);
                if reply.len() > MAX_MESSAGE {
                    return Err(Error::Malformed("RPC reply too large"));
                }
            }
        })();
        let _ = self.close(ipc, pipe.id);
        result
    }

    fn next_call(&mut self) -> u32 {
        self.call_id += 1;
        self.call_id
    }

    /// Write one RPC message to a pipe and read the whole reply message.
    fn transact(&mut self, tree: Tree, pipe: FileId, data: &[u8]) -> Result<Vec<u8>, Error> {
        let written = self.write(tree, pipe, 0, data)?;
        if written != data.len() {
            return Err(Error::Malformed("short pipe write"));
        }
        let mut out = Vec::new();
        loop {
            let (chunk, more) = self.read(tree, pipe, 0, self.max_read.min(MAX_IO))?;
            out.extend_from_slice(&chunk);
            if !more || out.len() > MAX_MESSAGE {
                return Ok(out);
            }
        }
    }

    // -- framing, credits, signing ------------------------------------------

    /// A full request (NetBIOS session header included), signed if asked.
    fn build(&mut self, command: u16, tree_id: u32, body: &[u8], sign: bool) -> Vec<u8> {
        let mut msg = alloc::vec![0u8; smb2::HEADER + body.len()];
        let charge = if self.dialect >= smb2::DIALECT_210 { 1 } else { 0 };
        let want = if self.credits < 64 { 64 } else { 1 };
        let flags = if sign && self.signer != Signer::None { smb2::FLAG_SIGNED } else { 0 };
        let session = if command == smb2::NEGOTIATE { 0 } else { self.session_id };
        smb2::write_header(&mut msg, command, charge, want, flags, self.message_id, tree_id, session);
        msg[smb2::HEADER..].copy_from_slice(body);
        self.message_id += 1;
        self.credits = self.credits.saturating_sub(1);
        if flags & smb2::FLAG_SIGNED != 0 {
            let signature = self.signature(&msg);
            msg[48..64].copy_from_slice(&signature);
        }
        msg
    }

    fn signature(&self, msg: &[u8]) -> [u8; 16] {
        let mut copy = msg.to_vec();
        copy[48..64].fill(0);
        match self.signer {
            Signer::None => [0; 16],
            Signer::HmacSha256(key) => {
                let full = hmac_sha256(&key, &[&copy]);
                let mut out = [0u8; 16];
                out.copy_from_slice(&full[..16]);
                out
            }
            Signer::Cmac(key) => aes_cmac(&key, &copy),
        }
    }

    fn verify(&self, msg: &[u8]) -> bool {
        msg.len() >= smb2::HEADER && self.signature(msg) == msg[48..64]
    }

    /// Send a request and wait for its final response (interim `PENDING`
    /// answers and unrelated messages are skipped).
    fn exchange(&mut self, request: Vec<u8>) -> Result<(Header, Vec<u8>), Error> {
        let id = u64_at(&request, 24).unwrap_or(0);
        let mut framed = Vec::with_capacity(request.len() + 4);
        framed.extend_from_slice(&(request.len() as u32).to_be_bytes());
        framed.extend_from_slice(&request);
        self.t.send(&framed)?;
        loop {
            let response = self.receive()?;
            let header = Header::parse(&response).ok_or(Error::Malformed("not an SMB2 response"))?;
            self.credits = self.credits.saturating_add(u32::from(header.credits));
            if header.message_id != id {
                // An oplock break or a stale reply — not ours.
                continue;
            }
            if header.flags & smb2::FLAG_ASYNC != 0 && header.status == status::PENDING {
                continue;
            }
            return Ok((header, response));
        }
    }

    /// One message from the server (NetBIOS session framing stripped).
    fn receive(&mut self) -> Result<Vec<u8>, Error> {
        loop {
            let mut head = [0u8; 4];
            self.t.recv_exact(&mut head)?;
            let len = (usize::from(head[1]) << 16) | (usize::from(head[2]) << 8) | usize::from(head[3]);
            if head[0] == 0x85 && len == 0 {
                // NetBIOS keep-alive.
                continue;
            }
            if head[0] != 0 || len > MAX_MESSAGE {
                return Err(Error::Malformed("bad session message header"));
            }
            let mut msg = alloc::vec![0u8; len];
            self.t.recv_exact(&mut msg)?;
            if msg.get(..4) == Some(smb2::TRANSFORM_ID.as_slice()) {
                return Err(Error::Unsupported("the server encrypts the session (SMB3 encryption)"));
            }
            if msg.get(..4) == Some([0xff, b'S', b'M', b'B'].as_slice()) {
                return Err(Error::Unsupported("the server speaks only SMB1"));
            }
            return Ok(msg);
        }
    }
}

fn ok(code: u32) -> Result<(), Error> {
    if code == status::SUCCESS { Ok(()) } else { Err(Error::Status(code)) }
}

/// Security buffer of a response body: offset/length u16 pair at `at`.
fn security_buffer(response: &[u8], at: usize) -> Result<&[u8], Error> {
    let b = &response[smb2::HEADER..];
    let offset = usize::from(u16_at(b, at).ok_or(Error::Malformed("short session setup"))?);
    let len = usize::from(u16_at(b, at + 2).ok_or(Error::Malformed("short session setup"))?);
    response.get(offset..offset + len).ok_or(Error::Malformed("security buffer out of bounds"))
}

/// Parse FileDirectoryInformation entries.
pub fn parse_directory(data: &[u8], out: &mut Vec<FileInfo>) -> Result<(), Error> {
    let mut at = 0usize;
    loop {
        let entry = data.get(at..).ok_or(Error::Malformed("directory entry"))?;
        let next = u32_at(entry, 0).ok_or(Error::Malformed("directory entry"))? as usize;
        let name_len = u32_at(entry, 60).ok_or(Error::Malformed("directory entry"))? as usize;
        let name = from_utf16(entry.get(64..64 + name_len).ok_or(Error::Malformed("directory name"))?);
        if name != "." && name != ".." {
            out.push(FileInfo {
                name,
                size: u64_at(entry, 40).unwrap_or(0),
                attributes: u32_at(entry, 56).unwrap_or(0),
                mtime: filetime_to_unix(u64_at(entry, 24).unwrap_or(0)),
            });
        }
        if next == 0 {
            return Ok(());
        }
        at += next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directory_entries() {
        let mut data = Vec::new();
        for (index, (name, attr)) in [(".", 0x10u32), ("..", 0x10), ("Документ.txt", 0x20), ("Папка", 0x10)].iter().enumerate() {
            let mut w = Writer::new();
            let units = utf16(name);
            let len = (64 + units.len() + 7) & !7;
            w.u32(if index == 3 { 0 } else { len as u32 }).u32(0);
            w.u64(0).u64(0).u64(crate::wire::unix_to_filetime(1_700_000_000)).u64(0);
            w.u64(1234).u64(4096).u32(*attr).u32(units.len() as u32).bytes(&units);
            w.buf.resize(len, 0);
            data.extend_from_slice(&w.buf);
        }
        let mut out = Vec::new();
        parse_directory(&data, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Документ.txt");
        assert_eq!(out[0].size, 1234);
        assert_eq!(out[0].mtime, 1_700_000_000);
        assert!(!out[0].is_dir());
        assert!(out[1].is_dir());
    }

    #[test]
    fn smb3_signing_key_kdf() {
        // MS-SMB2 3.0 test: SessionKey 7CD451825D0450D235424E44BA6E78CC →
        // SigningKey 0B7E9C5CAC36C0F6EA9AB275298CEDCE.
        let key = [0x7C, 0xD4, 0x51, 0x82, 0x5D, 0x04, 0x50, 0xD2, 0x35, 0x42, 0x4E, 0x44, 0xBA, 0x6E, 0x78, 0xCC];
        let signing = smb3_kdf(&key, b"SMB2AESCMAC\0", b"SmbSign\0");
        assert_eq!(signing, [0x0B, 0x7E, 0x9C, 0x5C, 0xAC, 0x36, 0xC0, 0xF6, 0xEA, 0x9A, 0xB2, 0x75, 0x29, 0x8C, 0xED, 0xCE]);
    }
}
