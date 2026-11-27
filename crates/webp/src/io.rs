//! Minimal `std::io` replacement (byte-slice backed) plus a `byteorder-lite`
//! style `ReadBytesExt`, just enough for the ported decoder.

use alloc::vec::Vec;
use core::fmt;

/// Kind of an I/O error produced by the in-memory reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorKind {
    /// The data ended before the expected number of bytes could be read.
    UnexpectedEof,
    /// A seek to a negative or overflowing position was requested.
    InvalidInput,
}

/// I/O error of the in-memory reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
}

impl Error {
    pub(crate) const fn new(kind: ErrorKind) -> Self {
        Self { kind }
    }

    /// The kind of this error.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ErrorKind::UnexpectedEof => f.write_str("unexpected end of data"),
            ErrorKind::InvalidInput => f.write_str("invalid seek"),
        }
    }
}

impl core::error::Error for Error {}

pub(crate) type Result<T> = core::result::Result<T, Error>;

pub(crate) trait Read {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    fn read_exact(&mut self, mut buf: &mut [u8]) -> Result<()> {
        while !buf.is_empty() {
            let n = self.read(buf)?;
            if n == 0 {
                return Err(Error::new(ErrorKind::UnexpectedEof));
            }
            buf = &mut buf[n..];
        }
        Ok(())
    }

    fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<usize> {
        let start = out.len();
        let mut tmp = [0u8; 4096];
        loop {
            let n = self.read(&mut tmp)?;
            if n == 0 {
                return Ok(out.len() - start);
            }
            out.extend_from_slice(&tmp[..n]);
        }
    }

    fn take(self, limit: u64) -> Take<Self>
    where
        Self: Sized,
    {
        Take { inner: self, limit }
    }
}

pub(crate) trait BufRead: Read {
    fn fill_buf(&mut self) -> Result<&[u8]>;
    fn consume(&mut self, amt: usize);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekFrom {
    Start(u64),
    #[allow(dead_code)]
    End(i64),
    Current(i64),
}

pub(crate) trait Seek {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64>;

    fn stream_position(&mut self) -> Result<u64> {
        self.seek(SeekFrom::Current(0))
    }

    fn seek_relative(&mut self, offset: i64) -> Result<()> {
        self.seek(SeekFrom::Current(offset)).map(|_| ())
    }
}

impl<R: Read + ?Sized> Read for &mut R {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        (**self).read(buf)
    }
}

impl<R: BufRead + ?Sized> BufRead for &mut R {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        (**self).fill_buf()
    }
    fn consume(&mut self, amt: usize) {
        (**self).consume(amt)
    }
}

impl<S: Seek + ?Sized> Seek for &mut S {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        (**self).seek(pos)
    }
}

impl Read for &[u8] {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = buf.len().min(self.len());
        let (a, b) = self.split_at(n);
        buf[..n].copy_from_slice(a);
        *self = b;
        Ok(n)
    }
}

impl BufRead for &[u8] {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(*self)
    }
    fn consume(&mut self, amt: usize) {
        *self = &self[amt.min(self.len())..];
    }
}

/// Seekable reader over an in-memory buffer (like `std::io::Cursor`).
#[derive(Debug, Clone)]
pub(crate) struct Cursor<T> {
    inner: T,
    pos: u64,
}

impl<T> Cursor<T> {
    pub(crate) fn new(inner: T) -> Self {
        Self { inner, pos: 0 }
    }
}

impl<T: AsRef<[u8]>> Cursor<T> {
    fn remaining(&self) -> &[u8] {
        let data = self.inner.as_ref();
        let start = usize::try_from(self.pos).unwrap_or(usize::MAX).min(data.len());
        &data[start..]
    }
}

impl<T: AsRef<[u8]>> Read for Cursor<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let rem = self.remaining();
        let n = buf.len().min(rem.len());
        buf[..n].copy_from_slice(&rem[..n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl<T: AsRef<[u8]>> BufRead for Cursor<T> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        Ok(self.remaining())
    }
    fn consume(&mut self, amt: usize) {
        self.pos = self.pos.saturating_add(amt as u64);
    }
}

impl<T: AsRef<[u8]>> Seek for Cursor<T> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let (base, offset) = match pos {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => (self.inner.as_ref().len() as u64, n),
            SeekFrom::Current(n) => (self.pos, n),
        };
        match base.checked_add_signed(offset) {
            Some(n) => {
                self.pos = n;
                Ok(n)
            }
            None => Err(Error::new(ErrorKind::InvalidInput)),
        }
    }
}

/// Reader adapter limiting the number of bytes read (like `std::io::Take`).
#[derive(Debug)]
pub(crate) struct Take<R> {
    inner: R,
    limit: u64,
}

impl<R: Read> Read for Take<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if self.limit == 0 {
            return Ok(0);
        }
        let max = (buf.len() as u64).min(self.limit) as usize;
        let n = self.inner.read(&mut buf[..max])?;
        self.limit -= n as u64;
        Ok(n)
    }
}

impl<R: BufRead> BufRead for Take<R> {
    fn fill_buf(&mut self) -> Result<&[u8]> {
        if self.limit == 0 {
            return Ok(&[]);
        }
        let buf = self.inner.fill_buf()?;
        let cap = (buf.len() as u64).min(self.limit) as usize;
        Ok(&buf[..cap])
    }
    fn consume(&mut self, amt: usize) {
        let amt = (amt as u64).min(self.limit);
        self.limit -= amt;
        self.inner.consume(amt as usize);
    }
}

/// Byte order marker (only little endian is needed by WebP).
pub(crate) trait ByteOrder {
    fn u16(b: [u8; 2]) -> u16;
    fn u24(b: [u8; 3]) -> u32;
    fn u32(b: [u8; 4]) -> u32;
}

pub(crate) enum LittleEndian {}

impl ByteOrder for LittleEndian {
    fn u16(b: [u8; 2]) -> u16 {
        u16::from_le_bytes(b)
    }
    fn u24(b: [u8; 3]) -> u32 {
        u32::from_le_bytes([b[0], b[1], b[2], 0])
    }
    fn u32(b: [u8; 4]) -> u32 {
        u32::from_le_bytes(b)
    }
}

/// `byteorder-lite`-shaped helpers for any [`Read`].
pub(crate) trait ReadBytesExt: Read {
    fn read_u8(&mut self) -> Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(b[0])
    }
    fn read_u16<B: ByteOrder>(&mut self) -> Result<u16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(B::u16(b))
    }
    fn read_u24<B: ByteOrder>(&mut self) -> Result<u32> {
        let mut b = [0u8; 3];
        self.read_exact(&mut b)?;
        Ok(B::u24(b))
    }
    fn read_u32<B: ByteOrder>(&mut self) -> Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(B::u32(b))
    }
}

impl<R: Read + ?Sized> ReadBytesExt for R {}
