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

pub mod http;

use core::panic::PanicInfo;

use user_abi::{
    ERR_BAD_PATH, FD_STDERR, FD_STDOUT, SYS_CLOSE, SYS_EXIT, SYS_GETGID, SYS_GETPID,
    SYS_GETUID, SYS_OPEN, SYS_READ, O_CREATE, O_TRUNC, O_WRITE, SYS_MKDIR, SYS_READDIR,
    SYS_REMOVE, SYS_RENAME, SYS_SEEK, SYS_SLEEP, SYS_SPAWN, SYS_STAT, SYS_TIME, SYS_TTYMODE,
    SYS_UPTIME, SYS_WAIT, SYS_WINSIZE, SYS_WRITE, SYS_YIELD, SPAWN_INHERIT, Stat, TTY_LINE,
    TTY_RAW, WAIT_NOHANG,
};

use user_abi::{LAUNCH_KEEP, Launch, SYS_LAUNCH, SYS_PIPE};

use user_abi::SYS_UPDATE;

use user_abi::{
    SOCK_TCP, SOCK_UDP, SYS_ACCEPT, SYS_BIND, SYS_CLOSE_SOCKET, SYS_CONNECT, SYS_LISTEN,
    SYS_NETCONF, SYS_NETINFO, SYS_PEER, SYS_RECV, SYS_RESOLVE, SYS_SEND, SYS_SHUTDOWN,
    SYS_RANDOM, SYS_SOCKET, SYS_STREAMSTATE,
};

pub use user_abi::{
    Dirent, ERR_AGAIN, ERR_BROKEN_PIPE, ERR_NOT_FOUND, ERR_NO_NETWORK, ERR_NO_TASK, ERR_PERMISSION,
    ERR_UPDATE_REFUSED, FD_STDIN, KIND_DIRECTORY, KIND_FILE, NetConfig, NetInfo, Peer, SEEK_CUR,
    SEEK_END, SEEK_SET, SLOT_A, SLOT_B, StreamState,
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

/// Создать файл с заданными правами и открыть его на запись.
///
/// Отличается от [`open_write`] тем, что права выбирает программа, а не ядро, и
/// тем, что занятое имя — отказ, а не «обрежу и открою». Нужно тому, кто
/// раскладывает чужие файлы: программа, положенная без бита исполнения, не
/// запустится, а положенная поверх существующей — испортит систему.
pub fn create(path: &str, mode: u16) -> i64 {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe {
        syscall(
            user_abi::SYS_CREATE,
            path.as_ptr() as usize,
            path.len(),
            mode as usize,
        )
    }
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

/// Прочитать из потока ввода. Ноль означает конец ввода, а не ошибку.
///
/// Вызов **останавливает программу** до тех пор, пока человек чего-нибудь не
/// наберёт: в этом и разница между потоком ввода и файлом, у которого конец
/// наступает сам. Неблокирующего чтения здесь нет, и обещать его сейчас значило
/// бы обещать поведение, которого не существует.
pub fn read_stdin(buffer: &mut [u8]) -> i64 {
    // SAFETY: буфер принадлежит программе и доступен ей на запись; ядро
    // проверит это ещё раз по своим таблицам.
    unsafe {
        syscall(
            SYS_READ,
            FD_STDIN,
            buffer.as_mut_ptr() as usize,
            buffer.len(),
        )
    }
}

/// Прочитать один байт ввода. `None` — ввод кончился.
///
/// Существует ради полноэкранных программ: в прямом режиме клавиша приезжает
/// последовательностью байтов, и разбирать её приходится по одному.
pub fn read_key() -> Option<u8> {
    let mut byte = [0u8; 1];
    match read_stdin(&mut byte) {
        1 => Some(byte[0]),
        _ => None,
    }
}

/// Прочитать строку без завершающего перевода.
///
/// `None` означает конец ввода до первого байта — это не то же самое, что
/// пустая строка, и различать их обязана программа: одно значит «человек нажал
/// Enter», другое — «спрашивать больше некого».
pub fn read_line(buffer: &mut [u8]) -> Option<&str> {
    let mut len = 0;
    while len < buffer.len() {
        let mut byte = [0u8; 1];
        if read_stdin(&mut byte) != 1 {
            // Ввод кончился. Если что-то уже набрано — это строка, и её надо
            // отдать; если нет — отдавать нечего.
            if len == 0 {
                return None;
            }
            break;
        }
        if byte[0] == b'\n' || byte[0] == b'\r' {
            break;
        }
        buffer[len] = byte[0];
        len += 1;
    }
    // Не-UTF-8 в строке — это байты, которых не набирала клавиатура: отдать их
    // как текст нельзя, а заменить вопросительными знаками значило бы соврать
    // про то, что было набрано.
    core::str::from_utf8(&buffer[..len]).ok()
}

/// Размер окна в знаках: столбцы и строки.
///
/// Нули означают, что окна нет вовсе — система работает в серийной консоли.
/// Программа, рисующая рамку, обязана этот случай различать: рамка шириной ноль
/// не рисуется, а не рисуется криво.
#[must_use]
pub fn window_size() -> (u32, u32) {
    // SAFETY: аргументов у вызова нет.
    let packed = unsafe { syscall(SYS_WINSIZE, 0, 0, 0) };
    if packed < 0 {
        return (0, 0);
    }
    let packed = packed as u64;
    ((packed >> 32) as u32, packed as u32)
}

/// Переключить терминал в прямой режим или вернуть в построчный.
///
/// В прямом режиме клавиша приезжает немедленно и без эха — так читает
/// клавиатуру всякая полноэкранная программа. Вернуть режим стоит самой
/// программе; ядро вернёт его и само, когда программа закончится, но полагаться
/// на уборку за собой — не то же самое, что убрать.
pub fn set_raw(raw: bool) -> i64 {
    let mode = if raw { TTY_RAW } else { TTY_LINE };
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_TTYMODE, mode, 0, 0) }
}

/// Напечатать в поток диагностики.
///
/// Отличается от [`print`] адресатом, а не оформлением: второй дескриптор идёт
/// в журнал системы, минуя окно. Полноэкранной программе это единственный
/// способ сказать слово, не испортив картинку.
pub fn error(text: &str) {
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe {
        syscall(SYS_WRITE, FD_STDERR, text.as_ptr() as usize, text.len());
    }
}

/// Напечатать число в поток диагностики.
pub fn error_num(value: i64) {
    let negative = value < 0;
    let mut buffer = [0u8; 21];
    let mut index = buffer.len();
    let mut rest = value.unsigned_abs();
    loop {
        index -= 1;
        buffer[index] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if negative {
        index -= 1;
        buffer[index] = b'-';
    }
    // SAFETY: в буфер записаны только цифры ASCII и знак.
    error(unsafe { core::str::from_utf8_unchecked(&buffer[index..]) });
}

/// Переименовать файл или каталог.
///
/// Оба пути уезжают одним буфером: у системного вызова три аргумента, а нужно
/// четыре значения — два адреса и две длины. Почему выбрана склейка, а не
/// расширение соглашения о вызовах, сказано в договоре у `SYS_RENAME`.
pub fn rename(old: &str, new: &str) -> i64 {
    /// Столько же, сколько принимает ядро.
    const MAX_PATH: usize = 255;

    let mut buffer = [0u8; 2 * MAX_PATH];
    let total = old.len() + new.len();
    if old.is_empty() || new.is_empty() || total > buffer.len() {
        return ERR_BAD_PATH;
    }
    buffer[..old.len()].copy_from_slice(old.as_bytes());
    buffer[old.len()..total].copy_from_slice(new.as_bytes());
    // SAFETY: буфер живёт в памяти программы, длины — его собственные.
    unsafe { syscall(SYS_RENAME, buffer.as_ptr() as usize, old.len(), total) }
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

/// Прочитать очередную запись открытого каталога.
///
/// `true` — запись записана в `out`, `false` — каталог кончился. Отрицательный
/// код от ядра тоже даёт `false`: программе, перечисляющей каталог, отличать
/// «кончилось» от «сломалось» обычно нечем и незачем — а той, которой нужно,
/// доступен `readdir_raw`.
pub fn readdir(fd: i64, out: &mut Dirent) -> bool {
    readdir_raw(fd, out) == 1
}

/// То же, но с кодом ядра как есть: `1`, `0` или ошибка.
pub fn readdir_raw(fd: i64, out: &mut Dirent) -> i64 {
    if fd < 0 {
        return fd;
    }
    // SAFETY: структура принадлежит программе и доступна ей на запись; ядро
    // проверит это ещё раз по своим таблицам.
    unsafe {
        syscall(
            SYS_READDIR,
            fd as usize,
            core::ptr::from_mut(out) as usize,
            size_of::<Dirent>(),
        )
    }
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

/// Запустить программу отдельной задачей и вернуть её номер.
///
/// `line` — путь и аргументы одной строкой, как их набирают в оболочке. Права
/// достаются те же, что у запускающего; чтобы отдать другие, есть
/// [`spawn_as`].
pub fn spawn(line: &str) -> i64 {
    // SAFETY: строка живёт в памяти программы, длина — её собственная.
    unsafe { syscall(SYS_SPAWN, line.as_ptr() as usize, line.len(), SPAWN_INHERIT) }
}

/// То же, но от имени указанных `uid`/`gid`.
///
/// Снизить права вправе кто угодно, повысить — никто: ядро отвечает
/// `ERR_PERMISSION`, если запускающий не root, а просят чужие.
pub fn spawn_as(line: &str, uid: u32, gid: u32) -> i64 {
    let who = ((uid as usize) << 32) | gid as usize;
    // SAFETY: строка живёт в памяти программы, длина — её собственная.
    unsafe { syscall(SYS_SPAWN, line.as_ptr() as usize, line.len(), who) }
}

/// Завести канал. Возвращает пару `(читающий, пишущий)` или отказ.
///
/// Канал — это байты от одной задачи к другой. Тому, кто отдаёт конец
/// запускаемой программе, придётся закрыть свою копию: пока она жива, на другом
/// конце не наступит конца файла — живой писатель есть, значит данные ещё
/// могут прийти.
pub fn pipe() -> Result<(i64, i64), i64> {
    // SAFETY: аргументов нет, возврат — число.
    let packed = unsafe { syscall(SYS_PIPE, 0, 0, 0) };
    if packed < 0 {
        return Err(packed);
    }
    Ok((packed >> 32, packed & 0xFFFF_FFFF))
}

/// Запустить программу, назвав ей стандартный ввод и вывод.
///
/// `stdin`/`stdout` — дескрипторы концов канала либо [`LAUNCH_KEEP`], если поток
/// оставить как есть. `uid`/`gid` — от чьего имени; `None` означает «от того же,
/// от кого запускают».
///
/// Дескрипторы **не закрываются**: закрыть их — дело вызывающего, и сделать это
/// надо сразу после запуска, иначе конец файла не наступит никогда.
pub fn launch(line: &str, who: Option<(u32, u32)>, stdin: i64, stdout: i64) -> i64 {
    let who = match who {
        Some((uid, gid)) => (u64::from(uid) << 32) | u64::from(gid),
        None => SPAWN_INHERIT as u64,
    };
    let request = Launch {
        command: line.as_ptr() as u64,
        command_len: line.len() as u64,
        who,
        stdin,
        stdout,
    };
    // SAFETY: структура лежит в памяти этой программы и живёт до конца вызова;
    // ядро прочитает её и проверит адреса по своим таблицам.
    unsafe { syscall(SYS_LAUNCH, core::ptr::addr_of!(request) as usize, 0, 0) }
}

/// Оставить поток программы как есть — см. [`launch`].
pub const KEEP: i64 = LAUNCH_KEEP;

/// Дождаться конца задачи и узнать её код возврата.
///
/// Останавливает программу до тех пор, пока та не закончится. `ERR_NO_TASK`
/// означает, что ждать нечего: такой задачи нет — и это не то же самое, что
/// «ещё работает».
pub fn wait(task: i64) -> i64 {
    if task < 0 {
        return task;
    }
    // SAFETY: аргументы — числа.
    unsafe { syscall(SYS_WAIT, task as usize, 0, 0) }
}

/// Спросить, не закончилась ли задача, **не** дожидаясь этого.
///
/// `ERR_AGAIN` — ещё работает. Именно так супервизор присматривает за
/// несколькими службами сразу: ожидание остановило бы его на первой.
pub fn wait_now(task: i64) -> i64 {
    if task < 0 {
        return task;
    }
    // SAFETY: аргументы — числа.
    unsafe { syscall(SYS_WAIT, task as usize, WAIT_NOHANG, 0) }
}

/// Прочитать кусок файла с указанного смещения.
///
/// Написано через [`seek`], а не отдельным вызовом: `pread` в ядре сегодня нет,
/// а программе, читающей контейнер по частям, эти две строки всё равно
/// пришлось бы писать у себя.
pub fn read_at(fd: i64, offset: u64, buffer: &mut [u8]) -> i64 {
    let moved = seek(fd, offset as i64, SEEK_SET);
    if moved < 0 {
        return moved;
    }
    read(fd, buffer)
}

// ---------------------------------------------------------------------------
// Сеть
// ---------------------------------------------------------------------------

/// Завести сокет UDP. Возвращает его номер или отрицательный код ошибки.
pub fn socket() -> i64 {
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_SOCKET, SOCK_UDP, 0, 0) }
}

/// Завести соединение TCP.
pub fn stream() -> i64 {
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_SOCKET, SOCK_TCP, 0, 0) }
}

/// Начать слушать входящие соединения.
pub fn listen(socket: i64) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_LISTEN, socket as usize, 0, 0) }
}

/// Забрать установленное соединение. `ERR_AGAIN` — очередь пуста.
pub fn accept(socket: i64) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_ACCEPT, socket as usize, 0, 0) }
}

/// Закрыть свою половину соединения: читать можно, писать больше нет.
pub fn shutdown(socket: i64) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_SHUTDOWN, socket as usize, 0, 0) }
}

/// Состояние соединения.
pub fn stream_state(socket: i64) -> Option<StreamState> {
    if socket < 0 {
        return None;
    }
    let mut out = StreamState::default();
    // SAFETY: структура живёт в памяти программы.
    let result = unsafe {
        syscall(SYS_STREAMSTATE, socket as usize, (&raw mut out) as usize, 0)
    };
    (result == 0).then_some(out)
}

/// Дождаться, пока соединение установится.
///
/// Возвращает `false`, если за отведённое время связь не поднялась или была
/// оборвана. Ожидание здесь, а не в ядре, намеренно: сколько ждать — решение
/// программы, и она же решает, чем заняться, пока ждёт.
pub fn wait_connected(socket: i64, timeout_ms: u64) -> bool {
    let deadline = uptime_ms() + timeout_ms;
    loop {
        match stream_state(socket) {
            Some(state) if state.reset != 0 => return false,
            Some(state) if state.open != 0 => return true,
            Some(_) => {}
            None => return false,
        }
        if uptime_ms() >= deadline {
            return false;
        }
        sleep_ms(5);
    }
}

/// Привязать сокет к порту; ноль означает «любой свободный».
pub fn bind(socket: i64, port: u16) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргументы — числа.
    unsafe { syscall(SYS_BIND, socket as usize, usize::from(port), 0) }
}

/// Запомнить, кому этот сокет отправляет.
pub fn connect(socket: i64, address: u32, port: u16) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргументы — числа.
    unsafe { syscall(SYS_CONNECT, socket as usize, address as usize, usize::from(port)) }
}

/// Отправить датаграмму. `ERR_AGAIN` означает «ещё выясняется адрес соседа».
pub fn send(socket: i64, data: &[u8]) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: срез живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_SEND, socket as usize, data.as_ptr() as usize, data.len()) }
}

/// Отправить датаграмму, повторяя попытки, пока выясняется адрес соседа.
///
/// Отдельно от [`send`] потому, что `ERR_AGAIN` при первой же отправке —
/// обычное дело (ARP ещё не ответил), и писать этот цикл у себя пришлось бы
/// каждому.
pub fn send_waiting(socket: i64, data: &[u8], attempts: u32) -> i64 {
    for _ in 0..attempts {
        let sent = send(socket, data);
        if sent != ERR_AGAIN {
            return sent;
        }
        sleep_ms(5);
    }
    ERR_AGAIN
}

/// Забрать датаграмму. `ERR_AGAIN` — очереди пока нет.
pub fn recv(socket: i64, buffer: &mut [u8]) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: буфер живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_RECV, socket as usize, buffer.as_mut_ptr() as usize, buffer.len()) }
}

/// Ждать датаграмму до `timeout_ms` миллисекунд.
pub fn recv_waiting(socket: i64, buffer: &mut [u8], timeout_ms: u64) -> i64 {
    let deadline = uptime_ms() + timeout_ms;
    loop {
        let got = recv(socket, buffer);
        if got != ERR_AGAIN {
            return got;
        }
        if uptime_ms() >= deadline {
            return ERR_AGAIN;
        }
        sleep_ms(5);
    }
}

/// Кто прислал последнюю принятую датаграмму.
pub fn peer(socket: i64) -> Option<Peer> {
    if socket < 0 {
        return None;
    }
    let mut out = Peer::default();
    // SAFETY: структура живёт в памяти программы.
    let result = unsafe {
        syscall(SYS_PEER, socket as usize, (&raw mut out) as usize, 0)
    };
    (result == 0).then_some(out)
}

/// Закрыть сокет.
pub fn close_socket(socket: i64) -> i64 {
    if socket < 0 {
        return socket;
    }
    // SAFETY: аргумент — число.
    unsafe { syscall(SYS_CLOSE_SOCKET, socket as usize, 0, 0) }
}

/// Задать настройки интерфейса. Только root.
pub fn netconf(config: &NetConfig) -> i64 {
    // SAFETY: структура живёт в памяти программы, длина — её собственная.
    unsafe {
        syscall(
            SYS_NETCONF,
            (config as *const NetConfig) as usize,
            core::mem::size_of::<NetConfig>(),
            0,
        )
    }
}

/// Заполнить буфер случайными байтами.
///
/// Возвращает `false`, только если ядро отказалось: с буфером программы этого
/// не случается, но проверять всё равно надо — ключ, собранный из
/// неинициализированного массива, выглядит как ключ.
pub fn random(buffer: &mut [u8]) -> bool {
    // SAFETY: буфер живёт в памяти программы, длина — его собственная.
    let result = unsafe {
        syscall(SYS_RANDOM, buffer.as_mut_ptr() as usize, buffer.len(), 0)
    };
    result == buffer.len() as i64
}

/// Спросить, что система знает о своей сети.
pub fn netinfo(out: &mut NetInfo) -> i64 {
    // SAFETY: структура живёт в памяти программы, длина — её собственная.
    unsafe {
        syscall(
            SYS_NETINFO,
            (out as *mut NetInfo) as usize,
            core::mem::size_of::<NetInfo>(),
            0,
        )
    }
}

/// Узнать адрес по имени.
pub fn resolve(name: &str) -> Option<u32> {
    let mut out = [0u8; 4];
    // SAFETY: имя и буфер живут в памяти программы.
    let result = unsafe {
        syscall(SYS_RESOLVE, name.as_ptr() as usize, name.len(), out.as_mut_ptr() as usize)
    };
    (result == 0).then(|| u32::from_be_bytes(out))
}

/// Где лежат правки человека.
pub const CONFIG_ETC: &str = "/etc";

/// Где лежит эталон настроек, приехавший с образом.
pub const CONFIG_DEFAULTS: &str = "/usr/share/defaults/etc";

/// Найти настройку: сначала правку в `/etc`, потом эталон образа.
///
/// Возвращает **готовый путь**, а не открытый дескриптор: читают настройки
/// по-разному (кто целиком, кто построчно), а сказать человеку, откуда файл
/// взят, обязаны все — и для этого нужен именно путь.
///
/// Ровно то же делает ядро (`kernel/src/config.rs`), и по той же причине живёт
/// отдельно: правило одно, а сторон границы две. Зачем правило нужно, сказано
/// там же — `/etc` лежит на разделе состояния, обновление до него не
/// дотягивается, и умолчание, приехавшее с новым образом, иначе не досталось бы
/// никому.
#[must_use]
pub fn config_path(name: &str) -> Option<Path> {
    for prefix in [CONFIG_ETC, CONFIG_DEFAULTS] {
        let mut path = Path::from(prefix)?;
        if !path.join(name) {
            continue;
        }
        let mut info = Stat::default();
        if stat(path.as_str(), &mut info) == 0 && info.kind == user_abi::KIND_FILE {
            return Some(path);
        }
    }
    None
}

/// Поставить обновление системы из контейнера.
///
/// Возвращает [`user_abi::SLOT_A`] или [`user_abi::SLOT_B`] — слот, который
/// станет активным, — либо отрицательный код. Работает **минутами**: внутри
/// перелив десятков мегабайт в чужой раздел, и задача всё это время стоит в
/// ядре.
pub fn apply_update(path: &str) -> i64 {
    // SAFETY: путь живёт в памяти программы, длина — его собственная.
    unsafe { syscall(SYS_UPDATE, path.as_ptr() as usize, path.len(), 0) }
}

/// Путь, собираемый по кусочкам в буфере на стеке.
///
/// Существует потому, что кучи у программы нет, а склеивать пути приходится
/// всем, кто ходит по дереву: `/opt` + имя пакета + путь внутри него. Своя
/// длина у каждого куска, общий предел — тот же, что принимает ядро.
pub struct Path {
    buffer: [u8; MAX_PATH],
    len: usize,
}

/// Самый длинный путь, который принимает ядро.
pub const MAX_PATH: usize = 255;

impl Path {
    #[must_use]
    pub const fn new() -> Self {
        Self { buffer: [0; MAX_PATH], len: 0 }
    }

    /// Начать путь с готовой строки. `None`, если она длиннее предела.
    #[must_use]
    pub fn from(text: &str) -> Option<Self> {
        let mut path = Self::new();
        path.push(text).then_some(path)
    }

    /// Дописать в конец. `false` означает, что не поместилось, — и путь при
    /// этом остаётся прежним, а не обрезанным: обрезанный путь указывает на
    /// другой файл, и работать с ним хуже, чем отказаться.
    pub fn push(&mut self, text: &str) -> bool {
        if self.len + text.len() > MAX_PATH {
            return false;
        }
        self.buffer[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        true
    }

    /// Дописать компонент, поставив перед ним `/`, если его там ещё нет.
    pub fn join(&mut self, name: &str) -> bool {
        if self.len > 0 && self.buffer[self.len - 1] != b'/' && !self.push("/") {
            return false;
        }
        self.push(name)
    }

    /// Сколько байт занято — чтобы потом вернуться к этой длине.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Отбросить всё после указанной длины.
    ///
    /// Так путь переиспользуется в цикле: собрали `/opt/hello/bin/greet`,
    /// поработали, вернулись к `/opt/hello` и собрали следующий.
    pub fn truncate(&mut self, len: usize) {
        if len <= self.len {
            self.len = len;
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // SAFETY: в буфер попадают только байты из `&str`, то есть UTF-8.
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }
}

impl Default for Path {
    fn default() -> Self {
        Self::new()
    }
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
