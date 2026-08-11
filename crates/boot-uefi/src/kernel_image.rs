//! Чтение образа ядра с системного раздела (ESP).
//!
//! Ядро лежит отдельным файлом в корне того же тома, с которого прошивка
//! запустила сам загрузчик. Это осознанно: том находится по loaded image, а не
//! перебором всех носителей, поэтому на машине с несколькими ESP гарантированно
//! возьмётся ядро «из того же комплекта», что и загрузчик.

use alloc::vec;
use alloc::vec::Vec;

use uefi::proto::media::file::{File, FileAttribute, FileInfo, FileMode, FileType};
use uefi::{boot, cstr16, println};

use crate::Aborted;

/// Путь к ядру относительно корня тома. Ведущий `\` делает путь абсолютным
/// внутри тома — то же самое, что открыть `kernel.elf` в корневом каталоге,
/// но однозначно читается.
const KERNEL_PATH: &uefi::CStr16 = cstr16!("\\kernel.elf");

/// Больше этого файл ядра быть не должен: 256 МиБ — заведомо абсурдный размер,
/// и упереться в него можно только при повреждённой файловой системе. Проверка
/// нужна, чтобы не пытаться выделить пул на мусорный `file_size`.
const MAX_KERNEL_SIZE: u64 = 256 * 1024 * 1024;

/// Читает `\kernel.elf` целиком в память, выделенную пулом boot services.
///
/// Буфер возвращается как `Vec<u8>`: он нужен только на время разбора ELF и
/// должен быть освобождён до снятия карты памяти, чтобы не занимать место в
/// карте, которую увидит ядро.
pub fn read() -> Result<Vec<u8>, Aborted> {
    let image_handle = boot::image_handle();

    let mut fs = match boot::get_image_file_system(image_handle) {
        Ok(fs) => fs,
        Err(err) => {
            println!("  [fs ] cannot reach the volume this loader was started from ({err:?})");
            println!("  [fs ] the firmware exposes no SimpleFileSystem on the boot device");
            return Err(Aborted);
        }
    };

    let mut root = match fs.open_volume() {
        Ok(root) => root,
        Err(err) => {
            println!("  [fs ] cannot open the volume root ({err:?})");
            return Err(Aborted);
        }
    };

    let handle = match root.open(KERNEL_PATH, FileMode::Read, FileAttribute::empty()) {
        Ok(handle) => handle,
        Err(err) => {
            println!("  [fs ] cannot open {KERNEL_PATH} ({err:?})");
            println!("  [fs ] put the kernel ELF in the ESP root under exactly that name");
            return Err(Aborted);
        }
    };

    let mut file = match handle.into_type() {
        Ok(FileType::Regular(file)) => file,
        Ok(FileType::Dir(_)) => {
            println!("  [fs ] {KERNEL_PATH} is a directory, not a file");
            return Err(Aborted);
        }
        Err(err) => {
            println!("  [fs ] cannot classify {KERNEL_PATH} ({err:?})");
            return Err(Aborted);
        }
    };

    let size = match file.get_boxed_info::<FileInfo>() {
        Ok(info) => info.file_size(),
        Err(err) => {
            println!("  [fs ] cannot stat {KERNEL_PATH} ({err:?})");
            return Err(Aborted);
        }
    };

    if size == 0 {
        println!("  [fs ] {KERNEL_PATH} is empty");
        return Err(Aborted);
    }
    if size > MAX_KERNEL_SIZE {
        println!("  [fs ] {KERNEL_PATH} claims {size} bytes, refusing (limit {MAX_KERNEL_SIZE})");
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

    println!("  [fs ] {KERNEL_PATH}: {size} bytes read into the firmware pool");

    Ok(buffer)
}
