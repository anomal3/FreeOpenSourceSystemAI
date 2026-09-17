//! Тесты своей среды без .NET на машине.
//!
//! Главная проверка — `cargo xtask clr-check`, где вывод сравнивается с
//! настоящим `dotnet` на свежесобранных образцах. Здесь — то же сравнение на
//! сборках, уже лежащих в репозитории (с выводом, записанным из настоящего
//! `dotnet`), и правила ECMA-335, которые легко нарушить незаметно.

use std::string::String;

use crate::ops::{self, Fault};
use crate::sandbox::Sandbox;
use crate::{Value, Vm, VmError};

/// Базовая библиотека своей среды — та же сборка, что едет в образ.
const CORELIB: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/FreeOs.CoreLib.dll");
/// Шаблон `dotnet new console`.
const HELLO: &[u8] = include_bytes!("../../clr-meta/fixtures/hello.dll");
/// Образец `tools/dotnet/samples/objects` (фаза N3a).
const OBJECTS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/objects.dll");

/// Что печатает `dotnet objects.dll` (записано 2026-09-14, .NET 10, LF).
const OBJECTS_OUTPUT: &str = concat!(
    "objects: start\n",
    "Rex says woof\n",
    "little Bim says yip\n",
    "Cat says meow\n",
    "Animal Rex\n",
    "hidden cat\n",
    "Animal Cat\n",
    "created: 3\n",
    "age: 2\n",
    "is Dog: True\n",
    "is Cat: False\n",
    "as Animal: True\n",
    "puppy: yip\n",
    "type: Puppy / FreeOs.Samples.Objects.Puppy\n",
    "counter at 2\n",
    "after reset: 0\n",
    "step: 2\n",
    "base next: 3\n",
    "is IResettable: True\n",
    "before Registry\n",
    "Registry: static constructor\n",
    "registry: 102\n",
    "before Lazy\n",
    "Lazy: static constructor\n",
    "Lazy: instance constructor\n",
    "hello from Lazy\n",
    "lazy is True\n",
    "a = (1, 2), b = (11, 12), sum 23\n",
    "points: (0, 0) (5, 5) (7, 0)\n",
    "array keeps (5, 5), copy (105, 105)\n",
    "holder: (50, 3) line (3, 3)-(4, 4)\n",
    "narrow: 0 32767\n",
    "plain: FreeOs.Samples.Objects.Plain\n",
    "boxed (1, 2), moved (2, 3), unboxed (1, 2)\n",
    "number 42 back 43 is int: True\n",
    "boxed equals: True True False\n",
    "level: 200 True null slot True\n",
    "primes: 129\n",
    "раздватри 3\n",
    "same: True False\n",
    "objects: done\n",
);
/// Образец `tools/dotnet/samples/arith` — тот же файл, что едет в образ.
const ARITH: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/arith.dll");

/// Что печатает `dotnet arith.dll` (записано 2026-09-14, .NET 10, переводы строк
/// приведены к LF).
///
/// Собрано из отдельных строк, а не одним литералом: в `switch: zero one ` в
/// конце пробел («zero » + «one »), и литерал с концом строки его прячет от
/// глаза и от редактора, который обрезает хвостовые пробелы.
const ARITH_OUTPUT: &str = concat!(
    "arith: start\n",
    "sum 1..10 = 55\n",
    "fib(20) = 6765\n",
    "fact(20) = 2432902008176640000\n",
    "gcd(1071, 462) = 21\n",
    "-17 / 5 = -3, -17 % 5 = -2\n",
    "uint: 4294967279\n",
    "uint >> 28: 15\n",
    "1L << 40 = 1099511627776\n",
    "-64 >> 3 = -8\n",
    "bool: True, char: Z\n",
    "switch: zero one \n",
    "switch: two many\n",
    "negative\n",
    "zero\n",
    "positive\n",
    "Привет из IL\n",
    "arith: done\n",
);

/// Запустить образец с файлами в своём пустом каталоге (фаза N5a): тесты идут
/// параллельно, и общий каталог они делили бы между собой.
fn run(data: &[u8]) -> (Result<i32, VmError>, String) {
    run_with(data, "", &[])
}

/// То же с путём сборки и аргументами командной строки (фаза N5b).
fn run_with(data: &[u8], program: &str, args: &[&str]) -> (Result<i32, VmError>, String) {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(std::format!("clr-vm-test-{}-{number}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("sandbox directory");
    let mut vm = Vm::new(data, CORELIB, Sandbox::new(&dir)).expect("assembly parses");
    vm.set_program_path(program);
    let result = vm.run_main(args);
    let output = vm.into_host().output;
    let _ = std::fs::remove_dir_all(&dir);
    (result, output)
}

/// Сколько стека получает поток теста, КиБ.
///
/// Тесты на машине разработчика идут на восьми мегабайтах, а `/bin/dotnet` —
/// на стеке в мегабайт, взятом у ядра (`DOTNET_STACK_BYTES`). Разница однажды
/// уже спрятала дефект: на обычных для программ 64 КиБ среда фазы N3a падала,
/// пока все тесты были зелёными (замер: `objects` — около 90 КиБ на хосте,
/// рекурсия загрузки типов по 3–5 КиБ на уровень). Четверть мегабайта — запас
/// на кадры AArch64, которые крупнее, и на глубокие иерархии вроде WinForms.
/// `CLR_STACK_KIB` меняет размер для замеров.
const TEST_STACK_KIB: usize = 256;

#[test]
fn samples_fit_in_the_user_stack() {
    let kib = std::env::var("CLR_STACK_KIB").ok().and_then(|v| v.parse().ok()).unwrap_or(TEST_STACK_KIB);
    for (name, data) in
        [("hello", HELLO), ("arith", ARITH), ("objects", OBJECTS), ("exceptions", EXCEPTIONS), ("generics", GENERICS), ("gc", GC), ("text", TEXT), ("collections", COLLECTIONS), ("floats", FLOATS), ("enums", ENUMS), ("linq", LINQ), ("files", FILES), ("time", TIME), ("form", FORM), ("winforms", WINFORMS), ("controls", CONTROLS), ("lists", LISTS), ("dialogs", DIALOGS), ("layout", LAYOUT), ("choices", CHOICES), ("tabs", TABS), ("numbers", NUMBERS), ("keys", KEYS), ("pqueue", PQUEUE), ("sorted", SORTED), ("nullable", NULLABLE), ("listsort", LISTSORT), ("dict", DICT), ("drawing", DRAWING)]
    {
        let worker = std::thread::Builder::new()
            .stack_size(kib * 1024)
            .spawn(move || run(data).0.map(|_| ()).map_err(|error| std::format!("{error}")))
            .expect("thread starts");
        // Переполнение стека обрывает весь процесс теста — это и есть провал.
        let result = worker.join().expect("thread finishes");
        assert!(result.is_ok(), "{name} in {kib} KiB: {result:?}");
    }
}

/// Образец `tools/dotnet/samples/exceptions` (фаза N3b).
const EXCEPTIONS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/exceptions.dll");

/// Что печатает `dotnet exceptions.dll` (записано 2026-09-14, .NET 10, LF).
/// `filter deep` раньше `unwind 1` — два прохода поиска обработчика.
const EXCEPTIONS_OUTPUT: &str = concat!(
    "exceptions: start\n",
    "caught simple code 3\n",
    "filter deep\n",
    "unwind 1\n",
    "unwind 2\n",
    "unwind 3\n",
    "deep: bottom code 7\n",
    "filter no\n",
    "fell through to AppError\n",
    "finally before return\n",
    "finally returns 1\n",
    "inner finally\n",
    "outer caught inner\n",
    "nested 110\n",
    "null: Object reference not set to an instance of an object.\n",
    "index: Index was outside the bounds of the array.\n",
    "divide: Attempted to divide by zero.\n",
    "cast failed\n",
    "overflow: Arithmetic operation resulted in an overflow.\n",
    "log again\n",
    "rethrown again\n",
    "open file\n",
    "close file\n",
    "argument: Value cannot be null. (Parameter 'path')\n",
    "finally throws\n",
    "got second\n",
    "FreeOs.Samples.Exceptions.AppError: not thrown\n",
    "Exception of type 'System.Exception' was thrown.\n",
    "loop 43\n",
    "exceptions: done\n",
);

#[test]
fn exceptions_print_what_dotnet_prints() {
    let (result, output) = run(EXCEPTIONS);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, EXCEPTIONS_OUTPUT);
    assert_eq!(code, 43);
}

/// Образец `tools/dotnet/samples/generics` (фаза N3c).
const GENERICS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/generics.dll");

/// Что печатает `dotnet generics.dll` (записано 2026-09-14, .NET 10, LF).
const GENERICS_OUTPUT: &str = concat!(
    "generics: start\n",
    "stacks: 25 4 два один\n",
    "empty peek: 0 True\n",
    "pair (7, семь) swapped (семь, 7)\n",
    "Tally<Int32> ready\n",
    "Tally<String> ready\n",
    "tally 2 10\n",
    "max 9 pear -5\n",
    "area 13 26\n",
    "delegates 5 1005 42 3\n",
    "multicast 22\n",
    "clicked OK\n",
    "clicks 1\n",
    "interpolated: привет, мир! 3 x (7, семь) = 21\n",
    "generics: done\n",
);

#[test]
fn generics_print_what_dotnet_prints() {
    let (result, output) = run(GENERICS);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, GENERICS_OUTPUT);
    assert_eq!(code, 22);
}

/// Образец `tools/dotnet/samples/gc` (фаза N3d).
const GC: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/gc.dll");

/// Что печатает `dotnet gc.dll` (записано 2026-09-14, .NET 10, LF).
const GC_OUTPUT: &str = concat!(
    "gc: start\n",
    "checksum 5886420\n",
    "chain 6 sum 7500\n",
    "cells cell 500 / cell 2500 weight 2500\n",
    "closure 2501 local -1\n",
    "литерал живёт\n",
    "gc: done\n",
);

/// Мусор собран, а живое уцелело: вывод совпадает с dotnet (каждое живое
/// значение программа проверяет сама), и сборок было больше нуля.
#[test]
fn garbage_is_collected_and_the_living_survive() {
    let mut vm = Vm::new(GC, CORELIB, Sandbox::new(std::env::temp_dir())).expect("assembly parses");
    let result = vm.run_main(&[]);
    let collections = vm.collections();
    let output = vm.into_host().output;
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, GC_OUTPUT);
    assert_eq!(code, 6);
    assert!(collections > 0, "the sample allocates ~70 MiB and never collected");
}

/// Образец `tools/dotnet/samples/text` (фаза N4a).
const TEXT: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/text.dll");

/// Что печатает `dotnet text.dll` в режиме инвариантной глобализации
/// (записано 2026-09-14, .NET 10, LF). В `y  |` и `[   7|8   ]` пробелы —
/// выравнивание, а не хвост строки.
const TEXT_OUTPUT: &str = concat!(
    "text: start\n",
    "[Hello, FreeOS world] 19 21 21\n",
    "HELLO, FREEOS WORLD hello, freeos world ПРИВЕТ, МИР\n",
    "FreeOS world|Hello|7|4|15|-1\n",
    "True True True False\n",
    "4 [a|b||c] abc\n",
    "aXYcaXYc heLLo ...x y  |\n",
    "------ok 3 li__ne rve\n",
    "True True -1 1\n",
    "2 + 3 = 5 [   7|8   ]\n",
    "   42|-7  |FF|00ff|003|-0012|-2147483648|18446744073709551615\n",
    "000000FF 1000 -005 c8\n",
    ">>Y=10,True -3 14 =\n",
    "ok 2\n",
    "-123 False 0 9000000000 True 77\n",
    "format: The input string 'x' was not in a correct format.\n",
    "7 -2 9 10 -1\n",
    "True True Ж 65 True True\n",
    "text: done\n",
);

#[test]
fn text_prints_what_dotnet_prints() {
    let (result, output) = run(TEXT);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, TEXT_OUTPUT);
    assert_eq!(code, 77);
}

/// Образец `tools/dotnet/samples/collections` (фаза N4b).
const COLLECTIONS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/collections.dll");

/// Что печатает `dotnet collections.dll` (записано 2026-09-14, .NET 10, LF).
/// Порядок в `dict` и `set` — порядок записей: место удалённого занял новый.
const COLLECTIONS_OUTPUT: &str = concat!(
    "collections: start\n",
    "list 5,8,1 count 3 has 8 True at 1\n",
    "sorted 8,7,1 sum 16 array 3\n",
    "names apple fig pear kiwi lime kiwi 1 True\n",
    "cells R1C2 True -1\n",
    "dict ann=31 dan=19 cid=41 count 3 False True 41\n",
    "missing: The given key 'zed' was not present in the dictionary.\n",
    "keys ann,dan,cid values 31,19,41\n",
    "struct key far class key 1 False\n",
    "set 16,1,9 count 3 added False has 9 True\n",
    "queue ab2 stack 211\n",
    "array 1,2,3 sum 6 index 1 ilist 33\n",
    "evens 0,2,4,6,8 sum 2550\n",
    "word alpha\n",
    "word beta\n",
    "words: finally\n",
    "collections: done\n",
);

#[test]
fn collections_print_what_dotnet_prints() {
    let (result, output) = run(COLLECTIONS);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, COLLECTIONS_OUTPUT);
    assert_eq!(code, 6);
}

/// Образец `tools/dotnet/samples/floats` (фаза N4c).
const FLOATS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/floats.dll");

/// Что печатает `dotnet floats.dll` (записано 2026-09-14, .NET 10, LF,
/// инвариантная глобализация). `¤` — знак валюты инвариантной культуры.
const FLOATS_OUTPUT: &str = concat!(
    "floats: start\n",
    "0.30000000000000004\n",
    "0.3333333333333333 0.6666666666666666 1E+21 1E-07 123.456 -0 100\n",
    "1.7976931348623157E+308 5E-324 NaN Infinity -Infinity\n",
    "3.14|2.7183|12,345.68|25.6 %|1.234E+003|(\u{a4}42.50)|2|4|0.12\n",
    "1,234,567.89 5E-1 (3.8) zero 2.68 2.67\n",
    "[     1.414] [1.23E+03] [1.5]\n",
    "1,234,567|-5.00|4.20E+001|700 %|\u{a4}255.00|12,345.00|neg|zero|00012\n",
    "0.33333334 16777216 16777216 3.4028235E+38 1 True 0.3333333432674408 0.1\n",
    "-123450 True Infinity False 0 3.14159 NaN -Infinity\n",
    "The input string 'abc' was not in a correct format.\n",
    "1.4142135623730951 -3 -2 -2 2 4 -2 3 1024\n",
    "2.35 3 -1.2 0 NaN -0 -1 5\n",
    "0.8414709848 0.5000000000 0.5463024898 2.3561944902 2.7182818285 2.3025850930 0.3010299957 1.4142135624 1.4142135\n",
    "3/3/3 -3/-3/0 2147483647/10000000000/4294967295 0/0/0 -1/-1/0 3.5 2 -1.5\n",
    "NaN -Infinity -1 0 2.25 3.5 True False True -1 1074266112 0\n",
    "3 2 2\n",
    "1.6439345666815615 3.1406380562059946 3FFA4D8E550A946E\n",
    "1.25 2.5 -1E-10\n",
    "floats: done\n",
);

#[test]
fn floats_print_what_dotnet_prints() {
    let (result, output) = run(FLOATS);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, FLOATS_OUTPUT);
    assert_eq!(code, 41);
}

/// Образец `tools/dotnet/samples/enums` (фаза N4d).
const ENUMS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/enums.dll");

/// Что печатает `dotnet enums.dll` (записано 2026-09-14, .NET 10, LF).
const ENUMS_OUTPUT: &str = concat!(
    "enums: start\n",
    "Blue Red Green 42 6\n",
    "Read, Execute | ReadWrite | All | None | 8 | 9\n",
    "0 Bold, Italic 8 High 7 Min Negative -5\n",
    "Blue 6 Blue 00000006 Read, Execute 5 C8 FF Bold, Italic\n",
    "Green ReadWrite False Red True Blue Blue High\n",
    "Red,Green,Blue None,Read,Write,ReadWrite,Execute,All True False Green\n",
    "True False True 1 True Color FreeOs.Samples.Enums.Color\n",
    "Requested value 'nope' was not found.\n",
    "3 False Green True cold warm\n",
    "enums: done\n",
);

#[test]
fn enums_print_what_dotnet_prints() {
    let (result, output) = run(ENUMS);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, ENUMS_OUTPUT);
    assert_eq!(code, 6);
}

/// Образец `tools/dotnet/samples/linq` (фаза N4d).
const LINQ: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/linq.dll");

/// Что печатает `dotnet linq.dll` (записано 2026-09-14, .NET 10, LF).
/// `OrderBy` устойчив: Bob раньше Eve, Ann раньше Cid.
const LINQ_OUTPUT: &str = concat!(
    "linq: start\n",
    "64,4,16,36,0 45 9 0 4.5 5\n",
    "0,1,2 2,1,0 5 0 0 True True False\n",
    "Dan Bob Eve Ann Cid | Cid Ann Eve Bob Dan\n",
    "Oslo: 2 Ann+Cid avg 31\n",
    "Rome: 2 Bob+Dan avg 22\n",
    "Kyiv: 1 Eve avg 25\n",
    "31 131 31 26.2 Dan Bob Dan\n",
    "EVE,CID,BOB,ANN 2,0,1 120 ababab\n",
    "Ann5,Bob3,Cid8,Dan1,Eve9 15 0,6 True 5,3,6,0\n",
    "two,four 6 x,y 0Ann,1Bob,2Cid,3Dan,4Eve\n",
    "0 4 10 True 13\n",
    "Sequence contains no elements\n",
    "Sequence contains more than one matching element\n",
    "4000000 1000000 8496120 1229 234 3999\n",
    "linq: done\n",
);

#[test]
fn linq_prints_what_dotnet_prints() {
    let (result, output) = run(LINQ);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, LINQ_OUTPUT);
    assert_eq!(code, 29);
}

#[test]
fn objects_print_what_dotnet_prints() {
    let (result, output) = run(OBJECTS);
    // Сначала ошибка среды: по одному обрезанному выводу не видно, где встала.
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, OBJECTS_OUTPUT);
    assert_eq!(code, 3);
}

#[test]
fn hello_world() {
    let (result, output) = run(HELLO);
    assert_eq!(result.expect("runs"), 0);
    assert_eq!(output, "Hello, World!\n");
}

#[test]
fn arith_prints_what_dotnet_prints() {
    let (result, output) = run(ARITH);
    assert_eq!(output, ARITH_OUTPUT);
    assert_eq!(result.expect("runs"), 0);
}

#[test]
fn integer_division_faults_like_dotnet() {
    assert_eq!(ops::binary(0x5B, Value::I32(1), Value::I32(0)), Err(Fault::DivideByZero));
    // `int.MinValue / -1` и `% -1` — исключение, а не перенос.
    assert_eq!(ops::binary(0x5B, Value::I32(i32::MIN), Value::I32(-1)), Err(Fault::Overflow));
    assert_eq!(ops::binary(0x5D, Value::I32(i32::MIN), Value::I32(-1)), Err(Fault::Overflow));
    assert_eq!(ops::binary(0x5C, Value::I32(-1), Value::I32(2)), Ok(Value::I32(i32::MAX)));
    assert_eq!(ops::binary(0x5D, Value::I32(-17), Value::I32(5)), Ok(Value::I32(-2)));
    assert_eq!(ops::binary(0xD6, Value::I32(i32::MAX), Value::I32(1)), Err(Fault::Overflow));
}

#[test]
fn conversions_follow_the_standard() {
    // `conv.u8` от `int32 -1` расширяет нулями, `conv.i8` — знаком.
    assert_eq!(ops::convert(0x6E, Value::I32(-1)), Ok(Value::I64(0xFFFF_FFFF)));
    assert_eq!(ops::convert(0x6A, Value::I32(-1)), Ok(Value::I64(-1)));
    assert_eq!(ops::convert(0x67, Value::I32(0xFF)), Ok(Value::I32(-1)));
    assert_eq!(ops::convert(0xD2, Value::I32(0x1FF)), Ok(Value::I32(0xFF)));
    // Проверяющие варианты: знаковый и беззнаковый источник.
    assert_eq!(ops::convert(0xB4, Value::I32(256)), Err(Fault::Overflow));
    assert_eq!(ops::convert(0x86, Value::I32(-1)), Err(Fault::Overflow));
    assert_eq!(ops::convert(0xB4, Value::I32(255)), Ok(Value::I32(255)));
    // Дробное в целое насыщает, NaN — ноль.
    assert_eq!(ops::convert(0x69, Value::F(3.9e10)), Ok(Value::I32(i32::MAX)));
    assert_eq!(ops::convert(0x69, Value::F(f64::NAN)), Ok(Value::I32(0)));
    assert_eq!(ops::convert(0x76, Value::I32(-1)), Ok(Value::F(4_294_967_295.0)));
}

#[test]
fn unordered_branches_take_nan() {
    let c = ops::compare(Value::F(f64::NAN), Value::F(1.0)).expect("comparable");
    assert!(!c.greater_or_equal());
    assert!(c.greater_or_equal_unordered());
    assert!(!c.equal);
    let ints = ops::compare(Value::I32(-1), Value::I32(1)).expect("comparable");
    assert!(ints.less());
    assert!(ints.greater_unordered(), "-1 как uint32 больше 1");
}

#[test]
fn shifts_mask_the_count() {
    assert_eq!(ops::shift(0x64, Value::I32(-17), Value::I32(28)), Ok(Value::I32(15)));
    assert_eq!(ops::shift(0x62, Value::I32(1), Value::I32(33)), Ok(Value::I32(2)));
    assert_eq!(ops::shift(0x63, Value::I32(-64), Value::I32(3)), Ok(Value::I32(-8)));
}

/// Образец `tools/dotnet/samples/files` (фаза N5a).
const FILES: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/files.dll");

/// Что печатает `dotnet files.dll`, запущенный в пустом каталоге (записано
/// 2026-09-14, .NET 10, LF).
const FILES_OUTPUT: &str = concat!(
    "files: start\n",
    "False False\n",
    "old True True\n",
    "31 3 [first line|вторая строка|third] 43 208 вторая 43\n",
    "5 250 255 True\n",
    "log 1: level=info\n",
    "log 2: 42\n",
    "log 3: appended\n",
    "end True\n",
    "3 appended\n",
    "a.md,data.bin,notes.txt | notes.txt | logs | b.txt,notes.txt | app.log,b.txt,old\n",
    "False 31\n",
    "copy over: IOException\n",
    "# a False\n",
    "read: FileNotFoundException missing.txt\n",
    "write: DirectoryNotFoundException\n",
    "rmdir: IOException\n",
    "False app.log,b.txt,moved.txt\n",
    "file.tar.gz file.tar .gz report.md False [] False\n",
    "notes.txt .txt True 43 False n5-files 2\n",
    "files: done\n",
);

#[test]
fn files_print_what_dotnet_prints() {
    let (result, output) = run(FILES);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, FILES_OUTPUT);
    assert_eq!(code, 13);
}

/// Образец `tools/dotnet/samples/time` (фаза N5b).
const TIME: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/time.dll");

/// Что печатает `dotnet time.dll alpha два` (записано 2026-09-15, .NET 10, LF).
/// Строки про текущее время, сон и машину — проверки, верные везде.
const TIME_OUTPUT: &str = concat!(
    "time: start\n",
    "639250167071230000 2026-9-14 Monday 257 Unspecified\n",
    "21:5:7.123 21:05:07.1230000 2026-09-14T00:00:00\n",
    "09/14/2026 21:05:07\n",
    "d: 09/14/2026\n",
    "D: Monday, 14 September 2026\n",
    "f: Monday, 14 September 2026 21:05\n",
    "F: Monday, 14 September 2026 21:05:07\n",
    "g: 09/14/2026 21:05\n",
    "G: 09/14/2026 21:05:07\n",
    "m: September 14\n",
    "o: 2026-09-14T21:05:07.1230000\n",
    "r: Mon, 14 Sep 2026 21:05:07 GMT\n",
    "s: 2026-09-14T21:05:07\n",
    "t: 21:05\n",
    "T: 21:05:07\n",
    "u: 2026-09-14 21:05:07Z\n",
    "y: 2026 September\n",
    "Monday, 14 September 2026 at 9:05 PM | 26/9/14 21:05:07.123\n",
    "Mon Sep 14 1200 12 A.D. hey q 21 | 09:05:07 A 02026\n",
    "21:05             2026-09-14 005 05 5 1 02\n",
    "2026-09-14T21:05:07.1230000Z Z|u|r\n",
    "2024-02-29 2025-02-28 2026-12-24T09:05:07.1230000 2026-09-13T17:35:07.3730005\n",
    "False True True 28 29 Saturday\n",
    "108.02:54:52.8770000 21:00:07 True True 1 False\n",
    "0001-01-01T00:00:00.0000000 3155378975999999999 9999-12-31T23:59:59.9999999\n",
    "2026-09-14T21:05:07.1234567 2026-09-14T21:05:00.0000000 2026-09-14T00:00:00.0000000\n",
    "2026-09-14T07:05:00.0000000 False 0 True Monday, 14 September 2026\n",
    "ctor: ArgumentOutOfRangeException\n",
    "parse: FormatException\n",
    "1.02:03:04.5670000 937845670000 1 2 3 4 567 26.051268611111112 1563.0761166666666 93784567\n",
    "1:2:03:04.567 1:02:03:04.5670000 -1.02:03:04.5670000 1.02:03:04.567 2 -1:2:03:04.567\n",
    "00:01:30.5000000 00:00:01.5000000 -00:02:15 00:00:00 10675199.02:48:05.4775807 -10675199.02:48:05.4775808 1.12:00:00 2.00:00:00\n",
    "1.02:03:04.5000000 -00:00:30 12:34:00 False 5.00:00:00 03:04:00\n",
    "1.03:03:04.5670000 -1.21:56:55.4330000 2.04:06:09.1340000 06:30:46.1417500 26.051268611111112 1.02:03:04.5670000 True 0 True\n",
    "span: OverflowException 0\n",
    "now: True Utc Local True True Local True True Utc\n",
    "slept: True True True True True False True True True\n",
    "reset: 00:00:00 False\n",
    "restart: True\n",
    "sleep: ArgumentOutOfRangeException\n",
    "args: 2 [alpha|два]\n",
    "command line: 3 time.dll alpha|два\n",
    "machine: True True\n",
    "time: done\n",
);

#[test]
fn time_prints_what_dotnet_prints() {
    let (result, output) = run_with(TIME, "time.dll", &["alpha", "два"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, TIME_OUTPUT);
    // `Environment.Exit(args.Length + 19)` мимо `finally` и `return 99`.
    assert_eq!(code, 21);
}

/// Образец `tools/dotnet/samples/form` (фаза N6a): окно WinForms.
const FORM: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/form.dll");

/// Что печатает `dotnet form.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): форма перерисовывается в `Update()` и закрывается.
const FORM_OUTPUT: &str = concat!(
    "form: start\n",
    "{X=10,Y=20,Width=100,Height=50} 110 70 True False {X=5,Y=15,Width=110,Height=60} {X=60,Y=20,Width=50,Height=20} False\n",
    "{X=3,Y=4} {X=13,Y=24} {Width=7, Height=8} {Width=8, Height=20} {X=1.5, Y=2} True {X=1,Y=2}\n",
    "Color [SteelBlue] -12156236 70,130,180 True Color [A=128, R=10, G=20, B=30] 128 800a141e False True True White Color [Control]\n",
    "ctor: 480x320 'FreeOS Form' DemoForm False Color [A=255, R=240, G=244, B=248] Segoe UI 9 Regular {X=0,Y=0,Width=480,Height=320}\n",
    "load: {Width=480, Height=320}\n",
    "shown: True\n",
    "paint 1: {X=0,Y=0,Width=480,Height=320}\n",
    "update: 1 paint(s)\n",
    "closing: UserClosing False\n",
    "closed: UserClosing\n",
    "form: Run returned, disposed True\n",
);

#[test]
fn form_prints_what_winforms_prints() {
    let (result, output) = run_with(FORM, "form.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, FORM_OUTPUT);
    assert_eq!(code, 17);
}

/// Образец `tools/dotnet/samples/winforms` (фаза N6b): шаблон `dotnet new winforms`
/// с кнопкой и надписью из дизайнера.
const WINFORMS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/winforms.dll");

/// Что печатает `dotnet winforms.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): форма дважды нажимает кнопку и закрывается.
const WINFORMS_OUTPUT: &str = concat!(
    "shown: Form1 | Click me | label1 | 2 True 0 True Form1\n",
    "button1: Clicked 1 time\n",
    "button1: Clicked 2 times\n",
    "closed: UserClosing Clicked 2 times\n",
);

#[test]
fn winforms_template_prints_what_winforms_prints() {
    let (result, output) = run_with(WINFORMS, "winforms.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, WINFORMS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/controls` (фаза N7a): поле ввода, флажок и
/// надпись из дизайнера.
const CONTROLS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/controls.dll");

/// Что печатает `dotnet controls.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF).
const CONTROLS_OUTPUT: &str = concat!(
    "shown: 0 32767 False False False Unchecked False True 0\n",
    "text: hello | False Unchecked | 1\n",
    "text: hello world | False Unchecked | 2\n",
    "caret: 11 0 11\n",
    "selected: ell 1 3\n",
    "text: abcdefgh | False Unchecked | 3\n",
    "text:  | False Unchecked | 4\n",
    "checked:  | True Checked | 4\n",
    "state: Checked\n",
    "state: Indeterminate\n",
    "checked:  | False Unchecked | 4\n",
    "state: Unchecked\n",
    "label:  / Unchecked\n",
);

#[test]
fn controls_print_what_winforms_prints() {
    let (result, output) = run_with(CONTROLS, "controls.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, CONTROLS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/lists` (фаза N7b): список и выпадающий список
/// из дизайнера.
const LISTS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/lists.dll");

/// Что печатает `dotnet lists.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): выбор при вставке, удалении и сортировке строк.
const LISTS_OUTPUT: &str = concat!(
    "shown: 3 -1 One False | 3 -1 DropDownList ''\n",
    "list: 1 Oslo | Moscow,Oslo,Paris\n",
    "after insert: 2 Oslo\n",
    "find: 3 2 -1 4 False\n",
    "list: -1 null | Berlin,Moscow,Paris,Tokyo\n",
    "after remove: -1\n",
    "list: 2 Paris | Berlin,Moscow,Paris,Tokyo\n",
    "sorted: Berlin,Moscow,Paris,Tokyo 2\n",
    "list: -1 null | Moscow,Paris,Tokyo\n",
    "combo: 2 Large 'Large' | Small,Medium,Large\n",
    "combo: 0 Small 'Small' | Small,Medium,Large\n",
    "combo text: 'Small' 0\n",
    "cleared: -1 '' 3\n",
    "range: ArgumentOutOfRangeException\n",
);

#[test]
fn lists_print_what_winforms_prints() {
    let (result, output) = run_with(LISTS, "lists.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, LISTS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/dialogs` (фаза N7c): таймер из дизайнера.
/// Окна сообщений самопроверка не открывает — под Windows модальное окно
/// ждало бы человека; их нажимает стенд.
const DIALOGS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/dialogs.dll");

/// Что печатает `dotnet dialogs.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): три тика, остановка и закрытие из обработчика.
const DIALOGS_OUTPUT: &str = concat!(
    "shown: False 50 null None YesNoCancel 6 Exclamation 48\n",
    "started: True\n",
    "tick 1 True 50\n",
    "tick 2 True 50\n",
    "tick 3 True 50\n",
    "stopped: False\n",
    "restart and disable: False 3\n",
    "closed: UserClosing 3\n",
);

#[test]
fn dialogs_print_what_winforms_prints() {
    let (result, output) = run_with(DIALOGS, "dialogs.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, DIALOGS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/layout` (фаза N7d): меню, Dock и Anchor из
/// дизайнера. Высоты меню и поля ввода зависят от шрифта, и образец печатает
/// расстояния и равенства, а не их.
const LAYOUT: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/layout.dll");

/// Что печатает `dotnet layout.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): раскладка до и после роста формы, пункты меню,
/// панель у правого края и скрытая.
const LAYOUT_OUTPUT: &str = concat!(
    "shown: panel1 0 100 True True | panel2 100 300 True | label 0 400 24 True | menu 0 400 | text 12 12 276 | button 12 12 100 28\n",
    "menu: Top 2 &File 3 True O, Control True False ToolStripSeparator Bottom, Right Fill\n",
    "grown: 500x300 panel1 0 100 True True | panel2 100 400 True | label 0 500 24 True | menu 0 500 | text 12 12 376 | button 12 12 100 28\n",
    "open: ToolStripMenuItem &Open\n",
    "wrap: True Checked\n",
    "wrap: False Unchecked\n",
    "right: 400 100 0 400\n",
    "hidden: 0 500 476\n",
    "shrunk: panel1 300 100 True True | panel2 0 300 True | label 0 400 24 True | menu 0 400 | text 12 12 276 | button -88 -38 100 28\n",
    "exit\n",
    "closed: UserClosing\n",
);

#[test]
fn layout_prints_what_winforms_prints() {
    let (result, output) = run_with(LAYOUT, "layout.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, LAYOUT_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/choices` (фаза N7e): переключатели в рамке,
/// полоса хода и ползунок из дизайнера.
const CHOICES: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/choices.dll");

/// Что печатает `dotnet choices.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): порядок событий переключателей, края полосы хода,
/// сужение диапазона ползунка без события.
const CHOICES_OUTPUT: &str = concat!(
    "shown: True False False | tabstop True False False | group Size 3 False | progress 0 100 30 25 Blocks | track 0 10 3 1 5 1 Horizontal BottomRight\n",
    "radio: Small False | False True False | tabstop False True False\n",
    "radio: Medium True | False True False | tabstop False True False\n",
    "radio: Medium False | False False True | tabstop False False True\n",
    "radio: Large True | False False True | tabstop False False True\n",
    "radio: Large False | False False False | tabstop False False False\n",
    "none: False False False | tabstop False False False\n",
    "radio: Small True | True False False | tabstop True False False\n",
    "step: 55\n",
    "increment: 100\n",
    "decrement: 0\n",
    "range: ArgumentOutOfRangeException value 0\n",
    "track: 7\n",
    "max: 5 5 70\n",
    "min: 6 6 6\n",
    "track range: value 6\n",
    "progress max: 50 50\n",
    "closed: UserClosing Level 7\n",
);

#[test]
fn choices_print_what_winforms_prints() {
    let (result, output) = run_with(CHOICES, "choices.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, CHOICES_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/tabs` (фаза N7f): вкладки, строка состояния и
/// подсказки из дизайнера.
const TABS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/tabs.dll");

/// Что печатает `dotnet tabs.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): выбор и видимость страниц, удаление выбранной,
/// строка состояния и подсказки.
const TABS_OUTPUT: &str = concat!(
    "shown: 2 0 General | General:True Options:False | True 3 Top Normal\n",
    "status: Bottom 1 Ready False True\n",
    "tips: 'The first page' 'Turns it on' '' True 500 5000 500 100 False\n",
    "selected: 2 1 Options | General:False Options:True\n",
    "selected: 2 0 General | General:True Options:False\n",
    "checked: True\n",
    "selected: 2 1 Options | General:False Options:True\n",
    "selected: 1 0 General | General:True\n",
    "removed: 1 0 General | General:True | True\n",
    "added: 3 0 General | General:True Extra:False Options:False Extra 0\n",
    "selected: 3 2 Options | General:False Extra:False Options:True\n",
    "range: ArgumentOutOfRangeException\n",
    "tips now: '' 'Pages'\n",
    "status now: 2 ToolStripStatusLabel More Page Options\n",
    "closed: UserClosing Page Options\n",
);

#[test]
fn tabs_print_what_winforms_prints() {
    let (result, output) = run_with(TABS, "tabs.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, TABS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/numbers` (фаза N7g): поля со стрелками из
/// дизайнера и арифметика `decimal`.
const NUMBERS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/numbers.dll");

/// Что печатает `dotnet numbers.dll self-test` (записано 2026-09-15, .NET 10,
/// WinForms на Windows, LF): порядок ValueChanged и Text, края, форматы поля и
/// `decimal` — деление до 28 знаков, округление к чётному, разбор и форматы.
const NUMBERS_OUTPUT: &str = concat!(
    "shown: 10 5 50 1 '10' | 1.5 0.25 '1.50' 2 False True True False Left\n",
    "value: 11 '10'\n",
    "value: 12 '11'\n",
    "value: 50 '12'\n",
    "value: 49 '50'\n",
    "range: value 49\n",
    "value: 20 '49'\n",
    "max: 20 20\n",
    "value: 30 '20'\n",
    "min: 30 30 30\n",
    "second: 1.75 '1.75'\n",
    "thousands: 1234.5 '1,234.50'\n",
    "hex: '4D2'\n",
    "math: 3.35 1.15 -1.15 2.475 2.0454545454545454545454545455 0.3333333333333333333333333333 2.5 -1.1 0.05\n",
    "compare: True True True 1 2.25 1.10 1.10\n",
    "round: 2.2 2.4 2 4 -2 -3 3 2.35\n",
    "convert: 2 -1 2.25 1.75 0.1 1.5 79228162514264337593543950335 -1 -12345.6789\n",
    "parse: 3.50 -0.001 False 0 1000\n",
    "format: 1234.5 1,234.50 12.5 % 0042.00 -3.14\n",
    "bits: 15,0,0,65536 -1,-1,-1,-2147483648 True\n",
    "closed: UserClosing 30\n",
);

#[test]
fn numbers_print_what_winforms_prints() {
    let (result, output) = run_with(NUMBERS, "numbers.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, NUMBERS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/keys` (фаза N7h): Tab, стрелки, сочетания
/// меню и набор в поле со стрелками.
const KEYS: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/keys.dll");

/// Что печатает `dotnet keys.dll self-test` (записано 2026-09-16, .NET 10,
/// WinForms на Windows, LF): Tab через ProcessDialogKey идёт к отмеченному
/// переключателю, стрелки — по кругу группы с отметкой раньше Enter; сочетание
/// нажимает пункт и во вложенном подменю, а без Ctrl и Alt не годится; текст
/// поля разбирается, когда спрашивают Value, — за краем подрезается, мусор
/// заменяется прежним значением без события.
const KEYS_OUTPUT: &str = concat!(
    "focus: numericUpDown1\n",
    "shown: numericUpDown1 5 '5' O, Control 2 True | True False False\n",
    "focus: button1\n",
    "focus: radioButton1\n",
    "radio: Red False\n",
    "radio: Green True\n",
    "focus: radioButton2\n",
    "radio: Green False\n",
    "radio: Blue True\n",
    "focus: radioButton3\n",
    "radio: Blue False\n",
    "radio: Red True\n",
    "focus: radioButton1\n",
    "arrows: radioButton1 True False False\n",
    "focus: button1\n",
    "back: button1\n",
    "open: &Open\n",
    "recent: second.txt\n",
    "shortcuts: True True False False\n",
    "value: 42\n",
    "typed: 42 '42'\n",
    "value: 100\n",
    "clamped: 100 '100'\n",
    "value: 0\n",
    "below: 0 '0'\n",
    "bad: 0 '0'\n",
    "focus: radioButton1\n",
    "next: True radioButton1 False radioButton1\n",
    "closed: UserClosing 0 True False False\n",
);

#[test]
fn keys_print_what_winforms_prints() {
    let (result, output) = run_with(KEYS, "keys.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, KEYS_OUTPUT);
    assert_eq!(code, 0);
}

/// Образец `tools/dotnet/samples/pqueue` (фаза N10): `PriorityQueue` из
/// dotnet/runtime, внесённая в corelib без правки.
const PQUEUE: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/pqueue.dll");

/// Что печатает `dotnet pqueue.dll` (записано 2026-09-16, .NET 10, LF,
/// инвариантная глобализация). Строки `heap`, `stream`, `ties` и `jobs` —
/// раскладка четверичной кучи и порядок при равных приоритетах: их не
/// угадать, их можно только повторить тем же алгоритмом. `copied (, 0)` —
/// нулевой элемент массива пар, который CopyTo не трогал.
const PQUEUE_OUTPUT: &str = concat!(
    "pqueue: start
",
    "heap g:0 b:2 d:4 a:1 c:3 f:6 e:5 count 7 capacity 7
",
    "peek g default True
",
    "drain g:0 a:1 b:2 c:3 d:4 e:5 f:6
",
    "stream capacity 8 layout one:1 five:5 four:4 two:2 three:3
",
    "ties red:3 gray:4 gold:4 pink:4 cyan:4 blue:4 green:5
",
    "max high top huge peek mid:5 rest mid:5 low:1 tiny:0
",
    "jobs removed True y:7 missing False layout urgent:1 later:8 z:7 x:7
",
    "capacity 20 trimmed 4 count 4
",
    "names 1:apple 2:banana 3:cherry
",
    "copied (, 0) (r, 1) (q, 3) (s, 2)
",
    "copy: Target array type is not compatible with the type of items in the collection. (Parameter 'array')
",
    "cleared 0 capacity 3
",
    "empty: Queue empty.
",
    "negative: initialCapacity | initialCapacity ('-1') must be a non-negative value. (Parameter 'initialCapacity') / Actual value was -1.
",
    "null: Value cannot be null. (Parameter 'items')
",
    "modified: Collection was modified after the enumerator was instantiated.
",
    "dijkstra A=0 B=7 C=9 F=11 E=20 D=20
",
    "pqueue: done
",
);

#[test]
fn pqueue_prints_what_dotnet_prints() {
    let (result, output) = run(PQUEUE);
    let code = result.unwrap_or_else(|error| panic!("{error}
printed so far:
{output}"));
    assert_eq!(output, PQUEUE_OUTPUT);
    assert_eq!(code, 20);
}

/// Образец `tools/dotnet/samples/sorted` (фаза N10b): LinkedList, Stack, Queue,
/// SortedList, SortedDictionary и SortedSet из dotnet/runtime без правки.
const SORTED: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/sorted.dll");

/// Что печатает `dotnet sorted.dll` (записано 2026-09-17, .NET 10, LF,
/// инвариантная глобализация; три запуска одинаковые). `set ops` идёт через
/// `stackalloc` в SortedSet, `stack:` — через упаковку `int` при копировании в
/// `object[]`, `queue capacity` — двухстрочный текст ArgumentOutOfRangeException.
const SORTED_OUTPUT: &str = concat!(
    "sorted: start\n",
    "linked: a,b,c,d,e,c 6 a c\n",
    "find: d e True\n",
    "removed: b,d,e False 3\n",
    "backwards: e,d,b\n",
    "attached: InvalidOperationException: The LinkedList node already belongs to a LinkedList.\n",
    "foreign: InvalidOperationException: The LinkedList node does not belong to current LinkedList.\n",
    "empty: InvalidOperationException: The LinkedList is empty.\n",
    "stack: 4,3,2,1 4 0,4,3,2,1,0 4,3,2,1 True\n",
    "stack pop: 4 True 3 2 InvalidOperationException: Stack empty.\n",
    "queue: q3,q4,q5 q3 True q3,q4,q5 InvalidOperationException: Queue empty.\n",
    "queue capacity: 4 ArgumentOutOfRangeException: capacity ('1') must be greater than or equal to '3'. (Parameter 'capacity')\n",
    "Actual value was 1.\n",
    "sorted list: [apple, 1],[fig, 7],[kiwi, 2],[pear, 3] 2 -1 1\n",
    "keys: apple,fig,kiwi,pear | 1,7,2,3 fig 4\n",
    "changed: [fig, 70],[kiwi, 2],[pear, 3] True 3\n",
    "duplicate: ArgumentException: An item with the same key has already been added. Key: kiwi (Parameter 'key')\n",
    "missing: KeyNotFoundException: The given key 'lime' was not present in the dictionary.\n",
    "by length: a,bb,ccc True\n",
    "from dictionary: [1, one],[3, three],[5, five]\n",
    "sorted dictionary: 10,30,50,60,70,80,90 7 v60 True False\n",
    "tree missing: KeyNotFoundException: The given key '20' was not present in the dictionary.\n",
    "tree copy: [10, v10] [90, v90]\n",
    "set: 1,3,5,7,9,11,13 1 13 view 5,7,9,11 4 5 11\n",
    "view add: 1,3,5,6,7,9,11,13 ArgumentOutOfRangeException: Specified argument was out of the range of valid values. (Parameter 'item')\n",
    "reverse: 13,11,9,7,6,5,3,1\n",
    "set ops: True True True True\n",
    "union: 1,3,5,6,7,8,9,11,13,100 | 5,6,7 | 1,3,9,11,13 | 2,4,5,6,7,9,11,13\n",
    "remove where: 3 1,5,7,11,13 True 7\n",
    "bounds: ArgumentException: Must be less than or equal to upperValue. (Parameter 'lowerValue')\n",
    "sorted: done\n",
);

#[test]
fn sorted_collections_print_what_dotnet_prints() {
    let (result, output) = run(SORTED);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, SORTED_OUTPUT);
    assert_eq!(code, 42);
}

/// Образец `tools/dotnet/samples/nullable` (фаза N10b): упаковка и распаковка
/// `Nullable<T>` по правилам среды .NET, `is int?`, операторы и `?.`.
const NULLABLE: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/nullable.dll");

/// Что печатает `dotnet nullable.dll` (записано 2026-09-17, .NET 10, LF,
/// инвариантная глобализация; три запуска одинаковые). `boxed: True` — пустое
/// значение упаковалось в null; `depth: 3 ` с хвостовым пробелом — пустой
/// `int?` печатается пустой строкой.
const NULLABLE_OUTPUT: &str = concat!(
    "nullable: start\n",
    "values: False True 5 0 7 5 '' '5'\n",
    "boxed: True Int32 5 True True False\n",
    "unboxed: 5 False 12 12\n",
    "no value: Nullable object must have a value.\n",
    "wrong type: InvalidCastException\n",
    "operators: 6 True True False True True -1\n",
    "equals: True True 5 0\n",
    "depth: 3 \n",
    "struct: Point (3, 4) 4 True\n",
    "list: 2 107 10||-3\n",
    "nullable: done\n",
);

#[test]
fn nullable_prints_what_dotnet_prints() {
    let (result, output) = run(NULLABLE);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, NULLABLE_OUTPUT);
    assert_eq!(code, 11);
}

/// Образец `tools/dotnet/samples/listsort` (фаза N10c): List<T>,
/// ReadOnlyCollection<T> и сортировка из dotnet/runtime.
const LISTSORT: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/listsort.dll");

/// Что печатает `dotnet listsort.dll` (записано 2026-09-17, .NET 10, LF,
/// инвариантная глобализация; три запуска одинаковые). `large`, `comparison`
/// и `part` — порядок равных ключей после неустойчивой сортировки .NET: его
/// повторяет только тот же алгоритм.
const LISTSORT_OUTPUT: &str = concat!(
    "listsort: start\n",
    "small: 0j9,1a0,1h7,3b1,3c2,3e4,3f5,3g6,3i8,4d3,4k10,4l11\n",
    "large: 0u46,0t45,0n39,0j9,0z25,0w48,1a0,1r43,1m38,1k36,1f31,1d29,1b27,1t19,1q16,1x49,1h7,1p15,2o14,2l37,2p41,2w22,2m12,2n13,2s18,2v47,3s44,3b1,3o40,3e4,3f5,3g6,3c2,3i34,3y24,3i8,3c28,3a26,3r17,3h33,4j35,4e30,4k10,4l11,4q42,4d3,4x23,4v21,4u20,4g32\n",
    "comparison: 4e30,4j35,4x23,4l11,4u20,4g32,4k10,4d3,4v21,3a26,3i8,3f5,3e4,3y24,3c2,3c28,3b1,3i34,3r17,3h33,3g6,2w22,2o14,2n13,2m12,2l37,2s18,1k36,1f31,1a0,1t19,1b27,1m38,1q16,1p15,1h7,1d29,0z25,0j9,0n39\n",
    "part: 1a0,3b1,3c2,4d3,3e4,0j9,1h7,1t19,1q16,1p15,2o14,2w22,2s18,2m12,2n13,3r17,3f5,3i8,3g6,3y24,4l11,4k10,4u20,4v21,4x23,0z25,3a26,1b27,3c28,1d29\n",
    "keys: 1,1,1,1,2,2,2,3,3,3,3,3,4,4,4,5,5,5,5,5 | v17,v3,v12,v7,v16,v6,v11,v9,v14,v19,v4,v1,v8,v13,v18,v10,v5,v15,v2,v0\n",
    "doubles: NaN,NaN,-Infinity,-1,0,-0,2,3.5,10000000000\n",
    "ordinal: Apple,Fig,Kiwi,apple,banana,date,fig,kiwi,pear\n",
    "search: 14 -21 24\n",
    "capacity: 3 1,2,3\n",
    "grown: 6\n",
    "inserted: 0,1,2,20,21,22,3,4,5,6,7,8 12 12\n",
    "range: 2,20,21,22 1,2,20 4 7 -1\n",
    "find: 21 4 4 7 0,2,20,22,4,6,8 True True\n",
    "removed: 3 0,1,2,3,4,5,6,7,8\n",
    "reversed: 5,4,3,0,6,7,8 #5,#4,#3,#0,#6,#7,#8\n",
    "trimmed: 7 20 20\n",
    "sum: 33\n",
    "read only: 7 5 True NotSupportedException: Collection is read-only.\n",
    "untyped: 7 True False ArgumentException: The value \"text\" is not of type \"System.Int32\" and cannot be used in this generic collection. (Parameter 'value')\n",
    "boxes: 5,4,3,0,6,7,8,9\n",
    "index: ArgumentOutOfRangeException: Index was out of range. Must be non-negative and less than the size of the collection. (Parameter 'index')\n",
    "insert: ArgumentOutOfRangeException: Index must be within the bounds of the List. (Parameter 'index')\n",
    "range fails: ArgumentException: Offset and length were out of bounds for the array or count is greater than the number of elements from index to the end of the source collection.\n",
    "modified: InvalidOperationException: Collection was modified; enumeration operation may not execute.\n",
    "set: a,b,d True True False\n",
    "listsort: done\n",
);

#[test]
fn list_and_sort_print_what_dotnet_prints() {
    let (result, output) = run(LISTSORT);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, LISTSORT_OUTPUT);
    assert_eq!(code, 16);
}

/// Образец `tools/dotnet/samples/dict` (фаза N10d): Dictionary<TKey, TValue> и
/// HashSet<T> из dotnet/runtime.
const DICT: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/dict.dll");

/// Что печатает `dotnet dict.dll` (записано 2026-09-17, .NET 10.0.5, LF).
/// `order`, `odd removed`, `sym` и `union` — порядок обхода после удалений и
/// повторных вставок: его решает список свободных записей; `capacity`,
/// `trimmed` и `trim` — простые числа из таблицы HashHelpers.
const DICT_OUTPUT: &str = concat!(
    "dict: start\n",
    "order: k0=0,n1=11,k2=2,n3=13,k4=4,k5=5,k6=6,n2=12,k8=8,k9=9,n4=14\n",
    "keys: k0,n1,k2,n3,k4,k5,k6,n2,k8,k9,n4 | 0,11,2,13,4,5,6,12,8,9,14 | 11\n",
    "capacity: 11 23 107 3\n",
    "trimmed: 71 50\n",
    "odd removed: 27 0,14,28,42,56,70 1001,336,1000\n",
    "ignore case: 1 True True True True True\n",
    "custom: two False ByLength 2\n",
    "try: False True True 26 False 0\n",
    "missing: KeyNotFoundException: The given key 'nope' was not present in the dictionary. | ArgumentException: An item with the same key has already been added. Key: k0 | ArgumentNullException: Value cannot be null. (Parameter 'key') | ArgumentOutOfRangeException: Specified argument was out of the range of valid values. (Parameter 'capacity')\n",
    "untyped: 0 True True ArgumentException: The value \"5\" is not of type \"System.String\" and cannot be used in this generic collection. (Parameter 'key') | ArgumentException: The value \"text\" is not of type \"System.Int32\" and cannot be used in this generic collection. (Parameter 'value') False False\n",
    "entries: k0:0,n1:11,k2:2\n",
    "copied: -,k0,n1,k2,n3,k4,k5,k6,n2,k8,k9,n4 ArgumentException: Destination array is not long enough to copy all the items in the collection. Check array index and length. | NotSupportedException: Mutating a key collection derived from a dictionary is not allowed. True False\n",
    "span: 2 False True n4 14 True 10 True False True\n",
    "span added: 7 KeyNotFoundException: The given key 'zz' was not present in the dictionary. | InvalidOperationException: The collection's comparer does not support the requested operation.\n",
    "marshal: False True 10 True 3\n",
    "copies: 0 17 n1,n3,n2 12\n",
    "set: 0,3,6,9,1,4,7,10,2,5,8,11 12 False True True 0,3,6,9,1,4,7,10,2,5,8,11\n",
    "sym: 1,2,22,21,6,7,8,9,10,11,12,13,14,4,16,17,18,19\n",
    "sym set: 30,31,22,21,6,7,8,9,10,11,12,13,14,4,16,17,18,19 False True True True True\n",
    "intersect: 30,31,22,21,10,11,12,13,14,16,17,18,19 13\n",
    "union: 31,21,7,11,13,5,17,19 8 23 59\n",
    "trim: 11 8 True 7 False 0\n",
    "words: Apple,banana,Cherry True True False True Apple,banana,date True True Apple\n",
    "set comparer: True False True 1\n",
    "copy: 0,31,21,7,0,0,0,0,0,0 ArgumentException: Destination array is not long enough to copy all the items in the collection. Check array index and length. | ArgumentException: Destination array is not long enough to copy all the items in the collection. Check array index and length.\n",
    "modified: InvalidOperationException: Collection was modified; enumeration operation may not execute. | InvalidOperationException: Collection was modified; enumeration operation may not execute.\n",
    "dict: done\n",
);

#[test]
fn dictionary_and_hash_set_print_what_dotnet_prints() {
    let (result, output) = run(DICT);
    let code = result.unwrap_or_else(|error| panic!("{error}\nprinted so far:\n{output}"));
    assert_eq!(output, DICT_OUTPUT);
    assert_eq!(code, 21);
}

/// Образец `tools/dotnet/samples/drawing` (фаза N9): System.Drawing на Bitmap с
/// чтением точек, пути, преобразования, отсечение и форма с двойной буферизацией.
const DRAWING: &[u8] = include_bytes!("../../../initrd/usr/share/dotnet/samples/drawing.dll");

/// Что печатает `dotnet drawing.dll self-test` (записано 2026-09-17, .NET 10,
/// GDI+ на Windows, LF): точки заливок, перьев, смешивания, градиента, текстуры и
/// картинок совпадают до значения канала; у кругов проверяется только внутри и
/// снаружи, у текста (фаза N9c) — отношения: шрифты у GDI+ и песочницы разные.
const DRAWING_OUTPUT: &str = concat!(
    "drawing: start\n",
    "bitmap: 64x48 Format32bppArgb 0,0,0,0 {Width=64, Height=48} 96\n",
    "pixel: 10,20,30,40 a141e28 False True\n",
    "pixel: out of range\n",
    "opaque: 255,0,0,0 255,20,30,40\n",
    "fill: 255 255 0 0 0 | 0 0 255 255 | 255 0 | 255 255\n",
    "smoothing: AntiAlias Default SourceOver Bilinear\n",
    "antialias: 255 255 127 0 0 | 0 0 127 255\n",
    "half: 255 255 0 0 0 | 0 0 255 255\n",
    "triangle: ##.#.#.#.\n",
    "ellipse: ###...##\n",
    "smooth ellipse: True 0 255\n",
    "pie: #...#.#\n",
    "rectangle: 255 255 0 0 0 0 0 0 0 0 0 0 0 255 255 | 255 255 0 255 255 255 255 255 255 255 255 255 0 255 255 | 255 255 255 255 255 255 255 255 255 255 255 255 255 255 255\n",
    "line: 255 255 0 0 0 | 0 0 0 255 255 | 2-4 5-8 9-12 13-16 17-20 21-24 25-28 29-30\n",
    "wide 5: ..#####.. 255 255 0 0\n",
    "wide 4: ..####... 255 255 0 0\n",
    "smooth wide: 255 255 127 0 255 0 0 255\n",
    "dash pattern: 3,1 Dash Miter Flat 10 Center SolidColor\n",
    "dashes: ######..######..\n",
    "joins: #.###..#.\n",
    "blend: 128,0,0,255 178,143,0,111 255,127,127,255 255,55,133,33\n",
    "copy: 0,0,0,0 0\n",
    "gradient brush: Tile {X=0,Y=-20,Width=40,Height=40} 255,0,0,0 {X=0,Y=20,Width=10,Height=20}\n",
    "gradient: 0 64 128 191 249 | True True\n",
    "texture: 255,255,0,0 255,0,255,0 255,0,0,255 255,0,0,0\n",
    "image: 255 255 0 0 0 0 0 0 0 0 0 0 255 255 255,0,0,255 255,255,0,0\n",
    "scaled: 0 0 127 255 255 255,0,0,255 255,191,0,64 255,255,0,0\n",
    "nearest: 0 0 255 255 255 255,0,0,255 255,255,0,0 255,255,0,0\n",
    "part: 255,255,0,0 255,255,255,255 255,0,0,255 255,255,0,0\n",
    "clone: 255,0,0,255 0,0,0,0 3 255,0,0,255\n",
    "matrix: 2,0,0,3,10,20 {X=12, Y=23} False True 10\n",
    "rotate: 1.732,1.5,-1,2.598,10,20\n",
    "invert: 0.433,-0.25,0.167,0.289,-7.663,-3.274\n",
    "rotate at: 0,1,-1,0,10,0\n",
    "shear: 2.5,3,2,2,15,26\n",
    "vectors: {X=2, Y=0} False\n",
    "translate: 1,0,0,1,20,10 | 2,0,0,2,20,10 | 1,0,0,1,20,10 255 255 0 0 0 0 0 255 255\n",
    "rotate transform: 0,1,-1,0,30,0 ##..#..\n",
    "scaled pen: .####..\n",
    "path: 17 0,1,1,129,0,3,3,3,3,3,3,3,3,3,3,3,131 Alternate {X=0,Y=0,Width=30,Height=30} False True {X=25, Y=15} {X=15, Y=25}\n",
    "fill mode: #. ## True\n",
    "arcs: 4 0,3,3,3,1,3,3,3,3,3,3,3,3,3 | 5 0,1,3,3,131\n",
    "lines: 14 0,1,1,1,129,0,3,3,3,0,1,0,1,129 {X=0, Y=5}\n",
    "curve: 7 2,2 8,12 16,12\n",
    "path transform: {X=5,Y=5,Width=30,Height=30} {X=0,Y=0,Width=30,Height=30}\n",
    "star: ##.##.\n",
    "clip: {X=0,Y=0,Width=10,Height=10} True False False {X=0,Y=0,Width=10,Height=10} {X=0,Y=0,Width=10,Height=10} #.\n",
    "reset: {X=-4194304,Y=-4194304,Width=8388608,Height=8388608} False True {X=0,Y=0,Width=40,Height=40}\n",
    "intersect: {X=12,Y=12,Width=3,Height=3}\n",
    "exclude: {X=0,Y=0,Width=40,Height=40} 255,255,0,0 255,255,255,255\n",
    "clip path: #..# 255,0,0,255 255,0,0,0\n",
    "region: True False True False\n",
    "xor: False True True False True\n",
    "infinite: True False True False True True\n",
    "bounds: {X=0,Y=0,Width=15,Height=15} {X=0,Y=0,Width=15,Height=15}\n",
    "fill region: #.#.#\n",
    "measure: True True True True {Width=0, Height=0} True True\n",
    "font: Arial 10 Point 10 Bold True Arial 12 Courier New\n",
    "text: True True True\n",
    "rotated text: True True True\n",
    "centered text: Center Center True True True\n",
    "clipped text: True True\n",
    "buffer: True True 255,240,244,248 255,255,140,0\n",
    "shown: 360x240\n",
    "paint: {X=0,Y=0,Width=360,Height=240} True True\n",
    "closed: UserClosing\n",
    "drawing: done\n",
);

#[test]
fn drawing_prints_what_gdiplus_prints() {
    let (result, output) = run_with(DRAWING, "drawing.dll", &["self-test"]);
    let code = result.unwrap_or_else(|error| panic!("{error}
printed so far:
{output}"));
    assert_eq!(output, DRAWING_OUTPUT);
    assert_eq!(code, 9);
}
