//! Загрузка типов и методов, разрешение токенов (фаза N3a).
//!
//! # Разрешение по имени
//!
//! Ссылка программы `[System.Runtime]System.Object` не ищется в
//! `System.Runtime`: такой сборки у среды нет. Тип ищется по полному имени в
//! базовой библиотеке (`FreeOs.CoreLib`), член — по имени и сигнатуре,
//! записанной словами (`WriteLine(string)void`). Сборка, из которой пришла
//! ссылка, называется только в сообщении о том, чего не нашлось: так видно, что
//! дописать в библиотеку.
//!
//! # Сигнатуры словами
//!
//! Токены в сигнатурах у каждой сборки свои, и побайтно сигнатуры программы и
//! библиотеки не сравнить. Имена типов общие — поэтому ключ метода — это его
//! сигнатура, записанная `clr_meta::sig::write_type`. Обобщённые параметры
//! остаются `!0`: ссылка на `List<!0>::Add(!0)` и определение `Add(!0)` пишутся
//! одинаково. Подставлять их нужно только при сравнении с чужой таблицей
//! виртуальных методов (см. `dispatch.rs`).

use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use clr_meta::sig::{self, TypeNames, elem};
use clr_meta::tables::id;
use clr_meta::{Assembly, Coded, Token, compressed_u32};

use crate::natives;
use crate::types::{
    Asm, Body, CORELIB, FIELD_LITERAL, FIELD_STATIC, FieldRef, IMPL_INTERNAL_CALL, Kind, MethodId,
    MethodInfo, PROGRAM, Prim, Resolved, Store, TYPE_INTERFACE, Type, TypeId,
};
use crate::vm::Vm;
use crate::{Host, VmError};

/// Самая глубокая цепочка загрузки типов (база базы, тип поля-структуры…).
///
/// Метаданные пришли из файла, и зацикленное наследование иначе исчерпало бы
/// стек программы.
const MAX_LOAD_DEPTH: u32 = 64;

/// Самая глубокая вложенность типа в сигнатуре.
const MAX_SIG_DEPTH: u32 = 32;

/// Тип, записанный в сигнатуре.
#[derive(Clone, Copy)]
pub(crate) enum SigType {
    Void,
    Type(TypeId),
    ByRef,
    Pointer,
}

/// Имена типов в сигнатуре — с подстановкой параметров типа или без.
struct Names<'x, 'a> {
    asm: Assembly<'a>,
    vars: Option<&'x [String]>,
}

impl TypeNames for Names<'_, '_> {
    fn write_name(&self, token: Token, out: &mut dyn core::fmt::Write) -> Result<(), clr_meta::Error> {
        self.asm.write_type_name(token, out, 0)
    }

    fn write_var(&self, number: u32, method: bool, out: &mut dyn core::fmt::Write) -> Result<(), clr_meta::Error> {
        match self.vars.and_then(|vars| vars.get(number as usize)).filter(|_| !method) {
            Some(name) => out.write_str(name).map_err(|_| clr_meta::Error::BadSignature("cannot format")),
            None => {
                let bang = if method { "!!" } else { "!" };
                write!(out, "{bang}{number}").map_err(|_| clr_meta::Error::BadSignature("cannot format"))
            }
        }
    }
}

/// Ключ метода: `Name<2>(int32,!0)string`.
pub(crate) fn signature_key(
    asm: Assembly<'_>,
    name: &str,
    blob: &[u8],
    vars: Option<&[String]>,
) -> Result<String, VmError> {
    let header = sig::method_header(blob)?;
    let names = Names { asm, vars };
    let mut key = String::from(name);
    if header.generic_params > 0 {
        let _ = write!(key, "<{}>", header.generic_params);
    }
    key.push('(');
    sig::write_params(blob, &names, &mut key)?;
    key.push(')');
    sig::write_type(blob, header.used, &names, &mut key)?;
    Ok(key)
}

fn read_u(blob: &[u8], at: &mut usize) -> Result<u32, VmError> {
    let (value, used) = blob
        .get(*at..)
        .and_then(compressed_u32)
        .ok_or(VmError::Meta(clr_meta::Error::BadSignature("truncated")))?;
    *at += used;
    Ok(value)
}

impl<'a, H: Host> Vm<'a, H> {
    pub(crate) fn assembly(&self, asm: Asm) -> Assembly<'a> {
        self.asms[usize::from(asm)]
    }

    // --------------------------------------------------------------------
    // Имена
    // --------------------------------------------------------------------

    /// Пространство имён и имя строки `TypeDef`.
    pub(crate) fn def_names(&self, asm: Asm, row: u32) -> Result<(&'a str, &'a str), VmError> {
        let a = self.assembly(asm);
        let name = a.root.strings.get(a.tables.column(id::TYPE_DEF, row, 1)?)?;
        let namespace = a.root.strings.get(a.tables.column(id::TYPE_DEF, row, 2)?)?;
        Ok((namespace, name))
    }

    /// Полное имя определения, как у `Type.FullName`: вложенный тип — через `+`.
    fn def_full_name(&self, asm: Asm, row: u32, depth: u32) -> Result<String, VmError> {
        if depth > MAX_LOAD_DEPTH {
            return Err(VmError::Meta(clr_meta::Error::BadIndex("type nesting")));
        }
        let (namespace, name) = self.def_names(asm, row)?;
        let mut out = match self.assembly(asm).enclosing_type(row)? {
            Some(outer) => {
                let mut outer = self.def_full_name(asm, outer, depth + 1)?;
                outer.push('+');
                outer
            }
            None if !namespace.is_empty() => format!("{namespace}."),
            None => String::new(),
        };
        out.push_str(name);
        Ok(out)
    }

    /// Имя типа в записи сигнатуры: `int32`, `string`, `FreeOs.Point`,
    /// `System.Collections.Generic.List`1<int32>`.
    pub(crate) fn sig_name(&self, ty: TypeId) -> String {
        let t = &self.types[ty.0 as usize];
        match t.kind {
            Kind::Prim(p) => return String::from(p.sig_name()),
            Kind::Array(element) => {
                let mut name = self.sig_name(element);
                name.push_str("[]");
                return name;
            }
            _ => {}
        }
        let Some((asm, row)) = t.def else { return t.name.clone() };
        if asm == CORELIB {
            match self.def_names(asm, row) {
                Ok(("System", "String")) => return String::from("string"),
                Ok(("System", "Object")) => return String::from("object"),
                _ => {}
            }
        }
        let mut name = String::new();
        let _ = self.assembly(asm).write_type_name(Token { table: id::TYPE_DEF, row }, &mut name, 0);
        if !t.args.is_empty() {
            name.push('<');
            for (index, arg) in t.args.iter().enumerate() {
                if index > 0 {
                    name.push(',');
                }
                name.push_str(&self.sig_name(*arg));
            }
            name.push('>');
        }
        name
    }

    pub(crate) fn var_names(&self, args: &[TypeId]) -> Vec<String> {
        args.iter().map(|arg| self.sig_name(*arg)).collect()
    }

    pub(crate) fn method_name(&self, method: MethodId) -> String {
        let info = &self.methods[method.0 as usize];
        format!("{}::{}", self.types[info.owner.0 as usize].name, info.name)
    }

    fn assembly_name(&self, asm: Asm) -> String {
        let a = self.assembly(asm);
        match a.tables.column(id::ASSEMBLY, 1, 7).and_then(|index| a.root.strings.get(index)) {
            Ok(name) => String::from(name),
            Err(_) => String::from("the program"),
        }
    }

    // --------------------------------------------------------------------
    // Поиск определений
    // --------------------------------------------------------------------

    /// Тип верхнего уровня по имени.
    fn find_type_def(&self, asm: Asm, namespace: &str, name: &str) -> Result<Option<u32>, VmError> {
        let a = self.assembly(asm);
        for row in 1..=a.tables.rows(id::TYPE_DEF) {
            if self.def_names(asm, row)? == (namespace, name) && a.enclosing_type(row)?.is_none() {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    /// Во что указывает строка `TypeRef`.
    fn resolve_type_ref(&mut self, asm: Asm, row: u32, depth: u32) -> Result<(Asm, u32), VmError> {
        if let Some(found) = self.typerefs.get(&(asm, row)) {
            return Ok(*found);
        }
        if depth > MAX_LOAD_DEPTH {
            return Err(VmError::Meta(clr_meta::Error::BadIndex("type reference nesting")));
        }
        let a = self.assembly(asm);
        let scope = a.tables.coded_column(id::TYPE_REF, row, 0, Coded::ResolutionScope)?;
        let name = a.root.strings.get(a.tables.column(id::TYPE_REF, row, 1)?)?;
        let namespace = a.root.strings.get(a.tables.column(id::TYPE_REF, row, 2)?)?;
        let found = match scope.table {
            id::TYPE_REF if !scope.is_nil() => {
                let (outer_asm, outer) = self.resolve_type_ref(asm, scope.row, depth + 1)?;
                let o = self.assembly(outer_asm);
                let mut hit = None;
                for nested in 1..=o.tables.rows(id::NESTED_CLASS) {
                    if o.tables.column(id::NESTED_CLASS, nested, 1)? == outer {
                        let inner = o.tables.column(id::NESTED_CLASS, nested, 0)?;
                        if self.def_names(outer_asm, inner)?.1 == name {
                            hit = Some((outer_asm, inner));
                            break;
                        }
                    }
                }
                hit
            }
            id::MODULE => self.find_type_def(asm, namespace, name)?.map(|r| (asm, r)),
            // Всё, что снаружи сборки, ищется в базовой библиотеке по имени.
            _ => self.find_type_def(CORELIB, namespace, name)?.map(|r| (CORELIB, r)),
        };
        let Some(found) = found else {
            let mut full = String::new();
            let _ = a.write_type_name(Token { table: id::TYPE_REF, row }, &mut full, 0);
            let from = if scope.table == id::ASSEMBLY_REF {
                String::from(a.root.strings.get(a.tables.column(id::ASSEMBLY_REF, scope.row, 6)?)?)
            } else {
                self.assembly_name(asm)
            };
            return Err(VmError::MissingType { name: format!("{full} from {from}") });
        };
        self.typerefs.insert((asm, row), found);
        Ok(found)
    }

    /// Строка `TypeDef`, которой принадлежит метод или поле.
    pub(crate) fn owner_row(&mut self, asm: Asm, row: u32, field: bool) -> Result<u32, VmError> {
        let slot = usize::from(asm) * 2 + usize::from(field);
        if self.owners[slot].is_empty() {
            let a = self.assembly(asm);
            let (table, column, pointer) =
                if field { (id::FIELD, 4, id::FIELD_PTR) } else { (id::METHOD_DEF, 5, id::METHOD_PTR) };
            let mut owners = Vec::new();
            owners.try_reserve_exact(a.tables.rows(table) as usize + 1).map_err(|_| VmError::OutOfMemory)?;
            owners.resize(a.tables.rows(table) as usize + 1, 0);
            for type_row in 1..=a.tables.rows(id::TYPE_DEF) {
                for member in a.tables.list(id::TYPE_DEF, type_row, column, table, pointer)? {
                    if let Some(owner) = owners.get_mut(member? as usize) {
                        *owner = type_row;
                    }
                }
            }
            self.owners[slot] = owners;
        }
        match self.owners[slot].get(row as usize) {
            Some(&owner) if owner != 0 => Ok(owner),
            _ => Err(VmError::Meta(clr_meta::Error::BadIndex("member without an owner type"))),
        }
    }

    // --------------------------------------------------------------------
    // Типы
    // --------------------------------------------------------------------

    /// Тип базовой библиотеки по полному имени: `System.Int32`.
    pub(crate) fn corelib_type(&mut self, full: &'static str) -> Result<TypeId, VmError> {
        if let Some(ty) = self.corelib_types.get(full) {
            return Ok(*ty);
        }
        let (namespace, name) = full.rsplit_once('.').unwrap_or(("", full));
        let Some(row) = self.find_type_def(CORELIB, namespace, name)? else {
            return Err(VmError::MissingType { name: format!("{full} from the base library") });
        };
        let ty = self.load_def(CORELIB, row, Rc::from([]))?;
        self.corelib_types.insert(full, ty);
        Ok(ty)
    }

    pub(crate) fn prim_type(&mut self, prim: Prim) -> Result<TypeId, VmError> {
        let full: &'static str = match prim {
            Prim::Bool => "System.Boolean",
            Prim::Char => "System.Char",
            Prim::I1 => "System.SByte",
            Prim::U1 => "System.Byte",
            Prim::I2 => "System.Int16",
            Prim::U2 => "System.UInt16",
            Prim::I4 => "System.Int32",
            Prim::U4 => "System.UInt32",
            Prim::I8 => "System.Int64",
            Prim::U8 => "System.UInt64",
            Prim::I => "System.IntPtr",
            Prim::U => "System.UIntPtr",
            Prim::R4 => "System.Single",
            Prim::R8 => "System.Double",
        };
        self.corelib_type(full)
    }

    /// Тип одномерного массива с этим элементом.
    pub(crate) fn array_of(&mut self, element: TypeId) -> Result<TypeId, VmError> {
        if let Some(ty) = self.array_types.get(&element) {
            return Ok(*ty);
        }
        let base = self.corelib_type("System.Array")?;
        let mut name = self.types[element.0 as usize].name.clone();
        name.push_str("[]");
        let vtable = self.types[base.0 as usize].vtable.clone();
        let ty = self.push_type(Type {
            name,
            def: None,
            args: Rc::from([]),
            kind: Kind::Array(element),
            base: Some(base),
            flags: 0,
            fields: Vec::new(),
            static_slots: Vec::new(),
            statics: Vec::new(),
            vtable,
            interfaces: Vec::new(),
            declared_interfaces: Vec::new(),
            initialized: true,
        })?;
        self.array_types.insert(element, ty);
        Ok(ty)
    }

    fn push_type(&mut self, t: Type) -> Result<TypeId, VmError> {
        let ty = TypeId(u32::try_from(self.types.len()).map_err(|_| VmError::OutOfMemory)?);
        self.types.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        self.types.push(t);
        Ok(ty)
    }

    /// Загрузить определение с аргументами: раскладка полей, база, таблица
    /// виртуальных методов, интерфейсы.
    pub(crate) fn load_def(&mut self, asm: Asm, row: u32, args: Rc<[TypeId]>) -> Result<TypeId, VmError> {
        let key = (asm, row, args.to_vec());
        if let Some(ty) = self.type_map.get(&key) {
            return Ok(*ty);
        }
        if self.load_depth > MAX_LOAD_DEPTH {
            return Err(VmError::Unsupported {
                what: String::from("types nested more than 64 deep (inheritance or struct fields)"),
            });
        }
        let a = self.assembly(asm);
        let (namespace, simple) = self.def_names(asm, row)?;
        let flags = a.tables.column(id::TYPE_DEF, row, 0)?;
        let extends = a.tables.coded_column(id::TYPE_DEF, row, 3, Coded::TypeDefOrRef)?;

        let mut name = self.def_full_name(asm, row, 0)?;
        if !args.is_empty() {
            name.push('[');
            for (index, arg) in args.iter().enumerate() {
                if index > 0 {
                    name.push(',');
                }
                name.push_str(&self.types[arg.0 as usize].name);
            }
            name.push(']');
        }

        let mut base_name = String::new();
        if !extends.is_nil() {
            a.write_type_name(extends, &mut base_name, 0)?;
        }
        let is_enum_itself = asm == CORELIB && namespace == "System" && simple == "Enum";
        let kind = match Prim::from_name(simple) {
            Some(p) if asm == CORELIB && namespace == "System" => Kind::Prim(p),
            _ if flags & TYPE_INTERFACE != 0 => Kind::Interface,
            _ if base_name == "System.ValueType" && !is_enum_itself => Kind::Struct,
            _ if base_name == "System.Enum" => Kind::Enum(self.enum_underlying(asm, row)?),
            _ => Kind::Class,
        };

        let ty = self.push_type(Type {
            name,
            def: Some((asm, row)),
            args: Rc::clone(&args),
            kind,
            base: None,
            flags,
            fields: Vec::new(),
            static_slots: Vec::new(),
            statics: Vec::new(),
            vtable: Vec::new(),
            interfaces: Vec::new(),
            declared_interfaces: Vec::new(),
            initialized: false,
        })?;
        self.type_map.insert(key, ty);

        self.load_depth += 1;
        let result = self.finish_type(ty, asm, extends, &args);
        self.load_depth -= 1;
        result?;
        Ok(ty)
    }


    fn finish_type(&mut self, ty: TypeId, asm: Asm, extends: Token, args: &[TypeId]) -> Result<(), VmError> {
        if !extends.is_nil() {
            let base = self.resolve_type_token(asm, extends, args, &[])?;
            self.types[ty.0 as usize].base = Some(base);
        }
        self.layout(ty)
    }


    fn enum_underlying(&self, asm: Asm, row: u32) -> Result<Prim, VmError> {
        let a = self.assembly(asm);
        for field in a.tables.list(id::TYPE_DEF, row, 4, id::FIELD, id::FIELD_PTR)? {
            let field = field?;
            if a.tables.column(id::FIELD, field, 0)? as u16 & FIELD_STATIC != 0 {
                continue;
            }
            let blob = a.root.blobs.get(a.tables.column(id::FIELD, field, 2)?)?;
            if let Some(p) = sig::element(blob, 1).ok().and_then(Prim::from_element) {
                return Ok(p);
            }
        }
        Err(VmError::Meta(clr_meta::Error::BadSignature("enum without an integer value field")))
    }

    /// Тип по токену `TypeDef`, `TypeRef` или `TypeSpec`.
    pub(crate) fn resolve_type_token(
        &mut self,
        asm: Asm,
        token: Token,
        targs: &[TypeId],
        margs: &[TypeId],
    ) -> Result<TypeId, VmError> {
        match token.table {
            id::TYPE_DEF => self.load_def(asm, token.row, Rc::from([])),
            id::TYPE_REF => {
                let (def_asm, row) = self.resolve_type_ref(asm, token.row, 0)?;
                self.load_def(def_asm, row, Rc::from([]))
            }
            id::TYPE_SPEC => {
                let a = self.assembly(asm);
                let blob = a.root.blobs.get(a.tables.column(id::TYPE_SPEC, token.row, 0)?)?;
                match self.sig_type(asm, blob, 0, targs, margs, 0)?.0 {
                    SigType::Type(ty) => Ok(ty),
                    _ => Err(VmError::Unsupported { what: String::from("a type specification that is not a type") }),
                }
            }
            _ => Err(VmError::Meta(clr_meta::Error::BadIndex("not a type token"))),
        }
    }

    /// Тип, записанный в сигнатуре с позиции `at`, и позиция за ним.
    pub(crate) fn sig_type(
        &mut self,
        asm: Asm,
        blob: &'a [u8],
        at: usize,
        targs: &[TypeId],
        margs: &[TypeId],
        depth: u32,
    ) -> Result<(SigType, usize), VmError> {
        if depth > MAX_SIG_DEPTH {
            return Err(VmError::Meta(clr_meta::Error::BadSignature("type nested too deeply")));
        }
        let start = sig::skip_modifiers(blob, at)?;
        let code = *blob.get(start).ok_or(VmError::Meta(clr_meta::Error::BadSignature("truncated")))?;
        let mut at = start + 1;
        if let Some(p) = Prim::from_element(code) {
            return Ok((SigType::Type(self.prim_type(p)?), at));
        }
        let ty = match code {
            elem::VOID => return Ok((SigType::Void, at)),
            elem::STRING => self.corelib_type("System.String")?,
            elem::OBJECT => self.corelib_type("System.Object")?,
            elem::BYREF => return Ok((SigType::ByRef, sig::skip_type(blob, at)?)),
            elem::PTR | elem::FNPTR => return Ok((SigType::Pointer, sig::skip_type(blob, start)?)),
            elem::CLASS | elem::VALUETYPE => {
                let token = sig::type_token(read_u(blob, &mut at)?)?;
                self.resolve_type_token(asm, token, targs, margs)?
            }
            elem::VAR | elem::MVAR => {
                let number = read_u(blob, &mut at)? as usize;
                let args = if code == elem::VAR { targs } else { margs };
                match args.get(number) {
                    Some(ty) => *ty,
                    None => {
                        return Err(VmError::Unsupported {
                            what: format!("open generic parameter {}{number}", if code == elem::VAR { "!" } else { "!!" }),
                        });
                    }
                }
            }
            elem::SZARRAY => {
                let (element, next) = self.sig_type(asm, blob, at, targs, margs, depth + 1)?;
                at = next;
                match element {
                    SigType::Type(element) => self.array_of(element)?,
                    _ => return Err(VmError::Unsupported { what: String::from("arrays of pointers") }),
                }
            }
            elem::GENERICINST => {
                at += 1;
                let token = sig::type_token(read_u(blob, &mut at)?)?;
                let count = read_u(blob, &mut at)?;
                let mut args = Vec::new();
                for _ in 0..count {
                    let (arg, next) = self.sig_type(asm, blob, at, targs, margs, depth + 1)?;
                    at = next;
                    match arg {
                        SigType::Type(arg) => args.push(arg),
                        _ => return Err(VmError::Unsupported { what: String::from("a generic argument that is not a type") }),
                    }
                }
                let (def_asm, row) = match token.table {
                    id::TYPE_DEF => (asm, token.row),
                    id::TYPE_REF => self.resolve_type_ref(asm, token.row, 0)?,
                    _ => return Err(VmError::Meta(clr_meta::Error::BadSignature("generic instance of a type spec"))),
                };
                self.load_def(def_asm, row, Rc::from(args))?
            }
            elem::ARRAY => {
                return Err(VmError::Unsupported { what: String::from("multi-dimensional arrays (phase N4)") });
            }
            _ => return Err(VmError::Unsupported { what: format!("type element 0x{code:02x} in a signature") }),
        };
        Ok((SigType::Type(ty), at))
    }

    /// Как место этого типа хранит значение.
    pub(crate) fn store_of(&self, sig_type: SigType) -> Store {
        match sig_type {
            SigType::Type(ty) => self.types[ty.0 as usize].store(ty),
            SigType::ByRef => Store::ByRef,
            SigType::Pointer => Store::Prim(Prim::I),
            SigType::Void => Store::Ref,
        }
    }

    // --------------------------------------------------------------------
    // Методы
    // --------------------------------------------------------------------


    pub(crate) fn method_id(
        &mut self,
        asm: Asm,
        row: u32,
        owner: TypeId,
        margs: Rc<[TypeId]>,
    ) -> Result<MethodId, VmError> {
        let key = (asm, row, owner, margs.to_vec());
        if let Some(method) = self.method_map.get(&key) {
            return Ok(*method);
        }
        let a = self.assembly(asm);
        let rva = a.tables.column(id::METHOD_DEF, row, 0)?;
        let impl_flags = a.tables.column(id::METHOD_DEF, row, 1)? as u16;
        let flags = a.tables.column(id::METHOD_DEF, row, 2)? as u16;
        let name = a.root.strings.get(a.tables.column(id::METHOD_DEF, row, 3)?)?;
        let blob = a.root.blobs.get(a.tables.column(id::METHOD_DEF, row, 4)?)?;
        let signature = sig::method_sig(blob)?;
        if signature.header.convention & 0x0F == sig::CALL_VARARG {
            return Err(VmError::Unsupported { what: format!("variable argument lists in {name}") });
        }
        let native = if impl_flags & IMPL_INTERNAL_CALL != 0 {
            natives::lookup(&self.native_key(asm, row, name, blob)?)
        } else {
            None
        };
        let method = MethodId(u32::try_from(self.methods.len()).map_err(|_| VmError::OutOfMemory)?);
        self.methods.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        self.methods.push(MethodInfo {
            asm,
            row,
            owner,
            margs,
            name,
            flags,
            impl_flags,
            rva,
            sig: blob,
            has_this: signature.header.convention & sig::HAS_THIS != 0,
            params: signature.header.params,
            returns: signature.returns_value,
            native,
            body: None,
        });
        self.method_map.insert(key, method);
        Ok(method)
    }

    /// Имя, по которому член находится в таблице `natives`:
    /// `System.String::Concat(string,string)`.
    pub(crate) fn native_key(&self, asm: Asm, row: u32, name: &str, blob: &[u8]) -> Result<String, VmError> {
        let a = self.assembly(asm);
        let owner = self.owner_of_method_quiet(asm, row);
        let mut key = String::new();
        a.write_type_name(Token { table: id::TYPE_DEF, row: owner }, &mut key, 0)?;
        let _ = write!(key, "::{name}(");
        sig::write_params(blob, &a, &mut key)?;
        key.push(')');
        Ok(key)
    }

    /// Владелец метода без кэша — для разовых имён (`native_key`).
    fn owner_of_method_quiet(&self, asm: Asm, row: u32) -> u32 {
        let a = self.assembly(asm);
        for type_row in 1..=a.tables.rows(id::TYPE_DEF) {
            if let Ok(list) = a.tables.list(id::TYPE_DEF, type_row, 5, id::METHOD_DEF, id::METHOD_PTR) {
                if list.into_iter().any(|m| m == Ok(row)) {
                    return type_row;
                }
            }
        }
        0
    }

    /// Тело метода, разобранное для его экземпляра.
    pub(crate) fn body(&mut self, method: MethodId) -> Result<Rc<Body<'a>>, VmError> {
        if let Some(body) = &self.methods[method.0 as usize].body {
            return Ok(Rc::clone(body));
        }
        let (asm, rva, owner, margs) = {
            let info = &self.methods[method.0 as usize];
            (info.asm, info.rva, info.owner, Rc::clone(&info.margs))
        };
        let targs = Rc::clone(&self.types[owner.0 as usize].args);
        let a = self.assembly(asm);
        let Some(parsed) = a.method_body(rva)? else {
            return Err(VmError::Unsupported {
                what: format!(
                    "{} has no IL body: it is extern, abstract or provided by the runtime",
                    self.method_name(method)
                ),
            });
        };
        let mut clauses = Vec::new();
        for clause in parsed.clauses() {
            clauses.push(clause?);
        }
        if !clauses.is_empty() {
            return Err(VmError::Unsupported {
                what: format!("exception handling in {} (phase N3b)", self.method_name(method)),
            });
        }
        let mut locals = Vec::new();
        if parsed.local_signature != 0 {
            let t = Token::from_value(parsed.local_signature);
            if t.table != id::STAND_ALONE_SIG {
                return Err(VmError::Invalid {
                    what: "local variable signature token points elsewhere",
                    at: self.method_name(method),
                });
            }
            let blob = a.root.blobs.get(a.tables.column(id::STAND_ALONE_SIG, t.row, 0)?)?;
            let (count, mut at) = sig::locals(blob)?;
            locals.try_reserve_exact(count as usize).map_err(|_| VmError::OutOfMemory)?;
            for _ in 0..count {
                let (local, next) = self.sig_type(asm, blob, at, &targs, &margs, 0)?;
                locals.push(self.store_of(local));
                at = next;
            }
        }
        let body = Rc::new(Body { code: parsed.code, max_stack: parsed.max_stack, locals, clauses });
        self.methods[method.0 as usize].body = Some(Rc::clone(&body));
        Ok(body)
    }

    // --------------------------------------------------------------------
    // Токены в теле метода
    // --------------------------------------------------------------------


    pub(crate) fn resolve(&mut self, context: MethodId, token: u32) -> Result<Resolved, VmError> {
        if let Some(resolved) = self.resolved.get(&(context, token)) {
            return Ok(*resolved);
        }
        let (asm, owner, margs) = {
            let info = &self.methods[context.0 as usize];
            (info.asm, info.owner, Rc::clone(&info.margs))
        };
        let targs = Rc::clone(&self.types[owner.0 as usize].args);
        let t = Token::from_value(token);
        let resolved = match t.table {
            id::METHOD_DEF => {
                let owner_row = self.owner_row(asm, t.row, false)?;
                let owner = self.load_def(asm, owner_row, Rc::from([]))?;
                Resolved::Method(self.method_id(asm, t.row, owner, Rc::from([]))?)
            }
            id::MEMBER_REF => self.resolve_member_ref(asm, t.row, &targs, &margs)?,
            id::METHOD_SPEC => {
                let a = self.assembly(asm);
                let generic = a.tables.coded_column(id::METHOD_SPEC, t.row, 0, Coded::MethodDefOrRef)?;
                let blob = a.root.blobs.get(a.tables.column(id::METHOD_SPEC, t.row, 1)?)?;
                let Resolved::Method(base) = self.resolve(context, generic.value())? else {
                    return Err(self.invalid("method specification of something that is not a method"));
                };
                let mut at = 1;
                let count = read_u(blob, &mut at)?;
                let mut args = Vec::new();
                for _ in 0..count {
                    let (arg, next) = self.sig_type(asm, blob, at, &targs, &margs, 0)?;
                    at = next;
                    match arg {
                        SigType::Type(arg) => args.push(arg),
                        _ => return Err(self.invalid("generic method argument is not a type")),
                    }
                }
                let (def_asm, row, def_owner) = {
                    let info = &self.methods[base.0 as usize];
                    (info.asm, info.row, info.owner)
                };
                Resolved::Method(self.method_id(def_asm, row, def_owner, Rc::from(args))?)
            }
            id::FIELD => {
                let owner_row = self.owner_row(asm, t.row, true)?;
                let owner = self.load_def(asm, owner_row, Rc::from([]))?;
                Resolved::Field(self.field_ref(owner, asm, t.row)?)
            }
            id::TYPE_DEF | id::TYPE_REF | id::TYPE_SPEC => {
                Resolved::Type(self.resolve_type_token(asm, t, &targs, &margs)?)
            }
            _ => return Err(self.invalid("token of an unexpected table")),
        };
        self.resolved.insert((context, token), resolved);
        Ok(resolved)
    }


    fn resolve_member_ref(
        &mut self,
        asm: Asm,
        row: u32,
        targs: &[TypeId],
        margs: &[TypeId],
    ) -> Result<Resolved, VmError> {
        let a = self.assembly(asm);
        let parent = a.tables.coded_column(id::MEMBER_REF, row, 0, Coded::MemberRefParent)?;
        let name = a.root.strings.get(a.tables.column(id::MEMBER_REF, row, 1)?)?;
        let blob = a.root.blobs.get(a.tables.column(id::MEMBER_REF, row, 2)?)?;
        let owner = match parent.table {
            id::TYPE_DEF | id::TYPE_REF | id::TYPE_SPEC => self.resolve_type_token(asm, parent, targs, margs)?,
            _ => {
                return Err(VmError::Unsupported {
                    what: format!("member reference {name} whose parent is a method or a module"),
                });
            }
        };
        if blob.first() == Some(&sig::FIELD) {
            return match self.find_field(owner, name)? {
                Some(field) => Ok(Resolved::Field(field)),
                None => Err(VmError::MissingMember {
                    name: format!("field {}::{name} from {}", self.types[owner.0 as usize].name, self.ref_origin(asm, parent)),
                }),
            };
        }
        let key = signature_key(a, name, blob, None)?;
        match self.find_method(owner, name, &key)? {
            Some(method) => Ok(Resolved::Method(method)),
            None => Err(VmError::MissingMember {
                name: format!("{}::{key} from {}", self.types[owner.0 as usize].name, self.ref_origin(asm, parent)),
            }),
        }
    }

    /// Какой сборке программа приписывала член — для сообщения.
    fn ref_origin(&self, asm: Asm, parent: Token) -> String {
        let a = self.assembly(asm);
        if parent.table == id::TYPE_REF {
            if let Ok(scope) = a.tables.coded_column(id::TYPE_REF, parent.row, 0, Coded::ResolutionScope) {
                if scope.table == id::ASSEMBLY_REF {
                    if let Ok(name) =
                        a.tables.column(id::ASSEMBLY_REF, scope.row, 6).and_then(|index| a.root.strings.get(index))
                    {
                        return String::from(name);
                    }
                }
            }
        }
        if asm == PROGRAM { self.assembly_name(asm) } else { String::from("the base library") }
    }

    /// Метод по имени и ключу — у типа или у его баз.
    pub(crate) fn find_method(&mut self, owner: TypeId, name: &str, key: &str) -> Result<Option<MethodId>, VmError> {
        let mut current = Some(owner);
        while let Some(ty) = current {
            if let Some((asm, row)) = self.types[ty.0 as usize].def {
                let a = self.assembly(asm);
                for method in a.tables.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR)? {
                    let method = method?;
                    if a.root.strings.get(a.tables.column(id::METHOD_DEF, method, 3)?)? != name {
                        continue;
                    }
                    let blob = a.root.blobs.get(a.tables.column(id::METHOD_DEF, method, 4)?)?;
                    if signature_key(a, name, blob, None)? == key {
                        return self.method_id(asm, method, ty, Rc::from([])).map(Some);
                    }
                }
            }
            current = self.types[ty.0 as usize].base;
        }
        Ok(None)
    }


    fn find_field(&mut self, owner: TypeId, name: &str) -> Result<Option<FieldRef>, VmError> {
        let mut current = Some(owner);
        while let Some(ty) = current {
            if let Some((asm, row)) = self.types[ty.0 as usize].def {
                let a = self.assembly(asm);
                for field in a.tables.list(id::TYPE_DEF, row, 4, id::FIELD, id::FIELD_PTR)? {
                    let field = field?;
                    if a.root.strings.get(a.tables.column(id::FIELD, field, 1)?)? == name {
                        return self.field_ref(ty, asm, field).map(Some);
                    }
                }
            }
            current = self.types[ty.0 as usize].base;
        }
        Ok(None)
    }

    /// Место поля `row` в раскладке владельца.
    pub(crate) fn field_ref(&mut self, owner: TypeId, asm: Asm, row: u32) -> Result<FieldRef, VmError> {
        let a = self.assembly(asm);
        let flags = a.tables.column(id::FIELD, row, 0)? as u16;
        let t = &self.types[owner.0 as usize];
        let is_static = flags & FIELD_STATIC != 0;
        let found = if is_static {
            t.static_slots.iter().position(|slot| slot.asm == asm && slot.row == row).map(|i| (i, t.static_slots[i].store))
        } else {
            t.fields.iter().position(|slot| slot.asm == asm && slot.row == row).map(|i| (i, t.fields[i].store))
        };
        match found {
            Some((index, store)) => Ok(FieldRef { owner, index: index as u32, store, asm, row }),
            None if flags & FIELD_LITERAL != 0 => Err(VmError::Unsupported {
                what: format!("run-time access to a constant field of {}", t.name),
            }),
            None => Err(VmError::Invalid { what: "field is not in the layout of its type", at: t.name.clone() }),
        }
    }
}
