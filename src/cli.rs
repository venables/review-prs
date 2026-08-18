//! Flags, env defaults and validation, mirroring the bash arg loop exactly --
//! the same spellings accepted, the same messages on the same exit codes. The
//! parser is hand-rolled rather than clap: the contract is byte-exact error
//! strings, exit 1 (not 2) on bad input, and an `=`-only value form for
//! --babysit, all of which a 20-branch match gives for free.

use crate::interval::{self, Interval};
use std::path::PathBuf;

pub const HELP: &str = r#"autoreview: review open PRs headlessly, with progress and a real exit status.

Usage: autoreview [--auto] [--babysit[=MINUTES]] [--continue] [--jobs N]
                  [--timeout SECONDS] [--budget USD] [--log-dir DIR]
                  [--all] [--dependabot] [--help]

  --auto, -A          Skip the picker; review every NEW/UPDATED PR.
  --babysit[=MIN], -b Re-run the pass every MIN minutes (default 30, or
                      $AUTOREVIEW_BABYSIT_INTERVAL), dropping PRs as they are
                      approved or closed, until none are left. A bare number is
                      minutes; 30m/1h/2d also work.
  --continue, -C      Resume this machine's earlier review session for a PR
                      (a second look at the findings) instead of reviewing it
                      from scratch. Marked RESUMABLE in the picker.
  --jobs N, -j N      Reviews to run at once (default 2, or $AUTOREVIEW_JOBS).
                      Keep it low: a panel review is itself several agents.
  --timeout SECONDS   Give up on a review that runs this long (default 3600,
                      or $AUTOREVIEW_TIMEOUT; 0 disables).
  --budget USD        Cap each review's API spend (claude --max-budget-usd).
  --log-dir DIR       Where to write per-PR output (default: a temp directory,
                      printed on every run).
  --all, -a           Include PRs already marked APPROVED (default: exclude).
  --dependabot, -d    Include Dependabot PRs (default: hidden; shown dimmed).
  --help, -h          Show this help.

Each PR is reviewed by a dash-p subprocess driving claude headlessly:
  dash-p --output-format json --meta-file ... --timeout ... \
    --dangerously-skip-permissions --session-id UUID -- "/panel-review N"
where UUID is derived from the repo directory plus owner/name#N, so the same PR
in this checkout always maps to the same session -- and `claude --resume UUID`
reopens it interactively later. Set $DASHP_BIN to point at a different dash-p.

Override the reviewer via $AUTOREVIEW_CMD, or $AUTOREVIEW_AUTO_CMD for
--auto/--babysit runs (the PR number replaces the first "{}", or is appended if
absent):
  AUTOREVIEW_CMD='my-review'                           (append form)
  AUTOREVIEW_CMD='gh pr checkout {} && my-review {}'   (placeholder form)
An overridden command owns its own session handling; it receives the id as
$REVIEW_PRS_SESSION_ID and a 0/1 $REVIEW_PRS_SESSION_RESUME.

Exit status is 0 only when every review in the final pass succeeded.

To fan the same PRs into terminal tabs you can watch and steer instead, use
`review-prs`.

Your own PRs are always hidden -- this tool is for reviewing others' work.
"#;

#[derive(Debug, Clone)]
pub struct Config {
    pub auto: bool,
    pub babysit: Option<Interval>,
    pub continue_sessions: bool,
    pub jobs: u32,
    pub timeout_secs: u64,
    pub budget: Option<String>,
    pub log_dir: Option<PathBuf>,
    pub include_approved: bool,
    pub include_dependabot: bool,
    /// The override to run instead of dash-p; None means the built-in
    /// reviewer. Resolved from $AUTOREVIEW_CMD / $AUTOREVIEW_AUTO_CMD by mode.
    pub review_cmd: Option<String>,
    /// Printed to stderr before the run starts, e.g. the silent-fallback
    /// warning when an unattended run ignores $AUTOREVIEW_CMD.
    pub startup_notes: Vec<String>,
}

impl Config {
    /// Unattended runs pick the auto prompt and the auto override.
    pub fn unattended(&self) -> bool {
        self.auto || self.babysit.is_some()
    }
}

pub enum Parsed {
    Run(Box<Config>),
    Help,
}

pub struct CliError {
    pub msg: String,
    /// Unknown args also print the help, to stderr.
    pub show_help: bool,
}

fn err(msg: String) -> CliError {
    CliError { msg, show_help: false }
}

/// Env access is injected so the unit tests are hermetic: a developer's own
/// $AUTOREVIEW_JOBS must not change what the tests assert.
pub type EnvFn<'a> = &'a dyn Fn(&str) -> Option<String>;

pub fn real_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn env_nonempty(env: EnvFn, name: &str) -> Option<String> {
    env(name).filter(|v| !v.is_empty())
}

/// Reject a flag value early rather than letting it reach a sleep, a slot
/// count or the reviewer itself, where the failure would be far from its
/// cause. The max matters as much as the min: a --jobs past u32 would
/// otherwise truncate to zero slots and stall the pool forever, and a
/// timeout past the deadline arithmetic would overflow it.
fn require_int(flag: &str, value: &str, min: u64, max: u64) -> Result<u64, CliError> {
    let ok = !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit());
    let parsed = if ok { value.parse::<u64>().ok() } else { None };
    match parsed {
        Some(n) if n >= min && n <= max => Ok(n),
        Some(n) if n > max => Err(err(format!(
            "error: {flag} expects an integer <= {max}, got \"{value}\""
        ))),
        _ => Err(err(format!(
            "error: {flag} expects an integer >= {min}, got \"{value}\""
        ))),
    }
}

/// Both spellings of a flag need a value: "--flag value" is checked when the
/// next argument is taken, "--flag=value" after unpacking. A bare "--budget="
/// must not pass silently for an empty cap while "--budget ''" is refused.
fn require_value(flag: &str, value: Option<String>) -> Result<String, CliError> {
    match value {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(err(format!("error: {flag} expects a value"))),
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I, env: EnvFn) -> Result<Parsed, CliError> {
    let mut auto = false;
    let mut babysit = false;
    let mut continue_sessions = false;
    let mut include_approved = false;
    let mut include_dependabot = false;

    let mut jobs_raw = env_nonempty(env, "AUTOREVIEW_JOBS").unwrap_or_else(|| "2".into());
    let mut timeout_raw = env_nonempty(env, "AUTOREVIEW_TIMEOUT").unwrap_or_else(|| "3600".into());
    let mut budget_raw = env_nonempty(env, "AUTOREVIEW_MAX_BUDGET_USD");
    let mut log_dir_raw = env_nonempty(env, "AUTOREVIEW_LOG_DIR");
    // Kept raw until after arg parsing: validating here would make a bad
    // $AUTOREVIEW_BABYSIT_INTERVAL in a shell profile hard-fail every run,
    // including --help and picker runs that never babysit.
    let mut babysit_interval_raw =
        env_nonempty(env, "AUTOREVIEW_BABYSIT_INTERVAL").unwrap_or_else(|| "30".into());

    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--auto" | "-A" => auto = true,
            "--babysit" | "-b" => babysit = true,
            "--continue" | "-C" => continue_sessions = true,
            "--all" | "-a" => include_approved = true,
            "--dependabot" | "-d" => include_dependabot = true,
            "--help" | "-h" => return Ok(Parsed::Help),
            "--jobs" | "-j" => jobs_raw = require_value("--jobs", it.next())?,
            "--timeout" => timeout_raw = require_value("--timeout", it.next())?,
            "--budget" => budget_raw = Some(require_value("--budget", it.next())?),
            "--log-dir" => log_dir_raw = Some(require_value("--log-dir", it.next())?),
            other => {
                if let Some(v) = other.strip_prefix("--babysit=") {
                    babysit = true;
                    babysit_interval_raw = v.to_string();
                } else if let Some(v) =
                    other.strip_prefix("--jobs=").or_else(|| other.strip_prefix("-j="))
                {
                    jobs_raw = require_value("--jobs", Some(v.to_string()))?;
                } else if let Some(v) = other.strip_prefix("--timeout=") {
                    timeout_raw = require_value("--timeout", Some(v.to_string()))?;
                } else if let Some(v) = other.strip_prefix("--budget=") {
                    budget_raw = Some(require_value("--budget", Some(v.to_string()))?);
                } else if let Some(v) = other.strip_prefix("--log-dir=") {
                    log_dir_raw = Some(require_value("--log-dir", Some(v.to_string()))?);
                } else {
                    return Err(CliError {
                        msg: format!("unknown arg: {other}"),
                        show_help: true,
                    });
                }
            }
        }
    }

    let jobs = require_int("--jobs", &jobs_raw, 1, 1024)? as u32;
    // The cap matches what dash-p is handed when the timeout is disabled, and
    // keeps the deadline arithmetic far from overflow. Nobody waits 31 years.
    let timeout_secs = require_int("--timeout", &timeout_raw, 0, 999_999_999)?;

    if let Some(b) = &budget_raw {
        let dollar = {
            let (whole, frac) = b.split_once('.').map_or((b.as_str(), None), |(w, f)| (w, Some(f)));
            !whole.is_empty()
                && whole.bytes().all(|c| c.is_ascii_digit())
                && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()))
        };
        if !dollar {
            return Err(err(format!(
                "error: --budget expects a dollar amount, got \"{b}\""
            )));
        }
        // A cap of zero is never what anyone meant, and it is ambiguous in
        // the worst place: it either fails every review or reads as "no cap".
        if b.bytes().all(|c| c == b'0' || c == b'.') {
            return Err(err(format!(
                "error: --budget expects a positive dollar amount, got \"{b}\""
            )));
        }
    }

    // Validated only when babysitting is actually on, so an unrelated bad env
    // var never blocks a plain run.
    let babysit_interval = if babysit {
        Some(interval::normalize(&babysit_interval_raw).map_err(err)?)
    } else {
        None
    };

    // The reviewer to run, and its unattended twin. An unattended run takes
    // the unattended override; falling silently back to the built-in reviewer
    // would be the expensive kind of surprise, so say which one is running.
    let cmd = env_nonempty(env, "AUTOREVIEW_CMD");
    let auto_cmd = env_nonempty(env, "AUTOREVIEW_AUTO_CMD");
    let unattended = auto || babysit;
    let mut startup_notes = Vec::new();
    let review_cmd = if unattended {
        if cmd.is_some() && auto_cmd.is_none() {
            let mode = if auto { "--auto" } else { "--babysit" };
            startup_notes.push(format!(
                "note: $AUTOREVIEW_CMD is set but $AUTOREVIEW_AUTO_CMD is not; {mode} runs the built-in reviewer"
            ));
        }
        auto_cmd
    } else {
        cmd
    };

    Ok(Parsed::Run(Box::new(Config {
        auto,
        babysit: babysit_interval,
        continue_sessions,
        jobs,
        timeout_secs,
        budget: budget_raw,
        log_dir: log_dir_raw.map(PathBuf::from),
        include_approved,
        include_dependabot,
        review_cmd,
        startup_notes,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn run(args: &[&str]) -> Result<Parsed, CliError> {
        parse(args.iter().map(|s| s.to_string()), &no_env)
    }

    fn run_env(args: &[&str], vars: &[(&str, &str)]) -> Result<Parsed, CliError> {
        let vars: Vec<(String, String)> =
            vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let env = move |name: &str| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        };
        parse(args.iter().map(|s| s.to_string()), &env)
    }

    fn cfg(args: &[&str]) -> Config {
        match run(args).ok().unwrap() {
            Parsed::Run(c) => *c,
            Parsed::Help => panic!("unexpected help"),
        }
    }

    fn msg(args: &[&str]) -> String {
        match run(args) {
            Err(e) => e.msg,
            Ok(_) => panic!("expected an error"),
        }
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]);
        assert!(!c.auto && c.babysit.is_none() && !c.continue_sessions);
        assert_eq!(c.jobs, 2);
        assert_eq!(c.timeout_secs, 3600);
        assert!(c.budget.is_none() && c.log_dir.is_none());
    }

    #[test]
    fn both_value_spellings_work() {
        assert_eq!(cfg(&["--jobs", "3"]).jobs, 3);
        assert_eq!(cfg(&["--jobs=3"]).jobs, 3);
        assert_eq!(cfg(&["-j", "4"]).jobs, 4);
        assert_eq!(cfg(&["-j=4"]).jobs, 4);
        assert_eq!(cfg(&["--timeout=0"]).timeout_secs, 0);
        assert_eq!(cfg(&["--budget", "2.50"]).budget.as_deref(), Some("2.50"));
        assert_eq!(cfg(&["--budget=2.50"]).budget.as_deref(), Some("2.50"));
    }

    #[test]
    fn babysit_takes_its_value_only_with_equals() {
        let c = cfg(&["--babysit=15"]);
        assert_eq!(c.babysit.unwrap().normalized, "15m");
        let c = cfg(&["--babysit"]);
        assert_eq!(c.babysit.unwrap().normalized, "30m");
        // "--babysit 15" leaves 15 a stray argument, exactly as bash did.
        let e = run(&["--babysit", "15"]).err().unwrap();
        assert_eq!(e.msg, "unknown arg: 15");
        assert!(e.show_help);
    }

    #[test]
    fn bad_input_messages_are_byte_exact() {
        assert_eq!(msg(&["--jobs", "0"]), "error: --jobs expects an integer >= 1, got \"0\"");
        assert_eq!(msg(&["--jobs", "abc"]), "error: --jobs expects an integer >= 1, got \"abc\"");
        assert_eq!(msg(&["--budget", "lots"]), "error: --budget expects a dollar amount, got \"lots\"");
        assert_eq!(msg(&["--budget", "0"]), "error: --budget expects a positive dollar amount, got \"0\"");
        assert_eq!(msg(&["--budget", "0.00"]), "error: --budget expects a positive dollar amount, got \"0.00\"");
        assert_eq!(msg(&["--timeout"]), "error: --timeout expects a value");
        assert_eq!(msg(&["--budget="]), "error: --budget expects a value");
        assert_eq!(msg(&["--log-dir="]), "error: --log-dir expects a value");
        assert_eq!(
            msg(&["--babysit=soon"]),
            "error: invalid babysit interval: \"soon\" (expected a positive duration, e.g. 30, 30m, 1h)"
        );
        let e = run(&["--nope"]).err().unwrap();
        assert_eq!(e.msg, "unknown arg: --nope");
        assert!(e.show_help);
    }

    #[test]
    fn out_of_range_values_are_rejected_not_truncated() {
        // 2^32 would truncate to zero pool slots and stall forever.
        assert_eq!(
            msg(&["--jobs", "4294967296"]),
            "error: --jobs expects an integer <= 1024, got \"4294967296\""
        );
        assert!(msg(&["--timeout", "99999999999999"]).contains("expects an integer <="));
    }

    #[test]
    fn help_flag() {
        assert!(matches!(run(&["--help"]).ok().unwrap(), Parsed::Help));
        assert!(matches!(run(&["-h"]).ok().unwrap(), Parsed::Help));
    }

    #[test]
    fn nonzero_budget_shapes_pass() {
        for good in ["1", "0.50", "2.5", "10.00"] {
            assert!(run(&["--budget", good]).is_ok(), "{good} should pass");
        }
    }

    #[test]
    fn env_defaults_flow_through_validation() {
        let c = match run_env(&[], &[("AUTOREVIEW_JOBS", "5"), ("AUTOREVIEW_TIMEOUT", "60")]) {
            Ok(Parsed::Run(c)) => c,
            _ => panic!(),
        };
        assert_eq!(c.jobs, 5);
        assert_eq!(c.timeout_secs, 60);

        // A bad env value fails with the flag's own message, even flagless.
        let e = run_env(&[], &[("AUTOREVIEW_JOBS", "abc")]).err().unwrap();
        assert_eq!(e.msg, "error: --jobs expects an integer >= 1, got \"abc\"");

        // An empty env value falls back to the default rather than erroring.
        let c = match run_env(&[], &[("AUTOREVIEW_JOBS", "")]) {
            Ok(Parsed::Run(c)) => c,
            _ => panic!(),
        };
        assert_eq!(c.jobs, 2);

        // A bad babysit interval in the profile must not break a plain run.
        assert!(run_env(&[], &[("AUTOREVIEW_BABYSIT_INTERVAL", "junk")]).is_ok());
        let e = run_env(&["--babysit"], &[("AUTOREVIEW_BABYSIT_INTERVAL", "junk")])
            .err()
            .unwrap();
        assert!(e.msg.contains("invalid babysit interval"));
    }

    #[test]
    fn unattended_runs_take_the_auto_override() {
        let get = |args: &[&str], vars: &[(&str, &str)]| match run_env(args, vars) {
            Ok(Parsed::Run(c)) => c,
            _ => panic!(),
        };
        let c = get(&["--auto"], &[("AUTOREVIEW_CMD", "my-review")]);
        assert_eq!(c.review_cmd, None);
        assert!(c.startup_notes[0].contains("--auto runs the built-in reviewer"));

        let c = get(&["--auto"], &[("AUTOREVIEW_AUTO_CMD", "auto-r")]);
        assert_eq!(c.review_cmd.as_deref(), Some("auto-r"));
        assert!(c.startup_notes.is_empty());

        let c = get(&[], &[("AUTOREVIEW_CMD", "my-review")]);
        assert_eq!(c.review_cmd.as_deref(), Some("my-review"));
        assert!(c.startup_notes.is_empty());
    }
}
