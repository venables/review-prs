//! Everything the user reads. The strings here are a contract: the bash test
//! suite greps for them verbatim, and so do people's eyes -- keep them
//! byte-identical across refactors.

use crate::job::{Job, JobState};
use std::io::IsTerminal;

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

pub struct Ui {
    pub tty: bool,
    notes: Vec<String>,
    pub rendered_lines: usize,
    pub window_max: usize,
    pub spin: usize,
}

impl Ui {
    pub fn new() -> Ui {
        Ui {
            tty: std::io::stdout().is_terminal(),
            notes: Vec::new(),
            rendered_lines: 0,
            window_max: 0,
            spin: 0,
        }
    }

    /// Notes raised while the progress block is on screen have nowhere good
    /// to go: printing one scrolls the block and leaves the redraw walking
    /// over the wrong rows. Hold them and flush after the pass. Without a TTY
    /// there is no block to disturb.
    pub fn queue_note(&mut self, note: String) {
        if self.tty {
            self.notes.push(note);
        } else {
            eprintln!("{note}");
        }
    }

    pub fn flush_notes(&mut self) {
        for n in self.notes.drain(..) {
            eprintln!("{n}");
        }
    }

    pub fn pass_header(&self, total: usize, jobs_max: u32, pass_dir: &std::path::Path) {
        println!("reviewing {total} PR(s), {jobs_max} at a time");
        println!("logs: {}\n", pass_dir.display());
    }

    /// Without a TTY -- a cron run, or output piped to a log -- there is no
    /// cursor to move, so the in-place block is replaced by one line per
    /// state change.
    pub fn note_transition(&self, job: &Job) {
        if self.tty {
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

    /// Reset per-pass display state and measure the window once: a resize
    /// mid-pass is rare, and measuring per redraw would fork a `tput` five
    /// times a second.
    pub fn begin_pass(&mut self) {
        self.rendered_lines = 0;
        self.spin = 0;
        self.window_max = window_rows();
    }

    /// The live TTY progress block, redrawn in place. The block is windowed:
    /// the cursor cannot climb past the top of the screen, so a queue taller
    /// than the terminal would otherwise redraw onto rows it does not own.
    /// A busy repo makes that ordinary -- the PR query fetches 50, and --auto
    /// queues every NEW/UPDATED one of them.
    pub fn render(&mut self, jobs: &[Job]) {
        if !self.tty {
            return;
        }
        let total = jobs.len();
        let (start, end) = if total > self.window_max {
            // Everything above the first unfinished row is done and scrolls
            // away first.
            let first_unfinished = jobs
                .iter()
                .position(|j| !matches!(j.state, JobState::Done | JobState::Failed | JobState::Timeout))
                .unwrap_or(total);
            let start = first_unfinished.min(total - self.window_max);
            (start, start + self.window_max)
        } else {
            (0, total)
        };

        let mut out = String::new();
        if self.rendered_lines > 0 {
            // Up, then erase everything below: the block can be one line
            // shorter than last time, and clearing only the rows we rewrite
            // would leave the last one standing for the rest of the pass.
            out.push_str(&format!("\x1b[{}A\x1b[J", self.rendered_lines));
        }
        let mut shown = 0;
        if start > 0 {
            out.push_str(&format!(
                "\x1b[2K  ... {start} finished, scrolled off (all of them are in the summary)\n"
            ));
            shown += 1;
        }
        const SPIN: [char; 4] = ['|', '/', '-', '\\'];
        for job in &jobs[start..end] {
            let (mark, label, elapsed) = match job.state {
                JobState::Queued => (' ', "queued".to_string(), String::new()),
                JobState::Running => (
                    SPIN[self.spin % SPIN.len()],
                    "reviewing".to_string(),
                    fmt_dur(job.started.map(|s| s.elapsed().as_secs()).unwrap_or(0)),
                ),
                JobState::Done => ('+', "done".to_string(), fmt_dur(job.elapsed_secs)),
                JobState::Failed => (
                    'x',
                    format!("failed ({})", job.outcome()),
                    fmt_dur(job.elapsed_secs),
                ),
                JobState::Timeout => ('x', "timed out".to_string(), fmt_dur(job.elapsed_secs)),
            };
            out.push_str(&format!("\x1b[2K  {mark}  #{:<5} {label:<24} {elapsed:>8}\n", job.pr));
            shown += 1;
        }
        if end < total {
            out.push_str(&format!("\x1b[2K  ... {} more waiting\n", total - end));
            shown += 1;
        }
        print!("{out}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        self.rendered_lines = shown;
        self.spin = (self.spin + 1) % SPIN.len();
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
        let mut rows: Vec<[String; 5]> = vec![[
            "PR".into(),
            "RESULT".into(),
            "TIME".into(),
            "COST".into(),
            "SESSION".into(),
        ]];
        for job in jobs {
            let result = match job.state {
                JobState::Done => "done".to_string(),
                JobState::Timeout => "timed out".to_string(),
                JobState::Failed => format!("failed ({})", job.outcome()),
                JobState::Queued => "queued".to_string(),
                JobState::Running => "running".to_string(),
            };
            rows.push([
                format!("#{}", job.pr),
                result,
                fmt_dur(job.elapsed_secs),
                cost_str(job.cost),
                job.sid.clone().unwrap_or_else(|| "-".into()),
            ]);
        }
        println!();
        print!("{}", align(&rows));
        println!("\nlogs: {}", pass_dir.display());
        println!("reopen any review with: claude --resume <SESSION>");
    }
}

/// How many rows the progress block may occupy: the terminal height minus
/// room for the two header lines, the elision line, and the prompt.
fn window_rows() -> usize {
    let lines = std::process::Command::new("tput")
        .arg("lines")
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse::<usize>().ok())
        .unwrap_or(24);
    lines.saturating_sub(5).max(3)
}

/// A panic must not leave the terminal without its cursor.
impl Drop for Ui {
    fn drop(&mut self) {
        self.show_cursor();
    }
}

/// What `column -t` did, natively: pad each column to its widest cell with a
/// two-space gutter, last column ragged. `column` lives in util-linux and the
/// boxes this tool is built for -- slim CI images -- routinely ship without
/// it; a summary must never die on formatting.
pub fn align(rows: &[[String; 5]]) -> String {
    let mut widths = [0usize; 5];
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }
    let mut out = String::new();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i == 4 {
                line.push_str(cell);
            } else {
                line.push_str(&format!("{cell:<width$}  ", width = widths[i]));
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

    #[test]
    fn durations_are_byte_identical_to_bash() {
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
            ["PR".into(), "RESULT".into(), "TIME".into(), "COST".into(), "SESSION".into()],
            ["#9".into(), "done".into(), "4m12s".into(), "$0.42".into(), "abc".into()],
            ["#123".into(), "failed (no result)".into(), "3s".into(), "-".into(), "-".into()],
        ];
        let out = align(&rows);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "PR    RESULT              TIME   COST   SESSION");
        assert_eq!(lines[1], "#9    done                4m12s  $0.42  abc");
        assert_eq!(lines[2], "#123  failed (no result)  3s     -      -");
    }

    #[test]
    fn transition_outcomes_render_in_the_failed_line() {
        let mut job = Job::new(9);
        job.exit_code = None;
        assert_eq!(job.outcome(), "no result");
        job.exit_code = Some(10);
        assert_eq!(format!("FAILED  #{} ({}, {})", job.pr, job.outcome(), fmt_dur(3)), "FAILED  #9 (exit 10, 3s)");
    }
}
