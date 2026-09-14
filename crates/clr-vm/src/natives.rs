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
    })
}

fn text<H: Host>(vm: &mut Vm<'_, H>, text: alloc::string::String) -> Result<Option<Value>, VmError> {
    Ok(Some(vm.new_string_from(&text)?))
}
