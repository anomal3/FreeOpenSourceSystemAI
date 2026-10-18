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
//! в FAT ядро и не умеет. Точки — `/usb`, `/usb1`, … по порядку.

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
    for (first_lba, count, origin) in volumes(&mut device) {
        if let Some((kind, label, fs)) = open_volume(&device, first_lba, count) {
            let path = MOUNT_POINTS[point];
            crate::fs::mount_volume_at(path, fs);
            MOUNTED.lock()[point] = Some(handle);
            kprintln!(
                "  usb-storage : {kind} volume '{label}' ({origin}, LBA {first_lba}) mounted read-only at {path}"
            );
            return;
        }
    }
    kprintln!("  usb-storage : no FAT32 or ext2 volume on the device");
}

/// Тома носителя: первый сектор, число секторов и откуда они взялись.
fn volumes(device: &mut Shared) -> Vec<(u64, u64, &'static str)> {
    let mut found = Vec::new();
    if let Ok(table) = disk::gpt::read(device) {
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

/// Открыть том: FAT32 или ext2. Возвращает формат, метку и файловую систему.
fn open_volume(
    device: &Shared,
    first_lba: u64,
    count: u64,
) -> Option<(&'static str, String, Arc<dyn vfs::FileSystem>)> {
    if device.sector_size() as usize == vfs::BLOCK_SIZE {
        let window = Window { device: device.clone(), first_lba, blocks: count };
        if let Ok(fat) = crate::fs::Fat32::mount(Box::new(window)) {
            let label = fat.label().unwrap_or_else(|| String::from("<unlabelled>"));
            return Some(("FAT32", label, Arc::new(fat)));
        }
    }
    // ext2 опознаёт себя сам: монтирование не своего тома отказывает на
    // суперблоке, ничего не записав (на чтение редактор не открывается).
    crate::fs::Ext2Fs::mount(Box::new(device.clone()), first_lba, false)
        .ok()
        .map(|mount| ("ext2", String::from("-"), Arc::new(mount) as Arc<dyn vfs::FileSystem>))
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
        xhci::with_storage_buffers(handle, |command, _, window| {
            command.fill(0);
            command[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
            command[4..8].copy_from_slice(&tag.to_le_bytes());
            command[8..12].copy_from_slice(&(length as u32).to_le_bytes());
            command[12] = if dir_in { 0x80 } else { 0 };
            command[14] = cb.len() as u8;
            command[15..15 + cb.len()].copy_from_slice(cb);
            if let Direction::Out(buf) = &data {
                window[..buf.len()].copy_from_slice(buf);
            }
        })
        .ok_or(Error::Bulk(BulkError::Gone))?;

        if let Err(err) = xhci::storage_bulk(handle, false, StorageBuffer::Command, 31) {
            xhci::storage_reset(handle);
            return Err(err.into());
        }
        if length > 0 {
            // Остановка в фазе данных законна: накопитель отдал меньше, чем
            // просили, или отказал; точка уже сброшена, а итог скажет CSW.
            match xhci::storage_bulk(handle, dir_in, StorageBuffer::Data, length) {
                Ok(_) | Err(BulkError::Stalled) => {}
                Err(err) => return Err(err.into()),
            }
        }
        let status = match xhci::storage_bulk(handle, true, StorageBuffer::Status, 13) {
            Ok(read) => read,
            // Остановка вместо CSW — попытка ещё одна, после сброса точки.
            Err(BulkError::Stalled) => xhci::storage_bulk(handle, true, StorageBuffer::Status, 13)?,
            Err(err) => return Err(err.into()),
        };
        let (signature, echoed, code) = xhci::with_storage_buffers(handle, |_, csw, window| {
            if let Direction::In(buf) = &mut data {
                let len = buf.len();
                buf.copy_from_slice(&window[..len]);
            }
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
        (self.window / self.sector_size as usize).max(1)
    }
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
        let batch = self.batch();
        for (index, chunk) in buf.chunks_mut(batch * sector).enumerate() {
            let at = lba + (index * batch) as u64;
            let count = (chunk.len() / sector) as u32;
            self.rw(at, count, Direction::In(chunk)).map_err(|err| {
                kprintln!("  usb-storage : reading LBA {at}: {err}");
                disk::Error::Io
            })?;
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
