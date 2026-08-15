//! Проверка цепочки: от сертификата сервера до корня, которому мы верим.
//!
//! # Порядок вопросов
//!
//! ```text
//!   имя → сроки → подписи снизу вверх → корень из хранилища
//! ```
//!
//! Имя первым намеренно: сертификат, выписанный на чужое имя, — самая частая и
//! самая опасная неисправность, и отвечать на неё после трёх проверок подписи
//! значит тратить время на разбор того, что уже не подходит.
//!
//! # Чего эта проверка не делает
//!
//! Не спрашивает, не отозван ли сертификат. Ни списком отзыва, ни по OCSP:
//! и то и другое требует сходить в сеть **до** того, как сеть признана
//! исправной, — а мы в этот момент как раз пытаемся её поднять. Сказать это
//! вслух важнее, чем сделать вид, что проверка полная: доверие к самому
//! обновлению у этой системы держится не на TLS, а на подписи Ed25519 под
//! образом (фаза 39). TLS нужен затем, чтобы GitHub вообще ответил.
//!
//! Не проверяет `nameConstraints` — но и не пропускает их: сертификат с этим
//! расширением, помеченным критическим, отвергается разбором (см. `cert`).

use crate::cert::{Certificate, usage};
use crate::store::Store;

/// Сколько звеньев цепочки эта система готова пройти.
///
/// Восемь — вдвое больше, чем встречается: настоящая цепочка это лист,
/// промежуточный и корень. Предел существует потому, что длину цепочки выбирает
/// сервер.
pub const MAX_CHAIN: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Сервер не прислал ни одного сертификата.
    Empty,
    /// Цепочка длиннее, чем эта система готова пройти.
    TooLong,
    /// Сертификат выписан не на то имя, к которому мы подключались.
    WrongName,
    /// Срок действия ещё не начался.
    NotYetValid,
    /// Срок действия кончился.
    Expired,
    /// Следующего звена нет ни в цепочке, ни в хранилище корней.
    NoIssuer,
    /// Звено нашлось, но подпись под ним не сходится.
    BadSignature,
    /// Звену не разрешено подписывать сертификаты.
    NotAuthority,
    /// Звену разрешено подписывать, но не так глубоко.
    PathTooLong,
    /// Сертификат сервера не годится для проверки подлинности сервера.
    NotForServers,
    /// Хранилище корней пусто: доверять нечему.
    NoRoots,
}

impl Error {
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::Empty => "the server sent no certificate at all",
            Self::TooLong => "the server sent a longer chain than this system will walk",
            Self::WrongName => "that certificate is for another name",
            Self::NotYetValid => "that certificate is not valid yet",
            Self::Expired => "that certificate has expired",
            Self::NoIssuer => "the chain leads to nobody this system trusts",
            Self::BadSignature => "a signature in the chain does not check out",
            Self::NotAuthority => "a certificate in the chain is not allowed to sign others",
            Self::PathTooLong => "a certificate in the chain signs deeper than it is allowed to",
            Self::NotForServers => "that certificate is not for authenticating a server",
            Self::NoRoots => "this system trusts no certificate authority; refusing",
        }
    }
}

/// Проверить цепочку: `chain[0]` — сертификат сервера, дальше — вверх.
///
/// `now` — секунды эпохи Unix. Ноль означает «часы неизвестны»; в этом случае
/// сроки не проверяются, и об этом обязан сказать вслух вызывающий. Молча
/// пропускать проверку дат нельзя: просроченный сертификат — это ровно тот
/// случай, ради которого сроки и существуют.
pub fn verify(
    chain: &[Certificate<'_>],
    roots: &Store<'_>,
    host: &str,
    now: i64,
) -> Result<(), Error> {
    if chain.is_empty() {
        return Err(Error::Empty);
    }
    if chain.len() > MAX_CHAIN {
        return Err(Error::TooLong);
    }
    if roots.is_empty() {
        return Err(Error::NoRoots);
    }

    let leaf = &chain[0];
    if !leaf.matches(host) {
        return Err(Error::WrongName);
    }
    if leaf.server_auth == Some(false) {
        return Err(Error::NotForServers);
    }

    // Идём снизу вверх. На каждом шаге сначала спрашиваем хранилище: корень
    // может подписать и лист напрямую, и тогда всё, что сервер прислал сверх
    // листа, нас не касается.
    for (index, certificate) in chain.iter().enumerate() {
        within_dates(certificate, now)?;

        if let Some(()) = trusted_issuer(certificate, roots, index, now) {
            return Ok(());
        }

        let Some(parent) = chain.get(index + 1) else {
            return Err(Error::NoIssuer);
        };
        if parent.subject != certificate.issuer {
            return Err(Error::NoIssuer);
        }
        authority(parent, index)?;
        if !parent.signed(certificate) {
            return Err(Error::BadSignature);
        }
    }

    // Цепочка кончилась, а корня не нашлось: последний сертификат мог быть
    // самоподписанным, но в нашем хранилище его нет.
    Err(Error::NoIssuer)
}

/// Есть ли в хранилище корень, подписавший этот сертификат.
fn trusted_issuer(
    certificate: &Certificate<'_>,
    roots: &Store<'_>,
    index: usize,
    now: i64,
) -> Option<()> {
    for root in roots.find(certificate.issuer) {
        if within_dates(&root, now).is_err() {
            continue;
        }
        if authority(&root, index).is_err() {
            continue;
        }
        if root.signed(certificate) {
            return Some(());
        }
    }
    None
}

/// Срок действия, если часы известны.
fn within_dates(certificate: &Certificate<'_>, now: i64) -> Result<(), Error> {
    if now == 0 {
        return Ok(());
    }
    if now < certificate.not_before {
        return Err(Error::NotYetValid);
    }
    if now > certificate.not_after {
        return Err(Error::Expired);
    }
    Ok(())
}

/// Разрешено ли этому сертификату подписывать чужие — и так ли глубоко.
///
/// `child` — место подписываемого в цепочке; оно же и есть число промежуточных
/// звеньев, которые окажутся ниже издателя (лист имеет место 0, и под ним нет
/// никого).
fn authority(certificate: &Certificate<'_>, child: usize) -> Result<(), Error> {
    // Отсутствие `basicConstraints` у промежуточного — это сертификат конечного
    // объекта, поставленный подписывать. Так выписывали в девяностых, и
    // принимать это сегодня значит принимать сертификат любого сайта в роли
    // удостоверяющего центра.
    if !certificate.basic.present || !certificate.basic.ca {
        return Err(Error::NotAuthority);
    }
    // `keyUsage` необязателен, но если он есть, `keyCertSign` обязан быть.
    if let Some(bits) = certificate.key_usage {
        if bits & usage::KEY_CERT_SIGN == 0 {
            return Err(Error::NotAuthority);
        }
    }
    if let Some(limit) = certificate.basic.path_len {
        if (child as u64) > u64::from(limit) {
            return Err(Error::PathTooLong);
        }
    }
    Ok(())
}
