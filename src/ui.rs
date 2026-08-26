//! Everything the user reads.
//!
//! Two renderings of the same pass. On a TTY: a live board -- one animated
//! spinner line per running review, finished reviews promoted to permanent
//! result lines above it, an overall progress bar below -- and a summary as
//! rounded tables. Without a TTY -- cron, CI, piped output -- there is no
//! cursor to move, so state changes print one plain line each and the summary
//! is a plain aligned table.
//!
//! The plain strings are a contract: the test suite greps for them verbatim,
//! and so do people's eyes -- keep them byte-identical across refactors.

use crate::job::{Job, JobState};
use crate::report::{Panelist, Trailer};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::collections::HashMap;
use std::io::IsTerminal;
use std::time::Duration;

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];
const TITLE_WIDTH: usize = 60;

pub fn fmt_dur(s: u64) -> String {
    if s >= 3600 {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

pub fn cost_str(cost: Option<f64>) -> String {
    match cost {
        Some(c) if c >= 0.0 => format!("${c:.2}"),
        _ => "-".into(),
    }
}

/// "1 PR" / "3 PRs". Every count the user reads goes through here: "1 PR(s)"
/// is the shape that made someone stop and reread the line.
pub fn count(n: usize, singular: &str) -> String {
    if n == 1 { format!("{n} {singular}") } else { format!("{n} {singular}s") }
}

/// The pass header. The concurrency is only worth saying when it actually
/// holds reviews back -- "1 PR, 2 at a time" describes nothing.
fn pass_headline(total: usize, jobs_max: u32) -> String {
    let subject = count(total, "PR");
    if (jobs_max as usize) < total {
        format!("reviewing {subject}, {jobs_max} at a time")
    } else {
        format!("reviewing {subject}")
    }
}

/// The base every PR hyperlink is built on. Owner and name come back from the
/// GitHub API and end up inside an escape sequence, so they are stripped of
/// anything that could close it early.
pub fn pr_url_base(owner: &str, name: &str) -> String {
    format!(
        "https://github.com/{}/{}/pull",
        crate::report::sanitize_for_display(owner),
        crate::report::sanitize_for_display(name)
    )
}

/// An OSC 8 hyperlink: the text stays the text, and the terminal makes it
/// clickable. Terminals that do not understand the sequence swallow it.
fn hyperlink(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// The RESULT cell, both modes. A reaped job's review already exited; only
/// its verdict readback is still in flight, and an interrupt summary must
/// not report it as a review that was cut short.
fn result_label(job: &Job) -> String {
    match job.state {
        JobState::Done => "done".to_string(),
        JobState::Timeout => "timed out".to_string(),
        JobState::Failed => format!("failed ({})", job.outcome()),
        JobState::Queued => "queued".to_string(),
        JobState::Running if job.reaped => "finishing".to_string(),
        JobState::Running => "running".to_string(),
    }
}

/// The FINDINGS cell: only the non-zero buckets, "none" for a clean report,
/// "-" when the review never said.
pub fn findings_label(trailer: Option<&Trailer>) -> String {
    let Some(f) = trailer.and_then(|t| t.findings.as_ref()) else {
        return "-".into();
    };
    let mut parts = Vec::new();
    for (n, word) in [(f.must_fix, "must-fix"), (f.should_fix, "should-fix"), (f.polish, "polish")] {
        match n {
            Some(0) | None => {}
            Some(n) => parts.push(format!("{n} {word}")),
        }
    }
    if parts.is_empty() {
        // "none" is a claim about all three buckets; a report that omitted
        // one has not made it.
        if [f.must_fix, f.should_fix, f.polish].iter().all(|n| *n == Some(0)) {
            "none".into()
        } else {
            "-".into()
        }
    } else {
        parts.join(", ")
    }
}

/// What landed on the PR, or the fact that nothing did. "-" read as a verdict
/// of its own -- a refusal to approve -- when it only ever meant "no review
/// was posted".
pub fn verdict_label(verdict: Option<&str>) -> &str {
    verdict.filter(|v| !v.is_empty()).unwrap_or("nothing posted")
}

/// Which panelist a row belongs to. The model is the identifying half; the
/// CLI's own name is the fallback for a panelist that never reported one.
pub fn panel_model_label(p: &Panelist) -> &str {
    fn named(s: Option<&str>) -> Option<&str> {
        s.filter(|v| !v.is_empty())
    }
    named(p.model.as_deref()).or_else(|| named(p.name.as_deref())).unwrap_or("unknown")
}

/// One panelist, in words: "codex (gpt-5.5) 3 findings, top MEDIUM".
pub fn panelist_label(p: &Panelist) -> String {
    let name = p.name.as_deref().unwrap_or("?");
    let model = p.model.as_deref().unwrap_or("unknown");
    let mut s = format!("{name} ({model})");
    if p.ok == Some(false) {
        s.push_str(" failed");
        return s;
    }
    match p.findings {
        Some(0) => s.push_str(" clean"),
        Some(1) => s.push_str(" 1 finding"),
        Some(n) => s.push_str(&format!(" {n} findings")),
        None => {}
    }
    if let Some(top) = p.top.as_deref()
        && p.findings.unwrap_or(0) > 0
    {
        s.push_str(&format!(", top {top}"));
    }
    s
}

fn opt_label(v: Option<&str>) -> String {
    v.filter(|s| !s.is_empty()).unwrap_or("-").to_string()
}

pub struct Ui {
    pub tty: bool,
    /// Where a "#9" links to, or None when hyperlinks are off (no terminal,
    /// or a terminal that asked for plain output).
    pr_url_base: Option<String>,
    board: Option<Board>,
}

/// The live TTY board: spinners for running reviews, a progress bar for the
/// pass. Finished reviews are printed once, above the bars, and scroll away
/// naturally -- so the board holds at most --jobs running rows, plus the
/// transient "finishing" rows of reaped reviews whose verdict readback is
/// still in flight.
struct Board {
    mp: MultiProgress,
    bars: HashMap<u64, ProgressBar>,
    footer: ProgressBar,
}

impl Ui {
    pub fn new(pr_url_base: String) -> Ui {
        let tty = std::io::stdout().is_terminal();
        // Piped output must stay greppable, and a reader who set $NO_COLOR (or
        // is on TERM=dumb) asked for text, not escape sequences -- which is
        // exactly what console::colors_enabled already answers.
        let linked = tty && console::colors_enabled();
        Ui { tty, pr_url_base: linked.then_some(pr_url_base), board: None }
    }

    /// The "#9" a summary shows, clickable where the terminal allows it.
    fn pr_label(&self, pr: u64) -> String {
        let text = format!("#{pr}");
        match &self.pr_url_base {
            Some(base) => hyperlink(&format!("{base}/{pr}"), &text),
            None => text,
        }
    }

    /// A note the user should see now: spawn failures, session fallbacks.
    /// On the board it prints above the bars; elsewhere it goes to stderr.
    pub fn note(&mut self, note: String) {
        match &self.board {
            Some(b) => {
                let _ = b.mp.println(format!("  {}", style(&note).yellow()));
            }
            None => eprintln!("{note}"),
        }
    }

    /// Without a TTY the in-place board is replaced by one line per state
    /// change. On a TTY this drives the board instead: a start adds a
    /// spinner, a finish prints a permanent result line and drops it.
    pub fn note_transition(&mut self, job: &Job) {
        if self.tty {
            self.board_transition(job);
            return;
        }
        let n = job.pr;
        match job.state {
            JobState::Running => println!("start   #{n}"),
            JobState::Done => println!("done    #{n} ({})", fmt_dur(job.elapsed_secs)),
            JobState::Failed => {
                println!("FAILED  #{n} ({}, {})", job.outcome(), fmt_dur(job.elapsed_secs))
            }
            JobState::Timeout => println!("TIMEOUT #{n} ({})", fmt_dur(job.elapsed_secs)),
            JobState::Queued => {}
        }
    }

    /// Print the pass header and stand up the live board.
    pub fn begin_pass(&mut self, total: usize, jobs_max: u32, pass_dir: &std::path::Path) {
        if !self.tty {
            println!("{}", pass_headline(total, jobs_max));
            println!("logs: {}\n", pass_dir.display());
            return;
        }
        println!(
            "{} {}",
            style(pass_headline(total, jobs_max)).bold(),
            style(format!("· logs: {}", pass_dir.display())).dim()
        );
        println!();
        let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());
        let footer = mp.add(ProgressBar::new(total as u64));
        footer.set_style(
            ProgressStyle::with_template("  {bar:24.cyan/238} {pos}/{len} {msg}")
                .expect("footer template")
                .progress_chars("━╸─"),
        );
        footer.set_message(style("reviewed").dim().to_string());
        self.board = Some(Board { mp, bars: HashMap::new(), footer });
    }

    fn board_transition(&mut self, job: &Job) {
        let Some(board) = &mut self.board else {
            return;
        };
        match job.state {
            JobState::Running => {
                let bar = board.mp.insert_before(&board.footer, ProgressBar::new_spinner());
                bar.set_style(
                    ProgressStyle::with_template("  {spinner:.magenta} {msg}")
                        .expect("spinner template")
                        .tick_strings(SPINNER_FRAMES),
                );
                bar.set_message(running_line(job));
                bar.enable_steady_tick(Duration::from_millis(80));
                board.bars.insert(job.pr, bar);
            }
            JobState::Done | JobState::Failed | JobState::Timeout => {
                if let Some(bar) = board.bars.remove(&job.pr) {
                    bar.finish_and_clear();
                    board.mp.remove(&bar);
                }
                let _ = board.mp.println(finished_line(job));
                board.footer.inc(1);
            }
            JobState::Queued => {}
        }
    }

    /// Refresh the running spinners' elapsed time and the footer counts.
    /// Called on the pool's tick; the spinner animation itself runs on
    /// indicatif's own steady tick.
    pub fn render(&mut self, jobs: &[Job]) {
        let Some(board) = &self.board else {
            return;
        };
        let mut running = 0usize;
        let mut finishing = 0usize;
        let mut queued = 0usize;
        for job in jobs {
            match job.state {
                JobState::Running => {
                    if job.reaped {
                        finishing += 1;
                    } else {
                        running += 1;
                    }
                    if let Some(bar) = board.bars.get(&job.pr) {
                        bar.set_message(running_line(job));
                    }
                }
                JobState::Queued => queued += 1,
                _ => {}
            }
        }
        let mut msg = format!("{running} running");
        if finishing > 0 {
            msg.push_str(&format!(" · {finishing} finishing"));
        }
        if queued > 0 {
            msg.push_str(&format!(" · {queued} queued"));
        }
        board.footer.set_message(style(msg).dim().to_string());
    }

    /// Tear the board down, leaving only the permanent result lines. Safe to
    /// call twice: the interrupt path and the normal end both come through.
    pub fn end_pass(&mut self) {
        if let Some(board) = self.board.take() {
            for (_, bar) in board.bars {
                bar.finish_and_clear();
            }
            board.footer.finish_and_clear();
            let _ = board.mp.clear();
        }
    }

    pub fn hide_cursor(&self) {
        if self.tty {
            print!("\x1b[?25l");
        }
    }

    pub fn show_cursor(&self) {
        if self.tty {
            print!("\x1b[?25h");
            let _ = std::io::Write::flush(&mut std::io::stdout());
        }
    }

    pub fn print_summary(&self, jobs: &[Job], pass_dir: &std::path::Path) {
        if self.tty {
            self.print_summary_tables(jobs, pass_dir);
        } else {
            self.print_summary_plain(jobs, pass_dir);
        }
    }

    fn print_summary_plain(&self, jobs: &[Job], pass_dir: &std::path::Path) {
        let mut rows: Vec<Vec<String>> = vec![
            ["PR", "RESULT", "VERDICT", "RISK", "FINDINGS", "TIME", "COST", "MODEL", "SESSION"]
                .map(String::from)
                .to_vec(),
        ];
        for job in jobs {
            rows.push(vec![
                format!("#{}", job.pr),
                result_label(job),
                verdict_label(job.verdict.as_deref()).to_string(),
                opt_label(job.trailer.as_ref().and_then(|t| t.risk.as_deref())),
                findings_label(job.trailer.as_ref()),
                fmt_dur(job.elapsed_secs),
                cost_str(job.cost),
                opt_label(job.model.as_deref()),
                job.sid.clone().unwrap_or_else(|| "-".into()),
            ]);
        }
        println!();
        print!("{}", align(&rows));
        for job in jobs {
            if let Some(t) = &job.trailer
                && !t.panel.is_empty()
            {
                let panelists: Vec<String> = t.panel.iter().map(panelist_label).collect();
                println!("panel #{}: {}", job.pr, panelists.join("; "));
            }
        }
        println!("\nlogs: {}", pass_dir.display());
        println!("reopen any review with: claude --resume <SESSION>");
    }

    /// What each review concluded. Split out from the printing so a test can
    /// read the rendered table back -- the PR cells carry hyperlinks, whose
    /// whole risk is that a terminal counts them as visible width.
    fn results_table(&self, jobs: &[Job]) -> Table {
        let mut table = new_table();
        table.set_header(vec!["PR", "RESULT", "VERDICT", "RISK", "FINDINGS", "TIME", "COST", "MODEL"]);
        for job in jobs {
            table.add_row(vec![
                Cell::new(self.pr_label(job.pr)).add_attribute(Attribute::Bold),
                result_cell(job),
                verdict_cell(job.verdict.as_deref()),
                risk_cell(job.trailer.as_ref().and_then(|t| t.risk.as_deref())),
                Cell::new(findings_label(job.trailer.as_ref())),
                Cell::new(fmt_dur(job.elapsed_secs)),
                Cell::new(cost_str(job.cost)),
                Cell::new(opt_label(job.model.as_deref())),
            ]);
        }
        table
    }

    /// Which models did the reviewing, one row per panelist. None when no
    /// review reported a panel.
    fn panel_table(&self, jobs: &[Job]) -> Option<Table> {
        if !jobs.iter().any(|j| j.trailer.as_ref().is_some_and(|t| !t.panel.is_empty())) {
            return None;
        }
        let mut panel = new_table();
        panel.set_header(vec!["PR", "MODEL", "STATUS", "FINDINGS", "TOP"]);
        for job in jobs {
            let Some(t) = &job.trailer else { continue };
            for p in &t.panel {
                panel.add_row(vec![
                    Cell::new(self.pr_label(job.pr)).add_attribute(Attribute::Bold),
                    Cell::new(panel_model_label(p)),
                    // Whether the panelist came back with a review at all --
                    // not whether it liked the PR. A panelist that never said
                    // gets a "-" rather than being read as a success.
                    match p.ok {
                        Some(true) => Cell::new("answered").fg(Color::Green),
                        Some(false) => Cell::new("failed").fg(Color::Red),
                        None => Cell::new("-").add_attribute(Attribute::Dim),
                    },
                    Cell::new(p.findings.map_or("-".into(), |n| n.to_string())),
                    risk_cell(p.top.as_deref().filter(|_| p.findings.unwrap_or(0) > 0)),
                ]);
            }
        }
        Some(panel)
    }

    fn print_summary_tables(&self, jobs: &[Job], pass_dir: &std::path::Path) {
        println!();
        println!("{}", self.results_table(jobs));
        if let Some(panel) = self.panel_table(jobs) {
            println!("{panel}");
        }

        let resumable: Vec<&Job> = jobs.iter().filter(|j| j.sid.is_some()).collect();
        if !resumable.is_empty() {
            println!("{}", style("reopen any review with: claude --resume <SESSION>").dim());
            // Padded by the number's own width: the label may carry a
            // hyperlink, whose bytes are not columns.
            let widest =
                resumable.iter().map(|j| j.pr.to_string().len()).max().unwrap_or(0);
            for job in resumable {
                println!(
                    "  {}{}  {}",
                    style(self.pr_label(job.pr)).cyan(),
                    " ".repeat(widest - job.pr.to_string().len()),
                    job.sid.as_deref().unwrap_or("-")
                );
            }
        }
        println!("{}", style(format!("logs: {}", pass_dir.display())).dim());
    }
}

fn new_table() -> Table {
    let mut table = Table::new();
    table
        .load_style(UTF8_FULL_CONDENSED.with_rounded_corners())
        .set_content_arrangement(ContentArrangement::Dynamic);
    table
}

fn result_cell(job: &Job) -> Cell {
    match job.state {
        JobState::Done => Cell::new("done").fg(Color::Green),
        JobState::Timeout => Cell::new("timed out").fg(Color::Yellow),
        JobState::Failed => Cell::new(result_label(job)).fg(Color::Red),
        _ => Cell::new(result_label(job)),
    }
}

fn verdict_cell(verdict: Option<&str>) -> Cell {
    match verdict {
        Some("approved") => Cell::new("approved").fg(Color::Green).add_attribute(Attribute::Bold),
        Some("changes requested") => Cell::new("changes requested").fg(Color::Yellow),
        Some("commented") => Cell::new("commented").fg(Color::Cyan),
        Some(other) if !other.is_empty() => Cell::new(other),
        _ => Cell::new(verdict_label(None)).add_attribute(Attribute::Dim),
    }
}

fn risk_cell(risk: Option<&str>) -> Cell {
    match risk {
        Some("LOW") => Cell::new("LOW").fg(Color::Green),
        Some("MEDIUM") => Cell::new("MEDIUM").fg(Color::Yellow),
        Some("HIGH") => Cell::new("HIGH").fg(Color::Red),
        Some("CRITICAL") => Cell::new("CRITICAL").fg(Color::Red).add_attribute(Attribute::Bold),
        Some(other) => Cell::new(other),
        None => Cell::new("-").add_attribute(Attribute::Dim),
    }
}

/// PR titles are other people's text headed for the terminal: control bytes
/// (ANSI/OSC escapes) could repaint the board and bidi/zero-width marks
/// could visually reorder it, so both are dropped before display.
fn short_title(title: &str) -> String {
    let clean = crate::report::sanitize_for_display(title);
    console::truncate_str(&clean, TITLE_WIDTH, "…").to_string()
}

fn running_line(job: &Job) -> String {
    // A reaped review already exited and only the verdict readback remains:
    // freeze the clock at the real duration rather than letting it climb
    // past what the summary will report.
    let (verb, secs) = if job.reaped {
        ("finishing", job.elapsed_secs)
    } else {
        (
            if job.resume { "rechecking" } else { "reviewing" },
            job.started.map(|s| s.elapsed().as_secs()).unwrap_or(0),
        )
    };
    format!(
        "{} {} {}",
        style(format!("#{}", job.pr)).cyan().bold(),
        style(short_title(&job.title)).dim(),
        style(format!("· {verb} {}", fmt_dur(secs))).magenta()
    )
}

/// The permanent line a finished review leaves on the board.
fn finished_line(job: &Job) -> String {
    let (mark, headline) = match job.state {
        JobState::Done => {
            let word = match job.verdict.as_deref() {
                Some("approved") => style("approved").green().bold().to_string(),
                Some("changes requested") => style("changes requested").yellow().to_string(),
                Some("commented") => style("commented").cyan().to_string(),
                Some(other) => other.to_string(),
                None => style("done").green().to_string(),
            };
            (style("✓").green().bold().to_string(), word)
        }
        JobState::Timeout => (style("✗").yellow().bold().to_string(), style("timed out").yellow().to_string()),
        _ => (
            style("✗").red().bold().to_string(),
            style(format!("failed ({})", job.outcome())).red().to_string(),
        ),
    };
    let mut extras = Vec::new();
    if let Some(risk) = job.trailer.as_ref().and_then(|t| t.risk.as_deref()) {
        extras.push(format!("risk {risk}"));
    }
    extras.push(fmt_dur(job.elapsed_secs));
    if let Some(cost) = job.cost {
        extras.push(format!("${cost:.2}"));
    }
    format!(
        "  {mark} {} {headline} {} {}",
        style(format!("#{}", job.pr)).cyan().bold(),
        style(format!("· {}", extras.join(" · "))).dim(),
        style(short_title(&job.title)).dim()
    )
}

/// A panic must not leave the terminal without its cursor.
impl Drop for Ui {
    fn drop(&mut self) {
        self.end_pass();
        self.show_cursor();
    }
}

/// What `column -t` did, natively: pad each column to its widest cell with a
/// two-space gutter, last column ragged. `column` lives in util-linux and the
/// boxes this tool is built for -- slim CI images -- routinely ship without
/// it; a summary must never die on formatting. Widths are display widths,
/// not byte counts: the verdict/risk/model columns carry agent-authored text
/// that may be multibyte.
pub fn align(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(console::measure_text_width(cell));
        }
    }
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            line.push_str(cell);
            if i + 1 < row.len() {
                let pad = widths[i] - console::measure_text_width(cell) + 2;
                line.push_str(&" ".repeat(pad));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Job;
    use crate::report::parse_trailer;

    #[test]
    fn durations_read_as_written() {
        assert_eq!(fmt_dur(3), "3s");
        assert_eq!(fmt_dur(63), "1m03s");
        assert_eq!(fmt_dur(252), "4m12s");
        assert_eq!(fmt_dur(3600), "1h00m");
        assert_eq!(fmt_dur(3900), "1h05m");
    }

    #[test]
    fn costs() {
        assert_eq!(cost_str(Some(0.42)), "$0.42");
        assert_eq!(cost_str(Some(1.005)), "$1.00");
        assert_eq!(cost_str(None), "-");
    }

    #[test]
    fn summary_alignment() {
        let rows = vec![
            vec!["PR".into(), "RESULT".into(), "SESSION".into()],
            vec!["#9".into(), "done".into(), "abc".into()],
            vec!["#123".into(), "failed (no result)".into(), "-".into()],
        ];
        let out = align(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "PR    RESULT              SESSION");
        assert_eq!(lines[1], "#9    done                abc");
        assert_eq!(lines[2], "#123  failed (no result)  -");
    }

    #[test]
    fn alignment_pads_by_display_width_not_bytes() {
        // "LÅG" is three columns wide but four bytes; byte padding would
        // shift every later column of its row.
        let rows = vec![
            vec!["RISK".into(), "NEXT".into(), "END".into()],
            vec!["LÅG".into(), "x".into(), "y".into()],
        ];
        let lines = align(&rows);
        let lines: Vec<&str> = lines.lines().collect();
        assert_eq!(lines[0], "RISK  NEXT  END");
        assert_eq!(lines[1], "LÅG   x     y");
    }

    #[test]
    fn a_reaped_running_job_reads_as_finishing() {
        let mut job = Job::new(9);
        job.state = JobState::Running;
        assert_eq!(result_label(&job), "running");
        job.reaped = true;
        assert_eq!(result_label(&job), "finishing");
    }

    #[test]
    fn transition_outcomes_render_in_the_failed_line() {
        let mut job = Job::new(9);
        job.exit_code = None;
        assert_eq!(job.outcome(), "no result");
        job.exit_code = Some(10);
        assert_eq!(format!("FAILED  #{} ({}, {})", job.pr, job.outcome(), fmt_dur(3)), "FAILED  #9 (exit 10, 3s)");
    }

    #[test]
    fn findings_cells() {
        assert_eq!(findings_label(None), "-");
        let t = parse_trailer("```autoreview\n{\"findings\":{\"must_fix\":1,\"should_fix\":0,\"polish\":2}}\n```");
        assert_eq!(findings_label(t.as_ref()), "1 must-fix, 2 polish");
        let clean = parse_trailer("```autoreview\n{\"findings\":{\"must_fix\":0,\"should_fix\":0,\"polish\":0}}\n```");
        assert_eq!(findings_label(clean.as_ref()), "none");
        let unknown = parse_trailer("```autoreview\n{\"decision\":\"approved\"}\n```");
        assert_eq!(findings_label(unknown.as_ref()), "-");
        // A report that omitted a bucket has not claimed "none".
        let partial = parse_trailer("```autoreview\n{\"findings\":{\"must_fix\":0}}\n```");
        assert_eq!(findings_label(partial.as_ref()), "-");
    }

    #[test]
    fn a_reaped_job_freezes_its_clock() {
        let mut job = Job::new(9);
        job.title = "t".into();
        job.reaped = true;
        job.elapsed_secs = 252;
        let line = running_line(&job);
        assert!(line.contains("finishing"));
        assert!(line.contains("4m12s"));
        job.reaped = false;
        assert!(running_line(&job).contains("reviewing"));
    }

    #[test]
    fn titles_lose_their_control_bytes() {
        assert_eq!(short_title("Add \x1b[31mretry\x1b[0m logic"), "Add [31mretry[0m logic");
        assert_eq!(short_title("plain title"), "plain title");
        // Bidi overrides and zero-width characters reorder or hide text
        // without being C0 controls; they must go too.
        assert_eq!(short_title("fix\u{202E}cod.exe"), "fixcod.exe");
        assert_eq!(short_title("a\u{200B}b\u{FEFF}c"), "abc");
    }

    #[test]
    fn counts_read_as_english() {
        assert_eq!(count(1, "PR"), "1 PR");
        assert_eq!(count(0, "PR"), "0 PRs");
        assert_eq!(count(3, "review"), "3 reviews");
    }

    #[test]
    fn the_pass_header_only_claims_a_limit_that_binds() {
        assert_eq!(pass_headline(1, 2), "reviewing 1 PR");
        assert_eq!(pass_headline(2, 2), "reviewing 2 PRs");
        assert_eq!(pass_headline(5, 2), "reviewing 5 PRs, 2 at a time");
    }

    #[test]
    fn an_empty_verdict_says_nothing_landed() {
        // A bare "-" read as a verdict of its own; it never was one.
        assert_eq!(verdict_label(None), "nothing posted");
        assert_eq!(verdict_label(Some("")), "nothing posted");
        assert_eq!(verdict_label(Some("approved")), "approved");
    }

    fn linked_ui() -> Ui {
        Ui {
            tty: true,
            pr_url_base: Some("https://github.com/acme/widgets/pull".into()),
            board: None,
        }
    }

    fn done_job(pr: u64) -> Job {
        let mut job = Job::new(pr);
        job.state = JobState::Done;
        job
    }

    #[test]
    fn pr_cells_link_to_the_pull_request() {
        let ui = linked_ui();
        assert_eq!(
            ui.pr_label(9),
            "\x1b]8;;https://github.com/acme/widgets/pull/9\x1b\\#9\x1b]8;;\x1b\\"
        );
        // Off the terminal there is nothing to click and escapes would only
        // break grep.
        let plain = Ui { tty: false, pr_url_base: None, board: None };
        assert_eq!(plain.pr_label(9), "#9");
    }

    #[test]
    fn a_linked_table_still_lines_up() {
        // The whole risk of an in-cell hyperlink: 40-odd invisible bytes that
        // a naive width count would pad around.
        let out = linked_ui().results_table(&[done_job(9), done_job(123)]).to_string();
        let widths: Vec<usize> =
            out.lines().map(console::measure_text_width).collect();
        let plain: Vec<usize> = Ui { tty: false, pr_url_base: None, board: None }
            .results_table(&[done_job(9), done_job(123)])
            .to_string()
            .lines()
            .map(console::measure_text_width)
            .collect();
        assert_eq!(widths.len(), plain.len());
        // Borders carry no links, so every border row must match exactly.
        for (i, line) in out.lines().enumerate() {
            if !line.contains('\x1b') {
                assert_eq!(widths[i], plain[i], "row {i} changed width: {line}");
            }
        }
    }

    #[test]
    fn the_panel_table_names_models_not_clis() {
        let job = {
            let mut j = done_job(9);
            j.trailer = parse_trailer(
                "```autoreview\n{\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\",\"ok\":true,\"findings\":1,\"top\":\"LOW\"},{\"name\":\"opencode\",\"ok\":false}]}\n```",
            );
            j
        };
        let out = Ui { tty: false, pr_url_base: None, board: None }
            .panel_table(&[job])
            .unwrap()
            .to_string();
        assert!(out.contains("MODEL") && !out.contains("PANELIST"));
        assert!(out.contains("gpt-5.5"));
        // A panelist that never reported a model still has to identify itself.
        assert!(out.contains("opencode"));
        // "ok" said nothing about whether the panelist actually replied.
        assert!(out.contains("answered") && out.contains("failed") && !out.contains("ok"));
    }

    #[test]
    fn panelists_fall_back_to_their_cli_name() {
        let t = parse_trailer(
            "```autoreview\n{\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\"},{\"name\":\"opencode\",\"model\":\"\"},{}]}\n```",
        )
        .unwrap();
        assert_eq!(panel_model_label(&t.panel[0]), "gpt-5.5");
        assert_eq!(panel_model_label(&t.panel[1]), "opencode");
        assert_eq!(panel_model_label(&t.panel[2]), "unknown");
    }

    #[test]
    fn panelist_lines() {
        let t = parse_trailer(
            "```autoreview\n{\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\",\"ok\":true,\"findings\":3,\"top\":\"MEDIUM\"},{\"name\":\"claude\",\"model\":\"claude-opus-4.7\",\"ok\":true,\"findings\":0},{\"name\":\"opencode\",\"ok\":false}]}\n```",
        )
        .unwrap();
        assert_eq!(panelist_label(&t.panel[0]), "codex (gpt-5.5) 3 findings, top MEDIUM");
        assert_eq!(panelist_label(&t.panel[1]), "claude (claude-opus-4.7) clean");
        assert_eq!(panelist_label(&t.panel[2]), "opencode (unknown) failed");
    }
}
