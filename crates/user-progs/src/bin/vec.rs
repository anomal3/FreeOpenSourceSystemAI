//! Программа, доказывающая, что векторные регистры принадлежат задаче.
//!
//! # Что именно она проверяет
//!
//! Восемь векторных регистров заполняются константой, которую программа
//! получила аргументом, после чего она **уступает процессор** и сверяет их
//! содержимое с той же константой. Два экземпляра с разными константами,
//! исполняющиеся по очереди, обязаны видеть каждый своё.
//!
//! До этой фазы такая проверка падает, и падает сразу: переключение задач
//! сохраняло целочисленный контекст и не сохраняло ничего больше. Это и есть
//! единственная форма проверки, которая доказывает, что фаза что-то изменила, —
//! в отличие от «ничего не сломалось», которое выглядит одинаково до и после.
//!
//! # Почему всё в одном ассемблерном блоке
//!
//! Потому что между записью в регистр и его чтением не должно быть ни одной
//! инструкции, о которой мы не знаем. Разбей это на два блока — и компилятор
//! вправе использовать те же регистры для своих нужд между ними: тест начнёт
//! падать по причине, к переключению задач отношения не имеющей. Системный
//! вызов стоит внутри блока по той же причине.

#![no_std]
#![no_main]

use core::arch::asm;

use user_progs::{Args, exit, print, print_u64, println};

/// Сколько раз повторяется цикл «записать — уступить — сверить».
///
/// Число подобрано так, чтобы за него планировщик заведомо переключил задачу
/// много раз: квант вытеснения — десятки миллисекунд, а уступка возвращает
/// управление сразу, поэтому важно не время, а количество.
const ROUNDS: u64 = 2000;

/// Номер `SYS_YIELD` в договоре. Продублирован здесь числом намеренно:
/// ассемблерный блок ниже не может позвать функцию обвязки, а `const` из
/// `user_abi` подставляется в `asm!` как обычная константа.
const YIELD: u64 = 3;

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };

    // Константа приходит аргументом, а не зашита: два экземпляра одной и той же
    // программы обязаны писать в регистры **разное**, иначе проверка пройдёт и
    // на системе, которая регистры не сохраняет вовсе.
    let tag = match args.get(1).and_then(parse_u64) {
        Some(value) if value != 0 => value,
        _ => {
            println("usage: vec <nonzero-number>");
            exit(2);
        }
    };

    print("vec ");
    print_u64(tag);
    println(": filling eight vector registers and yielding");

    let mut failures = 0u64;
    for _ in 0..ROUNDS {
        // SAFETY: блок пишет и читает только векторные регистры, объявленные
        // испорченными, и делает один системный вызов без аргументов-указателей.
        if unsafe { round(tag) } != 0 {
            failures += 1;
        }
    }

    if failures == 0 {
        print("vec ");
        print_u64(tag);
        print(": ");
        print_u64(ROUNDS);
        println(" checks passed");
        exit(0)
    } else {
        print("vec ");
        print_u64(tag);
        print(": MISMATCH in ");
        print_u64(failures);
        print(" of ");
        print_u64(ROUNDS);
        println(" checks");
        exit(1)
    }
}

/// Один виток: записать, уступить, сверить. Ноль — всё сошлось.
///
/// # Safety
///
/// Вызывать можно только из программы: внутри системный вызов.
#[cfg(target_arch = "x86_64")]
unsafe fn round(tag: u64) -> u64 {
    let mismatch: u64;
    // SAFETY: контракт функции. Все восемь регистров перечислены в списке
    // испорченных, поэтому компилятор не считает их содержимое своим.
    unsafe {
        asm!(
            "movq xmm0, {tag}",
            "movq xmm1, {tag}",
            "movq xmm2, {tag}",
            "movq xmm3, {tag}",
            "movq xmm4, {tag}",
            "movq xmm5, {tag}",
            "movq xmm6, {tag}",
            "movq xmm7, {tag}",
            // Уступка. Номер — в rax, результат — тоже: ядро восстанавливает
            // остальные регистры из кадра ловушки, и `tag` переживает вызов.
            "mov rax, {yield_number}",
            "int 0x80",
            // Сверка: разница с ожидаемым копится побитово, чтобы не ветвиться.
            "movq rax, xmm0",
            "xor rax, {tag}",
            "mov {acc}, rax",
            "movq rax, xmm1",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm2",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm3",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm4",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm5",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm6",
            "xor rax, {tag}",
            "or {acc}, rax",
            "movq rax, xmm7",
            "xor rax, {tag}",
            "or {acc}, rax",
            tag = in(reg) tag,
            acc = out(reg) mismatch,
            yield_number = const YIELD,
            out("rax") _,
            out("xmm0") _,
            out("xmm1") _,
            out("xmm2") _,
            out("xmm3") _,
            out("xmm4") _,
            out("xmm5") _,
            out("xmm6") _,
            out("xmm7") _,
            options(nostack),
        );
    }
    mismatch
}

/// То же самое на AArch64.
///
/// `dup v.2d, x` кладёт одно и то же 64-битное значение в обе половины
/// регистра, `umov x, v.d[0]` достаёт младшую обратно. Проверять обе половины
/// незачем: сохраняются они одной инструкцией.
#[cfg(target_arch = "aarch64")]
unsafe fn round(tag: u64) -> u64 {
    let mismatch: u64;
    // SAFETY: контракт функции; все восемь регистров объявлены испорченными.
    unsafe {
        asm!(
            "dup v0.2d, {tag}",
            "dup v1.2d, {tag}",
            "dup v2.2d, {tag}",
            "dup v3.2d, {tag}",
            "dup v4.2d, {tag}",
            "dup v5.2d, {tag}",
            "dup v6.2d, {tag}",
            "dup v7.2d, {tag}",
            "mov x8, {yield_number}",
            "svc #0",
            "umov {scratch}, v0.d[0]",
            "eor {acc}, {scratch}, {tag}",
            "umov {scratch}, v1.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v2.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v3.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v4.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v5.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v6.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            "umov {scratch}, v7.d[0]",
            "eor {scratch}, {scratch}, {tag}",
            "orr {acc}, {acc}, {scratch}",
            tag = in(reg) tag,
            acc = out(reg) mismatch,
            scratch = out(reg) _,
            yield_number = const YIELD,
            out("x0") _,
            out("x8") _,
            out("v0") _,
            out("v1") _,
            out("v2") _,
            out("v3") _,
            out("v4") _,
            out("v5") _,
            out("v6") _,
            out("v7") _,
            options(nostack),
        );
    }
    mismatch
}

/// Разобрать десятичное число. `None` — не число или пусто.
fn parse_u64(text: &str) -> Option<u64> {
    let mut value = 0u64;
    let mut digits = 0;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
        digits += 1;
    }
    if digits == 0 { None } else { Some(value) }
}
