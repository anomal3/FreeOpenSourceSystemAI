//! Собственно установка: разметка, форматирование и перенос системы.
//!
//! Вся работа с носителем идёт через крейт [`disk`] — тот самый, которым
//! `xtask image` собирает образ на хосте и который там же покрыт тестами. Это
//! главное свойство всей затеи: код, размечающий чужой диск, невозможно
//! отладить на месте, поэтому отлаживать его надо было раньше и в другом
//! месте.
//!
//! # Порядок шагов и точка невозврата
//!
//! До [`Step::Wipe`] на диск не записано ничего: экраны выбора и подтверждения
//! свободно отматываются назад. Первый же вызов [`Step::Wipe`] уничтожает
//! прежнюю разметку, и дальше отменять нечего — поэтому подтверждение стоит
//! прямо перед ним, а не где-то в середине.
//!
//! # Чего установщик не делает
//!
//! Не создаёт файловую систему на корневом разделе. Своей корневой ФС у FreeOS
//! ещё нет, а положить туда FAT32 значило бы закрепить формат без полей uid,
//! gid и mode — ровно то, ради чего собственная ФС и планируется. Раздел
//! создаётся, помечается своим типом и обнуляется в начале, чтобы прошивка не
//! приняла остатки чужой ФС за настоящие.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use disk::gpt::{self, PartitionSpec};
use disk::guid::Guid;
use disk::{BlockDevice, fat32};

use crate::account::Draft;
use crate::disks::{Disk, UefiDisk};
use crate::lang::Language;
use crate::logln;
use crate::payload::{self, Payload};

/// Желаемый размер системного раздела EFI.
///
/// 512 МиБ — то, что кладут современные установщики: на ESP со временем
/// оседают не только загрузчик и ядро, но и обновления прошивки, и запасные
/// ядра. Раздел ужимается, если диск мал (см. [`Plan::for_disk`]).
const WANTED_ESP: u64 = 512 * 1024 * 1024;

/// Запас на ESP сверх переносимого — на случай, если диск мал и ESP пришлось
/// ужать до размера полезной нагрузки.
const ESP_SLACK: u64 = 32 * 1024 * 1024;

/// Сколько байт в начале корневого раздела обнуляется.
///
/// Мегабайта хватает на суперблок любой существующей файловой системы. Без
/// этого прошивка (или чужая утилита) нашла бы на новом разделе остатки
/// прежней ФС и сочла бы их настоящими.
const ROOT_WIPE_BYTES: u64 = 1024 * 1024;

/// Куда и что ставим.
#[derive(Clone, Copy)]
pub struct Plan {
    pub layout: gpt::Layout,
    pub esp_bytes: u64,
    pub root_bytes: u64,
}

/// Отказ установки.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Диск слишком мал под задуманную разметку.
    TooSmall,
    /// Носитель отказал.
    Disk,
    /// Не удалось прочитать переносимый файл.
    Payload(payload::Error),
}

impl From<disk::Error> for Error {
    fn from(err: disk::Error) -> Self {
        logln!("[install] block layer failed: {err}");
        match err {
            disk::Error::TooSmall => Error::TooSmall,
            _ => Error::Disk,
        }
    }
}

impl Plan {
    /// Спланировать разметку под конкретный диск.
    ///
    /// `payload` — суммарный объём переносимого: ESP обязан вместить его в
    /// любом случае, даже если ради этого раздел придётся сделать меньше
    /// желаемого.
    pub fn for_disk(disk: &Disk, payload: u64) -> Result<Self, Error> {
        let sectors = disk.sectors;
        let usable = disk.bytes();

        // Половина диска — верхняя граница для ESP. Диск, у которого системный
        // раздел больше корневого, выглядит как ошибка установщика, и на малых
        // носителях именно ей бы и был.
        let wanted = WANTED_ESP.min(usable / 2);
        let needed = payload + ESP_SLACK;
        let esp_bytes = wanted.max(needed);

        let layout = gpt::plan(sectors, esp_bytes, true)?;
        let esp_bytes = layout.esp.bytes();
        if esp_bytes < needed {
            logln!(
                "[install] the disk is too small: ESP would be {} bytes, {needed} needed",
                esp_bytes
            );
            return Err(Error::TooSmall);
        }

        let root_bytes = layout.root.map_or(0, |root| root.bytes());
        Ok(Self { layout, esp_bytes, root_bytes })
    }
}

/// Шаг установки — то, что видно человеку на экране хода работ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wipe,
    Gpt,
    Format,
    /// Перенос файла; несёт его роль, чтобы подписать строку.
    Copy(payload::What),
    Config,
    Flush,
}

/// Сколько всего шагов — для полосы хода работ.
pub const TOTAL_STEPS: u32 = 6;

/// Настройки, которые установщик записывает на целевой диск.
pub struct Settings<'a> {
    pub language: Language,
    pub keyboard: &'a str,
    pub timezone: &'a str,
    /// Источник соли и идентификаторов GPT.
    pub entropy: u64,
}

/// Путь на целевом ESP, куда ложится файл учётных записей.
const PASSWD_PATH: &str = "FREEOS/PASSWD";
/// Путь на целевом ESP, куда ложится файл настроек.
const CONFIG_PATH: &str = "FREEOS/SYSTEM.CFG";

/// Выполнить установку.
///
/// `progress` вызывается перед каждым шагом: экран обязан обновиться до того,
/// как начнётся долгая операция, а не после неё.
pub fn run(
    target: &Disk,
    plan: &Plan,
    payload: &mut Payload,
    account: &Draft,
    settings: &Settings,
    mut progress: impl FnMut(u32, Step),
) -> Result<(), Error> {
    let mut dev = UefiDisk::open(target).map_err(|status| {
        logln!("[install] cannot open the target disk: {status:?}");
        Error::Disk
    })?;

    logln!(
        "[install] target: {} sectors, ESP {}..{}, root {:?}",
        dev.sector_count(),
        plan.layout.esp.first_lba,
        plan.layout.esp.last_lba,
        plan.layout.root.map(|root| (root.first_lba, root.last_lba)),
    );

    progress(0, Step::Wipe);
    gpt::wipe(&mut dev)?;

    progress(1, Step::Gpt);
    let mut partitions = Vec::new();
    partitions.push(PartitionSpec {
        type_guid: gpt::ESP_TYPE,
        unique_guid: Guid::from_entropy(expand(settings.entropy, b"freeos-esp")),
        first_lba: plan.layout.esp.first_lba,
        last_lba: plan.layout.esp.last_lba,
        attributes: 0,
        name: "FreeOS ESP",
    });
    if let Some(root) = plan.layout.root {
        partitions.push(PartitionSpec {
            type_guid: gpt::FREEOS_ROOT_TYPE,
            unique_guid: Guid::from_entropy(expand(settings.entropy, b"freeos-root")),
            first_lba: root.first_lba,
            last_lba: root.last_lba,
            attributes: 0,
            name: "FreeOS root",
        });
    }
    gpt::write(
        &mut dev,
        Guid::from_entropy(expand(settings.entropy, b"freeos-disk")),
        &partitions,
    )?;

    progress(2, Step::Format);
    let mut volume = fat32::format(
        &mut dev,
        plan.layout.esp,
        &fat32::FormatOptions {
            label: "FREEOS ESP",
            volume_id: (settings.entropy >> 32) as u32 ^ settings.entropy as u32,
            timestamp: now(),
        },
    )?;

    // Файлы читаются и записываются по одному: образ RAM-диска — сорок
    // мегабайт, и держать в памяти сразу все три значило бы требовать памяти
    // втрое больше без всякой на то причины.
    for index in 0..payload.items.len() {
        let (what, target_path, size) = {
            let item = &payload.items[index];
            (item.what, item.target, item.size)
        };
        progress(3, Step::Copy(what));
        logln!("[install] copying {} -> \\{target_path} ({size} bytes)", what.tag());
        let data = payload.read(index).map_err(Error::Payload)?;
        volume.write_file_path(&mut dev, target_path, &data)?;
        // Освобождаем сразу: следующий файл может оказаться крупнее.
        drop(data);
    }

    progress(4, Step::Config);
    volume.write_file_path(
        &mut dev,
        PASSWD_PATH,
        account.to_passwd(settings.entropy).as_bytes(),
    )?;
    volume.write_file_path(&mut dev, CONFIG_PATH, config_text(settings).as_bytes())?;

    progress(5, Step::Flush);
    volume.finish(&mut dev)?;

    // Корневой раздел остаётся без файловой системы, но его начало обнуляется:
    // иначе там осталась бы прежняя ФС, и всякий, кто посмотрит на диск,
    // увидел бы раздел FreeOS с чужим содержимым.
    if let Some(root) = plan.layout.root {
        let sectors = (ROOT_WIPE_BYTES / disk::SECTOR_SIZE as u64).min(root.sectors());
        logln!("[install] zeroing the first {sectors} sectors of the root partition");
        zero(&mut dev, root.first_lba, sectors)?;
    }

    dev.flush()?;
    logln!("[install] finished");
    Ok(())
}

/// Обнулить диапазон секторов.
///
/// Своя копия вместо `disk::zero_sectors`: та закрыта внутри крейта, и
/// открывать её наружу ради одного вызова — значит расширять интерфейс
/// разметки операцией, которая к разметке не относится.
fn zero(dev: &mut dyn BlockDevice, lba: u64, count: u64) -> Result<(), Error> {
    const CHUNK: usize = 16;
    static ZEROS: [u8; CHUNK * disk::SECTOR_SIZE] = [0; CHUNK * disk::SECTOR_SIZE];

    let mut done = 0u64;
    while done < count {
        let batch = ((count - done) as usize).min(CHUNK);
        dev.write(lba + done, &ZEROS[..batch * disk::SECTOR_SIZE])?;
        done += batch as u64;
    }
    Ok(())
}

/// Содержимое файла настроек.
fn config_text(settings: &Settings) -> String {
    let mut out = String::new();
    out.push_str("# FreeOS system configuration, written by the installer\n");
    out.push_str(&format!("language={}\n", settings.language.tag()));
    out.push_str(&format!("keyboard={}\n", settings.keyboard));
    out.push_str(&format!("timezone={}\n", settings.timezone));
    out
}

/// Текущее время прошивки в виде метки FAT.
///
/// Часы прошивки могут быть не выставлены — тогда метка окажется эпохой FAT.
/// Это не повод прерывать установку: неверная дата у файла на ESP не мешает
/// ничему, а отказ из-за неё был бы совершенно непропорционален.
fn now() -> fat32::Timestamp {
    match uefi::runtime::get_time() {
        Ok(time) => fat32::Timestamp::new(
            time.year(),
            time.month(),
            time.day(),
            time.hour(),
            time.minute(),
            time.second(),
        ),
        Err(err) => {
            logln!("[install] the firmware clock is unavailable ({err:?}), using the FAT epoch");
            fat32::Timestamp::EPOCH
        }
    }
}

/// Растянуть 64-битное зерно в 16 байт под GUID, подмешав назначение.
///
/// Соль нужна, чтобы идентификаторы диска и разделов не совпали между собой:
/// совпадающие GUID — законный повод для утилит счесть разметку испорченной.
fn expand(seed: u64, salt: &[u8]) -> [u8; 16] {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in salt {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let high = seed ^ hash;
    let low = seed.rotate_left(29) ^ hash.rotate_left(7);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&high.to_be_bytes());
    out[8..].copy_from_slice(&low.to_be_bytes());
    out
}
