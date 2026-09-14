//! Начало разбора сигнатур (ECMA-335 II.23.2).
//!
//! Фазе N1 от сигнатур нужно немногое: соглашение о вызове и число параметров —
//! сверка с чужим читателем идёт по сырым байтам сигнатур целиком. Разбор типов
//! внутри сигнатуры (`class`, `valuetype`, обобщения, массивы) пишется в фазе
//! N2, когда появится тот, кто им пользуется.

use crate::Error;
use crate::bytes::compressed_u32;
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
        let (count, n) = compressed_u32(&blob[used..]).ok_or(Error::BadSignature("generic count"))?;
        generic_params = count;
        used += n;
    }
    let (params, n) = compressed_u32(&blob[used..]).ok_or(Error::BadSignature("param count"))?;
    used += n;
    Ok(MethodHeader { convention, generic_params, params, used })
}

/// Токен типа, записанный в сигнатуре как `TypeDefOrRefOrSpecEncoded`.
///
/// Это кодированный индекс `TypeDefOrRef`, упакованный ещё и сжатым целым, —
/// два слоя кодирования на одно число.
pub fn type_token(encoded: u32) -> Result<Token, Error> {
    Tables::coded(Coded::TypeDefOrRef, encoded)
}
