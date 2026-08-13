//! Перечисление каталога — программой, а не командой ядра.
//!
//! `ls` в оболочке существует с Phase 9b и живёт внутри ядра, потому что до сих
//! пор перечислить каталог снаружи было нечем: `open` на каталоге отказывал, а
//! вызова, который вернул бы имена, в договоре не было. Обе версии печатают
//! одно и то же и остаются рядом намеренно: команда ядра нужна там, где `/bin`
//! ещё не смонтирован — например, когда разбираются, почему он не смонтирован.
//!
//! Разница видна в одном: этой программе никто не даёт доступа к файловой
//! системе. Она видит ровно то, что разрешено учётной записи, от имени которой
//! её запустили, — и отказ в правах приходит ей как ошибка вызова, а не как
//! отсутствующая строка в выводе.

#![no_std]
#![no_main]

use user_progs::{
    Args, Dirent, KIND_DIRECTORY, close, exit, open, print, print_octal, print_u64, println,
    readdir,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    // Без аргумента — корень. Текущего каталога у программы нет: его не
    // существует и в системе, потому что понятия «где я нахожусь» у неё пока
    // нет вовсе, и притворяться, что точка означает корень, было бы враньём.
    let path = args.get(1).unwrap_or("/");

    let fd = open(path);
    if fd < 0 {
        print("ls: cannot open ");
        print(path);
        // Отказ в правах выглядит здесь так же, как отсутствие файла, и это
        // намеренно: ядро не рассказывает программе, чего именно ей не хватило,
        // — иначе перебором ошибок можно было бы узнать содержимое каталога, в
        // который заглядывать не дали.
        println("");
        exit(1);
    }

    let mut entry = Dirent::default();
    let mut files = 0u64;
    let mut directories = 0u64;

    while readdir(fd, &mut entry) {
        let Some(name) = entry.name() else {
            println("ls: the kernel returned a name that is not valid UTF-8");
            continue;
        };

        // Тот же порядок столбцов, что у команды ядра: права, владелец, размер,
        // имя. Совпадение здесь не эстетическое — две версии одного вывода,
        // расходящиеся в мелочах, заставляют читателя гадать, какую он видит.
        print("  ");
        print_octal(entry.mode);
        print(" ");
        print_u64(u64::from(entry.uid));
        print(":");
        print_u64(u64::from(entry.gid));
        print(" ");
        print_u64(entry.size);
        print("  ");
        print(name);
        if entry.kind == KIND_DIRECTORY {
            print("/");
            directories += 1;
        } else {
            files += 1;
        }
        println("");
    }

    close(fd);

    print("ls: ");
    print_u64(files);
    print(" files, ");
    print_u64(directories);
    print(" directories in ");
    println(path);

    exit(0)
}
