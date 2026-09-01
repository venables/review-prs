//! Review-session continuity: the one derivation both front-ends use, so
//! they always agree on which session a PR belongs to. The golden tests below
//! pin the ids -- they are what a review already on disk was filed under, so
//! changing this derivation orphans every existing session.
//!
//! A PR's session id is derived from the repo directory plus owner/name#NUM
//! rather than recorded in a state file: the mapping is stable across runs
//! with nothing to keep in sync. $repo_root is in the hash on purpose -- a
//! second clone or worktree of the same repo gets its own id for the same PR,
//! so one checkout can never resume (and corrupt) another's session.

use md5::{Digest, Md5};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The v5-form UUID claude --session-id accepts. Byte 6's high nibble is the
/// version (5) and byte 8's is the variant (a => 10xx), which is why digest
/// nibbles 12 and 16 are replaced by literals rather than kept.
pub fn pr_session_id(repo_root: &Path, owner: &str, name: &str, n: u64) -> String {
    let input = format!("review-prs:{}:{}/{}#{}", repo_root.display(), owner, name, n);
    let h = format!("{:x}", Md5::digest(input.as_bytes()));
    format!(
        "{}-{}-5{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

/// Claude Code's configuration directory: $CLAUDE_CONFIG_DIR, else ~/.claude.
/// Sessions, and the user's own skills, live under it.
pub fn config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_default();
            PathBuf::from(home).join(".claude")
        })
}

/// Where Claude Code keeps its per-project session stores.
pub fn projects_dir() -> PathBuf {
    config_dir().join("projects")
}

/// Where the user's own skills live. A name found here beats the same name
/// in any directory the agent is handed with --add-dir.
pub fn user_skills_dir() -> PathBuf {
    config_dir().join("skills")
}

/// True when a session with this id already exists locally. Sessions live
/// under a directory named for the escaped cwd, but that escaping is
/// undocumented and has changed before -- so look in every project directory
/// instead of rebuilding the name. Searching wider than the repo is safe
/// because the id already encodes the checkout.
pub fn session_exists(id: &str) -> bool {
    transcript_path(id).is_some()
}

/// The transcript file for a session, wherever Claude Code put it.
///
/// Every assistant turn is in here, which is what makes it worth finding: a
/// dash-p `answer` holds only the final message, so a reviewer that says
/// anything after its review -- a sign-off, a note that the script exited --
/// leaves the review itself with nowhere else to be read from.
pub fn transcript_path(id: &str) -> Option<PathBuf> {
    let dir = projects_dir();
    if id.is_empty() || !dir.is_dir() {
        return None;
    }
    let file = format!("{id}.jsonl");
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path().join(&file))
        .find(|p| p.is_file())
}

/// True when another process still holds this session open. Claude Code
/// treats an id as taken once the transcript file exists, so it does not stop
/// a second process from reopening a live one -- two agents would then write
/// one transcript. pgrep matches the id on any command line; a false match
/// only costs a fresh review, so it fails safe.
pub fn session_in_use(id: &str) -> bool {
    if id.is_empty() {
        return false;
    }
    Command::new("pgrep")
        .args(["-f", "--", id])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A well-formed id and nothing else goes into a command line: anything that
/// is not one is dropped rather than word-split into argv.
pub fn is_uuid_shaped(s: &str) -> bool {
    s.len() == 36 && s.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// How a PR attaches to a session.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionFlag {
    /// `--session-id <id>`: pin a fresh, derived id.
    Pin(String),
    /// `--resume <id>`: reopen an earlier review.
    Resume(String),
    /// No flag; claude allocates its own id.
    None,
}

#[derive(Debug, Clone)]
pub struct PlannedSession {
    pub sid: Option<String>,
    pub flag: SessionFlag,
    pub resume: bool,
    /// A message for the user, surfaced by the caller so it cannot land in
    /// the middle of the progress display.
    pub note: Option<String>,
}

/// How a PR attaches to a session. Without --continue
/// an existing session gets no flag at all and claude allocates a fresh id:
/// reusing a taken id is a hard error, and quietly resuming would be a
/// surprise.
pub fn plan_session(
    repo_root: &Path,
    owner: &str,
    name: &str,
    n: u64,
    continue_sessions: bool,
) -> PlannedSession {
    let sid = pr_session_id(repo_root, owner, name, n);
    if session_exists(&sid) {
        if continue_sessions {
            if session_in_use(&sid) {
                return PlannedSession {
                    sid: Some(sid),
                    flag: SessionFlag::None,
                    resume: false,
                    note: Some(format!(
                        "note: PR #{n} has a review session open in another tab or process; reviewing fresh"
                    )),
                };
            }
            return PlannedSession {
                sid: Some(sid.clone()),
                flag: SessionFlag::Resume(sid),
                resume: true,
                note: None,
            };
        }
        PlannedSession {
            sid: Some(sid),
            flag: SessionFlag::None,
            resume: false,
            note: None,
        }
    } else {
        PlannedSession {
            sid: Some(sid.clone()),
            flag: SessionFlag::Pin(sid),
            resume: false,
            note: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // md5 of "review-prs:<root>:<owner>/<name>#<n>", nibbles 12 and 16
    // replaced by the version/variant literals. Pinned because sessions
    // already on disk were filed under these ids: changing the derivation
    // does not fail loudly, it quietly stops finding anyone's earlier review.
    #[test]
    fn golden_session_ids_are_stable() {
        assert_eq!(
            pr_session_id(Path::new("/sandbox/repo"), "acme", "widgets", 9),
            "7442b624-5cba-5d44-ae67-9c390cfe70a1"
        );
        assert_eq!(
            pr_session_id(Path::new("/tmp/x"), "octo", "repo", 123),
            "80e25f6a-45b7-5246-a9e2-8feda1021531"
        );
    }

    #[test]
    fn ids_are_v5_form_uuids() {
        let id = pr_session_id(Path::new("/a"), "b", "c", 1);
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "5");
        assert_eq!(&id[19..20], "a");
        assert!(is_uuid_shaped(&id));
    }

    #[test]
    fn different_checkouts_derive_different_ids() {
        let a = pr_session_id(Path::new("/clone-a"), "acme", "widgets", 9);
        let b = pr_session_id(Path::new("/clone-b"), "acme", "widgets", 9);
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_shape_guard() {
        assert!(is_uuid_shaped("7442b624-5cba-5d44-ae67-9c390cfe70a1"));
        assert!(!is_uuid_shaped("not-a-session-id"));
        assert!(!is_uuid_shaped(""));
        assert!(!is_uuid_shaped("7442b624-5cba-5d44-ae67-9c390cfe70a1x"));
    }

    #[test]
    fn session_exists_finds_a_transcript() {
        let tmp = std::env::temp_dir().join(format!("ar-sess-test-{}", std::process::id()));
        let store = tmp.join("projects").join("-some-project");
        std::fs::create_dir_all(&store).unwrap();
        // Env vars are process-global; tests in this file that touch
        // CLAUDE_CONFIG_DIR run serially because cargo runs same-name-prefix
        // tests in one process -- keep this the only test that sets it.
        unsafe { std::env::set_var("CLAUDE_CONFIG_DIR", &tmp) };
        let id = "00000000-0000-5000-a000-000000000001";
        assert!(!session_exists(id));
        std::fs::write(store.join(format!("{id}.jsonl")), "{}").unwrap();
        assert!(session_exists(id));
        // The same lookup hands the path to the review reader, which is the
        // only way it can find what a reviewer said before its last message.
        // Asserted here rather than in report.rs so that the one test setting
        // CLAUDE_CONFIG_DIR stays the one test setting it.
        assert_eq!(transcript_path(id).unwrap(), store.join(format!("{id}.jsonl")));
        assert_eq!(transcript_path("00000000-0000-5000-a000-000000000002"), None);
        assert_eq!(transcript_path(""), None);
        unsafe { std::env::remove_var("CLAUDE_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
