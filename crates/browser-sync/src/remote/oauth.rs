//! Вход через браузер — OAuth 2.0 Device Authorization Grant (RFC 8628):
//! программа получает короткий код, пользователь открывает страницу
//! площадки (OBSOS открывает её сам новой вкладкой), вводит код и разрешает
//! доступ; программа тем временем опрашивает площадку и получает токен.
//!
//! Нужен `client_id` OAuth-приложения площадки (у Google и Яндекса ещё
//! `client_secret` — для «устройств без клавиатуры» он не секретный). Без
//! него — вход токеном или паролем приложения (`Provider::token_page`).
//! У Bitbucket и WebDAV Device Flow нет.

use alloc::format;
use alloc::string::{String, ToString};

use super::{Account, Error, Http, Provider, Request, Response, form_decode, send};
use crate::json::Value;

/// Начало входа: что показать пользователю.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Опрашивать не чаще (секунды).
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tokens {
    pub access: String,
    pub refresh: String,
    /// Через сколько секунд истечёт `access` (0 — не истекает).
    pub expires_in: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    /// Пользователь ещё не подтвердил.
    Pending,
    /// Опрашивать реже (интервал + 5 с).
    SlowDown,
    Done(Tokens),
    /// Отказ, код истёк — начинать заново.
    Failed(String),
}

struct Endpoints {
    code: String,
    token: String,
    scope: &'static str,
}

fn endpoints(account: &Account) -> Option<Endpoints> {
    let host = account.host();
    Some(match account.provider {
        Provider::GitHub => Endpoints {
            // Вход — на github.com, а не на api.github.com (свой GitHub
            // Enterprise: адрес API …/api/v3 → вход на том же сервере).
            code: format!("{}/login/device/code", web_host(&host, "https://github.com")),
            token: format!("{}/login/oauth/access_token", web_host(&host, "https://github.com")),
            scope: "repo",
        },
        Provider::GitLab => Endpoints {
            code: format!("{host}/oauth/authorize_device"),
            token: format!("{host}/oauth/token"),
            scope: "api",
        },
        Provider::GoogleDrive => Endpoints {
            code: String::from("https://oauth2.googleapis.com/device/code"),
            token: String::from("https://oauth2.googleapis.com/token"),
            scope: "https://www.googleapis.com/auth/drive.file",
        },
        Provider::YandexDisk => Endpoints {
            code: String::from("https://oauth.yandex.ru/device/code"),
            token: String::from("https://oauth.yandex.ru/token"),
            scope: "",
        },
        Provider::Bitbucket | Provider::WebDav | Provider::Folder => return None,
    })
}

fn web_host(api: &str, default: &str) -> String {
    if api == "https://api.github.com" {
        default.to_string()
    } else {
        api.trim_end_matches("/api/v3").to_string()
    }
}

/// Есть ли у площадки вход через браузер.
pub fn supported(provider: Provider) -> bool {
    matches!(provider, Provider::GitHub | Provider::GitLab | Provider::GoogleDrive | Provider::YandexDisk)
}

/// Ответ JSON или форма (`a=1&b=2`) → поле.
fn field(response: &Response, json: &Option<Value>, key: &str) -> Option<String> {
    if let Some(v) = json {
        return match v.get(key)? {
            Value::Str(s) => Some(s.clone()),
            Value::Num(n) => Some(n.clone()),
            _ => None,
        };
    }
    form_decode(&response.text()).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn parsed(response: &Response) -> Option<Value> {
    crate::json::parse(&response.text()).ok()
}

pub fn start(account: &Account, http: &dyn Http) -> Result<DeviceCode, Error> {
    let ep = endpoints(account).ok_or_else(|| Error::Auth(format!("у {} нет входа через браузер", account.provider.title())))?;
    if account.client_id.is_empty() {
        return Err(Error::Auth(String::from("не задан client_id OAuth-приложения")));
    }
    let mut fields = alloc::vec![("client_id", account.client_id.as_str())];
    if !ep.scope.is_empty() {
        fields.push(("scope", ep.scope));
    }
    let request = Request::new("POST", ep.code).header("Accept", "application/json").header("User-Agent", "OBSOS-browser").form(&fields);
    let response = send(http, &request)?;
    let json = parsed(&response);
    if !response.ok() {
        let why = field(&response, &json, "error_description").or_else(|| field(&response, &json, "error")).unwrap_or_else(|| response.text());
        return Err(Error::Auth(format!("HTTP {}: {why}", response.status)));
    }
    let get = |k: &str| field(&response, &json, k);
    Ok(DeviceCode {
        device_code: get("device_code").ok_or_else(|| Error::Protocol("нет device_code".into()))?,
        user_code: get("user_code").ok_or_else(|| Error::Protocol("нет user_code".into()))?,
        // Google и Яндекс называют поле verification_url.
        verification_uri: get("verification_uri_complete")
            .or_else(|| get("verification_uri"))
            .or_else(|| get("verification_url"))
            .ok_or_else(|| Error::Protocol("нет адреса подтверждения".into()))?,
        interval: get("interval").and_then(|v| v.parse().ok()).unwrap_or(5),
        expires_in: get("expires_in").and_then(|v| v.parse().ok()).unwrap_or(900),
    })
}

fn tokens(response: &Response, json: &Option<Value>) -> Option<Tokens> {
    Some(Tokens {
        access: field(response, json, "access_token")?,
        refresh: field(response, json, "refresh_token").unwrap_or_default(),
        expires_in: field(response, json, "expires_in").and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

pub fn poll(account: &Account, http: &dyn Http, device_code: &str) -> Result<Poll, Error> {
    let ep = endpoints(account).ok_or_else(|| Error::Auth("нет входа через браузер".into()))?;
    // Яндекс — по-своему: grant_type=device_code и поле code.
    let mut fields = if account.provider == Provider::YandexDisk {
        alloc::vec![("client_id", account.client_id.as_str()), ("grant_type", "device_code"), ("code", device_code)]
    } else {
        alloc::vec![
            ("client_id", account.client_id.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", device_code),
        ]
    };
    if !account.client_secret.is_empty() {
        fields.push(("client_secret", account.client_secret.as_str()));
    }
    let request = Request::new("POST", ep.token).header("Accept", "application/json").header("User-Agent", "OBSOS-browser").form(&fields);
    let response = send(http, &request)?;
    let json = parsed(&response);
    // GitHub отвечает ошибки кодом 200, остальные — 400.
    if let Some(error) = field(&response, &json, "error") {
        return Ok(match error.as_str() {
            "authorization_pending" => Poll::Pending,
            "slow_down" => Poll::SlowDown,
            other => Poll::Failed(field(&response, &json, "error_description").unwrap_or_else(|| other.to_string())),
        });
    }
    if !response.ok() {
        return Ok(Poll::Failed(format!("HTTP {}", response.status)));
    }
    tokens(&response, &json).map(Poll::Done).ok_or_else(|| Error::Protocol("нет access_token".into()))
}

/// Обновить истекающий токен. `Ok(None)` — обновлять нечем.
pub fn refresh(account: &Account, http: &dyn Http) -> Result<Option<Tokens>, Error> {
    if account.refresh_token.is_empty() {
        return Ok(None);
    }
    let Some(ep) = endpoints(account) else { return Ok(None) };
    let mut fields = alloc::vec![
        ("client_id", account.client_id.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", account.refresh_token.as_str()),
    ];
    if !account.client_secret.is_empty() {
        fields.push(("client_secret", account.client_secret.as_str()));
    }
    let request = Request::new("POST", ep.token).header("Accept", "application/json").header("User-Agent", "OBSOS-browser").form(&fields);
    let response = send(http, &request)?;
    let json = parsed(&response);
    if !response.ok() || field(&response, &json, "error").is_some() {
        return Err(Error::Auth(format!("обновление токена: HTTP {} {}", response.status, super::clip(&response.text(), 120))));
    }
    Ok(tokens(&response, &json))
}

/// Записать полученные токены в учётную запись.
pub fn apply(account: &mut Account, tokens: &Tokens, now_ms: u64) {
    account.token = tokens.access.clone();
    if !tokens.refresh.is_empty() {
        account.refresh_token = tokens.refresh.clone();
    }
    account.expires_ms = if tokens.expires_in > 0 { now_ms + tokens.expires_in * 1000 } else { 0 };
}

/// Токен истекает в ближайшие 2 минуты (или уже истёк).
pub fn expiring(account: &Account, now_ms: u64) -> bool {
    account.expires_ms != 0 && account.expires_ms <= now_ms + 120_000
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use core::cell::RefCell;

    /// Площадка входа: отвечает заранее заданными ответами и запоминает запросы.
    struct Script {
        answers: RefCell<Vec<(u16, &'static str)>>,
        seen: RefCell<Vec<Request>>,
    }

    impl Http for Script {
        fn send(&self, request: &Request) -> Result<Response, String> {
            self.seen.borrow_mut().push(request.clone());
            let (status, body) = self.answers.borrow_mut().remove(0);
            Ok(Response { status, headers: Vec::new(), body: body.as_bytes().to_vec() })
        }
    }

    fn script(answers: &[(u16, &'static str)]) -> Script {
        Script { answers: RefCell::new(answers.to_vec()), seen: RefCell::new(Vec::new()) }
    }

    #[test]
    fn github_device_flow() {
        let mut acc = Account::new(Provider::GitHub);
        acc.client_id = String::from("Iv1.abc");
        let http = script(&[
            (200, r#"{"device_code":"dc","user_code":"WDJB-MJHT","verification_uri":"https://github.com/login/device","expires_in":899,"interval":5}"#),
            (200, r#"{"error":"authorization_pending"}"#),
            (200, r#"{"error":"slow_down"}"#),
            (200, "access_token=gho_x&token_type=bearer&scope=repo"),
        ]);
        let code = start(&acc, &http).unwrap();
        assert_eq!((code.user_code.as_str(), code.interval), ("WDJB-MJHT", 5));
        assert_eq!(http.seen.borrow()[0].url, "https://github.com/login/device/code");
        assert_eq!(poll(&acc, &http, &code.device_code).unwrap(), Poll::Pending);
        assert_eq!(poll(&acc, &http, &code.device_code).unwrap(), Poll::SlowDown);
        let Poll::Done(t) = poll(&acc, &http, &code.device_code).unwrap() else { panic!() };
        assert_eq!(t.access, "gho_x");
        apply(&mut acc, &t, 1_000);
        assert_eq!((acc.token.as_str(), acc.expires_ms), ("gho_x", 0));
        let body = String::from_utf8(http.seen.borrow()[1].body.clone()).unwrap();
        assert!(body.contains("device_code=dc") && body.contains("grant_type=urn%3Aietf"), "{body}");
    }

    #[test]
    fn google_and_yandex_names() {
        let mut g = Account::new(Provider::GoogleDrive);
        g.client_id = String::from("id");
        g.client_secret = String::from("sec");
        let http = script(&[
            (200, r#"{"device_code":"d","user_code":"ABC-DEF","verification_url":"https://www.google.com/device","expires_in":1800,"interval":5}"#),
            (428, r#"{"error":"authorization_pending"}"#),
            (200, r#"{"access_token":"ya29","expires_in":3599,"refresh_token":"1//r"}"#),
            (200, r#"{"access_token":"ya30","expires_in":3599}"#),
        ]);
        let code = start(&g, &http).unwrap();
        assert_eq!(code.verification_uri, "https://www.google.com/device");
        assert_eq!(poll(&g, &http, "d").unwrap(), Poll::Pending);
        let Poll::Done(t) = poll(&g, &http, "d").unwrap() else { panic!() };
        apply(&mut g, &t, 1_000);
        assert_eq!((g.refresh_token.as_str(), g.expires_ms), ("1//r", 1_000 + 3_599_000));
        assert!(expiring(&g, 3_500_000));
        let t = refresh(&g, &http).unwrap().unwrap();
        apply(&mut g, &t, 2_000);
        assert_eq!((g.token.as_str(), g.refresh_token.as_str()), ("ya30", "1//r"));

        let mut y = Account::new(Provider::YandexDisk);
        y.client_id = String::from("yid");
        let http = script(&[(400, r#"{"error":"authorization_pending","error_description":"User has not yet authorized"}"#)]);
        assert_eq!(poll(&y, &http, "code1").unwrap(), Poll::Pending);
        let body = String::from_utf8(http.seen.borrow()[0].body.clone()).unwrap();
        assert!(body.contains("grant_type=device_code") && body.contains("code=code1"), "{body}");
    }

    #[test]
    fn no_client_id_or_no_flow() {
        assert!(start(&Account::new(Provider::GitHub), &script(&[])).is_err());
        let mut b = Account::new(Provider::Bitbucket);
        b.client_id = String::from("x");
        assert!(start(&b, &script(&[])).is_err());
        assert!(!supported(Provider::WebDav) && supported(Provider::GitLab));
    }
}
