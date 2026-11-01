//! Подсветка синтаксиса для Блокнота (02.11): строка → куски «что это».
//!
//! Не разбор языка, а то, что делает Notepad++: ключевые слова, строки,
//! числа, комментарии. Строка разбирается сама по себе, и только блочный
//! комментарий (`/* … */`, `<!-- … -->`) переходит на следующую — поэтому
//! состояние между строками одно: внутри ли он.
//!
//! Позиции — номера **символов**, а не байтов: Блокнот считает столбцы в
//! символах, и кусок рисуется по сетке моноширинного шрифта.

use alloc::vec::Vec;

/// Язык файла.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Plain,
    Rust,
    C,
    CSharp,
    JavaScript,
    Python,
    Shell,
    Lua,
    Toml,
    Json,
    Markdown,
    Html,
}

impl Lang {
    /// Все языки — для меню выбора.
    pub const ALL: [Lang; 12] = [
        Lang::Plain,
        Lang::Rust,
        Lang::C,
        Lang::CSharp,
        Lang::JavaScript,
        Lang::Python,
        Lang::Shell,
        Lang::Lua,
        Lang::Toml,
        Lang::Json,
        Lang::Markdown,
        Lang::Html,
    ];

    /// Имя для человека.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Lang::Plain => "Обычный текст",
            Lang::Rust => "Rust",
            Lang::C => "C / C++",
            Lang::CSharp => "C#",
            Lang::JavaScript => "JavaScript",
            Lang::Python => "Python",
            Lang::Shell => "Shell",
            Lang::Lua => "Lua",
            Lang::Toml => "TOML / INI",
            Lang::Json => "JSON",
            Lang::Markdown => "Markdown",
            Lang::Html => "HTML / XML",
        }
    }

    /// Язык по имени файла.
    #[must_use]
    pub fn detect(name: &str) -> Lang {
        let ext = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext,
            _ => "",
        };
        let mut lower = [0u8; 8];
        let len = ext.len().min(lower.len());
        for (slot, byte) in lower.iter_mut().zip(ext.bytes()) {
            *slot = byte.to_ascii_lowercase();
        }
        match &lower[..len] {
            b"rs" => Lang::Rust,
            b"c" | b"h" | b"cpp" | b"hpp" | b"cc" => Lang::C,
            b"cs" => Lang::CSharp,
            b"js" | b"ts" | b"css" => Lang::JavaScript,
            b"py" => Lang::Python,
            b"sh" | b"bash" => Lang::Shell,
            b"lua" => Lang::Lua,
            b"toml" | b"ini" | b"cfg" | b"conf" | b"yaml" | b"yml" => Lang::Toml,
            b"json" => Lang::Json,
            b"md" => Lang::Markdown,
            b"html" | b"htm" | b"xml" | b"svg" => Lang::Html,
            _ => Lang::Plain,
        }
    }

    const fn keywords(self) -> &'static [&'static str] {
        match self {
            Lang::Rust => &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern", "false", "fn", "for", "if",
                "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            Lang::C => &[
                "auto", "break", "case", "char", "class", "const", "continue", "default", "do", "double", "else", "enum", "extern",
                "float", "for", "goto", "if", "inline", "int", "long", "namespace", "nullptr", "private", "public", "return", "short",
                "signed", "sizeof", "static", "struct", "switch", "template", "typedef", "union", "unsigned", "void", "volatile", "while",
                "#include", "#define", "#if", "#ifdef", "#ifndef", "#endif", "#else", "#pragma",
            ],
            Lang::CSharp => &[
                "abstract", "as", "async", "await", "base", "bool", "break", "case", "catch", "class", "const", "continue", "default",
                "do", "else", "enum", "false", "finally", "for", "foreach", "if", "in", "int", "interface", "internal", "is", "namespace",
                "new", "null", "out", "override", "private", "protected", "public", "readonly", "return", "static", "string", "struct",
                "switch", "this", "throw", "true", "try", "using", "var", "virtual", "void", "while",
            ],
            Lang::JavaScript => &[
                "async", "await", "break", "case", "catch", "class", "const", "continue", "default", "delete", "do", "else", "export",
                "extends", "false", "finally", "for", "function", "if", "import", "in", "instanceof", "let", "new", "null", "of", "return",
                "super", "switch", "this", "throw", "true", "try", "typeof", "undefined", "var", "void", "while", "yield",
            ],
            Lang::Python => &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else", "except", "False",
                "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass",
                "raise", "return", "self", "True", "try", "while", "with", "yield",
            ],
            Lang::Shell => &[
                "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local", "return", "then",
                "until", "while", "echo", "cd", "exit",
            ],
            Lang::Lua => &[
                "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in", "local", "nil", "not",
                "or", "repeat", "return", "then", "true", "until", "while",
            ],
            Lang::Json | Lang::Toml => &["true", "false", "null"],
            Lang::Plain | Lang::Markdown | Lang::Html => &[],
        }
    }

    /// Начало строчного комментария.
    const fn line_comment(self) -> Option<&'static str> {
        match self {
            Lang::Rust | Lang::C | Lang::CSharp | Lang::JavaScript => Some("//"),
            Lang::Python | Lang::Shell | Lang::Toml => Some("#"),
            Lang::Lua => Some("--"),
            Lang::Plain | Lang::Json | Lang::Markdown | Lang::Html => None,
        }
    }

    /// Блочный комментарий: начало и конец.
    const fn block_comment(self) -> Option<(&'static str, &'static str)> {
        match self {
            Lang::Rust | Lang::C | Lang::CSharp | Lang::JavaScript => Some(("/*", "*/")),
            Lang::Html | Lang::Markdown => Some(("<!--", "-->")),
            _ => None,
        }
    }
}

/// Что за кусок строки.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Keyword,
    String,
    Number,
    Comment,
    /// Заголовок Markdown, ключ TOML, тег HTML.
    Name,
}

/// Состояние на начало строки: внутри ли блочного комментария.
pub type State = bool;

/// Разобрать строку. Куски — `(первый символ, после последнего, вид)`, без
/// пропусков и по порядку; `state` — состояние на начало строки, после
/// вызова — на начало следующей.
#[must_use]
pub fn spans(lang: Lang, line: &str, state: &mut State) -> Vec<(usize, usize, Kind)> {
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(usize, usize, Kind)> = Vec::new();
    let push = |out: &mut Vec<(usize, usize, Kind)>, from: usize, to: usize, kind: Kind| {
        if from >= to {
            return;
        }
        match out.last_mut() {
            Some(last) if last.2 == kind && last.1 == from => last.1 = to,
            _ => out.push((from, to, kind)),
        }
    };
    if lang == Lang::Plain {
        push(&mut out, 0, chars.len(), Kind::Text);
        return out;
    }
    let starts = |at: usize, pat: &str| -> bool {
        let mut index = at;
        for ch in pat.chars() {
            if chars.get(index) != Some(&ch) {
                return false;
            }
            index += 1;
        }
        true
    };
    let find = |from: usize, pat: &str| -> Option<usize> { (from..chars.len()).find(|at| starts(*at, pat)) };

    // Markdown: заголовок — вся строка.
    if lang == Lang::Markdown && !*state && chars.first() == Some(&'#') {
        push(&mut out, 0, chars.len(), Kind::Name);
        return out;
    }
    // TOML/INI: [раздел] и ключ до `=`.
    let mut at = 0;
    if lang == Lang::Toml {
        let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(chars.len());
        if chars.get(first) == Some(&'[') {
            push(&mut out, 0, chars.len(), Kind::Name);
            return out;
        }
        if chars.get(first) != Some(&'#') {
            if let Some(eq) = chars.iter().position(|c| *c == '=' || *c == ':') {
                push(&mut out, 0, eq, Kind::Name);
                at = eq;
            }
        }
    }

    while at < chars.len() {
        if *state {
            let (_, end) = lang.block_comment().unwrap_or(("", ""));
            match find(at, end) {
                Some(close) => {
                    let to = close + end.chars().count();
                    push(&mut out, at, to, Kind::Comment);
                    at = to;
                    *state = false;
                }
                None => {
                    push(&mut out, at, chars.len(), Kind::Comment);
                    return out;
                }
            }
            continue;
        }
        if let Some((open, _)) = lang.block_comment() {
            if starts(at, open) {
                let to = at + open.chars().count();
                push(&mut out, at, to, Kind::Comment);
                at = to;
                *state = true;
                continue;
            }
        }
        if let Some(mark) = lang.line_comment() {
            if starts(at, mark) {
                push(&mut out, at, chars.len(), Kind::Comment);
                return out;
            }
        }
        let ch = chars[at];
        // Строки в кавычках; обратная косая экранирует.
        if ch == '"' || (ch == '\'' && lang != Lang::Rust && lang != Lang::Markdown) || (ch == '`' && lang == Lang::JavaScript) {
            let mut end = at + 1;
            while end < chars.len() && chars[end] != ch {
                if chars[end] == '\\' {
                    end += 1;
                }
                end += 1;
            }
            let to = (end + 1).min(chars.len());
            push(&mut out, at, to, Kind::String);
            at = to;
            continue;
        }
        // Тег HTML.
        if lang == Lang::Html && ch == '<' {
            let end = (at..chars.len()).find(|index| chars[*index] == '>' || chars[*index].is_whitespace()).unwrap_or(chars.len());
            push(&mut out, at, end, Kind::Name);
            at = end.max(at + 1);
            continue;
        }
        if ch.is_ascii_digit() && (at == 0 || !is_word(chars[at - 1])) {
            let end = (at..chars.len()).find(|index| !(chars[*index].is_ascii_alphanumeric() || chars[*index] == '.' || chars[*index] == '_')).unwrap_or(chars.len());
            push(&mut out, at, end, Kind::Number);
            at = end;
            continue;
        }
        if is_word(ch) || ch == '#' {
            let end = (at + 1..chars.len()).find(|index| !is_word(chars[*index])).unwrap_or(chars.len());
            let word: alloc::string::String = chars[at..end].iter().collect();
            let kind = if lang.keywords().contains(&word.as_str()) { Kind::Keyword } else { Kind::Text };
            push(&mut out, at, end, kind);
            at = end;
            continue;
        }
        push(&mut out, at, at + 1, Kind::Text);
        at += 1;
    }
    out
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_line() {
        let mut state = false;
        let got = spans(Lang::Rust, "let x = 42; // ответ", &mut state);
        assert_eq!(got[0], (0, 3, Kind::Keyword));
        assert!(got.contains(&(8, 10, Kind::Number)));
        assert_eq!(got.last().unwrap().2, Kind::Comment);
        assert!(!state);
    }

    #[test]
    fn block_comment_spans_lines() {
        let mut state = false;
        let first = spans(Lang::C, "int a; /* начало", &mut state);
        assert!(state);
        assert_eq!(first.last().unwrap().2, Kind::Comment);
        let second = spans(Lang::C, "конец */ return 1;", &mut state);
        assert!(!state);
        assert_eq!(second[0].2, Kind::Comment);
        assert!(second.iter().any(|span| span.2 == Kind::Keyword));
    }

    #[test]
    fn strings_toml_markdown() {
        let mut state = false;
        let got = spans(Lang::Python, "print(\"a \\\" b\")", &mut state);
        assert!(got.contains(&(6, 14, Kind::String)));
        let toml = spans(Lang::Toml, "name = \"fossai\"", &mut state);
        assert_eq!(toml[0], (0, 5, Kind::Name));
        assert_eq!(spans(Lang::Markdown, "# Заголовок", &mut state), alloc::vec![(0, 11, Kind::Name)]);
        assert_eq!(Lang::detect("main.RS"), Lang::Rust);
        assert_eq!(Lang::detect("README"), Lang::Plain);
    }
}
