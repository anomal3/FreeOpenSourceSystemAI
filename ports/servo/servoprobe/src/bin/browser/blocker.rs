//! Блокировка рекламы и трекеров: запросы страницы к известным рекламным и
//! следящим доменам отменяются ещё до сети (`WebViewDelegate::load_web_resource`).
//! Чем меньше чужих скриптов, тем легче программной отрисовке swgl.
//!
//! Правила — домены: встроенный короткий список, плюс `filters.txt` профиля
//! (EasyList/EasyPrivacy, скачиваются из настроек: берутся правила вида
//! `||домен^`), плюс свой `blocklist.txt` (домены, формат hosts или
//! `||домен^`). Исключения — сайты, на которых блокировка выключена
//! (`allow.txt`, щит в адресной строке).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Самые частые рекламные и следящие домены — чтобы блокировка работала и
/// без скачанных списков.
const BUILTIN: &[&str] = &[
    "doubleclick.net", "googlesyndication.com", "googleadservices.com", "google-analytics.com",
    "googletagmanager.com", "googletagservices.com", "adservice.google.com", "pagead2.googlesyndication.com",
    "analytics.google.com", "connect.facebook.net", "pixel.facebook.com", "an.facebook.com",
    "scorecardresearch.com", "quantserve.com", "adnxs.com", "criteo.com", "criteo.net", "taboola.com",
    "outbrain.com", "amazon-adsystem.com", "adsrvr.org", "rubiconproject.com", "pubmatic.com", "openx.net",
    "casalemedia.com", "moatads.com", "hotjar.com", "mc.yandex.ru", "an.yandex.ru", "yandexadexchange.net",
    "adfox.ru", "adfox.yandex.ru", "top-fwz1.mail.ru", "counter.yadro.ru", "ad.mail.ru", "mixpanel.com",
    "cdn.segment.com", "api.segment.io", "adform.net", "smartadserver.com", "teads.tv", "yieldmo.com",
    "bidswitch.net", "media.net", "zedo.com", "3lift.com", "sharethrough.com", "advertising.com",
    "adtechus.com", "serving-sys.com", "mathtag.com", "everesttech.net", "demdex.net", "omtrdc.net",
    "krxd.net", "bluekai.com", "exelator.com", "tapad.com", "rlcdn.com", "agkn.com", "adsafeprotected.com",
    "doubleverify.com", "branch.io", "app-measurement.com", "clarity.ms", "bat.bing.com", "ads.linkedin.com",
    "ads-twitter.com", "static.ads-twitter.com", "analytics.tiktok.com", "ads.tiktok.com", "popads.net",
    "propellerads.com", "adsterra.com", "exoclick.com", "trafficjunky.net", "mgid.com", "marketgid.com",
    "directadvert.ru", "begun.ru", "relap.io", "smi2.ru", "smi2.net", "lentainform.com", "tns-counter.ru",
    "mediametrics.ru", "liveinternet.ru", "counter.rambler.ru", "top100.rambler.ru", "vk-analytics.com",
];

pub struct Blocker {
    pub enabled: bool,
    domains: HashSet<String>,
    pub allow: HashSet<String>,
    dir: PathBuf,
    /// Сколько правил из скачанных списков.
    pub filter_rules: usize,
    pub blocked_total: u64,
}

impl Blocker {
    pub fn load(dir: &Path, enabled: bool) -> Self {
        let mut domains: HashSet<String> = BUILTIN.iter().map(|d| d.to_string()).collect();
        let filters = read_list(&dir.join("filters.txt"));
        let filter_rules = filters.len();
        domains.extend(filters);
        domains.extend(read_list(&dir.join("blocklist.txt")));
        let allow = read_list(&dir.join("allow.txt")).into_iter().collect();
        println!("browser: blocker {} domains ({} from lists)", domains.len(), filter_rules);
        Blocker { enabled, domains, allow, dir: dir.to_path_buf(), filter_rules, blocked_total: 0 }
    }

    /// Блокировать ли запрос к `host` со страницы сайта `site`.
    pub fn blocks(&self, host: &str, site: Option<&str>) -> bool {
        if !self.enabled || site.is_some_and(|s| self.allows(s)) {
            return false;
        }
        // Сам сайт свой ресурс не блокирует (реклама на её же домене — редкость,
        // а сломанный сайт — частая).
        if let Some(site) = site {
            if same_site(host, site) {
                return false;
            }
        }
        domain_suffixes(host).any(|d| self.domains.contains(d))
    }

    pub fn allows(&self, site: &str) -> bool {
        domain_suffixes(site).any(|d| self.allow.contains(d))
    }

    /// Включить или выключить блокировку на сайте (щит в адресной строке).
    pub fn toggle_site(&mut self, site: &str) -> bool {
        let site = site.trim_start_matches("www.").to_string();
        let now_allowed = if self.allows(&site) {
            self.allow.retain(|a| !domain_suffixes(&site).any(|d| d == a));
            false
        } else {
            self.allow.insert(site);
            true
        };
        let mut list: Vec<&String> = self.allow.iter().collect();
        list.sort();
        let text: String = list.iter().map(|d| format!("{d}\n")).collect();
        let _ = std::fs::write(self.dir.join("allow.txt"), text);
        now_allowed
    }

    /// Сохранить скачанный список фильтров; возвращает число доменов.
    pub fn store_filters(&mut self, text: &str) -> usize {
        let parsed = parse_list(text);
        let mut sorted: Vec<&String> = parsed.iter().collect();
        sorted.sort();
        sorted.dedup();
        let out: String = sorted.iter().map(|d| format!("{d}\n")).collect();
        let _ = std::fs::write(self.dir.join("filters.txt"), out);
        let count = sorted.len();
        self.filter_rules = count;
        self.domains.extend(parsed);
        count
    }
}

fn read_list(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path).map(|t| parse_list(&t)).unwrap_or_default()
}

/// Домены из списка: строки `домен`, `0.0.0.0 домен` (hosts) и правила
/// Adblock `||домен^` (с параметрами — только `$third-party` и типы; правила
/// с путями, исключения `@@` и косметика `##` пропускаются).
pub fn parse_list(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('!') || line.starts_with('#') || line.starts_with('[') || line.starts_with("@@") || line.contains("##") || line.contains("#@#") {
            continue;
        }
        let domain = if let Some(rule) = line.strip_prefix("||") {
            let (body, options) = rule.split_once('$').unwrap_or((rule, ""));
            if options.split(',').any(|o| o.starts_with("domain=") || o.starts_with('~') || o == "popup" || o.starts_with("csp") || o.starts_with("redirect")) {
                continue;
            }
            let Some(host) = body.strip_suffix('^').or_else(|| body.strip_suffix("^|")) else { continue };
            host
        } else {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("0.0.0.0" | "127.0.0.1" | "::"), Some(host)) => host,
                (Some(host), None) => host,
                _ => continue,
            }
        };
        let domain = domain.to_ascii_lowercase();
        if domain.contains('.') && domain.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') && domain != "localhost" {
            out.push(domain);
        }
    }
    out
}

/// `a.b.example.com` → `a.b.example.com`, `b.example.com`, `example.com`, `com`.
fn domain_suffixes(host: &str) -> impl Iterator<Item = &str> {
    let host = host.trim_end_matches('.');
    std::iter::successors(Some(host), |h| h.split_once('.').map(|(_, rest)| rest))
}

/// Один ли «сайт» (последние два уровня имени; для `.co.uk`-подобных — три).
fn same_site(a: &str, b: &str) -> bool {
    fn base(h: &str) -> &str {
        let parts: Vec<&str> = h.trim_end_matches('.').rsplitn(4, '.').collect();
        let keep = if parts.len() >= 3 && parts[1].len() <= 3 && parts[0].len() == 2 { 3 } else { 2 };
        let skip: usize = parts.iter().take(keep).map(|p| p.len() + 1).sum::<usize>();
        &h[h.len().saturating_sub(skip.saturating_sub(1))..]
    }
    base(a) == base(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_matching() {
        let text = "! comment\n||ads.example.com^\n||track.net^$third-party\n||x.com/path^\n@@||good.com^\nexample.org##.ad\n0.0.0.0 hosts.example\nplain.example\n||only.on^$domain=a.com\n";
        assert_eq!(parse_list(text), ["ads.example.com", "track.net", "hosts.example", "plain.example"]);
        let mut b = Blocker { enabled: true, domains: parse_list(text).into_iter().collect(), allow: HashSet::new(), dir: PathBuf::new(), filter_rules: 0, blocked_total: 0 };
        assert!(b.blocks("cdn.track.net", Some("news.ru")));
        assert!(!b.blocks("track.net", Some("www.track.net")));
        assert!(!b.blocks("news.ru", Some("news.ru")));
        b.allow.insert(String::from("news.ru"));
        assert!(!b.blocks("cdn.track.net", Some("www.news.ru")));
        assert!(same_site("a.bbc.co.uk", "www.bbc.co.uk") && !same_site("a.co.uk", "b.co.uk"));
    }
}
