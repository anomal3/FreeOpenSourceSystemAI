//! Имена и значения перечислений из метаданных (фаза N4d).
//!
//! Печать, флаги и разбор перечислений написаны на C# (`Enum` в
//! `tools/dotnet/corelib/Object.cs`); здесь — только то, что лежит в таблицах
//! сборки: литеральные поля с их значениями из таблицы `Constant` и атрибут
//! `[Flags]` из `CustomAttribute`.

use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::tables::{Coded, id};

use crate::types::{FIELD_LITERAL, FIELD_STATIC, Kind, Prim, TypeId};
use crate::vm::Vm;
use crate::{Host, VmError};

impl<H: Host> Vm<'_, H> {
    /// Базовый примитив перечисления.
    pub(crate) fn enum_prim(&self, ty: TypeId) -> Result<Prim, VmError> {
        match self.types[ty.0 as usize].kind {
            Kind::Enum(prim) => Ok(prim),
            _ => Err(self.exception("System.ArgumentException")),
        }
    }

    /// Имена и биты значений (расширенные нулями) по возрастанию значения без
    /// знака, как их упорядочивает .NET; равные — в порядке объявления.
    pub(crate) fn enum_members(&self, ty: TypeId) -> Result<Vec<(String, u64)>, VmError> {
        let prim = self.enum_prim(ty)?;
        let Some((asm, row)) = self.types[ty.0 as usize].def else {
            return Err(self.invalid("enum without a definition"));
        };
        let a = self.assembly(asm);
        let mut members = Vec::new();
        for field in a.tables.list(id::TYPE_DEF, row, 4, id::FIELD, id::FIELD_PTR)? {
            let field = field?;
            let flags = a.tables.column(id::FIELD, field, 0)? as u16;
            if flags & FIELD_STATIC == 0 || flags & FIELD_LITERAL == 0 {
                continue;
            }
            let name = a.root.strings.get(a.tables.column(id::FIELD, field, 1)?)?;
            let bits = self.field_constant(asm, field)? & enum_mask(prim);
            members.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
            members.push((String::from(name), bits));
        }
        members.sort_by_key(|(_, bits)| *bits);
        Ok(members)
    }

    /// Значение литерального поля — байты из `Constant`, от младшего.
    fn field_constant(&self, asm: crate::types::Asm, field: u32) -> Result<u64, VmError> {
        let a = self.assembly(asm);
        for row in 1..=a.tables.rows(id::CONSTANT) {
            let parent = a.tables.coded_column(id::CONSTANT, row, 2, Coded::HasConstant)?;
            if parent.table == id::FIELD && parent.row == field {
                let blob = a.root.blobs.get(a.tables.column(id::CONSTANT, row, 3)?)?;
                let mut raw = [0u8; 8];
                let length = blob.len().min(8);
                raw[..length].copy_from_slice(&blob[..length]);
                return Ok(u64::from_le_bytes(raw));
            }
        }
        Ok(0)
    }

    /// Помечено ли определение типа атрибутом `System.FlagsAttribute`.
    pub(crate) fn enum_is_flags(&mut self, ty: TypeId) -> Result<bool, VmError> {
        let Some((asm, row)) = self.types[ty.0 as usize].def else { return Ok(false) };
        // Конструктор атрибута — ссылка на член чужого типа (`TypeRef`) или
        // метод своего (`TypeDef`, его владельца ищет `owner_row`).
        let mut types = Vec::new();
        {
            let a = self.assembly(asm);
            for attribute in 1..=a.tables.rows(id::CUSTOM_ATTRIBUTE) {
                let parent = a.tables.coded_column(id::CUSTOM_ATTRIBUTE, attribute, 0, Coded::HasCustomAttribute)?;
                if parent.table != id::TYPE_DEF || parent.row != row {
                    continue;
                }
                let ctor = a.tables.coded_column(id::CUSTOM_ATTRIBUTE, attribute, 1, Coded::CustomAttributeType)?;
                match ctor.table {
                    id::MEMBER_REF => {
                        let class = a.tables.coded_column(id::MEMBER_REF, ctor.row, 0, Coded::MemberRefParent)?;
                        if class.table == id::TYPE_REF {
                            types.push((id::TYPE_REF, class.row));
                        }
                    }
                    id::METHOD_DEF => types.push((id::METHOD_DEF, ctor.row)),
                    _ => {}
                }
            }
        }
        for (table, found) in types {
            let (table, type_row) =
                if table == id::METHOD_DEF { (id::TYPE_DEF, self.owner_row(asm, found, false)?) } else { (table, found) };
            let a = self.assembly(asm);
            let name = a.root.strings.get(a.tables.column(table, type_row, 1)?)?;
            let namespace = a.root.strings.get(a.tables.column(table, type_row, 2)?)?;
            if namespace == "System" && name == "FlagsAttribute" {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Целый аргумент атрибута на определении типа — `[InlineArray(N)]`
    /// (фаза N11). Тот же обход, что у `[Flags]`, плюс значение из
    /// сгенерированного блоба: пролог 0x0001, затем `int32`.
    pub(crate) fn type_attribute_int(&mut self, ty: TypeId, namespace: &str, name: &str) -> Result<Option<u32>, VmError> {
        let Some((asm, row)) = self.types[ty.0 as usize].def else { return Ok(None) };
        let mut found = Vec::new();
        {
            let a = self.assembly(asm);
            for attribute in 1..=a.tables.rows(id::CUSTOM_ATTRIBUTE) {
                let parent = a.tables.coded_column(id::CUSTOM_ATTRIBUTE, attribute, 0, Coded::HasCustomAttribute)?;
                if parent.table != id::TYPE_DEF || parent.row != row {
                    continue;
                }
                let ctor = a.tables.coded_column(id::CUSTOM_ATTRIBUTE, attribute, 1, Coded::CustomAttributeType)?;
                let value = a.tables.column(id::CUSTOM_ATTRIBUTE, attribute, 2)?;
                match ctor.table {
                    id::MEMBER_REF => {
                        let class = a.tables.coded_column(id::MEMBER_REF, ctor.row, 0, Coded::MemberRefParent)?;
                        if class.table == id::TYPE_REF {
                            found.push((id::TYPE_REF, class.row, value));
                        }
                    }
                    id::METHOD_DEF => found.push((id::METHOD_DEF, ctor.row, value)),
                    _ => {}
                }
            }
        }
        for (table, candidate, value) in found {
            let (table, type_row) = if table == id::METHOD_DEF {
                (id::TYPE_DEF, self.owner_row(asm, candidate, false)?)
            } else {
                (table, candidate)
            };
            let a = self.assembly(asm);
            let found_name = a.root.strings.get(a.tables.column(table, type_row, 1)?)?;
            let found_namespace = a.root.strings.get(a.tables.column(table, type_row, 2)?)?;
            if found_namespace != namespace || found_name != name {
                continue;
            }
            let blob = a.root.blobs.get(value)?;
            if blob.len() < 6 || blob[0] != 1 || blob[1] != 0 {
                return Err(self.invalid("attribute value is not a single int32"));
            }
            return Ok(Some(u32::from_le_bytes([blob[2], blob[3], blob[4], blob[5]])));
        }
        Ok(None)
    }
}

/// Ширина базового типа в байтах; у знакового — со знаком минус.
pub(crate) fn enum_width(prim: Prim) -> i32 {
    let size = prim.size() as i32;
    match prim {
        Prim::I1 | Prim::I2 | Prim::I4 | Prim::I8 | Prim::I => -size,
        _ => size,
    }
}

/// Биты, которые помещаются в базовый тип.
pub(crate) fn enum_mask(prim: Prim) -> u64 {
    match prim.size() {
        8 => u64::MAX,
        size => (1u64 << (size * 8)) - 1,
    }
}
