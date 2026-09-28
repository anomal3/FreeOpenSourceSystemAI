//! `<арх>-freeos-pkg-config` — pkg-config набора (фаза 61).
//!
//! Чужой `configure` ищет библиотеки через pkg-config: cairo так находит pixman,
//! libpng и freetype, libgdiplus — cairo и fontconfig. На машине сборки
//! pkg-config может не быть вовсе (у Git для Windows его нет), а чужой,
//! хостовый, отвечал бы про библиотеки хоста. Поэтому свой — как у всякого
//! кросс-набора: autoconf (`PKG_PROG_PKG_CONFIG`) первым ищет именно
//! `$host-pkg-config`.
//!
//! Ищет он в sysroot цели — `lib/pkgconfig` и `share/pkgconfig` — и в
//! каталогах `PKG_CONFIG_PATH` перед ними; хостовых каталогов не видит вовсе.
//! Файлы `.pc` пишет `make install` чужих проектов, собранных с
//! `--prefix=<sysroot>`, поэтому пути в них уже указывают в набор.
//!
//! Умеет то, что спрашивают `configure` и `Makefile`: `--exists` с условиями
//! версии, `--modversion`, `--cflags`, `--libs`, `--static`, `--variable`,
//! `--atleast-version` и соседей, `--atleast-pkgconfig-version`, `--version`.
//! `Requires` раскрываются рекурсивно; с `--static` — и `Requires.private` с
//! `Libs.private`, потому что разделяемых библиотек в системе нет и каждая
//! компоновка статическая.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;

use crate::Target;

/// Версия, которую мы объявляем: `configure` проверяет «не старее 0.9.0».
const OUR_VERSION: &str = "0.29.2";

/// Один разобранный `.pc`.
struct Package {
    version: String,
    requires: Vec<Wanted>,
    requires_private: Vec<Wanted>,
    cflags: Vec<String>,
    libs: Vec<String>,
    libs_private: Vec<String>,
    variables: BTreeMap<String, String>,
}

/// Модуль с необязательным условием версии: `cairo >= 1.6`.
#[derive(Clone)]
struct Wanted {
    name: String,
    condition: Option<(String, String)>,
}

/// Точка входа: `args` — всё, что после имени программы.
pub fn run(target: Target, args: &[String]) -> i32 {
    let mut modules: Vec<String> = Vec::new();
    let mut want_cflags = false;
    let mut want_libs = false;
    let mut want_static = false;
    let mut want_modversion = false;
    let mut variable: Option<String> = None;
    let mut exists_only = true;
    let mut version_check: Option<(String, String)> = None;
    let mut print_errors = false;
    // `--cflags-only-I`, `--libs-only-l` и соседи: какую часть флагов отдать.
    let mut only: Option<&'static str> = None;

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| iter.next().cloned()).unwrap_or_default();
        match flag {
            "--version" => {
                println!("{OUR_VERSION}");
                return 0;
            }
            "--atleast-pkgconfig-version" => {
                let wanted = value();
                return if compare(OUR_VERSION, &wanted) >= 0 { 0 } else { 1 };
            }
            "--exists" => exists_only = true,
            "--cflags" | "--cflags-only-I" | "--cflags-only-other" => {
                want_cflags = true;
                exists_only = false;
                only = match flag {
                    "--cflags-only-I" => Some("-I"),
                    "--cflags-only-other" => Some("other"),
                    _ => only,
                };
            }
            "--libs" | "--libs-only-l" | "--libs-only-L" | "--libs-only-other" => {
                want_libs = true;
                exists_only = false;
                only = match flag {
                    "--libs-only-l" => Some("-l"),
                    "--libs-only-L" => Some("-L"),
                    "--libs-only-other" => Some("other"),
                    _ => only,
                };
            }
            "--static" => want_static = true,
            "--modversion" => {
                want_modversion = true;
                exists_only = false;
            }
            "--variable" => {
                variable = Some(value());
                exists_only = false;
            }
            "--atleast-version" => version_check = Some((">=".into(), value())),
            "--exact-version" => version_check = Some(("=".into(), value())),
            "--max-version" => version_check = Some(("<=".into(), value())),
            "--print-errors" | "--short-errors" => print_errors = true,
            // Молча принимаем то, что ответа не меняет.
            "--silence-errors" | "--errors-to-stdout" | "--uninstalled" | "--no-uninstalled" => {}
            _ if flag.starts_with("--") => {}
            _ => modules.push(arg.clone()),
        }
    }

    let wanted = parse_wanted(&modules.join(" "));
    if wanted.is_empty() {
        if print_errors {
            eprintln!("pkg-config: не названо ни одного модуля");
        }
        return 1;
    }
    let dirs = search_dirs(target);

    // Все ли на месте, и сходятся ли версии.
    let mut first: Vec<(String, Package)> = Vec::new();
    for item in &wanted {
        let Some(package) = load(&dirs, &item.name) else {
            if print_errors {
                eprintln!("Package {} was not found in the pkg-config search path.", item.name);
            }
            return 1;
        };
        let condition = version_check.clone().or_else(|| item.condition.clone());
        if let Some((op, version)) = condition {
            if !satisfies(&package.version, &op, &version) {
                if print_errors {
                    eprintln!(
                        "Requested '{} {op} {version}' but version of {} is {}",
                        item.name, item.name, package.version
                    );
                }
                return 1;
            }
        }
        first.push((item.name.clone(), package));
    }

    if exists_only && !want_modversion {
        return 0;
    }
    if want_modversion {
        for (_, package) in &first {
            println!("{}", package.version);
        }
        return 0;
    }
    if let Some(name) = variable {
        let values: Vec<String> =
            first.iter().map(|(_, package)| package.variables.get(&name).cloned().unwrap_or_default()).collect();
        println!("{}", values.join(" "));
        return 0;
    }

    // Раскрыть зависимости: в порядке «сначала модуль, потом то, что ему нужно»
    // — так же, как настоящий pkg-config, чтобы -l шли в верном для
    // компоновщика порядке.
    let mut order: Vec<Package> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<Wanted> = wanted.clone();
    stack.reverse();
    while let Some(item) = stack.pop() {
        if !seen.insert(item.name.clone()) {
            continue;
        }
        let Some(package) = load(&dirs, &item.name) else {
            if print_errors {
                eprintln!("Package {} was not found in the pkg-config search path.", item.name);
            }
            return 1;
        };
        let mut next: Vec<Wanted> = package.requires.clone();
        if want_static {
            next.extend(package.requires_private.clone());
        }
        next.reverse();
        stack.extend(next);
        order.push(package);
    }

    let mut out: Vec<String> = Vec::new();
    if want_cflags {
        for package in &order {
            for flag in &package.cflags {
                if !out.contains(flag) {
                    out.push(flag.clone());
                }
            }
        }
    }
    if want_libs {
        let mut libs: Vec<String> = Vec::new();
        for package in &order {
            libs.extend(package.libs.iter().cloned());
            if want_static {
                libs.extend(package.libs_private.iter().cloned());
            }
        }
        // Повтор `-l` оставляем последним — компоновщику важен последний, — а
        // всё остальное первым.
        for (index, flag) in libs.iter().enumerate() {
            let keep = if flag.starts_with("-l") {
                !libs[index + 1..].contains(flag)
            } else {
                !libs[..index].contains(flag)
            };
            if keep {
                out.push(flag.clone());
            }
        }
    }
    if let Some(part) = only {
        out.retain(|flag| match part {
            "other" => !flag.starts_with("-I") && !flag.starts_with("-l") && !flag.starts_with("-L"),
            prefix => flag.starts_with(prefix),
        });
    }
    println!("{}", out.join(" "));
    0
}

/// Где искать `.pc`: каталоги `PKG_CONFIG_PATH`, затем sysroot цели.
///
/// `PKG_CONFIG_PATH` — как у настоящего pkg-config: впереди умолчаний. Нужен он
/// библиотеке, которую собирают поверх чужой сборки, а не поверх набора
/// (фаза 61b): libgdiplus видит glib в лице eglib из сборки Mono своей
/// архитектуры, и класть этот `glib-2.0.pc` в sysroot значило бы сказать
/// всякому следующему порту, что настоящая glib в наборе есть. Разделитель —
/// `;`, как у pkg-config на Windows: в пути с буквой диска двоеточие своё.
fn search_dirs(target: Target) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var("PKG_CONFIG_PATH")
        .map(|list| list.split(';').filter(|dir| !dir.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default();
    if let Ok(root) = crate::sysroot(target) {
        dirs.push(root.join("lib/pkgconfig"));
        dirs.push(root.join("share/pkgconfig"));
    }
    dirs
}

fn load(dirs: &[PathBuf], name: &str) -> Option<Package> {
    let text = dirs.iter().find_map(|dir| fs::read_to_string(dir.join(format!("{name}.pc"))).ok())?;
    Some(parse(&text))
}

/// Разобрать `.pc`: сначала переменные (`имя=значение`), потом поля (`Имя: …`),
/// с подстановкой `${переменная}`.
fn parse(text: &str) -> Package {
    let mut variables: BTreeMap<String, String> = BTreeMap::new();
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let colon = line.find(':');
        let equals = line.find('=');
        match (colon, equals) {
            (Some(c), Some(e)) if e < c => {
                let value = expand(line[e + 1..].trim(), &variables);
                variables.insert(line[..e].trim().to_string(), value);
            }
            (None, Some(e)) => {
                let value = expand(line[e + 1..].trim(), &variables);
                variables.insert(line[..e].trim().to_string(), value);
            }
            (Some(c), _) => {
                let value = expand(line[c + 1..].trim(), &variables);
                fields.insert(line[..c].trim().to_string(), value);
            }
            _ => {}
        }
    }
    let words = |key: &str| -> Vec<String> {
        fields.get(key).map(|value| value.split_whitespace().map(String::from).collect()).unwrap_or_default()
    };
    Package {
        version: fields.get("Version").cloned().unwrap_or_default(),
        requires: parse_wanted(fields.get("Requires").map_or("", String::as_str)),
        requires_private: parse_wanted(fields.get("Requires.private").map_or("", String::as_str)),
        cflags: words("Cflags"),
        libs: words("Libs"),
        libs_private: words("Libs.private"),
        variables,
    }
}

fn expand(value: &str, variables: &BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                out.push_str(variables.get(&after[..end]).map_or("", String::as_str));
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// `"cairo >= 1.6, pixman-1"` — в список модулей с условиями.
fn parse_wanted(text: &str) -> Vec<Wanted> {
    let tokens: Vec<&str> = text.split(|c: char| c == ',' || c.is_whitespace()).filter(|t| !t.is_empty()).collect();
    let mut out = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let name = tokens[index].to_string();
        index += 1;
        let mut condition = None;
        if index + 1 < tokens.len() && matches!(tokens[index], ">=" | "<=" | "=" | ">" | "<" | "!=") {
            condition = Some((tokens[index].to_string(), tokens[index + 1].to_string()));
            index += 2;
        }
        out.push(Wanted { name, condition });
    }
    out
}

fn satisfies(have: &str, op: &str, want: &str) -> bool {
    let order = compare(have, want);
    match op {
        ">=" => order >= 0,
        "<=" => order <= 0,
        ">" => order > 0,
        "<" => order < 0,
        "=" => order == 0,
        "!=" => order != 0,
        _ => true,
    }
}

/// Сравнить версии по числам через точку: `1.16.0` больше `1.6`.
fn compare(a: &str, b: &str) -> i32 {
    let parts = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).map(|p| p.parse().unwrap_or(0)).collect()
    };
    let (a, b) = (parts(a), parts(b));
    for index in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(index).copied().unwrap_or(0), b.get(index).copied().unwrap_or(0));
        if x != y {
            return if x > y { 1 } else { -1 };
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pc_file_expands_its_variables() {
        let package = parse(
            "prefix=/sys\nlibdir=${prefix}/lib\nincludedir=${prefix}/include\n\nName: cairo\nVersion: 1.16.0\n\
             Requires.private: pixman-1 >= 0.30.0, libpng\nLibs: -L${libdir} -lcairo\nLibs.private: -lz\n\
             Cflags: -I${includedir}/cairo\n",
        );
        assert_eq!(package.version, "1.16.0");
        assert_eq!(package.libs, ["-L/sys/lib", "-lcairo"]);
        assert_eq!(package.cflags, ["-I/sys/include/cairo"]);
        assert_eq!(package.requires_private.len(), 2);
        assert_eq!(package.requires_private[0].name, "pixman-1");
        assert_eq!(package.requires_private[0].condition, Some((">=".into(), "0.30.0".into())));
    }

    #[test]
    fn versions_compare_by_numbers() {
        assert!(satisfies("1.16.0", ">=", "1.6"));
        assert!(!satisfies("0.29.2", ">=", "0.30.0"));
        assert!(satisfies("2.13.2", "=", "2.13.2"));
    }
}
