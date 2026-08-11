//! Том, с которого прошивка запустила загрузчик.
//!
//! Том находится по loaded image, а не перебором всех носителей: на машине с
//! несколькими ESP так гарантированно берутся ядро и образ ФС «из того же
//! комплекта», что и сам загрузчик.
//!
//! Открывается один раз на всю загрузку и закрывается до `ExitBootServices`:
//! после выхода протоколы прошивки недействительны, а держать открытый хендл
//! дольше, чем нужно, незачем.

use uefi::boot::{self, ScopedProtocol};
use uefi::proto::media::file::{
    Directory, File, FileAttribute, FileInfo, FileMode, FileType, RegularFile,
};
use uefi::proto::media::fs::SimpleFileSystem;
use uefi::{CStr16, Status, println};

use crate::Aborted;

/// Открытый корень загрузочного тома.
pub struct BootVolume {
    /// Корень тома. Объявлен раньше протокола намеренно: поля структуры
    /// уничтожаются в порядке объявления, и хендл каталога обязан закрыться до
    /// того, как будет закрыт сам `SimpleFileSystem`.
    root: Directory,
    /// Держится только ради `Drop`: пока протокол открыт, хендлы файлов живы.
    _fs: ScopedProtocol<SimpleFileSystem>,
}

impl BootVolume {
    /// Открывает том, с которого стартовал загрузчик.
    pub fn open() -> Result<Self, Aborted> {
        let image_handle = boot::image_handle();

        let mut fs = match boot::get_image_file_system(image_handle) {
            Ok(fs) => fs,
            Err(err) => {
                println!("  [fs ] cannot reach the volume this loader was started from ({err:?})");
                println!("  [fs ] the firmware exposes no SimpleFileSystem on the boot device");
                return Err(Aborted);
            }
        };

        let root = match fs.open_volume() {
            Ok(root) => root,
            Err(err) => {
                println!("  [fs ] cannot open the volume root ({err:?})");
                return Err(Aborted);
            }
        };

        Ok(Self { root, _fs: fs })
    }

    /// Открывает обычный файл в корне тома на чтение.
    ///
    /// `Ok(None)` означает ровно одно: файла с таким именем на томе нет. Это
    /// отличается от ошибки открытия — вызывающий сам решает, обязателен ли
    /// файл (ядро) или нет (образ ФС).
    pub fn open_regular(&mut self, path: &CStr16) -> Result<Option<RegularFile>, Aborted> {
        let handle = match self.root.open(path, FileMode::Read, FileAttribute::empty()) {
            Ok(handle) => handle,
            Err(err) if err.status() == Status::NOT_FOUND => return Ok(None),
            Err(err) => {
                println!("  [fs ] cannot open {path} ({err:?})");
                return Err(Aborted);
            }
        };

        match handle.into_type() {
            Ok(FileType::Regular(file)) => Ok(Some(file)),
            Ok(FileType::Dir(_)) => {
                println!("  [fs ] {path} is a directory, not a file");
                Err(Aborted)
            }
            Err(err) => {
                println!("  [fs ] cannot classify {path} ({err:?})");
                Err(Aborted)
            }
        }
    }
}

/// Размер файла по данным файловой системы.
///
/// Спрашивается у ФС, а не угадывается: размер образа заранее неизвестен, а
/// читать «до конца файла» через `EFI_FILE_PROTOCOL` без предварительно
/// выделенного буфера всё равно нельзя.
pub fn size_of(file: &mut RegularFile, path: &CStr16) -> Result<u64, Aborted> {
    match file.get_boxed_info::<FileInfo>() {
        Ok(info) => Ok(info.file_size()),
        Err(err) => {
            println!("  [fs ] cannot stat {path} ({err:?})");
            Err(Aborted)
        }
    }
}
