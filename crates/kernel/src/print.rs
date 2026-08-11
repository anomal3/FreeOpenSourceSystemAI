//! Макросы вывода ядра: одна строка уходит сразу в оба приёмника.
//!
//! Serial есть всегда, экран — только если загрузчик нашёл GOP. Поэтому
//! [`kprintln!`](crate::kprintln) пишет в оба, и каждый приёмник сам решает,
//! готов ли он: до `serial::init` и до `console::init` вызовы просто ничего
//! не делают, что позволяет пользоваться макросами с самой первой инструкции.

use core::fmt;

/// Точка входа макросов вывода. Не вызывать напрямую.
#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    crate::serial::_print(args);
    crate::console::_print(args);
}

/// Напечатать в serial и (если есть) на экран, без перевода строки.
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        $crate::print::_print(::core::format_args!($($arg)*))
    };
}

/// Напечатать в serial и (если есть) на экран, с переводом строки.
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::kprint!("{}\n", ::core::format_args!($($arg)*))
    };
}
