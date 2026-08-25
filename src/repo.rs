//! Dependency checks and repo/user context. The gh argv shapes are
//! load-bearing: the test suite's fake gh dispatches on them.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

/// A failure whose message already went to stderr at the site that understood
/// it. main() prints every other error chain -- a tool built for cron and CI
/// must never exit 1 silently -- and skips these to avoid saying it twice.
#[derive(Debug)]
pub struct AlreadyReported;

impl std::fmt::Display for AlreadyReported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "already reported")
    }
}

impl std::error::Error for AlreadyReported {}

pub struct RepoContext {
    pub owner: String,
    pub name: String,
    pub repo_root: PathBuf,
    pub me: String,
}

fn combined_output(out: &std::process::Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let e = String::from_utf8_lossy(&out.stderr);
    let e = e.trim_end();
    if !e.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(e);
    }
    s
}

/// Most names here are also their brew formula name; the few that are not get
/// a hint of their own, because a message naming a package that does not
/// exist is worse than no message.
fn dep_hint(cmd: &str) -> String {
    match cmd {
        // Ships with macOS, so this only ever prints on a slim Linux image.
        "pgrep" => "install procps (it ships with macOS)".into(),
        "dash-p" => "brew install venabots/tap/dash-p, or set $AUTOREVIEW_CMD".into(),
        other => format!("brew install {other}"),
    }
}

pub fn command_exists(cmd: &str) -> bool {
    // command -v semantics: absolute/relative paths checked directly, bare
    // names against PATH.
    if cmd.contains('/') {
        return std::path::Path::new(cmd).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| dir.join(cmd).is_file())
        })
        .unwrap_or(false)
}

/// Belt-and-suspenders: brew lists these as formula dependencies, but the
/// binary may also be run standalone on a box that never saw the tap.
pub fn require_deps(cmds: &[&str]) -> Result<()> {
    let mut missing = false;
    for cmd in cmds {
        if !command_exists(cmd) {
            eprintln!("error: missing required command: {cmd}");
            eprintln!("  install with: {}", dep_hint(cmd));
            missing = true;
        }
    }
    if missing {
        bail!(AlreadyReported);
    }
    Ok(())
}

/// The dash-p binary the built-in reviewer runs through. $DASHP_BIN is the
/// convention the other dash-p tooling honors.
pub fn dashp_bin() -> String {
    std::env::var("DASHP_BIN")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "dash-p".into())
}

/// The repo root out of `git rev-parse --show-toplevel`, or the message that
/// says why not. Pure, so each way it can go wrong is unit-testable -- the
/// order especially, since two of the three cases can be true at once.
fn repo_root_from(out: &std::process::Output) -> std::result::Result<PathBuf, String> {
    // The status first: a git that failed has nothing to say about encoding,
    // and reporting its stdout's bytes instead of its failure would send you
    // looking in the wrong place.
    if !out.status.success() {
        let detail = combined_output(out);
        let mut msg = String::from("error: not inside a git checkout");
        if !detail.is_empty() {
            msg.push('\n');
            msg.push_str(&detail);
        }
        return Err(msg);
    }
    // Refused rather than lossily converted. Every review runs in this
    // directory, and a path with a U+FFFD standing in for a byte names a
    // directory nobody has: autoreview would fail to spawn into it, and
    // review-prs would hand each tab a `cd` that cannot succeed. Saying so
    // once here beats failing once per PR with no explanation.
    //
    // The narrow cost: a checkout whose path is not valid UTF-8 is refused
    // where the bash review-prs passed the raw bytes through. macOS rejects
    // such names outright, so this only reaches an unusual Linux checkout.
    let Ok(root_utf8) = std::str::from_utf8(&out.stdout) else {
        return Err("error: the repo path is not valid UTF-8\n  \
                    reviews run in it and it is sent to a shell, so it has to be exact"
            .into());
    };
    // Exactly the one newline git adds, not every trailing whitespace-ish
    // byte. A directory name may legally end in a space -- or in a newline of
    // its own -- and eating either would name a directory nobody has, which is
    // the corruption the refusal above exists to prevent.
    let root = root_utf8.strip_suffix('\n').unwrap_or(root_utf8);
    if root.is_empty() {
        // Distinct from the failure above: git succeeded and said nothing,
        // which gh can produce by answering from $GH_REPO outside any checkout.
        // Its output rides along -- a warning on stderr is the only thing that
        // would explain a silent success.
        let detail = combined_output(out);
        let mut msg = String::from("error: git rev-parse --show-toplevel printed nothing");
        if !detail.is_empty() {
            msg.push('\n');
            msg.push_str(&detail);
        }
        return Err(msg);
    }
    Ok(PathBuf::from(root))
}

pub fn load() -> Result<RepoContext> {
    let repo_view = Command::new("gh")
        .args(["repo", "view", "--json", "owner,name"])
        .output()?;
    if !repo_view.status.success() {
        eprintln!("error: not a GitHub repo (or gh not authenticated)");
        eprintln!("{}", combined_output(&repo_view));
        bail!(AlreadyReported);
    }
    let repo_json: serde_json::Value = serde_json::from_slice(&repo_view.stdout)?;
    let owner = repo_json["owner"]["login"].as_str().unwrap_or_default().to_string();
    let name = repo_json["name"].as_str().unwrap_or_default().to_string();

    // gh can answer from $GH_REPO outside any checkout; an empty repo root
    // would hash into every session id and fail every spawn one PR at a time.
    let toplevel = Command::new("git").args(["rev-parse", "--show-toplevel"]).output()?;
    let repo_root = match repo_root_from(&toplevel) {
        Ok(root) => root,
        Err(msg) => {
            eprintln!("{msg}");
            bail!(AlreadyReported);
        }
    };

    // gh can exit 0 and still hand back an empty login, which would silently
    // mislabel every PR's engagement -- so check both the status and the value.
    let user = Command::new("gh").args(["api", "user", "--jq", ".login"]).output()?;
    if !user.status.success() {
        eprintln!("error: failed to fetch current GitHub user");
        eprintln!("{}", combined_output(&user));
        bail!(AlreadyReported);
    }
    let me = String::from_utf8_lossy(&user.stdout).trim().to_string();
    if me.is_empty() {
        eprintln!("error: gh api user returned empty login");
        bail!(AlreadyReported);
    }

    Ok(RepoContext { owner, name, repo_root, me })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    fn output(code: i32, stdout: &[u8], stderr: &str) -> std::process::Output {
        std::process::Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    #[test]
    fn a_failed_git_is_reported_as_a_failed_git() {
        // Even when its stdout is not valid UTF-8. Both are true here, and
        // naming the encoding would send you looking in the wrong place.
        let err = repo_root_from(&output(128, &[0xff], "fatal: not a git repository")).unwrap_err();
        assert!(err.starts_with("error: not inside a git checkout"));
        assert!(err.contains("fatal: not a git repository"));
    }

    #[test]
    fn a_non_utf8_root_is_refused_rather_than_mangled() {
        let err = repo_root_from(&output(0, &[b'/', 0xff, b'\n'], "")).unwrap_err();
        assert!(err.starts_with("error: the repo path is not valid UTF-8"));
    }

    #[test]
    fn a_git_that_succeeded_and_said_nothing_says_which_command() {
        let err = repo_root_from(&output(0, b"\n", "")).unwrap_err();
        assert!(err.contains("printed nothing"));
    }

    #[test]
    fn exactly_one_trailing_newline_is_stripped() {
        assert_eq!(repo_root_from(&output(0, b"/repo\n", "")).unwrap(), PathBuf::from("/repo"));
        // A directory name may legally end in a space, or in a newline of its
        // own. Eating either would name a directory nobody has -- git adds one
        // newline, so exactly one comes off.
        assert_eq!(repo_root_from(&output(0, b"/repo \n", "")).unwrap(), PathBuf::from("/repo "));
        assert_eq!(repo_root_from(&output(0, b"/repo\n\n", "")).unwrap(), PathBuf::from("/repo\n"));
        assert_eq!(repo_root_from(&output(0, b"/repo\r\n", "")).unwrap(), PathBuf::from("/repo\r"));
    }
}
