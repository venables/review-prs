//! Everything one run writes under --log-dir. Each run gets a directory of
//! its own: a --log-dir is a fixed path -- the point of passing one -- and
//! cron makes overlapping runs ordinary, with the default hour-long timeout
//! longer than most intervals. Sharing state would have runs reading each
//! other's results and reporting them as their own.
//!
//! The run directory is made, not named: a pid-named directory would be
//! inherited by whichever later run the kernel hands that pid to, and a
//! persistent --log-dir on a container -- low pids, recycled fast -- would
//! put that run inside an older one's state.

use crate::session::is_uuid_shaped;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

fn random_suffix(attempt: u32) -> String {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 20
        ^ (attempt as u64) << 40;
    let alphabet = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut n = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    (0..6)
        .map(|_| {
            n = n.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            alphabet[(n >> 33) as usize % alphabet.len()] as char
        })
        .collect()
}

fn make_unique_dir(parent: &Path, prefix: &str) -> Result<PathBuf> {
    for attempt in 0..100 {
        let candidate = parent.join(format!("{prefix}{}", random_suffix(attempt)));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).context(format!("creating {}", candidate.display())),
        }
    }
    bail!("could not create a unique directory under {}", parent.display());
}

pub struct RunDir {
    pub log_dir: PathBuf,
    pub root: PathBuf,
    pub pass_dir: PathBuf,
}

impl RunDir {
    pub fn new(log_dir: Option<PathBuf>) -> Result<RunDir> {
        let log_dir = match log_dir {
            Some(d) => {
                std::fs::create_dir_all(&d).context("creating --log-dir")?;
                d
            }
            None => {
                let tmp = std::env::temp_dir();
                make_unique_dir(&tmp, "autoreview.")?
            }
        };
        let root = make_unique_dir(&log_dir, "run-")?;
        Ok(RunDir { log_dir, pass_dir: root.clone(), root })
    }

    pub fn start_pass(&mut self, pass: u32) -> Result<&Path> {
        self.pass_dir = self.root.join(format!("pass-{pass}"));
        std::fs::create_dir_all(&self.pass_dir)?;
        Ok(&self.pass_dir)
    }

    pub fn stdout_path(&self, pr: u64) -> PathBuf {
        self.pass_dir.join(format!("pr-{pr}.json"))
    }
    pub fn log_path(&self, pr: u64) -> PathBuf {
        self.pass_dir.join(format!("pr-{pr}.log"))
    }
    pub fn meta_path(&self, pr: u64) -> PathBuf {
        self.pass_dir.join(format!("pr-{pr}.meta.json"))
    }
    fn session_file(&self, pr: u64) -> PathBuf {
        self.root.join(format!("session-{pr}.id"))
    }
    fn failed_marker(&self, pr: u64) -> PathBuf {
        self.root.join(format!("failed-{pr}"))
    }

    /// The session a review of this PR actually ran in, for a later babysit
    /// pass to resume. Written tmp-then-rename so a reader can never see half
    /// an id.
    pub fn record_session(&self, pr: u64, id: &str) -> Result<()> {
        let path = self.session_file(pr);
        let tmp = path.with_extension("id.tmp");
        std::fs::write(&tmp, id)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// A recorded id goes into a command line, so anything that is not one is
    /// dropped rather than word-split into argv.
    pub fn recorded_session(&self, pr: u64) -> Option<String> {
        let s = std::fs::read_to_string(self.session_file(pr)).ok()?;
        is_uuid_shaped(&s).then_some(s)
    }

    /// A pass that failed leaves nothing to re-check: without this marker the
    /// next pass would fall through to the derived session and "re-check"
    /// whatever an earlier run left on disk.
    pub fn mark_failed(&self, pr: u64) {
        let _ = std::fs::write(self.failed_marker(pr), "");
    }
    pub fn clear_failed(&self, pr: u64) {
        let _ = std::fs::remove_file(self.failed_marker(pr));
    }
    pub fn last_pass_failed(&self, pr: u64) -> bool {
        self.failed_marker(pr).is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_base() -> PathBuf {
        let d = std::env::temp_dir().join(format!("ar-rundir-{}-{}", std::process::id(), random_suffix(7)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn two_runs_sharing_a_log_dir_get_separate_roots() {
        let base = tmp_base();
        let a = RunDir::new(Some(base.clone())).unwrap();
        let b = RunDir::new(Some(base.clone())).unwrap();
        assert_ne!(a.root, b.root);
        assert!(a.root.starts_with(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn session_records_round_trip_and_reject_garbage() {
        let base = tmp_base();
        let rd = RunDir::new(Some(base.clone())).unwrap();
        assert_eq!(rd.recorded_session(9), None);
        rd.record_session(9, "7442b624-5cba-5d44-ae67-9c390cfe70a1").unwrap();
        assert_eq!(
            rd.recorded_session(9).as_deref(),
            Some("7442b624-5cba-5d44-ae67-9c390cfe70a1")
        );
        rd.record_session(8, "not-a-session-id").unwrap();
        assert_eq!(rd.recorded_session(8), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn failed_markers() {
        let base = tmp_base();
        let rd = RunDir::new(Some(base.clone())).unwrap();
        assert!(!rd.last_pass_failed(9));
        rd.mark_failed(9);
        assert!(rd.last_pass_failed(9));
        rd.clear_failed(9);
        assert!(!rd.last_pass_failed(9));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn pass_dirs_nest_under_the_run() {
        let base = tmp_base();
        let mut rd = RunDir::new(Some(base.clone())).unwrap();
        rd.start_pass(1).unwrap();
        assert!(rd.stdout_path(9).ends_with("pass-1/pr-9.json"));
        rd.start_pass(2).unwrap();
        assert!(rd.log_path(9).ends_with("pass-2/pr-9.log"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
