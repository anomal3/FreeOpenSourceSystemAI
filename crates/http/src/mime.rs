// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Тип содержимого по расширению имени.
//!
//! # Почему по расширению, а не по содержимому
//!
//! Потому что угадывание типа по первым байтам — это то, за что браузеры
//! расплачиваются до сих пор: файл, отданный как «не знаю что», но похожий на
//! разметку, где-нибудь да будет показан как разметка, и чужой текст, лежащий
//! на сайте, станет кодом на нём же. Расширение выбирает тот, кто положил файл
//! в каталог сайта, то есть хозяин машины, — и это единственный, чьё мнение
//! здесь имеет значение.
//!
//! Незнакомое расширение получает `application/octet-stream`: «это байты,
//! показывать их не надо». Ошибиться в эту сторону безопасно.

/// Тип неизвестного содержимого.
pub const UNKNOWN: &str = "application/octet-stream";

/// Расширение и тип. Список короткий нарочно: сюда дописывают то, что кладут в
/// каталог сайта, а не всё, что бывает на свете.
const TABLE: &[(&str, &str)] = &[
    ("html", "text/html; charset=utf-8"),
    ("htm", "text/html; charset=utf-8"),
    ("css", "text/css; charset=utf-8"),
    ("js", "text/javascript; charset=utf-8"),
    ("json", "application/json"),
    ("txt", "text/plain; charset=utf-8"),
    ("md", "text/plain; charset=utf-8"),
    ("svg", "image/svg+xml"),
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("ico", "image/x-icon"),
    ("pdf", "application/pdf"),
];

/// Тип по имени файла.
///
/// Кодировка названа у текстовых типов и названа одна — UTF-8: другая в этой
/// системе не водится, а промолчать значит отдать выбор тому, кто читает.
#[must_use]
pub fn of(name: &str) -> &'static str {
    let Some((_, extension)) = name.rsplit_once('.') else {
        return UNKNOWN;
    };
    // Имя `dir.d/file` расширения не имеет: точка стоит выше по пути.
    if extension.contains('/') {
        return UNKNOWN;
    }
    TABLE
        .iter()
        .find(|(known, _)| extension.eq_ignore_ascii_case(known))
        .map_or(UNKNOWN, |(_, kind)| *kind)
}
