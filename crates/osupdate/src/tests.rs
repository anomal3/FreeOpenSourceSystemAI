//! Проверки формата на хосте.
//!
//! Проверяется здесь не Ed25519 — он чужой и проверен, — а **разбор**: то, что
//! написанное сборщиком читается читателем, и то, что подделки отвергаются. Обе
//! ошибки этого рода выглядят на машине одинаково («обновление не встало») и
//! ищутся в подписи, а не в разборе текста.

use crate::drivers::{self, Catalogue};
use crate::index::{self, Index};
use crate::keys::Trusted;

/// Индекс, собранный сборщиком, разбирается разбором.
///
/// Смысл именно в паре: два места пишут и читают один формат, и разойтись они
/// могут молча — на машине это выглядело бы как «сервер отдаёт мусор».
#[test]
fn what_the_builder_writes_the_reader_reads() {
    let text = index::build::render(&[
        index::build::Offer {
            version: "0.3",
            arch: "x86_64",
            file: "freeos-0.3-x86_64.fpk",
            size: 25_165_824,
            sha256: [0x11; 32],
        },
        index::build::Offer {
            version: "0.3",
            arch: "aarch64",
            file: "freeos-0.3-aarch64.fpk",
            size: 25_165_800,
            sha256: [0x22; 32],
        },
    ]);

    let index = Index::parse(&text).expect("индекс разбирается");
    let image = index.image("aarch64").expect("запись для aarch64 есть");
    assert_eq!(image.version, "0.3");
    assert_eq!(image.file, "freeos-0.3-aarch64.fpk");
    assert_eq!(image.size, 25_165_800);
    assert_eq!(image.sha256, [0x22; 32]);

    // Запись первой архитектуры не перетекла во вторую полями.
    let first = index.image("x86_64").expect("запись для x86_64 есть");
    assert_eq!(first.file, "freeos-0.3-x86_64.fpk");
    assert_eq!(first.sha256, [0x11; 32]);

    // Архитектуры, которой в индексе нет, не находится — а не подставляется
    // первая попавшаяся.
    assert!(matches!(index.image("riscv64"), Err(index::Error::NoImage)));
}

/// Индекс более нового формата отвергается **своим** отказом.
///
/// Не «файл испорчен»: он не испорчен, он новее. Человеку это говорит
/// «обновитесь иначе», а «испорчен» отправило бы его чинить сервер.
#[test]
fn a_newer_format_is_refused_by_name() {
    let text = "format=2\n[image]\nversion=9\n";
    assert!(matches!(Index::parse(text), Err(index::Error::Format(2))));

    let text = "just some text from a captive portal\n";
    assert!(matches!(Index::parse(text), Err(index::Error::NoFormat)));
}

/// Имя файла с путём внутри отвергается.
///
/// Оно приходит из сети и превращается в путь у нас: `../../os-keys` в этом
/// поле означал бы, что индекс волен назвать любой файл на машине.
#[test]
fn a_file_name_with_a_path_in_it_is_refused() {
    let text = "format=1\n[image]\nversion=1\narch=x86_64\nfile=../os-keys\nsize=1\nsha256=00\n";
    let index = Index::parse(text).expect("заголовок разбирается");
    assert!(matches!(index.image("x86_64"), Err(index::Error::Field(_))));
}

/// Подпись индекса проверяется тем же ключом, что стоит в `/os-keys`.
///
/// И **не** проверяется чужим: проверка, которую проходит кто угодно, ничего не
/// доказывает.
#[test]
fn the_index_signature_checks_out_against_the_trusted_file() {
    use ed25519_dalek::{Signer, SigningKey};

    let key = SigningKey::from_bytes(&[7u8; 32]);
    let stranger = SigningKey::from_bytes(&[9u8; 32]);
    let text = index::build::render(&[index::build::Offer {
        version: "0.3",
        arch: "x86_64",
        file: "freeos-0.3-x86_64.fpk",
        size: 16,
        sha256: [0; 32],
    }]);

    let digest = index::digest(text.as_bytes());
    let signature = key.sign(&digest);
    let sig_text = index::build::render_signature(&signature.to_bytes());
    let parsed = index::parse_signature(&sig_text).expect("подпись разбирается");
    assert_eq!(parsed, signature.to_bytes());

    let trusted = Trusted::parse(&alloc::format!(
        "# comment\ned25519 {} working\n",
        crate::to_hex(&key.verifying_key().to_bytes())
    ));
    assert_eq!(trusted.len(), 1);
    assert!(trusted.verifies(&digest, &parsed));

    // Тот же индекс, подписанный другим ключом, не принимается.
    let forged = stranger.sign(&digest);
    assert!(!trusted.verifies(&digest, &forged.to_bytes()));

    // И правка индекса ломает подпись: подписан файл, а не запись в нём.
    let tampered = text.replace("version=0.3", "version=9.9");
    assert!(!trusted.verifies(&index::digest(tampered.as_bytes()), &parsed));
}

/// Список без ключей — это отказ, а не разрешение.
#[test]
fn an_empty_key_file_trusts_nobody() {
    let trusted = Trusted::parse("# nothing here\n\n");
    assert!(trusted.is_empty());
    assert!(!trusted.verifies(&[0u8; 32], &[0u8; 64]));

    // Ключ чужого вида пропускается, а не роняет разбор — как в
    // `authorized_keys`.
    let trusted = Trusted::parse("rsa AAAA...\ned25519 0011 short\n");
    assert!(trusted.is_empty());
}

/// Ключей больше, чем помещается, — и об этом можно сказать вслух.
#[test]
fn keys_beyond_the_limit_are_counted_not_hidden() {
    let mut text = alloc::string::String::new();
    for index in 0..(crate::keys::MAX_KEYS + 2) {
        text.push_str(&alloc::format!(
            "ed25519 {} key-{index}\n",
            crate::to_hex(&[index as u8; 32])
        ));
    }
    let trusted = Trusted::parse(&text);
    assert_eq!(trusted.len(), crate::keys::MAX_KEYS);
    assert_eq!(trusted.dropped(), 2);
}

/// Каталог драйверов, собранный сборщиком, разбирается разбором, и запись
/// находится по устройству **и** архитектуре — не по чему-то одному.
#[test]
fn the_driver_catalogue_finds_by_device_and_arch() {
    let offer = |arch, file, byte| drivers::build::Offer {
        drives: "1234:11e8 1234:11e9",
        arch,
        package: "edu",
        version: "1.0",
        file,
        size: 4096,
        sha256: [byte; 32],
    };
    let text = drivers::build::render(&[
        offer("x86_64", "edu-1.0-x86_64.fpk", 0x11),
        offer("aarch64", "edu-1.0-aarch64.fpk", 0x22),
    ]);
    let catalogue = Catalogue::parse(&text).expect("каталог разбирается");

    let found = catalogue.find(0x1234, 0x11e8, "aarch64").expect("драйвер есть");
    assert_eq!(found.package, "edu");
    assert_eq!(found.file, "edu-1.0-aarch64.fpk");
    assert_eq!(found.sha256, [0x22; 32]);
    // Второе устройство той же записи.
    assert_eq!(catalogue.find(0x1234, 0x11e9, "x86_64").expect("и второе").sha256, [0x11; 32]);
    // Чужого устройства и чужой архитектуры нет — а не первая попавшаяся запись.
    assert_eq!(catalogue.find(0x8086, 0x100e, "x86_64").unwrap_err(), drivers::Error::NoDriver);
    assert_eq!(catalogue.find(0x1234, 0x11e8, "riscv64").unwrap_err(), drivers::Error::NoDriver);
}

/// Каталог с путём в имени файла и каталог чужого формата отвергаются.
#[test]
fn a_driver_catalogue_with_a_path_or_a_newer_format_is_refused() {
    let text = "format=1
[driver]
drives=1234:11e8
arch=x86_64
package=edu
version=1
                file=../os-keys
size=1
sha256=00
";
    let catalogue = Catalogue::parse(text).expect("заголовок разбирается");
    assert!(matches!(catalogue.find(0x1234, 0x11e8, "x86_64"), Err(drivers::Error::Field(_))));

    assert_eq!(Catalogue::parse("format=2
").unwrap_err(), drivers::Error::Format(2));
    assert_eq!(Catalogue::parse("<html>
").unwrap_err(), drivers::Error::NoFormat);
    // Индекс обновлений каталогом не притворится: заголовок тот же, а записей
    // `[driver]` в нём нет.
    let index_text = index::build::render(&[index::build::Offer {
        version: "0.3",
        arch: "x86_64",
        file: "freeos-0.3-x86_64.fpk",
        size: 16,
        sha256: [0; 32],
    }]);
    let catalogue = Catalogue::parse(&index_text).expect("заголовок общий");
    assert_eq!(catalogue.find(0x1234, 0x11e8, "x86_64").unwrap_err(), drivers::Error::NoDriver);
}

/// Подпись индекса не годится как подпись каталога, даже по тем же байтам.
///
/// Ради этого у каталога своя приставка: без неё подписанный индекс,
/// выложенный под именем `drivers`, проходил бы проверку подписи.
#[test]
fn an_index_signature_does_not_sign_a_catalogue() {
    use ed25519_dalek::{Signer, SigningKey};

    let key = SigningKey::from_bytes(&[7u8; 32]);
    let trusted = Trusted::parse(&alloc::format!(
        "ed25519 {} working
",
        crate::to_hex(&key.verifying_key().to_bytes())
    ));
    let bytes = b"format=1
";
    let as_index = key.sign(&index::digest(bytes)).to_bytes();
    assert!(trusted.verifies(&index::digest(bytes), &as_index));
    assert!(!trusted.verifies(&drivers::digest(bytes), &as_index));

    let as_catalogue = key.sign(&drivers::digest(bytes)).to_bytes();
    let text = drivers::build::render_signature(&as_catalogue);
    let parsed = index::parse_signature(&text).expect("строка подписи общая");
    assert!(trusted.verifies(&drivers::digest(bytes), &parsed));
}
