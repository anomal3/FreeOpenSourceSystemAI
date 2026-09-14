//! Тесты на сборке, записанной `dotnet build`, и на синтетических телах методов.
//!
//! Главная проверка разбора — не здесь, а в `cargo xtask clr-check`, где тот же
//! разбор сверяется с `System.Reflection.Metadata`. Эти тесты — то, что можно
//! прогнать без .NET на машине: они держат уже найденное, чтобы оно не
//! сломалось молча.

use std::string::String;
use std::vec::Vec;

use crate::body::{CLAUSE_CATCH, CLAUSE_FINALLY};
use crate::tables::id;
use crate::{Assembly, Coded, MethodBody, Token, compressed_i32, compressed_u32, sig};

/// `Console.WriteLine("Hello, World!");` из шаблона `dotnet new console`,
/// собранное .NET SDK 10 (net10.0, Release).
const HELLO: &[u8] = include_bytes!("../fixtures/hello.dll");

fn string_of(units: impl Iterator<Item = u16>) -> String {
    char::decode_utf16(units).map(|c| c.unwrap_or('?')).collect()
}

#[test]
fn compressed_integers_match_the_standard_examples() {
    // Примеры из ECMA-335 II.23.2.
    let cases: &[(&[u8], u32)] = &[
        (&[0x03], 0x03),
        (&[0x7F], 0x7F),
        (&[0x80, 0x80], 0x80),
        (&[0xAE, 0x57], 0x2E57),
        (&[0xBF, 0xFF], 0x3FFF),
        (&[0xC0, 0x00, 0x40, 0x00], 0x4000),
        (&[0xDF, 0xFF, 0xFF, 0xFF], 0x1FFF_FFFF),
    ];
    for (bytes, value) in cases {
        assert_eq!(compressed_u32(bytes), Some((*value, bytes.len())), "{bytes:02x?}");
    }
    let signed: &[(&[u8], i32)] = &[
        (&[0x06], 3),
        (&[0x7B], -3),
        (&[0x80, 0x80], 64),
        (&[0x01], -64),
        (&[0xC0, 0x00, 0x40, 0x00], 8192),
        (&[0x80, 0x01], -8192),
        (&[0xDF, 0xFF, 0xFF, 0xFE], 268_435_455),
        (&[0xC0, 0x00, 0x00, 0x01], -268_435_456),
    ];
    for (bytes, value) in signed {
        assert_eq!(compressed_i32(bytes), Some((*value, bytes.len())), "{bytes:02x?}");
    }
    assert_eq!(compressed_u32(&[0xE0]), None);
}

#[test]
fn hello_parses() {
    let asm = Assembly::parse(HELLO).expect("parse");
    assert_eq!((asm.cli.runtime_major, asm.cli.runtime_minor), (2, 5));
    assert_eq!(asm.root.version, "v4.0.30319");
    assert!(asm.tables.rows(id::TYPE_DEF) >= 2, "<Module> и Program");
    assert_eq!(asm.tables.rows(id::ASSEMBLY), 1);
    let name = asm.root.strings.get(asm.tables.column(id::ASSEMBLY, 1, 7).unwrap()).unwrap();
    assert_eq!(name, "hello");
}

/// Точка входа указывает на метод, тело которого — ровно `ldstr`, `call`, `ret`,
/// и строка с вызываемым методом — те, что написаны в программе.
#[test]
fn hello_entry_point_prints_the_greeting() {
    let asm = Assembly::parse(HELLO).expect("parse");
    let entry = Token::from_value(asm.cli.entry_point);
    assert_eq!(entry.table, id::METHOD_DEF);

    let rva = asm.tables.column(id::METHOD_DEF, entry.row, 0).unwrap();
    let body = asm.method_body(rva).unwrap().expect("body");
    let code = body.code;
    assert_eq!(code.len(), 11, "{code:02x?}");
    assert_eq!(code[0], 0x72, "ldstr");
    assert_eq!(code[5], 0x28, "call");
    assert_eq!(code[10], 0x2A, "ret");

    let string_token = Token::from_value(u32::from_le_bytes(code[1..5].try_into().unwrap()));
    assert_eq!(string_token.table, id::USER_STRING);
    let text = asm.root.user_strings.get(string_token.row).unwrap();
    assert_eq!(string_of(text.units()), "Hello, World!");

    let call = Token::from_value(u32::from_le_bytes(code[6..10].try_into().unwrap()));
    assert_eq!(call.table, id::MEMBER_REF);
    let name = asm.root.strings.get(asm.tables.column(id::MEMBER_REF, call.row, 1).unwrap()).unwrap();
    assert_eq!(name, "WriteLine");
    let parent = asm.tables.coded_column(id::MEMBER_REF, call.row, 0, Coded::MemberRefParent).unwrap();
    assert_eq!(parent.table, id::TYPE_REF);
    let type_name = asm.root.strings.get(asm.tables.column(id::TYPE_REF, parent.row, 1).unwrap()).unwrap();
    let namespace = asm.root.strings.get(asm.tables.column(id::TYPE_REF, parent.row, 2).unwrap()).unwrap();
    assert_eq!((namespace, type_name), ("System", "Console"));

    // `void WriteLine(string)`: без this, один параметр.
    let blob = asm.root.blobs.get(asm.tables.column(id::MEMBER_REF, call.row, 2).unwrap()).unwrap();
    let header = sig::method_header(blob).unwrap();
    assert_eq!((header.convention, header.params), (sig::CALL_DEFAULT, 1));
}

/// Методы каждого типа — непрерывный список, и вместе они покрывают таблицу
/// `MethodDef` ровно один раз.
#[test]
fn method_lists_cover_the_table() {
    let asm = Assembly::parse(HELLO).expect("parse");
    let mut seen = Vec::new();
    for row in 1..=asm.tables.rows(id::TYPE_DEF) {
        let list = asm.tables.list(id::TYPE_DEF, row, 5, id::METHOD_DEF, id::METHOD_PTR).unwrap();
        for method in list {
            seen.push(method.unwrap());
        }
    }
    let expected: Vec<u32> = (1..=asm.tables.rows(id::METHOD_DEF)).collect();
    assert_eq!(seen, expected);
}

#[test]
fn tiny_body() {
    // Заголовок 0x0A: крошечный, два байта кода.
    let body = MethodBody::parse(&[0x0A, 0x16, 0x2A, 0xFF]).unwrap();
    assert_eq!(body.code, &[0x16, 0x2A]);
    assert_eq!(body.max_stack, 8);
    assert_eq!(body.clauses().count(), 0);
}

/// Полный заголовок, код в три байта (раздел начинается с выравнивания) и две
/// малые записи обработчиков: `catch` и `finally`.
#[test]
fn fat_body_with_small_clauses() {
    let mut data = Vec::new();
    data.extend_from_slice(&(0x3000u16 | 0x0003 | 0x0008 | 0x0010).to_le_bytes());
    data.extend_from_slice(&5u16.to_le_bytes()); // max stack
    data.extend_from_slice(&3u32.to_le_bytes()); // code size
    data.extend_from_slice(&0x1100_0001u32.to_le_bytes()); // locals
    data.extend_from_slice(&[0x00, 0x00, 0x2A]); // nop nop ret
    data.push(0); // выравнивание до 16
    data.push(0x01); // EH table, малая форма, последний раздел
    data.push(4 + 12 * 2);
    data.extend_from_slice(&[0, 0]);
    for (flags, token) in [(0u16, 0x0100_0002u32), (2, 0)] {
        data.extend_from_slice(&flags.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes()); // try offset
        data.push(1); // try length
        data.extend_from_slice(&2u16.to_le_bytes()); // handler offset
        data.push(1); // handler length
        data.extend_from_slice(&token.to_le_bytes());
    }
    let body = MethodBody::parse(&data).unwrap();
    assert_eq!(body.code, &[0x00, 0x00, 0x2A]);
    assert!(body.init_locals);
    assert_eq!(body.local_signature, 0x1100_0001);
    let clauses: Vec<_> = body.clauses().map(Result::unwrap).collect();
    assert_eq!(clauses.len(), 2);
    assert_eq!(clauses[0].flags, CLAUSE_CATCH);
    assert_eq!(clauses[0].class_or_filter, 0x0100_0002);
    assert_eq!((clauses[1].flags, clauses[1].handler_offset), (CLAUSE_FINALLY, 2));
}

#[test]
fn coded_index_rejects_reserved_tags() {
    // У `CustomAttributeType` номера 0, 1 и 4 зарезервированы.
    assert!(crate::Tables::coded(Coded::CustomAttributeType, (5 << 3) | 1).is_err());
    let token = crate::Tables::coded(Coded::CustomAttributeType, (5 << 3) | 3).unwrap();
    assert_eq!(token, Token { table: id::MEMBER_REF, row: 5 });
}
