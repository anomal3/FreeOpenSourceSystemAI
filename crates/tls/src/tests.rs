//! Проверки на хосте — без эмулятора и без сети.
//!
//! Их две породы, и обе нужны.
//!
//! 1. **Расписание ключей по RFC 8448.** Приложение к RFC 8446 содержит полный
//!    трейс настоящего рукопожатия вместе со всеми промежуточными секретами.
//!    Без него расписание ключей отладить нечем: ошибка в одной метке даёт
//!    рукопожатие, которое сходится само с собой и ни с кем больше, а видно её
//!    только как «Finished не сошёлся» — то есть в самом конце и не там, где
//!    причина.
//!
//! 2. **Рукопожатие с `rustls`.** Чужая реализация на том конце — единственное,
//!    что отличает «наш клиент работает» от «наши две половины согласны друг с
//!    другом». Разговор идёт в памяти, без сокетов: `rustls` умеет отдавать и
//!    принимать байты буферами, а нам ровно это и надо.

use std::vec;
use std::vec::Vec;

use crate::hkdf::{derive_empty, derive_secret, expand_label, extract, hmac, sha256};

/// Ключ из шестнадцатеричной строки.
fn key32(text: &str) -> [u8; 32] {
    let bytes = hex::decode(text).expect("вектор записан шестнадцатеричным");
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

/// RFC 5869, тестовый случай 1: HKDF на SHA-256.
///
/// Проверяет `Extract` и `Expand` отдельно от меток TLS — чтобы отказ называл,
/// что именно сломалось.
#[test]
fn hkdf_matches_rfc_5869() {
    let ikm = hex::decode("0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b").unwrap();
    let salt = hex::decode("000102030405060708090a0b0c").unwrap();
    let prk = extract(&salt, &ikm);
    assert_eq!(
        hex::encode(prk),
        "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5"
    );
}

/// HMAC-SHA256 на векторе из RFC 4231 (случай 2).
#[test]
fn hmac_matches_rfc_4231() {
    let tag = hmac(b"Jefe", &[b"what do ya want for nothing?"]);
    assert_eq!(
        hex::encode(tag),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

/// Расписание ключей целиком — по трейсу из RFC 8448, §3.
///
/// Числа взяты из приложения к стандарту, а не посчитаны здесь: посчитанное
/// нашим же кодом ожидание проверяет только то, что код равен сам себе.
#[test]
fn the_key_schedule_matches_rfc_8448() {
    // Общий секрет X25519 из трейса.
    let shared = key32("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
    // Хеш стенограммы ClientHello..ServerHello.
    let after_hello = key32("860c06edc07858ee8e78f0e7428c58edd6b43f2ca3e6e95f02ed063cf0e1cad8");
    // Хеш стенограммы до Finished сервера включительно.
    let after_finished =
        key32("9608102a0f1ccc6db6250b7b7e417b1a000eaada3daae4777a7686c9ff83df13");

    let early = extract(&[0u8; 32], &[0u8; 32]);
    assert_eq!(
        hex::encode(early),
        "33ad0a1c607ec03b09e6cd9893680ce210adf300aa1f2660e1b22e10f170f92a",
        "early_secret"
    );

    let derived = derive_empty(&early, "derived");
    assert_eq!(
        hex::encode(derived),
        "6f2615a108c702c5678f54fc9dbab69716c076189c48250cebeac3576c3611ba",
        "derived из early_secret"
    );

    let handshake = extract(&derived, &shared);
    assert_eq!(
        hex::encode(handshake),
        "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac",
        "handshake_secret"
    );

    let client_hs = derive_secret(&handshake, "c hs traffic", &after_hello);
    assert_eq!(
        hex::encode(client_hs),
        "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21",
        "client_handshake_traffic_secret"
    );

    let server_hs = derive_secret(&handshake, "s hs traffic", &after_hello);
    assert_eq!(
        hex::encode(server_hs),
        "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38",
        "server_handshake_traffic_secret"
    );

    let derived2 = derive_empty(&handshake, "derived");
    assert_eq!(
        hex::encode(derived2),
        "43de77e0c77713859a944db9db2590b53190a65b3ee2e4f12dd7a0bb7ce254b4",
        "derived из handshake_secret"
    );

    let master = extract(&derived2, &[0u8; 32]);
    assert_eq!(
        hex::encode(master),
        "18df06843d13a08bf2a449844c5f8a478001bc4d4c627984d5a41da8d0402919",
        "master_secret"
    );

    let client_ap = derive_secret(&master, "c ap traffic", &after_finished);
    assert_eq!(
        hex::encode(client_ap),
        "9e40646ce79a7f9dc05af8889bce6552875afa0b06df0087f792ebb7c17504a5",
        "client_application_traffic_secret_0"
    );

    let server_ap = derive_secret(&master, "s ap traffic", &after_finished);
    assert_eq!(
        hex::encode(server_ap),
        "a11af9f05531f856ad47116b45a950328204b4f44bfb6b3a4b4f1f3fcb631643",
        "server_application_traffic_secret_0"
    );

    // Ключ и `iv` выводятся теми же метками. В трейсе набор шифров другой
    // (AES-128), поэтому ключ там шестнадцати байт, — но вывод его тот же самый,
    // и это проверка меток `"key"` и `"iv"`.
    let mut key = [0u8; 16];
    let mut iv = [0u8; 12];
    expand_label(&server_hs, "key", &[], &mut key);
    expand_label(&server_hs, "iv", &[], &mut iv);
    assert_eq!(hex::encode(key), "3fce516009c21727d0f2e4e86ee403bc", "server_write_key");
    assert_eq!(hex::encode(iv), "5d313eb2671276ee13000b30", "server_write_iv");

    // И ключ `finished`, которым подписывается `Finished` сервера.
    let mut finished = [0u8; 32];
    expand_label(&server_hs, "finished", &[], &mut finished);
    assert_eq!(
        hex::encode(finished),
        "008d3b66f816ea559f96b537e885c31fc068bf492c652f01f288a1d8cdc19fc8",
        "finished_key сервера"
    );
}

/// Хеш пустой стенограммы — то, что подставляется в `Derive-Secret(_, _, "")`.
#[test]
fn the_empty_transcript_hash_is_the_published_one() {
    assert_eq!(
        hex::encode(sha256(&[])),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

// --- рукопожатие с чужой реализацией -------------------------------------

/// Поднять `rustls` сервером с самоподписанным корнем и поговорить с ним.
///
/// Сертификат выписывается тут же (`rcgen`), корень кладётся в наше хранилище —
/// ровно так же, как стенд кладёт свой корень гостю. Транспорт — два вектора
/// байтов: сокет здесь не нужен и только добавил бы способов отказать.
#[test]
fn a_handshake_with_rustls_completes_and_carries_data() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::io::{Read, Write};
    use std::sync::Arc;

    // Свой удостоверяющий центр и сертификат сервера, им подписанный.
    let (authority, authority_key) = authority_named("FreeOS test authority");
    let (leaf, leaf_key) = leaf_signed_by("updates.example", &authority, &authority_key);

    let leaf_der = leaf.der().to_vec();
    let root_der = authority.der().to_vec();
    let key_der = leaf_key.serialize_der();

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf_der.clone())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .expect("настройки сервера");
    let mut server =
        rustls::ServerConnection::new(Arc::new(config)).expect("сервер поднимается");

    // Наше хранилище: тот самый корень, в виде PEM.
    let pem = pem_of(&root_der);
    let mut store_buffer = [0u8; 4096];
    let roots =
        x509::store::Store::parse_pem(&pem, &mut store_buffer).expect("корень разбирается");
    assert_eq!(roots.len(), 1);

    let mut buffers = crate::Buffers::new();
    // Случайность в проверке постоянная: воспроизводимый прогон важнее
    // непредсказуемости там, где секрет живёт двадцать миллисекунд.
    let mut random = [0u8; 96];
    for (index, byte) in random.iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(7).wrapping_add(13);
    }
    // Момент внутри срока действия только что выписанного сертификата: `rcgen`
    // ставит его от сегодняшнего дня, поэтому спрашиваем часы у хоста.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("часы хоста идут вперёд")
        .as_secs() as i64;
    let mut session =
        crate::Session::new(&mut buffers, roots, "updates.example", now, &random)
            .expect("ClientHello собирается");

    // Гоняем байты между двумя реализациями, пока обе не договорятся.
    for _ in 0..32 {
        let outgoing = session.outgoing().to_vec();
        if !outgoing.is_empty() {
            server.read_tls(&mut outgoing.as_slice()).expect("сервер читает");
            server.process_new_packets().expect("сервер разбирает");
            session.consume_outgoing(outgoing.len());
        }
        let mut from_server = Vec::new();
        server.write_tls(&mut from_server).expect("сервер пишет");
        if !from_server.is_empty() {
            let mut at = 0usize;
            while at < from_server.len() {
                let used = session.feed(&from_server[at..]).expect("клиент разбирает");
                if used == 0 {
                    break;
                }
                at += used;
            }
        }
        if session.ready() && !server.is_handshaking() {
            break;
        }
    }
    assert!(session.ready(), "рукопожатие обязано закончиться");
    assert!(!server.is_handshaking(), "и на той стороне тоже");

    // Данные приложения в обе стороны.
    session.send(b"GET /index HTTP/1.1\r\n\r\n").expect("запрос шифруется");
    let outgoing = session.outgoing().to_vec();
    server.read_tls(&mut outgoing.as_slice()).expect("сервер читает запрос");
    server.process_new_packets().expect("сервер разбирает запрос");
    session.consume_outgoing(outgoing.len());

    let mut got = vec![0u8; 64];
    let read = server.reader().read(&mut got).expect("сервер видит запрос");
    assert_eq!(&got[..read], b"GET /index HTTP/1.1\r\n\r\n");

    server.writer().write_all(b"HTTP/1.1 200 OK\r\n\r\nhello").expect("сервер отвечает");
    let mut from_server = Vec::new();
    server.write_tls(&mut from_server).expect("сервер пишет ответ");
    let mut at = 0usize;
    while at < from_server.len() {
        let used = session.feed(&from_server[at..]).expect("клиент разбирает ответ");
        if used == 0 {
            break;
        }
        at += used;
    }
    assert_eq!(session.plaintext(), b"HTTP/1.1 200 OK\r\n\r\nhello");
}

/// Тот же сервер, но корень ему не наш: цепочка обязана быть отвергнута.
#[test]
fn a_certificate_from_an_unknown_authority_is_refused() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;

    let (authority, authority_key) = authority_named("somebody else");
    let (leaf, leaf_key) = leaf_signed_by("updates.example", &authority, &authority_key);
    let leaf_der = leaf.der().to_vec();
    let key_der = leaf_key.serialize_der();

    // А в хранилище — совсем другой корень.
    let (stranger, _) = authority_named("FreeOS test authority");
    let pem = pem_of(stranger.der());
    let mut store_buffer = [0u8; 4096];
    let roots = x509::store::Store::parse_pem(&pem, &mut store_buffer).expect("разбирается");

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(leaf_der)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der)),
        )
        .expect("настройки сервера");
    let mut server = rustls::ServerConnection::new(Arc::new(config)).expect("сервер");

    let mut buffers = crate::Buffers::new();
    let random = [0x5Au8; 96];
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let mut session =
        crate::Session::new(&mut buffers, roots, "updates.example", now, &random).unwrap();

    let mut failure = None;
    for _ in 0..32 {
        let outgoing = session.outgoing().to_vec();
        if !outgoing.is_empty() {
            server.read_tls(&mut outgoing.as_slice()).unwrap();
            server.process_new_packets().unwrap();
            session.consume_outgoing(outgoing.len());
        }
        let mut from_server = Vec::new();
        server.write_tls(&mut from_server).unwrap();
        let mut at = 0usize;
        while at < from_server.len() {
            match session.feed(&from_server[at..]) {
                Ok(0) => break,
                Ok(used) => at += used,
                Err(err) => {
                    failure = Some(err);
                    break;
                }
            }
        }
        if failure.is_some() || session.ready() {
            break;
        }
    }
    assert_eq!(
        failure,
        Some(crate::Error::Chain(x509::chain::Error::NoIssuer)),
        "цепочка к чужому корню обязана быть отвергнута"
    );
    assert!(!session.ready());
}

/// Выписать удостоверяющий центр с этим именем.
///
/// `rcgen` — чужой кодировщик, и это часть проверки: сертификат, собранный
/// нашим же кодом, доказывал бы, что наш разбор понимает нашу же запись.
fn authority_named(name: &str) -> (rcgen::Certificate, rcgen::KeyPair) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .expect("ключ создаётся");
    let mut params = rcgen::CertificateParams::new(Vec::new()).expect("параметры");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![rcgen::KeyUsagePurpose::KeyCertSign];
    params.distinguished_name.push(rcgen::DnType::CommonName, name);
    let certificate = params.self_signed(&key).expect("корень подписывает сам себя");
    (certificate, key)
}

/// Выписать сертификат сервера на это имя, подписанный этим центром.
fn leaf_signed_by(
    host: &str,
    authority: &rcgen::Certificate,
    authority_key: &rcgen::KeyPair,
) -> (rcgen::Certificate, rcgen::KeyPair) {
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .expect("ключ создаётся");
    let mut params =
        rcgen::CertificateParams::new(vec![host.into()]).expect("параметры");
    params.distinguished_name.push(rcgen::DnType::CommonName, host);
    params.use_authority_key_identifier_extension = true;
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let certificate = params
        .signed_by(&key, authority, authority_key)
        .expect("сервер подписан корнем");
    (certificate, key)
}

/// Обернуть DER в PEM — ровно так же, как это делает `openssl`.
fn pem_of(der: &[u8]) -> std::string::String {
    use std::fmt::Write as _;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = std::string::String::new();
    for chunk in der.chunks(3) {
        let mut block = [0u8; 3];
        block[..chunk.len()].copy_from_slice(chunk);
        let value = (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[((value >> (18 - 6 * index)) & 0x3F) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    let mut out = std::string::String::from("-----BEGIN CERTIFICATE-----\n");
    for line in encoded.as_bytes().chunks(64) {
        let _ = writeln!(out, "{}", core::str::from_utf8(line).expect("base64 — это ASCII"));
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}
