//! Which PRs a run works on: fetch, rank, then either sweep the actionable
//! ones or show the picker. Both front-ends select the same way -- they differ
//! only in what they do with the numbers afterwards, and in how each names the
//! flag that shows the rest, which is why the empty-sweep hint is passed in.

use crate::picker;
use crate::prlist;
use crate::repo::RepoContext;
use crate::session;
use crate::status::{Status, step};
use anyhow::Result;
use std::collections::HashMap;

pub struct Opts<'a> {
    pub include_approved: bool,
    pub include_dependabot: bool,
    /// Show the picker instead of sweeping every NEW/UPDATED PR.
    pub pick: bool,
    pub continue_sessions: bool,
    /// Appended to "no NEW or UPDATED PRs to review" when a sweep comes up
    /// empty: each tool names its own way to see the rest.
    pub sweep_empty_hint: &'a str,
}

/// The chosen PR numbers, plus every fetched PR's title -- the live board wants
/// them, the tab fan-out ignores them.
pub type Selection = (Vec<u64>, HashMap<u64, String>);

fn mark_resumable(rows: &mut [prlist::Row], ctx: &RepoContext) {
    // Marking costs one hash and one glob per PR, so skip the whole loop when
    // no session store exists -- there is nothing to find, and a box without
    // Claude Code should not pay for the lookup on every picker run.
    if !session::projects_dir().is_dir() {
        return;
    }
    for row in rows {
        let id = session::pr_session_id(&ctx.repo_root, &ctx.owner, &ctx.name, row.number);
        row.resumable = session::session_exists(&id);
    }
}

/// None (after printing why) means there is nothing to do and the caller exits
/// 0: an empty repo, a sweep with nothing actionable, or an empty pick.
pub fn run(ctx: &RepoContext, opts: &Opts, status: &Status) -> Result<Option<Selection>> {
    status.step(step::fetching(&ctx.owner, &ctx.name));
    let found = prlist::fetch(ctx, opts.include_approved, opts.include_dependabot, status)?;
    // Permanent, not a step: it is the line that keeps "3 PRs to review" from
    // reading as a broken query on a repo showing forty in the browser, and a
    // step is erased the moment the next one replaces it.
    status.say(step::found(found.open, found.prs.len()));
    // Cleared before anything writes to stdout: the spinner owns the last
    // line of the terminal until it does not.
    let empty = found.prs.is_empty();
    if empty {
        status.clear();
    }
    let Some(prs) = prlist::explain_if_empty(found.prs, opts.include_approved, opts.include_dependabot)
    else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut rows = prlist::build_rows(&prs, &ctx.me, now);
    let titles = rows.iter().map(|r| (r.number, r.title.clone())).collect();
    let numbers = if opts.pick {
        mark_resumable(&mut rows, ctx);
        // Cleared before the picker: gum owns the terminal from here, and a
        // spinner ticking underneath it would fight for the same lines.
        status.clear();
        picker::run(&rows, opts.continue_sessions, opts.include_dependabot)?
    } else {
        status.clear();
        prlist::select_auto(&rows, opts.sweep_empty_hint)
    };
    Ok(numbers.map(|n| (n, titles)))
}
