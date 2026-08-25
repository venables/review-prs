//! Running the panel: spawn every panelist at once, print each section the
//! moment it lands, and retry the failures that are worth retrying.
//!
//! No bounded pool here, unlike autoreview's. A panel is three to six
//! processes decided by what is installed, not a queue of unknown length, and
//! holding one back would only make the slowest panelist later.

use crate::panel::cli::Config;
use crate::panel::panelist::{self, Panelist};
use anyhow::{Context, Result};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// dash-p's exit code for a run that hit its own timeout.
const EXIT_TIMEOUT: i32 = 20;

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    /// What the panelist said it was running, or "unknown".
    pub model: String,
    pub exit: Option<i32>,
    pub stdout: String,
    /// Some(reason) when this panelist contributed nothing. A failed panelist
    /// is not a missing one: the synthesis is told, so it can weigh consensus
    /// against how many voices actually returned.
    pub failure: Option<String>,
    pub elapsed_secs: u64,
    pub retried: bool,
}

impl Outcome {
    pub fn ok(&self) -> bool {
        self.failure.is_none()
    }
}

/// Why a finished panelist contributed nothing, in the words a reader needs.
fn failure_reason(exit: Option<i32>, stdout: &str, stderr: &str) -> Option<String> {
    let detail = || {
        let tail: Vec<&str> = stderr.lines().filter(|l| !l.trim().is_empty()).collect();
        match tail.last() {
            Some(line) => format!(": {}", line.trim()),
            None => " and no error detail captured".into(),
        }
    };
    match exit {
        Some(EXIT_TIMEOUT) => Some("timed out".into()),
        Some(0) if stdout.trim().is_empty() => {
            Some(format!("exited 0 but produced no output{}", detail()))
        }
        Some(0) => None,
        Some(code) if stdout.trim().is_empty() => {
            Some(format!("no output (exit {code}){}", detail()))
        }
        // Output plus a bad status: the words are still worth reading, but the
        // run is not clean and the reader should know which half to trust.
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
    !o.retried
        && o.failure.is_some()
        && o.exit != Some(EXIT_TIMEOUT)
        && o.stdout.trim().is_empty()
}

struct Running {
    panelist: Panelist,
    child: Child,
    cwd: PathBuf,
    started: Instant,
    out_path: PathBuf,
    err_path: PathBuf,
    retried: bool,
}

fn read_file(path: &Path) -> String {
    let mut s = String::new();
    if let Ok(mut f) = File::open(path) {
        let _ = f.read_to_string(&mut s);
    }
    s
}

fn spawn(
    p: &Panelist,
    cwd: &Path,
    cfg: &Config,
    dashp: &str,
    prompt_path: &Path,
    out_dir: &Path,
    retried: bool,
) -> Result<Running> {
    let out_path = out_dir.join(format!("{}.out", p.id));
    let err_path = out_dir.join(format!("{}.err", p.id));
    // The prompt arrives on stdin, so dash-p reads it without it ever being
    // an argument. See panelist::dashp_args for why that matters.
    let stdin = File::open(prompt_path).context("opening the prompt file")?;
    let child = Command::new(dashp)
        .args(panelist::dashp_args(p, cwd, cfg.timeout_secs, cfg.isolated))
        .stdin(Stdio::from(stdin))
        .stdout(File::create(&out_path).context("creating a panelist's output file")?)
        .stderr(File::create(&err_path).context("creating a panelist's error file")?)
        .spawn()
        .with_context(|| format!("spawning dash-p for panelist '{}'", p.id))?;
    Ok(Running {
        panelist: p.clone(),
        child,
        cwd: cwd.to_path_buf(),
        started: Instant::now(),
        out_path,
        err_path,
        retried,
    })
}

fn finish(mut r: Running, exit: Option<i32>) -> Outcome {
    let stdout = read_file(&r.out_path);
    let stderr = read_file(&r.err_path);
    let _ = r.child.try_wait();
    Outcome {
        id: r.panelist.id.clone(),
        model: panelist::extract_model(&stdout).unwrap_or_else(|| "unknown".into()),
        exit,
        failure: failure_reason(exit, &stdout, &stderr),
        stdout,
        elapsed_secs: r.started.elapsed().as_secs(),
        retried: r.retried,
    }
}

/// Print one panelist's section as soon as it lands. Waiting for the slowest
/// one to print any of them is how a coordinator ends up staring at nothing
/// for ten minutes while three reviews sit finished on disk.
fn print_section(o: &Outcome) {
    println!("## {} / {} (exit {})", o.id, o.model, match o.exit {
        Some(c) => c.to_string(),
        None => "killed".into(),
    });
    println!();
    if let Some(reason) = &o.failure {
        println!("FAILED: {reason}");
        println!();
    }
    if !o.stdout.trim().is_empty() {
        println!("{}", o.stdout.trim_end());
        println!();
    }
    // stderr, not stdout: a heartbeat is progress, and progress must not land
    // in the middle of the report someone is piping to a file.
    eprintln!(
        "panel: {} ({}) done in {} (exit {})",
        o.id,
        o.model,
        crate::ui::fmt_dur(o.elapsed_secs),
        o.exit.map(|c| c.to_string()).unwrap_or_else(|| "killed".into())
    );
}

/// Run every panelist, streaming sections, and return what each contributed.
/// Order follows the panel list, not finishing order, so two runs of the same
/// panel are comparable.
pub fn run(
    panelists: &[Panelist],
    cwds: &[PathBuf],
    cfg: &Config,
    dashp: &str,
    prompt_path: &Path,
    out_dir: &Path,
) -> Result<Vec<Outcome>> {
    let mut running = Vec::new();
    for (p, cwd) in panelists.iter().zip(cwds) {
        eprintln!("panel: {} started (cwd={})", p.id, cwd.display());
        running.push(spawn(p, cwd, cfg, dashp, prompt_path, out_dir, false)?);
    }

    let mut done: Vec<Outcome> = Vec::new();
    while !running.is_empty() {
        let mut still = Vec::new();
        for mut r in running {
            match r.child.try_wait() {
                Ok(Some(status)) => {
                    let outcome = finish(r, status.code());
                    if worth_retrying(&outcome) {
                        eprintln!(
                            "panel: {} produced nothing ({}); trying once more",
                            outcome.id,
                            outcome.failure.clone().unwrap_or_default()
                        );
                        let p = panelists.iter().find(|p| p.id == outcome.id).expect("panelist");
                        let cwd = cwds[panelists.iter().position(|q| q.id == p.id).expect("index")].clone();
                        still.push(spawn(p, &cwd, cfg, dashp, prompt_path, out_dir, true)?);
                    } else {
                        print_section(&outcome);
                        done.push(outcome);
                    }
                }
                Ok(None) => still.push(r),
                Err(e) => {
                    // Cannot ask about it any more, so it is finished as far
                    // as this run is concerned.
                    let id = r.panelist.id.clone();
                    let cwd = r.cwd.clone();
                    let _ = cwd;
                    let mut outcome = finish(r, None);
                    outcome.failure = Some(format!("could not be waited on: {e}"));
                    eprintln!("panel: {id} could not be waited on: {e}");
                    print_section(&outcome);
                    done.push(outcome);
                }
            }
        }
        running = still;
        if !running.is_empty() {
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
        assert!(o.ok());
        assert!(!worth_retrying(&o));
    }

    #[test]
    fn each_way_of_contributing_nothing_says_which_it_was() {
        assert_eq!(outcome(Some(20), "", "").failure.unwrap(), "timed out");
        assert!(
            outcome(Some(1), "", "quota exceeded").failure.unwrap()
                == "no output (exit 1): quota exceeded"
        );
        assert!(
            outcome(Some(1), "", "").failure.unwrap().contains("no error detail captured")
        );
        assert!(outcome(Some(0), "", "").failure.unwrap().starts_with("exited 0 but produced no output"));
        assert_eq!(outcome(None, "", "").failure.unwrap(), "killed before it finished");
        // Output plus a bad status: readable, but say the status.
        assert_eq!(
            outcome(Some(3), "Model: m\nsome findings", "").failure.unwrap(),
            "exited 3 after producing output"
        );
    }

    #[test]
    fn only_an_empty_transient_failure_is_retried() {
        assert!(worth_retrying(&outcome(Some(1), "", "quota exceeded")));
        assert!(worth_retrying(&outcome(Some(0), "", "")));
        // A timeout would spend the same wall clock to fail the same way.
        assert!(!worth_retrying(&outcome(Some(20), "", "")));
        // Something came back: that is the review, not a blip.
        assert!(!worth_retrying(&outcome(Some(1), "Model: m\npartial", "")));
        // Once only.
        let mut second = outcome(Some(1), "", "");
        second.retried = true;
        assert!(!worth_retrying(&second));
    }
}
