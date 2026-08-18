//! Dependency checks and repo/user context, mirroring lib/repo.sh. The gh
//! argv shapes are load-bearing: the test suite's fake gh dispatches on them.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

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
        bail!("missing dependencies");
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

pub fn load() -> Result<RepoContext> {
    let repo_view = Command::new("gh")
        .args(["repo", "view", "--json", "owner,name"])
        .output()?;
    if !repo_view.status.success() {
        eprintln!("error: not a GitHub repo (or gh not authenticated)");
        eprintln!("{}", combined_output(&repo_view));
        bail!("no repo context");
    }
    let repo_json: serde_json::Value = serde_json::from_slice(&repo_view.stdout)?;
    let owner = repo_json["owner"]["login"].as_str().unwrap_or_default().to_string();
    let name = repo_json["name"].as_str().unwrap_or_default().to_string();

    let toplevel = Command::new("git").args(["rev-parse", "--show-toplevel"]).output()?;
    let repo_root = PathBuf::from(String::from_utf8_lossy(&toplevel.stdout).trim());

    // gh can exit 0 and still hand back an empty login, which would silently
    // mislabel every PR's engagement -- so check both the status and the value.
    let user = Command::new("gh").args(["api", "user", "--jq", ".login"]).output()?;
    if !user.status.success() {
        eprintln!("error: failed to fetch current GitHub user");
        eprintln!("{}", combined_output(&user));
        bail!("no user");
    }
    let me = String::from_utf8_lossy(&user.stdout).trim().to_string();
    if me.is_empty() {
        eprintln!("error: gh api user returned empty login");
        bail!("empty login");
    }

    Ok(RepoContext { owner, name, repo_root, me })
}
