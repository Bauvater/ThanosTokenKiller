//! Specialized token compilers.
//!
//! A compiler turns one concrete output format into a Token IR document. Every
//! compiler is:
//!
//! * **deterministic** — same input, same output, always;
//! * **honest** — it declares the invariants it preserved, and the firewall
//!   verifies them;
//! * **optional** — if it does not recognise the input it declines, and the
//!   content is passed through untouched.
//!
//! Compilers never allocate a capsule themselves. The pipeline stores the
//! original first and hands the capsule reference down, so a compiler can
//! point at the full data it left out.

pub mod git;
pub mod json;
pub mod logs;
pub mod shell;
pub mod testing;

use ttk_core::config::Config;
use ttk_core::content::ContentType;
use ttk_core::event::EventSource;
use ttk_core::firewall::Candidate;

/// Everything a compiler is allowed to look at.
#[derive(Debug, Clone)]
pub struct CompileInput<'a> {
    pub content: &'a str,
    pub source: EventSource,
    pub content_type: ContentType,
    /// Present when the content is the output of a command we ran.
    pub command: Option<&'a CommandContext>,
    /// `cap://<id>` of the stored original, if the pipeline already wrote one.
    pub capsule_ref: Option<&'a str>,
    pub config: &'a Config,
}

impl<'a> CompileInput<'a> {
    pub fn new(content: &'a str, config: &'a Config) -> Self {
        Self {
            content,
            source: EventSource::ShellOutput,
            content_type: ContentType::Unknown,
            command: None,
            capsule_ref: None,
            config,
        }
    }

    pub fn source(mut self, s: EventSource) -> Self {
        self.source = s;
        self
    }

    pub fn content_type(mut self, t: ContentType) -> Self {
        self.content_type = t;
        self
    }

    pub fn command(mut self, c: &'a CommandContext) -> Self {
        self.command = Some(c);
        self
    }

    pub fn capsule(mut self, r: &'a str) -> Self {
        self.capsule_ref = Some(r);
        self
    }

    /// argv of the command that produced the content, if any.
    pub fn argv(&self) -> &[String] {
        self.command.map(|c| c.argv.as_slice()).unwrap_or(&[])
    }

    /// The program name, lowercased and stripped of a path and `.exe`.
    pub fn program(&self) -> Option<String> {
        let first = self.argv().first()?;
        let base = first
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or(first)
            .to_ascii_lowercase();
        Some(base.strip_suffix(".exe").unwrap_or(&base).to_string())
    }

    /// Sub command, e.g. `status` for `git status --short`.
    pub fn subcommand(&self) -> Option<&str> {
        self.argv()
            .iter()
            .skip(1)
            .find(|a| !a.starts_with('-'))
            .map(String::as_str)
    }
}

/// What we know about the command that produced some output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandContext {
    pub argv: Vec<String>,
    pub cwd: String,
    pub exit_code: Option<i32>,
    /// Unix signal that killed the process, if any.
    pub signal: Option<i32>,
    pub duration_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}

impl CommandContext {
    pub fn command_line(&self) -> String {
        self.argv
            .iter()
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.signal.is_none()
    }
}

pub trait Compiler: Send + Sync {
    /// Stable identifier, e.g. `test.pytest`.
    fn id(&self) -> &'static str;

    fn version(&self) -> u32;

    /// Cheap check whether this compiler is applicable at all.
    fn detect(&self, input: &CompileInput<'_>) -> bool;

    /// Produce a candidate, or `None` to decline.
    fn compile(&self, input: &CompileInput<'_>) -> Option<Candidate>;
}

/// Ordered list of compilers. The first one that both detects and produces a
/// candidate wins; ordering therefore goes from most specific to most generic.
pub fn registry() -> Vec<Box<dyn Compiler>> {
    vec![
        Box::new(testing::PytestCompiler),
        Box::new(testing::CargoTestCompiler),
        Box::new(testing::JestCompiler),
        Box::new(testing::GoTestCompiler),
        Box::new(git::GitStatusCompiler),
        Box::new(git::GitDiffCompiler),
        Box::new(git::GitLogCompiler),
        Box::new(json::JsonCompiler),
        Box::new(logs::LogCompiler),
        Box::new(shell::GrepCompiler),
        Box::new(shell::GenericShellCompiler),
    ]
}

/// Run the registry over `input`, returning the first candidate.
///
/// Each compiler runs inside [`ttk_core::firewall::guarded`], so a panicking
/// parser degrades to "no candidate" instead of taking the process down.
pub fn compile(input: &CompileInput<'_>) -> Option<Candidate> {
    for compiler in registry() {
        if !compiler.detect(input) {
            continue;
        }
        let id = compiler.id();
        let version = compiler.version();
        let result = ttk_core::firewall::guarded(id, version, input.content, || {
            compiler
                .compile(input)
                .unwrap_or_else(|| Candidate::new(id, version, input.content.to_string()))
        });
        match result {
            Ok(candidate) if candidate.output != input.content => return Some(candidate),
            // Declined or panicked: try the next compiler.
            _ => continue,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Bounded line iterator: never look at more than `max` lines.
pub(crate) fn lines_capped(text: &str, max: usize) -> impl Iterator<Item = &str> {
    text.lines().take(max)
}

/// Compact a duration for IR output.
pub(crate) fn fmt_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        format!("{}m{}s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Trim a single line so a pathological input cannot blow up the IR.
pub(crate) fn clip(line: &str, max_chars: usize) -> String {
    if line.chars().count() <= max_chars {
        return line.to_string();
    }
    let head: String = line.chars().take(max_chars).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config::default()
    }

    #[test]
    fn program_and_subcommand_are_normalised() {
        let ctx = CommandContext {
            argv: vec![
                r"C:\Program Files\Git\cmd\GIT.EXE".to_string(),
                "-c".to_string(),
                "color.ui=false".to_string(),
                "status".to_string(),
            ],
            ..Default::default()
        };
        let c = cfg();
        let input = CompileInput::new("", &c).command(&ctx);
        assert_eq!(input.program().as_deref(), Some("git"));
        // `color.ui=false` is the value of `-c`, but it is the first
        // non-flag token; documenting the known limitation via the test.
        assert_eq!(input.subcommand(), Some("color.ui=false"));
    }

    #[test]
    fn command_line_quotes_spaces() {
        let ctx = CommandContext {
            argv: vec!["git".into(), "commit".into(), "-m".into(), "a b".into()],
            ..Default::default()
        };
        assert_eq!(ctx.command_line(), r#"git commit -m "a b""#);
        assert!(!ctx.succeeded());
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(fmt_duration_ms(12), "12ms");
        assert_eq!(fmt_duration_ms(1500), "1.5s");
        assert_eq!(fmt_duration_ms(125_000), "2m5s");
    }

    #[test]
    fn clip_is_char_safe() {
        assert_eq!(clip("abc", 10), "abc");
        assert_eq!(clip("日本語テキスト", 3), "日本語…");
    }

    #[test]
    fn unrecognised_content_is_declined() {
        let c = cfg();
        let input = CompileInput::new("a", &c);
        assert!(compile(&input).is_none());
    }

    #[test]
    fn lines_capped_is_bounded() {
        let text = "x\n".repeat(10_000);
        assert_eq!(lines_capped(&text, 10).count(), 10);
    }
}
