//! Таблицы метаданных ECMA-335 (II.22) и их схема.
//!
//! # Почему схема, а не сорок пять структур
//!
//! Ширина столбца в этих таблицах не постоянна: индекс в кучу занимает два или
//! четыре байта в зависимости от размера кучи, индекс в таблицу — в зависимости
//! от числа строк в ней, а кодированный индекс — от числа строк в **самой
//! большой** из таблиц, на которые он может указывать. Поэтому смещение каждой
//! таблицы в потоке зависит от ширины всех предыдущих, и ошибка в одном
//! столбце одной таблицы сдвигает все таблицы после неё.
//!
//! Схема одной таблицей типов столбцов делает эту арифметику одной функцией на
//! все таблицы — и позволяет проверить её одним сравнением с чужим читателем:
//! размер строки и смещение каждой таблицы (`cargo xtask clr-check`).

use core::ops::Range;

use crate::Error;
use crate::bytes::{u8_at, u16_at, u32_at, u64_at};

/// Номера таблиц.
pub mod id {
    pub const MODULE: u8 = 0x00;
    pub const TYPE_REF: u8 = 0x01;
    pub const TYPE_DEF: u8 = 0x02;
    pub const FIELD_PTR: u8 = 0x03;
    pub const FIELD: u8 = 0x04;
    pub const METHOD_PTR: u8 = 0x05;
    pub const METHOD_DEF: u8 = 0x06;
    pub const PARAM_PTR: u8 = 0x07;
    pub const PARAM: u8 = 0x08;
    pub const INTERFACE_IMPL: u8 = 0x09;
    pub const MEMBER_REF: u8 = 0x0A;
    pub const CONSTANT: u8 = 0x0B;
    pub const CUSTOM_ATTRIBUTE: u8 = 0x0C;
    pub const FIELD_MARSHAL: u8 = 0x0D;
    pub const DECL_SECURITY: u8 = 0x0E;
    pub const CLASS_LAYOUT: u8 = 0x0F;
    pub const FIELD_LAYOUT: u8 = 0x10;
    pub const STAND_ALONE_SIG: u8 = 0x11;
    pub const EVENT_MAP: u8 = 0x12;
    pub const EVENT_PTR: u8 = 0x13;
    pub const EVENT: u8 = 0x14;
    pub const PROPERTY_MAP: u8 = 0x15;
    pub const PROPERTY_PTR: u8 = 0x16;
    pub const PROPERTY: u8 = 0x17;
    pub const METHOD_SEMANTICS: u8 = 0x18;
    pub const METHOD_IMPL: u8 = 0x19;
    pub const MODULE_REF: u8 = 0x1A;
    pub const TYPE_SPEC: u8 = 0x1B;
    pub const IMPL_MAP: u8 = 0x1C;
    pub const FIELD_RVA: u8 = 0x1D;
    pub const ENC_LOG: u8 = 0x1E;
    pub const ENC_MAP: u8 = 0x1F;
    pub const ASSEMBLY: u8 = 0x20;
    pub const ASSEMBLY_PROCESSOR: u8 = 0x21;
    pub const ASSEMBLY_OS: u8 = 0x22;
    pub const ASSEMBLY_REF: u8 = 0x23;
    pub const ASSEMBLY_REF_PROCESSOR: u8 = 0x24;
    pub const ASSEMBLY_REF_OS: u8 = 0x25;
    pub const FILE: u8 = 0x26;
    pub const EXPORTED_TYPE: u8 = 0x27;
    pub const MANIFEST_RESOURCE: u8 = 0x28;
    pub const NESTED_CLASS: u8 = 0x29;
    pub const GENERIC_PARAM: u8 = 0x2A;
    pub const METHOD_SPEC: u8 = 0x2B;
    pub const GENERIC_PARAM_CONSTRAINT: u8 = 0x2C;
    /// Псевдотаблица `#US` в токенах `ldstr`.
    pub const USER_STRING: u8 = 0x70;
}

/// Сколько таблиц описано в ECMA-335.
pub const TABLE_COUNT: usize = 0x2D;

/// Кодированный индекс (II.24.2.6): номер строки и номер таблицы из короткого
/// списка в одном числе.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coded {
    TypeDefOrRef,
    HasConstant,
    HasCustomAttribute,
    HasFieldMarshal,
    HasDeclSecurity,
    MemberRefParent,
    HasSemantics,
    MethodDefOrRef,
    MemberForwarded,
    Implementation,
    CustomAttributeType,
    ResolutionScope,
    TypeOrMethodDef,
}

/// Место в списке таблиц кодированного индекса, которое форматом не занято.
const UNUSED: u8 = 0xFF;

impl Coded {
    /// Сколько младших бит занимает номер таблицы.
    const fn bits(self) -> u32 {
        match self {
            Self::HasFieldMarshal
            | Self::HasSemantics
            | Self::MethodDefOrRef
            | Self::MemberForwarded
            | Self::TypeOrMethodDef => 1,
            Self::TypeDefOrRef
            | Self::HasConstant
            | Self::HasDeclSecurity
            | Self::Implementation
            | Self::ResolutionScope => 2,
            Self::MemberRefParent | Self::CustomAttributeType => 3,
            Self::HasCustomAttribute => 5,
        }
    }

    /// Таблицы в порядке их номеров внутри индекса.
    const fn tables(self) -> &'static [u8] {
        use id::*;
        match self {
            Self::TypeDefOrRef => &[TYPE_DEF, TYPE_REF, TYPE_SPEC],
            Self::HasConstant => &[FIELD, PARAM, PROPERTY],
            Self::HasCustomAttribute => &[
                METHOD_DEF,
                FIELD,
                TYPE_REF,
                TYPE_DEF,
                PARAM,
                INTERFACE_IMPL,
                MEMBER_REF,
                MODULE,
                DECL_SECURITY,
                PROPERTY,
                EVENT,
                STAND_ALONE_SIG,
                MODULE_REF,
                TYPE_SPEC,
                ASSEMBLY,
                ASSEMBLY_REF,
                FILE,
                EXPORTED_TYPE,
                MANIFEST_RESOURCE,
                GENERIC_PARAM,
                GENERIC_PARAM_CONSTRAINT,
                METHOD_SPEC,
            ],
            Self::HasFieldMarshal => &[FIELD, PARAM],
            Self::HasDeclSecurity => &[TYPE_DEF, METHOD_DEF, ASSEMBLY],
            Self::MemberRefParent => &[TYPE_DEF, TYPE_REF, MODULE_REF, METHOD_DEF, TYPE_SPEC],
            Self::HasSemantics => &[EVENT, PROPERTY],
            Self::MethodDefOrRef => &[METHOD_DEF, MEMBER_REF],
            Self::MemberForwarded => &[FIELD, METHOD_DEF],
            Self::Implementation => &[FILE, ASSEMBLY_REF, EXPORTED_TYPE],
            // Номера 0, 1 и 4 форматом зарезервированы и не используются.
            Self::CustomAttributeType => &[UNUSED, UNUSED, METHOD_DEF, MEMBER_REF, UNUSED],
            Self::ResolutionScope => &[MODULE, MODULE_REF, ASSEMBLY_REF, TYPE_REF],
            Self::TypeOrMethodDef => &[TYPE_DEF, METHOD_DEF],
        }
    }
}

/// Тип столбца.
#[derive(Clone, Copy)]
enum Col {
    U8,
    U16,
    U32,
    Str,
    Guid,
    Blob,
    /// Индекс в таблицу с этим номером.
    Index(u8),
    Coded(Coded),
}

/// Самый широкий столбец в строке: у `Assembly` и `AssemblyRef` их девять.
const MAX_COLUMNS: usize = 9;

/// Столбцы каждой таблицы по порядку (ECMA-335 II.22.2–II.22.39).
const SCHEMA: [&[Col]; TABLE_COUNT] = {
    use Col::{Blob, Guid, Index, Str, U8, U16, U32};
    use Coded as C;
    use id::*;
    [
        /* 0x00 Module */ &[U16, Str, Guid, Guid, Guid],
        /* 0x01 TypeRef */ &[Col::Coded(C::ResolutionScope), Str, Str],
        /* 0x02 TypeDef */
        &[U32, Str, Str, Col::Coded(C::TypeDefOrRef), Index(FIELD), Index(METHOD_DEF)],
        /* 0x03 FieldPtr */ &[Index(FIELD)],
        /* 0x04 Field */ &[U16, Str, Blob],
        /* 0x05 MethodPtr */ &[Index(METHOD_DEF)],
        /* 0x06 MethodDef */ &[U32, U16, U16, Str, Blob, Index(PARAM)],
        /* 0x07 ParamPtr */ &[Index(PARAM)],
        /* 0x08 Param */ &[U16, U16, Str],
        /* 0x09 InterfaceImpl */ &[Index(TYPE_DEF), Col::Coded(C::TypeDefOrRef)],
        /* 0x0A MemberRef */ &[Col::Coded(C::MemberRefParent), Str, Blob],
        /* 0x0B Constant */ &[U8, U8, Col::Coded(C::HasConstant), Blob],
        /* 0x0C CustomAttribute */
        &[Col::Coded(C::HasCustomAttribute), Col::Coded(C::CustomAttributeType), Blob],
        /* 0x0D FieldMarshal */ &[Col::Coded(C::HasFieldMarshal), Blob],
        /* 0x0E DeclSecurity */ &[U16, Col::Coded(C::HasDeclSecurity), Blob],
        /* 0x0F ClassLayout */ &[U16, U32, Index(TYPE_DEF)],
        /* 0x10 FieldLayout */ &[U32, Index(FIELD)],
        /* 0x11 StandAloneSig */ &[Blob],
        /* 0x12 EventMap */ &[Index(TYPE_DEF), Index(EVENT)],
        /* 0x13 EventPtr */ &[Index(EVENT)],
        /* 0x14 Event */ &[U16, Str, Col::Coded(C::TypeDefOrRef)],
        /* 0x15 PropertyMap */ &[Index(TYPE_DEF), Index(PROPERTY)],
        /* 0x16 PropertyPtr */ &[Index(PROPERTY)],
        /* 0x17 Property */ &[U16, Str, Blob],
        /* 0x18 MethodSemantics */ &[U16, Index(METHOD_DEF), Col::Coded(C::HasSemantics)],
        /* 0x19 MethodImpl */
        &[Index(TYPE_DEF), Col::Coded(C::MethodDefOrRef), Col::Coded(C::MethodDefOrRef)],
        /* 0x1A ModuleRef */ &[Str],
        /* 0x1B TypeSpec */ &[Blob],
        /* 0x1C ImplMap */ &[U16, Col::Coded(C::MemberForwarded), Str, Index(MODULE_REF)],
        /* 0x1D FieldRVA */ &[U32, Index(FIELD)],
        /* 0x1E EncLog */ &[U32, U32],
        /* 0x1F EncMap */ &[U32],
        /* 0x20 Assembly */ &[U32, U16, U16, U16, U16, U32, Blob, Str, Str],
        /* 0x21 AssemblyProcessor */ &[U32],
        /* 0x22 AssemblyOS */ &[U32, U32, U32],
        /* 0x23 AssemblyRef */ &[U16, U16, U16, U16, U32, Blob, Str, Str, Blob],
        /* 0x24 AssemblyRefProcessor */ &[U32, Index(ASSEMBLY_REF)],
        /* 0x25 AssemblyRefOS */ &[U32, U32, U32, Index(ASSEMBLY_REF)],
        /* 0x26 File */ &[U32, Str, Blob],
        /* 0x27 ExportedType */ &[U32, U32, Str, Str, Col::Coded(C::Implementation)],
        /* 0x28 ManifestResource */ &[U32, U32, Str, Col::Coded(C::Implementation)],
        /* 0x29 NestedClass */ &[Index(TYPE_DEF), Index(TYPE_DEF)],
        /* 0x2A GenericParam */ &[U16, U16, Col::Coded(C::TypeOrMethodDef), Str],
        /* 0x2B MethodSpec */ &[Col::Coded(C::MethodDefOrRef), Blob],
        /* 0x2C GenericParamConstraint */ &[Index(GENERIC_PARAM), Col::Coded(C::TypeDefOrRef)],
    ]
};

/// Ссылка на строку таблицы — то же, что токен .NET: номер таблицы в старшем
/// байте, номер строки в младших трёх. Строка ноль — «ничего».
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Token {
    pub table: u8,
    pub row: u32,
}

impl Token {
    #[must_use]
    pub const fn from_value(value: u32) -> Self {
        Self { table: (value >> 24) as u8, row: value & 0x00FF_FFFF }
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        ((self.table as u32) << 24) | self.row
    }

    #[must_use]
    pub const fn is_nil(self) -> bool {
        self.row == 0
    }
}

/// Поток таблиц: сколько строк в каждой, какой ширины столбцы, где что лежит.
#[derive(Clone, Copy)]
pub struct Tables<'a> {
    data: &'a [u8],
    /// Где поток начинается от начала метаданных.
    stream_offset: usize,
    pub major: u8,
    pub minor: u8,
    pub heap_sizes: u8,
    pub valid: u64,
    pub sorted: u64,
    rows: [u32; TABLE_COUNT],
    offsets: [usize; TABLE_COUNT],
    row_sizes: [usize; TABLE_COUNT],
    widths: [[u8; MAX_COLUMNS]; TABLE_COUNT],
}

/// [`Tables::heap_sizes`]: индексы в `#Strings` четырёхбайтные.
const HEAP_STRINGS_WIDE: u8 = 0x01;
/// [`Tables::heap_sizes`]: индексы в `#GUID` четырёхбайтные.
const HEAP_GUID_WIDE: u8 = 0x02;
/// [`Tables::heap_sizes`]: индексы в `#Blob` четырёхбайтные.
const HEAP_BLOB_WIDE: u8 = 0x04;
/// [`Tables::heap_sizes`]: за числами строк лежат ещё четыре байта.
const HEAP_EXTRA_DATA: u8 = 0x40;

impl<'a> Tables<'a> {
    pub fn parse(data: &'a [u8], stream_offset: usize) -> Result<Self, Error> {
        let truncated = Error::Truncated("tables header");
        let major = u8_at(data, 4).ok_or(truncated)?;
        let minor = u8_at(data, 5).ok_or(truncated)?;
        let heap_sizes = u8_at(data, 6).ok_or(truncated)?;
        let valid = u64_at(data, 8).ok_or(truncated)?;
        let sorted = u64_at(data, 16).ok_or(truncated)?;

        let mut rows = [0u32; TABLE_COUNT];
        let mut at = 24usize;
        for bit in 0..64 {
            if valid & (1u64 << bit) == 0 {
                continue;
            }
            // Таблицы за пределами ECMA-335 (у переносимых PDB они с 0x30)
            // в сборке встречаться не должны. Молча пропустить их нельзя: их
            // ширину мы не знаем, и все смещения дальше были бы ложью.
            if bit >= TABLE_COUNT {
                return Err(Error::UnknownTable(bit as u8));
            }
            rows[bit] = u32_at(data, at).ok_or(Error::Truncated("row counts"))?;
            at += 4;
        }
        if heap_sizes & HEAP_EXTRA_DATA != 0 {
            at += 4;
        }

        let mut tables = Self {
            data,
            stream_offset,
            major,
            minor,
            heap_sizes,
            valid,
            sorted,
            rows,
            offsets: [0; TABLE_COUNT],
            row_sizes: [0; TABLE_COUNT],
            widths: [[0; MAX_COLUMNS]; TABLE_COUNT],
        };

        for table in 0..TABLE_COUNT {
            let mut size = 0usize;
            for (column, col) in SCHEMA[table].iter().enumerate() {
                let width = tables.width(*col);
                tables.widths[table][column] = width;
                size += usize::from(width);
            }
            tables.row_sizes[table] = size;
            tables.offsets[table] = at;
            at = size
                .checked_mul(rows[table] as usize)
                .and_then(|bytes| at.checked_add(bytes))
                .ok_or(Error::Truncated("tables"))?;
        }
        if at > data.len() {
            return Err(Error::Truncated("tables"));
        }
        Ok(tables)
    }

    /// Ширина столбца в байтах для этих метаданных.
    fn width(&self, col: Col) -> u8 {
        let wide = |flag| if self.heap_sizes & flag != 0 { 4 } else { 2 };
        match col {
            Col::U8 => 1,
            Col::U16 => 2,
            Col::U32 => 4,
            Col::Str => wide(HEAP_STRINGS_WIDE),
            Col::Guid => wide(HEAP_GUID_WIDE),
            Col::Blob => wide(HEAP_BLOB_WIDE),
            Col::Index(table) => {
                if self.rows[usize::from(table)] < 0x1_0000 { 2 } else { 4 }
            }
            Col::Coded(kind) => {
                let most = kind
                    .tables()
                    .iter()
                    .filter(|&&table| table != UNUSED)
                    .map(|&table| self.rows[usize::from(table)])
                    .max()
                    .unwrap_or(0);
                // Два байта, пока номер строки помещается в то, что осталось
                // от шестнадцати бит после номера таблицы.
                if most < (1u32 << (16 - kind.bits())) { 2 } else { 4 }
            }
        }
    }

    /// Сколько строк в таблице.
    #[must_use]
    pub fn rows(&self, table: u8) -> u32 {
        self.rows.get(usize::from(table)).copied().unwrap_or(0)
    }

    /// Размер строки таблицы в байтах.
    #[must_use]
    pub fn row_size(&self, table: u8) -> usize {
        self.row_sizes.get(usize::from(table)).copied().unwrap_or(0)
    }

    /// Где таблица начинается — от начала метаданных, как считает
    /// `MetadataReader.GetTableMetadataOffset`.
    #[must_use]
    pub fn metadata_offset(&self, table: u8) -> usize {
        self.stream_offset + self.offsets.get(usize::from(table)).copied().unwrap_or(0)
    }

    /// Сколько столбцов у таблицы.
    #[must_use]
    pub fn column_count(table: u8) -> usize {
        SCHEMA.get(usize::from(table)).map_or(0, |cols| cols.len())
    }

    /// Значение столбца как число. Строки нумеруются с единицы.
    ///
    /// Индексы в кучи и таблицы отдаются сырыми, кодированные — тоже (их
    /// раскладывает [`Tables::coded`]): так одна функция служит всем таблицам.
    pub fn column(&self, table: u8, row: u32, column: usize) -> Result<u32, Error> {
        let t = usize::from(table);
        let cols = SCHEMA.get(t).ok_or(Error::BadIndex("table"))?;
        if column >= cols.len() {
            return Err(Error::BadIndex("column"));
        }
        if row == 0 || row > self.rows[t] {
            return Err(Error::BadIndex("row"));
        }
        let skip: usize = self.widths[t][..column].iter().map(|&w| usize::from(w)).sum();
        let at = self.offsets[t] + (row as usize - 1) * self.row_sizes[t] + skip;
        let value = match self.widths[t][column] {
            1 => u8_at(self.data, at).map(u32::from),
            2 => u16_at(self.data, at).map(u32::from),
            _ => u32_at(self.data, at),
        };
        value.ok_or(Error::Truncated("table row"))
    }

    /// Разложить кодированный индекс на таблицу и строку.
    pub fn coded(kind: Coded, raw: u32) -> Result<Token, Error> {
        let bits = kind.bits();
        let tag = (raw & ((1 << bits) - 1)) as usize;
        let table = *kind.tables().get(tag).ok_or(Error::BadIndex("coded index tag"))?;
        if table == UNUSED {
            return Err(Error::BadIndex("coded index tag"));
        }
        Ok(Token { table, row: raw >> bits })
    }

    /// Прочитать столбец-кодированный индекс и сразу разложить.
    pub fn coded_column(&self, table: u8, row: u32, column: usize, kind: Coded) -> Result<Token, Error> {
        Self::coded(kind, self.column(table, row, column)?)
    }

    /// Строки, принадлежащие владельцу: `TypeDef.FieldList`, `TypeDef.MethodList`,
    /// `MethodDef.ParamList`, `EventMap.EventList`, `PropertyMap.PropertyList`.
    ///
    /// Список идёт от своего индекса до индекса следующей строки владельца, а у
    /// последней — до конца целевой таблицы. Возвращаются номера строк самой
    /// целевой таблицы: если метаданные несжатые и есть таблица-указатель,
    /// номера уже пропущены через неё.
    pub fn list(
        &self,
        owner: u8,
        row: u32,
        column: usize,
        target: u8,
        pointer: u8,
    ) -> Result<ListRows<'_, 'a>, Error> {
        let through = if self.rows(pointer) > 0 { pointer } else { target };
        let limit = self.rows(through) + 1;
        let start = self.column(owner, row, column)?.min(limit);
        let end = if row < self.rows(owner) {
            self.column(owner, row + 1, column)?.min(limit)
        } else {
            limit
        };
        Ok(ListRows {
            tables: self,
            range: start..end.max(start),
            pointer: (through == pointer).then_some(pointer),
        })
    }
}

/// Строки списка владельца — см. [`Tables::list`].
pub struct ListRows<'t, 'a> {
    tables: &'t Tables<'a>,
    range: Range<u32>,
    pointer: Option<u8>,
}

impl ListRows<'_, '_> {
    #[must_use]
    pub fn len(&self) -> usize {
        self.range.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.range.is_empty()
    }
}

impl Iterator for ListRows<'_, '_> {
    type Item = Result<u32, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let index = self.range.next()?;
        Some(match self.pointer {
            Some(pointer) => self.tables.column(pointer, index, 0),
            None => Ok(index),
        })
    }
}
