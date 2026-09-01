//! One review: how it is spawned, what its exit status means, and what its
//! meta envelope contributes to the summary.
//!
//! The built-in reviewer runs through dash-p, which owns the hard parts: it
//! setsids claude, enforces the timeout with killpg, and reports the truth in
//! a stable exit code -- 0 ok, 10 agent-error (including an is_error turn and
//! garbage output), 20 timeout -- so there is no envelope to sniff for a
//! failure it has already reported. An override is judged by its exit status
//! alone; prose on stdout is its normal shape, not a failure.

use crate::cli::Config;
use crate::report::Trailer;
use crate::repo::RepoContext;
use crate::rundir::RunDir;
use crate::session::{SessionFlag, is_uuid_shaped};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

/// What dash-p gets when --timeout 0 disables ours: effectively forever, but
/// still an explicit value -- dash-p's own default is 300s, which would
/// silently truncate an hour-long review.
const DASHP_TIMEOUT_DISABLED: u64 = 999_999_999;

/// How long past dash-p's own timeout we wait before killing it ourselves.
/// dash-p enforces the real cap; this guard only covers its one known hole
/// (an unbounded wait after the agent closes stdout but does not exit).
pub const GUARD_GRACE_SECS: u64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed,
    Timeout,
}

#[derive(Debug)]
pub struct Job {
    pub pr: u64,
    /// The PR title, for the live board; empty when unknown.
    pub title: String,
    /// Who opened it. On the board because a row that says only "#9" makes
    /// you go and look up whose work you are about to spend money reviewing.
    pub author: String,
    pub state: JobState,
    /// The child's pid doubles as its process-group id (process_group(0)).
    pub pgid: Option<i32>,
    pub flag: SessionFlag,
    /// The session shown in the summary and recorded for the next pass.
    pub sid: Option<String>,
    pub resume: bool,
    pub started: Option<Instant>,
    /// The child exited and was reaped; only the verdict readback remains.
    pub reaped: bool,
    /// Wall-clock start, for comparing against GitHub review timestamps.
    pub started_epoch: i64,
    pub elapsed_secs: u64,
    /// The exit code, or None for signal-death ("no result").
    pub exit_code: Option<i32>,
    pub guard_tripped: bool,
    pub cost: Option<f64>,
    /// The model dash-p reports the review ran on.
    pub model: Option<String>,
    /// What the review concluded: GitHub's readback first, else the agent's
    /// own trailer decision.
    pub verdict: Option<String>,
    /// The agent's self-reported trailer (risk, findings, panel).
    pub trailer: Option<Trailer>,
}

impl Job {
    pub fn new(pr: u64) -> Job {
        Job {
            pr,
            title: String::new(),
            author: String::new(),
            state: JobState::Queued,
            pgid: None,
            flag: SessionFlag::None,
            sid: None,
            resume: false,
            started: None,
            reaped: false,
            started_epoch: 0,
            elapsed_secs: 0,
            exit_code: None,
            guard_tripped: false,
            cost: None,
            model: None,
            verdict: None,
            trailer: None,
        }
    }

    /// Resuming and reviewing from scratch are different work, so they get
    /// different prompts: a resumed session already holds the earlier review,
    /// which is exactly what recheck-pr needs and what a fresh panel review
    /// would throw away. Slash names, not prose: an unattended one-shot has
    /// no human to correct a prompt that failed to trigger the skill.
    pub fn prompt(&self, cfg: &Config) -> String {
        let base = if cfg.no_post {
            // Structural, not a request. /panel-review reviews and reports;
            // it has no posting step to skip. Telling /auto-review not to
            // post would leave a model holding gh write access and an
            // instruction, which is not the same thing as being unable to.
            format!("/panel-review {}", self.pr)
        } else if self.resume {
            format!("/recheck-pr {}", self.pr)
        } else if cfg.unattended() {
            format!("/auto-review {}", self.pr)
        } else {
            format!("/panel-review {}", self.pr)
        };
        // /panel-review and /auto-review both define --focus and pass it down
        // to the panel, so it travels as the option it already is rather than
        // as prose a skill would have to interpret.
        //
        // /recheck-pr does not define it. The option still reaches a model
        // reading its own instructions, so it reads as guidance rather than
        // failing, but nothing carries it to the panelists. A --continue run
        // is a second look at findings that already exist, which is the case
        // that needs steering least.
        match &cfg.focus {
            Some(focus) => format!("{base} --focus \"{focus}\""),
            None => base,
        }
    }

    /// How a finished job failed, in the words the reader needs: an exit
    /// status when there was one, otherwise the fact that the job left no
    /// result at all (killed from outside -- an OOM kill, a stray pkill).
    pub fn outcome(&self) -> String {
        match self.exit_code {
            Some(code) => format!("exit {code}"),
            None => "no result".into(),
        }
    }
}

/// The argv for one built-in review. --meta-file always: stdout is empty on
/// timeout and interrupt, so the envelope is the only readable result there.
/// The budget is the single-token `=` form -- dash-p forwards unrecognized
/// flags only that way, and a silently dropped cap on an unattended sweep is
/// exactly the failure it exists to prevent.
pub fn dashp_args(job: &Job, cfg: &Config, rundir: &RunDir) -> Vec<String> {
    let timeout = if cfg.timeout_secs > 0 { cfg.timeout_secs } else { DASHP_TIMEOUT_DISABLED };
    let mut argv = vec![
        "--output-format".into(),
        "json".into(),
        "--meta-file".into(),
        rundir.meta_path(job.pr).display().to_string(),
        "--timeout".into(),
        timeout.to_string(),
        "--dangerously-skip-permissions".into(),
        // The trailer request rides in the system prompt rather than the
        // user prompt, so the slash command stays the whole prompt and the
        // skill trigger is never at risk. Single-token `=` form: dash-p
        // forwards unrecognized flags only that way.
        format!("--append-system-prompt={}", crate::report::TRAILER_INSTRUCTION),
    ];
    match &job.flag {
        SessionFlag::Pin(id) => {
            argv.push("--session-id".into());
            argv.push(id.clone());
        }
        SessionFlag::Resume(id) => {
            argv.push("--resume".into());
            argv.push(id.clone());
        }
        SessionFlag::None => {}
    }
    if let Some(b) = &cfg.budget {
        argv.push(format!("--max-budget-usd={b}"));
    }
    argv.push("--".into());
    argv.push(job.prompt(cfg));
    argv
}

/// The shell line an override runs: the PR number replaces every "{}", or is
/// appended when there is no placeholder.
pub fn override_invocation(cmd: &str, pr: u64) -> String {
    if cmd.contains("{}") {
        cmd.replace("{}", &pr.to_string())
    } else {
        format!("{cmd} {pr}")
    }
}

pub fn spawn(job: &Job, cfg: &Config, ctx: &RepoContext, rundir: &RunDir, dashp: &str) -> Result<Child> {
    let stdout = std::fs::File::create(rundir.stdout_path(job.pr))?;
    let stderr = std::fs::File::create(rundir.log_path(job.pr))?;

    let mut command = match &cfg.review_cmd {
        Some(cmd) => {
            // An override owns its own session handling; it receives the id
            // and a resume flag in its real environment. The id is exported
            // even when empty -- the contract is "always set".
            let mut c = Command::new("bash");
            c.arg("-c").arg(override_invocation(cmd, job.pr));
            c.env("REVIEW_PRS_SESSION_ID", job.sid.clone().unwrap_or_default());
            c.env("REVIEW_PRS_SESSION_RESUME", if job.resume { "1" } else { "0" });
            c
        }
        None => {
            let mut c = Command::new(dashp);
            c.args(dashp_args(job, cfg, rundir));
            c
        }
    };
    // Its own process group, so stopping the job is one killpg over
    // everything it spawned -- no tree walking, no reparenting races, and no
    // job-control noise in the middle of the progress display.
    command
        .current_dir(&ctx.repo_root)
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0);
    command.spawn().context("spawning the reviewer")
}

/// What a finished job's status means. The guard trumps everything: if we had
/// to kill it, it timed out, whatever exit the kill produced.
pub fn classify(status: std::process::ExitStatus, guard_tripped: bool, is_override: bool) -> (JobState, Option<i32>) {
    let code = status.code();
    if guard_tripped {
        return (JobState::Timeout, code);
    }
    match code {
        Some(0) => (JobState::Done, Some(0)),
        Some(20) if !is_override => (JobState::Timeout, Some(20)),
        Some(n) => (JobState::Failed, Some(n)),
        // Signal-death: killed from outside, no result to read.
        None => (JobState::Failed, None),
    }
}

/// The slice of dash-p's meta envelope the summary needs. Cost is dash-p's
/// own accounting (which is claude's), so it is only ever there for the
/// built-in reviewer.
#[derive(Deserialize, Debug, Default)]
pub struct MetaEnvelope {
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub total_cost_usd: f64,
    /// The model the harness actually ran, e.g. "claude-fable-5".
    #[serde(default)]
    pub model_resolved: String,
}

pub fn read_meta(rundir: &RunDir, pr: u64) -> Option<MetaEnvelope> {
    let raw = std::fs::read_to_string(rundir.meta_path(pr)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The session column and the recorded id, decided from the envelope:
/// - a uuid-shaped envelope id names the session the review actually ran in
///   (a run that sent no flag cannot know it any other way);
/// - no usable envelope id but a flag was sent: keep the planned id;
/// - an override, or no flag and no envelope: nothing to promise, show "-".
pub fn summary_sid(job: &Job, meta: Option<&MetaEnvelope>, is_override: bool) -> Option<String> {
    if is_override {
        return None;
    }
    if let Some(m) = meta
        && is_uuid_shaped(&m.session_id)
    {
        return Some(m.session_id.clone());
    }
    match &job.flag {
        SessionFlag::None => None,
        _ => job.sid.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn cfg_with(timeout: u64, budget: Option<&str>, pick: bool) -> Config {
        Config {
            pick,
            babysit: None,
            watch: None,
            focus: None,
            no_post: false,
            continue_sessions: false,
            jobs: 2,
            max_passes: 3,
            max_idle: 3,
            timeout_secs: timeout,
            budget: budget.map(String::from),
            log_dir: None,
            include_approved: false,
            include_dependabot: false,
            review_cmd: None,
            startup_notes: vec![],
        }
    }

    fn rundir() -> RunDir {
        let base = std::env::temp_dir().join(format!("ar-job-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let mut rd = RunDir::new(Some(base)).unwrap();
        rd.start_pass(1).unwrap();
        rd
    }

    #[test]
    fn prompts_follow_the_mode() {
        let mut job = Job::new(9);
        assert_eq!(job.prompt(&cfg_with(0, None, true)), "/panel-review 9");
        assert_eq!(job.prompt(&cfg_with(0, None, false)), "/auto-review 9");
        job.resume = true;
        assert_eq!(job.prompt(&cfg_with(0, None, false)), "/recheck-pr 9");
    }

    #[test]
    fn no_post_reviews_without_a_posting_step() {
        // Every path that would post goes to the skill that cannot.
        let mut cfg = cfg_with(0, None, false);
        cfg.no_post = true;
        assert_eq!(Job::new(9).prompt(&cfg), "/panel-review 9");
        let mut resumed = Job::new(9);
        resumed.resume = true;
        assert_eq!(resumed.prompt(&cfg), "/panel-review 9", "a recheck posts too");
    }

    #[test]
    fn a_focus_rides_along_as_an_option_the_skills_understand() {
        let job = Job::new(9);
        let mut cfg = cfg_with(0, None, false);
        cfg.focus = Some("be strict about the migration".into());
        assert_eq!(
            job.prompt(&cfg),
            "/auto-review 9 --focus \"be strict about the migration\""
        );
        // One line, always: the prompt is one argv element and one line in
        // the reviewer call log.
        assert!(!job.prompt(&cfg).contains('\n'));
        // A quote in the text must not close the option early.
        // Already made safe by the parser; the prompt just wraps it.
        cfg.focus = Some("the 'fast' path".into());
        assert_eq!(job.prompt(&cfg), "/auto-review 9 --focus \"the 'fast' path\"");
    }

    #[test]
    fn dashp_argv_shape() {
        let rd = rundir();
        let mut job = Job::new(9);
        job.flag = SessionFlag::Pin("7442b624-5cba-5d44-ae67-9c390cfe70a1".into());
        let argv = dashp_args(&job, &cfg_with(3600, Some("2.50"), false), &rd);
        let joined = argv.join(" ");
        assert!(joined.contains("--output-format json"));
        assert!(joined.contains("--meta-file"));
        assert!(joined.contains("--timeout 3600"));
        assert!(joined.contains("--dangerously-skip-permissions"));
        assert!(joined.contains("--session-id 7442b624-5cba-5d44-ae67-9c390cfe70a1"));
        // The trailer request rides along as one `=`-form token.
        assert!(argv.iter().any(|a| a.starts_with("--append-system-prompt=")));
        // The single-token = form: dash-p forwards unknown flags only this way.
        assert!(argv.contains(&"--max-budget-usd=2.50".to_string()));
        assert_eq!(argv.last().unwrap(), "/auto-review 9");
        assert_eq!(argv[argv.len() - 2], "--");
    }

    #[test]
    fn timeout_zero_still_sends_a_timeout() {
        let rd = rundir();
        let job = Job::new(9);
        let argv = dashp_args(&job, &cfg_with(0, None, true), &rd);
        assert!(argv.join(" ").contains(&format!("--timeout {DASHP_TIMEOUT_DISABLED}")));
    }

    #[test]
    fn override_substitution() {
        assert_eq!(override_invocation("my-review", 9), "my-review 9");
        assert_eq!(override_invocation("my-review {} --extra {}", 9), "my-review 9 --extra 9");
        assert_eq!(
            override_invocation("gh pr checkout {} && my-review {}", 12),
            "gh pr checkout 12 && my-review 12"
        );
    }

    #[test]
    fn classification() {
        let exit = |code: i32| ExitStatus::from_raw(code << 8);
        let signal = ExitStatus::from_raw(9); // SIGKILL
        assert_eq!(classify(exit(0), false, false), (JobState::Done, Some(0)));
        assert_eq!(classify(exit(20), false, false), (JobState::Timeout, Some(20)));
        assert_eq!(classify(exit(10), false, false), (JobState::Failed, Some(10)));
        assert_eq!(classify(exit(2), false, false), (JobState::Failed, Some(2)));
        assert_eq!(classify(signal, false, false), (JobState::Failed, None));
        // An override exiting 20 is just a failure -- 20 is dash-p's code.
        assert_eq!(classify(exit(20), false, true), (JobState::Failed, Some(20)));
        // The guard trumps whatever the kill produced.
        assert_eq!(classify(signal, true, false).0, JobState::Timeout);
    }

    #[test]
    fn summary_sid_rules() {
        let mut job = Job::new(9);
        job.flag = SessionFlag::Pin("7442b624-5cba-5d44-ae67-9c390cfe70a1".into());
        job.sid = Some("7442b624-5cba-5d44-ae67-9c390cfe70a1".into());

        // Envelope id wins: it names the session the review actually ran in.
        let meta = MetaEnvelope {
            session_id: "80e25f6a-45b7-5246-a9e2-8feda1021531".into(),
            total_cost_usd: 0.42,
            model_resolved: "claude-fable-5".into(),
        };
        assert_eq!(summary_sid(&job, Some(&meta), false).as_deref(), Some("80e25f6a-45b7-5246-a9e2-8feda1021531"));

        // Empty envelope id, flag sent: keep the planned id.
        let empty = MetaEnvelope::default();
        assert_eq!(
            summary_sid(&job, Some(&empty), false).as_deref(),
            Some("7442b624-5cba-5d44-ae67-9c390cfe70a1")
        );

        // No flag sent and nothing usable back: nothing to promise.
        job.flag = SessionFlag::None;
        assert_eq!(summary_sid(&job, Some(&empty), false), None);

        // Overrides own their sessions; the column shows "-".
        assert_eq!(summary_sid(&job, Some(&meta), true), None);
    }

    #[test]
    fn outcome_words() {
        let mut job = Job::new(9);
        job.exit_code = Some(10);
        assert_eq!(job.outcome(), "exit 10");
        job.exit_code = None;
        assert_eq!(job.outcome(), "no result");
    }
}
