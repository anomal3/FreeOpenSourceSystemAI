//! Кому эта машина верит, когда ей приносят обновление.
//!
//! Разбор файла и проверка подписи живут в крейте [`osupdate`], а здесь —
//! только чтение файла с носителя. Так вышло не ради красоты: тот же `/os-keys`
//! читает программа `/bin/sysupdate`, проверяя подпись индекса репозитория, — а
//! два разбора одного формата по разные стороны границы привилегий расходятся
//! молча, и выглядит это как «сервер отдаёт испорченный файл».
//!
//! Почему ключи лежат в образе, а не в ядре и не в `/etc`, сказано в заголовке
//! [`osupdate::keys`].

use osupdate::Trusted;
use osupdate::keys::LIMIT;

use crate::fs;

/// Где лежат доверенные ключи.
const PATH: &str = "/os-keys";

/// Прочитать доверенные ключи. Пустой список — обновления запрещены.
#[must_use]
pub fn keys() -> Trusted {
    // Читается **мимо проверки прав**, и это то же рассуждение, что у
    // `/etc/passwd`: спрашивает ядро, а не программа, и права здесь ни при чём.
    let Some(Ok((bytes, _))) = fs::read(PATH, LIMIT) else {
        return Trusted::empty();
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return Trusted::empty();
    };
    let trusted = Trusted::parse(text);
    if trusted.dropped() != 0 {
        // Молча отброшенный ключ — это машина, которая однажды откажется от
        // собственного обновления, и причину будут искать где угодно, только не
        // в девятой строке файла.
        crate::kprintln!(
            "  trust       : {PATH} lists more than {} keys; {} ignored",
            trusted.len(),
            trusted.dropped()
        );
    }
    trusted
}

/// Сходится ли подпись хоть с одним из доверенных ключей.
#[must_use]
pub fn verifies(digest: &[u8; 32], signature: &[u8], keys: &Trusted) -> bool {
    keys.verifies(digest, signature)
}
