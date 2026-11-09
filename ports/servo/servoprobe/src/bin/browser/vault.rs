//! Хранилище секретов OBSOS: пароли сайтов и учётные записи синхронизации.
//!
//! Шифр — XChaCha20-Poly1305, ключ — Argon2id. Два ключа:
//! * **ключ хранилища** (`vault.json`: учётные записи синхронизации, ключ
//!   синхронизации): из мастер-пароля, если он задан, иначе случайный ключ
//!   устройства в `device.key` (тогда защита — права на файлы профиля; об
//!   этом честно сказано в настройках);
//! * **ключ синхронизации** паролей: из «пароля синхронизации», одинакового
//!   на всех устройствах. Пароли сайтов лежат в коллекции `passwords`
//!   реплики (как закладки) — но и ключ записи, и значение там зашифрованы
//!   этим ключом: площадка видит только шум. Пока пароль синхронизации не
//!   задан, ключ случайный и свой у устройства — пароли синхронизируются, но
//!   прочитать их может только оно само.

use std::path::{Path, PathBuf};

use argon2::{Algorithm, Argon2, Params, Version};
use browser_sync::json::{self, Value};
use browser_sync::remote::{Account, b64_decode, b64_encode};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use sha2::{Digest, Sha256};

pub type Key = [u8; 32];

pub fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("getrandom");
    out
}

/// Ключ из пароля: Argon2id, 19 МиБ, 2 прохода (минимум OWASP 2024).
pub fn derive(password: &str, salt: &[u8]) -> Key {
    let params = Params::new(19 * 1024, 2, 1, Some(32)).expect("argon2 params");
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .expect("argon2");
    out
}

/// Зашифровать: base64(nonce 24 байта ‖ шифротекст с меткой).
pub fn seal(key: &Key, plain: &[u8]) -> String {
    let cipher = XChaCha20Poly1305::new_from_slice(key).expect("key 32");
    let nonce: [u8; 24] = random();
    let mut out = nonce.to_vec();
    out.extend(cipher.encrypt(&XNonce::from(nonce), plain).expect("encrypt"));
    b64_encode(&out)
}

pub fn open(key: &Key, sealed: &str) -> Option<Vec<u8>> {
    let raw = b64_decode(sealed)?;
    if raw.len() < 24 + 16 {
        return None;
    }
    let cipher = XChaCha20Poly1305::new_from_slice(key).ok()?;
    let nonce: [u8; 24] = raw[..24].try_into().ok()?;
    cipher.decrypt(&XNonce::from(nonce), &raw[24..]).ok()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok()).collect()
}

/// Сохранённый вход на сайт.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    /// `https://example.com` (схема + хост + порт).
    pub origin: String,
    pub username: String,
    pub password: String,
    pub updated_ms: u64,
}

impl Login {
    fn to_json(&self) -> String {
        let s = |v: &str| Value::Str(v.to_string());
        Value::Obj(vec![
            ("origin".into(), s(&self.origin)),
            ("username".into(), s(&self.username)),
            ("password".into(), s(&self.password)),
            ("updated_ms".into(), Value::Num(self.updated_ms.to_string())),
        ])
        .to_text()
    }

    fn from_json(text: &str) -> Option<Login> {
        let v = json::parse(text).ok()?;
        Some(Login {
            origin: v.str_field("origin")?.to_string(),
            username: v.str_field("username").unwrap_or("").to_string(),
            password: v.str_field("password")?.to_string(),
            updated_ms: match v.get("updated_ms") {
                Some(Value::Num(n)) => n.parse().unwrap_or(0),
                _ => 0,
            },
        })
    }

    /// Ключ записи в коллекции `passwords`: SHA-256(ключ ‖ сайт ‖ имя) —
    /// по нему нельзя узнать сайт, но одна и та же пара на двух устройствах
    /// попадает в одну запись.
    pub fn record_key(&self, key: &Key) -> String {
        let mut h = Sha256::new();
        h.update(key);
        h.update(self.origin.as_bytes());
        h.update([0u8]);
        h.update(self.username.as_bytes());
        hex(&h.finalize()[..16])
    }

    pub fn seal(&self, key: &Key) -> String {
        seal(key, self.to_json().as_bytes())
    }

    pub fn open(key: &Key, sealed: &str) -> Option<Login> {
        Login::from_json(&String::from_utf8(open(key, sealed)?).ok()?)
    }
}

/// Открытое хранилище.
pub struct Vault {
    dir: PathBuf,
    /// Ключ, которым зашифрован `vault.json`.
    key: Key,
    /// Соль Argon2id мастер-пароля; `None` — ключ устройства.
    master_salt: Option<Vec<u8>>,
    pub accounts: Vec<Account>,
    /// Активная учётная запись синхронизации (индекс), `None` — выключено.
    pub active: Option<usize>,
    /// Ключ синхронизации паролей и признак «задан паролем синхронизации».
    pub sync_key: Key,
    pub sync_key_shared: bool,
}

pub enum OpenError {
    /// Нужен мастер-пароль (или он не подошёл).
    NeedMaster,
    Broken(String),
}

impl Vault {
    fn vault_path(dir: &Path) -> PathBuf {
        dir.join("vault.json")
    }


    fn device_key(dir: &Path) -> Key {
        let path = dir.join("device.key");
        if let Some(key) = std::fs::read_to_string(&path).ok().and_then(|t| unhex(t.trim())).and_then(|b| b.try_into().ok()) {
            return key;
        }
        let key: Key = random();
        let _ = std::fs::write(&path, hex(&key));
        key
    }

    /// Открыть (или создать пустое). `master` — мастер-пароль, если задан.
    pub fn open(dir: &Path, master: Option<&str>) -> Result<Vault, OpenError> {
        let path = Self::vault_path(dir);
        let Ok(text) = std::fs::read_to_string(&path) else {
            let vault = Vault {
                dir: dir.to_path_buf(),
                key: Self::device_key(dir),
                master_salt: None,
                accounts: Vec::new(),
                active: None,
                sync_key: random(),
                sync_key_shared: false,
            };
            vault.save();
            return Ok(vault);
        };
        let v = json::parse(&text).map_err(|_| OpenError::Broken("vault.json испорчен".into()))?;
        let (key, master_salt) = if v.str_field("kdf") == Some("argon2id") {
            let salt = v.str_field("salt").and_then(b64_decode).ok_or_else(|| OpenError::Broken("нет соли".into()))?;
            let Some(master) = master else { return Err(OpenError::NeedMaster) };
            (derive(master, &salt), Some(salt))
        } else {
            (Self::device_key(dir), None)
        };
        let data = v.str_field("data").and_then(|d| open(&key, d)).ok_or(OpenError::NeedMaster)?;
        let inner = json::parse(&String::from_utf8_lossy(&data)).map_err(|_| OpenError::Broken("содержимое".into()))?;
        let accounts = match inner.get("accounts") {
            Some(Value::Arr(items)) => items.iter().filter_map(Account::from_json).collect(),
            _ => Vec::new(),
        };
        let active = match inner.get("active") {
            Some(Value::Num(n)) => n.parse().ok().filter(|i: &usize| *i < accounts.len()),
            _ => None,
        };
        let sync_key = inner.str_field("sync_key").and_then(unhex).and_then(|b| b.try_into().ok()).unwrap_or_else(random);
        let sync_key_shared = inner.get("sync_key_shared") == Some(&Value::Bool(true));
        Ok(Vault { dir: dir.to_path_buf(), key, master_salt, accounts, active, sync_key, sync_key_shared })
    }

    pub fn save(&self) {
        let inner = Value::Obj(vec![
            ("accounts".into(), Value::Arr(self.accounts.iter().map(Account::to_json).collect())),
            ("active".into(), self.active.map(|i| Value::Num(i.to_string())).unwrap_or(Value::Null)),
            ("sync_key".into(), Value::Str(hex(&self.sync_key))),
            ("sync_key_shared".into(), Value::Bool(self.sync_key_shared)),
        ]);
        let mut fields = vec![("version".into(), Value::Num("1".into()))];
        match &self.master_salt {
            Some(salt) => {
                fields.push(("kdf".into(), Value::Str("argon2id".into())));
                fields.push(("salt".into(), Value::Str(b64_encode(salt))));
            }
            None => fields.push(("kdf".into(), Value::Str("device".into()))),
        }
        fields.push(("data".into(), Value::Str(seal(&self.key, inner.to_text().as_bytes()))));
        let path = Self::vault_path(&self.dir);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, Value::Obj(fields).to_text()).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn has_master_password(&self) -> bool {
        self.master_salt.is_some()
    }

    /// Задать мастер-пароль (пустой — снять и вернуться к ключу устройства).
    pub fn set_master(&mut self, master: &str) {
        if master.is_empty() {
            self.master_salt = None;
            self.key = Self::device_key(&self.dir);
        } else {
            let salt: [u8; 16] = random();
            self.key = derive(master, &salt);
            self.master_salt = Some(salt.to_vec());
        }
        self.save();
    }

    /// Пароль синхронизации: один и тот же на всех устройствах. Соль — общая
    /// и постоянная (иначе устройства получили бы разные ключи).
    pub fn set_sync_password(&mut self, password: &str) {
        if password.is_empty() {
            self.sync_key = random();
            self.sync_key_shared = false;
        } else {
            self.sync_key = derive(password, b"OBSOS sync passwords v1");
            self.sync_key_shared = true;
        }
        self.save();
    }

    pub fn account(&self) -> Option<&Account> {
        self.active.and_then(|i| self.accounts.get(i))
    }
}

/// Сайт адреса: `https://example.com:8443`.
pub fn origin_of(url: &url::Url) -> Option<String> {
    match url.scheme() {
        "http" | "https" => {}
        _ => return None,
    }
    let host = url.host_str()?;
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_open_and_record_keys() {
        let key: Key = random();
        let login = Login { origin: "https://example.com".into(), username: "roman".into(), password: "пароль\"1".into(), updated_ms: 5 };
        let sealed = login.seal(&key);
        assert_eq!(Login::open(&key, &sealed), Some(login.clone()));
        assert_eq!(Login::open(&random(), &sealed), None);
        assert_eq!(login.record_key(&key), login.record_key(&key));
        assert_ne!(login.record_key(&key), login.record_key(&random()));
        assert_eq!(derive("x", b"0123456789abcdef"), derive("x", b"0123456789abcdef"));
    }
}
