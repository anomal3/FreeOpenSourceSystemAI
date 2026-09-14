//! Раскладка типа, таблица виртуальных методов, реализации интерфейсов,
//! совместимость типов и статические конструкторы (фаза N3a).
//!
//! # Таблица виртуальных методов
//!
//! Ячейки базового класса наследуются с теми же номерами; метод без флага
//! `NewSlot` занимает ячейку базового с тем же ключом (имя и сигнатура с
//! подставленными параметрами типа), с флагом — новую. Поэтому `callvirt
//! Base::M` берёт номер ячейки у `Base` и метод из таблицы настоящего типа
//! объекта, а `Cat.ToString`, объявленный через `new`, в ячейку
//! `Object.ToString` не попадает.
//!
//! # Интерфейсы
//!
//! Реализация ищется от типа объекта к базам: сначала явная (`MethodImpl` —
//! так компилятор записывает `void IResettable.Reset()`), затем неявная по
//! ключу. Неявная засчитывается, если метод переопределяет (а не скрывает)
//! или если тип сам перечисляет интерфейс: `StepCounter : Counter, ICounter` с
//! `new int Next()` заново реализует `ICounter.Next`, а без `ICounter` в списке
//! тот же `new` остался бы невидим интерфейсу. Не нашлось нигде — берётся тело
//! члена по умолчанию из самого интерфейса.

use alloc::format;
use alloc::rc::Rc;
use alloc::vec::Vec;

use clr_meta::sig::{self, elem};
use clr_meta::tables::id;
use clr_meta::{Coded, Token};

use crate::heap::Object;
use crate::loader::signature_key;
use crate::types::{
    Asm, FIELD_LITERAL, FIELD_STATIC, FieldSlot, Kind, METHOD_NEW_SLOT, METHOD_STATIC, METHOD_VIRTUAL,
    MethodId, Resolved, Store, TypeId, VSlot,
};
use crate::value::ObjRef;
use crate::vm::Vm;
use crate::{Host, VmError};

impl<'a, H: Host> Vm<'a, H> {
    /// Разложить загруженный тип: поля, таблица виртуальных методов,
    /// интерфейсы.
    pub(crate) fn layout(&mut self, ty: TypeId) -> Result<(), VmError> {
        let Some((asm, row)) = self.types[ty.0 as usize].def else { return Ok(()) };
        let args = Rc::clone(&self.types[ty.0 as usize].args);
        let base = self.types[ty.0 as usize].base;
        let a = self.assembly(asm);

        let mut fields = match base {
            Some(base) => self.types[base.0 as usize].fields.clone(),
            None => Vec::new(),
        };
        let mut static_slots = Vec::new();
        for field in a.tables.list(id::TYPE_DEF, row, 4, id::FIELD, id::FIELD_PTR)? {
            let field = field?;
            let flags = a.tables.column(id::FIELD, field, 0)? as u16;
            let is_static = flags & FIELD_STATIC != 0;
            if is_static && flags & FIELD_LITERAL != 0 {
                continue;
            }
            let blob = a.root.blobs.get(a.tables.column(id::FIELD, field, 2)?)?;
            // Ссылочному полю тип загружать незачем: хранится ссылка. Иначе
            // класс с полем `Form` тянул бы за собой всю WinForms.
            let store = match sig::element(blob, 1)? {
                elem::CLASS | elem::STRING | elem::OBJECT | elem::SZARRAY => Store::Ref,
                _ => {
                    let (field_type, _) = self.sig_type(asm, blob, 1, &args, &[], 0)?;
                    self.store_of(field_type)
                }
            };
            let slot = FieldSlot { asm, row: field, store };
            if is_static {
                static_slots.push(slot);
            } else {
                fields.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
                fields.push(slot);
            }
        }
        {
            let t = &mut self.types[ty.0 as usize];
            t.fields = fields;
            t.static_slots = static_slots;
        }

        // Интерфейсы: унаследованные, перечисленные и их базовые.
        let mut interfaces = match base {
            Some(base) => self.types[base.0 as usize].interfaces.clone(),
            None => Vec::new(),
        };
        let mut declared = Vec::new();
        for impl_row in 1..=a.tables.rows(id::INTERFACE_IMPL) {
            if a.tables.column(id::INTERFACE_IMPL, impl_row, 0)? != row {
                continue;
            }
            let token = a.tables.coded_column(id::INTERFACE_IMPL, impl_row, 1, Coded::TypeDefOrRef)?;
            let interface = self.resolve_type_token(asm, token, &args, &[])?;
            declared.push(interface);
            let inherited = self.types[interface.0 as usize].interfaces.clone();
            for candidate in core::iter::once(interface).chain(inherited) {
                if !interfaces.contains(&candidate) {
                    interfaces.push(candidate);
                }
            }
        }

        // Таблица виртуальных методов.
        let mut vtable = match base {
            Some(base) => self.types[base.0 as usize].vtable.clone(),
            None => Vec::new(),
        };
        let vars = self.var_names(&args);
        for method in a.tables.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR)? {
            let method = method?;
            let flags = a.tables.column(id::METHOD_DEF, method, 2)? as u16;
            if flags & METHOD_VIRTUAL == 0 {
                continue;
            }
            let name = a.root.strings.get(a.tables.column(id::METHOD_DEF, method, 3)?)?;
            let blob = a.root.blobs.get(a.tables.column(id::METHOD_DEF, method, 4)?)?;
            let key = signature_key(a, name, blob, Some(&vars))?;
            let id = self.method_id(asm, method, ty, Rc::from([]))?;
            let reuse = if flags & METHOD_NEW_SLOT == 0 { vtable.iter().rposition(|slot: &VSlot| slot.key == key) } else { None };
            let index = match reuse {
                Some(index) => {
                    vtable[index].method = id;
                    index
                }
                None => {
                    vtable.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
                    vtable.push(VSlot { key, method: id });
                    vtable.len() - 1
                }
            };
            self.slots.insert(id, index);
        }
        // Явные переопределения методов базового класса (`MethodImpl` с
        // объявлением у класса, не у интерфейса).
        for impl_row in 1..=a.tables.rows(id::METHOD_IMPL) {
            if a.tables.column(id::METHOD_IMPL, impl_row, 0)? != row {
                continue;
            }
            let declaration = a.tables.coded_column(id::METHOD_IMPL, impl_row, 2, Coded::MethodDefOrRef)?;
            let declaration = self.method_token(asm, declaration, ty)?;
            let declaring = self.methods[declaration.0 as usize].owner;
            if self.types[declaring.0 as usize].kind == Kind::Interface {
                continue;
            }
            let body = a.tables.coded_column(id::METHOD_IMPL, impl_row, 1, Coded::MethodDefOrRef)?;
            let body = self.method_token(asm, body, ty)?;
            if let Some(&index) = self.slots.get(&declaration) {
                if let Some(slot) = vtable.get_mut(index) {
                    slot.method = body;
                }
            }
        }

        let t = &mut self.types[ty.0 as usize];
        t.vtable = vtable;
        t.interfaces = interfaces;
        t.declared_interfaces = declared;
        Ok(())
    }

    /// Метод по токену `MethodDefOrRef` из таблицы `MethodImpl` типа `ty`.
    fn method_token(&mut self, asm: Asm, token: Token, ty: TypeId) -> Result<MethodId, VmError> {
        let args = Rc::clone(&self.types[ty.0 as usize].args);
        match token.table {
            id::METHOD_DEF => {
                let owner_row = self.owner_row(asm, token.row, false)?;
                let owner = if self.types[ty.0 as usize].def == Some((asm, owner_row)) {
                    ty
                } else {
                    self.load_def(asm, owner_row, Rc::from([]))?
                };
                self.method_id(asm, token.row, owner, Rc::from([]))
            }
            _ => {
                // MemberRef разрешается так же, как в теле метода, но в
                // контексте типа: чужого метода-контекста здесь нет.
                let a = self.assembly(asm);
                let parent = a.tables.coded_column(id::MEMBER_REF, token.row, 0, Coded::MemberRefParent)?;
                let name = a.root.strings.get(a.tables.column(id::MEMBER_REF, token.row, 1)?)?;
                let blob = a.root.blobs.get(a.tables.column(id::MEMBER_REF, token.row, 2)?)?;
                let owner = self.resolve_type_token(asm, parent, &args, &[])?;
                let key = signature_key(a, name, blob, None)?;
                match self.find_method(owner, name, &key)? {
                    Some(method) => Ok(method),
                    None => Err(VmError::MissingMember {
                        name: format!("{}::{key} named in a method override", self.types[owner.0 as usize].name),
                    }),
                }
            }
        }
    }

    /// Какой метод на самом деле выполнит `callvirt method` у объекта типа `ty`.
    pub(crate) fn dispatch(&mut self, ty: TypeId, method: MethodId) -> Result<MethodId, VmError> {
        if let Some(target) = self.dispatch_cache.get(&(ty, method)) {
            return Ok(*target);
        }
        let owner = self.methods[method.0 as usize].owner;
        let target = if self.types[owner.0 as usize].kind == Kind::Interface {
            self.interface_impl(ty, method)?
        } else {
            match self.slots.get(&method) {
                Some(&index) => match self.types[ty.0 as usize].vtable.get(index) {
                    Some(slot) => slot.method,
                    None => {
                        return Err(VmError::Invalid {
                            what: "virtual call on an object of an unrelated type",
                            at: self.method_name(method),
                        });
                    }
                },
                None => method,
            }
        };
        self.dispatch_cache.insert((ty, method), target);
        Ok(target)
    }


    fn interface_impl(&mut self, ty: TypeId, method: MethodId) -> Result<MethodId, VmError> {
        let (iasm, name, blob, interface, margs, has_body) = {
            let info = &self.methods[method.0 as usize];
            (info.asm, info.name, info.sig, info.owner, Rc::clone(&info.margs), info.rva != 0)
        };
        let ivars = self.var_names(&Rc::clone(&self.types[interface.0 as usize].args));
        let ikey = signature_key(self.assembly(iasm), name, blob, Some(&ivars))?;
        // Метод, найденный в классе, — определение; экземпляр обобщённого
        // метода получает аргументы вызова.
        let with_margs = |vm: &mut Self, found: MethodId| -> Result<MethodId, VmError> {
            if margs.is_empty() {
                return Ok(found);
            }
            let (asm, row, owner) = {
                let info = &vm.methods[found.0 as usize];
                (info.asm, info.row, info.owner)
            };
            vm.method_id(asm, row, owner, Rc::clone(&margs))
        };

        let generic_declaration = if margs.is_empty() {
            method
        } else {
            let (asm, row, owner) = {
                let info = &self.methods[method.0 as usize];
                (info.asm, info.row, info.owner)
            };
            self.method_id(asm, row, owner, Rc::from([]))?
        };

        let mut current = Some(ty);
        while let Some(class) = current {
            let Some((asm, row)) = self.types[class.0 as usize].def else {
                current = self.types[class.0 as usize].base;
                continue;
            };
            let a = self.assembly(asm);
            for impl_row in 1..=a.tables.rows(id::METHOD_IMPL) {
                if a.tables.column(id::METHOD_IMPL, impl_row, 0)? != row {
                    continue;
                }
                let declaration = a.tables.coded_column(id::METHOD_IMPL, impl_row, 2, Coded::MethodDefOrRef)?;
                if self.method_token(asm, declaration, class)? == generic_declaration {
                    let body = a.tables.coded_column(id::METHOD_IMPL, impl_row, 1, Coded::MethodDefOrRef)?;
                    let body = self.method_token(asm, body, class)?;
                    return with_margs(self, body);
                }
            }
            let declares = self.types[class.0 as usize].declared_interfaces.contains(&interface);
            let cvars = self.var_names(&Rc::clone(&self.types[class.0 as usize].args));
            for candidate in a.tables.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR)? {
                let candidate = candidate?;
                let flags = a.tables.column(id::METHOD_DEF, candidate, 2)? as u16;
                if flags & METHOD_VIRTUAL == 0 || (flags & METHOD_NEW_SLOT != 0 && !declares) {
                    continue;
                }
                let cname = a.root.strings.get(a.tables.column(id::METHOD_DEF, candidate, 3)?)?;
                if cname != name {
                    continue;
                }
                let cblob = a.root.blobs.get(a.tables.column(id::METHOD_DEF, candidate, 4)?)?;
                if signature_key(a, cname, cblob, Some(&cvars))? == ikey {
                    let found = self.method_id(asm, candidate, class, Rc::from([]))?;
                    return with_margs(self, found);
                }
            }
            current = self.types[class.0 as usize].base;
        }
        if has_body {
            return Ok(method);
        }
        Err(VmError::MissingMember {
            name: format!(
                "implementation of {} in {}",
                self.method_name(method),
                self.types[ty.0 as usize].name
            ),
        })
    }

    /// Можно ли значение типа `from` считать значением типа `to` (`isinst`).
    pub(crate) fn assignable(&self, from: TypeId, to: TypeId) -> bool {
        let mut current = Some(from);
        while let Some(ty) = current {
            if ty == to {
                return true;
            }
            current = self.types[ty.0 as usize].base;
        }
        let target = &self.types[to.0 as usize];
        if target.kind == Kind::Interface {
            return self.types[from.0 as usize].interfaces.contains(&to);
        }
        // Ковариантность массивов: `string[]` — это `object[]`, но `int[]` — не
        // `object[]`.
        if let (Kind::Array(a), Kind::Array(b)) = (self.types[from.0 as usize].kind, target.kind) {
            return !self.types[a.0 as usize].is_value_type()
                && !self.types[b.0 as usize].is_value_type()
                && self.assignable(a, b);
        }
        false
    }

    /// Настоящий тип объекта в куче.
    pub(crate) fn type_of_object(&mut self, object: ObjRef) -> Result<TypeId, VmError> {
        match self.heap.get(object) {
            Some(Object::String(_)) => self.corelib_type("System.String"),
            Some(
                Object::Array { ty, .. }
                | Object::Instance { ty, .. }
                | Object::Struct { ty, .. }
                | Object::Boxed { ty, .. }
                | Object::Delegate { ty, .. },
            ) => {
                Ok(*ty)
            }
            Some(Object::RuntimeType(_)) => self.corelib_type("System.RuntimeType"),
            None => Err(self.invalid("reference to an object that does not exist")),
        }
    }

    /// Запустить статический конструктор типа, если он ещё не запускался.
    ///
    /// `true` — кадр конструктора положен сверху, и инструкцию, которая к типу
    /// обратилась, нужно выполнить заново, когда он вернётся. Флаг ставится до
    /// запуска: обращение к статическим полям из самого конструктора (или по
    /// кругу из другого) видит тип инициализированным, как и в .NET.
    pub(crate) fn ensure_initialized(&mut self, ty: TypeId) -> Result<bool, VmError> {
        if self.types[ty.0 as usize].initialized {
            return Ok(false);
        }
        self.types[ty.0 as usize].initialized = true;
        let slots: Vec<Store> = self.types[ty.0 as usize].static_slots.iter().map(|slot| slot.store).collect();
        let mut statics = Vec::new();
        statics.try_reserve_exact(slots.len()).map_err(|_| VmError::OutOfMemory)?;
        for store in slots {
            statics.push(self.zero(store)?);
        }
        self.types[ty.0 as usize].statics = statics;

        let Some((asm, row)) = self.types[ty.0 as usize].def else { return Ok(false) };
        let a = self.assembly(asm);
        for method in a.tables.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR)? {
            let method = method?;
            let flags = a.tables.column(id::METHOD_DEF, method, 2)? as u16;
            if flags & METHOD_STATIC != 0 && a.root.strings.get(a.tables.column(id::METHOD_DEF, method, 3)?)? == ".cctor" {
                let cctor = self.method_id(asm, method, ty, Rc::from([]))?;
                self.enter(cctor, Vec::new())?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Тип, названный токеном в теле текущего метода.
    pub(crate) fn resolve_type(&mut self, context: MethodId, token: u32) -> Result<TypeId, VmError> {
        match self.resolve(context, token)? {
            Resolved::Type(ty) => Ok(ty),
            _ => Err(self.invalid("token does not name a type")),
        }
    }
}
