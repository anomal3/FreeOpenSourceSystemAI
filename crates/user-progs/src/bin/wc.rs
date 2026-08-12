//! Счётчик строк, слов и байтов — первая программа, которой сказали, что делать.
//!
//! До этой фазы программа не могла получить от запустившего её ни слова: путь к
//! файлу пришлось бы зашить в неё саму. Здесь путь приходит аргументом, а
//! размер файла берётся `seek`-ом к концу — то есть не чтением всего
//! содержимого ради одного числа.
//!
//! Печатает три числа и путь, как это делает `wc` в Unix. Сходство не ради
//! сходства: формат, который человек уже знает, не нужно объяснять, а стенду
//! всё равно, что искать.

#![no_std]
#![no_main]

use user_progs::{
    Args, SEEK_SET, close, exit, file_size, open, print, print_u64, println, read, seek,
};

/// Сколько байт читается за раз.
const CHUNK: usize = 512;

/// Точка входа. Два аргумента — то, что кладёт в регистры ядро перед возвратом
/// в третье кольцо: сколько аргументов и где они лежат.
#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра ровно в том виде, в каком их описывает
    // договор: массив из `argc` строк, завершённых нулём, в стеке этой
    // программы.
    let args = unsafe { Args::new(argc, argv) };

    let Some(path) = args.get(1) else {
        println("usage: wc <path>");
        exit(2);
    };

    let fd = open(path);
    if fd < 0 {
        print("wc: cannot open ");
        print(path);
        println("");
        exit(1);
    }

    // Размер — одним `seek`, без чтения. Это и есть разница, которую фаза
    // принесла: раньше единственным способом узнать длину файла было прочитать
    // его целиком.
    let size = file_size(fd);
    if size < 0 {
        println("wc: cannot measure the file");
        close(fd);
        exit(1);
    }

    // Позиция ставится в начало явно, а не подразумевается: `file_size` её
    // возвращает на место, но полагаться на чужую аккуратность в программе,
    // которая печатает числа, не стоит.
    seek(fd, 0, SEEK_SET);

    let mut lines = 0u64;
    let mut words = 0u64;
    let mut bytes = 0u64;
    let mut in_word = false;
    let mut buffer = [0u8; CHUNK];

    loop {
        let got = read(fd, &mut buffer);
        if got < 0 {
            println("wc: read failed");
            close(fd);
            exit(1);
        }
        if got == 0 {
            break;
        }

        for &byte in &buffer[..got as usize] {
            bytes += 1;
            if byte == b'\n' {
                lines += 1;
            }
            // Слово — это последовательность непробельных байт. Пробельными
            // считаются те же четыре, что и в Unix, а не «всё, что меньше 0x21»:
            // второе посчитало бы управляющие байты двоичного файла границами
            // слов и выдало бы правдоподобную чепуху.
            let space = matches!(byte, b' ' | b'\t' | b'\n' | b'\r');
            if space {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
    }

    close(fd);

    print("wc: ");
    print_u64(lines);
    print(" lines, ");
    print_u64(words);
    print(" words, ");
    print_u64(bytes);
    print(" bytes in ");
    println(path);

    // Размер, измеренный `seek`-ом, обязан совпасть с числом прочитанных байт.
    // Проверяет это сама программа, а не стенд: два числа получены разными
    // путями, и расхождение между ними — единственный признак того, что `seek`
    // врёт.
    if size as u64 == bytes {
        println("wc: size from seek matches the bytes read");
    } else {
        print("wc: MISMATCH, seek says ");
        print_u64(size as u64);
        println("");
        exit(1);
    }

    exit(0)
}
