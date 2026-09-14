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
use crate::types::{MethodId, Prim, TypeId};
use crate::value::{ObjRef, Value};

pub(crate) enum Object {
    /// Строка .NET — единицы UTF-16, как в самой среде.
    String(Vec<u16>),
    /// Одномерный массив; `ty` — тип массива (`int[]`), не элемента.
    Array { ty: TypeId, items: Items },
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

/// Элементы одномерного массива.
///
/// Массив примитивов хранит числа своей ширины, а не значения среды (16 байт
/// каждое): `byte[]` на четыре миллиона элементов занимает четыре мегабайта, а
/// не шестьдесят четыре, и помещается в кучу `/bin/dotnet` (фаза N4d, долг
/// N3d). Ссылки, структуры и `native int` лежат значениями.
pub(crate) enum Items {
    I8(Vec<i8>),
    /// `byte` и `bool`.
    U8(Vec<u8>),
    I16(Vec<i16>),
    /// `ushort` и `char`.
    U16(Vec<u16>),
    /// `int` и `uint` — одни и те же 32 бита.
    I32(Vec<i32>),
    /// `long` и `ulong`.
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    Values(Vec<Value>),
}

impl Items {
    /// `count` нулей для массива примитивов `prim`.
    pub(crate) fn zeroed(prim: Prim, count: usize) -> Result<Self, VmError> {
        fn filled<T: Clone>(count: usize, zero: T) -> Result<Vec<T>, VmError> {
            let mut items = Vec::new();
            items.try_reserve_exact(count).map_err(|_| VmError::OutOfMemory)?;
            items.resize(count, zero);
            Ok(items)
        }
        Ok(match prim {
            Prim::Bool | Prim::U1 => Self::U8(filled(count, 0)?),
            Prim::I1 => Self::I8(filled(count, 0)?),
            Prim::Char | Prim::U2 => Self::U16(filled(count, 0)?),
            Prim::I2 => Self::I16(filled(count, 0)?),
            Prim::I4 | Prim::U4 => Self::I32(filled(count, 0)?),
            Prim::I8 | Prim::U8 => Self::I64(filled(count, 0)?),
            Prim::R4 => Self::F32(filled(count, 0.0)?),
            Prim::R8 => Self::F64(filled(count, 0.0)?),
            Prim::I | Prim::U => Self::Values(filled(count, Value::Native(0))?),
        })
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            Self::I8(items) => items.len(),
            Self::U8(items) => items.len(),
            Self::I16(items) => items.len(),
            Self::U16(items) => items.len(),
            Self::I32(items) => items.len(),
            Self::I64(items) => items.len(),
            Self::F32(items) => items.len(),
            Self::F64(items) => items.len(),
            Self::Values(items) => items.len(),
        }
    }

    /// Оценка байт на элемент для счёта кучи.
    const fn element_bytes(&self) -> usize {
        match self {
            Self::I8(_) | Self::U8(_) => 1,
            Self::I16(_) | Self::U16(_) => 2,
            Self::I32(_) | Self::F32(_) => 4,
            Self::I64(_) | Self::F64(_) => 8,
            Self::Values(_) => 24,
        }
    }

    /// Элемент как значение на стеке: мелкие целые — `int32` с расширением
    /// знака или нулями по типу массива.
    pub(crate) fn get(&self, index: usize) -> Option<Value> {
        Some(match self {
            Self::I8(items) => Value::I32(i32::from(*items.get(index)?)),
            Self::U8(items) => Value::I32(i32::from(*items.get(index)?)),
            Self::I16(items) => Value::I32(i32::from(*items.get(index)?)),
            Self::U16(items) => Value::I32(i32::from(*items.get(index)?)),
            Self::I32(items) => Value::I32(*items.get(index)?),
            Self::I64(items) => Value::I64(*items.get(index)?),
            Self::F32(items) => Value::F32(*items.get(index)?),
            Self::F64(items) => Value::F(*items.get(index)?),
            Self::Values(items) => *items.get(index)?,
        })
    }

    /// Записать элемент, сузив до ширины массива. `false` — нет такого
    /// элемента или значение не того вида.
    pub(crate) fn set(&mut self, index: usize, value: Value) -> bool {
        fn put<T>(items: &mut [T], index: usize, item: T) -> bool {
            match items.get_mut(index) {
                Some(slot) => {
                    *slot = item;
                    true
                }
                None => false,
            }
        }
        let int = match value {
            Value::I32(x) => Some(i64::from(x)),
            Value::I64(x) | Value::Native(x) => Some(x),
            _ => None,
        };
        match self {
            Self::I8(items) => int.is_some_and(|x| put(items, index, x as i8)),
            Self::U8(items) => int.is_some_and(|x| put(items, index, x as u8)),
            Self::I16(items) => int.is_some_and(|x| put(items, index, x as i16)),
            Self::U16(items) => int.is_some_and(|x| put(items, index, x as u16)),
            Self::I32(items) => int.is_some_and(|x| put(items, index, x as i32)),
            Self::I64(items) => int.is_some_and(|x| put(items, index, x)),
            Self::F32(items) => match value {
                Value::F32(x) => put(items, index, x),
                Value::F(x) => put(items, index, x as f32),
                _ => false,
            },
            Self::F64(items) => match value {
                Value::F(x) => put(items, index, x),
                Value::F32(x) => put(items, index, f64::from(x)),
                _ => false,
            },
            Self::Values(items) => put(items, index, value),
        }
    }

    /// Значения среды внутри — ссылки для сборщика. У массива чисел их нет.
    pub(crate) fn values(&self) -> &[Value] {
        match self {
            Self::Values(items) => items,
            _ => &[],
        }
    }
}

fn estimate(object: &Object) -> usize {
    48 + match object {
        Object::String(units) => units.len() * 2,
        Object::Array { items, .. } => items.len() * items.element_bytes(),
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
