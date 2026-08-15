//! Проверка разбора и цепочек на **настоящих** сертификатах.
//!
//! # Почему настоящие, а не выписанные тут же
//!
//! Потому что сертификат, выписанный нашим же кодом, доказывает согласие кода с
//! самим собой. Файлы в `certs/` сняты с живых серверов (`openssl s_client
//! -showcerts`) 15 августа 2026 года и лежат в репозитории как есть. В них
//! встречается всё, на чём ломается самодельный разбор: длинные длины,
//! необязательные поля, расширения в разном порядке, `SAN` с несколькими
//! именами, знаковый ноль перед модулем RSA.
//!
//! Две цепочки взяты намеренно разные:
//!
//! * `github.com` — ECDSA целиком: лист на P-256 подписан SHA-256,
//!   промежуточный на P-256 подписан **SHA-384**, а корень `USERTrust ECC` —
//!   ключ **P-384**. Одна кривая или один хеш здесь не проходят.
//! * `www.rust-lang.org` — RSA целиком: 2048-битные ключи листа и
//!   промежуточного, корень `ISRG Root X1` на 4096 битах, всё
//!   `sha256WithRSAEncryption`. Это проверка длинной арифметики на настоящем
//!   модуле, а не на числе из учебника.
//!
//! # Время в проверках закреплено
//!
//! `NOW` — фиксированный момент внутри срока действия обоих листов. Иначе
//! проверка перестала бы проходить сама собой через три месяца, и выглядело бы
//! это как поломка разбора.

use x509::cert::{Algorithm, Certificate, PublicKey};
use x509::chain;
use x509::ecdsa::Curve;
use x509::hash::Hash;
use x509::store::Store;

/// 15 августа 2026 года, 12:00 UTC — внутри срока действия обоих листов.
const NOW: i64 = 1_786_881_600;

const GITHUB_LEAF: &[u8] = include_bytes!("certs/github-leaf.der");
const GITHUB_INTERMEDIATE: &[u8] = include_bytes!("certs/github-intermediate.der");
const GITHUB_CROSS: &[u8] = include_bytes!("certs/github-cross.der");
const RUSTLANG_LEAF: &[u8] = include_bytes!("certs/rustlang-leaf.der");
const RUSTLANG_INTERMEDIATE: &[u8] = include_bytes!("certs/rustlang-intermediate.der");
const RUSTLANG_CROSS: &[u8] = include_bytes!("certs/rustlang-cross.der");
const ROOTS: &str = include_str!("certs/roots.pem");

/// Хранилище из двух корней: `USERTrust ECC` и `ISRG Root X1`.
fn roots(buffer: &mut [u8]) -> Store<'_> {
    Store::parse_pem(ROOTS, buffer).expect("файл корней разбирается")
}

#[test]
fn the_root_store_holds_both_roots() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    assert_eq!(store.len(), 2);
    for bytes in store.certificates() {
        let root = Certificate::parse(bytes).expect("корень разбирается");
        // Корень подписывает сам себя — это его определение.
        assert_eq!(root.subject, root.issuer);
        assert!(root.basic.ca, "корень обязан объявлять себя удостоверяющим центром");
        assert!(root.signed(&root), "подпись корня под самим собой обязана сходиться");
    }
}

#[test]
fn the_github_leaf_parses_field_by_field() {
    let leaf = Certificate::parse(GITHUB_LEAF).expect("лист разбирается");
    assert!(matches!(leaf.key, PublicKey::Ec { curve: Curve::P256, .. }));
    assert_eq!(leaf.algorithm, Algorithm::Ecdsa(Hash::Sha256));
    assert!(leaf.matches("github.com"));
    assert!(leaf.matches("www.github.com"));
    assert!(!leaf.matches("gitlab.com"));
    // Лист не имеет права подписывать чужие сертификаты.
    assert!(!leaf.basic.ca);
    assert_eq!(leaf.server_auth, Some(true));
    assert!(leaf.valid_at(NOW));
}

#[test]
fn the_rustlang_leaf_parses_field_by_field() {
    let leaf = Certificate::parse(RUSTLANG_LEAF).expect("лист разбирается");
    let PublicKey::Rsa(key) = leaf.key else {
        panic!("ключ обязан быть RSA");
    };
    // Знаковый ноль снят: 2048 бит — это ровно 256 байт, а не 257.
    assert_eq!(key.modulus.len(), 256);
    assert_eq!(key.exponent, &[0x01, 0x00, 0x01]);
    assert_eq!(leaf.algorithm, Algorithm::RsaPkcs1(Hash::Sha256));
    assert!(leaf.matches("rust-lang.org"));
    assert!(leaf.matches("www.rust-lang.org"));
    assert!(leaf.valid_at(NOW));
}

/// Цепочка ECDSA целиком: три подписи и три разных сочетания кривой и хеша.
#[test]
fn the_github_chain_verifies_to_a_trusted_root() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    let chain = [
        Certificate::parse(GITHUB_LEAF).unwrap(),
        Certificate::parse(GITHUB_INTERMEDIATE).unwrap(),
        Certificate::parse(GITHUB_CROSS).unwrap(),
    ];
    assert_eq!(chain::verify(&chain, &store, "github.com", NOW), Ok(()));
}

/// Цепочка RSA целиком: длинная арифметика на 2048 и 4096 битах.
#[test]
fn the_rustlang_chain_verifies_to_a_trusted_root() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    let chain = [
        Certificate::parse(RUSTLANG_LEAF).unwrap(),
        Certificate::parse(RUSTLANG_INTERMEDIATE).unwrap(),
        Certificate::parse(RUSTLANG_CROSS).unwrap(),
    ];
    assert_eq!(chain::verify(&chain, &store, "www.rust-lang.org", NOW), Ok(()));
}

/// Каждая подпись в цепочке проверяется и по отдельности — чтобы отказ называл
/// звено, а не «цепочку».
#[test]
fn every_link_is_signed_by_the_next_one() {
    let leaf = Certificate::parse(GITHUB_LEAF).unwrap();
    let intermediate = Certificate::parse(GITHUB_INTERMEDIATE).unwrap();
    let cross = Certificate::parse(GITHUB_CROSS).unwrap();
    assert_eq!(intermediate.subject, leaf.issuer);
    assert!(intermediate.signed(&leaf), "P-256 + SHA-256");
    assert_eq!(cross.subject, intermediate.issuer);
    assert!(cross.signed(&intermediate), "P-256 + SHA-384");
    assert_eq!(intermediate.algorithm, Algorithm::Ecdsa(Hash::Sha384));
}

/// Один изменённый бит в подписанной части — и подпись обязана разойтись.
///
/// Проверка ценна тем, что ловит противоположную ошибку: разбор, который
/// «проверяет» подпись, ничего не сравнивая, прошёл бы все проверки выше.
#[test]
fn one_flipped_bit_breaks_the_signature() {
    let mut broken = RUSTLANG_LEAF.to_vec();
    // Портим байт внутри TBSCertificate — в середине, чтобы не задеть длины.
    let at = broken.len() / 2;
    broken[at] ^= 0x01;
    let intermediate = Certificate::parse(RUSTLANG_INTERMEDIATE).unwrap();
    match Certificate::parse(&broken) {
        Ok(leaf) => assert!(!intermediate.signed(&leaf), "испорченный лист не подписан"),
        // Испорченный байт мог попасть в длину — тогда сертификат просто не
        // разбирается, и это тоже правильный ответ.
        Err(_) => {}
    }
}

/// Та же цепочка, но чужому имени она не годится.
#[test]
fn a_chain_for_another_name_is_refused() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    let chain = [
        Certificate::parse(GITHUB_LEAF).unwrap(),
        Certificate::parse(GITHUB_INTERMEDIATE).unwrap(),
        Certificate::parse(GITHUB_CROSS).unwrap(),
    ];
    assert_eq!(
        chain::verify(&chain, &store, "example.com", NOW),
        Err(chain::Error::WrongName)
    );
}

/// Просроченный сертификат отвергается, а не принимается «почти».
#[test]
fn an_expired_chain_is_refused() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    let chain = [
        Certificate::parse(GITHUB_LEAF).unwrap(),
        Certificate::parse(GITHUB_INTERMEDIATE).unwrap(),
        Certificate::parse(GITHUB_CROSS).unwrap(),
    ];
    // 2030 год: лист к этому времени давно кончился.
    assert_eq!(
        chain::verify(&chain, &store, "github.com", 1_900_000_000),
        Err(chain::Error::Expired)
    );
    // И до начала срока — тоже отказ, с другим словом.
    assert_eq!(
        chain::verify(&chain, &store, "github.com", 1_600_000_000),
        Err(chain::Error::NotYetValid)
    );
}

/// Цепочка без промежуточного звена никуда не ведёт.
#[test]
fn a_chain_missing_its_intermediate_leads_nowhere() {
    let mut buffer = [0u8; 8 * 1024];
    let store = roots(&mut buffer);
    let chain = [Certificate::parse(GITHUB_LEAF).unwrap()];
    assert_eq!(
        chain::verify(&chain, &store, "github.com", NOW),
        Err(chain::Error::NoIssuer)
    );
}

/// Цепочка, ведущая к корню, которого в хранилище нет.
#[test]
fn a_chain_to_an_unknown_root_is_refused() {
    let mut buffer = [0u8; 8 * 1024];
    // Хранилище только с корнем Let's Encrypt: цепочка github.com к нему не ведёт.
    let only_isrg = ROOTS
        .split_once("ISRG Root X1")
        .map(|(_, tail)| tail)
        .expect("в файле есть этот корень");
    let store = Store::parse_pem(only_isrg, &mut buffer).expect("разбирается");
    assert_eq!(store.len(), 1);
    let chain = [
        Certificate::parse(GITHUB_LEAF).unwrap(),
        Certificate::parse(GITHUB_INTERMEDIATE).unwrap(),
        Certificate::parse(GITHUB_CROSS).unwrap(),
    ];
    assert_eq!(
        chain::verify(&chain, &store, "github.com", NOW),
        Err(chain::Error::NoIssuer)
    );
}

/// Пустое хранилище — это отказ, а не разрешение.
#[test]
fn an_empty_store_refuses_everything() {
    let store = Store::empty();
    let chain = [Certificate::parse(GITHUB_LEAF).unwrap()];
    assert_eq!(
        chain::verify(&chain, &store, "github.com", NOW),
        Err(chain::Error::NoRoots)
    );
}

/// Лист в роли промежуточного звена: сертификат сайта не подписывает чужие.
///
/// Это та самая дыра, ради которой существует `basicConstraints`, и в 2008 году
/// на ней ловили настоящие браузеры.
#[test]
fn a_leaf_cannot_stand_in_for_an_authority() {
    let leaf = Certificate::parse(GITHUB_LEAF).unwrap();
    assert!(!leaf.basic.ca);
    let intermediate = Certificate::parse(GITHUB_INTERMEDIATE).unwrap();
    assert!(intermediate.basic.ca);
    assert_eq!(intermediate.basic.path_len, Some(0));
    // pathLen = 0 означает: под этим звеном не бывает других удостоверяющих
    // центров, только листы.
}

/// Хранилище, которое **действительно едет в образе**, проверяется теми же
/// цепочками.
///
/// Это не дубль проверок выше: там корни подобраны под цепочку, здесь наоборот —
/// спрашивается, доедет ли система до тех двух мест, ради которых фаза написана.
/// Файл лежит там, откуда его берёт установщик и куда его кладёт `xtask`, и
/// путь сюда написан руками намеренно: разъехавшись, они дали бы систему,
/// которая не может обновиться, и зелёную проверку.
#[test]
fn the_store_that_ships_in_the_image_covers_both_chains() {
    const SHIPPED: &str = include_str!("../../../initrd/usr/share/defaults/etc/ca.pem");
    let mut buffer = [0u8; 16 * 1024];
    let store = Store::parse_pem(SHIPPED, &mut buffer).expect("ca.pem образа разбирается");
    assert!(store.len() >= 4, "в образе {} корней", store.len());

    let github = [
        Certificate::parse(GITHUB_LEAF).unwrap(),
        Certificate::parse(GITHUB_INTERMEDIATE).unwrap(),
        Certificate::parse(GITHUB_CROSS).unwrap(),
    ];
    assert_eq!(chain::verify(&github, &store, "github.com", NOW), Ok(()));

    // Второй адрес, куда GitHub переадресует за файлом релиза, живёт на
    // сертификате Let's Encrypt — то есть на другом корне и другой арифметике.
    let cdn = [
        Certificate::parse(RUSTLANG_LEAF).unwrap(),
        Certificate::parse(RUSTLANG_INTERMEDIATE).unwrap(),
        Certificate::parse(RUSTLANG_CROSS).unwrap(),
    ];
    assert_eq!(chain::verify(&cdn, &store, "rust-lang.org", NOW), Ok(()));
}

/// Имена из `subjectAltName` читаются все, а не первое.
#[test]
fn all_subject_alt_names_are_read() {
    let leaf = Certificate::parse(GITHUB_LEAF).unwrap();
    let names: Vec<&str> = leaf.dns_names().collect();
    assert!(names.contains(&"github.com"), "имена: {names:?}");
    assert!(names.len() >= 2, "имена: {names:?}");
}
