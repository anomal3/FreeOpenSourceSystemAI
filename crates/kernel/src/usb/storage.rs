//! Накопители USB: Bulk-Only Transport и команды SCSI (фаза 68n).
//!
//! # Зачем
//!
//! На настоящий ПК систему несут на флешке, и с флешки же берут файлы. Драйвер
//! xHCI поднимает накопитель до двух настроенных bulk-точек; всё остальное —
//! здесь: блок команды (CBW), данные, блок состояния (CSW), поверх них SCSI
//! (`INQUIRY`, `TEST UNIT READY`, `READ CAPACITY`, `READ`/`WRITE`), поверх
//! этого — [`disk::BlockDevice`], а дальше то, что уже было: GPT, FAT32, ext2.
//!
//! # Когда
//!
//! Подключение замечает перечисление портов, а оно идёт, пока контроллеры
//! вынуты из общего списка. Команды же идут тем путём, каким пойдёт и чтение
//! файла, — через список (`xhci::storage_bulk`). Поэтому подъём накопителя
//! откладывается: [`arrived`] его запоминает, [`settle`] — после возврата
//! контроллеров — опрашивает и монтирует. Так же и с выдёргиванием.
//!
//! # Что монтируется
//!
//! Первый том, который удалось открыть: разделы GPT, разделы MBR или
//! «суперфлоппи» — FAT прямо с нулевого сектора, как у многих флешек. FAT32 и
//! ext2 — только на чтение: съёмный носитель выдёргивают без спроса, а запись
//! в FAT ядро и не умеет. exFAT (20.11: внешние диски Windows, SSD Романа с
//! игрой) — на чтение и запись: запись идёт сразу на носитель, том помечен
//! грязным только пока пишут. Точки — `/usb`, `/usb1`, … по порядку.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use disk::BlockDevice as _;

use crate::block::Shared;
use crate::kprintln;
use crate::sync::SpinLock;
use crate::usb::xhci::{self, BulkError, StorageBuffer, StorageHandle};
use crate::vfs::{self, VfsError, VfsResult};

/// Сигнатуры блоков команды и состояния.
const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
/// Сколько раз ждать готовности носителя (`TEST UNIT READY`), с паузой.
const READY_ATTEMPTS: u32 = 20;
const READY_PAUSE_MS: u64 = 100;

/// Точки монтирования по порядку.
const MOUNT_POINTS: [&str; 4] = ["/usb", "/usb1", "/usb2", "/usb3"];

/// Подключённые, но ещё не опрошенные накопители.
static ARRIVED: SpinLock<Vec<StorageHandle>> = SpinLock::new(Vec::new());
/// Выдернутые, чьи тома ещё смонтированы.
static GONE: SpinLock<Vec<StorageHandle>> = SpinLock::new(Vec::new());
/// Кто смонтирован в какой точке.
static MOUNTED: SpinLock<[Option<StorageHandle>; MOUNT_POINTS.len()]> =
    SpinLock::new([None; MOUNT_POINTS.len()]);

/// Накопитель подключён; опросит его [`settle`].
pub fn arrived(handle: StorageHandle) {
    ARRIVED.lock().push(handle);
}

/// Накопитель выдернут; размонтирует его [`settle`].
pub fn gone(handle: StorageHandle) {
    GONE.lock().push(handle);
}

/// Опросить подключённые накопители и смонтировать их тома; размонтировать
/// выдернутые. Зовётся, когда контроллеры xHCI в списке, не под локом.
pub fn settle() {
    loop {
        let next = GONE.lock().pop();
        let Some(handle) = next else { break };
        // Точки выписываются под замком, а снимаются без него: таблица
        // монтирования — под мьютексом, и брать его, держа спин-замок, нельзя.
        let mut points = Vec::new();
        for (point, slot) in MOUNT_POINTS.iter().zip(MOUNTED.lock().iter_mut()) {
            if *slot == Some(handle) {
                *slot = None;
                points.push(*point);
            }
        }
        for point in points {
            crate::fs::unmount(point);
            kprintln!("  usb-storage : {point} unmounted, the device is gone");
        }
    }
    loop {
        let next = ARRIVED.lock().pop();
        let Some(handle) = next else { break };
        bring_up(handle);
    }
}

/// Опросить накопитель и смонтировать первый том, какой откроется.
fn bring_up(handle: StorageHandle) {
    let disk = match UsbDisk::open(handle) {
        Ok(disk) => disk,
        Err(err) => {
            kprintln!("  usb-storage : the device did not come up: {err}");
            return;
        }
    };
    let (sector, sectors) = (disk.sector_size, disk.sectors);
    kprintln!(
        "  usb-storage : '{}', {sectors} sectors of {sector} B ({} MiB)",
        disk.name,
        sectors * u64::from(sector) / (1024 * 1024)
    );
    let mut device = Shared::new(Box::new(disk));

    let Some(point) = MOUNTED.lock().iter().position(Option::is_none) else {
        kprintln!("  usb-storage : no free mount point; the device stays unmounted");
        return;
    };
    let found = volumes(&mut device);
    // Живая флешка несёт журнал в своём разделе (`\FREEOS\LOG.TXT`), и
    // монтировать его незачем: ядро переписывает только сектора файла.
    for (first_lba, _, _) in &found {
        super::stick_log::offer(&device, *first_lba);
    }
    for (first_lba, count, origin) in found {
        if let Some((kind, label, fs, writable)) = open_volume(&device, first_lba, count) {
            let path = MOUNT_POINTS[point];
            crate::fs::mount_volume_at(path, fs);
            MOUNTED.lock()[point] = Some(handle);
            let how = if writable { "read-write" } else { "read-only" };
            kprintln!(
                "  usb-storage : {kind} volume '{label}' ({origin}, LBA {first_lba}) mounted {how} at {path}"
            );
            live_packages(path);
            return;
        }
    }
    kprintln!("  usb-storage : no FAT32, exFAT or ext2 volume on the device");
}

/// Пакеты живой флешки (16.11, Роман: «браузер обсос в образ … на живом
/// проверить»): `\FREEOS\PKG\*.FPK` на носителе ставятся сами в тома
/// `/opt` и `/var` в памяти ([`crate::fs::ramvol`]) — до перезагрузки.
/// Уже стоящий пакет не ставится второй раз (флешку вынули и вставили).
fn live_packages(mount: &str) {
    if !crate::fs::ramvol::live() {
        return;
    }
    let dir = alloc::format!("{mount}/FREEOS/PKG");
    let Some(Ok(entries)) = crate::fs::list(&dir) else {
        return;
    };
    let mut queue: Vec<(usize, String, String)> = Vec::new();
    for entry in entries {
        let name = entry.name.to_ascii_lowercase();
        let Some(stem) = name.strip_suffix(".fpk") else {
            continue;
        };
        if matches!(crate::fs::list(&alloc::format!("/opt/{stem}")), Some(Ok(_))) {
            continue;
        }
        // Зависимые — после того, от чего зависят: `proton` стоит на
        // `debian`, `tmnf` — на `proton` (16.11: Proton с игрой на флешке).
        // По пути `pkg` зависимостей не тянет — порядок держим здесь.
        let rank = LIVE_ORDER.iter().position(|n| *n == stem).unwrap_or(LIVE_ORDER.len());
        queue.push((rank, alloc::format!("{dir}/{}", entry.name), String::from(stem)));
    }
    if queue.is_empty() {
        return;
    }
    queue.sort();
    LIVE_QUEUE.lock().extend(queue.into_iter().map(|(_, path, _)| path));
    // По одному и не в задаче USB: Proton — гигабайт с флешки.
    if !LIVE_RUNNING.swap(true, core::sync::atomic::Ordering::AcqRel) {
        if let Err(err) = crate::sched::spawn_daemon("live-pkg", live_install_task) {
            kprintln!("  live pkg    : could not start the install task: {err:?}");
            LIVE_RUNNING.store(false, core::sync::atomic::Ordering::Release);
        }
    }
}

/// Порядок установки пакетов живой флешки (кого нет в списке — в конце).
/// Приложения Craft — по 8 знаков имени файла 8.3 (`GRIDCRAF.FPK`).
/// 23.11: сначала Debian, Proton и OBS Studio (`obs`, своя glibc), потом
/// игра и остальное — пакеты ставятся строго по одному, память `/opt`
/// растёт только под распакованное.
const LIVE_ORDER: [&str; 10] =
    ["debian", "proton", "obs", "vlc", "tmnf", "obsos", "gridcraf", "wordcraf", "pdfcraft", "mailcraf"];

static LIVE_QUEUE: SpinLock<Vec<String>> = SpinLock::new(Vec::new());
static LIVE_RUNNING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Ставит пакеты очереди по одному, дожидаясь каждого.
fn live_install_task() {
    loop {
        let next = {
            let mut queue = LIVE_QUEUE.lock();
            if queue.is_empty() {
                LIVE_RUNNING.store(false, core::sync::atomic::Ordering::Release);
                return;
            }
            queue.remove(0)
        };
        let line = alloc::format!("/bin/pkg install {next}");
        let started = crate::time::uptime_ms();
        match crate::user::spawn(&line, crate::vfs::perm::Credentials::ROOT) {
            Ok(task) => {
                kprintln!("  live pkg    : installing {next} into the RAM /opt");
                let code = crate::sched::wait(task);
                kprintln!("  live pkg    : {next} done, code {code:?}, {} s", (crate::time::uptime_ms() - started) / 1000);
            }
            Err(err) => kprintln!("  live pkg    : could not start pkg for {next}: {err:?}"),
        }
    }
}

/// Тома носителя: первый сектор, число секторов и откуда они взялись.
fn volumes(device: &mut Shared) -> Vec<(u64, u64, &'static str)> {
    let mut found = Vec::new();
    if let Ok(table) = disk::gpt::read(device) {
        // Разделы — в журнал (20.11: по журналу диска Романа не было видно,
        // прочитана ли GPT и какого типа раздел).
        let sector = device.sector_size() as usize;
        for partition in &table.partitions {
            kprintln!(
                "  usb-storage : GPT part {}: {} MiB at LBA {}, type {}, '{}'",
                partition.index + 1,
                partition.range().bytes(sector) / (1024 * 1024),
                partition.first_lba,
                partition.type_guid,
                partition.name_string(),
            );
        }
        for partition in &table.partitions {
            found.push((partition.first_lba, partition.last_lba - partition.first_lba + 1, "GPT"));
        }
        return found;
    }
    let sector = device.sector_size() as usize;
    let mut first = alloc::vec![0u8; sector];
    if device.read(0, &mut first).is_err() || sector < 512 {
        return found;
    }
    let signed = first[510] == 0x55 && first[511] == 0xAA;
    // Загрузочный сектор FAT прямо на нулевом секторе — «суперфлоппи»: у
    // него та же подпись 55AA, и записи «разделов» там — это код загрузчика.
    let boot_sector = (first[0] == 0xEB || first[0] == 0xE9) && &first[82..87] == b"FAT32";
    if signed && !boot_sector {
        for entry in 0..4 {
            let at = 446 + entry * 16;
            let kind = first[at + 4];
            let start = u64::from(u32::from_le_bytes([first[at + 8], first[at + 9], first[at + 10], first[at + 11]]));
            let count =
                u64::from(u32::from_le_bytes([first[at + 12], first[at + 13], first[at + 14], first[at + 15]]));
            // Пустые записи и защитная запись GPT (0xEE) — не тома.
            if kind != 0 && kind != 0xEE && count != 0 {
                found.push((start, count, "MBR"));
            }
        }
        if !found.is_empty() {
            return found;
        }
    }
    found.push((0, device.sector_count(), "whole device"));
    found
}

/// Открыть том: FAT32, exFAT или ext2. Возвращает формат, метку, файловую
/// систему и открыт ли том на запись.
fn open_volume(
    device: &Shared,
    first_lba: u64,
    count: u64,
) -> Option<(&'static str, String, Arc<dyn vfs::FileSystem>, bool)> {
    if device.sector_size() as usize == vfs::BLOCK_SIZE {
        let window = Window { device: device.clone(), first_lba, blocks: count };
        if let Ok(fat) = crate::fs::Fat32::mount(Box::new(window)) {
            let label = fat.label().unwrap_or_else(|| String::from("<unlabelled>"));
            return Some(("FAT32", label, Arc::new(fat), false));
        }
    }
    // exFAT узнаётся по подписи и сумме загрузочной области; чужой том
    // отказывает, ничего не записав.
    match crate::fs::ExfatFs::mount(device.clone(), first_lba, count, false) {
        Ok(exfat) => {
            let label = exfat.label();
            let label = if label.is_empty() { String::from("<unlabelled>") } else { label };
            if exfat.was_dirty() {
                kprintln!("  usb-storage : exFAT volume '{label}' was not closed cleanly (unplugged while writing?)");
            }
            let writable = !exfat.read_only();
            return Some(("exFAT", label, Arc::new(exfat), writable));
        }
        Err(disk::exfat::Error::NotExfat) => {}
        Err(err) => kprintln!("  usb-storage : exFAT volume at LBA {first_lba} not mounted: {err}"),
    }
    // ext2 опознаёт себя сам: монтирование не своего тома отказывает на
    // суперблоке, ничего не записав (на чтение редактор не открывается).
    crate::fs::Ext2Fs::mount(Box::new(device.clone()), first_lba, false)
        .ok()
        .map(|mount| ("ext2", String::from("-"), Arc::new(mount) as Arc<dyn vfs::FileSystem>, false))
}

/// Том как `vfs::BlockDevice`: окно носителя с сектора `first_lba`.
struct Window {
    device: Shared,
    first_lba: u64,
    blocks: u64,
}

impl vfs::BlockDevice for Window {
    fn read_blocks(&self, block: u64, buf: &mut [u8]) -> VfsResult<()> {
        if block + (buf.len() / vfs::BLOCK_SIZE) as u64 > self.blocks {
            return Err(VfsError::Io);
        }
        let mut device = self.device.clone();
        device.read(self.first_lba + block, buf).map_err(|_| VfsError::Io)
    }

    fn write_blocks(&self, _block: u64, _buf: &[u8]) -> VfsResult<()> {
        Err(VfsError::Unsupported)
    }

    fn block_count(&self) -> u64 {
        self.blocks
    }
}

/// Почему команда не прошла.
#[derive(Clone, Copy, Debug)]
enum Error {
    /// Отказ передачи.
    Bulk(BulkError),
    /// Блок состояния не тот: подпись, метка или длина.
    BadStatus,
    /// Команда отвергнута (CSW 1); смысл — в `REQUEST SENSE`.
    Check { key: u8, asc: u8 },
    /// Ошибка фазы (CSW 2) — после неё накопитель сброшен.
    Phase,
    /// Носитель так и не стал готов.
    NotReady,
    /// Ёмкость пришла бессмысленной.
    BadCapacity,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Bulk(err) => write!(f, "bulk transfer failed: {err:?}"),
            Self::BadStatus => f.write_str("the status block is malformed"),
            Self::Check { key, asc } => write!(f, "the command was refused, sense key {key:#x}, ASC {asc:#04x}"),
            Self::Phase => f.write_str("phase error; the device was reset"),
            Self::NotReady => f.write_str("the medium never became ready"),
            Self::BadCapacity => f.write_str("the reported capacity makes no sense"),
        }
    }
}

impl From<BulkError> for Error {
    fn from(err: BulkError) -> Self {
        Self::Bulk(err)
    }
}

/// Накопитель как блочное устройство.
struct UsbDisk {
    handle: StorageHandle,
    sector_size: u32,
    sectors: u64,
    /// Сколько байт данных помещается в одну команду.
    window: usize,
    tag: u32,
    name: String,
}

impl UsbDisk {
    /// Опросить: `INQUIRY`, готовность, ёмкость.
    fn open(handle: StorageHandle) -> Result<Self, Error> {
        let window = xhci::storage_window(handle).ok_or(Error::Bulk(BulkError::Gone))?;
        let mut disk = Self { handle, sector_size: 512, sectors: 0, window, tag: 1, name: String::new() };

        let mut inquiry = [0u8; 36];
        disk.command(&[0x12, 0, 0, 0, 36, 0], Direction::In(&mut inquiry))?;
        let text = |bytes: &[u8]| String::from(core::str::from_utf8(bytes).unwrap_or("?").trim());
        disk.name = alloc::format!("{} {}", text(&inquiry[8..16]), text(&inquiry[16..32]));

        // Готовность: после подключения носитель отвечает «внимание, сменился
        // носитель» (UNIT ATTENTION), а большой — ещё и «раскручиваюсь».
        let mut ready = false;
        for _ in 0..READY_ATTEMPTS {
            match disk.command(&[0; 6], Direction::None) {
                Ok(()) => {
                    ready = true;
                    break;
                }
                Err(Error::Check { .. }) => crate::usb::sleep_ms(READY_PAUSE_MS),
                Err(err) => return Err(err),
            }
        }
        if !ready {
            return Err(Error::NotReady);
        }

        let mut capacity = [0u8; 8];
        disk.command(&[0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0], Direction::In(&mut capacity))?;
        let last = u32::from_be_bytes([capacity[0], capacity[1], capacity[2], capacity[3]]);
        let mut size = u32::from_be_bytes([capacity[4], capacity[5], capacity[6], capacity[7]]);
        let mut sectors = u64::from(last) + 1;
        if last == u32::MAX {
            // Больше 2 ТиБ — READ CAPACITY(16).
            let mut long = [0u8; 32];
            let mut cb = [0u8; 16];
            cb[0] = 0x9E;
            cb[1] = 0x10;
            cb[13] = 32;
            disk.command(&cb, Direction::In(&mut long))?;
            sectors = u64::from_be_bytes([long[0], long[1], long[2], long[3], long[4], long[5], long[6], long[7]]) + 1;
            size = u32::from_be_bytes([long[8], long[9], long[10], long[11]]);
        }
        if !disk::sector_size_supported(size) || size as usize > window || sectors == 0 {
            return Err(Error::BadCapacity);
        }
        disk.sector_size = size;
        disk.sectors = sectors;
        Ok(disk)
    }

    /// Одна команда SCSI: блок команды, данные, блок состояния.
    fn command(&mut self, cb: &[u8], data: Direction<'_>) -> Result<(), Error> {
        let result = self.transport(cb, data);
        if let Err(Error::Check { .. }) = result {
            // Причина отказа — следующей командой, иначе накопитель ответит ею
            // же на любую следующую.
            let mut sense = [0u8; 18];
            if self.transport(&[0x03, 0, 0, 0, 18, 0], Direction::In(&mut sense)).is_ok() {
                return Err(Error::Check { key: sense[2] & 0x0F, asc: sense[12] });
            }
        }
        result
    }

    fn transport(&mut self, cb: &[u8], mut data: Direction<'_>) -> Result<(), Error> {
        let handle = self.handle;
        self.tag = self.tag.wrapping_add(1);
        let tag = self.tag;
        let (length, dir_in) = match &data {
            Direction::None => (0, false),
            Direction::In(buf) => (buf.len(), true),
            Direction::Out(buf) => (buf.len(), false),
        };
        xhci::with_storage_buffers(handle, |command, _, _| {
            command.fill(0);
            command[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
            command[4..8].copy_from_slice(&tag.to_le_bytes());
            command[8..12].copy_from_slice(&(length as u32).to_le_bytes());
            command[12] = if dir_in { 0x80 } else { 0 };
            command[14] = cb.len() as u8;
            command[15..15 + cb.len()].copy_from_slice(cb);
        })
        .ok_or(Error::Bulk(BulkError::Gone))?;
        if let Direction::Out(buf) = &data {
            window_copy(handle, buf.len(), |window, range| window[range.clone()].copy_from_slice(&buf[range]))?;
        }

        if let Err(err) = xhci::storage_bulk(handle, false, StorageBuffer::Command, 31) {
            xhci::storage_reset(handle);
            return Err(err.into());
        }
        if length > 0 {
            // Остановка в фазе данных законна: накопитель отдал меньше, чем
            // просили, или отказал; точка уже сброшена, а итог скажет CSW.
            match xhci::storage_bulk(handle, dir_in, StorageBuffer::Data, length) {
                Ok(_) | Err(BulkError::Stalled) => {}
                // hw-log18: a transaction error halts the xHCI endpoint; without
                // Reset Recovery the next command waited on a halted ring.
                Err(err) => {
                    if matches!(err, BulkError::Failed(_) | BulkError::Timeout) {
                        xhci::storage_reset(handle);
                    }
                    return Err(err.into());
                }
            }
        }
        let status = match xhci::storage_bulk(handle, true, StorageBuffer::Status, 13) {
            Ok(read) => read,
            // Остановка вместо CSW — попытка ещё одна, после сброса точки.
            Err(BulkError::Stalled) => xhci::storage_bulk(handle, true, StorageBuffer::Status, 13)?,
            Err(err) => {
                if matches!(err, BulkError::Failed(_) | BulkError::Timeout) {
                    xhci::storage_reset(handle);
                }
                return Err(err.into());
            }
        };
        if let Direction::In(buf) = &mut data {
            let len = buf.len();
            window_copy(handle, len, |window, range| buf[range.clone()].copy_from_slice(&window[range]))?;
        }
        let (signature, echoed, code) = xhci::with_storage_buffers(handle, |_, csw, _| {
            (
                u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]),
                u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]),
                csw[12],
            )
        })
        .ok_or(Error::Bulk(BulkError::Gone))?;
        if status < 13 || signature != CSW_SIGNATURE || echoed != tag {
            xhci::storage_reset(handle);
            return Err(Error::BadStatus);
        }
        match code {
            0 => Ok(()),
            1 => Err(Error::Check { key: 0, asc: 0 }),
            _ => {
                xhci::storage_reset(handle);
                Err(Error::Phase)
            }
        }
    }

    /// `READ` или `WRITE` пачки секторов, умещающейся в окно.
    fn rw(&mut self, lba: u64, count: u32, data: Direction<'_>) -> Result<(), Error> {
        let write = matches!(data, Direction::Out(_));
        if lba + u64::from(count) <= u64::from(u32::MAX) {
            let lba = (lba as u32).to_be_bytes();
            let count = (count as u16).to_be_bytes();
            let op = if write { 0x2A } else { 0x28 };
            self.command(&[op, 0, lba[0], lba[1], lba[2], lba[3], 0, count[0], count[1], 0], data)
        } else {
            let mut cb = [0u8; 16];
            cb[0] = if write { 0x8A } else { 0x88 };
            cb[2..10].copy_from_slice(&lba.to_be_bytes());
            cb[10..14].copy_from_slice(&count.to_be_bytes());
            self.command(&cb, data)
        }
    }

    /// Сколько секторов в одной команде.
    fn batch(&self) -> usize {
        let cap = WINDOW_CAP.load(core::sync::atomic::Ordering::Relaxed);
        let window = if cap == 0 { self.window } else { self.window.min(cap) };
        (window / self.sector_size as usize).max(1)
    }
}

/// `usbwin KIB` (hw-log17): the longest USB disk command in bytes, 0 = the
/// whole data window of the device (1 MiB on SuperSpeed, else 128 KiB).
/// `usbwin 128` gives the commands of before 24.11.
pub static WINDOW_CAP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// `usbslow FIXED_US MBPS` (tests, hw-log17): a USB disk read command lasts
/// at least FIXED_US plus its bytes at MBPS (MB/s) - QEMU's disk answers at
/// once, a real one does not (the Netac SSD of hw-log17: ~0.15 ms per
/// command and ~250 MB/s). The device stays held meanwhile, as a real busy
/// disk. 0 0 = off (default).
pub static SLOW_US: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static SLOW_MBPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn pace(started: u64, bytes: usize) {
    use core::sync::atomic::Ordering::Relaxed;
    let (fixed, rate) = (SLOW_US.load(Relaxed), SLOW_MBPS.load(Relaxed));
    if fixed == 0 && rate == 0 {
        return;
    }
    let transfer = if rate == 0 { 0 } else { bytes as u64 * 1000 / rate };
    let deadline = started + fixed * 1000 + transfer;
    // A spin, not a sleep: QEMU's timers on a Windows host fire ~15 ms
    // late, which would make every command that long.
    while crate::time::uptime_ns() < deadline {
        core::hint::spin_loop();
    }
}

/// hw-log17: the data window is reached under the controllers' spin lock
/// (interrupts off); a 1 MiB command is copied in 128 KiB pieces, each under
/// its own short hold.
const COPY_PIECE: usize = 128 * 1024;

fn window_copy(
    handle: StorageHandle,
    len: usize,
    mut f: impl FnMut(&mut [u8], core::ops::Range<usize>),
) -> Result<(), Error> {
    let mut at = 0;
    while at < len {
        let end = len.min(at + COPY_PIECE);
        xhci::with_storage_buffers(handle, |_, _, window| f(window, at..end)).ok_or(Error::Bulk(BulkError::Gone))?;
        at = end;
    }
    Ok(())
}

/// Направление фазы данных.
enum Direction<'a> {
    None,
    In(&'a mut [u8]),
    Out(&'a [u8]),
}

impl disk::BlockDevice for UsbDisk {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        let sector = self.sector_size as usize;
        if buf.len() % sector != 0 {
            return Err(disk::Error::Unaligned);
        }
        if lba + (buf.len() / sector) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        crate::sched::charge_disk(buf.len());
        let batch = self.batch();
        for (index, chunk) in buf.chunks_mut(batch * sector).enumerate() {
            let at = lba + (index * batch) as u64;
            let count = (chunk.len() / sector) as u32;
            let started = crate::time::uptime_ns();
            let bytes = chunk.len();
            self.rw(at, count, Direction::In(chunk)).map_err(|err| {
                kprintln!("  usb-storage : reading LBA {at}: {err}");
                disk::Error::Io
            })?;
            pace(started, bytes);
        }
        Ok(())
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        let sector = self.sector_size as usize;
        if buf.len() % sector != 0 {
            return Err(disk::Error::Unaligned);
        }
        if lba + (buf.len() / sector) as u64 > self.sectors {
            return Err(disk::Error::OutOfRange);
        }
        crate::sched::charge_disk(buf.len());
        let batch = self.batch();
        for (index, chunk) in buf.chunks(batch * sector).enumerate() {
            let at = lba + (index * batch) as u64;
            let count = (chunk.len() / sector) as u32;
            self.rw(at, count, Direction::Out(chunk)).map_err(|err| {
                kprintln!("  usb-storage : writing LBA {at}: {err}");
                disk::Error::Io
            })?;
        }
        Ok(())
    }

    fn flush(&mut self) -> disk::Result<()> {
        // SYNCHRONIZE CACHE(10): кеш записи у флешек есть, хоть и не у всех.
        let _ = self.command(&[0x35, 0, 0, 0, 0, 0, 0, 0, 0, 0], Direction::None);
        Ok(())
    }
}
