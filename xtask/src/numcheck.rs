//! Печать и разбор чисел своей средой против настоящего `dotnet` (фаза N4c).
//!
//! `tools/dotnet/numcheck` печатает случайные и пограничные числа по сотне
//! стандартных и пользовательских форматов и разбор строк в `double` и
//! `float` — строка на случай, с исходными битами. Здесь каждая строка
//! повторяется модулем `clr_vm::number` и сравнивается до символа.
//!
//! Почему отдельная сверка, а не образец: образец проходит через
//! интерпретатор, и сотня тысяч случаев в нём шла бы минуты, а на FreeOS
//! печаталась бы в последовательный порт. Правила печати живут в одном модуле
//! без интерпретатора — его и сверяем, быстро и целиком.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clr_vm::number::{self, FormatError};

pub fn check(root: &Path, out: &Path) -> Result<()> {
    let dir = out.join("numcheck");
    crate::clrcheck::dotnet_build(&root.join("tools/dotnet/numcheck"), &dir)?;
    let output = Command::new("dotnet")
        .arg(dir.join("numcheck.dll"))
        .env("DOTNET_SYSTEM_GLOBALIZATION_INVARIANT", "1")
        .output()
        .context("run numcheck")?;
    if !output.status.success() {
        bail!("numcheck failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let text = String::from_utf8(output.stdout).context("numcheck output is not UTF-8")?;

    let mut cases = 0usize;
    let mut differ = Vec::new();
    for line in text.lines() {
        cases += 1;
        let fields: Vec<&str> = line.split('\t').collect();
        let (theirs, ours) = match fields.as_slice() {
            ["D", bits, format, result] => {
                let value = f64::from_bits(u64::from_str_radix(bits, 16)?);
                (*result, printed(number::format_double(value, &unescape(format)?)))
            }
            ["S", bits, format, result] => {
                let value = f32::from_bits(u32::from_str_radix(bits, 16)?);
                (*result, printed(number::format_single(value, &unescape(format)?)))
            }
            ["I", width, value, format, result] => {
                let value: i128 = value.parse()?;
                (*result, printed(number::format_integer(value, width.parse()?, &unescape(format)?)))
            }
            ["P", input, result] => {
                let parsed = number::parse_float(&unescape(input)?, false);
                (*result, parsed.map_or_else(|| "!Fail".to_string(), |x| format!("{:016X}", x.to_bits())))
            }
            ["Q", input, result] => {
                let parsed = number::parse_float(&unescape(input)?, true);
                (*result, parsed.map_or_else(|| "!Fail".to_string(), |x| format!("{:08X}", (x as f32).to_bits())))
            }
            _ => bail!("numcheck printed a line of unknown shape: {line}"),
        };
        if ours != theirs {
            differ.push(format!("{line}\n  ours: {ours}"));
        }
    }

    if differ.is_empty() {
        println!("clr-check: numbers: all {cases} formatting and parsing cases match dotnet");
        return Ok(());
    }
    let report = out.join("numcheck.differs.txt");
    fs::write(&report, differ.join("\n"))?;
    for case in differ.iter().take(25) {
        println!("  {case}");
    }
    bail!("numbers: {} of {cases} cases differ from dotnet, all of them in {}", differ.len(), report.display())
}

fn printed(result: Result<Vec<u16>, FormatError>) -> String {
    match result {
        Ok(units) => escape(&units),
        Err(FormatError::Bad) => "!Format".to_string(),
        Err(FormatError::TooLong) => "!TooLong".to_string(),
    }
}

/// Как `Escape` в `numcheck`: вне 0x20..0x7E и `\` — `\uXXXX`.
fn escape(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    for &unit in units {
        if (0x20..0x7F).contains(&unit) && unit != u16::from(b'\\') {
            out.push(char::from(unit as u8));
        } else {
            let _ = write!(out, "\\u{unit:04X}");
        }
    }
    out
}

fn unescape(text: &str) -> Result<Vec<u16>> {
    let bytes = text.as_bytes();
    let mut units = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let hex = text.get(i + 2..i + 6).ok_or_else(|| anyhow!("broken escape in {text}"))?;
            units.push(u16::from_str_radix(hex, 16)?);
            i += 6;
        } else {
            units.push(u16::from(bytes[i]));
            i += 1;
        }
    }
    Ok(units)
}
