//! Обёртка PE/COFF и заголовок CLI.
//!
//! Из всего формата PE сборке .NET нужны три вещи: таблица разделов (чтобы
//! переводить RVA в смещение в файле), каталог данных номер 14 (там лежит
//! заголовок CLI) и сам заголовок CLI. Импорты, релокации и ресурсы Windows
//! среде выполнения ни к чему — сборка не загружается загрузчиком Windows, её
//! читают как файл.

use crate::Error;
use crate::bytes::{slice, u16_at, u32_at};

/// Необязательный заголовок 32-битного образа.
const PE32: u16 = 0x10B;
/// Необязательный заголовок 64-битного образа.
const PE32_PLUS: u16 = 0x20B;
/// Номер каталога данных, в котором лежит заголовок CLI.
const CLI_DIRECTORY: usize = 14;
/// Размер записи в таблице разделов.
const SECTION_SIZE: usize = 40;
/// Размер заголовка CLI (ECMA-335 II.25.3.3).
pub const CLI_HEADER_SIZE: u32 = 72;

/// Один раздел образа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub raw_offset: u32,
    pub raw_size: u32,
}

/// Образ PE: где что лежит.
#[derive(Clone, Copy)]
pub struct Image<'a> {
    data: &'a [u8],
    sections_at: usize,
    section_count: usize,
    /// Машина из заголовка COFF. У сборок на IL это `0x14C` (i386) даже на
    /// 64-битной машине: код в них не машинный.
    pub machine: u16,
    pub pe32_plus: bool,
    pub cli_rva: u32,
    pub cli_size: u32,
}

impl<'a> Image<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        if data.get(0..2) != Some(b"MZ".as_slice()) {
            return Err(Error::BadMagic("MZ"));
        }
        let pe_at = u32_at(data, 0x3C).ok_or(Error::Truncated("DOS header"))? as usize;
        let signature = slice(data, pe_at, 4).ok_or(Error::Truncated("PE signature"))?;
        if signature != b"PE\0\0" {
            return Err(Error::BadMagic("PE"));
        }
        let coff = pe_at + 4;
        let machine = u16_at(data, coff).ok_or(Error::Truncated("COFF header"))?;
        let section_count =
            usize::from(u16_at(data, coff + 2).ok_or(Error::Truncated("COFF header"))?);
        let optional_size =
            usize::from(u16_at(data, coff + 16).ok_or(Error::Truncated("COFF header"))?);
        let optional = coff + 20;
        let magic = u16_at(data, optional).ok_or(Error::Truncated("optional header"))?;
        let pe32_plus = match magic {
            PE32 => false,
            PE32_PLUS => true,
            _ => return Err(Error::BadMagic("optional header")),
        };
        // Где в необязательном заголовке число каталогов и сами каталоги: у
        // 64-битного образа поле ImageBase шире на четыре байта, и всё после
        // него сдвинуто.
        let (count_at, directories_at) = if pe32_plus { (108, 112) } else { (92, 96) };
        let directory_count =
            u32_at(data, optional + count_at).ok_or(Error::Truncated("optional header"))? as usize;
        if directory_count <= CLI_DIRECTORY {
            return Err(Error::NotManaged);
        }
        if directories_at + (CLI_DIRECTORY + 1) * 8 > optional_size {
            return Err(Error::Truncated("data directories"));
        }
        let cli_at = optional + directories_at + CLI_DIRECTORY * 8;
        let cli_rva = u32_at(data, cli_at).ok_or(Error::Truncated("data directories"))?;
        let cli_size = u32_at(data, cli_at + 4).ok_or(Error::Truncated("data directories"))?;
        if cli_rva == 0 {
            return Err(Error::NotManaged);
        }
        let sections_at = optional + optional_size;
        let table_len = section_count
            .checked_mul(SECTION_SIZE)
            .ok_or(Error::Truncated("section table"))?;
        if slice(data, sections_at, table_len).is_none() {
            return Err(Error::Truncated("section table"));
        }
        Ok(Self { data, sections_at, section_count, machine, pe32_plus, cli_rva, cli_size })
    }

    /// Раздел по номеру.
    #[must_use]
    pub fn section(&self, index: usize) -> Option<Section> {
        if index >= self.section_count {
            return None;
        }
        let at = self.sections_at + index * SECTION_SIZE;
        Some(Section {
            virtual_size: u32_at(self.data, at + 8)?,
            virtual_address: u32_at(self.data, at + 12)?,
            raw_size: u32_at(self.data, at + 16)?,
            raw_offset: u32_at(self.data, at + 20)?,
        })
    }

    /// Раздел, в который попадает RVA, и смещение RVA внутри него.
    fn locate(&self, rva: u32) -> Option<(Section, u32)> {
        (0..self.section_count).filter_map(|i| self.section(i)).find_map(|section| {
            let delta = rva.checked_sub(section.virtual_address)?;
            // Размер раздела в памяти бывает и больше, и меньше сырого: хвост
            // `.bss` в файле не лежит, а сырой размер округлён до
            // выравнивания файла. Попадание считается по большему, а чтение —
            // только из сырых байтов.
            (delta < section.virtual_size.max(section.raw_size)).then_some((section, delta))
        })
    }

    /// Смещение в файле для RVA.
    #[must_use]
    pub fn offset_of(&self, rva: u32) -> Option<usize> {
        let (section, delta) = self.locate(rva)?;
        (delta < section.raw_size).then(|| section.raw_offset as usize + delta as usize)
    }

    /// Байты от RVA до конца сырых данных его раздела.
    ///
    /// Нужны телу метода: его длина становится известна только из его же
    /// заголовка.
    pub fn from_rva(&self, rva: u32) -> Result<&'a [u8], Error> {
        let (section, delta) = self.locate(rva).ok_or(Error::RvaOutside(rva))?;
        if delta >= section.raw_size {
            return Err(Error::RvaOutside(rva));
        }
        let start = section.raw_offset as usize + delta as usize;
        let end = (section.raw_offset as usize + section.raw_size as usize).min(self.data.len());
        self.data.get(start..end).ok_or(Error::RvaOutside(rva))
    }

    /// Ровно `len` байт по RVA.
    pub fn at_rva(&self, rva: u32, len: u32) -> Result<&'a [u8], Error> {
        self.from_rva(rva)?.get(..len as usize).ok_or(Error::RvaOutside(rva))
    }
}

/// Заголовок CLI (ECMA-335 II.25.3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CliHeader {
    pub runtime_major: u16,
    pub runtime_minor: u16,
    pub metadata_rva: u32,
    pub metadata_size: u32,
    pub flags: u32,
    /// Токен метода `Main` (или RVA машинной точки входа — у смешанных сборок,
    /// которых здесь не бывает).
    pub entry_point: u32,
    pub resources_rva: u32,
    pub resources_size: u32,
}

impl CliHeader {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let field = |at| u32_at(bytes, at).ok_or(Error::Truncated("CLI header"));
        let short = |at| u16_at(bytes, at).ok_or(Error::Truncated("CLI header"));
        if field(0)? < CLI_HEADER_SIZE {
            return Err(Error::BadMagic("CLI header size"));
        }
        Ok(Self {
            runtime_major: short(4)?,
            runtime_minor: short(6)?,
            metadata_rva: field(8)?,
            metadata_size: field(12)?,
            flags: field(16)?,
            entry_point: field(20)?,
            resources_rva: field(24)?,
            resources_size: field(28)?,
        })
    }
}
