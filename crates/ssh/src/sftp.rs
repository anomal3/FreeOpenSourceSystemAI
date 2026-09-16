// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! SFTP версии 3 (draft-ietf-secsh-filexfer-02): разбор запросов и ответы.
//!
//! # Почему здесь, а не в программе
//!
//! Потому что здесь его можно проверить на хосте. Всё, что приходит в этот
//! модуль, прислал клиент, то есть кто угодно, и каждое поле длины в пакете —
//! это число, которым пытаются заставить сервер прочитать или написать мимо
//! буфера. Проверить разбор чужих байтов в эмуляторе можно только клиентом,
//! который шлёт правильные пакеты; обрезанные, раздутые и перепутанные шлёт
//! только тест. Поэтому файловая система спрятана за [`Fs`], и тесты
//! (`sftp_tests.rs`) подставляют вместо неё свою, в памяти.
//!
//! # Кто проверяет права
//!
//! Не этот модуль. Сервер (`/bin/sftp-server`) запускается `sshd` **от имени
//! вошедшего**, и каждый вызов [`Fs`] — это системный вызов этой задачи: права
//! спрашивает ядро, тем же кодом, что за терминалом. Своей проверки прав здесь
//! нет намеренно — фаза 38b убрала вторую проверку из `sshd`, и возвращать её
//! через SFTP значило бы отменить то, ради чего она была.
//!
//! Единственное исключение — дескриптор, открытый на чтение **и** запись:
//! ядро открывает файл на запись, спросив только право писать, а читать из
//! такого дескриптора потом не мешает. Файл `0200` читался бы через SFTP, хотя
//! за терминалом его не прочесть. Поэтому право читать спрашивается у ядра
//! отдельным открытием **до** того, как файл откроют на запись.
//!
//! # Чего здесь нет — и ответ на это `OP_UNSUPPORTED`, а не молчаливый успех
//!
//! Ссылок (`READLINK`, `SYMLINK`): их нет в системе. Смены атрибутов
//! (`SETSTAT`, `FSETSTAT`): у ядра нет ни `chmod`, ни смены времени, и ответ
//! «готово» на просьбу поставить права означал бы, что клиент уверен в правах,
//! которых у файла нет. Расширений OpenSSH (`statvfs`, `posix-rename`,
//! `hardlink`, `limits`): их просят через `EXTENDED`, и отказ на него клиент
//! переживает сам.

use crate::wire::{Reader, Writer};

// --- типы сообщений --------------------------------------------------------

pub const FXP_INIT: u8 = 1;
pub const FXP_VERSION: u8 = 2;
pub const FXP_OPEN: u8 = 3;
pub const FXP_CLOSE: u8 = 4;
pub const FXP_READ: u8 = 5;
pub const FXP_WRITE: u8 = 6;
pub const FXP_LSTAT: u8 = 7;
pub const FXP_FSTAT: u8 = 8;
pub const FXP_SETSTAT: u8 = 9;
pub const FXP_FSETSTAT: u8 = 10;
pub const FXP_OPENDIR: u8 = 11;
pub const FXP_READDIR: u8 = 12;
pub const FXP_REMOVE: u8 = 13;
pub const FXP_MKDIR: u8 = 14;
pub const FXP_RMDIR: u8 = 15;
pub const FXP_REALPATH: u8 = 16;
pub const FXP_STAT: u8 = 17;
pub const FXP_RENAME: u8 = 18;
pub const FXP_READLINK: u8 = 19;
pub const FXP_SYMLINK: u8 = 20;
pub const FXP_STATUS: u8 = 101;
pub const FXP_HANDLE: u8 = 102;
pub const FXP_DATA: u8 = 103;
pub const FXP_NAME: u8 = 104;
pub const FXP_ATTRS: u8 = 105;
pub const FXP_EXTENDED: u8 = 200;

// --- коды ответа -----------------------------------------------------------

pub const FX_OK: u32 = 0;
pub const FX_EOF: u32 = 1;
pub const FX_NO_SUCH_FILE: u32 = 2;
pub const FX_PERMISSION_DENIED: u32 = 3;
pub const FX_FAILURE: u32 = 4;
pub const FX_BAD_MESSAGE: u32 = 5;
pub const FX_OP_UNSUPPORTED: u32 = 8;

// --- флаги открытия и атрибутов --------------------------------------------

pub const FXF_READ: u32 = 0x01;
pub const FXF_WRITE: u32 = 0x02;
pub const FXF_APPEND: u32 = 0x04;
pub const FXF_CREAT: u32 = 0x08;
pub const FXF_TRUNC: u32 = 0x10;
pub const FXF_EXCL: u32 = 0x20;

pub const ATTR_SIZE: u32 = 0x01;
pub const ATTR_UIDGID: u32 = 0x02;
pub const ATTR_PERMISSIONS: u32 = 0x04;
pub const ATTR_ACMODTIME: u32 = 0x08;
pub const ATTR_EXTENDED: u32 = 0x8000_0000;

/// Биты типа в поле прав — так их пишет `stat` в Unix, и так их читает клиент,
/// чтобы показать `d` в `ls -l` и не пытаться скачать каталог как файл.
const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;

/// Версия протокола, которую мы говорим.
pub const VERSION: u32 = 3;

/// Наибольшее сообщение, которое сервер согласен принять (без поля длины).
///
/// Клиент OpenSSH пишет файл кусками по 32 КиБ, и заголовок `WRITE` с путём к
/// дескриптору добавляет к ним пару десятков байт. Больше — это либо чужая
/// ошибка, либо попытка заставить сервер ждать, пока придут сотни мегабайт
/// «одного сообщения». OpenSSH на такое тоже рвёт соединение.
pub const MAX_MESSAGE: usize = 34_000;

/// Наибольший кусок данных в одном ответе `DATA`.
///
/// Протокол разрешает отдать меньше, чем попросили, и клиент тогда
/// дозапрашивает остаток. Больше 32 КиБ не просит ни OpenSSH, ни WinSCP.
pub const MAX_DATA: usize = 32_768;

/// Сколько места нужно буферу ответа: самый большой ответ — `DATA` на
/// [`MAX_DATA`] с заголовком.
pub const REPLY_BUFFER: usize = MAX_DATA + 64;

/// Сколько файлов и каталогов открыто одновременно.
///
/// Столько же, сколько ядро даёт одной программе (`MAX_OPEN_FILES`): больше
/// дескрипторов у задачи всё равно не будет, а отказ по своей таблице внятнее
/// отказа ядра посреди `OPEN`.
pub const HANDLES: usize = 8;

/// Самый длинный путь — столько же принимает ядро.
pub const MAX_PATH: usize = 255;

/// Самое длинное имя в каталоге — столько же хранит ext2.
pub const MAX_NAME: usize = 255;

/// Почему файловая система отказала.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    Permission,
    Exists,
    NotEmpty,
    NoSpace,
    /// Не файл и не каталог, а что-то третье, или операция не по силам тому.
    Unsupported,
    WrongKind,
    TooManyFiles,
    Io,
}

/// Что известно об узле.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub size: u64,
    /// Девять бит прав, без типа.
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub directory: bool,
    /// Время изменения, секунды эпохи. `None` — вызов его не сообщает (`stat`
    /// ядра времени не отдаёт, а запись каталога — отдаёт).
    pub mtime: Option<u32>,
}

/// Файловая система, от имени которой работает сервер.
///
/// Дескрипторы — числа ядра. Всё здесь отвечает `Result`, и ни одна ошибка не
/// становится паникой: отказ диска — это ответ клиенту, а не конец сеанса.
pub trait Fs {
    fn open_read(&mut self, path: &str) -> Result<i64, FsError>;
    /// Открыть существующий файл на запись; `truncate` — обрезать до нуля.
    fn open_write(&mut self, path: &str, truncate: bool) -> Result<i64, FsError>;
    /// Создать новый файл с правами `mode`; занятое имя — [`FsError::Exists`].
    fn create(&mut self, path: &str, mode: u32) -> Result<i64, FsError>;
    fn read_at(&mut self, file: i64, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError>;
    fn write_at(&mut self, file: i64, offset: u64, data: &[u8]) -> Result<usize, FsError>;
    fn size(&mut self, file: i64) -> Result<u64, FsError>;
    fn fstat(&mut self, file: i64) -> Result<Meta, FsError>;
    fn stat(&mut self, path: &str) -> Result<Meta, FsError>;
    /// Очередная запись открытого каталога: длина имени в `name` и сведения.
    fn next_entry(
        &mut self,
        dir: i64,
        name: &mut [u8; MAX_NAME],
    ) -> Result<Option<(usize, Meta)>, FsError>;
    fn close(&mut self, file: i64);
    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), FsError>;
    fn remove(&mut self, path: &str) -> Result<(), FsError>;
    fn rename(&mut self, old: &str, new: &str) -> Result<(), FsError>;
    /// Строка в журнал системы. Куски, а не одна строка: кучи нет.
    fn log(&mut self, _parts: &[&str]) {}
}

/// Путь в буфере фиксированной длины.
#[derive(Clone, Copy)]
pub struct PathBuf {
    bytes: [u8; MAX_PATH],
    len: usize,
}

impl PathBuf {
    pub const fn new() -> Self {
        Self { bytes: [0; MAX_PATH], len: 0 }
    }

    pub fn as_str(&self) -> &str {
        // Сюда попадают только куски проверенного UTF-8 и косые черты: см.
        // `resolve`. Проверка стоит копейки, а `unwrap_or` не даёт панике шанса.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("/")
    }

    fn push(&mut self, data: &[u8]) -> bool {
        if self.len + data.len() > MAX_PATH {
            return false;
        }
        self.bytes[self.len..self.len + data.len()].copy_from_slice(data);
        self.len += data.len();
        true
    }

    /// Отрезать последний компонент: `..`.
    fn pop(&mut self) {
        while self.len > 0 && self.bytes[self.len - 1] != b'/' {
            self.len -= 1;
        }
        // Косая черта перед компонентом тоже уходит — кроме корня.
        if self.len > 1 {
            self.len -= 1;
        }
    }
}

impl Default for PathBuf {
    fn default() -> Self {
        Self::new()
    }
}

/// Привести путь клиента к абсолютному виду без `.` и `..`.
///
/// Относительный путь считается от домашнего каталога: текущего каталога у
/// сервера нет, а клиенты SFTP начинают разговор с `REALPATH "."` и дальше
/// ходят от ответа. `..` у корня остаётся корнем — так ведёт себя всякий Unix.
///
/// Разбор чисто текстовый, и этого достаточно: ссылок в системе нет, а
/// выйти за пределы разрешённого этим всё равно нельзя — пускать или нет,
/// решает ядро по правам, а не по тому, как путь записан.
pub fn resolve(home: &str, raw: &[u8]) -> Option<PathBuf> {
    // Нулевой байт — конец строки для половины мира. Путь, который здесь
    // значит одно, а в журнале или у ядра другое, не принимается вовсе.
    if raw.contains(&0) {
        return None;
    }
    core::str::from_utf8(raw).ok()?;

    let mut out = PathBuf::new();
    out.push(b"/");
    let base: &[u8] = if raw.first() == Some(&b'/') { b"" } else { home.as_bytes() };
    for part in base.split(|b| *b == b'/').chain(raw.split(|b| *b == b'/')) {
        match part {
            b"" | b"." => {}
            b".." => out.pop(),
            name => {
                if name.len() > MAX_NAME {
                    return None;
                }
                if out.len > 1 && !out.push(b"/") {
                    return None;
                }
                if !out.push(name) {
                    return None;
                }
            }
        }
    }
    Some(out)
}

/// Длина очередного сообщения во входном потоке.
///
/// `None` — не хватает даже поля длины; `Some(Err(()))` — длина недопустима, и
/// разговаривать дальше не о чем: сдвинуться к следующему сообщению нельзя, не
/// поверив числу, которому верить нельзя. `Some(Ok(n))` — сообщение займёт `n`
/// байт вместе с полем длины.
pub fn frame_len(buffer: &[u8]) -> Option<Result<usize, ()>> {
    let head = buffer.get(..4)?;
    let len = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
    // Пустое сообщение не несёт даже типа.
    if len == 0 || len > MAX_MESSAGE {
        return Some(Err(()));
    }
    Some(Ok(4 + len))
}

/// Открытый дескриптор сервера.
#[derive(Clone, Copy)]
struct Handle {
    file: i64,
    /// Номер выдачи: дескриптор, закрытый и выданный заново, получает другое
    /// имя, и клиент, перепутавший старое, получит отказ, а не чужой файл.
    generation: u32,
    directory: bool,
    readable: bool,
    writable: bool,
    append: bool,
    /// Сколько байт записано через этот дескриптор — для строки в журнал.
    written: u64,
    path: PathBuf,
}

/// Сколько всего перенесено за сеанс.
#[derive(Clone, Copy, Debug, Default)]
pub struct Totals {
    pub bytes_read: u64,
    pub bytes_written: u64,
    pub requests: u64,
    /// Ответов-ошибок, кроме `EOF`. Сюда входит и «нет такого файла» на `STAT`,
    /// которым клиент проверяет имя перед записью, — это не всегда отказ.
    pub errors: u64,
}

/// Сервер одного сеанса.
pub struct Server<F: Fs> {
    pub fs: F,
    home: PathBuf,
    initialized: bool,
    handles: [Option<Handle>; HANDLES],
    generation: u32,
    pub totals: Totals,
}

/// Ответ, собранный наполовину: кто спрашивал и чем кончилось.
type Outcome = Result<usize, (u32, &'static str)>;

impl<F: Fs> Server<F> {
    /// Завести сервер. `home` — абсолютный путь домашнего каталога.
    pub fn new(fs: F, home: &str) -> Self {
        let home = resolve("/", home.as_bytes()).unwrap_or_default();
        Self {
            fs,
            home,
            initialized: false,
            handles: [None; HANDLES],
            generation: 0,
            totals: Totals::default(),
        }
    }

    /// Закрыть всё, что клиент оставил открытым.
    ///
    /// Клиент вправе оборвать сеанс, не закрыв дескрипторы; дописанный наполовину
    /// файл при этом остаётся на диске таким, каким его дописали, — но
    /// дескрипторы ядра обязаны вернуться, иначе программа, живущая дольше
    /// сеанса, упёрлась бы в предел открытых файлов.
    pub fn close_all(&mut self) {
        for slot in &mut self.handles {
            if let Some(handle) = slot.take() {
                self.fs.close(handle.file);
            }
        }
    }

    /// Обработать одно сообщение (без поля длины) и записать ответ в `reply`
    /// вместе с полем длины. Возвращает длину ответа; ноль — отвечать нечем
    /// (такого не бывает, но вызывающий обязан это пережить).
    pub fn handle(&mut self, message: &[u8], reply: &mut [u8]) -> usize {
        self.totals.requests += 1;
        if reply.len() < REPLY_BUFFER {
            return 0;
        }
        let mut reader = Reader::new(message);
        let Some(kind) = reader.byte() else {
            return status(reply, 0, FX_BAD_MESSAGE, "empty message");
        };

        if kind == FXP_INIT {
            if self.initialized {
                return status(reply, 0, FX_BAD_MESSAGE, "INIT twice");
            }
            // Версия клиента не проверяется: всякий клиент новее третьей обязан
            // уметь третью, а старше — не бывает (вторая описывала заготовку).
            // Расширения клиента после номера версии пропускаются молча.
            if reader.u32().is_none() {
                return status(reply, 0, FX_BAD_MESSAGE, "INIT without a version");
            }
            self.initialized = true;
            return frame(reply, |w| {
                w.byte(FXP_VERSION);
                w.u32(VERSION);
            });
        }

        // У всех остальных запросов за типом идёт номер. Прочитать его надо
        // до всего прочего: ответ без номера клиент не сопоставит ни с чем.
        let Some(id) = reader.u32() else {
            return status(reply, 0, FX_BAD_MESSAGE, "no request id");
        };
        if !self.initialized {
            return status(reply, id, FX_BAD_MESSAGE, "INIT first");
        }

        let outcome = match kind {
            FXP_OPEN => self.open(&mut reader, id, reply),
            FXP_CLOSE => self.close(&mut reader),
            FXP_READ => self.read(&mut reader, id, reply),
            FXP_WRITE => self.write(&mut reader),
            FXP_LSTAT | FXP_STAT => self.stat(&mut reader, id, reply),
            FXP_FSTAT => self.fstat(&mut reader, id, reply),
            FXP_SETSTAT | FXP_FSETSTAT => self.setstat(kind, &mut reader),
            FXP_OPENDIR => self.opendir(&mut reader, id, reply),
            FXP_READDIR => self.readdir(&mut reader, id, reply),
            FXP_REMOVE => self.remove(&mut reader, false),
            FXP_RMDIR => self.remove(&mut reader, true),
            FXP_MKDIR => self.mkdir(&mut reader),
            FXP_REALPATH => self.realpath(&mut reader, id, reply),
            FXP_RENAME => self.rename(&mut reader),
            FXP_READLINK | FXP_SYMLINK => Err((FX_OP_UNSUPPORTED, "there are no links here")),
            FXP_EXTENDED => Err((FX_OP_UNSUPPORTED, "no extensions")),
            _ => Err((FX_OP_UNSUPPORTED, "unknown request")),
        };

        match outcome {
            Ok(0) => status(reply, id, FX_OK, "ok"),
            Ok(len) => len,
            Err((code, text)) => {
                if code != FX_EOF {
                    self.totals.errors += 1;
                }
                status(reply, id, code, text)
            }
        }
    }

    // --- запросы ---------------------------------------------------------

    fn open(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let flags = reader.u32().ok_or(BAD)?;
        let attrs = read_attrs(reader).ok_or(BAD)?;
        let path = self.path(raw)?;
        let slot = self.free_slot()?;

        let writable = flags & (FXF_WRITE | FXF_APPEND) != 0;
        // Ни чтения, ни записи — клиент ничего не просил; читать безопаснее.
        let readable = flags & FXF_READ != 0 || !writable;
        let mode = attrs.permissions.map_or(0o644, |mode| mode & 0o777);

        let file = if writable {
            // Право читать спрашивается первым и отдельно — см. заголовок
            // модуля: дескриптор на запись читать не мешает, а ядро при его
            // открытии про чтение не спрашивало.
            if readable {
                match self.fs.open_read(path.as_str()) {
                    Ok(probe) => self.fs.close(probe),
                    Err(FsError::NotFound) if flags & FXF_CREAT != 0 => {}
                    Err(err) => return Err(self.refuse("open", &path, err)),
                }
            }
            let opened = if flags & FXF_CREAT != 0 {
                match self.fs.create(path.as_str(), mode) {
                    Err(FsError::Exists) if flags & FXF_EXCL == 0 => {
                        self.fs.open_write(path.as_str(), flags & FXF_TRUNC != 0)
                    }
                    other => other,
                }
            } else {
                self.fs.open_write(path.as_str(), flags & FXF_TRUNC != 0)
            };
            match opened {
                Ok(file) => file,
                Err(err) => return Err(self.refuse("open for writing", &path, err)),
            }
        } else {
            match self.fs.open_read(path.as_str()) {
                Ok(file) => file,
                Err(err) => return Err(self.refuse("open", &path, err)),
            }
        };

        // Каталог ядро откроет и на чтение — на перечисление. Но `READ` из него
        // отказал бы на каждом куске, и клиент увидел бы непонятную ошибку
        // посреди скачивания вместо внятной в самом начале.
        match self.fs.fstat(file) {
            Ok(meta) if meta.directory => {
                self.fs.close(file);
                return Err((FX_FAILURE, "is a directory"));
            }
            Ok(_) => {}
            Err(err) => {
                self.fs.close(file);
                return Err(self.refuse("fstat", &path, err));
            }
        }

        if writable {
            self.fs.log(&["sftp-server: writing ", path.as_str()]);
        }
        let handle = Handle {
            file,
            generation: self.next_generation(),
            directory: false,
            readable,
            writable,
            append: flags & FXF_APPEND != 0,
            written: 0,
            path,
        };
        self.handles[slot] = Some(handle);
        Ok(handle_reply(reply, id, slot, handle.generation))
    }

    fn close(&mut self, reader: &mut Reader<'_>) -> Outcome {
        let slot = self.slot(reader)?;
        let Some(handle) = self.handles[slot].take() else {
            return Err(BAD_HANDLE);
        };
        self.fs.close(handle.file);
        if handle.writable {
            let mut digits = [0u8; 20];
            self.fs.log(&[
                "sftp-server: closed ",
                handle.path.as_str(),
                " after ",
                decimal(handle.written, &mut digits),
                " bytes written",
            ]);
        }
        Ok(0)
    }

    fn read(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let slot = self.slot(reader)?;
        let offset = read_u64(reader).ok_or(BAD)?;
        let wanted = reader.u32().ok_or(BAD)? as usize;
        let handle = self.handles[slot].ok_or(BAD_HANDLE)?;
        if handle.directory || !handle.readable {
            return Err((FX_PERMISSION_DENIED, "not open for reading"));
        }

        // Данные читаются прямо на место в ответе: заголовок `DATA` — это
        // длина (4), тип (1), номер (4) и длина строки (4).
        const HEAD: usize = 13;
        let wanted = wanted.min(MAX_DATA);
        let got = match self.fs.read_at(handle.file, offset, &mut reply[HEAD..HEAD + wanted]) {
            Ok(got) => got.min(wanted),
            Err(err) => return Err(self.refuse("read", &handle.path, err)),
        };
        if got == 0 {
            // Протокол различает «кончился файл» и «прочитано ноль»: второго не
            // бывает, а клиент ждёт именно `EOF`, чтобы перестать спрашивать.
            return Err((FX_EOF, "end of file"));
        }
        self.totals.bytes_read += got as u64;
        let body = 1 + 4 + 4 + got;
        reply[..4].copy_from_slice(&(body as u32).to_be_bytes());
        reply[4] = FXP_DATA;
        reply[5..9].copy_from_slice(&id.to_be_bytes());
        reply[9..13].copy_from_slice(&(got as u32).to_be_bytes());
        Ok(HEAD + got)
    }

    fn write(&mut self, reader: &mut Reader<'_>) -> Outcome {
        let slot = self.slot(reader)?;
        let offset = read_u64(reader).ok_or(BAD)?;
        let data = reader.string().ok_or(BAD)?;
        let handle = self.handles[slot].ok_or(BAD_HANDLE)?;
        if handle.directory || !handle.writable {
            return Err((FX_PERMISSION_DENIED, "not open for writing"));
        }
        let mut at = if handle.append {
            match self.fs.size(handle.file) {
                Ok(size) => size,
                Err(err) => return Err(self.refuse("size", &handle.path, err)),
            }
        } else {
            offset
        };
        // Смещение приходит числом с провода, а ядро принимает знаковое:
        // значение за `i64::MAX` означало бы отрицательную позицию.
        if at > i64::MAX as u64 - data.len() as u64 {
            return Err((FX_FAILURE, "offset out of range"));
        }

        let mut rest = data;
        while !rest.is_empty() {
            match self.fs.write_at(handle.file, at, rest) {
                // Ноль при непустых данных — место кончилось, а не «попробуй
                // ещё»: крутиться здесь значило бы повесить сеанс.
                Ok(0) => return Err((FX_FAILURE, "no space left")),
                Ok(done) => {
                    let done = done.min(rest.len());
                    rest = &rest[done..];
                    at += done as u64;
                }
                Err(err) => return Err(self.refuse("write", &handle.path, err)),
            }
        }
        self.totals.bytes_written += data.len() as u64;
        if let Some(open) = self.handles[slot].as_mut() {
            open.written += data.len() as u64;
        }
        Ok(0)
    }

    fn stat(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let path = self.path(raw)?;
        match self.fs.stat(path.as_str()) {
            Ok(meta) => Ok(frame(reply, |w| {
                w.byte(FXP_ATTRS);
                w.u32(id);
                write_attrs(w, &meta);
            })),
            // Отказ `stat` — обычный вопрос «есть ли такой?», и в журнал он не
            // пишется: клиент спрашивает так о каждом файле перед записью.
            Err(err) => Err(code_of(err)),
        }
    }

    fn fstat(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let slot = self.slot(reader)?;
        let handle = self.handles[slot].ok_or(BAD_HANDLE)?;
        match self.fs.fstat(handle.file) {
            Ok(meta) => Ok(frame(reply, |w| {
                w.byte(FXP_ATTRS);
                w.u32(id);
                write_attrs(w, &meta);
            })),
            Err(err) => Err(code_of(err)),
        }
    }

    fn setstat(&mut self, kind: u8, reader: &mut Reader<'_>) -> Outcome {
        if kind == FXP_SETSTAT {
            let raw = reader.string().ok_or(BAD)?;
            self.path(raw)?;
        } else {
            self.slot(reader)?;
        }
        let attrs = read_attrs(reader).ok_or(BAD)?;
        // Нечего менять — нечего и отказывать: пустой запрос выполнен.
        if attrs.flags & !ATTR_EXTENDED == 0 {
            return Ok(0);
        }
        Err((FX_OP_UNSUPPORTED, "attributes cannot be changed on this system"))
    }

    fn opendir(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let path = self.path(raw)?;
        let slot = self.free_slot()?;
        let file = match self.fs.open_read(path.as_str()) {
            Ok(file) => file,
            Err(err) => return Err(self.refuse("opendir", &path, err)),
        };
        match self.fs.fstat(file) {
            Ok(meta) if meta.directory => {}
            Ok(_) => {
                self.fs.close(file);
                return Err((FX_FAILURE, "not a directory"));
            }
            Err(err) => {
                self.fs.close(file);
                return Err(code_of(err));
            }
        }
        let handle = Handle {
            file,
            generation: self.next_generation(),
            directory: true,
            readable: true,
            writable: false,
            append: false,
            written: 0,
            path,
        };
        self.handles[slot] = Some(handle);
        Ok(handle_reply(reply, id, slot, handle.generation))
    }

    fn readdir(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let slot = self.slot(reader)?;
        let handle = self.handles[slot].ok_or(BAD_HANDLE)?;
        if !handle.directory {
            return Err((FX_FAILURE, "not a directory handle"));
        }

        // Заголовок пишется в конце, когда станет известно число записей: длина
        // (4), тип (1), номер (4), счётчик (4).
        const HEAD: usize = 13;
        /// Самая длинная запись: имя, строка `ls -l` с тем же именем и
        /// атрибуты. Запись, взятая у ядра, обязана поместиться: вернуть её
        /// обратно в каталог нельзя, и не поместившаяся пропала бы из списка.
        const WORST_ENTRY: usize = 4 + MAX_NAME + 4 + (MAX_NAME + 80) + 32;
        /// Сколько записей в одном ответе. Предел не про место, а про время:
        /// каталог в тысячи имён отдаётся кусками, и клиент показывает ход.
        const BATCH: u32 = 100;

        let mut at = HEAD;
        let mut count = 0u32;
        let mut name = [0u8; MAX_NAME];
        while count < BATCH && at + WORST_ENTRY <= reply.len() {
            let (len, meta) = match self.fs.next_entry(handle.file, &mut name) {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => return Err(code_of(err)),
            };
            let len = len.min(MAX_NAME);
            let written = {
                let mut w = Writer::new(&mut reply[at..]);
                w.string(&name[..len]);
                let mut long = [0u8; MAX_NAME + 80];
                let long_len = long_name(&mut long, &name[..len], &meta);
                w.string(&long[..long_len]);
                write_attrs(&mut w, &meta);
                if !w.ok() {
                    return Err((FX_FAILURE, "listing does not fit"));
                }
                w.len()
            };
            at += written;
            count += 1;
        }
        if count == 0 {
            return Err((FX_EOF, "end of directory"));
        }
        let body = at - 4;
        reply[..4].copy_from_slice(&(body as u32).to_be_bytes());
        reply[4] = FXP_NAME;
        reply[5..9].copy_from_slice(&id.to_be_bytes());
        reply[9..13].copy_from_slice(&count.to_be_bytes());
        Ok(at)
    }

    fn remove(&mut self, reader: &mut Reader<'_>, directory: bool) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let path = self.path(raw)?;
        // Ядро удаляет одним вызовом и файл, и пустой каталог; протокол
        // различает их, и клиент, попросивший `rm`, не должен снести каталог.
        let meta = match self.fs.stat(path.as_str()) {
            Ok(meta) => meta,
            Err(err) => return Err(code_of(err)),
        };
        if meta.directory != directory {
            return Err((
                FX_FAILURE,
                if directory { "not a directory" } else { "is a directory" },
            ));
        }
        match self.fs.remove(path.as_str()) {
            Ok(()) => {
                let what = if directory { "directory " } else { "file " };
                self.fs.log(&["sftp-server: removed ", what, path.as_str()]);
                Ok(0)
            }
            Err(err) => Err(self.refuse("remove", &path, err)),
        }
    }

    fn mkdir(&mut self, reader: &mut Reader<'_>) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let attrs = read_attrs(reader).ok_or(BAD)?;
        let path = self.path(raw)?;
        let mode = attrs.permissions.map_or(0o755, |mode| mode & 0o777);
        match self.fs.mkdir(path.as_str(), mode) {
            Ok(()) => {
                self.fs.log(&["sftp-server: made directory ", path.as_str()]);
                Ok(0)
            }
            Err(err) => Err(self.refuse("mkdir", &path, err)),
        }
    }

    fn realpath(&mut self, reader: &mut Reader<'_>, id: u32, reply: &mut [u8]) -> Outcome {
        let raw = reader.string().ok_or(BAD)?;
        let path = self.path(raw)?;
        Ok(frame(reply, |w| {
            w.byte(FXP_NAME);
            w.u32(id);
            w.u32(1);
            w.string(path.as_str().as_bytes());
            // Строка `ls -l` для `REALPATH` не определена; OpenSSH шлёт то же
            // имя, и клиенты её не читают.
            w.string(path.as_str().as_bytes());
            w.u32(0);
        }))
    }

    fn rename(&mut self, reader: &mut Reader<'_>) -> Outcome {
        let old = reader.string().ok_or(BAD)?;
        let new = reader.string().ok_or(BAD)?;
        let old = self.path(old)?;
        let new = self.path(new)?;
        match self.fs.rename(old.as_str(), new.as_str()) {
            Ok(()) => {
                self.fs.log(&["sftp-server: renamed ", old.as_str(), " to ", new.as_str()]);
                Ok(0)
            }
            // Третья версия требует отказа, если имя занято, — ровно так
            // ведёт себя и ext2 этой системы, поэтому здесь нет второго
            // правила поверх первого.
            Err(err) => Err(self.refuse("rename", &old, err)),
        }
    }

    // --- мелочи ----------------------------------------------------------

    fn path(&self, raw: &[u8]) -> Result<PathBuf, (u32, &'static str)> {
        resolve(self.home.as_str(), raw).ok_or((FX_FAILURE, "bad or too long path"))
    }

    fn free_slot(&self) -> Result<usize, (u32, &'static str)> {
        self.handles
            .iter()
            .position(Option::is_none)
            .ok_or((FX_FAILURE, "too many open files"))
    }

    fn next_generation(&mut self) -> u32 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Разобрать дескриптор из запроса и найти его место в таблице.
    ///
    /// Дескриптор — четыре байта: номер места и номер выдачи. Всё прочее —
    /// чужая выдумка, и ответ на неё один: такого дескриптора нет.
    fn slot(&self, reader: &mut Reader<'_>) -> Result<usize, (u32, &'static str)> {
        let raw = reader.string().ok_or(BAD)?;
        if raw.len() != 4 {
            return Err(BAD_HANDLE);
        }
        let slot = usize::from(raw[0]);
        let generation = u32::from_be_bytes([0, raw[1], raw[2], raw[3]]);
        match self.handles.get(slot) {
            Some(Some(handle)) if handle.generation & 0x00FF_FFFF == generation => Ok(slot),
            _ => Err(BAD_HANDLE),
        }
    }

    /// Отказ файловой системы: в журнал и в ответ.
    fn refuse(&mut self, what: &str, path: &PathBuf, err: FsError) -> (u32, &'static str) {
        let (code, text) = code_of(err);
        self.fs.log(&["sftp-server: ", what, " ", path.as_str(), " refused: ", text]);
        (code, text)
    }
}

const BAD: (u32, &str) = (FX_BAD_MESSAGE, "malformed request");
const BAD_HANDLE: (u32, &str) = (FX_FAILURE, "no such handle");

/// Отказ ядра в словах протокола.
fn code_of(err: FsError) -> (u32, &'static str) {
    match err {
        FsError::NotFound => (FX_NO_SUCH_FILE, "no such file"),
        FsError::Permission => (FX_PERMISSION_DENIED, "permission denied"),
        FsError::Exists => (FX_FAILURE, "already exists"),
        FsError::NotEmpty => (FX_FAILURE, "directory not empty"),
        FsError::NoSpace => (FX_FAILURE, "no space left"),
        FsError::Unsupported => (FX_OP_UNSUPPORTED, "not supported by the filesystem"),
        FsError::WrongKind => (FX_FAILURE, "wrong kind of file"),
        FsError::TooManyFiles => (FX_FAILURE, "too many open files"),
        FsError::Io => (FX_FAILURE, "input/output error"),
    }
}

/// Атрибуты из запроса. Нужны только права; остальное читается, чтобы
/// сдвинуться дальше.
struct Attrs {
    flags: u32,
    permissions: Option<u32>,
}

fn read_attrs(reader: &mut Reader<'_>) -> Option<Attrs> {
    let flags = reader.u32()?;
    if flags & ATTR_SIZE != 0 {
        read_u64(reader)?;
    }
    if flags & ATTR_UIDGID != 0 {
        reader.u32()?;
        reader.u32()?;
    }
    let permissions = if flags & ATTR_PERMISSIONS != 0 { Some(reader.u32()?) } else { None };
    if flags & ATTR_ACMODTIME != 0 {
        reader.u32()?;
        reader.u32()?;
    }
    if flags & ATTR_EXTENDED != 0 {
        // Число пар приходит с провода; каждая пара — две строки, то есть не
        // меньше восьми байт. Больше пар, чем осталось байт, быть не может, и
        // цикл по такому числу — это способ занять сервер надолго.
        let count = reader.u32()? as usize;
        if count > reader.remaining() / 8 {
            return None;
        }
        for _ in 0..count {
            reader.string()?;
            reader.string()?;
        }
    }
    Some(Attrs { flags, permissions })
}

fn write_attrs(w: &mut Writer<'_>, meta: &Meta) {
    let mut flags = ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS;
    if meta.mtime.is_some() {
        flags |= ATTR_ACMODTIME;
    }
    w.u32(flags);
    w.u32((meta.size >> 32) as u32);
    w.u32(meta.size as u32);
    w.u32(meta.uid);
    w.u32(meta.gid);
    let kind = if meta.directory { S_IFDIR } else { S_IFREG };
    w.u32(kind | (meta.mode & 0o7777));
    if let Some(mtime) = meta.mtime {
        // Времени доступа система не хранит; OpenSSH в таком случае ставит то
        // же время изменения, и так ведут себя все, у кого `noatime`.
        w.u32(mtime);
        w.u32(mtime);
    }
}

fn read_u64(reader: &mut Reader<'_>) -> Option<u64> {
    let high = u64::from(reader.u32()?);
    let low = u64::from(reader.u32()?);
    Some((high << 32) | low)
}

/// Записать ответ целиком: поле длины и то, что соберёт `body`.
fn frame(reply: &mut [u8], body: impl FnOnce(&mut Writer<'_>)) -> usize {
    let len = {
        let mut w = Writer::new(&mut reply[4..]);
        body(&mut w);
        if !w.ok() {
            return 0;
        }
        w.len()
    };
    reply[..4].copy_from_slice(&(len as u32).to_be_bytes());
    4 + len
}

fn status(reply: &mut [u8], id: u32, code: u32, text: &str) -> usize {
    frame(reply, |w| {
        w.byte(FXP_STATUS);
        w.u32(id);
        w.u32(code);
        w.string(text.as_bytes());
        w.string(b"en");
    })
}

fn handle_reply(reply: &mut [u8], id: u32, slot: usize, generation: u32) -> usize {
    let g = generation.to_be_bytes();
    frame(reply, |w| {
        w.byte(FXP_HANDLE);
        w.u32(id);
        w.string(&[slot as u8, g[1], g[2], g[3]]);
    })
}

/// Число десятичной записью в буфере.
pub fn decimal(value: u64, digits: &mut [u8; 20]) -> &str {
    let mut at = digits.len();
    let mut rest = value;
    loop {
        at -= 1;
        digits[at] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    core::str::from_utf8(&digits[at..]).unwrap_or("?")
}

/// Строка `ls -l` для записи каталога — её показывает `ls -l` клиента OpenSSH.
///
/// Владельцы числами: имён учётных записей сервер не знает, а читать ради них
/// `/etc/passwd` значило бы спросить у ядра ещё один файл на каждую запись.
fn long_name(out: &mut [u8], name: &[u8], meta: &Meta) -> usize {
    let mut w = Writer::new(out);
    let bits = [
        (0o400, b'r'),
        (0o200, b'w'),
        (0o100, b'x'),
        (0o040, b'r'),
        (0o020, b'w'),
        (0o010, b'x'),
        (0o004, b'r'),
        (0o002, b'w'),
        (0o001, b'x'),
    ];
    w.byte(if meta.directory { b'd' } else { b'-' });
    for (bit, letter) in bits {
        w.byte(if meta.mode & bit != 0 { letter } else { b'-' });
    }
    let mut digits = [0u8; 20];
    w.bytes(b"    1 ");
    pad_right(&mut w, decimal(u64::from(meta.uid), &mut digits).as_bytes(), 8);
    w.byte(b' ');
    pad_right(&mut w, decimal(u64::from(meta.gid), &mut digits).as_bytes(), 8);
    w.byte(b' ');
    pad_left(&mut w, decimal(meta.size, &mut digits).as_bytes(), 12);
    w.byte(b' ');
    let (month, day, hour, minute) = civil(meta.mtime.unwrap_or(0));
    const MONTHS: [&[u8; 3]; 12] = [
        b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov",
        b"Dec",
    ];
    w.bytes(MONTHS[(month as usize).saturating_sub(1).min(11)]);
    w.byte(b' ');
    pad_left(&mut w, decimal(u64::from(day), &mut digits).as_bytes(), 2);
    w.byte(b' ');
    w.byte(b'0' + (hour / 10) as u8);
    w.byte(b'0' + (hour % 10) as u8);
    w.byte(b':');
    w.byte(b'0' + (minute / 10) as u8);
    w.byte(b'0' + (minute % 10) as u8);
    w.byte(b' ');
    w.bytes(name);
    if w.ok() { w.len() } else { 0 }
}

fn pad_left(w: &mut Writer<'_>, text: &[u8], width: usize) {
    for _ in text.len()..width {
        w.byte(b' ');
    }
    w.bytes(text);
}

fn pad_right(w: &mut Writer<'_>, text: &[u8], width: usize) {
    w.bytes(text);
    for _ in text.len()..width {
        w.byte(b' ');
    }
}

/// Месяц, день, час и минута по секундам эпохи (UTC).
///
/// Алгоритм Говарда Хиннанта (`civil_from_days`): без таблиц и без циклов по
/// годам, поэтому время на любую дату одно и то же.
fn civil(seconds: u32) -> (u32, u32, u32, u32) {
    let days = i64::from(seconds / 86_400);
    let rest = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (month, day, rest / 3_600, rest / 60 % 60)
}
