//! Арифметика, сравнения и преобразования по таблицам ECMA-335 (III.1.5).
//!
//! Чистые функции над значениями: ни кучи, ни кадров. Отдельно от цикла
//! исполнения затем, чтобы каждое правило можно было проверить тестом без
//! сборки — деление `int.MinValue` на `-1`, расширение `conv.u8` нулями,
//! сравнение с `NaN` в «беззнаковых» ветвлениях.

use core::cmp::Ordering;

use crate::value::Value;

/// Что пошло не так в операции. Место знает тот, кто её звал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fault {
    DivideByZero,
    Overflow,
    Invalid(&'static str),
}

/// `add` … `xor` и их проверяющие варианты (`add.ovf` … `sub.ovf.un`).
pub(crate) fn binary(op: u8, a: Value, b: Value) -> Result<Value, Fault> {
    match (a, b) {
        (Value::I32(x), Value::I32(y)) => int32(op, x, y).map(Value::I32),
        (Value::I64(x), Value::I64(y)) => int64(op, x, y).map(Value::I64),
        (Value::Native(x), Value::Native(y)) => int64(op, x, y).map(Value::Native),
        // `int32` рядом с `native int` расширяется со знаком (III.1.5, табл. 2).
        (Value::Native(x), Value::I32(y)) => int64(op, x, i64::from(y)).map(Value::Native),
        (Value::I32(x), Value::Native(y)) => int64(op, i64::from(x), y).map(Value::Native),
        (Value::F(x), Value::F(y)) => float(op, x, y).map(Value::F),
        // `float` с `float` — в одинарной точности, как у JIT .NET: округление
        // сразу до `f32`, а не через `f64`, иначе у половин выходит другой бит.
        (Value::F32(x), Value::F32(y)) => float32(op, x, y).map(Value::F32),
        // Вперемешку оба — вид `F`, и `float32` расширяется без потерь.
        (Value::F32(x), Value::F(y)) => float(op, f64::from(x), y).map(Value::F),
        (Value::F(x), Value::F32(y)) => float(op, x, f64::from(y)).map(Value::F),
        _ => Err(Fault::Invalid("arithmetic on incompatible operands")),
    }
}

fn float32(op: u8, x: f32, y: f32) -> Result<f32, Fault> {
    Ok(match op {
        0x58 => x + y,
        0x59 => x - y,
        0x5A => x * y,
        0x5B => x / y,
        0x5D => libm::fmodf(x, y),
        _ => return Err(Fault::Invalid("instruction not defined for floating point")),
    })
}

fn int32(op: u8, x: i32, y: i32) -> Result<i32, Fault> {
    let (ux, uy) = (x as u32, y as u32);
    Ok(match op {
        0x58 => x.wrapping_add(y),
        0x59 => x.wrapping_sub(y),
        0x5A => x.wrapping_mul(y),
        0x5B => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            // `int.MinValue / -1` не помещается в `int` — .NET бросает
            // `OverflowException`, а не отдаёт перенос.
            x.checked_div(y).ok_or(Fault::Overflow)?
        }
        0x5C => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            (ux / uy) as i32
        }
        0x5D => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            x.checked_rem(y).ok_or(Fault::Overflow)?
        }
        0x5E => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            (ux % uy) as i32
        }
        0x5F => x & y,
        0x60 => x | y,
        0x61 => x ^ y,
        0xD6 => x.checked_add(y).ok_or(Fault::Overflow)?,
        0xD7 => ux.checked_add(uy).ok_or(Fault::Overflow)? as i32,
        0xD8 => x.checked_mul(y).ok_or(Fault::Overflow)?,
        0xD9 => ux.checked_mul(uy).ok_or(Fault::Overflow)? as i32,
        0xDA => x.checked_sub(y).ok_or(Fault::Overflow)?,
        0xDB => ux.checked_sub(uy).ok_or(Fault::Overflow)? as i32,
        _ => return Err(Fault::Invalid("not an arithmetic instruction")),
    })
}

fn int64(op: u8, x: i64, y: i64) -> Result<i64, Fault> {
    let (ux, uy) = (x as u64, y as u64);
    Ok(match op {
        0x58 => x.wrapping_add(y),
        0x59 => x.wrapping_sub(y),
        0x5A => x.wrapping_mul(y),
        0x5B => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            x.checked_div(y).ok_or(Fault::Overflow)?
        }
        0x5C => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            (ux / uy) as i64
        }
        0x5D => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            x.checked_rem(y).ok_or(Fault::Overflow)?
        }
        0x5E => {
            if y == 0 {
                return Err(Fault::DivideByZero);
            }
            (ux % uy) as i64
        }
        0x5F => x & y,
        0x60 => x | y,
        0x61 => x ^ y,
        0xD6 => x.checked_add(y).ok_or(Fault::Overflow)?,
        0xD7 => ux.checked_add(uy).ok_or(Fault::Overflow)? as i64,
        0xD8 => x.checked_mul(y).ok_or(Fault::Overflow)?,
        0xD9 => ux.checked_mul(uy).ok_or(Fault::Overflow)? as i64,
        0xDA => x.checked_sub(y).ok_or(Fault::Overflow)?,
        0xDB => ux.checked_sub(uy).ok_or(Fault::Overflow)? as i64,
        _ => return Err(Fault::Invalid("not an arithmetic instruction")),
    })
}

fn float(op: u8, x: f64, y: f64) -> Result<f64, Fault> {
    Ok(match op {
        0x58 => x + y,
        0x59 => x - y,
        0x5A => x * y,
        // Деление чисел с плавающей точкой на ноль не исключение, а
        // бесконечность или NaN — так в IEEE 754 и так в .NET.
        0x5B => x / y,
        0x5D => x % y,
        _ => return Err(Fault::Invalid("instruction not defined for floating point")),
    })
}

/// `shl`, `shr`, `shr.un`. Сдвиг берётся по модулю ширины, как на x86 и ARM:
/// компилятор C# сам маскирует счётчик, а IL со сдвигом на ширину и больше
/// ECMA-335 оставляет неопределённым.
pub(crate) fn shift(op: u8, value: Value, amount: Value) -> Result<Value, Fault> {
    let n = match amount {
        Value::I32(n) => n as u32,
        Value::Native(n) => n as u32,
        _ => return Err(Fault::Invalid("shift amount is not an integer")),
    };
    Ok(match value {
        Value::I32(x) => Value::I32(match op {
            0x62 => x.wrapping_shl(n),
            0x63 => x.wrapping_shr(n),
            _ => (x as u32).wrapping_shr(n) as i32,
        }),
        Value::I64(x) => Value::I64(shift64(op, x, n)),
        Value::Native(x) => Value::Native(shift64(op, x, n)),
        _ => return Err(Fault::Invalid("shift of a non-integer")),
    })
}

fn shift64(op: u8, x: i64, n: u32) -> i64 {
    match op {
        0x62 => x.wrapping_shl(n),
        0x63 => x.wrapping_shr(n),
        _ => (x as u64).wrapping_shr(n) as i64,
    }
}

/// `neg` и `not`.
pub(crate) fn unary(op: u8, value: Value) -> Result<Value, Fault> {
    Ok(match (op, value) {
        (0x65, Value::I32(x)) => Value::I32(x.wrapping_neg()),
        (0x65, Value::I64(x)) => Value::I64(x.wrapping_neg()),
        (0x65, Value::Native(x)) => Value::Native(x.wrapping_neg()),
        (0x65, Value::F(x)) => Value::F(-x),
        (0x65, Value::F32(x)) => Value::F32(-x),
        (0x66, Value::I32(x)) => Value::I32(!x),
        (0x66, Value::I64(x)) => Value::I64(!x),
        (0x66, Value::Native(x)) => Value::Native(!x),
        _ => return Err(Fault::Invalid("unary operation on an incompatible operand")),
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Width {
    I1,
    U1,
    I2,
    U2,
    I4,
    U4,
    I8,
    U8,
    INative,
    UNative,
    R4,
    R8,
    RUn,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    /// `conv.*`: перенос без проверки.
    None,
    /// `conv.ovf.*`: источник знаковый.
    Signed,
    /// `conv.ovf.*.un`: источник беззнаковый.
    Unsigned,
}

/// Все `conv.*`.
pub(crate) fn convert(op: u8, value: Value) -> Result<Value, Fault> {
    use Check::{None as Plain, Signed, Unsigned};
    use Width::*;
    let (width, check) = match op {
        0x67 => (I1, Plain),
        0x68 => (I2, Plain),
        0x69 => (I4, Plain),
        0x6A => (I8, Plain),
        0x6B => (R4, Plain),
        0x6C => (R8, Plain),
        0x6D => (U4, Plain),
        0x6E => (U8, Plain),
        0x76 => (RUn, Plain),
        0xD1 => (U2, Plain),
        0xD2 => (U1, Plain),
        0xD3 => (INative, Plain),
        0xE0 => (UNative, Plain),
        0x82 => (I1, Unsigned),
        0x83 => (I2, Unsigned),
        0x84 => (I4, Unsigned),
        0x85 => (I8, Unsigned),
        0x86 => (U1, Unsigned),
        0x87 => (U2, Unsigned),
        0x88 => (U4, Unsigned),
        0x89 => (U8, Unsigned),
        0x8A => (INative, Unsigned),
        0x8B => (UNative, Unsigned),
        0xB3 => (I1, Signed),
        0xB4 => (U1, Signed),
        0xB5 => (I2, Signed),
        0xB6 => (U2, Signed),
        0xB7 => (I4, Signed),
        0xB8 => (U4, Signed),
        0xB9 => (I8, Signed),
        0xBA => (U8, Signed),
        0xD4 => (INative, Signed),
        0xD5 => (UNative, Signed),
        _ => return Err(Fault::Invalid("not a conversion instruction")),
    };

    // Источник: целое со знаковым и беззнаковым прочтением той же ширины — или
    // число с плавающей точкой.
    enum Source {
        Int { signed: i64, unsigned: u64 },
        Float(f64),
    }
    let source = match value {
        Value::I32(x) => Source::Int { signed: i64::from(x), unsigned: u64::from(x as u32) },
        Value::I64(x) | Value::Native(x) => Source::Int { signed: x, unsigned: x as u64 },
        Value::F(x) => Source::Float(x),
        Value::F32(x) => Source::Float(f64::from(x)),
        _ => return Err(Fault::Invalid("conversion of a reference")),
    };

    match width {
        R4 => {
            return Ok(Value::F32(match source {
                Source::Int { signed, .. } => signed as f32,
                Source::Float(f) => f as f32,
            }));
        }
        R8 => {
            return Ok(Value::F(match source {
                Source::Int { signed, .. } => signed as f64,
                Source::Float(f) => f,
            }));
        }
        RUn => {
            return Ok(Value::F(match source {
                Source::Int { unsigned, .. } => unsigned as f64,
                Source::Float(f) => f,
            }));
        }
        _ => {}
    }

    let (min, max): (i128, i128) = match width {
        I1 => (i8::MIN.into(), i8::MAX.into()),
        U1 => (0, u8::MAX.into()),
        I2 => (i16::MIN.into(), i16::MAX.into()),
        U2 => (0, u16::MAX.into()),
        I4 => (i32::MIN.into(), i32::MAX.into()),
        U4 => (0, u32::MAX.into()),
        I8 | INative => (i64::MIN.into(), i64::MAX.into()),
        _ => (0, u64::MAX.into()),
    };
    let unsigned_target = matches!(width, U1 | U2 | U4 | U8 | UNative);

    let wide: i128 = match source {
        Source::Int { signed, unsigned } => {
            let checked = match check {
                Check::Signed => i128::from(signed),
                Check::Unsigned => i128::from(unsigned),
                // Без проверки беззнаковая ширина берёт биты, расширенные нулями
                // (`conv.u8` от `int32 -1` — это 0xFFFFFFFF, III.3.18), знаковая —
                // расширенные знаком.
                Check::None => {
                    if unsigned_target {
                        i128::from(unsigned)
                    } else {
                        i128::from(signed)
                    }
                }
            };
            if check != Check::None && (checked < min || checked > max) {
                return Err(Fault::Overflow);
            }
            checked
        }
        Source::Float(f) => {
            // `as i128` отбрасывает дробную часть к нулю — это и есть проверка
            // диапазона после усечения (`f64::trunc` в `core` без `std` нет).
            if check != Check::None && (!f.is_finite() || (f as i128) < min || (f as i128) > max) {
                return Err(Fault::Overflow);
            }
            // С .NET 9 преобразование дробного в целое насыщает: вне
            // диапазона — ближайшая граница, NaN — ноль. Так и здесь.
            (f as i128).clamp(min, max)
        }
    };

    Ok(match width {
        I1 => Value::I32(i32::from(wide as i8)),
        U1 => Value::I32(i32::from(wide as u8)),
        I2 => Value::I32(i32::from(wide as i16)),
        U2 => Value::I32(i32::from(wide as u16)),
        I4 => Value::I32(wide as i32),
        U4 => Value::I32(wide as u32 as i32),
        I8 => Value::I64(wide as i64),
        U8 => Value::I64(wide as u64 as i64),
        INative => Value::Native(wide as i64),
        _ => Value::Native(wide as u64 as i64),
    })
}

/// Итог сравнения двух значений.
pub(crate) struct Comparison {
    /// Порядок при знаковом прочтении; `None` — несравнимы (NaN, ссылки).
    signed: Option<Ordering>,
    /// Порядок при беззнаковом прочтении; `None` — «неупорядочены» (NaN).
    unsigned: Option<Ordering>,
    pub(crate) equal: bool,
}

impl Comparison {
    pub(crate) fn greater(&self) -> bool {
        self.signed == Some(Ordering::Greater)
    }

    pub(crate) fn less(&self) -> bool {
        self.signed == Some(Ordering::Less)
    }

    pub(crate) fn greater_or_equal(&self) -> bool {
        matches!(self.signed, Some(Ordering::Greater | Ordering::Equal))
    }

    pub(crate) fn less_or_equal(&self) -> bool {
        matches!(self.signed, Some(Ordering::Less | Ordering::Equal))
    }

    // «Беззнаковые» варианты для чисел с плавающей точкой означают
    // «или неупорядочены»: `bge.un` с NaN переходит, `bge` — нет (III.3.7).

    pub(crate) fn greater_unordered(&self) -> bool {
        matches!(self.unsigned, Some(Ordering::Greater) | None)
    }

    pub(crate) fn less_unordered(&self) -> bool {
        matches!(self.unsigned, Some(Ordering::Less) | None)
    }

    pub(crate) fn greater_or_equal_unordered(&self) -> bool {
        matches!(self.unsigned, Some(Ordering::Greater | Ordering::Equal) | None)
    }

    pub(crate) fn less_or_equal_unordered(&self) -> bool {
        matches!(self.unsigned, Some(Ordering::Less | Ordering::Equal) | None)
    }
}

pub(crate) fn compare(a: Value, b: Value) -> Result<Comparison, Fault> {
    let ints = |x: i64, y: i64, ux: u64, uy: u64| Comparison {
        signed: Some(x.cmp(&y)),
        unsigned: Some(ux.cmp(&uy)),
        equal: x == y,
    };
    Ok(match (a, b) {
        (Value::I32(x), Value::I32(y)) => {
            ints(i64::from(x), i64::from(y), u64::from(x as u32), u64::from(y as u32))
        }
        (Value::I64(x), Value::I64(y)) | (Value::Native(x), Value::Native(y)) => {
            ints(x, y, x as u64, y as u64)
        }
        (Value::I32(x), Value::Native(y)) => ints(i64::from(x), y, i64::from(x) as u64, y as u64),
        (Value::Native(x), Value::I32(y)) => ints(x, i64::from(y), x as u64, i64::from(y) as u64),
        (Value::F(x), Value::F(y)) => floats(x, y),
        // `float32` расширяется до `f64` без потерь, и порядок тот же.
        (Value::F32(x), Value::F32(y)) => floats(f64::from(x), f64::from(y)),
        (Value::F32(x), Value::F(y)) => floats(f64::from(x), y),
        (Value::F(x), Value::F32(y)) => floats(x, f64::from(y)),
        // Ссылки сравниваются только на равенство — и с `null` через `cgt.un`,
        // которым компилятор C# записывает `x != null`. Порядок номеров в куче
        // смысла не несёт, но детерминирован.
        (Value::Obj(x), Value::Obj(y)) => Comparison { signed: None, unsigned: Some(x.cmp(&y)), equal: x == y },
        (Value::Ptr(x), Value::Ptr(y)) => {
            let equal = x == y;
            Comparison {
                signed: None,
                unsigned: Some(if equal { Ordering::Equal } else { Ordering::Greater }),
                equal,
            }
        }
        _ => return Err(Fault::Invalid("comparison of incompatible operands")),
    })
}

fn floats(x: f64, y: f64) -> Comparison {
    let order = x.partial_cmp(&y);
    Comparison { signed: order, unsigned: order, equal: x == y }
}

/// Истинность для `brtrue`/`brfalse`.
pub(crate) fn truthy(value: Value) -> Result<bool, Fault> {
    Ok(match value {
        Value::I32(x) => x != 0,
        Value::I64(x) | Value::Native(x) => x != 0,
        Value::Obj(x) => x.is_some(),
        Value::Ptr(_) => true,
        Value::Struct(_) => return Err(Fault::Invalid("branch on a struct value")),
        Value::Fn(_) => true,
        Value::F(_) | Value::F32(_) => return Err(Fault::Invalid("branch on a floating point value")),
    })
}
