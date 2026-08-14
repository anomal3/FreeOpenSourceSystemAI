//! Разбор ELF64 — ровно столько, сколько нужно, чтобы загрузить программу.
//!
//! # Что читается и что нет
//!
//! Читаются заголовок файла и таблица заголовков программы (`PT_LOAD`). Не
//! читаются: таблица секций (она нужна компоновщику, а не загрузчику), символы,
//! отладочная информация, динамические таблицы и релокации.
//!
//! Релокаций нет не по недосмотру: программа собрана как `ET_EXEC` по
//! фиксированному адресу, и переносить в ней нечего. Загрузка по произвольному
//! адресу (`ET_DYN`) потребовала бы разбора `.rela.dyn` — и появится вместе с
//! отдельным адресным пространством, где в этом будет смысл.
//!
//! # Данные здесь недоверенные
//!
//! Файл прочитан с носителя, то есть пришёл из-за границы доверия. Любое поле
//! может быть любым, включая такие, от которых арифметика переполняется, а
//! срез выходит за буфер. Поэтому каждое смещение проверяется, а сложения —
//! `checked_*`: ошибка обязана стать [`ElfError`], а не отказом страницы внутри
//! ядра.

use core::fmt;

/// Сигнатура файла: `\x7FELF`.
const MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// Класс: 64 бита.
const CLASS_64: u8 = 2;
/// Порядок байт: младший первым.
const DATA_LE: u8 = 1;
/// Тип файла: исполняемый.
const TYPE_EXEC: u16 = 2;

/// Машина, для которой собран файл.
#[cfg(target_arch = "x86_64")]
const MACHINE: u16 = 62;
#[cfg(target_arch = "aarch64")]
const MACHINE: u16 = 183;

/// Тип заголовка программы: загружаемый сегмент.
const PT_LOAD: u32 = 1;

/// Биты `p_flags` program header'а. Порядок обратен привычному по `readelf`
/// написанию «RWX» и обратен конвенции [`boot_info`] — см. [`Segment::flags`].
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// Длина заголовка файла.
const EHDR_LEN: usize = 64;
/// Длина одного заголовка программы.
const PHDR_LEN: usize = 56;

/// Почему файл не удалось загрузить.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// Файл короче заголовка или обрезан на таблице сегментов.
    Truncated,
    /// Не ELF: сигнатура другая.
    NotElf,
    /// ELF, но не тот: 32 бита, big-endian, не исполняемый, чужая машина.
    Unsupported,
    /// Сегмент не помещается в окно, отведённое под программу.
    OutOfWindow,
    /// Заголовок сегмента противоречив: файловая часть длиннее памяти,
    /// смещение за концом файла, переполнение при сложении.
    BadSegment,
    /// Сегментов, которые нужно загрузить, в файле нет.
    NoSegments,
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("the file is truncated"),
            Self::NotElf => f.write_str("not an ELF file"),
            Self::Unsupported => {
                f.write_str("not a 64-bit little-endian executable for this machine")
            }
            Self::OutOfWindow => f.write_str("a segment falls outside the program window"),
            Self::BadSegment => f.write_str("a program header contradicts itself"),
            Self::NoSegments => f.write_str("the file has no loadable segments"),
        }
    }
}

/// Загружаемый сегмент в том виде, в каком он нужен загрузчику.
#[derive(Clone, Copy, Debug)]
pub struct Segment {
    /// Куда класть.
    pub vaddr: usize,
    /// Сколько занимает в памяти.
    pub memsz: usize,
    /// Что копировать: срез внутри файла.
    pub file_offset: usize,
    pub filesz: usize,
    /// Права в конвенции [`boot_info`]: `SEG_READ`, `SEG_WRITE`, `SEG_EXEC`.
    ///
    /// **Не** `p_flags` из файла, хотя разбираются они именно оттуда. Обе
    /// конвенции — по три бита, и обе называют их одинаковыми словами, но
    /// порядок битов у них обратный: в ELF `PF_X = 1`, а в `boot_info`
    /// `SEG_READ = 1`. Отдать наружу сырое число значило бы разложить сегмент
    /// «только чтение» исполняемым, а сегмент «чтение и запись» — исполняемым и
    /// записываемым одновременно. Перевод делает [`seg_flags`], и делает его
    /// один раз — здесь, на границе разбора.
    pub flags: u32,
}

/// Перевести `p_flags` в конвенцию [`boot_info`].
///
/// Тот же перевод делает загрузчик для сегментов ядра (`boot-uefi::elf`), и по
/// той же причине: две трёхбитные записи, называющие биты одинаковыми словами в
/// разном порядке, — это ошибка, которую не видно глазом и которую нечем
/// поймать, кроме проверки прав в момент отображения.
const fn seg_flags(p_flags: u32) -> u32 {
    let mut flags = 0;
    if p_flags & PF_R != 0 {
        flags |= boot_info::SEG_READ;
    }
    if p_flags & PF_W != 0 {
        flags |= boot_info::SEG_WRITE;
    }
    if p_flags & PF_X != 0 {
        flags |= boot_info::SEG_EXEC;
    }
    flags
}

/// Разобранный файл.
pub struct Image<'a> {
    bytes: &'a [u8],
    pub entry: usize,
    /// Смещение таблицы заголовков программы и её геометрия.
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

impl<'a> Image<'a> {
    /// Разобрать заголовок.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ElfError> {
        if bytes.len() < EHDR_LEN {
            return Err(ElfError::Truncated);
        }
        if bytes[0..4] != MAGIC {
            return Err(ElfError::NotElf);
        }
        if bytes[4] != CLASS_64 || bytes[5] != DATA_LE {
            return Err(ElfError::Unsupported);
        }
        let kind = u16(bytes, 16);
        let machine = u16(bytes, 18);
        if kind != TYPE_EXEC || machine != MACHINE {
            return Err(ElfError::Unsupported);
        }

        let entry = u64(bytes, 24) as usize;
        let phoff = u64(bytes, 32) as usize;
        let phentsize = usize::from(u16(bytes, 54));
        let phnum = usize::from(u16(bytes, 56));

        // Размер записи проверяется, а не предполагается: разбирать таблицу с
        // чужим шагом — значит читать поля из середины соседних записей.
        if phentsize < PHDR_LEN {
            return Err(ElfError::Unsupported);
        }
        let table_len = phentsize.checked_mul(phnum).ok_or(ElfError::Truncated)?;
        let table_end = phoff.checked_add(table_len).ok_or(ElfError::Truncated)?;
        if table_end > bytes.len() {
            return Err(ElfError::Truncated);
        }

        Ok(Self { bytes, entry, phoff, phentsize, phnum })
    }

    /// Перебрать загружаемые сегменты.
    ///
    /// `window` — диапазон адресов, в который программе разрешено попадать.
    /// Проверка здесь, а не у вызывающего: сегмент, вылезший за окно, — это
    /// запись мимо отведённой памяти, и обнаружить её надо до записи.
    pub fn segments(
        &self,
        window: (usize, usize),
    ) -> impl Iterator<Item = Result<Segment, ElfError>> + '_ {
        (0..self.phnum).filter_map(move |index| {
            let at = self.phoff + index * self.phentsize;
            let bytes = self.bytes;
            if u32(bytes, at) != PT_LOAD {
                return None;
            }

            let flags = u32(bytes, at + 4);
            let file_offset = u64(bytes, at + 8) as usize;
            let vaddr = u64(bytes, at + 16) as usize;
            let filesz = u64(bytes, at + 32) as usize;
            let memsz = u64(bytes, at + 40) as usize;

            // Пустой сегмент — законная запись (например, `.bss` нулевой
            // длины), и пропускать его надо тихо.
            if memsz == 0 {
                return None;
            }
            if filesz > memsz {
                return Some(Err(ElfError::BadSegment));
            }
            let Some(file_end) = file_offset.checked_add(filesz) else {
                return Some(Err(ElfError::BadSegment));
            };
            if file_end > bytes.len() {
                return Some(Err(ElfError::BadSegment));
            }
            let Some(mem_end) = vaddr.checked_add(memsz) else {
                return Some(Err(ElfError::BadSegment));
            };
            if vaddr < window.0 || mem_end > window.1 {
                return Some(Err(ElfError::OutOfWindow));
            }

            Some(Ok(Segment {
                vaddr,
                memsz,
                file_offset,
                filesz,
                flags: seg_flags(flags),
            }))
        })
    }

    /// Байты файла — из них загрузчик копирует сегменты.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

fn u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}
