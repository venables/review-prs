//! review-prs's flags, env defaults and validation. Deliberately a different
//! surface from autoreview's: here the picker is the default and --auto is the
//! sweep, because a tab you can watch is the reason to reach for this tool at
//! all. Hand-rolled for the same reason as the other parser -- byte-exact
//! messages, exit 1 on bad input, and an `=`-only value form for --babysit.

use crate::cli::{CliError, EnvFn};
use crate::interval::{self, Interval};
use crate::skills::Source;

pub const HELP: &str = r#"review-prs: pick open non-draft, unapproved PRs and fan out a review per PR.

Usage: review-prs [--auto] [--babysit[=MINUTES]] [--continue]
                  [--skills DIR|installed] [--all] [--dependabot] [--help]

  --auto, -A          Skip the picker; fan out every NEW/UPDATED PR, running
                      $REVIEW_PRS_AUTO_CMD (default: the pr-review-tab skill,
                      which auto-reviews and closes the tab on approval)
                      instead of $REVIEW_PRS_CMD in each tab.
  --babysit[=MIN], -b Re-check any PR that doesn't come back approvable every
                      MIN minutes (default 30, or $REVIEW_PRS_BABYSIT_INTERVAL)
                      until it can be approved, then close its tab. Uses the
                      unattended command, so it works with the picker too.
                      A bare number is minutes; 30m/1h/2d also work.
  --continue, -C      Resume this machine's earlier review session for a PR
                      instead of reviewing it from scratch. PRs with a session
                      are marked RESUMABLE in the picker.
  --skills DIR        Which review skills the tabs run (or $REVIEW_PRS_SKILLS).
                      Unset: the ones this binary was built with, staged for
                      the run. A directory of <name>/SKILL.md: those instead.
                      The word "installed": stage nothing and let each tab
                      find what is installed. A skill installed under
                      ~/.claude/skills or the repo's .claude/skills still wins
                      over a staged one; the run says so.
  --all, -a           Include PRs already marked APPROVED (default: exclude).
  --dependabot, -d    Include Dependabot PRs (default: hidden; shown dimmed).
  --help, -h          Show this help.
  --version, -V       Show the version.

Each selected PR opens a tab that cd's to the repo root and runs a review
command. The built-in commands are:
  claude --dangerously-skip-permissions --session-id UUID "panel review N"
  claude --dangerously-skip-permissions --resume     UUID "recheck-pr N"   (-C)
where UUID is derived from the repo directory plus owner/name#N, so the same PR
in this checkout always maps to the same session. Override via $REVIEW_PRS_CMD
(the PR number replaces the first "{}", or is appended if absent):
  REVIEW_PRS_CMD='review'                              (append form)
  REVIEW_PRS_CMD='gh pr checkout {} && my-review {}'   (placeholder form)
An overridden command owns its own session handling; it receives the id as
$REVIEW_PRS_SESSION_ID and a 0/1 $REVIEW_PRS_SESSION_RESUME.

Under herdr/cmux the enclosing workspace is renamed to $REVIEW_PRS_WORKSPACE
(default "pr reviews"); set it empty to leave your workspace title alone.

To review the same PRs headlessly instead -- no tabs, a progress display, and
an exit status that reports the reviews rather than the spawns -- use
`autoreview`, which takes the same flags.

Your own PRs are always hidden -- this tool is for reviewing others' work.
"#;

#[derive(Debug, Clone)]
pub struct Config {
    /// Skip the picker and fan out every NEW/UPDATED PR.
    pub auto: bool,
    pub babysit: Option<Interval>,
    pub continue_sessions: bool,
    pub include_approved: bool,
    pub include_dependabot: bool,
    /// The override to run in each tab; None means the built-in claude
    /// invocation, which needs the PR number and the session state to pick its
    /// flags and prompt and so cannot be flattened into a template here.
    pub review_cmd: Option<String>,
    /// Where the built-in command's skills come from.
    pub skills: Source,
    /// What to rename the enclosing workspace to, or None to leave it alone.
    pub workspace: Option<String>,
    /// Printed to stderr before the run starts.
    pub startup_notes: Vec<String>,
}

impl Config {
    /// Any unattended run -- an --auto sweep OR a --babysit picker run -- takes
    /// the unattended command, so the tab self-closes on approval and loops on
    /// the interval. A plain picker run keeps the interactive one.
    pub fn unattended(&self) -> bool {
        self.auto || self.babysit.is_some()
    }
}

pub enum Parsed {
    Run(Box<Config>),
    Help,
    Version,
}

pub fn parse<I: IntoIterator<Item = String>>(args: I, env: EnvFn) -> Result<Parsed, CliError> {
    let mut auto = false;
    let mut babysit = false;
    let mut continue_sessions = false;
    let mut include_approved = false;
    let mut include_dependabot = false;

    // Kept raw until after arg parsing: validating here would make a bad
    // $REVIEW_PRS_BABYSIT_INTERVAL in a shell profile hard-fail every run,
    // including --help and ordinary picker runs that never babysit.
    let mut babysit_interval_raw = env("REVIEW_PRS_BABYSIT_INTERVAL")
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "30".into());

    let mut skills_raw = env("REVIEW_PRS_SKILLS").filter(|v| !v.is_empty());

    let mut it = args.into_iter();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--auto" | "-A" => auto = true,
            "--babysit" | "-b" => babysit = true,
            "--continue" | "-C" => continue_sessions = true,
            "--all" | "-a" => include_approved = true,
            "--dependabot" | "-d" => include_dependabot = true,
            "--help" | "-h" => return Ok(Parsed::Help),
            "--version" | "-V" => return Ok(Parsed::Version),
            "--skills" => skills_raw = Some(crate::cli::require_value("--skills", it.next())?),
            other => {
                if let Some(v) = other.strip_prefix("--babysit=") {
                    babysit = true;
                    babysit_interval_raw = v.to_string();
                } else if let Some(v) = other.strip_prefix("--skills=") {
                    skills_raw = Some(crate::cli::require_value("--skills", Some(v.to_string()))?);
                } else {
                    return Err(CliError {
                        msg: format!("unknown arg: {other}"),
                        show_help: true,
                    });
                }
            }
        }
    }

    let skills = match &skills_raw {
        Some(raw) => Source::parse(raw).map_err(|msg| CliError { msg, show_help: false })?,
        None => Source::Bundled,
    };

    // Validated only once babysitting is actually on, so an unrelated bad env
    // var never blocks a plain review run.
    let babysit_interval = if babysit {
        Some(interval::normalize(&babysit_interval_raw).map_err(|msg| CliError {
            msg,
            show_help: false,
        })?)
    } else {
        None
    };

    let cmd = env("REVIEW_PRS_CMD").filter(|v| !v.is_empty());
    let auto_cmd = env("REVIEW_PRS_AUTO_CMD").filter(|v| !v.is_empty());

    let mut startup_notes = Vec::new();
    if let (Some(_), Some(iv)) = (&auto_cmd, &babysit_interval) {
        // An overridden unattended command owns its own re-check behavior and
        // has no documented slot for the interval, so --babysit=MIN cannot
        // reach it. Say so rather than let the flag look effective.
        startup_notes.push(format!(
            "note: $REVIEW_PRS_AUTO_CMD is set; --babysit interval ({}) is not passed to it",
            iv.normalized
        ));
    }

    let unattended = auto || babysit;
    let review_cmd = if unattended { auto_cmd } else { cmd };
    if skills != Source::Bundled && review_cmd.is_some() {
        let which = if unattended { "REVIEW_PRS_AUTO_CMD" } else { "REVIEW_PRS_CMD" };
        startup_notes.push(format!(
            "note: --skills is not passed to ${which}; that command finds its own skills"
        ));
    }

    // Unset and empty mean different things here: an explicitly empty
    // REVIEW_PRS_WORKSPACE= opts out entirely, leaving the workspace you
    // launched from with the title you gave it.
    let workspace = match env("REVIEW_PRS_WORKSPACE") {
        None => Some("pr reviews".to_string()),
        Some(s) if s.is_empty() => None,
        Some(s) => Some(s),
    };

    Ok(Parsed::Run(Box::new(Config {
        auto,
        babysit: babysit_interval,
        continue_sessions,
        include_approved,
        include_dependabot,
        review_cmd,
        skills,
        workspace,
        startup_notes,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_env(args: &[&str], vars: &[(&str, &str)]) -> Result<Parsed, CliError> {
        let vars: Vec<(String, String)> =
            vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let env = move |name: &str| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        parse(args.iter().map(|s| s.to_string()), &env)
    }

    fn cfg_env(args: &[&str], vars: &[(&str, &str)]) -> Config {
        match run_env(args, vars) {
            Ok(Parsed::Run(c)) => *c,
            _ => panic!("expected a run"),
        }
    }

    fn cfg(args: &[&str]) -> Config {
        cfg_env(args, &[])
    }

    #[test]
    fn skills_default_to_the_bundle_and_take_a_directory_or_installed() {
        assert_eq!(cfg(&[]).skills, Source::Bundled);
        assert_eq!(cfg(&["--skills", "installed"]).skills, Source::Installed);
        assert_eq!(cfg(&["--skills=installed"]).skills, Source::Installed);
        let dir = crate::rundir::make_unique_dir(&std::env::temp_dir(), "rp-cli-skills.").unwrap();
        std::fs::create_dir_all(dir.join("my-review")).unwrap();
        std::fs::write(dir.join("my-review/SKILL.md"), "").unwrap();
        let path = dir.display().to_string();
        assert_eq!(cfg(&["--skills", &path]).skills, Source::Dir(dir.clone()));
        assert_eq!(cfg_env(&[], &[("REVIEW_PRS_SKILLS", &path)]).skills, Source::Dir(dir.clone()));
        assert_eq!(
            cfg_env(&["--skills", "installed"], &[("REVIEW_PRS_SKILLS", &path)]).skills,
            Source::Installed,
            "the flag wins over the env var"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_skills_value_that_names_no_skills_is_refused_up_front() {
        let e = run_env(&["--skills", "/nowhere/at/all"], &[]).err().unwrap();
        assert!(e.msg.starts_with("error: --skills expects a directory of skills"), "{}", e.msg);
        let e = run_env(&["--skills"], &[]).err().unwrap();
        assert_eq!(e.msg, "error: --skills expects a value");
        let e = run_env(&["--skills", "--auto"], &[]).err().unwrap();
        assert!(e.msg.contains("found the flag --auto"), "{}", e.msg);
    }

    #[test]
    fn skills_do_not_reach_an_override_and_the_run_says_so() {
        let c = cfg_env(&["--auto", "--skills", "installed"], &[("REVIEW_PRS_AUTO_CMD", "my-review")]);
        assert_eq!(
            c.startup_notes,
            vec!["note: --skills is not passed to $REVIEW_PRS_AUTO_CMD; that command finds its own skills"]
        );
        let c = cfg_env(&["--skills", "installed"], &[("REVIEW_PRS_CMD", "my-review")]);
        assert_eq!(
            c.startup_notes,
            vec!["note: --skills is not passed to $REVIEW_PRS_CMD; that command finds its own skills"]
        );
    }

    #[test]
    fn picking_is_the_default_and_sweeping_is_the_flag() {
        let c = cfg(&[]);
        assert!(!c.auto && !c.unattended());
        for spelling in [["--auto"], ["-A"]] {
            let c = cfg(&spelling);
            assert!(c.auto, "{spelling:?} should sweep");
            assert!(c.unattended(), "{spelling:?} is an unattended run");
        }
        // A babysit loop outlives whoever started it, picker or not.
        assert!(cfg(&["--babysit"]).unattended());
    }

    #[test]
    fn every_short_flag_matches_its_long_one() {
        assert!(cfg(&["-C"]).continue_sessions && cfg(&["--continue"]).continue_sessions);
        assert!(cfg(&["-a"]).include_approved && cfg(&["--all"]).include_approved);
        assert!(cfg(&["-d"]).include_dependabot && cfg(&["--dependabot"]).include_dependabot);
        assert!(cfg(&["-b"]).babysit.is_some() && cfg(&["--babysit"]).babysit.is_some());
    }

    #[test]
    fn babysit_takes_its_value_only_with_equals() {
        assert_eq!(cfg(&["--babysit=15"]).babysit.unwrap().normalized, "15m");
        assert_eq!(cfg(&["--babysit"]).babysit.unwrap().normalized, "30m");
        // "--babysit 15" leaves 15 a stray argument, as in autoreview.
        let e = run_env(&["--babysit", "15"], &[]).err().unwrap();
        assert_eq!(e.msg, "unknown arg: 15");
        assert!(e.show_help);
    }

    #[test]
    fn bad_input_messages_are_byte_exact() {
        let e = run_env(&["--babysit=soon"], &[]).err().unwrap();
        assert_eq!(
            e.msg,
            "error: invalid babysit interval: \"soon\" (expected a positive duration, e.g. 30, 30m, 1h)"
        );
        assert!(!e.show_help);
        let e = run_env(&["--nope"], &[]).err().unwrap();
        assert_eq!(e.msg, "unknown arg: --nope");
        assert!(e.show_help);
    }

    #[test]
    fn help_flag() {
        assert!(matches!(run_env(&["--help"], &[]).ok().unwrap(), Parsed::Help));
        assert!(matches!(run_env(&["-h"], &[]).ok().unwrap(), Parsed::Help));
    }
    #[test]
    fn version_flag() {
        assert!(matches!(run_env(&["--version"], &[]).ok().unwrap(), Parsed::Version));
        assert!(matches!(run_env(&["-V"], &[]).ok().unwrap(), Parsed::Version));
        // -V, not -v: lowercase is verbose in most tools, and this one may
        // want that later.
        assert!(run_env(&["-v"], &[]).is_err());
    }

    #[test]
    fn a_bad_interval_in_the_profile_only_bites_when_babysitting() {
        let vars = &[("REVIEW_PRS_BABYSIT_INTERVAL", "junk")];
        assert!(run_env(&[], vars).is_ok());
        assert!(run_env(&["--babysit"], vars).err().unwrap().msg.contains("invalid babysit"));
        // An env interval is what a bare --babysit picks up.
        assert_eq!(
            cfg_env(&["--babysit"], &[("REVIEW_PRS_BABYSIT_INTERVAL", "45")])
                .babysit
                .unwrap()
                .normalized,
            "45m"
        );
    }

    #[test]
    fn unattended_runs_take_the_unattended_override() {
        let both = &[("REVIEW_PRS_CMD", "mine"), ("REVIEW_PRS_AUTO_CMD", "auto-r")];
        assert_eq!(cfg_env(&[], both).review_cmd.as_deref(), Some("mine"));
        assert_eq!(cfg_env(&["--auto"], both).review_cmd.as_deref(), Some("auto-r"));
        assert_eq!(cfg_env(&["--babysit"], both).review_cmd.as_deref(), Some("auto-r"));
        // Empty is not an override -- it means "use the built-in reviewer".
        assert_eq!(cfg_env(&[], &[("REVIEW_PRS_CMD", "")]).review_cmd, None);
        // An unattended run with no unattended override falls back to the
        // built-in command, not to $REVIEW_PRS_CMD.
        assert_eq!(cfg_env(&["--auto"], &[("REVIEW_PRS_CMD", "mine")]).review_cmd, None);
    }

    #[test]
    fn an_override_is_told_the_interval_cannot_reach_it() {
        let c = cfg_env(&["--babysit=15"], &[("REVIEW_PRS_AUTO_CMD", "auto-r")]);
        assert_eq!(
            c.startup_notes,
            vec!["note: $REVIEW_PRS_AUTO_CMD is set; --babysit interval (15m) is not passed to it"]
        );
        // No override, or no babysit: nothing to warn about.
        assert!(cfg(&["--babysit=15"]).startup_notes.is_empty());
        assert!(cfg_env(&["--auto"], &[("REVIEW_PRS_AUTO_CMD", "auto-r")]).startup_notes.is_empty());
    }

    #[test]
    fn the_workspace_title_can_be_opted_out_of() {
        assert_eq!(cfg(&[]).workspace.as_deref(), Some("pr reviews"));
        assert_eq!(cfg_env(&[], &[("REVIEW_PRS_WORKSPACE", "reviews")]).workspace.as_deref(), Some("reviews"));
        assert_eq!(cfg_env(&[], &[("REVIEW_PRS_WORKSPACE", "")]).workspace, None);
    }
}
