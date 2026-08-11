//! Носители через `EFI_BLOCK_IO_PROTOCOL` и мост к крейту [`disk`].
//!
//! # Что показывать человеку, а что прятать
//!
//! Прошивка выдаёт `BlockIO` и на диски, и на каждый их раздел. Раздел в
//! списке «куда установить» бессмыслен — разметку мы создаём сами, — поэтому
//! всё, у чего выставлен `logical_partition`, отсеивается сразу.
//!
//! Отдельно отсеивать нечего, а вот **пометить** есть что: носитель, с
//! которого запущен сам установщик. Стереть его посреди установки — это не
//! теоретическая ошибка, а самый вероятный способ испортить прогон:
//! установочная флешка и целевой диск на экране выглядят одинаково. Носитель
//! находится по пути устройства загруженного образа и в списке помечается как
//! источник установки; выбрать его нельзя.
//!
//! # Почему перечисление открывает протоколы не эксклюзивно
//!
//! Эксклюзивное открытие заставляет прошивку отключить от носителя свои
//! драйверы. Для целевого диска это ровно то, что нужно (см. [`UefiDisk::open`]),
//! а вот при перечислении — прямой вред: установщик отключил бы файловую
//! систему у носителя, с которого сам же запущен и с которого ему ещё читать
//! ядро. Поэтому сведения собираются через `GetProtocol`, который не делает
//! вызывающего потребителем устройства.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use disk::{BlockDevice, SECTOR_SIZE};
use uefi::boot::{self, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol};
use uefi::proto::device_path::DevicePath;
use uefi::proto::loaded_image::LoadedImage;
use uefi::proto::media::block::BlockIO;
use uefi::{Handle, Status};

use crate::logln;

/// Носитель, пригодный для установки.
///
/// `Clone` нужен ровно в одном месте: перед установкой описание выбранного
/// диска вынимается из состояния приложения, иначе состояние оказалось бы
/// заимствовано и на чтение (диск), и на запись (ход работ) одновременно.
/// Копируются только числа — сам протокол открывается заново.
#[derive(Clone)]
pub struct Disk {
    pub handle: Handle,
    pub media_id: u32,
    pub block_size: u32,
    pub sectors: u64,
    pub removable: bool,
    pub read_only: bool,
    /// Требование прошивки к выравниванию буферов передачи.
    pub io_align: u32,
    /// С этого носителя запущен сам установщик.
    pub is_install_media: bool,
    /// Как носитель подключён: «SATA», «NVMe», «USB», …
    pub bus: &'static str,
}

impl Disk {
    /// Объём носителя в байтах.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.sectors * self.block_size as u64
    }

    /// Читаемый размер.
    #[must_use]
    pub fn size_text(&self) -> String {
        const GIB: u64 = 1024 * 1024 * 1024;
        const MIB: u64 = 1024 * 1024;
        let bytes = self.bytes();
        if bytes >= GIB {
            // Знак после запятой обязателен: «1 GiB» вместо «1.4 GiB» скрывает
            // разницу между двумя носителями, которые надо различить.
            format!("{}.{} GiB", bytes / GIB, (bytes % GIB) * 10 / GIB)
        } else if bytes >= MIB {
            format!("{} MiB", bytes / MIB)
        } else {
            format!("{bytes} B")
        }
    }

    /// Можно ли на него устанавливать.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        !self.read_only && !self.is_install_media && self.block_size as usize == SECTOR_SIZE
    }
}

/// Перечислить носители, пригодные для показа в списке.
#[must_use]
pub fn enumerate() -> Vec<Disk> {
    let boot_path = boot_device_path();
    let mut disks = Vec::new();

    let Ok(handles) = boot::find_handles::<BlockIO>() else {
        logln!("[disk] the firmware exposes no BlockIO handles at all");
        return disks;
    };

    for handle in handles {
        let Some(io) = peek::<BlockIO>(handle) else {
            continue;
        };
        let media = io.media();
        if media.is_logical_partition() || !media.is_media_present() {
            continue;
        }
        let sectors = media.last_block().saturating_add(1);
        if sectors == 0 || media.block_size() == 0 {
            continue;
        }

        let path = device_path_bytes(handle);
        let is_install_media = match (&boot_path, &path) {
            (Some(boot), Some(disk)) => is_prefix_of(disk, boot),
            _ => false,
        };

        disks.push(Disk {
            handle,
            media_id: media.media_id(),
            block_size: media.block_size(),
            sectors,
            removable: media.is_removable_media(),
            read_only: media.is_read_only(),
            io_align: media.io_align(),
            is_install_media,
            bus: bus_name(path.as_deref()),
        });
    }

    // Порядок стабильный и осмысленный: сначала то, на что можно ставить.
    // Прошивка возвращает хендлы в порядке своей внутренней базы данных, и
    // установочный носитель вполне может оказаться первым в списке — то есть
    // под курсором по умолчанию.
    disks.sort_by_key(|disk| (disk.is_install_media, disk.read_only, disk.bytes()));

    for disk in &disks {
        logln!(
            "[disk] {} {} sectors of {} bytes, io_align {}{}{}",
            disk.bus,
            disk.sectors,
            disk.block_size,
            disk.io_align,
            if disk.read_only { ", read-only" } else { "" },
            if disk.is_install_media { ", install media" } else { "" },
        );
    }

    disks
}

/// Открыть протокол только ради чтения его полей.
fn peek<P: uefi::proto::ProtocolPointer + ?Sized>(handle: Handle) -> Option<ScopedProtocol<P>> {
    let params = OpenProtocolParams {
        handle,
        agent: boot::image_handle(),
        controller: None,
    };
    // SAFETY: `GetProtocol` не регистрирует установщик потребителем устройства
    // и потому ничего не отключает; полученный указатель используется только
    // на чтение и не переживает возвращённый `ScopedProtocol`.
    unsafe { boot::open_protocol::<P>(params, OpenProtocolAttributes::GetProtocol) }.ok()
}

/// Путь устройства, с которого запущен установщик.
fn boot_device_path() -> Option<Vec<u8>> {
    let image = peek::<LoadedImage>(boot::image_handle())?;
    let device = image.device()?;
    device_path_bytes(device)
}

/// Байты пути устройства для хендла.
fn device_path_bytes(handle: Handle) -> Option<Vec<u8>> {
    let path = peek::<DevicePath>(handle)?;
    Some(path.as_bytes().to_vec())
}

/// Завершающий узел пути устройства: тип 0x7F, четыре байта.
const END_NODE_LEN: usize = 4;

/// Является ли `disk` путём носителя, на котором лежит `boot`.
///
/// Путь раздела — это путь его носителя плюс ещё один узел, поэтому сравнение
/// сводится к «начинается с». Единственная тонкость: у пути носителя в конце
/// стоит завершающий узел, которого в середине длинного пути быть не может, —
/// его надо отбросить.
fn is_prefix_of(disk: &[u8], boot: &[u8]) -> bool {
    if disk.len() <= END_NODE_LEN || boot.len() < disk.len() {
        return false;
    }
    boot.starts_with(&disk[..disk.len() - END_NODE_LEN])
}

/// Как носитель подключён — по последнему узлу «сообщения» в пути устройства.
///
/// Полезнее полного пути: путь на экране занимает три строки и всё равно
/// ничего не говорит человеку, а тип шины отвечает ровно на тот вопрос,
/// который у него есть, — «это моя флешка или внутренний диск».
///
/// Узлы разбираются вручную, тремя полями заголовка: тип, подтип и длина.
/// Разбор через типизированный API потребовал бы восстановить `DevicePath` из
/// байтов, то есть небезопасного преобразования указателя ради трёх байт.
fn bus_name(path: Option<&[u8]>) -> &'static str {
    /// Тип узла «сообщение» — описание того, как устройство подключено.
    const MESSAGING: u8 = 0x03;

    let Some(bytes) = path else {
        return "disk";
    };

    let mut name = "disk";
    let mut at = 0usize;
    while at + END_NODE_LEN <= bytes.len() {
        let node_type = bytes[at];
        let sub_type = bytes[at + 1];
        let length = u16::from_le_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        // Узел короче заголовка означает испорченный путь: продолжать разбор
        // бессмысленно, а зациклиться на нулевой длине — легко.
        if length < END_NODE_LEN {
            break;
        }
        if node_type == 0x7F {
            break;
        }
        if node_type == MESSAGING {
            name = match sub_type {
                0x01 => "ATA",
                0x02 => "SCSI",
                0x05 => "USB",
                0x12 => "SATA",
                0x17 => "NVMe",
                0x1A => "SD",
                0x1D => "eMMC",
                _ => name,
            };
        }
        at += length;
    }
    name
}

/// Наибольшая передача, которую крейт `disk` выполняет за раз (32 КиБ при
/// записи данных файла). Промежуточный буфер заводится сразу на неё.
const MAX_TRANSFER: usize = 64 * SECTOR_SIZE;

/// Носитель прошивки, представленный как [`BlockDevice`] для крейта `disk`.
pub struct UefiDisk {
    io: ScopedProtocol<BlockIO>,
    media_id: u32,
    block_size: u32,
    sectors: u64,
    read_only: bool,
    /// Буфер под передачи, если прошивка требует выравнивания.
    ///
    /// `Box<[u64]>`, а не `Vec<u8>`: у элемента `u64` выравнивание восемь
    /// байт, и это единственный простой способ его гарантировать. Массив байт
    /// выровнен на единицу, и прошивка с `IoAlign` больше единицы вправе
    /// отвергнуть такую передачу — а проверить это в QEMU, где `IoAlign` равен
    /// нулю, невозможно вовсе. Буфер поэтому и заводится по факту требования,
    /// а не «на всякий случай»: неиспользуемый путь кода был бы непроверяемым
    /// в обе стороны.
    bounce: Option<Box<[u64]>>,
}

impl UefiDisk {
    /// Открыть носитель на запись.
    ///
    /// Открытие эксклюзивное, и в этом всё дело: оно заставляет прошивку
    /// отключить от носителя свои драйверы, в том числе драйвер FAT. Без этого
    /// его кэш мог бы записать поверх свежей разметки старые сектора — дефект,
    /// который проявляется не сразу и не всегда.
    pub fn open(disk: &Disk) -> Result<Self, Status> {
        let io = boot::open_protocol_exclusive::<BlockIO>(disk.handle).map_err(|err| {
            logln!("[disk] cannot open the target exclusively: {err:?}");
            err.status()
        })?;

        let bounce = if disk.io_align > 1 {
            logln!("[disk] firmware requires {}-byte aligned buffers", disk.io_align);
            let mut buffer = Vec::new();
            buffer
                .try_reserve_exact(MAX_TRANSFER / 8)
                .map_err(|_| Status::OUT_OF_RESOURCES)?;
            buffer.resize(MAX_TRANSFER / 8, 0u64);
            Some(buffer.into_boxed_slice())
        } else {
            None
        };

        Ok(Self {
            io,
            media_id: disk.media_id,
            block_size: disk.block_size,
            sectors: disk.sectors,
            read_only: disk.read_only,
            bounce,
        })
    }
}

/// Первые `len` байт выровненного буфера.
///
/// Свободная функция, а не метод: иначе она заимствовала бы `self` целиком, и
/// вызвать после неё `self.io.write_blocks` было бы нельзя.
fn as_bytes_mut(buffer: &mut [u64], len: usize) -> Option<&mut [u8]> {
    if len > buffer.len() * 8 {
        return None;
    }
    // SAFETY: `[u64]` и `[u8]` имеют одинаковое представление в памяти с
    // точностью до длины, а выравнивание `u64` строже, чем у `u8`, поэтому
    // получившийся срез корректно выровнен. Длина проверена выше, и срез
    // заимствует тот же буфер, что и вход, — время жизни выводится из него.
    let bytes = unsafe {
        core::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), buffer.len() * 8)
    };
    Some(&mut bytes[..len])
}

impl BlockDevice for UefiDisk {
    fn sector_size(&self) -> u32 {
        self.block_size
    }

    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn read(&mut self, lba: u64, buf: &mut [u8]) -> disk::Result<()> {
        // Буфер вынимается из структуры на время передачи: иначе он и `self.io`
        // заимствовались бы одновременно, а срез в буфер, живущий рядом с
        // `&mut self`, — это ровно то место, где безопасный код заканчивается.
        let Some(mut bounce) = self.bounce.take() else {
            return self
                .io
                .read_blocks(self.media_id, lba, buf)
                .map_err(|_| disk::Error::Io);
        };
        let result = match as_bytes_mut(&mut bounce, buf.len()) {
            Some(staging) => self
                .io
                .read_blocks(self.media_id, lba, staging)
                .map_err(|_| disk::Error::Io)
                .map(|()| buf.copy_from_slice(staging)),
            None => Err(disk::Error::Io),
        };
        self.bounce = Some(bounce);
        result
    }

    fn write(&mut self, lba: u64, buf: &[u8]) -> disk::Result<()> {
        let Some(mut bounce) = self.bounce.take() else {
            return self
                .io
                .write_blocks(self.media_id, lba, buf)
                .map_err(|_| disk::Error::Io);
        };
        let result = match as_bytes_mut(&mut bounce, buf.len()) {
            Some(staging) => {
                staging.copy_from_slice(buf);
                self.io
                    .write_blocks(self.media_id, lba, staging)
                    .map_err(|_| disk::Error::Io)
            }
            None => Err(disk::Error::Io),
        };
        self.bounce = Some(bounce);
        result
    }

    fn flush(&mut self) -> disk::Result<()> {
        self.io.flush_blocks().map_err(|_| disk::Error::Io)
    }
}
