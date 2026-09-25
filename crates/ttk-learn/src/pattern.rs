//! Token templates: what a learned rule actually is.
//!
//! A rule is deliberately **not** a regular expression. Expressions produced by
//! a language model are unreviewable, they over-match, and a bad one silently
//! deletes evidence. A template is a fixed sequence of tokens where each token
//! is either a literal that must match exactly or a placeholder for one
//! *volatile* value. Matching is a linear token walk: no backtracking exists to
//! go wrong.
//!
//! ```text
//! npm WARN deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported
//!   ↓ tokenise, classify
//! npm WARN deprecated {ver} Glob versions prior to {ver} are no longer supported
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

/// Classes of token that vary between two runs of the same command.
///
/// Everything that is *not* one of these stays literal, which is what keeps a
/// template specific enough to be safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// `42`, `-3`, `1_000`, `12.5`, `98%`
    Num,
    /// `1.2.3`, `v9.0.0-rc.1`, `pkg@7.2.3`
    Version,
    /// A hex run long enough to be a hash or an object id.
    Hex,
    /// `9f1c2d3e-...` in canonical 8-4-4-4-12 shape.
    Uuid,
    /// `12:04:59`, `2026-09-05T10:11:12Z`, `2026-09-05`
    Time,
    /// `1.4MB`, `300ms`, `9s`, `71.20s`
    Size,
    /// Anything with a directory separator, or a bare `name.ext`.
    Path,
    /// `http://…`, `https://…`, `file://…`
    Url,
    /// A token that varied between two lessons and carries no other class.
    Any,
}

impl Class {
    pub fn as_str(self) -> &'static str {
        match self {
            Class::Num => "n",
            Class::Version => "ver",
            Class::Hex => "hex",
            Class::Uuid => "uuid",
            Class::Time => "t",
            Class::Size => "size",
            Class::Path => "path",
            Class::Url => "url",
            Class::Any => "*",
        }
    }
}

impl fmt::Display for Class {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}", self.as_str())
    }
}

/// One position in a template.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Seg {
    Lit { v: String },
    Cls { c: Class },
}

impl Seg {
    fn matches(&self, token: &str) -> bool {
        match self {
            Seg::Lit { v } => v == token,
            Seg::Cls { c: Class::Any } => true,
            Seg::Cls { c } => classify(token) == Some(*c),
        }
    }

    pub fn is_literal(&self) -> bool {
        matches!(self, Seg::Lit { .. })
    }
}

impl fmt::Display for Seg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Seg::Lit { v } => f.write_str(v),
            Seg::Cls { c } => write!(f, "{c}"),
        }
    }
}

/// A whole line pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Template {
    pub segs: Vec<Seg>,
}

/// Upper bound on tokens per line that a template will ever consider.
///
/// A single pathological 100k-token line must not turn matching into a
/// noticeable cost, and no genuine noise line is 64 tokens wide.
pub const MAX_TOKENS: usize = 64;

impl Template {
    /// Build the most specific template that still tolerates volatile values.
    ///
    /// Returns `None` for a line that carries nothing to match on — empty,
    /// whitespace only, or wider than [`MAX_TOKENS`].
    pub fn from_line(line: &str) -> Option<Self> {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() || tokens.len() > MAX_TOKENS {
            return None;
        }
        let segs = tokens
            .iter()
            .map(|t| match classify(t) {
                Some(c) => Seg::Cls { c },
                None => Seg::Lit {
                    v: (*t).to_string(),
                },
            })
            .collect();
        Some(Template { segs })
    }

    pub fn matches(&self, line: &str) -> bool {
        let mut segs = self.segs.iter();
        let mut count = 0usize;
        for token in line.split_whitespace() {
            count += 1;
            if count > MAX_TOKENS {
                return false;
            }
            match segs.next() {
                Some(seg) if seg.matches(token) => {}
                _ => return false,
            }
        }
        count > 0 && segs.next().is_none()
    }

    /// Widen `self` so it also covers `other`, or `None` if they are too far
    /// apart to merge.
    ///
    /// Only same-length templates that differ in at most one *new* position
    /// merge, and only that one position widens. Refusing to merge is how a
    /// rule stays narrow.
    ///
    /// A position that is already `{*}` on either side absorbs the other
    /// without spending the budget: without that, `alpha|beta` could merge but
    /// `alpha|beta|gamma` could not, because the third merge would see both an
    /// existing `{*}` and nothing else to widen. Absorbing costs no generality
    /// — the position matches everything already — and it is what makes a
    /// repeated pairwise merge behave like a proper multi-way one.
    pub fn merge(&self, other: &Template) -> Option<Template> {
        if self.segs.len() != other.segs.len() {
            return None;
        }
        const ANY: Seg = Seg::Cls { c: Class::Any };
        let mut out = Vec::with_capacity(self.segs.len());
        let mut diffs = 0;
        for (a, b) in self.segs.iter().zip(&other.segs) {
            if a == b {
                out.push(a.clone());
                continue;
            }
            if *a == ANY || *b == ANY {
                out.push(ANY);
                continue;
            }
            diffs += 1;
            if diffs > 1 {
                return None;
            }
            out.push(ANY);
        }
        Some(Template { segs: out })
    }

    pub fn literal_tokens(&self) -> usize {
        self.segs.iter().filter(|s| s.is_literal()).count()
    }

    pub fn literal_chars(&self) -> usize {
        self.segs
            .iter()
            .map(|s| match s {
                Seg::Lit { v } => v.chars().count(),
                Seg::Cls { .. } => 0,
            })
            .sum()
    }

    pub fn len(&self) -> usize {
        self.segs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.segs.is_empty()
    }

    /// Stable canonical form, used for the rule id and for display.
    pub fn canonical(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, seg) in self.segs.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{seg}")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Patterns: one line, or a run of them
// ---------------------------------------------------------------------------

/// Upper bound on the height of a block pattern.
///
/// A banner taller than this is not a banner, and an unbounded height would let
/// one rule claim an arbitrary share of a file.
pub const MAX_BLOCK_LINES: usize = 32;

/// What a rule matches: a single line, or a run of consecutive lines.
///
/// Block patterns exist for the output that a line pattern *cannot* describe
/// safely. A twelve line ASCII banner is twelve lines of `====` and stray
/// punctuation; each one on its own is far too generic to be a rule, and
/// [`crate::rule::check`] rightly refuses it. Together they are unmistakable.
/// So a block is not a shortcut around the guards — it is the shape that lets
/// the guards say yes to something they otherwise have to reject.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pattern {
    /// Never empty; a single entry is a line pattern.
    lines: Vec<Template>,
}

impl Pattern {
    pub fn line(template: Template) -> Self {
        Self {
            lines: vec![template],
        }
    }

    /// A block of at least two lines, capped at [`MAX_BLOCK_LINES`].
    ///
    /// Returns `None` for anything that is not actually a block, so a caller
    /// cannot accidentally build a one line "block" that then takes the block
    /// code path everywhere.
    pub fn block(lines: Vec<Template>) -> Option<Self> {
        if lines.len() < 2 || lines.len() > MAX_BLOCK_LINES {
            return None;
        }
        Some(Self { lines })
    }

    pub fn height(&self) -> usize {
        self.lines.len()
    }

    pub fn is_block(&self) -> bool {
        self.lines.len() > 1
    }

    pub fn lines(&self) -> &[Template] {
        &self.lines
    }

    pub fn first(&self) -> &Template {
        &self.lines[0]
    }

    /// Does this pattern match exactly this one line?
    ///
    /// Always false for a block: a block describes a run, and answering "yes"
    /// for one line of it would let a caller remove that line alone.
    pub fn matches(&self, line: &str) -> bool {
        !self.is_block() && self.lines[0].matches(line)
    }

    /// Does any single line of this pattern match `line`?
    ///
    /// Used only to decide whether a rule is the one that removed a line a user
    /// has since marked with `<filter-keep>`. It deliberately over-approximates
    /// for blocks: retiring a rule is reversible, removing evidence is not.
    pub fn any_line_matches(&self, line: &str) -> bool {
        self.lines.iter().any(|t| t.matches(line))
    }

    /// Try to match starting at `lines[start]`, returning how many input lines
    /// were consumed.
    ///
    /// Blank lines *between* two template lines are skipped and counted, which
    /// is what real banners look like. A blank line can never be the first or
    /// last line consumed, so a match can neither start nor end on one.
    pub fn match_run(&self, lines: &[&str], start: usize) -> Option<usize> {
        let mut i = start;
        for (n, template) in self.lines.iter().enumerate() {
            // Skip blanks, but only once a first real line has been matched:
            // otherwise a run of blanks before unrelated text could anchor us.
            if n > 0 {
                while i < lines.len() && lines[i].trim().is_empty() {
                    i += 1;
                }
            }
            let line = lines.get(i)?;
            if !template.matches(line) {
                return None;
            }
            i += 1;
        }
        Some(i - start)
    }

    pub fn literal_tokens(&self) -> usize {
        self.lines.iter().map(Template::literal_tokens).sum()
    }

    pub fn literal_chars(&self) -> usize {
        self.lines.iter().map(Template::literal_chars).sum()
    }

    /// Total number of positions across every line.
    pub fn len(&self) -> usize {
        self.lines.iter().map(Template::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(Template::is_empty)
    }

    /// Stable canonical form: the lines, newline separated.
    pub fn canonical(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{line}")?;
        }
        Ok(())
    }
}

/// On-disk shape of a [`Pattern`].
///
/// A line pattern serialises as the bare template it has always been, so a rule
/// file written before block rules existed still loads, and the common case
/// stays as readable as it was. A block serialises as a list of templates. The
/// two are unambiguous: a template is a list of *objects*, a block is a list of
/// *lists*.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum PatternWire {
    Block(Vec<Template>),
    Line(Template),
}

impl Serialize for Pattern {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self.lines.as_slice() {
            [one] => PatternWire::Line(one.clone()).serialize(s),
            many => PatternWire::Block(many.to_vec()).serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match PatternWire::deserialize(d)? {
            PatternWire::Line(t) => Ok(Pattern::line(t)),
            PatternWire::Block(lines) => {
                if lines.is_empty() {
                    return Err(D::Error::custom("a pattern needs at least one line"));
                }
                if lines.len() > MAX_BLOCK_LINES {
                    return Err(D::Error::custom(format!(
                        "a block pattern may be at most {MAX_BLOCK_LINES} lines"
                    )));
                }
                Ok(Pattern { lines })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

/// Which volatile class `token` belongs to, if any.
///
/// Order matters: the most specific shape wins, so a UUID is never seen as a
/// hex run and a URL is never seen as a path.
pub fn classify(token: &str) -> Option<Class> {
    let t = trim_punct(token);
    if t.is_empty() {
        return None;
    }
    if is_url(t) {
        return Some(Class::Url);
    }
    if is_uuid(t) {
        return Some(Class::Uuid);
    }
    if is_time(t) {
        return Some(Class::Time);
    }
    if is_size(t) {
        return Some(Class::Size);
    }
    if is_version(t) {
        return Some(Class::Version);
    }
    if is_hex(t) {
        return Some(Class::Hex);
    }
    if is_num(t) {
        return Some(Class::Num);
    }
    if is_path(t) {
        return Some(Class::Path);
    }
    None
}

/// Strip trailing punctuation that carries no information.
///
/// Leading punctuation is kept: `-3` and `+3` are different numbers, and a
/// leading `(` belongs to the literal shape of the line.
fn trim_punct(token: &str) -> &str {
    token.trim_end_matches([',', ':', ';', ')', ']', '}', '.', '"', '\'', '!', '?'])
}

fn is_url(t: &str) -> bool {
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with("file://")
        || t.starts_with("git+")
        || t.starts_with("ssh://")
}

fn is_uuid(t: &str) -> bool {
    let lens: Vec<usize> = t.split('-').map(str::len).collect();
    lens == [8, 4, 4, 4, 12]
        && t.split('-')
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_time(t: &str) -> bool {
    let digits = t.chars().filter(char::is_ascii_digit).count();
    // `12:04:59`, `10:11`
    if t.contains(':')
        && digits >= 3
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == ':' || c == '.')
    {
        return true;
    }
    // `2026-09-05`, `2026-09-05T10:11:12Z`, `2026-09-05T10:11:12.345+02:00`
    let bytes = t.as_bytes();
    bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit)
}

/// A number glued to a unit: `1.4MB`, `300ms`, `71.20s`, `9KiB`.
///
/// `%` is deliberately not a unit here: `98%` is a plain number and belongs to
/// [`Class::Num`], where every other percentage in the codebase already is.
fn is_size(t: &str) -> bool {
    let split = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || *c == '.' || *c == ','))
        .map(|(i, _)| i);
    let Some(i) = split else { return false };
    if i == 0 {
        return false;
    }
    let (num, unit) = t.split_at(i);
    if !num.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    matches!(
        unit.to_ascii_lowercase().as_str(),
        "b" | "kb"
            | "mb"
            | "gb"
            | "tb"
            | "kib"
            | "mib"
            | "gib"
            | "tib"
            | "ns"
            | "us"
            | "µs"
            | "ms"
            | "s"
            | "m"
            | "h"
            | "x"
    )
}

/// `1.2.3`, `v9.0.0`, `pkg@7.2.3`, `1.2.3-rc.1+build`.
fn is_version(t: &str) -> bool {
    let core = match t.rsplit_once('@') {
        // `@scope/pkg` is a package name, not a version.
        Some((head, tail)) if !head.is_empty() && !tail.is_empty() => tail,
        _ => t,
    };
    let core = core.strip_prefix('v').unwrap_or(core);
    let head = core.split(['-', '+']).next().unwrap_or(core);
    let parts: Vec<&str> = head.split('.').collect();
    parts.len() >= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// A hex run long enough that it can only be a hash, an object id or an
/// address. Short hex is left literal: `add`, `beef` and `cafe` are words.
fn is_hex(t: &str) -> bool {
    let t = t.strip_prefix("0x").unwrap_or(t);
    t.len() >= 7
        && t.chars().all(|c| c.is_ascii_hexdigit())
        && t.chars().any(|c| c.is_ascii_digit())
}

fn is_num(t: &str) -> bool {
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    let t = t.strip_suffix('%').unwrap_or(t);
    !t.is_empty()
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ',' || c == '_')
}

/// Path-shaped: a separator with something on both sides, or `name.ext`.
fn is_path(t: &str) -> bool {
    if t.len() < 3 {
        return false;
    }
    if t.contains(['/', '\\']) {
        return true;
    }
    match t.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && (1..=6).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && stem.chars().any(|c| c.is_ascii_alphanumeric())
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volatile_tokens_are_classified() {
        assert_eq!(classify("42"), Some(Class::Num));
        assert_eq!(classify("-3.5"), Some(Class::Num));
        assert_eq!(classify("98%"), Some(Class::Num));
        assert_eq!(classify("1.2.3"), Some(Class::Version));
        assert_eq!(classify("v9.0.0-rc.1"), Some(Class::Version));
        assert_eq!(classify("glob@7.2.3"), Some(Class::Version));
        assert_eq!(classify("a1b2c3d4e5"), Some(Class::Hex));
        assert_eq!(
            classify("9f1c2d3e-4a5b-6c7d-8e9f-0a1b2c3d4e5f"),
            Some(Class::Uuid)
        );
        assert_eq!(classify("12:04:59"), Some(Class::Time));
        assert_eq!(classify("2026-09-05T10:11:12Z"), Some(Class::Time));
        assert_eq!(classify("71.20s"), Some(Class::Size));
        assert_eq!(classify("1.4MB"), Some(Class::Size));
        assert_eq!(classify("src/main.rs"), Some(Class::Path));
        assert_eq!(classify("main.rs"), Some(Class::Path));
        assert_eq!(classify("https://example.com/x"), Some(Class::Url));
    }

    #[test]
    fn ordinary_words_stay_literal() {
        for word in [
            "deprecated",
            "WARN",
            "supported",
            "beef",
            "add",
            "no",
            "npm",
        ] {
            assert_eq!(classify(word), None, "`{word}` must stay literal");
        }
    }

    #[test]
    fn trailing_punctuation_does_not_hide_a_class() {
        assert_eq!(classify("1.2.3,"), Some(Class::Version));
        assert_eq!(classify("glob@7.2.3:"), Some(Class::Version));
    }

    #[test]
    fn a_template_matches_the_same_line_with_other_values() {
        let t = Template::from_line(
            "npm WARN deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported",
        )
        .expect("template");
        assert!(t.matches(
            "npm WARN deprecated inflight@1.0.6: Glob versions prior to v9 are no longer supported"
        ));
        assert!(!t.matches(
            "npm ERROR deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported"
        ));
        assert!(!t.matches("npm WARN deprecated glob@7.2.3:"));
    }

    #[test]
    fn whitespace_is_not_part_of_the_pattern() {
        let t = Template::from_line("added 412 packages in 9s").expect("template");
        assert!(t.matches("   added   3 packages in 12s  "));
    }

    #[test]
    fn empty_and_oversized_lines_have_no_template() {
        assert!(Template::from_line("").is_none());
        assert!(Template::from_line("   \t ").is_none());
        let wide = "x ".repeat(MAX_TOKENS + 1);
        assert!(Template::from_line(&wide).is_none());
    }

    #[test]
    fn merging_widens_exactly_one_position() {
        let a = Template::from_line("Downloading package alpha from cache").expect("a");
        let b = Template::from_line("Downloading package beta from cache").expect("b");
        let m = a.merge(&b).expect("merged");
        assert_eq!(m.to_string(), "Downloading package {*} from cache");
        assert!(m.matches("Downloading package gamma from cache"));
        assert!(!m.matches("Downloading package gamma from disk"));
    }

    #[test]
    fn merging_refuses_two_differences_or_a_length_change() {
        let a = Template::from_line("one two three").expect("a");
        assert!(
            a.merge(&Template::from_line("one x y").expect("b"))
                .is_none()
        );
        assert!(
            a.merge(&Template::from_line("one two three four").expect("b"))
                .is_none()
        );
    }

    fn tpl(line: &str) -> Template {
        Template::from_line(line).expect("template")
    }

    #[test]
    fn three_way_generalisation_works_by_repeated_merging() {
        let a = tpl("Downloading package alpha from the registry cache");
        let b = tpl("Downloading package beta from the registry cache");
        let c = tpl("Downloading package gamma from the registry cache");
        let ab = a.merge(&b).expect("a+b");
        // The third merge sees an existing `{*}` and must absorb it rather than
        // spending its one budgeted difference on it again.
        let abc = ab.merge(&c).expect("ab+c");
        assert_eq!(
            abc.to_string(),
            "Downloading package {*} from the registry cache"
        );
        assert!(abc.matches("Downloading package delta from the registry cache"));
        // Absorbing is free, but it is not a licence to widen a second slot.
        assert!(
            abc.merge(&tpl("Downloading module delta from the registry cache"))
                .is_some(),
            "one new difference is still allowed"
        );
        assert!(
            abc.merge(&tpl("Fetching module delta from the registry cache"))
                .is_none(),
            "two new differences are not"
        );
    }

    #[test]
    fn a_line_pattern_is_not_a_block() {
        let p = Pattern::line(tpl("npm WARN deprecated pkg@1.0.6"));
        assert!(!p.is_block());
        assert_eq!(p.height(), 1);
        assert!(p.matches("npm WARN deprecated pkg@2.0.0"));
        assert!(Pattern::block(vec![tpl("only one")]).is_none());
    }

    #[test]
    fn a_block_matches_a_run_and_nothing_less() {
        let banner = ["=== top ===", "  middle line here", "=== bottom ==="];
        let p = Pattern::block(banner.iter().map(|l| tpl(l)).collect()).expect("block");
        assert!(p.is_block());
        assert_eq!(p.height(), 3);

        let lines: Vec<&str> = vec![
            "before",
            "=== top ===",
            "  middle line here",
            "=== bottom ===",
            "after",
        ];
        assert_eq!(p.match_run(&lines, 1), Some(3));
        assert_eq!(p.match_run(&lines, 0), None);
        assert_eq!(p.match_run(&lines, 2), None);
        // A block never claims one of its own lines on its own.
        assert!(!p.matches("=== top ==="));
        // …but it is still identifiable as the rule that removed it.
        assert!(p.any_line_matches("=== top ==="));
    }

    #[test]
    fn a_block_skips_blank_lines_inside_the_run_but_not_around_it() {
        let p = Pattern::block(vec![tpl("=== top ==="), tpl("=== bottom ===")]).expect("block");
        let inside: Vec<&str> = vec!["=== top ===", "", "=== bottom ==="];
        assert_eq!(p.match_run(&inside, 0), Some(3), "the blank goes with it");

        let leading: Vec<&str> = vec!["", "=== top ===", "=== bottom ==="];
        assert_eq!(
            p.match_run(&leading, 0),
            None,
            "a match may not start on a blank line"
        );
        assert_eq!(p.match_run(&leading, 1), Some(2));
    }

    #[test]
    fn a_block_that_runs_off_the_end_does_not_match() {
        let p = Pattern::block(vec![tpl("=== top ==="), tpl("=== bottom ===")]).expect("block");
        let lines: Vec<&str> = vec!["=== top ==="];
        assert_eq!(p.match_run(&lines, 0), None);
    }

    #[test]
    fn a_block_taller_than_the_cap_is_refused() {
        let many: Vec<Template> = (0..MAX_BLOCK_LINES + 1)
            .map(|i| tpl(&format!("line number {i} of the banner")))
            .collect();
        assert!(Pattern::block(many).is_none());
    }

    /// A line rule keeps the exact on-disk shape it had before block rules
    /// existed, so an older rule file still loads and a newer one still reads
    /// the same way in a diff.
    #[test]
    fn patterns_round_trip_through_json_in_both_shapes() {
        let line = Pattern::line(tpl("npm WARN deprecated pkg@1.0.6"));
        let json = serde_json::to_string(&line).expect("serialize");
        assert!(
            json.starts_with("[{"),
            "a line is a list of segments: {json}"
        );
        assert_eq!(
            serde_json::from_str::<Pattern>(&json).expect("round trip"),
            line
        );

        let block = Pattern::block(vec![tpl("=== top ==="), tpl("=== bottom ===")]).expect("block");
        let json = serde_json::to_string(&block).expect("serialize");
        assert!(
            json.starts_with("[[{"),
            "a block is a list of lines: {json}"
        );
        assert_eq!(
            serde_json::from_str::<Pattern>(&json).expect("round trip"),
            block
        );
    }

    #[test]
    fn a_malformed_pattern_is_refused_rather_than_guessed_at() {
        assert!(serde_json::from_str::<Pattern>("[]").is_err());
        let too_tall = format!(
            "[{}]",
            r#"[{"t":"lit","v":"x"}]"#.to_string()
                + &format!(",{}", r#"[{"t":"lit","v":"x"}]"#).repeat(MAX_BLOCK_LINES)
        );
        assert!(serde_json::from_str::<Pattern>(&too_tall).is_err());
    }

    #[test]
    fn a_block_is_measured_across_all_of_its_lines() {
        let b = Pattern::block(vec![tpl("aaa bbb"), tpl("ccc 42")]).expect("block");
        assert_eq!(b.literal_tokens(), 3);
        assert_eq!(b.literal_chars(), 9);
        assert_eq!(b.len(), 4);
        assert_eq!(b.canonical(), "aaa bbb\nccc {n}");
    }

    #[test]
    fn canonical_form_is_readable_and_stable() {
        let t = Template::from_line("compiled 12 files in 3.4s").expect("t");
        assert_eq!(t.canonical(), "compiled {n} files in {size}");
        assert_eq!(
            t.canonical(),
            Template::from_line("compiled 9 files in 1.0s")
                .expect("t2")
                .canonical()
        );
        assert_eq!(t.literal_tokens(), 3);
        assert_eq!(t.literal_chars(), "compiledfilesin".len());
    }
}
