//! The interactive picker, driven through `gum choose`. gum is required only
//! here, so a sweep runs on a box that has never seen it. The row alignment
//! and the dimming are native -- no `column`, no `gum style` subprocess.

use crate::prlist::Row;
use crate::repo;
use anyhow::{Result, bail};
use std::io::Write;
use std::process::{Command, Stdio};

const DIM: &str = "\x1b[38;5;245m";
const LEGEND_COLOR: &str = "\x1b[38;5;240m";
const RESET: &str = "\x1b[0m";

/// Pad every column to its widest cell, two-space gutter, last column ragged.
fn align_rows(cells: &[Vec<String>]) -> Vec<String> {
    let cols = cells.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in cells {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    cells
        .iter()
        .map(|row| {
            let mut line = String::new();
            for (i, cell) in row.iter().enumerate() {
                if i + 1 == row.len() {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

fn display_rows(rows: &[Row], show_resumable: bool) -> Vec<String> {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let mut row = vec![
                format!("#{}", r.number),
                r.engage.label().to_string(),
                r.review.to_string(),
                r.ci.label().to_string(),
            ];
            if show_resumable {
                row.push(if r.resumable { "RESUMABLE".into() } else { "-".into() });
            }
            row.push(r.rel_time.clone());
            row.push(format!("@{}", r.author));
            row.push(r.title.clone());
            row
        })
        .collect();
    align_rows(&cells)
        .into_iter()
        .zip(rows)
        .map(|(line, r)| {
            if r.bot {
                // 256-color gray 245: bot rows read as lower-priority noise.
                format!("{DIM}{line}{RESET}")
            } else {
                line
            }
        })
        .collect()
}

fn legend(show_resumable: bool, continue_sessions: bool, include_dependabot: bool) -> String {
    let mut text = String::from(
        "NEW = unreviewed by you   UPDATED = activity since your last comment   SEEN = nothing new   CHANGES = changes requested   PENDING/FAILING = CI on the head commit",
    );
    if show_resumable {
        if continue_sessions {
            text.push_str("   RESUMABLE = earlier review session, resumed by -C");
        } else {
            text.push_str("   RESUMABLE = earlier review session; pass -C to resume it");
        }
    }
    if include_dependabot {
        text.push_str("   (dimmed = Dependabot)");
    }
    format!("{LEGEND_COLOR}{text}{RESET}")
}

/// Run the picker; None (after printing why) means nothing selected and the
/// caller exits 0.
pub fn run(
    rows: &[Row],
    continue_sessions: bool,
    include_dependabot: bool,
) -> Result<Option<Vec<u64>>> {
    repo::require_deps(&["gum"])?;

    let show_resumable = rows.iter().any(|r| r.resumable);
    let display = display_rows(rows, show_resumable);
    let header = format!(
        "Pick PRs to review (space toggles, enter confirms)\n{}",
        legend(show_resumable, continue_sessions, include_dependabot)
    );

    let mut child = Command::new("gum")
        .args(["choose", "--no-limit", "--header", &header, "--height", "20"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    {
        let mut stdin = child.stdin.take().expect("gum stdin");
        for line in &display {
            let _ = writeln!(stdin, "{line}");
        }
    }
    // Esc / an empty pick is "nothing selected", not an error.
    let out = child.wait_with_output()?;
    let selected = String::from_utf8_lossy(&out.stdout);

    if selected.trim().is_empty() {
        println!("no PRs selected");
        return Ok(None);
    }

    let mut numbers = Vec::new();
    for line in selected.lines() {
        if let Some(pos) = line.find('#') {
            let digits: String = line[pos + 1..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse::<u64>() {
                numbers.push(n);
            }
        }
    }
    if numbers.is_empty() {
        eprintln!("error: could not parse PR numbers from selection");
        bail!(repo::AlreadyReported);
    }
    Ok(Some(numbers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::Ci;
    use crate::prlist::Engagement;

    fn row(n: u64, bot: bool, resumable: bool) -> Row {
        Row {
            bot,
            number: n,
            engage: Engagement::New,
            review: "-",
            rel_time: "5h ago".into(),
            author: "alice".into(),
            title: "Add retry logic".into(),
            updated_at: "2026-08-10T10:00:00Z".into(),
            resumable,
            head: None,
            ci: Ci::None,
        }
    }

    #[test]
    fn rows_align_and_dim() {
        let rows = vec![row(9, false, false), row(123, true, false)];
        let display = display_rows(&rows, false);
        assert!(display[0].starts_with("#9    NEW"));
        assert!(display[1].starts_with(DIM));
        assert!(display[1].contains("#123  NEW"));
    }

    #[test]
    fn the_ci_column_says_what_the_sweep_would_wait_for() {
        // The picker does not hold anything -- a pick is a pick -- so the
        // column is how a person sees what the sweep would have held.
        let pending = Row { ci: Ci::Pending, ..row(9, false, false) };
        let display = display_rows(&[pending, row(8, false, false)], false);
        assert!(display[0].contains("PENDING"), "{}", display[0]);
        assert!(display[1].contains("  -  "), "no checks reads as a dash: {}", display[1]);
        assert!(legend(false, false, false).contains("PENDING/FAILING = CI"));
    }

    #[test]
    fn resumable_column_only_when_something_is() {
        let rows = vec![row(9, false, true)];
        let with = display_rows(&rows, true);
        assert!(with[0].contains("RESUMABLE"));
        let without = display_rows(&rows, false);
        assert!(!without[0].contains("RESUMABLE"));
    }

    #[test]
    fn legend_variants() {
        assert!(legend(false, false, false).contains("NEW = unreviewed by you"));
        assert!(legend(true, false, false).contains("pass -C to resume it"));
        assert!(legend(true, true, false).contains("resumed by -C"));
        assert!(legend(false, false, true).contains("dimmed = Dependabot"));
    }
}
