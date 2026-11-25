//! SMB2/3 client and LAN discovery packets for `\\host\share` (Roman 24.11).
//!
//! # Why a separate crate
//!
//! The same reason as `wpa` and `ssh`: this is bytes and numbers, checked on
//! the host against the standards' test vectors (MS-NLMP, RFC 4493, FIPS-197)
//! and, by `tests/live.rs`, against a real server — not by booting the
//! system and seeing a share fail to open for any of a hundred reasons.
//!
//! # What is here
//!
//! * [`client`] — SMB 2.0.2 to 3.1.1: negotiate (3.1.1 pre-authentication
//!   integrity with SHA-512), session setup with NTLMv2 in SPNEGO (anonymous,
//!   guest or a user), signing (HMAC-SHA256 for 2.x, AES-CMAC for 3.x),
//!   tree connect, create/read/write/close, directory listing, rename,
//!   delete, volume size, and the share list via `srvsvc` over `IPC$`.
//! * [`names`] — NetBIOS name service, LLMNR and mDNS messages, the
//!   WS-Discovery probe: finding computers and resolving `\\NAME`.
//!
//! # What is not
//!
//! SMB3 encryption (a server or share that requires it gets a clear
//! "encryption required" error), Kerberos, DFS referrals, oplocks/leases,
//! multi-credit (> 64 KiB) transfers, SMB1.

#![no_std]

extern crate alloc;

pub mod client;
pub mod crypto;
pub mod names;
pub mod ntlm;
pub mod rpc;
pub mod smb2;
pub mod spnego;
pub mod wire;

pub use client::{Auth, Client, FileId, FileInfo, Opened, Transport, Tree};

/// Why an operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The transport failed (connection refused, reset, no route).
    Io,
    /// The server closed the connection.
    Closed,
    /// No answer in time.
    Timeout,
    /// The server sent something we do not understand.
    Malformed(&'static str),
    /// The server answered with this NT status.
    Status(u32),
    /// A feature the server insists on and we do not have.
    Unsupported(&'static str),
    /// A signed message from the server did not verify.
    Signature,
}

impl Error {
    /// Would other credentials help.
    #[must_use]
    pub const fn is_auth(&self) -> bool {
        matches!(self, Error::Status(code) if smb2::status::is_auth(*code))
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Io => f.write_str("connection failed"),
            Error::Closed => f.write_str("the server closed the connection"),
            Error::Timeout => f.write_str("no answer from the server"),
            Error::Malformed(what) => write!(f, "protocol error: {what}"),
            Error::Status(code) => write!(f, "{} (0x{code:08X})", smb2::status::name(*code)),
            Error::Unsupported(what) => f.write_str(what),
            Error::Signature => f.write_str("bad signature from the server"),
        }
    }
}
