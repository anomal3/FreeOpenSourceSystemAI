//! Внутренние страницы `obsos://` — своя схема в реестре протоколов Servo
//! (как `data:` и `file:`), без сети:
//!
//! * `obsos://newtab` — новая вкладка: поиск, закладки плитками;
//! * `obsos://about` — о программе, авторство, честный список того, чего
//!   движок пока не умеет;
//! * `obsos://licenses` — `NOTICE.md` пакета;
//! * `obsos://view?path=…` — текст и Markdown с диска (Servo сам их не
//!   показывает: «Unknown content type»);
//! * `obsos://crashed?url=…&reason=…` — процесс страницы упал (вместо
//!   замёрзшей картинки);
//! * `obsos://unsupported?url=…&what=…` — страница требует того, чего нет в
//!   Servo: что именно, и кнопка «Открыть в Chromium».
//!
//! Ссылки `obsos://action/…` — команды оболочке (их перехватывает
//! `request_navigation`, страница никуда не уходит).

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, Request, ResourceFetchTiming, Response, ResponseBody,
};

/// Что страницам нужно знать об оболочке (обновляет главный поток).
#[derive(Clone, Default)]
pub struct PageData {
    pub bookmarks: Vec<(String, String)>,
    pub search_action: String,
    pub search_field: String,
    pub search_title: String,
    pub dark: bool,
    pub accent: String,
    pub accent2: String,
    pub chromium: bool,
    pub notice_path: String,
}

pub type Shared = Arc<Mutex<PageData>>;

pub struct ObsosProtocol {
    pub data: Shared,
}

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn query(url: &url::Url, key: &str) -> String {
    url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned()).unwrap_or_default()
}

fn enc(text: &str) -> String {
    url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn style(d: &PageData) -> String {
    let (bg, fg, card, border, dim) = if d.dark {
        ("linear-gradient(160deg,#1b1f3a,#2a1f4a 55%,#123a44)", "#e8ecf8", "rgba(255,255,255,.07)", "rgba(255,255,255,.13)", ".72")
    } else {
        ("linear-gradient(160deg,#f4f6fb,#efeafb 55%,#e6f5f4)", "#1d2433", "rgba(255,255,255,.75)", "rgba(29,36,51,.12)", ".68")
    };
    format!(
        "body{{margin:0;font-family:sans-serif;background:{bg};color:{fg};min-height:100vh}}\
.wrap{{max-width:760px;margin:0 auto;padding:48px 28px 40px}}\
h1{{margin:0;font-size:42px;letter-spacing:2px}} h2{{margin:28px 0 10px;font-size:22px}}\
.sub{{margin:6px 0 24px;opacity:{dim};font-size:15px}}\
.card{{padding:16px 20px;border-radius:12px;background:{card};border:1px solid {border};line-height:1.55;margin:0 0 14px}}\
a{{color:{a2}}} code,pre{{font-family:monospace;background:rgba(127,127,127,.15);border-radius:6px}} code{{padding:1px 5px}} pre{{padding:12px;overflow:auto}}\
form{{display:flex;margin:0 0 28px}}\
input{{flex:1;font-size:17px;padding:12px 16px;border-radius:10px 0 0 10px;border:1px solid {border};background:{card};color:{fg}}}\
button,.btn{{font-size:16px;padding:11px 20px;border:0;border-radius:10px;background:linear-gradient(90deg,{a1},{a2});color:#fff;text-decoration:none;display:inline-block;margin:6px 10px 0 0}}\
form button{{border-radius:0 10px 10px 0;margin:0}}\
.btn.plain{{background:{card};color:{fg};border:1px solid {border}}}\
.tiles{{display:flex;flex-wrap:wrap}}\
.tiles a{{display:block;width:150px;margin:0 14px 14px 0;padding:14px;border-radius:12px;background:{card};border:1px solid {border};color:{fg};text-decoration:none;overflow:hidden}}\
.tiles b{{display:block;font-size:17px;margin-bottom:6px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}\
.tiles span{{font-size:12px;opacity:.7;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;display:block}}\
.small{{font-size:13px;opacity:{dim}}} li{{margin:4px 0}}",
        a1 = d.accent,
        a2 = d.accent2,
    )
}

fn page(d: &PageData, title: &str, body: &str) -> String {
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>{}</title><style>{}</style></head><body><div class=\"wrap\">{body}</div></body></html>",
        escape(title),
        style(d)
    )
}

const LIMITS: &str = "<ul>\
<li><b>Потоковое видео</b> (YouTube и подобные) не работает: им нужны Media Source Extensions, которых в Servo нет. \
OBSOS говорит об этом прямо на такой странице и предлагает открыть её в Chromium.</li>\
<li><b>WebGL и аппаратное ускорение</b> — нет: страницу рисует процессор (swgl). Нужен <code>dlopen</code> и Mesa — это в планах ОС.</li>\
<li><b>Расширения</b> — движок их не поддерживает; вместо них — пользовательские скрипты и стили \
(<code>~/.config/obsos/&lt;профиль&gt;/userscripts/*.js</code>, <code>*.css</code>).</li>\
<li><b>Подстановка паролей</b> работает через скрипт в странице (перехват отправки формы и заполнение полей) и для HTTP-входа; \
сайты со сложными формами на JavaScript могут её не заметить.</li>\
<li>Servo молод: возможны поломанная вёрстка, отсутствующие API и сайты, не принимающие его User-Agent. \
Для них — «Открыть в Chromium» в меню.</li></ul>";

fn newtab(d: &PageData) -> String {
    let mut tiles = String::new();
    let defaults = [
        ("https://servo.org/", "Servo"),
        ("https://ru.wikipedia.org/", "Википедия"),
        ("https://github.com/", "GitHub"),
        ("https://www.openstreetmap.org/", "Карты"),
        ("https://example.com/", "Example"),
    ];
    let marks: Vec<(String, String)> = if d.bookmarks.is_empty() {
        defaults.iter().map(|(u, t)| (u.to_string(), t.to_string())).collect()
    } else {
        d.bookmarks.iter().take(11).cloned().collect()
    };
    for (url, title) in &marks {
        let host = url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| url.clone());
        let title = if title.trim().is_empty() { host.clone() } else { title.clone() };
        tiles.push_str(&format!("<a href=\"{}\"><b>{}</b><span>{}</span></a>", escape(url), escape(&title), escape(&host)));
    }
    tiles.push_str("<a href=\"obsos://about\"><b>О программе</b><span>obsos://about</span></a>");
    let body = format!(
        "<h1>OBSOS</h1><p class=\"sub\">Open Browser for System Open Source — браузер FreeOS</p>\
<p id=\"hello\" style=\"font-size:20px;margin:0 0 18px\">Привет!</p>\
<form action=\"{action}\" method=\"get\"><input name=\"{field}\" placeholder=\"Поиск: {engine}\" autofocus><button>Найти</button></form>\
<div class=\"tiles\">{tiles}</div>\
<p class=\"small\">Движок Servo — © The Servo Project Developers (MPL-2.0). Оболочка — своя, на egui; дизайн по мотивам Zervo — © Goddv \
(MPL-2.0). <a href=\"obsos://licenses\">Лицензии</a> · <a href=\"obsos://about\">О программе</a></p>\
<script>var h=new Date().getHours();document.getElementById('hello').textContent=\
(h<5?'Доброй ночи':h<12?'Доброе утро':h<18?'Добрый день':'Добрый вечер')+' — JavaScript работает.';</script>",
        action = escape(&d.search_action),
        field = escape(&d.search_field),
        engine = escape(&d.search_title),
    );
    page(d, "Новая вкладка", &body)
}

fn about(d: &PageData) -> String {
    let body = format!(
        "<h1>OBSOS</h1><p class=\"sub\">Open Browser for System Open Source · версия {}</p>\
<div class=\"card\"><b>Движок</b> — Servo {} (© The Servo Project Developers, <a href=\"https://servo.org\">servo.org</a>, MPL-2.0); \
JavaScript — SpiderMonkey, рисование — swgl/WebRender на процессоре (© Mozilla, MPL-2.0).<br>\
<b>Оболочка</b> — своя, на egui (MIT/Apache-2.0); дизайн по мотивам Zervo — © Goddv, \
<a href=\"https://github.com/Goddv/Zervo\">github.com/Goddv/Zervo</a> (MPL-2.0).<br>\
Текст MPL-2.0 — <a href=\"https://mozilla.org/MPL/2.0/\">mozilla.org/MPL/2.0</a>; все компоненты — <a href=\"obsos://licenses\">лицензии</a>.</div>\
<h2>Синхронизация</h2><div class=\"card\">Закладки, история, оформление и пароли — в ваш репозиторий GitHub, GitLab или Bitbucket \
(каждая синхронизация — коммит, точки — теги, к ним можно откатиться) или на Яндекс Диск, Google Диск, WebDAV, в папку. \
Пароли шифруются на устройстве (Argon2id + XChaCha20-Poly1305): площадка видит только шифротекст.</div>\
<h2>Чего движок пока не умеет</h2><div class=\"card\">{LIMITS}</div>",
        env!("CARGO_PKG_VERSION"),
        "0.8"
    );
    page(d, "О программе OBSOS", &body)
}

/// Маленький Markdown: заголовки, списки, код, ссылки, жирный, абзацы.
pub fn markdown(text: &str) -> String {
    fn inline(line: &str) -> String {
        let escaped = escape(line);
        let mut out = String::new();
        let mut rest = escaped.as_str();
        while !rest.is_empty() {
            if let Some(code) = rest.strip_prefix('`') {
                if let Some(end) = code.find('`') {
                    out.push_str(&format!("<code>{}</code>", &code[..end]));
                    rest = &code[end + 1..];
                    continue;
                }
            }
            if let Some(bold) = rest.strip_prefix("**") {
                if let Some(end) = bold.find("**") {
                    out.push_str(&format!("<b>{}</b>", &bold[..end]));
                    rest = &bold[end + 2..];
                    continue;
                }
            }
            if let Some(link) = rest.strip_prefix('[') {
                if let Some(mid) = link.find("](") {
                    if let Some(end) = link[mid + 2..].find(')') {
                        let href = &link[mid + 2..mid + 2 + end];
                        out.push_str(&format!("<a href=\"{href}\">{}</a>", &link[..mid]));
                        rest = &link[mid + 3 + end..];
                        continue;
                    }
                }
            }
            if rest.starts_with("https://") || rest.starts_with("http://") {
                let end = rest.find(|c: char| c.is_whitespace() || c == ')' || c == ',' || c == ';').unwrap_or(rest.len());
                let href = rest[..end].trim_end_matches('.');
                out.push_str(&format!("<a href=\"{href}\">{href}</a>"));
                rest = &rest[href.len()..];
                continue;
            }
            let c = rest.chars().next().unwrap_or(' ');
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
        out
    }
    let mut out = String::new();
    let mut in_code = false;
    let mut list: Option<&str> = None;
    let mut para = String::new();
    let flush = |out: &mut String, para: &mut String| {
        if !para.trim().is_empty() {
            out.push_str(&format!("<p>{}</p>", inline(para.trim())));
        }
        para.clear();
    };
    // Таблица GitHub: подряд идущие строки `| … |`, вторая — `|---|---|`.
    let table = |out: &mut String, rows: &mut Vec<String>| {
        if rows.is_empty() {
            return;
        }
        let cells = |row: &str| -> Vec<String> {
            row.trim().trim_matches('|').split('|').map(|c| inline(c.trim())).collect()
        };
        let rule = |row: &str| row.trim().trim_matches('|').split('|').all(|c| {
            let c = c.trim();
            !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
        });
        out.push_str("<table style=\"border-collapse:collapse;margin:12px 0\">");
        let head = rows.len() > 1 && rule(&rows[1]);
        for (i, row) in rows.iter().enumerate() {
            if head && i == 1 {
                continue;
            }
            let tag = if head && i == 0 { "th" } else { "td" };
            out.push_str("<tr>");
            for c in cells(row) {
                out.push_str(&format!("<{tag} style=\"border:1px solid rgba(128,128,160,.35);padding:6px 10px;text-align:left;vertical-align:top\">{c}</{tag}>"));
            }
            out.push_str("</tr>");
        }
        out.push_str("</table>");
        rows.clear();
    };
    let mut rows: Vec<String> = Vec::new();
    for line in text.lines() {
        if !in_code && line.trim_start().starts_with('|') {
            flush(&mut out, &mut para);
            if let Some(old) = list.take() {
                out.push_str(&format!("</{old}>"));
            }
            rows.push(line.trim().to_string());
            continue;
        }
        table(&mut out, &mut rows);
        if line.trim_start().starts_with("```") {
            flush(&mut out, &mut para);
            out.push_str(if in_code { "</pre>" } else { "<pre>" });
            in_code = !in_code;
            continue;
        }
        if in_code {
            out.push_str(&escape(line));
            out.push('\n');
            continue;
        }
        let trimmed = line.trim();
        let item = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).map(|t| ("ul", t)).or_else(|| {
            let digits = trimmed.find(". ").filter(|&i| i > 0 && trimmed[..i].chars().all(|c| c.is_ascii_digit()))?;
            Some(("ol", &trimmed[digits + 2..]))
        });
        if let Some((kind, body)) = item {
            flush(&mut out, &mut para);
            if list != Some(kind) {
                if let Some(old) = list {
                    out.push_str(&format!("</{old}>"));
                }
                out.push_str(&format!("<{kind}>"));
                list = Some(kind);
            }
            out.push_str(&format!("<li>{}</li>", inline(body)));
            continue;
        }
        if let Some(old) = list.take() {
            if trimmed.is_empty() || trimmed.starts_with('#') {
                out.push_str(&format!("</{old}>"));
            } else {
                list = Some(old);
                para.push_str(trimmed);
                para.push(' ');
                continue;
            }
        }
        if trimmed.is_empty() {
            flush(&mut out, &mut para);
        } else if let Some(level) = (1..=6).rev().find(|&n| trimmed.starts_with(&format!("{} ", "#".repeat(n)))) {
            flush(&mut out, &mut para);
            out.push_str(&format!("<h{level}>{}</h{level}>", inline(trimmed[level + 1..].trim())));
        } else {
            para.push_str(trimmed);
            para.push(' ');
        }
    }
    flush(&mut out, &mut para);
    table(&mut out, &mut rows);
    if let Some(old) = list {
        out.push_str(&format!("</{old}>"));
    }
    if in_code {
        out.push_str("</pre>");
    }
    out
}

fn view_file(d: &PageData, path: &str) -> (u16, String) {
    let shown = escape(path);
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let name = path.rsplit('/').next().unwrap_or(path);
            let body = if path.ends_with(".md") || path.ends_with(".markdown") {
                format!("<div class=\"card\">{}</div><p class=\"small\">{shown}</p>", markdown(&text))
            } else {
                format!("<p class=\"small\">{shown}</p><pre>{}</pre>", escape(&text))
            };
            (200, page(d, name, &body))
        }
        Err(e) => (404, page(d, "Файл не найден", &format!("<h1>Файл не открылся</h1><div class=\"card\">{shown}: {}</div>", escape(&e.to_string())))),
    }
}

fn chromium_button(d: &PageData, url: &str) -> String {
    if d.chromium {
        format!("<a class=\"btn\" href=\"obsos://action/chromium?url={}\">Открыть в Chromium</a>", enc(url))
    } else {
        String::from("<span class=\"small\">Chromium не установлен — его можно поставить пакетом в «Пакетах».</span>")
    }
}

fn crashed(d: &PageData, url: &str, reason: &str) -> String {
    let body = format!(
        "<h1>Страница упала</h1><p class=\"sub\">{}</p><div class=\"card\">Движок Servo не справился с этой страницей и остановил её \
(вместо того чтобы показывать замёрзшую картинку). Причина: <code>{}</code></div>\
<a class=\"btn\" href=\"{}\">Открыть снова</a>{}<a class=\"btn plain\" href=\"obsos://newtab\">Новая вкладка</a>",
        escape(url),
        escape(if reason.is_empty() { "неизвестна" } else { reason }),
        escape(url),
        chromium_button(d, url)
    );
    page(d, "Страница упала", &body)
}

fn unsupported(d: &PageData, url: &str, what: &str) -> String {
    let explain = match what {
        "MediaSource" => "Сайт показывает видео через <b>Media Source Extensions</b> (потоковое видео: YouTube и подобные). В движке Servo этого пока нет — обычное и локальное видео играет, потоковое нет.",
        "content-type" => "Servo не умеет показывать файлы этого типа.",
        _ => "Странице нужно то, чего в движке Servo пока нет.",
    };
    let body = format!(
        "<h1>Здесь Servo не справится</h1><p class=\"sub\">{}</p><div class=\"card\">{explain}<br><span class=\"small\">Не хватает: <code>{}</code></span></div>\
{}<a class=\"btn plain\" href=\"obsos://action/force?url={}\">Всё равно открыть здесь</a>",
        escape(url),
        escape(what),
        chromium_button(d, url),
        enc(url)
    );
    page(d, "Не поддерживается", &body)
}

/// Страница для адреса `obsos://…`: (код, HTML).
pub fn render(d: &PageData, url: &url::Url) -> (u16, String) {
    match url.host_str().unwrap_or("") {
        "newtab" | "" => (200, newtab(d)),
        "about" => (200, about(d)),
        "licenses" => view_file(d, &d.notice_path),
        "view" => view_file(d, &query(url, "path")),
        "crashed" => (200, crashed(d, &query(url, "url"), &query(url, "reason"))),
        "unsupported" => (200, unsupported(d, &query(url, "url"), &query(url, "what"))),
        other => (404, page(d, "Нет такой страницы", &format!("<h1>Нет такой страницы</h1><div class=\"card\">obsos://{}</div>", escape(other)))),
    }
}

impl ProtocolHandler for ObsosProtocol {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let parsed = url::Url::parse(url.as_str()).ok();
        let data = self.data.lock().map(|d| d.clone()).unwrap_or_default();
        let (code, html) = match parsed {
            Some(u) => render(&data, &u),
            None => (400, String::from("bad url")),
        };
        let mut response = Response::new(url, ResourceFetchTiming::new(request.timing_type()));
        *response.body.lock() = ResponseBody::Done(html.into_bytes());
        response.headers.insert(http::header::CONTENT_TYPE, http::HeaderValue::from_static("text/html; charset=utf-8"));
        response.status = if code == 200 { HttpStatus::default() } else { HttpStatus::new_raw(code, Vec::new()) };
        Box::pin(std::future::ready(response))
    }
}

/// Пути, которые Servo сам не покажет, — в `obsos://view`.
pub fn viewable_path(path: &str) -> bool {
    [".md", ".markdown", ".txt", ".log", ".json", ".toml", ".rs", ".c", ".h", ".py", ".sh", ".cfg", ".ini", ".csv"]
        .iter()
        .any(|ext| path.to_ascii_lowercase().ends_with(ext))
}

pub fn view_url(path: &str) -> url::Url {
    url::Url::parse(&format!("obsos://view?path={}", enc(path))).expect("view url")
}

pub fn crashed_url(url: &str, reason: &str) -> url::Url {
    url::Url::parse(&format!("obsos://crashed?url={}&reason={}", enc(url), enc(reason))).expect("crashed url")
}

pub fn unsupported_url(url: &str, what: &str) -> url::Url {
    url::Url::parse(&format!("obsos://unsupported?url={}&what={}", enc(url), enc(what))).expect("unsupported url")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_basics() {
        let html = markdown("# Title\n\nText **bold** `code` [link](https://a.b/).\n\n- one\n- two\n\n```\n<x>\n```\n");
        assert!(html.contains("<h1>Title</h1>"), "{html}");
        assert!(html.contains("<b>bold</b>") && html.contains("<code>code</code>"), "{html}");
        assert!(html.contains("<a href=\"https://a.b/\">link</a>"), "{html}");
        assert!(html.contains("<ul><li>one</li><li>two</li></ul>"), "{html}");
        assert!(html.contains("<pre>&lt;x&gt;\n</pre>"), "{html}");
    }

    #[test]
    fn markdown_table() {
        let html = markdown("Intro\n| A | B |\n|---|:-:|\n| 1 | **2** |\nAfter\n");
        assert!(html.contains("<p>Intro</p><table"), "{html}");
        assert!(html.contains(">A</th>") && html.contains(">B</th>"), "{html}");
        assert!(html.contains("><b>2</b></td>") && !html.contains("---"), "{html}");
        assert!(html.ends_with("</table><p>After</p>"), "{html}");
    }
}
