//! Переход в EL0 и обратно.
//!
//! Устройство то же, что на x86-64 (см. `arch::x86_64::user`): [`enter_user`]
//! выглядит для ядра обычным вызовом, который возвращается кодом завершения
//! программы, а [`return_to_kernel`] возвращается из него, будучи вызванной из
//! обработчика исключения.
//!
//! # Что здесь своё
//!
//! Стек. Ядро работает на `SP_EL0` (так его настроил [`super::interrupts`]),
//! а обработчики — на `SP_EL1`. Уходя в EL0, мы обязаны отдать `SP_EL0`
//! программе — то есть выбить стек из-под самих себя. Поэтому перед этим
//! исполнение переводится на `SP_EL1` (`SPSel = 1`), а вершина стека ядра
//! запоминается. Обратный путь ровно такой же наоборот.
//!
//! `SPSR_EL1 = 0` означает «вернуться в EL0t с открытыми D/A/I/F»: программа
//! исполняется с разрешёнными прерываниями, иначе вечный цикл в ней остановил
//! бы машину.

use core::arch::global_asm;
use core::sync::atomic::AtomicU64;

/// Вершина стека ядра (`SP_EL0`) на момент входа в EL0.
static KERNEL_SP: AtomicU64 = AtomicU64::new(0);

global_asm!(
    r#"
.section .text.user_entry, "ax", %progbits
.balign 16
.globl aarch64_enter_user
.hidden aarch64_enter_user
aarch64_enter_user:
    // x0 — точка входа, x1 — вершина пользовательского стека.
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

    // Переходим на стек обработчика: следующая инструкция отдаёт SP_EL0
    // программе, и делать это, стоя на нём, нельзя.
    msr     spsel, #1
    isb
    msr     sp_el0, x1
    msr     elr_el1, x0
    // EL0t, все маски сняты.
    msr     spsr_el1, xzr

    // Стереть всё, что могло остаться от ядра: в регистрах лежат адреса его
    // структур, и отдать их программе значило бы отдать раскладку памяти.
    mov     x0, xzr
    mov     x1, xzr
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
    // x0 — код возврата. Исполняется в обработчике, то есть на SP_EL1.
    mov     x11, x0
    adrp    x9, {kernel_sp}
    add     x9, x9, :lo12:{kernel_sp}
    ldr     x10, [x9]
    msr     sp_el0, x10
    msr     spsel, #0
    isb
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
    fn aarch64_enter_user(entry: usize, stack: usize) -> i64;
    fn aarch64_return_to_kernel(code: i64) -> !;
}

/// Уйти в EL0 и вернуться с кодом завершения программы.
///
/// # Safety
///
/// `entry` и `stack` должны указывать в память, отображённую доступной из EL0.
pub unsafe fn enter_user(entry: usize, stack: usize) -> i64 {
    // SAFETY: контракт функции.
    unsafe { aarch64_enter_user(entry, stack) }
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
