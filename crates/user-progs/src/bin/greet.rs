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

use user_progs::{Args, error, exit, print, println};

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let path = args.get(0).unwrap_or("<unknown>");

    print("greet: hello from a package, running as ");
    println(path);

    // И то же самое в журнал: окно оболочки снаружи не читается, а утверждение
    // фазы проверяется именно снаружи.
    error("greet: installed from a package, running as ");
    error(path);
    error("\n");

    exit(0)
}
