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
    EnumNames,
    EnumValues,
    EnumIsFlags,
    EnumUnderlying,
    EnumToBits,
    EnumBox,
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
    /// `ToString()` и `ToString(string)` у `double` (`false`) и `float` (`true`).
    FloatToString(bool),
    FloatTryParse,
    DoubleToBits,
    BitsToDouble,
    SingleToBits,
    BitsToSingle,
    /// `Math` (`false`) или `MathF` (`true`) с одним аргументом.
    Math(MathOp, bool),
    /// `Math.Atan2`/`Math.Pow` и их `MathF`.
    Math2(MathOp2, bool),
    /// Файловые члены `System.IO.FileSystem` (фаза N5a).
    CurrentDirectory,
    ReadFile,
    WriteFile,
    RemoveFile,
    CreateDirectory,
    RemoveDirectory,
    RenamePath,
    StatPath,
    ListDirectory,
    DecodeUtf8,
    EncodeUtf8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathOp {
    Sqrt,
    Cbrt,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Log,
    Log10,
    Log2,
    Floor,
    Ceiling,
    Truncate,
    Round,
    Abs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathOp2 {
    Atan2,
    Pow,
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
    ("System.Enum::InternalGetNames(System.Type)", Native::EnumNames),
    ("System.Enum::InternalGetValues(System.Type)", Native::EnumValues),
    ("System.Enum::InternalIsFlags(System.Type)", Native::EnumIsFlags),
    ("System.Enum::InternalUnderlying(System.Type)", Native::EnumUnderlying),
    ("System.Enum::InternalToUInt64(object)", Native::EnumToBits),
    ("System.Enum::InternalBox(System.Type,uint64)", Native::EnumBox),
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
    ("System.Double::ToString()", Native::FloatToString(false)),
    ("System.Double::ToString(string)", Native::FloatToString(false)),
    ("System.Single::ToString()", Native::FloatToString(true)),
    ("System.Single::ToString(string)", Native::FloatToString(true)),
    ("System.Double::TryParseFloat(string,bool,float64&)", Native::FloatTryParse),
    ("System.BitConverter::DoubleToInt64Bits(float64)", Native::DoubleToBits),
    ("System.BitConverter::Int64BitsToDouble(int64)", Native::BitsToDouble),
    ("System.BitConverter::SingleToInt32Bits(float32)", Native::SingleToBits),
    ("System.BitConverter::Int32BitsToSingle(int32)", Native::BitsToSingle),
    ("System.Math::Sqrt(float64)", Native::Math(MathOp::Sqrt, false)),
    ("System.Math::Cbrt(float64)", Native::Math(MathOp::Cbrt, false)),
    ("System.Math::Sin(float64)", Native::Math(MathOp::Sin, false)),
    ("System.Math::Cos(float64)", Native::Math(MathOp::Cos, false)),
    ("System.Math::Tan(float64)", Native::Math(MathOp::Tan, false)),
    ("System.Math::Asin(float64)", Native::Math(MathOp::Asin, false)),
    ("System.Math::Acos(float64)", Native::Math(MathOp::Acos, false)),
    ("System.Math::Atan(float64)", Native::Math(MathOp::Atan, false)),
    ("System.Math::Sinh(float64)", Native::Math(MathOp::Sinh, false)),
    ("System.Math::Cosh(float64)", Native::Math(MathOp::Cosh, false)),
    ("System.Math::Tanh(float64)", Native::Math(MathOp::Tanh, false)),
    ("System.Math::Exp(float64)", Native::Math(MathOp::Exp, false)),
    ("System.Math::Log(float64)", Native::Math(MathOp::Log, false)),
    ("System.Math::Log10(float64)", Native::Math(MathOp::Log10, false)),
    ("System.Math::Log2(float64)", Native::Math(MathOp::Log2, false)),
    ("System.Math::Floor(float64)", Native::Math(MathOp::Floor, false)),
    ("System.Math::Ceiling(float64)", Native::Math(MathOp::Ceiling, false)),
    ("System.Math::Truncate(float64)", Native::Math(MathOp::Truncate, false)),
    ("System.Math::Round(float64)", Native::Math(MathOp::Round, false)),
    ("System.Math::Abs(float64)", Native::Math(MathOp::Abs, false)),
    ("System.Math::Abs(float32)", Native::Math(MathOp::Abs, true)),
    ("System.Math::Atan2(float64,float64)", Native::Math2(MathOp2::Atan2, false)),
    ("System.Math::Pow(float64,float64)", Native::Math2(MathOp2::Pow, false)),
    ("System.MathF::Sqrt(float32)", Native::Math(MathOp::Sqrt, true)),
    ("System.MathF::Sin(float32)", Native::Math(MathOp::Sin, true)),
    ("System.MathF::Cos(float32)", Native::Math(MathOp::Cos, true)),
    ("System.MathF::Tan(float32)", Native::Math(MathOp::Tan, true)),
    ("System.MathF::Exp(float32)", Native::Math(MathOp::Exp, true)),
    ("System.MathF::Log(float32)", Native::Math(MathOp::Log, true)),
    ("System.MathF::Floor(float32)", Native::Math(MathOp::Floor, true)),
    ("System.MathF::Ceiling(float32)", Native::Math(MathOp::Ceiling, true)),
    ("System.MathF::Truncate(float32)", Native::Math(MathOp::Truncate, true)),
    ("System.MathF::Round(float32)", Native::Math(MathOp::Round, true)),
    ("System.MathF::Atan2(float32,float32)", Native::Math2(MathOp2::Atan2, true)),
    ("System.MathF::Pow(float32,float32)", Native::Math2(MathOp2::Pow, true)),
    ("System.IO.FileSystem::GetCurrentDirectoryNative()", Native::CurrentDirectory),
    ("System.IO.FileSystem::ReadFile(string,uint8[]&)", Native::ReadFile),
    ("System.IO.FileSystem::WriteFile(string,uint8[],bool)", Native::WriteFile),
    ("System.IO.FileSystem::RemoveFile(string)", Native::RemoveFile),
    ("System.IO.FileSystem::CreateDirectory(string)", Native::CreateDirectory),
    ("System.IO.FileSystem::RemoveDirectory(string)", Native::RemoveDirectory),
    ("System.IO.FileSystem::Rename(string,string)", Native::RenamePath),
    ("System.IO.FileSystem::Stat(string,int64&)", Native::StatPath),
    ("System.IO.FileSystem::ListDirectory(string,string[]&)", Native::ListDirectory),
    ("System.Text.Encoding::DecodeUtf8(uint8[],int32,int32)", Native::DecodeUtf8),
    ("System.Text.Encoding::EncodeUtf8(string)", Native::EncodeUtf8),
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
        Native::EnumNames | Native::EnumValues => {
            let ty = vm.runtime_type(arg(0)?)?;
            let members = vm.enum_members(ty)?;
            let (element, items) = if native == Native::EnumNames {
                let mut names = Vec::new();
                names.try_reserve_exact(members.len()).map_err(|_| VmError::OutOfMemory)?;
                for (name, _) in &members {
                    names.push(vm.new_string_from(name)?);
                }
                (vm.corelib_type("System.String")?, crate::heap::Items::Values(names))
            } else {
                let mut values = Vec::new();
                values.try_reserve_exact(members.len()).map_err(|_| VmError::OutOfMemory)?;
                values.extend(members.iter().map(|(_, bits)| *bits as i64));
                (vm.prim_type(Prim::U8)?, crate::heap::Items::I64(values))
            };
            let array_ty = vm.array_of(element)?;
            Some(Value::Obj(Some(vm.heap.alloc(Object::Array { ty: array_ty, items })?)))
        }
        Native::EnumIsFlags => {
            let ty = vm.runtime_type(arg(0)?)?;
            Some(Value::I32(i32::from(vm.enum_is_flags(ty)?)))
        }
        Native::EnumUnderlying => {
            let ty = vm.runtime_type(arg(0)?)?;
            Some(Value::I32(crate::enums::enum_width(vm.enum_prim(ty)?)))
        }
        Native::EnumToBits => {
            let Some((ty, value)) = vm.boxed(arg(0)?) else {
                return Err(vm.exception("System.NullReferenceException"));
            };
            let prim = match vm.types[ty.0 as usize].kind {
                crate::types::Kind::Enum(prim) | crate::types::Kind::Prim(prim) => prim,
                _ => return Err(vm.exception("System.ArgumentException")),
            };
            let bits = match value {
                Value::I32(x) => u64::from(x as u32),
                Value::I64(x) | Value::Native(x) => x as u64,
                _ => return Err(vm.invalid("enum value that is not an integer")),
            };
            Some(Value::I64((bits & crate::enums::enum_mask(prim)) as i64))
        }
        Native::EnumBox => {
            let ty = vm.runtime_type(arg(0)?)?;
            let prim = vm.enum_prim(ty)?;
            let bits = vm.int64(arg(1)?)? as u64;
            // Как значение этой ширины на стеке: мелкие — `int32` с расширением
            // знака или нулями.
            let value = match prim {
                Prim::I8 | Prim::U8 => Value::I64(bits as i64),
                Prim::I | Prim::U => Value::Native(bits as i64),
                _ => prim.narrow(Value::I32(bits as u32 as i32)),
            };
            Some(vm.box_value(ty, value)?)
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
            Some(Value::Obj(Some(vm.heap.alloc(Object::Array { ty, items: crate::heap::Items::U16(units) })?)))
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
            let format = vm.string_units(arg(1)?)?.unwrap_or_default();
            let printed = crate::number::format_integer(value, bits, &format);
            formatted(vm, printed)?
        }
        Native::FloatToString(single) => {
            let value = float(vm, number(vm)?)?;
            // `ToString()` без формата — то же, что с пустым.
            let format = match args.get(1) {
                Some(&format) => vm.string_units(format)?.unwrap_or_default(),
                None => Vec::new(),
            };
            let printed = if single {
                crate::number::format_single(value as f32, &format)
            } else {
                crate::number::format_double(value, &format)
            };
            formatted(vm, printed)?
        }
        Native::FloatTryParse => {
            let text = vm.string_units(arg(0)?)?.unwrap_or_default();
            let single = vm.int32(arg(1)?)? != 0;
            let Value::Ptr(result) = arg(2)? else {
                return Err(vm.invalid("out argument is not a pointer"));
            };
            let parsed = crate::number::parse_float(&text, single);
            vm.store(result, Value::F(parsed.unwrap_or(0.0)))?;
            Some(Value::I32(i32::from(parsed.is_some())))
        }
        Native::DoubleToBits => Some(Value::I64(float(vm, arg(0)?)?.to_bits() as i64)),
        Native::BitsToDouble => Some(Value::F(f64::from_bits(vm.int64(arg(0)?)? as u64))),
        Native::SingleToBits => Some(Value::I32((float(vm, arg(0)?)? as f32).to_bits() as i32)),
        Native::BitsToSingle => Some(Value::F32(f32::from_bits(vm.int32(arg(0)?)? as u32))),
        Native::Math(op, single) => {
            let x = float(vm, arg(0)?)?;
            Some(if single { Value::F32(math_f32(op, x as f32)) } else { Value::F(math_f64(op, x)) })
        }
        Native::Math2(op, single) => {
            let (x, y) = (float(vm, arg(0)?)?, float(vm, arg(1)?)?);
            Some(if single {
                let (x, y) = (x as f32, y as f32);
                Value::F32(match op {
                    MathOp2::Atan2 => libm::atan2f(x, y),
                    MathOp2::Pow => libm::powf(x, y),
                })
            } else {
                Value::F(match op {
                    MathOp2::Atan2 => libm::atan2(x, y),
                    MathOp2::Pow => libm::pow(x, y),
                })
            })
        }
        Native::CurrentDirectory => {
            let directory = vm.host.current_dir();
            Some(vm.new_string_from(&directory)?)
        }
        Native::ReadFile => {
            let path = path_arg(vm, arg(0)?)?;
            let Value::Ptr(out) = arg(1)? else {
                return Err(vm.invalid("out argument is not a pointer"));
            };
            match vm.host.read_file(&path) {
                Ok(data) => {
                    let array = byte_array(vm, data)?;
                    vm.store(out, array)?;
                    Some(Value::I32(0))
                }
                Err(failure) => {
                    vm.store(out, Value::Obj(None))?;
                    Some(Value::I32(failure.code()))
                }
            }
        }
        Native::WriteFile => {
            let path = path_arg(vm, arg(0)?)?;
            let data = bytes_of(vm, arg(1)?)?;
            let append = vm.int32(arg(2)?)? != 0;
            status(vm.host.write_file(&path, &data, append))
        }
        Native::RemoveFile => {
            let path = path_arg(vm, arg(0)?)?;
            status(vm.host.remove_file(&path))
        }
        Native::CreateDirectory => {
            let path = path_arg(vm, arg(0)?)?;
            status(vm.host.create_dir(&path))
        }
        Native::RemoveDirectory => {
            let path = path_arg(vm, arg(0)?)?;
            status(vm.host.remove_dir(&path))
        }
        Native::RenamePath => {
            let from = path_arg(vm, arg(0)?)?;
            let to = path_arg(vm, arg(1)?)?;
            status(vm.host.rename(&from, &to))
        }
        Native::StatPath => {
            let path = path_arg(vm, arg(0)?)?;
            let Value::Ptr(out) = arg(1)? else {
                return Err(vm.invalid("out argument is not a pointer"));
            };
            let (code, size) = match vm.host.stat(&path) {
                Ok((crate::FileKind::File, size)) => (1, size),
                Ok((crate::FileKind::Directory, size)) => (2, size),
                Err(failure) => (-failure.code(), 0),
            };
            vm.store(out, Value::I64(size as i64))?;
            Some(Value::I32(code))
        }
        Native::ListDirectory => {
            let path = path_arg(vm, arg(0)?)?;
            let Value::Ptr(out) = arg(1)? else {
                return Err(vm.invalid("out argument is not a pointer"));
            };
            match vm.host.list_dir(&path) {
                Ok(names) => {
                    let mut values = Vec::new();
                    values.try_reserve_exact(names.len()).map_err(|_| VmError::OutOfMemory)?;
                    for name in &names {
                        values.push(vm.new_string_from(name)?);
                    }
                    let string = vm.corelib_type("System.String")?;
                    let ty = vm.array_of(string)?;
                    let array = vm.heap.alloc(Object::Array { ty, items: crate::heap::Items::Values(values) })?;
                    vm.store(out, Value::Obj(Some(array)))?;
                    Some(Value::I32(0))
                }
                Err(failure) => {
                    vm.store(out, Value::Obj(None))?;
                    Some(Value::I32(failure.code()))
                }
            }
        }
        // Неверные последовательности — U+FFFD по «наибольшей части», как у .NET.
        Native::DecodeUtf8 => {
            let data = bytes_of(vm, arg(0)?)?;
            let start = usize::try_from(vm.int32(arg(1)?)?).unwrap_or(usize::MAX);
            let count = usize::try_from(vm.int32(arg(2)?)?).unwrap_or(usize::MAX);
            let Some(slice) = start.checked_add(count).and_then(|end| data.get(start..end)) else {
                return Err(vm.exception("System.ArgumentOutOfRangeException"));
            };
            let text = alloc::string::String::from_utf8_lossy(slice);
            Some(vm.new_string_from(&text)?)
        }
        // Одинокая суррогатная половинка — U+FFFD, как у .NET.
        Native::EncodeUtf8 => {
            let units = this_units(vm, arg(0)?)?;
            let text: alloc::string::String =
                char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect();
            Some(byte_array(vm, text.into_bytes())?)
        }
    })
}

/// Путь-аргумент файлового члена. `null` отсекает C#, но среда не верит.
fn path_arg<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<alloc::string::String, VmError> {
    let units = vm.string_units(value)?.ok_or_else(|| vm.exception("System.ArgumentNullException"))?;
    Ok(alloc::string::String::from_utf16_lossy(&units))
}

/// Копия содержимого `byte[]`: хосту нельзя отдать ссылку внутрь кучи, пока он
/// сам занимает среду.
fn bytes_of<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<Vec<u8>, VmError> {
    match value {
        Value::Obj(Some(object)) => match vm.heap.get(object) {
            Some(Object::Array { items: crate::heap::Items::U8(bytes), .. }) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(bytes.len()).map_err(|_| VmError::OutOfMemory)?;
                copy.extend_from_slice(bytes);
                Ok(copy)
            }
            _ => Err(vm.invalid("expected a byte array")),
        },
        Value::Obj(None) => Err(vm.exception("System.ArgumentNullException")),
        _ => Err(vm.invalid("expected a byte array reference")),
    }
}

fn byte_array<H: Host>(vm: &mut Vm<'_, H>, data: Vec<u8>) -> Result<Value, VmError> {
    let element = vm.prim_type(Prim::U1)?;
    let ty = vm.array_of(element)?;
    Ok(Value::Obj(Some(vm.heap.alloc(Object::Array { ty, items: crate::heap::Items::U8(data) })?)))
}

/// Итог файлового члена для C#: 0 или код отказа.
fn status(result: Result<(), crate::IoError>) -> Option<Value> {
    Some(Value::I32(result.map_or_else(crate::IoError::code, |()| 0)))
}

/// Строка по формату или исключение, которое бросил бы .NET.
fn formatted<H: Host>(
    vm: &mut Vm<'_, H>,
    printed: Result<Vec<u16>, crate::number::FormatError>,
) -> Result<Option<Value>, VmError> {
    match printed {
        Ok(units) => string_value(vm, units),
        Err(crate::number::FormatError::Bad) => Err(vm.exception("System.FormatException")),
        Err(crate::number::FormatError::TooLong) => Err(VmError::OutOfMemory),
    }
}

/// Дробное число любой точности как `f64` — `float32` расширяется без потерь.
fn float<H: Host>(vm: &Vm<'_, H>, value: Value) -> Result<f64, VmError> {
    match value {
        Value::F(x) => Ok(x),
        Value::F32(x) => Ok(f64::from(x)),
        _ => Err(vm.invalid("expected a floating point number")),
    }
}

/// Округление `Math.Round` — к ближайшему, половина к чётному (`roundeven`),
/// а не `round` из C, у которого половина уходит от нуля.
fn math_f64(op: MathOp, x: f64) -> f64 {
    match op {
        MathOp::Sqrt => libm::sqrt(x),
        MathOp::Cbrt => libm::cbrt(x),
        MathOp::Sin => libm::sin(x),
        MathOp::Cos => libm::cos(x),
        MathOp::Tan => libm::tan(x),
        MathOp::Asin => libm::asin(x),
        MathOp::Acos => libm::acos(x),
        MathOp::Atan => libm::atan(x),
        MathOp::Sinh => libm::sinh(x),
        MathOp::Cosh => libm::cosh(x),
        MathOp::Tanh => libm::tanh(x),
        MathOp::Exp => libm::exp(x),
        MathOp::Log => libm::log(x),
        MathOp::Log10 => libm::log10(x),
        MathOp::Log2 => libm::log2(x),
        MathOp::Floor => libm::floor(x),
        MathOp::Ceiling => libm::ceil(x),
        MathOp::Truncate => libm::trunc(x),
        MathOp::Round => libm::roundeven(x),
        MathOp::Abs => libm::fabs(x),
    }
}

fn math_f32(op: MathOp, x: f32) -> f32 {
    match op {
        MathOp::Sqrt => libm::sqrtf(x),
        MathOp::Cbrt => libm::cbrtf(x),
        MathOp::Sin => libm::sinf(x),
        MathOp::Cos => libm::cosf(x),
        MathOp::Tan => libm::tanf(x),
        MathOp::Asin => libm::asinf(x),
        MathOp::Acos => libm::acosf(x),
        MathOp::Atan => libm::atanf(x),
        MathOp::Sinh => libm::sinhf(x),
        MathOp::Cosh => libm::coshf(x),
        MathOp::Tanh => libm::tanhf(x),
        MathOp::Exp => libm::expf(x),
        MathOp::Log => libm::logf(x),
        MathOp::Log10 => libm::log10f(x),
        MathOp::Log2 => libm::log2f(x),
        MathOp::Floor => libm::floorf(x),
        MathOp::Ceiling => libm::ceilf(x),
        MathOp::Truncate => libm::truncf(x),
        MathOp::Round => libm::roundevenf(x),
        MathOp::Abs => libm::fabsf(x),
    }
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
            Some(Object::Array { items: crate::heap::Items::U16(chars), .. }) => {
                let mut units = Vec::new();
                units.try_reserve_exact(chars.len()).map_err(|_| VmError::OutOfMemory)?;
                units.extend_from_slice(chars);
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

