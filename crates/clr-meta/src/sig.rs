//! Сигнатуры (ECMA-335 II.23.2): заголовок метода, типы, локальные переменные.
//!
//! Сигнатура — это двоичная запись типа: байт элемента (`int32`, `class`,
//! `szarray`…), за ним, если нужно, токен или вложенный тип. Разбор здесь
//! идёт по позициям в исходных байтах и ничего не выделяет: тип «массив
//! массивов строк» — это три элемента подряд, и пройти их можно, не строя
//! дерево.

use core::fmt::Write;

use crate::Error;
use crate::bytes::{compressed_i32, compressed_u32};
use crate::tables::{Coded, Tables, Token};

/// Соглашение о вызове: обычный метод.
pub const CALL_DEFAULT: u8 = 0x00;
/// Метод с переменным числом аргументов (`__arglist`).
pub const CALL_VARARG: u8 = 0x05;
/// Сигнатура поля.
pub const FIELD: u8 = 0x06;
/// Сигнатура локальных переменных.
pub const LOCAL_SIG: u8 = 0x07;
/// Сигнатура свойства.
pub const PROPERTY: u8 = 0x08;
/// Флаг: у метода есть обобщённые параметры.
pub const GENERIC: u8 = 0x10;
/// Флаг: у метода есть `this`.
pub const HAS_THIS: u8 = 0x20;
/// Флаг: `this` передаётся явно первым параметром.
pub const EXPLICIT_THIS: u8 = 0x40;

/// Коды элементов типа (ECMA-335 II.23.1.16).
pub mod elem {
    pub const END: u8 = 0x00;
    pub const VOID: u8 = 0x01;
    pub const BOOLEAN: u8 = 0x02;
    pub const CHAR: u8 = 0x03;
    pub const I1: u8 = 0x04;
    pub const U1: u8 = 0x05;
    pub const I2: u8 = 0x06;
    pub const U2: u8 = 0x07;
    pub const I4: u8 = 0x08;
    pub const U4: u8 = 0x09;
    pub const I8: u8 = 0x0A;
    pub const U8: u8 = 0x0B;
    pub const R4: u8 = 0x0C;
    pub const R8: u8 = 0x0D;
    pub const STRING: u8 = 0x0E;
    pub const PTR: u8 = 0x0F;
    pub const BYREF: u8 = 0x10;
    pub const VALUETYPE: u8 = 0x11;
    pub const CLASS: u8 = 0x12;
    pub const VAR: u8 = 0x13;
    pub const ARRAY: u8 = 0x14;
    pub const GENERICINST: u8 = 0x15;
    pub const TYPEDBYREF: u8 = 0x16;
    pub const I: u8 = 0x18;
    pub const U: u8 = 0x19;
    pub const FNPTR: u8 = 0x1B;
    pub const OBJECT: u8 = 0x1C;
    pub const SZARRAY: u8 = 0x1D;
    pub const MVAR: u8 = 0x1E;
    pub const CMOD_REQD: u8 = 0x1F;
    pub const CMOD_OPT: u8 = 0x20;
    pub const SENTINEL: u8 = 0x41;
    pub const PINNED: u8 = 0x45;
}

/// Самая глубокая вложенность типа, которую разбор принимает.
///
/// Разбор рекурсивен, а сигнатура пришла из файла: без предела
/// `szarray szarray szarray …` в тысячу слоёв исчерпал бы стек программы.
/// Тридцать два слоя не встречаются ни в одной настоящей сборке.
const MAX_DEPTH: u32 = 32;

const TRUNCATED: Error = Error::BadSignature("truncated");
const FORMAT: Error = Error::BadSignature("cannot format");

/// Начало сигнатуры метода.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodHeader {
    /// Первый байт: соглашение и флаги.
    pub convention: u8,
    /// Число обобщённых параметров; ноль у необобщённых.
    pub generic_params: u32,
    /// Число параметров без возвращаемого значения и без `this`.
    pub params: u32,
    /// Сколько байт заголовок занял — дальше тип возвращаемого значения.
    pub used: usize,
}

/// Прочитать начало сигнатуры метода.
pub fn method_header(blob: &[u8]) -> Result<MethodHeader, Error> {
    let convention = *blob.first().ok_or(Error::BadSignature("empty method signature"))?;
    let mut used = 1;
    let mut generic_params = 0;
    if convention & GENERIC != 0 {
        generic_params = read_u(blob, &mut used)?;
    }
    let params = read_u(blob, &mut used)?;
    Ok(MethodHeader { convention, generic_params, params, used })
}

/// Сигнатура метода: заголовок и где что начинается.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MethodSig {
    pub header: MethodHeader,
    /// Возвращает ли метод значение (тип возврата — не `void`).
    pub returns_value: bool,
    /// Где начинается первый параметр.
    pub params_at: usize,
}

pub fn method_sig(blob: &[u8]) -> Result<MethodSig, Error> {
    let header = method_header(blob)?;
    let returns_value = element(blob, header.used)? != elem::VOID;
    let params_at = skip_type(blob, header.used)?;
    Ok(MethodSig { header, returns_value, params_at })
}

/// Токен типа, записанный в сигнатуре как `TypeDefOrRefOrSpecEncoded`.
///
/// Это кодированный индекс `TypeDefOrRef`, упакованный ещё и сжатым целым, —
/// два слоя кодирования на одно число.
pub fn type_token(encoded: u32) -> Result<Token, Error> {
    Tables::coded(Coded::TypeDefOrRef, encoded)
}

/// Кто называет типы по токенам.
///
/// Сигнатура знает только токен; имя живёт в таблицах сборки. Трейт, а не
/// сборка напрямую, — чтобы разбор сигнатур не зависел от того, откуда имена.
pub trait TypeNames {
    fn write_name(&self, token: Token, out: &mut dyn Write) -> Result<(), Error>;

    /// Записать обобщённый параметр: `!0` — параметр типа, `!!0` — метода.
    ///
    /// Среда выполнения подставляет сюда аргументы экземпляра: у
    /// `Base<int>::M(!0)` и `Derived::M(int32)` одна и та же ячейка таблицы
    /// виртуальных методов, и увидеть это можно, только записав `!0` как `int32`.
    fn write_var(&self, number: u32, method: bool, out: &mut dyn Write) -> Result<(), Error> {
        let bang = if method { "!!" } else { "!" };
        write!(out, "{bang}{number}").map_err(|_| FORMAT)
    }
}

/// Имена-заглушки для обхода, которому имена не нужны.
struct Anonymous;

impl TypeNames for Anonymous {
    fn write_name(&self, token: Token, out: &mut dyn Write) -> Result<(), Error> {
        write!(out, "{:08x}", token.value()).map_err(|_| FORMAT)
    }
}

/// Вывод, которому некуда писать.
struct Sink;

impl Write for Sink {
    fn write_str(&mut self, _: &str) -> core::fmt::Result {
        Ok(())
    }
}

/// Обёртка, превращающая `&mut W` любого размера в `&mut dyn Write`.
struct Dyn<'w, W: Write + ?Sized>(&'w mut W);

impl<W: Write + ?Sized> Write for Dyn<'_, W> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.0.write_str(text)
    }
}

fn read_u(blob: &[u8], at: &mut usize) -> Result<u32, Error> {
    let (value, used) = compressed_u32(blob.get(*at..).ok_or(TRUNCATED)?).ok_or(TRUNCATED)?;
    *at += used;
    Ok(value)
}

fn read_i(blob: &[u8], at: &mut usize) -> Result<i32, Error> {
    let (value, used) = compressed_i32(blob.get(*at..).ok_or(TRUNCATED)?).ok_or(TRUNCATED)?;
    *at += used;
    Ok(value)
}

/// Пропустить модификаторы (`modreq`, `modopt`) и `pinned` перед типом.
pub fn skip_modifiers(blob: &[u8], at: usize) -> Result<usize, Error> {
    let mut at = at;
    loop {
        match blob.get(at).copied() {
            Some(elem::CMOD_REQD | elem::CMOD_OPT) => {
                at += 1;
                read_u(blob, &mut at)?;
            }
            Some(elem::PINNED) => at += 1,
            Some(_) => return Ok(at),
            None => return Err(TRUNCATED),
        }
    }
}

/// Код элемента типа, начинающегося с `at`, без модификаторов.
pub fn element(blob: &[u8], at: usize) -> Result<u8, Error> {
    let at = skip_modifiers(blob, at)?;
    blob.get(at).copied().ok_or(TRUNCATED)
}

/// Пропустить один тип целиком. Возвращает позицию за ним.
pub fn skip_type(blob: &[u8], at: usize) -> Result<usize, Error> {
    visit::<Sink>(blob, at, 0, None, &Anonymous)
}

/// Записать тип словами — `int32`, `string[]`, `System.Collections.Generic.List<int32>`.
/// Возвращает позицию за типом.
pub fn write_type<W: Write + ?Sized>(
    blob: &[u8],
    at: usize,
    names: &dyn TypeNames,
    out: &mut W,
) -> Result<usize, Error> {
    visit(blob, at, 0, Some(out), names)
}

/// Параметры метода через запятую — `int32,string`.
pub fn write_params<W: Write + ?Sized>(
    blob: &[u8],
    names: &dyn TypeNames,
    out: &mut W,
) -> Result<(), Error> {
    let sig = method_sig(blob)?;
    let mut at = sig.params_at;
    for index in 0..sig.header.params {
        if index > 0 {
            out.write_str(",").map_err(|_| FORMAT)?;
        }
        if blob.get(at) == Some(&elem::SENTINEL) {
            at += 1;
        }
        at = write_type(blob, at, names, out)?;
    }
    Ok(())
}

/// Сигнатура локальных переменных: сколько их и где начинается первая.
pub fn locals(blob: &[u8]) -> Result<(u32, usize), Error> {
    if blob.first() != Some(&LOCAL_SIG) {
        return Err(Error::BadSignature("not a local variable signature"));
    }
    let mut at = 1;
    let count = read_u(blob, &mut at)?;
    Ok((count, at))
}

fn emit<W: Write + ?Sized>(out: &mut Option<&mut W>, text: &str) -> Result<(), Error> {
    match out {
        Some(out) => out.write_str(text).map_err(|_| FORMAT),
        None => Ok(()),
    }
}

fn visit<W: Write + ?Sized>(
    blob: &[u8],
    at: usize,
    depth: u32,
    mut out: Option<&mut W>,
    names: &dyn TypeNames,
) -> Result<usize, Error> {
    if depth > MAX_DEPTH {
        return Err(Error::BadSignature("type nested too deeply"));
    }
    let mut at = skip_modifiers(blob, at)?;
    let code = *blob.get(at).ok_or(TRUNCATED)?;
    at += 1;

    let primitive = match code {
        elem::VOID => Some("void"),
        elem::BOOLEAN => Some("bool"),
        elem::CHAR => Some("char"),
        elem::I1 => Some("int8"),
        elem::U1 => Some("uint8"),
        elem::I2 => Some("int16"),
        elem::U2 => Some("uint16"),
        elem::I4 => Some("int32"),
        elem::U4 => Some("uint32"),
        elem::I8 => Some("int64"),
        elem::U8 => Some("uint64"),
        elem::R4 => Some("float32"),
        elem::R8 => Some("float64"),
        elem::STRING => Some("string"),
        elem::OBJECT => Some("object"),
        elem::I => Some("native int"),
        elem::U => Some("native uint"),
        elem::TYPEDBYREF => Some("typedref"),
        _ => None,
    };
    if let Some(name) = primitive {
        emit(&mut out, name)?;
        return Ok(at);
    }

    match code {
        elem::PTR | elem::BYREF | elem::SZARRAY => {
            at = visit(blob, at, depth + 1, out.as_deref_mut(), names)?;
            let suffix = match code {
                elem::PTR => "*",
                elem::BYREF => "&",
                _ => "[]",
            };
            emit(&mut out, suffix)?;
        }
        elem::VALUETYPE | elem::CLASS => {
            let token = type_token(read_u(blob, &mut at)?)?;
            if let Some(out) = out.as_deref_mut() {
                names.write_name(token, &mut Dyn(out))?;
            }
        }
        elem::VAR | elem::MVAR => {
            let number = read_u(blob, &mut at)?;
            if let Some(out) = out.as_deref_mut() {
                names.write_var(number, code == elem::MVAR, &mut Dyn(out))?;
            }
        }
        elem::ARRAY => {
            at = visit(blob, at, depth + 1, out.as_deref_mut(), names)?;
            let rank = read_u(blob, &mut at)?;
            let sizes = read_u(blob, &mut at)?;
            for _ in 0..sizes {
                read_u(blob, &mut at)?;
            }
            let bounds = read_u(blob, &mut at)?;
            for _ in 0..bounds {
                read_i(blob, &mut at)?;
            }
            emit(&mut out, "[")?;
            for _ in 1..rank {
                emit(&mut out, ",")?;
            }
            emit(&mut out, "]")?;
        }
        elem::GENERICINST => {
            let kind = *blob.get(at).ok_or(TRUNCATED)?;
            at += 1;
            if kind != elem::CLASS && kind != elem::VALUETYPE {
                return Err(Error::BadSignature("generic instance of neither a class nor a value type"));
            }
            let token = type_token(read_u(blob, &mut at)?)?;
            if let Some(out) = out.as_deref_mut() {
                names.write_name(token, &mut Dyn(out))?;
            }
            let count = read_u(blob, &mut at)?;
            emit(&mut out, "<")?;
            for index in 0..count {
                if index > 0 {
                    emit(&mut out, ",")?;
                }
                at = visit(blob, at, depth + 1, out.as_deref_mut(), names)?;
            }
            emit(&mut out, ">")?;
        }
        elem::FNPTR => {
            at = skip_method(blob, at, depth + 1)?;
            emit(&mut out, "method")?;
        }
        _ => return Err(Error::BadSignature("unknown element type")),
    }
    Ok(at)
}

/// Пропустить сигнатуру метода внутри типа (`fnptr`).
fn skip_method(blob: &[u8], at: usize, depth: u32) -> Result<usize, Error> {
    let header = method_header(blob.get(at..).ok_or(TRUNCATED)?)?;
    let mut at = visit::<Sink>(blob, at + header.used, depth, None, &Anonymous)?;
    for _ in 0..header.params {
        if blob.get(at) == Some(&elem::SENTINEL) {
            at += 1;
        }
        at = visit::<Sink>(blob, at, depth, None, &Anonymous)?;
    }
    Ok(at)
}
