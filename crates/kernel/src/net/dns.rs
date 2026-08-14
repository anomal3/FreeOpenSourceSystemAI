//! DNS: спросить адрес по имени, и ровно это.
//!
//! Умеет один тип запроса — `A`, то есть адрес IPv4, — и один способ его
//! задать. Ни зоны, ни обратных запросов, ни кэша: кэш здесь был бы четвёртым
//! местом в системе, где что-то живёт со сроком годности, а выигрыш от него
//! появляется на десятках запросов в секунду, которых у нас не будет ещё долго.
//!
//! # Сжатие имён — не украшение формата, а условие разбора
//!
//! Имя в ответе почти никогда не записано целиком: вместо повторения оно
//! ссылается указателем на то место пакета, где уже встречалось. Пропустить имя
//! «до нулевого байта» поэтому нельзя — у сжатого имени нулевого байта нет,
//! оно кончается двухбайтовым указателем. Разборщик, не знающий об этом,
//! уезжает в поле данных следующей записи и находит там всё что угодно.
//!
//! Указатель ведёт назад по пакету, но проверять это обязательно: указатель на
//! самого себя — это бесконечный цикл, а пакет приходит из сети.

use crate::net::ipv4::Ipv4;

/// Порт сервера имён.
pub const PORT: u16 = 53;

/// Длина заголовка.
const HEADER: usize = 12;

/// Тип записи: адрес IPv4.
const TYPE_A: u16 = 1;
/// Класс: интернет.
const CLASS_IN: u16 = 1;

/// Наибольшая длина имени, которое мы согласны спросить.
///
/// 253 — предел самого формата; больше не бывает, а значит буфер запроса
/// ограничен и известен заранее.
pub const MAX_NAME: usize = 253;

/// Сколько прыжков по указателям сжатия допустимо, прежде чем признать пакет
/// злонамеренным.
const MAX_JUMPS: usize = 8;

/// Собрать запрос `A` и вернуть его длину.
///
/// Возвращает `None`, если имя не годится: пустое, слишком длинное, с пустой
/// или чрезмерно длинной меткой. Отвергать такое здесь дешевле, чем получать от
/// сервера отказ, который ещё надо разобрать.
pub fn write_query(buffer: &mut [u8], id: u16, name: &str) -> Option<usize> {
    if name.is_empty() || name.len() > MAX_NAME {
        return None;
    }

    buffer[0..2].copy_from_slice(&id.to_be_bytes());
    // 0x0100 — «прошу рекурсию»: спрашиваем у сервера, который сам сходит куда
    // надо, а не начинаем обход с корня. Обход с корня — это резолвер, а не
    // клиент, и он здесь не нужен.
    buffer[2..4].copy_from_slice(&0x0100u16.to_be_bytes());
    buffer[4..6].copy_from_slice(&1u16.to_be_bytes()); // один вопрос
    buffer[6..12].copy_from_slice(&[0; 6]); // ответов, полномочий и прочего нет

    let mut at = HEADER;
    for label in name.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        buffer[at] = label.len() as u8;
        buffer[at + 1..at + 1 + label.len()].copy_from_slice(label.as_bytes());
        at += 1 + label.len();
    }
    buffer[at] = 0; // конец имени
    at += 1;

    buffer[at..at + 2].copy_from_slice(&TYPE_A.to_be_bytes());
    buffer[at + 2..at + 4].copy_from_slice(&CLASS_IN.to_be_bytes());
    Some(at + 4)
}

/// Найти в ответе первый адрес IPv4.
///
/// Проверяется, что это ответ на **наш** вопрос: идентификатор совпадает, флаг
/// ответа выставлен, кода ошибки нет. Без первой проверки годится любой пакет,
/// пришедший на наш порт, — а порт эфемерный, и попасть в него может кто угодно.
pub fn parse_answer(message: &[u8], id: u16) -> Option<Ipv4> {
    if message.len() < HEADER {
        return None;
    }
    if u16::from_be_bytes([message[0], message[1]]) != id {
        return None;
    }
    let flags = u16::from_be_bytes([message[2], message[3]]);
    // Бит 15 — «это ответ»; младшие четыре бита — код ошибки, ноль означает
    // «всё в порядке». Имя, которого нет, приезжает кодом 3 и адресом не
    // становится.
    if flags & 0x8000 == 0 || flags & 0x000F != 0 {
        return None;
    }

    let questions = u16::from_be_bytes([message[4], message[5]]);
    let answers = u16::from_be_bytes([message[6], message[7]]);

    let mut at = HEADER;
    for _ in 0..questions {
        at = skip_name(message, at)?;
        // Тип и класс вопроса нам известны — мы их и посылали.
        at = at.checked_add(4)?;
    }

    for _ in 0..answers {
        at = skip_name(message, at)?;
        if at + 10 > message.len() {
            return None;
        }
        let kind = u16::from_be_bytes([message[at], message[at + 1]]);
        let class = u16::from_be_bytes([message[at + 2], message[at + 3]]);
        let length = usize::from(u16::from_be_bytes([message[at + 8], message[at + 9]]));
        at += 10;
        if at + length > message.len() {
            return None;
        }
        // Ответ на `A` может начинаться с цепочки `CNAME` — их мы пропускаем и
        // берём первый настоящий адрес: сервер, к которому мы обратились с
        // просьбой о рекурсии, кладёт его в тот же ответ.
        if kind == TYPE_A && class == CLASS_IN && length == 4 {
            let mut bytes = [0u8; 4];
            bytes.copy_from_slice(&message[at..at + 4]);
            return Some(Ipv4::from_bytes(bytes));
        }
        at += length;
    }
    None
}

/// Пропустить имя и вернуть позицию сразу за ним.
fn skip_name(message: &[u8], mut at: usize) -> Option<usize> {
    let mut jumps = 0;
    loop {
        let length = *message.get(at)?;
        // Два старших бита единицы — это указатель сжатия: имя продолжается в
        // другом месте пакета, а здесь оно кончается вторым байтом указателя.
        if length & 0xC0 == 0xC0 {
            message.get(at + 1)?;
            jumps += 1;
            if jumps > MAX_JUMPS {
                return None;
            }
            // Куда он ведёт, нам неважно: мы имя не читаем, а пропускаем.
            return Some(at + 2);
        }
        if length == 0 {
            return Some(at + 1);
        }
        at = at.checked_add(1 + usize::from(length))?;
        if at > message.len() {
            return None;
        }
    }
}
