//! Running the panel: spawn every panelist at once, print each report the
//! moment it lands, and retry the failures that are worth retrying.
//!
//! No bounded pool here, unlike autoreview's. A panel is three to six
//! processes decided by what is installed, not a queue of unknown length, and
//! holding one back would only make the slowest panelist later. Everything
//! else about supervising a child is the same job autoreview already does, so
//! this uses its `stop_group` rather than inventing a second way to kill one.

use crate::panel::cli::Config;
use crate::panel::panelist::{self, Panelist};
use crate::pool::stop_group;
use crate::status::{Status, step};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// dash-p's exit code for a run that hit its own timeout.
const EXIT_TIMEOUT: i32 = 20;

/// How long past dash-p's own timeout we wait before killing the group
/// ourselves. dash-p enforces the real cap; this covers its one known hole,
/// the same one job.rs documents -- an unbounded wait after the agent closes
/// stdout without exiting. Without it, one wedged panelist hangs the run for
/// as long as the terminal stays open.
pub const GRACE_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    /// What the panelist said it was running, or "unknown".
    pub model: String,
    pub exit: Option<i32>,
    pub stdout: String,
    /// Some(reason) when the run was not clean. Not the same question as
    /// whether it produced a review: a panelist can write a complete report
    /// and still exit non-zero on the way out.
    pub failure: Option<String>,
    pub elapsed_secs: u64,
    pub retried: bool,
}

impl Outcome {
    /// Did this panelist actually review anything? This, not `failure`, is
    /// what decides whether there is something to synthesize -- a report that
    /// arrived with a bad exit status is still a report, and discarding it
    /// would throw away the work and the API spend both.
    pub fn answered(&self) -> bool {
        !self.stdout.trim().is_empty()
    }

    pub fn clean(&self) -> bool {
        self.failure.is_none()
    }
}

/// Why a finished panelist's run was not clean, in the words a reader needs.
fn failure_reason(exit: Option<i32>, stdout: &str, stderr: &str) -> Option<String> {
    let detail = || {
        match stderr.lines().rfind(|l| !l.trim().is_empty()) {
            Some(line) => format!(": {}", line.trim()),
            None => " and no error detail captured".into(),
        }
    };
    let empty = stdout.trim().is_empty();
    match exit {
        Some(EXIT_TIMEOUT) => Some("timed out".into()),
        Some(0) if empty => Some(format!("exited 0 but produced no output{}", detail())),
        Some(0) => None,
        Some(code) if empty => Some(format!("no output (exit {code}){}", detail())),
        // Output plus a bad status: the words are still worth reading, and the
        // reader should know which half to trust.
        Some(code) => Some(format!("exited {code} after producing output")),
        None => Some("killed before it finished".into()),
    }
}

/// Worth another try? A transient refusal -- a quota blip, an auth hiccup, a
/// backend that returned nothing at all -- usually succeeds on a second call.
/// A timeout does not: it would spend the same wall clock again and time out
/// again, which on a six-panelist run is the difference between minutes and
/// twice as many minutes.
fn worth_retrying(o: &Outcome) -> bool {
    !o.retried && !o.answered() && o.exit != Some(EXIT_TIMEOUT)
}

struct Running {
    idx: usize,
    child: Child,
    /// The child's pid doubles as its process-group id (process_group(0)), so
    /// stopping it is one killpg over everything it spawned.
    pgid: i32,
    /// When this panelist's work started, kept across a retry so the reported
    /// time is what the user actually waited.
    started: Instant,
    deadline: Instant,
    out_path: PathBuf,
    err_path: PathBuf,
    retried: bool,
    /// Set when we killed it, so its exit tells the right story.
    stopped: Option<&'static str>,
}

fn read_file(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

/// What every spawn in a run shares. Passed as one value so `spawn` takes the
/// things that vary between panelists and nothing else.
struct Run<'a> {
    cfg: &'a Config,
    dashp: &'a str,
    prompt_path: &'a Path,
    out_dir: &'a Path,
}

fn spawn(
    run: &Run,
    idx: usize,
    p: &Panelist,
    cwd: &Path,
    started: Instant,
    retried: bool,
) -> Result<Running> {
    let out_path = run.out_dir.join(format!("{}.out", p.id));
    let err_path = run.out_dir.join(format!("{}.err", p.id));
    // The prompt arrives on stdin, so dash-p reads it without it ever being
    // an argument. See panelist::dashp_args for why that matters.
    let stdin = File::open(run.prompt_path).context("opening the prompt file")?;
    let child = Command::new(run.dashp)
        .args(panelist::dashp_args(p, cwd, run.cfg.timeout_secs, run.cfg.isolated))
        .stdin(Stdio::from(stdin))
        .stdout(File::create(&out_path).context("creating a panelist's output file")?)
        .stderr(File::create(&err_path).context("creating a panelist's error file")?)
        // Its own process group, so stopping this panelist is one killpg over
        // everything it spawned -- no tree to walk, no reparenting race.
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawning dash-p for panelist '{}'", p.id))?;
    let pgid = child.id() as i32;
    Ok(Running {
        idx,
        child,
        pgid,
        started,
        deadline: Instant::now() + Duration::from_secs(run.cfg.timeout_secs + GRACE_SECS),
        out_path,
        err_path,
        retried,
        stopped: None,
    })
}

fn finish(r: &mut Running, p: &Panelist, exit: Option<i32>) -> Outcome {
    let stdout = read_file(&r.out_path);
    let stderr = read_file(&r.err_path);
    let failure = match r.stopped {
        Some(why) => Some(why.to_string()),
        None => failure_reason(exit, &stdout, &stderr),
    };
    Outcome {
        id: p.id.clone(),
        model: panelist::extract_model(&stdout).unwrap_or_else(|| "unknown".into()),
        exit,
        failure,
        stdout,
        elapsed_secs: r.started.elapsed().as_secs(),
        retried: r.retried,
    }
}

/// A panelist that never started. Not an early return: the panelists already
/// running would be abandoned, and the worktrees they are sitting in would be
/// force-removed underneath them on the way out.
fn never_ran(p: &Panelist, why: String, started: Instant) -> Outcome {
    Outcome {
        id: p.id.clone(),
        model: "unknown".into(),
        exit: None,
        stdout: String::new(),
        failure: Some(why),
        elapsed_secs: started.elapsed().as_secs(),
        retried: false,
    }
}

/// Print one panelist's report as soon as it lands. Waiting for the slowest
/// one is how a coordinator ends up staring at nothing for ten minutes while
/// three reviews sit finished on disk.
///
/// Suspended around the whole thing: the spinner draws on the last line of
/// the terminal, and so does this.
fn print_section(o: &Outcome, status: &Status) {
    status.suspend(|| print_section_now(o));
}

fn print_section_now(o: &Outcome) {
    let exit = o.exit.map(|c| c.to_string()).unwrap_or_else(|| "killed".into());
    // The model name is the panelist's own first line, so it is model output
    // too -- sanitized here and at the heartbeat below, or it is the one way
    // an escape sequence still reaches the terminal.
    let model = crate::report::sanitize_for_display(&o.model);
    println!("## {} / {} (exit {})", o.id, model, exit);
    println!();
    if let Some(reason) = &o.failure {
        // Sanitized like the body: the reason carries the last line of the
        // child's stderr, which is process output and can hold escapes.
        println!("FAILED: {}", crate::report::sanitize_for_display(reason));
        println!();
    }
    if o.answered() {
        // Sanitized: this is model output going straight to a terminal, and a
        // stray escape sequence or bidi override in it could repaint or
        // reorder the report around it.
        println!("{}", crate::report::sanitize_block(o.stdout.trim_end()));
        println!();
    }
    // stderr, not stdout: a heartbeat is progress, and progress must not land
    // in the middle of the report someone is piping to a file.
    eprintln!(
        "panel: {} ({}) done in {} (exit {})",
        o.id,
        model,
        crate::ui::fmt_dur(o.elapsed_secs),
        exit
    );
}

/// Run every panelist, streaming reports, and return what each contributed.
/// Order follows the panel list, not finishing order, so two runs of the same
/// panel are comparable.
#[allow(clippy::too_many_arguments)]
pub fn run(
    panelists: &[Panelist],
    cwds: &[PathBuf],
    cfg: &Config,
    dashp: &str,
    prompt_path: &Path,
    out_dir: &Path,
    interrupted: &Arc<AtomicBool>,
    status: &Status,
) -> Result<Vec<Outcome>> {
    let run = Run { cfg, dashp, prompt_path, out_dir };
    let mut running: Vec<Running> = Vec::new();
    let mut done: Vec<Outcome> = Vec::new();

    for (idx, (p, cwd)) in panelists.iter().zip(cwds).enumerate() {
        // Checked before each spawn as well as in the poll loop: a signal that
        // arrived while the worktrees were being made would otherwise start
        // every panelist anyway and only then notice.
        if interrupted.load(Ordering::Relaxed) {
            eprintln!("panel: interrupted before {} started", p.id);
            let outcome = never_ran(p, "interrupted before it started".into(), Instant::now());
            // Printed like any other panelist: stdout is the whole panel, and
            // a reader counting sections should not have to check stderr to
            // learn that one is missing.
            print_section(&outcome, status);
            done.push(outcome);
            continue;
        }
        let started = Instant::now();
        match spawn(&run, idx, p, cwd, started, false) {
            Ok(r) => {
                eprintln!("panel: {} started (cwd={})", p.id, cwd.display());
                running.push(r);
            }
            Err(e) => {
                // One panelist that cannot start costs one voice, not the run.
                eprintln!("panel: {} could not start: {e:#}", p.id);
                let outcome = never_ran(p, format!("could not start: {e:#}"), started);
                print_section(&outcome, status);
                done.push(outcome);
            }
        }
    }

    while !running.is_empty() {
        // A signal stops the panelists first, then falls out of the loop so
        // the worktrees are removed on the way past -- they are registered in
        // the user's real repository, and leaving them is worse than losing an
        // unfinished review.
        if interrupted.load(Ordering::Relaxed) {
            eprintln!("\npanel: interrupted; stopping {}", crate::ui::count(running.len(), "panelist"));
            for r in &mut running {
                // try_wait first, as the synthesis poll does: a panelist that
                // already exited with a full report is finished, and marking
                // it "interrupted" would hide its real exit code and print
                // FAILED over a review that succeeded.
                let already = matches!(r.child.try_wait(), Ok(Some(_)));
                if !already {
                    stop_group(r.pgid);
                    r.stopped = Some("interrupted");
                }
                let exit = r.child.wait().ok().and_then(|s| s.code());
                let outcome = finish(r, &panelists[r.idx], exit);
                // Printed like any other outcome. A panelist stopped mid-run
                // may still have written most of a review, and dropping it
                // from stdout would throw away work already paid for.
                print_section(&outcome, status);
                done.push(outcome);
            }
            break;
        }

        let mut still = Vec::new();
        for mut r in running {
            let finished = match r.child.try_wait() {
                Ok(Some(code)) => Some(code.code()),
                Ok(None) if Instant::now() >= r.deadline => {
                    // dash-p should have enforced its own timeout by now. It
                    // did not, so the group goes.
                    stop_group(r.pgid);
                    r.stopped = Some("timed out and had to be killed");
                    Some(r.child.wait().ok().and_then(|s| s.code()))
                }
                Ok(None) => None,
                Err(e) => {
                    // Unwaitable, so it will never be reaped by the poll -- but
                    // it is still running, and its worktree is about to be
                    // removed underneath it.
                    stop_group(r.pgid);
                    // Reaped after the kill, the same way the synthesis does
                    // it, or it stays a zombie until this process exits.
                    let code = r.child.wait().ok().and_then(|s| s.code());
                    r.stopped = Some("could not be waited on");
                    eprintln!("panel: {} could not be waited on: {e}", panelists[r.idx].id);
                    Some(code)
                }
            };
            let Some(exit) = finished else {
                still.push(r);
                continue;
            };

            let p = &panelists[r.idx];
            let outcome = finish(&mut r, p, exit);
            if worth_retrying(&outcome) && r.stopped.is_none() {
                eprintln!(
                    "panel: {} produced nothing ({}); trying once more",
                    outcome.id,
                    outcome.failure.clone().unwrap_or_default()
                );
                // The clock carries over: a retried panelist reports the time
                // the user actually waited, not just the second attempt.
                match spawn(&run, r.idx, p, &cwds[r.idx], r.started, true) {
                    Ok(again) => still.push(again),
                    Err(e) => {
                        eprintln!("panel: {} could not be retried: {e:#}", p.id);
                        print_section(&outcome, status);
                        done.push(outcome);
                    }
                }
            } else {
                print_section(&outcome, status);
                done.push(outcome);
            }
        }
        running = still;
        if !running.is_empty() {
            // The long silence: every panelist is a model call, and the
            // slowest decides the wall clock. Ticking says the run is alive.
            let waited = running.iter().map(|r| r.started.elapsed().as_secs()).max().unwrap_or(0);
            status.tick(step::reviewing(running.len(), waited));
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    done.sort_by_key(|o| panelists.iter().position(|p| p.id == o.id).unwrap_or(usize::MAX));
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(exit: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
        Outcome {
            id: "claude".into(),
            model: "m".into(),
            exit,
            stdout: stdout.into(),
            failure: failure_reason(exit, stdout, stderr),
            elapsed_secs: 1,
            retried: false,
        }
    }

    #[test]
    fn a_clean_panelist_is_not_a_failure() {
        let o = outcome(Some(0), "Model: m\nGoal (clear): x\nNO_FINDINGS — checked", "");
        assert!(o.clean() && o.answered());
        assert!(!worth_retrying(&o));
    }

    #[test]
    fn a_report_with_a_bad_exit_still_counts_as_answered() {
        // The distinction that decides whether there is anything to
        // synthesize: this panelist did the work and stumbled on the way out.
        let o = outcome(Some(3), "Model: m\n- [HIGH] a.rs:1 — bug", "");
        assert!(o.answered(), "a report is a report");
        assert!(!o.clean(), "and the reader should still be told about the exit");
        assert_eq!(o.failure.unwrap(), "exited 3 after producing output");
    }

    #[test]
    fn each_way_of_contributing_nothing_says_which_it_was() {
        assert_eq!(outcome(Some(20), "", "").failure.unwrap(), "timed out");
        assert_eq!(
            outcome(Some(1), "", "quota exceeded").failure.unwrap(),
            "no output (exit 1): quota exceeded"
        );
        assert!(outcome(Some(1), "", "").failure.unwrap().contains("no error detail captured"));
        assert!(outcome(Some(0), "", "").failure.unwrap().starts_with("exited 0 but produced no output"));
        assert_eq!(outcome(None, "", "").failure.unwrap(), "killed before it finished");
        for o in [outcome(Some(20), "", ""), outcome(Some(1), "", ""), outcome(None, "", "")] {
            assert!(!o.answered());
        }
    }

    #[test]
    fn only_an_empty_transient_failure_is_retried() {
        assert!(worth_retrying(&outcome(Some(1), "", "quota exceeded")));
        assert!(worth_retrying(&outcome(Some(0), "", "")));
        // A timeout would spend the same wall clock to fail the same way.
        assert!(!worth_retrying(&outcome(Some(20), "", "")));
        // Something came back: that is the review, not a blip.
        assert!(!worth_retrying(&outcome(Some(1), "Model: m\npartial", "")));
        let mut second = outcome(Some(1), "", "");
        second.retried = true;
        assert!(!worth_retrying(&second));
    }

    #[test]
    fn a_panelist_that_never_started_is_an_outcome_not_an_error() {
        let p = Panelist { id: "codex".into(), backend: "codex".into(), model: None };
        let o = never_ran(&p, "could not start: no such file".into(), Instant::now());
        assert!(!o.answered() && !o.clean());
        assert_eq!(o.id, "codex");
        assert_eq!(o.exit, None);
        assert!(o.failure.unwrap().starts_with("could not start"));
    }
}
