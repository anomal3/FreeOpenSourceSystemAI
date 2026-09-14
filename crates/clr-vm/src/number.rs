//! Печать и разбор чисел так, как это делает .NET (фаза N4c).
//!
//! # Откуда правила
//!
//! Здесь повторён `System.Number` из CoreCLR с инвариантной культурой: целое
//! или дробное сначала становится записью «десятичные цифры, порядок, знак»
//! (`NumberBuffer` у .NET — [`Number`] здесь), а запись печатается по
//! стандартному формату (`F2`, `N`, `E3`, `P`, `G`) или по пользовательскому
//! (`#,##0.00`, `0.###E+0`, секции через `;`). Имена функций совпадают с
//! именами у .NET, чтобы расхождение можно было найти по исходнику CoreCLR.
//!
//! Проверяется не глазами: `cargo xtask clr-check` печатает десятки тысяч
//! случайных чисел по сотне форматов настоящим `dotnet` (`tools/dotnet/numcheck`)
//! и этим модулем и требует совпадения до символа.
//!
//! # Цифры дробного числа
//!
//! У .NET их даёт Grisu3 с запасным Dragon4 на больших целых. Здесь то, что
//! они обязаны дать, получено прямо:
//!
//! - **кратчайшая запись**, читаемая обратно в то же число (`ToString()`,
//!   `R`, `G`), — от `core::fmt`: Rust печатает дробные тем же правилом
//!   «кратчайшая, а из кратчайших — ближайшая»;
//! - **заданное число цифр** (`F2`, `E5`, `G20`) — точное десятичное
//!   разложение двоичного числа (оно всегда конечно: `m·2^e` при `e < 0` — это
//!   `m·5^-e / 10^-e`), округлённое по отброшенному хвосту. Ровно половина
//!   округляется к чётной цифре: `2.5.ToString("F0")` у .NET — `2`, а
//!   `0.125.ToString("F2")` — `0.12`.
//!
//! Пользовательский формат у .NET округляет дважды: сначала до 15 значащих
//! цифр (у `float` — до 7) к чётной, потом до места в формате — половину
//! вверх. Поэтому `2.675.ToString("0.00")` — `2.68`, хотя `F2` даёт `2.67`.
//! Это повторено.

use alloc::vec::Vec;
use core::fmt::Write as _;

/// Строка формата неверна — у .NET `FormatException`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    Bad,
    /// Формат требует строки длиннее, чем разумно выделять (`F999999999`).
    /// У .NET это `OutOfMemoryException` или строка на гигабайт.
    TooLong,
}

/// Не число, которое даёт разбор у .NET: `double.NaN` — со знаковым битом.
const NAN: f64 = f64::from_bits(0xFFF8_0000_0000_0000);

/// Больше нулей по формату не дописывается — [`FormatError::TooLong`].
const MAX_PADDING: i32 = 1 << 20;

/// Десятичная запись числа: `0.d₁d₂… × 10^scale`. Цифры — ASCII, без
/// завершающего нуля (у .NET он есть, здесь его заменяет [`Number::digit`]).
struct Number {
    digits: Vec<u8>,
    scale: i32,
    negative: bool,
    /// Дробное число: у него есть `-0`, и цифры уже округлены верно.
    floating: bool,
}

impl Number {
    /// Цифра по номеру или `0` за концом — как чтение за `'\0'` у .NET.
    fn digit(&self, index: usize) -> u8 {
        self.digits.get(index).copied().unwrap_or(0)
    }
}

// ------------------------------------------------------------------------
// Целые
// ------------------------------------------------------------------------

/// Целое по формату .NET. `bits` — ширина типа: `X` и `B` печатают
/// отрицательное в дополнительном коде этой ширины (`(-1).ToString("X")` у
/// `int` — `FFFFFFFF`, у `sbyte` — `FF`).
pub fn format_integer(value: i128, bits: u32, format: &[u16]) -> Result<Vec<u16>, FormatError> {
    let mut out = Vec::new();
    if format.is_empty() {
        push_str(&mut out, &decimal(value.unsigned_abs(), 0, value < 0));
        return Ok(out);
    }
    let (fmt, digits) = parse_format_specifier(format)?;
    let upper = fmt & 0xFFDF;
    if (upper == u16::from(b'G') && digits < 1) || upper == u16::from(b'D') {
        check_padding(digits)?;
        push_str(&mut out, &decimal(value.unsigned_abs(), digits, value < 0));
    } else if upper == u16::from(b'X') || upper == u16::from(b'B') {
        check_padding(digits)?;
        let masked = (value as u128) & (u128::MAX >> (128 - bits));
        let width = usize::try_from(digits.max(0)).unwrap_or(0);
        let mut text = alloc::string::String::new();
        let _ = match fmt as u8 {
            b'X' => write!(text, "{masked:0width$X}"),
            b'x' => write!(text, "{masked:0width$x}"),
            _ => write!(text, "{masked:0width$b}"),
        };
        push_str(&mut out, &text);
    } else {
        let magnitude = value.unsigned_abs();
        let mut number = Number {
            digits: if magnitude == 0 { Vec::new() } else { alloc::format!("{magnitude}").into_bytes() },
            scale: 0,
            negative: value < 0,
            floating: false,
        };
        number.scale = number.digits.len() as i32;
        if fmt != 0 {
            number_to_string(&mut out, &mut number, fmt, digits)?;
        } else {
            number_to_string_format(&mut out, &mut number, format);
        }
    }
    Ok(out)
}

/// Десятичные цифры с нулями до `digits` и минусом впереди.
fn decimal(magnitude: u128, digits: i32, negative: bool) -> alloc::string::String {
    let width = usize::try_from(digits.max(if negative { 1 } else { 0 })).unwrap_or(0);
    let sign = if negative { "-" } else { "" };
    alloc::format!("{sign}{magnitude:0width$}")
}

fn check_padding(digits: i32) -> Result<(), FormatError> {
    if digits > MAX_PADDING { Err(FormatError::TooLong) } else { Ok(()) }
}

// ------------------------------------------------------------------------
// Дробные
// ------------------------------------------------------------------------

/// `double` или `float` — у них разные «кратчайшая запись» и точность
/// пользовательского формата.
#[derive(Clone, Copy)]
enum Float {
    Double(f64),
    Single(f32),
}

impl Float {
    fn is_nan(self) -> bool {
        match self {
            Self::Double(x) => x.is_nan(),
            Self::Single(x) => x.is_nan(),
        }
    }

    fn is_infinite(self) -> bool {
        match self {
            Self::Double(x) => x.is_infinite(),
            Self::Single(x) => x.is_infinite(),
        }
    }

    fn is_negative(self) -> bool {
        match self {
            Self::Double(x) => x.is_sign_negative(),
            Self::Single(x) => x.is_sign_negative(),
        }
    }

    fn is_zero(self) -> bool {
        match self {
            Self::Double(x) => x == 0.0,
            Self::Single(x) => x == 0.0,
        }
    }

    /// Цифр, которых всегда хватает на запись, читаемую обратно.
    fn round_trip_digits(self) -> i32 {
        match self {
            Self::Double(_) => 17,
            Self::Single(_) => 9,
        }
    }

    /// Значащих цифр в пользовательском формате.
    fn custom_precision(self) -> i32 {
        match self {
            Self::Double(_) => 15,
            Self::Single(_) => 7,
        }
    }

    /// Модуль как `мантисса · 2^порядок`.
    fn parts(self) -> (u64, i32) {
        match self {
            Self::Double(x) => {
                let bits = x.to_bits();
                let fraction = bits & ((1 << 52) - 1);
                let exponent = ((bits >> 52) & 0x7FF) as i32;
                if exponent == 0 { (fraction, -1074) } else { (fraction | (1 << 52), exponent - 1075) }
            }
            Self::Single(x) => {
                let bits = x.to_bits();
                let fraction = u64::from(bits & ((1 << 23) - 1));
                let exponent = ((bits >> 23) & 0xFF) as i32;
                if exponent == 0 { (fraction, -149) } else { (fraction | (1 << 23), exponent - 150) }
            }
        }
    }

    /// Кратчайшие цифры модуля: `1.2345e-5` от `core::fmt` становится
    /// цифрами `12345` и порядком `-4`.
    fn shortest(self) -> (Vec<u8>, i32) {
        let mut text = alloc::string::String::new();
        let _ = match self {
            Self::Double(x) => write!(text, "{:e}", if x < 0.0 { -x } else { x }),
            Self::Single(x) => write!(text, "{:e}", if x < 0.0 { -x } else { x }),
        };
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let digits: Vec<u8> = mantissa.bytes().filter(u8::is_ascii_digit).collect();
        let exponent: i32 = exponent.parse().unwrap_or(0);
        let mut number = Number { digits, scale: exponent + 1, negative: false, floating: true };
        self.break_tie(&mut number);
        (number.digits, number.scale)
    }

    /// Две кратчайшие записи на равном расстоянии от числа: Rust берёт
    /// большую, Dragon4 у .NET — с чётной последней цифрой (`-2823557.25f`
    /// у .NET печатается `-2823557.2`). Так бывает, только когда точная запись
    /// на одну цифру длиннее кратчайшей и кончается пятёркой.
    fn break_tie(self, number: &mut Number) {
        let (mantissa, exponent) = self.parts();
        // У `m·2^e` с нечётным `m` и `e < 0` ровно `-e` цифр после точки, а
        // кратчайшая запись не длиннее 17 цифр: при `-e > 20` ничьей нет, и
        // точное разложение не нужно.
        if exponent + (mantissa.trailing_zeros() as i32) < -20 {
            return;
        }
        let (exact, scale) = exact_digits(mantissa, exponent);
        let count = number.digits.len();
        if exact.len() != count + 1 || exact[count] != b'5' {
            return;
        }
        let mut even = Number { digits: exact, scale, negative: false, floating: true };
        round_exact(&mut even, count as i32);
        // Чётная запись у границы порядка может не читаться обратно в то же
        // число — тогда Dragon4 её тоже не выбрал бы.
        if even.digits != number.digits && self.reads_back(&even) {
            *number = even;
        }
    }

    fn reads_back(self, number: &Number) -> bool {
        let mut text = alloc::string::String::from("0.");
        text.push_str(core::str::from_utf8(&number.digits).unwrap_or("0"));
        let _ = write!(text, "e{}", number.scale);
        match self {
            Self::Double(x) => text.parse::<f64>().is_ok_and(|y| y == if x < 0.0 { -x } else { x }),
            Self::Single(x) => text.parse::<f32>().is_ok_and(|y| y == if x < 0.0 { -x } else { x }),
        }
    }
}

pub fn format_double(value: f64, format: &[u16]) -> Result<Vec<u16>, FormatError> {
    format_float(Float::Double(value), format)
}

pub fn format_single(value: f32, format: &[u16]) -> Result<Vec<u16>, FormatError> {
    format_float(Float::Single(value), format)
}

/// `Number.FormatFloat` у .NET.
fn format_float(value: Float, format: &[u16]) -> Result<Vec<u16>, FormatError> {
    let mut out = Vec::new();
    // Не число и бесконечность печатаются словом при любом формате, даже
    // неверном.
    if value.is_nan() {
        push_str(&mut out, "NaN");
        return Ok(out);
    }
    if value.is_infinite() {
        push_str(&mut out, if value.is_negative() { "-Infinity" } else { "Infinity" });
        return Ok(out);
    }
    let (fmt, mut precision) = parse_format_specifier(format)?;
    if fmt == 0 {
        precision = value.custom_precision();
    }
    let mut number = Number { digits: Vec::new(), scale: 0, negative: value.is_negative(), floating: true };
    let (max_digits, significant) = max_digits_and_precision(fmt, &mut precision)?;
    if !value.is_zero() {
        if precision == -1 {
            (number.digits, number.scale) = value.shortest();
        } else {
            let (mantissa, exponent) = value.parts();
            let (digits, scale) = exact_digits(mantissa, exponent);
            number.digits = digits;
            number.scale = scale;
            let keep = if significant { precision } else { number.scale.saturating_add(precision) };
            round_exact(&mut number, keep);
        }
    }
    if fmt != 0 {
        let max = if precision == -1 { (number.digits.len() as i32).max(value.round_trip_digits()) } else { max_digits };
        number_to_string(&mut out, &mut number, fmt, max)?;
    } else {
        number_to_string_format(&mut out, &mut number, format);
    }
    Ok(out)
}

/// `GetFloatingPointMaxDigitsAndPrecision`: сколько цифр печатать и считать ли
/// точность значащими цифрами (иначе — цифрами после точки).
fn max_digits_and_precision(fmt: u16, precision: &mut i32) -> Result<(i32, bool), FormatError> {
    if fmt == 0 {
        return Ok((*precision, true));
    }
    let max = *precision;
    let significant = match fmt as u8 {
        b'C' | b'c' | b'F' | b'f' | b'N' | b'n' => {
            if *precision == -1 {
                *precision = 2;
            }
            false
        }
        b'E' | b'e' => {
            if *precision == -1 {
                *precision = 6;
            }
            *precision += 1;
            true
        }
        b'G' | b'g' => {
            if *precision == 0 {
                *precision = -1;
            }
            true
        }
        b'P' | b'p' => {
            if *precision == -1 {
                *precision = 2;
            }
            *precision += 2;
            false
        }
        b'R' | b'r' => {
            *precision = -1;
            true
        }
        _ => return Err(FormatError::Bad),
    };
    Ok((max, significant))
}

/// Все десятичные цифры `mantissa · 2^exponent` (`mantissa > 0`) без
/// завершающих нулей и порядок первой.
fn exact_digits(mantissa: u64, exponent: i32) -> (Vec<u8>, i32) {
    let mut big = Big::from_u64(mantissa);
    let fraction_digits = if exponent >= 0 {
        big.shift_left(exponent as u32);
        0
    } else {
        let mut power = exponent.unsigned_abs();
        // 5^13 — наибольшая степень пятёрки, помещающаяся в u32.
        while power >= 13 {
            big.mul_small(1_220_703_125);
            power -= 13;
        }
        big.mul_small(5u32.pow(power));
        exponent.unsigned_abs() as i32
    };
    let mut digits = big.to_decimal();
    let scale = digits.len() as i32 - fraction_digits;
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    (digits, scale)
}

/// Оставить `keep` первых цифр, округлив по хвосту; ровно половина — к чётной.
fn round_exact(number: &mut Number, keep: i32) {
    let len = number.digits.len();
    let Ok(keep) = usize::try_from(keep) else {
        number.digits.clear();
        number.scale = 0;
        return;
    };
    if keep >= len {
        return;
    }
    let first = number.digits[keep];
    let odd = keep > 0 && (number.digits[keep - 1] - b'0') % 2 == 1;
    // Завершающих нулей нет, так что любая цифра после `first` не ноль.
    let up = first > b'5' || (first == b'5' && (len > keep + 1 || odd));
    number.digits.truncate(keep);
    if up {
        let mut i = keep;
        while i > 0 && number.digits[i - 1] == b'9' {
            i -= 1;
        }
        if i == 0 {
            number.digits.clear();
            number.digits.push(b'1');
            number.scale += 1;
        } else {
            number.digits[i - 1] += 1;
            number.digits.truncate(i);
        }
    }
    while number.digits.last() == Some(&b'0') {
        number.digits.pop();
    }
    if number.digits.is_empty() {
        number.scale = 0;
    }
}

/// Неотрицательное длинное целое: слова по 32 бита, младшее первым. Нужно
/// только на точное разложение — умножить, сдвинуть, перевести в десятичные.
struct Big(Vec<u32>);

impl Big {
    fn from_u64(value: u64) -> Self {
        Self(alloc::vec![value as u32, (value >> 32) as u32])
    }

    fn mul_small(&mut self, factor: u32) {
        let mut carry = 0u64;
        for word in &mut self.0 {
            let product = u64::from(*word) * u64::from(factor) + carry;
            *word = product as u32;
            carry = product >> 32;
        }
        if carry != 0 {
            self.0.push(carry as u32);
        }
    }

    fn shift_left(&mut self, bits: u32) {
        let words = (bits / 32) as usize;
        let bits = bits % 32;
        if bits != 0 {
            let mut carry = 0u32;
            for word in &mut self.0 {
                let shifted = (*word << bits) | carry;
                carry = *word >> (32 - bits);
                *word = shifted;
            }
            if carry != 0 {
                self.0.push(carry);
            }
        }
        if words != 0 {
            self.0.splice(0..0, core::iter::repeat_n(0, words));
        }
    }

    /// Остаток от деления на `divisor`; частное остаётся на месте.
    fn div_small(&mut self, divisor: u32) -> u32 {
        let mut remainder = 0u64;
        for word in self.0.iter_mut().rev() {
            let current = (remainder << 32) | u64::from(*word);
            *word = (current / u64::from(divisor)) as u32;
            remainder = current % u64::from(divisor);
        }
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        remainder as u32
    }

    fn to_decimal(mut self) -> Vec<u8> {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
        // Кусками по девять цифр с младшего конца.
        let mut chunks = Vec::new();
        while !self.0.is_empty() {
            chunks.push(self.div_small(1_000_000_000));
        }
        let mut digits = Vec::new();
        for (index, chunk) in chunks.iter().rev().enumerate() {
            let text = if index == 0 { alloc::format!("{chunk}") } else { alloc::format!("{chunk:09}") };
            digits.extend_from_slice(text.as_bytes());
        }
        digits
    }
}

// ------------------------------------------------------------------------
// Стандартные форматы
// ------------------------------------------------------------------------

/// `ParseFormatSpecifier`: буква и число после неё (`-1` — числа нет). Буква
/// `0` — формат пользовательский. Пустой формат — `G`.
fn parse_format_specifier(format: &[u16]) -> Result<(u16, i32), FormatError> {
    let Some(&c) = format.first() else { return Ok((u16::from(b'G'), -1)) };
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&(c & 0xFFDF)) {
        let mut n: i32 = 0;
        let mut i = 1;
        while i < format.len() && (u16::from(b'0')..=u16::from(b'9')).contains(&format[i]) {
            // Больше девяти цифр в числе быть не может.
            if n >= 100_000_000 {
                return Err(FormatError::Bad);
            }
            n = n * 10 + i32::from(format[i] - u16::from(b'0'));
            i += 1;
        }
        if i >= format.len() || format[i] == 0 {
            return Ok((c, if i == 1 { -1 } else { n }));
        }
    }
    // `'\0'` у .NET — конец строки формата, даже если за ним что-то есть.
    Ok((if c == 0 { u16::from(b'G') } else { 0 }, -1))
}

/// `NumberToString`: запись по стандартному формату.
fn number_to_string(out: &mut Vec<u16>, number: &mut Number, fmt: u16, mut max: i32) -> Result<(), FormatError> {
    let correct = number.floating;
    check_padding(max)?;
    match fmt as u8 {
        b'C' | b'c' => {
            if max < 0 {
                max = 2;
            }
            round_number(number, number.scale.saturating_add(max), correct);
            let pattern = if number.negative { "($#)" } else { "$#" };
            for ch in pattern.bytes() {
                match ch {
                    b'#' => format_fixed(out, number, max, true),
                    b'$' => out.push(0xA4),
                    other => out.push(u16::from(other)),
                }
            }
        }
        b'F' | b'f' => {
            if max < 0 {
                max = 2;
            }
            round_number(number, number.scale.saturating_add(max), correct);
            if number.negative {
                out.push(u16::from(b'-'));
            }
            format_fixed(out, number, max, false);
        }
        b'N' | b'n' => {
            if max < 0 {
                max = 2;
            }
            round_number(number, number.scale.saturating_add(max), correct);
            if number.negative {
                out.push(u16::from(b'-'));
            }
            format_fixed(out, number, max, true);
        }
        b'E' | b'e' => {
            if max < 0 {
                max = 6;
            }
            max += 1;
            round_number(number, max, correct);
            if number.negative {
                out.push(u16::from(b'-'));
            }
            format_scientific(out, number, max, fmt);
        }
        b'G' | b'g' | b'R' | b'r' => {
            // `R` у целого — это `G`.
            let general = if (fmt & 0xFFDF) == u16::from(b'R') { fmt - u16::from(b'R' - b'G') } else { fmt };
            if max < 1 {
                max = number.digits.len() as i32;
            }
            round_number(number, max, correct);
            if number.negative {
                out.push(u16::from(b'-'));
            }
            format_general(out, number, max, general - u16::from(b'G' - b'E'));
        }
        b'P' | b'p' => {
            if max < 0 {
                max = 2;
            }
            number.scale = number.scale.saturating_add(2);
            round_number(number, number.scale.saturating_add(max), correct);
            let pattern = if number.negative { "-# %" } else { "# %" };
            for ch in pattern.bytes() {
                match ch {
                    b'#' => format_fixed(out, number, max, true),
                    b'-' => out.push(u16::from(b'-')),
                    other => out.push(u16::from(other)),
                }
            }
        }
        _ => return Err(FormatError::Bad),
    }
    Ok(())
}

/// `RoundNumber`: оставить `pos` цифр. Верно округлённые цифры дробного
/// только обрезаются, остальные округляются половиной вверх.
fn round_number(number: &mut Number, pos: i32, correctly_rounded: bool) {
    let mut i = 0usize;
    while (i as i64) < i64::from(pos) && number.digit(i) != 0 {
        i += 1;
    }
    if i as i64 == i64::from(pos) && !correctly_rounded && number.digit(i) >= b'5' {
        while i > 0 && number.digits[i - 1] == b'9' {
            i -= 1;
        }
        if i > 0 {
            number.digits[i - 1] += 1;
        } else {
            number.scale += 1;
            number.digits[0] = b'1';
            i = 1;
        }
    } else {
        while i > 0 && number.digits[i - 1] == b'0' {
            i -= 1;
        }
    }
    if i == 0 {
        // У целого нет `-0`; у дробного есть.
        if !number.floating {
            number.negative = false;
        }
        number.scale = 0;
    }
    number.digits.truncate(i);
}

/// `FormatFixed`: целая часть (с разделителем разрядов по три) и `max` цифр
/// после точки.
fn format_fixed(out: &mut Vec<u16>, number: &Number, max: i32, grouped: bool) {
    let mut dig = 0usize;
    let mut dig_pos = number.scale;
    if dig_pos > 0 {
        let count = dig_pos as usize;
        let start = out.len();
        for i in 0..count {
            out.push(u16::from(if i < number.digits.len() { number.digits[i] } else { b'0' }));
        }
        if grouped {
            let mut at = count;
            while at > 3 {
                at -= 3;
                out.insert(start + at, u16::from(b','));
            }
        }
        dig = count.min(number.digits.len());
    } else {
        out.push(u16::from(b'0'));
    }
    let mut max = max;
    if max > 0 {
        out.push(u16::from(b'.'));
        if dig_pos < 0 {
            let zeroes = (-dig_pos).min(max);
            for _ in 0..zeroes {
                out.push(u16::from(b'0'));
            }
            dig_pos += zeroes;
            max -= zeroes;
        }
        while max > 0 {
            let d = number.digit(dig);
            if d != 0 {
                out.push(u16::from(d));
                dig += 1;
            } else {
                out.push(u16::from(b'0'));
            }
            max -= 1;
        }
    }
    let _ = dig_pos;
}

/// `FormatScientific`: `d.ddddE+ddd`.
fn format_scientific(out: &mut Vec<u16>, number: &Number, max: i32, exp_char: u16) {
    let mut dig = 0usize;
    let mut next = |out: &mut Vec<u16>| {
        let d = number.digit(dig);
        if d != 0 {
            out.push(u16::from(d));
            dig += 1;
        } else {
            out.push(u16::from(b'0'));
        }
    };
    next(out);
    if max != 1 {
        out.push(u16::from(b'.'));
    }
    let mut left = max;
    loop {
        left -= 1;
        if left <= 0 {
            break;
        }
        next(out);
    }
    let exponent = if number.digit(0) == 0 { 0 } else { number.scale - 1 };
    format_exponent(out, exponent, exp_char, 3, true);
}

/// `FormatGeneral`: как `F`, но с переходом на экспоненту, когда порядок
/// больше `max` или число меньше `0.0001`.
fn format_general(out: &mut Vec<u16>, number: &Number, max: i32, exp_char: u16) {
    let mut dig_pos = number.scale;
    let mut scientific = false;
    if dig_pos > max || dig_pos < -3 {
        dig_pos = 1;
        scientific = true;
    }
    let mut dig = 0usize;
    if dig_pos > 0 {
        loop {
            let d = number.digit(dig);
            if d != 0 {
                out.push(u16::from(d));
                dig += 1;
            } else {
                out.push(u16::from(b'0'));
            }
            dig_pos -= 1;
            if dig_pos <= 0 {
                break;
            }
        }
    } else {
        out.push(u16::from(b'0'));
    }
    if number.digit(dig) != 0 || dig_pos < 0 {
        out.push(u16::from(b'.'));
        while dig_pos < 0 {
            out.push(u16::from(b'0'));
            dig_pos += 1;
        }
        while number.digit(dig) != 0 {
            out.push(u16::from(number.digit(dig)));
            dig += 1;
        }
    }
    if scientific {
        format_exponent(out, number.scale - 1, exp_char, 2, true);
    }
}

/// `FormatExponent`: буква, знак и не меньше `min_digits` цифр.
fn format_exponent(out: &mut Vec<u16>, value: i32, exp_char: u16, min_digits: usize, positive_sign: bool) {
    out.push(exp_char);
    if value < 0 {
        out.push(u16::from(b'-'));
    } else if positive_sign {
        out.push(u16::from(b'+'));
    }
    push_str(out, &alloc::format!("{:0min_digits$}", value.unsigned_abs()));
}

fn push_str(out: &mut Vec<u16>, text: &str) {
    out.extend(text.encode_utf16());
}

// ------------------------------------------------------------------------
// Пользовательский формат
// ------------------------------------------------------------------------

const ZERO: u16 = b'0' as u16;
const HASH: u16 = b'#' as u16;
const DOT: u16 = b'.' as u16;
const COMMA: u16 = b',' as u16;
const PERCENT: u16 = b'%' as u16;
const PER_MILLE: u16 = 0x2030;
const QUOTE: u16 = b'\'' as u16;
const DOUBLE_QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const SEMICOLON: u16 = b';' as u16;
const PLUS: u16 = b'+' as u16;
const MINUS: u16 = b'-' as u16;

/// `FindSection`: начало секции номер `section` (0 — положительные,
/// 1 — отрицательные, 2 — ноль); пустая или отсутствующая секция — первая.
fn find_section(format: &[u16], section: usize) -> usize {
    if section == 0 {
        return 0;
    }
    let mut section = section;
    let mut src = 0;
    loop {
        if src >= format.len() {
            return 0;
        }
        let ch = format[src];
        src += 1;
        match ch {
            QUOTE | DOUBLE_QUOTE => {
                while src < format.len() && format[src] != 0 {
                    let c = format[src];
                    src += 1;
                    if c == ch {
                        break;
                    }
                }
            }
            BACKSLASH => {
                if src < format.len() && format[src] != 0 {
                    src += 1;
                }
            }
            SEMICOLON => {
                section -= 1;
                if section != 0 {
                    continue;
                }
                if src < format.len() && format[src] != 0 && format[src] != SEMICOLON {
                    return src;
                }
                return 0;
            }
            0 => return 0,
            _ => {}
        }
    }
}

/// `NumberToStringFormat`: запись по пользовательскому формату.
#[allow(clippy::too_many_lines)]
fn number_to_string_format(out: &mut Vec<u16>, number: &mut Number, format: &[u16]) {
    let at = |i: usize| format.get(i).copied().unwrap_or(0);
    let mut section = find_section(format, if number.digit(0) == 0 { 2 } else if number.negative { 1 } else { 0 });

    let mut digit_count;
    let mut decimal_pos;
    let mut first_digit;
    let mut last_digit;
    let mut scientific;
    let mut thousand_pos;
    let mut thousand_count = 0;
    let mut thousand_seps;
    let mut scale_adjust;
    let mut src;

    loop {
        digit_count = 0i32;
        decimal_pos = -1i32;
        first_digit = 0x7FFF_FFFFi32;
        last_digit = 0i32;
        scientific = false;
        thousand_pos = -1i32;
        thousand_seps = false;
        scale_adjust = 0i32;
        src = section;

        while src < format.len() && at(src) != 0 && at(src) != SEMICOLON {
            let ch = at(src);
            src += 1;
            match ch {
                HASH => digit_count += 1,
                ZERO => {
                    if first_digit == 0x7FFF_FFFF {
                        first_digit = digit_count;
                    }
                    digit_count += 1;
                    last_digit = digit_count;
                }
                DOT => {
                    if decimal_pos < 0 {
                        decimal_pos = digit_count;
                    }
                }
                COMMA => {
                    if digit_count > 0 && decimal_pos < 0 {
                        if thousand_pos >= 0 {
                            if thousand_pos == digit_count {
                                thousand_count += 1;
                                continue;
                            }
                            thousand_seps = true;
                        }
                        thousand_pos = digit_count;
                        thousand_count = 1;
                    }
                }
                PERCENT => scale_adjust += 2,
                PER_MILLE => scale_adjust += 3,
                QUOTE | DOUBLE_QUOTE => {
                    while src < format.len() && at(src) != 0 {
                        let c = at(src);
                        src += 1;
                        if c == ch {
                            break;
                        }
                    }
                }
                BACKSLASH => {
                    if src < format.len() && at(src) != 0 {
                        src += 1;
                    }
                }
                0x45 | 0x65 => {
                    if (src < format.len() && at(src) == ZERO)
                        || (src + 1 < format.len() && (at(src) == PLUS || at(src) == MINUS) && at(src + 1) == ZERO)
                    {
                        loop {
                            src += 1;
                            if !(src < format.len() && at(src) == ZERO) {
                                break;
                            }
                        }
                        scientific = true;
                    }
                }
                _ => {}
            }
        }

        if decimal_pos < 0 {
            decimal_pos = digit_count;
        }
        if thousand_pos >= 0 {
            if thousand_pos == decimal_pos {
                scale_adjust -= thousand_count * 3;
            } else {
                thousand_seps = true;
            }
        }

        if number.digit(0) != 0 {
            number.scale = number.scale.saturating_add(scale_adjust);
            let pos = if scientific { digit_count } else { number.scale.saturating_add(digit_count - decimal_pos) };
            round_number(number, pos, false);
            if number.digit(0) == 0 {
                let zero_section = find_section(format, 2);
                if zero_section != section {
                    section = zero_section;
                    continue;
                }
            }
        } else {
            if !number.floating {
                number.negative = false;
            }
            number.scale = 0;
        }
        break;
    }

    first_digit = if first_digit < decimal_pos { decimal_pos - first_digit } else { 0 };
    last_digit = if last_digit > decimal_pos { decimal_pos - last_digit } else { 0 };
    let mut dig_pos;
    let mut adjust;
    if scientific {
        dig_pos = decimal_pos;
        adjust = 0;
    } else {
        dig_pos = number.scale.max(decimal_pos);
        adjust = number.scale - decimal_pos;
    }
    src = section;

    // Где ставить разделители разрядов: номера позиций от точки, после
    // которых он идёт. Группы по три, как у инвариантной культуры.
    let mut separators: Vec<i32> = Vec::new();
    if thousand_seps {
        let total_digits = dig_pos + if adjust < 0 { adjust } else { 0 };
        let num_digits = first_digit.max(total_digits);
        let mut group_total = 3;
        while num_digits > group_total {
            separators.push(group_total);
            group_total += 3;
        }
    }
    let mut separator = separators.len() as isize - 1;

    if number.negative && section == 0 && number.scale != 0 {
        out.push(MINUS);
    }

    let mut decimal_written = false;
    let mut cur = 0usize;
    let start = out.len();

    let group = |out: &mut Vec<u16>, dig_pos: i32, separator: &mut isize| {
        if thousand_seps && dig_pos > 1 && *separator >= 0 && dig_pos == separators[*separator as usize] + 1 {
            out.push(COMMA);
            *separator -= 1;
        }
    };

    while src < format.len() && at(src) != 0 && at(src) != SEMICOLON {
        let ch = at(src);
        src += 1;

        if adjust > 0 && matches!(ch, HASH | ZERO | DOT) {
            while adjust > 0 {
                let d = number.digit(cur);
                if d != 0 {
                    out.push(u16::from(d));
                    cur += 1;
                } else {
                    out.push(ZERO);
                }
                group(out, dig_pos, &mut separator);
                dig_pos -= 1;
                adjust -= 1;
            }
        }

        match ch {
            HASH | ZERO => {
                let printed = if adjust < 0 {
                    adjust += 1;
                    if dig_pos <= first_digit { ZERO } else { 0 }
                } else {
                    let d = number.digit(cur);
                    if d != 0 {
                        cur += 1;
                        u16::from(d)
                    } else if dig_pos > last_digit {
                        ZERO
                    } else {
                        0
                    }
                };
                if printed != 0 {
                    out.push(printed);
                    group(out, dig_pos, &mut separator);
                }
                dig_pos -= 1;
            }
            DOT => {
                if dig_pos != 0 || decimal_written {
                    continue;
                }
                if last_digit < 0 || (decimal_pos < digit_count && number.digit(cur) != 0) {
                    out.push(DOT);
                    decimal_written = true;
                }
            }
            PER_MILLE => out.push(PER_MILLE),
            PERCENT => out.push(PERCENT),
            COMMA => {}
            QUOTE | DOUBLE_QUOTE => {
                while src < format.len() && at(src) != 0 && at(src) != ch {
                    out.push(at(src));
                    src += 1;
                }
                if src < format.len() && at(src) != 0 {
                    src += 1;
                }
            }
            BACKSLASH => {
                if src < format.len() && at(src) != 0 {
                    out.push(at(src));
                    src += 1;
                }
            }
            0x45 | 0x65 => {
                let mut positive_sign = false;
                let mut i = 0usize;
                if scientific {
                    if src < format.len() && at(src) == ZERO {
                        i += 1;
                    } else if src + 1 < format.len() && at(src) == PLUS && at(src + 1) == ZERO {
                        positive_sign = true;
                    } else if src + 1 < format.len() && at(src) == MINUS && at(src + 1) == ZERO {
                    } else {
                        out.push(ch);
                        continue;
                    }
                    loop {
                        src += 1;
                        if !(src < format.len() && at(src) == ZERO) {
                            break;
                        }
                        i += 1;
                    }
                    let i = i.min(10);
                    let exponent = if number.digit(0) == 0 { 0 } else { number.scale - decimal_pos };
                    format_exponent(out, exponent, ch, i, positive_sign);
                    scientific = false;
                } else {
                    out.push(ch);
                    if src < format.len() {
                        if at(src) == PLUS || at(src) == MINUS {
                            out.push(at(src));
                            src += 1;
                        }
                        while src < format.len() && at(src) == ZERO {
                            out.push(at(src));
                            src += 1;
                        }
                    }
                }
            }
            other => out.push(other),
        }
    }

    if number.negative && section == 0 && number.scale == 0 && out.len() > start {
        out.insert(start, MINUS);
    }
}

// ------------------------------------------------------------------------
// Разбор
// ------------------------------------------------------------------------

/// `double.Parse`/`float.Parse` с инвариантной культурой (`NumberStyles.Float |
/// AllowThousands`): пробелы по краям, знак, разделители разрядов в целой
/// части, точка, экспонента; `NaN`, `Infinity`, `-Infinity` без учёта
/// регистра. Слишком большое — бесконечность, а не ошибка, как у .NET с 3.0.
pub fn parse_float(text: &[u16], single: bool) -> Option<f64> {
    if let Some((negative, digits, scale)) = parse_number(text) {
        let magnitude = if digits.is_empty() {
            0.0
        } else {
            // Порядок за ±1000 уже ноль или бесконечность при любых цифрах:
            // значение — `0.ddd × 10^scale`, а `0.ddd < 1`.
            let scale = scale.clamp(-100_000, 100_000);
            let mut decimal = alloc::string::String::with_capacity(digits.len() + 16);
            decimal.push_str("0.");
            decimal.push_str(core::str::from_utf8(&digits).ok()?);
            let _ = write!(decimal, "e{scale}");
            if single { f64::from(decimal.parse::<f32>().ok()?) } else { decimal.parse::<f64>().ok()? }
        };
        return Some(if negative { -magnitude } else { magnitude });
    }
    let trimmed = trim(text);
    let equals = |s: &[u16], word: &str| s.len() == word.len() && s.iter().zip(word.bytes()).all(|(&c, w)| c < 0x80 && (c as u8).eq_ignore_ascii_case(&w));
    if equals(trimmed, "Infinity") {
        Some(f64::INFINITY)
    } else if equals(trimmed, "-Infinity") {
        Some(f64::NEG_INFINITY)
    } else if equals(trimmed, "NaN") {
        Some(NAN)
    } else if let Some(rest) = trimmed.strip_prefix(&[PLUS]) {
        if equals(rest, "Infinity") {
            Some(f64::INFINITY)
        } else if equals(rest, "NaN") {
            Some(NAN)
        } else {
            None
        }
    } else if let Some(rest) = trimmed.strip_prefix(&[MINUS]) {
        equals(rest, "NaN").then_some(NAN)
    } else {
        None
    }
}

/// Пробельные по краям — как `MemoryExtensions.Trim` (`char.IsWhiteSpace`).
fn trim(text: &[u16]) -> &[u16] {
    let white = |c: &u16| char::from_u32(u32::from(*c)).is_some_and(char::is_whitespace);
    let start = text.iter().position(|c| !white(c)).unwrap_or(text.len());
    let end = text.iter().rposition(|c| !white(c)).map_or(start, |i| i + 1);
    &text[start..end]
}

/// `TryParseNumber` для дробного: знак, значащие цифры без ведущих нулей и
/// порядок. `None` — строка не число целиком.
fn parse_number(text: &[u16]) -> Option<(bool, Vec<u8>, i64)> {
    const DIGITS: u8 = 1;
    const NON_ZERO: u8 = 2;
    const DECIMAL: u8 = 4;
    const SIGN: u8 = 8;
    let is_white = |c: u16| c == 0x20 || (0x09..=0x0D).contains(&c);
    let is_digit = |c: u16| (u16::from(b'0')..=u16::from(b'9')).contains(&c);
    let at = |p: usize| text.get(p).copied().unwrap_or(0);

    let mut state = 0u8;
    let mut negative = false;
    let mut p = 0usize;
    // Пробелы, потом знак; пробел после знака — уже не число.
    loop {
        let ch = at(p);
        if !is_white(ch) || state & SIGN != 0 {
            if state & SIGN == 0 && (ch == PLUS || ch == MINUS) && p < text.len() {
                state |= SIGN;
                negative = ch == MINUS;
            } else {
                break;
            }
        }
        if p >= text.len() {
            break;
        }
        p += 1;
    }

    let mut digits = Vec::new();
    let mut scale: i64 = 0;
    loop {
        let ch = at(p);
        if p < text.len() && is_digit(ch) {
            state |= DIGITS;
            if ch != ZERO || state & NON_ZERO != 0 {
                digits.push(ch as u8);
                if state & DECIMAL == 0 {
                    scale += 1;
                }
                state |= NON_ZERO;
            } else if state & DECIMAL != 0 {
                scale -= 1;
            }
        } else if p < text.len() && state & DECIMAL == 0 && ch == DOT {
            state |= DECIMAL;
        } else if p < text.len() && state & DIGITS != 0 && state & DECIMAL == 0 && ch == COMMA {
        } else {
            break;
        }
        p += 1;
    }
    if state & DIGITS == 0 {
        return None;
    }
    let ch = at(p);
    if p < text.len() && (ch == 0x45 || ch == 0x65) {
        let mark = p;
        p += 1;
        let mut negative_exponent = false;
        if at(p) == PLUS && p < text.len() {
            p += 1;
        } else if at(p) == MINUS && p < text.len() {
            p += 1;
            negative_exponent = true;
        }
        if p < text.len() && is_digit(at(p)) {
            let mut exponent: i64 = 0;
            while p < text.len() && is_digit(at(p)) {
                exponent = (exponent * 10 + i64::from(at(p) - ZERO)).min(1 << 40);
                p += 1;
            }
            scale += if negative_exponent { -exponent } else { exponent };
        } else {
            p = mark;
        }
    }
    while p < text.len() && is_white(at(p)) {
        p += 1;
    }
    // Хвост из `'\0'` допустим, как у .NET.
    if text[p..].iter().any(|&c| c != 0) {
        return None;
    }
    if state & NON_ZERO == 0 {
        digits.clear();
    }
    while digits.last() == Some(&b'0') {
        digits.pop();
    }
    Some((negative, digits, scale))
}
