//! `autorun` — программы «Автозагрузки» при входе (фаза П3, 20.11).
//!
//! # Зачем
//!
//! «Автозагрузка» (`/bin/startup`) ведёт список `/etc/autostart`: что открыть,
//! когда стол поднялся, — как папка «Автозагрузка» в Windows. Кто-то должен
//! этот список исполнить. Стол — окно ядра, и тащить в ядро чтение файла и
//! запуск программ ради одной строки незачем: это работа на один проход, и её
//! делает служба init, которая запускает строки и выходит с нулём (init такую
//! не перезапускает — см. `init.rs`, `reap`).
//!
//! # Формат `/etc/autostart`
//!
//! По строке на программу: `on <командная строка>` или `off <командная
//! строка>`; `#` — комментарий. Выключенная строка остаётся в файле, чтобы её
//! можно было включить обратно, не вспоминая команду.
//!
//! # Чего здесь нет
//!
//! Сеанса пользователя: служба работает от того, что записано в
//! `/etc/services` (root), и программы запускаются от того же имени. Свой
//! список на каждого пользователя — когда у стола появится вход с выбором
//! учётной записи до запуска программ.

#![no_std]
#![no_main]

use user_progs::{Line, close, exit, monotonic_ms, open, read, sleep_ms, spawn, sysinfo};

/// Файл списка.
const LIST: &str = "/etc/autostart";

/// Сколько ждать графики: программа без стола открыть окно не сможет.
const WAIT_GRAPHICS_MS: u64 = 20_000;

/// Пауза после появления графики: стол при загрузке занят, и окно, открытое в
/// первую же секунду, спорит с ним за кадр (см. заголовок `/etc/services` про
/// монитор системы).
const SETTLE_MS: u64 = 1_500;

/// Самый большой список, который читается: строк в нём единицы.
const MAX_FILE: usize = 4096;

#[unsafe(no_mangle)]
pub extern "C" fn _start(_argc: usize, _argv: *const *const u8) -> ! {
    let mut buffer = [0u8; MAX_FILE];
    let fd = open(LIST);
    if fd < 0 {
        Line::to_log().str("autorun: no ").str(LIST).end();
        exit(0);
    }
    let mut got = 0usize;
    while got < buffer.len() {
        let step = read(fd, &mut buffer[got..]);
        if step <= 0 {
            break;
        }
        got += step as usize;
    }
    close(fd);
    let Ok(text) = core::str::from_utf8(&buffer[..got]) else {
        Line::to_log().str("autorun: ").str(LIST).str(" is not UTF-8 text, nothing started").end();
        exit(0);
    };
    let wanted = text.lines().filter(|line| command_of(line).is_some()).count();
    if wanted == 0 {
        Line::to_log().str("autorun: 0 program(s) started (nothing enabled in ").str(LIST).str(")").end();
        exit(0);
    }

    // Графика: без неё запускать окна некуда.
    let deadline = monotonic_ms() + WAIT_GRAPHICS_MS;
    loop {
        if sysinfo().is_some_and(|info| info.screen_w != 0 && info.pixel_format != 0) {
            break;
        }
        if monotonic_ms() >= deadline {
            Line::to_log().str("autorun: no graphics, ").num(wanted as u64).str(" program(s) not started").end();
            exit(0);
        }
        sleep_ms(200);
    }
    sleep_ms(SETTLE_MS);

    let mut started = 0u64;
    for line in text.lines() {
        let Some(given) = command_of(line) else { continue };
        let mut path = [0u8; 256];
        let Some(command) = absolute(given, &mut path) else {
            Line::to_log().str("autorun: command too long, skipped").end();
            continue;
        };
        let id = spawn(command);
        if id >= 0 {
            started += 1;
            Line::to_log().str("autorun: started '").str(command).str("' as ").signed(id).end();
        } else {
            Line::to_log().str("autorun: cannot start '").str(command).str("': ").signed(id).end();
        }
        // Окна по одному: каждое открытие держит стол.
        sleep_ms(300);
    }
    Line::to_log().str("autorun: ").num(started).str(" program(s) started").end();
    exit(0)
}

/// Команда включённой строки `on <команда>`; всё прочее — `None`.
fn command_of(line: &str) -> Option<&str> {
    let line = line.trim();
    let rest = line.strip_prefix("on")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let command = rest.trim();
    (!command.is_empty()).then_some(command)
}

/// Короткое имя (`notepad`) — программа из `/bin`; полный путь — как есть.
fn absolute<'a>(command: &'a str, buffer: &'a mut [u8; 256]) -> Option<&'a str> {
    if command.starts_with('/') {
        return Some(command);
    }
    let prefix = b"/bin/";
    let total = prefix.len() + command.len();
    if total > buffer.len() {
        return None;
    }
    buffer[..prefix.len()].copy_from_slice(prefix);
    buffer[prefix.len()..total].copy_from_slice(command.as_bytes());
    core::str::from_utf8(&buffer[..total]).ok()
}
