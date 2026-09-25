//! Terminal presentation.
//!
//! One module owns every byte `ttk` prints, for three reasons:
//!
//! * **A closed pipe must not panic.** `ttk retrieve … | head` closes stdout
//!   early; [`outln!`] swallows the resulting `EPIPE` the way a well behaved
//!   CLI does.
//! * **Colour is a decision, not a sprinkle.** Everything goes through
//!   [`anstream`], which strips styling when stdout is not a terminal, honours
//!   `NO_COLOR` / `CLICOLOR_FORCE`, and turns on virtual terminal processing on
//!   Windows. `--color` overrides all of it.
//! * **One vocabulary.** A key looks the same in `ttk doctor` as in
//!   `ttk stats`, so the eye learns the layout once.
//!
//! The palette is deliberately small: an accent, a good, a warning, a bad, a
//! key and a dim. Anything that needs a seventh colour needs a better layout.

use std::fmt;

use anstyle::{AnsiColor, Color, Style};

/// `println!` that tolerates a closed stdout and routes through anstream.
macro_rules! outln {
    () => {{ let _ = { use std::io::Write; writeln!(anstream::stdout()) }; }};
    ($($t:tt)*) => {{ let _ = { use std::io::Write; writeln!(anstream::stdout(), $($t)*) }; }};
}

/// `print!` that tolerates a closed stdout.
macro_rules! out {
    ($($t:tt)*) => {{ let _ = { use std::io::Write; write!(anstream::stdout(), $($t)*) }; }};
}

/// `eprintln!` through anstream, so stderr is styled and stripped by the same
/// rules as stdout.
macro_rules! errln {
    ($($t:tt)*) => {{ let _ = { use std::io::Write; writeln!(anstream::stderr(), $($t)*) }; }};
}

pub(crate) use {errln, out, outln};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// The brand colour. Purple, for reasons that should be obvious.
pub const ACCENT: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::BrightMagenta)))
    .bold();
/// A heading inside a report.
pub const HEAD: Style = Style::new().bold();
/// The left hand column of a key/value block.
pub const KEY: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Cyan)));
/// A measured number worth looking at.
pub const NUM: Style = Style::new().bold();
/// Something went right.
pub const OK: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Green)));
/// Something needs attention but nothing is broken.
pub const WARN: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Yellow)));
/// Something is broken.
pub const BAD: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Red)));
/// Secondary text: units, notes, provenance.
pub const DIM: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightBlack)));
/// A literal command the reader can type.
pub const CODE: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightCyan)));
/// The one number a report exists to show.
pub const HERO: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::BrightGreen)))
    .bold();
/// Tokens saved, wherever they appear in a table.
pub const SAVED: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::BrightGreen)));
/// The frame around the masthead.
pub const FRAME: Style = Style::new().fg_color(Some(Color::Ansi(AnsiColor::Magenta)));

/// Apply a style to anything printable.
///
/// `paint(OK, "yes")` renders as `yes` in green, or as plain `yes` when the
/// stream is redirected — anstream decides, not this function.
pub fn paint<T: fmt::Display>(style: Style, value: T) -> String {
    format!("{style}{value}{style:#}")
}

/// Set the global colour policy from a `--color` value.
pub fn set_color_choice(choice: &str) -> Result<(), ttk_core::Error> {
    let c = match choice.trim().to_ascii_lowercase().as_str() {
        "auto" => anstream::ColorChoice::Auto,
        "always" => anstream::ColorChoice::Always,
        "never" => anstream::ColorChoice::Never,
        other => {
            return Err(ttk_core::Error::Config(format!(
                "unknown --color value `{other}` (expected auto, always or never)"
            )));
        }
    };
    c.write_global();
    Ok(())
}

// ---------------------------------------------------------------------------
// Glyphs
// ---------------------------------------------------------------------------

/// A small glyph set, with an ASCII fallback for terminals that mangle UTF-8.
///
/// `TTK_ASCII=1` switches it. There is no auto-detection: guessing a Windows
/// console code page wrongly produces mojibake that looks like a bug in ttk.
pub struct Glyphs {
    pub ok: &'static str,
    pub warn: &'static str,
    pub bad: &'static str,
    pub bullet: &'static str,
    pub arrow: &'static str,
    pub bar_full: &'static str,
    pub bar_empty: &'static str,
    pub corner: &'static str,
    /// The brand mark in the masthead.
    pub mark: &'static str,
    /// The marker in front of a section heading.
    pub section: &'static str,
    /// Box drawing: top left, top right, bottom left, bottom right,
    /// horizontal, vertical.
    pub frame: [&'static str; 6],
    /// Eight levels, lowest first, for sparklines.
    pub levels: [&'static str; 8],
}

const UNICODE: Glyphs = Glyphs {
    ok: "✓",
    warn: "!",
    bad: "✗",
    bullet: "·",
    arrow: "→",
    bar_full: "█",
    bar_empty: "░",
    corner: "└",
    mark: "◆",
    section: "▌",
    frame: ["╭", "╮", "╰", "╯", "─", "│"],
    levels: ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"],
};

const ASCII: Glyphs = Glyphs {
    ok: "+",
    warn: "!",
    bad: "x",
    bullet: "-",
    arrow: "->",
    bar_full: "#",
    bar_empty: ".",
    corner: "\\",
    mark: "*",
    section: ">",
    frame: ["+", "+", "+", "+", "-", "|"],
    levels: ["_", "_", ".", ".", "-", "=", "#", "#"],
};

pub fn glyphs() -> &'static Glyphs {
    match std::env::var("TTK_ASCII").as_deref() {
        Ok("1") | Ok("true") | Ok("yes") => &ASCII,
        _ => &UNICODE,
    }
}

// ---------------------------------------------------------------------------
// Building blocks
// ---------------------------------------------------------------------------

/// Width of the key column in every key/value block, so unrelated reports still
/// line up when read one after the other.
pub const KEY_WIDTH: usize = 17;

/// Inner width of the masthead frame. Fits an 80 column terminal with room to
/// spare, which is what a default `cmd.exe` window still is.
pub const FRAME_WIDTH: usize = 64;

/// The masthead, a framed line:
///
/// ```text
/// ╭────────────────────────────────────────────────────────────────╮
/// │  ◆ ttk  lifetime statistics                            v0.1.0  │
/// ╰────────────────────────────────────────────────────────────────╯
/// ```
pub fn banner(subtitle: &str) -> String {
    let g = glyphs();
    let [tl, tr, bl, br, h, v] = g.frame;
    let version = format!("v{}", env!("CARGO_PKG_VERSION"));
    // A long title wraps onto further framed lines instead of being cut in
    // the middle of a word; only a single word too long for a line (a path)
    // is shortened, and then in the middle so both ends stay recognisable.
    let lines = wrap(subtitle, FRAME_WIDTH - 20);
    let rule = h.repeat(FRAME_WIDTH);
    let mut out = paint(FRAME, format!("{tl}{rule}{tr}"));
    for (i, line) in lines.iter().enumerate() {
        let left = if i == 0 {
            format!(
                "  {} {}  {}",
                paint(ACCENT, g.mark),
                paint(ACCENT, "ttk"),
                paint(HEAD, line)
            )
        } else {
            format!("         {}", paint(HEAD, line))
        };
        let right = if i == 0 {
            format!("{}  ", paint(DIM, &version))
        } else {
            String::new()
        };
        let fill = FRAME_WIDTH.saturating_sub(visible_len(&left) + visible_len(&right));
        out.push('\n');
        out.push_str(&format!(
            "{}{left}{}{right}{}",
            paint(FRAME, v),
            " ".repeat(fill),
            paint(FRAME, v)
        ));
    }
    out.push('\n');
    out.push_str(&paint(FRAME, format!("{bl}{rule}{br}")));
    out
}

/// Greedy word wrap to `width` visible characters; never returns no lines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let word = ellipsize(word, width);
        let needed =
            current.chars().count() + usize::from(!current.is_empty()) + word.chars().count();
        if !current.is_empty() && needed > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(&word);
    }
    lines.push(current);
    lines
}

/// A section heading: `▌ by project`, set off by a blank line.
pub fn heading(title: &str) -> String {
    format!(
        "\n{} {}",
        paint(ACCENT, glyphs().section),
        paint(HEAD, title)
    )
}

/// One cell per value, scaled to the largest: `▁▃█▅▂`.
pub fn sparkline(values: &[u64]) -> String {
    let g = glyphs();
    let top = values.iter().copied().max().unwrap_or(0);
    values
        .iter()
        .map(|&v| {
            if top == 0 || v == 0 {
                paint(DIM, g.levels[0])
            } else {
                let i = ((v as f64 / top as f64) * 7.0).round() as usize;
                paint(SAVED, g.levels[i.clamp(1, 7)])
            }
        })
        .collect()
}

/// `(year, month, day)` for a count of days since 1970-01-01.
///
/// Howard Hinnant's `civil_from_days`: exact for the proleptic Gregorian
/// calendar, and small enough that a date is not worth a dependency.
pub fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `2026-09-25`.
pub fn iso_date(days: i64) -> String {
    let (y, m, d) = civil_date(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `Fri 09-25`: weekday, month and day, for chart rows.
pub fn short_day(days: i64) -> String {
    const WEEKDAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    let (_, m, d) = civil_date(days);
    format!("{} {m:02}-{d:02}", WEEKDAYS[days.rem_euclid(7) as usize])
}

/// The viewer's UTC offset in seconds, or 0 when it cannot be determined.
pub fn local_offset_secs() -> i64 {
    time::UtcOffset::current_local_offset()
        .map(|o| i64::from(o.whole_seconds()))
        .unwrap_or(0)
}

/// `  key              value`
pub fn kv(key: &str, value: impl fmt::Display) -> String {
    // Padding is computed from the *visible* width. The styled key carries
    // escape bytes that a plain `{:<width$}` would count as characters, which
    // is exactly how coloured output ends up misaligned.
    let pad = KEY_WIDTH.saturating_sub(visible_len(key));
    format!("  {}{}{value}", paint(KEY, key), " ".repeat(pad))
}

/// `  key              value   note`, with the note dimmed.
pub fn kv_note(key: &str, value: impl fmt::Display, note: impl fmt::Display) -> String {
    let note = note.to_string();
    if note.is_empty() {
        return kv(key, value);
    }
    format!("{}  {}", kv(key, value), paint(DIM, note))
}

/// Width of the value column in a `kv_num` row.
pub const NUM_WIDTH: usize = 12;

/// A right aligned number in the value column, so digits line up.
pub fn kv_num(key: &str, value: impl fmt::Display, note: impl fmt::Display) -> String {
    // As in [`kv`], the padding has to be measured on the visible string: the
    // value is styled before it gets here.
    let value = value.to_string();
    let fill = NUM_WIDTH.saturating_sub(visible_len(&value));
    kv_note(
        key,
        format!("{}{}", " ".repeat(fill), paint(NUM, value)),
        note,
    )
}

/// A key/value row whose value is several lines, aligned under the first.
///
/// Used for a block rule's pattern: it is a run of lines and squeezing it onto
/// one would lose the only thing that makes it readable.
pub fn kv_multiline(key: &str, value: &str) -> String {
    let mut lines = value.lines();
    let Some(first) = lines.next() else {
        return kv(key, "");
    };
    let mut out = kv(key, first);
    for line in lines {
        out.push('\n');
        out.push_str(&" ".repeat(2 + KEY_WIDTH));
        out.push_str(line);
    }
    out
}

pub fn ok(msg: impl fmt::Display) -> String {
    format!("{} {msg}", paint(OK, glyphs().ok))
}

pub fn warn(msg: impl fmt::Display) -> String {
    format!("{} {msg}", paint(WARN, glyphs().warn))
}

pub fn bad(msg: impl fmt::Display) -> String {
    format!("{} {msg}", paint(BAD, glyphs().bad))
}

/// An indented continuation line under a status line.
pub fn detail(msg: impl fmt::Display) -> String {
    format!("  {} {}", paint(DIM, glyphs().corner), paint(DIM, msg))
}

/// A suggested next command.
pub fn hint(label: &str, command: &str) -> String {
    format!("  {label}  {}", paint(CODE, command))
}

/// `████████░░░░░░░░` — `fraction` of `width` filled.
///
/// Saturates rather than panicking on a fraction outside `0.0..=1.0`: a report
/// must never be the thing that crashes.
pub fn bar(fraction: f64, width: usize) -> String {
    let g = glyphs();
    let f = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (f * width as f64).round() as usize;
    let filled = filled.min(width);
    format!(
        "{}{}",
        paint(ACCENT, g.bar_full.repeat(filled)),
        paint(DIM, g.bar_empty.repeat(width - filled))
    )
}

/// Thousands separators, because a token count is read, not parsed.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A token count, marked `~` when it is a heuristic estimate.
pub fn tokens(n: u64, estimated: bool) -> String {
    if estimated {
        format!("~{}", thousands(n))
    } else {
        thousands(n)
    }
}

/// How a column is laid out in [`table`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A minimal, dependency-free table.
///
/// Column widths come from the content, the header row is dim, and nothing is
/// truncated: a report that hides a value to fit a width is worse than one that
/// wraps in the terminal.
pub fn table(headers: &[&str], aligns: &[Align], rows: &[Vec<String>]) -> String {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| visible_len(h)).collect();
    for row in rows {
        for (i, cell) in row.iter().take(cols).enumerate() {
            widths[i] = widths[i].max(visible_len(cell));
        }
    }

    let mut out = String::new();
    out.push_str("  ");
    for (i, h) in headers.iter().enumerate() {
        // The last header carries no padding: trailing spaces are invisible in
        // a terminal but show up in `| cat`, in diffs and in test fixtures.
        let cell = if i + 1 == cols {
            (*h).to_string()
        } else {
            pad(h, widths[i], aligns[i])
        };
        out.push_str(&paint(DIM, cell));
        if i + 1 < cols {
            out.push_str("  ");
        }
    }
    out.push('\n');

    for row in rows {
        out.push_str("  ");
        for i in 0..cols {
            let cell = row.get(i).map(String::as_str).unwrap_or("");
            out.push_str(&pad(cell, widths[i], aligns[i]));
            if i + 1 < cols {
                out.push_str("  ");
            }
        }
        // Trailing padding on the last column is invisible but shows up in
        // diffs and in `| cat`, so it goes.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out
}

fn pad(s: &str, width: usize, align: Align) -> String {
    let len = visible_len(s);
    let fill = width.saturating_sub(len);
    match align {
        Align::Left => format!("{s}{}", " ".repeat(fill)),
        Align::Right => format!("{}{s}", " ".repeat(fill)),
    }
}

/// Length of `s` as the terminal sees it: escape sequences do not take space.
///
/// Cells are styled before they reach [`table`], so measuring the raw string
/// would misalign every coloured column.
pub fn visible_len(s: &str) -> usize {
    #[derive(Clone, Copy)]
    enum State {
        Text,
        /// Saw ESC, waiting to see whether a control sequence follows.
        Escape,
        /// Inside `ESC [ … final`, which ends at a byte in `@`..=`~`.
        Csi,
    }

    let mut state = State::Text;
    let mut len = 0;
    for c in s.chars() {
        state = match state {
            State::Text if c == '\u{1b}' => State::Escape,
            State::Text => {
                len += 1;
                State::Text
            }
            // `ESC [` opens a control sequence. The `[` itself is inside the
            // final-byte range, so it must be consumed here rather than being
            // mistaken for the end of the sequence.
            State::Escape if c == '[' => State::Csi,
            State::Escape => State::Text,
            State::Csi if ('\u{40}'..='\u{7e}').contains(&c) => State::Text,
            State::Csi => State::Csi,
        };
    }
    len
}

/// Shorten a string in the middle, keeping both ends recognisable.
pub fn ellipsize(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max || max < 5 {
        return s.to_string();
    }
    let head: String = s.chars().take(max.div_ceil(2) - 1).collect();
    let tail: String = s.chars().skip(len - (max / 2 - 1)).collect();
    format!("{head}…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thousands_are_grouped_from_the_right() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1 000");
        assert_eq!(thousands(14_536), "14 536");
        assert_eq!(thousands(1_234_567), "1 234 567");
    }

    #[test]
    fn estimates_are_marked() {
        assert_eq!(tokens(1500, true), "~1 500");
        assert_eq!(tokens(1500, false), "1 500");
    }

    #[test]
    fn a_bar_never_exceeds_its_width_or_panics() {
        assert_eq!(visible_len(&bar(0.0, 10)), 10);
        assert_eq!(visible_len(&bar(1.0, 10)), 10);
        assert_eq!(visible_len(&bar(2.5, 10)), 10);
        assert_eq!(visible_len(&bar(-1.0, 10)), 10);
        assert_eq!(visible_len(&bar(f64::NAN, 10)), 10);
        assert_eq!(visible_len(&bar(0.5, 0)), 0);
    }

    #[test]
    fn escape_sequences_do_not_count_as_width() {
        let painted = paint(OK, "yes");
        assert_eq!(visible_len(&painted), 3);
        assert_eq!(visible_len("plain"), 5);
    }

    #[test]
    fn columns_align_even_when_cells_are_coloured() {
        let rows = vec![
            vec![paint(OK, "short"), "1".to_string()],
            vec!["a much longer cell".to_string(), "22".to_string()],
        ];
        let text = table(&["name", "n"], &[Align::Left, Align::Right], &rows);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "header plus two rows");
        // The data rows end at the same visible column: the last column is
        // right aligned and both rows are padded to the same width, even though
        // one of the cells carries colour escapes and the other does not.
        assert_eq!(visible_len(lines[1]), visible_len(lines[2]));
        // The header is not padded past its last cell, so it may be shorter.
        assert!(visible_len(lines[0]) <= visible_len(lines[1]));
        assert!(
            !lines.iter().any(|l| l.ends_with(' ')),
            "no trailing spaces"
        );
    }

    #[test]
    fn a_table_never_hides_a_value() {
        let rows = vec![vec!["a-very-long-identifier-that-must-stay".to_string()]];
        let text = table(&["id"], &[Align::Left], &rows);
        assert!(text.contains("a-very-long-identifier-that-must-stay"));
    }

    #[test]
    fn dates_come_out_of_day_numbers() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(short_day(0), "Thu 01-01");
        assert_eq!(iso_date(-1), "1969-12-31");
        // 2026-09-25 is a Friday.
        assert_eq!(iso_date(20_721), "2026-09-25");
        assert_eq!(short_day(20_721), "Fri 09-25");
        assert_eq!(iso_date(11_016), "2000-02-29");
    }

    #[test]
    fn the_banner_is_a_closed_frame() {
        let text = banner("lifetime statistics");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(text.contains("lifetime statistics"));
        assert!(text.contains(env!("CARGO_PKG_VERSION")));
        let widths: Vec<usize> = lines.iter().map(|l| visible_len(l)).collect();
        assert!(widths.iter().all(|w| *w == FRAME_WIDTH + 2), "{widths:?}");
    }

    #[test]
    fn a_long_banner_wraps_instead_of_cutting_words() {
        let title = "filter dry run · 12 rule(s) in scope, 3 matching a run";
        let text = banner(title);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4, "{text}");
        assert!(
            lines.iter().all(|l| visible_len(l) == FRAME_WIDTH + 2),
            "{text}"
        );
        for word in title.split_whitespace() {
            assert!(text.contains(word), "`{word}` went missing:\n{text}");
        }
        // A single overlong word, such as a path, is shortened in the middle.
        let path = format!("C:\\{}\\project", "very-long-directory".repeat(5));
        assert!(
            banner(&path)
                .lines()
                .all(|l| visible_len(l) == FRAME_WIDTH + 2)
        );
    }

    #[test]
    fn a_sparkline_has_one_cell_per_value() {
        assert_eq!(visible_len(&sparkline(&[0, 3, 10, 7])), 4);
        assert_eq!(visible_len(&sparkline(&[])), 0);
        assert_eq!(visible_len(&sparkline(&[0, 0])), 2);
    }

    #[test]
    fn ellipsize_keeps_both_ends() {
        assert_eq!(ellipsize("short", 10), "short");
        let s = ellipsize("abcdefghijklmnop", 9);
        assert!(s.starts_with("abcd"), "{s}");
        assert!(s.ends_with("nop"), "{s}");
        assert!(s.chars().count() <= 9, "{s}");
    }

    #[test]
    fn key_columns_line_up_across_reports() {
        let a = kv("mode", "safe");
        let b = kv("workspace", "/tmp/.ttk");
        assert_eq!(
            a.find("safe").map(|i| visible_len(&a[..i])),
            b.find("/tmp").map(|i| visible_len(&b[..i]))
        );
    }

    #[test]
    fn a_multiline_value_lines_up_under_itself() {
        let text = kv_multiline("pattern", "first\nsecond\nthird");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        let column = |l: &str, needle: &str| l.find(needle).map(|i| visible_len(&l[..i]));
        assert_eq!(column(lines[0], "first"), column(lines[1], "second"));
        assert_eq!(column(lines[1], "second"), column(lines[2], "third"));
        // A single line value is an ordinary row.
        assert_eq!(kv_multiline("pattern", "only"), kv("pattern", "only"));
    }

    #[test]
    fn an_empty_note_does_not_leave_trailing_space() {
        assert_eq!(kv_note("mode", "safe", ""), kv("mode", "safe"));
    }

    #[test]
    fn the_colour_choice_is_validated() {
        assert!(set_color_choice("always").is_ok());
        assert!(set_color_choice("never").is_ok());
        assert!(set_color_choice("auto").is_ok());
        assert!(set_color_choice("rainbow").is_err());
    }
}
