//! Проверки SFTP на хосте: разбор чужих пакетов без эмулятора и без сети.
//!
//! Главное здесь — не «правильный клиент получает правильный ответ» (это
//! проверяет стенд настоящим OpenSSH), а **неправильный**: обрезанный пакет,
//! длина строки больше самого пакета, чужой дескриптор, путь с нулём внутри.
//! Такие пакеты не пришлёт ни один клиент, которым можно проверить сервер в
//! эмуляторе, — а пришлёт их первый же, кто захочет сервер уронить.

use std::collections::BTreeMap;
use std::string::{String, ToString};
use std::vec::Vec;
use std::{format, vec};

use crate::sftp::*;
use crate::wire::{Reader, Writer};

/// Файловая система в памяти с запретами «как у ядра».
#[derive(Default)]
struct Memory {
    nodes: BTreeMap<String, (bool, Vec<u8>)>,
    /// Пути, которые нельзя прочитать (открыть на чтение).
    no_read: Vec<String>,
    /// Каталоги, в которых нельзя создавать, и файлы, которые нельзя писать.
    no_write: Vec<String>,
    open: Vec<Option<(String, usize)>>,
    log: Vec<String>,
}

impl Memory {
    fn new() -> Self {
        let mut fs = Self::default();
        fs.nodes.insert("/".into(), (true, Vec::new()));
        fs.nodes.insert("/home".into(), (true, Vec::new()));
        fs.nodes.insert("/home/roman".into(), (true, Vec::new()));
        fs.nodes.insert("/etc".into(), (true, Vec::new()));
        fs.nodes.insert("/etc/passwd".into(), (false, b"roman:1000".to_vec()));
        fs.no_write.push("/etc".into());
        fs.no_write.push("/etc/passwd".into());
        fs
    }

    fn parent(path: &str) -> String {
        match path.rfind('/') {
            Some(0) => "/".into(),
            Some(at) => path[..at].into(),
            None => "/".into(),
        }
    }

    fn fd(&mut self, path: &str) -> i64 {
        self.open.push(Some((path.into(), 0)));
        (self.open.len() - 1) as i64 + 3
    }

    fn path_of(&self, file: i64) -> Result<String, FsError> {
        let index = usize::try_from(file - 3).map_err(|_| FsError::Io)?;
        match self.open.get(index) {
            Some(Some((path, _))) => Ok(path.clone()),
            _ => Err(FsError::Io),
        }
    }

    fn meta(&self, path: &str) -> Result<Meta, FsError> {
        let (dir, data) = self.nodes.get(path).ok_or(FsError::NotFound)?;
        Ok(Meta {
            size: data.len() as u64,
            mode: if *dir { 0o755 } else { 0o644 },
            uid: 1000,
            gid: 1000,
            directory: *dir,
            mtime: None,
        })
    }
}

impl Fs for Memory {
    fn open_read(&mut self, path: &str) -> Result<i64, FsError> {
        if !self.nodes.contains_key(path) {
            return Err(FsError::NotFound);
        }
        if self.no_read.iter().any(|p| p == path) {
            return Err(FsError::Permission);
        }
        Ok(self.fd(path))
    }

    fn open_write(&mut self, path: &str, truncate: bool) -> Result<i64, FsError> {
        let Some((dir, data)) = self.nodes.get_mut(path) else {
            return Err(FsError::NotFound);
        };
        if *dir {
            return Err(FsError::WrongKind);
        }
        if self.no_write.iter().any(|p| p == path) {
            return Err(FsError::Permission);
        }
        if truncate {
            data.clear();
        }
        Ok(self.fd(path))
    }

    fn create(&mut self, path: &str, _mode: u32) -> Result<i64, FsError> {
        if self.nodes.contains_key(path) {
            return Err(FsError::Exists);
        }
        let parent = Self::parent(path);
        if !self.nodes.contains_key(&parent) {
            return Err(FsError::NotFound);
        }
        if self.no_write.iter().any(|p| *p == parent) {
            return Err(FsError::Permission);
        }
        self.nodes.insert(path.into(), (false, Vec::new()));
        Ok(self.fd(path))
    }

    fn read_at(&mut self, file: i64, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        let path = self.path_of(file)?;
        let (_, data) = &self.nodes[&path];
        let start = (offset as usize).min(data.len());
        let n = buffer.len().min(data.len() - start);
        buffer[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    fn write_at(&mut self, file: i64, offset: u64, bytes: &[u8]) -> Result<usize, FsError> {
        let path = self.path_of(file)?;
        let (_, data) = self.nodes.get_mut(&path).ok_or(FsError::Io)?;
        let end = offset as usize + bytes.len();
        if data.len() < end {
            data.resize(end, 0);
        }
        data[offset as usize..end].copy_from_slice(bytes);
        Ok(bytes.len())
    }

    fn size(&mut self, file: i64) -> Result<u64, FsError> {
        let path = self.path_of(file)?;
        Ok(self.nodes[&path].1.len() as u64)
    }

    fn fstat(&mut self, file: i64) -> Result<Meta, FsError> {
        let path = self.path_of(file)?;
        self.meta(&path)
    }

    fn stat(&mut self, path: &str) -> Result<Meta, FsError> {
        self.meta(path)
    }

    fn next_entry(
        &mut self,
        dir: i64,
        name: &mut [u8; MAX_NAME],
    ) -> Result<Option<(usize, Meta)>, FsError> {
        let path = self.path_of(dir)?;
        let prefix = if path == "/" { "/".to_string() } else { format!("{path}/") };
        let children: Vec<String> = self
            .nodes
            .keys()
            .filter(|k| k.starts_with(&prefix) && k.len() > prefix.len() && !k[prefix.len()..].contains('/'))
            .cloned()
            .collect();
        let index = (dir - 3) as usize;
        let Some(Some((_, position))) = self.open.get_mut(index) else {
            return Err(FsError::Io);
        };
        let Some(child) = children.get(*position) else {
            return Ok(None);
        };
        *position += 1;
        let short = &child[prefix.len()..];
        name[..short.len()].copy_from_slice(short.as_bytes());
        let meta = self.meta(child)?;
        Ok(Some((short.len(), meta)))
    }

    fn close(&mut self, file: i64) {
        if let Ok(index) = usize::try_from(file - 3) {
            if let Some(slot) = self.open.get_mut(index) {
                *slot = None;
            }
        }
    }

    fn mkdir(&mut self, path: &str, _mode: u32) -> Result<(), FsError> {
        if self.nodes.contains_key(path) {
            return Err(FsError::Exists);
        }
        if self.no_write.iter().any(|p| *p == Self::parent(path)) {
            return Err(FsError::Permission);
        }
        self.nodes.insert(path.into(), (true, Vec::new()));
        Ok(())
    }

    fn remove(&mut self, path: &str) -> Result<(), FsError> {
        let prefix = format!("{path}/");
        if self.nodes.keys().any(|k| k.starts_with(&prefix)) {
            return Err(FsError::NotEmpty);
        }
        self.nodes.remove(path).map(|_| ()).ok_or(FsError::NotFound)
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), FsError> {
        if self.nodes.contains_key(new) {
            return Err(FsError::Exists);
        }
        let node = self.nodes.remove(old).ok_or(FsError::NotFound)?;
        self.nodes.insert(new.into(), node);
        Ok(())
    }

    fn log(&mut self, parts: &[&str]) {
        self.log.push(parts.concat());
    }
}

fn server() -> Server<Memory> {
    let mut server = Server::new(Memory::new(), "/home/roman");
    let reply = call(&mut server, |w| {
        w.byte(FXP_INIT);
        w.u32(3);
    });
    assert_eq!(reply[0], FXP_VERSION);
    server
}

/// Собрать запрос, отдать серверу, вернуть тело ответа (без поля длины).
fn call(server: &mut Server<Memory>, build: impl FnOnce(&mut Writer<'_>)) -> Vec<u8> {
    let mut message = [0u8; 40_000];
    let len = {
        let mut w = Writer::new(&mut message);
        build(&mut w);
        assert!(w.ok());
        w.len()
    };
    raw(server, &message[..len])
}

fn raw(server: &mut Server<Memory>, message: &[u8]) -> Vec<u8> {
    let mut reply = vec![0u8; REPLY_BUFFER];
    let len = server.handle(message, &mut reply);
    assert!(len >= 5, "ответ есть всегда");
    let declared = u32::from_be_bytes([reply[0], reply[1], reply[2], reply[3]]) as usize;
    assert_eq!(declared + 4, len, "поле длины совпадает с ответом");
    reply[4..len].to_vec()
}

/// Код из ответа `STATUS`.
fn status_of(body: &[u8]) -> u32 {
    assert_eq!(body[0], FXP_STATUS, "ожидался STATUS");
    u32::from_be_bytes([body[5], body[6], body[7], body[8]])
}

fn open(server: &mut Server<Memory>, path: &str, flags: u32) -> Result<Vec<u8>, u32> {
    let body = call(server, |w| {
        w.byte(FXP_OPEN);
        w.u32(7);
        w.string(path.as_bytes());
        w.u32(flags);
        w.u32(0);
    });
    if body[0] == FXP_HANDLE {
        let mut r = Reader::new(&body[5..]);
        Ok(r.string().unwrap().to_vec())
    } else {
        Err(status_of(&body))
    }
}

#[test]
fn resolves_paths_against_home() {
    let cases = [
        (".", "/home/roman"),
        ("", "/home/roman"),
        ("apps/x.dll", "/home/roman/apps/x.dll"),
        ("..", "/home"),
        ("../../../..", "/"),
        ("/etc/./passwd", "/etc/passwd"),
        ("/a//b/../c/", "/a/c"),
        ("/", "/"),
    ];
    for (raw, want) in cases {
        let got = resolve("/home/roman", raw.as_bytes()).unwrap();
        assert_eq!(got.as_str(), want, "{raw}");
    }
    assert!(resolve("/home/roman", b"a\0b").is_none(), "ноль внутри пути");
    assert!(resolve("/home/roman", &[0xFF, 0xFE]).is_none(), "не UTF-8");
    let long = "x/".repeat(200);
    assert!(resolve("/home/roman", long.as_bytes()).is_none(), "длиннее ядра");
}

#[test]
fn frame_length_is_bounded() {
    assert_eq!(frame_len(&[0, 0, 0]), None);
    assert_eq!(frame_len(&[0, 0, 0, 0]), Some(Err(())));
    assert_eq!(frame_len(&[0xFF, 0xFF, 0xFF, 0xFF]), Some(Err(())));
    assert_eq!(frame_len(&(MAX_MESSAGE as u32 + 1).to_be_bytes()), Some(Err(())));
    assert_eq!(frame_len(&5u32.to_be_bytes()), Some(Ok(9)));
}

#[test]
fn requests_before_init_are_refused() {
    let mut server = Server::new(Memory::new(), "/home/roman");
    let body = call(&mut server, |w| {
        w.byte(FXP_REALPATH);
        w.u32(1);
        w.string(b".");
    });
    assert_eq!(status_of(&body), FX_BAD_MESSAGE);
}

#[test]
fn write_then_read_round_trip() {
    let mut server = server();
    let handle = open(&mut server, "file.bin", FXF_WRITE | FXF_CREAT | FXF_TRUNC).unwrap();
    let payload: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    for (index, chunk) in payload.chunks(MAX_DATA).enumerate() {
        let body = call(&mut server, |w| {
            w.byte(FXP_WRITE);
            w.u32(10 + index as u32);
            w.string(&handle);
            w.u32(0);
            w.u32((index * MAX_DATA) as u32);
            w.string(chunk);
        });
        assert_eq!(status_of(&body), FX_OK);
    }
    // Чтение через дескриптор, открытый только на запись, — отказ.
    let body = call(&mut server, |w| {
        w.byte(FXP_READ);
        w.u32(1);
        w.string(&handle);
        w.u32(0);
        w.u32(0);
        w.u32(10);
    });
    assert_eq!(status_of(&body), FX_PERMISSION_DENIED);
    let body = call(&mut server, |w| {
        w.byte(FXP_CLOSE);
        w.u32(2);
        w.string(&handle);
    });
    assert_eq!(status_of(&body), FX_OK);
    assert!(server.fs.log.iter().any(|l| l.contains("after 70000 bytes written")));

    let handle = open(&mut server, "/home/roman/file.bin", FXF_READ).unwrap();
    let mut back = Vec::new();
    loop {
        let body = call(&mut server, |w| {
            w.byte(FXP_READ);
            w.u32(3);
            w.string(&handle);
            w.u32(0);
            w.u32(back.len() as u32);
            // Больше, чем разрешено: сервер обязан урезать, а не писать мимо.
            w.u32(1_000_000);
        });
        if body[0] == FXP_STATUS {
            assert_eq!(status_of(&body), FX_EOF);
            break;
        }
        assert_eq!(body[0], FXP_DATA);
        let mut r = Reader::new(&body[5..]);
        let data = r.string().unwrap();
        assert!(data.len() <= MAX_DATA);
        back.extend_from_slice(data);
    }
    assert_eq!(back, payload);
}

#[test]
fn permission_errors_come_from_the_filesystem() {
    let mut server = server();
    assert_eq!(
        open(&mut server, "/etc/new", FXF_WRITE | FXF_CREAT | FXF_TRUNC),
        Err(FX_PERMISSION_DENIED)
    );
    assert_eq!(open(&mut server, "/etc/passwd", FXF_WRITE), Err(FX_PERMISSION_DENIED));
    assert!(server.fs.log.iter().any(|l| l.contains("/etc/new refused: permission denied")));
    assert_eq!(open(&mut server, "/nope", FXF_READ), Err(FX_NO_SUCH_FILE));

    // Файл, который можно писать, но нельзя читать: чтение с записью
    // обязано спросить право читать отдельно.
    server.fs.nodes.insert("/home/roman/secret".into(), (false, b"x".to_vec()));
    server.fs.no_read.push("/home/roman/secret".into());
    assert_eq!(
        open(&mut server, "secret", FXF_READ | FXF_WRITE),
        Err(FX_PERMISSION_DENIED)
    );
    assert!(open(&mut server, "secret", FXF_WRITE).is_ok());
}

#[test]
fn exclusive_create_refuses_existing() {
    let mut server = server();
    assert!(open(&mut server, "a", FXF_WRITE | FXF_CREAT | FXF_EXCL).is_ok());
    assert_eq!(open(&mut server, "a", FXF_WRITE | FXF_CREAT | FXF_EXCL), Err(FX_FAILURE));
    // Без EXCL — открыть существующий.
    assert!(open(&mut server, "a", FXF_WRITE | FXF_CREAT).is_ok());
    // Каталог — не файл.
    assert_eq!(open(&mut server, "/etc", FXF_READ), Err(FX_FAILURE));
}

#[test]
fn directories_rename_and_remove() {
    let mut server = server();
    let ok = |server: &mut Server<Memory>, kind: u8, a: &str, b: Option<&str>| {
        let body = call(server, |w| {
            w.byte(kind);
            w.u32(5);
            w.string(a.as_bytes());
            match b {
                Some(b) => {
                    w.string(b.as_bytes());
                }
                None if kind == FXP_MKDIR => {
                    w.u32(0);
                }
                None => {}
            }
        });
        status_of(&body)
    };
    assert_eq!(ok(&mut server, FXP_MKDIR, "apps", None), FX_OK);
    assert_eq!(ok(&mut server, FXP_MKDIR, "/etc/x", None), FX_PERMISSION_DENIED);
    server.fs.nodes.insert("/home/roman/apps/one".into(), (false, b"1".to_vec()));
    assert_eq!(ok(&mut server, FXP_RENAME, "apps/one", Some("apps/two")), FX_OK);
    assert_eq!(ok(&mut server, FXP_RENAME, "apps/two", Some("/etc/passwd")), FX_FAILURE);
    assert_eq!(ok(&mut server, FXP_REMOVE, "apps", None), FX_FAILURE, "rm на каталоге");
    assert_eq!(ok(&mut server, FXP_RMDIR, "apps/two", None), FX_FAILURE, "rmdir на файле");
    assert_eq!(ok(&mut server, FXP_RMDIR, "apps", None), FX_FAILURE, "не пуст");
    assert_eq!(ok(&mut server, FXP_REMOVE, "apps/two", None), FX_OK);
    assert_eq!(ok(&mut server, FXP_RMDIR, "apps", None), FX_OK);
}

#[test]
fn readdir_lists_every_entry_once() {
    let mut server = server();
    for i in 0..250 {
        server.fs.nodes.insert(format!("/home/roman/f{i:03}"), (false, vec![0; i]));
    }
    let body = call(&mut server, |w| {
        w.byte(FXP_OPENDIR);
        w.u32(1);
        w.string(b".");
    });
    assert_eq!(body[0], FXP_HANDLE);
    let handle = Reader::new(&body[5..]).string().unwrap().to_vec();
    let mut names = Vec::new();
    loop {
        let body = call(&mut server, |w| {
            w.byte(FXP_READDIR);
            w.u32(2);
            w.string(&handle);
        });
        if body[0] == FXP_STATUS {
            assert_eq!(status_of(&body), FX_EOF);
            break;
        }
        assert_eq!(body[0], FXP_NAME);
        let mut r = Reader::new(&body[5..]);
        let count = r.u32().unwrap();
        for _ in 0..count {
            names.push(String::from_utf8(r.string().unwrap().to_vec()).unwrap());
            let long = r.string().unwrap();
            assert!(long.starts_with(b"-rw-r--r--"));
            let flags = r.u32().unwrap();
            assert_eq!(flags, ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS);
            for _ in 0..5 {
                r.u32().unwrap();
            }
        }
        assert_eq!(r.remaining(), 0);
    }
    assert_eq!(names.len(), 250);
}

#[test]
fn handles_are_checked() {
    let mut server = server();
    let handle = open(&mut server, "/etc/passwd", FXF_READ).unwrap();
    for bogus in [&b""[..], b"abc", b"abcde", &[9, 0, 0, 1], &[handle[0], 0xFF, 0xFF, 0xFF]] {
        let body = call(&mut server, |w| {
            w.byte(FXP_READ);
            w.u32(1);
            w.string(bogus);
            w.u32(0);
            w.u32(0);
            w.u32(10);
        });
        assert_eq!(status_of(&body), FX_FAILURE, "{bogus:?}");
    }
    // Закрытый дескриптор больше не действует, а новый на том же месте
    // получает другое имя.
    call(&mut server, |w| {
        w.byte(FXP_CLOSE);
        w.u32(1);
        w.string(&handle);
    });
    let again = open(&mut server, "/etc/passwd", FXF_READ).unwrap();
    assert_eq!(again[0], handle[0]);
    assert_ne!(again, handle);
    let body = call(&mut server, |w| {
        w.byte(FXP_CLOSE);
        w.u32(1);
        w.string(&handle);
    });
    assert_eq!(status_of(&body), FX_FAILURE);
}

#[test]
fn handle_table_is_bounded() {
    let mut server = server();
    for _ in 0..HANDLES {
        assert!(open(&mut server, "/etc/passwd", FXF_READ).is_ok());
    }
    assert_eq!(open(&mut server, "/etc/passwd", FXF_READ), Err(FX_FAILURE));
    server.close_all();
    assert!(open(&mut server, "/etc/passwd", FXF_READ).is_ok());
}

#[test]
fn unsupported_requests_say_so() {
    let mut server = server();
    for kind in [FXP_READLINK, FXP_SYMLINK, FXP_EXTENDED, 77] {
        let body = call(&mut server, |w| {
            w.byte(kind);
            w.u32(9);
            w.string(b"statvfs@openssh.com");
        });
        assert_eq!(status_of(&body), FX_OP_UNSUPPORTED, "{kind}");
        assert_eq!(&body[1..5], &9u32.to_be_bytes(), "номер запроса сохранён");
    }
    let body = call(&mut server, |w| {
        w.byte(FXP_SETSTAT);
        w.u32(1);
        w.string(b"/etc/passwd");
        w.u32(ATTR_PERMISSIONS);
        w.u32(0o777);
    });
    assert_eq!(status_of(&body), FX_OP_UNSUPPORTED);
}

/// Каждый обрезанный вариант каждого запроса — это отказ, а не паника и не
/// чтение мимо буфера. И длины строк, раздутые до четырёх гигабайт, тоже.
#[test]
fn truncated_and_inflated_requests_are_rejected() {
    let mut samples: Vec<Vec<u8>> = Vec::new();
    let mut push = |build: &dyn Fn(&mut Writer<'_>)| {
        let mut buffer = [0u8; 256];
        let mut w = Writer::new(&mut buffer);
        build(&mut w);
        let len = w.len();
        samples.push(buffer[..len].to_vec());
    };
    push(&|w| {
        w.byte(FXP_OPEN);
        w.u32(1);
        w.string(b"f");
        w.u32(FXF_WRITE | FXF_CREAT);
        w.u32(ATTR_PERMISSIONS | ATTR_EXTENDED);
        w.u32(0o600);
        w.u32(1);
        w.string(b"k");
        w.string(b"v");
    });
    push(&|w| {
        w.byte(FXP_WRITE);
        w.u32(1);
        w.string(&[0, 0, 0, 1]);
        w.u32(0);
        w.u32(0);
        w.string(b"data");
    });
    push(&|w| {
        w.byte(FXP_READ);
        w.u32(1);
        w.string(&[0, 0, 0, 1]);
        w.u32(0);
        w.u32(0);
        w.u32(4);
    });
    push(&|w| {
        w.byte(FXP_RENAME);
        w.u32(1);
        w.string(b"a");
        w.string(b"b");
    });
    push(&|w| {
        w.byte(FXP_MKDIR);
        w.u32(1);
        w.string(b"d");
        w.u32(ATTR_SIZE | ATTR_UIDGID | ATTR_ACMODTIME);
        w.u32(0);
        w.u32(0);
        w.u32(0);
        w.u32(0);
        w.u32(0);
        w.u32(0);
    });

    for sample in &samples {
        for cut in 1..sample.len() {
            let mut server = server();
            let body = raw(&mut server, &sample[..cut]);
            assert_eq!(body[0], FXP_STATUS, "обрезка до {cut}");
        }
        // Каждое четырёхбайтное окно, заменённое на 0xFFFFFFFF, — как длина
        // строки или число пар расширений, которым нельзя верить.
        for at in 1..sample.len().saturating_sub(4) {
            let mut bad = sample.clone();
            bad[at..at + 4].copy_from_slice(&[0xFF; 4]);
            let mut server = server();
            let mut reply = vec![0u8; REPLY_BUFFER];
            // Разумный ответ или ничего — но не паника.
            let _ = server.handle(&bad, &mut reply);
        }
    }

    // Число пар расширений больше, чем байт в пакете.
    let mut server = server();
    let body = call(&mut server, |w| {
        w.byte(FXP_MKDIR);
        w.u32(1);
        w.string(b"d");
        w.u32(ATTR_EXTENDED);
        w.u32(0x7FFF_FFFF);
    });
    assert_eq!(status_of(&body), FX_BAD_MESSAGE);

    // Смещение записи, за которым файл перешёл бы в отрицательные позиции.
    let handle = open(&mut server, "big", FXF_WRITE | FXF_CREAT).unwrap();
    let body = call(&mut server, |w| {
        w.byte(FXP_WRITE);
        w.u32(1);
        w.string(&handle);
        w.u32(0xFFFF_FFFF);
        w.u32(0xFFFF_FFF0);
        w.string(b"0123456789abcdef0123");
    });
    assert_eq!(status_of(&body), FX_FAILURE);
}

#[test]
fn realpath_and_stat() {
    let mut server = server();
    let body = call(&mut server, |w| {
        w.byte(FXP_REALPATH);
        w.u32(4);
        w.string(b".");
    });
    assert_eq!(body[0], FXP_NAME);
    let mut r = Reader::new(&body[5..]);
    assert_eq!(r.u32(), Some(1));
    assert_eq!(r.string(), Some(&b"/home/roman"[..]));

    let body = call(&mut server, |w| {
        w.byte(FXP_STAT);
        w.u32(4);
        w.string(b"/etc");
    });
    assert_eq!(body[0], FXP_ATTRS);
    let mut r = Reader::new(&body[5..]);
    assert_eq!(r.u32(), Some(ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS));
    r.u32();
    r.u32();
    r.u32();
    r.u32();
    assert_eq!(r.u32(), Some(0o040_755));
}
