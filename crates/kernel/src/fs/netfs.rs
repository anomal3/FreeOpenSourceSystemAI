//! `/net` — shared folders of other computers as ordinary files (Roman 24.11:
//! Win+R `\\192.168.88.246` must open the share list).
//!
//! # The tree
//!
//! ```text
//! /net                      computers found on the network (`net::lan`)
//! /net/<host>               its shared folders (srvsvc share list)
//! /net/<host>/<share>/...   files and folders over SMB2/3
//! /net/.login               credentials in (write), the last outcome out (read)
//! ```
//!
//! `\\host\share\dir` typed in the Run box or the Files address bar becomes
//! `/net/host/share/dir` (`sysconf::winpath::accept`), so Files, Notepad,
//! Linux programs and Wine (a drive letter on `/net/...`) see paths and
//! nothing else.
//!
//! # Why in the kernel
//!
//! The file system layer is in the kernel and programs reach every volume
//! through it; a user-space daemon would need a whole FUSE-like protocol
//! first. The protocol itself is the `smb` crate (host-tested against Samba
//! and Windows); this file is the glue: a transport over the kernel's own
//! TCP, a pool of logged-on connections per (user, host), credentials, and
//! the `Node`s.
//!
//! # Who logs on
//!
//! Connections belong to the user (uid) of the program asking. Credentials
//! given through `/net/.login` (the `netlogon` window) are kept in memory;
//! with "remember" they are saved to `/etc/netcreds` (root, 0600) as the NT
//! hash — never the password: the hash is all NTLM needs, and the password,
//! which a person may use elsewhere, is not written anywhere. Without
//! credentials a host is tried anonymously, then as guest; refusal is
//! `PermissionDenied`, which is the cue to ask for credentials.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use smb::client::{Auth, Client, FileId, FileInfo, Transport, Tree};
use smb::{Error as SmbError, ntlm, smb2};

use crate::net::ipv4::Ipv4;
use crate::net::{self, NetError, lan, stream};
use crate::sched::TaskId;
use crate::sync::{Mutex, SpinLock};
use crate::vfs::{DirEntry, FileSystem, Metadata, Node, NodeKind, VfsError, VfsResult};

/// Where it is mounted.
pub const MOUNT: &str = "/net";
const SMB_PORT: u16 = 445;
/// Name of the credentials file in `/etc`.
const CREDS_FILE: &str = "netcreds";
/// How long a directory listing answers lookups without asking again.
const LIST_CACHE_MS: u64 = 3_000;
const SHARES_CACHE_MS: u64 = 10_000;
const CONNECT_TIMEOUT_MS: u64 = 5_000;
const IO_TIMEOUT_MS: u64 = 30_000;

/// Mount `/net`.
pub fn mount() {
    super::mount_volume_at(MOUNT, Arc::new(NetFs));
}

// ---------------------------------------------------------------------------
// Transport: the kernel's TCP
// ---------------------------------------------------------------------------

struct KernelTcp {
    owner: TaskId,
    index: usize,
}

/// Wait for `ready` (Some = done) with the I/O event generation, until
/// `deadline`; a program being killed stops the wait.
fn wait_for<T>(deadline: u64, mut ready: impl FnMut() -> Option<Result<T, SmbError>>) -> Result<T, SmbError> {
    loop {
        let seen = crate::io_events::generation();
        if let Some(done) = ready() {
            return done;
        }
        let now = crate::time::uptime_ms();
        if now >= deadline {
            return Err(SmbError::Timeout);
        }
        if crate::user::kill_pending() {
            return Err(SmbError::Io);
        }
        crate::io_events::wait(seen, Some(deadline.min(now + 250)));
    }
}

impl KernelTcp {
    fn connect(address: Ipv4, port: u16) -> Result<Self, SmbError> {
        let owner = lan::owner().unwrap_or_else(crate::sched::current);
        let index = net::stream_open(owner).map_err(|_| SmbError::Io)?;
        let tcp = Self { owner, index };
        net::stream_connect(owner, index, address, port).map_err(|_| SmbError::Io)?;
        wait_for(crate::time::uptime_ms() + CONNECT_TIMEOUT_MS, || match net::stream_info(owner, index) {
            Ok((_, _, _, _, _, true)) => Some(Err(SmbError::Io)),
            Ok((_, _, _, state, _, _)) if state.is_open() => Some(Ok(())),
            Ok((_, _, _, stream::State::Closed, _, _)) => Some(Err(SmbError::Io)),
            Ok(_) => None,
            Err(_) => Some(Err(SmbError::Io)),
        })?;
        Ok(tcp)
    }
}

impl Drop for KernelTcp {
    fn drop(&mut self) {
        let _ = net::stream_close(self.owner, self.index);
    }
}

impl Transport for KernelTcp {
    fn send(&mut self, data: &[u8]) -> Result<(), SmbError> {
        let mut sent = 0usize;
        let (owner, index) = (self.owner, self.index);
        wait_for(crate::time::uptime_ms() + IO_TIMEOUT_MS, || loop {
            if sent == data.len() {
                return Some(Ok(()));
            }
            match net::stream_send(owner, index, &data[sent..]) {
                Ok(0) => return None,
                Ok(taken) => sent += taken,
                Err(NetError::Stream(stream::StreamError::WouldBlock)) => return None,
                Err(_) => return Some(Err(SmbError::Closed)),
            }
        })
    }

    fn recv_exact(&mut self, buf: &mut [u8]) -> Result<(), SmbError> {
        let mut got = 0usize;
        let (owner, index) = (self.owner, self.index);
        wait_for(crate::time::uptime_ms() + IO_TIMEOUT_MS, || loop {
            if got == buf.len() {
                return Some(Ok(()));
            }
            match net::stream_recv(owner, index, &mut buf[got..]) {
                Ok(0) => {
                    return match net::stream_info(owner, index) {
                        Ok((_, _, _, _, peer_closed, reset)) if peer_closed || reset => Some(Err(SmbError::Closed)),
                        Ok(_) => None,
                        Err(_) => Some(Err(SmbError::Closed)),
                    };
                }
                Ok(read) => got += read,
                Err(_) => return Some(Err(SmbError::Closed)),
            }
        })
    }

    fn random(&mut self, out: &mut [u8]) {
        crate::random::fill(out);
    }

    fn filetime(&self) -> u64 {
        crate::time::now_unix().map_or(0, smb::wire::unix_to_filetime)
    }
}

// ---------------------------------------------------------------------------
// Credentials
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Cred {
    uid: u32,
    host: String,
    id: ntlm::Identity,
    saved: bool,
}

/// `None` — not read from `/etc/netcreds` yet.
static CREDS: Mutex<Option<Vec<Cred>>> = Mutex::new(None);

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

fn load_creds(slot: &mut Option<Vec<Cred>>) -> &mut Vec<Cred> {
    slot.get_or_insert_with(|| {
        let text = crate::config::text(CREDS_FILE, 64 * 1024);
        text.lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let uid = fields.next()?.parse().ok()?;
                let host = String::from(fields.next()?);
                let domain = String::from(fields.next()?);
                let user = String::from(fields.next()?);
                let nt_hash = unhex(fields.next()?)?;
                Some(Cred { uid, host, id: ntlm::Identity { user, domain, nt_hash }, saved: true })
            })
            .collect()
    })
}

fn save_creds(list: &[Cred]) {
    let mut text = String::from("# Saved network credentials (NT hashes) — written by the kernel, root only.\n");
    for cred in list.iter().filter(|cred| cred.saved) {
        text.push_str(&format!("{}\t{}\t{}\t{}\t{}\n", cred.uid, cred.host, cred.id.domain, cred.id.user, hex(&cred.id.nt_hash)));
    }
    if let Err(err) = crate::config::write_private(CREDS_FILE, &text) {
        crate::kprintln!("netfs: could not save credentials ({err})");
    }
}

fn creds_for(uid: u32, host: &str) -> Option<ntlm::Identity> {
    let mut slot = CREDS.lock();
    load_creds(&mut slot).iter().find(|cred| cred.uid == uid && cred.host == host).map(|cred| cred.id.clone())
}

fn remember(uid: u32, host: &str, id: ntlm::Identity, save: bool) {
    let mut slot = CREDS.lock();
    let list = load_creds(&mut slot);
    let had_saved = list.iter().any(|cred| cred.uid == uid && cred.host == host && cred.saved);
    list.retain(|cred| !(cred.uid == uid && cred.host == host));
    list.push(Cred { uid, host: String::from(host), id, saved: save });
    if save || had_saved {
        save_creds(list);
    }
}

fn forget(uid: u32, host: &str) {
    let mut slot = CREDS.lock();
    let list = load_creds(&mut slot);
    let had_saved = list.iter().any(|cred| cred.uid == uid && cred.host == host && cred.saved);
    list.retain(|cred| !(cred.uid == uid && cred.host == host));
    if had_saved {
        save_creds(list);
    }
}

// ---------------------------------------------------------------------------
// Connections
// ---------------------------------------------------------------------------

struct State {
    client: Option<Client<KernelTcp>>,
    trees: Vec<(String, Tree)>,
    /// Grows with every new connection: file handles of an older one are void.
    generation: u64,
    /// Visible disk shares and when they were read.
    shares: Option<(u64, Vec<smb::rpc::Share>)>,
}

struct Conn {
    uid: u32,
    /// Lower-case name or address as typed.
    host: String,
    state: Mutex<State>,
}

static POOL: Mutex<Vec<Arc<Conn>>> = Mutex::new(Vec::new());
static GENERATION: AtomicU64 = AtomicU64::new(1);
/// The last outcome per uid, for `/net/.login` reads and the logon window.
static LAST: SpinLock<Vec<(u32, String)>> = SpinLock::new(Vec::new());

fn set_last(uid: u32, text: String) {
    let mut last = LAST.lock();
    last.retain(|(who, _)| *who != uid);
    last.push((uid, text));
}

fn caller() -> u32 {
    crate::user::credentials().uid
}

fn key(host: &str) -> String {
    host.to_lowercase()
}

fn conn_for(uid: u32, host: &str) -> Arc<Conn> {
    let host = key(host);
    let mut pool = POOL.lock();
    if let Some(conn) = pool.iter().find(|conn| conn.uid == uid && conn.host == host) {
        return Arc::clone(conn);
    }
    let conn = Arc::new(Conn {
        uid,
        host,
        state: Mutex::new(State { client: None, trees: Vec::new(), generation: 0, shares: None }),
    });
    pool.push(Arc::clone(&conn));
    conn
}

fn map_error(err: SmbError) -> VfsError {
    use smb2::status::*;
    match err {
        SmbError::Status(code) if is_auth(code) => VfsError::PermissionDenied,
        SmbError::Status(OBJECT_NAME_NOT_FOUND | OBJECT_PATH_NOT_FOUND | NO_SUCH_FILE | BAD_NETWORK_NAME | NOT_FOUND | BAD_NETWORK_PATH) => VfsError::NotFound,
        SmbError::Status(OBJECT_NAME_COLLISION) => VfsError::Exists,
        SmbError::Status(DIRECTORY_NOT_EMPTY) => VfsError::NotEmpty,
        SmbError::Status(DISK_FULL) => VfsError::NoSpace,
        SmbError::Status(MEDIA_WRITE_PROTECTED) => VfsError::ReadOnly,
        SmbError::Status(FILE_IS_A_DIRECTORY | NOT_A_DIRECTORY) => VfsError::WrongKind,
        SmbError::Status(NOT_SUPPORTED | INVALID_PARAMETER | OBJECT_NAME_INVALID) | SmbError::Unsupported(_) => VfsError::Unsupported,
        SmbError::Status(SHARING_VIOLATION | DELETE_PENDING) => VfsError::PermissionDenied,
        _ => VfsError::Io,
    }
}

/// A broken connection worth one reconnect.
fn reconnectable(err: SmbError) -> bool {
    use smb2::status::*;
    matches!(err, SmbError::Io | SmbError::Closed | SmbError::Timeout | SmbError::Status(USER_SESSION_DELETED | NETWORK_NAME_DELETED | NETWORK_SESSION_EXPIRED))
}

/// `name:port` — a server on another port than 445 (a NAS behind port
/// forwarding, the test server of the `smb` scenario); just `name` is 445.
fn host_port(host: &str) -> (&str, u16) {
    match host.rsplit_once(':') {
        Some((name, port)) if !name.is_empty() => match port.parse() {
            Ok(port) => (name, port),
            Err(_) => (host, SMB_PORT),
        },
        _ => (host, SMB_PORT),
    }
}

/// Connect and log on to `host` as `auth`.
fn open_client(host: &str, auth: &Auth) -> Result<Client<KernelTcp>, SmbError> {
    let (name, port) = host_port(host);
    let address = lan::resolve(name, 3_000).ok_or(SmbError::Status(smb2::status::BAD_NETWORK_PATH))?;
    let tcp = KernelTcp::connect(address, port)?;
    let mut client = Client::connect(tcp, name)?;
    client.login(auth, lan::NAME)?;
    if port == SMB_PORT {
        lan::note_host(name, address, client.server_name.as_deref());
    }
    Ok(client)
}

/// Log on with what we know for (uid, host): saved credentials, else
/// anonymous, else guest.
fn login(uid: u32, host: &str) -> Result<Client<KernelTcp>, SmbError> {
    if let Some(id) = creds_for(uid, host) {
        let who = format!("{}\\{}", id.domain, id.user);
        return match open_client(host, &Auth::User(id)) {
            Ok(client) => {
                crate::kprintln!("netfs: {host}: logged on as {who} (SMB {})", smb2::dialect_name(client.dialect));
                Ok(client)
            }
            Err(err) => {
                crate::kprintln!("netfs: {host}: logon as {who} failed: {err}");
                Err(err)
            }
        };
    }
    let mut last = SmbError::Io;
    for auth in [Auth::Anonymous, Auth::Guest] {
        match open_client(host, &auth) {
            Ok(client) => {
                crate::kprintln!(
                    "netfs: {host}: {} session (SMB {})",
                    if matches!(auth, Auth::Anonymous) { "anonymous" } else { "guest" },
                    smb2::dialect_name(client.dialect)
                );
                return Ok(client);
            }
            Err(err @ (SmbError::Io | SmbError::Timeout | SmbError::Closed)) => {
                crate::kprintln!("netfs: {host}: no connection ({err})");
                return Err(err);
            }
            Err(SmbError::Status(code)) if code == smb2::status::BAD_NETWORK_PATH => return Err(SmbError::Status(code)),
            Err(err) => last = err,
        }
    }
    crate::kprintln!("netfs: {host}: anonymous and guest refused ({last}) — credentials needed");
    // Whatever the server said, the way on is a user name and a password.
    Err(SmbError::Status(smb2::status::ACCESS_DENIED))
}

/// Run `op` on the session of (caller, host), with the tree of `share`
/// connected if one is named. One reconnect on a broken connection.
fn with<R>(host: &str, share: Option<&str>, mut op: impl FnMut(&mut Client<KernelTcp>, Option<Tree>, u64) -> Result<R, SmbError>) -> VfsResult<R> {
    let uid = caller();
    let conn = conn_for(uid, host);
    let mut guard = conn.state.lock();
    let state = &mut *guard;
    for attempt in 0..2 {
        if state.client.is_none() {
            match login(conn.uid, &conn.host) {
                Ok(client) => {
                    state.client = Some(client);
                    state.trees.clear();
                    state.shares = None;
                    state.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
                }
                Err(err) => {
                    set_last(uid, format!("{err}"));
                    return Err(map_error(err));
                }
            }
        }
        let generation = state.generation;
        let Some(client) = state.client.as_mut() else { continue };
        let tree = match share {
            None => None,
            Some(name) => {
                let known = state.trees.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)).map(|(_, tree)| *tree);
                match known {
                    Some(tree) => Some(tree),
                    None => match client.tree_connect(name) {
                        Ok(tree) => {
                            state.trees.push((String::from(name), tree));
                            Some(tree)
                        }
                        Err(err) if attempt == 0 && reconnectable(err) => {
                            state.client = None;
                            continue;
                        }
                        Err(err) => {
                            set_last(uid, format!("{err}"));
                            return Err(map_error(err));
                        }
                    },
                }
            }
        };
        match op(client, tree, generation) {
            Ok(value) => return Ok(value),
            Err(err) if attempt == 0 && reconnectable(err) => {
                crate::kprintln!("netfs: {}: connection lost ({err}), reconnecting", conn.host);
                state.client = None;
            }
            Err(err) => return Err(map_error(err)),
        }
    }
    Err(VfsError::Io)
}

// ---------------------------------------------------------------------------
// Caches: directory listings answer lookups for a moment
// ---------------------------------------------------------------------------

struct Listed {
    uid: u32,
    host: String,
    share: String,
    path: String,
    at_ms: u64,
    entries: Vec<FileInfo>,
}

static LISTED: SpinLock<Vec<Listed>> = SpinLock::new(Vec::new());

fn cached_entry(host: &str, share: &str, dir: &str, name: &str) -> Option<FileInfo> {
    let uid = caller();
    let now = crate::time::uptime_ms();
    let host = key(host);
    let listed = LISTED.lock();
    let list = listed.iter().find(|l| l.uid == uid && l.host == host && l.share.eq_ignore_ascii_case(share) && l.path == dir && now.saturating_sub(l.at_ms) < LIST_CACHE_MS)?;
    list.entries.iter().find(|entry| entry.name.eq_ignore_ascii_case(name)).cloned()
}

fn remember_listing(host: &str, share: &str, dir: &str, entries: &[FileInfo]) {
    let uid = caller();
    let host = key(host);
    let now = crate::time::uptime_ms();
    let mut listed = LISTED.lock();
    listed.retain(|l| now.saturating_sub(l.at_ms) < LIST_CACHE_MS && !(l.uid == uid && l.host == host && l.share == share && l.path == dir));
    if listed.len() < 32 {
        listed.push(Listed { uid, host, share: String::from(share), path: String::from(dir), at_ms: now, entries: entries.to_vec() });
    }
}

/// Forget cached listings of `host/share` (after a change).
fn invalidate(host: &str, share: &str) {
    let host = key(host);
    LISTED.lock().retain(|l| !(l.host == host && l.share.eq_ignore_ascii_case(share)));
}

// ---------------------------------------------------------------------------
// The file system
// ---------------------------------------------------------------------------

struct NetFs;

fn dir_meta(uid: u32, mtime: u32) -> Metadata {
    Metadata { kind: NodeKind::Directory, size: 0, mode: 0o777, uid, gid: 0, mtime }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() { String::from(name) } else { format!("{dir}\\{name}") }
}

/// Split `/host/share/a/b` into (host, share, `a\b`).
fn split(path: &str) -> Option<(&str, &str, String)> {
    let mut parts = path.trim_matches('/').splitn(3, '/');
    let host = parts.next().filter(|p| !p.is_empty())?;
    let share = parts.next().filter(|p| !p.is_empty())?;
    let rest = parts.next().unwrap_or("").trim_matches('/').replace('/', "\\");
    Some((host, share, rest))
}

impl FileSystem for NetFs {
    fn name(&self) -> &'static str {
        "SMB"
    }

    fn root(&self) -> VfsResult<Box<dyn Node>> {
        Ok(Box::new(RootNode))
    }

    fn rename(&self, old: &str, new: &str) -> VfsResult<()> {
        let (host, share, from) = split(old).ok_or(VfsError::BadPath)?;
        let (host2, share2, to) = split(new).ok_or(VfsError::BadPath)?;
        if !host.eq_ignore_ascii_case(host2) || !share.eq_ignore_ascii_case(share2) || from.is_empty() || to.is_empty() {
            return Err(VfsError::Unsupported);
        }
        let result = with(host, Some(share), |client, tree, _| client.rename(tree.ok_or(SmbError::Io)?, &from, &to));
        invalidate(host, share);
        result
    }
}

/// `/net`.
struct RootNode;

impl Node for RootNode {
    fn metadata(&self) -> Metadata {
        dir_meta(0, 0)
    }

    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        Err(VfsError::WrongKind)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        lan::rescan();
        let uid = caller();
        let mut out: Vec<DirEntry> = Vec::new();
        for host in lan::hosts() {
            let name = if host.name.is_empty() || host.name.contains('/') { format!("{}", host.address) } else { host.name.clone() };
            if out.iter().any(|entry| entry.name.eq_ignore_ascii_case(&name)) {
                continue;
            }
            out.push(DirEntry { name, kind: NodeKind::Directory, size: 0, mode: 0o777, uid, gid: 0, mtime: 0 });
        }
        Ok(out)
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        if name == ".login" {
            return Ok(Box::new(LoginNode));
        }
        if name.starts_with('.') || name.is_empty() {
            return Err(VfsError::NotFound);
        }
        // An address or a name we can resolve — otherwise there is no such
        // computer.
        lan::resolve(host_port(name).0, 3_000).ok_or(VfsError::NotFound)?;
        Ok(Box::new(HostNode { host: String::from(name) }))
    }
}

/// `/net/<host>`.
struct HostNode {
    host: String,
}

impl Node for HostNode {
    fn metadata(&self) -> Metadata {
        dir_meta(caller(), 0)
    }

    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> VfsResult<usize> {
        Err(VfsError::WrongKind)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        let uid = caller();
        let conn = conn_for(uid, &self.host);
        let now = crate::time::uptime_ms();
        {
            let state = conn.state.lock();
            if let Some((at, shares)) = &state.shares {
                if now.saturating_sub(*at) < SHARES_CACHE_MS && state.client.is_some() {
                    return Ok(share_entries(uid, shares));
                }
            }
        }
        let shares = with(&self.host, None, |client, _, _| client.shares())?;
        let visible: Vec<smb::rpc::Share> = shares.into_iter().filter(smb::rpc::Share::is_visible_disk).collect();
        crate::kprintln!("netfs: {}: {} shared folder(s)", self.host, visible.len());
        let entries = share_entries(uid, &visible);
        conn.state.lock().shares = Some((now, visible));
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        if name.is_empty() || name.starts_with('.') {
            return Err(VfsError::NotFound);
        }
        // Tree connect now: a share that is not there is "not found", one we
        // may not open is "permission denied" — before anything is listed.
        with(&self.host, Some(name), |_, _, _| Ok(()))?;
        Ok(Box::new(SmbNode { host: self.host.clone(), share: String::from(name), path: String::new(), info: None, open: Mutex::new(None) }))
    }
}

fn share_entries(uid: u32, shares: &[smb::rpc::Share]) -> Vec<DirEntry> {
    shares.iter().map(|share| DirEntry { name: share.name.clone(), kind: NodeKind::Directory, size: 0, mode: 0o777, uid, gid: 0, mtime: 0 }).collect()
}

/// An open handle of a file node: id, generation of the connection, write.
type Handle = (FileId, u64, bool);

/// A file or folder inside a share. `info: None` — the share root.
struct SmbNode {
    host: String,
    share: String,
    path: String,
    info: Option<FileInfo>,
    open: Mutex<Option<Handle>>,
}

impl SmbNode {
    fn is_dir(&self) -> bool {
        self.info.as_ref().is_none_or(FileInfo::is_dir)
    }

    fn child(&self, info: FileInfo) -> SmbNode {
        SmbNode { host: self.host.clone(), share: self.share.clone(), path: join(&self.path, &info.name), info: Some(info), open: Mutex::new(None) }
    }

    /// Run `op` with an open handle (opened for writing if `write`).
    fn with_handle<R>(&self, write: bool, mut op: impl FnMut(&mut Client<KernelTcp>, Tree, FileId) -> Result<R, SmbError>) -> VfsResult<R> {
        let mut open = self.open.lock();
        let path = self.path.clone();
        with(&self.host, Some(&self.share), |client, tree, generation| {
            let tree = tree.ok_or(SmbError::Io)?;
            let current = match *open {
                Some((id, made, writable)) if made == generation && (writable || !write) => id,
                stale => {
                    if let Some((id, made, _)) = stale {
                        if made == generation {
                            let _ = client.close(tree, id);
                        }
                    }
                    *open = None;
                    let access = if write { smb2::ACCESS_WRITE } else { smb2::ACCESS_READ };
                    let opened = client.create(tree, &path, access, smb2::FILE_OPEN, smb2::FILE_NON_DIRECTORY_FILE, 0)?;
                    *open = Some((opened.id, generation, write));
                    opened.id
                }
            };
            op(client, tree, current)
        })
    }
}

impl Drop for SmbNode {
    fn drop(&mut self) {
        let taken = self.open.lock().take();
        if let Some((id, generation, _)) = taken {
            let _ = with(&self.host, Some(&self.share), |client, tree, current| {
                if current == generation {
                    client.close(tree.ok_or(SmbError::Io)?, id)?;
                }
                Ok(())
            });
        }
    }
}

impl Node for SmbNode {
    fn metadata(&self) -> Metadata {
        let uid = caller();
        match &self.info {
            Some(info) if !info.is_dir() => Metadata {
                kind: NodeKind::File,
                size: info.size,
                mode: if info.attributes & smb2::ATTR_READONLY != 0 { 0o555 } else { 0o777 },
                uid,
                gid: 0,
                mtime: info.mtime.min(u64::from(u32::MAX)) as u32,
            },
            Some(info) => dir_meta(uid, info.mtime.min(u64::from(u32::MAX)) as u32),
            None => dir_meta(uid, 0),
        }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        if self.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let mut done = 0usize;
        while done < buf.len() {
            let want = (buf.len() - done).min(smb::client::MAX_IO as usize) as u32;
            let at = offset + done as u64;
            let chunk = self.with_handle(false, |client, tree, id| client.read(tree, id, at, want).map(|(data, _)| data))?;
            if chunk.is_empty() {
                break;
            }
            let take = chunk.len().min(buf.len() - done);
            buf[done..done + take].copy_from_slice(&chunk[..take]);
            done += take;
            if chunk.len() < want as usize {
                break;
            }
        }
        Ok(done)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        if !self.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let path = self.path.clone();
        let entries = with(&self.host, Some(&self.share), |client, tree, _| {
            let tree = tree.ok_or(SmbError::Io)?;
            let dir = client.create(tree, &path, smb2::ACCESS_LIST, smb2::FILE_OPEN, smb2::FILE_DIRECTORY_FILE, 0)?;
            let listed = client.list(tree, dir.id);
            let _ = client.close(tree, dir.id);
            listed
        })?;
        remember_listing(&self.host, &self.share, &self.path, &entries);
        let uid = caller();
        Ok(entries
            .iter()
            .map(|info| DirEntry {
                name: info.name.clone(),
                kind: if info.is_dir() { NodeKind::Directory } else { NodeKind::File },
                size: if info.is_dir() { 0 } else { info.size },
                mode: if info.is_dir() { 0o777 } else if info.attributes & smb2::ATTR_READONLY != 0 { 0o555 } else { 0o777 },
                uid,
                gid: 0,
                mtime: info.mtime.min(u64::from(u32::MAX)) as u32,
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> VfsResult<Box<dyn Node>> {
        if !self.is_dir() {
            return Err(VfsError::WrongKind);
        }
        if name.is_empty() || name == "." || name == ".." || name.contains('\\') {
            return Err(VfsError::NotFound);
        }
        if let Some(info) = cached_entry(&self.host, &self.share, &self.path, name) {
            return Ok(Box::new(self.child(info)));
        }
        let path = join(&self.path, name);
        let mut info = with(&self.host, Some(&self.share), |client, tree, _| client.stat(tree.ok_or(SmbError::Io)?, &path))?;
        info.name = String::from(name);
        Ok(Box::new(self.child(info)))
    }

    fn write_at(&self, offset: u64, data: &[u8]) -> VfsResult<usize> {
        if self.is_dir() {
            return Err(VfsError::WrongKind);
        }
        let mut done = 0usize;
        while done < data.len() {
            let end = (done + smb::client::MAX_IO as usize).min(data.len());
            let at = offset + done as u64;
            let part = &data[done..end];
            let wrote = self.with_handle(true, |client, tree, id| client.write(tree, id, at, part))?;
            if wrote == 0 {
                break;
            }
            done += wrote;
        }
        invalidate(&self.host, &self.share);
        Ok(done)
    }

    fn truncate(&self, size: u64) -> VfsResult<()> {
        if self.is_dir() {
            return Err(VfsError::WrongKind);
        }
        self.with_handle(true, |client, tree, id| client.set_size(tree, id, size))?;
        invalidate(&self.host, &self.share);
        Ok(())
    }

    fn times(&self) -> crate::vfs::Times {
        let clip = |seconds: u64| seconds.min(u64::from(u32::MAX)) as u32;
        match &self.info {
            Some(info) => {
                let mtime = clip(info.mtime);
                let or = |value: u64| if value == 0 { mtime } else { clip(value) };
                crate::vfs::Times { mtime, atime: or(info.atime), ctime: or(info.btime) }
            }
            None => crate::vfs::Times::default(),
        }
    }

    fn set_times(&self, atime: Option<u32>, mtime: Option<u32>) -> VfsResult<()> {
        let (atime, mtime) = (atime.map(u64::from), mtime.map(u64::from));
        let result = if self.is_dir() {
            let path = self.path.clone();
            with(&self.host, Some(&self.share), |client, tree, _| {
                client.set_path_times(tree.ok_or(SmbError::Io)?, &path, true, atime, mtime)
            })
        } else {
            // Through the node's own handle: a write handle closed later must
            // not be the last word on the time (the server stops updating it
            // on a handle whose time was set explicitly).
            self.with_handle(true, |client, tree, id| client.set_times(tree, id, atime, mtime))
        };
        invalidate(&self.host, &self.share);
        result
    }

    fn create(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let path = join(&self.path, name);
        let opened = with(&self.host, Some(&self.share), |client, tree, generation| {
            let tree = tree.ok_or(SmbError::Io)?;
            let opened = client.create(tree, &path, smb2::ACCESS_WRITE, smb2::FILE_CREATE, smb2::FILE_NON_DIRECTORY_FILE, smb2::ATTR_NORMAL)?;
            Ok((opened, generation))
        })?;
        invalidate(&self.host, &self.share);
        let (opened, generation) = opened;
        let mut info = opened.info;
        info.name = String::from(name);
        let node = self.child(info);
        *node.open.lock() = Some((opened.id, generation, true));
        Ok(Box::new(node))
    }

    fn mkdir(&self, name: &str, _mode: u16, _uid: u32, _gid: u32) -> VfsResult<Box<dyn Node>> {
        let path = join(&self.path, name);
        let info = with(&self.host, Some(&self.share), |client, tree, _| {
            let tree = tree.ok_or(SmbError::Io)?;
            let opened = client.create(tree, &path, smb2::ACCESS_LIST, smb2::FILE_CREATE, smb2::FILE_DIRECTORY_FILE, 0)?;
            let _ = client.close(tree, opened.id);
            Ok(opened.info)
        })?;
        invalidate(&self.host, &self.share);
        let mut info = info;
        info.name = String::from(name);
        Ok(Box::new(self.child(info)))
    }

    fn unlink(&self, name: &str) -> VfsResult<()> {
        let path = join(&self.path, name);
        let result = with(&self.host, Some(&self.share), |client, tree, _| client.delete(tree.ok_or(SmbError::Io)?, &path, false));
        invalidate(&self.host, &self.share);
        result
    }

    fn rmdir(&self, name: &str) -> VfsResult<()> {
        let path = join(&self.path, name);
        let result = with(&self.host, Some(&self.share), |client, tree, _| client.delete(tree.ok_or(SmbError::Io)?, &path, true));
        invalidate(&self.host, &self.share);
        result
    }
}

// ---------------------------------------------------------------------------
// `/net/.login`
// ---------------------------------------------------------------------------

/// Credentials in: `host\nuser\npassword\nremember(0|1)\n` — the user may be
/// `DOMAIN\user` or `user@domain`; an empty user forgets the host's
/// credentials. The write succeeds only if the server accepted them (a new
/// session replaces the pooled one). Read: the last outcome for this user.
struct LoginNode;

impl Node for LoginNode {
    fn metadata(&self) -> Metadata {
        Metadata { kind: NodeKind::File, size: 0, mode: 0o666, uid: 0, gid: 0, mtime: 0 }
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> VfsResult<usize> {
        let uid = caller();
        let text = LAST.lock().iter().find(|(who, _)| *who == uid).map(|(_, text)| text.clone()).unwrap_or_default();
        let bytes = text.as_bytes();
        let start = (offset as usize).min(bytes.len());
        let take = (bytes.len() - start).min(buf.len());
        buf[..take].copy_from_slice(&bytes[start..start + take]);
        Ok(take)
    }

    fn list(&self) -> VfsResult<Vec<DirEntry>> {
        Err(VfsError::WrongKind)
    }

    fn lookup(&self, _name: &str) -> VfsResult<Box<dyn Node>> {
        Err(VfsError::WrongKind)
    }

    fn truncate(&self, _size: u64) -> VfsResult<()> {
        Ok(())
    }

    fn write_at(&self, _offset: u64, data: &[u8]) -> VfsResult<usize> {
        let uid = caller();
        let text = core::str::from_utf8(data).map_err(|_| VfsError::BadPath)?;
        let mut lines = text.split('\n');
        let host = lines.next().unwrap_or("").trim();
        let typed_user = lines.next().unwrap_or("").trim();
        let password = lines.next().unwrap_or("").trim_end_matches('\r');
        let save = lines.next().is_some_and(|line| line.trim() == "1");
        if host.is_empty() || host.contains(['/', '\\', '\t']) || typed_user.contains('\t') {
            return Err(VfsError::BadPath);
        }
        let host_key = key(host);
        let conn = conn_for(uid, &host_key);
        if typed_user.is_empty() {
            forget(uid, &host_key);
            let mut state = conn.state.lock();
            if let Some(mut client) = state.client.take() {
                client.logoff();
            }
            state.trees.clear();
            set_last(uid, String::from("forgotten"));
            return Ok(data.len());
        }
        let (user, domain) = ntlm::split_user(typed_user);
        let id = ntlm::Identity { user, domain, nt_hash: ntlm::nt_hash(password) };
        match open_client(&host_key, &Auth::User(id.clone())) {
            Ok(client) => {
                // A server that maps unknown users to guest "accepts" any
                // password: say so instead of pretending to be logged on.
                let guest = client.is_guest_or_null();
                crate::kprintln!(
                    "netfs: {host_key}: credentials of {}\\{} accepted{}{} (SMB {})",
                    id.domain,
                    id.user,
                    if guest { " as guest" } else { "" },
                    if save { ", remembered" } else { "" },
                    smb2::dialect_name(client.dialect)
                );
                remember(uid, &host_key, id, save);
                let mut state = conn.state.lock();
                if let Some(mut old) = state.client.replace(client) {
                    old.logoff();
                }
                state.trees.clear();
                state.shares = None;
                state.generation = GENERATION.fetch_add(1, Ordering::Relaxed);
                set_last(uid, String::from(if guest { "ok guest" } else { "ok" }));
                Ok(data.len())
            }
            Err(err) => {
                crate::kprintln!("netfs: {host_key}: credentials of {typed_user} refused: {err}");
                set_last(uid, format!("{err}"));
                Err(match map_error(err) {
                    VfsError::PermissionDenied => VfsError::PermissionDenied,
                    VfsError::NotFound => VfsError::NotFound,
                    VfsError::Unsupported => VfsError::Unsupported,
                    _ => VfsError::Io,
                })
            }
        }
    }
}
