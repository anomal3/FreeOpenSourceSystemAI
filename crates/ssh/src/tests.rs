//! Проверки на хосте: то, что можно доказать без эмулятора и без сети.
//!
//! Здесь проверяется вход по ключу — разбор `authorized_keys`, свой base64 и,
//! главное, **раскладка подписанного блока**. Последнее иначе не проверить
//! ничем: ошибка в порядке полей выглядит снаружи как `Permission denied
//! (publickey)` у клиента, то есть ровно так же, как чужой ключ, забытый файл и
//! опечатка в имени пользователя. Отладка такого в эмуляторе — это часы; здесь
//! это секунда.
//!
//! Кучи тесты не используют намеренно: тот же код собирается для программы, у
//! которой её нет, и буфер на стеке с явной длиной ближе к тому, как он
//! работает на самом деле.

use ed25519_dalek::{Signer, SigningKey};

use crate::auth::{self, KEY_ALGORITHM, SERVICE};
use crate::wire::Writer;

/// Ключ берётся из постоянного зерна: тест обязан давать один и тот же
/// результат на всякой машине, а случайный ключ означал бы, что провал
/// воспроизводится через раз.
fn key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

/// Открытый ключ в том виде, в каком он едет на провод.
fn key_blob(key: &SigningKey) -> ([u8; 128], usize) {
    let mut buffer = [0u8; 128];
    let len = {
        let mut writer = Writer::new(&mut buffer);
        writer.string(KEY_ALGORITHM.as_bytes());
        writer.string(&key.verifying_key().to_bytes());
        assert!(writer.ok(), "ключ помещается в 128 байт");
        writer.len()
    };
    (buffer, len)
}

/// Собрать `USERAUTH_REQUEST` так, как его собирает клиент.
///
/// `signed_user` и `signed_session` отдельно от `sent_user` именно затем, чтобы
/// тест мог подписать одно, а прислать другое.
fn request_packet(
    key: &SigningKey,
    signed_user: &[u8],
    sent_user: &[u8],
    signed_session: &[u8; 32],
) -> ([u8; 512], usize) {
    let (blob, blob_len) = key_blob(key);
    let blob = &blob[..blob_len];

    // То, что подписывает клиент (RFC 4252 §7).
    let mut signed = [0u8; 512];
    let signed_len = {
        let mut writer = Writer::new(&mut signed);
        writer.string(signed_session);
        writer.byte(crate::MSG_USERAUTH_REQUEST);
        writer.string(signed_user);
        writer.string(SERVICE.as_bytes());
        writer.string(b"publickey");
        writer.byte(1);
        writer.string(KEY_ALGORITHM.as_bytes());
        writer.string(blob);
        assert!(writer.ok(), "подписываемое помещается");
        writer.len()
    };
    let signature = key.sign(&signed[..signed_len]);

    let mut signature_blob = [0u8; 128];
    let signature_blob_len = {
        let mut writer = Writer::new(&mut signature_blob);
        writer.string(KEY_ALGORITHM.as_bytes());
        writer.string(&signature.to_bytes());
        assert!(writer.ok(), "подпись помещается");
        writer.len()
    };

    let mut packet = [0u8; 512];
    let packet_len = {
        let mut writer = Writer::new(&mut packet);
        writer.byte(crate::MSG_USERAUTH_REQUEST);
        writer.string(sent_user);
        writer.string(SERVICE.as_bytes());
        writer.string(b"publickey");
        writer.byte(1);
        writer.string(KEY_ALGORITHM.as_bytes());
        writer.string(blob);
        writer.string(&signature_blob[..signature_blob_len]);
        assert!(writer.ok(), "пакет помещается");
        writer.len()
    };
    (packet, packet_len)
}

#[test]
fn signature_of_a_real_client_layout_verifies() {
    let key = key();
    let session = [0x5au8; 32];
    let (packet, len) = request_packet(&key, b"roman", b"roman", &session);

    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");
    assert!(request.has_signature);
    assert_eq!(request.user, b"roman");
    assert_eq!(request.algorithm, KEY_ALGORITHM.as_bytes());

    let mut scratch = [0u8; 512];
    assert!(auth::verify(&request, &session, &mut scratch));
}

#[test]
fn a_signature_from_another_session_is_refused() {
    // Ровно та причина, по которой идентификатор сеанса вообще входит в
    // подписанное: записанная подпись не должна пускать никуда, кроме того
    // соединения, в котором она была сделана.
    let key = key();
    let (packet, len) = request_packet(&key, b"roman", b"roman", &[0x11u8; 32]);
    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");

    let mut scratch = [0u8; 512];
    assert!(!auth::verify(&request, &[0x22u8; 32], &mut scratch));
}

#[test]
fn a_signature_made_for_another_user_is_refused() {
    // Клиент подписал вход под именем `roman`, а прислал его под именем `root`.
    // Имя входит в подписанное, поэтому подмена обязана не сойтись.
    let key = key();
    let session = [0x5au8; 32];
    let (packet, len) = request_packet(&key, b"roman", b"root", &session);
    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");

    let mut scratch = [0u8; 512];
    assert!(!auth::verify(&request, &session, &mut scratch));
}

#[test]
fn a_scratch_buffer_too_small_refuses_instead_of_passing() {
    // Не поместившееся подписанное — это отказ, а не «проверили, что успели».
    let key = key();
    let session = [0x5au8; 32];
    let (packet, len) = request_packet(&key, b"roman", b"roman", &session);
    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");

    let mut scratch = [0u8; 16];
    assert!(!auth::verify(&request, &session, &mut scratch));
}

#[test]
fn a_query_without_a_signature_is_parsed_as_such() {
    // Первый шаг клиента: «годится ли такой ключ?». Подписи в нём нет, и принять
    // такой запрос за вход значило бы пускать в систему по одному открытому
    // ключу — то есть по тому, что и так лежит на всякой машине, куда владелец
    // ключа когда-либо входил.
    let key = key();
    let (blob, blob_len) = key_blob(&key);
    let mut packet = [0u8; 256];
    let len = {
        let mut writer = Writer::new(&mut packet);
        writer.byte(crate::MSG_USERAUTH_REQUEST);
        writer.string(b"roman");
        writer.string(SERVICE.as_bytes());
        writer.string(b"publickey");
        writer.byte(0);
        writer.string(KEY_ALGORITHM.as_bytes());
        writer.string(&blob[..blob_len]);
        assert!(writer.ok());
        writer.len()
    };

    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");
    assert!(!request.has_signature);
    assert!(request.signature_blob.is_empty());

    let mut scratch = [0u8; 512];
    assert!(!auth::verify(&request, &[0u8; 32], &mut scratch));
}

#[test]
fn the_none_method_is_parsed_without_a_key() {
    // Клиент OpenSSH всегда начинает с метода `none`: так он узнаёт список того,
    // чем можно продолжать. Разбор обязан не спотыкаться о запрос без ключа.
    let mut packet = [0u8; 128];
    let len = {
        let mut writer = Writer::new(&mut packet);
        writer.byte(crate::MSG_USERAUTH_REQUEST);
        writer.string(b"roman");
        writer.string(SERVICE.as_bytes());
        writer.string(b"none");
        assert!(writer.ok());
        writer.len()
    };

    let request = auth::parse_request(&packet[..len]).expect("попытка входа разбирается");
    assert_eq!(request.method, b"none");
    assert!(!request.has_signature);
}

#[test]
fn a_truncated_request_is_refused_rather_than_read_past_the_end() {
    // Длина строки приходит с провода. Пакет, оборванный посреди неё, обязан
    // дать отказ разбора, а не чтение за концом буфера.
    let key = key();
    let session = [0x5au8; 32];
    let (packet, len) = request_packet(&key, b"roman", b"roman", &session);
    for cut in 0..len {
        assert!(
            auth::parse_request(&packet[..cut]).is_none(),
            "обрезанный до {cut} байт запрос разобрался"
        );
    }
    assert!(auth::parse_request(&packet[..len]).is_some());
}

/// Открытый ключ в том виде, в каком его пишет `ssh-keygen`.
///
/// Настоящая строка настоящей пары, сделанной `ssh-keygen -t ed25519`: свой
/// base64 обязан совпасть с чужим до байта, иначе ключ из файла не сойдётся с
/// ключом, приехавшим с провода.
const SAMPLE_LINE: &str = "ssh-ed25519 \
AAAAC3NzaC1lZDI1NTE5AAAAIB1zOwP+hts637vEH3JtLutkgfx54kduCUQDe8ZbOm4v roman@freeos";

/// 32 байта того же ключа — то, что должно получиться из строки выше.
fn sample_key() -> [u8; 32] {
    let mut blob = [0u8; 128];
    let encoded = SAMPLE_LINE.split(' ').nth(1).expect("вторая колонка");
    let len = auth::base64_decode(encoded.as_bytes(), &mut blob).expect("строка декодируется");
    auth::key_from_blob(&blob[..len]).expect("это ed25519")
}

#[test]
fn a_key_from_authorized_keys_matches_the_same_key_from_the_wire() {
    assert!(auth::authorized(SAMPLE_LINE.as_bytes(), &sample_key()));
    // Тот же файл, но ключ другой — отказ. Иначе проверка ничего не проверяет.
    assert!(!auth::authorized(SAMPLE_LINE.as_bytes(), &[0u8; 32]));
}

#[test]
fn comments_blank_lines_and_other_key_types_are_skipped() {
    const FILE: &str = "# ключ для запасного ноутбука\n\
         \n\
         ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQC7 someone@elsewhere\n\
         ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTY= someone@elsewhere\n\
           ssh-ed25519 \
AAAAC3NzaC1lZDI1NTE5AAAAIB1zOwP+hts637vEH3JtLutkgfx54kduCUQDe8ZbOm4v roman@freeos   \n";
    assert!(auth::authorized(FILE.as_bytes(), &sample_key()));
}

#[test]
fn a_file_with_windows_line_endings_still_works() {
    // Файл, привезённый с чужой машины, приходит с `\r\n`. Незамеченный `\r`
    // попадает в base64 как посторонний знак и превращает годный ключ в отказ —
    // с сообщением, по которому этого не понять.
    const FILE: &str = "ssh-ed25519 \
AAAAC3NzaC1lZDI1NTE5AAAAIB1zOwP+hts637vEH3JtLutkgfx54kduCUQDe8ZbOm4v roman@freeos\r\n";
    assert!(auth::authorized(FILE.as_bytes(), &sample_key()));
}

#[test]
fn an_empty_file_authorizes_nobody() {
    assert!(!auth::authorized(b"", &[0u8; 32]));
    assert!(!auth::authorized(b"\n\n#  \n", &[0u8; 32]));
}

#[test]
fn base64_refuses_a_line_with_a_stray_character() {
    // Молча пропустить непонятный знак значило бы собрать ключ из огрызков и
    // сравнивать с ним — то есть отказывать по причине, которой нет в файле.
    let mut out = [0u8; 64];
    assert!(auth::base64_decode(b"AAAA!AAA", &mut out).is_none());
    assert_eq!(auth::base64_decode(b"AAAA", &mut out), Some(3));
}

#[test]
fn base64_does_not_write_past_the_end_of_the_buffer() {
    let mut out = [0u8; 4];
    let encoded = SAMPLE_LINE.split(' ').nth(1).expect("вторая колонка");
    assert!(auth::base64_decode(encoded.as_bytes(), &mut out).is_none());
}
