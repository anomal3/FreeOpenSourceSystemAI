// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Стартовый код программы, собранной позиционно-независимой (ASLR, часть 2).
//!
//! # Зачем
//!
//! Программа компонуется от адреса 0, а ядро кладёт её по случайному адресу в
//! окне образа. Код от этого не страдает — он обращается к своим данным
//! относительно счётчика команд. Страдают **данные, в которых лежат адреса**:
//! таблицы виртуальных функций, указатели на строки и GOT — через него идут
//! вызовы функций соседних крейтов. Компоновщик записал в них адреса «от нуля»
//! и оставил в `.rela.dyn` список мест, к которым надо прибавить настоящее
//! начало образа.
//!
//! # Почему это делает программа, а не ядро
//!
//! С фазы 54 страницы образа читаются с носителя по обращению: при запуске ядро
//! не держит в руках ни одной из них, и поправить их ему нечем. Программа же
//! начинает с того, что все эти страницы трогает, — и поправляет сама. Так же
//! устроен `rcrt1.o` у musl для статических PIE.
//!
//! # Почему на ассемблере
//!
//! До конца цикла ни одна запись GOT не верна — а значит, **нельзя позвать ни
//! одну функцию другого крейта**. Первая версия была на Rust из одних сырых
//! указателей и всё равно прыгнула на адрес 0: в отладочной сборке `ptr.read()`
//! зовёт проверку предусловий из `core`, то есть через GOT, ещё не
//! перемещённый. Такие вызовы вставляет компилятор, а не автор, и запретить их
//! в Rust нечем. Двадцать инструкций на ассемблере не вставят ничего.
//!
//! Разбирается только то, что нужно: `DT_RELA`, `DT_RELASZ`, `DT_RELAENT` и
//! перемещения вида `RELATIVE` — других в статической PIE не бывает. Всё прочее
//! (включая сжатые `DT_RELR`, которых мы у компоновщика не просим) — отказ с
//! внятной строкой и кодом 127, а не падение на первом же указателе.

use user_abi::{FD_STDOUT, SYS_EXIT, SYS_WRITE};

/// Метки динамической таблицы.
const DT_RELA: usize = 7;
const DT_RELASZ: usize = 8;
const DT_RELAENT: usize = 9;
const DT_RELR: usize = 36;

/// «Прибавить начало образа» — единственный вид перемещения в статической PIE.
#[cfg(target_arch = "x86_64")]
const R_RELATIVE: usize = 8;
#[cfg(target_arch = "aarch64")]
const R_RELATIVE: usize = 1027;

/// Код выхода программы, которую не удалось переместить.
const EXIT_BAD_IMAGE: usize = 127;

// Метки только именованные: числовая метка в ассемблере однажды уже стоила
// этому проекту тройной ошибки на загрузке.
//
// Аргументы ядра (`argc`, `argv`) приходят в регистрах и должны дойти до
// `_start` нетронутыми — поэтому сохраняются на стеке. Два слова: выравнивание
// стека не меняется.
#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    ".section .rodata.freeos_start, \"a\"",
    "freeos_start_message:",
    ".ascii \"start: this program cannot be relocated (a relocation other than RELATIVE, or DT_RELR)\\n\"",
    "freeos_start_message_end:",
    ".text",
    ".global __freeos_entry",
    ".type __freeos_entry, @function",
    "__freeos_entry:",
    "push rdi",
    "push rsi",
    // r8 — начало образа, r9 — бегунок по `_DYNAMIC`.
    "lea r8, [rip + __image_base]",
    "lea r9, [rip + _DYNAMIC]",
    // r10 — `DT_RELA`, r11 — `DT_RELASZ`, rcx — `DT_RELAENT`.
    "xor r10d, r10d",
    "xor r11d, r11d",
    "mov ecx, 24",
    "freeos_dynamic_next:",
    "mov rax, [r9]",
    "mov rdx, [r9 + 8]",
    "add r9, 16",
    "test rax, rax",
    "jz freeos_dynamic_done",
    "cmp rax, {dt_rela}",
    "jne freeos_dynamic_not_rela",
    "mov r10, rdx",
    "jmp freeos_dynamic_next",
    "freeos_dynamic_not_rela:",
    "cmp rax, {dt_relasz}",
    "jne freeos_dynamic_not_size",
    "mov r11, rdx",
    "jmp freeos_dynamic_next",
    "freeos_dynamic_not_size:",
    "cmp rax, {dt_relaent}",
    "jne freeos_dynamic_not_step",
    "mov rcx, rdx",
    "jmp freeos_dynamic_next",
    "freeos_dynamic_not_step:",
    "cmp rax, {dt_relr}",
    "je freeos_relocate_bad",
    "jmp freeos_dynamic_next",
    "freeos_dynamic_done:",
    "test r10, r10",
    "jz freeos_relocate_done",
    "test rcx, rcx",
    "jz freeos_relocate_bad",
    // r10 — текущая запись, r11 — конец таблицы, обе уже по настоящему адресу.
    "add r10, r8",
    "add r11, r10",
    "freeos_relocate_next:",
    "cmp r10, r11",
    "jae freeos_relocate_done",
    // Вид перемещения — младшие 32 бита `r_info`.
    "mov eax, dword ptr [r10 + 8]",
    "cmp rax, {r_relative}",
    "jne freeos_relocate_bad",
    "mov rax, [r10]",
    "mov rdx, [r10 + 16]",
    "add rdx, r8",
    "mov [r8 + rax], rdx",
    "add r10, rcx",
    "jmp freeos_relocate_next",
    "freeos_relocate_done:",
    "pop rsi",
    "pop rdi",
    "jmp _start",
    "freeos_relocate_bad:",
    "mov eax, {sys_write}",
    "mov edi, {fd}",
    "lea rsi, [rip + freeos_start_message]",
    "lea rdx, [rip + freeos_start_message_end]",
    "sub rdx, rsi",
    "int 0x80",
    "mov eax, {sys_exit}",
    "mov edi, {exit_code}",
    "int 0x80",
    "ud2",
    dt_rela = const DT_RELA,
    dt_relasz = const DT_RELASZ,
    dt_relaent = const DT_RELAENT,
    dt_relr = const DT_RELR,
    r_relative = const R_RELATIVE,
    sys_write = const SYS_WRITE,
    sys_exit = const SYS_EXIT,
    fd = const FD_STDOUT,
    exit_code = const EXIT_BAD_IMAGE,
);

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".section .rodata.freeos_start, \"a\"",
    "freeos_start_message:",
    ".ascii \"start: this program cannot be relocated (a relocation other than RELATIVE, or DT_RELR)\\n\"",
    "freeos_start_message_end:",
    ".text",
    ".global __freeos_entry",
    ".type __freeos_entry, %function",
    "__freeos_entry:",
    "stp x0, x1, [sp, #-16]!",
    // x8 — начало образа, x9 — бегунок по `_DYNAMIC`.
    "adrp x8, __image_base",
    "add x8, x8, :lo12:__image_base",
    "adrp x9, _DYNAMIC",
    "add x9, x9, :lo12:_DYNAMIC",
    // x10 — `DT_RELA`, x11 — `DT_RELASZ`, x12 — `DT_RELAENT`.
    "mov x10, #0",
    "mov x11, #0",
    "mov x12, #24",
    "freeos_dynamic_next:",
    "ldp x13, x14, [x9], #16",
    "cbz x13, freeos_dynamic_done",
    "cmp x13, #{dt_rela}",
    "b.ne freeos_dynamic_not_rela",
    "mov x10, x14",
    "b freeos_dynamic_next",
    "freeos_dynamic_not_rela:",
    "cmp x13, #{dt_relasz}",
    "b.ne freeos_dynamic_not_size",
    "mov x11, x14",
    "b freeos_dynamic_next",
    "freeos_dynamic_not_size:",
    "cmp x13, #{dt_relaent}",
    "b.ne freeos_dynamic_not_step",
    "mov x12, x14",
    "b freeos_dynamic_next",
    "freeos_dynamic_not_step:",
    "cmp x13, #{dt_relr}",
    "b.eq freeos_relocate_bad",
    "b freeos_dynamic_next",
    "freeos_dynamic_done:",
    "cbz x10, freeos_relocate_done",
    "cbz x12, freeos_relocate_bad",
    "add x10, x10, x8",
    "add x11, x11, x10",
    "mov x15, #{r_relative}",
    "freeos_relocate_next:",
    "cmp x10, x11",
    "b.hs freeos_relocate_done",
    // Вид перемещения — младшие 32 бита `r_info`.
    "ldr w13, [x10, #8]",
    "cmp x13, x15",
    "b.ne freeos_relocate_bad",
    "ldr x13, [x10]",
    "ldr x14, [x10, #16]",
    "add x14, x14, x8",
    "str x14, [x8, x13]",
    "add x10, x10, x12",
    "b freeos_relocate_next",
    "freeos_relocate_done:",
    "ldp x0, x1, [sp], #16",
    "b _start",
    "freeos_relocate_bad:",
    "mov x8, #{sys_write}",
    "mov x0, #{fd}",
    "adrp x1, freeos_start_message",
    "add x1, x1, :lo12:freeos_start_message",
    "adrp x2, freeos_start_message_end",
    "add x2, x2, :lo12:freeos_start_message_end",
    "sub x2, x2, x1",
    "svc #0",
    "mov x8, #{sys_exit}",
    "mov x0, #{exit_code}",
    "svc #0",
    "brk #0",
    dt_rela = const DT_RELA,
    dt_relasz = const DT_RELASZ,
    dt_relaent = const DT_RELAENT,
    dt_relr = const DT_RELR,
    r_relative = const R_RELATIVE,
    sys_write = const SYS_WRITE,
    sys_exit = const SYS_EXIT,
    fd = const FD_STDOUT,
    exit_code = const EXIT_BAD_IMAGE,
);
