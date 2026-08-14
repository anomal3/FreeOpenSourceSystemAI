//! Сборка контейнера. Нужна одному `xtask` — и потому за флагом сборки.
//!
//! Разбор обходится без кучи, а сборка без неё обошлась бы только ценой
//! двухпроходного API («сначала спросите размер, потом дайте буфер»), который
//! понадобился бы ровно одному вызывающему, работающему на хосте с полной
//! стандартной библиотекой. Держать формат в одном месте важнее.

use alloc::string::String;
use alloc::vec::Vec;

use crate::{FORMAT_VERSION, HEADER_SIZE, Kind, MAGIC, SIGNATURE_SIZE, crc32};

/// Один файл, который уедет в пакет.
pub struct Entry {
    /// Путь внутри пакета: относительный, без `..`.
    pub path: String,
    /// Права в unix-нотации.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub data: Vec<u8>,
}

/// Собираемый контейнер.
pub struct Builder {
    kind: Kind,
    name: String,
    version: String,
    requires: Vec<String>,
    /// Строки манифеста сверх обязательных — в том порядке, в каком их добавили.
    extra: Vec<String>,
    payload: Vec<u8>,
    /// Строки `file=` и `<ключ>=`, ссылающиеся на уже уложенную нагрузку.
    placed: Vec<String>,
}

impl Builder {
    #[must_use]
    pub fn new(kind: Kind, name: &str, version: &str) -> Self {
        Self {
            kind,
            name: String::from(name),
            version: String::from(version),
            requires: Vec::new(),
            extra: Vec::new(),
            payload: Vec::new(),
            placed: Vec::new(),
        }
    }

    /// Объявить зависимость по имени.
    pub fn requires(&mut self, name: &str) -> &mut Self {
        self.requires.push(String::from(name));
        self
    }

    /// Дописать произвольную строку `ключ=значение` в манифест.
    pub fn field(&mut self, key: &str, value: &str) -> &mut Self {
        self.extra.push(alloc::format!("{key}={value}"));
        self
    }

    /// Уложить файл в нагрузку и описать его в манифесте.
    ///
    /// # Паника
    ///
    /// Паникует на пути, который вывел бы установку за пределы каталога пакета.
    /// Это `xtask`, то есть код сборки на хосте: собрать пакет с таким путём
    /// нельзя, и превращать ошибку сборщика в `Result`, который никто не
    /// обработает, значило бы спрятать её.
    pub fn file(&mut self, entry: &Entry) -> &mut Self {
        assert!(
            crate::is_safe_path(&entry.path),
            "путь внутри пакета обязан быть относительным и без '..': {}",
            entry.path
        );
        let offset = self.payload.len() as u64;
        self.payload.extend_from_slice(&entry.data);
        self.placed.push(alloc::format!(
            "file={:o} {} {} {offset} {} {:08x} {}",
            entry.mode & 0o777,
            entry.uid,
            entry.gid,
            entry.data.len(),
            crc32(&entry.data),
            entry.path,
        ));
        self
    }

    /// Уложить именованный кусок: образ корня, ядро, initrd.
    ///
    /// Хеш куска пишется **всегда**, а не только у подписываемого контейнера:
    /// шестьдесят четыре знака в манифесте стоят ничего, а собранный без хеша
    /// контейнер нельзя было бы подписать задним числом, не пересобрав его.
    pub fn blob(&mut self, key: &str, data: &[u8]) -> &mut Self {
        let offset = self.payload.len() as u64;
        self.payload.extend_from_slice(data);
        let mut hasher = crate::Hasher::new();
        hasher.update(data);
        self.placed.push(alloc::format!(
            "{key}={offset} {} {:08x} {}",
            data.len(),
            crc32(data),
            hex(&hasher.finish()),
        ));
        self
    }

    /// Собрать контейнер целиком.
    #[must_use]
    pub fn finish(&self) -> Vec<u8> {
        let mut manifest = String::new();
        manifest.push_str("# FreeOS package manifest\n");
        manifest.push_str(&alloc::format!("name={}\n", self.name));
        manifest.push_str(&alloc::format!("version={}\n", self.version));
        manifest.push_str(&alloc::format!("kind={}\n", self.kind.tag()));
        if !self.requires.is_empty() {
            manifest.push_str(&alloc::format!("requires={}\n", self.requires.join(" ")));
        }
        for line in &self.extra {
            manifest.push_str(line);
            manifest.push('\n');
        }
        for line in &self.placed {
            manifest.push_str(line);
            manifest.push('\n');
        }

        let manifest = manifest.into_bytes();
        let mut out = Vec::with_capacity(HEADER_SIZE + manifest.len() + self.payload.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.kind.code().to_le_bytes());
        out.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        out.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u64).to_le_bytes());
        out.extend_from_slice(&crc32(&manifest).to_le_bytes());
        out.extend_from_slice(&crc32(&self.payload).to_le_bytes());
        // Алгоритм подписи и её длина — нули: подписывает не сборщик, а тот, у
        // кого есть ключ, и делает он это отдельным шагом (`sign`). Место под
        // подпись занято всегда, см. заголовок крейта.
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.resize(crate::SIGNATURE_OFFSET + SIGNATURE_SIZE, 0);
        out.resize(HEADER_SIZE, 0);
        out.extend_from_slice(&manifest);
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Шестнадцатеричная запись — та, которую читает разбор манифеста.
fn hex(bytes: &[u8]) -> String {
    let digits = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(digits[(byte >> 4) as usize] as char);
        out.push(digits[(byte & 0xF) as usize] as char);
    }
    out
}

/// Что подписывать в готовом контейнере и куда класть подпись.
///
/// Возвращает то, что надо подписать (SHA-256 заголовка и манифеста), — сам
/// ключ сюда не приезжает: подписывающий живёт на хосте, а этот крейт собирается
/// и для ядра, которому закрытые ключи не нужны и не должны быть доступны.
///
/// # Паника
///
/// Паникует на контейнере короче заголовка: это сборка на хосте, и такой
/// контейнер собрал бы только сам сборщик — то есть это его ошибка, а не входные
/// данные.
#[must_use]
pub fn digest_of(container: &[u8]) -> [u8; 32] {
    assert!(container.len() >= HEADER_SIZE, "контейнер короче заголовка");
    let manifest_len = u32::from_le_bytes([
        container[12],
        container[13],
        container[14],
        container[15],
    ]) as usize;
    crate::signed_digest(
        &container[..HEADER_SIZE],
        &container[HEADER_SIZE..HEADER_SIZE + manifest_len],
    )
}

/// Вписать подпись в готовый контейнер.
pub fn seal(container: &mut [u8], algorithm: u16, signature: &[u8]) {
    assert!(container.len() >= HEADER_SIZE, "контейнер короче заголовка");
    assert!(signature.len() <= SIGNATURE_SIZE, "подпись длиннее отведённого места");
    container[32..34].copy_from_slice(&algorithm.to_le_bytes());
    container[34..36].copy_from_slice(&(signature.len() as u16).to_le_bytes());
    container[crate::SIGNATURE_OFFSET..crate::SIGNATURE_OFFSET + signature.len()]
        .copy_from_slice(signature);
}
