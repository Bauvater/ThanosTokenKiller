//! Putting `ttk` on the user's `PATH`.
//!
//! # Why this does not use `setx`
//!
//! `setx` truncates any value longer than **1024 characters**. A developer
//! machine routinely has a `PATH` well past that, so `setx PATH "%PATH%;…"`
//! silently destroys entries — and the damage is only noticed later, when some
//! unrelated tool stops resolving. That is unacceptable for an installer, so
//! this module never shells out to `setx` (nor to `reg add`, whose command line
//! length limit has the same failure mode for long values).
//!
//! Instead:
//!
//! * **Windows** — the user `PATH` is read from and written back to
//!   `HKCU\Environment` through the registry API, with
//!   `DoNotExpandEnvironmentNames` so entries like `%USERPROFILE%\bin` stay
//!   symbolic, and with the value's original kind (`REG_EXPAND_SZ` /
//!   `REG_SZ`) preserved. There is no length limit on that path.
//! * **Unix** — a single marked block is appended to the shell profile, in the
//!   same idempotent way `ttk install` maintains agent instruction files.
//!
//! Both are additive: an existing `PATH` is never rewritten, only extended.

use std::path::{Path, PathBuf};

use ttk_core::{Error, Result};

/// Set to `1` to make `ttk install` skip the PATH step entirely. The test
/// suite sets it so it can never touch the developer's real environment.
pub const SKIP_ENV_VAR: &str = "TTK_NO_PATH_SETUP";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathOutcome {
    /// The directory was already on the user's PATH.
    AlreadyPresent,
    /// Added; `detail` names where (registry key or profile file).
    Added { detail: String },
    /// Not attempted, with a reason.
    Skipped { reason: String },
}

impl PathOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            PathOutcome::AlreadyPresent => "already on PATH",
            PathOutcome::Added { .. } => "added to PATH",
            PathOutcome::Skipped { .. } => "skipped",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            PathOutcome::AlreadyPresent => "",
            PathOutcome::Added { detail } => detail,
            PathOutcome::Skipped { reason } => reason,
        }
    }

    /// True when a new shell is needed before `ttk` resolves everywhere.
    pub fn needs_new_shell(&self) -> bool {
        matches!(self, PathOutcome::Added { .. })
    }
}

/// Directory holding the running `ttk` binary.
pub fn binary_dir() -> Result<PathBuf> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::other(format!("cannot locate the ttk binary: {e}")))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| Error::other("the ttk binary has no parent directory"))
}

/// Is `dir` already an entry of `path_value`?
///
/// Comparison ignores a trailing separator, and on Windows also case.
pub fn contains_dir(path_value: &str, dir: &Path) -> bool {
    let separator = if cfg!(windows) { ';' } else { ':' };
    let wanted = normalise(&dir.display().to_string());
    path_value
        .split(separator)
        .filter(|e| !e.trim().is_empty())
        .any(|entry| normalise(entry) == wanted)
}

fn normalise(entry: &str) -> String {
    let trimmed = entry.trim().trim_end_matches(['/', '\\']);
    if cfg!(windows) {
        trimmed.to_ascii_lowercase().replace('/', "\\")
    } else {
        trimmed.to_string()
    }
}

/// Add `dir` to the user's PATH, unless it is already there.
pub fn ensure_on_path(dir: &Path, dry_run: bool) -> Result<PathOutcome> {
    if std::env::var(SKIP_ENV_VAR).is_ok_and(|v| v.trim() == "1") {
        return Ok(PathOutcome::Skipped {
            reason: format!("{SKIP_ENV_VAR}=1"),
        });
    }
    // The process PATH is the cheapest check and covers the common case.
    if let Ok(current) = std::env::var("PATH")
        && contains_dir(&current, dir)
    {
        return Ok(PathOutcome::AlreadyPresent);
    }
    if dry_run {
        return Ok(PathOutcome::Added {
            detail: format!("{} (dry run)", persistence_location()),
        });
    }
    platform::add(dir)
}

/// Where a directory stands relative to the user's PATH.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPresence {
    /// Visible to this process — everything works.
    Active,
    /// Stored persistently, but this shell still has the old environment.
    PendingNewShell,
    /// Not on PATH at all.
    Absent,
    /// The persistent store could not be read.
    Unknown,
}

/// Distinguish "not installed" from "installed, but this shell predates it".
///
/// That difference is the single most confusing part of a PATH change on
/// Windows, so it gets a first class answer instead of a plain yes/no.
pub fn presence(dir: &Path) -> PathPresence {
    if std::env::var("PATH").is_ok_and(|p| contains_dir(&p, dir)) {
        return PathPresence::Active;
    }
    match platform::persisted_contains(dir) {
        Some(true) => PathPresence::PendingNewShell,
        Some(false) => PathPresence::Absent,
        None => PathPresence::Unknown,
    }
}

fn persistence_location() -> String {
    if cfg!(windows) {
        "HKCU\\Environment".to_string()
    } else {
        platform::profile_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "your shell profile".to_string())
    }
}

// ---------------------------------------------------------------------------
// Windows
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod platform {
    use super::*;
    use std::process::Command;

    /// PowerShell that edits `HKCU\Environment` through the registry API.
    ///
    /// Deliberately **not** `setx` (1024 character truncation) and not
    /// `[Environment]::GetEnvironmentVariable(..,'User')` either, because that
    /// expands `%VAR%` references and would bake them into the stored value.
    pub(super) fn script() -> &'static str {
        r#"param(
    [Parameter(Mandatory=$true)][string]$Dir,
    # Only the test suite overrides this; production always edits Environment.
    [string]$KeyPath = 'Environment'
)
$ErrorActionPreference = 'Stop'

$key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($KeyPath, $true)
if ($null -eq $key) { $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($KeyPath) }

# DoNotExpandEnvironmentNames keeps entries such as %USERPROFILE%\bin symbolic.
$raw = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
$kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
if ($key.GetValueNames() -contains 'Path') {
    $existing = $key.GetValueKind('Path')
    if ($existing -eq [Microsoft.Win32.RegistryValueKind]::String) {
        $kind = [Microsoft.Win32.RegistryValueKind]::String
    }
}

$parts = @()
if ($raw) { $parts = @($raw -split ';' | Where-Object { $_.Trim() -ne '' }) }

$target = $Dir.TrimEnd('\')
foreach ($p in $parts) {
    if ($p.Trim().TrimEnd('\') -ieq $target) { Write-Output 'already-present'; exit 0 }
}

$new = (($parts + $Dir) -join ';')
$key.SetValue('Path', $new, $kind)
$key.Close()
Write-Output 'added'
"#
    }

    pub(super) fn profile_path() -> Option<PathBuf> {
        None
    }

    /// Read the persisted user PATH from the registry.
    ///
    /// Only called when the process PATH does not already contain the
    /// directory, so the extra process spawn stays off the hot path.
    pub(super) fn persisted_contains(dir: &Path) -> Option<bool> {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment'); \
                 if ($null -eq $k) { '' } else { \
                 $k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) }",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&output.stdout);
        Some(super::contains_dir(value.trim(), dir))
    }

    // `HWND_BROADCAST`, `WM_SETTINGCHANGE`, `SMTO_ABORTIFHUNG`.
    const HWND_BROADCAST: isize = 0xffff;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageTimeoutW(
            hwnd: isize,
            msg: u32,
            wparam: usize,
            lparam: *const u16,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
    }

    /// Tell running applications that the environment changed.
    ///
    /// Writing the registry is only half the job: Explorer caches its
    /// environment block and hands that copy to every process it starts, so
    /// without this broadcast a *newly opened* terminal still shows the old
    /// PATH until the next logon. (`setx` does this too — it is the one thing
    /// it gets right.)
    ///
    /// Already-running shells never pick it up; that is a Windows property, not
    /// something a broadcast can fix.
    pub(super) fn broadcast_environment_change() -> bool {
        // UTF-16, NUL terminated: the message names which settings changed.
        let param: Vec<u16> = "Environment"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut result: usize = 0;
        // SAFETY: `param` outlives the call and is a valid NUL terminated
        // UTF-16 buffer; `result` is a valid out pointer. SMTO_ABORTIFHUNG plus
        // the timeout bound the call even when some window is not pumping
        // messages.
        let rc = unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                param.as_ptr(),
                SMTO_ABORTIFHUNG,
                5_000,
                &mut result,
            )
        };
        rc != 0
    }

    pub(super) fn add(dir: &Path) -> Result<PathOutcome> {
        let outcome = run_script(dir, None)?;
        if matches!(outcome, PathOutcome::Added { .. }) {
            // Best effort: a failed broadcast only costs the user a re-login,
            // so it must not turn a successful PATH write into an error.
            let delivered = broadcast_environment_change();
            return Ok(PathOutcome::Added {
                detail: if delivered {
                    "HKCU\\Environment — open a new terminal to use it".to_string()
                } else {
                    "HKCU\\Environment — log out and back in to use it".to_string()
                },
            });
        }
        Ok(outcome)
    }

    /// Run the registry script. `key_path` is `None` in production, meaning
    /// `HKCU\Environment`; the test suite passes a scratch key so the write
    /// path can be exercised without touching the real PATH.
    pub(super) fn run_script(dir: &Path, key_path: Option<&str>) -> Result<PathOutcome> {
        let script_path = std::env::temp_dir().join(format!(
            "ttk-path-{}-{}.ps1",
            std::process::id(),
            ttk_core::ids::now_millis()
        ));
        std::fs::write(&script_path, script())?;

        let mut command = Command::new("powershell");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ])
            .arg(&script_path)
            .arg("-Dir")
            .arg(dir);
        if let Some(key) = key_path {
            command.arg("-KeyPath").arg(key);
        }
        let output = command.output();
        let _ = std::fs::remove_file(&script_path);

        let output = output.map_err(|e| {
            Error::other(format!(
                "cannot run powershell to update the user PATH: {e}. \
                 Add `{}` to your PATH manually.",
                dir.display()
            ))
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::other(format!(
                "updating the user PATH failed: {}. Nothing was changed; add `{}` manually.",
                stderr.trim(),
                dir.display()
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.contains("already-present") {
            Ok(PathOutcome::AlreadyPresent)
        } else {
            // `add` replaces this once it knows whether the change could be
            // broadcast to running applications.
            Ok(PathOutcome::Added {
                detail: "HKCU\\Environment".to_string(),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Unix
// ---------------------------------------------------------------------------

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) const BEGIN: &str = "# BEGIN ThanosTokenKiller (ttk) — managed block";
    pub(super) const END: &str = "# END ThanosTokenKiller (ttk)";

    /// Shell profile to extend, chosen from `$SHELL`.
    pub(super) fn profile_path() -> Option<PathBuf> {
        let home = dirs::home_dir()?;
        let shell = std::env::var("SHELL").unwrap_or_default();
        Some(if shell.ends_with("zsh") {
            home.join(".zshrc")
        } else if shell.ends_with("fish") {
            home.join(".config").join("fish").join("config.fish")
        } else if shell.ends_with("bash") {
            home.join(".bashrc")
        } else {
            home.join(".profile")
        })
    }

    /// Does the shell profile already carry the directory?
    pub(super) fn persisted_contains(dir: &Path) -> Option<bool> {
        let profile = profile_path()?;
        let text = std::fs::read_to_string(&profile).ok()?;
        Some(text.contains(&block_for(&profile, dir)))
    }

    pub(super) fn block_for(profile: &Path, dir: &Path) -> String {
        let dir = dir.display();
        if profile.extension().is_some_and(|e| e == "fish") {
            format!("fish_add_path {dir}")
        } else {
            format!("export PATH=\"$PATH:{dir}\"")
        }
    }

    pub(super) fn add(dir: &Path) -> Result<PathOutcome> {
        let profile = profile_path().ok_or_else(|| {
            Error::other(format!(
                "no home directory found; add `{}` to your PATH manually",
                dir.display()
            ))
        })?;

        let existing = if profile.is_file() {
            std::fs::read_to_string(&profile)?
        } else {
            String::new()
        };
        let body = block_for(&profile, dir);
        if existing.contains(&body) {
            return Ok(PathOutcome::AlreadyPresent);
        }

        let updated = crate::install::upsert(&existing, &body, BEGIN, END);
        if let Some(parent) = profile.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = profile.with_extension("ttk-tmp");
        std::fs::write(&tmp, updated.as_bytes())?;
        if std::fs::rename(&tmp, &profile).is_err() {
            std::fs::write(&profile, updated.as_bytes())?;
            let _ = std::fs::remove_file(&tmp);
        }

        Ok(PathOutcome::Added {
            detail: format!(
                "{} (run `source {}` or open a new shell)",
                profile.display(),
                profile.display()
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_an_entry_that_is_already_present() {
        let sep = if cfg!(windows) { ';' } else { ':' };
        let dir = PathBuf::from(if cfg!(windows) {
            r"C:\tools\ttk"
        } else {
            "/opt/ttk"
        });
        let value =
            ["/usr/bin", &dir.display().to_string(), "/usr/local/bin"].join(&sep.to_string());
        assert!(contains_dir(&value, &dir));
        assert!(!contains_dir("/usr/bin", &dir));
    }

    #[test]
    fn ignores_a_trailing_separator() {
        let dir = PathBuf::from(if cfg!(windows) {
            r"C:\tools\ttk"
        } else {
            "/opt/ttk"
        });
        let with_slash = format!(
            "{}{}",
            dir.display(),
            if cfg!(windows) { "\\" } else { "/" }
        );
        assert!(contains_dir(&with_slash, &dir));
    }

    #[test]
    fn empty_entries_never_match() {
        let dir = PathBuf::from("/opt/ttk");
        assert!(!contains_dir(";;;", &dir));
        assert!(!contains_dir("", &dir));
    }

    #[cfg(windows)]
    #[test]
    fn windows_matching_is_case_insensitive() {
        let dir = PathBuf::from(r"C:\Tools\TTK");
        assert!(contains_dir(r"c:\tools\ttk", &dir));
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_script_never_uses_setx() {
        let s = platform::script();
        assert!(
            !s.to_ascii_lowercase().contains("setx"),
            "setx truncates PATH at 1024 characters and must never be used"
        );
        assert!(!s.to_ascii_lowercase().contains("reg add"));
        // The two properties that keep an existing PATH intact.
        assert!(s.contains("DoNotExpandEnvironmentNames"));
        assert!(s.contains("RegistryValueKind"));
        // Additive only: the existing parts are always carried over.
        assert!(s.contains("$parts + $Dir"));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_block_matches_the_shell() {
        assert!(
            platform::block_for(Path::new("/home/u/.bashrc"), Path::new("/opt/ttk"))
                .starts_with("export PATH=")
        );
        assert!(
            platform::block_for(
                Path::new("/home/u/.config/fish/config.fish"),
                Path::new("/opt/ttk")
            )
            .starts_with("fish_add_path")
        );
    }

    #[test]
    fn the_skip_switch_is_honoured() {
        // SAFETY: single-threaded test scope, restored immediately.
        unsafe { std::env::set_var(SKIP_ENV_VAR, "1") };
        let outcome = ensure_on_path(Path::new("/definitely/not/on/path"), false).expect("skip");
        unsafe { std::env::remove_var(SKIP_ENV_VAR) };
        assert!(matches!(outcome, PathOutcome::Skipped { .. }));
        assert!(!outcome.needs_new_shell());
    }

    #[test]
    fn a_directory_already_on_path_is_not_touched() {
        let dir = std::env::var("PATH")
            .ok()
            .and_then(|p| {
                let sep = if cfg!(windows) { ';' } else { ':' };
                p.split(sep)
                    .find(|e| !e.trim().is_empty())
                    .map(PathBuf::from)
            })
            .expect("PATH has at least one entry");
        assert_eq!(
            ensure_on_path(&dir, false).expect("check"),
            PathOutcome::AlreadyPresent
        );
    }

    /// The step that was missing: without it a *newly opened* terminal keeps
    /// the stale PATH that Explorer handed it, which looks exactly like "the
    /// entry only exists for the admin account".
    ///
    /// Broadcasting changes no setting — it asks running applications to
    /// re-read the ones they already have — so it is safe to run in the suite.
    #[cfg(windows)]
    #[test]
    fn environment_change_is_broadcast_without_hanging() {
        let start = std::time::Instant::now();
        let delivered = platform::broadcast_environment_change();
        // SMTO_ABORTIFHUNG plus the 5s timeout must bound this call even if
        // some window on the desktop is not pumping messages.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "broadcast blocked for {:?}",
            start.elapsed()
        );
        assert!(delivered, "SendMessageTimeoutW reported failure");
    }

    #[test]
    fn binary_dir_exists() {
        let dir = binary_dir().expect("binary dir");
        assert!(dir.is_dir(), "{}", dir.display());
    }

    /// Exercises the real registry write against a scratch key.
    ///
    /// Ignored by default because it creates (and removes) a key under
    /// `HKCU\Software\ThanosTokenKiller`. It never touches `HKCU\Environment`.
    /// Run with: `cargo test -p ttk-cli -- --ignored write_path`
    #[cfg(windows)]
    #[test]
    #[ignore = "writes to a scratch registry key; run explicitly"]
    fn write_path_extends_without_destroying_existing_entries() {
        use std::process::Command;
        // A single key with no parent of its own, so cleanup leaves nothing.
        const KEY: &str = r"Software\ThanosTokenKiller-PathTest";

        fn ps(script: &str) -> String {
            let out = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", script])
                .output()
                .expect("powershell");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        // A seed value that is longer than setx's 1024 character limit and
        // contains an unexpanded %VAR% — the two things that must survive.
        let long_entry: String = (0..40)
            .map(|i| format!(r"C:\some\reasonably\long\directory\number-{i:02}"))
            .collect::<Vec<_>>()
            .join(";");
        let seed = format!(r"%USERPROFILE%\bin;{long_entry}");
        assert!(seed.len() > 1024, "seed must exceed the setx limit");

        let cleanup = format!(
            r"Remove-Item -Path 'HKCU:\{KEY}' -Recurse -Force -ErrorAction SilentlyContinue"
        );
        ps(&cleanup);
        ps(&format!(
            r"New-Item -Path 'HKCU:\{KEY}' -Force | Out-Null; \
              $k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('{KEY}', $true); \
              $k.SetValue('Path', '{seed}', [Microsoft.Win32.RegistryValueKind]::ExpandString); $k.Close()"
        ));

        let new_dir = Path::new(r"C:\tools\ttk-write-path-test");
        let first = platform::run_script(new_dir, Some(KEY)).expect("first run");
        assert!(matches!(first, PathOutcome::Added { .. }), "{first:?}");

        let read = format!(
            r"$k=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('{KEY}'); \
              $v=$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames); \
              Write-Output $v; Write-Output $k.GetValueKind('Path')"
        );
        let after = ps(&read);
        let (value, kind) = after.rsplit_once('\n').expect("value and kind");

        // 1. Nothing was lost — the whole seed is still there, verbatim.
        assert!(value.contains(&seed), "existing PATH was not preserved");
        // 2. The %VAR% reference was not expanded.
        assert!(value.contains(r"%USERPROFILE%\bin"), "%VAR% got expanded");
        // 3. The new directory was appended.
        assert!(value.trim_end().ends_with(r"C:\tools\ttk-write-path-test"));
        // 4. The value kind was preserved.
        assert_eq!(kind.trim(), "ExpandString");
        // 5. Longer than setx could ever have written.
        assert!(value.len() > 1024);

        // 6. Running it again is a no-op.
        let second = platform::run_script(new_dir, Some(KEY)).expect("second run");
        assert_eq!(second, PathOutcome::AlreadyPresent);
        let unchanged = ps(&read);
        assert_eq!(after, unchanged, "second run must not modify anything");

        ps(&cleanup);
        assert!(
            ps(&format!(r"Test-Path -Path 'HKCU:\{KEY}'")) == "False",
            "scratch key must be removed again"
        );
    }
}
