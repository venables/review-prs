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

/// The --skills value that means "stage nothing".
pub const INSTALLED: &str = "installed";

/// Where a run's skills come from. Exactly one source per run, chosen up
/// front, so nobody has to work out which of three copies won.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The tree this binary was built with. The default.
    Bundled,
    /// A directory of `<name>/SKILL.md` on disk, staged in place of the
    /// bundle: a fork, a checkout of this repo, a repo's own tuned reviewer.
    Dir(PathBuf),
    /// Nothing staged. The reviewer finds whatever is installed, which is
    /// what every run did before the binaries carried the skills.
    Installed,
}

impl Source {
    /// A --skills value: the literal `installed`, or a directory holding at
    /// least one `<name>/SKILL.md`. Anything else is refused here, where the
    /// message can say what was expected, rather than by a review that
    /// found no skill.
    pub fn parse(value: &str) -> std::result::Result<Source, String> {
        if value == INSTALLED {
            return Ok(Source::Installed);
        }
        let dir = PathBuf::from(value);
        if !dir.is_dir() {
            return Err(format!(
                "error: --skills expects a directory of skills (<name>/SKILL.md) or '{INSTALLED}', got \"{value}\""
            ));
        }
        if names_in(&dir).is_empty() {
            return Err(format!(
                "error: --skills expects a directory of skills (<name>/SKILL.md), but {value} holds none"
            ));
        }
        Ok(Source::Dir(dir))
    }

    /// One line for the run to print, so the summary says which copy ran.
    pub fn describe(&self) -> String {
        match self {
            Source::Bundled => format!("bundled ({})", env!("CARGO_PKG_VERSION")),
            Source::Dir(dir) => dir.display().to_string(),
            Source::Installed => INSTALLED.to_string(),
        }
    }
}

/// The skill names under a directory on disk: every child holding a
/// `SKILL.md`, which is how the agent recognises one.
fn names_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("SKILL.md").is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort_unstable();
    names
}

/// The names a source stages, which is what an installed copy could shadow.
pub fn staged_names(source: &Source) -> Vec<String> {
    match source {
        Source::Bundled => names().into_iter().map(String::from).collect(),
        Source::Dir(dir) => names_in(dir),
        Source::Installed => Vec::new(),
    }
}

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

/// Write the bundled skills under `target`.
///
/// The embedded tree carries no file modes, and the skills run their helper
/// scripts directly (`scripts/fetch_pr_threads.sh <pr>`), so every `.sh`
/// file gets the execute bit back after it is written. A test holds the
/// repo to that spelling: an executable helper under skills/ must be a
/// `.sh`, or it would stage without the bit.
fn write_bundled(target: &Path) -> Result<()> {
    SKILLS
        .extract(target)
        .with_context(|| format!("writing the bundled skills under {}", target.display()))?;
    let mut files = Vec::new();
    files_under(&SKILLS, &mut files);
    for path in files.into_iter().filter(|p| p.extension().is_some_and(|e| e == "sh")) {
        use std::os::unix::fs::PermissionsExt;
        let script = target.join(path);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .with_context(|| format!("marking {} executable", script.display()))?;
    }
    Ok(())
}

/// Copy one tree. Symlinks are followed, not recreated: a skills directory
/// is often links into a checkout, and the agent needs the files. `fs::copy`
/// carries the mode, so a script that is executable on disk stays so.
fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        // A skill installed by git clone carries its history; the agent
        // has no use for it, and it can outweigh the skill many times over.
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let dest = to.join(entry.file_name());
        // is_dir follows a link; file_type() would report the link itself.
        if path.is_dir() {
            copy_tree(&path, &dest)?;
        } else {
            std::fs::copy(&path, &dest)
                .with_context(|| format!("copying {} to {}", path.display(), dest.display()))?;
        }
    }
    Ok(())
}

/// Copy the skills under `from` -- the `<name>/` directories parse accepted
/// and nothing else beside them -- so the staged set is exactly what
/// `staged_names` reports, and a `.git` or a log directory next to the
/// skills is never copied along.
fn copy_skills(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for name in names_in(from) {
        copy_tree(&from.join(&name), &to.join(&name))?;
    }
    Ok(())
}

/// Write the source's skills under `dir` as `<dir>/.claude/skills/<name>/...`
/// and return `dir`, which is the value `--add-dir` wants. `Installed`
/// stages nothing and returns None: there is no directory to hand over.
pub fn stage(source: &Source, dir: &Path) -> Result<Option<PathBuf>> {
    let target = dir.join(SKILLS_SUBDIR);
    match source {
        Source::Installed => return Ok(None),
        Source::Bundled => {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("creating {}", target.display()))?;
            write_bundled(&target)?;
        }
        Source::Dir(from) => copy_skills(from, &target)?,
    }
    Ok(Some(dir.to_path_buf()))
}

/// The single-token form: dash-p forwards unknown flags only that way.
pub fn add_dir_flag(dir: &Path) -> String {
    format!("--add-dir={}", dir.display())
}

/// The staged names that a skill under `skills_dir` shadows. A skill counts
/// when its `SKILL.md` exists there, symlink or not, which is how the agent
/// finds it too. Both the user's own directory and the reviewed repo's
/// `.claude/skills` win over a directory a run adds.
pub fn shadowed<'a>(skills_dir: &Path, staged: &'a [String]) -> Vec<&'a str> {
    staged
        .iter()
        .filter(|name| skills_dir.join(name).join("SKILL.md").is_file())
        .map(String::as_str)
        .collect()
}

/// What to say when skills under any of `skills_dirs` shadow the ones a
/// run staged, or nothing.
pub fn shadow_note(skills_dirs: &[PathBuf], staged: &[String]) -> Option<String> {
    let found: Vec<String> = skills_dirs
        .iter()
        .map(|dir| (dir, shadowed(dir, staged)))
        .filter(|(_, names)| !names.is_empty())
        .map(|(dir, names)| format!("{} under {}", names.join(", "), dir.display()))
        .collect();
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "note: {} shadow the staged copies; the installed skills run",
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

    fn bundled_names() -> Vec<String> {
        staged_names(&Source::Bundled)
    }

    #[test]
    fn staging_writes_the_layout_an_agent_reads() {
        let base = tmp();
        let dir = stage(&Source::Bundled, &base.join("agent")).unwrap().unwrap();
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
        let staged = bundled_names();
        assert_eq!(shadow_note(&dirs, &staged), None, "no directory, nothing shadows");
        // A directory without a SKILL.md is not a skill the agent would load.
        std::fs::create_dir_all(user.join("auto-review")).unwrap();
        assert_eq!(shadow_note(&dirs, &staged), None);
        std::fs::write(user.join("auto-review/SKILL.md"), "---\nname: auto-review\n---\n").unwrap();
        std::fs::create_dir_all(user.join("recheck-pr")).unwrap();
        std::fs::write(user.join("recheck-pr/SKILL.md"), "").unwrap();
        // An unrelated installed skill is not a shadow.
        std::fs::create_dir_all(user.join("something-else")).unwrap();
        std::fs::write(user.join("something-else/SKILL.md"), "").unwrap();
        let note = shadow_note(&dirs, &staged).unwrap();
        assert!(note.starts_with("note: auto-review, recheck-pr under "), "{note}");
        assert!(note.ends_with("shadow the staged copies; the installed skills run"), "{note}");
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
        let staged = bundled_names();
        let note = shadow_note(&[user.clone(), repo.clone()], &staged).unwrap();
        assert_eq!(
            note,
            format!(
                "note: auto-review under {} and panel-review under {} shadow the staged copies; the installed skills run",
                user.display(),
                repo.display()
            )
        );
        // A directory with nothing in it is left out of the sentence.
        let note = shadow_note(&[base.join("nothing"), repo.clone()], &staged).unwrap();
        assert!(note.starts_with("note: panel-review under "), "{note}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A directory of two skills on disk, one carrying an executable helper.
    fn skills_on_disk(base: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir = base.join("mine");
        std::fs::create_dir_all(dir.join("my-review/scripts")).unwrap();
        std::fs::write(dir.join("my-review/SKILL.md"), "---\nname: my-review\n---\n").unwrap();
        std::fs::write(dir.join("my-review/scripts/helper.sh"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            dir.join("my-review/scripts/helper.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("approve-pr/.git/objects")).unwrap();
        std::fs::write(dir.join("approve-pr/SKILL.md"), "").unwrap();
        std::fs::write(dir.join("approve-pr/.git/HEAD"), "").unwrap();
        // Not a skill: no SKILL.md. Must not be staged either.
        std::fs::create_dir_all(dir.join("notes")).unwrap();
        std::fs::write(dir.join("notes/README.md"), "").unwrap();
        // A skill that is a link into a checkout, which is how a skills
        // directory is usually assembled.
        std::fs::create_dir_all(base.join("checkout/linked-review")).unwrap();
        std::fs::write(base.join("checkout/linked-review/SKILL.md"), "").unwrap();
        std::os::unix::fs::symlink(base.join("checkout/linked-review"), dir.join("linked-review")).unwrap();
        dir
    }

    #[test]
    fn a_skills_value_is_the_literal_or_a_directory_of_skills() {
        let base = tmp();
        assert_eq!(Source::parse("installed"), Ok(Source::Installed));
        let dir = skills_on_disk(&base);
        assert_eq!(Source::parse(&dir.display().to_string()), Ok(Source::Dir(dir.clone())));
        // A path that is not there, and one that holds no skill, both say so.
        let missing = base.join("missing");
        let err = Source::parse(&missing.display().to_string()).unwrap_err();
        assert!(err.contains("expects a directory of skills") && err.contains("or 'installed'"), "{err}");
        let empty = base.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let err = Source::parse(&empty.display().to_string()).unwrap_err();
        assert!(err.ends_with("holds none"), "{err}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_directory_source_is_staged_as_it_is_on_disk() {
        use std::os::unix::fs::PermissionsExt;
        let base = tmp();
        let dir = skills_on_disk(&base);
        let source = Source::Dir(dir);
        assert_eq!(staged_names(&source), vec!["approve-pr", "linked-review", "my-review"]);
        let staged = stage(&source, &base.join("agent")).unwrap().unwrap();
        let skills = staged.join(".claude/skills");
        assert!(skills.join("my-review/SKILL.md").is_file());
        assert!(skills.join("approve-pr/SKILL.md").is_file());
        // A linked skill arrives as files, not as a link.
        assert!(skills.join("linked-review/SKILL.md").is_file());
        assert!(!skills.join("linked-review").is_symlink());
        // Nothing bundled comes along: this source replaces the bundle. And
        // nothing beside the skills comes along either.
        assert!(!skills.join("auto-review").exists());
        assert!(!skills.join("notes").exists());
        // Nor a skill's own git history.
        assert!(!skills.join("approve-pr/.git").exists());
        let mode = std::fs::metadata(skills.join("my-review/scripts/helper.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "the helper's mode came with it");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn installed_stages_nothing_and_shadows_nothing() {
        let base = tmp();
        assert_eq!(stage(&Source::Installed, &base.join("agent")).unwrap(), None);
        assert!(!base.join("agent").exists());
        assert!(staged_names(&Source::Installed).is_empty());
        assert_eq!(Source::Installed.describe(), "installed");
        assert!(Source::Bundled.describe().starts_with("bundled ("));
        let _ = std::fs::remove_dir_all(&base);
    }
}
