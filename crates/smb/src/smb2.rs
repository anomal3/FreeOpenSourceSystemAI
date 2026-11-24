//! SMB2/3 wire constants and the header (MS-SMB2 2.2).

use crate::wire::{u16_at, u32_at, u64_at};

pub const HEADER: usize = 64;
pub const PROTOCOL_ID: [u8; 4] = [0xfe, b'S', b'M', b'B'];
/// SMB3 transform header (encrypted message) — recognised to say so.
pub const TRANSFORM_ID: [u8; 4] = [0xfd, b'S', b'M', b'B'];

// Commands.
pub const NEGOTIATE: u16 = 0x00;
pub const SESSION_SETUP: u16 = 0x01;
pub const LOGOFF: u16 = 0x02;
pub const TREE_CONNECT: u16 = 0x03;
pub const TREE_DISCONNECT: u16 = 0x04;
pub const CREATE: u16 = 0x05;
pub const CLOSE: u16 = 0x06;
pub const READ: u16 = 0x08;
pub const WRITE: u16 = 0x09;
pub const ECHO: u16 = 0x0d;
pub const QUERY_DIRECTORY: u16 = 0x0e;
pub const QUERY_INFO: u16 = 0x10;
pub const SET_INFO: u16 = 0x11;

// Header flags.
pub const FLAG_RESPONSE: u32 = 0x0000_0001;
pub const FLAG_ASYNC: u32 = 0x0000_0002;
pub const FLAG_SIGNED: u32 = 0x0000_0008;

// Dialects.
pub const DIALECT_202: u16 = 0x0202;
pub const DIALECT_210: u16 = 0x0210;
pub const DIALECT_300: u16 = 0x0300;
pub const DIALECT_302: u16 = 0x0302;
pub const DIALECT_311: u16 = 0x0311;
pub const DIALECTS: [u16; 5] = [DIALECT_202, DIALECT_210, DIALECT_300, DIALECT_302, DIALECT_311];

// Security mode.
pub const SIGNING_ENABLED: u16 = 0x0001;
pub const SIGNING_REQUIRED: u16 = 0x0002;

// Session flags.
pub const SESSION_IS_GUEST: u16 = 0x0001;
pub const SESSION_IS_NULL: u16 = 0x0002;
pub const SESSION_ENCRYPT_DATA: u16 = 0x0004;

// Share.
pub const SHARE_TYPE_DISK: u8 = 0x01;
pub const SHARE_TYPE_PIPE: u8 = 0x02;
pub const SHAREFLAG_ENCRYPT_DATA: u32 = 0x0000_8000;

// Access masks.
pub const FILE_READ_DATA: u32 = 0x0000_0001;
pub const FILE_LIST_DIRECTORY: u32 = 0x0000_0001;
pub const FILE_WRITE_DATA: u32 = 0x0000_0002;
pub const FILE_APPEND_DATA: u32 = 0x0000_0004;
pub const FILE_READ_EA: u32 = 0x0000_0008;
pub const FILE_WRITE_EA: u32 = 0x0000_0010;
pub const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
pub const FILE_WRITE_ATTRIBUTES: u32 = 0x0000_0100;
pub const DELETE: u32 = 0x0001_0000;
pub const READ_CONTROL: u32 = 0x0002_0000;
pub const SYNCHRONIZE: u32 = 0x0010_0000;

pub const ACCESS_READ: u32 = FILE_READ_DATA | FILE_READ_EA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE;
pub const ACCESS_WRITE: u32 =
    ACCESS_READ | FILE_WRITE_DATA | FILE_APPEND_DATA | FILE_WRITE_EA | FILE_WRITE_ATTRIBUTES;
pub const ACCESS_LIST: u32 = FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE;

// Share access.
pub const SHARE_ALL: u32 = 0x7;

// Create dispositions.
pub const FILE_OPEN: u32 = 1;
pub const FILE_CREATE: u32 = 2;
pub const FILE_OPEN_IF: u32 = 3;
pub const FILE_OVERWRITE_IF: u32 = 5;

// Create options.
pub const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
pub const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;
pub const FILE_DELETE_ON_CLOSE: u32 = 0x0000_1000;
pub const FILE_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

// File attributes.
pub const ATTR_READONLY: u32 = 0x0001;
pub const ATTR_HIDDEN: u32 = 0x0002;
pub const ATTR_DIRECTORY: u32 = 0x0010;
pub const ATTR_NORMAL: u32 = 0x0080;

// Information classes.
pub const INFO_FILE: u8 = 1;
pub const INFO_FILESYSTEM: u8 = 2;
pub const FILE_DIRECTORY_INFORMATION: u8 = 0x01;
pub const FILE_RENAME_INFORMATION: u8 = 10;
pub const FILE_DISPOSITION_INFORMATION: u8 = 13;
pub const FILE_END_OF_FILE_INFORMATION: u8 = 20;
pub const FS_SIZE_INFORMATION: u8 = 3;

/// NT status codes the client looks at.
pub mod status {
    pub const SUCCESS: u32 = 0;
    pub const PENDING: u32 = 0x0000_0103;
    pub const BUFFER_OVERFLOW: u32 = 0x8000_0005;
    pub const NO_MORE_FILES: u32 = 0x8000_0006;
    pub const INVALID_PARAMETER: u32 = 0xC000_000D;
    pub const NO_SUCH_FILE: u32 = 0xC000_000F;
    pub const END_OF_FILE: u32 = 0xC000_0011;
    pub const MORE_PROCESSING_REQUIRED: u32 = 0xC000_0016;
    pub const ACCESS_DENIED: u32 = 0xC000_0022;
    pub const OBJECT_NAME_INVALID: u32 = 0xC000_0033;
    pub const OBJECT_NAME_NOT_FOUND: u32 = 0xC000_0034;
    pub const OBJECT_NAME_COLLISION: u32 = 0xC000_0035;
    pub const OBJECT_PATH_NOT_FOUND: u32 = 0xC000_003A;
    pub const SHARING_VIOLATION: u32 = 0xC000_0043;
    pub const DELETE_PENDING: u32 = 0xC000_0056;
    pub const LOGON_FAILURE: u32 = 0xC000_006D;
    pub const ACCOUNT_RESTRICTION: u32 = 0xC000_006E;
    pub const PASSWORD_EXPIRED: u32 = 0xC000_0071;
    pub const ACCOUNT_DISABLED: u32 = 0xC000_0072;
    pub const DISK_FULL: u32 = 0xC000_007F;
    pub const MEDIA_WRITE_PROTECTED: u32 = 0xC000_00A2;
    pub const FILE_IS_A_DIRECTORY: u32 = 0xC000_00BA;
    pub const NOT_SUPPORTED: u32 = 0xC000_00BB;
    pub const BAD_NETWORK_PATH: u32 = 0xC000_00BE;
    pub const NETWORK_NAME_DELETED: u32 = 0xC000_00C9;
    pub const NETWORK_ACCESS_DENIED: u32 = 0xC000_00CA;
    pub const BAD_NETWORK_NAME: u32 = 0xC000_00CC;
    pub const DIRECTORY_NOT_EMPTY: u32 = 0xC000_0101;
    pub const NOT_A_DIRECTORY: u32 = 0xC000_0103;
    pub const LOGON_TYPE_NOT_GRANTED: u32 = 0xC000_015B;
    pub const USER_SESSION_DELETED: u32 = 0xC000_0203;
    pub const PASSWORD_MUST_CHANGE: u32 = 0xC000_0224;
    pub const NOT_FOUND: u32 = 0xC000_0225;
    pub const ACCOUNT_LOCKED_OUT: u32 = 0xC000_0234;
    pub const NETWORK_SESSION_EXPIRED: u32 = 0xC000_035C;

    /// Does the status mean "who you are is not good enough" — the cases in
    /// which asking the person for (other) credentials makes sense.
    #[must_use]
    pub const fn is_auth(status: u32) -> bool {
        matches!(
            status,
            ACCESS_DENIED
                | LOGON_FAILURE
                | ACCOUNT_RESTRICTION
                | PASSWORD_EXPIRED
                | ACCOUNT_DISABLED
                | NETWORK_ACCESS_DENIED
                | LOGON_TYPE_NOT_GRANTED
                | PASSWORD_MUST_CHANGE
                | ACCOUNT_LOCKED_OUT
        )
    }

    /// Short English name for logs.
    #[must_use]
    pub const fn name(status: u32) -> &'static str {
        match status {
            SUCCESS => "SUCCESS",
            ACCESS_DENIED => "ACCESS_DENIED",
            LOGON_FAILURE => "LOGON_FAILURE",
            ACCOUNT_RESTRICTION => "ACCOUNT_RESTRICTION",
            ACCOUNT_DISABLED => "ACCOUNT_DISABLED",
            ACCOUNT_LOCKED_OUT => "ACCOUNT_LOCKED_OUT",
            PASSWORD_EXPIRED => "PASSWORD_EXPIRED",
            BAD_NETWORK_NAME => "BAD_NETWORK_NAME",
            OBJECT_NAME_NOT_FOUND => "OBJECT_NAME_NOT_FOUND",
            OBJECT_PATH_NOT_FOUND => "OBJECT_PATH_NOT_FOUND",
            OBJECT_NAME_COLLISION => "OBJECT_NAME_COLLISION",
            NOT_SUPPORTED => "NOT_SUPPORTED",
            INVALID_PARAMETER => "INVALID_PARAMETER",
            SHARING_VIOLATION => "SHARING_VIOLATION",
            USER_SESSION_DELETED => "USER_SESSION_DELETED",
            NETWORK_NAME_DELETED => "NETWORK_NAME_DELETED",
            DIRECTORY_NOT_EMPTY => "DIRECTORY_NOT_EMPTY",
            _ => "NT status",
        }
    }
}

/// A parsed response header.
#[derive(Debug, Clone, Copy)]
pub struct Header {
    pub credit_charge: u16,
    pub status: u32,
    pub command: u16,
    pub credits: u16,
    pub flags: u32,
    pub next_command: u32,
    pub message_id: u64,
    pub tree_id: u32,
    pub session_id: u64,
}

impl Header {
    /// Parse the 64-byte header at the start of `msg`.
    #[must_use]
    pub fn parse(msg: &[u8]) -> Option<Self> {
        if msg.get(..4)? != PROTOCOL_ID || u16_at(msg, 4)? != 64 {
            return None;
        }
        let flags = u32_at(msg, 16)?;
        Some(Self {
            credit_charge: u16_at(msg, 6)?,
            status: u32_at(msg, 8)?,
            command: u16_at(msg, 12)?,
            credits: u16_at(msg, 14)?,
            flags,
            next_command: u32_at(msg, 20)?,
            message_id: u64_at(msg, 24)?,
            // Async responses carry an AsyncId where the tree id would be.
            tree_id: if flags & FLAG_ASYNC != 0 { 0 } else { u32_at(msg, 36)? },
            session_id: u64_at(msg, 40)?,
        })
    }
}

/// Write a request header into `out[..64]`.
#[allow(clippy::too_many_arguments)]
pub fn write_header(out: &mut [u8], command: u16, credit_charge: u16, credits: u16, flags: u32, message_id: u64, tree_id: u32, session_id: u64) {
    out[..64].fill(0);
    out[..4].copy_from_slice(&PROTOCOL_ID);
    out[4..6].copy_from_slice(&64u16.to_le_bytes());
    out[6..8].copy_from_slice(&credit_charge.to_le_bytes());
    out[12..14].copy_from_slice(&command.to_le_bytes());
    out[14..16].copy_from_slice(&credits.to_le_bytes());
    out[16..20].copy_from_slice(&flags.to_le_bytes());
    out[24..32].copy_from_slice(&message_id.to_le_bytes());
    out[32..36].copy_from_slice(&0xFEFFu32.to_le_bytes());
    out[36..40].copy_from_slice(&tree_id.to_le_bytes());
    out[40..48].copy_from_slice(&session_id.to_le_bytes());
}

/// Name of a dialect for logs.
#[must_use]
pub const fn dialect_name(dialect: u16) -> &'static str {
    match dialect {
        DIALECT_202 => "2.0.2",
        DIALECT_210 => "2.1",
        DIALECT_300 => "3.0",
        DIALECT_302 => "3.0.2",
        DIALECT_311 => "3.1.1",
        _ => "?",
    }
}
