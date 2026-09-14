//! Члены базовой библиотеки, написанные здесь, а не на C#.
//!
//! # Почему в Rust
//!
//! Базовая библиотека своей среды будет на C# (фаза N4) — но её IL нечем
//! исполнять, пока у интерпретатора нет объектов (N3). Поэтому первые члены,
//! без которых не напечатать и строки, живут здесь. Их ровно столько, сколько
//! нашлось в ссылках образцов `hello` и `arith` (`MemberRef` в
//! `cargo xtask clr-check`), плюс ближайшие соседи тех же перегрузок.
//!
//! # Как находятся
//!
//! По полному имени с типами параметров — `System.Console::WriteLine(string)`.
//! Сборка, из которой ссылка пришла (`System.Console` или `System.Runtime`), не
//! участвует: в .NET тип переезжает между сборками пересылкой, и одна и та же
//! программа, собранная под разные версии, ссылается на `Console` то через
//! одну, то через другую.

use alloc::format;
use alloc::vec::Vec;

use crate::heap::Object;
use crate::value::Value;
use crate::vm::Vm;
use crate::{Host, VmError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Native {
    WriteLineString,
    WriteLineEmpty,
    WriteLineI32,
    WriteLineI64,
    WriteLineU32,
    WriteLineBool,
    WriteLineChar,
    WriteString,
    WriteI32,
    WriteChar,
    SetOutputEncoding,
    EncodingUtf8,
    I32ToString,
    I64ToString,
    U32ToString,
    U64ToString,
    I16ToString,
    U16ToString,
    ByteToString,
    SByteToString,
    BoolToString,
    CharToString,
    Concat2,
    Concat3,
    Concat4,
    StringLength,
    StringChar,
    StringEquals,
    StringNotEquals,
    ObjectCtor,
}

const TABLE: &[(&str, Native)] = &[
    ("System.Console::WriteLine(string)", Native::WriteLineString),
    ("System.Console::WriteLine()", Native::WriteLineEmpty),
    ("System.Console::WriteLine(int32)", Native::WriteLineI32),
    ("System.Console::WriteLine(int64)", Native::WriteLineI64),
    ("System.Console::WriteLine(uint32)", Native::WriteLineU32),
    ("System.Console::WriteLine(bool)", Native::WriteLineBool),
    ("System.Console::WriteLine(char)", Native::WriteLineChar),
    ("System.Console::Write(string)", Native::WriteString),
    ("System.Console::Write(int32)", Native::WriteI32),
    ("System.Console::Write(char)", Native::WriteChar),
    ("System.Console::set_OutputEncoding(System.Text.Encoding)", Native::SetOutputEncoding),
    ("System.Text.Encoding::get_UTF8()", Native::EncodingUtf8),
    ("System.Int32::ToString()", Native::I32ToString),
    ("System.Int64::ToString()", Native::I64ToString),
    ("System.UInt32::ToString()", Native::U32ToString),
    ("System.UInt64::ToString()", Native::U64ToString),
    ("System.Int16::ToString()", Native::I16ToString),
    ("System.UInt16::ToString()", Native::U16ToString),
    ("System.Byte::ToString()", Native::ByteToString),
    ("System.SByte::ToString()", Native::SByteToString),
    ("System.Boolean::ToString()", Native::BoolToString),
    ("System.Char::ToString()", Native::CharToString),
    ("System.String::Concat(string,string)", Native::Concat2),
    ("System.String::Concat(string,string,string)", Native::Concat3),
    ("System.String::Concat(string,string,string,string)", Native::Concat4),
    ("System.String::get_Length()", Native::StringLength),
    ("System.String::get_Chars(int32)", Native::StringChar),
    ("System.String::op_Equality(string,string)", Native::StringEquals),
    ("System.String::op_Inequality(string,string)", Native::StringNotEquals),
    ("System.Object::.ctor()", Native::ObjectCtor),
];

pub(crate) fn lookup(key: &str) -> Option<Native> {
    TABLE.iter().find(|(name, _)| *name == key).map(|(_, native)| *native)
}

impl Native {
    /// Сколько значений член снимает со стека — вместе с `this`.
    pub(crate) const fn arguments(self) -> usize {
        match self {
            Self::WriteLineEmpty | Self::EncodingUtf8 => 0,
            Self::Concat2 | Self::StringChar | Self::StringEquals | Self::StringNotEquals => 2,
            Self::Concat3 => 3,
            Self::Concat4 => 4,
            _ => 1,
        }
    }
}

/// Выполнить член. `Some` — значение, которое он вернул.
pub(crate) fn call<H: Host>(
    vm: &mut Vm<'_, H>,
    native: Native,
    args: &[Value],
) -> Result<Option<Value>, VmError> {
    let arg = |index: usize| args.get(index).copied().ok_or(VmError::StackOverflow);
    Ok(match native {
        Native::WriteLineString | Native::WriteString => {
            let text = vm.string_units(arg(0)?)?.unwrap_or_default();
            vm.print_units(&text, native == Native::WriteLineString);
            None
        }
        Native::WriteLineEmpty => {
            vm.print_units(&[], true);
            None
        }
        Native::WriteLineI32 | Native::WriteI32 => {
            let value = vm.int32(arg(0)?)?;
            vm.print_text(&format!("{value}"), native == Native::WriteLineI32);
            None
        }
        Native::WriteLineI64 => {
            let value = vm.int64(arg(0)?)?;
            vm.print_text(&format!("{value}"), true);
            None
        }
        Native::WriteLineU32 => {
            let value = vm.int32(arg(0)?)? as u32;
            vm.print_text(&format!("{value}"), true);
            None
        }
        Native::WriteLineBool => {
            let value = vm.int32(arg(0)?)? != 0;
            vm.print_text(if value { "True" } else { "False" }, true);
            None
        }
        Native::WriteLineChar | Native::WriteChar => {
            let unit = vm.int32(arg(0)?)? as u16;
            vm.print_units(&[unit], native == Native::WriteLineChar);
            None
        }
        // Вывод своей среды всегда UTF-8, так что смена кодировки — пустая
        // операция. Смысл у неё один: программа, написанная для настоящего
        // dotnet на русской Windows, работает здесь без правки.
        Native::SetOutputEncoding => None,
        Native::EncodingUtf8 => Some(vm.utf8_encoding()?),
        // `call instance string int32::ToString()` получает `this` указателем
        // на значение: компилятор кладёт число в переменную и берёт её адрес.
        Native::I32ToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)?;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::I64ToString => {
            let value = vm.int64(vm.deref(arg(0)?)?)?;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::U32ToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? as u32;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::U64ToString => {
            let value = vm.int64(vm.deref(arg(0)?)?)? as u64;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::I16ToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? as i16;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::U16ToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? as u16;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::ByteToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? as u8;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::SByteToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? as i8;
            Some(vm.new_string_from(&format!("{value}"))?)
        }
        Native::BoolToString => {
            let value = vm.int32(vm.deref(arg(0)?)?)? != 0;
            Some(vm.new_string_from(if value { "True" } else { "False" })?)
        }
        Native::CharToString => {
            let unit = vm.int32(vm.deref(arg(0)?)?)? as u16;
            Some(Value::Obj(Some(vm.heap.string(core::iter::once(unit))?)))
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
        Native::StringEquals | Native::StringNotEquals => {
            let left = vm.string_units(arg(0)?)?;
            let right = vm.string_units(arg(1)?)?;
            let equal = left == right;
            Some(Value::I32(i32::from(equal == (native == Native::StringEquals))))
        }
        Native::ObjectCtor => None,
    })
}
