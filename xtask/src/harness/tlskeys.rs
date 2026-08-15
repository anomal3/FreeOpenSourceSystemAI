//! Сертификаты, которыми стенд поднимает HTTPS-сервер обновлений.
//!
//! # Почему их два комплекта
//!
//! Потому что проверка «мы соединились по HTTPS» ничего не доказывает сама по
//! себе: клиент, принимающий любой сертификат, проходит её так же успешно, как
//! правильный. Поэтому стенд поднимает **два** сервера — один с сертификатом от
//! корня, который лежит у гостя в `/etc/ca.pem`, другой от корня, которого гость
//! не знает вовсе, — и сценарий требует, чтобы второй был отвергнут, а первый
//! принят. Тот же приём, что у SSH с «чужим ключом» (см. [`super::sshkeys`]).
//!
//! # Почему сертификат на адрес, а не на имя
//!
//! Потому что гость видит хост как `10.0.2.2` и разрешать имена ему негде: DNS
//! в пользовательской сети QEMU есть, но запись `updates.freeos.test` в нём не
//! появится. Значит имя в сертификате — это `iPAddress`, а не `dNSName`, и
//! проверять его надо соответствующим полем `subjectAltName`. Ровно это и
//! проверяется здесь: подстановка `dNSName = 10.0.2.2` не подошла бы.
//!
//! # Почему они переживают прогоны
//!
//! По той же причине, что и ключи SSH: корень уезжает гостю на раздел
//! состояния, а `ext2::Editor` перезаписывать не умеет. Новый корень на каждом
//! прогоне означал бы гостя, у которого лежит вчерашний.

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::paths;

/// Адрес, на который выписывается сертификат: так гость видит хост.
pub const HOST_ADDRESS: [u8; 4] = [10, 0, 2, 2];

/// Готовый комплект: корень в PEM и цепочка с ключом для сервера.
pub struct Material {
    /// Корень в PEM — то, что кладётся гостю в `/etc/ca.pem`.
    pub root_pem: String,
    /// Сертификат сервера в DER.
    pub leaf_der: Vec<u8>,
    /// Закрытый ключ сервера в PKCS#8 DER.
    pub key_der: Vec<u8>,
}

/// Комплект, которому гость доверяет.
pub fn trusted() -> Result<Material> {
    ensure("tls-trusted", "FreeOS harness authority")
}

/// Комплект от корня, которого гость не знает.
pub fn stranger() -> Result<Material> {
    ensure("tls-stranger", "Somebody else entirely")
}

/// Сделать комплект, если его ещё нет, и прочитать с диска.
fn ensure(prefix: &str, authority: &str) -> Result<Material> {
    let dir = paths::test_dir();
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("не удалось создать {}", dir.display()))?;
    let root_path: PathBuf = dir.join(format!("{prefix}-root.pem"));
    let leaf_path: PathBuf = dir.join(format!("{prefix}-leaf.der"));
    let key_path: PathBuf = dir.join(format!("{prefix}-leaf.key"));

    if !(root_path.is_file() && leaf_path.is_file() && key_path.is_file()) {
        let made = issue(authority)?;
        std::fs::write(&root_path, made.root_pem.as_bytes())
            .with_context(|| format!("не удалось записать {}", root_path.display()))?;
        std::fs::write(&leaf_path, &made.leaf_der)
            .with_context(|| format!("не удалось записать {}", leaf_path.display()))?;
        std::fs::write(&key_path, &made.key_der)
            .with_context(|| format!("не удалось записать {}", key_path.display()))?;
        println!("стенд: выписан комплект TLS {prefix} в {}", dir.display());
    }

    Ok(Material {
        root_pem: std::fs::read_to_string(&root_path)
            .with_context(|| format!("не удалось прочитать {}", root_path.display()))?,
        leaf_der: std::fs::read(&leaf_path)
            .with_context(|| format!("не удалось прочитать {}", leaf_path.display()))?,
        key_der: std::fs::read(&key_path)
            .with_context(|| format!("не удалось прочитать {}", key_path.display()))?,
    })
}

/// Выписать корень и сертификат сервера на адрес хоста.
fn issue(authority: &str) -> Result<Material> {
    use rcgen::{
        BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256, SanType,
    };

    let root_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .context("не удалось сделать ключ корня")?;
    let mut root_params =
        CertificateParams::new(Vec::new()).context("параметры корня не собираются")?;
    root_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    root_params.distinguished_name.push(DnType::CommonName, authority);
    let root = root_params.self_signed(&root_key).context("корень не подписывается")?;

    let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256)
        .context("не удалось сделать ключ сервера")?;
    let mut leaf_params =
        CertificateParams::new(Vec::new()).context("параметры сервера не собираются")?;
    // Имя сервера — адрес. `SanType::IpAddress` пишет `[7] iPAddress`, а не
    // `[2] dNSName`, и клиент, подключающийся по адресу, обязан смотреть именно
    // туда.
    leaf_params.subject_alt_names =
        vec![SanType::IpAddress(std::net::IpAddr::from(HOST_ADDRESS))];
    leaf_params.distinguished_name.push(DnType::CommonName, "FreeOS harness update server");
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.use_authority_key_identifier_extension = true;
    let leaf = leaf_params
        .signed_by(&leaf_key, &root, &root_key)
        .context("сертификат сервера не подписывается")?;

    Ok(Material {
        root_pem: root.pem(),
        leaf_der: leaf.der().to_vec(),
        key_der: leaf_key.serialize_der(),
    })
}
