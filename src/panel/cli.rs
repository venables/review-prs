//! panel's flags. A third small surface rather than a shared one: the three
//! binaries have almost nothing in common at the flag level, and a union of
//! them would be a flag list nobody can read.

use crate::cli::{CliError, EnvFn};
use crate::panel::panelist::{self, Spec};
use crate::panel::target::Target;
use std::path::PathBuf;

pub const HELP: &str = r#"panel: review one change with several models at once, then synthesize.

Usage: panel [--base REF | --uncommitted | --staged] [--panelist BACKEND[:MODEL]]...
             [--focus TEXT] [--timeout SECONDS] [--log-dir DIR]
             [--synth BACKEND[:MODEL]] [--no-synthesis] [--help]

Every backend CLI on PATH (codex, claude, opencode) reviews the change in
parallel through dash-p, each one blind to the others. Their reports print as
they land. One more model call then reads all of them, verifies the
questionable claims against the code, and writes the report.

  --base REF          Review REF...HEAD -- what this branch added. Each
                      panelist gets its own worktree and may run the tests.
  --uncommitted       Review what is not committed yet (default). Panelists
                      read your working tree and change nothing.
  --staged            Review the index only.
  --panelist B[:M]    Add one panelist: a backend (codex, claude, opencode)
                      and optionally the model to pin it to. Repeatable. The
                      default is every backend found on PATH, on its own
                      default model.
  --focus TEXT        What the reviewers should pay attention to.
  --timeout SECONDS   Give up on a panelist that runs this long (default 600,
                      or $PANEL_TIMEOUT).
  --log-dir DIR       Where to keep each panelist's output (default: a temp
                      directory, printed on every run).
  --synth B[:M]       Which model writes the synthesis (default claude, or
                      $PANEL_SYNTH).
  --no-synthesis      Print the panelist sections and stop. For comparing the
                      panel against a synthesis you write yourself.
  --help, -h          Show this help.
  --version, -V       Show the version.

The synthesis runs in this repository with read-only access, because verifying
a finding means reading the code it is about. It is told which panelists
failed, so it does not read a dead panelist's silence as agreement.

Exit status is 0 when the report was produced. A panel where some panelists
failed still exits 0 and says so -- it is a thinner review, not a broken run.
"#;

#[derive(Debug, Clone)]
pub struct Config {
    pub target: Target,
    /// Empty means "every backend on PATH".
    pub panelists: Vec<Spec>,
    pub focus: Option<String>,
    pub timeout_secs: u64,
    pub log_dir: Option<PathBuf>,
    pub synthesize: bool,
    pub synth_backend: String,
    pub synth_model: Option<String>,
    /// Set once the target is resolved: worktree-per-panelist and exec, or
    /// the user's own tree and read-only.
    pub isolated: bool,
}

pub enum Parsed {
    Run(Box<Config>),
    Help,
    Version,
}

fn err(msg: String) -> CliError {
    CliError { msg, show_help: false }
}

fn require_value(flag: &str, value: Option<String>) -> Result<String, CliError> {
    match value {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(err(format!("error: {flag} expects a value"))),
    }
}

pub fn parse<I: IntoIterator<Item = String>>(args: I, env: EnvFn) -> Result<Parsed, CliError> {
    let mut target = Target::Uncommitted;
    let mut panelists: Vec<Spec> = Vec::new();
    let mut focus = None;
    let mut log_dir = None;
    let mut synthesize = true;
    let mut timeout_raw = env("PANEL_TIMEOUT").filter(|v| !v.is_empty()).unwrap_or_else(|| "600".into());
    let mut synth_raw = env("PANEL_SYNTH").filter(|v| !v.is_empty()).unwrap_or_else(|| "claude".into());

    let mut it = args.into_iter().peekable();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--uncommitted" => target = Target::Uncommitted,
            "--staged" => target = Target::Staged,
            "--base" => target = Target::Base(require_value("--base", it.next())?),
            "--panelist" => {
                let spec = require_value("--panelist", it.next())?;
                panelists.push(panelist::parse_spec(&spec).map_err(err)?);
            }
            "--focus" => focus = Some(require_value("--focus", it.next())?),
            "--timeout" => timeout_raw = require_value("--timeout", it.next())?,
            "--log-dir" => log_dir = Some(require_value("--log-dir", it.next())?),
            "--synth" => synth_raw = require_value("--synth", it.next())?,
            "--no-synthesis" => synthesize = false,
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            other => {
                if let Some(v) = other.strip_prefix("--base=") {
                    target = Target::Base(require_value("--base", Some(v.to_string()))?);
                } else if let Some(v) = other.strip_prefix("--panelist=") {
                    let spec = require_value("--panelist", Some(v.to_string()))?;
                    panelists.push(panelist::parse_spec(&spec).map_err(err)?);
                } else if let Some(v) = other.strip_prefix("--focus=") {
                    focus = Some(require_value("--focus", Some(v.to_string()))?);
                } else if let Some(v) = other.strip_prefix("--timeout=") {
                    timeout_raw = require_value("--timeout", Some(v.to_string()))?;
                } else if let Some(v) = other.strip_prefix("--log-dir=") {
                    log_dir = Some(require_value("--log-dir", Some(v.to_string()))?);
                } else if let Some(v) = other.strip_prefix("--synth=") {
                    synth_raw = require_value("--synth", Some(v.to_string()))?;
                } else {
                    return Err(CliError { msg: format!("unknown arg: {other}"), show_help: true });
                }
            }
        }
    }

    // The shared checker, so a value over the maximum says that rather than
    // reporting the wrong cause.
    let timeout_secs = crate::cli::require_int("--timeout", &timeout_raw, 1, 999_999_999)?;

    // The synthesizer is a panelist spec too, so "claude:opus-4.8" works and
    // an unknown backend is refused the same way.
    let synth = panelist::parse_spec(&synth_raw).map_err(|e| err(e.replace("panelist", "--synth")))?;

    Ok(Parsed::Run(Box::new(Config {
        target,
        panelists,
        focus,
        timeout_secs,
        log_dir: log_dir.map(PathBuf::from),
        synthesize,
        synth_backend: synth.backend,
        synth_model: synth.model,
        // Filled in by run() once the target is resolved; the flag layer has
        // no business deciding it.
        isolated: false,
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

    fn cfg(args: &[&str]) -> Config {
        match run(args).ok().unwrap() {
            Parsed::Run(c) => *c,
            other => panic!("expected a run, got {}", match other {
                Parsed::Help => "help",
                Parsed::Version => "version",
                Parsed::Run(_) => unreachable!(),
            }),
        }
    }

    #[test]
    fn defaults() {
        let c = cfg(&[]);
        assert_eq!(c.target, Target::Uncommitted);
        assert!(c.panelists.is_empty());
        assert_eq!(c.timeout_secs, 600);
        assert!(c.synthesize);
        assert_eq!(c.synth_backend, "claude");
        assert_eq!(c.synth_model, None);
    }

    #[test]
    fn both_value_spellings_work() {
        assert_eq!(cfg(&["--base", "main"]).target, Target::Base("main".into()));
        assert_eq!(cfg(&["--base=origin/main"]).target, Target::Base("origin/main".into()));
        assert_eq!(cfg(&["--timeout=30"]).timeout_secs, 30);
        assert_eq!(cfg(&["--focus=auth"]).focus.as_deref(), Some("auth"));
    }

    #[test]
    fn panelists_accumulate_in_order() {
        let c = cfg(&["--panelist", "codex", "--panelist=claude:opus-4.8"]);
        assert_eq!(c.panelists.len(), 2);
        assert_eq!(c.panelists[0].backend, "codex");
        assert_eq!(c.panelists[1].model.as_deref(), Some("opus-4.8"));
    }

    #[test]
    fn the_synthesizer_is_a_spec_too() {
        let c = cfg(&["--synth", "codex:gpt-5"]);
        assert_eq!(c.synth_backend, "codex");
        assert_eq!(c.synth_model.as_deref(), Some("gpt-5"));
        let e = run(&["--synth", "gpt4"]).err().unwrap();
        assert!(e.msg.contains("unknown --synth backend"), "{}", e.msg);
    }

    #[test]
    fn bad_input_is_refused_with_a_reason() {
        assert_eq!(
            run(&["--timeout", "0"]).err().unwrap().msg,
            "error: --timeout expects an integer >= 1, got \"0\""
        );
        assert!(
            run(&["--timeout", "9999999999"]).err().unwrap().msg.contains("expects an integer <="),
            "a value over the maximum must say so, not blame the minimum"
        );
        assert_eq!(run(&["--base"]).err().unwrap().msg, "error: --base expects a value");
        assert!(run(&["--panelist", "nope"]).err().unwrap().msg.contains("unknown panelist backend"));
        let e = run(&["--nope"]).err().unwrap();
        assert_eq!(e.msg, "unknown arg: --nope");
        assert!(e.show_help);
    }

    #[test]
    fn help_flag() {
        assert!(matches!(run(&["--help"]).ok().unwrap(), Parsed::Help));
        assert!(matches!(run(&["-h"]).ok().unwrap(), Parsed::Help));
    }
    #[test]
    fn version_flag() {
        assert!(matches!(run(&["--version"]).ok().unwrap(), Parsed::Version));
        assert!(matches!(run(&["-V"]).ok().unwrap(), Parsed::Version));
        // -V, not -v: lowercase is verbose in most tools, and this one may
        // want that later.
        assert!(run(&["-v"]).is_err());
    }
}
