//! Process runner.
//!
//! Runs a child process, captures stdout and stderr concurrently (two reader
//! threads — a single-threaded read would deadlock as soon as the child fills
//! the other pipe), and records everything the shell compiler needs: argv,
//! working directory, exit code, signal, wall clock duration.
//!
//! Capture is bounded by `limits.max_capture_bytes`; beyond that the stream is
//! truncated and the truncation is reported, never silently swallowed.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Instant;

use ttk_compilers::CommandContext;
use ttk_core::{Error, Result};

pub struct Captured {
    pub context: CommandContext,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl Captured {
    /// Text handed to the compilers.
    ///
    /// stdout and stderr are captured separately, so their relative ordering
    /// cannot be reconstructed; stderr is appended after stdout. This is
    /// documented rather than guessed at.
    pub fn combined(&self) -> String {
        if self.stderr.trim().is_empty() {
            return self.stdout.clone();
        }
        if self.stdout.trim().is_empty() {
            return self.stderr.clone();
        }
        format!("{}\n{}", self.stdout.trim_end(), self.stderr)
    }

    pub fn truncated(&self) -> bool {
        self.stdout_truncated || self.stderr_truncated
    }
}

/// Run `argv`, capturing both streams.
///
/// When `echo` is true the raw streams are also forwarded to the terminal, so
/// a human can watch a long build while ThanosTokenKiller still compiles the
/// result.
pub fn run(argv: &[String], max_bytes: u64, echo: bool) -> Result<Captured> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| Error::other("no command given"))?;

    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();

    let started = Instant::now();
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::other(format!("cannot run `{program}`: {e}")))?;

    let out_pipe = child.stdout.take().expect("stdout piped");
    let err_pipe = child.stderr.take().expect("stderr piped");

    let out_handle = std::thread::spawn(move || drain(out_pipe, max_bytes, echo, false));
    let err_handle = std::thread::spawn(move || drain(err_pipe, max_bytes, echo, true));

    let status = child
        .wait()
        .map_err(|e| Error::other(format!("waiting for `{program}` failed: {e}")))?;

    let (stdout, stdout_truncated) = out_handle
        .join()
        .map_err(|_| Error::other("stdout reader thread panicked"))?;
    let (stderr, stderr_truncated) = err_handle
        .join()
        .map_err(|_| Error::other("stderr reader thread panicked"))?;

    let context = CommandContext {
        argv: argv.to_vec(),
        cwd,
        exit_code: status.code(),
        signal: signal_of(&status),
        duration_ms: started.elapsed().as_millis() as u64,
        stdout_bytes: stdout.len() as u64,
        stderr_bytes: stderr.len() as u64,
    };

    Ok(Captured {
        context,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

fn drain(mut pipe: impl Read, max_bytes: u64, echo: bool, is_err: bool) -> (String, bool) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    let mut truncated = false;
    loop {
        match pipe.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if echo {
                    use std::io::Write;
                    let slice = &chunk[..n];
                    if is_err {
                        let _ = std::io::stderr().write_all(slice);
                    } else {
                        let _ = std::io::stdout().write_all(slice);
                    }
                }
                if (buf.len() as u64) < max_bytes {
                    let room = (max_bytes - buf.len() as u64) as usize;
                    buf.extend_from_slice(&chunk[..n.min(room)]);
                    if n > room {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
        }
    }
    (String::from_utf8_lossy(&buf).into_owned(), truncated)
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &std::process::ExitStatus) -> Option<i32> {
    // Windows has no signals; a killed process reports an exit code.
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny cross platform command: the Rust test binary itself is not
    /// suitable, so use the platform shell.
    fn echo_cmd(text: &str) -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), format!("echo {text}")]
        } else {
            vec!["sh".into(), "-c".into(), format!("echo {text}")]
        }
    }

    fn fail_cmd() -> Vec<String> {
        if cfg!(windows) {
            vec!["cmd".into(), "/C".into(), "exit 3".into()]
        } else {
            vec!["sh".into(), "-c".into(), "exit 3".into()]
        }
    }

    #[test]
    fn captures_stdout_and_exit_code() {
        let c = run(&echo_cmd("hello"), 1 << 20, false).expect("run");
        assert!(c.stdout.contains("hello"), "{:?}", c.stdout);
        assert_eq!(c.context.exit_code, Some(0));
        assert!(c.context.succeeded());
        assert!(!c.truncated());
    }

    #[test]
    fn reports_non_zero_exit() {
        let c = run(&fail_cmd(), 1 << 20, false).expect("run");
        assert_eq!(c.context.exit_code, Some(3));
        assert!(!c.context.succeeded());
    }

    #[test]
    fn missing_program_is_an_error() {
        let argv = vec!["ttk-definitely-not-a-real-program".to_string()];
        assert!(run(&argv, 1 << 20, false).is_err());
    }

    #[test]
    fn truncates_at_the_limit() {
        let c = run(&echo_cmd("aaaaaaaaaaaaaaaaaaaa"), 4, false).expect("run");
        assert!(c.stdout_truncated);
        assert!(c.stdout.len() <= 4);
    }

    #[test]
    fn empty_argv_is_rejected() {
        assert!(run(&[], 1 << 20, false).is_err());
    }
}
