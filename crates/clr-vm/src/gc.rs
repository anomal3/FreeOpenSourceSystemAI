//! Сборщик мусора: пометка от корней и сборка остального (фаза N3d).
//!
//! # Точный
//!
//! Среда знает, где ссылки: они лежат в кадрах, статических полях и нескольких
//! таблицах самой среды, и каждое значение помечено своим видом (`Value`).
//! Консервативно сканировать память, гадая, похоже ли число на адрес, не нужно.
//!
//! # Когда
//!
//! Только в безопасной точке — в начале оборота цикла исполнения, между
//! инструкциями. Там все живые ссылки программы уже разложены по кадрам. Член
//! в Rust посреди работы держит номера объектов в своих переменных, и сборка
//! внутри выделения их бы не увидела и освободила.
//!
//! # Без рекурсии
//!
//! Пометка — вектор-очередь номеров. Цепочку из миллиона узлов рекурсивный
//! обход не пережил бы даже на стеке в мегабайт.

use alloc::vec::Vec;

use crate::eh::Continuation;
use crate::heap::Object;
use crate::value::{ObjRef, Pointer, Value};
use crate::vm::{FrameKind, Vm};
use crate::{Host, VmError};

/// Номер объекта, на который ссылается значение, если ссылается.
const fn referenced(value: Value) -> Option<ObjRef> {
    match value {
        Value::Obj(Some(object))
        | Value::Struct(object)
        | Value::Ptr(Pointer::Element { array: object, .. })
        | Value::Ptr(Pointer::Field { object, .. })
        | Value::Ptr(Pointer::Boxed(object))
        | Value::Ptr(Pointer::Struct(object)) => Some(object),
        _ => None,
    }
}

fn push(work: &mut Vec<u32>, value: Value) -> Result<(), VmError> {
    if let Some(object) = referenced(value) {
        work.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        work.push(object.0);
    }
    Ok(())
}

impl<H: Host> Vm<'_, H> {
    /// Собрать, если с прошлой сборки выделено достаточно.
    pub(crate) fn collect_if_needed(&mut self) -> Result<(), VmError> {
        if self.heap.wants_collection() {
            self.collect()?;
        }
        Ok(())
    }

    pub(crate) fn collect(&mut self) -> Result<(), VmError> {
        let mut work = Vec::new();
        for frame in &self.frames {
            for value in frame.args.iter().chain(&frame.locals).chain(&frame.stack) {
                push(&mut work, *value)?;
            }
            for (_, exception) in &frame.caught {
                push(&mut work, Value::Obj(Some(*exception)))?;
            }
            for continuation in &frame.continuations {
                if let Continuation::Unwind { exception, .. } = continuation {
                    push(&mut work, Value::Obj(Some(*exception)))?;
                }
            }
            if let Some(exception) = frame.then_throw {
                push(&mut work, Value::Obj(Some(exception)))?;
            }
            if let FrameKind::Filter { exception, .. } = frame.kind {
                push(&mut work, Value::Obj(Some(exception)))?;
            }
        }
        for ty in &self.types {
            for value in &ty.statics {
                push(&mut work, *value)?;
            }
        }
        for object in self.strings.values().chain(self.type_objects.values()) {
            push(&mut work, Value::Obj(Some(*object)))?;
        }

        let mut marked = Vec::new();
        marked.try_reserve_exact(self.heap.slot_count()).map_err(|_| VmError::OutOfMemory)?;
        marked.resize(self.heap.slot_count(), false);
        while let Some(index) = work.pop() {
            let Some(mark) = marked.get_mut(index as usize) else { continue };
            if *mark {
                continue;
            }
            *mark = true;
            match self.heap.get(ObjRef(index)) {
                Some(Object::Array { items: values, .. })
                | Some(Object::Instance { fields: values, .. })
                | Some(Object::Struct { fields: values, .. }) => {
                    for value in values {
                        push(&mut work, *value)?;
                    }
                }
                Some(Object::Boxed { value, .. }) => push(&mut work, *value)?,
                Some(Object::Delegate { targets, .. }) => {
                    for (target, _) in targets {
                        push(&mut work, *target)?;
                    }
                }
                Some(Object::String(_) | Object::RuntimeType(_)) | None => {}
            }
        }
        self.heap.sweep(&marked);
        Ok(())
    }
}
