//! Образ флешки для сценария `usbstick` (фаза 68n).
//!
//! Как у купленной флешки: таблица MBR, один раздел FAT32 (тип `0x0C`) с
//! сектора 2048. На нём короткий текст, большой файл и ещё один текст **за**
//! большим — его кластеры лежат в десятке мегабайт от начала, и прочитать его
//! значит верно адресовать далёкие сектора, а не только первые.

use std::path::Path;

use anyhow::{Result, anyhow};
use disk::fat32;
use disk::gpt::Range;

/// Содержимое `HELLO.TXT`.
pub const HELLO: &str = "hello from a usb stick";
/// Содержимое `LATE.TXT`.
pub const LATE: &str = "read from far into the stick";
/// Размер `BIG.BIN`.
pub const BIG: usize = 8 * 1024 * 1024;

const SIZE_MIB: u64 = 64;
const FIRST_LBA: u64 = 2048;

/// Собрать образ в `path`.
pub fn build(path: &Path) -> Result<()> {
    let sectors = SIZE_MIB * 2048;
    let mut dev = disk::MemDisk::new(sectors).ok_or_else(|| anyhow!("образ флешки не создаётся"))?;
    let options = fat32::FormatOptions {
        label: "FREEOSUSB",
        volume_id: 0x6868_0068,
        timestamp: fat32::Timestamp::new(2026, 10, 18, 12, 0, 0),
    };
    let range = Range { first_lba: FIRST_LBA, last_lba: sectors - 1 };
    let fail = |err: disk::Error| anyhow!("флешка: {err}");
    let mut volume = fat32::format(&mut dev, range, &options).map_err(fail)?;
    volume.write_file_path(&mut dev, "HELLO.TXT", HELLO.as_bytes()).map_err(fail)?;
    let big: Vec<u8> = (0..BIG).map(|index| (index % 251) as u8).collect();
    volume.write_file_path(&mut dev, "DOCS/BIG.BIN", &big).map_err(fail)?;
    volume.write_file_path(&mut dev, "DOCS/LATE.TXT", LATE.as_bytes()).map_err(fail)?;
    volume.finish(&mut dev).map_err(fail)?;

    let mut bytes = dev.into_vec();
    // Запись MBR: раздел FAT32 с адресацией LBA (тип 0x0C).
    let entry = 446;
    bytes[entry + 4] = 0x0C;
    bytes[entry + 8..entry + 12].copy_from_slice(&(FIRST_LBA as u32).to_le_bytes());
    bytes[entry + 12..entry + 16].copy_from_slice(&((sectors - FIRST_LBA) as u32).to_le_bytes());
    bytes[510] = 0x55;
    bytes[511] = 0xAA;
    std::fs::write(path, bytes).map_err(|err| anyhow!("{}: {err}", path.display()))
}
