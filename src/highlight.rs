//! Tiny language-agnostic syntax highlighter (Rust/Python/JS/TS/Go/shell-ish),
//! coloured from the pywal theme. Works line by line; good enough for previews.

use crate::theme::{Rgb, Theme};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

const KEYWORDS: &[&str] = &[
    // rust
    "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "match", "use", "mod", "crate", "self", "Self", "super", "where", "as",
    "ref", "move", "dyn", "unsafe", "loop", "in", "type", "static", "const",
    // shared / c-like
    "if", "else", "for", "while", "return", "break", "continue", "true", "false", "null", "new", "this", "class", "extends", "switch",
    "case", "default", "try", "catch", "finally", "throw", "async", "await", "yield", "import", "export", "from", "function", "var",
    "interface", "package", "func", "go", "defer", "chan", "select", "range", "map",
    // python
    "def", "lambda", "None", "True", "False", "and", "or", "not", "is", "with", "pass", "raise", "except", "elif", "global", "nonlocal",
    // shell
    "then", "fi", "do", "done", "esac", "echo", "export", "local", "sudo",
];

fn c(x: Rgb) -> Color {
    Color::Rgb(x.0, x.1, x.2)
}

pub struct Palette {
    pub base: Style,
    kw: Style,
    string: Style,
    num: Style,
    func: Style,
    ty: Style,
    comment: Style,
    punct: Style,
    attr: Style,
}

impl Palette {
    pub fn new(t: &Theme, base: Rgb) -> Palette {
        let s = |x: Rgb| Style::default().fg(c(x));
        Palette {
            base: s(base),
            kw: s(t.a(4)).add_modifier(Modifier::BOLD),
            string: s(t.a(1)),
            num: s(t.a(2)),
            func: s(t.a(3)),
            ty: s(t.a(5)),
            comment: s(t.dim).add_modifier(Modifier::ITALIC),
            punct: s(base.mix(t.dim, 0.5)),
            attr: s(t.a(2)).add_modifier(Modifier::DIM),
        }
    }
}

/// Does this line look like source code rather than prose?
pub fn looks_like_code(s: &str) -> bool {
    let t = s.trim();
    if t.len() < 3 {
        return false;
    }
    let strong = [";", "{", "}", "=>", "->", "::", "();", "#[", "</", "/>", "!=", "==", ":=", "&&", "||"];
    let starts = ["fn ", "let ", "pub ", "def ", "const ", "import ", "from ", "use ", "class ", "return ", "if (", "for (", "#include", "$ ", "func ", "var ", "export "];
    strong.iter().any(|p| t.contains(p)) || starts.iter().any(|p| t.starts_with(p)) || (t.ends_with(':') && (t.starts_with("def ") || t.starts_with("class ")))
}

pub fn spans<'a>(line: &str, p: &Palette) -> Vec<Span<'a>> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out: Vec<Span<'a>> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<Span<'a>>| {
        if !plain.is_empty() {
            out.push(Span::styled(std::mem::take(plain), p.base));
        }
    };
    let collect = |a: usize, b: usize| chars[a..b].iter().collect::<String>();
    let mut i = 0;
    while i < n {
        let ch = chars[i];
        let next = chars.get(i + 1).copied();
        // comments to end of line
        if (ch == '/' && next == Some('/')) || (ch == '#' && matches!(next, Some(' ') | None) && line[..].trim_start().starts_with('#')) || (ch == '-' && next == Some('-') && chars.get(i + 2) == Some(&' ')) {
            flush(&mut plain, &mut out);
            out.push(Span::styled(collect(i, n), p.comment));
            break;
        }
        // rust attributes / decorators
        if (ch == '#' && next == Some('[')) || (ch == '@' && next.is_some_and(|c| c.is_alphabetic()) && out.is_empty() && plain.trim().is_empty()) {
            flush(&mut plain, &mut out);
            let end = chars[i..].iter().position(|c| *c == ']' || *c == '(' || *c == ' ').map_or(n, |e| i + e + usize::from(chars[i + e] == ']'));
            out.push(Span::styled(collect(i, end), p.attr));
            i = end;
            continue;
        }
        // strings
        if ch == '"' || ch == '`' || (ch == '\'' && chars[i + 1..].iter().take(80).any(|c| *c == '\'')) {
            let mut j = i + 1;
            while j < n && chars[j] != ch {
                if chars[j] == '\\' {
                    j += 1;
                }
                j += 1;
            }
            let end = (j + 1).min(n);
            // a lone apostrophe in prose ("it's") is not a string
            if ch == '\'' && i > 0 && chars[i - 1].is_alphanumeric() {
                plain.push(ch);
                i += 1;
                continue;
            }
            flush(&mut plain, &mut out);
            out.push(Span::styled(collect(i, end), p.string));
            i = end;
            continue;
        }
        // numbers
        if ch.is_ascii_digit() && (i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '_')) {
            let mut j = i;
            while j < n && (chars[j].is_ascii_alphanumeric() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            flush(&mut plain, &mut out);
            out.push(Span::styled(collect(i, j), p.num));
            i = j;
            continue;
        }
        // identifiers
        if ch.is_alphabetic() || ch == '_' {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let word = collect(i, j);
            let after = chars[j..].iter().find(|c| **c != ' ').copied();
            let style = if KEYWORDS.contains(&word.as_str()) {
                Some(p.kw)
            } else if after == Some('(') || (after == Some('!') && chars.get(j + 1) != Some(&'=')) {
                Some(p.func)
            } else if word.chars().next().is_some_and(char::is_uppercase) && word.chars().any(char::is_lowercase) {
                Some(p.ty)
            } else {
                None
            };
            match style {
                Some(s) => {
                    flush(&mut plain, &mut out);
                    out.push(Span::styled(word, s));
                }
                None => plain.push_str(&word),
            }
            i = j;
            continue;
        }
        if "{}()[];,.:<>=+-*/&|!?%^~".contains(ch) {
            flush(&mut plain, &mut out);
            let mut j = i;
            while j < n && "{}()[];,.:<>=+-*/&|!?%^~".contains(chars[j]) && !(chars[j] == '/' && chars.get(j + 1) == Some(&'/')) {
                j += 1;
            }
            out.push(Span::styled(collect(i, j), p.punct));
            i = j;
            continue;
        }
        plain.push(ch);
        i += 1;
    }
    flush(&mut plain, &mut out);
    out
}

/// Claude Code diff row: "413 +  code", "413 -  code" or "413    code".
/// Returns (line number, sign, code).
pub fn diff_row(s: &str) -> Option<(String, char, String)> {
    let t = s.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    let rest = &t[digits.len()..];
    if rest.trim().is_empty() {
        return Some((digits, ' ', String::new()));
    }
    let mut it = rest.chars();
    match it.next() {
        Some(' ') => {}
        _ => return None,
    }
    let after: String = it.collect();
    let (sign, code) = match after.chars().next() {
        Some(c @ ('+' | '-')) => (c, after[1..].to_string()),
        _ => (' ', after),
    };
    Some((digits, sign, code.strip_prefix(' ').unwrap_or(&code).to_string()))
}
