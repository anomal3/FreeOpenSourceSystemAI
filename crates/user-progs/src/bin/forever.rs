//! Программа, которая не заканчивается.
//!
//! Отличие от `/bin/spin` ровно одно, и оно принципиальное: у той цикл конечен,
//! и она доказывает, что вытеснение работает. Эта не кончится никогда — её
//! может убрать из системы только `kill`. Без него единственным способом
//! избавиться от неё было бы выключить питание.
//!
//! Системных вызовов после первой строки нет ни одного: снятие происходит не
//! потому, что программа о чём-то попросила ядро, а потому, что ядро дождалось
//! её возврата в третье кольцо по прерыванию таймера.

#![no_std]
#![no_main]

use core::arch::global_asm;

use user_progs::{pid, print, print_u64, println};

// Переход на самого себя. Написано ассемблером по той же причине, что и цикл в
// `spin`: `loop {}` на Rust — это обещание компилятора, а не инструкция, и
// проверять на нём поведение ядра значит проверять заодно и оптимизатор.
#[cfg(target_arch = "x86_64")]
global_asm!(
    r#"
.section .text.forever, "ax", @progbits
.balign 16
.globl forever_burn
.hidden forever_burn
forever_burn:
    jmp     forever_burn
"#
);

#[cfg(target_arch = "aarch64")]
global_asm!(
    r#"
.section .text.forever, "ax", %progbits
.balign 16
.globl forever_burn
.hidden forever_burn
forever_burn:
    b       forever_burn
"#
);

unsafe extern "C" {
    /// Не возвращается никогда.
    fn forever_burn() -> !;
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    print("forever ");
    print_u64(pid());
    println(": this program never ends on its own");

    // SAFETY: функция не трогает ни память, ни стек и не возвращается.
    unsafe { forever_burn() }
}
