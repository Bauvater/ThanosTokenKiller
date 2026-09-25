//! The installer proper. Windows only; see `main.rs` for the layout.

use std::fmt;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anstyle::{AnsiColor, Color, Style};

/// `ttk.exe`, embedded by `build.rs`. Empty when the installer was built
/// without `TTK_INSTALLER_PAYLOAD`; it then installs a `ttk.exe` found next to
/// itself instead.
static PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PRODUCT: &str = "ThanosTokenKiller";
const UNINSTALL_KEY: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\ThanosTokenKiller";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

const ACCENT: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::BrightMagenta)))
    .bold();
const FRAME: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta)));
const HEAD: Style = Style::new().bold();
const DIM: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlack)));
const KEY: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
const CODE: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightCyan)));
const OK: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::BrightGreen)))
    .bold();
const WARN: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
const BAD: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .bold();

fn paint<T: fmt::Display>(style: Style, value: T) -> String {
    format!("{style}{value}{style:#}")
}

macro_rules! say {
    () => {{ let _ = writeln!(anstream::stdout()); }};
    ($($t:tt)*) => {{ let _ = writeln!(anstream::stdout(), $($t)*); }};
}

const LOGO: [&str; 6] = [
    "████████╗████████╗██╗  ██╗",
    "╚══██╔══╝╚══██╔══╝██║ ██╔╝",
    "   ██║      ██║   █████╔╝ ",
    "   ██║      ██║   ██╔═██╗ ",
    "   ██║      ██║   ██║  ██╗",
    "   ╚═╝      ╚═╝   ╚═╝  ╚═╝",
];

fn masthead(subtitle: &str) {
    say!();
    for (i, line) in LOGO.iter().enumerate() {
        let side = match i {
            1 => format!("   {}", paint(HEAD, PRODUCT)),
            2 => format!("   {}", paint(DIM, "the token killer for coding agents")),
            4 => format!(
                "   {} {}",
                paint(ACCENT, subtitle),
                paint(DIM, format!("v{VERSION}"))
            ),
            _ => String::new(),
        };
        say!("   {}{side}", paint(ACCENT, line));
    }
    say!();
}

fn rule() {
    say!("   {}", paint(FRAME, "─".repeat(66)));
}

fn row(key: &str, value: impl fmt::Display) {
    say!(
        "   {}{}{value}",
        paint(KEY, key),
        " ".repeat(16usize.saturating_sub(key.chars().count()))
    );
}

fn step(n: usize, of: usize, title: &str) {
    let _ = write!(
        anstream::stdout(),
        "   {} {:<44}",
        paint(DIM, format!("[{n}/{of}]")),
        title
    );
    let _ = anstream::stdout().flush();
}

fn done(note: impl fmt::Display) {
    say!("{} {}", paint(OK, "✓"), paint(DIM, note));
}

fn skipped(note: impl fmt::Display) {
    say!("{} {}", paint(DIM, "–"), paint(DIM, note));
}

fn failed(note: impl fmt::Display) {
    say!("{} {}", paint(WARN, "!"), note);
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

struct Args {
    yes: bool,
    uninstall: bool,
    dir: Option<PathBuf>,
    path: bool,
    agent: Option<bool>,
}

fn parse_args() -> Result<Args, String> {
    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let mut args = Args {
        yes: false,
        uninstall: exe_name.contains("uninstall"),
        dir: None,
        path: true,
        agent: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.to_ascii_lowercase().as_str() {
            "-y" | "--yes" | "/s" | "/silent" | "/verysilent" => args.yes = true,
            "--uninstall" | "/uninstall" => args.uninstall = true,
            "--dir" | "/dir" => {
                args.dir = Some(PathBuf::from(it.next().ok_or("--dir needs a path")?));
            }
            "--no-path" => args.path = false,
            "--agent" => args.agent = Some(true),
            "--no-agent" => args.agent = Some(false),
            "-h" | "--help" | "/?" => {
                say!("ttk-setup {VERSION} — installs ThanosTokenKiller for the current user");
                say!();
                say!("  --yes         answer every question with its default");
                say!("  --dir <path>  install somewhere other than %LOCALAPPDATA%\\Programs\\ttk");
                say!("  --no-path     do not add ttk to the user PATH");
                say!("  --agent       also teach Claude Code to use ttk (global CLAUDE.md)");
                say!("  --no-agent    …or do not, without asking");
                say!("  --uninstall   remove ThanosTokenKiller again");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument `{other}` (try --help)")),
        }
    }
    Ok(args)
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn main() -> i32 {
    set_title(&format!("{PRODUCT} Setup"));
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            say!("{} {e}", paint(BAD, "✗"));
            return 2;
        }
    };
    let code = if args.uninstall {
        uninstall(&args)
    } else {
        install(&args)
    };
    pause_if_double_clicked(args.yes);
    code
}

fn install_dir(args: &Args) -> Option<PathBuf> {
    if let Some(d) = &args.dir {
        return Some(d.clone());
    }
    let local = std::env::var_os("LOCALAPPDATA")?;
    Some(PathBuf::from(local).join("Programs").join("ttk"))
}

fn global_home() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TTK_GLOBAL_HOME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    Some(PathBuf::from(std::env::var_os("APPDATA")?).join("ttk"))
}

fn start_menu_dir() -> Option<PathBuf> {
    Some(
        PathBuf::from(std::env::var_os("APPDATA")?)
            .join(r"Microsoft\Windows\Start Menu\Programs")
            .join(PRODUCT),
    )
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

fn install(args: &Args) -> i32 {
    masthead("setup");

    let Some(dir) = install_dir(args) else {
        say!(
            "{} %LOCALAPPDATA% is not set; pass --dir <path>",
            paint(BAD, "✗")
        );
        return 1;
    };
    let payload = match payload() {
        Ok(p) => p,
        Err(e) => {
            say!("   {} {e}", paint(BAD, "✗"));
            return 1;
        }
    };
    let home = global_home();

    say!(
        "   {}",
        paint(
            HEAD,
            "This installs ttk for you alone — no administrator rights needed."
        )
    );
    say!();
    row("program", paint(CODE, dir.join("ttk.exe").display()));
    if let Some(h) = &home {
        row("global folder", paint(CODE, h.display()));
        row(
            "",
            paint(DIM, "filters\\ for every project, and every token saved"),
        );
    }
    row(
        "PATH",
        if args.path {
            "goes first on your user PATH; older ttk copies are replaced".to_string()
        } else {
            paint(DIM, "left alone (--no-path)")
        },
    );
    row(
        "Start menu",
        format!("{PRODUCT} → savings, global folder, uninstall"),
    );
    say!();
    if !args.yes {
        if !confirm("Install ThanosTokenKiller?", true) {
            say!("   {}", paint(DIM, "Nothing was changed."));
            return 1;
        }
        say!();
    }
    rule();

    const STEPS: usize = 6;
    let ttk = dir.join("ttk.exe");

    // 1. the program
    step(1, STEPS, "installing ttk.exe");
    if let Err(e) = place_binary(&dir, &ttk, &payload) {
        failed(format!("cannot write {}: {e}", ttk.display()));
        return 1;
    }
    match Command::new(&ttk).arg("--version").output() {
        Ok(o) if o.status.success() => done(String::from_utf8_lossy(&o.stdout).trim()),
        _ => {
            failed("the installed ttk.exe does not start on this machine");
            return 1;
        }
    }

    // 2. the uninstaller
    step(2, STEPS, "adding the uninstaller");
    let uninstaller = dir.join("uninstall.exe");
    match std::env::current_exe().and_then(|me| {
        if same_file(&me, &uninstaller) {
            Ok(0)
        } else {
            std::fs::copy(&me, &uninstaller)
        }
    }) {
        Ok(_) => done("uninstall.exe"),
        Err(e) => failed(format!("could not copy the uninstaller: {e}")),
    }

    // 3. the global folder
    step(3, STEPS, "creating the global folder");
    match quiet(&ttk, &["global", "--init", "--json"]) {
        Ok(_) => done(
            home.as_ref()
                .map(|h| h.display().to_string())
                .unwrap_or_default(),
        ),
        Err(e) => failed(e),
    }

    // 4. PATH
    // Not "add to PATH": an older ttk earlier on the PATH would keep winning,
    // so this replaces every old copy and puts this one first.
    step(4, STEPS, "making this ttk the one on your PATH");
    let mut new_shell = false;
    if args.path {
        match quiet(&ttk, &["__installer", "take-over"]) {
            Ok(out) => {
                let warnings: Vec<&str> = out
                    .lines()
                    .filter_map(|l| l.strip_prefix("warn "))
                    .collect();
                let changes: Vec<&str> =
                    out.lines().filter_map(|l| l.strip_prefix("ok ")).collect();
                new_shell = changes.iter().any(|c| !c.contains("already first"));
                if warnings.is_empty() {
                    done("replaced every older copy");
                } else {
                    failed("done, with a warning");
                }
                for c in changes {
                    say!("         {} {}", paint(DIM, "└"), paint(DIM, c));
                }
                for w in warnings {
                    say!("         {} {}", paint(WARN, "!"), w);
                }
            }
            Err(e) => failed(e),
        }
    } else {
        skipped("skipped (--no-path)");
    }

    // 5. Start menu
    step(5, STEPS, "creating Start menu shortcuts");
    match start_menu_dir() {
        Some(menu) => match shortcuts(&menu, &ttk, &uninstaller, home.as_deref()) {
            Ok(()) => done(format!("Start → {PRODUCT}")),
            Err(e) => failed(e),
        },
        None => skipped("no Start menu for this user"),
    }

    // 6. Settings → Apps
    step(6, STEPS, "registering with Windows");
    match register(&dir, &ttk, &uninstaller) {
        Ok(()) => done("Settings → Apps → ThanosTokenKiller"),
        Err(e) => failed(e),
    }
    rule();

    // An upgrade brings every instruction block that is already installed up
    // to date, without asking: a stale block teaches the agent commands that
    // may no longer exist. Duplicates are removed on the way.
    say!();
    let refreshed = quiet(&ttk, &["__installer", "refresh-agents"]).unwrap_or_default();
    let refreshed: Vec<(&str, &str)> = refreshed
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .collect();
    for (action, path) in &refreshed {
        say!(
            "   {} {} {}",
            paint(OK, "✓"),
            format!("{action:<19}"),
            paint(DIM, path)
        );
    }
    let already_set_up = refreshed.iter().any(|(a, _)| *a != "duplicate removed");

    // A first install asks once; an upgrade of a set-up agent never does.
    let agent = match args.agent {
        Some(a) => a && !already_set_up,
        None if already_set_up || args.yes => false,
        None => {
            say!(
                "   {}",
                paint(HEAD, "Teach Claude Code to route its commands through ttk?")
            );
            say!(
                "   {}",
                paint(
                    DIM,
                    "Adds one marked block to %USERPROFILE%\\.claude\\CLAUDE.md; your own text stays."
                )
            );
            confirm("Set up Claude Code now?", true)
        }
    };
    if agent {
        match quiet(
            &ttk,
            &[
                "install",
                "--target",
                "claude",
                "--scope",
                "global",
                "--yes",
                "--no-path",
            ],
        ) {
            Ok(_) => say!(
                "   {} {}",
                paint(OK, "✓"),
                "Claude Code now uses ttk in every project"
            ),
            Err(e) => say!("   {} {e}", paint(WARN, "!")),
        }
    }

    // The card.
    say!();
    say!(
        "   {}  {}",
        paint(OK, "✓"),
        paint(HEAD, "ThanosTokenKiller is installed.")
    );
    say!();
    if new_shell {
        say!(
            "   {}",
            paint(
                WARN,
                "Open a NEW terminal window so it can find `ttk`, then try:"
            )
        );
    } else {
        say!("   {}", paint(DIM, "Try:"));
    }
    say!();
    for (cmd, what) in [
        ("ttk", "what it can do"),
        ("ttk run -- git status", "any command, compact output"),
        ("ttk gain", "how many tokens you have saved, in total"),
        ("ttk global --open", "the folder with your global filters"),
    ] {
        say!(
            "     {}{}{}",
            paint(CODE, cmd),
            " ".repeat(26usize.saturating_sub(cmd.len())),
            paint(DIM, what)
        );
    }
    say!();
    0
}

/// The bytes to install: the embedded payload, or a `ttk.exe` beside us.
fn payload() -> Result<Vec<u8>, String> {
    if !PAYLOAD.is_empty() {
        return Ok(PAYLOAD.to_vec());
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ttk.exe")));
    match beside {
        Some(p) if p.is_file() => {
            std::fs::read(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))
        }
        _ => Err(
            "this installer was built without ttk.exe inside it, and there is no ttk.exe \
                  next to it. Build it with scripts\\build-installer.ps1."
                .to_string(),
        ),
    }
}

/// Write `ttk.exe`, moving a running copy out of the way first.
///
/// A running executable cannot be overwritten on Windows but it can be
/// renamed, which is how an upgrade works while a terminal still uses it.
fn place_binary(dir: &Path, dest: &Path, bytes: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    if dest.exists() {
        let old = dir.join("ttk.exe.old");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(dest, &old);
    }
    let tmp = dir.join("ttk.exe.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, dest)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Run the installed `ttk` without a console of its own, returning stdout.
fn quiet(ttk: &Path, args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    let out = Command::new(ttk)
        .args(args)
        .arg("--color")
        .arg("never")
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", ttk.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.trim().trim_start_matches("ttk: ").to_string())
    }
}

/// Start menu shortcuts, through the Windows Script Host shell object: the
/// only way to write a `.lnk` without a COM binding of our own.
fn shortcuts(
    menu: &Path,
    ttk: &Path,
    uninstaller: &Path,
    home: Option<&Path>,
) -> Result<(), String> {
    std::fs::create_dir_all(menu).map_err(|e| e.to_string())?;
    let q = |p: &Path| p.display().to_string().replace('\'', "''");
    let plain = |p: &Path| p.display().to_string();
    let comspec =
        std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
    let mut script = String::from("$s = New-Object -ComObject WScript.Shell\n");
    let mut add = |name: &str, target: &str, arguments: &str, icon: &str| {
        script.push_str(&format!(
            "$l = $s.CreateShortcut('{}'); $l.TargetPath = '{}'; $l.Arguments = '{}'; \
             $l.IconLocation = '{}'; $l.WorkingDirectory = $env:USERPROFILE; $l.Save()\n",
            q(&menu.join(format!("{name}.lnk"))),
            target.replace('\'', "''"),
            arguments.replace('\'', "''"),
            icon.replace('\'', "''"),
        ));
    };
    add(
        "ttk - tokens saved",
        &comspec,
        &format!("/k \"\"{}\" gain\"", ttk.display()),
        &format!("{},0", ttk.display()),
    );
    add(
        "ttk - command prompt",
        &comspec,
        &format!("/k \"\"{}\"\"", ttk.display()),
        &format!("{},0", ttk.display()),
    );
    if let Some(h) = home {
        add(
            "ttk - global folder",
            &plain(h),
            "",
            r"%SystemRoot%\explorer.exe,0",
        );
    }
    add(
        "Uninstall ThanosTokenKiller",
        &plain(uninstaller),
        "",
        &format!("{},0", uninstaller.display()),
    );
    powershell(&script)
}

/// The *Settings → Apps* entry, per user.
fn register(dir: &Path, ttk: &Path, uninstaller: &Path) -> Result<(), String> {
    let size_kb = std::fs::metadata(ttk).map(|m| m.len() / 1024).unwrap_or(0)
        + std::fs::metadata(uninstaller)
            .map(|m| m.len() / 1024)
            .unwrap_or(0);
    let values: [(&str, &str, String); 11] = [
        ("DisplayName", "REG_SZ", PRODUCT.to_string()),
        ("DisplayVersion", "REG_SZ", VERSION.to_string()),
        ("Publisher", "REG_SZ", PRODUCT.to_string()),
        ("DisplayIcon", "REG_SZ", ttk.display().to_string()),
        ("InstallLocation", "REG_SZ", dir.display().to_string()),
        (
            "UninstallString",
            "REG_SZ",
            format!("\"{}\"", uninstaller.display()),
        ),
        (
            "QuietUninstallString",
            "REG_SZ",
            format!("\"{}\" --yes", uninstaller.display()),
        ),
        (
            "URLInfoAbout",
            "REG_SZ",
            "https://github.com/Bauvater/ThanosTokenKiller".to_string(),
        ),
        ("EstimatedSize", "REG_DWORD", size_kb.to_string()),
        ("NoModify", "REG_DWORD", "1".to_string()),
        ("NoRepair", "REG_DWORD", "1".to_string()),
    ];
    for (name, kind, data) in values {
        let status = Command::new("reg")
            .args([
                "add",
                UNINSTALL_KEY,
                "/v",
                name,
                "/t",
                kind,
                "/d",
                &data,
                "/f",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|e| format!("cannot run reg.exe: {e}"))?;
        if !status.success() {
            return Err(format!("could not write {name} to the registry"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Uninstall
// ---------------------------------------------------------------------------

fn uninstall(args: &Args) -> i32 {
    masthead("uninstall");

    let dir = if args.dir.is_some() {
        install_dir(args)
    } else {
        // An uninstaller removes the copy it sits next to.
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|d| d.join("ttk.exe").is_file())
            .or_else(|| install_dir(args))
    };
    let Some(dir) = dir else {
        say!(
            "{} cannot tell where ttk is installed; pass --dir <path>",
            paint(BAD, "✗")
        );
        return 1;
    };
    let ttk = dir.join("ttk.exe");
    let home = global_home();

    row("program", paint(CODE, dir.display()));
    if let Some(h) = &home {
        row(
            "global folder",
            format!(
                "{}  {}",
                paint(CODE, h.display()),
                paint(DIM, "(kept unless you say so)")
            ),
        );
    }
    say!();
    let mut purge = false;
    if !args.yes {
        if !confirm("Remove ThanosTokenKiller?", true) {
            say!("   {}", paint(DIM, "Nothing was changed."));
            return 1;
        }
        purge = home.as_ref().is_some_and(|h| h.is_dir())
            && confirm(
                "Also delete the global folder (global filters and your savings history)?",
                false,
            );
        say!();
    }
    rule();

    const STEPS: usize = 4;
    step(1, STEPS, "removing ttk from your PATH");
    if ttk.is_file() {
        match quiet(&ttk, &["__installer", "path-remove"]) {
            Ok(out) => done(out.trim()),
            Err(e) => failed(e),
        }
    } else {
        skipped("ttk.exe is already gone");
    }

    step(2, STEPS, "removing Start menu shortcuts");
    match start_menu_dir() {
        Some(menu) if menu.is_dir() => match std::fs::remove_dir_all(&menu) {
            Ok(()) => done("removed"),
            Err(e) => failed(e),
        },
        _ => skipped("none found"),
    }

    step(3, STEPS, "unregistering from Windows");
    let _ = Command::new("reg")
        .args(["delete", UNINSTALL_KEY, "/f"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    done("removed from Settings → Apps");

    step(4, STEPS, "deleting the program");
    for name in ["ttk.exe", "ttk.exe.old", "ttk.exe.tmp"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if dir.join("ttk.exe").exists() {
        failed("ttk.exe is still running somewhere; close it and delete the folder by hand");
    } else {
        done(dir.display());
    }

    if purge && let Some(h) = &home {
        match std::fs::remove_dir_all(h) {
            Ok(()) => say!("   {} global folder deleted", paint(OK, "✓")),
            Err(e) => say!(
                "   {} could not delete {}: {e}",
                paint(WARN, "!"),
                h.display()
            ),
        }
    }
    rule();
    say!();
    say!(
        "   {}  {}",
        paint(OK, "✓"),
        paint(HEAD, "ThanosTokenKiller has been removed.")
    );
    if !purge && home.as_ref().is_some_and(|h| h.is_dir()) {
        say!(
            "   {}",
            paint(
                DIM,
                "Your global filters and savings history are still there, for next time."
            )
        );
    }
    say!(
        "   {}",
        paint(
            DIM,
            "A ttk block in CLAUDE.md, if you added one, is yours to remove."
        )
    );
    say!();

    // The running uninstaller cannot delete itself; a detached shell does it
    // a moment after this process exits.
    if let Ok(me) = std::env::current_exe()
        && me.parent() == Some(dir.as_path())
    {
        schedule_self_delete(&me, &dir);
    }
    0
}

fn schedule_self_delete(me: &Path, dir: &Path) {
    use std::os::windows::process::CommandExt;
    let script = format!(
        "ping 127.0.0.1 -n 3 > nul & del /f /q \"{}\" & rmdir \"{}\"",
        me.display(),
        dir.display()
    );
    let _ = Command::new("cmd")
        .arg("/c")
        .raw_arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

// ---------------------------------------------------------------------------
// Small Windows helpers
// ---------------------------------------------------------------------------

/// Run a PowerShell script from a temporary file.
///
/// A file rather than `-Command -`: fed through stdin, PowerShell parses each
/// line on its own and mangles quoting, which is exactly what shortcut
/// arguments are full of. The UTF-8 BOM makes Windows PowerShell 5.1 read
/// non-ASCII paths correctly.
fn powershell(script: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let path = std::env::temp_dir().join(format!("ttk-setup-{}.ps1", std::process::id()));
    let mut body = vec![0xEF, 0xBB, 0xBF];
    body.extend_from_slice(format!("$ErrorActionPreference = 'Stop'\n{script}").as_bytes());
    std::fs::write(&path, body).map_err(|e| format!("cannot write a temporary script: {e}"))?;
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&path)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = std::fs::remove_file(&path);
    let out = out.map_err(|e| format!("cannot run powershell: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("powershell failed")
            .to_string())
    }
}

/// `[Y/n]` / `[y/N]`. Enter takes the default; a closed stdin does too.
fn confirm(question: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let _ = write!(
        anstream::stdout(),
        "   {} {} {} ",
        paint(ACCENT, "?"),
        paint(HEAD, question),
        paint(DIM, hint)
    );
    let _ = anstream::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        say!();
        return default_yes;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" | "j" | "ja" => true,
        _ => false,
    }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    fn SetConsoleTitleW(title: *const u16) -> i32;
}

/// Double-clicked from Explorer, the console window belongs to this process
/// alone and closes the moment it exits — before anyone could read the result.
fn pause_if_double_clicked(yes: bool) {
    if yes {
        return;
    }
    let mut pids = [0u32; 4];
    // SAFETY: the buffer is valid for `pids.len()` elements.
    let attached = unsafe { GetConsoleProcessList(pids.as_mut_ptr(), pids.len() as u32) };
    if attached == 1 {
        let _ = write!(
            anstream::stdout(),
            "   {}",
            paint(DIM, "Press Enter to close this window.")
        );
        let _ = anstream::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
    }
}

fn set_title(title: &str) {
    let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL terminated UTF-16 buffer that outlives the call.
    unsafe {
        SetConsoleTitleW(wide.as_ptr());
    }
}
