//! Учётная запись: проверка ввода и то, что из неё попадает на диск.
//!
//! # Про пароль — прямо и без обиняков
//!
//! Настоящей функции выведения ключа (KDF) здесь нет. Ни PBKDF2, ни scrypt, ни
//! Argon2 в проекте не реализованы, а тащить криптографическую зависимость в
//! UEFI-приложение ради одной записи в файл — решение, которое надо принимать
//! осознанно и отдельно, а не мимоходом внутри установщика.
//!
//! Поэтому пароль сохраняется как соль плюс многократно повторённый FNV-1a, и
//! алгоритм записан в самой строке — `fnv1a64-4096`. Это **не** защита от
//! подбора: FNV быстр по построению, и перебор словаря по такому хешу стоит
//! ровно столько же, сколько его вычисление. Смысл конструкции в другом:
//!
//! * пароль не лежит на диске открытым текстом;
//! * формат файла уже содержит поле алгоритма, поэтому переход на настоящий
//!   KDF не потребует ни менять формат, ни переносить данные — появится второй
//!   тег, а старые записи останутся читаемыми;
//! * поля `uid`, `gid` и `mode` заложены с первого дня, хотя проверять права
//!   пока некому.
//!
//! Сделать вид, что здесь криптография, было бы хуже, чем не делать её вовсе:
//! от такого «хеша» ожидали бы стойкости, которой у него нет.
//!
//! # Куда это пишется
//!
//! На системный раздел EFI, в `\FREEOS\PASSWD`. Правильное место — корневой
//! раздел, но своей файловой системы у FreeOS ещё нет (см. дорожную карту), а
//! FAT32 не хранит ни uid, ни режим доступа. Формат файла поэтому и текстовый:
//! перенести его на корневую ФС, когда она появится, — это скопировать файл, а
//! не мигрировать базу.

use alloc::format;
use alloc::string::String;

/// Первый непривилегированный идентификатор — то же соглашение, что в Unix.
pub const FIRST_UID: u32 = 1000;

/// Права на домашний каталог: rwxr-x---.
pub const HOME_MODE: u16 = 0o750;

/// Права на файл учётных записей: rw-r-----.
///
/// Читать его посторонним незачем даже с сегодняшним хешем-заглушкой, а
/// выставить права позже, когда появится проверка доступа, значит на какое-то
/// время оставить файл открытым.
pub const PASSWD_MODE: u16 = 0o640;

/// Сколько раз прогоняется хеш пароля.
///
/// Число заведомо недостаточное для стойкости и выбрано не ради неё: см.
/// заголовок модуля. Оно лишь делает перебор не мгновенным и оставляет в
/// формате место, где настоящему KDF будет что занять.
const ROUNDS: u32 = 4096;

/// Максимальная длина имени.
///
/// Шестнадцать знаков — то же ограничение, что у `useradd` в Linux по
/// умолчанию, и оно же гарантирует, что имя поместится в строку экрана.
pub const MAX_NAME: usize = 16;

/// Наибольшая длина пароля.
///
/// Ограничение интерфейса, а не формата: строка ввода на экране конечна, и
/// пароль, уехавший за её край, человек набирает вслепую.
pub const MAX_PASSWORD: usize = 32;

/// Что человек набрал на экране учётной записи.
#[derive(Default)]
pub struct Draft {
    pub name: String,
    pub password: String,
    pub repeat: String,
}

/// Чем плох введённый набор.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    Name,
    Password,
    Mismatch,
}

impl Draft {
    /// Проверить набранное.
    pub fn validate(&self) -> Result<(), Invalid> {
        if !is_valid_name(&self.name) {
            return Err(Invalid::Name);
        }
        if self.password.is_empty() {
            return Err(Invalid::Password);
        }
        if self.password != self.repeat {
            return Err(Invalid::Mismatch);
        }
        Ok(())
    }

    /// Собрать содержимое файла учётных записей.
    ///
    /// `entropy` — источник соли. Криптостойким он не является и не обязан:
    /// соль нужна, чтобы одинаковые пароли двух установок дали разные строки,
    /// а не чтобы противостоять подготовленным таблицам.
    #[must_use]
    pub fn to_passwd(&self, entropy: u64) -> String {
        let salt = mix(entropy ^ hash_bytes(self.name.as_bytes()));
        let digest = derive(&self.password, salt);

        let mut out = String::new();
        out.push_str("# FreeOS accounts, written by the installer\n");
        out.push_str("# name:uid:gid:mode:home:algorithm:salt:digest\n");
        out.push_str(
            "# The digest is NOT produced by a key derivation function; see\n\
             # crates/installer/src/account.rs for what it is and what it is not.\n",
        );
        out.push_str(&format!(
            "{name}:{uid}:{gid}:{mode:04o}:/home/{name}:fnv1a64-{ROUNDS}:{salt:016x}:{digest:016x}\n",
            name = self.name,
            uid = FIRST_UID,
            gid = FIRST_UID,
            mode = HOME_MODE,
        ));
        out
    }
}

/// Допустимо ли имя.
///
/// Набор знаков сознательно уже, чем позволяет Unix: имя попадает и в путь
/// домашнего каталога, и в будущую файловую систему, а имя с пробелом или
/// точкой — источник неприятностей на годы вперёд.
#[must_use]
pub fn is_valid_name(name: &str) -> bool {
    if name.is_empty() || name.chars().count() > MAX_NAME {
        return false;
    }
    // Первый знак — буква: имя, начинающееся с цифры или дефиса, часть
    // утилит принимает за номер или за ключ командной строки.
    let mut chars = name.chars();
    let first = chars.next().unwrap_or(' ');
    if !first.is_ascii_lowercase() {
        return false;
    }
    name.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
}

/// Допустим ли знак в имени — нужно тому, кто обрабатывает нажатия.
#[must_use]
pub fn is_name_char(ch: char) -> bool {
    ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_'
}

/// Многократный прогон хеша.
fn derive(password: &str, salt: u64) -> u64 {
    let mut state = salt;
    for round in 0..ROUNDS {
        state = mix(state ^ hash_bytes(password.as_bytes()));
        state = mix(state ^ u64::from(round));
    }
    state
}

/// FNV-1a, 64 бита.
fn hash_bytes(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Перемешивание битов (финализатор SplitMix64).
///
/// Нужно затем, что у FNV-1a соседние входы дают близкие выходы в старших
/// битах, а соль и хеш здесь ещё и складываются между собой.
const fn mix(mut value: u64) -> u64 {
    value ^= value >> 30;
    value = value.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value ^= value >> 27;
    value = value.wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}
