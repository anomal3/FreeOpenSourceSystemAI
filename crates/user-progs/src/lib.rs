//! Обвязка пользовательской программы: системные вызовы и то, без чего
//! `no_std`-бинарник не линкуется.
//!
//! # Что здесь важно понимать
//!
//! Этот код исполняется **не в ядре**. У него нет доступа ни к одной строчке
//! ядра, ни к его памяти, ни к его функциям: страницы ядра не помечены
//! доступными из третьего кольца (EL0), и обращение к ним даёт отказ. Всё, что
//! программа может сделать с системой, проходит через [`syscall`] — и это
//! ровно то, ради чего вся фаза затевалась.
//!
//! # Почему обвязка своя, а не `libc`
//!
//! Потому что связывать первый же запуск пользовательской программы с чужой
//! библиотекой значит отлаживать сразу и её. Здесь тридцать строк, целиком
//! видимых глазом: три инструкции на архитектуру и печать строки.

#![no_std]

use core::panic::PanicInfo;

use user_abi::{
    FD_STDOUT, SYS_CLOSE, SYS_EXIT, SYS_GETGID, SYS_GETPID, SYS_GETUID, SYS_OPEN, SYS_READ,
    SYS_SLEEP, SYS_STAT, SYS_UPTIME, SYS_WRITE, SYS_YIELD, Stat,
};

/// Выполнить системный вызов.
///
/// # Safety
///
/// Аргументы обязаны иметь смысл, которого ждёт вызов: указатель — указывать на
/// живую память программы, длина — не выходить за её пределы. Ядро проверяет
/// адреса, но проверка отвечает «этот адрес не мой», а не «эти байты те».
#[inline]
unsafe fn syscall(number: usize, a0: usize, a1: usize, a2: usize) -> i64 {
    let result: i64;

    #[cfg(target_arch = "x86_64")]
    // SAFETY: контракт функции. `int 0x80` — ловушка с DPL 3, ядро её ждёт.
    // rcx и r11 в списке испорченных не нужны (их портит `syscall`, а не
    // `int`), но перечисление лишнего безвредно, а пропуск нужного — нет.
    unsafe {
        core::arch::asm!(
            "int 0x80",
            inlateout("rax") number as i64 => result,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            options(nostack),
        );
    }

    #[cfg(target_arch = "aarch64")]
    // SAFETY: контракт функции. `svc #0` — единственный способ попасть из EL0
    // в EL1.
    unsafe {
        core::arch::asm!(
            "svc #0",
            in("x8") number,
            inlateout("x0") a0 => result,
            in("x1") a1,
            in("x2") a2,
            options(nostack),
        );
    }

    result
}

/// Напечатать строку.
pub fn print(text: &str) {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe {
        syscall(SYS_WRITE, FD_STDOUT, text.as_ptr() as usize, text.len());
    }
}

/// Напечатать строку и перевести строку.
pub fn println(text: &str) {
    print(text);
    print("\n");
}

/// Напечатать беззнаковое число.
///
/// Своё, а не `write!`: форматирование `core::fmt` тянет за собой заметный кусок
/// кода, а нужна одна десятичная запись.
pub fn print_u64(mut value: u64) {
    let mut buffer = [0u8; 20];
    let mut index = buffer.len();
    loop {
        index -= 1;
        buffer[index] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    // SAFETY: в буфер записаны только цифры ASCII.
    print(unsafe { core::str::from_utf8_unchecked(&buffer[index..]) });
}

/// Напечатать знаковое число.
pub fn print_i64(value: i64) {
    if value < 0 {
        print("-");
        // `unsigned_abs`, а не `-value`: у самого младшего `i64`
        // противоположного значения не существует, и обычное отрицание было бы
        // переполнением.
        print_u64(value.unsigned_abs());
    } else {
        print_u64(value as u64);
    }
}

/// Напечатать число в восьмеричной записи с ведущими нулями до четырёх знаков —
/// то, как принято показывать права.
pub fn print_octal(value: u32) {
    let mut buffer = [b'0'; 11];
    let mut index = buffer.len();
    let mut rest = value;
    loop {
        index -= 1;
        buffer[index] = b'0' + (rest % 8) as u8;
        rest /= 8;
        if rest == 0 {
            break;
        }
    }
    // Ведущие нули: `644` и `0644` — одно и то же число, но второе сразу
    // сообщает, что запись восьмеричная.
    let start = index.min(buffer.len() - 4);
    // SAFETY: в буфер записаны только цифры ASCII.
    print(unsafe { core::str::from_utf8_unchecked(&buffer[start..]) });
}

/// Открыть файл на чтение. Отрицательный результат — код ошибки из `user_abi`.
///
/// Обёртки возвращают числа договора, а не `Result`: перевод кода в тип — это
/// уже библиотека, а здесь тридцать строк, целиком видимых глазом. Программа,
/// которой понадобится `Result`, построит его сама и там, где ей удобно.
pub fn open(path: &str) -> i64 {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_OPEN, path.as_ptr() as usize, path.len(), 0) }
}

/// Прочитать в буфер. Ноль означает конец файла.
pub fn read(fd: i64, buffer: &mut [u8]) -> i64 {
    if fd < 0 {
        return fd;
    }
    // SAFETY: буфер принадлежит программе и доступен ей на запись; ядро
    // проверит это ещё раз по своим таблицам.
    unsafe { syscall(SYS_READ, fd as usize, buffer.as_mut_ptr() as usize, buffer.len()) }
}

/// Закрыть дескриптор.
pub fn close(fd: i64) -> i64 {
    if fd < 0 {
        return fd;
    }
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_CLOSE, fd as usize, 0, 0) }
}

/// Спросить о файле, не открывая его.
pub fn stat(path: &str, out: &mut Stat) -> i64 {
    // SAFETY: и путь, и приёмник лежат в памяти программы; выравнивание `Stat`
    // обеспечено типом.
    unsafe {
        syscall(
            SYS_STAT,
            path.as_ptr() as usize,
            path.len(),
            core::ptr::from_mut(out) as usize,
        )
    }
}

/// От чьего имени исполняется программа.
#[must_use]
pub fn uid() -> u32 {
    // SAFETY: аргументов у вызова нет.
    let value = unsafe { syscall(SYS_GETUID, 0, 0, 0) };
    value.max(0) as u32
}

/// Номер этой программы — он же номер её задачи в ядре.
#[must_use]
pub fn pid() -> u64 {
    // SAFETY: аргументов у вызова нет.
    let value = unsafe { syscall(SYS_GETPID, 0, 0, 0) };
    value.max(0) as u64
}

/// Группа, от имени которой исполняется программа.
#[must_use]
pub fn gid() -> u32 {
    // SAFETY: аргументов у вызова нет.
    let value = unsafe { syscall(SYS_GETGID, 0, 0, 0) };
    value.max(0) as u32
}

/// Уступить процессор, оставаясь готовой к исполнению.
pub fn yield_now() {
    // SAFETY: аргументов у вызова нет.
    unsafe {
        syscall(SYS_YIELD, 0, 0, 0);
    }
}

/// Уснуть на указанное число миллисекунд.
///
/// В отличие от [`yield_now`], спящая программа выходит из очереди на
/// исполнение: пока она спит, процессор достаётся другим — или не достаётся
/// никому, и тогда машина простаивает по-настоящему.
pub fn sleep_ms(ms: u64) {
    // SAFETY: аргумент — число.
    unsafe {
        syscall(SYS_SLEEP, ms as usize, 0, 0);
    }
}

/// Сколько миллисекунд работает система.
#[must_use]
pub fn uptime_ms() -> u64 {
    // SAFETY: аргументов у вызова нет; результат — беззнаковое число.
    let value = unsafe { syscall(SYS_UPTIME, 0, 0, 0) };
    value.max(0) as u64
}

/// Завершить программу.
pub fn exit(code: i64) -> ! {
    // SAFETY: вызов не возвращается.
    unsafe {
        syscall(SYS_EXIT, code as usize, 0, 0);
    }
    // Ядро сюда не возвращается. Если всё-таки вернулось — крутимся, уступая
    // процессор: свалиться за конец функции значило бы исполнять мусор.
    loop {
        yield_now();
    }
}

/// Обработчик паники программы.
///
/// Печатает и завершается, а не останавливает машину: паника **программы** —
/// это её дело, и система обязана её пережить. В этом и разница между кодом в
/// ядре и кодом здесь.
#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    println("user: panic");
    exit(101)
}
