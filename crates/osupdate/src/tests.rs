//! Проверки формата на хосте.
//!
//! Проверяется здесь не Ed25519 — он чужой и проверен, — а **разбор**: то, что
//! написанное сборщиком читается читателем, и то, что подделки отвергаются. Обе
//! ошибки этого рода выглядят на машине одинаково («обновление не встало») и
//! ищутся в подписи, а не в разборе текста.

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
