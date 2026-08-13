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
//! # Два раздела, две файловые системы
//!
//! На системном разделе EFI обязана быть FAT32 — этого требует спецификация
//! UEFI, и выбора здесь нет. На корневом стоит ext2, и там выбор был: FAT32 не
//! хранит ни `uid`, ни `gid`, ни `mode`, а добавить права после того, как на
//! диске появились пользовательские данные, значит менять формат и мигрировать.
//!
//! Отсюда и разделение содержимого. Загрузчик, ядро и образ RAM-диска лежат на
//! ESP: их читает прошивка и загрузчик, то есть до того, как существует хоть
//! какой-то драйвер FreeOS. Учётная запись и настройки лежат на корневом
//! разделе с правильными правами: их читает уже система.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use disk::gpt::{self, PartitionSpec};
use disk::guid::Guid;
use disk::{BlockDevice, fat32};

use crate::account::{self, Draft};
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

/// Метка корневого тома.
const ROOT_LABEL: &str = "FreeOS";

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
    /// Не удалось создать корневую файловую систему.
    RootFs,
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

impl From<ext2::Error> for Error {
    fn from(err: ext2::Error) -> Self {
        logln!("[install] the root filesystem failed: {err}");
        match err {
            ext2::Error::TooSmall => Error::TooSmall,
            _ => Error::RootFs,
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
        let sector = disk.block_size as usize;
        let usable = disk.bytes();

        // Половина диска — верхняя граница для ESP. Диск, у которого системный
        // раздел больше корневого, выглядит как ошибка установщика, и на малых
        // носителях именно ей бы и был.
        let wanted = WANTED_ESP.min(usable / 2);
        let needed = payload + ESP_SLACK;
        let esp_bytes = wanted.max(needed);

        let layout = gpt::plan(sectors, sector, esp_bytes, true)?;
        let esp_bytes = layout.esp.bytes(sector);
        if esp_bytes < needed {
            logln!(
                "[install] the disk is too small: ESP would be {} bytes, {needed} needed",
                esp_bytes
            );
            return Err(Error::TooSmall);
        }

        let root_bytes = layout.root.map_or(0, |root| root.bytes(sector));
        Ok(Self { layout, esp_bytes, root_bytes })
    }
}

/// Шаг установки — то, что видно человеку на экране хода работ.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Wipe,
    Gpt,
    FormatEsp,
    /// Перенос файла; несёт его роль, чтобы подписать строку.
    Copy(payload::What),
    FormatRoot,
    Config,
    Flush,
}

/// Сколько всего шагов — для полосы хода работ.
pub const TOTAL_STEPS: u32 = 7;

/// Настройки, которые установщик записывает на целевой диск.
pub struct Settings<'a> {
    pub language: Language,
    pub keyboard: &'a str,
    pub timezone: &'a str,
    /// Источник соли и идентификаторов GPT.
    pub entropy: u64,
    /// Текущее время в секундах эпохи Unix.
    ///
    /// Передаётся снаружи, а не берётся здесь: часы доступны только через
    /// runtime-сервисы UEFI, о которых крейты разметки знать не должны.
    pub unix_time: u32,
}

/// Путь на корневом разделе, куда ложится файл учётных записей.
const PASSWD_PATH: &str = "etc/passwd";
/// Путь на корневом разделе, куда ложится файл настроек.
const CONFIG_PATH: &str = "etc/system.cfg";

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

    progress(2, Step::FormatEsp);
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
        // Программы на ESP не едут: их место на корневом разделе, где есть
        // права. Они переносятся ниже, после того как он отформатирован.
        if what == payload::What::Program {
            continue;
        }
        progress(3, Step::Copy(what));
        logln!("[install] copying {} -> \\{target_path} ({size} bytes)", what.tag());
        let data = payload.read(index).map_err(Error::Payload)?;
        volume.write_file_path(&mut dev, target_path, &data)?;
        // Освобождаем сразу: следующий файл может оказаться крупнее.
        drop(data);
    }

    volume.finish(&mut dev)?;

    // Корневой раздел. Его отсутствие — не отказ: на диске, где под него не
    // хватило места, система всё равно загрузится, просто учётной записи ей
    // будет негде взять. Сказать об этом в журнале честнее, чем прервать
    // установку, которая почти удалась.
    let Some(root) = plan.layout.root else {
        logln!("[install] no root partition: the account has nowhere to go");
        progress(6, Step::Flush);
        dev.flush()?;
        return Ok(());
    };

    progress(4, Step::FormatRoot);
    let ext2_options = ext2::FormatOptions {
        label: ROOT_LABEL,
        uuid: expand(settings.entropy, b"freeos-root-fs"),
        time: settings.unix_time,
    };
    let mut fs = ext2::format(&mut dev, root.first_lba, root.sectors(), &ext2_options)?;
    // Том помечается используемым на всё время установки. Прерванная установка
    // — это ровно тот случай, ради которого признак существует: файлов на томе
    // не хватает, счётчики не сброшены, и следующая загрузка обязана об этом
    // знать, а не считать полуготовый раздел исправным.
    fs.mark_dirty(&mut dev)?;
    logln!(
        "[install] root: ext2, {} blocks of {} bytes in {} group(s)",
        fs.geometry().blocks,
        fs.geometry().block_size.bytes(),
        fs.geometry().groups,
    );

    progress(5, Step::Config);
    // Права проставляются сразу и настоящие, хотя проверять их пока некому:
    // выставить их позже значит на какое-то время оставить файл учётных
    // записей открытым на чтение всем.
    fs.create_dir_path(&mut dev, "etc", 0o755, 0, 0)?;
    fs.write_file_path(
        &mut dev,
        PASSWD_PATH,
        account.to_passwd(settings.entropy).as_bytes(),
        account::PASSWD_MODE,
        0,
        0,
    )?;
    fs.write_file_path(
        &mut dev,
        CONFIG_PATH,
        config_text(settings, account).as_bytes(),
        0o644,
        0,
        0,
    )?;

    // Домашний каталог принадлежит пользователю, а не root: иначе первое, что
    // человек обнаружит в своей системе, — что ему некуда писать. А вот сам
    // `/home` остаётся за root: иначе первый заведённый пользователь смог бы
    // удалить домашний каталог второго.
    fs.create_dir_path(&mut dev, "home", 0o755, 0, 0)?;
    let home = alloc::format!("home/{}", account.name);
    fs.create_dir_path(
        &mut dev,
        &home,
        account::HOME_MODE,
        account::FIRST_UID,
        account::FIRST_UID,
    )?;
    // Файл в домашнем каталоге принадлежит человеку и закрыт от всех
    // остальных. Он здесь не ради содержимого: это первый файл в системе,
    // который читается по классу владельца, — и единственный способ увидеть,
    // что этот класс вообще работает.
    fs.write_file_path(
        &mut dev,
        &format!("{home}/notes.txt"),
        notes_text(&account.name).as_bytes(),
        0o600,
        account::FIRST_UID,
        account::FIRST_UID,
    )?;

    // Домашний каталог суперпользователя, закрытый от всех: `0700`, владелец
    // root. Файл внутри намеренно оставлен читаемым всем — `0644`. Пара
    // существует затем, чтобы разница между «проверили файл» и «прошли по
    // пути» была видна: права файла разрешают чтение, а каталог не пускает,
    // и правильный ответ системы — отказ.
    fs.create_dir_path(&mut dev, "root", 0o700, 0, 0)?;
    fs.write_file_path(
        &mut dev,
        "root/notes.txt",
        b"This file is world-readable and lives in a directory that is not.\n",
        0o644,
        0,
        0,
    )?;

    logln!("[install] root: /etc/passwd, /etc/system.cfg, /{home} and /root");

    // Программы. `/bin` принадлежит root и открыт всем на чтение и проход, сами
    // программы — `0755`: запускать их вправе кто угодно, менять — никто, кроме
    // root. Ровно так же выглядит любой Unix, и не по традиции: право менять
    // исполняемый файл — это право исполнять что угодно от чужого имени.
    fs.create_dir_path(&mut dev, "bin", 0o755, 0, 0)?;
    let mut programs = 0usize;
    for index in 0..payload.items.len() {
        let (what, name, size) = {
            let item = &payload.items[index];
            (item.what, item.target, item.size)
        };
        if what != payload::What::Program {
            continue;
        }
        progress(5, Step::Copy(what));
        logln!("[install] copying program -> /bin/{name} ({size} bytes)");
        let data = payload.read(index).map_err(Error::Payload)?;
        fs.write_file_path(&mut dev, &format!("bin/{name}"), &data, 0o755, 0, 0)?;
        drop(data);
        programs += 1;
    }
    logln!("[install] root: {programs} program(s) in /bin");

    progress(6, Step::Flush);
    fs.flush(&mut dev)?;
    dev.flush()?;
    // И только теперь — «закрыт чисто»: признак ставится последним, потому что
    // он утверждает, что всё предыдущее состоялось.
    fs.mark_clean(&mut dev)?;
    logln!("[install] finished");
    Ok(())
}

/// Содержимое файла настроек.
fn config_text(settings: &Settings, account: &Draft) -> String {
    let mut out = String::new();
    out.push_str("# FreeOS system configuration, written by the installer\n");
    out.push_str(&format!("language={}\n", settings.language.tag()));
    out.push_str(&format!("keyboard={}\n", settings.keyboard));
    out.push_str(&format!("timezone={}\n", settings.timezone));
    // Имя и домашний каталог лежат и в `/etc/passwd`, но тот закрыт от всех,
    // кроме root: в нём отпечаток пароля. Программе, которой нужно знать, где
    // её домашний каталог, читать файл с паролями незачем — потому эти две
    // строки и продублированы в открытом файле настроек. Так же разведены
    // `/etc/passwd` и общедоступные настройки в любом Unix.
    out.push_str(&format!("user={}\n", account.name));
    out.push_str(&format!("home=/home/{}\n", account.name));
    out
}

/// Содержимое файла в домашнем каталоге.
fn notes_text(name: &str) -> String {
    format!(
        "Hello, {name}.\n\
         This file belongs to you and to nobody else: mode 0600.\n\
         A program running as you can read it; one running as anyone else cannot.\n"
    )
}

/// Текущее время прошивки в виде метки FAT.
///
/// Часы прошивки могут быть не выставлены — тогда метка окажется эпохой FAT.
/// Это не повод прерывать установку: неверная дата у файла не мешает ничему, а
/// отказ из-за неё был бы совершенно непропорционален.
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

/// Текущее время в секундах эпохи Unix.
///
/// Ноль, если часов нет: файл с датой 1970 года выглядит странно, но это
/// честнее выдуманной даты и не мешает ничему.
/// Календарная арифметика вынесена в крейт `calendar`, потому что теперь она
/// нужна и ядру: время суток система получает от прошивки через загрузчик.
/// Копия этих тридцати строк в третьем месте разошлась бы с остальными ровно в
/// той мере, в какой её потом правили бы порознь.
#[must_use]
pub fn unix_now() -> u32 {
    let Ok(time) = uefi::runtime::get_time() else {
        return 0;
    };
    let civil = calendar::DateTime::new(
        i32::from(time.year()),
        time.month(),
        time.day(),
        time.hour(),
        time.minute(),
        time.second(),
    );
    u32::try_from(civil.to_unix()).unwrap_or(0)
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
