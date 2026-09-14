//! Куча объектов со сборкой мусора (фаза N3d).
//!
//! Номер объекта (`ObjRef`) — индекс слота. Освобождённый слот
//! переиспользуется следующим выделением, поэтому пропущенный корень не
//! роняет программу, а тихо подсовывает под старую ссылку чужой объект:
//! образец `gc` проверяет каждое живое значение в конце, а не только то, что
//! программа дожила.
//!
//! Размер кучи в байтах — оценка, а не счёт распределителя: 48 байт на объект
//! плюс содержимое по 24 байта на значение (две на единицу строки). Её хватает
//! решить, когда собирать; точный счёт стоил бы обёртки над распределителем
//! программы.

use alloc::vec::Vec;

use crate::VmError;
use crate::types::{MethodId, TypeId};
use crate::value::{ObjRef, Value};

pub(crate) enum Object {
    /// Строка .NET — единицы UTF-16, как в самой среде.
    String(Vec<u16>),
    /// Одномерный массив; `ty` — тип массива (`int[]`), не элемента.
    Array { ty: TypeId, items: Vec<Value> },
    /// Экземпляр класса: поля базовых классов первыми.
    Instance { ty: TypeId, fields: Vec<Value> },
    /// Поля значения структуры (см. [`Value::Struct`]).
    Struct { ty: TypeId, fields: Vec<Value> },
    /// Упакованное значение: число или `Value::Struct`.
    Boxed { ty: TypeId, value: Value },
    /// Объект `System.Type`, который возвращает `GetType()`.
    RuntimeType(TypeId),
    /// Делегат: список вызовов по порядку — цель (`null` у статического
    /// метода) и метод.
    Delegate { ty: TypeId, targets: Vec<(Value, MethodId)> },
}

/// Сколько выделить с прошлой сборки, прежде чем собирать снова, — не меньше.
///
/// Куча `/bin/dotnet` — 16 МиБ, а оценка не видит издержек распределителя
/// и дробления: собирать надо задолго до края.
const MIN_THRESHOLD: usize = 2 * 1024 * 1024;

fn estimate(object: &Object) -> usize {
    48 + match object {
        Object::String(units) => units.len() * 2,
        Object::Array { items, .. } => items.len() * 24,
        Object::Instance { fields, .. } | Object::Struct { fields, .. } => fields.len() * 24,
        Object::Boxed { .. } => 24,
        Object::RuntimeType(_) => 0,
        Object::Delegate { targets, .. } => targets.len() * 32,
    }
}

pub(crate) struct Heap {
    slots: Vec<Option<Object>>,
    free: Vec<u32>,
    live: usize,
    live_bytes: usize,
    since_collect: usize,
    threshold: usize,
    collections: u32,
}

impl Heap {
    pub(crate) const fn new() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            live_bytes: 0,
            since_collect: 0,
            threshold: MIN_THRESHOLD,
            collections: 0,
        }
    }

    /// Выделить объект. Сама никогда не собирает: член в Rust, который зовёт
    /// выделение, держит номера объектов в своих переменных, и сборка их бы
    /// не увидела. Собирает цикл исполнения в безопасной точке (`gc.rs`).
    pub(crate) fn alloc(&mut self, object: Object) -> Result<ObjRef, VmError> {
        let bytes = estimate(&object);
        let index = match self.free.pop() {
            Some(index) => {
                self.slots[index as usize] = Some(object);
                index
            }
            None => {
                let index = u32::try_from(self.slots.len()).map_err(|_| VmError::OutOfMemory)?;
                self.slots.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
                self.slots.push(Some(object));
                index
            }
        };
        self.live += 1;
        self.live_bytes += bytes;
        self.since_collect += bytes;
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
        self.slots.get(reference.0 as usize).and_then(Option::as_ref)
    }

    pub(crate) fn get_mut(&mut self, reference: ObjRef) -> Option<&mut Object> {
        self.slots.get_mut(reference.0 as usize).and_then(Option::as_mut)
    }

    /// Живые объекты.
    pub(crate) const fn len(&self) -> usize {
        self.live
    }

    pub(crate) const fn collections(&self) -> u32 {
        self.collections
    }

    pub(crate) const fn wants_collection(&self) -> bool {
        self.since_collect >= self.threshold
    }

    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Освободить всё непомеченное.
    pub(crate) fn sweep(&mut self, marked: &[bool]) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if marked.get(index).copied().unwrap_or(false) {
                continue;
            }
            if let Some(object) = slot.take() {
                self.live_bytes = self.live_bytes.saturating_sub(estimate(&object));
                self.live -= 1;
                // Место в `free` уже зарезервировано: оно не больше числа
                // слотов, а слоты выделялись с резервом.
                if self.free.try_reserve(1).is_ok() {
                    self.free.push(index as u32);
                }
            }
        }
        self.since_collect = 0;
        self.threshold = MIN_THRESHOLD.max(self.live_bytes);
        self.collections += 1;
    }
}
