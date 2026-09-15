//! Тип `decimal` (фаза N7g): 96-битная мантисса, масштаб от 0 до 28 и знак — как
//! `System.Decimal` в CoreCLR.
//!
//! # Округление
//!
//! Цифры, не влезающие в мантиссу или за 28-й знак после запятой, отбрасываются
//! с округлением к чётному (`ScaleResult` у CoreCLR). Деление досчитывает цифры,
//! пока остаток не нуль, масштаб меньше 28 и следующая цифра влезает, — поэтому
//! `1m / 3m` печатается 0.3333333333333333333333333333, а `10m / 4m` — 2.5, без
//! хвостовых нулей. Сложение берёт больший масштаб: `1.10m + 0m` — 1.10.
//!
//! # Граница с C#
//!
//! Базовая библиотека отдаёт сюда число четырьмя `int` — ровно как
//! `decimal.GetBits` — и получает обратно так же. Читать структуру из памяти не
//! нужно, а сверка с .NET идёт образцом `numbers`.

use alloc::vec::Vec;

/// Наибольшая мантисса: 2^96 − 1.
const MAX: u128 = (1u128 << 96) - 1;

/// Наибольший масштаб.
const MAX_SCALE: u32 = 28;

/// Степени десяти как `double` — та же таблица, что у CoreCLR
/// (`s_doublePowers10`): `1e23` в ней — ближайшее к литералу, а не произведение.
const DOUBLE_POWERS: [f64; 29] = [
    1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
    1e17, 1e18, 1e19, 1e20, 1e21, 1e22, 1e23, 1e24, 1e25, 1e26, 1e27, 1e28,
];

/// Число `decimal`: `mantissa · 10^-scale`, со знаком.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decimal {
    pub mantissa: u128,
    pub scale: u32,
    pub negative: bool,
}

/// Почему арифметика не дала числа.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecimalError {
    Overflow,
    DivideByZero,
}

/// Почему строка не стала числом.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Format,
    Overflow,
}

impl Decimal {
    /// Из частей `decimal.GetBits`. `None` — флаги неверны: лишние биты или
    /// масштаб больше 28.
    #[must_use]
    pub fn from_bits(lo: i32, mid: i32, hi: i32, flags: i32) -> Option<Self> {
        let flags = flags as u32;
        let scale = (flags >> 16) & 0xFF;
        if flags & 0x7F00_FFFF != 0 || scale > MAX_SCALE {
            return None;
        }
        let mantissa = u128::from(lo as u32) | (u128::from(mid as u32) << 32) | (u128::from(hi as u32) << 64);
        Some(Self { mantissa, scale, negative: flags & 0x8000_0000 != 0 })
    }

    /// Части для `decimal.GetBits`.
    #[must_use]
    pub fn bits(self) -> [i32; 4] {
        let m = self.mantissa;
        let sign = if self.negative { 0x8000_0000u32 } else { 0 };
        [m as u32 as i32, (m >> 32) as u32 as i32, (m >> 64) as u32 as i32, ((self.scale << 16) | sign) as i32]
    }
}

/// Беззнаковое в 256 бит: промежуточные произведения и выравнивание масштабов
/// выходят за 128.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Wide {
    hi: u128,
    lo: u128,
}

const LOW64: u128 = u64::MAX as u128;

impl Wide {
    const ZERO: Self = Self { hi: 0, lo: 0 };

    const fn from(value: u128) -> Self {
        Self { hi: 0, lo: value }
    }

    /// Полное произведение двух 128-битных.
    fn mul(a: u128, b: u128) -> Self {
        let (a1, a0) = (a >> 64, a & LOW64);
        let (b1, b0) = (b >> 64, b & LOW64);
        let p00 = a0 * b0;
        let p01 = a0 * b1;
        let p10 = a1 * b0;
        let p11 = a1 * b1;
        let middle = (p00 >> 64) + (p01 & LOW64) + (p10 & LOW64);
        Self { hi: p11 + (p01 >> 64) + (p10 >> 64) + (middle >> 64), lo: (p00 & LOW64) | ((middle & LOW64) << 64) }
    }

    fn add(self, other: Self) -> Self {
        let (lo, carry) = self.lo.overflowing_add(other.lo);
        Self { hi: self.hi + other.hi + u128::from(carry), lo }
    }

    fn sub(self, other: Self) -> Self {
        let (lo, borrow) = self.lo.overflowing_sub(other.lo);
        Self { hi: self.hi - other.hi - u128::from(borrow), lo }
    }

    /// Деление на небольшое число с остатком — по 64-битным разрядам.
    fn div_small(self, divisor: u64) -> (Self, u64) {
        let limbs = [(self.hi >> 64) as u64, self.hi as u64, (self.lo >> 64) as u64, self.lo as u64];
        let mut quotient = [0u64; 4];
        let mut remainder: u128 = 0;
        for (i, limb) in limbs.iter().enumerate() {
            let current = (remainder << 64) | u128::from(*limb);
            quotient[i] = (current / u128::from(divisor)) as u64;
            remainder = current % u128::from(divisor);
        }
        let hi = (u128::from(quotient[0]) << 64) | u128::from(quotient[1]);
        let lo = (u128::from(quotient[2]) << 64) | u128::from(quotient[3]);
        (Self { hi, lo }, remainder as u64)
    }

    /// Остаток от деления сдвигом и вычитанием. Делитель меньше 2^255.
    fn rem(self, divisor: Self) -> Self {
        let mut remainder = Self::ZERO;
        for bit in (0..256).rev() {
            remainder = Self { hi: (remainder.hi << 1) | (remainder.lo >> 127), lo: remainder.lo << 1 };
            let set = if bit >= 128 { (self.hi >> (bit - 128)) & 1 } else { (self.lo >> bit) & 1 };
            remainder.lo |= set;
            if remainder >= divisor {
                remainder = remainder.sub(divisor);
            }
        }
        remainder
    }

    const fn fits(self) -> bool {
        self.hi == 0 && self.lo <= MAX
    }
}

/// 10^n для n ≤ 38.
const fn pow10(n: u32) -> u128 {
    let mut value = 1u128;
    let mut i = 0;
    while i < n {
        value *= 10;
        i += 1;
    }
    value
}

/// Уложить `value · 10^-scale` в мантиссу и масштаб: лишние цифры уходят с
/// округлением к чётному. Целая часть больше 96 бит — переполнение.
fn fit(mut value: Wide, mut scale: u32, negative: bool) -> Result<Decimal, DecimalError> {
    let mut last = 0u64;
    let mut sticky = false;
    while !value.fits() || scale > MAX_SCALE {
        if scale == 0 {
            return Err(DecimalError::Overflow);
        }
        sticky |= last != 0;
        let (quotient, remainder) = value.div_small(10);
        value = quotient;
        last = remainder;
        scale -= 1;
    }
    if last > 5 || (last == 5 && (sticky || value.lo & 1 == 1)) {
        value = value.add(Wide::from(1));
        if !value.fits() {
            // 2^96 — одна лишняя цифра: её шестёрка округляет вверх.
            if scale == 0 {
                return Err(DecimalError::Overflow);
            }
            let (quotient, remainder) = value.div_small(10);
            value = if remainder >= 5 { quotient.add(Wide::from(1)) } else { quotient };
            scale -= 1;
        }
    }
    Ok(Decimal { mantissa: value.lo, scale, negative })
}

/// Обе мантиссы при общем, большем масштабе.
fn aligned(a: Decimal, b: Decimal) -> (Wide, Wide, u32) {
    let scale = a.scale.max(b.scale);
    (Wide::mul(a.mantissa, pow10(scale - a.scale)), Wide::mul(b.mantissa, pow10(scale - b.scale)), scale)
}

pub fn add(a: Decimal, b: Decimal) -> Result<Decimal, DecimalError> {
    let (x, y, scale) = aligned(a, b);
    if a.negative == b.negative {
        fit(x.add(y), scale, a.negative)
    } else if x >= y {
        fit(x.sub(y), scale, a.negative)
    } else {
        fit(y.sub(x), scale, b.negative)
    }
}

pub fn sub(a: Decimal, b: Decimal) -> Result<Decimal, DecimalError> {
    add(a, Decimal { negative: !b.negative, ..b })
}

pub fn mul(a: Decimal, b: Decimal) -> Result<Decimal, DecimalError> {
    fit(Wide::mul(a.mantissa, b.mantissa), a.scale + b.scale, a.negative != b.negative)
}

pub fn div(a: Decimal, b: Decimal) -> Result<Decimal, DecimalError> {
    if b.mantissa == 0 {
        return Err(DecimalError::DivideByZero);
    }
    let negative = a.negative != b.negative;
    let divisor = b.mantissa;
    let mut scale = a.scale as i32 - b.scale as i32;
    let mut quotient = a.mantissa / divisor;
    let mut remainder = a.mantissa % divisor;
    // Масштаб делимого меньше — досчитать до нуля после запятой.
    while scale < 0 {
        let shifted = remainder * 10;
        quotient = quotient * 10 + shifted / divisor;
        remainder = shifted % divisor;
        scale += 1;
        if quotient > MAX {
            return Err(DecimalError::Overflow);
        }
    }
    while remainder != 0 && scale < MAX_SCALE as i32 {
        let shifted = remainder * 10;
        let next = quotient * 10 + shifted / divisor;
        if next > MAX {
            break;
        }
        quotient = next;
        remainder = shifted % divisor;
        scale += 1;
    }
    if remainder != 0 {
        let twice = remainder * 2;
        if twice > divisor || (twice == divisor && quotient & 1 == 1) {
            quotient += 1;
        }
        if quotient > MAX {
            return fit(Wide::from(quotient), scale as u32, negative);
        }
    }
    Ok(Decimal { mantissa: quotient, scale: scale as u32, negative })
}

/// Остаток: знак делимого, больший из масштабов.
pub fn rem(a: Decimal, b: Decimal) -> Result<Decimal, DecimalError> {
    if b.mantissa == 0 {
        return Err(DecimalError::DivideByZero);
    }
    let (x, y, scale) = aligned(a, b);
    fit(x.rem(y), scale, a.negative)
}

/// −1, 0 или 1. Нули равны при любом знаке и масштабе.
#[must_use]
pub fn compare(a: Decimal, b: Decimal) -> i32 {
    let a_negative = a.negative && a.mantissa != 0;
    let b_negative = b.negative && b.mantissa != 0;
    if a.mantissa == 0 && b.mantissa == 0 {
        return 0;
    }
    if a_negative != b_negative {
        return if a_negative { -1 } else { 1 };
    }
    let (x, y, _) = aligned(a, b);
    let order = x.cmp(&y) as i32;
    if a_negative { -order } else { order }
}

/// Оставить `decimals` знаков после запятой. `mode` — `MidpointRounding`:
/// 0 к чётному, 1 от нуля, 2 к нулю, 3 к минус бесконечности, 4 к плюс.
#[must_use]
pub fn round(value: Decimal, decimals: u32, mode: u32) -> Decimal {
    if value.scale <= decimals {
        return value;
    }
    let divisor = pow10(value.scale - decimals);
    let quotient = value.mantissa / divisor;
    let remainder = value.mantissa % divisor;
    let up = match mode {
        0 => remainder * 2 > divisor || (remainder * 2 == divisor && quotient & 1 == 1),
        1 => remainder * 2 >= divisor,
        2 => false,
        3 => remainder != 0 && value.negative,
        _ => remainder != 0 && !value.negative,
    };
    Decimal { mantissa: quotient + u128::from(up), scale: decimals, negative: value.negative }
}

/// `VarR8FromDec`: мантисса как `double`, делённая на степень десяти.
#[must_use]
pub fn to_double(value: Decimal) -> f64 {
    let low = value.mantissa as u64;
    let high = (value.mantissa >> 64) as u32;
    let magnitude = (low as f64 + f64::from(high) * 18_446_744_073_709_551_616.0) / DOUBLE_POWERS[value.scale as usize];
    if value.negative { -magnitude } else { magnitude }
}

/// `VarDecFromR8` и `VarDecFromR4`: число округляется до 15 значащих цифр у
/// `double` и до 7 у `float`, хвостовые нули снимаются, пока позволяет масштаб.
pub fn from_float(input: f64, single: bool) -> Result<Decimal, DecimalError> {
    if input.is_nan() || input.is_infinite() {
        return Err(DecimalError::Overflow);
    }
    let exponent = if single {
        (((input as f32).to_bits() >> 23) & 0xFF) as i32 - 126
    } else {
        ((input.to_bits() >> 52) & 0x7FF) as i32 - 1022
    };
    if exponent < -94 {
        return Ok(Decimal { mantissa: 0, scale: 0, negative: false });
    }
    if exponent > 96 {
        return Err(DecimalError::Overflow);
    }
    let negative = input < 0.0;
    let mut dbl = input.abs();
    let (digits, low_limit, high_limit) = if single { (6, 1e6, 1e7) } else { (14, 1e14, 1e15) };
    let mut power = digits - ((exponent * 19728) >> 16);
    if power >= 0 {
        if power > MAX_SCALE as i32 {
            power = MAX_SCALE as i32;
        }
        dbl *= DOUBLE_POWERS[power as usize];
    } else if power != -1 || dbl >= high_limit {
        dbl /= DOUBLE_POWERS[(-power) as usize];
    } else {
        power = 0;
    }
    if dbl < low_limit && power < MAX_SCALE as i32 {
        dbl *= 10.0;
        power += 1;
    }
    // `rint` при обычном режиме округления — половина к чётному, как `Math.Round`
    // у CoreCLR; `f64::round_ties_even` без `std` недоступен.
    let mut mantissa = libm::rint(dbl) as u64;
    if mantissa == 0 {
        return Ok(Decimal { mantissa: 0, scale: 0, negative: false });
    }
    if power < 0 {
        let value = u128::from(mantissa) * pow10((-power) as u32);
        if value > MAX {
            return Err(DecimalError::Overflow);
        }
        return Ok(Decimal { mantissa: value, scale: 0, negative });
    }
    let mut limit = power.min(if single { 6 } else { 14 });
    for step in [8, 4, 2, 1] {
        if limit >= step {
            let divisor = pow10(step as u32) as u64;
            if mantissa % divisor == 0 {
                mantissa /= divisor;
                power -= step;
                limit -= step;
            }
        }
    }
    Ok(Decimal { mantissa: u128::from(mantissa), scale: power as u32, negative })
}

/// `NumberStyles` — те же биты, что у .NET.
const LEADING_WHITE: u32 = 1;
const TRAILING_WHITE: u32 = 2;
const LEADING_SIGN: u32 = 4;
const TRAILING_SIGN: u32 = 8;
const PARENTHESES: u32 = 16;
const DECIMAL_POINT: u32 = 32;
const THOUSANDS: u32 = 64;
const EXPONENT: u32 = 128;

/// Разбор по `NumberStyles` с инвариантной культурой. Хвостовые нули дробной
/// части сохраняются в масштабе: `"3.50"` — это 3.50, а не 3.5.
pub fn parse(text: &[u16], style: u32) -> Result<Decimal, ParseError> {
    let white = |c: u16| c == 0x20 || (0x09..=0x0D).contains(&c);
    let digit = |c: u16| (0x30..=0x39).contains(&c);
    let mut p = 0usize;
    let at = |p: usize| text.get(p).copied();
    if style & LEADING_WHITE != 0 {
        while at(p).is_some_and(white) {
            p += 1;
        }
    }
    let mut negative = false;
    let mut signed = false;
    let mut parenthesized = false;
    if style & LEADING_SIGN != 0 {
        if let Some(c) = at(p) {
            if c == 0x2B || c == 0x2D {
                negative = c == 0x2D;
                signed = true;
                p += 1;
            }
        }
    }
    if !signed && style & PARENTHESES != 0 && at(p) == Some(0x28) {
        parenthesized = true;
        negative = true;
        p += 1;
    }
    // Значащие цифры (до 40 — дальше только липкий бит) и порядок.
    let mut significant: Vec<u8> = Vec::new();
    let mut sticky = false;
    let mut exponent: i64 = 0;
    let mut seen_digit = false;
    let mut in_fraction = false;
    loop {
        match at(p) {
            Some(c) if digit(c) => {
                seen_digit = true;
                let d = (c - 0x30) as u8;
                if significant.is_empty() && d == 0 {
                    if in_fraction {
                        exponent -= 1;
                    }
                } else if significant.len() < 40 {
                    significant.push(d);
                    if in_fraction {
                        exponent -= 1;
                    }
                } else {
                    sticky |= d != 0;
                    if !in_fraction {
                        exponent += 1;
                    }
                }
            }
            Some(0x2E) if style & DECIMAL_POINT != 0 && !in_fraction => in_fraction = true,
            Some(0x2C) if style & THOUSANDS != 0 && !in_fraction && seen_digit => {}
            _ => break,
        }
        p += 1;
    }
    if !seen_digit {
        return Err(ParseError::Format);
    }
    if style & EXPONENT != 0 && matches!(at(p), Some(0x45 | 0x65)) {
        let mark = p;
        p += 1;
        let mut exp_negative = false;
        if let Some(c) = at(p) {
            if c == 0x2B || c == 0x2D {
                exp_negative = c == 0x2D;
                p += 1;
            }
        }
        if at(p).is_some_and(digit) {
            let mut value: i64 = 0;
            while let Some(c) = at(p).filter(|&c| digit(c)) {
                value = (value * 10 + i64::from(c - 0x30)).min(1 << 20);
                p += 1;
            }
            exponent += if exp_negative { -value } else { value };
        } else {
            p = mark;
        }
    }
    if !signed && !parenthesized && style & TRAILING_SIGN != 0 {
        if let Some(c) = at(p) {
            if c == 0x2B || c == 0x2D {
                negative = c == 0x2D;
                p += 1;
            }
        }
    }
    if parenthesized {
        if at(p) != Some(0x29) {
            return Err(ParseError::Format);
        }
        p += 1;
    }
    if style & TRAILING_WHITE != 0 {
        while at(p).is_some_and(white) {
            p += 1;
        }
    }
    if text[p..].iter().any(|&c| c != 0) {
        return Err(ParseError::Format);
    }

    // Значение: significant · 10^exponent.
    let mut value = Wide::ZERO;
    for &d in &significant {
        value = Wide::mul(value.lo, 10).add(Wide::from(u128::from(d)));
    }
    if exponent >= 0 {
        if exponent > 60 && !significant.is_empty() {
            return Err(ParseError::Overflow);
        }
        let mut scaled = value;
        for _ in 0..exponent {
            scaled = Wide::mul(scaled.lo, 10);
            if !scaled.fits() && !significant.is_empty() {
                return Err(ParseError::Overflow);
            }
        }
        if !scaled.fits() {
            return Err(ParseError::Overflow);
        }
        return Ok(Decimal { mantissa: scaled.lo, scale: 0, negative });
    }
    let scale = (-exponent) as u64;
    // Мелкие числа за 28-м знаком — ноль, как у .NET.
    if scale > 28 + 40 {
        return Ok(Decimal { mantissa: 0, scale: MAX_SCALE, negative });
    }
    let mut result = fit(value, scale as u32, negative).map_err(|_| ParseError::Overflow)?;
    // Отброшенный за 40 цифрами хвост нужен только для ровно половины.
    let _ = sticky;
    if result.mantissa == 0 && significant.is_empty() {
        result.scale = result.scale.min(MAX_SCALE);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(mantissa: u128, scale: u32) -> Decimal {
        Decimal { mantissa, scale, negative: false }
    }

    #[test]
    fn division_matches_dotnet() {
        assert_eq!(div(d(1, 0), d(3, 0)).unwrap(), d(3_333_333_333_333_333_333_333_333_333, 28));
        assert_eq!(div(d(10, 0), d(4, 0)).unwrap(), d(25, 1));
        assert_eq!(div(d(225, 2), d(11, 1)).unwrap(), d(20_454_545_454_545_454_545_454_545_455, 28));
    }

    #[test]
    fn rounding_to_even() {
        assert_eq!(round(d(225, 2), 1, 0), d(22, 1));
        assert_eq!(round(d(235, 2), 1, 0), d(24, 1));
        assert_eq!(round(Decimal { mantissa: 27, scale: 1, negative: true }, 0, 3), Decimal { mantissa: 3, scale: 0, negative: true });
    }

    #[test]
    fn floats_keep_fifteen_digits() {
        assert_eq!(from_float(0.1, false).unwrap(), d(1, 1));
        assert_eq!(from_float(1.5, true).unwrap(), d(15, 1));
    }
}
