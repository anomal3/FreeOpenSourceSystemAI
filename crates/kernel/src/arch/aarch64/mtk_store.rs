//! Раздел `cache` eMMC телефона — том программ, которые не помещаются в
//! раздел `boot` вместе с ядром и RAM-диском (10.11.2026: браузер OBSOS,
//! 124 МБ). Только чтение: драйвер eMMC (`mtk_msdc`) пишет не умеет, а образ
//! ext2 кладёт `xtask phone-store` и `fastboot flash cache`.
//!
//! Том монтируется корнем в `/opt/obsos`: в нём `bin/obsos`, `share/` и
//! значок — как разложил бы пакет. Программа читается с eMMC по мере
//! надобности (страницы образа), в память целиком не копируется.

use alloc::boxed::Box;
use alloc::sync::Arc;

use super::mtk_msdc as emmc;

/// Куда монтируется том.
pub const MOUNT: &str = "/opt/obsos";

/// Окно раздела eMMC: блоки `first .. first + sectors`, только чтение.
struct Window {
    first: u64,
    sectors: u64,
}

impl disk::BlockDevice for Window {
    fn sector_size(&self) -> u32 {
        emmc::BLOCK as u32
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if buf.len() % emmc::BLOCK != 0 {
            return Err(disk::Error::Unaligned);
        }
        let count = (buf.len() / emmc::BLOCK) as u64;
        if lba.checked_add(count).is_none_or(|end| end > self.sectors) {
            return Err(disk::Error::OutOfRange);
        }
        for (index, chunk) in buf.chunks_exact_mut(emmc::BLOCK).enumerate() {
            let block: &mut [u8; emmc::BLOCK] = chunk.try_into().map_err(|_| disk::Error::Unaligned)?;
            let at = self.first + lba + index as u64;
            emmc::read_block(at as u32, block).map_err(|_| disk::Error::Io)?;
        }
        Ok(())
    }

    fn write(&mut self, _lba: u64, _buf: &[u8]) -> disk::Result<()> {
        Err(disk::Error::ReadOnly)
    }

    fn flush(&mut self) -> disk::Result<()> {
        Ok(())
    }
}

/// Домашний том телефона в памяти (512 МиБ, страницы по требованию).
pub fn mount_home() {
    crate::fs::ramvol::mount("/home", 512, "home", *b"freeos-phonehome");
}

/// Смонтировать раздел `cache`, если на нём ext2. Нет раздела или там не
/// ext2 (стоковый кэш MIUI) — молча без OBSOS: телефон работает как раньше.
pub fn mount() {
    let part = match emmc::find("cache") {
        Ok(part) => part,
        Err(err) => {
            crate::kprintln!("  emmc        : no 'cache' partition ({err:?}), OBSOS not available");
            return;
        }
    };
    let window = Window { first: part.first, sectors: part.blocks() };
    match crate::fs::Ext2Fs::mount(Box::new(window), 0, false) {
        Ok(fs) => {
            crate::fs::mount_volume_at(MOUNT, Arc::new(fs));
            crate::kprintln!("  emmc        : 'cache' ({} MiB) mounted read-only at {MOUNT}", part.blocks() / 2048);
        }
        Err(err) => crate::kprintln!("  emmc        : 'cache' is not an ext2 volume ({err:?}), OBSOS not available"),
    }
}
