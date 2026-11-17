//! Приложения Craft — GridCraft, WordCraft, PdfCraft, MailCraft (storytold /
//! UCS-Care-Group, MIT OR Apache-2.0) — **нативно** под `<арх>-unknown-freeos`,
//! без слоя Linux (ROADMAP: «Приложения Craft»).
//!
//! # Обновления без форка
//!
//! Исходники не копируются в репозиторий и не форкаются: `ports/craft/
//! versions.toml` называет выпуск (тег) каждого приложения, сборка
//! (`ports/craft/builder/build-craft.sh`, Docker) берёт исходник этого тега с
//! GitHub, накладывает наши правки `ports/craft/<app>/*.patch` и подменяет
//! eframe/rfd/arboard/winit своими крейтами (`ports/craft/host`). С `--latest`
//! тег спрашивается у GitHub (последний выпуск); правки не легли или не
//! собралось — команда падает, а пакет прошлой версии остаётся (ночная сборка
//! `.github/workflows/craft.yml` так и публикует только то, что собралось).
//!
//! Готовое — `build/craft/<арх>/<app>-<версия>.fpk` (подписаны ключом выпуска,
//! как OBSOS) и `craft-index.json` + `craft-index.json.sig`: список пакетов с
//! версией, размером, SHA-256 и адресом в выпуске `craft-packages` на GitHub.
//! Систему обновляет `craftupd` (ports/craft/craftupd).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use fpk::build::{Builder, Entry};
use fpk::Kind;

use crate::arch::Arch;
use crate::paths;

/// Куда выкладываются пакеты и индекс (выпуск GitHub с постоянным тегом).
pub const RELEASE_URL: &str = "https://github.com/anomal3/FreeOpenSourceSystemAI/releases/download/craft-packages";

/// Приложение из `ports/craft/versions.toml`.
#[derive(Debug, Clone, Default)]
pub struct CraftApp {
    pub name: String,
    pub repo: String,
    pub tag: String,
    pub bin: String,
    pub caption: String,
    pub icon: String,
    pub permissions: String,
}

fn versions_file() -> PathBuf {
    paths::workspace_root().join("ports/craft/versions.toml")
}

/// Разобрать `versions.toml`: секции `[имя]` и строки `ключ = "значение"`.
/// Своим разбором, без крейта toml: формат файла — ровно такой и не больше.
pub fn apps() -> Result<Vec<CraftApp>> {
    let text = fs::read_to_string(versions_file()).with_context(|| format!("нет {}", versions_file().display()))?;
    parse_versions(&text)
}

fn parse_versions(text: &str) -> Result<Vec<CraftApp>> {
    let mut apps: Vec<CraftApp> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            apps.push(CraftApp { name: name.trim().to_string(), ..Default::default() });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_string();
        let Some(app) = apps.last_mut() else { bail!("versions.toml: «{line}» вне секции") };
        match key.trim() {
            "repo" => app.repo = value,
            "tag" => app.tag = value,
            "bin" => app.bin = value,
            "caption" => app.caption = value,
            "icon" => app.icon = value,
            "permissions" => app.permissions = value,
            _ => {}
        }
    }
    for app in &apps {
        if app.repo.is_empty() || app.tag.is_empty() || app.bin.is_empty() {
            bail!("versions.toml: у [{}] нет repo/tag/bin", app.name);
        }
    }
    Ok(apps)
}

/// Версия пакета по тегу выпуска: `v0.19.1` → `0.19.1`.
pub fn version_of(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Последний выпуск на GitHub (`releases/latest`; без выпусков — самый новый
/// тег вида vX.Y.Z). Через `curl` хоста: у xtask нет клиента HTTPS, а сеть
/// здесь нужна ровно для одного запроса.
pub fn latest_tag(repo: &str) -> Result<String> {
    let get = |url: &str| -> Result<String> {
        let out = Command::new("curl")
            .args(["-sSfL", "-H", "Accept: application/vnd.github+json", "-A", "fossai-xtask", url])
            .output()
            .context("не запустился curl")?;
        if !out.status.success() {
            bail!("{url}: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    if let Ok(body) = get(&format!("https://api.github.com/repos/{repo}/releases/latest"))
        && let Some(tag) = json_string(&body, "tag_name")
    {
        return Ok(tag);
    }
    let body = get(&format!("https://api.github.com/repos/{repo}/tags?per_page=100"))?;
    let mut tags: Vec<String> = Vec::new();
    let mut rest = body.as_str();
    while let Some(at) = rest.find("\"name\"") {
        rest = &rest[at..];
        if let Some(tag) = json_string(rest, "name") {
            tags.push(tag);
        }
        rest = &rest[6..];
    }
    tags.retain(|t| t.starts_with('v') && t[1..].split('.').all(|p| p.parse::<u32>().is_ok()));
    tags.sort_by_key(|t| version_key(version_of(t)));
    tags.pop().with_context(|| format!("у {repo} нет тегов vX.Y.Z"))
}

/// Ключ сравнения версий `X.Y.Z` (числа по частям).
pub fn version_key(version: &str) -> Vec<u64> {
    version.split(['.', '-', '+']).map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

/// Первая строка `"ключ": "значение"` в тексте JSON.
fn json_string(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\""))?;
    let rest = &body[at + key.len() + 2..];
    let rest = &rest[rest.find(':')? + 1..];
    let rest = &rest[rest.find('"')? + 1..];
    Some(rest[..rest.find('"')?].to_string())
}

/// Docker хоста (Docker Desktop на Windows кладёт docker.exe не в PATH).
fn docker() -> PathBuf {
    let desktop = PathBuf::from(r"C:\Program Files\Docker\Docker\resources\bin\docker.exe");
    if desktop.is_file() { desktop } else { PathBuf::from("docker") }
}

/// Собрать приложение в контейнере fossai-builder (`build-craft.sh`).
pub fn build(app: &CraftApp, arch: Arch, tag: &str) -> Result<()> {
    let root = paths::workspace_root();
    let mut cmd = Command::new(docker());
    cmd.arg("run")
        .arg("--rm")
        .arg("-e")
        .arg(format!("CRAFT_TAG={tag}"))
        .arg("-v")
        .arg(format!("{}:/work", root.display()))
        .arg("-v")
        .arg("fossai-craft:/cache")
        .arg("-v")
        .arg("fossai-cargo-registry:/opt/cargo/registry")
        .arg("fossai-builder")
        .arg("sh")
        .arg("/work/ports/craft/builder/build-craft.sh")
        .arg(&app.name)
        .arg(arch.name());
    crate::util::run(&mut cmd, &format!("{} {tag} под {}-unknown-freeos (Docker)", app.name, arch.name()))
}

/// Что собрал `build-craft.sh`: программа, тег и файлы для пакета
/// (`build/linux/craft-<app>/`: значок, лицензии, NOTICE приложения).
fn built(app: &CraftApp, arch: Arch) -> Option<(PathBuf, String, PathBuf)> {
    let program = crate::ruststd::linux_host_program(&app.name, arch)?;
    let tag = fs::read_to_string(program.with_extension("version")).ok()?;
    let share = paths::build_dir().join("linux").join(format!("craft-{}", app.name));
    Some((program, tag.trim().to_string(), share))
}

/// Пакет `<app>-<версия>.fpk`, подписанный ключом выпуска. Нет собранной
/// программы — `None` (система собирается и без приложений Craft).
pub fn package(app: &CraftApp, arch: Arch) -> Result<Option<(String, Vec<u8>)>> {
    let Some((program, tag, share)) = built(app, arch) else {
        say!("пакет {} пропущен: нет build/linux/{}-{} — собрать: cargo xtask craft --app {}", app.name, app.name, arch.name(), app.name);
        return Ok(None);
    };
    let version = version_of(&tag).to_string();
    let mut pkg = Builder::new(Kind::Package, &app.name, &version);
    pkg.field("summary", &format!("{} {version} — https://github.com/{} (MIT OR Apache-2.0), native FossAi build", app.caption, app.repo));
    pkg.field("about", &format!("{} (выпуск {tag}, исходник https://github.com/{}); сборка FossAi без слоя Linux", app.caption, app.repo));
    pkg.field("permissions", &app.permissions);
    pkg.field("caption", &app.caption);
    pkg.field("start", &format!("/opt/{}/bin/{}", app.name, app.bin));
    pkg.field("homepage", &format!("https://github.com/{}", app.repo));
    let read = |path: &Path| fs::read(path).with_context(|| format!("не удалось прочитать {}", path.display()));
    pkg.file(&Entry { path: format!("bin/{}", app.bin), mode: 0o755, uid: 0, gid: 0, data: read(&program)? });
    // Лицензии и уведомления едут с программой (MIT и Apache-2.0 требуют
    // текст лицензии; NOTICE — Apache-2.0 §4d) + наша записка об авторстве.
    for name in ["LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md"] {
        let path = share.join(name);
        if path.is_file() {
            pkg.file(&Entry { path: format!("share/{name}"), mode: 0o644, uid: 0, gid: 0, data: read(&path)? });
        }
    }
    let about = about_text(app, &tag);
    pkg.file(&Entry { path: String::from("share/FOSSAI.md"), mode: 0o644, uid: 0, gid: 0, data: about.into_bytes() });
    let icon = share.join("icon.png");
    if icon.is_file() {
        let fimg = crate::fimg::from_png(&read(&icon)?).with_context(|| format!("значок {}: PNG не разобрался", app.name))?;
        pkg.file(&Entry { path: String::from("icon.fimg"), mode: 0o644, uid: 0, gid: 0, data: fimg });
        pkg.field("icon", &format!("/opt/{}/icon.fimg", app.name));
    }
    let mut bytes = pkg.finish();
    crate::keys::sign(&mut bytes, &crate::keys::release()?);
    Ok(Some((format!("{}-{version}.fpk", app.name), bytes)))
}

/// Записка об авторстве (просьба авторов: имена остаются, ссылки на исходники).
fn about_text(app: &CraftApp, tag: &str) -> String {
    format!(
        "# {caption}\n\n\
         {name} — программа проекта Craft, © её авторы: https://github.com/{repo}\n\
         (выпуск {tag}). Лицензия — MIT OR Apache-2.0 (тексты рядом: LICENSE-MIT,\n\
         LICENSE-APACHE; уведомления — NOTICE, ATTRIBUTION.md).\n\n\
         Это сборка для FossAi (FreeOS) без изменения названия, с согласия авторов\n\
         (soft fork): исходник берётся с GitHub по тегу выпуска как есть, правки для\n\
         FreeOS — в https://github.com/anomal3/FreeOpenSourceSystemAI/tree/master/ports/craft\n\
         ({name}/*.patch и подменённые крейты eframe/rfd/arboard в host/).\n\
         Ошибки этой сборки — в репозиторий FossAi, а не авторам приложения.\n",
        caption = app.caption,
        name = app.name,
        repo = app.repo,
    )
}

/// Пакеты всех собранных приложений (для образа и носителя установки).
pub fn packages(arch: Arch) -> Result<Vec<(String, Vec<u8>)>> {
    let mut out = Vec::new();
    for app in apps()? {
        if let Some(p) = package(&app, arch)? {
            out.push(p);
        }
    }
    Ok(out)
}

/// Файл пакета Craft (`gridcraft-0.19.1.fpk` и т. п.)?
pub fn is_package_file(file_name: &str) -> bool {
    ["gridcraft-", "wordcraft-", "pdfcraft-", "mailcraft-"].iter().any(|p| file_name.starts_with(p)) && file_name.ends_with(".fpk")
}

/// Имя пакета на ESP живой флешки (8.3: ≤ 8 знаков имени).
pub fn esp_name(app: &str) -> String {
    let stem: String = app.chars().filter(char::is_ascii_alphanumeric).take(8).collect();
    format!("{}.FPK", stem.to_ascii_uppercase())
}

/// Опции `cargo xtask craft`.
pub struct Options {
    pub arch: Arch,
    pub app: Option<String>,
    pub tag: Option<String>,
    pub latest: bool,
    pub no_build: bool,
    pub out: Option<PathBuf>,
}

/// `cargo xtask craft`: собрать (по тегу из versions.toml, `--tag` или
/// `--latest`), упаковать, написать индекс.
pub fn run(opts: &Options) -> Result<()> {
    let mut chosen = apps()?;
    if let Some(name) = &opts.app {
        chosen.retain(|a| &a.name == name);
        if chosen.is_empty() {
            bail!("нет приложения {name} в {}", versions_file().display());
        }
    }
    let out = opts.out.clone().unwrap_or_else(|| paths::build_dir().join("craft").join(opts.arch.name()));
    fs::create_dir_all(&out).with_context(|| format!("не создать {}", out.display()))?;
    let mut failed = Vec::new();
    for app in &chosen {
        let tag = match (&opts.tag, opts.latest) {
            (Some(tag), _) => tag.clone(),
            (None, true) => latest_tag(&app.repo).unwrap_or_else(|e| {
                say!("{}: последний выпуск не узнать ({e:#}), беру {}", app.name, app.tag);
                app.tag.clone()
            }),
            (None, false) => app.tag.clone(),
        };
        if !opts.no_build {
            if let Err(e) = build(app, opts.arch, &tag) {
                // Новый выпуск не лёг на наши правки — остаётся закреплённый.
                say!("{} {tag} не собрался: {e:#}", app.name);
                if tag != app.tag {
                    say!("{}: пробую закреплённый {}", app.name, app.tag);
                    if let Err(e) = build(app, opts.arch, &app.tag) {
                        say!("{} {} тоже не собрался: {e:#}", app.name, app.tag);
                        failed.push(app.name.clone());
                    }
                } else {
                    failed.push(app.name.clone());
                }
            }
        }
        if let Some((file, bytes)) = package(app, opts.arch)? {
            let path = out.join(&file);
            fs::write(&path, &bytes).with_context(|| format!("не записать {}", path.display()))?;
            say!("пакет: {} ({} байт)", path.display(), bytes.len());
        }
    }
    write_index(&out, opts.arch)?;
    if !failed.is_empty() {
        bail!("не собрались: {}", failed.join(", "));
    }
    Ok(())
}

/// `craft-index.json` по пакетам в каталоге + подпись `craft-index.json.sig`
/// (Ed25519 ключа выпуска по `osupdate::index::digest`, 128 hex-знаков) —
/// её проверяет `craftupd` ключами `/os-keys`.
pub fn write_index(dir: &Path, arch: Arch) -> Result<()> {
    let mut entries: Vec<(String, String, String, u64, String)> = Vec::new();
    for entry in fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("fpk") {
            continue;
        }
        let bytes = fs::read(&path)?;
        let header = fpk::Header::parse(&bytes).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
        let manifest = &bytes[fpk::HEADER_SIZE..fpk::HEADER_SIZE + header.manifest_len as usize];
        let manifest = fpk::Manifest::parse(&header, manifest).map_err(|e| anyhow::anyhow!("{}: {e:?}", path.display()))?;
        let name = manifest.name().map_err(|e| anyhow::anyhow!("{e:?}"))?.to_string();
        let version = manifest.version().map_err(|e| anyhow::anyhow!("{e:?}"))?.to_string();
        let mut hasher = fpk::Hasher::new();
        hasher.update(&bytes);
        let sha = hex(&hasher.finish());
        let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
        entries.push((name, version, file, bytes.len() as u64, sha));
    }
    entries.sort();
    let mut json = String::from("{\n  \"format\": 1,\n");
    json += &format!("  \"arch\": \"{}\",\n  \"packages\": [\n", arch.name());
    for (i, (name, version, file, size, sha)) in entries.iter().enumerate() {
        json += &format!(
            "    {{ \"name\": \"{name}\", \"version\": \"{version}\", \"file\": \"{file}\", \"size\": {size}, \"sha256\": \"{sha}\", \"url\": \"{RELEASE_URL}/{arch}-{file}\" }}{}\n",
            if i + 1 < entries.len() { "," } else { "" },
            arch = arch.name(),
        );
    }
    json += "  ]\n}\n";
    let index = dir.join("craft-index.json");
    fs::write(&index, &json)?;
    let signature = crate::keys::sign_index(json.as_bytes())?;
    fs::write(dir.join("craft-index.json.sig"), hex(&signature) + "\n")?;
    say!("индекс: {} ({} пакетов, подписан)", index.display(), entries.len());
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_file_parses() {
        let apps = super::apps().expect("versions.toml разбирается");
        let names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["gridcraft", "wordcraft", "pdfcraft", "mailcraft"]);
        assert!(apps.iter().all(|a| a.tag.starts_with('v') && !a.caption.is_empty()));
    }

    #[test]
    fn esp_names_are_8_3() {
        assert_eq!(super::esp_name("gridcraft"), "GRIDCRAF.FPK");
        assert_eq!(super::esp_name("pdfcraft"), "PDFCRAFT.FPK");
        assert_eq!(super::esp_name("mailcraft"), "MAILCRAF.FPK");
        assert_eq!(super::esp_name("wordcraft"), "WORDCRAF.FPK");
    }

    #[test]
    fn versions_compare_by_number() {
        assert!(super::version_key("0.19.10") > super::version_key("0.19.9"));
        assert_eq!(super::version_of("v0.19.1"), "0.19.1");
        assert_eq!(super::json_string(r#"{"url":"x","tag_name": "v1.2.3"}"#, "tag_name").as_deref(), Some("v1.2.3"));
    }
}
