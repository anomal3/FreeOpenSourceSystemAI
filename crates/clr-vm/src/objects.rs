//! Вызовы, создание объектов, поля, упаковка и приведения (фаза N3a).
//!
//! Инструкция, которой нужен статический конструктор, не ждёт его внутри
//! себя: кладёт его кадр сверху и возвращается, не сдвинув счётчик команд.
//! Когда конструктор вернётся, та же инструкция выполнится заново и увидит
//! тип инициализированным. Так интерпретатору не нужна рекурсия Rust.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::tables::id;

use crate::heap::Object;
use crate::natives;
use crate::types::{FieldRef, IMPL_INTERNAL_CALL, Kind, METHOD_ABSTRACT, MethodId, Prim, Resolved, Store, TYPE_BEFORE_FIELD_INIT, TypeId};
use crate::value::{ObjRef, Pointer, Value};
use crate::vm::{Frame, MAX_FRAMES, Vm};
use crate::{Host, VmError};

impl<'a, H: Host> Vm<'a, H> {
    /// Положить кадр метода с телом IL.
    pub(crate) fn enter(&mut self, method: MethodId, args: Vec<Value>) -> Result<(), VmError> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(VmError::StackOverflow);
        }
        let body = self.body(method)?;
        let mut locals = Vec::new();
        locals.try_reserve_exact(body.locals.len()).map_err(|_| VmError::OutOfMemory)?;
        for store in body.locals.iter().copied() {
            locals.push(self.zero(store)?);
        }
        let mut stack = Vec::new();
        stack.try_reserve(usize::from(body.max_stack)).map_err(|_| VmError::OutOfMemory)?;
        self.frames.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        self.frames.push(Frame::new(method, body, args, locals, stack));
        Ok(())
    }

    /// Тип делегата — наследник `MulticastDelegate`.
    pub(crate) fn is_delegate(&mut self, ty: TypeId) -> Result<bool, VmError> {
        let multicast = self.corelib_type("System.MulticastDelegate")?;
        Ok(self.types[ty.0 as usize].base == Some(multicast))
    }

    /// `Invoke` делегата: вызвать весь список по порядку.
    ///
    /// Кадры кладутся от последнего вызова к первому, так что первый
    /// выполнится первым, — без рекурсии Rust и без ожидания внутри этой
    /// функции. Результат у делегата — от последнего вызова, остальные кадры
    /// помечены `discard_result`. Аргументы-структуры получает копией каждый
    /// вызов, кроме последнего: у значения один владелец.
    fn invoke_delegate(&mut self, args: Vec<Value>) -> Result<(), VmError> {
        let Some(&this) = args.first() else {
            return Err(self.invalid("delegate invoked without the delegate"));
        };
        let targets = match this {
            Value::Obj(Some(object)) => match self.heap.get(object) {
                Some(Object::Delegate { targets, .. }) => {
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(targets.len()).map_err(|_| VmError::OutOfMemory)?;
                    copy.extend_from_slice(targets);
                    copy
                }
                _ => return Err(self.invalid("Invoke on something that is not a delegate")),
            },
            Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
            _ => return Err(self.invalid("Invoke on a value that is not a reference")),
        };
        let last = targets.len().saturating_sub(1);
        for (index, (target, method)) in targets.iter().copied().enumerate().rev() {
            let (has_this, native) = {
                let info = &self.methods[method.0 as usize];
                (info.has_this, info.native.is_some())
            };
            if native && targets.len() > 1 {
                // Член в Rust выполнился бы сразу, раньше кадров, лежащих под
                // ним, и порядок вызовов списка сломался бы.
                return Err(VmError::Unsupported {
                    what: format!("a multicast delegate over the runtime member {}", self.method_name(method)),
                });
            }
            let mut call_args = Vec::new();
            call_args.try_reserve_exact(args.len()).map_err(|_| VmError::OutOfMemory)?;
            if has_this {
                call_args.push(target);
            }
            for value in &args[1..] {
                call_args.push(if index == last { *value } else { self.copy_out(*value)? });
            }
            let depth = self.frames.len();
            self.run_method(method, call_args)?;
            if index != last && self.frames.len() > depth {
                self.frames[depth].discard_result = true;
            }
        }
        Ok(())
    }

    /// `Delegate.Combine`: списки подряд; `null` с любой стороны не меняет другой.
    pub(crate) fn combine_delegates(&mut self, a: Value, b: Value) -> Result<Value, VmError> {
        let (Value::Obj(Some(first)), Value::Obj(Some(second))) = (a, b) else {
            return Ok(if a == Value::Obj(None) { b } else { a });
        };
        let (ty, mut targets) = self.delegate_targets(first)?;
        let (_, more) = self.delegate_targets(second)?;
        targets.try_reserve(more.len()).map_err(|_| VmError::OutOfMemory)?;
        targets.extend(more);
        Ok(Value::Obj(Some(self.heap.alloc(Object::Delegate { ty, targets })?)))
    }

    /// `Delegate.Remove`: убрать последнее вхождение списка `value`, как .NET;
    /// пустой список — `null`.
    pub(crate) fn remove_delegate(&mut self, source: Value, value: Value) -> Result<Value, VmError> {
        let (Value::Obj(Some(from)), Value::Obj(Some(what))) = (source, value) else {
            return Ok(source);
        };
        let (ty, mut targets) = self.delegate_targets(from)?;
        let (_, removed) = self.delegate_targets(what)?;
        if removed.is_empty() || removed.len() > targets.len() {
            return Ok(source);
        }
        let Some(start) = (0..=targets.len() - removed.len()).rev().find(|&at| targets[at..at + removed.len()] == removed[..])
        else {
            return Ok(source);
        };
        targets.drain(start..start + removed.len());
        if targets.is_empty() {
            return Ok(Value::Obj(None));
        }
        Ok(Value::Obj(Some(self.heap.alloc(Object::Delegate { ty, targets })?)))
    }

    fn delegate_targets(&self, object: ObjRef) -> Result<(TypeId, Vec<(Value, MethodId)>), VmError> {
        match self.heap.get(object) {
            Some(Object::Delegate { ty, targets }) => {
                let mut copy = Vec::new();
                copy.try_reserve_exact(targets.len()).map_err(|_| VmError::OutOfMemory)?;
                copy.extend_from_slice(targets);
                Ok((*ty, copy))
            }
            _ => Err(self.exception("System.InvalidCastException")),
        }
    }

    /// Экземпляр класса с обнулёнными полями, без конструктора.
    pub(crate) fn new_instance(&mut self, ty: TypeId) -> Result<ObjRef, VmError> {
        let stores: Vec<Store> = self.types[ty.0 as usize].fields.iter().map(|slot| slot.store).collect();
        let mut fields = Vec::new();
        fields.try_reserve_exact(stores.len()).map_err(|_| VmError::OutOfMemory)?;
        for store in stores {
            fields.push(self.zero(store)?);
        }
        self.heap.alloc(Object::Instance { ty, fields })
    }

    /// Выполнить метод с готовыми аргументами: член в Rust — сразу, метод IL —
    /// кадром.
    fn run_method(&mut self, method: MethodId, args: Vec<Value>) -> Result<(), VmError> {
        let (native, impl_flags, flags, asm, row, name, sig) = {
            let info = &self.methods[method.0 as usize];
            (info.native, info.impl_flags, info.flags, info.asm, info.row, info.name, info.sig)
        };
        let owner = self.methods[method.0 as usize].owner;
        if name == "Invoke" && self.is_delegate(owner)? {
            return self.invoke_delegate(args);
        }
        if let Some(native) = native {
            if let Some(result) = natives::call(self, native, &args)? {
                self.push(result)?;
            }
            return Ok(());
        }
        if impl_flags & IMPL_INTERNAL_CALL != 0 {
            return Err(VmError::MissingMember {
                name: format!("{} (an internal call the runtime does not provide)", self.native_key(asm, row, name, sig)?),
            });
        }
        if flags & METHOD_ABSTRACT != 0 {
            return Err(VmError::Invalid { what: "call to an abstract method", at: self.method_name(method) });
        }
        self.enter(method, args)
    }

    /// `call` и `callvirt`. Счётчик команд сдвигается, только если вызов
    /// состоялся.
    pub(crate) fn call(&mut self, token: u32, virtual_call: bool, next: usize) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let context = self.frames[top].method;
        let Resolved::Method(method) = self.resolve(context, token)? else {
            return Err(self.invalid("call token is not a method"));
        };
        let (owner, is_static, is_ctor, is_virtual, count) = {
            let info = &self.methods[method.0 as usize];
            (info.owner, info.is_static(), info.name == ".ctor", info.is_virtual(), info.params as usize + usize::from(info.has_this))
        };
        if (is_static || is_ctor) && self.types[owner.0 as usize].flags & TYPE_BEFORE_FIELD_INIT == 0 && self.ensure_initialized(owner)? {
            return Ok(());
        }
        let mut target = method;
        if virtual_call && count > 0 {
            let mut dispatched = false;
            if let Some(constraint) = self.frames[top].constrained.take() {
                let Value::Ptr(pointer) = self.peek_at(count - 1)? else {
                    return Err(self.invalid("constrained call without a pointer to this"));
                };
                if self.types[constraint.0 as usize].is_value_type() {
                    let found = if is_virtual { self.dispatch(constraint, method)? } else { method };
                    if self.methods[found.0 as usize].owner != constraint {
                        // Метод унаследован от Object или ValueType: ему нужен
                        // объект, и значение упаковывается.
                        let value = self.load(pointer)?;
                        let value = self.copy_out(value)?;
                        let boxed = self.box_value(constraint, value)?;
                        self.replace_at(count - 1, boxed)?;
                    }
                    target = found;
                    dispatched = true;
                } else {
                    let reference = self.load(pointer)?;
                    self.replace_at(count - 1, reference)?;
                }
            }
            if !dispatched {
                match self.peek_at(count - 1)? {
                    Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
                    Value::Obj(Some(object)) if is_virtual => {
                        let ty = self.type_of_object(object)?;
                        target = self.dispatch(ty, method)?;
                        // Метод структуры, найденный у упакованного значения,
                        // ждёт `this` указателем на значение, а не объект
                        // упаковки — то, что в CLR делает unboxing-заглушка.
                        let target_owner = self.methods[target.0 as usize].owner;
                        if self.types[target_owner.0 as usize].is_value_type() {
                            if let Some((_, value)) = self.boxed(Value::Obj(Some(object))) {
                                let pointer = match value {
                                    Value::Struct(inner) => Pointer::Struct(inner),
                                    _ => Pointer::Boxed(object),
                                };
                                self.replace_at(count - 1, Value::Ptr(pointer))?;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        self.frames[top].pc = next;
        let args = self.pop_args(count)?;
        self.run_method(target, args)
    }

    /// `newobj`.
    pub(crate) fn new_object(&mut self, token: u32, next: usize) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let context = self.frames[top].method;
        let Resolved::Method(ctor) = self.resolve(context, token)? else {
            return Err(self.invalid("newobj token is not a constructor"));
        };
        let (owner, params) = {
            let info = &self.methods[ctor.0 as usize];
            (info.owner, info.params as usize)
        };
        if self.types[owner.0 as usize].flags & TYPE_BEFORE_FIELD_INIT == 0 && self.ensure_initialized(owner)? {
            return Ok(());
        }
        if owner == self.corelib_type("System.String")? {
            // Строка неизменяема и собирается средой целиком: её конструктор —
            // член в Rust, который сам возвращает готовый объект.
            let Some(native) = self.methods[ctor.0 as usize].native else {
                return Err(VmError::MissingMember {
                    name: format!("{} (a string constructor the runtime does not provide)", self.method_name(ctor)),
                });
            };
            let args = self.pop_args(params)?;
            self.frames[top].pc = next;
            if let Some(result) = natives::call(self, native, &args)? {
                self.push(result)?;
            }
            return Ok(());
        }
        if self.is_delegate(owner)? {
            // Конструктор делегата — «runtime managed»: тела нет, объект
            // собирает среда из цели и указателя на метод.
            let function = self.pop()?;
            let target = self.pop()?;
            let Value::Fn(method) = function else {
                return Err(self.invalid("delegate constructed without a method pointer"));
            };
            let mut targets = Vec::new();
            targets.try_reserve_exact(1).map_err(|_| VmError::OutOfMemory)?;
            targets.push((target, MethodId(method)));
            let object = self.heap.alloc(Object::Delegate { ty: owner, targets })?;
            self.push(Value::Obj(Some(object)))?;
            self.frames[top].pc = next;
            return Ok(());
        }
        let args = self.pop_args(params)?;
        let (instance, this) = match self.types[owner.0 as usize].kind {
            Kind::Struct => {
                let Value::Struct(object) = self.zero(Store::Struct(owner))? else {
                    return Err(self.invalid("struct zero is not a struct"));
                };
                (Value::Struct(object), Value::Ptr(Pointer::Struct(object)))
            }
            Kind::Class => {
                let object = Value::Obj(Some(self.new_instance(owner)?));
                (object, object)
            }
            _ => {
                return Err(VmError::Unsupported {
                    what: format!("newobj of {}", self.types[owner.0 as usize].name),
                });
            }
        };
        self.push(instance)?;
        let mut call_args = Vec::new();
        call_args.try_reserve_exact(args.len() + 1).map_err(|_| VmError::OutOfMemory)?;
        call_args.push(this);
        call_args.extend(args);
        self.frames[top].pc = next;
        self.run_method(ctor, call_args)
    }

    // --------------------------------------------------------------------
    // Поля
    // --------------------------------------------------------------------

    fn field(&mut self, token: u32) -> Result<FieldRef, VmError> {
        let context = self.frames[self.frames.len() - 1].method;
        match self.resolve(context, token)? {
            Resolved::Field(field) => Ok(field),
            _ => Err(self.invalid("field token does not name a field")),
        }
    }

    /// Объект, в котором лежат поля: класс, значение структуры или то, на что
    /// указывает указатель.
    fn field_object(&self, target: Value) -> Result<ObjRef, VmError> {
        let target = match target {
            Value::Ptr(pointer) => self.load(pointer)?,
            other => other,
        };
        match target {
            Value::Obj(Some(object)) | Value::Struct(object) => Ok(object),
            Value::Obj(None) => Err(self.exception("System.NullReferenceException")),
            _ => Err(self.invalid("field access on a value that holds no fields")),
        }
    }

    /// `ldfld` и `ldflda`.
    pub(crate) fn load_field(&mut self, token: u32, address: bool) -> Result<(), VmError> {
        let field = self.field(token)?;
        let target = self.pop()?;
        let object = self.field_object(target)?;
        let pointer = Pointer::Field { object, index: field.index };
        if address {
            return self.push(Value::Ptr(pointer));
        }
        let value = self.load(pointer)?;
        let value = self.copy_out(value)?;
        self.push(value)
    }

    /// `stfld`.
    pub(crate) fn store_field(&mut self, token: u32) -> Result<(), VmError> {
        let field = self.field(token)?;
        let value = self.pop()?;
        let target = self.pop()?;
        let object = self.field_object(target)?;
        self.store(Pointer::Field { object, index: field.index }, field.store.narrow(value))
    }

    /// `ldsfld`, `ldsflda` (`op` 0x7E, 0x7F) и `stsfld` (0x80). `false` —
    /// сначала запускается статический конструктор, инструкцию повторить.
    pub(crate) fn static_field(&mut self, op: u8, token: u32) -> Result<bool, VmError> {
        let field = self.field(token)?;
        if self.ensure_initialized(field.owner)? {
            return Ok(false);
        }
        let pointer = Pointer::Static { ty: field.owner.0, index: field.index };
        match op {
            0x7E => {
                let value = self.load(pointer)?;
                let value = self.copy_out(value)?;
                self.push(value)?;
            }
            0x7F => self.push(Value::Ptr(pointer))?,
            _ => {
                let value = self.pop()?;
                self.store(pointer, field.store.narrow(value))?;
            }
        }
        Ok(true)
    }

    // --------------------------------------------------------------------
    // Упаковка и приведения
    // --------------------------------------------------------------------


    pub(crate) fn box_value(&mut self, ty: TypeId, value: Value) -> Result<Value, VmError> {
        let object = match self.types[ty.0 as usize].kind {
            Kind::Struct => Object::Boxed { ty, value },
            Kind::Prim(p) | Kind::Enum(p) => Object::Boxed { ty, value: p.narrow(value) },
            // `box` у ссылочного типа (в обобщённом коде) ничего не делает.
            _ => return Ok(value),
        };
        Ok(Value::Obj(Some(self.heap.alloc(object)?)))
    }

    /// `unbox` (`any` = false) и `unbox.any`.
    pub(crate) fn unbox(&mut self, ty: TypeId, any: bool) -> Result<(), VmError> {
        if !self.types[ty.0 as usize].is_value_type() {
            return if any { self.cast(ty, true) } else { Err(self.invalid("unbox to a reference type")) };
        }
        let object = match self.pop()? {
            Value::Obj(Some(object)) => object,
            Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
            _ => return Err(self.invalid("unbox of a value that is not a reference")),
        };
        let (boxed_ty, value) = match self.heap.get(object) {
            Some(Object::Boxed { ty, value }) => (*ty, *value),
            _ => return Err(self.exception("System.InvalidCastException")),
        };
        // Перечисление и его базовый примитив взаимозаменяемы при распаковке.
        let underlying = |kind: Kind| -> Option<Prim> {
            match kind {
                Kind::Prim(p) | Kind::Enum(p) => Some(p),
                _ => None,
            }
        };
        let same = boxed_ty == ty
            || underlying(self.types[boxed_ty.0 as usize].kind)
                .is_some_and(|p| underlying(self.types[ty.0 as usize].kind) == Some(p));
        if !same {
            return Err(self.exception("System.InvalidCastException"));
        }
        if any {
            let value = self.copy_out(value)?;
            return self.push(value);
        }
        let pointer = match value {
            Value::Struct(inner) => Pointer::Struct(inner),
            _ => Pointer::Boxed(object),
        };
        self.push(Value::Ptr(pointer))
    }

    /// `castclass` (`throw` = true) и `isinst`.
    pub(crate) fn cast(&mut self, ty: TypeId, throw: bool) -> Result<(), VmError> {
        let value = self.pop()?;
        match value {
            Value::Obj(None) => self.push(value),
            Value::Obj(Some(object)) => {
                let actual = self.type_of_object(object)?;
                if self.assignable(actual, ty) {
                    self.push(value)
                } else if throw {
                    Err(self.exception("System.InvalidCastException"))
                } else {
                    self.push(Value::Obj(None))
                }
            }
            _ => Err(self.invalid("type check of a value that is not a reference")),
        }
    }

    /// `ldtoken`: пока только поле — для `RuntimeHelpers.InitializeArray`.
    pub(crate) fn load_token(&mut self, token: u32) -> Result<(), VmError> {
        let context = self.frames[self.frames.len() - 1].method;
        match self.resolve(context, token)? {
            Resolved::Field(field) => self.push(Value::Native((i64::from(field.asm) << 32) | i64::from(field.row))),
            // `typeof(T)`: вместо дескриптора сразу объект типа, и
            // `Type.GetTypeFromHandle` возвращает его как есть.
            Resolved::Type(ty) => {
                let object = self.type_object(ty)?;
                self.push(object)
            }
            _ => Err(VmError::Unsupported { what: format!("typeof and method handles in {} (phase N4)", self.location()) }),
        }
    }

    /// `RuntimeHelpers.InitializeArray`: элементы из данных сборки.
    pub(crate) fn initialize_array(&mut self, array: Value, handle: Value) -> Result<(), VmError> {
        let Value::Native(handle) = handle else {
            return Err(self.invalid("field handle is not a handle"));
        };
        let asm = (handle >> 32) as u16;
        let row = handle as u32;
        let (ty, length) = match array {
            Value::Obj(Some(object)) => match self.heap.get(object) {
                Some(Object::Array { ty, items }) => (*ty, items.len()),
                _ => return Err(self.invalid("InitializeArray on something that is not an array")),
            },
            _ => return Err(self.exception("System.NullReferenceException")),
        };
        let Kind::Array(element) = self.types[ty.0 as usize].kind else {
            return Err(self.invalid("array without an element type"));
        };
        let Store::Prim(prim) = self.types[element.0 as usize].store(element) else {
            return Err(VmError::Unsupported { what: String::from("array initializer of non-primitive elements") });
        };
        let a = self.assembly(asm);
        let mut rva = None;
        for rva_row in 1..=a.tables.rows(id::FIELD_RVA) {
            if a.tables.column(id::FIELD_RVA, rva_row, 1)? == row {
                rva = Some(a.tables.column(id::FIELD_RVA, rva_row, 0)?);
                break;
            }
        }
        let Some(rva) = rva else {
            return Err(self.invalid("array initializer field has no data"));
        };
        let size = prim.size();
        let data = a.image.from_rva(rva)?;
        let Some(data) = length.checked_mul(size).and_then(|bytes| data.get(..bytes)) else {
            return Err(self.invalid("array initializer data is shorter than the array"));
        };
        let Value::Obj(Some(object)) = array else { return Ok(()) };
        if let Some(Object::Array { items, .. }) = self.heap.get_mut(object) {
            for (index, bytes) in data.chunks_exact(size).enumerate().take(items.len()) {
                let mut raw = [0u8; 8];
                raw[..size].copy_from_slice(bytes);
                let bits = u64::from_le_bytes(raw);
                let value = match prim {
                    Prim::Bool | Prim::U1 | Prim::Char | Prim::U2 | Prim::U4 => Value::I32(bits as u32 as i32),
                    Prim::I1 => Value::I32(i32::from(bits as u8 as i8)),
                    Prim::I2 => Value::I32(i32::from(bits as u16 as i16)),
                    Prim::I4 => Value::I32(bits as u32 as i32),
                    Prim::I8 | Prim::U8 => Value::I64(bits as i64),
                    Prim::I | Prim::U => Value::Native(bits as i64),
                    Prim::R4 => Value::F32(f32::from_bits(bits as u32)),
                    Prim::R8 => Value::F(f64::from_bits(bits)),
                };
                items.set(index, value);
            }
        }
        Ok(())
    }

    /// Объект `System.Type` для типа — один на тип.
    pub(crate) fn type_object(&mut self, ty: TypeId) -> Result<Value, VmError> {
        if let Some(object) = self.type_objects.get(&ty) {
            return Ok(Value::Obj(Some(*object)));
        }
        self.corelib_type("System.RuntimeType")?;
        let object = self.heap.alloc(Object::RuntimeType(ty))?;
        self.type_objects.insert(ty, object);
        Ok(Value::Obj(Some(object)))
    }

    /// Первый аргумент члена `RuntimeType` — номер типа.
    pub(crate) fn runtime_type(&self, value: Value) -> Result<TypeId, VmError> {
        match value {
            Value::Obj(Some(object)) => match self.heap.get(object) {
                Some(Object::RuntimeType(ty)) => Ok(*ty),
                _ => Err(self.invalid("expected a System.Type")),
            },
            _ => Err(self.exception("System.NullReferenceException")),
        }
    }

    /// Имя без пространства имён и без внешних типов: `Puppy`, `Int32[]`.
    pub(crate) fn simple_name(&self, ty: TypeId) -> Result<String, VmError> {
        let t = &self.types[ty.0 as usize];
        if let Kind::Array(element) = t.kind {
            let mut name = self.simple_name(element)?;
            name.push_str("[]");
            return Ok(name);
        }
        match t.def {
            Some((asm, row)) => Ok(String::from(self.def_names(asm, row)?.1)),
            None => Ok(t.name.clone()),
        }
    }

    /// Поля двух значений равны — для `ValueType.Equals`.
    pub(crate) fn values_equal(&self, a: Value, b: Value, depth: u32) -> bool {
        if depth > 32 {
            return false;
        }
        match (a, b) {
            (Value::Struct(x), Value::Struct(y)) => match (self.heap.get(x), self.heap.get(y)) {
                (Some(Object::Struct { ty: tx, fields: fx }), Some(Object::Struct { ty: ty_, fields: fy })) => {
                    tx == ty_ && fx.len() == fy.len() && fx.iter().zip(fy).all(|(p, q)| self.values_equal(*p, *q, depth + 1))
                }
                _ => false,
            },
            (x, y) => x == y,
        }
    }

    /// Данные упаковки: тип и значение.
    pub(crate) fn boxed(&self, value: Value) -> Option<(TypeId, Value)> {
        match value {
            Value::Obj(Some(object)) => match self.heap.get(object) {
                Some(Object::Boxed { ty, value }) => Some((*ty, *value)),
                _ => None,
            },
            _ => None,
        }
    }
}
