//! Content type model and conservative content detection.
//!
//! Detection is intentionally cheap (no full parse for the common cases) and
//! conservative: anything we are not confident about becomes
//! [`ContentType::Unknown`], which the pipeline treats as *pass-through only*.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    PlainText,
    Markdown,
    SourceCode,
    Json,
    Yaml,
    Toml,
    Xml,
    Csv,
    ShellOutput,
    GitOutput,
    TestOutput,
    CompilerOutput,
    StackTrace,
    ApplicationLog,
    TabularData,
    ToolSchema,
    Unknown,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            ContentType::PlainText => "plain_text",
            ContentType::Markdown => "markdown",
            ContentType::SourceCode => "source_code",
            ContentType::Json => "json",
            ContentType::Yaml => "yaml",
            ContentType::Toml => "toml",
            ContentType::Xml => "xml",
            ContentType::Csv => "csv",
            ContentType::ShellOutput => "shell_output",
            ContentType::GitOutput => "git_output",
            ContentType::TestOutput => "test_output",
            ContentType::CompilerOutput => "compiler_output",
            ContentType::StackTrace => "stack_trace",
            ContentType::ApplicationLog => "application_log",
            ContentType::TabularData => "tabular_data",
            ContentType::ToolSchema => "tool_schema",
            ContentType::Unknown => "unknown",
        }
    }

    /// Content that must never be paraphrased or semantically rewritten.
    pub fn is_literal_sensitive(self) -> bool {
        matches!(
            self,
            ContentType::SourceCode
                | ContentType::Json
                | ContentType::Yaml
                | ContentType::Toml
                | ContentType::Xml
                | ContentType::ToolSchema
                | ContentType::Unknown
        )
    }
}

/// Confidence of a detection result. Anything below `High` keeps the pipeline
/// in structural-only mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    None,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detection {
    pub content_type: ContentType,
    pub confidence: Confidence,
    /// Short, human readable reason. Shown by `ttk explain`.
    pub reason: String,
}

impl Detection {
    fn new(content_type: ContentType, confidence: Confidence, reason: &str) -> Self {
        Self {
            content_type,
            confidence,
            reason: reason.to_string(),
        }
    }

    pub fn unknown() -> Self {
        Self::new(
            ContentType::Unknown,
            Confidence::None,
            "no detector matched",
        )
    }
}

const MAX_SNIFF_BYTES: usize = 64 * 1024;

/// Detect the content type of `text`, optionally helped by a file name.
///
/// Only the first [`MAX_SNIFF_BYTES`] are inspected, so detection cost is
/// bounded regardless of input size.
pub fn detect(text: &str, hint_name: Option<&str>) -> Detection {
    if let Some(name) = hint_name
        && let Some(d) = by_extension(name)
    {
        return d;
    }

    let head: &str = if text.len() > MAX_SNIFF_BYTES {
        // Never split a UTF-8 char.
        let mut end = MAX_SNIFF_BYTES;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        &text[..end]
    } else {
        text
    };
    let trimmed = head.trim_start();

    if trimmed.is_empty() {
        return Detection::new(ContentType::PlainText, Confidence::High, "empty input");
    }

    // JSON: cheap prefix check plus a real parse of the *whole* input, because
    // downstream JSON handling requires an exact tree.
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if serde_json::from_str::<serde_json::Value>(text).is_ok() {
            return Detection::new(ContentType::Json, Confidence::High, "parsed as JSON");
        }
        return Detection::new(
            ContentType::Unknown,
            Confidence::Low,
            "looks like JSON but does not parse",
        );
    }

    if trimmed.starts_with("<?xml") || (trimmed.starts_with('<') && trimmed.contains("</")) {
        return Detection::new(ContentType::Xml, Confidence::Medium, "XML-like markup");
    }

    if is_stack_trace(head) {
        return Detection::new(
            ContentType::StackTrace,
            Confidence::High,
            "stack trace frame markers",
        );
    }
    if is_log(head) {
        return Detection::new(
            ContentType::ApplicationLog,
            Confidence::Medium,
            "timestamped / levelled log lines",
        );
    }
    if let Some(d) = is_delimited(head) {
        return d;
    }
    if is_markdown(head) {
        return Detection::new(
            ContentType::Markdown,
            Confidence::Medium,
            "markdown headings / fences",
        );
    }

    Detection::new(
        ContentType::PlainText,
        Confidence::Low,
        "fallback: plain text",
    )
}

fn by_extension(name: &str) -> Option<Detection> {
    let ext = name.rsplit('.').next()?.to_ascii_lowercase();
    let (ct, why) = match ext.as_str() {
        "json" => (ContentType::Json, "extension .json"),
        "yaml" | "yml" => (ContentType::Yaml, "extension .yaml"),
        "toml" => (ContentType::Toml, "extension .toml"),
        "xml" | "html" | "htm" | "svg" => (ContentType::Xml, "extension .xml-family"),
        "csv" => (ContentType::Csv, "extension .csv"),
        "tsv" => (ContentType::TabularData, "extension .tsv"),
        "md" | "markdown" => (ContentType::Markdown, "extension .md"),
        "log" => (ContentType::ApplicationLog, "extension .log"),
        "txt" => (ContentType::PlainText, "extension .txt"),
        "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "go" | "java" | "kt" | "c" | "h" | "cc"
        | "cpp" | "hpp" | "cs" | "rb" | "php" | "swift" | "scala" | "sh" | "bash" | "ps1" => {
            (ContentType::SourceCode, "source file extension")
        }
        _ => return None,
    };
    Some(Detection::new(ct, Confidence::High, why))
}

fn is_stack_trace(text: &str) -> bool {
    let mut markers = 0usize;
    for line in text.lines().take(200) {
        let l = line.trim_start();
        if l.starts_with("at ")
            || l.starts_with("File \"")
            || l.starts_with("Traceback (most recent call last)")
            || l.starts_with("Caused by:")
            || (l.starts_with("thread '") && l.contains("panicked"))
            || l.starts_with("stack backtrace:")
        {
            markers += 1;
        }
    }
    markers >= 2
}

fn is_log(text: &str) -> bool {
    let mut hits = 0usize;
    let mut total = 0usize;
    for line in text.lines().take(200) {
        if line.trim().is_empty() {
            continue;
        }
        total += 1;
        if crate::invariants::LOG_LEVEL_RE.is_match(line) || starts_with_timestamp(line) {
            hits += 1;
        }
    }
    total >= 3 && hits * 2 >= total
}

/// `2024-01-02T03:04:05`, `2024-01-02 03:04:05`, `[03:04:05]`, `Jan 02 03:04:05`.
fn starts_with_timestamp(line: &str) -> bool {
    let l = line.trim_start().trim_start_matches(['[', '(']);
    let b = l.as_bytes();
    if b.len() >= 10
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && (b[4] == b'-' || b[4] == b'/')
    {
        return true;
    }
    // bare clock time
    b.len() >= 8
        && b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2] == b':'
        && b[3].is_ascii_digit()
        && b[4].is_ascii_digit()
        && b[5] == b':'
}

fn is_delimited(text: &str) -> Option<Detection> {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(50)
        .collect();
    if lines.len() < 2 {
        return None;
    }
    for (delim, ct, why) in [
        (',', ContentType::Csv, "consistent comma columns"),
        ('\t', ContentType::TabularData, "consistent tab columns"),
    ] {
        let first = lines[0].matches(delim).count();
        if first == 0 {
            continue;
        }
        if lines.iter().all(|l| l.matches(delim).count() == first) {
            return Some(Detection::new(ct, Confidence::Medium, why));
        }
    }
    // Markdown pipe table
    if lines.len() >= 2
        && lines[0].starts_with('|')
        && lines[1].chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
    {
        return Some(Detection::new(
            ContentType::TabularData,
            Confidence::High,
            "markdown table header separator",
        ));
    }
    None
}

fn is_markdown(text: &str) -> bool {
    let mut score = 0usize;
    for line in text.lines().take(200) {
        if line.starts_with("# ")
            || line.starts_with("## ")
            || line.starts_with("### ")
            || line.starts_with("```")
        {
            score += 2;
        } else if line.starts_with("- ") || line.starts_with("* ") {
            score += 1;
        }
    }
    score >= 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json_only_when_it_parses() {
        assert_eq!(detect(r#"{"a":1}"#, None).content_type, ContentType::Json);
        assert_eq!(detect("{not json", None).content_type, ContentType::Unknown);
    }

    #[test]
    fn extension_hint_wins() {
        let d = detect("anything at all", Some("main.rs"));
        assert_eq!(d.content_type, ContentType::SourceCode);
        assert_eq!(d.confidence, Confidence::High);
    }

    #[test]
    fn detects_stack_trace_and_log() {
        let trace =
            "Traceback (most recent call last)\n  File \"a.py\", line 3\n  File \"b.py\", line 9\n";
        assert_eq!(detect(trace, None).content_type, ContentType::StackTrace);

        let log = "2024-01-02T03:04:05Z INFO started\n2024-01-02T03:04:06Z WARN slow\n2024-01-02T03:04:07Z ERROR boom\n";
        assert_eq!(detect(log, None).content_type, ContentType::ApplicationLog);
    }

    #[test]
    fn detects_csv() {
        let csv = "a,b,c\n1,2,3\n4,5,6\n";
        assert_eq!(detect(csv, None).content_type, ContentType::Csv);
    }

    #[test]
    fn huge_input_is_bounded() {
        let big = "x".repeat(MAX_SNIFF_BYTES * 4);
        let d = detect(&big, None);
        assert_eq!(d.content_type, ContentType::PlainText);
    }
}
