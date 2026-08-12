//! Первая программа, исполняющаяся вне ядра.
//!
//! Делает ровно столько, сколько нужно, чтобы доказать, что граница привилегий
//! работает в обе стороны: печатает (данные наружу), спрашивает время работы
//! системы (данные внутрь), уступает процессор и завершается с кодом.

#![no_std]
#![no_main]

use user_progs::{exit, print, print_u64, println, yield_now};

/// Точка входа. Имя `_start` — то, что записано в `ENTRY()` компоновочного
/// сценария и что ядро возьмёт из заголовка ELF.
///
/// `extern "C"` и `no_mangle` — потому что вызывающая сторона (ядро) знает про
/// эту функцию только адрес: ни аргументов, ни возврата у неё нет.
#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    println("hello from userspace");
    println("this code runs outside the kernel: no kernel page is reachable from here");

    print("uptime as the kernel sees it: ");
    print_u64(uptime());
    println(" ms");

    // Уступка процессора — тоже системный вызов: планировщик кооперативный, и
    // без неё программа держала бы машину до самого конца.
    for _ in 0..3 {
        yield_now();
    }

    // Время суток — не то же, что время работы: одно говорит, который час,
    // другое — сколько машина включена. Ноль означал бы, что часов не было ни у
    // прошивки, ни у платы, и программа обязана различать этот случай, а не
    // печатать 1970 год с уверенным видом.
    let now = user_progs::time_now();
    if now == 0 {
        println("the system does not know the time of day");
    } else {
        print("hello: epoch ");
        print_u64(now);
        println(" s");
    }

    println("done, exiting with code 0");
    exit(0)
}

fn uptime() -> u64 {
    user_progs::uptime_ms()
}
