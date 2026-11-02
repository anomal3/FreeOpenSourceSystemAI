//! `bigfile <путь>` — файл больше 4 ГиБ на томе состояния (02.11).
//!
//! # Зачем
//!
//! С 02.11 раздел состояния — btrfs, и одна из причин переезда — большие
//! файлы: FAT не держит больше 4 ГиБ, а смещение, где-то по дороге
//! сжатое в 32 бита, молча записало бы данные в начало файла. Проверка
//! пишет мегабайт **поперёк** границы 4 ГиБ и мегабайт на 5 ГиБ, читает их
//! обратно и сверяет каждый байт; между ними дыра, которая обязана читаться
//! нулями. Дыра же позволяет проверить смещения за 4 ГиБ на диске стенда,
//! где пяти гигабайт свободного места нет.
//!
//! Байт зависит от своего **абсолютного** смещения, поэтому кусок, легший
//! не туда (смещение по модулю 2³²), при сверке не совпадёт.

#![no_std]
#![no_main]

use user_abi::{SEEK_SET, Stat};
use user_progs::{Args, Line, close, exit, open, open_write, read, remove, seek, stat, write};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
/// Первый кусок — полмегабайта до границы 4 ГиБ и полмегабайта после.
const FIRST: u64 = 4 * GIB - MIB / 2;
/// Второй — на 5 ГиБ: размер файла станет 5 ГиБ + 1 МиБ.
const SECOND: u64 = 5 * GIB;
const PIECE: u64 = MIB;
/// Середина дыры между кусками.
const HOLE: u64 = 4 * GIB + GIB / 2;
const CHUNK: usize = 16 * 1024;

fn byte_at(offset: u64) -> u8 {
    // Не повторяется ни с периодом 2³², ни с периодом страницы.
    (offset.wrapping_mul(0x9E37_79B1) >> 24) as u8 ^ (offset >> 32) as u8
}

fn fail(what: &str, value: i64) -> ! {
    Line::to_log().str("bigfile: FAILED ").str(what).str(" (").signed(value).str(")").end();
    exit(1)
}

fn write_piece(fd: i64, at: u64) {
    let moved = seek(fd, at as i64, SEEK_SET);
    if moved != at as i64 {
        fail("seek for write", moved);
    }
    let mut buffer = [0u8; CHUNK];
    let mut done = 0u64;
    while done < PIECE {
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = byte_at(at + done + index as u64);
        }
        let wrote = write(fd, &buffer);
        if wrote != CHUNK as i64 {
            fail("write", wrote);
        }
        done += CHUNK as u64;
    }
}

/// Прочитать `len` байт с `at` и сверить: с узором или с нулями.
fn check_piece(fd: i64, at: u64, len: u64, zeros: bool) {
    let moved = seek(fd, at as i64, SEEK_SET);
    if moved != at as i64 {
        fail("seek for read", moved);
    }
    let mut buffer = [0u8; CHUNK];
    let mut done = 0u64;
    while done < len {
        let got = read(fd, &mut buffer);
        if got != CHUNK as i64 {
            fail("read", got);
        }
        for (index, byte) in buffer.iter().enumerate() {
            let offset = at + done + index as u64;
            let expected = if zeros { 0 } else { byte_at(offset) };
            if *byte != expected {
                Line::to_log()
                    .str("bigfile: FAILED byte at ")
                    .num(offset)
                    .str(" is ")
                    .num(u64::from(*byte))
                    .str(", expected ")
                    .num(u64::from(expected))
                    .end();
                exit(1);
            }
        }
        done += CHUNK as u64;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(argc: usize, argv: *const *const u8) -> ! {
    // SAFETY: значения пришли от ядра в том виде, в каком их описывает договор.
    let args = unsafe { Args::new(argc, argv) };
    let Some(path) = args.get(1) else {
        Line::to_log().str("bigfile: usage: bigfile <path>").end();
        exit(2)
    };
    let _ = remove(path);

    let fd = open_write(path, true, true);
    if fd < 0 {
        fail("open for write", fd);
    }
    write_piece(fd, FIRST);
    write_piece(fd, SECOND);
    close(fd);

    let mut info = Stat::default();
    let status = stat(path, &mut info);
    if status < 0 {
        fail("stat", status);
    }
    if info.size != SECOND + PIECE {
        fail("size", info.size as i64);
    }

    let fd = open(path);
    if fd < 0 {
        fail("open for read", fd);
    }
    check_piece(fd, FIRST, PIECE, false);
    check_piece(fd, SECOND, PIECE, false);
    check_piece(fd, HOLE, 64 * 1024, true);
    close(fd);

    let removed = remove(path);
    if removed < 0 {
        fail("remove", removed);
    }
    Line::to_log()
        .str("bigfile: ok, ")
        .num(info.size)
        .str(" bytes across 4 GiB written, read back and removed")
        .end();
    exit(0)
}
