//! Тесты своей среды без .NET на машине.
//!
//! Главная проверка — `cargo xtask clr-check`, где вывод сравнивается с
//! настоящим `dotnet` на свежесобранных образцах. Здесь — то же сравнение на
//! сборках, уже лежащих в репозитории (с выводом, записанным из настоящего
//! `dotnet`), и правила ECMA-335, которые легко нарушить незаметно.

use std::string::String;

use crate::ops::{self, Fault};
use crate::{Host, Value, Vm, VmError};

struct Capture(String);

impl Host for Capture {
    fn write_out(&mut self, text: &str) {
        self.0.push_str(text);
    }
}

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

fn run(data: &[u8]) -> (Result<i32, VmError>, String) {
    let mut vm = Vm::new(data, CORELIB, Capture(String::new())).expect("assembly parses");
    let result = vm.run_main(&[]);
    (result, vm.into_host().0)
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
        [("hello", HELLO), ("arith", ARITH), ("objects", OBJECTS), ("exceptions", EXCEPTIONS), ("generics", GENERICS), ("gc", GC), ("text", TEXT), ("collections", COLLECTIONS), ("floats", FLOATS), ("enums", ENUMS), ("linq", LINQ)]
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
    let mut vm = Vm::new(GC, CORELIB, Capture(String::new())).expect("assembly parses");
    let result = vm.run_main(&[]);
    let collections = vm.collections();
    let output = vm.into_host().0;
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
