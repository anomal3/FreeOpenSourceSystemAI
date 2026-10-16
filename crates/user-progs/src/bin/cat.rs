//! `cat`: выложить файл в стандартный вывод.
//!
//! # Почему она появилась только сейчас
//!
//! Потому что до фазы 38b её негде было применить. Оболочка умеет печатать
//! файл сама, а сеть до программ не доходила: `sshd` носил внутри себя
//! собственный `cat` — со второй в системе проверкой прав. Каналы убрали
//! причину: теперь по сети запускается **эта** программа, от имени вошедшего, и
//! права спрашивает ядро при `open`, а не сервер по своим таблицам.
//!
//! # Байты, а не текст
//!
//! Читает и пишет как есть, без разбора UTF-8. Файл может быть чем угодно, а
//! программа, решившая, что его содержимое обязано быть строкой, отказывает
//! ровно там, где `cat` нужнее всего, — на файле, в который надо заглянуть.
//!
//! Без аргумента читает стандартный ввод: так `cat` работает везде, и так он
//! годится серединой конвейера.

#![no_std]
#![no_main]

use user_progs::{
    Args, ERR_BROKEN_PIPE, ERR_NOT_FOUND, ERR_PERMISSION, FD_STDIN, Line, close, exit, open,
    println, read, write,
};
use user_abi::FD_STDOUT;

/// Сколько байт читается за раз.
///
/// Полкилобайта: столько же берёт `wc`, и это заметно меньше страницы канала —
/// то есть кусок доезжает целиком, не заставляя читателя собирать его из
/// половинок.
const CHUNK: usize = 512;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра ровно в том виде, в каком их описывает
    // договор: массив из `argc` строк, завершённых нулём, в стеке программы.
    let args = unsafe { Args::new(argc, argv) };

    let (fd, opened) = match args.get(1) {
        Some(path) => {
            let fd = open(path);
            if fd < 0 {
                // Причина называется словами, потому что ядро их различает:
                // «нет такого файла» и «читать его вам нельзя» — разные ответы
                // и лечатся по-разному. Всё остальное печатается числом: врать
                // про причину хуже, чем назвать код.
                let mut line = Line::new();
                line.str("cat: ").str(path);
                match fd {
                    ERR_PERMISSION => line.str(": permission denied"),
                    ERR_NOT_FOUND => line.str(": no such file"),
                    other => line.str(": error ").signed(other),
                };
                line.end();
                exit(1);
            }
            (fd, true)
        }
        None => (FD_STDIN as i64, false),
    };

    let mut buffer = [0u8; CHUNK];
    let mut code = 0;
    loop {
        let got = read(fd, &mut buffer);
        if got < 0 {
            println("cat: read failed");
            code = 1;
            break;
        }
        if got == 0 {
            break;
        }
        // Записанное считается, и это не педантизм: канал принимает столько,
        // сколько у него места, и остаток надо дописать. Программа, поверившая,
        // что `write` записал всё, теряет вывод молча — ровно посередине
        // большого файла.
        let mut sent = 0usize;
        while sent < got as usize {
            let written = write(FD_STDOUT as i64, &buffer[sent..got as usize]);
            if written == ERR_BROKEN_PIPE {
                // Тот, для кого мы печатали, ушёл. Это не ошибка программы:
                // так заканчивается всякий `cat`, чей читатель насчитал своё.
                exit(0);
            }
            if written <= 0 {
                println("cat: write failed");
                code = 1;
                break;
            }
            sent += written as usize;
        }
        if code != 0 {
            break;
        }
    }

    if opened {
        close(fd);
    }
    exit(code)
}
