// Точка входа позиционно-независимой программы (ASLR, часть 2), AArch64.
//
// Один файл на программы обоих языков: Rust включает его в `user_progs::start`
// (`global_asm!`), для C он собирается в `crt0.o` рядом с `crt0.c`. Две копии
// одного ассемблера разъехались бы молча — и сломалась бы одна половина
// программ, а не все.
//
// Что делает: прибавляет настоящее начало образа ко всем местам, перечисленным
// в `.rela.dyn` (`R_AARCH64_RELATIVE`), и только потом передаёт управление
// `_start` программы. До конца цикла GOT не верен, поэтому ни одного вызова
// отсюда нет — почему это так важно, сказано в `crates/user-progs/src/start.rs`.
//
// Числа записаны прямо и сверяются с `user_abi` при сборке `user_progs`:
//   DT_RELA = 7, DT_RELASZ = 8, DT_RELAENT = 9, DT_RELR = 36,
//   R_AARCH64_RELATIVE = 1027, SYS_WRITE = 1, SYS_EXIT = 2, FD_STDOUT = 1.
//
// Метки только именованные: числовая метка в ассемблере однажды уже стоила
// этому проекту тройной ошибки на загрузке.

    .section .rodata.freeos_start, "a"
freeos_start_message:
    .ascii "start: this program cannot be relocated (a relocation other than RELATIVE, or DT_RELR)\n"
freeos_start_message_end:

    .text
    .globl __freeos_entry
    .type __freeos_entry, %function
__freeos_entry:
    // argc и argv ядро положило в x0 и x1; `_start` обязан получить их
    // нетронутыми. Два слова на стеке — выравнивание не меняется.
    stp x0, x1, [sp, #-16]!
    // x8 — начало образа, x9 — бегунок по `_DYNAMIC`.
    adrp x8, __image_base
    add x8, x8, :lo12:__image_base
    adrp x9, _DYNAMIC
    add x9, x9, :lo12:_DYNAMIC
    // x10 — DT_RELA, x11 — DT_RELASZ, x12 — DT_RELAENT.
    mov x10, #0
    mov x11, #0
    mov x12, #24
freeos_dynamic_next:
    ldp x13, x14, [x9], #16
    cbz x13, freeos_dynamic_done
    cmp x13, #7
    b.ne freeos_dynamic_not_rela
    mov x10, x14
    b freeos_dynamic_next
freeos_dynamic_not_rela:
    cmp x13, #8
    b.ne freeos_dynamic_not_size
    mov x11, x14
    b freeos_dynamic_next
freeos_dynamic_not_size:
    cmp x13, #9
    b.ne freeos_dynamic_not_step
    mov x12, x14
    b freeos_dynamic_next
freeos_dynamic_not_step:
    cmp x13, #36
    b.eq freeos_relocate_bad
    b freeos_dynamic_next
freeos_dynamic_done:
    cbz x10, freeos_relocate_done
    cbz x12, freeos_relocate_bad
    // x10 — текущая запись, x11 — конец таблицы, обе по настоящему адресу.
    add x10, x10, x8
    add x11, x11, x10
    mov x15, #1027
freeos_relocate_next:
    cmp x10, x11
    b.hs freeos_relocate_done
    // Вид перемещения — младшие 32 бита r_info.
    ldr w13, [x10, #8]
    cmp x13, x15
    b.ne freeos_relocate_bad
    ldr x13, [x10]
    ldr x14, [x10, #16]
    add x14, x14, x8
    str x14, [x8, x13]
    add x10, x10, x12
    b freeos_relocate_next
freeos_relocate_done:
    ldp x0, x1, [sp], #16
    b _start
freeos_relocate_bad:
    mov x8, #1
    mov x0, #1
    adrp x1, freeos_start_message
    add x1, x1, :lo12:freeos_start_message
    adrp x2, freeos_start_message_end
    add x2, x2, :lo12:freeos_start_message_end
    sub x2, x2, x1
    svc #0
    mov x8, #2
    mov x0, #127
    svc #0
    brk #0
