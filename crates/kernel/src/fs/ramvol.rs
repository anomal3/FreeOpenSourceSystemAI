//! Том в памяти «по требованию» — ext2 на [`Sparse`].
//!
//! Корень живой системы — RAM-диск FAT32 только на чтение. Программам нужно
//! куда-то писать: на телефоне — `/home` (профиль OBSOS, кеш, база Servo), на
//! живой флешке ПК (16.11) — `/opt` и `/var`, куда `pkg` ставит пакеты.
//! Том живёт до перезагрузки.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use crate::mm::{FrameAllocator, PhysAddr};

/// Диск в памяти «по требованию»: кадр (4 КиБ, восемь секторов) выделяется
/// при первой ненулевой записи в него, нули не хранятся. Корень живой системы
/// телефона — RAM-диск FAT32 только на чтение, а программам (OBSOS: профиль,
/// кеш, база Servo) нужно куда-то писать; том живёт до перезагрузки.
pub struct Sparse {
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

/// Отформатировать ext2 размером `mib` МиБ в памяти и смонтировать в `path`.
/// Память берётся только под записанное.
pub fn mount(path: &'static str, mib: u64, label: &str, uuid: [u8; 16]) -> bool {
    let sectors = mib * 2048;
    let mut disk = Sparse { sectors, pages: BTreeMap::new() };
    let options = ext2::FormatOptions { label, uuid, time: 0 };
    if let Err(err) = ext2::format(&mut disk, 0, sectors, &options) {
        crate::kprintln!("  ramvol      : could not format the RAM volume for {path} ({err:?})");
        return false;
    }
    match super::Ext2Fs::mount(Box::new(disk), 0, true) {
        Ok(fs) => {
            super::mount_volume_at(path, Arc::new(fs));
            crate::kprintln!("  ramvol      : {mib} MiB RAM volume at {path} (pages on demand, lost on reboot)");
            true
        }
        Err(err) => {
            crate::kprintln!("  ramvol      : RAM volume for {path} did not mount ({err:?})");
            false
        }
    }
}

/// Система загружена с живой флешки и получила тома в памяти.
static LIVE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Живая флешка (16.11, Роман: «браузер обсос … на живом проверить»): корень
/// только на чтение, раздела состояния нет — `/opt` и `/var` получают тома в
/// памяти, и `pkg install` работает до перезагрузки; `/root` — тоже, под
/// профили программ. `/etc` и `/home` не трогаются: в образе RAM-диска там
/// лежат настройки и учётные записи.
pub fn mount_live_branches() {
    let opt = mount("/opt", 4096, "live-opt", *b"fossai-live-opt0");
    let var = mount("/var", 256, "live-var", *b"fossai-live-var0");
    // Домашний каталог `root` (под ним живая система и работает): OBSOS
    // держит там профиль (`~/.config/obsos`) и без записи падает в Servo.
    // Настройки стола — в `/etc`, раскладка мониторов — на флешке, их это
    // не касается.
    mount("/root", 1024, "live-root", *b"fossai-live-root");
    LIVE.store(opt && var, core::sync::atomic::Ordering::Release);
}

/// Есть ли у живой системы `/opt` и `/var` для пакетов.
pub fn live() -> bool {
    LIVE.load(core::sync::atomic::Ordering::Acquire)
}
