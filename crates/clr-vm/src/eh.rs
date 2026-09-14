//! Исключения: `throw`, поиск обработчика, `finally`, фильтры (фаза N3b).
//!
//! # Два прохода, как в CLR
//!
//! Первый проход ищет обработчик и ничего не разматывает: кадры над ним живы,
//! и фильтр `when` выполняется, пока внутренние `finally` ещё не отработали.
//! Это видно в выводе — `filter deep` печатается раньше `unwind 1` — и
//! однопроходная размотка напечатала бы строки в другом порядке.
//!
//! Второй проход идёт от вершины к найденному кадру и выполняет `finally` и
//! `fault` по дороге, после чего передаёт исключение в `catch`.
//!
//! # Без рекурсии Rust
//!
//! Фильтр и `finally` — управляемый код, и выполнять их вызовом Rust нельзя
//! (см. `lib.rs`). Поэтому поиск и размотка — это состояние: фильтр
//! запускается кадром, конец фильтра (`endfilter`) продолжает поиск со
//! следующего обработчика, конец `finally` (`endfinally`) — размотку.
//!
//! Фильтр работает с переменными своего метода, а тот лежит в стеке кадров
//! ниже, под вызванными им методами. Кадр фильтра забирает его аргументы и
//! переменные себе на время работы и возвращает на `endfilter`.
//!
//! # Чего нет
//!
//! Исключение, вылетевшее из статического конструктора, летит как есть, а не
//! обёрнутым в `TypeInitializationException`, и тип после него считается
//! инициализированным.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use clr_meta::ExceptionClause;
use clr_meta::body::{CLAUSE_CATCH, CLAUSE_FAULT, CLAUSE_FILTER, CLAUSE_FINALLY};

use crate::heap::Object;
use crate::types::{MethodId, Resolved, TypeId};
use crate::value::{ObjRef, Pointer, Value};
use crate::vm::{Frame, FrameKind, Vm};
use crate::{Host, VmError};

/// Что делать, когда кончится `finally`, выполняемый в этом кадре.
#[derive(Clone)]
pub(crate) enum Continuation {
    /// `leave`: выполнить оставшиеся `finally` и перейти на `target`.
    Leave { clause: u32, remaining: Vec<u32>, target: usize },
    /// Размотка: продолжить второй проход после обработчика `clause`.
    Unwind { clause: u32, exception: ObjRef, handler_frame: usize, handler_clause: u32 },
}

impl Continuation {
    const fn clause(&self) -> u32 {
        match self {
            Self::Leave { clause, .. } | Self::Unwind { clause, .. } => *clause,
        }
    }
}

const fn kind(clause: &ExceptionClause) -> u32 {
    clause.flags & 0x7
}

const fn try_covers(clause: &ExceptionClause, pc: usize) -> bool {
    let start = clause.try_offset as usize;
    pc >= start && pc < start + clause.try_length as usize
}

const fn handler_covers(clause: &ExceptionClause, pc: usize) -> bool {
    let start = clause.handler_offset as usize;
    pc >= start && pc < start + clause.handler_length as usize
}

impl<'a, H: Host> Vm<'a, H> {
    /// Бросить объект исключения из текущей инструкции верхнего кадра.
    pub(crate) fn raise(&mut self, exception: ObjRef) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        self.search(exception, top, 0)
    }

    /// Первый проход с кадра `frame`, начиная с обработчика `clause`.
    fn search(&mut self, exception: ObjRef, frame: usize, clause: u32) -> Result<(), VmError> {
        let exception_type = self.type_of_object(exception)?;
        let mut frame = frame;
        let mut start = clause;
        loop {
            // Исключение внутри фильтра не выходит за фильтр: CLR считает
            // такой фильтр ложным.
            if let FrameKind::Filter { .. } = self.frames[frame].kind {
                return self.abandon_filter(frame);
            }
            let pc = self.frames[frame].pc;
            let body = Rc::clone(&self.frames[frame].body);
            let method = self.frames[frame].method;
            for (index, c) in body.clauses.iter().enumerate().skip(start as usize) {
                if !try_covers(c, pc) {
                    continue;
                }
                match kind(c) {
                    CLAUSE_CATCH => {
                        let catches = self.catch_type(method, c.class_or_filter)?;
                        if self.assignable(exception_type, catches) {
                            let top = self.frames.len() - 1;
                            return self.unwind(exception, frame, index as u32, top, 0);
                        }
                    }
                    CLAUSE_FILTER => {
                        return self.run_filter(exception, frame, index as u32, c.class_or_filter as usize);
                    }
                    _ => {}
                }
            }
            if frame == 0 {
                return Err(self.unhandled(exception));
            }
            frame -= 1;
            start = 0;
        }
    }

    fn catch_type(&mut self, method: MethodId, token: u32) -> Result<TypeId, VmError> {
        match self.resolve(method, token)? {
            Resolved::Type(ty) => Ok(ty),
            _ => Err(self.invalid("catch clause does not name a type")),
        }
    }

    fn run_filter(&mut self, exception: ObjRef, owner: usize, clause: u32, offset: usize) -> Result<(), VmError> {
        let args = core::mem::take(&mut self.frames[owner].args);
        let locals = core::mem::take(&mut self.frames[owner].locals);
        let method = self.frames[owner].method;
        let body = Rc::clone(&self.frames[owner].body);
        let mut stack = Vec::new();
        stack.try_reserve(usize::from(body.max_stack).max(1)).map_err(|_| VmError::OutOfMemory)?;
        stack.push(Value::Obj(Some(exception)));
        self.frames.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
        let mut frame = Frame::new(method, body, args, locals, stack);
        frame.pc = offset;
        frame.kind = FrameKind::Filter { owner, clause, exception };
        self.frames.push(frame);
        Ok(())
    }

    /// `endfilter`.
    pub(crate) fn end_filter(&mut self) -> Result<(), VmError> {
        let verdict = self.pop()?;
        let top = self.frames.len() - 1;
        let FrameKind::Filter { owner, clause, exception } = self.frames[top].kind else {
            return Err(self.invalid("endfilter outside a filter"));
        };
        let accepted = matches!(verdict, Value::I32(x) if x != 0);
        self.close_filter(top, owner);
        if accepted {
            let top = self.frames.len() - 1;
            self.unwind(exception, owner, clause, top, 0)
        } else {
            self.search(exception, owner, clause + 1)
        }
    }

    /// Исключение внутри фильтра: снять всё до кадра фильтра включительно и
    /// продолжить поиск, как если бы фильтр вернул ложь.
    fn abandon_filter(&mut self, filter: usize) -> Result<(), VmError> {
        let FrameKind::Filter { owner, clause, exception } = self.frames[filter].kind else {
            return Err(self.invalid("filter frame expected"));
        };
        self.frames.truncate(filter + 1);
        self.close_filter(filter, owner);
        self.search(exception, owner, clause + 1)
    }

    fn close_filter(&mut self, filter: usize, owner: usize) {
        let mut frame = self.frames.remove(filter);
        self.frames[owner].args = core::mem::take(&mut frame.args);
        self.frames[owner].locals = core::mem::take(&mut frame.locals);
    }

    /// Второй проход: от кадра `frame` (обработчики с `start`) вниз до
    /// обработчика `handler_clause` в `handler_frame`.
    fn unwind(
        &mut self,
        exception: ObjRef,
        handler_frame: usize,
        handler_clause: u32,
        frame: usize,
        start: u32,
    ) -> Result<(), VmError> {
        let mut frame = frame;
        let mut start = start;
        loop {
            let pc = self.frames[frame].pc;
            let body = Rc::clone(&self.frames[frame].body);
            let limit = if frame == handler_frame { handler_clause as usize } else { body.clauses.len() };
            for (index, c) in body.clauses.iter().enumerate().take(limit).skip(start as usize) {
                if try_covers(c, pc) && matches!(kind(c), CLAUSE_FINALLY | CLAUSE_FAULT) {
                    self.frames.truncate(frame + 1);
                    self.enter_handler(frame, c.handler_offset as usize);
                    self.frames[frame].continuations.push(Continuation::Unwind {
                        clause: index as u32,
                        exception,
                        handler_frame,
                        handler_clause,
                    });
                    return Ok(());
                }
            }
            if frame == handler_frame {
                let target = body.clauses[handler_clause as usize].handler_offset as usize;
                self.frames.truncate(frame + 1);
                self.enter_handler(frame, target);
                self.frames[frame].stack.push(Value::Obj(Some(exception)));
                self.frames[frame].caught.push((handler_clause, exception));
                return Ok(());
            }
            frame -= 1;
            start = 0;
        }
    }

    /// Передать управление обработчику: стек вычислений пуст, а `finally` и
    /// `catch`, из которых управление при этом уходит, забыты.
    fn enter_handler(&mut self, frame: usize, target: usize) {
        let body = Rc::clone(&self.frames[frame].body);
        let f = &mut self.frames[frame];
        f.stack.clear();
        f.pc = target;
        f.continuations.retain(|c| handler_covers(&body.clauses[c.clause() as usize], target));
        f.caught.retain(|(clause, _)| handler_covers(&body.clauses[*clause as usize], target));
    }

    /// `endfinally`.
    pub(crate) fn end_finally(&mut self) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let Some(continuation) = self.frames[top].continuations.pop() else {
            return Err(self.invalid("endfinally without a running finally"));
        };
        match continuation {
            Continuation::Leave { remaining, target, .. } => self.run_finallies(top, remaining, target),
            Continuation::Unwind { clause, exception, handler_frame, handler_clause } => {
                self.unwind(exception, handler_frame, handler_clause, top, clause + 1)
            }
        }
    }

    /// `leave` из `pc` на `target`: сначала все `finally`, из чьих блоков
    /// управление уходит, изнутри наружу.
    pub(crate) fn leave(&mut self, pc: usize, target: usize) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let body = Rc::clone(&self.frames[top].body);
        let mut finallies = Vec::new();
        for (index, c) in body.clauses.iter().enumerate() {
            if kind(c) == CLAUSE_FINALLY && try_covers(c, pc) && !try_covers(c, target) {
                finallies.try_reserve(1).map_err(|_| VmError::OutOfMemory)?;
                finallies.push(index as u32);
            }
        }
        self.frames[top].caught.retain(|(clause, _)| handler_covers(&body.clauses[*clause as usize], target));
        self.run_finallies(top, finallies, target)
    }

    fn run_finallies(&mut self, frame: usize, mut finallies: Vec<u32>, target: usize) -> Result<(), VmError> {
        if finallies.is_empty() {
            let f = &mut self.frames[frame];
            f.stack.clear();
            f.pc = target;
            return Ok(());
        }
        let clause = finallies.remove(0);
        let handler = self.frames[frame].body.clauses[clause as usize].handler_offset as usize;
        self.enter_handler(frame, handler);
        self.frames[frame].continuations.push(Continuation::Leave { clause, remaining: finallies, target });
        Ok(())
    }

    /// `rethrow`: то исключение, которое поймал обработчик вокруг `pc`.
    pub(crate) fn rethrow(&mut self) -> Result<(), VmError> {
        let top = self.frames.len() - 1;
        let pc = self.frames[top].pc;
        let body = Rc::clone(&self.frames[top].body);
        let found = self.frames[top]
            .caught
            .iter()
            .rev()
            .find(|(clause, _)| handler_covers(&body.clauses[*clause as usize], pc))
            .map(|(_, exception)| *exception);
        match found {
            Some(exception) => self.raise(exception),
            None => Err(self.invalid("rethrow outside a catch handler")),
        }
    }

    /// Исключение, которое бросает сама среда: объект создаётся конструктором
    /// без параметров — текст сообщения живёт в базовой библиотеке, в одном
    /// месте, — и когда конструктор вернётся, объект бросается.
    pub(crate) fn throw_runtime(&mut self, name: &'static str) -> Result<(), VmError> {
        let ty = self.corelib_type(name)?;
        let object = self.new_instance(ty)?;
        let Some(ctor) = self.find_method(ty, ".ctor", ".ctor()void")? else {
            return Err(VmError::MissingMember { name: alloc::format!("{name}::.ctor()") });
        };
        let mut args = Vec::new();
        args.try_reserve_exact(1).map_err(|_| VmError::OutOfMemory)?;
        args.push(Value::Obj(Some(object)));
        self.enter(ctor, args)?;
        let top = self.frames.len() - 1;
        self.frames[top].then_throw = Some(object);
        Ok(())
    }

    /// Необработанное исключение — конец программы. Имя и текст берутся без
    /// вызова управляемого кода: продолжать программу уже нельзя.
    fn unhandled(&mut self, exception: ObjRef) -> VmError {
        let name = match self.heap.get(exception) {
            Some(Object::Instance { ty, .. }) => self.types[ty.0 as usize].name.clone(),
            _ => String::from("?"),
        };
        let message = self.exception_message(exception).unwrap_or_default();
        VmError::Unhandled { name, message, at: self.location() }
    }

    /// Поле `message` у `System.Exception` базовой библиотеки.
    fn exception_message(&mut self, exception: ObjRef) -> Option<String> {
        let base = self.corelib_type("System.Exception").ok()?;
        let field = self.find_field(base, "message").ok()??;
        let value = self.load(Pointer::Field { object: exception, index: field.index }).ok()?;
        let units = self.string_units(value).ok()??;
        Some(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect())
    }
}
