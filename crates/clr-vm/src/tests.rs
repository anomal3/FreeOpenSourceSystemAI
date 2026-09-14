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
    for (name, data) in [("hello", HELLO), ("arith", ARITH), ("objects", OBJECTS)] {
        let worker = std::thread::Builder::new()
            .stack_size(kib * 1024)
            .spawn(move || run(data).0.map(|_| ()).map_err(|error| std::format!("{error}")))
            .expect("thread starts");
        // Переполнение стека обрывает весь процесс теста — это и есть провал.
        let result = worker.join().expect("thread finishes");
        assert!(result.is_ok(), "{name} in {kib} KiB: {result:?}");
    }
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
