//! Мелкие инструменты панели «Пароли» и «Настроек»: генератор паролей,
//! проверка утечки (Have I Been Pwned, k-анонимность), импорт закладок
//! (HTML Netscape — Chrome, Firefox, Edge) и паролей (CSV Chrome/Firefox),
//! экспорт обратно.

use browser_sync::remote::{Http, Request};
use sha1::{Digest, Sha1};

/// Пароль из `len` знаков: буквы обоих регистров и цифры, по желанию знаки.
/// Похожие знаки (`l`, `1`, `I`, `O`, `0`) не берутся — пароль иногда
/// переписывают руками. Хотя бы одна строчная, заглавная, цифра (и знак).
pub fn generate_password(len: usize, symbols: bool) -> String {
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const DIGITS: &[u8] = b"23456789";
    const SYMBOLS: &[u8] = b"!#$%&*+-=?@^_~";
    let len = len.clamp(8, 64);
    let mut sets: Vec<&[u8]> = vec![LOWER, UPPER, DIGITS];
    if symbols {
        sets.push(SYMBOLS);
    }
    let all: Vec<u8> = sets.iter().flat_map(|s| s.iter().copied()).collect();
    loop {
        let mut random = vec![0u8; len * 2];
        if getrandom::fill(&mut random).is_err() {
            return String::new();
        }
        // Без перекоса: байт берётся, только если он ниже кратного размеру набора.
        let limit = 256 - 256 % all.len();
        let out: Vec<u8> = random.iter().filter(|&&b| (b as usize) < limit).map(|&b| all[b as usize % all.len()]).take(len).collect();
        if out.len() == len && sets.iter().all(|set| out.iter().any(|c| set.contains(c))) {
            return String::from_utf8(out).unwrap_or_default();
        }
    }
}

/// Сколько раз пароль встречался в утечках (Have I Been Pwned). На сервер
/// уходят только первые 5 знаков SHA-1 пароля — сам пароль и даже его
/// отпечаток целиком никуда не уходят (k-анонимность).
pub fn leak_count(password: &str) -> Result<u64, String> {
    let digest = Sha1::digest(password.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02X}")).collect();
    let (prefix, suffix) = hex.split_at(5);
    let request = Request::new("GET", &format!("https://api.pwnedpasswords.com/range/{prefix}"))
        .header("User-Agent", "OBSOS-FreeOS")
        .header("Add-Padding", "true");
    let response = crate::net::Client.send(&request)?;
    if response.status != 200 {
        return Err(format!("сервер ответил {}", response.status));
    }
    let text = String::from_utf8_lossy(&response.body);
    Ok(text
        .lines()
        .filter_map(|l| l.trim().split_once(':'))
        .find(|(s, _)| s.eq_ignore_ascii_case(suffix))
        .and_then(|(_, n)| n.trim().parse().ok())
        .unwrap_or(0))
}

/// Закладки из файла HTML (формат Netscape: `<DT><A HREF="…">Имя</A>`).
pub fn parse_bookmarks_html(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = text.to_ascii_lowercase();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find("<a ") {
        let start = pos + i;
        let Some(tag_end) = lower[start..].find('>').map(|e| start + e) else { break };
        let tag = &text[start..tag_end];
        let close = lower[tag_end..].find("</a>").map(|e| tag_end + e).unwrap_or(tag_end + 1);
        let title = html_unescape(text[tag_end + 1..close.max(tag_end + 1)].trim());
        if let Some(href) = attr(tag, "href") {
            if href.starts_with("http://") || href.starts_with("https://") {
                out.push((html_unescape(&href), if title.is_empty() { html_unescape(&href) } else { title }));
            }
        }
        pos = close.max(tag_end + 1);
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))?;
    let rest = &tag[i + name.len() + 1..];
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        rest[1..].split(quote).next().map(str::to_string)
    } else {
        rest.split(|c: char| c.is_whitespace() || c == '>').next().map(str::to_string)
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&quot;", "\"").replace("&#39;", "'").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Закладки в HTML Netscape — его читают Chrome, Firefox, Edge.
pub fn bookmarks_html(items: &[(String, String)]) -> String {
    let mut out = String::from("<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n<DT><H3>OBSOS</H3>\n<DL><p>\n");
    for (url, title) in items {
        out.push_str(&format!("<DT><A HREF=\"{}\">{}</A>\n", html_escape(url), html_escape(title)));
    }
    out.push_str("</DL><p>\n</DL><p>\n");
    out
}

/// Разбор CSV (кавычки, `""` внутри, переводы строк в кавычках).
fn csv_rows(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            ('"', true) => quoted = false,
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\n', false) => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            ('\r', false) => {}
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|f| !f.is_empty()));
    rows
}

/// Пароли из CSV Chrome (`name,url,username,password,note`) или Firefox
/// (`"url","username","password",…`): колонки ищутся по заголовку.
pub fn parse_passwords_csv(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let rows = csv_rows(text.trim_start_matches('\u{feff}'));
    let header: Vec<String> = rows.first().ok_or("файл пустой")?.iter().map(|h| h.trim().to_ascii_lowercase()).collect();
    let col = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let (Some(url), Some(user), Some(pass)) = (col(&["url", "origin", "login_uri", "website"]), col(&["username", "login", "user", "login_username"]), col(&["password", "login_password"])) else {
        return Err(String::from("нет колонок url, username, password"));
    };
    Ok(rows
        .iter()
        .skip(1)
        .filter_map(|r| {
            let origin = url::Url::parse(r.get(url)?.trim()).ok().and_then(|u| crate::vault::origin_of(&u))?;
            Some((origin, r.get(user).cloned().unwrap_or_default(), r.get(pass)?.clone()))
        })
        .filter(|(_, _, p)| !p.is_empty())
        .collect())
}

fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// Пароли в CSV Chrome (`name,url,username,password,note`).
pub fn passwords_csv(items: &[(String, String, String)]) -> String {
    let mut out = String::from("name,url,username,password,note\n");
    for (origin, user, pass) in items {
        let name = origin.split("://").nth(1).unwrap_or(origin);
        out.push_str(&format!("{},{},{},{},\n", csv_field(name), csv_field(origin), csv_field(user), csv_field(pass)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generator() {
        let p = generate_password(20, true);
        assert_eq!(p.len(), 20);
        assert!(p.chars().any(|c| c.is_ascii_digit()) && p.chars().any(|c| c.is_ascii_uppercase()));
        assert!(!p.contains(['l', '1', 'I', 'O', '0']));
    }

    #[test]
    fn imports() {
        let html = "<DL><p><DT><A HREF=\"https://a.ru/?x=1&amp;y=2\" ADD_DATE=\"1\">Сайт &quot;А&quot;</A>\n<DT><a href='https://b.org'></a><DT><A HREF=\"javascript:x\">no</A>";
        assert_eq!(parse_bookmarks_html(html), [("https://a.ru/?x=1&y=2".into(), "Сайт \"А\"".into()), ("https://b.org".into(), "https://b.org".into())]);
        let csv = "name,url,username,password,note\nex,https://ex.com/login,bob,\"p,\"\"w\",\n";
        assert_eq!(parse_passwords_csv(csv).unwrap(), [("https://ex.com".into(), "bob".into(), "p,\"w".into())]);
        let back = passwords_csv(&[("https://ex.com".into(), "bob".into(), "p,\"w".into())]);
        assert_eq!(parse_passwords_csv(&back).unwrap()[0].2, "p,\"w");
        let ff = "\"url\",\"username\",\"password\",\"httpRealm\"\n\"https://x.org\",\"\",\"s\",\"\"\n";
        assert_eq!(parse_passwords_csv(ff).unwrap(), [("https://x.org".into(), String::new(), "s".into())]);
    }
}
