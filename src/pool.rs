//! The pass engine: a bounded pool of review subprocesses, driven by events
//! rather than polling. Each job gets a monitor thread whose entire body is
//! `child.wait()` -- which is what makes a job killed from outside ordinary
//! rather than a special case: it is a wait() that returns a signal-death
//! status, and the slot frees immediately.

use crate::cli::Config;
use crate::job::{self, GUARD_GRACE_SECS, Job, JobState};
use crate::report;
use crate::repo::RepoContext;
use crate::rundir::RunDir;
use crate::session::{self, SessionFlag};
use crate::ui::Ui;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use std::collections::HashMap;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub enum Event {
    /// The child was reaped. Sent the moment wait() returns, before the
    /// GitHub readback delays JobExited: the deadline guard must never fire
    /// for a review that already exited, and the recorded time must not
    /// include the readback's duration.
    JobReaped { idx: usize, elapsed_secs: u64 },
    JobExited {
        idx: usize,
        status: std::process::ExitStatus,
        /// The GitHub verdict readback, run on the monitor thread so a slow
        /// answer never blocks the event loop. None when the job did not
        /// exit cleanly -- an unfinished review has no verdict to read.
        readback: Option<report::Readback>,
    },
    Signal,
}

/// Stop one job: every process in its group, TERM first so a well-behaved
/// reviewer can clean up, then KILL, which cannot be ignored -- a reviewer
/// that shrugs off TERM must not outlive its timeout, because the pass waits
/// on it. One killpg covers everything the job spawned: the group was created
/// at spawn, so there is no tree to walk and no reparenting race to lose.
pub fn stop_group(pgid: i32) {
    let pg = Pid::from_raw(pgid);
    let _ = killpg(pg, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(200));
    let _ = killpg(pg, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(200));
    let _ = killpg(pg, Signal::SIGKILL);
}

/// Ctrl-C (or a dropped ssh session) must not leave reviews running, and the
/// reviews that already finished are still worth reopening -- so hand back
/// their session ids on the way out rather than dropping them.
fn interrupt(jobs: &[Job], ui: &mut Ui, rundir: &RunDir) -> ! {
    // The board first: its tick threads keep repainting until it is torn
    // down, and would overdraw the interrupt message below.
    ui.end_pass();
    println!();
    eprintln!("interrupted; stopping running reviews");
    for job in jobs {
        if let Some(pgid) = job.pgid
            && !matches!(job.state, JobState::Done | JobState::Failed | JobState::Timeout)
        {
            stop_group(pgid);
        }
    }
    if !jobs.is_empty() {
        ui.print_summary(jobs, &rundir.pass_dir);
    }
    ui.show_cursor();
    std::process::exit(130);
}

struct Deadline {
    at: Instant,
}

/// Decide how PR #n attaches to a session for this pass. The recorded id --
/// the session an earlier pass of this run actually reviewed in -- outranks
/// the derived one: where a session already existed before the run, pass 1
/// reviewed under an id claude allocated, and the derived id names something
/// older. A failed pass leaves nothing to re-check, so it reviews fresh;
/// fresh still pins the derived id when no transcript exists, so the review
/// stays reachable afterwards.
fn plan_job(job: &mut Job, cfg: &Config, ctx: &RepoContext, rundir: &RunDir, ui: &mut Ui) {
    let mut prior = if cfg.continue_sessions { rundir.recorded_session(job.pr) } else { None };
    let mut busy = false;
    if let Some(p) = &prior
        && session::session_in_use(p)
    {
        ui.note(format!(
            "note: PR #{} has its review session open elsewhere; reviewing fresh",
            job.pr
        ));
        prior = None;
        busy = true;
    }
    let fresh = rundir.last_pass_failed(job.pr);

    if busy || fresh {
        let derived = session::pr_session_id(&ctx.repo_root, &ctx.owner, &ctx.name, job.pr);
        if !session::session_exists(&derived) {
            job.sid = Some(derived.clone());
            job.flag = SessionFlag::Pin(derived);
        } else {
            job.sid = None;
            job.flag = SessionFlag::None;
        }
        job.resume = false;
    } else if let Some(p) = prior {
        job.sid = Some(p.clone());
        job.flag = SessionFlag::Resume(p);
        job.resume = true;
    } else {
        let planned = session::plan_session(
            &ctx.repo_root,
            &ctx.owner,
            &ctx.name,
            job.pr,
            cfg.continue_sessions,
        );
        if let Some(note) = planned.note {
            ui.note(note);
        }
        job.sid = planned.sid;
        job.flag = planned.flag;
        job.resume = planned.resume;
        // Where this pass's half of the transcript begins. A resumed session
        // already holds every earlier review, and those belong to the passes
        // that produced them, not to this one.
        job.transcript_from = job
            .sid
            .as_deref()
            .and_then(crate::session::transcript_path)
            .and_then(|p| std::fs::metadata(p).ok())
            .map_or(0, |m| m.len());
    }
}

fn deadline_for(cfg: &Config, is_override: bool) -> Option<Duration> {
    if cfg.timeout_secs == 0 {
        return None;
    }
    // dash-p enforces the real cap itself; the grace only covers its one
    // known hang hole. An override has no inner enforcement, so its deadline
    // is exact.
    let secs = if is_override { cfg.timeout_secs } else { cfg.timeout_secs + GUARD_GRACE_SECS };
    Some(Duration::from_secs(secs))
}

#[allow(clippy::too_many_arguments)]
pub fn run_pass(
    queue: &[u64],
    info: &HashMap<u64, crate::prlist::PrInfo>,
    cfg: &Config,
    ctx: &RepoContext,
    rundir: &RunDir,
    dashp: &str,
    rx: &Receiver<Event>,
    tx: &Sender<Event>,
    ui: &mut Ui,
) -> Vec<Job> {
    let is_override = cfg.review_cmd.is_some();
    let mut jobs: Vec<Job> = queue
        .iter()
        .map(|&n| {
            let mut job = Job::new(n);
            if let Some(meta) = info.get(&n) {
                job.title = meta.title.clone();
                job.author = meta.author.clone();
            }
            job
        })
        .collect();
    let mut deadlines: Vec<Option<Deadline>> = queue.iter().map(|_| None).collect();
    let total = jobs.len();
    let jobs_max = cfg.jobs as usize;

    ui.begin_pass(total, cfg.jobs, &rundir.pass_dir);

    let mut running = 0usize;
    let mut next = 0usize;
    let mut finished = 0usize;

    while finished < total {
        // Fill free slots in queue order -- the order the tests (and eyes)
        // expect the starts to happen.
        while running < jobs_max && next < total {
            let idx = next;
            next += 1;
            plan_job(&mut jobs[idx], cfg, ctx, rundir, ui);
            match job::spawn(&jobs[idx], cfg, ctx, rundir, dashp) {
                Ok(child) => {
                    let started = Instant::now();
                    jobs[idx].pgid = Some(child.id() as i32);
                    jobs[idx].started = Some(started);
                    jobs[idx].started_epoch = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    jobs[idx].state = JobState::Running;
                    deadlines[idx] =
                        deadline_for(cfg, is_override).map(|d| Deadline { at: Instant::now() + d });
                    ui.note_transition(&jobs[idx]);
                    running += 1;
                    let tx = tx.clone();
                    let pr = jobs[idx].pr;
                    let me = ctx.me.clone();
                    let started_epoch = jobs[idx].started_epoch;
                    std::thread::spawn(move || {
                        let mut child = child;
                        // A wait() error (ECHILD, if anything else reaped the
                        // child) still frees the slot: a dropped event would
                        // hang the pass forever, in a tool built for cron.
                        let status = child.wait().unwrap_or_else(|_| {
                            std::os::unix::process::ExitStatusExt::from_raw(127 << 8)
                        });
                        // Reap first, then read back: JobReaped disarms the
                        // deadline and pins the elapsed time, so the readback
                        // below -- off the event loop, bounded by its own
                        // deadline -- delays only this slot's release, never
                        // the guard, the clock, or the other jobs.
                        let _ = tx.send(Event::JobReaped {
                            idx,
                            elapsed_secs: started.elapsed().as_secs(),
                        });
                        let readback = status
                            .success()
                            .then(|| report::github_verdict(pr, &me, started_epoch));
                        let _ = tx.send(Event::JobExited { idx, status, readback });
                    });
                }
                Err(e) => {
                    // A reviewer that cannot even spawn is a failed review,
                    // not a dead run: the other PRs still get theirs.
                    ui.note(format!("error: could not start the review for PR #{}: {e}", jobs[idx].pr));
                    jobs[idx].state = JobState::Failed;
                    jobs[idx].exit_code = Some(127);
                    // The failed marker too, like the exit path: the next
                    // babysit pass must review fresh, not re-check a stale
                    // session for a PR this run never reviewed.
                    rundir.mark_failed(jobs[idx].pr);
                    finished += 1;
                    ui.note_transition(&jobs[idx]);
                }
            }
        }

        // A spawn failure can finish the last job right here, with nothing
        // running and nothing to receive -- waiting on the channel then would
        // stall the end of the pass for the full receive timeout.
        if finished >= total {
            break;
        }

        let wait = if ui.tty {
            Duration::from_millis(200)
        } else {
            // Event-driven: sleep to the nearest deadline, or just wait for
            // an exit. The cap keeps a wrong deadline from wedging the loop.
            deadlines
                .iter()
                .flatten()
                .map(|d| d.at.saturating_duration_since(Instant::now()))
                .min()
                .unwrap_or(Duration::from_secs(60))
                .min(Duration::from_secs(60))
                .max(Duration::from_millis(10))
        };

        match rx.recv_timeout(wait) {
            Ok(Event::JobReaped { idx, elapsed_secs }) => {
                // The child is gone. Neither the guard nor the interrupt
                // path may killpg its dead (possibly recycled) pgid, the
                // clock stops at the real exit, and the slot frees for the
                // next queued review -- only the readback remains, and it
                // belongs to no process group.
                let job = &mut jobs[idx];
                deadlines[idx] = None;
                job.reaped = true;
                job.elapsed_secs = elapsed_secs;
                job.pgid = None;
                running -= 1;
                // The meta envelope is complete once the child is reaped, so
                // even a summary printed mid-readback -- an interrupt --
                // hands back this review's session id and cost.
                let meta = job::read_meta(rundir, job.pr);
                if let Some(m) = &meta
                    && m.total_cost_usd > 0.0
                    && !is_override
                {
                    job.cost = Some(m.total_cost_usd);
                }
                if let Some(m) = &meta
                    && !m.model_resolved.is_empty()
                    && !is_override
                {
                    job.model = Some(m.model_resolved.clone());
                }
                job.sid = job::summary_sid(job, meta.as_ref(), is_override);
            }
            Ok(Event::JobExited { idx, status, readback }) => {
                let job = &mut jobs[idx];
                let (state, code) = job::classify(status, job.guard_tripped, is_override);
                job.state = state;
                job.exit_code = code;
                // The slot and the deadline were released at JobReaped;
                // this event only finishes the bookkeeping.
                finished += 1;

                let ok = job.state == JobState::Done;
                if ok {
                    rundir.clear_failed(job.pr);
                } else {
                    rundir.mark_failed(job.pr);
                }
                // Cost, model and sid were read at JobReaped. Only a review
                // that finished is worth resuming, and only an envelope id
                // names the session it actually ran in.
                if ok && !is_override
                    && let Some(m) = job::read_meta(rundir, job.pr)
                    && session::is_uuid_shaped(&m.session_id)
                {
                    let _ = rundir.record_session(job.pr, &m.session_id);
                }
                // What the finished review concluded: the agent's trailer,
                // then GitHub's readback (carried in from the monitor
                // thread), which outranks it. Only a review that completed
                // has a conclusion to read, and only the built-in reviewer
                // promises the envelope the trailer lives in -- but the
                // GitHub readback works under any reviewer.
                if ok && let Some(gh) = readback {
                    // dash-p's answer is only the reviewer's last message,
                    // so the transcript is where a review that was followed
                    // by a sign-off still lives.
                    let transcript =
                        job.sid.as_deref().and_then(crate::session::transcript_path);
                    if !is_override {
                        job.trailer = report::read_trailer(
                            &rundir.stdout_path(job.pr),
                            transcript.as_deref(),
                            job.transcript_from,
                        );
                    }
                    // The review in a form a person can open. Best effort: a
                    // review that ran is not spoiled by a file that could not
                    // be written, and pr-N.json still holds the original.
                    if let Some(review) = report::read_review(
                        &rundir.stdout_path(job.pr),
                        transcript.as_deref(),
                        job.transcript_from,
                    ) {
                        let _ = std::fs::write(rundir.review_path(job.pr), review);
                    }
                    // A --no-post reviewer ran the skill with no posting
                    // step, so its own claim about what landed cannot be
                    // true. GitHub alone decides, which is also what makes
                    // the "nothing was posted" line under the summary safe
                    // to print. The rest of the trailer -- risk, findings,
                    // the panel -- is still worth reading.
                    let claimed = (!cfg.no_post).then_some(job.trailer.as_ref()).flatten();
                    job.verdict = report::resolve_verdict(&gh, claimed);
                    if let Some(claim) = report::vetoed_claim(&gh, job.trailer.as_ref()) {
                        ui.note(format!(
                            "note: PR #{}'s reviewer reported \"{claim}\" but GitHub shows no such review landed",
                            job.pr
                        ));
                    }
                    if gh == report::Readback::Failed {
                        // Say what actually fills the column: the agent's own
                        // report only when a trailer decision resolved one.
                        let tail = if job.verdict.is_some() {
                            "the verdict is the agent's own report"
                        } else {
                            "its verdict is unknown"
                        };
                        ui.note(format!(
                            "note: could not read PR #{}'s review back from GitHub; {tail}",
                            job.pr
                        ));
                    }
                }
                ui.note_transition(job);
            }
            Ok(Event::Signal) => interrupt(&jobs, ui, rundir),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Trip the guard on anything past its deadline. The job stays Running
        // until its monitor reports the exit the KILL guarantees, so there is
        // no state to race with.
        let now = Instant::now();
        for (idx, slot) in deadlines.iter_mut().enumerate() {
            if let Some(d) = slot
                && now >= d.at
            {
                if let Some(pgid) = jobs[idx].pgid {
                    stop_group(pgid);
                }
                jobs[idx].guard_tripped = true;
                *slot = None;
            }
        }

        ui.render(&jobs);
    }

    ui.render(&jobs);
    ui.end_pass();
    jobs
}

pub fn failures(jobs: &[Job]) -> usize {
    jobs.iter().filter(|j| j.state != JobState::Done).count()
}
