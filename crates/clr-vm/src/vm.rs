//! Цикл исполнения: кадры, инструкции IL, вызовы.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use clr_meta::sig::{self, elem};
use clr_meta::tables::id;
use clr_meta::{Assembly, Coded, Token};

use crate::heap::{Heap, Object};
use crate::natives::{self, Native};
use crate::ops::{self, Fault};
use crate::value::{ObjRef, Pointer, Value};
use crate::{Host, VmError};

/// Сколько вызовов может быть вложено друг в друга.
///
/// Предел ради памяти, а не ради стека Rust: кадры лежат в векторе. Две тысячи
/// с лишним — глубина рекурсии, до которой настоящие программы с окнами не
/// доходят, а бесконечная рекурсия упирается в него за доли секунды и
/// называется переполнением стека, как и в .NET.
pub const MAX_FRAMES: usize = 2048;

/// Флаг `MethodDef.Flags`: метод виртуальный.
const METHOD_VIRTUAL: u32 = 0x0040;

/// Во что обнуляется локальная переменная при входе в метод.
#[derive(Clone, Copy)]
enum Slot {
    I32,
    I64,
    Native,
    F,
    Obj,
}

impl Slot {
    const fn zero(self) -> Value {
        match self {
            Self::I32 => Value::I32(0),
            Self::I64 => Value::I64(0),
            Self::Native => Value::Native(0),
            Self::F => Value::F(0.0),
            Self::Obj => Value::Obj(None),
        }
    }
}

/// Метод, подготовленный к исполнению: разобран один раз и лежит в кэше.
struct Method<'a> {
    row: u32,
    code: &'a [u8],
    max_stack: u16,
    has_this: bool,
    params: u32,
    returns_value: bool,
    is_virtual: bool,
    locals: Vec<Slot>,
}

/// Кого зовёт инструкция `call`.
#[derive(Clone, Copy)]
enum Callee {
    /// Метод этой сборки.
    User(u32),
    /// Член базовой библиотеки, написанный в Rust.
    Native(Native),
}

struct Frame<'a> {
    method: Rc<Method<'a>>,
    pc: usize,
    args: Vec<Value>,
    locals: Vec<Value>,
    stack: Vec<Value>,
}

/// Среда выполнения одной сборки.
pub struct Vm<'a, H: Host> {
    asm: Assembly<'a>,
    host: H,
    pub(crate) heap: Heap,
    frames: Vec<Frame<'a>>,
    methods: BTreeMap<u32, Rc<Method<'a>>>,
    callees: BTreeMap<u32, Callee>,
    /// Объекты строк `ldstr` по смещению в `#US`: литерал выделяется один раз,
    /// как интернированная строка в .NET, — иначе цикл со строкой внутри рос бы
    /// в куче на каждом обороте.
    strings: BTreeMap<u32, ObjRef>,
    encoding: Option<ObjRef>,
    /// Сколько инструкций выполнено.
    pub instructions: u64,
}

impl<'a, H: Host> Vm<'a, H> {
    /// Подготовить сборку к запуску.
    pub fn new(data: &'a [u8], host: H) -> Result<Self, VmError> {
        Ok(Self {
            asm: Assembly::parse(data)?,
            host,
            heap: Heap::new(),
            frames: Vec::new(),
            methods: BTreeMap::new(),
            callees: BTreeMap::new(),
            strings: BTreeMap::new(),
            encoding: None,
            instructions: 0,
        })
    }

    /// Отдать то, во что среда печатала.
    pub fn into_host(self) -> H {
        self.host
    }

    #[must_use]
    pub fn type_count(&self) -> u32 {
        self.asm.tables.rows(id::TYPE_DEF)
    }

    #[must_use]
    pub fn method_count(&self) -> u32 {
        self.asm.tables.rows(id::METHOD_DEF)
    }

    #[must_use]
    pub fn object_count(&self) -> usize {
        self.heap.len()
    }

    /// Выполнить точку входа. Возвращает то, что вернул `Main` (ноль у `void`).
    pub fn run_main(&mut self, args: &[&str]) -> Result<i32, VmError> {
        let entry = Token::from_value(self.asm.cli.entry_point);
        if entry.table != id::METHOD_DEF || entry.is_nil() {
            return Err(VmError::NoEntryPoint);
        }
        let method = self.method(entry.row)?;
        let mut call_args = Vec::new();
        match method.params {
            0 => {}
            1 => {
                let mut elements = Vec::new();
                elements.try_reserve_exact(args.len()).map_err(|_| VmError::OutOfMemory)?;
                for arg in args {
                    let text = self.heap.string(arg.encode_utf16())?;
                    elements.push(Value::Obj(Some(text)));
                }
                let array = self.heap.alloc(Object::Array(elements))?;
                call_args.push(Value::Obj(Some(array)));
            }
            _ => {
                return Err(VmError::Unsupported {
                    what: String::from("an entry point with more than one parameter"),
                });
            }
        }
        self.enter(method, call_args)?;
        Ok(match self.execute()? {
            Some(Value::I32(code)) => code,
            _ => 0,
        })
    }

    /// Исполнять, пока не вернётся кадр, лежащий сверху в момент вызова.
    fn execute(&mut self) -> Result<Option<Value>, VmError> {
        let floor = self.frames.len() - 1;
        loop {
            self.instructions += 1;
            let top = self.frames.len() - 1;
            let code = self.frames[top].method.code;
            let pc = self.frames[top].pc;
            let Some(&op) = code.get(pc) else {
                return Err(self.invalid("execution ran past the end of the method"));
            };
            let mut next = pc + 1;
            match op {
                // nop, break
                0x00 | 0x01 => {}
                // ldarg.0 … ldarg.3
                0x02..=0x05 => {
                    let value = self.arg(usize::from(op - 0x02))?;
                    self.push(value)?;
                }
                // ldloc.0 … ldloc.3
                0x06..=0x09 => {
                    let value = self.local(usize::from(op - 0x06))?;
                    self.push(value)?;
                }
                // stloc.0 … stloc.3
                0x0A..=0x0D => {
                    let value = self.pop()?;
                    self.set_local(usize::from(op - 0x0A), value)?;
                }
                0x0E => {
                    let index = self.operand_u8(code, &mut next)?;
                    let value = self.arg(index)?;
                    self.push(value)?;
                }
                0x0F => {
                    let index = self.operand_u8(code, &mut next)?;
                    let pointer = self.arg_pointer(index)?;
                    self.push(pointer)?;
                }
                0x10 => {
                    let index = self.operand_u8(code, &mut next)?;
                    let value = self.pop()?;
                    self.set_arg(index, value)?;
                }
                0x11 => {
                    let index = self.operand_u8(code, &mut next)?;
                    let value = self.local(index)?;
                    self.push(value)?;
                }
                0x12 => {
                    let index = self.operand_u8(code, &mut next)?;
                    let pointer = self.local_pointer(index)?;
                    self.push(pointer)?;
                }
                0x13 => {
                    let index = self.operand_u8(code, &mut next)?;
                    let value = self.pop()?;
                    self.set_local(index, value)?;
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
                    self.push(Value::F(f64::from(f32::from_le_bytes(bytes))))?;
                }
                0x23 => {
                    let bytes = self.operand::<8>(code, &mut next)?;
                    self.push(Value::F(f64::from_le_bytes(bytes)))?;
                }
                // dup
                0x25 => {
                    let value = self.peek()?;
                    self.push(value)?;
                }
                // pop
                0x26 => {
                    self.pop()?;
                }
                // call, callvirt
                0x28 | 0x6F => {
                    let token = u32::from_le_bytes(self.operand::<4>(code, &mut next)?);
                    self.frames[top].pc = next;
                    self.call(token, op == 0x6F)?;
                    continue;
                }
                // ret
                0x2A => {
                    let value = if self.frames[top].method.returns_value {
                        Some(self.pop()?)
                    } else {
                        None
                    };
                    self.frames.pop();
                    if self.frames.len() == floor {
                        return Ok(value);
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
                    let offset = i32::from_le_bytes(self.operand::<4>(code, &mut next)?);
                    if self.branch(op - 0x38)? {
                        next = self.target(code, next, i64::from(offset))?;
                    }
                }
                // switch
                0x45 => {
                    let count = u32::from_le_bytes(self.operand::<4>(code, &mut next)?) as usize;
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
                        let offset = i32::from_le_bytes(self.operand::<4>(code, &mut at)?);
                        next = self.target(code, after, i64::from(offset))?;
                    }
                }
                // ldind.i1 … ldind.ref
                0x46..=0x50 => {
                    let pointer = self.pop()?;
                    let value = self.load_indirect(pointer)?;
                    self.push(narrow_load(op, value))?;
                }
                // stind.ref … stind.r8, stind.i
                0x51..=0x57 | 0xDF => {
                    let value = self.pop()?;
                    let pointer = self.pop()?;
                    self.store_indirect(pointer, narrow_store(op, value))?;
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
                // ldstr
                0x72 => {
                    let token = u32::from_le_bytes(self.operand::<4>(code, &mut next)?);
                    let value = self.load_string(token)?;
                    self.push(value)?;
                }
                // newarr
                0x8D => {
                    let token = u32::from_le_bytes(self.operand::<4>(code, &mut next)?);
                    let length = self.pop()?;
                    let array = self.new_array(token, length)?;
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
                    self.operand::<4>(code, &mut next)?;
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    self.push(Value::Ptr(Pointer::Element { array, index }))?;
                }
                // ldelem.i1 … ldelem.ref, ldelem <T>
                0x90..=0x9A | 0xA3 => {
                    if op == 0xA3 {
                        self.operand::<4>(code, &mut next)?;
                    }
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    let value = self.load_indirect(Value::Ptr(Pointer::Element { array, index }))?;
                    // Элементы лежат так же, как значения по указателю:
                    // `ldelem.X` — это `ldind.X`, сдвинутый на 0x4A.
                    let value = if op == 0xA3 { value } else { narrow_load(op - 0x4A, value) };
                    self.push(value)?;
                }
                // stelem.i … stelem.ref, stelem <T>
                0x9B..=0xA2 | 0xA4 => {
                    if op == 0xA4 {
                        self.operand::<4>(code, &mut next)?;
                    }
                    let value = self.pop()?;
                    let index = self.pop()?;
                    let array = self.pop()?;
                    let (array, index) = self.element(array, index)?;
                    let stind = match op {
                        0x9C..=0xA1 => op - 0x4A,
                        _ => 0x51,
                    };
                    self.store_indirect(Value::Ptr(Pointer::Element { array, index }), narrow_store(stind, value))?;
                }
                // leave, leave.s: выход из защищённого блока. Обработчиков у
                // методов этой фазы нет (см. `method`), и `leave` — это переход,
                // очищающий стек вычислений.
                0xDD | 0xDE => {
                    let offset = if op == 0xDD {
                        i64::from(i32::from_le_bytes(self.operand::<4>(code, &mut next)?))
                    } else {
                        let [byte] = self.operand::<1>(code, &mut next)?;
                        i64::from(byte as i8)
                    };
                    self.frames[top].stack.clear();
                    next = self.target(code, next, offset)?;
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
                            let value = self.arg(index)?;
                            self.push(value)?;
                        }
                        0x0A => {
                            let index = self.operand_u16(code, &mut next)?;
                            let pointer = self.arg_pointer(index)?;
                            self.push(pointer)?;
                        }
                        0x0B => {
                            let index = self.operand_u16(code, &mut next)?;
                            let value = self.pop()?;
                            self.set_arg(index, value)?;
                        }
                        0x0C => {
                            let index = self.operand_u16(code, &mut next)?;
                            let value = self.local(index)?;
                            self.push(value)?;
                        }
                        0x0D => {
                            let index = self.operand_u16(code, &mut next)?;
                            let pointer = self.local_pointer(index)?;
                            self.push(pointer)?;
                        }
                        0x0E => {
                            let index = self.operand_u16(code, &mut next)?;
                            let value = self.pop()?;
                            self.set_local(index, value)?;
                        }
                        // Префиксы `unaligned.` и `no.` с байтом операнда,
                        // `volatile.`, `tail.` и `readonly.` без: интерпретатору
                        // они ничего не меняют.
                        0x12 | 0x19 => {
                            self.operand::<1>(code, &mut next)?;
                        }
                        0x13 | 0x14 | 0x1E => {}
                        _ => return Err(self.unsupported_instruction(0xFE00 | u16::from(second))),
                    }
                }
                _ => return Err(self.unsupported_instruction(u16::from(op))),
            }
            self.frames[top].pc = next;
        }
    }

    // --------------------------------------------------------------------
    // Стек, аргументы, локальные переменные
    // --------------------------------------------------------------------

    fn push(&mut self, value: Value) -> Result<(), VmError> {
        let Some(frame) = self.frames.last_mut() else {
            return Err(VmError::Invalid { what: "push without a frame", at: String::new() });
        };
        frame.stack.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        frame.stack.push(value);
        Ok(())
    }

    fn pop(&mut self) -> Result<Value, VmError> {
        match self.frames.last_mut().and_then(|frame| frame.stack.pop()) {
            Some(value) => Ok(value),
            None => Err(self.invalid("evaluation stack underflow")),
        }
    }

    fn peek(&self) -> Result<Value, VmError> {
        self.frames
            .last()
            .and_then(|frame| frame.stack.last().copied())
            .ok_or_else(|| self.invalid("evaluation stack underflow"))
    }

    fn arg(&self, index: usize) -> Result<Value, VmError> {
        self.frames
            .last()
            .and_then(|frame| frame.args.get(index).copied())
            .ok_or_else(|| self.invalid("argument number out of range"))
    }

    fn set_arg(&mut self, index: usize, value: Value) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].args.len() {
            return Err(self.invalid("argument number out of range"));
        }
        self.frames[top].args[index] = value;
        Ok(())
    }

    fn local(&self, index: usize) -> Result<Value, VmError> {
        self.frames
            .last()
            .and_then(|frame| frame.locals.get(index).copied())
            .ok_or_else(|| self.invalid("local variable number out of range"))
    }

    fn set_local(&mut self, index: usize, value: Value) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].locals.len() {
            return Err(self.invalid("local variable number out of range"));
        }
        self.frames[top].locals[index] = value;
        Ok(())
    }

    fn arg_pointer(&self, index: usize) -> Result<Value, VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].args.len() {
            return Err(self.invalid("argument number out of range"));
        }
        Ok(Value::Ptr(Pointer::Arg { frame: top as u32, index: index as u32 }))
    }

    fn local_pointer(&self, index: usize) -> Result<Value, VmError> {
        let top = self.frames.len() - 1;
        if index >= self.frames[top].locals.len() {
            return Err(self.invalid("local variable number out of range"));
        }
        Ok(Value::Ptr(Pointer::Local { frame: top as u32, index: index as u32 }))
    }

    fn load_indirect(&self, pointer: Value) -> Result<Value, VmError> {
        match pointer {
            Value::Ptr(Pointer::Local { frame, index }) => self
                .frames
                .get(frame as usize)
                .and_then(|frame| frame.locals.get(index as usize).copied())
                .ok_or_else(|| self.invalid("pointer to a local variable that no longer exists")),
            Value::Ptr(Pointer::Arg { frame, index }) => self
                .frames
                .get(frame as usize)
                .and_then(|frame| frame.args.get(index as usize).copied())
                .ok_or_else(|| self.invalid("pointer to an argument that no longer exists")),
            Value::Ptr(Pointer::Element { array, index }) => match self.heap.get(array) {
                Some(Object::Array(elements)) => elements
                    .get(index as usize)
                    .copied()
                    .ok_or_else(|| self.exception("System.IndexOutOfRangeException")),
                _ => Err(self.invalid("element pointer into something that is not an array")),
            },
            Value::Obj(None) => Err(self.exception("System.NullReferenceException")),
            _ => Err(self.invalid("indirection through a value that is not a pointer")),
        }
    }

    fn store_indirect(&mut self, pointer: Value, value: Value) -> Result<(), VmError> {
        match pointer {
            Value::Ptr(Pointer::Local { frame, index }) => {
                let ok = self.frames.get(frame as usize).is_some_and(|f| (index as usize) < f.locals.len());
                if !ok {
                    return Err(self.invalid("pointer to a local variable that no longer exists"));
                }
                self.frames[frame as usize].locals[index as usize] = value;
            }
            Value::Ptr(Pointer::Arg { frame, index }) => {
                let ok = self.frames.get(frame as usize).is_some_and(|f| (index as usize) < f.args.len());
                if !ok {
                    return Err(self.invalid("pointer to an argument that no longer exists"));
                }
                self.frames[frame as usize].args[index as usize] = value;
            }
            Value::Ptr(Pointer::Element { array, index }) => {
                let exists = matches!(self.heap.get(array), Some(Object::Array(e)) if (index as usize) < e.len());
                if !exists {
                    return Err(self.exception("System.IndexOutOfRangeException"));
                }
                if let Some(Object::Array(elements)) = self.heap.get_mut(array) {
                    elements[index as usize] = value;
                }
            }
            Value::Obj(None) => return Err(self.exception("System.NullReferenceException")),
            _ => return Err(self.invalid("indirection through a value that is not a pointer")),
        }
        Ok(())
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

    fn operand_u8(&self, code: &[u8], next: &mut usize) -> Result<usize, VmError> {
        let [byte] = self.operand::<1>(code, next)?;
        Ok(usize::from(byte))
    }

    fn operand_u16(&self, code: &[u8], next: &mut usize) -> Result<usize, VmError> {
        Ok(usize::from(u16::from_le_bytes(self.operand::<2>(code, next)?)))
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

    // --------------------------------------------------------------------
    // Вызовы
    // --------------------------------------------------------------------

    fn call(&mut self, token: u32, virtual_call: bool) -> Result<(), VmError> {
        match self.resolve(token)? {
            Callee::User(row) => {
                let method = self.method(row)?;
                if virtual_call && method.is_virtual {
                    return Err(VmError::Unsupported {
                        what: format!("virtual call to {} (phase N3)", self.method_name(row)),
                    });
                }
                let count = method.params as usize + usize::from(method.has_this);
                let args = self.pop_args(count)?;
                if virtual_call && method.has_this && args.first() == Some(&Value::Obj(None)) {
                    return Err(self.exception("System.NullReferenceException"));
                }
                self.enter(method, args)
            }
            Callee::Native(native) => {
                let args = self.pop_args(native.arguments())?;
                if let Some(result) = natives::call(self, native, &args)? {
                    self.push(result)?;
                }
                Ok(())
            }
        }
    }

    fn pop_args(&mut self, count: usize) -> Result<Vec<Value>, VmError> {
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

    fn enter(&mut self, method: Rc<Method<'a>>, args: Vec<Value>) -> Result<(), VmError> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(VmError::StackOverflow);
        }
        let mut locals = Vec::new();
        locals.try_reserve_exact(method.locals.len()).map_err(|_| VmError::OutOfMemory)?;
        locals.extend(method.locals.iter().map(|slot| slot.zero()));
        let mut stack = Vec::new();
        stack.try_reserve(usize::from(method.max_stack)).map_err(|_| VmError::OutOfMemory)?;
        self.frames.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        self.frames.push(Frame { method, pc: 0, args, locals, stack });
        Ok(())
    }

    fn resolve(&mut self, token: u32) -> Result<Callee, VmError> {
        if let Some(callee) = self.callees.get(&token) {
            return Ok(*callee);
        }
        let t = Token::from_value(token);
        let callee = match t.table {
            id::METHOD_DEF => Callee::User(t.row),
            id::MEMBER_REF => self.resolve_member_ref(t.row)?,
            id::METHOD_SPEC => {
                return Err(VmError::Unsupported {
                    what: format!("call to a generic method instantiation in {} (phase N3)", self.location()),
                });
            }
            _ => return Err(self.invalid("call token is not a method")),
        };
        self.callees.insert(token, callee);
        Ok(callee)
    }

    fn resolve_member_ref(&self, row: u32) -> Result<Callee, VmError> {
        let tables = self.asm.tables;
        let strings = self.asm.root.strings;
        let parent = tables.coded_column(id::MEMBER_REF, row, 0, Coded::MemberRefParent)?;
        let name = strings.get(tables.column(id::MEMBER_REF, row, 1)?)?;
        let signature = self.asm.root.blobs.get(tables.column(id::MEMBER_REF, row, 2)?)?;
        match parent.table {
            id::TYPE_DEF => return self.find_method(parent.row, name, signature).map(Callee::User),
            id::TYPE_REF => {}
            _ => {
                return Err(VmError::Unsupported {
                    what: format!("call to {name} on a generic type instantiation (phase N3)"),
                });
            }
        }

        let scope = tables.coded_column(id::TYPE_REF, parent.row, 0, Coded::ResolutionScope)?;
        if scope.table == id::MODULE {
            let type_name = strings.get(tables.column(id::TYPE_REF, parent.row, 1)?)?;
            let namespace = strings.get(tables.column(id::TYPE_REF, parent.row, 2)?)?;
            if let Some(type_row) = self.find_type_def(namespace, type_name)? {
                return self.find_method(type_row, name, signature).map(Callee::User);
            }
        }

        let mut key = String::new();
        self.asm.write_type_name(parent, &mut key, 0)?;
        let _ = write!(key, "::{name}(");
        sig::write_params(signature, &self.asm, &mut key)?;
        key.push(')');
        if let Some(native) = natives::lookup(&key) {
            return Ok(Callee::Native(native));
        }
        let assembly = if scope.table == id::ASSEMBLY_REF {
            strings.get(tables.column(id::ASSEMBLY_REF, scope.row, 6)?)?
        } else {
            "this module"
        };
        Err(VmError::MissingMember { name: format!("{key} from {assembly}") })
    }

    fn find_type_def(&self, namespace: &str, name: &str) -> Result<Option<u32>, VmError> {
        let tables = self.asm.tables;
        let strings = self.asm.root.strings;
        for row in 1..=tables.rows(id::TYPE_DEF) {
            if strings.get(tables.column(id::TYPE_DEF, row, 1)?)? == name
                && strings.get(tables.column(id::TYPE_DEF, row, 2)?)? == namespace
            {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn find_method(&self, type_row: u32, name: &str, signature: &[u8]) -> Result<u32, VmError> {
        let tables = self.asm.tables;
        for method in tables.list(id::TYPE_DEF, type_row, 5, id::METHOD_DEF, id::METHOD_PTR)? {
            let method = method?;
            // Перегрузки различаются сигнатурой, и сравнивается она побайтно:
            // обе стороны записаны одним компилятором одной сборки.
            if self.asm.root.strings.get(tables.column(id::METHOD_DEF, method, 3)?)? == name
                && self.asm.root.blobs.get(tables.column(id::METHOD_DEF, method, 4)?)? == signature
            {
                return Ok(method);
            }
        }
        let mut owner = String::new();
        let _ = self.asm.write_type_name(Token { table: id::TYPE_DEF, row: type_row }, &mut owner, 0);
        Err(VmError::MissingMember { name: format!("{owner}::{name}") })
    }

    /// Разобрать метод один раз и положить в кэш.
    fn method(&mut self, row: u32) -> Result<Rc<Method<'a>>, VmError> {
        if let Some(method) = self.methods.get(&row) {
            return Ok(Rc::clone(method));
        }
        let tables = self.asm.tables;
        let rva = tables.column(id::METHOD_DEF, row, 0)?;
        let flags = tables.column(id::METHOD_DEF, row, 2)?;
        let blob = self.asm.root.blobs.get(tables.column(id::METHOD_DEF, row, 4)?)?;
        let signature = sig::method_sig(blob)?;
        if signature.header.convention & 0x0F == sig::CALL_VARARG {
            return Err(VmError::Unsupported {
                what: format!("variable argument lists in {}", self.method_name(row)),
            });
        }
        if signature.header.generic_params > 0 {
            return Err(VmError::Unsupported {
                what: format!("generic method {} (phase N3)", self.method_name(row)),
            });
        }
        let Some(body) = self.asm.method_body(rva)? else {
            return Err(VmError::Unsupported {
                what: format!(
                    "{} has no IL body: it is extern, abstract or provided by the runtime",
                    self.method_name(row)
                ),
            });
        };
        if body.clauses().next().is_some() {
            return Err(VmError::Unsupported {
                what: format!("exception handling in {} (phase N3)", self.method_name(row)),
            });
        }
        let locals = self.local_slots(body.local_signature, row)?;
        let method = Rc::new(Method {
            row,
            code: body.code,
            max_stack: body.max_stack,
            has_this: signature.header.convention & sig::HAS_THIS != 0,
            params: signature.header.params,
            returns_value: signature.returns_value,
            is_virtual: flags & METHOD_VIRTUAL != 0,
            locals,
        });
        self.methods.insert(row, Rc::clone(&method));
        Ok(method)
    }

    fn local_slots(&self, token: u32, row: u32) -> Result<Vec<Slot>, VmError> {
        let mut slots = Vec::new();
        if token == 0 {
            return Ok(slots);
        }
        let t = Token::from_value(token);
        if t.table != id::STAND_ALONE_SIG {
            return Err(VmError::Invalid {
                what: "local variable signature token points elsewhere",
                at: self.method_name(row),
            });
        }
        let blob = self.asm.root.blobs.get(self.asm.tables.column(id::STAND_ALONE_SIG, t.row, 0)?)?;
        let (count, mut at) = sig::locals(blob)?;
        slots.try_reserve_exact(count as usize).map_err(|_| VmError::OutOfMemory)?;
        for _ in 0..count {
            let slot = match sig::element(blob, at)? {
                elem::BOOLEAN | elem::CHAR | elem::I1 | elem::U1 | elem::I2 | elem::U2 | elem::I4 | elem::U4 => {
                    Slot::I32
                }
                elem::I8 | elem::U8 => Slot::I64,
                elem::I | elem::U | elem::PTR | elem::FNPTR => Slot::Native,
                elem::R4 | elem::R8 => Slot::F,
                elem::STRING | elem::CLASS | elem::OBJECT | elem::SZARRAY | elem::ARRAY | elem::BYREF => Slot::Obj,
                elem::GENERICINST if blob.get(sig::skip_modifiers(blob, at)? + 1) == Some(&elem::CLASS) => Slot::Obj,
                _ => {
                    let mut name = String::new();
                    let _ = sig::write_type(blob, at, &self.asm, &mut name);
                    return Err(VmError::Unsupported {
                        what: format!(
                            "local variable of type {name} in {} (value types: phase N3)",
                            self.method_name(row)
                        ),
                    });
                }
            };
            slots.push(slot);
            at = sig::skip_type(blob, at)?;
        }
        Ok(slots)
    }

    // --------------------------------------------------------------------
    // Строки и массивы
    // --------------------------------------------------------------------

    fn load_string(&mut self, token: u32) -> Result<Value, VmError> {
        let t = Token::from_value(token);
        if t.table != id::USER_STRING {
            return Err(self.invalid("ldstr token is not a user string"));
        }
        if let Some(existing) = self.strings.get(&t.row) {
            return Ok(Value::Obj(Some(*existing)));
        }
        let text = self.asm.root.user_strings.get(t.row)?;
        let reference = self.heap.string(text.units())?;
        self.strings.insert(t.row, reference);
        Ok(Value::Obj(Some(reference)))
    }

    fn new_array(&mut self, token: u32, length: Value) -> Result<Value, VmError> {
        let count = match length {
            Value::I32(n) => i64::from(n),
            Value::Native(n) => n,
            _ => return Err(self.invalid("array length is not an integer")),
        };
        if count < 0 {
            return Err(self.exception("System.OverflowException"));
        }
        let zero = self.element_zero(token)?;
        let mut elements = Vec::new();
        elements.try_reserve_exact(count as usize).map_err(|_| VmError::OutOfMemory)?;
        elements.resize(count as usize, zero);
        let array = self.heap.alloc(Object::Array(elements))?;
        Ok(Value::Obj(Some(array)))
    }

    /// Чем заполнен новый массив элементов этого типа.
    fn element_zero(&self, token: u32) -> Result<Value, VmError> {
        let mut name = String::new();
        self.asm.write_type_name(Token::from_value(token), &mut name, 0)?;
        Ok(match name.as_str() {
            "System.Boolean" | "System.Char" | "System.SByte" | "System.Byte" | "System.Int16"
            | "System.UInt16" | "System.Int32" | "System.UInt32" => Value::I32(0),
            "System.Int64" | "System.UInt64" => Value::I64(0),
            "System.IntPtr" | "System.UIntPtr" => Value::Native(0),
            "System.Single" | "System.Double" => Value::F(0.0),
            _ => {
                // Массив значимых типов пользователя — это структуры, а
                // структур у среды ещё нет. Сказать об этом честнее, чем
                // молча завести массив ссылок.
                let t = Token::from_value(token);
                if t.table == id::TYPE_DEF && self.is_value_type(t.row)? {
                    return Err(VmError::Unsupported {
                        what: format!("array of value type {name} (phase N3)"),
                    });
                }
                Value::Obj(None)
            }
        })
    }

    fn is_value_type(&self, type_row: u32) -> Result<bool, VmError> {
        let base = self.asm.tables.coded_column(id::TYPE_DEF, type_row, 3, Coded::TypeDefOrRef)?;
        if base.is_nil() {
            return Ok(false);
        }
        let mut name = String::new();
        self.asm.write_type_name(base, &mut name, 0)?;
        Ok(name == "System.ValueType" || name == "System.Enum")
    }

    fn element(&self, array: Value, index: Value) -> Result<(ObjRef, u32), VmError> {
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

    fn array_len(&self, array: Value) -> Result<usize, VmError> {
        match array {
            Value::Obj(Some(reference)) => match self.heap.get(reference) {
                Some(Object::Array(elements)) => Ok(elements.len()),
                _ => Err(self.invalid("ldlen on something that is not an array")),
            },
            Value::Obj(None) => Err(self.exception("System.NullReferenceException")),
            _ => Err(self.invalid("ldlen on a value that is not a reference")),
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

    pub(crate) fn print_text(&mut self, text: &str, newline: bool) {
        if newline {
            let mut line = String::from(text);
            line.push('\n');
            self.host.write_out(&line);
        } else {
            self.host.write_out(text);
        }
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

    /// Значение по указателю — или само значение, если это не указатель.
    pub(crate) fn deref(&self, value: Value) -> Result<Value, VmError> {
        match value {
            Value::Ptr(_) => self.load_indirect(value),
            other => Ok(other),
        }
    }

    pub(crate) fn new_string_from(&mut self, text: &str) -> Result<Value, VmError> {
        Ok(Value::Obj(Some(self.heap.string(text.encode_utf16())?)))
    }

    pub(crate) fn utf8_encoding(&mut self) -> Result<Value, VmError> {
        if let Some(existing) = self.encoding {
            return Ok(Value::Obj(Some(existing)));
        }
        let reference = self.heap.alloc(Object::Encoding)?;
        self.encoding = Some(reference);
        Ok(Value::Obj(Some(reference)))
    }

    // --------------------------------------------------------------------
    // Ошибки и имена
    // --------------------------------------------------------------------

    /// Где сейчас исполнение: `Program::<Main>$ IL_0012`.
    pub(crate) fn location(&self) -> String {
        match self.frames.last() {
            Some(frame) => format!("{} IL_{:04x}", self.method_name(frame.method.row), frame.pc),
            None => String::from("the runtime"),
        }
    }

    fn method_name(&self, row: u32) -> String {
        let mut out = String::new();
        let tables = self.asm.tables;
        'types: for type_row in 1..=tables.rows(id::TYPE_DEF) {
            let Ok(list) = tables.list(id::TYPE_DEF, type_row, 5, id::METHOD_DEF, id::METHOD_PTR) else {
                break;
            };
            for method in list {
                if method == Ok(row) {
                    let _ = self.asm.write_type_name(Token { table: id::TYPE_DEF, row: type_row }, &mut out, 0);
                    out.push_str("::");
                    break 'types;
                }
            }
        }
        match tables.column(id::METHOD_DEF, row, 3).and_then(|index| self.asm.root.strings.get(index)) {
            Ok(name) => out.push_str(name),
            Err(_) => {
                let _ = write!(out, "method #{row}");
            }
        }
        out
    }

    pub(crate) fn invalid(&self, what: &'static str) -> VmError {
        VmError::Invalid { what, at: self.location() }
    }

    pub(crate) fn exception(&self, name: &'static str) -> VmError {
        VmError::Exception { name, at: self.location() }
    }

    fn fault(&self, fault: Fault) -> VmError {
        match fault {
            Fault::DivideByZero => self.exception("System.DivideByZeroException"),
            Fault::Overflow => self.exception("System.OverflowException"),
            Fault::Invalid(what) => self.invalid(what),
        }
    }

    fn unsupported_instruction(&self, op: u16) -> VmError {
        let topic = match op {
            0x70 | 0x71 | 0x73..=0x75 | 0x79 | 0x7B..=0x81 | 0x8C | 0xA5 | 0xFE15 | 0xFE1C => {
                "objects, fields and value types (phase N3)"
            }
            0x7A | 0xDC | 0xFE11 | 0xFE1A => "exceptions (phase N3)",
            0x29 | 0xFE06 | 0xFE07 => "delegates and function pointers (phase N3)",
            0xFE16 => "constrained calls on generic parameters (phase N3)",
            _ => "not implemented",
        };
        VmError::Unsupported { what: format!("IL instruction 0x{op:02x} in {}: {topic}", self.location()) }
    }
}

/// Сузить значение, прочитанное `ldind.X`.
fn narrow_load(op: u8, value: Value) -> Value {
    match (op, value) {
        (0x46, Value::I32(x)) => Value::I32(i32::from(x as i8)),
        (0x47, Value::I32(x)) => Value::I32(i32::from(x as u8)),
        (0x48, Value::I32(x)) => Value::I32(i32::from(x as i16)),
        (0x49, Value::I32(x)) => Value::I32(i32::from(x as u16)),
        _ => value,
    }
}

/// Сузить значение, записываемое `stind.X`.
fn narrow_store(op: u8, value: Value) -> Value {
    match (op, value) {
        (0x52, Value::I32(x)) => Value::I32(i32::from(x as i8)),
        (0x53, Value::I32(x)) => Value::I32(i32::from(x as i16)),
        _ => value,
    }
}
