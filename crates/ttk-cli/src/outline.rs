//! Reading a file without reading all of it.
//!
//! An agent opens a 900 line file to change three lines of it, and pays for all
//! 900 on every request for the rest of the session. That is the largest single
//! item in a coding agent's budget and no compiler touches it, because a
//! compiler only ever sees command output.
//!
//! An outline is the table of contents: every line that declares something,
//! with its line number, and a capsule holding the file. The agent reads the
//! outline, then asks for the twenty lines it actually needs.
//!
//! # What this is not
//!
//! It is **not** a parser. There is no syntax tree here, and there is not going
//! to be one until `ttk` grows a real symbol index (see `docs/roadmap.md`). It
//! is a line classifier with a list of keywords per language family, and it
//! will miss things — a declaration split across lines, an unusual macro, a
//! language nobody listed.
//!
//! That is survivable for exactly one reason: **an outline never replaces the
//! file.** It is a mode you ask for by name, it says `[outline]` on its first
//! line, it reports how many lines it did not show, and the whole file is one
//! `ttk retrieve` away. A heuristic that hides something is a nuisance; the
//! same heuristic deciding on its own to throw the file away would not be, and
//! that is why this is not wired into automatic compilation.

/// Longest a single line may be before the outline clips it.
const MAX_SIGNATURE_CHARS: usize = 120;

/// One declaration found in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 1-based, so it can be pasted into `ttk retrieve --lines`.
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default)]
pub struct Outline {
    pub entries: Vec<Entry>,
    pub total_lines: usize,
}

impl Outline {
    pub fn lines_not_shown(&self) -> usize {
        self.total_lines.saturating_sub(self.entries.len())
    }

    pub fn is_useful(&self) -> bool {
        // An outline that shows most of the file is not an outline, and an
        // empty one is not either. Both mean "just read the file".
        !self.entries.is_empty() && self.entries.len() * 3 < self.total_lines
    }

    /// Render as a Token IR document.
    pub fn render(&self, path: &str, capsule: Option<&str>) -> String {
        let mut out = format!(
            "[outline] file={path} lines={} shown={}\n",
            self.total_lines,
            self.entries.len()
        );
        for entry in &self.entries {
            out.push_str(&format!("{}: {}\n", entry.line, entry.text));
        }
        out.push_str(&format!("lines_not_shown={}\n", self.lines_not_shown()));
        if let Some(c) = capsule {
            out.push_str(&format!("raw={c}\n"));
        }
        out
    }
}

/// Keywords that start a declaration, by language family.
///
/// One flat list on purpose. Guessing the language from an extension and then
/// applying the wrong list is worse than applying all of them: a false positive
/// costs one line in the outline, a false negative hides a function.
const DECLARES: &[&str] = &[
    // Rust
    "fn ",
    "struct ",
    "enum ",
    "trait ",
    "impl ",
    "mod ",
    "macro_rules!",
    "type ",
    "const ",
    "static ",
    "union ",
    // Python
    "def ",
    "class ",
    "async def ",
    // JavaScript / TypeScript
    "function ",
    "interface ",
    "namespace ",
    "declare ",
    "export ",
    "abstract class ",
    // Go
    "func ",
    "package ",
    "var ",
    // JVM / C#
    "public ",
    "private ",
    "protected ",
    "internal ",
    "record ",
    "sealed ",
    // C / C++
    "template",
    "namespace ",
    "typedef ",
    "struct",
    "#define ",
    // Shell / Make
    "function",
];

/// Visibility and modifier words that may sit in front of a declaration.
const PREFIXES: &[&str] = &[
    "pub",
    "pub(crate)",
    "pub(super)",
    "export",
    "default",
    "async",
    "unsafe",
    "extern",
    "const",
    "static",
    "final",
    "abstract",
    "override",
    "virtual",
    "inline",
    "@staticmethod",
    "@classmethod",
    "@property",
];

/// Extract the declarations from `text`.
pub fn extract(text: &str) -> Outline {
    let mut out = Outline {
        total_lines: text.lines().count(),
        ..Outline::default()
    };
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || is_comment(trimmed) {
            continue;
        }
        if !declares(trimmed) {
            continue;
        }
        out.entries.push(Entry {
            line: i + 1,
            text: signature(line),
        });
    }
    out
}

fn is_comment(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('#')
        || trimmed.starts_with("/*")
        || trimmed.starts_with('*')
        || trimmed.starts_with("--")
}

/// Does this line declare something?
fn declares(trimmed: &str) -> bool {
    // A declaration keyword, possibly behind visibility modifiers.
    let mut rest = trimmed;
    for _ in 0..4 {
        if DECLARES.iter().any(|k| rest.starts_with(k)) {
            return true;
        }
        let Some((head, tail)) = rest.split_once(char::is_whitespace) else {
            break;
        };
        if !PREFIXES.contains(&head.trim_end_matches(':')) {
            break;
        }
        rest = tail.trim_start();
    }
    // `name() {` — a shell function, and a C function definition at column 0.
    if let Some(open) = trimmed.find('(')
        && trimmed.ends_with('{')
        && open > 0
        && trimmed[..open]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '*' || c == ' ')
    {
        return true;
    }
    false
}

/// The declaration itself: everything up to the body, clipped.
fn signature(line: &str) -> String {
    let text = line.trim_end();
    // A body on the same line is not part of the signature.
    let text = match text.find(" {") {
        Some(i) => &text[..i],
        None => text,
    };
    let text = text.trim_end_matches([':', ';', '{']).trim_end();
    if text.chars().count() <= MAX_SIGNATURE_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_SIGNATURE_CHARS).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: &str = r#"//! A module comment.

use std::fmt;

/// Documented.
pub struct Config {
    pub mode: Mode,
    pub level: u32,
}

impl Config {
    pub fn load(cwd: &Path) -> Result<Self> {
        let mut value = 1;
        value += 1;
        Ok(Self::default())
    }

    fn helper(&self) -> bool {
        true
    }
}

pub enum Mode {
    Safe,
    Fast,
}
"#;

    #[test]
    fn rust_declarations_are_found_with_their_line_numbers() {
        let o = extract(RUST);
        let found: Vec<(usize, &str)> = o.entries.iter().map(|e| (e.line, e.text.trim())).collect();
        assert!(found.contains(&(6, "pub struct Config")), "{found:?}");
        assert!(found.contains(&(11, "impl Config")), "{found:?}");
        assert!(
            found.contains(&(12, "pub fn load(cwd: &Path) -> Result<Self>")),
            "{found:?}"
        );
        assert!(
            found.contains(&(18, "fn helper(&self) -> bool")),
            "{found:?}"
        );
        assert!(found.contains(&(23, "pub enum Mode")), "{found:?}");
    }

    #[test]
    fn a_body_is_never_part_of_a_signature() {
        let o = extract("pub fn tiny() -> u32 { 42 }\n");
        assert_eq!(o.entries[0].text, "pub fn tiny() -> u32");
    }

    #[test]
    fn python_and_javascript_are_recognised_too() {
        let text = "class Thing:\n    def method(self):\n        pass\n\n\
                    export function build(a, b) {\n  return a;\n}\n\
                    async def fetch(url):\n    pass\n";
        let found: Vec<String> = extract(text)
            .entries
            .iter()
            .map(|e| e.text.trim().to_string())
            .collect();
        assert!(found.iter().any(|t| t == "class Thing"), "{found:?}");
        assert!(found.iter().any(|t| t == "def method(self)"), "{found:?}");
        assert!(
            found.iter().any(|t| t == "export function build(a, b)"),
            "{found:?}"
        );
        assert!(
            found.iter().any(|t| t == "async def fetch(url)"),
            "{found:?}"
        );
    }

    #[test]
    fn a_shell_function_is_a_declaration() {
        let o = extract("greet() {\n  echo hi\n}\n");
        assert_eq!(o.entries.len(), 1);
        assert_eq!(o.entries[0].text, "greet()");
    }

    #[test]
    fn comments_are_never_declarations() {
        let o = extract("// pub fn not_real()\n# def also_not(self)\n/* class Nope */\n");
        assert!(o.entries.is_empty(), "{:?}", o.entries);
    }

    #[test]
    fn the_render_says_what_it_did_not_show() {
        let o = extract(RUST);
        let text = o.render("src/config.rs", Some("cap://a7f3c"));
        assert!(text.starts_with("[outline] file=src/config.rs"), "{text}");
        assert!(text.contains(&format!("lines_not_shown={}", o.lines_not_shown())));
        assert!(text.contains("raw=cap://a7f3c"));
        // Indentation survives, because nesting is information: it is what
        // tells a reader that `load` is a method on `Config`.
        assert!(text.contains("12:     pub fn load"), "{text}");
    }

    /// An outline of a file that is nearly all declarations saves nothing and
    /// only risks hiding something, so it declines.
    #[test]
    fn an_outline_that_would_show_everything_is_not_worth_it() {
        let dense = "pub fn a()\npub fn b()\npub fn c()\n";
        assert!(!extract(dense).is_useful());
        assert!(!extract("no declarations here at all\n").is_useful());
        assert!(extract(RUST).is_useful());
    }

    #[test]
    fn a_very_long_signature_is_clipped_but_marked() {
        let long = format!("pub fn wide({}) -> u32 {{}}\n", "a: u32, ".repeat(40));
        let o = extract(&long);
        assert!(o.entries[0].text.chars().count() <= MAX_SIGNATURE_CHARS + 1);
        assert!(o.entries[0].text.ends_with('…'));
    }

    #[test]
    fn an_empty_file_outlines_to_nothing_without_panicking() {
        let o = extract("");
        assert_eq!(o.total_lines, 0);
        assert_eq!(o.lines_not_shown(), 0);
        assert!(!o.is_useful());
    }
}
