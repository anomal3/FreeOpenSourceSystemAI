//! Раздел состояния готового образа — с хоста, до запуска: ext2 или btrfs.
//!
//! С 31.10 установщик умеет размечать состояние под btrfs, с 02.11 это
//! умолчание; ext2 — по метке `\FREEOS\STATEEXT.CFG` (см.
//! [`STATE_EXT2_MARK`]). Стенд кладёт туда настройки, ключи SSH и Rider. Формат узнаётся пробой
//! первых байт тома, как это делает ядро; всё, что стенду нужно от тома, —
//! здесь, одним набором операций на оба формата.

use std::path::Path;

use anyhow::{Result, anyhow};
use disk::BlockDevice as _;

use crate::diskfile::DiskFile;

/// Метка на установочном носителе: раздел состояния — ext2 (откат; с 02.11
/// умолчание — btrfs). Стенд кладёт её, когда `FOSSAI_STATE_FS=ext2`.
pub const STATE_EXT2_MARK: &str = "STATEEXT.CFG";

/// Просит ли прогон раздел состояния ext2 вместо btrfs.
pub fn ext2_requested() -> bool {
    std::env::var("FOSSAI_STATE_FS").is_ok_and(|value| value.eq_ignore_ascii_case("ext2"))
}

enum Fs {
    Ext2(ext2::Editor),
    Btrfs(btrfs::Writer),
}

/// Раздел состояния образа, открытый на запись.
pub struct StateVolume {
    dev: DiskFile,
    fs: Fs,
    /// Время правок btrfs (ext2 берёт своё).
    time: u64,
    /// Правок с прошлой фиксации btrfs.
    pending: usize,
}

/// Узел: номер и каталог ли.
#[derive(Clone, Copy)]
pub struct Found {
    pub number: u64,
    pub directory: bool,
}

/// Через сколько правок фиксировать btrfs: память писателя не растёт без
/// конца, когда стенд кладёт десятки тысяч файлов Rider.
const COMMIT_EVERY: usize = 2000;

fn state_lba(dev: &mut DiskFile, disk_path: &Path) -> Result<u64> {
    let table = disk::gpt::read(dev).map_err(|err| anyhow!("на образе {} нет GPT: {err}", disk_path.display()))?;
    let state = table.find(disk::gpt::FREEOS_STATE_TYPE).ok_or_else(|| anyhow!("на образе нет раздела состояния"))?;
    Ok(state.first_lba)
}

/// Прочитать файл раздела состояния тем же крейтом, которым прочтёт система.
/// `None` — файла нет.
pub fn read_file(disk_path: &Path, path: &str) -> Result<Option<Vec<u8>>> {
    let mut dev = DiskFile::open(disk_path, 512)?;
    let lba = state_lba(&mut dev, disk_path)?;
    let path = format!("/{}", path.trim_start_matches('/'));
    if btrfs::detect(&mut dev, lba) {
        let mut fs = btrfs::Btrfs::mount(&mut dev, lba).map_err(|err| anyhow!("раздел состояния не монтируется: {err}"))?;
        let Ok(inode) = fs.resolve(&mut dev, &path) else { return Ok(None) };
        let data = fs.read_file(&mut dev, &inode).map_err(|err| anyhow!("{path} не читается: {err}"))?;
        return Ok(Some(data));
    }
    let fs = ext2::Ext2::mount(&mut dev, lba).map_err(|err| anyhow!("раздел состояния не монтируется: {err}"))?;
    let Ok(inode) = fs.resolve(&mut dev, &path) else { return Ok(None) };
    let data = fs.read_file(&mut dev, &inode).map_err(|err| anyhow!("{path} не читается: {err}"))?;
    Ok(Some(data))
}

/// Права, владелец и вид узла — для проверок стенда.
#[cfg(test)]
pub fn stat(disk_path: &Path, path: &str) -> Result<Option<(bool, u16, u32, u32)>> {
    let mut dev = DiskFile::open(disk_path, 512)?;
    let lba = state_lba(&mut dev, disk_path)?;
    let path = format!("/{}", path.trim_start_matches('/'));
    if btrfs::detect(&mut dev, lba) {
        let mut fs = btrfs::Btrfs::mount(&mut dev, lba).map_err(|err| anyhow!("раздел состояния не монтируется: {err}"))?;
        let Ok(inode) = fs.resolve(&mut dev, &path) else { return Ok(None) };
        return Ok(Some((inode.kind == btrfs::FileType::Directory, inode.mode, inode.uid, inode.gid)));
    }
    let fs = ext2::Ext2::mount(&mut dev, lba).map_err(|err| anyhow!("раздел состояния не монтируется: {err}"))?;
    let Ok(inode) = fs.resolve(&mut dev, &path) else { return Ok(None) };
    Ok(Some((inode.kind == ext2::FileType::Directory, inode.mode & 0o7777, inode.uid, inode.gid)))
}

impl StateVolume {
    /// Открыть раздел состояния образа на запись. ext2 помечается
    /// используемым до [`StateVolume::finish`] — как это делает установщик.
    pub fn open(disk_path: &Path) -> Result<Self> {
        let mut dev = DiskFile::open(disk_path, 512)?;
        let lba = state_lba(&mut dev, disk_path)?;
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        let fs = if btrfs::detect(&mut dev, lba) {
            Fs::Btrfs(btrfs::Writer::open(&mut dev, lba).map_err(|err| anyhow!("раздел состояния (btrfs) не открывается: {err}"))?)
        } else {
            let mut fs = ext2::Editor::open(&mut dev, lba).map_err(|err| anyhow!("раздел состояния не открывается: {err}"))?;
            fs.mark_dirty(&mut dev).map_err(|err| anyhow!("не удалось пометить том используемым: {err}"))?;
            Fs::Ext2(fs)
        };
        Ok(Self { dev, fs, time, pending: 0 })
    }

    fn attrs(&self, mode: u16, uid: u32, gid: u32) -> btrfs::Attributes {
        btrfs::Attributes { mode, uid, gid, time: self.time }
    }

    /// Правка сделана: btrfs фиксируется раз в [`COMMIT_EVERY`] правок.
    fn changed(&mut self) -> Result<()> {
        self.pending += 1;
        if self.pending >= COMMIT_EVERY {
            self.pending = 0;
            if let Fs::Btrfs(writer) = &mut self.fs {
                writer.commit(&mut self.dev).map_err(|err| anyhow!("фиксация раздела состояния не удалась: {err}"))?;
            }
        }
        Ok(())
    }

    /// Узел по пути от корня тома; `None` — нет такого.
    pub fn lookup_path(&mut self, path: &str) -> Result<Option<Found>> {
        let mut found = Found { number: 0, directory: true };
        let mut first = true;
        for component in path.split('/').filter(|part| !part.is_empty()) {
            // Звено пути — не каталог: дальше идти некуда.
            if !found.directory {
                return Ok(None);
            }
            let parent = if first { None } else { Some(found.number) };
            first = false;
            let next = match &mut self.fs {
                Fs::Ext2(fs) => fs
                    .lookup(&mut self.dev, parent.map_or(ext2::ROOT_INODE, |n| n as u32), component)
                    .map_err(|err| anyhow!("не удалось прочитать /{path}: {err}"))?
                    .map(|(number, kind)| Found { number: u64::from(number), directory: kind == ext2::DIR_TYPE_DIRECTORY }),
                Fs::Btrfs(writer) => {
                    let dir = match parent {
                        Some(n) => n,
                        None => writer.resolve(&mut self.dev, "/").map_err(|err| anyhow!("корень тома не читается: {err}"))?,
                    };
                    match writer.lookup(&mut self.dev, dir, component).map_err(|err| anyhow!("не удалось прочитать /{path}: {err}"))? {
                        Some(number) => {
                            let inode = writer.inode(&mut self.dev, number).map_err(|err| anyhow!("/{path}: {err}"))?;
                            Some(Found { number, directory: inode.kind == btrfs::FileType::Directory })
                        }
                        None => None,
                    }
                }
            };
            match next {
                Some(next) => found = next,
                None => return Ok(None),
            }
        }
        Ok((!first).then_some(found))
    }

    /// `mkdir -p`; возвращает номер последнего каталога.
    pub fn create_dir_path(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<u64> {
        let attrs = self.attrs(mode, uid, gid);
        let number = match &mut self.fs {
            Fs::Ext2(fs) => fs.create_dir_path(&mut self.dev, path, mode, uid, gid).map(u64::from).map_err(|err| anyhow!("не удалось создать /{path}: {err}"))?,
            Fs::Btrfs(writer) => writer.create_dir_path(&mut self.dev, path, &attrs).map_err(|err| anyhow!("не удалось создать /{path}: {err}"))?,
        };
        self.changed()?;
        Ok(number)
    }

    /// Создать файл по пути с содержимым. `None` — файл уже есть (стенд
    /// узнаёт по этому «уже лежит»).
    pub fn write_file_path(&mut self, path: &str, data: &[u8], mode: u16, uid: u32, gid: u32) -> Result<Option<u64>> {
        let attrs = self.attrs(mode, uid, gid);
        let number = match &mut self.fs {
            Fs::Ext2(fs) => match fs.write_file_path(&mut self.dev, path, data, mode, uid, gid) {
                Ok(number) => u64::from(number),
                Err(ext2::Error::Exists) => return Ok(None),
                Err(err) => return Err(anyhow!("не удалось записать /{path}: {err}")),
            },
            Fs::Btrfs(writer) => match writer.write_file_path(&mut self.dev, path, data, &attrs) {
                Ok(number) => number,
                Err(btrfs::Error::Exists) => return Ok(None),
                Err(err) => return Err(anyhow!("не удалось записать /{path}: {err}")),
            },
        };
        self.changed()?;
        Ok(Some(number))
    }

    /// Дописать байты в файл по смещению.
    pub fn write_at(&mut self, number: u64, offset: u64, data: &[u8]) -> Result<()> {
        let time = self.time;
        match &mut self.fs {
            Fs::Ext2(fs) => {
                fs.write_at(&mut self.dev, number as u32, offset, data).map_err(|err| anyhow!("{err}"))?;
            }
            Fs::Btrfs(writer) => {
                writer.write_at(&mut self.dev, number, offset, data, time).map_err(|err| anyhow!("{err}"))?;
            }
        }
        self.changed()
    }

    /// Стереть файл или пустой каталог по пути; нет такого — `false`.
    pub fn remove_path(&mut self, path: &str) -> Result<bool> {
        let trimmed = path.trim_matches('/');
        let (parent_path, name) = trimmed.rsplit_once('/').unwrap_or(("", trimmed));
        let parent = if parent_path.is_empty() {
            match &mut self.fs {
                Fs::Ext2(_) => u64::from(ext2::ROOT_INODE),
                Fs::Btrfs(writer) => writer.resolve(&mut self.dev, "/").map_err(|err| anyhow!("{err}"))?,
            }
        } else {
            match self.lookup_path(parent_path)? {
                Some(found) if found.directory => found.number,
                _ => return Ok(false),
            }
        };
        let Some(target) = self.lookup_path(trimmed)? else { return Ok(false) };
        let time = self.time;
        match &mut self.fs {
            Fs::Ext2(fs) => {
                let parent = parent as u32;
                if target.directory { fs.rmdir(&mut self.dev, parent, name) } else { fs.unlink(&mut self.dev, parent, name) }
                    .map_err(|err| anyhow!("не удалось стереть /{trimmed}: {err}"))?;
            }
            Fs::Btrfs(writer) => {
                if target.directory {
                    writer.remove_directory(&mut self.dev, parent, name, time)
                } else {
                    writer.unlink(&mut self.dev, parent, name, time)
                }
                .map_err(|err| anyhow!("не удалось стереть /{trimmed}: {err}"))?;
            }
        }
        self.changed()?;
        Ok(true)
    }

    /// Довести до носителя: ext2 — сброс и метка «закрыт чисто», btrfs —
    /// фиксация.
    pub fn finish(mut self) -> Result<()> {
        match &mut self.fs {
            Fs::Ext2(fs) => {
                fs.flush_everywhere(&mut self.dev).map_err(|err| anyhow!("не удалось сбросить раздел состояния: {err}"))?;
                fs.mark_clean(&mut self.dev).map_err(|err| anyhow!("не удалось пометить том чистым: {err}"))?;
            }
            Fs::Btrfs(writer) => {
                writer.commit(&mut self.dev).map_err(|err| anyhow!("фиксация раздела состояния не удалась: {err}"))?;
            }
        }
        self.dev.flush().map_err(|err| anyhow!("не удалось сбросить образ: {err}"))?;
        Ok(())
    }
}
