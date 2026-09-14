//! Типы и методы среды: то, во что превращаются строки `TypeDef` и `MethodDef`,
//! когда программа до них дотягивается (фаза N3a).
//!
//! # Тип среды и тип метаданных — не одно и то же
//!
//! В метаданных `List<T>` — одна строка `TypeDef`. В среде `List<int>` и
//! `List<string>` — два разных типа с разной раскладкой полей: у первого поле
//! `T` хранит число, у второго — ссылку. Поэтому тип среды ([`TypeId`]) — это
//! пара «определение и аргументы», а метод ([`MethodId`]) — «определение,
//! тип-владелец с аргументами и аргументы самого метода». Тело метода одно на
//! все экземпляры, различаются только разрешённые токены.
//!
//! # Лениво
//!
//! Тип загружается при первом упоминании, метод разбирается при первом вызове.
//! Сборка `System.Windows.Forms` — сотни типов, и программа с одной кнопкой
//! не должна платить за все.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::ExceptionClause;

use crate::natives::Native;
use crate::value::Value;

/// Номер типа среды.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TypeId(pub u32);

/// Номер метода среды.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct MethodId(pub u32);

/// Номер сборки среди загруженных.
pub(crate) type Asm = u16;

/// Базовая библиотека всегда первая.
pub(crate) const CORELIB: Asm = 0;
/// Программа — вторая.
pub(crate) const PROGRAM: Asm = 1;

/// Флаги `TypeDef.Flags` (ECMA-335 II.23.1.15).
pub(crate) const TYPE_INTERFACE: u32 = 0x20;
pub(crate) const TYPE_BEFORE_FIELD_INIT: u32 = 0x0010_0000;

/// Флаги `MethodDef.Flags` (II.23.1.10).
pub(crate) const METHOD_STATIC: u16 = 0x0010;
pub(crate) const METHOD_VIRTUAL: u16 = 0x0040;
pub(crate) const METHOD_NEW_SLOT: u16 = 0x0100;
pub(crate) const METHOD_ABSTRACT: u16 = 0x0400;

/// Флаг `MethodDef.ImplFlags`: тело даёт среда (`MethodImplOptions.InternalCall`).
pub(crate) const IMPL_INTERNAL_CALL: u16 = 0x1000;

/// Флаги `Field.Flags` (II.23.1.5).
pub(crate) const FIELD_STATIC: u16 = 0x0010;
pub(crate) const FIELD_LITERAL: u16 = 0x0040;

/// Примитив — то, что на стеке лежит числом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Prim {
    Bool,
    Char,
    I1,
    U1,
    I2,
    U2,
    I4,
    U4,
    I8,
    U8,
    I,
    U,
    R4,
    R8,
}

pub(crate) const PRIMS: [Prim; 14] = [
    Prim::Bool,
    Prim::Char,
    Prim::I1,
    Prim::U1,
    Prim::I2,
    Prim::U2,
    Prim::I4,
    Prim::U4,
    Prim::I8,
    Prim::U8,
    Prim::I,
    Prim::U,
    Prim::R4,
    Prim::R8,
];

impl Prim {
    /// Имя типа в `System`.
    pub(crate) const fn type_name(self) -> &'static str {
        match self {
            Self::Bool => "Boolean",
            Self::Char => "Char",
            Self::I1 => "SByte",
            Self::U1 => "Byte",
            Self::I2 => "Int16",
            Self::U2 => "UInt16",
            Self::I4 => "Int32",
            Self::U4 => "UInt32",
            Self::I8 => "Int64",
            Self::U8 => "UInt64",
            Self::I => "IntPtr",
            Self::U => "UIntPtr",
            Self::R4 => "Single",
            Self::R8 => "Double",
        }
    }

    /// Имя в записи сигнатуры (`clr_meta::sig::write_type`).
    pub(crate) const fn sig_name(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::Char => "char",
            Self::I1 => "int8",
            Self::U1 => "uint8",
            Self::I2 => "int16",
            Self::U2 => "uint16",
            Self::I4 => "int32",
            Self::U4 => "uint32",
            Self::I8 => "int64",
            Self::U8 => "uint64",
            Self::I => "native int",
            Self::U => "native uint",
            Self::R4 => "float32",
            Self::R8 => "float64",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        PRIMS.iter().copied().find(|p| p.type_name() == name)
    }

    /// Код элемента сигнатуры в примитив.
    pub(crate) const fn from_element(code: u8) -> Option<Self> {
        use clr_meta::sig::elem;
        Some(match code {
            elem::BOOLEAN => Self::Bool,
            elem::CHAR => Self::Char,
            elem::I1 => Self::I1,
            elem::U1 => Self::U1,
            elem::I2 => Self::I2,
            elem::U2 => Self::U2,
            elem::I4 => Self::I4,
            elem::U4 => Self::U4,
            elem::I8 => Self::I8,
            elem::U8 => Self::U8,
            elem::I => Self::I,
            elem::U => Self::U,
            elem::R4 => Self::R4,
            elem::R8 => Self::R8,
            _ => return None,
        })
    }

    pub(crate) const fn zero(self) -> Value {
        match self {
            Self::I8 | Self::U8 => Value::I64(0),
            Self::I | Self::U => Value::Native(0),
            Self::R4 | Self::R8 => Value::F(0.0),
            _ => Value::I32(0),
        }
    }

    /// Размер значения в байтах — для данных инициализатора массива.
    pub(crate) const fn size(self) -> usize {
        match self {
            Self::Bool | Self::I1 | Self::U1 => 1,
            Self::Char | Self::I2 | Self::U2 => 2,
            Self::I4 | Self::U4 | Self::R4 => 4,
            _ => 8,
        }
    }

    /// Сузить значение при записи в место этого типа (ECMA-335 III.1.6):
    /// `byte` хранит младший байт, `short` — два с расширением знака.
    pub(crate) fn narrow(self, value: Value) -> Value {
        match (self, value) {
            (Self::Bool | Self::U1, Value::I32(x)) => Value::I32(i32::from(x as u8)),
            (Self::I1, Value::I32(x)) => Value::I32(i32::from(x as i8)),
            (Self::I2, Value::I32(x)) => Value::I32(i32::from(x as i16)),
            (Self::U2 | Self::Char, Value::I32(x)) => Value::I32(i32::from(x as u16)),
            (Self::I8 | Self::U8, Value::Native(x)) => Value::I64(x),
            (Self::I, Value::I32(x)) => Value::Native(i64::from(x)),
            (Self::U, Value::I32(x)) => Value::Native(i64::from(x as u32)),
            (Self::R4, Value::F(x)) => Value::F(f64::from(x as f32)),
            _ => value,
        }
    }
}

/// Чем тип является для среды.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Class,
    Interface,
    Struct,
    /// Перечисление: на стеке — значение базового примитива.
    Enum(Prim),
    Prim(Prim),
    /// Одномерный массив с этим типом элемента.
    Array(TypeId),
}

/// Как место (поле, переменная, элемент) хранит значение.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Store {
    Prim(Prim),
    Ref,
    Struct(TypeId),
    /// Управляемый указатель (`ref`-переменная).
    ByRef,
}

impl Store {
    pub(crate) fn narrow(self, value: Value) -> Value {
        match self {
            Self::Prim(p) => p.narrow(value),
            _ => value,
        }
    }
}

/// Поле экземпляра в раскладке.
#[derive(Clone, Copy)]
pub(crate) struct FieldSlot {
    pub asm: Asm,
    pub row: u32,
    pub store: Store,
}

/// Ячейка таблицы виртуальных методов.
#[derive(Clone)]
pub(crate) struct VSlot {
    /// Имя с сигнатурой, в которой параметры типа подставлены: по нему
    /// переопределение находит ячейку базового класса.
    pub key: String,
    pub method: MethodId,
}

pub(crate) struct Type {
    /// Полное имя, как его печатает `Type.FullName`: вложенные через `+`.
    pub name: String,
    /// Определение; у массивов его нет.
    pub def: Option<(Asm, u32)>,
    pub args: Rc<[TypeId]>,
    pub kind: Kind,
    pub base: Option<TypeId>,
    pub flags: u32,
    /// Поля экземпляра, базовых классов первыми.
    pub fields: Vec<FieldSlot>,
    /// Статические поля этого определения: строка `Field` и как хранится.
    pub static_slots: Vec<FieldSlot>,
    /// Значения статических полей; заводятся при первом обращении.
    pub statics: Vec<Value>,
    pub vtable: Vec<VSlot>,
    /// Все интерфейсы, включая унаследованные.
    pub interfaces: Vec<TypeId>,
    /// Интерфейсы, перечисленные у самого определения (`InterfaceImpl`).
    pub declared_interfaces: Vec<TypeId>,
    /// Статический конструктор уже запускался (или его нет).
    pub initialized: bool,
}

impl Type {
    pub(crate) fn is_value_type(&self) -> bool {
        matches!(self.kind, Kind::Struct | Kind::Enum(_) | Kind::Prim(_))
    }

    pub(crate) fn store(&self, id: TypeId) -> Store {
        match self.kind {
            Kind::Prim(p) | Kind::Enum(p) => Store::Prim(p),
            Kind::Struct => Store::Struct(id),
            _ => Store::Ref,
        }
    }
}

/// Тело метода, разобранное для этого экземпляра.
pub(crate) struct Body<'a> {
    pub code: &'a [u8],
    pub max_stack: u16,
    pub locals: Vec<Store>,
    /// Обработчики исключений. Тело с ними пока не разбирается вовсе
    /// (`Vm::body` отказывает с названием фазы); читать их будет N3b.
    #[allow(dead_code)]
    pub clauses: Vec<ExceptionClause>,
}

pub(crate) struct MethodInfo<'a> {
    pub asm: Asm,
    pub row: u32,
    pub owner: TypeId,
    pub margs: Rc<[TypeId]>,
    pub name: &'a str,
    pub flags: u16,
    pub impl_flags: u16,
    pub rva: u32,
    pub sig: &'a [u8],
    pub has_this: bool,
    pub params: u32,
    pub returns: bool,
    /// Член, написанный в Rust (`InternalCall`); `None` у метода с телом IL.
    pub native: Option<Native>,
    pub body: Option<Rc<Body<'a>>>,
}

impl MethodInfo<'_> {
    pub(crate) const fn is_virtual(&self) -> bool {
        self.flags & METHOD_VIRTUAL != 0
    }

    pub(crate) const fn is_static(&self) -> bool {
        self.flags & METHOD_STATIC != 0
    }
}

/// Во что разрешился токен в теле метода.
#[derive(Clone, Copy)]
pub(crate) enum Resolved {
    Method(MethodId),
    Type(TypeId),
    Field(FieldRef),
}

/// Поле, найденное по токену.
#[derive(Clone, Copy)]
pub(crate) struct FieldRef {
    /// Тип, объявивший поле (с аргументами).
    pub owner: TypeId,
    /// Номер среди полей экземпляра или среди статических полей владельца.
    pub index: u32,
    pub store: Store,
    /// Строка `Field` — для `ldtoken`.
    pub asm: Asm,
    pub row: u32,
}
