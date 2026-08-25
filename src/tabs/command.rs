//! The shell line one tab runs, and the name that tab carries.
//!
//! Every tab is a fresh shell the terminal spawns, not a child of this
//! process -- so everything the review needs (the directory, the session, an
//! override's environment) has to ride in the command string itself.

use crate::job::override_invocation;
use crate::session::{PlannedSession, SessionFlag};
use crate::tabs::cli::Config;
use std::path::Path;

/// POSIX-safe quoting for the two strings that ride into the tab's shell: the
/// repo path and the session id. This is what bash's `printf %q` did -- a path
/// with a space or a quote in it has to stay one word.
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

/// The built-in claude invocation for one PR: the session flag it was planned
/// with, and the prompt that goes with it.
fn builtin_invocation(cfg: &Config, pr: u64, plan: &PlannedSession) -> String {
    let flag = match &plan.flag {
        SessionFlag::Pin(id) => format!("--session-id {id} "),
        SessionFlag::Resume(id) => format!("--resume {id} "),
        SessionFlag::None => String::new(),
    };
    format!(
        "claude --dangerously-skip-permissions {flag}\"{}\"",
        builtin_prompt(cfg, pr, plan.resume)
    )
}

/// The whole line: cd to the repo root, then review.
pub fn line(cfg: &Config, pr: u64, plan: &PlannedSession, repo_root: &Path) -> String {
    let cd = format!("cd {} &&", shell_quote(&repo_root.display().to_string()));
    match &cfg.review_cmd {
        None => format!("{cd} {}", builtin_invocation(cfg, pr, plan)),
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

    #[test]
    fn a_picker_run_reviews_from_scratch() {
        let line = line(&cfg(), 9, &fresh(), root());
        assert_eq!(
            line,
            format!(
                "cd /sandbox/repo && claude --dangerously-skip-permissions --session-id {SID} \"panel review 9\""
            )
        );
    }

    #[test]
    fn continue_swaps_the_prompt_as_well_as_the_flag() {
        let line = line(&cfg(), 9, &resumed(), root());
        assert!(line.contains(&format!("--resume {SID}")));
        assert!(line.ends_with("\"recheck-pr 9\""));
    }

    #[test]
    fn an_unattended_run_seeds_the_pr_review_tab_skill() {
        let mut c = cfg();
        c.auto = true;
        assert!(line(&c, 9, &fresh(), root()).ends_with("\"pr-review-tab 9\""));
        assert!(line(&c, 9, &resumed(), root()).ends_with("\"pr-review-tab 9 --recheck\""));

        c.babysit = Some(interval::normalize("15").unwrap());
        assert!(line(&c, 9, &fresh(), root()).ends_with("\"pr-review-tab 9 --babysit 15m\""));
        assert!(
            line(&c, 9, &resumed(), root()).ends_with("\"pr-review-tab 9 --recheck --babysit 15m\"")
        );
    }

    #[test]
    fn babysit_seeds_the_skill_even_from_the_picker() {
        // --babysit without --auto is still unattended: the loop outlives you.
        let mut c = cfg();
        c.babysit = Some(interval::normalize("1h").unwrap());
        assert!(line(&c, 9, &fresh(), root()).ends_with("\"pr-review-tab 9 --babysit 1h\""));
    }

    #[test]
    fn a_planned_session_with_no_flag_sends_none() {
        let plan = planned(SessionFlag::None, false);
        let line = line(&cfg(), 9, &plan, root());
        assert!(!line.contains("--session-id") && !line.contains("--resume"));
        assert!(line.contains("claude --dangerously-skip-permissions \"panel review 9\""));
    }

    #[test]
    fn an_override_gets_the_number_and_the_session() {
        let mut c = cfg();
        c.review_cmd = Some("my-review".into());
        assert_eq!(
            line(&c, 9, &fresh(), root()),
            format!(
                "export REVIEW_PRS_SESSION_ID={SID} REVIEW_PRS_SESSION_RESUME=0; cd /sandbox/repo && my-review 9"
            )
        );
        assert!(line(&c, 9, &resumed(), root()).contains("REVIEW_PRS_SESSION_RESUME=1"));
    }

    #[test]
    fn the_cd_guard_still_covers_a_compound_override() {
        let mut c = cfg();
        c.review_cmd = Some("gh pr checkout {} && my-review {}".into());
        let line = line(&c, 9, &fresh(), root());
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

        let line = line(&cfg(), 9, &fresh(), Path::new("/has space/repo"));
        assert!(line.starts_with("cd '/has space/repo' &&"));
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
