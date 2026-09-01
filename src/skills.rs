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

/// Every file under `dir`, at any depth, as a path relative to the root.
fn files_under<'a>(dir: &'a Dir<'a>, out: &mut Vec<&'a Path>) {
    for file in dir.files() {
        out.push(file.path());
    }
    for sub in dir.dirs() {
        files_under(sub, out);
    }
}

/// Write the skills under `dir` as `<dir>/.claude/skills/<name>/...` and
/// return `dir`, which is the value `--add-dir` wants.
///
/// The embedded tree carries no file modes, and the skills run their helper
/// scripts directly (`scripts/fetch_pr_threads.sh <pr>`), so every `.sh`
/// file gets the execute bit back after it is written. A test holds the
/// repo to that spelling: an executable helper under skills/ must be a
/// `.sh`, or it would stage without the bit.
pub fn stage(dir: &Path) -> Result<PathBuf> {
    let target = dir.join(SKILLS_SUBDIR);
    std::fs::create_dir_all(&target)
        .with_context(|| format!("creating {}", target.display()))?;
    SKILLS
        .extract(&target)
        .with_context(|| format!("writing the bundled skills under {}", target.display()))?;
    let mut files = Vec::new();
    files_under(&SKILLS, &mut files);
    for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "sh")) {
        use std::os::unix::fs::PermissionsExt;
        let script = target.join(path);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("marking {} executable", script.display()))?;
    }
    Ok(dir.to_path_buf())
}

/// The single-token form: dash-p forwards unknown flags only that way.
pub fn add_dir_flag(dir: &Path) -> String {
    format!("--add-dir={}", dir.display())
}

/// The bundled names that a skill under `skills_dir` shadows. A skill counts
/// when its `SKILL.md` exists there, symlink or not, which is how the agent
/// finds it too. Both the user's own directory and the reviewed repo's
/// `.claude/skills` win over a directory a run adds.
pub fn shadowed(skills_dir: &Path) -> Vec<&'static str> {
    names()
        .into_iter()
        .filter(|name| skills_dir.join(name).join("SKILL.md").is_file())
        .collect()
}

/// What to say when skills under any of `skills_dirs` shadow bundled ones,
/// or nothing.
pub fn shadow_note(skills_dirs: &[PathBuf]) -> Option<String> {
    let found: Vec<String> = skills_dirs
        .iter()
        .map(|dir| (dir, shadowed(dir)))
        .filter(|(_, names)| !names.is_empty())
        .map(|(dir, names)| format!("{} under {}", names.join(", "), dir.display()))
        .collect();
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "note: {} shadow the bundled copies; the installed skills run",
        found.join(" and ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        crate::rundir::make_unique_dir(&std::env::temp_dir(), "ar-skills.").unwrap()
    }

    #[test]
    fn every_executable_helper_in_the_repo_is_a_sh_file() {
        // stage() gives the bit back by extension. A helper with another
        // extension would be checked in executable and staged without it,
        // and fail in a review rather than here.
        use std::os::unix::fs::PermissionsExt;
        fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if std::fs::metadata(&path).unwrap().permissions().mode() & 0o111 != 0 {
                    out.push(path);
                }
            }
        }
        let mut executable = Vec::new();
        walk(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/skills")), &mut executable);
        assert!(!executable.is_empty(), "the skills carry helper scripts");
        for path in executable {
            assert!(
                path.extension().is_some_and(|e| e == "sh"),
                "{} is executable but stage() would not mark it: name it .sh",
                path.display()
            );
        }
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
        // A skill's helpers travel with it, and run as the instructions
        // run them: directly, so the execute bit the embed dropped is back.
        use std::os::unix::fs::PermissionsExt;
        for script in [
            "panel-review/panel-review.sh",
            "panel-review/pr-line-url.sh",
            "recheck-pr/scripts/fetch_pr_threads.sh",
            "auto-post-panel-review-comments/scripts/pr-line-url.sh",
        ] {
            let path = dir.join(".claude/skills").join(script);
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "{script} has mode {mode:o}");
        }
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
        let dirs = vec![user.clone()];
        assert_eq!(shadow_note(&dirs), None, "no directory, nothing shadows");
        // A directory without a SKILL.md is not a skill the agent would load.
        std::fs::create_dir_all(user.join("auto-review")).unwrap();
        assert_eq!(shadow_note(&dirs), None);
        std::fs::write(user.join("auto-review/SKILL.md"), "---\nname: auto-review\n---\n").unwrap();
        std::fs::create_dir_all(user.join("recheck-pr")).unwrap();
        std::fs::write(user.join("recheck-pr/SKILL.md"), "").unwrap();
        // An unrelated installed skill is not a shadow.
        std::fs::create_dir_all(user.join("something-else")).unwrap();
        std::fs::write(user.join("something-else/SKILL.md"), "").unwrap();
        let note = shadow_note(&dirs).unwrap();
        assert!(note.starts_with("note: auto-review, recheck-pr under "), "{note}");
        assert!(note.ends_with("shadow the bundled copies; the installed skills run"), "{note}");
        assert!(!note.contains("something-else"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_repo_s_own_skills_are_reported_beside_the_user_s() {
        let base = tmp();
        let user = base.join("user-skills");
        let repo = base.join("repo/.claude/skills");
        std::fs::create_dir_all(user.join("auto-review")).unwrap();
        std::fs::write(user.join("auto-review/SKILL.md"), "").unwrap();
        std::fs::create_dir_all(repo.join("panel-review")).unwrap();
        std::fs::write(repo.join("panel-review/SKILL.md"), "").unwrap();
        let note = shadow_note(&[user.clone(), repo.clone()]).unwrap();
        assert_eq!(
            note,
            format!(
                "note: auto-review under {} and panel-review under {} shadow the bundled copies; the installed skills run",
                user.display(),
                repo.display()
            )
        );
        // A directory with nothing in it is left out of the sentence.
        let note = shadow_note(&[base.join("nothing"), repo.clone()]).unwrap();
        assert!(note.starts_with("note: panel-review under "), "{note}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
