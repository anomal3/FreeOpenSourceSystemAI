//! `greet` — программа, которая едет в системе только внутри пакета.
//!
//! В `/bin` её нет и не будет: она попадает в систему исключительно через
//! `pkg install`, и запускается по своему пути в `/opt`. Именно этим она и
//! проверяет фазу — «пакет положил программу, и она работает» нельзя доказать
//! программой, которая и так лежала на диске.
//!
//! Печатает свой путь запуска: он приходит нулевым аргументом, и по нему видно,
//! что исполняется именно распакованный файл, а не одноимённый из `/bin`.

#![no_std]
#![no_main]

use user_abi::ERR_PERMISSION;
use user_progs::{Args, Line, exit, socket};

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let path = args.get(0).unwrap_or("<unknown>");

    Line::new()
        .str("greet: hello from a package, running as ")
        .str(path)
        .end();

    // И то же самое в журнал: окно оболочки снаружи не читается, а утверждение
    // фазы проверяется именно снаружи.
    Line::to_log()
        .str("greet: installed from a package, running as ")
        .str(path)
        .end();

    // Проба сети. Программа её не хочет и не умеет — она существует затем,
    // чтобы показать **отказ**: манифест `hello` не просит ни одного права, а
    // значит система обязана отказать, даже когда запустивший — root.
    //
    // Проверяется именно код отказа, а не «что-то пошло не так»: сокет может
    // не открыться и оттого, что в системе нет сетевой карты, и такой отказ
    // о правах не говорит ничего. Различить их можно только числом.
    let fd = socket();
    let mut line = Line::to_log();
    line.str("greet: opening a socket without asking for it returned ")
        .signed(fd);
    if fd == ERR_PERMISSION {
        line.str(" (refused by the package permissions)");
    } else {
        line.str(" (NOT refused, which means the permissions did nothing)");
    }
    line.end();

    exit(0)
}
