# Точка входа позиционно-независимой программы (ASLR, часть 2), x86-64.
#
# Один файл на программы обоих языков: Rust включает его в `user_progs::start`
# (`global_asm!`), для C он собирается в `crt0.o` рядом с `crt0.c`. Две копии
# одного ассемблера разъехались бы молча — и сломалась бы одна половина
# программ, а не все.
#
# Что делает: прибавляет настоящее начало образа ко всем местам, перечисленным
# в `.rela.dyn` (`R_X86_64_RELATIVE`), и только потом передаёт управление
# `_start` программы. До конца цикла GOT не верен, поэтому ни одного вызова
# отсюда нет — почему это так важно, сказано в `crates/user-progs/src/start.rs`.
#
# Числа записаны прямо и сверяются с `user_abi` при сборке `user_progs`:
#   DT_RELA = 7, DT_RELASZ = 8, DT_RELAENT = 9, DT_RELR = 36,
#   R_X86_64_RELATIVE = 8, SYS_WRITE = 1, SYS_EXIT = 2, FD_STDOUT = 1.
#
# Метки только именованные: числовая метка в ассемблере однажды уже стоила
# этому проекту тройной ошибки на загрузке.

    .section .rodata.freeos_start, "a"
freeos_start_message:
    .ascii "start: this program cannot be relocated (a relocation other than RELATIVE, or DT_RELR)\n"
freeos_start_message_end:

    .text
    .globl __freeos_entry
    .type __freeos_entry, @function
__freeos_entry:
    # argc и argv ядро положило в rdi и rsi; `_start` обязан получить их
    # нетронутыми. Два слова на стеке — выравнивание не меняется.
    pushq %rdi
    pushq %rsi
    # r8 — начало образа, r9 — бегунок по `_DYNAMIC`.
    leaq __image_base(%rip), %r8
    leaq _DYNAMIC(%rip), %r9
    # r10 — DT_RELA, r11 — DT_RELASZ, rcx — DT_RELAENT.
    xorl %r10d, %r10d
    xorl %r11d, %r11d
    movl $24, %ecx
freeos_dynamic_next:
    movq (%r9), %rax
    movq 8(%r9), %rdx
    addq $16, %r9
    testq %rax, %rax
    jz freeos_dynamic_done
    cmpq $7, %rax
    jne freeos_dynamic_not_rela
    movq %rdx, %r10
    jmp freeos_dynamic_next
freeos_dynamic_not_rela:
    cmpq $8, %rax
    jne freeos_dynamic_not_size
    movq %rdx, %r11
    jmp freeos_dynamic_next
freeos_dynamic_not_size:
    cmpq $9, %rax
    jne freeos_dynamic_not_step
    movq %rdx, %rcx
    jmp freeos_dynamic_next
freeos_dynamic_not_step:
    cmpq $36, %rax
    je freeos_relocate_bad
    jmp freeos_dynamic_next
freeos_dynamic_done:
    testq %r10, %r10
    jz freeos_relocate_done
    testq %rcx, %rcx
    jz freeos_relocate_bad
    # r10 — текущая запись, r11 — конец таблицы, обе по настоящему адресу.
    addq %r8, %r10
    addq %r10, %r11
freeos_relocate_next:
    cmpq %r11, %r10
    jae freeos_relocate_done
    # Вид перемещения — младшие 32 бита r_info.
    movl 8(%r10), %eax
    cmpq $8, %rax
    jne freeos_relocate_bad
    movq (%r10), %rax
    movq 16(%r10), %rdx
    addq %r8, %rdx
    movq %rdx, (%r8,%rax)
    addq %rcx, %r10
    jmp freeos_relocate_next
freeos_relocate_done:
    popq %rsi
    popq %rdi
    jmp _start
freeos_relocate_bad:
    movl $1, %eax
    movl $1, %edi
    leaq freeos_start_message(%rip), %rsi
    leaq freeos_start_message_end(%rip), %rdx
    subq %rsi, %rdx
    int $0x80
    movl $2, %eax
    movl $127, %edi
    int $0x80
    ud2
