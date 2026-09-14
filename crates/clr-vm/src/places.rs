//! Места, где лежат значения: стек вычислений, аргументы, переменные, поля,
//! элементы массивов — и копирование структур между ними (фаза N3a).
//!
//! Правило владения структурой описано в `value.rs`: чтение даёт копию
//! ([`Vm::copy_out`]), запись переписывает поля уже лежащего объекта
//! ([`Vm::store`]).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::Token;
use clr_meta::tables::id;

use crate::heap::Object;
use crate::ops::Fault;
use crate::types::{Asm, Store, TypeId};
use crate::value::{ObjRef, Pointer, Value};
use crate::vm::Vm;
use crate::{Host, VmError};

/// Самая глубокая вложенность структуры в структуру при копировании.
///
/// Структура не может содержать саму себя, но метаданные пришли из файла, и
/// рекурсивное копирование иначе исчерпало бы стек программы.
const MAX_STRUCT_DEPTH: u32 = 32;

impl<'a, H: Host> Vm<'a, H> {
    // --------------------------------------------------------------------
    // Стек вычислений
    // --------------------------------------------------------------------

    pub(crate) fn push(&mut self, value: Value) -> Result<(), VmError> {
        let Some(frame) = self.frames.last_mut() else {
            return Err(VmError::Invalid { what: "push without a frame", at: String::new() });
        };
        frame.stack.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        frame.stack.push(value);
        Ok(())
    }

    pub(crate) fn pop(&mut self) -> Result<Value, VmError> {
        match self.frames.last_mut().and_then(|frame| frame.stack.pop()) {
            Some(value) => Ok(value),
            None => Err(self.invalid("evaluation stack underflow")),
        }
    }

    pub(crate) fn peek(&self) -> Result<Value, VmError> {
        self.frames
            .last()
            .and_then(|frame| frame.stack.last().copied())
            .ok_or_else(|| self.invalid("evaluation stack underflow"))
    }

    /// Значение, лежащее `depth` позиций ниже вершины (0 — вершина).
    pub(crate) fn peek_at(&self, depth: usize) -> Result<Value, VmError> {
        let frame = self.frames.last().ok_or_else(|| self.invalid("evaluation stack underflow"))?;
        frame
            .stack
            .len()
            .checked_sub(depth + 1)
            .and_then(|index| frame.stack.get(index).copied())
            .ok_or_else(|| self.invalid("evaluation stack underflow"))
    }

    pub(crate) fn replace_at(&mut self, depth: usize, value: Value) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let len = self.frames[top].stack.len();
        match len.checked_sub(depth + 1) {
            Some(index) => {
                self.frames[top].stack[index] = value;
                Ok(())
            }
            None => Err(self.invalid("evaluation stack underflow")),
        }
    }


    pub(crate) fn pop_args(&mut self, count: usize) -> Result<Vec<Value>, VmError> {
        let top = self.frames.len() - 1;
        let depth = self.frames[top].stack.len();
        if depth < count {
            return Err(self.invalid("not enough arguments on the evaluation stack"));
        }
        let mut args = Vec::new();
        args.try_reserve_exact(count).map_err(|_| VmError::OutOfMemory)?;
        args.extend(self.frames[top].stack.drain(depth - count..));
        Ok(args)
    }

    // --------------------------------------------------------------------
    // Места
    // --------------------------------------------------------------------

    pub(crate) fn arg_pointer(&self, index: usize) -> Result<Value, VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].args.len() {
            return Err(self.invalid("argument number out of range"));
        }
        Ok(Value::Ptr(Pointer::Arg { frame: top as u32, index: index as u32 }))
    }

    pub(crate) fn local_pointer(&self, index: usize) -> Result<Value, VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].locals.len() {
            return Err(self.invalid("local variable number out of range"));
        }
        Ok(Value::Ptr(Pointer::Local { frame: top as u32, index: index as u32 }))
    }

    /// Прочитать место как есть, без копии.
    pub(crate) fn load(&self, pointer: Pointer) -> Result<Value, VmError> {
        let found = match pointer {
            Pointer::Local { frame, index } => {
                self.frames.get(frame as usize).and_then(|f| f.locals.get(index as usize).copied())
            }
            Pointer::Arg { frame, index } => {
                self.frames.get(frame as usize).and_then(|f| f.args.get(index as usize).copied())
            }
            Pointer::Element { array, index } => match self.heap.get(array) {
                Some(Object::Array { items, .. }) => match items.get(index as usize) {
                    Some(value) => Some(*value),
                    None => return Err(self.exception("System.IndexOutOfRangeException")),
                },
                _ => None,
            },
            Pointer::Field { object, index } => match self.heap.get(object) {
                Some(Object::Instance { fields, .. } | Object::Struct { fields, .. }) => {
                    fields.get(index as usize).copied()
                }
                _ => None,
            },
            Pointer::Static { ty, index } => {
                self.types.get(ty as usize).and_then(|t| t.statics.get(index as usize).copied())
            }
            Pointer::Boxed(object) => match self.heap.get(object) {
                Some(Object::Boxed { value, .. }) => Some(*value),
                _ => None,
            },
            Pointer::Struct(object) => Some(Value::Struct(object)),
        };
        found.ok_or_else(|| self.invalid("pointer to a place that does not exist"))
    }

    /// Записать в место. Структура переписывается на месте — номер объекта с
    /// её полями остаётся прежним, и указатели на неё не устаревают.
    pub(crate) fn store(&mut self, pointer: Pointer, value: Value) -> Result<(), VmError> {
        let current = self.load(pointer)?;
        if let (Value::Struct(destination), Value::Struct(source)) = (current, value) {
            if destination != source {
                self.assign_struct(destination, source, 0)?;
            }
            return Ok(());
        }
        let slot = match pointer {
            Pointer::Local { frame, index } => {
                self.frames.get_mut(frame as usize).and_then(|f| f.locals.get_mut(index as usize))
            }
            Pointer::Arg { frame, index } => {
                self.frames.get_mut(frame as usize).and_then(|f| f.args.get_mut(index as usize))
            }
            Pointer::Element { array, index } => match self.heap.get_mut(array) {
                Some(Object::Array { items, .. }) => items.get_mut(index as usize),
                _ => None,
            },
            Pointer::Field { object, index } => match self.heap.get_mut(object) {
                Some(Object::Instance { fields, .. } | Object::Struct { fields, .. }) => {
                    fields.get_mut(index as usize)
                }
                _ => None,
            },
            Pointer::Static { ty, index } => {
                self.types.get_mut(ty as usize).and_then(|t| t.statics.get_mut(index as usize))
            }
            Pointer::Boxed(object) => match self.heap.get_mut(object) {
                Some(Object::Boxed { value, .. }) => Some(value),
                _ => None,
            },
            Pointer::Struct(_) => None,
        };
        match slot {
            Some(slot) => {
                *slot = value;
                Ok(())
            }
            None => Err(self.invalid("store into a place that does not hold this kind of value")),
        }
    }

    /// Значение, которое можно положить на стек: структура копируется.
    pub(crate) fn copy_out(&mut self, value: Value) -> Result<Value, VmError> {
        match value {
            Value::Struct(object) => Ok(Value::Struct(self.clone_struct(object, 0)?)),
            other => Ok(other),
        }
    }


    fn clone_struct(&mut self, object: ObjRef, depth: u32) -> Result<ObjRef, VmError> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(VmError::Unsupported { what: String::from("structs nested more than 32 deep") });
        }
        let (ty, count) = match self.heap.get(object) {
            Some(Object::Struct { ty, fields }) => (*ty, fields.len()),
            _ => return Err(self.invalid("struct value that is not a struct")),
        };
        let mut copy = Vec::new();
        copy.try_reserve_exact(count).map_err(|_| VmError::OutOfMemory)?;
        for index in 0..count {
            let field = match self.heap.get(object) {
                Some(Object::Struct { fields, .. }) => fields[index],
                _ => return Err(self.invalid("struct value that is not a struct")),
            };
            copy.push(match field {
                Value::Struct(inner) => Value::Struct(self.clone_struct(inner, depth + 1)?),
                other => other,
            });
        }
        self.heap.alloc(Object::Struct { ty, fields: copy })
    }

    /// Переписать поля `destination` полями `source`. `source` после этого
    /// испорчен — он и так принадлежал только стеку.
    fn assign_struct(&mut self, destination: ObjRef, source: ObjRef, depth: u32) -> Result<(), VmError> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(VmError::Unsupported { what: String::from("structs nested more than 32 deep") });
        }
        let incoming = match self.heap.get_mut(source) {
            Some(Object::Struct { fields, .. }) => core::mem::take(fields),
            _ => return Err(self.invalid("struct value that is not a struct")),
        };
        let same_shape = matches!(self.heap.get(destination), Some(Object::Struct { fields, .. }) if fields.len() == incoming.len());
        if !same_shape {
            return Err(self.invalid("struct assigned to a place of another struct type"));
        }
        for (index, value) in incoming.into_iter().enumerate() {
            let current = match self.heap.get(destination) {
                Some(Object::Struct { fields, .. }) => fields[index],
                _ => return Err(self.invalid("struct value that is not a struct")),
            };
            match (current, value) {
                (Value::Struct(inner), Value::Struct(sub)) if inner != sub => {
                    self.assign_struct(inner, sub, depth + 1)?;
                }
                _ => {
                    if let Some(Object::Struct { fields, .. }) = self.heap.get_mut(destination) {
                        fields[index] = value;
                    }
                }
            }
        }
        Ok(())
    }

    /// Нулевое значение места.
    pub(crate) fn zero(&mut self, store: Store) -> Result<Value, VmError> {
        Ok(match store {
            Store::Prim(p) => p.zero(),
            Store::Ref | Store::ByRef => Value::Obj(None),
            Store::Struct(ty) => Value::Struct(self.zero_struct(ty, 0)?),
        })
    }


    fn zero_struct(&mut self, ty: TypeId, depth: u32) -> Result<ObjRef, VmError> {
        if depth > MAX_STRUCT_DEPTH {
            return Err(VmError::Unsupported {
                what: format!("struct {} contains itself", self.types[ty.0 as usize].name),
            });
        }
        let stores: Vec<Store> = self.types[ty.0 as usize].fields.iter().map(|slot| slot.store).collect();
        let mut fields = Vec::new();
        fields.try_reserve_exact(stores.len()).map_err(|_| VmError::OutOfMemory)?;
        for store in stores {
            fields.push(match store {
                Store::Struct(inner) => Value::Struct(self.zero_struct(inner, depth + 1)?),
                other => self.zero(other)?,
            });
        }
        self.heap.alloc(Object::Struct { ty, fields })
    }

    /// Обнулить место (`initobj`).
    pub(crate) fn clear(&mut self, pointer: Pointer, ty: TypeId) -> Result<(), VmError> {
        let store = self.types[ty.0 as usize].store(ty);
        let fresh = self.zero(store)?;
        self.store(pointer, fresh)
    }

    // --------------------------------------------------------------------
    // Строки и массивы
    // --------------------------------------------------------------------


    pub(crate) fn load_string(&mut self, asm: Asm, token: u32) -> Result<Value, VmError> {
        let t = Token::from_value(token);
        if t.table != id::USER_STRING {
            return Err(self.invalid("ldstr token is not a user string"));
        }
        if let Some(existing) = self.strings.get(&(asm, t.row)) {
            return Ok(Value::Obj(Some(*existing)));
        }
        let text = self.assembly(asm).root.user_strings.get(t.row)?;
        let reference = self.heap.string(text.units())?;
        self.strings.insert((asm, t.row), reference);
        Ok(Value::Obj(Some(reference)))
    }


    pub(crate) fn new_array(&mut self, element: TypeId, length: Value) -> Result<Value, VmError> {
        let count = match length {
            Value::I32(n) => i64::from(n),
            Value::Native(n) => n,
            _ => return Err(self.invalid("array length is not an integer")),
        };
        if count < 0 {
            return Err(self.exception("System.OverflowException"));
        }
        let ty = self.array_of(element)?;
        let store = self.types[element.0 as usize].store(element);
        let mut items = Vec::new();
        items.try_reserve_exact(count as usize).map_err(|_| VmError::OutOfMemory)?;
        if let Store::Struct(_) = store {
            for _ in 0..count {
                items.push(self.zero(store)?);
            }
        } else {
            items.resize(count as usize, self.zero(store)?);
        }
        Ok(Value::Obj(Some(self.heap.alloc(Object::Array { ty, items })?)))
    }

    pub(crate) fn element(&self, array: Value, index: Value) -> Result<(ObjRef, u32), VmError> {
        let reference = match array {
            Value::Obj(Some(reference)) => reference,
            Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
            _ => return Err(self.invalid("element access on a value that is not an array")),
        };
        let index = match index {
            Value::I32(i) => i64::from(i),
            Value::Native(i) => i,
            _ => return Err(self.invalid("array index is not an integer")),
        };
        let length = self.array_len(Value::Obj(Some(reference)))?;
        if index < 0 || index as usize >= length {
            return Err(self.exception("System.IndexOutOfRangeException"));
        }
        Ok((reference, index as u32))
    }

    pub(crate) fn array_len(&self, array: Value) -> Result<usize, VmError> {
        match array {
            Value::Obj(Some(reference)) => match self.heap.get(reference) {
                Some(Object::Array { items, .. }) => Ok(items.len()),
                _ => Err(self.invalid("length of something that is not an array")),
            },
            Value::Obj(None) => Err(self.exception("System.NullReferenceException")),
            _ => Err(self.invalid("length of a value that is not a reference")),
        }
    }

    // --------------------------------------------------------------------
    // Для членов библиотеки
    // --------------------------------------------------------------------

    pub(crate) fn string_units(&self, value: Value) -> Result<Option<Vec<u16>>, VmError> {
        match value {
            Value::Obj(None) => Ok(None),
            Value::Obj(Some(reference)) => match self.heap.get(reference) {
                Some(Object::String(units)) => {
                    let mut copy = Vec::new();
                    copy.try_reserve_exact(units.len()).map_err(|_| VmError::OutOfMemory)?;
                    copy.extend_from_slice(units);
                    Ok(Some(copy))
                }
                _ => Err(self.invalid("expected a string")),
            },
            _ => Err(self.invalid("expected a string reference")),
        }
    }

    pub(crate) fn print_units(&mut self, units: &[u16], newline: bool) {
        let mut text = String::new();
        // Одиночная половинка суррогатной пары печатается знаком замены — так
        // же поступает кодировщик UTF-8 в .NET.
        text.extend(char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')));
        if newline {
            text.push('\n');
        }
        self.host.write_out(&text);
    }

    pub(crate) fn int32(&self, value: Value) -> Result<i32, VmError> {
        match value {
            Value::I32(x) => Ok(x),
            _ => Err(self.invalid("expected an int32")),
        }
    }

    pub(crate) fn int64(&self, value: Value) -> Result<i64, VmError> {
        match value {
            Value::I64(x) => Ok(x),
            _ => Err(self.invalid("expected an int64")),
        }
    }

    /// Число, до которого добирается `this` метода примитива: по указателю,
    /// из упаковки или само значение.
    pub(crate) fn deref(&self, value: Value) -> Result<Value, VmError> {
        match value {
            Value::Ptr(pointer) => self.load(pointer),
            Value::Obj(Some(object)) => match self.heap.get(object) {
                Some(Object::Boxed { value, .. }) => Ok(*value),
                _ => Ok(value),
            },
            other => Ok(other),
        }
    }

    pub(crate) fn new_string_from(&mut self, text: &str) -> Result<Value, VmError> {
        Ok(Value::Obj(Some(self.heap.string(text.encode_utf16())?)))
    }

    // --------------------------------------------------------------------
    // Ошибки
    // --------------------------------------------------------------------

    /// Где сейчас исполнение: `Program::Main IL_0012`.
    pub(crate) fn location(&self) -> String {
        match self.frames.last() {
            Some(frame) => format!("{} IL_{:04x}", self.method_name(frame.method), frame.pc),
            None => String::from("the runtime"),
        }
    }


    pub(crate) fn invalid(&self, what: &'static str) -> VmError {
        VmError::Invalid { what, at: self.location() }
    }


    pub(crate) fn exception(&self, name: &'static str) -> VmError {
        VmError::Exception { name, at: self.location() }
    }


    pub(crate) fn fault(&self, fault: Fault) -> VmError {
        match fault {
            Fault::DivideByZero => self.exception("System.DivideByZeroException"),
            Fault::Overflow => self.exception("System.OverflowException"),
            Fault::Invalid(what) => self.invalid(what),
        }
    }


    pub(crate) fn unsupported_instruction(&self, op: u16) -> VmError {
        let topic = match op {
            0x29 | 0xFE06 | 0xFE07 => "delegates and function pointers (phase N3c)",
            0xFE1C => "sizeof (phase N4)",
            _ => "not implemented",
        };
        VmError::Unsupported { what: format!("IL instruction 0x{op:02x} in {}: {topic}", self.location()) }
    }
}

/// Сузить значение, прочитанное `ldind.X`.
pub(crate) fn narrow_load(op: u8, value: Value) -> Value {
    match (op, value) {
        (0x46, Value::I32(x)) => Value::I32(i32::from(x as i8)),
        (0x47, Value::I32(x)) => Value::I32(i32::from(x as u8)),
        (0x48, Value::I32(x)) => Value::I32(i32::from(x as i16)),
        (0x49, Value::I32(x)) => Value::I32(i32::from(x as u16)),
        _ => value,
    }
}

/// Сузить значение, записываемое `stind.X`.
pub(crate) fn narrow_store(op: u8, value: Value) -> Value {
    match (op, value) {
        (0x52, Value::I32(x)) => Value::I32(i32::from(x as i8)),
        (0x53, Value::I32(x)) => Value::I32(i32::from(x as i16)),
        _ => value,
    }
}
