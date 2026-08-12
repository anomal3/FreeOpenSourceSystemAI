//! Программа, которая создаёт файл и читает его обратно.
//!
//! Проверяет она не файловую систему — ту проверяет `cargo test` крейта `ext2`
//! чужим читателем. Здесь проверяется **граница**: что запись доступна из
//! третьего кольца, через системные вызовы, от имени сеанса и с его правами.
//! Оболочка пишет тот же файл из кольца ноль, и разница между этими двумя
//! путями — ровно то, ради чего в системе есть кольца.

#![no_std]
#![no_main]

use user_progs::{
    close, exit, open_write, print, print_i64, println, read, remove, sleep_ms, write,
};

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
        print("save: cannot create the file: ");
        print_i64(fd);
        println("");
        exit(1)
    }

    let written = write(fd, TEXT.as_bytes());
    close(fd);
    if written != TEXT.len() as i64 {
        print("save: short write: ");
        print_i64(written);
        println("");
        exit(1)
    }
    print("save: wrote ");
    print_i64(written);
    println(" bytes");

    // Пауза перед чтением не нужна ядру — она нужна проверке: между записью и
    // чтением файл успевает пережить смену задачи, то есть читается он с
    // носителя, а не из того, что осталось в памяти после записи.
    sleep_ms(50);

    let fd = user_progs::open(PATH);
    let mut buffer = [0u8; 64];
    let read = read(fd, &mut buffer);
    close(fd);
    if read < 0 {
        print("save: cannot read it back: ");
        print_i64(read);
        println("");
        exit(1)
    }

    // SAFETY: прочитано ровно то, что программа сама и записала, — а записывала
    // она строку.
    let text = unsafe { core::str::from_utf8_unchecked(&buffer[..read as usize]) };
    print("save: read back: ");
    print(text);
    println("save: done");
    exit(0)
}
