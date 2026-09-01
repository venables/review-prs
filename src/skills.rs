//! The review skills, compiled in.
//!
//! The binaries invoke skills by name, and an agent finds a skill on disk.
//! So the tree under `skills/` is embedded at build time and written out
//! again for each run, into a directory the agent is told about with
//! `--add-dir`. A Homebrew install then needs nothing else, and the skill a
//! review ran is the one this version was built with.
//!
//! Installed skills still win. Claude Code resolves a name from the user's
//! own skills directory before any directory a flag adds, which is the
//! override an operator wants: a copy they installed on purpose beats the
//! one that came with the binary. It is also silent, so the shadowing is
//! reported at startup rather than discovered from a review that read the
//! wrong instructions.

use anyhow::{Context, Result};
use include_dir::{Dir, include_dir};
use std::path::{Path, PathBuf};

static SKILLS: Dir<'static> = include_dir!("$CARGO_MANIFEST_DIR/skills");

/// Where an agent looks for skills inside a directory it is handed.
const SKILLS_SUBDIR: &str = ".claude/skills";

/// The bundled skill names, sorted.
pub fn names() -> Vec<&'static str> {
    let mut names: Vec<_> = SKILLS
        .dirs()
        .filter_map(|d| d.path().file_name()?.to_str())
        .collect();
    names.sort_unstable();
    names
}

/// Write the skills under `dir` as `<dir>/.claude/skills/<name>/...` and
/// return `dir`, which is the value `--add-dir` wants.
pub fn stage(dir: &Path) -> Result<PathBuf> {
    let target = dir.join(SKILLS_SUBDIR);
    std::fs::create_dir_all(&target)
        .with_context(|| format!("creating {}", target.display()))?;
    SKILLS
        .extract(&target)
        .with_context(|| format!("writing the bundled skills under {}", target.display()))?;
    Ok(dir.to_path_buf())
}

/// The single-token form: dash-p forwards unknown flags only that way.
pub fn add_dir_flag(dir: &Path) -> String {
    format!("--add-dir={}", dir.display())
}

/// The bundled names that an installed skill shadows. A skill is installed
/// when its `SKILL.md` exists under the user's skills directory, symlink or
/// not, which is how the agent finds it too.
pub fn shadowed(user_skills_dir: &Path) -> Vec<&'static str> {
    names()
        .into_iter()
        .filter(|name| user_skills_dir.join(name).join("SKILL.md").is_file())
        .collect()
}

/// What to say when installed skills shadow bundled ones, or nothing.
pub fn shadow_note(user_skills_dir: &Path) -> Option<String> {
    let shadowed = shadowed(user_skills_dir);
    if shadowed.is_empty() {
        return None;
    }
    let dir = user_skills_dir.display();
    Some(format!(
        "note: {} under {dir} shadow the bundled copies; the installed skills run",
        shadowed.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ar-skills-{}-{}",
            std::process::id(),
            crate::rundir::make_unique_dir(&std::env::temp_dir(), "ar-skills-seed.")
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn every_skill_the_binaries_invoke_is_bundled() {
        // The prompts in job.rs and tabs/command.rs name these. A rename on
        // either side must fail here, not in a review that found no skill.
        let names = names();
        for wanted in ["auto-review", "panel-review", "recheck-pr", "pr-review-tab"] {
            assert!(names.contains(&wanted), "{wanted} is not bundled: {names:?}");
        }
        // And the ones those call.
        for wanted in ["approve-pr", "auto-post-panel-review-comments"] {
            assert!(names.contains(&wanted), "{wanted} is not bundled: {names:?}");
        }
    }

    #[test]
    fn staging_writes_the_layout_an_agent_reads() {
        let base = tmp();
        let dir = stage(&base.join("agent")).unwrap();
        assert_eq!(dir, base.join("agent"));
        for name in names() {
            let skill = dir.join(".claude/skills").join(name).join("SKILL.md");
            assert!(skill.is_file(), "{} was not written", skill.display());
        }
        // A skill's helpers travel with it: the instructions name them.
        assert!(dir.join(".claude/skills/panel-review/panel-review.sh").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_flag_is_one_token() {
        let flag = add_dir_flag(Path::new("/run/agent"));
        assert_eq!(flag, "--add-dir=/run/agent");
        assert!(!flag.contains(' '));
    }

    #[test]
    fn shadowing_is_reported_by_name_and_only_when_real() {
        let base = tmp();
        let user = base.join("skills");
        assert_eq!(shadow_note(&user), None, "no directory, nothing shadows");
        // A directory without a SKILL.md is not a skill the agent would load.
        std::fs::create_dir_all(user.join("auto-review")).unwrap();
        assert_eq!(shadow_note(&user), None);
        std::fs::write(user.join("auto-review/SKILL.md"), "---\nname: auto-review\n---\n").unwrap();
        std::fs::create_dir_all(user.join("recheck-pr")).unwrap();
        std::fs::write(user.join("recheck-pr/SKILL.md"), "").unwrap();
        // An unrelated installed skill is not a shadow.
        std::fs::create_dir_all(user.join("something-else")).unwrap();
        std::fs::write(user.join("something-else/SKILL.md"), "").unwrap();
        let note = shadow_note(&user).unwrap();
        assert!(note.starts_with("note: auto-review, recheck-pr under "), "{note}");
        assert!(note.ends_with("shadow the bundled copies; the installed skills run"), "{note}");
        assert!(!note.contains("something-else"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
