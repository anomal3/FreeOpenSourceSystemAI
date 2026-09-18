// Copyright (C) 2026 Виталий Ардашов, Роман Кощеев
// Этот файл является частью FreeOpenSourceSystemAI.
// Программа распространяется на условиях GNU General Public License v3.

//! Проверки разбора: обычное сообщение — и всё то, чем разбор пробуют сломать.
//!
//! Половина проверок здесь не про «разобралось правильно», а про «отказано».
//! Это и есть смысл файла: сервер, терпимо относящийся к странному запросу,
//! отдаёт наружу то, чего не собирался, а прокси — ещё и передаёт дальше
//! сообщение, границу которого он и его собеседник видят по-разному.
//!
//! Кучи здесь нет, как и во всём крейте: буфер под путь — массив на стеке
//! проверки, ровно такой же, какой заводит сервер.

use super::{Body, Error, Head, Method, Request, Response, Version, head_end, mime, path};

/// Путь в буфер такого размера кладёт и сервер.
const ROOM: usize = 256;

/// Путь приводится к `want`.
fn gives(raw: &str, want: &str) -> bool {
    let mut out = [0u8; ROOM];
    path::normalize(raw, &mut out) == Some(want)
}

/// Путь не годится вовсе.
fn refused(raw: &str) -> bool {
    let mut out = [0u8; ROOM];
    path::normalize(raw, &mut out).is_none()
}

#[test]
fn ordinary_request_parses() {
    let bytes = b"GET /index.html HTTP/1.1\r\nHost: freeos\r\nUser-Agent: curl/8\r\n\r\n";
    assert_eq!(head_end(bytes), Some(bytes.len()));
    let request = Request::parse(bytes).expect("обычный запрос");
    assert_eq!(request.method, Method::Get);
    assert_eq!(request.target, "/index.html");
    assert_eq!(request.version, Version::Http11);
    assert_eq!(request.head.value("host"), Some("freeos"));
    assert_eq!(request.body(), Ok(Body::None));
    assert!(request.keep_alive());
}

#[test]
fn head_ends_only_at_the_empty_line() {
    assert_eq!(head_end(b"GET / HTTP/1.1\r\nHost: x\r\n"), None);
    assert_eq!(head_end(b"GET / HTTP/1.1\r\n\r\nbody"), Some(18));
    // Пустая строка из одних переводов строки концом головы не считается: этим
    // и отличается наш разбор от того, который увидит прокси на той стороне.
    assert_eq!(head_end(b"GET / HTTP/1.1\n\nbody"), None);
}

#[test]
fn field_names_ignore_case_and_keep_order() {
    let head = Head::parse(b"GET / HTTP/1.1\r\nHost: a\r\nX-Thing: 1\r\nX-THING: 2\r\n\r\n")
        .expect("поля разбираются");
    assert_eq!(head.value("HOST"), Some("a"));
    assert_eq!(head.value("x-thing"), Some("1"));
    assert_eq!(head.count("X-Thing"), 2);
    let expected = [("Host", "a"), ("X-Thing", "1"), ("X-THING", "2")];
    let mut seen = 0usize;
    for (got, want) in head.fields().zip(expected) {
        assert_eq!(got, want);
        seen += 1;
    }
    assert_eq!(seen, expected.len());
}

#[test]
fn a_bare_newline_is_refused() {
    // Одинокий `\n` внутри головы — классическая щель контрабанды: одни
    // считают строку законченной, другие нет.
    assert_eq!(
        Head::parse(b"GET / HTTP/1.1\nHost: a\r\n\r\n").unwrap_err(),
        Error::BadLineEnd
    );
}

#[test]
fn folded_and_spaced_fields_are_refused() {
    assert_eq!(
        Head::parse(b"GET / HTTP/1.1\r\nHost: a\r\n continued\r\n\r\n").unwrap_err(),
        Error::BadField
    );
    assert_eq!(Head::parse(b"GET / HTTP/1.1\r\nHost : a\r\n\r\n").unwrap_err(), Error::BadField);
    assert_eq!(Head::parse(b"GET / HTTP/1.1\r\nHost\r\n\r\n").unwrap_err(), Error::BadField);
}

#[test]
fn two_lengths_are_refused_even_when_they_agree() {
    let head = Head::parse(b"POST / HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n")
        .expect("поля сами по себе законны");
    assert_eq!(head.content_length(), Err(Error::Conflicting));
}

#[test]
fn length_together_with_chunks_is_refused() {
    let request = Request::parse(
        b"POST / HTTP/1.1\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n",
    )
    .expect("строка запроса законна");
    assert_eq!(request.body(), Err(Error::Conflicting));
}

#[test]
fn a_length_that_is_not_a_number_is_refused() {
    let head = Head::parse(b"POST / HTTP/1.1\r\nContent-Length: 0x10\r\n\r\n").expect("поле есть");
    assert_eq!(head.content_length(), Err(Error::BadLength));
    let head = Head::parse(b"POST / HTTP/1.1\r\nContent-Length: +5\r\n\r\n").expect("поле есть");
    assert_eq!(head.content_length(), Err(Error::BadLength));
}

#[test]
fn the_target_must_be_a_path() {
    // Форма `http://...` — это просьба сходить куда-то от имени сервера.
    assert_eq!(
        Request::parse(b"GET http://elsewhere/x HTTP/1.1\r\n\r\n").unwrap_err(),
        Error::BadStart
    );
    assert_eq!(Request::parse(b"GET * HTTP/1.1\r\n\r\n").unwrap_err(), Error::BadStart);
    // Пробел внутри цели превратил бы одну строку запроса в две разных.
    assert_eq!(Request::parse(b"GET /a b HTTP/1.1\r\n\r\n").unwrap_err(), Error::BadStart);
}

#[test]
fn unknown_versions_are_refused() {
    assert_eq!(Request::parse(b"GET / HTTP/2.0\r\n\r\n").unwrap_err(), Error::BadVersion);
    assert_eq!(Request::parse(b"GET / HTTP/1\r\n\r\n").unwrap_err(), Error::BadVersion);
}

#[test]
fn keep_alive_follows_the_version_and_the_field() {
    let http11 = Request::parse(b"GET / HTTP/1.1\r\n\r\n").expect("запрос");
    assert!(http11.keep_alive());
    let closing = Request::parse(b"GET / HTTP/1.1\r\nConnection: close\r\n\r\n").expect("запрос");
    assert!(!closing.keep_alive());
    let http10 = Request::parse(b"GET / HTTP/1.0\r\n\r\n").expect("запрос");
    assert!(!http10.keep_alive());
    let http10_alive =
        Request::parse(b"GET / HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").expect("запрос");
    assert!(http10_alive.keep_alive());
    // Слово ищется в списке, а не подстрокой.
    let listed = Request::parse(b"GET / HTTP/1.1\r\nConnection: keep-alive, upgrade\r\n\r\n")
        .expect("запрос");
    assert!(listed.keep_alive());
}

#[test]
fn answers_parse_and_frame_their_body() {
    let bytes = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nContent-Type: text/plain\r\n\r\n";
    let answer = Response::parse(bytes).expect("ответ");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.reason, "OK");
    assert_eq!(answer.body(Method::Get), Ok(Body::Length(12)));
    // У ответа на HEAD тела нет, сколько бы ни было объявлено. Прокси,
    // забывший об этом, ждёт байты, которых не будет, до самого таймаута.
    assert_eq!(answer.body(Method::Head), Ok(Body::None));
}

#[test]
fn an_answer_without_a_length_ends_with_the_connection() {
    let answer = Response::parse(b"HTTP/1.0 200 OK\r\n\r\n").expect("ответ");
    assert_eq!(answer.body(Method::Get), Ok(Body::ToClose));
    assert!(!answer.keep_alive());
    // А у `304` тела нет по определению, и закрытия ждать не надо.
    let not_modified = Response::parse(b"HTTP/1.1 304 Not Modified\r\n\r\n").expect("ответ");
    assert_eq!(not_modified.body(Method::Get), Ok(Body::None));
}

#[test]
fn a_status_line_is_three_digits() {
    assert_eq!(Response::parse(b"HTTP/1.1 20 OK\r\n\r\n").unwrap_err(), Error::BadStatus);
    assert_eq!(Response::parse(b"HTTP/1.1 OK\r\n\r\n").unwrap_err(), Error::BadStatus);
    // Пояснения может не быть вовсе — это законно.
    let bare = Response::parse(b"HTTP/1.1 204\r\n\r\n").expect("ответ без пояснения");
    assert_eq!(bare.status, 204);
    assert_eq!(bare.reason, "");
}

#[test]
fn paths_are_decoded_before_they_are_judged() {
    assert!(gives("/a/b", "/a/b"));
    assert!(gives("/%41%2fb", "/A/b"));
    assert!(gives("/a/./b", "/a/b"));
    assert!(gives("//a///b/", "/a/b"));
    assert!(gives("/", "/"));
    assert!(gives("/a/b/..", "/a"));
    assert!(gives("/a/../b", "/b"));
}

#[test]
fn paths_do_not_leave_the_root() {
    assert!(refused("/../etc/passwd"));
    assert!(refused("/a/../../etc/passwd"));
    // То же самое, написанное через `%`: проверка «нет ли двух точек», сделанная
    // до разворачивания, пропустила бы обе эти строки.
    assert!(refused("/%2e%2e/etc/passwd"));
    assert!(refused("/a/..%2f..%2fetc/passwd"));
    assert!(refused("etc/passwd"));
}

#[test]
fn broken_percent_sequences_are_refused() {
    assert!(refused("/%"));
    assert!(refused("/%4"));
    assert!(refused("/%zz"));
    // Развёрнутый управляющий знак — тоже отказ: `\0` обрежет имя у всякого,
    // кто передаст его дальше строкой C, а `\n` нарисует в журнале строку,
    // которой не было.
    assert!(refused("/a%00b"));
    assert!(refused("/a%0ab"));
    assert!(refused("/a\\b"));
}

#[test]
fn a_path_that_does_not_fit_is_refused() {
    let mut long = [b'a'; ROOM + 1];
    long[0] = b'/';
    let long = core::str::from_utf8(&long).expect("буквы");
    assert!(refused(long));

    // И слишком глубокий: сегменты считает тот, кто прислал запрос.
    let mut deep = [b'/'; 128];
    for (at, byte) in deep.iter_mut().enumerate() {
        if at % 2 == 1 {
            *byte = b'a';
        }
    }
    let deep = core::str::from_utf8(&deep).expect("буквы");
    assert!(refused(deep));
}

#[test]
fn a_prefix_matches_whole_segments() {
    assert_eq!(path::under("/up/x", "/up"), Some("/x"));
    assert_eq!(path::under("/up", "/up"), Some("/"));
    assert_eq!(path::under("/up/", "/up"), Some("/"));
    // `/update` начинается на `/up`, но лежит не под ним.
    assert_eq!(path::under("/update", "/up"), None);
    assert_eq!(path::under("/other", "/up"), None);
}

#[test]
fn the_query_is_kept_apart_from_the_path() {
    assert_eq!(path::split_target("/a/b?x=1&y=2"), ("/a/b", "x=1&y=2"));
    assert_eq!(path::split_target("/a/b"), ("/a/b", ""));
    assert_eq!(path::split_target("/a?"), ("/a", ""));
}

#[test]
fn types_come_from_the_extension() {
    assert_eq!(mime::of("/index.html"), "text/html; charset=utf-8");
    assert_eq!(mime::of("/style.CSS"), "text/css; charset=utf-8");
    assert_eq!(mime::of("/big.dat"), mime::UNKNOWN);
    assert_eq!(mime::of("/no-extension"), mime::UNKNOWN);
    // Точка выше по пути расширением не считается.
    assert_eq!(mime::of("/dir.d/file"), mime::UNKNOWN);
}
