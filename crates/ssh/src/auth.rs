//! Вход по открытому ключу (RFC 4252 §7) и разбор `authorized_keys`.
//!
//! # Что именно подписывает клиент
//!
//! Не пароль и не случайную строку сервера, а **описание самой попытки входа**,
//! начинающееся с идентификатора сеанса:
//!
//! ```text
//!   string  session_id        хеш первого обмена ключами
//!   byte    SSH_MSG_USERAUTH_REQUEST
//!   string  имя пользователя
//!   string  имя службы ("ssh-connection")
//!   string  "publickey"
//!   boolean TRUE
//!   string  имя алгоритма ключа
//!   string  сам открытый ключ
//! ```
//!
//! Смысл в первой строке. Идентификатор сеанса уникален для соединения и
//! известен обеим сторонам, но никому третьему: он выведен из общего секрета
//! обмена. Поэтому подпись **нельзя переиспользовать** — записав её, чужой
//! сервер не сможет войти от вашего имени куда-то ещё. Без этой привязки
//! аутентификация по ключу превратилась бы в предъявление пароля.
//!
//! # Пароли не принимаются никогда
//!
//! В `/etc/passwd` этой системы лежит итерированный FNV-1a, а не функция
//! выведения ключа. Пускать по нему в систему **по сети** значило бы выдать
//! его за защиту. Отказ жёсткий: единственный метод — `publickey`.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use crate::wire::{Reader, Writer};

/// Имя алгоритма ключа, который мы принимаем.
pub const KEY_ALGORITHM: &str = "ssh-ed25519";

/// Имя службы, к которой ведёт вход.
pub const SERVICE: &str = "ssh-connection";

/// Разобранная попытка входа.
pub struct Request<'a> {
    pub user: &'a [u8],
    pub service: &'a [u8],
    pub method: &'a [u8],
    /// `false` — клиент только спрашивает, годится ли ключ.
    pub has_signature: bool,
    pub algorithm: &'a [u8],
    /// Открытый ключ целиком, как он едет на провод (blob).
    pub key_blob: &'a [u8],
    pub signature_blob: &'a [u8],
}

/// Разобрать `SSH_MSG_USERAUTH_REQUEST`.
pub fn parse_request(payload: &[u8]) -> Option<Request<'_>> {
    let mut reader = Reader::new(payload.get(1..)?);
    let user = reader.string()?;
    let service = reader.string()?;
    let method = reader.string()?;
    if method != b"publickey" {
        return Some(Request {
            user,
            service,
            method,
            has_signature: false,
            algorithm: b"",
            key_blob: b"",
            signature_blob: b"",
        });
    }
    let has_signature = reader.byte()? != 0;
    let algorithm = reader.string()?;
    let key_blob = reader.string()?;
    let signature_blob = if has_signature { reader.string()? } else { b"" };

    Some(Request {
        user,
        service,
        method,
        has_signature,
        algorithm,
        key_blob,
        signature_blob,
    })
}

/// Достать 32 байта ключа из его представления на проводе.
pub fn key_from_blob(blob: &[u8]) -> Option<[u8; 32]> {
    let mut reader = Reader::new(blob);
    if reader.string()? != KEY_ALGORITHM.as_bytes() {
        return None;
    }
    let key = reader.string()?;
    if key.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(key);
    Some(out)
}

/// Проверить подпись попытки входа.
///
/// `scratch` — рабочий буфер, в котором собирается подписанное. Своего буфера
/// функция не заводит: кучи в программе нет, а размер зависит от длины имени
/// пользователя, то есть приходит снаружи.
pub fn verify(
    request: &Request<'_>,
    session_id: &[u8; 32],
    scratch: &mut [u8],
) -> bool {
    let Some(key) = key_from_blob(request.key_blob) else {
        return false;
    };
    let Ok(verifying) = VerifyingKey::from_bytes(&key) else {
        return false;
    };

    // Подпись едет в том же виде, что и ключ: имя алгоритма, потом сами байты.
    let mut reader = Reader::new(request.signature_blob);
    let Some(algorithm) = reader.string() else {
        return false;
    };
    if algorithm != KEY_ALGORITHM.as_bytes() {
        return false;
    }
    let Some(bytes) = reader.string() else {
        return false;
    };
    if bytes.len() != 64 {
        return false;
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(bytes);

    let mut writer = Writer::new(scratch);
    writer.string(session_id);
    writer.byte(crate::MSG_USERAUTH_REQUEST);
    writer.string(request.user);
    writer.string(request.service);
    writer.string(b"publickey");
    writer.byte(1);
    writer.string(request.algorithm);
    writer.string(request.key_blob);
    let Some(signed) = writer.finish() else {
        return false;
    };

    verifying
        .verify(signed, &Signature::from_bytes(&signature))
        .is_ok()
}

/// Есть ли такой ключ в файле `authorized_keys`.
///
/// Формат тот же, что у всех: строки вида `ssh-ed25519 <base64> комментарий`,
/// пустые строки и строки, начинающиеся с `#`, пропускаются. Ключи других
/// типов пропускаются молча — это не ошибка файла, а ключ, который эта система
/// не умеет проверять.
pub fn authorized(file: &[u8], key: &[u8; 32]) -> bool {
    for line in file.split(|byte| *byte == b'\n') {
        let line = trim(line);
        if line.is_empty() || line[0] == b'#' {
            continue;
        }
        let mut parts = line.splitn(3, |byte| *byte == b' ');
        let Some(algorithm) = parts.next() else {
            continue;
        };
        if algorithm != KEY_ALGORITHM.as_bytes() {
            continue;
        }
        let Some(encoded) = parts.next() else {
            continue;
        };
        let mut blob = [0u8; 128];
        let Some(len) = base64_decode(encoded, &mut blob) else {
            continue;
        };
        if let Some(candidate) = key_from_blob(&blob[..len]) {
            // Сравнение постоянного времени здесь не нужно: открытый ключ не
            // секрет, и его сравнение ничего не выдаёт. Секретна подпись, а её
            // проверяет чужая библиотека.
            if candidate == *key {
                return true;
            }
        }
    }
    false
}

fn trim(line: &[u8]) -> &[u8] {
    let start = line.iter().position(|byte| !byte.is_ascii_whitespace()).unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |at| at + 1);
    &line[start..end]
}

/// Раскодировать base64. Возвращает длину результата.
///
/// Своё, а не крейт: тридцать строк против ещё одной зависимости, которую
/// пришлось бы объяснять в списке. Дополнение `=` допускается и пропускается,
/// перенос строки внутри — тоже.
pub fn base64_decode(input: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut accumulator: u32 = 0;
    let mut bits = 0u32;
    let mut at = 0usize;

    for byte in input {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => continue,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            // Любой другой символ означает, что это не base64, и молча
            // пропустить его нельзя: получится ключ, собранный из огрызков.
            _ => return None,
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            if at >= out.len() {
                return None;
            }
            out[at] = (accumulator >> bits) as u8;
            at += 1;
        }
    }
    Some(at)
}
