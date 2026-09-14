//! Своя среда выполнения .NET: интерпретатор IL (веха v0.7c, фазы N2–N3).
//!
//! # Что умеет
//!
//! Фаза N2 — программа без объектов: статические методы, арифметика
//! `int32`/`int64`/`native int`/`float64` по таблицам ECMA-335, ветвления и
//! `switch`, строки из кучи `#US`, одномерные массивы.
//!
//! Фаза N3a — объекты: классы и поля, конструкторы, виртуальные вызовы и
//! интерфейсы (явная реализация, повторная реализация в наследнике, члены по
//! умолчанию), статические конструкторы, структуры с копированием по значению,
//! упаковка, `is`/`as`/приведения, инициализаторы массивов из данных сборки.
//!
//! Фаза N3b — исключения: `throw`, `catch` по типу, фильтры `when`, `finally`
//! и `fault`, `rethrow`, исключения самой среды, которые программа ловит
//! ([`eh`]).
//!
//! Фаза N3c — обобщения, специализированные значимыми типами, делегаты,
//! замыкания и события. Фаза N3d — точный сборщик мусора ([`gc`]).
//!
//! Фаза N4 — базовая библиотека: строки, коллекции, дробные числа. Печать и
//! разбор чисел повторяют .NET до символа ([`number`]), а `float32` на стеке
//! — отдельный вид значения, потому что .NET считает его в одинарной точности.
//!
//! Чего нет — Linq, `Enum.ToString`, `decimal` (N4d и дальше). Встреча с неподдержанным — не падение и не
//! молчание, а [`VmError`] с полным именем члена или кодом инструкции и местом.
//!
//! # Базовая библиотека
//!
//! Программа ссылается на `System.Runtime` и `System.Console` от Microsoft, а
//! работает поверх своей библиотеки на C# (`tools/dotnet/corelib`), в которую
//! ссылки разрешаются по имени типа и члена ([`loader`]). То, что нельзя
//! написать на C#, библиотека объявляет `InternalCall`, и это член в Rust
//! ([`natives`]).
//!
//! # Почему явный стек кадров, а не рекурсия
//!
//! Вызов метода C# не вызывает функцию Rust: кадр кладётся в вектор, и цикл
//! исполнения продолжает уже его. Даже статический конструктор, который нужен
//! посреди инструкции, запускается кадром, после которого инструкция
//! выполняется заново. У программы на FreeOS стек небольшой и фиксированный,
//! и рекурсивный интерпретатор падал бы на глубине, до которой настоящий
//! `dotnet` доходит спокойно. Здесь глубину ограничивает [`vm::MAX_FRAMES`].
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

mod dispatch;
mod eh;
mod gc;
mod heap;
mod loader;
mod natives;
pub mod number;
mod objects;
mod ops;
mod places;
mod types;
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
    /// Тип, которого нет ни в программе, ни в базовой библиотеке.
    MissingType { name: String },
    /// IL, которого не пропустил бы верификатор: стек пуст, типы не сходятся.
    Invalid { what: &'static str, at: String },
    /// Исключение, которое бросает сама среда (`System.NullReferenceException`
    /// и подобные). Цикл исполнения превращает его в объект и бросает в
    /// программу; наружу оно выходит, только если бросить некуда.
    Exception { name: &'static str, at: String },
    /// Исключение, которое программа не поймала.
    Unhandled { name: String, message: String, at: String },
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
            Self::MissingType { name } => write!(f, "missing type {name} (not in the base library yet)"),
            Self::Invalid { what, at } => write!(f, "invalid IL: {what} in {at}"),
            Self::Exception { name, at } => write!(f, "unhandled exception {name} in {at}"),
            Self::Unhandled { name, message, at } => write!(f, "unhandled exception {name}: {message} in {at}"),
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
