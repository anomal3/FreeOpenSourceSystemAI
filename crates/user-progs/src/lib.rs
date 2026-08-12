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
    O_CREATE, O_TRUNC, O_WRITE, SYS_MKDIR, SYS_REMOVE, SYS_SEEK, SYS_SLEEP, SYS_STAT, SYS_TIME,
    SYS_UPTIME, SYS_WRITE, SYS_YIELD, Stat,
};

pub use user_abi::{SEEK_CUR, SEEK_END, SEEK_SET};

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

/// Открыть файл на запись, при необходимости создав или обрезав его.
///
/// Два булевых аргумента вместо битовой маски: их здесь ровно столько, сколько
/// различает договор, и разбирать маску в каждой программе значило бы написать
/// тридцать строк библиотеки ради того, чтобы спрятать два вопроса.
pub fn open_write(path: &str, create: bool, truncate: bool) -> i64 {
    let mut flags = O_WRITE;
    if create {
        flags |= O_CREATE;
    }
    if truncate {
        flags |= O_TRUNC;
    }
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_OPEN, path.as_ptr() as usize, path.len(), flags) }
}

/// Записать в дескриптор. Возвращает, сколько записано.
pub fn write(fd: i64, data: &[u8]) -> i64 {
    if fd < 0 {
        return fd;
    }
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_WRITE, fd as usize, data.as_ptr() as usize, data.len()) }
}

/// Создать каталог.
pub fn mkdir(path: &str, mode: u32) -> i64 {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_MKDIR, path.as_ptr() as usize, path.len(), mode as usize) }
}

/// Удалить файл или пустой каталог.
pub fn remove(path: &str) -> i64 {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_REMOVE, path.as_ptr() as usize, path.len(), 0) }
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

/// Аргументы командной строки в том виде, в каком их передало ядро.
///
/// Программа получает их первыми двумя аргументами `_start`: число и адрес
/// массива указателей на строки с завершающим нулём. Строки лежат в её
/// собственном стеке — их положило туда ядро до входа в третье кольцо, — то
/// есть читаются как обычная память, без единого системного вызова.
///
/// Нулевой аргумент — путь, которым программу запустили; так это устроено во
/// всяком Unix, и программе, которая печатает своё имя в сообщении об ошибке,
/// взять его больше неоткуда.
pub struct Args {
    argc: usize,
    argv: *const *const u8,
}

impl Args {
    /// Обернуть то, что пришло в `_start`.
    ///
    /// # Safety
    ///
    /// Вызывать можно только с теми значениями, которые ядро передало в
    /// `_start`: указатель обязан вести на массив из `argc` строк, завершённых
    /// нулём. Сочинить их самому и получить чтение чужой памяти — ровно то, от
    /// чего эта пометка предостерегает.
    #[must_use]
    pub const unsafe fn new(argc: usize, argv: *const *const u8) -> Self {
        Self { argc, argv }
    }

    /// Сколько аргументов, включая нулевой.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.argc
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.argc == 0
    }

    /// Аргумент по номеру. `None`, если такого нет.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&str> {
        if index >= self.argc || self.argv.is_null() {
            return None;
        }
        // SAFETY: индекс проверен, массив построен ядром по контракту `new`.
        let pointer = unsafe { self.argv.add(index).read_unaligned() };
        if pointer.is_null() {
            return None;
        }

        // Длина ищется по завершающему нулю — с потолком: строка, у которой его
        // почему-то не оказалось, иначе увела бы поиск за пределы стека.
        let mut len = 0;
        while len < MAX_ARG_LEN {
            // SAFETY: читаем внутри строки, положенной ядром в стек этой
            // программы; предел не даёт выйти за него, даже если нуля нет.
            if unsafe { pointer.add(len).read() } == 0 {
                break;
            }
            len += 1;
        }

        // SAFETY: адрес и длина получены выше; строки приходят от ядра, которое
        // взяло их из командной строки — то есть из UTF-8.
        let bytes = unsafe { core::slice::from_raw_parts(pointer, len) };
        core::str::from_utf8(bytes).ok()
    }
}

/// Предел длины одного аргумента при поиске завершающего нуля.
const MAX_ARG_LEN: usize = 255;

/// Передвинуть позицию в открытом файле, вернуть новую.
///
/// `whence` — [`SEEK_SET`], [`SEEK_CUR`] или [`SEEK_END`]. Смещение знаковое:
/// `seek(fd, -16, SEEK_END)` — это «шестнадцать байт с конца», и именно так
/// читают хвост файла, не читая всего остального.
pub fn seek(fd: i64, offset: i64, whence: usize) -> i64 {
    if fd < 0 {
        return fd;
    }
    // SAFETY: аргументы — числа; ядро проверит дескриптор само.
    unsafe { syscall(SYS_SEEK, fd as usize, offset as usize, whence) }
}

/// Размер открытого файла в байтах.
///
/// Написано через `seek`, а не через `stat`: `stat` спрашивает про имя, а имя
/// могло к этому моменту указывать уже на другой файл. Дескриптор указывает на
/// тот файл, который открыли.
pub fn file_size(fd: i64) -> i64 {
    let saved = seek(fd, 0, SEEK_CUR);
    if saved < 0 {
        return saved;
    }
    let size = seek(fd, 0, SEEK_END);
    // Позиция возвращается на место: измерение не должно менять состояние того,
    // что измеряют.
    seek(fd, saved, SEEK_SET);
    size
}

/// Текущее время в секундах эпохи Unix, UTC.
///
/// Ноль означает «система не знает, который час», а не 1970 год: часов не было
/// ни у прошивки, ни у платы. Программе, ставящей метку, эти два случая
/// различать обязательно.
pub fn time_now() -> u64 {
    // SAFETY: аргументов у вызова нет; результат — беззнаковое число.
    let value = unsafe { syscall(SYS_TIME, 0, 0, 0) };
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
