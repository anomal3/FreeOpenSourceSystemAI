//! Программа, которая создаёт файл и читает его обратно.
//!
//! Проверяет она не файловую систему — ту проверяет `cargo test` крейта `ext2`
//! чужим читателем. Здесь проверяется **граница**: что запись доступна из
//! третьего кольца, через системные вызовы, от имени сеанса и с его правами.
//! Оболочка пишет тот же файл из кольца ноль, и разница между этими двумя
//! путями — ровно то, ради чего в системе есть кольца.

#![no_std]
#![no_main]

use user_progs::{Line, close, exit, open_write, read, remove, sleep_ms, write};

/// Куда писать. Каталог принадлежит пользователю сеанса — тому самому, от чьего
/// имени установщик его и создал.
const PATH: &str = "/home/roman/from-a-program.txt";

/// Что писать.
const TEXT: &str = "written from ring 3\n";

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Файла может не быть — тогда `remove` вернёт ошибку, и это не беда:
    // программа обязана работать и на чистой системе, и на той, где её уже
    // запускали.
    let _ = remove(PATH);

    let fd = open_write(PATH, true, true);
    if fd < 0 {
        Line::new()
            .str("save: cannot create the file: ")
            .signed(fd)
            .end();
        exit(1)
    }

    let written = write(fd, TEXT.as_bytes());
    close(fd);
    if written != TEXT.len() as i64 {
        Line::new().str("save: short write: ").signed(written).end();
        exit(1)
    }
    Line::new()
        .str("save: wrote ")
        .signed(written)
        .str(" bytes")
        .end();

    // Пауза перед чтением не нужна ядру — она нужна проверке: между записью и
    // чтением файл успевает пережить смену задачи, то есть читается он с
    // носителя, а не из того, что осталось в памяти после записи.
    sleep_ms(50);

    let fd = user_progs::open(PATH);
    let mut buffer = [0u8; 64];
    let read = read(fd, &mut buffer);
    close(fd);
    if read < 0 {
        Line::new()
            .str("save: cannot read it back: ")
            .signed(read)
            .end();
        exit(1)
    }

    // SAFETY: прочитано ровно то, что программа сама и записала, — а записывала
    // она строку.
    let text = unsafe { core::str::from_utf8_unchecked(&buffer[..read as usize]) };
    // Прочитанный текст кончается переводом строки, так что это две строки
    // журнала, отданные одним вызовом.
    Line::new()
        .str("save: read back: ")
        .str(text)
        .str("save: done")
        .end();
    exit(0)
}
