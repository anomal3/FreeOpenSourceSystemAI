//! `sftp-server` — файлы туда и обратно штатным клиентом (фаза 38c).
//!
//! # Как он запускается
//!
//! Не человеком. `sshd`, получив в канале запрос `subsystem sftp`, запускает
//! эту программу **от имени вошедшего** — с `uid`/`gid` из `/etc/passwd` — и
//! связывает её двумя каналами: в стандартный ввод идут байты клиента, из
//! стандартного вывода забираются ответы. Единственный аргумент — домашний
//! каталог: от него считаются относительные пути, и с него клиент начинает
//! (`REALPATH "."`).
//!
//! # Почему отдельной программой, а не внутри `sshd`
//!
//! Дорожная карта сначала предлагала обратное — подсистему внутри сервера. Не
//! вышло бы, и причина та же, по которой фаза 38b выселила из `sshd` `ls` и
//! `cat`: сервер исполняется от root, а системного вызова «работай дальше от
//! имени такого-то» в ядре нет. Внутри `sshd` каждое `open` клиента проверялось
//! бы по правам root, и честным это можно было бы сделать только второй
//! проверкой прав в самом сервере — той самой, которую 38b убрала.
//!
//! Отдельная задача от имени вошедшего даёт это даром: каждый системный вызов
//! здесь спрашивает ядро, тем же кодом, что за терминалом, и отказ записи в
//! `/etc` — это отказ ядра. Вторая выгода не меньше первой: разбор пакетов,
//! которые прислал кто угодно, идёт **не** в процессе root. Ошибка в разборе
//! роняет программу с правами одного человека, а не сервер, держащий ключ
//! машины.
//!
//! Сам протокол — в крейте `ssh` (`ssh::sftp`), где его проверяют на хосте;
//! здесь только системные вызовы и цикл чтения.

#![no_std]
#![no_main]

use ssh::sftp::{self, FsError, MAX_NAME, MAX_MESSAGE, Meta, REPLY_BUFFER, Server};
use user_abi::{
    Dirent, ERR_EXISTS, ERR_IO, ERR_NOT_EMPTY, ERR_NOT_FOUND, ERR_NO_SPACE, ERR_PERMISSION,
    ERR_TOO_MANY_FILES, ERR_UNSUPPORTED, FD_STDIN, FD_STDOUT, KIND_DIRECTORY, SEEK_END,
    SEEK_SET, Stat,
};
use user_progs::{
    Args, close, create, error, exit, fstat, gid, mkdir, open, open_write, read, readdir_raw,
    remove, rename, seek, stat, uid, write,
};

/// Сколько байт входа держится разом: одно сообщение наибольшего размера и
/// хвост следующего, пришедший тем же чтением.
const INPUT: usize = 4 + MAX_MESSAGE + 4096;

/// Буфер входа. Статический, а не на стеке: стек программы — 64 КиБ на всё, и
/// два буфера по 32 КиБ в одном кадре — это способ узнать про охранную страницу.
static mut INPUT_BUFFER: [u8; INPUT] = [0u8; INPUT];
/// Буфер ответа.
static mut REPLY: [u8; REPLY_BUFFER] = [0u8; REPLY_BUFFER];

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра ровно в том виде, в каком их описывает
    // договор: массив из `argc` строк, завершённых нулём, в стеке программы.
    let args = unsafe { Args::new(argc, argv) };
    let Some(home) = args.get(1) else {
        error("sftp-server: started without a home directory; sshd starts this, not a person\n");
        exit(2);
    };

    // Кто мы — в журнал первой строкой: весь смысл программы в том, что это
    // **не** root, и стенд проверяет именно это.
    let mut digits = [0u8; 20];
    error("sftp-server: session for uid ");
    error(sftp::decimal(u64::from(uid()), &mut digits));
    error(" gid ");
    error(sftp::decimal(u64::from(gid()), &mut digits));
    error(" in ");
    error(home);
    error("\n");

    let mut server = Server::new(Kernel, home);
    // SAFETY: программа однопоточная, оба буфера используются только здесь и
    // последовательно; ссылки живут до `exit`.
    let input = unsafe { &mut *(&raw mut INPUT_BUFFER) };
    let reply = unsafe { &mut *(&raw mut REPLY) };
    let mut filled = 0usize;

    let code = loop {
        // Все целые сообщения, что уже лежат в буфере, — до следующего чтения:
        // клиент OpenSSH шлёт запросы пачкой, не дожидаясь ответов, и одно
        // чтение приносит их несколько.
        let mut at = 0usize;
        let mut broken = false;
        while let Some(size) = sftp::frame_len(&input[at..filled]) {
            let Ok(size) = size else {
                // Длина за пределом: следующего сообщения не найти, не
                // поверив числу, которому верить нельзя. OpenSSH на это тоже
                // заканчивает сеанс.
                error("sftp-server: the client sent a message longer than allowed; ending\n");
                broken = true;
                break;
            };
            if filled - at < size {
                break;
            }
            let len = server.handle(&input[at + 4..at + size], reply);
            if len > 0 && !send(&reply[..len]) {
                error("sftp-server: nobody reads the replies any more; ending\n");
                broken = true;
                break;
            }
            at += size;
        }
        if broken {
            break 1;
        }
        // Необработанный хвост — к началу буфера.
        input.copy_within(at..filled, 0);
        filled -= at;

        // Ввод — канал от `sshd`, и чтение из него **ждёт**: программе нечего
        // делать, пока клиент молчит, и крутиться в цикле ей незачем.
        let got = read(FD_STDIN as i64, &mut input[filled..]);
        if got == 0 {
            // Конец ввода: клиент закрыл канал. Это нормальный конец сеанса.
            break 0;
        }
        if got < 0 {
            error("sftp-server: reading the channel failed\n");
            break 1;
        }
        filled += got as usize;
    };

    server.close_all();
    let totals = server.totals;
    error("sftp-server: session over: ");
    error(sftp::decimal(totals.requests, &mut digits));
    error(" request(s), ");
    error(sftp::decimal(totals.bytes_written, &mut digits));
    error(" byte(s) written, ");
    error(sftp::decimal(totals.bytes_read, &mut digits));
    error(" byte(s) read, ");
    error(sftp::decimal(totals.errors, &mut digits));
    error(" error reply(ies)\n");
    exit(code)
}

/// Отдать ответ целиком. `false` — читателя больше нет.
///
/// Запись в канал принимает столько, сколько в нём места, — остаток надо
/// дописать. Ответ, отправленный наполовину, клиент не разберёт, а следующий
/// за ним разберёт как продолжение первого.
fn send(mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let written = write(FD_STDOUT as i64, data);
        if written <= 0 {
            return false;
        }
        data = &data[(written as usize).min(data.len())..];
    }
    true
}

/// Файловая система — это ядро, от имени того, кто нас запустил.
struct Kernel;

/// Отказ ядра в словах протокола.
fn fs_error(code: i64) -> FsError {
    match code {
        ERR_NOT_FOUND => FsError::NotFound,
        ERR_PERMISSION => FsError::Permission,
        ERR_EXISTS => FsError::Exists,
        ERR_NOT_EMPTY => FsError::NotEmpty,
        ERR_NO_SPACE => FsError::NoSpace,
        ERR_UNSUPPORTED => FsError::Unsupported,
        ERR_TOO_MANY_FILES => FsError::TooManyFiles,
        ERR_IO => FsError::Io,
        // Остальные коды (плохой адрес, плохой путь, нет файловой системы)
        // программе исправить нечем, и клиенту они говорят одно: не вышло.
        _ => FsError::Io,
    }
}

fn check(code: i64) -> Result<i64, FsError> {
    if code < 0 { Err(fs_error(code)) } else { Ok(code) }
}

fn meta_of(info: &Stat) -> Meta {
    Meta {
        size: info.size,
        mode: info.mode & 0o7777,
        uid: info.uid,
        gid: info.gid,
        directory: info.kind == KIND_DIRECTORY,
        mtime: None,
    }
}

impl sftp::Fs for Kernel {
    fn open_read(&mut self, path: &str) -> Result<i64, FsError> {
        check(open(path))
    }

    fn open_write(&mut self, path: &str, truncate: bool) -> Result<i64, FsError> {
        check(open_write(path, false, truncate))
    }

    fn create(&mut self, path: &str, mode: u32) -> Result<i64, FsError> {
        check(create(path, (mode & 0o777) as u16))
    }

    fn read_at(&mut self, file: i64, offset: u64, buffer: &mut [u8]) -> Result<usize, FsError> {
        let offset = i64::try_from(offset).map_err(|_| FsError::Io)?;
        check(seek(file, offset, SEEK_SET))?;
        // Ядро отдаёт меньше, чем просили, только у конца файла; но кусок,
        // отданный пополам, протокол тоже разрешает, так что цикла не нужно.
        check(read(file, buffer)).map(|got| got as usize)
    }

    fn write_at(&mut self, file: i64, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let offset = i64::try_from(offset).map_err(|_| FsError::Io)?;
        check(seek(file, offset, SEEK_SET))?;
        check(write(file, data)).map(|done| done as usize)
    }

    fn size(&mut self, file: i64) -> Result<u64, FsError> {
        check(seek(file, 0, SEEK_END)).map(|end| end as u64)
    }

    fn fstat(&mut self, file: i64) -> Result<Meta, FsError> {
        let mut info = Stat::default();
        check(fstat(file, &mut info))?;
        Ok(meta_of(&info))
    }

    fn stat(&mut self, path: &str) -> Result<Meta, FsError> {
        let mut info = Stat::default();
        check(stat(path, &mut info))?;
        Ok(meta_of(&info))
    }

    fn next_entry(
        &mut self,
        dir: i64,
        name: &mut [u8; MAX_NAME],
    ) -> Result<Option<(usize, Meta)>, FsError> {
        let mut entry = Dirent::default();
        match check(readdir_raw(dir, &mut entry))? {
            0 => Ok(None),
            _ => {
                // Длина имени пришла от ядра; проверяется всё равно — копировать
                // по числу, не сверенному с буфером, нельзя ни от кого.
                let len = (entry.name_len as usize).min(MAX_NAME);
                name[..len].copy_from_slice(&entry.name[..len]);
                let meta = Meta {
                    size: entry.size,
                    mode: entry.mode & 0o7777,
                    uid: entry.uid,
                    gid: entry.gid,
                    directory: entry.kind == KIND_DIRECTORY,
                    mtime: (entry.mtime != 0).then_some(entry.mtime),
                };
                Ok(Some((len, meta)))
            }
        }
    }

    fn close(&mut self, file: i64) {
        close(file);
    }

    fn mkdir(&mut self, path: &str, mode: u32) -> Result<(), FsError> {
        check(mkdir(path, mode & 0o777)).map(|_| ())
    }

    fn remove(&mut self, path: &str) -> Result<(), FsError> {
        check(remove(path)).map(|_| ())
    }

    fn rename(&mut self, old: &str, new: &str) -> Result<(), FsError> {
        check(rename(old, new)).map(|_| ())
    }

    fn log(&mut self, parts: &[&str]) {
        // Пути в строке прислал клиент. Управляющий байт, доехавший до
        // терминала человека, читающего журнал, — это уже не диагностика, а
        // команда его терминалу; поэтому всё, кроме печатного ASCII, выходит
        // знаком вопроса.
        let mut staged = [0u8; 128];
        let mut len = 0usize;
        for byte in parts.iter().flat_map(|part| part.bytes()) {
            if len == staged.len() {
                // SAFETY: в буфер попадает только печатный ASCII.
                error(unsafe { core::str::from_utf8_unchecked(&staged[..len]) });
                len = 0;
            }
            staged[len] = if byte.is_ascii_graphic() || byte == b' ' { byte } else { b'?' };
            len += 1;
        }
        // SAFETY: то же.
        error(unsafe { core::str::from_utf8_unchecked(&staged[..len]) });
        error("\n");
    }
}
