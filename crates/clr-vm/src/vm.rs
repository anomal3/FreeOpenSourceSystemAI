//! Цикл исполнения: кадры и инструкции IL.
//!
//! Что делают вызовы, поля и упаковка — `objects.rs`; где лежат значения —
//! `places.rs`; типы и токены — `loader.rs` и `dispatch.rs`.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::tables::id;
use clr_meta::{Assembly, Token};

use crate::eh::Continuation;
use crate::heap::{Heap, Object};
use crate::ops;
use crate::places::{narrow_load, narrow_store};
use crate::types::{Asm, Body, MethodId, MethodInfo, PROGRAM, Resolved, Type, TypeId, TYPE_BEFORE_FIELD_INIT};
use crate::value::{ObjRef, Pointer, Value};
use crate::{Host, VmError};

/// Сколько вызовов может быть вложено друг в друга.
///
/// Предел ради памяти, а не ради стека Rust: кадры лежат в векторе. Две тысячи
/// с лишним — глубина рекурсии, до которой настоящие программы с окнами не
/// доходят, а бесконечная рекурсия упирается в него за доли секунды и
/// называется переполнением стека, как и в .NET.
pub const MAX_FRAMES: usize = 2048;

pub(crate) struct Frame<'a> {
    pub method: MethodId,
    pub body: Rc<Body<'a>>,
    pub pc: usize,
    pub args: Vec<Value>,
    pub locals: Vec<Value>,
    pub stack: Vec<Value>,
    /// Тип из префикса `constrained.` — для следующего `callvirt`.
    pub constrained: Option<TypeId>,
    pub kind: FrameKind,
    /// Что делать по `endfinally` выполняемых сейчас `finally` (см. `eh.rs`).
    pub continuations: Vec<Continuation>,
    /// Исключения работающих обработчиков `catch` — для `rethrow`.
    pub caught: Vec<(u32, ObjRef)>,
    /// Кадр конструктора исключения, брошенного средой: объект бросается,
    /// когда конструктор вернётся.
    pub then_throw: Option<ObjRef>,
    /// Не класть значение, которое вернёт метод: это не последний вызов в
    /// списке делегата, и результат у делегата — от последнего.
    pub discard_result: bool,
}

#[derive(Clone, Copy)]
pub(crate) enum FrameKind {
    Normal,
    /// Фильтр `when` обработчика `clause` в кадре `owner`: он забрал аргументы
    /// и переменные владельца и вернёт их на `endfilter`.
    Filter { owner: usize, clause: u32, exception: ObjRef },
}

impl<'a> Frame<'a> {
    pub(crate) const fn new(
        method: MethodId,
        body: Rc<Body<'a>>,
        args: Vec<Value>,
        locals: Vec<Value>,
        stack: Vec<Value>,
    ) -> Self {
        Self {
            method,
            body,
            pc: 0,
            args,
            locals,
            stack,
            constrained: None,
            kind: FrameKind::Normal,
            continuations: Vec::new(),
            caught: Vec::new(),
            then_throw: None,
            discard_result: false,
        }
    }
}

/// Среда выполнения программы вместе с базовой библиотекой.
pub struct Vm<'a, H: Host> {
    /// Базовая библиотека (`types::CORELIB`) и программа (`types::PROGRAM`).
    pub(crate) asms: [Assembly<'a>; 2],
    pub(crate) host: H,
    pub(crate) heap: Heap,
    pub(crate) frames: Vec<Frame<'a>>,
    pub(crate) types: Vec<Type>,
    pub(crate) type_map: BTreeMap<(Asm, u32, Vec<TypeId>), TypeId>,
    pub(crate) array_types: BTreeMap<TypeId, TypeId>,
    pub(crate) corelib_types: BTreeMap<&'static str, TypeId>,
    pub(crate) typerefs: BTreeMap<(Asm, u32), (Asm, u32)>,
    pub(crate) load_depth: u32,
    pub(crate) methods: Vec<MethodInfo<'a>>,
    pub(crate) method_map: BTreeMap<(Asm, u32, TypeId, Vec<TypeId>), MethodId>,
    /// Номер ячейки таблицы виртуальных методов у каждого виртуального метода.
    pub(crate) slots: BTreeMap<MethodId, usize>,
    pub(crate) dispatch_cache: BTreeMap<(TypeId, MethodId), MethodId>,
    pub(crate) resolved: BTreeMap<(MethodId, u32), Resolved>,
    /// Владельцы методов и полей: `[сборка * 2 + (поле ли)]`, строятся при
    /// первом обращении.
    pub(crate) owners: [Vec<u32>; 4],
    /// Объекты строк `ldstr`: литерал выделяется один раз, как интернированная
    /// строка в .NET, — иначе цикл со строкой внутри рос бы в куче на каждом
    /// обороте.
    pub(crate) strings: BTreeMap<(Asm, u32), ObjRef>,
    pub(crate) type_objects: BTreeMap<TypeId, ObjRef>,
    /// Сколько инструкций выполнено.
    pub instructions: u64,
}

impl<'a, H: Host> Vm<'a, H> {
    /// Подготовить программу к запуску поверх базовой библиотеки.
    pub fn new(program: &'a [u8], corelib: &'a [u8], host: H) -> Result<Self, VmError> {
        Ok(Self {
            asms: [Assembly::parse(corelib)?, Assembly::parse(program)?],
            host,
            heap: Heap::new(),
            frames: Vec::new(),
            types: Vec::new(),
            type_map: BTreeMap::new(),
            array_types: BTreeMap::new(),
            corelib_types: BTreeMap::new(),
            typerefs: BTreeMap::new(),
            load_depth: 0,
            methods: Vec::new(),
            method_map: BTreeMap::new(),
            slots: BTreeMap::new(),
            dispatch_cache: BTreeMap::new(),
            resolved: BTreeMap::new(),
            owners: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            strings: BTreeMap::new(),
            type_objects: BTreeMap::new(),
            instructions: 0,
        })
    }

    /// Отдать то, во что среда печатала.
    pub fn into_host(self) -> H {
        self.host
    }

    #[must_use]
    pub fn type_count(&self) -> u32 {
        self.asms[usize::from(PROGRAM)].tables.rows(id::TYPE_DEF)
    }

    #[must_use]
    pub fn method_count(&self) -> u32 {
        self.asms[usize::from(PROGRAM)].tables.rows(id::METHOD_DEF)
    }

    /// Живые объекты — после последней сборки мусора и того, что выделено с неё.
    #[must_use]
    pub fn object_count(&self) -> usize {
        self.heap.len()
    }

    /// Сколько раз собирался мусор.
    #[must_use]
    pub fn collections(&self) -> u32 {
        self.heap.collections()
    }

    /// Выполнить точку входа. Возвращает то, что вернул `Main` (ноль у `void`).
    pub fn run_main(&mut self, args: &[&str]) -> Result<i32, VmError> {
        let entry = Token::from_value(self.asms[usize::from(PROGRAM)].cli.entry_point);
        if entry.table != id::METHOD_DEF || entry.is_nil() {
            return Err(VmError::NoEntryPoint);
        }
        let owner_row = self.owner_row(PROGRAM, entry.row, false)?;
        let owner = self.load_def(PROGRAM, owner_row, Rc::from([]))?;
        let method = self.method_id(PROGRAM, entry.row, owner, Rc::from([]))?;
        let mut call_args = Vec::new();
        match self.methods[method.0 as usize].params {
            0 => {}
            1 => {
                let string = self.corelib_type("System.String")?;
                let array_ty = self.array_of(string)?;
                let mut items = Vec::new();
                items.try_reserve_exact(args.len()).map_err(|_| VmError::OutOfMemory)?;
                for arg in args {
                    items.push(Value::Obj(Some(self.heap.string(arg.encode_utf16())?)));
                }
                let array = self.heap.alloc(Object::Array { ty: array_ty, items })?;
                call_args.push(Value::Obj(Some(array)));
            }
            _ => {
                return Err(VmError::Unsupported {
                    what: String::from("an entry point with more than one parameter"),
                });
            }
        }
        self.enter(method, call_args)?;
        // Статический конструктор типа с `Main` выполняется раньше `Main`: его
        // кадр ложится сверху.
        if self.types[owner.0 as usize].flags & TYPE_BEFORE_FIELD_INIT == 0 {
            self.ensure_initialized(owner)?;
        }
        Ok(match self.execute(0)? {
            Some(Value::I32(code)) => code,
            _ => 0,
        })
    }

    /// Исполнять, пока число кадров не опустится до `floor`.
    ///
    /// Исключение, которое бросила сама среда посреди инструкции
    /// ([`VmError::Exception`]: `null.Length`, выход за массив), становится
    /// объектом и летит в программу — всё состояние в кадрах, и цикл просто
    /// начинается заново. Наружу уходит только то, чего программа не поймала.
    fn execute(&mut self, floor: usize) -> Result<Option<Value>, VmError> {
        loop {
            match self.run_frames(floor) {
                Err(VmError::Exception { name, .. }) if self.frames.len() > floor => self.throw_runtime(name)?,
                other => return other,
            }
        }
    }

    fn run_frames(&mut self, floor: usize) -> Result<Option<Value>, VmError> {
        loop {
            // Безопасная точка: между инструкциями все живые ссылки лежат в
            // кадрах и статике (см. `gc.rs`).
            self.collect_if_needed()?;
            self.instructions += 1;
            let top = self.frames.len() - 1;
            let code = self.frames[top].body.code;
            let pc = self.frames[top].pc;
            let Some(&op) = code.get(pc) else {
                return Err(self.invalid("execution ran past the end of the method"));
            };
            let mut next = pc + 1;
            match op {
                // nop, break
                0x00 | 0x01 => {}
                // ldarg.0 … ldarg.3
                0x02..=0x05 => self.push_place(Pointer::Arg { frame: top as u32, index: u32::from(op - 0x02) })?,
                // ldloc.0 … ldloc.3
                0x06..=0x09 => self.push_place(Pointer::Local { frame: top as u32, index: u32::from(op - 0x06) })?,
                // stloc.0 … stloc.3
                0x0A..=0x0D => self.pop_to(Pointer::Local { frame: top as u32, index: u32::from(op - 0x0A) })?,
                0x0E => {
                    let index = self.operand_u8(code, &mut next)?;
                    self.push_place(Pointer::Arg { frame: top as u32, index })?;
                }
                0x0F => {
                    let index = self.operand_u8(code, &mut next)?;
                    let pointer = self.arg_pointer(index as usize)?;
                    self.push(pointer)?;
                }
                0x10 => {
                    let index = self.operand_u8(code, &mut next)?;
                    self.pop_to(Pointer::Arg { frame: top as u32, index })?;
                }
                0x11 => {
                    let index = self.operand_u8(code, &mut next)?;
                    self.push_place(Pointer::Local { frame: top as u32, index })?;
                }
                0x12 => {
                    let index = self.operand_u8(code, &mut next)?;
                    let pointer = self.local_pointer(index as usize)?;
                    self.push(pointer)?;
                }
                0x13 => {
                    let index = self.operand_u8(code, &mut next)?;
                    self.pop_to(Pointer::Local { frame: top as u32, index })?;
                }
                // ldnull
                0x14 => self.push(Value::Obj(None))?,
                // ldc.i4.m1 … ldc.i4.8
                0x15..=0x1E => self.push(Value::I32(i32::from(op) - 0x16))?,
                0x1F => {
                    let [byte] = self.operand::<1>(code, &mut next)?;
                    self.push(Value::I32(i32::from(byte as i8)))?;
                }
                0x20 => {
                    let bytes = self.operand::<4>(code, &mut next)?;
                    self.push(Value::I32(i32::from_le_bytes(bytes)))?;
                }
                0x21 => {
                    let bytes = self.operand::<8>(code, &mut next)?;
                    self.push(Value::I64(i64::from_le_bytes(bytes)))?;
                }
                0x22 => {
                    let bytes = self.operand::<4>(code, &mut next)?;
                    self.push(Value::F32(f32::from_le_bytes(bytes)))?;
                }
                0x23 => {
                    let bytes = self.operand::<8>(code, &mut next)?;
                    self.push(Value::F(f64::from_le_bytes(bytes)))?;
                }
                // dup: копия структуры, а не второй владелец.
                0x25 => {
                    let value = self.peek()?;
                    let value = self.copy_out(value)?;
                    self.push(value)?;
                }
                // pop
                0x26 => {
                    self.pop()?;
                }
                // call, callvirt
                0x28 | 0x6F => {
                    let token = self.operand_u32(code, &mut next)?;
                    self.call(token, op == 0x6F, next)?;
                    continue;
                }
                // ret
                0x2A => {
                    let returns = self.methods[self.frames[top].method.0 as usize].returns;
                    let value = if returns { Some(self.pop()?) } else { None };
                    let finished = self.frames.pop();
                    if self.frames.len() == floor {
                        return Ok(value);
                    }
                    let (then_throw, discard) =
                        finished.map_or((None, false), |frame| (frame.then_throw, frame.discard_result));
                    if let Some(exception) = then_throw {
                        self.raise(exception)?;
                        continue;
                    }
                    if discard {
                        continue;
                    }
                    if let Some(value) = value {
                        self.push(value)?;
                    }
                    continue;
                }
                // br.s … blt.un.s
                0x2B..=0x37 => {
                    let [byte] = self.operand::<1>(code, &mut next)?;
                    if self.branch(op - 0x2B)? {
                        next = self.target(code, next, i64::from(byte as i8))?;
                    }
                }
                // br … blt.un
                0x38..=0x44 => {
                    let offset = self.operand_u32(code, &mut next)? as i32;
                    if self.branch(op - 0x38)? {
                        next = self.target(code, next, i64::from(offset))?;
                    }
                }
                // switch
                0x45 => {
                    let count = self.operand_u32(code, &mut next)? as usize;
                    let table = next;
                    let Some(after) = count
                        .checked_mul(4)
                        .and_then(|bytes| table.checked_add(bytes))
                        .filter(|&end| end <= code.len())
                    else {
                        return Err(self.invalid("switch table runs past the end of the method"));
                    };
                    let index = match self.pop()? {
                        Value::I32(value) => value as u32 as usize,
                        _ => return Err(self.invalid("switch on a value that is not an int32")),
                    };
                    next = after;
                    if index < count {
                        let mut at = table + index * 4;
                        let offset = self.operand_u32(code, &mut at)? as i32;
                        next = self.target(code, after, i64::from(offset))?;
                    }
                }
                // ldind.i1 … ldind.ref
                0x46..=0x50 => {
                    let pointer = self.pop_pointer()?;
                    let value = self.load(pointer)?;
                    self.push(narrow_load(op, value))?;
                }
                // stind.ref … stind.r8, stind.i
                0x51..=0x57 | 0xDF => {
                    let value = self.pop()?;
                    let pointer = self.pop_pointer()?;
                    self.store(pointer, narrow_store(op, value))?;
                }
                // add … xor, add.ovf … sub.ovf.un
                0x58..=0x61 | 0xD6..=0xDB => {
                    let right = self.pop()?;
                    let left = self.pop()?;
                    let result = ops::binary(op, left, right).map_err(|fault| self.fault(fault))?;
                    self.push(result)?;
                }
                // shl, shr, shr.un
                0x62..=0x64 => {
                    let amount = self.pop()?;
                    let value = self.pop()?;
                    let result = ops::shift(op, value, amount).map_err(|fault| self.fault(fault))?;
                    self.push(result)?;
                }
                // neg, not
                0x65 | 0x66 => {
                    let value = self.pop()?;
                    let result = ops::unary(op, value).map_err(|fault| self.fault(fault))?;
                    self.push(result)?;
                }
                // conv.*
                0x67..=0x6E | 0x76 | 0x82..=0x8B | 0xB3..=0xBA | 0xD1..=0xD5 | 0xE0 => {
                    let value = self.pop()?;
                    let result = ops::convert(op, value).map_err(|fault| self.fault(fault))?;
                    self.push(result)?;
                }
                // cpobj
                0x70 => {
                    self.operand_u32(code, &mut next)?;
                    let source = self.pop_pointer()?;
                    let destination = self.pop_pointer()?;
                    let value = self.load(source)?;
                    let value = self.copy_out(value)?;
                    self.store(destination, value)?;
                }
                // ldobj
                0x71 => {
                    let token = self.operand_u32(code, &mut next)?;
                    let ty = self.resolve_type(self.frames[top].method, token)?;
                    let pointer = self.pop_pointer()?;
                    let value = self.load(pointer)?;
                    let value = self.copy_out(value)?;
                    let store = self.types[ty.0 as usize].store(ty);
                    self.push(store.narrow(value))?;
                }
                // ldstr
                0x72 => {
                    let token = self.operand_u32(code, &mut next)?;
                    let asm = self.methods[self.frames[top].method.0 as usize].asm;
                    let value = self.load_string(asm, token)?;
                    self.push(value)?;
                }
                // newobj
                0x73 => {
                    let token = self.operand_u32(code, &mut next)?;
                    self.new_object(token, next)?;
                    continue;
                }
                // castclass, isinst
                0x74 | 0x75 => {
                    let token = self.operand_u32(code, &mut next)?;
                    let ty = self.resolve_type(self.frames[top].method, token)?;
                    self.cast(ty, op == 0x74)?;
                }
                // unbox, unbox.any
                0x79 | 0xA5 => {
                    let token = self.operand_u32(code, &mut next)?;
                    let ty = self.resolve_type(self.frames[top].method, token)?;
                    self.unbox(ty, op == 0xA5)?;
                }
                // throw
                0x7A => {
                    match self.pop()? {
                        Value::Obj(Some(exception)) => self.raise(exception)?,
                        Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
                        _ => return Err(self.invalid("throw of a value that is not a reference")),
                    }
                    continue;
                }
                // endfinally
                0xDC => {
                    self.end_finally()?;
                    continue;
                }
                // ldfld, ldflda
                0x7B | 0x7C => {
                    let token = self.operand_u32(code, &mut next)?;
                    self.load_field(token, op == 0x7C)?;
                }
                // stfld
                0x7D => {
                    let token = self.operand_u32(code, &mut next)?;
                    self.store_field(token)?;
                }
                // ldsfld, ldsflda, stsfld
                0x7E..=0x80 => {
                    let token = self.operand_u32(code, &mut next)?;
                    if !self.static_field(op, token)? {
                        continue;
                    }
                }
                // stobj
                0x81 => {
                    let token = self.operand_u32(code, &mut next)?;
                    let ty = self.resolve_type(self.frames[top].method, token)?;
                    let value = self.pop()?;
                    let pointer = self.pop_pointer()?;
                    let store = self.types[ty.0 as usize].store(ty);
                    self.store(pointer, store.narrow(value))?;
                }
                // box
                0x8C => {
                    let token = self.operand_u32(code, &mut next)?;
                    let ty = self.resolve_type(self.frames[top].method, token)?;
                    let value = self.pop()?;
                    let boxed = self.box_value(ty, value)?;
                    self.push(boxed)?;
                }
                // newarr
                0x8D => {
                    let token = self.operand_u32(code, &mut next)?;
                    let element = self.resolve_type(self.frames[top].method, token)?;
                    let length = self.pop()?;
                    let array = self.new_array(element, length)?;
                    self.push(array)?;
                }
                // ldlen
                0x8E => {
                    let array = self.pop()?;
                    let length = self.array_len(array)?;
                    self.push(Value::Native(length as i64))?;
                }
                // ldelema
                0x8F => {
                    self.operand_u32(code, &mut next)?;
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    self.push(Value::Ptr(Pointer::Element { array, index }))?;
                }
                // ldelem.i1 … ldelem.ref, ldelem <T>
                0x90..=0x9A | 0xA3 => {
                    if op == 0xA3 {
                        self.operand_u32(code, &mut next)?;
                    }
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    let value = self.load(Pointer::Element { array, index })?;
                    let value = self.copy_out(value)?;
                    // Элементы лежат так же, как значения по указателю:
                    // `ldelem.X` — это `ldind.X`, сдвинутый на 0x4A.
                    let value = if op == 0xA3 { value } else { narrow_load(op - 0x4A, value) };
                    self.push(value)?;
                }
                // stelem.i … stelem.ref, stelem <T>
                0x9B..=0xA2 | 0xA4 => {
                    if op == 0xA4 {
                        self.operand_u32(code, &mut next)?;
                    }
                    let value = self.pop()?;
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    let stind = match op {
                        0x9C..=0xA1 => op - 0x4A,
                        _ => 0x51,
                    };
                    self.store(Pointer::Element { array, index }, narrow_store(stind, value))?;
                }
                // ldtoken
                0xD0 => {
                    let token = self.operand_u32(code, &mut next)?;
                    self.load_token(token)?;
                }
                // leave, leave.s: выход из защищённого блока через все
                // `finally` по дороге (см. `eh.rs`).
                0xDD | 0xDE => {
                    let offset = if op == 0xDD {
                        i64::from(self.operand_u32(code, &mut next)? as i32)
                    } else {
                        let [byte] = self.operand::<1>(code, &mut next)?;
                        i64::from(byte as i8)
                    };
                    let target = self.target(code, next, offset)?;
                    self.leave(pc, target)?;
                    continue;
                }
                0xFE => {
                    let [second] = self.operand::<1>(code, &mut next)?;
                    match second {
                        // ceq, cgt, cgt.un, clt, clt.un
                        0x01..=0x05 => {
                            let right = self.pop()?;
                            let left = self.pop()?;
                            let c = ops::compare(left, right).map_err(|fault| self.fault(fault))?;
                            let result = match second {
                                0x01 => c.equal,
                                0x02 => c.greater(),
                                0x03 => c.greater_unordered(),
                                0x04 => c.less(),
                                _ => c.less_unordered(),
                            };
                            self.push(Value::I32(i32::from(result)))?;
                        }
                        0x09 => {
                            let index = self.operand_u16(code, &mut next)?;
                            self.push_place(Pointer::Arg { frame: top as u32, index })?;
                        }
                        0x0A => {
                            let index = self.operand_u16(code, &mut next)?;
                            let pointer = self.arg_pointer(index as usize)?;
                            self.push(pointer)?;
                        }
                        0x0B => {
                            let index = self.operand_u16(code, &mut next)?;
                            self.pop_to(Pointer::Arg { frame: top as u32, index })?;
                        }
                        0x0C => {
                            let index = self.operand_u16(code, &mut next)?;
                            self.push_place(Pointer::Local { frame: top as u32, index })?;
                        }
                        0x0D => {
                            let index = self.operand_u16(code, &mut next)?;
                            let pointer = self.local_pointer(index as usize)?;
                            self.push(pointer)?;
                        }
                        0x0E => {
                            let index = self.operand_u16(code, &mut next)?;
                            self.pop_to(Pointer::Local { frame: top as u32, index })?;
                        }
                        // initobj
                        0x15 => {
                            let token = self.operand_u32(code, &mut next)?;
                            let ty = self.resolve_type(self.frames[top].method, token)?;
                            let pointer = self.pop_pointer()?;
                            self.clear(pointer, ty)?;
                        }
                        // constrained.
                        0x16 => {
                            let token = self.operand_u32(code, &mut next)?;
                            let ty = self.resolve_type(self.frames[top].method, token)?;
                            self.frames[top].constrained = Some(ty);
                        }
                        // Префиксы `unaligned.` и `no.` с байтом операнда,
                        // `volatile.`, `tail.` и `readonly.` без: интерпретатору
                        // они ничего не меняют.
                        0x12 | 0x19 => {
                            self.operand::<1>(code, &mut next)?;
                        }
                        0x13 | 0x14 | 0x1E => {}
                        // ldftn
                        0x06 => {
                            let token = self.operand_u32(code, &mut next)?;
                            let Resolved::Method(method) = self.resolve(self.frames[top].method, token)? else {
                                return Err(self.invalid("ldftn token is not a method"));
                            };
                            self.push(Value::Fn(method.0))?;
                        }
                        // ldvirtftn: метод настоящего типа объекта.
                        0x07 => {
                            let token = self.operand_u32(code, &mut next)?;
                            let Resolved::Method(method) = self.resolve(self.frames[top].method, token)? else {
                                return Err(self.invalid("ldvirtftn token is not a method"));
                            };
                            let object = match self.pop()? {
                                Value::Obj(Some(object)) => object,
                                Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
                                _ => return Err(self.invalid("ldvirtftn on a value that is not a reference")),
                            };
                            let ty = self.type_of_object(object)?;
                            let target = self.dispatch(ty, method)?;
                            self.push(Value::Fn(target.0))?;
                        }
                        // endfilter
                        0x11 => {
                            self.end_filter()?;
                            continue;
                        }
                        // rethrow
                        0x1A => {
                            self.rethrow()?;
                            continue;
                        }
                        _ => return Err(self.unsupported_instruction(0xFE00 | u16::from(second))),
                    }
                }
                _ => return Err(self.unsupported_instruction(u16::from(op))),
            }
            self.frames[top].pc = next;
        }
    }

    /// Положить на стек копию значения из места.
    fn push_place(&mut self, pointer: Pointer) -> Result<(), VmError> {
        let value = self.load(pointer)?;
        let value = self.copy_out(value)?;
        self.push(value)
    }

    /// Снять значение со стека в место.
    fn pop_to(&mut self, pointer: Pointer) -> Result<(), VmError> {
        let value = self.pop()?;
        self.store(pointer, value)
    }

    fn pop_pointer(&mut self) -> Result<Pointer, VmError> {
        match self.pop()? {
            Value::Ptr(pointer) => Ok(pointer),
            Value::Obj(None) => Err(self.exception("System.NullReferenceException")),
            _ => Err(self.invalid("indirection through a value that is not a pointer")),
        }
    }

    // --------------------------------------------------------------------
    // Операнды и переходы
    // --------------------------------------------------------------------

    fn operand<const N: usize>(&self, code: &[u8], next: &mut usize) -> Result<[u8; N], VmError> {
        let Some(bytes) = next.checked_add(N).and_then(|end| code.get(*next..end)) else {
            return Err(self.invalid("instruction operand runs past the end of the method"));
        };
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        *next += N;
        Ok(out)
    }

    fn operand_u8(&self, code: &[u8], next: &mut usize) -> Result<u32, VmError> {
        let [byte] = self.operand::<1>(code, next)?;
        Ok(u32::from(byte))
    }

    fn operand_u16(&self, code: &[u8], next: &mut usize) -> Result<u32, VmError> {
        Ok(u32::from(u16::from_le_bytes(self.operand::<2>(code, next)?)))
    }

    fn operand_u32(&self, code: &[u8], next: &mut usize) -> Result<u32, VmError> {
        Ok(u32::from_le_bytes(self.operand::<4>(code, next)?))
    }

    fn target(&self, code: &[u8], next: usize, offset: i64) -> Result<usize, VmError> {
        let target = next as i64 + offset;
        if target < 0 || target as usize >= code.len() {
            return Err(self.invalid("branch target outside the method"));
        }
        Ok(target as usize)
    }

    /// Выполнить условие ветвления: `kind` — номер в ряду `br` … `blt.un`.
    fn branch(&mut self, kind: u8) -> Result<bool, VmError> {
        Ok(match kind {
            0 => true,
            1 | 2 => {
                let value = self.pop()?;
                let truth = ops::truthy(value).map_err(|fault| self.fault(fault))?;
                truth == (kind == 2)
            }
            _ => {
                let right = self.pop()?;
                let left = self.pop()?;
                let c = ops::compare(left, right).map_err(|fault| self.fault(fault))?;
                match kind {
                    3 => c.equal,
                    4 => c.greater_or_equal(),
                    5 => c.greater(),
                    6 => c.less_or_equal(),
                    7 => c.less(),
                    8 => !c.equal,
                    9 => c.greater_or_equal_unordered(),
                    10 => c.greater_unordered(),
                    11 => c.less_or_equal_unordered(),
                    _ => c.less_unordered(),
                }
            }
        })
    }
}
