//! The shell line one tab runs, and the name that tab carries.
//!
//! Every tab is a fresh shell the terminal spawns, not a child of this
//! process -- so everything the review needs (the directory, the session, an
//! override's environment) has to ride in the command string itself.

use crate::job::override_invocation;
use crate::session::{PlannedSession, SessionFlag};
use crate::tabs::cli::Config;
use std::path::Path;

/// POSIX-safe quoting for the strings that ride into the tab's shell: the
/// repo path, the staged skills path, and the session id. A path with a
/// space or a quote in it has to reach the tab as one word.
pub fn shell_quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_./:=+,%@-".contains(&b));
    if safe {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Resuming and reviewing from scratch are different jobs, so they get
/// different prompts: a resumed session already holds the earlier review,
/// which is exactly what recheck-pr needs and what a fresh panel review would
/// throw away.
fn builtin_prompt(cfg: &Config, pr: u64, resume: bool) -> String {
    if cfg.unattended() {
        // Always the pr-review-tab skill, so the tab still self-closes on
        // approval and still loops under --babysit. The close/loop behavior
        // lives in that skill, not here -- this only seeds the command.
        let mut prompt = format!("pr-review-tab {pr}");
        if resume {
            prompt.push_str(" --recheck");
        }
        if let Some(iv) = &cfg.babysit {
            prompt.push_str(&format!(" --babysit {}", iv.normalized));
        }
        prompt
    } else if resume {
        format!("recheck-pr {pr}")
    } else {
        format!("panel review {pr}")
    }
}

/// The built-in claude invocation for one PR: the skills this binary was
/// built with, the session flag it was planned with, and the prompt that
/// goes with it. `skills_dir` is where those skills were staged; the tab is
/// told about it with --add-dir, so it finds them without an install.
fn builtin_invocation(
    cfg: &Config,
    pr: u64,
    plan: &PlannedSession,
    skills_dir: Option<&Path>,
) -> String {
    // The `=` form, not `--add-dir <dir>`: claude's --add-dir takes every
    // following argument as another directory, so with no session flag in
    // between it would eat the prompt.
    let skills = skills_dir
        .map(|d| format!("--add-dir={} ", shell_quote(&d.display().to_string())))
        .unwrap_or_default();
    let flag = match &plan.flag {
        SessionFlag::Pin(id) => format!("--session-id {id} "),
        SessionFlag::Resume(id) => format!("--resume {id} "),
        SessionFlag::None => String::new(),
    };
    format!(
        "claude --dangerously-skip-permissions {skills}{flag}\"{}\"",
        builtin_prompt(cfg, pr, plan.resume)
    )
}

/// The whole line: cd to the repo root, then review. `skills_dir` is the
/// staged skills for the built-in command; an override has no use for it,
/// and the caller does not stage one for it.
pub fn line(
    cfg: &Config,
    pr: u64,
    plan: &PlannedSession,
    repo_root: &Path,
    skills_dir: Option<&Path>,
) -> String {
    let cd = format!("cd {} &&", shell_quote(&repo_root.display().to_string()));
    match &cfg.review_cmd {
        None => format!("{cd} {}", builtin_invocation(cfg, pr, plan, skills_dir)),
        Some(cmd) => {
            // An overridden command owns its own session handling, so hand it
            // the id and let it decide.
            //
            // The export is a statement ahead of the cd rather than a prefix
            // on it. Two things that shape gets right and an assignment prefix
            // does not: an override may be a compound command -- the help text
            // documents `gh pr checkout {} && my-review {}` -- and a prefix
            // reaches only the first command of it; and putting the export
            // first keeps the `&&` guard covering the override itself, so a
            // failed cd still stops the review from running in the wrong
            // directory.
            format!(
                "export REVIEW_PRS_SESSION_ID={} REVIEW_PRS_SESSION_RESUME={}; {cd} {}",
                shell_quote(plan.sid.as_deref().unwrap_or("")),
                u8::from(plan.resume),
                override_invocation(cmd, pr)
            )
        }
    }
}

/// Name each tab for its PR and for what that tab is actually doing, e.g.
/// "PR 27 Review" / "PR 27 Babysit". Babysit wins over a re-check, and a
/// re-check over a plain sweep: each is the more specific description of what
/// the tab will do. Under herdr/cmux this name is sticky -- it survives the
/// title escapes the review command emits as it runs, which would otherwise
/// overwrite it with a generic agent-generated summary.
pub fn label(cfg: &Config, pr: u64, resume: bool) -> String {
    if cfg.babysit.is_some() {
        format!("PR {pr} Babysit")
    } else if resume {
        format!("PR {pr} Recheck")
    } else if cfg.auto {
        format!("PR {pr} Auto-Review")
    } else {
        format!("PR {pr} Review")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interval;

    const SID: &str = "7442b624-5cba-5d44-ae67-9c390cfe70a1";

    fn cfg() -> Config {
        Config {
            auto: false,
            babysit: None,
            continue_sessions: false,
            include_approved: false,
            include_dependabot: false,
            review_cmd: None,
            workspace: None,
            startup_notes: vec![],
        }
    }

    fn planned(flag: SessionFlag, resume: bool) -> PlannedSession {
        PlannedSession {
            sid: Some(SID.to_string()),
            flag,
            resume,
            note: None,
        }
    }

    fn fresh() -> PlannedSession {
        planned(SessionFlag::Pin(SID.to_string()), false)
    }

    fn resumed() -> PlannedSession {
        planned(SessionFlag::Resume(SID.to_string()), true)
    }

    fn root() -> &'static Path {
        Path::new("/sandbox/repo")
    }

    fn skills() -> Option<&'static Path> {
        Some(Path::new("/sandbox/skills"))
    }

    #[test]
    fn a_picker_run_reviews_from_scratch() {
        let line = line(&cfg(), 9, &fresh(), root(), skills());
        assert_eq!(
            line,
            format!(
                "cd /sandbox/repo && claude --dangerously-skip-permissions --add-dir=/sandbox/skills --session-id {SID} \"panel review 9\""
            )
        );
    }

    #[test]
    fn continue_swaps_the_prompt_as_well_as_the_flag() {
        let line = line(&cfg(), 9, &resumed(), root(), skills());
        assert!(line.contains(&format!("--resume {SID}")));
        assert!(line.ends_with("\"recheck-pr 9\""));
    }

    #[test]
    fn an_unattended_run_seeds_the_pr_review_tab_skill() {
        let mut c = cfg();
        c.auto = true;
        assert!(line(&c, 9, &fresh(), root(), skills()).ends_with("\"pr-review-tab 9\""));
        assert!(line(&c, 9, &resumed(), root(), skills()).ends_with("\"pr-review-tab 9 --recheck\""));

        c.babysit = Some(interval::normalize("15").unwrap());
        assert!(line(&c, 9, &fresh(), root(), skills()).ends_with("\"pr-review-tab 9 --babysit 15m\""));
        assert!(
            line(&c, 9, &resumed(), root(), skills()).ends_with("\"pr-review-tab 9 --recheck --babysit 15m\"")
        );
    }

    #[test]
    fn babysit_seeds_the_skill_even_from_the_picker() {
        // --babysit without --auto is still unattended: the loop outlives you.
        let mut c = cfg();
        c.babysit = Some(interval::normalize("1h").unwrap());
        assert!(line(&c, 9, &fresh(), root(), skills()).ends_with("\"pr-review-tab 9 --babysit 1h\""));
    }

    #[test]
    fn a_planned_session_with_no_flag_sends_none() {
        let plan = planned(SessionFlag::None, false);
        let line = line(&cfg(), 9, &plan, root(), skills());
        assert!(!line.contains("--session-id") && !line.contains("--resume"));
        // With no session flag the prompt follows --add-dir directly, which is
        // the case the `=` form exists for.
        assert!(line.contains("claude --dangerously-skip-permissions --add-dir=/sandbox/skills \"panel review 9\""));
    }

    #[test]
    fn an_override_gets_the_number_and_the_session() {
        let mut c = cfg();
        c.review_cmd = Some("my-review".into());
        assert_eq!(
            line(&c, 9, &fresh(), root(), skills()),
            format!(
                "export REVIEW_PRS_SESSION_ID={SID} REVIEW_PRS_SESSION_RESUME=0; cd /sandbox/repo && my-review 9"
            )
        );
        assert!(line(&c, 9, &resumed(), root(), skills()).contains("REVIEW_PRS_SESSION_RESUME=1"));
    }

    #[test]
    fn the_cd_guard_still_covers_a_compound_override() {
        let mut c = cfg();
        c.review_cmd = Some("gh pr checkout {} && my-review {}".into());
        let line = line(&c, 9, &fresh(), root(), skills());
        assert!(line.starts_with("export REVIEW_PRS_SESSION_ID="));
        assert!(line.contains("; cd /sandbox/repo && gh pr checkout 9 && my-review 9"));
    }

    #[test]
    fn a_path_that_needs_quoting_stays_one_word() {
        assert_eq!(shell_quote("/plain/path"), "/plain/path");
        assert_eq!(shell_quote("/has space/repo"), "'/has space/repo'");
        assert_eq!(shell_quote("/it's here"), r"'/it'\''s here'");
        // An empty id is still one (empty) word rather than nothing at all.
        assert_eq!(shell_quote(""), "''");

        let line = line(&cfg(), 9, &fresh(), Path::new("/has space/repo"), Some(Path::new("/tmp dir/skills")));
        assert!(line.starts_with("cd '/has space/repo' &&"));
        // The skills directory is a temp path, so it gets the same care.
        assert!(line.contains("--add-dir='/tmp dir/skills' "), "{line}");
    }

    #[test]
    fn labels_name_what_the_tab_is_doing() {
        let mut c = cfg();
        assert_eq!(label(&c, 9, false), "PR 9 Review");
        assert_eq!(label(&c, 9, true), "PR 9 Recheck");
        c.auto = true;
        assert_eq!(label(&c, 9, false), "PR 9 Auto-Review");
        // A re-check is the more specific description of an --auto -C tab...
        assert_eq!(label(&c, 9, true), "PR 9 Recheck");
        // ...and babysitting is more specific still.
        c.babysit = Some(interval::normalize("30").unwrap());
        assert_eq!(label(&c, 9, true), "PR 9 Babysit");
        assert_eq!(label(&c, 9, false), "PR 9 Babysit");
    }
}
