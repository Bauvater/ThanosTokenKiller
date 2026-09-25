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

/// Take `dir` back out of the user's PATH. `true` when it was there.
///
/// The uninstaller's half of [`ensure_on_path`]: every other entry is left
/// exactly as it was.
pub fn remove_from_path(dir: &Path) -> Result<bool> {
    if std::env::var(SKIP_ENV_VAR).is_ok_and(|v| v.trim() == "1") {
        return Ok(false);
    }
    platform::remove(dir)
}

/// The file name of the ttk binary on this platform.
pub const BINARY_NAME: &str = if cfg!(windows) { "ttk.exe" } else { "ttk" };

/// The `ttk` a new terminal would start: the first match on the process PATH.
pub fn first_on_path() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(BINARY_NAME))
        .find(|p| p.is_file())
}

/// Do two paths name the same file?
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => normalise(&a.display().to_string()) == normalise(&b.display().to_string()),
    }
}

/// What [`take_over`] did, one line per action, for the installer to show.
#[derive(Debug, Default)]
pub struct Takeover {
    /// Things that were changed.
    pub done: Vec<String>,
    /// Things that could not be fixed without the user.
    pub warnings: Vec<String>,
}

/// Make `dir` the one and only `ttk` on the user's PATH.
///
/// An upgrade that leaves an older copy earlier on the PATH has not upgraded
/// anything: the terminal keeps starting the old binary. So every other
/// directory on the user PATH that holds a `ttk` binary is dealt with:
///
/// * a directory that exists for ttk alone (its path names ttk, such as an old
///   install directory or a `target\release` of this repository) is taken off
///   the PATH;
/// * a shared directory (`~/.cargo/bin`, …) keeps its PATH entry, and only the
///   stale binary in it is removed.
///
/// Then `dir` goes to the *front* of the user PATH, and the PATH a new
/// terminal will get is checked to really start this copy. A copy on the
/// system PATH needs administrator rights to change, so it is reported, never
/// touched.
pub fn take_over(dir: &Path) -> Result<Takeover> {
    let mut report = Takeover::default();
    if std::env::var(SKIP_ENV_VAR).is_ok_and(|v| v.trim() == "1") {
        report
            .warnings
            .push(format!("{SKIP_ENV_VAR}=1: PATH left alone"));
        return Ok(report);
    }
    let user_path = platform::persisted_user_path().unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    let user_entries: Vec<&str> = user_path
        .split(separator)
        .filter(|e| !e.trim().is_empty())
        .collect();

    for entry in &user_entries {
        let expanded = PathBuf::from(expand_vars(entry.trim()));
        if same_dir(&expanded, dir) {
            continue;
        }
        let stale = expanded.join(BINARY_NAME);
        if !stale.is_file() {
            continue;
        }
        if owned_by_ttk(&expanded) {
            match platform::remove_entry(entry.trim()) {
                Ok(true) => report
                    .done
                    .push(format!("removed old PATH entry {}", expanded.display())),
                Ok(false) => {}
                Err(e) => report.warnings.push(format!(
                    "could not remove {} from PATH: {e}",
                    expanded.display()
                )),
            }
        } else {
            match retire_binary(&stale) {
                Ok(()) => report
                    .done
                    .push(format!("removed old copy {}", stale.display())),
                Err(e) => report.warnings.push(format!(
                    "an old copy at {} is still in the way: {e}",
                    stale.display()
                )),
            }
        }
    }

    match platform::put_first(dir)? {
        true => report
            .done
            .push(format!("{} is first on your PATH", dir.display())),
        false => report
            .done
            .push(format!("{} was already first on your PATH", dir.display())),
    }

    // The proof: what a terminal opened from now on will actually start.
    if let Some(fresh) = platform::fresh_path() {
        let winner = fresh
            .split(separator)
            .filter(|e| !e.trim().is_empty())
            .map(|e| PathBuf::from(expand_vars(e.trim())).join(BINARY_NAME))
            .find(|p| p.is_file());
        match winner {
            Some(p) if same_file(&p, &dir.join(BINARY_NAME)) => {}
            // Only the system PATH can still be in front of the user's now,
            // and that one needs administrator rights to change.
            Some(p) => report.warnings.push(format!(
                "a new terminal would still start {} (system PATH); \
                 remove it there, which needs administrator rights",
                p.display()
            )),
            None => report
                .warnings
                .push("a new terminal will not find ttk on its PATH".to_string()),
        }
    }
    Ok(report)
}

/// Does this directory exist only for ttk? Judged by its path, which is all
/// that can be known without guessing.
fn owned_by_ttk(dir: &Path) -> bool {
    let lower = dir.display().to_string().to_ascii_lowercase();
    dir.file_name()
        .is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case("ttk"))
        || lower.contains("thanostokenkiller")
        || lower.contains(&format!(
            "{}ttk{}",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        ))
}

/// Remove an old binary. A running one cannot be deleted on Windows but can
/// be renamed, and the renamed file is no longer found as `ttk`.
fn retire_binary(path: &Path) -> std::io::Result<()> {
    if std::fs::remove_file(path).is_ok() {
        return Ok(());
    }
    let old = path.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(path, &old)
}

fn same_dir(a: &Path, b: &Path) -> bool {
    normalise(&a.display().to_string()) == normalise(&b.display().to_string())
}

/// Expand `%VAR%` references the way the Windows shell does; anything that
/// is not a set variable stays as written.
fn expand_vars(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name) {
            Ok(v) if !name.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
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
    [string]$KeyPath = 'Environment',
    # Take the directory out instead of adding it (the uninstaller).
    [switch]$Remove,
    # Make it the first entry, moving it if it is already further back.
    [switch]$Front
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
if ($Remove) {
    $kept = @($parts | Where-Object { $_.Trim().TrimEnd('\') -ine $target })
    if ($kept.Count -eq $parts.Count) { Write-Output 'absent'; exit 0 }
    $key.SetValue('Path', ($kept -join ';'), $kind)
    $key.Close()
    Write-Output 'removed'
    exit 0
}
if ($Front) {
    if ($parts.Count -gt 0 -and $parts[0].Trim().TrimEnd('\') -ieq $target) { Write-Output 'already-first'; exit 0 }
    $rest = @($parts | Where-Object { $_.Trim().TrimEnd('\') -ine $target })
    $key.SetValue('Path', ((@($Dir) + $rest) -join ';'), $kind)
    $key.Close()
    Write-Output 'moved-first'
    exit 0
}
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
        Some(super::contains_dir(&persisted_user_path()?, dir))
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
    /// Take `dir` out of the user PATH. `true` when it was there.
    pub(super) fn remove(dir: &Path) -> Result<bool> {
        let removed = remove_at(dir, None)?;
        if removed {
            broadcast_environment_change();
        }
        Ok(removed)
    }

    /// [`remove`] against `key_path` (`None` = `HKCU\Environment`).
    pub(super) fn remove_at(dir: &Path, key_path: Option<&str>) -> Result<bool> {
        Ok(invoke(dir, key_path, Mode::Remove)?.contains("removed"))
    }

    /// Remove one raw PATH entry, exactly as it is written in the registry.
    pub(super) fn remove_entry(entry: &str) -> Result<bool> {
        let removed = remove_at(Path::new(entry), None)?;
        if removed {
            broadcast_environment_change();
        }
        Ok(removed)
    }

    /// Put `dir` first on the user PATH. `true` when something changed.
    pub(super) fn put_first(dir: &Path) -> Result<bool> {
        let changed = put_first_at(dir, None)?;
        if changed {
            broadcast_environment_change();
        }
        Ok(changed)
    }

    pub(super) fn put_first_at(dir: &Path, key_path: Option<&str>) -> Result<bool> {
        Ok(invoke(dir, key_path, Mode::Front)?.contains("moved-first"))
    }

    /// The PATH a terminal opened from now on gets: system entries first,
    /// then the user's, expanded.
    pub(super) fn fresh_path() -> Option<String> {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "[Environment]::GetEnvironmentVariable('Path','Machine') + ';' + \
                 [Environment]::GetEnvironmentVariable('Path','User')",
            ])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// The user PATH as stored, with `%VAR%` references unexpanded.
    pub(super) fn persisted_user_path() -> Option<String> {
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
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Mode {
        Add,
        Remove,
        Front,
    }

    pub(super) fn run_script(dir: &Path, key_path: Option<&str>) -> Result<PathOutcome> {
        let stdout = invoke(dir, key_path, Mode::Add)?;
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

    fn invoke(dir: &Path, key_path: Option<&str>, mode: Mode) -> Result<String> {
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
        match mode {
            Mode::Add => {}
            Mode::Remove => {
                command.arg("-Remove");
            }
            Mode::Front => {
                command.arg("-Front");
            }
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

        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

    /// The profile block is not a list of entries; nothing to read.
    pub(super) fn persisted_user_path() -> Option<String> {
        None
    }

    pub(super) fn fresh_path() -> Option<String> {
        None
    }

    pub(super) fn remove_entry(_entry: &str) -> Result<bool> {
        Ok(false)
    }

    /// A profile block appends; making it first is `add` on this platform.
    pub(super) fn put_first(dir: &Path) -> Result<bool> {
        Ok(matches!(add(dir)?, PathOutcome::Added { .. }))
    }

    /// Drop the managed block from the shell profile, if it names `dir`.
    pub(super) fn remove(dir: &Path) -> Result<bool> {
        let Some(profile) = profile_path() else {
            return Ok(false);
        };
        let Ok(text) = std::fs::read_to_string(&profile) else {
            return Ok(false);
        };
        let (Some(start), Some(end)) = (text.find(BEGIN), text.find(END)) else {
            return Ok(false);
        };
        if end < start || !text[start..end].contains(&block_for(&profile, dir)) {
            return Ok(false);
        }
        let mut stop = end + END.len();
        if text[stop..].starts_with('\n') {
            stop += 1;
        }
        let updated = format!("{}{}", &text[..start], &text[stop..]);
        std::fs::write(&profile, updated)?;
        Ok(true)
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
    fn variables_expand_like_the_shell_and_unknown_ones_survive() {
        // SAFETY: test-local variable name, removed right after.
        unsafe { std::env::set_var("TTK_TEST_EXPAND", r"C:\Users\me") };
        assert_eq!(expand_vars(r"%TTK_TEST_EXPAND%\bin"), r"C:\Users\me\bin");
        assert_eq!(
            expand_vars(r"%TTK_TEST_NOT_SET_ANYWHERE%\x"),
            r"%TTK_TEST_NOT_SET_ANYWHERE%\x"
        );
        assert_eq!(expand_vars("100% sure"), "100% sure");
        assert_eq!(expand_vars("plain"), "plain");
        unsafe { std::env::remove_var("TTK_TEST_EXPAND") };
    }

    #[cfg(windows)]
    #[test]
    fn only_directories_that_exist_for_ttk_are_taken_off_the_path() {
        assert!(owned_by_ttk(Path::new(
            r"C:\Users\me\Desktop\Projekte\ThanosTokenKiller\target\release"
        )));
        assert!(owned_by_ttk(Path::new(
            r"C:\Users\me\AppData\Local\Programs\ttk"
        )));
        assert!(owned_by_ttk(Path::new(r"C:\tools\ttk\bin")));
        assert!(!owned_by_ttk(Path::new(r"C:\Users\me\.cargo\bin")));
        assert!(!owned_by_ttk(Path::new(r"C:\tools\attk")));
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

        // 7. The uninstaller takes exactly that entry back out, and nothing else.
        assert!(platform::remove_at(new_dir, Some(KEY)).expect("remove"));
        let removed = ps(&read);
        let (value, kind) = removed.rsplit_once('\n').expect("value and kind");
        assert_eq!(value.trim(), seed, "only the added entry may go");
        assert_eq!(kind.trim(), "ExpandString");
        assert!(!platform::remove_at(new_dir, Some(KEY)).expect("remove again"));

        // 8. Taking over: the directory goes to the front, exactly once.
        assert!(platform::put_first_at(new_dir, Some(KEY)).expect("front"));
        let (value, _) = ps(&read)
            .rsplit_once('\n')
            .map(|(v, k)| (v.to_string(), k.to_string()))
            .expect("value");
        assert_eq!(
            value.trim(),
            format!(r"C:\tools\ttk-write-path-test;{seed}")
        );
        assert!(!platform::put_first_at(new_dir, Some(KEY)).expect("front again"));
        assert!(
            platform::put_first_at(
                Path::new(r"C:\some\reasonably\long\directory\number-07"),
                Some(KEY)
            )
            .expect("move")
        );
        let (value, _) = ps(&read)
            .rsplit_once('\n')
            .map(|(v, k)| (v.to_string(), k.to_string()))
            .expect("value");
        assert!(value.starts_with(r"C:\some\reasonably\long\directory\number-07;C:\tools\ttk-write-path-test;%USERPROFILE%\bin"));
        assert_eq!(
            value.matches("number-07").count(),
            1,
            "moved, not duplicated"
        );

        ps(&cleanup);
        assert!(
            ps(&format!(r"Test-Path -Path 'HKCU:\{KEY}'")) == "False",
            "scratch key must be removed again"
        );
    }
}
