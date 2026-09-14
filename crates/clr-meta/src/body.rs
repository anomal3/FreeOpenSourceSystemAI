//! Тело метода на IL (ECMA-335 II.25.4): заголовок, код и таблица обработчиков
//! исключений.

use crate::Error;
use crate::bytes::{slice, u8_at, u16_at, u32_at};

/// Младшие два бита первого байта: крошечный заголовок.
const TINY: u8 = 0x2;
/// Младшие два бита первого байта: полный заголовок.
const FAT: u8 = 0x3;
/// Флаг полного заголовка: за кодом идут дополнительные разделы.
const MORE_SECTS: u16 = 0x08;
/// Флаг полного заголовка: локальные переменные обнуляются.
const INIT_LOCALS: u16 = 0x10;
/// Вид раздела: таблица обработчиков исключений.
const SECTION_EH_TABLE: u8 = 0x01;
/// Раздел в полной форме (длины и смещения по 32 бита).
const SECTION_FAT: u8 = 0x40;
/// За этим разделом есть ещё один.
const SECTION_MORE: u8 = 0x80;

/// Тело метода.
#[derive(Clone, Copy)]
pub struct MethodBody<'a> {
    /// Глубина стека вычислений, которую метод не превышает. У крошечного
    /// заголовка — восемь, так договорено форматом.
    pub max_stack: u16,
    /// Обнулять ли локальные переменные при входе. Компилятор C# ставит флаг
    /// всегда, если переменные есть (кроме `SkipLocalsInit`).
    pub init_locals: bool,
    /// Токен сигнатуры локальных переменных (`StandAloneSig`); ноль — их нет.
    pub local_signature: u32,
    /// Байты IL.
    pub code: &'a [u8],
    /// Дополнительные разделы после кода.
    sections: &'a [u8],
}

impl<'a> MethodBody<'a> {
    /// Разобрать тело, начинающееся с первого байта `data`.
    pub fn parse(data: &'a [u8]) -> Result<Self, Error> {
        let first = u8_at(data, 0).ok_or(Error::BadBody("empty"))?;
        match first & 0x3 {
            TINY => {
                let size = usize::from(first >> 2);
                let code = slice(data, 1, size).ok_or(Error::BadBody("tiny code"))?;
                Ok(Self { max_stack: 8, init_locals: false, local_signature: 0, code, sections: &[] })
            }
            FAT => {
                let flags_and_size = u16_at(data, 0).ok_or(Error::BadBody("fat header"))?;
                let flags = flags_and_size & 0x0FFF;
                let header = usize::from(flags_and_size >> 12) * 4;
                if header < 12 {
                    return Err(Error::BadBody("fat header size"));
                }
                let max_stack = u16_at(data, 2).ok_or(Error::BadBody("fat header"))?;
                let code_size = u32_at(data, 4).ok_or(Error::BadBody("fat header"))? as usize;
                let local_signature = u32_at(data, 8).ok_or(Error::BadBody("fat header"))?;
                let code = slice(data, header, code_size).ok_or(Error::BadBody("fat code"))?;
                let sections = if flags & MORE_SECTS != 0 {
                    // Разделы выровнены до четырёх байт от начала тела.
                    let at = (header + code_size).div_ceil(4) * 4;
                    data.get(at..).ok_or(Error::BadBody("sections"))?
                } else {
                    &[]
                };
                Ok(Self {
                    max_stack,
                    init_locals: flags & INIT_LOCALS != 0,
                    local_signature,
                    code,
                    sections,
                })
            }
            _ => Err(Error::BadBody("header kind")),
        }
    }

    /// Обработчики исключений по порядку, как их записал компилятор.
    #[must_use]
    pub fn clauses(&self) -> Clauses<'a> {
        Clauses {
            data: self.sections,
            cursor: 0,
            remaining: 0,
            fat: false,
            next_section: (!self.sections.is_empty()).then_some(0),
        }
    }
}

/// Один обработчик исключения.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExceptionClause {
    /// 0 — `catch` по типу, 1 — фильтр (`when`), 2 — `finally`, 4 — `fault`.
    pub flags: u32,
    pub try_offset: u32,
    pub try_length: u32,
    pub handler_offset: u32,
    pub handler_length: u32,
    /// Токен типа у `catch`, смещение фильтра у фильтра, у остальных — что
    /// записал компилятор (обычно ноль).
    pub class_or_filter: u32,
}

/// Вид обработчика: `catch` по типу.
pub const CLAUSE_CATCH: u32 = 0;
/// Вид обработчика: фильтр.
pub const CLAUSE_FILTER: u32 = 1;
/// Вид обработчика: `finally`.
pub const CLAUSE_FINALLY: u32 = 2;
/// Вид обработчика: `fault`.
pub const CLAUSE_FAULT: u32 = 4;

/// Обход обработчиков по всем разделам тела.
pub struct Clauses<'a> {
    data: &'a [u8],
    cursor: usize,
    remaining: usize,
    fat: bool,
    next_section: Option<usize>,
}

impl Iterator for Clauses<'_> {
    type Item = Result<ExceptionClause, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.remaining > 0 {
                self.remaining -= 1;
                let clause = self.read_clause();
                if clause.is_err() {
                    self.remaining = 0;
                    self.next_section = None;
                }
                return Some(clause);
            }
            let at = self.next_section?;
            let Some(kind) = u8_at(self.data, at) else {
                self.next_section = None;
                return Some(Err(Error::BadBody("section header")));
            };
            let fat = kind & SECTION_FAT != 0;
            let size = if fat {
                self.data
                    .get(at + 1..at + 4)
                    .map(|b| usize::from(b[0]) | (usize::from(b[1]) << 8) | (usize::from(b[2]) << 16))
            } else {
                u8_at(self.data, at + 1).map(usize::from)
            };
            let Some(size) = size.filter(|&size| size >= 4) else {
                self.next_section = None;
                return Some(Err(Error::BadBody("section size")));
            };
            self.next_section =
                (kind & SECTION_MORE != 0).then(|| (at + size).div_ceil(4) * 4);
            if kind & 0x3F == SECTION_EH_TABLE {
                let per_clause = if fat { 24 } else { 12 };
                self.fat = fat;
                self.cursor = at + 4;
                self.remaining = (size - 4) / per_clause;
            }
        }
    }
}

impl Clauses<'_> {
    fn read_clause(&mut self) -> Result<ExceptionClause, Error> {
        let at = self.cursor;
        let bad = Error::BadBody("exception clause");
        let clause = if self.fat {
            let field = |offset| u32_at(self.data, at + offset).ok_or(bad);
            self.cursor += 24;
            ExceptionClause {
                flags: field(0)?,
                try_offset: field(4)?,
                try_length: field(8)?,
                handler_offset: field(12)?,
                handler_length: field(16)?,
                class_or_filter: field(20)?,
            }
        } else {
            // Малая форма: смещения по 16 бит, длины по 8.
            let short = |offset| u16_at(self.data, at + offset).map(u32::from).ok_or(bad);
            let byte = |offset| u8_at(self.data, at + offset).map(u32::from).ok_or(bad);
            self.cursor += 12;
            ExceptionClause {
                flags: short(0)?,
                try_offset: short(2)?,
                try_length: byte(4)?,
                handler_offset: short(5)?,
                handler_length: byte(7)?,
                class_or_filter: u32_at(self.data, at + 8).ok_or(bad)?,
            }
        };
        Ok(clause)
    }
}
