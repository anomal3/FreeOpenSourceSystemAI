//! Куча объектов.
//!
//! В фазе N2 объектов три вида — строка, одномерный массив и объект кодировки,
//! — и живут они до конца программы: сборщика мусора ещё нет, он приходит в
//! фазе N3 вместе с объектами пользователя. Для программ этой фазы это честно:
//! строки `ldstr` не выделяются заново (см. `Vm::load_string`), а склейки в
//! цикле растят кучу — и об этом говорит число объектов в строке завершения
//! `/bin/dotnet`.

use alloc::vec::Vec;

use crate::VmError;
use crate::value::{ObjRef, Value};

pub(crate) enum Object {
    /// Строка .NET — единицы UTF-16, как в самой среде.
    String(Vec<u16>),
    Array(Vec<Value>),
    /// `System.Text.Encoding`: нужен только как значение, которое программа
    /// передаёт обратно в `Console.OutputEncoding`.
    Encoding,
}

pub(crate) struct Heap {
    objects: Vec<Object>,
}

impl Heap {
    pub(crate) const fn new() -> Self {
        Self { objects: Vec::new() }
    }

    pub(crate) fn alloc(&mut self, object: Object) -> Result<ObjRef, VmError> {
        let index = u32::try_from(self.objects.len()).map_err(|_| VmError::OutOfMemory)?;
        self.objects.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        self.objects.push(object);
        Ok(ObjRef(index))
    }

    pub(crate) fn string(&mut self, units: impl Iterator<Item = u16>) -> Result<ObjRef, VmError> {
        let mut text = Vec::new();
        for unit in units {
            text.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
            text.push(unit);
        }
        self.alloc(Object::String(text))
    }

    pub(crate) fn get(&self, reference: ObjRef) -> Option<&Object> {
        self.objects.get(reference.0 as usize)
    }

    pub(crate) fn get_mut(&mut self, reference: ObjRef) -> Option<&mut Object> {
        self.objects.get_mut(reference.0 as usize)
    }

    pub(crate) fn len(&self) -> usize {
        self.objects.len()
    }
}
