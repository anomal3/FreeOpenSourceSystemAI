//! Члены базовой библиотеки, написанные в Rust, а не на C#.
//!
//! # Как находятся
//!
//! Базовая библиотека (`tools/dotnet/corelib`) объявляет их `extern` с
//! `MethodImplOptions.InternalCall`. Встретив такой метод, среда ищет его здесь
//! по полному имени с типами параметров — `System.String::Concat(string,string)`.
//! Всё, что можно написать на C#, написано на C#; здесь только то, что требует
//! знать устройство среды: печать, строки UTF-16, тип объекта, раскладку
//! значения, данные сборки.

use alloc::format;
use alloc::vec::Vec;

use crate::heap::Object;
use crate::types::Prim;
use crate::value::Value;
use crate::vm::Vm;
use crate::{Host, VmError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    Write,
    WriteLine,
    WriteLineEmpty,
    GetType,
    TypeName,
    TypeFullName,
    ValueTypeEquals,
    ValueTypeHash,
    EnumToString,
    InitializeArray,
    ObjectHash,
    ArrayLength,
    StringLength,
    StringChar,
    Concat2,
    Concat3,
    Concat4,
    StringEquals,
    StringHash,
    BoolToString,
    CharToString,
    I8ToString,
    U8ToString,
    I16ToString,
    U16ToString,
    I32ToString,
    U32ToString,
    I64ToString,
    U64ToString,
    DelegateCombine,
    DelegateRemove,
    TypeFromHandle,
    I32CompareTo,
    I64CompareTo,
    StringCompareTo,
    StringFromCharCount,
    StringFromChars,
    StringFromCharsRange,
    StringToCharArray,
    StringSubstring,
    StringIndexOfChar,
    StringIndexOfString,
    StringLastIndexOfChar,
    StringToUpper,
    StringToLower,
    StringReplace,
    StringReplaceChar,
    StringCompareOrdinal,
    CharIsLetter,
    CharIsDigit,
    CharIsLetterOrDigit,
    CharIsWhiteSpace,
    CharIsUpper,
    CharIsLower,
    CharToUpper,
    CharToLower,
    IntegerToString(IntKind),
}

/// Какой целый примитив печатается по формату.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntKind {
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
}

const TABLE: &[(&str, Native)] = &[
    ("System.Console::Write(string)", Native::Write),
    ("System.Console::WriteLine(string)", Native::WriteLine),
    ("System.Console::WriteLine()", Native::WriteLineEmpty),
    ("System.Object::GetType()", Native::GetType),
    ("System.RuntimeType::get_Name()", Native::TypeName),
    ("System.RuntimeType::get_FullName()", Native::TypeFullName),
    ("System.ValueType::Equals(object)", Native::ValueTypeEquals),
    ("System.ValueType::GetHashCode()", Native::ValueTypeHash),
    ("System.Enum::ToString()", Native::EnumToString),
    (
        "System.Runtime.CompilerServices.RuntimeHelpers::InitializeArray(System.Array,System.RuntimeFieldHandle)",
        Native::InitializeArray,
    ),
    ("System.Runtime.CompilerServices.RuntimeHelpers::GetHashCode(object)", Native::ObjectHash),
    ("System.Array::get_Length()", Native::ArrayLength),
    ("System.String::get_Length()", Native::StringLength),
    ("System.String::get_Chars(int32)", Native::StringChar),
    ("System.String::Concat(string,string)", Native::Concat2),
    ("System.String::Concat(string,string,string)", Native::Concat3),
    ("System.String::Concat(string,string,string,string)", Native::Concat4),
    ("System.String::Equals(string,string)", Native::StringEquals),
    ("System.String::GetHashCode()", Native::StringHash),
    ("System.Boolean::ToString()", Native::BoolToString),
    ("System.Char::ToString()", Native::CharToString),
    ("System.SByte::ToString()", Native::I8ToString),
    ("System.Byte::ToString()", Native::U8ToString),
    ("System.Int16::ToString()", Native::I16ToString),
    ("System.UInt16::ToString()", Native::U16ToString),
    ("System.Int32::ToString()", Native::I32ToString),
    ("System.UInt32::ToString()", Native::U32ToString),
    ("System.Int64::ToString()", Native::I64ToString),
    ("System.UInt64::ToString()", Native::U64ToString),
    ("System.Delegate::Combine(System.Delegate,System.Delegate)", Native::DelegateCombine),
    ("System.Delegate::Remove(System.Delegate,System.Delegate)", Native::DelegateRemove),
    ("System.Type::GetTypeFromHandle(System.RuntimeTypeHandle)", Native::TypeFromHandle),
    ("System.Int32::CompareTo(int32)", Native::I32CompareTo),
    ("System.Int64::CompareTo(int64)", Native::I64CompareTo),
    ("System.String::CompareTo(string)", Native::StringCompareTo),
    ("System.String::.ctor(char,int32)", Native::StringFromCharCount),
    ("System.String::.ctor(char[])", Native::StringFromChars),
    ("System.String::.ctor(char[],int32,int32)", Native::StringFromCharsRange),
    ("System.String::ToCharArray()", Native::StringToCharArray),
    ("System.String::Substring(int32,int32)", Native::StringSubstring),
    ("System.String::IndexOf(char,int32)", Native::StringIndexOfChar),
    ("System.String::IndexOf(string,int32)", Native::StringIndexOfString),
    ("System.String::LastIndexOf(char)", Native::StringLastIndexOfChar),
    ("System.String::ToUpper()", Native::StringToUpper),
    ("System.String::ToLower()", Native::StringToLower),
    ("System.String::Replace(string,string)", Native::StringReplace),
    ("System.String::Replace(char,char)", Native::StringReplaceChar),
    ("System.String::CompareOrdinal(string,string)", Native::StringCompareOrdinal),
    ("System.Char::IsLetter(char)", Native::CharIsLetter),
    ("System.Char::IsDigit(char)", Native::CharIsDigit),
    ("System.Char::IsLetterOrDigit(char)", Native::CharIsLetterOrDigit),
    ("System.Char::IsWhiteSpace(char)", Native::CharIsWhiteSpace),
    ("System.Char::IsUpper(char)", Native::CharIsUpper),
    ("System.Char::IsLower(char)", Native::CharIsLower),
    ("System.Char::ToUpper(char)", Native::CharToUpper),
    ("System.Char::ToLower(char)", Native::CharToLower),
    ("System.SByte::ToString(string)", Native::IntegerToString(IntKind::I8)),
    ("System.Byte::ToString(string)", Native::IntegerToString(IntKind::U8)),
    ("System.Int16::ToString(string)", Native::IntegerToString(IntKind::I16)),
    ("System.UInt16::ToString(string)", Native::IntegerToString(IntKind::U16)),
    ("System.Int32::ToString(string)", Native::IntegerToString(IntKind::I32)),
    ("System.UInt32::ToString(string)", Native::IntegerToString(IntKind::U32)),
    ("System.Int64::ToString(string)", Native::IntegerToString(IntKind::I64)),
    ("System.UInt64::ToString(string)", Native::IntegerToString(IntKind::U64)),
];

pub(crate) fn lookup(key: &str) -> Option<Native> {
    TABLE.iter().find(|(name, _)| *name == key).map(|(_, native)| *native)
}

/// Выполнить член. `Some` — значение, которое он вернул.
pub(crate) fn call<H: Host>(vm: &mut Vm<'_, H>, native: Native, args: &[Value]) -> Result<Option<Value>, VmError> {
    let arg = |index: usize| args.get(index).copied().ok_or(VmError::Invalid { what: "missing argument", at: alloc::string::String::new() });
    // `this` метода примитива приходит указателем на число или упакованным
    // объектом; `deref` достаёт число в обоих случаях.
    let number = |vm: &Vm<'_, H>| -> Result<Value, VmError> { vm.deref(arg(0)?) };
    Ok(match native {
        Native::Write | Native::WriteLine => {
            let text = vm.string_units(arg(0)?)?.unwrap_or_default();
            vm.print_units(&text, native == Native::WriteLine);
            None
        }
        Native::WriteLineEmpty => {
            vm.print_units(&[], true);
            None
        }
        Native::GetType => {
            let Value::Obj(Some(object)) = arg(0)? else {
                return Err(vm.exception("System.NullReferenceException"));
            };
            let ty = vm.type_of_object(object)?;
            Some(vm.type_object(ty)?)
        }
        Native::TypeName => {
            let ty = vm.runtime_type(arg(0)?)?;
            let name = vm.simple_name(ty)?;
            Some(vm.new_string_from(&name)?)
        }
        Native::TypeFullName => {
            let ty = vm.runtime_type(arg(0)?)?;
            let name = vm.types[ty.0 as usize].name.clone();
            Some(vm.new_string_from(&name)?)
        }
        Native::ValueTypeEquals => {
            let equal = match (vm.boxed(arg(0)?), vm.boxed(arg(1)?)) {
                (Some((ta, a)), Some((tb, b))) => ta == tb && vm.values_equal(a, b, 0),
                _ => false,
            };
            Some(Value::I32(i32::from(equal)))
        }
        // Хэш значения и объекта печатать бессмысленно (у .NET он случаен от
        // запуска к запуску), но он обязан быть одинаков у равных значений.
        Native::ValueTypeHash => {
            let hash = match vm.boxed(arg(0)?) {
                Some((_, Value::I32(x))) => x,
                Some((_, Value::I64(x) | Value::Native(x))) => (x ^ (x >> 32)) as i32,
                _ => 0,
            };
            Some(Value::I32(hash))
        }
        Native::ObjectHash => Some(Value::I32(match arg(0)? {
            Value::Obj(Some(object)) => object.0 as i32,
            _ => 0,
        })),
        Native::EnumToString => {
            return Err(VmError::Unsupported { what: format!("Enum.ToString in {} (phase N4)", vm.location()) });
        }
        Native::InitializeArray => {
            vm.initialize_array(arg(0)?, arg(1)?)?;
            None
        }
        Native::ArrayLength => Some(Value::I32(vm.array_len(arg(0)?)? as i32)),
        Native::StringLength => {
            let Some(text) = vm.string_units(arg(0)?)? else {
                return Err(vm.exception("System.NullReferenceException"));
            };
            Some(Value::I32(text.len() as i32))
        }
        Native::StringChar => {
            let Some(text) = vm.string_units(arg(0)?)? else {
                return Err(vm.exception("System.NullReferenceException"));
            };
            let index = vm.int32(arg(1)?)?;
            let unit = usize::try_from(index)
                .ok()
                .and_then(|index| text.get(index).copied())
                .ok_or_else(|| vm.exception("System.IndexOutOfRangeException"))?;
            Some(Value::I32(i32::from(unit)))
        }
        Native::Concat2 | Native::Concat3 | Native::Concat4 => {
            // `null` в склейке — пустая строка, как в .NET.
            let mut joined: Vec<u16> = Vec::new();
            for value in args {
                let part = vm.string_units(*value)?.unwrap_or_default();
                joined.try_reserve(part.len()).map_err(|_| VmError::OutOfMemory)?;
                joined.extend_from_slice(&part);
            }
            Some(Value::Obj(Some(vm.heap.alloc(Object::String(joined))?)))
        }
        Native::StringEquals => {
            let left = vm.string_units(arg(0)?)?;
            let right = vm.string_units(arg(1)?)?;
            Some(Value::I32(i32::from(left == right)))
        }
        Native::StringHash => {
            let text = vm.string_units(arg(0)?)?.unwrap_or_default();
            let hash = text.iter().fold(5381i32, |h, &u| h.wrapping_mul(33) ^ i32::from(u));
            Some(Value::I32(hash))
        }
        Native::BoolToString => {
            let value = vm.int32(number(vm)?)? != 0;
            Some(vm.new_string_from(if value { "True" } else { "False" })?)
        }
        Native::CharToString => {
            let unit = vm.int32(number(vm)?)? as u16;
            Some(Value::Obj(Some(vm.heap.string(core::iter::once(unit))?)))
        }
        Native::I8ToString => text(vm, format!("{}", vm.int32(number(vm)?)? as i8))?,
        Native::U8ToString => text(vm, format!("{}", vm.int32(number(vm)?)? as u8))?,
        Native::I16ToString => text(vm, format!("{}", vm.int32(number(vm)?)? as i16))?,
        Native::U16ToString => text(vm, format!("{}", vm.int32(number(vm)?)? as u16))?,
        Native::I32ToString => text(vm, format!("{}", vm.int32(number(vm)?)?))?,
        Native::U32ToString => text(vm, format!("{}", vm.int32(number(vm)?)? as u32))?,
        Native::I64ToString => text(vm, format!("{}", vm.int64(number(vm)?)?))?,
        Native::U64ToString => text(vm, format!("{}", vm.int64(number(vm)?)? as u64))?,
        Native::DelegateCombine => Some(vm.combine_delegates(arg(0)?, arg(1)?)?),
        Native::DelegateRemove => Some(vm.remove_delegate(arg(0)?, arg(1)?)?),
        Native::TypeFromHandle => Some(arg(0)?),
        Native::I32CompareTo => {
            let order = vm.int32(number(vm)?)?.cmp(&vm.int32(arg(1)?)?);
            Some(Value::I32(order as i32))
        }
        Native::I64CompareTo => {
            let order = vm.int64(number(vm)?)?.cmp(&vm.int64(arg(1)?)?);
            Some(Value::I32(order as i32))
        }
        // `null` меньше любой строки, как в .NET.
        Native::StringCompareTo => {
            let left = vm.string_units(arg(0)?)?;
            let right = vm.string_units(arg(1)?)?;
            Some(Value::I32(left.cmp(&right) as i32))
        }
        Native::StringFromCharCount => {
            let unit = vm.int32(arg(0)?)? as u16;
            let count = usize::try_from(vm.int32(arg(1)?)?).map_err(|_| vm.exception("System.ArgumentOutOfRangeException"))?;
            let mut units = Vec::new();
            units.try_reserve_exact(count).map_err(|_| VmError::OutOfMemory)?;
            units.resize(count, unit);
            string_value(vm, units)?
        }
        // `new string((char[])null)` — пустая строка, как в .NET.
        Native::StringFromChars => {
            let units = char_array(vm, arg(0)?)?.unwrap_or_default();
            string_value(vm, units)?
        }
        Native::StringFromCharsRange => {
            let Some(units) = char_array(vm, arg(0)?)? else {
                return Err(vm.exception("System.ArgumentNullException"));
            };
            let range = checked_range(units.len(), vm.int32(arg(1)?)?, vm.int32(arg(2)?)?)
                .ok_or_else(|| vm.exception("System.ArgumentOutOfRangeException"))?;
            let mut part = Vec::new();
            part.try_reserve_exact(range.len()).map_err(|_| VmError::OutOfMemory)?;
            part.extend_from_slice(&units[range]);
            string_value(vm, part)?
        }
        Native::StringToCharArray => {
            let units = this_units(vm, arg(0)?)?;
            let char_type = vm.prim_type(Prim::Char)?;
            let ty = vm.array_of(char_type)?;
            let mut items = Vec::new();
            items.try_reserve_exact(units.len()).map_err(|_| VmError::OutOfMemory)?;
            items.extend(units.iter().map(|&unit| Value::I32(i32::from(unit))));
            Some(Value::Obj(Some(vm.heap.alloc(Object::Array { ty, items })?)))
        }
        Native::StringSubstring => {
            let units = this_units(vm, arg(0)?)?;
            let range = checked_range(units.len(), vm.int32(arg(1)?)?, vm.int32(arg(2)?)?)
                .ok_or_else(|| vm.exception("System.ArgumentOutOfRangeException"))?;
            let mut part = Vec::new();
            part.try_reserve_exact(range.len()).map_err(|_| VmError::OutOfMemory)?;
            part.extend_from_slice(&units[range]);
            string_value(vm, part)?
        }
        Native::StringIndexOfChar => {
            let units = this_units(vm, arg(0)?)?;
            let unit = vm.int32(arg(1)?)? as u16;
            let start = start_index(vm, units.len(), arg(2)?)?;
            let found = units[start..].iter().position(|&u| u == unit).map(|at| at + start);
            Some(Value::I32(found.map_or(-1, |at| at as i32)))
        }
        // Порядковый поиск: у FreeOS культур нет (см. `tools/dotnet/corelib/Text.cs`).
        Native::StringIndexOfString => {
            let units = this_units(vm, arg(0)?)?;
            let Some(needle) = vm.string_units(arg(1)?)? else {
                return Err(vm.exception("System.ArgumentNullException"));
            };
            let start = start_index(vm, units.len(), arg(2)?)?;
            let found = if needle.is_empty() {
                Some(start)
            } else {
                units[start..].windows(needle.len()).position(|window| window == needle.as_slice()).map(|at| at + start)
            };
            Some(Value::I32(found.map_or(-1, |at| at as i32)))
        }
        Native::StringLastIndexOfChar => {
            let units = this_units(vm, arg(0)?)?;
            let unit = vm.int32(arg(1)?)? as u16;
            Some(Value::I32(units.iter().rposition(|&u| u == unit).map_or(-1, |at| at as i32)))
        }
        Native::StringToUpper | Native::StringToLower => {
            let mut units = this_units(vm, arg(0)?)?;
            for unit in &mut units {
                *unit = map_case(*unit, native == Native::StringToUpper);
            }
            string_value(vm, units)?
        }
        Native::StringReplace => {
            let units = this_units(vm, arg(0)?)?;
            let Some(old) = vm.string_units(arg(1)?)? else {
                return Err(vm.exception("System.ArgumentNullException"));
            };
            if old.is_empty() {
                return Err(vm.exception("System.ArgumentException"));
            }
            let new = vm.string_units(arg(2)?)?.unwrap_or_default();
            let mut out = Vec::new();
            let mut at = 0;
            while at < units.len() {
                if units[at..].starts_with(&old) {
                    out.try_reserve(new.len()).map_err(|_| VmError::OutOfMemory)?;
                    out.extend_from_slice(&new);
                    at += old.len();
                } else {
                    out.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
                    out.push(units[at]);
                    at += 1;
                }
            }
            string_value(vm, out)?
        }
        Native::StringReplaceChar => {
            let mut units = this_units(vm, arg(0)?)?;
            let old = vm.int32(arg(1)?)? as u16;
            let new = vm.int32(arg(2)?)? as u16;
            for unit in &mut units {
                if *unit == old {
                    *unit = new;
                }
            }
            string_value(vm, units)?
        }
        // Разница первых несовпавших единиц или длин, как у .NET; `null`
        // меньше любой строки.
        Native::StringCompareOrdinal => {
            let difference = match (vm.string_units(arg(0)?)?, vm.string_units(arg(1)?)?) {
                (None, None) => 0,
                (None, Some(_)) => -1,
                (Some(_), None) => 1,
                (Some(a), Some(b)) => a
                    .iter()
                    .zip(&b)
                    .find(|(x, y)| x != y)
                    .map_or(a.len() as i32 - b.len() as i32, |(x, y)| i32::from(*x) - i32::from(*y)),
            };
            Some(Value::I32(difference))
        }
        Native::CharIsLetter
        | Native::CharIsDigit
        | Native::CharIsLetterOrDigit
        | Native::CharIsWhiteSpace
        | Native::CharIsUpper
        | Native::CharIsLower => {
            let unit = vm.int32(arg(0)?)? as u16;
            // Суррогатная половинка — не символ: ни буква, ни цифра.
            let answer = char::from_u32(u32::from(unit)).is_some_and(|c| match native {
                // У .NET буква — категории L*, у Rust `Alphabetic` шире (римские
                // цифры, некоторые знаки); числа из неё вычитаются.
                Native::CharIsLetter => c.is_alphabetic() && !c.is_numeric(),
                // У .NET цифра — категория Nd; `is_numeric` шире (дроби, римские
                // цифры) — берутся десятичные ASCII и цифры других письменностей.
                Native::CharIsDigit => is_decimal_digit(c),
                Native::CharIsLetterOrDigit => (c.is_alphabetic() && !c.is_numeric()) || is_decimal_digit(c),
                Native::CharIsWhiteSpace => c.is_whitespace(),
                Native::CharIsUpper => c.is_uppercase(),
                _ => c.is_lowercase(),
            });
            Some(Value::I32(i32::from(answer)))
        }
        Native::CharToUpper | Native::CharToLower => {
            let unit = vm.int32(arg(0)?)? as u16;
            Some(Value::I32(i32::from(map_case(unit, native == Native::CharToUpper))))
        }
        Native::IntegerToString(kind) => {
            let raw = number(vm)?;
            let (value, bits): (i128, u32) = match (kind, raw) {
                (IntKind::I8, Value::I32(x)) => (i128::from(x as i8), 8),
                (IntKind::U8, Value::I32(x)) => (i128::from(x as u8), 8),
                (IntKind::I16, Value::I32(x)) => (i128::from(x as i16), 16),
                (IntKind::U16, Value::I32(x)) => (i128::from(x as u16), 16),
                (IntKind::I32, Value::I32(x)) => (i128::from(x), 32),
                (IntKind::U32, Value::I32(x)) => (i128::from(x as u32), 32),
                (IntKind::I64, Value::I64(x)) => (i128::from(x), 64),
                (IntKind::U64, Value::I64(x)) => (i128::from(x as u64), 64),
                _ => return Err(vm.invalid("integer ToString on a value of another width")),
            };
            let format = vm.string_units(arg(1)?)?;
            match format_integer(value, bits, format.as_deref()) {
                Ok(text) => Some(vm.new_string_from(&text)?),
                Err(FormatFailure::Bad) => return Err(vm.exception("System.FormatException")),
                Err(FormatFailure::Unsupported(spec)) => {
                    return Err(VmError::Unsupported {
                        what: format!("numeric format \"{spec}\" in {} (phase N4c)", vm.location()),
                    });
                }
            }
        }
    })
}

fn text<H: Host>(vm: &mut Vm<'_, H>, text: alloc::string::String) -> Result<Option<Value>, VmError> {
    Ok(Some(vm.new_string_from(&text)?))
}

fn string_value<H: Host>(vm: &mut Vm<'_, H>, units: Vec<u16>) -> Result<Option<Value>, VmError> {
    Ok(Some(Value::Obj(Some(vm.heap.alloc(Object::String(units))?))))
}

/// Единицы строки `this`; `null` — `NullReferenceException`, как у вызова
/// метода на `null`.
fn this_units<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Vec<u16>, VmError> {
    vm.string_units(value)?.ok_or_else(|| vm.exception("System.NullReferenceException"))
}

/// Содержимое `char[]`; `None` у `null`.
fn char_array<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Option<Vec<u16>>, VmError> {
    match value {
        Value::Obj(None) => Ok(None),
        Value::Obj(Some(object)) => match vm.heap.get(object) {
            Some(Object::Array { items, .. }) => {
                let mut units = Vec::new();
                units.try_reserve_exact(items.len()).map_err(|_| VmError::OutOfMemory)?;
                for item in items {
                    units.push(match item {
                        Value::I32(unit) => *unit as u16,
                        _ => return Err(vm.invalid("char array holds a value that is not a char")),
                    });
                }
                Ok(Some(units))
            }
            _ => Err(vm.invalid("expected a char array")),
        },
        _ => Err(vm.invalid("expected a char array reference")),
    }
}

/// Отрезок `start..start + length` внутри `len`, если он там помещается.
fn checked_range(len: usize, start: i32, length: i32) -> Option<core::ops::Range<usize>> {
    let start = usize::try_from(start).ok()?;
    let end = start.checked_add(usize::try_from(length).ok()?)?;
    (end <= len).then_some(start..end)
}

fn start_index<H: Host>(vm: &Vm<'_, H>, len: usize, value: Value) -> Result<usize, VmError> {
    usize::try_from(vm.int32(value)?)
        .ok()
        .filter(|&start| start <= len)
        .ok_or_else(|| vm.exception("System.ArgumentOutOfRangeException"))
}

/// Смена регистра одной единицы UTF-16 — простое отображение Unicode, как у
/// .NET в инвариантном режиме: символ, который меняется больше чем на один
/// (`ß` → `SS`), или выходит за BMP, остаётся как был.
fn map_case(unit: u16, upper: bool) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else { return unit };
    let mut mapped = if upper { MappedCase::Upper(c.to_uppercase()) } else { MappedCase::Lower(c.to_lowercase()) };
    match (mapped.next(), mapped.next()) {
        (Some(single), None) => u16::try_from(u32::from(single)).unwrap_or(unit),
        _ => unit,
    }
}

enum MappedCase {
    Upper(core::char::ToUppercase),
    Lower(core::char::ToLowercase),
}

impl Iterator for MappedCase {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        match self {
            Self::Upper(inner) => inner.next(),
            Self::Lower(inner) => inner.next(),
        }
    }
}

/// Десятичная цифра любой письменности: числовой символ, чья группа из десяти
/// подряд начинается с нуля. Так устроены все блоки категории Nd в Unicode, а
/// дроби и римские цифры (тоже `is_numeric`) так не устроены.
fn is_decimal_digit(c: char) -> bool {
    if c.is_ascii_digit() {
        return true;
    }
    if c.is_ascii() || !c.is_numeric() {
        return false;
    }
    let code = u32::from(c);
    (0..10).any(|offset| {
        code.checked_sub(offset).and_then(char::from_u32).is_some_and(|zero| {
            (0..10).all(|i| char::from_u32(u32::from(zero) + i).is_some_and(char::is_numeric))
        })
    })
}

enum FormatFailure {
    /// Строка формата неверна — `FormatException`.
    Bad,
    /// Формат верный, но ещё не написан.
    Unsupported(alloc::string::String),
}

/// Целое по стандартному формату .NET: пусто и `G` — десятичное, `Dn` —
/// десятичное с нулями до `n` цифр, `Xn`/`xn` — шестнадцатеричное в
/// дополнительном коде ширины типа (`-1` у `int` — `FFFFFFFF`).
fn format_integer(value: i128, bits: u32, format: Option<&[u16]>) -> Result<alloc::string::String, FormatFailure> {
    let spec: alloc::string::String =
        char::decode_utf16(format.unwrap_or(&[]).iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
    let mut chars = spec.chars();
    let Some(letter) = chars.next() else {
        return Ok(format!("{value}"));
    };
    let rest = chars.as_str();
    if !letter.is_ascii_alphabetic() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        // Пользовательский формат (`0000`, `#,##0`) — фаза N4c.
        return Err(FormatFailure::Unsupported(spec));
    }
    let precision: usize = if rest.is_empty() { 0 } else { rest.parse().map_err(|_| FormatFailure::Bad)? };
    if precision > 999_999_999 {
        return Err(FormatFailure::Bad);
    }
    match letter.to_ascii_uppercase() {
        'G' if rest.is_empty() => Ok(format!("{value}")),
        'D' => {
            let digits = format!("{:0>precision$}", value.unsigned_abs());
            Ok(if value < 0 { format!("-{digits}") } else { digits })
        }
        'X' => {
            let bits_value = (value as u128) & ((1u128 << bits) - 1);
            Ok(if letter == 'X' { format!("{bits_value:0>precision$X}") } else { format!("{bits_value:0>precision$x}") })
        }
        'C' | 'E' | 'F' | 'G' | 'N' | 'P' | 'R' => Err(FormatFailure::Unsupported(spec)),
        _ => Err(FormatFailure::Bad),
    }
}
