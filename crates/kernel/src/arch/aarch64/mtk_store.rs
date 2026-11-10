//! Раздел `cache` eMMC телефона — том программ, которые не помещаются в
//! раздел `boot` вместе с ядром и RAM-диском (10.11.2026: браузер OBSOS,
//! 124 МБ). Только чтение: драйвер eMMC (`mtk_msdc`) пишет не умеет, а образ
//! ext2 кладёт `xtask phone-store` и `fastboot flash cache`.
//!
//! Том монтируется корнем в `/opt/obsos`: в нём `bin/obsos`, `share/` и
//! значок — как разложил бы пакет. Программа читается с eMMC по мере
//! надобности (страницы образа), в память целиком не копируется.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::mm::{FrameAllocator, PhysAddr};

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

/// Сколько секторов в домашнем томе в памяти (512 МиБ). Память берётся только
/// под записанное — см. [`Sparse`].
const HOME_SECTORS: u64 = 512 * 2048;

/// Диск в памяти «по требованию»: кадр (4 КиБ, восемь секторов) выделяется
/// при первой ненулевой записи в него, нули не хранятся. Корень живой системы
/// телефона — RAM-диск FAT32 только на чтение, а программам (OBSOS: профиль,
/// кеш, база Servo) нужно куда-то писать; том живёт до перезагрузки.
struct Sparse {
    sectors: u64,
    pages: BTreeMap<u64, PhysAddr>,
}

const SECTOR: usize = 512;
const PER_PAGE: u64 = 4096 / SECTOR as u64;

impl Sparse {
    fn page(&self, index: u64) -> Option<*mut u8> {
        self.pages.get(&index).map(|frame| frame.to_direct_map().as_mut_ptr::<u8>())
    }
}

impl disk::BlockDevice for Sparse {
    fn sector_size(&self) -> u32 {
        SECTOR as u32
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        if buf.len() % SECTOR != 0 {
            return Err(disk::Error::Unaligned);
        }
        let count = (buf.len() / SECTOR) as u64;
        if lba.checked_add(count).is_none_or(|end| end > self.sectors) {
            return Err(disk::Error::OutOfRange);
        }
        for (index, chunk) in buf.chunks_exact_mut(SECTOR).enumerate() {
            let sector = lba + index as u64;
            match self.page(sector / PER_PAGE) {
                // SAFETY: кадр наш, жив, пока жив диск; смещение внутри кадра.
                Some(base) => unsafe {
                    core::ptr::copy_nonoverlapping(base.add((sector % PER_PAGE) as usize * SECTOR), chunk.as_mut_ptr(), SECTOR);
                },
                None => chunk.fill(0),
            }
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        if buf.len() % SECTOR != 0 {
            return Err(disk::Error::Unaligned);
        }
        let count = (buf.len() / SECTOR) as u64;
        if lba.checked_add(count).is_none_or(|end| end > self.sectors) {
            return Err(disk::Error::OutOfRange);
        }
        for (index, chunk) in buf.chunks_exact(SECTOR).enumerate() {
            let sector = lba + index as u64;
            let page = sector / PER_PAGE;
            let base = match self.page(page) {
                Some(base) => base,
                // Нули в невыделенный кадр — и так нули.
                None if chunk.iter().all(|b| *b == 0) => continue,
                None => {
                    let frame = crate::mm::frame::with(|frames| frames.allocate()).flatten().ok_or(disk::Error::NoSpace)?;
                    self.pages.insert(page, frame);
                    frame.to_direct_map().as_mut_ptr::<u8>()
                }
            };
            // SAFETY: как в `read`.
            unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), base.add((sector % PER_PAGE) as usize * SECTOR), SECTOR) };
        }
        Ok(())
    }

    fn flush(&mut self) -> disk::Result<()> {
        Ok(())
    }
}

/// Домашний том телефона в памяти: ext2 на [`Sparse`], корнем в `/home`.
pub fn mount_home() {
    let mut disk = Sparse { sectors: HOME_SECTORS, pages: BTreeMap::new() };
    let options = ext2::FormatOptions { label: "home", uuid: *b"freeos-phonehome", time: 0 };
    if let Err(err) = ext2::format(&mut disk, 0, HOME_SECTORS, &options) {
        crate::kprintln!("  home        : could not format the RAM home ({err:?})");
        return;
    }
    match crate::fs::Ext2Fs::mount(Box::new(disk), 0, true) {
        Ok(fs) => {
            crate::fs::mount_volume_at("/home", Arc::new(fs));
            crate::kprintln!("  home        : {} MiB RAM volume at /home (pages on demand, lost on reboot)", HOME_SECTORS / 2048);
        }
        Err(err) => crate::kprintln!("  home        : RAM home did not mount ({err:?})"),
    }
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
