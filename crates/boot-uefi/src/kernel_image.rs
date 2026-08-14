//! Чтение образа ядра с системного раздела (ESP).
//!
//! Ядро лежит отдельным файлом в корне того же тома, с которого прошивка
//! запустила сам загрузчик; том открывается в [`crate::volume`].

use alloc::vec;
use alloc::vec::Vec;

use slots::Slot;
use uefi::{CStr16, cstr16, println};

use crate::Aborted;
use crate::volume::{self, BootVolume};

/// Путь к ядру относительно корня тома. Ведущий `\` делает путь абсолютным
/// внутри тома — то же самое, что открыть `kernel.elf` в корневом каталоге,
/// но однозначно читается.
const KERNEL_PATH: &CStr16 = cstr16!("\\kernel.elf");

/// Ядра слотов. Отдельные файлы, а не один переименовываемый: переименование
/// посреди обновления оставило бы том без ядра вовсе.
const KERNEL_A: &CStr16 = cstr16!("\\kernel-a.elf");
const KERNEL_B: &CStr16 = cstr16!("\\kernel-b.elf");

/// Путь к ядру выбранного слота.
///
/// `None` вместо слота означает систему без слотов — живой носитель или
/// установку прежним установщиком.
#[must_use]
pub const fn path_for(slot: Option<Slot>) -> &'static CStr16 {
    match slot {
        Some(Slot::A) => KERNEL_A,
        Some(Slot::B) => KERNEL_B,
        None => KERNEL_PATH,
    }
}

/// Больше этого файл ядра быть не должен: 256 МиБ — заведомо абсурдный размер,
/// и упереться в него можно только при повреждённой файловой системе. Проверка
/// нужна, чтобы не пытаться выделить пул на мусорный `file_size`.
const MAX_KERNEL_SIZE: u64 = 256 * 1024 * 1024;

/// Читает `\kernel.elf` целиком в память, выделенную пулом boot services.
///
/// Буфер возвращается как `Vec<u8>`: он нужен только на время разбора ELF и
/// должен быть освобождён до снятия карты памяти, чтобы не занимать место в
/// карте, которую увидит ядро.
pub fn read(volume: &mut BootVolume, path: &CStr16) -> Result<Vec<u8>, Aborted> {
    let Some(mut file) = volume.open_regular(path)? else {
        println!("  [fs ] {path} not found on the boot volume");
        println!("  [fs ] put the kernel ELF in the ESP root under exactly that name");
        return Err(Aborted);
    };

    let size = volume::size_of(&mut file, path)?;

    if size == 0 {
        println!("  [fs ] {path} is empty");
        return Err(Aborted);
    }
    if size > MAX_KERNEL_SIZE {
        println!("  [fs ] {path} claims {size} bytes, refusing (limit {MAX_KERNEL_SIZE})");
        return Err(Aborted);
    }

    // `size` уже проверен на MAX_KERNEL_SIZE, поэтому usize-каст безопасен даже
    // на 32-битной прошивке (которую мы, впрочем, не поддерживаем).
    let mut buffer = vec![0u8; size as usize];

    // EFI_FILE_PROTOCOL.Read имеет право вернуть меньше запрошенного, поэтому
    // читаем в цикле, а не одним вызовом.
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => {
                println!(
                    "  [fs ] short read: {filled} of {size} bytes before end of file",
                );
                return Err(Aborted);
            }
            Ok(n) => filled += n,
            Err(err) => {
                println!("  [fs ] read error at offset {filled} ({err:?})");
                return Err(Aborted);
            }
        }
    }

    println!("  [fs ] {path}: {size} bytes read into the firmware pool");

    Ok(buffer)
}
