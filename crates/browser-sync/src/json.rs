// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Маленький JSON: ровно столько, сколько нужно журналам и снимкам.
//!
//! Числа хранятся текстом (`Value::Num`) — синхронизации они нужны только как
//! есть, без арифметики. Порядок полей объекта сохраняется, повторы ключей не
//! проверяются (берётся первый — `Value::get`).

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    Obj(Vec<(String, Value)>),
}

impl Value {
    /// Поле объекта по имени (первое, если повторяется).
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Obj(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn str_field(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Запись одной строкой, без пробелов.
    pub fn write(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Num(n) => out.push_str(n),
            Value::Str(s) => write_str(s, out),
            Value::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Value::Obj(fields) => {
                out.push('{');
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_str(k, out);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }

    pub fn to_text(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }
}

/// Строка JSON в кавычках. Управляющие символы — `\uXXXX`, остальное — UTF-8
/// как есть (так журналы на GitHub читаются глазами).
pub fn write_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Ошибка разбора: смещение в байтах, где разбор споткнулся.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError(pub usize);

pub fn parse(text: &str) -> Result<Value, ParseError> {
    let mut p = Parser { s: text.as_bytes(), i: 0, depth: 0 };
    p.ws();
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(ParseError(p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    depth: u32,
}

/// Глубже не бывает ни в журнале, ни в снимке; предел защищает стек от
/// присланного кем-то файла `[[[[…`.
const MAX_DEPTH: u32 = 64;

impl Parser<'_> {
    fn err<T>(&self) -> Result<T, ParseError> {
        Err(ParseError(self.i))
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.s.get(self.i) {
            self.i += 1;
        }
    }

    fn eat(&mut self, lit: &[u8]) -> bool {
        if self.s[self.i..].starts_with(lit) {
            self.i += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self) -> Result<Value, ParseError> {
        match self.s.get(self.i) {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') if self.eat(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat(b"null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => self.err(),
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.err();
        }
        self.i += 1;
        self.ws();
        Ok(())
    }

    fn object(&mut self) -> Result<Value, ParseError> {
        self.enter()?;
        let mut fields = Vec::new();
        if self.eat(b"}") {
            self.depth -= 1;
            return Ok(Value::Obj(fields));
        }
        loop {
            if self.s.get(self.i) != Some(&b'"') {
                return self.err();
            }
            let k = self.string()?;
            self.ws();
            if !self.eat(b":") {
                return self.err();
            }
            self.ws();
            let v = self.value()?;
            fields.push((k, v));
            self.ws();
            if self.eat(b",") {
                self.ws();
                continue;
            }
            if self.eat(b"}") {
                self.depth -= 1;
                return Ok(Value::Obj(fields));
            }
            return self.err();
        }
    }

    fn array(&mut self) -> Result<Value, ParseError> {
        self.enter()?;
        let mut items = Vec::new();
        if self.eat(b"]") {
            self.depth -= 1;
            return Ok(Value::Arr(items));
        }
        loop {
            items.push(self.value()?);
            self.ws();
            if self.eat(b",") {
                self.ws();
                continue;
            }
            if self.eat(b"]") {
                self.depth -= 1;
                return Ok(Value::Arr(items));
            }
            return self.err();
        }
    }

    fn number(&mut self) -> Result<Value, ParseError> {
        let start = self.i;
        if self.s[self.i] == b'-' {
            self.i += 1;
        }
        let digits = |p: &mut Self| {
            let st = p.i;
            while let Some(b'0'..=b'9') = p.s.get(p.i) {
                p.i += 1;
            }
            p.i > st
        };
        if !digits(self) {
            return self.err();
        }
        if self.s.get(self.i) == Some(&b'.') {
            self.i += 1;
            if !digits(self) {
                return self.err();
            }
        }
        if let Some(b'e' | b'E') = self.s.get(self.i) {
            self.i += 1;
            if let Some(b'+' | b'-') = self.s.get(self.i) {
                self.i += 1;
            }
            if !digits(self) {
                return self.err();
            }
        }
        // Только ASCII — срез по границе символа.
        let text = core::str::from_utf8(&self.s[start..self.i]).map_err(|_| ParseError(start))?;
        Ok(Value::Num(String::from(text)))
    }

    fn hex4(&mut self) -> Result<u32, ParseError> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = match self.s.get(self.i) {
                Some(c @ b'0'..=b'9') => c - b'0',
                Some(c @ b'a'..=b'f') => c - b'a' + 10,
                Some(c @ b'A'..=b'F') => c - b'A' + 10,
                _ => return self.err(),
            };
            v = v * 16 + u32::from(d);
            self.i += 1;
        }
        Ok(v)
    }

    fn string(&mut self) -> Result<String, ParseError> {
        self.i += 1; // кавычка
        let mut out = String::new();
        loop {
            // Кусок без экранирования копируется целиком: вход — `&str`,
            // так что границы символов здесь всегда честные.
            let st = self.i;
            while let Some(&c) = self.s.get(self.i) {
                if c == b'"' || c == b'\\' || c < 0x20 {
                    break;
                }
                self.i += 1;
            }
            out.push_str(core::str::from_utf8(&self.s[st..self.i]).map_err(|_| ParseError(st))?);
            match self.s.get(self.i) {
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let e = match self.s.get(self.i) {
                        Some(&e) => e,
                        None => return self.err(),
                    };
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if !self.eat(b"\\u") {
                                    return self.err();
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return self.err();
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else {
                                hi
                            };
                            match char::from_u32(cp) {
                                Some(c) => out.push(c),
                                None => return self.err(),
                            }
                        }
                        _ => return self.err(),
                    }
                }
                _ => return self.err(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn roundtrip_strings() {
        for s in ["", "abc", "кавычка \" и \\", "\n\t\r\u{1}", "😀 emoji", "\u{2028}"] {
            let mut t = String::new();
            write_str(s, &mut t);
            assert_eq!(parse(&t), Ok(Value::Str(String::from(s))), "{t}");
        }
    }

    #[test]
    fn surrogates_and_escapes() {
        assert_eq!(parse(r#""😀\/""#), Ok(Value::Str(String::from("😀/"))));
        assert!(parse(r#""\ud83d""#).is_err());
        assert!(parse("\"a\nb\"").is_err());
    }

    #[test]
    fn nested() {
        let v = parse(r#" {"a": [1, -2.5e3, true, null], "b": {"c": "d"}} "#).unwrap();
        assert_eq!(v.get("b").and_then(|b| b.str_field("c")), Some("d"));
        assert_eq!(
            v.get("a"),
            Some(&Value::Arr(vec![
                Value::Num("1".into()),
                Value::Num("-2.5e3".into()),
                Value::Bool(true),
                Value::Null
            ]))
        );
        assert_eq!(parse(&v.to_text()), Ok(v));
    }

    #[test]
    fn rejects_garbage() {
        for t in ["", "{", "{\"a\"}", "[1,]", "{\"a\":1,}", "01x", "tru", "{} x", "-"] {
            assert!(parse(t).is_err(), "{t}");
        }
        let deep = "[".repeat(1000);
        assert!(parse(&deep).is_err());
    }
}
