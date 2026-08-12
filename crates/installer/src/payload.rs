//! Полезная нагрузка: то, что установщик переносит на целевой диск.
//!
//! Файлы лежат на том же носителе, с которого запущен установщик, в каталоге
//! `\FREEOS`. Том открывается по загруженному образу, а не перебором носителей:
//! на машине с несколькими ESP так гарантированно берётся комплект «из той же
//! коробки», что и сам установщик (та же причина и тот же приём, что в
//! загрузчике).
//!
//! # Почему размеры узнаются заранее, а содержимое — нет
//!
//! Образ RAM-диска — сорок мегабайт. Держать его в памяти всё время работы
//! установщика незачем: он нужен ровно на время одной записи. А вот **размеры**
//! нужны на первом же экране: без них нельзя ни проверить, что носитель
//! комплектный, ни сказать, поместится ли система на выбранный диск. Отсюда
//! разделение: [`probe`] спрашивает у файловой системы размеры, [`Payload::read`]
//! читает файл в память непосредственно перед записью.

use alloc::vec::Vec;

use uefi::boot::{self, ScopedProtocol};
use uefi::proto::media::file::{Directory, File, FileAttribute, FileInfo, FileMode, FileType};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CStr16, Status, cstr16};

use crate::logln;

/// Имя загрузчика в стандартном пути removable media.
///
/// Единственная арх-специфичная строка в установщике: всё остальное обязано
/// быть общим для `x86_64-unknown-uefi` и `aarch64-unknown-uefi`.
pub const BOOT_FILE: &str = if cfg!(target_arch = "x86_64") {
    "BOOTX64.EFI"
} else {
    "BOOTAA64.EFI"
};

/// `BOOT_FILE` схлопывается в вариант ARM на любой третьей архитектуре,
/// поэтому сборка под неё запрещена явно.
const _: () = assert!(
    cfg!(target_arch = "x86_64") || cfg!(target_arch = "aarch64"),
    "installer supports only the x86_64-unknown-uefi and aarch64-unknown-uefi targets"
);

/// Один переносимый файл.
pub struct Item {
    /// Путь на установочном носителе.
    source: &'static CStr16,
    /// Путь назначения, разделитель — `/`. Куда именно он ведёт, решает
    /// [`Item::what`]: система едет на ESP, программы — на корневой раздел.
    pub target: &'static str,
    /// Что это, для показа человеку.
    pub what: What,
    pub size: u64,
}

/// Роль файла в системе.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum What {
    Bootloader,
    Kernel,
    Initrd,
    /// Пользовательская программа. Едет в `/bin` корневого раздела, а не на
    /// ESP: программу запускает система, а не прошивка, и лежать ей полагается
    /// там, где есть права.
    Program,
}

/// Открытый установочный носитель вместе с описью.
pub struct Payload {
    root: Directory,
    _fs: ScopedProtocol<SimpleFileSystem>,
    pub items: Vec<Item>,
}

/// Отказ при подготовке или чтении.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Том, с которого запущен установщик, недоступен.
    NoVolume,
    /// Файла нет — установочный носитель собран неполно.
    Missing(What),
    /// Файл есть, но прочитать его не удалось.
    Unreadable(What),
    /// Не хватило памяти под содержимое файла.
    NoMemory(What),
}

impl What {
    /// Ключ, по которому этот файл называют в журнале.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            What::Bootloader => "bootloader",
            What::Kernel => "kernel",
            What::Initrd => "initrd",
            What::Program => "program",
        }
    }
}

/// Открыть установочный носитель и снять опись.
pub fn probe() -> Result<Payload, Error> {
    let mut fs = boot::get_image_file_system(boot::image_handle()).map_err(|err| {
        logln!("[payload] no SimpleFileSystem on the boot device: {err:?}");
        Error::NoVolume
    })?;
    let mut root = fs.open_volume().map_err(|err| {
        logln!("[payload] cannot open the volume root: {err:?}");
        Error::NoVolume
    })?;

    // Пути на носителе и пути на целевом ESP различаются намеренно. На
    // носителе загрузчик лежит в `\FREEOS`, потому что стандартный путь
    // `\EFI\BOOT\BOOT*.EFI` занят самим установщиком — прошивка запускает
    // именно его. На целевом диске всё встаёт по своим местам.
    let sources: [(&'static CStr16, &'static str, What); 3] = [
        (
            if cfg!(target_arch = "x86_64") {
                cstr16!("\\FREEOS\\BOOTX64.EFI")
            } else {
                cstr16!("\\FREEOS\\BOOTAA64.EFI")
            },
            if cfg!(target_arch = "x86_64") {
                "EFI/BOOT/BOOTX64.EFI"
            } else {
                "EFI/BOOT/BOOTAA64.EFI"
            },
            What::Bootloader,
        ),
        (cstr16!("\\FREEOS\\KERNEL.ELF"), "kernel.elf", What::Kernel),
        (cstr16!("\\FREEOS\\INITRD.IMG"), "initrd.img", What::Initrd),
    ];

    let mut items = Vec::new();
    for (source, target, what) in sources {
        let size = stat(&mut root, source, what)?;
        logln!("[payload] {} {source}: {size} bytes", what.tag());
        items.push(Item { source, target, what, size });
    }

    // Программы. Их отсутствие установку не срывает: система без `/bin`
    // загрузится и будет работать, просто запускать ей будет нечего. Прервать
    // из-за них установку значило бы оценить программы дороже учётной записи и
    // настроек, ради которых всё и затевалось.
    for (source, target) in PROGRAMS {
        match stat(&mut root, source, What::Program) {
            Ok(size) => {
                logln!("[payload] program {source}: {size} bytes");
                items.push(Item { source, target, what: What::Program, size });
            }
            Err(_) => logln!("[payload] program {source} is missing; /bin will lack it"),
        }
    }

    Ok(Payload { root, _fs: fs, items })
}

/// Пользовательские программы на носителе и их имена в `/bin`.
///
/// Список обязан совпадать с `USER_PROGRAMS` в `xtask/src/build.rs` — это тот
/// же комплект, разложенный по носителю. Расхождение не остаётся незамеченным:
/// установленная система без `/bin/perms` валит сценарий `installed` на стенде.
const PROGRAMS: [(&CStr16, &str); 4] = [
    (cstr16!("\\FREEOS\\BIN\\HELLO"), "hello"),
    (cstr16!("\\FREEOS\\BIN\\CRASH"), "crash"),
    (cstr16!("\\FREEOS\\BIN\\PEEK"), "peek"),
    (cstr16!("\\FREEOS\\BIN\\PERMS"), "perms"),
];

/// Размер файла по данным файловой системы.
fn stat(root: &mut Directory, path: &CStr16, what: What) -> Result<u64, Error> {
    let handle = match root.open(path, FileMode::Read, FileAttribute::empty()) {
        Ok(handle) => handle,
        Err(err) if err.status() == Status::NOT_FOUND => {
            logln!("[payload] {path} is missing from the install media");
            return Err(Error::Missing(what));
        }
        Err(err) => {
            logln!("[payload] cannot open {path}: {err:?}");
            return Err(Error::Unreadable(what));
        }
    };
    let Ok(FileType::Regular(mut file)) = handle.into_type() else {
        logln!("[payload] {path} is not a regular file");
        return Err(Error::Unreadable(what));
    };
    // Размер спрашивается у файловой системы, а не угадывается: читать «до
    // конца файла» через EFI_FILE_PROTOCOL без заранее выделенного буфера
    // нельзя.
    file.get_boxed_info::<FileInfo>()
        .map(|info| info.file_size())
        .map_err(|err| {
            logln!("[payload] cannot stat {path}: {err:?}");
            Error::Unreadable(what)
        })
}

impl Payload {
    /// Суммарный объём переносимого.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    /// Прочитать файл целиком.
    pub fn read(&mut self, index: usize) -> Result<Vec<u8>, Error> {
        let item = self.items.get(index).ok_or(Error::NoVolume)?;
        let (path, what, size) = (item.source, item.what, item.size);

        let handle = self
            .root
            .open(path, FileMode::Read, FileAttribute::empty())
            .map_err(|err| {
                logln!("[payload] cannot reopen {path}: {err:?}");
                Error::Unreadable(what)
            })?;
        let Ok(FileType::Regular(mut file)) = handle.into_type() else {
            return Err(Error::Unreadable(what));
        };

        let len = usize::try_from(size).map_err(|_| Error::NoMemory(what))?;
        let mut data = Vec::new();
        // `try_reserve_exact`, а не `vec![0; len]`: сорок мегабайт под образ
        // ФС — отказ здесь совершенно реален, и он обязан быть сообщением на
        // экране, а не паникой посреди установки.
        data.try_reserve_exact(len).map_err(|_| {
            logln!("[payload] out of memory reading {} ({len} bytes)", what.tag());
            Error::NoMemory(what)
        })?;
        data.resize(len, 0);

        let read = file.read(&mut data).map_err(|err| {
            logln!("[payload] cannot read {path}: {err:?}");
            Error::Unreadable(what)
        })?;
        if read != len {
            logln!("[payload] short read of {path}: {read} of {len} bytes");
            return Err(Error::Unreadable(what));
        }

        Ok(data)
    }
}
