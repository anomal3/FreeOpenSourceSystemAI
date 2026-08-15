//! Ключи, которыми подписываются образы обновления.
//!
//! # Кто кому доверяет
//!
//! Система принимает обновление, если его подпись сходится с одним из ключей,
//! перечисленных в файле `/os-keys` **её собственного корневого образа**. Файл
//! приезжает вместе с образом и заменяется вместе с ним: так новая версия
//! может принести новый рабочий ключ, а старая об этом ничего заранее знать не
//! обязана. `/etc` для этого не годится — он живёт на разделе состояния и
//! обновлению не принадлежит.
//!
//! # Откуда берётся ключ на машине разработчика
//!
//! Он **не лежит в репозитории**, и это то же решение, что с ключами стенда для
//! SSH: закрытый ключ в открытом репозитории — это закрытый ключ, которого
//! больше нет. Здесь цена ошибки даже выше: обладатель такого ключа подписывает
//! обновление, которое любая FreeOS примет и поставит себе в корень.
//!
//! Пара делается при первом же вызове и живёт в `build/keys/`, то есть в
//! `.gitignore`. Настоящий выпуск подписывается ключом, который Роман держит
//! отдельно от машины сборки; сюда он приезжает тем же файлом.
//!
//! # Почему источник случайности — `ssh-keygen`
//!
//! Потому что своего у `xtask` нет: в стандартной библиотеке Rust генератора
//! случайных чисел не существует, а тянуть ради тридцати двух байт ещё одну
//! зависимость с платформенными ветками незачем — тем более что `ssh-keygen`
//! стенду и так необходим (см. [`super::harness::sshkeys`]).
//!
//! Файл, который он делает, **не является нашим ключом** и как ключ не
//! используется: из него берётся только энтропия — SHA-256 от его содержимого с
//! отдельной приставкой. Отсюда и имя файла: `update-entropy`, а не `id_*`.
//! Разбирать чужой формат ради тех же тридцати двух байт значило бы завязаться
//! на внутренности OpenSSH, которые нам ничего не обещали.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signer, SigningKey};

use crate::paths;

/// Приставка, отделяющая это выведение ключа от любого другого по тому же файлу.
const DOMAIN: &[u8] = b"freeos update signing key v1";

/// Чем подписан образ, который стенд считает годным.
pub fn release() -> Result<SigningKey> {
    key("update-entropy")
}

/// Чужой ключ: им подписывается образ, который система обязана отвергнуть.
///
/// Без него проверка подписи ничего не доказывает: система, принимающая что
/// угодно, проходит проверку «годный образ поставился» ровно так же успешно,
/// как правильная.
pub fn stranger() -> Result<SigningKey> {
    key("stranger-entropy")
}

/// Содержимое `/os-keys`: доверенные открытые ключи, по одному на строку.
///
/// Формат текстовый и с комментарием сверху — файл придётся читать и человеку,
/// который спросит «а чьи обновления берёт эта машина».
pub fn trusted_text() -> Result<String> {
    let working = release()?;
    let mut text = String::new();
    text.push_str("# FreeOS trusted update keys, one per line: ed25519 <64 hex digits> <comment>\n");
    text.push_str("# This file arrives with the system image and is replaced with it, so a new\n");
    text.push_str("# version can bring a new key. Removing every line disables updates.\n");
    text.push_str(&format!(
        "ed25519 {} working\n",
        hex(&working.verifying_key().to_bytes())
    ));
    // Запасного ключа у стенда нет намеренно: он должен лежать офлайн, а
    // положить его сюда значило бы держать оба ключа в одном месте — то есть не
    // иметь запасного вовсе. Настоящий выпуск дописывает вторую строку рукой.
    Ok(text)
}

/// Подписать индекс репозитория тем же рабочим ключом.
///
/// Подписывается **файл целиком**, а не запись в нём: индекс — это утверждение
/// «вот что предлагается сейчас», и подпись по отдельной записи позволила бы
/// собрать из подписанных кусков индекс, которого никто не подписывал.
pub fn sign_index(index: &[u8]) -> Result<[u8; 64]> {
    let key = release()?;
    Ok(key.sign(&osupdate::index::digest(index)).to_bytes())
}

/// Подписать индекс ключом, которого система не знает.
///
/// Только для стенда: им подписывается индекс, который обязан быть отвергнут.
pub fn sign_index_with_stranger(index: &[u8]) -> Result<[u8; 64]> {
    let key = stranger()?;
    Ok(key.sign(&osupdate::index::digest(index)).to_bytes())
}

/// Подписать готовый контейнер: посчитать, что подписывается, и вписать подпись.
pub fn sign(container: &mut [u8], key: &SigningKey) {
    let digest = fpk::build::digest_of(container);
    let signature = key.sign(&digest);
    fpk::build::seal(container, fpk::SIGNATURE_ED25519, &signature.to_bytes());
}

/// Ключ, выведенный из файла-источника; файл делается при первом обращении.
fn key(name: &str) -> Result<SigningKey> {
    let source = ensure_entropy(name)?;
    let raw = std::fs::read(&source)
        .with_context(|| format!("не удалось прочитать {}", source.display()))?;
    let mut hasher = fpk::Hasher::new();
    hasher.update(DOMAIN);
    hasher.update(&raw);
    Ok(SigningKey::from_bytes(&hasher.finish()))
}

/// Сделать файл-источник, если его ещё нет.
fn ensure_entropy(name: &str) -> Result<PathBuf> {
    let dir = paths::build_dir().join("keys");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать каталог {}", dir.display()))?;
    let path = dir.join(name);
    if path.is_file() {
        return Ok(path);
    }
    // `ssh-keygen` откажется писать поверх, а половинка пары от прерванного
    // прогона хуже отсутствующей.
    let public = dir.join(format!("{name}.pub"));
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&public);

    let status = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(format!("freeos-{name}"))
        .arg("-f")
        .arg(&path)
        .arg("-q")
        .status()
        .context("не удалось запустить ssh-keygen; он нужен, чтобы завести ключ подписи")?;
    if !status.success() {
        // Отказ бывает и оттого, что пару в этот же миг завёл кто-то другой:
        // `cargo test` гоняет проверки в несколько потоков, а `ssh-keygen`
        // отказывается писать поверх существующего файла. Появившийся файл —
        // это успех, чей бы он ни был.
        if path.is_file() {
            return Ok(path);
        }
        bail!("ssh-keygen отказался делать пару в {}", path.display());
    }
    println!("подпись: заведён источник ключа {}", path.display());
    Ok(path)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    /// Подписать и проверить — теми же двумя функциями, которыми это делают
    /// подписывающий и ядро.
    ///
    /// Проверяется здесь не Ed25519 (он чужой и проверен), а **то, что обе
    /// стороны считают одно и то же**: подписывающий берёт `digest_of` от
    /// готового контейнера, ядро — `signed_digest` от заголовка и манифеста.
    /// Разойдись они на байт — обновление отвергалось бы с сообщением про
    /// чужой ключ, и искать причину пришлось бы в ключах.
    #[test]
    fn a_signed_container_verifies_with_the_public_half() {
        let mut builder = fpk::build::Builder::new(fpk::Kind::System, "freeos", "0.2");
        builder.blob("image", b"not really an image");
        let mut container = builder.finish();

        let key = super::release().expect("ключ заводится");
        super::sign(&mut container, &key);

        // Дальше — ровно то, что делает ядро.
        let header = fpk::Header::parse(&container).expect("заголовок разбирается");
        assert_eq!(header.signature_algorithm, fpk::SIGNATURE_ED25519);
        assert_eq!(header.signature_len as usize, 64);
        let manifest = &container[fpk::HEADER_SIZE..fpk::HEADER_SIZE + header.manifest_len as usize];
        let digest = fpk::signed_digest(&container[..fpk::HEADER_SIZE], manifest);

        let verifying =
            VerifyingKey::from_bytes(&key.verifying_key().to_bytes()).expect("ключ годный");
        let mut bytes = [0u8; 64];
        bytes.copy_from_slice(&header.signature);
        assert!(verifying.verify(&digest, &Signature::from_bytes(&bytes)).is_ok());

        // Чужой ключ ту же подпись не принимает — иначе проверка пустая.
        let stranger = super::stranger().expect("второй ключ заводится");
        let other = VerifyingKey::from_bytes(&stranger.verifying_key().to_bytes()).unwrap();
        assert!(other.verify(&digest, &Signature::from_bytes(&bytes)).is_err());

        // И правка манифеста ломает подпись: подписано именно его содержимое.
        let mut tampered = container.clone();
        tampered[fpk::HEADER_SIZE] ^= 1;
        let manifest = &tampered[fpk::HEADER_SIZE..fpk::HEADER_SIZE + header.manifest_len as usize];
        let digest = fpk::signed_digest(&tampered[..fpk::HEADER_SIZE], manifest);
        assert!(verifying.verify(&digest, &Signature::from_bytes(&bytes)).is_err());
    }

    /// Открытый ключ в `/os-keys` — тот самый, которым подписано.
    #[test]
    fn the_trusted_file_names_the_signing_key() {
        let text = super::trusted_text().expect("файл собирается");
        let key = super::release().expect("ключ заводится");
        let hex = super::hex(&key.verifying_key().to_bytes());
        assert!(text.contains(&hex), "в /os-keys нет открытой половины ключа");
        assert!(text.starts_with('#'), "файл читает человек, и первая строка — про формат");
    }
}

/// Шестнадцатеричная запись — та, которую читает система.
fn hex(bytes: &[u8]) -> String {
    let digits = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(digits[(byte >> 4) as usize] as char);
        out.push(digits[(byte & 0xF) as usize] as char);
    }
    out
}
