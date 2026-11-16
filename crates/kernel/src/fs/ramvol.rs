//! Том в памяти «по требованию» — ext2 на [`Sparse`].
//!
//! Корень живой системы — RAM-диск FAT32 только на чтение. Программам нужно
//! куда-то писать: на телефоне — `/home` (профиль OBSOS, кеш, база Servo), на
//! живой флешке ПК (16.11) — `/opt` и `/var`, куда `pkg` ставит пакеты.
//! Том живёт до перезагрузки.

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::sync::Arc;

use crate::mm::{FrameAllocator, PhysAddr};

/// Диск в памяти «по требованию»: кадр (4 КиБ, восемь секторов) выделяется
/// при первой ненулевой записи в него, нули не хранятся. Корень живой системы
/// телефона — RAM-диск FAT32 только на чтение, а программам (OBSOS: профиль,
/// кеш, база Servo) нужно куда-то писать; том живёт до перезагрузки.
///
/// Где лежит каждая страница — в двухуровневой таблице, как таблицы страниц:
/// в куче только верхний уровень (8 байт на 2 МиБ тома), а адреса кадров
/// данных — в кадрах-листах по 512. 16.11: прежняя `BTreeMap` в куче ядра
/// на Proton (гигабайты в `/opt` живой флешки) съела 48 МиБ кучи — паника
/// «memory allocation failed» посреди первого запуска Wine.
pub struct Sparse {
    sectors: u64,
    /// Кадры-листы: по 512 адресов кадров данных (0 — страницы нет).
    leaves: Vec<Option<PhysAddr>>,
}

const SECTOR: usize = 512;
const PER_PAGE: u64 = 4096 / SECTOR as u64;
const PER_LEAF: u64 = 512;

impl Sparse {
    fn new(sectors: u64) -> Self {
        let pages = sectors.div_ceil(PER_PAGE);
        let mut leaves = Vec::new();
        leaves.resize(pages.div_ceil(PER_LEAF) as usize, None);
        Self { sectors, leaves }
    }

    fn slot(&self, index: u64) -> Option<*mut u64> {
        let leaf = (*self.leaves.get((index / PER_LEAF) as usize)?)?;
        // SAFETY: лист — наш кадр, 512 слов; номер внутри листа меньше 512.
        Some(unsafe { leaf.to_direct_map().as_mut_ptr::<u64>().add((index % PER_LEAF) as usize) })
    }

    fn page(&self, index: u64) -> Option<*mut u8> {
        let slot = self.slot(index)?;
        // SAFETY: см. `slot`.
        let frame = unsafe { slot.read() };
        (frame != 0).then(|| PhysAddr::new(frame).to_direct_map().as_mut_ptr::<u8>())
    }

    /// Запомнить кадр страницы `index` (лист заводится по нужде).
    fn insert(&mut self, index: u64, frame: PhysAddr) -> disk::Result<()> {
        let at = (index / PER_LEAF) as usize;
        if self.leaves[at].is_none() {
            let leaf = crate::mm::frame::with(|frames| frames.allocate()).flatten().ok_or(disk::Error::NoSpace)?;
            // SAFETY: свежий кадр, наш целиком.
            unsafe { core::ptr::write_bytes(leaf.to_direct_map().as_mut_ptr::<u8>(), 0, 4096) };
            self.leaves[at] = Some(leaf);
        }
        let slot = self.slot(index).ok_or(disk::Error::OutOfRange)?;
        // SAFETY: см. `slot`.
        unsafe { slot.write(frame.as_u64()) };
        Ok(())
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
                    // Остальные семь секторов страницы обязаны читаться
                    // нулями, как до записи, — кадр от пула не обнулён.
                    // SAFETY: свежий кадр, наш целиком.
                    unsafe { core::ptr::write_bytes(frame.to_direct_map().as_mut_ptr::<u8>(), 0, 4096) };
                    self.insert(page, frame)?;
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
    let mut disk = Sparse::new(sectors);
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
    // Размеры — потолки, память берётся только под записанное: Proton с
    // Mesa и корнем Debian раскладывается на несколько гигабайт, префикс
    // Wine с игрой — ещё на пару (16.11: Proton и Trackmania на флешке).
    let opt = mount("/opt", 16 * 1024, "live-opt", *b"fossai-live-opt0");
    let var = mount("/var", 2048, "live-var", *b"fossai-live-var0");
    // `/tmp` программ Linux — это `/var/tmp` (`linux_path`): wineserver
    // держит там сокет.
    if var {
        let _ = super::mkdir_as(crate::vfs::perm::Credentials::ROOT, "/var/tmp", 0o1777);
    }
    // Домашний каталог `root` (под ним живая система и работает): OBSOS
    // держит там профиль (`~/.config/obsos`) и без записи падает в Servo.
    // Настройки стола — в `/etc`, раскладка мониторов — на флешке, их это
    // не касается.
    mount("/root", 16 * 1024, "live-root", *b"fossai-live-root");
    LIVE.store(opt && var, core::sync::atomic::Ordering::Release);
}

/// Есть ли у живой системы `/opt` и `/var` для пакетов.
pub fn live() -> bool {
    LIVE.load(core::sync::atomic::Ordering::Acquire)
}
