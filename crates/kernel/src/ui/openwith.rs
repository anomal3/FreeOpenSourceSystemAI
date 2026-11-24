//! «Открыть с помощью» (22.11.2026): какие программы открывают файл.
//!
//! Роман: у `.txt` в «Открыть с помощью» должен быть Notepad++, поставленный
//! в префикс Wine. Источники, как у Проводника:
//! - свои программы: «Блокнот» (текст) и пакеты с `opens=` в манифесте;
//! - программы Windows: сопоставления из реестра префикса (`system.reg`,
//!   `user.reg`, ветка `Software\Classes`): `.ext` → ProgID (значение по
//!   умолчанию и `OpenWithProgids`) → `shell\open\command`; `.ext\OpenWithList`
//!   и `Applications\<программа>.exe` с `SupportedTypes`. Программа из
//!   `Applications` без `SupportedTypes` (так регистрирует себя Notepad++)
//!   открывает, как в Windows, что угодно — в список «подходит» она попадает
//!   для текстовых файлов, для прочих — в «Другие программы».
//! - «Выбрать другую программу…»: все «Программы Windows» (`ui::winapps`) и
//!   все свои программы с запуском.
//!
//! Программа Windows запускается `fossai-proton --with=ПРОГРАММА ФАЙЛ`:
//! сценарий берёт префикс из пути программы и переводит путь файла в путь
//! Windows (буква тома из `dosdevices` или `Z:`). Выбор «всегда» с 24.11
//! хранится в хранилище настроек (`config::registry`): ключ
//! `filetypes/.ext/default` области пользователя (`~/.config/registry/
//! filetypes.reg`); прежняя строка `open_with=` в `desktop.cfg` ещё читается.
//! Умолчания системы — там же, в области `system` и встроенные (OBSOS для
//! `.html .htm .xhtml .svg .url`); программы, зарегистрированные ключами
//! `filetypes/.ext/openwith/<id>`, идут в список подходящих.
//!
//! «Файлы» (программа) получают тот же список вызовом
//! [`user_abi::SYS_OPEN_WITH`], значки — картинками FIMG.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use mini_ui::image::Image;

use super::appicon::{self, Art};

/// Программа, которой можно открыть файл.
#[derive(Clone, Debug)]
pub struct Handler {
    pub caption: String,
    /// Командная строка без файла; путь допишет [`line`].
    pub command: String,
    /// Ключ значка для [`icon_fimg`]: `/winicon/…`, `manifest:ИМЯ`,
    /// `program:ПУТЬ`; пусто — значка нет.
    pub icon: String,
    /// Открывает файлы этого типа (иначе — из «Других программ»).
    pub fits: bool,
    /// Выбрана человеком («всегда открывать этой программой»).
    pub chosen: bool,
    /// Программа Windows (через Proton).
    pub windows: bool,
}

/// Строка запуска `command` для файла `path`. `fossai-proton` получает путь
/// с `%20` вместо пробела: ядро делит строку по пробелам.
#[must_use]
pub fn line(command: &str, path: &str) -> String {
    // Internet shortcut (`.url`, 24.11): a program registered for `.url` (the
    // browser) gets the address inside, not the file.
    if path.to_ascii_lowercase().ends_with(".url") && crate::config::registry::registered_programs("url").iter().any(|(_, known)| known == command) {
        if let Some(Ok((bytes, _))) = crate::fs::read(path, 8192) {
            if let Some(url) = sysconf::store::url_of_shortcut(&String::from_utf8_lossy(&bytes)).filter(|url| !url.contains(char::is_whitespace)) {
                return alloc::format!("{command} {url}");
            }
        }
    }
    if command.contains(super::panel::PROTON_SCRIPT) {
        alloc::format!("{command} {}", super::panel::pct(path))
    } else {
        alloc::format!("{command} {path}")
    }
}

/// Команда запуска программы Windows `exe` (путь в префиксе) с файлом.
#[must_use]
pub fn windows_command(exe: &str) -> String {
    super::panel::proton_line(&alloc::format!("--with={}", super::panel::pct(exe)))
}

/// То же с ключами из реестра (`open "%1"` у `"C:\…\x.exe" open "%1"`):
/// `%1` сценарий заменит путём Windows файла.
fn windows_command_args(exe: &str, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() || args == "\"%1\"" || args == "%1" {
        return windows_command(exe);
    }
    super::panel::proton_line(&alloc::format!("--with={} --args={}", super::panel::pct(exe), super::panel::pct(args)))
}

/// Все программы для файла `path`: выбранная — первой, затем подходящие,
/// затем прочие (по имени). `all = false` — только подходящие.
#[must_use]
pub fn handlers(path: &str, all: bool) -> Vec<Handler> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = sysconf::filetypes::extension(name);
    let text = sysconf::filetypes::is_text(name) || ext.is_empty();
    let mut out: Vec<Handler> = Vec::new();
    push(&mut out, Handler {
        caption: String::from("Блокнот"),
        command: String::from("/bin/notepad"),
        icon: String::from("program:/bin/notepad"),
        fits: text,
        chosen: false,
        windows: false,
    });
    for launcher in super::panel::list_packages() {
        if launcher.start.is_empty() || matches!(launcher.name.as_str(), "proton" | "debian" | "wine") {
            continue;
        }
        let fits = !ext.is_empty() && launcher.opens.split(',').any(|known| known.trim().eq_ignore_ascii_case(&ext));
        push(&mut out, Handler {
            caption: launcher.caption.clone(),
            command: launcher.start.clone(),
            icon: alloc::format!("manifest:{}", launcher.icon),
            fits,
            chosen: false,
            windows: false,
        });
    }
    if super::winapps::proton_present() {
        let apps = super::winapps::scan();
        // Подходящие по реестру префиксов.
        if !ext.is_empty() {
    // Programs registered in the settings store (24.11, `filetypes/.ext/
    // openwith/<id>`, see `sysconf::store`): OBSOS for web pages, ports later.
    if !ext.is_empty() {
        for (id, command) in crate::config::registry::registered_programs(&ext) {
            let caption = crate::config::registry::get(&alloc::format!("apps/{id}/caption")).unwrap_or_else(|| caption_of(command.split_whitespace().next().unwrap_or(&command)));
            let icon = match crate::config::registry::get(&alloc::format!("apps/{id}/icon")) {
                Some(icon) => alloc::format!("manifest:{icon}"),
                None => alloc::format!("program:{}", command.split_whitespace().next().unwrap_or("")),
            };
            let windows = command.contains(super::panel::PROTON_SCRIPT);
            push(&mut out, Handler { caption, command, icon, fits: true, chosen: false, windows });
        }
    }
            for prefix in super::winapps::prefixes() {
                for (exe, args) in registered(&prefix, &ext, text) {
                    let app = apps.iter().find(|app| app.target.eq_ignore_ascii_case(&exe));
                    let caption = app.map_or_else(|| caption_of(&exe), |app| app.caption.clone());
                    let icon = app.map(|app| app.icon.clone()).filter(|icon| !icon.is_empty()).unwrap_or_else(|| super::winapps::remember_icon(&exe, 0));
                    // Та же программа из «Пуска» — одной строкой (ключи реестра важнее).
                    out.retain(|known| !(known.windows && known.command == windows_command(&exe)));
                    push(&mut out, Handler { caption, command: windows_command_args(&exe, &args), icon, fits: true, chosen: false, windows: true });
                }
            }
        }
        // Все «Программы Windows» — для «Выбрать другую программу…».
        for app in apps.iter().filter(|app| !app.uninstall && app.target.to_ascii_lowercase().ends_with(".exe")) {
            let prefix = alloc::format!("--with={}", super::panel::pct(&app.target));
            if out.iter().any(|known| known.windows && known.command.split_whitespace().any(|word| word == prefix)) {
                continue;
            }
            push(&mut out, Handler {
                caption: app.caption.clone(),
                command: windows_command(&app.target),
                icon: app.icon.clone(),
                fits: false,
                chosen: false,
                windows: true,
            });
        }
    }
    // Выбор человека — первым, даже если такой программы в списке нет.
    if let Some(command) = super::prefs::open_with(&ext) {
        match out.iter().position(|known| known.command == command) {
            Some(at) => {
                let mut handler = out.remove(at);
                handler.chosen = true;
                handler.fits = true;
                out.insert(0, handler);
            }
            None => {
                let windows = command.contains(super::panel::PROTON_SCRIPT);
                let program = if windows {
                    command.split_whitespace().find(|word| word.starts_with("--with=")).unwrap_or(&command)
                } else {
                    command.split_whitespace().last().unwrap_or(&command)
                };
                let caption = if windows { caption_of(&unpct(program.trim_start_matches("--with="))) } else { String::from(program.rsplit('/').next().unwrap_or(program)) };
                out.insert(0, Handler { caption, command, icon: String::new(), fits: true, chosen: true, windows });
            }
        }
    }
    let (mut head, mut rest): (Vec<Handler>, Vec<Handler>) = out.into_iter().partition(|handler| handler.chosen || handler.fits);
    head.sort_by_key(|handler| !handler.chosen);
    rest.sort_by(|a, b| a.caption.to_lowercase().cmp(&b.caption.to_lowercase()));
    if all {
        head.extend(rest);
    }
    head
}

/// Добавить программу; та же команда второй раз — только «подходит».
fn push(out: &mut Vec<Handler>, handler: Handler) {
    match out.iter_mut().find(|known| known.command == handler.command) {
        Some(known) => known.fits |= handler.fits,
        None => out.push(handler),
    }
}

fn unpct(text: &str) -> String {
    text.replace("%20", " ").replace("%25", "%")
}

/// Подпись программы без ярлыка: имя файла без `.exe` (`notepad++`).
fn caption_of(exe: &str) -> String {
    let file = exe.rsplit(['/', '\\']).next().unwrap_or(exe);
    let stem = file.strip_suffix(".exe").or_else(|| file.strip_suffix(".EXE")).unwrap_or(file);
    let mut chars = stem.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::from(stem),
    }
}

// ── Реестр префикса ────────────────────────────────────────────────────────

/// Нужная часть ветки `Software\Classes` одного префикса: ключ (строчными, от
/// `Classes\`) → значения (имя строчными, `@` — по умолчанию; строка или пусто).
#[derive(Default)]
struct Classes {
    keys: BTreeMap<String, Vec<(String, String)>>,
}

impl Classes {
    fn value(&self, key: &str, name: &str) -> Option<&str> {
        self.keys.get(key)?.iter().find(|(known, _)| known == name).map(|(_, value)| value.as_str())
    }

    fn has(&self, key: &str) -> bool {
        self.keys.contains_key(key)
    }

    /// Имена подключей `key` (первая часть пути после него).
    fn children(&self, key: &str) -> Vec<String> {
        let start = alloc::format!("{key}\\");
        let mut out: Vec<String> = Vec::new();
        for known in self.keys.range(start.clone()..).map(|(known, _)| known).take_while(|known| known.starts_with(&start)) {
            let child = known[start.len()..].split('\\').next().unwrap_or("");
            if !child.is_empty() && out.last().map(String::as_str) != Some(child) {
                out.push(String::from(child));
            }
        }
        out.dedup();
        out
    }

    /// Команда глагола по умолчанию у ProgID или `Applications\x.exe`.
    fn command(&self, base: &str) -> Option<String> {
        let verb = self.value(&alloc::format!("{base}\\shell"), "@").filter(|verb| !verb.is_empty()).map(str::to_lowercase);
        let verb = verb.as_deref().unwrap_or("open");
        self.value(&alloc::format!("{base}\\shell\\{verb}\\command"), "@")
            .or_else(|| self.value(&alloc::format!("{base}\\shell\\open\\command"), "@"))
            .filter(|command| !command.trim().is_empty())
            .map(String::from)
    }
}

/// Разобранные реестры: префикс, подпись размеров файлов, ветка.
static CACHE: crate::sync::SpinLock<Vec<(String, String, Arc<Classes>)>> = crate::sync::SpinLock::new(Vec::new());

/// Самый большой файл реестра, который читается.
const REG_LIMIT: usize = 64 << 20;

fn classes(prefix: &str) -> Arc<Classes> {
    let files = [alloc::format!("{prefix}/system.reg"), alloc::format!("{prefix}/user.reg")];
    let stamp: String = files
        .iter()
        .map(|file| match crate::fs::read(file, 0) {
            Some(Ok((_, size))) => alloc::format!("{size};"),
            _ => String::from("-;"),
        })
        .collect();
    if let Some((_, _, found)) = CACHE.lock().iter().find(|(known, known_stamp, _)| known == prefix && *known_stamp == stamp) {
        return found.clone();
    }
    let mut classes = Classes::default();
    for file in &files {
        if let Some(Ok((bytes, _))) = crate::fs::read(file, REG_LIMIT) {
            parse_reg(&String::from_utf8_lossy(&bytes), &mut classes);
        }
    }
    crate::kprintln!("  openwith    : {prefix}: {} class key(s) from the registry", classes.keys.len());
    let classes = Arc::new(classes);
    let mut cache = CACHE.lock();
    cache.retain(|(known, _, _)| known != prefix);
    cache.push((String::from(prefix), stamp, classes.clone()));
    classes
}

/// Снять экранирование строки реестра Wine (`\\`, `\"`, `\n`, `\xABCD`).
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('0') => {}
            Some('x') => {
                let mut code = 0u32;
                let mut digits = 0;
                while digits < 4 {
                    match chars.peek().and_then(|ch| ch.to_digit(16)) {
                        Some(digit) => {
                            code = code * 16 + digit;
                            chars.next();
                            digits += 1;
                        }
                        None => break,
                    }
                }
                if let Some(ch) = char::from_u32(code) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Строка в кавычках с начала `text`: (содержимое без экранирования, остаток).
fn quoted(text: &str) -> Option<(String, &str)> {
    let body = text.strip_prefix('"')?;
    let mut escaped = false;
    for (at, ch) in body.char_indices() {
        match ch {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return Some((unescape(&body[..at]), &body[at + 1..])),
            _ => escaped = false,
        }
    }
    None
}

/// Нужная ли «Пуску» и «Открыть с помощью» часть ветки (`Classes\…`
/// строчными): расширения, `Applications`, глаголы ProgID.
fn wanted(key: &str) -> bool {
    key.starts_with('.') || key.starts_with("applications\\") || key.contains("\\shell")
}

/// Разобрать файл реестра Wine: ключи `Software\Classes\…` (в `system.reg`
/// — HKLM, в `user.reg` — HKCU; второй дополняет первый).
fn parse_reg(text: &str, classes: &mut Classes) {
    const ROOT: &str = "software\\classes\\";
    let mut current: Option<String> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('[') {
            current = None;
            let Some(end) = rest.rfind(']') else { continue };
            let key = unescape(&rest[..end]).to_lowercase();
            if let Some(relative) = key.strip_prefix(ROOT) {
                if wanted(relative) {
                    classes.keys.entry(String::from(relative)).or_default();
                    current = Some(String::from(relative));
                }
            }
            continue;
        }
        let Some(key) = current.as_ref() else { continue };
        let (name, rest) = if let Some(rest) = line.strip_prefix("@=") {
            (String::from("@"), rest)
        } else if line.starts_with('"') {
            let Some((name, rest)) = quoted(line) else { continue };
            let Some(rest) = rest.strip_prefix('=') else { continue };
            (name.to_lowercase(), rest)
        } else {
            continue;
        };
        let rest = rest.strip_prefix("str(2):").or_else(|| rest.strip_prefix("str(1):")).unwrap_or(rest);
        let value = quoted(rest).map(|(value, _)| value).unwrap_or_default();
        if let Some(values) = classes.keys.get_mut(key) {
            match values.iter_mut().find(|(known, _)| *known == name) {
                Some(slot) => slot.1 = value,
                None => values.push((name, value)),
            }
        }
    }
}

/// Программа (путь Windows) и её ключи из командной строки
/// `"C:\…\x.exe" open "%1"`.
fn exe_of(command: &str) -> Option<(String, String)> {
    let command = command.trim();
    let (exe, args) = if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"')?;
        (String::from(&rest[..end]), String::from(&rest[end + 1..]))
    } else {
        let lower = command.to_ascii_lowercase();
        match lower.find(".exe") {
            Some(at) => (String::from(&command[..at + 4]), String::from(&command[at + 4..])),
            None => {
                let (exe, args) = command.split_once(' ').unwrap_or((command, ""));
                (String::from(exe), String::from(args))
            }
        }
    };
    exe.to_ascii_lowercase().ends_with(".exe").then(|| (exe, String::from(args.trim())))
}

/// Программа самого Wine (Блокнот Wine, `winebrowser`, WordPad, IE): в
/// «Открыть с помощью» не предлагается — у человека свои.
fn wine_own(exe: &str) -> bool {
    let lower = exe.to_ascii_lowercase().replace('/', "\\");
    let file = lower.rsplit('\\').next().unwrap_or(&lower);
    lower.contains(":\\windows\\")
        || lower.contains("%systemroot%")
        || lower.contains("%windir%")
        || lower.contains("\\internet explorer\\")
        || lower.contains("\\windows nt\\")
        || lower.contains("\\windows media player\\")
        || matches!(file, "notepad.exe" | "winebrowser.exe" | "wordpad.exe" | "iexplore.exe" | "start.exe" | "rundll32.exe" | "winhlp32.exe" | "explorer.exe")
}

/// Программы префикса `prefix`, которые открывают расширение `ext`: путь в
/// префиксе и ключи из реестра. `text` — файл текстовый: программы из
/// `Applications` без `SupportedTypes` (редакторы вроде Notepad++) его тоже
/// открывают.
fn registered(prefix: &str, ext: &str, text: bool) -> Vec<(String, String)> {
    let classes = classes(prefix);
    let dot = alloc::format!(".{ext}");
    let mut commands: Vec<String> = Vec::new();
    // ProgID по умолчанию и из OpenWithProgids.
    let mut progids: Vec<String> = Vec::new();
    if let Some(progid) = classes.value(&dot, "@").filter(|id| !id.is_empty()) {
        progids.push(progid.to_lowercase());
    }
    if let Some(values) = classes.keys.get(&alloc::format!("{dot}\\openwithprogids")) {
        progids.extend(values.iter().filter(|(name, _)| name != "@").map(|(name, _)| name.clone()));
    }
    for progid in &progids {
        if let Some(command) = classes.command(progid) {
            commands.push(command);
        }
    }
    // OpenWithList: подключи — имена программ из `Applications`.
    let mut apps: Vec<String> = classes.children(&alloc::format!("{dot}\\openwithlist"));
    // `Applications\x.exe\SupportedTypes` с этим расширением — или без списка
    // вовсе (для текста).
    for app in classes.children("applications") {
        let types = alloc::format!("applications\\{app}\\supportedtypes");
        let fits = if classes.has(&types) { classes.value(&types, &dot).is_some() } else { text };
        if fits && !apps.contains(&app) {
            apps.push(app);
        }
    }
    for app in apps {
        if classes.value(&alloc::format!("applications\\{app}"), "noopenwith").is_some() {
            continue;
        }
        if let Some(command) = classes.command(&alloc::format!("applications\\{app}")) {
            commands.push(command);
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    for command in commands {
        let Some((exe, args)) = exe_of(&command) else { continue };
        if wine_own(&exe) {
            continue;
        }
        if let Some(unix) = super::winapps::to_unix(prefix, &exe) {
            if !out.iter().any(|(known, _)| known.eq_ignore_ascii_case(&unix)) {
                out.push((unix, args));
            }
        }
    }
    out
}

// ── Значки для «Файлов» ────────────────────────────────────────────────────

/// Значок по ключу [`Handler::icon`] — FIMG не больше 64×64.
#[must_use]
pub fn icon_fimg(key: &str) -> Option<Vec<u8>> {
    let art = if key.starts_with("/winicon/") {
        Art::Image(super::winapps::icon_by_name(key).or_else(|| appicon::image(key))?)
    } else if let Some(icon) = key.strip_prefix("manifest:") {
        appicon::of_manifest(icon)
    } else if let Some(path) = key.strip_prefix("program:") {
        appicon::of_program(path, "").0
    } else {
        return None;
    };
    let image = match art {
        Art::Image(image) => image,
        Art::Glyph(icon, ..) => appicon::design_for(icon)?,
        Art::Tile(..) => return None,
        Art::Named(name, ..) => appicon::design(name)?,
    };
    Some(encode(&image, 64))
}

/// FIMG картинки, уменьшенной до `max` по большей стороне (усреднением).
fn encode(image: &Image, max: u32) -> Vec<u8> {
    let scale = image.width.max(image.height).div_ceil(max).max(1);
    let (w, h) = ((image.width / scale).max(1), (image.height / scale).max(1));
    let mut out = Vec::with_capacity(8 + (w * h * 4) as usize);
    out.extend_from_slice(mini_ui::image::MAGIC);
    out.extend_from_slice(&(w as u16).to_le_bytes());
    out.extend_from_slice(&(h as u16).to_le_bytes());
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0u32; 4];
            for dy in 0..scale {
                for dx in 0..scale {
                    let px = image.pixel(x * scale + dx, y * scale + dy);
                    let alpha = u32::from(px[3]);
                    for c in 0..3 {
                        sum[c] += u32::from(px[c]) * alpha;
                    }
                    sum[3] += alpha;
                }
            }
            let n = scale * scale;
            let px = if sum[3] == 0 { [0, 0, 0, 0] } else { [(sum[0] / sum[3]) as u8, (sum[1] / sum[3]) as u8, (sum[2] / sum[3]) as u8, (sum[3] / n) as u8] };
            out.extend_from_slice(&px);
        }
    }
    out
}

/// Список для «Файлов» строками `признак\tподпись\tкоманда\tзначок`:
/// признак `d` — выбрана человеком, `f` — подходит, `o` — прочие.
#[must_use]
pub fn list_text(path: &str) -> String {
    let mut out = String::new();
    for handler in handlers(path, true) {
        let mark = if handler.chosen { 'd' } else if handler.fits { 'f' } else { 'o' };
        let clean = |text: &str| text.replace(['\t', '\n'], " ");
        out.push_str(&alloc::format!("{mark}\t{}\t{}\t{}\n", clean(&handler.caption), clean(&handler.command), clean(&handler.icon)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wine_registry() {
        let text = "WINE REGISTRY Version 2\n;; All keys relative to \\\\Machine\n\n\
            [Software\\\\Classes\\\\.txt] 1700000000\n#time=1\n@=\"txtfile\"\n\"Content Type\"=\"text/plain\"\n\n\
            [Software\\\\Classes\\\\.txt\\\\OpenWithProgids] 1700000000\n\"FossDemo.Document\"=\"\"\n\n\
            [Software\\\\Classes\\\\FossDemo.Document\\\\shell\\\\open\\\\command] 1700000000\n@=\"\\\"C:\\\\Program Files\\\\FossDemo\\\\fossdemo.exe\\\" open \\\"%1\\\"\"\n\n\
            [Software\\\\Classes\\\\txtfile\\\\shell\\\\open\\\\command] 1700000000\n@=str(2):\"%SystemRoot%\\\\system32\\\\NOTEPAD.EXE %1\"\n\n\
            [Software\\\\Classes\\\\Applications\\\\notepad++.exe\\\\shell\\\\open\\\\command] 1700000000\n@=\"\\\"C:\\\\Program Files\\\\Notepad++\\\\notepad++.exe\\\" \\\"%1\\\"\"\n\n\
            [Software\\\\Classes\\\\CLSID\\\\{0}] 1700000000\n@=\"x\"\n";
        let mut classes = Classes::default();
        parse_reg(text, &mut classes);
        assert_eq!(classes.value(".txt", "@"), Some("txtfile"));
        assert!(classes.value(".txt\\openwithprogids", "fossdemo.document").is_some());
        let command = classes.command("fossdemo.document").expect("command");
        let (exe, args) = exe_of(&command).expect("exe");
        assert_eq!(exe, "C:\\Program Files\\FossDemo\\fossdemo.exe");
        assert_eq!(args, "open \"%1\"");
        let wine = classes.command("txtfile").expect("wine notepad");
        assert!(wine_own(&exe_of(&wine).expect("exe").0));
        assert_eq!(classes.children("applications"), alloc::vec![String::from("notepad++.exe")]);
        assert!(!classes.has("clsid\\{0}"));
    }
}
