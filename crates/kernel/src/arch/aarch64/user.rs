//! Переход в EL0 и обратно.
//!
//! Устройство то же, что на x86-64 (см. `arch::x86_64::user`): [`enter_user`]
//! выглядит для ядра обычным вызовом, который возвращается кодом завершения
//! программы, а [`return_to_kernel`] возвращается из него, будучи вызванной из
//! обработчика исключения.
//!
//! # Что здесь своё
//!
//! Стек. Ядро работает на `SP_EL1`, программа — на `SP_EL0`, и переключать
//! ничего не требуется: кадр [`enter_user`] остаётся на стеке своей задачи, а
//! ловушка из EL0 приходит туда же, прямо под него. Именно поэтому у каждой
//! задачи с программой её собственный стек ловушек — им служит её же стек ядра
//! (см. [`super::interrupts`]).
//!
//! `SPSR_EL1 = 0` означает «вернуться в EL0t с открытыми D/A/I/F»: программа
//! исполняется с разрешёнными прерываниями, иначе вечный цикл в ней остановил
//! бы машину.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

/// Вершина стека ядра (`SP_EL1`) на момент входа в EL0.
///
/// Принадлежит той задаче, которая сейчас исполняет программу; планировщик
/// сохраняет и восстанавливает значение при переключении — см.
/// [`swap_user_return_stack`].
static KERNEL_SP: AtomicU64 = AtomicU64::new(0);

/// Отдать планировщику стек возврата уходящей задачи и поставить стек той,
/// которая получает процессор. См. одноимённую функцию x86-64.
pub fn swap_user_return_stack(next: usize) -> usize {
    KERNEL_SP.swap(next as u64, Ordering::Relaxed) as usize
}

global_asm!(
    r#"
.section .text.user_entry, "ax", %progbits
.balign 16
.globl aarch64_enter_user
.hidden aarch64_enter_user
aarch64_enter_user:
    // x0 — точка входа, x1 — вершина пользовательского стека, x2 — argc,
    // x3 — адрес массива argv в памяти программы.
    sub     sp, sp, #96
    stp     x19, x20, [sp, #0]
    stp     x21, x22, [sp, #16]
    stp     x23, x24, [sp, #32]
    stp     x25, x26, [sp, #48]
    stp     x27, x28, [sp, #64]
    stp     x29, x30, [sp, #80]

    adrp    x9, {kernel_sp}
    add     x9, x9, :lo12:{kernel_sp}
    mov     x10, sp
    str     x10, [x9]

    // Стек ядра этой задачи — SP_EL1, и мы на нём же и остаёмся: SP_EL0
    // отдаётся программе целиком.
    msr     sp_el0, x1
    msr     elr_el1, x0
    // EL0t, все маски сняты.
    msr     spsr_el1, xzr

    // Стереть всё, что могло остаться от ядра: в регистрах лежат адреса его
    // структур, и отдать их программе значило бы отдать раскладку памяти.
    // Аргументы программы переставляются в x0/x1 — первые два по AAPCS64 — и
    // только потом стираются их исходные регистры. Обнулить x2/x3 вместе со
    // всеми значило бы передать программе два нуля вместо аргументов.
    mov     x0, x2
    mov     x1, x3
    mov     x2, xzr
    mov     x3, xzr
    mov     x4, xzr
    mov     x5, xzr
    mov     x6, xzr
    mov     x7, xzr
    mov     x8, xzr
    mov     x9, xzr
    mov     x10, xzr
    mov     x11, xzr
    mov     x12, xzr
    mov     x13, xzr
    mov     x14, xzr
    mov     x15, xzr
    mov     x16, xzr
    mov     x17, xzr
    mov     x18, xzr
    mov     x19, xzr
    mov     x20, xzr
    mov     x21, xzr
    mov     x22, xzr
    mov     x23, xzr
    mov     x24, xzr
    mov     x25, xzr
    mov     x26, xzr
    mov     x27, xzr
    mov     x28, xzr
    mov     x29, xzr
    mov     x30, xzr
    eret
    // Барьер прямолинейной спекуляции — см. таблицу векторов.
    dsb     nsh
    isb

.balign 16
.globl aarch64_return_to_kernel
.hidden aarch64_return_to_kernel
aarch64_return_to_kernel:
    // x0 — код возврата. Исполняется в обработчике, то есть на том же SP_EL1,
    // на котором лежит и кадр enter_user: возврат — это просто перестановка
    // указателя на него, кадр обработчика бросается целиком.
    mov     x11, x0
    adrp    x9, {kernel_sp}
    add     x9, x9, :lo12:{kernel_sp}
    ldr     x10, [x9]
    mov     sp, x10
    // Вход в исключение замаскировал прерывания. Вернуться в ядро с
    // закрытым IRQ значило бы остановить таймер навсегда.
    msr     daifclr, #2
    mov     x0, x11

    ldp     x19, x20, [sp, #0]
    ldp     x21, x22, [sp, #16]
    ldp     x23, x24, [sp, #32]
    ldp     x25, x26, [sp, #48]
    ldp     x27, x28, [sp, #64]
    ldp     x29, x30, [sp, #80]
    add     sp, sp, #96
    ret
"#,
    kernel_sp = sym KERNEL_SP,
);

unsafe extern "C" {
    fn aarch64_enter_user(entry: usize, stack: usize, argc: usize, argv: usize) -> i64;
    fn aarch64_return_to_kernel(code: i64) -> !;
}

/// Уйти в EL0 и вернуться с кодом завершения программы.
///
/// # Safety
///
/// `entry` и `stack` должны указывать в память, отображённую доступной из EL0.
pub unsafe fn enter_user(entry: usize, stack: usize, argc: usize, argv: usize) -> i64 {
    // SAFETY: контракт функции.
    unsafe { aarch64_enter_user(entry, stack, argc, argv) }
}

/// Вернуться в ядро из обработчика, бросив контекст программы.
///
/// # Safety
///
/// Вызывать можно только во время исполнения [`enter_user`].
pub unsafe fn return_to_kernel(code: i64) -> ! {
    // SAFETY: контракт функции.
    unsafe { aarch64_return_to_kernel(code) }
}
