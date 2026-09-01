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
//!
//! The board is drawn by `crate::board`, an inline viewport that redraws
//! itself at the terminal's current size. This module decides what each row
//! says and how wide it may be; the board decides where it goes.

use crate::board::{self, Action, Board};
use crate::job::{Job, JobState};
use crate::report::{Panelist, Trailer};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Attribute, Cell, Color, ContentArrangement, Table};
use console::style;
use crossterm::event::Event;
use ratatui::style::{Color as Ink, Stylize};
use ratatui::text::{Line, Span};
use std::collections::HashSet;
use std::io::IsTerminal;

pub const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", " "];
/// The most title any board row will show, on a terminal wide enough for it.
const TITLE_WIDTH: usize = 60;
/// Below this a title is no longer a title. A row this tight drops the title
/// outright rather than shaving it further, because what the row has left --
/// the PR number, the verb and the clock -- is the part that tells you the
/// review is alive.
const TITLE_FLOOR: usize = 16;
/// The columns the footer's gauge draws: a full bar is this many `━`.
const GAUGE_WIDTH: usize = 24;
/// The most lines an expanded row adds under itself: what is being followed,
/// the counts, and the last few events.
const DETAIL_LINES: usize = 6;
/// The indent of a detail line, so it sits under the row's label.
const DETAIL_INDENT: &str = "      ";

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
    /// The live area, open for the length of a pass on a TTY.
    board: Option<Board>,
    /// Which spinner frame the next draw shows.
    frame: usize,
    /// The footer's counts: reviews finished, out of the pass.
    finished: usize,
    total: usize,
    /// The PRs whose rows show their details.
    expanded: HashSet<u64>,
    /// The PRs on the board at the last draw, top to bottom: what a digit
    /// key names.
    live: Vec<u64>,
}

impl Ui {
    pub fn new(pr_url_base: String) -> Ui {
        let tty = std::io::stdout().is_terminal();
        // Piped output must stay greppable, and a reader who set $NO_COLOR (or
        // is on TERM=dumb) asked for text, not escape sequences -- which is
        // exactly what console::colors_enabled already answers.
        let linked = tty && console::colors_enabled();
        Ui {
            tty,
            pr_url_base: linked.then_some(pr_url_base),
            board: None,
            frame: 0,
            finished: 0,
            total: 0,
            expanded: HashSet::new(),
            live: Vec::new(),
        }
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
    /// On the board it prints above the rows; elsewhere it goes to stderr.
    pub fn note(&mut self, note: String) {
        match &mut self.board {
            Some(b) => {
                let note = fit_str(&note, b.width().saturating_sub(2));
                let _ = b.println(Line::from(vec![Span::raw("  "), Span::from(note).yellow()]));
            }
            None => eprintln!("{note}"),
        }
    }

    /// Without a TTY the in-place board is replaced by one line per state
    /// change. On a TTY this drives the board instead: a finish prints a
    /// permanent result line, and the next draw picks up a start.
    pub fn note_transition(&mut self, job: &Job) {
        if self.tty {
            self.board_transition(job);
            return;
        }
        let n = job.pr;
        // The same three facts the board shows, one line each: who opened it,
        // and whether this is a first look or a second one. A log that only
        // says "start #9" makes you open the PR to learn either.
        let who = if job.author.is_empty() { String::new() } else { format!(" @{}", job.author) };
        match job.state {
            JobState::Running => {
                let verb = if job.resume { "rechecking" } else { "reviewing" };
                println!("start   #{n}{who} ({verb})");
            }
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
        self.finished = 0;
        self.total = total;
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
        // As many rows as can run at once, plus the footer. A reaped review
        // waiting on its readback keeps its row while the next one starts,
        // so the board may still have to grow; it does that on its own.
        let rows = total.min(jobs_max as usize) as u16;
        match Board::open(rows + 1) {
            Ok(board) => self.board = Some(board),
            Err(e) => {
                // A terminal that will not take raw mode still gets the pass,
                // one plain line per change, like a pipe would.
                eprintln!("note: could not draw the board ({e}); printing plain lines");
                self.tty = false;
            }
        }
    }

    fn board_transition(&mut self, job: &Job) {
        let label = board_label(job.pr);
        let Some(board) = &mut self.board else {
            return;
        };
        match job.state {
            // The next draw adds the row; nothing to insert.
            JobState::Running | JobState::Queued => {}
            JobState::Done | JobState::Failed | JobState::Timeout => {
                let width = board.width();
                let _ = board.println(finished_line(label, job, width));
                self.finished += 1;
            }
        }
    }

    /// Redraw the live area: one row per running review, the footer under
    /// them. Called on the pool's tick, which is also what turns the spinner.
    pub fn render(&mut self, jobs: &[Job]) {
        let Some(board) = &mut self.board else {
            return;
        };
        // Read once, not once per row: every row of a tick is drawn in the
        // same terminal, and each read is a size ioctl.
        let width = board.width();
        let spinner = SPINNER_FRAMES[self.frame % SPINNER_FRAMES.len()];
        self.frame += 1;
        let now = epoch_now();
        let mut running = 0usize;
        let mut finishing = 0usize;
        let mut queued = 0usize;
        let mut lines: Vec<Line<'static>> = Vec::new();
        self.live.clear();
        for job in jobs {
            match job.state {
                JobState::Running => {
                    if job.reaped {
                        finishing += 1;
                    } else {
                        running += 1;
                    }
                    self.live.push(job.pr);
                    lines.push(running_line(board_label(job.pr), job, width, spinner));
                    if self.expanded.contains(&job.pr) {
                        lines.extend(detail_lines(job, width, now));
                    }
                }
                JobState::Queued => queued += 1,
                _ => {}
            }
        }
        // A row that finished takes its details with it, so "any shown"
        // keeps meaning what the space key thinks it means.
        let live = &self.live;
        self.expanded.retain(|pr| live.contains(pr));
        let mut msg = format!("{running} running");
        if finishing > 0 {
            msg.push_str(&format!(" · {finishing} finishing"));
        }
        if queued > 0 {
            msg.push_str(&format!(" · {queued} queued"));
        }
        let hint = if self.expanded.is_empty() { "space details · q stop" } else { "space hide · q stop" };
        lines.push(footer_line(self.finished, self.total, &msg, hint, width));
        let _ = board.ensure_height(lines.len() as u16);
        let _ = board.draw(&lines);
    }

    /// What the keys pressed since the last tick ask for. The ones that only
    /// change what the board shows are applied here; the rest are handed
    /// back for the pass to act on. Nothing off a TTY: there is no board to
    /// press a key at.
    pub fn poll_input(&mut self) -> Vec<Action> {
        let Some(board) = &self.board else {
            return Vec::new();
        };
        let actions: Vec<Action> = board
            .events()
            .into_iter()
            .filter_map(|e| match e {
                Event::Key(key) => board::key_to_action(key),
                // The next draw redraws at the new size; nothing to decide.
                _ => None,
            })
            .collect();
        for action in &actions {
            self.apply(*action);
        }
        actions
    }

    /// One key's effect on what the board shows.
    fn apply(&mut self, action: Action) {
        match action {
            Action::ToggleAll => {
                if self.expanded.is_empty() {
                    self.expanded = self.live.iter().copied().collect();
                } else {
                    self.expanded.clear();
                }
            }
            Action::Toggle(n) => {
                if let Some(&pr) = n.checked_sub(1).and_then(|i| self.live.get(i))
                    && !self.expanded.remove(&pr)
                {
                    self.expanded.insert(pr);
                }
            }
            Action::Collapse => self.expanded.clear(),
            Action::Stop => {}
        }
    }

    /// Tear the board down, leaving only the permanent result lines. Safe to
    /// call twice: the interrupt path and the normal end both come through.
    /// This is also where raw mode ends, so it runs before anything else
    /// prints.
    pub fn end_pass(&mut self) {
        if let Some(board) = self.board.take() {
            board.close();
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
fn short_title(title: &str, width: usize) -> String {
    let clean = crate::report::sanitize_for_display(title);
    console::truncate_str(&clean, width, "…").to_string()
}

/// What a board row calls a PR: plain text, never the hyperlinked label the
/// summary uses.
///
/// The board is the one place a row is measured and redrawn in place, and a
/// hyperlink is forty-odd bytes that draw five columns. The first board
/// measured them as bytes, believed every linked row wrapped, and climbed the
/// screen overwriting scrollback. The current one measures spans correctly,
/// but the rule stays: the summary is where a `#N` links, and the board says
/// the number plain. Every board call site goes through here so the links
/// cannot come back one site at a time.
fn board_label(pr: u64) -> String {
    format!("#{pr}")
}

/// What is left for the title once the parts that must survive have been paid
/// for. `fixed` is measured by the caller from the strings it will actually
/// draw, rather than estimated from a constant, because the parts vary: a
/// seven-digit PR number and "rechecking 1h05m" cost eight columns more than
/// "#123" and "reviewing 3s".
///
/// Zero means the row cannot afford a title at all.
fn title_budget(width: usize, fixed: usize) -> usize {
    let left = width.saturating_sub(fixed);
    if left < TITLE_FLOOR { 0 } else { TITLE_WIDTH.min(left) }
}

/// Cut a plain string to the width it is drawn in. For the parts of the
/// board that are not rows: a note, the footer's message.
fn fit_str(line: &str, width: usize) -> String {
    // console::truncate_str returns the ellipsis itself at width 0, which is
    // one column and so still overruns. A width this small has nothing to say
    // anyway.
    if width == 0 {
        return String::new();
    }
    console::truncate_str(line, width, "…").to_string()
}

/// Cut a rendered row to the width it is drawn in. This is a backstop, not
/// the mechanism: the row builders size the title so it never fires. It exists
/// for the row too narrow to hold even its fixed parts, where something has to
/// give and there is nothing left to choose.
///
/// The cut keeps every span's style up to the column it stops at, and ends
/// in an ellipsis styled like the span it cut.
fn fit(line: Line<'static>, width: usize) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }
    if line.width() <= width {
        return line;
    }
    // Measured on the plain text, so a wide character at the boundary is
    // counted the way the terminal draws it; then the same number of
    // characters is taken back out of the spans.
    let plain: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let cut = console::truncate_str(&plain, width, "…");
    let mut keep = cut.chars().count().saturating_sub(1);
    let mut spans = Vec::new();
    for span in line.spans {
        let chars = span.content.chars().count();
        if chars <= keep {
            keep -= chars;
            spans.push(span);
            continue;
        }
        let head: String = span.content.chars().take(keep).collect();
        spans.push(Span::styled(format!("{head}…"), span.style));
        break;
    }
    Line::from(spans)
}

/// Who opened it and what it is called, in the width the board has. A row
/// that says only "#9" makes you go and look up whose work you are about to
/// spend money reviewing.
fn who_and_what(job: &Job, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if job.author.is_empty() {
        return short_title(&job.title, width);
    }
    short_title(&format!("@{} {}", job.author, job.title), width)
}

/// A row's parts joined with single spaces, skipping any that draws nothing --
/// a row that could not afford a title must not show where it would have been.
fn join_spans(parts: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for part in parts.into_iter().filter(|p| p.width() > 0) {
        if !out.is_empty() {
            out.push(Span::raw(" "));
        }
        out.push(part);
    }
    out
}

/// The width of a string as the terminal will draw it.
fn cols(s: &str) -> usize {
    console::measure_text_width(s)
}

/// What a running row draws ahead of its text: two spaces, the spinner, one
/// space. Built as spans and measured, never restated as a constant, so a
/// row cut to the terminal width is never drawn wider than the terminal.
fn spinner_lead(spinner: &'static str) -> Vec<Span<'static>> {
    vec![Span::raw("  "), Span::raw(spinner).magenta(), Span::raw(" ")]
}

/// `label` is the PR number as the caller wants it rendered. The board passes
/// plain text; see `board_label` for why it may not pass a hyperlink.
fn running_line(label: String, job: &Job, width: usize, spinner: &'static str) -> Line<'static> {
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
    let status = format!("· {verb} {}", fmt_dur(secs));
    let lead = spinner_lead(spinner);
    let reserve: usize = lead.iter().map(Span::width).sum();
    // Two single spaces join the three parts; an absent title takes its space
    // with it, which join_spans handles.
    let fixed = reserve + cols(&label) + cols(&status) + 2;
    // The tool the review is in, when the row can afford it and a title too.
    // It is the least important part: it goes before the title shrinks to
    // nothing, and long before the clock.
    let tool = job
        .activity
        .current_tool()
        .map(|t| format!("· {t}"))
        .filter(|t| title_budget(width, fixed + cols(t) + 1) > 0)
        .unwrap_or_default();
    let fixed = if tool.is_empty() { fixed } else { fixed + cols(&tool) + 1 };
    let who = who_and_what(job, title_budget(width, fixed));
    let mut spans = lead;
    spans.extend(join_spans(vec![
        Span::from(label).cyan().bold(),
        Span::from(who).dim(),
        Span::from(status).magenta(),
        Span::from(tool).dim(),
    ]));
    fit(Line::from(spans), width)
}

/// Seconds since the epoch, for the ages in a details block.
fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// How long ago an event was, in the clock's own words, or nothing when the
/// event did not say.
fn age(at: Option<i64>, now: i64) -> String {
    match at {
        Some(at) => format!("{} ago", fmt_dur(now.saturating_sub(at).max(0) as u64)),
        None => String::new(),
    }
}

/// The lines an expanded row shows under itself: what is being followed, how
/// much has happened, and the last few things the reviewer did, oldest
/// first, each cut to the width.
fn detail_lines(job: &Job, width: usize, now: i64) -> Vec<Line<'static>> {
    let tail = &job.activity;
    let mut lines = vec![detail_line(vec![Span::from(tail.source_label()).dim()], width)];
    let counts = if tail.turns == 0 {
        "waiting for the first turn".to_string()
    } else {
        format!("{} · {}", count(tail.turns as usize, "turn"), count(tail.tool_calls as usize, "tool call"))
    };
    lines.push(detail_line(vec![Span::from(counts).dim()], width));
    let shown = tail.events.len().min(DETAIL_LINES - lines.len());
    for event in tail.events.iter().skip(tail.events.len() - shown) {
        let when = format!("{:>9}", age(event.at, now));
        let name = event.tool.clone().unwrap_or_else(|| "said".to_string());
        lines.push(detail_line(
            vec![
                Span::from(when).dim(),
                Span::raw("  "),
                Span::from(format!("{name:<6}")).cyan(),
                Span::raw(" "),
                Span::from(event.what.clone()),
            ],
            width,
        ));
    }
    lines
}

fn detail_line(mut spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    spans.insert(0, Span::raw(DETAIL_INDENT));
    fit(Line::from(spans), width)
}

/// The permanent line a finished review leaves on the board.
fn finished_line(label: String, job: &Job, width: usize) -> Line<'static> {
    let (mark, headline) = match job.state {
        JobState::Done => {
            let word = match job.verdict.as_deref() {
                Some("approved") => Span::raw("approved").green().bold(),
                Some("changes requested") => Span::raw("changes requested").yellow(),
                Some("commented") => Span::raw("commented").cyan(),
                Some(other) => Span::from(other.to_string()),
                None => Span::raw("done").green(),
            };
            (Span::raw("✓").green().bold(), word)
        }
        JobState::Timeout => (Span::raw("✗").yellow().bold(), Span::raw("timed out").yellow()),
        _ => (
            Span::raw("✗").red().bold(),
            Span::from(format!("failed ({})", job.outcome())).red(),
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
    let extras = format!("· {}", extras.join(" · "));
    // "  " + mark + the three joining spaces, plus the parts themselves.
    let fixed = 2 + mark.width() + cols(&label) + headline.width() + cols(&extras) + 4;
    let who = who_and_what(job, title_budget(width, fixed));
    let mut spans = vec![Span::raw("  "), mark, Span::raw(" ")];
    spans.extend(join_spans(vec![
        Span::from(label).cyan().bold(),
        headline,
        Span::from(extras).dim(),
        Span::from(who).dim(),
    ]));
    fit(Line::from(spans), width)
}

/// The footer's gauge: GAUGE_WIDTH columns, the done part solid, a tip on
/// the boundary while the pass is unfinished, the rest a faint line.
fn gauge(pos: usize, len: usize) -> Vec<Span<'static>> {
    let full = (pos * GAUGE_WIDTH).checked_div(len).unwrap_or(GAUGE_WIDTH).min(GAUGE_WIDTH);
    let tip = usize::from(full < GAUGE_WIDTH);
    vec![
        Span::from("━".repeat(full)).cyan(),
        Span::from("╸".repeat(tip)).cyan(),
        Span::from("─".repeat(GAUGE_WIDTH - full - tip)).fg(Ink::Indexed(238)),
    ]
}

/// The line under the rows: the gauge, the counts, what the rows are doing
/// in words, and at the right edge, when there is room, the keys.
fn footer_line(pos: usize, len: usize, msg: &str, hint: &str, width: usize) -> Line<'static> {
    let mut lead = vec![Span::raw("  ")];
    lead.extend(gauge(pos, len));
    lead.push(Span::raw(" "));
    lead.push(Span::from(format!("{pos}/{len}")));
    lead.push(Span::raw(" "));
    let reserve: usize = lead.iter().map(Span::width).sum();
    let msg = fit_str(msg, width.saturating_sub(reserve));
    let used = reserve + cols(&msg);
    let mut spans = lead;
    spans.push(Span::from(msg).dim());
    // Two columns of air at least, or the hint is not worth the space.
    if let Some(gap) = width.checked_sub(used + cols(hint) + 2) {
        spans.push(Span::raw(" ".repeat(gap + 2)));
        spans.push(Span::from(hint.to_string()).dim());
    }
    fit(Line::from(spans), width)
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
    use crate::board::ASSUMED_WIDTH;
    use crate::job::Job;
    use crate::report::parse_trailer;

    /// The text of a line, styles dropped: what the row says.
    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

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
        let line = text(&running_line("#9".into(), &job, ASSUMED_WIDTH, "⠋"));
        assert!(line.contains("finishing"));
        assert!(line.contains("4m12s"));
        job.reaped = false;
        assert!(text(&running_line("#9".into(), &job, ASSUMED_WIDTH, "⠋")).contains("reviewing"));
        // A resumed review says so: it is the difference between paying for a
        // first look and paying for a second one.
        job.resume = true;
        assert!(text(&running_line("#9".into(), &job, ASSUMED_WIDTH, "⠋")).contains("rechecking"));
    }

    #[test]
    fn a_board_row_says_who_opened_it() {
        let mut job = Job::new(9);
        job.title = "Add retry logic".into();
        job.author = "alice".into();
        assert_eq!(who_and_what(&job, TITLE_WIDTH), "@alice Add retry logic");
        // An author the fetch never learned leaves the title alone rather
        // than printing a bare "@".
        job.author = String::new();
        assert_eq!(who_and_what(&job, TITLE_WIDTH), "Add retry logic");
    }

    #[test]
    fn a_board_row_never_carries_a_hyperlink() {
        // The summary table links its PR numbers and the board does not; see
        // `board_label` for why the asymmetry is load-bearing. Asserting on
        // `board_label` rather than on a literal is what stops the links
        // returning through a call site no test covers.
        assert_eq!(board_label(9), "#9");
        assert!(!board_label(9).contains('\x1b'));
        let job = Job::new(9);
        for line in [running_line(board_label(9), &job, ASSUMED_WIDTH, "⠋"), finished_line(board_label(9), &job, ASSUMED_WIDTH)] {
            let plain = text(&line);
            assert!(plain.contains("#9"), "the row still names the PR: {plain:?}");
            assert!(!plain.contains('\x1b'), "no escapes on the board: {plain:?}");
        }
    }

    #[test]
    fn the_title_is_the_only_part_of_a_row_that_shrinks() {
        // The budget is what a row can spend on a title after the parts that
        // must survive are paid for. Pinned at real widths, because the
        // terminal under `cargo test` is always the same one and a test that
        // only restates the min/max clamps cannot fail.
        assert_eq!(title_budget(200, 30), TITLE_WIDTH);
        assert_eq!(title_budget(80, 30), 50);
        assert_eq!(title_budget(60, 30), 30);
        // Too tight for a title worth the name: the row drops it rather than
        // shaving it, and keeps the number, the verb and the clock.
        assert_eq!(title_budget(45, 30), 0);
        assert_eq!(title_budget(10, 30), 0);
        assert_eq!(who_and_what(&Job::new(9), 0), "");
    }

    #[test]
    fn a_row_fits_the_width_it_is_given() {
        let mut job = Job::new(1234567);
        job.author = "domleboss97".into();
        job.title = "ENG-2304: add a protocol-neutral payment credential format".into();
        job.resume = true;

        // Widths are given, not read, so this pins the arithmetic at every
        // shape of terminal rather than at whichever one cargo test ran in --
        // including the degenerate ones, where a naive cut would hand back a
        // one-column ellipsis for a zero-column budget.
        for width in [200, 120, 80, 60, 45, 30, 20, 6, 4, 1, 0] {
            let run = running_line(board_label(job.pr), &job, width, "⠋");
            assert!(run.width() <= width, "running row at {width}: {} > {width}", run.width());
            let fin = finished_line(board_label(job.pr), &job, width);
            assert!(fin.width() <= width, "finished row at {width}: {} > {width}", fin.width());
            let foot = footer_line(1, 2, "1 running · 3 queued", "space details · q stop", width);
            assert!(foot.width() <= width, "footer at {width}: {} > {width}", foot.width());
            for line in detail_lines(&job, width, 0) {
                assert!(line.width() <= width, "detail line at {width}: {} > {width}", line.width());
            }
        }
        // Down to the width where the title stops fitting, the row keeps the
        // parts that say the review is alive.
        let run = text(&running_line(board_label(job.pr), &job, 45, "⠋"));
        assert!(run.contains("#1234567") && run.contains("rechecking"), "got {run:?}");
    }

    fn busy_job() -> Job {
        let mut job = Job::new(9);
        job.author = "alice".into();
        job.title = "Add retry logic".into();
        job.activity = crate::activity::Tail::transcript_at("/nonexistent".into(), 0);
        let line = |block: serde_json::Value| {
            serde_json::json!({"type": "assistant", "timestamp": "2026-09-01T11:00:05.000Z", "message": {"content": [block]}}).to_string()
        };
        let mut text = String::new();
        text.push_str(&line(serde_json::json!({"type": "text", "text": "Reading the diff."})));
        text.push('\n');
        text.push_str(&line(serde_json::json!({"type": "tool_use", "name": "Bash", "input": {"command": "cargo test --quiet"}})));
        text.push('\n');
        job.activity.feed(&text);
        job
    }

    #[test]
    fn a_running_row_says_which_tool_it_is_in_when_there_is_room() {
        let job = busy_job();
        let wide = text(&running_line("#9".into(), &job, 120, "⠋"));
        assert!(wide.ends_with("· Bash"), "got {wide:?}");
        assert!(wide.contains("@alice Add retry logic"));
        // The tool goes before the title would have to go, and the clock is
        // never what pays for it.
        let tight = text(&running_line("#9".into(), &job, 40, "⠋"));
        assert!(!tight.contains("Bash"), "got {tight:?}");
        assert!(tight.contains("reviewing"), "got {tight:?}");
        // A review that just said something is not in a tool.
        let mut job = job;
        job.activity.feed(&format!(
            "{}\n",
            serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Done."}]}})
        ));
        assert!(!text(&running_line("#9".into(), &job, 120, "⠋")).contains("Bash"));
    }

    #[test]
    fn an_expanded_row_shows_what_the_review_is_doing() {
        let job = busy_job();
        let lines: Vec<String> = detail_lines(&job, 120, 1_788_260_417).iter().map(text).collect();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!(lines[0].starts_with(DETAIL_INDENT), "indented under the row: {:?}", lines[0]);
        assert!(lines[0].contains("waiting for its transcript"), "{:?}", lines[0]);
        assert_eq!(lines[1].trim(), "2 turns · 1 tool call");
        assert!(lines[2].contains("12s ago") && lines[2].contains("said") && lines[2].contains("Reading the diff."), "{:?}", lines[2]);
        assert!(lines[3].contains("Bash") && lines[3].contains("cargo test --quiet"), "{:?}", lines[3]);
        // Nothing followed yet: the block says so and stays short.
        let idle = detail_lines(&Job::new(8), 120, 0);
        let idle: Vec<String> = idle.iter().map(text).collect();
        assert_eq!(idle.len(), 2, "{idle:?}");
        assert!(idle[0].contains("not started") && idle[1].contains("waiting for the first turn"), "{idle:?}");
        // The block never grows past its cap, whatever the tail holds.
        let mut job = busy_job();
        for i in 0..20 {
            job.activity.feed(&format!(
                "{}\n",
                serde_json::json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Read", "input": {"file_path": format!("f{i}.rs")}}]}})
            ));
        }
        assert_eq!(detail_lines(&job, 120, 0).len(), DETAIL_LINES);
    }

    #[test]
    fn the_footer_names_the_keys_when_there_is_room() {
        let wide = text(&footer_line(0, 2, "2 running", "space details · q stop", 100));
        assert!(wide.ends_with("space details · q stop"), "got {wide:?}");
        assert_eq!(wide.len(), wide.trim_end().len(), "the hint sits at the right edge");
        assert_eq!(console::measure_text_width(&wide), 100);
        let narrow = text(&footer_line(0, 2, "2 running", "space details · q stop", 45));
        assert!(!narrow.contains("space"), "got {narrow:?}");
        assert!(narrow.contains("2 running"));
    }

    #[test]
    fn keys_change_what_the_board_shows() {
        let mut ui = self::ui(true, None);
        ui.live = vec![9, 8];
        ui.apply(Action::ToggleAll);
        assert_eq!(ui.expanded.len(), 2);
        ui.apply(Action::ToggleAll);
        assert!(ui.expanded.is_empty());
        ui.apply(Action::Toggle(2));
        assert!(ui.expanded.contains(&8) && !ui.expanded.contains(&9));
        ui.apply(Action::Toggle(2));
        assert!(ui.expanded.is_empty());
        // A digit past the last row names nothing, and zero is not a row.
        ui.apply(Action::Toggle(3));
        ui.apply(Action::Toggle(0));
        assert!(ui.expanded.is_empty());
        ui.apply(Action::Toggle(1));
        ui.apply(Action::Collapse);
        assert!(ui.expanded.is_empty());
        ui.apply(Action::Stop);
    }

    #[test]
    fn a_running_row_starts_with_the_spinner() {
        // The row builder pays for its own lead. If the lead ever changed
        // shape, the width arithmetic in running_line would be paying for the
        // wrong thing, so the lead is pinned here.
        let lead = spinner_lead("⠋");
        assert_eq!(lead.iter().map(Span::width).sum::<usize>(), 4);
        let mut job = Job::new(9);
        job.title = "t".into();
        assert!(text(&running_line("#9".into(), &job, ASSUMED_WIDTH, "⠋")).starts_with("  ⠋ #9"));
    }

    #[test]
    fn the_gauge_is_always_the_same_width() {
        for (pos, len) in [(0, 2), (1, 2), (2, 2), (0, 0), (3, 7)] {
            let width: usize = gauge(pos, len).iter().map(Span::width).sum();
            assert_eq!(width, GAUGE_WIDTH, "gauge at {pos}/{len}");
        }
        // A finished pass is a solid bar with no tip.
        let full: String = gauge(2, 2).iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(full, "━".repeat(GAUGE_WIDTH));
        let half: String = gauge(1, 2).iter().map(|s| s.content.as_ref()).collect();
        assert!(half.starts_with("━━━━━━━━━━━━╸"), "got {half:?}");
    }

    #[test]
    fn a_row_with_no_room_for_a_title_leaves_no_gap() {
        // An empty title is a span that draws nothing; it must take its
        // joining space with it.
        let joined = join_spans(vec![Span::raw("#9"), Span::raw("").dim(), Span::raw("· reviewing 3s")]);
        let plain: String = joined.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(plain, "#9 · reviewing 3s");
    }

    #[test]
    fn fit_cuts_to_the_width_it_is_given() {
        assert_eq!(fit(Line::from("hello"), 80).width(), 5);
        let cut = fit(Line::from("hello world, this is long"), 10);
        assert_eq!(cut.width(), 10);
        assert!(text(&cut).ends_with('…'));
        // Colour is not width: a styled line is cut by what it draws, and the
        // cut keeps the style of the span it fell in.
        let styled = Line::from(vec![Span::raw("hello ").green(), Span::raw("world").red()]);
        let cut = fit(styled, 8);
        assert_eq!(cut.width(), 8);
        assert_eq!(text(&cut), "hello w…");
        assert_eq!(cut.spans.len(), 2);
        assert_eq!(cut.spans[1].style, Span::raw("").red().style);
        // Nothing fits in nothing, and not an ellipsis either.
        assert_eq!(fit(Line::from("hello"), 0).width(), 0);
        assert_eq!(fit_str("hello", 0), "");
    }

    #[test]
    fn titles_lose_their_control_bytes() {
        assert_eq!(short_title("Add \x1b[31mretry\x1b[0m logic", TITLE_WIDTH), "Add [31mretry[0m logic");
        assert_eq!(short_title("plain title", TITLE_WIDTH), "plain title");
        // Bidi overrides and zero-width characters reorder or hide text
        // without being C0 controls; they must go too.
        assert_eq!(short_title("fix\u{202E}cod.exe", TITLE_WIDTH), "fixcod.exe");
        assert_eq!(short_title("a\u{200B}b\u{FEFF}c", TITLE_WIDTH), "abc");
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

    fn ui(tty: bool, pr_url_base: Option<&str>) -> Ui {
        Ui {
            tty,
            pr_url_base: pr_url_base.map(String::from),
            board: None,
            frame: 0,
            finished: 0,
            total: 0,
            expanded: HashSet::new(),
            live: Vec::new(),
        }
    }

    fn linked_ui() -> Ui {
        ui(true, Some("https://github.com/acme/widgets/pull"))
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
        let plain = self::ui(false, None);
        assert_eq!(plain.pr_label(9), "#9");
    }

    #[test]
    fn a_linked_table_still_lines_up() {
        // The whole risk of an in-cell hyperlink: 40-odd invisible bytes that
        // a naive width count would pad around.
        let out = linked_ui().results_table(&[done_job(9), done_job(123)]).to_string();
        let widths: Vec<usize> =
            out.lines().map(console::measure_text_width).collect();
        let plain: Vec<usize> = ui(false, None)
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
        let out = ui(false, None)
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
