//! Программа, проверяющая, что права на файлы действительно проверяются.
//!
//! Печатает по строке на файл: что ядро о нём сообщило и чем ответило на
//! попытку прочитать. Строки одинакового вида — их читает не только человек, но
//! и стенд (`cargo xtask test`), а утверждение «чужой файл не читается» иначе
//! нечем подтвердить: отсутствие содержимого на экране доказывает лишь то, что
//! программа его не напечатала.
//!
//! # Что именно проверяется
//!
//! Четыре разных ответа, и каждый получен по своей причине:
//!
//! * `/etc/system.cfg` — права `0644`, владелец root: читается, потому что
//!   «остальным» разрешено чтение;
//! * `/etc/passwd` — права `0640`, владелец root: не читается, потому что
//!   «остальным» не разрешено ничего, хотя каталог `/etc` пройти можно;
//! * `<home>/notes.txt` — владелец мы: читается по классу владельца;
//! * `/root/notes.txt` — сам файл `0644`, но каталог `/root` имеет права
//!   `0700` и принадлежит root: не читается, и это главный случай из четырёх.
//!   Он отличает проверку, которая смотрит на файл, от проверки, которая идёт
//!   по пути целиком, — а установщик расставляет права, рассчитывая на вторую.
//!
//! Путь к домашнему каталогу не зашит: имя пользователя выбирают при установке.
//! Программа берёт его из `/etc/system.cfg` — того самого файла, который ей
//! читать разрешено. Это заодно и есть проверка того, что файловые вызовы
//! работают: строка `home=` доехала сюда через `open`, `read` и `close`.

#![no_std]
#![no_main]

use user_abi::{ERR_PERMISSION, KIND_DIRECTORY, Stat};
use user_progs::{Line, close, exit, gid, open, println, read, stat, uid};

/// Сколько байт файла настроек программа согласна прочитать.
const CONFIG_LIMIT: usize = 512;

/// Ключ, за которым в файле настроек стоит домашний каталог.
const HOME_KEY: &[u8] = b"home=";

/// Хватит на `/home/` плюс имя пользователя плюс имя файла.
const PATH_MAX: usize = 96;

/// Имя файла, который установщик кладёт в домашний каталог.
const NOTES: &[u8] = b"/notes.txt";

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Каждая строка собирается целиком и уходит ядру одним вызовом. Иначе её
    // разрывает вывод соседней программы — а `init` в первые секунды после
    // загрузки как раз заводит службы, и стенд запускает эту программу тогда
    // же. Подробности в заголовке [`Line`].
    Line::new()
        .str("perms: uid ")
        .num(u64::from(uid()))
        .str(" gid ")
        .num(u64::from(gid()))
        .end();

    try_path("/etc/system.cfg");
    try_path("/etc/passwd");

    let mut home = [0u8; PATH_MAX];
    match home_notes(&mut home) {
        Some(path) => try_path(path),
        None => println("perms: no home= line in /etc/system.cfg"),
    }

    try_path("/root/notes.txt");

    println("perms: done");
    exit(0)
}

/// Прочитать файл и рассказать, чем это кончилось.
fn try_path(path: &str) {
    // Строка копится здесь и уходит одним вызовом в конце — в каждой ветке
    // своим `end()`.
    let mut line = Line::new();
    line.str("perms: ").str(path).str(": ");

    let mut info = Stat::default();
    let code = stat(path, &mut info);
    if code < 0 {
        report_error(&mut line, code);
        return;
    }

    line.str("mode ")
        .octal(info.mode & 0o7777)
        .str(" owner ")
        .num(u64::from(info.uid))
        .str(":")
        .num(u64::from(info.gid))
        .str(" -> ");

    if info.kind == KIND_DIRECTORY {
        line.str("directory, not read").end();
        return;
    }

    let fd = open(path);
    if fd < 0 {
        report_error(&mut line, fd);
        return;
    }

    // Читается ровно один буфер: программе нужно доказать, что содержимое
    // доехало, а не пересказать файл.
    let mut buffer = [0u8; 64];
    let got = read(fd, &mut buffer);
    close(fd);

    if got < 0 {
        report_error(&mut line, got);
        return;
    }
    line.str("read ").num(got as u64).str(" bytes").end();
}

/// Напечатать причину отказа.
///
/// Отдельно выделен только отказ в правах: остальные коды печатаются числом.
/// Разница не косметическая — «нельзя» и «нет такого» требуют от человека
/// разных действий, и стенд ждёт именно слова.
fn report_error(line: &mut Line, code: i64) {
    if code == ERR_PERMISSION {
        line.str("permission denied").end();
    } else {
        line.str("error ").signed(code).end();
    }
}

/// Собрать путь `<home>/notes.txt`, прочитав домашний каталог из настроек.
fn home_notes(buffer: &mut [u8; PATH_MAX]) -> Option<&str> {
    let fd = open("/etc/system.cfg");
    if fd < 0 {
        return None;
    }
    let mut config = [0u8; CONFIG_LIMIT];
    let got = read(fd, &mut config);
    close(fd);
    if got <= 0 {
        return None;
    }

    let home = find_value(&config[..got as usize], HOME_KEY)?;
    if home.is_empty() || home.len() + NOTES.len() > PATH_MAX {
        return None;
    }
    buffer[..home.len()].copy_from_slice(home);
    buffer[home.len()..home.len() + NOTES.len()].copy_from_slice(NOTES);

    core::str::from_utf8(&buffer[..home.len() + NOTES.len()]).ok()
}

/// Найти значение ключа в файле вида `ключ=значение` по строке на пару.
///
/// Разбор без выделения памяти и без предположений о содержимом: файл прочитан
/// с диска, то есть может быть любым, и единственный вывод из непонятной строки
/// — пропустить её.
fn find_value<'a>(text: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    for line in text.split(|byte| *byte == b'\n') {
        let line = trim(line);
        if line.starts_with(key) {
            return Some(trim(&line[key.len()..]));
        }
    }
    None
}

/// Обрезать пробелы и возвраты каретки по краям.
fn trim(mut bytes: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = bytes {
        if first.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = bytes {
        if last.is_ascii_whitespace() {
            bytes = rest;
        } else {
            break;
        }
    }
    bytes
}
