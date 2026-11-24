//! The one system clipboard (24.11.2026): text and a list of files.
//!
//! Before this the desktop had a bare `String` (terminal, Notepad), the file
//! manager kept copied files in its own process (Ctrl+C in one window, Ctrl+V
//! in another said "the clipboard is empty"), and Wayland clients had their
//! own selection that native programs never saw (Roman: text copied in
//! Notepad++ under Proton did not paste into our terminal).
//!
//! Now there is one clipboard with two formats:
//! * **text** - UTF-8;
//! * **files** - absolute paths plus "cut" (Ctrl+X: the paste moves them).
//!   Setting files also sets the text to the paths, one per line (pasting a
//!   copied file into Notepad gives its path, as before).
//!
//! Native programs reach it through `SYS_CLIPBOARD`; the kernel desktop and
//! terminal call it directly. The Wayland server ([`crate::linux::wayland`])
//! bridges both ways: a native change is offered to Wayland clients as
//! `text/plain;charset=utf-8` / `UTF8_STRING` / `TEXT` / `STRING` and, with
//! files, `text/uri-list`; a Wayland client's selection is read right when it
//! is set (the server asks the client to write it into a kernel pipe) and
//! lands here through [`set_from_wayland`], which does not re-announce it.

// The Wayland half exists only where the Linux layer does (x86-64).
#![cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::sync::SpinLock;

/// What is on the clipboard.
struct Clip {
    text: String,
    files: Vec<String>,
    cut: bool,
}

static CLIP: SpinLock<Clip> = SpinLock::new(Clip { text: String::new(), files: Vec::new(), cut: false });

/// Grows on every change (native or from Wayland).
static SEQ: AtomicU32 = AtomicU32::new(1);

/// Change number: grows with every change.
#[must_use]
pub fn seq() -> u32 {
    SEQ.load(Ordering::Acquire)
}

/// The text (a copy).
#[must_use]
pub fn text() -> String {
    CLIP.lock().text.clone()
}

/// Copied (or cut) files: paths and "cut". `None` - no files on the clipboard.
#[must_use]
pub fn files() -> Option<(Vec<String>, bool)> {
    let clip = CLIP.lock();
    (!clip.files.is_empty()).then(|| (clip.files.clone(), clip.cut))
}

/// Put text on the clipboard (a native program, the terminal): the files
/// format goes away, Wayland clients are offered the text.
pub fn set_text(text: String) {
    {
        let mut clip = CLIP.lock();
        clip.text = text;
        clip.files.clear();
        clip.cut = false;
    }
    SEQ.fetch_add(1, Ordering::AcqRel);
    announce();
}

/// Put files on the clipboard (Files, the desktop): Ctrl+C - `cut = false`,
/// Ctrl+X - `cut = true`. The text becomes the paths, one per line.
pub fn set_files(paths: Vec<String>, cut: bool) {
    {
        let mut clip = CLIP.lock();
        clip.text = paths.join("\n");
        clip.files = paths;
        clip.cut = cut;
    }
    SEQ.fetch_add(1, Ordering::AcqRel);
    announce();
}

/// Tell the Wayland server (x86-64 only: the Linux layer lives there).
fn announce() {
    #[cfg(target_arch = "x86_64")]
    crate::linux::wayland::native_clipboard_changed();
}

/// A Wayland client's selection, read by the server: text and/or files
/// (`text/uri-list`). Not announced back to Wayland - the client still owns
/// its selection there.
pub fn set_from_wayland(text: Option<String>, files: Option<Vec<String>>) {
    let mut clip = CLIP.lock();
    if let Some(files) = files.filter(|files| !files.is_empty()) {
        if text.is_none() {
            clip.text = files.join("\n");
        }
        clip.files = files;
        clip.cut = false;
    } else {
        clip.files.clear();
        clip.cut = false;
    }
    if let Some(text) = text {
        clip.text = text;
    }
    drop(clip);
    SEQ.fetch_add(1, Ordering::AcqRel);
}

/// Bytes of the clipboard in a Wayland MIME type; `None` - not offered.
#[must_use]
pub fn bytes_for(mime: &str) -> Option<Vec<u8>> {
    let clip = CLIP.lock();
    match mime {
        "text/uri-list" if !clip.files.is_empty() => Some(uri_list(&clip.files).into_bytes()),
        "x-special/gnome-copied-files" if !clip.files.is_empty() => {
            let mut out = String::from(if clip.cut { "cut" } else { "copy" });
            for path in &clip.files {
                out.push('\n');
                out.push_str(&file_uri(path));
            }
            Some(out.into_bytes())
        }
        _ if is_text_mime(mime) => Some(clip.text.clone().into_bytes()),
        _ => None,
    }
}

/// The MIME types the native clipboard is offered as.
#[must_use]
pub fn offered_mimes() -> Vec<String> {
    let clip = CLIP.lock();
    let mut out = Vec::new();
    if !clip.files.is_empty() {
        out.push(String::from("text/uri-list"));
        out.push(String::from("x-special/gnome-copied-files"));
    }
    if !clip.text.is_empty() || !clip.files.is_empty() {
        for mime in TEXT_MIMES {
            out.push(String::from(*mime));
        }
    }
    out
}

/// Text types, best first.
pub const TEXT_MIMES: &[&str] = &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"];

/// Is `mime` one of the text types.
#[must_use]
pub fn is_text_mime(mime: &str) -> bool {
    TEXT_MIMES.iter().any(|known| known.eq_ignore_ascii_case(mime)) || mime.eq_ignore_ascii_case("text/plain;charset=UTF-8")
}

/// `file:///path` with the bytes a URI cannot carry percent-encoded.
#[must_use]
pub fn file_uri(path: &str) -> String {
    let mut out = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&alloc::format!("%{byte:02X}"));
        }
    }
    out
}

/// `text/uri-list`: one URI per line, CRLF (RFC 2483).
#[must_use]
pub fn uri_list(paths: &[String]) -> String {
    let mut out = String::new();
    for path in paths {
        out.push_str(&file_uri(path));
        out.push_str("\r\n");
    }
    out
}

/// Paths from a `text/uri-list` (comments and non-`file:` URIs skipped).
/// Wine's paths under the prefix (`/root/.wine/dosdevices/z:/x`) are real
/// paths of our file system too, so they pass as they are.
#[must_use]
pub fn parse_uri_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split(['\r', '\n']) {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line.strip_prefix("file://") else {
            continue;
        };
        // `file://host/path` - only the local host is ours.
        let path = match rest.find('/') {
            Some(0) => rest,
            Some(at) if &rest[..at] == "localhost" => &rest[at..],
            _ => continue,
        };
        let decoded = percent_decode(path);
        if !decoded.is_empty() {
            out.push(decoded);
        }
    }
    out
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(Some(hi)), Some(Some(lo))) = (bytes.get(at + 1).map(|b| hex(*b)), bytes.get(at + 2).map(|b| hex(*b))) {
                out.push((hi * 16 + lo) as u8);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Wire form of the files format for `SYS_CLIPBOARD` (`CLIP_SET_FILES` /
/// `CLIP_GET_FILES`): `c` or `m`, a line feed, the paths one per line.
#[must_use]
pub fn files_wire() -> String {
    let clip = CLIP.lock();
    if clip.files.is_empty() {
        return String::new();
    }
    let mut out = String::from(if clip.cut { "m" } else { "c" });
    for path in &clip.files {
        out.push('\n');
        out.push_str(path);
    }
    out
}

/// Parse [`files_wire`]; `None` - malformed.
#[must_use]
pub fn parse_files_wire(text: &str) -> Option<(Vec<String>, bool)> {
    let mut lines = text.split('\n');
    let cut = match lines.next()? {
        "m" => true,
        "c" => false,
        _ => return None,
    };
    let paths: Vec<String> = lines.filter(|line| line.starts_with('/')).map(String::from).collect();
    Some((paths, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uri_round_trip() {
        let paths = alloc::vec![String::from("/home/roman/Видео/клип 1.mp4"), String::from("/tmp/a%b")];
        let list = uri_list(&paths);
        assert!(list.starts_with("file:///home/roman/%D0%92"));
        assert_eq!(parse_uri_list(&list), paths);
        assert_eq!(parse_uri_list("# c\r\nfile://localhost/x/y\r\nhttp://e/z\n"), alloc::vec![String::from("/x/y")]);
    }

    #[test]
    fn wire() {
        assert_eq!(parse_files_wire("m\n/a\n/b"), Some((alloc::vec![String::from("/a"), String::from("/b")], true)));
        assert_eq!(parse_files_wire("x\n/a"), None);
    }
}
