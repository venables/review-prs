//! Fetching and ranking the PRs worth reviewing. Both front-ends read this
//! one list, so they cannot disagree about what is actionable.

use crate::ci::Ci;
use crate::repo::RepoContext;
use crate::status::Status;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

/// Author logins treated as bots: hidden unless --dependabot, dimmed when
/// shown. An anchored prefix match: extend it as more coding bots appear.
const BOT_LOGIN_PREFIX: &str = "dependabot";

pub fn is_bot(login: &str) -> bool {
    login.starts_with(BOT_LOGIN_PREFIX)
}

/// How many PRs one call asks for. A repo with more than this returns a full
/// page and the true total is unknown, which is why a count that reaches it is
/// reported as "50+" rather than as fact.
pub const QUERY_LIMIT: usize = 50;

// One GraphQL call for the open PRs, enough activity to rank engagement, and
// the checks on the head commit. That last is its own aliased field rather
// than a line in the commits list: the rollup is wanted for one commit, and
// asking for it on a hundred would make the query pay for ninety-nine
// answers it throws away.
const QUERY: &str = "
      query($owner:String!, $name:String!) {
        repository(owner:$owner, name:$name) {
          pullRequests(states:OPEN, first:50, orderBy:{field:UPDATED_AT, direction:DESC}) {
            nodes {
              number
              title
              isDraft
              updatedAt
              reviewDecision
              headRefOid
              author { login }
              comments(last:100) { nodes { author { login } updatedAt } }
              reviews(last:100)  { nodes { author { login } submittedAt } }
              commits(last:100)  { nodes { commit { committedDate author { user { login } } } } }
              headCommit: commits(last:1) { nodes { commit { statusCheckRollup { state } } } }
            }
          }
        }
      }";

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Actor {
    pub login: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommentNode {
    pub author: Option<Actor>,
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct ReviewNode {
    pub author: Option<Actor>,
    #[serde(rename = "submittedAt")]
    pub submitted_at: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommitAuthor {
    pub user: Option<Actor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Commit {
    #[serde(rename = "committedDate")]
    pub committed_date: Option<String>,
    pub author: Option<CommitAuthor>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct CommitNode {
    pub commit: Commit,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Rollup {
    pub state: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct HeadCommit {
    /// Null when the commit has no checks and no statuses at all.
    #[serde(rename = "statusCheckRollup")]
    pub status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct HeadCommitNode {
    pub commit: HeadCommit,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct Nodes<T> {
    #[serde(default = "Vec::new")]
    pub nodes: Vec<T>,
}

fn empty_nodes<T>() -> Nodes<T> {
    Nodes { nodes: Vec::new() }
}

#[derive(Deserialize, Debug, Clone)]
pub struct PrNode {
    pub number: u64,
    pub title: String,
    #[serde(rename = "isDraft")]
    pub is_draft: bool,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "reviewDecision")]
    pub review_decision: Option<String>,
    /// The SHA at the tip of the PR branch. This is what tells a push from a
    /// comment: both make a PR actionable again, and only one is new code.
    #[serde(rename = "headRefOid")]
    pub head_ref_oid: Option<String>,
    pub author: Option<Actor>,
    #[serde(default = "empty_nodes")]
    pub comments: Nodes<CommentNode>,
    #[serde(default = "empty_nodes")]
    pub reviews: Nodes<ReviewNode>,
    #[serde(default = "empty_nodes")]
    pub commits: Nodes<CommitNode>,
    /// The tip of the branch alone, for its checks. See `ci`.
    #[serde(rename = "headCommit", default = "empty_nodes")]
    pub head_commit: Nodes<HeadCommitNode>,
}

impl PrNode {
    pub fn author_login(&self) -> &str {
        self.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")
    }

    /// The checks on the head commit. A missing node, a null rollup and an
    /// unknown state all read as "no checks": none of them is a reason to
    /// hold a PR.
    pub fn ci(&self) -> Ci {
        let state = self
            .head_commit
            .nodes
            .first()
            .and_then(|n| n.commit.status_check_rollup.as_ref())
            .and_then(|r| r.state.as_deref());
        Ci::from_state(state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engagement {
    New,
    Updated,
    Seen,
}

impl Engagement {
    /// NEW or UPDATED: something happened that the sweep has not answered.
    pub fn engaged(self) -> bool {
        matches!(self, Engagement::New | Engagement::Updated)
    }

    pub fn label(self) -> &'static str {
        match self {
            Engagement::New => "NEW",
            Engagement::Updated => "UPDATED",
            Engagement::Seen => "SEEN",
        }
    }
    fn rank(self) -> u8 {
        match self {
            Engagement::New => 0,
            Engagement::Updated => 1,
            Engagement::Seen => 2,
        }
    }
}

/// What the board needs to say about a PR that is not in the Job itself:
/// who opened it, and why it is in the queue. Carried alongside the numbers
/// from selection through to the pass, because the GraphQL answer is the only
/// place any of it exists and re-fetching to render a line would be absurd.
#[derive(Debug, Clone)]
pub struct PrInfo {
    pub title: String,
    pub author: String,
    pub engage: Engagement,
    /// See `Row::head`.
    pub head: Option<String>,
    /// See `Row::ci`.
    pub ci: Ci,
}

impl Row {
    pub fn info(&self) -> PrInfo {
        PrInfo {
            title: self.title.clone(),
            author: self.author.clone(),
            engage: self.engage,
            head: self.head.clone(),
            ci: self.ci,
        }
    }
}

/// One display/selection row, already ranked.
#[derive(Debug, Clone)]
pub struct Row {
    pub bot: bool,
    pub number: u64,
    pub engage: Engagement,
    pub review: &'static str,
    pub rel_time: String,
    pub author: String,
    pub title: String,
    pub updated_at: String,
    pub resumable: bool,
    /// The SHA at the tip of the PR branch: the fingerprint a watch run
    /// compares to tell a push from a comment. None when the query did not
    /// return one, which reads as "no push we can prove".
    ///
    /// A SHA rather than the newest commit date, because a date is not
    /// unique: a force-push that keeps committer dates, or two pushes inside
    /// the same second, would read as no push at all and leave the PR capped.
    pub head: Option<String>,
    /// The checks on that commit, which decide whether the sweep reviews
    /// the PR now or holds it.
    pub ci: Ci,
}

impl Row {
    pub fn engaged(&self) -> bool {
        self.engage.engaged()
    }

    /// What the sweep reviews now: engaged, and, when the checks gate it,
    /// with its checks passing.
    pub fn ready(&self, gate_ci: bool) -> bool {
        self.engaged() && (!gate_ci || self.ci.ready())
    }

    /// Engaged, but the checks say not yet: what the sweep names as held.
    pub fn held(&self, gate_ci: bool) -> bool {
        self.engaged() && !self.ready(gate_ci)
    }
}

impl PrInfo {
    /// See `Row::held`.
    pub fn held(&self, gate_ci: bool) -> bool {
        self.engage.engaged() && gate_ci && !self.ci.ready()
    }
}

/// What one fetch found: the PRs this run may act on, and how many were open
/// before the filters ran. The two differ on any repo where most of the open
/// PRs are your own -- and "found 3 open PRs" on a repo showing 40 in the
/// browser reads as a broken query rather than a working filter.
pub struct Fetched {
    pub prs: Vec<PrNode>,
    /// Open and non-draft, before your own PRs, bots and approved ones were
    /// removed.
    pub open: usize,
    /// The query came back full, so there are more PRs than this saw. Taken
    /// from the raw node count, before drafts are dropped: a full page with
    /// two drafts in it would otherwise report 48 as a fact.
    pub truncated: bool,
}

/// Fetch and filter, saying nothing. What a refresh wants: a babysit loop
/// that explained "no matching open PRs" on every interval would be noise.
pub fn fetch(
    ctx: &RepoContext,
    include_approved: bool,
    include_dependabot: bool,
    status: &Status,
) -> Result<Fetched> {
    let out = Command::new("gh")
        .args(["api", "graphql", "-F"])
        .arg(format!("owner={}", ctx.owner))
        .arg("-F")
        .arg(format!("name={}", ctx.name))
        .arg("-f")
        .arg(format!("query={QUERY}"))
        .output()
        .context("running gh api graphql")?;
    if !out.status.success() {
        status.say(String::from_utf8_lossy(&out.stderr).trim_end().to_string());
        bail!(crate::repo::AlreadyReported);
    }
    let parsed: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing gh api graphql output")?;
    let prs = extract_nodes(&parsed, status)?;

    Ok(filter_prs(prs, &ctx.me, include_approved, include_dependabot))
}

/// Who is left after the filters, and how many were open before them. Split
/// out and pure so the tests exercise the real chain: a test that re-lists the
/// same four filters would agree with itself while the code drifted, which is
/// the whole failure this crate spent a release removing.
pub fn filter_prs(
    prs: Vec<PrNode>,
    me: &str,
    include_approved: bool,
    include_dependabot: bool,
) -> Fetched {
    let truncated = prs.len() >= QUERY_LIMIT;
    let open: Vec<PrNode> = prs.into_iter().filter(|pr| !pr.is_draft).collect();
    let total = open.len();
    let prs: Vec<PrNode> = open
        .into_iter()
        // Always hide your own PRs -- this tool is for reviewing others' work.
        .filter(|pr| pr.author_login() != me)
        .filter(|pr| include_dependabot || !is_bot(pr.author_login()))
        .filter(|pr| include_approved || pr.review_decision.as_deref() != Some("APPROVED"))
        .collect();
    Fetched { prs, open: total, truncated }
}

/// Nothing left after the filters is not an error, but it does need a reason:
/// the two flags that would have widened the search are the answer most of
/// the time. Returns None (after printing) when there is nothing to review --
/// the caller exits 0.
pub fn explain_if_empty(
    prs: Vec<PrNode>,
    include_approved: bool,
    include_dependabot: bool,
) -> Option<Vec<PrNode>> {
    if prs.is_empty() {
        let mut hint = String::new();
        if !include_approved {
            hint.push_str(" --all (approved)");
        }
        if !include_dependabot {
            hint.push_str(" --dependabot (bots)");
        }
        if !hint.is_empty() {
            println!("no matching open PRs; try:{hint}");
        } else {
            println!("no open non-draft PRs");
        }
        return None;
    }
    Some(prs)
}

/// The PR nodes out of a GraphQL response -- or a refusal. A 200 carrying an
/// `errors` array, or a response missing the data path entirely, must not
/// read as "no PRs": an unattended sweep would then exit 0 having reviewed
/// nothing, which is the lie the exit status exists to prevent.
fn extract_nodes(parsed: &serde_json::Value, status: &Status) -> Result<Vec<PrNode>> {
    if let Some(errors) = parsed.get("errors").and_then(|e| e.as_array())
        && !errors.is_empty()
    {
        let mut said = String::from("error: gh api graphql returned errors:");
        for err in errors {
            match err.get("message").and_then(|m| m.as_str()) {
                Some(msg) => said.push_str(&format!("\n  {msg}")),
                None => said.push_str(&format!("\n  {err}")),
            }
        }
        status.say(said);
        bail!(crate::repo::AlreadyReported);
    }
    let Some(nodes) = parsed.pointer("/data/repository/pullRequests/nodes") else {
        status.say("error: gh api graphql returned no pull request data");
        bail!(crate::repo::AlreadyReported);
    };
    serde_json::from_value(nodes.clone()).context("parsing the pull request list")
}

/// Seconds since the epoch for a strict "YYYY-MM-DDTHH:MM:SSZ" timestamp --
/// the only shape GitHub emits here. Days-from-civil, no external crate.
pub fn parse_iso(ts: &str) -> Option<i64> {
    let b = ts.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let (y, m, d) = (num(&ts[0..4])?, num(&ts[5..7])?, num(&ts[8..10])?);
    let (hh, mm, ss) = (num(&ts[11..13])?, num(&ts[14..16])?, num(&ts[17..19])?);
    // Howard Hinnant's days_from_civil.
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// "5h ago" shapes, floored to the unit.
pub fn rel(now_epoch: i64, ts: &str) -> String {
    let Some(t) = parse_iso(ts) else {
        return "-".into();
    };
    let s = now_epoch - t;
    if s < 60 {
        format!("{s}s ago")
    } else if s < 3600 {
        format!("{}m ago", s / 60)
    } else if s < 86_400 {
        format!("{}h ago", s / 3600)
    } else {
        format!("{}d ago", s / 86_400)
    }
}

/// NEW/UPDATED/SEEN from the PR's activity, exactly as the jq did: the latest
/// event by me vs the latest by anyone else, ISO strings compared
/// lexicographically (which is chronological for this fixed shape).
pub fn engagement(pr: &PrNode, me: &str) -> Engagement {
    let mut events: Vec<(&str, &str)> = Vec::new();
    for c in &pr.comments.nodes {
        if let Some(at) = c.updated_at.as_deref() {
            events.push((at, c.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")));
        }
    }
    for r in &pr.reviews.nodes {
        if let Some(at) = r.submitted_at.as_deref() {
            events.push((at, r.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("")));
        }
    }
    for c in &pr.commits.nodes {
        if let Some(at) = c.commit.committed_date.as_deref() {
            let who = c
                .commit
                .author
                .as_ref()
                .and_then(|a| a.user.as_ref())
                .and_then(|u| u.login.as_deref())
                .unwrap_or("");
            events.push((at, who));
        }
    }
    let mine = events.iter().filter(|(_, w)| *w == me).map(|(at, _)| *at).max();
    let other = events.iter().filter(|(_, w)| *w != me).map(|(at, _)| *at).max();
    match (mine, other) {
        (None, _) => Engagement::New,
        (Some(m), Some(o)) if o > m => Engagement::Updated,
        _ => Engagement::Seen,
    }
}

pub fn build_rows(prs: &[PrNode], me: &str, now_epoch: i64) -> Vec<Row> {
    let mut rows: Vec<Row> = prs
        .iter()
        .map(|pr| {
            let engage = engagement(pr, me);
            let review = match pr.review_decision.as_deref() {
                Some("CHANGES_REQUESTED") => "CHANGES",
                Some("APPROVED") => "APPROVED",
                _ => "-",
            };
            let author = if pr.author_login().is_empty() { "ghost" } else { pr.author_login() };
            Row {
                bot: is_bot(pr.author_login()),
                number: pr.number,
                engage,
                review,
                rel_time: rel(now_epoch, &pr.updated_at),
                author: author.to_string(),
                title: pr.title.clone(),
                updated_at: pr.updated_at.clone(),
                resumable: false,
                head: pr.head_ref_oid.clone(),
                ci: pr.ci(),
            }
        })
        .collect();
    // Most actionable first: rank ascending, then recency descending (string
    // compare on the ISO timestamps, as `sort -k2,2r` did).
    rows.sort_by(|a, b| {
        a.engage
            .rank()
            .cmp(&b.engage.rank())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    rows
}

/// The sweep: every NEW/UPDATED PR whose checks are not in the way. SEEN PRs
/// are skipped on purpose -- nothing has changed since you last engaged, so
/// an unattended sweep has no reason to re-review them. A PR whose checks
/// are pending or failing is held, and named, unless `gate_ci` is off.
/// Prints the selection; None (after printing) means nothing to do now. The
/// hint names the caller's own way to see the rest, which is a different
/// flag in each front-end.
pub fn select_auto(rows: &[Row], empty_hint: &str, gate_ci: bool) -> Option<Vec<u64>> {
    let held: Vec<(u64, Ci)> =
        rows.iter().filter(|r| r.held(gate_ci)).map(|r| (r.number, r.ci)).collect();
    if !held.is_empty() {
        println!("{}", crate::ci::held_line(&held));
    }
    let ready: Vec<&Row> = rows.iter().filter(|r| r.ready(gate_ci)).collect();
    if ready.is_empty() {
        println!("no NEW or UPDATED PRs to review{empty_hint}");
        return None;
    }
    // Each number carries the reason it is here. "4 PRs to review" answers
    // how many; the reader's next question is always which of them are new.
    let list: Vec<String> = ready
        .iter()
        .map(|r| format!("#{} ({})", r.number, r.engage.label().to_lowercase()))
        .collect();
    println!("{} to review: {}", crate::ui::count(ready.len(), "PR"), list.join(" "));
    Some(ready.iter().map(|r| r.number).collect())
}

/// Why a babysit loop should stop watching a PR, or None while it should keep
/// waiting. Approval is the expected end, but a PR that was closed, or merged
/// without ever collecting an approving review, is just as finished. A failed
/// lookup reads as "keep waiting" -- a transient API error must not end the
/// loop early.
pub fn pr_babysit_done(n: u64) -> Option<String> {
    let out = Command::new("gh")
        .args(["pr", "view", &n.to_string(), "--json", "state,reviewDecision"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let decision = v["reviewDecision"].as_str().unwrap_or("");
    let state = v["state"].as_str().unwrap_or("");
    if decision == "APPROVED" {
        Some("approved".into())
    } else if !state.is_empty() && state != "OPEN" {
        Some(state.to_lowercase())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The same shape tests/helpers.sh seeds (its numbering differs slightly:
    // there 5 is the approved PR and 2 the draft): 9 and 8 are NEW by others,
    // 6 is SEEN (me commented last), 5 is a draft, 4 is mine, 3 is
    // dependabot, 2 is approved.
    fn fixture() -> Vec<PrNode> {
        let json = r#"[
          {"number":9,"title":"Add retry logic","isDraft":false,
           "updatedAt":"2026-08-10T10:00:00Z","reviewDecision":null,
           "author":{"login":"alice"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-10T10:00:00Z","author":{"user":{"login":"alice"}}}}]}},
          {"number":8,"title":"Fix flaky test","isDraft":false,
           "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":"CHANGES_REQUESTED",
           "author":{"login":"bob"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T10:00:00Z","author":{"user":{"login":"bob"}}}}]}},
          {"number":6,"title":"Refactor helpers","isDraft":false,
           "updatedAt":"2026-08-07T10:00:00Z","reviewDecision":null,
           "author":{"login":"carol"},
           "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
           "reviews":{"nodes":[]},
           "commits":{"nodes":[{"commit":{"committedDate":"2026-08-07T10:00:00Z","author":{"user":{"login":"carol"}}}}]}},
          {"number":5,"title":"WIP draft","isDraft":true,
           "updatedAt":"2026-08-06T10:00:00Z","reviewDecision":null,
           "author":{"login":"dave"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":4,"title":"My own PR","isDraft":false,
           "updatedAt":"2026-08-05T10:00:00Z","reviewDecision":null,
           "author":{"login":"me"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":3,"title":"Bump lodash","isDraft":false,
           "updatedAt":"2026-08-04T10:00:00Z","reviewDecision":null,
           "author":{"login":"dependabot[bot]"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":2,"title":"Approved already","isDraft":false,
           "updatedAt":"2026-08-03T10:00:00Z","reviewDecision":"APPROVED",
           "author":{"login":"erin"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}}
        ]"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn the_head_sha_reaches_the_queue_that_compares_it() {
        // The push detection is only as good as this wiring: the query asks
        // for headRefOid, Row carries it, and PrInfo hands it to the queue.
        // Every link was previously untested, so the whole reset could have
        // been inert against a real GitHub answer and every test still pass.
        assert!(QUERY.contains("headRefOid"), "the query must ask for it");

        let json = r#"[
          {"number":9,"title":"Add retry logic","isDraft":false,
           "updatedAt":"2026-08-10T10:00:00Z","reviewDecision":null,
           "headRefOid":"abc123","author":{"login":"alice"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}},
          {"number":8,"title":"No head","isDraft":false,
           "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":null,
           "author":{"login":"bob"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}}
        ]"#;
        let prs: Vec<PrNode> = serde_json::from_str(json).unwrap();
        assert_eq!(prs[0].head_ref_oid.as_deref(), Some("abc123"));
        assert_eq!(prs[1].head_ref_oid, None, "a missing head is None, not an error");

        let rows = build_rows(&prs, "me", 0);
        let nine = rows.iter().find(|r| r.number == 9).unwrap();
        let eight = rows.iter().find(|r| r.number == 8).unwrap();
        assert_eq!(nine.head.as_deref(), Some("abc123"));
        assert_eq!(nine.info().head.as_deref(), Some("abc123"));
        assert_eq!(eight.info().head, None);
    }

    /// The real chain, not a copy of it: `fetch` differs from this only by
    /// the network call in front of it.
    fn filtered(include_approved: bool, include_dependabot: bool) -> Vec<PrNode> {
        filter_prs(fixture(), "me", include_approved, include_dependabot).prs
    }

    #[test]
    fn hidden_prs_stay_hidden() {
        let nums: Vec<u64> = filtered(false, false).iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![9, 8, 6]);
        let nums: Vec<u64> = filtered(true, true).iter().map(|p| p.number).collect();
        assert_eq!(nums, vec![9, 8, 6, 3, 2]);
    }

    #[test]
    fn a_full_page_is_known_to_be_incomplete_before_drafts_are_dropped() {
        // The count of nodes the query returned, not the count that survived
        // the filters: a full page holding two drafts reports 48 open, and
        // without this flag it would report that as an exact total.
        let mut page = Vec::new();
        for n in 0..QUERY_LIMIT {
            let mut pr = fixture().remove(0);
            pr.number = n as u64 + 100;
            pr.is_draft = n < 2;
            page.push(pr);
        }
        let found = filter_prs(page, "me", false, false);
        assert!(found.truncated, "a full page means there are more");
        assert_eq!(found.open, QUERY_LIMIT - 2, "the drafts are still not open");
    }

    #[test]
    fn the_query_asks_for_exactly_the_limit_it_reports() {
        // Two values that would otherwise drift, and the "50+" label would be
        // wrong the moment they did.
        assert!(
            QUERY.contains(&format!("first:{QUERY_LIMIT}")),
            "the query and QUERY_LIMIT disagree"
        );
    }

    #[test]
    fn an_empty_result_names_the_flags_that_would_widen_it() {
        // Nothing left is not an error, but it needs a reason: the flags that
        // would have found something are the answer most of the time.
        assert!(explain_if_empty(Vec::new(), false, false).is_none());
        assert!(explain_if_empty(Vec::new(), true, true).is_none());
        // Something left is passed straight through.
        let prs = filtered(false, false);
        assert_eq!(explain_if_empty(prs, false, false).unwrap().len(), 3);
    }

    #[test]
    fn the_fetch_reports_what_its_filters_removed() {
        // The number a user sees in the browser, and the number this tool
        // will act on. They differ on any repo where most PRs are your own,
        // and both come out of the one function that does the filtering.
        let found = filter_prs(fixture(), "me", false, false);
        assert_eq!(found.open, 6, "six open, one draft");
        assert_eq!(found.prs.len(), 3, "yours, the bot and the approved one go");
        assert!(!found.truncated, "seven nodes is not a full page");

        // Widening the flags moves the second number and never the first.
        let wide = filter_prs(fixture(), "me", true, true);
        assert_eq!(wide.open, 6, "the draft is still not open");
        assert_eq!(wide.prs.len(), 5);
    }

    #[test]
    fn engagement_and_ranking() {
        let prs = filtered(false, false);
        let rows = build_rows(&prs, "me", parse_iso("2026-08-10T12:00:00Z").unwrap());
        let shaped: Vec<(u64, &str)> = rows.iter().map(|r| (r.number, r.engage.label())).collect();
        assert_eq!(shaped, vec![(9, "NEW"), (8, "NEW"), (6, "SEEN")]);
    }

    #[test]
    fn select_auto_takes_new_and_updated_only() {
        let prs = filtered(false, false);
        let rows = build_rows(&prs, "me", parse_iso("2026-08-10T12:00:00Z").unwrap());
        assert_eq!(select_auto(&rows, "", true), Some(vec![9, 8]));
    }

    /// The fixture with the head commit's checks set on the PRs named.
    fn with_ci(states: &[(u64, &str)]) -> Vec<PrNode> {
        let mut prs = filtered(false, false);
        for pr in &mut prs {
            if let Some((_, state)) = states.iter().find(|(n, _)| *n == pr.number) {
                pr.head_commit = Nodes {
                    nodes: vec![HeadCommitNode {
                        commit: HeadCommit {
                            status_check_rollup: Some(Rollup { state: Some((*state).to_string()) }),
                        },
                    }],
                };
            }
        }
        prs
    }

    #[test]
    fn the_checks_reach_the_rows_from_the_query() {
        // The whole hold is only as good as this wiring: the query asks for
        // the rollup, PrNode reads it, and Row carries it to the sweep.
        assert!(QUERY.contains("statusCheckRollup"), "the query must ask for it");
        assert!(QUERY.contains("headCommit: commits(last:1)"), "...on the head commit alone");

        let json = r#"[
          {"number":9,"title":"t","isDraft":false,"updatedAt":"2026-08-10T10:00:00Z",
           "reviewDecision":null,"author":{"login":"alice"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]},
           "headCommit":{"nodes":[{"commit":{"statusCheckRollup":{"state":"PENDING"}}}]}},
          {"number":8,"title":"t","isDraft":false,"updatedAt":"2026-08-09T10:00:00Z",
           "reviewDecision":null,"author":{"login":"bob"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]},
           "headCommit":{"nodes":[{"commit":{"statusCheckRollup":null}}]}},
          {"number":7,"title":"t","isDraft":false,"updatedAt":"2026-08-08T10:00:00Z",
           "reviewDecision":null,"author":{"login":"carol"},
           "comments":{"nodes":[]},"reviews":{"nodes":[]},"commits":{"nodes":[]}}
        ]"#;
        let prs: Vec<PrNode> = serde_json::from_str(json).unwrap();
        assert_eq!(prs[0].ci(), Ci::Pending);
        assert_eq!(prs[1].ci(), Ci::None, "a null rollup is no checks");
        assert_eq!(prs[2].ci(), Ci::None, "a missing field is no checks");
        let rows = build_rows(&prs, "me", 0);
        assert_eq!(rows[0].ci, Ci::Pending);
    }

    #[test]
    fn the_sweep_holds_a_pr_whose_checks_are_not_green() {
        let prs = with_ci(&[(9, "PENDING"), (8, "SUCCESS")]);
        let rows = build_rows(&prs, "me", parse_iso("2026-08-10T12:00:00Z").unwrap());
        assert_eq!(select_auto(&rows, "", true), Some(vec![8]), "9 is held");
        assert_eq!(select_auto(&rows, "", false), Some(vec![9, 8]), "--skip-wait-for-ci");

        // Failing is held the same way: the author is about to push.
        let prs = with_ci(&[(9, "FAILURE"), (8, "ERROR")]);
        let rows = build_rows(&prs, "me", 0);
        assert_eq!(select_auto(&rows, "", true), None, "nothing left to review");
        assert_eq!(select_auto(&rows, "", false), Some(vec![9, 8]));

        // A PR with no checks at all is never held.
        let rows = build_rows(&filtered(false, false), "me", 0);
        assert_eq!(select_auto(&rows, "", true), Some(vec![9, 8]));
    }

    #[test]
    fn readiness_is_engagement_and_then_checks() {
        let prs = with_ci(&[(9, "PENDING"), (6, "PENDING")]);
        let rows = build_rows(&prs, "me", 0);
        let nine = rows.iter().find(|r| r.number == 9).unwrap();
        let six = rows.iter().find(|r| r.number == 6).unwrap();
        assert!(nine.engaged() && !nine.ready(true) && nine.ready(false));
        assert!(nine.held(true) && !nine.held(false));
        // SEEN is not ready whatever its checks say, and not held either:
        // the hold is for PRs the sweep would otherwise review.
        assert!(!six.engaged() && !six.ready(true) && !six.ready(false));
        assert!(!six.held(true));
        // PrInfo answers the same question for the loop.
        assert!(nine.info().held(true) && !six.info().held(true));
    }

    #[test]
    fn updated_beats_seen() {
        // me commented, then carol pushed: UPDATED.
        let json = r#"{"number":7,"title":"t","isDraft":false,
          "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":null,
          "author":{"login":"carol"},
          "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
          "reviews":{"nodes":[]},
          "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T09:00:00Z","author":{"user":{"login":"carol"}}}}]}}"#;
        let pr: PrNode = serde_json::from_str(json).unwrap();
        assert_eq!(engagement(&pr, "me"), Engagement::Updated);
    }

    #[test]
    fn rel_time_shapes() {
        let now = parse_iso("2026-08-10T12:00:00Z").unwrap();
        assert_eq!(rel(now, "2026-08-10T11:59:30Z"), "30s ago");
        assert_eq!(rel(now, "2026-08-10T11:30:00Z"), "30m ago");
        assert_eq!(rel(now, "2026-08-10T07:00:00Z"), "5h ago");
        assert_eq!(rel(now, "2026-08-07T12:00:00Z"), "3d ago");
    }

    #[test]
    fn graphql_errors_do_not_read_as_an_empty_repo() {
        // A 200 carrying errors, and a response missing the data path: both
        // must refuse, or an unattended sweep exits 0 having reviewed nothing.
        let with_errors: serde_json::Value =
            serde_json::from_str(r#"{"errors":[{"message":"rate limited"}],"data":null}"#).unwrap();
        let quiet = Status::silent();
        assert!(extract_nodes(&with_errors, &quiet).is_err());

        let missing_path: serde_json::Value = serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert!(extract_nodes(&missing_path, &quiet).is_err());

        let ok: serde_json::Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequests":{"nodes":[]}}}}"#,
        )
        .unwrap();
        assert_eq!(extract_nodes(&ok, &quiet).unwrap().len(), 0);
    }

    #[test]
    fn iso_parsing_is_correct_around_epoch_math() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        // Cross-checked against `date -u -j -f ... +%s` on this machine.
        assert_eq!(parse_iso("2026-08-10T10:00:00Z"), Some(1_786_356_000));
        assert_eq!(parse_iso("not a date"), None);
    }
}
