//! Secret and PII redaction.
//!
//! Redaction replaces a detected secret with a stable placeholder
//! `<secret:<kind>_<n>>`. The mapping placeholder → real value stays in
//! memory / in the local capsule store and is never part of anything sent to a
//! model. Redaction is *fail-closed*: if we detect something that looks like a
//! secret we redact it, even at the cost of a false positive.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactionHit {
    pub kind: String,
    pub placeholder: String,
    /// Byte range in the *original* text.
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Redacted {
    pub text: String,
    pub hits: Vec<RedactionHit>,
    /// placeholder → original value. Local only.
    pub mapping: BTreeMap<String, String>,
}

impl Redacted {
    pub fn is_clean(&self) -> bool {
        self.hits.is_empty()
    }

    /// Restore the original text. Used for capsule level 4 access and by the
    /// property tests.
    pub fn restore(&self) -> String {
        let mut out = self.text.clone();
        for (placeholder, value) in &self.mapping {
            out = out.replace(placeholder.as_str(), value);
        }
        out
    }
}

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture group holding the secret itself.
    group: usize,
}

macro_rules! rule {
    ($kind:literal, $pattern:literal, $group:literal) => {
        Rule {
            kind: $kind,
            re: Regex::new($pattern).expect("static regex"),
            group: $group,
        }
    };
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule!(
            "auth_header",
            r"(?i)(authorization\s*[:=]\s*(?:bearer|basic|token)\s+)([A-Za-z0-9._~+/=-]{8,})",
            2
        ),
        rule!("aws_access_key", r"\b((?:AKIA|ASIA)[0-9A-Z]{16})\b", 1),
        rule!("github_token", r"\b(gh[pousr]_[A-Za-z0-9]{20,255})\b", 1),
        rule!("slack_token", r"\b(xox[abposr]-[A-Za-z0-9-]{10,})\b", 1),
        rule!("openai_key", r"\b(sk-[A-Za-z0-9_-]{16,})\b", 1),
        rule!("google_key", r"\b(AIza[0-9A-Za-z_-]{35})\b", 1),
        rule!(
            "private_key",
            r"(?s)(-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----)",
            1
        ),
        rule!(
            "jwt",
            r"\b(eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,})\b",
            1
        ),
        rule!(
            "assigned_secret",
            r#"(?i)\b((?:api[_-]?key|secret|passwd|password|token|access[_-]?key)\w*)\s*[:=]\s*["']?([^\s"'\n,;]{8,})"#,
            2
        ),
        rule!(
            "connection_string",
            r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)([^\s@]{3,})(@)",
            2
        ),
    ]
});

static PII_RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule!(
            "email",
            r"\b([A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,24})\b",
            1
        ),
        rule!(
            "ipv4",
            r"\b((?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?:\.(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3})\b",
            1
        ),
    ]
});

/// Values that look like secrets but are known placeholders.
fn is_placeholder(value: &str) -> bool {
    if value.contains("<secret:") {
        return true;
    }
    let v = value.trim_matches(|c: char| !c.is_alphanumeric());
    let lower = v.to_ascii_lowercase();
    lower.is_empty()
        || lower.contains("example")
        || lower.contains("changeme")
        || lower.contains("redacted")
        || lower.contains("xxxx")
        || lower.chars().all(|c| c == '*' || c == '.' || c == '-')
}

/// Heuristic: a value containing code punctuation is an expression, not a
/// credential. Deliberately narrow — everything else stays fail-closed.
fn looks_like_code(value: &str) -> bool {
    value.contains(['(', ')', '{', '}', '[', ']', '<', '>', '$', '`'])
}

/// Redact secrets (and optionally PII) from `text`.
pub fn redact(text: &str, include_pii: bool) -> Redacted {
    let mut spans: Vec<(usize, usize, &'static str)> = Vec::new();
    let mut rules: Vec<&Rule> = RULES.iter().collect();
    if include_pii {
        rules.extend(PII_RULES.iter());
    }

    for rule in rules {
        for caps in rule.re.captures_iter(text) {
            let Some(m) = caps.get(rule.group) else {
                continue;
            };
            if is_placeholder(m.as_str()) {
                continue;
            }
            // `let token = make_token(ttl=-1)` is source code, not a secret.
            // Only the generic `name = value` rule needs this guard; the
            // provider specific patterns are already unambiguous.
            if rule.kind == "assigned_secret" && looks_like_code(m.as_str()) {
                continue;
            }
            spans.push((m.start(), m.end(), rule.kind));
        }
    }

    if spans.is_empty() {
        return Redacted {
            text: text.to_string(),
            ..Default::default()
        };
    }

    // Longest match wins on overlap; then apply left to right.
    spans.sort_by(|a, b| a.0.cmp(&b.0).then((b.1 - b.0).cmp(&(a.1 - a.0))));
    let mut chosen: Vec<(usize, usize, &'static str)> = Vec::with_capacity(spans.len());
    for s in spans {
        if chosen.last().is_some_and(|last| s.0 < last.1) {
            continue;
        }
        chosen.push(s);
    }

    let mut out = String::with_capacity(text.len());
    let mut hits = Vec::new();
    let mut mapping = BTreeMap::new();
    let mut counters: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_value: BTreeMap<&str, String> = BTreeMap::new();
    let mut cursor = 0usize;

    for (start, end, kind) in chosen {
        let value = &text[start..end];
        let placeholder = match by_value.get(value) {
            Some(p) => p.clone(),
            None => {
                let n = counters.entry(kind).or_insert(0);
                *n += 1;
                let p = format!("<secret:{kind}_{n}>");
                by_value.insert(value, p.clone());
                mapping.insert(p.clone(), value.to_string());
                p
            }
        };
        out.push_str(&text[cursor..start]);
        out.push_str(&placeholder);
        cursor = end;
        hits.push(RedactionHit {
            kind: kind.to_string(),
            placeholder,
            start,
            end,
        });
    }
    out.push_str(&text[cursor..]);

    Redacted {
        text: out,
        hits,
        mapping,
    }
}

/// Cheap check used to decide the sensitivity label without building the
/// redacted copy.
pub fn contains_secret(text: &str) -> bool {
    RULES.iter().any(|r| {
        r.re.captures_iter(text)
            .any(|c| c.get(r.group).is_some_and(|m| !is_placeholder(m.as_str())))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_token() {
        let input = "Authorization: Bearer eyJhbGciOi.eyJzdWIiOiJ4.QWxhZGRpbg\nnext line";
        let r = redact(input, false);
        assert!(!r.is_clean());
        assert!(r.text.contains("<secret:"));
        assert!(!r.text.contains("eyJhbGciOi.eyJzdWIiOiJ4.QWxhZGRpbg"));
        assert!(r.text.starts_with("Authorization: Bearer "));
        assert_eq!(r.restore(), input);
    }

    #[test]
    fn redacts_common_key_shapes() {
        for s in [
            "AKIAQ7RT4YZL2WNPKD3B",
            "ghp_0123456789abcdefghijklmnopqrstuvwx",
            "sk-0123456789abcdefghij",
            "api_key = s3cr3tvalue123",
            "postgres://user:hunter2pass@localhost/db",
        ] {
            let r = redact(s, false);
            assert!(!r.is_clean(), "no secret detected in `{s}`");
            assert_eq!(r.restore(), s, "restore failed for `{s}`");
        }
    }

    #[test]
    fn placeholders_are_not_redacted_again() {
        let once = redact("api_key = realvalue1234", false);
        let twice = redact(&once.text, false);
        assert!(
            twice.is_clean(),
            "double redaction produced {:?}",
            twice.text
        );
    }

    #[test]
    fn identical_values_share_a_placeholder() {
        let r = redact("t=abcdefghijkl and token=abcdefghijkl", false);
        assert_eq!(r.mapping.len(), 1, "{:?}", r.mapping);
    }

    #[test]
    fn source_code_is_not_mistaken_for_a_secret() {
        for line in [
            "        token = make_token(ttl=-1)",
            "let password = prompt_user();",
            "api_key = os.environ[\"API_KEY\"]",
        ] {
            assert!(
                redact(line, false).is_clean(),
                "false positive on `{line}`: {:?}",
                redact(line, false).hits
            );
        }
        // …but a literal value is still caught.
        assert!(!redact("token = 8f3a91c04d77bb21", false).is_clean());
    }

    #[test]
    fn clean_text_is_untouched() {
        let input = "just a normal line with a number 42 and a path src/main.rs:9";
        let r = redact(input, false);
        assert!(r.is_clean());
        assert_eq!(r.text, input);
        assert!(!contains_secret(input));
    }

    #[test]
    fn pii_is_opt_in() {
        let input = "contact person@example.org from 10.0.0.7";
        assert!(redact(input, false).is_clean());
        // `example` is treated as a placeholder, so use a non-example domain.
        let input = "contact person@corp.internal from 10.0.0.7";
        let r = redact(input, true);
        assert_eq!(r.hits.len(), 2, "{:?}", r.hits);
        assert_eq!(r.restore(), input);
    }

    #[test]
    fn unicode_offsets_are_safe() {
        let input = "日本語 token=abcdefgh1234 日本語";
        let r = redact(input, false);
        assert_eq!(r.restore(), input);
    }
}
