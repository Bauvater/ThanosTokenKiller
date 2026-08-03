//! Token IR — the compact, human *and* machine readable intermediate format
//! every specialized compiler emits.
//!
//! Grammar (v1), line oriented, UTF-8, `\n` separated:
//!
//! ```text
//! doc      := header field* section* raw?
//! header   := "[" kind (":" variant)? "]" (" " pair)*
//! field    := pair (" " pair)*                  ; before the first section
//! section  := "@" label (" " pair)* body*
//! body     := "  " <verbatim line> | ""         ; two-space indent
//! raw      := "raw=cap://" <capsule id>
//! pair     := key "=" (bare | quoted)
//! bare     := [^ \t"]+
//! quoted   := '"' ( [^"\\] | "\\" ["\\n] )* '"'
//! ```
//!
//! Two properties are guaranteed and tested:
//! * `parse(render(doc)) == doc` for every document (round trip),
//! * section bodies are copied verbatim, so invariants inside them survive.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const IR_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrDoc {
    /// Primary category, e.g. `test`, `git`, `shell`, `log`, `json`.
    pub kind: String,
    /// Sub category, e.g. `pytest`, `diff`.
    pub variant: Option<String>,
    /// Key/value pairs rendered on the header line.
    pub header: Vec<(String, String)>,
    /// Key/value pairs rendered on the lines below the header.
    pub fields: Vec<(String, String)>,
    pub sections: Vec<IrSection>,
    /// Capsule holding the untouched original.
    pub raw: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IrSection {
    pub label: String,
    pub attrs: Vec<(String, String)>,
    /// Verbatim lines. Never re-wrapped, never paraphrased.
    pub body: Vec<String>,
}

impl IrSection {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            attrs: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn attr(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.attrs.push((k.into(), v.into()));
        self
    }

    pub fn line(mut self, line: impl Into<String>) -> Self {
        self.body.push(line.into());
        self
    }

    pub fn lines<I: IntoIterator<Item = S>, S: Into<String>>(mut self, lines: I) -> Self {
        self.body.extend(lines.into_iter().map(Into::into));
        self
    }
}

impl IrDoc {
    pub fn new(kind: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            ..Default::default()
        }
    }

    pub fn variant(mut self, v: impl Into<String>) -> Self {
        self.variant = Some(v.into());
        self
    }

    pub fn head(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.header.push((k.into(), v.into()));
        self
    }

    pub fn field(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.fields.push((k.into(), v.into()));
        self
    }

    pub fn field_num(self, k: impl Into<String>, v: impl std::fmt::Display) -> Self {
        self.field(k, v.to_string())
    }

    pub fn section(mut self, s: IrSection) -> Self {
        self.sections.push(s);
        self
    }

    pub fn raw_capsule(mut self, capsule_id: impl std::fmt::Display) -> Self {
        self.raw = Some(format!("cap://{capsule_id}"));
        self
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.header
            .iter()
            .chain(self.fields.iter())
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn render(&self) -> String {
        let mut out = String::with_capacity(256);
        out.push('[');
        out.push_str(&self.kind);
        if let Some(v) = &self.variant {
            out.push(':');
            out.push_str(v);
        }
        out.push(']');
        for (k, v) in &self.header {
            let _ = write!(out, " {k}={}", quote(v));
        }
        out.push('\n');

        if !self.fields.is_empty() {
            let mut first = true;
            for (k, v) in &self.fields {
                if !first {
                    out.push(' ');
                }
                let _ = write!(out, "{k}={}", quote(v));
                first = false;
            }
            out.push('\n');
        }

        for s in &self.sections {
            out.push('@');
            out.push_str(&s.label);
            for (k, v) in &s.attrs {
                let _ = write!(out, " {k}={}", quote(v));
            }
            out.push('\n');
            for line in &s.body {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str("  ");
                    out.push_str(line);
                    out.push('\n');
                }
            }
        }

        if let Some(raw) = &self.raw {
            let _ = writeln!(out, "raw={raw}");
        }
        out
    }
}

fn quote(v: &str) -> String {
    let needs = v.is_empty()
        || v.chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\\');
    if !needs {
        return v.to_string();
    }
    let mut out = String::with_capacity(v.len() + 2);
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse a rendered Token IR document.
pub fn parse(text: &str) -> Result<IrDoc> {
    let mut lines = text.lines().peekable();
    let header_line = lines
        .next()
        .ok_or_else(|| Error::parse("token-ir", "empty document"))?;

    let rest = header_line
        .strip_prefix('[')
        .ok_or_else(|| Error::parse("token-ir", "document must start with `[`"))?;
    let close = rest
        .find(']')
        .ok_or_else(|| Error::parse("token-ir", "unterminated `[kind]` header"))?;
    let (name, tail) = rest.split_at(close);
    let tail = &tail[1..];

    let (kind, variant) = match name.split_once(':') {
        Some((k, v)) => (k.to_string(), Some(v.to_string())),
        None => (name.to_string(), None),
    };
    if kind.is_empty() {
        return Err(Error::parse("token-ir", "empty kind"));
    }

    let mut doc = IrDoc {
        kind,
        variant,
        header: parse_pairs(tail)?,
        ..Default::default()
    };

    let mut current: Option<IrSection> = None;
    for line in lines {
        if let Some(sec) = line.strip_prefix('@') {
            if let Some(prev) = current.take() {
                doc.sections.push(prev);
            }
            let (label, attrs) = match sec.find(' ') {
                Some(i) => (&sec[..i], &sec[i..]),
                None => (sec, ""),
            };
            current = Some(IrSection {
                label: label.to_string(),
                attrs: parse_pairs(attrs)?,
                body: Vec::new(),
            });
        } else if let Some(sec) = current.as_mut() {
            if line.is_empty() {
                sec.body.push(String::new());
            } else if let Some(body) = line.strip_prefix("  ") {
                sec.body.push(body.to_string());
            } else if let Some(raw) = line.strip_prefix("raw=") {
                doc.raw = Some(raw.to_string());
            } else {
                return Err(Error::parse(
                    "token-ir",
                    format!("unindented line inside section `{}`: {line}", sec.label),
                ));
            }
        } else if let Some(raw) = line.strip_prefix("raw=") {
            doc.raw = Some(raw.to_string());
        } else if line.is_empty() {
            continue;
        } else {
            doc.fields.extend(parse_pairs(line)?);
        }
    }
    if let Some(prev) = current.take() {
        doc.sections.push(prev);
    }
    Ok(doc)
}

fn parse_pairs(s: &str) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i] == ' ' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != '=' && bytes[i] != ' ' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != '=' {
            return Err(Error::parse(
                "token-ir",
                format!("expected `key=value` near `{}`", s.trim()),
            ));
        }
        let key: String = bytes[key_start..i].iter().collect();
        i += 1; // skip '='

        let value = if i < bytes.len() && bytes[i] == '"' {
            i += 1;
            let mut v = String::new();
            loop {
                if i >= bytes.len() {
                    return Err(Error::parse("token-ir", "unterminated quoted value"));
                }
                match bytes[i] {
                    '"' => {
                        i += 1;
                        break;
                    }
                    '\\' => {
                        i += 1;
                        if i >= bytes.len() {
                            return Err(Error::parse("token-ir", "dangling escape"));
                        }
                        match bytes[i] {
                            'n' => v.push('\n'),
                            c => v.push(c),
                        }
                        i += 1;
                    }
                    c => {
                        v.push(c);
                        i += 1;
                    }
                }
            }
            v
        } else {
            let start = i;
            while i < bytes.len() && bytes[i] != ' ' {
                i += 1;
            }
            bytes[start..i].iter().collect()
        };
        out.push((key, value));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> IrDoc {
        IrDoc::new("test")
            .variant("pytest")
            .head("status", "fail")
            .field_num("passed", 812)
            .field_num("failed", 2)
            .field("duration", "71.2s")
            .section(
                IrSection::new("fail#1")
                    .attr("at", "tests/auth/test_expiry.py:87")
                    .line("AssertionError expected=401 actual=200")
                    .line("trace=auth/middleware.py:42 > auth/token.py:118"),
            )
            .raw_capsule("cap_01JTEST7")
    }

    #[test]
    fn renders_expected_shape() {
        let rendered = sample().render();
        let expected = "[test:pytest] status=fail\n\
                        passed=812 failed=2 duration=71.2s\n\
                        @fail#1 at=tests/auth/test_expiry.py:87\n\
                        \x20 AssertionError expected=401 actual=200\n\
                        \x20 trace=auth/middleware.py:42 > auth/token.py:118\n\
                        raw=cap://cap_01JTEST7\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn roundtrips() {
        let doc = sample();
        let back = parse(&doc.render()).expect("parse");
        assert_eq!(doc, back);
    }

    #[test]
    fn roundtrips_awkward_values() {
        let doc = IrDoc::new("shell")
            .head("cmd", "git commit -m \"fix: don't break\\n\"")
            .field("empty", "")
            .field("unicode", "日本語")
            .section(IrSection::new("out").lines(["", "  deep indent", "@not-a-section"]));
        let back = parse(&doc.render()).expect("parse");
        assert_eq!(doc, back);
    }

    #[test]
    fn body_is_verbatim() {
        let payload = "expected=401 actual=200 at src/x.rs:9";
        let doc = IrDoc::new("test").section(IrSection::new("f").line(payload));
        assert!(doc.render().contains(payload));
    }

    #[test]
    fn rejects_malformed_documents() {
        assert!(parse("").is_err());
        assert!(parse("no header\n").is_err());
        assert!(parse("[test\n").is_err());
        assert!(parse("[]\n").is_err());
        assert!(parse("[test]\n@sec\nunindented\n").is_err());
        assert!(parse("[test]\nnot-a-pair\n").is_err());
        assert!(parse("[test] k=\"unterminated\n").is_err());
    }

    #[test]
    fn raw_line_after_section_is_recognised() {
        let doc = parse("[git:diff] files=1\n@f a=b\n  +x\nraw=cap://cap_1\n").expect("parse");
        assert_eq!(doc.raw.as_deref(), Some("cap://cap_1"));
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].body, vec!["+x"]);
        assert_eq!(doc.get("files"), Some("1"));
    }
}
