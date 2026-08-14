//! Транспортный слой SSH (RFC 4253): пакеты, обмен ключами, шифрование.
//!
//! # Что здесь есть
//!
//! Ровно один набор алгоритмов, и все четыре — те, что современный OpenSSH
//! предлагает первыми:
//!
//! ```text
//!   обмен ключами   curve25519-sha256
//!   ключ хоста      ssh-ed25519
//!   шифр            chacha20-poly1305@openssh.com
//!   подпись пакета  встроена в шифр (AEAD)
//! ```
//!
//! Один набор — это решение, а не заготовка. Каждый лишний алгоритм — это код,
//! который надо проверить чужим клиентом, и ветка согласования, по которой
//! соединение может пойти незамеченным путём. Клиент, у которого нет ни одного
//! из наших, получит внятный отказ на этапе согласования, а не странное
//! поведение потом.
//!
//! # Чего здесь нет
//!
//! **Смены ключей на ходу** (`SSH_MSG_KEXINIT` посреди сеанса). RFC советует
//! менять их каждый гигабайт или час; мы отвечаем на такую просьбу отказом
//! соединения, и это записано здесь, а не выяснится на длинной сессии. Причина
//! проста: смена ключей — это второй проход всего обмена в состоянии, когда по
//! соединению уже идут данные, и делать её вслепую (без клиента, который её
//! потребует в тесте) значит писать непроверяемый код.
//!
//! **Сжатия** — `none`, и это не упрощение: сжатие в SSH давно считается скорее
//! вредным (оно течёт информацией о содержимом).
//!
//! # Кто здесь чему доверяет
//!
//! Всё, что приходит с провода, — недоверенное. Длина пакета приходит оттуда же,
//! поэтому она проверяется против [`MAX_PACKET`] **до** того, как по ней
//! что-нибудь выделяется или читается, а подпись проверяется **до** расшифровки
//! содержимого. Порядок здесь не стилистический: обратный означает, что мы
//! обрабатываем данные, которые подделал кто угодно.

#![no_std]

pub mod cipher;
pub mod wire;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

use cipher::{Cipher, LENGTH_LEN, TAG_LEN};
use wire::{Reader, Writer};

/// Строка версии, которой мы представляемся.
///
/// Формат задан RFC 4253 §4.2 и обязателен до последнего символа: `SSH-2.0-`,
/// затем имя реализации без пробелов, затем `\r\n`. Имя видно в журналах чужих
/// машин — это первое, что о нас узнают снаружи.
pub const VERSION_LINE: &str = "SSH-2.0-FreeOS_0.1\r\n";

/// Версия без завершающих `\r\n` — именно она попадает в хеш обмена.
pub const VERSION: &str = "SSH-2.0-FreeOS_0.1";

/// Наибольший пакет, который мы согласны принять.
///
/// RFC 4253 требует уметь принимать 32768 байт содержимого; 35000 — с запасом
/// на заголовок, набивку и подпись. Больше — это либо чужая ошибка, либо
/// попытка заставить нас выделить память по числу с провода.
pub const MAX_PACKET: usize = 35_000;

/// Сколько места отводится под `KEXINIT` собеседника.
///
/// Четыре килобайта, и это не запас «на всякий случай»: OpenSSH перечисляет в
/// нём три десятка алгоритмов с длинными именами, и его `KEXINIT` уверенно
/// переваливает за килобайт. Пакет обязан сохраниться целиком — он входит в
/// хеш обмена, и обрезанная копия дала бы подпись, которая не сойдётся.
const PEER_KEXINIT_MAX: usize = 4096;

/// Наименьшая длина пакета вместе с набивкой.
const MIN_PACKET: usize = 16;

/// Кратность, до которой добивается пакет.
///
/// Восемь, а не размер блока шифра: у поточного шифра блока нет, и RFC 4253
/// велит в этом случае выравнивать на восемь.
const BLOCK: usize = 8;

// --- номера сообщений --------------------------------------------------------

pub const MSG_DISCONNECT: u8 = 1;
pub const MSG_IGNORE: u8 = 2;
pub const MSG_UNIMPLEMENTED: u8 = 3;
pub const MSG_DEBUG: u8 = 4;
pub const MSG_SERVICE_REQUEST: u8 = 5;
pub const MSG_SERVICE_ACCEPT: u8 = 6;
pub const MSG_KEXINIT: u8 = 20;
pub const MSG_NEWKEYS: u8 = 21;
pub const MSG_KEX_ECDH_INIT: u8 = 30;
pub const MSG_KEX_ECDH_REPLY: u8 = 31;

pub const MSG_USERAUTH_REQUEST: u8 = 50;
pub const MSG_USERAUTH_FAILURE: u8 = 51;
pub const MSG_USERAUTH_SUCCESS: u8 = 52;
pub const MSG_USERAUTH_PK_OK: u8 = 60;

pub const MSG_GLOBAL_REQUEST: u8 = 80;
pub const MSG_REQUEST_FAILURE: u8 = 82;
pub const MSG_CHANNEL_OPEN: u8 = 90;
pub const MSG_CHANNEL_OPEN_CONFIRMATION: u8 = 91;
pub const MSG_CHANNEL_OPEN_FAILURE: u8 = 92;
pub const MSG_CHANNEL_WINDOW_ADJUST: u8 = 93;
pub const MSG_CHANNEL_DATA: u8 = 94;
pub const MSG_CHANNEL_EOF: u8 = 96;
pub const MSG_CHANNEL_CLOSE: u8 = 97;
pub const MSG_CHANNEL_REQUEST: u8 = 98;
pub const MSG_CHANNEL_SUCCESS: u8 = 99;
pub const MSG_CHANNEL_FAILURE: u8 = 100;

/// Причина разрыва: не сошлись алгоритмы.
pub const DISCONNECT_KEY_EXCHANGE_FAILED: u32 = 3;
/// Причина разрыва: протокол нарушен.
pub const DISCONNECT_PROTOCOL_ERROR: u32 = 2;

// --- имена алгоритмов --------------------------------------------------------

const KEX_ALGORITHM: &str = "curve25519-sha256";
/// То же под старым именем: OpenSSH до 7.4 знает его только так, и стоит оно
/// одной строки.
const KEX_ALGORITHM_LEGACY: &str = "curve25519-sha256@libssh.org";
const HOST_KEY_ALGORITHM: &str = "ssh-ed25519";
const CIPHER_ALGORITHM: &str = "chacha20-poly1305@openssh.com";
const MAC_ALGORITHM: &str = "none";
const COMPRESSION_ALGORITHM: &str = "none";

/// Почему не вышло.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Пакет длиннее [`MAX_PACKET`] или короче возможного.
    BadLength,
    /// Подпись пакета не сошлась.
    BadTag,
    /// Пакет разобран, но его содержимое бессмысленно.
    Malformed,
    /// Не нашлось общего алгоритма.
    NoCommonAlgorithm,
    /// Не хватило места в буфере.
    NoRoom,
    /// Сообщение пришло не в том состоянии, в каком его ждут.
    OutOfOrder,
}

/// Состояние транспорта одного соединения.
pub struct Transport {
    /// Ключ хоста — тот, которым мы подписываем обмен и по которому нас узнают.
    host_key: SigningKey,
    /// Секрет обмена. Живёт до вычисления общего секрета и стирается.
    kex_secret: Option<StaticSecret>,

    /// Шифр входящего направления; `None` — до `NEWKEYS`.
    incoming: Option<Cipher>,
    /// Шифр исходящего направления.
    outgoing: Option<Cipher>,
    in_sequence: u64,
    out_sequence: u64,

    /// Идентификатор сеанса — хеш **первого** обмена. Дальше он не меняется
    /// никогда и служит привязкой подписи пользователя к этому соединению.
    session_id: [u8; 32],
    established: bool,

    /// Строка версии клиента без `\r\n` — она попадает в хеш обмена.
    peer_version: [u8; 255],
    peer_version_len: usize,
    /// Наш `KEXINIT` целиком: он тоже попадает в хеш, поэтому хранится.
    our_kexinit: [u8; 512],
    our_kexinit_len: usize,
    peer_kexinit: [u8; PEER_KEXINIT_MAX],
    peer_kexinit_len: usize,
}

impl Transport {
    /// Завести транспорт.
    ///
    /// `host_seed` — 32 байта ключа хоста, `kex_seed` — 32 байта под эфемерный
    /// секрет обмена. Оба приходят снаружи, потому что источник случайности —
    /// дело системы, а не библиотеки: библиотека, которая берёт случайность
    /// сама, однажды возьмёт её не оттуда.
    pub fn new(host_seed: [u8; 32], kex_seed: [u8; 32]) -> Self {
        Self {
            host_key: SigningKey::from_bytes(&host_seed),
            kex_secret: Some(StaticSecret::from(kex_seed)),
            incoming: None,
            outgoing: None,
            in_sequence: 0,
            out_sequence: 0,
            session_id: [0u8; 32],
            established: false,
            peer_version: [0u8; 255],
            peer_version_len: 0,
            our_kexinit: [0u8; 512],
            our_kexinit_len: 0,
            peer_kexinit: [0u8; PEER_KEXINIT_MAX],
            peer_kexinit_len: 0,
        }
    }

    /// Установлены ли ключи: с этого момента всё шифруется.
    pub fn is_encrypted(&self) -> bool {
        self.established
    }

    pub fn session_id(&self) -> &[u8; 32] {
        &self.session_id
    }

    /// Публичный ключ хоста в том виде, в каком его показывают снаружи.
    pub fn host_public_key(&self) -> [u8; 32] {
        self.host_key.verifying_key().to_bytes()
    }

    /// Запомнить строку версии клиента (без `\r\n`).
    pub fn set_peer_version(&mut self, line: &[u8]) -> Result<(), Error> {
        if line.len() > self.peer_version.len() {
            return Err(Error::Malformed);
        }
        self.peer_version[..line.len()].copy_from_slice(line);
        self.peer_version_len = line.len();
        Ok(())
    }

    /// Собрать наш `KEXINIT` и запомнить его для хеша.
    pub fn write_kexinit(&mut self, cookie: [u8; 16], out: &mut [u8]) -> Result<usize, Error> {
        let mut writer = Writer::new(out);
        writer.byte(MSG_KEXINIT);
        writer.bytes(&cookie);

        // Порядок списков задан RFC 4253 §7.1 и обязателен: получатель читает
        // их по порядку, а не по имени.
        let kex = concat_two(KEX_ALGORITHM, KEX_ALGORITHM_LEGACY);
        writer.string(&kex[..kex_len()]);
        writer.string(HOST_KEY_ALGORITHM.as_bytes());
        writer.string(CIPHER_ALGORITHM.as_bytes()); // клиент → сервер
        writer.string(CIPHER_ALGORITHM.as_bytes()); // сервер → клиент
        writer.string(MAC_ALGORITHM.as_bytes());
        writer.string(MAC_ALGORITHM.as_bytes());
        writer.string(COMPRESSION_ALGORITHM.as_bytes());
        writer.string(COMPRESSION_ALGORITHM.as_bytes());
        writer.string(b""); // языки, которых никто никогда не использовал
        writer.string(b"");
        // «Дальше сразу пойдёт первый пакет обмена» — мы так не делаем, поэтому
        // ноль. Клиент, поставивший здесь единицу и угадавший алгоритм, шлёт
        // свой `KEX_ECDH_INIT` следом, и это нам не мешает.
        writer.byte(0);
        writer.u32(0); // зарезервировано

        if !writer.ok() {
            return Err(Error::NoRoom);
        }
        let len = writer.len();
        if len > self.our_kexinit.len() {
            return Err(Error::NoRoom);
        }
        self.our_kexinit[..len].copy_from_slice(&out[..len]);
        self.our_kexinit_len = len;
        Ok(len)
    }

    /// Разобрать `KEXINIT` клиента и проверить, что общий язык нашёлся.
    pub fn read_kexinit(&mut self, payload: &[u8]) -> Result<(), Error> {
        if payload.first() != Some(&MSG_KEXINIT) {
            return Err(Error::OutOfOrder);
        }
        if payload.len() > self.peer_kexinit.len() {
            return Err(Error::NoRoom);
        }
        self.peer_kexinit[..payload.len()].copy_from_slice(payload);
        self.peer_kexinit_len = payload.len();

        let mut reader = Reader::new(&payload[1..]);
        reader.take(16).ok_or(Error::Malformed)?; // cookie

        let kex = reader.string().ok_or(Error::Malformed)?;
        let host_key = reader.string().ok_or(Error::Malformed)?;
        let cipher_in = reader.string().ok_or(Error::Malformed)?;
        let cipher_out = reader.string().ok_or(Error::Malformed)?;

        let kex_ok = wire::list_contains(kex, KEX_ALGORITHM)
            || wire::list_contains(kex, KEX_ALGORITHM_LEGACY);
        if !kex_ok
            || !wire::list_contains(host_key, HOST_KEY_ALGORITHM)
            || !wire::list_contains(cipher_in, CIPHER_ALGORITHM)
            || !wire::list_contains(cipher_out, CIPHER_ALGORITHM)
        {
            return Err(Error::NoCommonAlgorithm);
        }
        Ok(())
    }

    /// Ответить на `KEX_ECDH_INIT`: вычислить общий секрет, подписать обмен и
    /// собрать `KEX_ECDH_REPLY`.
    pub fn reply_to_kex(&mut self, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        if payload.first() != Some(&MSG_KEX_ECDH_INIT) {
            return Err(Error::OutOfOrder);
        }
        let mut reader = Reader::new(&payload[1..]);
        let client_public = reader.string().ok_or(Error::Malformed)?;
        if client_public.len() != 32 {
            return Err(Error::Malformed);
        }
        let mut client_bytes = [0u8; 32];
        client_bytes.copy_from_slice(client_public);

        let secret = self.kex_secret.take().ok_or(Error::OutOfOrder)?;
        let server_public = PublicKey::from(&secret);
        let shared = secret.diffie_hellman(&PublicKey::from(client_bytes));
        // Секрет с нулевым результатом означает, что нам подсунули точку малого
        // порядка: обмена не получилось, и продолжать с таким «общим секретом»
        // нельзя.
        if !shared.was_contributory() {
            return Err(Error::Malformed);
        }

        // Ключ хоста в том виде, в каком он едет на провод и попадает в хеш.
        let mut host_blob = [0u8; 64];
        let host_blob_len = {
            let mut writer = Writer::new(&mut host_blob);
            writer.string(HOST_KEY_ALGORITHM.as_bytes());
            writer.string(&self.host_key.verifying_key().to_bytes());
            if !writer.ok() {
                return Err(Error::NoRoom);
            }
            writer.len()
        };

        let hash = self.exchange_hash(
            &host_blob[..host_blob_len],
            client_public,
            server_public.as_bytes(),
            shared.as_bytes(),
        );

        // Идентификатор сеанса — хеш первого обмена, и меняться он больше не
        // будет: к нему привязывается подпись пользователя при входе.
        if !self.established {
            self.session_id = hash;
        }

        let signature = self.host_key.sign(&hash);
        let mut signature_blob = [0u8; 96];
        let signature_blob_len = {
            let mut writer = Writer::new(&mut signature_blob);
            writer.string(HOST_KEY_ALGORITHM.as_bytes());
            writer.string(&signature.to_bytes());
            if !writer.ok() {
                return Err(Error::NoRoom);
            }
            writer.len()
        };

        let mut writer = Writer::new(out);
        writer.byte(MSG_KEX_ECDH_REPLY);
        writer.string(&host_blob[..host_blob_len]);
        writer.string(server_public.as_bytes());
        writer.string(&signature_blob[..signature_blob_len]);
        if !writer.ok() {
            return Err(Error::NoRoom);
        }
        let len = writer.len();

        self.derive_keys(shared.as_bytes(), &hash);
        Ok(len)
    }

    /// Хеш обмена: всё, о чём стороны договорились, свёрнутое в 32 байта.
    ///
    /// Именно его подписывает ключ хоста, и именно поэтому в него входит
    /// **всё**: версии обеих сторон, оба `KEXINIT`, ключ хоста, оба публичных
    /// значения и общий секрет. Подмена любого из них на пути меняет хеш, и
    /// подпись перестаёт сходиться — на этом держится защита от посредника.
    fn exchange_hash(
        &self,
        host_blob: &[u8],
        client_public: &[u8],
        server_public: &[u8],
        shared: &[u8; 32],
    ) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash_string(&mut hash, &self.peer_version[..self.peer_version_len]);
        hash_string(&mut hash, VERSION.as_bytes());
        hash_string(&mut hash, &self.peer_kexinit[..self.peer_kexinit_len]);
        hash_string(&mut hash, &self.our_kexinit[..self.our_kexinit_len]);
        hash_string(&mut hash, host_blob);
        hash_string(&mut hash, client_public);
        hash_string(&mut hash, server_public);
        hash_mpint(&mut hash, shared);

        let digest = hash.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    /// Вывести ключи направлений из общего секрета и хеша.
    fn derive_keys(&mut self, shared: &[u8; 32], hash: &[u8; 32]) {
        // Буквы заданы RFC 4253 §7.2. Нам нужны только 'C' и 'D' — ключи
        // шифров; векторов инициализации у `chacha20-poly1305` нет (их роль
        // играет номер пакета), а ключей MAC нет, потому что подпись встроена
        // в шифр.
        let client_to_server = self.derive(shared, hash, b'C');
        let server_to_client = self.derive(shared, hash, b'D');
        self.incoming = Some(Cipher::new(&client_to_server));
        self.outgoing = Some(Cipher::new(&server_to_client));
    }

    /// Один ключ: `HASH(K || H || X || session_id)`, растянутый до 64 байт.
    ///
    /// Растягивание — тоже из RFC: если хеша не хватает, следующий кусок это
    /// `HASH(K || H || всё, что уже получено)`. Своя схема здесь означала бы
    /// ключи, которые не совпадут с клиентскими.
    fn derive(&self, shared: &[u8; 32], hash: &[u8; 32], letter: u8) -> [u8; cipher::KEY_LEN] {
        let mut first = Sha256::new();
        hash_mpint(&mut first, shared);
        first.update(hash);
        first.update([letter]);
        first.update(self.session_id);
        let first = first.finalize();

        let mut second = Sha256::new();
        hash_mpint(&mut second, shared);
        second.update(hash);
        second.update(first);
        let second = second.finalize();

        let mut out = [0u8; cipher::KEY_LEN];
        out[..32].copy_from_slice(&first);
        out[32..].copy_from_slice(&second);
        out
    }

    /// Перейти на новые ключи. Вызывается после обмена `NEWKEYS`.
    pub fn enable_encryption(&mut self) {
        self.established = true;
    }

    /// Сколько байт нужно, чтобы пакет был полным.
    ///
    /// `None` — данных пока не хватает даже на то, чтобы понять длину.
    pub fn packet_size(&self, buffer: &[u8]) -> Option<Result<usize, Error>> {
        if buffer.len() < LENGTH_LEN {
            return None;
        }
        let length = match &self.incoming {
            Some(cipher) if self.established => {
                let mut encrypted = [0u8; LENGTH_LEN];
                encrypted.copy_from_slice(&buffer[..LENGTH_LEN]);
                cipher.decrypt_length(self.in_sequence, encrypted) as usize
            }
            _ => u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize,
        };

        // Проверка **до** любого использования длины: число пришло с провода.
        if length < MIN_PACKET - LENGTH_LEN || length > MAX_PACKET {
            return Some(Err(Error::BadLength));
        }
        let total = if self.established {
            LENGTH_LEN + length + TAG_LEN
        } else {
            LENGTH_LEN + length
        };
        Some(Ok(total))
    }

    /// Разобрать пакет целиком. Возвращает длину содержимого в начале буфера.
    ///
    /// Буфер меняется на месте: содержимое расшифровывается поверх
    /// зашифрованного, и после успешного разбора первые `len` байт — это
    /// полезная нагрузка.
    pub fn open_packet(&mut self, buffer: &mut [u8]) -> Result<(usize, usize), Error> {
        let Some(total) = self.packet_size(buffer) else {
            return Err(Error::BadLength);
        };
        let total = total?;
        if buffer.len() < total {
            return Err(Error::BadLength);
        }

        let (length, body_end) = if self.established {
            let cipher = self.incoming.as_ref().ok_or(Error::OutOfOrder)?.clone();
            let body_end = total - TAG_LEN;
            let mut tag = [0u8; TAG_LEN];
            tag.copy_from_slice(&buffer[body_end..total]);
            // Подпись проверяется по зашифрованному и **до** расшифровки: иначе
            // мы разбираем то, что подделал кто угодно.
            if !cipher.verify(self.in_sequence, &buffer[..body_end], &tag) {
                return Err(Error::BadTag);
            }
            let mut encrypted = [0u8; LENGTH_LEN];
            encrypted.copy_from_slice(&buffer[..LENGTH_LEN]);
            let length = cipher.decrypt_length(self.in_sequence, encrypted) as usize;
            cipher.apply_payload(self.in_sequence, &mut buffer[LENGTH_LEN..body_end]);
            (length, body_end)
        } else {
            let length =
                u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
            (length, LENGTH_LEN + length)
        };

        let padding = usize::from(buffer[LENGTH_LEN]);
        // Набивки не может быть больше, чем всего содержимого: такая длина
        // означала бы отрицательный размер полезной нагрузки.
        if padding + 1 > length {
            return Err(Error::Malformed);
        }
        let payload_len = length - padding - 1;

        // Содержимое сдвигается в начало буфера: вызывающему незачем помнить,
        // что перед ним лежат пять байт заголовка.
        buffer.copy_within(LENGTH_LEN + 1..LENGTH_LEN + 1 + payload_len, 0);
        self.in_sequence = self.in_sequence.wrapping_add(1);
        Ok((payload_len, total.max(body_end)))
    }

    /// Собрать пакет из содержимого. Возвращает его полную длину.
    pub fn seal_packet(&mut self, payload: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        // Набивка добивает пакет до кратности восьми и не может быть короче
        // четырёх байт — так велит RFC 4253 §6. Но **что именно** выравнивается,
        // зависит от шифра, и это та самая тонкость, на которой рукопожатие
        // ломается последним шагом:
        //
        // * до шифрования выравнивается весь пакет вместе с полем длины;
        // * у `chacha20-poly1305` поле длины шифруется **отдельным ключом** и
        //   в блок не входит, поэтому выравнивается всё, кроме него.
        //
        // Перепутать их значит собрать пакет, который клиент примет за
        // испорченный: OpenSSH так и говорит — `padding error: need 28 block 8`.
        let unpadded = if self.established {
            1 + payload.len()
        } else {
            LENGTH_LEN + 1 + payload.len()
        };
        let mut padding = BLOCK - (unpadded % BLOCK);
        if padding < 4 {
            padding += BLOCK;
        }
        let length = 1 + payload.len() + padding;
        let total = LENGTH_LEN + length + if self.established { TAG_LEN } else { 0 };
        if out.len() < total {
            return Err(Error::NoRoom);
        }

        out[LENGTH_LEN] = padding as u8;
        out[LENGTH_LEN + 1..LENGTH_LEN + 1 + payload.len()].copy_from_slice(payload);
        // Набивка нулями. RFC требует случайных байт, и это требование не
        // декоративное — но у поточного шифра с AEAD набивка целиком лежит под
        // шифрованием и подписью, и её предсказуемость не даёт наблюдателю
        // ничего. Здесь нули, потому что источник случайности в программе
        // стоит системного вызова на каждый пакет.
        let padding_at = LENGTH_LEN + 1 + payload.len();
        out[padding_at..padding_at + padding].fill(0);

        if self.established {
            let cipher = self.outgoing.as_ref().ok_or(Error::OutOfOrder)?.clone();
            let encrypted_length = cipher.encrypt_length(self.out_sequence, length as u32);
            out[..LENGTH_LEN].copy_from_slice(&encrypted_length);
            let body_end = LENGTH_LEN + length;
            cipher.apply_payload(self.out_sequence, &mut out[LENGTH_LEN..body_end]);
            let tag = cipher.sign(self.out_sequence, &out[..body_end]);
            out[body_end..body_end + TAG_LEN].copy_from_slice(&tag);
        } else {
            out[..LENGTH_LEN].copy_from_slice(&(length as u32).to_be_bytes());
        }

        self.out_sequence = self.out_sequence.wrapping_add(1);
        Ok(total)
    }
}

/// Записать строку в хеш так же, как она пишется на провод.
fn hash_string(hash: &mut Sha256, data: &[u8]) {
    hash.update((data.len() as u32).to_be_bytes());
    hash.update(data);
}

/// Записать в хеш число в формате `mpint` — с ведущим нулём, если надо.
fn hash_mpint(hash: &mut Sha256, value: &[u8; 32]) {
    let start = value.iter().position(|byte| *byte != 0).unwrap_or(32);
    let trimmed = &value[start..];
    if trimmed.is_empty() {
        hash.update(0u32.to_be_bytes());
        return;
    }
    if trimmed[0] & 0x80 != 0 {
        hash.update((trimmed.len() as u32 + 1).to_be_bytes());
        hash.update([0u8]);
    } else {
        hash.update((trimmed.len() as u32).to_be_bytes());
    }
    hash.update(trimmed);
}

/// Два имени алгоритма через запятую, без кучи.
fn concat_two(first: &str, second: &str) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut at = 0;
    for byte in first.as_bytes() {
        out[at] = *byte;
        at += 1;
    }
    out[at] = b',';
    at += 1;
    for byte in second.as_bytes() {
        out[at] = *byte;
        at += 1;
    }
    out
}

/// Длина строки, которую собрал [`concat_two`] для алгоритмов обмена.
const fn kex_len() -> usize {
    KEX_ALGORITHM.len() + 1 + KEX_ALGORITHM_LEGACY.len()
}
