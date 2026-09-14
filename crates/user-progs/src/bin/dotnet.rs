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

/// Базовая библиотека своей среды: на неё разрешаются ссылки программы на
/// `System.Runtime` и `System.Console` (фаза N3a).
const CORELIB: &str = "/usr/share/dotnet/FreeOs.CoreLib.dll";

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

/// Стек среды: мегабайт вместо обычных 64 КиБ.
///
/// Загрузка типа в `clr-vm` рекурсивна по цепочке баз и полей-структур, по
/// 3–5 КиБ на уровень, и образец `objects` фазы N3a съедал около 90 КиБ —
/// `/bin/dotnet` снимался ядром на первой же сборке. Иерархии WinForms
/// (`Form` → … → `Object`, семь уровней) глубже. Мегабайт — с запасом на них и
/// на кадры AArch64, а тест `samples_fit_in_the_user_stack` в `clr-vm` держит
/// образцы в четверти этого.
const DOTNET_STACK_BYTES: usize = 1024 * 1024;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // Первым делом, до любого выделения: и размер кучи, и место стека зависят
    // от того, что ещё ничего не выделено (см. `run_on_own_stack`).
    heap_size(HEAP_BYTES);
    let start = (argc, argv);
    let code = user_progs::run_on_own_stack(DOTNET_STACK_BYTES, run, core::ptr::from_ref(&start) as usize);
    error(&format!("dotnet: error: no memory for a {DOTNET_STACK_BYTES}-byte stack (code {code})\n"));
    exit(1)
}

/// Всё остальное — уже на стеке среды.
extern "C" fn run(start: usize) -> ! {
    // SAFETY: `start` — адрес пары в кадре `_start`, который не вернётся
    // никогда, так что пара жива до конца программы.
    let (argc, argv) = unsafe { *(start as *const (usize, *const *const u8)) };

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

    let corelib = match load(CORELIB) {
        Ok(corelib) => corelib,
        Err(text) => {
            error(&format!("dotnet: error: {CORELIB}: {text}\n"));
            exit(1)
        }
    };

    let mut vm = match Vm::new(&data, &corelib, Console) {
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
                "dotnet: {name}: Main returned {code} after {} instruction(s), {} object(s), {} collection(s)\n",
                vm.instructions,
                vm.object_count(),
                vm.collections()
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
