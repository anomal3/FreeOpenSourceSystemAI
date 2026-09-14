//! `/bin/dotnet` — своя среда .NET (веха v0.7c, фаза N2).
//!
//! `dotnet /usr/share/dotnet/samples/hello.dll` запускает сборку, собранную
//! обычным `dotnet build` на Windows, без переделки. Программа — обёртка:
//! прочитать файл, отдать его интерпретатору (`clr-vm`), напечатать итог. Всё
//! исполнение в крейте, и тот же код в `cargo xtask clr-check` сравнивается с
//! настоящим `dotnet` на тех же сборках.
//!
//! # Что пишет в журнал
//!
//! Дескриптор 2: `dotnet: <файл>: N bytes, T type(s), M method(s)` при загрузке
//! и `dotnet: <файл>: Main returned C after K instruction(s), O object(s)` по
//! завершении — по этим строкам стенд и проверяет, что программа не просто
//! что-то напечатала, а дошла до конца. Отказ среды — `dotnet: error: …` с
//! полным именем того, чего не хватило.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use clr_vm::{Host, Vm};
use user_progs::{Args, close, error, exit, file_size, heap_size, open, print, read};

/// Куча программы. Сборка, разобранные методы, кадры и все объекты программы
/// живут в ней; мегабайта, который достаётся остальным программам, не хватает.
const HEAP_BYTES: usize = 16 * 1024 * 1024;

/// Самая большая сборка, которую программа читает.
const FILE_MAX: usize = 4 * 1024 * 1024;

/// Код выхода, когда среда не довела программу до конца.
///
/// 134 — «прервано», как у процесса, получившего `SIGABRT`: ноль и единица
/// заняты кодами, которые программа вправе вернуть сама.
const RUNTIME_FAILED: i64 = 134;

/// Стандартный вывод программы — это терминал.
struct Console;

impl Host for Console {
    fn write_out(&mut self, text: &str) {
        print(text);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // Первым делом, до любого выделения: размер кучи задаётся один раз.
    heap_size(HEAP_BYTES);

    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let Some(path) = args.get(1) else {
        error("usage: dotnet <assembly.dll> [arguments]\n");
        exit(2)
    };
    let name = path.rsplit('/').next().unwrap_or(path);

    let data = match load(path) {
        Ok(data) => data,
        Err(text) => {
            error(&format!("dotnet: error: {path}: {text}\n"));
            exit(1)
        }
    };

    let mut program_args = Vec::new();
    let mut index = 2;
    while let Some(arg) = args.get(index) {
        program_args.push(arg);
        index += 1;
    }

    let mut vm = match Vm::new(&data, Console) {
        Ok(vm) => vm,
        Err(failure) => {
            error(&format!("dotnet: error: {name}: {failure}\n"));
            exit(1)
        }
    };
    error(&format!(
        "dotnet: {name}: {} bytes, {} type(s), {} method(s)\n",
        data.len(),
        vm.type_count(),
        vm.method_count()
    ));

    match vm.run_main(&program_args) {
        Ok(code) => {
            error(&format!(
                "dotnet: {name}: Main returned {code} after {} instruction(s), {} object(s)\n",
                vm.instructions,
                vm.object_count()
            ));
            exit(i64::from(code))
        }
        Err(failure) => {
            error(&format!("dotnet: error: {name}: {failure}\n"));
            exit(RUNTIME_FAILED)
        }
    }
}

/// Прочитать сборку целиком.
fn load(path: &str) -> Result<Vec<u8>, String> {
    let fd = open(path);
    if fd < 0 {
        return Err(format!("cannot open (code {fd})"));
    }
    let size = file_size(fd);
    if size < 0 {
        close(fd);
        return Err(format!("cannot read the size (code {size})"));
    }
    let size = size as usize;
    if size > FILE_MAX {
        close(fd);
        return Err(format!("{size} bytes is more than the {FILE_MAX} this runtime reads"));
    }
    let mut data = Vec::new();
    if data.try_reserve_exact(size).is_err() {
        close(fd);
        return Err(String::from("out of memory"));
    }
    data.resize(size, 0);
    let mut filled = 0;
    while filled < size {
        let got = read(fd, &mut data[filled..]);
        if got <= 0 {
            break;
        }
        filled += got as usize;
    }
    close(fd);
    if filled != size {
        return Err(format!("read {filled} of {size} bytes"));
    }
    Ok(data)
}
