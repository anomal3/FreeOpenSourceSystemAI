//! Своя среда выполнения .NET: интерпретатор IL (веха v0.7c, фаза N2).
//!
//! # Что умеет в фазе N2
//!
//! Программу без объектов: статические методы, аргументы и локальные
//! переменные, арифметику `int32`/`int64`/`native int`/`float64` по таблицам
//! ECMA-335, ветвления и `switch`, управляемые указатели на локальные
//! переменные (ими компилятор вызывает `int.ToString()`), строки из кучи `#US`,
//! одномерные массивы и горсть членов базовой библиотеки, написанных здесь же
//! ([`natives`]). Этого хватает шаблону `dotnet new console` и образцу `arith`.
//!
//! Чего нет — объектов и полей, виртуальных вызовов, значимых типов,
//! исключений и обобщений (фаза N3), базовой библиотеки на C# (N4) и сборщика
//! мусора: объекты живут до конца программы. Встреча с неподдержанным — не
//! падение и не молчание, а [`VmError`] с полным именем члена или кодом
//! инструкции и местом, где она стоит.
//!
//! # Почему явный стек кадров, а не рекурсия
//!
//! Вызов метода C# не вызывает функцию Rust: кадр кладётся в вектор, и цикл
//! исполнения продолжает уже его. У программы на FreeOS стек небольшой и
//! фиксированный, и рекурсивный интерпретатор падал бы на глубине, до которой
//! настоящий `dotnet` доходит спокойно. Здесь глубину ограничивает
//! [`vm::MAX_FRAMES`] — и превышение называется переполнением стека, как в .NET.
//!
//! # Почему `no_std`
//!
//! Один и тот же крейт работает в `/bin/dotnet` на FreeOS и в
//! `cargo xtask clr-check` на машине разработчика, где его вывод сравнивается с
//! выводом настоящего `dotnet` на тех же сборках. Всё, что нужно ему снаружи, —
//! куда печатать ([`Host`]).

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod heap;
mod natives;
mod ops;
mod value;
pub mod vm;

#[cfg(test)]
mod tests;

use alloc::string::String;
use core::fmt;

pub use value::{ObjRef, Pointer, Value};
pub use vm::Vm;

/// То, что среда берёт у системы, на которой работает.
pub trait Host {
    /// Напечатать текст в стандартный вывод программы.
    fn write_out(&mut self, text: &str);
}

/// Почему программа не выполнилась до конца.
#[derive(Debug)]
pub enum VmError {
    /// Сборка повреждена или не разбирается.
    Meta(clr_meta::Error),
    /// У сборки нет точки входа — это библиотека, а не программа.
    NoEntryPoint,
    /// Программа просит то, чего среда пока не умеет.
    Unsupported { what: String },
    /// Член базовой библиотеки, которого ещё нет.
    MissingMember { name: String },
    /// IL, которого не пропустил бы верификатор: стек пуст, типы не сходятся.
    Invalid { what: &'static str, at: String },
    /// Исключение .NET, которое программа не поймала.
    Exception { name: &'static str, at: String },
    /// Кончилась память.
    OutOfMemory,
    /// Вызовы вложены глубже, чем разрешено.
    StackOverflow,
}

impl fmt::Display for VmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Meta(error) => write!(f, "malformed assembly: {error:?}"),
            Self::NoEntryPoint => write!(f, "the assembly has no entry point"),
            Self::Unsupported { what } => write!(f, "not supported yet: {what}"),
            Self::MissingMember { name } => {
                write!(f, "missing member {name} (not in the base library yet)")
            }
            Self::Invalid { what, at } => write!(f, "invalid IL: {what} in {at}"),
            Self::Exception { name, at } => write!(f, "unhandled exception {name} in {at}"),
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::StackOverflow => {
                write!(f, "stack overflow: more than {} nested calls", vm::MAX_FRAMES)
            }
        }
    }
}

impl From<clr_meta::Error> for VmError {
    fn from(error: clr_meta::Error) -> Self {
        Self::Meta(error)
    }
}
