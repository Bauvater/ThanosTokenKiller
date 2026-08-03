//! Token accounting.
//!
//! ThanosTokenKiller never reports a number without saying *how* it was
//! obtained. `bytes / 4` style guesses are allowed only as
//! [`CountMethod::Estimated`] and are rendered with a `~` prefix everywhere.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CountMethod {
    /// Reported by the model provider in a usage field.
    Provider,
    /// Computed with a real tokenizer for the target model.
    Tokenizer,
    /// Heuristic estimate. Never presented as exact.
    Estimated,
    /// No number available at all.
    Unavailable,
}

impl CountMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            CountMethod::Provider => "provider",
            CountMethod::Tokenizer => "tokenizer",
            CountMethod::Estimated => "estimated",
            CountMethod::Unavailable => "unavailable",
        }
    }

    /// True when the value may be presented as an exact measurement.
    pub fn is_exact(self) -> bool {
        matches!(self, CountMethod::Provider | CountMethod::Tokenizer)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenCount {
    pub value: u64,
    pub method: CountMethod,
}

impl TokenCount {
    pub const fn new(value: u64, method: CountMethod) -> Self {
        Self { value, method }
    }

    pub const fn estimated(value: u64) -> Self {
        Self::new(value, CountMethod::Estimated)
    }

    pub const fn unavailable() -> Self {
        Self::new(0, CountMethod::Unavailable)
    }

    /// Difference `self - other`, degrading the method to the weaker of both.
    pub fn saved_against(self, other: TokenCount) -> TokenCount {
        let method = if self.method.is_exact() && other.method.is_exact() {
            CountMethod::Tokenizer
        } else if self.method == CountMethod::Unavailable
            || other.method == CountMethod::Unavailable
        {
            CountMethod::Unavailable
        } else {
            CountMethod::Estimated
        };
        TokenCount::new(self.value.saturating_sub(other.value), method)
    }
}

impl fmt::Display for TokenCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.method {
            CountMethod::Unavailable => f.write_str("n/a"),
            CountMethod::Estimated => write!(f, "~{}", self.value),
            _ => write!(f, "{}", self.value),
        }
    }
}

/// Heuristic token estimator.
///
/// Calibrated against byte-pair encodings used by current OpenAI/Anthropic
/// models: ASCII words split roughly every 4 characters, digits every 3, CJK
/// is roughly one token per character, and punctuation is its own token.
/// Single pass over the input, no allocation.
pub fn estimate(text: &str) -> TokenCount {
    let mut tokens: u64 = 0;
    let mut word_len: usize = 0;
    let mut digit_run: usize = 0;

    #[inline]
    fn flush(len: &mut usize, per: usize, tokens: &mut u64) {
        if *len > 0 {
            *tokens += (*len).div_ceil(per) as u64;
            *len = 0;
        }
    }

    for ch in text.chars() {
        if ch.is_ascii_alphabetic() || ch == '_' {
            flush(&mut digit_run, 3, &mut tokens);
            word_len += 1;
        } else if ch.is_ascii_digit() {
            flush(&mut word_len, 4, &mut tokens);
            digit_run += 1;
        } else {
            flush(&mut word_len, 4, &mut tokens);
            flush(&mut digit_run, 3, &mut tokens);
            if ch == ' ' {
                // Leading spaces merge into the following token in BPE.
                continue;
            }
            if ch == '\n' || ch == '\t' || ch == '\r' {
                tokens += 1;
            } else if (ch as u32) < 128 {
                tokens += 1; // punctuation / symbol
            } else if is_wide(ch) {
                tokens += 1; // CJK & friends: ~1 token per character
            } else {
                tokens += 1; // other non-ascii: conservative 1 token
            }
        }
    }
    flush(&mut word_len, 4, &mut tokens);
    flush(&mut digit_run, 3, &mut tokens);

    TokenCount::estimated(tokens)
}

fn is_wide(ch: char) -> bool {
    matches!(ch as u32,
        0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 |
        0xF900..=0xFAFF | 0xFE30..=0xFE6F | 0xFF00..=0xFF60 |
        0xFFE0..=0xFFE6 | 0x20000..=0x3FFFD)
}

/// Convenience: estimated tokens for a slice of strings.
pub fn estimate_all<'a>(parts: impl IntoIterator<Item = &'a str>) -> TokenCount {
    let mut total = 0u64;
    for p in parts {
        total += estimate(p).value;
    }
    TokenCount::estimated(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_is_in_a_sane_range() {
        // Roughly 1 token per 4 characters of English prose.
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(20);
        let est = estimate(&text).value as f64;
        let chars = text.chars().count() as f64;
        let ratio = chars / est;
        assert!(
            (2.5..=6.0).contains(&ratio),
            "chars/token ratio was {ratio}"
        );
    }

    #[test]
    fn estimates_are_marked() {
        let c = estimate("hello world");
        assert_eq!(c.method, CountMethod::Estimated);
        assert!(c.to_string().starts_with('~'));
        assert_eq!(TokenCount::unavailable().to_string(), "n/a");
    }

    #[test]
    fn empty_input_is_zero() {
        assert_eq!(estimate("").value, 0);
    }

    #[test]
    fn saving_degrades_method() {
        let exact = TokenCount::new(100, CountMethod::Provider);
        let est = TokenCount::estimated(40);
        assert_eq!(exact.saved_against(est).method, CountMethod::Estimated);
        assert_eq!(exact.saved_against(est).value, 60);
        assert_eq!(
            exact
                .saved_against(TokenCount::new(40, CountMethod::Tokenizer))
                .method,
            CountMethod::Tokenizer
        );
        assert_eq!(
            exact.saved_against(TokenCount::unavailable()).method,
            CountMethod::Unavailable
        );
    }

    #[test]
    fn saving_never_underflows() {
        let small = TokenCount::estimated(3);
        let big = TokenCount::estimated(30);
        assert_eq!(small.saved_against(big).value, 0);
    }

    #[test]
    fn unicode_does_not_panic() {
        for s in ["日本語のテキスト", "🙂🙃", "e\u{301}\u{200b}"] {
            let _ = estimate(s);
        }
    }
}
