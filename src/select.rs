//! Which PRs a run works on: fetch, rank, then either sweep the actionable
//! ones or show the picker. Both front-ends select the same way -- they differ
//! only in what they do with the numbers afterwards, and in how each names the
//! flag that shows the rest, which is why the empty-sweep hint is passed in.

use crate::picker;
use crate::prlist;
use crate::repo::RepoContext;
use crate::session;
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
pub fn run(ctx: &RepoContext, opts: &Opts) -> Result<Option<Selection>> {
    let Some(prs) = prlist::fetch_prs(ctx, opts.include_approved, opts.include_dependabot)? else {
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
        picker::run(&rows, opts.continue_sessions, opts.include_dependabot)?
    } else {
        prlist::select_auto(&rows, opts.sweep_empty_hint)
    };
    Ok(numbers.map(|n| (n, titles)))
}
