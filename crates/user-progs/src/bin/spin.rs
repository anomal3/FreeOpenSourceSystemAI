//! Программа, которая не уступает процессор.
//!
//! Между двумя своими строками она не делает **ни одного** системного вызова:
//! ни `yield`, ни `write`, ни даже `uptime`. До Phase 13b это означало, что
//! машина принадлежит ей целиком — оболочка не отвечала, окна не
//! перерисовывались, и вернуть систему можно было только перезагрузкой.
//!
//! Отсюда и способ проверки: если между «no system calls from here on» и «done»
//! в журнале успела появиться чужая строка, значит задачу сняли с процессора
//! против её воли. Другого объяснения у этой строки нет.
//!
//! # Второй режим: `spin <мс>`
//!
//! С числом программа не молчит, а **жжёт**: крутит тот же цикл кусками, пока не
//! пройдёт заказанное время, и между кусками спрашивает у ядра, на каком
//! процессоре исполняется. В конце печатает одну строку — сколько жгла и на
//! каких процессорах побывала.
//!
//! Режим появился в фазе 43 и отвечает на её вопрос: две такие программы,
//! запущенные рядом на машине с несколькими процессорами, обязаны исполняться
//! **одновременно**, а не по очереди. Молчащий режим для этого не годится —
//! спросить номер процессора он не может, не нарушив собственного обещания.
//!
//! # Почему цикл написан ассемблером
//!
//! Потому что иначе он означает разное в разных сборках, а сценарий на стенде
//! гоняется в обеих. Первая версия считала на Rust через
//! [`core::hint::black_box`] — и оптимизированная сборка проходила те же сто
//! двадцать миллионов витков за 140 мс, тогда как отладочной требовалось 3,3 с.
//! Двенадцать инструкций ассемблера этот вопрос закрывают: компилятор к ним не
//! прикасается. Метки — именованные и внутри [`global_asm!`]: числовая метка в
//! блоке `asm!` однажды уже стоила этому проекту тройной ошибки на загрузке.

#![no_std]
#![no_main]

use core::arch::global_asm;

use user_progs::{Args, cpu, exit, pid, print, print_u64, println, uptime_ms};

/// Сколько витков крутит [`spin_burn`] в молчащем режиме.
///
/// Миллиард — это под QEMU 1,5 с на x86-64 и 0,7 с на AArch64. Снизу счёт обязан
/// быть заметно длиннее, чем оборот команды по серийной линии, — иначе «оболочка
/// ответила посреди счёта» превращается в совпадение; сверху его ограничивает
/// сама оболочка, заканчивающая сеанс через двадцать секунд без ввода.
const ROUNDS: u64 = 1_000_000_000;

/// Сколько витков в одном куске жгущего режима.
///
/// Десять миллионов — десятки миллисекунд под эмулятором: достаточно редко,
/// чтобы вопрос о процессоре не стал основной работой программы, и достаточно
/// часто, чтобы переезд с процессора на процессор был виден.
const BURN_CHUNK: u64 = 10_000_000;

// Цикл вычитания: две инструкции, ни одного обращения к памяти и ни одной
// ловушки наружу. Нулевой аргумент отсекается проверкой до входа.
//
// Тип секции пишется по-разному: на AArch64 `@` начинает комментарий, поэтому
// там `%progbits`, а на x86-64 — `@progbits`.
#[cfg(target_arch = "x86_64")]
global_asm!(
    r#"
.section .text.spin, "ax", @progbits
.balign 16
.globl spin_burn
.hidden spin_burn
spin_burn:
    test    rdi, rdi
    jz      spin_burn_done
spin_burn_loop:
    dec     rdi
    jnz     spin_burn_loop
spin_burn_done:
    ret
"#
);

#[cfg(target_arch = "aarch64")]
global_asm!(
    r#"
.section .text.spin, "ax", %progbits
.balign 16
.globl spin_burn
.hidden spin_burn
spin_burn:
    cbz     x0, spin_burn_done
spin_burn_loop:
    subs    x0, x0, #1
    bne     spin_burn_loop
spin_burn_done:
    ret
"#
);

unsafe extern "C" {
    /// Прокрутить `rounds` витков и вернуться. Ничего не читает и не пишет.
    fn spin_burn(rounds: u64);
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let me = pid();

    if let Some(ms) = args.get(1).and_then(parse_u64) {
        burn(me, ms);
    }

    let start = uptime_ms();

    print("spin ");
    print_u64(me);
    println(": no system calls from here on");

    // SAFETY: функция не трогает ни память, ни стек — только собственный
    // счётчик в регистре аргумента.
    unsafe { spin_burn(ROUNDS) };

    print("spin ");
    print_u64(me);
    print(": done after ");
    print_u64(uptime_ms() - start);
    println(" ms, never yielded once");
    exit(0)
}

/// Жечь процессор `ms` миллисекунд, запоминая, на каких процессорах довелось.
fn burn(me: u64, ms: u64) -> ! {
    let start = uptime_ms();
    let mut seen = 0u64;
    loop {
        seen |= 1 << (cpu() & 63);
        if uptime_ms() - start >= ms {
            break;
        }
        // SAFETY: см. молчащий режим.
        unsafe { spin_burn(BURN_CHUNK) };
    }
    let elapsed = uptime_ms() - start;

    // Строка собирается целиком и уходит одним вызовом: соседняя программа
    // печатает то же самое в тот же миг, и строка, отправленная кусками,
    // срослась бы с её строкой.
    let mut line = Text::new();
    line.push("spin ");
    line.number(me);
    line.push(": burned ");
    line.number(elapsed);
    line.push(" ms on cpus");
    for index in 0..64 {
        if seen & (1 << index) != 0 {
            line.push(" ");
            line.number(index);
        }
    }
    line.push("\n");
    print(line.as_str());
    exit(0)
}

/// Десятичное число без знака. `None` — не число или не помещается.
fn parse_u64(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    let mut value = 0u64;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
    }
    Some(value)
}

/// Строка фиксированной длины на стеке — ради одного вызова `write`.
struct Text {
    bytes: [u8; 192],
    len: usize,
}

impl Text {
    const fn new() -> Self {
        Self { bytes: [0; 192], len: 0 }
    }

    /// Дописать текст; не поместившееся отбрасывается, а не роняет программу.
    fn push(&mut self, text: &str) {
        for &byte in text.as_bytes() {
            if self.len == self.bytes.len() {
                return;
            }
            self.bytes[self.len] = byte;
            self.len += 1;
        }
    }

    fn number(&mut self, mut value: u64) {
        let mut digits = [0u8; 20];
        let mut at = digits.len();
        loop {
            at -= 1;
            digits[at] = b'0' + (value % 10) as u8;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        for &digit in &digits[at..] {
            if self.len == self.bytes.len() {
                return;
            }
            self.bytes[self.len] = digit;
            self.len += 1;
        }
    }

    fn as_str(&self) -> &str {
        // Внутрь попадают только ASCII-цифры и куски `&str` целиком.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}
